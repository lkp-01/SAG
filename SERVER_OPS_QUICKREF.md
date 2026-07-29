# SAG 各机常用命令速查（生产编译 / 启动 / 冒烟 / 压测）

> **【必读】** 接手请先读 **[`README.md`](README.md)**。所有命令在 **`REPO_ROOT`**（= 含 `.git` 与 `docker-compose.edge.yml` 的目录）执行。  
> **当前生产 Edge**：`172.16.9.107` · **Intra**：`192.168.9.26` · **Windows 压测**：`172.16.9.108`  
> 详细说明见 [`DUAL_HOST_OPERATIONS.md`](DUAL_HOST_OPERATIONS.md)；交接结论见 [`PROJECT_HANDOFF.md`](PROJECT_HANDOFF.md)。

```bash
export REPO_ROOT="${REPO_ROOT:-$HOME/secure_access_gateway_sag}"
cd "$REPO_ROOT"
```

---

## 0. 三台机器一览

| 机器 | IP | SSH 用户（示例） | 目录 |
|------|-----|------------------|------|
| **Edge** | `172.16.9.107` | `lxz` | `~/secure_access_gateway_sag` |
| **Intra** | `192.168.9.26` | 按实际 | 同仓库 clone |
| **Windows 压测** | `172.16.9.108` | — | `D:\lxz\compile\Rust_project\Secure_Access_Gateway_SAG\sag-cloud` |
| **Linux 压测（麒麟）** | 待填 | — | clone 根目录 |

---

## 1. Edge（172.16.9.107）— 【重点】生产模式编译 + 启动

> **【重点】** 压测 / 演示前必须先 **release 编译**，再 `docker-compose.release.edge.yml` 启动；勿长期 `cargo run`（尤其 bridge-2、zentinel）。

### 1.1 首次 / 大版本更新：全量 release 编译 + 启动

```bash
export REPO_ROOT="${REPO_ROOT:-$HOME/secure_access_gateway_sag}"
cd "$REPO_ROOT"

# 若无 .env
test -f .env || cp edge-host.env.example .env

# 1) zentinel release（产物：./proxy/core/target/release/zentinel）
docker compose -f docker-compose.edge.yml run --rm zentinel \
  cargo build --release -p zentinel-proxy --bin zentinel \
  --manifest-path /workspace/proxy/core/Cargo.toml

# 2) 其余 Rust workspace release（首次较慢，约数十分钟）
docker compose -f docker-compose.edge.yml run --rm control-plane-admin \
  cargo build --workspace --release

# 3) 生产覆盖 + 后台全栈（单实例 Auth，无 hscale）
docker compose -f docker-compose.edge.yml -f docker-compose.release.edge.yml up -d --build
```

### 1.2 【推荐】数据面 hscale + release + 绑核（不含 Auth 扩展）

```bash
cd "$REPO_ROOT"

docker compose \
  -f docker-compose.edge.yml \
  -f docker-compose.release.edge.yml \
  -f docker-compose.hscale-edge.yml \
  -f docker-compose.edge.perf.yml \
  -f docker-compose.hscale-edge.perf.yml \
  --env-file scripts/ops/cpuset-edge-28.env \
  up -d --force-recreate
```

### 1.3 全量 hscale（数据面 + Auth 双副本 + nginx LB）— 当前【不推荐】默认启用

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

### 1.4 【重点】Auth hscale 回滚 → 单实例 sag-auth

```bash
cd "$REPO_ROOT"
bash scripts/ops/rollback-auth-hscale-edge.sh
# 期望：login 响应头无 Server: nginx；:8080 由 sag-auth 直接发布
```

### 1.5 自检

```bash
docker compose -f docker-compose.edge.yml -f docker-compose.release.edge.yml ps
curl -sS -o /dev/null -w "control=%{http_code}\n" http://127.0.0.1:8090/health
curl -sS -o /dev/null -w "frontend=%{http_code}\n" http://127.0.0.1:3001/
curl -sS -o /dev/null -w "login=%{http_code}\n" -X POST http://127.0.0.1:8080/api/v1/auth/login \
  -H 'Content-Type: application/json' -d '{"username":"admin","password":"Admin@123"}'
EDGE_IP=172.16.9.107 bash scripts/ops/verify-hscale-edge.sh
```

### 1.6 只重建单个服务

```bash
cd "$REPO_ROOT"

# 控制面
docker compose -f docker-compose.edge.yml -f docker-compose.release.edge.yml \
  up -d --force-recreate control-plane-admin

# Zentinel（改 kdl / release 后）
docker compose -f docker-compose.edge.yml -f docker-compose.release.edge.yml \
  up -d --force-recreate zentinel

# 前端（改代码后须 --build）
docker compose -f docker-compose.edge.yml -f docker-compose.release.edge.yml \
  up -d --build frontend-admin-next

# policy（改 cpuset 后）
docker compose \
  -f docker-compose.edge.yml -f docker-compose.release.edge.yml \
  -f docker-compose.hscale-edge.yml -f docker-compose.hscale-auth.yml \
  -f docker-compose.edge.perf.yml -f docker-compose.hscale-edge.perf.yml \
  -f docker-compose.hscale-auth.perf.yml \
  --env-file scripts/ops/cpuset-edge-28.env \
  up -d --force-recreate sag-policy
```

### 1.7 git pull 与 cpuset 冲突

```bash
cd "$REPO_ROOT"
git checkout -- scripts/ops/cpuset-edge-28.env
git pull origin clean-main --ff-only
grep SAG_EDGE_CPUSET_POLICY scripts/ops/cpuset-edge-28.env   # 期望 4-7
```

### 1.8 放置较久后 down / up（不删卷）

```bash
cd "$REPO_ROOT"
docker compose -f docker-compose.edge.yml -f docker-compose.release.edge.yml down
docker compose -f docker-compose.edge.yml -f docker-compose.release.edge.yml up -d
```

### 1.9 常用诊断

```bash
cd "$REPO_ROOT"
docker compose -f docker-compose.edge.yml -f docker-compose.release.edge.yml ps -a
docker logs --tail 200 sag-zentinel
docker compose -f docker-compose.edge.yml -f docker-compose.release.edge.yml logs --tail 200 http-tunnel-bridge
docker logs --tail 200 sag-stealth-agent

# 绑核一览
grep SAG_EDGE_CPUSET scripts/ops/cpuset-edge-28.env
docker inspect sag-policy --format 'policy cpuset={{.HostConfig.CpusetCpus}}'
docker inspect sag-postgres --format 'postgres cpuset={{.HostConfig.CpusetCpus}}'

# Auth EMFILE（压测中）
docker logs --since 5m secure_access_gateway_sag-sag-auth-1 2>&1 | grep -iE 'emfile|accept error' | tail -20
docker logs --since 5m secure_access_gateway_sag-sag-auth-lb-1 2>&1 | tail -50
```

---

## 2. Intra（192.168.9.26）— 【重点】生产模式编译 + 启动

### 2.1 `.env.intra`（【重点】必须指向当前 Edge）

```bash
export REPO_ROOT="${REPO_ROOT:-$HOME/secure_access_gateway_sag}"
cd "$REPO_ROOT"

cat > .env.intra <<'EOF'
SAG_TUNNEL_ENDPOINT=https://172.16.9.107:50051
SAG_GRPC_TLS_SERVER_NAME=localhost
SAG_GRPC_TLS_CLIENT_CERT=/workspace/infra/tls/client.crt
SAG_GRPC_TLS_CLIENT_KEY=/workspace/infra/tls/client.key
SAG_GRPC_TLS_CA=/workspace/infra/tls/ca.crt
EOF
```

The Connector requires the Edge tunnel endpoint and mTLS material, but no database DSN. Central persistence is owned by Edge services.

### 2.2 release 编译 + 启动

```bash
cd "$REPO_ROOT"

docker compose -f docker-compose.intra.yml run --rm sag-connector \
  cargo build -p sag-connector --release

docker compose -f docker-compose.intra.yml -f docker-compose.release.intra.yml up -d --build
```

### 2.3 绑核 + perf（可选）

```bash
cd "$REPO_ROOT"
docker compose -f docker-compose.intra.yml -f docker-compose.release.intra.yml \
  -f docker-compose.intra.perf.yml \
  --env-file scripts/ops/cpuset-intra-8.env \
  up -d --force-recreate
```

### 2.4 改 Edge IP 后只重建 connector

```bash
cd "$REPO_ROOT"
docker compose -f docker-compose.intra.yml -f docker-compose.release.intra.yml \
  up -d --force-recreate sag-connector
```

### 2.5 mock 路径更新后

```bash
docker compose -f docker-compose.intra.yml -f docker-compose.release.intra.yml \
  up -d --force-recreate mock-workload
```

### 2.6 自检

```bash
docker compose -f docker-compose.intra.yml -f docker-compose.release.intra.yml ps
curl -sS -o /dev/null -w "apisix=%{http_code}\n" http://127.0.0.1:9080/
docker logs --tail 100 sag-connector
```

---

## 3. Windows 压测机（172.16.9.108）

> **【重点】** 适合 **数据面** 压测；**Auth 门禁请用 Linux §4**，Windows 高 QPS 易临时端口耗尽。

### 3.1 前置（管理员，高 QPS 建议）

```powershell
netsh int ipv4 set dynamicport tcp start=10000 num=55535
netsh int ipv4 show dynamicport tcp
k6 version
```

### 3.2 双机冒烟

```powershell
cd D:\lxz\compile\Rust_project\Secure_Access_Gateway_SAG\sag-cloud
git pull origin clean-main
.\scripts\smoke-remote-windows.ps1 -EdgeHost 172.16.9.107 -IntraHost 192.168.9.26 -Rounds 1
```

### 3.3 数据面压测（apisix_routed）

```powershell
cd D:\lxz\compile\Rust_project\Secure_Access_Gateway_SAG\sag-cloud
$env:K6_LOG_OUTPUT = "none"
$ts = Get-Date -Format "yyyyMMdd-HHmmss"
$RPS = 5000   # 3000 / 4000 / 5000 / 6000 / 7000

.\scripts\ops\run-load-dataplane.ps1 `
  -EdgeHost 172.16.9.107 `
  -RunMode dataplane_only `
  -ScenarioType dataplane_only `
  -DataplaneSuccessMode apisix_routed `
  -GateProfile dataplane_routed `
  -ConstantRps $RPS `
  -Stage1Duration 2m -Stage2Duration 2m -Stage3Duration 2m -Stage4Duration 2m `
  -PreAllocatedVUs $RPS -MaxVUs ([int]($RPS * 3)) `
  -NoCapacityVuCap `
  -SummaryJson ".\artifacts\k6-dp-$RPS-routed-$ts.json"
```

### 3.4 Auth @2000（仅参考，非主门禁）

```powershell
$ts = Get-Date -Format "yyyyMMdd-HHmmss"
.\scripts\ops\run-load-dataplane.ps1 `
  -EdgeHost 172.16.9.107 -RunMode capacity -ScenarioType auth_login_verify -GateProfile auth `
  -ConstantRps 2000 -Stage1Duration 2m -Stage2Duration 2m -Stage3Duration 2m -Stage4Duration 2m `
  -PreAllocatedVUs 2000 -MaxVUs 10000 -RequestTimeout 90s -NoCapacityVuCap `
  -SummaryJson ".\artifacts\k6-auth-win-2000-$ts.json"
```

---

## 4. Linux 压测机（麒麟 VM，【推荐】Auth 门禁）

### 4.1 一次性准备

```bash
export EDGE_HOST=172.16.9.107
export REPO_ROOT="${REPO_ROOT:-$HOME/secure_access_gateway_sag}"
cd "$REPO_ROOT"

sysctl -w net.ipv4.ip_local_port_range="10000 65535"

# 安装 k6（apt 或离线包，见 DUAL_HOST_OPERATIONS.md §3e.0）
k6 version
```

### 4.2 Auth @2000 一键

```bash
cd "$REPO_ROOT"
export EDGE_HOST=172.16.9.107
./scripts/ops/run-auth-gate-2000.sh
# 将终端输出的 SAG K6 RESULT PASTE BLOCK 贴回交接
```

### 4.3 数据面 @5000（bash）

```bash
cd "$REPO_ROOT"
TS=$(date +%Y%m%d-%H%M%S)
./scripts/ops/run-load-dataplane.sh \
  --edge-host 172.16.9.107 \
  --run-mode dataplane_only \
  --scenario-type dataplane_only \
  --dataplane-success-mode apisix_routed \
  --gate-profile dataplane_routed \
  --constant-rps 5000 \
  --stage1-duration 2m --stage2-duration 2m --stage3-duration 2m --stage4-duration 2m \
  --pre-allocated-vus 5000 --max-vus 15000 \
  --no-capacity-vu-cap \
  --summary-json "./artifacts/k6-dp-linux-5000-${TS}.json"
```

---

## 5. 全仓库 Git（任意机器）

```bash
cd "$REPO_ROOT"
git checkout clean-main
git pull origin clean-main --ff-only
git rev-parse HEAD
git log -1 --oneline
```

**克隆**：

```bash
git clone git@192.168.14.10:digital-operation/secure_access_gateway_sag.git
cd secure_access_gateway_sag
git checkout clean-main
git submodule update --init --depth 1 proxy/core
```

---

## 6. 相关文档

| 文档 | 内容 |
|------|------|
| [`PROJECT_HANDOFF.md`](PROJECT_HANDOFF.md) | **交接总结（重点结论）** |
| [`DUAL_HOST_OPERATIONS.md`](DUAL_HOST_OPERATIONS.md) | 完整运维手册 |
| [`docs/ops/dataplane-load-3000-7000-report.md`](docs/ops/dataplane-load-3000-7000-report.md) | 数据面压测报告 |
