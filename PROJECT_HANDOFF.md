# SAG 双机项目交接总结

> **【必读】** 接手请从 **[`README.md`](README.md)** 的「上手」章节开始；本文档为结论与待办速查。详细运维见 [`DUAL_HOST_OPERATIONS.md`](DUAL_HOST_OPERATIONS.md) §0；各机命令见 [`SERVER_OPS_QUICKREF.md`](SERVER_OPS_QUICKREF.md)。

**更新日期**：2026-05-21  
**分支**：`clean-main`  
**Git**：`git@192.168.14.10:digital-operation/secure_access_gateway_sag.git`

---

## 【重点】一句话结论

| 维度 | 结论 |
|------|------|
| **数据面** | **不是 blocker**；`apisix_routed` 在 **≤5000 QPS** 稳定（≥94%），演示可用 **3000–5000** |
| **Auth login** | **是当前 blocker**；单实例 @2000 约 **79%**；hscale **更差（~56% chain）** |
| **Auth hscale** | 代码在仓库，**当前不建议启用**；建议 **单实例 + Linux 压测机** 继续攻关 |
| **全链路 @3000** | 整链 ~40%，**login ~0.5%** → Auth 被打穿 |

---

## 【重点】三台（四台）机器

| 角色 | IP | `REPO_ROOT`（示例） | 用途 |
|------|-----|---------------------|------|
| **Edge** | **`172.16.9.107`**（28 逻辑核） | `~/secure_access_gateway_sag` | Docker 全栈、Zentinel :10080、Auth :8080、Postgres |
| **Intra** | **`192.168.9.26`**（8 核） | 同仓库另一份 clone | APISIX :9080、mock、**sag-connector** → Edge |
| **压测 Windows** | **`172.16.9.108`** | `D:\...\Secure_Access_Gateway_SAG\sag-cloud` | **数据面** k6 可用；Auth 高 QPS 易客户端瓶颈 |
| **压测 Linux（推荐）** | 麒麟 VM（**k6 待安装**） | clone 根目录 | **Auth 门禁应用此机**，见 `DUAL_HOST_OPERATIONS.md` §3e |

> **【重点】** Intra 上 **`sag-connector`** 的 **`SAG_TUNNEL_ENDPOINT`** 必须在 **`.env.intra`** 里指向 **当前 Edge `172.16.9.107`**，改后须 **`force-recreate sag-connector`**。The Connector requires the Edge tunnel endpoint and mTLS material, but no database DSN. Central persistence is owned by Edge services.

---

## 【重点】Edge 栈现状（接手必核对）

| 组件 | 目标态 | 核对 / 风险 |
|------|--------|-------------|
| **bridge ×2 + zentinel** | hscale；cpuset 12–14 / 15–17 / 18–25 | 确认 **release** 二进制，勿长期 `cargo run` |
| **sag-policy** | cpuset **4–7** | 勿用旧 **3–6**（与 auth-2 抢核 3） |
| **sag-auth** | **建议单实例** 直连 :8080 | 若 curl login 响应头 **`Server: nginx`** → 仍在 **Auth hscale**，执行 `bash scripts/ops/rollback-auth-hscale-edge.sh` |
| **auth nofile** | **1048576** | EMFILE 曾导致 nginx 499 |
| **Postgres / Redis** | 均绑 **CPU 0** | login 热路径**不逐请求查库**；与 Redis 同核 |

**数据面 hscale 一键（不含 Auth 扩展）** — 见 [`SERVER_OPS_QUICKREF.md`](SERVER_OPS_QUICKREF.md) Edge 节。

---

## 【重点】压测结论摘要

### 数据面（Windows k6 → `:10080`，`apisix_routed`）

| 目标 QPS | apisix_routed | 实际 iter/s | 备注 |
|---------|---------------|-------------|------|
| 3000 | **97.19%** | 2903.9 | 稳 |
| 4000 | **96.13%** | 3858.8 | 稳 |
| 5000 | **94.08%** | 4821.5 | **推荐对外稳定上限** |
| 6000 | 91.41% | 5503.2 | 过渡区 |
| 7000 | 92.83%（复测） | 6061.3 | 波动大，**不宜稳态承诺** |

- 路径：**Zentinel → Bridge×2 → Agent ↔ Connector → APISIX → mock**
- 全程 **0** 次 `connector tunnel is unhealthy` / `no tunnel route`
- 详细表格：**[`docs/ops/dataplane-load-3000-7000-report.md`](docs/ops/dataplane-load-3000-7000-report.md)**
- JSON：`artifacts/k6-dp-*-routed-202605*.json`

### Auth（`auth_login_verify` @2000）

| 形态 | login / chain | 实际 iter/s | artifact |
|------|---------------|-------------|----------|
| **单实例** | **~79% / ~79%** | ~1858 | `k6-auth-2000-20260519-182051.json` |
| **hscale + policy 4–7** | **~59% / ~56%** | ~220 | `k6-auth-win-2000-20260520-135225.json` |

> **【重点 · 门禁】** login / verify / chain **均 >90%** 才晋级 Auth **3000**；**当前未过，勿测 Auth 3000**。

---

## 【重点】团队决策（2026-05）

1. **数据面**：已验证，演示用 **3000–5000** 即可。  
2. **Auth hscale**：**暂不启用**；优先单实例 + 排除 Windows 客户端/LB 因素。  
3. **Auth 压测主路径**：**Linux 麒麟 VM + k6**（§3e），勿用 Windows 结果代表 Edge 能力。  
4. **Postgres 单核**：可观察，但 login 瓶颈更可能在 **argon2 / Redis / TCP / nginx LB**，需压测时 `docker stats` + memo 指标验证。

---

## 接手建议顺序

1. 读本文 + [`DUAL_HOST_OPERATIONS.md`](DUAL_HOST_OPERATIONS.md) **§0** + [`SERVER_OPS_QUICKREF.md`](SERVER_OPS_QUICKREF.md)  
2. **Edge**：`git pull`（§2c 处理 cpuset 冲突）→ §6 **release 编译** → 数据面 hscale compose → 若不用 Auth hscale → **rollback 脚本**  
3. **Intra**：核对 `.env.intra` → `force-recreate sag-connector`  
4. **麒麟 VM**：安装 k6 → `run-auth-gate-2000.sh`  
5. Auth @2000 **>90%** 后再考虑 3000 / 全链路  

---

## 关键文件索引

| 文件 | 说明 |
|------|------|
| [`DUAL_HOST_OPERATIONS.md`](DUAL_HOST_OPERATIONS.md) | 主运维手册（§0 会话接续） |
| [`SERVER_OPS_QUICKREF.md`](SERVER_OPS_QUICKREF.md) | 各机常用命令（含生产编译启动） |
| [`docs/ops/dataplane-load-3000-7000-report.md`](docs/ops/dataplane-load-3000-7000-report.md) | 数据面 3000–7000 完整报告 |
| [`scripts/ops/rollback-auth-hscale-edge.sh`](scripts/ops/rollback-auth-hscale-edge.sh) | Auth hscale → 单实例 |
| [`scripts/ops/verify-hscale-edge.sh`](scripts/ops/verify-hscale-edge.sh) | Edge 冒烟 |
| [`scripts/ops/run-auth-gate-2000.sh`](scripts/ops/run-auth-gate-2000.sh) | Linux Auth @2000 一键 |
| `artifacts/k6-*.json` | 压测结果归档 |

---

## 尚未完成 / 风险

- [ ] Auth **单实例 @2000** 在 **Linux 压测机** 无正式结果（k6 未装好）  
- [ ] Edge **Auth hscale 是否已回滚** — 以 `curl` login 无 `Server: nginx` 为准  
- [ ] **bridge-2 / zentinel** 是否仍为 `cargo run`  
- [ ] 本文档、`dataplane-load-3000-7000-report.md`、`rollback-auth-hscale-edge.sh` 需已 **push 到 GitLab**

---

## 对外演示口径

- **数据面**：北向约 **5000 iter/s**，`apisix_routed` **≥94%**；经 Zentinel/Bridge/隧道/Connector/APISIX。  
- **全链路**：受 **Auth login** 限制。  
- **Auth 扩展**：代码存在，**当前未证明收益，默认单实例**。
