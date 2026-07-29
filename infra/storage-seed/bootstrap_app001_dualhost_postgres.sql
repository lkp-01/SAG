-- Edge Postgres（库名一般为 sag）：补全双机 / 冒烟 / 门户（app-001）所需的最小路由数据。
--
-- 何时需要执行：
--   1) 先导入过 company_demo_postgres.sql，表里已有 app-dev 等行，但从未写入 app-001；
--   2) 控制面未设 SAG_BOOTSTRAP_DEMO_TUNNEL_ROUTE=true，或表非空导致 bootstrap 跳过插入。
--
-- 执行后请重启或等待 stealth-tunnel-agent 从控制面同步路由；并确认 control-plane 会对 app-001 做一次 APISIX reconcile（或重启 sag-control-plane-admin）。

BEGIN;

INSERT INTO tunnel_routes (host, app_id, connector_endpoint, require_healthy_tunnel)
VALUES ('app.internal.com', 'app-001', 'connector-local-001:stream', true)
ON CONFLICT (host) DO UPDATE SET
  app_id = EXCLUDED.app_id,
  connector_endpoint = EXCLUDED.connector_endpoint,
  require_healthy_tunnel = EXCLUDED.require_healthy_tunnel;

-- Intra 上 APISIX 与 mock 同 compose 网络时，主机名 mock-workload:18080 与控制面 bootstrap 一致。
INSERT INTO intranet_upstreams (app_id, upstream, scheme)
VALUES ('app-001', 'mock-workload:18080', 'http')
ON CONFLICT (app_id) DO UPDATE SET
  upstream = EXCLUDED.upstream,
  scheme = EXCLUDED.scheme;

COMMIT;
