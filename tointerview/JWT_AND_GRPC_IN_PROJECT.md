# SAG 项目中的 JWT 与 gRPC：代码事实梳理

> 用途：本文件用于与 AI 或面试官讨论。内容以当前工作区的 Rust 源码、Proto 协议和 `docker-compose.yml` 为准，不把通用架构想象成项目事实。
>
> 范围：JWT 的签发、保存、传递、验证、授权使用；gRPC 的协议、两类调用、隧道、mTLS、超时和失败处理。
>
> 安全说明：本文不会记录真实密钥、密码、私钥或可复用 Token。配置名可以讨论，配置值一律不写入。

---

## 0. 先给结论

本项目把 JWT 和 gRPC 用在两件不同的事情上：

```text
JWT：证明“这个用户是谁、有什么角色”
gRPC：把“已经通过/正在进行安全处理的 HTTP 请求”可靠地送过隧道
```

二者不是替代关系：

```text
浏览器携带 JWT 的 HTTP 请求
  ↓
Agent 用 JWT 得到用户身份与角色，并调用策略服务作出允许/拒绝决定
  ↓
若允许，HTTP 请求被包装为 gRPC 消息，进入内网侧 Connector 隧道
```

在当前 Compose 默认链路中：

```text
浏览器
  └─ HTTP + Authorization: Bearer <JWT> + x-sag-app-id
       → Zentinel
       → http-tunnel-bridge
       ── gRPC Unary Forward ──→ stealth-tunnel-agent
                                      ├─ HTTP JSON → sag-auth /verify
                                      ├─ HTTP JSON → sag-policy /evaluate
                                      └─ 既有 gRPC 双向流 → sag-connector
                                                                  └─ HTTP → APISIX → 内网应用
```

重要边界：控制面到 Agent 的路由同步是 **HTTP**，不是 gRPC；Agent 到认证/策略服务也是 **HTTP JSON**，不是 gRPC。

---

## 1. JWT 在项目中是什么

JWT 是认证服务签发的一串有签名的声明（claims）。调用方把它放进：

```http
Authorization: Bearer <token>
```

接收服务用相同的签名密钥验证它没有被篡改，并读取其中的用户和角色信息。

本项目使用 Rust 的 `jsonwebtoken` 依赖。代码用 `EncodingKey::from_secret` 签发、`DecodingKey::from_secret` 验签，说明它是**共享对称密钥**模式：签发者和验签者都必须拿到相同的 `SAG_JWT_SECRET`。

代码没有显式设置 JWT 算法，而是使用 `Header::default()`；若要讨论算法的精确默认值，需要进一步核对当前锁定版本 `jsonwebtoken = 9` 的依赖实现或文档，不能仅凭项目代码断言。

### 1.1 Token 中有什么

认证服务的 `Claims` 结构如下：

| Claim | 含义 | 代码事实 |
|---|---|---|
| `sub` | 用户唯一 ID | 本地用户 ID 或 SSO 用户标识 |
| `username` | 用户名 | 登录名或 SSO 返回的用户标识 |
| `roles` | 本地角色数组 | 例如 `admin`、`boss`、`ops`、`tech` 等；实际取决于数据/映射 |
| `external_groups` | 外部身份源组 | 可选；OIDC 场景会从 token/userinfo 取组后参与映射 |
| `exp` | 过期时间（秒级 Unix 时间） | 默认有效期 3600 秒，可由 `SAG_JWT_EXPIRES_SEC` 覆盖 |
| `iat` | 签发时间 | 秒级 Unix 时间 |
| `iss` | 签发者 | 签发时写为 `sag-auth` |

代码中没有写入 `aud`、`jti`、`nbf` 等 claim，也没有看到 Token 黑名单、单 Token 主动注销或刷新 Token 接口。

来源：`services/sag-auth/src/main.rs` 中的 `Claims`、`issue_jwt()`。

### 1.2 JWT 从哪里签发

两条登录路径最后都调用同一个 `issue_jwt()`：

```text
A. 本地账号密码登录
   POST /api/v1/auth/login
   → 从 users 表/内存用户表找到用户
   → Argon2 校验密码 hash
   → issue_jwt()

B. 4A 或 OIDC 单点登录
   GET /api/v1/auth/sso/login
   → 外部身份提供方授权码流程
   → 获取用户/组信息，映射为本地角色
   → issue_jwt()
```

本地密码本身不在 JWT 内。项目只保存 Argon2 哈希，登录时用 `verify_password()` 校验。

### 1.3 JWT 如何从后端到浏览器

本地密码登录的 HTTP 响应体包含：

```json
{
  "token": "<JWT>",
  "user": { "id": "...", "username": "...", "roles": ["..."] },
  "expires_in_sec": 3600
}
```

Next 管理台把 Token 放在浏览器 `localStorage` 的 `sag_token`；旧 Vite 管理台使用 `sag.console.token`。随后前端 API 封装会自动加上 `Authorization: Bearer <token>`。

SSO 回调则会把 Token 作为 `sso_token` 查询参数重定向到门户，前端验证成功后写入 `localStorage`，并用 `history.replaceState()` 从地址栏删除该参数。

这不是 Cookie Session 方案：当前可见前端代码把 JWT 存在 `localStorage`，而不是 `HttpOnly` Cookie。

### 1.4 JWT 在哪里验证、验证后做什么

| 使用点 | 如何验证 | 成功后用途 | 所需角色 |
|---|---|---|---|
| `sag-auth /api/v1/auth/verify` | 用共享密钥 `decode`，再显式检查 `exp` | 返回 `{ active, user }`；是 Agent 获取可信身份的正常入口 | 不要求特权角色 |
| `sag-auth` 用户/身份源/映射管理 API | 解析 Bearer Token | 管理用户、身份提供方、组角色映射 | `admin` / `boss` / `ops` 任一即可 |
| `control-plane-admin` 管理 API | 解析 Bearer Token | 管理应用、路由、上游、审计、故障等 | `admin` 或 `boss` |
| `sag-policy` 策略管理 API | 解析 Bearer Token | 新增、删除、查询策略 | `admin` 或 `boss` |
| `stealth-tunnel-agent` 数据平面 | 正常情况下不自行 decode；HTTP 调 `sag-auth /verify` | 获取用户 ID 和角色，再调用策略服务 | 由策略决定 |

说明：`sag-policy /api/v1/policy/evaluate` 本身的 handler 未在代码中做 Bearer JWT 校验。项目设计是让 Agent 先完成身份解析，再把 `user_id`、`roles`、`app_id`、`path`、`method` 作为 JSON 请求体传给策略服务。

### 1.5 “认证”与“授权”的严格分工

```text
sag-auth：Authentication（认证）
  输出：用户 ID、角色、外部组；即“你是谁”

sag-policy：Authorization（授权）
  输入：用户 ID、角色、app_id、path、method
  输出：ALLOW / DENY；即“你是否可以访问这项资源”

Agent：Policy Enforcement Point（执行点）
  负责让未认证、无权限、无可用隧道的请求无法继续进入 Connector
```

策略服务会将规则按优先级倒序排列，第一条同时匹配主体（用户/角色）和资源（应用/路径）的规则决定结果；完全没有匹配项时是 `DENY`。这意味着 JWT 的 `roles` 是策略判断的关键输入，但 **拥有角色不等于一定允许访问**。

### 1.6 JWT 在数据平面中的实际过程

正常请求应携带：

```http
Authorization: Bearer <JWT>
x-sag-app-id: app-001
```

浏览器前端还会发送 `x-sag-user-id` 和 `x-sag-user-roles`，但它们不应被当成最终可信身份来源：这些是调用方可伪造的普通 HTTP Header。

Agent 的正常行为是：

1. 从 `Authorization` 取 Bearer Token；
2. POST 到 `SAG_AUTH_VERIFY_ENDPOINT`，请求体为 `{ "token": "..." }`；
3. 若认证服务返回 `active: true`，采用返回的用户 ID、角色；
4. 调用 `SAG_POLICY_EVALUATE_ENDPOINT`；
5. 只有 ALLOW 才继续寻找 Connector 和转发。

### 1.7 代码观察：无 Bearer Token 时的身份头行为（待审查）

`StealthTunnelConfig` 的注释说明：只有未配置认证服务并且显式开启 `SAG_TRUST_IDENTITY_HEADERS` 时，才应信任调用方提供的身份 Header。

但是当前 `resolve_user_identity()` 的实际分支是：

```text
认证服务已配置 + 请求带 Bearer Token
  → 调 /verify，使用认证服务返回的身份

认证服务已配置 + 请求没有 Bearer Token
  → 不调用 /verify，直接返回请求中的 x-sag-user-id / x-sag-user-roles（如果存在）
```

因此，这份代码的“无 Bearer Token”分支与上述安全意图并不完全一致。它是否会在真实入口被上游网关阻断，当前可见 Zentinel 源码缺失，无法确认。

这是应与 AI/面试官讨论、并建议代码审查的点；不要把“配置了认证服务就绝不会信任身份 Header”当成已经被代码证明的结论。

### 1.8 JWT 失效、降级和当前限制

| 情况 | 当前实现 |
|---|---|
| JWT 验签失败或 `exp` 已过期 | `/verify` 返回 `active: false`；Agent 会把身份解析失败处理为 403 响应 |
| `sag-auth /verify` 网络错误/超时 | Agent 可从可选 Redis 降级缓存读取旧身份；没有缓存则失败 |
| Policy 服务超时/错误 | Agent 可使用可选 Redis 中的旧 `ALLOW` 决策；没有则返回 503 |
| 用户在浏览器“退出登录” | 前端仅删除 `localStorage`；没有看到服务端 Token 撤销 |
| 管理员修改用户角色/禁用用户 | 当前 Token 中已有角色仍会持续到过期；没有看到验证时回查用户 enabled 状态 |
| 变更 JWT 密钥 | 旧 Token 会无法通过依赖新密钥的验签；这是一种整体失效方式，但不是细粒度撤销 |

---

## 2. gRPC 在项目中是什么

gRPC 在本项目中是 **Bridge、Agent、Connector 之间的服务通信协议**。其消息结构不靠手写 JSON 约定，而由 `shared/tunnel-proto/proto/tunnel.proto` 定义，Rust 在构建时通过 `tonic-build` 生成类型和客户端/服务端代码。

项目使用 `tonic`，底层是 HTTP/2；目前定义了一个服务、两个 RPC：

```proto
service TunnelService {
  rpc CreateTunnel(stream TunnelMessage) returns (stream TunnelMessage);
  rpc Forward(ForwardRequest) returns (ForwardResponse);
}
```

这两个 RPC 的角色完全不同。

### 2.1 RPC 一：`Forward`，Bridge 到 Agent 的一问一答

```text
http-tunnel-bridge ── Forward(ForwardRequest) ──→ Agent
http-tunnel-bridge ←─ ForwardResponse ────────── Agent
```

它是 unary RPC：一个请求对应一个响应。

`ForwardRequest` 是 HTTP 请求的结构化表示：

| 字段 | 来源 | 用途 |
|---|---|---|
| `request_id` | Bridge 生成 UUID | 将 Connector 返回的响应关联回原请求 |
| `app_id` | `x-sag-app-id` | Agent 找路由，APISIX 匹配应用上游 |
| `method` | HTTP 方法 | Connector 重建 HTTP 请求 |
| `path` | HTTP path + query | Connector 重建 HTTP URL |
| `headers` | HTTP Header，去掉 hop-by-hop Header 后 | JWT、内容类型、追踪等信息随请求传递 |
| `body` | HTTP Body | Connector 重建 HTTP 请求体 |

`ForwardResponse` 返回 HTTP 状态码、Header、Body，Bridge 再将其还原为浏览器能收到的 HTTP 响应。

Bridge 并非只做序列化。它还会限制 body、清理 hop-by-hop Header、控制 gRPC 并发、维护 gRPC client pool、可选限流/熔断/Redis 排队，并在一次 RPC 失败后尝试另一个或重连后的 channel。

### 2.2 RPC 二：`CreateTunnel`，Connector 主动建立的双向流

```text
sag-connector ── 主动连接 ──→ Agent gRPC :50051
      │                              │
      ├──── Register / Heartbeat ───→│
      │←──── ForwardRequest ─────────┤
      └──── ForwardResponse ────────→│
```

它是 bidirectional streaming RPC：一个长期存在的连接上，双方可以各自持续发送多条 `TunnelMessage`。

`TunnelMessage` 是一个 `oneof` 包装，可以装四类消息：

| 消息 | 发送方向 | 含义 |
|---|---|---|
| `ConnectorRegister` | Connector → Agent | 注册 `connector_id`、`app_id`、`external_host`、`endpoint` |
| `ConnectorHeartbeat` | Connector → Agent | 心跳，表明该 endpoint 仍健康 |
| `ForwardRequest` | Agent → Connector | 已通过认证/授权的用户 HTTP 请求 |
| `ForwardResponse` | Connector → Agent | Connector 从 APISIX 获得的 HTTP 响应 |

这条流由 Connector 发起，所以项目不需要让公网主动拨入内网。Connector 断线时会退出本轮连接、按退避策略重连；Agent 也会从 registry 中移除对应 endpoint。

### 2.3 Agent 如何把请求送到正确 Connector

Agent 内部维护两个不同的内存结构：

```text
路由表：app_id → connector_endpoint
  来源：Agent 定期通过 HTTP 从 control-plane-admin 拉取 tunnel_routes

连接注册表：connector_endpoint → 当前双向 gRPC 流的发送通道
  来源：Connector 的 Register 消息
```

处理一个 `ForwardRequest` 时：

1. Agent 完成认证、策略和健康检查；
2. 用 `app_id` 从路由表找 `connector_endpoint`；
3. 用 endpoint 从连接注册表找到对应 stream 的发送通道；
4. 以 `request_id` 建立一个内部 one-shot 等待器；
5. 将 `ForwardRequest` 推入该 Connector stream；
6. Connector 返回同一 `request_id` 的 `ForwardResponse`；
7. Agent 用 `request_id` 唤醒正确的等待器，回复给 Bridge。

因此，“路由配置存在”不等于“此刻能转发”：还必须存在已注册、且最近心跳健康的 Connector。

### 2.4 gRPC 与 HTTP 的边界总表

| 起点 | 终点 | 协议 | 主要目的 |
|---|---|---|---|
| 浏览器 | Zentinel | HTTPS/HTTP | 用户访问应用 |
| Zentinel | Bridge | HTTP | 外部入口转交给隧道链路 |
| Bridge | Agent | gRPC Unary `Forward` | HTTP 请求进入 Agent |
| Connector | Agent | gRPC 双向流 `CreateTunnel` | 主动建立、维持隧道；承载请求和响应 |
| Agent | sag-auth | HTTP JSON | JWT Token 验证 |
| Agent | sag-policy | HTTP JSON | 授权决策 |
| Agent | control-plane-admin | HTTP JSON | 定期拉取路由 |
| Connector | APISIX | HTTP | 进入内网应用路由层 |
| APISIX | 内网应用 | HTTP/HTTPS，按 upstream 配置 | 访问最终业务服务 |

另有 OpenTelemetry Collector 暴露 OTLP gRPC 端口；这是可观测性通道，和上述隧道 gRPC 不是同一用途。

---

## 3. gRPC 传输安全：当前实现能确认什么

### 3.1 设计目标

Agent、Bridge、Connector 的配置都使用 `SAG_GRPC_MTLS_ENABLED`，默认开启。Bridge 和 Connector 都会读取：

- 客户端证书；
- 客户端私钥；
- CA 证书；
- 可选的服务端名称（SNI / hostname verification）。

Agent 作为 gRPC server 读取服务端证书和私钥；如果能读取到配置的客户端 CA，则将该 CA 设置为 client CA root。

Compose 中为 Agent、Bridge、Connector 都配置了证书路径，所以默认编排的意图是 mTLS：客户端验证 Agent，Agent 也验证客户端证书链。

### 3.2 必须说清的代码条件

Agent 的 server 代码在读取客户端 CA 时使用 `if let Ok(ca) = read(...)`。也就是说：

```text
CA 文件可读 → 配置 client CA root
CA 文件不可读 → server 不会在此处直接报错，而是继续启动 TLS server
```

所以不能把“环境变量名包含 MTLS”直接等同于“任何部署下都已实现双向认证”。是否真的达成 mTLS，要检查实际证书文件、启动日志和握手结果。

### 3.3 超时和保活

| 参数类别 | 默认/Compose 意图 | 目的 |
|---|---|---|
| connect timeout | Bridge 默认 5 秒 | 建立到 Agent 的 gRPC 连接超时 |
| RPC deadline | Bridge 默认 120 秒 | Unary RPC 最大生命周期 |
| Bridge forward timeout | Compose 设置为较短的业务等待窗口 | 等待 Agent 转发响应 |
| Agent forward timeout | Compose 有单独配置 | Agent 等待 Connector 返回响应 |
| Connector HTTP timeout | Compose 有单独配置 | Connector 等 APISIX/内网应用响应 |
| HTTP/2/TCP keepalive | Bridge/Connector 都配置 | 维持长连接、发现死连接 |
| Connector heartbeat | Compose 配置周期 | Agent 判定隧道是否健康 |

超时需要保持“外层比内层略长”的关系。代码本身会在发现 gRPC RPC deadline 小于 Bridge forward timeout 时记录警告，因为那会导致业务尚未等到结果，gRPC 已提前断开，浏览器最终可能看到 HTTP 502。

---

## 4. 一次成功访问的逐步证据链

以“用户访问 `app-001` 的某个路径”为例：

1. 前端从 `localStorage` 读取 JWT，发送 HTTP `Authorization: Bearer <JWT>` 和 `x-sag-app-id: app-001`。
2. Zentinel 依据当前 KDL 配置将 HTTP 请求转发给 Bridge。
3. Bridge 验证 `x-sag-app-id` 不为空，收集 method/path/headers/body，生成 UUID 作为 `request_id`，创建 `ForwardRequest`。
4. Bridge 使用 gRPC `Forward` 调 Agent。
5. Agent 调用认证服务 `/api/v1/auth/verify`，获得用户 ID、角色。
6. Agent 调用策略服务 `/api/v1/policy/evaluate`；如果命中 ALLOW，则继续。
7. Agent 依据 `app_id` 找到配置的 Connector endpoint；若没有路由，返回 502；若心跳过期，返回 gRPC unavailable。
8. Agent 将 `ForwardRequest` 放入 endpoint 对应的双向 stream，并按 `request_id` 等待响应。
9. Connector 收到 request，重建 HTTP 请求，发到 `SAG_APISIX_BASE_URL`。
10. APISIX 按 `x-sag-app-id` 匹配控制面下发的 route，转到内网 upstream。
11. 响应变成 `ForwardResponse` 沿同一个双向 stream 返回 Agent。
12. Agent 按 `request_id` 找到等待者，回复 Bridge；Bridge 恢复 HTTP status/header/body，返回浏览器。

---

## 5. 失败结果如何理解

| 失败点 | 当前代码表现（概括） | 用户侧可能看到 |
|---|---|---|
| 无法建立/调用 Bridge → Agent gRPC | Bridge 尝试 channel；仍失败则 tunnel error | HTTP 502 |
| JWT 无效，且 Agent 走认证服务验证 | Agent 解析身份失败 | HTTP 403（由 `ForwardResponse` 表示） |
| 策略明确拒绝 | Agent 返回 deny response | HTTP 403 |
| 策略服务临时不可用，且没有旧 ALLOW 可降级 | Agent fail closed | HTTP 503 |
| `app_id` 没有路由 | Agent 返回 deny-like response | HTTP 502 |
| Connector 心跳不健康 | Agent 返回 gRPC unavailable | Bridge 转为 HTTP 502 |
| Connector stream 断开/回包超时 | Agent 返回 gRPC internal/deadline error | Bridge 转为 HTTP 502 |
| Connector 调 APISIX/上游失败 | Connector 封装为 `ForwardResponse(502)` | HTTP 502 |

注意：策略拒绝是业务层 `ForwardResponse(403)`；Connector 健康失败等有些是 gRPC transport status，最终由 Bridge 映射为 HTTP 错误。两者表面上都是浏览器收到 HTTP 响应，但失败来源不同。

---

## 6. 关键缓存与性能机制

### JWT / 身份相关

- 本地密码登录可选 memo cache：命中时复用已签发 Token，避免重复 Argon2 和 JWT encode。
- OAuth state 可用 Redis 保存；未配置/连接失败则回退到内存。
- Agent 可选 Redis 保存过期前曾成功验证的身份（stale auth），在认证服务暂时不可用时尝试降级。

### gRPC / 隧道相关

- Bridge 有 1–32 个 gRPC `TunnelServiceClient` channel 的连接池，轮询挑选；第一次失败后可替换 channel 再尝试。
- Agent 对 policy、auth HTTP 调用各有 semaphore，限制同时请求数。
- Agent 对 Connector 等待数有上限，避免无限积压。
- Connector 有接受队列和最大 in-flight 控制，满时回送 503。
- Bridge 可选 Redis Queue：当同步转发压力达到软阈值时返回 HTTP 202，客户端轮询结果；硬阈值则拒绝。

这些机制的目标是“过载时有限排队、有限拒绝、避免无界内存增长”，而不是改变认证与策略规则。

---

## 7. 当前代码中的安全讨论点

以下是“从代码能观察到”的讨论项，不代表已完成整改或正式漏洞结论：

1. **共享 JWT 对称密钥**：认证、控制面、策略服务都需要相同密钥。优点是简单；代价是密钥分发范围更大。代码没有显式设置算法、issuer 校验或 audience 校验。
2. **JWT 在 localStorage**：浏览器端脚本可读取它；这与 HttpOnly Cookie 的威胁模型不同。
3. **SSO Token 经 URL 查询参数短暂传递**：前端会尽快清除，但在清除前可能出现在浏览器历史、日志或 Referer 风险路径中，需结合实际部署验证。
4. **没有看到 JWT 撤销列表**：前端退出只删除本地 Token，既有 Token 是否继续有效取决于过期时间和签名密钥是否变更。
5. **Agent 无 Bearer 分支**：见 1.7；需要确认是否应当对“认证服务已配置但没有 Authorization”的请求直接拒绝。
6. **策略接口本身没有认证中间件**：安全边界依赖其网络可达性和 Agent 调用方式；需要确认部署网络是否限制浏览器/非 Agent 直接访问 `:8081`。
7. **mTLS 依赖实际文件读取成功**：需要以启动日志、证书链和握手验证证明，而非只看环境变量名。

---

## 8. 面试表述模板

### 30 秒版

> SAG 使用 JWT 解决用户身份与角色传递，使用 gRPC 解决隧道内服务间通信。用户登录后由 `sag-auth` 签发 JWT；数据平面的 Agent 调认证服务校验 Token、调策略服务判定权限。通过后，Bridge 把 HTTP 请求封装成 gRPC `ForwardRequest` 发给 Agent；Agent 再沿 Connector 主动建立的双向 gRPC 流把请求送入内网，Connector 将其转为 HTTP 交给 APISIX 路由到最终应用。

### 追问“为什么同时需要 JWT 和 mTLS/gRPC”

> JWT 证明终端用户身份，解决“谁在访问、带什么角色”；mTLS 证明服务/Connector 身份，解决“哪一个服务或内网节点在通信”；gRPC 解决 Bridge、Agent、Connector 之间的结构化调用和长期双向流。它们分别保护用户身份、服务身份和通信方式，不能互相替代。

### 追问“Agent 为什么不是普通代理”

> 因为它在转发前执行认证、策略、路由和隧道健康检查。它既是策略执行点，也是 Connector 请求与响应的关联调度点；普通代理只要找到上游就可以转发，Agent 必须先决定是否允许转发。

---

## 9. 可直接交给 AI 的讨论问题

1. 当前 `resolve_user_identity()` 在认证服务已配置但缺少 Bearer Token 时返回调用方身份 Header，这是否构成绕过？请仅基于代码的所有可达路径分析。
2. 如何为当前共享密钥 JWT 方案增加 issuer、audience、算法白名单、key rotation 和用户级撤销，同时尽量少改现有接口？
3. 把 JWT 从 localStorage 改为 HttpOnly Secure SameSite Cookie，会怎样影响 Next rewrite、SSO 回调和 Zentinel 数据平面请求？
4. `sag-policy /evaluate` 没有自行验证 JWT。部署上应如何限制网络访问，或者该接口是否应加入服务间认证？
5. Agent 的 stale auth / stale ALLOW 降级策略在认证和策略服务故障时的安全边界是什么？哪些情况必须 fail closed？
6. Agent 采用“route table + connector registry + request_id one-shot”的模式。高并发、断流、重复 response、request_id 冲突时有哪些正确性风险？
7. 当前 mTLS 的 server 端在 client CA 文件不可读时仍可启动。如何将这一情况改为 fail-fast，并如何做启动时证书自检？
8. 需要怎样的端到端测试证明：无 JWT、过期 JWT、错误角色、策略拒绝、Connector 断线、APISIX 5xx、mTLS 证书错误都按预期失败？

---

## 10. 本文对应的主要源码

| 主题 | 文件 |
|---|---|
| JWT claim、签发、验证、本地/SSO 登录 | `services/sag-auth/src/main.rs` |
| 4A/OIDC 授权码流程 | `services/sag-auth/src/foura.rs` |
| 控制面 JWT 管理权限 | `services/control-plane-admin/src/main.rs` |
| 策略管理 JWT 权限、策略判断 | `services/sag-policy/src/main.rs` |
| gRPC 契约 | `shared/tunnel-proto/proto/tunnel.proto` |
| Proto Rust 生成入口 | `shared/tunnel-proto/build.rs`、`shared/tunnel-proto/src/lib.rs` |
| Agent gRPC server、认证/授权/转发 | `proxy/agents/stealth-tunnel-agent/src/grpc_server.rs` |
| Agent 配置、TLS、HTTP 路由同步 | `proxy/agents/stealth-tunnel-agent/src/config.rs`、`main.rs`、`manager.rs` |
| Connector 建隧道、心跳、HTTP 转 APISIX | `proxy/connectors/sag-connector/src/main.rs` |
| Bridge HTTP 转 gRPC、client pool、过载处理 | `proxy/http-tunnel-bridge/src/main.rs`、`queue.rs`、`limits.rs` |
| 默认服务、端口、证书和环境变量 | `docker-compose.yml` |
| Zentinel 可见配置 | `proxy/zentinel-proxy/config/dataplane-compose.kdl` |

## 11. 待确认边界

- `proxy/core` 子模块源码在当前工作区为空，因此本文只依据 Zentinel 的 KDL 配置确认它“监听 HTTPS 并把请求发到 Bridge”；无法证明 Zentinel 内部是否还有额外 JWT、Header 清洗、限流或鉴权行为。
- 本文未启动整套 Compose 做真实握手和端到端验证；mTLS、服务发现、实际网络暴露范围应通过运行时验证补充。
- 文中所有“默认/Compose”描述只说明当前 `docker-compose.yml` 的编排，不等于生产环境已经使用相同配置。
