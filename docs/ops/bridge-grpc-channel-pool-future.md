# bridge → agent 多 gRPC Channel 池（设计说明与实现对照）

## 实现状态（已合并）

`http-tunnel-bridge` 已实现进程内 **N 条** 到 `stealth-tunnel-agent:50051` 的 **mTLS Channel**：

- 环境变量 **`SAG_BRIDGE_GRPC_CHANNEL_POOL_SIZE`**（默认 **`1`**，范围 **1–32**），与 `docker-compose.edge.yml` / `docker-compose.hscale-edge.yml` 对齐。
- **选路**：每个逻辑请求轮询选择一个槽位并只执行一次 Unary Forward。若该次因 `Unavailable` 失败，Bridge 异步重连同一槽位供后续请求使用，不重试当前请求。
- **指标**：`bridge_grpc_channel_forward_total{channel="0"…}`、`bridge_grpc_channel_forward_err_total{channel=…}`（标签为槽位索引字符串）。

代码入口：[http-tunnel-bridge/src/main.rs](../../proxy/http-tunnel-bridge/src/main.rs)（`TunnelClientPool`、`forward_request_inner`）。

## 原始目标（保留作架构备忘）

- 减轻 **单 HTTP/2 连接** 上的流控与队头阻塞。  
- **connector 对每个显式 Agent 地址各维持一条双向流**（Register/Heartbeat/Request/Response/Cancel）；Bridge 多 Channel 只拆分到其配置 Agent endpoint 的 **Unary Forward**，不改变 Connector 隧道协议。

## 风险（仍适用）

- **TLS session / 连接数** 上升；需与 **nofile**、**防火墙并发** 对齐。  
- 总 gRPC 连接数 ≈ **`bridge 副本数` × `SAG_BRIDGE_GRPC_CHANNEL_POOL_SIZE`**（多 bridge 见 [horizontal-scale-edge-bridge.md](horizontal-scale-edge-bridge.md)）。

## 相关

- [high-concurrency-reliability-master-plan.md](high-concurrency-reliability-master-plan.md) §1  
- [horizontal-scale-edge-bridge.md](horizontal-scale-edge-bridge.md)
