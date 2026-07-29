-- Forward-compatible audit/fault schema hardening. Apply after 001_init.sql.
-- Production deployments must apply this migration before rolling out binaries
-- that enable the bounded audit writer.

CREATE TABLE IF NOT EXISTS audit_logs (
  id TEXT PRIMARY KEY,
  ts_ms BIGINT NOT NULL,
  service TEXT NOT NULL,
  user_id TEXT NOT NULL DEFAULT '',
  app_id TEXT NOT NULL DEFAULT '',
  path TEXT NOT NULL DEFAULT '',
  method TEXT NOT NULL DEFAULT '',
  latency_ms BIGINT NOT NULL DEFAULT 0,
  decision TEXT NOT NULL DEFAULT '',
  result TEXT NOT NULL DEFAULT '',
  trace_id TEXT NOT NULL DEFAULT '',
  extra_json TEXT NOT NULL DEFAULT ''
);

CREATE TABLE IF NOT EXISTS fault_events (
  id TEXT PRIMARY KEY,
  ts_ms BIGINT NOT NULL,
  service TEXT NOT NULL,
  event_type TEXT NOT NULL,
  severity TEXT NOT NULL DEFAULT 'warn',
  path TEXT NOT NULL DEFAULT '',
  method TEXT NOT NULL DEFAULT '',
  latency_ms BIGINT NOT NULL DEFAULT 0,
  baseline_ms BIGINT NOT NULL DEFAULT 0,
  threshold_ms BIGINT NOT NULL DEFAULT 0,
  status_code BIGINT NOT NULL DEFAULT 0,
  result TEXT NOT NULL DEFAULT '',
  trace_id TEXT NOT NULL DEFAULT '',
  source TEXT NOT NULL DEFAULT 'detector',
  resolved_at_ms BIGINT,
  meta_json TEXT NOT NULL DEFAULT ''
);

CREATE INDEX IF NOT EXISTS idx_audit_logs_ts_ms
  ON audit_logs(ts_ms);
CREATE INDEX IF NOT EXISTS idx_audit_logs_service_ts_ms
  ON audit_logs(service, ts_ms);
CREATE INDEX IF NOT EXISTS idx_audit_logs_user_ts_ms
  ON audit_logs(user_id, ts_ms);
CREATE INDEX IF NOT EXISTS idx_audit_logs_app_ts_ms
  ON audit_logs(app_id, ts_ms);
CREATE INDEX IF NOT EXISTS idx_audit_logs_trace_id
  ON audit_logs(trace_id);

CREATE INDEX IF NOT EXISTS idx_fault_events_ts_ms
  ON fault_events(ts_ms);
CREATE INDEX IF NOT EXISTS idx_fault_events_service_ts_ms
  ON fault_events(service, ts_ms);
CREATE INDEX IF NOT EXISTS idx_fault_events_trace_id
  ON fault_events(trace_id);

-- Retention is deliberately operator-scheduled, not run by application startup.
-- Call repeatedly with a millisecond cutoff and a bounded batch size, for example:
--   SELECT sag_delete_audit_before(
--     (extract(epoch FROM now() - interval '90 days') * 1000)::bigint,
--     10000
--   );
CREATE OR REPLACE FUNCTION sag_delete_audit_before(
  cutoff_ts_ms BIGINT,
  delete_batch_size INTEGER DEFAULT 10000
) RETURNS BIGINT
LANGUAGE plpgsql
AS $$
DECLARE
  deleted_rows BIGINT;
BEGIN
  IF delete_batch_size < 1 OR delete_batch_size > 100000 THEN
    RAISE EXCEPTION 'delete_batch_size must be between 1 and 100000';
  END IF;

  WITH doomed AS (
    SELECT ctid
    FROM audit_logs
    WHERE ts_ms < cutoff_ts_ms
    ORDER BY ts_ms
    LIMIT delete_batch_size
  )
  DELETE FROM audit_logs target
  USING doomed
  WHERE target.ctid = doomed.ctid;

  GET DIAGNOSTICS deleted_rows = ROW_COUNT;
  RETURN deleted_rows;
END;
$$;

CREATE OR REPLACE FUNCTION sag_delete_fault_events_before(
  cutoff_ts_ms BIGINT,
  delete_batch_size INTEGER DEFAULT 10000
) RETURNS BIGINT
LANGUAGE plpgsql
AS $$
DECLARE
  deleted_rows BIGINT;
BEGIN
  IF delete_batch_size < 1 OR delete_batch_size > 100000 THEN
    RAISE EXCEPTION 'delete_batch_size must be between 1 and 100000';
  END IF;

  WITH doomed AS (
    SELECT ctid
    FROM fault_events
    WHERE ts_ms < cutoff_ts_ms
    ORDER BY ts_ms
    LIMIT delete_batch_size
  )
  DELETE FROM fault_events target
  USING doomed
  WHERE target.ctid = doomed.ctid;

  GET DIAGNOSTICS deleted_rows = ROW_COUNT;
  RETURN deleted_rows;
END;
$$;
