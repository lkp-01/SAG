# SAG Runbook (MVP)

## 1) `no tunnel route for app_id`
1. Check control-plane routes:
   - `GET /api/v1/agent/routes?app_id=app-001`
2. If empty, run:
   - `.\scripts\seed-demo-tunnel-route.ps1`
3. Wait one sync interval (default 5s) and retry smoke.

## 1.1) `/ops/apps` card missing for an app
1. Current card rendering should use `appsMeta ∪ routes.app_id`.
2. If an app exists in metadata but has no route, it should still appear with a “未配置路由” hint.
3. If a card is still missing, clear browser cache / localStorage key `sag.ops.apps.cache.v1` and refresh.
4. Verify:
   - `GET /api/v1/apps`
   - `GET /api/v1/apps/tree?with_latest=false`

## 2) APISIX 404/502 on `/api/test`
1. Verify APISIX data plane responds (`http://127.0.0.1:9080`).
2. Ensure route/upstream exists for `/api/test` and points to mock (`:18080`).
3. Run smoke layer S1 to isolate APISIX from tunnel chain.

## 3) Agent sync failures
- Check `SAG_CONTROL_PLANE_SYNC_ENDPOINT`.
- Multi-endpoint fallback supports comma-separated values.
- Localhost fallback can be disabled only with `SAG_CONTROL_PLANE_SYNC_NO_LOCALHOST_FALLBACK=true`.

## 4) Quick diagnostics
- `.\scripts\ops\diag-sync-routes.ps1`
- `.\scripts\ops\smoke-all.ps1`

## 5) Public readonly security entry
- Public pages:
  - `/security/audit`
  - `/security/pentest`
- Required env:
  - `SAG_PUBLIC_READONLY_TOKEN`
- Client access modes:
  - paste token into page
  - or open with `?token=<readonly-token>`
- Scope:
  - readonly only
  - masked user identifiers
  - no write / no destructive test execution

## 6) Dual-host tunnel unhealthy (connector)
Symptoms:
- `/api-bridge/*` or `/api-zentinel/*` returns:
  - `tunnel forward failed: ... "connector tunnel is unhealthy"`
- Workflow page shows `zentinel` / `apisix` down while control-plane route sync is normal.

Primary root causes in current deployment:
- `intra` `sag-connector` cannot establish the gRPC/mTLS connection to the Edge tunnel endpoint.
- **Connector ID mismatch**: bootstrap demo `tunnel_routes.connector_endpoint` is `connector-local-001:stream`. If `SAG_CONNECTOR_ID` is e.g. `connector-intra-001`, the stealth agent never matches heartbeats → `connector tunnel is unhealthy` even when gRPC works.

Fix checklist:
1. In `intra` `.env.intra` (env_file), ensure connector uses the current Edge tunnel:
   - `SAG_TUNNEL_ENDPOINT=https://<edge-ip>:50051`
   - Omit `SAG_CONNECTOR_ID` or set `SAG_CONNECTOR_ID=connector-local-001` to match bootstrap.
   - Note: `docker-compose.intra.yml` must not rely on `${VAR:-default}` for these keys if values live only in `env_file` — compose interpolation does not read `env_file`. Put tunnel/TLS values in `.env.intra`, or run `docker compose --env-file .env.intra ...`.
   - The Connector requires the Edge tunnel endpoint and mTLS material, but no database DSN. Central persistence is owned by Edge services.
2. Recreate connector:
   - `docker compose -f docker-compose.intra.yml up -d --force-recreate sag-connector`
3. Verify connector logs:
   - should contain `sag-connector tunnel up`
   - should not contain PostgreSQL connection or storage-backend messages
4. On edge, restart bridge path once for state convergence:
   - `docker-compose -f docker-compose.edge.yml -f docker-compose.release.edge.yml restart stealth-tunnel-agent http-tunnel-bridge`
5. Validate with script:
   - `bash ./scripts/ops/check-dualhost-tunnel.sh`

6. Load test correlation (k6 JSON, `docker logs --since`, VPN/ulimit/gRPC A/B, defer queue until tunnel stable):
   - See **[docs/ops/tunnel-loadtest-correlation.md](tunnel-loadtest-correlation.md)**.

## 7) Production hardening incident response

- **Alert first response:** acknowledge the alert, freeze deploy/config changes, capture the alert labels and dashboard window, and correlate request/attempt/trace/stream epoch. Do not restart every replica simultaneously.
- **Queue/Permanent PEL:** stop new mutation admission if terminal classifications no longer equal submissions. Preserve the Redis stream and consumer group, inspect ownership/attempt state, run the documented recovery worker, and never delete a group or replay `dispatched`/`indeterminate` work to clear the alert.
- **PostgreSQL failover:** Auth and Policy must fail closed. Confirm pool acquire wait remains bounded and aggregate connections stay within budget. Resume admission only after the new primary is writable, migrations match, and reconnect rate is stable; do not increase pool sizes during an outage.
- **Redis failover:** verify bounded fallback, consumer ownership, PEL age, and final job classifications. A recovered Redis endpoint is not sufficient if any job remains unknown.
- **Auth invalidation lag:** inspect `auth_cache_staleness_seconds`, `auth_invalidation_failed_total`, and `token_version_rejected_total`. Database/current `auth_version` is authoritative; do not lengthen cache TTL or accept the JWT version when the dependency is unavailable.
- **Indeterminate mutation:** follow [idempotency reconciliation](idempotency-reconciliation-runbook.md). Require current admin/boss authorization, upstream system-of-record evidence, a reason, exact confirmation, and state-version CAS.
- **Break glass:** declare an incident, use a time-limited individually attributable credential, require a second operator, record every read/action in the incident timeline, and revoke the credential immediately afterward. Break glass never permits disabling mTLS, identity/version checks, fail-closed policy, deadline/cancellation, or idempotency state rules.

Run faults and releases using [the production fault matrix](production-fault-matrix.md) and [ordered rollout](production-hardening-rollout.md). A validator self-test or single-host Compose run is not HA/failover evidence.
