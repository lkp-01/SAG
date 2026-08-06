use serde::{Deserialize, Serialize};

use crate::{StorageError, StorageStore, TunnelRouteRecord};

const JOB_COLUMNS: &str = "job_id, generation, target, resource_type, resource_id, app_id, \
    operation, payload_json, status, attempt_count, next_attempt_at_ms, last_error, \
    lease_owner, lease_expires_at_ms, superseded_by_generation, created_at_ms, \
    updated_at_ms, applied_at_ms";
const CLAIMED_JOB_COLUMNS: &str = "jobs.job_id, jobs.generation, jobs.target, \
    jobs.resource_type, jobs.resource_id, jobs.app_id, jobs.operation, jobs.payload_json, \
    jobs.status, jobs.attempt_count, jobs.next_attempt_at_ms, jobs.last_error, \
    jobs.lease_owner, jobs.lease_expires_at_ms, jobs.superseded_by_generation, \
    jobs.created_at_ms, jobs.updated_at_ms, jobs.applied_at_ms";
const AGENT_APPLY_COLUMNS: &str =
    "agent_id, applied_generation, snapshot_hash, applied_at_ms, reported_at_ms";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum ConfigSyncOperation {
    Upsert,
    Delete,
}

impl ConfigSyncOperation {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Upsert => "UPSERT",
            Self::Delete => "DELETE",
        }
    }

    fn from_db(value: &str) -> Result<Self, StorageError> {
        match value {
            "UPSERT" => Ok(Self::Upsert),
            "DELETE" => Ok(Self::Delete),
            other => Err(StorageError::Invariant(format!(
                "unknown config sync operation {other:?}"
            ))),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum ConfigSyncStatus {
    Pending,
    Applied,
    Failed,
}

impl ConfigSyncStatus {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "PENDING",
            Self::Applied => "APPLIED",
            Self::Failed => "FAILED",
        }
    }

    fn from_db(value: &str) -> Result<Self, StorageError> {
        match value {
            "PENDING" => Ok(Self::Pending),
            "APPLIED" => Ok(Self::Applied),
            "FAILED" => Ok(Self::Failed),
            other => Err(StorageError::Invariant(format!(
                "unknown config sync status {other:?}"
            ))),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConfigSyncJobDraft {
    pub generation: i64,
    pub target: String,
    pub resource_type: String,
    pub resource_id: String,
    pub app_id: String,
    pub operation: ConfigSyncOperation,
    pub payload_json: Option<String>,
    pub next_attempt_at_ms: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConfigSyncJob {
    pub job_id: String,
    pub generation: i64,
    pub target: String,
    pub resource_type: String,
    pub resource_id: String,
    pub app_id: String,
    pub operation: ConfigSyncOperation,
    pub payload_json: Option<String>,
    pub status: ConfigSyncStatus,
    pub attempt_count: i64,
    pub next_attempt_at_ms: i64,
    pub last_error: Option<String>,
    pub lease_owner: Option<String>,
    pub lease_expires_at_ms: Option<i64>,
    pub superseded_by_generation: Option<i64>,
    pub created_at_ms: i64,
    pub updated_at_ms: i64,
    pub applied_at_ms: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentConfigApply {
    pub agent_id: String,
    pub applied_generation: i64,
    pub snapshot_hash: Option<String>,
    pub applied_at_ms: i64,
    pub reported_at_ms: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RouteConfigSnapshot {
    pub generation: i64,
    pub routes: Vec<TunnelRouteRecord>,
}

#[derive(Debug)]
struct RawConfigSyncJob {
    job_id: String,
    generation: i64,
    target: String,
    resource_type: String,
    resource_id: String,
    app_id: String,
    operation: String,
    payload_json: Option<String>,
    status: String,
    attempt_count: i64,
    next_attempt_at_ms: i64,
    last_error: Option<String>,
    lease_owner: Option<String>,
    lease_expires_at_ms: Option<i64>,
    superseded_by_generation: Option<i64>,
    created_at_ms: i64,
    updated_at_ms: i64,
    applied_at_ms: Option<i64>,
}

impl RawConfigSyncJob {
    fn into_typed(self) -> Result<ConfigSyncJob, StorageError> {
        Ok(ConfigSyncJob {
            job_id: self.job_id,
            generation: self.generation,
            target: self.target,
            resource_type: self.resource_type,
            resource_id: self.resource_id,
            app_id: self.app_id,
            operation: ConfigSyncOperation::from_db(&self.operation)?,
            payload_json: self.payload_json,
            status: ConfigSyncStatus::from_db(&self.status)?,
            attempt_count: self.attempt_count,
            next_attempt_at_ms: self.next_attempt_at_ms,
            last_error: self.last_error,
            lease_owner: self.lease_owner,
            lease_expires_at_ms: self.lease_expires_at_ms,
            superseded_by_generation: self.superseded_by_generation,
            created_at_ms: self.created_at_ms,
            updated_at_ms: self.updated_at_ms,
            applied_at_ms: self.applied_at_ms,
        })
    }
}

fn raw_sqlite_job(row: &rusqlite::Row<'_>) -> rusqlite::Result<RawConfigSyncJob> {
    Ok(RawConfigSyncJob {
        job_id: row.get(0)?,
        generation: row.get(1)?,
        target: row.get(2)?,
        resource_type: row.get(3)?,
        resource_id: row.get(4)?,
        app_id: row.get(5)?,
        operation: row.get(6)?,
        payload_json: row.get(7)?,
        status: row.get(8)?,
        attempt_count: row.get(9)?,
        next_attempt_at_ms: row.get(10)?,
        last_error: row.get(11)?,
        lease_owner: row.get(12)?,
        lease_expires_at_ms: row.get(13)?,
        superseded_by_generation: row.get(14)?,
        created_at_ms: row.get(15)?,
        updated_at_ms: row.get(16)?,
        applied_at_ms: row.get(17)?,
    })
}

fn raw_postgres_job(row: &tokio_postgres::Row) -> RawConfigSyncJob {
    RawConfigSyncJob {
        job_id: row.get(0),
        generation: row.get(1),
        target: row.get(2),
        resource_type: row.get(3),
        resource_id: row.get(4),
        app_id: row.get(5),
        operation: row.get(6),
        payload_json: row.get(7),
        status: row.get(8),
        attempt_count: row.get(9),
        next_attempt_at_ms: row.get(10),
        last_error: row.get(11),
        lease_owner: row.get(12),
        lease_expires_at_ms: row.get(13),
        superseded_by_generation: row.get(14),
        created_at_ms: row.get(15),
        updated_at_ms: row.get(16),
        applied_at_ms: row.get(17),
    }
}

fn sqlite_agent_apply(row: &rusqlite::Row<'_>) -> rusqlite::Result<AgentConfigApply> {
    Ok(AgentConfigApply {
        agent_id: row.get(0)?,
        applied_generation: row.get(1)?,
        snapshot_hash: row.get(2)?,
        applied_at_ms: row.get(3)?,
        reported_at_ms: row.get(4)?,
    })
}

fn postgres_agent_apply(row: &tokio_postgres::Row) -> AgentConfigApply {
    AgentConfigApply {
        agent_id: row.get(0),
        applied_generation: row.get(1),
        snapshot_hash: row.get(2),
        applied_at_ms: row.get(3),
        reported_at_ms: row.get(4),
    }
}

fn validate_nonnegative(name: &str, value: i64) -> Result<(), StorageError> {
    if value < 0 {
        return Err(StorageError::Invariant(format!(
            "{name} must be non-negative"
        )));
    }
    Ok(())
}

fn validate_identifier(name: &str, value: &str) -> Result<(), StorageError> {
    if value.is_empty() || value.trim() != value {
        return Err(StorageError::Invariant(format!(
            "{name} must be non-empty and trimmed"
        )));
    }
    Ok(())
}

fn validate_snapshot_hash(snapshot_hash: Option<&str>) -> Result<(), StorageError> {
    if let Some(snapshot_hash) = snapshot_hash {
        if snapshot_hash.len() != 64
            || !snapshot_hash
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        {
            return Err(StorageError::Invariant(
                "snapshot_hash must contain exactly 64 lowercase hexadecimal characters".into(),
            ));
        }
    }
    Ok(())
}

fn validate_draft(draft: &ConfigSyncJobDraft, now_ms: i64) -> Result<(), StorageError> {
    validate_nonnegative("generation", draft.generation)?;
    validate_nonnegative("next_attempt_at_ms", draft.next_attempt_at_ms)?;
    validate_nonnegative("now_ms", now_ms)?;
    validate_identifier("target", &draft.target)?;
    validate_identifier("resource_type", &draft.resource_type)?;
    validate_identifier("resource_id", &draft.resource_id)?;
    validate_identifier("app_id", &draft.app_id)?;
    if let Some(payload) = &draft.payload_json {
        serde_json::from_str::<serde_json::Value>(payload)?;
    }
    Ok(())
}

fn job_matches_draft(job: &ConfigSyncJob, draft: &ConfigSyncJobDraft) -> bool {
    job.generation == draft.generation
        && job.target == draft.target
        && job.resource_type == draft.resource_type
        && job.resource_id == draft.resource_id
        && job.app_id == draft.app_id
        && job.operation == draft.operation
        && job.payload_json == draft.payload_json
}

fn bounded_error(error: &str) -> String {
    let mut normalized = String::with_capacity(error.len().min(4_096));
    let mut pending_space = false;
    let mut character_count = 0_usize;
    for character in error.trim().chars() {
        if character.is_whitespace() || character.is_control() {
            pending_space = !normalized.is_empty();
            continue;
        }
        if pending_space && character_count < 4_096 {
            normalized.push(' ');
            character_count += 1;
        }
        pending_space = false;
        if character_count >= 4_096 {
            break;
        }
        normalized.push(character);
        character_count += 1;
    }
    if normalized.is_empty() {
        "unspecified external error".into()
    } else {
        normalized
    }
}

fn rebase_schedule_ms(
    source_now_ms: i64,
    source_due_ms: i64,
    database_now_ms: i64,
) -> Result<i64, StorageError> {
    let delay_ms = source_due_ms.saturating_sub(source_now_ms).max(0);
    database_now_ms
        .checked_add(delay_ms)
        .ok_or_else(|| StorageError::Invariant("rebased schedule timestamp overflow".into()))
}

async fn postgres_transaction_now_ms(
    transaction: &tokio_postgres::Transaction<'_>,
) -> Result<i64, StorageError> {
    Ok(transaction
        .query_one(
            "SELECT (EXTRACT(EPOCH FROM clock_timestamp()) * 1000)::BIGINT",
            &[],
        )
        .await?
        .get(0))
}

pub struct ConfigSyncStore;

impl ConfigSyncStore {
    pub async fn current_generation(store: &StorageStore) -> Result<i64, StorageError> {
        match store {
            StorageStore::Sqlite(sqlite) => {
                let path = sqlite.path().to_string();
                tokio::task::spawn_blocking(move || {
                    let connection = rusqlite::Connection::open(path)?;
                    let generation = connection.query_row(
                        "SELECT generation FROM config_state WHERE id = 1",
                        [],
                        |row| row.get(0),
                    )?;
                    Ok::<_, StorageError>(generation)
                })
                .await
                .map_err(|error| StorageError::Task(error.to_string()))?
            }
            StorageStore::Postgres(postgres) => {
                let client = postgres.client().await?;
                let row = client
                    .query_one("SELECT generation FROM config_state WHERE id = 1", &[])
                    .await?;
                Ok(row.get(0))
            }
        }
    }

    pub async fn load_route_snapshot(
        store: &StorageStore,
    ) -> Result<RouteConfigSnapshot, StorageError> {
        match store {
            StorageStore::Sqlite(sqlite) => {
                let path = sqlite.path().to_string();
                tokio::task::spawn_blocking(move || {
                    let mut connection = rusqlite::Connection::open(path)?;
                    let transaction = connection
                        .transaction_with_behavior(rusqlite::TransactionBehavior::Deferred)?;
                    let generation = transaction.query_row(
                        "SELECT generation FROM config_state WHERE id = 1",
                        [],
                        |row| row.get(0),
                    )?;
                    let routes = {
                        let mut statement = transaction.prepare(
                            "SELECT host, app_id, connector_endpoint, require_healthy_tunnel \
                             FROM tunnel_routes ORDER BY app_id, host",
                        )?;
                        let rows = statement.query_map([], |row| {
                            Ok(TunnelRouteRecord {
                                host: row.get(0)?,
                                app_id: row.get(1)?,
                                connector_endpoint: row.get(2)?,
                                require_healthy_tunnel: row.get::<_, i32>(3)? != 0,
                            })
                        })?;
                        let mut routes = Vec::new();
                        for row in rows {
                            routes.push(row?);
                        }
                        routes
                    };
                    transaction.commit()?;
                    Ok::<_, StorageError>(RouteConfigSnapshot { generation, routes })
                })
                .await
                .map_err(|error| StorageError::Task(error.to_string()))?
            }
            StorageStore::Postgres(postgres) => {
                let mut client = postgres.client().await?;
                let transaction = client
                    .build_transaction()
                    .isolation_level(tokio_postgres::IsolationLevel::RepeatableRead)
                    .read_only(true)
                    .start()
                    .await?;
                let generation: i64 = transaction
                    .query_one("SELECT generation FROM config_state WHERE id = 1", &[])
                    .await?
                    .get(0);
                let rows = transaction
                    .query(
                        "SELECT host, app_id, connector_endpoint, require_healthy_tunnel \
                         FROM tunnel_routes ORDER BY app_id, host",
                        &[],
                    )
                    .await?;
                let routes = rows
                    .into_iter()
                    .map(|row| TunnelRouteRecord {
                        host: row.get(0),
                        app_id: row.get(1),
                        connector_endpoint: row.get(2),
                        require_healthy_tunnel: row.get(3),
                    })
                    .collect();
                transaction.commit().await?;
                Ok(RouteConfigSnapshot { generation, routes })
            }
        }
    }

    pub async fn enqueue_job(
        store: &StorageStore,
        draft: &ConfigSyncJobDraft,
        now_ms: i64,
    ) -> Result<ConfigSyncJob, StorageError> {
        validate_draft(draft, now_ms)?;
        match store {
            StorageStore::Sqlite(sqlite) => {
                let path = sqlite.path().to_string();
                let draft = draft.clone();
                tokio::task::spawn_blocking(move || {
                    let mut connection = rusqlite::Connection::open(path)?;
                    let transaction = connection
                        .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
                    let job = Self::enqueue_sqlite_transaction(&transaction, &draft, now_ms)?;
                    transaction.commit()?;
                    Ok::<_, StorageError>(job)
                })
                .await
                .map_err(|error| StorageError::Task(error.to_string()))?
            }
            StorageStore::Postgres(postgres) => {
                let mut client = postgres.client().await?;
                let transaction = client.transaction().await?;
                let job = Self::enqueue_postgres_transaction(&transaction, draft, now_ms).await?;
                transaction.commit().await?;
                Ok(job)
            }
        }
    }

    /// Ensures drift repair is due even when the mutation job for the current
    /// generation was already applied. Active leases are never stolen; a
    /// worker already converging the resource remains its sole owner.
    pub async fn requeue_job(
        store: &StorageStore,
        draft: &ConfigSyncJobDraft,
        now_ms: i64,
    ) -> Result<bool, StorageError> {
        let job = Self::enqueue_job(store, draft, now_ms).await?;
        let job_id = job.job_id;
        let next_attempt_at_ms = draft.next_attempt_at_ms;
        match store {
            StorageStore::Sqlite(sqlite) => {
                let path = sqlite.path().to_string();
                let job_id = job_id.clone();
                tokio::task::spawn_blocking(move || {
                    let connection = rusqlite::Connection::open(path)?;
                    let changed = connection.execute(
                        "UPDATE config_sync_jobs \
                         SET status = 'PENDING', next_attempt_at_ms = ?1, last_error = NULL, \
                             applied_at_ms = NULL, updated_at_ms = ?2 \
                         WHERE job_id = ?3 AND superseded_by_generation IS NULL \
                           AND (lease_expires_at_ms IS NULL OR lease_expires_at_ms <= ?2)",
                        rusqlite::params![next_attempt_at_ms, now_ms, job_id],
                    )?;
                    Ok::<_, StorageError>(changed == 1)
                })
                .await
                .map_err(|error| StorageError::Task(error.to_string()))?
            }
            StorageStore::Postgres(postgres) => {
                let mut client = postgres.client().await?;
                let transaction = client.transaction().await?;
                let database_now_ms = postgres_transaction_now_ms(&transaction).await?;
                let database_next_attempt_at_ms =
                    rebase_schedule_ms(now_ms, next_attempt_at_ms, database_now_ms)?;
                let changed = transaction
                    .execute(
                        "UPDATE config_sync_jobs \
                         SET status = 'PENDING', next_attempt_at_ms = $1, last_error = NULL, \
                             applied_at_ms = NULL, updated_at_ms = $2 \
                         WHERE job_id = $3 AND superseded_by_generation IS NULL \
                           AND (lease_expires_at_ms IS NULL OR lease_expires_at_ms <= $2)",
                        &[&database_next_attempt_at_ms, &database_now_ms, &job_id],
                    )
                    .await?;
                transaction.commit().await?;
                Ok(changed == 1)
            }
        }
    }

    pub async fn claim_due_jobs(
        store: &StorageStore,
        lease_owner: &str,
        now_ms: i64,
        lease_duration_ms: i64,
        limit: usize,
    ) -> Result<Vec<ConfigSyncJob>, StorageError> {
        validate_identifier("lease_owner", lease_owner)?;
        validate_nonnegative("now_ms", now_ms)?;
        if lease_duration_ms <= 0 {
            return Err(StorageError::Invariant(
                "lease_duration_ms must be greater than zero".into(),
            ));
        }
        if limit == 0 {
            return Ok(Vec::new());
        }
        let limit = i64::try_from(limit)
            .map_err(|_| StorageError::Invariant("claim limit exceeds i64".into()))?;
        match store {
            StorageStore::Sqlite(sqlite) => {
                let lease_expires_at_ms =
                    now_ms.checked_add(lease_duration_ms).ok_or_else(|| {
                        StorageError::Invariant("lease expiration timestamp overflow".into())
                    })?;
                let path = sqlite.path().to_string();
                let lease_owner = lease_owner.to_string();
                tokio::task::spawn_blocking(move || {
                    let mut connection = rusqlite::Connection::open(path)?;
                    let transaction = connection
                        .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
                    let job_ids = {
                        let mut statement = transaction.prepare(
                            "SELECT jobs.job_id FROM config_sync_jobs AS jobs \
                             WHERE jobs.status IN ('PENDING', 'FAILED') \
                               AND jobs.superseded_by_generation IS NULL \
                               AND jobs.next_attempt_at_ms <= ?1 \
                               AND (jobs.lease_expires_at_ms IS NULL OR jobs.lease_expires_at_ms <= ?1) \
                               AND NOT EXISTS ( \
                                 SELECT 1 FROM config_sync_jobs AS active \
                                 WHERE active.target = jobs.target \
                                   AND active.app_id = jobs.app_id \
                                   AND active.job_id <> jobs.job_id \
                                   AND active.lease_expires_at_ms > ?1 \
                               ) \
                             ORDER BY jobs.generation, jobs.created_at_ms, jobs.job_id LIMIT ?2",
                        )?;
                        let rows = statement
                            .query_map(rusqlite::params![now_ms, limit], |row| {
                                row.get::<_, String>(0)
                            })?;
                        let mut job_ids = Vec::new();
                        for row in rows {
                            job_ids.push(row?);
                        }
                        job_ids
                    };

                    let mut claimed = Vec::with_capacity(job_ids.len());
                    for job_id in job_ids {
                        let changed = transaction.execute(
                            "UPDATE config_sync_jobs \
                             SET lease_owner = ?1, lease_expires_at_ms = ?2, \
                                 attempt_count = attempt_count + 1, updated_at_ms = ?3 \
                             WHERE job_id = ?4 \
                               AND status IN ('PENDING', 'FAILED') \
                               AND superseded_by_generation IS NULL \
                               AND next_attempt_at_ms <= ?3 \
                               AND (lease_expires_at_ms IS NULL OR lease_expires_at_ms <= ?3) \
                               AND NOT EXISTS ( \
                                 SELECT 1 FROM config_sync_jobs AS active \
                                 WHERE active.target = config_sync_jobs.target \
                                   AND active.app_id = config_sync_jobs.app_id \
                                   AND active.job_id <> config_sync_jobs.job_id \
                                   AND active.lease_expires_at_ms > ?3 \
                               )",
                            rusqlite::params![lease_owner, lease_expires_at_ms, now_ms, job_id],
                        )?;
                        if changed == 1 {
                            let sql = format!(
                                "SELECT {JOB_COLUMNS} FROM config_sync_jobs WHERE job_id = ?1"
                            );
                            let raw = transaction.query_row(&sql, [&job_id], raw_sqlite_job)?;
                            claimed.push(raw.into_typed()?);
                        }
                    }
                    transaction.commit()?;
                    claimed.sort_by(|left, right| {
                        (left.generation, left.created_at_ms, &left.job_id).cmp(&(
                            right.generation,
                            right.created_at_ms,
                            &right.job_id,
                        ))
                    });
                    Ok::<_, StorageError>(claimed)
                })
                .await
                .map_err(|error| StorageError::Task(error.to_string()))?
            }
            StorageStore::Postgres(postgres) => {
                let mut client = postgres.client().await?;
                let transaction = client.transaction().await?;
                // Lease comparisons use the database clock so replicas with
                // skewed wall clocks cannot steal or retain work incorrectly.
                let database_now_ms = postgres_transaction_now_ms(&transaction).await?;
                let lease_expires_at_ms = database_now_ms
                    .checked_add(lease_duration_ms)
                    .ok_or_else(|| {
                        StorageError::Invariant("lease expiration timestamp overflow".into())
                    })?;
                let search_limit = limit.saturating_mul(4).min(400);
                let candidates = transaction
                    .query(
                        "SELECT candidate.job_id, candidate.target, candidate.app_id \
                         FROM config_sync_jobs AS candidate \
                         WHERE candidate.status IN ('PENDING', 'FAILED') \
                           AND candidate.superseded_by_generation IS NULL \
                           AND candidate.next_attempt_at_ms <= $1 \
                           AND (candidate.lease_expires_at_ms IS NULL OR candidate.lease_expires_at_ms <= $1) \
                           AND NOT EXISTS ( \
                             SELECT 1 FROM config_sync_jobs AS active \
                             WHERE active.target = candidate.target \
                               AND active.app_id = candidate.app_id \
                               AND active.job_id <> candidate.job_id \
                               AND active.lease_expires_at_ms > $1 \
                           ) \
                         ORDER BY candidate.generation, candidate.created_at_ms, candidate.job_id \
                         LIMIT $2 FOR UPDATE SKIP LOCKED",
                        &[&database_now_ms, &search_limit],
                    )
                    .await?;
                let update_sql = format!(
                    "UPDATE config_sync_jobs AS jobs \
                     SET lease_owner = $1, lease_expires_at_ms = $2, \
                         attempt_count = jobs.attempt_count + 1, updated_at_ms = $3 \
                     WHERE jobs.job_id = $4 \
                       AND jobs.status IN ('PENDING', 'FAILED') \
                       AND jobs.superseded_by_generation IS NULL \
                       AND jobs.next_attempt_at_ms <= $3 \
                       AND (jobs.lease_expires_at_ms IS NULL OR jobs.lease_expires_at_ms <= $3) \
                       AND NOT EXISTS ( \
                         SELECT 1 FROM config_sync_jobs AS active \
                         WHERE active.target = jobs.target \
                           AND active.app_id = jobs.app_id \
                           AND active.job_id <> jobs.job_id \
                           AND active.lease_expires_at_ms > $3 \
                       ) \
                     RETURNING {CLAIMED_JOB_COLUMNS}"
                );
                let mut rows = Vec::new();
                for candidate in candidates {
                    if rows.len() >= limit as usize {
                        break;
                    }
                    let job_id: String = candidate.get(0);
                    let target: String = candidate.get(1);
                    let app_id: String = candidate.get(2);
                    // This transaction-scoped lock closes the READ COMMITTED
                    // window where two replicas could each lease a different
                    // ROUTE/ROUTE_ID row for the same app before either commit.
                    let acquired: bool = transaction
                        .query_one(
                            "SELECT pg_try_advisory_xact_lock(\
                               hashtextextended(json_build_array($1::TEXT, $2::TEXT)::TEXT, 0)\
                             )",
                            &[&target, &app_id],
                        )
                        .await?
                        .get(0);
                    if !acquired {
                        continue;
                    }
                    if let Some(row) = transaction
                        .query_opt(
                            &update_sql,
                            &[
                                &lease_owner,
                                &lease_expires_at_ms,
                                &database_now_ms,
                                &job_id,
                            ],
                        )
                        .await?
                    {
                        rows.push(row);
                    }
                }
                transaction.commit().await?;
                let mut claimed = rows
                    .iter()
                    .map(raw_postgres_job)
                    .map(RawConfigSyncJob::into_typed)
                    .collect::<Result<Vec<_>, _>>()?;
                claimed.sort_by(|left, right| {
                    (left.generation, left.created_at_ms, &left.job_id).cmp(&(
                        right.generation,
                        right.created_at_ms,
                        &right.job_id,
                    ))
                });
                Ok(claimed)
            }
        }
    }

    pub async fn mark_applied(
        store: &StorageStore,
        job_id: &str,
        lease_owner: &str,
        applied_at_ms: i64,
    ) -> Result<bool, StorageError> {
        validate_identifier("job_id", job_id)?;
        validate_identifier("lease_owner", lease_owner)?;
        validate_nonnegative("applied_at_ms", applied_at_ms)?;
        match store {
            StorageStore::Sqlite(sqlite) => {
                let path = sqlite.path().to_string();
                let job_id = job_id.to_string();
                let lease_owner = lease_owner.to_string();
                tokio::task::spawn_blocking(move || {
                    let connection = rusqlite::Connection::open(path)?;
                    let changed = connection.execute(
                        "UPDATE config_sync_jobs \
                         SET status = 'APPLIED', applied_at_ms = ?1, updated_at_ms = ?1, \
                             last_error = NULL, lease_owner = NULL, lease_expires_at_ms = NULL \
                         WHERE job_id = ?2 AND lease_owner = ?3 \
                           AND status IN ('PENDING', 'FAILED') \
                           AND superseded_by_generation IS NULL",
                        rusqlite::params![applied_at_ms, job_id, lease_owner],
                    )?;
                    Ok::<_, StorageError>(changed == 1)
                })
                .await
                .map_err(|error| StorageError::Task(error.to_string()))?
            }
            StorageStore::Postgres(postgres) => {
                let client = postgres.client().await?;
                let changed = client
                    .execute(
                        "UPDATE config_sync_jobs \
                         SET status = 'APPLIED', applied_at_ms = $1, updated_at_ms = $1, \
                             last_error = NULL, lease_owner = NULL, lease_expires_at_ms = NULL \
                         WHERE job_id = $2 AND lease_owner = $3 \
                           AND status IN ('PENDING', 'FAILED') \
                           AND superseded_by_generation IS NULL",
                        &[&applied_at_ms, &job_id, &lease_owner],
                    )
                    .await?;
                Ok(changed == 1)
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub async fn mark_failed(
        store: &StorageStore,
        job_id: &str,
        lease_owner: &str,
        error: &str,
        failed_at_ms: i64,
        next_attempt_at_ms: i64,
    ) -> Result<bool, StorageError> {
        validate_identifier("job_id", job_id)?;
        validate_identifier("lease_owner", lease_owner)?;
        validate_nonnegative("failed_at_ms", failed_at_ms)?;
        validate_nonnegative("next_attempt_at_ms", next_attempt_at_ms)?;
        if next_attempt_at_ms < failed_at_ms {
            return Err(StorageError::Invariant(
                "next_attempt_at_ms must not precede failed_at_ms".into(),
            ));
        }
        let error = bounded_error(error);
        match store {
            StorageStore::Sqlite(sqlite) => {
                let path = sqlite.path().to_string();
                let job_id = job_id.to_string();
                let lease_owner = lease_owner.to_string();
                tokio::task::spawn_blocking(move || {
                    let connection = rusqlite::Connection::open(path)?;
                    let changed = connection.execute(
                        "UPDATE config_sync_jobs \
                         SET status = 'FAILED', last_error = ?1, updated_at_ms = ?2, \
                             next_attempt_at_ms = ?3, applied_at_ms = NULL, \
                             lease_owner = NULL, lease_expires_at_ms = NULL \
                         WHERE job_id = ?4 AND lease_owner = ?5 \
                           AND status IN ('PENDING', 'FAILED') \
                           AND superseded_by_generation IS NULL",
                        rusqlite::params![
                            error,
                            failed_at_ms,
                            next_attempt_at_ms,
                            job_id,
                            lease_owner
                        ],
                    )?;
                    Ok::<_, StorageError>(changed == 1)
                })
                .await
                .map_err(|error| StorageError::Task(error.to_string()))?
            }
            StorageStore::Postgres(postgres) => {
                let mut client = postgres.client().await?;
                let transaction = client.transaction().await?;
                let database_failed_at_ms = postgres_transaction_now_ms(&transaction).await?;
                let database_next_attempt_at_ms =
                    rebase_schedule_ms(failed_at_ms, next_attempt_at_ms, database_failed_at_ms)?;
                let changed = transaction
                    .execute(
                        "UPDATE config_sync_jobs \
                         SET status = 'FAILED', last_error = $1, updated_at_ms = $2, \
                             next_attempt_at_ms = $3, applied_at_ms = NULL, \
                             lease_owner = NULL, lease_expires_at_ms = NULL \
                         WHERE job_id = $4 AND lease_owner = $5 \
                           AND status IN ('PENDING', 'FAILED') \
                           AND superseded_by_generation IS NULL",
                        &[
                            &error,
                            &database_failed_at_ms,
                            &database_next_attempt_at_ms,
                            &job_id,
                            &lease_owner,
                        ],
                    )
                    .await?;
                transaction.commit().await?;
                Ok(changed == 1)
            }
        }
    }

    /// Records an operation whose external result may be unknown while keeping
    /// the app-scoped lease until its original expiry. This quarantine prevents
    /// a retry/newer generation from overlapping a server-side write that may
    /// still complete after the client timed out or disconnected.
    #[allow(clippy::too_many_arguments)]
    pub async fn mark_failed_retaining_lease(
        store: &StorageStore,
        job_id: &str,
        lease_owner: &str,
        error: &str,
        failed_at_ms: i64,
        next_attempt_at_ms: i64,
    ) -> Result<bool, StorageError> {
        validate_identifier("job_id", job_id)?;
        validate_identifier("lease_owner", lease_owner)?;
        validate_nonnegative("failed_at_ms", failed_at_ms)?;
        validate_nonnegative("next_attempt_at_ms", next_attempt_at_ms)?;
        if next_attempt_at_ms < failed_at_ms {
            return Err(StorageError::Invariant(
                "next_attempt_at_ms must not precede failed_at_ms".into(),
            ));
        }
        let error = bounded_error(error);
        match store {
            StorageStore::Sqlite(sqlite) => {
                let path = sqlite.path().to_string();
                let job_id = job_id.to_string();
                let lease_owner = lease_owner.to_string();
                tokio::task::spawn_blocking(move || {
                    let connection = rusqlite::Connection::open(path)?;
                    let changed = connection.execute(
                        "UPDATE config_sync_jobs \
                         SET status = 'FAILED', last_error = ?1, updated_at_ms = ?2, \
                             next_attempt_at_ms = ?3, applied_at_ms = NULL \
                         WHERE job_id = ?4 AND lease_owner = ?5 \
                           AND status IN ('PENDING', 'FAILED')",
                        rusqlite::params![
                            error,
                            failed_at_ms,
                            next_attempt_at_ms,
                            job_id,
                            lease_owner
                        ],
                    )?;
                    Ok::<_, StorageError>(changed == 1)
                })
                .await
                .map_err(|error| StorageError::Task(error.to_string()))?
            }
            StorageStore::Postgres(postgres) => {
                let mut client = postgres.client().await?;
                let transaction = client.transaction().await?;
                let database_failed_at_ms = postgres_transaction_now_ms(&transaction).await?;
                let database_next_attempt_at_ms =
                    rebase_schedule_ms(failed_at_ms, next_attempt_at_ms, database_failed_at_ms)?;
                let changed = transaction
                    .execute(
                        "UPDATE config_sync_jobs \
                         SET status = 'FAILED', last_error = $1, updated_at_ms = $2, \
                             next_attempt_at_ms = $3, applied_at_ms = NULL \
                         WHERE job_id = $4 AND lease_owner = $5 \
                           AND status IN ('PENDING', 'FAILED')",
                        &[
                            &error,
                            &database_failed_at_ms,
                            &database_next_attempt_at_ms,
                            &job_id,
                            &lease_owner,
                        ],
                    )
                    .await?;
                transaction.commit().await?;
                Ok(changed == 1)
            }
        }
    }

    /// Releases a lease when a worker discovers that its job was superseded
    /// while external I/O was in flight. The superseded row remains as history,
    /// but newer work for the same resource can be claimed immediately.
    pub async fn release_lease(
        store: &StorageStore,
        job_id: &str,
        lease_owner: &str,
        released_at_ms: i64,
    ) -> Result<bool, StorageError> {
        validate_identifier("job_id", job_id)?;
        validate_identifier("lease_owner", lease_owner)?;
        validate_nonnegative("released_at_ms", released_at_ms)?;
        match store {
            StorageStore::Sqlite(sqlite) => {
                let path = sqlite.path().to_string();
                let job_id = job_id.to_string();
                let lease_owner = lease_owner.to_string();
                tokio::task::spawn_blocking(move || {
                    let connection = rusqlite::Connection::open(path)?;
                    let changed = connection.execute(
                        "UPDATE config_sync_jobs \
                         SET lease_owner = NULL, lease_expires_at_ms = NULL, updated_at_ms = ?1 \
                         WHERE job_id = ?2 AND lease_owner = ?3",
                        rusqlite::params![released_at_ms, job_id, lease_owner],
                    )?;
                    Ok::<_, StorageError>(changed == 1)
                })
                .await
                .map_err(|error| StorageError::Task(error.to_string()))?
            }
            StorageStore::Postgres(postgres) => {
                let client = postgres.client().await?;
                let changed = client
                    .execute(
                        "UPDATE config_sync_jobs \
                         SET lease_owner = NULL, lease_expires_at_ms = NULL, updated_at_ms = $1 \
                         WHERE job_id = $2 AND lease_owner = $3",
                        &[&released_at_ms, &job_id, &lease_owner],
                    )
                    .await?;
                Ok(changed == 1)
            }
        }
    }

    pub async fn ack_agent_generation(
        store: &StorageStore,
        agent_id: &str,
        applied_generation: i64,
        snapshot_hash: Option<&str>,
        applied_at_ms: i64,
        reported_at_ms: i64,
    ) -> Result<AgentConfigApply, StorageError> {
        validate_identifier("agent_id", agent_id)?;
        validate_nonnegative("applied_generation", applied_generation)?;
        validate_snapshot_hash(snapshot_hash)?;
        validate_nonnegative("applied_at_ms", applied_at_ms)?;
        validate_nonnegative("reported_at_ms", reported_at_ms)?;
        match store {
            StorageStore::Sqlite(sqlite) => {
                let path = sqlite.path().to_string();
                let agent_id = agent_id.to_string();
                let snapshot_hash = snapshot_hash.map(str::to_owned);
                tokio::task::spawn_blocking(move || {
                    let mut connection = rusqlite::Connection::open(path)?;
                    let transaction = connection.transaction_with_behavior(
                        rusqlite::TransactionBehavior::Immediate,
                    )?;
                    let sql =
                        "INSERT INTO agent_config_applies \
                         (agent_id, applied_generation, snapshot_hash, applied_at_ms, reported_at_ms) \
                         VALUES (?1, ?2, ?3, ?4, ?5) \
                         ON CONFLICT(agent_id) DO UPDATE SET \
                           applied_generation = CASE \
                             WHEN excluded.applied_generation > agent_config_applies.applied_generation \
                             THEN excluded.applied_generation \
                             ELSE agent_config_applies.applied_generation END, \
                           snapshot_hash = CASE \
                             WHEN excluded.applied_generation > agent_config_applies.applied_generation \
                             THEN excluded.snapshot_hash \
                             ELSE COALESCE(agent_config_applies.snapshot_hash, excluded.snapshot_hash) END, \
                           applied_at_ms = CASE \
                             WHEN excluded.applied_generation > agent_config_applies.applied_generation \
                             THEN excluded.applied_at_ms \
                             ELSE agent_config_applies.applied_at_ms END, \
                           reported_at_ms = MAX( \
                             agent_config_applies.reported_at_ms, excluded.reported_at_ms) \
                         WHERE excluded.applied_generation > agent_config_applies.applied_generation \
                            OR (excluded.applied_generation = agent_config_applies.applied_generation \
                                AND (excluded.snapshot_hash IS NULL \
                                     OR agent_config_applies.snapshot_hash IS NULL \
                                     OR excluded.snapshot_hash = agent_config_applies.snapshot_hash))";
                    transaction.execute(
                        sql,
                        rusqlite::params![
                            agent_id,
                            applied_generation,
                            snapshot_hash,
                            applied_at_ms,
                            reported_at_ms
                        ],
                    )?;
                    let select_sql = format!(
                        "SELECT {AGENT_APPLY_COLUMNS} \
                         FROM agent_config_applies WHERE agent_id = ?1"
                    );
                    let apply = transaction.query_row(&select_sql, [&agent_id], sqlite_agent_apply)?;
                    if apply.applied_generation == applied_generation
                        && apply
                            .snapshot_hash
                            .as_deref()
                            .zip(snapshot_hash.as_deref())
                            .is_some_and(|(stored, reported)| stored != reported)
                    {
                        return Err(StorageError::Conflict(format!(
                            "Agent {agent_id} reported a different snapshot hash for generation {applied_generation}"
                        )));
                    }
                    transaction.commit()?;
                    Ok::<_, StorageError>(apply)
                })
                .await
                .map_err(|error| StorageError::Task(error.to_string()))?
            }
            StorageStore::Postgres(postgres) => {
                let mut client = postgres.client().await?;
                let transaction = client.transaction().await?;
                let sql =
                    "INSERT INTO agent_config_applies \
                     (agent_id, applied_generation, snapshot_hash, applied_at_ms, reported_at_ms) \
                     VALUES ($1, $2, $3, $4, $5) \
                     ON CONFLICT(agent_id) DO UPDATE SET \
                       applied_generation = CASE \
                         WHEN EXCLUDED.applied_generation > agent_config_applies.applied_generation \
                         THEN EXCLUDED.applied_generation \
                         ELSE agent_config_applies.applied_generation END, \
                       snapshot_hash = CASE \
                         WHEN EXCLUDED.applied_generation > agent_config_applies.applied_generation \
                         THEN EXCLUDED.snapshot_hash \
                         ELSE COALESCE(agent_config_applies.snapshot_hash, EXCLUDED.snapshot_hash) END, \
                       applied_at_ms = CASE \
                         WHEN EXCLUDED.applied_generation > agent_config_applies.applied_generation \
                         THEN EXCLUDED.applied_at_ms \
                         ELSE agent_config_applies.applied_at_ms END, \
                       reported_at_ms = GREATEST( \
                         agent_config_applies.reported_at_ms, EXCLUDED.reported_at_ms) \
                     WHERE EXCLUDED.applied_generation > agent_config_applies.applied_generation \
                        OR (EXCLUDED.applied_generation = agent_config_applies.applied_generation \
                            AND (EXCLUDED.snapshot_hash IS NULL \
                                 OR agent_config_applies.snapshot_hash IS NULL \
                                 OR EXCLUDED.snapshot_hash = agent_config_applies.snapshot_hash))";
                transaction
                    .execute(
                        sql,
                        &[
                            &agent_id,
                            &applied_generation,
                            &snapshot_hash,
                            &applied_at_ms,
                            &reported_at_ms,
                        ],
                    )
                    .await?;
                let row = transaction
                    .query_one(
                        &format!(
                            "SELECT {AGENT_APPLY_COLUMNS} \
                             FROM agent_config_applies WHERE agent_id = $1"
                        ),
                        &[&agent_id],
                    )
                    .await?;
                let apply = postgres_agent_apply(&row);
                if apply.applied_generation == applied_generation
                    && apply
                        .snapshot_hash
                        .as_deref()
                        .zip(snapshot_hash)
                        .is_some_and(|(stored, reported)| stored != reported)
                {
                    return Err(StorageError::Conflict(format!(
                        "Agent {agent_id} reported a different snapshot hash for generation {applied_generation}"
                    )));
                }
                transaction.commit().await?;
                Ok(apply)
            }
        }
    }

    pub async fn get_job(
        store: &StorageStore,
        job_id: &str,
    ) -> Result<Option<ConfigSyncJob>, StorageError> {
        validate_identifier("job_id", job_id)?;
        match store {
            StorageStore::Sqlite(sqlite) => {
                let path = sqlite.path().to_string();
                let job_id = job_id.to_string();
                tokio::task::spawn_blocking(move || {
                    use rusqlite::OptionalExtension;

                    let connection = rusqlite::Connection::open(path)?;
                    let sql =
                        format!("SELECT {JOB_COLUMNS} FROM config_sync_jobs WHERE job_id = ?1");
                    let raw = connection
                        .query_row(&sql, [&job_id], raw_sqlite_job)
                        .optional()?;
                    raw.map(RawConfigSyncJob::into_typed).transpose()
                })
                .await
                .map_err(|error| StorageError::Task(error.to_string()))?
            }
            StorageStore::Postgres(postgres) => {
                let client = postgres.client().await?;
                let sql = format!("SELECT {JOB_COLUMNS} FROM config_sync_jobs WHERE job_id = $1");
                client
                    .query_opt(&sql, &[&job_id])
                    .await?
                    .as_ref()
                    .map(raw_postgres_job)
                    .map(RawConfigSyncJob::into_typed)
                    .transpose()
            }
        }
    }

    pub async fn list_jobs(store: &StorageStore) -> Result<Vec<ConfigSyncJob>, StorageError> {
        match store {
            StorageStore::Sqlite(sqlite) => {
                let path = sqlite.path().to_string();
                tokio::task::spawn_blocking(move || {
                    let connection = rusqlite::Connection::open(path)?;
                    let sql = format!(
                        "SELECT {JOB_COLUMNS} FROM config_sync_jobs \
                         ORDER BY generation, created_at_ms, job_id"
                    );
                    let mut statement = connection.prepare(&sql)?;
                    let rows = statement.query_map([], raw_sqlite_job)?;
                    let mut jobs = Vec::new();
                    for row in rows {
                        jobs.push(row?.into_typed()?);
                    }
                    Ok::<_, StorageError>(jobs)
                })
                .await
                .map_err(|error| StorageError::Task(error.to_string()))?
            }
            StorageStore::Postgres(postgres) => {
                let client = postgres.client().await?;
                let sql = format!(
                    "SELECT {JOB_COLUMNS} FROM config_sync_jobs \
                     ORDER BY generation, created_at_ms, job_id"
                );
                client
                    .query(&sql, &[])
                    .await?
                    .iter()
                    .map(raw_postgres_job)
                    .map(RawConfigSyncJob::into_typed)
                    .collect()
            }
        }
    }

    pub async fn get_agent_apply(
        store: &StorageStore,
        agent_id: &str,
    ) -> Result<Option<AgentConfigApply>, StorageError> {
        validate_identifier("agent_id", agent_id)?;
        match store {
            StorageStore::Sqlite(sqlite) => {
                let path = sqlite.path().to_string();
                let agent_id = agent_id.to_string();
                tokio::task::spawn_blocking(move || {
                    use rusqlite::OptionalExtension;

                    let connection = rusqlite::Connection::open(path)?;
                    let sql = format!(
                        "SELECT {AGENT_APPLY_COLUMNS} \
                         FROM agent_config_applies WHERE agent_id = ?1"
                    );
                    connection
                        .query_row(&sql, [&agent_id], sqlite_agent_apply)
                        .optional()
                        .map_err(StorageError::from)
                })
                .await
                .map_err(|error| StorageError::Task(error.to_string()))?
            }
            StorageStore::Postgres(postgres) => {
                let client = postgres.client().await?;
                let sql = format!(
                    "SELECT {AGENT_APPLY_COLUMNS} \
                     FROM agent_config_applies WHERE agent_id = $1"
                );
                Ok(client
                    .query_opt(&sql, &[&agent_id])
                    .await?
                    .as_ref()
                    .map(postgres_agent_apply))
            }
        }
    }

    pub async fn list_agent_applies(
        store: &StorageStore,
    ) -> Result<Vec<AgentConfigApply>, StorageError> {
        match store {
            StorageStore::Sqlite(sqlite) => {
                let path = sqlite.path().to_string();
                tokio::task::spawn_blocking(move || {
                    let connection = rusqlite::Connection::open(path)?;
                    let sql = format!(
                        "SELECT {AGENT_APPLY_COLUMNS} \
                         FROM agent_config_applies ORDER BY agent_id"
                    );
                    let mut statement = connection.prepare(&sql)?;
                    let rows = statement.query_map([], sqlite_agent_apply)?;
                    let mut applies = Vec::new();
                    for row in rows {
                        applies.push(row?);
                    }
                    Ok::<_, StorageError>(applies)
                })
                .await
                .map_err(|error| StorageError::Task(error.to_string()))?
            }
            StorageStore::Postgres(postgres) => {
                let client = postgres.client().await?;
                let sql = format!(
                    "SELECT {AGENT_APPLY_COLUMNS} \
                     FROM agent_config_applies ORDER BY agent_id"
                );
                Ok(client
                    .query(&sql, &[])
                    .await?
                    .iter()
                    .map(postgres_agent_apply)
                    .collect())
            }
        }
    }

    pub async fn prune_agent_applies_before(
        store: &StorageStore,
        reported_before_ms: i64,
    ) -> Result<u64, StorageError> {
        validate_nonnegative("reported_before_ms", reported_before_ms)?;
        match store {
            StorageStore::Sqlite(sqlite) => {
                let path = sqlite.path().to_string();
                tokio::task::spawn_blocking(move || {
                    let connection = rusqlite::Connection::open(path)?;
                    let changed = connection.execute(
                        "DELETE FROM agent_config_applies \
                         WHERE reported_at_ms < ?1 AND snapshot_hash IS NULL",
                        [reported_before_ms],
                    )?;
                    u64::try_from(changed).map_err(|_| {
                        StorageError::Invariant("pruned Agent ACK count exceeds u64".into())
                    })
                })
                .await
                .map_err(|error| StorageError::Task(error.to_string()))?
            }
            StorageStore::Postgres(postgres) => {
                let client = postgres.client().await?;
                Ok(client
                    .execute(
                        "DELETE FROM agent_config_applies \
                         WHERE reported_at_ms < $1 AND snapshot_hash IS NULL",
                        &[&reported_before_ms],
                    )
                    .await?)
            }
        }
    }

    pub(crate) fn bump_generation_sqlite_transaction(
        transaction: &rusqlite::Transaction<'_>,
        updated_at_ms: i64,
    ) -> Result<i64, StorageError> {
        validate_nonnegative("updated_at_ms", updated_at_ms)?;
        let generation = transaction.query_row(
            "UPDATE config_state \
             SET generation = generation + 1, updated_at_ms = ?1 \
             WHERE id = 1 RETURNING generation",
            [updated_at_ms],
            |row| row.get(0),
        )?;
        Ok(generation)
    }

    pub(crate) async fn bump_generation_postgres_transaction(
        transaction: &tokio_postgres::Transaction<'_>,
        updated_at_ms: i64,
    ) -> Result<i64, StorageError> {
        validate_nonnegative("updated_at_ms", updated_at_ms)?;
        let row = transaction
            .query_one(
                "UPDATE config_state \
                 SET generation = generation + 1, updated_at_ms = $1 \
                 WHERE id = 1 RETURNING generation",
                &[&updated_at_ms],
            )
            .await?;
        Ok(row.get(0))
    }

    pub(crate) fn enqueue_sqlite_transaction(
        transaction: &rusqlite::Transaction<'_>,
        draft: &ConfigSyncJobDraft,
        now_ms: i64,
    ) -> Result<ConfigSyncJob, StorageError> {
        validate_draft(draft, now_ms)?;
        let newer_generation: Option<i64> = transaction.query_row(
            "SELECT MAX(generation) FROM config_sync_jobs \
             WHERE target = ?1 AND resource_type = ?2 AND resource_id = ?3",
            rusqlite::params![draft.target, draft.resource_type, draft.resource_id],
            |row| row.get(0),
        )?;
        if newer_generation.is_some_and(|generation| generation > draft.generation) {
            return Err(StorageError::Invariant(format!(
                "cannot enqueue stale generation {} for {}/{}/{}; newer generation exists",
                draft.generation, draft.target, draft.resource_type, draft.resource_id
            )));
        }

        transaction.execute(
            "UPDATE config_sync_jobs \
             SET superseded_by_generation = ?1, updated_at_ms = ?2 \
             WHERE target = ?3 AND resource_type = ?4 AND resource_id = ?5 \
               AND generation < ?1 \
               AND superseded_by_generation IS NULL",
            rusqlite::params![
                draft.generation,
                now_ms,
                draft.target,
                draft.resource_type,
                draft.resource_id
            ],
        )?;

        let job_id = uuid::Uuid::new_v4().to_string();
        transaction.execute(
            "INSERT INTO config_sync_jobs ( \
               job_id, generation, target, resource_type, resource_id, app_id, operation, \
               payload_json, status, attempt_count, next_attempt_at_ms, created_at_ms, updated_at_ms \
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, 'PENDING', 0, ?9, ?10, ?10) \
             ON CONFLICT(target, resource_type, resource_id, generation) DO NOTHING",
            rusqlite::params![
                job_id,
                draft.generation,
                draft.target,
                draft.resource_type,
                draft.resource_id,
                draft.app_id,
                draft.operation.as_str(),
                draft.payload_json,
                draft.next_attempt_at_ms,
                now_ms
            ],
        )?;
        let sql = format!(
            "SELECT {JOB_COLUMNS} FROM config_sync_jobs \
             WHERE target = ?1 AND resource_type = ?2 AND resource_id = ?3 AND generation = ?4"
        );
        let job = transaction
            .query_row(
                &sql,
                rusqlite::params![
                    draft.target,
                    draft.resource_type,
                    draft.resource_id,
                    draft.generation
                ],
                raw_sqlite_job,
            )?
            .into_typed()?;
        if !job_matches_draft(&job, draft) {
            return Err(StorageError::Invariant(format!(
                "conflicting config sync job for {}/{}/{} generation {}",
                draft.target, draft.resource_type, draft.resource_id, draft.generation
            )));
        }
        Ok(job)
    }

    pub(crate) async fn enqueue_postgres_transaction(
        transaction: &tokio_postgres::Transaction<'_>,
        draft: &ConfigSyncJobDraft,
        now_ms: i64,
    ) -> Result<ConfigSyncJob, StorageError> {
        validate_draft(draft, now_ms)?;
        transaction
            .query_one(
                "SELECT pg_advisory_xact_lock(\
                   hashtextextended(json_build_array($1::TEXT, $2::TEXT)::TEXT, 0)\
                 )",
                &[&draft.target, &draft.app_id],
            )
            .await?;
        let database_now_ms = postgres_transaction_now_ms(transaction).await?;
        let database_next_attempt_at_ms =
            rebase_schedule_ms(now_ms, draft.next_attempt_at_ms, database_now_ms)?;
        let newer_generation: Option<i64> = transaction
            .query_one(
                "SELECT MAX(generation) FROM config_sync_jobs \
                 WHERE target = $1 AND resource_type = $2 AND resource_id = $3",
                &[&draft.target, &draft.resource_type, &draft.resource_id],
            )
            .await?
            .get(0);
        if newer_generation.is_some_and(|generation| generation > draft.generation) {
            return Err(StorageError::Invariant(format!(
                "cannot enqueue stale generation {} for {}/{}/{}; newer generation exists",
                draft.generation, draft.target, draft.resource_type, draft.resource_id
            )));
        }

        transaction
            .execute(
                "UPDATE config_sync_jobs \
                 SET superseded_by_generation = $1, updated_at_ms = $2 \
                 WHERE target = $3 AND resource_type = $4 AND resource_id = $5 \
               AND generation < $1 \
               AND superseded_by_generation IS NULL",
                &[
                    &draft.generation,
                    &database_now_ms,
                    &draft.target,
                    &draft.resource_type,
                    &draft.resource_id,
                ],
            )
            .await?;

        let job_id = uuid::Uuid::new_v4().to_string();
        transaction
            .execute(
                "INSERT INTO config_sync_jobs ( \
                   job_id, generation, target, resource_type, resource_id, app_id, operation, \
                   payload_json, status, attempt_count, next_attempt_at_ms, created_at_ms, updated_at_ms \
                 ) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, 'PENDING', 0, $9, $10, $10) \
                 ON CONFLICT(target, resource_type, resource_id, generation) DO NOTHING",
                &[
                    &job_id,
                    &draft.generation,
                    &draft.target,
                    &draft.resource_type,
                    &draft.resource_id,
                    &draft.app_id,
                    &draft.operation.as_str(),
                    &draft.payload_json,
                    &database_next_attempt_at_ms,
                    &database_now_ms,
                ],
            )
            .await?;
        let sql = format!(
            "SELECT {JOB_COLUMNS} FROM config_sync_jobs \
             WHERE target = $1 AND resource_type = $2 AND resource_id = $3 AND generation = $4"
        );
        let row = transaction
            .query_one(
                &sql,
                &[
                    &draft.target,
                    &draft.resource_type,
                    &draft.resource_id,
                    &draft.generation,
                ],
            )
            .await?;
        let job = raw_postgres_job(&row).into_typed()?;
        if !job_matches_draft(&job, draft) {
            return Err(StorageError::Invariant(format!(
                "conflicting config sync job for {}/{}/{} generation {}",
                draft.target, draft.resource_type, draft.resource_id, draft.generation
            )));
        }
        Ok(job)
    }
}
