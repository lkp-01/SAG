# SAG 项目中的 APISIX 与 Zentinel：代码事实、边界与待确认问题

> 目的：这是一份用于面试准备和向项目维护者追问的技术说明。它刻意把“当前代码已经证明的事实”和“当前工作区无法证明的事项”分开；不要把待确认内容当作项目已实现的能力。
>
> 代码快照范围：`D:\developer\Secure_Access_Gateway_SAG-clean-main`。本文依据 `docker-compose.yml`、Zentinel KDL 配置、APISIX 配置、控制面下发 APISIX 路由的 Rust 代码、Connector 代码及 gRPC 协议编写；没有启动容器做运行验证。

---

## 0. 先给结论

这个项目在 HTTP 数据平面上使用了两层不同位置的网关/反向代理：

```text
用户请求
  -> Zentinel                 外部数据平面入口；统一转给 Bridge
  -> http-tunnel-bridge       HTTP 转 gRPC
  -> Stealth Tunnel Agent     验证身份、评估策略、选择 Connector
  -> gRPC 隧道
  -> sag-connector            将隧道消息还原为 HTTP
  -> APISIX                   按 app_id 路由到真实内网业务上游
  -> 内网应用
```

它们并非重复：

- **Zentinel** 处理“用户请求如何进入隧道数据平面”。
- **APISIX** 处理“请求已经到达内网后，应转到哪个 HTTP 业务上游”。

两者都转发 HTTP，但处于不同网络位置，路由依据不同，配置来源也不同。

---

## 1. 必须先分清的基础概念

### 1.1 什么是网关 / 反向代理？

这里的“网关”是一个角色，而不是某种唯一技术。它是一个位于两个网络或两个系统边界的软件：接收一侧的请求，依据规则选择拒绝、改写、记录或转发给另一侧。

当它接收客户端请求，并把请求送往后面的服务时，常被称为**反向代理**。用户只知道网关地址，不必知道后端服务的真实地址。

一个 HTTP 网关通常可以承担下面几类职责；某个具体网关是否真的实现，必须回到配置和代码确认：

| 职责 | 含义 |
|---|---|
| TLS 终止 | 客户端先与网关建立 HTTPS，网关再向后端转发 |
| 路由 | 根据路径、请求头、域名、方法等选择目标服务 |
| 反向代理 | 将请求和响应在客户端与后端服务之间转送 |
| 流量治理 | 超时、限流、负载均衡、重试、熔断等 |
| 可观测性 | 记录日志、暴露指标、追踪请求 |

### 1.2 什么是“七层 / L7”路由？

TCP/UDP 只知道端口；HTTP 网关能进一步理解 HTTP 的内容，例如：

```text
HTTP 方法：GET / POST
路径：/dev/
请求头：x-sag-app-id: app-001
```

根据这些 HTTP 字段做判断和转发，就是这里所说的七层（L7）路由。

### 1.3 什么是 upstream（上游）？

对于一个代理而言，**上游**就是它接下来要把请求转发给的目标服务地址。例如：

```text
Zentinel 的上游：http-tunnel-bridge:9000
APISIX 的上游：某个 app_id 对应的内网业务地址
```

“上游”只是代理视角下的下一跳，不等于最终用户，也不必等于最终业务应用。

### 1.4 什么是控制面和数据面？

本项目可这样划分：

| 平面 | 主要职责 | 本项目的组件 |
|---|---|---|
| 控制面 | 管理配置：某应用对应什么上游、应下发什么路由 | `control-plane-admin`、数据库、APISIX Admin API |
| 数据面 | 实际处理每一个用户请求 | Zentinel、Bridge、Agent、Connector、APISIX |

控制面通常不在每个用户请求中出现；它提前把规则写好。数据面则按这些规则处理用户的真实流量。

---

## 2. Zentinel：当前项目中能确认什么？

### 2.1 它在项目中是什么

在当前项目中，Zentinel 是名为 `zentinel-proxy` 的**数据平面 HTTP/HTTPS 代理程序**。Docker Compose 使用以下方式启动它：

```text
cargo run --manifest-path /workspace/proxy/core/Cargo.toml \
  -p zentinel-proxy --bin zentinel -- \
  --config /workspace/proxy/zentinel-proxy/config/dataplane-compose.kdl
```

也就是说，`zentinel` 是一个 Rust 二进制程序；`dataplane-compose.kdl` 是它的运行配置。它不是 HTTP、gRPC、APISIX 之类的协议或通用标准名称。

### 2.2 可由配置直接证实的运行行为

配置文件：`proxy/zentinel-proxy/config/dataplane-compose.kdl`。

| 项目 | 配置事实 | 含义 |
|---|---|---|
| 客户端监听 | `0.0.0.0:10080`，协议 `https` | Zentinel 接收 HTTPS 数据平面请求 |
| TLS | 引用 `infra/tls/server-default.crt/key`，最低 `TLS1.2` | 它在该入口配置了 TLS |
| 路由匹配 | `path-prefix "/"` | 当前所有路径都匹配同一条 `dataplane-api` 路由 |
| 上游 | `http-tunnel-bridge:9000` | 所有已匹配请求都被转发给 Bridge |
| 路由超时 | `90` 秒，`failure-mode "closed"` | 配置了超时和关闭式失败策略；该策略的精确运行语义需结合 Zentinel 源码确认 |
| 请求限制 | 头部最大 16 KiB，Body 最大 10 MiB | 入口层做了大小限制配置 |
| 指标 | 内部 `:9090/metrics` | 为 Prometheus 提供指标暴露点 |
| 日志 | `info`、文本格式 | 启用信息级别文本日志 |

从这份配置可以准确得到的请求路径是：

```text
浏览器/前端
  -- HTTPS --> Zentinel :10080
  -- HTTP  --> http-tunnel-bridge :9000
```

### 2.3 Zentinel 在本项目中“不负责什么”

从当前可见 KDL 配置**没有**看到下列规则：

- 没有可见的 JWT 验证配置；
- 没有可见的业务授权策略配置；
- 没有按 `x-sag-app-id` 选择不同内网应用的配置；
- 没有可见的用户身份到角色的映射配置。

因此不应说“Zentinel 在当前项目中负责最终鉴权或最终业务应用路由”。这两件事分别在 Agent/策略服务和 APISIX 路由逻辑中实现。

### 2.4 当前必须保留的待确认项

当前工作区中的 `proxy/core` 目录为空，缺少 Compose 启动命令引用的 `proxy/core/Cargo.toml` 及 Zentinel 源码。因此无法仅凭该工作区证明下列内部细节：

- `failure-mode "closed"` 在 Zentinel 内部具体返回哪个 HTTP 状态和响应体；
- 它如何建立、复用、超时或重试到 Bridge 的 HTTP 连接；
- 它是否会删除、增加或改写任何 HTTP 请求头；
- 它是否在实现层面还有未写在当前 KDL 中的认证、限流或安全逻辑；
- 它的指标名称、日志字段、性能模型和并发模型；
- KDL 中的 `weight=1` 与 `round_robin` 在仅一个上游时的具体执行细节。

**给维护者的直接问题：**“请提供当前部署的 `proxy/core` 提交版本和 `zentinel-proxy` 源码，尤其是 KDL 的 `failure-mode`、请求头处理、上游连接和错误返回实现。”

---

## 3. APISIX：当前项目中能确认什么？

### 3.1 它在项目中是什么

APISIX 在此项目中是一台独立运行的 **HTTP API 网关 / 反向代理服务**。Compose 使用镜像 `apache/apisix:3.10.0-debian` 启动它，并启动 etcd 作为它的配置提供者。

这意味着 APISIX 不是项目里的 Rust 模块，而是外部网关产品的容器化实例。项目自己编写的 Rust 控制面通过 APISIX 的 Admin API 向它写入路由。

基础配置位于 `infra/apisix/config.yaml`：

| 项目 | 配置事实 |
|---|---|
| HTTP 数据面端口 | `9080` |
| Admin API 端口 | `9180` |
| 配置提供者 | etcd |
| etcd 地址 | `http://etcd:2379` |
| 启用插件 | `prometheus`、`proxy-rewrite` |
| 指标导出 | `:9091/apisix/prometheus/metrics` |

注意：本文不记录 Compose 中的开发密钥或任何真实凭据；部署时应由安全的环境变量或密钥管理系统提供。

### 3.2 APISIX 在这条链路中的位置

Connector 明确要求配置 `SAG_APISIX_BASE_URL`。它收到 Agent 下发的 `ForwardRequest` 后，将 gRPC 请求还原为 HTTP，并向该地址发起请求：

```text
Agent
  -- 双向 gRPC 隧道 --> Connector
  -- HTTP --> APISIX :9080
  -- HTTP --> 内网业务上游
```

因此，APISIX 接收到的不是浏览器的最初 TCP 连接，而是 Connector 发起的 HTTP 请求。Connector 会保留大多数原始 HTTP 请求头；它会去掉 `Host`、`Content-Length` 以及 HTTP hop-by-hop 头。

### 3.3 谁把路由规则写入 APISIX？

`services/control-plane-admin/src/apisix.rs` 中的 `sync_app_route()` 做了这件事：

1. 控制面从数据库读取 `intranet_upstreams` 中某个 `app_id` 的上游地址；
2. 构造一份 APISIX Route JSON；
3. 通过 HTTP `PUT /apisix/admin/routes/{route_id}` 写入 APISIX Admin API；
4. 带上 Admin API 所需的认证头；
5. 控制面启动时会全量同步，后续也会周期性重对齐；更新某项路由/上游时也会触发同步。

路由配置并不是由 Connector 自动发现和写入，而是由控制面基于数据库配置下发。

### 3.4 项目实际下发给 APISIX 的路由语义

对于一个应用 `app_id`，控制面生成的路由含义如下：

```text
匹配条件：
  - URI：/*
  - HTTP 方法：GET、POST、PUT、PATCH、DELETE、HEAD、OPTIONS
  - HTTP 请求头 x-sag-app-id 必须等于该 app_id

匹配后：
  - 代理到该 app_id 配置的 intranet_upstreams.upstream
  - 上游协议按配置使用 http 或 https
  - 启用 Prometheus 插件
  - 对 /api/<name> 做正则路径改写为 /<name>/
```

例如，假设控制面保存：

```text
app_id = app-001
upstream = company-service.internal:8080
scheme = http
```

则 Connector 传来下列 HTTP 请求时：

```http
GET /dev/
x-sag-app-id: app-001
```

APISIX 的职责是匹配 `app-001` 对应路由，然后把请求转给 `http://company-service.internal:8080/dev/`。

这里的“应用隔离键”是 **`x-sag-app-id` 请求头**，不是 `Host`。控制面代码刻意没有将 Host 绑定为匹配条件，以避免开发/Compose 环境中 Host 不一致导致 404。

### 3.5 APISIX 在本项目中不承担最终授权

控制面代码的注释明确表达：APISIX 做 L7 流量治理，最终授权仍在 Agent + PDP（即 `sag-policy`）完成。

实际顺序是：

```text
Agent 先调用 sag-auth 验证 JWT
Agent 再调用 sag-policy 评估 ALLOW / DENY
只有通过后，Agent 才把请求发给 Connector
Connector 才会请求 APISIX
```

因此，当前 APISIX Route 的 `x-sag-app-id` 匹配是**应用路由条件**，不是用户授权判断。不要把“请求带 `app_id`”误讲成“APISIX 在判断用户是否有权访问”。

### 3.6 APISIX 的已知边界与待确认项

当前项目配置和控制面代码可确认路由、Prometheus、路径改写和 etcd 配置；但下列问题仍需要在真实环境或 APISIX 运行态确认：

- APISIX Admin API 是否真的成功下发了每条 Route；
- etcd 中运行时是否存在被其他部署步骤覆盖的 Route；
- 每个真实内网 `upstream` 是否 DNS 可达、端口可达、TLS 证书有效；
- 多个 `app_id` 路由都匹配 `/*` 时，最终的优先级、Header 匹配和回退行为是否符合预期；
- APISIX 运行态是否还加载了当前仓库未列出的全局插件或额外路由；
- `proxy-rewrite` 对业务应用真实路径、查询参数和重定向响应是否完全符合预期。

---

## 4. Zentinel 和 APISIX 的逐项对比

| 维度 | Zentinel | APISIX |
|---|---|---|
| 在当前系统的位置 | 外部数据平面入口 | 内网侧业务路由入口 |
| 前一跳 | 浏览器、前端代理或外部调用方 | `sag-connector` |
| 后一跳 | `http-tunnel-bridge:9000` | `intranet_upstreams` 中配置的业务上游 |
| 当前主要路由依据 | 所有路径 `/` 前缀都进入同一个 Bridge 上游 | `x-sag-app-id` 与 HTTP 路径/方法 |
| 是否处理 HTTP | 是 | 是 |
| 是否将 HTTP 转 gRPC | 否；它转发给 Bridge | 否；Connector 已把 gRPC 还原成 HTTP |
| 是否执行最终用户授权 | 当前可见配置不能证明，且主链路授权在 Agent/PDP | 否，项目代码明确将最终授权放在 Agent/PDP |
| 配置来源 | `dataplane-compose.kdl` | `infra/apisix/config.yaml` + 控制面动态 Admin API 下发 |
| 源码可用性 | 当前快照缺 `proxy/core`，内部实现待确认 | APISIX 本体为外部镜像；项目对它的配置和下发代码可见 |
| 已知观测能力 | `:9090/metrics` 配置 | Prometheus 插件和 `:9091` 导出配置 |

一句话区别：

> Zentinel 是把用户 HTTP 请求送进安全隧道数据平面的入口代理；APISIX 是把从隧道出来的 HTTP 请求送到正确内网业务应用的路由代理。

---

## 5. 用一个具体请求走完整流程

假设用户访问 `app-001` 的 `/dev/`：

```text
1. 用户请求
   GET /dev/
   Authorization: Bearer <JWT>
   x-sag-app-id: app-001

2. Zentinel
   - HTTPS 接收请求
   - 因 /dev/ 匹配 path-prefix /，转发给 http-tunnel-bridge:9000

3. Bridge
   - 读取 HTTP 方法、路径、头、Body
   - 生成 ForwardRequest(request_id, app_id, method, path, headers, body)
   - 调用 Agent 的 gRPC Forward

4. Agent
   - 调 sag-auth 验证 JWT
   - 调 sag-policy 判断此用户能否访问 app-001 的 /dev/
   - 查 app-001 对应的健康 Connector
   - 沿已建立的双向 gRPC 流下发 ForwardRequest

5. Connector
   - 收到 gRPC 消息
   - 把 ForwardRequest 还原为 HTTP 请求
   - 转发到 SAG_APISIX_BASE_URL，即 APISIX

6. APISIX
   - 查看 x-sag-app-id: app-001
   - 匹配控制面写入的 app-001 Route
   - 转发到 app-001 对应的内网 upstream

7. 响应
   内网应用 -> APISIX -> Connector -> Agent -> Bridge -> Zentinel -> 用户
```

一个容易忽略但很重要的事实：**同一个 `x-sag-app-id` 从浏览器请求一路携带到 APISIX。**Agent 用它选择隧道；APISIX 用它选择业务上游。但用户访问权不是由这个 Header 本身决定，而是由 Agent 调用的认证与策略服务决定。

---

## 6. 用于向他人请教的提问清单

### 6.1 先问 Zentinel 维护者

1. `proxy/core` 的正确提交、获取方式和构建前置条件是什么？当前仓库为何缺失其源码？
2. `failure-mode "closed"` 对上游超时、连接失败、上游 5xx 分别返回什么？
3. Zentinel 是否改写、过滤或新增请求头？`Authorization` 和 `x-sag-app-id` 是否原样向 Bridge 传递？
4. 到 Bridge 的 HTTP 连接是否使用连接池？请求超时和上游重试如何实现？
5. 是否存在未写入当前 KDL 的全局鉴权、限流、WAF、mTLS 或 Header 信任规则？
6. Zentinel 的路由优先级、负载均衡、健康检查和故障转移是如何工作的？
7. 为什么它被选为入口而不是直接让客户端访问 Bridge？它提供了哪些 Bridge 不提供的能力？

### 6.2 再问 APISIX / 运维维护者

1. 在运行环境中，控制面是否已成功向 `:9180` 下发路由？如何查看某个 `sag-route-{app_id}`？
2. `x-sag-app-id` 是如何保证由可信链路提供的？是否允许外部调用者伪造？
3. 多个应用都匹配 `/*` 时，是否始终能由 Header 条件正确区分？没有 Header 时应返回什么？
4. 内网 upstream 是服务发现地址、固定 IP、容器 DNS 还是负载均衡地址？其健康检查在哪里实现？
5. 为什么 Connector 必须先到 APISIX，而不是直接访问业务 upstream？需要 APISIX 提供哪些能力？
6. APISIX 路由的生命周期如何管理：应用删除、上游变更和旧 Route 清理分别怎样处理？
7. 生产环境的 Admin API 是否限制来源、轮换密钥，并禁止暴露到公网？

---

## 7. 可直接用于面试的 40 秒说明

> 项目把 HTTP 数据平面拆成了入口网关和内网应用网关两层。Zentinel 是入口侧的 HTTPS 代理：当前配置中所有请求都被统一转发到 HTTP-to-gRPC Bridge。Bridge 将 HTTP 请求封装成 gRPC 的 `ForwardRequest` 交给 Agent；Agent 先验证 JWT、评估策略，再通过已有的 Connector 隧道把请求送进内网。Connector 将请求恢复为 HTTP 并调用 APISIX。APISIX 根据控制面下发的规则，用 `x-sag-app-id` 把请求路由到对应业务上游。换言之，Zentinel 负责进入隧道，APISIX 负责从隧道出来后选择业务应用；最终授权不在它们中间完成，而在 Agent 加策略服务完成。

---

## 8. 关键代码和配置定位

| 文件 | 应阅读的原因 |
|---|---|
| `docker-compose.yml` | Zentinel、APISIX、etcd、Bridge、Connector 的启动命令、端口和环境变量 |
| `proxy/zentinel-proxy/config/dataplane-compose.kdl` | 当前可见的 Zentinel 监听、TLS、路由、上游、限制和指标配置 |
| `infra/apisix/config.yaml` | APISIX 数据面、Admin API、etcd、插件和 Prometheus 基础配置 |
| `services/control-plane-admin/src/apisix.rs` | 项目如何通过 Admin API 创建/更新 APISIX Route |
| `services/control-plane-admin/src/main.rs` | 路由和内网上游管理 API；启动和周期性 APISIX 重对齐 |
| `proxy/http-tunnel-bridge/src/main.rs` | HTTP 如何变成 gRPC `ForwardRequest` |
| `shared/tunnel-proto/proto/tunnel.proto` | Agent 和 Connector 间的 gRPC 服务与消息定义 |
| `proxy/agents/stealth-tunnel-agent/src/grpc_server.rs` | Agent 如何认证、授权、选择 Connector 并下发请求 |
| `proxy/connectors/sag-connector/src/main.rs` | Connector 如何建立隧道、接收请求、转发到 APISIX |

