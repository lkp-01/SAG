# 高并发可靠性：实施路线图（P0–P3 可执行清单）

对应主计划 [high-concurrency-reliability-master-plan.md §7](high-concurrency-reliability-master-plan.md#7-建议实施顺序路线图)。按阶段勾选；每阶段结束保留 **k6 JSON + metrics 片段**（见 [docs-maintenance-runbook.md](docs-maintenance-runbook.md)）。

---

## P0 — 可观测与归因（先做）

**目标**：分清 **mock/APISIX** vs **隧道/bridge** vs **k6 客户端超时**。

| # | 动作 | 文档 / 命令 |
|---|------|-------------|
| 1 | 压测时间窗对齐 Edge/Intra 日志 | [tunnel-loadtest-correlation.md](tunnel-loadtest-correlation.md) |
| 2 | 看 k6 `sag_dataplane_failure_cause_total` | `upstream_5xx` / `timeout` / `network` |
| 3 | 看 `sag_dataplane_http_first_status_total{status:0}` | 若高 → [timeout-deadline-runbook.md](timeout-deadline-runbook.md) |
| 4 | bridge `curl :9000/metrics` | `bridge_grpc_channel_forward_err_total`、`bridge_queue_*` |
| 5 | connector `:9103/metrics` | `connector_forward_total`、`accept_queue_full` |
| 6 | 归档本阶段 k6 summary | `scripts/ops/archive-k6-baseline.ps1`（可选） |

**出口标准**：能口头回答「本批 5xx 主要来自 mock 还是隧道」。

---

## P1 — 容量与超时基线

**目标**：Intra 上游与 Edge 隧道 **不先饱和**；超时链自洽。

| # | 动作 | 文档 |
|---|------|------|
| 1 | sysctl + compose B/C/D | [tunnel-capacity-bootstrap.md](tunnel-capacity-bootstrap.md) |
| 2 | 超时链自检 | `bash scripts/ops/verify-timeout-chain.sh` |
| 3 | Intra APISIX + mock 扩展评估 | [intra-mock-apisix-horizontal.md](intra-mock-apisix-horizontal.md) |
| 4 | Edge 多 gRPC Channel | `SAG_BRIDGE_GRPC_CHANNEL_POOL_SIZE`、[bridge-grpc-channel-pool-future.md](bridge-grpc-channel-pool-future.md) |
| 5 | 背压 / 202 / poll | [backpressure-queue-runbook.md](backpressure-queue-runbook.md) + `-PollDataplane202` |
| 6 | 限流 / connector 队列 | [rate-limit-circuit-breaker-runbook.md](rate-limit-circuit-breaker-runbook.md) |

**出口标准**：同口径 k6 下 `dataplane_get` 成功率可解释，且 `grpc_rpc >= bridge_forward`。

---

## P2 — Edge 水平扩展

**目标**：多 bridge 分担 HTTP accept；观测 **单 agent** 极限。

| # | 动作 | 文档 |
|---|------|------|
| 1 | `docker compose ... --scale http-tunnel-bridge=N` | [horizontal-scale-edge-bridge.md](horizontal-scale-edge-bridge.md) |
| 2 | 可选 `docker-compose.hscale-edge.yml` + 双 upstream kdl | 同上 |
| 3 | 压测对比单副本 vs 多副本 | 保存两份 k6 JSON，注明 git commit |
| 4 | 看 agent CPU、`bridge_grpc_channel_forward_*` | agent 仍为单点注册表 |

**出口标准**：多 bridge 提升 **HTTP 层** 吞吐，且 agent 未先于 bridge 打满。

---

## P3 — 多 connector 分片 / 大改

**目标**：突破 **单 connector 流 / 单 agent 进程内注册表**。

| # | 动作 | 说明 |
|---|------|------|
| 1 | 多个 `SAG_CONNECTOR_ID` + 相同逻辑 `SAG_CONNECTOR_ENDPOINT` | **已做**：同 endpoint generation-bound session 池 |
| 2 | 每副本独立 mTLS 证书 | **已做**：`SAG_CONNECTOR_CERT_BINDINGS` 支持同 endpoint 多指纹 |
| 3 | Agent 心跳租约与主动摘除 | **已做**：默认 10s，1s reaper，按 generation 清 pending |
| 4 | 多 Agent | Connector 使用 `SAG_TUNNEL_ENDPOINTS` 显式连接每个 Agent；session 状态保持进程本地 |

**出口标准**：分片后单 endpoint 负载下降，且无双注册错乱。

---

## 与域手册对照

| 域 | 手册 |
|----|------|
| §1 水平扩展 | [horizontal-scale-edge-bridge.md](horizontal-scale-edge-bridge.md)、[intra-mock-apisix-horizontal.md](intra-mock-apisix-horizontal.md) |
| §2 背压 | [backpressure-queue-runbook.md](backpressure-queue-runbook.md) |
| §3 限流熔断 | [rate-limit-circuit-breaker-runbook.md](rate-limit-circuit-breaker-runbook.md) |
| §4 超时 | [timeout-deadline-runbook.md](timeout-deadline-runbook.md) |
| §5 缓存 | [cache-read-runbook.md](cache-read-runbook.md) |
| §6 异步 | [async-patterns-runbook.md](async-patterns-runbook.md) |
