use serde::{Deserialize, Serialize};

use crate::{StorageError, StorageStore};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UserRecord {
    pub id: String,
    pub username: String,
    pub password_hash: String,
    pub roles: Vec<String>,
    pub display_name: Option<String>,
    pub title: Option<String>,
    #[serde(default = "default_enabled")]
    pub enabled: bool,
    #[serde(default = "default_auth_version")]
    pub auth_version: i64,
    #[serde(default)]
    pub updated_at_ms: i64,
}

fn default_enabled() -> bool {
    true
}

fn default_auth_version() -> i64 {
    1
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_millis() as i64)
        .unwrap_or(0)
}

pub struct UsersStore;

impl UsersStore {
    pub async fn upsert(store: &StorageStore, record: &UserRecord) -> Result<(), StorageError> {
        match store {
            StorageStore::Sqlite(sqlite) => {
                let store = sqlite.clone();
                let r = record.clone();
                tokio::task::spawn_blocking(move || {
                    let conn = rusqlite::Connection::open(store.path())?;
                    let roles = serde_json::to_string(&r.roles)?;
                    let updated_at_ms = now_ms().max(r.updated_at_ms);
                    conn.execute(
                        r#"INSERT INTO users (id, username, password_hash, roles_json, display_name, title, enabled, auth_version, updated_at_ms)
                           VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)
                           ON CONFLICT(username) DO UPDATE SET
                             id=excluded.id,
                             password_hash=excluded.password_hash,
                             roles_json=excluded.roles_json,
                             display_name=excluded.display_name,
                             title=excluded.title,
                             enabled=excluded.enabled,
                             auth_version=users.auth_version + 1,
                             updated_at_ms=excluded.updated_at_ms"#,
                        rusqlite::params![
                            r.id,
                            r.username,
                            r.password_hash,
                            roles,
                            r.display_name,
                            r.title,
                            if r.enabled { 1 } else { 0 },
                            r.auth_version.max(1),
                            updated_at_ms,
                        ],
                    )?;
                    Ok::<_, StorageError>(())
                })
                .await
                .map_err(|e| StorageError::Task(e.to_string()))?
            }
            StorageStore::Postgres(pg) => {
                let client = pg.client().await?;
                let roles = serde_json::to_string(&record.roles)?;
                let updated_at_ms = now_ms().max(record.updated_at_ms);
                client
                    .execute(
                        r#"INSERT INTO users (id, username, password_hash, roles_json, display_name, title, enabled, auth_version, updated_at_ms)
                           VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9)
                           ON CONFLICT(username) DO UPDATE SET
                             id=excluded.id,
                             password_hash=excluded.password_hash,
                             roles_json=excluded.roles_json,
                             display_name=excluded.display_name,
                             title=excluded.title,
                             enabled=excluded.enabled,
                             auth_version=users.auth_version + 1,
                             updated_at_ms=excluded.updated_at_ms"#,
                        &[
                            &record.id,
                            &record.username,
                            &record.password_hash,
                            &roles,
                            &record.display_name,
                            &record.title,
                            &record.enabled,
                            &record.auth_version.max(1),
                            &updated_at_ms,
                        ],
                    )
                    .await?;
                Ok(())
            }
        }
    }

    pub async fn delete_by_username(
        store: &StorageStore,
        username: &str,
    ) -> Result<(), StorageError> {
        match store {
            StorageStore::Sqlite(sqlite) => {
                let store = sqlite.clone();
                let username = username.to_string();
                tokio::task::spawn_blocking(move || {
                    let conn = rusqlite::Connection::open(store.path())?;
                    conn.execute("DELETE FROM users WHERE username = ?1", [&username])?;
                    Ok::<_, StorageError>(())
                })
                .await
                .map_err(|e| StorageError::Task(e.to_string()))?
            }
            StorageStore::Postgres(pg) => {
                let client = pg.client().await?;
                client
                    .execute("DELETE FROM users WHERE username = $1", &[&username])
                    .await?;
                Ok(())
            }
        }
    }

    pub async fn load_all(store: &StorageStore) -> Result<Vec<UserRecord>, StorageError> {
        match store {
            StorageStore::Sqlite(sqlite) => {
                let store = sqlite.clone();
                tokio::task::spawn_blocking(move || {
                    let conn = rusqlite::Connection::open(store.path())?;
                    let mut stmt = conn.prepare(
                        "SELECT id, username, password_hash, roles_json, display_name, title, enabled, auth_version, updated_at_ms FROM users",
                    )?;
                    let rows = stmt.query_map([], |row| {
                        let roles: Vec<String> =
                            serde_json::from_str(&row.get::<_, String>(3)?).unwrap_or_default();
                        let enabled_int: i64 = row.get(6)?;
                        Ok(UserRecord {
                            id: row.get(0)?,
                            username: row.get(1)?,
                            password_hash: row.get(2)?,
                            roles,
                            display_name: row.get(4)?,
                            title: row.get(5)?,
                            enabled: enabled_int != 0,
                            auth_version: row.get(7)?,
                            updated_at_ms: row.get(8)?,
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
                        "SELECT id, username, password_hash, roles_json, display_name, title, enabled, auth_version, updated_at_ms FROM users",
                        &[],
                    )
                    .await?;
                let mut out = Vec::with_capacity(rows.len());
                for row in rows {
                    let roles_json: String = row.get(3);
                    let roles = serde_json::from_str::<Vec<String>>(&roles_json)?;
                    out.push(UserRecord {
                        id: row.get(0),
                        username: row.get(1),
                        password_hash: row.get(2),
                        roles,
                        display_name: row.get(4),
                        title: row.get(5),
                        enabled: row.get(6),
                        auth_version: row.get(7),
                        updated_at_ms: row.get(8),
                    });
                }
                Ok(out)
            }
        }
    }

    pub async fn load_by_id(
        store: &StorageStore,
        id: &str,
    ) -> Result<Option<UserRecord>, StorageError> {
        Self::load_one(store, "id", id).await
    }

    pub async fn load_by_username(
        store: &StorageStore,
        username: &str,
    ) -> Result<Option<UserRecord>, StorageError> {
        Self::load_one(store, "username", username).await
    }

    async fn load_one(
        store: &StorageStore,
        column: &'static str,
        value: &str,
    ) -> Result<Option<UserRecord>, StorageError> {
        debug_assert!(matches!(column, "id" | "username"));
        match store {
            StorageStore::Sqlite(sqlite) => {
                let store = sqlite.clone();
                let value = value.to_string();
                tokio::task::spawn_blocking(move || {
                    use rusqlite::OptionalExtension;
                    let conn = rusqlite::Connection::open(store.path())?;
                    let sql = format!(
                        "SELECT id, username, password_hash, roles_json, display_name, title, enabled, auth_version, updated_at_ms FROM users WHERE {column} = ?1"
                    );
                    conn.query_row(&sql, [&value], |row| {
                        let roles_json: String = row.get(3)?;
                        Ok(UserRecord {
                            id: row.get(0)?,
                            username: row.get(1)?,
                            password_hash: row.get(2)?,
                            roles: serde_json::from_str(&roles_json).unwrap_or_default(),
                            display_name: row.get(4)?,
                            title: row.get(5)?,
                            enabled: row.get::<_, i64>(6)? != 0,
                            auth_version: row.get(7)?,
                            updated_at_ms: row.get(8)?,
                        })
                    })
                    .optional()
                    .map_err(StorageError::from)
                })
                .await
                .map_err(|error| StorageError::Task(error.to_string()))?
            }
            StorageStore::Postgres(postgres) => {
                let client = postgres.client().await?;
                let sql = format!(
                    "SELECT id, username, password_hash, roles_json, display_name, title, enabled, auth_version, updated_at_ms FROM users WHERE {column} = $1"
                );
                let row = client.query_opt(&sql, &[&value]).await?;
                row.map(|row| {
                    let roles_json: String = row.get(3);
                    Ok(UserRecord {
                        id: row.get(0),
                        username: row.get(1),
                        password_hash: row.get(2),
                        roles: serde_json::from_str(&roles_json)?,
                        display_name: row.get(4),
                        title: row.get(5),
                        enabled: row.get(6),
                        auth_version: row.get(7),
                        updated_at_ms: row.get(8),
                    })
                })
                .transpose()
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{ensure_store_schema, SqliteStore};

    #[tokio::test]
    async fn authorization_changes_increment_a_monotonic_user_version() {
        let path =
            std::env::temp_dir().join(format!("sag-user-version-{}.db", uuid::Uuid::new_v4()));
        let store = StorageStore::Sqlite(SqliteStore::new(path.to_string_lossy().to_string()));
        ensure_store_schema(&store).await.unwrap();
        let mut user = UserRecord {
            id: "u-alice".into(),
            username: "alice".into(),
            password_hash: "hash-1".into(),
            roles: vec!["user".into()],
            display_name: None,
            title: None,
            enabled: true,
            auth_version: 1,
            updated_at_ms: 1,
        };
        UsersStore::upsert(&store, &user).await.unwrap();
        let first = UsersStore::load_by_id(&store, "u-alice")
            .await
            .unwrap()
            .unwrap();
        user.roles = vec!["ops".into()];
        UsersStore::upsert(&store, &user).await.unwrap();
        let second = UsersStore::load_by_id(&store, "u-alice")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(second.auth_version, first.auth_version + 1);
        assert!(second.updated_at_ms >= first.updated_at_ms);
        let _ = std::fs::remove_file(path);
    }
}
