# SAG 技术细节与实现边界

> 本文是 [PROJECT_ARCHITECTURE.md](PROJECT_ARCHITECTURE.md) 的实现级补充。重点不是宣传能力，而是说明当前代码实际做了什么、依赖什么、何时不生效，以及如何验证。所有“性能结果”均标注为历史压测口径，未被本轮重新复现。

## 1. 事实规则与源码入口

### 1.1 状态标签

- **实现**：当前代码路径可见。
- **条件实现**：只有环境变量、服务依赖或 Compose profile 满足时运行。
- **历史证据**：仓内文档/`artifacts` 的旧结果，不能自动视为本次环境的结果。
- **限制**：代码缺口、单点状态或需要外部补齐的能力。

### 1.2 核心源码地图

| 主题 | 事实来源 |
|---|---|
| gRPC 合约 | [tunnel.proto](../shared/tunnel-proto/proto/tunnel.proto) |
| HTTP→隧道适配、队列、限流、熔断 | [bridge main](../proxy/http-tunnel-bridge/src/main.rs)、[queue](../proxy/http-tunnel-bridge/src/queue.rs)、[limits](../proxy/http-tunnel-bridge/src/limits.rs) |
| 隧道服务、门控、路由同步 | [Agent gRPC](../proxy/agents/stealth-tunnel-agent/src/grpc_server.rs)、[manager](../proxy/agents/stealth-tunnel-agent/src/manager.rs)、[registry](../proxy/agents/stealth-tunnel-agent/src/connector_registry.rs) |
| 内网 Connector | [connector main](../proxy/connectors/sag-connector/src/main.rs) |
| 身份与 SSO | [sag-auth main](../services/sag-auth/src/main.rs)、[foura](../services/sag-auth/src/foura.rs) |
| PDP | [sag-policy main](../services/sag-policy/src/main.rs) |
| 控制面与 APISIX 同步 | [admin main](../services/control-plane-admin/src/main.rs)、[apisix](../services/control-plane-admin/src/apisix.rs) |
| 持久化 | [storage lib](../shared/storage/src/lib.rs)、[store](../shared/storage/src/store.rs) |

## 2. 协议与请求模型

`TunnelService` 有两个 RPC：

| RPC | 调用方 → 服务方 | 用途 |
|---|---|---|
| `CreateTunnel(stream TunnelMessage)` | Connector → Agent | 长连接双向流。Connector 注册、发心跳和响应；Agent 下发 ForwardRequest。 |
| `Forward(ForwardRequest)` | Bridge → Agent | 每个外部 HTTP 请求对应一次 unary RPC。 |

关键消息字段：

- `ConnectorRegister`：`connector_id`、`app_id`、`external_host`、`endpoint`。
- `ConnectorHeartbeat`：`connector_id`、`endpoint`、`unix_ts`。
- `ForwardRequest`：`request_id`、`app_id`、`method`、`path`、`headers`、`body`。
- `ForwardResponse`：相同 `request_id`、HTTP 状态码、响应头和响应体。

`request_id` 在 Bridge 的 HTTP 入口生成，Agent 负责将它作为 pending map 的关联键；不要表述成由 Agent 无条件生成。

## 3. 数据面逐跳行为

### 3.1 Zentinel 与 Bridge

`proxy/zentinel-proxy/config/dataplane-compose.kdl` 配置 HTTPS `:10080` 到 `http-tunnel-bridge:9000` 的路由，并声明 `failure-mode "closed"`。但 Zentinel 本体属于 `proxy/core` 子模块；当前工作区该目录为空，无法在本仓核验其内部安全逻辑。

Bridge 的 fallback `proxy()`：

1. 必须存在 `x-sag-app-id`，否则 HTTP 400。
2. 保留 URI 的 path 和 query。
3. 过滤标准 hop-by-hop 头，以及 `Connection` 中列出的扩展连接级头。
4. 用 `SAG_BRIDGE_MAX_BODY_BYTES` 限制读取体积，超过时 HTTP 413。
5. 生成 UUID `request_id` 后构造 `ForwardRequest`。
6. 正常路径通过 `forward_request_inner()` 调用 Agent；Bridge 用 round-robin 选择 gRPC Channel，首个 RPC 失败时最多重连/重试一次。

Bridge 返回的 Agent/Connector HTTP 响应会保留状态、过滤后的头和 body。它不是应用协议转换层，也不解析业务 JSON。

### 3.2 Bridge 背压、队列与保护开关

下表描述的是实现，不代表所有开关默认启用：

| 机制 | 开启条件 | 行为 |
|---|---|---|
| 每应用限流 | `SAG_BRIDGE_HTTP_RPS_PER_APP > 0` | `x-sag-app-id` 维度 token bucket；拒绝 HTTP 429。 |
| Forward 熔断 | `SAG_BRIDGE_FORWARD_CB_FAILURE_THRESHOLD > 0` | 连续两次 gRPC 尝试均失败才累计；开路期间 HTTP 503 + `Retry-After`。 |
| 隧道并发闸 | `SAG_BRIDGE_MAX_TUNNEL_INFLIGHT > 0` | 同步 HTTP 路径先 `try_acquire`；worker 路径等待 permit。 |
| Redis Stream 队列 | 设置 `SAG_BRIDGE_REDIS_URL` | soft/hard 水位、队列工作者、202+轮询 URL、结果 TTL、去重。 |
| soft 水位 | Redis 队列可用且 `sync_inflight >= soft` | 成功入队时 HTTP 202；队列满时 HTTP 429。 |
| hard 水位 | Redis 队列可用且 `sync_inflight >= hard` | 直接 HTTP 429。 |
| 隧道饱和回退 | 同步路径 `try_acquire` 失败 | 有队列则尝试 202；无队列则 HTTP 503。 |

队列成功处理后 `XACK + XDEL`；解析或 Forward 失败会写入 DLQ。当前没有 `XAUTOCLAIM`/pending reclaim、DLQ 自动重放或人工回放 API，因此消费者异常后的恢复不是完整闭环。Redis 入队失败的策略可设为同步 fallback 或 503，默认是 fallback。

### 3.3 Agent：路由、心跳与请求关联

Agent 的 `TunnelManager` 将路由组织为 `app_id → RouteInfo`，而 `ConnectorRegistry` 维护：

- `connector_endpoint → mpsc Sender<TunnelMessage>`；
- `request_id → oneshot Sender<ForwardResponse>`。

Connector 注册时会覆盖同 endpoint 的现有 Sender。Agent 收到 Connector Response 后按 request id 唤醒对应 oneshot。请求超时、通道关闭或发送失败会删除 pending 项，且 `SAG_MAX_PENDING_WAITERS` 限制同时等待数。

Agent 从控制面拉取路由的默认间隔是 5 秒。每次成功同步会**全量替换**内存路由表；配置多个 endpoint 时按顺序尝试，首个成功者生效。Connector 心跳的默认健康窗口是 120 秒；超窗会让需要健康隧道的路由拒绝请求。

### 3.4 Connector：并发与内网转发

Connector 启动后：

1. 按默认或环境配置建立到 Agent 的 gRPC TLS/mTLS Channel。
2. 发送 `Register`，默认每 2 秒发送 Heartbeat。
3. 接收到 ForwardRequest 后使用 `try_send` 写入有界 accept queue；满时立即返回 HTTP 语义 503 和 `Retry-After: 1`。
4. dispatcher 以 `FuturesUnordered` 将在途请求限制在 `SAG_CONNECTOR_MAX_INFLIGHT`。
5. `handle_forward` 将路径、可选 `x-sag-query` 和非连接级头发送到 `SAG_APISIX_BASE_URL`。
6. 上游响应过大时可按 `SAG_CONNECTOR_MAX_RESPONSE_BODY_BYTES` 截断并返回 502。

`SAG_APISIX_BASE_URL` 对 Connector 是必填项；当前代码不再提供空配置时的 echo 数据面。Connector 断线重连是**线性递增、封顶并带确定性抖动**的退避，不是指数退避。

## 4. 请求级身份与 PDP

### 4.1 策略模型

策略记录包含：

```text
id, effect(ALLOW|DENY), subjects, app_id?, path_prefix?, priority
```

主体匹配支持 `*`、`role:<role>`、`user:<id>`；资源匹配支持精确/通配 `app_id` 和 path prefix。策略按 priority 降序 first-match，没有匹配项时默认 `DENY`。策略更新递增 `policy_version`，缓存 key 含该版本以避免旧决策复用。

`sag-policy` 有 Moka 决策缓存，可选 Redis 二级缓存。缓存开启、TTL 和容量均由环境变量决定。缓存不改变策略的 first-match 语义。

### 4.2 Agent PEP 的实际条件

Agent 只有在 `SAG_POLICY_EVALUATE_ENDPOINT` 存在时才调用 `authorize_forward_or_deny_response()`；该 endpoint 未配置时，它直接继续隧道转发。因此不得把 PDP 门控描述成当前二进制的无条件行为。

在 PDP 启用时，Agent：

1. 解析用户 ID 和角色。
2. 缺少 user 或 role 时返回 403。
3. 对同键评估使用 Moka 缓存和 coalescing，避免缓存失效时集中请求 policy 服务。
4. 以信号量限制 Policy/Auth 外调并发，以 timeout 限制单次调用。
5. 允许的决策可写入 stale-ALLOW Redis；Policy 暂时不可用且存在 stale-ALLOW 时可短期继续放行，否则返回 503。
6. 明确命中策略的 DENY 才进入负缓存；瞬态错误不会作为正常拒绝长期缓存。

### 4.3 当前必须保留的安全风险

`SAG_AUTH_VERIFY_ENDPOINT` 配置后，代码意图是不信任调用方身份头而调用 `/verify`；但如果请求**没有** `Authorization`，`resolve_user_identity()` 仍会返回 `x-sag-user-id` / `x-sag-user-roles`。这些值随后可能进入 PDP。

因此当前实现不能宣称“配置 Auth 后身份头绝不可信”。部署前应修正为：认证 endpoint 已配置而缺少 Bearer Token 时直接拒绝，除非显式启用一个仅限受信代理网络的 identity-header 模式。

## 5. 认证、JWT、4A 与 OIDC

### 5.1 密码登录

`sag-auth` 启动时从存储加载用户；首次可创建 bootstrap 管理员。密码用 Argon2 校验，成功后签发 JWT。`SAG_ALLOW_PASSWORD_LOGIN=false` 可关闭密码入口。

可选 login memo 缓存的 key 综合 JWT secret、用户名、提交密码和当前密码哈希；命中可跳过 Argon2 与 JWT 重新编码。它能改善热点重复登录的 CPU 开销，但不会自动解决客户端临时端口、连接数、负载均衡或数据库瓶颈。

### 5.2 JWT 与管理授权

- `POST /api/v1/auth/verify` 解码 token、检查过期并返回 active/user。
- 用户管理、身份源和映射管理要求 Bearer JWT 且具有 `admin`、`boss` 或 `ops` 角色。
- `sag-policy` 与 `control-plane-admin` 的管理接口要求 `admin` 或 `boss`。

生产环境必须显式提供高熵 JWT secret、关闭/限制 bootstrap 入口，并保护 Auth/Policy/Admin 的直连端口。仓库 Compose 中的开发默认配置不能复制到生产。

### 5.3 4A / OIDC 授权码流程

两类 provider 都走浏览器授权码模式：

1. `/api/v1/auth/sso/login` 创建一次性、10 分钟 TTL 的 state，写入内存或 Redis。
2. 重定向到 4A 或 OIDC authorize endpoint。
3. `/api/v1/auth/sso/callback` 用 `take()` 消费 state，换取 token/userinfo。
4. 4A 从员工号取得角色；OIDC 从 token/userinfo 提取 groups，再通过组角色映射得出本地角色。
5. 签发 SAG JWT；配置门户 URL 时将 token 放入重定向 query 的 `sso_token`。

最后一点适合演示一键登录，但 query token 可能进入浏览器历史、代理日志和 Referer。生产环境应改为短期一次性 code 或受保护的 Cookie/session 交接。

仓内 `infra/fake-4a` 仅是授权码流程模拟器，不能证明与真实客户 4A 的协议、字段和安全策略兼容。

## 6. 控制面、APISIX 与应用模型

### 6.1 管理资源

| 资源 | 控制面能力 |
|---|---|
| 应用 | 应用 CRUD、树状视图、分钟级指标查询。 |
| API 路由 | CRUD；空 id 会根据 app/method/path 生成确定性 id。 |
| 隧道路由 | `host → app_id + connector_endpoint + require_healthy_tunnel`。 |
| 内网上游 | `app_id → upstream + scheme`，是 APISIX 下发的前提。 |
| 审计/故障 | 查询审计日志、故障事件和公开只读安全视图。 |
| 故障演示 | 管理端可修改故障注入 toggle；需要对应环境开关才会生效。 |

### 6.2 APISIX 推送语义

APISIX 推送只有在 `SAG_APISIX_ADMIN_BASE_URL` 与 `SAG_APISIX_ADMIN_API_KEY` 同时存在时启用。写入隧道路由、内网上游或 API route 时会调用 `try_sync_app()`；控制面启动后也可定期 reconcile。

单应用下发规则：

- route id 派生自 `app_id`；
- 用 `http_x_sag_app_id == app_id` 隔离应用；
- 路径统一匹配 `/*`；
- `proxy-rewrite` 兼容部分 `/api/<name>` 演示路径；
- 上游来自 `intranet_upstreams`。

这是 best-effort 同步：缺上游、Admin API 不可达或请求失败时只记录日志，原管理请求仍可成功。因此运维必须同时检查数据库记录、APISIX route、reconcile 日志和冒烟请求。

## 7. 存储、缓存、审计与指标

### 7.1 存储后端

`SAG_STORAGE_BACKEND` 选择 SQLite 或 PostgreSQL。PostgreSQL DSN 在日志中会脱敏；迁移工具位于 `services/control-plane-admin/src/bin/migrate_sqlite_to_postgres.rs`。主存储模型包括用户、身份、策略、路由、应用、API route、审计、故障和分钟聚合指标。

### 7.2 审计并非统一实现

| 位置 | 写入模式 | 影响 |
|---|---|---|
| Connector | 有界 mpsc audit queue | 队列满会丢弃并计数，优先保护数据面。 |
| Agent、Admin、public-edge 等 | 多处 `tokio::spawn` | 异步，但不是统一有界背压。 |
| `sag-policy` middleware | 直接 `await` 存储写入 | 可能把数据库延迟带入请求路径。 |
| Bridge | middleware 记录请求/故障 | 行为取决于当前存储和异步路径。 |

此外，`POST /api/v1/audit/logs` 的 handler 本身未做管理 JWT 校验。若该接口暴露给不受信网络，任何调用方可尝试注入审计记录；应通过网络隔离、Agent token 或服务间认证保护，并在代码层补上授权。

### 7.3 可观测性

主要指标类型：

- HTTP 服务：`http_requests_total`、`http_request_duration_seconds`；
- Agent：`agent_forward_total`、`agent_policy_eval_*`、缓存与 stale 降级计数；
- Bridge：`bridge_sync_inflight`、队列 202/拒绝/回退、gRPC channel 错误、限流与熔断；
- Connector：隧道 up/reconnect、接收队列等待、上游耗时、回写耗时和 audit dropped；
- 控制面：route cache、应用指标聚合和审计/故障查询。

Compose 编排 Prometheus、Grafana、OTel Collector、Node Exporter 及 Intra metrics gateway；后者由 Nginx 汇聚 Connector、APISIX 与 Mock 的指标供跨机抓取。仓内没有完整告警规则、日志集中化和 trace 跨服务关联的已验证闭环。

## 8. HTTP API 索引

| 服务 | 主要入口 |
|---|---|
| `sag-auth` | `/health`、`/metrics`、`/api/v1/auth/login`、`/verify`、`/users`、`/identity/providers`、`/identity/mappings`；SSO 配置完成后增加 `/auth/sso/login`、`/callback`。 |
| `sag-policy` | `/health`、`/metrics`、`/api/v1/policies`、`/api/v1/policy/evaluate`、`/api/v1/identity/map-roles`。 |
| `control-plane-admin` | `/health`、`/metrics`、`/api/v1/apps`、`/api-routes`、`/agent/routes`、`/agent/intranet-upstreams`、`/audit/logs`、`/fault-events`、`/fault-injection`、`/apps/tree`、`/apps/metrics`，以及 `/api/public/security/*`。 |
| Bridge | `/metrics`、`/__sag/queue/:id/status`，其余路径进入数据面 fallback。 |
| Agent | gRPC `TunnelService`；可选 debug admin `POST /debug/clear-ephemeral-caches`。 |
| Connector | 独立 Prometheus listener，默认 `:9103/metrics`。 |
| public-edge | `/metrics` 与 catch-all HTTP proxy。 |

管理 API 和公开只读 API 的认证规则不同；前端 rewrite 仅是便利层，不是服务间认证边界。

### 8.1 前端管理能力与代理

`frontend-admin-next` 通过 `/api-control`、`/api-auth`、`/api-policy`、`/api-bridge`、`/api-zentinel`、`/api-prom` 和 `/api-grafana` rewrite 对接后端。它包含应用/API route 管理、OpenAPI 解析后批量创建 API route、身份源与映射、审计、故障、安全、工作流和观测页面；这些页面多数是管理/演示视图，不表示后端已经提供完整 OpenAPI 托管、API 生命周期或多租户治理产品。

`frontend` 与 `frontend-portal` 仍分别保留 Vite proxy 配置。它们使用浏览器 HTTP 调用服务，不直接参与 Agent/Connector gRPC 隧道，也不构成后端访问控制的可信边界。

## 9. 配置与超时

常用配置按职责分组：

| 领域 | 代表变量 |
|---|---|
| 存储 | `SAG_STORAGE_BACKEND`、`SAG_STORAGE_DB_PATH`、`SAG_POSTGRES_DSN` |
| Agent 路由 | `SAG_CONTROL_PLANE_SYNC_ENDPOINT(S)`、`SAG_AGENT_SYNC_TOKEN`、`SAG_TUNNEL_HEALTHY_WINDOW_SEC` |
| 身份与 PDP | `SAG_AUTH_VERIFY_ENDPOINT`、`SAG_POLICY_EVALUATE_ENDPOINT`、对应 timeout 与 inflight limit、`SAG_TRUST_IDENTITY_HEADERS` |
| gRPC TLS | `SAG_GRPC_MTLS_ENABLED`、证书/私钥/CA/SNI 变量 |
| Bridge | `SAG_BRIDGE_REDIS_URL`、soft/hard in-flight、queue 限额、gRPC Channel pool、body limit、限流与熔断变量 |
| Connector | `SAG_TUNNEL_ENDPOINT`、`SAG_APISIX_BASE_URL`、heartbeat、accept queue、max inflight、HTTP timeout |
| Auth/SSO | JWT、密码登录、4A/OIDC、state/session/login memo 相关变量 |
| APISIX | Admin URL、Admin key、reconcile 开关与间隔 |

双机 Edge Compose 推荐的超时阶梯是 Connector HTTP 55s < Agent Forward 58s < Bridge Forward 60s ≤ gRPC 120s，Zentinel 路由约 90s。根 `docker-compose.yml` 的 Bridge Forward 配置与该双机模板不同，使用任何压测/部署结论前都必须读取实际生效的 Compose、env 文件和容器 env，不能只背一个默认数值。

## 10. 历史压测：可说与不可说

仓内历史数据面报告采用 `dataplane_only + apisix_routed`：只测 Zentinel → Bridge×2 → Agent → Connector → APISIX → Mock，**不含 Auth/Policy**。`apisix_routed` 的定义是收到 HTTP 200–599；上游 5xx 也计算为路由成功。

因此以下说法可以成立：

> 历史双机压测中，5000 目标 iter/s 档约 94% 请求完成了路由并收到 HTTP 响应；6000+ 出现明显 dropped iterations 和波动。

以下说法不能成立：

> 系统在 5000 QPS 下有 94% 的业务成功率，或已可对生产承诺 5000 QPS。

历史 Auth `auth_login_verify @ 2000` 指的是 2000 目标 login+verify iteration/s，不是“2000 并发登录”。Windows 压测结果受端口耗尽、VUs、Nginx/CPU 等变量影响；当前文档把 Argon2 视为待验证的候选瓶颈之一，而非已经完成的单一根因。

## 11. 当前验证状态

本次文档重建期间只完成以下可复现检查：

- `frontend`、`frontend-admin-next`、`frontend-portal` 的 TypeScript typecheck 通过；
- Rust `cargo test --workspace` 在本机因缺少 MSVC `link.exe` 被环境阻断，未形成当前代码的测试结果；
- 未重新执行真实多服务启动、真实 4A/OIDC、APISIX 到业务上游的 E2E、Clippy、前端 Lint/Build 或性能压测；
- 仓内可见 Rust 单元测试仅 10 个，未见前端 E2E 或 CI 配置。

“曾经编译/压测过”与“当前工作区已经验证”必须分开陈述。

## 12. 运维与工程限制清单

1. `proxy/core` 是 Git 子模块，当前工作区为空；Zentinel 二进制不可独立构建。
2. 单 Agent 的注册表和 pending map 是内存状态，不能直接横向扩为多 Agent。
3. 多 Connector 需要不同 endpoint 和控制面明确路由分片，不是同一 endpoint 的透明无限副本。
4. Redis 队列有 DLQ，但缺 pending reclaim 与自动重放。
5. public-edge 是简单 HTTP proxy，不是已实现的 TLS/WAF/限流产品。
6. 开发 Compose 包含源码挂载、开发启动、测试服务和必须生产覆盖的配置；release overlay 也依赖外部构建条件。
7. 管理/审计入口、Prometheus、APISIX Admin、数据库和调试端口都需要网络隔离与最小权限，不应只依赖“内网”假设。
8. 证书、私钥、DSN、JWT secret、4A/OIDC secret 和 Redis 连接信息不得进入演示文档、日志、截图或提交历史。

## 13. 推荐验证顺序

1. 补齐并初始化 `proxy/core` 子模块，确认 Zentinel 可构建或使用已批准二进制。
2. 用实际 Compose/env 运行 `docker compose config`，检查端口、密钥引用、TLS/SNI 和依赖拓扑。
3. 先做单服务健康与 API 授权验证，再跑 Bridge、Agent、Connector、APISIX 的冒烟链路。
4. 专门验证：缺 Bearer 时的身份头、PDP endpoint 缺失、APISIX 同步失败、Redis 不可用、Connector 断线、队列 pending/DLQ。
5. 重新定义业务成功断言后，再运行数据面和 Auth 压测；保存配置、指标和原始结果作为可追溯证据。
