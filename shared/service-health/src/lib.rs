use std::future::Future;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::Notify;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReadyState {
    Ready,
    NotReady,
}

struct Inner {
    draining: AtomicBool,
    consecutive_successes: AtomicUsize,
    successes_required: usize,
    active: AtomicUsize,
    drained: Notify,
}

#[derive(Clone)]
pub struct Readiness {
    inner: Arc<Inner>,
}

impl Readiness {
    pub fn new(successes_required: usize) -> Self {
        Self {
            inner: Arc::new(Inner {
                draining: AtomicBool::new(false),
                consecutive_successes: AtomicUsize::new(0),
                successes_required: successes_required.max(1),
                active: AtomicUsize::new(0),
                drained: Notify::new(),
            }),
        }
    }

    pub fn is_live(&self) -> bool {
        true
    }

    pub fn is_ready(&self) -> bool {
        !self.inner.draining.load(Ordering::Acquire)
            && self.inner.consecutive_successes.load(Ordering::Acquire)
                >= self.inner.successes_required
    }

    pub fn observe_dependency(&self, success: bool) -> ReadyState {
        if !success || self.inner.draining.load(Ordering::Acquire) {
            self.inner.consecutive_successes.store(0, Ordering::Release);
            metrics::gauge!("service_ready").set(0.0);
            return ReadyState::NotReady;
        }
        let mut current = self.inner.consecutive_successes.load(Ordering::Acquire);
        loop {
            let next = current.saturating_add(1).min(self.inner.successes_required);
            match self.inner.consecutive_successes.compare_exchange_weak(
                current,
                next,
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                Ok(_) => break,
                Err(actual) => current = actual,
            }
        }
        if self.is_ready() {
            metrics::gauge!("service_ready").set(1.0);
            ReadyState::Ready
        } else {
            metrics::gauge!("service_ready").set(0.0);
            ReadyState::NotReady
        }
    }

    pub async fn probe<F>(&self, deadline: Duration, check: F) -> ReadyState
    where
        F: Future<Output = bool>,
    {
        let success = tokio::time::timeout(deadline, check).await.unwrap_or(false);
        self.observe_dependency(success)
    }

    pub fn begin_draining(&self) {
        self.inner.draining.store(true, Ordering::Release);
        self.inner.consecutive_successes.store(0, Ordering::Release);
        metrics::gauge!("service_ready").set(0.0);
    }

    pub fn is_draining(&self) -> bool {
        self.inner.draining.load(Ordering::Acquire)
    }

    pub fn active(&self) -> usize {
        self.inner.active.load(Ordering::Acquire)
    }

    pub fn try_admit(&self) -> Option<ActiveRequest> {
        if self.is_draining() {
            return None;
        }
        self.inner.active.fetch_add(1, Ordering::AcqRel);
        if self.is_draining() {
            if self.inner.active.fetch_sub(1, Ordering::AcqRel) == 1 {
                self.inner.drained.notify_waiters();
            }
            return None;
        }
        Some(ActiveRequest {
            inner: Arc::clone(&self.inner),
        })
    }

    pub async fn wait_for_drain(&self, deadline: Duration) -> DrainReport {
        let wait = async {
            loop {
                let notified = self.inner.drained.notified();
                if self.active() == 0 {
                    break;
                }
                notified.await;
            }
        };
        let timed_out = tokio::time::timeout(deadline, wait).await.is_err();
        DrainReport {
            timed_out,
            remaining: self.active(),
        }
    }
}

pub struct ActiveRequest {
    inner: Arc<Inner>,
}

impl Drop for ActiveRequest {
    fn drop(&mut self) {
        if self.inner.active.fetch_sub(1, Ordering::AcqRel) == 1 {
            self.inner.drained.notify_waiters();
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DrainReport {
    pub timed_out: bool,
    pub remaining: usize,
}

pub async fn shutdown_signal() {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{signal, SignalKind};
        let mut terminate = signal(SignalKind::terminate()).expect("install SIGTERM handler");
        tokio::select! {
            _ = tokio::signal::ctrl_c() => {},
            _ = terminate.recv() => {},
        }
    }
    #[cfg(not(unix))]
    {
        let _ = tokio::signal::ctrl_c().await;
    }
}

#[cfg(test)]
mod tests {
    use super::{Readiness, ReadyState};
    use std::time::{Duration, Instant};

    #[tokio::test]
    async fn live_is_independent_from_failed_dependency() {
        let readiness = Readiness::new(2);
        assert!(readiness.is_live());
        assert_eq!(readiness.observe_dependency(false), ReadyState::NotReady);
        assert!(readiness.is_live());
        assert!(!readiness.is_ready());
    }

    #[tokio::test]
    async fn readiness_probe_times_out_and_requires_consecutive_successes() {
        let readiness = Readiness::new(2);
        let started = Instant::now();
        let state = readiness
            .probe(Duration::from_millis(20), async {
                tokio::time::sleep(Duration::from_secs(1)).await;
                true
            })
            .await;
        assert_eq!(state, ReadyState::NotReady);
        assert!(started.elapsed() < Duration::from_millis(200));

        assert_eq!(readiness.observe_dependency(true), ReadyState::NotReady);
        assert_eq!(readiness.observe_dependency(true), ReadyState::Ready);
        assert!(readiness.is_ready());
        assert_eq!(readiness.observe_dependency(false), ReadyState::NotReady);
    }

    #[tokio::test]
    async fn shutdown_rejects_admission_and_drain_is_bounded() {
        let readiness = Readiness::new(1);
        readiness.observe_dependency(true);
        let permit = readiness.try_admit().expect("admitted before shutdown");
        readiness.begin_draining();
        assert!(!readiness.is_ready());
        assert!(readiness.try_admit().is_none());
        let report = readiness.wait_for_drain(Duration::from_millis(10)).await;
        assert!(report.timed_out);
        assert_eq!(report.remaining, 1);
        drop(permit);
        let report = readiness.wait_for_drain(Duration::from_millis(50)).await;
        assert!(!report.timed_out);
        assert_eq!(report.remaining, 0);
    }
}
