# 缓存与读多写少：运维手册（操作版）

对应主计划 [high-concurrency-reliability-master-plan.md §5](high-concurrency-reliability-master-plan.md#5-缓存与读多写少)。原则：**能缓存的才缓存**；数据面敏感读勿做全局语义缓存。

**相关**：[rate-limit-circuit-breaker-runbook.md](rate-limit-circuit-breaker-runbook.md)（agent policy 路径）、[tunnel-loadtest-correlation.md](tunnel-loadtest-correlation.md)。

---

## 1. 压测路径：可否缓存（清单）

| 路径 / 场景 | 是否建议缓存 | 说明 |
|-------------|--------------|------|
| **数据面** `GET https://<edge>:10080/dev/...` | **否**（默认） | 带租户/令牌/策略上下文；全局缓存有 **越权/陈旧策略** 风险 |
| **mock 静态页**（仅压测 mock 域名） | **可试点** | APISIX `proxy-cache` **短 TTL**；仅限无鉴权、可丢一致性场景 |
| **策略评估** `POST .../policy/evaluate` | **是**（已实现） | `sag-policy` Moka + 可选 Redis；见 §2 |
| **agent 负缓存**（未匹配路由等） | **是**（已实现） | `SAG_NEGATIVE_CACHE_*`；见 §2 |
| **登录** `POST .../auth/login` | **是**（已实现） | `sag-auth` login memo（Redis 或内存）；减轻登录风暴 CPU |
| **控制面路由列表** 等读 API | **视产品** | 一致性允许时 **秒级 TTL**；多由存储层或 BFF 缓存 |

---

## 2. 已实现缓存（Edge / 服务）

### 2.1 `sag-policy`

| 变量 | compose 典型值 | 含义 |
|------|----------------|------|
| `SAG_POLICY_CACHE_ENABLED` | 见 `docker-compose.edge.yml` | 是否启用评估结果缓存 |
| `SAG_POLICY_CACHE_TTL_SEC` | 秒级 TTL | |
| `SAG_POLICY_CACHE_MAX_CAPACITY` | 条目上限 | |
| `SAG_POLICY_CACHE_REDIS_URL` | 可选 | 跨副本共享（若配置） |

**指标**（`:8081/metrics` 或经 Prometheus）：

```bash
curl -sS "http://127.0.0.1:8081/metrics" | grep -E '^cache_(hit|miss)_total|^policy_eval_cache_hit_rate'
```

| 指标 | 含义 |
|------|------|
| `cache_hit_total{service="sag-policy",cache="policy_eval"}` | 内存 Moka 命中 |
| `cache_hit_total{...,cache="policy_eval_redis"}` | Redis 层命中 |
| `cache_miss_total{service="sag-policy",cache="policy_eval"}` | 未命中 |
| `policy_eval_cache_hit_rate{result=hit\|miss\|redis_hit}` | 命中率分桶 |

### 2.2 `stealth-tunnel-agent`

| 变量 | 默认倾向 | 含义 |
|------|----------|------|
| `SAG_NEGATIVE_CACHE_ENABLED` | true | 负缓存（未匹配路由等） |
| `SAG_NEGATIVE_CACHE_TTL_SEC` | 2 | 短 TTL，降低陈旧风险 |
| `SAG_AGENT_DEBUG_ADMIN` | 关 | `1` 时可 `POST /debug/clear-ephemeral-caches` 清空 Moka/负缓存（**不断**隧道） |

**指标**（`:9104/metrics`）：

```bash
curl -sS "http://127.0.0.1:9104/metrics" | grep -E '^cache_(hit|miss)_total.*stealth-tunnel-agent|^agent_degrade_redis_policy_stale'
```

另：`agent_degrade_redis_*` 为 Redis 降级 **陈旧策略** 路径，与「命中率」不同，勿混读。

### 2.3 `sag-auth`（登录 memo）

| 变量 | compose 默认（示例） | 含义 |
|------|----------------------|------|
| `SAG_SESSION_REDIS_URL` | `redis://redis:6379/0` | login memo / OAuth state |
| `SAG_LOGIN_MEMO_TTL_SEC` | 600 | |
| `SAG_LOGIN_MEMO_MAX_CAPACITY` | 500000 | |
| `SAG_LOGIN_MEMO_ENABLED` | 见代码 | 关闭则不走 memo |

**指标**：`sag_auth_login_memo_backend_redis_total` / `_in_memory_total`（登录路径）。

---

## 3. APISIX mock 路由 cache 试点（运维 checklist）

仓库 **未** 默认提交带 `proxy-cache` 的路由 JSON（避免与单机演示冲突）。仅在 **mock 域名 / 无鉴权静态** 试点：

1. 在 APISIX Admin 为目标 upstream 路由增加 **proxy-cache**（**短 TTL**，如 1–5s）。  
2. **勿** 对带 `Authorization` / 租户头的数据面路径启用。  
3. 变更后对比 k6：命中率上升时确认 **无越权样本**（手工或专项用例）。  
4. 步骤背景见 [intra-mock-apisix-horizontal.md](intra-mock-apisix-horizontal.md)。

---

## 4. 判定树（压测时 cache 是否帮倒忙）

1. **policy CPU 高、evaluate QPS 高** → 看 `policy_eval_cache_hit_rate`；miss 高则调 **TTL/容量** 或 Redis URL。  
2. **大量 403/404 重复** → agent `cache_miss_total{cache="negative"}` 是否下降。  
3. **登录风暴** → auth memo 是否走 Redis；是否仍打满 DB。  
4. **数据面延迟仍高** → **不要** 先加 APISIX 全路径 cache；先查 [timeout-deadline-runbook.md](timeout-deadline-runbook.md) 与隧道/ mock。

---

## 5. 保守调参顺序

1. 确认路径在 §1 表中 **允许缓存**。  
2. **policy**：先观测命中率，再调 `SAG_POLICY_CACHE_TTL_SEC` / `MAX_CAPACITY`。  
3. **agent 负缓存**：仅调 `SAG_NEGATIVE_CACHE_TTL_SEC`（不宜过大）。  
4. **auth login memo**：登录压测场景再调 `SAG_LOGIN_MEMO_*`。  
5. **APISIX cache**：最后、仅 mock。

---

## 6. 回滚

- 关闭 policy：`SAG_POLICY_CACHE_ENABLED=false` → recreate `sag-policy`。  
- 关闭 agent 负缓存：`SAG_NEGATIVE_CACHE_ENABLED=false` → recreate `stealth-tunnel-agent`。  
- 清空 agent 临时缓存（维护窗口）：`SAG_AGENT_DEBUG_ADMIN=1` 后调用 debug 端点（见 README）。  
- 移除 APISIX cache 插件并 reload。

---

## 7. 代码锚点

- `services/sag-policy/src/main.rs`：`policy_eval` 缓存与 `cache_*` 指标。  
- `proxy/agents/stealth-tunnel-agent/src/grpc_server.rs`：Moka、负缓存。  
- `services/sag-auth/src/main.rs`：`LoginMemoCache`。
