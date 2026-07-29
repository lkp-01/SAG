\# APISIX 内网层方案评估（重要策略变更）



Last updated: 2026-03-30



\## 1. 背景与目标



本次架构策略调整如下：



\- 用户侧与 `Zentinel` 之间新增 `Public Edge`（CDN/WAF/DDoS）以做通用互联网防护解耦。

\- 外网入口继续使用 `Zentinel`（零信任门卫、统一安全边界、可扩展 agent）。

\- 内网流量层引入 `APISIX`，用于提升 L7 路由能力、性能上限与插件生态。

\- 当前阶段不改代码，先完成可行性评估与迁移计划，作为后续开发蓝图。



\## 2. 当前链路基线（As-Is）



当前可运行数据面链路：



`Zentinel -> http-tunnel-bridge -> stealth-tunnel-agent -> sag-connector -> response`



规划后的全链路口径（汇报版）：



`Client -> PublicEdge -> Zentinel -> http-tunnel-bridge -> stealth-tunnel-agent -> connector -> APISIX(optional) -> Mesh(optional) -> Workload`



职责拆分（当前）：



\- `Zentinel`：外网 HTTPS 入口与网关策略边界

\- `http-tunnel-bridge`：HTTP 转 gRPC

\- `stealth-tunnel-agent`：路由门控、PDP/IAM 协同、隧道请求调度

\- `sag-connector`：注册/心跳/请求回包（当前为 echo 原型）



主要短板（当前）：



\- `sag-connector` 仍是原型实现，不是成熟内网流量分发中间件

\- 内网路由治理、插件生态（灰度、重写、观测、复杂流量策略）需要额外自研

\- 未来多租户、多应用规模化时，运维与能力扩展成本偏高



\## 3. 目标链路（To-Be）



目标链路建议：



`Zentinel -> http-tunnel-bridge -> stealth-tunnel-agent -> APISIX -> intranet apps`



职责拆分（目标）：



\- `Zentinel`：外网暴露、零信任边界、防护与统一入口

\- `stealth-tunnel-agent`：身份/策略门控与转发编排（保留最终鉴权决策点）

\- `APISIX`：内网 L7 流量治理（上游负载、重试、超时、熔断、插件扩展）

\- `connector`：从“流量分发者”收敛为“连接与接入侧组件”（可演进为轻量隧道/sidecar 职责）



\### 3.1 分层责任矩阵（表格版）



与 mentor 白板及「业界分层」讨论对齐：\*\*每层职责单一，授权不重复裁决\*\*。



| 层级/组件 | 主要职责 | 明确不负责 | 与 IAM / PDP | 可选平替 |

|-----------|----------|------------|--------------|----------|

| \*\*Public Edge\*\* | CDN 缓存、通用 WAF、DDoS 缓解 | 最终业务授权 | 仅做通用风险拦截，不替代 PDP | 云厂商 CDN+WAF 一体化、Cloudflare |

| \*\*Zentinel\*\* | 外网边界、TLS、零信任准入、粗粒度路由 | 互联网规模高防与 CDN 产品化能力 | 可接 Keycloak/OPA/SPIRE；业务策略仍以 `sag-policy` 为 PDP | 作为核心自研能力保留 |

| \*\*http-tunnel-bridge\*\* | HTTP 与 gRPC `Forward` 适配 | 鉴权与策略裁决 | 透传 `Authorization`、`x-sag-\*` 到 Agent | - |

| \*\*stealth-tunnel-agent\*\* | 隧道路由、健康、调度、mTLS 到 connector；数据面门控 | 替代 APISIX 的全部 L7 产品化能力 | 调用 `sag-policy`；可选 `sag-auth/verify` | - |

| \*\*sag-connector（目标）\*\* | 私网侧隧道落点；与 APISIX 相邻部署 | 大流量路由引擎 | 仅转发 Agent 已允许之请求 | Cloudflare Tunnel、云专线 |

| \*\*APISIX（可选）\*\* | 内网 L7：路由、上游、重试、熔断、插件、观测 | 默认不实现与 `sag-policy` 冲突的第二套最终授权 | 位于 Agent 门控之后 | 轻量可选 Traefik；无产品化诉求可省略 |

| \*\*Ambient Mesh（可选）\*\* | 东西向 L4 mTLS 与 L7 治理 | 替代北南向网关 | 与网关策略互补 | Istio Ambient、Cilium |

| \*\*Workload\*\* | 业务服务、消息队列、数据库、AI 推理服务 | 边界安全控制平面 | 消费上层身份/策略结果 | 按业务与团队栈选型 |



\*\*目标路径一句话\*\*：`Client -> PublicEdge -> Zentinel -> bridge -> stealth-tunnel-agent -> connector -> APISIX(optional) -> Mesh(optional) -> Workload`。



\## 4. 可行性结论



结论：\*\*Conditional Go（有条件可行）\*\*



可行前提：



1\. 明确“最终授权判定点”仍在 `stealth-tunnel-agent + sag-policy`，避免策略分散与绕行。

2\. 控制面数据模型允许从 `connector\_endpoint` 演进为 `intranet\_upstream` 抽象（或兼容双字段）。

3\. 先双轨灰度，保留回退路径，不做一次性替换。

4\. 先验证 APISIX 在目标流量特征下的性能收益与运维复杂度净值。



不建议做法：



\- 在 APISIX 与 agent 双边同时做“最终授权决策”，会导致策略漂移和排障复杂度激增。



\## 5. 迁移路线（不改代码阶段制定）



\### Phase 0：评估定稿



\- 固化现网基线（成功率/延迟/错误码分布/故障模式）

\- 输出 ADR（Architecture Decision Record）：

&#x20; - 目标、范围、非目标

&#x20; - 风险与回滚策略

&#x20; - 责任边界（谁做最终授权）



\### Phase 1：旁路验证（PoC）



\- 不动主链路，搭建 APISIX 沙箱与样例内网服务

\- 验证能力：

&#x20; - 路由、超时、重试、熔断、限流

&#x20; - 可观测（日志、指标、追踪）

&#x20; - 与现有 agent 链路兼容性



\### Phase 2：双轨灰度



\- 引入“按 app\_id 切换”的路由开关（旧链路与 APISIX 链路并存）

\- 小流量 app 先迁，逐批推进

\- 建立“可回退开关 + 回退剧本”



\### Phase 3：收敛与替换



\- 稳定后收敛 connector 分发职责

\- 更新控制面 API 契约、运维流程、故障演练清单

\- 输出正式版本设计文档



\## 6. 风险清单与规避



\- 策略一致性风险：

&#x20; - 规避：授权只在 agent+policy 判定，APISIX 做流量治理

\- 控制面兼容风险：

&#x20; - 规避：新增字段并保持旧字段兼容一个迁移周期

\- 双轨复杂度风险：

&#x20; - 规避：每批 app 有明确入口开关、观测项与回滚条件

\- 运维门槛风险：

&#x20; - 规避：PoC 先验证配置管理与自动化能力，再扩大范围



\## 7. 回滚策略（必须项）



\- 按 app\_id 回滚：从 APISIX 路由切回现有 connector 路由

\- 配置级回滚：保留前一版本配置快照（route/upstream/policy）

\- 发布级回滚：灰度窗口内支持一键恢复旧链路

\- 观测阈值触发回滚：

&#x20; - 错误率、P95 延迟、连接失败率超过阈值时自动回退



\## 8. 验收标准（进入代码实施前）



\- 安全：未出现授权绕过路径

\- 性能：在目标负载下达到预期收益（吞吐或延迟改进）

\- 稳定性：灰度期回滚路径可用且演练通过

\- 可运维性：配置管理、监控告警、排障路径清晰



\## 9. 对 11 模块的影响



\- 模块 1（统一网关）：定位不变，继续承担外网入口

\- 模块 5（API/Web 代理）：内网流量层新增 APISIX，代理拓扑升级

\- 模块 6（连接器与数据保护）：connector 职责从“分发”向“接入/隧道”收敛

\- 模块 11（基础设施与部署）：新增 APISIX 运维与发布能力建设



其余模块（2/3/4/7/8/9/10）接口契约原则上可保持，但需在集成阶段验证兼容性。



\## 10. 仓库内占位路径



\- APISIX 相关配置与说明占位：`infra/apisix/`

\- 分层与模块对照：`architecture/MODULE\_MAP.md`





