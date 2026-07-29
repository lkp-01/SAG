-- Company demo seed for PostgreSQL backend.
-- Scope: tunnel routes + intranet upstreams + access policies.
-- Note: sag-auth users are currently in-memory only (not persisted in DB).

BEGIN;

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

COMMIT;
