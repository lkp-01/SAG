# Deployment (Docker Compose)

> Scope: the base files are development/split-host integration stacks. They
> are not production HA. The production relationship and failure-domain
> contract is documented in [production-ha-topology.md](production-ha-topology.md).

From the repository root, the base Compose files keep Bridge, Redis, etcd, and
APISIX Admin ports on container networks only:

```bash
docker compose -f docker-compose.edge.yml build
docker compose -f docker-compose.edge.yml up -d
# On the Intra host, after creating its local-only .env.intra:
docker compose -f docker-compose.intra.yml build
docker compose -f docker-compose.intra.yml up -d
```

For local troubleshooting only, add the debug override. It publishes internal
ports on loopback, never on all host interfaces:

The single debug override covers services from both halves of the split stack,
so render it with both base files:

```bash
docker compose \
  -f docker-compose.edge.yml \
  -f docker-compose.intra.yml \
  -f docker-compose.debug-ports.yml \
  up -d
```

Do not add `docker-compose.debug-ports.yml` to a production deployment.

Core URLs:

- control-plane-admin: `http://127.0.0.1:8090`
- sag-auth: `http://127.0.0.1:8080`
- sag-policy: `http://127.0.0.1:8081`
- bridge debug override: `http://127.0.0.1:9000`
- APISIX data debug override: `http://127.0.0.1:9080`
- APISIX Admin debug override: `http://127.0.0.1:9180`
- zentinel ingress: `https://127.0.0.1:10080`
- adminplane (Next): `http://127.0.0.1:3001`
- user portal: `http://127.0.0.1:5174`
- grafana: `http://127.0.0.1:3000`
- prometheus: `http://127.0.0.1:9091`

Frontend notes:

- 管理端主入口为 `frontend-admin-next`（非旧版 `frontend`）。
- 兼容控制面板页面：`/control`，已并入旧 Vite 控制台核心功能（路由/上游/策略/用户/登录会话/健康探测/4A 调试占位）。
- 用户门户“进入管理端”按钮默认跳转 `http://127.0.0.1:3001`（`boss/ops/admin` 角色可见）。
- 4A 联调入口：`http://127.0.0.1:8080/api/v1/auth/sso/login`（Fake 4A 模式下可一键跳转门户并自动登录）。

Smoke:

```powershell
.\scripts\smoke-dataplane.ps1
```

## HA overlay rendering

The following is static configuration validation, not a failover or capacity
test:

```bash
docker compose -f docker-compose.edge.yml -f docker-compose.release.edge.yml \
  -f docker-compose.hscale-edge.yml -f docker-compose.hscale-auth.yml config
docker compose -f docker-compose.intra.yml -f docker-compose.release.intra.yml config
```

The Edge release overlay requires external PostgreSQL and Redis endpoints and
places the local single-node containers behind explicit development profiles.
The Intra release overlay describes two APISIX processes and a three-member
etcd relationship. Running all those services on one Docker host is useful for
configuration testing only; production members must be scheduled across
independent failure domains.

`SAG_TUNNEL_ENDPOINTS` must list both Agent endpoints. Public traffic must use
a load balancer that removes a Bridge unless `GET /ready` returns 200. Do not
substitute a TCP connection, `/live`, or the metrics listener for readiness.

## Public Readonly Security Entry

For public demo pages under `3001`, configure a shared readonly token in `control-plane-admin`:

```yaml
SAG_PUBLIC_READONLY_TOKEN: "replace-with-demo-token"
```

Public pages:

- `http://<host>:3001/security/audit`
- `http://<host>:3001/security/pentest`

Usage:

- Open the page and paste the token once, or append `?token=<value>` for demo shortcuts.
- These endpoints are readonly only and return masked / limited data.
- They do not replace admin JWT auth for `/ops/*`.

Dual-host reliability notes (verified):

- Connector -> edge agent TLS must keep SNI aligned with cert SAN:
  - `SAG_GRPC_TLS_SERVER_NAME=localhost` (with current test certs)
- If `SAG_TUNNEL_ENDPOINT` DNS is wrong or agent is down, dataplane fails closed (`502 no connector stream` / `502 transport error`).
- If bridge returns `connector tunnel is unhealthy`, confirm route `connector_endpoint` matches active connector ID (`<SAG_CONNECTOR_ID>:stream`).

## Dual-host Production Split

Verified compose skeletons:

- edge: `docker-compose.edge.yml`
- intra: `docker-compose.intra.yml`

Recommended responsibility split:

- edge host:
  - `frontend-admin-next`
  - `control-plane-admin`
  - `sag-auth`
  - `sag-policy`
  - `stealth-tunnel-agent`
  - `http-tunnel-bridge`
  - `zentinel`
- intra host:
  - `apisix`
  - `sag-connector`
  - `mock-workload` or real workloads

Release-mode recommendation:

1. Build release binaries in container volume cache.
2. Start edge/intra services with release overrides where applicable.
3. Validate north probe, route sync, connector to APISIX reachability, and public readonly pages.

Release Compose uses `${VAR:?required}` for PostgreSQL, Redis, JWT, internal
Agent/Policy, APISIX Admin, 4A, public-readonly, Grafana, and TLS credentials.
Keep these values in a host secret manager or orchestrator secret, not in a
checked-in `.env`. Redis URLs must contain URL-encoded credentials. The APISIX
Admin key is read from `SAG_APISIX_ADMIN_API_KEY` through APISIX's native
environment-variable expansion; Admin CORS is disabled and the allowlist is
restricted to loopback and container-private address space.

The Intra development Compose is a single APISIX/etcd node. It is not a
production HA template. A production split must use a private or managed APISIX
Admin endpoint reachable only from the control plane; removing the Compose host
publication is intentional. PostgreSQL/Redis failover requires qualified
external clusters and is not supplied by this Compose stack.

Preconditions before production validation:

- VPN/DNS between edge and intra are ready
- TLS cert SAN matches real SNI
- `SAG_GRPC_TLS_SERVER_NAME` matches cert
- control-plane route `connector_endpoint` matches active connector id
- `SAG_PUBLIC_READONLY_TOKEN` is configured if public security pages are enabled
- every Bridge replica resolves the same non-empty mTLS CA/client certificate/
  client key/server-name block and has a unique `SAG_BRIDGE_INSTANCE_ID`
- `SAG_POLICY_INTERNAL_TOKEN` is identical on Agent and Policy replicas
- Redis endpoints are authenticated and are never host-published

## Secret and certificate rotation order

1. Generate new values outside the repository and back up the currently active
   secret references. Never place either generation in Compose YAML or logs.
2. For mTLS CA rotation, deploy a CA bundle trusting old and new issuers, then
   rotate server and client leaf certificates, verify every Bridge/Agent/
   Connector path, and only then remove the old CA.
3. APISIX Admin key rotation must temporarily accept old and new keys (or use a
   drained maintenance window when the deployment supports only one key).
   Switch control-plane callers to the new key before removing the old key.
4. Redis `requirepass` in this Compose supports one password. Stop admission,
   drain the queue/PEL, rotate the server password, update every authenticated
   Redis URL, restart consumers before producers, and resume only after queue
   recovery checks pass. Managed Redis should use ACL users for overlap.
5. Rotate the Policy internal token with Agent and Policy drained together.
   Rotate Agent sync and public-readonly tokens at their consumers first where
   overlap is supported.
6. The current JWT verifier has no dual-key window. Drain login traffic and
   rotate JWT secret across Auth, Policy, and Admin as one coordinated change;
   existing sessions are intentionally invalidated.
7. After validation, revoke old values, remove temporary overlap, and retain an
   audit record containing secret identifiers and timestamps but never secret
   contents.

Validation checklist:

1. `frontend-admin-next` reachable on `:3001`
2. `curl -i http://127.0.0.1:3001/api-zentinel/api/test` returns expected business response
3. route sync succeeds and connector is healthy
4. APISIX upstream path is reachable from intra host
5. `/security/audit` and `/security/pentest` are readable with readonly token
6. `/ops/workflow` and `/ops/observability` remain available with admin JWT

Validation status:

- already verified: dual-host compose skeleton, north/tunnel recovery path, release startup flow
- needs re-check after this change: public readonly token pages, production smoke with public security endpoints, Windows load script against target deployment

Zentinel startup and TLS guardrails:

- `zentinel` startup should use manifest-path mode to avoid historical toolchain-sync stalls:
  - `cargo run --manifest-path /workspace/proxy/core/Cargo.toml -p zentinel-proxy --bin zentinel -- --config /workspace/proxy/zentinel-proxy/config/dataplane-compose.kdl`
- Keep cert/key paths absolute in KDL config (container path), not relative.
- Before first boot on a new server, run certificate preflight:
  - `openssl x509 -in <cert> -noout -dates -ext subjectAltName`
  - `openssl pkey -in <key> -pubout | sha256sum`
  - `openssl x509 -in <cert> -pubkey -noout | sha256sum`
- Post-boot acceptance checks:
  - `curl -i http://127.0.0.1:9000/api/test` (T1)
  - `curl -i http://127.0.0.1:3001/api-zentinel/api/test` (N1)
