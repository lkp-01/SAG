# SAG Cloud 安全接入网关 — 产品报告（竞标/汇报体）

> **文档性质**：技术方案与产品能力说明，供立项汇报、售前交流、内部评审使用。  
> **依据**：`README.md`、`DEPLOYMENT_README.md`、`Context_Handoff.md`（若与运行环境不一致，以部署文档与现场配置为准）。

---

## 1. 文档信息

| 项目 | 说明 |
|------|------|
| 产品名称 | SAG Cloud（Secure Access Gateway，安全接入网关） |
| 形态 | 可运行原型 + Docker Compose 交付基线；含双机（边缘/内网）部署模板 |
| 当前阶段 | Phase-1：单机构图 + 双机模板 + 运维控制台与观测基线（见 `Context_Handoff.md`） |

---

## 2. 背景与问题陈述

### 2.1 业务背景

政企与大型组织普遍采用 **内外网隔离**：内网 API、管理系统、数据服务不应直接暴露于公网。传统做法依赖防火墙与专线，但在 **远程办公、生态协作、云边协同** 场景下，仅网络隔离无法满足 **最小权限、身份可信、策略统一、可审计** 等要求。

### 2.2 典型痛点

| 痛点 | 说明 |
|------|------|
| 暴露面过大 | 内网服务若直接映射公网 IP/端口，攻击面与合规风险陡增 |
| 身份与网络脱钩 | 仅“能连上内网”不等于“有权访问某应用/某 API” |
| 策略分散 | 网关、应用、身份系统各自策略，难以统一运维与审计 |
| 联调周期长 | 甲方统一身份（如 4A）未开放接口时，方案难以端到端演示 |
| 运维不可见 | 仅进程存活无法反映隧道、TLS、路由是否真正可用 |

---

## 3. 建设目标（客户视角）

1. **统一安全接入**：北向 HTTPS 入口，南向经 **加密隧道** 投递至内网，避免内网裸暴露。  
2. **身份与策略闭环**：认证（`sag-auth`）与策略决策（`sag-policy`，PDP）可组合使用；隧道层转发前可策略裁决。  
3. **内网 L7 治理标准化**：数据面约定 **内网流量经 APISIX**（`sag-connector` 必配 `SAG_APISIX_BASE_URL`），便于路由、限流、观测与后续治理扩展。  
4. **可部署、可验收**：`docker compose` 主路径；冒烟脚本；**T1 / N1** 等业务探针支撑上线检查。  
5. **可演示**：Fake 4A 模拟 OAuth2 授权码流程；门户支持 SSO 跳转与访客拦截演示。

---

## 4. 产品定位与价值主张

### 4.1 一句话定位

**SAG Cloud 是一套分层清晰的企业零信任安全接入网关原型**：边界接入（Zentinel）+ 隧道（Agent/Bridge/Connector）+ 策略与身份（Policy/Auth）+ 内网网关（APISIX），配套控制面与可观测栈，适合 **PoC、技术验证与迭代产品化**。

### 4.2 核心价值（竞标话术）

| 维度 | 价值 |
|------|------|
| 架构可评审 | 边界、隧道、PDP、内网 L7 职责分离，安全与运维可分段落地 |
| 路径可验证 | 标准数据面路径明确；配套脚本与探针，减少“能启动但不通” |
| 身份可扩展 | 本地账号/JWT；4A OAuth2；无甲方接口时 Fake 4A 支撑演示 |
| 运维可观测 | Prometheus/Grafana；管理端工作流与指标；北向 N1 补充业务健康语义 |

---

## 5. 总体架构

### 5.1 逻辑分层

1. **北向接入与边界**：Zentinel（HTTPS 入口、TLS 终止；metrics 可抓取）。  
2. **隧道适配**：http-tunnel-bridge（HTTP ↔ gRPC）；stealth-tunnel-agent（gRPC 隧道、mTLS、路由同步、策略前检）。  
3. **内网接入**：sag-connector（注册隧道，HTTP 代理至 APISIX 数据面）。  
4. **内网 L7**：APISIX（路由/治理；控制面可选下发路由）。  
5. **控制面**：control-plane-admin、sag-auth、sag-policy。  
6. **观测与门户**：Prometheus、Grafana、OTel；frontend-admin-next（运维控制台）、frontend-portal（用户门户）。

### 5.2 标准数据面路径（与仓库约定一致）

```
Client
  → Zentinel（北向 HTTPS）
    → http-tunnel-bridge
      → stealth-tunnel-agent（策略/IAM 前检）
        → sag-connector
          → APISIX（内网 L7）
            → 业务上游（Mock/真实工作负载）
```

**说明**：`sag-connector` 必须配置 `SAG_APISIX_BASE_URL`；连接器不再承担“内网流量调度中心”，**APISIX 为标准内网 L7 路径**（见 `Context_Handoff.md` 策略说明）。

### 5.3 控制面职责摘要

| 组件 | 职责 |
|------|------|
| control-plane-admin | 隧道路由 CRUD、内网上游映射；可选 APISIX Admin API 联动 |
| sag-auth | 登录、JWT、会话；可选 4A/OIDC SSO |
| sag-policy | 策略 CRUD、`/policy/evaluate` PDP |

---

## 6. 核心功能说明

### 6.1 身份与访问

- **本地认证**：用户名密码、Argon2、JWT。  
- **4A / SSO**：配置 `SAG_FOURA_*` 等后启用 `/api/v1/auth/sso/*`；详情见 `docs/identity-4a.md`。  
- **演示环境**：`infra/fake-4a` 提供轻量 OAuth2 模拟；支持门户 `sso_token` 跳转、角色映射演示、`guest_preview` 展示未认证拦截。

### 6.2 策略（PDP）

- 优先级化 ALLOW/DENY；门户可策略预检；隧道层可调用 `sag-policy` 再转发。

### 6.3 隧道与路由

- Agent 从控制面同步路由；**`connector_endpoint` 须与当前运行的 `SAG_CONNECTOR_ID` 一致**（如 `connector-xxx:stream`），否则易出现隧道不健康等业务失败（双机场景尤需核对）。

### 6.4 运维控制台（adminplane）

- **frontend-admin-next**（默认 `:3001`）：工作流视图、Prometheus 指标、硬件相关展示、应用/API 树等；旧版控制台能力并入 `/control`。  
- 工作流中 **Zentinel 健康**：除 Prometheus `up` 外，可通过 **北向 N1 探针**（经 `/api-zentinel`）对 5xx 做业务侧降级展示（见 `Context_Handoff.md`）。

---

## 7. 可靠性与安全要点（已文档化 / 已验证类）

| 主题 | 说明 |
|------|------|
| Fail-closed | 错误 SNI、错误 endpoint、Agent 停机等场景下链路按预期失败，恢复配置后可回到正常 |
| TLS / SNI / SAN | 客户端 `SAG_GRPC_TLS_SERVER_NAME` 须与服务端证书一致；北向访问主机名须在证书 SAN 内；CA 需注入客户端（如 Node `NODE_EXTRA_CA_CERTS`） |
| 健康语义 | 进程存活 ≠ 业务可用；建议 **T1 + N1** 作为验收组合（见 `DEPLOYMENT_README.md`） |
| 证书部署 | KDL 中证书路径建议 **容器内绝对路径**；新环境建议 openssl 预检后再启动 |

---

## 8. 部署与交付

### 8.1 主路径

在 `sag-cloud` 根目录：`docker compose build`、`docker compose up -d`。**Zentinel 为默认服务**，无需额外 profile。

### 8.2 主要端口（摘录）

| 端口 | 服务/用途 |
|------|-----------|
| 8090 | control-plane-admin |
| 8080 | sag-auth |
| 8081 | sag-policy |
| 9000 | http-tunnel-bridge |
| 10080 | Zentinel HTTPS |
| 9080 / 9180 | APISIX 数据面 / Admin |
| 9091 | Prometheus（宿主机映射，以 compose 为准） |
| 3000 | Grafana |
| 3001 | frontend-admin-next |
| 5174 | frontend-portal |

完整列表见 `DEPLOYMENT_README.md`。

### 8.3 双机场景

- 模板：`docker-compose.edge.yml`、`docker-compose.intra.yml`，环境变量示例：`.env.dualhost.example`。  
- 重点：TLS、DNS、**路由中的 connector_endpoint 与实际 connector ID** 一致。

### 8.4 上线验收（建议写进项目交付 checklist）

- `curl -i http://<host>:9000/api/test` → **T1**  
- `curl -i http://<host>:3001/api-zentinel/api/test` → **N1**（管理端代理北向；均需带演示用 header 时以现场为准）  
- 冒烟：`scripts/smoke-dataplane.ps1` 或 WSL 脚本，预期 **M*/N1/T1/S*** 等 PASS。

### 8.5 Zentinel 启动说明（避免误解）

- Compose 已采用 `--manifest-path` 等方式降低工具链同步阻塞风险；**冷机首次仍可能有 Rust 编译耗时**。  
- **启动耗时与配置时效性无直接耦合**；路由与策略仍以控制面、环境变量与配置文件为准。

---

## 9. 差异化与可比性（售前常用）

| 对比点 | SAG 表述建议 |
|--------|----------------|
| vs 纯 VPN | 在“能连内网”之上叠加 **应用级策略与统一接入面** |
| vs 单一边界 WAF | 分层清晰：**边界 TLS + 隧道 + PDP + 内网 L7**，便于分阶段建设 |
| vs 仅网关插件 | 身份、策略、隧道、控制面 **一体演示路径**，PoC 可闭环 |

---

## 10. 风险、限制与后续路线

### 10.1 诚实边界

- 部分模块为规划占位（如终端安全、审计风控等，见 `architecture/`、`services/planned/`）。  
- Windows 与容器、curl 后端差异可能导致 TLS 探测表现不同，**生产验收建议在目标 OS 上执行脚本**。  
- 甲方正式 4A 需按 `docs/identity-4a.md` 与现场网络策略联调。

### 10.2 建议演进

- 发布 **预编译镜像**，缩短冷启动与交付时间。  
- 证书与密钥：**卷挂载 + 密钥管理 + 轮换流程** 标准化。  
- 持续强化 **指标维度**（host/app_id/route）与大盘统一展示。

---

## 11. 结语

SAG Cloud 以 **可运行的端到端数据面**、**控制面与策略闭环**、**可观测与可验收路径**，支撑 **零信任安全接入** 的技术验证与方案汇报；后续可在甲方合规与基础设施条件确定后，推进产品化与规模化部署。

---

## 附录 A：术语速查

| 术语 | 含义 |
|------|------|
| PDP | Policy Decision Point，策略决策点（本仓库主要为 `sag-policy`） |
| T1 | 经 bridge 的隧道向探测（典型 `:9000/api/test`） |
| N1 | 经北向（含管理端代理到 Zentinel）的探测（典型 `:3001/api-zentinel/...`） |
| Fake 4A | 仓库内轻量 OAuth2 模拟服务，用于无甲方接口时的演示 |

## 附录 B：相关文档索引

- `README.md` — 使用手册、环境变量、冒烟说明  
- `DEPLOYMENT_README.md` — 服务器部署步骤、常见问题、TLS 预检  
- `Context_Handoff.md` — 会话级进度与验证结论  
- `docs/identity-4a.md` — 身份与 4A 联调说明  
