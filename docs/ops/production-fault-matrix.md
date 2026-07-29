# Production fault matrix

**Qualification status: NOT RUN in a real Linux/production-like environment.** The repository contains a machine-checkable orchestrator and validation contract. Passing its self-test proves the validator rejects incomplete evidence; it does not prove service failover.

Faults must run one at a time under continuous `full_chain` traffic. Success means the expected business status and body plus Auth, Policy, audit, idempotency (for mutations), tunnel, APISIX, and workload correlation. “Any HTTP 200–599” is never a success criterion.

| Scenario | Injection and recovery evidence | Required safety evidence |
|---|---|---|
| `kill_bridge` | LB stops new traffic to the unready Bridge; a second complete Bridge→Agent path takes over; service is restored | accepted mutations are not dispatched twice; all jobs have final classification |
| `kill_agent` | Bridge removes the failed Agent; Connector registers a new epoch after recovery | old-epoch responses are rejected; no old response completes a new request |
| `kill_connector` | Agent becomes unready; Connector/session recovers | retries occur only before a safe dispatch boundary; the absolute deadline is preserved |
| `auth_policy_replica` | one Auth or Policy replica exits and a ready peer takes over | no fail-open authorization and no unauthorized allow |
| `postgres_failover` | database becomes unavailable or performs a controlled primary change, then recovers | Auth/Policy fail closed, pool wait and audit buffer remain bounded, no reconnect storm |
| `redis_failover` | Redis becomes unavailable or performs controlled failover, then recovers | fallback is bounded, PEL drains, no silent job loss |
| `apisix_workload` | APISIX or workload is interrupted and restored | Connector error/timeout semantics are correct and response memory stays bounded |
| `network_impairment` | controlled latency/loss is applied and removed | deadline is not reset per hop; cancellation releases bounded resources within SLO |

## Machine gate

PowerShell:

```powershell
$env:SAG_FAULT_GATE_ACK = "AUTHORIZED_ISOLATED_ENVIRONMENT"
$env:SAG_PERF_ENVIRONMENT = "approved-staging"
$env:SAG_FAULT_SCENARIO_RUNNER = "C:\approved\sag-fault-adapter.ps1"
.\scripts\ops\run-production-fault-gate.ps1 -Scenario all -TrafficRps 350
```

Linux:

```bash
export SAG_FAULT_GATE_ACK=AUTHORIZED_ISOLATED_ENVIRONMENT
export SAG_PERF_ENVIRONMENT=approved-staging
export SAG_FAULT_SCENARIO_RUNNER=/opt/sag/bin/fault-adapter
bash scripts/ops/run-production-fault-gate.sh all 350
```

The adapter is deliberately environment-specific: it controls Kubernetes, VMs, managed PostgreSQL/Redis, or another approved platform and must restore each fault before returning. The PowerShell adapter receives `-Scenario`, `-TrafficRps`, `-ArtifactPath`, `-EnvironmentName`, and `-GitSha`. The shell adapter receives equivalent long options.

The orchestrator refuses destructive execution without the explicit acknowledgement, a named environment, an adapter, and a real Git SHA. It runs scenarios serially and validates `sag.production-fault-gate/v1` artifacts. A non-zero result is mandatory for any of these conditions:

- submitted request/job total does not equal the sum of final classifications;
- unknown jobs, duplicate mutation side effects, incorrect authorization, unready traffic acceptance, or permanent PEL entries;
- hard permits or PostgreSQL connections exceed their captured budgets;
- expected business status/body, Auth, Policy, or audit evidence is incomplete;
- TLS verification is absent, RTO exceeds its declared limit, or the service is not restored;
- a scenario-specific invariant in the table above is absent.

Validator-only checks, safe on a workstation:

```powershell
.\scripts\ops\test-production-fault-gate.ps1
```

```bash
bash scripts/ops/run-production-fault-gate.sh --self-test
```

Self-test output must never be recorded as a production fault-gate pass.

