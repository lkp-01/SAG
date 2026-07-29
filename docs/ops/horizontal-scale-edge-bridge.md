# Edge：http-tunnel-bridge 水平扩展（Zentinel 双 upstream）

## Stream epoch 协调升级

Connector、Agent、Bridge 和 `sag-tunnel-proto` 是一个协议发布单元。引入
epoch 协议前必须停止入口、等待 Bridge/Agent 进入 unready 并排空已接受请求，
随后全量替换四个参与方，最后恢复入口；不支持新旧协议在线混跑。

Connector 为每条 gRPC stream 生成新 UUID epoch，并在收到匹配的
`RegisterAck` 后才 ready。`ForwardAccepted` 是有界接收队列边界。stream
丢失后不得把旧 attempt 迁移到新 Agent 或新 epoch；Bridge 返回
`x-sag-outcome: unknown`，且不得自动重新 dispatch mutation。未决记录必须走
幂等对账，不能把“未收到 ACK”当作“未执行”的证据。

双机运维总览（选组件、副本数、与邻居关系）见 **[DUAL_HOST_OPERATIONS.md](../../DUAL_HOST_OPERATIONS.md) §3b**。

目标：在 **单 `stealth-tunnel-agent`**、**单 connector 流** 不变的前提下，用 **多个 bridge 进程** 分担 Zentinel 前 HTTP/TLS/解析压力；每个 bridge 进程各自建立到 agent 的 gRPC（含可选 **多 Channel 池**，见 `SAG_BRIDGE_GRPC_CHANNEL_POOL_SIZE`）。

## 前置

- 仓库路径下 `sag-cloud/` 执行 compose。
- 默认 **不要** 把本文件与 `docker-compose.hscale-edge.yml` 用于首装冒烟，除非你明确要测多 bridge。

## 方式一：Compose scale（同服务名多副本）

`docker-compose.edge.yml` 中 **已去掉** `http-tunnel-bridge` 的 `container_name`，以便：

```bash
docker compose -f docker-compose.edge.yml -f docker-compose.release.edge.yml up -d --scale http-tunnel-bridge=2
```

注意：

- **主机端口 `9000:9000`**：多副本时仅 **第一个** 任务会绑定宿主机 9000（Docker 行为）；其余副本 **仅集群内** `http-tunnel-bridge:9000` 可解析。
- 客户端应走 **Zentinel `10080`**，不要依赖直连 `localhost:9000` 命中「某一个」副本。
- 日志 / exec：  
  `docker compose logs http-tunnel-bridge`  
  或 `docker compose ps -q http-tunnel-bridge | head -n1 | xargs -I{} docker logs {}`

若 Zentinel 仍使用 **单 target** `http-tunnel-bridge:9000`，嵌入式 DNS 可能对多 A 记录做轮询（取决于解析器实现）。要 **显式** 两条独立 DNS 名，用 **方式二**。

## 方式二：hscale override（`http-tunnel-bridge-2` + 专用 kdl）

1. 启动：

```bash
cd sag-cloud
docker compose -f docker-compose.edge.yml -f docker-compose.release.edge.yml -f docker-compose.hscale-edge.yml up -d
```

2. 该 override 会：

- 启动 **`http-tunnel-bridge-2`**（与主 bridge 同镜像/环境；**无宿主机端口**，仅 `expose: 9000`）。
- 将 **Zentinel** 切到 [`dataplane-compose.hscale.kdl`](../proxy/zentinel-proxy/config/dataplane-compose.hscale.kdl)，upstream 对 `http-tunnel-bridge:9000` 与 `http-tunnel-bridge-2:9000` **round_robin**。

3. 水平切换顺序建议：

1. `up -d` 拉起第二 bridge（及依赖）。  
2. 确认两实例 `curl -s http://127.0.0.1:10080/metrics`（经 Zentinel 的 metrics 路径按你部署）或各自容器内 `:9000/metrics` 中 `bridge_grpc_channel_forward_total` 有增量。  
3. 跑 `scripts/smoke-remote-windows.ps1` 或最小 dataplane k6。  
4. 再考虑缩回单 bridge：去掉 hscale compose 与 kdl，**滚动**重启 Zentinel。

## Redis 队列

多 bridge 共享 **`SAG_BRIDGE_REDIS_URL`**（默认 `redis://redis:6379/2`）时，**未消费的 202 任务** 可由任意 bridge worker 消费；滚动重启 bridge **不会丢队列语义**（仍受 TTL 约束）。

## 风险与边界

- **agent / 单 connector** 仍是全局瓶颈；多 bridge 只缓解 **Edge 入口侧**。
- 调高副本后关注 **agent CPU、nofile、`SAG_BRIDGE_MAX_TUNNEL_INFLIGHT`** 与 **gRPC 连接总数**（每进程 × `SAG_BRIDGE_GRPC_CHANNEL_POOL_SIZE`）。

## CPU 亲和性（cpuset）

- 单 bridge + 数据面热点：`docker-compose.edge.perf.yml` + `scripts/ops/cpuset-dualhost.env.example`（复制为 `cpuset-dualhost.env` 后改核号）。
- **hscale 第二 bridge**：叠加 `docker-compose.hscale-edge.perf.yml`，为 `http-tunnel-bridge-2` 设 `SAG_EDGE_CPUSET_BRIDGE_2`，与 `SAG_EDGE_CPUSET_BRIDGE` **错开**。
- **`--scale http-tunnel-bridge=2`**：两个副本会共用同一 `SAG_EDGE_CPUSET_BRIDGE`；要按进程绑不同 CPU，请用 **方式二（bridge-2 独立服务）** 而非 scale。

## 相关

- [high-concurrency-reliability-master-plan.md](high-concurrency-reliability-master-plan.md) §1  
- [bridge-grpc-channel-pool-future.md](bridge-grpc-channel-pool-future.md)（进程内多 Channel，可与多 bridge 叠加）
