# 主计划与压测基线：文档维护（§8）

对应主计划 [high-concurrency-reliability-master-plan.md §8](high-concurrency-reliability-master-plan.md#8-文档维护)。

---

## 1. 何时更新主计划

在下列变更后，更新 [high-concurrency-reliability-master-plan.md](high-concurrency-reliability-master-plan.md) 并追加 **修订记录**：

- 多 bridge / 多 agent / 外置 ConnectorRegistry  
- 默认 compose 超时、队列、限流 env **语义变更**  
- 新增或废弃某域 runbook  

**做法**：在 §8「修订记录」增加一行：`日期 | 变更摘要 | 关联 PR/commit`。

---

## 2. 修订记录模板（复制到主计划 §8）

```markdown
- **YYYY-MM-DD**：<一句话摘要>（<可选：PR # / commit>）；<废弃/替代段落说明>。
```

示例：

```markdown
- **2026-05-19**：§5–§8 runbook 落地（cache / async / roadmap / docs-maintenance）；k6 默认 RequestTimeout 90s（§4）。
```

---

## 3. 压测基线 JSON 版本化

### 3.1 命名建议

存放目录：`sag-cloud/artifacts/`（已 gitignore 或按需提交）。

| 模式 | 示例 |
|------|------|
| 带时间戳 | `k6-mixed-700-20260514-111112.json` |
| 带场景 | `k6-sweep-20260512-154514-dp-700.json` |
| 带环境 | `k6-gated-500-700-<host>-dp-500.json` |

**建议字段**（写入同目录 `README.txt` 或 PR 描述，不必改 JSON 结构）：

- Git commit / tag  
- Edge / Intra IP  
- `run-load-dataplane.ps1` 参数摘要（`-RequestTimeout`、`-PollDataplane202`、RPS）  
- compose 关键 env（`SOFT_INFLIGHT`、`WORKER`）  

### 3.2 归档脚本（Windows）

在 `sag-cloud` 目录：

```powershell
.\scripts\ops\archive-k6-baseline.ps1 -SourceJson .\artifacts\k6-fullchain-summary.json -Label "post-p1-tune"
```

生成：`artifacts/k6-baseline-<label>-<yyyyMMdd-HHmmss>.json` 并打印建议写入 PR 的备注行。

---

## 4. Runbook 索引（避免重复写长文）

| § | 手册 |
|---|------|
| 1 | 主计划 §1 + [horizontal-scale-edge-bridge.md](horizontal-scale-edge-bridge.md) |
| 2 | [backpressure-queue-runbook.md](backpressure-queue-runbook.md) |
| 3 | [rate-limit-circuit-breaker-runbook.md](rate-limit-circuit-breaker-runbook.md) |
| 4 | [timeout-deadline-runbook.md](timeout-deadline-runbook.md) |
| 5 | [cache-read-runbook.md](cache-read-runbook.md) |
| 6 | [async-patterns-runbook.md](async-patterns-runbook.md) |
| 7 | [implementation-roadmap.md](implementation-roadmap.md) |
| 8 | 本文 |

主计划各 § 保留 **原则 + 落地状态表**；步骤以 runbook 为准。

---

## 5. 双机运维入口

总索引：[DUAL_HOST_OPERATIONS.md](../DUAL_HOST_OPERATIONS.md) §11。
