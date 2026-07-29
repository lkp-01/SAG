use serde::{Deserialize, Serialize};

use crate::{StorageError, StorageStore};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AppMetricMinuteRecord {
    pub ts_minute: i64,
    pub app_id: String,
    pub request_count: i64,
    pub pv_count: i64,
    pub uv_count: i64,
    pub unique_ip_count: i64,
    pub err4xx_count: i64,
    pub err5xx_count: i64,
    pub qps_avg: f64,
}

pub struct AppMetricsStore;

impl AppMetricsStore {
    pub async fn upsert_minute(
        store: &StorageStore,
        record: &AppMetricMinuteRecord,
    ) -> Result<(), StorageError> {
        match store {
            StorageStore::Sqlite(sqlite) => {
                let sqlite = sqlite.clone();
                let r = record.clone();
                tokio::task::spawn_blocking(move || {
                    let conn = rusqlite::Connection::open(sqlite.path())?;
                    conn.execute(
                        r#"INSERT INTO app_metrics_minute
                           (ts_minute, app_id, request_count, pv_count, uv_count, unique_ip_count, err4xx_count, err5xx_count, qps_avg)
                           VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)
                           ON CONFLICT(ts_minute, app_id) DO UPDATE SET
                             request_count=excluded.request_count,
                             pv_count=excluded.pv_count,
                             uv_count=excluded.uv_count,
                             unique_ip_count=excluded.unique_ip_count,
                             err4xx_count=excluded.err4xx_count,
                             err5xx_count=excluded.err5xx_count,
                             qps_avg=excluded.qps_avg"#,
                        rusqlite::params![
                            r.ts_minute,
                            r.app_id,
                            r.request_count,
                            r.pv_count,
                            r.uv_count,
                            r.unique_ip_count,
                            r.err4xx_count,
                            r.err5xx_count,
                            r.qps_avg
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
                        r#"INSERT INTO app_metrics_minute
                           (ts_minute, app_id, request_count, pv_count, uv_count, unique_ip_count, err4xx_count, err5xx_count, qps_avg)
                           VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9)
                           ON CONFLICT(ts_minute, app_id) DO UPDATE SET
                             request_count=excluded.request_count,
                             pv_count=excluded.pv_count,
                             uv_count=excluded.uv_count,
                             unique_ip_count=excluded.unique_ip_count,
                             err4xx_count=excluded.err4xx_count,
                             err5xx_count=excluded.err5xx_count,
                             qps_avg=excluded.qps_avg"#,
                        &[
                            &record.ts_minute,
                            &record.app_id,
                            &record.request_count,
                            &record.pv_count,
                            &record.uv_count,
                            &record.unique_ip_count,
                            &record.err4xx_count,
                            &record.err5xx_count,
                            &record.qps_avg,
                        ],
                    )
                    .await?;
                Ok(())
            }
        }
    }

    pub async fn latest_by_app_ids(
        store: &StorageStore,
        app_ids: &[String],
    ) -> Result<Vec<AppMetricMinuteRecord>, StorageError> {
        if app_ids.is_empty() {
            return Ok(Vec::new());
        }
        match store {
            StorageStore::Sqlite(sqlite) => {
                let sqlite = sqlite.clone();
                let ids = app_ids.to_vec();
                tokio::task::spawn_blocking(move || {
                    let conn = rusqlite::Connection::open(sqlite.path())?;
                    let placeholders = (0..ids.len()).map(|_| "?".to_string()).collect::<Vec<_>>().join(",");
                    let sql = format!(
                        r#"SELECT m.ts_minute, m.app_id, m.request_count, m.pv_count, m.uv_count, m.unique_ip_count, m.err4xx_count, m.err5xx_count, m.qps_avg
                           FROM app_metrics_minute m
                           JOIN (
                               SELECT app_id, MAX(ts_minute) AS max_ts
                               FROM app_metrics_minute
                               WHERE app_id IN ({})
                               GROUP BY app_id
                           ) t ON t.app_id = m.app_id AND t.max_ts = m.ts_minute
                           ORDER BY m.app_id ASC"#,
                        placeholders
                    );
                    let mut stmt = conn.prepare(&sql)?;
                    let rows = stmt.query_map(rusqlite::params_from_iter(ids), |row| {
                        Ok(AppMetricMinuteRecord {
                            ts_minute: row.get(0)?,
                            app_id: row.get(1)?,
                            request_count: row.get(2)?,
                            pv_count: row.get(3)?,
                            uv_count: row.get(4)?,
                            unique_ip_count: row.get(5)?,
                            err4xx_count: row.get(6)?,
                            err5xx_count: row.get(7)?,
                            qps_avg: row.get(8)?,
                        })
                    })?;
                    let mut out = Vec::new();
                    for row in rows {
                        out.push(row?);
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
                        r#"SELECT DISTINCT ON (app_id)
                               ts_minute, app_id, request_count, pv_count, uv_count, unique_ip_count, err4xx_count, err5xx_count, qps_avg
                           FROM app_metrics_minute
                           WHERE app_id = ANY($1)
                           ORDER BY app_id, ts_minute DESC"#,
                        &[&app_ids],
                    )
                    .await?;
                Ok(rows
                    .into_iter()
                    .map(|r| AppMetricMinuteRecord {
                        ts_minute: r.get(0),
                        app_id: r.get(1),
                        request_count: r.get(2),
                        pv_count: r.get(3),
                        uv_count: r.get(4),
                        unique_ip_count: r.get(5),
                        err4xx_count: r.get(6),
                        err5xx_count: r.get(7),
                        qps_avg: r.get(8),
                    })
                    .collect())
            }
        }
    }

    pub async fn list_by_app_range(
        store: &StorageStore,
        app_id: &str,
        from_ts_minute: i64,
        to_ts_minute: i64,
    ) -> Result<Vec<AppMetricMinuteRecord>, StorageError> {
        match store {
            StorageStore::Sqlite(sqlite) => {
                let sqlite = sqlite.clone();
                let app_id = app_id.to_string();
                tokio::task::spawn_blocking(move || {
                    let conn = rusqlite::Connection::open(sqlite.path())?;
                    let mut stmt = conn.prepare(
                        r#"SELECT ts_minute, app_id, request_count, pv_count, uv_count, unique_ip_count, err4xx_count, err5xx_count, qps_avg
                           FROM app_metrics_minute
                           WHERE app_id = ?1 AND ts_minute >= ?2 AND ts_minute <= ?3
                           ORDER BY ts_minute ASC"#,
                    )?;
                    let rows = stmt.query_map(
                        rusqlite::params![app_id, from_ts_minute, to_ts_minute],
                        |row| {
                            Ok(AppMetricMinuteRecord {
                                ts_minute: row.get(0)?,
                                app_id: row.get(1)?,
                                request_count: row.get(2)?,
                                pv_count: row.get(3)?,
                                uv_count: row.get(4)?,
                                unique_ip_count: row.get(5)?,
                                err4xx_count: row.get(6)?,
                                err5xx_count: row.get(7)?,
                                qps_avg: row.get(8)?,
                            })
                        },
                    )?;
                    let mut out = Vec::new();
                    for row in rows {
                        out.push(row?);
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
                        r#"SELECT ts_minute, app_id, request_count, pv_count, uv_count, unique_ip_count, err4xx_count, err5xx_count, qps_avg
                           FROM app_metrics_minute
                           WHERE app_id = $1 AND ts_minute >= $2 AND ts_minute <= $3
                           ORDER BY ts_minute ASC"#,
                        &[&app_id, &from_ts_minute, &to_ts_minute],
                    )
                    .await?;
                Ok(rows
                    .into_iter()
                    .map(|r| AppMetricMinuteRecord {
                        ts_minute: r.get(0),
                        app_id: r.get(1),
                        request_count: r.get(2),
                        pv_count: r.get(3),
                        uv_count: r.get(4),
                        unique_ip_count: r.get(5),
                        err4xx_count: r.get(6),
                        err5xx_count: r.get(7),
                        qps_avg: r.get(8),
                    })
                    .collect())
            }
        }
    }

    pub async fn list_by_apps_range(
        store: &StorageStore,
        app_ids: &[String],
        from_ts_minute: i64,
        to_ts_minute: i64,
    ) -> Result<Vec<AppMetricMinuteRecord>, StorageError> {
        if app_ids.is_empty() {
            return Ok(Vec::new());
        }
        match store {
            StorageStore::Sqlite(sqlite) => {
                let sqlite = sqlite.clone();
                let ids = app_ids.to_vec();
                tokio::task::spawn_blocking(move || {
                    let conn = rusqlite::Connection::open(sqlite.path())?;
                    let placeholders = (0..ids.len()).map(|_| "?".to_string()).collect::<Vec<_>>().join(",");
                    let sql = format!(
                        "SELECT ts_minute, app_id, request_count, pv_count, uv_count, unique_ip_count, err4xx_count, err5xx_count, qps_avg
                         FROM app_metrics_minute
                         WHERE app_id IN ({}) AND ts_minute >= ? AND ts_minute <= ?
                         ORDER BY app_id ASC, ts_minute ASC",
                        placeholders
                    );
                    let mut params: Vec<rusqlite::types::Value> = ids.into_iter().map(Into::into).collect();
                    params.push(from_ts_minute.into());
                    params.push(to_ts_minute.into());
                    let mut stmt = conn.prepare(&sql)?;
                    let rows = stmt.query_map(rusqlite::params_from_iter(params), |row| {
                        Ok(AppMetricMinuteRecord {
                            ts_minute: row.get(0)?,
                            app_id: row.get(1)?,
                            request_count: row.get(2)?,
                            pv_count: row.get(3)?,
                            uv_count: row.get(4)?,
                            unique_ip_count: row.get(5)?,
                            err4xx_count: row.get(6)?,
                            err5xx_count: row.get(7)?,
                            qps_avg: row.get(8)?,
                        })
                    })?;
                    let mut out = Vec::new();
                    for row in rows {
                        out.push(row?);
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
                        r#"SELECT ts_minute, app_id, request_count, pv_count, uv_count, unique_ip_count, err4xx_count, err5xx_count, qps_avg
                           FROM app_metrics_minute
                           WHERE app_id = ANY($1) AND ts_minute >= $2 AND ts_minute <= $3
                           ORDER BY app_id ASC, ts_minute ASC"#,
                        &[&app_ids, &from_ts_minute, &to_ts_minute],
                    )
                    .await?;
                Ok(rows
                    .into_iter()
                    .map(|r| AppMetricMinuteRecord {
                        ts_minute: r.get(0),
                        app_id: r.get(1),
                        request_count: r.get(2),
                        pv_count: r.get(3),
                        uv_count: r.get(4),
                        unique_ip_count: r.get(5),
                        err4xx_count: r.get(6),
                        err5xx_count: r.get(7),
                        qps_avg: r.get(8),
                    })
                    .collect())
            }
        }
    }
}
