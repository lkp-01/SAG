# SAG 生产性能测试方案

## 资格边界

容量结论只来自 `full_chain` 场景。`transport` 只证明隧道可达；`workload` 只证明指定 workload 的精确 2xx 和响应内容。二者都不能发布为生产容量。

`full_chain` 的每次迭代必须实际经过 Auth 登录与 token 校验、Policy ALLOW 判定、带稳定 `Idempotency-Key` 的 mutation、Redis queue/poll、Agent 幂等账本、Connector、APISIX、已知 workload，以及按 trace 抽样回查的持久化审计。任一环节缺证据即失败。Policy DENY 是正确的业务拒绝，但不是本允许场景的成功样本。

业务成功同时满足：

- 状态码等于场景指定的 2xx；不能用“任意 200–599”或“已 routed”替代；
- JSON 中 `service`、唯一 `correlation`、规范化用户和角色与本次请求一致；
- 同一幂等键重放后 workload `side_effect_count` 仍恰为 1；
- generator `dropped_iterations=0`；
- sampled trace 在审计 lag SLO 内可查；
- Auth、Policy、Redis queue、idempotency、APISIX 和 workload 的参与证据完整。

## 三种场景

| 场景 | 用途 | 可发布容量 |
|---|---|---|
| `transport` | tunnel reachability、传输瓶颈定位 | 否 |
| `workload` | 精确 2xx、correlation/identity/body | 否 |
| `full_chain` | 生产容量候选和 soak | 是，但必须通过 production gate |

## 测试步骤

1. 在独立 Linux 主机/VM 上运行 load generator，确认 CPU ≤85%、网络利用率 ≤80%。
2. 低负载预热后，每级提高 20–25%，每级保持 10–15 分钟，直到连续两级违反任一 SLO；这才是可重复饱和拐点。
3. 在候选点运行至少三次 10–15 分钟稳态测试。
4. 三次均通过后运行一次 2–4 小时 soak。
5. 稳定生产限额取第一个可重复饱和拐点的 70%，并重新验证 Task 9 的进程内存预算。

PowerShell gate：

```powershell
pwsh scripts/ops/run-production-gate.ps1 -Scenario full_chain -TargetRps 500 -Repeats 3 -SoakMinutes 120
```

Linux gate：

```bash
scripts/ops/run-production-gate.sh full_chain 500 3 120
```

gate 要求 `SAG_PERF_ENVIRONMENT`、不可变镜像 digest JSON、资源水位 JSON 和依赖证据 JSON。缺失 Git SHA、镜像 digest、APISIX 请求增量、PG pool wait、Redis PEL age、audit drop、错误授权或 load-generator headroom 时 fail closed。

## Artifact 契约

每次运行必须保存原始 k6 JSON、Prometheus snapshot、container/process stats、日志 correlation 样本和 `sag.production-gate/v1` artifact。artifact 至少包含：

- 场景、Git SHA、镜像 digest、具名环境、开始/结束时间；
- 去敏配置快照、目标 RPS、实际 completed RPS；
- 业务错误和精确 HTTP 状态分布、p50/p95/p99；
- 各进程 RSS/CPU/连接/队列水位和 load-generator utilization；
- Auth/Policy/audit/Redis/idempotency/APISIX/workload 证据；
- PG pool wait、Redis PEL oldest age、audit dropped、authorization error。

summary 百分比不能代替原始产物。artifact 的 `qualification` 在单次运行中保持 `unqualified-run`；只有 gate 的全部重复和 soak 通过后，单独的 `sag.production-gate-result/v1` 才能写 `passed`。

## 停止与拒绝条件

出现以下任一情况立即拒绝该档：任意意外状态码、错误/旧 correlation、side effect ≠1、dropped iteration、错误授权、审计缺失/drop、PG/Redis 等依赖超阈值、进程 RSS 超 budget、发压机资源不足。不能通过调高并发或内存上限掩盖拐点。

历史 [3000–7000 routed 实验](dataplane-load-3000-7000-report.md) 仅用于传输瓶颈研究，不是生产容量基线。
