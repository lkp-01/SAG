-- Company demo seed for PostgreSQL backend.
-- Scope: tunnel routes + intranet upstreams + access policies.
-- Note: sag-auth users are currently in-memory only (not persisted in DB).
-- Prefer scripts/seed-company-demo.ps1 for a running stack. If this offline SQL
-- path is used, stop every control-plane-admin replica for the transaction;
-- Agents may stay up because generation/outbox state is advanced atomically.

BEGIN;

CREATE TEMP TABLE sag_seed_previous_apps ON COMMIT DROP AS
SELECT DISTINCT app_id
FROM tunnel_routes
WHERE host IN (
  'dev.internal.com', 'ci.internal.com', 'finance.internal.com',
  'oa.internal.com', 'hr.internal.com', 'bi.internal.com', 'vendor.internal.com'
);

-- 1) Tunnel routes
INSERT INTO tunnel_routes (host, app_id, connector_endpoint, require_healthy_tunnel) VALUES
  ('dev.internal.com',     'app-dev',     'connector-local-001:stream', TRUE),
  ('ci.internal.com',      'app-ci',      'connector-local-001:stream', TRUE),
  ('finance.internal.com', 'app-finance', 'connector-local-001:stream', TRUE),
  ('oa.internal.com',      'app-oa',      'connector-local-001:stream', TRUE),
  ('hr.internal.com',      'app-hr',      'connector-local-001:stream', TRUE),
  ('bi.internal.com',      'app-bi',      'connector-local-001:stream', TRUE),
  ('vendor.internal.com',  'app-vendor',  'connector-local-001:stream', TRUE)
ON CONFLICT (host) DO UPDATE SET
  app_id = EXCLUDED.app_id,
  connector_endpoint = EXCLUDED.connector_endpoint,
  require_healthy_tunnel = EXCLUDED.require_healthy_tunnel;

-- 2) Intranet upstream mappings (all point to company-demo-sites HTML service)
INSERT INTO intranet_upstreams (app_id, upstream, scheme) VALUES
  ('app-dev',     'company-demo-sites:28080', 'http'),
  ('app-ci',      'company-demo-sites:28080', 'http'),
  ('app-finance', 'company-demo-sites:28080', 'http'),
  ('app-oa',      'company-demo-sites:28080', 'http'),
  ('app-hr',      'company-demo-sites:28080', 'http'),
  ('app-bi',      'company-demo-sites:28080', 'http'),
  ('app-vendor',  'company-demo-sites:28080', 'http')
ON CONFLICT (app_id) DO UPDATE SET
  upstream = EXCLUDED.upstream,
  scheme = EXCLUDED.scheme;

CREATE TEMP TABLE sag_seed_affected_apps ON COMMIT DROP AS
SELECT app_id FROM sag_seed_previous_apps
UNION
SELECT app_id FROM (VALUES
  ('app-dev'), ('app-ci'), ('app-finance'), ('app-oa'),
  ('app-hr'), ('app-bi'), ('app-vendor')
) AS desired(app_id);

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

-- 3) Policies (subjects_json is JSON array string)
INSERT INTO policies (id, effect, subjects_json, app_id, path_prefix, priority) VALUES
  ('p-allow-admin-all',     'ALLOW', '["role:admin"]',     '*',           '/',             6000),
  ('p-allow-boss-all',      'ALLOW', '["role:boss"]',      '*',           '/',             5000),
  ('p-allow-tech-dev',      'ALLOW', '["role:tech"]',      'app-dev',     '/',             3000),
  ('p-allow-tech-ci',       'ALLOW', '["role:tech"]',      'app-ci',      '/',             3000),
  ('p-allow-tech-oa',       'ALLOW', '["role:tech"]',      'app-oa',      '/',             2500),
  ('p-allow-finance-core',  'ALLOW', '["role:finance"]',   'app-finance', '/',             3200),
  ('p-allow-finance-oa',    'ALLOW', '["role:finance"]',   'app-oa',      '/',             2500),
  ('p-allow-vendor-only',   'ALLOW', '["role:vendor"]',    'app-vendor',  '/',             2800),
  -- UI portal “多卡片”在仅 bootstrap app-001 时共用隧道；非 admin 角色需能访问 app-001 下的各 path。
  ('p-allow-sandbox-app001','ALLOW', '["role:tech","role:finance","role:vendor"]', 'app-001', '/', 4500),
  ('p-deny-vendor-finance', 'DENY',  '["role:vendor"]',    'app-finance', '/',             9000),
  ('p-deny-vendor-hr',      'DENY',  '["role:vendor"]',    'app-hr',      '/',             9000),
  ('p-deny-tech-finance',   'DENY',  '["role:tech"]',      'app-finance', '/',             8500),
  ('p-deny-tech-hr',        'DENY',  '["role:tech"]',      'app-hr',      '/',             8500),
  ('p-deny-tech-bi',        'DENY',  '["role:tech"]',      'app-bi',      '/',             8500),
  ('p-deny-tech-vendor',    'DENY',  '["role:tech"]',      'app-vendor',  '/',             8500)
ON CONFLICT (id) DO UPDATE SET
  effect = EXCLUDED.effect,
  subjects_json = EXCLUDED.subjects_json,
  app_id = EXCLUDED.app_id,
  path_prefix = EXCLUDED.path_prefix,
  priority = EXCLUDED.priority;

-- 4) Publish one durable configuration generation and APISIX job per affected
-- app. Abort on an old schema rather than silently changing rows without a
-- generation/outbox.
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
  'seed-company-' || state.generation || '-' || affected.app_id,
  state.generation, 'APISIX', 'ROUTE', affected.app_id, affected.app_id,
  CASE WHEN EXISTS (
    SELECT 1 FROM tunnel_routes AS routes WHERE routes.app_id = affected.app_id
  ) AND EXISTS (
    SELECT 1 FROM intranet_upstreams AS upstreams WHERE upstreams.app_id = affected.app_id
  ) THEN 'UPSERT' ELSE 'DELETE' END,
  NULL, 'PENDING', 0, state.updated_at_ms, NULL,
  NULL, NULL, NULL, state.updated_at_ms, state.updated_at_ms, NULL
FROM sag_seed_affected_apps AS affected
CROSS JOIN config_state AS state
WHERE state.id = 1;

INSERT INTO audit_logs (
  id, ts_ms, service, user_id, app_id, path, method, latency_ms,
  decision, result, trace_id, extra_json
)
SELECT
  'seed-company-audit-' || state.generation || '-' || affected.app_id,
  state.updated_at_ms, 'storage-seed', 'system:offline-seed', affected.app_id,
  '/infra/storage-seed/company_demo_postgres.sql', 'SEED', 0,
  'MUTATE', 'COMMITTED',
  'seed-company-trace-' || state.generation || '-' || affected.app_id,
  json_build_object('generation', state.generation)::TEXT
FROM sag_seed_affected_apps AS affected
CROSS JOIN config_state AS state
WHERE state.id = 1;

COMMIT;
