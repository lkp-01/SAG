use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;
use std::time::Instant;

use axum::body::Body;
use axum::extract::{Path, State};
use axum::http::Request;
use axum::http::{HeaderMap, StatusCode};
use axum::middleware::{self, Next};
use axum::response::IntoResponse;
use axum::routing::{delete, get, post};
use axum::{Json, Router};
use jsonwebtoken::{decode, DecodingKey, Validation};
use moka::future::Cache;
use redis::AsyncCommands;
use sag_service_health::Readiness;
use serde::{Deserialize, Serialize};
use tokio::sync::{Mutex, RwLock};
use tower_http::trace::TraceLayer;
use tracing::{info, warn};

use shared_storage::{
    build_store_from_env, ensure_store_schema, redact_postgres_dsn, resolve_postgres_dsn,
    resolve_storage_backend, AuditLogRecord, AuditLogsStore, AuditWriter, FaultEventRecord,
    FaultEventsStore, IdentityStore, PoliciesStore, PolicyEffect as StoragePolicyEffect,
    PolicyRecord as StoragePolicyRecord, SecurityMutation, StorageBackend, StorageStore,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "UPPERCASE")]
enum PolicyEffect {
    Allow,
    Deny,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct PolicyRecord {
    id: String,
    effect: PolicyEffect,
    subjects: Vec<String>,
    #[serde(default)]
    app_id: Option<String>,
    #[serde(default)]
    path_prefix: Option<String>,
    #[serde(default = "default_priority")]
    priority: i32,
}

fn default_priority() -> i32 {
    1000
}

#[derive(Debug, Deserialize)]
struct EvaluateRequest {
    user_id: String,
    #[serde(default)]
    roles: Vec<String>,
    app_id: String,
    path: String,
    #[serde(default)]
    method: Option<String>,
    #[serde(default)]
    identity_verified: bool,
}

#[derive(Debug, Serialize, Deserialize)]
struct EvaluateResponse {
    decision: String,
    reason: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    matched_policy_id: Option<String>,
    #[serde(default)]
    cache_hit: bool,
}

#[derive(Debug, Deserialize)]
struct MapRolesRequest {
    provider_id: String,
    #[serde(default)]
    user_id: String,
    #[serde(default)]
    external_groups: Vec<String>,
    #[serde(default)]
    base_roles: Vec<String>,
}

#[derive(Debug, Serialize)]
struct MapRolesResponse {
    provider_id: String,
    user_id: String,
    external_groups: Vec<String>,
    effective_roles: Vec<String>,
    matched_rules: Vec<String>,
}

#[derive(Clone)]
struct AppState {
    policies: Arc<RwLock<HashMap<String, PolicyRecord>>>,
    decision_cache: Arc<Cache<String, CachedDecision>>,
    decision_redis_cache: Option<RedisDecisionCache>,
    policy_cache_enabled: bool,
    policy_version: Arc<AtomicU64>,
    store: Option<StorageStore>,
    audit_writer: AuditWriter,
    metrics: metrics_exporter_prometheus::PrometheusHandle,
    policy_internal_token: Arc<str>,
    readiness: Readiness,
}

fn management_actor(headers: &HeaderMap) -> String {
    headers
        .get("x-sag-user-id")
        .and_then(|value| value.to_str().ok())
        .filter(|value| !value.trim().is_empty())
        .unwrap_or("authenticated-admin")
        .to_string()
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct CachedDecision {
    decision: String,
    reason: String,
    matched_policy_id: Option<String>,
}

#[derive(Clone)]
struct RedisDecisionCache {
    ttl_sec: u64,
    key_prefix: String,
    conn: Arc<Mutex<redis::aio::ConnectionManager>>,
}

#[derive(Debug, Deserialize)]
struct JwtClaims {
    roles: Vec<String>,
}

fn require_admin_or_boss(headers: &HeaderMap) -> Result<(), (StatusCode, String)> {
    let auth = headers
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .ok_or((StatusCode::UNAUTHORIZED, "missing Authorization".into()))?;
    let token = auth.strip_prefix("Bearer ").ok_or((
        StatusCode::UNAUTHORIZED,
        "invalid Authorization format".into(),
    ))?;
    let secret = std::env::var("SAG_JWT_SECRET").unwrap_or_else(|_| "dev-jwt-secret".to_string());
    let data = decode::<JwtClaims>(
        token,
        &DecodingKey::from_secret(secret.as_bytes()),
        &Validation::default(),
    )
    .map_err(|_| (StatusCode::UNAUTHORIZED, "invalid token".into()))?;
    let allow = data
        .claims
        .roles
        .iter()
        .any(|r| r == "admin" || r == "boss");
    if !allow {
        return Err((StatusCode::FORBIDDEN, "admin/boss role required".into()));
    }
    Ok(())
}

fn constant_time_token_eq(actual: &str, expected: &str) -> bool {
    if actual.len() != expected.len() {
        return false;
    }
    actual
        .as_bytes()
        .iter()
        .zip(expected.as_bytes())
        .fold(0_u8, |difference, (left, right)| {
            difference | (left ^ right)
        })
        == 0
}

fn validate_internal_policy_context(
    headers: &HeaderMap,
    expected_token: &str,
) -> Result<(), (StatusCode, String)> {
    if expected_token.trim().is_empty() {
        return Err((
            StatusCode::SERVICE_UNAVAILABLE,
            "policy internal authentication is not configured".into(),
        ));
    }
    let marker = headers
        .get("x-sag-internal-authenticated")
        .and_then(|value| value.to_str().ok())
        .filter(|value| *value == "agent")
        .ok_or((
            StatusCode::UNAUTHORIZED,
            "missing trusted Agent context".into(),
        ))?;
    debug_assert_eq!(marker, "agent");

    let authorization = headers
        .get("authorization")
        .and_then(|value| value.to_str().ok())
        .ok_or((
            StatusCode::UNAUTHORIZED,
            "missing internal Authorization".into(),
        ))?;
    let (scheme, token) = authorization.split_once(' ').ok_or((
        StatusCode::UNAUTHORIZED,
        "invalid internal Authorization format".into(),
    ))?;
    if !scheme.eq_ignore_ascii_case("Bearer")
        || token.trim().is_empty()
        || !constant_time_token_eq(token.trim(), expected_token)
    {
        return Err((
            StatusCode::UNAUTHORIZED,
            "invalid internal Authorization".into(),
        ));
    }
    Ok(())
}

fn cache_key(req: &EvaluateRequest, policy_version: u64) -> String {
    let mut roles = req.roles.clone();
    roles.sort();
    format!(
        "{}|{}|{}|{}|{}|v{}",
        req.user_id,
        roles.join(","),
        req.app_id,
        req.path,
        req.method.as_deref().unwrap_or(""),
        policy_version
    )
}

async fn decision_redis_cache_from_env(ttl_sec: u64) -> Option<RedisDecisionCache> {
    let url = std::env::var("SAG_POLICY_CACHE_REDIS_URL")
        .ok()
        .filter(|v| !v.trim().is_empty())
        .or_else(|| std::env::var("SAG_SESSION_REDIS_URL").ok())
        .filter(|v| !v.trim().is_empty())?;
    match redis::Client::open(url.clone()) {
        Ok(client) => match redis::aio::ConnectionManager::new(client).await {
            Ok(conn) => {
                info!(ttl_sec, "sag-policy redis decision cache enabled");
                Some(RedisDecisionCache {
                    ttl_sec,
                    key_prefix: "sag:policy:decision:".to_string(),
                    conn: Arc::new(Mutex::new(conn)),
                })
            }
            Err(e) => {
                warn!(error = %e, "failed to initialize sag-policy redis cache; fallback to local cache only");
                None
            }
        },
        Err(e) => {
            warn!(error = %e, "failed to create redis client for sag-policy cache; fallback to local cache only");
            None
        }
    }
}

async fn get_cached_decision_from_redis(
    redis_cache: &RedisDecisionCache,
    key: &str,
) -> Option<CachedDecision> {
    let full_key = format!("{}{}", redis_cache.key_prefix, key);
    let mut conn = redis_cache.conn.lock().await;
    let raw: Option<String> = conn.get::<_, Option<String>>(full_key).await.ok().flatten();
    raw.and_then(|v| serde_json::from_str::<CachedDecision>(&v).ok())
}

async fn set_cached_decision_to_redis(
    redis_cache: &RedisDecisionCache,
    key: &str,
    value: &CachedDecision,
) {
    if let Ok(payload) = serde_json::to_string(value) {
        let full_key = format!("{}{}", redis_cache.key_prefix, key);
        let mut conn = redis_cache.conn.lock().await;
        let _: Result<(), _> = conn
            .set_ex::<_, _, ()>(full_key, payload, redis_cache.ttl_sec)
            .await;
    }
}

async fn metrics(State(state): State<AppState>) -> impl IntoResponse {
    state.metrics.render()
}

async fn metrics_mw(
    State(state): State<AppState>,
    req: Request<Body>,
    next: Next,
) -> impl IntoResponse {
    let start = Instant::now();
    let method = req.method().to_string();
    let path = req.uri().path().to_string();
    if fault_injection_env_enabled() && path.contains("/api/v1/") {
        let mode = std::env::var("SAG_FAULT_MODE").unwrap_or_else(|_| "off".to_string());
        if mode == "timeout" || mode == "http_status" {
            let code = std::env::var("SAG_FAULT_STATUS_CODE")
                .ok()
                .and_then(|v| v.parse::<u16>().ok())
                .unwrap_or(504);
            let delay_ms = std::env::var("SAG_FAULT_DELAY_MS")
                .ok()
                .and_then(|v| v.parse::<u64>().ok())
                .unwrap_or(1200);
            tokio::time::sleep(std::time::Duration::from_millis(delay_ms)).await;
            if let Some(store) = &state.store {
                let event = FaultEventRecord {
                    id: uuid::Uuid::new_v4().to_string(),
                    ts_ms: now_epoch_ms(),
                    service: "sag-policy".to_string(),
                    event_type: if mode == "timeout" {
                        "timeout".to_string()
                    } else {
                        "injected_fault".to_string()
                    },
                    severity: "critical".to_string(),
                    path: path.clone(),
                    method: method.clone(),
                    latency_ms: delay_ms as i64,
                    baseline_ms: 0,
                    threshold_ms: delay_ms as i64,
                    status_code: code as i64,
                    result: code.to_string(),
                    trace_id: "".to_string(),
                    source: "injector".to_string(),
                    resolved_at_ms: None,
                    meta_json: serde_json::json!({"mode":mode}).to_string(),
                };
                let _ = FaultEventsStore::insert(store, &event).await;
            }
            return (
                StatusCode::from_u16(code).unwrap_or(StatusCode::GATEWAY_TIMEOUT),
                format!("injected {}", mode),
            )
                .into_response();
        }
    }
    let user_id = req
        .headers()
        .get("x-sag-user-id")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_string();
    let app_id = req
        .headers()
        .get("x-sag-app-id")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_string();
    let res = next.run(req).await;
    let status = res.status().as_u16().to_string();
    let method2 = method.clone();
    let elapsed = start.elapsed().as_secs_f64();
    let c = metrics::counter!(
        "http_requests_total",
        "service" => "sag-policy",
        "method" => method.clone(),
        "path" => path.clone(),
        "status" => status.clone()
    );
    c.increment(1);
    let h = metrics::histogram!(
        "http_request_duration_seconds",
        "service" => "sag-policy",
        "method" => method2.clone(),
        "path" => path.clone(),
        "status" => status.clone()
    );
    h.record(elapsed);
    if state.store.is_some() {
        let row = AuditLogRecord {
            id: uuid::Uuid::new_v4().to_string(),
            ts_ms: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_millis() as i64)
                .unwrap_or(0),
            service: "sag-policy".to_string(),
            user_id,
            app_id,
            path: path.clone(),
            method: method2,
            latency_ms: (elapsed * 1000.0) as i64,
            decision: "observe".to_string(),
            result: status,
            trace_id: "".to_string(),
            extra_json: "".to_string(),
        };
        let _ = state.audit_writer.try_record(row);
    }
    res
}

fn now_epoch_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

fn fault_injection_env_enabled() -> bool {
    matches!(
        std::env::var("SAG_ENABLE_FAULT_INJECTION")
            .unwrap_or_default()
            .to_lowercase()
            .as_str(),
        "1" | "true" | "yes" | "on"
    )
}

fn matches_subject(policy: &PolicyRecord, user_id: &str, roles: &[String]) -> bool {
    if policy.subjects.iter().any(|s| s == "*") {
        return true;
    }
    for s in &policy.subjects {
        if let Some(rest) = s.strip_prefix("role:") {
            if roles.iter().any(|r| r == rest) {
                return true;
            }
        } else if let Some(rest) = s.strip_prefix("user:") {
            if rest == user_id {
                return true;
            }
        }
    }
    false
}

fn matches_resource(policy: &PolicyRecord, app_id: &str, path: &str) -> bool {
    match &policy.app_id {
        None => {}
        Some(a) if a.is_empty() => {}
        Some(a) if a == "*" => {}
        Some(a) if a == app_id => {}
        Some(_) => return false,
    }
    match &policy.path_prefix {
        None => {}
        Some(p) if p.is_empty() => {}
        Some(p) if path.starts_with(p) => {}
        Some(_) => return false,
    }
    true
}

fn evaluate_inner(policies: &[PolicyRecord], req: &EvaluateRequest) -> CachedDecision {
    let mut sorted: Vec<&PolicyRecord> = policies.iter().collect();
    sorted.sort_by_key(|policy| std::cmp::Reverse(policy.priority));

    for p in sorted {
        if !matches_subject(p, &req.user_id, &req.roles) {
            continue;
        }
        if !matches_resource(p, &req.app_id, &req.path) {
            continue;
        }
        let decision = match p.effect {
            PolicyEffect::Allow => "ALLOW",
            PolicyEffect::Deny => "DENY",
        };
        return CachedDecision {
            decision: decision.to_string(),
            reason: format!("matched policy {}", p.id),
            matched_policy_id: Some(p.id.clone()),
        };
    }

    CachedDecision {
        decision: "DENY".to_string(),
        reason: "no matching policy (default deny)".to_string(),
        matched_policy_id: None,
    }
}

async fn load_policies(store: &StorageStore) -> anyhow::Result<HashMap<String, PolicyRecord>> {
    ensure_store_schema(store).await?;
    PoliciesStore::init_schema(store).await?;
    let list = PoliciesStore::load_all(store).await?;
    let mut map = HashMap::new();
    for p in list {
        let effect = match p.effect {
            StoragePolicyEffect::Allow => PolicyEffect::Allow,
            StoragePolicyEffect::Deny => PolicyEffect::Deny,
        };
        let record = PolicyRecord {
            id: p.id,
            effect,
            subjects: p.subjects,
            app_id: p.app_id,
            path_prefix: p.path_prefix,
            priority: p.priority,
        };
        map.insert(record.id.clone(), record);
    }
    Ok(map)
}

async fn list_policies(
    headers: HeaderMap,
    State(state): State<AppState>,
) -> Result<Json<Vec<PolicyRecord>>, (StatusCode, String)> {
    require_admin_or_boss(&headers)?;
    let g = state.policies.read().await;
    Ok(Json(g.values().cloned().collect()))
}

async fn post_policy(
    headers: HeaderMap,
    State(state): State<AppState>,
    Json(mut body): Json<PolicyRecord>,
) -> Result<StatusCode, (StatusCode, String)> {
    require_admin_or_boss(&headers)?;
    if body.id.is_empty() {
        return Err((StatusCode::BAD_REQUEST, "id required".into()));
    }
    if body.priority == 0 {
        body.priority = default_priority();
    }
    let store = state.store.as_ref().ok_or_else(|| {
        (
            StatusCode::SERVICE_UNAVAILABLE,
            "policy storage unavailable".to_string(),
        )
    })?;
    let storage_record = StoragePolicyRecord {
        id: body.id.clone(),
        effect: match body.effect {
            PolicyEffect::Allow => StoragePolicyEffect::Allow,
            PolicyEffect::Deny => StoragePolicyEffect::Deny,
        },
        subjects: body.subjects.clone(),
        app_id: body.app_id.clone(),
        path_prefix: body.path_prefix.clone(),
        priority: body.priority,
    };
    let audit = AuditLogRecord::management(
        "sag-policy",
        management_actor(&headers),
        body.app_id.clone().unwrap_or_default(),
        format!("/api/v1/policies/{}", body.id),
        "PUT",
    );
    AuditLogsStore::apply_security_mutation(
        store,
        &SecurityMutation::UpsertPolicy(storage_record),
        &audit,
    )
    .await
    .map_err(|error| (StatusCode::INTERNAL_SERVER_ERROR, error.to_string()))?;
    state.policies.write().await.insert(body.id.clone(), body);
    state.policy_version.fetch_add(1, Ordering::Relaxed);
    let bump = metrics::counter!("cache_version_bump_total", "service" => "sag-policy", "cache" => "policy_eval");
    bump.increment(1);
    Ok(StatusCode::CREATED)
}

async fn delete_policy(
    headers: HeaderMap,
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<StatusCode, (StatusCode, String)> {
    require_admin_or_boss(&headers)?;
    let store = state.store.as_ref().ok_or_else(|| {
        (
            StatusCode::SERVICE_UNAVAILABLE,
            "policy storage unavailable".to_string(),
        )
    })?;
    let audit = AuditLogRecord::management(
        "sag-policy",
        management_actor(&headers),
        "",
        format!("/api/v1/policies/{id}"),
        "DELETE",
    );
    AuditLogsStore::apply_security_mutation(
        store,
        &SecurityMutation::DeletePolicy(id.clone()),
        &audit,
    )
    .await
    .map_err(|error| (StatusCode::INTERNAL_SERVER_ERROR, error.to_string()))?;
    state.policies.write().await.remove(&id);
    state.policy_version.fetch_add(1, Ordering::Relaxed);
    let bump = metrics::counter!("cache_version_bump_total", "service" => "sag-policy", "cache" => "policy_eval");
    bump.increment(1);
    Ok(StatusCode::NO_CONTENT)
}

async fn evaluate(
    headers: HeaderMap,
    State(state): State<AppState>,
    Json(req): Json<EvaluateRequest>,
) -> Result<Json<EvaluateResponse>, (StatusCode, String)> {
    validate_internal_policy_context(&headers, &state.policy_internal_token)?;
    if !req.identity_verified || req.user_id.trim().is_empty() || req.roles.is_empty() {
        metrics::counter!("policy_identity_rejected_total", "reason" => "missing_canonical_identity")
            .increment(1);
        return Err((
            StatusCode::UNAUTHORIZED,
            "missing canonical verified identity".into(),
        ));
    }
    if !state.policy_cache_enabled {
        let policies: Vec<PolicyRecord> = {
            let g = state.policies.read().await;
            g.values().cloned().collect()
        };
        let out = evaluate_inner(&policies, &req);
        return Ok(Json(EvaluateResponse {
            decision: out.decision,
            reason: out.reason,
            matched_policy_id: out.matched_policy_id,
            cache_hit: false,
        }));
    }

    let version = state.policy_version.load(Ordering::Relaxed);
    let key = cache_key(&req, version);
    if let Some(cached) = state.decision_cache.get(&key).await {
        let hit = metrics::counter!("cache_hit_total", "service" => "sag-policy", "cache" => "policy_eval");
        hit.increment(1);
        let rate = metrics::counter!("policy_eval_cache_hit_rate", "result" => "hit");
        rate.increment(1);
        return Ok(Json(EvaluateResponse {
            decision: cached.decision.clone(),
            reason: cached.reason.clone(),
            matched_policy_id: cached.matched_policy_id.clone(),
            cache_hit: true,
        }));
    }

    if let Some(redis_cache) = &state.decision_redis_cache {
        if let Some(cached) = get_cached_decision_from_redis(redis_cache, &key).await {
            state
                .decision_cache
                .insert(key.clone(), cached.clone())
                .await;
            let hit = metrics::counter!("cache_hit_total", "service" => "sag-policy", "cache" => "policy_eval_redis");
            hit.increment(1);
            let rate = metrics::counter!("policy_eval_cache_hit_rate", "result" => "redis_hit");
            rate.increment(1);
            return Ok(Json(EvaluateResponse {
                decision: cached.decision,
                reason: cached.reason,
                matched_policy_id: cached.matched_policy_id,
                cache_hit: true,
            }));
        }
    }

    let miss =
        metrics::counter!("cache_miss_total", "service" => "sag-policy", "cache" => "policy_eval");
    miss.increment(1);
    let rate = metrics::counter!("policy_eval_cache_hit_rate", "result" => "miss");
    rate.increment(1);
    let req_for_compute = req;
    let policies_ref = state.policies.clone();
    let out = state
        .decision_cache
        .get_with(key.clone(), async move {
            let policies: Vec<PolicyRecord> = {
                let g = policies_ref.read().await;
                g.values().cloned().collect()
            };
            evaluate_inner(&policies, &req_for_compute)
        })
        .await;
    if let Some(redis_cache) = &state.decision_redis_cache {
        set_cached_decision_to_redis(redis_cache, &key, &out).await;
    }
    Ok(Json(EvaluateResponse {
        decision: out.decision,
        reason: out.reason,
        matched_policy_id: out.matched_policy_id,
        cache_hit: false,
    }))
}

fn split_roles_csv(s: &str) -> Vec<String> {
    s.split(',')
        .map(|x| x.trim().to_string())
        .filter(|x| !x.is_empty())
        .collect()
}

async fn map_roles(
    State(state): State<AppState>,
    Json(req): Json<MapRolesRequest>,
) -> Result<Json<MapRolesResponse>, (StatusCode, String)> {
    let Some(store) = &state.store else {
        return Err((
            StatusCode::INTERNAL_SERVER_ERROR,
            "storage unavailable".into(),
        ));
    };
    if req.provider_id.trim().is_empty() {
        return Err((StatusCode::BAD_REQUEST, "provider_id is required".into()));
    }
    let rows = IdentityStore::list_mappings(store, Some(req.provider_id.as_str()))
        .await
        .map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("list mappings failed: {e}"),
            )
        })?;
    let mut roles = req.base_roles.clone();
    let mut matched_rules = Vec::<String>::new();
    for r in rows {
        if !r.enabled {
            continue;
        }
        if req.external_groups.iter().any(|g| g == &r.external_group) {
            roles.extend(split_roles_csv(&r.local_roles_csv));
            matched_rules.push(r.id);
        }
    }
    roles.sort();
    roles.dedup();
    Ok(Json(MapRolesResponse {
        provider_id: req.provider_id,
        user_id: req.user_id,
        external_groups: req.external_groups,
        effective_roles: roles,
        matched_rules,
    }))
}

async fn live() -> StatusCode {
    StatusCode::OK
}

async fn ready(State(state): State<AppState>) -> StatusCode {
    let store = state.store.clone();
    let timeout = Duration::from_millis(
        std::env::var("SAG_READINESS_PROBE_TIMEOUT_MS")
            .ok()
            .and_then(|value| value.parse().ok())
            .unwrap_or(1_000),
    );
    let status = state
        .readiness
        .probe(timeout, async move {
            match store {
                Some(store) => store.health_check().await.is_ok(),
                None => false,
            }
        })
        .await;
    if status == sag_service_health::ReadyState::Ready {
        StatusCode::OK
    } else {
        StatusCode::SERVICE_UNAVAILABLE
    }
}

async fn admission_mw(
    State(state): State<AppState>,
    req: Request<Body>,
    next: Next,
) -> axum::response::Response {
    if matches!(
        req.uri().path(),
        "/live" | "/ready" | "/health" | "/metrics"
    ) {
        return next.run(req).await;
    }
    let Some(_active) = state.readiness.try_admit() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    next.run(req).await
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    let backend = resolve_storage_backend();
    let store = build_store_from_env();
    ensure_store_schema(&store).await?;
    let audit_writer = AuditWriter::from_env(store.clone())?;
    PoliciesStore::init_schema(&store).await?;
    let mut map = load_policies(&store).await?;
    let store_hint = match backend {
        StorageBackend::Sqlite => format!("sqlite:{}", shared_storage::resolve_storage_db_path()),
        StorageBackend::Postgres => {
            format!("postgres:{}", redact_postgres_dsn(&resolve_postgres_dsn()))
        }
    };
    if map.is_empty() {
        let bootstrap = PolicyRecord {
            id: "p-allow-admin".into(),
            effect: PolicyEffect::Allow,
            subjects: vec!["role:admin".into()],
            app_id: None,
            path_prefix: None,
            priority: 1000,
        };
        map.insert(bootstrap.id.clone(), bootstrap.clone());
        PoliciesStore::upsert(
            &store,
            &StoragePolicyRecord {
                id: bootstrap.id.clone(),
                effect: StoragePolicyEffect::Allow,
                subjects: bootstrap.subjects.clone(),
                app_id: bootstrap.app_id.clone(),
                path_prefix: bootstrap.path_prefix.clone(),
                priority: bootstrap.priority,
            },
        )
        .await?;
    }

    let policies = Arc::new(RwLock::new(map));
    let policy_cache_enabled = std::env::var("SAG_POLICY_CACHE_ENABLED")
        .map(|v| matches!(v.to_lowercase().as_str(), "1" | "true" | "yes" | "on"))
        .unwrap_or(true);
    let policy_cache_ttl_sec = std::env::var("SAG_POLICY_CACHE_TTL_SEC")
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
        .unwrap_or(15);
    let policy_cache_max_capacity = std::env::var("SAG_POLICY_CACHE_MAX_CAPACITY")
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
        .unwrap_or(200_000);
    let decision_redis_cache = decision_redis_cache_from_env(policy_cache_ttl_sec).await;
    let decision_cache = Arc::new(
        Cache::builder()
            .time_to_live(Duration::from_secs(policy_cache_ttl_sec))
            .max_capacity(policy_cache_max_capacity)
            .build(),
    );

    let state = AppState {
        policies,
        decision_cache,
        decision_redis_cache,
        policy_cache_enabled,
        policy_version: Arc::new(AtomicU64::new(1)),
        store: Some(store),
        audit_writer,
        metrics: metrics_exporter_prometheus::PrometheusBuilder::new()
            .install_recorder()
            .map_err(|e| anyhow::anyhow!("install prometheus recorder failed: {e}"))?,
        policy_internal_token: Arc::<str>::from(
            std::env::var("SAG_POLICY_INTERNAL_TOKEN")
                .ok()
                .filter(|value| !value.trim().is_empty())
                .ok_or_else(|| {
                    anyhow::anyhow!(
                        "SAG_POLICY_INTERNAL_TOKEN is required for trusted Agent policy calls"
                    )
                })?,
        ),
        readiness: Readiness::new(
            std::env::var("SAG_READINESS_SUCCESS_THRESHOLD")
                .ok()
                .and_then(|value| value.parse().ok())
                .unwrap_or(2),
        ),
    };
    let redis_cache_enabled = state.decision_redis_cache.is_some();

    let app = Router::new()
        .route("/live", get(live))
        .route("/ready", get(ready))
        .route("/health", get(ready))
        .route("/metrics", get(metrics))
        .route("/api/v1/policies", get(list_policies).post(post_policy))
        .route("/api/v1/policies/:id", delete(delete_policy))
        .route("/api/v1/policy/evaluate", post(evaluate))
        .route("/api/v1/identity/map-roles", post(map_roles))
        .layer(middleware::from_fn_with_state(state.clone(), metrics_mw))
        .layer(middleware::from_fn_with_state(state.clone(), admission_mw))
        .layer(TraceLayer::new_for_http())
        .with_state(state.clone());

    let addr: SocketAddr = "0.0.0.0:8081".parse()?;
    info!(
        %addr,
        backend=?backend,
        store=%store_hint,
        policy_cache_enabled=policy_cache_enabled,
        policy_cache_ttl_sec=policy_cache_ttl_sec,
        policy_cache_max_capacity=policy_cache_max_capacity,
        redis_cache_enabled=redis_cache_enabled,
        "sag-policy listening"
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
    let drain_timeout = Duration::from_millis(
        std::env::var("SAG_DRAIN_TIMEOUT_MS")
            .ok()
            .and_then(|value| value.parse().ok())
            .unwrap_or(30_000),
    );
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
    fn policy_evaluate_rejects_missing_internal_identity_context() {
        let headers = HeaderMap::new();

        let error = validate_internal_policy_context(&headers, "policy-internal-secret")
            .expect_err("public callers must not supply trusted policy identity");

        assert_eq!(error.0, StatusCode::UNAUTHORIZED);
    }

    #[test]
    fn policy_evaluate_rejects_forged_marker_without_internal_token() {
        let mut headers = HeaderMap::new();
        headers.insert("x-sag-internal-authenticated", "agent".parse().unwrap());
        headers.insert("authorization", "Bearer caller-token".parse().unwrap());

        let error = validate_internal_policy_context(&headers, "policy-internal-secret")
            .expect_err("an internal marker alone is not an authentication fact");

        assert_eq!(error.0, StatusCode::UNAUTHORIZED);
    }

    #[test]
    fn policy_evaluate_accepts_agent_marker_with_matching_internal_token() {
        let mut headers = HeaderMap::new();
        headers.insert("x-sag-internal-authenticated", "agent".parse().unwrap());
        headers.insert(
            "authorization",
            "Bearer policy-internal-secret".parse().unwrap(),
        );

        assert!(validate_internal_policy_context(&headers, "policy-internal-secret").is_ok());
    }
}
