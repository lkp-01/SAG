//! 可选：�?`intranet_upstreams` + 隧道路由同步�?APISIX Admin API�?
//! 策略见仓库根目录 `APISIX_INTRARENT_STRATEGY.md`：APISIX 只做 L7 流量治理，最终授权仍�?Agent+PDP�?
use reqwest::Client;
use serde_json::json;
use shared_storage::{RoutesStore, StorageStore};
use tracing::{info, warn};

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

fn route_id_for_app(app_id: &str) -> String {
    let safe: String = app_id
        .chars()
        .map(|c| {
            if c.is_alphanumeric() || c == '-' {
                c
            } else {
                '_'
            }
        })
        .collect();
    format!("sag-route-{safe}")
}

/// 为单个 `app_id` 下发 Route（内联 upstream）。
/// 使用 `uri: "/*"` 统一承接真实业务路径；`/api/<name>` 由 proxy-rewrite 兼容映射。
pub async fn sync_app_route(
    client: &Client,
    cfg: &ApisixPushConfig,
    store: &StorageStore,
    app_id: &str,
) -> anyhow::Result<()> {
    let Some(up) = RoutesStore::get_intranet_upstream(store, app_id).await? else {
        info!(app_id, "apisix sync skipped: no intranet upstream");
        return Ok(());
    };

    let nodes_key = up.upstream.trim().to_string();
    let scheme = if up.scheme.eq_ignore_ascii_case("https") {
        "https"
    } else {
        "http"
    };

    let route_id = route_id_for_app(app_id);
    let route = json!({
        "id": route_id,
        "name": format!("sag-{}", app_id),
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
    });

    // NOTE: do NOT bind host by default.
    // In compose/dev, requests may reach APISIX with Host=apisix or Host=127.0.0.1,
    // while tunnel metadata host is `app.internal.com`. Binding host here would cause
    // confusing 404s and break the end-to-end smoke tests.

    let put_route_url = format!("{}/apisix/admin/routes/{}", cfg.admin_base, route_id);
    let rr = client
        .put(&put_route_url)
        .header("X-API-KEY", &cfg.admin_key)
        .json(&route)
        .send()
        .await?;

    let status = rr.status();
    if !status.is_success() {
        let t = rr.text().await.unwrap_or_default();
        anyhow::bail!("apisix route put failed: {} �?{}", status, t);
    }

    info!(app_id, %route_id, "apisix route upserted");
    Ok(())
}

pub async fn try_sync_app(
    client: &Client,
    cfg: Option<&ApisixPushConfig>,
    store: &StorageStore,
    app_id: &str,
) {
    let Some(c) = cfg else { return };
    if let Err(e) = sync_app_route(client, c, store, app_id).await {
        warn!(error = %e, app_id, "apisix sync failed");
    }
}

pub async fn try_sync_all_apps(
    client: &Client,
    cfg: Option<&ApisixPushConfig>,
    store: &StorageStore,
) {
    let Some(c) = cfg else { return };
    match RoutesStore::load_all(store).await {
        Ok(rows) => {
            let mut app_ids: Vec<String> = rows.into_iter().map(|r| r.app_id).collect();
            app_ids.sort();
            app_ids.dedup();
            let total = app_ids.len();
            let mut ok = 0usize;
            let mut failed = 0usize;
            for app_id in app_ids {
                if let Err(e) = sync_app_route(client, c, store, &app_id).await {
                    failed += 1;
                    warn!(error = %e, app_id, "apisix sync-all failed");
                } else {
                    ok += 1;
                }
            }
            info!(total, ok, failed, "apisix reconcile finished");
        }
        Err(e) => warn!(error = %e, "apisix sync-all skipped: load routes failed"),
    }
}
