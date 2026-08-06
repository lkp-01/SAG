# Connector Health and Configuration Convergence Implementation Plan

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

> **For Codex:** Use the available `executing-plans` skill when resuming this
> plan. Preserve the pre-send/post-send retry boundary in the accepted design.

**Goal:** Make Connector availability reflect the real request path and make control-plane route changes converge durably to every Agent and APISIX.

**Architecture:** Connector sessions use one generation-aware health/revoke path and a dispatcher-level synthetic probe. Route mutations use a transactional generation plus outbox; Agents atomically apply consistent snapshots and ACK their generation, while an APISIX worker and actual-state reconciler repair drift.

**Tech Stack:** Rust 2021, Tokio, tonic/prost, Axum, reqwest, SQLite/rusqlite, PostgreSQL/tokio-postgres, APISIX Admin API, Prometheus, Docker Compose.

---

## Implementation status (2026-08-04)

- Tasks 0-8 are implemented in the current worktree, including generation-aware
  Connector revocation, dispatcher-path probes, atomic desired-state/outbox
  writes, Agent apply ACKs, APISIX tombstones, and bounded actual-state
  reconciliation.
- Task 9's configuration, metrics, alerts, documentation, and Compose wiring are
  implemented. Production fault/load evidence and environment-specific rollout
  remain release gates; probe timings stay configurable for that reason.
- PostgreSQL multi-worker claims, concurrent configuration writes,
  reconcile-vs-mutation ordering, the current-resource partial unique index,
  and migration backfill were verified against an isolated PostgreSQL 16
  instance. A delayed server-side APISIX write test proves that an unknown
  outcome retains the app lease and fences a newer generation during the
  configured quarantine window. This is bounded risk mitigation, not an
  APISIX-side generation fence: the Admin API exposes no CAS, and periodic
  reconciliation repairs a write that lands after quarantine expiry.
- Full Rust tests, strict Clippy, all three frontend typecheck/lint/build flows,
  Compose rendering, and all 18 Prometheus rules pass. Existing frontend lint
  warnings remain warnings and are outside this backend convergence change.

Remaining production gates are a live APISIX drift/fault run, proof that the
configured unknown-outcome isolation exceeds the APISIX server-side execution
deadline (or adoption of a server-side CAS/fence), Redis queue and
multi-instance auth integration tests, and load-based tuning of the default
probe/heartbeat thresholds.

## Invariants and scope

- Keep the existing Agent-local generation and wire `stream_epoch`; neither is
  replaced by configuration generation.
- Retry another Connector only when channel enqueue definitively failed.
- Never replay `Sent`, `Accepted`, or response-timeout attempts across streams.
- APISIX ownership remains per app. Safe IDs retain `sag-route-{app_id}`;
  unsafe/overlong IDs use a bounded hash ID with fingerprint-guarded legacy
  migration. `api_routes` remains metadata for this change.
- All schema changes support both SQLite and PostgreSQL.
- Every Agent and every configured control-plane sync endpoint must share the
  same durable configuration store; independent per-endpoint databases are an
  unsupported topology because an ACK must be recoverable by that Agent.
- The current Windows shell lacks MSVC `link.exe`; use the repository GNU target
  setup, WSL/Linux, or CI for executable tests. Formatting remains runnable.

### Task 0: Record the baseline and align decision documents

**Files:**

- Create: `docs/plans/2026-08-04-connector-health-config-convergence-design.md`
- Create: `docs/plans/2026-08-04-connector-health-config-convergence.md`
- Modify: `docs/adr/0002-stream-epoch-and-request-outcome.md`

**Step 1: Record the audited implemented/missing matrix**

Document existing lease/epoch behavior and the exact remaining gaps.

**Step 2: Resolve the ADR status mismatch**

Change ADR 0002 from `Proposed` to `Accepted and implemented`, because the
protocol and tests are already present in source. Do not claim production fault
evidence that has not run.

**Step 3: Verify documentation references**

Run:

```powershell
rg -n "stream_epoch|config_sync_jobs|X-SAG-Config-Generation|pre-enqueue" docs/plans docs/adr
```

Expected: both new plans and ADR 0002 express the same retry boundary.

### Task 1: Unify Connector eligibility and revocation

**Files:**

- Modify: `proxy/agents/stealth-tunnel-agent/src/connector_registry.rs`
- Modify: `proxy/agents/stealth-tunnel-agent/src/main.rs`

**Step 1: Write failing registry tests**

Add tests proving:

```rust
assert!(!registry.is_tunnel_healthy_with_window(endpoint, window));
// when the mpsc receiver is closed, even with a fresh heartbeat
```

and proving a failed first sender is revoked and a second session receives the
same not-yet-enqueued attempt.

**Step 2: Run the focused test**

```powershell
cargo test -p stealth-tunnel-agent connector_registry::tests
```

Expected before implementation: the new assertions fail.

**Step 3: Implement one eligibility predicate**

Use `heartbeat fresh && !tx.is_closed() && tx.capacity() > 0 && !revoked` for
health, selection, and readiness. Add `healthy_session_count(window)`.

**Step 4: Implement generation-aware revoke and pre-enqueue failover**

`revoke_session(endpoint, generation, reason)` must remove only the matching
generation, send its close signal, and fail its pending requests. Request send
uses `try_send`; `Full`/`Closed` revokes that session and tries another eligible
session. Insert the pending entry before `try_send`, remove it on definite send
failure, and construct the `PendingRequest` only after successful enqueue.

**Step 5: Make `/ready` use the same predicate**

Pass the configured lease window into health state and require the minimum
number of healthy, send-capable sessions rather than raw registrations.

**Step 6: Run tests and format**

```powershell
cargo fmt --all -- --check
cargo test -p stealth-tunnel-agent connector_registry::tests
```

Expected: formatting and focused tests pass in a working linker environment.

### Task 2: Guarantee Connector cleanup and HTTP 503 semantics

**Files:**

- Modify: `proxy/connectors/sag-connector/src/main.rs`
- Modify: `proxy/agents/stealth-tunnel-agent/src/grpc_server.rs`
- Modify: `proxy/http-tunnel-bridge/src/main.rs`

**Step 1: Write cleanup and mapping tests**

Test protocol/read error terminal classification, response-timeout generation
revocation, and `tonic::Code::Unavailable -> HTTP 503` when no response was
received.

**Step 2: Refactor Connector loop to one cleanup epilogue**

After heartbeat/dispatcher creation, no `?` or `bail!` may bypass cleanup.
Capture a terminal result, cancel all active work, close the accept queue, bound
dispatcher drain, abort heartbeat, set tunnel gauge down, then return it.

**Step 3: Propagate one-way stream send failures**

Use a fatal notification channel so heartbeat or dispatcher response-send
failure wakes and terminates the main tunnel loop immediately.

**Step 4: Revoke timed-out Agent sessions**

Acceptance/response timeout revokes only the pending request's bound generation.
It does not automatically resend that request.

**Step 5: Map availability failures to 503**

Preserve existing outcome-unknown response metadata and map ordinary Connector
`Unavailable` to HTTP 503 rather than 502.

**Step 6: Verify**

```powershell
cargo test -p sag-connector -p stealth-tunnel-agent -p http-tunnel-bridge
```

Expected: all focused lifecycle and mapping tests pass.

### Task 3: Add dispatcher-path active probes

**Files:**

- Modify: `shared/tunnel-proto/proto/tunnel.proto`
- Modify: `shared/tunnel-proto/src/lib.rs`
- Modify: `proxy/agents/stealth-tunnel-agent/src/connector_registry.rs`
- Modify: `proxy/agents/stealth-tunnel-agent/src/main.rs`
- Modify: `proxy/connectors/sag-connector/src/main.rs`
- Modify: `.env.example`
- Modify: `docker-compose.yml`
- Modify: `docker-compose.edge.yml`

**Step 1: Lock the additive wire field**

Add dedicated `HealthProbe` / `HealthProbeAck` payloads plus the
`health-probe-v1` registration capability and protobuf round-trip tests. Do not
renumber any existing field.

**Step 2: Add failing probe tests**

Cover success, timeout revocation, old-epoch response rejection, queue
saturation, and one replica failing without removing another.

**Step 3: Send probes to exact generations**

Agent emits unique probe IDs directly to each eligible session, tracks RTT
without high-cardinality labels, and requires the matching ACK before the
configured deadline.

**Step 4: Execute probes in the normal Connector dispatcher**

The dedicated frame consumes the normal accept/dispatch path. Connector returns
an ACK locally instead of contacting APISIX.

**Step 5: Add conservative configuration**

Introduce interval, timeout, consecutive-failure threshold, and startup grace.
Defaults remain opt-in until fault/load tests establish safe production values.

**Step 6: Verify coordinated protocol packages**

```powershell
cargo test -p sag-tunnel-proto -p stealth-tunnel-agent -p sag-connector
```

Expected: protocol, fencing, and probe tests pass.

### Task 4: Add durable configuration convergence schema

**Files:**

- Create: `infra/migrations/postgres/005_config_convergence.sql`
- Create: `shared/storage/src/config_sync.rs`
- Create: `shared/storage/tests/config_convergence.rs`
- Modify: `shared/storage/src/lib.rs`
- Modify: `shared/storage/src/sqlite_store.rs`
- Modify: `shared/storage/src/store.rs`

**Step 1: Write failing SQLite storage tests**

Cover initial generation 0, monotonic ACK, job persistence/claim/failure/retry,
and consistent route snapshot reads.

**Step 2: Add the three tables and indexes**

Create `config_state`, `agent_config_applies`, and `config_sync_jobs` with the
columns and constraints from the accepted design. Include migration 005 from
`ensure_store_schema`; mirror it in SQLite's embedded schema.

**Step 3: Implement typed storage APIs**

Provide:

```rust
ConfigSyncStore::current_generation(...)
ConfigSyncStore::load_route_snapshot(...)
ConfigSyncStore::claim_due_jobs(...)
ConfigSyncStore::mark_applied(...)
ConfigSyncStore::mark_failed(...)
ConfigSyncStore::ack_agent_generation(...)
```

Use a SQLite transaction and PostgreSQL repeatable-read transaction for the
snapshot. ACK upsert must include a monotonic `WHERE` guard.

**Step 4: Verify persistence APIs**

```powershell
cargo test -p shared_storage --test config_convergence
```

Expected: both backend-independent tests and optional PostgreSQL tests pass.

### Task 5: Commit route change, generation, audit, and outbox atomically

**Files:**

- Modify: `shared/storage/src/audit_logs.rs`
- Modify: `shared/storage/tests/config_convergence.rs`
- Modify: `services/control-plane-admin/src/main.rs`

**Step 1: Write rollback and tombstone tests**

Use a duplicate audit ID to force rollback and assert route, generation, and job
all remain unchanged. Add cases for deleting the last route, deleting one of
multiple app routes, and moving a host between apps.

**Step 2: Extend the existing security mutation transaction**

For tunnel-route and intranet-upstream mutations:

1. read the previous app ownership when needed;
2. apply the domain mutation;
3. increment generation once;
4. supersede older non-terminal work for each affected app;
5. insert UPSERT or DELETE work;
6. insert the audit row;
7. commit once.

**Step 3: Remove best-effort APISIX calls from handlers**

Handlers only commit desired state/outbox, invalidate the compatible cache, and
return. They must not lose the sync intent when APISIX is down.

**Step 4: Verify atomicity**

```powershell
cargo test -p shared_storage config_convergence
cargo test -p control-plane-admin
```

Expected: rollback/tombstone and handler compilation tests pass.

### Task 6: Process durable APISIX UPSERT and DELETE jobs

**Files:**

- Modify: `services/control-plane-admin/src/apisix.rs`
- Modify: `services/control-plane-admin/src/main.rs`
- Modify: `services/control-plane-admin/Cargo.toml`

**Step 1: Extract deterministic route construction**

Make `route_id_for_app` and `build_app_route` pure and unit-tested.

**Step 2: Add idempotent deletion**

DELETE `/apisix/admin/routes/{id}`; treat success and 404 as applied.

**Step 3: Add the retry worker**

Lease due jobs, apply the requested operation, persist APPLIED or FAILED, and
use capped exponential backoff. Prevent stale work from applying after newer
work for the same resource.

**Step 4: Start the worker after schema readiness**

Use a short configurable poll interval and batch limit. Initialize job gauges
even when no job has run.

**Step 5: Test with a mock APISIX server**

Cover PUT success, DELETE 404, 500 then recovery, timeout, and restart with a
persisted due job.

### Task 7: Return versioned snapshots and ACK Agent application

**Files:**

- Modify: `services/control-plane-admin/src/main.rs`
- Modify: `proxy/agents/stealth-tunnel-agent/src/manager.rs`
- Modify: `proxy/agents/stealth-tunnel-agent/src/main.rs`
- Modify: `.env.example`
- Modify: `docker-compose.yml`
- Modify: `docker-compose.edge.yml`

**Step 1: Add generation-aware manager tests**

Prove map replacement occurs before generation publication, an older snapshot
cannot roll back a newer map (including after process restart), and ACK
URL/body construction is stable.

**Step 2: Return a consistent version header**

Load rows and generation from one storage snapshot. Return the existing JSON
array plus `X-SAG-Config-Generation`. Cache values store both routes and their
generation so a race never pairs old rows with a new version.

**Step 3: Stage, durably ACK, then publish**

Agent reads the header, validates/builds a new map, and stages it without making
it resolvable. It POSTs
`agent_id + applied_generation + applied_at_ms + snapshot_hash` to `/ack`, then
atomically publishes only after a durable ACK succeeds. Reuse
`SAG_AGENT_INSTANCE_ID`; ACK failures remain fail-closed and retry without
exposing an uncommitted generation. Restore the persisted generation/hash as a
fail-closed floor at startup and require the same fingerprint or a newer
generation before ready. While a newer generation is staged, previously
published routes are intentionally hidden until its ACK commits; this favors
configuration safety over availability during an ACK outage.

**Step 4: Add the authenticated ACK endpoint**

Reject generations ahead of desired state. Monotonically upsert the Agent row;
duplicate/older ACKs are idempotent and cannot decrease the stored generation,
while a same-generation fingerprint conflict is rejected.

**Step 5: Verify**

```powershell
cargo test -p shared_storage -p control-plane-admin -p stealth-tunnel-agent
```

Expected: snapshot/ACK ordering and monotonicity tests pass.

### Task 8: Replace blind PUT with actual APISIX reconciliation

**Files:**

- Modify: `services/control-plane-admin/src/apisix.rs`
- Modify: `services/control-plane-admin/src/main.rs`

**Step 1: Add Admin API response fixtures and diff tests**

Cover missing, expected-subset mismatch, extra `sag-route-*`, and unrelated
unmanaged routes.

**Step 2: Build the expected managed map**

Join tunnel-route app IDs with intranet upstreams and produce canonical JSON.

**Step 3: Fetch and normalize actual managed routes**

Accept APISIX 3.10 list response shapes, compare only the expected owned subset,
and never select routes outside the managed ID/prefix contract.

**Step 4: Enqueue repairs**

UPSERT missing/different routes and DELETE extra managed routes using the current
desired generation. Reconciliation itself does not increment generation.

**Step 5: Verify**

Run unit tests and one Docker APISIX drift scenario. Expected: drift returns to
zero and unmanaged routes remain byte-for-byte unchanged.

### Task 9: Metrics, alerts, operations, and release gates

**Files:**

- Modify: `infra/observability/alerts/production-hardening.yml`
- Modify: `README.md`
- Modify: `DUAL_HOST_OPERATIONS.md`
- Create: `scripts/ops/test-connector-health-convergence.ps1`
- Create: `scripts/ops/test-config-convergence.ps1`

**Step 1: Add low-cardinality metrics**

Expose healthy sessions, revoke reasons, probe RTT/failures, desired/applied
generation, ACK age, job state/oldest age, reconcile drift, and last success.
Never label metrics by request, attempt, probe, epoch, or arbitrary host.

**Step 2: Add alerts**

Alert on Agent generation lag/ACK staleness, failed/old pending jobs, persistent
APISIX drift, and repeated probe/session revocation.

**Step 3: Run the fault matrix**

Test closed stream, read error, blocked dispatcher, stale response, APISIX 500,
control-plane restart, last-route deletion, ghost injection, and unmanaged route
preservation. Save evidence under `artifacts/` without deleting volumes.

**Step 4: Run static and package verification**

```powershell
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace --all-targets
powershell -NoProfile -ExecutionPolicy Bypass -File scripts/verify-project.ps1
docker compose -f docker-compose.yml config --quiet
docker compose -f docker-compose.edge.yml config --quiet
docker compose -f docker-compose.intra.yml config --quiet
```

Expected: all commands pass in a GNU/MSVC/CI environment with a linker.

## Definition of done

- Health, readiness, and allocation share one session predicate.
- All tunnel exits execute cleanup; affected waiters finish promptly.
- Dispatcher probes detect the heartbeat-alive/request-stalled failure mode.
- Availability failures return 503 and post-send attempts never auto-migrate.
- Desired state, audit, generation, and APISIX jobs are one atomic commit.
- Agents ACK only fully applied, monotonic snapshots.
- Persistent jobs and tombstones converge APISIX after failure/restart.
- Actual-state diff repairs missing/changed/extra managed routes and preserves
  unmanaged configuration.
- Focused tests, full workspace checks, Compose renders, and fault/load evidence
  pass before enabling aggressive probe/timeouts in production.
