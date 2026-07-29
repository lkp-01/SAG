mod foura;
mod user_directory;

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::{Duration, Instant};

use argon2::password_hash::{PasswordHash, PasswordHasher, PasswordVerifier, SaltString};
use argon2::Argon2;
use axum::body::Body;
use axum::extract::{Query, State};
use axum::http::Request;
use axum::http::{HeaderMap, StatusCode};
use axum::middleware::{self, Next};
use axum::response::IntoResponse;
use axum::response::Redirect;
use axum::routing::{get, post};
use axum::{Json, Router};
use jsonwebtoken::{decode, encode, DecodingKey, EncodingKey, Header, Validation};
use moka::future::Cache;
use redis::AsyncCommands;
use sag_service_health::Readiness;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use shared_storage::{
    build_store_from_env, ensure_store_schema, AuditLogRecord, AuditLogsStore, AuditWriter,
    FaultEventRecord, FaultEventsStore, GroupRoleMappingRecord, IdentityProviderRecord,
    IdentityStore, SecurityMutation, StorageStore, UserRecord, UsersStore,
};
use tokio::sync::{Mutex, RwLock};
use tower_http::trace::TraceLayer;
use tracing::{info, warn};

use crate::foura::{FourAConfig, OidcConfig};
use crate::user_directory::UserDirectory;

#[derive(Clone)]
struct AppState {
    jwt_secret: Arc<String>,
    users: Arc<RwLock<HashMap<String, User>>>,
    store: StorageStore,
    audit_writer: AuditWriter,
    http: reqwest::Client,
    foura: Option<FourAConfig>,
    oidc: Option<OidcConfig>,
    oauth_states: OAuthStateStore,
    metrics: metrics_exporter_prometheus::PrometheusHandle,
    login_memo_cache: Option<LoginMemoCache>,
    identity_read_cache: IdentityReadCache,
    users_read_cache: Cache<String, Vec<UserDto>>,
    readiness: Readiness,
    user_directory: UserDirectory,
}

fn management_actor(headers: &HeaderMap) -> String {
    headers
        .get("x-sag-user-id")
        .and_then(|value| value.to_str().ok())
        .filter(|value| !value.trim().is_empty())
        .unwrap_or("authenticated-admin")
        .to_string()
}

#[derive(Clone)]
enum LoginMemoBackend {
    InMemory(Cache<String, String>),
    Redis {
        conn: Box<redis::aio::ConnectionManager>,
        key_prefix: String,
    },
}

#[derive(Clone)]
struct LoginMemoCache {
    ttl_sec: u64,
    backend: LoginMemoBackend,
}

#[derive(Debug, Serialize, Deserialize)]
struct LoginMemoEntry {
    token: String,
    user: UserDto,
    expires_in_sec: u64,
    cached_at_epoch_sec: u64,
}

#[derive(Clone, Serialize, Deserialize)]
struct OAuthState {
    expires_at: u64,
    provider_id: String,
}

#[derive(Clone)]
enum OAuthStateBackend {
    InMemory(Arc<RwLock<HashMap<String, OAuthState>>>),
    Redis {
        conn: Arc<Mutex<redis::aio::ConnectionManager>>,
        key_prefix: String,
    },
}

#[derive(Clone)]
struct OAuthStateStore {
    backend: OAuthStateBackend,
}

impl OAuthStateStore {
    async fn from_env() -> Self {
        if let Ok(url) = std::env::var("SAG_SESSION_REDIS_URL") {
            if !url.trim().is_empty() {
                if let Ok(client) = redis::Client::open(url.clone()) {
                    if let Ok(conn) = redis::aio::ConnectionManager::new(client).await {
                        info!("oauth state backend: redis (connection-manager)");
                        return Self {
                            backend: OAuthStateBackend::Redis {
                                conn: Arc::new(Mutex::new(conn)),
                                key_prefix: "sag:auth:oauth_state:".to_string(),
                            },
                        };
                    }
                }
                warn!("oauth state backend: redis requested but connection failed; falling back to in-memory");
            }
        }
        info!("oauth state backend: in-memory");
        Self {
            backend: OAuthStateBackend::InMemory(Arc::new(RwLock::new(HashMap::new()))),
        }
    }

    async fn put(&self, state: &str, value: OAuthState) {
        match &self.backend {
            OAuthStateBackend::InMemory(map) => {
                let mut m = map.write().await;
                prune_oauth_states(&mut m);
                m.insert(state.to_string(), value);
            }
            OAuthStateBackend::Redis { conn, key_prefix } => {
                let k = format!("{}{}", key_prefix, state);
                let ttl = value.expires_at.saturating_sub(now_epoch_sec()).max(1);
                let payload = serde_json::to_string(&value).unwrap_or_default();
                let mut c = conn.lock().await;
                let _: Result<(), _> = c.set_ex(k, payload, ttl).await;
            }
        }
    }

    async fn take(&self, state: &str) -> Option<OAuthState> {
        match &self.backend {
            OAuthStateBackend::InMemory(map) => {
                let mut m = map.write().await;
                prune_oauth_states(&mut m);
                m.remove(state)
            }
            OAuthStateBackend::Redis { conn, key_prefix } => {
                let k = format!("{}{}", key_prefix, state);
                let mut c = conn.lock().await;
                let raw: Option<String> = c.get(&k).await.ok().flatten();
                if raw.is_some() {
                    let _: Result<(), _> = c.del(&k).await;
                }
                raw.and_then(|s| serde_json::from_str::<OAuthState>(&s).ok())
            }
        }
    }
}

#[derive(Clone)]
struct User {
    id: String,
    username: String,
    roles: Vec<String>,
    display_name: Option<String>,
    title: Option<String>,
    enabled: bool,
    auth_version: i64,
    updated_at_ms: i64,
}

#[derive(Debug, Serialize, Deserialize)]
struct Claims {
    sub: String,
    username: String,
    roles: Vec<String>,
    #[serde(default)]
    external_groups: Vec<String>,
    exp: usize,
    iat: usize,
    #[serde(default)]
    iss: String,
    #[serde(default)]
    auth_version: i64,
}

#[derive(Deserialize)]
struct LoginRequest {
    username: String,
    password: String,
}

#[derive(Serialize)]
struct LoginResponse {
    token: String,
    user: UserDto,
    expires_in_sec: u64,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
struct UserDto {
    id: String,
    username: String,
    roles: Vec<String>,
    #[serde(default)]
    external_groups: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    roles_display: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    display_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    title: Option<String>,
}

#[derive(Deserialize)]
struct VerifyRequest {
    token: String,
}

#[derive(Deserialize)]
struct SsoCallbackQuery {
    code: String,
    state: String,
}

#[derive(Deserialize)]
struct SsoLoginQuery {
    provider_id: Option<String>,
}

#[derive(Serialize)]
struct VerifyResponse {
    active: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    user: Option<UserDto>,
}

#[derive(Deserialize)]
struct UpsertUserRequest {
    id: Option<String>,
    username: String,
    password: Option<String>,
    roles: Vec<String>,
    display_name: Option<String>,
    title: Option<String>,
    enabled: Option<bool>,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
struct IdentityProviderDto {
    id: String,
    kind: String,
    issuer: String,
    client_id: String,
    client_secret: String,
    scopes: String,
    enabled: bool,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
struct GroupRoleMappingDto {
    id: String,
    provider_id: String,
    external_group: String,
    local_roles_csv: String,
    enabled: bool,
    priority: i64,
}

#[derive(Debug, Deserialize)]
struct MappingsQuery {
    provider_id: Option<String>,
}

#[derive(Clone)]
struct IdentityReadCache {
    providers: Cache<String, Vec<IdentityProviderDto>>,
    mappings: Cache<String, Vec<GroupRoleMappingDto>>,
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

fn fault_injection_env_enabled() -> bool {
    matches!(
        std::env::var("SAG_ENABLE_FAULT_INJECTION")
            .unwrap_or_default()
            .to_lowercase()
            .as_str(),
        "1" | "true" | "yes" | "on"
    )
}

fn jwt_expires_in_sec() -> u64 {
    std::env::var("SAG_JWT_EXPIRES_SEC")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(3600)
}

fn login_memo_enabled() -> bool {
    !matches!(
        std::env::var("SAG_LOGIN_MEMO_ENABLED")
            .unwrap_or_else(|_| "true".to_string())
            .to_lowercase()
            .as_str(),
        "0" | "false" | "off" | "no"
    )
}

fn login_memo_ttl_sec() -> u64 {
    std::env::var("SAG_LOGIN_MEMO_TTL_SEC")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(60)
}

fn login_memo_max_capacity() -> u64 {
    std::env::var("SAG_LOGIN_MEMO_MAX_CAPACITY")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(50_000)
}

fn identity_read_cache_ttl_sec() -> u64 {
    std::env::var("SAG_IDENTITY_READ_CACHE_TTL_SEC")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(15)
}

fn identity_read_cache_max_capacity() -> u64 {
    std::env::var("SAG_IDENTITY_READ_CACHE_MAX_CAPACITY")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(128)
}

fn users_read_cache_ttl_sec() -> u64 {
    std::env::var("SAG_USERS_READ_CACHE_TTL_SEC")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(10)
}

fn users_read_cache_max_capacity() -> u64 {
    std::env::var("SAG_USERS_READ_CACHE_MAX_CAPACITY")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(16)
}

fn login_memo_key(
    jwt_secret: &str,
    username: &str,
    password: &str,
    password_hash: &str,
    auth_version: i64,
) -> String {
    let mut hasher = Sha256::new();
    hasher.update(jwt_secret.as_bytes());
    hasher.update(b":");
    hasher.update(username.as_bytes());
    hasher.update(b":");
    hasher.update(password.as_bytes());
    hasher.update(b":");
    hasher.update(password_hash.as_bytes());
    hasher.update(b":");
    hasher.update(auth_version.to_be_bytes());
    let digest = format!("{:x}", hasher.finalize());
    format!("{username}:{digest}")
}

impl LoginMemoCache {
    async fn from_env() -> Option<Self> {
        if !login_memo_enabled() {
            info!("login memo cache disabled by SAG_LOGIN_MEMO_ENABLED");
            return None;
        }
        let ttl_sec = login_memo_ttl_sec();
        if let Ok(url) = std::env::var("SAG_SESSION_REDIS_URL") {
            if !url.trim().is_empty() {
                match redis::Client::open(url.clone()) {
                    Ok(client) => match redis::aio::ConnectionManager::new(client).await {
                        Ok(conn) => {
                            info!("login memo cache backend: redis (connection-manager)");
                            metrics::counter!("sag_auth_login_memo_backend_redis_total")
                                .increment(1);
                            return Some(Self {
                                ttl_sec,
                                backend: LoginMemoBackend::Redis {
                                    conn: Box::new(conn),
                                    key_prefix: "sag:auth:login_memo:".to_string(),
                                },
                            });
                        }
                        Err(e) => {
                            tracing::warn!(
                                    "redis connection manager init failed, fallback to in-memory cache: {e}"
                                );
                        }
                    },
                    Err(e) => {
                        tracing::warn!(
                            "redis client init failed, fallback to in-memory cache: {e}"
                        );
                    }
                }
            }
        }
        let max_capacity = login_memo_max_capacity();
        info!(max_capacity, "login memo cache backend: in-memory");
        metrics::counter!("sag_auth_login_memo_backend_in_memory_total").increment(1);
        Some(Self {
            ttl_sec,
            backend: LoginMemoBackend::InMemory(
                Cache::builder()
                    .time_to_live(std::time::Duration::from_secs(ttl_sec))
                    .max_capacity(max_capacity)
                    .build(),
            ),
        })
    }

    async fn get(&self, key: &str) -> Option<String> {
        match &self.backend {
            LoginMemoBackend::InMemory(cache) => cache.get(key).await,
            LoginMemoBackend::Redis { conn, key_prefix } => {
                let full_key = format!("{key_prefix}{key}");
                let mut c = conn.as_ref().clone();
                c.get::<_, Option<String>>(full_key).await.ok().flatten()
            }
        }
    }

    async fn set(&self, key: &str, value: &str) {
        match &self.backend {
            LoginMemoBackend::InMemory(cache) => {
                cache.insert(key.to_string(), value.to_string()).await;
            }
            LoginMemoBackend::Redis { conn, key_prefix } => {
                let full_key = format!("{key_prefix}{key}");
                let mut c = conn.as_ref().clone();
                let _: Result<(), _> = c
                    .set_ex::<_, _, ()>(full_key, value.to_string(), self.ttl_sec)
                    .await;
            }
        }
    }
}

impl IdentityReadCache {
    fn from_env() -> Self {
        let ttl_sec = identity_read_cache_ttl_sec();
        let max_capacity = identity_read_cache_max_capacity();
        Self {
            providers: Cache::builder()
                .time_to_live(std::time::Duration::from_secs(ttl_sec))
                .max_capacity(max_capacity)
                .build(),
            mappings: Cache::builder()
                .time_to_live(std::time::Duration::from_secs(ttl_sec))
                .max_capacity(max_capacity)
                .build(),
        }
    }

    async fn get_providers(&self) -> Option<Vec<IdentityProviderDto>> {
        self.providers.get("all").await
    }

    async fn set_providers(&self, rows: Vec<IdentityProviderDto>) {
        self.providers.insert("all".to_string(), rows).await;
    }

    async fn invalidate_providers(&self) {
        self.providers.invalidate("all").await;
    }

    async fn get_mappings(&self, provider_id: Option<&str>) -> Option<Vec<GroupRoleMappingDto>> {
        self.mappings.get(provider_id.unwrap_or("*")).await
    }

    async fn set_mappings(&self, provider_id: Option<&str>, rows: Vec<GroupRoleMappingDto>) {
        self.mappings
            .insert(provider_id.unwrap_or("*").to_string(), rows)
            .await;
    }

    async fn invalidate_mappings(&self) {
        self.mappings.invalidate_all();
    }
}

fn role_to_cn(role: &str) -> String {
    match role {
        "admin" => "管理员".to_string(),
        "boss" => "老板".to_string(),
        "tech" => "技术".to_string(),
        "finance" => "财务".to_string(),
        "vendor" => "外包".to_string(),
        other => other.to_string(),
    }
}

fn foura_roles_for_employee(emp: &str) -> Vec<String> {
    // Optional per-employee role mapping for SSO simulation:
    // SAG_FOURA_ROLE_MAP="boss:boss;alice:tech;bob:ops"
    if let Ok(map_raw) = std::env::var("SAG_FOURA_ROLE_MAP") {
        for pair in map_raw.split(';') {
            let p = pair.trim();
            if p.is_empty() {
                continue;
            }
            let mut kv = p.splitn(2, ':');
            let k = kv.next().unwrap_or("").trim();
            let v = kv.next().unwrap_or("").trim();
            if k.eq_ignore_ascii_case(emp) && !v.is_empty() {
                let roles: Vec<String> = v
                    .split(',')
                    .map(|s| s.trim().to_string())
                    .filter(|s| !s.is_empty())
                    .collect();
                if !roles.is_empty() {
                    return roles;
                }
            }
        }
    }

    let roles_raw = std::env::var("SAG_FOURA_DEFAULT_ROLES").unwrap_or_else(|_| "user".into());
    roles_raw
        .split(',')
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect()
}

fn split_roles_csv(s: &str) -> Vec<String> {
    s.split(',')
        .map(|x| x.trim().to_string())
        .filter(|x| !x.is_empty())
        .collect()
}

async fn mapped_roles_for_groups(
    state: &AppState,
    provider_id: &str,
    groups: &[String],
) -> Result<Vec<String>, (StatusCode, String)> {
    if groups.is_empty() {
        return Ok(vec![]);
    }
    let rows = IdentityStore::list_mappings(&state.store, Some(provider_id))
        .await
        .map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("list mappings failed: {e}"),
            )
        })?;
    let mut roles = Vec::<String>::new();
    for m in rows {
        if !m.enabled {
            continue;
        }
        if groups.iter().any(|g| g == &m.external_group) {
            roles.extend(split_roles_csv(&m.local_roles_csv));
        }
    }
    roles.sort();
    roles.dedup();
    Ok(roles)
}

fn oidc_user_id(v: &serde_json::Value) -> String {
    v.get("preferred_username")
        .and_then(|x| x.as_str())
        .or_else(|| v.get("email").and_then(|x| x.as_str()))
        .or_else(|| v.get("sub").and_then(|x| x.as_str()))
        .unwrap_or("unknown")
        .to_string()
}

fn prune_oauth_states(map: &mut HashMap<String, OAuthState>) {
    let now = now_epoch_sec();
    map.retain(|_, v| v.expires_at > now);
}

fn verify_password(hash: &str, password: &str) -> bool {
    let Ok(parsed) = PasswordHash::new(hash) else {
        return false;
    };
    Argon2::default()
        .verify_password(password.as_bytes(), &parsed)
        .is_ok()
}

fn parse_bearer(headers: &HeaderMap) -> Option<String> {
    let v = headers
        .get("authorization")?
        .to_str()
        .ok()?
        .trim()
        .to_string();
    let mut parts = v.splitn(2, ' ');
    let scheme = parts.next()?.trim();
    let token = parts.next()?.trim();
    if scheme.eq_ignore_ascii_case("bearer") && !token.is_empty() {
        Some(token.to_string())
    } else {
        None
    }
}

async fn require_admin(
    headers: &HeaderMap,
    state: &AppState,
) -> Result<Claims, (StatusCode, String)> {
    let Some(token) = parse_bearer(headers) else {
        return Err((StatusCode::UNAUTHORIZED, "missing bearer token".into()));
    };
    let data = decode::<Claims>(
        &token,
        &DecodingKey::from_secret(state.jwt_secret.as_bytes()),
        &Validation::default(),
    )
    .map_err(|_| (StatusCode::UNAUTHORIZED, "invalid token".into()))?;
    let current = state
        .user_directory
        .current_by_id(&data.claims.sub)
        .await
        .map_err(|error| {
            metrics::counter!("auth_invalidation_failed_total", "stage" => "admin_verify")
                .increment(1);
            (
                StatusCode::SERVICE_UNAVAILABLE,
                format!("authorization state unavailable: {error}"),
            )
        })?;
    let Some(current) = current else {
        metrics::counter!("token_version_rejected_total", "reason" => "user_missing").increment(1);
        return Err((StatusCode::UNAUTHORIZED, "revoked token".into()));
    };
    if !current.enabled || current.auth_version != data.claims.auth_version {
        metrics::counter!("token_version_rejected_total", "reason" => "version_or_status")
            .increment(1);
        return Err((StatusCode::UNAUTHORIZED, "revoked token".into()));
    }
    if !current
        .roles
        .iter()
        .any(|r| r == "admin" || r == "boss" || r == "ops")
    {
        return Err((StatusCode::FORBIDDEN, "insufficient role".into()));
    }
    Ok(data.claims)
}

fn health_duration_env(name: &str, default_ms: u64) -> Duration {
    Duration::from_millis(
        std::env::var(name)
            .ok()
            .and_then(|value| value.parse().ok())
            .unwrap_or(default_ms),
    )
}

async fn live() -> StatusCode {
    StatusCode::OK
}

async fn ready(State(state): State<AppState>) -> StatusCode {
    let store = state.store.clone();
    let status = state
        .readiness
        .probe(
            health_duration_env("SAG_READINESS_PROBE_TIMEOUT_MS", 1_000),
            async move { store.health_check().await.is_ok() },
        )
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
            let event = FaultEventRecord {
                id: uuid::Uuid::new_v4().to_string(),
                ts_ms: now_epoch_ms(),
                service: "sag-auth".to_string(),
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
            let store = state.store.clone();
            tokio::spawn(async move {
                let _ = FaultEventsStore::insert(&store, &event).await;
            });
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
        "service" => "sag-auth",
        "method" => method.clone(),
        "path" => path.clone(),
        "status" => status.clone()
    );
    c.increment(1);
    let h = metrics::histogram!(
        "http_request_duration_seconds",
        "service" => "sag-auth",
        "method" => method2.clone(),
        "path" => path.clone(),
        "status" => status.clone()
    );
    h.record(elapsed);
    let row = AuditLogRecord {
        id: uuid::Uuid::new_v4().to_string(),
        ts_ms: std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as i64)
            .unwrap_or(0),
        service: "sag-auth".to_string(),
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
    res
}

fn issue_jwt(
    state: &AppState,
    id: String,
    username: String,
    roles: Vec<String>,
    external_groups: Vec<String>,
    auth_version: i64,
) -> Result<LoginResponse, (StatusCode, String)> {
    let expires_in_sec = jwt_expires_in_sec();
    let now = now_epoch_sec();
    let claims = Claims {
        sub: id.clone(),
        username: username.clone(),
        roles: roles.clone(),
        external_groups: external_groups.clone(),
        exp: (now + expires_in_sec) as usize,
        iat: now as usize,
        iss: "sag-auth".into(),
        auth_version,
    };
    let token = encode(
        &Header::default(),
        &claims,
        &EncodingKey::from_secret(state.jwt_secret.as_bytes()),
    )
    .map_err(|e| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("token encode: {e}"),
        )
    })?;

    Ok(LoginResponse {
        token,
        user: UserDto {
            id,
            username,
            roles,
            external_groups,
            roles_display: None,
            display_name: None,
            title: None,
        },
        expires_in_sec,
    })
}

async fn login(
    State(state): State<AppState>,
    Json(payload): Json<LoginRequest>,
) -> Result<Json<LoginResponse>, (StatusCode, String)> {
    if std::env::var("SAG_ALLOW_PASSWORD_LOGIN")
        .map(|v| v == "false")
        .unwrap_or(false)
    {
        return Err((StatusCode::FORBIDDEN, "password login disabled".into()));
    }
    let user_snapshot = state
        .user_directory
        .load_login_user(&payload.username)
        .await
        .map_err(|error| {
            metrics::counter!("auth_invalidation_failed_total", "stage" => "login_read")
                .increment(1);
            (
                StatusCode::SERVICE_UNAVAILABLE,
                format!("authorization state unavailable: {error}"),
            )
        })?
        .filter(|user| user.enabled)
        .ok_or((StatusCode::UNAUTHORIZED, "invalid credentials".into()))?;

    let cache_key = login_memo_key(
        &state.jwt_secret,
        &payload.username,
        &payload.password,
        &user_snapshot.password_hash,
        user_snapshot.auth_version,
    );
    if let Some(cache) = &state.login_memo_cache {
        if let Some(raw) = cache.get(&cache_key).await {
            if let Ok(entry) = serde_json::from_str::<LoginMemoEntry>(&raw) {
                metrics::counter!("sag_auth_login_memo_hit_total").increment(1);
                return Ok(Json(LoginResponse {
                    token: entry.token,
                    user: entry.user,
                    expires_in_sec: entry.expires_in_sec,
                }));
            }
        }
        metrics::counter!("sag_auth_login_memo_miss_total").increment(1);
    }

    if !verify_password(&user_snapshot.password_hash, &payload.password) {
        return Err((StatusCode::UNAUTHORIZED, "invalid credentials".into()));
    }

    let body = issue_jwt(
        &state,
        user_snapshot.id.clone(),
        user_snapshot.username.clone(),
        user_snapshot.roles.clone(),
        vec![],
        user_snapshot.auth_version,
    )?;

    if let Some(cache) = &state.login_memo_cache {
        // Cache the issued token to bypass both argon2 and jwt encode in hot-login bursts.
        // Key includes password_hash to ensure password updates invalidate memo quickly.
        let entry = LoginMemoEntry {
            token: body.token.clone(),
            user: body.user.clone(),
            expires_in_sec: body.expires_in_sec,
            cached_at_epoch_sec: now_epoch_sec(),
        };
        if let Ok(raw) = serde_json::to_string(&entry) {
            cache.set(&cache_key, &raw).await;
        }
    }
    Ok(Json(body))
}

fn external_base_url(headers: &HeaderMap) -> Option<String> {
    let proto = headers
        .get("x-forwarded-proto")
        .and_then(|v| v.to_str().ok())
        .filter(|s| !s.trim().is_empty())
        .unwrap_or("http");
    let host = headers
        .get("x-forwarded-host")
        .or_else(|| headers.get("host"))
        .and_then(|v| v.to_str().ok())?
        .trim();
    if host.is_empty() {
        return None;
    }
    Some(format!("{proto}://{host}"))
}

fn host_only_from_base(base: &str) -> Option<String> {
    let u = reqwest::Url::parse(base).ok()?;
    u.host_str().map(|h| h.to_string())
}

fn is_local_host(host: &str) -> bool {
    matches!(host, "127.0.0.1" | "localhost" | "::1")
}

fn resolve_portal_redirect_url(_headers: &HeaderMap) -> Option<String> {
    if let Some(explicit) = std::env::var("SAG_SSO_PORTAL_REDIRECT_URL")
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
    {
        return Some(explicit);
    }

    // Prefer fixed deploy host to avoid accidental localhost redirects.
    if let Some(host) = std::env::var("SAG_PUBLIC_HOST")
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty() && !is_local_host(s))
    {
        return Some(format!("http://{host}:3001/app"));
    }

    // Deployment default in this project context.
    Some("http://192.168.9.26:3001/app".to_string())
}

fn browser_first_uri(cfg_first: &str, headers: &HeaderMap) -> String {
    let Some(base) = external_base_url(headers) else {
        return cfg_first.to_string();
    };
    let Some(host) = host_only_from_base(&base) else {
        return cfg_first.to_string();
    };
    let Ok(mut u) = reqwest::Url::parse(cfg_first) else {
        return cfg_first.to_string();
    };
    let need_rewrite = matches!(
        u.host_str(),
        Some("127.0.0.1") | Some("localhost") | Some("fake-4a")
    );
    if !need_rewrite {
        return cfg_first.to_string();
    }
    let _ = u.set_host(Some(&host));
    let _ = u.set_port(Some(19080));
    u.to_string()
}

/// Public OAuth `redirect_uri` for the authorize step. Path comes from `SAG_FOURA_REDIRECT_URI`
/// (e.g. `/api-auth/api/v1/...` behind Next on :3001); only the origin is taken from forwarded headers.
/// If the browser talks to sag-auth directly on :8080, strip the Next-only `/api-auth` prefix.
fn oauth_redirect_uri_for_request(headers: &HeaderMap) -> String {
    let default = foura::redirect_uri();
    let Ok(cfg) = reqwest::Url::parse(&default) else {
        return default;
    };
    let Some(ext) = external_base_url(headers) else {
        return default;
    };
    let Ok(mut origin) = reqwest::Url::parse(&ext) else {
        return default;
    };
    let mut path = cfg.path().to_string();
    if matches!(origin.port(), Some(8080)) && path.starts_with("/api-auth/") {
        path = path["/api-auth".len()..].to_string();
        if !path.starts_with('/') {
            path = format!("/{path}");
        }
    }
    origin.set_path(&path);
    origin.set_query(None);
    origin.to_string()
}

async fn sso_login(
    headers: HeaderMap,
    State(state): State<AppState>,
    Query(q): Query<SsoLoginQuery>,
) -> Result<Redirect, (StatusCode, String)> {
    let provider_id = q.provider_id.unwrap_or_else(|| "foura".to_string());
    let row = IdentityStore::list_providers(&state.store)
        .await
        .map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("list providers failed: {e}"),
            )
        })?
        .into_iter()
        .find(|x| x.enabled && x.id == provider_id);

    let kind = row
        .as_ref()
        .map(|x| x.kind.to_lowercase())
        .unwrap_or_else(|| {
            if provider_id == "oidc" {
                "oidc".to_string()
            } else {
                "foura".to_string()
            }
        });

    let csrf = uuid::Uuid::new_v4().to_string();
    state
        .oauth_states
        .put(
            &csrf,
            OAuthState {
                expires_at: now_epoch_sec() + 600,
                provider_id: provider_id.clone(),
            },
        )
        .await;
    let redirect_uri = oauth_redirect_uri_for_request(&headers);
    let url = if kind == "oidc" {
        let mut oc = state.oidc.clone().ok_or((
            StatusCode::NOT_FOUND,
            "OIDC provider not configured".to_string(),
        ))?;
        if let Some(r) = row {
            if !r.client_id.trim().is_empty() {
                oc.client_id = r.client_id;
            }
            if !r.client_secret.trim().is_empty() {
                oc.client_secret = r.client_secret;
            }
            if !r.scopes.trim().is_empty() {
                oc.scopes = r.scopes;
            }
            if !r.issuer.trim().is_empty() {
                oc.issuer = r.issuer.clone();
                if std::env::var("SAG_OIDC_AUTHORIZE_URI").is_err() {
                    oc.authorize_uri = format!("{}/authorize", r.issuer.trim_end_matches('/'));
                }
                if std::env::var("SAG_OIDC_TOKEN_URI").is_err() {
                    oc.token_uri = format!("{}/token", r.issuer.trim_end_matches('/'));
                }
                if std::env::var("SAG_OIDC_USERINFO_URI").is_err() {
                    oc.userinfo_uri = format!("{}/userinfo", r.issuer.trim_end_matches('/'));
                }
            }
        }
        foura::authorize_url_with_redirect_oidc(&oc, &csrf, &redirect_uri).map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("authorize url: {e}"),
            )
        })?
    } else {
        let mut fc2 = state
            .foura
            .clone()
            .ok_or((StatusCode::NOT_FOUND, "4A SSO not configured".to_string()))?;
        fc2.first_uri = browser_first_uri(&fc2.first_uri, &headers);
        if let Some(r) = row {
            if !r.client_id.trim().is_empty() {
                fc2.client_id = r.client_id;
            }
            if !r.client_secret.trim().is_empty() {
                fc2.client_secret = r.client_secret;
            }
        }
        foura::authorize_url_with_redirect(&fc2, &csrf, &redirect_uri).map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("authorize url: {e}"),
            )
        })?
    };
    Ok(Redirect::temporary(&url))
}

async fn sso_callback(
    headers: HeaderMap,
    State(state): State<AppState>,
    Query(q): Query<SsoCallbackQuery>,
) -> Result<axum::response::Response, (StatusCode, String)> {
    let oauth_state = state.oauth_states.take(&q.state).await;
    let Some(oauth_state) = oauth_state else {
        return Err((StatusCode::BAD_REQUEST, "invalid or expired state".into()));
    };
    let provider_id = oauth_state.provider_id;
    let provider_row = IdentityStore::list_providers(&state.store)
        .await
        .map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("list providers failed: {e}"),
            )
        })?
        .into_iter()
        .find(|x| x.enabled && x.id == provider_id);
    let provider_kind = provider_row
        .as_ref()
        .map(|x| x.kind.to_lowercase())
        .unwrap_or_else(|| {
            if provider_id == "oidc" {
                "oidc".to_string()
            } else {
                "foura".to_string()
            }
        });

    let redirect_uri = oauth_redirect_uri_for_request(&headers);
    let (username, mut groups, roles) = if provider_kind == "oidc" {
        let mut oc = state.oidc.clone().ok_or((
            StatusCode::NOT_FOUND,
            "OIDC provider not configured".to_string(),
        ))?;
        if let Some(r) = provider_row {
            if !r.client_id.trim().is_empty() {
                oc.client_id = r.client_id;
            }
            if !r.client_secret.trim().is_empty() {
                oc.client_secret = r.client_secret;
            }
            if !r.scopes.trim().is_empty() {
                oc.scopes = r.scopes;
            }
            if !r.issuer.trim().is_empty() {
                oc.issuer = r.issuer.clone();
                if std::env::var("SAG_OIDC_TOKEN_URI").is_err() {
                    oc.token_uri = format!("{}/token", r.issuer.trim_end_matches('/'));
                }
                if std::env::var("SAG_OIDC_USERINFO_URI").is_err() {
                    oc.userinfo_uri = format!("{}/userinfo", r.issuer.trim_end_matches('/'));
                }
            }
        }

        let token = foura::exchange_code_for_token_oidc(&state.http, &oc, &q.code, &redirect_uri)
            .await
            .map_err(|e| (StatusCode::BAD_GATEWAY, e))?;
        let access = token.get("access_token").and_then(|x| x.as_str()).ok_or((
            StatusCode::BAD_GATEWAY,
            "oidc missing access_token".to_string(),
        ))?;
        let mut groups = foura::extract_groups(&token);
        let userinfo = foura::fetch_oidc_userinfo(&state.http, &oc, access)
            .await
            .map_err(|e| (StatusCode::BAD_GATEWAY, e))?;
        groups.extend(foura::extract_groups(&userinfo));
        groups.sort();
        groups.dedup();
        let roles = mapped_roles_for_groups(&state, &provider_id, &groups).await?;
        let roles = if roles.is_empty() {
            vec!["user".to_string()]
        } else {
            roles
        };
        (oidc_user_id(&userinfo), groups, roles)
    } else {
        let mut fc = state
            .foura
            .clone()
            .ok_or((StatusCode::NOT_FOUND, "4A SSO not configured".to_string()))?;
        if let Some(r) = provider_row {
            if !r.client_id.trim().is_empty() {
                fc.client_id = r.client_id;
            }
            if !r.client_secret.trim().is_empty() {
                fc.client_secret = r.client_secret;
            }
        }
        let access = foura::exchange_code_for_token(&state.http, &fc, &q.code)
            .await
            .map_err(|e| (StatusCode::BAD_GATEWAY, e))?;
        let emp = foura::fetch_user_employee_id(&state.http, &fc, &access)
            .await
            .map_err(|e| (StatusCode::BAD_GATEWAY, e))?;
        let roles = foura_roles_for_employee(&emp);
        (emp, vec![], roles)
    };

    if groups.is_empty() {
        groups = vec![];
    }
    let existing = UsersStore::load_by_username(&state.store, &username)
        .await
        .map_err(|error| {
            (
                StatusCode::SERVICE_UNAVAILABLE,
                format!("authorization state unavailable: {error}"),
            )
        })?;
    let authorization_changed = existing
        .as_ref()
        .is_none_or(|user| !user.enabled || user.roles != roles);
    if authorization_changed {
        let record = UserRecord {
            id: existing
                .as_ref()
                .map(|user| user.id.clone())
                .unwrap_or_else(|| format!("sso-{username}")),
            username: username.clone(),
            password_hash: existing
                .as_ref()
                .map(|user| user.password_hash.clone())
                .unwrap_or_else(|| "external-identity-no-password".into()),
            roles: roles.clone(),
            display_name: existing.as_ref().and_then(|user| user.display_name.clone()),
            title: existing.as_ref().and_then(|user| user.title.clone()),
            enabled: true,
            auth_version: existing.as_ref().map_or(1, |user| user.auth_version),
            updated_at_ms: 0,
        };
        UsersStore::upsert(&state.store, &record)
            .await
            .map_err(|error| {
                (
                    StatusCode::SERVICE_UNAVAILABLE,
                    format!("persist SSO authorization state failed: {error}"),
                )
            })?;
        state.user_directory.publish_invalidation(&record.id).await;
    }
    let current = state
        .user_directory
        .load_login_user(&username)
        .await
        .map_err(|error| {
            (
                StatusCode::SERVICE_UNAVAILABLE,
                format!("authorization state unavailable: {error}"),
            )
        })?
        .ok_or((StatusCode::UNAUTHORIZED, "SSO user is unavailable".into()))?;
    let body = issue_jwt(
        &state,
        current.id,
        username.clone(),
        current.roles,
        groups,
        current.auth_version,
    )?;

    // Resolve portal redirect without falling back to localhost.
    let target_portal = resolve_portal_redirect_url(&headers);
    if let Some(portal_url) = target_portal {
        let mut url = reqwest::Url::parse(&portal_url).map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("invalid portal redirect url: {e}"),
            )
        })?;
        url.query_pairs_mut().append_pair("sso_token", &body.token);
        return Ok(Redirect::temporary(url.as_str()).into_response());
    }

    Ok(Json(body).into_response())
}

async fn verify(
    State(state): State<AppState>,
    Json(payload): Json<VerifyRequest>,
) -> Result<Json<VerifyResponse>, (StatusCode, String)> {
    let data = match decode::<Claims>(
        &payload.token,
        &DecodingKey::from_secret(state.jwt_secret.as_bytes()),
        &Validation::default(),
    ) {
        Ok(v) => v,
        Err(_) => {
            return Ok(Json(VerifyResponse {
                active: false,
                user: None,
            }))
        }
    };
    let now = now_epoch_sec();
    if data.claims.exp < now as usize {
        return Ok(Json(VerifyResponse {
            active: false,
            user: None,
        }));
    }
    let current = state
        .user_directory
        .current_by_id(&data.claims.sub)
        .await
        .map_err(|error| {
            metrics::counter!("auth_invalidation_failed_total", "stage" => "token_verify")
                .increment(1);
            (
                StatusCode::SERVICE_UNAVAILABLE,
                format!("authorization state unavailable: {error}"),
            )
        })?;
    if current
        .as_ref()
        .is_none_or(|user| !user.enabled || user.auth_version != data.claims.auth_version)
    {
        metrics::counter!("token_version_rejected_total").increment(1);
        return Ok(Json(VerifyResponse {
            active: false,
            user: None,
        }));
    }

    Ok(Json(VerifyResponse {
        active: true,
        user: Some(UserDto {
            roles_display: Some(data.claims.roles.iter().map(|r| role_to_cn(r)).collect()),
            id: data.claims.sub,
            username: data.claims.username,
            roles: data.claims.roles,
            external_groups: data.claims.external_groups,
            display_name: None,
            title: None,
        }),
    }))
}

fn hash_password(password: &str) -> Result<String, (StatusCode, String)> {
    let salt = SaltString::generate(&mut rand::thread_rng());
    let argon = Argon2::default();
    argon
        .hash_password(password.as_bytes(), &salt)
        .map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("password hash failed: {e}"),
            )
        })
        .map(|v| v.to_string())
}

async fn list_users(
    headers: HeaderMap,
    State(state): State<AppState>,
) -> Result<Json<Vec<UserDto>>, (StatusCode, String)> {
    require_admin(&headers, &state).await?;
    if let Some(rows) = state.users_read_cache.get("all").await {
        return Ok(Json(rows));
    }
    let users = state.users.read().await;
    let mut rows: Vec<UserDto> = users
        .values()
        .map(|u| UserDto {
            id: u.id.clone(),
            username: u.username.clone(),
            roles: u.roles.clone(),
            external_groups: vec![],
            roles_display: Some(u.roles.iter().map(|r| role_to_cn(r)).collect()),
            display_name: u.display_name.clone(),
            title: u.title.clone(),
        })
        .collect();
    rows.sort_by(|a, b| a.username.cmp(&b.username));
    state
        .users_read_cache
        .insert("all".to_string(), rows.clone())
        .await;
    Ok(Json(rows))
}

async fn upsert_user(
    headers: HeaderMap,
    State(state): State<AppState>,
    Json(payload): Json<UpsertUserRequest>,
) -> Result<Json<UserDto>, (StatusCode, String)> {
    require_admin(&headers, &state).await?;
    if payload.username.trim().is_empty() {
        return Err((StatusCode::BAD_REQUEST, "username is required".into()));
    }
    let username = payload.username.trim().to_string();
    if payload.roles.is_empty() {
        return Err((StatusCode::BAD_REQUEST, "roles cannot be empty".into()));
    }
    let maybe_existing = UsersStore::load_by_username(&state.store, &username)
        .await
        .map_err(|error| {
            (
                StatusCode::SERVICE_UNAVAILABLE,
                format!("authorization state unavailable: {error}"),
            )
        })?;

    let password_hash = match (&payload.password, maybe_existing.as_ref()) {
        (Some(pw), _) if !pw.is_empty() => hash_password(pw)?,
        (_, Some(existing)) => existing.password_hash.clone(),
        _ => {
            return Err((
                StatusCode::BAD_REQUEST,
                "password is required for new user".into(),
            ))
        }
    };

    let mut user = User {
        id: payload
            .id
            .unwrap_or_else(|| format!("u-{}", username.to_lowercase())),
        username: username.clone(),
        roles: payload.roles,
        display_name: payload.display_name,
        title: payload.title,
        enabled: payload.enabled.unwrap_or(true),
        auth_version: maybe_existing.as_ref().map_or(1, |user| user.auth_version),
        updated_at_ms: 0,
    };

    let record = UserRecord {
        id: user.id.clone(),
        username: user.username.clone(),
        password_hash,
        roles: user.roles.clone(),
        display_name: user.display_name.clone(),
        title: user.title.clone(),
        enabled: user.enabled,
        auth_version: user.auth_version,
        updated_at_ms: user.updated_at_ms,
    };
    let audit = AuditLogRecord::management(
        "sag-auth",
        management_actor(&headers),
        "",
        format!("/api/v1/users/{username}"),
        "PUT",
    );
    AuditLogsStore::apply_security_mutation(
        &state.store,
        &SecurityMutation::UpsertUser(record),
        &audit,
    )
    .await
    .map_err(|e| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("persist user failed: {e}"),
        )
    })?;
    let persisted = UsersStore::load_by_username(&state.store, &username)
        .await
        .map_err(|error| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("reload user version failed: {error}"),
            )
        })?
        .ok_or((
            StatusCode::INTERNAL_SERVER_ERROR,
            "persisted user disappeared".into(),
        ))?;
    user.auth_version = persisted.auth_version;
    user.updated_at_ms = persisted.updated_at_ms;
    state.user_directory.publish_invalidation(&user.id).await;

    {
        let mut users = state.users.write().await;
        users.insert(username, user.clone());
    }
    state.users_read_cache.invalidate("all").await;

    let roles_display = user.roles.iter().map(|r| role_to_cn(r)).collect();
    Ok(Json(UserDto {
        id: user.id,
        username: user.username,
        roles: user.roles,
        external_groups: vec![],
        roles_display: Some(roles_display),
        display_name: user.display_name,
        title: user.title,
    }))
}

async fn delete_user(
    headers: HeaderMap,
    State(state): State<AppState>,
    axum::extract::Path(username): axum::extract::Path<String>,
) -> Result<StatusCode, (StatusCode, String)> {
    require_admin(&headers, &state).await?;
    if username == "admin" {
        return Err((StatusCode::BAD_REQUEST, "cannot delete admin".into()));
    }
    let existing = UsersStore::load_by_username(&state.store, &username)
        .await
        .map_err(|error| {
            (
                StatusCode::SERVICE_UNAVAILABLE,
                format!("authorization state unavailable: {error}"),
            )
        })?;
    let audit = AuditLogRecord::management(
        "sag-auth",
        management_actor(&headers),
        "",
        format!("/api/v1/users/{username}"),
        "DELETE",
    );
    AuditLogsStore::apply_security_mutation(
        &state.store,
        &SecurityMutation::DeleteUser(username.clone()),
        &audit,
    )
    .await
    .map_err(|e| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("delete user failed: {e}"),
        )
    })?;
    {
        let mut users = state.users.write().await;
        users.remove(&username);
    }
    state.users_read_cache.invalidate("all").await;
    if let Some(existing) = existing {
        state
            .user_directory
            .publish_invalidation(&existing.id)
            .await;
    }
    Ok(StatusCode::NO_CONTENT)
}

async fn list_identity_providers(
    headers: HeaderMap,
    State(state): State<AppState>,
) -> Result<Json<Vec<IdentityProviderDto>>, (StatusCode, String)> {
    require_admin(&headers, &state).await?;
    if let Some(rows) = state.identity_read_cache.get_providers().await {
        return Ok(Json(rows));
    }
    let rows = IdentityStore::list_providers(&state.store)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    let rows: Vec<IdentityProviderDto> = rows
        .into_iter()
        .map(|r| IdentityProviderDto {
            id: r.id,
            kind: r.kind,
            issuer: r.issuer,
            client_id: r.client_id,
            client_secret: r.client_secret,
            scopes: r.scopes,
            enabled: r.enabled,
        })
        .collect();
    state.identity_read_cache.set_providers(rows.clone()).await;
    Ok(Json(rows))
}

async fn upsert_identity_provider(
    headers: HeaderMap,
    State(state): State<AppState>,
    Json(body): Json<IdentityProviderDto>,
) -> Result<StatusCode, (StatusCode, String)> {
    require_admin(&headers, &state).await?;
    if body.id.trim().is_empty() {
        return Err((StatusCode::BAD_REQUEST, "id is required".into()));
    }
    let rec = IdentityProviderRecord {
        id: body.id,
        kind: body.kind,
        issuer: body.issuer,
        client_id: body.client_id,
        client_secret: body.client_secret,
        scopes: body.scopes,
        enabled: body.enabled,
    };
    let audit = AuditLogRecord::management(
        "sag-auth",
        management_actor(&headers),
        "",
        format!("/api/v1/identity/providers/{}", rec.id),
        "PUT",
    );
    AuditLogsStore::apply_security_mutation(
        &state.store,
        &SecurityMutation::UpsertIdentityProvider(rec),
        &audit,
    )
    .await
    .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    state.identity_read_cache.invalidate_providers().await;
    Ok(StatusCode::CREATED)
}

async fn delete_identity_provider(
    headers: HeaderMap,
    State(state): State<AppState>,
    axum::extract::Path(id): axum::extract::Path<String>,
) -> Result<StatusCode, (StatusCode, String)> {
    require_admin(&headers, &state).await?;
    let audit = AuditLogRecord::management(
        "sag-auth",
        management_actor(&headers),
        "",
        format!("/api/v1/identity/providers/{id}"),
        "DELETE",
    );
    AuditLogsStore::apply_security_mutation(
        &state.store,
        &SecurityMutation::DeleteIdentityProvider(id),
        &audit,
    )
    .await
    .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    state.identity_read_cache.invalidate_providers().await;
    Ok(StatusCode::NO_CONTENT)
}

async fn list_group_role_mappings(
    headers: HeaderMap,
    State(state): State<AppState>,
    Query(q): Query<MappingsQuery>,
) -> Result<Json<Vec<GroupRoleMappingDto>>, (StatusCode, String)> {
    require_admin(&headers, &state).await?;
    if let Some(rows) = state
        .identity_read_cache
        .get_mappings(q.provider_id.as_deref())
        .await
    {
        return Ok(Json(rows));
    }
    let rows = IdentityStore::list_mappings(&state.store, q.provider_id.as_deref())
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    let rows: Vec<GroupRoleMappingDto> = rows
        .into_iter()
        .map(|r| GroupRoleMappingDto {
            id: r.id,
            provider_id: r.provider_id,
            external_group: r.external_group,
            local_roles_csv: r.local_roles_csv,
            enabled: r.enabled,
            priority: r.priority,
        })
        .collect();
    state
        .identity_read_cache
        .set_mappings(q.provider_id.as_deref(), rows.clone())
        .await;
    Ok(Json(rows))
}

async fn upsert_group_role_mapping(
    headers: HeaderMap,
    State(state): State<AppState>,
    Json(mut body): Json<GroupRoleMappingDto>,
) -> Result<StatusCode, (StatusCode, String)> {
    require_admin(&headers, &state).await?;
    if body.provider_id.trim().is_empty() {
        return Err((StatusCode::BAD_REQUEST, "provider_id is required".into()));
    }
    if body.external_group.trim().is_empty() {
        return Err((StatusCode::BAD_REQUEST, "external_group is required".into()));
    }
    if body.id.trim().is_empty() {
        body.id = format!("{}:{}", body.provider_id, body.external_group);
    }
    let rec = GroupRoleMappingRecord {
        id: body.id,
        provider_id: body.provider_id,
        external_group: body.external_group,
        local_roles_csv: body.local_roles_csv,
        enabled: body.enabled,
        priority: body.priority,
    };
    let audit = AuditLogRecord::management(
        "sag-auth",
        management_actor(&headers),
        "",
        format!("/api/v1/identity/mappings/{}", rec.id),
        "PUT",
    );
    AuditLogsStore::apply_security_mutation(
        &state.store,
        &SecurityMutation::UpsertGroupRoleMapping(rec),
        &audit,
    )
    .await
    .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    state.identity_read_cache.invalidate_mappings().await;
    Ok(StatusCode::CREATED)
}

async fn delete_group_role_mapping(
    headers: HeaderMap,
    State(state): State<AppState>,
    axum::extract::Path(id): axum::extract::Path<String>,
) -> Result<StatusCode, (StatusCode, String)> {
    require_admin(&headers, &state).await?;
    let audit = AuditLogRecord::management(
        "sag-auth",
        management_actor(&headers),
        "",
        format!("/api/v1/identity/mappings/{id}"),
        "DELETE",
    );
    AuditLogsStore::apply_security_mutation(
        &state.store,
        &SecurityMutation::DeleteGroupRoleMapping(id),
        &audit,
    )
    .await
    .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    state.identity_read_cache.invalidate_mappings().await;
    Ok(StatusCode::NO_CONTENT)
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    let jwt_secret =
        std::env::var("SAG_JWT_SECRET").unwrap_or_else(|_| "dev-jwt-secret".to_string());
    let pw =
        std::env::var("SAG_BOOTSTRAP_ADMIN_PASSWORD").unwrap_or_else(|_| "Admin@123".to_string());
    let store = build_store_from_env();
    ensure_store_schema(&store).await?;
    let audit_writer = AuditWriter::from_env(store.clone())?;
    let mut users = HashMap::new();
    for u in UsersStore::load_all(&store).await? {
        users.insert(
            u.username.clone(),
            User {
                id: u.id,
                username: u.username,
                roles: u.roles,
                display_name: u.display_name,
                title: u.title,
                enabled: u.enabled,
                auth_version: u.auth_version,
                updated_at_ms: u.updated_at_ms,
            },
        );
    }
    if !users.contains_key("admin") {
        let hash = hash_password(&pw).map_err(|e| anyhow::anyhow!(e.1))?;
        let admin = User {
            id: "u-admin".to_string(),
            username: "admin".to_string(),
            roles: vec!["admin".to_string()],
            display_name: Some("系统管理员".to_string()),
            title: Some("平台管理员".to_string()),
            enabled: true,
            auth_version: 1,
            updated_at_ms: 0,
        };
        UsersStore::upsert(
            &store,
            &UserRecord {
                id: admin.id.clone(),
                username: admin.username.clone(),
                password_hash: hash,
                roles: admin.roles.clone(),
                display_name: admin.display_name.clone(),
                title: admin.title.clone(),
                enabled: true,
                auth_version: admin.auth_version,
                updated_at_ms: admin.updated_at_ms,
            },
        )
        .await?;
        users.insert("admin".to_string(), admin);
    }

    let foura = foura::config_from_env();
    let oidc = foura::oidc_config_from_env();
    let sso_on = foura.is_some() || oidc.is_some();
    if sso_on {
        info!("SSO enabled (configurable OIDC/4A authorization code flow)");
    }

    let user_directory = UserDirectory::from_env(store.clone());
    let state = AppState {
        jwt_secret: Arc::new(jwt_secret.clone()),
        users: Arc::new(RwLock::new(users)),
        store,
        audit_writer,
        http: reqwest::Client::builder().build()?,
        foura,
        oidc,
        oauth_states: OAuthStateStore::from_env().await,
        metrics: metrics_exporter_prometheus::PrometheusBuilder::new()
            .install_recorder()
            .map_err(|e| anyhow::anyhow!("install prometheus recorder failed: {e}"))?,
        login_memo_cache: LoginMemoCache::from_env().await,
        identity_read_cache: IdentityReadCache::from_env(),
        users_read_cache: Cache::builder()
            .time_to_live(std::time::Duration::from_secs(users_read_cache_ttl_sec()))
            .max_capacity(users_read_cache_max_capacity())
            .build(),
        readiness: Readiness::new(
            std::env::var("SAG_READINESS_SUCCESS_THRESHOLD")
                .ok()
                .and_then(|value| value.parse().ok())
                .unwrap_or(2),
        ),
        user_directory,
    };

    let mut app = Router::new()
        .route("/live", get(live))
        .route("/ready", get(ready))
        .route("/health", get(ready))
        .route("/metrics", get(metrics))
        .route("/api/v1/auth/login", post(login))
        .route("/api/v1/auth/verify", post(verify))
        .route("/api/v1/users", get(list_users).post(upsert_user))
        .route(
            "/api/v1/users/{username}",
            axum::routing::delete(delete_user),
        )
        .route(
            "/api/v1/identity/providers",
            get(list_identity_providers).post(upsert_identity_provider),
        )
        .route(
            "/api/v1/identity/providers/{id}",
            axum::routing::delete(delete_identity_provider),
        )
        .route(
            "/api/v1/identity/mappings",
            get(list_group_role_mappings).post(upsert_group_role_mapping),
        )
        .route(
            "/api/v1/identity/mappings/{id}",
            axum::routing::delete(delete_group_role_mapping),
        );

    if sso_on {
        app = app
            .route("/api/v1/auth/sso/login", get(sso_login))
            .route("/api/v1/auth/sso/callback", get(sso_callback));
    }

    let app = app
        .layer(middleware::from_fn_with_state(state.clone(), metrics_mw))
        .layer(middleware::from_fn_with_state(state.clone(), admission_mw))
        .layer(TraceLayer::new_for_http())
        .with_state(state.clone());

    let addr: SocketAddr = "0.0.0.0:8080".parse()?;
    info!(%addr, "sag-auth listening");
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
    let drain_timeout = health_duration_env("SAG_DRAIN_TIMEOUT_MS", 30_000);
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

    fn headers_with_roles(roles: Vec<&str>) -> HeaderMap {
        let now = now_epoch_sec();
        let claims = Claims {
            sub: "test-user".to_string(),
            username: "test-user".to_string(),
            roles: roles.into_iter().map(str::to_string).collect(),
            external_groups: Vec::new(),
            exp: (now + 300) as usize,
            iat: now as usize,
            iss: "sag-auth".to_string(),
            auth_version: 1,
        };
        let token = encode(
            &Header::default(),
            &claims,
            &EncodingKey::from_secret(b"test-jwt-secret"),
        )
        .unwrap();
        let mut headers = HeaderMap::new();
        headers.insert(
            axum::http::header::AUTHORIZATION,
            format!("Bearer {token}").parse().unwrap(),
        );
        headers
    }

    async fn test_state(roles: Vec<&str>) -> AppState {
        let recorder = metrics_exporter_prometheus::PrometheusBuilder::new().build_recorder();
        let database_path =
            std::env::temp_dir().join(format!("test-sag-auth-{}.db", uuid::Uuid::new_v4()));
        let store = StorageStore::Sqlite(shared_storage::SqliteStore::new(
            database_path.to_string_lossy().to_string(),
        ));
        let audit_writer = AuditWriter::new(store.clone(), Default::default()).unwrap();
        ensure_store_schema(&store).await.unwrap();
        UsersStore::upsert(
            &store,
            &UserRecord {
                id: "test-user".into(),
                username: "test-user".into(),
                password_hash: "unused".into(),
                roles: roles.into_iter().map(str::to_string).collect(),
                display_name: None,
                title: None,
                enabled: true,
                auth_version: 1,
                updated_at_ms: 0,
            },
        )
        .await
        .unwrap();
        let user_directory = UserDirectory::new(store.clone(), Duration::from_secs(30), 16, None);
        AppState {
            jwt_secret: Arc::new("test-jwt-secret".to_string()),
            users: Arc::new(RwLock::new(HashMap::new())),
            store,
            audit_writer,
            http: reqwest::Client::new(),
            foura: None,
            oidc: None,
            oauth_states: OAuthStateStore {
                backend: OAuthStateBackend::InMemory(Arc::new(RwLock::new(HashMap::new()))),
            },
            metrics: recorder.handle(),
            login_memo_cache: None,
            identity_read_cache: IdentityReadCache::from_env(),
            users_read_cache: Cache::new(16),
            readiness: Readiness::new(1),
            user_directory,
        }
    }

    #[tokio::test]
    async fn list_users_requires_a_bearer_token() {
        let err = list_users(HeaderMap::new(), State(test_state(vec!["admin"]).await))
            .await
            .expect_err("anonymous user listing must be rejected");
        assert_eq!(err.0, StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn list_users_rejects_a_non_privileged_role() {
        let err = list_users(
            headers_with_roles(vec!["user"]),
            State(test_state(vec!["user"]).await),
        )
        .await
        .expect_err("ordinary users must not list accounts");
        assert_eq!(err.0, StatusCode::FORBIDDEN);
    }

    #[tokio::test]
    async fn list_users_allows_an_admin_role() {
        let response = list_users(
            headers_with_roles(vec!["admin"]),
            State(test_state(vec!["admin"]).await),
        )
        .await
        .expect("admin should be allowed to list accounts");
        assert!(response.0.is_empty());
    }
}
