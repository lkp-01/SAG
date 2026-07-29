use serde::{Deserialize, Serialize};

use crate::{StorageError, StorageStore};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AppRecord {
    pub app_id: String,
    pub display_name: String,
    pub description: String,
    pub enabled: bool,
}

pub struct AppsStore;

impl AppsStore {
    pub async fn upsert(store: &StorageStore, record: &AppRecord) -> Result<(), StorageError> {
        match store {
            StorageStore::Sqlite(sqlite) => {
                let sqlite = sqlite.clone();
                let r = record.clone();
                tokio::task::spawn_blocking(move || {
                    let conn = rusqlite::Connection::open(sqlite.path())?;
                    conn.execute(
                        r#"INSERT INTO apps (app_id, display_name, description, enabled)
                           VALUES (?1, ?2, ?3, ?4)
                           ON CONFLICT(app_id) DO UPDATE SET
                             display_name=excluded.display_name,
                             description=excluded.description,
                             enabled=excluded.enabled"#,
                        rusqlite::params![
                            r.app_id,
                            r.display_name,
                            r.description,
                            if r.enabled { 1i32 } else { 0i32 }
                        ],
                    )?;
                    Ok::<_, StorageError>(())
                })
                .await
                .map_err(|e| StorageError::Task(e.to_string()))?
            }
            StorageStore::Postgres(pg) => {
                let client = pg.client().await?;
                client
                    .execute(
                        r#"INSERT INTO apps (app_id, display_name, description, enabled)
                           VALUES ($1,$2,$3,$4)
                           ON CONFLICT(app_id) DO UPDATE SET
                             display_name=excluded.display_name,
                             description=excluded.description,
                             enabled=excluded.enabled"#,
                        &[
                            &record.app_id,
                            &record.display_name,
                            &record.description,
                            &record.enabled,
                        ],
                    )
                    .await?;
                Ok(())
            }
        }
    }

    pub async fn delete(store: &StorageStore, app_id: &str) -> Result<(), StorageError> {
        match store {
            StorageStore::Sqlite(sqlite) => {
                let sqlite = sqlite.clone();
                let app_id = app_id.to_string();
                tokio::task::spawn_blocking(move || {
                    let conn = rusqlite::Connection::open(sqlite.path())?;
                    conn.execute("DELETE FROM apps WHERE app_id=?1", [&app_id])?;
                    Ok::<_, StorageError>(())
                })
                .await
                .map_err(|e| StorageError::Task(e.to_string()))?
            }
            StorageStore::Postgres(pg) => {
                let client = pg.client().await?;
                client
                    .execute("DELETE FROM apps WHERE app_id=$1", &[&app_id])
                    .await?;
                Ok(())
            }
        }
    }

    pub async fn load_all(store: &StorageStore) -> Result<Vec<AppRecord>, StorageError> {
        match store {
            StorageStore::Sqlite(sqlite) => {
                let sqlite = sqlite.clone();
                tokio::task::spawn_blocking(move || {
                    let conn = rusqlite::Connection::open(sqlite.path())?;
                    let mut stmt = conn.prepare("SELECT app_id, display_name, description, enabled FROM apps ORDER BY app_id")?;
                    let rows = stmt.query_map([], |row| {
                        Ok(AppRecord {
                            app_id: row.get(0)?,
                            display_name: row.get(1)?,
                            description: row.get(2)?,
                            enabled: row.get::<_, i32>(3)? != 0,
                        })
                    })?;
                    let mut out = Vec::new();
                    for r in rows { out.push(r?); }
                    Ok::<_, StorageError>(out)
                })
                .await
                .map_err(|e| StorageError::Task(e.to_string()))?
            }
            StorageStore::Postgres(pg) => {
                let client = pg.client().await?;
                let rows = client.query("SELECT app_id, display_name, description, enabled FROM apps ORDER BY app_id", &[]).await?;
                Ok(rows
                    .into_iter()
                    .map(|r| AppRecord {
                        app_id: r.get(0),
                        display_name: r.get(1),
                        description: r.get(2),
                        enabled: r.get(3),
                    })
                    .collect())
            }
        }
    }
}
