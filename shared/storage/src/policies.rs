use serde::{Deserialize, Serialize};

use crate::{StorageError, StorageStore};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "UPPERCASE")]
pub enum PolicyEffect {
    Allow,
    Deny,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PolicyRecord {
    pub id: String,
    pub effect: PolicyEffect,
    pub subjects: Vec<String>,
    #[serde(default)]
    pub app_id: Option<String>,
    #[serde(default)]
    pub path_prefix: Option<String>,
    #[serde(default = "default_priority")]
    pub priority: i32,
}

fn default_priority() -> i32 {
    1000
}

pub struct PoliciesStore;

impl PoliciesStore {
    pub async fn init_schema(store: &StorageStore) -> Result<(), StorageError> {
        match store {
            StorageStore::Sqlite(sqlite) => sqlite.ensure_schema().await,
            StorageStore::Postgres(pg) => {
                let client = pg.client().await?;
                client
                    .batch_execute(
                        r#"
                        CREATE TABLE IF NOT EXISTS policies (
                            id TEXT PRIMARY KEY,
                            effect TEXT NOT NULL,
                            subjects_json TEXT NOT NULL,
                            app_id TEXT,
                            path_prefix TEXT,
                            priority INTEGER NOT NULL DEFAULT 1000
                        );
                        "#,
                    )
                    .await?;
                Ok(())
            }
        }
    }

    pub async fn upsert(store: &StorageStore, record: &PolicyRecord) -> Result<(), StorageError> {
        match store {
            StorageStore::Sqlite(sqlite) => {
                let store = sqlite.clone();
                let r = record.clone();
                tokio::task::spawn_blocking(move || {
                    let conn = rusqlite::Connection::open(store.path())?;
                    let effect = match r.effect {
                        PolicyEffect::Allow => "ALLOW",
                        PolicyEffect::Deny => "DENY",
                    };
                    let subjects = serde_json::to_string(&r.subjects)?;
                    conn.execute(
                        r"INSERT INTO policies (id, effect, subjects_json, app_id, path_prefix, priority)
                          VALUES (?1, ?2, ?3, ?4, ?5, ?6)
                          ON CONFLICT(id) DO UPDATE SET
                            effect=excluded.effect,
                            subjects_json=excluded.subjects_json,
                            app_id=excluded.app_id,
                            path_prefix=excluded.path_prefix,
                            priority=excluded.priority",
                        rusqlite::params![
                            r.id,
                            effect,
                            subjects,
                            r.app_id,
                            r.path_prefix,
                            r.priority
                        ],
                    )?;
                    Ok::<_, StorageError>(())
                })
                .await
                .map_err(|e| StorageError::Task(e.to_string()))?
            }
            StorageStore::Postgres(pg) => {
                let client = pg.client().await?;
                let effect = match record.effect {
                    PolicyEffect::Allow => "ALLOW",
                    PolicyEffect::Deny => "DENY",
                };
                let subjects = serde_json::to_string(&record.subjects)?;
                client
                    .execute(
                        r#"INSERT INTO policies (id, effect, subjects_json, app_id, path_prefix, priority)
                           VALUES ($1, $2, $3, $4, $5, $6)
                           ON CONFLICT(id) DO UPDATE SET
                             effect=excluded.effect,
                             subjects_json=excluded.subjects_json,
                             app_id=excluded.app_id,
                             path_prefix=excluded.path_prefix,
                             priority=excluded.priority"#,
                        &[
                            &record.id,
                            &effect,
                            &subjects,
                            &record.app_id,
                            &record.path_prefix,
                            &record.priority,
                        ],
                    )
                    .await?;
                Ok(())
            }
        }
    }

    pub async fn delete(store: &StorageStore, id: &str) -> Result<(), StorageError> {
        match store {
            StorageStore::Sqlite(sqlite) => {
                let store = sqlite.clone();
                let id = id.to_string();
                tokio::task::spawn_blocking(move || {
                    let conn = rusqlite::Connection::open(store.path())?;
                    conn.execute("DELETE FROM policies WHERE id = ?1", [&id])?;
                    Ok::<_, StorageError>(())
                })
                .await
                .map_err(|e| StorageError::Task(e.to_string()))?
            }
            StorageStore::Postgres(pg) => {
                let client = pg.client().await?;
                client
                    .execute("DELETE FROM policies WHERE id = $1", &[&id])
                    .await?;
                Ok(())
            }
        }
    }

    pub async fn load_all(store: &StorageStore) -> Result<Vec<PolicyRecord>, StorageError> {
        match store {
            StorageStore::Sqlite(sqlite) => {
                let store = sqlite.clone();
                tokio::task::spawn_blocking(move || {
                    let conn = rusqlite::Connection::open(store.path())?;
                    let mut stmt = conn.prepare(
                        "SELECT id, effect, subjects_json, app_id, path_prefix, priority FROM policies",
                    )?;
                    let rows = stmt.query_map([], |row| {
                        let effect_s: String = row.get(1)?;
                        let effect = match effect_s.as_str() {
                            "ALLOW" => PolicyEffect::Allow,
                            "DENY" => PolicyEffect::Deny,
                            _ => PolicyEffect::Deny,
                        };
                        let subjects: Vec<String> =
                            serde_json::from_str(&row.get::<_, String>(2)?).unwrap_or_default();
                        Ok(PolicyRecord {
                            id: row.get(0)?,
                            effect,
                            subjects,
                            app_id: row.get(3)?,
                            path_prefix: row.get(4)?,
                            priority: row.get(5)?,
                        })
                    })?;
                    let mut out = Vec::new();
                    for r in rows {
                        out.push(r?);
                    }
                    Ok::<_, StorageError>(out)
                })
                .await
                .map_err(|e| StorageError::Task(e.to_string()))?
            }
            StorageStore::Postgres(pg) => {
                let client = pg.client().await?;
                let rows = client
                    .query(
                        "SELECT id, effect, subjects_json, app_id, path_prefix, priority FROM policies",
                        &[],
                    )
                    .await?;
                let mut out = Vec::with_capacity(rows.len());
                for row in rows {
                    let effect_s: String = row.get(1);
                    let effect = match effect_s.as_str() {
                        "ALLOW" => PolicyEffect::Allow,
                        _ => PolicyEffect::Deny,
                    };
                    let subjects_json: String = row.get(2);
                    let subjects = serde_json::from_str::<Vec<String>>(&subjects_json)?;
                    out.push(PolicyRecord {
                        id: row.get(0),
                        effect,
                        subjects,
                        app_id: row.get(3),
                        path_prefix: row.get(4),
                        priority: row.get(5),
                    });
                }
                Ok(out)
            }
        }
    }
}
