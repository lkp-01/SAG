# Connector PostgreSQL Decoupling Implementation Plan

> **Status: Superseded (2026-07-26).** Do not execute this plan against the
> current code. Use
> [`2026-07-26-connector-postgres-decoupling-revision.md`](2026-07-26-connector-postgres-decoupling-revision.md).
> In particular, the old Task 3 signature and immediate-return instruction
> would remove deadline/cancellation behavior added after this plan was written.

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

> **For Codex:** Use the available `executing-plans` skill, execute one task at a time, and stop at each commit/checkpoint if the observed result differs from this plan.

**Goal:** Remove the Connector's direct dependency on Edge PostgreSQL while preserving the reverse-tunnel data plane, VPN-based APISIX management plane, Edge audit trail, and Connector Prometheus metrics.

**Architecture:** The Connector becomes a stateless forwarding process: it opens the outbound tunnel, forwards requests to intranet APISIX, and exports metrics. Agent and bridge remain the Edge-side audit writers. PostgreSQL stays on the Edge Docker network and its optional host port is bound to loopback rather than the VPN/LAN interface.

**Tech Stack:** Rust 2021, Tokio, tonic gRPC, reqwest, Prometheus metrics, Docker Compose, PowerShell verification.

---

## Scope and Invariants

Read the design first:

- `docs/plans/2026-07-25-connector-postgres-decoupling-design.md`

In scope:

- Remove `shared_storage`, database initialization, and audit/fault persistence from `sag-connector`.
- Retain all Connector forwarding and latency metrics.
- Remove Connector-only database and audit-queue configuration.
- Restrict the Edge PostgreSQL host binding to loopback.
- Update current deployment and operations documentation.

Out of scope:

- Removing VPN or internal DNS.
- Changing control-plane -> APISIX Admin connectivity.
- Changing the tunnel protobuf.
- Adding Connector telemetry delivery.
- Refactoring audit persistence in Agent, bridge, auth, policy, or control-plane services.
- Removing PostgreSQL from Edge services.
- Changing APISIX data/admin port exposure on the intranet host.

Before editing, create a dedicated branch/worktree if Git metadata is available:

```powershell
git rev-parse --show-toplevel
git switch -c refactor/connector-postgres-decoupling
```

If `git rev-parse` fails because this snapshot has no valid Git metadata, continue without commit steps and keep the task checkpoints.

### Task 1: Record the Architecture Decision

**Files:**

- Create: `docs/adr/0001-connector-postgres-decoupling.md`
- Reference: `docs/plans/2026-07-25-connector-postgres-decoupling-design.md`

**Step 1: Create the ADR**

Create the file with this content:

```markdown
# ADR-0001: Remove Direct PostgreSQL Access from sag-connector

- Status: Accepted
- Date: 2026-07-25

## Context

The business data plane uses the outbound reverse tunnel, while APISIX management uses VPN/internal DNS. The Connector additionally connects to Edge PostgreSQL solely for per-hop audit and fault persistence. That expands credentials and couples Connector startup to central database reachability.

## Decision

Remove `shared_storage` and all database writes from `sag-connector`. Keep tunnel-forward audit at `stealth-tunnel-agent`, ingress audit at `http-tunnel-bridge`, and hop observability in Connector Prometheus metrics. Keep the control-plane-to-APISIX VPN path unchanged. Bind Edge PostgreSQL to loopback for host administration while Edge containers continue using the Docker network.

## Consequences

- Connector can start and forward without PostgreSQL.
- Intranet deployment no longer needs Edge database credentials or Edge TCP/5432 access.
- Durable `service=sag-connector` audit/fault rows stop being produced.
- Existing Agent/bridge audit rows and Connector metrics remain.
- A future compliance requirement for Connector-local durable events requires a separate batched telemetry design.

## Rollback

Revert the Connector source and manifest, restore Connector storage variables and the prior PostgreSQL host binding, rebuild, and recreate the Connector container. No data migration is required.
```

**Step 2: Verify the ADR states the explicit non-goals**

Run:

```powershell
rg -n "VPN|APISIX|telemetry|Rollback" docs/adr/0001-connector-postgres-decoupling.md
```

Expected: matches for the unchanged VPN/APISIX management plane, deferred telemetry, and rollback.

**Step 3: Commit**

```powershell
git add docs/adr/0001-connector-postgres-decoupling.md docs/plans/2026-07-25-connector-postgres-decoupling-design.md docs/plans/2026-07-25-connector-postgres-decoupling.md
git commit -m "docs: decide connector database decoupling"
```

### Task 2: Add a Failing Architecture Regression Check

**Files:**

- Create: `scripts/ops/verify-connector-db-independence.ps1`

**Step 1: Write the regression script**

```powershell
$ErrorActionPreference = "Stop"

$repoRoot = (Resolve-Path (Join-Path $PSScriptRoot "..\..")).Path
$failures = [System.Collections.Generic.List[string]]::new()

function Assert-NoText {
    param(
        [string]$RelativePath,
        [string[]]$Forbidden
    )
    $path = Join-Path $repoRoot $RelativePath
    $content = Get-Content -Raw -LiteralPath $path
    foreach ($term in $Forbidden) {
        if ($content.Contains($term)) {
            $failures.Add("$RelativePath still contains '$term'")
        }
    }
}

function Assert-HasText {
    param(
        [string]$RelativePath,
        [string]$Required
    )
    $path = Join-Path $repoRoot $RelativePath
    $content = Get-Content -Raw -LiteralPath $path
    if (-not $content.Contains($Required)) {
        $failures.Add("$RelativePath is missing '$Required'")
    }
}

Assert-NoText "proxy/connectors/sag-connector/src/main.rs" @(
    "shared_storage",
    "AuditJob",
    "SAG_CONNECTOR_AUDIT_QUEUE",
    "connector_audit_dropped_total",
    "resolve_storage_backend",
    "SAG_POSTGRES_DSN"
)
Assert-NoText "proxy/connectors/sag-connector/Cargo.toml" @(
    "shared_storage",
    "uuid.workspace"
)
Assert-NoText "docker-compose.intra.yml" @(
    "SAG_STORAGE_BACKEND:",
    "SAG_CONNECTOR_AUDIT_QUEUE"
)
Assert-NoText "docker-compose.yml" @(
    "SAG_CONNECTOR_AUDIT_QUEUE"
)
Assert-NoText "intra-host.env.example" @(
    "SAG_POSTGRES_DSN"
)
Assert-NoText ".env.example" @(
    "SAG_CONNECTOR_AUDIT_QUEUE"
)
Assert-HasText "docker-compose.edge.yml" '127.0.0.1:5432:5432'

Push-Location $repoRoot
try {
    $tree = (& cargo tree -p sag-connector -e normal 2>&1 | Out-String)
    if ($LASTEXITCODE -ne 0) {
        throw "cargo tree failed:`n$tree"
    }
    if ($tree -match "(?m)shared_storage") {
        $failures.Add("cargo tree still contains shared_storage")
    }
}
finally {
    Pop-Location
}

if ($failures.Count -gt 0) {
    $failures | ForEach-Object { Write-Host "FAIL: $_" -ForegroundColor Red }
    exit 1
}

Write-Host "PASS: sag-connector has no direct database dependency."
```

**Step 2: Run the check and verify it fails before implementation**

Run:

```powershell
pwsh -File scripts/ops/verify-connector-db-independence.ps1
```

Expected: exit code `1`; failures mention Connector source/manifest, intra Compose/env, audit queue, and PostgreSQL host binding.

**Step 3: Commit the failing check**

```powershell
git add scripts/ops/verify-connector-db-independence.ps1
git commit -m "test: guard connector database independence"
```

### Task 3: Remove Storage and Audit Persistence from sag-connector

**Files:**

- Modify: `proxy/connectors/sag-connector/src/main.rs`
- Modify: `proxy/connectors/sag-connector/Cargo.toml`
- Modify: `Cargo.lock`

**Step 1: Remove storage-only imports and types**

In `main.rs`:

- Delete the entire `use shared_storage::{...};` import.
- Delete `AuditJob`.
- Keep `tokio::sync::mpsc`; it is still required by the tunnel and accept queues.

**Step 2: Remove database initialization from `run_tunnel_once`**

Delete:

- `resolve_storage_backend()`.
- The PostgreSQL/SQLite logging `match`.
- `build_store_from_env()`.
- `ensure_store_schema()`.

The HTTP client construction must flow directly into stream-buffer setup.

**Step 3: Remove the Connector audit queue**

Delete:

- `SAG_CONNECTOR_AUDIT_QUEUE` parsing.
- `mpsc::channel::<AuditJob>`.
- The background audit writer.
- `audit_disp`, `audit_i`, and the audit argument passed to `handle_forward`.
- `audit_queue_cap` from the tunnel-up log.
- `drop(audit_tx)` during shutdown.

Do not modify:

- `job_tx` / `job_rx`.
- `SAG_CONNECTOR_ACCEPT_QUEUE`.
- `connector_forward_accept_wait_seconds`.
- `connector_forward_upstream_seconds`.
- `connector_forward_out_send_seconds`.
- `connector_forward_total` or `connector_forward_duration_seconds`.

**Step 4: Simplify `handle_forward`**

Change the function parameters from:

```rust
async fn handle_forward(
    client: &Client,
    apisix_base: &str,
    req: ForwardRequest,
    audit_tx: &mpsc::Sender<AuditJob>,
    max_response_body: u64,
) -> ForwardResponse
```

to:

```rust
async fn handle_forward(
    client: &Client,
    apisix_base: &str,
    req: ForwardRequest,
    max_response_body: u64,
) -> ForwardResponse
```

After constructing `resp`, return it immediately. Delete construction and submission of `AuditLogRecord` and `FaultEventRecord`, including `connector_audit_dropped_total`.

**Step 5: Remove obsolete PostgreSQL reconnect classification**

- Delete the `"postgres"` branch from `tunnel_error_class`.
- Delete the special `if err.to_lowercase().contains("postgres")` warning in `main`.
- Retain transport, HTTP/2-body, and generic error classification.

**Step 6: Remove dependencies**

Delete from `proxy/connectors/sag-connector/Cargo.toml`:

```toml
shared_storage = { path = "../../../shared/storage" }
uuid.workspace = true
```

Do not remove workspace-level `shared_storage` or `uuid`; other services still use them.

**Step 7: Format and compile**

Run:

```powershell
cargo fmt --package sag-connector
cargo check -p sag-connector
cargo test -p sag-connector
cargo tree -p sag-connector -e normal | Select-String shared_storage
```

Expected:

- `cargo check` succeeds.
- `cargo test` succeeds.
- The final command prints no match.
- `Cargo.lock` no longer lists `shared_storage` or `uuid` under the `sag-connector` package dependency list; the packages themselves remain because other workspace members use them.

**Step 8: Commit**

```powershell
git add proxy/connectors/sag-connector/src/main.rs proxy/connectors/sag-connector/Cargo.toml Cargo.lock
git commit -m "refactor: remove connector database persistence"
```

### Task 4: Remove Connector Database Configuration and Restrict PostgreSQL

**Files:**

- Modify: `docker-compose.intra.yml`
- Modify: `docker-compose.yml`
- Modify: `docker-compose.edge.yml`
- Modify: `intra-host.env.example`
- Modify: `.env.dualhost.example`
- Modify: `.env.example`

**Step 1: Clean the intra Connector service**

In `docker-compose.intra.yml`:

- Remove `SAG_STORAGE_BACKEND`.
- Remove `SAG_CONNECTOR_AUDIT_QUEUE`.
- Change the comment listing required `.env.intra` values from “Tunnel / Postgres / connector id / gRPC TLS name” to “Tunnel / connector id / gRPC TLS name”.

In `docker-compose.yml`, remove only:

```yaml
SAG_CONNECTOR_AUDIT_QUEUE: "8192"
```

Do not remove storage configuration from Edge services.

**Step 2: Clean environment examples**

In `intra-host.env.example`:

- Delete `SAG_POSTGRES_DSN`.
- Rewrite the introductory comment so moving Edge requires changing only `SAG_TUNNEL_ENDPOINT`.

In `.env.dualhost.example`:

- Delete `SAG_EDGE_POSTGRES_HOST`.
- Delete `SAG_EDGE_POSTGRES_PORT`.
- Delete the Connector-section `SAG_POSTGRES_DSN`.
- Preserve the Edge storage section's `SAG_STORAGE_BACKEND` and Docker-internal `SAG_POSTGRES_DSN=...@postgres:5432/sag`.

In `.env.example`, remove the commented `SAG_CONNECTOR_AUDIT_QUEUE`. Preserve generic storage examples because Edge services use them.

**Step 3: Restrict Edge PostgreSQL host access**

In `docker-compose.edge.yml`, change:

```yaml
ports:
  - "5432:5432"
```

to:

```yaml
ports:
  - "127.0.0.1:5432:5432"
```

Do not change Edge service DSNs; containers continue using `postgres:5432`.

**Step 4: Run the architecture guard**

Run:

```powershell
pwsh -File scripts/ops/verify-connector-db-independence.ps1
```

Expected: `PASS: sag-connector has no direct database dependency.`

**Step 5: Validate Compose rendering**

Run:

```powershell
docker compose -f docker-compose.edge.yml config | Select-String "127.0.0.1:5432"
```

Expected: rendered PostgreSQL binding contains host IP `127.0.0.1`.

For the intra file, use the operator's existing `.env.intra`:

```powershell
docker compose -f docker-compose.intra.yml -f docker-compose.release.intra.yml config | Select-String "SAG_POSTGRES_DSN|SAG_STORAGE_BACKEND|SAG_CONNECTOR_AUDIT_QUEUE"
```

Expected: no match inside the rendered `sag-connector` service. If the operator's `.env.intra` still contains an old database variable, remove it before recreating the container.

**Step 6: Commit**

```powershell
git add docker-compose.intra.yml docker-compose.yml docker-compose.edge.yml intra-host.env.example .env.dualhost.example .env.example
git commit -m "ops: remove connector database configuration"
```

### Task 5: Update Operations and Architecture Documentation

**Files:**

- Modify: `README.md`
- Modify: `DUAL_HOST_OPERATIONS.md`
- Modify: `PROJECT_HANDOFF.md`
- Modify: `SERVER_OPS_QUICKREF.md`
- Modify: `Context_Handoff.md`
- Modify: `docs/ops/async-patterns-runbook.md`
- Modify: `docs/ops/high-concurrency-reliability-master-plan.md`
- Modify: `docs/ops/rate-limit-circuit-breaker-runbook.md`
- Modify: `docs/ops/runbook.md`
- Modify: `docs/ops/config-dictionary.md`

**Step 1: Remove obsolete Connector database instructions**

Replace instructions that require Connector `SAG_POSTGRES_DSN` or `SAG_STORAGE_BACKEND` with:

```text
The Connector requires the Edge tunnel endpoint and mTLS material, but no database DSN. Central persistence is owned by Edge services.
```

In `DUAL_HOST_OPERATIONS.md`, update all four current references around:

- dual-host connectivity prerequisites;
- `.env.intra` creation;
- required variable checklist;
- Edge-IP migration/recovery.

Do not delete generic PostgreSQL documentation for Edge services.

**Step 2: Update asynchronous-pattern documentation**

Replace:

```text
connector audit -> mpsc SAG_CONNECTOR_AUDIT_QUEUE -> background PG/SQLite write
```

with:

```text
connector forwarding -> Prometheus hop metrics
agent/bridge forwarding -> Edge-side audit_logs / fault_events
```

Remove tuning advice for `SAG_CONNECTOR_AUDIT_QUEUE` and `connector_audit_dropped_total`.

**Step 3: Clarify the configuration dictionary**

Keep `SAG_STORAGE_BACKEND` and `SAG_POSTGRES_DSN`, but state that they apply to Edge persistence services and are intentionally not consumed by `sag-connector`.

**Step 4: Verify stale guidance is gone**

Run:

```powershell
rg -n "SAG_CONNECTOR_AUDIT_QUEUE|connector_audit_dropped_total" README.md DUAL_HOST_OPERATIONS.md PROJECT_HANDOFF.md SERVER_OPS_QUICKREF.md Context_Handoff.md docs/ops
rg -n "sag-connector.*SAG_POSTGRES_DSN|Connector.*Postgres|connector.*PG/SQLite" README.md DUAL_HOST_OPERATIONS.md PROJECT_HANDOFF.md SERVER_OPS_QUICKREF.md Context_Handoff.md docs/ops
```

Expected:

- First command returns no active configuration guidance.
- Second command returns no statement requiring Connector database access.
- Historical text may remain only if explicitly labelled “before ADR-0001”.

**Step 5: Commit**

```powershell
git add README.md DUAL_HOST_OPERATIONS.md PROJECT_HANDOFF.md SERVER_OPS_QUICKREF.md Context_Handoff.md docs/ops
git commit -m "docs: document stateless connector boundary"
```

### Task 6: Perform Static and Workspace Verification

**Files:**

- Verify only; no expected edits except formatter or lockfile corrections.

**Step 1: Run focused checks**

```powershell
pwsh -File scripts/ops/verify-connector-db-independence.ps1
cargo fmt --check
cargo check -p sag-connector
cargo test -p sag-connector
cargo test -p stealth-tunnel-agent
cargo check --workspace
```

Expected: every command succeeds.

**Step 2: Confirm storage ownership**

Run:

```powershell
rg -n "AuditLogsStore::insert|FaultEventsStore::insert" proxy/connectors/sag-connector proxy/agents/stealth-tunnel-agent proxy/http-tunnel-bridge
```

Expected:

- No matches under `proxy/connectors/sag-connector`.
- Matches remain under Agent and bridge.

**Step 3: Confirm Connector observability remains**

Run:

```powershell
rg -n "connector_forward_total|connector_forward_duration_seconds|connector_forward_upstream_seconds|connector_forward_accept_wait_seconds|connector_forward_out_send_seconds" proxy/connectors/sag-connector/src/main.rs
```

Expected: all five metric families remain.

**Step 4: Inspect the final diff**

```powershell
git diff --check
git status --short
```

Expected: no whitespace errors and only intended files are modified.

**Step 5: Commit verification corrections, if any**

```powershell
git status --short
# Stage only formatter/lockfile corrections produced by this task; do not stage unrelated files.
git commit -m "test: verify connector database decoupling"
```

Skip this commit if Task 6 produced no changes.

### Task 7: Roll Out and Prove Failure Isolation

**Files:**

- Deployment state only.
- Do not edit committed files during this task.

**Step 1: Update the untracked intra environment**

On the Intra host, remove:

```text
SAG_POSTGRES_DSN
SAG_STORAGE_BACKEND
SAG_CONNECTOR_AUDIT_QUEUE
```

from `.env.intra`. Keep `SAG_TUNNEL_ENDPOINT`, Connector identity, APISIX base URL, and mTLS variables.

**Step 2: Rebuild and recreate Connector**

```bash
docker compose -f docker-compose.intra.yml -f docker-compose.release.intra.yml build sag-connector
docker compose -f docker-compose.intra.yml -f docker-compose.release.intra.yml up -d --force-recreate sag-connector
```

Expected: Connector starts without any database configuration.

**Step 3: Verify the tunnel**

On the Intra host:

```bash
curl -fsS http://127.0.0.1:9103/metrics | grep 'connector_tunnel_up'
docker logs --tail 100 sag-connector
```

Expected:

- `connector_tunnel_up` is `1`.
- Logs contain `sag-connector tunnel up`.
- Logs contain no PostgreSQL connection or storage-backend message.

**Step 4: Verify PostgreSQL is no longer cross-host reachable**

From the Intra host:

```bash
nc -vz <EDGE_VPN_IP> 5432
```

Expected: connection fails.

On the Edge host:

```bash
docker exec sag-postgres pg_isready -U postgres -d sag
```

Expected: PostgreSQL is healthy for Edge containers.

**Step 5: Run end-to-end traffic**

From the Edge host:

```powershell
pwsh -File scripts/smoke-dataplane.ps1
```

Or use the dual-host check:

```bash
bash scripts/ops/check-dualhost-tunnel.sh
```

Expected:

- Bridge and northbound probes return the normal success/policy result.
- No `connector tunnel is unhealthy`.
- Connector forwarding counters increase.

**Step 6: Verify Edge audit still exists**

Use the existing admin UI or authenticated `GET /api/v1/audit/logs` and filter for the test request's application/path.

Expected:

- `stealth-tunnel-agent` and/or `http-tunnel-bridge` records are present.
- New `sag-connector` durable audit rows are absent by design.
- Connector Prometheus metrics still show the request.

**Step 7: Observe for one normal operating window**

Check:

```bash
curl -fsS http://127.0.0.1:9103/metrics | grep -E 'connector_tunnel_up|connector_forward_total|connector_tunnel_drop_total'
```

Expected: tunnel remains up, forwards increase, and no new database-related drop class exists.

**Step 8: Final commit/tag**

```powershell
git status --short
git log --oneline -5
```

Expected: clean worktree and the task commits listed above.

## Rollback Procedure

If forwarding fails after rollout:

1. Revert the implementation commits in reverse order.
2. Restore the previous Connector database variables in `.env.intra`.
3. Restore `"5432:5432"` only if the old Connector must temporarily access Edge PostgreSQL.
4. Rebuild and force-recreate `sag-connector`.
5. Re-run `scripts/smoke-dataplane.ps1`.

No database migration or data restoration is required.
