use serde::{Deserialize, Serialize};

use crate::{StorageError, StorageStore};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FaultEventRecord {
    pub id: String,
    pub ts_ms: i64,
    pub service: String,
    pub event_type: String,
    pub severity: String,
    pub path: String,
    pub method: String,
    pub latency_ms: i64,
    pub baseline_ms: i64,
    pub threshold_ms: i64,
    pub status_code: i64,
    pub result: String,
    pub trace_id: String,
    pub source: String,
    pub resolved_at_ms: Option<i64>,
    pub meta_json: String,
}

#[derive(Debug, Clone, Default)]
pub struct FaultEventFilter {
    pub from_ts_ms: Option<i64>,
    pub to_ts_ms: Option<i64>,
    pub service: Option<String>,
    pub event_type: Option<String>,
    pub severity: Option<String>,
    pub result: Option<String>,
    pub source: Option<String>,
    pub limit: i64,
}

pub struct FaultEventsStore;

impl FaultEventsStore {
    pub async fn insert(store: &StorageStore, row: &FaultEventRecord) -> Result<(), StorageError> {
        match store {
            StorageStore::Sqlite(sqlite) => {
                let sqlite = sqlite.clone();
                let r = row.clone();
                tokio::task::spawn_blocking(move || {
                    let conn = rusqlite::Connection::open(sqlite.path())?;
                    conn.execute(
                        r#"INSERT INTO fault_events
                        (id, ts_ms, service, event_type, severity, path, method, latency_ms, baseline_ms, threshold_ms, status_code, result, trace_id, source, resolved_at_ms, meta_json)
                        VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16)"#,
                        rusqlite::params![
                            r.id, r.ts_ms, r.service, r.event_type, r.severity, r.path, r.method, r.latency_ms, r.baseline_ms,
                            r.threshold_ms, r.status_code, r.result, r.trace_id, r.source, r.resolved_at_ms, r.meta_json
                        ],
                    )?;
                    Ok::<_, StorageError>(())
                })
                .await
                .map_err(|e| StorageError::Task(e.to_string()))?
            }
            StorageStore::Postgres(pg) => {
                let client = pg.client().await?;
                client.execute(
                    r#"INSERT INTO fault_events
                    (id, ts_ms, service, event_type, severity, path, method, latency_ms, baseline_ms, threshold_ms, status_code, result, trace_id, source, resolved_at_ms, meta_json)
                    VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13,$14,$15,$16)"#,
                    &[
                        &row.id, &row.ts_ms, &row.service, &row.event_type, &row.severity, &row.path, &row.method,
                        &row.latency_ms, &row.baseline_ms, &row.threshold_ms, &row.status_code, &row.result,
                        &row.trace_id, &row.source, &row.resolved_at_ms, &row.meta_json,
                    ],
                ).await?;
                Ok(())
            }
        }
    }

    pub async fn list(
        store: &StorageStore,
        f: &FaultEventFilter,
    ) -> Result<Vec<FaultEventRecord>, StorageError> {
        let limit = if f.limit <= 0 { 200 } else { f.limit.min(1000) };
        match store {
            StorageStore::Sqlite(sqlite) => {
                let sqlite = sqlite.clone();
                let ff = f.clone();
                tokio::task::spawn_blocking(move || {
                    let conn = rusqlite::Connection::open(sqlite.path())?;
                    let mut conds = Vec::<String>::new();
                    let mut vals = Vec::<rusqlite::types::Value>::new();
                    if let Some(v) = ff.from_ts_ms { conds.push("ts_ms >= ?".into()); vals.push(v.into()); }
                    if let Some(v) = ff.to_ts_ms { conds.push("ts_ms <= ?".into()); vals.push(v.into()); }
                    if let Some(v) = ff.service { conds.push("service = ?".into()); vals.push(v.into()); }
                    if let Some(v) = ff.event_type { conds.push("event_type = ?".into()); vals.push(v.into()); }
                    if let Some(v) = ff.severity { conds.push("severity = ?".into()); vals.push(v.into()); }
                    if let Some(v) = ff.result { conds.push("result = ?".into()); vals.push(v.into()); }
                    if let Some(v) = ff.source { conds.push("source = ?".into()); vals.push(v.into()); }
                    let mut sql = "SELECT id, ts_ms, service, event_type, severity, path, method, latency_ms, baseline_ms, threshold_ms, status_code, result, trace_id, source, resolved_at_ms, meta_json FROM fault_events".to_string();
                    if !conds.is_empty() {
                        sql.push_str(" WHERE ");
                        sql.push_str(&conds.join(" AND "));
                    }
                    sql.push_str(" ORDER BY ts_ms DESC LIMIT ?");
                    vals.push(limit.into());
                    let mut stmt = conn.prepare(&sql)?;
                    let rows = stmt.query_map(rusqlite::params_from_iter(vals), |r| {
                        Ok(FaultEventRecord {
                            id: r.get(0)?, ts_ms: r.get(1)?, service: r.get(2)?, event_type: r.get(3)?, severity: r.get(4)?,
                            path: r.get(5)?, method: r.get(6)?, latency_ms: r.get(7)?, baseline_ms: r.get(8)?,
                            threshold_ms: r.get(9)?, status_code: r.get(10)?, result: r.get(11)?, trace_id: r.get(12)?,
                            source: r.get(13)?, resolved_at_ms: r.get(14)?, meta_json: r.get(15)?,
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
                let sql = r#"
                    SELECT id, ts_ms, service, event_type, severity, path, method, latency_ms, baseline_ms, threshold_ms, status_code, result, trace_id, source, resolved_at_ms, meta_json
                    FROM fault_events
                    WHERE ($1::bigint IS NULL OR ts_ms >= $1)
                      AND ($2::bigint IS NULL OR ts_ms <= $2)
                      AND ($3::text IS NULL OR service = $3)
                      AND ($4::text IS NULL OR event_type = $4)
                      AND ($5::text IS NULL OR severity = $5)
                      AND ($6::text IS NULL OR result = $6)
                      AND ($7::text IS NULL OR source = $7)
                    ORDER BY ts_ms DESC
                    LIMIT $8
                "#;
                let rows = client
                    .query(
                        sql,
                        &[
                            &f.from_ts_ms,
                            &f.to_ts_ms,
                            &f.service,
                            &f.event_type,
                            &f.severity,
                            &f.result,
                            &f.source,
                            &limit,
                        ],
                    )
                    .await?;
                Ok(rows
                    .into_iter()
                    .map(|r| FaultEventRecord {
                        id: r.get(0),
                        ts_ms: r.get(1),
                        service: r.get(2),
                        event_type: r.get(3),
                        severity: r.get(4),
                        path: r.get(5),
                        method: r.get(6),
                        latency_ms: r.get(7),
                        baseline_ms: r.get(8),
                        threshold_ms: r.get(9),
                        status_code: r.get(10),
                        result: r.get(11),
                        trace_id: r.get(12),
                        source: r.get(13),
                        resolved_at_ms: r.get(14),
                        meta_json: r.get(15),
                    })
                    .collect())
            }
        }
    }
}
