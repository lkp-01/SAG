# SAG Cloud — 安全访问网关（Secure Access Gateway）

## Production qualification status

The seven-point hardening implementation is repository-complete: identity and mTLS fail-closed checks, absolute deadline/cancellation, bounded audit/pools/queues/bodies, crash-safe Redis work, stream epochs, readiness/drain, multi-instance auth revocation, and auditable idempotency reconciliation are represented in code, tests, migrations, configuration, and runbooks.

This is not yet a claim that the product is qualified on Linux or in production. Capacity remains **pending full-chain validation**; production HA/failover remains **not run** without the company environment, immutable images, real credentials, external PostgreSQL/Redis, and an independent load/fault setup. Single-host Docker Compose is a topology/development aid, not production HA.

- Capacity evidence: [docs/ops/production-capacity-baseline.md](docs/ops/production-capacity-baseline.md)
- HA topology contract: [docs/ops/production-ha-topology.md](docs/ops/production-ha-topology.md)
- Fault matrix: [docs/ops/production-fault-matrix.md](docs/ops/production-fault-matrix.md)
- Release and rollback order: [docs/ops/production-hardening-rollout.md](docs/ops/production-hardening-rollout.md)
- Operator reconciliation: [docs/ops/idempotency-reconciliation-runbook.md](docs/ops/idempotency-reconciliation-runbook.md)

> **【必读 · 接手 / 新同事入口】** 请按下面顺序阅读文档并完成「第一天操作清单」。  
> 仓库 Git 根目录 = **`REPO_ROOT`**（本目录，含 `.git` 与 `docker-compose.edge.yml`）。

**当前生产规划（2026-05）**

| 角色 | IP | 说明 |
|------|-----|------|
| **Edge** | `172.16.9.107` | Docker 全栈、Zentinel `:10080`、Auth `:8080` |
| **Intra** | `192.168.9.26` | APISIX、mock、**sag-connector** → Edge |
| **压测 Windows** | `172.16.9.108` | 数据面 k6；Auth 高 QPS 易客户端瓶颈 |
| **压测 Linux（推荐）** | 麒麟 VM | **Auth 门禁**应用此机 |

**Git**：`git@192.168.14.10:digital-operation/secure_access_gateway_sag.git` · 分支 **`clean-main`**

---

## 上手：先看哪些文档？（阅读顺序）

| 顺序 | 文档 | 用时 | 你会得到什么 |
|------|------|------|----------------|
| **①** | **[`PROJECT_HANDOFF.md`](PROJECT_HANDOFF.md)** | 10 min | **【重点】** 项目结论：数据面 OK、Auth 瓶颈、hscale 暂不启用、待办清单 |
| **②** | **[`SERVER_OPS_QUICKREF.md`](SERVER_OPS_QUICKREF.md)** | 15 min | **【重点】** Edge/Intra/Windows/Linux **生产 release 编译+启动**、冒烟、压测命令（复制即用） |
| **③** | **[`DUAL_HOST_OPERATIONS.md`](DUAL_HOST_OPERATIONS.md) §0** | 15 min | 会话接续、压测口径、k6 术语、§3.0 栈快照 |
| **④** | [`docs/ops/performance-test-plan.md`](docs/ops/performance-test-plan.md) | 按需 | full-chain 生产压测门禁与证据格式 |
| **⑤** | [`DUAL_HOST_OPERATIONS.md`](DUAL_HOST_OPERATIONS.md) 全文 | 查阅 | 迁移 Edge、隧道故障 §1c–1d、§10 诊断、§3e Linux 压测 |
| ⑥ | 下文 **§1 起** 本 README | 开发时 | Rust 结构、环境变量表、本地 Docker、前端启动 |

**不必一次读完**：日常运维以 **② `SERVER_OPS_QUICKREF.md`** 为主；排障查 **③④⑤**。

---

## 上手：第一天操作清单

### A. 任意机器 — 拉代码

```bash
git clone git@192.168.14.10:digital-operation/secure_access_gateway_sag.git
cd secure_access_gateway_sag    # 即 REPO_ROOT；本机若目录名不同则 cd 到含 compose 的目录
git checkout clean-main
git pull origin clean-main --ff-only
git submodule update --init --depth 1 proxy/core
```

### B. Edge（`172.16.9.107`）— 【重点】生产模式

在 Edge 上执行（完整命令见 [`SERVER_OPS_QUICKREF.md` §1](SERVER_OPS_QUICKREF.md)）：

1. `test -f .env || cp edge-host.env.example .env`
2. **release 编译** zentinel + workspace（`docker compose run --rm` + `cargo build --release`）
3. **启动**：数据面 hscale + release + 绑核（**推荐暂不启用 Auth hscale**）
4. 自检：`EDGE_IP=172.16.9.107 bash scripts/ops/verify-hscale-edge.sh`
5. 若 `:8080` login 响应头含 **`Server: nginx`** → 执行 **`bash scripts/ops/rollback-auth-hscale-edge.sh`**

### C. Intra（`192.168.9.26`）

1. 配置 **`.env.intra`**，让 `SAG_TUNNEL_ENDPOINT` 指向 **`172.16.9.107`**，并配置 Connector 身份与 mTLS 材料
2. `cargo build -p sag-connector --release` + `docker compose ... release.intra.yml up -d --build`
3. **`force-recreate sag-connector`**（改 IP 后必做）

The Connector requires the Edge tunnel endpoint and mTLS material, but no database DSN. Central persistence is owned by Edge services.

### D. 冒烟（Windows 或 Edge 本机）

```powershell
# Windows（sag-cloud 目录）
.\scripts\smoke-remote-windows.ps1 -EdgeHost 172.16.9.107 -IntraHost 192.168.9.26 -Rounds 1
```

期望：**S1** 直连 APISIX 正常；**N1 / T1** 在 Intra connector 指向当前 Edge 后为 **2xx**。

### E. Auth 攻关（【重点】未过门禁前）

- **不要**用 Windows 结果作为 Auth 最终结论（易临时端口耗尽）
- 在 **Linux 压测机** 安装 k6 → 运行 **`./scripts/ops/run-auth-gate-2000.sh`**
- 门禁：**login / verify / chain 均 >90% @2000** 再测 3000

### F. 容量验证（尚待 full-chain gate）

历史 3000–7000 数据只属于 routed/transport 实验，不能作为生产容量。候选环境应在独立 Linux 发压机按 [`performance-test-plan.md`](docs/ops/performance-test-plan.md) 运行三次稳态测试和一次 soak；任何 5xx、错误 body、dropped iteration 或 Auth/Policy/幂等/Redis/APISIX/workload/审计证据缺失都会令 gate 失败。

---

## 【重点】当前结论（给汇报用）

| 维度 | 状态 |
|------|------|
| **生产容量** | **待 full-chain gate 验证**；历史 `apisix_routed` 结果已撤销容量资格 |
| **Auth** | **瓶颈**；单实例 @2000 login **~79%**；**Auth hscale 暂不启用** |
| **全链路 @3000** | login **~0.5%**，勿作容量承诺 |

---

## 文档与脚本索引（速查）

| 类型 | 路径 |
|------|------|
| 交接总结 | [`PROJECT_HANDOFF.md`](PROJECT_HANDOFF.md) |
| 各机命令 | [`SERVER_OPS_QUICKREF.md`](SERVER_OPS_QUICKREF.md) |
| 双机运维手册 | [`DUAL_HOST_OPERATIONS.md`](DUAL_HOST_OPERATIONS.md) |
| 生产压测计划 | [`docs/ops/performance-test-plan.md`](docs/ops/performance-test-plan.md) |
| 当前生产容量资格 | [`docs/ops/production-capacity-baseline.md`](docs/ops/production-capacity-baseline.md)（当前 `NOT ESTABLISHED`） |
| 历史 routed 实验（非容量基线） | [`docs/ops/dataplane-load-3000-7000-report.md`](docs/ops/dataplane-load-3000-7000-report.md) |
| Auth 回滚单实例 | [`scripts/ops/rollback-auth-hscale-edge.sh`](scripts/ops/rollback-auth-hscale-edge.sh) |
| Edge 冒烟 | [`scripts/ops/verify-hscale-edge.sh`](scripts/ops/verify-hscale-edge.sh) |
| Linux Auth @2000 | [`scripts/ops/run-auth-gate-2000.sh`](scripts/ops/run-auth-gate-2000.sh) |
| 稳定基线说明 | [`docs/ops/STABLE_BASELINE.md`](docs/ops/STABLE_BASELINE.md) |

---

## 以下为开发手册（Rust 新手版 · 本地 Docker / 架构细节）

本项目是一个企业级零信任安全网关，当前已经有可运行原型：

- `stealth-tunnel-agent`（自定义隧道 Agent）
- `sag-connector`（内网连接器原型）
- `control-plane-admin`（控制平面管理 API 原型）
- `sag-auth`（认证服务原型）
- `sag-policy`（策略引擎 PDP 原型）

---

## 1. 你当前环境还需要准备什么

你当前是 Windows + PowerShell，已经可以成功执行 `cargo check`，说明 Rust 工具链基本可用。  
下一步建议补齐以下环境：

- **必须**：`Git`（用于后续拉 Zentinel 子模块）
- **必须**：`Rust stable`（建议 1.75+）
- **建议**：`protoc`（Protocol Buffers 编译器；你当前大概率已可用）
- **强烈建议**：`WSL2 + Ubuntu`
  - 目前 `stealth-tunnel-agent` 依赖 `zentinel-agent-sdk` 的 Unix Socket 传输
  - 在纯 Windows 原生环境下该 Agent 目前仅能编译，不能完整运行
- **后续阶段需要**：Docker Desktop（用于 Redis/RabbitMQ/ClickHouse 等依赖）

### 快速检查命令（PowerShell）

```powershell
rustc --version
cargo --version
git --version
protoc --version
```

---

## 0.1 开发执行纪律（防遗漏）

为避免长会话过程中 todo 遗忘，根目录新增了临时跟踪文件：

- `TEMP_TODOS_TRACKER.md`

执行要求：

1. 一次只允许一个 todo 处于 `IN_PROGRESS`。
2. 每个 todo 完成后必须立即：
   - 运行受影响模块检查（`cargo check` / 前端 lint）；
   - `git add` + `git commit` + `git push`；
   - 更新 `README.md` 与 `Context_Handoff.md` 的最近进展。
3. Linux 验证环境按固定流程同步：
   - `git pull`
   - `docker compose up -d --build --force-recreate <受影响服务>`

---

## 2. 当前代码结构（已落地）

- `shared/storage`（SQLite 存储 trait 与适配器）
- `proxy/agents/stealth-tunnel-agent`
- `proxy/connectors/sag-connector`
- `services/control-plane-admin`
- `services/sag-auth`
- `services/sag-policy`
- `shared/proto/tunnel.proto`
- `proxy/zentinel-proxy/config/*.kdl`

### 2.1 架构分层与 11 模块索引（无额外代码）

与路线图对齐的**目录索引**（占位 + 说明，不含新 crate）：

- `architecture/README.md`：分层与仓库路径总览
- `architecture/MODULE_MAP.md`：**11 模块 × 分层 × 路径**对照表
- `architecture/layers/`：各分层说明
- `architecture/modules/01-` … `11-`：各业务模块说明
- `infra/`：Public Edge、可观测、APISIX、Mesh、身份集成、GitOps 等占位
- `services/planned/`：模块 7/8（终端安全、审计风控）规划占位

### 2.2 APISIX 浏览器控制台（Dashboard / UI）

APISIX 自带**嵌入在 Admin 端口上的 Web UI**（需 `deployment.admin.enable_admin_ui: true`，quickstart 一般默认开启）。

- **地址**：在浏览器打开 **http://127.0.0.1:9180/ui/**（注意路径末尾的 **`/ui/`**）。
- **密钥**：页面若提示输入 **Admin API Key**，填写你在 APISIX **`config.yaml`** 里 `deployment.admin.admin_key` 下的 **`key`** 字段（与命令行 `curl -H "X-API-KEY: ..."` 使用**同一个**值）。
- **403 Forbidden**：多为 **`allow_admin` IP 白名单**未包含 Docker 视角下的客户端地址（日志里常见 `client: 172.18.0.1`）。在 `allow_admin` 中增加例如 `172.16.0.0/12` 或 `172.18.0.0/16` 后重启容器。
- **401 wrong apikey**：说明请求已到 Admin API，但 **`X-API-KEY` 与当前 `config.yaml` 里配置的 `key` 不一致**（改 key 后需重启 APISIX，且 SAG 的 `SAG_APISIX_ADMIN_API_KEY` 要与之一致）。

另：官方还提供**独立容器**镜像 `apache/apisix-dashboard`（常见映射 **9000** 端口），需单独 `docker compose` / `docker run`；本地开发通常 **仅用 9180 的 `/ui/` 即可**，不必重复部署 Dashboard 容器。

### 2.3 SAG 各服务环境变量速查（SQLite / APISIX / Mesh）

下表便于复制到 PowerShell（`$env:NAME="value"`）或 RustRover Run Configuration。未列出的变量多为可选或使用代码内默认值。

**约定（本仓库标准数据面）**：**内网 L7 网关固定为 APISIX**——`sag-connector` **必须**配置 `SAG_APISIX_BASE_URL`（数据面 `9080`）；`control-plane-admin` **应**配置 `SAG_APISIX_ADMIN_*` 以便向 APISIX 下发 route/upstream（与 `intranet-upstreams` 对齐）。

**冒烟脚本与 APISIX**：`smoke-dataplane.ps1` / `smoke-dataplane-wsl.sh` 中 **`[S1]`** 直连 APISIX 数据面，验证 **Route→上游**（不经隧道）；**`[N1]` / `[T1]`** 走完整数据面时，在 **`sag-connector` 已配置 `SAG_APISIX_BASE_URL`** 且 APISIX 已匹配路径的前提下，流量为 **Connector → APISIX → 上游**。

| 服务 | 环境变量 | 作用 | 示例 / 默认 |
|------|-----------|------|-------------|
| **control-plane-admin** | `SAG_STORAGE_DB_PATH` | SQLite 路径；**不设时与 sag-policy 共用同一默认相对路径** `data/sag-storage/sag.db`（相对进程 `cwd`，定义于 `shared/storage/src/paths.rs`） | `data/sag-storage/sag.db` |
| **control-plane-admin** | `SAG_BOOTSTRAP_DEMO_TUNNEL_ROUTE` | `1`/`true` 时：若 **`tunnel_routes` 表为空** 则插入演示行（`app-001` / `connector-local-001:stream`），等同一次 HTTP `POST /api/v1/agent/routes`；表非空则不改 | （不设则关闭） |
| **control-plane-admin** | `SAG_APISIX_ADMIN_BASE_URL` | APISIX Admin API 根 URL（**标准部署应配置**；不设置则不下发 APISIX） | `http://127.0.0.1:9180` |
| **control-plane-admin** | `SAG_APISIX_ADMIN_API_KEY` | 请求头 `X-API-KEY`；**必须与 APISIX `config.yaml` 中 `admin_key.key` 一致** | （与 APISIX 配置相同） |
| **sag-policy** | `SAG_STORAGE_DB_PATH` | 与 **control-plane-admin 共用** 同一 SQLite 文件（policies 表）；代码内默认与 admin **同一相对路径规则** | `data/sag-storage/sag.db`（同左） |
| **sag-auth** | `SAG_JWT_SECRET` | JWT 签名密钥 | 如 `dev-jwt-secret` |
| **sag-auth** | `SAG_JWT_EXPIRES_SEC` | SAG JWT 过期秒数 | 默认 `3600` |
| **sag-auth** | `SAG_BOOTSTRAP_ADMIN_PASSWORD` | 初始 `admin` 用户密码 | 如 `Admin@123` |
| **sag-auth** | `SAG_ALLOW_PASSWORD_LOGIN` | 是否允许用户名密码登录 | 生产可 `false` 仅 SSO |
| **sag-auth** | `SAG_SESSION_REDIS_URL` | 会话与 OAuth state 存 Redis（多实例） | 未设则用内存 |
| **sag-auth** | `SAG_FOURA_FIRST_URI` / `SAG_FOURA_SECOND_URI` / `SAG_FOURA_THIRD_URI` / `SAG_FOURA_CLIENT_ID` / `SAG_FOURA_CLIENT_SECRET` | 中交4A OAuth2 授权码模式；设齐则启用 `GET /api/v1/auth/sso/*` | 见 [docs/identity-4a.md](docs/identity-4a.md) |
| **sag-auth** | `SAG_SSO_PORTAL_REDIRECT_URL` / `SAG_PUBLIC_HOST` | 4A/OIDC 回调后的门户跳转地址（优先 `SAG_SSO_PORTAL_REDIRECT_URL`，其次 `SAG_PUBLIC_HOST`，避免回退 `127.0.0.1`） | 例如 `http://192.168.9.26:3001/app` |
| **sag-auth** | `SAG_OIDC_ISSUER` / `SAG_OIDC_CLIENT_ID` / `SAG_OIDC_CLIENT_SECRET` / `SAG_OIDC_TOKEN_URI` / `SAG_OIDC_USERINFO_URI` | 配置化 OIDC 授权码流程（支持 groups 解析） | 见 [docs/identity-4a.md](docs/identity-4a.md) |
| **sag-auth** | `SAG_OIDC_AUTHORIZE_URI` / `SAG_OIDC_SCOPES` | OIDC 可选覆盖（默认由 issuer 推导 `/authorize`，scope 默认 `openid profile email groups`） | 可选 |
| **sag-connector** | `SAG_TUNNEL_ENDPOINT` | 连接 `stealth-tunnel-agent` 的 gRPC 地址 | `https://127.0.0.1:50051` |
| **sag-connector** | `SAG_CONNECTOR_ID` | 连接器 ID | `connector-local-001` |
| **sag-connector** | `SAG_APP_ID` | 应用 ID（与路由、`X-Sag-App-Id` 一致） | `app-001` |
| **sag-connector** | `SAG_EXTERNAL_HOST` | 对外域名（路由 host） | `app.internal.com` |
| **sag-connector** | `SAG_CONNECTOR_ENDPOINT` | 注册到 Agent 的 endpoint 名 | 默认 `{CONNECTOR_ID}:stream` |
| **sag-connector** | `SAG_APISIX_BASE_URL` | **必配（标准数据面）**：把隧道请求 **HTTP 代理到 APISIX 数据面**；仅清空用于本地调试 echo | `http://127.0.0.1:9080` |
| **sag-connector** | `SAG_MESH_MODE` | `noop` / `ambient`（Mesh TLS 预留，当前均为 noop 行为） | `noop` |
| **sag-connector** | `SAG_CONNECTOR_MAX_INFLIGHT` | 隧道内 **同时进行** 的 APISIX 转发上限（dispatcher 并发） | 二进制未设 env 时 **2048**；`docker-compose.intra.yml` 默认 **4096** |
| **sag-connector** | `SAG_CONNECTOR_ACCEPT_QUEUE` | 有界接收队列容量；满则 **503**（`connector_forward_reject_total`） | 默认 `max(512, 2×max_inflight)` |
| **sag-connector** | `SAG_CONNECTOR_HTTP_TIMEOUT_MS` | `reqwest` 单次转发总超时（宜 **短于** agent `forward_timeout`、bridge forward） | `55000` |
| **sag-connector** | `SAG_CONNECTOR_GRPC_CHANNEL_TIMEOUT_MS` | tonic `Endpoint::timeout`（与隧道长连接语义对齐，宜 ≥ 心跳周期） | `120000` |
| **sag-connector** | `SAG_CONNECTOR_MAX_RESPONSE_BODY_BYTES` | 流式响应体上限，`0` 表示不限制；超限立即停止读取并返回 **502** | `4194304` |
| **sag-connector**（gRPC mTLS） | `SAG_GRPC_MTLS_ENABLED` 等 | 与 Agent 间证书路径，见连接器 README/源码 | 默认 mTLS 开启 |
| **stealth-tunnel-agent** | `SAG_CONTROL_PLANE_SYNC_ENDPOINT` | 从控制面拉取路由；**支持逗号分隔多个 URL**，按顺序尝试直到成功；若你只配置了非本机地址且未包含 `127.0.0.1`，进程会**自动在前面追加** `http://127.0.0.1:8090/api/v1/agent/routes`（便于 WSL→Windows 上的 admin） | `http://127.0.0.1:8090/api/v1/agent/routes` |
| **stealth-tunnel-agent** | `SAG_CONTROL_PLANE_SYNC_NO_LOCALHOST_FALLBACK` | `true` 时**禁用**上述自动追加 localhost（仅当你确定不要访问本机 8090） | （不设） |
| **stealth-tunnel-agent** | `SAG_FORWARD_TIMEOUT_MS` | 等待 connector 返回 forward 的上限（应 **>** `SAG_CONNECTOR_HTTP_TIMEOUT_MS`，**<** bridge `SAG_GRPC_RPC_TIMEOUT_MS`） | `58000`（edge compose） |
| **stealth-tunnel-agent** | `SAG_MAX_PENDING_WAITERS` | 所有 Connector session 共享的 pending forward 上限 | `8192`（edge compose） |
| **stealth-tunnel-agent** | `SAG_TUNNEL_HEALTHY_WINDOW_SEC` | generation-bound Connector 心跳租约；过期后主动摘除 session 并唤醒其 pending | `10` |
| **stealth-tunnel-agent** | `SAG_CONNECTOR_CERT_BINDINGS` | `endpoint=证书SHA256`，逗号分隔；同 endpoint 可重复配置多个副本证书 | mTLS 开启时必填 |

> `tunnel_routes.require_healthy_tunnel` 为兼容旧控制面字段而保留；Agent 数据面始终强制执行 generation-bound 心跳租约，不能再通过该字段绕过健康检查。
| **stealth-tunnel-agent** | `SAG_AGENT_DEBUG_ADMIN` | `1`/`true` 时在本机监听 **`SAG_AGENT_DEBUG_LISTEN`（默认 `127.0.0.1:19104`）**，提供 **`POST /debug/clear-ephemeral-caches`**：清空策略 Moka 缓存、负缓存、`policy_eval` 合并 map（**不**断开 connector 隧道） | （默认关闭） |
| **stealth-tunnel-agent** | `SAG_AGENT_DEBUG_LISTEN` | 上项开启时的 debug HTTP 绑定地址 | `127.0.0.1:19104` |
| **http-tunnel-bridge** | `SAG_BRIDGE_REDIS_URL` | 设后：`sync_inflight ≥ soft` **或** 隧道并发 **try 失败** 时 **202** 入队 + worker；空则全程同步 forward | edge 默认 `redis://redis:6379/2` |
| **http-tunnel-bridge** | `SAG_BRIDGE_SOFT_INFLIGHT` / `SAG_BRIDGE_HARD_INFLIGHT` | 软阈值走 202；硬阈值 **429** | edge 默认 **48** / 2048 |
| **http-tunnel-bridge** | `SAG_BRIDGE_MAX_TUNNEL_INFLIGHT` | 并发 unary `Forward` 上限；`0` 关闭；满载且无 Redis 时同步路径 **503** | edge 默认 **1024** |
| **http-tunnel-bridge** | `SAG_BRIDGE_FORWARD_TIMEOUT_MS` / `SAG_GRPC_RPC_TIMEOUT_MS` | unary forward 与 tonic Channel 期限；**RPC 期限须 ≥ forward** | 60000 / 120000 |
| **压测 k6** | `run-load-dataplane.ps1 -PollDataplane202` | 202 后轮询 `GET /__sag/queue/.../status`（与真实客户端一致），便于调低 `SOFT_INFLIGHT` | 可选 |
| **压测 k6** | `SAG_DP_POLL_MAX_MS` / `SAG_DP_POLL_INTERVAL_MS` | 轮询总时长与间隔（对齐 `SAG_BRIDGE_POLL_MIN_INTERVAL_MS`） | 120000 / 100 |
| **public-edge** | `PUBLIC_EDGE_LISTEN_ADDR` | 对外监听地址（TLS 终止） | `0.0.0.0:10443` |
| **public-edge** | `PUBLIC_EDGE_CERT_FILE` / `PUBLIC_EDGE_KEY_FILE` | TLS 证书与私钥 | （PoC 默认回退到仓库测试证书） |
| **public-edge** | `PUBLIC_EDGE_UPSTREAM_BASE_URL` | 下游（Zentinel）基础 URL | `https://127.0.0.1:10080` |
| **public-edge** | `PUBLIC_EDGE_UPSTREAM_TLS_INSECURE` | 源站/下游 TLS 校验开关 | `true`（PoC 可跳过） |
| **public-edge** | `PUBLIC_EDGE_TRUST_X_FORWARDED_FOR` | 是否信任 `X-Forwarded-For` 归因 | `false` |
| **public-edge** | `PUBLIC_EDGE_ENABLE_RATE_LIMIT` / `PUBLIC_EDGE_RATE_LIMIT_RPS` / `PUBLIC_EDGE_RATE_LIMIT_WINDOW_SECS` | IP 固定窗口限流参数 | 默认开启（50 rps / 1s） |
| **public-edge** | `PUBLIC_EDGE_MAX_BODY_BYTES` | 请求体大小上限 | 默认 `1048576` |
| **public-edge** | `PUBLIC_EDGE_ENABLE_BLOCKING` / `PUBLIC_EDGE_BLOCK_PATH_PREFIXES` / `PUBLIC_EDGE_BLOCK_METHODS` | 路径前缀/方法阻断规则 | 默认启用（无规则时不触发） |

**数据面超时阶梯（由短到长，推荐）**：`SAG_CONNECTOR_HTTP_TIMEOUT_MS`（如 55s） < `SAG_FORWARD_TIMEOUT_MS`（如 58s） < `SAG_BRIDGE_FORWARD_TIMEOUT_MS`（如 60s） ≤ `SAG_GRPC_RPC_TIMEOUT_MS`（如 120s）；入口 **Zentinel / Nginx** `proxy_read_timeout` 与 k6 **`-RequestTimeout`** 建议 **≥ 90s** 且不长于最外层（`run-load-dataplane.ps1` 默认 **90s**）。细表与自检见 [docs/ops/timeout-deadline-runbook.md](docs/ops/timeout-deadline-runbook.md)。调低 **`SAG_BRIDGE_SOFT_INFLIGHT`** 前请启用 **`-PollDataplane202`**（或前端等价轮询），否则压测会把 202 当终态。

写请求必须提供稳定的 **`Idempotency-Key`**；`x-request-id` 仅用于追踪。deadline、取消、结果重放、多 Agent 隧道与升级顺序见 [请求 deadline、取消与幂等运行手册](docs/ops/request-deadline-cancellation.md)。

**sag-connector 瓶颈判别**：Prometheus 看 **`connector_forward_accept_wait_seconds`**（accept 队列）、**`connector_forward_upstream_seconds`**（APISIX）、**`connector_forward_out_send_seconds`**（回写隧道）；若 **out_send** 主导尾延迟，再评估 HTTP 与 `out_tx.send` 解耦。

**http-tunnel-bridge 与 202 是否出现**：`curl :9000/metrics | grep -E 'bridge_sync_inflight|bridge_soft_gate_entered_total|bridge_queue_202_total|bridge_tunnel_shed_to_queue_total|bridge_soft_fallback_total'`。**gauge `bridge_sync_inflight`** 为当前同步转发中的请求数；若其峰值 **长期小于 `SAG_BRIDGE_SOFT_INFLIGHT`** 且隧道未打满，可能仍无 soft 202；**隧道满载** 时见 **`bridge_tunnel_try_saturated_total`** / **`bridge_tunnel_shed_to_queue_total`**。**`bridge_soft_fallback_total{reason="redis_enqueue"}`** 大于 0 表示 Redis 入队失败回退同步，应先修 Redis。**`bridge_soft_gate_entered_total` ≫ `bridge_queue_202_total`** 时多为 body 超限或队列满（429）。

**说明**：`control-plane-admin` 与 `sag-policy` 在代码里通过 **`shared_storage::resolve_storage_db_path()`** 解析路径：**都不设 `SAG_STORAGE_DB_PATH` 时默认同为 `data/sag-storage/sag.db`**。请务必在所有终端里 **`cd` 到同一个 `sag-cloud` 目录再 `cargo run`**（Windows 与 WSL 在「同盘挂载」下也会落到同一物理文件）。只有需要改目录时才显式设置 `SAG_STORAGE_DB_PATH`。

**4A / SSO**：对接客户统一身份前，请按 [docs/identity-4a.md](docs/identity-4a.md) 索取协议与端点；配置回调 `…/api/v1/auth/sso/callback` 与降级策略见该文档第 7、8 节。

### 2.4 内网测试 API（Mock Workload，可选）

已部署 APISIX 后，可用仓库内 **极简上游（Mock Workload）** 跑通 `APISIX → Workload`，再进一步验证 `Connector → APISIX → Workload`：

- 目录：[infra/test-workload](infra/test-workload/README.md)（`GET /api/whoami`、`/api/echo`、`POST /api/body`）
- 默认 **宿主机端口 18080**；与 `control-plane-admin` 的 **intranet upstream**、示例 `app_id=test-app-mock` 的配法见该 README。

#### 方式 A：Docker（推荐，固定网络与端口）

- `cd infra/test-workload && docker compose up -d`
- 快速探测：

```powershell
curl.exe -sS http://127.0.0.1:18080/health
# 或分层冒烟中的上游探测: .\scripts\smoke-dataplane.ps1（含 [S2] mock /health）
```

#### 方式 B：直接用 Python 在 Windows 起（你当前已跑通的方式）

```powershell
cd d:\lxz\compile\Rust_project\Secure_Access_Gateway_SAG\sag-cloud\infra\test-workload
python .\mock_http_server.py
```

#### 用 APISIX UI 连接到 Mock（最省事）

当 APISIX 运行在 Docker 容器时，Upstream 的 host 应使用 **`host.docker.internal`**（而不是 `127.0.0.1`，后者在容器内指向容器自身）。

1. 打开 APISIX UI：`http://127.0.0.1:9180/ui/`
2. **Upstreams → Add Upstream**
   - Scheme：`http`
   - Nodes：`host.docker.internal:18080`（weight=1）
3. **Routes → Add Route**
   - Hosts：`mock.local`（示例；也可不填 host 仅用 uri 匹配）
   - URI：`/api/*`
   - Upstream：选择上一步创建的 upstream
4. 验证（从宿主机访问 APISIX 数据面 9080）：

```powershell
curl.exe -i "http://127.0.0.1:9080/api/whoami" -H "Host: mock.local"
```

---

## 3. 快速启动（Docker 主路径）

当前文档主流程已切换到 Docker Compose。手工逐服务启动仅保留为调试参考。

推荐直接用一键脚本（会自动启动核心服务 + 种子路由 + 冒烟 + 审计日志采集后台进程）：

```bash
bash ./scripts/ops/start-dev.sh
```

Windows PowerShell：

```powershell
.\scripts\ops\start-dev.ps1
```

审计采集可通过环境变量覆盖（默认开启）：

- `SAG_AUDIT_INGEST_ENABLE=1|0`
- `SAG_AUDIT_INGEST_USER`（默认 `admin`）
- `SAG_AUDIT_INGEST_PASSWORD`（默认 `Admin@123`）
- `SAG_AUDIT_CONTROL_BASE`（默认 `http://127.0.0.1:8090`）
- `SAG_AUDIT_INGEST_SERVICES`（默认 `zentinel,http-tunnel-bridge,stealth-tunnel-agent,sag-connector,public-edge,apisix`）

```powershell
cd d:\lxz\compile\Rust_project\Secure_Access_Gateway_SAG\sag-cloud
docker compose up -d
```

代码有改动后（建议）：

```powershell
docker compose down
docker compose build control-plane-admin sag-auth sag-policy stealth-tunnel-agent sag-connector http-tunnel-bridge zentinel
docker compose up -d postgres etcd apisix mock-workload control-plane-admin sag-auth sag-policy stealth-tunnel-agent sag-connector http-tunnel-bridge zentinel otel-collector prometheus grafana frontend-admin frontend-portal company-demo-sites
```

快速验证：

```powershell
docker compose ps
.\scripts\smoke-dataplane.ps1
curl http://127.0.0.1:9091/-/ready
```

新增功能入口：
- admin 控制台（Next）：`http://127.0.0.1:3001`（工作流 + 硬件 + 应用/API + 兼容控制面板）
- admin 兼容控制面板：`http://127.0.0.1:3001/control`
- 用户门户：`http://127.0.0.1:5174`（策略预检 + 门户导航）
- Prometheus：`http://127.0.0.1:9091`
- Grafana：`http://127.0.0.1:3000`（`admin/sag-admin`）

### [Legacy] 手工启动（仅调试）

已迁移到文档目录，避免根 README 过长：见 `docs/legacy/manual-startup.md`。

## 4. 控制平面 API 用法（可直接试）

### 新增路由

```powershell
$body = @{
  host = "app.internal.com"
  app_id = "app-001"
  connector_endpoint = "connector-local-001:stream"
  require_healthy_tunnel = $true
} | ConvertTo-Json

Invoke-RestMethod -Method POST `
  -Uri http://127.0.0.1:8090/api/v1/agent/routes `
  -ContentType "application/json" `
  -Body $body
```

### 查询路由

```powershell
Invoke-RestMethod "http://127.0.0.1:8090/api/v1/agent/routes?app_id=app-001"
```

### 删除路由

```powershell
Invoke-RestMethod -Method DELETE `
  -Uri http://127.0.0.1:8090/api/v1/agent/routes/app.internal.com
```

---

## 5. 常见问题排查

- **冒烟 `[N1]`/`[T1]` 502，正文含 `no tunnel route for app_id`；Zentinel WARN `bridge-upstream` 失败率 100%**
  - **原因**：`stealth-tunnel-agent` 内存里 **没有** `x-sag-app-id`（默认 `app-001`）对应的隧道路由。常见情况：**从未向 control-plane-admin 写入路由**，或 **Agent 拉不到** `GET .../8090/api/v1/agent/routes`（同步地址/WAF/防火墙），导致路由表一直为空。
  - **处理**：
    1. 确认 admin 在跑，并在 **Agent 所在环境** 能访问：`Invoke-RestMethod "http://127.0.0.1:8090/api/v1/agent/routes"`（或你真实的 `SAG_CONTROL_PLANE_SYNC_ENDPOINT` 去掉协议路径前的 base 换为同机测试）。
    2. **写入演示路由**（与默认 connector 一致），任选其一：
       - **HTTP**：`.\scripts\seed-demo-tunnel-route.ps1` 或 §4 的 `POST /api/v1/agent/routes`
       - **SQLite**：见 [infra/storage-seed](infra/storage-seed/README.md)（`demo_tunnel_route.sql` 或启动 admin 时设 **`SAG_BOOTSTRAP_DEMO_TUNNEL_ROUTE=true`**，仅在表为空时插入）

       其中 **`connector_endpoint` 必须为 `connector-local-001:stream`**（与未改环境变量时的 `sag-connector` 一致）。
    3. 等待默认 **约 5s**（同步间隔）或重启 agent，再跑 `.\scripts\smoke-dataplane.ps1`。
    4. 若下一步变为 **connector tunnel is unhealthy**：说明路由已有，但 **心跳未在窗口内**——确认 connector 在线；联调阶段也可先把路由里的 **`require_healthy_tunnel`** 设为 `false` 再试。

- **`stealth-tunnel-agent` WARN：`failed to sync routes from control-plane` 且 endpoint 里仍是 `http://<windows-host-ip>:8090/...`**
  - **原因**：把文档里的占位符 **原样**当成了环境变量；这不是合法主机名，同步必然失败。
  - **处理**：优先 **`127.0.0.1:8090`**（Agent 已默认/自动尝试）；不要用 **`/etc/resolv.conf` 的 `nameserver`（常见 `10.255.255.254`）当 Windows 宿主机 IP**——在你当前环境常会 **Connection refused**。可改用 **`ip route` 默认网关**，或使用 **`SAG_CONTROL_PLANE_SYNC_ENDPOINT` 的逗号多地址**（见 §2.3 stealth-tunnel-agent 表）。

- **`smoke-dataplane-wsl.sh` 返回 HTML，且正文带 `Powered by APISIX` / `404 Not Found`（OpenResty）**
  - **原因**：`Zentinel` 的上游指向 **`http-tunnel-bridge` 的 `:9000`**，但当前 **`:9000` 上实际跑的不是 bridge**（常见：Windows 上 **APISIX Dashboard** 或其它服务也占用了 **9000**，而 `scripts/start-zentinel-wsl.sh` 会把 upstream 改到 **Windows 宿主机 IP:9000**，于是误打到 APISIX，出现网关默认 404 HTML）。
  - **快速确认（在 WSL 执行）**：`curl -sS -i http://127.0.0.1:9000/api/test -H "x-sag-app-id: app-001"`  
    - 正常：应返回 **JSON**（或 bridge 的业务错误文本），**不应**是整页 HTML。
  - **处理**（二选一）：
    - **A**：保证 **只有** `http-tunnel-bridge` 监听 `:9000`；把占用 9000 的 APISIX Dashboard / 其它进程改端口或停掉。
    - **B**：让 bridge 换端口（例如设 `SAG_HTTP_LISTEN_ADDR=0.0.0.0:19000`），并同步修改 `proxy/zentinel-proxy/config/dataplane-verify.kdl` 里 `bridge-upstream` 的 `target`，或改 `start-zentinel-wsl.sh` 探测端口逻辑（进阶）。

- **冒烟脚本报 `ok=None` / Zentinel 日志 `status=404`，但已启用 `SAG_APISIX_BASE_URL`**
  - **`ok=None` 的原因**：未设置 APISIX 时，`sag-connector` 会返回 demo JSON（含 `ok:true`）；**启用 APISIX 后会把上游响应原样透传**，mock 的 JSON 通常 **没有** `ok` 字段——这是预期行为。`smoke-dataplane-wsl.sh` / `smoke-dataplane.ps1` 已同时识别 **echo** 与 **mock**（`service=sag-test-workload`）。
  - **`404` 的常见原因**：你在 APISIX UI 里给 Route 配了 **Hosts（例如 `mock.local`）**，而 `connector` 用 `reqwest` 访问 `http://127.0.0.1:9080/...` 时，HTTP **Host** 往往是 `127.0.0.1`，**与 `mock.local` 不匹配** → APISIX 返回 404。
  - **处理**：在 APISIX 再建一条 **不限制 Host**（或 Host 含 `127.0.0.1`）的 Route，匹配 `/api/test` 或 `/api/*` 指向上游；或把 smoke 的路径改成你已匹配的路由（例如 `PATH_REQ=/api/whoami` 且 Route 无 Host 限制）。

- **`cargo` 报错：`via 127.0.0.1` / 连不上 `index.crates.io`**
  - **原因**：系统或终端里设置了 **HTTP/HTTPS 代理** 指向本机（例如 `127.0.0.1:7890`），但本机代理程序（Clash、V2Ray、公司代理等）**没在运行**或端口不对，Cargo 就会失败。
  - **不是要“必须开 VPN”**：二选一即可——**(A) 把本地代理软件打开并确认端口**；**(B) 在本终端临时关闭代理再走直连**。
  - **快速关闭代理（当前 PowerShell 会话）**：

    ```powershell
    Remove-Item Env:HTTP_PROXY, Env:HTTPS_PROXY, Env:ALL_PROXY -ErrorAction SilentlyContinue
    Remove-Item Env:http_proxy, Env:https_proxy, Env:all_proxy -ErrorAction SilentlyContinue
    cargo test --workspace
    ```

  - **或使用仓库脚本（推荐）**：

    ```powershell
    cd d:\lxz\compile\Rust_project\Secure_Access_Gateway_SAG\sag-cloud
    .\scripts\cargo-no-proxy.ps1 test --workspace
    ```

  - 若仍失败，到「设置 → 网络和 Internet → 代理」检查 **使用代理服务器** 是否误开；或检查用户级 `C:\Users\<你>\.cargo\config.toml` 是否配置了 `http.proxy`。

- `protoc failed`：安装 Protocol Buffers 编译器并重开终端
- `port already in use`：换端口或先关闭旧进程
- `connector stream is not ready`：说明连接器还没成功注册到 Agent
- `deadline exceeded`：连接器未及时回包，检查网络或超时配置
- Windows 下 Agent 无法跑：请在 WSL2/Linux 启动 `stealth-tunnel-agent`

---

## 6. 当前进度与下一步

当前已经具备：控制面 API、隧道消息协议、Agent/Connector 双端原型和编译通过基线。  
下一步将继续完善：端到端自动化测试、错误路径覆盖、再推进 Zentinel 主仓集成与前端控制台。

### 6.1 冒烟测试覆盖到哪一层？（南北向数据面）

术语上：**客户端从公网/入口侧进入，经边界与隧道到内网工作负载**，一般称为**南北向（North-South）**流量；本仓库的 `smoke-dataplane*.ps1` / `smoke-dataplane-wsl.sh` 验证的是其中 **数据面主链路**（在本地/Demo 拓扑下）：

`Zentinel(HTTPS) → http-tunnel-bridge → stealth-tunnel-agent（含 PDP/IAM 门控）→ sag-connector → APISIX（数据面）→ 上游/Mock`

**标准约定**下 connector **必须**经 APISIX 再访问内网上游；冒烟脚本中 **`[S1]`** 直连验证 APISIX，`[N1]`/`[T1]` 验证经 connector 的整条链是否可达 mock；**`[M*]`、`[S2]`** 分别隔离管理面与上游 mock。

在**已按文档启动全部相关进程**、且 **APISIX**、**Mock/上游路由**对齐的前提下，脚本 **SUMMARY 全绿** 可视为 **南北向数据面端到端 + 管理面健康探针** 通过。它**不替代**生产中的：Public Edge（CDN/WAF）、完整 IAM/审计、多活与容量等；那些需按部署架构单独验证。

### 6.2 生产环境保留「实时连通性」探测（规划，非本仓库现成能力）

**理论上可行**，且业界常见做法是 **合成监控（Synthetic Monitoring）**：周期性从可信探针发起与真实用户同路径的探测请求，用状态码/延迟/断言 JSON 判断「整条链路是否还活着」。

若将来要在生产沿用类似思路，需在架构上单独考虑（本阶段**不实现**定时轮询脚本，仅作路线图备忘）：

- **专用探测路由与身份**：避免与真实业务路径冲突；使用服务账号/受限 Token，或仅在内网探针到入口的一段上探测。
- **频率与副作用**：控制 QPS，避免刷爆日志、误触限流/风控；告警与「真实故障」要可区分（探针失败 vs 业务错误）。
- **证书与入口**：生产多为正式 TLS 与真实域名，与本地自签、`/api/test` Demo 路径可能不同，需独立配置。
- **落地方式**：可由外部 Blackbox / CronJob / 可观测平台执行，或由运维在 GitOps 中部署探针；与 `smoke-dataplane` 脚本解耦。

---

## 7. Admin Console 前端（当前基线）

当前主用管理端是 **Next.js 版** `frontend-admin-next`（不是旧版 Vite `frontend`）。

### 7.1 启动步骤（Windows / PowerShell）

进入前端目录：

```powershell
cd d:\lxz\compile\Rust_project\Secure_Access_Gateway_SAG\sag-cloud\frontend-admin-next
```

1) 安装依赖（网络不稳定可加 `--prefer-offline`）：

```powershell
$env:npm_config_cache="$PWD\.npm-cache"
npm install --prefer-offline --no-audit --progress=false
```

2) 启动开发服务器：

```powershell
npm run dev -- --hostname 0.0.0.0 --port 3001
```

3) 打开页面：
- 管理端主页：`http://127.0.0.1:3001`
- 工作流健康：`http://127.0.0.1:3001/workflow`
- 兼容控制面板（整合旧 Vite 能力）：`http://127.0.0.1:3001/control`

### 7.2 前端能力说明（Next）

- `workflow`：工作流节点健康 + QPS/错误率/P95（Prometheus 驱动）
- `hardware`：CPU/内存/磁盘/网络硬件指标
- `apps`：应用/API 树图（ECharts）
- `control`：路由/上游/策略/用户/登录会话/健康探测/4A 调试占位（由旧 Vite 控制台并入）

---

## 8. User Portal 前端（Demo）

用户门户（`frontend-portal`）是 Vite + React + TS，包含登录、服务导航、策略预检与网关探测。

### 8.1 启动步骤（Windows / PowerShell）

进入目录：

```powershell
cd d:\lxz\compile\Rust_project\Secure_Access_Gateway_SAG\sag-cloud\frontend-portal
```

1) 安装依赖：

```powershell
$env:npm_config_cache="$PWD\.npm-cache"
npm install --prefer-offline --no-audit --progress=false
```

2) 启动开发服务器：

```powershell
npm run dev -- --host 127.0.0.1 --port 5174
```

3) 打开页面：
- `http://127.0.0.1:5174`

### 8.2 环境变量（Vite）

前端常用环境变量：
- `VITE_AUTH_PROXY_TARGET`（默认 `http://sag-auth:8080`）
- `VITE_POLICY_PROXY_TARGET`（默认 `http://sag-policy:8081`）
- `VITE_CONTROL_PROXY_TARGET`（默认 `http://control-plane-admin:8090`）
- `VITE_ZENTINEL_PROXY_TARGET`（默认 `https://zentinel:10080`）
- `VITE_DEMO_SITES_BASE`（默认 `http://127.0.0.1:28080`）
- `VITE_ADMIN_PLANE_URL`（默认 `http://127.0.0.1:3001`，用于“进入管理端”跳转）

### 8.3 4A 联调（甲方未开放接口时）

已内置轻量 Python 假 4A（授权码模式）：

```bash
docker compose up -d fake-4a sag-auth
```

然后访问：

- `http://127.0.0.1:8080/api/v1/auth/sso/login`

会跳转到 Fake 4A 账号选择页，完成后回调 `sag-auth` 签发 SAG JWT。详细见 `infra/fake-4a/README.md`。

#### 8.3.1 一键直达云枢门户（更贴近甲方真实体验）

默认 Compose 已开启“回调后自动跳转门户”的联调模式：

- `sag-auth` 在 `sso/callback` 成功后 **302 跳转** `frontend-portal` 并携带 `sso_token`
- 门户自动校验 `sso_token` 并建立登录态（地址栏会清理 `sso_token`，避免泄露）

入口仍然是：

- `http://127.0.0.1:8080/api/v1/auth/sso/login`

#### 8.3.2 未认证访客拦截演示

Fake 4A 页面提供 “未认证访客（预期被拦截）” 入口，用于展示缺少身份时网关拒绝：

- 预期：`403 missing user identity`

#### 8.3.3 `invalid or expired state`（回退后点击用户）

这是 `state` 的安全机制（防 CSRF/重放）导致的预期行为：

- `state` 一次性使用，且默认有效期约 10 分钟
- 不要用浏览器回退复用旧的 Fake 4A 页面
- 每次切换用户请重新打开：`/api/v1/auth/sso/login`

---

## 9. 架构路线图（新版分层：Public Edge + Zero Trust + Tunnel + APISIX）

### 9.1 当前共识（用于方案汇报）

- 结论：**Conditional Go（有条件可行）**
- 外网前置新增 `Public Edge`（CDN/WAF/DDoS）
- `Zentinel` 聚焦零信任入口，不与公共边缘层重复造轮子
- 私网接入保持 `Tunnel Agent + Connector`，只做安全传输
- **内网 L7 网关在本仓库标准数据面中固定为 `APISIX`**（与 `sag-connector` 的 `SAG_APISIX_BASE_URL` 对齐）；`Ambient Mesh` 仍按业务可选

### 9.2 分层目标链路

`Client -> PublicEdge -> Zentinel -> http-tunnel-bridge -> stealth-tunnel-agent -> connector -> APISIX -> Mesh(optional) -> Workload`

### 9.3 分层职责矩阵（表格版）

原则：**公共边缘层负责通用网络防护；Zentinel 负责零信任准入；隧道层负责私网可达；数据面最终业务授权仍以 `stealth-tunnel-agent + sag-policy` 为单一真源。**

| 层级 | 主要职责 | 与 IAM / PDP 关系 | 技术栈（当前/推荐） | 可选平替 |
|------|----------|-------------------|----------------------|----------|
| **Public Edge（公共边缘层）** | 静态资源加速、Web 攻击防护、DDoS 缓解 | 仅做通用防护，不做最终业务授权裁决 | Cloudflare / 阿里云 CDN；Cloudflare WAF / 腾讯云 WAF；Cloudflare DDoS / 高防 IP | 小团队可先用云厂商 CDN+WAF 一体化服务 |
| **外网边界层（Zentinel）** | TLS 终止、全局安全防护、粗粒度路由、零信任准入 | 可集成 Keycloak/OPA/SPIRE；业务策略仍以 `sag-policy` 为 PDP | `Zentinel`；OpenTelemetry Collector | 保持自研，不建议以通用 Ingress 直接替代 |
| **私网接入层（Tunnel + Connector）** | 私网不可达场景下的 mTLS 安全接入、连接复用、故障转移；只做传输不做业务鉴权 | 透传身份上下文，门控由 `stealth-tunnel-agent + sag-policy` 完成 | 自研 Tunnel Agent（gRPC+mTLS）；Connector（内网终止并对接 APISIX） | Cloudflare Tunnel；腾讯云专线/阿里云高速通道 |
| **内网 API 产品层（本仓库默认 APISIX）** | 细粒度路由、插件扩展、灰度发布、协议转换（gRPC/HTTP）、多租户 API 管理 | 位于 Agent/PDP 门控之后，默认不实现第二套最终业务授权 | APISIX（标准数据面必配） | 极简场景可讨论 Traefik 等替代，需与 connector 对齐 |
| **东西向治理层（可选）** | 内网服务间 L4 零信任（mTLS）与 L7 治理（限流、重试、熔断） | 与网关策略互补，不替代北南向 PDP 裁决 | Istio Ambient（ztunnel + waypoint）；Cilium（eBPF） | 规模较小时可先不启用 |
| **工作负载层（Workload）** | 承载业务逻辑与数据能力 | 消费上层身份/策略结果，不承担边界授权控制平面 | gRPC 服务（Go/Java）；Kafka/RabbitMQ/NATS；PostgreSQL/MySQL/Redis；TensorFlow Serving/Triton | 按团队栈选型 |

### 9.4 关键边界（避免能力重叠）

1. Public Edge 处理通用互联网攻击与缓存，不承担最终业务授权。
2. Zentinel 聚焦零信任准入与边界策略，不复制云边缘的 DDoS/WAF 体系。
3. Tunnel/Connector 只做安全传输与私网可达，不承担业务 PDP 裁决。
4. APISIX 侧默认不再实现与 `sag-policy` 冲突的第二套业务授权规则。

### 9.5 详细文档

- `APISIX_INTRANET_STRATEGY.md`
- `CONTEXT_HANDOFF.md`

---

## 10. 准生产增强（本轮新增）

### 10.1 存储后端抽象与 PostgreSQL

- `shared_storage` 已支持双后端：
  - `SAG_STORAGE_BACKEND=sqlite`（默认）
  - `SAG_STORAGE_BACKEND=postgres`（配 `SAG_POSTGRES_DSN`）
- `control-plane-admin` 与 `sag-policy` 均已切到统一 `StorageStore`，无需业务代码区分数据库方言。
- PostgreSQL 初始化 SQL 见：`infra/migrations/postgres/001_init.sql`

### 10.2 统一配置模板

- 根目录新增：
  - `.env.example`
  - `.env.dev.example`
  - `.env.stage.example`
- 启动日志会打印关键配置摘要（含存储后端与存储目标）；PostgreSQL DSN 会脱敏显示密码。

### 10.3 Docker 一键部署

- 根目录新增：`docker-compose.yml`（含 postgres/etcd/apisix/mock + 核心服务）
- APISIX 配置：`infra/apisix/config.yaml`
- zentinel 已纳入默认编排：`docker compose up -d` 会直接启动 `sag-zentinel`
- 部署文档：`docs/ops/deployment-compose.md`

### 10.4 可观测性（最小集）

- 新增 Compose 观测组件：
  - `otel-collector`（OTLP 接收，配置：`infra/observability/otel-collector.yaml`）
  - `prometheus`（配置：`infra/observability/prometheus.yml`，宿主机端口 `9091`）
  - `grafana`（宿主机端口 `3000`，默认 `admin/sag-admin`）
- `control-plane-admin` / `sag-auth` / `sag-policy` 已提供 `/metrics`（HTTP 请求量/延迟/状态码）。
- 数据面新增指标接入：
  - `http-tunnel-bridge`：`/metrics`
  - `stealth-tunnel-agent`：独立 metrics listener（默认 `9104`，容器内抓取）
  - `sag-connector`：独立 metrics listener（默认 `9103`，容器内抓取）
- `zentinel`（`proxy/core`）在 compose 配置中启用 `observability.metrics`（`0.0.0.0:9090/metrics`），并已被 Prometheus 同屏抓取。
- adminplane（`frontend`）新增“概览”页，统一展示管理面与 zentinel 的 QPS/错误率/P95；并可跳转 Grafana。

### 10.5 运维脚本

- `scripts/ops/start-dev.ps1`
- `scripts/ops/start-dev.sh`
- `scripts/ops/smoke-all.ps1`
- `scripts/ops/diag-sync-routes.ps1`

### 10.6 前端控制台（React + shadcn/Radix）

- 新目录：`frontend/`
- 已包含完整联调控制台页面：
  - 健康总览（含数据面探测）
  - 路由管理（CRUD）
  - 上游映射（upsert + 最近记录）
  - 策略管理（CRUD）
  - 登录会话（login/verify/logout）
  - 4A 调试占位（firstUri/callback/token 预览）
- 启动：
  - `cd frontend`
  - `npm install`
  - `npm run dev`
- 构建：
  - `npm run build`（产物：`frontend/dist`）
- 说明文档：
  - `frontend/README.md`

### 10.7 运维文档与配置字典

- `docs/ops/deployment-compose.md`
- `docs/ops/runbook.md`
- `docs/ops/config-dictionary.md`

### 10.8 用户门户前端（中文导航版）

- 新目录：`frontend-portal/`
- 功能：
  - 中文登录页（`sag-auth` 登录/校验）
  - 服务图标导航 + 右侧列表检索
  - 点击图标跳转 **演示静态站**（Compose 中的 `company-demo-sites`，宿主机 `http://127.0.0.1:28080`，路径如 `/dev/`、`/finance/`）；与“网关探测”（经 `Zentinel -> 隧道 -> APISIX`）是两条链路
  - 登录后为每个应用调用 `sag-policy` `/api/v1/policy/evaluate` 预检，命中拒绝则灰显卡片并禁用“网关探测”
- `admin/boss/ops` 角色显示“进入管理端”按钮（默认跳转 `http://127.0.0.1:3001`，可通过 `VITE_ADMIN_PLANE_URL` 覆盖）
- **策略在数据面生效的前提**：`stealth-tunnel-agent` 必须配置 `SAG_POLICY_EVALUATE_ENDPOINT`（例如 `http://sag-policy:8081/api/v1/policy/evaluate`）；未配置时隧道转发不做 PDP 校验。更新种子策略后请重新执行 `scripts/seed-company-demo.ps1`（或导入 `infra/storage-seed/company_demo_postgres.sql`）。
- 本地启动：
  - `cd frontend-portal`
  - `npm install`
  - `npm run dev`（默认 `5174`）
- Docker Compose 一键双前端：
  - `docker compose up -d frontend-admin frontend-portal`

### 10.10 两机部署（VPN/内网 DNS）与参数化补充

- 新增：
  - `docker-compose.edge.yml`（外网侧）
  - `docker-compose.intra.yml`（内网侧）
  - `.env.dualhost.example`（两机环境变量模板）
- 地址参数化补充：
  - `frontend-portal` 支持 `VITE_ADMIN_PLANE_URL`（“进入管理端”跳转地址）
  - `scripts/seed-company-demo.ps1` 支持 `SAG_ADMIN_BASE_URL` / `SAG_AUTH_BASE_URL` / `SAG_POLICY_BASE_URL`
  - `scripts/smoke-dataplane.ps1` 支持 `EDGE_BASE_URL` / `INTRA_APISIX_DATA_BASE_URL`

### 10.11 双机可靠性验证结论（已实测）

已完成一次“边测边改”的双机可靠性验证，重点覆盖 TLS/SNI 和故障注入：

- 错误 SNI（`SAG_GRPC_TLS_SERVER_NAME`）会触发证书名不匹配，链路按预期失败（fail-closed）。
- 错误隧道 endpoint（`SAG_TUNNEL_ENDPOINT`）会触发 DNS 失败，链路按预期失败（fail-closed）。
- 停止 edge 侧 `stealth-tunnel-agent` 时，数据面返回 `502 transport error`；恢复 agent+connector 后可恢复。
- 恢复正确参数后，`bridge` 与 north ingress 均恢复为 `200`。

本轮为提升双机启动稳定性，做了两项配置修复：

- `docker-compose.edge.yml` 中 `zentinel` 改为在 `/workspace` 目录启动，并通过 `--manifest-path /workspace/proxy/core/Cargo.toml` 指定工程，降低容器首次启动时的工具链同步阻塞风险。
- `proxy/zentinel-proxy/config/dataplane-compose.kdl` 中 HTTPS 证书路径改为绝对路径：
  - `/workspace/proxy/core/tests/fixtures/tls/server-default.crt`
  - `/workspace/proxy/core/tests/fixtures/tls/server-default.key`

双机调试时还需注意：

- `app_id` 对应路由的 `connector_endpoint` 必须和当前运行中的 `SAG_CONNECTOR_ID` 对齐（例如 `connector-intra-001:stream`），否则会出现 `connector tunnel is unhealthy`。
- Windows 上 `curl.exe`（Schannel）对某些 TLS 场景兼容性较差，优先使用脚本内的回退逻辑或在 WSL/Linux 侧复核 north HTTPS 探测。

### 10.12 zentinel 启动与 TLS 预防性治理（重点）

围绕近期多次出现的 zentinel 启动慢/TLS 握手失败问题，当前结论如下：

- `docker-compose.yml`（主编排）与 `docker-compose.edge.yml`（双机 edge）均已改为：
  - `working_dir: /workspace`
  - `cargo run --manifest-path /workspace/proxy/core/Cargo.toml ...`
- 该调整的目标是避免在 `proxy/core` 目录触发不必要的 toolchain 同步阻塞；**首次冷启动仍可能因为 Rust 依赖编译较慢**，但不再属于异常卡死。
- 这类调整不会引入“配置滞后”：
  - 运行版本由镜像/代码版本决定；
  - 连接参数与路由时效由 compose 环境变量、control-plane 路由配置与 zentinel 配置文件决定；
  - 与是否执行 rustup 同步无直接耦合。

新服务器部署时，建议将以下 TLS 预检作为上线前必做项（可脚本化）：

1. 证书/私钥文件存在且容器挂载路径一致（建议使用绝对路径）。
2. 证书未过期（`notAfter`）。
3. SAN 包含实际访问主机名（例如 `example.com`），并与客户端 SNI 对齐：
   - connector/bridge -> agent：`SAG_GRPC_TLS_SERVER_NAME`
   - frontend -> zentinel：`ZENTINEL_PROXY_TARGET` 中的 host
4. 客户端信任链就绪：
   - Node 前端：`NODE_EXTRA_CA_CERTS`
   - Rust/gRPC 客户端：使用匹配 CA 或系统信任链。
5. 业务探针通过（不仅是进程 up）：
   - `N1`: `http://127.0.0.1:3001/api-zentinel/api/test`
   - `T1`: `http://127.0.0.1:9000/api/test`

推荐最小预检命令（Linux/WSL）：

```bash
openssl x509 -in /path/to/server.crt -noout -dates -subject -issuer -ext subjectAltName
openssl pkey -in /path/to/server.key -pubout | sha256sum
openssl x509 -in /path/to/server.crt -pubkey -noout | sha256sum
```

说明：后两条哈希需一致，表示证书与私钥匹配。

### 10.9 管理 API 权限收敛（重要）

- `control-plane-admin` 的管理接口（路由/上游）现在要求 `Authorization: Bearer <JWT>` 且角色需包含 `admin` 或 `boss`。
- `sag-policy` 的管理接口（策略列表/新增/删除）同样要求 `admin/boss`。
- 这意味着：仅隐藏前端按钮不足以防越权，后端也会统一返回 `403` 拒绝未授权访问。
- `scripts/seed-company-demo.ps1` 已更新为先用 `admin` 登录拿 token，再执行受保护写入。

### 10.13 身份映射评估 API（sag-policy）

- 新增接口：`POST /api/v1/identity/map-roles`
- 输入：
  - `provider_id`
  - `external_groups`（外部身份组列表）
  - `base_roles`（可选基础角色）
- 输出：
  - `effective_roles`
  - `matched_rules`
- 作用：基于 `group_role_mappings` 规则，把外部组映射成本地角色列表，供门户/策略联动使用。

### 10.14 审计采集与查询（MVP）

- 新增共享存储表：`audit_logs`
- `control-plane-admin` 新增审计接口：
  - `POST /api/v1/audit/logs`（采集端写入）
  - `GET /api/v1/audit/logs`（按 `time/user/app` 查询，admin/boss）
- Admin Console 新增审计查询页：
  - `/ops/audit`
- 当前阶段优先以 JSON 审计模型落地；接入 Loki/Promtail 时可复用同一字段结构（`user_id/app_id/path/latency/decision/result/trace_id`）。

### 10.15 统一监控入口（Admin Console）

- 新增页面：`/ops/observability`
- 整合入口：
  - `/ops/workflow`
  - `/ops/apps`
  - Grafana（默认 `http://127.0.0.1:3000`）
  - Prometheus（默认 `http://127.0.0.1:9091`）
- 支持通过前端环境变量覆盖：
  - `NEXT_PUBLIC_GRAFANA_URL`
  - `NEXT_PUBLIC_PROMETHEUS_URL`
