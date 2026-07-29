use serde::{Deserialize, Serialize};

use crate::{StorageError, StorageStore};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IdentityProviderRecord {
    pub id: String,
    pub kind: String, // "oidc" | "foura"
    pub issuer: String,
    pub client_id: String,
    pub client_secret: String,
    pub scopes: String,
    pub enabled: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GroupRoleMappingRecord {
    pub id: String,
    pub provider_id: String,
    pub external_group: String,
    pub local_roles_csv: String,
    pub enabled: bool,
    pub priority: i64,
}

pub struct IdentityStore;

impl IdentityStore {
    pub async fn upsert_provider(
        store: &StorageStore,
        record: &IdentityProviderRecord,
    ) -> Result<(), StorageError> {
        match store {
            StorageStore::Sqlite(sqlite) => {
                let sqlite = sqlite.clone();
                let r = record.clone();
                tokio::task::spawn_blocking(move || {
                    let conn = rusqlite::Connection::open(sqlite.path())?;
                    conn.execute(
                        r#"INSERT INTO identity_providers (id, kind, issuer, client_id, client_secret, scopes, enabled)
                           VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
                           ON CONFLICT(id) DO UPDATE SET
                             kind=excluded.kind,
                             issuer=excluded.issuer,
                             client_id=excluded.client_id,
                             client_secret=excluded.client_secret,
                             scopes=excluded.scopes,
                             enabled=excluded.enabled"#,
                        rusqlite::params![
                            r.id,
                            r.kind,
                            r.issuer,
                            r.client_id,
                            r.client_secret,
                            r.scopes,
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
                        r#"INSERT INTO identity_providers (id, kind, issuer, client_id, client_secret, scopes, enabled)
                           VALUES ($1,$2,$3,$4,$5,$6,$7)
                           ON CONFLICT(id) DO UPDATE SET
                             kind=excluded.kind,
                             issuer=excluded.issuer,
                             client_id=excluded.client_id,
                             client_secret=excluded.client_secret,
                             scopes=excluded.scopes,
                             enabled=excluded.enabled"#,
                        &[
                            &record.id,
                            &record.kind,
                            &record.issuer,
                            &record.client_id,
                            &record.client_secret,
                            &record.scopes,
                            &record.enabled,
                        ],
                    )
                    .await?;
                Ok(())
            }
        }
    }

    pub async fn list_providers(
        store: &StorageStore,
    ) -> Result<Vec<IdentityProviderRecord>, StorageError> {
        match store {
            StorageStore::Sqlite(sqlite) => {
                let sqlite = sqlite.clone();
                tokio::task::spawn_blocking(move || {
                    let conn = rusqlite::Connection::open(sqlite.path())?;
                    let mut stmt = conn.prepare(
                        "SELECT id, kind, issuer, client_id, client_secret, scopes, enabled FROM identity_providers ORDER BY id",
                    )?;
                    let rows = stmt.query_map([], |row| {
                        Ok(IdentityProviderRecord {
                            id: row.get(0)?,
                            kind: row.get(1)?,
                            issuer: row.get(2)?,
                            client_id: row.get(3)?,
                            client_secret: row.get(4)?,
                            scopes: row.get(5)?,
                            enabled: row.get::<_, i32>(6)? != 0,
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
                let rows = client.query(
                    "SELECT id, kind, issuer, client_id, client_secret, scopes, enabled FROM identity_providers ORDER BY id",
                    &[],
                ).await?;
                Ok(rows
                    .into_iter()
                    .map(|r| IdentityProviderRecord {
                        id: r.get(0),
                        kind: r.get(1),
                        issuer: r.get(2),
                        client_id: r.get(3),
                        client_secret: r.get(4),
                        scopes: r.get(5),
                        enabled: r.get(6),
                    })
                    .collect())
            }
        }
    }

    pub async fn delete_provider(store: &StorageStore, id: &str) -> Result<(), StorageError> {
        match store {
            StorageStore::Sqlite(sqlite) => {
                let sqlite = sqlite.clone();
                let id = id.to_string();
                tokio::task::spawn_blocking(move || {
                    let conn = rusqlite::Connection::open(sqlite.path())?;
                    conn.execute("DELETE FROM identity_providers WHERE id=?1", [&id])?;
                    Ok::<_, StorageError>(())
                })
                .await
                .map_err(|e| StorageError::Task(e.to_string()))?
            }
            StorageStore::Postgres(pg) => {
                let client = pg.client().await?;
                client
                    .execute("DELETE FROM identity_providers WHERE id=$1", &[&id])
                    .await?;
                Ok(())
            }
        }
    }

    pub async fn upsert_mapping(
        store: &StorageStore,
        record: &GroupRoleMappingRecord,
    ) -> Result<(), StorageError> {
        match store {
            StorageStore::Sqlite(sqlite) => {
                let sqlite = sqlite.clone();
                let r = record.clone();
                tokio::task::spawn_blocking(move || {
                    let conn = rusqlite::Connection::open(sqlite.path())?;
                    conn.execute(
                        r#"INSERT INTO group_role_mappings (id, provider_id, external_group, local_roles_csv, enabled, priority)
                           VALUES (?1, ?2, ?3, ?4, ?5, ?6)
                           ON CONFLICT(id) DO UPDATE SET
                             provider_id=excluded.provider_id,
                             external_group=excluded.external_group,
                             local_roles_csv=excluded.local_roles_csv,
                             enabled=excluded.enabled,
                             priority=excluded.priority"#,
                        rusqlite::params![
                            r.id,
                            r.provider_id,
                            r.external_group,
                            r.local_roles_csv,
                            if r.enabled { 1i32 } else { 0i32 },
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
                client
                    .execute(
                        r#"INSERT INTO group_role_mappings (id, provider_id, external_group, local_roles_csv, enabled, priority)
                           VALUES ($1,$2,$3,$4,$5,$6)
                           ON CONFLICT(id) DO UPDATE SET
                             provider_id=excluded.provider_id,
                             external_group=excluded.external_group,
                             local_roles_csv=excluded.local_roles_csv,
                             enabled=excluded.enabled,
                             priority=excluded.priority"#,
                        &[
                            &record.id,
                            &record.provider_id,
                            &record.external_group,
                            &record.local_roles_csv,
                            &record.enabled,
                            &record.priority,
                        ],
                    )
                    .await?;
                Ok(())
            }
        }
    }

    pub async fn list_mappings(
        store: &StorageStore,
        provider_id: Option<&str>,
    ) -> Result<Vec<GroupRoleMappingRecord>, StorageError> {
        match store {
            StorageStore::Sqlite(sqlite) => {
                let sqlite = sqlite.clone();
                let provider_id = provider_id.map(|s| s.to_string());
                tokio::task::spawn_blocking(move || {
                    let conn = rusqlite::Connection::open(sqlite.path())?;
                    let (sql, params): (&str, Vec<rusqlite::types::Value>) = match provider_id {
                        Some(p) => (
                            "SELECT id, provider_id, external_group, local_roles_csv, enabled, priority FROM group_role_mappings WHERE provider_id=?1 ORDER BY priority ASC, id ASC",
                            vec![p.into()],
                        ),
                        None => (
                            "SELECT id, provider_id, external_group, local_roles_csv, enabled, priority FROM group_role_mappings ORDER BY priority ASC, id ASC",
                            vec![],
                        ),
                    };
                    let mut stmt = conn.prepare(sql)?;
                    let rows = stmt.query_map(rusqlite::params_from_iter(params), |row| {
                        Ok(GroupRoleMappingRecord {
                            id: row.get(0)?,
                            provider_id: row.get(1)?,
                            external_group: row.get(2)?,
                            local_roles_csv: row.get(3)?,
                            enabled: row.get::<_, i32>(4)? != 0,
                            priority: row.get(5)?,
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
                let rows = if let Some(p) = provider_id {
                    client.query(
                        "SELECT id, provider_id, external_group, local_roles_csv, enabled, priority FROM group_role_mappings WHERE provider_id=$1 ORDER BY priority ASC, id ASC",
                        &[&p],
                    ).await?
                } else {
                    client.query(
                        "SELECT id, provider_id, external_group, local_roles_csv, enabled, priority FROM group_role_mappings ORDER BY priority ASC, id ASC",
                        &[],
                    ).await?
                };
                Ok(rows
                    .into_iter()
                    .map(|r| GroupRoleMappingRecord {
                        id: r.get(0),
                        provider_id: r.get(1),
                        external_group: r.get(2),
                        local_roles_csv: r.get(3),
                        enabled: r.get(4),
                        priority: r.get(5),
                    })
                    .collect())
            }
        }
    }

    pub async fn delete_mapping(store: &StorageStore, id: &str) -> Result<(), StorageError> {
        match store {
            StorageStore::Sqlite(sqlite) => {
                let sqlite = sqlite.clone();
                let id = id.to_string();
                tokio::task::spawn_blocking(move || {
                    let conn = rusqlite::Connection::open(sqlite.path())?;
                    conn.execute("DELETE FROM group_role_mappings WHERE id=?1", [&id])?;
                    Ok::<_, StorageError>(())
                })
                .await
                .map_err(|e| StorageError::Task(e.to_string()))?
            }
            StorageStore::Postgres(pg) => {
                let client = pg.client().await?;
                client
                    .execute("DELETE FROM group_role_mappings WHERE id=$1", &[&id])
                    .await?;
                Ok(())
            }
        }
    }
}
