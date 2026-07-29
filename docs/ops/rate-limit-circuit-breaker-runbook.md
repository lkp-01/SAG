# 限流与熔断：运维手册（操作版）

对应主计划 [high-concurrency-reliability-master-plan.md §3](high-concurrency-reliability-master-plan.md#3-限流与熔断)。本文只写 **可执行步骤与判定**；分层原则以主计划为准。

**相关**：bridge **隧道门闸 / 202 / Redis 队列** 见 [backpressure-queue-runbook.md](backpressure-queue-runbook.md)；压测窗口与日志对齐 [tunnel-loadtest-correlation.md](tunnel-loadtest-correlation.md)；Intra APISIX/mock 扩展 [intra-mock-apisix-horizontal.md](intra-mock-apisix-horizontal.md)。

---

## 1. 数据流与责任边界

```text
k6/客户端 → Edge: Zentinel:10080 → http-tunnel-bridge:9000 → Unary gRPC → stealth-tunnel-agent
         → Intra: sag-connector → APISIX:9080 → mock/上游
```

**限流可在多跳叠加**：同一 RPS 下，可能先触达 **APISIX**、再 **connector 有界队列**、再 **bridge 隧道 semaphore** 或 **agent policy 并发**。排查时 **由外到内** 对齐时间窗（见 correlation 文档），避免只调一层。

---

## 2. Intra：`sag-connector` 有界并发（已落地）

### 2.1 行为摘要

- **`SAG_CONNECTOR_MAX_INFLIGHT`**：dispatcher 同时进行的 **出向 HTTP（经 APISIX）** 任务上限。  
- **`SAG_CONNECTOR_ACCEPT_QUEUE`**：隧道线程 **`try_send`** 到 dispatcher 的 **有界队列**；满则立即 **503** 风格响应（`accept_queue_saturated_response`），并打点 **`connector_forward_reject_total{reason="accept_queue_full"}`**。  
- **未设置 `SAG_CONNECTOR_ACCEPT_QUEUE` 时**：代码默认 `max(512, 2 × max_inflight)`（见 `sag-connector/src/main.rs`）。
- Connector 不消费数据库配置；中央持久化由 Edge 服务负责，Connector 保留 forwarding/latency Prometheus 指标。

### 2.2 与 compose 对齐（`docker-compose.intra.yml`）

| 变量 | compose 默认 | 代码内仅未注入 env 时 |
|------|----------------|------------------------|
| `SAG_CONNECTOR_MAX_INFLIGHT` | 4096 | 2048 |
| `SAG_CONNECTOR_ACCEPT_QUEUE` | 8192 | 上式推导 |
| `SAG_METRICS_LISTEN_ADDR` | `0.0.0.0:9103` | 同左 |

### 2.3 指标（容器内拉取）

在 **Intra 宿主机**（`$REPO_ROOT/sag-cloud`）：

```bash
docker compose -f docker-compose.intra.yml -f docker-compose.release.intra.yml exec sag-connector \
  sh -c 'curl -sS "http://127.0.0.1:9103/metrics"' | grep -E '^connector_forward_(reject_total|total)|^connector_forward_accept_wait_seconds|^connector_tunnel_up'
```

| 指标 | 用途 |
|------|------|
| `connector_forward_reject_total{reason="accept_queue_full",connector="..."}` | **accept 队列满**；限流/背压在 connector 入口触发 |
| `connector_forward_accept_wait_seconds` | 入队后等到开始 forward 的等待；变长说明 **max_inflight 打满或上游慢** |
| `connector_forward_total` / `connector_forward_duration_seconds` | 按 HTTP 状态分桶；与 **APISIX/mock 5xx** 对照 |
| `connector_tunnel_up` | 隧道是否注册成功 |

---

## 3. Edge：`stealth-tunnel-agent` policy/auth 并发（已落地）

限制的是 **调用 policy / auth HTTP 评估** 的并发（Semaphore），**不是**整条隧道字节吞吐；与主计划 §3.1 第 5 条一致。

### 3.1 环境变量（`docker-compose.edge.yml`）

| 变量 | compose 默认 | `config.rs` 未注入 env 时 |
|------|----------------|---------------------------|
| `SAG_POLICY_INFLIGHT_LIMIT` | 2048 | 1024 |
| `SAG_AUTH_INFLIGHT_LIMIT` | 2048 | 1024 |

### 3.2 指标（宿主机已映射 `9104:9104` 时）

```bash
curl -sS "http://127.0.0.1:9104/metrics" | grep -E '^agent_policy_|^agent_auth_|^agent_forward_|^agent_degrade_'
```

择要：`agent_policy_eval_total`、`agent_policy_eval_duration_seconds`、`agent_forward_policy_unavailable_total` 等；与 **policy/sag-auth 日志** 同一时间窗对齐。

---

## 4. Edge：Zentinel 当前 kdl 能力

默认数据面配置：[proxy/zentinel-proxy/config/dataplane-compose.kdl](../../proxy/zentinel-proxy/config/dataplane-compose.kdl)。

| 能力 | 现状 |
|------|------|
| 路由级 **超时** | `routes` → `policies { timeout-secs 90 }`（示例） |
| **请求体 / 头** 上限 | `limits { max-header-size-bytes … max-body-size-bytes … }` |
| **RPS / 连接数** 类 limit-req | **本仓库 kdl 未给默认可复制示例**；若需要，请查 Zentinel 产品文档或 **前置 L4/L7 LB** 限连 |

metrics：`listener "metrics"` 内网 `9090`（多副本时见 [horizontal-scale-edge-bridge.md](horizontal-scale-edge-bridge.md)）。

---

## 5. Intra：APISIX（运维侧 checklist）

仓库形态与扩展步骤见 [intra-mock-apisix-horizontal.md](intra-mock-apisix-horizontal.md)（**未**默认提交双 APISIX 容器）。运维 checklist：

1. 在 **脆弱上游 / mock** 路由上考虑 **`limit-req` / `limit-conn`**，避免 connector worker 在慢上游上无限堆积。  
2. **缩短** `proxy` 上游超时、**重试次数 0 或小**（与主计划 §3.1 一致）。  
3. 变更后按 [tunnel-loadtest-correlation.md](tunnel-loadtest-correlation.md) 对齐 **APISIX access.log** 与 k6 `upstream_5xx`。

---

## 6. Edge：`http-tunnel-bridge`（隧道、队列、HTTP 限流、简易熔断）

- **隧道 Unary 并发 / 202 / Redis**：见 [backpressure-queue-runbook.md](backpressure-queue-runbook.md)。  
- **按 `x-sag-app-id` 的 HTTP 令牌桶**（`SAG_BRIDGE_HTTP_RPS_PER_APP`）：`>0` 时在 **读完请求头、读 body 之前** 按 app_id 限流；拒绝为 **HTTP 429** JSON，`bridge_http_app_ratelimit_reject_total`。默认 **0** 关闭。  
- **Unary Forward 全局简易熔断**（`SAG_BRIDGE_FORWARD_CB_FAILURE_THRESHOLD` + `SAG_BRIDGE_FORWARD_CB_COOL_OFF_MS`）：当连续多个逻辑请求各自唯一的 gRPC attempt 失败后，在 cool-off 窗口内 **`forward_request_inner` 快速失败**（`BridgeForwardError::CircuitOpen`），HTTP 为 **503** JSON，`bridge_forward_circuit_open_total` / `bridge_forward_circuit_reject_total` / `bridge_forward_circuit_reject_http_total`。阈值 **0** 关闭。`Unavailable` 只异步重连槽位供后续请求使用；不会重试当前请求。  
- **per-IP** HTTP 限流：仍未实现（若需真实客户端 IP，需 `Forwarded` / `X-Forwarded-For` 信任链或前置 LB）。

### 6.1 指标片段（bridge `:9000/metrics`）

```bash
curl -sS "http://127.0.0.1:9000/metrics" | grep -E '^bridge_(http_app_ratelimit_reject_total|forward_circuit_)'
```

---

## 7. 熔断 vs 当前单次尝试

| 路径 | 主计划期望 | 当前仓库（如实） |
|------|------------|------------------|
| **APISIX → mock** | 连续失败 **短路**（503/固定 JSON） | 由 **APISIX 插件/路由配置** 实现；非 compose 默认 |
| **bridge → agent** | 可演进为按错误类型的断路器 | **`forward_request_inner`**：每个逻辑请求只有一次 attempt；可选 **全局连续失败熔断**（见 §6），失败槽位仅为后续请求重连；**不是**按错误类型的半开状态机 |

---

## 8. 重试风暴（客户端与服务间）

- **k6 / 数据面**：必须带 **Poll + Accept 202**（见 [tunnel-capacity-bootstrap.md](tunnel-capacity-bootstrap.md)）；避免对 202 **无限轮询**。  
- **gRPC**：**非幂等 POST** 默认不在中间层盲目重试；幂等读可在业务约定下有限重试。

---

## 9. 「限流症状」判定树（简版）

1. **k6 `upstream_5xx` 高** 且 **APISIX / mock 日志**先爆 → 优先 **mock 容量与 APISIX 超时/限流**（correlation 文档）。  
2. **`connector_forward_reject_total{reason="accept_queue_full"}` 升** → **accept 队列或 max_inflight**；对照 `connector_forward_accept_wait_seconds`。  
3. **`bridge_queue_reject_total` / `bridge_tunnel_saturated_503_total` / 202** → 见 **背压 runbook**。  
4. **`bridge_http_app_ratelimit_reject_total` 升** → 提高 `SAG_BRIDGE_HTTP_RPS_PER_APP` 或压测降并发。  
5. **`bridge_forward_circuit_reject_*` 与 `bridge_forward_circuit_open_total`** → agent/gRPC 健康与阈值；可调 **`SAG_BRIDGE_FORWARD_CB_FAILURE_THRESHOLD`** / **`SAG_BRIDGE_FORWARD_CB_COOL_OFF_MS`**。  
6. **`agent_policy_*` 异常或超时** → **policy/auth 服务** 与 **`SAG_POLICY_INFLIGHT_LIMIT`**。  

---

## 10. 保守调参顺序（每步留 k6 JSON + metrics 片段）

1. **确认现象分层**（§9），避免把 mock 5xx 误判为 connector 队列满。  
2. **Intra：APISIX** 超时与（可选）`limit-req` / `limit-conn`。  
3. **connector**：仅在确认 **accept_queue_full** 或等待直方图异常时再升 **`SAG_CONNECTOR_ACCEPT_QUEUE`** / **`MAX_INFLIGHT`**（注意单机线程与 APISIX 单点）。  
4. **agent**：在 policy/auth 成为瓶颈时再调 **`SAG_POLICY_INFLIGHT_LIMIT`** / **`SAG_AUTH_INFLIGHT_LIMIT`**（与 Edge CPU、下游 sag-policy 能力匹配）。  
5. **bridge 隧道、HTTP app RPS、Forward 熔断**：与 [backpressure-queue-runbook.md](backpressure-queue-runbook.md) 及本节 §6 衔接（先确认 **429** 来自 `SAG_BRIDGE_HTTP_RPS_PER_APP` 而非 Zentinel）。

---

## 11. 回滚

- 恢复 **`.env` / `.env.intra`** 中相关变量后，对改动服务 **`docker compose ... up -d --force-recreate`**（`sag-connector`、`stealth-tunnel-agent`、`http-tunnel-bridge`、APISIX 等 **按实际改动范围**）。  
- **不**要求对 Redis 或 APISIX etcd 做破坏性清理作为默认回滚步骤。

---

## 12. 代码锚点（只读排查）

- Connector：`proxy/connectors/sag-connector/src/main.rs`（`max_inflight`、`accept_queue_cap`、`try_send`、`connector_forward_reject_total`）。  
- Agent：`proxy/agents/stealth-tunnel-agent/src/config.rs`（`SAG_MAX_PENDING_WAITERS`）、`connector_registry.rs`（`PendingRequest::drop`、generation-aware pending 清理与 best-effort cancel）、`grpc_server.rs`（pending semaphore admission）。  
- Bridge：`proxy/http-tunnel-bridge/src/main.rs`、`limits.rs`（`AppRpsLimiter`、`ForwardCircuit`）。
