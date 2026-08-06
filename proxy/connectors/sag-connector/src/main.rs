use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, AtomicU8, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use std::time::Instant;

use futures::stream::{FuturesUnordered, StreamExt};
use reqwest::Client;
use reqwest::Method as ReqMethod;
use sag_runtime_budget::{MemoryBudget, ValidatedMemoryBudget};
use sag_service_health::Readiness;
use sag_tunnel_proto::{
    tunnel_message, tunnel_service_client::TunnelServiceClient, ConnectorHeartbeat,
    ConnectorRegister, ConnectorRegisterAck, ForwardAccepted, ForwardRequest, ForwardResponse,
    HealthProbe, HealthProbeAck, HttpHeader, TunnelMessage,
};
use tokio::sync::{mpsc, watch, Notify};
use tokio_stream::wrappers::ReceiverStream;
use tonic::transport::{Certificate, Channel, ClientTlsConfig, Endpoint, Identity};
use tracing::{debug, info, warn};
use uuid::Uuid;

const HEALTH_PROBE_CAPABILITY: &str = "health-probe-v1";

fn default_connector_endpoint(connector_id: &str) -> String {
    format!("{connector_id}:stream")
}

fn env_u64(key: &str, default: u64) -> u64 {
    std::env::var(key)
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
        .unwrap_or(default)
}

#[derive(Clone)]
struct ConnectorHealth {
    readiness: Readiness,
    acknowledged_sessions: Arc<AtomicUsize>,
    apisix_readiness_url: Arc<String>,
    client: Client,
}

impl ConnectorHealth {
    fn acknowledge(&self) -> AcknowledgedSession {
        self.acknowledged_sessions.fetch_add(1, Ordering::AcqRel);
        AcknowledgedSession {
            sessions: self.acknowledged_sessions.clone(),
        }
    }
}

struct AcknowledgedSession {
    sessions: Arc<AtomicUsize>,
}

impl Drop for AcknowledgedSession {
    fn drop(&mut self) {
        self.sessions.fetch_sub(1, Ordering::AcqRel);
    }
}

fn register_ack_matches(
    ack: &ConnectorRegisterAck,
    connector_id: &str,
    endpoint: &str,
    stream_epoch: &str,
) -> bool {
    ack.connector_id == connector_id && ack.endpoint == endpoint && ack.stream_epoch == stream_epoch
}

async fn health_live() -> axum::http::StatusCode {
    axum::http::StatusCode::OK
}

async fn health_ready(
    axum::extract::State(health): axum::extract::State<ConnectorHealth>,
) -> axum::http::StatusCode {
    let sessions = health.acknowledged_sessions.clone();
    let client = health.client.clone();
    let url = health.apisix_readiness_url.clone();
    let state = health
        .readiness
        .probe(
            Duration::from_millis(env_u64("SAG_READINESS_TIMEOUT_MS", 1_000).max(1)),
            async move {
                sessions.load(Ordering::Acquire) > 0
                    && client
                        .get(url.as_str())
                        .send()
                        .await
                        .is_ok_and(|response| response.status().is_success())
            },
        )
        .await;
    if state == sag_service_health::ReadyState::Ready {
        axum::http::StatusCode::OK
    } else {
        axum::http::StatusCode::SERVICE_UNAVAILABLE
    }
}

#[derive(Clone)]
struct ConnectorRuntimeLimits {
    max_inflight: usize,
    accept_queue: usize,
    stream_buffer: usize,
    max_request_body: usize,
    max_response_body: usize,
    memory_budget_bytes: u64,
    memory_required_bytes: u64,
    memory_allowed_bytes: u64,
}

impl ConnectorRuntimeLimits {
    fn from_env() -> anyhow::Result<Self> {
        let mut limits = Self {
            max_inflight: env_u64("SAG_CONNECTOR_MAX_INFLIGHT", 256)
                .try_into()
                .unwrap_or(usize::MAX),
            accept_queue: env_u64("SAG_CONNECTOR_ACCEPT_QUEUE", 256)
                .try_into()
                .unwrap_or(usize::MAX),
            stream_buffer: env_u64("SAG_CONNECTOR_STREAM_BUFFER", 128)
                .try_into()
                .unwrap_or(usize::MAX),
            max_request_body: env_u64("SAG_CONNECTOR_MAX_REQUEST_BODY_BYTES", 1_048_576)
                .try_into()
                .unwrap_or(usize::MAX),
            max_response_body: env_u64("SAG_CONNECTOR_MAX_RESPONSE_BODY_BYTES", 1_048_576)
                .try_into()
                .unwrap_or(usize::MAX),
            memory_budget_bytes: env_u64("SAG_MEMORY_BUDGET_BYTES", 1536 * 1024 * 1024),
            memory_required_bytes: 0,
            memory_allowed_bytes: 0,
        };
        let budget = validate_connector_memory_budget(
            limits.max_inflight,
            limits.accept_queue,
            limits.stream_buffer,
            limits.max_request_body,
            limits.max_response_body,
            limits.memory_budget_bytes,
        )
        .map_err(anyhow::Error::msg)?;
        limits.memory_required_bytes = budget.required_bytes;
        limits.memory_allowed_bytes = budget.allowed_bytes;
        Ok(limits)
    }
}

fn validate_connector_memory_budget(
    max_inflight: usize,
    accept_queue: usize,
    stream_buffer: usize,
    max_request_body: usize,
    max_response_body: usize,
    budget_bytes: u64,
) -> Result<ValidatedMemoryBudget, String> {
    MemoryBudget {
        budget_bytes,
        safety_factor_percent: 80,
        reserved_bytes: 64 * 1024 * 1024,
        ingress_concurrency: max_inflight as u64,
        max_request_body: max_request_body as u64,
        response_concurrency: max_inflight as u64,
        max_response_body: max_response_body as u64,
        queue_capacity: accept_queue as u64,
        max_enqueued_bytes: max_request_body as u64,
        stream_capacity: stream_buffer as u64,
        max_frame_bytes: max_request_body.max(max_response_body) as u64,
    }
    .validate()
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

fn collect_response_headers(
    source: &reqwest::header::HeaderMap,
) -> (HashMap<String, String>, Vec<HttpHeader>) {
    let mut legacy = HashMap::new();
    let mut values = Vec::new();
    for (name, value) in source.iter() {
        if is_hop_by_hop_header(name.as_str()) {
            continue;
        }
        values.push(HttpHeader {
            name: name.as_str().to_string(),
            value: value.as_bytes().to_vec(),
        });
        if let Ok(value) = value.to_str() {
            legacy
                .entry(name.as_str().to_string())
                .or_insert_with(|| value.to_string());
        }
    }
    (legacy, values)
}

/// Classify tunnel `run_tunnel_once` errors for `connector_tunnel_drop_total{class=...}`.
fn tunnel_error_class(err: &str) -> &'static str {
    let e = err.to_lowercase();
    if e.contains("h2 protocol") || e.contains("reading a body") {
        return "h2_body";
    }
    if e.contains("transport") {
        return "transport";
    }
    "other"
}

struct QueuedRequest {
    req: ForwardRequest,
    enqueued_at: Instant,
    cancel: Arc<CancelState>,
}

enum DispatcherJob {
    Forward(Box<QueuedRequest>),
    HealthProbe(HealthProbe),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
#[repr(u8)]
enum AttemptPhase {
    Reserved = 0,
    Accepted = 1,
    Executing = 2,
    Completed = 3,
    Cancelled = 4,
}

struct CancelState {
    cancelled: AtomicBool,
    phase: AtomicU8,
    notify: Notify,
}

impl Default for CancelState {
    fn default() -> Self {
        Self {
            cancelled: AtomicBool::new(false),
            phase: AtomicU8::new(AttemptPhase::Reserved as u8),
            notify: Notify::new(),
        }
    }
}

impl CancelState {
    fn advance(&self, phase: AttemptPhase) {
        self.phase.fetch_max(phase as u8, Ordering::AcqRel);
    }

    #[cfg(test)]
    fn phase(&self) -> AttemptPhase {
        match self.phase.load(Ordering::Acquire) {
            0 => AttemptPhase::Reserved,
            1 => AttemptPhase::Accepted,
            2 => AttemptPhase::Executing,
            3 => AttemptPhase::Completed,
            _ => AttemptPhase::Cancelled,
        }
    }

    fn cancel(&self) {
        self.cancelled.store(true, Ordering::Release);
        self.advance(AttemptPhase::Cancelled);
        self.notify.notify_waiters();
    }

    fn is_cancelled(&self) -> bool {
        self.cancelled.load(Ordering::Acquire)
    }

    async fn cancelled(&self) {
        loop {
            let notified = self.notify.notified();
            if self.is_cancelled() {
                return;
            }
            notified.await;
        }
    }
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

fn attempt_key(req: &ForwardRequest) -> String {
    if req.attempt_id.is_empty() {
        req.request_id.clone()
    } else {
        req.attempt_id.clone()
    }
}

fn error_response(
    req: &ForwardRequest,
    status_code: u32,
    message: impl Into<Vec<u8>>,
) -> ForwardResponse {
    ForwardResponse {
        request_id: req.request_id.clone(),
        status_code,
        headers: Default::default(),
        body: message.into(),
        attempt_id: attempt_key(req),
        header_values: Default::default(),
        stream_epoch: req.stream_epoch.clone(),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum OutboundSendError {
    Closed,
    TimedOut,
}

impl std::fmt::Display for OutboundSendError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Closed => formatter.write_str("outbound stream is closed"),
            Self::TimedOut => formatter.write_str("outbound stream send timed out"),
        }
    }
}

impl std::error::Error for OutboundSendError {}

/// Applies one bounded backpressure policy to every Connector -> Agent frame.
///
/// A full channel is allowed a short grace period to drain. If the gRPC request
/// body remains stalled for the whole period, the caller treats the session as
/// unusable instead of waiting forever while readiness still reports it as up.
async fn send_outbound_message(
    sender: &mpsc::Sender<TunnelMessage>,
    message: TunnelMessage,
    send_timeout: Duration,
) -> Result<(), OutboundSendError> {
    match tokio::time::timeout(send_timeout, sender.send(message)).await {
        Ok(Ok(())) => Ok(()),
        Ok(Err(_)) => Err(OutboundSendError::Closed),
        Err(_) => Err(OutboundSendError::TimedOut),
    }
}

async fn send_health_probe_ack(
    sender: &mpsc::Sender<TunnelMessage>,
    probe: HealthProbe,
    send_timeout: Duration,
) -> Result<(), OutboundSendError> {
    send_outbound_message(
        sender,
        TunnelMessage {
            payload: Some(tunnel_message::Payload::HealthProbeAck(HealthProbeAck {
                probe_id: probe.probe_id,
                stream_epoch: probe.stream_epoch,
                received_unix_ms: now_ms(),
            })),
        },
        send_timeout,
    )
    .await
}

async fn build_channel(
    endpoint: &str,
    tls_enabled: bool,
    tls_server_name: Option<String>,
    cert_p: &str,
    key_p: &str,
    ca_p: &str,
) -> anyhow::Result<Channel> {
    let keepalive_ms = env_u64("SAG_GRPC_KEEPALIVE_MS", 10_000).max(1000);
    let keepalive_timeout_ms = env_u64("SAG_GRPC_KEEPALIVE_TIMEOUT_MS", 5_000).max(1000);
    let tcp_keepalive_ms = env_u64("SAG_GRPC_TCP_KEEPALIVE_MS", 10_000).max(1000);
    let rpc_timeout_ms = env_u64("SAG_CONNECTOR_GRPC_CHANNEL_TIMEOUT_MS", 120_000).max(5000);
    let mut ep = Endpoint::from_shared(endpoint.to_string())?
        .connect_timeout(Duration::from_secs(5))
        .timeout(Duration::from_millis(rpc_timeout_ms))
        .http2_keep_alive_interval(Duration::from_millis(keepalive_ms))
        .keep_alive_timeout(Duration::from_millis(keepalive_timeout_ms))
        .keep_alive_while_idle(true)
        .tcp_keepalive(Some(Duration::from_millis(tcp_keepalive_ms)));

    if tls_enabled {
        let cert = tokio::fs::read(cert_p).await?;
        let key = tokio::fs::read(key_p).await?;
        let ca = tokio::fs::read(ca_p).await?;
        let mut tls = ClientTlsConfig::new()
            .identity(Identity::from_pem(cert, key))
            .ca_certificate(Certificate::from_pem(ca));
        if let Some(name) = tls_server_name.as_deref() {
            let name = name.trim();
            if !name.is_empty() {
                tls = tls.domain_name(name.to_string());
            }
        }
        ep = ep.tls_config(tls)?;
    }
    Ok(ep.connect().await?)
}

fn accept_queue_saturated_response(req: &ForwardRequest) -> ForwardResponse {
    let mut headers = HashMap::new();
    headers.insert("content-type".into(), "text/plain; charset=utf-8".into());
    headers.insert("retry-after".into(), "1".into());
    ForwardResponse {
        request_id: req.request_id.clone(),
        status_code: 503,
        headers,
        body: b"sag-connector: accept queue saturated (retry)".to_vec(),
        attempt_id: attempt_key(req),
        header_values: Default::default(),
        stream_epoch: req.stream_epoch.clone(),
    }
}

// These values form one connection attempt's immutable runtime configuration.
// A typed config object belongs to the later configuration-cleanup phase.
#[allow(clippy::too_many_arguments)]
async fn run_tunnel_once(
    endpoint: &str,
    capacity_divisor: usize,
    connector_id: &str,
    app_id: &str,
    external_host: &str,
    conn_ep: &str,
    apisix: &str,
    tls_enabled: bool,
    tls_server_name: Option<String>,
    cert_p: &str,
    key_p: &str,
    ca_p: &str,
    limits: &ConnectorRuntimeLimits,
    health: ConnectorHealth,
    mut shutdown: watch::Receiver<bool>,
) -> anyhow::Result<()> {
    let channel =
        build_channel(endpoint, tls_enabled, tls_server_name, cert_p, key_p, ca_p).await?;
    let mut client = TunnelServiceClient::new(channel);
    let stream_epoch = Uuid::new_v4().to_string();

    let http_timeout_ms = env_u64("SAG_CONNECTOR_HTTP_TIMEOUT_MS", 55_000).max(1000);
    let http = Client::builder()
        .timeout(Duration::from_millis(http_timeout_ms))
        .pool_idle_timeout(Duration::from_secs(60))
        .pool_max_idle_per_host(limits.max_inflight)
        .build()?;

    let stream_buf = limits
        .stream_buffer
        .checked_div(capacity_divisor.max(1))
        .unwrap_or(1)
        .max(1);
    let (out_tx, out_rx) = mpsc::channel::<TunnelMessage>(stream_buf);
    let outbound_send_timeout_ms = env_u64("SAG_CONNECTOR_OUTBOUND_SEND_TIMEOUT_MS", 2_000).max(1);
    let outbound_send_timeout = Duration::from_millis(outbound_send_timeout_ms);
    // Any one-way producer that discovers the Agent stream is no longer
    // writable must wake the owning tunnel loop. Otherwise the heartbeat task
    // or a dispatcher worker can fail while the main inbound loop remains
    // blocked and the Connector continues to advertise the session as up.
    let (fatal_tx, mut fatal_rx) = mpsc::unbounded_channel::<String>();
    send_outbound_message(
        &out_tx,
        TunnelMessage {
            payload: Some(tunnel_message::Payload::Register(ConnectorRegister {
                connector_id: connector_id.to_string(),
                app_id: app_id.to_string(),
                external_host: external_host.to_string(),
                endpoint: conn_ep.to_string(),
                stream_epoch: stream_epoch.clone(),
                capabilities: vec![HEALTH_PROBE_CAPABILITY.to_string()],
            })),
        },
        outbound_send_timeout,
    )
    .await
    .map_err(|error| anyhow::anyhow!("failed to send Connector registration: {error}"))?;

    let hb_interval_ms = env_u64("SAG_CONNECTOR_HEARTBEAT_INTERVAL_MS", 2000).max(200);
    let hb_tx = out_tx.clone();
    let hb_id = connector_id.to_string();
    let hb_ep = conn_ep.to_string();
    let hb_epoch = stream_epoch.clone();
    let heartbeat_fatal_tx = fatal_tx.clone();
    let heartbeat_task = tokio::spawn(async move {
        let mut iv = tokio::time::interval(Duration::from_millis(hb_interval_ms));
        loop {
            iv.tick().await;
            let ts = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_secs() as i64)
                .unwrap_or(0);
            if let Err(error) = send_outbound_message(
                &hb_tx,
                TunnelMessage {
                    payload: Some(tunnel_message::Payload::Heartbeat(ConnectorHeartbeat {
                        connector_id: hb_id.clone(),
                        endpoint: hb_ep.clone(),
                        unix_ts: ts,
                        stream_epoch: hb_epoch.clone(),
                    })),
                },
                outbound_send_timeout,
            )
            .await
            {
                let _ = heartbeat_fatal_tx.send(format!("heartbeat stream send failed: {error}"));
                break;
            }
        }
    });

    let max_inflight = limits
        .max_inflight
        .checked_div(capacity_divisor.max(1))
        .unwrap_or(1)
        .max(1);
    let accept_queue_cap = limits
        .accept_queue
        .checked_div(capacity_divisor.max(1))
        .unwrap_or(1)
        .max(1);
    let max_response_body = limits.max_response_body as u64;
    let max_request_body = limits.max_request_body;

    let apisix_str = apisix.to_string();
    let (job_tx, mut job_rx) = mpsc::channel::<DispatcherJob>(accept_queue_cap);
    let cancellations = Arc::new(Mutex::new(HashMap::<String, Arc<CancelState>>::new()));
    let out_disp = out_tx.clone();
    let http_disp = http.clone();
    let apisix_disp = apisix_str.clone();
    let cid = connector_id.to_string();
    let cancellations_disp = cancellations.clone();
    let dispatcher_fatal_tx = fatal_tx.clone();
    type ForwardFut = Pin<Box<dyn Future<Output = ()> + Send>>;
    let dispatch = tokio::spawn(async move {
        let mut in_flight: FuturesUnordered<ForwardFut> = FuturesUnordered::new();
        let mut closed = false;
        loop {
            if closed && in_flight.is_empty() {
                break;
            }
            tokio::select! {
                Some(()) = in_flight.next(), if !in_flight.is_empty() => {}
                recv = job_rx.recv(), if !closed && in_flight.len() < max_inflight => {
                    match recv {
                        Some(DispatcherJob::Forward(q)) => {
                            let q = *q;
                            q.cancel.advance(AttemptPhase::Executing);
                            let out_i = out_disp.clone();
                            let http_i = http_disp.clone();
                            let apisix_i = apisix_disp.clone();
                            let cid_i = cid.clone();
                            let cancellations_i = cancellations_disp.clone();
                            let fatal_i = dispatcher_fatal_tx.clone();
                            in_flight.push(Box::pin(async move {
                                let attempt_id = attempt_key(&q.req);
                                let request_id = q.req.request_id.clone();
                                let trace_id = q
                                    .req
                                    .headers
                                    .get("x-request-id")
                                    .or_else(|| q.req.headers.get("x-trace-id"))
                                    .cloned()
                                    .unwrap_or_else(|| request_id.clone());
                                let accept_wait_s = q.enqueued_at.elapsed().as_secs_f64();
                                metrics::histogram!(
                                    "connector_forward_accept_wait_seconds",
                                    "connector" => cid_i.clone(),
                                )
                                .record(accept_wait_s);

                                let t_http = Instant::now();
                                let resp = if q.cancel.is_cancelled() {
                                    metrics::counter!("connector_forward_cancelled_total", "stage" => "accept_queue")
                                        .increment(1);
                                    error_response(&q.req, 499, "request cancelled before APISIX dispatch")
                                } else if q.req.deadline_unix_ms > 0
                                    && remaining_until(q.req.deadline_unix_ms).is_none()
                                {
                                    metrics::counter!("connector_forward_deadline_total", "stage" => "accept_queue")
                                        .increment(1);
                                    error_response(&q.req, 504, "request deadline expired in connector queue")
                                } else {
                                    handle_forward(
                                        &http_i,
                                        apisix_i.as_str(),
                                        q.req,
                                        max_response_body,
                                        q.cancel.clone(),
                                    )
                                    .await
                                };
                                let status_code = resp.status_code;
                                q.cancel.advance(AttemptPhase::Completed);
                                let upstream_s = t_http.elapsed().as_secs_f64();
                                metrics::histogram!(
                                    "connector_forward_upstream_seconds",
                                    "connector" => cid_i.clone(),
                                    "status" => status_code.to_string(),
                                )
                                .record(upstream_s);

                                {
                                    let mut active = cancellations_i
                                        .lock()
                                        .expect("connector cancellation map poisoned");
                                    if active
                                        .get(&attempt_id)
                                        .is_some_and(|current| Arc::ptr_eq(current, &q.cancel))
                                    {
                                        active.remove(&attempt_id);
                                    }
                                }

                                let t_send = Instant::now();
                                let send_result = send_outbound_message(
                                    &out_i,
                                    TunnelMessage {
                                        payload: Some(tunnel_message::Payload::Response(resp)),
                                    },
                                    outbound_send_timeout,
                                )
                                .await;
                                let send_ok = send_result.is_ok();
                                if let Err(error) = send_result {
                                    let _ = fatal_i.send(format!(
                                        "response stream send failed for attempt {attempt_id}: {error}"
                                    ));
                                }
                                let send_s = t_send.elapsed().as_secs_f64();
                                metrics::histogram!(
                                    "connector_forward_out_send_seconds",
                                    "connector" => cid_i,
                                )
                                .record(send_s);

                                tracing::debug!(
                                    accept_wait_s,
                                    upstream_s,
                                    send_s,
                                    status = status_code,
                                    out_ok = send_ok,
                                    %request_id,
                                    %attempt_id,
                                    %trace_id,
                                    "sag-connector forward response emitted"
                                );
                            }));
                        }
                        Some(DispatcherJob::HealthProbe(probe)) => {
                            let out_i = out_disp.clone();
                            let fatal_i = dispatcher_fatal_tx.clone();
                            in_flight.push(Box::pin(async move {
                                let probe_id = probe.probe_id.clone();
                                if let Err(error) = send_health_probe_ack(
                                    &out_i,
                                    probe,
                                    outbound_send_timeout,
                                ).await {
                                    let _ = fatal_i.send(format!(
                                        "health probe ACK stream send failed for probe {probe_id}: {error}"
                                    ));
                                } else {
                                    metrics::counter!("connector_health_probe_total", "result" => "ok")
                                        .increment(1);
                                }
                            }));
                        }
                        None => closed = true,
                    }
                }
            }
        }
    });

    // Everything after the background tasks start runs inside this result
    // boundary. `?` and `bail!` now leave only the inner future; the common
    // cancellation/drain epilogue below always executes before we return.
    let terminal_result: anyhow::Result<()> = async {
        let mut inbound = client
            .create_tunnel(ReceiverStream::new(out_rx))
            .await?
            .into_inner();

        let ack_timeout = Duration::from_millis(
            env_u64("SAG_CONNECTOR_REGISTER_ACK_TIMEOUT_MS", 5_000).max(100),
        );
        let first = tokio::time::timeout(ack_timeout, inbound.next())
            .await
            .map_err(|_| anyhow::anyhow!("timed out waiting for matching RegisterAck"))?
            .ok_or_else(|| anyhow::anyhow!("Agent stream ended before RegisterAck"))??;
        match first.payload {
            Some(tunnel_message::Payload::RegisterAck(ack))
                if register_ack_matches(&ack, connector_id, conn_ep, &stream_epoch) => {}
            _ => anyhow::bail!("Agent did not send a matching RegisterAck as the first message"),
        }
        let _acknowledged_session = health.acknowledge();

        info!(
            endpoint = %endpoint,
            connector_id = %connector_id,
            apisix = %apisix_str,
            hb_interval_ms = %hb_interval_ms,
            stream_buf = %stream_buf,
            max_inflight = %max_inflight,
            accept_queue_cap = %accept_queue_cap,
            outbound_send_timeout_ms = %outbound_send_timeout_ms,
            http_timeout_ms = %http_timeout_ms,
            max_response_body = %max_response_body,
            max_request_body = %max_request_body,
            stream_epoch = %stream_epoch,
            "sag-connector tunnel acknowledged (APISIX required)"
        );
        let g = metrics::gauge!(
            "connector_tunnel_up",
            "connector" => connector_id.to_string(),
            "app_id" => app_id.to_string(),
            "agent_endpoint" => endpoint.to_string()
        );
        g.set(1.0);

        loop {
            let msg = tokio::select! {
                changed = shutdown.changed() => {
                    if changed.is_err() || *shutdown.borrow() {
                        break;
                    }
                    continue;
                }
                reason = fatal_rx.recv() => {
                    anyhow::bail!(
                        "Connector outbound stream failed: {}",
                        reason.unwrap_or_else(|| "fatal notification channel closed".into())
                    );
                }
                msg = inbound.next() => {
                    let Some(msg) = msg else { break; };
                    msg
                }
            };
            let msg = msg?;
            match msg.payload {
            Some(tunnel_message::Payload::Request(mut req)) => {
                if req.stream_epoch != stream_epoch {
                    anyhow::bail!("request stream_epoch does not match active Connector stream");
                }
                if req.attempt_id.is_empty() {
                    req.attempt_id = req.request_id.clone();
                }
                if req.body.len() > max_request_body {
                    metrics::counter!("connector_forward_reject_total", "connector" => connector_id.to_string(), "reason" => "request_body_too_large")
                        .increment(1);
                    let resp = error_response(
                        &req,
                        413,
                        format!(
                            "request body exceeds SAG_CONNECTOR_MAX_REQUEST_BODY_BYTES ({max_request_body})"
                        ),
                    );
                    send_outbound_message(
                        &out_tx,
                        TunnelMessage {
                            payload: Some(tunnel_message::Payload::Response(resp)),
                        },
                        outbound_send_timeout,
                    )
                    .await
                    .map_err(|error| {
                        anyhow::anyhow!("failed to send request-size rejection: {error}")
                    })?;
                    continue;
                }
                if req.deadline_unix_ms > 0 && remaining_until(req.deadline_unix_ms).is_none() {
                    metrics::counter!("connector_forward_deadline_total", "stage" => "tunnel_receive")
                        .increment(1);
                    let resp = error_response(
                        &req,
                        504,
                        "request deadline expired before connector enqueue",
                    );
                    send_outbound_message(
                        &out_tx,
                        TunnelMessage {
                            payload: Some(tunnel_message::Payload::Response(resp)),
                        },
                        outbound_send_timeout,
                    )
                    .await
                    .map_err(|error| {
                        anyhow::anyhow!("failed to send deadline rejection: {error}")
                    })?;
                    continue;
                }

                let key = attempt_key(&req);
                let cancel = Arc::new(CancelState::default());
                let duplicate = {
                    let mut active = cancellations
                        .lock()
                        .expect("connector cancellation map poisoned");
                    if active.contains_key(&key) {
                        true
                    } else {
                        active.insert(key.clone(), cancel.clone());
                        false
                    }
                };
                if duplicate {
                    metrics::counter!("connector_forward_reject_total", "connector" => connector_id.to_string(), "reason" => "duplicate_attempt")
                        .increment(1);
                    let resp = error_response(&req, 409, "duplicate connector attempt_id");
                    send_outbound_message(
                        &out_tx,
                        TunnelMessage {
                            payload: Some(tunnel_message::Payload::Response(resp)),
                        },
                        outbound_send_timeout,
                    )
                    .await
                    .map_err(|error| {
                        anyhow::anyhow!("failed to send duplicate-attempt rejection: {error}")
                    })?;
                    continue;
                }

                let accepted = ForwardAccepted {
                    request_id: req.request_id.clone(),
                    attempt_id: key.clone(),
                    stream_epoch: stream_epoch.clone(),
                };
                match job_tx.try_send(DispatcherJob::Forward(Box::new(QueuedRequest {
                    enqueued_at: Instant::now(),
                    req,
                    cancel,
                }))) {
                    Ok(()) => {
                        let state = cancellations
                            .lock()
                            .expect("connector cancellation map poisoned")
                            .get(&key)
                            .cloned();
                        if let Some(state) = state {
                            state.advance(AttemptPhase::Accepted);
                        }
                        send_outbound_message(
                            &out_tx,
                            TunnelMessage {
                                payload: Some(tunnel_message::Payload::Accepted(accepted)),
                            },
                            outbound_send_timeout,
                        )
                        .await
                        .map_err(|error| {
                            anyhow::anyhow!("failed to send ForwardAccepted: {error}")
                        })?;
                    }
                    Err(mpsc::error::TrySendError::Full(DispatcherJob::Forward(q))) => {
                        let q = *q;
                        metrics::counter!(
                            "connector_forward_reject_total",
                            "connector" => connector_id.to_string(),
                            "reason" => "accept_queue_full",
                        )
                        .increment(1);
                        cancellations
                            .lock()
                            .expect("connector cancellation map poisoned")
                            .remove(&attempt_key(&q.req));
                        let resp = accept_queue_saturated_response(&q.req);
                        send_outbound_message(
                            &out_tx,
                            TunnelMessage {
                                payload: Some(tunnel_message::Payload::Response(resp)),
                            },
                            outbound_send_timeout,
                        )
                        .await
                        .map_err(|error| {
                            anyhow::anyhow!("failed to send accept-queue rejection: {error}")
                        })?;
                    }
                    Err(mpsc::error::TrySendError::Full(DispatcherJob::HealthProbe(_))) => {
                        unreachable!("forward enqueue returned a health-probe job")
                    }
                    Err(mpsc::error::TrySendError::Closed(_)) => break,
                }
            }
            Some(tunnel_message::Payload::HealthProbe(probe)) => {
                if probe.stream_epoch != stream_epoch || probe.probe_id.is_empty() {
                    anyhow::bail!("health probe does not match active Connector stream");
                }
                match job_tx.try_send(DispatcherJob::HealthProbe(probe)) {
                    Ok(()) => {}
                    Err(mpsc::error::TrySendError::Full(_)) => {
                        // Intentionally do not bypass the dispatcher with an
                        // immediate ACK. The Agent timeout will revoke a
                        // Connector whose real accept/dispatch path is stuck.
                        metrics::counter!("connector_health_probe_total", "result" => "queue_full")
                            .increment(1);
                    }
                    Err(mpsc::error::TrySendError::Closed(_)) => break,
                }
            }
            Some(tunnel_message::Payload::Cancel(cancel)) => {
                if cancel.stream_epoch != stream_epoch {
                    anyhow::bail!("cancel stream_epoch does not match active Connector stream");
                }
                let key = if cancel.attempt_id.is_empty() {
                    cancel.request_id.clone()
                } else {
                    cancel.attempt_id.clone()
                };
                let state = cancellations
                    .lock()
                    .expect("connector cancellation map poisoned")
                    .get(&key)
                    .cloned();
                match state {
                    Some(state) => {
                        state.cancel();
                        metrics::counter!("connector_cancel_total", "result" => "matched")
                            .increment(1);
                        debug!(
                            request_id = %cancel.request_id,
                            attempt_id = %key,
                            reason = %cancel.reason,
                            "connector cancellation matched active attempt"
                        );
                    }
                    None => {
                        metrics::counter!("connector_cancel_total", "result" => "late")
                            .increment(1);
                        debug!(
                            request_id = %cancel.request_id,
                            attempt_id = %key,
                            reason = %cancel.reason,
                            "connector cancellation arrived after attempt completion"
                        );
                    }
                }
            }
                _ => anyhow::bail!("invalid Agent-to-Connector stream message"),
            }
        }
        Ok(())
    }
    .await;

    for state in cancellations
        .lock()
        .expect("connector cancellation map poisoned")
        .values()
    {
        state.cancel();
    }
    drop(job_tx);
    let drain_timeout =
        Duration::from_millis(env_u64("SAG_CONNECTOR_STREAM_DRAIN_TIMEOUT_MS", 2_000).max(1));
    let mut dispatch = dispatch;
    if tokio::time::timeout(drain_timeout, &mut dispatch)
        .await
        .is_err()
    {
        dispatch.abort();
        metrics::counter!("connector_stream_drain_timeout_total").increment(1);
        warn!(
            remaining = cancellations
                .lock()
                .expect("connector cancellation map poisoned")
                .len(),
            "connector stream drain deadline expired"
        );
    }
    heartbeat_task.abort();

    let g2 = metrics::gauge!(
        "connector_tunnel_up",
        "connector" => connector_id.to_string(),
        "app_id" => app_id.to_string(),
        "agent_endpoint" => endpoint.to_string()
    );
    g2.set(0.0);
    match terminal_result {
        Ok(()) => anyhow::bail!("tunnel stream ended"),
        Err(error) => Err(error),
    }
}

async fn handle_forward(
    client: &Client,
    apisix_base: &str,
    req: ForwardRequest,
    max_response_body: u64,
    cancel: Arc<CancelState>,
) -> ForwardResponse {
    let start = Instant::now();
    let rid = req.request_id.clone();
    let attempt_id = attempt_key(&req);
    let trace_id = req
        .headers
        .get("x-request-id")
        .or_else(|| req.headers.get("x-trace-id"))
        .cloned()
        .unwrap_or_else(|| rid.clone());
    let query = req.headers.get("x-sag-query").cloned().unwrap_or_default();
    let path = if query.is_empty() {
        req.path.clone()
    } else if req.path.contains('?') {
        format!("{}&{}", req.path, query)
    } else {
        format!("{}?{}", req.path, query)
    };

    let base = apisix_base.trim_end_matches('/');
    let url = format!("{}/{}", base, path.trim_start_matches('/'));
    let method = ReqMethod::from_bytes(req.method.as_bytes()).unwrap_or(ReqMethod::GET);
    let mut b = client.request(method, &url);
    for (k, v) in &req.headers {
        let kl = k.to_lowercase();
        if kl == "host" || kl == "content-length" || is_hop_by_hop_header(&kl) {
            continue;
        }
        b = b.header(k.as_str(), v);
    }
    if !req.idempotency_key.is_empty() {
        b = b.header("idempotency-key", &req.idempotency_key);
    }
    b = b.body(req.body.clone());

    let Some(remaining) = remaining_until(req.deadline_unix_ms) else {
        metrics::counter!("connector_forward_deadline_total", "stage" => "http_start").increment(1);
        warn!(
            request_id = %rid,
            %attempt_id,
            %trace_id,
            stage = "http_start",
            deadline_unix_ms = req.deadline_unix_ms,
            "request deadline exceeded in Connector"
        );
        return error_response(&req, 504, "request deadline expired before APISIX request");
    };
    b = b.timeout(remaining);

    let send_result = tokio::select! {
        _ = cancel.cancelled() => {
            metrics::counter!("connector_forward_cancelled_total", "stage" => "http_send")
                .increment(1);
            return error_response(&req, 499, "request cancelled during APISIX request");
        }
        result = b.send() => result,
    };

    let resp = match send_result {
        Ok(r) => {
            let status = u32::from(r.status().as_u16());
            let (mut headers, mut header_values) = collect_response_headers(r.headers());
            let mut body_stream = r.bytes_stream();
            let mut body = Vec::new();
            loop {
                let next = tokio::select! {
                    _ = cancel.cancelled() => {
                        metrics::counter!("connector_forward_cancelled_total", "stage" => "http_body")
                            .increment(1);
                        return error_response(&req, 499, "request cancelled while reading APISIX response body");
                    }
                    next = body_stream.next() => next,
                };
                match next {
                    Some(Ok(chunk)) => {
                        if max_response_body > 0
                            && body.len().saturating_add(chunk.len()) > max_response_body as usize
                        {
                            metrics::counter!("connector_forward_body_truncated_total")
                                .increment(1);
                            return error_response(
                                &req,
                                502,
                                format!(
                                    "sag-connector: response body exceeds SAG_CONNECTOR_MAX_RESPONSE_BODY_BYTES ({max_response_body})"
                                ),
                            );
                        }
                        body.extend_from_slice(&chunk);
                    }
                    Some(Err(error)) => {
                        let is_timeout = error.is_timeout();
                        metrics::counter!(
                            "connector_forward_body_error_total",
                            "reason" => if is_timeout { "timeout" } else { "read_error" }
                        )
                        .increment(1);
                        warn!(
                            request_id = %rid,
                            %attempt_id,
                            %trace_id,
                            stage = "http_body",
                            timeout = is_timeout,
                            %error,
                            "APISIX response body failed"
                        );
                        return error_response(
                            &req,
                            if is_timeout { 504 } else { 502 },
                            format!("upstream response body error: {error}"),
                        );
                    }
                    None => break,
                }
            }
            headers.insert("x-sag-connector".into(), "sag-connector".into());
            header_values.retain(|header| !header.name.eq_ignore_ascii_case("x-sag-connector"));
            header_values.push(HttpHeader {
                name: "x-sag-connector".into(),
                value: b"sag-connector".to_vec(),
            });
            let elapsed = start.elapsed().as_secs_f64();
            let c = metrics::counter!(
                "connector_forward_total",
                "connector" => "sag-connector",
                "status" => status.to_string()
            );
            c.increment(1);
            let h = metrics::histogram!(
                "connector_forward_duration_seconds",
                "connector" => "sag-connector",
                "status" => status.to_string()
            );
            h.record(elapsed);
            ForwardResponse {
                request_id: rid.clone(),
                status_code: status,
                headers,
                body,
                attempt_id: attempt_key(&req),
                header_values,
                stream_epoch: req.stream_epoch.clone(),
            }
        }
        Err(e) => {
            let is_timeout = e.is_timeout();
            metrics::counter!(
                "connector_forward_http_error_total",
                "reason" => if is_timeout { "timeout" } else { "send_error" }
            )
            .increment(1);
            warn!(
                request_id = %rid,
                %attempt_id,
                %trace_id,
                stage = "http_send",
                timeout = is_timeout,
                error = %e,
                "APISIX request failed"
            );
            error_response(
                &req,
                if is_timeout { 504 } else { 502 },
                format!("upstream error: {e}"),
            )
        }
    };
    resp
}

#[derive(Clone)]
struct ConnectorConnectionConfig {
    connector_id: String,
    app_id: String,
    external_host: String,
    connector_endpoint: String,
    apisix: String,
    tls_enabled: bool,
    tls_server_name: Option<String>,
    cert_path: String,
    key_path: String,
    ca_path: String,
    limits: ConnectorRuntimeLimits,
    health: ConnectorHealth,
}

async fn maintain_agent_tunnel(
    endpoint: String,
    capacity_divisor: usize,
    cfg: ConnectorConnectionConfig,
    mut shutdown: watch::Receiver<bool>,
) {
    let mut attempt: u64 = 0;
    loop {
        attempt = attempt.saturating_add(1);
        let res = run_tunnel_once(
            &endpoint,
            capacity_divisor,
            &cfg.connector_id,
            &cfg.app_id,
            &cfg.external_host,
            &cfg.connector_endpoint,
            &cfg.apisix,
            cfg.tls_enabled,
            cfg.tls_server_name.clone(),
            &cfg.cert_path,
            &cfg.key_path,
            &cfg.ca_path,
            &cfg.limits,
            cfg.health.clone(),
            shutdown.clone(),
        )
        .await;
        if *shutdown.borrow() {
            return;
        }
        match res {
            Ok(()) => attempt = 0,
            Err(error) => {
                metrics::counter!(
                    "connector_tunnel_reconnect_total",
                    "result" => "error",
                    "agent_endpoint" => endpoint.clone()
                )
                .increment(1);
                let error_text = error.to_string();
                let class = tunnel_error_class(&error_text);
                metrics::counter!(
                    "connector_tunnel_drop_total",
                    "class" => class,
                    "agent_endpoint" => endpoint.clone()
                )
                .increment(1);
                warn!(attempt, class, agent_endpoint = %endpoint, error = %error, "connector tunnel dropped");
                let base = (200u64 * (attempt.min(25))).min(5000);
                let jitter = attempt.wrapping_mul(173) % 250;
                let backoff_ms = (base + jitter).min(8000);
                tokio::select! {
                    _ = tokio::time::sleep(Duration::from_millis(backoff_ms)) => {}
                    changed = shutdown.changed() => {
                        if changed.is_err() || *shutdown.borrow() {
                            return;
                        }
                    }
                }
            }
        }
    }
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

    // Prometheus metrics server (separate from the tunnel stream).
    let metrics_addr =
        std::env::var("SAG_METRICS_LISTEN_ADDR").unwrap_or_else(|_| "0.0.0.0:9103".to_string());
    // IMPORTANT: `install_recorder()` only installs the recorder; it does NOT start an HTTP listener.
    // We need `build()` + spawn the exporter future to actually serve `/metrics`.
    let metrics_addr = metrics_addr.parse::<std::net::SocketAddr>()?;
    let (recorder, exporter) = metrics_exporter_prometheus::PrometheusBuilder::new()
        .with_http_listener(metrics_addr)
        .build()
        .map_err(|e| anyhow::anyhow!("build prometheus exporter failed: {e}"))?;
    metrics::set_global_recorder(recorder)
        .map_err(|e| anyhow::anyhow!("set global recorder failed: {e}"))?;
    tokio::spawn(exporter);
    info!(%metrics_addr, "metrics listening (/metrics)");

    let default_endpoint = std::env::var("SAG_TUNNEL_ENDPOINT").unwrap_or_else(|_| {
        if std::env::var("SAG_DOCKER_COMPOSE").ok().as_deref() == Some("1") {
            "https://stealth-tunnel-agent:50051".into()
        } else {
            "https://127.0.0.1:50051".into()
        }
    });
    let endpoint_list = std::env::var("SAG_TUNNEL_ENDPOINTS").unwrap_or(default_endpoint);
    let mut endpoints = endpoint_list
        .split(',')
        .map(str::trim)
        .filter(|endpoint| !endpoint.is_empty())
        .map(str::to_string)
        .collect::<Vec<_>>();
    endpoints.sort();
    endpoints.dedup();
    if endpoints.is_empty() {
        anyhow::bail!("SAG_TUNNEL_ENDPOINTS must contain at least one Agent endpoint");
    }
    let connector_id =
        std::env::var("SAG_CONNECTOR_ID").unwrap_or_else(|_| "connector-local-001".into());
    let app_id = std::env::var("SAG_APP_ID").unwrap_or_else(|_| "app-001".into());
    let external_host =
        std::env::var("SAG_EXTERNAL_HOST").unwrap_or_else(|_| "app.internal.com".into());
    let conn_ep = std::env::var("SAG_CONNECTOR_ENDPOINT")
        .unwrap_or_else(|_| default_connector_endpoint(&connector_id));
    let apisix = std::env::var("SAG_APISIX_BASE_URL").map_err(|_| {
        anyhow::anyhow!(
            "SAG_APISIX_BASE_URL is required: connector must forward to APISIX data plane (e.g. http://127.0.0.1:9080)"
        )
    })?;
    let apisix = apisix.trim().to_string();
    if apisix.is_empty() {
        anyhow::bail!("SAG_APISIX_BASE_URL must not be empty");
    }

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

    let limits = ConnectorRuntimeLimits::from_env()?;
    let readiness = Readiness::new(env_u64("SAG_READINESS_SUCCESS_THRESHOLD", 2).max(1) as usize);
    let health = ConnectorHealth {
        readiness: readiness.clone(),
        acknowledged_sessions: Arc::new(AtomicUsize::new(0)),
        apisix_readiness_url: Arc::new(
            std::env::var("SAG_APISIX_READINESS_URL")
                .unwrap_or_else(|_| format!("{}/apisix/status", apisix.trim_end_matches('/'))),
        ),
        client: Client::builder()
            .timeout(Duration::from_millis(
                env_u64("SAG_READINESS_TIMEOUT_MS", 1_000).max(1),
            ))
            .build()?,
    };
    let health_addr: std::net::SocketAddr = std::env::var("SAG_HEALTH_LISTEN_ADDR")
        .unwrap_or_else(|_| "0.0.0.0:9106".into())
        .parse()?;
    let health_listener = tokio::net::TcpListener::bind(health_addr).await?;
    let health_app = axum::Router::new()
        .route("/live", axum::routing::get(health_live))
        .route("/ready", axum::routing::get(health_ready))
        .route("/health", axum::routing::get(health_ready))
        .with_state(health.clone());
    let health_task = tokio::spawn(async move { axum::serve(health_listener, health_app).await });
    info!(%health_addr, "connector health listening (/live, /ready)");
    info!(
        max_inflight = limits.max_inflight,
        accept_queue = limits.accept_queue,
        stream_buffer = limits.stream_buffer,
        max_request_body = limits.max_request_body,
        max_response_body = limits.max_response_body,
        memory_required_bytes = limits.memory_required_bytes,
        memory_allowed_bytes = limits.memory_allowed_bytes,
        "sag-connector bounded data-plane memory budget enabled"
    );
    let connection_cfg = ConnectorConnectionConfig {
        connector_id,
        app_id,
        external_host,
        connector_endpoint: conn_ep,
        apisix,
        tls_enabled,
        tls_server_name,
        cert_path: cert_p,
        key_path: key_p,
        ca_path: ca_p,
        limits,
        health,
    };
    let capacity_divisor = endpoints.len();
    info!(
        agent_endpoints = ?endpoints,
        capacity_divisor,
        "starting one connector tunnel per explicit Agent endpoint"
    );
    let mut tasks = tokio::task::JoinSet::new();
    let (shutdown_tx, shutdown_rx) = watch::channel(false);
    for endpoint in endpoints {
        tasks.spawn(maintain_agent_tunnel(
            endpoint,
            capacity_divisor,
            connection_cfg.clone(),
            shutdown_rx.clone(),
        ));
    }
    tokio::select! {
        _ = sag_service_health::shutdown_signal() => {}
        result = tasks.join_next() => {
            readiness.begin_draining();
            health_task.abort();
            return match result {
                Some(Ok(())) => Err(anyhow::anyhow!("connector tunnel maintenance loop stopped unexpectedly")),
                Some(Err(error)) => Err(anyhow::anyhow!("connector tunnel maintenance task failed: {error}")),
                None => Err(anyhow::anyhow!("no connector tunnel maintenance tasks started")),
            };
        }
    }
    readiness.begin_draining();
    let _ = shutdown_tx.send(true);
    let drain_timeout = Duration::from_millis(env_u64("SAG_DRAIN_TIMEOUT_MS", 30_000).max(1));
    if tokio::time::timeout(drain_timeout, async {
        while tasks.join_next().await.is_some() {}
    })
    .await
    .is_err()
    {
        tasks.abort_all();
        metrics::counter!("shutdown_drain_timeout_total").increment(1);
        warn!("connector shutdown drain deadline expired; remaining tasks aborted");
    }
    health_task.abort();
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn cancellation_wakes_all_waiters_and_is_sticky() {
        let state = Arc::new(CancelState::default());
        let waiter = state.clone();
        let task = tokio::spawn(async move { waiter.cancelled().await });
        state.cancel();
        tokio::time::timeout(Duration::from_millis(100), task)
            .await
            .expect("cancel waiter must wake")
            .unwrap();
        tokio::time::timeout(Duration::from_millis(10), state.cancelled())
            .await
            .expect("late waiter must observe sticky cancellation");
    }

    #[tokio::test]
    async fn outbound_send_succeeds_with_available_capacity() {
        let (tx, mut rx) = mpsc::channel(1);
        send_outbound_message(
            &tx,
            TunnelMessage { payload: None },
            Duration::from_millis(50),
        )
        .await
        .unwrap();

        assert!(rx.recv().await.is_some());
    }

    #[tokio::test]
    async fn outbound_send_reports_closed_stream() {
        let (tx, rx) = mpsc::channel(1);
        drop(rx);

        assert_eq!(
            send_outbound_message(
                &tx,
                TunnelMessage { payload: None },
                Duration::from_millis(50),
            )
            .await,
            Err(OutboundSendError::Closed)
        );
    }

    #[tokio::test]
    async fn outbound_send_times_out_when_channel_stays_full() {
        let (tx, mut rx) = mpsc::channel(1);
        tx.try_send(TunnelMessage { payload: None }).unwrap();

        assert_eq!(
            send_outbound_message(
                &tx,
                TunnelMessage { payload: None },
                Duration::from_millis(10),
            )
            .await,
            Err(OutboundSendError::TimedOut)
        );

        assert!(rx.recv().await.is_some());
        assert!(matches!(
            rx.try_recv(),
            Err(mpsc::error::TryRecvError::Empty)
        ));
    }

    #[tokio::test]
    async fn health_probe_crosses_dispatcher_queue_before_ack_without_http_upstream() {
        let (job_tx, mut job_rx) = mpsc::channel(1);
        let (out_tx, mut out_rx) = mpsc::channel(1);
        job_tx
            .try_send(DispatcherJob::HealthProbe(HealthProbe {
                probe_id: "probe-1".into(),
                stream_epoch: "epoch-1".into(),
                sent_unix_ms: now_ms(),
            }))
            .unwrap();

        let job = job_rx.recv().await.unwrap();
        let DispatcherJob::HealthProbe(probe) = job else {
            panic!("probe must remain a dedicated dispatcher job");
        };
        send_health_probe_ack(&out_tx, probe, Duration::from_millis(50))
            .await
            .unwrap();

        let ack = match out_rx.recv().await.unwrap().payload {
            Some(tunnel_message::Payload::HealthProbeAck(ack)) => ack,
            other => panic!("expected HealthProbeAck, got {other:?}"),
        };
        assert_eq!(ack.probe_id, "probe-1");
        assert_eq!(ack.stream_epoch, "epoch-1");
    }

    #[test]
    fn registration_ack_must_match_connector_endpoint_and_epoch() {
        let ack = ConnectorRegisterAck {
            connector_id: "connector".into(),
            endpoint: "connector:stream".into(),
            stream_epoch: "epoch-new".into(),
        };
        assert!(register_ack_matches(
            &ack,
            "connector",
            "connector:stream",
            "epoch-new"
        ));
        assert!(!register_ack_matches(
            &ack,
            "connector",
            "connector:stream",
            "epoch-old"
        ));
    }

    #[test]
    fn connector_attempt_phase_is_monotonic() {
        let state = CancelState::default();
        assert_eq!(state.phase(), AttemptPhase::Reserved);
        state.advance(AttemptPhase::Accepted);
        state.advance(AttemptPhase::Executing);
        state.advance(AttemptPhase::Accepted);
        assert_eq!(state.phase(), AttemptPhase::Executing);
        state.advance(AttemptPhase::Completed);
        assert_eq!(state.phase(), AttemptPhase::Completed);
    }

    #[test]
    fn attempt_key_never_uses_empty_key() {
        let mut request = ForwardRequest {
            request_id: "logical".into(),
            ..Default::default()
        };
        assert_eq!(attempt_key(&request), "logical");
        request.attempt_id = "transport-attempt".into();
        assert_eq!(attempt_key(&request), "transport-attempt");
    }

    #[test]
    fn memory_budget_rejects_unsafe_connector_queue_products() {
        assert!(validate_connector_memory_budget(
            256,
            256,
            128,
            1_048_576,
            1_048_576,
            1536 * 1024 * 1024,
        )
        .is_ok());
        assert!(validate_connector_memory_budget(
            4096,
            8192,
            32768,
            1_048_576,
            4_194_304,
            1536 * 1024 * 1024,
        )
        .is_err());
    }

    #[test]
    fn duplicate_response_headers_are_appended_in_wire_order() {
        let mut headers = reqwest::header::HeaderMap::new();
        headers.append("set-cookie", "a=1; Path=/".parse().unwrap());
        headers.append("set-cookie", "b=2; Path=/".parse().unwrap());
        headers.append("x-single", "value".parse().unwrap());

        let (legacy, values) = collect_response_headers(&headers);
        assert_eq!(
            legacy.get("set-cookie").map(String::as_str),
            Some("a=1; Path=/")
        );
        let cookies = values
            .iter()
            .filter(|header| header.name.eq_ignore_ascii_case("set-cookie"))
            .map(|header| String::from_utf8(header.value.clone()).unwrap())
            .collect::<Vec<_>>();
        assert_eq!(cookies, vec!["a=1; Path=/", "b=2; Path=/"]);
    }
}
