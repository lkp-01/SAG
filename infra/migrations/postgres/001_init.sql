CREATE TABLE IF NOT EXISTS tunnel_routes (
  host TEXT PRIMARY KEY,
  app_id TEXT NOT NULL,
  connector_endpoint TEXT NOT NULL,
  require_healthy_tunnel BOOLEAN NOT NULL DEFAULT TRUE
);

CREATE TABLE IF NOT EXISTS intranet_upstreams (
  app_id TEXT PRIMARY KEY,
  upstream TEXT NOT NULL,
  scheme TEXT NOT NULL DEFAULT ''http''
);

CREATE TABLE IF NOT EXISTS policies (
  id TEXT PRIMARY KEY,
  effect TEXT NOT NULL,
  subjects_json TEXT NOT NULL,
  app_id TEXT,
  path_prefix TEXT,
  priority INTEGER NOT NULL DEFAULT 1000
);

CREATE TABLE IF NOT EXISTS users (
  id TEXT NOT NULL,
  username TEXT PRIMARY KEY,
  password_hash TEXT NOT NULL,
  roles_json TEXT NOT NULL,
  display_name TEXT,
  title TEXT,
  enabled BOOLEAN NOT NULL DEFAULT TRUE
);

CREATE INDEX IF NOT EXISTS idx_tunnel_routes_app_id ON tunnel_routes(app_id);
CREATE INDEX IF NOT EXISTS idx_policies_priority ON policies(priority);
CREATE INDEX IF NOT EXISTS idx_users_enabled ON users(enabled);

CREATE TABLE IF NOT EXISTS idempotency_records (
  scope_key TEXT PRIMARY KEY,
  request_hash TEXT NOT NULL,
  state TEXT NOT NULL CHECK (state IN ('pending', 'completed')),
  owner_attempt_id TEXT NOT NULL,
  status_code BIGINT NOT NULL DEFAULT 0,
  headers_json TEXT NOT NULL DEFAULT '{}',
  body BYTEA NOT NULL DEFAULT ''::bytea,
  created_at_ms BIGINT NOT NULL,
  updated_at_ms BIGINT NOT NULL,
  expires_at_ms BIGINT NOT NULL
);

CREATE INDEX IF NOT EXISTS idx_idempotency_expires
  ON idempotency_records(state, expires_at_ms);
