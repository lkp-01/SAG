-- Edge Postgres（库名一般为 sag）：补全双机 / 冒烟 / 门户（app-001）所需的最小路由数据。
--
-- 何时需要执行：
--   1) 先导入过 company_demo_postgres.sql，表里已有 app-dev 等行，但从未写入 app-001；
--   2) 控制面未设 SAG_BOOTSTRAP_DEMO_TUNNEL_ROUTE=true，或表非空导致 bootstrap 跳过插入。
--
-- 执行后请重启或等待 stealth-tunnel-agent 从控制面同步路由；并确认 control-plane 会对 app-001 做一次 APISIX reconcile（或重启 sag-control-plane-admin）。

-- Prefer the management API while the stack is running. For this raw SQL path,
-- stop every control-plane-admin replica; generation, audit, and APISIX outbox
-- state are advanced atomically below, so Agents may remain online.
BEGIN;

CREATE TEMP TABLE sag_seed_previous_apps ON COMMIT DROP AS
SELECT DISTINCT app_id
FROM tunnel_routes
WHERE host = 'app.internal.com';

INSERT INTO tunnel_routes (host, app_id, connector_endpoint, require_healthy_tunnel)
VALUES ('app.internal.com', 'app-001', 'connector-local-001:stream', true)
ON CONFLICT (host) DO UPDATE SET
  app_id = EXCLUDED.app_id,
  connector_endpoint = EXCLUDED.connector_endpoint,
  require_healthy_tunnel = EXCLUDED.require_healthy_tunnel;

-- Intra APISIX and the mock workload share this Compose DNS name.
INSERT INTO intranet_upstreams (app_id, upstream, scheme)
VALUES ('app-001', 'mock-workload:18080', 'http')
ON CONFLICT (app_id) DO UPDATE SET
  upstream = EXCLUDED.upstream,
  scheme = EXCLUDED.scheme;

CREATE TEMP TABLE sag_seed_affected_apps ON COMMIT DROP AS
SELECT app_id FROM sag_seed_previous_apps
UNION
SELECT 'app-001';

DO $$
BEGIN
  IF EXISTS (
    SELECT app_id
    FROM tunnel_routes
    GROUP BY app_id
    HAVING COUNT(DISTINCT (connector_endpoint, require_healthy_tunnel)) > 1
  ) THEN
    RAISE EXCEPTION 'offline seed would leave conflicting Connector settings for one app';
  END IF;
END
$$;

DO $$
BEGIN
  IF NOT EXISTS (SELECT 1 FROM config_state WHERE id = 1) THEN
    RAISE EXCEPTION 'config convergence schema is required before offline seed';
  END IF;
END
$$;

UPDATE config_state
SET generation = GREATEST(
      generation,
      COALESCE((SELECT MAX(generation) FROM config_sync_jobs), 0),
      COALESCE((SELECT MAX(applied_generation) FROM agent_config_applies), 0)
    ) + 1,
    updated_at_ms = GREATEST(
      updated_at_ms,
      (EXTRACT(EPOCH FROM clock_timestamp()) * 1000)::BIGINT
    )
WHERE id = 1;

UPDATE config_sync_jobs AS jobs
SET superseded_by_generation = state.generation,
    updated_at_ms = state.updated_at_ms
FROM config_state AS state
WHERE state.id = 1
  AND jobs.target = 'APISIX'
  AND jobs.resource_type = 'ROUTE'
  AND jobs.resource_id IN (SELECT app_id FROM sag_seed_affected_apps)
  AND jobs.superseded_by_generation IS NULL;

INSERT INTO config_sync_jobs (
  job_id, generation, target, resource_type, resource_id, app_id, operation,
  payload_json, status, attempt_count, next_attempt_at_ms, last_error,
  lease_owner, lease_expires_at_ms, superseded_by_generation, created_at_ms,
  updated_at_ms, applied_at_ms
)
SELECT
  'seed-app001-' || state.generation || '-' || affected.app_id,
  state.generation, 'APISIX', 'ROUTE', affected.app_id, affected.app_id,
  CASE WHEN EXISTS (
    SELECT 1 FROM tunnel_routes AS routes WHERE routes.app_id = affected.app_id
  ) AND EXISTS (
    SELECT 1 FROM intranet_upstreams AS upstreams WHERE upstreams.app_id = affected.app_id
  ) THEN 'UPSERT' ELSE 'DELETE' END,
  NULL, 'PENDING', 0, state.updated_at_ms,
  NULL, NULL, NULL, NULL, updated_at_ms, updated_at_ms, NULL
FROM sag_seed_affected_apps AS affected
CROSS JOIN config_state AS state
WHERE state.id = 1;

INSERT INTO audit_logs (
  id, ts_ms, service, user_id, app_id, path, method, latency_ms,
  decision, result, trace_id, extra_json
)
SELECT
  'seed-app001-audit-' || state.generation || '-' || affected.app_id,
  state.updated_at_ms, 'storage-seed',
  'system:offline-seed', affected.app_id,
  '/infra/storage-seed/bootstrap_app001_dualhost_postgres.sql', 'SEED', 0,
  'MUTATE', 'COMMITTED',
  'seed-app001-trace-' || state.generation || '-' || affected.app_id,
  json_build_object('generation', state.generation)::TEXT
FROM sag_seed_affected_apps AS affected
CROSS JOIN config_state AS state
WHERE state.id = 1;

COMMIT;
