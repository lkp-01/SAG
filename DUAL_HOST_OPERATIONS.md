# 双机（Edge / Intra）运维交接 — 会话切换速查

> **【项目根目录 · 接手入口】** 请先看 **[`README.md`](README.md)**（阅读顺序 + 第一天操作清单）。  
> - **[`PROJECT_HANDOFF.md`](PROJECT_HANDOFF.md)** — 交接总结（**重点结论、压测结果、待办**）  
> - **[`SERVER_OPS_QUICKREF.md`](SERVER_OPS_QUICKREF.md)** — 各机常用命令（**生产 release 编译/启动**、冒烟、压测）  
> - **[`docs/ops/dataplane-load-3000-7000-report.md`](docs/ops/dataplane-load-3000-7000-report.md)** — 数据面 3000–7000 压测报告  

本文档供切换会话或换人接手时 **快速恢复上下文**。

**`REPO_ROOT`（= Git 仓库根）**：与 **`.git` 所在目录** 相同，且该目录下直接含有 **`docker-compose.edge.yml`**（Edge / Intra compose 均从此根执行）。克隆远端 `secure_access_gateway_sag.git` 后，常见文件夹名为 `secure_access_gateway_sag`；若本机把工作副本放在 `~/workspace/sag-cloud`，则 **`REPO_ROOT=~/workspace/sag-cloud`**。**不要**把仅用于解压/打包的**上层父目录**（无 `.git`、无 compose）当成仓库根——在那种目录里执行 `git pull` 会失败。

**Bash 约定**：下文出现 **`cd "$REPO_ROOT"`** 时，须已 **`export REPO_ROOT=…`**（与上表一致）。§**1b-A** 首次给出带默认值的 **`export`**；若只复制其他小节，请自行先导出。

**快速跳转**：**§0 会话接续（必读）** · §1 角色与地址 · §1b 迁 Edge · §1c～§1d 隧道故障 · **§1e 本机通浏览器不通** · §2 Git · **§2b pull 后 Docker 重建** · **§2c pull 冲突（cpuset）** · §3～4 组件表 · **§3.0 当前扩展快照** · **§3b～3c 水平扩展** · **§3d Auth/数据面压测（Windows k6，已弃作主路径）** · **§3e 压测机（麒麟 VM + Linux k6，推荐）** · §5 环境变量 · §6～7 生产启动 · §8 重启 · **§8b 冒烟** · **§10 诊断** · §11 参考文件

---

## 0. 会话接续（2026-05-20）— 给下一个 Agent / 同事

**读完本节 + §3.0 + §3e**，即可按与上一会话相同的方式继续压测、排障、改 compose（Auth 压测优先 **麒麟 VM §3e**，非 Windows §3d）。

### 0.1 最近在干什么（时间线）

| 阶段 | 目标 | 状态 |
|------|------|------|
| 数据面容量 | Windows k6 → Edge `:10080`，`dataplane_only` + **`apisix_routed`** | **1000→3000 iter/s 约 97–99%** 成功率 |
| 全链路 | `mixed_fullchain` @ 3000 | **整链 ~40%**；**login ~0.5%** → 确认 Auth 为瓶颈 |
| Auth 单实例基线 | `auth_login_verify` @ 2000（Windows） | **login ~79%**，verify ~100% |
| Auth 水平扩展 | `hscale-auth.yml`：2×`sag-auth` + nginx LB `:8080` | 已合入 `clean-main` |
| EMFILE 修复 | auth 容器 `nofile=1048576`、nginx `keepalive 512` | 合入 **`a41cd5a6`**；curl 5×200 正常 |
| Cpuset 冲突 | `policy` 曾 **3–6** 与 `auth-2` **CPU 3** 重叠 | 仓库改为 **policy=4–7**（**`17390a10`**）；Edge 上 policy 已 recreate 为 **4–7** |
| Auth hscale 复测 | Windows `auth_login_verify` @ 2000 | **2026-05-20**：chain **56%**，login **59%**，verify **95%**；**未达 90%**，**勿测 3000** |

### 0.2 三台机器与目录（当前生产规划）

| 角色 | IP / 主机 | 代码路径（示例） | 用途 |
|------|-----------|------------------|------|
| **Edge** | **`172.16.9.107`**（28 逻辑核，`lxz-Super-Server`） | `~/secure_access_gateway_sag` = **`REPO_ROOT`** | Docker 全栈、Postgres、Zentinel、Auth LB |
| **Intra** | **`192.168.9.26`**（8 核） | 同仓库另一份 clone | APISIX、mock、**sag-connector** → Edge |
| **压测机（Windows，旧）** | **`172.16.9.108`** | `D:\...\Secure_Access_Gateway_SAG\sag-cloud` | 曾用 k6；高 QPS 易 **临时端口耗尽**（`connectex`），**Auth 门禁改走 §3e** |
| **压测机（麒麟 VM，推荐）** | **`<LOADGEN_IP>` 待填** | **`REPO_ROOT`** = 含 `docker-compose.edge.yml` 的 clone 根（例 `~/secure_access_gateway_sag`） | **8 核 / 64GiB / 麒麟**；**k6 + bash 脚本** → Edge **172.16.9.107**；见 **§3e** |

**Git**：`git@192.168.14.10:digital-operation/secure_access_gateway_sag.git`，分支 **`clean-main`**。

**近期相关提交**（从新到旧）：

| 提交 | 内容 |
|------|------|
| `17390a10` | `SAG_EDGE_CPUSET_POLICY=4-7`；`verify-hscale-edge.sh` 修 auth cpuset 查询；本文档更新 |
| `a41cd5a6` | Auth hscale **nofile**、`sag-auth-lb.conf` keepalive |
| `7a4316e2` | `hscale-auth*.yml`、`auth_login_verify` k6 场景、runbook |

### 0.3 Edge 当前栈形态（目标态 vs 已知偏差）

**目标 compose 叠层**（数据面 + Auth hscale + 绑核）：

```bash
export REPO_ROOT="${REPO_ROOT:-$HOME/secure_access_gateway_sag}"
cd "$REPO_ROOT"

docker compose \
  -f docker-compose.edge.yml \
  -f docker-compose.release.edge.yml \
  -f docker-compose.hscale-edge.yml \
  -f docker-compose.hscale-auth.yml \
  -f docker-compose.edge.perf.yml \
  -f docker-compose.hscale-edge.perf.yml \
  -f docker-compose.hscale-auth.perf.yml \
  --env-file scripts/ops/cpuset-edge-28.env \
  up -d --force-recreate
```

| 组件 | 目标 | 核对命令 |
|------|------|----------|
| bridge ×2 + zentinel hscale | cpuset 12–14 / 15–17 / 18–25 | `EDGE_IP=172.16.9.107 bash scripts/ops/verify-hscale-edge.sh` |
| **sag-auth + sag-auth-2 + sag-auth-lb** | :8080 → LB；auth **2/3**，LB **26** | `curl -sS -o /dev/null -w "%{http_code}\n" -X POST http://127.0.0.1:8080/api/v1/auth/login -H 'Content-Type: application/json' -d '{"username":"admin","password":"Admin@123"}'` |
| **sag-policy** | cpuset **4–7**（勿与 auth-2 抢 3） | `docker inspect sag-policy --format 'cpuset={{.HostConfig.CpusetCpus}}'` |
| auth nofile | **1048576** | `docker inspect secure_access_gateway_sag-sag-auth-1 --format '{{json .HostConfig.Ulimits}}'` |

**已知偏差（接手后建议修）**：

- 部分容器仍可能为 **`cargo run`**（非 release），尤其 **`bridge-2`**、**`zentinel`** → 压测前跑 §6 release 并 `--force-recreate`。
- Edge 上 **`git pull`** 若报 **`cpuset-edge-28.env` 本地修改**，见 **§2c**（policy 已手动 recreate 时，优先与仓库对齐 env 再 pull）。
- `verify-hscale-edge.sh` 里 **`:9104` 是 stealth-agent metrics**，不是 auth；**以 LB login 200 为准**。

### 0.4 压测口径（k6 必读，避免误判）

| 术语 | 含义 |
|------|------|
| **`apisix_routed`** | 数据面：客户端收到 HTTP 响应且 status 200–599 即成功；上游 mock **5xx 可算成功**；**不含** login/policy |
| **`auth_login_verify`** | 每迭代 **login + verify**，**无会话缓存**；QPS=2000 → Auth HTTP 约 **4000/s** |
| **`mixed_fullchain`** | 每迭代整条混合链；配置的 QPS = **迭代/秒**，≠ 单独数据面 RPS |
| **Gate `auth`** | `sag_api_success_rate{api:auth_login}`、`auth_verify`、`sag_chain_success_rate` 均 **>90%**（脚本 threshold 写 >0.98 的是 k6 严格档；业务门禁按 **90%**） |
| **晋级 3000** | 仅当 **压测机（§3e 麒麟 VM）** `auth_login_verify` @ **2000** 且 **chain/login/verify 均 >90%** 再测 3000 |

**压测机选型**：**Auth / 全链路门禁以 §3e Linux（麒麟 VM）为准**。Windows **172.16.9.108** 高 QPS 短连接易 **`WSAEADDRINUSE`** / **`wsarecv`**（§3d.1），结果易混入客户端瓶颈，**勿与 Edge 能力直接划等号**。

### 0.5 压测结果一览（artifact 在 Windows `sag-cloud/artifacts/`）

| 场景 | 配置 | login | verify | chain | 实际 iter/s | 备注 |
|------|------|-------|--------|-------|-------------|------|
| 数据面 only | `apisix_routed` 1000→3000 | — | — | — | ≈目标 97% | `k6-dp-*` |
| 全链路 | mixed @ 3000 | ~0.5% | — | ~40% | — | Auth 打穿 |
| Auth 单实例 | `auth_login_verify` @ 2000 | **~79%** | ~100% | **~79%** | ~1858 | `k6-auth-2000-20260519-182051.json` |
| Auth hscale（EMFILE 前） | 同上 | ~6% | ~73% | ~4% | ~1824 | nginx/auth **EMFILE** |
| Auth hscale（nofile 后） | 同上 | ~17% | ~33% | ~5% | ~1338 | 仍有 Windows 端口问题 |
| **Auth hscale（policy 4–7 后）** | **2026-05-20 Windows** | **59%** | **95%** | **56%** | **~220** | **`k6-auth-win-2000-20260520-135225.json`**；`dropped_iterations` **805839** |

### 0.6 接手后建议顺序（复制即用）

1. **Edge**：§2c 处理 `git pull` → §3.0 全量 compose（或 §3c 只重建 auth/policy）→ §10 Auth 诊断块。
2. **压测机（麒麟 VM，§3e）**：`run-auth-gate-2000.sh` → 将终端里 **`SAG K6 RESULT PASTE BLOCK`** 整段贴回。
3. **>90% 再 3000**；否则查 Edge nginx/auth 日志、EMFILE、cpuset，勿盲目加 RPS。
4. 数据面回归：§3d **`dataplane_only` + `apisix_routed`**。

---

## 1. 两台机器角色与地址（按当前规划填写；换机请改）

| 项 | Edge（外网/隧道/管理面入口） | Intra（内网 APISIX / Connector） |
|----|------------------------------|----------------------------------|
| **角色** | ZTNA 数据面、Bridge、Agent、Auth/Policy/Admin、Postgres/Redis、前端 :3001、Zentinel :10080 | etcd、APISIX、mock-workload、company-demo、**sag-connector**、metrics-gateway |
| **典型 IP（示例）** | **`172.16.9.107`**（新 Edge VM，Ubuntu 24.04） | **`192.168.9.26`**（文档/Woo 内网机；以实际为准） |
| **旧文档默认 Edge** | `192.168.8.87`（compose 默认值；**换机后必须用 `.env` 覆盖**） | 同上 Intra |
| **代码目录（`REPO_ROOT`）** | 见文首：含 **`.git` + `docker-compose.edge.yml`** 的目录（例 `~/secure_access_gateway_sag` 或 `~/workspace/sag-cloud`） | 同左，**另一台物理机/VM 上各自 clone 一份** |
| **Docker** | 例：Docker CE **29.x** + Compose plugin **v5.x**（与 Intra 不必逐字相同） | Intra 曾为 Ubuntu **25.04** + `docker.io` **28.2.2** + compose-v2 **2.37.1**（仅作对照） |

**双机连通要点**：Intra 上 **`sag-connector`** 的 **`SAG_TUNNEL_ENDPOINT`**、Connector 身份和 TLS 证书路径必须写在 **`.env.intra`**（`env_file`）。**不要**指望 `docker-compose.intra.yml` 里 **`${VAR:-default}`** 去读 `.env.intra`：Compose 只在解析 compose 时用宿主机 shell / 项目 **`.env`** 插值，**不会**用 `env_file` 的值，曾导致仍连旧 Edge 且与 **connector 心跳键** 错位。The Connector requires the Edge tunnel endpoint and mTLS material, but no database DSN. Central persistence is owned by Edge services.

---

## 1b. 迁移到新 Edge（`172.16.9.107`）— 直接复制

**说明**：仓库已提交 **`intra-host.env.example`**（Intra）与既有 **`edge-host.env.example`**（Edge）。两台机器各自 **`git pull`** 后按下面做；**不必**先关老 Edge，但必须让 **Intra 的 connector 指向新 Edge IP**。

### A. 两台机器都先更新代码

```bash
export REPO_ROOT="${REPO_ROOT:-$HOME/secure_access_gateway_sag}"   # 若本机在 ~/workspace/sag-cloud，先改此默认值或事先 export
cd "$REPO_ROOT"
git checkout clean-main
git pull origin clean-main --ff-only
```

### B. Edge 上（`172.16.9.107`）

```bash
export REPO_ROOT="${REPO_ROOT:-$HOME/secure_access_gateway_sag}"
cd "$REPO_ROOT"

# 若无 .env，从模板生成（已含 SAG_PUBLIC_HOST=172.16.9.107 与 APISIX admin 等）
test -f .env || cp edge-host.env.example .env

# 可选：确认外网/回调主机与 Intra Admin
grep -E '^(SAG_PUBLIC_HOST|SAG_APISIX_ADMIN_)' .env || true

# 让 agent / bridge / 控制面重新加载（connector 改连后路由会同步）
docker compose -f docker-compose.edge.yml -f docker-compose.release.edge.yml up -d --force-recreate stealth-tunnel-agent http-tunnel-bridge control-plane-admin
```

### C. Intra 上（`192.168.9.26`，以实际为准）

**仓库路径以本机为准**（`REPO_ROOT` 须含 `.git`，见文首；常见 `~/workspace/sag-cloud` 或 `~/secure_access_gateway_sag`）。

**方式 1 — 不依赖 pull，整段粘贴（当前生产 Edge `172.16.9.107`）**（先 `cd` 到本机已有 clone 的仓库根）：

```bash
export REPO_ROOT="${REPO_ROOT:-$HOME/workspace/sag-cloud}"
cd "$REPO_ROOT"

cat > .env.intra <<'EOF'
SAG_TUNNEL_ENDPOINT=https://172.16.9.107:50051
SAG_GRPC_TLS_SERVER_NAME=localhost
SAG_GRPC_TLS_CLIENT_CERT=/workspace/infra/tls/client.crt
SAG_GRPC_TLS_CLIENT_KEY=/workspace/infra/tls/client.key
SAG_GRPC_TLS_CA=/workspace/infra/tls/ca.crt
EOF

docker compose -f docker-compose.intra.yml -f docker-compose.release.intra.yml up -d --force-recreate sag-connector
# 若刚 pull 了 mock 路径（/oa/、/ci/ 等）相关代码，建议同时重建 mock：
# docker compose -f docker-compose.intra.yml -f docker-compose.release.intra.yml up -d --force-recreate mock-workload
```

**方式 2 — 已 `git pull` 含 `intra-host.env.example`**：

```bash
export REPO_ROOT="${REPO_ROOT:-$HOME/workspace/sag-cloud}"
cd "$REPO_ROOT"
cp -f intra-host.env.example .env.intra
docker compose -f docker-compose.intra.yml -f docker-compose.release.intra.yml up -d --force-recreate sag-connector
```

若 Intra 上 **`.env.intra` 已存在且含旧 IP**，请把其中 **`192.168.8.87`** 全部改为 **`172.16.9.107`** 后再执行 **`up -d --force-recreate sag-connector`**。

### D. Windows 上复测冒烟

```powershell
cd D:\lxz\compile\Rust_project\Secure_Access_Gateway_SAG\sag-cloud
git pull origin clean-main
.\scripts\smoke-remote-windows.ps1 -EdgeHost 172.16.9.107 -IntraHost 192.168.9.26 -Rounds 1
```

脚本双机模式默认 **`HDR_APP=app-001`**（与控制面 **`SAG_BOOTSTRAP_DEMO_TUNNEL_ROUTE`** / connector **`SAG_APP_ID`** 一致）。默认 **`SMOKE_SKIP_MULTI_APP=1`** 跳过 `V*` 多应用层（未单独 seed 的 app 会 502）；要跑全量可 **`$env:SMOKE_SKIP_MULTI_APP='0'`** 再执行脚本。

期望：**S1** 仍 200；**N1 / T1** 在 **Intra `.env.intra` 指向当前 Edge** 且 **connector 正常心跳** 后为 **2xx**。

若仍见 **`no tunnel route for app_id`**，按顺序排查：**(1)** Intra connector / Edge agent 同步（§1b、§1d）；**(2)** Postgres 是否真有 **`app-001`** 行（先执行过 **`company_demo_postgres.sql`** 且无 `app-001` 时，bootstrap 不会自动补，见 **`infra/storage-seed/bootstrap_app001_dualhost_postgres.sql`** 与 **`infra/storage-seed/README.md`**）；**(3)** 仅浏览器、本机 `curl` 已 200 时见 **§1e**（门户经 **:3001**、旧前端包等）。

### 1c. 现象：`connector tunnel dropped … error=transport error`（TCP 通、gRPC 不通）

**含义**：Intra 到 Edge **50051 TCP 已通**（`nc` 成功），但 **gRPC/mTLS 建链失败** 或 **Edge `sag-stealth-agent` 未正常收流**。与 **`.env.intra` 里 IP 写错** 无直接关系（IP 对仍 transport error 时优先查 TLS/进程）。

**在 Intra（Gzh）补全 TLS 路径后重建 connector**（`intra-host.env.example` 已含这三行）：

```bash
export REPO_ROOT="${REPO_ROOT:-$HOME/workspace/sag-cloud}"
cd "$REPO_ROOT"
grep -E 'SAG_GRPC_TLS_(CLIENT_CERT|CLIENT_KEY|CA)=' .env.intra || true
docker compose -f docker-compose.intra.yml -f docker-compose.release.intra.yml up -d --force-recreate sag-connector
```

**在 Edge 上看 stealth 是否在接 TLS、是否有拒连日志**：

```bash
export REPO_ROOT="${REPO_ROOT:-$HOME/secure_access_gateway_sag}"
cd "$REPO_ROOT"
docker ps -a --filter name=sag-stealth-agent
docker logs --tail 80 sag-stealth-agent 2>&1
```

**压测 / k6 与隧道日志时间对齐**（`docker logs --since`、指标对照、VPN/ulimit/gRPC A/B、延后队列）：见 **[docs/ops/tunnel-loadtest-correlation.md](docs/ops/tunnel-loadtest-correlation.md)**。

### 1d. 现象：`connector tunnel is unhealthy`（TCP/TLS 已通、仍 502）

**含义**：`stealth-tunnel-agent` 用 **`tunnel_routes.connector_endpoint`** 与 connector 心跳里的 endpoint 做匹配。控制面空库 bootstrap 写入的 demo 行为 **`connector-local-001:stream`**（见 `shared/storage/src/routes.rs`）。若 Intra 上误设 **`SAG_CONNECTOR_ID=connector-intra-001`**（或旧 compose 默认），心跳键为 **`connector-intra-001:stream`** → **与 DB 不一致** → agent 认为隧道不健康。

**修复**：在 **`.env.intra` 删除 `SAG_CONNECTOR_ID`**（使用二进制默认 **`connector-local-001`**），或显式设为 **`SAG_CONNECTOR_ID=connector-local-001`**，然后 **`docker compose ... up -d --force-recreate sag-connector`**；必要时 **`docker compose ... restart stealth-tunnel-agent`**（compose **服务名**，勿写成容器名 `sag-stealth-agent`）或 **`docker restart sag-stealth-agent`** 清负向缓存。

**从 Intra 粗测 TLS（不换行粘贴）**：

```bash
echo | openssl s_client -connect 172.16.9.107:50051 -servername localhost 2>&1 | tail -n 25
```

**APISIX `9080/dev/` 返回 404**：多为 **Edge 上 control-plane 未把路由 reconcile 到 Intra APISIX**（`SAG_APISIX_ADMIN_BASE_URL` / **admin key** / 网络）。**直连 APISIX 9080 探活时**还须请求头 **`x-sag-app-id: app-001`**（与 `control-plane-admin` 下发 route 的 `vars` 一致）；缺该头也会 **404**。在 **Edge** 查：

```bash
export REPO_ROOT="${REPO_ROOT:-$HOME/secure_access_gateway_sag}"
grep -E '^SAG_APISIX_ADMIN' "$REPO_ROOT/.env"
docker logs --tail 80 sag-control-plane-admin 2>&1 | tail -n 40
```

### 1e. 现象：Edge 上 `curl 127.0.0.1:10080` 正常，浏览器 `:3001/portal` 仍 `502 no tunnel route`

**含义**：数据库与 **`GET /api/v1/agent/routes?app_id=app-001`**（8090，须先用 **8080** `sag-auth` 登录拿 Bearer）已能列出路由时，问题多在 **浏览器 → Next（3001）→ Zentinel** 这一段，而不是「库里没数据」。

**检查清单**：

1. **登录控制面查询用 8080，不是 8090**  
   `POST http://<Edge>:8080/api/v1/auth/login`，`GET http://<Edge>:8090/api/v1/agent/routes?app_id=app-001` 头 **`Authorization: Bearer <token>`**。

2. **门户代码是否已更新并重建**  
   仓库中用户门户卡片已统一为 **`x-sag-app-id: app-001`**（多路径 `/dev/`、`/oa/` 等）。改代码后必须在 Edge 执行 **`docker compose ... up -d --build frontend-admin-next`**，浏览器 **强刷或无痕**，并在开发者工具 **Network** 里确认探测请求头里 **`x-sag-app-id`** 为 **`app-001`**。

3. **经 3001 与直连对比**（在 Edge 宿主机）：

```bash
# 直连 Zentinel（与冒烟一致）
curl -sS -k -w "\nHTTP=%{http_code}\n" -H "x-sag-app-id: app-001" -H "x-sag-user-id: u-admin" -H "x-sag-user-roles: admin" \
  "https://127.0.0.1:10080/dev/" | tail -n 2

# 经 admin-next 反代（与浏览器同源路径）
curl -sS -k -w "\nHTTP=%{http_code}\n" -H "x-sag-app-id: app-001" -H "x-sag-user-id: u-admin" -H "x-sag-user-roles: admin" \
  "http://127.0.0.1:3001/api-zentinel/dev/" | tail -n 2
```

若第一条 **200**、第二条 **502**，重点查 **`frontend-admin-next`** 的 **`ZENTINEL_PROXY_TARGET`**（compose 默认 **`https://example.com:10080`** + **`extra_hosts`**，见 §3）及容器内 **`npm run build`** 是否用当前 env。

---

## 2. Git 与稳定基线

| 项 | 值 |
|----|-----|
| **仓库（SSH）** | `git@192.168.14.10:digital-operation/secure_access_gateway_sag.git` |
| **Git 工作区根（`REPO_ROOT`）** | 克隆得到的目录即仓库根（远端工程名 `secure_access_gateway_sag`）；**compose、脚本、`.env` 均在此根执行**，与本地文件夹是否重命名为 `sag-cloud` 无关 |
| **Web** | `http://192.168.14.10/digital-operation/secure_access_gateway_sag` |
| **主开发分支** | `clean-main` |
| **稳定 tag（代码树）** | `stable/edge-baseline-20260507` → 提交 **`ff261f98`**（4873b9ca + admin-next BodyInit 修复） |
| **基线说明文件** | `docs/ops/STABLE_BASELINE.md` |

**克隆与子模块：**

```bash
git clone git@192.168.14.10:digital-operation/secure_access_gateway_sag.git
cd secure_access_gateway_sag   # 即 REPO_ROOT；若 clone 到其他路径则 cd 到该路径（须见 .git 与 docker-compose.edge.yml）
git checkout clean-main
git pull origin clean-main --ff-only
git submodule update --init --depth 1 proxy/core
```

**查看当前提交：**

```bash
cd "$REPO_ROOT"
git rev-parse HEAD
git log -1 --oneline
```

## 2b. `git pull` 后是否要做 Docker 重建（最小清单）

在 **`$REPO_ROOT`（仓库根，含 compose）** 下执行；**只 pull、不重建** 时，容器内仍可能是旧代码/旧 mock。

| 本轮 pull 动到的内容 | Edge | Intra |
|---------------------|------|-------|
| **`docker-compose.hscale-auth*.yml`**、`infra/nginx/sag-auth-lb.conf` | 按需 §3c 追加 Auth hscale 或全量 §3.0 命令 | — |
| **`infra/test-workload/mock_http_server.py`** 或 mock compose | — | `docker compose -f docker-compose.intra.yml -f docker-compose.release.intra.yml up -d --force-recreate mock-workload` |
| **`frontend-admin-next/`**、`next.config.js`、门户相关 | `docker compose -f docker-compose.edge.yml -f docker-compose.release.edge.yml up -d --build frontend-admin-next` | — |
| **仅 `scripts/*.ps1`、`scripts/*.sh`**（冒烟脚本） | 无需重建；本机重新执行脚本即可 | 无需重建 |
| **Rust 服务 / compose 命令变更** | 按需 `up -d --build` 或 `--force-recreate` 对应 service（见 §1b、§8） | 同理，换 **`docker-compose.intra.yml`** |

从 **Windows** 跑 **`smoke-portal-seven.ps1`** 的 **P 层**时，脚本使用 **`curl.exe -L`**（与 `.sh` 一致），避免 **`Invoke-WebRequest`** 遇 Next **308** 尾斜杠重定向时误判失败。

### 2c. `git pull` 冲突：`scripts/ops/cpuset-edge-28.env`

**现象**（Edge 上常见）：

```text
error: 您对下列文件的本地修改将被合并操作覆盖：
        scripts/ops/cpuset-edge-28.env
```

**原因**：曾在 Edge 手改绑核（例如 policy **3–6**），与仓库 **`SAG_EDGE_CPUSET_POLICY=4-7`** 不一致。

**处理（二选一）**：

```bash
cd "$REPO_ROOT"

# A. 以仓库为准（推荐：已手动 recreate policy 4-7 时）
git checkout -- scripts/ops/cpuset-edge-28.env
git pull origin clean-main --ff-only
grep SAG_EDGE_CPUSET_POLICY scripts/ops/cpuset-edge-28.env   # 期望 4-7

# B. 保留本地再合并
git stash push -m "local cpuset" -- scripts/ops/cpuset-edge-28.env
git pull origin clean-main --ff-only
git stash pop   # 若有冲突，保留 POLICY=4-7
```

**pull 后仅重建 policy**（不必整栈 down）：

```bash
docker compose \
  -f docker-compose.edge.yml -f docker-compose.release.edge.yml \
  -f docker-compose.hscale-edge.yml -f docker-compose.hscale-auth.yml \
  -f docker-compose.edge.perf.yml -f docker-compose.hscale-edge.perf.yml \
  -f docker-compose.hscale-auth.perf.yml \
  --env-file scripts/ops/cpuset-edge-28.env \
  up -d --force-recreate sag-policy

docker inspect sag-policy --format 'policy cpuset={{.HostConfig.CpusetCpus}}'   # 期望 4-7
```

---

## 3. Edge 上跑的组件（`docker-compose.edge.yml`）

| 服务名（compose） | 容器名（常见） | 宿主机端口（默认） | 说明 |
|-------------------|----------------|---------------------|------|
| postgres | sag-postgres | 127.0.0.1:5432 | Edge 上主库；宿主机仅回环访问，Edge 容器经 Docker 网络访问 |
| redis | sag-redis | 6379 | |
| fake-4a | sag-fake-4a | 19080 | |
| control-plane-admin | sag-control-plane-admin | 8090 | APISIX reconcile：`SAG_APISIX_ADMIN_BASE_URL` → Intra **9180** |
| sag-auth | sag-auth（单实例）或 **sag-auth-lb**（hscale-auth） | **8080** | 默认单容器 `sag-auth`；扩 Auth 后 **:8080 由 nginx LB** 分发到 `sag-auth` + `sag-auth-2`（§3.0、§3c） |
| sag-policy | sag-policy | 8081 | |
| stealth-tunnel-agent | sag-stealth-agent | 50051, 9104 | gRPC 隧道 |
| http-tunnel-bridge | 服务名 `http-tunnel-bridge`（无固定 `container_name` 时可 `compose ps -q`） | 9000 | 见 **§3b** 可多副本；客户端应走 **Zentinel :10080** |
| zentinel | sag-zentinel | 10080 | **release** 需 **`proxy/core/target/release/zentinel`**；多 bridge 时换 **hscale kdl**（§3b） |
| company-demo-sites | sag-company-demo-sites | 28080 | |
| frontend-admin-next | sag-frontend-admin-next | **3001** | `npm ci` + `build` + `start`，首次较慢 |
| prometheus / grafana / otel / node-exporter | 见 compose | 9091 / 3000 / 4317-4318… | |
| **不在 Edge** | — | — | **sag-connector** 仅在 Intra |

**`docker compose restart` 须用左列「服务名」**，不可写容器名（否则报 `no such service`）。例：`restart stealth-tunnel-agent control-plane-admin`；若习惯容器名则用 **`docker restart sag-stealth-agent`**。

**Docker 网络（Edge compose）**：默认 bridge **`172.19.0.0/16`**，`zentinel` 固定 **`172.19.0.250`**；`frontend-admin-next` 有 **`example.com:172.19.0.250`** 的 `extra_hosts`（TLS/SNI 演示用）。

---

## 3.0 当前扩展状态快照（便于交接；换机后请改本表）

**记录日期**：**2026-05-20** · **Edge** `172.16.9.107`（28 逻辑核）· **Intra** `192.168.9.26`（8 逻辑核）· **压测** Windows `172.16.9.108`

### Edge — 已落地 vs 默认单实例

| 组件 | 当前推荐状态 | compose / 绑核 | 说明 |
|------|----------------|----------------|------|
| **http-tunnel-bridge** | **2 副本** | `hscale-edge.yml`：`http-tunnel-bridge` + `http-tunnel-bridge-2`；cpuset **12–14 / 15–17** | 宿主机 **:9000** 仅第一副本；业务入口 **Zentinel :10080** |
| **zentinel** | **1 实例，双 upstream** | `dataplane-compose.hscale.kdl`；cpuset **18–25** | RR 到两个 bridge；确认 **release** 二进制 |
| **stealth-tunnel-agent** | **1 实例** | cpuset **7–11** | 多 agent 需分片，见主计划 §1 |
| **sag-auth** | **2 副本 + nginx LB（推荐）** | `hscale-auth.yml`；**auth=2, auth-2=3, LB=26** | 北向 **:8080** → `sag-auth-lb`；**nofile=1048576** |
| **sag-policy** | 单实例 | cpuset **4–7**（**勿用旧 3–6**，与 auth-2 抢核） | `scripts/ops/cpuset-edge-28.env` |
| **sag-control** | 单实例 | cpuset **1** | |
| **构建** | **release** | `docker-compose.release.edge.yml` | 压测勿长期 `cargo run`（bridge-2/zentinel 重点查） |

**Edge 全量 hscale（数据面 + Auth）一键命令** — 在 **`$REPO_ROOT`**，且已 §6 编过 release：

```bash
cd "$REPO_ROOT"

docker compose \
  -f docker-compose.edge.yml \
  -f docker-compose.release.edge.yml \
  -f docker-compose.hscale-edge.yml \
  -f docker-compose.hscale-auth.yml \
  -f docker-compose.edge.perf.yml \
  -f docker-compose.hscale-edge.perf.yml \
  -f docker-compose.hscale-auth.perf.yml \
  --env-file scripts/ops/cpuset-edge-28.env \
  up -d --force-recreate
```

**仅数据面 hscale（维持当前、不扩 Auth）** — 与上条相比去掉 `*hscale-auth*` 相关 `-f`：

```bash
docker compose \
  -f docker-compose.edge.yml \
  -f docker-compose.release.edge.yml \
  -f docker-compose.hscale-edge.yml \
  -f docker-compose.edge.perf.yml \
  -f docker-compose.hscale-edge.perf.yml \
  --env-file scripts/ops/cpuset-edge-28.env \
  up -d --force-recreate
```

**Edge 验证**：

```bash
EDGE_IP=172.16.9.107 bash scripts/ops/verify-hscale-edge.sh
```

### Intra — 已落地

| 组件 | 绑核（`cpuset-intra-8.env`） | 说明 |
|------|------------------------------|------|
| apisix | 1–4 | 数据面入口 **9080** |
| mock-workload | 5–6 | 压测常先饱和 mock |
| sag-connector | 7 | `SAG_TUNNEL_ENDPOINT=https://172.16.9.107:50051` |
| etcd | 0 | |

```bash
# Intra 启 perf + 绑核（在 Intra 的 REPO_ROOT）
docker compose -f docker-compose.intra.yml -f docker-compose.release.intra.yml \
  -f docker-compose.intra.perf.yml \
  --env-file scripts/ops/cpuset-intra-8.env up -d --force-recreate
```

### 压测结论摘要（Windows k6 → Edge）

| 场景 | 目标 | 口径 | 结果要点 |
|------|------|------|----------|
| 数据面 only | 1000→1500→2000→3000 iter/s | `apisix_routed` | 成功率 **~97–99%**；实际 iter/s ≈ 目标 **97%** |
| 全链路 mixed | 3000 iter/s | login+policy+dataplane | **整链 ~40%**；**login ~0.5%** |
| Auth **单实例** | 2000 iter/s | `auth_login_verify` | **login ~79%**，verify ~100% |
| Auth **hscale + nofile** | 2000 iter/s | 同上 | **2026-05-20**：**chain 56%**，login 59%，verify 95%；**未过 90%** |

**完整命令与 artifact 命名见 §3d**；历史 JSON 在仓库 **`sag-cloud/artifacts/`**（Windows 本机路径同上）。

---

## 3b. 水平扩展：选哪些组件、开几个副本、和邻居怎么配合

### 是不是「一条命令就能在 Docker 里起很多个同一组件」？

对 **Compose 支持 `scale` 的服务**，可以：

```bash
cd "$REPO_ROOT"
docker compose -f docker-compose.edge.yml -f docker-compose.release.edge.yml up -d --scale http-tunnel-bridge=2
```

**但不是所有服务都适合** `--scale`，也**不是**靠「统一消息队列」把所有邻居都串起来——下面按 **数据怎么流** 说明。

### 相邻组件之间靠什么？（不全是 MQ）

| 链路 | 机制 | 说明 |
|------|------|------|
| **Zentinel → bridge** | HTTP 反向代理 +（可选）**upstream 轮询** | 多 bridge 时由 **KDL** 配多个 `target` 或依赖 DNS；见 `docs/ops/horizontal-scale-edge-bridge.md`。 |
| **bridge → stealth-tunnel-agent** | **Unary gRPC**（每 bridge 进程内还可 **多 Channel**，`SAG_BRIDGE_GRPC_CHANNEL_POOL_SIZE`） | 同步 RPC；不是 Kafka。 |
| **agent ↔ sag-connector** | **每个 Connector 副本一条长连接**上的 **双向流**（Register / Heartbeat / Request-Response） | `connector_endpoint` 现在是逻辑会话组；同组可注册多个 generation，Agent 只在 10 秒租约内的 session 间轮询。副本必须使用不同 `SAG_CONNECTOR_ID` 和不同客户端证书，但可以设置相同 `SAG_CONNECTOR_ENDPOINT` 实现滚动发布/active-active。每张证书指纹都要加入 Edge 的 `SAG_CONNECTOR_CERT_BINDINGS`。 |
| **bridge 过载削峰** | **Redis Stream**（`SAG_BRIDGE_REDIS_URL`，默认 DB `/2`）+ HTTP **202** + 客户端 **poll** | 这是 **队列/生产消费**，但范围限于 **bridge 入队 ↔ bridge worker 出队**，不改变 agent↔connector 隧道语义。 |
| **control-plane-admin → APISIX** | **Admin HTTP API** + etcd | 控制面下发路由，不是业务 MQ。 |
| **connector → APISIX → mock** | **同步 HTTP** | 扩 mock 通常要 **APISIX upstream 多节点** 或 **多实例 + LB**，见 `docs/ops/intra-mock-apisix-horizontal.md`。 |

### Edge：谁适合「水平分身」、命令怎么写

在 **`$REPO_ROOT`** 下（与 §6 相同 compose 文件）。

| 服务（compose 名） | 是否适合 `up -d --scale …=N`（N>1） | 建议 N / 注意 |
|--------------------|--------------------------------------|----------------|
| **http-tunnel-bridge** | **适合**（已去掉固定 `container_name`） | 先试 **2**；观察 **agent CPU**、**`SAG_BRIDGE_MAX_TUNNEL_INFLIGHT`**。宿主机 **9000** 仅会绑到 **第一个** 副本，业务入口请走 **Zentinel :10080**。 |
| **zentinel** | 一般 **保持 1**；若要多副本需前置 **LB + 共享/一致配置** | 多实例时证书、SNI、`10080` 端口冲突需自行设计。 |
| **stealth-tunnel-agent** | **当前不适合**多副本（进程内 `ConnectorRegistry`） | 多 agent 要外置注册或分片，见主计划 §1。 |
| **sag-auth** | **推荐 2 副本 + nginx LB**（`hscale-auth.yml`） | 宿主机 **:8080 → sag-auth-lb**；共享 **`SAG_SESSION_REDIS_URL`**；详见 **§3c**、[horizontal-scale-edge-auth.md](docs/ops/horizontal-scale-edge-auth.md)。 |
| **sag-policy / control-plane-admin** | **可** scale | 注意 **Postgres / Redis 连接池**。 |
| **postgres / redis** | **不要**用 compose `--scale` 当集群 | 高可用用主从、哨兵或云托管；bridge 队列依赖 **单一 Redis 逻辑**。 |

**推荐两条命令（二选一）**：

1. **仅扩 bridge 副本数**（同一服务名、Docker DNS 多任务）：

```bash
docker compose -f docker-compose.edge.yml -f docker-compose.release.edge.yml up -d --scale http-tunnel-bridge=2
```

2. **显式第二 bridge + Zentinel 双 upstream**（仓库自带 override，RR 最稳）：

```bash
docker compose -f docker-compose.edge.yml -f docker-compose.release.edge.yml -f docker-compose.hscale-edge.yml up -d
```

详情、回滚与指标：**[docs/ops/horizontal-scale-edge-bridge.md](docs/ops/horizontal-scale-edge-bridge.md)**。

### 3c. Edge：`sag-auth` 水平扩展（login 瓶颈）

**不适合**对 `sag-auth` 直接 `docker compose --scale sag-auth=2`：基线 compose 带固定 **`container_name: sag-auth`**。请用仓库 override：

| 文件 | 作用 |
|------|------|
| `docker-compose.hscale-auth.yml` | `sag-auth-2`、`sag-auth-lb`（nginx RR）；`sag-auth` 仅 `expose` |
| `docker-compose.hscale-auth.perf.yml` | cpuset：`AUTH=2`、`AUTH_2=3`、`AUTH_LB=26`；**policy=4-7**（避免与 auth-2 抢核 3） |
| `infra/nginx/sag-auth-lb.conf` | upstream `sag-auth` + `sag-auth-2` |

**在已有 bridge hscale 上追加 Auth**（不必整栈 down）：

```bash
cd "$REPO_ROOT"
docker compose \
  -f docker-compose.edge.yml \
  -f docker-compose.release.edge.yml \
  -f docker-compose.hscale-edge.yml \
  -f docker-compose.hscale-auth.yml \
  -f docker-compose.edge.perf.yml \
  -f docker-compose.hscale-edge.perf.yml \
  -f docker-compose.hscale-auth.perf.yml \
  --env-file scripts/ops/cpuset-edge-28.env \
  up -d --force-recreate sag-auth sag-auth-2 sag-auth-lb stealth-tunnel-agent frontend-admin-next
```

**回滚 Auth 扩展**：去掉所有 `*hscale-auth*` 文件后 `up -d --force-recreate sag-auth`（恢复单容器占 **8080**）。

**缓存（已有，非 MQ）**：`SAG_LOGIN_MEMO_*` + Redis — 热用户减 argon2/JWT CPU；**不替代** 水平扩展。全链路压测请 **`LoginEveryN` 拉大** 或 **`-SharedToken`**，避免每迭代 login。

操作细节：**[docs/ops/horizontal-scale-edge-auth.md](docs/ops/horizontal-scale-edge-auth.md)**。

**仅修正 policy 绑核（与 auth-2 错开 CPU 3）**：

```bash
cd "$REPO_ROOT"
docker compose \
  -f docker-compose.edge.yml -f docker-compose.release.edge.yml \
  -f docker-compose.hscale-edge.yml -f docker-compose.hscale-auth.yml \
  -f docker-compose.edge.perf.yml -f docker-compose.hscale-edge.perf.yml \
  -f docker-compose.hscale-auth.perf.yml \
  --env-file scripts/ops/cpuset-edge-28.env \
  up -d --force-recreate sag-policy
docker inspect sag-policy --format 'cpuset={{.HostConfig.CpusetCpus}}'   # 4-7
```

### 3d. 压测手册（Windows k6 → Edge，与生产路径一致）

**脚本**：`scripts/ops/run-load-dataplane.ps1` + `scripts/ops/load-dataplane-k6.js`  
**工作目录**：Windows 上 **`sag-cloud`**（含 `artifacts/`）。  
**前提**：本机已装 **k6**（`k6 version`）；Edge **:8080 / :10080** 从压测机 TCP 可达。

#### 3d.1 Windows 准备（建议每次压测前）

```powershell
cd D:\lxz\compile\Rust_project\Secure_Access_Gateway_SAG\sag-cloud
git pull origin clean-main

# 冒烟：Auth LB + 隧道
.\scripts\smoke-remote-windows.ps1 -EdgeHost 172.16.9.107 -IntraHost 192.168.9.26 -Rounds 1

# 可选：管理员 PowerShell 扩大临时端口（默认仅 49152-65535 约 16k）
netsh int ipv4 show dynamicport tcp
netsh int ipv4 set dynamicport tcp start=10000 num=55535
```

**登录冒烟**（与 k6 相同账号）：

```powershell
$body = '{"username":"admin","password":"Admin@123"}'
Invoke-RestMethod -Uri "http://172.16.9.107:8080/api/v1/auth/login" -Method POST -ContentType "application/json" -Body $body
```

#### 3d.2 Auth 门禁压测（`auth_login_verify`）

**口径**：每迭代 **POST login + POST verify**，无 token 缓存；**ConstantRps=2000** 时目标 **2000 iter/s**（Auth HTTP 约 **4000/s**）。  
**门禁**：**`sag_chain_success_rate` > 90%**（且 login、verify 分别 >90%）才晋级 **3000**。

```powershell
cd D:\lxz\compile\Rust_project\Secure_Access_Gateway_SAG\sag-cloud
$ts = Get-Date -Format "yyyyMMdd-HHmmss"

.\scripts\ops\run-load-dataplane.ps1 `
  -EdgeHost 172.16.9.107 `
  -RunMode capacity `
  -ScenarioType auth_login_verify `
  -GateProfile auth `
  -ConstantRps 2000 `
  -Stage1Duration 2m -Stage2Duration 2m -Stage3Duration 2m -Stage4Duration 2m `
  -PreAllocatedVUs 2000 -MaxVUs 10000 `
  -RequestTimeout 90s `
  -NoCapacityVuCap `
  -SummaryJson ".\artifacts\k6-auth-win-2000-$ts.json"
```

**跑完后看结果**（PowerShell）：

```powershell
$j = Get-Content ".\artifacts\k6-auth-win-2000-<时间戳>.json" -Raw | ConvertFrom-Json
$j.metrics.'sag_api_success_rate{api:auth_login}'.value
$j.metrics.'sag_api_success_rate{api:auth_verify}'.value
$j.metrics.sag_chain_success_rate.value
$j.metrics.dropped_iterations.count
$j.metrics.iterations.rate
```

**2026-05-20 实测参考**（hscale + policy 4–7）：login **0.59**，verify **0.95**，chain **0.56**，iter/s **~220**，`dropped_iterations` **805839** → 文件 **`artifacts/k6-auth-win-2000-20260520-135225.json`**。

**3000（仅当 2000 过关）**：

```powershell
.\scripts\ops\run-load-dataplane.ps1 `
  -EdgeHost 172.16.9.107 -ScenarioType auth_login_verify -GateProfile auth `
  -ConstantRps 3000 -Stage1Duration 2m -Stage2Duration 2m -Stage3Duration 2m -Stage4Duration 2m `
  -PreAllocatedVUs 3000 -MaxVUs 12000 -RequestTimeout 90s -NoCapacityVuCap `
  -SummaryJson ".\artifacts\k6-auth-win-3000-$ts.json"
```

#### 3d.3 数据面 only（`apisix_routed`）

**口径**：每迭代 1 次 **GET** `https://<Edge>:10080/dev/`；**5xx 可算成功**（隧道+APISIX 已路由）；**不是**全链路 login。

```powershell
.\scripts\ops\run-load-dataplane.ps1 `
  -EdgeHost 172.16.9.107 `
  -RunMode dataplane_only `
  -ScenarioType dataplane_only `
  -DataplaneSuccessMode apisix_routed `
  -GateProfile dataplane_routed `
  -ConstantRps 3000 `
  -Stage1Duration 2m -Stage2Duration 2m -Stage3Duration 2m -Stage4Duration 2m `
  -PreAllocatedVUs 3000 -MaxVUs 10000 `
  -NoCapacityVuCap `
  -SummaryJson ".\artifacts\k6-dp-3000-routed-$ts.json"
```

阶梯 1000→1500→2000→3000：去掉 `-ConstantRps`，用 `-StartQps` / `-Stage1Qps` … `-Stage4Qps` 与默认阶段时长。

#### 3d.4 全链路 mixed（易打穿 Auth，仅诊断用）

```powershell
.\scripts\ops\run-load-dataplane.ps1 `
  -EdgeHost 172.16.9.107 `
  -RunMode capacity `
  -ScenarioType mixed_fullchain `
  -ConstantRps 3000 `
  -DataplaneSuccessMode apisix_routed `
  -SteadyFullchain `
  -NoCapacityVuCap `
  -SummaryJson ".\artifacts\k6-fullchain-3000-routed-$ts.json"
```

#### 3d.5 Edge 侧：压测同期诊断（SSH 到 172.16.9.107）

```bash
export REPO_ROOT="${REPO_ROOT:-$HOME/secure_access_gateway_sag}"
cd "$REPO_ROOT"

# 绑核一览（env 设计值 + 实际 CpusetCpus）
grep SAG_EDGE_CPUSET scripts/ops/cpuset-edge-28.env
for c in sag-policy secure_access_gateway_sag-sag-auth-1 secure_access_gateway_sag-sag-auth-2-1 secure_access_gateway_sag-sag-auth-lb-1; do
  docker inspect "$c" --format '{{.Name}} cpuset={{.HostConfig.CpusetCpus}}' 2>/dev/null
done

# auth nofile
docker inspect secure_access_gateway_sag-sag-auth-1 --format '{{json .HostConfig.Ulimits}}'

# EMFILE / accept 错误（压测中另开终端 tail）
docker logs --since 5m secure_access_gateway_sag-sag-auth-1 2>&1 | grep -iE 'emfile|too many open|accept error' | tail -20
docker logs --since 5m secure_access_gateway_sag-sag-auth-2-1 2>&1 | grep -iE 'emfile|too many open|accept error' | tail -20
docker logs --since 5m secure_access_gateway_sag-sag-auth-lb-1 2>&1 | tail -50

# hscale 冒烟
EDGE_IP=172.16.9.107 bash scripts/ops/verify-hscale-edge.sh
```

**失败特征对照**：

| 日志/现象 | 可能原因 |
|-----------|----------|
| `axum::serve: accept error: Too many open files` | auth **nofile** 未生效 → 重建 auth + `hscale-auth.perf.yml` |
| nginx **499** 大量 | 客户端先断（Windows 端口/VU 不足） |
| `wsarecv: forcibly closed` / **EOF**（Windows k6） | Edge/LB 过载或客户端端口耗尽 |
| `connectex: Only one usage of each socket address` | Windows **临时端口用尽** → §3d.1 `netsh` |
| login 低、verify 高 | login/accept 路径瓶颈（非 verify JWT） |
| `dropped_iterations` 极大、实际 iter/s ≪ 目标 | k6 供给不足（VU/超时/客户端），不单是 Edge CPU |

### 3e. 压测机（麒麟 VM + Linux k6）— 推荐主路径

**定位**：专用 **负载发生器**，与 Edge / Intra **分离**；避免 Windows 客户端端口/网卡成为 Auth 瓶颈。  
**硬件（当前规划）**：**8 vCPU**、**64 GiB RAM**、OS **麒麟（Kylin）**（与 Ubuntu 系类似，按实际 deb/rpm 选安装方式）。  
**网络**：须能 TCP 访问 **`172.16.9.107:8080`**（Auth LB）、**`:10080`**（Zentinel）；**不要**在 Edge 本机跑 k6 与 Windows 单实例 79% 混比，除非刻意对照。

**脚本（仓库内，`REPO_ROOT` = 含 `docker-compose.edge.yml` 的目录）**：

| 文件 | 作用 |
|------|------|
| `scripts/ops/run-load-dataplane.sh` | Linux 版压测入口（对齐 `run-load-dataplane.ps1` 环境变量） |
| `scripts/ops/run-auth-gate-2000.sh` | **一键** Auth `auth_login_verify` @ **2000** + 写 log + 打印粘贴块 |
| `scripts/ops/print-k6-summary.sh` | 从 `k6 --summary-export` JSON 打印 **`SAG K6 RESULT PASTE BLOCK`** |
| `scripts/ops/load-dataplane-k6.js` | k6 场景（与 Windows 共用） |

**artifact 命名**：`artifacts/k6-auth-linux-2000-<yyyyMMdd-HHmmss>.json`（及同前缀 `.log`）。

---

#### 3e.0 一次性准备（麒麟 VM 上整段复制）

```bash
# --- 0) 变量：按本机改 IP / 路径 ---
export EDGE_HOST=172.16.9.107
export REPO_ROOT="${REPO_ROOT:-$HOME/secure_access_gateway_sag}"
export LOADGEN_IP="<本机内网IP>"   # 例 172.16.9.109，填好后便于交接记录

# --- 1) 依赖：curl、git、python3、jq（解析结果；无 jq 时脚本回退 python3）---
sudo apt-get update -y || sudo yum makecache -y || true
sudo apt-get install -y curl git python3 jq ca-certificates gnupg 2>/dev/null \
  || sudo yum install -y curl git python3 jq ca-certificates gnupg2 2>/dev/null || true

# --- 2) 安装 k6（任选一种成功即可）---

# 方式 A：Grafana 官方 apt（Debian/Ubuntu/多数麒麟 deb 系）
sudo gpg -k
sudo gpg --no-default-keyring --keyring /usr/share/keyrings/k6-archive-keyring.gpg \
  --keyserver hkp://keyserver.ubuntu.com:80 \
  --recv-keys C5AD17C747E3415A3642D57D77C6C491D6AC1D69 2>/dev/null || true
echo "deb [signed-by=/usr/share/keyrings/k6-archive-keyring.gpg] https://dl.k6.io/deb stable main" \
  | sudo tee /etc/apt/sources.list.d/k6.list
sudo apt-get update && sudo apt-get install -y k6

# 方式 B：若 apt 不可用 — 官方二进制（x86_64）
# K6_VER=v1.7.1
# curl -fsSL "https://github.com/grafana/k6/releases/download/${K6_VER}/k6-${K6_VER}-linux-amd64.tar.gz" -o /tmp/k6.tgz
# sudo tar -xzf /tmp/k6.tgz -C /usr/local/bin --strip-components=1 k6-${K6_VER}-linux-amd64/k6

k6 version

# --- 3) 拉代码（SSH 需已配 GitLab 密钥）---
mkdir -p "$(dirname "$REPO_ROOT")"
if [[ ! -d "$REPO_ROOT/.git" ]]; then
  git clone git@192.168.14.10:digital-operation/secure_access_gateway_sag.git "$REPO_ROOT"
fi
cd "$REPO_ROOT"
git checkout clean-main
git pull origin clean-main --ff-only
chmod +x scripts/ops/run-load-dataplane.sh scripts/ops/print-k6-summary.sh scripts/ops/run-auth-gate-2000.sh

# --- 4) 本机内核参数（临时端口，压测前）---
echo "ip_local_port_range_before: $(cat /proc/sys/net/ipv4/ip_local_port_range)"
sudo sysctl -w net.ipv4.ip_local_port_range="10000 65535"
echo "ip_local_port_range_after: $(cat /proc/sys/net/ipv4/ip_local_port_range)"

# --- 5) 连通 + Auth 冒烟（应与 k6 同账号）---
for port in 8080 8081 8090 10080; do
  timeout 2 bash -c "echo >/dev/tcp/${EDGE_HOST}/${port}" && echo "TCP OK ${EDGE_HOST}:${port}" || echo "TCP FAIL ${EDGE_HOST}:${port}"
done
curl -sS -o /dev/null -w "login_http=%{http_code}\n" \
  -X POST "http://${EDGE_HOST}:8080/api/v1/auth/login" \
  -H 'Content-Type: application/json' \
  -d '{"username":"admin","password":"Admin@123"}'
```

---

#### 3e.1 Auth 门禁 @ 2000（主测，约 8 分钟）

```bash
export EDGE_HOST=172.16.9.107
export REPO_ROOT="${REPO_ROOT:-$HOME/secure_access_gateway_sag}"
cd "$REPO_ROOT"

# 一键：压测 + tee 日志 + 结束自动打印 PASTE BLOCK
./scripts/ops/run-auth-gate-2000.sh
```

等价手动命令：

```bash
TS=$(date +%Y%m%d-%H%M%S)
./scripts/ops/run-load-dataplane.sh \
  --edge-host "$EDGE_HOST" \
  --run-mode capacity \
  --scenario-type auth_login_verify \
  --gate-profile auth \
  --constant-rps 2000 \
  --stage1-duration 2m --stage2-duration 2m --stage3-duration 2m --stage4-duration 2m \
  --pre-allocated-vus 2000 --max-vus 10000 \
  --request-timeout 90s \
  --no-capacity-vu-cap \
  --summary-json "./artifacts/k6-auth-linux-2000-${TS}.json"
```

**跑完后必须贴回的内容**（终端里搜索 `SAG K6 RESULT PASTE BLOCK`，**两行 `==========` 之间整段复制**）：

```bash
# 若已跑完、只需重打粘贴块：
LATEST=$(ls -1t ./artifacts/k6-auth-linux-2000-*.json 2>/dev/null | head -1)
./scripts/ops/print-k6-summary.sh "$LATEST" auth_login_verify 2000
```

粘贴块字段含义（Agent 据此判门禁）：

| 字段 | 含义 |
|------|------|
| `paste_login_success_rate` | login 成功率，**>90%** 过关 |
| `paste_verify_success_rate` | verify 成功率，**>90%** 过关 |
| `paste_chain_success_rate` | 整链（login+verify），**>90%** 过关 |
| `paste_gate_90_all` | **PASS** 才可晋级 3000 |
| `paste_iterations_per_sec` | 实际 iter/s（目标 2000） |
| `paste_dropped_iterations` | k6 丢弃迭代数（过大=供给不足） |
| `paste_http_req_failed_rate` | HTTP 失败比例 |
| `paste_summary_json` | 本地 JSON 路径 |

**晋级 3000**（仅当 `paste_gate_90_all=PASS`）：

```bash
TS=$(date +%Y%m%d-%H%M%S)
./scripts/ops/run-load-dataplane.sh \
  --edge-host "$EDGE_HOST" \
  --scenario-type auth_login_verify --gate-profile auth \
  --constant-rps 3000 \
  --stage1-duration 2m --stage2-duration 2m --stage3-duration 2m --stage4-duration 2m \
  --pre-allocated-vus 3000 --max-vus 12000 \
  --request-timeout 90s --no-capacity-vu-cap \
  --summary-json "./artifacts/k6-auth-linux-3000-${TS}.json"
./scripts/ops/print-k6-summary.sh "./artifacts/k6-auth-linux-3000-${TS}.json" auth_login_verify 3000
```

---

#### 3e.2 数据面回归（`apisix_routed`，可选）

```bash
TS=$(date +%Y%m%d-%H%M%S)
./scripts/ops/run-load-dataplane.sh \
  --edge-host "$EDGE_HOST" \
  --run-mode dataplane_only \
  --scenario-type dataplane_only \
  --dataplane-success-mode apisix_routed \
  --gate-profile dataplane_routed \
  --constant-rps 3000 \
  --stage1-duration 2m --stage2-duration 2m --stage3-duration 2m --stage4-duration 2m \
  --pre-allocated-vus 3000 --max-vus 10000 \
  --no-capacity-vu-cap \
  --summary-json "./artifacts/k6-dp-linux-3000-routed-${TS}.json"
./scripts/ops/print-k6-summary.sh "./artifacts/k6-dp-linux-3000-routed-${TS}.json" dataplane_only 3000
```

---

#### 3e.3 压测同期 Edge 诊断（SSH 到 172.16.9.107）

与 **§3d.5** 相同（policy **4–7**、auth nofile、nginx/auth 日志）。在 **另一终端** 于压测进行时执行。

---

#### 3e.4 交接记录模板（填 LOADGEN_IP 后写入 §0.5 表）

```text
LOADGEN_IP=<麒麟VM IP>  EDGE=172.16.9.107  scenario=auth_login_verify  target_rps=2000
artifact=artifacts/k6-auth-linux-2000-<ts>.json
(paste 整段 SAG K6 RESULT PASTE BLOCK)
```

### Intra：谁适合分身

| 服务 | 说明 |
|------|------|
| **mock-workload** | 可起第二实例并改 **APISIX upstream**；默认 compose 未提交双 mock，避免与单机端口演示冲突。 |
| **apisix** | 多实例通常 **VIP/LB + 多 etcd 或共享 etcd**；connector 的 `SAG_APISIX_BASE_URL` 要指向 **入口 VIP**。 |
| **sag-connector** | **不要**对同一 `SAG_CONNECTOR_ID` / `endpoint` 做无差别 scale；要 **分片** 则新进程 + 新 ID + 控制面路由。 |

操作与配置要点：**[docs/ops/intra-mock-apisix-horizontal.md](docs/ops/intra-mock-apisix-horizontal.md)**。

### 运维命令速查（多副本后）

```bash
# 看所有 bridge 任务日志（聚合服务名）
docker compose -f docker-compose.edge.yml -f docker-compose.release.edge.yml logs --tail=200 http-tunnel-bridge

# 进「某一个」bridge 容器（多副本时 compose 默认进其一；需指定可加 --index，视 compose 版本）
docker compose -f docker-compose.edge.yml -f docker-compose.release.edge.yml exec http-tunnel-bridge sh -c 'ulimit -n'
```

---

## 4. Intra 上跑的组件（`docker-compose.intra.yml`）

| 服务名 | 容器名 | 端口 | 说明 |
|--------|--------|------|------|
| etcd | sag-etcd | 2379 | |
| apisix | sag-apisix | **9080** 数据面、**9180** Admin | 配置：`infra/apisix/config.yaml`；`admin_key.key` 默认 **`your-admin-key`** |
| mock-workload | sag-mock | 18080 | |
| company-demo-sites | sag-company-demo-sites | 28080 | |
| sag-connector | sag-connector | 9103 等 | **`env_file: ./.env.intra`**；连 Edge 隧道并转发到 Intra APISIX，不连接数据库 |
| metrics-gateway | sag-metrics-gateway | 19090 | |

---

## 5. 环境变量与「路由」相关配置（文件级）

### Edge：仓库根 `.env`（不入库）

- **模板（入库）**：`edge-host.env.example` —— `cp edge-host.env.example .env`
- **常用键**：
  - **`SAG_PUBLIC_HOST`**：浏览器/Fake4A/SSO 外链可见主机（新 Edge 例：`172.16.9.107`）
  - **`SAG_APISIX_ADMIN_BASE_URL`**：例 `http://192.168.9.26:9180`
  - **`SAG_APISIX_ADMIN_API_KEY`**：须与 **`infra/apisix/config.yaml`** 里 **`deployment.admin.admin_key[].key`** 一致（默认 **`your-admin-key`**）
  - **`NEXT_PUBLIC_GRAFANA_URL`** / **`NEXT_PUBLIC_PROMETHEUS_URL`**：填 **当前 Edge 对外可达** 的 3000 / 9091（模板里若仍为 `192.168.8.87`，请改成现网 IP，并 **`--build frontend-admin-next`** 使 `NEXT_PUBLIC_*` 生效）

### Intra：`.env.intra`（不入库；由团队维护）

- **`docker-compose.intra.yml`** 中 **`sag-connector`** 使用 **`env_file: ./.env.intra`**
- **必须核对**：`SAG_TUNNEL_ENDPOINT`（Edge **50051** gRPC）、Connector 身份与 TLS 路径；`SAG_APISIX_BASE_URL` 多在 compose 里默认 `http://apisix:9080`。勿在 `.env.intra` 里把 **`SAG_CONNECTOR_ID`** 设成与 bootstrap 不一致的值（见 §1d）。Connector 不需要数据库 DSN，中央持久化由 Edge 服务负责。

### 双机变量命名参考（文档/脚本）

- **`/.env.dualhost.example`**：变量命名与 smoke 脚本一致，可复制片段到各机 `.env`

### APISIX 与数据面路由

- **静态配置**：`infra/apisix/config.yaml`（`admin_key`、`etcd` 前缀等）
- **动态路由**：由 **control-plane-admin** 调 Admin API 下发；依赖 Edge 上 **`SAG_APISIX_*`** 与网络可达 **9180**

### Zentinel 数据面

- **release 启动配置**：`proxy/zentinel-proxy/config/dataplane-compose.kdl`（compose 挂载进容器）

### 前端（admin-next）反向代理

- **`frontend-admin-next/next.config.js`**：`rewrites` 使用 **`CONTROL_PROXY_TARGET`**、**`AUTH_PROXY_TARGET`**、**`ZENTINEL_PROXY_TARGET`** 等（**构建期**读容器内环境变量）。
- **`docker-compose.edge.yml`** 中 **`frontend-admin-next`**：`ZENTINEL_PROXY_TARGET` 默认 **`https://example.com:10080`**，与 **`extra_hosts: example.com:172.19.0.250`**（Zentinel 在 bridge 网中的地址）配套，以满足默认 TLS 证书 SAN；若自定义 **`ZENTINEL_PROXY_TARGET`**，须保证容器内能解析且 TLS 与证书一致。

### Cargo / Zentinel 构建时 Git

- **`infra/docker/zentinel-gitconfig`**：挂载为容器内 `/root/.gitconfig` —— **勿**再默认使用易 502 的 `gitclone.com` 重写 `github.com`（已修正）；若环境必须走镜像，自行改该文件或换可达镜像。

---

## 6. 生产模式：Edge 全量编译 + 启动（可复制）

在 **`REPO_ROOT`** 执行；**先**有 **`.env`**（见 §5）。

```bash
cd "$REPO_ROOT"

# 1) zentinel release 二进制（产物在宿主机 ./proxy/core/target/release/zentinel）
docker compose -f docker-compose.edge.yml run --rm zentinel \
  cargo build --release -p zentinel-proxy --bin zentinel \
  --manifest-path /workspace/proxy/core/Cargo.toml

# 2) 其余 Rust workspace release（首次较慢）
docker compose -f docker-compose.edge.yml run --rm control-plane-admin \
  cargo build --workspace --release

# 3) 生产覆盖 + 后台全栈
docker compose -f docker-compose.edge.yml -f docker-compose.release.edge.yml up -d --build
```

**仅前台看前端日志（3001）**：

```bash
docker compose -f docker-compose.edge.yml -f docker-compose.release.edge.yml up frontend-admin-next
```

**自检：**

```bash
docker compose -f docker-compose.edge.yml -f docker-compose.release.edge.yml ps
curl -sS -o /dev/null -w "%{http_code}\n" http://127.0.0.1:8090/health
curl -sS -o /dev/null -w "%{http_code}\n" http://127.0.0.1:3001/
```

---

## 7. 生产模式：Intra（可复制）

在 **Intra 机器** 的 **`REPO_ROOT`**：

```bash
cd "$REPO_ROOT"
# 确认 .env.intra 存在且 TUNNEL 指向当前 Edge

docker compose -f docker-compose.intra.yml run --rm sag-connector \
  cargo build -p sag-connector --release

docker compose -f docker-compose.intra.yml -f docker-compose.release.intra.yml up -d --build
```

---

## 8. 只改了某几个组件时如何重启

在对应机器 **`REPO_ROOT`**：

```bash
# 例：只重建并重启 Rust 控制面
docker compose -f docker-compose.edge.yml -f docker-compose.release.edge.yml up -d --build control-plane-admin

# 例：zentinel 改 KDL / 二进制后
docker compose -f docker-compose.edge.yml -f docker-compose.release.edge.yml up -d --build zentinel

# 例：仅前端 admin-next
docker compose -f docker-compose.edge.yml -f docker-compose.release.edge.yml up -d --build frontend-admin-next

# 跟日志
docker compose -f docker-compose.edge.yml -f docker-compose.release.edge.yml logs -f zentinel
```

Intra 同理，把 **compose 文件** 换成 **`docker-compose.intra.yml` + `docker-compose.release.intra.yml`**，服务名换 **`apisix`** / **`sag-connector`** 等。

---

## 8b. 从 Windows 对新 Edge 冒烟（双机）

在 **`sag-cloud`** 目录（PowerShell）：

```powershell
.\scripts\smoke-remote-windows.ps1 -EdgeHost 172.16.9.107 -IntraHost 192.168.9.26 -Rounds 1
```

脚本会设置 **`EDGE_BASE_URL`**、**`INTRA_APISIX_DATA_BASE_URL`**、**`MOCK_BASE_URL`**，并将 **`HDR_APP`** 默认为 **`app-001`**（与 bootstrap demo 隧道路由一致）。

**与一键体检的差异**：体检页默认探测 **`/api/test`**（经 APISIX 改写到 **`/test/`**）；用户门户「网关探测」走 **`/dev/`、`/ci/`…**。若体检 200 而门户 502，用下面 **七路径冒烟** 对齐门户行为。经 **:3001** 的 P 层与 **`page.tsx` 一致**，使用 **`/api-zentinel/dev/`**（保留 path 尾 **`/`**）；Bash 脚本用 **`curl -L`** 跟随尾斜杠 **308**。**`mock_http_server`** 同时接受 **`/dev`** 与 **`/dev/`**，避免经 Next 规范化后落到无尾斜杠路径时出现 **404**。

```powershell
# 七路径 + 可选经 :3001（与浏览器同源）
.\scripts\smoke-portal-seven.ps1 -EdgeHost 172.16.9.107 -IntraHost 192.168.9.26 -IncludeAdminNext
```

Edge 本机 Bash：

```bash
export REPO_ROOT="${REPO_ROOT:-$HOME/secure_access_gateway_sag}"
cd "$REPO_ROOT"
chmod +x scripts/smoke-portal-seven.sh
EDGE_BASE_URL=http://127.0.0.1 INTRA_APISIX_DATA_BASE_URL=http://192.168.9.26:9080 \
  ADMIN_NEXT_BASE_URL=http://127.0.0.1:3001 ./scripts/smoke-portal-seven.sh
```

**结果粗判**：

| 层 | 含义 |
|----|------|
| **N1** zentinel HTTPS | 失败多为 **zentinel 未起 / TLS / 链路到 bridge** |
| **T1** bridge | 失败而 **S1** 直连 APISIX 正常 → **隧道侧（agent/bridge/connector 连错 Edge）** |
| **S1** APISIX 直连 | 失败 → **Intra 路由/上游** 或 APISIX 未起 |

**老 Edge 未关、Intra 仍连旧 IP**：`sag-connector` 使用 **`SAG_TUNNEL_ENDPOINT`** 决定连接哪台 Edge，配置来自 **Intra 上 `.env.intra` / compose 环境**；这不是「同时只能连一台」的独占锁，但 Connector 只会连你配置的那台。若仍指向 **`192.168.8.87`**，新 Edge **172.16.9.107** 上的隧道不会被 Connector 使用。应把 Intra 的 **`SAG_TUNNEL_ENDPOINT`** 改成 **当前生产 Edge**，再 **`docker compose ... up -d --force-recreate sag-connector`**。旧 Edge 可关可留，**关键是隧道端点指向谁**。

---

## 9. 放置较久后：一键 down 再拉起（不删数据卷）

**Edge：**

```bash
cd "$REPO_ROOT"
docker compose -f docker-compose.edge.yml -f docker-compose.release.edge.yml down
docker compose -f docker-compose.edge.yml -f docker-compose.release.edge.yml up -d
```

**Intra：**

```bash
cd "$REPO_ROOT"
docker compose -f docker-compose.intra.yml -f docker-compose.release.intra.yml down
docker compose -f docker-compose.intra.yml -f docker-compose.release.intra.yml up -d
```

**警告**：若使用 **`down -v`** 会删命名卷（含 **Postgres 数据**、**etcd**、**node_modules** 等），仅在你明确要清空数据时使用。

---

## 10. 常用诊断命令

```bash
docker compose -f docker-compose.edge.yml -f docker-compose.release.edge.yml ps -a
docker logs --tail 200 sag-zentinel
docker compose -f docker-compose.edge.yml -f docker-compose.release.edge.yml logs --tail 200 http-tunnel-bridge
docker logs --tail 200 sag-stealth-agent
docker logs --tail 200 sag-frontend-admin-next
docker exec sag-apisix grep -A3 admin_key /usr/local/apisix/conf/config.yaml   # 在 Intra
curl -sS -o /dev/null -w "%{http_code}\n" -H "X-API-KEY: your-admin-key" "http://192.168.9.26:9180/apisix/admin/routes"
```

**Edge Postgres：隧道路由与上游（库名 `sag`）**

```bash
docker exec -it sag-postgres psql -U postgres -d sag -c "SELECT host, app_id, connector_endpoint FROM tunnel_routes ORDER BY app_id;"
docker exec -it sag-postgres psql -U postgres -d sag -c "SELECT app_id, upstream, scheme FROM intranet_upstreams ORDER BY app_id;"
```

**Edge 本机数据面（与 `scripts/smoke-dataplane.ps1` 一致；`INTRA` 改成你的内网机）**

```bash
INTRA=192.168.9.26
curl -sS -k --http1.1 -w "\nN1 HTTP %{http_code}\n" -o /tmp/n1.txt \
  -H "x-sag-app-id: app-001" -H "x-sag-user-id: u-admin" -H "x-sag-user-roles: admin" \
  "https://127.0.0.1:10080/dev/" && head -c 200 /tmp/n1.txt && echo
curl -sS -w "\nT1 HTTP %{http_code}\n" -o /tmp/t1.txt \
  -H "x-sag-app-id: app-001" -H "x-sag-user-id: u-admin" -H "x-sag-user-roles: admin" \
  "http://127.0.0.1:9000/dev/" && head -c 200 /tmp/t1.txt && echo
curl -sS -w "\nS1 HTTP %{http_code}\n" -o /tmp/s1.txt \
  -H "x-sag-app-id: app-001" -H "x-sag-user-id: u-admin" -H "x-sag-user-roles: admin" \
  "http://${INTRA}:9080/dev/" && head -c 200 /tmp/s1.txt && echo
```

**拉 agent 路由（注意：登录在 8080）**

```bash
TOKEN=$(curl -sS -X POST http://127.0.0.1:8080/api/v1/auth/login \
  -H 'Content-Type: application/json' \
  -d '{"username":"admin","password":"Admin@123"}' | jq -r '.token // empty')
curl -sS "http://127.0.0.1:8090/api/v1/agent/routes?app_id=app-001" -H "Authorization: Bearer ${TOKEN}"
```

**Auth hscale：cpuset / nofile / LB 快速核对**

```bash
export REPO_ROOT="${REPO_ROOT:-$HOME/secure_access_gateway_sag}"
cd "$REPO_ROOT"

docker compose -f docker-compose.edge.yml -f docker-compose.release.edge.yml \
  -f docker-compose.hscale-edge.yml -f docker-compose.hscale-auth.yml \
  -f docker-compose.edge.perf.yml -f docker-compose.hscale-auth.perf.yml \
  ps -q sag-auth sag-auth-2 sag-auth-lb | while read id; do
  name=$(docker inspect "$id" --format '{{.Name}}' | sed 's/^\///')
  echo "$name -> cpuset=$(docker inspect "$id" --format '{{.HostConfig.CpusetCpus}}')"
done

docker inspect secure_access_gateway_sag-sag-auth-1 --format 'auth-1 ulimits={{json .HostConfig.Ulimits}}' 2>/dev/null
docker inspect secure_access_gateway_sag-sag-auth-2-1 --format 'auth-2 ulimits={{json .HostConfig.Ulimits}}' 2>/dev/null

curl -sS -o /dev/null -w "LB login HTTP %{http_code}\n" -X POST http://127.0.0.1:8080/api/v1/auth/login \
  -H 'Content-Type: application/json' -d '{"username":"admin","password":"Admin@123"}'
```

**Windows 压测机**：源 IP 一般为 **`172.16.9.108`**；结果 JSON 在 **`sag-cloud/artifacts/k6-*.json`**，日志可用 `Tee-Object` 存 **`artifacts/k6-*.log`**。

---

## 11. 与本文档同级的其它参考

| 文件 | 用途 |
|------|------|
| `DEPLOYMENT_README.md` | 通用部署、端口、健康检查 |
| `DEPLOYMENT_README_FRESH_UBUNTU.md` | 新机、GitHub 镜像、子模块排障 |
| `edge-host.env.example` | Edge `.env` 模板 |
| `intra-host.env.example` | Intra **`.env.intra`** 模板（connector → 新 Edge） |
| `.env.dualhost.example` | 双机变量命名参考 |
| `infra/storage-seed/README.md` | SQLite/Postgres 种子说明、`company_demo` 与 **仅补 `app-001`** 的 SQL |
| `infra/storage-seed/bootstrap_app001_dualhost_postgres.sql` | 在已存在其它 `tunnel_routes` 行时补 **`app-001`** + mock 上游 |
| `docs/ops/horizontal-scale-edge-bridge.md` | Edge 多 bridge：`--scale` 与 `docker-compose.hscale-edge.yml`、Zentinel kdl、端口与日志 |
| `docs/ops/horizontal-scale-edge-auth.md` | Edge 双 `sag-auth` + nginx **:8080** LB、与 bridge hscale 叠加命令 |
| `scripts/ops/cpuset-edge-28.env` | Edge 28 核绑核（含 bridge-2、zentinel hscale、auth/auth-2） |
| `scripts/ops/cpuset-intra-8.env` | Intra 8 核绑核 |
| `scripts/ops/run-load-dataplane.ps1` | Windows k6 压测入口（legacy；Auth 门禁优先 §3e） |
| `scripts/ops/run-load-dataplane.sh` | **Linux/麒麟** k6 压测入口 |
| `scripts/ops/run-auth-gate-2000.sh` | **一键** Auth @ 2000 + PASTE BLOCK |
| `scripts/ops/print-k6-summary.sh` | 压测结果粘贴块（给 Agent/同事） |
| `scripts/ops/load-dataplane-k6.js` | k6 场景与 `apisix_routed` / `auth_login_verify` 口径 |
| `scripts/ops/verify-hscale-edge.sh` | Edge hscale 冒烟（含 auth LB login；**:9104 为 agent**） |
| `sag-cloud/artifacts/k6-*.json` | 压测 summary 归档（Windows 本机，可选提交样例） |
| `scripts/ops/verify-hscale-intra.sh` | Intra 冒烟 |
| `docs/ops/intra-mock-apisix-horizontal.md` | Intra：APISIX / mock 水平扩展说明（与默认 compose 的关系） |
| `docs/ops/high-concurrency-reliability-master-plan.md` | 高并发总计划（含 §1 落地状态、connector 分片等） |
| `docs/ops/backpressure-queue-runbook.md` | bridge **背压与 Redis 202 队列**：env、Redis、`/metrics` 判定树、k6 poll 口径、回滚 |
| `docs/ops/rate-limit-circuit-breaker-runbook.md` | **限流与熔断**（主计划 §3）：connector/agent env 与指标、Zentinel/APISIX checklist、判定树、与背压手册衔接 |
| `docs/ops/timeout-deadline-runbook.md` | **超时与线程预算**（主计划 §4）：全链阶梯表、k6 status 0 vs 5xx、`verify-timeout-chain` 脚本 |
| `docs/ops/cache-read-runbook.md` | **缓存与读多写少**（§5）：可缓存路径、policy/agent/auth 指标、APISIX 试点 |
| `docs/ops/async-patterns-runbook.md` | **异步化**（§6）：202 队列、审计 spawn、connector 有界队列 |
| `docs/ops/implementation-roadmap.md` | **实施路线图**（§7）：P0–P3 可执行清单 |
| `docs/ops/docs-maintenance-runbook.md` | **文档与基线维护**（§8）：修订记录、k6 JSON 归档 |
| `scripts/smoke-remote-windows.ps1` | 双机分层冒烟（Windows） |
| `scripts/smoke-portal-seven.ps1` / `scripts/smoke-portal-seven.sh` | **七条门户路径** × N/T/S（+ 可选 P=3001），对齐门户探测与一键体检差异 |
| `docs/ops/STABLE_BASELINE.md` | 稳定 tag / 提交说明 |

---

## 12. 给「下一个你」的一句话

**先读 §0**。当前主线：**Auth hscale + policy 4–7 + nofile**；**Auth @ 2000 未过 90%，勿上 3000**。压测改在 **§3e 麒麟 VM（8c/64G）** 上跑 `run-auth-gate-2000.sh`，贴回 **`SAG K6 RESULT PASTE BLOCK`**。Edge：**§2c** → **§3.0 compose**；数据面回归 **§3e.2 `apisix_routed`**。
