# SAG 技术栈与原理知识树

> 面向场景：技术面试复习、架构讲解、项目交接。
>
> 审计口径：以当前仓库源码、`Cargo.toml`、`package.json`、Docker Compose 和运行配置为准；历史文档与压测产物仅作辅助说明。
>
> 状态标记：`[已落地]` 有源码或默认编排证据；`[可选]` 需要环境变量/部署配置启用；`[仓外]` 当前仓库只有接入或配置，核心实现不在本工作区。

## 0. 一句话架构

SAG（Secure Access Gateway）是一个以 **Rust/Tokio** 实现的企业内网安全访问网关：控制面维护身份、策略和路由；数据面通过 **HTTPS → HTTP Bridge → gRPC Agent → 反向 Connector → APISIX → 内网应用** 的链路，把外部受控请求转发到内网服务。

```text
Browser / API Client
  ├─ 管理面：Next.js / Vite ──HTTP 反向代理──> sag-auth / sag-policy / control-plane-admin
  └─ 数据面：Zentinel HTTPS
                    │
                    ▼
             http-tunnel-bridge ── Unary gRPC Forward ──> stealth-tunnel-agent
                                                               │
                                                    双向 gRPC CreateTunnel
                                                               │
                                                               ▼
                                                        sag-connector
                                                               │
                                                               ▼
                                                        APISIX + etcd
                                                               │
                                                               ▼
                                                         内网应用/Mock
```

## 1. 后端基础：Rust 异步服务

- **Rust 2021 + Cargo Workspace** `[已落地]`
  - **应用**：仓库由 9 个 crate 组成，拆分为认证、策略、控制面、Bridge、Agent、Connector、公共边缘代理与共享协议/存储层。
  - **原理**：Cargo workspace 统一依赖与构建产物；Rust 的所有权、类型系统和 `Result` 错误模型在编译期消除一部分内存与并发错误。
  - **证据**：[根 Cargo.toml](Cargo.toml)。

- **Tokio Runtime** `[已落地]`
  - **应用**：承载 HTTP/gRPC 服务、控制面定时同步、心跳、队列 worker、后台审计和缓存刷新。
  - **原理**：协作式异步 I/O 将大量等待网络的任务复用到少量运行时线程上；它是异步任务模型，不是“一请求一系统线程”。

- **Axum + Tower + tower-http** `[已落地]`
  - **应用**：`sag-auth`、`sag-policy`、`control-plane-admin`、Bridge 等提供 `/api/v1/*`、`/health`、`/metrics` HTTP 接口。
  - **原理**：Axum `Router` 负责路径/方法分发；Tower Service 与 Layer 组成责任链；`TraceLayer` 和自定义 middleware 实现日志、时延和指标等横切能力。
  - **面试表述**：这里没有 Spring AOP，等价的横切处理由 HTTP middleware/Layers 完成。

- **Hyper / hyper-util / Reqwest** `[已落地]`
  - **应用**：Bridge 处理 HTTP 转发；Connector 用 Reqwest 请求 APISIX；认证服务调用 4A/OIDC；控制面调用 APISIX Admin API。
  - **原理**：Hyper 是 Rust 异步 HTTP 底座；Reqwest 封装连接池、TLS、超时和 JSON 序列化；连接复用降低 TCP/TLS 建连成本。

- **Serde / serde_json** `[已落地]`
  - **应用**：HTTP JSON 请求/响应、Redis 缓存对象、配置和审计扩展字段。
  - **原理**：通过 `Serialize`/`Deserialize` trait 在编译期生成数据编解码逻辑，减少手写解析错误。

## 2. 身份认证与授权

- **Argon2 密码哈希** `[已落地]`
  - **应用**：本地用户创建和密码登录校验。
  - **原理**：Argon2 是内存硬哈希算法；每个密码用随机盐进行哈希，数据库保存 hash 而非明文，提升离线撞库成本。

- **JWT（HS256）+ Bearer Token** `[已落地]`
  - **应用**：`sag-auth` 签发含用户、角色、签发时间和过期时间的访问令牌；管理 API 校验 `admin`/`boss` 等角色。
  - **原理**：HS256 用共享密钥对 claims 做 HMAC 签名；服务端验证签名和 `exp` 即可完成无状态认证。
  - **边界**：当前是对称密钥签名，不是基于 JWKS 的非对称 Token 分发体系。

- **OAuth 2.0 Authorization Code / OIDC / 企业 4A** `[已落地]`
  - **应用**：支持跳转企业 4A 或 OIDC，回调后以授权码交换 token、获取 userinfo/组信息，并映射成本地角色。
  - **原理**：授权码模式不在浏览器前端直接暴露 access token；随机 `state` 在 Redis 或内存中限时一次性存取，用于抵抗 OAuth 回调 CSRF。

- **RBAC + Policy Decision Point（PDP）** `[已落地]`
  - **应用**：`sag-policy` 根据 `subject × app_id × path_prefix × effect × priority` 判定访问；Agent 可在转发前调用 `/api/v1/policy/evaluate`。
  - **原理**：策略按优先级排序并 first-match；无匹配时默认 DENY，属于显式授权而不是默认放行。
  - **边界**：Agent 的认证/策略 endpoint 可通过部署配置关闭，不能宣传为“每一条数据面请求都无条件经过 JWT 与 PDP”。

- **mTLS（rustls / tokio-rustls）** `[已落地]`
  - **应用**：Bridge 和 Connector 使用客户端证书接入 Agent；Agent 可配置 CA 来校验客户端证书。
  - **原理**：TLS 提供加密与服务端身份验证；mTLS 进一步验证客户端身份，降低仅依赖网络位置的信任风险。
  - **运维要点**：生产环境必须外置管理证书、私钥、CA、SNI 和轮换策略；仓库内证书仅适用于开发/演示。

## 3. 数据面：安全访问与反向隧道

- **Zentinel HTTPS Edge** `[仓外：配置已落地]`
  - **应用**：监听数据面 HTTPS `:10080`，转发给 `http-tunnel-bridge`；配置 TLS 1.2、90 秒路由超时、请求头/体大小限制与 fail-closed。
  - **原理**：L7 入口在无法将流量安全转给上游时直接拒绝；这比故障时旁路到未知上游更安全。
  - **边界**：当前仓库只包含 KDL 配置；Zentinel 的二进制核心来自 `proxy/core` Git 子模块。

- **HTTP Tunnel Bridge** `[已落地]`
  - **应用**：把外部 HTTP 请求转换为 gRPC `ForwardRequest`，将 Agent 返回的 `ForwardResponse` 恢复成 HTTP 状态、头和 body。
  - **原理**：这是应用层（L7）隧道：将 `method/path/headers/body` 以 Protobuf 序列化，而不是裸 TCP/VPN 隧道；`request_id` 贯穿请求—响应关联。

- **gRPC / Tonic / Prost / Proto3** `[已落地]`
  - **应用**：Bridge 到 Agent 使用 Unary `Forward`；Agent 与 Connector 使用双向流 `CreateTunnel`。
  - **原理**：gRPC 基于 HTTP/2，支持多路复用和流式通信；双向流让 Connector 主动连出，在已有长连接上接收下发请求，减少内网应用被公网直接访问的需要。
  - **协议证据**：[tunnel.proto](shared/tunnel-proto/proto/tunnel.proto)。

- **Agent Connector Registry** `[已落地]`
  - **应用**：Agent 在内存中维护 `endpoint → outbound sender` 与 `request_id → oneshot responder`，并依据 connector 心跳和路由选择内网连接器。
  - **原理**：`mpsc` 负责消息投递，`oneshot` 完成一次性的异步响应通知；读写锁保护共享注册表；待处理请求上限阻止 pending map 无限增长。
  - **边界**：注册表与 pending 表是单 Agent 进程内状态，当前没有共享服务发现或自动多活路由。

- **sag-connector + APISIX** `[已落地]`
  - **应用**：Connector 将流内请求再发成 HTTP 请求给 APISIX；APISIX 根据路由/上游将流量送至内网应用或 Mock。
  - **原理**：Connector 主动从内网向外建立 gRPC 长连；APISIX 将 L7 路由、上游选择与真实业务服务解耦。

- **Apache APISIX + etcd** `[已落地]`
  - **应用**：APISIX 处理内网 HTTP 路由与上游；控制面可经 Admin API 下发应用路由，并执行定期 reconcile。
  - **原理**：APISIX 使用 etcd 保存动态配置；控制面以 HTTP Admin API 进行最终一致性同步，而非与业务数据库组成强一致分布式事务。

## 4. 并发控制、背压与故障隔离

- **Tokio `mpsc` 有界队列 + `try_send`** `[已落地]`
  - **应用**：Connector 的接入队列、审计队列、Agent 的流消息通道。
  - **原理**：有界队列将内存上限显式化；队列满时立即拒绝（如 503）或丢弃低优先级审计，而不是无限堆积导致进程 OOM。

- **Semaphore 并发闸门** `[已落地]`
  - **应用**：限制 Bridge 的隧道 RPC、Agent 对 policy/auth 的调用，以及 Connector 的最大 in-flight 转发。
  - **原理**：令牌数等于可同时进入临界区的最大请求数；没有许可时等待、排队或拒绝，形成对慢下游的背压。

- **FuturesUnordered / Tokio `select!`** `[已落地]`
  - **应用**：Connector 在固定 in-flight 上限内并发执行请求，同时继续接收流消息和处理完成事件。
  - **原理**：轮询一组 Future 的就绪事件，避免每个请求都建立独占线程；`select!` 在多个异步事件源之间竞争等待。

- **DashMap + 分应用令牌桶** `[可选]`
  - **应用**：Bridge 按 `x-sag-app-id` 做应用级 RPS 限流。
  - **原理**：DashMap 将并发 Map 分片，避免整表单锁；每个应用的 Token Bucket 按时间补充令牌，请求无令牌时快速返回 HTTP 429。

- **熔断器（Circuit Breaker）** `[可选]`
  - **应用**：Bridge 连续 gRPC 转发失败达到阈值后，cool-off 窗口内快速返回 HTTP 503。
  - **原理**：Atomic 记录连续失败次数和 `open_until`；打开后不再继续打故障下游，减少重试风暴。
  - **边界**：当前是全局连续失败模型，不是按错误类型、按租户或带完整 half-open 探测的熔断器。

- **超时预算阶梯** `[已落地]`
  - **应用**：Connector HTTP、Agent 等待 Connector、Bridge gRPC、Zentinel 路由和 k6 客户端各有 timeout。
  - **原理**：内层 deadline 应短于外层 deadline，超时能够尽早释放资源并避免“客户端已断开，服务端仍占用连接等待”的悬挂请求。

## 5. 缓存、队列与降级

- **Redis 7** `[已落地/可选]`
  - **应用**：OAuth state、登录 memo、策略决策缓存、Agent 降级缓存和 Bridge 异步队列。
  - **原理**：TTL 让临时状态自动过期；Redis 故障时部分模块退化到进程内缓存或同步转发，避免单点缓存使全系统完全不可用。

- **Moka Future Cache** `[已落地]`
  - **应用**：策略决策、用户/身份源读取、控制面路由读取、Agent 负缓存。
  - **原理**：本地并发缓存按 TTL 和最大容量淘汰；缓存命中可避免 DB/PDP 的重复调用。

- **短 TTL 负缓存** `[已落地]`
  - **应用**：Agent 缓存“无隧道路由”“connector 不健康”“策略拒绝”等高频失败。
  - **原理**：短时间内相同错误直接返回，降低攻击流量或错误客户端带来的重复计算；TTL 必须短，避免配置刚修复仍长期返回旧错误。

- **Redis Streams + Consumer Group** `[可选]`
  - **应用**：Bridge 达到软/硬容量门槛时返回 HTTP 202，并将请求异步入队；客户端轮询 `__sag/queue/{id}/status` 获取结果。
  - **原理**：`XADD` 追加消息；`XREADGROUP` 让 worker 竞争消费；`XACK` 确认完成；任务状态、去重键和结果均带 TTL。

- **去重与死信队列（DLQ）** `[可选]`
  - **应用**：同一 `request_id` 只处理一次，无法解析或转发失败的队列消息进入 DLQ。
  - **原理**：`SET NX EX` 用原子“仅首次写入”实现有限时间幂等；异常消息保存到独立 Stream，避免阻塞主消费流。

- **stale-while-degraded** `[可选]`
  - **应用**：policy/auth 短暂不可用时，Agent 可读取近期成功的允许策略或身份信息。
  - **原理**：以短期陈旧数据换取短暂可用性；它天然存在授权陈旧风险，必须限制 TTL、调用范围和审计。

## 6. 可观测性、压测与运维

- **`tracing` + TraceLayer** `[已落地]`
  - **应用**：服务端记录结构化日志和请求处理上下文。
  - **原理**：日志事件携带字段而不是纯字符串拼接，利于按服务、路径、错误、时延过滤和关联。

- **Prometheus metrics crate + Prometheus** `[已落地]`
  - **应用**：暴露 `/metrics`，采集 HTTP 时延、缓存命中、队列、熔断、gRPC、Connector 健康等指标。
  - **原理**：Prometheus 采用 Pull 模型抓取指标；Counter 反映累计事件，Histogram/Trend 反映分位时延，可用于 RED/容量分析。

- **Grafana / Node Exporter** `[已落地]`
  - **应用**：展示服务指标和主机资源。
  - **原理**：Grafana 查询 Prometheus 时序数据；Node Exporter 把操作系统 CPU、内存、磁盘、网络等转换为指标。

- **OpenTelemetry Collector** `[仅编排]`
  - **应用**：预留 OTLP gRPC/HTTP 接收和 Prometheus exporter。
  - **原理**：Collector 可以接收、处理和转发 trace/metric/log 遥测数据。
  - **边界**：当前 Rust 服务未见 OTLP exporter 或 tracing-opentelemetry 依赖，不能描述为“全链路 Trace 已接通”。

- **k6 压测** `[已落地]`
  - **应用**：数据面与全链路压测；区分 202 异步排队、429 保护性拒绝、超时和 APISIX/upstream 失败。
  - **原理**：`ramping-arrival-rate` 控制到达速率；通过成功率、Trend、Counter/Rate 与服务指标按同一时间窗关联定位瓶颈。

- **Docker Compose / Edge-Intra 分部署** `[已落地]`
  - **应用**：提供单机演示、Edge/Intra 双机、横向扩展、性能与发布覆盖配置。
  - **原理**：容器网络、服务发现、volume、healthcheck、环境变量和 `nofile` ulimit 共同约束运行时拓扑与资源边界。

## 7. 前端与 BFF 风格代理

- **Next.js 15 + React 18 + TypeScript** `[已落地]`
  - **应用**：`frontend-admin-next` 是主控制台，覆盖应用、路由、身份源、审计、可观测性和工作流界面。
  - **原理**：Next rewrite 将浏览器同源路径代理至多个后端服务，减少浏览器跨域配置和后端地址暴露。

- **Vite 5 + React 18** `[已落地]`
  - **应用**：保留旧管理台 `frontend` 和用户门户 `frontend-portal`。
  - **原理**：Vite 基于原生 ESM 开发服务器与构建工具链；开发时 proxy 将 `/api-auth`、`/api-policy` 等前缀重写到后端。

- **Tailwind CSS / Radix UI / Lucide** `[已落地]`
  - **应用**：后台表单、表格、标签、按钮、图标等 UI。
  - **原理**：Tailwind 用原子类组合样式；Radix 提供无样式、可访问性优先的组件原语；组件层再封装业务视觉规范。

- **ECharts / React Flow** `[已落地]`
  - **应用**：控制台展示可观测性图表和工作流/拓扑节点。
  - **原理**：ECharts 将时序数据渲染成图形；React Flow 用节点、边和 viewport 模型表达可交互关系图。

## 8. 面试口径与事实边界

- **不是 Spring/JVM 项目**
  - 项目没有 Spring Boot、JVM 参数调优、MySQL、RabbitMQ 或 Kafka 的实际依赖。
  - 横切能力由 Axum middleware、Tower Layer 与 `tracing` 实现，而不是 AOP。

- **不是“所有请求强制鉴权”的无条件实现**
  - 数据面认证和 PDP 调用由 Agent 环境配置决定；标准部署意图是开启，但代码允许不配置。

- **不是已经完成多活的分布式隧道控制面**
  - Connector 注册、心跳和 pending 响应目前是 Agent 单进程内存状态；多 Agent 需要共享注册、分片或粘性路由方案。

- **不是完整 OTel 链路追踪平台**
  - 已有 Prometheus 指标和 Collector 编排，但未发现应用侧 OTLP trace 导出和 trace backend。

- **开发凭据不能等同生产安全能力**
  - Compose 中的默认 JWT、数据库、APISIX/4A 凭据与测试证书属于开发/演示配置；生产需要 Secret 管理、证书轮换、最小化网络暴露和审计告警。

## 9. 关键源码入口

- [根依赖与成员](Cargo.toml)
- [单机完整拓扑](docker-compose.yml)
- [认证服务](services/sag-auth/src/main.rs)
- [策略服务](services/sag-policy/src/main.rs)
- [控制面](services/control-plane-admin/src/main.rs)
- [APISIX 同步逻辑](services/control-plane-admin/src/apisix.rs)
- [隧道协议](shared/tunnel-proto/proto/tunnel.proto)
- [Agent gRPC 服务](proxy/agents/stealth-tunnel-agent/src/grpc_server.rs)
- [Connector](proxy/connectors/sag-connector/src/main.rs)
- [Bridge](proxy/http-tunnel-bridge/src/main.rs)
- [Bridge Redis 队列](proxy/http-tunnel-bridge/src/queue.rs)
- [Zentinel 数据面配置](proxy/zentinel-proxy/config/dataplane-compose.kdl)
