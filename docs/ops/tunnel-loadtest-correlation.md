# 压测窗口：隧道与 k6 指标对照（运维执行清单）

用于 **Intra connector ↔ Edge stealth-tunnel-agent（50051）** 不稳定时的排查；与 [DUAL_HOST_OPERATIONS.md](../DUAL_HOST_OPERATIONS.md) 节 1c 互补。承载力调参的一揽子步骤见 [tunnel-capacity-bootstrap.md](tunnel-capacity-bootstrap.md)。水平扩展、背压等中长期方案见 [high-concurrency-reliability-master-plan.md](high-concurrency-reliability-master-plan.md)；Edge 多 bridge 操作见 [horizontal-scale-edge-bridge.md](horizontal-scale-edge-bridge.md)。

## 1. 时间对齐（必做）

压测机（k6）与 **Edge / Intra** 统一用 **UTC** 或同时标注 **本地时区**，避免日志差 8 小时对不上。

- k6 报告 JSON：看文件修改时间或 k6 控制台起止时间。
- **不要用** `docker logs --tail 50` 单独判断「压测中是否掉线」——会混入历史行。改用：

**Intra（connector）**

```bash
docker logs --since "2026-05-13T09:20:00Z" sag-connector 2>&1 \
  | grep -iE 'tunnel dropped|h2 protocol|transport error' | tail -100
```

**Edge（agent / bridge）**

```bash
docker logs --since "2026-05-13T09:20:00Z" sag-stealth-agent 2>&1 | grep -iE 'ERROR|WARN|reset|GOAWAY|tls|stream' | tail -120
docker compose -f docker-compose.edge.yml -f docker-compose.release.edge.yml logs --since "2026-05-13T09:20:00Z" http-tunnel-bridge 2>&1 \
  | grep -iE 'ERROR|accept error|too many open files' | tail -120
```

将上述片段与 k6 **`sag_dataplane_failure_cause_total{cause:upstream_5xx}`** 高峰时段对齐。若 **`cause:timeout`** 或 **`sag_dataplane_http_first_status_total{status:0}`** 为主，先对照 [timeout-deadline-runbook.md](timeout-deadline-runbook.md)（k6 `-RequestTimeout` 是否短于 bridge/Zentinel）。

## 2. k6 summary 字段与现象（对照表）

| k6 指标 | 含义（与隧道相关） |
|---------|-------------------|
| `sag_api_success_rate{api:policy_evaluate}` ≈ 1 | Edge 内 policy 正常；问题不在策略服务本身。 |
| `sag_api_success_rate{api:dataplane_get}` 低 | 数据面（Zentinel→Bridge→Agent→gRPC→Connector→…）失败集中。 |
| `sag_dataplane_bridge_status_total{status:500}` 高 | bridge 将下游失败映射为 5xx；常与 **connector 上游 HTTP** 或 **隧道不可用** 相关。 |
| `sag_dataplane_failure_cause_total{cause:upstream_5xx}` 高 | bridge 归因 **经隧道的上游 HTTP 5xx**；需对齐 **connector / APISIX / mock** 日志。 |
| `sag_dataplane_bridge_status_total{status:202}` ≈ 0 | 未以 Redis 排队为主路径；调 soft_inflight 前先稳定 gRPC。 |
| `sag_api_network_error_total` | k6 客户端连接级失败；与 **connector `transport error`** 时间对齐。 |

## 3. VPN / NAT / 防火墙（记录项）

压测前在运维台账填：

- 路径：Intra → Edge **50051** 是否经 VPN；**MTU**（是否需 `mssfix` / TCPMSS）。
- **空闲连接超时**（防火墙/NAT 对 TCP 无流量断连时间）；与 **gRPC keepalive 间隔** 对比（见 `.env.intra` / compose 中 `SAG_GRPC_KEEPALIVE_MS`）。
- 是否对 **单源并发连接数** 或 **新建连接速率** 有限制。

抽样（压测窗口内执行几次即可）：

```bash
for i in 1 2 3 4 5; do date -u; nc -zv -w 3 <EDGE_IP> 50051; sleep 3; done
```

## 4. ulimit 核对（Edge + Intra）

**Edge**：`docker-compose.edge.yml` 已为 `stealth-tunnel-agent`、`http-tunnel-bridge` 设置 `ulimits.nofile`；容器内执行：

```bash
docker compose -f docker-compose.edge.yml -f docker-compose.release.edge.yml exec http-tunnel-bridge sh -c 'ulimit -n'
docker exec sag-stealth-agent sh -c 'ulimit -n'
```

**Intra**：`docker-compose.intra.yml` 中 `sag-connector` 已配置 `ulimits.nofile`；确认：

```bash
docker exec sag-connector sh -c 'ulimit -n'
```

若容器内仍是 **1024**，检查宿主机 **`/etc/docker/daemon.json`** 的 `default-ulimits` 与 **`systemctl show docker`** 是否限制更低。

## 5. gRPC keepalive A/B（一次只改一类）

在 **Intra** `.env.intra` 或 compose 覆盖中调整（与 Edge `SAG_GRPC_KEEPALIVE_MS` 保持同量级，避免一端极短一端极长）：

- `SAG_GRPC_KEEPALIVE_MS`（默认常 10000）
- `SAG_GRPC_TCP_KEEPALIVE_MS`
- `SAG_CONNECTOR_GRPC_CHANNEL_TIMEOUT_MS`

每次只改一项，用 **同一 k6 命令** 复跑，对比 `connector_tunnel_drop_total{class="..."}`（见 sag-connector Prometheus 指标）与 `dataplane_get` 成功率。

## 6. 队列与缓存（延后）

在 **`sag_dataplane_bridge_status_total{status:202}`** 仍接近 0、且 **tunnel dropped** 仍频繁时，**不要**优先调 `SAG_BRIDGE_SOFT_INFLIGHT` 或做数据面全量缓存；先满足本节 1–5 的 SLO。
