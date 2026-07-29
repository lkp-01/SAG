# 高并发可靠性：主计划（水平扩展 / 背压 / 限流熔断 / 超时预算 / 缓存 / 异步化）

本文档对 SAG 数据面与控制面在 **高负载、有限 CPU** 下的演进方向做 **分域计划**，便于后续按优先级迭代代码与运维。与下列文档配套阅读：

- [tunnel-capacity-bootstrap.md](tunnel-capacity-bootstrap.md)：B/C/D 一次性 sysctl + compose + k6 poll  
- [timeout-deadline-runbook.md](timeout-deadline-runbook.md)：全链路超时阶梯、k6 与 Zentinel 对齐、自检脚本  
- [tunnel-loadtest-correlation.md](tunnel-loadtest-correlation.md)：压测窗口日志与指标对齐  
- [bridge-grpc-channel-pool-future.md](bridge-grpc-channel-pool-future.md)：bridge→agent 多 Channel 池（设计说明；实现见 `http-tunnel-bridge` 与 `SAG_BRIDGE_GRPC_CHANNEL_POOL_SIZE`）  
- [cache-read-runbook.md](cache-read-runbook.md)、[async-patterns-runbook.md](async-patterns-runbook.md)、[implementation-roadmap.md](implementation-roadmap.md)、[docs-maintenance-runbook.md](docs-maintenance-runbook.md)

---

## 架构速览（便于后文「谁扩谁」）

```text
[浏览器/k6] → Edge: zentinel(10080) → http-tunnel-bridge(9000→agent) → stealth-tunnel-agent:50051
                                                                    ↘ Unary Forward (gRPC)
Intra: sag-connector ——双向流 Register/Heartbeat + 流内 Request/Response——→ agent
       sag-connector → APISIX → mock/业务
```

- **无状态 HTTP 入口**：Zentinel 反向代理、Bridge 的 HTTP 面、APISIX、多数业务 HTTP 服务。  
- **强状态 / 单点逻辑**：`stealth-tunnel-agent` 内 **按 `endpoint` 注册的 connector 流**（`ConnectorRegistry`）；每条隧道 **一个** connector 控制面身份与 **一条** 长连接。  
- **半状态**：`http-tunnel-bridge` 的 **gRPC 连接池**（`SAG_BRIDGE_GRPC_CHANNEL_POOL_SIZE` 条 HTTP/2 到 agent）+ 可选 **Redis 队列**。

---

## 1. 水平扩展

### 1.1 目标

在 **不突破协议语义** 的前提下，通过 **多副本 + 负载均衡** 提高吞吐；对 **有状态组件** 用 **分片、连接亲和或共享注册层** 避免「多副本互踩」。

### 1.2 按组件：是否适合水平扩展、收益与约束

| 组件 | 典型部署 | 水平扩展适合度 | 说明 |
|------|-----------|----------------|------|
| **APISIX** | Intra | **高** | 网关成熟能力：多 worker / 多实例 + LB；数据面路径优先扩。 |
| **mock / 上游业务** | Intra | **高** | 压测瓶颈常在 mock；多实例 + 上游负载均衡或 DNS 轮询。 |
| **sag-policy / sag-auth / control-plane-admin** | Edge | **高** | 无隧道长状态；前置 LB + 多副本；注意 **Postgres/Redis** 连接池与迁移锁。 |
| **zentinel-proxy** | Edge | **中高** | 若仅为反向代理到固定 `bridge:9000`，多副本需 **同一后端或后端池**；配置见 `proxy/zentinel-proxy/config/*.kdl`。 |
| **http-tunnel-bridge** | Edge | **中（需设计）** | HTTP 面无状态，但 **每条请求最终Unary到同一逻辑 agent**。**多 bridge 副本** 可分担 **HTTP accept/TLS/JSON**，但若全部仍打 **同一 agent 单进程**，agent 与 **单 connector 流** 仍可能是瓶颈。 |
| **stealth-tunnel-agent** | Edge | **低→中（要改架构）** | 当前 `ConnectorRegistry` 为 **进程内内存**（`proxy/agents/stealth-tunnel-agent/src/connector_registry.rs`）。多 agent 副本时：同一 `connector_endpoint` **不能**同时在两个进程注册，除非引入 **共享注册/路由层** 或 **按租户分片到固定 agent 实例**（类似 Redis/NATS 协调）。 |
| **sag-connector** | Intra | **中（按实例分片）** | 每个进程 **一条** 到 agent 的隧道流；**多 connector** 需 **不同 `SAG_CONNECTOR_ID` + 不同 `SAG_CONNECTOR_ENDPOINT`**，且控制面 **`tunnel_routes` 中路由到正确 endpoint**。等同 **多隧道分片**，不是「同一 endpoint 无限副本」。 |
| **Redis（bridge 队列）** | Edge | **垂直+副本** | 队列用 DB `/2`；扩展多为 **内存/IO** 与 **哨兵/集群**（注意 Stream 语义与运维复杂度）。 |
| **Postgres** | Edge | **读写分离/连接池** | 控制面与审计；数据面热路径尽量不直连 PG。 |

### 1.3 有限 CPU 下「优先扩谁」收益更大（数据面）

1. **Intra：APISIX + mock（及真实业务）**  
   - 500 RPS 时若 **`upstream_5xx` 占比高**，先验证 **mock/上游** 是否先饱和；扩 mock 或调大 worker 往往比先加 bridge 更划算。见 [intra-mock-apisix-horizontal.md](intra-mock-apisix-horizontal.md)。  
2. **Edge：bridge 进程数（谨慎）**  
   - 在 **单 agent、单 connector 流** 不变前提下，多 bridge 副本 = **多条到 agent 的 gRPC 连接**（每进程池大小见 `SAG_BRIDGE_GRPC_CHANNEL_POOL_SIZE`），可缓解 **单 bridge 进程** 的 HTTP/CPU，但会 **叠加到 agent 的 accept 与 forward 调度**；需配合 **agent CPU、nofile、MAX_TUNNEL_INFLIGHT**。见 [horizontal-scale-edge-bridge.md](horizontal-scale-edge-bridge.md)。  
3. **单 bridge 内：多 gRPC Channel 池（代码）**  
   - 已实现：`SAG_BRIDGE_GRPC_CHANNEL_POOL_SIZE`（默认 `1`，最大 `32`），指标 `bridge_grpc_channel_forward_total{channel}` / `bridge_grpc_channel_forward_err_total{channel}`。  
4. **多 connector 分片（产品与路由）**  
   - 例如 `connector-local-001` / `connector-local-002` 各绑不同 **app 或路径前缀**；控制面下发不同 `connector_endpoint`。逻辑链：**k6/客户端 → 仍打同一 Zentinel URL** 时，需 **网关或策略** 把流量分到不同 tunnel 路由（否则扩 connector 无意义）。

### 1.4 扩展后的连接与适配

| 变更 | 连接侧需适配的内容 |
|------|---------------------|
| 多 **APISIX** | connector 的 `SAG_APISIX_BASE_URL` 指向 VIP 或 DNS；证书/SNI。 |
| 多 **bridge**（同 agent） | Zentinel `upstream` 指向 **bridge 服务 DNS**（Docker Swarm/K8s Service）或 **L4 LB VIP**；每台 bridge 仍配置 `SAG_TUNNEL_GRPC_ENDPOINT=https://stealth-tunnel-agent:50051`（或 LB 到 agent——仅当 agent 也多实例且下层已解决注册一致性）。 |
| 多 **agent**（未来） | connector **只能**连到一个对外 **gRPC VIP**；VIP 后若多 agent，须有 **gRPC 层路由**（少见）或 **每 agent 独立对外地址 + 分片**。 |
| 多 **connector** | 每个实例独立 **mTLS 客户端证书**（若按机部署）或共享证书但 **不同 connector_id**；**Postgres 中 tunnel_routes** 与 **agent 侧路由表** 一致。 |

### 1.5 水平切换（发布/流量迁移）建议逻辑

1. **先加后减**：新副本健康检查（`/metrics`、smoke）通过后再摘老副本。  
2. **长连接组件**（connector）：切换 = **新进程注册 + 旧进程优雅退出**；避免双注册同一 `endpoint`（agent 会覆盖或错乱）。代码参考：`grpc_server.rs` 中 stream 断开后 `unregister`（见 `stealth-tunnel-agent` 隧道 inbound 循环）。  
3. **bridge**：可滚动重启；注意 **Redis 队列中未消费条目** 在重启期间仍由 **worker 或新进程** 消费（同 Redis URL）。  
4. **k6 基线**：切换前后 **同一 Summary 字段**（`dataplane_get`、`upstream_5xx`、`status:202`）对比。

### 1.6 代码锚点（后续迭代）

- Bridge gRPC 池与转发：`proxy/http-tunnel-bridge/src/main.rs` 中 `TunnelClientPool`、`forward_request_inner`、`connect_tunnel_client`。  
- Agent 注册表：`proxy/agents/stealth-tunnel-agent/src/connector_registry.rs`（`register` / `send_request_to_connector` / `resolve_response`）。  
- Connector 流与背压：`proxy/connectors/sag-connector/src/main.rs`（`max_inflight`、`accept_queue_cap`、Register/Heartbeat 任务）。

### 1.7 落地状态（水平扩展）

| 项 | 状态 |
|----|--------|
| 去掉 `http-tunnel-bridge` 固定 `container_name`，支持 `compose up --scale` | 已落地：`docker-compose.edge.yml`、`docker-compose.yml` |
| 可选第二 bridge + Zentinel 双 upstream kdl + override compose | 已落地：`docker-compose.hscale-edge.yml`、`proxy/zentinel-proxy/config/dataplane-compose.hscale.kdl`、[horizontal-scale-edge-bridge.md](horizontal-scale-edge-bridge.md) |
| 单 bridge 内多 gRPC Channel | 已落地：`SAG_BRIDGE_GRPC_CHANNEL_POOL_SIZE`，见 `http-tunnel-bridge` 与 [bridge-grpc-channel-pool-future.md](bridge-grpc-channel-pool-future.md) |
| Intra APISIX/mock 扩展说明 | 已落地：[intra-mock-apisix-horizontal.md](intra-mock-apisix-horizontal.md) |
| **多 stealth-tunnel-agent 副本 + 共享 ConnectorRegistry** | **未做**（需单独架构/存储） |

---

## 2. 背压与排队

**详细操作步骤**（Redis 检查、metrics 判定树、调参顺序、k6 口径、回滚）见 [backpressure-queue-runbook.md](backpressure-queue-runbook.md)。

### 2.1 原则

**有界队列 + 明确策略**：等（排队）、丢（429/503）、降级（缓存/默认响应）。禁止无界内存队列拖垮进程。

### 2.2 当前实现（bridge）

- **同步在途计数** `sync_inflight` + **软门限** `SAG_BRIDGE_SOFT_INFLIGHT`：**达到且 Redis 可用** 时尝试 **202 入队**（`main.rs` 中 enqueue 分支；指标 `bridge_queue_202_total`、`bridge_soft_gate_entered_total`）。  
- **硬门限 / 隧道 semaphore**：`SAG_BRIDGE_MAX_TUNNEL_INFLIGHT`；满时可能 **429** 或走队列分支（以当前二进制逻辑为准）。  
- **Worker**：`SAG_BRIDGE_WORKER_CONCURRENCY` 消费 Redis Stream，仍受 **tunnel semaphore** 约束。  
- **失败回退**：Redis 序列化/写入失败时 **打 warn 并同步 fallback**（`bridge_soft_fallback_total` reason `redis_enqueue`），此时 **202 为 0** 但负载仍撞击同步路径。

### 2.3 「202 仍为 0」排查计划（运维 + 代码）

1. **指标**：`bridge_soft_gate_entered_total` 是否上升；`bridge_soft_fallback_total` 是否非 0；`bridge_sync_inflight` 与 soft 关系。  
2. **Redis**：`PING`、`DB /2`、OOM、慢日志；队列长度与 `SAG_BRIDGE_QUEUE_MAX_LEN`。  
3. **压力形态**：`sync_inflight` 是否在未达到 soft 前已被 **上游 5xx/超时** 拖死（表现为失败而非排队）。  
4. **代码增强（可选）**：enqueue 失败时 **区分** 返回 503 vs 同步 forward — **已实现**：环境变量 `SAG_BRIDGE_SOFT_ENQUEUE_ON_FAILURE`（`fallback` \| `503` \| `service_unavailable`）；指标 `bridge_soft_enqueue_failure_503_total`。其它 **gauge 增强**（若仍希望补充除 `bridge_sync_inflight` 外的观测）可后续迭代。

### 2.4 代码锚点

- `proxy/http-tunnel-bridge/src/main.rs`：soft/hard、enqueue、fallback 日志与 counter。  
- `proxy/http-tunnel-bridge/src/queue.rs`：`QueueConfig::from_env`、`XADD`、worker drain。

### 2.5 落地状态（文档与指标）

| 项 | 状态 |
|----|------|
| §2 原则与实现说明 | 保留在本节 §2.1–§2.4；**可执行 runbook** 已拆至 [backpressure-queue-runbook.md](backpressure-queue-runbook.md) |
| 队列 Stream / 组 / DLQ | 已落地：`sag:dataplane:queue`、`bridge-workers`、`sag:dataplane:dlq`（`queue.rs`） |
| bridge 指标（示例） | 已落地：`bridge_sync_inflight`、`bridge_soft_gate_entered_total`、`bridge_queue_202_total`、`bridge_queue_enqueue_total`、`bridge_soft_fallback_total`、`bridge_soft_enqueue_failure_503_total`（`SAG_BRIDGE_SOFT_ENQUEUE_ON_FAILURE=503` 时）、`bridge_queue_reject_total`、`bridge_tunnel_*`、`bridge_queue_depth`、`bridge_worker_forward_total`、`bridge_queue_dlq_total`、`bridge_queue_poll_throttled_total` 等 |
| §2.3 第 4 条「可选代码增强」 | **503 vs 同步 forward**：已落地 `SAG_BRIDGE_SOFT_ENQUEUE_ON_FAILURE`（见 [backpressure-queue-runbook.md](backpressure-queue-runbook.md) §2.5）；**额外 gauge 等** 未强制纳入本次 |

---

## 3. 限流与熔断

**详细操作步骤**（connector/agent env 与指标、Zentinel 与 APISIX 运维 checklist、判定树、调参顺序、回滚）见 [rate-limit-circuit-breaker-runbook.md](rate-limit-circuit-breaker-runbook.md)。

### 3.1 放置位置（由外到内）

1. **APISIX**：对 **公网或内网入口** 做 **limit-req / limit-conn**；对脆弱上游做 **proxy 超时 + 重试次数 0 或小**。  
2. **Zentinel**：对到 bridge 的并发连接与 RPS 限制（视 kdl 能力）。  
3. **bridge**：在进入 gRPC 前 **全局 semaphore**（隧道 inflight）；**按 `x-sag-app-id` 的 HTTP 令牌桶**（`SAG_BRIDGE_HTTP_RPS_PER_APP`，可选）；**Unary Forward 全局简易熔断**（`SAG_BRIDGE_FORWARD_CB_FAILURE_THRESHOLD` / `SAG_BRIDGE_FORWARD_CB_COOL_OFF_MS`，可选）；**per-IP** HTTP 限流仍 **未做**。  
4. **connector**：已有 **max inflight + accept queue**；队列满策略需与指标 `accept_queue_full`（若有）对齐。  
5. **agent**：`SAG_POLICY_INFLIGHT_LIMIT` 等与 **出向 HTTP** 相关；隧道侧由 `SAG_MAX_PENDING_WAITERS` semaphore、按 `attempt_id` 的 pending 表和 `PendingRequest::drop` 的 generation-aware 清理共同限制并回收在途请求，需压测验证 permit、gauge 和取消消息都能归零。

### 3.2 熔断（circuit breaker）

- **对 APISIX→mock**：上游连续失败时 **短路**，返回 **503/固定 JSON**，避免 connector 线程池阻塞。可用 APISIX 插件或 mock 前小 sidecar。  
- **对 agent→connector**：Unary Forward 失败率超阈时，bridge 或 agent **快速失败**（需定义「半开」探测间隔，避免抖动）。当前 Bridge 每个逻辑请求只执行一次 Forward；`Unavailable` 只触发槽位异步重连供后续请求使用。另可选 **全局连续失败熔断**（`SAG_BRIDGE_FORWARD_CB_FAILURE_THRESHOLD`，见 [rate-limit-circuit-breaker-runbook.md](rate-limit-circuit-breaker-runbook.md) §6）。**按错误类型** 的半开状态机仍可后续扩展。

### 3.3 重试风暴控制

- **k6 / 客户端**：登录重试已有上限；数据面避免 **无限轮询 202**。  
- **服务间**：当前 Bridge/APISIX 对同一逻辑请求不自动重试；写请求依赖 Agent durable claim 和下游 `Idempotency-Key`。若未来恢复只读重试，必须复用绝对 deadline 并生成新的 `attempt_id`。

### 3.4 代码锚点

- Bridge：`forward_request_inner` 与 `connect_tunnel_client`。  
- Connector：`max_inflight`、`accept_queue_cap` 与 forward 处理循环（`sag-connector/src/main.rs`）。

### 3.5 落地状态（文档与实现）

| 项 | 状态 |
|----|------|
| §3 原则与分层说明 | 保留在本节 §3.1–§3.4；**可执行 runbook** 已拆至 [rate-limit-circuit-breaker-runbook.md](rate-limit-circuit-breaker-runbook.md) |
| **connector** `max_inflight` + `accept_queue` + 拒绝指标 | 已落地：`SAG_CONNECTOR_MAX_INFLIGHT`、`SAG_CONNECTOR_ACCEPT_QUEUE`；`connector_forward_reject_total{reason="accept_queue_full"}`、`connector_forward_accept_wait_seconds`、`connector_forward_total` 等（`sag-connector/src/main.rs`） |
| **agent** policy/auth 并发 | 已落地：`SAG_POLICY_INFLIGHT_LIMIT`、`SAG_AUTH_INFLIGHT_LIMIT`（`stealth-tunnel-agent` `config.rs`）；`agent_policy_*` 等指标 |
| **bridge** 隧道 semaphore / 429 / 202 / Redis 队列 | 已落地；操作细排见 [backpressure-queue-runbook.md](backpressure-queue-runbook.md) |
| **bridge** 按 `x-sag-app-id` 的 HTTP RPS 限流 | 已落地：`SAG_BRIDGE_HTTP_RPS_PER_APP`；`bridge_http_app_ratelimit_reject_total`（`limits.rs` + `main.rs`） |
| **bridge** Unary Forward 全局简易熔断（连续全失败 + cool-off） | 已落地：`SAG_BRIDGE_FORWARD_CB_FAILURE_THRESHOLD`、`SAG_BRIDGE_FORWARD_CB_COOL_OFF_MS`；`bridge_forward_circuit_*` 指标 |
| **bridge** HTTP 层 **per-IP** 限流 | **未做** |
| **完整熔断器**（APISIX 插件级或按错误类型的半开状态机） | **部分**：bridge 仅有 **全局连续失败窗口** + 原有 **有限重试**；APISIX 侧仍依赖路由插件 |

---

## 4. 超时与线程预算分层

**详细操作步骤**（全链路超时/并发/队列表、k6 status 0 与 5xx 区分、自检脚本、调参顺序）见 [timeout-deadline-runbook.md](timeout-deadline-runbook.md)。

### 4.1 原则

每一跳 **deadline ≤ 上游用户可等待时间**，且 **向下游传递的累积超时** 小于连接与线程池能承受的排队时间。避免出现 **客户端 20s、k6 已断、服务端还在等 60s gRPC** 的线程堆积。

### 4.2 当前链（compose 注释链）

典型关系（具体以仓库 `docker-compose.edge.yml` / `docker-compose.intra.yml` 为准）：

- `SAG_BRIDGE_FORWARD_TIMEOUT_MS`（bridge 包 Unary 的 tokio timeout）  
- `SAG_GRPC_RPC_TIMEOUT_MS`（tonic Channel，应 **≥** forward）  
- `SAG_FORWARD_TIMEOUT_MS`（agent 等 connector）与 `SAG_CONNECTOR_HTTP_TIMEOUT_MS`  
- k6 `RequestTimeout`（`run-load-dataplane.ps1` 默认 **90s**；快速冒烟可显式 `-RequestTimeout 20s`）

### 4.3 迭代计划

1. **画一张表**：从 k6 → zentinel → bridge → agent → connector → APISIX → mock，每跳 **超时、最大并发、队列长度**。  
2. **收紧明显不合理项**：例如 k6 **20s** 而 bridge **60s** 时，失败表现多为 **k6 timeout (status 0)**，与 **bridge 5xx** 混淆；可 **对齐** 或 **分层记录原因**。  
3. **线程/任务**：Rust 异步以 **task** 为主；connector **mpsc + worker** 需看 **blocking 线程池**（若有）与 **hyper client** 连接池上限。

### 4.4 代码锚点

- Bridge：`SAG_BRIDGE_FORWARD_TIMEOUT_MS`、`SAG_GRPC_RPC_TIMEOUT_MS`（`main.rs` 启动时若 RPC < forward 打 warn）。  
- Agent：`SAG_FORWARD_TIMEOUT_MS`、`SAG_POLICY_EVALUATE_TIMEOUT_MS`（`config.rs` / `grpc_server.rs`）。  
- Connector：`SAG_CONNECTOR_HTTP_TIMEOUT_MS`、`reqwest` 超时（`sag-connector/src/main.rs`）。  
- k6：`scripts/ops/load-dataplane-k6.js`（`sag_dataplane_failure_cause_total`、`sag_dataplane_http_first_status_total`）。

### 4.5 落地状态（文档与对齐）

| 项 | 状态 |
|----|------|
| §4 原则与 §4.2 链说明 | 保留在本节；**可执行 runbook** 已拆至 [timeout-deadline-runbook.md](timeout-deadline-runbook.md) |
| 全链路超时/并发/队列表 | 已落地（runbook §1–§2） |
| k6 与 Zentinel 默认对齐（减轻 status 0 误判） | 已落地：`run-load-dataplane.ps1` 默认 **90s**；Zentinel kdl **90s/120s**（既有） |
| 失败分层记录 | 已落地：k6 `sag_dataplane_failure_cause_total` 等（既有 `load-dataplane-k6.js`） |
| 自检脚本 | 已落地：`scripts/ops/verify-timeout-chain.sh`、`.ps1` |
| 自动向下游传递 deadline（gRPC metadata / 链路 trace） | **未做**（仍以各跳独立 env 为准） |

---

## 5. 缓存与读多写少

**详细操作步骤**（可缓存路径清单、policy/agent/auth 指标、APISIX 试点 checklist）见 [cache-read-runbook.md](cache-read-runbook.md)。

### 5.1 适合缓存的路径

- **策略评估结果**：在 **租户+资源+动作** 维度可短时缓存（已有 policy 服务相关缓存路径则 **保持并观测命中率**）。  
- **静态资源 / OpenAPI 文档**：CDN 或 APISIX **proxy-cache**（短 TTL）。  
- **控制面读多**：路由列表等在 **一致性要求允许** 下 TTL 秒级缓存。

### 5.2 不适合盲目缓存的路径

- **数据面 `GET /dev/`**（或等价敏感读）：带 **租户、令牌、策略上下文**，全局语义缓存易导致 **越权或陈旧策略**；若做仅 **mock 层** 或 **无鉴权静态页** 可单独讨论。

### 5.3 迭代计划

1. 压测路径上 **列清单**：哪些 URL 可静态化。  
2. APISIX 路由级 **cache plugin** 试点（仅 mock 域名）。  
3. policy：已有 Redis/内存则加 **metrics**：命中率、剔除率。

### 5.4 代码锚点

- `services/sag-policy/src/main.rs`：`SAG_POLICY_CACHE_*`、`cache_hit_total`、`policy_eval_cache_hit_rate`。  
- `proxy/agents/stealth-tunnel-agent/src/grpc_server.rs`：负缓存、`SAG_NEGATIVE_CACHE_*`。  
- `services/sag-auth/src/main.rs`：login memo、`SAG_LOGIN_MEMO_*`、`SAG_SESSION_REDIS_URL`。

### 5.5 落地状态（文档与实现）

| 项 | 状态 |
|----|------|
| §5 原则与 §5.1–§5.3 | 保留在本节；**可执行 runbook** 已拆至 [cache-read-runbook.md](cache-read-runbook.md) |
| 压测路径可缓存清单 | 已落地（runbook §1） |
| policy / agent / auth 缓存与指标 | **已落地**（代码既有）；runbook §2 + `verify-cache-metrics.sh` |
| APISIX mock **proxy-cache** 默认路由 | **未提交**（运维试点 checklist 在 runbook §3） |
| 数据面 `/dev/` 全局缓存 | **未做**（原则禁止） |

---

## 6. 异步化

**详细操作步骤**（202 队列、Edge 侧 bridge/agent 审计、connector 有界队列与 Prometheus 指标）见 [async-patterns-runbook.md](async-patterns-runbook.md)。

### 6.1 已实现形态

- **Bridge 202 + Redis Stream + 客户端 poll**：把 **同步 dataplane** 变为 **可观测的异步完成**；与 k6 `PollDataplane202` 配套。

### 6.2 可演进方向

- **审计 / 日志**：Agent/bridge 在 Edge 侧负责 `audit_logs` / `fault_events`；Connector 只保留 hop 指标，不直接写 PG。  
- **控制面变更**：大表同步可考虑 **消息队列**（Outbox 模式）而非同步 HTTP 全链路阻塞。  
- **connector 侧**：若 HTTP 调用 mock 可改为 **内部队列 + worker**（已是类似结构则扩 worker 与队列 cap 并 **观测延迟**）。

### 6.3 代码锚点

- `http-tunnel-bridge/src/queue.rs`：worker、`XREADGROUP`。  
- Agent/bridge 的 `shared_storage` / audit：确认写路径是否在 `spawn_blocking` 或独立任务。

### 6.4 落地状态（文档与实现）

| 项 | 状态 |
|----|------|
| §6 原则与演进方向 | 保留在本节 §6.1–§6.3；**可执行 runbook** 已拆至 [async-patterns-runbook.md](async-patterns-runbook.md) |
| Bridge **202 + Redis + poll** | **已落地**（见 [backpressure-queue-runbook.md](backpressure-queue-runbook.md)） |
| Bridge 审计 **tokio::spawn** | **已落地**（`main.rs` `metrics_mw`） |
| Connector **accept_queue + Prometheus hop metrics** | **已落地**；审计由 Edge 侧 Agent/bridge 持久化 |
| 控制面 **Outbox + MQ** | **未做** |

---

## 7. 建议实施顺序（路线图）

**P0–P3 可勾选清单与出口标准**见 [implementation-roadmap.md](implementation-roadmap.md)。

| 阶段 | 内容 |
|------|------|
| P0 | 指标与日志对齐（`tunnel-loadtest-correlation.md`）；确认 5xx 来自 **mock** 还是 **隧道/bridge**。 |
| P1 | Intra：**APISIX + mock** 容量（见 `intra-mock-apisix-horizontal.md`）；Edge：**多 bridge / 多 Channel**（见 `horizontal-scale-edge-bridge.md` 与 `SAG_BRIDGE_GRPC_CHANNEL_POOL_SIZE`）；compose **超时链** 表。 |
| P2 | **多 bridge + Zentinel RR**（`docker-compose.hscale-edge.yml`）；观测 agent CPU 与 **单 agent 极限**。 |
| P3 | **多 connector 分片**（产品路由 + 多 `connector_endpoint`）；必要时 **agent 注册表外置**（大改）。 |

---

## 8. 文档维护

**修订记录模板、基线 JSON 命名、`archive-k6-baseline.ps1`、runbook 索引**见 [docs-maintenance-runbook.md](docs-maintenance-runbook.md)。

- 每次重大架构变更（多 bridge、多 agent、外置 registry）在本文件追加 **「修订记录」** 小节：日期、决策、废弃段落。  
- 与 **压测基线 JSON** 同版本号保存，便于回归对比。

### 修订记录

- **2026-05-19**：§2–§6 代码与 runbook 批次（背压 503 开关、bridge 限流/熔断、超时链、k6 默认 90s）；§5–§8 runbook（cache / async / roadmap / docs-maintenance）与自检脚本。  
- **2026-05-14**：§1 水平扩展落地（compose scale、hscale override、grpc channel pool、Intra 扩展说明）；主计划迁入 `sag-cloud/docs/ops/` 为唯一真源。
