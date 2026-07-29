# 背压与排队：bridge Redis 202 运维手册（操作版）

对应主计划 [high-concurrency-reliability-master-plan.md §2](high-concurrency-reliability-master-plan.md#2-背压与排队)。本文只写 **可执行步骤**；架构原则仍以主计划为准。

**相关**：数据面 B/C/D 基线 [tunnel-capacity-bootstrap.md](tunnel-capacity-bootstrap.md)；压测与日志对齐 [tunnel-loadtest-correlation.md](tunnel-loadtest-correlation.md)。

---

## 1. 数据流（你要调的是哪一段）

```text
客户端/k6 → Zentinel:10080 → http-tunnel-bridge:9000
                                    │
                    sync_inflight < SAG_BRIDGE_SOFT_INFLIGHT
                    且未走隧道门闸饱和 ──► 同步 Unary gRPC → stealth-agent → connector → …
                                    │
                    sync_inflight ≥ soft 且 SAG_BRIDGE_REDIS_URL 非空
                    且 body ≤ SAG_BRIDGE_QUEUE_MAX_BODY_BYTES
                    且 enqueue 成功 ──► HTTP 202 + Location /__sag/queue/{id}/status
                                    │
                                    └► Redis Stream sag:dataplane:queue（DB 见 URL）
                                        → worker（SAG_BRIDGE_WORKER_CONCURRENCY）→ 同上 gRPC
```

**要点**：202 **不是**全链路异步消息总线；只是 **bridge 进程内** 用 Redis 把「同步 forward」改成「先入队再出队 forward」。客户端必须 **接受 202 并轮询**（见 §7），否则 k6/业务会把 202 计成失败或悬挂。

`docker-compose.edge.yml` 中的 Redis 是带密码、volume 和 AOF 的**开发单节点**，不是生产 HA。生产必须使用具备自动故障转移的托管 Redis，或独立部署的 Sentinel/Redis 集群；Bridge 到 Redis/Sentinel 使用认证和 TLS。不得以此 Compose 声称主从切换能力。

---

## 2. 环境变量（与 `docker-compose.edge.yml` 默认一致）

| 变量 | compose 默认 | 二进制内 fallback（仅当未注入 env） | 含义 |
|------|----------------|--------------------------------------|------|
| `SAG_BRIDGE_REDIS_URL` | `redis://:dev-only-change-me@redis:6379/2` | 非空才启用队列；**清空则禁用 202 路径** | 直接/托管 endpoint；生产用认证 `rediss://`；Sentinel 模式下提供 master 的认证/TLS/DB 模板 |
| `SAG_BRIDGE_REDIS_SENTINELS` / `SAG_BRIDGE_REDIS_SENTINEL_SERVICE` | 空 | 空 | 必须同时为空或同时非空；前者是逗号分隔的 Sentinel URL，后者是 master service name |
| `SAG_BRIDGE_REDIS_CONNECT_TIMEOUT_MS` | 2000 | 2000 | 单次建连上限 |
| `SAG_BRIDGE_REDIS_COMMAND_TIMEOUT_MS` | 5000 | 5000 | 命令上限；必须大于 worker 的 2000 ms blocking read |
| `SAG_BRIDGE_REDIS_RECONNECT_RETRIES` | 6 | 6 | 有界重连/重发现次数 |
| `SAG_BRIDGE_REDIS_RECONNECT_BASE_MS` / `SAG_BRIDGE_REDIS_RECONNECT_MAX_MS` | 100 / 2000 | 100 / 2000 | 指数退避基数和封顶 |
| `SAG_BRIDGE_SOFT_INFLIGHT` | 24 | 24 | 精确 sync semaphore 满时尝试入队 202 |
| `SAG_BRIDGE_HARD_INFLIGHT` | 128 | 128 | body 读取前的 hard ingress semaphore；满时快速 **503** |
| `SAG_BRIDGE_MAX_TUNNEL_INFLIGHT` | 128 | 有队列时不高于 512 且取 hard limit；无队列 128 | **隧道 Unary 并发 semaphore**；`0` = 关闭门闸 |
| `SAG_BRIDGE_QUEUE_MAX_LEN` | 20000 | 5000 | Stream 长度上限；满则 `queue_full` |
| `SAG_BRIDGE_QUEUE_MAX_BODY_BYTES` | 262144 | 262144 | 超过则不入队（413 或拒绝） |
| `SAG_BRIDGE_QUEUE_TTL_SEC` | 600 | 600 | 任务/状态 TTL 语义（见代码） |
| `SAG_BRIDGE_WORKER_CONCURRENCY` | 16 | 16 | 消费 worker 数；与 batch/body 一起计入启动内存预算 |
| `SAG_BRIDGE_QUEUE_MAX_RESULT_BODY_BYTES` | 65536 | 65536 | 结果体上限 |
| `SAG_BRIDGE_POLL_MIN_INTERVAL_MS` | 100 | 100 | poll 节流 |
| `SAG_BRIDGE_DEDUP_TTL_SEC` | 600 | 600 | 去重键 TTL |
| `SAG_BRIDGE_READ_ONLY_SYNC_FALLBACK_ON_QUEUE_ERROR` | false | false | 默认入队依赖失败返回 503；显式为 true 时也仅 GET/HEAD/OPTIONS 可回退，mutation 永不回退 |
| `SAG_BRIDGE_HTTP_RPS_PER_APP` | 0（关闭） | 0 | **>0** 时按 `x-sag-app-id` 令牌桶限流 dataplane HTTP；超限 **429**，`bridge_http_app_ratelimit_reject_total`。见 [rate-limit-circuit-breaker-runbook.md](rate-limit-circuit-breaker-runbook.md) §6 |
| `SAG_BRIDGE_FORWARD_CB_FAILURE_THRESHOLD` | 0（关闭） | 0 | **>0** 时连续「Unary 两次 attempt 均失败」达阈值则打开熔断 cool-off；`bridge_forward_circuit_*`。见同上 |
| `SAG_BRIDGE_FORWARD_CB_COOL_OFF_MS` | （随 threshold 使用，默认 10000） | 10000 | 熔断打开持续时间（毫秒） |

修改后：**Edge 上**对 `http-tunnel-bridge` **`docker compose ... up -d --force-recreate http-tunnel-bridge`**（多副本时每个任务都需相同 env）。

---

## 2.5 入队依赖失败：默认 fail closed

在 **`sync_inflight ≥ soft`** 或 **隧道 semaphore 满并尝试 shed 到 Redis** 时，若 `enqueue` 因 **序列化** 或 **Redis** 失败：

默认返回 **HTTP 503**（`error: queue_unavailable`、`Retry-After: 5`），避免 Redis 故障时把流量重新压回已经饱和的同步路径。仅在经过容量评估的紧急场景，才可设置 `SAG_BRIDGE_READ_ONLY_SYNC_FALLBACK_ON_QUEUE_ERROR=true`；该开关只允许 GET/HEAD/OPTIONS 同步回退。POST/PUT/PATCH/DELETE 等 mutation 始终 fail closed，不因队列错误自动改走同步路径。

---

## 3. Redis：启用检查与队列长度（Edge 宿主机）

在 **`$REPO_ROOT`**（含 compose）的 Edge 上：

```bash
# 1) 存活与库号（REDISCLI_AUTH 避免把密码放在 argv；bridge 默认用 DB 2）
docker exec -e REDISCLI_AUTH="$SAG_REDIS_PASSWORD" sag-redis redis-cli --no-auth-warning -n 2 PING

# 2) Stream 长度（key 名见代码常量 sag:dataplane:queue）
docker exec -e REDISCLI_AUTH="$SAG_REDIS_PASSWORD" sag-redis redis-cli --no-auth-warning -n 2 XLEN sag:dataplane:queue

# 3) 可选：消费组是否存在（首次 worker 会创建）
docker exec -e REDISCLI_AUTH="$SAG_REDIS_PASSWORD" sag-redis redis-cli --no-auth-warning -n 2 XINFO GROUPS sag:dataplane:queue
docker exec -e REDISCLI_AUTH="$SAG_REDIS_PASSWORD" sag-redis redis-cli --no-auth-warning -n 2 XPENDING sag:dataplane:queue bridge-workers
docker exec -e REDISCLI_AUTH="$SAG_REDIS_PASSWORD" sag-redis redis-cli --no-auth-warning -n 2 XLEN sag:dataplane:dlq
```

**不要做**：不得对没有持久化终态的 entry 人工 `XACK`/`XDEL`；这会把“未知”伪装成完成。也不要运行 `FLUSHDB` / `FLUSHALL`。DLQ 必须按 request/idempotency scope 对账后处理。

### RPO、RTO 与恢复验证

- Compose 使用 `appendonly yes` + `appendfsync everysec`：在宿主机或 Redis 进程突然失败时，理论上可能丢失约 1 秒最近写入，**不是 RPO 0**。若业务要求 RPO 0，应选择满足该承诺的托管持久化/复制产品并实测。
- 单节点 Compose 没有自动主从切换，RTO 等于人工恢复时间，不能声明生产 RTO。生产 Sentinel/托管 HA 的切换 RTO 由供应商 SLA 和演练结果决定；Bridge 的单次建连、重连次数与退避上限只是客户端等待边界。
- 每次发布或 Redis 拓扑变更后运行 `pwsh scripts/ops/test-queue-recovery.ps1 -Jobs 100`（Linux 可运行 `scripts/ops/test-queue-recovery.sh 100`）。验收要求 completed + 可解释 indeterminate/DLQ = 100、unknown = 0、重复 mutation dispatch = 0、PEL = 0。
- 故障恢复先观察 `XPENDING` oldest idle、DLQ 增量、job hash 终态和 Agent idempotency ledger。终态已持久化的 replay 只能 ACK；可能已 dispatch 但无终态的 mutation 进入 indeterminate/人工对账，禁止自动重放。

---

## 4. bridge `/metrics`：背压相关计数器 / 仪表盘

在 **能访问 bridge 9000** 的机器上（Edge 本机或经 VPN；多副本时见 [horizontal-scale-edge-bridge.md](horizontal-scale-edge-bridge.md)）：

```bash
curl -sS "http://127.0.0.1:9000/metrics" | grep -E '^bridge_(sync_inflight|soft_gate_entered_total|queue_202_total|queue_enqueue_total|soft_fallback_total|soft_enqueue_failure_503_total|queue_reject_total|tunnel_try_saturated_total|tunnel_shed_to_queue_total|tunnel_saturated_503_total|queue_depth|worker_forward_total|queue_dlq_total|queue_poll_throttled_total)\b'
```

| 指标名（前缀 `bridge_`） | 用途 |
|--------------------------|------|
| `sync_inflight`（gauge） | 当前同步路径在途近似 |
| `soft_gate_entered_total` | 进入「软门限」分支次数 |
| `queue_202_total` | 返回 **202** 次数 |
| `queue_enqueue_total` | 成功写入 Redis 次数 |
| `soft_fallback_total{reason="redis_enqueue"\|"serialization"}` | **未 202**：回退同步或错误路径（`SAG_BRIDGE_SOFT_ENQUEUE_ON_FAILURE=fallback`） |
| `soft_enqueue_failure_503_total{reason=...}` | 入队失败且配置为 **`503` / `service_unavailable`** 时直接 503，**无**同步 fallback |
| `queue_reject_total{reason=...}` | hard_inflight / queue_full / body_too_large |
| `tunnel_try_saturated_total` / `tunnel_shed_to_queue_total` | 隧道门闸满时尝试 shed 到队列 |
| `queue_depth`（gauge） | 队列深度（近似） |
| `worker_forward_total{result=ok\|error}` | worker 出队后 forward 成败 |
| `queue_dlq_total` | 死信 |
| `queue_poll_throttled_total` | poll 被限流 |

---

## 5. 「202 仍为 0」判定树（按顺序做）

1. **`SAG_BRIDGE_REDIS_URL` 是否非空？**  
   - 空 → **无队列**；不会出现 202。  
2. **`redis-cli -n 2 PING` 是否 OK？**  
   - 失败 → enqueue 失败：默认看 `queue_dependency_unavailable_total`、`bridge_soft_enqueue_failure_503_total` 与 HTTP **503**；mutation 无同步 fallback。  
3. **压测并发是否 ≥ `SAG_BRIDGE_SOFT_INFLIGHT`？**  
   - 长期低于 soft → **不会进软门限**，202 为 0 正常。  
4. **`bridge_soft_gate_entered_total` 是否上升而 `queue_202_total` 不升？**  
   - 看 `queue_dependency_unavailable_total` 与 bridge **warn 日志**（`queue enqueue redis error` / serialization）。  
5. **`upstream_5xx` / 超时是否已占满失败？**  
   - 同步路径已失败时，未必观察到 202；按 [tunnel-loadtest-correlation.md](tunnel-loadtest-correlation.md) 对齐 **connector / APISIX / mock** 时间窗。

---

## 6. 调参顺序（保守，每步留 k6 JSON 基线）

1. **确认 Redis DB2** 与 **`bridge_soft_fallback_total{reason=redis_enqueue}`** 为 0 或可接受。  
2. **降低 `SAG_BRIDGE_SOFT_INFLIGHT`**（例如 24→16）使更早 202；**客户端必须** 打开 Poll+Accept202（§7）。  
3. **提高 `SAG_BRIDGE_WORKER_CONCURRENCY`** 加快 drain（观察 CPU）。  
4. **提高 `SAG_BRIDGE_MAX_TUNNEL_INFLIGHT`**（与 agent/connector 能力匹配，避免单点打爆）。  
5. **提高 `SAG_BRIDGE_QUEUE_MAX_LEN`** 仅在 `queue_full` 明显时。  

每步：**改 `.env` 或 compose 覆盖 → `--force-recreate http-tunnel-bridge` → 同口径 k6 → 保存 `artifacts/*.json` 与 `metrics` 片段**。

---

## 7. 压测口径（k6）

必须使用（PowerShell）：

```powershell
.\scripts\ops\run-load-dataplane.ps1 -EdgeHost <IP> ... -PollDataplane202 -AcceptDataplane202 -AcceptDataplane429Shed
```

否则：**202 会计入失败或未完成**；`sag_dataplane_queue_poll_total` 等可能为 0。

---

## 8. 禁用队列（紧急回退纯同步）

1. 在 Edge **`.env` 或 compose** 中 **取消设置或置空** `SAG_BRIDGE_REDIS_URL`。  
2. **`docker compose ... up -d --force-recreate http-tunnel-bridge`**。  
3. 预期：**无 202**；过载时依赖 **429/502** 等行为，需接受风险。

---

## 9. 回滚

- 恢复上一版 **`.env` / compose** 中 bridge 段，**`--force-recreate http-tunnel-bridge`**。  
- **不**默认要求删 Redis Stream；回滚二进制前要先停准入并排空 queue/PEL。只有业务终态已持久化或 indeterminate 已对账后才允许清理；不得用 `XTRIM`/`XACK` 跳过对账。

---

## 10. Redis Key 备忘（只读排查）

| Key / 模式 | 说明 |
|------------|------|
| `sag:dataplane:queue` | Stream |
| `sag:dataplane:dlq` | DLQ |
| `sag:dataplane:job:{queue_id}` | 任务状态（poll 读） |
| `sag:dataplane:dedup:{app_id:idempotency_key}` | mutation 去重；read fallback 使用 request ID |

消费组名：**`bridge-workers`**（`queue.rs` `GROUP_NAME`）。
