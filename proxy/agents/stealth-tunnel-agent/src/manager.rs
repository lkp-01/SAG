use std::collections::HashMap;
use std::error::Error;
use std::sync::atomic::{AtomicBool, AtomicI64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use serde::Deserialize;
use tokio::sync::RwLock;
use tracing::{info, warn};

#[derive(Debug, Clone, Deserialize)]
pub struct TunnelRouteRecord {
    pub host: String,
    pub app_id: String,
    pub connector_endpoint: String,
    pub require_healthy_tunnel: bool,
}

#[derive(Clone)]
#[allow(dead_code)]
pub struct RouteInfo {
    pub host: String,
    pub app_id: String,
    pub connector_endpoint: String,
    pub require_healthy_tunnel: bool,
}

#[derive(Clone, Default)]
pub struct TunnelManager {
    routes_by_app: Arc<RwLock<HashMap<String, RouteInfo>>>,
    initial_sync_succeeded: Arc<AtomicBool>,
    last_sync_ms: Arc<AtomicI64>,
}

impl TunnelManager {
    pub fn new() -> Self {
        Self {
            routes_by_app: Arc::new(RwLock::new(HashMap::new())),
            initial_sync_succeeded: Arc::new(AtomicBool::new(false)),
            last_sync_ms: Arc::new(AtomicI64::new(now_ms())),
        }
    }

    pub async fn replace_routes(&self, routes: Vec<TunnelRouteRecord>) {
        let mut m = self.routes_by_app.write().await;
        m.clear();
        for r in routes {
            m.insert(
                r.app_id.clone(),
                RouteInfo {
                    host: r.host,
                    app_id: r.app_id,
                    connector_endpoint: r.connector_endpoint,
                    require_healthy_tunnel: r.require_healthy_tunnel,
                },
            );
        }
        self.initial_sync_succeeded.store(true, Ordering::Release);
        self.last_sync_ms.store(now_ms(), Ordering::Release);
    }

    pub async fn resolve_route_by_app_id(&self, app_id: &str) -> Option<RouteInfo> {
        let g = self.routes_by_app.read().await;
        g.get(app_id).cloned()
    }

    pub fn initial_sync_succeeded(&self) -> bool {
        self.initial_sync_succeeded.load(Ordering::Acquire)
    }

    pub fn route_sync_age_seconds(&self) -> f64 {
        now_ms().saturating_sub(self.last_sync_ms.load(Ordering::Acquire)) as f64 / 1_000.0
    }
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_millis() as i64)
        .unwrap_or(0)
}

fn format_reqwest_err(e: &reqwest::Error) -> String {
    let mut detail = e.to_string();
    let mut cur = e.source();
    while let Some(c) = cur {
        detail.push_str(" — ");
        detail.push_str(&c.to_string());
        cur = c.source();
    }
    detail
}

pub async fn sync_routes_loop(
    endpoints: Vec<String>,
    manager: TunnelManager,
    interval: Duration,
    sync_token: Option<String>,
) {
    let request_timeout = Duration::from_millis(
        std::env::var("SAG_CONTROL_PLANE_SYNC_TIMEOUT_MS")
            .ok()
            .and_then(|value| value.parse::<u64>().ok())
            .unwrap_or(2_000)
            .max(100),
    );
    let client = reqwest::Client::builder()
        .timeout(request_timeout)
        .build()
        .expect("reqwest client");
    if endpoints.is_empty() {
        warn!("sync routes: no control-plane endpoints configured");
    }
    loop {
        if endpoints.is_empty() {
            tokio::time::sleep(interval).await;
            continue;
        }

        let mut synced = false;
        let mut last_fail: Option<(String, String)> = None;

        for url in &endpoints {
            let mut req = client.get(url);
            if let Some(token) = &sync_token {
                req = req.header("x-sag-agent-token", token);
            }
            match req.send().await {
                Ok(resp) => {
                    if resp.status().is_success() {
                        match resp.json::<Vec<TunnelRouteRecord>>().await {
                            Ok(list) => {
                                let n = list.len();
                                manager.replace_routes(list).await;
                                info!(url = %url, count = n, "sync routes ok");
                                synced = true;
                                break;
                            }
                            Err(e) => {
                                last_fail = Some((url.clone(), e.to_string()));
                                warn!(url = %url, "sync routes: response body is not valid json");
                            }
                        }
                    } else {
                        last_fail = Some((url.clone(), format!("HTTP {}", resp.status())));
                        warn!(url = %url, status = %resp.status(), "sync routes failed");
                    }
                }
                Err(e) => {
                    let detail = format_reqwest_err(&e);
                    last_fail = Some((url.clone(), detail.clone()));
                    warn!(url = %url, detail = %detail, "sync routes http failed");
                }
            }
        }

        if !synced {
            if let Some((url, msg)) = last_fail {
                warn!(tried = ?endpoints, last_url = %url, detail = %msg, "sync routes: all endpoints failed this round");
            }
        }
        metrics::gauge!("agent_route_sync_age_seconds").set(manager.route_sync_age_seconds());

        tokio::time::sleep(interval).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn readiness_requires_a_successful_initial_route_sync_even_for_empty_routes() {
        let manager = TunnelManager::new();
        assert!(!manager.initial_sync_succeeded());

        manager.replace_routes(Vec::new()).await;

        assert!(manager.initial_sync_succeeded());
    }
}
