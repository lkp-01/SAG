//! Optional Redis-backed **stale-while-degraded** cache for policy ALLOW and auth identity.
//! Used when live sag-policy / sag-auth calls time out or fail under load so dataplane does not
//! collapse to hard 403/502 solely due to dependency pressure (see README `SAG_AGENT_DEGRADE_REDIS_URL`).

use std::sync::Arc;
use std::time::Duration;

use redis::AsyncCommands;
use serde::{Deserialize, Serialize};
use tokio::sync::Mutex;
use tracing::{info, warn};

const POLICY_KEY_PREFIX: &str = "sag:agent:stale:policy:v1:";
const AUTH_KEY_PREFIX: &str = "sag:agent:stale:auth:v1:";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StalePolicyPayload {
    pub decision: String,
    pub reason: String,
    pub matched_policy_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct StaleAuthPayload {
    user_id: String,
    roles_csv: String,
}

fn env_u64(key: &str, default: u64) -> u64 {
    std::env::var(key)
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
        .unwrap_or(default)
}

fn env_bool(key: &str, default: bool) -> bool {
    std::env::var(key)
        .ok()
        .map(|v| matches!(v.as_str(), "1" | "true" | "TRUE" | "yes" | "YES"))
        .unwrap_or(default)
}

fn redis_url_from_env() -> Option<String> {
    std::env::var("SAG_AGENT_DEGRADE_REDIS_URL")
        .ok()
        .filter(|s| !s.trim().is_empty())
        .or_else(|| {
            std::env::var("SAG_POLICY_CACHE_REDIS_URL")
                .ok()
                .filter(|s| !s.trim().is_empty())
        })
}

fn fingerprint(s: &str) -> String {
    use sha2::{Digest, Sha256};
    let mut h = Sha256::new();
    h.update(s.as_bytes());
    hex::encode(h.finalize())
}

/// Redis-backed stale cache; `None` inner if URL unset or connect failed.
#[derive(Clone)]
pub struct AgentDegradeRedis {
    conn: Option<Arc<Mutex<redis::aio::ConnectionManager>>>,
    policy_ttl_sec: u64,
    auth_ttl_sec: u64,
    /// When policy returns DENY with a transient-looking reason, try stale ALLOW from Redis.
    pub stale_on_transient_deny: bool,
}

impl AgentDegradeRedis {
    pub async fn connect_from_env() -> Self {
        let Some(url) = redis_url_from_env() else {
            info!("SAG_AGENT_DEGRADE_REDIS_URL unset (and no SAG_POLICY_CACHE_REDIS_URL): stale degrade cache disabled");
            return Self {
                conn: None,
                policy_ttl_sec: env_u64("SAG_AGENT_POLICY_STALE_TTL_SEC", 600).max(30),
                auth_ttl_sec: env_u64("SAG_AGENT_AUTH_STALE_TTL_SEC", 300).max(30),
                stale_on_transient_deny: env_bool("SAG_AGENT_POLICY_STALE_ON_TRANSIENT_DENY", true),
            };
        };
        let policy_ttl_sec = env_u64("SAG_AGENT_POLICY_STALE_TTL_SEC", 600).max(30);
        let auth_ttl_sec = env_u64("SAG_AGENT_AUTH_STALE_TTL_SEC", 300).max(30);
        let stale_on_transient_deny = env_bool("SAG_AGENT_POLICY_STALE_ON_TRANSIENT_DENY", true);
        match redis::Client::open(url.as_str()) {
            Ok(client) => match redis::aio::ConnectionManager::new(client).await {
                Ok(conn) => {
                    info!(
                        %url,
                        policy_ttl_sec,
                        auth_ttl_sec,
                        stale_on_transient_deny,
                        "stealth-tunnel-agent: Redis stale degrade cache enabled"
                    );
                    Self {
                        conn: Some(Arc::new(Mutex::new(conn))),
                        policy_ttl_sec,
                        auth_ttl_sec,
                        stale_on_transient_deny,
                    }
                }
                Err(e) => {
                    warn!(error = %e, "stealth-tunnel-agent: Redis stale cache connect failed; continuing without");
                    Self {
                        conn: None,
                        policy_ttl_sec,
                        auth_ttl_sec,
                        stale_on_transient_deny,
                    }
                }
            },
            Err(e) => {
                warn!(error = %e, "stealth-tunnel-agent: Redis client open failed for stale cache");
                Self {
                    conn: None,
                    policy_ttl_sec,
                    auth_ttl_sec,
                    stale_on_transient_deny,
                }
            }
        }
    }

    pub async fn get_stale_policy_allow(
        &self,
        policy_cache_key: &str,
    ) -> Option<StalePolicyPayload> {
        let conn = self.conn.as_ref()?;
        let key = format!("{}{}", POLICY_KEY_PREFIX, fingerprint(policy_cache_key));
        let mut g = conn.lock().await;
        let fetched: redis::RedisResult<Option<String>> = g.get(&key).await;
        let raw: String = match fetched {
            Ok(Some(s)) => s,
            Ok(None) | Err(_) => return None,
        };
        let v: StalePolicyPayload = serde_json::from_str(&raw).ok()?;
        if v.decision.eq_ignore_ascii_case("ALLOW") {
            metrics::counter!("agent_degrade_redis_policy_stale_hit_total").increment(1);
            Some(v)
        } else {
            None
        }
    }

    pub async fn set_stale_policy_allow(
        &self,
        policy_cache_key: &str,
        payload: &StalePolicyPayload,
    ) {
        let Some(conn) = &self.conn else {
            return;
        };
        if !payload.decision.eq_ignore_ascii_case("ALLOW") {
            return;
        }
        let Ok(json) = serde_json::to_string(payload) else {
            return;
        };
        let key = format!("{}{}", POLICY_KEY_PREFIX, fingerprint(policy_cache_key));
        let ttl = self.policy_ttl_sec.max(30);
        let mut g = conn.lock().await;
        let _: Result<(), _> = g.set_ex(&key, json, ttl).await;
        metrics::counter!("agent_degrade_redis_policy_stale_write_total").increment(1);
    }

    pub async fn get_stale_auth(&self, bearer_token: &str) -> Option<(String, String)> {
        let conn = self.conn.as_ref()?;
        let key = format!("{}{}", AUTH_KEY_PREFIX, fingerprint(bearer_token));
        let mut g = conn.lock().await;
        let fetched: redis::RedisResult<Option<String>> = g.get(&key).await;
        let raw: String = match fetched {
            Ok(Some(s)) => s,
            Ok(None) | Err(_) => return None,
        };
        let v: StaleAuthPayload = serde_json::from_str(&raw).ok()?;
        if v.user_id.is_empty() {
            return None;
        }
        metrics::counter!("agent_degrade_redis_auth_stale_hit_total").increment(1);
        Some((v.user_id, v.roles_csv))
    }

    pub async fn set_stale_auth(&self, bearer_token: &str, user_id: &str, roles_csv: &str) {
        let Some(conn) = &self.conn else {
            return;
        };
        let payload = StaleAuthPayload {
            user_id: user_id.to_string(),
            roles_csv: roles_csv.to_string(),
        };
        let Ok(json) = serde_json::to_string(&payload) else {
            return;
        };
        let key = format!("{}{}", AUTH_KEY_PREFIX, fingerprint(bearer_token));
        let ttl = self.auth_ttl_sec.max(30);
        let mut g = conn.lock().await;
        let _: Result<(), _> = g.set_ex(&key, json, ttl).await;
        metrics::counter!("agent_degrade_redis_auth_stale_write_total").increment(1);
    }

    /// Heuristic: policy service returned DENY but reason looks like overload/transient failure.
    pub fn transient_policy_denial_reason(reason: &str) -> bool {
        let r = reason.to_lowercase();
        r.contains("timeout")
            || r.contains("temporarily unavailable")
            || r.contains("unavailable")
            || r.contains("bulkhead")
            || r.contains("overload")
            || r.contains("resource exhausted")
            || r.contains("deadline")
            || r.contains("503")
            || r.contains("504")
            || r.contains("502")
    }

    /// Optional delayed refresh: best-effort re-call policy HTTP and refresh Redis + caller cache.
    pub fn spawn_policy_refresh_hint(
        http: reqwest::Client,
        policy_url: String,
        timeout_ms: u64,
        payload: serde_json::Value,
        cache_key: String,
        degrade: AgentDegradeRedis,
    ) {
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_secs(3)).await;
            #[derive(serde::Deserialize)]
            struct PolicyEvalResponse {
                decision: String,
                reason: String,
                matched_policy_id: Option<String>,
            }
            let resp_result = tokio::time::timeout(
                Duration::from_millis(timeout_ms.max(500)),
                http.post(&policy_url).json(&payload).send(),
            )
            .await;
            let resp = match resp_result {
                Ok(Ok(r)) => r,
                _ => return,
            };
            let resp = match resp.error_for_status() {
                Ok(r) => r,
                _ => return,
            };
            let Ok(v) = resp.json::<PolicyEvalResponse>().await else {
                return;
            };
            if v.decision.eq_ignore_ascii_case("ALLOW") {
                let snap = StalePolicyPayload {
                    decision: v.decision,
                    reason: v.reason,
                    matched_policy_id: v.matched_policy_id,
                };
                degrade.set_stale_policy_allow(&cache_key, &snap).await;
                metrics::counter!("agent_degrade_redis_policy_background_refresh_ok_total")
                    .increment(1);
            }
        });
    }
}
