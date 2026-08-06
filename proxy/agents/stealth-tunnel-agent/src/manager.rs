use std::collections::HashMap;
use std::error::Error;
use std::sync::atomic::{AtomicBool, AtomicI64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use bytes::BytesMut;
use futures::StreamExt;
use reqwest::header::HeaderMap;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tokio::sync::RwLock;
use tracing::{info, warn};

const CONFIG_GENERATION_HEADER: &str = "x-sag-config-generation";

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct TunnelRouteRecord {
    pub host: String,
    pub app_id: String,
    pub connector_endpoint: String,
    pub require_healthy_tunnel: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
#[allow(dead_code)]
pub struct RouteInfo {
    pub host: String,
    pub app_id: String,
    pub connector_endpoint: String,
    pub require_healthy_tunnel: bool,
}

#[derive(Default)]
struct AppliedRouteState {
    routes_by_app: HashMap<String, RouteInfo>,
    applied_generation: Option<u64>,
    applied_at_ms: Option<i64>,
    snapshot_hash: Option<String>,
    snapshot_loaded: bool,
    snapshot_durable: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum SnapshotApplyOutcome {
    Applied {
        generation: u64,
        applied_at_ms: i64,
    },
    AlreadyCurrent {
        generation: u64,
        applied_at_ms: i64,
    },
    RejectedOlder {
        received_generation: u64,
        current_generation: u64,
    },
    RejectedConflictingApp {
        received_generation: u64,
        app_id: String,
    },
    RejectedSameGenerationConflict {
        generation: u64,
    },
}

impl SnapshotApplyOutcome {
    fn ack(&self, agent_id: &str, snapshot_hash: &str) -> Option<AgentConfigAck> {
        let (applied_generation, applied_at_ms) = match self {
            Self::Applied {
                generation,
                applied_at_ms,
            }
            | Self::AlreadyCurrent {
                generation,
                applied_at_ms,
            } => (*generation, *applied_at_ms),
            Self::RejectedOlder { .. }
            | Self::RejectedConflictingApp { .. }
            | Self::RejectedSameGenerationConflict { .. } => return None,
        };
        Some(AgentConfigAck {
            agent_id: agent_id.to_string(),
            applied_generation,
            applied_at_ms,
            snapshot_hash: snapshot_hash.to_string(),
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
struct AgentConfigAck {
    agent_id: String,
    applied_generation: u64,
    applied_at_ms: i64,
    snapshot_hash: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct FetchedRouteSnapshot {
    url: String,
    generation: u64,
    routes: Vec<TunnelRouteRecord>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum SnapshotSelectionError {
    NoSnapshots,
    SameGenerationConflict {
        generation: u64,
        first_url: String,
        second_url: String,
    },
}

impl std::fmt::Display for SnapshotSelectionError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NoSnapshots => formatter.write_str("no valid route snapshot was returned"),
            Self::SameGenerationConflict {
                generation,
                first_url,
                second_url,
            } => write!(
                formatter,
                "generation {generation} has divergent content at {first_url} and {second_url}"
            ),
        }
    }
}

#[derive(Clone, Default)]
pub struct TunnelManager {
    applied_routes: Arc<RwLock<AppliedRouteState>>,
    initial_sync_succeeded: Arc<AtomicBool>,
    last_sync_ms: Arc<AtomicI64>,
}

impl TunnelManager {
    pub fn new() -> Self {
        Self {
            applied_routes: Arc::new(RwLock::new(AppliedRouteState::default())),
            initial_sync_succeeded: Arc::new(AtomicBool::new(false)),
            last_sync_ms: Arc::new(AtomicI64::new(now_ms())),
        }
    }

    async fn stage_route_snapshot(
        &self,
        routes: Vec<TunnelRouteRecord>,
        generation: u64,
    ) -> SnapshotApplyOutcome {
        self.apply_route_snapshot_at_with_durability(routes, generation, now_ms(), false)
            .await
    }

    #[cfg(test)]
    async fn apply_route_snapshot_at(
        &self,
        routes: Vec<TunnelRouteRecord>,
        generation: u64,
        applied_at_ms: i64,
    ) -> SnapshotApplyOutcome {
        self.apply_route_snapshot_at_with_durability(routes, generation, applied_at_ms, true)
            .await
    }

    async fn apply_route_snapshot_at_with_durability(
        &self,
        mut routes: Vec<TunnelRouteRecord>,
        generation: u64,
        applied_at_ms: i64,
        durable_on_new_generation: bool,
    ) -> SnapshotApplyOutcome {
        canonicalize_routes(&mut routes);
        let snapshot_hash = canonical_snapshot_hash(&routes);
        // Build the full replacement before taking the write lock. Readers can
        // therefore never observe a partially constructed route map.
        let mut replacement: HashMap<String, RouteInfo> = HashMap::with_capacity(routes.len());
        for r in routes {
            if let Some(existing) = replacement.get_mut(&r.app_id) {
                if existing.connector_endpoint != r.connector_endpoint
                    || existing.require_healthy_tunnel != r.require_healthy_tunnel
                {
                    return SnapshotApplyOutcome::RejectedConflictingApp {
                        received_generation: generation,
                        app_id: r.app_id,
                    };
                }
                // Multiple hosts may intentionally point at one app. The
                // routing decision is app-scoped, so retain a deterministic
                // representative host rather than depending on row order.
                if r.host < existing.host {
                    existing.host = r.host;
                }
                continue;
            }
            replacement.insert(
                r.app_id.clone(),
                RouteInfo {
                    host: r.host,
                    app_id: r.app_id,
                    connector_endpoint: r.connector_endpoint,
                    require_healthy_tunnel: r.require_healthy_tunnel,
                },
            );
        }

        let (outcome, publishable) = {
            let mut state = self.applied_routes.write().await;
            let outcome = match state.applied_generation {
                Some(current_generation) if generation < current_generation => {
                    SnapshotApplyOutcome::RejectedOlder {
                        received_generation: generation,
                        current_generation,
                    }
                }
                Some(current_generation)
                    if generation == current_generation
                        && state.snapshot_hash.as_deref() != Some(snapshot_hash.as_str()) =>
                {
                    SnapshotApplyOutcome::RejectedSameGenerationConflict {
                        generation: current_generation,
                    }
                }
                Some(current_generation)
                    if generation == current_generation
                        && state.snapshot_loaded
                        && state.routes_by_app == replacement =>
                {
                    SnapshotApplyOutcome::AlreadyCurrent {
                        generation: current_generation,
                        applied_at_ms: state.applied_at_ms.unwrap_or(applied_at_ms),
                    }
                }
                Some(current_generation)
                    if generation == current_generation && !state.snapshot_loaded =>
                {
                    state.routes_by_app = replacement;
                    state.applied_at_ms = Some(applied_at_ms);
                    state.snapshot_hash = Some(snapshot_hash);
                    state.snapshot_loaded = true;
                    SnapshotApplyOutcome::Applied {
                        generation: current_generation,
                        applied_at_ms,
                    }
                }
                Some(current_generation) if generation == current_generation => {
                    SnapshotApplyOutcome::RejectedSameGenerationConflict {
                        generation: current_generation,
                    }
                }
                _ => {
                    // Publish the replacement before its generation while both
                    // values are protected by the same lock.
                    state.routes_by_app = replacement;
                    state.applied_at_ms = Some(applied_at_ms);
                    state.applied_generation = Some(generation);
                    state.snapshot_hash = Some(snapshot_hash);
                    state.snapshot_loaded = true;
                    state.snapshot_durable = durable_on_new_generation;
                    SnapshotApplyOutcome::Applied {
                        generation,
                        applied_at_ms,
                    }
                }
            };
            let publishable = state.snapshot_loaded && state.snapshot_durable;
            (outcome, publishable)
        };

        if !matches!(
            &outcome,
            SnapshotApplyOutcome::RejectedOlder { .. }
                | SnapshotApplyOutcome::RejectedConflictingApp { .. }
                | SnapshotApplyOutcome::RejectedSameGenerationConflict { .. }
        ) {
            self.initial_sync_succeeded
                .store(publishable, Ordering::Release);
            if publishable {
                self.last_sync_ms.store(now_ms(), Ordering::Release);
            }
        }
        outcome
    }

    /// Publish a staged snapshot only after its ACK has durably committed at a
    /// control-plane endpoint. Until this succeeds, route resolution and
    /// readiness remain fail-closed.
    async fn publish_durable_snapshot(&self, generation: u64, snapshot_hash: &str) -> bool {
        let published = {
            let mut state = self.applied_routes.write().await;
            if state.applied_generation != Some(generation)
                || state.snapshot_hash.as_deref() != Some(snapshot_hash)
                || !state.snapshot_loaded
            {
                false
            } else {
                state.snapshot_durable = true;
                true
            }
        };
        if published {
            self.initial_sync_succeeded.store(true, Ordering::Release);
            self.last_sync_ms.store(now_ms(), Ordering::Release);
        }
        published
    }

    async fn snapshot_is_durable(&self, generation: u64, snapshot_hash: &str) -> bool {
        let state = self.applied_routes.read().await;
        state.applied_generation == Some(generation)
            && state.snapshot_hash.as_deref() == Some(snapshot_hash)
            && state.snapshot_loaded
            && state.snapshot_durable
    }

    /// Restore only the monotonic generation fence recorded by the previous
    /// process. Readiness remains false and no route is served until a snapshot
    /// with the same fingerprint (or a newer generation) has been fetched and
    /// installed in this process.
    pub async fn restore_generation_floor(
        &self,
        generation: u64,
        snapshot_hash: String,
        applied_at_ms: i64,
    ) -> Result<(), String> {
        if !is_snapshot_hash(&snapshot_hash) {
            return Err("persisted route snapshot hash must be 64 lowercase hex characters".into());
        }
        let mut state = self.applied_routes.write().await;
        if state.applied_generation.is_some() || state.snapshot_loaded {
            return Err("route generation floor can only be restored before initial sync".into());
        }
        state.applied_generation = Some(generation);
        state.applied_at_ms = Some(applied_at_ms);
        state.snapshot_hash = Some(snapshot_hash);
        state.snapshot_durable = true;
        Ok(())
    }

    /// Exposed for readiness/metrics integration performed by the surrounding
    /// Task 7 slice; manager tests also use it to assert monotonic publication.
    #[allow(dead_code)]
    pub async fn applied_generation(&self) -> Option<u64> {
        self.applied_routes.read().await.applied_generation
    }

    pub async fn resolve_route_by_app_id(&self, app_id: &str) -> Option<RouteInfo> {
        let state = self.applied_routes.read().await;
        if !state.snapshot_loaded || !state.snapshot_durable {
            return None;
        }
        state.routes_by_app.get(app_id).cloned()
    }

    pub fn initial_sync_succeeded(&self) -> bool {
        self.initial_sync_succeeded.load(Ordering::Acquire)
    }

    pub fn route_sync_age_seconds(&self) -> f64 {
        now_ms().saturating_sub(self.last_sync_ms.load(Ordering::Acquire)) as f64 / 1_000.0
    }
}

fn canonicalize_routes(routes: &mut [TunnelRouteRecord]) {
    routes.sort_by(|left, right| {
        (
            &left.app_id,
            &left.host,
            &left.connector_endpoint,
            left.require_healthy_tunnel,
        )
            .cmp(&(
                &right.app_id,
                &right.host,
                &right.connector_endpoint,
                right.require_healthy_tunnel,
            ))
    });
}

fn canonical_snapshot_hash(routes: &[TunnelRouteRecord]) -> String {
    let mut hasher = Sha256::new();
    hasher.update((routes.len() as u64).to_be_bytes());
    for route in routes {
        for value in [&route.app_id, &route.host, &route.connector_endpoint] {
            hasher.update((value.len() as u64).to_be_bytes());
            hasher.update(value.as_bytes());
        }
        hasher.update([u8::from(route.require_healthy_tunnel)]);
    }
    hex::encode(hasher.finalize())
}

fn is_snapshot_hash(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_millis() as i64)
        .unwrap_or(0)
}

fn format_reqwest_err(e: &reqwest::Error) -> String {
    let mut detail = e.to_string();
    let mut cur = e.source();
    while let Some(c) = cur {
        detail.push_str(" — ");
        detail.push_str(&c.to_string());
        cur = c.source();
    }
    detail
}

fn config_generation(headers: &HeaderMap) -> Result<u64, String> {
    let raw = headers
        .get(CONFIG_GENERATION_HEADER)
        .ok_or_else(|| format!("missing {CONFIG_GENERATION_HEADER} response header"))?;
    let raw = raw
        .to_str()
        .map_err(|_| format!("invalid {CONFIG_GENERATION_HEADER} response header"))?;
    let generation = raw
        .parse::<u64>()
        .map_err(|_| format!("invalid {CONFIG_GENERATION_HEADER} value: {raw}"))?;
    if generation > i64::MAX as u64 {
        return Err(format!(
            "{CONFIG_GENERATION_HEADER} exceeds the persistent BIGINT range: {raw}"
        ));
    }
    Ok(generation)
}

fn ack_url(routes_url: &str) -> Result<String, String> {
    let mut url = reqwest::Url::parse(routes_url)
        .map_err(|error| format!("invalid route sync URL {routes_url}: {error}"))?;
    let path = url.path().trim_end_matches('/');
    url.set_path(&format!("{path}/ack"));
    url.set_query(None);
    url.set_fragment(None);
    Ok(url.to_string())
}

pub fn agent_instance_id() -> String {
    ["SAG_AGENT_INSTANCE_ID", "HOSTNAME", "COMPUTERNAME"]
        .into_iter()
        .find_map(|key| {
            std::env::var(key)
                .ok()
                .map(|value| value.trim().to_string())
                .filter(|value| !value.is_empty())
        })
        .unwrap_or_else(|| "stealth-tunnel-agent".to_string())
}

async fn post_apply_ack(
    client: &reqwest::Client,
    routes_url: &str,
    sync_token: Option<&str>,
    ack: &AgentConfigAck,
) -> Result<(), String> {
    let target = ack_url(routes_url)?;
    let mut request = client.post(&target).json(ack);
    if let Some(token) = sync_token {
        request = request.header("x-sag-agent-token", token);
    }
    let response = request.send().await.map_err(|error| {
        format!(
            "config apply ACK request failed: {}",
            format_reqwest_err(&error)
        )
    })?;
    if response.status().is_success() {
        Ok(())
    } else {
        Err(format!(
            "config apply ACK failed: HTTP {}",
            response.status()
        ))
    }
}

async fn fetch_route_snapshot(
    client: &reqwest::Client,
    url: &str,
    sync_token: Option<&str>,
    max_body_bytes: usize,
    max_routes: usize,
) -> Result<FetchedRouteSnapshot, String> {
    let mut request = client.get(url);
    if let Some(token) = sync_token {
        request = request.header("x-sag-agent-token", token);
    }
    let response = request
        .send()
        .await
        .map_err(|error| format_reqwest_err(&error))?;
    if !response.status().is_success() {
        return Err(format!("HTTP {}", response.status()));
    }
    let generation = config_generation(response.headers())?;
    if response
        .content_length()
        .is_some_and(|length| length > max_body_bytes as u64)
    {
        return Err(format!(
            "route snapshot body exceeds {max_body_bytes} bytes"
        ));
    }
    let mut body = BytesMut::with_capacity(
        response
            .content_length()
            .unwrap_or(0)
            .min(max_body_bytes as u64) as usize,
    );
    let mut chunks = response.bytes_stream();
    while let Some(chunk) = chunks.next().await {
        let chunk = chunk.map_err(|error| format!("route snapshot body read failed: {error}"))?;
        if body.len().saturating_add(chunk.len()) > max_body_bytes {
            return Err(format!(
                "route snapshot body exceeds {max_body_bytes} bytes"
            ));
        }
        body.extend_from_slice(&chunk);
    }
    let routes = serde_json::from_slice::<Vec<TunnelRouteRecord>>(&body)
        .map_err(|error| format!("response body is not valid json: {error}"))?;
    if routes.len() > max_routes {
        return Err(format!(
            "route snapshot contains {} routes; maximum is {max_routes}",
            routes.len()
        ));
    }
    Ok(FetchedRouteSnapshot {
        url: url.to_string(),
        generation,
        routes,
    })
}

fn select_freshest_snapshot(
    mut snapshots: Vec<FetchedRouteSnapshot>,
) -> Result<FetchedRouteSnapshot, SnapshotSelectionError> {
    if snapshots.is_empty() {
        return Err(SnapshotSelectionError::NoSnapshots);
    }
    for snapshot in &mut snapshots {
        canonicalize_routes(&mut snapshot.routes);
    }
    let highest_generation = snapshots
        .iter()
        .map(|snapshot| snapshot.generation)
        .max()
        .expect("non-empty snapshot list has a maximum generation");
    let mut freshest = snapshots
        .into_iter()
        .filter(|snapshot| snapshot.generation == highest_generation);
    let selected = freshest
        .next()
        .expect("the maximum generation belongs to at least one snapshot");
    for peer in freshest {
        if peer.routes != selected.routes {
            return Err(SnapshotSelectionError::SameGenerationConflict {
                generation: highest_generation,
                first_url: selected.url,
                second_url: peer.url,
            });
        }
    }
    Ok(selected)
}

pub async fn sync_routes_loop(
    endpoints: Vec<String>,
    manager: TunnelManager,
    interval: Duration,
    sync_token: Option<String>,
) {
    let agent_id = agent_instance_id();
    let request_timeout = Duration::from_millis(
        std::env::var("SAG_CONTROL_PLANE_SYNC_TIMEOUT_MS")
            .ok()
            .and_then(|value| value.parse::<u64>().ok())
            .unwrap_or(2_000)
            .max(100),
    );
    let max_snapshot_body_bytes = std::env::var("SAG_CONTROL_PLANE_SYNC_MAX_BODY_BYTES")
        .ok()
        .and_then(|value| value.parse::<usize>().ok())
        .unwrap_or(4 * 1024 * 1024)
        .clamp(1_024, 64 * 1024 * 1024);
    let max_snapshot_routes = std::env::var("SAG_CONTROL_PLANE_SYNC_MAX_ROUTES")
        .ok()
        .and_then(|value| value.parse::<usize>().ok())
        .unwrap_or(10_000)
        .clamp(1, 1_000_000);
    let client = reqwest::Client::builder()
        .timeout(request_timeout)
        .build()
        .expect("reqwest client");
    if endpoints.is_empty() {
        warn!("sync routes: no control-plane endpoints configured");
    }
    loop {
        if endpoints.is_empty() {
            tokio::time::sleep(interval).await;
            continue;
        }

        let mut synced = false;
        let mut last_fail: Option<(String, String)> = None;

        // Fetch concurrently so an unavailable fallback does not add its full
        // timeout to every round. Apply only the highest generation returned by
        // any reachable endpoint; equal generations must have identical
        // canonical content or the round fails closed.
        let fetches = endpoints.iter().map(|url| {
            fetch_route_snapshot(
                &client,
                url,
                sync_token.as_deref(),
                max_snapshot_body_bytes,
                max_snapshot_routes,
            )
        });
        let mut snapshots = Vec::new();
        for (url, result) in endpoints
            .iter()
            .zip(futures::future::join_all(fetches).await)
        {
            match result {
                Ok(snapshot) => snapshots.push(snapshot),
                Err(detail) => {
                    last_fail = Some((url.clone(), detail.clone()));
                    warn!(url = %url, detail = %detail, "sync routes endpoint failed");
                }
            }
        }

        let selected = match select_freshest_snapshot(snapshots) {
            Ok(snapshot) => Some(snapshot),
            Err(SnapshotSelectionError::NoSnapshots) => None,
            Err(error @ SnapshotSelectionError::SameGenerationConflict { .. }) => {
                metrics::counter!(
                    "agent_route_snapshot_rejected_total",
                    "reason" => "multi_endpoint_same_generation_conflict"
                )
                .increment(1);
                last_fail = Some(("multiple endpoints".into(), error.to_string()));
                warn!(%error, "sync routes: control-plane endpoints disagree at one generation");
                None
            }
        };

        if let Some(selected) = selected {
            let url = selected.url;
            let generation = selected.generation;
            let count = selected.routes.len();
            let snapshot_hash = canonical_snapshot_hash(&selected.routes);
            let outcome = manager
                .stage_route_snapshot(selected.routes, generation)
                .await;
            match &outcome {
                SnapshotApplyOutcome::RejectedOlder {
                    received_generation,
                    current_generation,
                } => {
                    let detail = format!(
                        "stale generation {received_generation}; current generation is {current_generation}"
                    );
                    last_fail = Some((url.clone(), detail));
                    warn!(
                        url = %url,
                        received_generation,
                        current_generation,
                        "sync routes: rejected older snapshot"
                    );
                }
                SnapshotApplyOutcome::RejectedConflictingApp {
                    received_generation,
                    app_id,
                } => {
                    last_fail = Some((
                        url.clone(),
                        format!(
                            "generation {received_generation} has conflicting routes for app {app_id}"
                        ),
                    ));
                    metrics::counter!(
                        "agent_route_snapshot_rejected_total",
                        "reason" => "duplicate_app_conflict"
                    )
                    .increment(1);
                    warn!(
                        url = %url,
                        received_generation,
                        %app_id,
                        "sync routes: rejected conflicting duplicate app configuration"
                    );
                }
                SnapshotApplyOutcome::RejectedSameGenerationConflict { generation } => {
                    last_fail = Some((
                        url.clone(),
                        format!(
                            "generation {generation} content differs from the already applied snapshot"
                        ),
                    ));
                    metrics::counter!(
                        "agent_route_snapshot_rejected_total",
                        "reason" => "same_generation_conflict"
                    )
                    .increment(1);
                    warn!(
                        url = %url,
                        generation,
                        "sync routes: rejected divergent content for the current generation"
                    );
                }
                SnapshotApplyOutcome::Applied { .. }
                | SnapshotApplyOutcome::AlreadyCurrent { .. } => {
                    let ack = outcome
                        .ack(&agent_id, &snapshot_hash)
                        .expect("an applied snapshot must produce an ACK");
                    info!(
                        url = %url,
                        count,
                        applied_generation = ack.applied_generation,
                        "sync routes staged from freshest reachable endpoint"
                    );
                    let mut acked = false;
                    let mut ack_error = None;
                    for ack_target in std::iter::once(&url)
                        .chain(endpoints.iter().filter(|candidate| *candidate != &url))
                    {
                        match post_apply_ack(&client, ack_target, sync_token.as_deref(), &ack).await
                        {
                            Ok(()) => {
                                acked = true;
                                break;
                            }
                            Err(error) => ack_error = Some((ack_target.clone(), error)),
                        }
                    }
                    if !acked {
                        // The map remains staged and unservable. Polling the
                        // same generation retries this ACK without rebuilding
                        // or exposing an uncommitted configuration.
                        let (ack_target, error) = ack_error
                            .unwrap_or_else(|| (url.clone(), "no ACK endpoint available".into()));
                        warn!(
                            url = %ack_target,
                            applied_generation = ack.applied_generation,
                            %error,
                            "sync routes: apply ACK failed at every control-plane endpoint; will retry"
                        );
                    }
                    let durable = if acked {
                        manager
                            .publish_durable_snapshot(generation, &snapshot_hash)
                            .await
                    } else {
                        manager
                            .snapshot_is_durable(generation, &snapshot_hash)
                            .await
                    };
                    if acked && !durable {
                        warn!(
                            generation,
                            "sync routes: ACK committed but staged snapshot changed before publication"
                        );
                    }
                    synced = durable;
                }
            }
        }

        if !synced {
            if let Some((url, msg)) = last_fail {
                warn!(tried = ?endpoints, last_url = %url, detail = %msg, "sync routes: all endpoints failed this round");
            }
        }
        metrics::gauge!("agent_route_sync_age_seconds").set(manager.route_sync_age_seconds());

        tokio::time::sleep(interval).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn route(app_id: &str, connector_endpoint: &str) -> TunnelRouteRecord {
        TunnelRouteRecord {
            host: format!("{app_id}.internal"),
            app_id: app_id.to_string(),
            connector_endpoint: connector_endpoint.to_string(),
            require_healthy_tunnel: true,
        }
    }

    fn route_snapshot_hash(mut routes: Vec<TunnelRouteRecord>) -> String {
        canonicalize_routes(&mut routes);
        canonical_snapshot_hash(&routes)
    }

    #[tokio::test]
    async fn empty_generation_zero_snapshot_is_a_successful_initial_sync() {
        let manager = TunnelManager::new();
        assert!(!manager.initial_sync_succeeded());

        assert_eq!(
            manager.apply_route_snapshot_at(Vec::new(), 0, 100).await,
            SnapshotApplyOutcome::Applied {
                generation: 0,
                applied_at_ms: 100,
            }
        );

        assert!(manager.initial_sync_succeeded());
        assert_eq!(manager.applied_generation().await, Some(0));
        assert!(manager.resolve_route_by_app_id("missing").await.is_none());
    }

    #[tokio::test]
    async fn older_generation_cannot_roll_back_routes() {
        let manager = TunnelManager::new();
        manager
            .apply_route_snapshot_at(vec![route("current", "connector-new")], 9, 100)
            .await;

        assert_eq!(
            manager
                .apply_route_snapshot_at(vec![route("stale", "connector-old")], 8, 200)
                .await,
            SnapshotApplyOutcome::RejectedOlder {
                received_generation: 8,
                current_generation: 9,
            }
        );

        assert_eq!(manager.applied_generation().await, Some(9));
        assert_eq!(
            manager
                .resolve_route_by_app_id("current")
                .await
                .unwrap()
                .connector_endpoint,
            "connector-new"
        );
        assert!(manager.resolve_route_by_app_id("stale").await.is_none());
    }

    #[tokio::test]
    async fn replacement_map_is_visible_with_its_published_generation() {
        let manager = TunnelManager::new();
        manager
            .apply_route_snapshot_at(vec![route("app-a", "connector-a")], 3, 100)
            .await;
        manager
            .apply_route_snapshot_at(vec![route("app-b", "connector-b")], 4, 200)
            .await;

        let state = manager.applied_routes.read().await;
        assert_eq!(state.applied_generation, Some(4));
        assert_eq!(state.applied_at_ms, Some(200));
        assert!(!state.routes_by_app.contains_key("app-a"));
        assert_eq!(
            state.routes_by_app["app-b"].connector_endpoint,
            "connector-b"
        );
    }

    #[tokio::test]
    async fn current_generation_retries_ack_without_reapplying() {
        let manager = TunnelManager::new();
        manager
            .apply_route_snapshot_at(vec![route("current", "connector-a")], 5, 100)
            .await;

        let outcome = manager
            .apply_route_snapshot_at(vec![route("current", "connector-a")], 5, 200)
            .await;
        let snapshot_hash = route_snapshot_hash(vec![route("current", "connector-a")]);
        assert_eq!(
            outcome,
            SnapshotApplyOutcome::AlreadyCurrent {
                generation: 5,
                applied_at_ms: 100,
            }
        );
        assert_eq!(
            outcome.ack("agent-a", &snapshot_hash),
            Some(AgentConfigAck {
                agent_id: "agent-a".into(),
                applied_generation: 5,
                applied_at_ms: 100,
                snapshot_hash,
            })
        );
        assert_eq!(
            manager
                .resolve_route_by_app_id("current")
                .await
                .unwrap()
                .connector_endpoint,
            "connector-a"
        );
    }

    #[tokio::test]
    async fn staged_snapshot_is_not_served_until_its_ack_is_durable() {
        let manager = TunnelManager::new();
        let routes = vec![route("pending", "connector-new")];
        let snapshot_hash = route_snapshot_hash(routes.clone());

        assert!(matches!(
            manager.stage_route_snapshot(routes, 6).await,
            SnapshotApplyOutcome::Applied { generation: 6, .. }
        ));
        assert!(!manager.initial_sync_succeeded());
        assert!(manager.resolve_route_by_app_id("pending").await.is_none());
        assert!(!manager.publish_durable_snapshot(6, &"0".repeat(64)).await);

        assert!(manager.publish_durable_snapshot(6, &snapshot_hash).await);
        assert!(manager.initial_sync_succeeded());
        assert_eq!(
            manager
                .resolve_route_by_app_id("pending")
                .await
                .unwrap()
                .connector_endpoint,
            "connector-new"
        );
    }

    #[tokio::test]
    async fn same_generation_with_different_content_is_rejected_without_ack() {
        let manager = TunnelManager::new();
        manager
            .apply_route_snapshot_at(vec![route("current", "connector-a")], 5, 100)
            .await;

        let outcome = manager
            .apply_route_snapshot_at(vec![route("current", "connector-b")], 5, 200)
            .await;
        assert_eq!(
            outcome,
            SnapshotApplyOutcome::RejectedSameGenerationConflict { generation: 5 }
        );
        assert!(outcome.ack("agent-a", &"0".repeat(64)).is_none());
        assert_eq!(manager.applied_generation().await, Some(5));
        assert_eq!(
            manager
                .resolve_route_by_app_id("current")
                .await
                .unwrap()
                .connector_endpoint,
            "connector-a"
        );
    }

    #[tokio::test]
    async fn conflicting_duplicate_app_is_rejected_without_swap_or_ack() {
        let manager = TunnelManager::new();
        manager
            .apply_route_snapshot_at(vec![route("current", "connector-a")], 5, 100)
            .await;
        let mut conflicting = route("duplicate", "connector-a");
        conflicting.host = "second.internal".into();
        conflicting.connector_endpoint = "connector-b".into();

        let outcome = manager
            .apply_route_snapshot_at(vec![route("duplicate", "connector-a"), conflicting], 6, 200)
            .await;
        assert_eq!(
            outcome,
            SnapshotApplyOutcome::RejectedConflictingApp {
                received_generation: 6,
                app_id: "duplicate".into(),
            }
        );
        assert!(outcome.ack("agent-a", &"0".repeat(64)).is_none());
        assert_eq!(manager.applied_generation().await, Some(5));
        assert!(manager.resolve_route_by_app_id("duplicate").await.is_none());
        assert_eq!(
            manager
                .resolve_route_by_app_id("current")
                .await
                .unwrap()
                .connector_endpoint,
            "connector-a"
        );
    }

    #[tokio::test]
    async fn consistent_duplicate_app_hosts_use_a_deterministic_representative() {
        let manager = TunnelManager::new();
        let mut second = route("shared", "connector-a");
        second.host = "a.internal".into();
        let mut first = route("shared", "connector-a");
        first.host = "z.internal".into();

        let outcome = manager
            .apply_route_snapshot_at(vec![first, second], 1, 100)
            .await;
        assert!(matches!(outcome, SnapshotApplyOutcome::Applied { .. }));
        assert_eq!(
            manager
                .resolve_route_by_app_id("shared")
                .await
                .unwrap()
                .host,
            "a.internal"
        );
    }

    #[test]
    fn ack_url_is_derived_from_routes_url() {
        assert_eq!(
            ack_url("http://control-plane:8090/api/v1/agent/routes").unwrap(),
            "http://control-plane:8090/api/v1/agent/routes/ack"
        );
        assert_eq!(
            ack_url("http://control-plane:8090/api/v1/agent/routes/?app_id=app-1").unwrap(),
            "http://control-plane:8090/api/v1/agent/routes/ack"
        );
    }

    #[test]
    fn config_generation_is_read_from_response_header() {
        let mut headers = HeaderMap::new();
        headers.insert(CONFIG_GENERATION_HEADER, "42".parse().unwrap());
        assert_eq!(config_generation(&headers), Ok(42));

        headers.insert(
            CONFIG_GENERATION_HEADER,
            (i64::MAX as u64 + 1).to_string().parse().unwrap(),
        );
        assert!(config_generation(&headers)
            .unwrap_err()
            .contains("exceeds the persistent BIGINT range"));
    }

    #[tokio::test]
    async fn persisted_floor_blocks_restart_rollback_until_matching_snapshot_is_loaded() {
        let manager = TunnelManager::new();
        let expected = route("current", "connector-new");
        let expected_hash = route_snapshot_hash(vec![expected.clone()]);
        manager
            .restore_generation_floor(9, expected_hash, 100)
            .await
            .unwrap();

        assert!(!manager.initial_sync_succeeded());
        assert!(manager.resolve_route_by_app_id("current").await.is_none());
        assert_eq!(
            manager
                .apply_route_snapshot_at(vec![route("stale", "connector-old")], 8, 200)
                .await,
            SnapshotApplyOutcome::RejectedOlder {
                received_generation: 8,
                current_generation: 9,
            }
        );
        assert_eq!(
            manager
                .apply_route_snapshot_at(vec![route("current", "connector-wrong")], 9, 300)
                .await,
            SnapshotApplyOutcome::RejectedSameGenerationConflict { generation: 9 }
        );
        assert_eq!(
            manager
                .apply_route_snapshot_at(vec![expected], 9, 400)
                .await,
            SnapshotApplyOutcome::Applied {
                generation: 9,
                applied_at_ms: 400,
            }
        );
        assert!(manager.initial_sync_succeeded());
        assert_eq!(
            manager
                .resolve_route_by_app_id("current")
                .await
                .unwrap()
                .connector_endpoint,
            "connector-new"
        );
    }

    #[test]
    fn freshest_endpoint_wins_even_when_a_reachable_fallback_is_stale() {
        let selected = select_freshest_snapshot(vec![
            FetchedRouteSnapshot {
                url: "http://localhost/routes".into(),
                generation: 3,
                routes: vec![route("old", "connector-old")],
            },
            FetchedRouteSnapshot {
                url: "http://primary/routes".into(),
                generation: 8,
                routes: vec![route("new", "connector-new")],
            },
        ])
        .unwrap();

        assert_eq!(selected.url, "http://primary/routes");
        assert_eq!(selected.generation, 8);
        assert_eq!(selected.routes[0].app_id, "new");
    }

    #[test]
    fn equal_generations_require_identical_canonical_content() {
        let first = route("app-a", "connector-a");
        let second = route("app-b", "connector-b");
        let selected = select_freshest_snapshot(vec![
            FetchedRouteSnapshot {
                url: "http://primary/routes".into(),
                generation: 8,
                routes: vec![second.clone(), first.clone()],
            },
            FetchedRouteSnapshot {
                url: "http://secondary/routes".into(),
                generation: 8,
                routes: vec![first, second],
            },
        ])
        .unwrap();
        assert_eq!(selected.url, "http://primary/routes");

        let error = select_freshest_snapshot(vec![
            FetchedRouteSnapshot {
                url: "http://primary/routes".into(),
                generation: 9,
                routes: vec![route("app-a", "connector-a")],
            },
            FetchedRouteSnapshot {
                url: "http://secondary/routes".into(),
                generation: 9,
                routes: vec![route("app-a", "connector-b")],
            },
        ])
        .unwrap_err();
        assert!(matches!(
            error,
            SnapshotSelectionError::SameGenerationConflict { generation: 9, .. }
        ));
    }
}
