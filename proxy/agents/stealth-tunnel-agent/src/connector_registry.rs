use std::collections::hash_map::Entry;
use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, RwLock};
use std::time::{Duration, Instant};

use sag_tunnel_proto::{
    tunnel_message, CancelRequest, ForwardAccepted, ForwardRequest, ForwardResponse, HealthProbe,
    HealthProbeAck, TunnelMessage,
};
use tokio::sync::{mpsc, oneshot, watch, OwnedSemaphorePermit};
use tracing::{debug, warn};
use uuid::Uuid;

#[derive(Debug, Clone, Copy)]
pub struct ProbePolicy {
    pub enabled: bool,
    pub freshness: Duration,
    pub startup_grace: Duration,
    pub failure_threshold: u8,
}

impl ProbePolicy {
    fn disabled() -> Self {
        Self {
            enabled: false,
            freshness: Duration::MAX,
            startup_grace: Duration::MAX,
            failure_threshold: u8::MAX,
        }
    }
}

#[derive(Clone)]
struct OutboundEntry {
    generation: u64,
    stream_epoch: String,
    connector_id: String,
    tx: mpsc::Sender<TunnelMessage>,
    close_tx: watch::Sender<bool>,
    registered_at: Instant,
    last_heartbeat: Instant,
    last_probe_success: Option<Instant>,
    probe_in_flight: bool,
    consecutive_probe_failures: u8,
}

impl OutboundEntry {
    fn is_eligible(&self, heartbeat_max_age: Duration, probe_policy: ProbePolicy) -> bool {
        let probe_fresh = !probe_policy.enabled
            || self
                .last_probe_success
                .is_some_and(|success| success.elapsed() < probe_policy.freshness)
            || (self.last_probe_success.is_none()
                && self.registered_at.elapsed() < probe_policy.startup_grace);
        self.last_heartbeat.elapsed() < heartbeat_max_age
            && probe_fresh
            && !self.tx.is_closed()
            && self.tx.capacity() > 0
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProbeTarget {
    pub endpoint: String,
    pub generation: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProbeOutcome {
    Healthy,
    AlreadyInFlight,
    TimedOut,
    SessionGone,
    Revoked,
}

struct ProbePending {
    endpoint: String,
    generation: u64,
    stream_epoch: String,
    tx: oneshot::Sender<bool>,
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
    revoke_on_drop_armed: bool,
}

struct AttemptReservation {
    registry: ConnectorRegistry,
    attempt_id: String,
    generation: u64,
    active: bool,
}

impl AttemptReservation {
    fn release(mut self) {
        self.registry
            .release_attempt_reservation(&self.attempt_id, self.generation);
        self.active = false;
    }
}

impl Drop for AttemptReservation {
    fn drop(&mut self) {
        if self.active {
            self.registry
                .release_attempt_reservation(&self.attempt_id, self.generation);
        }
    }
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

    /// Revokes exactly the Connector session carrying this attempt. Callers use
    /// this when a session-level timeout makes the transport unsafe for new
    /// work; the private generation fence prevents an old waiter from closing a
    /// replacement session registered under the same logical endpoint.
    pub fn revoke_session(&self, reason: &'static str) -> bool {
        self.registry
            .revoke_session(&self.endpoint, self.outbound_generation, reason)
    }
}

impl Drop for PendingRequest {
    fn drop(&mut self) {
        if !self.terminal && self.revoke_on_drop_armed {
            metrics::counter!("agent_cancel_total", "reason" => "waiter_dropped_revoke")
                .increment(1);
            self.registry.revoke_session(
                &self.endpoint,
                self.outbound_generation,
                "waiter_dropped",
            );
            self.registry
                .remove_pending_if_generation(&self.attempt_id, self.generation);
            self.registry.finish_pending_attempt();
            return;
        }

        let removed = self
            .registry
            .remove_pending_if_generation(&self.attempt_id, self.generation);
        self.registry.finish_pending_attempt();

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
    /// attempt_id -> dispatch reservation generation. This reservation spans
    /// the complete candidate retry loop, including the gap between removing a
    /// failed candidate assignment and inserting the next one.
    dispatching_attempts: Arc<Mutex<HashMap<String, u64>>>,
    next_generation: Arc<AtomicU64>,
    next_session_pick: Arc<AtomicU64>,
    pending_current: Arc<AtomicUsize>,
    probe_pending: Arc<Mutex<HashMap<String, ProbePending>>>,
    probe_policy: ProbePolicy,
}

impl Default for ConnectorRegistry {
    fn default() -> Self {
        Self {
            outbound: Arc::new(RwLock::new(HashMap::new())),
            pending: Arc::new(Mutex::new(HashMap::new())),
            dispatching_attempts: Arc::new(Mutex::new(HashMap::new())),
            next_generation: Arc::new(AtomicU64::new(1)),
            next_session_pick: Arc::new(AtomicU64::new(0)),
            pending_current: Arc::new(AtomicUsize::new(0)),
            probe_pending: Arc::new(Mutex::new(HashMap::new())),
            probe_policy: ProbePolicy::disabled(),
        }
    }
}

impl ConnectorRegistry {
    pub fn with_probe_policy(probe_policy: ProbePolicy) -> Self {
        Self {
            probe_policy,
            ..Self::default()
        }
    }

    pub fn healthy_session_count(&self, window: Duration) -> usize {
        self.outbound
            .read()
            .expect("connector registry lock poisoned")
            .values()
            .flat_map(|sessions| sessions.iter())
            .filter(|entry| entry.is_eligible(window, self.probe_policy))
            .count()
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
        let now = Instant::now();
        g.entry(endpoint).or_default().push(OutboundEntry {
            generation,
            stream_epoch,
            connector_id,
            tx,
            close_tx,
            registered_at: now,
            last_heartbeat: now,
            last_probe_success: None,
            probe_in_flight: false,
            consecutive_probe_failures: 0,
        });
        let count = g.values().map(Vec::len).sum::<usize>();
        drop(g);
        metrics::gauge!("agent_connector_sessions").set(count as f64);
        generation
    }

    /// Removes only the generation that is actually closing. Other replicas in
    /// the same logical endpoint pool remain eligible for new requests.
    pub fn unregister(&self, endpoint: &str, generation: u64) {
        let (removed, count) = self.take_session(endpoint, generation);
        if removed.is_none() {
            return;
        }
        metrics::gauge!("agent_connector_sessions").set(count as f64);
        self.fail_pending_for_session(endpoint, generation);
        self.fail_probes_for_session(endpoint, generation);
    }

    /// Removes and closes only the named session generation, then wakes the
    /// attempts assigned to it. A late failure from an old stream therefore
    /// cannot revoke a newly registered replacement for the same endpoint.
    pub fn revoke_session(&self, endpoint: &str, generation: u64, reason: &'static str) -> bool {
        let (removed, count) = self.take_session(endpoint, generation);
        let Some(session) = removed else {
            return false;
        };

        let _ = session.close_tx.send(true);
        metrics::gauge!("agent_connector_sessions").set(count as f64);
        metrics::counter!("agent_connector_session_revoked_total", "reason" => reason).increment(1);
        self.fail_pending_for_session(endpoint, generation);
        self.fail_probes_for_session(endpoint, generation);
        true
    }

    fn take_session(&self, endpoint: &str, generation: u64) -> (Option<OutboundEntry>, usize) {
        let mut outbound = self
            .outbound
            .write()
            .expect("connector registry outbound poisoned");
        let mut removed = None;
        let mut remove_endpoint = false;
        if let Some(sessions) = outbound.get_mut(endpoint) {
            if let Some(index) = sessions
                .iter()
                .position(|entry| entry.generation == generation)
            {
                removed = Some(sessions.swap_remove(index));
                remove_endpoint = sessions.is_empty();
            }
        }
        if remove_endpoint {
            outbound.remove(endpoint);
        }
        let count = outbound.values().map(Vec::len).sum::<usize>();
        (removed, count)
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

    pub fn probe_targets(&self) -> Vec<ProbeTarget> {
        if !self.probe_policy.enabled {
            return Vec::new();
        }
        self.outbound
            .read()
            .expect("connector registry outbound poisoned")
            .iter()
            .flat_map(|(endpoint, sessions)| {
                sessions.iter().map(|session| ProbeTarget {
                    endpoint: endpoint.clone(),
                    generation: session.generation,
                })
            })
            .collect()
    }

    /// Sends one transport-only probe through the same outbound stream and the
    /// Connector's bounded dispatcher queue used by business requests. Probe
    /// waiters are isolated from the business pending map and its admission
    /// gauge/semaphore.
    pub async fn probe_session(&self, target: ProbeTarget, timeout: Duration) -> ProbeOutcome {
        if !self.probe_policy.enabled {
            return ProbeOutcome::SessionGone;
        }

        let probe_id = Uuid::new_v4().to_string();
        let (stream_epoch, sender, mut receiver) = {
            // Keep the session write guard through probe waiter insertion so
            // unregister cannot remove the generation before it is tracked.
            let mut outbound = self
                .outbound
                .write()
                .expect("connector registry outbound poisoned");
            let Some(session) = outbound.get_mut(&target.endpoint).and_then(|sessions| {
                sessions
                    .iter_mut()
                    .find(|session| session.generation == target.generation)
            }) else {
                return ProbeOutcome::SessionGone;
            };
            if session.probe_in_flight {
                return ProbeOutcome::AlreadyInFlight;
            }
            session.probe_in_flight = true;
            let (done_tx, done_rx) = oneshot::channel();
            self.probe_pending
                .lock()
                .expect("connector probe pending poisoned")
                .insert(
                    probe_id.clone(),
                    ProbePending {
                        endpoint: target.endpoint.clone(),
                        generation: target.generation,
                        stream_epoch: session.stream_epoch.clone(),
                        tx: done_tx,
                    },
                );
            (session.stream_epoch.clone(), session.tx.clone(), done_rx)
        };

        let probe = HealthProbe {
            probe_id: probe_id.clone(),
            stream_epoch,
            sent_unix_ms: unix_time_ms(),
        };
        if sender
            .try_send(TunnelMessage {
                payload: Some(tunnel_message::Payload::HealthProbe(probe)),
            })
            .is_err()
        {
            self.remove_probe_if_generation(&probe_id, &target.endpoint, target.generation);
            self.clear_probe_in_flight(&target.endpoint, target.generation);
            metrics::counter!("agent_connector_probe_total", "result" => "send_failed")
                .increment(1);
            self.revoke_session(&target.endpoint, target.generation, "probe_send_failed");
            return ProbeOutcome::Revoked;
        }

        match tokio::time::timeout(timeout, &mut receiver).await {
            Ok(Ok(true)) => {
                metrics::counter!("agent_connector_probe_total", "result" => "ok").increment(1);
                ProbeOutcome::Healthy
            }
            Ok(Ok(false)) | Ok(Err(_)) => ProbeOutcome::SessionGone,
            Err(_) => {
                if self
                    .remove_probe_if_generation(&probe_id, &target.endpoint, target.generation)
                    .is_some()
                {
                    self.clear_probe_in_flight(&target.endpoint, target.generation);
                    metrics::counter!("agent_connector_probe_total", "result" => "timeout")
                        .increment(1);
                    match self.record_probe_timeout(&target.endpoint, target.generation) {
                        Some(true) => {
                            self.revoke_session(
                                &target.endpoint,
                                target.generation,
                                "probe_timeout_threshold",
                            );
                            ProbeOutcome::Revoked
                        }
                        Some(false) => ProbeOutcome::TimedOut,
                        None => ProbeOutcome::SessionGone,
                    }
                } else {
                    // The ACK won the deadline race after timeout stopped
                    // polling the receiver. Resolution always sends a terminal
                    // value after removing the waiter, so this cannot hang.
                    match receiver.await {
                        Ok(true) => ProbeOutcome::Healthy,
                        Ok(false) | Err(_) => ProbeOutcome::SessionGone,
                    }
                }
            }
        }
    }

    /// Applies a probe ACK only to the exact registered generation and epoch.
    /// A late ACK from an old stream can neither make a replacement fresh nor
    /// clear the replacement's in-flight state.
    pub fn resolve_probe_ack(
        &self,
        outbound_generation: u64,
        registered_stream_epoch: &str,
        ack: HealthProbeAck,
    ) -> bool {
        let mut outbound = self
            .outbound
            .write()
            .expect("connector registry outbound poisoned");
        let Some(session) = outbound
            .values_mut()
            .flat_map(|sessions| sessions.iter_mut())
            .find(|session| {
                session.generation == outbound_generation
                    && session.stream_epoch == registered_stream_epoch
            })
        else {
            return false;
        };
        let pending = {
            let mut probes = self
                .probe_pending
                .lock()
                .expect("connector probe pending poisoned");
            let matches = probes.get(&ack.probe_id).is_some_and(|pending| {
                pending.generation == outbound_generation
                    && pending.stream_epoch == registered_stream_epoch
                    && ack.stream_epoch == registered_stream_epoch
            });
            matches.then(|| probes.remove(&ack.probe_id)).flatten()
        };
        let Some(pending) = pending else {
            return false;
        };
        session.last_probe_success = Some(Instant::now());
        session.probe_in_flight = false;
        session.consecutive_probe_failures = 0;
        drop(outbound);
        let _ = pending.tx.send(true);
        true
    }

    fn remove_probe_if_generation(
        &self,
        probe_id: &str,
        endpoint: &str,
        generation: u64,
    ) -> Option<ProbePending> {
        let mut probes = self
            .probe_pending
            .lock()
            .expect("connector probe pending poisoned");
        if probes
            .get(probe_id)
            .is_some_and(|pending| pending.endpoint == endpoint && pending.generation == generation)
        {
            probes.remove(probe_id)
        } else {
            None
        }
    }

    fn clear_probe_in_flight(&self, endpoint: &str, generation: u64) {
        if let Some(session) = self
            .outbound
            .write()
            .expect("connector registry outbound poisoned")
            .get_mut(endpoint)
            .and_then(|sessions| {
                sessions
                    .iter_mut()
                    .find(|session| session.generation == generation)
            })
        {
            session.probe_in_flight = false;
        }
    }

    fn record_probe_timeout(&self, endpoint: &str, generation: u64) -> Option<bool> {
        let mut outbound = self
            .outbound
            .write()
            .expect("connector registry outbound poisoned");
        let session = outbound.get_mut(endpoint).and_then(|sessions| {
            sessions
                .iter_mut()
                .find(|session| session.generation == generation)
        })?;
        session.consecutive_probe_failures = session.consecutive_probe_failures.saturating_add(1);
        Some(session.consecutive_probe_failures >= self.probe_policy.failure_threshold)
    }

    fn fail_probes_for_session(&self, endpoint: &str, generation: u64) {
        let mut probes = self
            .probe_pending
            .lock()
            .expect("connector probe pending poisoned");
        let probe_ids = probes
            .iter()
            .filter(|(_, pending)| pending.endpoint == endpoint && pending.generation == generation)
            .map(|(probe_id, _)| probe_id.clone())
            .collect::<Vec<_>>();
        for probe_id in probe_ids {
            if let Some(pending) = probes.remove(&probe_id) {
                let _ = pending.tx.send(false);
            }
        }
    }

    pub fn is_tunnel_healthy_with_window(&self, endpoint: &str, window: Duration) -> bool {
        let g = self
            .outbound
            .read()
            .expect("connector registry outbound poisoned");
        g.get(endpoint).is_some_and(|sessions| {
            sessions
                .iter()
                .any(|entry| entry.is_eligible(window, self.probe_policy))
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
            self.fail_probes_for_session(&session.endpoint, session.generation);
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

    fn select_eligible_sessions(&self, endpoint: &str, max_age: Duration) -> Vec<OutboundEntry> {
        let g = self
            .outbound
            .read()
            .expect("connector registry outbound poisoned");
        let Some(sessions) = g.get(endpoint) else {
            return Vec::new();
        };
        let mut eligible = sessions
            .iter()
            .filter(|entry| entry.is_eligible(max_age, self.probe_policy))
            .cloned()
            .collect::<Vec<_>>();
        if !eligible.is_empty() {
            let pick =
                self.next_session_pick.fetch_add(1, Ordering::Relaxed) as usize % eligible.len();
            eligible.rotate_left(pick);
        }
        eligible
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
        let candidates = self.select_eligible_sessions(endpoint, max_session_age);
        if candidates.is_empty() {
            return Err("no healthy connector stream");
        }
        self.send_request_to_candidates(endpoint, req, permit, candidates)
    }

    fn send_request_to_candidates(
        &self,
        endpoint: &str,
        req: ForwardRequest,
        permit: OwnedSemaphorePermit,
        candidates: Vec<OutboundEntry>,
    ) -> Result<PendingRequest, &'static str> {
        self.send_request_to_candidates_with_retry_hook(endpoint, req, permit, candidates, || {})
    }

    fn send_request_to_candidates_with_retry_hook<F>(
        &self,
        endpoint: &str,
        req: ForwardRequest,
        permit: OwnedSemaphorePermit,
        candidates: Vec<OutboundEntry>,
        mut before_revoke: F,
    ) -> Result<PendingRequest, &'static str>
    where
        F: FnMut(),
    {
        let attempt_id = req.attempt_id.clone();
        let request_id = req.request_id.clone();
        let reservation = self.reserve_attempt(&attempt_id)?;
        let mut send_failed = false;

        for outbound in candidates {
            // Lock order is outbound -> pending. Revocation takes the outbound
            // write lock first and releases it before failing pending entries.
            // Holding this read guard through pending insertion and try_send
            // prevents unregister from missing a newly assigned attempt.
            let outbound_guard = self
                .outbound
                .read()
                .expect("connector registry outbound poisoned");
            let still_registered = outbound_guard.get(endpoint).is_some_and(|sessions| {
                sessions
                    .iter()
                    .any(|entry| entry.generation == outbound.generation)
            });
            if !still_registered {
                continue;
            }

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
            self.start_pending_attempt();

            let mut outbound_request = req.clone();
            outbound_request.stream_epoch = outbound.stream_epoch.clone();
            let msg = TunnelMessage {
                payload: Some(tunnel_message::Payload::Request(outbound_request)),
            };
            match outbound.tx.try_send(msg) {
                Ok(()) => {
                    self.advance_phase(&attempt_id, generation, PendingPhase::Sent);
                    drop(outbound_guard);
                    reservation.release();
                    return Ok(PendingRequest {
                        registry: self.clone(),
                        request_id,
                        attempt_id,
                        generation,
                        outbound_generation: outbound.generation,
                        endpoint: endpoint.to_string(),
                        stream_epoch: outbound.stream_epoch,
                        receiver: Some(done_rx),
                        accepted_rx,
                        _permit: Some(permit),
                        terminal: false,
                        revoke_on_drop_armed: true,
                    });
                }
                Err(error) => {
                    let reason = match error {
                        mpsc::error::TrySendError::Full(_) => "send_full",
                        mpsc::error::TrySendError::Closed(_) => "send_closed",
                    };
                    let removed = self.remove_pending_if_generation(&attempt_id, generation);
                    debug_assert!(removed, "failed send must still own its pending attempt");
                    self.finish_pending_attempt();
                    drop(outbound_guard);
                    before_revoke();
                    self.revoke_session(endpoint, outbound.generation, reason);
                    send_failed = true;
                }
            }
        }

        if send_failed {
            Err("send to connector failed")
        } else {
            Err("no healthy connector stream")
        }
    }

    fn reserve_attempt(&self, attempt_id: &str) -> Result<AttemptReservation, &'static str> {
        let generation = self.next_generation.fetch_add(1, Ordering::Relaxed);
        let mut dispatching = self
            .dispatching_attempts
            .lock()
            .expect("connector registry dispatch reservations poisoned");
        if dispatching.contains_key(attempt_id) {
            return Err("duplicate connector attempt_id");
        }
        if self
            .pending
            .lock()
            .expect("connector registry pending poisoned")
            .contains_key(attempt_id)
        {
            return Err("duplicate connector attempt_id");
        }
        dispatching.insert(attempt_id.to_string(), generation);
        Ok(AttemptReservation {
            registry: self.clone(),
            attempt_id: attempt_id.to_string(),
            generation,
            active: true,
        })
    }

    fn release_attempt_reservation(&self, attempt_id: &str, generation: u64) {
        let mut dispatching = self
            .dispatching_attempts
            .lock()
            .expect("connector registry dispatch reservations poisoned");
        if dispatching
            .get(attempt_id)
            .is_some_and(|current| *current == generation)
        {
            dispatching.remove(attempt_id);
        }
    }

    fn start_pending_attempt(&self) {
        self.pending_current.fetch_add(1, Ordering::AcqRel);
        metrics::gauge!("agent_pending_waiters")
            .set(self.pending_current.load(Ordering::Acquire) as f64);
    }

    fn finish_pending_attempt(&self) {
        let previous = self.pending_current.fetch_sub(1, Ordering::AcqRel);
        debug_assert!(previous > 0, "pending waiter gauge underflow");
        metrics::gauge!("agent_pending_waiters")
            .set(self.pending_current.load(Ordering::Acquire) as f64);
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
    fn probe_pending_len(&self) -> usize {
        self.probe_pending
            .lock()
            .expect("connector probe pending poisoned")
            .len()
    }

    #[cfg(test)]
    fn dispatching_attempt_len(&self) -> usize {
        self.dispatching_attempts
            .lock()
            .expect("connector registry dispatch reservations poisoned")
            .len()
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

fn unix_time_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_millis() as i64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Barrier;
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

    fn enabled_probe_policy(startup_grace: Duration) -> ProbePolicy {
        ProbePolicy {
            enabled: true,
            freshness: Duration::from_secs(10),
            startup_grace,
            failure_threshold: 3,
        }
    }

    fn health_probe_from(message: TunnelMessage) -> HealthProbe {
        match message.payload {
            Some(tunnel_message::Payload::HealthProbe(probe)) => probe,
            other => panic!("expected HealthProbe, got {other:?}"),
        }
    }

    async fn timeout_one_probe(
        registry: &ConnectorRegistry,
        rx: &mut mpsc::Receiver<TunnelMessage>,
    ) -> ProbeOutcome {
        let task_registry = registry.clone();
        let target = registry.probe_targets().pop().unwrap();
        let task = tokio::spawn(async move {
            task_registry
                .probe_session(target, Duration::from_millis(20))
                .await
        });
        let _probe = health_probe_from(rx.recv().await.unwrap());
        task.await.unwrap()
    }

    async fn complete_one_probe(
        registry: &ConnectorRegistry,
        rx: &mut mpsc::Receiver<TunnelMessage>,
    ) -> ProbeOutcome {
        let task_registry = registry.clone();
        let target = registry.probe_targets().pop().unwrap();
        let generation = target.generation;
        let task = tokio::spawn(async move {
            task_registry
                .probe_session(target, Duration::from_secs(1))
                .await
        });
        let probe = health_probe_from(rx.recv().await.unwrap());
        assert!(registry.resolve_probe_ack(
            generation,
            &probe.stream_epoch,
            HealthProbeAck {
                probe_id: probe.probe_id,
                stream_epoch: probe.stream_epoch.clone(),
                received_unix_ms: unix_time_ms(),
            }
        ));
        task.await.unwrap()
    }

    #[tokio::test]
    async fn probe_uses_stream_path_and_success_makes_exact_generation_fresh() {
        let registry = ConnectorRegistry::with_probe_policy(enabled_probe_policy(Duration::ZERO));
        let (generation, mut rx, _close_rx) =
            register_session(&registry, "connector:stream", "connector", 2);
        assert!(
            !registry.is_tunnel_healthy_with_window("connector:stream", Duration::from_secs(10))
        );

        let task_registry = registry.clone();
        let target = registry.probe_targets().pop().unwrap();
        let task = tokio::spawn(async move {
            task_registry
                .probe_session(target, Duration::from_secs(1))
                .await
        });
        let probe = health_probe_from(rx.recv().await.unwrap());
        assert_eq!(probe.stream_epoch, "epoch-connector");
        assert!(registry.resolve_probe_ack(
            generation,
            "epoch-connector",
            HealthProbeAck {
                probe_id: probe.probe_id,
                stream_epoch: probe.stream_epoch,
                received_unix_ms: unix_time_ms(),
            }
        ));
        assert_eq!(task.await.unwrap(), ProbeOutcome::Healthy);
        assert!(registry.is_tunnel_healthy_with_window("connector:stream", Duration::from_secs(10)));
        assert_eq!(registry.probe_pending_len(), 0);
        assert_eq!(registry.pending_current(), 0);
    }

    #[test]
    fn probe_startup_grace_keeps_new_session_eligible_until_first_round_trip() {
        let registry =
            ConnectorRegistry::with_probe_policy(enabled_probe_policy(Duration::from_secs(5)));
        let (_generation, _rx, _close_rx) =
            register_session(&registry, "connector:stream", "connector", 2);
        assert!(registry.is_tunnel_healthy_with_window("connector:stream", Duration::from_secs(10)));

        registry
            .outbound
            .write()
            .expect("connector registry outbound poisoned")
            .get_mut("connector:stream")
            .unwrap()[0]
            .registered_at = Instant::now() - Duration::from_secs(6);
        assert!(
            !registry.is_tunnel_healthy_with_window("connector:stream", Duration::from_secs(10))
        );
    }

    #[tokio::test]
    async fn transient_probe_timeout_does_not_revoke_generation() {
        let registry =
            ConnectorRegistry::with_probe_policy(enabled_probe_policy(Duration::from_secs(5)));
        let (_generation, mut rx, _close_rx) =
            register_session(&registry, "connector:stream", "connector", 2);
        assert_eq!(
            timeout_one_probe(&registry, &mut rx).await,
            ProbeOutcome::TimedOut
        );
        assert_eq!(registry.session_count("connector:stream"), 1);
        assert_eq!(registry.probe_pending_len(), 0);
        assert_eq!(registry.pending_current(), 0);
    }

    #[tokio::test]
    async fn probe_send_failure_immediately_revokes_exact_generation() {
        let registry =
            ConnectorRegistry::with_probe_policy(enabled_probe_policy(Duration::from_secs(5)));
        let (_generation, mut rx, mut close_rx) =
            register_session(&registry, "connector:stream", "connector", 1);
        registry
            .outbound
            .read()
            .expect("connector registry outbound poisoned")["connector:stream"][0]
            .tx
            .try_send(TunnelMessage { payload: None })
            .unwrap();

        let outcome = registry
            .probe_session(
                registry.probe_targets().pop().unwrap(),
                Duration::from_secs(1),
            )
            .await;
        assert_eq!(outcome, ProbeOutcome::Revoked);
        close_rx.changed().await.unwrap();
        assert!(*close_rx.borrow());
        assert_eq!(registry.session_count("connector:stream"), 0);
        assert!(rx.recv().await.is_some());
        assert_eq!(registry.probe_pending_len(), 0);
    }

    #[tokio::test]
    async fn consecutive_probe_timeout_threshold_revokes_without_business_pending_pollution() {
        let registry =
            ConnectorRegistry::with_probe_policy(enabled_probe_policy(Duration::from_secs(5)));
        let (_generation, mut rx, mut close_rx) =
            register_session(&registry, "connector:stream", "connector", 2);
        assert_eq!(
            timeout_one_probe(&registry, &mut rx).await,
            ProbeOutcome::TimedOut
        );
        assert_eq!(
            timeout_one_probe(&registry, &mut rx).await,
            ProbeOutcome::TimedOut
        );
        assert_eq!(
            timeout_one_probe(&registry, &mut rx).await,
            ProbeOutcome::Revoked
        );
        close_rx.changed().await.unwrap();
        assert!(*close_rx.borrow());
        assert_eq!(registry.session_count("connector:stream"), 0);
        assert_eq!(registry.probe_pending_len(), 0);
        assert_eq!(registry.pending_current(), 0);
    }

    #[tokio::test]
    async fn successful_probe_resets_consecutive_timeout_counter() {
        let mut policy = enabled_probe_policy(Duration::from_secs(5));
        policy.failure_threshold = 2;
        let registry = ConnectorRegistry::with_probe_policy(policy);
        let (_generation, mut rx, mut close_rx) =
            register_session(&registry, "connector:stream", "connector", 2);

        assert_eq!(
            timeout_one_probe(&registry, &mut rx).await,
            ProbeOutcome::TimedOut
        );
        assert_eq!(
            complete_one_probe(&registry, &mut rx).await,
            ProbeOutcome::Healthy
        );
        assert_eq!(
            timeout_one_probe(&registry, &mut rx).await,
            ProbeOutcome::TimedOut
        );
        assert_eq!(registry.session_count("connector:stream"), 1);
        assert_eq!(
            timeout_one_probe(&registry, &mut rx).await,
            ProbeOutcome::Revoked
        );
        close_rx.changed().await.unwrap();
        assert!(*close_rx.borrow());
    }

    #[tokio::test]
    async fn old_generation_probe_ack_cannot_refresh_replacement_session() {
        let registry = ConnectorRegistry::with_probe_policy(enabled_probe_policy(Duration::ZERO));
        let (old_generation, mut old_rx, _old_close_rx) =
            register_session(&registry, "connector:stream", "old", 2);
        let task_registry = registry.clone();
        let old_target = registry.probe_targets().pop().unwrap();
        let old_task = tokio::spawn(async move {
            task_registry
                .probe_session(old_target, Duration::from_secs(1))
                .await
        });
        let old_probe = health_probe_from(old_rx.recv().await.unwrap());
        registry.unregister("connector:stream", old_generation);
        assert_eq!(old_task.await.unwrap(), ProbeOutcome::SessionGone);

        let (_new_generation, _new_rx, _new_close_rx) =
            register_session(&registry, "connector:stream", "new", 2);
        assert!(!registry.resolve_probe_ack(
            old_generation,
            "epoch-old",
            HealthProbeAck {
                probe_id: old_probe.probe_id,
                stream_epoch: "epoch-old".into(),
                received_unix_ms: unix_time_ms(),
            }
        ));
        assert!(
            !registry.is_tunnel_healthy_with_window("connector:stream", Duration::from_secs(10))
        );
        assert_eq!(registry.probe_pending_len(), 0);
    }

    #[tokio::test]
    async fn duplicate_attempt_id_is_rejected_without_overwriting_waiter() {
        let registry = ConnectorRegistry::default();
        let (_session_generation, mut rx, mut close_rx) =
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
        close_rx.changed().await.unwrap();
        assert!(*close_rx.borrow());
        assert_eq!(registry.session_count("connector:stream"), 0);
        assert!(rx.recv().await.is_none());
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
        let (session_generation, mut rx, close_rx) =
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
        assert_eq!(registry.session_count("connector:stream"), 1);
        assert!(!*close_rx.borrow());
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

    #[tokio::test]
    async fn eligibility_requires_a_fresh_open_sender_with_available_capacity() {
        let registry = ConnectorRegistry::default();
        let (_generation, mut rx, _close_rx) =
            register_session(&registry, "connector:stream", "connector", 1);
        assert!(registry.is_tunnel_healthy_with_window("connector:stream", Duration::from_secs(10)));

        let tx = registry
            .outbound
            .read()
            .expect("connector registry outbound poisoned")["connector:stream"][0]
            .tx
            .clone();
        tx.try_send(TunnelMessage { payload: None }).unwrap();
        assert!(
            !registry.is_tunnel_healthy_with_window("connector:stream", Duration::from_secs(10))
        );

        assert!(rx.recv().await.is_some());
        assert!(registry.is_tunnel_healthy_with_window("connector:stream", Duration::from_secs(10)));

        drop(rx);
        assert!(
            !registry.is_tunnel_healthy_with_window("connector:stream", Duration::from_secs(10))
        );
    }

    #[tokio::test]
    async fn pending_request_can_revoke_only_its_assigned_session() {
        let registry = ConnectorRegistry::default();
        let (first_generation, mut first_rx, mut first_close) =
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

        assert!(first_pending.revoke_session("response_timeout"));
        first_close.changed().await.unwrap();
        assert!(*first_close.borrow());
        assert_eq!(registry.session_count("connector:stream"), 1);
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
        assert!(!registry.register_heartbeat(
            "connector:stream",
            "connector-1",
            first_generation,
            "epoch-connector-1"
        ));
    }

    #[tokio::test]
    async fn closed_selected_sender_is_revoked_and_same_attempt_uses_next_session() {
        let registry = ConnectorRegistry::default();
        let (_first_generation, first_rx, mut first_close) =
            register_session(&registry, "connector:stream", "connector-1", 1);
        let (second_generation, mut second_rx, _second_close) =
            register_session(&registry, "connector:stream", "connector-2", 1);
        let candidates =
            registry.select_eligible_sessions("connector:stream", Duration::from_secs(10));
        assert_eq!(candidates.len(), 2);
        drop(first_rx);

        let sem = Arc::new(Semaphore::new(1));
        let mut pending = registry
            .send_request_to_candidates(
                "connector:stream",
                request("logical", "same-attempt"),
                sem.clone().try_acquire_owned().unwrap(),
                candidates,
            )
            .unwrap();

        first_close.changed().await.unwrap();
        assert!(*first_close.borrow());
        assert_eq!(registry.session_count("connector:stream"), 1);
        assert!(second_rx.recv().await.is_some());
        assert_eq!(registry.pending_len(), 1);
        assert_eq!(registry.pending_current(), 1);
        assert_eq!(sem.available_permits(), 0);

        registry.resolve_response(
            second_generation,
            "epoch-connector-2",
            ForwardResponse {
                request_id: "logical".into(),
                attempt_id: "same-attempt".into(),
                status_code: 200,
                stream_epoch: "epoch-connector-2".into(),
                ..Default::default()
            },
        );
        assert_eq!(pending.recv().await.unwrap().status_code, 200);
        drop(pending);
        assert_eq!(registry.pending_len(), 0);
        assert_eq!(registry.pending_current(), 0);
        assert_eq!(sem.available_permits(), 1);
    }

    #[tokio::test]
    async fn full_selected_sender_is_revoked_and_retry_does_not_leak_pending_state() {
        let registry = ConnectorRegistry::default();
        let (_first_generation, mut first_rx, mut first_close) =
            register_session(&registry, "connector:stream", "connector-1", 1);
        let (_second_generation, mut second_rx, _second_close) =
            register_session(&registry, "connector:stream", "connector-2", 1);
        let candidates =
            registry.select_eligible_sessions("connector:stream", Duration::from_secs(10));
        assert_eq!(candidates.len(), 2);
        candidates[0]
            .tx
            .try_send(TunnelMessage { payload: None })
            .unwrap();

        let sem = Arc::new(Semaphore::new(1));
        let pending = registry
            .send_request_to_candidates(
                "connector:stream",
                request("logical", "same-attempt"),
                sem.clone().try_acquire_owned().unwrap(),
                candidates,
            )
            .unwrap();

        first_close.changed().await.unwrap();
        assert!(*first_close.borrow());
        assert_eq!(registry.session_count("connector:stream"), 1);
        assert!(first_rx.recv().await.is_some());
        assert!(second_rx.recv().await.is_some());
        assert_eq!(registry.pending_len(), 1);
        assert_eq!(registry.pending_current(), 1);
        assert_eq!(sem.available_permits(), 0);

        drop(pending);
        assert_eq!(registry.pending_len(), 0);
        assert_eq!(registry.pending_current(), 0);
        assert_eq!(sem.available_permits(), 1);
    }

    #[tokio::test]
    async fn all_selected_senders_can_fail_without_pending_or_permit_leaks() {
        let registry = ConnectorRegistry::default();
        let (_generation, rx, mut close_rx) =
            register_session(&registry, "connector:stream", "connector", 1);
        let candidates =
            registry.select_eligible_sessions("connector:stream", Duration::from_secs(10));
        assert_eq!(candidates.len(), 1);
        drop(rx);

        let sem = Arc::new(Semaphore::new(1));
        let result = registry.send_request_to_candidates(
            "connector:stream",
            request("logical", "same-attempt"),
            sem.clone().try_acquire_owned().unwrap(),
            candidates,
        );
        assert!(matches!(result, Err("send to connector failed")));

        close_rx.changed().await.unwrap();
        assert!(*close_rx.borrow());
        assert_eq!(registry.session_count("connector:stream"), 0);
        assert_eq!(registry.pending_len(), 0);
        assert_eq!(registry.pending_current(), 0);
        assert_eq!(sem.available_permits(), 1);
    }

    #[tokio::test]
    async fn dropping_an_enqueued_waiter_revokes_its_session_and_wakes_peer_waiters() {
        let registry = ConnectorRegistry::default();
        let (_generation, mut rx, mut close_rx) =
            register_session(&registry, "connector:stream", "connector", 4);
        let sem = Arc::new(Semaphore::new(2));
        let first = registry
            .send_request_to_connector(
                "connector:stream",
                request("logical-1", "attempt-1"),
                sem.clone().acquire_owned().await.unwrap(),
                Duration::from_secs(10),
            )
            .await
            .unwrap();
        let mut second = registry
            .send_request_to_connector(
                "connector:stream",
                request("logical-2", "attempt-2"),
                sem.clone().acquire_owned().await.unwrap(),
                Duration::from_secs(10),
            )
            .await
            .unwrap();
        assert!(rx.recv().await.is_some());
        assert!(rx.recv().await.is_some());

        drop(first);
        close_rx.changed().await.unwrap();
        assert!(*close_rx.borrow());
        assert_eq!(registry.session_count("connector:stream"), 0);
        assert_eq!(
            second.recv().await,
            Err(PendingFailure::StreamLost {
                phase: PendingPhase::Sent,
                stream_epoch: "epoch-connector".into(),
            })
        );
        drop(second);
        assert_eq!(registry.pending_len(), 0);
        assert_eq!(registry.pending_current(), 0);
        assert_eq!(sem.available_permits(), 2);
    }

    #[test]
    fn attempt_reservation_spans_the_full_send_retry_gap() {
        let registry = ConnectorRegistry::default();
        let (_first_generation, mut first_rx, _first_close) =
            register_session(&registry, "connector:stream", "connector-1", 1);
        let (_second_generation, mut second_rx, _second_close) =
            register_session(&registry, "connector:stream", "connector-2", 1);
        let candidates =
            registry.select_eligible_sessions("connector:stream", Duration::from_secs(10));
        candidates[0]
            .tx
            .try_send(TunnelMessage { payload: None })
            .unwrap();

        let entered_retry_gap = Arc::new(Barrier::new(2));
        let release_retry = Arc::new(Barrier::new(2));
        let sem = Arc::new(Semaphore::new(2));
        let task_registry = registry.clone();
        let task_entered = entered_retry_gap.clone();
        let task_release = release_retry.clone();
        let task_sem = sem.clone();
        let task = std::thread::spawn(move || {
            task_registry.send_request_to_candidates_with_retry_hook(
                "connector:stream",
                request("logical", "same-attempt"),
                task_sem.try_acquire_owned().unwrap(),
                candidates,
                move || {
                    task_entered.wait();
                    task_release.wait();
                },
            )
        });

        entered_retry_gap.wait();
        assert_eq!(registry.dispatching_attempt_len(), 1);
        let duplicate_candidates =
            registry.select_eligible_sessions("connector:stream", Duration::from_secs(10));
        let duplicate = registry.send_request_to_candidates(
            "connector:stream",
            request("logical", "same-attempt"),
            sem.clone().try_acquire_owned().unwrap(),
            duplicate_candidates,
        );
        assert!(matches!(duplicate, Err("duplicate connector attempt_id")));

        release_retry.wait();
        let pending = task.join().unwrap().unwrap();
        assert!(first_rx.try_recv().is_ok());
        assert!(second_rx.try_recv().is_ok());
        assert_eq!(registry.dispatching_attempt_len(), 0);
        drop(pending);
        assert_eq!(registry.pending_len(), 0);
        assert_eq!(registry.pending_current(), 0);
        assert_eq!(sem.available_permits(), 2);
    }
}
