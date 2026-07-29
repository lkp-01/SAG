//! Per-app HTTP RPS gate (dataplane) and optional global forward circuit breaker.

use std::sync::atomic::{AtomicI64, AtomicU32, Ordering};
use std::sync::Arc;
use std::sync::Mutex;
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use dashmap::DashMap;
use tokio::sync::{OwnedSemaphorePermit, Semaphore};

/// An exact non-blocking admission limit. The only active-count source is the
/// semaphore's owned permits; cancellation and every early-return path release
/// capacity through RAII.
pub struct AdmissionGate {
    reason: &'static str,
    limit: usize,
    semaphore: Arc<Semaphore>,
}

impl AdmissionGate {
    pub fn new(reason: &'static str, limit: usize) -> Result<Self, &'static str> {
        if limit == 0 {
            return Err("admission limit must be greater than zero");
        }
        let gate = Self {
            reason,
            limit,
            semaphore: Arc::new(Semaphore::new(limit)),
        };
        gate.update_gauge();
        Ok(gate)
    }

    pub fn try_acquire(self: &Arc<Self>) -> Option<AdmissionPermit> {
        let started = Instant::now();
        let permit = self.semaphore.clone().try_acquire_owned().ok();
        metrics::histogram!("admission_wait_seconds", "gate" => self.reason)
            .record(started.elapsed().as_secs_f64());
        match permit {
            Some(permit) => {
                let admission = AdmissionPermit {
                    permit: Some(permit),
                    gate: self.clone(),
                };
                self.update_gauge();
                Some(admission)
            }
            None => {
                metrics::counter!("admission_rejected_total", "reason" => self.reason).increment(1);
                self.update_gauge();
                None
            }
        }
    }

    pub fn active(&self) -> usize {
        self.limit
            .saturating_sub(self.semaphore.available_permits())
    }

    /// Refreshes gauges from semaphore state immediately before a metrics scrape.
    pub fn refresh_metrics(&self) {
        self.update_gauge();
    }

    #[cfg(test)]
    pub fn available(&self) -> usize {
        self.semaphore.available_permits()
    }

    fn update_gauge(&self) {
        let active = self.active() as f64;
        metrics::gauge!("admission_active", "gate" => self.reason).set(active);
        match self.reason {
            "hard_limit" => metrics::gauge!("bridge_ingress_active").set(active),
            "sync_limit" => metrics::gauge!("bridge_sync_inflight").set(active),
            _ => {}
        }
    }
}

pub struct AdmissionPermit {
    permit: Option<OwnedSemaphorePermit>,
    gate: Arc<AdmissionGate>,
}

impl Drop for AdmissionPermit {
    fn drop(&mut self) {
        self.permit.take();
        self.gate.update_gauge();
    }
}

fn unix_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

struct TokenBucket {
    tokens: f64,
    last: Instant,
}

/// Token-bucket per `x-sag-app-id` (only constructed when `rps > 0`).
pub struct AppRpsLimiter {
    rate: f64,
    capacity: f64,
    buckets: DashMap<String, Mutex<TokenBucket>>,
}

impl AppRpsLimiter {
    pub fn new(rps: u64) -> Self {
        let r = rps.max(1) as f64;
        Self {
            rate: r,
            capacity: r,
            buckets: DashMap::new(),
        }
    }

    /// Returns `true` if the request may proceed.
    pub fn try_acquire(&self, app_id: &str) -> bool {
        let cell = self.buckets.entry(app_id.to_string()).or_insert_with(|| {
            Mutex::new(TokenBucket {
                tokens: self.capacity,
                last: Instant::now(),
            })
        });
        let mut inner = cell.lock().unwrap_or_else(|e| e.into_inner());
        let now = Instant::now();
        let elapsed = now.duration_since(inner.last).as_secs_f64();
        inner.last = now;
        inner.tokens = (inner.tokens + elapsed * self.rate).min(self.capacity);
        if inner.tokens >= 1.0 {
            inner.tokens -= 1.0;
            true
        } else {
            false
        }
    }
}

/// Consecutive full Unary `Forward` failures (both gRPC attempts failed) open a cool-off window.
pub struct ForwardCircuit {
    threshold: u32,
    cooloff_ms: i64,
    consecutive_failures: AtomicU32,
    open_until_ms: AtomicI64,
}

impl ForwardCircuit {
    pub fn new(threshold: u32, cooloff_ms: u64) -> Self {
        Self {
            threshold,
            cooloff_ms: cooloff_ms.max(100) as i64,
            consecutive_failures: AtomicU32::new(0),
            open_until_ms: AtomicI64::new(0),
        }
    }

    pub fn is_open(&self) -> bool {
        if self.threshold == 0 {
            return false;
        }
        let until = self.open_until_ms.load(Ordering::Acquire);
        if until == 0 {
            return false;
        }
        let now = unix_ms();
        if now < until {
            return true;
        }
        self.open_until_ms.store(0, Ordering::Release);
        self.consecutive_failures.store(0, Ordering::Release);
        false
    }

    pub fn record_success(&self) {
        if self.threshold == 0 {
            return;
        }
        self.consecutive_failures.store(0, Ordering::Release);
        self.open_until_ms.store(0, Ordering::Release);
    }

    pub fn record_full_failure(&self) {
        if self.threshold == 0 {
            return;
        }
        let f = self.consecutive_failures.fetch_add(1, Ordering::Relaxed) + 1;
        if f >= self.threshold {
            let until = unix_ms() + self.cooloff_ms;
            self.open_until_ms.store(until, Ordering::Release);
        }
        if f == self.threshold {
            metrics::counter!("bridge_forward_circuit_open_total").increment(1);
            tracing::warn!(
                threshold = self.threshold,
                cooloff_ms = self.cooloff_ms,
                "http-tunnel-bridge: forward circuit opened (consecutive Unary Forward failures)"
            );
        }
    }
}

#[cfg(test)]
mod admission_tests {
    use super::AdmissionGate;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;
    use tokio::sync::Barrier;

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn admission_barrier_never_exceeds_limit_and_releases_every_permit() {
        const LIMIT: usize = 8;
        const CONTENDERS: usize = LIMIT * 4;

        for _ in 0..100 {
            let gate = Arc::new(AdmissionGate::new("sync_limit", LIMIT).unwrap());
            let barrier = Arc::new(Barrier::new(CONTENDERS));
            let active = Arc::new(AtomicUsize::new(0));
            let maximum = Arc::new(AtomicUsize::new(0));
            let mut tasks = Vec::with_capacity(CONTENDERS);

            for _ in 0..CONTENDERS {
                let gate = gate.clone();
                let barrier = barrier.clone();
                let active = active.clone();
                let maximum = maximum.clone();
                tasks.push(tokio::spawn(async move {
                    barrier.wait().await;
                    let Some(_permit) = gate.try_acquire() else {
                        return false;
                    };
                    let now = active.fetch_add(1, Ordering::SeqCst) + 1;
                    maximum.fetch_max(now, Ordering::SeqCst);
                    tokio::time::sleep(std::time::Duration::from_millis(1)).await;
                    active.fetch_sub(1, Ordering::SeqCst);
                    true
                }));
            }

            let mut admitted = 0usize;
            for task in tasks {
                admitted += usize::from(task.await.unwrap());
            }
            assert!(admitted <= LIMIT);
            assert!(maximum.load(Ordering::SeqCst) <= LIMIT);
            assert_eq!(gate.active(), 0);
            assert_eq!(gate.available(), LIMIT);
        }
    }

    #[tokio::test]
    async fn admission_cancel_and_error_paths_release_permits() {
        let gate = Arc::new(AdmissionGate::new("hard_limit", 1).unwrap());
        let permit = gate.try_acquire().unwrap();
        assert_eq!(gate.active(), 1);
        drop(permit);
        assert_eq!(gate.active(), 0);

        let task_gate = gate.clone();
        let task = tokio::spawn(async move {
            let _permit = task_gate.try_acquire().unwrap();
            std::future::pending::<()>().await;
        });
        tokio::task::yield_now().await;
        assert_eq!(gate.active(), 1);
        task.abort();
        let _ = task.await;
        assert_eq!(gate.active(), 0);

        async fn error_after_admit(gate: &Arc<AdmissionGate>) -> Result<(), &'static str> {
            let _permit = gate.try_acquire().ok_or("rejected")?;
            Err("synthetic error")
        }
        assert!(error_after_admit(&gate).await.is_err());
        assert_eq!(gate.active(), 0);

        let timeout_gate = gate.clone();
        let timed = tokio::time::timeout(std::time::Duration::from_millis(1), async move {
            let _permit = timeout_gate.try_acquire().unwrap();
            std::future::pending::<()>().await;
        })
        .await;
        assert!(timed.is_err());
        assert_eq!(gate.active(), 0);
    }
}
