# SAG Server Deployment README

这份文档给执行部署的同事使用，目标是用一套命令在服务器上拉起 SAG 后端与中间件（含 zentinel）。

## 1. 前置条件

- 已安装 `Docker` + `Docker Compose`（`docker compose version` 可用）
- 服务器可访问镜像仓库
- 服务器开放（或内网可访问）端口：
  - `8090`（control-plane-admin）
  - `8080`（sag-auth）
  - `8081`（sag-policy）
  - `9000`（http-tunnel-bridge）
  - `10080`（zentinel HTTPS ingress）
  - `9091`（Prometheus UI，宿主机映射）
  - `3000`（Grafana UI，宿主机映射）
  - `9080` / `9180`（APISIX data/admin）
  - `28080`（可选：`company-demo-sites`，门户图标跳转用的演示静态页）
  - `5432`（PostgreSQL，可按需仅内网）

说明：`stealth-tunnel-agent` 需设置 `SAG_POLICY_EVALUATE_ENDPOINT`（Compose 已默认指向 `http://sag-policy:8081/api/v1/policy/evaluate`），否则隧道层不会对请求做策略裁决。

Edge 独占机部署：`docker-compose.edge.yml` 会读取仓库根目录的 `.env`。可复制入库的示例 **`edge-host.env.example`** 为 `.env`（`cp edge-host.env.example .env`），再补上 **`SAG_APISIX_ADMIN_API_KEY`** 等机密；勿把含真实密钥的 `.env` 提交到 Git。

## 2. 拉起服务（默认包含 zentinel）

在 `sag-cloud` 根目录执行（首次建议先 build，以便 Rust 服务容器内具备 `protoc/cmake` 等编译依赖）：

```bash
docker compose build
docker compose up -d
```

说明：当前编排中 `zentinel` 已是默认服务，不需要 `--profile`。

`zentinel` 启动说明（重点）：

- 当前主编排已采用 `--manifest-path /workspace/proxy/core/Cargo.toml` 的启动方式，避免历史上在特定目录触发工具链同步阻塞。
- 首次冷机部署仍可能出现较长编译时间（Rust 依赖编译），属于预期现象；建议首次部署预留 3-10 分钟观察窗口。
- 启动慢不等于配置滞后：配置更新仍由 compose env / control-plane 路由 / KDL 配置决定。

若代码变更后长时间未重启（尤其修改了 Rust 服务/zentinel 配置/prometheus 配置），建议执行一次“全量刷新”：

```bash
docker compose down
docker compose build control-plane-admin sag-auth sag-policy stealth-tunnel-agent sag-connector http-tunnel-bridge zentinel
docker compose up -d postgres etcd apisix mock-workload control-plane-admin sag-auth sag-policy stealth-tunnel-agent sag-connector http-tunnel-bridge zentinel otel-collector prometheus grafana frontend-admin-next frontend-portal company-demo-sites
```

## 3. 状态检查

```bash
docker compose ps
docker compose logs -f control-plane-admin sag-auth sag-policy stealth-tunnel-agent sag-connector http-tunnel-bridge zentinel
```

观测面检查：

```bash
curl http://127.0.0.1:9091/-/ready
curl http://127.0.0.1:9091/api/v1/targets
```

说明：`prometheus.yml` 已包含 `zentinel-proxy` 抓取（`zentinel:9090/metrics`，容器内网络访问），并抓取 `bridge/agent/connector` 与管理面指标。

健康检查（任意可访问节点执行）：

```bash
curl http://127.0.0.1:8090/health
curl http://127.0.0.1:8080/health
curl http://127.0.0.1:8081/health
```

关键链路检查（建议作为上线验收）：

```bash
curl -i http://127.0.0.1:9000/api/test
curl -i http://127.0.0.1:3001/api-zentinel/api/test
```

预期：两条都返回 `200`。若 `N1=500` 且 `T1=200`，优先排查 zentinel TLS/SNI/证书链。

## 4. 冒烟验证

Windows（PowerShell）：

```powershell
.\scripts\smoke-dataplane.ps1
```

WSL/Linux：

```bash
bash ./scripts/smoke-dataplane-wsl.sh
```

预期：`M1/M2/M3/N1/T1/S1/S2` 全部 `PASS`。

## 5. 常见问题

- `no tunnel route for app_id`
  - 执行 `.\scripts\seed-demo-tunnel-route.ps1`
  - 或检查 `GET /api/v1/agent/routes?app_id=app-001` 是否有数据
- `N1/T1` 502 但 `S1/S2` 正常
  - 多为路由同步或 connector 注册问题，执行：
    - `.\scripts\ops\diag-sync-routes.ps1`
- APISIX 401（Admin API）
  - 检查 `SAG_APISIX_ADMIN_API_KEY` 与 `infra/apisix/config.yaml` 中 key 一致
- 双机下 `http-tunnel-bridge` 返回 `connector tunnel is unhealthy`
  - 检查 `control-plane-admin` 中该 `app_id` 的 `connector_endpoint` 是否与实际 connector ID 一致（如 `connector-intra-001:stream`）
  - 检查 intra 侧 `SAG_TUNNEL_ENDPOINT` 与 `SAG_GRPC_TLS_SERVER_NAME` 是否正确
- 双机下 `no connector stream` / `transport error`
  - 常见于 connector 到 edge agent 断连（endpoint DNS 不可达、SNI/证书不匹配、agent 停止）
  - 先恢复 `SAG_TUNNEL_ENDPOINT` 与 `SAG_GRPC_TLS_SERVER_NAME=localhost`，再重启 `stealth-tunnel-agent` 和 `sag-connector`
- zentinel 容器启动后 `:10080` 不可用且日志提示 TLS 证书找不到
  - 确认 `proxy/zentinel-proxy/config/dataplane-compose.kdl` 使用绝对证书路径：
    - `/workspace/proxy/core/tests/fixtures/tls/server-default.crt`
    - `/workspace/proxy/core/tests/fixtures/tls/server-default.key`
- 新服务器首次部署后出现 TLS 握手失败（无法即时取日志）
  - 先做离线预检（无需启动服务）：
    - `openssl x509 -in <cert> -noout -dates -ext subjectAltName`
    - `openssl pkey -in <key> -pubout | sha256sum`
    - `openssl x509 -in <cert> -pubkey -noout | sha256sum`
  - 确保：
    - SAN 与访问域名/SNI 一致（`SAG_GRPC_TLS_SERVER_NAME`、`ZENTINEL_PROXY_TARGET`）
    - 证书和私钥哈希匹配
    - CA 已注入客户端（Node: `NODE_EXTRA_CA_CERTS`）
  - 再执行 `docker compose up -d zentinel frontend-admin-next` 和 `N1/T1` 探针，避免“盲启后再排错”。

## 6. 停止与清理

停止：

```bash
docker compose down
```

停止并清理卷（会删除 Postgres 持久化数据）：

```bash
docker compose down -v
```

## 7. 前端控制台与门户（当前推荐）

本仓库当前主用前端是：

- 管理端（Next.js）：`frontend-admin-next`（默认 `http://127.0.0.1:3001`）
- 用户门户（Vite）：`frontend-portal`（默认 `http://127.0.0.1:5174`）

### 7.1 Docker Compose 启动（推荐）

在 `sag-cloud` 根目录：

```bash
docker compose up -d frontend-admin-next frontend-portal
```

访问地址：

- Adminplane（Next）：`http://127.0.0.1:3001`
- 兼容控制面板页：`http://127.0.0.1:3001/control`
- 用户门户：`http://127.0.0.1:5174`

说明：`frontend-portal` 中“进入管理端”按钮默认跳转 `http://127.0.0.1:3001`（`boss/ops/admin` 可见）。

### 7.2 本地手工启动（仅调试）

管理端（Next）：

```bash
cd frontend-admin-next
npm install
npm run dev -- --hostname 0.0.0.0 --port 3001
```

用户门户（Vite）：

```bash
cd frontend-portal
npm install
npm run dev -- --host 0.0.0.0 --port 5174
```

## 8. 4A 联调（Fake 4A）

当甲方暂未开放 4A 接口，可用仓库内 Fake 4A 做完整浏览器链路演示（授权码模式）：

```bash
docker compose up -d fake-4a sag-auth frontend-portal
```

入口（每次切换用户都从这里重新进）：

- `http://127.0.0.1:8080/api/v1/auth/sso/login`

说明：

- 若 compose 已配置 `SAG_SSO_PORTAL_REDIRECT_URL`，认证成功后会自动跳转门户并完成一键登录（通过 `sso_token`）。
- Fake 4A 页面包含 “未认证访客（预期被拦截）” 入口，用于演示缺少身份时的拒绝（通常为 403）。
- 如果你在浏览器回退后复用旧页面，可能看到 `invalid or expired state`，这是 `state` 一次性 + 时效机制导致的预期安全行为。
