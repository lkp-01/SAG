# 异步化与有界队列：运维手册（操作版）

对应主计划 [high-concurrency-reliability-master-plan.md §6](high-concurrency-reliability-master-plan.md#6-异步化)。原则：**热路径不阻塞**；异步必须有 **有界队列** 与 **可观测丢弃/延迟**。

**相关**：[backpressure-queue-runbook.md](backpressure-queue-runbook.md)（bridge 202）、[rate-limit-circuit-breaker-runbook.md](rate-limit-circuit-breaker-runbook.md)（connector 队列）。

---

## 1. 已实现形态总览

```text
客户端/k6 ──► Zentinel ──► bridge
                              ├─ 同步 forward（sync_inflight）
                              └─ soft/隧道满 ──► Redis Stream ──► worker ──► gRPC forward
                                    └──► HTTP 202 + poll /__sag/queue/{id}/status

bridge metrics_mw ──► tokio::spawn ──► audit_logs / fault_events（有界由存储实现决定）

connector 隧道读 ──► try_send accept_queue ──► dispatcher (max_inflight) ──► reqwest → APISIX

connector forwarding ──► Prometheus hop metrics
agent/bridge forwarding ──► Edge-side audit_logs / fault_events
```

---

## 2. Bridge：202 + Redis Stream（数据面异步）

| 项 | 说明 |
|----|------|
| 启用 | `SAG_BRIDGE_REDIS_URL` 非空 |
| 消费 | `SAG_BRIDGE_WORKER_CONCURRENCY` × `XREADGROUP` |
| 客户端 | **必须** `-PollDataplane202 -AcceptDataplane202`（`run-load-dataplane.ps1`） |

细排见 [backpressure-queue-runbook.md](backpressure-queue-runbook.md)。

**指标**：`bridge_queue_202_total`、`bridge_queue_enqueue_total`、`bridge_worker_forward_total`、`bridge_queue_depth`。

---

## 3. Bridge：审计 / 故障写入（非阻塞 HTTP）

`metrics_mw` 在响应后 **`tokio::spawn`** 写 `AuditLogsStore` / `FaultEventsStore`（见 `http-tunnel-bridge/src/main.rs`）。

| 关注点 | 操作 |
|--------|------|
| 压测后审计延迟 | 查存储后端（PG/SQLite）负载，非同步阻塞 dataplane |
| 5xx / 高延迟留痕 | `latency_ms >= 1200` 或 status≥500 写 fault |

**无** 单独 `bridge_audit_dropped_total`；若存储慢，表现为 spawn 任务堆积与 DB 压力（需 DB 侧监控）。

---

## 4. Connector：dispatcher + hop 指标

| 变量 | compose 默认 | 行为 |
|------|--------------|------|
| `SAG_CONNECTOR_MAX_INFLIGHT` | 4096 | 同时出向 HTTP 数 |
| `SAG_CONNECTOR_ACCEPT_QUEUE` | 8192 | 隧道线程 `try_send` 有界队列 |

**满队列**：accept 满 → **503** + `connector_forward_reject_total{reason="accept_queue_full"}`。  

Connector 不直接写数据库；Agent/bridge 在 Edge 侧持久化审计与故障记录。

**指标**：

```bash
docker compose -f docker-compose.intra.yml exec sag-connector \
  sh -c 'curl -sS http://127.0.0.1:9103/metrics' | grep -E 'connector_forward_(reject|total|duration|upstream|accept_wait|out_send)'
```

---

## 5. Agent / policy：后台任务

- **agent**：`grpc_server` 内 policy 评估、degrade Redis 刷新等使用 **`tokio::spawn`**；隧道主循环仍为 async。  
- **policy**：评估缓存与 HTTP 调用在 async 上下文；无单独「数据面队列」。

---

## 6. 未默认落地（演进方向）

| 项 | 状态 |
|----|------|
| 控制面大表变更 **Outbox + MQ** | **未做**（主计划 §6.2） |
| connector 再拆独立 HTTP worker 池 | **已有** dispatcher 结构；扩 `MAX_INFLIGHT` 即可，见 §4 |

---

## 7. k6 / 压测口径

- **202 不算完成**：必须 poll（[tunnel-capacity-bootstrap.md](tunnel-capacity-bootstrap.md)）。  
- **队列 worker 延迟**：对照 `bridge_worker_latency_seconds` 与 `connector_forward_accept_wait_seconds`。  
- **勿** 用「仅 202 率」衡量吞吐而不 poll。

---

## 8. 保守调参顺序

1. 先确认 **202 + poll** 与 Redis 健康（背压 runbook）。  
2. 再调 **`SAG_BRIDGE_WORKER_CONCURRENCY`**（drain 速度）。  
3. connector **`ACCEPT_QUEUE` / `MAX_INFLIGHT`** 仅在 `accept_queue_full` 升高时调整。  
4. 控制面 Outbox **不在本手册范围**。

---

## 9. 回滚

- 禁用 bridge 队列：清空 `SAG_BRIDGE_REDIS_URL` → recreate bridge。  
- 降 worker / 队列：恢复 compose env → recreate `http-tunnel-bridge` / `sag-connector`。

---

## 10. 代码锚点

- `proxy/http-tunnel-bridge/src/queue.rs`：`worker_loop`、`XREADGROUP`。  
- `proxy/http-tunnel-bridge/src/main.rs`：`metrics_mw` 内 `tokio::spawn` 审计。  
- `proxy/connectors/sag-connector/src/main.rs`：`job_tx` 与 Connector forwarding/latency Prometheus 指标。
