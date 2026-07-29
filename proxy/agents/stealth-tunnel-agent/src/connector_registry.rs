use std::collections::hash_map::Entry;
use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, RwLock};
use std::time::{Duration, Instant};

use sag_tunnel_proto::{
    tunnel_message, CancelRequest, ForwardAccepted, ForwardRequest, ForwardResponse, TunnelMessage,
};
use tokio::sync::{mpsc, oneshot, watch, OwnedSemaphorePermit};
use tracing::{debug, warn};

#[derive(Clone)]
struct OutboundEntry {
    generation: u64,
    stream_epoch: String,
    connector_id: String,
    tx: mpsc::Sender<TunnelMessage>,
    close_tx: watch::Sender<bool>,
    last_heartbeat: Instant,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExpiredSession {
    pub endpoint: String,
    pub connector_id: String,
    pub generation: u64,
    pub stream_epoch: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum PendingPhase {
    Queued,
    Sent,
    Accepted,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PendingFailure {
    StreamLost {
        phase: PendingPhase,
        stream_epoch: String,
    },
    ProtocolViolation {
        phase: PendingPhase,
        reason: String,
    },
}

struct PendingEntry {
    generation: u64,
    outbound_generation: u64,
    endpoint: String,
    stream_epoch: String,
    phase: PendingPhase,
    accepted_tx: watch::Sender<bool>,
    tx: oneshot::Sender<Result<ForwardResponse, PendingFailure>>,
}

/// Owns every resource associated with one transport attempt.
///
/// Dropping this value is the cancellation path used by tonic when the bridge
/// resets/times out its HTTP/2 stream. Cleanup is synchronous and generation
/// aware, so an old attempt can never delete a newer waiter.
pub struct PendingRequest {
    registry: ConnectorRegistry,
    request_id: String,
    attempt_id: String,
    generation: u64,
    outbound_generation: u64,
    endpoint: String,
    stream_epoch: String,
    receiver: Option<oneshot::Receiver<Result<ForwardResponse, PendingFailure>>>,
    accepted_rx: watch::Receiver<bool>,
    _permit: Option<OwnedSemaphorePermit>,
    terminal: bool,
}

impl PendingRequest {
    /// Resolves only after the Connector has durably reserved and explicitly
    /// accepted this exact stream-epoch attempt. `false` means the stream
    /// terminated before Agent observed that boundary.
    pub async fn wait_for_acceptance(&mut self) -> bool {
        if *self.accepted_rx.borrow() {
            return true;
        }
        self.accepted_rx
            .wait_for(|accepted| *accepted)
            .await
            .is_ok()
    }

    pub async fn recv(&mut self) -> Result<ForwardResponse, PendingFailure> {
        let result = self
            .receiver
            .as_mut()
            .expect("pending receiver consumed")
            .await
            .unwrap_or_else(|_| {
                Err(PendingFailure::ProtocolViolation {
                    phase: PendingPhase::Sent,
                    reason: "pending response channel closed without a terminal outcome".into(),
                })
            });
        self.terminal = true;
        result
    }
}

impl Drop for PendingRequest {
    fn drop(&mut self) {
        let removed = self
            .registry
            .remove_pending_if_generation(&self.attempt_id, self.generation);
        self.registry.pending_current.fetch_sub(1, Ordering::AcqRel);
        metrics::gauge!("agent_pending_waiters")
            .set(self.registry.pending_current.load(Ordering::Acquire) as f64);

        if !self.terminal && removed {
            metrics::counter!("agent_cancel_total", "reason" => "waiter_dropped").increment(1);
            self.registry.try_cancel(
                &self.endpoint,
                self.outbound_generation,
                CancelRequest {
                    request_id: self.request_id.clone(),
                    attempt_id: self.attempt_id.clone(),
                    reason: "agent waiter dropped".into(),
                    stream_epoch: self.stream_epoch.clone(),
                },
            );
        }
    }
}

#[derive(Clone)]
pub struct ConnectorRegistry {
    /// Logical endpoint -> all currently registered Connector sessions.
    outbound: Arc<RwLock<HashMap<String, Vec<OutboundEntry>>>>,
    /// attempt_id -> exactly one waiter. Logical request_id is deliberately not
    /// used as the key because retries and late responses are different attempts.
    pending: Arc<Mutex<HashMap<String, PendingEntry>>>,
    next_generation: Arc<AtomicU64>,
    next_session_pick: Arc<AtomicU64>,
    pending_current: Arc<AtomicUsize>,
}

impl Default for ConnectorRegistry {
    fn default() -> Self {
        Self {
            outbound: Arc::new(RwLock::new(HashMap::new())),
            pending: Arc::new(Mutex::new(HashMap::new())),
            next_generation: Arc::new(AtomicU64::new(1)),
            next_session_pick: Arc::new(AtomicU64::new(0)),
            pending_current: Arc::new(AtomicUsize::new(0)),
        }
    }
}

impl ConnectorRegistry {
    pub fn total_session_count(&self) -> usize {
        self.outbound
            .read()
            .expect("connector registry lock poisoned")
            .values()
            .map(Vec::len)
            .sum()
    }
    pub fn register(
        &self,
        endpoint: String,
        connector_id: String,
        stream_epoch: String,
        tx: mpsc::Sender<TunnelMessage>,
        close_tx: watch::Sender<bool>,
    ) -> u64 {
        let generation = self.next_generation.fetch_add(1, Ordering::Relaxed);
        let mut g = self
            .outbound
            .write()
            .expect("connector registry outbound poisoned");
        g.entry(endpoint).or_default().push(OutboundEntry {
            generation,
            stream_epoch,
            connector_id,
            tx,
            close_tx,
            last_heartbeat: Instant::now(),
        });
        let count = g.values().map(Vec::len).sum::<usize>();
        drop(g);
        metrics::gauge!("agent_connector_sessions").set(count as f64);
        generation
    }

    /// Removes only the generation that is actually closing. Other replicas in
    /// the same logical endpoint pool remain eligible for new requests.
    pub fn unregister(&self, endpoint: &str, generation: u64) {
        let (removed, count) = {
            let mut g = self
                .outbound
                .write()
                .expect("connector registry outbound poisoned");
            let mut removed = false;
            let mut remove_endpoint = false;
            if let Some(sessions) = g.get_mut(endpoint) {
                let old_len = sessions.len();
                sessions.retain(|entry| entry.generation != generation);
                removed = sessions.len() != old_len;
                remove_endpoint = sessions.is_empty();
            }
            if remove_endpoint {
                g.remove(endpoint);
            }
            (removed, g.values().map(Vec::len).sum::<usize>())
        };

        if !removed {
            return;
        }
        metrics::gauge!("agent_connector_sessions").set(count as f64);
        self.fail_pending_for_session(endpoint, generation);
    }

    pub fn register_heartbeat(
        &self,
        endpoint: &str,
        connector_id: &str,
        generation: u64,
        stream_epoch: &str,
    ) -> bool {
        let mut g = self
            .outbound
            .write()
            .expect("connector registry outbound poisoned");
        let Some(session) = g.get_mut(endpoint).and_then(|sessions| {
            sessions
                .iter_mut()
                .find(|entry| entry.generation == generation)
        }) else {
            return false;
        };
        if session.connector_id != connector_id || session.stream_epoch != stream_epoch {
            return false;
        }
        session.last_heartbeat = Instant::now();
        true
    }

    pub fn is_tunnel_healthy_with_window(&self, endpoint: &str, window: Duration) -> bool {
        let g = self
            .outbound
            .read()
            .expect("connector registry outbound poisoned");
        g.get(endpoint).is_some_and(|sessions| {
            sessions
                .iter()
                .any(|entry| entry.last_heartbeat.elapsed() < window)
        })
    }

    /// Revokes every session whose heartbeat lease has expired. Closing the
    /// response stream makes the Connector maintenance loop reconnect, while
    /// dropping pending senders wakes affected Agent handlers immediately.
    pub fn expire_stale(&self, max_age: Duration) -> Vec<ExpiredSession> {
        let (expired, count) = {
            let mut g = self
                .outbound
                .write()
                .expect("connector registry outbound poisoned");
            let mut expired = Vec::new();
            for (endpoint, sessions) in g.iter_mut() {
                let mut idx = 0;
                while idx < sessions.len() {
                    if sessions[idx].last_heartbeat.elapsed() >= max_age {
                        let session = sessions.swap_remove(idx);
                        let _ = session.close_tx.send(true);
                        expired.push(ExpiredSession {
                            endpoint: endpoint.clone(),
                            connector_id: session.connector_id,
                            generation: session.generation,
                            stream_epoch: session.stream_epoch,
                        });
                    } else {
                        idx += 1;
                    }
                }
            }
            g.retain(|_, sessions| !sessions.is_empty());
            let count = g.values().map(Vec::len).sum::<usize>();
            (expired, count)
        };

        for session in &expired {
            self.fail_pending_for_session(&session.endpoint, session.generation);
        }
        if !expired.is_empty() {
            metrics::gauge!("agent_connector_sessions").set(count as f64);
            metrics::counter!("agent_connector_session_expired_total")
                .increment(expired.len() as u64);
        }
        expired
    }

    fn fail_pending_for_session(&self, endpoint: &str, generation: u64) {
        // Send an explicit phase-bearing outcome before removing each waiter.
        let mut pending = self
            .pending
            .lock()
            .expect("connector registry pending poisoned");
        let attempts = pending
            .iter()
            .filter(|(_, entry)| {
                entry.endpoint == endpoint && entry.outbound_generation == generation
            })
            .map(|(attempt, _)| attempt.clone())
            .collect::<Vec<_>>();
        for attempt in attempts {
            if let Some(entry) = pending.remove(&attempt) {
                let _ = entry.tx.send(Err(PendingFailure::StreamLost {
                    phase: entry.phase,
                    stream_epoch: entry.stream_epoch,
                }));
            }
        }
    }

    fn select_healthy_session(&self, endpoint: &str, max_age: Duration) -> Option<OutboundEntry> {
        let g = self
            .outbound
            .read()
            .expect("connector registry outbound poisoned");
        let sessions = g.get(endpoint)?;
        let healthy_count = sessions
            .iter()
            .filter(|entry| entry.last_heartbeat.elapsed() < max_age)
            .count();
        if healthy_count == 0 {
            return None;
        }
        let pick = self.next_session_pick.fetch_add(1, Ordering::Relaxed) as usize % healthy_count;
        sessions
            .iter()
            .filter(|entry| entry.last_heartbeat.elapsed() < max_age)
            .nth(pick)
            .cloned()
    }

    pub async fn send_request_to_connector(
        &self,
        endpoint: &str,
        mut req: ForwardRequest,
        permit: OwnedSemaphorePermit,
        max_session_age: Duration,
    ) -> Result<PendingRequest, &'static str> {
        if req.attempt_id.is_empty() {
            // Backward compatibility for direct callers during a rolling upgrade.
            req.attempt_id = req.request_id.clone();
        }
        let attempt_id = req.attempt_id.clone();
        let request_id = req.request_id.clone();
        let outbound = self.select_healthy_session(endpoint, max_session_age);
        let Some(outbound) = outbound else {
            return Err("no healthy connector stream");
        };
        req.stream_epoch = outbound.stream_epoch.clone();

        let generation = self.next_generation.fetch_add(1, Ordering::Relaxed);
        let (done_tx, done_rx) = oneshot::channel();
        let (accepted_tx, accepted_rx) = watch::channel(false);
        {
            let mut pending = self
                .pending
                .lock()
                .expect("connector registry pending poisoned");
            match pending.entry(attempt_id.clone()) {
                Entry::Vacant(slot) => {
                    slot.insert(PendingEntry {
                        generation,
                        outbound_generation: outbound.generation,
                        endpoint: endpoint.to_string(),
                        stream_epoch: outbound.stream_epoch.clone(),
                        phase: PendingPhase::Queued,
                        accepted_tx,
                        tx: done_tx,
                    });
                }
                Entry::Occupied(_) => return Err("duplicate connector attempt_id"),
            }
        }
        self.pending_current.fetch_add(1, Ordering::AcqRel);
        metrics::gauge!("agent_pending_waiters")
            .set(self.pending_current.load(Ordering::Acquire) as f64);

        let guard = PendingRequest {
            registry: self.clone(),
            request_id,
            attempt_id,
            generation,
            outbound_generation: outbound.generation,
            endpoint: endpoint.to_string(),
            stream_epoch: outbound.stream_epoch.clone(),
            receiver: Some(done_rx),
            accepted_rx,
            _permit: Some(permit),
            terminal: false,
        };

        let msg = TunnelMessage {
            payload: Some(tunnel_message::Payload::Request(req)),
        };
        if outbound.tx.send(msg).await.is_err() {
            return Err("send to connector failed");
        }
        self.advance_phase(&guard.attempt_id, guard.generation, PendingPhase::Sent);
        Ok(guard)
    }

    fn advance_phase(&self, attempt_id: &str, generation: u64, phase: PendingPhase) -> bool {
        let mut pending = self
            .pending
            .lock()
            .expect("connector registry pending poisoned");
        let Some(entry) = pending.get_mut(attempt_id) else {
            return false;
        };
        if entry.generation != generation {
            return false;
        }
        entry.phase = entry.phase.max(phase);
        true
    }

    pub fn resolve_accepted(&self, outbound_generation: u64, accepted: ForwardAccepted) -> bool {
        let attempt_id = if accepted.attempt_id.is_empty() {
            accepted.request_id
        } else {
            accepted.attempt_id
        };
        let mut pending = self
            .pending
            .lock()
            .expect("connector registry pending poisoned");
        let Some(entry) = pending.get_mut(&attempt_id) else {
            return false;
        };
        if entry.outbound_generation != outbound_generation
            || entry.stream_epoch != accepted.stream_epoch
        {
            return false;
        }
        entry.phase = entry.phase.max(PendingPhase::Accepted);
        let _ = entry.accepted_tx.send(true);
        true
    }

    pub fn resolve_response(
        &self,
        outbound_generation: u64,
        stream_epoch: &str,
        mut resp: ForwardResponse,
    ) {
        if resp.attempt_id.is_empty() {
            resp.attempt_id = resp.request_id.clone();
        }
        let (entry, expected_generation) = {
            let mut pending = self
                .pending
                .lock()
                .expect("connector registry pending poisoned");
            match pending.get(&resp.attempt_id) {
                Some(entry)
                    if entry.outbound_generation == outbound_generation
                        && entry.stream_epoch == stream_epoch
                        && resp.stream_epoch == stream_epoch =>
                {
                    (pending.remove(&resp.attempt_id), None)
                }
                Some(entry) => (None, Some(entry.outbound_generation)),
                None => (None, None),
            }
        };
        match entry {
            Some(entry) => {
                if entry.tx.send(Ok(resp)).is_err() {
                    metrics::counter!("agent_late_response_total", "reason" => "waiter_closed")
                        .increment(1);
                    warn!("forward response arrived after waiter closed");
                }
            }
            None => {
                if let Some(expected_generation) = expected_generation {
                    metrics::counter!(
                        "agent_late_response_total",
                        "reason" => "wrong_session"
                    )
                    .increment(1);
                    warn!(
                        request_id = %resp.request_id,
                        attempt_id = %resp.attempt_id,
                        expected_generation,
                        response_generation = outbound_generation,
                        "Connector response came from the wrong session generation"
                    );
                } else {
                    metrics::counter!("agent_late_response_total", "reason" => "no_attempt")
                        .increment(1);
                    warn!(
                        request_id = %resp.request_id,
                        attempt_id = %resp.attempt_id,
                        "late connector response has no matching attempt"
                    );
                }
            }
        }
    }

    fn remove_pending_if_generation(&self, attempt_id: &str, generation: u64) -> bool {
        let mut pending = self
            .pending
            .lock()
            .expect("connector registry pending poisoned");
        if pending
            .get(attempt_id)
            .is_some_and(|entry| entry.generation == generation)
        {
            pending.remove(attempt_id);
            true
        } else {
            false
        }
    }

    fn try_cancel(&self, endpoint: &str, generation: u64, cancel: CancelRequest) {
        let outbound = self
            .outbound
            .read()
            .expect("connector registry outbound poisoned")
            .get(endpoint)
            .and_then(|sessions| {
                sessions
                    .iter()
                    .find(|entry| entry.generation == generation)
                    .cloned()
            });
        if let Some(outbound) = outbound {
            let request_id = cancel.request_id.clone();
            let attempt_id = cancel.attempt_id.clone();
            if cancel.stream_epoch != outbound.stream_epoch {
                return;
            }
            if let Err(err) = outbound.tx.try_send(TunnelMessage {
                payload: Some(tunnel_message::Payload::Cancel(cancel)),
            }) {
                metrics::counter!("agent_cancel_total", "reason" => "stream_send_failed")
                    .increment(1);
                debug!(%request_id, %attempt_id, ?err, "connector cancel could not be queued");
            }
        }
    }

    #[cfg(test)]
    fn pending_len(&self) -> usize {
        self.pending
            .lock()
            .expect("connector registry pending poisoned")
            .len()
    }

    #[cfg(test)]
    fn pending_current(&self) -> usize {
        self.pending_current.load(Ordering::Acquire)
    }

    #[cfg(test)]
    fn session_count(&self, endpoint: &str) -> usize {
        self.outbound
            .read()
            .expect("connector registry outbound poisoned")
            .get(endpoint)
            .map_or(0, Vec::len)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::sync::Semaphore;

    fn register_session(
        registry: &ConnectorRegistry,
        endpoint: &str,
        connector_id: &str,
        capacity: usize,
    ) -> (u64, mpsc::Receiver<TunnelMessage>, watch::Receiver<bool>) {
        let (tx, rx) = mpsc::channel(capacity);
        let (close_tx, close_rx) = watch::channel(false);
        let generation = registry.register(
            endpoint.into(),
            connector_id.into(),
            format!("epoch-{connector_id}"),
            tx,
            close_tx,
        );
        (generation, rx, close_rx)
    }

    fn request(request_id: &str, attempt_id: &str) -> ForwardRequest {
        ForwardRequest {
            request_id: request_id.into(),
            attempt_id: attempt_id.into(),
            ..Default::default()
        }
    }

    #[tokio::test]
    async fn duplicate_attempt_id_is_rejected_without_overwriting_waiter() {
        let registry = ConnectorRegistry::default();
        let (_session_generation, mut rx, _close_rx) =
            register_session(&registry, "connector:stream", "connector", 4);
        let sem = Arc::new(Semaphore::new(2));

        let first = registry
            .send_request_to_connector(
                "connector:stream",
                request("logical", "attempt"),
                sem.clone().acquire_owned().await.unwrap(),
                Duration::from_secs(10),
            )
            .await
            .unwrap();
        let _ = rx.recv().await;

        let second = registry
            .send_request_to_connector(
                "connector:stream",
                request("logical", "attempt"),
                sem.clone().acquire_owned().await.unwrap(),
                Duration::from_secs(10),
            )
            .await;
        assert!(matches!(second, Err("duplicate connector attempt_id")));
        assert_eq!(registry.pending_len(), 1);
        drop(first);
        assert_eq!(registry.pending_len(), 0);
        assert_eq!(registry.pending_current(), 0);
        assert_eq!(sem.available_permits(), 2);
        let cancel = rx.recv().await.expect("dropping waiter must emit cancel");
        assert!(matches!(
            cancel.payload,
            Some(tunnel_message::Payload::Cancel(CancelRequest {
                request_id,
                attempt_id,
                ..
            })) if request_id == "logical" && attempt_id == "attempt"
        ));
    }

    #[tokio::test]
    async fn stale_unregister_does_not_remove_new_connector_stream() {
        let registry = ConnectorRegistry::default();
        let (old_generation, _old_rx, _old_close) =
            register_session(&registry, "connector:stream", "old", 1);
        let (_new_generation, mut new_rx, _new_close) =
            register_session(&registry, "connector:stream", "new", 1);

        registry.unregister("connector:stream", old_generation);
        assert_eq!(registry.session_count("connector:stream"), 1);
        let sem = Arc::new(Semaphore::new(1));
        let pending = registry
            .send_request_to_connector(
                "connector:stream",
                request("logical", "new-attempt"),
                sem.acquire_owned().await.unwrap(),
                Duration::from_secs(10),
            )
            .await
            .unwrap();
        assert!(new_rx.recv().await.is_some());
        drop(pending);
    }

    #[tokio::test]
    async fn response_completion_removes_once_without_emitting_cancel() {
        let registry = ConnectorRegistry::default();
        let (session_generation, mut rx, _close_rx) =
            register_session(&registry, "connector:stream", "connector", 4);
        let sem = Arc::new(Semaphore::new(1));
        let mut pending = registry
            .send_request_to_connector(
                "connector:stream",
                request("logical", "attempt"),
                sem.clone().acquire_owned().await.unwrap(),
                Duration::from_secs(10),
            )
            .await
            .unwrap();
        let _ = rx.recv().await;

        registry.resolve_response(
            session_generation,
            "epoch-connector",
            ForwardResponse {
                request_id: "logical".into(),
                attempt_id: "attempt".into(),
                status_code: 200,
                stream_epoch: "epoch-connector".into(),
                ..Default::default()
            },
        );
        assert_eq!(pending.recv().await.unwrap().status_code, 200);
        drop(pending);

        assert_eq!(registry.pending_len(), 0);
        assert_eq!(registry.pending_current(), 0);
        assert_eq!(sem.available_permits(), 1);
        assert!(matches!(
            rx.try_recv(),
            Err(mpsc::error::TryRecvError::Empty)
        ));
    }

    #[tokio::test]
    async fn unregistering_one_replica_fails_over_to_the_other() {
        let registry = ConnectorRegistry::default();
        let (first_generation, mut first_rx, _first_close) =
            register_session(&registry, "connector:stream", "connector-1", 2);
        let (_second_generation, mut second_rx, _second_close) =
            register_session(&registry, "connector:stream", "connector-2", 2);
        let sem = Arc::new(Semaphore::new(2));

        let first_pending = registry
            .send_request_to_connector(
                "connector:stream",
                request("logical-1", "attempt-1"),
                sem.clone().acquire_owned().await.unwrap(),
                Duration::from_secs(10),
            )
            .await
            .unwrap();
        assert!(first_rx.recv().await.is_some());
        drop(first_pending);

        registry.unregister("connector:stream", first_generation);
        let second_pending = registry
            .send_request_to_connector(
                "connector:stream",
                request("logical-2", "attempt-2"),
                sem.acquire_owned().await.unwrap(),
                Duration::from_secs(10),
            )
            .await
            .unwrap();
        assert!(second_rx.recv().await.is_some());
        drop(second_pending);
    }

    #[tokio::test]
    async fn session_loss_wakes_only_attempts_assigned_to_that_generation() {
        let registry = ConnectorRegistry::default();
        let (first_generation, mut first_rx, _first_close) =
            register_session(&registry, "connector:stream", "connector-1", 2);
        let (second_generation, mut second_rx, _second_close) =
            register_session(&registry, "connector:stream", "connector-2", 2);
        let sem = Arc::new(Semaphore::new(2));

        let mut first_pending = registry
            .send_request_to_connector(
                "connector:stream",
                request("logical-1", "attempt-1"),
                sem.clone().acquire_owned().await.unwrap(),
                Duration::from_secs(10),
            )
            .await
            .unwrap();
        let mut second_pending = registry
            .send_request_to_connector(
                "connector:stream",
                request("logical-2", "attempt-2"),
                sem.acquire_owned().await.unwrap(),
                Duration::from_secs(10),
            )
            .await
            .unwrap();
        assert!(first_rx.recv().await.is_some());
        assert!(second_rx.recv().await.is_some());

        registry.unregister("connector:stream", first_generation);
        assert_eq!(
            first_pending.recv().await,
            Err(PendingFailure::StreamLost {
                phase: PendingPhase::Sent,
                stream_epoch: "epoch-connector-1".into(),
            })
        );
        registry.resolve_response(
            second_generation,
            "epoch-connector-2",
            ForwardResponse {
                request_id: "logical-2".into(),
                attempt_id: "attempt-2".into(),
                status_code: 200,
                stream_epoch: "epoch-connector-2".into(),
                ..Default::default()
            },
        );
        assert_eq!(second_pending.recv().await.unwrap().status_code, 200);
    }

    #[tokio::test]
    async fn heartbeat_and_response_are_bound_to_the_stream_generation() {
        let registry = ConnectorRegistry::default();
        let (generation, mut rx, _close_rx) =
            register_session(&registry, "connector:stream", "connector", 2);

        assert!(!registry.register_heartbeat(
            "connector:stream",
            "connector",
            generation + 100,
            "epoch-connector"
        ));
        assert!(!registry.register_heartbeat(
            "connector:stream",
            "other-connector",
            generation,
            "epoch-connector"
        ));
        assert!(!registry.register_heartbeat(
            "connector:stream",
            "connector",
            generation,
            "old-epoch"
        ));
        assert!(registry.register_heartbeat(
            "connector:stream",
            "connector",
            generation,
            "epoch-connector"
        ));

        let sem = Arc::new(Semaphore::new(1));
        let mut pending = registry
            .send_request_to_connector(
                "connector:stream",
                request("logical", "attempt"),
                sem.acquire_owned().await.unwrap(),
                Duration::from_secs(10),
            )
            .await
            .unwrap();
        let _ = rx.recv().await;
        registry.resolve_response(
            generation + 100,
            "epoch-connector",
            ForwardResponse {
                request_id: "logical".into(),
                attempt_id: "attempt".into(),
                status_code: 418,
                stream_epoch: "epoch-connector".into(),
                ..Default::default()
            },
        );
        assert_eq!(registry.pending_len(), 1);
        registry.resolve_response(
            generation,
            "epoch-connector",
            ForwardResponse {
                request_id: "logical".into(),
                attempt_id: "attempt".into(),
                status_code: 200,
                stream_epoch: "epoch-connector".into(),
                ..Default::default()
            },
        );
        assert_eq!(pending.recv().await.unwrap().status_code, 200);
    }

    #[tokio::test]
    async fn old_epoch_acceptance_and_response_cannot_complete_current_attempt() {
        let registry = ConnectorRegistry::default();
        let (generation, mut rx, _close_rx) =
            register_session(&registry, "connector:stream", "connector", 2);
        let sem = Arc::new(Semaphore::new(1));
        let mut pending = registry
            .send_request_to_connector(
                "connector:stream",
                request("logical", "attempt"),
                sem.acquire_owned().await.unwrap(),
                Duration::from_secs(10),
            )
            .await
            .unwrap();
        let _ = rx.recv().await;

        assert!(!registry.resolve_accepted(
            generation,
            ForwardAccepted {
                request_id: "logical".into(),
                attempt_id: "attempt".into(),
                stream_epoch: "old-epoch".into(),
            },
        ));
        registry.resolve_response(
            generation,
            "epoch-connector",
            ForwardResponse {
                request_id: "logical".into(),
                attempt_id: "attempt".into(),
                status_code: 418,
                stream_epoch: "old-epoch".into(),
                ..Default::default()
            },
        );
        assert_eq!(registry.pending_len(), 1);

        assert!(registry.resolve_accepted(
            generation,
            ForwardAccepted {
                request_id: "logical".into(),
                attempt_id: "attempt".into(),
                stream_epoch: "epoch-connector".into(),
            },
        ));
        registry.unregister("connector:stream", generation);
        assert_eq!(
            pending.recv().await,
            Err(PendingFailure::StreamLost {
                phase: PendingPhase::Accepted,
                stream_epoch: "epoch-connector".into(),
            })
        );
    }

    #[tokio::test]
    async fn stale_session_is_closed_and_removed_from_health() {
        let registry = ConnectorRegistry::default();
        let (generation, _rx, mut close_rx) =
            register_session(&registry, "connector:stream", "connector", 1);
        assert!(registry.is_tunnel_healthy_with_window("connector:stream", Duration::from_secs(10)));

        let expired = registry.expire_stale(Duration::ZERO);
        assert_eq!(
            expired,
            vec![ExpiredSession {
                endpoint: "connector:stream".into(),
                connector_id: "connector".into(),
                generation,
                stream_epoch: "epoch-connector".into(),
            }]
        );
        close_rx.changed().await.unwrap();
        assert!(*close_rx.borrow());
        assert!(
            !registry.is_tunnel_healthy_with_window("connector:stream", Duration::from_secs(10))
        );
        assert!(!registry.register_heartbeat(
            "connector:stream",
            "connector",
            generation,
            "epoch-connector"
        ));
    }
}
