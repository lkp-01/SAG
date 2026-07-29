# Production full-chain capacity baseline

**Status: NOT ESTABLISHED**  
**Last evaluated: 2026-07-26**

No production capacity number is currently qualified. The checked-out workspace is not a recognizable Git worktree, the documented candidate endpoints `172.16.9.107:{8080,8081,8090,10080}` were unreachable from this execution environment, k6 is not installed here, and no independent Linux load generator, immutable image digest set, production credentials, resource watermark capture, or dependency evidence files were supplied.

The historical 3000–7000 report is a routed/transport experiment. It includes HTTP 5xx as routed success and non-zero dropped iterations, and omits Auth, Policy, idempotency, Redis queue and sampled audit proof. It cannot be promoted into this document.

## Required measurement record

This document may change from `NOT ESTABLISHED` only after the following are attached:

- first repeatable saturation point found with 20–25% full-chain steps;
- three 10–15 minute candidate repeats plus one 2–4 hour soak;
- raw k6 JSON, Prometheus snapshots, container/process stats and correlation samples for every run;
- `sag.production-gate-result/v1` with `qualification=passed`;
- hardware, OS/kernel, network RTT, payload distribution, read/write mix, Auth/Policy mode, audit mode and HA topology;
- 40-character Git SHA, immutable image digests and test date;
- exact Bridge/Agent/Connector/PG/Redis limits derived as 70% of the first repeatable saturation point;
- a fresh Task 9 memory-budget validation for those limits.

## Current safe configuration state

Until a gate result exists, retain the bounded defaults in `scripts/ops/perf-target.env.example`. They are safety defaults, not claimed capacity. Do not raise permits, connection pools, Redis queue capacity or alert thresholds based on the old routed report.

## Publication rule

README must say “capacity pending full-chain validation” and link only here for the current qualification state. A future update must preserve old raw artifacts, state the first saturation point and show the 70% calculation; it must not select the highest attempted rate merely because transport remained reachable.
