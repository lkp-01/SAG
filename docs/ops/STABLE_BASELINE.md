# Legacy Edge deployment snapshot (not a production capacity baseline)

> This 2026-05-07 tag records a build/deployment snapshot only. It predates the full-chain business-success gate and contains no qualifying capacity result. The sole current capacity qualification document is [production-capacity-baseline.md](production-capacity-baseline.md), whose status remains `NOT ESTABLISHED` until the required tests pass.

| 项 | 值 |
|----|-----|
| **记录日期** | 2026-05-07 |
| **Git tag** | `stable/edge-baseline-20260507` |
| **提交（代码尖端）** | `ff261f98` |
| **叠加上一层（压测脚本基线）** | `4873b9ca` — `chore(ops): tiered dataplane k6 700 then 900...` |

## 含义

- `ff261f98`：在 `4873b9ca` 之上仅增加 **admin-next** `bridge-dataplane.ts` 的 **BodyInit / ArrayBuffer** 修复，使 **Next 15** 在 Linux 上 `next build` 通过；Edge 上 **`frontend-admin-next` 暴露 3001** 经实机验证可用（首次需等 `npm ci` + `build` 完成后再访问）。

## 检出与对齐

Tag 指向 **`ff261f98`**（纯代码快照）。本说明文件在 **`clean-main`** 上；若只 `checkout` tag，工作区不含此 Markdown，但代码与稳定运行时一致。

```bash
git fetch origin
git checkout stable/edge-baseline-20260507
# 或: git checkout ff261f98
```

## 相关部署（Edge）

```bash
docker-compose -f docker-compose.edge.yml -f docker-compose.release.edge.yml up -d --build frontend-admin-next
```

若使用 release 中的 zentinel 二进制路径，需事先在仓库根挂载下编译 `proxy/core/target/release/zentinel`（见运维手册或 compose 注释）。
