# 隧道承载力跃迁：B / C / D 一次性执行（单人）

与仓库默认 compose 变更配套：**更小的 gRPC keepalive 间隔**、**更大 stream buffer / tunnel inflight**、**更低 soft_inflight 以提前走 Redis 202**（客户端需 **PollDataplane202 + AcceptDataplane202**）。

## 0. 拉代码

```bash
cd sag-cloud
git pull origin clean-main --ff-only
```

## B — 宿主机（Edge + Intra 各一次）

1. **台账**（纸质或 Wiki）：Intra→Edge 是否经 VPN、防火墙 **TCP idle**、是否限连；**idle ≥ 2 × SAG_GRPC_KEEPALIVE_MS**（默认 **5000ms**，建议 idle ≥ **15s**）。`SAG_GRPC_KEEPALIVE_TIMEOUT_MS` 显式为 **5000ms**，Connector 应每 **2000ms** 心跳，Agent 的 `SAG_TUNNEL_HEALTHY_WINDOW_SEC` 默认为 **10s**。
2. **sysctl**（需 sudo）：

```bash
cd sag-cloud   # 或你的 REPO_ROOT/sag-cloud
sudo bash scripts/ops/apply-tunnel-host-sysctl.sh --dry-run   # 先看
sudo bash scripts/ops/apply-tunnel-host-sysctl.sh
```

3. **VPN**：若存在 MTU 黑洞，按运维规范做 **MSS clamp**（不在此脚本内）。

## C + D — 已由 compose 默认注入（仍需重建容器）

`docker-compose.edge.yml` / `docker-compose.intra.yml` 已调高：

- **Edge agent**：`SAG_AGENT_STREAM_BUFFER=32768`，`SAG_MAX_PENDING_WAITERS=16384`
- **Edge bridge**：`SAG_GRPC_KEEPALIVE_MS` / `SAG_GRPC_TCP_KEEPALIVE_MS` = **5000**；`SAG_BRIDGE_SOFT_INFLIGHT=24`；`SAG_BRIDGE_MAX_TUNNEL_INFLIGHT=2048`；`SAG_BRIDGE_WORKER_CONCURRENCY=64`；`SAG_BRIDGE_QUEUE_MAX_LEN=20000`
- **Intra connector**：`SAG_GRPC_*` = **5000**；`SAG_CONNECTOR_STREAM_BUFFER=32768`

若你本地用 **`.env` / `.env.intra` 覆盖**，请与上表一致或更高，避免 Intra/Edge **keepalive 不一致**。

## 重建与自检

**Edge**

```bash
docker compose -f docker-compose.edge.yml -f docker-compose.release.edge.yml up -d \
  --force-recreate stealth-tunnel-agent http-tunnel-bridge
docker exec sag-redis redis-cli -n 2 PING
docker compose -f docker-compose.edge.yml -f docker-compose.release.edge.yml exec http-tunnel-bridge sh -c 'ulimit -n'
```

**Intra**

```bash
docker compose -f docker-compose.intra.yml -f docker-compose.release.intra.yml up -d --force-recreate sag-connector
bash scripts/ops/verify-tunnel-capacity-after-tune.sh
```

## 冒烟（Windows，双机）

在仓库的 `sag-cloud` 目录下执行（把路径换成你本机 clone 位置）：

```powershell
cd D:\lxz\compile\Rust_project\Secure_Access_Gateway_SAG\sag-cloud
.\scripts\smoke-remote-windows.ps1 -EdgeHost 172.16.9.107 -IntraHost 192.168.9.26 -Rounds 1
```

（按你环境改 IP。）

## 压测（Windows，必须带 poll）

**不要**把文档里的省略号 `...` 粘进命令行；PowerShell 会把它当成 `-EdgeHost` 的值，预检会失败。应显式指定 Edge，例如：

```powershell
cd D:\lxz\compile\Rust_project\Secure_Access_Gateway_SAG\sag-cloud
.\scripts\ops\run-load-dataplane.ps1 -EdgeHost 172.16.9.107 -PollDataplane202 -AcceptDataplane202 -AcceptDataplane429Shed
```

对比 k6 summary：`sag_dataplane_bridge_status_total{status:202}` 应 **> 0**；`dataplane_get` 成功率应较调参前上升。

背压与排队（软/硬门限、Stream、判定树、调参顺序）的细排见 [backpressure-queue-runbook.md](backpressure-queue-runbook.md)。超时阶梯与 k6 对齐见 [timeout-deadline-runbook.md](timeout-deadline-runbook.md)。

进一步水平扩展（多 bridge、Zentinel RR）见 [horizontal-scale-edge-bridge.md](horizontal-scale-edge-bridge.md)。

## 回滚

- compose `git checkout` 旧版或恢复 `.env` 后 **`--force-recreate`** 同上服务。
- 删除 **`/etc/sysctl.d/99-sag-tunnel-capacity.conf`** 后 `sysctl --system`。
