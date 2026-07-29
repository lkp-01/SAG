use std::time::Duration;

use futures::StreamExt;
use moka::future::Cache;
use shared_storage::{StorageError, StorageStore, UserRecord, UsersStore};
use tracing::warn;

const INVALIDATION_CHANNEL: &str = "sag:auth:user-invalidation:v1";

#[derive(Clone)]
struct CachedUser {
    record: UserRecord,
    fetched_at_ms: i64,
}

#[derive(Clone)]
pub struct UserDirectory {
    store: StorageStore,
    cache: Cache<String, CachedUser>,
    invalidation_redis_url: Option<String>,
}

impl UserDirectory {
    pub fn from_env(store: StorageStore) -> Self {
        let ttl = Duration::from_secs(
            std::env::var("SAG_AUTH_VERSION_CACHE_TTL_SEC")
                .ok()
                .and_then(|value| value.parse::<u64>().ok())
                .unwrap_or(5)
                .clamp(1, 30),
        );
        let capacity = std::env::var("SAG_AUTH_VERSION_CACHE_MAX_CAPACITY")
            .ok()
            .and_then(|value| value.parse::<u64>().ok())
            .unwrap_or(100_000)
            .max(1);
        let invalidation_redis_url = std::env::var("SAG_AUTH_INVALIDATION_REDIS_URL")
            .ok()
            .filter(|value| !value.trim().is_empty())
            .or_else(|| {
                std::env::var("SAG_SESSION_REDIS_URL")
                    .ok()
                    .filter(|value| !value.trim().is_empty())
            });
        let directory = Self::new(store, ttl, capacity, invalidation_redis_url);
        directory.spawn_invalidation_listener();
        directory
    }

    pub(crate) fn new(
        store: StorageStore,
        ttl: Duration,
        capacity: u64,
        invalidation_redis_url: Option<String>,
    ) -> Self {
        Self {
            store,
            cache: Cache::builder()
                .time_to_live(ttl)
                .max_capacity(capacity)
                .build(),
            invalidation_redis_url,
        }
    }

    pub async fn current_by_id(&self, id: &str) -> Result<Option<UserRecord>, StorageError> {
        if let Some(cached) = self.cache.get(id).await {
            let staleness = now_ms().saturating_sub(cached.fetched_at_ms) as f64 / 1_000.0;
            metrics::gauge!("auth_cache_staleness_seconds").set(staleness);
            metrics::counter!("auth_version_cache_total", "result" => "hit").increment(1);
            return Ok(Some(cached.record));
        }
        metrics::counter!("auth_version_cache_total", "result" => "miss").increment(1);
        let record = UsersStore::load_by_id(&self.store, id).await?;
        if let Some(record) = record.as_ref() {
            self.cache
                .insert(
                    id.to_string(),
                    CachedUser {
                        record: record.clone(),
                        fetched_at_ms: now_ms(),
                    },
                )
                .await;
        }
        metrics::gauge!("auth_cache_staleness_seconds").set(0.0);
        Ok(record)
    }

    pub async fn load_login_user(
        &self,
        username: &str,
    ) -> Result<Option<UserRecord>, StorageError> {
        let record = UsersStore::load_by_username(&self.store, username).await?;
        if let Some(record) = record.as_ref() {
            self.cache
                .insert(
                    record.id.clone(),
                    CachedUser {
                        record: record.clone(),
                        fetched_at_ms: now_ms(),
                    },
                )
                .await;
        }
        Ok(record)
    }

    pub async fn invalidate(&self, user_id: &str) {
        self.cache.invalidate(user_id).await;
    }

    pub async fn publish_invalidation(&self, user_id: &str) {
        self.invalidate(user_id).await;
        let Some(url) = self.invalidation_redis_url.clone() else {
            return;
        };
        let user_id = user_id.to_string();
        tokio::spawn(async move {
            let result = async {
                let client = redis::Client::open(url)?;
                let mut connection = client.get_multiplexed_async_connection().await?;
                redis::cmd("PUBLISH")
                    .arg(INVALIDATION_CHANNEL)
                    .arg(user_id)
                    .query_async::<i64>(&mut connection)
                    .await?;
                Ok::<(), redis::RedisError>(())
            }
            .await;
            if let Err(error) = result {
                metrics::counter!("auth_invalidation_failed_total", "stage" => "publish")
                    .increment(1);
                warn!(
                    ?error,
                    "auth invalidation publish failed; cache TTL remains the safety bound"
                );
            }
        });
    }

    fn spawn_invalidation_listener(&self) {
        let Some(url) = self.invalidation_redis_url.clone() else {
            return;
        };
        let cache = self.cache.clone();
        tokio::spawn(async move {
            loop {
                let result = async {
                    let client = redis::Client::open(url.as_str())?;
                    let mut pubsub = client.get_async_pubsub().await?;
                    pubsub.subscribe(INVALIDATION_CHANNEL).await?;
                    let mut messages = pubsub.on_message();
                    while let Some(message) = messages.next().await {
                        let user_id: String = message.get_payload()?;
                        cache.invalidate(&user_id).await;
                        metrics::counter!("auth_invalidation_received_total").increment(1);
                    }
                    Ok::<(), redis::RedisError>(())
                }
                .await;
                if let Err(error) = result {
                    metrics::counter!("auth_invalidation_failed_total", "stage" => "subscribe")
                        .increment(1);
                    warn!(?error, "auth invalidation subscriber disconnected");
                }
                tokio::time::sleep(Duration::from_secs(1)).await;
            }
        });
    }
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_millis() as i64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use shared_storage::{ensure_store_schema, SqliteStore};

    fn user(roles: &[&str]) -> UserRecord {
        UserRecord {
            id: "u-alice".into(),
            username: "alice".into(),
            password_hash: "hash".into(),
            roles: roles.iter().map(|role| role.to_string()).collect(),
            display_name: None,
            title: None,
            enabled: true,
            auth_version: 1,
            updated_at_ms: 0,
        }
    }

    #[tokio::test]
    async fn independent_instance_caches_converge_by_invalidation_or_ttl() {
        let path =
            std::env::temp_dir().join(format!("sag-auth-directory-{}.db", uuid::Uuid::new_v4()));
        let store = StorageStore::Sqlite(SqliteStore::new(path.to_string_lossy().to_string()));
        ensure_store_schema(&store).await.unwrap();
        UsersStore::upsert(&store, &user(&["user"])).await.unwrap();
        let instance_a = UserDirectory::new(store.clone(), Duration::from_millis(25), 16, None);
        let instance_b = UserDirectory::new(store.clone(), Duration::from_secs(30), 16, None);
        let old_a = instance_a.current_by_id("u-alice").await.unwrap().unwrap();
        let old_b = instance_b.current_by_id("u-alice").await.unwrap().unwrap();

        UsersStore::upsert(&store, &user(&["ops"])).await.unwrap();
        instance_b.invalidate("u-alice").await;
        let new_b = instance_b.current_by_id("u-alice").await.unwrap().unwrap();
        assert_eq!(new_b.auth_version, old_b.auth_version + 1);
        assert_eq!(new_b.roles, vec!["ops"]);

        tokio::time::sleep(Duration::from_millis(40)).await;
        let new_a = instance_a.current_by_id("u-alice").await.unwrap().unwrap();
        assert_eq!(new_a.auth_version, old_a.auth_version + 1);
        let _ = std::fs::remove_file(path);
    }
}
