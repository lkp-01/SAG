# 请求 deadline、取消与幂等运行手册

## 链路契约

Bridge 在读取请求体之前创建一个绝对 `deadline_unix_ms`。Agent 和 Connector 只能缩短它，不能重新开始计时；进入 semaphore、Redis 队列、Connector accept queue、gRPC 和 HTTP 都消耗同一份剩余预算。

默认超时阶梯为：APISIX `connect 3s / send 5s / read 45s`、Connector HTTP `55s`、Agent fallback `58s`、Bridge `60s`、gRPC Channel `120s`。APISIX 路由和 Bridge 对同一个逻辑请求都不做自动重试。

Bridge 为每次逻辑请求生成 `request_id`，为传输尝试生成独立 `attempt_id`。Agent 的 pending 表只按 `attempt_id` 匹配；迟到响应不会完成另一次请求。

## 写请求契约

`POST`、`PUT`、`PATCH`、`DELETE`、`CONNECT` 等非只读方法必须携带稳定的 `Idempotency-Key`（兼容 `x-idempotency-key`）。`x-request-id` 只用于追踪，不能替代业务幂等键。

Agent 在转发前通过共享存储原子 claim：

- 首次请求获得 claim 后才能发往 Connector。
- 相同 key、不同请求指纹返回 `409 conflict`。
- 相同 key 尚在执行或结果不确定时返回 `409 pending`，不会再次执行。
- 已完成请求直接重放持久化的状态码、响应头和响应体，并返回 `x-sag-idempotency-state: replayed`。
- claim 成功但确认尚未派发时，只有 owner attempt 可以原子释放 claim。

多 Agent 部署必须让 Agent 使用同一个 PostgreSQL；生产 Compose 已显式设置。SQLite 只适用于单 Agent。Gateway 能保证不自动重复派发，但无法回滚已经在业务系统提交的副作用；业务服务仍应使用透传的 `Idempotency-Key` 实现最终的业务幂等。

PostgreSQL 是 Edge 正确性边界，而不是 Connector 依赖：Connector 在 PostgreSQL 不可达时仍可启动并维持隧道；Agent 在首次 claim 失败时对写请求 fail closed，且不会向 Connector 派发。若下游已经返回但 completed 结果无法写回，Agent 返回不可用/超时并保留需要人工核对的 `pending`。按当前实现，Agent 与 Bridge 冷启动都会在 schema 检查失败时退出；运行期审计写失败是 best-effort，不能据此推断幂等账本也可降级。

## 取消语义

Bridge 的 gRPC 请求超时或客户端断开会 drop Agent 的 pending guard。guard 原子移除自己的 attempt、释放 semaphore permit，并向 Connector 发送 `CancelRequest`。Connector 在 accept queue 和 APISIX HTTP 的发送/响应体阶段检查取消；取消 HTTP future 后由 reqwest/hyper 管理连接是否可复用，不会把旧响应错配到新请求。

取消只能回收尚未完成的资源，不能撤销已提交的写操作。Agent crash 后，Bridge 会收到 gRPC 失败且不会自动重试；Connector 看到隧道关闭后取消该隧道的全部在途请求。已留下的 durable `pending` 必须先与业务系统核对，禁止按时间自动偷取 claim。

## 多实例

Connector 可通过 `SAG_TUNNEL_ENDPOINTS=https://agent-1:50051,https://agent-2:50051` 对每个 Agent 建立独立隧道，并把 `SAG_CONNECTOR_MAX_INFLIGHT` 和 accept queue 总容量切分到各隧道。必须填写每个实例的真实地址；单个随机 LB 地址无法保证所有 Agent 都有返回路径。

禁止在实时流量下混跑不兼容版本。新增 protobuf 字段虽然是 additive，运行语义却不是双向兼容：旧 Agent 不携带有效 deadline，新 Connector 会拒绝；旧 Connector 不回传新 `attempt_id`，新 Agent 无法匹配 pending。升级必须进入维护窗口，先停止入口并排空 Redis queue、Bridge synchronous inflight 和 Agent pending，再停止三类进程，整组部署同一发布版本。启动顺序为 Agent → Connector → Bridge；确认所有显式 Agent 隧道、幂等表和指标正常后才恢复入口。Connector、Agent、Bridge 任一项失败时，按同样流程整组回滚，禁止只回滚单个组件。

## 关键指标

- Agent：`agent_pending_waiters`、`agent_late_response_total`、`agent_cancel_total`、`agent_idempotency_total`、`agent_forward_total`。
- Connector：`connector_cancel_total`、`connector_forward_cancelled_total`、`connector_forward_deadline_total`、`connector_forward_body_error_total`、`connector_forward_accept_wait_seconds`。
- Bridge：`bridge_forward_error_total`、`bridge_grpc_channel_forward_err_total`、`bridge_queue_expired_total`、`bridge_request_reject_total`。

超时日志应同时包含 `request_id`、`attempt_id`、`trace_id`、`stage` 和绝对 deadline，以便区分 Bridge、Agent、Connector 和 APISIX 阶段。

## 验证

运行中的双机环境执行：

```powershell
.\scripts\ops\verify-timeout-chain.ps1
```

或：

```bash
bash scripts/ops/verify-timeout-chain.sh
```

脚本在超时阶梯反转、变量缺失、APISIX 未禁重试或 Bridge 恢复旧双尝试逻辑时返回非零状态。
