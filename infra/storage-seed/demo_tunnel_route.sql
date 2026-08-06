-- Demo tunnel route for local smoke (matches default sag-connector + smoke headers).
-- Prefer scripts/seed-demo-tunnel-route.ps1 (management API). This raw SQLite
-- path is generation/outbox aware, but control-plane-admin must first be
-- stopped so the seed owns the one write transaction. Current schema tables
-- must already exist (start control-plane-admin once before using this file).

.bail on
BEGIN IMMEDIATE;

INSERT OR IGNORE INTO config_state (id, generation, updated_at_ms)
VALUES (1, 0, 0);

CREATE TEMP TABLE sag_seed_previous_apps (app_id TEXT PRIMARY KEY);
INSERT OR IGNORE INTO sag_seed_previous_apps (app_id)
SELECT app_id FROM tunnel_routes WHERE host = 'app.internal.com';

INSERT OR REPLACE INTO tunnel_routes (host, app_id, connector_endpoint, require_healthy_tunnel)
VALUES ('app.internal.com', 'app-001', 'connector-local-001:stream', 1);

CREATE TEMP TABLE sag_seed_affected_apps (app_id TEXT PRIMARY KEY);
INSERT OR IGNORE INTO sag_seed_affected_apps (app_id) VALUES ('app-001');
INSERT OR IGNORE INTO sag_seed_affected_apps (app_id)
SELECT app_id FROM sag_seed_previous_apps;

-- Abort rather than commit a snapshot that the Agent must reject.
CREATE TEMP TABLE sag_seed_guard (
  must_be_zero INTEGER NOT NULL CHECK (must_be_zero = 0)
);
INSERT INTO sag_seed_guard (must_be_zero)
SELECT 1
WHERE EXISTS (
  SELECT app_id
  FROM tunnel_routes
  GROUP BY app_id
  HAVING COUNT(DISTINCT connector_endpoint || char(31) || require_healthy_tunnel) > 1
);

UPDATE config_state
SET generation = MAX(
      generation,
      COALESCE((SELECT MAX(generation) FROM config_sync_jobs), 0),
      COALESCE((SELECT MAX(applied_generation) FROM agent_config_applies), 0)
    ) + 1,
    updated_at_ms = MAX(
      updated_at_ms,
      CAST((julianday('now') - 2440587.5) * 86400000 AS INTEGER)
    )
WHERE id = 1;

UPDATE config_sync_jobs
SET superseded_by_generation = (SELECT generation FROM config_state WHERE id = 1),
    updated_at_ms = (SELECT updated_at_ms FROM config_state WHERE id = 1)
WHERE target = 'APISIX'
  AND resource_type = 'ROUTE'
  AND resource_id IN (SELECT app_id FROM sag_seed_affected_apps)
  AND superseded_by_generation IS NULL;

INSERT INTO config_sync_jobs (
  job_id, generation, target, resource_type, resource_id, app_id, operation,
  payload_json, status, attempt_count, next_attempt_at_ms, last_error,
  lease_owner, lease_expires_at_ms, superseded_by_generation, created_at_ms,
  updated_at_ms, applied_at_ms
)
SELECT
  'seed-' || lower(hex(randomblob(16))), generation, 'APISIX', 'ROUTE',
  affected.app_id, affected.app_id,
  CASE WHEN EXISTS (
    SELECT 1 FROM tunnel_routes AS routes WHERE routes.app_id = affected.app_id
  ) AND EXISTS (
    SELECT 1 FROM intranet_upstreams AS upstreams WHERE upstreams.app_id = affected.app_id
  ) THEN 'UPSERT' ELSE 'DELETE' END,
  NULL, 'PENDING', 0, updated_at_ms, NULL, NULL, NULL, NULL,
  updated_at_ms, updated_at_ms, NULL
FROM config_state
CROSS JOIN sag_seed_affected_apps AS affected
WHERE config_state.id = 1;

INSERT INTO audit_logs (
  id, ts_ms, service, user_id, app_id, path, method, latency_ms,
  decision, result, trace_id, extra_json
)
SELECT
  'seed-' || lower(hex(randomblob(16))), updated_at_ms, 'storage-seed',
  'system:offline-seed', affected.app_id, '/infra/storage-seed/demo_tunnel_route.sql',
  'SEED', 0, 'MUTATE', 'COMMITTED', lower(hex(randomblob(16))),
  '{"generation":' || generation || '}'
FROM config_state
CROSS JOIN sag_seed_affected_apps AS affected
WHERE config_state.id = 1;

DROP TABLE sag_seed_guard;
DROP TABLE sag_seed_affected_apps;
DROP TABLE sag_seed_previous_apps;

COMMIT;
