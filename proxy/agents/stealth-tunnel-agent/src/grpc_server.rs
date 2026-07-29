use std::collections::{BTreeMap, HashMap};
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;
use std::time::Instant;

use futures::StreamExt;
use moka::future::Cache;
use reqwest::Client;
use sag_service_health::Readiness;
use sag_tunnel_proto::tunnel_service_server::TunnelService;
use sag_tunnel_proto::{
    tunnel_message, ConnectorRegisterAck, ForwardRequest, ForwardResponse, HttpHeader,
    TunnelMessage,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use shared_storage::{
    AuditLogRecord, AuditWriter, FaultEventRecord, FaultEventsStore, IdempotencyClaim,
    IdempotencyStore, StorageStore,
};
use tokio::sync::{mpsc, watch, RwLock, Semaphore};
use tokio_stream::wrappers::ReceiverStream;
use tonic::{Request, Response, Status, Streaming};
use tracing::{debug, info, warn};
use uuid::Uuid;

use crate::config::StealthTunnelConfig;
use crate::connector_registry::{ConnectorRegistry, PendingFailure};
use crate::degrade_redis::{AgentDegradeRedis, StalePolicyPayload};
use crate::manager::TunnelManager;

#[derive(Debug)]
struct RegisteredConnectorSession {
    endpoint: String,
    connector_id: String,
    generation: u64,
    stream_epoch: String,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct StoredResponseHeaders {
    legacy: HashMap<String, String>,
    values: Vec<(String, Vec<u8>)>,
}

fn heartbeat_matches_session(
    session: &RegisteredConnectorSession,
    endpoint: &str,
    connector_id: &str,
    stream_epoch: &str,
) -> bool {
    session.endpoint == endpoint
        && session.connector_id == connector_id
        && session.stream_epoch == stream_epoch
}

#[derive(Clone)]
pub struct PolicyDecisionSnapshot {
    decision: String,
    reason: String,
    matched_policy_id: Option<String>,
}

type PolicyEvalResult = Result<PolicyDecisionSnapshot, &'static str>;

#[derive(Debug, Clone)]
struct IdempotencyContext {
    scope_key: String,
    request_hash: String,
    owner_attempt_id: String,
    state_version: i64,
}

/// Removes an idempotency reservation if this RPC disappears before the
/// request is sent to the connector stream. After `disarm_release`, the
/// record must stay reserved until a definitive response is persisted because
/// the downstream side effect may already have happened.
struct IdempotencyDispatchGuard {
    store: StorageStore,
    context: Option<IdempotencyContext>,
}

impl IdempotencyDispatchGuard {
    fn new(store: StorageStore, context: Option<IdempotencyContext>) -> Self {
        Self { store, context }
    }

    fn disarm_release(&mut self) {
        self.context = None;
    }
}

impl Drop for IdempotencyDispatchGuard {
    fn drop(&mut self) {
        let Some(context) = self.context.take() else {
            return;
        };
        let store = self.store.clone();
        tokio::spawn(async move {
            match IdempotencyStore::release_undispatched(
                &store,
                &context.scope_key,
                &context.request_hash,
                &context.owner_attempt_id,
                context.state_version,
            )
            .await
            {
                Ok(true) => {
                    metrics::counter!("agent_idempotency_total", "result" => "released_undispatched")
                        .increment(1);
                }
                Ok(false) => {
                    metrics::counter!("agent_idempotency_total", "result" => "release_not_owner")
                        .increment(1);
                }
                Err(error) => {
                    metrics::counter!("agent_idempotency_total", "result" => "release_failed")
                        .increment(1);
                    warn!(%error, "failed to release undispatched idempotency claim");
                }
            }
        });
    }
}

enum IdempotencyDecision {
    NotRequired,
    Claimed(IdempotencyContext),
    Respond(ForwardResponse),
}

/// gRPC dataplane service (policy/auth, connector multiplex). **Clone** is cheap (Arc-heavy internals).
#[derive(Clone)]
pub struct StealthTunnelGrpcService {
    pub manager: TunnelManager,
    pub connector_registry: ConnectorRegistry,
    pub config: Arc<RwLock<StealthTunnelConfig>>,
    pub http_client: Client,
    pub policy_semaphore: Arc<Semaphore>,
    pub auth_semaphore: Arc<Semaphore>,
    pub pending_semaphore: Arc<Semaphore>,
    pub store: StorageStore,
    pub audit_writer: AuditWriter,
    pub policy_eval_cache: Arc<Cache<String, PolicyDecisionSnapshot>>,
    pub negative_cache: Arc<Cache<String, String>>,
    pub negative_cache_enabled: bool,
    pub readiness: Readiness,
    /// Optional Redis stale-while-degraded for policy ALLOW + auth identity (see `degrade_redis`).
    pub degrade: AgentDegradeRedis,
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_millis() as i64)
        .unwrap_or(0)
}

fn remaining_until(deadline_unix_ms: i64) -> Option<Duration> {
    let remaining_ms = deadline_unix_ms.saturating_sub(now_ms());
    (remaining_ms > 0).then(|| Duration::from_millis(remaining_ms as u64))
}

fn is_mutating_method(method: &str) -> bool {
    !matches!(
        method.to_ascii_uppercase().as_str(),
        "GET" | "HEAD" | "OPTIONS" | "TRACE"
    )
}

fn sha256_hex(parts: &[&[u8]]) -> String {
    let mut hasher = Sha256::new();
    for part in parts {
        hasher.update((part.len() as u64).to_be_bytes());
        hasher.update(part);
    }
    hex::encode(hasher.finalize())
}

fn may_trust_identity_headers(auth_endpoint_configured: bool, explicitly_trusted: bool) -> bool {
    !auth_endpoint_configured && explicitly_trusted
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum IdentityHeaderError {
    Missing,
    Invalid,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum IdentityVerificationError {
    Inactive,
    MissingUser,
}

fn select_verified_identity(
    active: bool,
    verified_user: Option<(String, Vec<String>)>,
) -> Result<(String, Vec<String>), IdentityVerificationError> {
    if !active {
        return Err(IdentityVerificationError::Inactive);
    }
    let (user_id, roles) = verified_user.ok_or(IdentityVerificationError::MissingUser)?;
    if user_id.trim().is_empty() || roles.is_empty() {
        return Err(IdentityVerificationError::MissingUser);
    }
    Ok((user_id, roles))
}

fn header_value_case_insensitive<'a>(
    headers: &'a HashMap<String, String>,
    name: &str,
) -> Option<&'a str> {
    headers
        .iter()
        .find(|(key, _)| key.eq_ignore_ascii_case(name))
        .map(|(_, value)| value.as_str())
        .filter(|value| !value.trim().is_empty())
}

fn required_bearer_token(headers: &HashMap<String, String>) -> Result<String, IdentityHeaderError> {
    let authorization = header_value_case_insensitive(headers, "authorization")
        .ok_or(IdentityHeaderError::Missing)?;
    let (scheme, token) = authorization
        .split_once(' ')
        .ok_or(IdentityHeaderError::Invalid)?;
    let token = token.trim();
    if !scheme.eq_ignore_ascii_case("Bearer")
        || token.is_empty()
        || token.chars().any(char::is_whitespace)
    {
        return Err(IdentityHeaderError::Invalid);
    }
    Ok(token.to_string())
}

fn install_canonical_identity(
    headers: &mut HashMap<String, String>,
    user_id: &str,
    roles: &[String],
) {
    const RESERVED: &[&str] = &[
        "x-sag-user-id",
        "x-sag-user-roles",
        "x-sag-authenticated",
        "x-user-id",
        "x-user-roles",
    ];
    headers.retain(|name, _| {
        !RESERVED
            .iter()
            .any(|reserved| name.eq_ignore_ascii_case(reserved))
    });
    headers.insert("x-sag-user-id".into(), user_id.to_string());
    headers.insert("x-sag-user-roles".into(), roles.join(","));
    headers.insert("x-sag-authenticated".into(), "verified".into());
}

fn canonical_identity_from_headers(
    headers: &HashMap<String, String>,
) -> Option<(String, Vec<String>)> {
    let authenticated = header_value_case_insensitive(headers, "x-sag-authenticated")?;
    if authenticated != "verified" {
        return None;
    }
    let user_id = header_value_case_insensitive(headers, "x-sag-user-id")?.to_string();
    let roles = header_value_case_insensitive(headers, "x-sag-user-roles")?
        .split(',')
        .map(str::trim)
        .filter(|role| !role.is_empty())
        .map(str::to_string)
        .collect::<Vec<_>>();
    if roles.is_empty() {
        return None;
    }
    Some((user_id, roles))
}

impl StealthTunnelGrpcService {
    /// Clears the in-process policy decision and negative caches.
    /// Does **not** close connector gRPC streams (they are meant to stay up) or control-plane routes.
    pub async fn clear_ephemeral_caches(&self) {
        self.policy_eval_cache.invalidate_all();
        self.negative_cache.invalidate_all();
        metrics::counter!("agent_ephemeral_cache_cleared_total").increment(1);
        info!("cleared agent ephemeral caches (policy + negative)");
    }

    fn policy_eval_key(req: &ForwardRequest, user_id: &str, roles: &[String]) -> String {
        let mut sorted_roles = roles.to_vec();
        sorted_roles.sort();
        format!(
            "{}|{}|{}|{}|{}",
            user_id,
            sorted_roles.join(","),
            req.app_id,
            req.path,
            req.method
        )
    }

    fn negative_key(req: &ForwardRequest, reason: &str) -> String {
        format!("{}|{}|{}|{}", reason, req.app_id, req.path, req.method)
    }

    fn deny_response(
        req: &ForwardRequest,
        status: u32,
        body: impl Into<Vec<u8>>,
    ) -> ForwardResponse {
        ForwardResponse {
            request_id: req.request_id.clone(),
            status_code: status,
            headers: Default::default(),
            body: body.into(),
            attempt_id: if req.attempt_id.is_empty() {
                req.request_id.clone()
            } else {
                req.attempt_id.clone()
            },
            header_values: Default::default(),
            stream_epoch: req.stream_epoch.clone(),
        }
    }

    fn idempotency_response(
        req: &ForwardRequest,
        status: u32,
        body: impl Into<Vec<u8>>,
        state: &str,
    ) -> ForwardResponse {
        let mut response = Self::deny_response(req, status, body);
        response
            .headers
            .insert("content-type".into(), "text/plain; charset=utf-8".into());
        response
            .headers
            .insert("x-sag-idempotency-state".into(), state.into());
        if state == "pending" {
            response.headers.insert("retry-after".into(), "1".into());
        }
        response
    }

    async fn prepare_idempotency(
        &self,
        req: &ForwardRequest,
        verified_principal: Option<&str>,
    ) -> Result<IdempotencyDecision, Status> {
        if !is_mutating_method(&req.method) {
            return Ok(IdempotencyDecision::NotRequired);
        }
        if req.idempotency_key.trim().is_empty() {
            metrics::counter!("agent_idempotency_total", "result" => "missing_key").increment(1);
            return Ok(IdempotencyDecision::Respond(Self::idempotency_response(
                req,
                400,
                "mutating request requires an idempotency key",
                "missing-key",
            )));
        }

        // Include the caller credential hash in the scope so a guessed key can
        // never replay another caller's response body.
        let authorization = req
            .headers
            .get("authorization")
            .or_else(|| req.headers.get("Authorization"))
            .map(String::as_bytes)
            .unwrap_or_default();
        let asserted_user_id = req
            .headers
            .get("x-sag-user-id")
            .or_else(|| req.headers.get("x-user-id"))
            .map(String::as_bytes)
            .unwrap_or_default();
        let principal_scope = verified_principal
            .map(str::as_bytes)
            .filter(|principal| !principal.is_empty())
            .unwrap_or({
                if authorization.is_empty() {
                    asserted_user_id
                } else {
                    authorization
                }
            });
        let scope_key = sha256_hex(&[
            req.app_id.as_bytes(),
            principal_scope,
            req.idempotency_key.as_bytes(),
        ]);
        let mut semantic_headers = BTreeMap::new();
        for (name, value) in &req.headers {
            let name = name.to_ascii_lowercase();
            if matches!(
                name.as_str(),
                "authorization"
                    | "idempotency-key"
                    | "x-idempotency-key"
                    | "x-request-id"
                    | "x-trace-id"
            ) {
                continue;
            }
            semantic_headers.insert(name, value);
        }
        let semantic_headers = serde_json::to_vec(&semantic_headers)
            .map_err(|_| Status::internal("failed to canonicalize idempotent request"))?;
        let method = req.method.to_ascii_uppercase();
        let request_hash = sha256_hex(&[
            req.app_id.as_bytes(),
            method.as_bytes(),
            req.path.as_bytes(),
            semantic_headers.as_slice(),
            req.body.as_slice(),
        ]);
        let ttl_sec = std::env::var("SAG_IDEMPOTENCY_TTL_SEC")
            .ok()
            .and_then(|value| value.parse::<i64>().ok())
            .unwrap_or(86_400)
            .clamp(60, 30 * 86_400);
        let now = now_ms();
        let expires_at_ms = now.saturating_add(ttl_sec.saturating_mul(1000));
        let remaining = remaining_until(req.deadline_unix_ms).ok_or_else(|| {
            Status::deadline_exceeded("request deadline expired before idempotency claim")
        })?;
        let claim = tokio::time::timeout(
            remaining,
            IdempotencyStore::claim(
                &self.store,
                &scope_key,
                &request_hash,
                &req.attempt_id,
                now,
                expires_at_ms,
            ),
        )
        .await
        .map_err(|_| Status::deadline_exceeded("idempotency claim deadline exceeded"))?
        .map_err(|error| {
            warn!(
                request_id = %req.request_id,
                attempt_id = %req.attempt_id,
                %error,
                "idempotency claim failed"
            );
            Status::unavailable("idempotency store unavailable")
        })?;

        match claim {
            IdempotencyClaim::Claimed { state_version } => {
                metrics::counter!("agent_idempotency_total", "result" => "claimed").increment(1);
                Ok(IdempotencyDecision::Claimed(IdempotencyContext {
                    scope_key,
                    request_hash,
                    owner_attempt_id: req.attempt_id.clone(),
                    state_version,
                }))
            }
            IdempotencyClaim::Pending => {
                metrics::counter!("agent_idempotency_total", "result" => "pending").increment(1);
                Ok(IdempotencyDecision::Respond(Self::idempotency_response(
                    req,
                    409,
                    "an operation with this idempotency key is still pending; do not re-execute",
                    "pending",
                )))
            }
            IdempotencyClaim::Conflict => {
                metrics::counter!("agent_idempotency_total", "result" => "conflict").increment(1);
                Ok(IdempotencyDecision::Respond(Self::idempotency_response(
                    req,
                    409,
                    "idempotency key was already used with a different request",
                    "conflict",
                )))
            }
            IdempotencyClaim::Completed(record) => {
                let (mut headers, mut header_values) =
                    match serde_json::from_str::<StoredResponseHeaders>(&record.headers_json) {
                        Ok(stored) => (
                            stored.legacy,
                            stored
                                .values
                                .into_iter()
                                .map(|(name, value)| HttpHeader { name, value })
                                .collect(),
                        ),
                        Err(_) => (
                            serde_json::from_str::<HashMap<String, String>>(&record.headers_json)
                                .unwrap_or_default(),
                            Vec::new(),
                        ),
                    };
                headers.insert("x-sag-idempotency-state".into(), "replayed".into());
                header_values.push(HttpHeader {
                    name: "x-sag-idempotency-state".into(),
                    value: b"replayed".to_vec(),
                });
                metrics::counter!("agent_idempotency_total", "result" => "replayed").increment(1);
                Ok(IdempotencyDecision::Respond(ForwardResponse {
                    request_id: req.request_id.clone(),
                    attempt_id: req.attempt_id.clone(),
                    status_code: record.status_code,
                    headers,
                    body: record.body,
                    header_values,
                    stream_epoch: String::new(),
                }))
            }
        }
    }

    async fn persist_idempotent_response(
        &self,
        req: &ForwardRequest,
        context: &IdempotencyContext,
        response: &ForwardResponse,
    ) -> Result<(), Status> {
        let remaining = remaining_until(req.deadline_unix_ms).ok_or_else(|| {
            Status::deadline_exceeded("request deadline expired before idempotency completion")
        })?;
        let headers_json = serde_json::to_string(&StoredResponseHeaders {
            legacy: response.headers.clone(),
            values: response
                .header_values
                .iter()
                .map(|header| (header.name.clone(), header.value.clone()))
                .collect(),
        })
        .map_err(|_| Status::internal("failed to encode idempotent response headers"))?;
        let completed = tokio::time::timeout(
            remaining,
            IdempotencyStore::complete(
                &self.store,
                &context.scope_key,
                &context.request_hash,
                &context.owner_attempt_id,
                context.state_version,
                response.status_code,
                &headers_json,
                &response.body,
                now_ms(),
            ),
        )
        .await
        .map_err(|_| Status::deadline_exceeded("idempotency completion deadline exceeded"))?
        .map_err(|error| {
            warn!(
                request_id = %req.request_id,
                attempt_id = %req.attempt_id,
                %error,
                "idempotency completion failed"
            );
            Status::unavailable("idempotency result could not be persisted")
        })?;
        if !completed {
            return Err(Status::aborted(
                "idempotency claim ownership changed before completion",
            ));
        }
        metrics::counter!("agent_idempotency_total", "result" => "completed").increment(1);
        Ok(())
    }

    async fn persist_idempotency_dispatched(
        &self,
        req: &ForwardRequest,
        context: &mut IdempotencyContext,
    ) -> Result<(), Status> {
        let remaining = remaining_until(req.deadline_unix_ms).ok_or_else(|| {
            Status::deadline_exceeded(
                "request deadline expired before dispatch acceptance was persisted",
            )
        })?;
        let next_version = tokio::time::timeout(
            remaining,
            IdempotencyStore::mark_dispatched(
                &self.store,
                &context.scope_key,
                &context.request_hash,
                &context.owner_attempt_id,
                context.state_version,
                now_ms(),
            ),
        )
        .await
        .map_err(|_| Status::deadline_exceeded("idempotency dispatch persistence timed out"))?
        .map_err(|error| {
            warn!(%error, "failed to persist accepted idempotency dispatch");
            Status::unavailable("idempotency dispatch state unavailable")
        })?
        .ok_or_else(|| Status::aborted("idempotency dispatch ownership changed"))?;
        context.state_version = next_version;
        metrics::counter!("agent_idempotency_total", "result" => "dispatched").increment(1);
        Ok(())
    }

    /// Conservatively records an unknown result. If Agent did not observe the
    /// acceptance frame, the intermediate dispatched transition represents
    /// possible dispatch, never proof that it is safe to retry.
    async fn persist_idempotency_indeterminate(&self, context: &mut IdempotencyContext) {
        let write_timeout = Duration::from_secs(2);
        if context.state_version == 1 {
            match tokio::time::timeout(
                write_timeout,
                IdempotencyStore::mark_dispatched(
                    &self.store,
                    &context.scope_key,
                    &context.request_hash,
                    &context.owner_attempt_id,
                    context.state_version,
                    now_ms(),
                ),
            )
            .await
            {
                Ok(Ok(Some(version))) => context.state_version = version,
                Ok(Ok(None)) => return,
                Ok(Err(error)) => {
                    warn!(%error, "failed to record conservative idempotency dispatch");
                    return;
                }
                Err(_) => {
                    warn!("timed out recording conservative idempotency dispatch");
                    return;
                }
            }
        }

        match tokio::time::timeout(
            write_timeout,
            IdempotencyStore::mark_indeterminate(
                &self.store,
                &context.scope_key,
                &context.request_hash,
                &context.owner_attempt_id,
                context.state_version,
                now_ms(),
            ),
        )
        .await
        {
            Ok(Ok(Some(version))) => {
                context.state_version = version;
                metrics::counter!("agent_idempotency_total", "result" => "indeterminate")
                    .increment(1);
            }
            Ok(Ok(None)) => {
                warn!("idempotency state changed before it could become indeterminate")
            }
            Ok(Err(error)) => warn!(%error, "failed to persist indeterminate idempotency state"),
            Err(_) => warn!("timed out persisting indeterminate idempotency state"),
        }
    }

    /// Low-cardinality labels for Prometheus-style counters (do not use free-form policy text).
    fn record_forward_deny_403(reason: &'static str) {
        metrics::counter!("agent_forward_http_403_total", "reason" => reason).increment(1);
    }

    fn metric_reason_for_policy_deny(eval: &PolicyDecisionSnapshot) -> &'static str {
        match eval.reason.as_str() {
            "policy bulkhead limit reached" => "policy_bulkhead",
            "policy evaluate timeout" => "policy_eval_timeout",
            "policy evaluate http send failed" => "policy_eval_http_send_failed",
            "policy evaluate http error" => "policy_eval_http_error",
            "policy evaluate parse failed" => "policy_eval_parse_failed",
            _ if eval.matched_policy_id.is_some() => "policy_service_deny",
            _ => "policy_deny_other",
        }
    }

    fn policy_cache_entry_authoritative(s: &PolicyDecisionSnapshot) -> bool {
        if s.decision.eq_ignore_ascii_case("ALLOW") {
            return true;
        }
        s.decision.eq_ignore_ascii_case("DENY") && s.matched_policy_id.is_some()
    }

    fn record_policy_unavailable(reason: &'static str) {
        metrics::counter!("agent_forward_policy_unavailable_total", "reason" => reason)
            .increment(1);
    }

    async fn fetch_policy_eval_http_once(
        &self,
        policy_url: &str,
        timeout_ms: u64,
        payload: &serde_json::Value,
    ) -> PolicyEvalResult {
        let _permit = self
            .policy_semaphore
            .clone()
            .acquire_owned()
            .await
            .map_err(|_| "policy_eval_semaphore_closed")?;

        #[derive(Deserialize)]
        struct PolicyEvalResponse {
            decision: String,
            reason: String,
            matched_policy_id: Option<String>,
            #[allow(dead_code)]
            cache_hit: bool,
        }

        let internal_token = std::env::var("SAG_POLICY_INTERNAL_TOKEN")
            .ok()
            .filter(|value| !value.trim().is_empty())
            .ok_or("policy_eval_internal_auth_missing")?;
        let resp_result = tokio::time::timeout(
            Duration::from_millis(timeout_ms),
            self.http_client
                .post(policy_url)
                .header("x-sag-internal-authenticated", "agent")
                .bearer_auth(internal_token)
                .json(payload)
                .send(),
        )
        .await;

        let resp = match resp_result {
            Ok(Ok(r)) => r,
            Ok(Err(_)) => return Err("policy_eval_http_send_failed"),
            Err(_) => return Err("policy_eval_timeout"),
        };
        if matches!(
            resp.status(),
            reqwest::StatusCode::UNAUTHORIZED | reqwest::StatusCode::FORBIDDEN
        ) {
            return Err("policy_eval_internal_auth_rejected");
        }
        let resp = resp
            .error_for_status()
            .map_err(|_| "policy_eval_http_error")?;
        match resp.json::<PolicyEvalResponse>().await {
            Ok(v) => Ok(PolicyDecisionSnapshot {
                decision: v.decision,
                reason: v.reason,
                matched_policy_id: v.matched_policy_id,
            }),
            Err(_) => Err("policy_eval_parse_failed"),
        }
    }

    async fn authorize_forward_or_deny_response(
        &self,
        req: &mut ForwardRequest,
    ) -> Result<(Option<ForwardResponse>, Option<String>), Status> {
        let start = Instant::now();
        let (user_id, roles_raw) = self.resolve_user_identity(req).await?;

        let canonical_identity = match (&user_id, &roles_raw) {
            (Some(user_id), Some(roles_raw)) if !user_id.is_empty() && !roles_raw.is_empty() => {
                let roles = roles_raw
                    .split(',')
                    .map(str::trim)
                    .filter(|role| !role.is_empty())
                    .map(str::to_string)
                    .collect::<Vec<_>>();
                if roles.is_empty() {
                    None
                } else {
                    install_canonical_identity(&mut req.headers, user_id, &roles);
                    Some((user_id.clone(), roles))
                }
            }
            _ => None,
        };

        let policy_endpoint = {
            let cfg = self.config.read().await;
            cfg.policy_evaluate_endpoint.clone()
        };
        let Some(policy_url) = policy_endpoint else {
            return Ok((None, canonical_identity.map(|(user_id, _)| user_id)));
        };

        let (user_id, roles) = match canonical_identity {
            Some(identity) => identity,
            None => {
                Self::record_forward_deny_403("missing_canonical_identity");
                return Ok((
                    Some(Self::deny_response(req, 403, "missing canonical identity")),
                    None,
                ));
            }
        };

        let timeout_ms = {
            let cfg = self.config.read().await;
            cfg.policy_evaluate_timeout_ms
        };

        let cache_key = Self::policy_eval_key(req, &user_id, &roles);
        let ck = cache_key.clone();

        if let Some(cached) = self.policy_eval_cache.get(&ck).await {
            if Self::policy_cache_entry_authoritative(&cached) {
                let response = self
                    .finish_policy_eval(req, cached, start, Some(ck.as_str()))
                    .await?;
                return Ok((response, Some(user_id)));
            }
            self.policy_eval_cache.invalidate(&ck).await;
        }

        let payload = serde_json::json!({
            "user_id": user_id.clone(),
            "roles": roles,
            "app_id": req.app_id,
            "path": req.path,
            "method": req.method,
            "identity_verified": true,
        });

        let policy_url_refresh = policy_url.clone();
        let payload_refresh = payload.clone();

        let eval = match self
            .fetch_policy_eval_http_once(&policy_url, timeout_ms, &payload)
            .await
        {
            Ok(v) => v,
            Err(reason) => {
                let stale_eligible = matches!(
                    reason,
                    "policy_eval_http_send_failed"
                        | "policy_eval_timeout"
                        | "policy_eval_http_error"
                        | "policy_eval_parse_failed"
                        | "policy_eval_semaphore_closed"
                );
                if stale_eligible {
                    if let Some(stale) = self.degrade.get_stale_policy_allow(&ck).await {
                        metrics::counter!("agent_policy_stale_allow_on_live_error_total")
                            .increment(1);
                        let snap = PolicyDecisionSnapshot {
                            decision: stale.decision,
                            reason: format!("stale_allow_redis({reason})"),
                            matched_policy_id: stale.matched_policy_id,
                        };
                        self.policy_eval_cache
                            .insert(ck.clone(), snap.clone())
                            .await;
                        AgentDegradeRedis::spawn_policy_refresh_hint(
                            self.http_client.clone(),
                            policy_url_refresh,
                            timeout_ms,
                            payload_refresh,
                            ck.clone(),
                            self.degrade.clone(),
                        );
                        let response = self
                            .finish_policy_eval(req, snap, start, Some(ck.as_str()))
                            .await?;
                        return Ok((response, Some(user_id)));
                    }
                }
                Self::record_policy_unavailable(reason);
                let body = match reason {
                    "policy_eval_http_send_failed" => {
                        "policy evaluate temporarily unavailable (send failed)"
                    }
                    "policy_eval_timeout" => "policy evaluate temporarily unavailable (timeout)",
                    "policy_eval_http_error" => {
                        "policy evaluate temporarily unavailable (http error)"
                    }
                    "policy_eval_parse_failed" => {
                        "policy evaluate temporarily unavailable (parse failed)"
                    }
                    "policy_eval_semaphore_closed" => {
                        "policy evaluate temporarily unavailable (semaphore closed)"
                    }
                    "policy_eval_internal_auth_missing" => {
                        "policy internal authentication is not configured"
                    }
                    "policy_eval_internal_auth_rejected" => {
                        "policy rejected internal authentication"
                    }
                    _ => "policy evaluate temporarily unavailable",
                };
                return Ok((Some(Self::deny_response(req, 503, body)), Some(user_id)));
            }
        };

        if eval.decision.eq_ignore_ascii_case("ALLOW") {
            self.policy_eval_cache
                .insert(ck.clone(), eval.clone())
                .await;
            self.degrade
                .set_stale_policy_allow(
                    &ck,
                    &StalePolicyPayload {
                        decision: eval.decision.clone(),
                        reason: eval.reason.clone(),
                        matched_policy_id: eval.matched_policy_id.clone(),
                    },
                )
                .await;
            let response = self
                .finish_policy_eval(req, eval, start, Some(ck.as_str()))
                .await?;
            return Ok((response, Some(user_id)));
        }

        if eval.matched_policy_id.is_some() {
            self.policy_eval_cache
                .insert(ck.clone(), eval.clone())
                .await;
        }

        let response = self
            .finish_policy_eval(req, eval, start, Some(ck.as_str()))
            .await?;
        Ok((response, Some(user_id)))
    }

    async fn finish_policy_eval(
        &self,
        req: &ForwardRequest,
        eval: PolicyDecisionSnapshot,
        start: Instant,
        policy_cache_key: Option<&str>,
    ) -> Result<Option<ForwardResponse>, Status> {
        debug!(decision = %eval.decision, "policy evaluate completed");

        let is_allow = eval.decision.eq_ignore_ascii_case("ALLOW");
        let stale_recovered = if !is_allow {
            if let Some(ck) = policy_cache_key {
                if self.degrade.stale_on_transient_deny
                    && AgentDegradeRedis::transient_policy_denial_reason(&eval.reason)
                {
                    self.degrade.get_stale_policy_allow(ck).await
                } else {
                    None
                }
            } else {
                None
            }
        } else {
            None
        };
        let effective_allow = is_allow || stale_recovered.is_some();

        if let (Some(stale), Some(ck)) = (&stale_recovered, policy_cache_key) {
            if !is_allow {
                metrics::counter!("agent_policy_stale_allow_on_transient_deny_total").increment(1);
                let recovered = PolicyDecisionSnapshot {
                    decision: stale.decision.clone(),
                    reason: format!("stale_allow_after_transient_deny({})", eval.reason),
                    matched_policy_id: stale.matched_policy_id.clone(),
                };
                self.policy_eval_cache
                    .insert(ck.to_string(), recovered)
                    .await;
            }
        }

        let elapsed = start.elapsed().as_secs_f64();
        metrics::histogram!("agent_policy_eval_duration_seconds").record(elapsed);
        let decision_label = if effective_allow { "ALLOW" } else { "DENY" };
        metrics::counter!("agent_policy_eval_total", "decision" => decision_label).increment(1);

        if !effective_allow {
            let body = format!(
                "access denied (policy: {}, reason: {})",
                eval.matched_policy_id
                    .clone()
                    .unwrap_or_else(|| "unknown".into()),
                eval.reason
            );
            if self.negative_cache_enabled && eval.matched_policy_id.is_some() {
                let k = Self::negative_key(req, "policy_deny");
                self.negative_cache.insert(k, body.clone()).await;
            }
            Self::record_forward_deny_403(Self::metric_reason_for_policy_deny(&eval));
            return Ok(Some(Self::deny_response(req, 403, body)));
        }

        Ok(None)
    }

    async fn resolve_user_identity(
        &self,
        req: &ForwardRequest,
    ) -> Result<(Option<String>, Option<String>), Status> {
        let user_id = header_value_case_insensitive(&req.headers, "x-sag-user-id")
            .or_else(|| header_value_case_insensitive(&req.headers, "x-user-id"))
            .map(str::to_string);

        let roles_raw = header_value_case_insensitive(&req.headers, "x-sag-user-roles")
            .or_else(|| header_value_case_insensitive(&req.headers, "x-user-roles"))
            .map(str::to_string);

        let (auth_url, trust_identity_headers) = {
            let cfg = self.config.read().await;
            (cfg.auth_verify_endpoint.clone(), cfg.trust_identity_headers)
        };
        if may_trust_identity_headers(auth_url.is_some(), trust_identity_headers) {
            return Ok((user_id, roles_raw));
        }
        let Some(endpoint) = auth_url else {
            return Ok((None, None));
        };

        let token = match required_bearer_token(&req.headers) {
            Ok(token) => token,
            Err(IdentityHeaderError::Missing) => {
                metrics::counter!("auth_missing", "service" => "stealth-tunnel-agent").increment(1);
                return Err(Status::unauthenticated("missing Bearer token"));
            }
            Err(IdentityHeaderError::Invalid) => {
                metrics::counter!("auth_invalid", "service" => "stealth-tunnel-agent").increment(1);
                return Err(Status::unauthenticated("invalid Authorization header"));
            }
        };

        let _permit = self
            .auth_semaphore
            .clone()
            .acquire_owned()
            .await
            .map_err(|_| Status::internal("auth semaphore closed"))?;

        let timeout_ms = self.config.read().await.auth_verify_timeout_ms;
        let verify_result = tokio::time::timeout(
            Duration::from_millis(timeout_ms),
            self.http_client
                .post(&endpoint)
                .json(&serde_json::json!({ "token": token }))
                .send(),
        )
        .await;

        let resp = match verify_result {
            Ok(Ok(r)) => r,
            Ok(Err(_)) => {
                if let Some((uid, roles_csv)) = self.degrade.get_stale_auth(&token).await {
                    metrics::counter!("agent_auth_stale_on_send_fail_total").increment(1);
                    return Ok((Some(uid), Some(roles_csv)));
                }
                return Err(Status::internal("auth verify send failed"));
            }
            Err(_) => {
                if let Some((uid, roles_csv)) = self.degrade.get_stale_auth(&token).await {
                    metrics::counter!("agent_auth_stale_on_timeout_total").increment(1);
                    return Ok((Some(uid), Some(roles_csv)));
                }
                return Err(Status::deadline_exceeded("auth verify timeout"));
            }
        };

        if matches!(
            resp.status(),
            reqwest::StatusCode::UNAUTHORIZED | reqwest::StatusCode::FORBIDDEN
        ) {
            metrics::counter!("auth_invalid", "service" => "stealth-tunnel-agent").increment(1);
            return Err(Status::unauthenticated("token verification rejected"));
        }
        if !resp.status().is_success() {
            return Err(Status::unavailable("auth verification service unavailable"));
        }

        #[derive(Deserialize)]
        struct VerifyResponse {
            active: bool,
            user: Option<VerifyUser>,
        }
        #[derive(Deserialize)]
        struct VerifyUser {
            id: String,
            roles: Vec<String>,
        }

        let verify = match resp.json::<VerifyResponse>().await {
            Ok(v) => v,
            Err(_) => return Err(Status::internal("auth verify parse failed")),
        };

        let verified_user = verify.user.map(|user| (user.id, user.roles));
        let (uid, roles) =
            select_verified_identity(verify.active, verified_user).map_err(|error| {
                metrics::counter!("auth_invalid", "service" => "stealth-tunnel-agent").increment(1);
                match error {
                    IdentityVerificationError::Inactive => {
                        Status::unauthenticated("inactive token")
                    }
                    IdentityVerificationError::MissingUser => Status::unauthenticated("no user"),
                }
            })?;
        let roles = roles.join(",");
        self.degrade.set_stale_auth(&token, &uid, &roles).await;
        Ok((Some(uid), Some(roles)))
    }
}

#[tonic::async_trait]
impl TunnelService for StealthTunnelGrpcService {
    type CreateTunnelStream =
        Pin<Box<dyn tokio_stream::Stream<Item = Result<TunnelMessage, Status>> + Send>>;

    async fn create_tunnel(
        &self,
        request: Request<Streaming<TunnelMessage>>,
    ) -> Result<Response<Self::CreateTunnelStream>, Status> {
        if self.readiness.is_draining() {
            return Err(Status::unavailable("agent is draining"));
        }
        let peer_cert_fingerprint = request.peer_certs().and_then(|certificates| {
            certificates
                .first()
                .map(|certificate| sha256_hex(&[certificate.as_ref()]))
        });
        let mut inbound = request.into_inner();
        let stream_buf = self.config.read().await.stream_buffer;
        let (outbound_tx, mut outbound_rx) = mpsc::channel::<TunnelMessage>(stream_buf);
        let (stream_tx, stream_rx) = mpsc::channel::<Result<TunnelMessage, Status>>(stream_buf);
        let (close_tx, mut close_rx) = watch::channel(false);

        let registry = self.connector_registry.clone();
        let config = self.config.clone();
        let outbound_for_register = outbound_tx.clone();

        tokio::spawn(async move {
            let mut registered: Option<RegisteredConnectorSession> = None;
            loop {
                tokio::select! {
                    inc = inbound.next() => {
                        match inc {
                            Some(Ok(msg)) => {
                                match msg.payload {
                                    Some(tunnel_message::Payload::Register(reg)) => {
                                        if registered.is_some() {
                                            warn!("Connector stream sent more than one Register message");
                                            let _ = stream_tx.try_send(Err(Status::already_exists(
                                                "Connector stream is already registered",
                                            )));
                                            break;
                                        }
                                        if reg.endpoint.trim().is_empty()
                                            || reg.connector_id.trim().is_empty()
                                            || Uuid::parse_str(&reg.stream_epoch).is_err()
                                        {
                                            warn!("Connector Register has invalid identity or stream_epoch");
                                            let _ = stream_tx.try_send(Err(Status::invalid_argument(
                                                "Connector endpoint, connector_id, and UUID stream_epoch are required",
                                            )));
                                            break;
                                        }
                                        let authorization = {
                                            let config = config.read().await;
                                            config.authorize_connector_certificate(
                                                &reg.endpoint,
                                                peer_cert_fingerprint.as_deref(),
                                            )
                                        };
                                        if let Err(reason) = authorization {
                                            metrics::counter!(
                                                "agent_connector_registration_rejected_total",
                                                "reason" => "certificate_binding"
                                            )
                                            .increment(1);
                                            warn!(
                                                connector_id = %reg.connector_id,
                                                endpoint = %reg.endpoint,
                                                %reason,
                                                "Connector registration rejected"
                                            );
                                            let _ = stream_tx.try_send(Err(Status::permission_denied(reason)));
                                            break;
                                        }
                                        let ack = TunnelMessage {
                                            payload: Some(tunnel_message::Payload::RegisterAck(
                                                ConnectorRegisterAck {
                                                    connector_id: reg.connector_id.clone(),
                                                    endpoint: reg.endpoint.clone(),
                                                    stream_epoch: reg.stream_epoch.clone(),
                                                },
                                            )),
                                        };
                                        if outbound_for_register.send(ack).await.is_err() {
                                            break;
                                        }
                                        let generation = registry.register(
                                            reg.endpoint.clone(),
                                            reg.connector_id.clone(),
                                            reg.stream_epoch.clone(),
                                            outbound_for_register.clone(),
                                            close_tx.clone(),
                                        );
                                        metrics::counter!("agent_connector_registration_total").increment(1);
                                        info!(
                                            connector_id = %reg.connector_id,
                                            endpoint = %reg.endpoint,
                                            generation,
                                            stream_epoch = %reg.stream_epoch,
                                            "Connector session registered"
                                        );
                                        registered = Some(RegisteredConnectorSession {
                                            endpoint: reg.endpoint,
                                            connector_id: reg.connector_id,
                                            generation,
                                            stream_epoch: reg.stream_epoch,
                                        });
                                    }
                                    Some(tunnel_message::Payload::Heartbeat(hb)) => {
                                        let Some(session) = registered.as_ref() else {
                                            warn!("Connector sent Heartbeat before Register");
                                            let _ = stream_tx.try_send(Err(Status::failed_precondition(
                                                "Connector must Register before Heartbeat",
                                            )));
                                            break;
                                        };
                                        if !heartbeat_matches_session(
                                            session,
                                            &hb.endpoint,
                                            &hb.connector_id,
                                            &hb.stream_epoch,
                                        ) || !registry.register_heartbeat(
                                            &session.endpoint,
                                            &session.connector_id,
                                            session.generation,
                                            &hb.stream_epoch,
                                        ) {
                                            metrics::counter!(
                                                "agent_connector_heartbeat_rejected_total"
                                            )
                                            .increment(1);
                                            warn!(
                                                registered_endpoint = %session.endpoint,
                                                heartbeat_endpoint = %hb.endpoint,
                                                registered_connector_id = %session.connector_id,
                                                heartbeat_connector_id = %hb.connector_id,
                                                generation = session.generation,
                                                "Connector heartbeat does not match the active session"
                                            );
                                            let _ = stream_tx.try_send(Err(Status::failed_precondition(
                                                "Heartbeat does not match the registered Connector session",
                                            )));
                                            break;
                                        }
                                    }
                                    Some(tunnel_message::Payload::Response(resp)) => {
                                        let Some(session) = registered.as_ref() else {
                                            warn!("Connector sent Response before Register");
                                            let _ = stream_tx.try_send(Err(Status::failed_precondition(
                                                "Connector must Register before Response",
                                            )));
                                            break;
                                        };
                                        registry.resolve_response(
                                            session.generation,
                                            &session.stream_epoch,
                                            resp,
                                        );
                                    }
                                    Some(tunnel_message::Payload::Accepted(accepted)) => {
                                        let Some(session) = registered.as_ref() else {
                                            let _ = stream_tx.try_send(Err(Status::failed_precondition(
                                                "Connector must Register before ForwardAccepted",
                                            )));
                                            break;
                                        };
                                        if !registry.resolve_accepted(session.generation, accepted) {
                                            metrics::counter!(
                                                "agent_connector_accept_rejected_total"
                                            )
                                            .increment(1);
                                            let _ = stream_tx.try_send(Err(Status::failed_precondition(
                                                "ForwardAccepted does not match the active stream attempt",
                                            )));
                                            break;
                                        }
                                    }
                                    _ => {
                                        warn!("Connector stream sent an invalid client-to-Agent message");
                                        let _ = stream_tx.try_send(Err(Status::invalid_argument(
                                            "invalid Connector stream message",
                                        )));
                                        break;
                                    }
                                }
                            }
                            Some(Err(st)) => {
                                warn!(
                                    code = ?st.code(),
                                    message = %st.message(),
                                    "connector tunnel inbound stream error"
                                );
                                break;
                            }
                            None => break,
                        }
                    }
                    out = outbound_rx.recv() => {
                        match out {
                            Some(msg) => {
                                if stream_tx.send(Ok(msg)).await.is_err() {
                                    warn!("connector_stream_downstream_closed: outbound send failed (peer reset, full buffer, or client disconnect)");
                                    break;
                                }
                            }
                            None => break,
                        }
                    }
                    changed = close_rx.changed() => {
                        if changed.is_err() || *close_rx.borrow() {
                            if let Some(session) = registered.as_ref() {
                                warn!(
                                    connector_id = %session.connector_id,
                                    endpoint = %session.endpoint,
                                    generation = session.generation,
                                    "Connector session revoked"
                                );
                            }
                            break;
                        }
                    }
                }
            }
            if let Some(session) = registered.take() {
                registry.unregister(&session.endpoint, session.generation);
            }
        });

        let stream = ReceiverStream::new(stream_rx);
        Ok(Response::new(Box::pin(stream)))
    }

    async fn forward(
        &self,
        request: Request<ForwardRequest>,
    ) -> Result<Response<ForwardResponse>, Status> {
        let Some(_active_request) = self.readiness.try_admit() else {
            return Err(Status::unavailable("agent is draining"));
        };
        let start = Instant::now();
        let mut req = request.into_inner();
        if req.attempt_id.is_empty() {
            req.attempt_id = req.request_id.clone();
        }
        let (agent_timeout_ms, max_request_body_bytes, max_response_body_bytes) = {
            let config = self.config.read().await;
            (
                config.forward_timeout_ms.max(1),
                config.max_request_body_bytes,
                config.max_response_body_bytes,
            )
        };
        if req.body.len() > max_request_body_bytes {
            metrics::counter!("agent_forward_total", "result" => "request_body_too_large")
                .increment(1);
            return Ok(Response::new(Self::deny_response(
                &req,
                413,
                format!(
                    "request body exceeds SAG_AGENT_MAX_REQUEST_BODY_BYTES ({max_request_body_bytes})"
                ),
            )));
        }
        let agent_deadline = now_ms().saturating_add(agent_timeout_ms as i64);
        if req.deadline_unix_ms <= 0 || req.deadline_unix_ms > agent_deadline {
            req.deadline_unix_ms = agent_deadline;
        }
        let request_id = req.request_id.clone();
        let app_id = req.app_id.clone();
        let path = req.path.clone();
        let method = req.method.clone();
        let trace_id = req
            .headers
            .get("x-request-id")
            .or_else(|| req.headers.get("x-trace-id"))
            .cloned()
            .unwrap_or_else(|| Uuid::new_v4().to_string());
        let ts_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as i64)
            .unwrap_or(0);

        let authorize_remaining = remaining_until(req.deadline_unix_ms).ok_or_else(|| {
            Status::deadline_exceeded("request deadline expired before authorization")
        })?;
        let (authorization, verified_principal) = tokio::time::timeout(
            authorize_remaining,
            self.authorize_forward_or_deny_response(&mut req),
        )
        .await
        .map_err(|_| {
            metrics::counter!("agent_forward_total", "result" => "authorization_deadline")
                .increment(1);
            warn!(
                %request_id,
                attempt_id = %req.attempt_id,
                %trace_id,
                deadline_unix_ms = req.deadline_unix_ms,
                stage = "authorization",
                "request deadline exceeded"
            );
            Status::deadline_exceeded("request deadline expired during authorization")
        })??;
        if let Some(deny) = authorization {
            let c = metrics::counter!(
                "agent_forward_total",
                "result" => "denied",
                "app_id" => req.app_id.clone()
            );
            c.increment(1);
            return Ok(Response::new(deny));
        }
        let user_id = canonical_identity_from_headers(&req.headers)
            .map(|(user_id, _)| user_id)
            .unwrap_or_default();

        if self.negative_cache_enabled {
            let miss_route_key = Self::negative_key(&req, "no_tunnel_route");
            if let Some(body) = self.negative_cache.get(&miss_route_key).await {
                let c = metrics::counter!("cache_hit_total", "service" => "stealth-tunnel-agent", "cache" => "negative");
                c.increment(1);
                return Ok(Response::new(Self::deny_response(&req, 502, body)));
            }
        }
        let Some(route) = self.manager.resolve_route_by_app_id(&req.app_id).await else {
            let body = "no tunnel route for app_id".to_string();
            if self.negative_cache_enabled {
                let key = Self::negative_key(&req, "no_tunnel_route");
                self.negative_cache.insert(key, body.clone()).await;
                let miss = metrics::counter!("cache_miss_total", "service" => "stealth-tunnel-agent", "cache" => "negative");
                miss.increment(1);
            }
            return Ok(Response::new(Self::deny_response(&req, 502, body)));
        };

        let tunnel_healthy_window =
            Duration::from_secs(self.config.read().await.tunnel_healthy_window_sec.max(1));
        // Session freshness is a data-plane safety invariant. The legacy
        // require_healthy_tunnel route field remains wire/storage compatible,
        // but can no longer bypass generation-bound lease enforcement.
        if !self
            .connector_registry
            .is_tunnel_healthy_with_window(&route.connector_endpoint, tunnel_healthy_window)
        {
            if self.negative_cache_enabled {
                let body = "connector tunnel is unhealthy".to_string();
                let k = Self::negative_key(&req, "connector_unhealthy");
                self.negative_cache.insert(k, body.clone()).await;
                let miss = metrics::counter!("cache_miss_total", "service" => "stealth-tunnel-agent", "cache" => "negative");
                miss.increment(1);
            }
            return Err(Status::unavailable("connector tunnel is unhealthy"));
        }

        let mut idempotency_context = match self
            .prepare_idempotency(&req, verified_principal.as_deref())
            .await?
        {
            IdempotencyDecision::NotRequired => None,
            IdempotencyDecision::Claimed(context) => Some(context),
            IdempotencyDecision::Respond(response) => return Ok(Response::new(response)),
        };
        let mut idempotency_dispatch_guard =
            IdempotencyDispatchGuard::new(self.store.clone(), idempotency_context.clone());

        let permit = self
            .pending_semaphore
            .clone()
            .try_acquire_owned()
            .map_err(|_| {
                metrics::counter!("agent_forward_total", "result" => "pending_overflow")
                    .increment(1);
                warn!(
                    %request_id,
                    attempt_id = %req.attempt_id,
                    %trace_id,
                    stage = "pending_admission",
                    "Agent pending limit reached"
                );
                Status::resource_exhausted(
                    "connector pending overflow (too many in-flight requests)",
                )
            })?;

        let send_remaining = remaining_until(req.deadline_unix_ms).ok_or_else(|| {
            Status::deadline_exceeded("request deadline expired before connector send")
        })?;
        let mut pending = match tokio::time::timeout(
            send_remaining,
            self.connector_registry.send_request_to_connector(
                &route.connector_endpoint,
                req.clone(),
                permit,
                tunnel_healthy_window,
            ),
        )
        .await
        {
            Ok(Ok(pending)) => pending,
            Ok(Err(error)) => {
                warn!(
                    %request_id,
                    attempt_id = %req.attempt_id,
                    %trace_id,
                    stage = "connector_send",
                    %error,
                    "failed to send request to Connector stream"
                );
                return Err(Status::unavailable(error));
            }
            Err(_) => {
                metrics::counter!("agent_forward_total", "result" => "connector_send_deadline")
                    .increment(1);
                return Err(Status::deadline_exceeded(
                    "request deadline expired sending to connector",
                ));
            }
        };
        // The request is now on the transport. Even before an acceptance frame
        // arrives, cancellation cannot prove that downstream never reserved it.
        idempotency_dispatch_guard.disarm_release();

        let acceptance_remaining = remaining_until(req.deadline_unix_ms).ok_or_else(|| {
            Status::deadline_exceeded("request deadline expired before connector acceptance")
        })?;
        match tokio::time::timeout(acceptance_remaining, pending.wait_for_acceptance()).await {
            Ok(true) => {
                if let Some(context) = idempotency_context.as_mut() {
                    if let Err(mut status) =
                        self.persist_idempotency_dispatched(&req, context).await
                    {
                        self.persist_idempotency_indeterminate(context).await;
                        status
                            .metadata_mut()
                            .insert("x-sag-outcome", "unknown".parse().expect("static metadata"));
                        return Err(status);
                    }
                }
            }
            Ok(false) => {
                // The exact stream failure is retained by `pending.recv()` and
                // handled below as an unknown outcome.
            }
            Err(_) => {
                if let Some(context) = idempotency_context.as_mut() {
                    self.persist_idempotency_indeterminate(context).await;
                }
                let mut status = Status::deadline_exceeded(
                    "request deadline expired waiting for connector acceptance",
                );
                status
                    .metadata_mut()
                    .insert("x-sag-outcome", "unknown".parse().expect("static metadata"));
                return Err(status);
            }
        }

        let response_remaining = remaining_until(req.deadline_unix_ms).ok_or_else(|| {
            Status::deadline_exceeded("request deadline expired before connector wait")
        })?;
        let out = tokio::time::timeout(response_remaining, pending.recv()).await;

        let result = match out {
            Ok(Ok(resp)) => {
                let resp = if resp.body.len() > max_response_body_bytes {
                    metrics::counter!("agent_forward_total", "result" => "response_body_too_large")
                        .increment(1);
                    Self::deny_response(
                        &req,
                        503,
                        format!(
                            "connector response exceeds SAG_AGENT_MAX_RESPONSE_BODY_BYTES ({max_response_body_bytes})"
                        ),
                    )
                } else {
                    resp
                };
                if let Some(context) = idempotency_context.as_ref() {
                    self.persist_idempotent_response(&req, context, &resp)
                        .await?;
                }
                let elapsed = start.elapsed().as_secs_f64();
                let h = metrics::histogram!(
                    "agent_forward_duration_seconds",
                    "app_id" => req.app_id.clone()
                );
                h.record(elapsed);
                let c = metrics::counter!(
                    "agent_forward_total",
                    "result" => "ok",
                    "app_id" => req.app_id.clone(),
                    "status" => resp.status_code.to_string()
                );
                c.increment(1);
                Ok(Response::new(resp))
            }
            Ok(Err(PendingFailure::StreamLost {
                phase,
                stream_epoch,
            })) => {
                if let Some(context) = idempotency_context.as_mut() {
                    self.persist_idempotency_indeterminate(context).await;
                }
                let c = metrics::counter!(
                    "agent_forward_total",
                    "result" => "connector_stream_lost",
                    "app_id" => req.app_id.clone()
                );
                c.increment(1);
                let mut status =
                    Status::unavailable(format!("connector stream lost after phase {phase:?}"));
                status
                    .metadata_mut()
                    .insert("x-sag-outcome", "unknown".parse().expect("static metadata"));
                if let Ok(value) = req.attempt_id.parse() {
                    status.metadata_mut().insert("x-sag-attempt-id", value);
                }
                if let Ok(value) = stream_epoch.parse() {
                    status.metadata_mut().insert("x-sag-stream-epoch", value);
                }
                Err(status)
            }
            Ok(Err(PendingFailure::ProtocolViolation { phase, reason })) => {
                if let Some(context) = idempotency_context.as_mut() {
                    self.persist_idempotency_indeterminate(context).await;
                }
                metrics::counter!("agent_forward_total", "result" => "protocol_violation")
                    .increment(1);
                let mut status = Status::internal(format!(
                    "connector protocol violation after phase {phase:?}: {reason}"
                ));
                status
                    .metadata_mut()
                    .insert("x-sag-outcome", "unknown".parse().expect("static metadata"));
                Err(status)
            }
            Err(_) => {
                if let Some(context) = idempotency_context.as_mut() {
                    self.persist_idempotency_indeterminate(context).await;
                }
                let c = metrics::counter!(
                    "agent_forward_total",
                    "result" => "connector_timeout",
                    "app_id" => req.app_id.clone()
                );
                c.increment(1);
                warn!(
                    %request_id,
                    attempt_id = %req.attempt_id,
                    %trace_id,
                    deadline_unix_ms = req.deadline_unix_ms,
                    "connector response deadline exceeded"
                );
                let mut status = Status::deadline_exceeded("connector response timeout");
                status
                    .metadata_mut()
                    .insert("x-sag-outcome", "unknown".parse().expect("static metadata"));
                Err(status)
            }
        };
        drop(pending);

        let latency_ms = start.elapsed().as_millis() as i64;
        let (status_code, result_code) = match &result {
            Ok(resp) => (
                resp.get_ref().status_code as i64,
                resp.get_ref().status_code.to_string(),
            ),
            Err(status) => (status.code() as i64, format!("grpc:{:?}", status.code())),
        };
        let audit = AuditLogRecord {
            id: Uuid::new_v4().to_string(),
            ts_ms,
            service: "stealth-tunnel-agent".into(),
            user_id,
            app_id,
            path: path.clone(),
            method: method.clone(),
            latency_ms,
            decision: "FORWARD".into(),
            result: result_code.clone(),
            trace_id: trace_id.clone(),
            extra_json: "{}".into(),
        };
        let _ = self.audit_writer.try_record(audit);
        if status_code >= 500 || result_code.contains("DeadlineExceeded") || latency_ms >= 1200 {
            let fault = FaultEventRecord {
                id: Uuid::new_v4().to_string(),
                ts_ms,
                service: "stealth-tunnel-agent".into(),
                event_type: if result_code.contains("DeadlineExceeded") {
                    "timeout".into()
                } else if status_code >= 500 {
                    "upstream_error".into()
                } else {
                    "latency_spike".into()
                },
                severity: if status_code >= 500 || result_code.contains("DeadlineExceeded") {
                    "critical".into()
                } else {
                    "warn".into()
                },
                path,
                method,
                latency_ms,
                baseline_ms: 300,
                threshold_ms: 1200,
                status_code,
                result: result_code,
                trace_id,
                source: "agent_forward".into(),
                resolved_at_ms: None,
                meta_json: "{}".into(),
            };
            let store2 = self.store.clone();
            tokio::spawn(async move {
                let _ = FaultEventsStore::insert(&store2, &fault).await;
            });
        }
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn configured_auth_endpoint_disables_identity_header_trust() {
        assert!(!may_trust_identity_headers(true, true));
    }

    #[test]
    fn identity_headers_require_explicit_opt_in_without_auth_service() {
        assert!(!may_trust_identity_headers(false, false));
        assert!(may_trust_identity_headers(false, true));
    }

    #[test]
    fn identity_missing_bearer_rejects_forged_headers() {
        let headers = std::collections::HashMap::from([
            ("x-sag-user-id".into(), "forged".into()),
            ("x-sag-user-roles".into(), "admin".into()),
        ]);

        assert_eq!(
            required_bearer_token(&headers),
            Err(IdentityHeaderError::Missing)
        );
    }

    #[test]
    fn identity_invalid_authorization_never_falls_back_to_headers() {
        let headers = std::collections::HashMap::from([
            ("authorization".into(), "Basic forged".into()),
            ("x-user-id".into(), "forged".into()),
            ("x-user-roles".into(), "boss".into()),
        ]);

        assert_eq!(
            required_bearer_token(&headers),
            Err(IdentityHeaderError::Invalid)
        );
    }

    #[test]
    fn identity_verifier_rejection_never_selects_asserted_identity() {
        let forged_asserted_identity = Some(("forged".to_string(), vec!["admin".to_string()]));

        assert_eq!(
            select_verified_identity(false, forged_asserted_identity),
            Err(IdentityVerificationError::Inactive)
        );
        assert_eq!(
            select_verified_identity(true, None),
            Err(IdentityVerificationError::MissingUser)
        );
    }

    #[test]
    fn verified_identity_replaces_caller_assertions_for_policy_and_connector() {
        let mut headers = std::collections::HashMap::from([
            ("X-SAG-User-ID".into(), "forged".into()),
            ("x-user-roles".into(), "boss".into()),
            ("x-business-header".into(), "preserved".into()),
        ]);
        let roles = vec!["reader".to_string(), "operator".to_string()];

        install_canonical_identity(&mut headers, "verified-user", &roles);
        let policy_identity = canonical_identity_from_headers(&headers).unwrap();

        assert_eq!(
            policy_identity,
            ("verified-user".to_string(), roles.clone())
        );
        assert_eq!(
            canonical_identity_from_headers(&headers),
            Some(("verified-user".to_string(), roles))
        );
        assert_eq!(
            headers.get("x-business-header").map(String::as_str),
            Some("preserved")
        );
        assert_eq!(
            headers.get("x-sag-authenticated").map(String::as_str),
            Some("verified")
        );
        assert!(!headers
            .keys()
            .any(|name| name.eq_ignore_ascii_case("x-user-roles")));
    }

    #[test]
    fn heartbeat_must_match_the_stream_local_session_identity() {
        let session = RegisteredConnectorSession {
            endpoint: "connector-group:stream".into(),
            connector_id: "connector-1".into(),
            generation: 7,
            stream_epoch: "epoch-7".into(),
        };
        assert!(heartbeat_matches_session(
            &session,
            "connector-group:stream",
            "connector-1",
            "epoch-7"
        ));
        assert!(!heartbeat_matches_session(
            &session,
            "other-group:stream",
            "connector-1",
            "epoch-7"
        ));
        assert!(!heartbeat_matches_session(
            &session,
            "connector-group:stream",
            "connector-2",
            "epoch-7"
        ));
        assert!(!heartbeat_matches_session(
            &session,
            "connector-group:stream",
            "connector-1",
            "old-epoch"
        ));
    }
}
