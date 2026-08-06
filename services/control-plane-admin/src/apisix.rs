//! 可选：�?`intranet_upstreams` + 隧道路由同步�?APISIX Admin API�?
//! 策略见仓库根目录 `APISIX_INTRARENT_STRATEGY.md`：APISIX 只做 L7 流量治理，最终授权仍�?Agent+PDP�?
use std::collections::BTreeMap;
use std::fmt::Write as _;

use reqwest::Client;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use shared_storage::{IntranetUpstreamRecord, RoutesStore, StorageStore};
use tracing::info;

const MANAGED_ROUTE_PREFIX: &str = "sag-route-";
const HASHED_ROUTE_PREFIX: &str = "sag-route-v2-";
const MANAGED_BY_LABEL: &str = "control-plane-admin";
const APISIX_ID_MAX_LEN: usize = 64;
// A normal 500-route APISIX page is far smaller than this. Keep a hard cap at
// the transport boundary so an absent or dishonest Content-Length cannot make
// reconciliation buffer an unbounded response before route-count checks run.
const APISIX_INVENTORY_MAX_RESPONSE_BYTES: usize = 8 * 1024 * 1024;

#[derive(Clone)]
pub struct ApisixPushConfig {
    pub admin_base: String,
    pub admin_key: String,
}

pub fn config_from_env() -> Option<ApisixPushConfig> {
    let admin_base = std::env::var("SAG_APISIX_ADMIN_BASE_URL").ok()?;
    let admin_key = std::env::var("SAG_APISIX_ADMIN_API_KEY").ok()?;
    if admin_base.is_empty() || admin_key.is_empty() {
        return None;
    }
    Some(ApisixPushConfig {
        admin_base: admin_base.trim_end_matches('/').to_string(),
        admin_key,
    })
}

pub(crate) fn route_id_for_app(app_id: &str) -> String {
    let directly_encodable = !app_id.is_empty()
        && app_id.len() <= APISIX_ID_MAX_LEN - MANAGED_ROUTE_PREFIX.len()
        && app_id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'.' | b'_'));
    if directly_encodable {
        return format!("{MANAGED_ROUTE_PREFIX}{app_id}");
    }

    // APISIX caps textual IDs at 64 characters. A 160-bit SHA-256 prefix keeps
    // unsafe/long app IDs deterministic and collision-resistant without the
    // lossy character replacement used by the legacy implementation.
    let digest = Sha256::digest(app_id.as_bytes());
    let mut encoded = String::with_capacity(40);
    for byte in &digest[..20] {
        write!(&mut encoded, "{byte:02x}").expect("writing to String cannot fail");
    }
    format!("{HASHED_ROUTE_PREFIX}{encoded}")
}

fn legacy_route_id_for_app(app_id: &str) -> String {
    let safe: String = app_id
        .chars()
        .map(|character| {
            if character.is_alphanumeric() || character == '-' {
                character
            } else {
                '_'
            }
        })
        .collect();
    format!("{MANAGED_ROUTE_PREFIX}{safe}")
}

fn is_valid_apisix_route_id(route_id: &str) -> bool {
    !route_id.is_empty()
        && route_id.len() <= APISIX_ID_MAX_LEN
        && route_id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'.' | b'_'))
}

fn build_app_route(app_id: &str, upstream: &IntranetUpstreamRecord) -> anyhow::Result<Value> {
    let nodes_key = upstream.upstream.trim().to_string();
    let scheme = match upstream.scheme.to_ascii_lowercase().as_str() {
        "http" => "http",
        "https" => "https",
        invalid => {
            anyhow::bail!("invalid APISIX upstream scheme {invalid:?}; expected http or https")
        }
    };
    let route_id = route_id_for_app(app_id);
    Ok(json!({
        "id": route_id,
        "name": format!("sag-{}", app_id),
        "labels": {
            "sag-managed-by": MANAGED_BY_LABEL,
            "sag-app-id": app_id
        },
        // Match all app paths; app isolation is enforced by x-sag-app-id in vars.
        "uri": "/*",
        "methods": ["GET", "POST", "PUT", "PATCH", "DELETE", "HEAD", "OPTIONS"],
        // Higher priority than generic /api/* routes.
        "priority": 100,
        // Route isolation by app_id avoids collisions between multiple app-* routes.
        "vars": [["http_x_sag_app_id", "==", app_id]],
        "plugins": {
            // Required for APISIX route-level latency/status series,
            // otherwise only node-level request counter may be visible.
            "prometheus": {},
            // Keep compatibility for /api/<name> style probes while serving
            // real app paths from company-demo-sites (/dev/, /ci/, ...).
            "proxy-rewrite": {
                // Normalize to upstream directory-style pages:
                // /api/vendor or /api/vendor/ -> /vendor/
                "regex_uri": ["^/api/(.*?)/?$", "/$1/"]
            }
        },
        "upstream": {
            "type": "roundrobin",
            "scheme": scheme,
            // APISIX must not retry a write after an ambiguous upstream result.
            // Its per-phase budget also remains below Connector's 55s total cap.
            "retries": 0,
            "timeout": {
                "connect": 3,
                "send": 5,
                "read": 45
            },
            "nodes": { nodes_key: 1 }
        }
    }))
}

async fn desired_app_route(
    store: &StorageStore,
    app_id: &str,
) -> anyhow::Result<Option<(String, Value)>> {
    let Some(upstream) = RoutesStore::get_intranet_upstream(store, app_id).await? else {
        return Ok(None);
    };
    Ok(Some((
        route_id_for_app(app_id),
        build_app_route(app_id, &upstream)?,
    )))
}

async fn desired_active_app_route(
    store: &StorageStore,
    app_id: &str,
) -> anyhow::Result<Option<(String, Value)>> {
    let has_tunnel_route = RoutesStore::load_all(store)
        .await?
        .iter()
        .any(|route| route.app_id == app_id);
    if !has_tunnel_route {
        return Ok(None);
    }
    desired_app_route(store, app_id).await
}

fn adopt_legacy_routes_enabled() -> bool {
    std::env::var("SAG_APISIX_ADOPT_LEGACY_ROUTES")
        .map(|value| {
            matches!(
                value.to_ascii_lowercase().as_str(),
                "1" | "true" | "yes" | "on"
            )
        })
        .unwrap_or(false)
}

fn route_resource_value(body: Value, route_id: &str) -> Value {
    let mut value = body.get("value").cloned().unwrap_or(body);
    if value.get("id").is_none() {
        value["id"] = Value::String(route_id.to_string());
    }
    value
}

fn managed_app_id(route: &Value) -> Option<&str> {
    (route
        .pointer("/labels/sag-managed-by")
        .and_then(Value::as_str)
        == Some(MANAGED_BY_LABEL))
    .then(|| {
        route
            .pointer("/labels/sag-app-id")
            .and_then(Value::as_str)
            .filter(|app_id| !app_id.is_empty())
    })
    .flatten()
}

fn is_legacy_route_for_app(route: &Value, app_id: &str) -> bool {
    let expected_name = format!("sag-{app_id}");
    if route.get("name").and_then(Value::as_str) != Some(expected_name.as_str()) {
        return false;
    }
    route
        .get("vars")
        .and_then(Value::as_array)
        .is_some_and(|vars| {
            vars.iter().any(|condition| {
                let Some(parts) = condition.as_array() else {
                    return false;
                };
                parts.len() == 3
                    && parts[0].as_str() == Some("http_x_sag_app_id")
                    && parts[1].as_str() == Some("==")
                    && parts[2].as_str() == Some(app_id)
            })
        })
}

fn is_unlabeled_legacy_route_for_app(route: &Value, app_id: &str) -> bool {
    route.pointer("/labels/sag-managed-by").is_none()
        && route.pointer("/labels/sag-app-id").is_none()
        && is_legacy_route_for_app(route, app_id)
}

fn legacy_route_app_id(route: &Value) -> Option<&str> {
    let name_app_id = route.get("name")?.as_str()?.strip_prefix("sag-")?;
    if name_app_id.is_empty() {
        return None;
    }
    route
        .get("vars")?
        .as_array()?
        .iter()
        .filter_map(Value::as_array)
        .find(|parts| {
            parts.len() == 3
                && parts[0].as_str() == Some("http_x_sag_app_id")
                && parts[1].as_str() == Some("==")
                && parts[2].as_str() == Some(name_app_id)
        })
        .map(|_| name_app_id)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ExtraRouteCleanupDecision {
    Delete,
    AlreadyReassigned,
    RejectUnverified,
}

fn extra_route_cleanup_decision(
    route: &Value,
    expected_app_id: &str,
    adopt_legacy: bool,
) -> ExtraRouteCleanupDecision {
    match managed_app_id(route) {
        Some(owner) if owner == expected_app_id => ExtraRouteCleanupDecision::Delete,
        Some(_) => ExtraRouteCleanupDecision::AlreadyReassigned,
        None if adopt_legacy && is_unlabeled_legacy_route_for_app(route, expected_app_id) => {
            ExtraRouteCleanupDecision::Delete
        }
        None => ExtraRouteCleanupDecision::RejectUnverified,
    }
}

fn is_cleanup_eligible_legacy_route(route: &Value, app_id: &str) -> bool {
    extra_route_cleanup_decision(route, app_id, adopt_legacy_routes_enabled())
        == ExtraRouteCleanupDecision::Delete
}

async fn fetch_route_by_id(
    client: &Client,
    cfg: &ApisixPushConfig,
    route_id: &str,
) -> anyhow::Result<Option<Value>> {
    let url = format!("{}/apisix/admin/routes/{route_id}", cfg.admin_base);
    let response = client
        .get(url)
        .header("X-API-KEY", &cfg.admin_key)
        .send()
        .await?;
    if response.status() == reqwest::StatusCode::NOT_FOUND {
        return Ok(None);
    }
    let status = response.status();
    if !status.is_success() {
        let body = response.text().await.unwrap_or_default();
        anyhow::bail!("apisix route get failed: {status} - {body}");
    }
    Ok(Some(route_resource_value(
        response.json::<Value>().await?,
        route_id,
    )))
}

async fn verify_route_write_ownership(
    client: &Client,
    cfg: &ApisixPushConfig,
    route_id: &str,
    app_id: &str,
) -> anyhow::Result<()> {
    let Some(existing) = fetch_route_by_id(client, cfg, route_id).await? else {
        return Ok(());
    };
    if managed_app_id(&existing) == Some(app_id) {
        return Ok(());
    }
    if managed_app_id(&existing).is_none()
        && adopt_legacy_routes_enabled()
        && is_unlabeled_legacy_route_for_app(&existing, app_id)
    {
        info!(%route_id, %app_id, "adopting a fingerprint-matched legacy SAG route");
        return Ok(());
    }
    anyhow::bail!(
        "refusing to overwrite APISIX route {route_id}: ownership labels do not match app {app_id}; set SAG_APISIX_ADOPT_LEGACY_ROUTES=true only for a verified legacy SAG route"
    )
}

/// 为单个 `app_id` 下发 Route（内联 upstream）。
/// 使用 `uri: "/*"` 统一承接真实业务路径；`/api/<name>` 由 proxy-rewrite 兼容映射。
async fn put_app_route(
    client: &Client,
    cfg: &ApisixPushConfig,
    app_id: &str,
    route_id: &str,
    route: &Value,
) -> anyhow::Result<()> {
    verify_route_write_ownership(client, cfg, route_id, app_id).await?;

    // NOTE: do NOT bind host by default.
    // In compose/dev, requests may reach APISIX with Host=apisix or Host=127.0.0.1,
    // while tunnel metadata host is `app.internal.com`. Binding host here would cause
    // confusing 404s and break the end-to-end smoke tests.

    let put_route_url = format!("{}/apisix/admin/routes/{route_id}", cfg.admin_base);
    let rr = client
        .put(&put_route_url)
        .header("X-API-KEY", &cfg.admin_key)
        .json(route)
        .send()
        .await?;

    let status = rr.status();
    if !status.is_success() {
        let t = rr.text().await.unwrap_or_default();
        anyhow::bail!("apisix route put failed: {} �?{}", status, t);
    }

    info!(app_id, route_id, "apisix route upserted");
    Ok(())
}

pub async fn delete_app_route(
    client: &Client,
    cfg: &ApisixPushConfig,
    app_id: &str,
) -> anyhow::Result<()> {
    let route_id = route_id_for_app(app_id);
    delete_owned_route(client, cfg, &route_id, Some(app_id)).await
}

async fn cleanup_legacy_app_route(
    client: &Client,
    cfg: &ApisixPushConfig,
    app_id: &str,
    current_route_id: &str,
) -> anyhow::Result<()> {
    let legacy_route_id = legacy_route_id_for_app(app_id);
    if legacy_route_id == current_route_id || !is_valid_apisix_route_id(&legacy_route_id) {
        return Ok(());
    }
    let Some(existing) = fetch_route_by_id(client, cfg, &legacy_route_id).await? else {
        return Ok(());
    };
    if !is_cleanup_eligible_legacy_route(&existing, app_id) {
        // A lossy legacy ID may collide with another app. Never delete a
        // mismatched occupant; that app's own convergence pass will adopt it
        // or a subsequent retry will create the now-free deterministic ID.
        return Ok(());
    }
    delete_owned_route(client, cfg, &legacy_route_id, Some(app_id)).await?;
    metrics::counter!("apisix_legacy_route_cleanup_total").increment(1);
    info!(%app_id, %legacy_route_id, %current_route_id, "removed fingerprint-verified legacy APISIX route after converging the current ID");
    Ok(())
}

/// Apply the latest control-plane intent for one app. Workers deliberately
/// derive the action at execution time so an older leased tombstone cannot
/// delete a route that was recreated by a newer generation.
pub async fn converge_app_route(
    client: &Client,
    cfg: &ApisixPushConfig,
    store: &StorageStore,
    app_id: &str,
) -> anyhow::Result<&'static str> {
    const MAX_COMPENSATION_PASSES: usize = 4;
    for pass in 1..=MAX_COMPENSATION_PASSES {
        let desired_before = desired_active_app_route(store, app_id).await?;
        let applied_operation = if let Some((route_id, route)) = &desired_before {
            put_app_route(client, cfg, app_id, route_id, route).await?;
            // The replacement is live before the lossy legacy ID is removed.
            cleanup_legacy_app_route(client, cfg, app_id, route_id).await?;
            "UPSERT"
        } else {
            delete_app_route(client, cfg, app_id).await?;
            cleanup_legacy_app_route(client, cfg, app_id, &route_id_for_app(app_id)).await?;
            "DELETE"
        };
        let desired_after = desired_active_app_route(store, app_id).await?;
        if desired_before == desired_after {
            return Ok(applied_operation);
        }
        metrics::counter!("apisix_convergence_compensation_total").increment(1);
        info!(
            %app_id,
            pass,
            "control-plane intent changed during APISIX I/O; applying compensation"
        );
    }
    anyhow::bail!("control-plane intent for {app_id} kept changing during APISIX convergence")
}

pub async fn delete_route_by_id_for_app(
    client: &Client,
    cfg: &ApisixPushConfig,
    route_id: &str,
    expected_app_id: &str,
) -> anyhow::Result<()> {
    if !route_id.starts_with(MANAGED_ROUTE_PREFIX) {
        anyhow::bail!("refusing to delete unmanaged APISIX route {route_id}");
    }
    let Some(existing) = fetch_route_by_id(client, cfg, route_id).await? else {
        return Ok(());
    };
    match extra_route_cleanup_decision(&existing, expected_app_id, adopt_legacy_routes_enabled()) {
        ExtraRouteCleanupDecision::Delete => {}
        ExtraRouteCleanupDecision::AlreadyReassigned => {
            // This API is used only for an extra-ID tombstone. If a newer app
            // has since claimed a lossy legacy-collision ID, the old app's
            // cleanup is already satisfied. Deterministic current-ID deletion
            // keeps the strict erroring guard.
            info!(%route_id, %expected_app_id, actual_app_id = ?managed_app_id(&existing), "legacy route cleanup skipped because the ID is now owned by another app");
            return Ok(());
        }
        ExtraRouteCleanupDecision::RejectUnverified => {
            anyhow::bail!(
                "refusing to complete APISIX route {route_id} cleanup: ownership is unlabeled or unverified for app {expected_app_id}"
            );
        }
    }
    delete_owned_route(client, cfg, route_id, Some(expected_app_id)).await
}

async fn delete_owned_route(
    client: &Client,
    cfg: &ApisixPushConfig,
    route_id: &str,
    expected_app_id: Option<&str>,
) -> anyhow::Result<()> {
    if !route_id.starts_with(MANAGED_ROUTE_PREFIX) {
        anyhow::bail!("refusing to delete unmanaged APISIX route {route_id}");
    }
    let Some(existing) = fetch_route_by_id(client, cfg, route_id).await? else {
        return Ok(());
    };
    let owned_app_id = managed_app_id(&existing);
    let labeled_owner_matches = match expected_app_id {
        Some(app_id) => owned_app_id == Some(app_id),
        None => owned_app_id.is_some(),
    };
    let verified_legacy = expected_app_id.is_some_and(|app_id| {
        owned_app_id.is_none()
            && adopt_legacy_routes_enabled()
            && is_unlabeled_legacy_route_for_app(&existing, app_id)
    });
    if !labeled_owner_matches && !verified_legacy {
        anyhow::bail!(
            "refusing to delete APISIX route {route_id}: managed ownership labels are missing or mismatched"
        );
    }
    let url = format!("{}/apisix/admin/routes/{}", cfg.admin_base, route_id);
    let response = client
        .delete(url)
        .header("X-API-KEY", &cfg.admin_key)
        .send()
        .await?;
    let status = response.status();
    if status.is_success() || status == reqwest::StatusCode::NOT_FOUND {
        info!(%route_id, %status, "apisix route deleted or already absent");
        return Ok(());
    }
    let body = response.text().await.unwrap_or_default();
    anyhow::bail!("apisix route delete failed: {status} - {body}")
}

fn value_contains(actual: &Value, expected: &Value) -> bool {
    match (actual, expected) {
        (Value::Object(actual), Value::Object(expected)) => expected.iter().all(|(key, value)| {
            actual
                .get(key)
                .is_some_and(|actual_value| value_contains(actual_value, value))
        }),
        (Value::Array(actual), Value::Array(expected)) => {
            actual.len() == expected.len()
                && actual
                    .iter()
                    .zip(expected)
                    .all(|(actual, expected)| value_contains(actual, expected))
        }
        _ => actual == expected,
    }
}

fn parse_managed_route_list(body: Value) -> anyhow::Result<BTreeMap<String, Value>> {
    let list = body
        .get("list")
        .and_then(Value::as_array)
        .ok_or_else(|| anyhow::anyhow!("APISIX route list response is missing list[]"))?;
    let mut routes = BTreeMap::new();
    for item in list {
        let value = item.get("value").unwrap_or(item);
        let route_id = value.get("id").and_then(Value::as_str).or_else(|| {
            item.get("key")
                .and_then(Value::as_str)
                .and_then(|key| key.rsplit('/').next())
        });
        let Some(route_id) = route_id.filter(|id| id.starts_with(MANAGED_ROUTE_PREFIX)) else {
            continue;
        };
        if managed_app_id(value).is_none() {
            continue;
        }
        let mut normalized = value.clone();
        if normalized.get("id").is_none() {
            normalized["id"] = Value::String(route_id.to_string());
        }
        routes.insert(route_id.to_string(), normalized);
    }
    Ok(routes)
}

async fn load_expected_managed_routes(
    store: &StorageStore,
) -> anyhow::Result<BTreeMap<String, Value>> {
    let mut app_ids = RoutesStore::load_all(store)
        .await?
        .into_iter()
        .map(|route| route.app_id)
        .collect::<Vec<_>>();
    app_ids.sort();
    app_ids.dedup();

    let mut expected = BTreeMap::new();
    for app_id in app_ids {
        if let Some((route_id, route)) = desired_app_route(store, &app_id).await? {
            expected.insert(route_id, route);
        }
    }
    Ok(expected)
}

#[derive(Debug, Default)]
struct ActualRouteInventory {
    managed: BTreeMap<String, Value>,
    legacy: Vec<ManagedRouteDeletion>,
}

fn parse_legacy_route_candidates(body: &Value) -> anyhow::Result<Vec<ManagedRouteDeletion>> {
    let list = body
        .get("list")
        .and_then(Value::as_array)
        .ok_or_else(|| anyhow::anyhow!("APISIX route list response is missing list[]"))?;
    let mut legacy = Vec::new();
    for item in list {
        let value = item.get("value").unwrap_or(item);
        if managed_app_id(value).is_some() {
            continue;
        }
        let route_id = value.get("id").and_then(Value::as_str).or_else(|| {
            item.get("key")
                .and_then(Value::as_str)
                .and_then(|key| key.rsplit('/').next())
        });
        let Some(route_id) = route_id.filter(|id| id.starts_with(MANAGED_ROUTE_PREFIX)) else {
            continue;
        };
        let Some(app_id) = legacy_route_app_id(value) else {
            continue;
        };
        if legacy_route_id_for_app(app_id) != route_id
            || !is_unlabeled_legacy_route_for_app(value, app_id)
        {
            continue;
        }
        legacy.push(ManagedRouteDeletion {
            route_id: route_id.to_string(),
            app_id: Some(app_id.to_string()),
        });
    }
    legacy.sort_by(|left, right| left.route_id.cmp(&right.route_id));
    legacy.dedup_by(|left, right| left.route_id == right.route_id);
    Ok(legacy)
}

async fn read_response_body_limited(
    mut response: reqwest::Response,
    max_bytes: usize,
) -> anyhow::Result<Vec<u8>> {
    if response
        .content_length()
        .is_some_and(|length| length > max_bytes as u64)
    {
        anyhow::bail!("APISIX route inventory response exceeded {max_bytes} bytes");
    }

    let initial_capacity = response
        .content_length()
        .and_then(|length| usize::try_from(length).ok())
        .unwrap_or_default();
    let mut body = Vec::with_capacity(initial_capacity);
    while let Some(chunk) = response.chunk().await? {
        let next_length = body
            .len()
            .checked_add(chunk.len())
            .ok_or_else(|| anyhow::anyhow!("APISIX route inventory response size overflow"))?;
        if next_length > max_bytes {
            anyhow::bail!("APISIX route inventory response exceeded {max_bytes} bytes");
        }
        body.extend_from_slice(&chunk);
    }
    Ok(body)
}

async fn fetch_actual_route_inventory(
    client: &Client,
    cfg: &ApisixPushConfig,
) -> anyhow::Result<ActualRouteInventory> {
    const PAGE_SIZE: usize = 500;
    let max_pages = std::env::var("SAG_APISIX_RECONCILE_MAX_PAGES")
        .ok()
        .and_then(|value| value.parse::<usize>().ok())
        .unwrap_or(20)
        .clamp(1, 1_000);
    let max_routes = std::env::var("SAG_APISIX_RECONCILE_MAX_ROUTES")
        .ok()
        .and_then(|value| value.parse::<usize>().ok())
        .unwrap_or(10_000)
        .clamp(PAGE_SIZE, 500_000);
    let mut page = 1_usize;
    let mut total_routes_seen = 0_usize;
    let mut inventory = ActualRouteInventory::default();
    loop {
        if page > max_pages {
            anyhow::bail!("APISIX route inventory exceeded {max_pages} pages");
        }
        let url = format!(
            "{}/apisix/admin/routes?page={page}&page_size={PAGE_SIZE}",
            cfg.admin_base
        );
        let response = client
            .get(url)
            .header("X-API-KEY", &cfg.admin_key)
            .send()
            .await?;
        let status = response.status();
        let response_body =
            read_response_body_limited(response, APISIX_INVENTORY_MAX_RESPONSE_BYTES).await?;
        if !status.is_success() {
            let body = String::from_utf8_lossy(&response_body);
            anyhow::bail!("apisix route list failed: {status} - {body}");
        }
        let body = serde_json::from_slice::<Value>(&response_body)
            .map_err(|error| anyhow::anyhow!("invalid APISIX route list JSON: {error}"))?;
        let item_count = body
            .get("list")
            .and_then(Value::as_array)
            .map(Vec::len)
            .ok_or_else(|| anyhow::anyhow!("APISIX route list response is missing list[]"))?;
        let total = body.get("total").and_then(Value::as_u64);
        total_routes_seen = total_routes_seen
            .checked_add(item_count)
            .ok_or_else(|| anyhow::anyhow!("APISIX route inventory count overflow"))?;
        if total_routes_seen > max_routes || total.is_some_and(|total| total > max_routes as u64) {
            anyhow::bail!("APISIX route inventory exceeded {max_routes} routes");
        }
        if adopt_legacy_routes_enabled() {
            inventory
                .legacy
                .extend(parse_legacy_route_candidates(&body)?);
        }
        inventory.managed.extend(parse_managed_route_list(body)?);

        let consumed = page.saturating_mul(PAGE_SIZE);
        if item_count < PAGE_SIZE || total.is_some_and(|total| consumed as u64 >= total) {
            break;
        }
        page = page
            .checked_add(1)
            .ok_or_else(|| anyhow::anyhow!("APISIX route pagination overflow"))?;
    }
    inventory
        .legacy
        .sort_by(|left, right| left.route_id.cmp(&right.route_id));
    inventory
        .legacy
        .dedup_by(|left, right| left.route_id == right.route_id);
    Ok(inventory)
}

#[derive(Debug, Default, PartialEq, Eq)]
pub struct ManagedRouteDrift {
    pub upsert_app_ids: Vec<String>,
    pub delete_routes: Vec<ManagedRouteDeletion>,
}

#[derive(Debug, PartialEq, Eq)]
pub struct ManagedRouteDeletion {
    pub route_id: String,
    pub app_id: Option<String>,
}

fn diff_managed_routes(
    expected: &BTreeMap<String, Value>,
    actual: &BTreeMap<String, Value>,
) -> ManagedRouteDrift {
    let mut drift = ManagedRouteDrift::default();
    for (route_id, expected_route) in expected {
        if !actual
            .get(route_id)
            .is_some_and(|actual_route| value_contains(actual_route, expected_route))
        {
            if let Some(app_id) = expected_route
                .pointer("/labels/sag-app-id")
                .and_then(Value::as_str)
            {
                drift.upsert_app_ids.push(app_id.to_string());
            }
        }
    }
    drift.delete_routes.extend(
        actual
            .iter()
            .filter(|(route_id, _)| !expected.contains_key(*route_id))
            .map(|(route_id, route)| ManagedRouteDeletion {
                route_id: route_id.clone(),
                app_id: route
                    .pointer("/labels/sag-app-id")
                    .and_then(Value::as_str)
                    .map(str::to_string),
            }),
    );
    drift
}

fn merge_legacy_route_drift(
    expected: &BTreeMap<String, Value>,
    drift: &mut ManagedRouteDrift,
    legacy_routes: Vec<ManagedRouteDeletion>,
) {
    let active_apps = expected
        .values()
        .filter_map(|route| route.pointer("/labels/sag-app-id").and_then(Value::as_str))
        .collect::<std::collections::BTreeSet<_>>();
    for legacy in legacy_routes {
        let Some(app_id) = legacy.app_id.as_deref() else {
            continue;
        };
        if active_apps.contains(app_id) {
            // Convergence creates/adopts the current ID before cleaning the
            // fingerprint-verified legacy ID.
            drift.upsert_app_ids.push(app_id.to_string());
        } else {
            // Deleted apps are absent from `expected`; preserve the app ID
            // recovered from the strict legacy fingerprint for a durable
            // tombstone instead of leaving an invisible ghost route.
            drift.delete_routes.push(legacy);
        }
    }
    drift.upsert_app_ids.sort();
    drift.upsert_app_ids.dedup();
    drift
        .delete_routes
        .sort_by(|left, right| left.route_id.cmp(&right.route_id));
    drift
        .delete_routes
        .dedup_by(|left, right| left.route_id == right.route_id);
}

pub async fn inspect_managed_route_drift(
    client: &Client,
    cfg: &ApisixPushConfig,
    store: &StorageStore,
) -> anyhow::Result<ManagedRouteDrift> {
    let expected = load_expected_managed_routes(store).await?;
    let inventory = fetch_actual_route_inventory(client, cfg).await?;
    let mut drift = diff_managed_routes(&expected, &inventory.managed);
    if adopt_legacy_routes_enabled() {
        merge_legacy_route_drift(&expected, &mut drift, inventory.legacy);
    }
    Ok(drift)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

    use axum::extract::{Path, State};
    use axum::http::StatusCode;
    use axum::routing::get;
    use axum::{Json, Router};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    #[derive(Clone)]
    struct MockRouteState {
        route: Value,
        put_count: Arc<AtomicUsize>,
        delete_count: Arc<AtomicUsize>,
    }

    async fn mock_get_route(
        Path(_route_id): Path<String>,
        State(state): State<MockRouteState>,
    ) -> (StatusCode, Json<Value>) {
        (StatusCode::OK, Json(json!({ "value": state.route })))
    }

    async fn mock_put_route(State(state): State<MockRouteState>) -> StatusCode {
        state.put_count.fetch_add(1, Ordering::SeqCst);
        StatusCode::OK
    }

    async fn mock_delete_route(State(state): State<MockRouteState>) -> StatusCode {
        state.delete_count.fetch_add(1, Ordering::SeqCst);
        StatusCode::OK
    }

    async fn spawn_mock_route_server(
        route: Value,
    ) -> (
        ApisixPushConfig,
        Arc<AtomicUsize>,
        Arc<AtomicUsize>,
        tokio::task::JoinHandle<()>,
    ) {
        let put_count = Arc::new(AtomicUsize::new(0));
        let delete_count = Arc::new(AtomicUsize::new(0));
        let state = MockRouteState {
            route,
            put_count: put_count.clone(),
            delete_count: delete_count.clone(),
        };
        let app = Router::new()
            .route(
                "/apisix/admin/routes/:route_id",
                get(mock_get_route)
                    .put(mock_put_route)
                    .delete(mock_delete_route),
            )
            .with_state(state);
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        (
            ApisixPushConfig {
                admin_base: format!("http://{address}"),
                admin_key: "test-key".into(),
            },
            put_count,
            delete_count,
            server,
        )
    }

    async fn spawn_raw_http_response(
        wire_response: Vec<u8>,
    ) -> (String, tokio::task::JoinHandle<()>) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut request = [0_u8; 1024];
            let request_length = socket.read(&mut request).await.unwrap();
            assert!(request_length > 0);
            socket.write_all(&wire_response).await.unwrap();
            socket.shutdown().await.unwrap();
        });
        (format!("http://{address}/"), server)
    }

    fn fixed_length_response(body: &[u8]) -> Vec<u8> {
        let mut response = format!(
            "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            body.len()
        )
        .into_bytes();
        response.extend_from_slice(body);
        response
    }

    fn chunked_response(body: &[u8]) -> Vec<u8> {
        let mut response =
            b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n".to_vec();
        for chunk in body.chunks(7) {
            response.extend_from_slice(format!("{:x}\r\n", chunk.len()).as_bytes());
            response.extend_from_slice(chunk);
            response.extend_from_slice(b"\r\n");
        }
        response.extend_from_slice(b"0\r\n\r\n");
        response
    }

    fn route(app_id: &str, upstream: &str) -> Value {
        build_app_route(
            app_id,
            &IntranetUpstreamRecord {
                app_id: app_id.into(),
                upstream: upstream.into(),
                scheme: "http".into(),
            },
        )
        .unwrap()
    }

    #[tokio::test]
    async fn inventory_body_limit_rejects_declared_oversize_before_json_buffering() {
        let (url, server) = spawn_raw_http_response(fixed_length_response(&[b'x'; 33])).await;
        let response = Client::builder()
            .no_proxy()
            .build()
            .unwrap()
            .get(url)
            .send()
            .await
            .unwrap();
        assert_eq!(response.content_length(), Some(33));

        let error = read_response_body_limited(response, 32).await.unwrap_err();
        assert!(error.to_string().contains("exceeded 32 bytes"));
        server.await.unwrap();
    }

    #[tokio::test]
    async fn inventory_body_limit_rejects_chunked_oversize_without_content_length() {
        let (url, server) = spawn_raw_http_response(chunked_response(&[b'x'; 33])).await;
        let response = Client::builder()
            .no_proxy()
            .build()
            .unwrap()
            .get(url)
            .send()
            .await
            .unwrap();
        assert_eq!(response.content_length(), None);

        let error = read_response_body_limited(response, 32).await.unwrap_err();
        assert!(error.to_string().contains("exceeded 32 bytes"));
        server.await.unwrap();
    }

    #[tokio::test]
    async fn inventory_body_limit_accepts_an_exact_limit_response() {
        let expected = [b'x'; 32];
        let (url, server) = spawn_raw_http_response(chunked_response(&expected)).await;
        let response = Client::builder()
            .no_proxy()
            .build()
            .unwrap()
            .get(url)
            .send()
            .await
            .unwrap();

        assert_eq!(
            read_response_body_limited(response, 32).await.unwrap(),
            expected
        );
        server.await.unwrap();
    }

    #[test]
    fn list_parser_selects_only_owned_routes_and_accepts_apisix_3_shape() {
        let parsed = parse_managed_route_list(json!({
            "total": 3,
            "list": [
                {
                    "key": "/apisix/routes/sag-route-app-1",
                    "value": {
                        "uri": "/*",
                        "labels": {
                            "sag-managed-by": "control-plane-admin",
                            "sag-app-id": "app-1"
                        }
                    }
                },
                {
                    "key": "/apisix/routes/sag-route-unlabeled",
                    "value": {"id": "sag-route-unlabeled", "uri": "/*"}
                },
                {
                    "key": "/apisix/routes/customer-route",
                    "value": {"id": "customer-route", "uri": "/customer/*"}
                }
            ]
        }))
        .unwrap();

        assert_eq!(parsed.len(), 1);
        assert!(parsed.contains_key("sag-route-app-1"));
        assert_eq!(parsed["sag-route-app-1"]["id"], "sag-route-app-1");
        assert!(!parsed.contains_key("sag-route-unlabeled"));
        assert!(!parsed.contains_key("customer-route"));
    }

    #[test]
    fn drift_detects_missing_changed_and_extra_managed_routes() {
        let expected = BTreeMap::from([
            ("sag-route-app-1".into(), route("app-1", "one:80")),
            ("sag-route-app-2".into(), route("app-2", "two:80")),
        ]);
        let mut app_1_actual = route("app-1", "wrong:80");
        app_1_actual["create_time"] = json!(123);
        let actual = BTreeMap::from([
            ("sag-route-app-1".into(), app_1_actual),
            (
                "sag-route-ghost".into(),
                json!({
                    "id": "sag-route-ghost",
                    "labels": {
                        "sag-managed-by": "control-plane-admin",
                        "sag-app-id": "ghost"
                    }
                }),
            ),
        ]);

        let drift = diff_managed_routes(&expected, &actual);

        assert_eq!(drift.upsert_app_ids, vec!["app-1", "app-2"]);
        assert_eq!(
            drift.delete_routes,
            vec![ManagedRouteDeletion {
                route_id: "sag-route-ghost".into(),
                app_id: Some("ghost".into()),
            }]
        );
    }

    #[test]
    fn apisix_added_metadata_does_not_create_false_drift() {
        let expected_route = route("app-1", "one:80");
        let mut actual_route = expected_route.clone();
        actual_route["create_time"] = json!(123);
        actual_route["update_time"] = json!(456);
        let expected = BTreeMap::from([("sag-route-app-1".into(), expected_route)]);
        let actual = BTreeMap::from([("sag-route-app-1".into(), actual_route)]);

        assert_eq!(
            diff_managed_routes(&expected, &actual),
            ManagedRouteDrift::default()
        );
    }

    #[test]
    fn ownership_requires_labels_and_legacy_adoption_requires_a_strict_fingerprint() {
        let owned = route("app-1", "one:80");
        assert_eq!(managed_app_id(&owned), Some("app-1"));

        let unlabeled = json!({
            "id": "sag-route-app-1",
            "name": "sag-app-1",
            "vars": [["http_x_sag_app_id", "==", "app-1"]]
        });
        assert_eq!(managed_app_id(&unlabeled), None);
        assert!(is_legacy_route_for_app(&unlabeled, "app-1"));
        assert!(!is_legacy_route_for_app(&unlabeled, "app-2"));
        assert_eq!(
            extra_route_cleanup_decision(&unlabeled, "app-1", false),
            ExtraRouteCleanupDecision::RejectUnverified
        );
        assert_eq!(
            extra_route_cleanup_decision(&unlabeled, "app-1", true),
            ExtraRouteCleanupDecision::Delete
        );
        let reassigned = route("app-2", "two:80");
        assert_eq!(
            extra_route_cleanup_decision(&reassigned, "app-1", true),
            ExtraRouteCleanupDecision::AlreadyReassigned
        );
    }

    #[test]
    fn delete_guard_rejects_unmanaged_route_ids_before_network_io() {
        assert!(!route_id_for_app("app/one").contains('/'));
        assert!(route_id_for_app("app/one").starts_with(MANAGED_ROUTE_PREFIX));
        assert_eq!(route_id_for_app("app-001"), "sag-route-app-001");
        assert_ne!(route_id_for_app("app/a"), route_id_for_app("app_a"));
        let long_id = route_id_for_app(&"x".repeat(200));
        assert!(long_id.len() <= APISIX_ID_MAX_LEN);
        assert!(long_id
            .bytes()
            .all(|byte| { byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'.' | b'_') }));
    }

    #[test]
    fn legacy_lossy_collisions_get_distinct_current_ids() {
        assert_eq!(
            legacy_route_id_for_app("app/a"),
            legacy_route_id_for_app("app_a")
        );
        assert_eq!(
            legacy_route_id_for_app("app.a"),
            legacy_route_id_for_app("app_a")
        );
        assert_ne!(route_id_for_app("app/a"), route_id_for_app("app_a"));
        assert_ne!(route_id_for_app("app.a"), route_id_for_app("app_a"));
        assert!(is_legacy_route_for_app(
            &json!({
                "name": "sag-app/a",
                "vars": [["http_x_sag_app_id", "==", "app/a"]]
            }),
            "app/a"
        ));
    }

    #[test]
    fn legacy_inventory_produces_migration_for_active_apps_and_tombstones_for_deleted_apps() {
        let candidates = parse_legacy_route_candidates(&json!({
            "total": 3,
            "list": [
                {
                    "key": "/apisix/routes/sag-route-active_app",
                    "value": {
                        "name": "sag-active/app",
                        "vars": [["http_x_sag_app_id", "==", "active/app"]]
                    }
                },
                {
                    "key": "/apisix/routes/sag-route-deleted_app",
                    "value": {
                        "name": "sag-deleted/app",
                        "vars": [["http_x_sag_app_id", "==", "deleted/app"]]
                    }
                },
                {
                    "key": "/apisix/routes/sag-route-external_app",
                    "value": {
                        "name": "sag-external/app",
                        "labels": {"owner": "customer", "sag-app-id": "external/app"},
                        "vars": [["http_x_sag_app_id", "==", "external/app"]]
                    }
                }
            ]
        }))
        .unwrap();
        assert_eq!(candidates.len(), 2);

        let expected = BTreeMap::from([(
            route_id_for_app("active/app"),
            route("active/app", "one:80"),
        )]);
        let mut drift = ManagedRouteDrift::default();
        merge_legacy_route_drift(&expected, &mut drift, candidates);

        assert_eq!(drift.upsert_app_ids, vec!["active/app"]);
        assert_eq!(
            drift.delete_routes,
            vec![ManagedRouteDeletion {
                route_id: "sag-route-deleted_app".into(),
                app_id: Some("deleted/app".into()),
            }]
        );
    }

    #[tokio::test]
    async fn ownership_guards_do_not_put_or_delete_a_route_owned_by_another_app() {
        let external = json!({
            "id": "sag-route-app-1",
            "name": "customer-route",
            "labels": {
                "sag-managed-by": "control-plane-admin",
                "sag-app-id": "another-app"
            },
            "vars": [["http_x_sag_app_id", "==", "another-app"]]
        });
        let (cfg, put_count, delete_count, server) = spawn_mock_route_server(external).await;
        let client = Client::builder().no_proxy().build().unwrap();
        let desired = route("app-1", "one:80");

        let put_error = put_app_route(&client, &cfg, "app-1", "sag-route-app-1", &desired)
            .await
            .unwrap_err();
        assert!(put_error.to_string().contains("ownership labels"));
        let delete_error = delete_app_route(&client, &cfg, "app-1").await.unwrap_err();
        assert!(delete_error.to_string().contains("ownership labels"));
        delete_route_by_id_for_app(&client, &cfg, "sag-route-app-1", "app-1")
            .await
            .expect("stale cleanup must be a no-op after another app claims a legacy ID");
        assert_eq!(put_count.load(Ordering::SeqCst), 0);
        assert_eq!(delete_count.load(Ordering::SeqCst), 0);

        server.abort();
    }
}
