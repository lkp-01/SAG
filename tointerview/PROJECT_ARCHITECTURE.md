# SAG 项目架构

> 适用范围：当前工作区 `Secure_Access_Gateway_SAG-clean-main`。本文以 Rust 源码、Compose、协议和运行脚本为事实来源；历史 README、交接文档和压测产物只用于说明历史结果，不等价于当前复验。

## 1. 项目定位与事实边界

SAG（Secure Access Gateway）是一个面向企业内网应用访问的安全接入原型。它把认证、请求级策略、反向隧道、内网 L7 路由、控制面管理和可观测性组合在一起，目标是让外部调用通过受控链路访问内网应用，而不是直接把应用暴露到公网。

阅读本文时请区分下列状态：

- **已实现**：当前仓库有对应源码和配置。
- **可选**：只有设置相应环境变量或启用特定 Compose 后才生效。
- **模板/演示**：存在部署或演示材料，不代表生产基线。
- **外部依赖**：能力依赖仓库外系统或 Git 子模块，本仓不一定含其实现。
- **限制**：当前实现已知的非高可用、非强一致或未验证边界。

## 2. 系统全景

```text
浏览器、API 调用方
  │
  ├─ 管理面 / 门户
  │    ├─ frontend-admin-next（当前主控制台）
  │    ├─ frontend（旧 Vite 控制台）
  │    └─ frontend-portal（用户门户）
  │             │ HTTP 代理 / 重写
  │             ▼
  │    sag-auth ───── sag-policy ───── control-plane-admin
  │        │                │                    │
  │        └────── shared/storage ───────────────┘
  │                         │
  │                  SQLite 或 PostgreSQL
  │
  └─ 数据面
       [外部 CDN/WAF/公共边缘：仓外，可选]
                    │
                 Zentinel（proxy/core 子模块）
                    │ HTTPS :10080 → Bridge
                    ▼
             http-tunnel-bridge
                    │ unary gRPC Forward（可配置 mTLS）
                    ▼
          stealth-tunnel-agent
             │      │        │
             │      │        └─ sag-auth /verify（可选）
             │      └────────── sag-policy /evaluate（可选）
             ▼
    已注册的 sag-connector 双向 gRPC 流
                    ▼
            APISIX → 内网应用 / Mock workload
```

数据面协议定义在 [shared/tunnel-proto/proto/tunnel.proto](../shared/tunnel-proto/proto/tunnel.proto)。控制面和数据面共享 [shared/storage](../shared/storage/src/lib.rs) 的持久化模型，但数据面 Connector/Agent 的连接注册表在内存中，不是共享集群状态。

## 3. 组件与职责

| 层级 | 组件 | 状态 | 职责与边界 |
|---|---|---|---|
| 管理前端 | `frontend-admin-next` | 已实现 | Next.js 主控制台，覆盖应用、路由、身份源、审计、工作流、监控和安全视图。通过 Next rewrite 访问后端。 |
| 管理前端 | `frontend` | 已实现/旧 | Vite 控制台，仍保留路由、策略、用户、健康和 4A 调试页面。 |
| 用户前端 | `frontend-portal` | 已实现 | Vite 门户，提供登录、服务导航、策略预检和网关探测。 |
| 认证 | `sag-auth` | 已实现 | 密码登录、JWT 签发/验证、用户 CRUD、4A/OIDC 授权码登录、身份源与组角色映射管理。 |
| 策略 | `sag-policy` | 已实现 | 策略 CRUD、PDP evaluate、外部组到本地角色的映射评估。 |
| 控制面 | `control-plane-admin` | 已实现 | 应用、API 路由、隧道路由、内网上游、审计、故障事件、应用指标和故障注入开关。可选地向 APISIX Admin API 推送路由。 |
| 协议适配 | `http-tunnel-bridge` | 已实现 | HTTP 转 unary gRPC、请求体限制、可选按应用限流/熔断、可选 Redis Stream 过载队列和状态轮询。 |
| 隧道协调 | `stealth-tunnel-agent` | 已实现 | 提供 gRPC 服务、同步控制面路由、维护 Connector 注册与心跳、执行可选的身份/PDP 门控、关联请求和响应。 |
| 内网接入 | `sag-connector` | 已实现 | 主动连接 Agent、注册并发回心跳、以受控并发把隧道请求代理到 APISIX。 |
| 内网网关 | APISIX | 外部依赖 | 处理内网 HTTP 路由及上游选择。控制面仅在配齐 Admin 配置和上游映射时尝试下发路由。 |
| 边界入口 | Zentinel | 外部子模块 | `proxy/zentinel-proxy` 只提供 KDL 配置；二进制实现来自 `proxy/core` Git 子模块。当前工作区的 `proxy/core` 没有文件，不能把它当作本仓已包含的实现。 |
| 公共边缘 | `public-edge` | 已实现但未纳入主 Compose | 独立 HTTP 转发 sidecar，记录指标/审计。当前代码不提供 TLS listener、IP 限流或路径阻断；这些是应由 CDN/WAF/stunnel 等补齐的部署职责。 |
| 观测 | Prometheus、Grafana、OTel、Node Exporter | Compose 编排 | 抓取服务指标、展示和转发观测数据；不等价于完整告警与日志平台。 |

## 4. 两条主业务流

### 4.1 北南向数据面

1. Zentinel 的 HTTPS listener 将请求代理给 `http-tunnel-bridge`；其 KDL 配置的路由是 fail-closed。
2. Bridge 必须读取 `x-sag-app-id`，保留完整 `path_and_query`，删除 hop-by-hop HTTP 头，并限制请求体大小。
3. Bridge 创建 `ForwardRequest`，经一个或多个 gRPC Channel 调用 Agent 的 `Forward`。
4. 若配置了 PDP endpoint，Agent 解析身份并请求策略服务；随后按 `app_id` 选择内存路由表中的 Connector endpoint，并检查心跳窗口。
5. Agent 将请求写进该 Connector 已建立的 `CreateTunnel` 双向流，以 `request_id` 等待响应。
6. Connector 的 dispatcher 受并发与接收队列限制，将请求转为 APISIX HTTP 请求。APISIX 再到真实上游或仓内 Mock。
7. `ForwardResponse` 原路返回，Bridge 还原 HTTP 状态、响应头和响应体。

### 4.2 控制面变更

1. 管理前端通过 `control-plane-admin`、`sag-auth` 和 `sag-policy` 的 HTTP API 管理用户、身份源、策略、应用和路由。
2. 隧道路由和内网上游写入 `shared/storage` 管理的 SQLite/PostgreSQL 表。
3. Agent 默认每 5 秒轮询控制面路由 API；可配置多个 endpoint 并按顺序尝试，成功后全量替换本地路由表。
4. 若配置 APISIX Admin 地址和密钥，控制面会在相关写操作后尝试推送 APISIX route，并可周期性 reconcile。该调用是 best-effort：失败被记录为 warning，不会使原管理请求回滚。

## 5. 身份、策略和信任边界

### 5.1 身份与授权职责

| 责任 | 组件 | 当前实现 |
|---|---|---|
| 登录与 JWT | `sag-auth` | Argon2 密码校验、HS256 JWT、可选 Redis login memo。 |
| 外部身份 | `sag-auth` + 4A/OIDC | 授权码模式，`state` 一次性消费；身份源配置可覆盖部分客户端参数。 |
| 策略决策 | `sag-policy` | `subject × app_id × path_prefix × effect × priority`，按 priority 降序 first-match，默认 DENY。 |
| 策略执行 | `stealth-tunnel-agent` | 当 PDP endpoint 配置时在转发前调用 PDP；未配置时该门控不参与链路。 |
| 内网路由 | Agent + Connector + APISIX | Agent 选择 Connector；Connector 统一代理到 APISIX；APISIX 选择内网上游。 |

### 5.2 必须理解的边界

- “每个请求都经过 JWT + PDP”是标准部署意图，不是无条件代码事实：认证与策略 endpoint 都可不配置。
- Connector 主动发起到 Agent 的长连接，能减少业务应用对 Edge 的直接可达需求；但当前 Intra Compose 为演示/运维映射了 APISIX、Mock、etcd 和 metrics 等宿主机端口，不能宣传为“系统绝无入站端口”。
- Agent 的 Connector 注册表、pending 响应表和心跳表都是单进程内存状态。多 Agent 需要共享注册/路由机制或明确分片，当前没有自动多活。
- gRPC mTLS 默认开启但可被环境变量关闭；生产部署必须显式管理证书、SNI、CA 和密钥，不应把开发默认值当成安全保证。

## 6. 数据、缓存与异步状态

`shared/storage` 通过 `StorageStore` enum 支持 SQLite 与 PostgreSQL，并非 trait 插件体系。当前 schema 覆盖：

- `users`、`identity_providers`、`group_role_mappings`；
- `policies`；
- `apps`、`api_routes`、`tunnel_routes`、`intranet_upstreams`；
- `audit_logs`、`fault_events`、`app_metrics_minute`。

Redis 在不同组件承担不同职责：

| 用途 | 组件 | 是否必需 |
|---|---|---|
| Session、OAuth state、login memo | `sag-auth` | 可选，缺失时部分状态回退内存。 |
| PDP 决策缓存 | `sag-policy` | 可选，本地 Moka 仍可用。 |
| stale 身份/策略 ALLOW | Agent | 可选；只有历史缓存存在时才可能降级。 |
| 过载 Redis Stream 队列 | Bridge | 可选；未设置时没有 HTTP 202 排队路径。 |

Bridge 队列含 job/result TTL、去重、poll 最小间隔和 DLQ 写入；但尚未实现 pending 消息自动认领或 DLQ 自动重放。

## 7. 部署形态

| 形态 | 主要文件 | 说明 |
|---|---|---|
| 单机演示 | `docker-compose.yml` | 包含 PostgreSQL、Redis、etcd、APISIX、Mock、核心 Rust 服务、Zentinel、前端和观测组件。大量服务以源码挂载与开发命令启动。 |
| Edge / Intra 双机 | `docker-compose.edge.yml`、`docker-compose.intra.yml` | Edge 放置控制面、Agent、Bridge、Zentinel；Intra 放置 APISIX、Mock、Connector 等。通过 VPN/DNS 与环境变量对齐。 |
| 横向扩展试验 | `docker-compose.hscale-*.yml` | 包含 Bridge 与 Auth 横向扩展覆盖配置；Auth 扩展历史结果并不证明收益。 |
| 性能配置 | `*.perf.yml`、`scripts/ops/*` | 承载 CPU、压测、指标采集和回滚实验。 |
| release 覆盖 | `docker-compose.release*.yml` | 改用 release 二进制启动；是否可运行仍依赖构建产物、外部子模块和目标环境。 |

当前根目录共有 13 份 `docker-compose*.yml`：它们不是互相替代的单一配置，而是单机、双机、性能、横向扩展和 release 的组合矩阵。运行或审查任何一个环境前，应明确主 Compose、所有 `-f` 覆盖文件、`.env`/`env_file` 和宿主机端口策略。

## 8. 基础设施、演示资产与脚本

| 路径 | 内容与用途 |
|---|---|
| `infra/apisix/` | APISIX 基础配置。业务路由可由控制面 Admin API 动态推送。 |
| `infra/fake-4a/` | Python Fake 4A，供授权码登录演示，不代表真实客户 4A 兼容性。 |
| `infra/demo-sites/` | 公司门户/部门页演示站点。 |
| `infra/test-workload/` | Mock HTTP 上游，用于数据面和 APISIX 冒烟。 |
| `infra/migrations/postgres/` | PostgreSQL 初始化 schema。 |
| `infra/storage-seed/` | 双机/演示路由、公司用户和应用数据种子。 |
| `infra/observability/` | Prometheus、OTel Collector 和 Intra Nginx metrics gateway 配置。 |
| `infra/tls/` | 本地/演示 TLS 材料；生产必须从受控密钥系统注入和轮换。 |
| `scripts/`、`scripts/ops/` | 启动、冒烟、压测、指标快照、回滚、路由诊断和容量验证工具。 |

## 9. 前端与可观测性

前端不参与 gRPC 隧道本身。当前有三个独立项目：

- `frontend-admin-next`：主控制台，含应用与 API route 管理、OpenAPI 导入、身份源、角色映射、审计、故障、安全、工作流、硬件与观测页面。
- `frontend`：旧控制台，保留用户、路由、上游、策略、健康与 4A 调试页面。
- `frontend-portal`：用户登录、应用导航、策略预检与网关探测。

观测面包括服务 `/metrics`、Prometheus、Grafana、OTel Collector、Node Exporter 及 Intra metrics gateway。审计和故障事件落入共享存储，但不同服务的写入模式不同：有的同步写、有的每请求 spawn、有的使用有界队列；不能概括为全链路统一的非阻塞审计管道。

## 10. 当前限制与不能夸大的结论

1. `proxy/core` 子模块在当前工作区为空，Zentinel 本体不可由本仓独立审计或构建。
2. 主 Compose 仍含开发型启动方式、演示性依赖和需覆盖的开发配置；它不是自动满足生产基线的交付物。
3. APISIX 路由推送并非强一致事务；需要监控 reconcile、Admin API 和上游映射是否成功。
4. 仅有少量 Rust 单元测试，缺少当前工作区可复现的完整端到端测试、前端 E2E 和 CI 定义。
5. 历史压测的 `apisix_routed` 口径将 HTTP 200–599 都视为“已路由”；它说明链路可达与响应覆盖，不能替代业务成功率或生产容量承诺。
6. `public-edge` 当前不是完整 WAF/CDN/TLS 产品，公共边缘防护属于仓外部署职责。

## 11. 推荐阅读顺序

1. [Cargo.toml](../Cargo.toml) 与 [tunnel.proto](../shared/tunnel-proto/proto/tunnel.proto)：先确认 workspace 和协议。
2. [http-tunnel-bridge](../proxy/http-tunnel-bridge/src/main.rs)、[Agent](../proxy/agents/stealth-tunnel-agent/src/grpc_server.rs)、[Connector](../proxy/connectors/sag-connector/src/main.rs)：理解数据面。
3. [sag-auth](../services/sag-auth/src/main.rs)、[sag-policy](../services/sag-policy/src/main.rs)、[control-plane-admin](../services/control-plane-admin/src/main.rs)：理解控制面。
4. [docker-compose.edge.yml](../docker-compose.edge.yml)、[docker-compose.intra.yml](../docker-compose.intra.yml)、[Zentinel KDL](../proxy/zentinel-proxy/config/dataplane-compose.kdl)：理解部署边界。
5. [TECHNICAL_DETAILS.md](TECHNICAL_DETAILS.md)：查看精确行为、配置、验证口径和已知限制。
