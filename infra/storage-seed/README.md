# SQLite seed snippets

Schema is owned by `shared_storage` (`tunnel_routes`, `intranet_upstreams`, `policies`). Prefer letting **control-plane-admin** create tables by starting it once.

Default DB path for admin + policy (when `SAG_STORAGE_DB_PATH` is unset): **`sag-cloud/data/sag-storage/sag.db`** relative to process cwd.

## Demo tunnel route (smoke / default connector)

**Option A — admin HTTP API**

```powershell
cd <sag-cloud>
.\scripts\seed-demo-tunnel-route.ps1
```

**Option B — env on admin startup**（表为空时插入一行）

```powershell
$env:SAG_BOOTSTRAP_DEMO_TUNNEL_ROUTE = "true"
$env:SAG_STORAGE_DB_PATH = "D:/tmp/sag-storage/sag.db"  # same as policy / your path
cargo run -p control-plane-admin
```

**Option C — raw SQL**（`sqlite3` CLI；路径与 `SAG_STORAGE_DB_PATH` 一致）

```bash
sqlite3 /path/to/sag.db < infra/storage-seed/demo_tunnel_route.sql
```

If the DB file does not exist yet, start `control-plane-admin` once to create tables, then run the SQL.

## Company demo seed (tech/finance/boss/vendor)

### Option A — API seed (recommended for current compose)

```powershell
cd <sag-cloud>
.\scripts\seed-company-demo.ps1
```

This writes:
- `tunnel_routes`
- `intranet_upstreams`
- `policies`

and refreshes `infra/storage-seed/company_users.sample.json` for frontend/user-portal mock display.

### Option B — PostgreSQL SQL seed

```bash
psql "postgres://postgres:postgres@127.0.0.1:5432/sag" -f infra/storage-seed/company_demo_postgres.sql
```

若你更新了仓库里的策略 SQL，需要重新执行上述导入（或再跑 Option A），并让 `stealth-tunnel-agent` 已配置 `SAG_POLICY_EVALUATE_ENDPOINT`，数据面才会按新策略裁决。

### 仅双机 demo：`app-001` 隧道路由 + mock 上游（PostgreSQL）

控制面 **`SAG_BOOTSTRAP_DEMO_TUNNEL_ROUTE=true`** 时，**仅在 `tunnel_routes` 表为空** 时自动插入 `app-001`。若你先执行了 **`company_demo_postgres.sql`**，表内已有 `app-dev` 等行，则 **不会** 自动出现 **`app-001`**，表现为 **`no tunnel route for app_id`**（对 `x-sag-app-id: app-001` 的请求）。

在 **Edge 本机**（或能连 Edge Postgres 的机器）执行：

```bash
docker exec -i sag-postgres psql -U postgres -d sag < infra/storage-seed/bootstrap_app001_dualhost_postgres.sql
```

然后 **Edge** 上建议：

```bash
docker compose -f docker-compose.edge.yml -f docker-compose.release.edge.yml restart stealth-tunnel-agent control-plane-admin
```

### User data note

Current `sag-auth` keeps users in-memory (bootstrap admin) and does not persist user accounts to DB yet.
So “users” are currently provided as mock fixture file:

- `infra/storage-seed/company_users.sample.json`
