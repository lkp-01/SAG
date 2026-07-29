# Edge：`sag-auth` 水平扩展（nginx LB + 双副本）

## 背景

压测表明 **login**（`:8080`）在全链路/高 QPS 下先于 verify 成为瓶颈；verify 仅为 JWT 解码。`sag-auth` **无隧道长状态**，适合多副本 + **共享 Redis login memo**（`SAG_SESSION_REDIS_URL`）。

## 与 bridge hscale 的关系

| 项 | bridge hscale | auth hscale |
|----|---------------|-------------|
| override 文件 | `docker-compose.hscale-edge.yml` | `docker-compose.hscale-auth.yml` |
| 宿主机入口 | Zentinel **:10080** | **:8080**（`sag-auth-lb`） |
| 第二副本 | `http-tunnel-bridge-2` | `sag-auth-2` |
| release | `release.edge.yml`（`sag-auth`）+ `hscale-auth.yml` 内 **`sag-auth-2` release 命令** | 须已 `cargo build --release -p sag-auth` |

可与既有 **bridge-2 + zentinel hscale kdl + cpuset-edge-28** 同一套 `docker compose` 命令叠加。

## 一键启动（Edge，`REPO_ROOT`）

先完成 **release 编译**（§6 `DUAL_HOST_OPERATIONS.md`），再：

```bash
cd "$REPO_ROOT"

docker compose \
  -f docker-compose.edge.yml \
  -f docker-compose.release.edge.yml \
  -f docker-compose.hscale-edge.yml \
  -f docker-compose.hscale-auth.yml \
  -f docker-compose.edge.perf.yml \
  -f docker-compose.hscale-edge.perf.yml \
  -f docker-compose.hscale-auth.perf.yml \
  --env-file scripts/ops/cpuset-edge-28.env \
  up -d --force-recreate
```

## 仅追加 Auth 扩展（已跑 bridge hscale 时）

```bash
docker compose \
  -f docker-compose.edge.yml \
  -f docker-compose.release.edge.yml \
  -f docker-compose.hscale-edge.yml \
  -f docker-compose.hscale-auth.yml \
  -f docker-compose.edge.perf.yml \
  -f docker-compose.hscale-edge.perf.yml \
  -f docker-compose.hscale-auth.perf.yml \
  --env-file scripts/ops/cpuset-edge-28.env \
  up -d --force-recreate sag-auth sag-auth-2 sag-auth-lb stealth-tunnel-agent frontend-admin-next
```

`frontend-admin-next` 需重建以拾取 `AUTH_PROXY_TARGET=http://sag-auth-lb:8080`（override 已写）。

## 回滚（恢复单实例 `sag-auth`）

去掉 `-f docker-compose.hscale-auth.yml` 等 auth override，并：

```bash
docker compose -f docker-compose.edge.yml -f docker-compose.release.edge.yml \
  -f docker-compose.hscale-edge.yml \
  -f docker-compose.edge.perf.yml -f docker-compose.hscale-edge.perf.yml \
  --env-file scripts/ops/cpuset-edge-28.env \
  up -d --force-recreate sag-auth
```

确认 **`sag-auth` 再次发布 `8080:8080`**（无 LB 容器占用）。

## 验证

```bash
EDGE_IP=172.16.9.107 bash scripts/ops/verify-hscale-edge.sh
```

期望：`auth login via :8080 LB => 200`；`sag_auth_login_memo_*` 在 `:9104/metrics` 可见。

## 压测（仅 login+verify）

Windows：

```powershell
.\scripts\ops\run-load-dataplane.ps1 -EdgeHost 172.16.9.107 `
  -ScenarioType auth_login_verify -ConstantRps 2000 -NoCapacityVuCap `
  -PreAllocatedVUs 2000 -MaxVUs 10000 -SkipPrecheck `
  -SummaryJson ".\artifacts\k6-auth-2000.json"
```

## 注意

- 各副本 **`SAG_JWT_SECRET` 必须相同**（compose 默认一致）。  
- **Postgres 连接池**：副本数 × 每进程连接 ≤ DB `max_connections`。  
- **login memo** 减 CPU，**不减** TCP `connectex`；仍须 **release 构建** + 客户端 **少打 login**（长 TTL token）。  
- **不要用 MQ** 削同步 login 峰；见主计划与 `cache-read-runbook.md`。

## 压测若成功率极低：查 EMFILE

nginx access 大量 **499** + auth 日志 **`accept error: Too many open files (os error 24)`** → 为 **`ulimit -n` 过小**（非业务逻辑错误）。  
`docker-compose.hscale-auth.perf.yml` 已为 `sag-auth` / `sag-auth-2` / `sag-auth-lb` 设置 **`nofile=1048576`**；改后须 **`--force-recreate`** 上述三服务。压测后 `curl` 单发 login 仍可能 200（进程从 EMFILE 恢复）。
