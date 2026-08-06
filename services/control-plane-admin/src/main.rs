mod apisix;

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::{Duration, Instant};

use axum::body::Body;
use axum::extract::{Path, Query, State};
use axum::http::Request;
use axum::http::{HeaderMap, HeaderValue, StatusCode};
use axum::middleware::{self, Next};
use axum::response::IntoResponse;
#[allow(unused_imports)]
use axum::routing::{delete, get, post, put};
use axum::{Json, Router};
use jsonwebtoken::{decode, DecodingKey, Validation};
use moka::future::Cache;
use sag_service_health::Readiness;
use serde::{Deserialize, Serialize};
use shared_storage::{
    build_store_from_env, ensure_store_schema, redact_postgres_dsn, resolve_postgres_dsn,
    resolve_storage_backend, ApiRouteRecord, ApiRoutesStore, AppMetricMinuteRecord,
    AppMetricsStore, AppRecord, AppsStore, AuditLogFilter, AuditLogRecord, AuditLogsStore,
    AuditWriter, ConfigSyncJob, ConfigSyncJobDraft, ConfigSyncOperation, ConfigSyncStore,
    FaultEventFilter, FaultEventRecord, FaultEventsStore, IdempotencyRecord, IdempotencyStore,
    IntranetUpstreamRecord, RoutesStore, SecurityMutation, StorageBackend, StorageError,
    StorageStore, TunnelRouteRecord, UsersStore,
};
use tokio::sync::RwLock;
use tower_http::trace::TraceLayer;
use tracing::{info, warn};

#[derive(Clone)]
struct AppState {
    store: StorageStore,
    audit_writer: AuditWriter,
    http: reqwest::Client,
    apisix: Option<apisix::ApisixPushConfig>,
    metrics: metrics_exporter_prometheus::PrometheusHandle,
    fault_toggle: Arc<RwLock<FaultInjectionToggle>>,
    route_cache: Arc<Cache<String, RouteCacheEntry>>,
    route_cache_enabled: bool,
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

fn require_uuid_v4_or_generate(value: &str) -> Result<String, (StatusCode, String)> {
    if value.trim().is_empty() {
        return Ok(uuid::Uuid::new_v4().to_string());
    }
    let parsed = uuid::Uuid::parse_str(value)
        .map_err(|_| (StatusCode::BAD_REQUEST, "id must be a UUID v4".to_string()))?;
    if parsed.get_version() != Some(uuid::Version::Random) {
        return Err((StatusCode::BAD_REQUEST, "id must be a UUID v4".to_string()));
    }
    Ok(parsed.to_string())
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct FaultInjectionToggle {
    enabled: bool,
    ttl_sec: u64,
    expires_at_ms: i64,
    mode: String,
    service: String,
    path_contains: String,
    delay_ms: i64,
    status_code: u16,
    hit_percent: u8,
}

impl Default for FaultInjectionToggle {
    fn default() -> Self {
        Self {
            enabled: false,
            ttl_sec: 120,
            expires_at_ms: 0,
            mode: "delay".to_string(),
            service: "control-plane-admin".to_string(),
            path_contains: "".to_string(),
            delay_ms: 1200,
            status_code: 504,
            hit_percent: 100,
        }
    }
}

#[derive(Debug, Deserialize)]
struct JwtClaims {
    #[serde(default)]
    sub: String,
    #[serde(default)]
    username: String,
    #[serde(default)]
    auth_version: i64,
}

#[derive(Debug, Clone, Default)]
struct RequestIdentity {
    user_id: String,
    username: String,
    roles: Vec<String>,
}

fn has_valid_agent_sync_token(headers: &HeaderMap) -> bool {
    let expected = match std::env::var("SAG_AGENT_SYNC_TOKEN") {
        Ok(v) if !v.is_empty() => v,
        _ => return false,
    };
    let got = headers
        .get("x-sag-agent-token")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    got == expected
}

async fn require_admin_or_boss(
    headers: &HeaderMap,
    store: &StorageStore,
) -> Result<(), (StatusCode, String)> {
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
    let current = UsersStore::load_by_id(store, &data.claims.sub)
        .await
        .map_err(|error| {
            (
                StatusCode::SERVICE_UNAVAILABLE,
                format!("authorization state unavailable: {error}"),
            )
        })?
        .filter(|user| user.enabled && user.auth_version == data.claims.auth_version)
        .ok_or((StatusCode::UNAUTHORIZED, "revoked token".into()))?;
    let allow = current.roles.iter().any(|r| r == "admin" || r == "boss");
    if !allow {
        return Err((StatusCode::FORBIDDEN, "admin/boss role required".into()));
    }
    Ok(())
}

fn require_public_readonly(headers: &HeaderMap) -> Result<(), (StatusCode, String)> {
    let expected = std::env::var("SAG_PUBLIC_READONLY_TOKEN").map_err(|_| {
        (
            StatusCode::SERVICE_UNAVAILABLE,
            "readonly token not configured".into(),
        )
    })?;
    let got = headers
        .get("x-sag-readonly-token")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    if got.is_empty() || got != expected {
        return Err((StatusCode::UNAUTHORIZED, "invalid readonly token".into()));
    }
    Ok(())
}

async fn authenticated_reconciliation_operator(
    headers: &HeaderMap,
    store: &StorageStore,
) -> Result<String, (StatusCode, String)> {
    require_admin_or_boss(headers, store).await?;
    parse_identity(headers, store)
        .await
        .map(|identity| identity.user_id)
        .filter(|user_id| !user_id.trim().is_empty())
        .ok_or((
            StatusCode::UNAUTHORIZED,
            "authenticated operator identity unavailable".into(),
        ))
}

#[derive(Debug, Deserialize)]
struct IndeterminateQuery {
    #[serde(default = "default_reconciliation_age_ms")]
    min_age_ms: i64,
    #[serde(default = "default_reconciliation_limit")]
    limit: usize,
}

fn default_reconciliation_age_ms() -> i64 {
    5 * 60 * 1_000
}

fn default_reconciliation_limit() -> usize {
    100
}

#[derive(Debug, Deserialize)]
struct OperatorCompletionRequest {
    expected_version: i64,
    status_code: u32,
    #[serde(default = "default_headers_json")]
    headers_json: String,
    #[serde(default)]
    result_body: String,
    reason: String,
    confirmation: String,
}

fn default_headers_json() -> String {
    "{}".into()
}

#[derive(Debug, Deserialize)]
struct OperatorReleaseRequest {
    expected_version: i64,
    reason: String,
    confirmation: String,
}

async fn list_indeterminate_idempotency(
    headers: HeaderMap,
    State(state): State<AppState>,
    Query(query): Query<IndeterminateQuery>,
) -> Result<Json<Vec<IdempotencyRecord>>, (StatusCode, String)> {
    require_admin_or_boss(&headers, &state.store).await?;
    if query.min_age_ms < 0 {
        return Err((
            StatusCode::BAD_REQUEST,
            "min_age_ms must be non-negative".into(),
        ));
    }
    let cutoff = now_epoch_ms().saturating_sub(query.min_age_ms);
    IdempotencyStore::list_indeterminate(&state.store, cutoff, query.limit)
        .await
        .map(Json)
        .map_err(|error| (StatusCode::SERVICE_UNAVAILABLE, error.to_string()))
}

async fn get_idempotency_evidence(
    headers: HeaderMap,
    State(state): State<AppState>,
    Path(scope_key): Path<String>,
) -> Result<Json<IdempotencyRecord>, (StatusCode, String)> {
    require_admin_or_boss(&headers, &state.store).await?;
    IdempotencyStore::get(&state.store, &scope_key)
        .await
        .map_err(|error| (StatusCode::SERVICE_UNAVAILABLE, error.to_string()))?
        .map(Json)
        .ok_or((StatusCode::NOT_FOUND, "idempotency record not found".into()))
}

async fn complete_idempotency_by_operator(
    headers: HeaderMap,
    State(state): State<AppState>,
    Path(scope_key): Path<String>,
    Json(body): Json<OperatorCompletionRequest>,
) -> Result<StatusCode, (StatusCode, String)> {
    let operator = authenticated_reconciliation_operator(&headers, &state.store).await?;
    if body.confirmation != "COMPLETE" {
        return Err((
            StatusCode::BAD_REQUEST,
            "confirmation must exactly equal COMPLETE".into(),
        ));
    }
    serde_json::from_str::<serde_json::Value>(&body.headers_json).map_err(|_| {
        (
            StatusCode::BAD_REQUEST,
            "headers_json must be valid JSON".into(),
        )
    })?;
    let event_id = uuid::Uuid::new_v4().to_string();
    let updated = IdempotencyStore::complete_by_operator(
        &state.store,
        &scope_key,
        body.expected_version,
        body.status_code,
        &body.headers_json,
        body.result_body.as_bytes(),
        &operator,
        &body.reason,
        now_epoch_ms(),
        &event_id,
    )
    .await
    .map_err(|error| (StatusCode::SERVICE_UNAVAILABLE, error.to_string()))?;
    if !updated {
        return Err((
            StatusCode::CONFLICT,
            "record is not indeterminate at the expected version".into(),
        ));
    }
    Ok(StatusCode::NO_CONTENT)
}

async fn release_idempotency_by_operator(
    headers: HeaderMap,
    State(state): State<AppState>,
    Path(scope_key): Path<String>,
    Json(body): Json<OperatorReleaseRequest>,
) -> Result<StatusCode, (StatusCode, String)> {
    let operator = authenticated_reconciliation_operator(&headers, &state.store).await?;
    if body.confirmation != "RELEASE" {
        return Err((
            StatusCode::BAD_REQUEST,
            "confirmation must exactly equal RELEASE".into(),
        ));
    }
    let event_id = uuid::Uuid::new_v4().to_string();
    let updated = IdempotencyStore::release_by_operator(
        &state.store,
        &scope_key,
        body.expected_version,
        &operator,
        &body.reason,
        now_epoch_ms(),
        &event_id,
    )
    .await
    .map_err(|error| (StatusCode::SERVICE_UNAVAILABLE, error.to_string()))?;
    if !updated {
        return Err((
            StatusCode::CONFLICT,
            "record is not indeterminate at the expected version".into(),
        ));
    }
    Ok(StatusCode::NO_CONTENT)
}

async fn parse_identity(headers: &HeaderMap, store: &StorageStore) -> Option<RequestIdentity> {
    let auth = headers.get("authorization")?.to_str().ok()?;
    let token = auth.strip_prefix("Bearer ")?;
    let secret = std::env::var("SAG_JWT_SECRET").unwrap_or_else(|_| "dev-jwt-secret".to_string());
    let data = decode::<JwtClaims>(
        token,
        &DecodingKey::from_secret(secret.as_bytes()),
        &Validation::default(),
    )
    .ok()?;
    let current = UsersStore::load_by_id(store, &data.claims.sub)
        .await
        .ok()??;
    if !current.enabled || current.auth_version != data.claims.auth_version {
        return None;
    }
    let user_id = if !data.claims.sub.is_empty() {
        data.claims.sub
    } else {
        data.claims.username.clone()
    };
    Some(RequestIdentity {
        user_id,
        username: data.claims.username,
        roles: current.roles,
    })
}

fn derive_department(roles: &[String]) -> String {
    if roles.iter().any(|r| r == "finance") {
        "finance".to_string()
    } else if roles.iter().any(|r| r == "tech") {
        "tech".to_string()
    } else if roles.iter().any(|r| r == "ops") {
        "ops".to_string()
    } else if roles.iter().any(|r| r == "boss") {
        "management".to_string()
    } else if roles.iter().any(|r| r == "vendor") {
        "vendor".to_string()
    } else {
        "".to_string()
    }
}

fn debug_log(
    location: &str,
    message: &str,
    hypothesis_id: &str,
    data: serde_json::Value,
    run_id: &str,
) {
    // #region agent log
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open("debug-ac5396.log")
    {
        let line = serde_json::json!({
            "sessionId":"ac5396",
            "runId":run_id,
            "hypothesisId":hypothesis_id,
            "location":location,
            "message":message,
            "data":data,
            "timestamp":std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis()).unwrap_or(0)
        });
        let _ = std::io::Write::write_all(&mut f, format!("{}\n", line).as_bytes());
    }
    // #endregion
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct TunnelRouteRecordDto {
    host: String,
    app_id: String,
    connector_endpoint: String,
    #[serde(default = "default_require_healthy")]
    require_healthy_tunnel: bool,
}

#[derive(Debug, Clone)]
struct RouteCacheEntry {
    generation: i64,
    routes: Vec<TunnelRouteRecordDto>,
}

type RouteListResponse = (HeaderMap, Json<Vec<TunnelRouteRecordDto>>);

#[derive(Debug, Deserialize)]
struct AgentConfigAckDto {
    agent_id: String,
    applied_generation: u64,
    applied_at_ms: i64,
    #[serde(default)]
    snapshot_hash: Option<String>,
}

fn default_require_healthy() -> bool {
    true
}

#[derive(Deserialize)]
struct RoutesQuery {
    app_id: Option<String>,
}

#[derive(Deserialize)]
struct IntranetQuery {
    app_id: String,
}

#[derive(Deserialize, Serialize)]
struct IntranetBody {
    upstream: String,
    #[serde(default = "default_scheme")]
    scheme: String,
}

#[derive(Debug, Deserialize)]
struct AppMetricsQuery {
    app_id: Option<String>,
    range_min: Option<u64>,
}

#[derive(Debug, Deserialize)]
struct AppsTreeQuery {
    with_latest: Option<bool>,
}

#[derive(Debug, Serialize)]
struct AppMetricsPointDto {
    ts_minute: i64,
    request_count: i64,
    pv_count: i64,
    uv_count: i64,
    unique_ip_count: i64,
    err4xx_count: i64,
    err5xx_count: i64,
    qps_avg: f64,
    err4xx_rate: f64,
    err5xx_rate: f64,
}

#[derive(Debug, Serialize)]
struct AppMetricsSeriesDto {
    app_id: String,
    latest: Option<AppMetricsPointDto>,
    points: Vec<AppMetricsPointDto>,
}

#[derive(Debug, Serialize)]
struct AppMetricsResponseDto {
    generated_at_minute: i64,
    series: Vec<AppMetricsSeriesDto>,
    note: String,
}

#[derive(Debug, Serialize)]
struct AppTreeNodeDto {
    app_id: String,
    routes: Vec<TunnelRouteRecordDto>,
    latest: Option<AppMetricsPointDto>,
}

#[derive(Debug, Deserialize, Serialize)]
struct AppRecordDto {
    app_id: String,
    #[serde(default)]
    display_name: String,
    #[serde(default)]
    description: String,
    #[serde(default = "default_app_enabled")]
    enabled: bool,
}

#[derive(Debug, Deserialize, Serialize)]
struct ApiRouteRecordDto {
    id: String,
    app_id: String,
    method: String,
    path: String,
    #[serde(default = "default_app_enabled")]
    enabled: bool,
    #[serde(default)]
    description: String,
}

#[derive(Debug, Deserialize)]
struct ApiRoutesQuery {
    app_id: Option<String>,
}

#[derive(Debug, Deserialize, Serialize)]
struct AuditLogDto {
    id: String,
    ts_ms: i64,
    service: String,
    user_id: String,
    app_id: String,
    path: String,
    method: String,
    latency_ms: i64,
    decision: String,
    result: String,
    trace_id: String,
    #[serde(default)]
    extra_json: String,
}

#[derive(Debug, Deserialize)]
struct AuditLogsQuery {
    from_ts_ms: Option<i64>,
    to_ts_ms: Option<i64>,
    user_id: Option<String>,
    app_id: Option<String>,
    service: Option<String>,
    result: Option<String>,
    decision: Option<String>,
    path_contains: Option<String>,
    department: Option<String>,
    limit: Option<i64>,
}

#[derive(Debug, Deserialize, Serialize)]
struct FaultEventDto {
    id: String,
    ts_ms: i64,
    service: String,
    event_type: String,
    severity: String,
    path: String,
    method: String,
    latency_ms: i64,
    baseline_ms: i64,
    threshold_ms: i64,
    status_code: i64,
    result: String,
    trace_id: String,
    source: String,
    resolved_at_ms: Option<i64>,
    #[serde(default)]
    meta_json: String,
}

#[derive(Debug, Deserialize)]
struct FaultEventsQuery {
    from_ts_ms: Option<i64>,
    to_ts_ms: Option<i64>,
    service: Option<String>,
    event_type: Option<String>,
    severity: Option<String>,
    result: Option<String>,
    source: Option<String>,
    limit: Option<i64>,
}

#[derive(Debug, Deserialize)]
struct FaultInjectionUpdate {
    enabled: Option<bool>,
    ttl_sec: Option<u64>,
    mode: Option<String>,
    service: Option<String>,
    path_contains: Option<String>,
    delay_ms: Option<i64>,
    status_code: Option<u16>,
    hit_percent: Option<u8>,
}

#[derive(Debug, Serialize)]
struct PublicSecurityOverviewDto {
    audit_count: usize,
    fault_event_count: usize,
    critical_fault_count: usize,
    top_services: Vec<ServiceCountDto>,
    note: String,
}

#[derive(Debug, Serialize)]
struct ServiceCountDto {
    service: String,
    count: usize,
}

fn default_app_enabled() -> bool {
    true
}

async fn list_apps(
    headers: HeaderMap,
    State(state): State<AppState>,
) -> Result<Json<Vec<AppRecordDto>>, (StatusCode, String)> {
    let t0 = Instant::now();
    require_admin_or_boss(&headers, &state.store).await?;
    let rows = AppsStore::load_all(&state.store)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    debug_log(
        "control-plane-admin/main.rs:list_apps",
        "list apps completed",
        "H1",
        serde_json::json!({"count":rows.len(),"ms":t0.elapsed().as_millis()}),
        "initial",
    );
    Ok(Json(
        rows.into_iter()
            .map(|r| AppRecordDto {
                app_id: r.app_id,
                display_name: r.display_name,
                description: r.description,
                enabled: r.enabled,
            })
            .collect(),
    ))
}

async fn upsert_app(
    headers: HeaderMap,
    State(state): State<AppState>,
    Json(mut body): Json<AppRecordDto>,
) -> Result<StatusCode, (StatusCode, String)> {
    require_admin_or_boss(&headers, &state.store).await?;
    if body.app_id.trim().is_empty() {
        return Err((StatusCode::BAD_REQUEST, "app_id required".into()));
    }
    if body.display_name.trim().is_empty() {
        body.display_name = body.app_id.clone();
    }
    let rec = AppRecord {
        app_id: body.app_id,
        display_name: body.display_name,
        description: body.description,
        enabled: body.enabled,
    };
    let audit = AuditLogRecord::management(
        "control-plane-admin",
        management_actor(&headers),
        rec.app_id.clone(),
        format!("/api/v1/apps/{}", rec.app_id),
        "PUT",
    );
    AuditLogsStore::apply_security_mutation(
        &state.store,
        &SecurityMutation::UpsertApp(rec),
        &audit,
    )
    .await
    .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    Ok(StatusCode::CREATED)
}

async fn delete_app(
    headers: HeaderMap,
    State(state): State<AppState>,
    Path(app_id): Path<String>,
) -> Result<StatusCode, (StatusCode, String)> {
    require_admin_or_boss(&headers, &state.store).await?;
    let audit = AuditLogRecord::management(
        "control-plane-admin",
        management_actor(&headers),
        app_id.clone(),
        format!("/api/v1/apps/{app_id}"),
        "DELETE",
    );
    AuditLogsStore::apply_security_mutation(
        &state.store,
        &SecurityMutation::DeleteApp(app_id),
        &audit,
    )
    .await
    .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    Ok(StatusCode::NO_CONTENT)
}

async fn list_api_routes(
    headers: HeaderMap,
    State(state): State<AppState>,
    Query(q): Query<ApiRoutesQuery>,
) -> Result<Json<Vec<ApiRouteRecordDto>>, (StatusCode, String)> {
    require_admin_or_boss(&headers, &state.store).await?;
    let rows = ApiRoutesStore::list_by_app(&state.store, q.app_id.as_deref())
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    Ok(Json(
        rows.into_iter()
            .map(|r| ApiRouteRecordDto {
                id: r.id,
                app_id: r.app_id,
                method: r.method,
                path: r.path,
                enabled: r.enabled,
                description: r.description,
            })
            .collect(),
    ))
}

async fn upsert_api_route(
    headers: HeaderMap,
    State(state): State<AppState>,
    Json(mut body): Json<ApiRouteRecordDto>,
) -> Result<StatusCode, (StatusCode, String)> {
    require_admin_or_boss(&headers, &state.store).await?;
    if body.id.trim().is_empty() {
        // deterministic id helps OpenAPI batch create
        body.id = format!(
            "{}:{}:{}",
            body.app_id,
            body.method.to_uppercase(),
            body.path
        );
    }
    let rec = ApiRouteRecord {
        id: body.id,
        app_id: body.app_id.clone(),
        method: body.method.to_uppercase(),
        path: body.path,
        enabled: body.enabled,
        description: body.description,
    };
    let audit = AuditLogRecord::management(
        "control-plane-admin",
        management_actor(&headers),
        rec.app_id.clone(),
        format!("/api/v1/api-routes/{}", rec.id),
        "PUT",
    );
    AuditLogsStore::apply_security_mutation(
        &state.store,
        &SecurityMutation::UpsertApiRoute(rec),
        &audit,
    )
    .await
    .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    Ok(StatusCode::CREATED)
}

async fn delete_api_route(
    headers: HeaderMap,
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<StatusCode, (StatusCode, String)> {
    require_admin_or_boss(&headers, &state.store).await?;
    let audit = AuditLogRecord::management(
        "control-plane-admin",
        management_actor(&headers),
        "",
        format!("/api/v1/api-routes/{id}"),
        "DELETE",
    );
    AuditLogsStore::apply_security_mutation(
        &state.store,
        &SecurityMutation::DeleteApiRoute(id),
        &audit,
    )
    .await
    .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    Ok(StatusCode::NO_CONTENT)
}

async fn post_audit_log(
    headers: HeaderMap,
    State(state): State<AppState>,
    Json(mut body): Json<AuditLogDto>,
) -> Result<StatusCode, (StatusCode, String)> {
    require_admin_or_boss(&headers, &state.store).await?;
    let t0 = Instant::now();
    body.id = require_uuid_v4_or_generate(&body.id)?;
    if body.ts_ms <= 0 {
        body.ts_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as i64)
            .unwrap_or(0);
    }
    let row = AuditLogRecord {
        id: body.id,
        ts_ms: body.ts_ms,
        service: body.service,
        user_id: body.user_id,
        app_id: body.app_id,
        path: body.path,
        method: body.method,
        latency_ms: body.latency_ms,
        decision: body.decision,
        result: body.result,
        trace_id: body.trace_id,
        extra_json: body.extra_json,
    };
    AuditLogsStore::insert(&state.store, &row)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    debug_log(
        "control-plane-admin/main.rs:post_audit_log",
        "audit log inserted",
        "H3",
        serde_json::json!({"service":row.service,"user_id":row.user_id,"app_id":row.app_id,"path":row.path,"ms":t0.elapsed().as_millis()}),
        "initial",
    );
    Ok(StatusCode::CREATED)
}

async fn list_audit_logs(
    headers: HeaderMap,
    State(state): State<AppState>,
    Query(q): Query<AuditLogsQuery>,
) -> Result<Json<Vec<AuditLogDto>>, (StatusCode, String)> {
    let t0 = Instant::now();
    require_admin_or_boss(&headers, &state.store).await?;
    let user_id_q = q.user_id.clone();
    let app_id_q = q.app_id.clone();
    let mut rows = AuditLogsStore::list(
        &state.store,
        &AuditLogFilter {
            from_ts_ms: q.from_ts_ms,
            to_ts_ms: q.to_ts_ms,
            user_id: q.user_id,
            app_id: q.app_id,
            limit: q.limit.unwrap_or(200),
        },
    )
    .await
    .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    if let Some(service) = q.service.as_ref() {
        rows.retain(|r| &r.service == service);
    }
    if let Some(result) = q.result.as_ref() {
        rows.retain(|r| r.result.contains(result));
    }
    if let Some(decision) = q.decision.as_ref() {
        rows.retain(|r| r.decision.contains(decision));
    }
    if let Some(path_contains) = q.path_contains.as_ref() {
        rows.retain(|r| r.path.contains(path_contains));
    }
    if let Some(dept) = q.department.as_ref() {
        rows.retain(|r| {
            r.extra_json
                .contains(&format!("\"department\":\"{}\"", dept))
        });
    }
    debug_log(
        "control-plane-admin/main.rs:list_audit_logs",
        "audit logs queried",
        "H3",
        serde_json::json!({
            "count":rows.len(),
            "user_id":user_id_q,
            "app_id":app_id_q,
            "service":q.service,
            "result":q.result,
            "decision":q.decision,
            "department":q.department,
            "ms":t0.elapsed().as_millis()
        }),
        "initial",
    );
    Ok(Json(
        rows.into_iter()
            .map(|r| AuditLogDto {
                id: r.id,
                ts_ms: r.ts_ms,
                service: r.service,
                user_id: r.user_id,
                app_id: r.app_id,
                path: r.path,
                method: r.method,
                latency_ms: r.latency_ms,
                decision: r.decision,
                result: r.result,
                trace_id: r.trace_id,
                extra_json: r.extra_json,
            })
            .collect(),
    ))
}

async fn post_fault_event(
    headers: HeaderMap,
    State(state): State<AppState>,
    Json(mut body): Json<FaultEventDto>,
) -> Result<StatusCode, (StatusCode, String)> {
    require_admin_or_boss(&headers, &state.store).await?;
    body.id = require_uuid_v4_or_generate(&body.id)?;
    if body.ts_ms <= 0 {
        body.ts_ms = now_epoch_ms();
    }
    let row = FaultEventRecord {
        id: body.id,
        ts_ms: body.ts_ms,
        service: body.service,
        event_type: body.event_type,
        severity: body.severity,
        path: body.path,
        method: body.method,
        latency_ms: body.latency_ms,
        baseline_ms: body.baseline_ms,
        threshold_ms: body.threshold_ms,
        status_code: body.status_code,
        result: body.result,
        trace_id: body.trace_id,
        source: body.source,
        resolved_at_ms: body.resolved_at_ms,
        meta_json: body.meta_json,
    };
    FaultEventsStore::insert(&state.store, &row)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    Ok(StatusCode::CREATED)
}

async fn list_fault_events(
    headers: HeaderMap,
    State(state): State<AppState>,
    Query(q): Query<FaultEventsQuery>,
) -> Result<Json<Vec<FaultEventDto>>, (StatusCode, String)> {
    require_admin_or_boss(&headers, &state.store).await?;
    let rows = FaultEventsStore::list(
        &state.store,
        &FaultEventFilter {
            from_ts_ms: q.from_ts_ms,
            to_ts_ms: q.to_ts_ms,
            service: q.service,
            event_type: q.event_type,
            severity: q.severity,
            result: q.result,
            source: q.source,
            limit: q.limit.unwrap_or(200),
        },
    )
    .await
    .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    Ok(Json(
        rows.into_iter()
            .map(|r| FaultEventDto {
                id: r.id,
                ts_ms: r.ts_ms,
                service: r.service,
                event_type: r.event_type,
                severity: r.severity,
                path: r.path,
                method: r.method,
                latency_ms: r.latency_ms,
                baseline_ms: r.baseline_ms,
                threshold_ms: r.threshold_ms,
                status_code: r.status_code,
                result: r.result,
                trace_id: r.trace_id,
                source: r.source,
                resolved_at_ms: r.resolved_at_ms,
                meta_json: r.meta_json,
            })
            .collect(),
    ))
}

async fn get_fault_injection_toggle(
    headers: HeaderMap,
    State(state): State<AppState>,
) -> Result<Json<FaultInjectionToggle>, (StatusCode, String)> {
    require_admin_or_boss(&headers, &state.store).await?;
    Ok(Json(state.fault_toggle.read().await.clone()))
}

async fn put_fault_injection_toggle(
    headers: HeaderMap,
    State(state): State<AppState>,
    Json(body): Json<FaultInjectionUpdate>,
) -> Result<Json<FaultInjectionToggle>, (StatusCode, String)> {
    require_admin_or_boss(&headers, &state.store).await?;
    let mut t = state.fault_toggle.write().await;
    if let Some(v) = body.enabled {
        t.enabled = v;
    }
    if let Some(v) = body.ttl_sec {
        t.ttl_sec = v.max(1);
    }
    if let Some(v) = body.mode {
        t.mode = v;
    }
    if let Some(v) = body.service {
        t.service = v;
    }
    if let Some(v) = body.path_contains {
        t.path_contains = v;
    }
    if let Some(v) = body.delay_ms {
        t.delay_ms = v.max(0);
    }
    if let Some(v) = body.status_code {
        t.status_code = v;
    }
    if let Some(v) = body.hit_percent {
        t.hit_percent = v.min(100);
    }
    if t.enabled {
        t.expires_at_ms = now_epoch_ms() + (t.ttl_sec as i64 * 1000);
    } else {
        t.expires_at_ms = 0;
    }
    Ok(Json(t.clone()))
}

fn redact_user_id(v: &str) -> String {
    if v.is_empty() {
        return "".into();
    }
    if v.len() <= 2 {
        return "*".repeat(v.len());
    }
    format!("{}***", &v[..2])
}

async fn public_audit_logs(
    headers: HeaderMap,
    State(state): State<AppState>,
) -> Result<Json<Vec<AuditLogDto>>, (StatusCode, String)> {
    require_public_readonly(&headers)?;
    let rows = AuditLogsStore::list(
        &state.store,
        &AuditLogFilter {
            from_ts_ms: Some(now_epoch_ms() - 60 * 60 * 1000),
            to_ts_ms: None,
            user_id: None,
            app_id: None,
            limit: 100,
        },
    )
    .await
    .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    Ok(Json(
        rows.into_iter()
            .map(|r| AuditLogDto {
                id: r.id,
                ts_ms: r.ts_ms,
                service: r.service,
                user_id: redact_user_id(&r.user_id),
                app_id: r.app_id,
                path: r.path,
                method: r.method,
                latency_ms: r.latency_ms,
                decision: r.decision,
                result: r.result,
                trace_id: r.trace_id,
                extra_json: r.extra_json,
            })
            .collect(),
    ))
}

async fn public_fault_events(
    headers: HeaderMap,
    State(state): State<AppState>,
) -> Result<Json<Vec<FaultEventDto>>, (StatusCode, String)> {
    require_public_readonly(&headers)?;
    let rows = FaultEventsStore::list(
        &state.store,
        &FaultEventFilter {
            from_ts_ms: Some(now_epoch_ms() - 60 * 60 * 1000),
            to_ts_ms: None,
            service: None,
            event_type: None,
            severity: None,
            result: None,
            source: None,
            limit: 100,
        },
    )
    .await
    .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    Ok(Json(
        rows.into_iter()
            .map(|r| FaultEventDto {
                id: r.id,
                ts_ms: r.ts_ms,
                service: r.service,
                event_type: r.event_type,
                severity: r.severity,
                path: r.path,
                method: r.method,
                latency_ms: r.latency_ms,
                baseline_ms: r.baseline_ms,
                threshold_ms: r.threshold_ms,
                status_code: r.status_code,
                result: r.result,
                trace_id: r.trace_id,
                source: r.source,
                resolved_at_ms: r.resolved_at_ms,
                meta_json: r.meta_json,
            })
            .collect(),
    ))
}

async fn public_security_overview(
    headers: HeaderMap,
    State(state): State<AppState>,
) -> Result<Json<PublicSecurityOverviewDto>, (StatusCode, String)> {
    require_public_readonly(&headers)?;
    let audit_rows = AuditLogsStore::list(
        &state.store,
        &AuditLogFilter {
            from_ts_ms: Some(now_epoch_ms() - 60 * 60 * 1000),
            to_ts_ms: None,
            user_id: None,
            app_id: None,
            limit: 200,
        },
    )
    .await
    .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    let fault_rows = FaultEventsStore::list(
        &state.store,
        &FaultEventFilter {
            from_ts_ms: Some(now_epoch_ms() - 60 * 60 * 1000),
            to_ts_ms: None,
            service: None,
            event_type: None,
            severity: None,
            result: None,
            source: None,
            limit: 200,
        },
    )
    .await
    .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    let mut svc_counts = HashMap::<String, usize>::new();
    for row in &fault_rows {
        *svc_counts.entry(row.service.clone()).or_insert(0) += 1;
    }
    let mut top_services: Vec<ServiceCountDto> = svc_counts
        .into_iter()
        .map(|(service, count)| ServiceCountDto { service, count })
        .collect();
    top_services.sort_by(|a, b| {
        b.count
            .cmp(&a.count)
            .then_with(|| a.service.cmp(&b.service))
    });
    top_services.truncate(5);
    Ok(Json(PublicSecurityOverviewDto {
        audit_count: audit_rows.len(),
        fault_event_count: fault_rows.len(),
        critical_fault_count: fault_rows
            .iter()
            .filter(|r| r.severity == "critical")
            .count(),
        top_services,
        note: "公开安全演示入口仅提供脱敏只读数据，不包含写入或真实破坏性测试能力。".to_string(),
    }))
}

fn default_scheme() -> String {
    "http".into()
}

fn now_epoch_sec() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn now_epoch_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

async fn validate_persisted_route_configuration(store: &StorageStore) -> anyhow::Result<()> {
    let routes = RoutesStore::load_all(store).await?;
    let upstreams = RoutesStore::load_all_intranet_upstreams(store).await?;
    shared_storage::validate_route_configuration_snapshot(&routes, &upstreams).map_err(|error| {
        anyhow::anyhow!(
            "persisted route configuration is unsafe; repair it before starting control-plane convergence: {error}"
        )
    })
}

fn parse_bool_env(name: &str, default: bool) -> bool {
    std::env::var(name)
        .map(|v| matches!(v.to_lowercase().as_str(), "1" | "true" | "yes" | "on"))
        .unwrap_or(default)
}

fn config_sync_retry_delay_ms(attempt_count: i64, job_id: &str) -> i64 {
    let exponent = attempt_count.saturating_sub(1).clamp(0, 6) as u32;
    let base = 1_000_i64.saturating_mul(1_i64 << exponent).min(60_000);
    let jitter = job_id.bytes().fold(0_u64, |acc, byte| {
        acc.wrapping_mul(31).wrapping_add(byte as u64)
    }) % 251;
    base + jitter as i64
}

async fn apply_config_sync_job(
    http: &reqwest::Client,
    apisix_cfg: &apisix::ApisixPushConfig,
    store: &StorageStore,
    job: &ConfigSyncJob,
) -> anyhow::Result<()> {
    if job.target != "APISIX" {
        anyhow::bail!(
            "unsupported config sync target {}/{} for job {}",
            job.target,
            job.resource_type,
            job.job_id
        );
    }
    let applied_operation = match job.resource_type.as_str() {
        "ROUTE" => apisix::converge_app_route(http, apisix_cfg, store, &job.app_id).await?,
        "ROUTE_ID" if job.operation == ConfigSyncOperation::Delete => {
            // Establish the current deterministic route (or its desired
            // absence) before removing a legacy/extra ID. Together with the
            // app-scoped lease this prevents a tombstone from creating a gap
            // or racing a recreate onto the new ID.
            let converged =
                apisix::converge_app_route(http, apisix_cfg, store, &job.app_id).await?;
            if apisix::route_id_for_app(&job.app_id) != job.resource_id {
                apisix::delete_route_by_id_for_app(http, apisix_cfg, &job.resource_id, &job.app_id)
                    .await?;
                "DELETE"
            } else {
                converged
            }
        }
        _ => anyhow::bail!(
            "unsupported config sync resource type/operation {}/{} for job {}",
            job.resource_type,
            job.operation.as_str(),
            job.job_id
        ),
    };
    if applied_operation != job.operation.as_str() {
        info!(
            job_id = %job.job_id,
            recorded_operation = job.operation.as_str(),
            applied_operation,
            "config sync job was reconciled to newer control-plane intent"
        );
    }
    Ok(())
}

async fn run_config_sync_worker(
    store: StorageStore,
    http: reqwest::Client,
    apisix_cfg: apisix::ApisixPushConfig,
) {
    let worker_id = format!("control-plane-admin:{}", uuid::Uuid::new_v4());
    let poll_ms = std::env::var("SAG_CONFIG_SYNC_POLL_MS")
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .unwrap_or(500)
        .max(100);
    let batch_size = std::env::var("SAG_CONFIG_SYNC_BATCH_SIZE")
        .ok()
        .and_then(|value| value.parse::<usize>().ok())
        .unwrap_or(10)
        .clamp(1, 100);
    let operation_timeout_ms = std::env::var("SAG_CONFIG_SYNC_OPERATION_TIMEOUT_MS")
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .unwrap_or(10_000)
        .clamp(1_000, 60_000);
    let operation_timeout = Duration::from_millis(operation_timeout_ms);
    let unknown_outcome_isolation_ms =
        std::env::var("SAG_CONFIG_SYNC_UNKNOWN_OUTCOME_ISOLATION_MS")
            .ok()
            .and_then(|value| value.parse::<i64>().ok())
            .unwrap_or(60_000)
            .clamp(10_000, 300_000);
    // Work is claimed one job at a time, immediately before its external I/O.
    // A failed/timed-out external write keeps this lease for the full isolation
    // period. Successful operations release it immediately.
    let lease_ms =
        (operation_timeout.as_millis() as i64).saturating_add(unknown_outcome_isolation_ms);
    let mut interval = tokio::time::interval(Duration::from_millis(poll_ms));
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

    loop {
        interval.tick().await;
        for _ in 0..batch_size {
            let mut claimed = match ConfigSyncStore::claim_due_jobs(
                &store,
                &worker_id,
                now_epoch_ms(),
                lease_ms,
                1,
            )
            .await
            {
                Ok(jobs) => jobs,
                Err(error) => {
                    warn!(%error, "config sync job claim failed");
                    metrics::counter!("config_sync_worker_errors_total", "stage" => "claim")
                        .increment(1);
                    break;
                }
            };
            let Some(job) = claimed.pop() else {
                break;
            };
            let result = tokio::time::timeout(
                operation_timeout,
                apply_config_sync_job(&http, &apisix_cfg, &store, &job),
            )
            .await;
            match result {
                Ok(Ok(())) => {
                    match ConfigSyncStore::mark_applied(
                        &store,
                        &job.job_id,
                        &worker_id,
                        now_epoch_ms(),
                    )
                    .await
                    {
                        Ok(true) => {
                            metrics::counter!("config_sync_jobs_total", "status" => "applied")
                                .increment(1);
                        }
                        Ok(false) => {
                            warn!(
                                job_id = %job.job_id,
                                "config sync result was superseded or its lease was lost"
                            );
                            if let Err(error) = ConfigSyncStore::release_lease(
                                &store,
                                &job.job_id,
                                &worker_id,
                                now_epoch_ms(),
                            )
                            .await
                            {
                                warn!(job_id = %job.job_id, %error, "failed to release superseded config sync lease");
                            }
                        }
                        Err(error) => {
                            warn!(job_id = %job.job_id, %error, "failed to mark config sync job applied")
                        }
                    }
                }
                outcome => {
                    let error = match outcome {
                        Ok(Err(error)) => error.to_string(),
                        Err(_) => format!(
                            "APISIX operation exceeded {}ms",
                            operation_timeout.as_millis()
                        ),
                        Ok(Ok(())) => unreachable!(),
                    };
                    let failed_at_ms = now_epoch_ms();
                    let next_attempt_at_ms = failed_at_ms
                        .saturating_add(config_sync_retry_delay_ms(job.attempt_count, &job.job_id));
                    match ConfigSyncStore::mark_failed_retaining_lease(
                        &store,
                        &job.job_id,
                        &worker_id,
                        &error,
                        failed_at_ms,
                        next_attempt_at_ms,
                    )
                    .await
                    {
                        Ok(true) => {
                            metrics::counter!(
                                "config_sync_jobs_total",
                                "status" => "outcome_unknown"
                            )
                            .increment(1);
                            warn!(
                                job_id = %job.job_id,
                                app_id = %job.app_id,
                                attempt = job.attempt_count,
                                retry_at_ms = next_attempt_at_ms,
                                quarantine_until_ms = ?job.lease_expires_at_ms,
                                %error,
                                "config sync job external outcome may be unknown; app lease retained for isolation"
                            );
                        }
                        Ok(false) => {
                            warn!(
                                job_id = %job.job_id,
                                %error,
                                "failed config sync result was superseded or its lease was lost; retaining the lease until expiry because the external outcome may be unknown"
                            );
                        }
                        Err(store_error) => warn!(
                            job_id = %job.job_id,
                            error = %store_error,
                            "failed to persist config sync failure"
                        ),
                    }
                }
            }
        }
    }
}

async fn reconcile_apisix_managed_routes(state: &AppState) {
    let Some(apisix_cfg) = state.apisix.as_ref() else {
        return;
    };
    let generation_before = match ConfigSyncStore::current_generation(&state.store).await {
        Ok(generation) => generation,
        Err(error) => {
            warn!(%error, "APISIX reconciliation could not read desired generation");
            return;
        }
    };
    let drift = match apisix::inspect_managed_route_drift(&state.http, apisix_cfg, &state.store)
        .await
    {
        Ok(drift) => drift,
        Err(error) => {
            metrics::counter!("apisix_reconcile_errors_total", "stage" => "inspect").increment(1);
            warn!(%error, "APISIX actual-state reconciliation inspection failed");
            return;
        }
    };
    let generation_after = match ConfigSyncStore::current_generation(&state.store).await {
        Ok(generation) => generation,
        Err(error) => {
            warn!(%error, "APISIX reconciliation could not recheck desired generation");
            return;
        }
    };
    if generation_before != generation_after {
        metrics::counter!("apisix_reconcile_skipped_total", "reason" => "generation_changed")
            .increment(1);
        info!(
            generation_before,
            generation_after,
            "APISIX reconciliation skipped a snapshot that changed during inspection"
        );
        return;
    }
    metrics::gauge!("config_desired_generation").set(generation_after as f64);

    let drift_count = drift.upsert_app_ids.len() + drift.delete_routes.len();
    metrics::gauge!("apisix_managed_route_drift").set(drift_count as f64);
    if drift_count > 0 {
        warn!(
            upsert_count = drift.upsert_app_ids.len(),
            delete_count = drift.delete_routes.len(),
            "APISIX managed route drift detected"
        );
    }

    let queued_at_ms = now_epoch_ms();
    let mut action_failed = false;
    for app_id in drift.upsert_app_ids {
        let draft = ConfigSyncJobDraft {
            generation: generation_after,
            target: "APISIX".into(),
            resource_type: "ROUTE".into(),
            resource_id: app_id.clone(),
            app_id: app_id.clone(),
            operation: ConfigSyncOperation::Upsert,
            payload_json: None,
            next_attempt_at_ms: queued_at_ms,
        };
        match ConfigSyncStore::requeue_job(&state.store, &draft, queued_at_ms).await {
            Ok(true) => {
                metrics::counter!("apisix_reconcile_actions_total", "operation" => "upsert", "status" => "queued").increment(1);
            }
            Ok(false) => {
                metrics::counter!("apisix_reconcile_actions_total", "operation" => "upsert", "status" => "inflight").increment(1);
            }
            Err(error) => {
                action_failed = true;
                metrics::counter!("apisix_reconcile_actions_total", "operation" => "upsert", "status" => "failed").increment(1);
                warn!(%app_id, %error, "failed to enqueue APISIX reconciliation upsert");
            }
        }
    }
    for deletion in drift.delete_routes {
        let route_id = deletion.route_id;
        let Some(app_id) = deletion.app_id else {
            warn!(%route_id, "owned APISIX route lacks sag-app-id; refusing reconciliation delete");
            continue;
        };
        let deterministic_id = apisix::route_id_for_app(&app_id);
        let (resource_type, resource_id) = if deterministic_id == route_id {
            ("ROUTE", app_id.clone())
        } else {
            ("ROUTE_ID", route_id.clone())
        };
        let draft = ConfigSyncJobDraft {
            generation: generation_after,
            target: "APISIX".into(),
            resource_type: resource_type.into(),
            resource_id,
            app_id: app_id.clone(),
            operation: ConfigSyncOperation::Delete,
            payload_json: None,
            next_attempt_at_ms: queued_at_ms,
        };
        match ConfigSyncStore::requeue_job(&state.store, &draft, queued_at_ms).await {
            Ok(true) => {
                metrics::counter!("apisix_reconcile_actions_total", "operation" => "delete", "status" => "queued").increment(1);
            }
            Ok(false) => {
                metrics::counter!("apisix_reconcile_actions_total", "operation" => "delete", "status" => "inflight").increment(1);
            }
            Err(error) => {
                action_failed = true;
                metrics::counter!("apisix_reconcile_actions_total", "operation" => "delete", "status" => "failed").increment(1);
                warn!(%route_id, %app_id, %error, "failed to enqueue APISIX reconciliation delete");
            }
        }
    }
    if action_failed {
        metrics::counter!("apisix_reconcile_errors_total", "stage" => "queue").increment(1);
    } else {
        metrics::gauge!("apisix_reconcile_last_success_timestamp_seconds")
            .set(now_epoch_ms() as f64 / 1_000.0);
    }
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

fn should_hit_toggle(toggle: &FaultInjectionToggle, path: &str) -> bool {
    if !toggle.enabled || toggle.service != "control-plane-admin" {
        return false;
    }
    if toggle.expires_at_ms > 0 && now_epoch_ms() > toggle.expires_at_ms {
        return false;
    }
    if !toggle.path_contains.is_empty() && !path.contains(&toggle.path_contains) {
        return false;
    }
    let pct = toggle.hit_percent.min(100) as u64;
    if pct >= 100 {
        return true;
    }
    let n = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0);
    (n % 100) < pct
}

fn minute_floor(ts_sec: u64) -> i64 {
    ((ts_sec / 60) * 60) as i64
}

fn approx_unique(v: i64) -> i64 {
    if v <= 0 {
        return 0;
    }
    let f = (v as f64).sqrt();
    f.round() as i64
}

async fn prom_scalar(client: &reqwest::Client, base: &str, query: &str) -> Option<f64> {
    let url = format!("{}/api/v1/query", base.trim_end_matches('/'));
    let resp = client
        .get(url)
        .query(&[("query", query)])
        .send()
        .await
        .ok()?;
    let body = resp.text().await.ok()?;
    let v: serde_json::Value = serde_json::from_str(&body).ok()?;
    let arr = v.get("data")?.get("result")?.as_array()?;
    let first = arr.first()?;
    let value = first.get("value")?.as_array()?;
    let n = value.get(1)?.as_str()?.parse::<f64>().ok()?;
    if n.is_finite() {
        Some(n)
    } else {
        None
    }
}

async fn aggregate_and_persist_app_metrics(state: &AppState) -> Result<(), String> {
    let prom_base =
        std::env::var("SAG_PROM_BASE_URL").unwrap_or_else(|_| "http://prometheus:9090".to_string());
    let routes = RoutesStore::load_all(&state.store)
        .await
        .map_err(|e| e.to_string())?;
    let mut app_ids: Vec<String> = routes.into_iter().map(|r| r.app_id).collect();
    app_ids.sort();
    app_ids.dedup();
    let ts = minute_floor(now_epoch_sec());
    for app_id in app_ids {
        let qps = prom_scalar(
            &state.http,
            &prom_base,
            &format!("(sum(rate(agent_forward_total{{app_id=\"{app_id}\",result=\"ok\"}}[1m])) or vector(0))"),
        )
        .await
        .unwrap_or(0.0);
        let err4 = prom_scalar(
            &state.http,
            &prom_base,
            "(sum(rate(connector_forward_total{status=~\"4..\"}[1m])) or vector(0))",
        )
        .await
        .unwrap_or(0.0);
        let err5 = prom_scalar(
            &state.http,
            &prom_base,
            "(sum(rate(connector_forward_total{status=~\"5..\"}[1m])) or vector(0))",
        )
        .await
        .unwrap_or(0.0);
        let request_count = (qps * 60.0).round() as i64;
        let err4_count = (err4 * 60.0).round() as i64;
        let err5_count = (err5 * 60.0).round() as i64;
        let rec = AppMetricMinuteRecord {
            ts_minute: ts,
            app_id,
            request_count,
            pv_count: request_count,
            // NOTE: current agent metrics do not expose user/ip cardinality; use deterministic approximation
            // until dedicated event aggregation is wired.
            uv_count: approx_unique(request_count),
            unique_ip_count: approx_unique(request_count),
            err4xx_count: err4_count,
            err5xx_count: err5_count,
            qps_avg: qps,
        };
        AppMetricsStore::upsert_minute(&state.store, &rec)
            .await
            .map_err(|e| e.to_string())?;
    }
    Ok(())
}

fn map_metric_point(r: &AppMetricMinuteRecord) -> AppMetricsPointDto {
    let req = r.request_count.max(0) as f64;
    let err4 = r.err4xx_count.max(0) as f64;
    let err5 = r.err5xx_count.max(0) as f64;
    AppMetricsPointDto {
        ts_minute: r.ts_minute,
        request_count: r.request_count,
        pv_count: r.pv_count,
        uv_count: r.uv_count,
        unique_ip_count: r.unique_ip_count,
        err4xx_count: r.err4xx_count,
        err5xx_count: r.err5xx_count,
        qps_avg: r.qps_avg,
        err4xx_rate: if req > 0.0 { err4 / req } else { 0.0 },
        err5xx_rate: if req > 0.0 { err5 / req } else { 0.0 },
    }
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
        .probe(timeout, async move { store.health_check().await.is_ok() })
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
    if fault_injection_env_enabled() && !path.starts_with("/api/v1/fault-injection") {
        let toggle = state.fault_toggle.read().await.clone();
        if should_hit_toggle(&toggle, &path) {
            let mode = toggle.mode.clone();
            if mode == "delay" {
                tokio::time::sleep(std::time::Duration::from_millis(
                    toggle.delay_ms.max(0) as u64
                ))
                .await;
            }
            let status = if mode == "http_status" || mode == "timeout" {
                StatusCode::from_u16(toggle.status_code).unwrap_or(StatusCode::GATEWAY_TIMEOUT)
            } else {
                StatusCode::OK
            };
            if mode == "timeout" {
                tokio::time::sleep(std::time::Duration::from_millis(
                    toggle.delay_ms.max(1000) as u64
                ))
                .await;
            }
            let event = FaultEventRecord {
                id: uuid::Uuid::new_v4().to_string(),
                ts_ms: now_epoch_ms(),
                service: "control-plane-admin".to_string(),
                event_type: if mode == "timeout" {
                    "timeout".to_string()
                } else {
                    "injected_fault".to_string()
                },
                severity: "critical".to_string(),
                path: path.clone(),
                method: method.clone(),
                latency_ms: toggle.delay_ms.max(0),
                baseline_ms: 0,
                threshold_ms: toggle.delay_ms.max(0),
                status_code: status.as_u16() as i64,
                result: status.as_u16().to_string(),
                trace_id: "".to_string(),
                source: "injector".to_string(),
                resolved_at_ms: None,
                meta_json: serde_json::json!({"mode":mode,"toggle_ttl_sec":toggle.ttl_sec})
                    .to_string(),
            };
            let store = state.store.clone();
            tokio::spawn(async move {
                let _ = FaultEventsStore::insert(&store, &event).await;
            });
            if mode == "http_status" || mode == "timeout" {
                return (status, format!("injected {}", mode)).into_response();
            }
        }
    }
    let identity = parse_identity(req.headers(), &state.store)
        .await
        .unwrap_or_default();
    let user_id_hdr = req
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
    let trace_id = req
        .headers()
        .get("x-trace-id")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_string();
    let res = next.run(req).await;
    let status = res.status().as_u16().to_string();
    let method2 = method.clone();
    let elapsed = start.elapsed().as_secs_f64();
    let c = metrics::counter!(
        "http_requests_total",
        "service" => "control-plane-admin",
        "method" => method.clone(),
        "path" => path.clone(),
        "status" => status.clone()
    );
    c.increment(1);
    let h = metrics::histogram!(
        "http_request_duration_seconds",
        "service" => "control-plane-admin",
        "method" => method2.clone(),
        "path" => path.clone(),
        "status" => status.clone()
    );
    h.record(elapsed);
    let log_row = AuditLogRecord {
        id: uuid::Uuid::new_v4().to_string(),
        ts_ms: std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as i64)
            .unwrap_or(0),
        service: "control-plane-admin".to_string(),
        user_id: if !user_id_hdr.is_empty() {
            user_id_hdr
        } else {
            identity.user_id
        },
        app_id,
        path: path.clone(),
        method: method2,
        latency_ms: (elapsed * 1000.0) as i64,
        decision: "observe".to_string(),
        result: status.clone(),
        trace_id,
        extra_json: serde_json::json!({
            "username": identity.username,
            "roles": identity.roles,
            "department": derive_department(&identity.roles)
        })
        .to_string(),
    };
    let _ = state.audit_writer.try_record(log_row);
    res
}

async fn list_routes(
    headers: HeaderMap,
    State(state): State<AppState>,
    Query(q): Query<RoutesQuery>,
) -> Result<RouteListResponse, (StatusCode, String)> {
    let t0 = Instant::now();
    if !has_valid_agent_sync_token(&headers) {
        require_admin_or_boss(&headers, &state.store).await?;
    }
    let filtered_app = q.app_id.clone().unwrap_or_default();
    let key = format!("app={filtered_app}");
    if state.route_cache_enabled {
        let current_generation = ConfigSyncStore::current_generation(&state.store)
            .await
            .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
        if let Some(cached) = state
            .route_cache
            .get(&key)
            .await
            .filter(|entry| entry.generation == current_generation)
        {
            metrics::gauge!("config_desired_generation").set(current_generation as f64);
            let hit = metrics::counter!("cache_hit_total", "service" => "control-plane-admin", "cache" => "agent_routes");
            hit.increment(1);
            let rate = metrics::counter!("route_cache_hit_rate", "result" => "hit");
            rate.increment(1);
            return route_snapshot_response(cached);
        }
        let miss = metrics::counter!("cache_miss_total", "service" => "control-plane-admin", "cache" => "agent_routes");
        miss.increment(1);
        let rate = metrics::counter!("route_cache_hit_rate", "result" => "miss");
        rate.increment(1);
    }

    let load_snapshot = || async {
        let snapshot = ConfigSyncStore::load_route_snapshot(&state.store)
            .await
            .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
        let mut rows = snapshot.routes;
        if !filtered_app.is_empty() {
            rows.retain(|r| r.app_id == filtered_app);
        }
        let routes = rows
            .into_iter()
            .map(|r| TunnelRouteRecordDto {
                host: r.host,
                app_id: r.app_id,
                connector_endpoint: r.connector_endpoint,
                require_healthy_tunnel: r.require_healthy_tunnel,
            })
            .collect::<Vec<_>>();
        Ok::<RouteCacheEntry, (StatusCode, String)>(RouteCacheEntry {
            generation: snapshot.generation,
            routes,
        })
    };
    let out = if state.route_cache_enabled {
        let fresh = load_snapshot().await?;
        state.route_cache.insert(key, fresh.clone()).await;
        fresh
    } else {
        load_snapshot().await?
    };
    debug_log(
        "control-plane-admin/main.rs:list_routes",
        "list routes completed",
        "H5",
        serde_json::json!({"count":out.routes.len(),"generation":out.generation,"ms":t0.elapsed().as_millis(),"filtered_app": if filtered_app.is_empty() { serde_json::Value::Null } else { serde_json::Value::String(filtered_app) }}),
        "initial",
    );
    metrics::gauge!("config_desired_generation").set(out.generation as f64);
    route_snapshot_response(out)
}

fn route_snapshot_response(
    snapshot: RouteCacheEntry,
) -> Result<RouteListResponse, (StatusCode, String)> {
    let mut headers = HeaderMap::new();
    let generation = HeaderValue::from_str(&snapshot.generation.to_string())
        .map_err(|error| (StatusCode::INTERNAL_SERVER_ERROR, error.to_string()))?;
    headers.insert("x-sag-config-generation", generation);
    Ok((headers, Json(snapshot.routes)))
}

fn agent_apply_active_window_ms() -> i64 {
    let seconds = std::env::var("SAG_AGENT_APPLY_ACTIVE_WINDOW_SEC")
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .unwrap_or(300)
        .clamp(30, 86_400);
    i64::try_from(seconds.saturating_mul(1_000)).unwrap_or(i64::MAX)
}

fn agent_apply_retention_ms() -> i64 {
    let seconds = std::env::var("SAG_AGENT_APPLY_RETENTION_SEC")
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .unwrap_or(7 * 24 * 60 * 60)
        .clamp(300, 365 * 24 * 60 * 60);
    i64::try_from(seconds.saturating_mul(1_000))
        .unwrap_or(i64::MAX)
        .max(agent_apply_active_window_ms())
}

async fn refresh_agent_apply_metrics(store: &StorageStore, desired_generation: i64) {
    let active_cutoff_ms = now_epoch_ms().saturating_sub(agent_apply_active_window_ms());
    match ConfigSyncStore::list_agent_applies(store).await {
        Ok(applies) => {
            for apply in applies {
                let active = apply.reported_at_ms >= active_cutoff_ms;
                metrics::gauge!("agent_config_ack_active", "agent_id" => apply.agent_id.clone())
                    .set(if active { 1.0 } else { 0.0 });
                metrics::gauge!("agent_config_generation_lag", "agent_id" => apply.agent_id)
                    .set(desired_generation.saturating_sub(apply.applied_generation) as f64);
            }
        }
        Err(error) => warn!(%error, "failed to refresh Agent generation metrics"),
    }
}

async fn invalidate_route_cache(state: &AppState) {
    let bump = metrics::counter!("cache_invalidation_total", "service" => "control-plane-admin", "cache" => "agent_routes");
    bump.increment(1);
    if state.route_cache_enabled {
        state.route_cache.invalidate_all();
    }
    match ConfigSyncStore::current_generation(&state.store).await {
        Ok(generation) => {
            metrics::gauge!("config_desired_generation").set(generation as f64);
            refresh_agent_apply_metrics(&state.store, generation).await;
        }
        Err(error) => warn!(%error, "failed to refresh desired configuration generation metric"),
    }
}

async fn ack_routes(
    headers: HeaderMap,
    State(state): State<AppState>,
    Json(body): Json<AgentConfigAckDto>,
) -> Result<StatusCode, (StatusCode, String)> {
    if !has_valid_agent_sync_token(&headers) {
        return Err((StatusCode::UNAUTHORIZED, "invalid agent sync token".into()));
    }
    let agent_id = body.agent_id.trim();
    if agent_id.is_empty()
        || agent_id.len() > 128
        || !agent_id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b':'))
    {
        return Err((
            StatusCode::BAD_REQUEST,
            "agent_id must be 1..=128 ASCII letters, digits, '-', '_', '.', or ':'".into(),
        ));
    }
    if body.applied_at_ms < 0 || body.applied_generation > i64::MAX as u64 {
        return Err((
            StatusCode::BAD_REQUEST,
            "invalid applied generation or timestamp".into(),
        ));
    }
    if body.snapshot_hash.as_ref().is_some_and(|snapshot_hash| {
        snapshot_hash.len() != 64
            || !snapshot_hash
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    }) {
        return Err((
            StatusCode::BAD_REQUEST,
            "snapshot_hash must be 64 lowercase hexadecimal characters".into(),
        ));
    }
    let current_generation = ConfigSyncStore::current_generation(&state.store)
        .await
        .map_err(|error| (StatusCode::INTERNAL_SERVER_ERROR, error.to_string()))?;
    let applied_generation = body.applied_generation as i64;
    if applied_generation > current_generation {
        return Err((
            StatusCode::CONFLICT,
            format!(
                "applied_generation {applied_generation} is newer than current generation {current_generation}"
            ),
        ));
    }
    let stored = ConfigSyncStore::ack_agent_generation(
        &state.store,
        agent_id,
        applied_generation,
        body.snapshot_hash.as_deref(),
        body.applied_at_ms,
        now_epoch_ms(),
    )
    .await
    .map_err(config_mutation_http_error)?;
    metrics::gauge!("config_desired_generation").set(current_generation as f64);
    metrics::gauge!("agent_applied_generation", "agent_id" => stored.agent_id.clone())
        .set(stored.applied_generation as f64);
    metrics::gauge!("agent_config_ack_active", "agent_id" => stored.agent_id.clone()).set(1.0);
    metrics::gauge!("agent_config_generation_lag", "agent_id" => stored.agent_id)
        .set(current_generation.saturating_sub(stored.applied_generation) as f64);
    Ok(StatusCode::NO_CONTENT)
}

fn config_mutation_http_error(error: StorageError) -> (StatusCode, String) {
    let status = match &error {
        StorageError::Validation(_) => StatusCode::BAD_REQUEST,
        StorageError::Conflict(_) => StatusCode::CONFLICT,
        _ => StatusCode::INTERNAL_SERVER_ERROR,
    };
    (status, error.to_string())
}

async fn post_route(
    headers: HeaderMap,
    State(state): State<AppState>,
    Json(body): Json<TunnelRouteRecordDto>,
) -> Result<StatusCode, (StatusCode, String)> {
    require_admin_or_boss(&headers, &state.store).await?;
    let rec = TunnelRouteRecord {
        host: body.host.clone(),
        app_id: body.app_id.clone(),
        connector_endpoint: body.connector_endpoint.clone(),
        require_healthy_tunnel: body.require_healthy_tunnel,
    };
    let audit = AuditLogRecord::management(
        "control-plane-admin",
        management_actor(&headers),
        rec.app_id.clone(),
        format!("/api/v1/agent/routes/{}", rec.host),
        "POST",
    );
    AuditLogsStore::apply_security_mutation(
        &state.store,
        &SecurityMutation::UpsertTunnelRoute(rec),
        &audit,
    )
    .await
    .map_err(config_mutation_http_error)?;
    invalidate_route_cache(&state).await;
    Ok(StatusCode::CREATED)
}

async fn put_route(
    headers: HeaderMap,
    State(state): State<AppState>,
    Path(host): Path<String>,
    Json(mut body): Json<TunnelRouteRecordDto>,
) -> Result<StatusCode, (StatusCode, String)> {
    require_admin_or_boss(&headers, &state.store).await?;
    body.host = host;
    let rec = TunnelRouteRecord {
        host: body.host.clone(),
        app_id: body.app_id.clone(),
        connector_endpoint: body.connector_endpoint.clone(),
        require_healthy_tunnel: body.require_healthy_tunnel,
    };
    let audit = AuditLogRecord::management(
        "control-plane-admin",
        management_actor(&headers),
        rec.app_id.clone(),
        format!("/api/v1/agent/routes/{}", rec.host),
        "PUT",
    );
    AuditLogsStore::apply_security_mutation(
        &state.store,
        &SecurityMutation::UpsertTunnelRoute(rec),
        &audit,
    )
    .await
    .map_err(config_mutation_http_error)?;
    invalidate_route_cache(&state).await;
    Ok(StatusCode::OK)
}

async fn delete_route(
    headers: HeaderMap,
    State(state): State<AppState>,
    Path(host): Path<String>,
) -> Result<StatusCode, (StatusCode, String)> {
    require_admin_or_boss(&headers, &state.store).await?;
    let audit = AuditLogRecord::management(
        "control-plane-admin",
        management_actor(&headers),
        "",
        format!("/api/v1/agent/routes/{host}"),
        "DELETE",
    );
    AuditLogsStore::apply_security_mutation(
        &state.store,
        &SecurityMutation::DeleteTunnelRoute(host),
        &audit,
    )
    .await
    .map_err(config_mutation_http_error)?;
    invalidate_route_cache(&state).await;
    Ok(StatusCode::NO_CONTENT)
}

async fn put_intranet_upstream(
    headers: HeaderMap,
    State(state): State<AppState>,
    Query(q): Query<IntranetQuery>,
    Json(body): Json<IntranetBody>,
) -> Result<StatusCode, (StatusCode, String)> {
    require_admin_or_boss(&headers, &state.store).await?;
    let app_id = q.app_id.clone();
    let rec = IntranetUpstreamRecord {
        app_id: q.app_id,
        upstream: body.upstream,
        scheme: body.scheme.to_ascii_lowercase(),
    };
    let audit = AuditLogRecord::management(
        "control-plane-admin",
        management_actor(&headers),
        app_id.clone(),
        format!("/api/v1/intranet/upstream/{app_id}"),
        "PUT",
    );
    AuditLogsStore::apply_security_mutation(
        &state.store,
        &SecurityMutation::UpsertIntranetUpstream(rec),
        &audit,
    )
    .await
    .map_err(config_mutation_http_error)?;
    invalidate_route_cache(&state).await;
    Ok(StatusCode::OK)
}

async fn get_apps_tree(
    headers: HeaderMap,
    State(state): State<AppState>,
    Query(q): Query<AppsTreeQuery>,
) -> Result<Json<Vec<AppTreeNodeDto>>, (StatusCode, String)> {
    let t0 = Instant::now();
    require_admin_or_boss(&headers, &state.store).await?;
    let t_routes = Instant::now();
    let routes = RoutesStore::load_all(&state.store)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    let routes_ms = t_routes.elapsed().as_millis();
    let mut by_app: HashMap<String, Vec<TunnelRouteRecordDto>> = HashMap::new();
    for r in routes {
        by_app
            .entry(r.app_id.clone())
            .or_default()
            .push(TunnelRouteRecordDto {
                host: r.host,
                app_id: r.app_id,
                connector_endpoint: r.connector_endpoint,
                require_healthy_tunnel: r.require_healthy_tunnel,
            });
    }
    let mut app_ids: Vec<String> = by_app.keys().cloned().collect();
    app_ids.sort();
    let with_latest = q.with_latest.unwrap_or(true);
    let mut latest_ms = 0;
    let latest_map: HashMap<String, AppMetricMinuteRecord> = if with_latest {
        let t_latest = Instant::now();
        let latest = AppMetricsStore::latest_by_app_ids(&state.store, &app_ids)
            .await
            .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
        latest_ms = t_latest.elapsed().as_millis();
        latest.into_iter().map(|r| (r.app_id.clone(), r)).collect()
    } else {
        HashMap::new()
    };
    let mut out = Vec::new();
    for app_id in app_ids {
        out.push(AppTreeNodeDto {
            latest: latest_map.get(&app_id).map(map_metric_point),
            routes: by_app.remove(&app_id).unwrap_or_default(),
            app_id,
        });
    }
    debug_log(
        "control-plane-admin/main.rs:get_apps_tree",
        "apps tree generated",
        "H6",
        serde_json::json!({"apps":out.len(),"ms":t0.elapsed().as_millis(),"routes_ms":routes_ms,"latest_ms":latest_ms,"with_latest":with_latest}),
        "initial",
    );
    Ok(Json(out))
}

async fn get_apps_metrics(
    headers: HeaderMap,
    State(state): State<AppState>,
    Query(q): Query<AppMetricsQuery>,
) -> Result<Json<AppMetricsResponseDto>, (StatusCode, String)> {
    let t0 = Instant::now();
    require_admin_or_boss(&headers, &state.store).await?;
    let routes = RoutesStore::load_all(&state.store)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    let mut app_ids: Vec<String> = routes.into_iter().map(|r| r.app_id).collect();
    app_ids.sort();
    app_ids.dedup();
    if let Some(app_id) = q.app_id.clone() {
        app_ids.retain(|x| x == &app_id);
    }
    let now_min = minute_floor(now_epoch_sec());
    let range_min = q.range_min.unwrap_or(60) as i64;
    let from = now_min - range_min * 60;
    let latest = AppMetricsStore::latest_by_app_ids(&state.store, &app_ids)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    let latest_map: HashMap<String, AppMetricMinuteRecord> =
        latest.into_iter().map(|r| (r.app_id.clone(), r)).collect();
    let mut series = Vec::new();
    let all_rows = AppMetricsStore::list_by_apps_range(&state.store, &app_ids, from, now_min)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    let mut rows_map: HashMap<String, Vec<AppMetricMinuteRecord>> = HashMap::new();
    for r in all_rows {
        rows_map.entry(r.app_id.clone()).or_default().push(r);
    }
    for app_id in app_ids {
        let rows = rows_map.remove(&app_id).unwrap_or_default();
        series.push(AppMetricsSeriesDto {
            latest: latest_map.get(&app_id).map(map_metric_point),
            points: rows.iter().map(map_metric_point).collect(),
            app_id,
        });
    }
    debug_log(
        "control-plane-admin/main.rs:get_apps_metrics",
        "apps metrics generated",
        "H2",
        serde_json::json!({"series":series.len(),"range_min":range_min,"ms":t0.elapsed().as_millis()}),
        "initial",
    );
    Ok(Json(AppMetricsResponseDto {
        generated_at_minute: now_min,
        series,
        note: "UV/独立IP当前为近似值（基于请求量估算）；后续可接入明细日志聚合做精确去重"
            .to_string(),
    }))
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
    // Online mutations enforce these invariants, but an in-place upgrade can
    // inherit rows written by an older version. Fail closed before Agent reads
    // or APISIX workers start rather than declaring poisoned legacy state
    // converged.
    validate_persisted_route_configuration(&store).await?;
    let audit_writer = AuditWriter::from_env(store.clone())?;
    let store_hint = match backend {
        StorageBackend::Sqlite => format!("sqlite:{}", shared_storage::resolve_storage_db_path()),
        StorageBackend::Postgres => {
            format!("postgres:{}", redact_postgres_dsn(&resolve_postgres_dsn()))
        }
    };

    let bootstrap_demo = std::env::var("SAG_BOOTSTRAP_DEMO_TUNNEL_ROUTE")
        .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
        .unwrap_or(false);
    let apisix_cfg = apisix::config_from_env();
    if apisix_cfg.is_some() {
        info!("APISIX Admin push enabled (SAG_APISIX_ADMIN_BASE_URL)");
    }
    let http_timeout_ms = std::env::var("SAG_CONTROL_PLANE_HTTP_TIMEOUT_MS")
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .unwrap_or(8_000)
        .clamp(100, 60_000);
    let http = reqwest::Client::builder()
        .timeout(Duration::from_millis(http_timeout_ms))
        .build()?;

    if bootstrap_demo {
        match RoutesStore::load_all(&store).await {
            Ok(routes) if routes.is_empty() => {
                let route = TunnelRouteRecord {
                    host: "app.internal.com".into(),
                    app_id: "app-001".into(),
                    connector_endpoint: "connector-local-001:stream".into(),
                    require_healthy_tunnel: true,
                };
                let audit = AuditLogRecord::management(
                    "control-plane-admin",
                    "system:bootstrap",
                    "app-001",
                    "/bootstrap/demo-tunnel-route",
                    "UPSERT",
                );
                if let Err(error) = AuditLogsStore::apply_security_mutation(
                    &store,
                    &SecurityMutation::UpsertTunnelRoute(route),
                    &audit,
                )
                .await
                {
                    warn!(%error, "SAG_BOOTSTRAP_DEMO_TUNNEL_ROUTE: demo insert failed");
                } else {
                    info!("SAG_BOOTSTRAP_DEMO_TUNNEL_ROUTE: inserted demo tunnel_routes row (app-001)");
                }
            }
            Ok(_) => {
                info!("SAG_BOOTSTRAP_DEMO_TUNNEL_ROUTE: tunnel_routes non-empty, skip demo insert")
            }
            Err(error) => {
                warn!(%error, "SAG_BOOTSTRAP_DEMO_TUNNEL_ROUTE: route lookup failed");
            }
        }

        // Also seed an intranet upstream for `app-001` so that APISIX can be pushed and
        // smoke tests can validate APISIX data-plane routing (/api/* -> mock-workload).
        match RoutesStore::get_intranet_upstream(&store, "app-001").await {
            Ok(Some(_)) => {
                info!("SAG_BOOTSTRAP_DEMO_TUNNEL_ROUTE: intranet_upstreams already has app-001, skip demo insert");
            }
            Ok(None) => {
                let rec = IntranetUpstreamRecord {
                    app_id: "app-001".to_string(),
                    upstream: "mock-workload:18080".to_string(),
                    scheme: "http".to_string(),
                };
                let audit = AuditLogRecord::management(
                    "control-plane-admin",
                    "system:bootstrap",
                    "app-001",
                    "/bootstrap/demo-intranet-upstream",
                    "UPSERT",
                );
                if let Err(error) = AuditLogsStore::apply_security_mutation(
                    &store,
                    &SecurityMutation::UpsertIntranetUpstream(rec),
                    &audit,
                )
                .await
                {
                    warn!(%error, "SAG_BOOTSTRAP_DEMO_TUNNEL_ROUTE: intranet upstream insert failed");
                } else {
                    info!("SAG_BOOTSTRAP_DEMO_TUNNEL_ROUTE: inserted demo intranet_upstreams row (app-001 -> mock-workload:18080)");
                }
            }
            Err(e) => {
                tracing::warn!(%e, "SAG_BOOTSTRAP_DEMO_TUNNEL_ROUTE: intranet upstream lookup failed");
            }
        }
    }
    let state = AppState {
        store,
        audit_writer,
        http,
        apisix: apisix_cfg,
        metrics: metrics_exporter_prometheus::PrometheusBuilder::new()
            .install_recorder()
            .map_err(|e| anyhow::anyhow!("install prometheus recorder failed: {e}"))?,
        fault_toggle: Arc::new(RwLock::new(FaultInjectionToggle::default())),
        route_cache: Arc::new(
            Cache::builder()
                .time_to_live(std::time::Duration::from_secs(
                    std::env::var("SAG_ROUTE_CACHE_TTL_SEC")
                        .ok()
                        .and_then(|v| v.parse::<u64>().ok())
                        .unwrap_or(10),
                ))
                .build(),
        ),
        route_cache_enabled: std::env::var("SAG_ROUTE_CACHE_ENABLED")
            .map(|v| matches!(v.to_lowercase().as_str(), "1" | "true" | "yes" | "on"))
            .unwrap_or(true),
        readiness: Readiness::new(
            std::env::var("SAG_READINESS_SUCCESS_THRESHOLD")
                .ok()
                .and_then(|value| value.parse().ok())
                .unwrap_or(2),
        ),
    };

    match ConfigSyncStore::current_generation(&state.store).await {
        Ok(generation) => {
            metrics::gauge!("config_desired_generation").set(generation as f64);
            refresh_agent_apply_metrics(&state.store, generation).await;
        }
        Err(error) => warn!(%error, "failed to initialize desired configuration generation metric"),
    }

    if let Some(config) = state.apisix.clone() {
        tokio::spawn(run_config_sync_worker(
            state.store.clone(),
            state.http.clone(),
            config,
        ));
        info!("persistent APISIX config sync worker started");
    } else {
        warn!("persistent APISIX config sync worker disabled: APISIX admin config missing");
    }

    let bg_state = state.clone();
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(std::time::Duration::from_secs(60));
        loop {
            interval.tick().await;
            if let Err(e) = aggregate_and_persist_app_metrics(&bg_state).await {
                tracing::warn!(%e, "app metrics aggregation tick failed");
            }
            match ConfigSyncStore::current_generation(&bg_state.store).await {
                Ok(generation) => {
                    refresh_agent_apply_metrics(&bg_state.store, generation).await;
                    let cutoff_ms = now_epoch_ms().saturating_sub(agent_apply_retention_ms());
                    if let Err(error) =
                        ConfigSyncStore::prune_agent_applies_before(&bg_state.store, cutoff_ms)
                            .await
                    {
                        warn!(%error, "failed to prune expired Agent apply acknowledgements");
                    }
                }
                Err(error) => warn!(%error, "failed to refresh Agent apply activity"),
            }
        }
    });

    // Periodic APISIX reconciliation keeps old/overwritten route shape corrected.
    let reconcile_enabled = parse_bool_env("SAG_APISIX_RECONCILE_ENABLED", true);
    let reconcile_interval_sec = std::env::var("SAG_APISIX_RECONCILE_INTERVAL_SEC")
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
        .unwrap_or(30)
        .max(5);
    let reconcile_timeout_sec = std::env::var("SAG_APISIX_RECONCILE_TIMEOUT_SEC")
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .unwrap_or(20)
        .clamp(5, 300);
    if reconcile_enabled && state.apisix.is_some() {
        metrics::gauge!("apisix_reconcile_last_success_timestamp_seconds")
            .set(now_epoch_ms() as f64 / 1_000.0);
        let reconcile_state = state.clone();
        tokio::spawn(async move {
            let mut interval =
                tokio::time::interval(std::time::Duration::from_secs(reconcile_interval_sec));
            loop {
                interval.tick().await;
                if tokio::time::timeout(
                    Duration::from_secs(reconcile_timeout_sec),
                    reconcile_apisix_managed_routes(&reconcile_state),
                )
                .await
                .is_err()
                {
                    metrics::counter!("apisix_reconcile_errors_total", "stage" => "round_timeout")
                        .increment(1);
                    warn!(
                        timeout_sec = reconcile_timeout_sec,
                        "APISIX reconciliation round timed out"
                    );
                }
            }
        });
        info!(
            interval_sec = reconcile_interval_sec,
            timeout_sec = reconcile_timeout_sec,
            "apisix periodic reconcile started"
        );
    } else if state.apisix.is_none() {
        warn!("apisix periodic reconcile disabled: APISIX admin config missing");
    } else {
        warn!("apisix periodic reconcile disabled by env");
    }

    let app = Router::new()
        .route("/live", get(live))
        .route("/ready", get(ready))
        .route("/health", get(ready))
        .route("/metrics", get(metrics))
        .route("/api/v1/apps", get(list_apps).post(upsert_app))
        .route("/api/v1/apps/:app_id", delete(delete_app))
        .route(
            "/api/v1/api-routes",
            get(list_api_routes).post(upsert_api_route),
        )
        .route("/api/v1/api-routes/:id", delete(delete_api_route))
        .route(
            "/api/v1/audit/logs",
            get(list_audit_logs).post(post_audit_log),
        )
        .route(
            "/api/v1/fault-events",
            get(list_fault_events).post(post_fault_event),
        )
        .route("/api/public/security/audit", get(public_audit_logs))
        .route(
            "/api/public/security/fault-events",
            get(public_fault_events),
        )
        .route(
            "/api/public/security/overview",
            get(public_security_overview),
        )
        .route(
            "/api/v1/fault-injection",
            get(get_fault_injection_toggle).put(put_fault_injection_toggle),
        )
        .route(
            "/api/v1/idempotency/indeterminate",
            get(list_indeterminate_idempotency),
        )
        .route(
            "/api/v1/idempotency/:scope_key",
            get(get_idempotency_evidence),
        )
        .route(
            "/api/v1/idempotency/:scope_key/complete",
            post(complete_idempotency_by_operator),
        )
        .route(
            "/api/v1/idempotency/:scope_key/release",
            post(release_idempotency_by_operator),
        )
        .route("/api/v1/agent/routes", get(list_routes).post(post_route))
        .route("/api/v1/agent/routes/ack", post(ack_routes))
        .route(
            "/api/v1/agent/routes/:host",
            put(put_route).delete(delete_route),
        )
        .route(
            "/api/v1/agent/intranet-upstreams",
            put(put_intranet_upstream),
        )
        .route("/api/v1/apps/tree", get(get_apps_tree))
        .route("/api/v1/apps/metrics", get(get_apps_metrics))
        .layer(middleware::from_fn_with_state(state.clone(), metrics_mw))
        .layer(middleware::from_fn_with_state(state.clone(), admission_mw))
        .layer(TraceLayer::new_for_http())
        .with_state(state.clone());

    let addr: SocketAddr = "0.0.0.0:8090".parse()?;
    info!(%addr, backend=?backend, store=%store_hint, "control-plane-admin listening");
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
    use std::sync::atomic::{AtomicUsize, Ordering};

    use axum::extract::State as AxumState;

    #[test]
    fn route_snapshot_response_carries_generation_without_changing_json_shape() {
        let (headers, Json(routes)) = route_snapshot_response(RouteCacheEntry {
            generation: 42,
            routes: vec![TunnelRouteRecordDto {
                host: "app.internal".into(),
                app_id: "app-001".into(),
                connector_endpoint: "connector:stream".into(),
                require_healthy_tunnel: true,
            }],
        })
        .unwrap();

        assert_eq!(headers["x-sag-config-generation"], "42");
        assert_eq!(routes.len(), 1);
        assert_eq!(routes[0].app_id, "app-001");
    }

    #[test]
    fn config_sync_retry_is_bounded_exponential_with_small_jitter() {
        let first = config_sync_retry_delay_ms(1, "job-a");
        let seventh = config_sync_retry_delay_ms(7, "job-a");
        let much_later = config_sync_retry_delay_ms(100, "job-a");

        assert!((1_000..=1_250).contains(&first));
        assert!((60_000..=60_250).contains(&seventh));
        assert_eq!(seventh, much_later);
    }

    #[tokio::test]
    async fn stale_current_id_tombstone_converges_recreated_route_instead_of_deleting_it() {
        #[derive(Clone)]
        struct MockState {
            put_count: Arc<AtomicUsize>,
            delete_count: Arc<AtomicUsize>,
        }

        async fn get_route() -> StatusCode {
            StatusCode::NOT_FOUND
        }
        async fn put_route(AxumState(state): AxumState<MockState>) -> StatusCode {
            state.put_count.fetch_add(1, Ordering::SeqCst);
            StatusCode::OK
        }
        async fn delete_route(AxumState(state): AxumState<MockState>) -> StatusCode {
            state.delete_count.fetch_add(1, Ordering::SeqCst);
            StatusCode::OK
        }

        let put_count = Arc::new(AtomicUsize::new(0));
        let delete_count = Arc::new(AtomicUsize::new(0));
        let app = Router::new()
            .route(
                "/apisix/admin/routes/:route_id",
                get(get_route).put(put_route).delete(delete_route),
            )
            .with_state(MockState {
                put_count: put_count.clone(),
                delete_count: delete_count.clone(),
            });
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });

        let db_path = std::env::temp_dir().join(format!(
            "sag-control-plane-stale-tombstone-{}-{}.db",
            std::process::id(),
            uuid::Uuid::new_v4()
        ));
        let store = StorageStore::Sqlite(shared_storage::SqliteStore::new(
            db_path.to_string_lossy().to_string(),
        ));
        ensure_store_schema(&store).await.unwrap();
        RoutesStore::upsert(
            &store,
            &TunnelRouteRecord {
                host: "recreated.internal".into(),
                app_id: "app-1".into(),
                connector_endpoint: "connector:50051".into(),
                require_healthy_tunnel: true,
            },
        )
        .await
        .unwrap();
        RoutesStore::upsert_intranet_upstream(
            &store,
            &IntranetUpstreamRecord {
                app_id: "app-1".into(),
                upstream: "upstream:8080".into(),
                scheme: "http".into(),
            },
        )
        .await
        .unwrap();
        let job = ConfigSyncJob {
            job_id: "stale-delete".into(),
            generation: 1,
            target: "APISIX".into(),
            resource_type: "ROUTE_ID".into(),
            resource_id: apisix::route_id_for_app("app-1"),
            app_id: "app-1".into(),
            operation: ConfigSyncOperation::Delete,
            payload_json: None,
            status: shared_storage::ConfigSyncStatus::Pending,
            attempt_count: 1,
            next_attempt_at_ms: 0,
            last_error: None,
            lease_owner: Some("worker".into()),
            lease_expires_at_ms: Some(i64::MAX),
            superseded_by_generation: None,
            created_at_ms: 0,
            updated_at_ms: 0,
            applied_at_ms: None,
        };
        let cfg = apisix::ApisixPushConfig {
            admin_base: format!("http://{address}"),
            admin_key: "test-key".into(),
        };

        let client = reqwest::Client::builder().no_proxy().build().unwrap();
        apply_config_sync_job(&client, &cfg, &store, &job)
            .await
            .unwrap();
        assert_eq!(put_count.load(Ordering::SeqCst), 1);
        assert_eq!(delete_count.load(Ordering::SeqCst), 0);

        server.abort();
        std::fs::remove_file(db_path).unwrap();
    }

    #[tokio::test]
    async fn timed_out_apisix_write_quarantines_newer_generation_until_old_lease_expires() {
        #[derive(Clone)]
        struct DelayedMockState {
            started: Arc<AtomicUsize>,
            completed: Arc<AtomicUsize>,
            entered: Arc<tokio::sync::Notify>,
        }

        async fn slow_put_route(AxumState(state): AxumState<DelayedMockState>) -> StatusCode {
            state.started.fetch_add(1, Ordering::SeqCst);
            state.entered.notify_one();
            let completed = state.completed.clone();
            tokio::spawn(async move {
                tokio::time::sleep(Duration::from_millis(80)).await;
                completed.fetch_add(1, Ordering::SeqCst);
            });
            // Model an upstream which accepted the mutation, continues it
            // independently, but does not return a response before our client
            // deadline.
            tokio::time::sleep(Duration::from_millis(200)).await;
            StatusCode::OK
        }

        async fn route_not_found() -> StatusCode {
            StatusCode::NOT_FOUND
        }

        let started = Arc::new(AtomicUsize::new(0));
        let completed = Arc::new(AtomicUsize::new(0));
        let entered = Arc::new(tokio::sync::Notify::new());
        let app = Router::new()
            .route(
                "/apisix/admin/routes/:route_id",
                get(route_not_found).put(slow_put_route),
            )
            .with_state(DelayedMockState {
                started: started.clone(),
                completed: completed.clone(),
                entered: entered.clone(),
            });
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });

        let db_path = std::env::temp_dir().join(format!(
            "sag-control-plane-unknown-outcome-{}-{}.db",
            std::process::id(),
            uuid::Uuid::new_v4()
        ));
        let store = StorageStore::Sqlite(shared_storage::SqliteStore::new(
            db_path.to_string_lossy().to_string(),
        ));
        ensure_store_schema(&store).await.unwrap();
        RoutesStore::upsert(
            &store,
            &TunnelRouteRecord {
                host: "late-write.internal".into(),
                app_id: "app-late".into(),
                connector_endpoint: "connector:50051".into(),
                require_healthy_tunnel: true,
            },
        )
        .await
        .unwrap();
        RoutesStore::upsert_intranet_upstream(
            &store,
            &IntranetUpstreamRecord {
                app_id: "app-late".into(),
                upstream: "upstream:8080".into(),
                scheme: "http".into(),
            },
        )
        .await
        .unwrap();

        let resource_id = apisix::route_id_for_app("app-late");
        let first = ConfigSyncStore::enqueue_job(
            &store,
            &ConfigSyncJobDraft {
                generation: 1,
                target: "APISIX".into(),
                resource_type: "ROUTE".into(),
                resource_id: resource_id.clone(),
                app_id: "app-late".into(),
                operation: ConfigSyncOperation::Upsert,
                payload_json: None,
                next_attempt_at_ms: 10,
            },
            10,
        )
        .await
        .unwrap();
        let claimed = ConfigSyncStore::claim_due_jobs(&store, "worker-old", 10, 200, 1)
            .await
            .unwrap();
        assert_eq!(claimed.len(), 1);
        assert_eq!(claimed[0].job_id, first.job_id);

        let cfg = apisix::ApisixPushConfig {
            admin_base: format!("http://{address}"),
            admin_key: "test-key".into(),
        };
        let client = reqwest::Client::builder().no_proxy().build().unwrap();
        let mut apply = Box::pin(apply_config_sync_job(&client, &cfg, &store, &claimed[0]));
        let entered_before_completion = tokio::time::timeout(Duration::from_secs(2), async {
            tokio::select! {
                outcome = &mut apply => Some(outcome),
                () = entered.notified() => None,
            }
        })
        .await
        .expect("the APISIX mock must receive the PUT request");
        assert!(
            entered_before_completion.is_none(),
            "the APISIX call unexpectedly completed before entering the delayed PUT"
        );
        let result = tokio::time::timeout(Duration::from_millis(20), &mut apply).await;
        assert!(result.is_err(), "the delayed APISIX write must time out");
        drop(apply);
        assert_eq!(started.load(Ordering::SeqCst), 1);
        assert!(ConfigSyncStore::mark_failed_retaining_lease(
            &store,
            &first.job_id,
            "worker-old",
            "APISIX operation timed out",
            30,
            40,
        )
        .await
        .unwrap());

        ConfigSyncStore::enqueue_job(
            &store,
            &ConfigSyncJobDraft {
                generation: 2,
                target: "APISIX".into(),
                resource_type: "ROUTE".into(),
                resource_id,
                app_id: "app-late".into(),
                operation: ConfigSyncOperation::Upsert,
                payload_json: None,
                next_attempt_at_ms: 31,
            },
            31,
        )
        .await
        .unwrap();

        let prematurely_claimed =
            ConfigSyncStore::claim_due_jobs(&store, "worker-new", 100, 100, 1)
                .await
                .unwrap();
        assert!(
            prematurely_claimed.is_empty(),
            "the old unknown-outcome lease must fence the newer generation"
        );

        tokio::time::sleep(Duration::from_millis(100)).await;
        assert_eq!(
            completed.load(Ordering::SeqCst),
            1,
            "the server-side write completed after the client timed out"
        );
        let after_quarantine = ConfigSyncStore::claim_due_jobs(&store, "worker-new", 211, 100, 1)
            .await
            .unwrap();
        assert_eq!(after_quarantine.len(), 1);
        assert_eq!(after_quarantine[0].generation, 2);

        server.abort();
        std::fs::remove_file(db_path).unwrap();
    }

    #[tokio::test]
    async fn startup_preflight_rejects_poisoned_legacy_route_configuration() {
        let db_path = std::env::temp_dir().join(format!(
            "sag-control-plane-legacy-route-preflight-{}-{}.db",
            std::process::id(),
            uuid::Uuid::new_v4()
        ));
        let store = StorageStore::Sqlite(shared_storage::SqliteStore::new(
            db_path.to_string_lossy().to_string(),
        ));
        ensure_store_schema(&store).await.unwrap();
        RoutesStore::upsert(
            &store,
            &TunnelRouteRecord {
                host: "legacy.internal".into(),
                app_id: "app-legacy".into(),
                connector_endpoint: "connector-legacy:stream".into(),
                require_healthy_tunnel: true,
            },
        )
        .await
        .unwrap();
        // This low-level insert models a row persisted by an older binary,
        // before online mutation validation existed.
        RoutesStore::upsert_intranet_upstream(
            &store,
            &IntranetUpstreamRecord {
                app_id: "app-legacy".into(),
                upstream: "bad!host:8080".into(),
                scheme: "http".into(),
            },
        )
        .await
        .unwrap();

        let error = validate_persisted_route_configuration(&store)
            .await
            .unwrap_err();
        assert!(error
            .to_string()
            .contains("persisted route configuration is unsafe"));

        std::fs::remove_file(db_path).unwrap();
    }
}
