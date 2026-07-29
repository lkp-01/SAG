use serde::{Deserialize, Serialize};

use crate::{StorageError, StorageStore};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ApiRouteRecord {
    pub id: String,
    pub app_id: String,
    pub method: String,
    pub path: String,
    pub enabled: bool,
    pub description: String,
}

pub struct ApiRoutesStore;

impl ApiRoutesStore {
    pub async fn upsert(store: &StorageStore, record: &ApiRouteRecord) -> Result<(), StorageError> {
        match store {
            StorageStore::Sqlite(sqlite) => {
                let sqlite = sqlite.clone();
                let r = record.clone();
                tokio::task::spawn_blocking(move || {
                    let conn = rusqlite::Connection::open(sqlite.path())?;
                    conn.execute(
                        r#"INSERT INTO api_routes (id, app_id, method, path, enabled, description)
                           VALUES (?1, ?2, ?3, ?4, ?5, ?6)
                           ON CONFLICT(id) DO UPDATE SET
                             app_id=excluded.app_id,
                             method=excluded.method,
                             path=excluded.path,
                             enabled=excluded.enabled,
                             description=excluded.description"#,
                        rusqlite::params![
                            r.id,
                            r.app_id,
                            r.method,
                            r.path,
                            if r.enabled { 1i32 } else { 0i32 },
                            r.description
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
                        r#"INSERT INTO api_routes (id, app_id, method, path, enabled, description)
                           VALUES ($1,$2,$3,$4,$5,$6)
                           ON CONFLICT(id) DO UPDATE SET
                             app_id=excluded.app_id,
                             method=excluded.method,
                             path=excluded.path,
                             enabled=excluded.enabled,
                             description=excluded.description"#,
                        &[
                            &record.id,
                            &record.app_id,
                            &record.method,
                            &record.path,
                            &record.enabled,
                            &record.description,
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
                let sqlite = sqlite.clone();
                let id = id.to_string();
                tokio::task::spawn_blocking(move || {
                    let conn = rusqlite::Connection::open(sqlite.path())?;
                    conn.execute("DELETE FROM api_routes WHERE id=?1", [&id])?;
                    Ok::<_, StorageError>(())
                })
                .await
                .map_err(|e| StorageError::Task(e.to_string()))?
            }
            StorageStore::Postgres(pg) => {
                let client = pg.client().await?;
                client
                    .execute("DELETE FROM api_routes WHERE id=$1", &[&id])
                    .await?;
                Ok(())
            }
        }
    }

    pub async fn list_by_app(
        store: &StorageStore,
        app_id: Option<&str>,
    ) -> Result<Vec<ApiRouteRecord>, StorageError> {
        match store {
            StorageStore::Sqlite(sqlite) => {
                let sqlite = sqlite.clone();
                let app_id = app_id.map(|s| s.to_string());
                tokio::task::spawn_blocking(move || {
                    let conn = rusqlite::Connection::open(sqlite.path())?;
                    let (sql, params): (&str, Vec<rusqlite::types::Value>) = match app_id {
                        Some(a) => (
                            "SELECT id, app_id, method, path, enabled, description FROM api_routes WHERE app_id=?1 ORDER BY app_id, path, method",
                            vec![a.into()],
                        ),
                        None => (
                            "SELECT id, app_id, method, path, enabled, description FROM api_routes ORDER BY app_id, path, method",
                            vec![],
                        ),
                    };
                    let mut stmt = conn.prepare(sql)?;
                    let rows = stmt.query_map(rusqlite::params_from_iter(params), |row| {
                        Ok(ApiRouteRecord {
                            id: row.get(0)?,
                            app_id: row.get(1)?,
                            method: row.get(2)?,
                            path: row.get(3)?,
                            enabled: row.get::<_, i32>(4)? != 0,
                            description: row.get(5)?,
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
                let rows = if let Some(a) = app_id {
                    client
                        .query(
                            "SELECT id, app_id, method, path, enabled, description FROM api_routes WHERE app_id=$1 ORDER BY app_id, path, method",
                            &[&a],
                        )
                        .await?
                } else {
                    client
                        .query(
                            "SELECT id, app_id, method, path, enabled, description FROM api_routes ORDER BY app_id, path, method",
                            &[],
                        )
                        .await?
                };
                Ok(rows
                    .into_iter()
                    .map(|r| ApiRouteRecord {
                        id: r.get(0),
                        app_id: r.get(1),
                        method: r.get(2),
                        path: r.get(3),
                        enabled: r.get(4),
                        description: r.get(5),
                    })
                    .collect())
            }
        }
    }
}
