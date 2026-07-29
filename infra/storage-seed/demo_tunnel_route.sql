-- Demo tunnel route for local smoke (matches default sag-connector + smoke headers).
-- Apply when control-plane-admin is STOPPED, or use INSERT OR REPLACE anytime.
-- Table is created by control-plane-admin on first start (ensure_schema).

INSERT OR REPLACE INTO tunnel_routes (host, app_id, connector_endpoint, require_healthy_tunnel)
VALUES ('app.internal.com', 'app-001', 'connector-local-001:stream', 1);
