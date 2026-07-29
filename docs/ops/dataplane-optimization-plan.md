# 数据面容量与尾延迟：问题定位与优化计划

本文档结合 **压测 JSON**（`artifacts/k6-*.json`）、**《压力测试记录》** 与 **关键组件源码**，给出「瓶颈在哪、对应哪段代码、下一步怎么改」。部署拓扑参见双机 Handoff（Edge Zentinel+bridge+agent / Intra connector+APISIX）。

---

## 1. 已观察到的现象（证据）

| 现象 | 含义 |
|------|------|
| **2026-05-07 sweep**（`k6-sweep-20260507-171628`，300/500/700 RPS）：**dataplane_get 成功率 0%**、`http_req_failed` **100%**、p95 **≈30s**、`queue_poll` **0** | 与 **gRPC Channel `SAG_GRPC_RPC_TIMEOUT_MS` 默认曾为 30s** 或 **隧道/connector 不可用** 一致；summary 里 **不见** `bridge_status{200,202,429}` 多为 **首包 `status=0`**（k6 超时），旧脚本未导出 `status:0` 桶。 |
| `dataplane_only` 抬 RPS 后 **有效 200/s 进入平台期**（约 250–370 req/s），再拉高主要增加 **失败 / dropped / p95≈30s** | **端到端同步路径饱和**（不是单点 auth/policy）。 |
| **202 / queue_poll 为 0**（在 `ConstantRps=500`、`SAG_BRIDGE_SOFT_INFLIGHT=128` 等条件下） | 多为 **`bridge_sync_inflight` 未达到 soft**，并非 Poll 脚本失效；Little：`并发 ≈ RPS × 平均转发耗时`。 |
| **连跑多档 RPS** 后下一档近乎全失败 | **会话级叠加**（连接池、内核、Redis、VU）；应 **档间冷却 + 单档归档**。 |
| policy_only @800 eval/s 仍健康 | **sag-policy** 通常不是数据面瓶颈。 |

---

## 2. 组件级：问题落在哪段代码 / 配置

### 2.1 `http-tunnel-bridge`（`proxy/http-tunnel-bridge/src/main.rs`）

- **同步并发计数 `sync_inflight`**：仅在进入同步分支前 `fetch_add`，在 `SyncInflightGuard` drop 时减少。**`sync_inflight ≥ SAG_BRIDGE_SOFT_INFLIGHT`** 且 **Redis 队列启用** 时 **`enqueue` → HTTP 202**。
- **`SAG_BRIDGE_MAX_TUNNEL_INFLIGHT`（信号量）**：限制并发 unary `Forward`；HTTP 路径 **try** 拿不到许可且 Redis 可用时 **同样入队 202**（指标 `bridge_tunnel_shed_to_queue_total`）；队列 worker **阻塞**等许可。`0` 关闭该门控。
- **若 `SAG_BRIDGE_REDIS_URL` 未生效或 Redis 入队失败**：走 `bridge_soft_fallback_total{reason="redis_enqueue"}`；无队列且隧道满载则 **503**。
- **设计权衡**：soft 阈值基于「bridge 内同步 forward 数量」，若单次链路耗时短，500 RPS 下并发可能 **长期低于 128**，则 **202 路径不触发是预期行为**。

### 2.2 `stealth-tunnel-agent`（`proxy/agents/stealth-tunnel-agent/src/grpc_server.rs`）

- **`forward` RPC**：鉴权/策略后 **`send_request_to_connector` + `oneshot` 等待**；超时 `SAG_FORWARD_TIMEOUT_MS`。
- **`ConnectorRegistry`**：单 connector 流上多请求 **多路复用**，但 **connector 侧处理慢** 会堆积 pending。

### 2.3 `sag-connector`（`proxy/connectors/sag-connector/src/main.rs`）

- **有界 accept 队列 + `FuturesUnordered` 限并发**（`SAG_CONNECTOR_MAX_INFLIGHT`）；满则 **503**（`connector_forward_reject_total`）。
- **分段指标**：`connector_forward_accept_wait_seconds`、`connector_forward_upstream_seconds`、`connector_forward_out_send_seconds` —— 用于区分 **排队 / APISIX / 回写 gRPC 流**。
- **`handle_forward`**：`reqwest` + 可选 **响应体截断**（`SAG_CONNECTOR_MAX_RESPONSE_BODY_BYTES`）。

### 2.4 Zentinel（`proxy/zentinel-proxy/config/dataplane-compose.kdl`）

- **HTTPS :10080 → `http-tunnel-bridge:9000`**；路由级 **timeout-secs 90**。尾延迟与 **bridge/agent/connector** 一致时，瓶颈不在「Zentinel 未转发」，而在 **下游耗时与并发**。

---

## 3. 优化计划（按优先级）

### P0 — 测准再改（避免误判）

1. 使用 **`scripts/ops/run-dataplane-sweep.ps1`**：多档 RPS、**`-CooldownSeconds`**，产出 `k6-sweep-*-dp-<rps>.json`。
2. 压测同时在 Edge：`snapshot-bridge-metrics.ps1 -BridgeBaseUrl http://<edge>:9000`，核对 **`bridge_sync_inflight`、`bridge_soft_gate_entered_total`、`bridge_queue_202_total`、`bridge_soft_fallback_total`**。
3. 在 Intra：脚本 **`scripts/ops/snapshot-connector-metrics.ps1`**（在 Intra 本机默认 `http://127.0.0.1:19090/connector/metrics`；**在 Windows 发起机请换 `-MetricsUrl http://<INTRA_IP>:19090/...` 或先 `ssh -L 19090:127.0.0.1:19090 user@intra`**）或 shell：`curl -sS ... | grep connector_forward_`，看 **accept_wait vs upstream vs out_send**。对照实验：**`quick-check-intra-dataplane.sh`** / **`.ps1`**：mock 用 **Python urllib**；APISIX 直连须 **`Host` + `x-sag-app-id`**（与 `services/control-plane-admin/src/apisix.rs` 中 route `vars` 一致，默认 **app-001**），再走 **curl → wget → 侧车**。
4. **若要强制验证 202 全链路**：临时将 Edge **`SAG_BRIDGE_SOFT_INFLIGHT`** 降为 **32**（或更低），并保持 **`-PollDataplane202`**；测完恢复。

### P1 — 吞吐与尾延迟（配置为主）

| 项 | 动作 |
|----|------|
| 超时阶梯 | 保持 README 约定：`SAG_CONNECTOR_HTTP_TIMEOUT_MS` < `SAG_FORWARD_TIMEOUT_MS` ≤ `SAG_BRIDGE_FORWARD_TIMEOUT_MS` ≤ `SAG_GRPC_RPC_TIMEOUT_MS`；Zentinel/k6 **≥ 90s**。 |
| bridge 卸压 | 在确认 Redis 健康前提下，按负载调 **`SAG_BRIDGE_SOFT_INFLIGHT`**、**`SAG_BRIDGE_WORKER_CONCURRENCY`**；依赖 **k6 Poll** 完成异步请求。 |
| connector | 调 **`SAG_CONNECTOR_MAX_INFLIGHT`** / **`SAG_CONNECTOR_ACCEPT_QUEUE`**；上游慢时优先 **APISIX / mock workload** 容量。 |

### P2 — 代码级（ evidence 驱动）

| 条件 | 方向 |
|------|------|
| **`connector_forward_out_send_seconds` ≫ upstream** | agent **stream 写路径** 或 **单流背压**；评估 agent **`grpc_server` 中 outbound channel** 与 connector **`out_tx.send` 是否需分流或更大 buffer。 |
| **`bridge_soft_fallback_total{redis_enqueue}` 高** | Redis 网络、**DB 索引**、**stream 长度**；bridge **重试策略**（现有 warn + 回退同步）。 |
| **长期无 202 且同步路径排队严重** | 除降 soft 外，可考虑 **基于队列深度的准入**（补充或替代纯 inflight 计数）——需改 bridge，属架构增量。 |

### P3 — 压测客户端

- 高 RPS + 长迭代：增大 **MaxVUs** 或降低目标 RPS，避免 **`Insufficient VUs` / `dropped_iterations`** 掩盖服务端指标。
- k6 **exit 99**：阈值未通过，非脚本崩溃；CI 需单独判定。

---

## 4. 700 RPS（`dataplane_get` ≥90%）基线与验收

**基线（P0）**：单档 **700** 固定记录：`iterations` rate、`dropped_iterations`、`http_req_failed`、`sag_api_success_rate{api:dataplane_get}`；Edge `curl :9000/metrics` 关注 `bridge_queue_202_total`、`bridge_tunnel_try_saturated_total`、`bridge_tunnel_shed_to_queue_total`、`bridge_sync_inflight`；Intra connector `connector_forward_accept_wait_seconds` / `connector_forward_reject_total`。

**成功口径**：k6 开启 **`-PollDataplane202`** 时，以 **poll 完成后等价 HTTP 200** 为成功（见 `load-dataplane-k6.js`）。

**压测侧**：高 RPS 需足够 **MaxVUs**（`run-dataplane-sweep.ps1` 默认已抬高），否则 `dropped_iterations` 会压低有效到达率。

**部署侧（edge/intra）**：`SAG_BRIDGE_SOFT_INFLIGHT` 下调、`SAG_BRIDGE_MAX_TUNNEL_INFLIGHT` 限制并发 unary Forward、隧道瞬时满载时 **优先 Redis 202**；agent/connector **stream buffer** 与 connector **max_inflight** 与 compose 默认值对齐（见 `docker-compose.edge.yml` / `docker-compose.intra.yml`）。**Intra 默认已将 `SAG_CONNECTOR_MAX_INFLIGHT` 提至 4096、`SAG_CONNECTOR_ACCEPT_QUEUE` 至 8192**；`sag-connector` 内 `reqwest` **pool_max_idle_per_host** 与高压单上游对齐为 **2048**。

**关于「残留」与 HTTP 403**：数据面 **403** 在 agent 侧主要来自 **策略 DENY** 或 **身份/角色缺失**（见 `grpc_server.rs` `agent_forward_http_403_total`），**不是**「gRPC 隧道未关闭」的典型症状；connector **长连接**为正常形态。压测前若要清空 **进程内** 策略缓存与合并状态，可在 agent 上设 `SAG_AGENT_DEBUG_ADMIN=1` 后执行 `curl -X POST http://127.0.0.1:19104/debug/clear-ephemeral-caches`（勿对公网暴露）。**Redis 队列积压**需运维侧对 `SAG_BRIDGE_REDIS_URL` 所用 DB 单独处理，不在该接口范围内。

---

## 5. 脚本索引

| 脚本 | 用途 |
|------|------|
| `scripts/ops/run-load-dataplane.ps1` | 单次 k6，`-PollDataplane202` 等 |
| `scripts/ops/run-dataplane-sweep.ps1` | 多档 RPS + 档间冷却（默认 MaxVUs 抬高） |
| `scripts/ops/run-dataplane-tiered-700-900.ps1` | 700→（达标）900 tiered；报告含 bottleneck 指标摘要 |
| `scripts/ops/snapshot-bridge-metrics.ps1` | 抓取 bridge Prometheus 子集 |
| `scripts/ops/snapshot-connector-metrics.ps1` | Intra：connector 指标子集（默认 `:19090/connector/metrics`） |
| `scripts/ops/snapshot-mock-metrics.ps1` | Intra：mock-workload 指标子集（默认 `:19090/mock/metrics`） |
| `scripts/ops/quick-check-intra-dataplane.ps1` | Intra：docker 内直连 mock + APISIX（绕过隧道，Windows） |
| `scripts/ops/quick-check-intra-dataplane.sh` | 同上（Bash） |

---

*与代码审查同步；若压测环境 commit 与仓库不一致，请先以运行镜像为准核对环境变量。*
