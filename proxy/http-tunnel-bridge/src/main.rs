mod limits;
mod queue;

use std::collections::{HashMap, HashSet};
use std::fmt;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;
use std::time::Instant;

use axum::body::Body;
use axum::extract::Path;
use axum::extract::Request;
use axum::extract::State;
use axum::middleware::{self, Next};
use axum::response::IntoResponse;
use axum::response::Response;
use axum::routing::get;
use axum::Json;
use axum::Router;
use sag_runtime_budget::{MemoryBudget, ValidatedMemoryBudget};
use sag_service_health::Readiness;
use sag_tunnel_proto::{
    tunnel_service_client::TunnelServiceClient, ForwardRequest, ForwardResponse,
};
use serde::Serialize;
use shared_storage::{
    build_store_from_env, ensure_store_schema, AuditLogRecord, AuditWriter, FaultEventRecord,
    FaultEventsStore, StorageStore,
};
use tokio::sync::{RwLock, Semaphore};
use tonic::transport::{Certificate, Channel, ClientTlsConfig, Endpoint, Identity};
use tracing::{info, warn};
use uuid::Uuid;

#[derive(Clone)]
struct TunnelClientConfig {
    endpoint: String,
    tls_enabled: bool,
    tls_server_name: Option<String>,
    cert_p: String,
    key_p: String,
    ca_p: String,
    keepalive_ms: u64,
    keepalive_timeout_ms: u64,
    tcp_keepalive_ms: u64,
}

#[derive(Clone)]
struct TunnelClientPool {
    slots: Arc<Vec<Arc<RwLock<TunnelServiceClient<Channel>>>>>,
    rr: Arc<AtomicUsize>,
}

impl TunnelClientPool {
    async fn replace_slot(&self, idx: usize, cfg: &TunnelClientConfig) {
        match connect_tunnel_client(cfg).await {
            Ok(nc) => {
                let mut w = self.slots[idx].write().await;
                *w = nc;
            }
            Err(e) => warn!(?e, idx, "tunnel grpc slot reconnect failed"),
        }
    }
}

fn grpc_channel_metric_label(idx: usize) -> &'static str {
    const LABELS: &[&str] = &[
        "0", "1", "2", "3", "4", "5", "6", "7", "8", "9", "10", "11", "12", "13", "14", "15", "16",
        "17", "18", "19", "20", "21", "22", "23", "24", "25", "26", "27", "28", "29", "30", "31",
    ];
    LABELS.get(idx).copied().unwrap_or("unknown")
}

/// When soft-gate or tunnel-shed path fails to enqueue to Redis (serialization / Redis error).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum SoftEnqueueOnFailure {
    /// Return HTTP 503 JSON and `bridge_soft_enqueue_failure_503_total` (no sync forward).
    #[default]
    ServiceUnavailable503,
    /// Explicitly configured, bounded fallback for read-only methods only.
    ReadOnlyFallback,
}

fn soft_enqueue_on_failure_from_env() -> SoftEnqueueOnFailure {
    if env_bool("SAG_BRIDGE_READ_ONLY_SYNC_FALLBACK_ON_QUEUE_ERROR", false) {
        SoftEnqueueOnFailure::ReadOnlyFallback
    } else {
        SoftEnqueueOnFailure::ServiceUnavailable503
    }
}

fn queue_error_may_use_sync_fallback(policy: SoftEnqueueOnFailure, method: &str) -> bool {
    policy == SoftEnqueueOnFailure::ReadOnlyFallback && !is_mutating_method(method)
}

#[derive(Clone)]
pub struct AppState {
    metrics: metrics_exporter_prometheus::PrometheusHandle,
    /// One or more tonic clients (separate HTTP/2 connections) to stealth-tunnel-agent.
    tunnel_pool: TunnelClientPool,
    tunnel_cfg: TunnelClientConfig,
    forward_timeout_ms: u64,
    store: StorageStore,
    audit_writer: AuditWriter,
    /// Redis queue (optional).
    queue: Option<Arc<queue::QueueRuntime>>,
    /// Rejects before request-body polling when the total ingress budget is full.
    hard_ingress: Arc<limits::AdmissionGate>,
    /// Exact synchronous-forward budget; saturation sheds to Redis or fails fast.
    sync_admission: Arc<limits::AdmissionGate>,
    /// Caps concurrent tunnel unary `Forward` RPCs (HTTP sync + queue workers). `None` = disabled.
    tunnel_inflight: Option<Arc<Semaphore>>,
    /// After soft gate or tunnel shed: on `Serialization` / `Redis` enqueue errors, either sync fallback or 503.
    soft_enqueue_on_failure: SoftEnqueueOnFailure,
    /// Optional token-bucket per `x-sag-app-id` on dataplane HTTP (before body read).
    app_rps_limiter: Option<Arc<limits::AppRpsLimiter>>,
    /// Optional global circuit after consecutive full Unary `Forward` failures.
    forward_circuit: Option<Arc<limits::ForwardCircuit>>,
    /// Cool-off duration for circuit (for HTTP `Retry-After` when open).
    forward_circuit_cooloff_ms: u64,
    /// Maximum request body accepted by the bridge on both synchronous and queued paths.
    max_body_bytes: usize,
    /// Maximum tunnel response retained before returning it to the HTTP client.
    max_response_body_bytes: usize,
    readiness: Readiness,
}

fn validate_bridge_memory_budget(
    hard_ingress: usize,
    sync_limit: usize,
    worker_concurrency: usize,
    max_request_body: usize,
    max_response_body: usize,
    max_queue_body: usize,
    budget_bytes: u64,
) -> Result<ValidatedMemoryBudget, String> {
    let stream_capacity = (worker_concurrency as u64)
        .checked_mul(32)
        .ok_or_else(|| "Bridge worker batch capacity overflowed u64".to_string())?;
    MemoryBudget {
        budget_bytes,
        safety_factor_percent: 80,
        reserved_bytes: 64 * 1024 * 1024,
        ingress_concurrency: hard_ingress as u64,
        max_request_body: max_request_body as u64,
        response_concurrency: sync_limit as u64,
        max_response_body: max_response_body as u64,
        queue_capacity: 0,
        max_enqueued_bytes: max_queue_body as u64,
        stream_capacity,
        max_frame_bytes: max_queue_body as u64,
    }
    .validate()
}

/// Non-2xx tunnel errors; `InflightSaturated` is used when `try` acquire on [`AppState::tunnel_inflight`] fails.
#[derive(Debug)]
pub enum BridgeForwardError {
    Tunnel(String),
    DeadlineExceeded(String),
    OutcomeUnknown {
        detail: String,
        attempt_id: String,
        stream_epoch: String,
    },
    InflightSaturated,
    /// Global forward circuit is open (consecutive Unary failures exceeded threshold).
    CircuitOpen,
}

impl fmt::Display for BridgeForwardError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            BridgeForwardError::Tunnel(s) => write!(f, "{s}"),
            BridgeForwardError::DeadlineExceeded(s) => write!(f, "{s}"),
            BridgeForwardError::OutcomeUnknown { detail, .. } => write!(f, "{detail}"),
            BridgeForwardError::InflightSaturated => write!(f, "tunnel concurrent limit saturated"),
            BridgeForwardError::CircuitOpen => write!(f, "forward circuit open"),
        }
    }
}

impl std::error::Error for BridgeForwardError {}

fn unknown_outcome_metadata(status: &tonic::Status) -> Option<(String, String)> {
    let outcome = status.metadata().get("x-sag-outcome")?.to_str().ok()?;
    if outcome != "unknown" {
        return None;
    }
    let attempt_id = status
        .metadata()
        .get("x-sag-attempt-id")
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default()
        .to_string();
    let stream_epoch = status
        .metadata()
        .get("x-sag-stream-epoch")
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default()
        .to_string();
    Some((attempt_id, stream_epoch))
}

fn env_u64(key: &str, default: u64) -> u64 {
    std::env::var(key)
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
        .unwrap_or(default)
}

fn env_bool(key: &str, default: bool) -> bool {
    std::env::var(key)
        .map(|value| {
            matches!(
                value.to_ascii_lowercase().as_str(),
                "1" | "true" | "yes" | "on"
            )
        })
        .unwrap_or(default)
}

fn env_u32(key: &str, default: u32) -> u32 {
    std::env::var(key)
        .ok()
        .and_then(|v| v.parse::<u32>().ok())
        .unwrap_or(default)
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

fn header_value_case_insensitive(headers: &HashMap<String, String>, name: &str) -> Option<String> {
    headers
        .iter()
        .find(|(key, _)| key.eq_ignore_ascii_case(name))
        .map(|(_, value)| value.clone())
        .filter(|value| !value.trim().is_empty())
}

fn path_and_query_for_forward(uri: &axum::http::Uri) -> String {
    uri.path_and_query()
        .map(|value| value.as_str().to_string())
        .unwrap_or_else(|| "/".to_string())
}

fn is_hop_by_hop_header(name: &str) -> bool {
    matches!(
        name.to_ascii_lowercase().as_str(),
        "connection"
            | "keep-alive"
            | "proxy-authenticate"
            | "proxy-authorization"
            | "te"
            | "trailer"
            | "transfer-encoding"
            | "upgrade"
    )
}

fn connection_header_tokens(headers: &HashMap<String, String>) -> HashSet<String> {
    headers
        .iter()
        .filter(|(name, _)| name.eq_ignore_ascii_case("connection"))
        .flat_map(|(_, value)| value.split(','))
        .map(|value| value.trim().to_ascii_lowercase())
        .filter(|value| !value.is_empty())
        .collect()
}

fn sanitize_tunnel_headers(headers: HashMap<String, String>) -> HashMap<String, String> {
    let connection_tokens = connection_header_tokens(&headers);
    headers
        .into_iter()
        .filter(|(name, _)| {
            !is_hop_by_hop_header(name) && !connection_tokens.contains(&name.to_ascii_lowercase())
        })
        .collect()
}

fn sanitize_untrusted_headers(mut headers: axum::http::HeaderMap) -> axum::http::HeaderMap {
    const RESERVED_IDENTITY_HEADERS: &[&str] = &[
        "x-sag-user-id",
        "x-sag-user-roles",
        "x-sag-authenticated",
        "x-user-id",
        "x-user-roles",
    ];

    let mut stripped = 0_u64;
    for name in RESERVED_IDENTITY_HEADERS {
        stripped = stripped.saturating_add(headers.remove(*name).is_some() as u64);
    }
    if stripped > 0 {
        metrics::counter!("identity_header_stripped_total", "service" => "http-tunnel-bridge")
            .increment(stripped);
    }
    headers
}

async fn collect_body_limited(body: Body, max_body_bytes: usize) -> Result<Vec<u8>, ()> {
    axum::body::to_bytes(body, max_body_bytes)
        .await
        .map(|bytes| bytes.to_vec())
        .map_err(|_| ())
}

async fn metrics(State(state): State<AppState>) -> String {
    state.hard_ingress.refresh_metrics();
    state.sync_admission.refresh_metrics();
    state.metrics.render()
}

async fn live() -> axum::http::StatusCode {
    axum::http::StatusCode::OK
}

async fn ready(State(state): State<AppState>) -> axum::http::StatusCode {
    let tunnel_cfg = state.tunnel_cfg.clone();
    let queue = state.queue.clone();
    let timeout = Duration::from_millis(env_u64("SAG_READINESS_PROBE_TIMEOUT_MS", 1_000));
    let status = state
        .readiness
        .probe(timeout, async move {
            if connect_tunnel_client(&tunnel_cfg).await.is_err() {
                return false;
            }
            match queue {
                Some(queue) => queue.health_check().await.is_ok(),
                None => true,
            }
        })
        .await;
    if status == sag_service_health::ReadyState::Ready {
        axum::http::StatusCode::OK
    } else {
        axum::http::StatusCode::SERVICE_UNAVAILABLE
    }
}

#[derive(Serialize)]
struct QueuedAccepted {
    status: &'static str,
    queue_id: String,
    poll: String,
}

#[derive(Serialize)]
struct PollStatusBody {
    status: String,
    queue_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    http_status: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    headers_json: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    body_b64: Option<String>,
    body_truncated: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    retry_after_ms: Option<u64>,
}

async fn queue_status(
    State(state): State<AppState>,
    Path(queue_id): Path<String>,
) -> impl IntoResponse {
    let Some(qr) = state.queue.as_ref() else {
        return (
            axum::http::StatusCode::NOT_FOUND,
            Json(serde_json::json!({"error":"queue_disabled"})),
        )
            .into_response();
    };
    if qr.throttle_poll(&queue_id).await.is_err() {
        metrics::counter!("bridge_queue_poll_throttled_total").increment(1);
        return (
            axum::http::StatusCode::TOO_MANY_REQUESTS,
            [(
                axum::http::header::RETRY_AFTER,
                qr.cfg
                    .poll_min_interval_ms
                    .div_ceil(1000)
                    .max(1)
                    .to_string(),
            )],
            Json(serde_json::json!({
                "error":"poll_rate_limited",
                "retry_after_ms": qr.cfg.poll_min_interval_ms
            })),
        )
            .into_response();
    }
    let rec = match qr.read_job(&queue_id).await {
        Ok(r) => r,
        Err(e) => {
            tracing::error!(?e, "read_job");
            return (
                axum::http::StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({"error":"redis"})),
            )
                .into_response();
        }
    };
    let Some(job) = rec else {
        return (
            axum::http::StatusCode::NOT_FOUND,
            Json(serde_json::json!({"error":"unknown_queue_id"})),
        )
            .into_response();
    };
    let retry_after_ms = match job.status.as_str() {
        "pending" | "running" => Some(qr.cfg.poll_min_interval_ms.max(50)),
        _ => None,
    };
    let body = PollStatusBody {
        status: job.status.clone(),
        queue_id: queue_id.clone(),
        http_status: job.http_status,
        headers_json: job.headers_json.clone(),
        body_b64: job.body_b64.clone(),
        body_truncated: job.body_truncated,
        error: job.error.clone(),
        retry_after_ms,
    };
    let mut res = Json(body).into_response();
    if let Some(ms) = retry_after_ms {
        let sec = ms.div_ceil(1000).max(1);
        if let Ok(hv) = axum::http::HeaderValue::from_str(&sec.to_string()) {
            res.headers_mut()
                .insert(axum::http::header::RETRY_AFTER, hv);
        }
    }
    res
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

async fn metrics_mw(State(state): State<AppState>, req: Request, next: Next) -> Response<Body> {
    let start = Instant::now();
    let method = req.method().to_string();
    let path = req.uri().path().to_string();
    let app_id = req
        .headers()
        .get("x-sag-app-id")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_string();
    let user_id = req
        .headers()
        .get("x-sag-user-id")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_string();
    let trace_id = req
        .headers()
        .get("x-request-id")
        .or_else(|| req.headers().get("x-trace-id"))
        .and_then(|v| v.to_str().ok())
        .map(ToString::to_string)
        .unwrap_or_else(|| Uuid::new_v4().to_string());
    let res = next.run(req).await;
    let status = res.status().as_u16().to_string();
    let status_code = res.status().as_u16() as i64;
    let method2 = method.clone();
    let elapsed = start.elapsed().as_secs_f64();
    let latency_ms = (elapsed * 1000.0) as i64;

    let c = metrics::counter!(
        "http_requests_total",
        "service" => "http-tunnel-bridge",
        "method" => method,
        "path" => path.clone(),
        "status" => status.clone()
    );
    c.increment(1);
    let h = metrics::histogram!(
        "http_request_duration_seconds",
        "service" => "http-tunnel-bridge",
        "method" => method2.clone(),
        "path" => path.clone(),
        "status" => status.clone()
    );
    h.record(elapsed);

    let audit = AuditLogRecord {
        id: Uuid::new_v4().to_string(),
        ts_ms: now_ms(),
        service: "http-tunnel-bridge".into(),
        user_id,
        app_id,
        path: path.clone(),
        method: method2.clone(),
        latency_ms,
        decision: "FORWARD".into(),
        result: status.clone(),
        trace_id: trace_id.clone(),
        extra_json: "{}".into(),
    };
    let _ = state.audit_writer.try_record(audit);
    if status_code >= 500 || latency_ms >= 1200 {
        let fault = FaultEventRecord {
            id: Uuid::new_v4().to_string(),
            ts_ms: now_ms(),
            service: "http-tunnel-bridge".into(),
            event_type: if status_code >= 500 {
                "http_5xx".into()
            } else {
                "latency_spike".into()
            },
            severity: if status_code >= 500 {
                "critical".into()
            } else {
                "warn".into()
            },
            path: path.clone(),
            method: method2,
            latency_ms,
            baseline_ms: 300,
            threshold_ms: 1200,
            status_code,
            result: status,
            trace_id,
            source: "bridge_metrics_mw".into(),
            resolved_at_ms: None,
            meta_json: "{}".into(),
        };
        let store2 = state.store.clone();
        tokio::spawn(async move {
            let _ = FaultEventsStore::insert(&store2, &fault).await;
        });
    }

    res
}

async fn acquire_tunnel_permit(
    state: &AppState,
    block: bool,
    deadline_unix_ms: i64,
) -> Result<Option<tokio::sync::OwnedSemaphorePermit>, BridgeForwardError> {
    let Some(sem) = state.tunnel_inflight.as_ref() else {
        return Ok(None);
    };
    if block {
        let remaining = remaining_until(deadline_unix_ms).ok_or_else(|| {
            BridgeForwardError::DeadlineExceeded("request deadline exceeded in tunnel queue".into())
        })?;
        tokio::time::timeout(remaining, sem.clone().acquire_owned())
            .await
            .map_err(|_| {
                BridgeForwardError::DeadlineExceeded(
                    "request deadline exceeded in tunnel queue".into(),
                )
            })?
            .map_err(|_| BridgeForwardError::Tunnel("tunnel semaphore closed".into()))
            .map(Some)
    } else {
        sem.clone()
            .try_acquire_owned()
            .map_err(|_| BridgeForwardError::InflightSaturated)
            .map(Some)
    }
}

async fn forward_request_inner(
    state: &AppState,
    fr: ForwardRequest,
    tunnel_block: bool,
) -> Result<ForwardResponse, BridgeForwardError> {
    if let Some(cb) = state.forward_circuit.as_ref() {
        if cb.is_open() {
            metrics::counter!("bridge_forward_circuit_reject_total").increment(1);
            return Err(BridgeForwardError::CircuitOpen);
        }
    }

    let _tunnel_permit = acquire_tunnel_permit(state, tunnel_block, fr.deadline_unix_ms).await?;

    let n = state.tunnel_pool.slots.len().max(1);
    let idx = state.tunnel_pool.rr.fetch_add(1, Ordering::Relaxed) % n;
    let ch = grpc_channel_metric_label(idx);
    metrics::counter!("bridge_grpc_channel_forward_total", "channel" => ch).increment(1);

    let mut client = {
        let guard = state.tunnel_pool.slots[idx].read().await;
        guard.clone()
    };
    let request_id = fr.request_id.clone();
    let attempt_id = fr.attempt_id.clone();
    let deadline_unix_ms = fr.deadline_unix_ms;
    let trace_id = header_value_case_insensitive(&fr.headers, "x-request-id")
        .or_else(|| header_value_case_insensitive(&fr.headers, "x-trace-id"))
        .unwrap_or_else(|| request_id.clone());
    let remaining = remaining_until(fr.deadline_unix_ms).ok_or_else(|| {
        BridgeForwardError::DeadlineExceeded("request deadline exceeded before gRPC".into())
    })?;
    let mut request = tonic::Request::new(fr);
    request.set_timeout(remaining);
    let rpc = tokio::time::timeout(remaining, client.forward(request)).await;

    match rpc {
        Ok(Ok(resp)) => {
            if let Some(cb) = state.forward_circuit.as_ref() {
                cb.record_success();
            }
            Ok(resp.into_inner())
        }
        failed => {
            let (detail, deadline_exceeded, reconnect, unknown_outcome) = match failed {
                Ok(Err(error)) => {
                    let unknown_outcome = unknown_outcome_metadata(&error);
                    (
                        error.to_string(),
                        error.code() == tonic::Code::DeadlineExceeded,
                        error.code() == tonic::Code::Unavailable,
                        unknown_outcome,
                    )
                }
                Err(_) => (
                    "forward rpc deadline exceeded".to_string(),
                    true,
                    false,
                    None,
                ),
                Ok(Ok(_)) => unreachable!(),
            };
            metrics::counter!("bridge_grpc_channel_forward_err_total", "channel" => ch)
                .increment(1);
            metrics::counter!(
                "bridge_forward_error_total",
                "layer" => "bridge_grpc",
                "reason" => if deadline_exceeded { "deadline" } else { "transport" }
            )
            .increment(1);
            if let Some(cb) = state.forward_circuit.as_ref() {
                cb.record_full_failure();
            }
            warn!(
                %request_id,
                %attempt_id,
                %trace_id,
                deadline_unix_ms,
                %detail,
                "bridge forward failed"
            );

            // Reconnect only prepares a later logical request. Retrying this
            // request here can duplicate a committed mutating operation.
            if reconnect {
                let pool = state.tunnel_pool.clone();
                let cfg = state.tunnel_cfg.clone();
                tokio::spawn(async move {
                    pool.replace_slot(idx, &cfg).await;
                });
            }
            if let Some((attempt_id, stream_epoch)) = unknown_outcome {
                Err(BridgeForwardError::OutcomeUnknown {
                    detail,
                    attempt_id,
                    stream_epoch,
                })
            } else if deadline_exceeded {
                Err(BridgeForwardError::DeadlineExceeded(detail))
            } else {
                Err(BridgeForwardError::Tunnel(format!(
                    "tunnel forward failed: {detail}"
                )))
            }
        }
    }
}

fn outcome_unknown_http(
    detail: String,
    attempt_id: String,
    stream_epoch: String,
) -> Result<Response<Body>, (axum::http::StatusCode, String)> {
    metrics::counter!("bridge_forward_unknown_outcome_total").increment(1);
    let body = serde_json::to_string(&serde_json::json!({
        "error": "outcome_unknown",
        "detail": detail,
        "attempt_id": attempt_id,
        "stream_epoch": stream_epoch,
        "retry_policy": "do_not_automatically_redispatch_mutations"
    }))
    .map_err(|error| {
        (
            axum::http::StatusCode::INTERNAL_SERVER_ERROR,
            error.to_string(),
        )
    })?;
    let mut response = Response::builder()
        .status(axum::http::StatusCode::SERVICE_UNAVAILABLE)
        .header(axum::http::header::CONTENT_TYPE, "application/json")
        .header("x-sag-outcome", "unknown");
    if !attempt_id.is_empty() {
        response = response.header("x-sag-attempt-id", attempt_id);
    }
    if !stream_epoch.is_empty() {
        response = response.header("x-sag-stream-epoch", stream_epoch);
    }
    response.body(Body::from(body)).map_err(|error| {
        (
            axum::http::StatusCode::INTERNAL_SERVER_ERROR,
            error.to_string(),
        )
    })
}

/// Queue workers always block on the tunnel concurrency gate when enabled.
pub async fn forward_request(
    state: &AppState,
    fr: ForwardRequest,
) -> Result<ForwardResponse, BridgeForwardError> {
    forward_request_inner(state, fr, true).await
}

fn queue_unavailable_503(
    reason: &'static str,
) -> Result<Response<Body>, (axum::http::StatusCode, String)> {
    metrics::counter!("admission_rejected_total", "reason" => "queue_unavailable").increment(1);
    metrics::counter!("bridge_soft_enqueue_failure_503_total", "reason" => reason).increment(1);
    metrics::counter!("queue_dependency_unavailable_total", "operation" => reason).increment(1);
    let body = serde_json::json!({
        "error": "queue_unavailable",
        "reason": reason,
        "retry_after_sec": 5u64,
    });
    let body = serde_json::to_string(&body)
        .map_err(|e| (axum::http::StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    Response::builder()
        .status(axum::http::StatusCode::SERVICE_UNAVAILABLE)
        .header(axum::http::header::CONTENT_TYPE, "application/json")
        .header(axum::http::header::RETRY_AFTER, "5")
        .body(Body::from(body))
        .map_err(|e| (axum::http::StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))
}

fn http_app_rate_limited_response() -> Result<Response<Body>, (axum::http::StatusCode, String)> {
    metrics::counter!("bridge_http_app_ratelimit_reject_total").increment(1);
    let body = serde_json::json!({
        "error": "http_app_rate_limited",
        "retry_after_sec": 1u64,
    });
    let body = serde_json::to_string(&body)
        .map_err(|e| (axum::http::StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    Response::builder()
        .status(axum::http::StatusCode::TOO_MANY_REQUESTS)
        .header(axum::http::header::CONTENT_TYPE, "application/json")
        .header(axum::http::header::RETRY_AFTER, "1")
        .body(Body::from(body))
        .map_err(|e| (axum::http::StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))
}

fn forward_circuit_open_http(
    cooloff_ms: u64,
) -> Result<Response<Body>, (axum::http::StatusCode, String)> {
    metrics::counter!("bridge_forward_circuit_reject_http_total").increment(1);
    let retry_sec = cooloff_ms.div_ceil(1000).max(1);
    let body = serde_json::json!({
        "error": "forward_circuit_open",
        "retry_after_sec": retry_sec,
    });
    let body = serde_json::to_string(&body)
        .map_err(|e| (axum::http::StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    Response::builder()
        .status(axum::http::StatusCode::SERVICE_UNAVAILABLE)
        .header(axum::http::header::CONTENT_TYPE, "application/json")
        .header(axum::http::header::RETRY_AFTER, retry_sec.to_string())
        .body(Body::from(body))
        .map_err(|e| (axum::http::StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))
}

fn admission_unavailable_503(
    reason: &'static str,
) -> Result<Response<Body>, (axum::http::StatusCode, String)> {
    let body = serde_json::to_string(&serde_json::json!({
        "error": "admission_unavailable",
        "reason": reason,
        "retry_after_sec": 1u64,
    }))
    .map_err(|error| {
        (
            axum::http::StatusCode::INTERNAL_SERVER_ERROR,
            error.to_string(),
        )
    })?;
    Response::builder()
        .status(axum::http::StatusCode::SERVICE_UNAVAILABLE)
        .header(axum::http::header::CONTENT_TYPE, "application/json")
        .header(axum::http::header::RETRY_AFTER, "1")
        .body(Body::from(body))
        .map_err(|error| {
            (
                axum::http::StatusCode::INTERNAL_SERVER_ERROR,
                error.to_string(),
            )
        })
}

fn declared_content_length(
    headers: &axum::http::HeaderMap,
) -> Result<Option<u64>, (axum::http::StatusCode, String)> {
    let Some(value) = headers.get(axum::http::header::CONTENT_LENGTH) else {
        return Ok(None);
    };
    let length = value
        .to_str()
        .ok()
        .and_then(|raw| raw.parse::<u64>().ok())
        .ok_or_else(|| {
            (
                axum::http::StatusCode::BAD_REQUEST,
                "invalid Content-Length".into(),
            )
        })?;
    Ok(Some(length))
}

fn queue_accepted_202(
    fr: &ForwardRequest,
) -> Result<Response<Body>, (axum::http::StatusCode, String)> {
    let poll = format!("/__sag/queue/{}/status", fr.request_id);
    let body = serde_json::to_string(&QueuedAccepted {
        status: "queued",
        queue_id: fr.request_id.clone(),
        poll: poll.clone(),
    })
    .map_err(|e| (axum::http::StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    Response::builder()
        .status(axum::http::StatusCode::ACCEPTED)
        .header(axum::http::header::CONTENT_TYPE, "application/json")
        .header("X-SAG-Queue", "1")
        .header(axum::http::header::RETRY_AFTER, "0")
        .header(axum::http::header::LOCATION, &poll)
        .body(Body::from(body))
        .map_err(|e| (axum::http::StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))
}

enum QueueShedOutcome {
    Respond(Response<Body>),
    RetryReadOnlySync,
}

async fn shed_to_queue(
    state: &AppState,
    queue: &Arc<queue::QueueRuntime>,
    request: &ForwardRequest,
    tunnel_saturated: bool,
) -> Result<QueueShedOutcome, (axum::http::StatusCode, String)> {
    if request.body.len() > queue.cfg.max_body_bytes {
        metrics::counter!("bridge_queue_reject_total", "reason" => "body_too_large").increment(1);
        let body = serde_json::to_string(&serde_json::json!({
            "error": "payload_too_large_for_queue",
            "max_body_bytes": queue.cfg.max_body_bytes
        }))
        .map_err(|error| {
            (
                axum::http::StatusCode::INTERNAL_SERVER_ERROR,
                error.to_string(),
            )
        })?;
        let response = Response::builder()
            .status(axum::http::StatusCode::PAYLOAD_TOO_LARGE)
            .header(axum::http::header::CONTENT_TYPE, "application/json")
            .body(Body::from(body))
            .map_err(|error| {
                (
                    axum::http::StatusCode::INTERNAL_SERVER_ERROR,
                    error.to_string(),
                )
            })?;
        return Ok(QueueShedOutcome::Respond(response));
    }

    match queue.enqueue(request).await {
        Ok(()) => {
            if tunnel_saturated {
                metrics::counter!("bridge_tunnel_shed_to_queue_total").increment(1);
            }
            metrics::counter!("bridge_queue_202_total").increment(1);
            Ok(QueueShedOutcome::Respond(queue_accepted_202(request)?))
        }
        Err(queue::EnqueueError::OverCapacity) | Err(queue::EnqueueError::BodyTooLarge) => {
            metrics::counter!("bridge_queue_reject_total", "reason" => "queue_full").increment(1);
            metrics::counter!("admission_rejected_total", "reason" => "queue_full").increment(1);
            Ok(QueueShedOutcome::Respond(admission_unavailable_503(
                "queue_full",
            )?))
        }
        Err(queue::EnqueueError::Serialization) => {
            tracing::error!("queue payload serialization failed");
            Ok(QueueShedOutcome::Respond(queue_unavailable_503(
                "serialization",
            )?))
        }
        Err(queue::EnqueueError::Redis(error)) => {
            tracing::warn!(?error, "queue enqueue Redis error");
            if queue_error_may_use_sync_fallback(state.soft_enqueue_on_failure, &request.method) {
                metrics::counter!("bridge_soft_fallback_total", "reason" => "read_only_redis_enqueue")
                    .increment(1);
                Ok(QueueShedOutcome::RetryReadOnlySync)
            } else {
                Ok(QueueShedOutcome::Respond(queue_unavailable_503(
                    "redis_enqueue",
                )?))
            }
        }
    }
}

fn forward_response_to_http(
    tun: ForwardResponse,
) -> Result<Response<Body>, (axum::http::StatusCode, String)> {
    let ForwardResponse {
        status_code,
        headers,
        body,
        header_values,
        ..
    } = tun;
    let mut res = Response::builder().status(status_code as u16);
    if header_values.is_empty() {
        for (name, value) in sanitize_tunnel_headers(headers) {
            res = res.header(name, value);
        }
    } else {
        let connection_tokens = header_values
            .iter()
            .filter(|header| header.name.eq_ignore_ascii_case("connection"))
            .filter_map(|header| std::str::from_utf8(&header.value).ok())
            .flat_map(|value| value.split(','))
            .map(|value| value.trim().to_ascii_lowercase())
            .filter(|value| !value.is_empty())
            .collect::<HashSet<_>>();
        for header in header_values {
            let lower_name = header.name.to_ascii_lowercase();
            if is_hop_by_hop_header(&lower_name) || connection_tokens.contains(&lower_name) {
                continue;
            }
            let Ok(name) = axum::http::HeaderName::from_bytes(header.name.as_bytes()) else {
                continue;
            };
            let Ok(value) = axum::http::HeaderValue::from_bytes(&header.value) else {
                continue;
            };
            // `Builder::header` appends, preserving Set-Cookie and other multi-value fields.
            res = res.header(name, value);
        }
    }
    res.body(Body::from(body))
        .map_err(|e| (axum::http::StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))
}

async fn proxy(
    State(state): State<AppState>,
    req: Request,
) -> Result<Response<Body>, (axum::http::StatusCode, String)> {
    let Some(_active_request) = state.readiness.try_admit() else {
        return Err((
            axum::http::StatusCode::SERVICE_UNAVAILABLE,
            "bridge is draining".into(),
        ));
    };
    let t_rpc = Instant::now();
    let deadline_unix_ms = now_ms().saturating_add(state.forward_timeout_ms as i64);

    let app_id = req
        .headers()
        .get("x-sag-app-id")
        .and_then(|h| h.to_str().ok())
        .unwrap_or("")
        .to_string();
    if app_id.is_empty() {
        return Err((
            axum::http::StatusCode::BAD_REQUEST,
            "missing x-sag-app-id".into(),
        ));
    }

    if state.forward_circuit.as_ref().is_some_and(|c| c.is_open()) {
        return forward_circuit_open_http(state.forward_circuit_cooloff_ms.max(1000));
    }

    if let Some(lim) = state.app_rps_limiter.as_ref() {
        if !lim.try_acquire(&app_id) {
            return http_app_rate_limited_response();
        }
    }

    if declared_content_length(req.headers())?
        .is_some_and(|length| length > state.max_body_bytes as u64)
    {
        metrics::counter!("bridge_request_reject_total", "reason" => "body_too_large").increment(1);
        return Err((
            axum::http::StatusCode::PAYLOAD_TOO_LARGE,
            "request Content-Length exceeds SAG_BRIDGE_MAX_BODY_BYTES".into(),
        ));
    }
    let Some(_hard_ingress_permit) = state.hard_ingress.try_acquire() else {
        return admission_unavailable_503("hard_limit");
    };

    let method = req.method().as_str().to_string();
    let path = path_and_query_for_forward(req.uri());
    let mut raw_headers = HashMap::new();
    let sanitized_headers = sanitize_untrusted_headers(req.headers().clone());
    for (k, v) in &sanitized_headers {
        if let Ok(s) = v.to_str() {
            raw_headers.insert(k.to_string(), s.to_string());
        }
    }
    let caller_request_id = header_value_case_insensitive(&raw_headers, "x-request-id");
    let trace_id = caller_request_id
        .clone()
        .or_else(|| header_value_case_insensitive(&raw_headers, "x-trace-id"))
        .unwrap_or_else(|| Uuid::new_v4().to_string());

    let idempotency_key = if is_mutating_method(&method) {
        header_value_case_insensitive(&raw_headers, "idempotency-key")
            .or_else(|| header_value_case_insensitive(&raw_headers, "x-idempotency-key"))
            .ok_or_else(|| {
                metrics::counter!("bridge_request_reject_total", "reason" => "missing_idempotency_key")
                    .increment(1);
                (
                    axum::http::StatusCode::BAD_REQUEST,
                    "mutating requests require a stable Idempotency-Key".into(),
                )
            })?
    } else {
        String::new()
    };
    raw_headers
        .entry("x-request-id".into())
        .or_insert_with(|| trace_id.clone());
    if !idempotency_key.is_empty() {
        raw_headers
            .entry("idempotency-key".into())
            .or_insert_with(|| idempotency_key.clone());
    }
    let headers = sanitize_tunnel_headers(raw_headers);

    let remaining = remaining_until(deadline_unix_ms).ok_or_else(|| {
        (
            axum::http::StatusCode::REQUEST_TIMEOUT,
            "request deadline exceeded before body read".into(),
        )
    })?;
    let body = tokio::time::timeout(
        remaining,
        collect_body_limited(req.into_body(), state.max_body_bytes),
    )
    .await
    .map_err(|_| {
        metrics::counter!("bridge_request_reject_total", "reason" => "body_read_timeout")
            .increment(1);
        (
            axum::http::StatusCode::REQUEST_TIMEOUT,
            "request deadline exceeded while reading body".into(),
        )
    })?
    .map_err(|_| {
        metrics::counter!("bridge_request_reject_total", "reason" => "body_too_large").increment(1);
        (
            axum::http::StatusCode::PAYLOAD_TOO_LARGE,
            "request body exceeds SAG_BRIDGE_MAX_BODY_BYTES".into(),
        )
    })?;

    let request_id = Uuid::new_v4().to_string();
    let fr = ForwardRequest {
        request_id,
        app_id,
        method,
        path,
        headers,
        body,
        attempt_id: Uuid::new_v4().to_string(),
        deadline_unix_ms,
        idempotency_key,
        stream_epoch: String::new(),
    };

    let sync_permit = match state.sync_admission.try_acquire() {
        Some(permit) => permit,
        None => {
            metrics::counter!("bridge_soft_gate_entered_total").increment(1);
            let Some(queue) = state.queue.as_ref() else {
                return admission_unavailable_503("sync_limit");
            };
            match shed_to_queue(&state, queue, &fr, false).await? {
                QueueShedOutcome::Respond(response) => return Ok(response),
                QueueShedOutcome::RetryReadOnlySync => {
                    let Some(permit) = state.sync_admission.try_acquire() else {
                        return admission_unavailable_503("sync_limit");
                    };
                    permit
                }
            }
        }
    };

    let tun = match forward_request_inner(&state, fr.clone(), false).await {
        Ok(t) => t,
        Err(BridgeForwardError::CircuitOpen) => {
            std::mem::drop(sync_permit);
            return forward_circuit_open_http(state.forward_circuit_cooloff_ms.max(1000));
        }
        Err(BridgeForwardError::InflightSaturated) => {
            metrics::counter!("bridge_tunnel_try_saturated_total").increment(1);
            std::mem::drop(sync_permit);
            let Some(queue) = state.queue.as_ref() else {
                metrics::counter!("bridge_tunnel_saturated_503_total").increment(1);
                metrics::counter!("admission_rejected_total", "reason" => "tunnel_limit")
                    .increment(1);
                return admission_unavailable_503("tunnel_limit");
            };
            match shed_to_queue(&state, queue, &fr, true).await? {
                QueueShedOutcome::Respond(response) => return Ok(response),
                QueueShedOutcome::RetryReadOnlySync => {}
            }
            let Some(_retry_sync_permit) = state.sync_admission.try_acquire() else {
                return admission_unavailable_503("sync_limit");
            };
            match forward_request_inner(&state, fr, true).await {
                Ok(t) => t,
                Err(BridgeForwardError::CircuitOpen) => {
                    return forward_circuit_open_http(state.forward_circuit_cooloff_ms.max(1000));
                }
                Err(BridgeForwardError::Tunnel(msg)) => {
                    return Err((axum::http::StatusCode::BAD_GATEWAY, msg));
                }
                Err(BridgeForwardError::DeadlineExceeded(msg)) => {
                    return Err((axum::http::StatusCode::GATEWAY_TIMEOUT, msg));
                }
                Err(BridgeForwardError::OutcomeUnknown {
                    detail,
                    attempt_id,
                    stream_epoch,
                }) => {
                    return outcome_unknown_http(detail, attempt_id, stream_epoch);
                }
                Err(BridgeForwardError::InflightSaturated) => {
                    return Err((
                        axum::http::StatusCode::SERVICE_UNAVAILABLE,
                        "tunnel concurrent limit".into(),
                    ));
                }
            }
        }
        Err(BridgeForwardError::Tunnel(msg)) => {
            return Err((axum::http::StatusCode::BAD_GATEWAY, msg));
        }
        Err(BridgeForwardError::DeadlineExceeded(msg)) => {
            return Err((axum::http::StatusCode::GATEWAY_TIMEOUT, msg));
        }
        Err(BridgeForwardError::OutcomeUnknown {
            detail,
            attempt_id,
            stream_epoch,
        }) => {
            return outcome_unknown_http(detail, attempt_id, stream_epoch);
        }
    };

    tracing::debug!(
        rpc_ms = t_rpc.elapsed().as_millis(),
        status = tun.status_code,
        "http-tunnel-bridge forward rpc completed"
    );

    if tun.body.len() > state.max_response_body_bytes {
        metrics::counter!("admission_rejected_total", "reason" => "response_too_large")
            .increment(1);
        return admission_unavailable_503("response_too_large");
    }
    let mut response = forward_response_to_http(tun)?;
    if let Ok(value) = axum::http::HeaderValue::from_str(&trace_id) {
        response.headers_mut().insert("x-request-id", value);
    }
    Ok(response)
}

async fn connect_tunnel_client(
    cfg: &TunnelClientConfig,
) -> anyhow::Result<TunnelServiceClient<Channel>> {
    let connect_timeout_ms = env_u64("SAG_GRPC_CONNECT_TIMEOUT_MS", 5000).max(1000);
    // Default aligned with docker-compose.edge (must be >= forward RPC body timeout).
    let rpc_timeout_ms = env_u64("SAG_GRPC_RPC_TIMEOUT_MS", 120_000).max(5000);
    let mut ep = Endpoint::from_shared(cfg.endpoint.clone())?
        .connect_timeout(Duration::from_millis(connect_timeout_ms))
        .timeout(Duration::from_millis(rpc_timeout_ms))
        .http2_keep_alive_interval(Duration::from_millis(cfg.keepalive_ms.max(1000)))
        .keep_alive_timeout(Duration::from_millis(cfg.keepalive_timeout_ms.max(1000)))
        .keep_alive_while_idle(true)
        .tcp_keepalive(Some(Duration::from_millis(cfg.tcp_keepalive_ms.max(1000))));

    if cfg.tls_enabled {
        let cert = tokio::fs::read(&cfg.cert_p).await?;
        let key = tokio::fs::read(&cfg.key_p).await?;
        let ca = tokio::fs::read(&cfg.ca_p).await?;
        let mut tls = ClientTlsConfig::new()
            .identity(Identity::from_pem(cert, key))
            .ca_certificate(Certificate::from_pem(ca));
        if let Some(name) = cfg.tls_server_name.as_deref() {
            let name = name.trim();
            if !name.is_empty() {
                tls = tls.domain_name(name.to_string());
            }
        }
        ep = ep.tls_config(tls)?;
    }

    let channel = ep.connect().await?;
    Ok(TunnelServiceClient::new(channel))
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let _ = rustls::crypto::ring::default_provider().install_default();

    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    let prom = metrics_exporter_prometheus::PrometheusBuilder::new()
        .install_recorder()
        .map_err(|e| anyhow::anyhow!("install prometheus recorder failed: {e}"))?;
    let agent = std::env::var("SAG_TUNNEL_GRPC_ENDPOINT").unwrap_or_else(|_| {
        if std::env::var("SAG_DOCKER_COMPOSE").ok().as_deref() == Some("1") {
            "https://stealth-tunnel-agent:50051".into()
        } else {
            "https://127.0.0.1:50051".into()
        }
    });
    let tls_enabled = std::env::var("SAG_GRPC_MTLS_ENABLED")
        .map(|v| v != "false" && v != "0")
        .unwrap_or(true);
    let tls_server_name = std::env::var("SAG_GRPC_TLS_SERVER_NAME").ok();
    let cert_p = if tls_enabled {
        std::env::var("SAG_GRPC_TLS_CLIENT_CERT").map_err(|_| {
            anyhow::anyhow!("SAG_GRPC_TLS_CLIENT_CERT is required when mTLS is enabled")
        })?
    } else {
        String::new()
    };
    let key_p = if tls_enabled {
        std::env::var("SAG_GRPC_TLS_CLIENT_KEY").map_err(|_| {
            anyhow::anyhow!("SAG_GRPC_TLS_CLIENT_KEY is required when mTLS is enabled")
        })?
    } else {
        String::new()
    };
    let ca_p = if tls_enabled {
        std::env::var("SAG_GRPC_TLS_CA")
            .map_err(|_| anyhow::anyhow!("SAG_GRPC_TLS_CA is required when mTLS is enabled"))?
    } else {
        String::new()
    };
    let keepalive_ms = std::env::var("SAG_GRPC_KEEPALIVE_MS")
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
        .unwrap_or(10_000);
    let keepalive_timeout_ms = std::env::var("SAG_GRPC_KEEPALIVE_TIMEOUT_MS")
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
        .unwrap_or(5_000);
    let tcp_keepalive_ms = std::env::var("SAG_GRPC_TCP_KEEPALIVE_MS")
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
        .unwrap_or(10_000);
    let forward_timeout_ms = std::env::var("SAG_BRIDGE_FORWARD_TIMEOUT_MS")
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
        .unwrap_or(60_000);
    let rpc_deadline_ms = env_u64("SAG_GRPC_RPC_TIMEOUT_MS", 120_000).max(5000);
    if rpc_deadline_ms < forward_timeout_ms {
        warn!(
            rpc_deadline_ms = rpc_deadline_ms,
            forward_timeout_ms = forward_timeout_ms,
            "SAG_GRPC_RPC_TIMEOUT_MS < SAG_BRIDGE_FORWARD_TIMEOUT_MS: tonic unary Forward may be cut before bridge forward timeout (client may see HTTP 502)"
        );
    }
    let tunnel_cfg = TunnelClientConfig {
        endpoint: agent.clone(),
        tls_enabled,
        tls_server_name: tls_server_name.clone(),
        cert_p,
        key_p,
        ca_p,
        keepalive_ms,
        keepalive_timeout_ms,
        tcp_keepalive_ms,
    };
    let pool_size = std::env::var("SAG_BRIDGE_GRPC_CHANNEL_POOL_SIZE")
        .ok()
        .and_then(|s| s.parse::<usize>().ok())
        .unwrap_or(1)
        .clamp(1, 32);
    let mut slot_clients: Vec<Arc<RwLock<TunnelServiceClient<Channel>>>> =
        Vec::with_capacity(pool_size);
    for i in 0..pool_size {
        let t_slot = Instant::now();
        let c = connect_tunnel_client(&tunnel_cfg).await?;
        info!(
            slot = i,
            connect_ms = t_slot.elapsed().as_millis(),
            pool_size,
            "http-tunnel-bridge grpc channel slot connected"
        );
        slot_clients.push(Arc::new(RwLock::new(c)));
    }
    let tunnel_pool = TunnelClientPool {
        slots: Arc::new(slot_clients),
        rr: Arc::new(AtomicUsize::new(0)),
    };
    let queue_rt = match queue::QueueConfig::from_env() {
        Some(cfg) => Some(queue::QueueRuntime::connect(cfg).await?),
        None => None,
    };
    let sync_limit = queue_rt
        .as_ref()
        .map(|queue| queue.cfg.soft_inflight)
        .unwrap_or_else(|| {
            std::env::var("SAG_BRIDGE_SOFT_INFLIGHT")
                .ok()
                .and_then(|value| value.parse::<usize>().ok())
                .unwrap_or(24)
        });
    let hard_limit = queue_rt
        .as_ref()
        .map(|queue| queue.cfg.hard_inflight)
        .unwrap_or_else(|| {
            std::env::var("SAG_BRIDGE_HARD_INFLIGHT")
                .ok()
                .and_then(|value| value.parse::<usize>().ok())
                .unwrap_or(128)
        });
    if sync_limit == 0 || hard_limit == 0 || sync_limit > hard_limit {
        anyhow::bail!(
            "Bridge admission limits must be non-zero and SAG_BRIDGE_SOFT_INFLIGHT ({sync_limit}) must not exceed SAG_BRIDGE_HARD_INFLIGHT ({hard_limit})"
        );
    }
    let hard_ingress =
        Arc::new(limits::AdmissionGate::new("hard_limit", hard_limit).map_err(anyhow::Error::msg)?);
    let sync_admission =
        Arc::new(limits::AdmissionGate::new("sync_limit", sync_limit).map_err(anyhow::Error::msg)?);
    info!(
        hard_limit,
        sync_limit, "http-tunnel-bridge exact ingress and synchronous admission semaphores enabled"
    );

    let max_tunnel_permits = std::env::var("SAG_BRIDGE_MAX_TUNNEL_INFLIGHT")
        .ok()
        .and_then(|s| s.parse::<usize>().ok())
        .unwrap_or_else(|| {
            queue_rt
                .as_ref()
                .map(|q| q.cfg.hard_inflight.clamp(1, 512))
                .unwrap_or(128)
        });
    let tunnel_inflight = if max_tunnel_permits == 0 {
        info!("http-tunnel-bridge: SAG_BRIDGE_MAX_TUNNEL_INFLIGHT=0 — tunnel concurrency gate disabled");
        None
    } else {
        let n = max_tunnel_permits.max(1);
        info!(
            permits = n,
            "http-tunnel-bridge: tunnel unary Forward concurrency capped (HTTP try + queue workers block)"
        );
        Some(Arc::new(Semaphore::new(n)))
    };

    if let Some(ref qr) = queue_rt {
        let redis_mode = qr.cfg.deployment_mode();
        let redis_endpoint = qr.cfg.safe_endpoint();
        info!(
            ?redis_mode,
            redis_endpoint = %redis_endpoint,
            redis_connect_timeout_ms = qr.cfg.redis_connect_timeout_ms,
            redis_command_timeout_ms = qr.cfg.redis_command_timeout_ms,
            redis_reconnect_retries = qr.cfg.redis_reconnect_retries,
            redis_reconnect_base_ms = qr.cfg.redis_reconnect_base_ms,
            redis_reconnect_max_ms = qr.cfg.redis_reconnect_max_ms,
            soft_inflight = qr.cfg.soft_inflight,
            hard_inflight = qr.cfg.hard_inflight,
            worker_concurrency = qr.cfg.worker_concurrency,
            max_tunnel_permits = max_tunnel_permits,
            reclaim_idle_ms = qr.cfg.reclaim_idle_ms,
            max_forward_deadline_ms = qr.cfg.max_forward_deadline_ms,
            reclaim_jitter_margin_ms = qr.cfg.reclaim_jitter_margin_ms,
            max_attempts = qr.cfg.max_attempts,
            key_prefix = %qr.cfg.key_prefix,
            "http-tunnel-bridge: Redis queue enabled; HTTP 202 when bridge_sync_inflight >= soft_inflight or tunnel saturated (see bridge_sync_inflight)"
        );
    } else {
        info!("http-tunnel-bridge: Redis queue disabled (unset SAG_BRIDGE_REDIS_URL); no 202 enqueue path");
    }

    let soft_enqueue_on_failure = soft_enqueue_on_failure_from_env();
    info!(
        ?soft_enqueue_on_failure,
        "http-tunnel-bridge queue failure policy (mutations always fail closed; read-only fallback requires explicit opt-in)"
    );

    let max_body_bytes = std::env::var("SAG_BRIDGE_MAX_BODY_BYTES")
        .ok()
        .and_then(|s| s.parse::<usize>().ok())
        .unwrap_or(1_048_576)
        .max(1);
    let max_response_body_bytes = std::env::var("SAG_BRIDGE_MAX_RESPONSE_BODY_BYTES")
        .ok()
        .and_then(|value| value.parse::<usize>().ok())
        .unwrap_or(1_048_576);
    let memory_budget_bytes = std::env::var("SAG_MEMORY_BUDGET_BYTES")
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .unwrap_or(512 * 1024 * 1024);
    let worker_concurrency = queue_rt
        .as_ref()
        .map(|queue| queue.cfg.worker_concurrency)
        .unwrap_or(0);
    let max_queue_body = queue_rt
        .as_ref()
        .map(|queue| queue.cfg.max_body_bytes)
        .unwrap_or(max_body_bytes);
    let memory_budget = validate_bridge_memory_budget(
        hard_limit,
        sync_limit,
        worker_concurrency,
        max_body_bytes,
        max_response_body_bytes,
        max_queue_body,
        memory_budget_bytes,
    )
    .map_err(anyhow::Error::msg)?;
    info!(
        max_body_bytes,
        max_response_body_bytes,
        memory_required_bytes = memory_budget.required_bytes,
        memory_allowed_bytes = memory_budget.allowed_bytes,
        "http-tunnel-bridge request/response bounds and memory budget enabled"
    );

    let http_rps_per_app = std::env::var("SAG_BRIDGE_HTTP_RPS_PER_APP")
        .ok()
        .and_then(|s| s.parse::<u64>().ok())
        .unwrap_or(0);
    let app_rps_limiter = if http_rps_per_app > 0 {
        info!(
            http_rps_per_app,
            "http-tunnel-bridge: per-app dataplane HTTP RPS limit (x-sag-app-id token bucket)"
        );
        Some(Arc::new(limits::AppRpsLimiter::new(http_rps_per_app)))
    } else {
        info!("http-tunnel-bridge: SAG_BRIDGE_HTTP_RPS_PER_APP=0 — no per-app HTTP RPS limit");
        None
    };

    let cb_threshold = env_u32("SAG_BRIDGE_FORWARD_CB_FAILURE_THRESHOLD", 0);
    let cb_cooloff_ms = env_u64("SAG_BRIDGE_FORWARD_CB_COOL_OFF_MS", 10_000);
    let (forward_circuit, forward_circuit_cooloff_ms) = if cb_threshold > 0 {
        info!(
            threshold = cb_threshold,
            cooloff_ms = cb_cooloff_ms,
            "http-tunnel-bridge: Unary Forward circuit breaker enabled (consecutive full failures)"
        );
        (
            Some(Arc::new(limits::ForwardCircuit::new(
                cb_threshold,
                cb_cooloff_ms,
            ))),
            cb_cooloff_ms,
        )
    } else {
        info!("http-tunnel-bridge: SAG_BRIDGE_FORWARD_CB_FAILURE_THRESHOLD=0 — no forward circuit breaker");
        (None, 0u64)
    };

    let store = build_store_from_env();
    ensure_store_schema(&store).await?;
    let audit_writer = AuditWriter::from_env(store.clone())?;
    let state = AppState {
        metrics: prom,
        tunnel_pool,
        tunnel_cfg,
        forward_timeout_ms,
        store,
        audit_writer,
        queue: queue_rt,
        hard_ingress,
        sync_admission,
        tunnel_inflight,
        soft_enqueue_on_failure,
        app_rps_limiter,
        forward_circuit,
        forward_circuit_cooloff_ms,
        max_body_bytes,
        max_response_body_bytes,
        readiness: Readiness::new(env_u64("SAG_READINESS_SUCCESS_THRESHOLD", 2).max(1) as usize),
    };

    if let Some(qr) = state.queue.clone() {
        let n = qr.cfg.worker_concurrency;
        let sem = Arc::new(tokio::sync::Semaphore::new(n.max(1)));
        for i in 0..n {
            let st = state.clone();
            let q2 = qr.clone();
            let sem2 = sem.clone();
            let consumer = format!("bridge-{}-{}", Uuid::new_v4(), i);
            tokio::spawn(queue::worker_loop(st, q2, sem2, consumer));
        }
        info!(workers = n, "http-tunnel-bridge queue workers started");
    }

    let listen = std::env::var("SAG_HTTP_LISTEN_ADDR").unwrap_or_else(|_| "0.0.0.0:9000".into());
    let addr: SocketAddr = listen.parse()?;

    let app = Router::new()
        .route("/metrics", axum::routing::get(metrics))
        .route("/live", get(live))
        .route("/ready", get(ready))
        .route("/health", get(ready))
        .route("/__sag/queue/:id/status", get(queue_status))
        .fallback(proxy)
        .layer(middleware::from_fn_with_state(state.clone(), metrics_mw))
        .with_state(state.clone());

    info!(%addr, "http-tunnel-bridge listening");
    let listener = tokio::net::TcpListener::bind(addr).await?;
    let (shutdown_tx, shutdown_rx) = tokio::sync::oneshot::channel::<()>();
    let mut server_task = tokio::spawn(async move {
        axum::serve(listener, app)
            .with_graceful_shutdown(async move {
                let _ = shutdown_rx.await;
            })
            .await
    });
    sag_service_health::shutdown_signal().await;
    state.readiness.begin_draining();
    let _ = shutdown_tx.send(());
    let drain_timeout = Duration::from_millis(env_u64("SAG_DRAIN_TIMEOUT_MS", 30_000));
    let drain_report = state.readiness.wait_for_drain(drain_timeout).await;
    if drain_report.timed_out {
        metrics::counter!("shutdown_drain_timeout_total").increment(1);
        warn!(
            remaining = drain_report.remaining,
            "request/queue drain deadline expired"
        );
    }
    let server_error = match tokio::time::timeout(drain_timeout, &mut server_task).await {
        Ok(Ok(Ok(()))) => None,
        Ok(Ok(Err(error))) => Some(anyhow::Error::from(error)),
        Ok(Err(error)) => Some(anyhow::Error::from(error)),
        Err(_) => {
            server_task.abort();
            metrics::counter!("shutdown_server_abort_total").increment(1);
            warn!(
                remaining = state.readiness.active(),
                "server drain forced to abort"
            );
            None
        }
    };
    let audit_report = state.audit_writer.shutdown().await;
    if audit_report.dropped > 0 {
        warn!(
            dropped = audit_report.dropped,
            timed_out = audit_report.timed_out,
            "audit writer did not drain completely during shutdown"
        );
    }
    if let Some(error) = server_error {
        return Err(error);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn forward_path_preserves_encoded_and_repeated_query_parameters() {
        let uri: axum::http::Uri = "/search?q=a%20b&tag=one&tag=two&empty=".parse().unwrap();
        assert_eq!(
            path_and_query_for_forward(&uri),
            "/search?q=a%20b&tag=one&tag=two&empty="
        );
    }

    #[test]
    fn forward_path_defaults_to_root() {
        let uri: axum::http::Uri = "/".parse().unwrap();
        assert_eq!(path_and_query_for_forward(&uri), "/");
    }

    #[test]
    fn only_read_only_methods_skip_idempotency() {
        for method in ["GET", "HEAD", "OPTIONS", "TRACE", "get"] {
            assert!(!is_mutating_method(method), "{method}");
        }
        for method in ["POST", "PUT", "PATCH", "DELETE", "CONNECT"] {
            assert!(is_mutating_method(method), "{method}");
        }
    }

    #[test]
    fn queue_dependency_failure_never_falls_back_for_mutations() {
        for method in ["POST", "PUT", "PATCH", "DELETE"] {
            assert!(!queue_error_may_use_sync_fallback(
                SoftEnqueueOnFailure::ReadOnlyFallback,
                method
            ));
        }
        assert!(queue_error_may_use_sync_fallback(
            SoftEnqueueOnFailure::ReadOnlyFallback,
            "GET"
        ));
        assert!(!queue_error_may_use_sync_fallback(
            SoftEnqueueOnFailure::ServiceUnavailable503,
            "GET"
        ));
    }

    #[tokio::test]
    async fn body_limit_accepts_exact_limit_and_rejects_larger_body() {
        assert_eq!(
            collect_body_limited(Body::from("abc"), 3).await.unwrap(),
            b"abc"
        );
        assert!(collect_body_limited(Body::from("abcd"), 3).await.is_err());
    }

    #[test]
    fn tunnel_headers_exclude_hop_by_hop_and_connection_named_headers() {
        let headers = HashMap::from([
            ("content-type".into(), "application/json".into()),
            ("x-sag-app-id".into(), "demo".into()),
            ("connection".into(), "x-remove-me".into()),
            ("x-remove-me".into(), "1".into()),
            ("transfer-encoding".into(), "chunked".into()),
        ]);

        assert_eq!(
            sanitize_tunnel_headers(headers),
            HashMap::from([
                ("content-type".into(), "application/json".into()),
                ("x-sag-app-id".into(), "demo".into()),
            ])
        );
    }

    #[test]
    fn sanitize_untrusted_headers_removes_reserved_identity_assertions() {
        let mut headers = axum::http::HeaderMap::new();
        headers.insert("X-SAG-User-ID", "forged-sag-user".parse().unwrap());
        headers.insert("x-sag-user-roles", "admin".parse().unwrap());
        headers.insert("X-SAG-Authenticated", "true".parse().unwrap());
        headers.insert("x-user-id", "forged-user".parse().unwrap());
        headers.insert("X-User-Roles", "boss".parse().unwrap());
        headers.insert("x-business-header", "preserved".parse().unwrap());

        let sanitized = sanitize_untrusted_headers(headers);

        for reserved in [
            "x-sag-user-id",
            "x-sag-user-roles",
            "x-sag-authenticated",
            "x-user-id",
            "x-user-roles",
        ] {
            assert!(sanitized.get(reserved).is_none(), "{reserved} was retained");
        }
        assert_eq!(
            sanitized
                .get("x-business-header")
                .and_then(|value| value.to_str().ok()),
            Some("preserved")
        );
    }

    #[test]
    fn admission_rejects_oversized_declared_body_before_collection() {
        let mut headers = axum::http::HeaderMap::new();
        headers.insert(axum::http::header::CONTENT_LENGTH, "1025".parse().unwrap());
        assert_eq!(declared_content_length(&headers).unwrap(), Some(1025));
        assert!(declared_content_length(&headers)
            .unwrap()
            .is_some_and(|length| length > 1024));

        headers.insert(
            axum::http::header::CONTENT_LENGTH,
            "invalid".parse().unwrap(),
        );
        assert!(declared_content_length(&headers).is_err());
    }

    #[test]
    fn memory_budget_rejects_unsafe_bridge_concurrency_products() {
        assert!(validate_bridge_memory_budget(
            128,
            24,
            16,
            1_048_576,
            1_048_576,
            262_144,
            512 * 1024 * 1024
        )
        .is_ok());
        assert!(validate_bridge_memory_budget(
            2048,
            2048,
            64,
            1_048_576,
            4_194_304,
            262_144,
            512 * 1024 * 1024
        )
        .is_err());
        assert!(validate_bridge_memory_budget(
            128,
            24,
            16,
            0,
            1_048_576,
            262_144,
            512 * 1024 * 1024
        )
        .is_err());
    }

    #[test]
    fn duplicate_tunnel_response_headers_are_appended() {
        let response = forward_response_to_http(ForwardResponse {
            status_code: 200,
            header_values: vec![
                sag_tunnel_proto::HttpHeader {
                    name: "set-cookie".into(),
                    value: b"a=1; Path=/".to_vec(),
                },
                sag_tunnel_proto::HttpHeader {
                    name: "set-cookie".into(),
                    value: b"b=2; Path=/".to_vec(),
                },
            ],
            ..Default::default()
        })
        .unwrap();
        let cookies = response
            .headers()
            .get_all("set-cookie")
            .iter()
            .map(|value| value.to_str().unwrap())
            .collect::<Vec<_>>();
        assert_eq!(cookies, vec!["a=1; Path=/", "b=2; Path=/"]);
    }

    #[test]
    fn stream_loss_metadata_is_classified_as_unknown_outcome() {
        let mut status = tonic::Status::unavailable("stream lost");
        status
            .metadata_mut()
            .insert("x-sag-outcome", "unknown".parse().unwrap());
        status
            .metadata_mut()
            .insert("x-sag-attempt-id", "attempt-1".parse().unwrap());
        status
            .metadata_mut()
            .insert("x-sag-stream-epoch", "epoch-1".parse().unwrap());

        assert_eq!(
            unknown_outcome_metadata(&status),
            Some(("attempt-1".into(), "epoch-1".into()))
        );
    }
}
