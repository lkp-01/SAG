use rusqlite::OptionalExtension;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::{StorageError, StorageStore};

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum IdempotencyState {
    Claimed,
    Dispatched,
    Completed,
    Indeterminate,
    CompletedByOperator,
    ReleasedByOperator,
}

impl IdempotencyState {
    fn from_database(value: &str) -> Result<Self, StorageError> {
        match value {
            "claimed" => Ok(Self::Claimed),
            "dispatched" => Ok(Self::Dispatched),
            "completed" => Ok(Self::Completed),
            // Old binaries used `pending` for a request whose dispatch outcome
            // was not durable. Treat it conservatively; never make it retryable.
            "pending" | "indeterminate" => Ok(Self::Indeterminate),
            "completed_by_operator" => Ok(Self::CompletedByOperator),
            "released_by_operator" => Ok(Self::ReleasedByOperator),
            other => Err(StorageError::Invariant(format!(
                "unknown idempotency state {other}"
            ))),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct IdempotencyRecord {
    pub scope_key: String,
    pub request_hash: String,
    pub state: IdempotencyState,
    pub state_version: i64,
    pub owner_attempt_id: String,
    pub status_code: u32,
    pub headers_json: String,
    pub body: Vec<u8>,
    pub created_at_ms: i64,
    pub updated_at_ms: i64,
    pub expires_at_ms: i64,
    pub dispatched_at_ms: Option<i64>,
    pub completed_at_ms: Option<i64>,
    pub reconciled_by: Option<String>,
    pub reconcile_reason: Option<String>,
    pub result_hash: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IdempotencyClaim {
    Claimed { state_version: i64 },
    Pending,
    Completed(Box<IdempotencyRecord>),
    Conflict,
}

pub struct IdempotencyStore;

impl IdempotencyStore {
    /// Atomically reserves a mutating operation. Non-terminal records are never
    /// stolen, including legacy `pending` rows created during a rolling upgrade.
    pub async fn claim(
        store: &StorageStore,
        scope_key: &str,
        request_hash: &str,
        owner_attempt_id: &str,
        now_ms: i64,
        expires_at_ms: i64,
    ) -> Result<IdempotencyClaim, StorageError> {
        match store {
            StorageStore::Sqlite(sqlite) => {
                let sqlite = sqlite.clone();
                let scope_key = scope_key.to_string();
                let request_hash = request_hash.to_string();
                let owner_attempt_id = owner_attempt_id.to_string();
                tokio::task::spawn_blocking(move || {
                    let mut conn = rusqlite::Connection::open(sqlite.path())?;
                    let tx = conn.transaction_with_behavior(
                        rusqlite::TransactionBehavior::Immediate,
                    )?;
                    tx.execute(
                        "DELETE FROM idempotency_records WHERE state IN ('completed', 'completed_by_operator') AND expires_at_ms <= ?1",
                        [now_ms],
                    )?;
                    let inserted = tx.execute(
                        r#"INSERT OR IGNORE INTO idempotency_records
                           (scope_key, request_hash, state, owner_attempt_id, status_code,
                            headers_json, body, created_at_ms, updated_at_ms, expires_at_ms,
                            state_version)
                           VALUES (?1, ?2, 'claimed', ?3, 0, '{}', X'', ?4, ?4, ?5, 1)"#,
                        rusqlite::params![
                            scope_key,
                            request_hash,
                            owner_attempt_id,
                            now_ms,
                            expires_at_ms
                        ],
                    )?;
                    if inserted == 1 {
                        tx.commit()?;
                        return Ok(IdempotencyClaim::Claimed { state_version: 1 });
                    }
                    let record = query_sqlite_record(&tx, &scope_key)?
                        .ok_or_else(|| StorageError::Invariant(
                            "idempotency row disappeared during claim".into(),
                        ))?;
                    tx.commit()?;
                    Ok(classify_existing(&request_hash, record))
                })
                .await
                .map_err(|error| StorageError::Task(error.to_string()))?
            }
            StorageStore::Postgres(pg) => {
                let client = pg.client().await?;
                client
                    .execute(
                        "DELETE FROM idempotency_records WHERE state IN ('completed', 'completed_by_operator') AND expires_at_ms <= $1",
                        &[&now_ms],
                    )
                    .await?;
                let empty_body = Vec::<u8>::new();
                let inserted = client
                    .query_opt(
                        r#"INSERT INTO idempotency_records
                           (scope_key, request_hash, state, owner_attempt_id, status_code,
                            headers_json, body, created_at_ms, updated_at_ms, expires_at_ms,
                            state_version)
                           VALUES ($1, $2, 'claimed', $3, 0, '{}', $4, $5, $5, $6, 1)
                           ON CONFLICT (scope_key) DO NOTHING
                           RETURNING state_version"#,
                        &[
                            &scope_key,
                            &request_hash,
                            &owner_attempt_id,
                            &empty_body,
                            &now_ms,
                            &expires_at_ms,
                        ],
                    )
                    .await?;
                if let Some(row) = inserted {
                    return Ok(IdempotencyClaim::Claimed {
                        state_version: row.get(0),
                    });
                }
                let record = query_postgres_record(&client, scope_key)
                    .await?
                    .ok_or_else(|| {
                        StorageError::Invariant("idempotency row disappeared during claim".into())
                    })?;
                Ok(classify_existing(request_hash, record))
            }
        }
    }

    pub async fn get(
        store: &StorageStore,
        scope_key: &str,
    ) -> Result<Option<IdempotencyRecord>, StorageError> {
        match store {
            StorageStore::Sqlite(sqlite) => {
                let sqlite = sqlite.clone();
                let scope_key = scope_key.to_string();
                tokio::task::spawn_blocking(move || {
                    let conn = rusqlite::Connection::open(sqlite.path())?;
                    query_sqlite_record(&conn, &scope_key)
                })
                .await
                .map_err(|error| StorageError::Task(error.to_string()))?
            }
            StorageStore::Postgres(pg) => {
                let client = pg.client().await?;
                query_postgres_record(&client, scope_key).await
            }
        }
    }

    pub async fn list_indeterminate(
        store: &StorageStore,
        updated_before_ms: i64,
        limit: usize,
    ) -> Result<Vec<IdempotencyRecord>, StorageError> {
        let limit = limit.clamp(1, 1_000) as i64;
        match store {
            StorageStore::Sqlite(sqlite) => {
                let sqlite = sqlite.clone();
                tokio::task::spawn_blocking(move || {
                    let conn = rusqlite::Connection::open(sqlite.path())?;
                    let mut statement = conn.prepare(&format!(
                        "{SELECT_COLUMNS} WHERE state IN ('pending', 'indeterminate') AND updated_at_ms <= ?1 ORDER BY updated_at_ms ASC LIMIT ?2"
                    ))?;
                    let rows = statement.query_map(
                        rusqlite::params![updated_before_ms, limit],
                        map_sqlite_record,
                    )?;
                    rows.collect::<Result<Vec<_>, _>>().map_err(StorageError::from)
                })
                .await
                .map_err(|error| StorageError::Task(error.to_string()))?
            }
            StorageStore::Postgres(pg) => {
                let client = pg.client().await?;
                let rows = client
                    .query(
                        &format!(
                            "{SELECT_COLUMNS} WHERE state IN ('pending', 'indeterminate') AND updated_at_ms <= $1 ORDER BY updated_at_ms ASC LIMIT $2"
                        ),
                        &[&updated_before_ms, &limit],
                    )
                    .await?;
                rows.iter().map(map_postgres_record).collect()
            }
        }
    }

    pub async fn mark_dispatched(
        store: &StorageStore,
        scope_key: &str,
        request_hash: &str,
        owner_attempt_id: &str,
        expected_version: i64,
        now_ms: i64,
    ) -> Result<Option<i64>, StorageError> {
        owner_transition(
            store,
            scope_key,
            request_hash,
            owner_attempt_id,
            expected_version,
            "claimed",
            "dispatched",
            now_ms,
        )
        .await
    }

    pub async fn mark_indeterminate(
        store: &StorageStore,
        scope_key: &str,
        request_hash: &str,
        owner_attempt_id: &str,
        expected_version: i64,
        now_ms: i64,
    ) -> Result<Option<i64>, StorageError> {
        owner_transition(
            store,
            scope_key,
            request_hash,
            owner_attempt_id,
            expected_version,
            "dispatched",
            "indeterminate",
            now_ms,
        )
        .await
    }

    #[allow(clippy::too_many_arguments)]
    pub async fn complete(
        store: &StorageStore,
        scope_key: &str,
        request_hash: &str,
        owner_attempt_id: &str,
        expected_version: i64,
        status_code: u32,
        headers_json: &str,
        body: &[u8],
        now_ms: i64,
    ) -> Result<bool, StorageError> {
        let result_hash = result_hash(status_code, headers_json, body);
        match store {
            StorageStore::Sqlite(sqlite) => {
                let sqlite = sqlite.clone();
                let scope_key = scope_key.to_string();
                let request_hash = request_hash.to_string();
                let owner_attempt_id = owner_attempt_id.to_string();
                let headers_json = headers_json.to_string();
                let body = body.to_vec();
                tokio::task::spawn_blocking(move || {
                    let conn = rusqlite::Connection::open(sqlite.path())?;
                    let updated = conn.execute(
                        r#"UPDATE idempotency_records
                           SET state = 'completed', state_version = state_version + 1,
                               status_code = ?5, headers_json = ?6, body = ?7,
                               updated_at_ms = ?8, completed_at_ms = ?8, result_hash = ?9
                           WHERE scope_key = ?1 AND request_hash = ?2
                             AND owner_attempt_id = ?3 AND state_version = ?4
                             AND state IN ('claimed', 'dispatched')"#,
                        rusqlite::params![
                            scope_key,
                            request_hash,
                            owner_attempt_id,
                            expected_version,
                            status_code as i64,
                            headers_json,
                            body,
                            now_ms,
                            result_hash,
                        ],
                    )?;
                    Ok(updated == 1)
                })
                .await
                .map_err(|error| StorageError::Task(error.to_string()))?
            }
            StorageStore::Postgres(pg) => {
                let client = pg.client().await?;
                let status_code = status_code as i64;
                Ok(client
                    .execute(
                        r#"UPDATE idempotency_records
                           SET state = 'completed', state_version = state_version + 1,
                               status_code = $5, headers_json = $6, body = $7,
                               updated_at_ms = $8, completed_at_ms = $8, result_hash = $9
                           WHERE scope_key = $1 AND request_hash = $2
                             AND owner_attempt_id = $3 AND state_version = $4
                             AND state IN ('claimed', 'dispatched')"#,
                        &[
                            &scope_key,
                            &request_hash,
                            &owner_attempt_id,
                            &expected_version,
                            &status_code,
                            &headers_json,
                            &body,
                            &now_ms,
                            &result_hash,
                        ],
                    )
                    .await?
                    == 1)
            }
        }
    }

    /// Deletes only the exact, undispatched owner/version claim. A dispatched,
    /// legacy pending, or indeterminate mutation can never pass this CAS.
    pub async fn release_undispatched(
        store: &StorageStore,
        scope_key: &str,
        request_hash: &str,
        owner_attempt_id: &str,
        expected_version: i64,
    ) -> Result<bool, StorageError> {
        match store {
            StorageStore::Sqlite(sqlite) => {
                let sqlite = sqlite.clone();
                let scope_key = scope_key.to_string();
                let request_hash = request_hash.to_string();
                let owner_attempt_id = owner_attempt_id.to_string();
                tokio::task::spawn_blocking(move || {
                    let conn = rusqlite::Connection::open(sqlite.path())?;
                    let deleted = conn.execute(
                        r#"DELETE FROM idempotency_records
                           WHERE scope_key = ?1 AND request_hash = ?2
                             AND owner_attempt_id = ?3 AND state_version = ?4
                             AND state = 'claimed'"#,
                        rusqlite::params![
                            scope_key,
                            request_hash,
                            owner_attempt_id,
                            expected_version
                        ],
                    )?;
                    Ok(deleted == 1)
                })
                .await
                .map_err(|error| StorageError::Task(error.to_string()))?
            }
            StorageStore::Postgres(pg) => {
                let client = pg.client().await?;
                Ok(client
                    .execute(
                        r#"DELETE FROM idempotency_records
                           WHERE scope_key = $1 AND request_hash = $2
                             AND owner_attempt_id = $3 AND state_version = $4
                             AND state = 'claimed'"#,
                        &[
                            &scope_key,
                            &request_hash,
                            &owner_attempt_id,
                            &expected_version,
                        ],
                    )
                    .await?
                    == 1)
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub async fn complete_by_operator(
        store: &StorageStore,
        scope_key: &str,
        expected_version: i64,
        status_code: u32,
        headers_json: &str,
        body: &[u8],
        operator: &str,
        reason: &str,
        now_ms: i64,
        audit_event_id: &str,
    ) -> Result<bool, StorageError> {
        validate_reconciliation(operator, reason, audit_event_id)?;
        let hash = result_hash(status_code, headers_json, body);
        operator_transition(
            store,
            scope_key,
            expected_version,
            "completed_by_operator",
            operator,
            reason,
            now_ms,
            audit_event_id,
            Some((status_code, headers_json, body, hash.as_str())),
        )
        .await
    }

    #[allow(clippy::too_many_arguments)]
    pub async fn release_by_operator(
        store: &StorageStore,
        scope_key: &str,
        expected_version: i64,
        operator: &str,
        reason: &str,
        now_ms: i64,
        audit_event_id: &str,
    ) -> Result<bool, StorageError> {
        validate_reconciliation(operator, reason, audit_event_id)?;
        operator_transition(
            store,
            scope_key,
            expected_version,
            "released_by_operator",
            operator,
            reason,
            now_ms,
            audit_event_id,
            None,
        )
        .await
    }
}

#[allow(clippy::too_many_arguments)]
async fn owner_transition(
    store: &StorageStore,
    scope_key: &str,
    request_hash: &str,
    owner_attempt_id: &str,
    expected_version: i64,
    from_state: &'static str,
    to_state: &'static str,
    now_ms: i64,
) -> Result<Option<i64>, StorageError> {
    match store {
        StorageStore::Sqlite(sqlite) => {
            let sqlite = sqlite.clone();
            let scope_key = scope_key.to_string();
            let request_hash = request_hash.to_string();
            let owner_attempt_id = owner_attempt_id.to_string();
            tokio::task::spawn_blocking(move || {
                let conn = rusqlite::Connection::open(sqlite.path())?;
                let sql = if to_state == "dispatched" {
                    r#"UPDATE idempotency_records
                       SET state = ?6, state_version = state_version + 1,
                           updated_at_ms = ?5, dispatched_at_ms = ?5
                       WHERE scope_key = ?1 AND request_hash = ?2
                         AND owner_attempt_id = ?3 AND state_version = ?4 AND state = ?7
                       RETURNING state_version"#
                } else {
                    r#"UPDATE idempotency_records
                       SET state = ?6, state_version = state_version + 1, updated_at_ms = ?5
                       WHERE scope_key = ?1 AND request_hash = ?2
                         AND owner_attempt_id = ?3 AND state_version = ?4 AND state = ?7
                       RETURNING state_version"#
                };
                conn.query_row(
                    sql,
                    rusqlite::params![
                        scope_key,
                        request_hash,
                        owner_attempt_id,
                        expected_version,
                        now_ms,
                        to_state,
                        from_state,
                    ],
                    |row| row.get(0),
                )
                .optional()
                .map_err(StorageError::from)
            })
            .await
            .map_err(|error| StorageError::Task(error.to_string()))?
        }
        StorageStore::Postgres(pg) => {
            let client = pg.client().await?;
            let sql = if to_state == "dispatched" {
                r#"UPDATE idempotency_records
                   SET state = $6, state_version = state_version + 1,
                       updated_at_ms = $5, dispatched_at_ms = $5
                   WHERE scope_key = $1 AND request_hash = $2
                     AND owner_attempt_id = $3 AND state_version = $4 AND state = $7
                   RETURNING state_version"#
            } else {
                r#"UPDATE idempotency_records
                   SET state = $6, state_version = state_version + 1, updated_at_ms = $5
                   WHERE scope_key = $1 AND request_hash = $2
                     AND owner_attempt_id = $3 AND state_version = $4 AND state = $7
                   RETURNING state_version"#
            };
            Ok(client
                .query_opt(
                    sql,
                    &[
                        &scope_key,
                        &request_hash,
                        &owner_attempt_id,
                        &expected_version,
                        &now_ms,
                        &to_state,
                        &from_state,
                    ],
                )
                .await?
                .map(|row| row.get(0)))
        }
    }
}

type Completion<'a> = (u32, &'a str, &'a [u8], &'a str);

#[allow(clippy::too_many_arguments)]
async fn operator_transition(
    store: &StorageStore,
    scope_key: &str,
    expected_version: i64,
    new_state: &'static str,
    operator: &str,
    reason: &str,
    now_ms: i64,
    audit_event_id: &str,
    completion: Option<Completion<'_>>,
) -> Result<bool, StorageError> {
    match store {
        StorageStore::Sqlite(sqlite) => {
            let sqlite = sqlite.clone();
            let scope_key = scope_key.to_string();
            let operator = operator.to_string();
            let reason = reason.to_string();
            let audit_event_id = audit_event_id.to_string();
            let completion = completion.map(|(status, headers, body, hash)| {
                (status, headers.to_string(), body.to_vec(), hash.to_string())
            });
            tokio::task::spawn_blocking(move || {
                let mut conn = rusqlite::Connection::open(sqlite.path())?;
                let tx =
                    conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
                let result_hash = completion.as_ref().map(|value| value.3.as_str());
                let updated = if let Some((status, headers, body, hash)) = completion.as_ref() {
                    tx.execute(
                        r#"UPDATE idempotency_records
                           SET state = ?3, state_version = state_version + 1,
                               status_code = ?7, headers_json = ?8, body = ?9,
                               updated_at_ms = ?6, completed_at_ms = ?6,
                               reconciled_by = ?4, reconcile_reason = ?5, result_hash = ?10
                           WHERE scope_key = ?1 AND state_version = ?2
                             AND state = 'indeterminate'"#,
                        rusqlite::params![
                            scope_key,
                            expected_version,
                            new_state,
                            operator,
                            reason,
                            now_ms,
                            *status as i64,
                            headers,
                            body,
                            hash,
                        ],
                    )?
                } else {
                    tx.execute(
                        r#"UPDATE idempotency_records
                           SET state = ?3, state_version = state_version + 1,
                               updated_at_ms = ?6, reconciled_by = ?4,
                               reconcile_reason = ?5
                           WHERE scope_key = ?1 AND state_version = ?2
                             AND state = 'indeterminate'"#,
                        rusqlite::params![
                            scope_key,
                            expected_version,
                            new_state,
                            operator,
                            reason,
                            now_ms,
                        ],
                    )?
                };
                if updated == 0 {
                    tx.rollback()?;
                    return Ok(false);
                }
                insert_sqlite_reconciliation_event(
                    &tx,
                    &audit_event_id,
                    &scope_key,
                    new_state,
                    expected_version,
                    &operator,
                    &reason,
                    result_hash,
                    now_ms,
                )?;
                tx.commit()?;
                Ok(true)
            })
            .await
            .map_err(|error| StorageError::Task(error.to_string()))?
        }
        StorageStore::Postgres(pg) => {
            let mut client = pg.client().await?;
            let transaction = client.transaction().await?;
            let result_hash = completion.map(|value| value.3);
            let updated = if let Some((status, headers, body, hash)) = completion {
                let status = status as i64;
                transaction
                    .execute(
                        r#"UPDATE idempotency_records
                           SET state = $3, state_version = state_version + 1,
                               status_code = $7, headers_json = $8, body = $9,
                               updated_at_ms = $6, completed_at_ms = $6,
                               reconciled_by = $4, reconcile_reason = $5, result_hash = $10
                           WHERE scope_key = $1 AND state_version = $2
                             AND state = 'indeterminate'"#,
                        &[
                            &scope_key,
                            &expected_version,
                            &new_state,
                            &operator,
                            &reason,
                            &now_ms,
                            &status,
                            &headers,
                            &body,
                            &hash,
                        ],
                    )
                    .await?
            } else {
                transaction
                    .execute(
                        r#"UPDATE idempotency_records
                           SET state = $3, state_version = state_version + 1,
                               updated_at_ms = $6, reconciled_by = $4,
                               reconcile_reason = $5
                           WHERE scope_key = $1 AND state_version = $2
                             AND state = 'indeterminate'"#,
                        &[
                            &scope_key,
                            &expected_version,
                            &new_state,
                            &operator,
                            &reason,
                            &now_ms,
                        ],
                    )
                    .await?
            };
            if updated == 0 {
                transaction.rollback().await?;
                return Ok(false);
            }
            let new_version = expected_version + 1;
            transaction
                .execute(
                    r#"INSERT INTO idempotency_reconciliation_events
                       (event_id, scope_key, previous_state, new_state,
                        previous_version, new_version, reconciled_by,
                        reconcile_reason, result_hash, created_at_ms)
                       VALUES ($1, $2, 'indeterminate', $3, $4, $5, $6, $7, $8, $9)"#,
                    &[
                        &audit_event_id,
                        &scope_key,
                        &new_state,
                        &expected_version,
                        &new_version,
                        &operator,
                        &reason,
                        &result_hash,
                        &now_ms,
                    ],
                )
                .await?;
            transaction.commit().await?;
            Ok(true)
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn insert_sqlite_reconciliation_event(
    transaction: &rusqlite::Transaction<'_>,
    audit_event_id: &str,
    scope_key: &str,
    new_state: &str,
    expected_version: i64,
    operator: &str,
    reason: &str,
    result_hash: Option<&str>,
    now_ms: i64,
) -> Result<(), StorageError> {
    transaction.execute(
        r#"INSERT INTO idempotency_reconciliation_events
           (event_id, scope_key, previous_state, new_state,
            previous_version, new_version, reconciled_by,
            reconcile_reason, result_hash, created_at_ms)
           VALUES (?1, ?2, 'indeterminate', ?3, ?4, ?5, ?6, ?7, ?8, ?9)"#,
        rusqlite::params![
            audit_event_id,
            scope_key,
            new_state,
            expected_version,
            expected_version + 1,
            operator,
            reason,
            result_hash,
            now_ms,
        ],
    )?;
    Ok(())
}

fn validate_reconciliation(
    operator: &str,
    reason: &str,
    audit_event_id: &str,
) -> Result<(), StorageError> {
    if operator.trim().is_empty() || audit_event_id.trim().is_empty() {
        return Err(StorageError::Invariant(
            "operator and audit event id are required for reconciliation".into(),
        ));
    }
    if reason.trim().len() < 8 {
        return Err(StorageError::Invariant(
            "reconciliation reason must contain at least 8 characters".into(),
        ));
    }
    Ok(())
}

fn result_hash(status_code: u32, headers_json: &str, body: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(status_code.to_be_bytes());
    hasher.update(headers_json.as_bytes());
    hasher.update(body);
    hasher
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

const SELECT_COLUMNS: &str = r#"SELECT scope_key, request_hash, state, state_version,
    owner_attempt_id, status_code, headers_json, body, created_at_ms, updated_at_ms,
    expires_at_ms, dispatched_at_ms, completed_at_ms, reconciled_by,
    reconcile_reason, result_hash FROM idempotency_records"#;

fn query_sqlite_record(
    connection: &rusqlite::Connection,
    scope_key: &str,
) -> Result<Option<IdempotencyRecord>, StorageError> {
    connection
        .query_row(
            &format!("{SELECT_COLUMNS} WHERE scope_key = ?1"),
            [scope_key],
            map_sqlite_record,
        )
        .optional()
        .map_err(StorageError::from)
}

fn map_sqlite_record(row: &rusqlite::Row<'_>) -> rusqlite::Result<IdempotencyRecord> {
    let state = row.get::<_, String>(2)?;
    let state = IdempotencyState::from_database(&state).map_err(|error| {
        rusqlite::Error::FromSqlConversionFailure(2, rusqlite::types::Type::Text, Box::new(error))
    })?;
    Ok(IdempotencyRecord {
        scope_key: row.get(0)?,
        request_hash: row.get(1)?,
        state,
        state_version: row.get(3)?,
        owner_attempt_id: row.get(4)?,
        status_code: row.get::<_, i64>(5)?.max(0) as u32,
        headers_json: row.get(6)?,
        body: row.get(7)?,
        created_at_ms: row.get(8)?,
        updated_at_ms: row.get(9)?,
        expires_at_ms: row.get(10)?,
        dispatched_at_ms: row.get(11)?,
        completed_at_ms: row.get(12)?,
        reconciled_by: row.get(13)?,
        reconcile_reason: row.get(14)?,
        result_hash: row.get(15)?,
    })
}

async fn query_postgres_record(
    client: &tokio_postgres::Client,
    scope_key: &str,
) -> Result<Option<IdempotencyRecord>, StorageError> {
    client
        .query_opt(
            &format!("{SELECT_COLUMNS} WHERE scope_key = $1"),
            &[&scope_key],
        )
        .await?
        .as_ref()
        .map(map_postgres_record)
        .transpose()
}

fn map_postgres_record(row: &tokio_postgres::Row) -> Result<IdempotencyRecord, StorageError> {
    Ok(IdempotencyRecord {
        scope_key: row.get(0),
        request_hash: row.get(1),
        state: IdempotencyState::from_database(row.get::<_, String>(2).as_str())?,
        state_version: row.get(3),
        owner_attempt_id: row.get(4),
        status_code: row.get::<_, i64>(5).max(0) as u32,
        headers_json: row.get(6),
        body: row.get(7),
        created_at_ms: row.get(8),
        updated_at_ms: row.get(9),
        expires_at_ms: row.get(10),
        dispatched_at_ms: row.get(11),
        completed_at_ms: row.get(12),
        reconciled_by: row.get(13),
        reconcile_reason: row.get(14),
        result_hash: row.get(15),
    })
}

fn classify_existing(requested_hash: &str, record: IdempotencyRecord) -> IdempotencyClaim {
    if record.request_hash != requested_hash {
        return IdempotencyClaim::Conflict;
    }
    if matches!(
        record.state,
        IdempotencyState::Completed | IdempotencyState::CompletedByOperator
    ) {
        return IdempotencyClaim::Completed(Box::new(record));
    }
    IdempotencyClaim::Pending
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{ensure_store_schema, SqliteStore};

    fn test_store(name: &str) -> (StorageStore, String) {
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path = std::env::temp_dir()
            .join(format!(
                "sag-idempotency-{name}-{}-{nonce}.db",
                std::process::id()
            ))
            .to_string_lossy()
            .into_owned();
        (StorageStore::Sqlite(SqliteStore::new(path.clone())), path)
    }

    #[tokio::test]
    async fn claim_conflict_complete_and_replay_are_atomic() {
        let (store, path) = test_store("lifecycle");
        ensure_store_schema(&store).await.unwrap();

        let first = IdempotencyStore::claim(&store, "scope", "hash", "attempt-1", 10, 1_000)
            .await
            .unwrap();
        assert_eq!(first, IdempotencyClaim::Claimed { state_version: 1 });
        let pending = IdempotencyStore::claim(&store, "scope", "hash", "attempt-2", 11, 1_000)
            .await
            .unwrap();
        assert_eq!(pending, IdempotencyClaim::Pending);
        let conflict = IdempotencyStore::claim(&store, "scope", "other", "attempt-3", 12, 1_000)
            .await
            .unwrap();
        assert_eq!(conflict, IdempotencyClaim::Conflict);

        assert!(IdempotencyStore::complete(
            &store,
            "scope",
            "hash",
            "attempt-1",
            1,
            201,
            r#"{"content-type":"application/json"}"#,
            br#"{"ok":true}"#,
            20,
        )
        .await
        .unwrap());
        let replay = IdempotencyStore::claim(&store, "scope", "hash", "attempt-4", 21, 1_000)
            .await
            .unwrap();
        let IdempotencyClaim::Completed(record) = replay else {
            panic!("expected completed replay");
        };
        assert_eq!(record.status_code, 201);
        assert_eq!(record.body, br#"{"ok":true}"#);

        drop(store);
        let _ = std::fs::remove_file(path);
    }

    #[tokio::test]
    async fn only_owner_and_version_can_release_an_undispatched_claim() {
        let (store, path) = test_store("release");
        ensure_store_schema(&store).await.unwrap();
        IdempotencyStore::claim(&store, "scope", "hash", "owner", 10, 1_000)
            .await
            .unwrap();

        assert!(
            !IdempotencyStore::release_undispatched(&store, "scope", "hash", "not-owner", 1)
                .await
                .unwrap()
        );
        assert!(
            IdempotencyStore::release_undispatched(&store, "scope", "hash", "owner", 1)
                .await
                .unwrap()
        );
        assert_eq!(
            IdempotencyStore::claim(&store, "scope", "hash", "new-owner", 20, 1_000)
                .await
                .unwrap(),
            IdempotencyClaim::Claimed { state_version: 1 }
        );

        drop(store);
        let _ = std::fs::remove_file(path);
    }
}
