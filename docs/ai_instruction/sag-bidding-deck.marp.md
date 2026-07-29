---
marp: false
theme: default
paginate: true
size: 16:9
style: |
  section {
    background: linear-gradient(180deg, #f8fafc 0%, #ffffff 40%);
    font-family: "Segoe UI", "Microsoft YaHei", sans-serif;
  }
  h1 { color: #0B2A5B; font-weight: 700; }
  h2 { color: #0B2A5B; border-bottom: 2px solid #2563eb; padding-bottom: 0.2em; }
  strong { color: #1e40af; }
  table { font-size: 0.85em; }
  footer { color: #64748b; font-size: 0.75em; }
footer: "SAG Cloud · Secure Access Gateway"
---

<!-- _class: lead -->
# SAG Cloud 安全接入网关
## Secure Access Gateway（零信任 · 内外网安全接入）

**客户**：{客户名称}  
**日期**：{汇报日期}  
**汇报**：{汇报人/部门}

---

## 汇报议程

1. **背景与痛点** — 内外网隔离下的接入与安全诉求  
2. **目标与范围** — Phase-1 交付边界（诚实表述）  
3. **方案总览** — 分层架构与标准数据面路径  
4. **身份与策略** — 认证、4A、PDP 与演示能力  
5. **可靠性与 TLS** — Fail-closed、SNI、证书治理  
6. **部署与验收** — Compose、端口、T1/N1 探针  
7. **差异化与路线** — PoC 价值与后续演进  

---

## 背景与痛点

| 痛点 | 说明 |
|------|------|
| **暴露面** | 内网 API 直接映射公网，攻击面与合规风险高 |
| **身份脱钩** | “能进内网” ≠ “有权访问某应用/API” |
| **策略分散** | 网关、应用、身份各自策略，难统一运维审计 |
| **联调周期长** | 甲方 4A 未开放时，方案难端到端演示 |
| **运维盲区** | 进程存活无法代表隧道、TLS、路由真正可用 |

**本页结论**：需要 **统一接入面 + 身份策略闭环 + 可验收观测**。

---

## 建设目标与当前范围

**建设目标（客户视角）**

- 北向 **HTTPS** 统一入口，南向经 **加密隧道** 投递内网  
- **身份**（sag-auth）与 **策略 PDP**（sag-policy）可组合闭环  
- 内网 **L7 标准路径经 APISIX**，便于治理与扩展  
- **Docker Compose** 交付 + **冒烟与 T1/N1** 业务探针验收  

**当前阶段：Phase-1（原型 / PoC 基线）**

- 单机可运行全链路 + **双机 edge/intra 模板**  
- 运维控制台（Next）+ Prometheus/Grafana 观测基线  
- **非完整商用产品线** — 部分模块为规划占位，汇报时如实标注  

**本页结论**：适合 **技术验证、立项汇报、迭代产品化**，不夸大“已生产商用”。

---

## 方案总览（一页读懂）

**一句话**  
分层清晰的 **零信任安全接入网关原型**：边界 TLS + 隧道 + 策略身份 + 内网 L7 + 控制面与观测。

**五大分层关键词**

| 分层 | 代表组件 |
|------|----------|
| 边界接入 | **Zentinel**（北向 HTTPS） |
| 隧道 | **bridge** + **stealth-tunnel-agent**（gRPC / mTLS） |
| 策略与身份 | **sag-policy**（PDP）、**sag-auth** |
| 内网 L7 | **APISIX**（标准数据面路径） |
| 控制与观测 | **control-plane-admin**、Prometheus、Grafana |

**本页结论**：职责分离，**安全评审与分期建设**可落地。

---

## 标准数据面路径（核心）

```
用户 / 调用方
    → Zentinel（北向 HTTPS，TLS 终止）
        → http-tunnel-bridge（HTTP ↔ gRPC）
            → stealth-tunnel-agent（路由同步 · 策略前检）
                → sag-connector（内网连接器）
                    → APISIX（内网 L7 网关）
                        → 业务上游
```

**硬约束（与仓库一致）**

- `sag-connector` **必须**配置 `SAG_APISIX_BASE_URL`  
- 控制面路由中 **`connector_endpoint` 须与运行中 `SAG_CONNECTOR_ID` 一致**（如 `xxx:stream`）

**本页结论**：**全链路路径唯一、可画图、可逐段排障**。

---

## 控制面组成

| 组件 | 职责（各一行） |
|------|----------------|
| **control-plane-admin** | 隧道路由 CRUD、内网上游映射；可选向 APISIX 下发路由 |
| **sag-auth** | 登录、JWT、会话；可选 **4A / OIDC** SSO |
| **sag-policy** | 策略 CRUD；`/policy/evaluate` 作为 **PDP** |

**本页结论**：**路由、身份、策略** 三块控制面解耦、可独立演进。

---

## 身份与 SSO

- **本地认证**：用户名密码、Argon2、JWT  
- **甲方 4A**：OAuth2 **授权码模式**，配置齐全后启用 `/api/v1/auth/sso/*`（正式环境以联调为准）  
- **Fake 4A**（`infra/fake-4a`）：甲方接口未开放时，**完整浏览器链路演示**  
- **门户体验**：支持 **sso_token** 跳转登录；**访客预览** 展示未认证拦截（安全演示）  

**本页结论**：**可对接真实 4A，也可无依赖完成 PoC 演示**。

---

## 策略与零信任

- **PDP**：`sag-policy`，优先级化 ALLOW/DENY  
- **隧道层**：`stealth-tunnel-agent` 转发前可调用策略裁决  
- **门户**：按角色展示与策略预检（演示场景）  

**表述边界**：策略规则与角色模型需按甲方业务继续细化；当前为 **可运行原型能力**。

**本页结论**：**策略集中决策、多 enforcement 点可挂接**。

---

## 内网 L7：APISIX

- **标准约定**：内网流量 **经 APISIX** 再至上游（非连接器自行调度全链路）  
- **连接器**：`SAG_APISIX_BASE_URL` **必配** → 数据面通常 `:9080`  
- **控制面可选联动**：配置 APISIX Admin 后，可按 `app_id` 等 upsert 路由  

**本页结论**：**内网治理与 SAG 隧道解耦，沿用成熟 L7 网关生态**。

---

## 可靠性与 Fail-closed（已验证类）

| 场景 | 预期行为 |
|------|----------|
| 错误 **SNI**（如 `SAG_GRPC_TLS_SERVER_NAME`） | 证书名不匹配 → **链路失败**（fail-closed） |
| 错误 **隧道 endpoint** / DNS | 解析失败 → **链路失败** |
| **Agent 停止** | connector stream 中断 → **502 类错误** |
| 恢复正确配置与服务 | 链路可 **恢复到 200** |

**本页结论**：**故障可预期、可演练**，非“静默失败”。

---

## TLS / SNI / 证书治理

- **SNI 与 SAN**：客户端使用的名字须出现在服务端证书 **SAN**  
- **内网 gRPC**（测试证书场景）：常用 `SAG_GRPC_TLS_SERVER_NAME=localhost`  
- **北向 / 前端 → Zentinel**：访问主机名须与证书一致；Node 侧可配 `NODE_EXTRA_CA_CERTS` 信任 CA  
- **新服务器上线前**：建议 **openssl** 校验有效期、SAN、**证书与私钥匹配**（见 `DEPLOYMENT_README.md`）  

**本页结论**：**TLS 问题多在部署与命名不一致，用预检与探针前置发现**。

---

## 运维可观测

- **Prometheus + Grafana**：抓取管理面与数据面组件 metrics  
- **Zentinel**：`:9090/metrics`（与 proxy/core 指标体系衔接）  
- **管理端工作流**：除 Prometheus `up` 外，可用 **北向 N1 探针** 反映业务层 5xx（与进程存活区分）  

**本页结论**：**从“进程活着”升级到“链路真能通”**。

---

## 管理端与用户端

| 入口 | 说明 |
|------|------|
| **http://…:3001** | `frontend-admin-next`：工作流、指标、硬件视图、应用/API 树 |
| **/control** | 兼容旧版控制台能力（路由、策略、用户、探测等） |
| **http://…:5174** | `frontend-portal`：门户导航、策略预检、SSO 演示 |

**本页结论**：**运维侧与用户侧界面分离、各司其职**。

---

## 部署方式（Docker Compose）

- **主路径**：`docker compose build` → `docker compose up -d`  
- **Zentinel**：**默认服务**，无需额外 profile  
- **冷启动**：首次可能 **Rust 依赖编译较久**，属预期；**不等于配置滞后**（配置由 env / 控制面 / KDL 决定）  
- **双机**：`docker-compose.edge.yml` + `docker-compose.intra.yml` + `.env.dualhost.example`  

**本页结论**：**一键拉起 + 模板化双机**，降低交付摩擦。

---

## 主要端口一览（默认映射）

| 端口 | 用途 |
|------|------|
| 8090 | control-plane-admin |
| 8080 / 8081 | sag-auth / sag-policy |
| 9000 | http-tunnel-bridge |
| 10080 | Zentinel HTTPS |
| 9080 / 9180 | APISIX 数据面 / Admin |
| 9091 | Prometheus（宿主机映射，以 compose 为准） |
| 3000 / 3001 / 5174 | Grafana / 管理端 Next / 门户 |

**本页结论**：**安全组与防火墙按表开放，现场以实际 compose 为准**。

---

## 验收与探针（建议写进交付清单）

| 探针 | 典型 URL | 含义 |
|------|-----------|------|
| **T1** | `http://<host>:9000/api/test` | 经 **bridge** 的隧道向链路 |
| **N1** | `http://<host>:3001/api-zentinel/...` | 经管理端代理的 **北向** 探测 |

- **脚本**：`scripts/smoke-dataplane.ps1`（Windows）或 WSL 等价脚本  
- **判读**：若 **T1=200 而 N1=5xx**，优先查 **Zentinel TLS / SNI / 证书链**  

**本页结论**：**业务探针与冒烟脚本 = 可重复验收**。

---

## 双机部署要点

- 使用 **edge / intra** 分离模板，参数化地址与证书  
- **connector_endpoint** 必须与 **实际运行的 connector ID** 一致  
- **TLS、DNS、SNI** 跨机对齐；故障注入验证过 fail-closed 与恢复路径  

**本页结论**：**双机问题的首查项是“名字、证书、路由是否同一事实来源”**。

---

## 差异化优势（售前话术）

| 对比维度 | SAG 表述 |
|----------|----------|
| vs 纯 VPN | 在“能连内网”上叠加 **应用级策略与统一接入面** |
| vs 单一边界防护 | **边界 TLS + 隧道 + PDP + 内网 L7** 分段建设、分段评审 |
| vs 仅买网关 | **身份、策略、隧道、控制面、观测** 可 **端到端 PoC** |

**本页结论**：**可演示、可验收、可演进**，适合立项与技术标附件。

---

## 风险与后续路线（诚实）

- 部分能力为 **规划占位**（如部分终端、审计模块），**不写成已交付**  
- 无客户环境实测的 **性能数字**（QPS 等）**不编造**  
- **建议演进**：预编译镜像缩短冷启动；证书与密钥走 **密钥管理 + 轮换**；正式 **4A 联调**  

**本页结论**：**风险透明，路线可签进下一阶段工作说明书**。

---

## 下一步合作（占位）

- **PoC 范围**：（由售前填写：例如单机房全链路 + 双机模板验证）  
- **交付物**：（例如：部署包、验收脚本、演示账号、汇报材料）  
- **周期假设**：（由项目计划填写）  

**本页结论**：**本页数字须与合同/立项表一致后再对外**。

---

## 附录：术语速查

| 术语 | 含义 |
|------|------|
| Zentinel | 北向 HTTPS 接入（边界） |
| stealth-tunnel-agent | gRPC 隧道、路由同步、策略前检 |
| sag-connector | 内网连接器 → HTTP 至 APISIX |
| PDP | 策略决策点（主要为 sag-policy） |
| T1 / N1 | bridge 探针 / 北向（经管理端代理）探针 |

---

## Q&A / 致谢

**谢谢聆听。欢迎提问。**

**联系方式**：{电话 / 邮箱}  
**材料索引**：仓库 `docs/ai_instruction/product-report.md`、`DEPLOYMENT_README.md`

---
