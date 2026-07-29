# Request Deadline, Cancellation, and Idempotency Implementation Plan

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** Make the Bridge -> Agent -> Connector -> APISIX path deadline-bounded, cancellation-safe, observable, and safe from gateway-generated duplicate write effects.

**Architecture:** One absolute deadline and one logical request ID cross every hop; each transport attempt has a separate attempt ID. Agent uses RAII pending registrations and an Edge-side durable idempotency ledger, while the database-independent Connector supports explicit cancellation and deadline-aware dispatch.

**Tech Stack:** Rust 2021, Tokio, tonic/prost, reqwest, SQLite/PostgreSQL through `shared_storage`, Prometheus metrics, Docker Compose.

---

### Task 1: Lock protocol semantics with tests

**Files:**

- Modify: `shared/tunnel-proto/proto/tunnel.proto`
- Modify: `proxy/agents/stealth-tunnel-agent/src/connector_registry.rs`
- Modify: `proxy/connectors/sag-connector/src/main.rs`

**Steps:**

1. Add protocol fields for attempt, absolute deadline, idempotency key, and cancellation.
2. Add tests proving duplicate pending attempts are rejected and old guards cannot remove newer entries.
3. Add tests for deadline expiry and mutating-method classification.
4. Run `cargo test` for the three tunnel packages; expect the new tests to fail before implementation.

### Task 2: Replace pending counter with a cancellation-safe RAII registration

**Files:**

- Modify: `proxy/agents/stealth-tunnel-agent/src/connector_registry.rs`
- Modify: `proxy/agents/stealth-tunnel-agent/src/grpc_server.rs`
- Modify: `proxy/agents/stealth-tunnel-agent/src/main.rs`

**Steps:**

1. Store pending entries by `attempt_id`, with a unique registration generation and connector endpoint.
2. Return a guard that owns the oneshot receiver and semaphore permit.
3. On Drop, remove only the matching generation, update the pending gauge, and best-effort `try_send` a cancel message.
4. Make Connector stream unregister generation-aware and fail its matching pending requests immediately.
5. Add late-response and cancel metrics.
6. Run Agent unit tests and format.

### Task 3: Propagate a single absolute deadline and remove unsafe Bridge retry

**Files:**

- Modify: `proxy/http-tunnel-bridge/src/main.rs`
- Modify: `proxy/http-tunnel-bridge/src/queue.rs`
- Modify: deployment Compose files containing timeout overrides.

**Steps:**

1. Create request deadline before reading the HTTP body and bound body collection by it.
2. Inject a generated trace ID into forwarded headers when the caller omitted one.
3. Generate a logical request ID and a distinct attempt ID.
4. Execute exactly one tonic Forward attempt and reconnect the channel only for subsequent requests.
5. Set the tonic request timeout from remaining budget.
6. Reject expired Redis jobs before forwarding.
7. Normalize defaults to Connector 55s, Agent 58s fallback, Bridge 60s, gRPC 120s, Zentinel 90s.

### Task 4: Add Connector cancellation and deadline-aware HTTP

**Files:**

- Modify: `proxy/connectors/sag-connector/src/main.rs`

**Steps:**

1. Register a cancellation handle as soon as a ForwardRequest enters the bounded accept queue.
2. Consume CancelRequest messages in the tunnel loop.
3. Reject requests whose absolute deadline expired before enqueue or dispatch.
4. Race the reqwest future against cancellation and use remaining deadline as per-request timeout.
5. Remove handles on every completion path.
6. Convert response-body read errors into classified 502/504 responses.
7. Add structured request/attempt/trace logging and metrics.

### Task 5: Add durable idempotency claims and result replay

**Files:**

- Create: `shared/storage/src/idempotency.rs`
- Modify: `shared/storage/src/lib.rs`
- Modify: `shared/storage/src/sqlite_store.rs`
- Modify: `shared/storage/src/store.rs`
- Modify: `infra/migrations/postgres/001_init.sql`
- Modify: `proxy/agents/stealth-tunnel-agent/src/grpc_server.rs`

**Steps:**

1. Add SQLite/PostgreSQL schema for claim state, payload fingerprint, response, timestamps, and expiry.
2. Implement atomic claim returning `Claimed`, `Pending`, `Completed`, or `Conflict`.
3. Implement completion and completed-result reads.
4. Require `Idempotency-Key` for mutating methods at Bridge and copy it into the tunnel request and downstream header.
5. Agent claims before dispatch, waits/replays an existing completed claim, blocks unresolved pending claims, and stores the response before returning.
6. Add storage and Agent tests for claim races, replay, and key/payload conflict.

### Task 6: Remove cancellation-unsafe policy coalescing

**Files:**

- Modify: `proxy/agents/stealth-tunnel-agent/src/grpc_server.rs`
- Modify: `proxy/agents/stealth-tunnel-agent/src/main.rs`

**Steps:**

1. Delete the coalescing sender map and follower loop.
2. Retain policy cache and semaphore.
3. Bound the whole authorization phase by the request's remaining deadline.
4. Add a test showing cancellation cannot leave future calls waiting on an abandoned leader.

### Task 7: Observability and operational validation

**Files:**

- Modify: `scripts/ops/verify-timeout-chain.ps1`
- Modify: `scripts/ops/verify-timeout-chain.sh`
- Modify: `docs/ops/timeout-deadline-runbook.md`
- Modify: relevant Compose/env examples.

**Steps:**

1. Expose gauges/counters for pending, late response, cancellation, expiry, body-read error, and idempotency outcomes.
2. Make every timeout log include request ID, attempt ID, trace ID, stage, and remaining milliseconds.
3. Update timeout verification to reject inverted ladders and account for zero retries.
4. Run `cargo fmt --all`, targeted tests, workspace tests/check, timeout-chain scripts, and Compose rendering.
5. Record any environment-only verification blocker without weakening tests or implementation.

