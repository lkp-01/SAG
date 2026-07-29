# 一次公网 HTTPS 请求到真实业务的完整链路

适用范围：按 docker-compose.edge.yml 和 docker-compose.intra.yml 的双机同步路径说明。
主图假定请求没有进入 Redis 过载队列；过载分支在图的底部单列。

~~~text
┌──────────────────────────── 前置准备：不是每个请求都会重复执行 ─────────────────────────────┐
│                                                                                              │
│  [控制面] --每 5 秒同步--> [隧道代理内存路由表]                                              │
│       │                        app_id -> connector_endpoint + require_healthy_tunnel        │
│       │                                                                                      │
│       └--APISIX Admin API 下发--> [APISIX 路由]                                               │
│                                      匹配：x-sag-app-id == app_id                            │
│                                      上游：intranet_upstreams[app_id]                        │
│                                                                                              │
│  [内网连接器] --主动发起 mTLS 双向流--> [隧道代理 :50051]                                   │
│       ├─ 首个消息：注册 connector_endpoint                                                    │
│       └─ 后续消息：每 2 秒发送一次心跳                                                       │
│                                                                                              │
│  隧道代理按最近一次心跳判断健康；默认健康窗口为 120 秒。                                     │
└──────────────────────────────────────────────────────────────────────────────────────────────┘


┌────────────────────────────── 单请求正向链路：同步转发 ───────────────────────────────┐
│                                                                                        │
│  [门户 / 外部客户端]                                                                   │
│     HTTPS GET|POST /业务路径                                                           │
│     Authorization: Bearer <token>                                                      │
│     x-sag-app-id: <app_id>                                                             │
│     │                                                                                  │
│     │ ① HTTPS                                                                          │
│     v                                                                                  │
│  [公网 HTTPS 入口 :10080]                                                              │
│     - 配置 TLS 最低版本为 TLS 1.2                                                      │
│     - 全路径 / 匹配；失败模式为 closed                                                  │
│     - 上游固定为 HTTP 桥 :9000                                                        │
│     │                                                                                  │
│     │ ② 原始 HTTP 请求                                                                 │
│     v                                                                                  │
│  [HTTP 桥 :9000]                                                                       │
│     - 缺少 x-sag-app-id -----------------------------------------------> HTTP 400      │
│     - 采集：method、path + query、普通请求头、完整 body                                │
│     - 删除：connection / transfer-encoding / upgrade 等逐跳头                          │
│     - 完整读取 body；默认上限 1 MiB                                                     │
│     - 新建 request_id = UUID（用于隧道内请求/响应配对）                                  │
│     - 封装为：{request_id, app_id, method, path, headers, body}                         │
│     │                                                                                  │
│     │ ③ mTLS 单次转发调用                                                              │
│     │    双机配置：桥等待上限 60 秒；首次失败会重连并再试 1 次                          │
│     v                                                                                  │
│  [隧道代理 :50051]                                                                     │
│     │                                                                                  │
│     ├─ A. 身份解析（正式 edge 配置已启用认证服务）                                      │
│     │      Authorization token --POST--> [认证服务]                                    │
│     │      <-- active + user_id + roles --                                             │
│     │                                                                                  │
│     ├─ B. 策略裁决（正式 edge 配置已启用策略服务）                                      │
│     │      {user_id, roles, app_id, path, method} --POST--> [策略服务]                  │
│     │      实际匹配：用户/角色 + app_id + path_prefix；未命中默认拒绝                    │
│     │                                                                                  │
│     ├─ C. 按 app_id 查 connector_endpoint                                              │
│     │      若路由要求健康：最近心跳必须在 120 秒窗口内                                  │
│     │                                                                                  │
│     └─ D. pending[request_id] = 一次性通道                                              │
│               然后向已建立的连接器双向流写入请求                                       │
│               │                                                                        │
│               │ ④ 同一条 mTLS 双向流中的 Request 消息                                  │
│               v                                                                        │
│  [内网连接器]                                                                           │
│     - 先 try_send 到有界接收队列；有容量才并发处理                                      │
│     - 重建 HTTP：保留 method/path/body/普通头                                           │
│     - 删除 Host、Content-Length 和逐跳头                                                │
│     - 内网 HTTP 超时：55 秒                                                            │
│     │                                                                                  │
│     │ ⑤ HTTP                                                                           │
│     v                                                                                  │
│  [APISIX :9080]                                                                        │
│     - 匹配：x-sag-app-id == 当前 app_id，uri = /*                                      │
│     - 兼容重写：/api/xxx 或 /api/xxx/  ->  /xxx/                                       │
│     - 上游地址来自 intranet_upstreams[app_id]                                          │
│     │                                                                                  │
│     │ ⑥ HTTP 到配置的真实内网上游                                                      │
│     v                                                                                  │
│  [真实业务服务]                                                                        │
│     - app-001 的演示映射：mock-workload:18080                                          │
│     - 生产业务由 intranet_upstreams 配置地址和协议，具体实现不在本仓库                  │
│                                                                                        │
└────────────────────────────────────────────────────────────────────────────────────────┘


┌──────────────────────────── 响应回程：用同一个 request_id 对齐 ────────────────────────────┐
│                                                                                            │
│  [真实业务服务]                                                                            │
│       │ HTTP response：status + headers + body                                             │
│       v                                                                                    │
│  [APISIX]                                                                                  │
│       │                                                                                    │
│       v                                                                                    │
│  [内网连接器]                                                                              │
│       - 完整读取响应 body，过滤逐跳响应头                                                  │
│       - 封装：{request_id = 原 request_id, status_code, headers, body}                    │
│       │                                                                                    │
│       │ ⑦ 同一条 mTLS 双向流中的 Response 消息                                             │
│       v                                                                                    │
│  [隧道代理]                                                                                │
│       - 从 pending[request_id] 取出一次性通道并投递响应                                    │
│       - 删除 pending[request_id]，避免等待项残留                                           │
│       │                                                                                    │
│       v                                                                                    │
│  [HTTP 桥] --还原 HTTP status / 普通响应头 / body--> [公网 HTTPS 入口] --> [客户端]       │
│                                                                                            │
└────────────────────────────────────────────────────────────────────────────────────────────┘


┌──────────────────────────────────── 异常与分支 ────────────────────────────────────┐
│                                                                                      │
│  身份解析失败、身份缺失、明确策略拒绝 -------------------------------> 403            │
│  策略服务调用失败/超时：有旧 ALLOW 结果可降级；没有则 ----------------> 503            │
│  app_id 没有隧道路由 -----------------------------------------------> 502            │
│  路由要求健康但心跳过期 --> 隧道代理 unavailable                                   │
│                                  └─ HTTP 桥两次转发均失败后 ----------> 502            │
│  连接器接收队列满 -----------------------------------------------> 503                │
│  连接器访问 APISIX / 真实业务连接失败 ---------------------------> 502                │
│                                                                                      │
│  【过载分支，未画入主线】                                                             │
│  edge 配置中同步在途请求达到 24：HTTP 桥写 Redis 队列，先返回 202；                     │
│  达到 2048 硬阈值或队列满：返回 429。队列 worker 后续仍从第③步继续。                    │
│                                                                                      │
│  【超时阶梯】                                                                          │
│  连接器访问内网：55 秒  <  隧道代理等连接器：58 秒  <  HTTP 桥：60 秒                  │
└──────────────────────────────────────────────────────────────────────────────────────┘
~~~

## 不能在面试中夸大的点

1. **公网 HTTPS 入口的内部实现未找到。** proxy/core 是外部子模块但当前为空；本仓库只能从入口配置确认监听、TLS 和到 HTTP 桥的路由，不能声称已审阅其具体代理实现。
2. **不是 WebSocket 或端到端流式代理。** HTTP 桥和连接器都过滤 upgrade，并将请求体、响应体整体读入内存后转发。
3. **“强制验 token”与代码不一致。** 配置注释说启用认证服务后不应信任调用方身份头；但认证端点已配置而请求没有 Authorization 时，代码仍会返回调用方给的用户和角色。不能把当前实现说成“每个请求都严格校验 token”。
4. **不支持方法级授权。** 策略请求和缓存键带了 HTTP method，但策略记录和匹配函数只检查用户/角色、应用和路径；不能说“GET 和 POST 能配不同权限”。
5. **真实生产业务的实现不在仓库里。** 网关只把 intranet_upstreams[app_id] 的地址作为最终目标；仓库可运行的示例是 mock-workload:18080。

## 源码查证索引

- 公网 HTTPS 监听、全路径到 HTTP 桥、失败关闭：[dataplane-compose.kdl](../proxy/zentinel-proxy/config/dataplane-compose.kdl) 第 5–14、26–40 行。
- 外部子模块声明：[.gitmodules](../.gitmodules) 第 1–4 行。
- HTTP 桥校验应用编号、采集请求、创建 request ID：[main.rs](../proxy/http-tunnel-bridge/src/main.rs) 第 621–678 行；逐跳头过滤见第 166–197 行。
- HTTP 桥 mTLS 与两次调用：[main.rs](../proxy/http-tunnel-bridge/src/main.rs) 第 470–525、877–907 行。
- 代理认证、策略、路由与等待响应：[grpc_server.rs](../proxy/agents/stealth-tunnel-agent/src/grpc_server.rs) 第 248–399、655–795 行。
- request ID 与一次性通道：[connector_registry.rs](../proxy/agents/stealth-tunnel-agent/src/connector_registry.rs) 第 42–99 行。
- 连接器注册、心跳、双向流收发和 HTTP 转发：[main.rs](../proxy/connectors/sag-connector/src/main.rs) 第 164–207、325–391、394–479 行。
- APISIX 路由下发、应用隔离、路径重写和上游：[apisix.rs](../services/control-plane-admin/src/apisix.rs) 第 40–108 行。
- app-001 演示上游：[bootstrap_app001_dualhost_postgres.sql](../infra/storage-seed/bootstrap_app001_dualhost_postgres.sql) 第 7–18 行。
- 两处不一致：身份头回退 [grpc_server.rs](../proxy/agents/stealth-tunnel-agent/src/grpc_server.rs) 第 470–500 行；方法未参与策略匹配 [main.rs](../services/sag-policy/src/main.rs) 第 38–64、358–419 行。
