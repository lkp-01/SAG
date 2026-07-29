# Seven-Point Production Hardening Implementation Plan

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** 在不重写现有 Bridge → Agent → Connector 架构的前提下，修复七项生产风险，并用可重复的故障注入和全链路负载测试证明系统在高并发、进程退出、依赖断开及横向扩容时不会越权、失控或静默丢请求。

**Architecture:** 采用分阶段原位加固。第一波先关闭身份伪造与公网管理面暴露，并消除 PostgreSQL 连接风暴；第二波补齐 Redis 队列、硬准入和内存预算；第三波建立真实业务语义的性能门禁；第四波完成 readiness、HA、跨实例认证一致性和不确定写请求的人工对账。数据库迁移保持向前兼容，协议变更采用停止准入、排空、协调升级。

**Tech Stack:** Rust 1.75 workspace、Tokio/Axum/Tonic、PostgreSQL、Redis Streams、APISIX/etcd、Docker Compose、Prometheus、k6、PowerShell/Bash。

---

## 0. 执行约束与完成定义

本计划的设计依据是 [seven-point production hardening design](./2026-07-26-seven-point-production-hardening-design.md)，并继承已有的 deadline/cancellation、Connector session lease、stream epoch 和 idempotency 设计约束。

执行前先处理两个仓库环境问题：

- 当前 Windows 环境缺少 `link.exe`，因此 Rust 测试必须在安装了 MSVC Build Tools 的 PowerShell，或安装了 Rust 的 Linux/WSL 环境执行。
- 当前工作目录中的 `.git` 不能被 Git 识别为仓库。不要初始化一个新仓库覆盖历史；应从正确 clone/worktree 执行本计划。下面的 `git commit` 是建议提交边界，只有恢复正常 Git 元数据后才执行。

七项完成定义：

| 编号 | 上线阻断验收 |
|---|---|
| 1 身份边界 | 伪造的 `x-sag-*`/`x-user-*` 不能影响 Policy 或上游身份；缺 Bearer token 必须为 401/Unauthenticated |
| 2 PG/审计 | 峰值和 PG 故障期间，每进程连接数不超过池上限；审计任务和内存不随请求数无限增长 |
| 3 Redis 队列 | worker 在投递后死亡，任务可被回收；结果与 ACK 不会出现半完成；Redis 故障不触发无界同步回退 |
| 4 准入/内存 | 同步突发不能超过 semaphore；请求体、响应体、队列和 stream buffer 都有硬上限 |
| 5 性能门禁 | 只将期望的业务 2xx + 响应体算成功；Auth、Policy、审计全部在链路内；三次短测和一次 soak 均通过 |
| 6 HA/readiness | 任一单实例或主库故障不错误授权；readiness 在依赖断开后于阈值内变为失败；恢复满足声明的 RTO |
| 7 横扩一致性 | 所有副本 mTLS 配置一致；禁用用户/改角色在撤权 SLO 内全实例生效；不确定写请求可审计地人工收敛 |

建议发布波次：

1. **Wave 0，安全止血：** Task 1–4。完成前禁止把 Bridge、Redis、etcd、APISIX Admin 暴露到非可信网络。
2. **Wave 1，可靠性：** Task 5–9。完成前禁止发布新的吞吐量声明。
3. **Wave 2，容量证据：** Task 10–11。以结果选择生产并发值，而不是先设目标再宣称通过。
4. **Wave 3，HA 与一致性：** Task 12–17。协议变更必须协调升级，不能滚动混跑不兼容版本。

## Task 1: 建立可重复的基线和静态安全门禁

**Files:**

- Modify: `scripts/verify-project.sh`
- Modify: `scripts/verify-project.ps1`
- Create: `scripts/ops/verify-production-invariants.sh`
- Create: `scripts/ops/verify-production-invariants.ps1`
- Create: `docs/ops/production-hardening-verification.md`

**Step 1: 写一个会失败的生产配置检查器**

检查解析后的 Compose，而不是只 grep YAML 文本。脚本至少拒绝：

- Bridge、Redis、etcd 或 APISIX Admin 绑定 `0.0.0.0`；
- 任一 Bridge 副本缺少 `SAG_GRPC_MTLS_ENABLED`、CA、证书或私钥；
- 生产服务缺少 restart policy、healthcheck 或 resource limit；
- Redis 没有持久卷/AOF，或生产配置允许空密码；
- 已知示例密钥出现在 release compose 的最终环境变量中。

Run:

```powershell
pwsh scripts/ops/verify-production-invariants.ps1
```

Expected: 当前配置以非零状态退出，并逐项列出 Bridge 端口、Redis/etcd 端口、hscale Bridge mTLS 等违规项。

**Step 2: 把基础验证接入总验证脚本**

在两个 `verify-project` 脚本中依次执行：格式、workspace 编译/测试、Compose config 解析、production invariants。每一步失败时保留真实退出码。

**Step 3: 固化基线证据**

文档记录：命令、环境、Git SHA、Compose 文件组合、已知失败，不复制旧报告中的“routed 200–599 = success”定义。

Run:

```powershell
cargo fmt --all -- --check
cargo test --workspace --all-targets
docker compose -f docker-compose.edge.yml -f docker-compose.hscale-edge.yml config | Out-Null
docker compose -f docker-compose.intra.yml config | Out-Null
```

Expected: 工具链完整时 Rust 测试运行；当前未修复 Compose 应只在 invariant gate 失败。

**Step 4: Commit**

```bash
git add scripts/verify-project.sh scripts/verify-project.ps1 scripts/ops/verify-production-invariants.sh scripts/ops/verify-production-invariants.ps1 docs/ops/production-hardening-verification.md
git commit -m "test: add production hardening invariant gate"
```

## Task 2: 用测试锁定身份信任边界

**Files:**

- Modify: `proxy/http-tunnel-bridge/src/main.rs`
- Modify: `proxy/agents/stealth-tunnel-agent/src/grpc_server.rs`
- Modify: `services/sag-policy/src/main.rs`

**Step 1: Bridge 先写失败测试**

抽出 `sanitize_untrusted_headers(HeaderMap) -> HeaderMap`，测试大小写不敏感地移除：`x-sag-user-id`、`x-sag-user-roles`、`x-sag-authenticated`、`x-user-id`、`x-user-roles`，同时保留普通业务头。

Run:

```bash
cargo test -p http-tunnel-bridge sanitize_untrusted_headers -- --nocapture
```

Expected: helper 尚未实现，测试失败。

**Step 2: Agent 先写失败测试**

将身份解析抽成纯函数或可注入 verifier 的小组件，至少覆盖：

- 配置了 `SAG_AUTH_VERIFY_ENDPOINT` 且没有 Bearer token，即便携带伪造身份头也返回 Unauthenticated；
- token 校验失败不降级到 header；
- token 校验成功时，调用者身份头被 verified identity 覆盖；
- Policy 输入与转发给 Connector 的身份完全相同。

Run:

```bash
cargo test -p stealth-tunnel-agent identity -- --nocapture
```

Expected: 现有 fallback 行为令至少一个测试失败。

**Step 3: Policy 写防御性测试**

Policy 不再把“来自公网的 header”当成认证事实；要求 Agent 提供的内部认证标记/结构。缺少内部可信上下文时拒绝，而不是匿名放行。

**Step 4: 仅实现最小身份修复**

- Bridge 在构造 gRPC 请求前清除保留头；
- Agent 在认证端点启用时强制 Bearer，验证后重建 canonical identity headers；
- Policy 使用 canonical identity；
- 记录 `auth_missing`、`auth_invalid`、`identity_header_stripped_total` 指标，不记录 token。

Run:

```bash
cargo test -p http-tunnel-bridge
cargo test -p stealth-tunnel-agent
cargo test -p sag-policy
```

Expected: 全部通过，日志中无 Authorization 内容。

**Step 5: Commit**

```bash
git add proxy/http-tunnel-bridge/src/main.rs proxy/agents/stealth-tunnel-agent/src/grpc_server.rs services/sag-policy/src/main.rs
git commit -m "fix: enforce authenticated identity boundary"
```

## Task 3: 收紧网络暴露、密钥和 hscale mTLS 配置

**Files:**

- Modify: `docker-compose.edge.yml`
- Modify: `docker-compose.intra.yml`
- Modify: `docker-compose.hscale-edge.yml`
- Modify: `docker-compose.release.edge.yml`
- Modify: `docker-compose.release.intra.yml`
- Modify: `infra/apisix/config.yaml`
- Modify: `.env.example`
- Modify: `.env.dualhost.example`
- Create: `docker-compose.debug-ports.yml`
- Modify: `docs/ops/deployment-compose.md`

**Step 1: 用 YAML anchors 定义唯一 Bridge 安全环境块**

在 hscale 配置中让每个 Bridge 引用同一个 extension block。所有副本必须继承 Agent endpoint、`SAG_GRPC_MTLS_ENABLED=true`、CA、client cert、client key 和 server name；只允许 endpoint/instance id 覆盖。

**Step 2: 默认不发布内部端口**

- Bridge gRPC/HTTP 内部端口使用 `expose` 或内部 network；
- Redis、etcd、APISIX Admin 不出现在 production host `ports`；
- 本地调试通过 `docker-compose.debug-ports.yml` 绑定 `127.0.0.1`，不能绑定所有网卡。

**Step 3: 删除 release 默认密钥**

release compose 使用 `${VAR:?required}`，APISIX Admin key、Redis 凭据、JWT secret、数据库密码不能为空，且脚本拒绝仓库示例值。文档明确密钥轮换顺序。

**Step 4: 验证两个 Bridge 的最终配置**

Run:

```powershell
$cfg = docker compose -f docker-compose.edge.yml -f docker-compose.hscale-edge.yml config --format json | ConvertFrom-Json
$cfg.services.PSObject.Properties | Where-Object Name -Like '*bridge*' | ForEach-Object { $_.Value.environment }
pwsh scripts/ops/verify-production-invariants.ps1
```

Expected: 每个 Bridge 都启用 mTLS 且证书变量非空；production 配置不发布内部管理端口。

**Step 5: Commit**

```bash
git add docker-compose.edge.yml docker-compose.intra.yml docker-compose.hscale-edge.yml docker-compose.release.edge.yml docker-compose.release.intra.yml docker-compose.debug-ports.yml infra/apisix/config.yaml .env.example .env.dualhost.example docs/ops/deployment-compose.md
git commit -m "fix: make internal services private and unify bridge mtls"
```

## Task 4: 用共享 PostgreSQL 连接池替代逐操作建连

**Files:**

- Modify: `shared/storage/Cargo.toml`
- Modify: `shared/storage/src/store.rs`
- Modify: `shared/storage/src/lib.rs`
- Modify: `shared/storage/src/audit_logs.rs`
- Modify: `shared/storage/src/fault_events.rs`
- Modify: `shared/storage/src/app_metrics.rs`
- Modify: `shared/storage/src/users.rs`
- Modify: `shared/storage/src/routes.rs`
- Modify: `shared/storage/src/policies.rs`
- Modify: `shared/storage/src/apps.rs`
- Modify: `shared/storage/src/api_routes.rs`
- Modify: `shared/storage/src/identity.rs`
- Modify: `shared/storage/src/idempotency.rs`

**Step 1: 写池配置和并发测试**

新增 `PostgresPoolConfig { max_size, acquire_timeout, connect_timeout, query_timeout }`。集成测试并发执行高于 `max_size` 的查询，并从 `pg_stat_activity` 验证该应用连接数不超过上限；池耗尽必须在 acquire timeout 内返回 typed error。

**Step 2: 引入连接池依赖**

Run:

```bash
cargo add deadpool-postgres --package shared_storage
```

锁定 Cargo.lock 中实际解析的、兼容 Rust 1.75 的版本；如果最新版本提高 MSRV，选择仍受支持的兼容版本并在提交说明中记录。

**Step 3: 让 `PostgresStore` 持有 pool**

构造时解析 DSN 并创建一次 pool。所有 storage 方法改为 `pool.get()`，在 query timeout 中执行。禁止任何业务方法调用 `tokio_postgres::connect`。

静态检查：

```bash
rg -n "tokio_postgres::connect" shared/storage/src
```

Expected: 只允许在连接池初始化封装中出现，普通 store 文件为零处。

**Step 4: 加运行指标和总预算检查**

导出 `db_pool_in_use`、`db_pool_available`、`db_pool_wait_seconds`、`db_query_timeout_total`。启动时检查“副本数 × 单副本 max_size + 管理保留连接”不超过文档化的 PG `max_connections` 预算。

**Step 5: 测试数据库中断和恢复**

Run:

```bash
cargo test -p shared_storage --all-targets
```

Expected: 池上限、获取超时、查询超时、断开后重连测试通过；无无限等待。

**Step 6: Commit**

```bash
git add Cargo.lock shared/storage
git commit -m "refactor: pool postgres connections with timeouts"
```

## Task 5: 建立有界、批量且可观测的审计管道

**Files:**

- Create: `shared/storage/src/audit_writer.rs`
- Modify: `shared/storage/src/lib.rs`
- Modify: `shared/storage/src/audit_logs.rs`
- Modify: `shared/storage/src/store.rs`
- Modify: `shared/storage/src/sqlite_store.rs`
- Create: `infra/migrations/postgres/002_audit_hardening.sql`
- Modify: `proxy/http-tunnel-bridge/src/main.rs`
- Modify: `proxy/agents/stealth-tunnel-agent/src/grpc_server.rs`
- Modify: `services/sag-auth/src/main.rs`
- Modify: `services/sag-policy/src/main.rs`
- Modify: `services/control-plane-admin/src/main.rs`

**Step 1: 写有界性测试**

用容量很小的 channel 和可暂停的 fake sink 测试：

- 队列满后 `try_record` 立即返回 `Full`，不会 await 或 spawn；
- 单 worker 按 `batch_size` 或 `flush_interval` 批量写入；
- shutdown 在 drain deadline 内刷新，超时后明确计数丢弃；
- sink 失败时内存占用仍受 channel 容量约束。

Run:

```bash
cargo test -p shared_storage audit_writer -- --nocapture
```

Expected: writer 尚未实现，测试失败。

**Step 2: 实现一个进程一个 writer**

`AuditWriter` 持有 bounded `mpsc`、单一 batch worker 和 `shutdown()`。数据面只允许 `try_record`；不得每请求 `tokio::spawn`。管理面安全关键变更使用同一 PostgreSQL transaction 写业务变更与审计，审计失败则业务变更回滚。

**Step 3: 修复 schema 和 ID**

- 审计、fault event ID 使用 UUID v4，不再使用毫秒时间；
- migration 补齐 `audit_logs`/`fault_events` 的正式 PostgreSQL schema、`ts_ms`、`service`、`user_id`、`app_id`、`trace_id` 常用索引；
- 给出按时间分区或批量删除的 retention SQL；
- 使启动期 schema 与 migration 一致，migration 是生产唯一来源。

**Step 4: 替换所有 per-request spawn**

Run:

```bash
rg -n -U "tokio::spawn\([\s\S]{0,500}(append_audit|insert_audit|audit)" proxy services
```

Expected: 数据面请求路径没有审计专用 spawn。

**Step 5: 导出告警指标**

至少包含 `audit_queue_depth`、`audit_enqueued_total`、`audit_dropped_total{reason}`、`audit_batch_write_total`、`audit_write_failed_total`、`audit_oldest_buffered_seconds`。

**Step 6: 验证**

```bash
cargo test -p shared_storage -p http-tunnel-bridge -p stealth-tunnel-agent -p sag-auth -p sag-policy -p control-plane-admin
```

Expected: ID 唯一性、有界性、事务回滚和 shutdown drain 测试全部通过。

**Step 7: Commit**

```bash
git add shared/storage proxy/http-tunnel-bridge/src/main.rs proxy/agents/stealth-tunnel-agent/src/grpc_server.rs services/sag-auth/src/main.rs services/sag-policy/src/main.rs services/control-plane-admin/src/main.rs infra/migrations/postgres/002_audit_hardening.sql
git commit -m "feat: add bounded batched audit pipeline"
```

## Task 6: 将 Redis Streams 队列改成可恢复的原子状态机

**Files:**

- Modify: `proxy/http-tunnel-bridge/src/queue.rs`
- Modify: `proxy/http-tunnel-bridge/src/main.rs`
- Create: `proxy/http-tunnel-bridge/tests/queue_recovery.rs`

**Step 1: 先写队列故障矩阵测试**

使用独立测试 stream/group，覆盖：

1. 容量检查与创建 job 不可交叉超卖；
2. `XREADGROUP` 后、持久结果前杀 worker，另一 worker 能 `XAUTOCLAIM`；
3. 持久结果成功、ACK 前杀 worker，重试不会再次发起上游 mutation；
4. 失败/DLQ 保存失败时不能 ACK；
5. dedup 查询失败时拒绝或重试，不能当作“已去重”；
6. reclaim idle time 大于最大 forward deadline。

Run:

```bash
cargo test -p http-tunnel-bridge --test queue_recovery -- --ignored --nocapture
```

Expected: 当前 `read new only`、分步写入和 fail-open dedup 令测试失败。

**Step 2: 原子 enqueue**

用版本化 Lua script 在一个 Redis 执行单元内完成：检查精确容量、写 job hash、设置 TTL、`XADD`。不要用会删除未 ACK entry 的 approximate MAXLEN 作为容量控制。

**Step 3: 增加 pending recovery**

worker 启动和周期任务调用 `XAUTOCLAIM`；entry 带 `attempt`、`claimed_at`，超过最大次数先原子写 DLQ 再 ACK。回收延迟配置必须启动校验为 `> max_forward_deadline + jitter_margin`。

**Step 4: 原子完成/失败**

用 Lua 把“保存最终结果 + 更新 job 状态 + XACK + 可选 XDEL”放在一次操作中。mutation 的重复投递必须携带相同 idempotency scope key，由 Agent ledger 阻止二次 dispatch。

**Step 5: 去掉 fail-open 和无界回退**

删除 `unwrap_or(true)`。Redis/dedup 不可判定时：只读请求可按明确配置进入受限同步路径；mutation 和 production 默认返回 503，带 `Retry-After`，同时增加 `queue_dependency_unavailable_total`。

**Step 6: 验证**

```bash
cargo test -p http-tunnel-bridge
cargo test -p http-tunnel-bridge --test queue_recovery -- --ignored --nocapture
```

Expected: kill-point 矩阵通过；Redis PEL 最终清空；每个 job 恰有一个终态。

**Step 7: Commit**

```bash
git add proxy/http-tunnel-bridge
git commit -m "fix: make redis queue atomic and recover pending jobs"
```

## Task 7: 给 Redis 加持久化、鉴权和生产 HA 契约

**Files:**

- Modify: `docker-compose.edge.yml`
- Modify: `docker-compose.release.edge.yml`
- Modify: `.env.example`
- Modify: `.env.dualhost.example`
- Modify: `docs/ops/backpressure-queue-runbook.md`
- Modify: `docs/ops/config-dictionary.md`
- Create: `scripts/ops/test-queue-recovery.sh`
- Create: `scripts/ops/test-queue-recovery.ps1`

**Step 1: 开发 Compose 也保留数据**

给 Redis 显式 volume、AOF `appendonly yes`、健康检查和非空密码；端口仅内部可见。开发单机和生产 HA 要在文档中明确区分，不能把单 Redis Compose 描述为 HA。

**Step 2: 定义生产连接契约**

Bridge 接受带 TLS/鉴权的 Redis/Sentinel/托管 HA endpoint；配置连接、命令、重连超时和最大重试退避。启动日志打印模式和超时，但不打印凭据。

**Step 3: 自动化断点测试**

脚本提交 N 个带唯一 idempotency key 的任务，在 `delivered`、`result persisted`、`before ack` 三个点杀 worker/Redis，恢复后验证：无静默丢失、无第二次 mutation dispatch、PEL 无永久 entry。

Run:

```powershell
pwsh scripts/ops/test-queue-recovery.ps1 -Jobs 100
```

Expected: 100 个 job 全部进入 completed 或可解释的 indeterminate/DLQ；零未知消失。

**Step 4: 写 RPO/RTO 和操作步骤**

runbook 记录 AOF fsync 策略对应的队列 RPO、主从切换 RTO、PEL/DLQ 观察命令和禁止人工 `XACK` 未持久化结果的规则。

**Step 5: Commit**

```bash
git add docker-compose.edge.yml docker-compose.release.edge.yml .env.example .env.dualhost.example docs/ops/backpressure-queue-runbook.md docs/ops/config-dictionary.md scripts/ops/test-queue-recovery.sh scripts/ops/test-queue-recovery.ps1
git commit -m "ops: persist and validate redis overload queue"
```

## Task 8: 用 semaphore 实现严格准入并消除计数竞态

**Files:**

- Modify: `proxy/http-tunnel-bridge/src/main.rs`
- Modify: `proxy/http-tunnel-bridge/src/limits.rs`
- Modify: `proxy/http-tunnel-bridge/src/queue.rs`

**Step 1: 写同步突发测试**

用 barrier 同时释放 `limit × 4` 个请求，fake Agent 挂起已准入请求。断言：

- 读取 body 前必须拿到 hard ingress permit；
- active sync calls 从未高于 `sync_limit`；
- 超量只进入有界 Redis queue 或快速 503；
- 取消/超时/错误路径都归还 permit。

Run:

```bash
cargo test -p http-tunnel-bridge admission -- --nocapture
```

Expected: 当前 load-then-increment 竞态使最大值越界。

**Step 2: 替换原子计数器**

分别建立 hard ingress 和 sync-path `tokio::sync::Semaphore`。使用 owned permit，让 permit 生命周期与请求 future 一致；不要同时维护另一套可漂移的 active counter。

**Step 3: 在 body 前准入**

先检查 header/Content-Length 上限并获取 hard permit，再读取有界 body。排队/拒绝响应给出可观测 reason：`hard_limit`、`sync_limit`、`queue_full`、`queue_unavailable`。

**Step 4: 指标来源改为 permit**

`active = configured - available_permits`。增加 `admission_rejected_total{reason}`、`admission_wait_seconds`，并测试取消后 gauge 回零。

**Step 5: 验证**

```bash
cargo test -p http-tunnel-bridge
```

Expected: 100 次并发 barrier 回归均不超过上限；无 permit leak。

**Step 6: Commit**

```bash
git add proxy/http-tunnel-bridge/src/main.rs proxy/http-tunnel-bridge/src/limits.rs proxy/http-tunnel-bridge/src/queue.rs
git commit -m "fix: enforce hard semaphore admission limits"
```

## Task 9: 给各数据面进程建立可计算的内存预算

**Files:**

- Modify: `proxy/public-edge/src/main.rs`
- Modify: `proxy/http-tunnel-bridge/src/main.rs`
- Modify: `proxy/agents/stealth-tunnel-agent/src/config.rs`
- Modify: `proxy/agents/stealth-tunnel-agent/src/grpc_server.rs`
- Modify: `proxy/connectors/sag-connector/src/main.rs`
- Modify: `.env.example`
- Modify: `docs/ops/config-dictionary.md`

**Step 1: 建立 startup budget 测试**

每个进程用以下保守公式检查配置：

```text
reserved + ingress_concurrency × max_request_body
         + response_concurrency × max_response_body
         + queue_capacity × max_enqueued_bytes
         + stream_capacity × max_frame_bytes
         <= SAG_MEMORY_BUDGET_BYTES × safety_factor
```

测试超预算、0/无限 body、乘法溢出均启动失败；合理配置通过。

**Step 2: 修复 public-edge 的无界路径**

- 进程级复用一个 `reqwest::Client`；
- 连接、首字节、总请求 timeout；
- request/response body 硬上限；
- 默认启用 TLS 证书验证，开发跳过必须是显式危险开关且 production mode 拒绝；
- 能流式转发时不把完整响应收进内存。

**Step 3: 收紧 Bridge/Agent/Connector 缓冲**

降低默认 in-flight、accept queue、stream channel、4 MB response cap 的组合乘积；配置任何一项增大时必须同时满足进程 budget。Connector 对重复响应头使用 append 语义，不能 collapse `Set-Cookie` 等多值头。

**Step 4: 验证资源上限**

Run:

```bash
cargo test -p public-edge -p http-tunnel-bridge -p stealth-tunnel-agent -p sag-connector
```

再用固定并发发送最大尺寸 body，观察 RSS 在 steady state 后不随请求总数增长。

Expected: 超上限返回 413/503；timeout 返回 504；RSS 受预算控制；连接池复用生效。

**Step 5: Commit**

```bash
git add proxy/public-edge/src/main.rs proxy/http-tunnel-bridge/src/main.rs proxy/agents/stealth-tunnel-agent/src/config.rs proxy/agents/stealth-tunnel-agent/src/grpc_server.rs proxy/connectors/sag-connector/src/main.rs .env.example docs/ops/config-dictionary.md
git commit -m "fix: enforce bounded data-plane memory budgets"
```

## Task 10: 重写性能测试的成功语义和证据格式

**Files:**

- Modify: `scripts/ops/load-dataplane-k6.js`
- Modify: `scripts/ops/run-load-dataplane.ps1`
- Modify: `scripts/ops/run-load-regression.ps1`
- Modify: `scripts/validate-workflow-p95.sh`
- Create: `scripts/ops/run-production-gate.sh`
- Create: `scripts/ops/run-production-gate.ps1`
- Modify: `docs/ops/performance-test-plan.md`
- Modify: `docs/ops/dataplane-load-3000-7000-report.md`
- Modify: `README.md`

**Step 1: 先让旧报告失去“稳定容量”资格**

把旧报告标注为 routed/transport experiment，不删除历史数据，但撤销“≤5000 稳定”结论。README 在新 gate 通过前只写“容量待全链路验证”。

**Step 2: 将成功条件改成业务断言**

k6 同时要求：

- `status` 等于场景期望值，正常场景必须为指定 2xx；
- 响应 JSON/body 带本次请求的唯一 correlation value；
- 不是缓存或旧 job 的结果；
- 没有 generator dropped iterations；
- mutation 场景的幂等副作用计数恰为 1。

HTTP 500、任意 200–599、仅 routed 都不能进入 `business_success_rate`。

**Step 3: 分成三种明确场景**

1. `transport`：只测 tunnel reachability；
2. `workload`：测已知 mock workload 的期望 2xx/body；
3. `full_chain`：强制 Auth、Policy、idempotency、audit、Redis queue、APISIX 和 workload 全部参与。

每份 artifact 写出场景、Git SHA、镜像 digest、环境、配置快照、目标 RPS、实际 completed RPS、业务错误分布、p50/p95/p99、资源水位和依赖错误。

**Step 4: 新增 gate 逻辑**

候选点至少三次 10–15 分钟稳态测试，再做一次 2–4 小时 soak。稳定容量取“第一个可重复饱和拐点”的 70%，并满足：业务成功率、p95/p99、零错误授权、审计 drop 预算、PG pool wait、Redis PEL age、进程 RSS 和 load-generator utilization 阈值。

Run:

```powershell
pwsh scripts/ops/run-production-gate.ps1 -Scenario full_chain -TargetRps 500 -Repeats 3 -SoakMinutes 120
```

Expected: 任何 500、错误 body、dropped iteration 或缺失 Auth/Policy/audit 证据都会令命令非零退出。

**Step 5: Commit**

```bash
git add scripts/ops/load-dataplane-k6.js scripts/ops/run-load-dataplane.ps1 scripts/ops/run-load-regression.ps1 scripts/validate-workflow-p95.sh scripts/ops/run-production-gate.sh scripts/ops/run-production-gate.ps1 docs/ops/performance-test-plan.md docs/ops/dataplane-load-3000-7000-report.md README.md
git commit -m "test: gate capacity on full-chain business success"
```

## Task 11: 测量饱和拐点并确定生产限额

**Files:**

- Modify: `scripts/ops/perf-target.env.example`
- Create: `docs/ops/production-capacity-baseline.md`
- Modify: `docs/ops/STABLE_BASELINE.md`
- Modify: `README.md`

**Step 1: 使用阶梯测试找拐点**

从低负载开始，每级只提高 20–25%，直到任一 SLO 连续两级失败或错误率明显上升。load generator 必须在独立 Linux 主机/VM，CPU 和网络均有余量。

**Step 2: 在候选生产点做三次重复和 soak**

保存每次原始 k6 JSON、Prometheus snapshot、container stats、日志 correlation 样本。不要只保存汇总百分比。

**Step 3: 反推配置，而非抬高上限**

以测得稳定容量的 70% 设置 Bridge hard/sync permits、Agent inflight、Connector inflight、PG pool、Redis queue capacity 和 alert threshold。验证各进程内存预算仍通过。

**Step 4: 发布唯一有效基线**

新基线必须注明硬件、网络 RTT、payload 分布、读写比例、Auth/Policy 模式、审计模式、HA 拓扑、Git SHA 和测试日期。README 只引用这份基线。

**Step 5: Commit**

```bash
git add scripts/ops/perf-target.env.example docs/ops/production-capacity-baseline.md docs/ops/STABLE_BASELINE.md README.md
git commit -m "docs: publish reproducible full-chain capacity baseline"
```

## Task 12: 实现真实 liveness、readiness 和有界优雅退出

**Files:**

- Modify: `proxy/http-tunnel-bridge/src/main.rs`
- Modify: `proxy/agents/stealth-tunnel-agent/src/main.rs`
- Modify: `proxy/agents/stealth-tunnel-agent/src/manager.rs`
- Modify: `proxy/connectors/sag-connector/src/main.rs`
- Modify: `services/sag-auth/src/main.rs`
- Modify: `services/sag-policy/src/main.rs`
- Modify: `services/control-plane-admin/src/main.rs`
- Modify: `proxy/public-edge/src/main.rs`
- Modify: `docker-compose.edge.yml`
- Modify: `docker-compose.intra.yml`
- Modify: `docker-compose.release.edge.yml`
- Modify: `docker-compose.release.intra.yml`

**Step 1: 为 dependency state 写测试**

每个服务区分 `/live` 和 `/ready`。用 fake dependency 测试：

- event loop 正常但依赖断开：live=200、ready=503；
- 检查超时：ready 在固定 deadline 内返回 503；
- 恢复并连续成功 N 次后才 ready，避免抖动；
- SIGTERM 后立即 unready，停止新准入，再在 drain deadline 内退出。

**Step 2: 定义每个服务的 readiness 契约**

- Bridge：Agent channel 可用；启用 queue 时 Redis 可执行脚本；
- Agent：路由首次同步成功且最小 Connector session 数满足；manager HTTP sync 必须有 timeout；
- Connector：收到 Agent 的 RegisterAck 且 APISIX 健康；metrics TCP 监听不算 ready；
- Auth/Policy/Admin：PG pool 可获取连接，并完成轻量只读检查；
- Public Edge：至少一个 Bridge endpoint 可用。

**Step 3: 实现 shutdown drain**

停止监听/准入，readiness 失败，等待已有请求、审计 batch 和队列状态在固定 deadline 内收敛；超时则取消并记录未完成数量。mutation 已 dispatch 但未返回必须进入 indeterminate，不得重发。

**Step 4: Compose 只依赖 health**

为所有长期服务添加 restart policy、healthcheck、start period 和资源限制；只在真正需要启动顺序时使用 `depends_on.condition: service_healthy`，但应用仍必须能处理运行期依赖断开。

Run:

```bash
cargo test --workspace --all-targets
docker compose -f docker-compose.edge.yml config >/dev/null
docker compose -f docker-compose.intra.yml config >/dev/null
```

Expected: 依赖断开时 readiness 在约定阈值内失败；恢复后自动 ready；SIGTERM 不接收新流量。

**Step 5: Commit**

```bash
git add proxy services docker-compose.edge.yml docker-compose.intra.yml docker-compose.release.edge.yml docker-compose.release.intra.yml
git commit -m "feat: add dependency-aware readiness and graceful drain"
```

## Task 13: 完成 stream epoch、RegisterAck 和 Connector 归属协议

**Files:**

- Modify: `shared/tunnel-proto/proto/tunnel.proto`
- Modify: `shared/tunnel-proto/src/lib.rs`
- Modify: `proxy/agents/stealth-tunnel-agent/src/grpc_server.rs`
- Modify: `proxy/agents/stealth-tunnel-agent/src/connector_registry.rs`
- Modify: `proxy/connectors/sag-connector/src/main.rs`
- Modify: `proxy/http-tunnel-bridge/src/main.rs`
- Modify: `docs/plans/2026-07-26-stream-epoch-request-state-design.md`
- Modify: `docs/ops/horizontal-scale-edge-bridge.md`

**Step 1: 先批准并冻结已有协议设计**

只有 design 中的状态、epoch 所有权、RegisterAck、ForwardAccepted、取消和 drain 语义全部被实现方确认后，状态从 Proposed 改为 Accepted。不要在实现时另造第二套状态机。

**Step 2: 写协议状态机测试**

覆盖：旧 epoch 响应不能完成新 owner 请求；注册未 ACK 的 Connector 不 ready；Agent 断开/重连生成新 epoch；ForwardAccepted 后的 mutation 不自动换 Agent；排空超时进入明确终态。

**Step 3: 先加字段，再切换行为**

proto 使用新的 field number，旧字段不复用。生成代码后，先部署能读写新字段但仍使用旧行为的兼容版本，再进行 coordinated cutover；若无法证明混跑兼容，停止准入后全量升级。

Run:

```bash
cargo test -p sag-tunnel-proto -p stealth-tunnel-agent -p sag-connector -p http-tunnel-bridge
```

Expected: epoch 隔离、registration ack、accepted boundary 和 drain 测试通过。

**Step 4: Commit**

```bash
git add shared/tunnel-proto proxy/agents/stealth-tunnel-agent proxy/connectors/sag-connector proxy/http-tunnel-bridge docs/plans/2026-07-26-stream-epoch-request-state-design.md docs/ops/horizontal-scale-edge-bridge.md
git commit -m "feat: add stream epoch and acknowledged connector registration"
```

## Task 14: 建立可部署的完整 HA 拓扑

**Files:**

- Modify: `docker-compose.hscale-edge.yml`
- Modify: `docker-compose.hscale-auth.yml`
- Modify: `docker-compose.release.edge.yml`
- Modify: `docker-compose.release.intra.yml`
- Modify: `infra/observability/prometheus.yml`
- Create: `infra/observability/alerts/production-hardening.yml`
- Create: `docs/ops/production-ha-topology.md`
- Modify: `docs/ops/deployment-compose.md`

**Step 1: 画出并配置完整副本路径**

至少两个完整的 Bridge → Agent 路径，每条路径都能接到有 RegisterAck 的 Connector sessions。Auth/Policy 至少两个副本；APISIX 至少两个；生产 PostgreSQL 和 Redis 使用有自动 failover 的外部/托管集群；etcd 为三成员。开发 Compose 明确保持单节点，不能被当作生产 HA 模板。

**Step 2: 配置负载均衡和反亲和约束**

公开入口只把流量发给 ready Bridge；同一完整路径的副本不要落在同一故障域。Agent/Connector session 的归属和重连必须遵守 Task 13 的 epoch 规则。

**Step 3: 添加故障和容量告警**

至少告警：ready replica 不足、PG pool wait、audit drop、Redis PEL oldest age、DLQ 增长、queue saturation、Agent 无 Connector、route sync stale、auth invalidation lag、indeterminate idempotency age、restart loop。

**Step 4: Compose 静态与运行验证**

Run:

```bash
docker compose -f docker-compose.edge.yml -f docker-compose.hscale-edge.yml config >/dev/null
docker compose -f docker-compose.intra.yml -f docker-compose.hscale-auth.yml config >/dev/null
```

Expected: 每个副本有唯一 instance id、相同安全环境块、健康检查、restart、资源限制和可达的完整路径。

**Step 5: Commit**

```bash
git add docker-compose.hscale-edge.yml docker-compose.hscale-auth.yml docker-compose.release.edge.yml docker-compose.release.intra.yml infra/observability docs/ops/production-ha-topology.md docs/ops/deployment-compose.md
git commit -m "ops: define complete highly available gateway topology"
```

## Task 15: 统一多 Auth 实例的用户状态和撤权语义

**Files:**

- Create: `infra/migrations/postgres/003_auth_version.sql`
- Modify: `shared/storage/src/users.rs`
- Create: `services/sag-auth/src/user_directory.rs`
- Modify: `services/sag-auth/src/main.rs`
- Modify: `services/sag-auth/Cargo.toml`
- Modify: `services/control-plane-admin/src/main.rs`
- Modify: `docker-compose.hscale-auth.yml`
- Create: `services/sag-auth/tests/multi_instance_consistency.rs`

**Step 1: 写双实例失败测试**

启动 Auth A/B，共享 PostgreSQL，使用独立内存 cache。A 登录得到 token，Admin 在 B 路径禁用用户/修改角色；验证 A/B 都在撤权 SLO 内拒绝旧 token，并在重新登录后只签发新 `auth_version`。

**Step 2: PostgreSQL 成为唯一用户真相源**

给 users 增加单调 `auth_version` 和 `updated_at_ms`。禁用、密码、角色等授权相关修改在一个 transaction 中递增 version；移除仅修改单实例内存用户表的生产路径。

**Step 3: 设计 token/version 校验**

JWT claims 带 `auth_version`。verify 时用 bounded cache 查当前版本；管理变更发布 Redis/PostgreSQL notification 使各实例失效 cache。即使通知丢失，cache TTL 也保证在撤权 SLO 内重新查库。依赖不可用时对高风险授权 fail closed。

**Step 4: 防止旧 token 无期限有效**

令 token 最大有效期、cache TTL 和 invalidation retry 满足明确撤权 SLO。记录 `auth_cache_staleness_seconds`、`auth_invalidation_failed_total`、`token_version_rejected_total`。

Run:

```bash
cargo test -p shared_storage -p sag-auth -p control-plane-admin
cargo test -p sag-auth --test multi_instance_consistency -- --ignored --nocapture
```

Expected: 用户禁用/角色更新后，两个实例均在 SLO 内拒绝旧 token。

**Step 5: Commit**

```bash
git add infra/migrations/postgres/003_auth_version.sql shared/storage/src/users.rs services/sag-auth services/control-plane-admin/src/main.rs docker-compose.hscale-auth.yml
git commit -m "fix: make auth revocation consistent across replicas"
```

## Task 16: 把幂等记录扩展为可人工对账的状态机

**Files:**

- Create: `infra/migrations/postgres/004_idempotency_reconciliation.sql`
- Modify: `shared/storage/src/store.rs`
- Modify: `shared/storage/src/sqlite_store.rs`
- Modify: `shared/storage/src/idempotency.rs`
- Modify: `shared/storage/src/lib.rs`
- Modify: `proxy/agents/stealth-tunnel-agent/src/grpc_server.rs`
- Modify: `services/control-plane-admin/src/main.rs`
- Create: `services/control-plane-admin/src/bin/reconcile_idempotency.rs`
- Create: `shared/storage/tests/idempotency_reconciliation.rs`
- Create: `docs/ops/idempotency-reconciliation-runbook.md`

**Step 1: 先写状态迁移测试**

合法迁移仅允许：

```text
claimed -> dispatched -> completed
claimed -> completed
dispatched -> indeterminate
indeterminate -> completed_by_operator
indeterminate -> released_by_operator
```

禁止自动偷取 `dispatched`/`indeterminate`；只有从未 dispatch 且 owner 已失效的 `claimed` 才能按既有设计回收。旧 `pending` migration 必须根据现有证据保守映射，无法判定时进入 indeterminate。

**Step 2: 增加原子 compare-and-set API**

每次状态变更比较 owner attempt/version，并记录 `dispatched_at_ms`、`completed_at_ms`、`reconciled_by`、`reconcile_reason`、`result_hash`。并发完成和人工操作只能有一个成功。

**Step 3: 在 Agent 的真实 dispatch 边界标记**

只有在 Connector/上游明确接受请求时从 claimed 进入 dispatched；断开前未接受可以按规则释放，接受后丢失结果必须 indeterminate。绝不能因 client retry 自动再发 mutation。

**Step 4: 增加认证、授权、审计的 operator API/CLI**

提供：列出超龄 indeterminate、查看 trace/evidence、确认已完成并提供 result、确认未执行并安全释放。每个操作要求管理员权限、二次确认 reason，并写不可丢的管理审计。

Run:

```bash
cargo test -p shared_storage idempotency
cargo test -p shared_storage --test idempotency_reconciliation
cargo test -p control-plane-admin -p stealth-tunnel-agent
```

Expected: crash-point 和并发 CAS 测试通过；没有 pending/indeterminate 被后台 cleanup 删除或自动重发。

**Step 5: Commit**

```bash
git add infra/migrations/postgres/004_idempotency_reconciliation.sql shared/storage proxy/agents/stealth-tunnel-agent/src/grpc_server.rs services/control-plane-admin docs/ops/idempotency-reconciliation-runbook.md
git commit -m "feat: add auditable idempotency reconciliation"
```

## Task 17: 故障注入、最终验收与分阶段发布

**Files:**

- Create: `scripts/ops/run-production-fault-gate.sh`
- Create: `scripts/ops/run-production-fault-gate.ps1`
- Create: `docs/ops/production-fault-matrix.md`
- Create: `docs/ops/production-hardening-rollout.md`
- Modify: `docs/ops/runbook.md`
- Modify: `README.md`

**Step 1: 自动化故障矩阵**

在持续 full-chain 流量下依次执行，不并发混入多个故障，确保能定位：

| 故障 | 必须证明 |
|---|---|
| kill Bridge | LB 停止发新流量；另一完整路径接管；已接受 mutation 不被重复 dispatch |
| kill Agent | Bridge unready/切换；Connector 新 epoch 注册；旧响应不能污染新请求 |
| kill Connector / 断 gRPC | Agent readiness 失败；只在安全边界重试；超时遵守 absolute deadline |
| Auth/Policy 单实例退出 | ready 副本接管；无 fail-open 授权 |
| PostgreSQL 断开/主从切换 | pool 有界等待；Auth/Policy fail closed；审计 buffer 有界；恢复后无连接风暴 |
| Redis 断开/主从切换 | 不无界同步回退；PEL 可恢复；job 无静默消失 |
| APISIX/workload 断开 | Connector timeout/错误语义正确；响应内存有界 |
| 网络高延迟/丢包 | absolute deadline 不被每跳重置；取消后资源及时释放 |

**Step 2: 定义机器可判定的通过条件**

脚本最终非零退出条件包括：任何错误授权、任何未知 job、同一 mutation 两次副作用、超出 hard permits、PG 连接超预算、永久 PEL、unready 仍接收新流量、RTO 超标或业务 SLO 失败。

Run:

```powershell
pwsh scripts/ops/run-production-fault-gate.ps1 -Scenario all -TrafficRps 350
pwsh scripts/ops/run-production-gate.ps1 -Scenario full_chain -TargetRps 350 -Repeats 3 -SoakMinutes 120
```

Expected: 两个 gate 都为 0；artifact 包含每个请求/job 的最终分类，分类总数等于提交总数。

**Step 3: 分波发布**

1. 先发布 additive migrations；验证旧版本仍运行；
2. 发布 Wave 0，观察身份拒绝、PG pool 和审计指标；
3. 发布 Wave 1，先 drain 老 Redis group，再启用新 Lua/worker；
4. 执行 Wave 2 gate，按结果设置生产 limits；
5. 对 Task 13 协议做停止准入、排空、协调升级；
6. 增加第二条完整路径，再逐一故障注入；
7. 最后启用 auth version 强制和 reconciliation API；
8. 保留上一版本镜像，但禁止回滚到会信任伪造 header 或自动重发 indeterminate mutation 的版本。

**Step 4: 最终文档和结论**

README 只声明经过 gate 证明的容量/HA 属性；runbook 写出报警响应、queue recovery、PG/Redis failover、auth invalidation lag、indeterminate reconciliation 和 break-glass 流程。

**Step 5: Commit**

```bash
git add scripts/ops/run-production-fault-gate.sh scripts/ops/run-production-fault-gate.ps1 docs/ops/production-fault-matrix.md docs/ops/production-hardening-rollout.md docs/ops/runbook.md README.md
git commit -m "test: add production fault and release gates"
```

## 最终发布检查表

- [ ] `cargo fmt --all -- --check` 通过。
- [ ] `cargo test --workspace --all-targets` 在受支持的 Rust 环境通过。
- [ ] release Compose 全部能够 `docker compose ... config`，且 invariant gate 通过。
- [ ] migration 在生产快照副本上验证向前升级、旧 binary 兼容和备份恢复。
- [ ] 身份伪造、缺 token、旧 auth_version 全部 fail closed。
- [ ] PG pool、审计 channel、Redis queue、hard/sync semaphore、request/response body 全部能观测且有硬上限。
- [ ] queue crash-point、deadline/cancellation、epoch、idempotency crash-point 测试全部通过。
- [ ] 三次 full-chain 稳态测试和一次 soak 通过，且 load generator 无 dropped iterations。
- [ ] 单故障矩阵全部通过；未测试组合故障必须明确写为非承诺范围。
- [ ] 生产并发值为测得 saturation knee 的至多 70%，且满足内存预算。
- [ ] runbook、报警、dashboard、on-call 操作权限和人工 reconciliation 流程可用。

## 明确不在本计划中承诺的内容

- 不将单机 Docker Compose 包装成真正的 PostgreSQL、Redis 或 etcd HA；生产需使用合格的外部集群或独立编排。
- 不承诺 exactly-once 网络投递；mutation 安全依靠明确 dispatch 边界、幂等 ledger 和 indeterminate reconciliation。
- 不在没有全链路证据时承诺固定 3k/5k/7k RPS。
- 不用提高 queue/concurrency/buffer 默认值来掩盖背压；容量扩展优先增加完整路径和下游能力。
