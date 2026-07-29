# Connector PostgreSQL Decoupling Revision Implementation Plan

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

> **For Codex:** Use the available `executing-plans` skill. Stop at a task
> checkpoint if the observed code differs from the baseline described here.

**Goal:** Keep `sag-connector` free of durable storage and Edge database access without regressing the implemented deadline, cancellation, idempotency, or explicit multi-Agent tunnel contract.

**Architecture:** Connector retains request-lifetime in-memory coordination but owns no durable state. Bridge creates the logical request, attempt, and absolute deadline; Agent owns authorization, generation-aware pending cleanup, and the Edge-side durable idempotency ledger; all production Agents share Edge PostgreSQL. The protocol participants are deployed as one drained, compatible release rather than mixed online.

**Tech Stack:** Rust 2021, Tokio, tonic/prost, reqwest, Redis Streams, SQLite/PostgreSQL through `shared_storage`, Prometheus metrics, Docker Compose, PowerShell/Bash verification.

---

## Execution rules and baseline

Read first:

- `docs/plans/2026-07-26-connector-postgres-decoupling-revision-design.md`
- `docs/plans/2026-07-26-request-deadline-cancellation-design.md`
- `docs/ops/request-deadline-cancellation.md`

Do not execute the superseded 2026-07-25 plan. The current source already has
the database-independent Connector and the request-correctness implementation;
steps that are already satisfied are verification-only and must not create
cosmetic rewrites.

In scope:

- Guard Connector database independence.
- Preserve Proto, Connector, Agent, Bridge, and Storage correctness fields and
  behavior.
- Strengthen tests, metrics checks, documentation, and release procedure.
- Keep Edge PostgreSQL loopback-only and recreate its container without deleting
  the volume.

Out of scope:

- New retries, telemetry RPCs, queues, databases, or APISIX topology.
- Moving the idempotency ledger out of Agent/Edge.
- Making PostgreSQL optional for mutating request correctness.
- Automatic recovery or stealing of durable `pending` claims.
- Live rolling upgrade between protocol generations.

This workspace snapshot may have no valid Git metadata. Run each Commit step
only if `git rev-parse --show-toplevel` succeeds; otherwise record the checkpoint
and continue without inventing repository history.

### Task 1: Supersede the stale plan and record the revised decision

**Files:**

- Modify: `docs/plans/2026-07-25-connector-postgres-decoupling-design.md:3-6`
- Modify: `docs/plans/2026-07-25-connector-postgres-decoupling.md:1-12`
- Create: `docs/plans/2026-07-26-connector-postgres-decoupling-revision-design.md`
- Modify: `docs/adr/0001-connector-postgres-decoupling.md`

**Step 1: Mark both old documents Superseded**

Add a link to the revision and an explicit warning that old Task 3 predates
deadline/cancellation/idempotency and must not be run against current source.

**Step 2: Record the clarified ADR boundary**

The ADR must state all of the following:

- Connector has no durable state or database dependency, but retains ephemeral
  cancellation, queue, in-flight, deadline, attempt, and tunnel state.
- Agent owns the durable idempotency decision before Connector dispatch.
- Multiple Agents share one PostgreSQL; SQLite is single-Agent only.
- Rollout/rollback replaces Connector, Agent, and Bridge as one compatible set.
- Rollback never restores Connector DSN or cross-host PostgreSQL exposure.

**Step 3: Verify the decision text**

Run:

```powershell
rg -n "Superseded|not a stateless|database-independent|idempotency|mixed|rollback" docs/plans/2026-07-25-connector-postgres-decoupling*.md docs/plans/2026-07-26-connector-postgres-decoupling-revision-design.md docs/adr/0001-connector-postgres-decoupling.md
```

Expected: the old files point to the revision; the revision/ADR contain every
current boundary and no text instructs operators to restore live cross-host
database access.

**Step 4: Commit if Git metadata is valid**

```powershell
git add docs/plans/2026-07-25-connector-postgres-decoupling-design.md docs/plans/2026-07-25-connector-postgres-decoupling.md docs/plans/2026-07-26-connector-postgres-decoupling-revision-design.md docs/adr/0001-connector-postgres-decoupling.md
git commit -m "docs: revise connector database boundary"
```

### Task 2: Expand the architecture regression guard across every hop

**Files:**

- Modify: `scripts/ops/verify-connector-db-independence.ps1`
- Reference: `shared/tunnel-proto/proto/tunnel.proto`
- Reference: `proxy/connectors/sag-connector/src/main.rs`
- Reference: `proxy/agents/stealth-tunnel-agent/src/connector_registry.rs`
- Reference: `proxy/agents/stealth-tunnel-agent/src/grpc_server.rs`
- Reference: `proxy/http-tunnel-bridge/src/main.rs`
- Reference: `proxy/http-tunnel-bridge/src/queue.rs`
- Reference: `shared/storage/src/idempotency.rs`

**Step 1: Run the current guard as the pre-change baseline**

```powershell
pwsh -NoProfile -File scripts/ops/verify-connector-db-independence.ps1
```

Expected: PASS. If it fails, resolve the baseline contradiction before editing
the guard.

**Step 2: Add positive preservation assertions**

Reuse the existing `Assert-HasText` helper and add this exact block after the
negative Connector assertions:

```powershell
Assert-HasText "shared/tunnel-proto/proto/tunnel.proto" "CancelRequest cancel = 5;"
Assert-HasText "shared/tunnel-proto/proto/tunnel.proto" "string attempt_id = 7;"
Assert-HasText "shared/tunnel-proto/proto/tunnel.proto" "int64 deadline_unix_ms = 8;"
Assert-HasText "shared/tunnel-proto/proto/tunnel.proto" "string idempotency_key = 9;"
Assert-HasText "shared/tunnel-proto/proto/tunnel.proto" "string attempt_id = 5;"

Assert-HasText "proxy/connectors/sag-connector/src/main.rs" "struct CancelState"
Assert-HasText "proxy/connectors/sag-connector/src/main.rs" "cancel: Arc<CancelState>"
Assert-HasText "proxy/connectors/sag-connector/src/main.rs" "SAG_TUNNEL_ENDPOINTS"
Assert-HasText "proxy/connectors/sag-connector/src/main.rs" "connector_forward_deadline_total"
Assert-HasText "proxy/connectors/sag-connector/src/main.rs" "connector_forward_cancelled_total"

Assert-HasText "proxy/agents/stealth-tunnel-agent/src/connector_registry.rs" "pub struct PendingRequest"
Assert-HasText "proxy/agents/stealth-tunnel-agent/src/connector_registry.rs" "remove_pending_if_generation"
Assert-HasText "proxy/agents/stealth-tunnel-agent/src/connector_registry.rs" "agent_late_response_total"
Assert-HasText "proxy/agents/stealth-tunnel-agent/src/grpc_server.rs" "IdempotencyStore::claim"
Assert-HasText "proxy/agents/stealth-tunnel-agent/src/grpc_server.rs" "IdempotencyStore::complete"

Assert-HasText "proxy/http-tunnel-bridge/src/main.rs" "request.set_timeout(remaining)"
Assert-HasText "proxy/http-tunnel-bridge/src/queue.rs" "bridge_queue_expired_total"
Assert-HasText "shared/storage/src/idempotency.rs" "state = 'completed' AND expires_at_ms"
Assert-HasText "shared/storage/src/idempotency.rs" "release_undispatched"
```

**Step 3: Add forbidden regression assertions**

```powershell
Assert-NoText "proxy/connectors/sag-connector/src/main.rs" @(
    "shared_storage",
    "SAG_STORAGE_BACKEND",
    "SAG_POSTGRES_DSN",
    "SAG_CONNECTOR_AUDIT_QUEUE",
    "AuditLogsStore::insert",
    "FaultEventsStore::insert"
)
Assert-NoText "proxy/http-tunnel-bridge/src/main.rs" @(
    "for attempt in 0..2"
)
```

Do not add a rule forbidding `shared_storage` in Agent or Bridge. Those are Edge
storage owners. Do not add a rule forbidding in-memory maps or queues in
Connector.

**Step 4: Run the expanded guard**

```powershell
pwsh -NoProfile -File scripts/ops/verify-connector-db-independence.ps1
```

Expected: PASS and the final message still says Connector has no direct database
dependency.

**Step 5: Commit if available**

```powershell
git add scripts/ops/verify-connector-db-independence.ps1
git commit -m "test: guard connector request correctness boundary"
```

### Task 3: Preserve the cancellation-safe Connector while enforcing database independence

**Files:**

- Verify/Modify only if required: `proxy/connectors/sag-connector/src/main.rs:1-950`
- Verify/Modify only if required: `proxy/connectors/sag-connector/Cargo.toml`
- Verify/Modify only if required: `Cargo.lock`

This replaces Task 3 from the superseded plan. On the current baseline it should
be a no-op apart from tests or a real storage artifact discovered by Task 2.

**Step 1: Lock the current signature before cleanup**

The required signature is:

```rust
async fn handle_forward(
    client: &Client,
    apisix_base: &str,
    req: ForwardRequest,
    max_response_body: u64,
    cancel: Arc<CancelState>,
) -> ForwardResponse
```

Never replace it with the old four-argument signature. Never remove the cancel
argument, `remaining_until`, `attempt_key`, or response-body streaming loop.

**Step 2: Run Connector tests before any edit**

```powershell
cargo test -p sag-connector
```

Expected: the sticky-cancellation and attempt-key tests pass.

**Step 3: Remove only durable-storage artifacts if they actually exist**

Allowed removals are limited to:

- `shared_storage` imports/dependency;
- database backend/schema initialization;
- Connector `AuditLogRecord`/`FaultEventRecord` construction and writer queue;
- Connector-only database/audit-queue variables and metric.

On the 2026-07-26 baseline these artifacts are already absent. If the searches
below print no matches, make no source edit:

```powershell
rg -n "shared_storage|SAG_STORAGE_BACKEND|SAG_POSTGRES_DSN|SAG_CONNECTOR_AUDIT_QUEUE|AuditLogsStore|FaultEventsStore" proxy/connectors/sag-connector
```

Expected: no matches.

**Step 4: Explicitly preserve request-correctness paths**

After any necessary removal, verify all of these remain:

```powershell
rg -n "CancelState|cancellations|duplicate_attempt|remaining_until|deadline_unix_ms|attempt_id|idempotency-key|http_send|http_body|SAG_TUNNEL_ENDPOINTS|capacity_divisor" proxy/connectors/sag-connector/src/main.rs
```

Expected: matches for cancellation registration before enqueue, duplicate
attempt rejection, queue and HTTP deadline checks, cancel races for send/body,
attempt IDs in responses, idempotency-key propagation, and per-Agent capacity
division.

Do not follow the old instruction to “construct `resp` and return immediately.”
The complete match/streaming path must still convert body errors to 502/504,
return cancellation as 499, and attach the matching attempt ID.

**Step 5: Format, test, and inspect dependencies**

```powershell
cargo fmt --package sag-connector -- --check
cargo check -p sag-connector
cargo test -p sag-connector
cargo tree -p sag-connector -e normal | Select-String "shared_storage|tokio-postgres|rusqlite"
```

Expected: checks pass and the dependency search prints nothing.

**Step 6: Commit only if Task 3 made a real correction**

```powershell
git add proxy/connectors/sag-connector/src/main.rs proxy/connectors/sag-connector/Cargo.toml Cargo.lock
git commit -m "refactor: keep connector database independent"
```

Skip the commit when the task is verification-only.

### Task 4: Cover Proto, Connector, Agent, Bridge, and Storage correctness scenarios

**Files:**

- Modify: `proxy/connectors/sag-connector/src/main.rs` test module
- Modify: `proxy/agents/stealth-tunnel-agent/src/connector_registry.rs` test module
- Modify: `proxy/agents/stealth-tunnel-agent/src/grpc_server.rs` test module
- Modify: `proxy/http-tunnel-bridge/src/main.rs` test module
- Modify: `proxy/http-tunnel-bridge/src/queue.rs` test module
- Modify: `shared/storage/src/idempotency.rs` test module
- Optional gated integration test: `shared/storage/tests/postgres_idempotency.rs`

**Step 1: Preserve Proto field/tag compatibility through the guard**

Run:

```powershell
cargo test -p sag-tunnel-proto
pwsh -NoProfile -File scripts/ops/verify-connector-db-independence.ps1
```

Expected: generated Proto code compiles and the exact tag assertions pass.

**Step 2: Add Connector unit coverage**

Add tests that prove:

- cancellation is sticky for early and late waiters;
- empty attempt falls back only to logical request ID and a supplied attempt is
  never replaced;
- an expired absolute deadline produces no remaining budget;
- endpoint parsing removes blanks/duplicates and capacity division never
  becomes zero;
- every generated error response carries the matching attempt ID.

Run:

```powershell
cargo test -p sag-connector
```

Expected: all tests pass without a database service.

**Step 3: Keep Agent RAII/pending coverage**

The test suite must prove:

- duplicate attempt registration cannot overwrite the first waiter;
- dropping a non-terminal guard releases its permit, removes only its
  generation, updates pending count, and emits one cancel;
- response completion removes once and emits no cancel;
- stale stream unregister cannot remove a newer stream;
- late responses increment a classified metric path and cannot complete another
  attempt.

Run:

```powershell
cargo test -p stealth-tunnel-agent connector_registry
```

Expected: all pending/stream-generation tests pass.

**Step 4: Extend durable ledger tests**

For SQLite and the gated PostgreSQL test, cover:

- two concurrent claims yield exactly one `Claimed` and one `Pending`;
- same key with another fingerprint is `Conflict`;
- completed status/headers/body replay exactly;
- only the owner can release before dispatch;
- elapsed time never steals or deletes a pending claim;
- only expired completed rows are reclaimed;
- completion failure is surfaced and does not authorize redispatch.

Use `SAG_TEST_POSTGRES_DSN` only for the PostgreSQL integration test; skip it
with an explicit message when unset.

```powershell
cargo test -p shared_storage idempotency
$env:SAG_TEST_POSTGRES_DSN = "postgres://postgres:postgres@127.0.0.1:5432/sag"
cargo test -p shared_storage --test postgres_idempotency -- --ignored --nocapture
Remove-Item Env:SAG_TEST_POSTGRES_DSN
```

Expected: SQLite always passes; PostgreSQL passes when the isolated test database
is available.

**Step 5: Extend Bridge preservation coverage**

Add tests that prove:

- the deadline is set before body collection;
- mutating methods require a stable idempotency key and read-only methods do not;
- queued serialization round-trips request ID, attempt ID, absolute deadline,
  idempotency key, headers, and body;
- an expired queue payload is acknowledged/failed without invoking Forward;
- a failed unary Forward reconnects only for a later request and is never
  retried in the same request.

Run:

```powershell
cargo test -p http-tunnel-bridge
```

Expected: all non-Redis tests pass; the Redis 7 test remains explicitly ignored
unless `SAG_TEST_REDIS_URL` is provided.

**Step 6: Run the critical end-to-end scenario matrix**

With isolated PostgreSQL, Redis, Agent, Connector, and a counting mock APISIX:

1. same write/key/payload twice -> one APISIX hit and one replay;
2. same key/different payload -> 409 and no second APISIX hit;
3. cancel before Connector dispatch -> zero APISIX hits;
4. cancel during delayed HTTP -> future stops and permit/pending return to zero;
5. expired Redis job -> no Agent/Connector/APISIX hit;
6. late response -> late counter increments and another attempt remains untouched;
7. PostgreSQL down before claim -> unavailable and zero Connector hits;
8. PostgreSQL down after dispatch/before completion -> indeterminate error and no
   automatic second dispatch.

Save request IDs, attempt IDs, mock hit counts, and metric snapshots under the
existing test artifact convention. Do not weaken a failure into a retry.

**Step 7: Commit**

```powershell
git add proxy/connectors/sag-connector/src/main.rs proxy/agents/stealth-tunnel-agent/src/connector_registry.rs proxy/agents/stealth-tunnel-agent/src/grpc_server.rs proxy/http-tunnel-bridge/src/main.rs proxy/http-tunnel-bridge/src/queue.rs shared/storage/src/idempotency.rs shared/storage/tests/postgres_idempotency.rs
git commit -m "test: cover tunnel cancellation and idempotency boundaries"
```

### Task 5: Correct stale operational descriptions and version policy

**Files:**

- Modify: `docs/ops/request-deadline-cancellation.md`
- Modify: `docs/ops/high-concurrency-reliability-master-plan.md`
- Modify: `docs/ops/rate-limit-circuit-breaker-runbook.md`
- Modify: `docs/ops/timeout-deadline-runbook.md`
- Modify: `docs/ops/bridge-grpc-channel-pool-future.md`
- Modify: `proxy/agents/stealth-tunnel-agent/src/main.rs` diagnostic text
- Modify: `docker-compose.edge.yml` comments
- Modify: `shared/storage/src/lib.rs` module documentation

**Step 1: Replace nonexistent `drop_pending` descriptions**

Name the actual mechanism: `SAG_MAX_PENDING_WAITERS`, owned semaphore permit,
attempt-keyed pending entry, `PendingRequest::drop`, generation-aware removal,
pending gauge, and best-effort `CancelRequest`.

**Step 2: Remove obsolete coalescing references**

The debug cache-clearing description must list policy/negative caches only and
must explicitly exclude tunnels and the durable idempotency ledger. Do not
reintroduce policy coalescing.

**Step 3: Remove old retry wording**

Compose/runbook comments must say each logical request has one gRPC attempt.
Reconnect prepares a later request only.

**Step 4: Document the PostgreSQL failure boundary and mixed-version ban**

Add the fail-closed claim, indeterminate completion, cold-start schema check,
shared multi-Agent ledger, and whole-set upgrade/rollback behavior to the
request-cancellation runbook.

**Step 5: Search for stale guidance**

```powershell
rg -n "drop_pending|coalesc|coalesce|both gRPC attempts|two-attempt" docs/ops proxy/agents/stealth-tunnel-agent/src/main.rs docker-compose.edge.yml
```

Expected: no stale active guidance. Historical deadline design text may say
coalescing was removed; that statement is correct.

**Step 6: Commit**

```powershell
git add docs/ops/request-deadline-cancellation.md docs/ops/high-concurrency-reliability-master-plan.md docs/ops/rate-limit-circuit-breaker-runbook.md docs/ops/timeout-deadline-runbook.md docs/ops/bridge-grpc-channel-pool-future.md proxy/agents/stealth-tunnel-agent/src/main.rs docker-compose.edge.yml shared/storage/src/lib.rs
git commit -m "docs: align tunnel operations with request correctness"
```

### Task 6: Run static, test, architecture, metrics, and Compose verification

**Files:**

- Verification only; formatter corrections are allowed.

**Step 1: Format and run focused package tests**

```powershell
cargo fmt --all -- --check
cargo test -p sag-tunnel-proto
cargo test -p sag-connector
cargo test -p shared_storage
cargo test -p stealth-tunnel-agent
cargo test -p http-tunnel-bridge
```

Expected: all non-environment-gated tests pass.

**Step 2: Run compilation and workspace tests**

```powershell
cargo check --workspace
cargo test --workspace
```

Expected: both commands pass. Record an external dependency blocker rather than
deleting or ignoring a correctness test.

**Step 3: Run the architecture guard**

```powershell
pwsh -NoProfile -File scripts/ops/verify-connector-db-independence.ps1
```

Expected: PASS.

**Step 4: Verify metric families and structured identifiers**

```powershell
rg -n "agent_pending_waiters|agent_late_response_total|agent_cancel_total|agent_idempotency_total|agent_forward_total" proxy/agents/stealth-tunnel-agent/src
rg -n "connector_cancel_total|connector_forward_cancelled_total|connector_forward_deadline_total|connector_forward_body_error_total|connector_forward_accept_wait_seconds" proxy/connectors/sag-connector/src
rg -n "bridge_forward_error_total|bridge_grpc_channel_forward_err_total|bridge_queue_expired_total|bridge_request_reject_total" proxy/http-tunnel-bridge/src
rg -n "request_id|attempt_id|trace_id|deadline_unix_ms|stage" proxy/connectors/sag-connector/src/main.rs proxy/agents/stealth-tunnel-agent/src/grpc_server.rs proxy/http-tunnel-bridge/src/main.rs
```

Expected: every family and identifier has an active code match.

**Step 5: Render all deployment shapes**

Use the real operator `.env.intra` for the Intra render; it is intentionally not
committed.

```powershell
docker compose -f docker-compose.yml -f docker-compose.release.yml config --quiet
docker compose -f docker-compose.edge.yml -f docker-compose.release.edge.yml config --quiet
docker compose -f docker-compose.intra.yml -f docker-compose.release.intra.yml config --quiet
```

Then inspect the rendered boundary:

```powershell
docker compose -f docker-compose.edge.yml -f docker-compose.release.edge.yml config | Select-String "127.0.0.1:5432|SAG_IDEMPOTENCY_TTL_SEC|SAG_POSTGRES_DSN"
docker compose -f docker-compose.intra.yml -f docker-compose.release.intra.yml config | Select-String "SAG_STORAGE_BACKEND|SAG_POSTGRES_DSN|SAG_CONNECTOR_AUDIT_QUEUE"
```

Expected: every render succeeds; Edge shows loopback PostgreSQL and Agent shared
storage; the Intra Connector search prints no storage/audit-queue variable.

**Step 6: Commit formatter-only corrections if any**

```powershell
git status --short
git diff --check
# Stage only formatter corrections produced by this task, then commit them.
git commit -m "test: verify connector decoupling revision"
```

Skip this step when Git metadata is unavailable or no correction exists.

### Task 7: Perform a drained, coordinated release with PostgreSQL recreation

**Files:**

- Deployment state only.
- Do not modify tracked source during the maintenance window.

**Step 1: Freeze one release tuple before stopping traffic**

Record the exact Connector, Agent, Bridge image digest/commit and the Proto
checksum. Build all three before the window. Verify every Agent will use the
same Edge PostgreSQL and Connector has every real Agent address in
`SAG_TUNNEL_ENDPOINTS`; a random load-balancer address is invalid.

**Step 2: Stop new ingress**

Remove the Edge instance from the external load balancer or enable the approved
maintenance response, then stop Zentinel as the local entry:

```bash
docker compose -f docker-compose.edge.yml -f docker-compose.release.edge.yml stop zentinel
```

Also block direct access to Bridge port 9000 for the window. Do not stop Bridge
workers yet; they must drain accepted Redis jobs.

**Step 3: Drain queues and in-flight work**

Repeat until every value is zero for at least one full scrape interval:

```bash
docker exec sag-redis redis-cli -n 2 XLEN sag:dataplane:queue
docker exec sag-redis redis-cli -n 2 XINFO GROUPS sag:dataplane:queue
curl -fsS http://127.0.0.1:9000/metrics | grep '^bridge_sync_inflight '
curl -fsS http://127.0.0.1:9104/metrics | grep '^agent_pending_waiters '
```

Expected: stream length `0`, consumer-group `pending` `0`,
`bridge_sync_inflight 0`, and `agent_pending_waiters 0`. If they do not drain,
inspect deadline/cancel/DLQ metrics; do not delete Redis data or steal an
idempotency claim to force progress.

**Step 4: Stop every protocol participant**

Stop Bridge after drain, stop Connector on every Intra host, then stop every
Agent. Confirm no old process remains:

```bash
# Edge
docker compose -f docker-compose.edge.yml -f docker-compose.release.edge.yml stop http-tunnel-bridge stealth-tunnel-agent

# Intra
docker compose -f docker-compose.intra.yml -f docker-compose.release.intra.yml stop sag-connector
```

From this point until Step 9, no data-plane traffic may be admitted.

**Step 5: Back up and recreate the PostgreSQL container without deleting data**

Take the standard database backup. Recreate only the container so the named
volume remains intact; never use `down -v`:

```bash
docker compose -f docker-compose.edge.yml -f docker-compose.release.edge.yml up -d --no-deps --force-recreate postgres
docker compose -f docker-compose.edge.yml -f docker-compose.release.edge.yml exec -T postgres pg_isready -U postgres -d sag
```

Expected: PostgreSQL is healthy and the rendered host binding is loopback-only.

**Step 6: Apply and verify the idempotent schema**

An existing named volume does not replay image init scripts. Apply the checked-in
schema explicitly:

```bash
docker compose -f docker-compose.edge.yml -f docker-compose.release.edge.yml exec -T postgres psql -v ON_ERROR_STOP=1 -U postgres -d sag < infra/migrations/postgres/001_init.sql
docker compose -f docker-compose.edge.yml -f docker-compose.release.edge.yml exec -T postgres psql -U postgres -d sag -c "\d+ idempotency_records"
```

Expected: table, primary key, state constraint, owner attempt, response fields,
timestamps, expiry, and expiry index exist. Never truncate `pending` rows.

**Step 7: Deploy and start the coherent release**

Replace all binaries/configurations while stopped. Start every Agent first,
then Connector, then Bridge:

```bash
# Edge
docker compose -f docker-compose.edge.yml -f docker-compose.release.edge.yml up -d --no-deps --force-recreate stealth-tunnel-agent

# Intra
docker compose -f docker-compose.intra.yml -f docker-compose.release.intra.yml up -d --no-deps --force-recreate sag-connector

# Edge, after all explicit tunnels are healthy
docker compose -f docker-compose.edge.yml -f docker-compose.release.edge.yml up -d --no-deps --force-recreate http-tunnel-bridge
```

No old Connector, Agent, or Bridge instance may remain serving alongside the new
generation.

**Step 8: Verify before reopening ingress**

Check:

- one `connector_tunnel_up{agent_endpoint=...} 1` for every explicit Agent;
- `agent_pending_waiters 0` at idle;
- no PostgreSQL/SQLite dependency in Connector env, logs, or dependency tree;
- PostgreSQL is unreachable from the Intra host on Edge VPN/LAN port 5432;
- a read succeeds;
- first keyed mutation executes once;
- repeat replays with `x-sag-idempotency-state: replayed`;
- same key/different payload returns conflict;
- deadline/cancel probe leaves queue/pending/permits at zero;
- all required metric families scrape successfully.

Run the existing timeout chain verification against the running containers:

```powershell
pwsh -NoProfile -File scripts/ops/verify-timeout-chain.ps1
```

Expected: Connector < Agent < Bridge <= gRPC and no Bridge/APISIX retry.

**Step 9: Resume traffic and observe**

Start Zentinel/re-enable the load balancer only after Step 8 passes:

```bash
docker compose -f docker-compose.edge.yml -f docker-compose.release.edge.yml up -d --no-deps zentinel
```

Observe one normal operating window. Alert on late responses, failed cancels,
deadline stages, idempotency pending/conflict/store failures, queue expiry, and
tunnel drops.

**Step 10: Roll back as one version set if verification fails**

Stop ingress, drain again, stop all three participants, deploy the previously
verified Connector/Agent/Bridge tuple, and start Agent -> Connector -> Bridge.
The additive table remains. Do not delete the PostgreSQL volume, delete pending
claims, restore Connector database variables, expose port 5432 cross-host, or
live-downgrade a single component.
