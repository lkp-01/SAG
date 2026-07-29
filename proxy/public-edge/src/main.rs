use std::convert::Infallible;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::{Duration, Instant};

use axum::extract::Request;
use axum::extract::State;
use axum::middleware::{self, Next};
use axum::response::Response;
use axum::routing::any;
use axum::Router;
use futures::StreamExt;
use metrics_exporter_prometheus::PrometheusHandle;
use sag_runtime_budget::MemoryBudget;
use sag_service_health::Readiness;
use shared_storage::{
    build_store_from_env, ensure_store_schema, AuditLogRecord, AuditWriter, FaultEventRecord,
    FaultEventsStore, StorageStore,
};
use tower_http::trace::TraceLayer;
use tracing::{info, warn};
use uuid::Uuid;

#[derive(Clone)]
struct PublicEdgeConfig {
    upstream_base_url: String,
    production_mode: bool,
    upstream_tls_insecure: bool,
    connect_timeout_ms: u64,
    first_byte_timeout_ms: u64,
    total_timeout_ms: u64,
    max_inflight: usize,
    max_request_body_bytes: usize,
    max_response_body_bytes: usize,
    memory_budget_bytes: u64,
    memory_reserved_bytes: u64,
    memory_safety_factor_percent: u8,
}

impl PublicEdgeConfig {
    fn from_env() -> Self {
        Self {
            upstream_base_url: std::env::var("PUBLIC_EDGE_UPSTREAM_BASE_URL")
                .unwrap_or_else(|_| "https://127.0.0.1:10080".into()),
            production_mode: env_bool("SAG_PRODUCTION_MODE", false),
            upstream_tls_insecure: env_bool("PUBLIC_EDGE_UPSTREAM_TLS_INSECURE", false),
            connect_timeout_ms: env_u64("PUBLIC_EDGE_CONNECT_TIMEOUT_MS", 3_000),
            first_byte_timeout_ms: env_u64("PUBLIC_EDGE_FIRST_BYTE_TIMEOUT_MS", 10_000),
            total_timeout_ms: env_u64("PUBLIC_EDGE_TOTAL_TIMEOUT_MS", 60_000),
            max_inflight: env_usize("PUBLIC_EDGE_MAX_INFLIGHT", 32),
            max_request_body_bytes: env_usize("PUBLIC_EDGE_MAX_REQUEST_BODY_BYTES", 1_048_576),
            max_response_body_bytes: env_usize("PUBLIC_EDGE_MAX_RESPONSE_BODY_BYTES", 4_194_304),
            memory_budget_bytes: env_u64("SAG_MEMORY_BUDGET_BYTES", 512 * 1024 * 1024),
            memory_reserved_bytes: env_u64("PUBLIC_EDGE_MEMORY_RESERVED_BYTES", 32 * 1024 * 1024),
            memory_safety_factor_percent: env_u64("SAG_MEMORY_SAFETY_FACTOR_PERCENT", 80)
                .try_into()
                .unwrap_or(0),
        }
    }

    fn validate(&self) -> anyhow::Result<sag_runtime_budget::ValidatedMemoryBudget> {
        if self.production_mode && self.upstream_tls_insecure {
            anyhow::bail!(
                "PUBLIC_EDGE_UPSTREAM_TLS_INSECURE cannot be enabled in SAG_PRODUCTION_MODE"
            );
        }
        if self.connect_timeout_ms == 0
            || self.first_byte_timeout_ms == 0
            || self.total_timeout_ms == 0
            || self.max_inflight == 0
        {
            anyhow::bail!("public-edge timeouts and concurrency must be greater than zero");
        }
        if self.first_byte_timeout_ms > self.total_timeout_ms {
            anyhow::bail!("PUBLIC_EDGE_FIRST_BYTE_TIMEOUT_MS must not exceed total timeout");
        }
        MemoryBudget {
            budget_bytes: self.memory_budget_bytes,
            safety_factor_percent: self.memory_safety_factor_percent,
            reserved_bytes: self.memory_reserved_bytes,
            ingress_concurrency: self.max_inflight as u64,
            max_request_body: self.max_request_body_bytes as u64,
            response_concurrency: self.max_inflight as u64,
            max_response_body: self.max_response_body_bytes as u64,
            queue_capacity: 0,
            max_enqueued_bytes: self.max_request_body_bytes as u64,
            stream_capacity: 0,
            max_frame_bytes: self.max_response_body_bytes as u64,
        }
        .validate()
        .map_err(anyhow::Error::msg)
    }

    #[cfg(test)]
    fn test_default() -> Self {
        Self {
            upstream_base_url: "https://upstream.example".into(),
            production_mode: false,
            upstream_tls_insecure: false,
            connect_timeout_ms: 100,
            first_byte_timeout_ms: 100,
            total_timeout_ms: 200,
            max_inflight: 2,
            max_request_body_bytes: 1024,
            max_response_body_bytes: 2048,
            memory_budget_bytes: 1024 * 1024,
            memory_reserved_bytes: 1024,
            memory_safety_factor_percent: 80,
        }
    }
}

#[derive(Clone)]
struct AppState {
    metrics: PrometheusHandle,
    store: StorageStore,
    audit_writer: AuditWriter,
    upstream_client: reqwest::Client,
    config: Arc<PublicEdgeConfig>,
    inflight: Arc<tokio::sync::Semaphore>,
    readiness: Readiness,
    readiness_urls: Arc<Vec<String>>,
}

fn env_bool(key: &str, default: bool) -> bool {
    std::env::var(key)
        .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
        .unwrap_or(default)
}

fn env_u64(key: &str, default: u64) -> u64 {
    std::env::var(key)
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(default)
}

fn env_usize(key: &str, default: usize) -> usize {
    std::env::var(key)
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(default)
}

fn error_response(status: u16, code: &'static str, message: impl std::fmt::Display) -> Response {
    let body = serde_json::json!({"error": code, "message": message.to_string()}).to_string();
    Response::builder()
        .status(status)
        .header("content-type", "application/json")
        .body(axum::body::Body::from(body))
        .expect("static public-edge error response")
}

fn response_content_length_exceeds(length: Option<u64>, max: usize) -> bool {
    length.is_some_and(|length| length > max as u64)
}

async fn forward_upstream(
    State(state): State<AppState>,
    req: Request,
) -> Result<Response, Infallible> {
    let Some(_active_request) = state.readiness.try_admit() else {
        return Ok(error_response(
            503,
            "draining",
            "public-edge is not accepting new requests",
        ));
    };
    let path_q = req
        .uri()
        .path_and_query()
        .map(|pq| pq.as_str())
        .unwrap_or("/");

    let url = format!(
        "{}{}",
        state.config.upstream_base_url.trim_end_matches('/'),
        path_q
    );

    let declared_length = req
        .headers()
        .get(axum::http::header::CONTENT_LENGTH)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.parse::<u64>().ok());
    if declared_length.is_some_and(|length| length > state.config.max_request_body_bytes as u64) {
        return Ok(error_response(
            413,
            "request_too_large",
            "Content-Length exceeds limit",
        ));
    }
    let permit = match state.inflight.clone().try_acquire_owned() {
        Ok(permit) => permit,
        Err(_) => {
            metrics::counter!("public_edge_admission_rejected_total", "reason" => "inflight_limit")
                .increment(1);
            return Ok(error_response(
                503,
                "over_capacity",
                "public-edge inflight limit reached",
            ));
        }
    };

    let method = reqwest::Method::from_bytes(req.method().as_str().as_bytes())
        .unwrap_or(reqwest::Method::GET);
    let mut upstream = state.upstream_client.request(method, &url);
    for (k, v) in req.headers().iter() {
        upstream = upstream.header(k, v);
    }
    let body = match tokio::time::timeout(
        Duration::from_millis(state.config.total_timeout_ms),
        axum::body::to_bytes(req.into_body(), state.config.max_request_body_bytes),
    )
    .await
    {
        Err(_) => {
            return Ok(error_response(
                504,
                "request_timeout",
                "request body read timed out",
            ))
        }
        Ok(Err(_)) => {
            return Ok(error_response(
                413,
                "request_too_large",
                "request body exceeds limit",
            ))
        }
        Ok(Ok(body)) => body,
    };
    upstream = upstream.body(body);

    let resp = match tokio::time::timeout(
        Duration::from_millis(state.config.first_byte_timeout_ms),
        upstream.send(),
    )
    .await
    {
        Err(_) => {
            return Ok(error_response(
                504,
                "upstream_timeout",
                "upstream first byte timed out",
            ))
        }
        Ok(Err(error)) if error.is_timeout() => {
            return Ok(error_response(504, "upstream_timeout", error))
        }
        Ok(Err(error)) => return Ok(error_response(502, "upstream", error)),
        Ok(Ok(response)) => response,
    };

    if response_content_length_exceeds(resp.content_length(), state.config.max_response_body_bytes)
    {
        metrics::counter!("public_edge_response_rejected_total", "reason" => "body_too_large")
            .increment(1);
        return Ok(error_response(
            503,
            "response_too_large",
            "upstream response exceeds limit",
        ));
    }

    let status = resp.status();
    let mut out = Response::builder().status(status.as_u16());
    for (k, v) in resp.headers().iter() {
        out = out.header(k, v);
    }
    let max_response_body = state.config.max_response_body_bytes;
    let body_stream = resp.bytes_stream().scan(
        (0usize, permit),
        move |(seen, _permit), item| {
            let result = match item {
                Ok(chunk) => match seen.checked_add(chunk.len()) {
                    Some(total) if total <= max_response_body => {
                        *seen = total;
                        Ok(chunk)
                    }
                    _ => {
                        metrics::counter!("public_edge_response_rejected_total", "reason" => "body_too_large")
                            .increment(1);
                        Err(std::io::Error::other("upstream response body exceeded configured limit"))
                    }
                },
                Err(error) => Err(std::io::Error::other(error)),
            };
            futures::future::ready(Some(result))
        },
    );
    Ok(out
        .body(axum::body::Body::from_stream(body_stream))
        .expect("validated upstream response"))
}

async fn metrics(State(state): State<AppState>) -> String {
    state.metrics.render()
}

async fn live() -> axum::http::StatusCode {
    axum::http::StatusCode::OK
}

async fn ready(State(state): State<AppState>) -> axum::http::StatusCode {
    let client = state.upstream_client.clone();
    let urls = state.readiness_urls.clone();
    let timeout = Duration::from_millis(env_u64("SAG_READINESS_PROBE_TIMEOUT_MS", 1_000));
    let status = state
        .readiness
        .probe(timeout, async move {
            for url in urls.iter() {
                if client
                    .get(url)
                    .send()
                    .await
                    .is_ok_and(|response| response.status().is_success())
                {
                    return true;
                }
            }
            false
        })
        .await;
    if status == sag_service_health::ReadyState::Ready {
        axum::http::StatusCode::OK
    } else {
        axum::http::StatusCode::SERVICE_UNAVAILABLE
    }
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

async fn metrics_mw(State(state): State<AppState>, req: Request, next: Next) -> Response {
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
    let latency_ms = start.elapsed().as_millis() as i64;

    let c = metrics::counter!(
        "http_requests_total",
        "service" => "public-edge",
        "method" => method.clone(),
        "path" => path.clone(),
        "status" => status.clone()
    );
    c.increment(1);
    let h = metrics::histogram!(
        "http_request_duration_seconds",
        "service" => "public-edge",
        "method" => method.clone(),
        "path" => path.clone(),
        "status" => status.clone()
    );
    h.record((latency_ms as f64) / 1000.0);

    let audit = AuditLogRecord {
        id: Uuid::new_v4().to_string(),
        ts_ms: now_ms(),
        service: "public-edge".into(),
        user_id,
        app_id,
        path: path.clone(),
        method: method.clone(),
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
            service: "public-edge".into(),
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
            path,
            method,
            latency_ms,
            baseline_ms: 300,
            threshold_ms: 1200,
            status_code,
            result: status,
            trace_id,
            source: "public_edge_metrics_mw".into(),
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

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    let prom = metrics_exporter_prometheus::PrometheusBuilder::new()
        .install_recorder()
        .map_err(|e| anyhow::anyhow!("install prometheus recorder failed: {e}"))?;
    let config = Arc::new(PublicEdgeConfig::from_env());
    let budget = config.validate()?;
    let upstream_client = reqwest::Client::builder()
        .danger_accept_invalid_certs(config.upstream_tls_insecure)
        .connect_timeout(Duration::from_millis(config.connect_timeout_ms))
        .timeout(Duration::from_millis(config.total_timeout_ms))
        .pool_idle_timeout(Duration::from_secs(60))
        .pool_max_idle_per_host(config.max_inflight)
        .build()?;
    info!(
        max_inflight = config.max_inflight,
        max_request_body_bytes = config.max_request_body_bytes,
        max_response_body_bytes = config.max_response_body_bytes,
        memory_required_bytes = budget.required_bytes,
        memory_allowed_bytes = budget.allowed_bytes,
        upstream_tls_verification = !config.upstream_tls_insecure,
        "public-edge bounded upstream client and memory budget enabled"
    );
    let store = build_store_from_env();
    ensure_store_schema(&store).await?;
    let audit_writer = AuditWriter::from_env(store.clone())?;
    let state = AppState {
        metrics: prom,
        store,
        audit_writer,
        upstream_client,
        config: config.clone(),
        inflight: Arc::new(tokio::sync::Semaphore::new(config.max_inflight)),
        readiness: Readiness::new(env_usize("SAG_READINESS_SUCCESS_THRESHOLD", 2)),
        readiness_urls: Arc::new(
            std::env::var("PUBLIC_EDGE_BRIDGE_READINESS_URLS")
                .unwrap_or_else(|_| {
                    format!("{}/ready", config.upstream_base_url.trim_end_matches('/'))
                })
                .split(',')
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(ToString::to_string)
                .collect(),
        ),
    };

    let listen =
        std::env::var("PUBLIC_EDGE_LISTEN_ADDR").unwrap_or_else(|_| "0.0.0.0:10443".into());
    let addr: SocketAddr = listen.parse()?;

    let app = Router::new()
        .route("/metrics", any(metrics))
        .route("/live", any(live))
        .route("/ready", any(ready))
        .route("/health", any(ready))
        .route("/", any(forward_upstream))
        .route("/*path", any(forward_upstream))
        .layer(TraceLayer::new_for_http())
        .layer(middleware::from_fn_with_state(state.clone(), metrics_mw))
        .with_state(state.clone());

    info!(
        %addr,
        "public-edge listening (HTTP proxy to Zentinel; add TLS termination via CDN or stunnel for production)"
    );
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
            "request drain deadline expired"
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
    fn memory_budget_and_tls_safety_are_startup_invariants() {
        let mut config = PublicEdgeConfig::test_default();
        assert!(config.validate().is_ok());

        config.production_mode = true;
        config.upstream_tls_insecure = true;
        assert!(config.validate().is_err());

        config.upstream_tls_insecure = false;
        config.max_request_body_bytes = 0;
        assert!(config.validate().is_err());

        config.max_request_body_bytes = 1024;
        config.memory_budget_bytes = 1;
        assert!(config.validate().is_err());
    }

    #[test]
    fn known_oversized_upstream_response_is_rejected_before_streaming() {
        assert!(response_content_length_exceeds(Some(1025), 1024));
        assert!(!response_content_length_exceeds(Some(1024), 1024));
        assert!(!response_content_length_exceeds(None, 1024));
    }
}
