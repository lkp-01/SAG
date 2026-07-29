# 超时与线程预算分层：运维手册（操作版）

对应主计划 [high-concurrency-reliability-master-plan.md §4](high-concurrency-reliability-master-plan.md#4-超时与线程预算分层)。原则：**每一跳 deadline ≤ 上游可等待时间**；避免 **k6 已断（status 0）而 bridge/agent 仍在等 60s gRPC**。

**相关**：[tunnel-loadtest-correlation.md](tunnel-loadtest-correlation.md)（失败归因）、[tunnel-capacity-bootstrap.md](tunnel-capacity-bootstrap.md)（压测口径）、[backpressure-queue-runbook.md](backpressure-queue-runbook.md)、[rate-limit-circuit-breaker-runbook.md](rate-limit-circuit-breaker-runbook.md)。

---

## 1. 推荐超时阶梯（由短到长）

数据面 **同步 GET** 全链路（compose 默认，单位 ms 除非注明）：

| 顺序 | 组件 | 配置项 | compose 默认 | 说明 |
|------|------|--------|--------------|------|
| 1 | **k6** | `run-load-dataplane.ps1` **`-RequestTimeout`** | **90s**（脚本默认；快速冒烟可显式 `-RequestTimeout 20s`） | 客户端总等待；**短于** Zentinel/bridge 时多见 **status 0**、`sag_dataplane_failure_cause_total{cause:timeout}` |
| 2 | **Zentinel** | `dataplane-compose.kdl` `routes.policies.timeout-secs` | **90s** | 到 bridge 上游策略 |
| 2b | **Zentinel** | `listener request-timeout-secs`（`:10080`） | **120s** | 监听器硬上限，应 **≥** route timeout |
| 3 | **bridge** | `SAG_BRIDGE_FORWARD_TIMEOUT_MS` | **60000** | `tokio::time::timeout` 包住 Unary `Forward` |
| 4 | **bridge** | `SAG_GRPC_RPC_TIMEOUT_MS` | **120000** | tonic Channel；须 **≥** `SAG_BRIDGE_FORWARD_TIMEOUT_MS` |
| 5 | **agent** | `SAG_FORWARD_TIMEOUT_MS` | **58000** | 等 connector 流内响应；宜 **<** bridge forward、**>** connector HTTP |
| 6 | **connector** | `SAG_CONNECTOR_HTTP_TIMEOUT_MS` | **55000** | `reqwest` 调 APISIX |
| 7 | **connector** | `SAG_CONNECTOR_GRPC_CHANNEL_TIMEOUT_MS` | **120000** | 到 agent 的 gRPC Channel |
| 8 | **APISIX** | 路由 `timeout` / 插件 | **运维配置**（仓库未固定默认路由 JSON） | 宜 **≤** connector HTTP，避免 connector 先断而 APISIX 仍挂起 |
| 9 | **mock** | Python `mock_http_server` | 无显式全局 deadline | 瓶颈常在单进程 CPU；见 [intra-mock-apisix-horizontal.md](intra-mock-apisix-horizontal.md) |

**记忆口诀**：`connector_http(55s) < agent_forward(58s) < bridge_forward(60s) ≤ grpc_rpc(120s)`；入口 **k6 / Zentinel ≥ 90s** 与高压压测一致。

---

## 2. 全链路表：超时 + 并发 + 队列（数据面）

| 跳 | 超时（典型） | 最大并发 / 队列 | 环境变量或配置 |
|----|--------------|-----------------|----------------|
| **k6** | `-RequestTimeout` | VU / 阶段 QPS（脚本参数） | `run-load-dataplane.ps1`；`load-dataplane-k6.js` → `sag_dataplane_failure_cause_total` |
| **Zentinel :10080** | 90s route / 120s listener | 连接由 OS + `limits` | `proxy/zentinel-proxy/config/dataplane-compose.kdl` |
| **http-tunnel-bridge** | forward 60s；RPC 120s | `SAG_BRIDGE_SOFT/HARD_INFLIGHT`、`MAX_TUNNEL_INFLIGHT`、Redis 队列 | 见 [backpressure-queue-runbook.md](backpressure-queue-runbook.md) |
| **stealth-tunnel-agent** | forward 58s；policy/auth 各 5s | `SAG_POLICY_INFLIGHT_LIMIT`、`SAG_AUTH_INFLIGHT_LIMIT`；`SAG_MAX_PENDING_WAITERS` | `docker-compose.edge.yml` |
| **sag-connector** | HTTP 55s；gRPC channel 120s | `SAG_CONNECTOR_MAX_INFLIGHT`、`SAG_CONNECTOR_ACCEPT_QUEUE` | 见 [rate-limit-circuit-breaker-runbook.md](rate-limit-circuit-breaker-runbook.md) |
| **APISIX → mock** | 路由级 | worker / 多实例 | `infra/apisix` + etcd 路由 |

---

## 3. k6「status 0」与 bridge「5xx」如何区分

| 现象 | 常见原因 | 对照指标 / 日志 |
|------|----------|-----------------|
| **status 0**、k6 `timeout` | **客户端 deadline** 先到（k6 20s 而服务端 60s） | `sag_dataplane_failure_cause_total{cause:timeout}`、`sag_dataplane_http_first_status_total{status:0}` |
| **502/504** 经 Zentinel | bridge forward 超时或 gRPC 断 | `bridge_grpc_channel_forward_err_total`；bridge 日志 |
| **upstream 5xx**（隧道内） | connector / APISIX / mock | correlation 文档；`connector_forward_total` |
| **202 后 poll 超时** | 队列 worker 慢或 poll 间隔 | `sag_dataplane_queue_poll_total`；须 `-PollDataplane202` |

**对齐建议**：高压压测使用 **`-RequestTimeout 90s`**（与 Zentinel 90s 一致）；勿在未改服务端的情况下单独把 k6 设为 20s 再与 bridge 60s 对比成功率。

---

## 4. 线程 / 任务预算（connector 重点）

| 组件 | 模型 | 说明 |
|------|------|------|
| bridge / agent | **Tokio task** | 异步；热路径避免 `spawn_blocking` 堆积 |
| **sag-connector** | **mpsc** `accept_queue` + `FuturesUnordered` **max_inflight** | 满则 **503** + `connector_forward_reject_total`；**无**单独 blocking 线程池 |
| connector **reqwest** | `pool_max_idle_per_host(2048)` | 对齐单 APISIX 上游高压；见 `sag-connector/src/main.rs` |
| agent **pending** | `SAG_MAX_PENDING_WAITERS` semaphore + `PendingRequest::drop` generation-aware cleanup | 按 `attempt_id` 回收 pending、permit，并 best-effort 发送 Connector cancel |

---

## 5. 自检命令

### 5.1 脚本（推荐）

在 **Edge** 或 **Intra** 宿主机、`sag-cloud` 目录：

```bash
bash scripts/ops/verify-timeout-chain.sh
```

Windows（Edge 本机 Docker）：

```powershell
cd D:\path\to\sag-cloud
.\scripts\ops\verify-timeout-chain.ps1
```

### 5.2 手工拉 env（Edge bridge）

```bash
docker compose -f docker-compose.edge.yml exec http-tunnel-bridge sh -c 'env | grep -E "SAG_BRIDGE_FORWARD|SAG_GRPC_RPC|SAG_GRPC_CONNECT" | sort'
docker compose -f docker-compose.edge.yml exec stealth-tunnel-agent sh -c 'env | grep -E "SAG_FORWARD_TIMEOUT|SAG_POLICY_EVALUATE_TIMEOUT" | sort'
```

### 5.3 Intra connector

```bash
docker compose -f docker-compose.intra.yml exec sag-connector sh -c 'env | grep -E "SAG_CONNECTOR_HTTP|SAG_CONNECTOR_GRPC|SAG_CONNECTOR_MAX|SAG_CONNECTOR_ACCEPT" | sort'
```

---

## 6. 调参顺序（保守）

1. **先对齐 k6 与 Zentinel**（≥ 90s）再调服务端。  
2. **保持阶梯**：勿单独把 `SAG_BRIDGE_FORWARD_TIMEOUT_MS` 降到小于 connector HTTP。  
3. **收紧时从外向内**：先缩 k6 RPS，再缩 `SOFT_INFLIGHT` / 队列，最后才动 forward 毫秒级。  
4. 每步保留 k6 JSON 中 **`failure_cause`** 与 **`dataplane_http_first_status_total`** 片段。

---

## 7. 回滚

恢复 `.env` / compose 中超时相关项后 **`--force-recreate`** 对应服务（`http-tunnel-bridge`、`stealth-tunnel-agent`、`sag-connector`、Zentinel 若改 kdl 需重建镜像或挂载配置）。

---

## 8. 代码锚点

- Bridge：`proxy/http-tunnel-bridge/src/main.rs`（`forward_timeout_ms`、`SAG_GRPC_RPC_TIMEOUT_MS` 启动 warn）。  
- Agent：`proxy/agents/stealth-tunnel-agent/src/grpc_server.rs`、`config.rs`。  
- Connector：`proxy/connectors/sag-connector/src/main.rs`（`http_timeout_ms`、`max_inflight`）。  
- k6：`scripts/ops/load-dataplane-k6.js`（`classifyFailure`、`sag_dataplane_failure_cause_total`）。
