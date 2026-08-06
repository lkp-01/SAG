use std::collections::{HashMap, HashSet};

use sag_runtime_budget::{MemoryBudget, ValidatedMemoryBudget};
use serde::Deserialize;

#[derive(Debug, Clone, Deserialize)]
pub struct StealthTunnelConfig {
    #[serde(default = "default_listen_addr")]
    pub listen_addr: String,
    /// URLs to `GET` tunnel routes from concurrently; the highest consistent
    /// generation wins. Built by [`control_plane_sync_endpoints_from_env`].
    #[serde(default = "default_control_plane_sync_endpoints")]
    pub control_plane_sync_endpoints: Vec<String>,
    #[serde(default = "default_sync_interval_ms")]
    pub sync_interval_ms: u64,
    #[serde(default = "default_forward_timeout_ms")]
    pub forward_timeout_ms: u64,
    #[serde(default = "default_max_pending_waiters")]
    pub max_pending_waiters: usize,
    #[serde(default = "default_stream_buffer")]
    pub stream_buffer: usize,
    #[serde(default = "default_max_body_bytes")]
    pub max_request_body_bytes: usize,
    #[serde(default = "default_max_body_bytes")]
    pub max_response_body_bytes: usize,
    #[serde(default = "default_memory_budget_bytes")]
    pub memory_budget_bytes: u64,
    #[serde(default)]
    pub memory_required_bytes: u64,
    #[serde(default)]
    pub memory_allowed_bytes: u64,
    pub policy_evaluate_endpoint: Option<String>,
    #[serde(default = "default_policy_evaluate_timeout_ms")]
    pub policy_evaluate_timeout_ms: u64,
    pub auth_verify_endpoint: Option<String>,
    /// Trust caller-supplied identity headers only when no auth service is configured.
    /// Disabled by default because public callers can forge these headers.
    #[serde(default)]
    pub trust_identity_headers: bool,
    #[serde(default = "default_auth_verify_timeout_ms")]
    pub auth_verify_timeout_ms: u64,
    #[serde(default = "default_policy_inflight_limit")]
    pub policy_inflight_limit: usize,
    #[serde(default = "default_auth_inflight_limit")]
    pub auth_inflight_limit: usize,
    #[serde(default = "default_negative_cache_enabled")]
    pub negative_cache_enabled: bool,
    #[serde(default = "default_negative_cache_ttl_sec")]
    pub negative_cache_ttl_sec: u64,
    /// Health window for connector heartbeats. If last heartbeat is older than this, agent treats tunnel unhealthy.
    #[serde(default = "default_tunnel_healthy_window_sec")]
    pub tunnel_healthy_window_sec: u64,
    /// Require a successful Agent -> Connector dispatcher -> Agent round trip
    /// in addition to a fresh heartbeat before assigning business traffic.
    #[serde(default = "default_connector_probe_enabled")]
    pub connector_probe_enabled: bool,
    #[serde(default = "default_connector_probe_interval_ms")]
    pub connector_probe_interval_ms: u64,
    #[serde(default = "default_connector_probe_timeout_ms")]
    pub connector_probe_timeout_ms: u64,
    #[serde(default = "default_connector_probe_freshness_ms")]
    pub connector_probe_freshness_ms: u64,
    #[serde(default = "default_connector_probe_startup_grace_ms")]
    pub connector_probe_startup_grace_ms: u64,
    #[serde(default = "default_connector_probe_failure_threshold")]
    pub connector_probe_failure_threshold: u8,
    #[serde(default = "default_grpc_tls_enabled")]
    pub grpc_tls_enabled: bool,
    pub grpc_tls_cert: Option<String>,
    pub grpc_tls_key: Option<String>,
    pub grpc_tls_client_ca: Option<String>,
    /// Require the presented mTLS client certificate to be explicitly bound to
    /// the Connector endpoint claimed by Register.
    #[serde(default)]
    pub require_connector_cert_binding: bool,
    #[serde(default)]
    pub connector_cert_bindings: HashMap<String, HashSet<String>>,
}

fn default_listen_addr() -> String {
    "0.0.0.0:50051".into()
}

fn default_control_plane_sync_endpoint() -> String {
    "http://127.0.0.1:8090/api/v1/agent/routes".into()
}

fn default_control_plane_sync_endpoints() -> Vec<String> {
    vec![default_control_plane_sync_endpoint()]
}

/// Resolves sync URLs from `SAG_CONTROL_PLANE_SYNC_ENDPOINT` (comma-separated allowed).
/// Unless `SAG_CONTROL_PLANE_SYNC_NO_LOCALHOST_FALLBACK=true`, appends
/// localhost as the lowest-priority fallback. Every reachable endpoint is
/// still generation-compared by the sync loop.
fn resolve_control_plane_sync_endpoints(
    configured: Option<&str>,
    no_local_fallback: bool,
) -> Vec<String> {
    const LOCAL: &str = "http://127.0.0.1:8090/api/v1/agent/routes";
    let mut urls: Vec<String> = configured
        .map(|s| {
            s.split(',')
                .map(|x| x.trim().to_string())
                .filter(|x| !x.is_empty())
                .collect()
        })
        .unwrap_or_default();

    if urls.is_empty() {
        return vec![LOCAL.to_string()];
    }
    if !no_local_fallback && !urls.iter().any(|u| u == LOCAL) {
        urls.push(LOCAL.to_string());
    }
    urls
}

pub fn control_plane_sync_endpoints_from_env() -> Vec<String> {
    let configured = std::env::var("SAG_CONTROL_PLANE_SYNC_ENDPOINT").ok();
    let no_local_fallback = std::env::var("SAG_CONTROL_PLANE_SYNC_NO_LOCALHOST_FALLBACK")
        .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
        .unwrap_or(false);
    resolve_control_plane_sync_endpoints(configured.as_deref(), no_local_fallback)
}

fn default_sync_interval_ms() -> u64 {
    5000
}

fn default_forward_timeout_ms() -> u64 {
    60_000
}

fn default_max_pending_waiters() -> usize {
    128
}

fn default_stream_buffer() -> usize {
    128
}

fn default_max_body_bytes() -> usize {
    1_048_576
}

fn default_memory_budget_bytes() -> u64 {
    768 * 1024 * 1024
}

fn validate_agent_memory_budget(
    max_pending_waiters: usize,
    stream_buffer: usize,
    max_request_body: usize,
    max_response_body: usize,
    budget_bytes: u64,
) -> Result<ValidatedMemoryBudget, String> {
    let stream_capacity = (stream_buffer as u64)
        .checked_mul(2)
        .ok_or_else(|| "Agent bidirectional stream capacity overflowed u64".to_string())?;
    MemoryBudget {
        budget_bytes,
        safety_factor_percent: 80,
        reserved_bytes: 64 * 1024 * 1024,
        ingress_concurrency: max_pending_waiters as u64,
        max_request_body: max_request_body as u64,
        response_concurrency: max_pending_waiters as u64,
        max_response_body: max_response_body as u64,
        queue_capacity: 0,
        max_enqueued_bytes: max_request_body as u64,
        stream_capacity,
        max_frame_bytes: max_request_body.max(max_response_body) as u64,
    }
    .validate()
}

fn default_policy_evaluate_timeout_ms() -> u64 {
    2000
}

fn default_auth_verify_timeout_ms() -> u64 {
    2000
}

fn default_policy_inflight_limit() -> usize {
    1024
}

fn default_auth_inflight_limit() -> usize {
    1024
}

fn default_negative_cache_enabled() -> bool {
    true
}

fn default_negative_cache_ttl_sec() -> u64 {
    2
}

fn default_tunnel_healthy_window_sec() -> u64 {
    10
}

fn default_connector_probe_enabled() -> bool {
    // Keep the binary rolling-upgrade compatible. Managed deployments enable
    // probing explicitly only after Connectors advertising health-probe-v1 are
    // available.
    false
}

fn default_connector_probe_interval_ms() -> u64 {
    2_000
}

fn default_connector_probe_timeout_ms() -> u64 {
    1_500
}

fn default_connector_probe_freshness_ms() -> u64 {
    6_000
}

fn default_connector_probe_startup_grace_ms() -> u64 {
    5_000
}

fn default_connector_probe_failure_threshold() -> u8 {
    3
}

fn default_grpc_tls_enabled() -> bool {
    true
}

fn env_bool(key: &str, default: bool) -> bool {
    std::env::var(key)
        .map(|v| matches!(v.to_lowercase().as_str(), "1" | "true" | "yes" | "on"))
        .unwrap_or(default)
}

fn normalize_certificate_fingerprint(raw: &str) -> Option<String> {
    let normalized = raw.trim().replace(':', "").to_ascii_lowercase();
    (normalized.len() == 64 && normalized.bytes().all(|byte| byte.is_ascii_hexdigit()))
        .then_some(normalized)
}

fn parse_connector_cert_bindings(raw: &str) -> anyhow::Result<HashMap<String, HashSet<String>>> {
    let mut bindings = HashMap::<String, HashSet<String>>::new();
    for item in raw
        .split(',')
        .map(str::trim)
        .filter(|item| !item.is_empty())
    {
        let (endpoint, fingerprint) = item.split_once('=').ok_or_else(|| {
            anyhow::anyhow!(
                "invalid SAG_CONNECTOR_CERT_BINDINGS entry {item:?}; expected endpoint=sha256"
            )
        })?;
        let endpoint = endpoint.trim();
        if endpoint.is_empty() {
            anyhow::bail!("SAG_CONNECTOR_CERT_BINDINGS endpoint must not be empty");
        }
        let fingerprint = normalize_certificate_fingerprint(fingerprint).ok_or_else(|| {
            anyhow::anyhow!(
                "invalid SHA-256 certificate fingerprint for Connector endpoint {endpoint:?}"
            )
        })?;
        bindings
            .entry(endpoint.to_string())
            .or_default()
            .insert(fingerprint);
    }
    Ok(bindings)
}

fn authorize_connector_certificate(
    required: bool,
    bindings: &HashMap<String, HashSet<String>>,
    endpoint: &str,
    peer_fingerprint: Option<&str>,
) -> Result<(), String> {
    if !required {
        return Ok(());
    }
    let fingerprint = peer_fingerprint
        .and_then(normalize_certificate_fingerprint)
        .ok_or_else(|| "Connector mTLS peer certificate is missing or invalid".to_string())?;
    let allowed = bindings.get(endpoint).ok_or_else(|| {
        format!("no Connector certificate binding configured for endpoint {endpoint:?}")
    })?;
    if allowed.contains(&fingerprint) {
        Ok(())
    } else {
        Err(format!(
            "Connector certificate is not authorized for endpoint {endpoint:?}"
        ))
    }
}

impl StealthTunnelConfig {
    pub fn from_env() -> anyhow::Result<Self> {
        let grpc_tls_enabled = std::env::var("SAG_GRPC_MTLS_ENABLED")
            .map(|v| v != "false" && v != "0")
            .unwrap_or(true);
        let require_connector_cert_binding =
            env_bool("SAG_REQUIRE_CONNECTOR_CERT_BINDING", grpc_tls_enabled);
        if require_connector_cert_binding && !grpc_tls_enabled {
            anyhow::bail!(
                "SAG_REQUIRE_CONNECTOR_CERT_BINDING=true requires SAG_GRPC_MTLS_ENABLED=true"
            );
        }
        let connector_cert_bindings = parse_connector_cert_bindings(
            &std::env::var("SAG_CONNECTOR_CERT_BINDINGS").unwrap_or_default(),
        )?;
        if require_connector_cert_binding && connector_cert_bindings.is_empty() {
            anyhow::bail!(
                "SAG_CONNECTOR_CERT_BINDINGS must contain endpoint=sha256 entries when Connector certificate binding is required"
            );
        }

        let mut base = Self {
            listen_addr: std::env::var("SAG_STEALTH_LISTEN_ADDR")
                .unwrap_or_else(|_| default_listen_addr()),
            control_plane_sync_endpoints: control_plane_sync_endpoints_from_env(),
            sync_interval_ms: std::env::var("SAG_CONTROL_PLANE_SYNC_INTERVAL_MS")
                .ok()
                .and_then(|s| s.parse().ok())
                .unwrap_or_else(default_sync_interval_ms),
            forward_timeout_ms: std::env::var("SAG_FORWARD_TIMEOUT_MS")
                .ok()
                .and_then(|s| s.parse().ok())
                .unwrap_or_else(default_forward_timeout_ms),
            max_pending_waiters: std::env::var("SAG_MAX_PENDING_WAITERS")
                .ok()
                .and_then(|s| s.parse().ok())
                .unwrap_or_else(default_max_pending_waiters),
            stream_buffer: std::env::var("SAG_AGENT_STREAM_BUFFER")
                .ok()
                .and_then(|value| value.parse().ok())
                .unwrap_or_else(default_stream_buffer),
            max_request_body_bytes: std::env::var("SAG_AGENT_MAX_REQUEST_BODY_BYTES")
                .ok()
                .and_then(|value| value.parse().ok())
                .unwrap_or_else(default_max_body_bytes),
            max_response_body_bytes: std::env::var("SAG_AGENT_MAX_RESPONSE_BODY_BYTES")
                .ok()
                .and_then(|value| value.parse().ok())
                .unwrap_or_else(default_max_body_bytes),
            memory_budget_bytes: std::env::var("SAG_MEMORY_BUDGET_BYTES")
                .ok()
                .and_then(|value| value.parse().ok())
                .unwrap_or_else(default_memory_budget_bytes),
            memory_required_bytes: 0,
            memory_allowed_bytes: 0,
            policy_evaluate_endpoint: std::env::var("SAG_POLICY_EVALUATE_ENDPOINT").ok(),
            policy_evaluate_timeout_ms: std::env::var("SAG_POLICY_EVALUATE_TIMEOUT_MS")
                .ok()
                .and_then(|s| s.parse().ok())
                .unwrap_or_else(default_policy_evaluate_timeout_ms),
            auth_verify_endpoint: std::env::var("SAG_AUTH_VERIFY_ENDPOINT").ok(),
            trust_identity_headers: std::env::var("SAG_TRUST_IDENTITY_HEADERS")
                .map(|v| matches!(v.to_lowercase().as_str(), "1" | "true" | "yes" | "on"))
                .unwrap_or(false),
            auth_verify_timeout_ms: std::env::var("SAG_AUTH_VERIFY_TIMEOUT_MS")
                .ok()
                .and_then(|s| s.parse().ok())
                .unwrap_or_else(default_auth_verify_timeout_ms),
            policy_inflight_limit: std::env::var("SAG_POLICY_INFLIGHT_LIMIT")
                .ok()
                .and_then(|s| s.parse().ok())
                .unwrap_or_else(default_policy_inflight_limit),
            auth_inflight_limit: std::env::var("SAG_AUTH_INFLIGHT_LIMIT")
                .ok()
                .and_then(|s| s.parse().ok())
                .unwrap_or_else(default_auth_inflight_limit),
            negative_cache_enabled: std::env::var("SAG_NEGATIVE_CACHE_ENABLED")
                .map(|v| matches!(v.to_lowercase().as_str(), "1" | "true" | "yes" | "on"))
                .unwrap_or_else(|_| default_negative_cache_enabled()),
            negative_cache_ttl_sec: std::env::var("SAG_NEGATIVE_CACHE_TTL_SEC")
                .ok()
                .and_then(|s| s.parse().ok())
                .unwrap_or_else(default_negative_cache_ttl_sec),
            tunnel_healthy_window_sec: std::env::var("SAG_TUNNEL_HEALTHY_WINDOW_SEC")
                .ok()
                .and_then(|s| s.parse().ok())
                .unwrap_or_else(default_tunnel_healthy_window_sec),
            connector_probe_enabled: env_bool(
                "SAG_CONNECTOR_PROBE_ENABLED",
                default_connector_probe_enabled(),
            ),
            connector_probe_interval_ms: std::env::var("SAG_CONNECTOR_PROBE_INTERVAL_MS")
                .ok()
                .and_then(|s| s.parse().ok())
                .unwrap_or_else(default_connector_probe_interval_ms)
                .max(100),
            connector_probe_timeout_ms: std::env::var("SAG_CONNECTOR_PROBE_TIMEOUT_MS")
                .ok()
                .and_then(|s| s.parse().ok())
                .unwrap_or_else(default_connector_probe_timeout_ms)
                .max(50),
            connector_probe_freshness_ms: std::env::var("SAG_CONNECTOR_PROBE_FRESHNESS_MS")
                .ok()
                .and_then(|s| s.parse().ok())
                .unwrap_or_else(default_connector_probe_freshness_ms)
                .max(100),
            connector_probe_startup_grace_ms: std::env::var("SAG_CONNECTOR_PROBE_STARTUP_GRACE_MS")
                .ok()
                .and_then(|s| s.parse().ok())
                .unwrap_or_else(default_connector_probe_startup_grace_ms)
                .max(100),
            connector_probe_failure_threshold: std::env::var(
                "SAG_CONNECTOR_PROBE_FAILURE_THRESHOLD",
            )
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or_else(default_connector_probe_failure_threshold)
            .clamp(1, 10),
            grpc_tls_enabled,
            grpc_tls_cert: std::env::var("SAG_GRPC_TLS_CERT").ok(),
            grpc_tls_key: std::env::var("SAG_GRPC_TLS_KEY").ok(),
            grpc_tls_client_ca: std::env::var("SAG_GRPC_TLS_CLIENT_CA").ok(),
            require_connector_cert_binding,
            connector_cert_bindings,
        };
        let budget = validate_agent_memory_budget(
            base.max_pending_waiters,
            base.stream_buffer,
            base.max_request_body_bytes,
            base.max_response_body_bytes,
            base.memory_budget_bytes,
        )
        .map_err(anyhow::Error::msg)?;
        base.memory_required_bytes = budget.required_bytes;
        base.memory_allowed_bytes = budget.allowed_bytes;
        Ok(base)
    }

    pub fn authorize_connector_certificate(
        &self,
        endpoint: &str,
        peer_fingerprint: Option<&str>,
    ) -> Result<(), String> {
        authorize_connector_certificate(
            self.require_connector_cert_binding,
            &self.connector_cert_bindings,
            endpoint,
            peer_fingerprint,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const FP1: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
    const FP2: &str = "abcdef0123456789abcdef0123456789abcdef0123456789abcdef0123456789";

    #[test]
    fn connector_certificate_bindings_support_replica_certificates() {
        let parsed = parse_connector_cert_bindings(&format!(
            "connector:stream={FP1}, connector:stream={FP2}"
        ))
        .unwrap();
        assert_eq!(parsed["connector:stream"].len(), 2);
        assert!(
            authorize_connector_certificate(true, &parsed, "connector:stream", Some(FP1)).is_ok()
        );
        assert!(
            authorize_connector_certificate(true, &parsed, "connector:stream", Some(FP2)).is_ok()
        );
    }

    #[test]
    fn connector_certificate_binding_rejects_wrong_endpoint_or_certificate() {
        let parsed = parse_connector_cert_bindings(&format!("connector:stream={FP1}")).unwrap();
        assert!(authorize_connector_certificate(true, &parsed, "other:stream", Some(FP1)).is_err());
        assert!(
            authorize_connector_certificate(true, &parsed, "connector:stream", Some(FP2)).is_err()
        );
        assert!(authorize_connector_certificate(true, &parsed, "connector:stream", None).is_err());
    }

    #[test]
    fn invalid_connector_certificate_binding_is_rejected() {
        assert!(parse_connector_cert_bindings("connector:stream=not-a-sha256").is_err());
        assert!(parse_connector_cert_bindings("missing-separator").is_err());
    }

    #[test]
    fn memory_budget_rejects_unsafe_agent_buffers() {
        assert!(
            validate_agent_memory_budget(128, 128, 1_048_576, 1_048_576, 768 * 1024 * 1024,)
                .is_ok()
        );
        assert!(validate_agent_memory_budget(
            16_384,
            32_768,
            1_048_576,
            4_194_304,
            768 * 1024 * 1024,
        )
        .is_err());
        assert!(validate_agent_memory_budget(128, 128, 0, 1_048_576, 768 * 1024 * 1024).is_err());
    }

    #[test]
    fn real_path_probe_has_safe_binary_rollout_defaults() {
        assert!(!default_connector_probe_enabled());
        assert_eq!(default_connector_probe_interval_ms(), 2_000);
        assert_eq!(default_connector_probe_timeout_ms(), 1_500);
        assert_eq!(default_connector_probe_freshness_ms(), 6_000);
        assert_eq!(default_connector_probe_failure_threshold(), 3);
    }

    #[test]
    fn explicit_control_plane_endpoints_precede_localhost_fallback() {
        let endpoints = resolve_control_plane_sync_endpoints(
            Some("http://primary:8090/routes,http://secondary:8090/routes"),
            false,
        );
        assert_eq!(
            endpoints,
            vec![
                "http://primary:8090/routes",
                "http://secondary:8090/routes",
                "http://127.0.0.1:8090/api/v1/agent/routes",
            ]
        );
        assert_eq!(
            resolve_control_plane_sync_endpoints(Some("http://primary:8090/routes"), true),
            vec!["http://primary:8090/routes"]
        );
    }
}
