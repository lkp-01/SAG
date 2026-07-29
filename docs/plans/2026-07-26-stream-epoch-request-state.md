# Stream Epoch and Request Outcome Implementation Plan

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

> **For Codex:** Use the available `executing-plans` skill. Stop at every task
> checkpoint when the observed source differs from the baseline in this plan;
> do not overwrite concurrent workspace work.

**Goal:** Add an explicit wire-level Stream Epoch and request acceptance state so tunnel loss fails affected requests promptly and safely without migrating, replaying, or automatically retrying old work.

**Architecture:** Connector generates a UUID epoch for every stream run; Agent retains its existing local generation as the authoritative fence and validates the wire epoch as a second boundary. RegisterAck makes session readiness explicit, ForwardAccepted advances an Agent-side monotonic request phase, and stream loss sends an explicit outcome-unknown failure while the existing durable idempotency claim remains pending for any possibly dispatched mutation.

**Tech Stack:** Rust 2021, Tokio, tonic/prost, reqwest, UUID v4, SQLite/PostgreSQL through `shared_storage`, Prometheus metrics, Docker Compose, PowerShell/Bash fault verification.

---

## Approval gates

Implementation must not start until the reviewer accepts all six decisions:

1. **Fail fast, do not resume:** a new stream never inherits old attempts.
2. **No automatic Bridge retry:** reconnect only prepares later logical requests.
3. **Two fences:** Agent-local generation remains authoritative; the wire
   `stream_epoch` is also required and validated.
4. **Ack is diagnostic, not proof of non-delivery:** missing ForwardAccepted
   never authorizes redispatch of a mutation.
5. **Reuse the existing ledger:** no second idempotency store and no Connector
   database dependency.
6. **Coordinated release:** mixed live Connector/Agent/Bridge protocol semantics
   remain unsupported.

## Baseline preservation rules

The 2026-07-26 source already contains behavior that this plan must preserve:

- `request_id` and `attempt_id` are separate.
- one absolute `deadline_unix_ms` is consumed across queues and network calls.
- Bridge executes one tonic Forward attempt.
- Agent pending entries are attempt-keyed, semaphore-owned, RAII-cleaned, and
  bound to an Agent-local Connector generation.
- Agent supports multiple healthy Connector sessions per logical endpoint,
  round-robin selection, heartbeat lease expiry, mTLS certificate binding, and
  wrong-generation response rejection.
- Connector owns only ephemeral accept queues, cancellation state, and HTTP
  futures; it has no `shared_storage` dependency.
- Connector cancels active attempts when a stream ends.
- Agent owns the durable mutation claim and completed-response replay.

Do not execute a step that removes or weakens any item above.

The current desktop shell cannot compile Rust because MSVC `link.exe` is not
installed. The observed baseline failure is linker discovery, not a failing
test. Implementation must begin in a Visual Studio Developer Shell, WSL/Linux,
or CI environment where the unchanged focused test command can run.

This workspace may have no valid Git metadata. Execute Commit steps only when
`git rev-parse --show-toplevel` succeeds. Otherwise record the checkpoint and
continue without creating fake history.

### Task 0: Freeze and verify the moving baseline

**Files:**

- Reference: `docs/plans/2026-07-26-stream-epoch-request-state-design.md`
- Reference: `docs/adr/0002-stream-epoch-and-request-outcome.md`
- Reference: `shared/tunnel-proto/proto/tunnel.proto`
- Reference: `proxy/agents/stealth-tunnel-agent/src/connector_registry.rs`
- Reference: `proxy/agents/stealth-tunnel-agent/src/grpc_server.rs`
- Reference: `proxy/connectors/sag-connector/src/main.rs`
- Reference: `proxy/http-tunnel-bridge/src/main.rs`
- Reference: `shared/storage/src/idempotency.rs`

**Step 1: Confirm the approved design status**

After user approval, change `Proposed` to `Accepted` in the design and ADR. Do
not change it before approval.

**Step 2: Assert that the preservation baseline is still present**

Run:

```powershell
rg -n "attempt_id|deadline_unix_ms|CancelRequest" shared/tunnel-proto/proto/tunnel.proto
rg -n "Vec<OutboundEntry>|outbound_generation|expire_stale|wrong_session|PendingRequest" proxy/agents/stealth-tunnel-agent/src/connector_registry.rs
rg -n "peer_certs|authorize_connector_certificate|mark_dispatched|IdempotencyStore" proxy/agents/stealth-tunnel-agent/src/grpc_server.rs
rg -n "CancelState|SAG_TUNNEL_ENDPOINTS|for state in cancellations|dispatch.await" proxy/connectors/sag-connector/src/main.rs
rg -n "one tonic Forward|client.forward|for attempt in 0\.\.2" proxy/http-tunnel-bridge/src/main.rs
```

Expected:

- all positive invariants match;
- the last search has `client.forward` but no `for attempt in 0..2`;
- Connector still has no `shared_storage`, PostgreSQL, or SQLite reference.

If the structure differs materially, stop and revise this plan before editing.

**Step 3: Run the unchanged focused baseline**

```powershell
cargo test -p sag-tunnel-proto -p stealth-tunnel-agent -p sag-connector -p http-tunnel-bridge -p shared_storage
```

Expected: PASS in a working Rust build environment. If it fails because
`link.exe` is absent, move to a Developer Shell/WSL/CI; do not alter code or
dependencies to conceal the environment problem.

**Step 4: Record a source checkpoint**

```powershell
Get-FileHash shared/tunnel-proto/proto/tunnel.proto,
  proxy/agents/stealth-tunnel-agent/src/connector_registry.rs,
  proxy/agents/stealth-tunnel-agent/src/grpc_server.rs,
  proxy/connectors/sag-connector/src/main.rs,
  proxy/http-tunnel-bridge/src/main.rs,
  shared/storage/src/idempotency.rs
```

Save the hashes in the execution log. They are drift detection, not a release
identity.

**Step 5: Commit the accepted decision if Git is available**

```powershell
git add docs/plans/2026-07-26-stream-epoch-request-state-design.md docs/adr/0002-stream-epoch-and-request-outcome.md
git commit -m "docs: accept tunnel epoch and request outcome design"
```

### Task 1: Lock the wire contract with failing tests

**Files:**

- Modify: `shared/tunnel-proto/proto/tunnel.proto`
- Modify: `shared/tunnel-proto/src/lib.rs`
- Create: `scripts/ops/verify-stream-epoch-contract.ps1`
- Create: `scripts/ops/verify-stream-epoch-contract.sh`

**Step 1: Write failing protobuf round-trip tests**

Add tests to `shared/tunnel-proto/src/lib.rs` that construct, encode, decode,
and compare both new server/client messages:

```rust
#[cfg(test)]
mod tests {
    use prost::Message;
    use super::{tunnel_message, ConnectorRegisterAck, ForwardAccepted, TunnelMessage};

    #[test]
    fn register_ack_round_trips_stream_epoch() {
        let original = TunnelMessage {
            payload: Some(tunnel_message::Payload::RegisterAck(ConnectorRegisterAck {
                connector_id: "connector-1".into(),
                endpoint: "connector-1:stream".into(),
                stream_epoch: "11111111-1111-4111-8111-111111111111".into(),
            })),
        };
        let decoded = TunnelMessage::decode(original.encode_to_vec().as_slice()).unwrap();
        assert_eq!(decoded, original);
    }

    #[test]
    fn accepted_round_trips_attempt_and_epoch() {
        let original = TunnelMessage {
            payload: Some(tunnel_message::Payload::Accepted(ForwardAccepted {
                request_id: "logical".into(),
                attempt_id: "attempt".into(),
                stream_epoch: "22222222-2222-4222-8222-222222222222".into(),
            })),
        };
        let decoded = TunnelMessage::decode(original.encode_to_vec().as_slice()).unwrap();
        assert_eq!(decoded, original);
    }
}
```

**Step 2: Run the tests and verify they fail**

```powershell
cargo test -p sag-tunnel-proto
```

Expected: FAIL because `ConnectorRegisterAck`, `ForwardAccepted`, and the new
oneof variants do not exist.

**Step 3: Extend the protocol additively**

Keep all existing tag numbers. Add:

```proto
message TunnelMessage {
  oneof payload {
    ConnectorRegister register = 1;
    ConnectorHeartbeat heartbeat = 2;
    ForwardRequest request = 3;
    ForwardResponse response = 4;
    CancelRequest cancel = 5;
    ConnectorRegisterAck register_ack = 6;
    ForwardAccepted accepted = 7;
  }
}
```

Add `ConnectorRegister.stream_epoch = 5`,
`ConnectorHeartbeat.stream_epoch = 4`, `ForwardRequest.stream_epoch = 10`,
`ForwardResponse.stream_epoch = 6`, and `CancelRequest.stream_epoch = 4`, plus
the two message definitions from the approved design. Do not renumber or reuse
tags 1 through 5.

**Step 4: Add static contract guards**

Both scripts must fail unless all exact field/tag declarations are present and
must also fail if `for attempt in 0..2` reappears in Bridge. The PowerShell
script should use `Select-String -SimpleMatch`; the shell script should use
`rg -F`. End with `stream epoch contract: PASS`.

**Step 5: Run protocol tests and guards**

```powershell
cargo test -p sag-tunnel-proto
pwsh -NoProfile -File scripts/ops/verify-stream-epoch-contract.ps1
```

Expected: both round-trip tests pass and the guard prints PASS.

**Step 6: Commit**

```powershell
git add shared/tunnel-proto/proto/tunnel.proto shared/tunnel-proto/src/lib.rs scripts/ops/verify-stream-epoch-contract.ps1 scripts/ops/verify-stream-epoch-contract.sh
git commit -m "feat: define tunnel stream epoch protocol"
```

### Task 2: Add an explicit Agent pending state machine

**Files:**

- Modify: `proxy/agents/stealth-tunnel-agent/src/connector_registry.rs`

**Step 1: Write failing state and fencing tests**

Add focused Tokio tests with these exact behavioral names:

- `request_phase_advances_monotonically_to_accepted`
- `lost_session_returns_stream_lost_with_last_phase`
- `ack_from_wrong_generation_does_not_advance_attempt`
- `ack_with_wrong_epoch_does_not_advance_attempt`
- `response_with_wrong_epoch_does_not_complete_attempt`
- `one_lost_session_does_not_fail_other_replica_attempts`
- `stale_unregister_cannot_remove_new_epoch`

The stream-loss test must create at least three pending attempts on one session,
unregister it, and assert all three receive explicit `PendingFailure::StreamLost`
without waiting for a timeout.

**Step 2: Run tests and verify the new tests fail**

```powershell
cargo test -p stealth-tunnel-agent connector_registry
```

Expected: FAIL because phase/failure/epoch fields do not exist.

**Step 3: Introduce the state types**

Add:

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum PendingPhase {
    Queued,
    Sent,
    Accepted,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PendingFailure {
    StreamLost {
        phase: PendingPhase,
        stream_epoch: String,
    },
    ProtocolViolation {
        phase: PendingPhase,
        reason: String,
    },
}
```

Change the pending sender/receiver payload to
`Result<ForwardResponse, PendingFailure>`. Add `stream_epoch` to
`OutboundEntry`, `PendingEntry`, and `PendingRequest`; add `phase` to
`PendingEntry`.

**Step 4: Make transitions monotonic**

Implement private helpers that update an entry only when all of these match:

- `attempt_id`;
- pending registration generation;
- selected outbound local generation;
- selected wire epoch.

Use `entry.phase = entry.phase.max(next)` so a fast Accepted message cannot be
overwritten by a later local Sent transition.

`send_request_to_connector` must:

1. select a healthy session;
2. overwrite `req.stream_epoch` with that selected session's epoch;
3. insert phase Queued;
4. enqueue the request;
5. advance to Sent only after enqueue succeeds.

**Step 5: Send explicit failure on session loss**

Replace `pending.retain(...)` in `fail_pending_for_session` with a two-phase
drain: remove matching entries under the mutex, release the mutex, then send
`Err(PendingFailure::StreamLost { ... })` to each waiter. Match endpoint, local
generation, and epoch.

Do not decrement `pending_current` here; `PendingRequest::drop` remains the
single owner of the permit and gauge decrement.

**Step 6: Validate source generation and epoch for Ack/Response**

Add:

```rust
pub fn mark_accepted(
    &self,
    outbound_generation: u64,
    accepted: ForwardAccepted,
) -> bool
```

Update `resolve_response` so it requires the actual handler generation and the
matching payload epoch before removing the entry. Wrong-session or wrong-epoch
messages increment classified counters and leave the real waiter untouched.

**Step 7: Run Agent registry tests**

```powershell
cargo fmt --package stealth-tunnel-agent
cargo test -p stealth-tunnel-agent connector_registry
```

Expected: all old multi-session/lease/RAII tests and all new state tests pass.

**Step 8: Commit**

```powershell
git add proxy/agents/stealth-tunnel-agent/src/connector_registry.rs
git commit -m "feat: track tunnel request acceptance state"
```

### Task 3: Make Agent registration and failure semantics explicit

**Files:**

- Modify: `proxy/agents/stealth-tunnel-agent/src/grpc_server.rs`
- Modify: `proxy/agents/stealth-tunnel-agent/src/main.rs`

**Step 1: Write failing Agent tests**

Add tests for:

- invalid or empty epoch is rejected before registry publication;
- RegisterAck echoes connector ID, endpoint, and epoch;
- RegisterAck is queued before the session becomes selectable;
- heartbeat must match endpoint, connector ID, and epoch;
- ForwardAccepted before Register is a protocol error;
- ForwardAccepted from the wrong stream cannot advance a pending request;
- stream loss maps to gRPC Unavailable with `x-sag-outcome=unknown`;
- session expiry wakes affected requests but keeps another replica healthy.

**Step 2: Run the failing tests**

```powershell
cargo test -p stealth-tunnel-agent
```

Expected: the new epoch/Ack/status tests fail.

**Step 3: Extend the registered-session identity**

Add `stream_epoch: String` to `RegisteredConnectorSession`. Parse registration
epochs with `Uuid::parse_str`; reject empty/malformed values with
`InvalidArgument`. Continue mTLS endpoint binding before registry publication.

**Step 4: Queue RegisterAck before publishing the session**

Construct and `send().await` this message before calling `registry.register`:

```rust
TunnelMessage {
    payload: Some(tunnel_message::Payload::RegisterAck(ConnectorRegisterAck {
        connector_id: reg.connector_id.clone(),
        endpoint: reg.endpoint.clone(),
        stream_epoch: reg.stream_epoch.clone(),
    })),
}
```

Then register the session with the same epoch. If queuing the acknowledgement
fails, close the stream without registry publication. This ordering ensures
FIFO Ack-before-Request behavior.

**Step 5: Bind every inbound message to the actual session**

- Heartbeat: require endpoint, connector ID, and epoch equality.
- ForwardAccepted: require registration, then call
  `registry.mark_accepted(session.generation, accepted)`.
- ForwardResponse: call the epoch-validating `resolve_response` with the actual
  local generation.
- RegisterAck, Request, and Cancel received from Connector are invalid-direction
  protocol messages and close the stream.

**Step 6: Map explicit pending failures**

When `pending.recv()` returns `PendingFailure::StreamLost`, create a tonic
`Unavailable` status with metadata:

```rust
let mut status = Status::unavailable("connector stream lost; outcome unknown");
status.metadata_mut().insert(
    "x-sag-outcome",
    tonic::metadata::MetadataValue::from_static("unknown"),
);
```

Also attach validated ASCII values for attempt ID, last phase, and stream epoch.
Do not label a missing Ack as `not-dispatched`.

**Step 7: Preserve the durable claim boundary**

Keep `idempotency_dispatch_guard.mark_dispatched()` immediately after local
stream enqueue succeeds. Do not move it to ForwardAccepted: an Ack can be lost
after Connector accepted the mutation.

Add a comment and a regression test stating this invariant.

**Step 8: Run Agent tests**

```powershell
cargo fmt --package stealth-tunnel-agent
cargo test -p stealth-tunnel-agent
```

Expected: all authentication, session, pending, deadline, cancellation, and
idempotency tests pass.

**Step 9: Commit**

```powershell
git add proxy/agents/stealth-tunnel-agent/src/grpc_server.rs proxy/agents/stealth-tunnel-agent/src/main.rs
git commit -m "feat: fence connector sessions with wire epoch"
```

### Task 4: Add Connector handshake, attempt phases, and bounded drain

**Files:**

- Modify: `proxy/connectors/sag-connector/Cargo.toml`
- Modify: `proxy/connectors/sag-connector/src/main.rs`
- Modify: `Cargo.lock`

**Step 1: Write failing Connector tests**

Add tests proving:

- every new tunnel run creates a nonempty valid UUID epoch;
- tunnel-up is false until a matching RegisterAck is received;
- mismatched RegisterAck, Request, or Cancel epoch terminates the run;
- ForwardAccepted is emitted only after active-attempt reservation and queue
  insertion succeed;
- queue-full and expired-before-enqueue paths emit a terminal response but no
  Accepted message;
- attempt phase advances `Reserved -> Accepted -> Executing -> Completed`;
- stream loss makes cancellation sticky for queued and in-flight work;
- drain timeout aborts a deliberately stuck dispatcher within its configured
  budget;
- every response, error response, heartbeat, and cancel match includes the
  current epoch.

**Step 2: Run tests and verify failure**

```powershell
cargo test -p sag-connector
```

Expected: new handshake/epoch/phase tests fail.

**Step 3: Add UUID support and generate one epoch per run**

Add `uuid.workspace = true` to the Connector manifest. At the beginning of
`run_tunnel_once`, create:

```rust
let stream_epoch = uuid::Uuid::new_v4().to_string();
```

Include it in Register and all Heartbeats. A reconnect invokes
`run_tunnel_once` again and therefore creates a different epoch.

**Step 4: Require RegisterAck before readiness**

Set tunnel-up gauge only after receiving a RegisterAck whose connector ID,
endpoint, and epoch all match. Bound the handshake with
`SAG_CONNECTOR_REGISTER_ACK_TIMEOUT_MS`, default 5000 ms. A timeout or mismatch
ends the run and enters the existing reconnect backoff.

Do not accept ForwardRequest or CancelRequest before the matching Ack.

**Step 5: Validate and echo the epoch**

- Reject/bail on a Request or Cancel whose epoch differs from the current run.
- Set `ForwardResponse.stream_epoch` on every success and error constructor.
- Send `ForwardAccepted` after successful `job_tx.try_send` only.
- Include epoch in structured logs, never Prometheus labels.

**Step 6: Extend ephemeral attempt state**

Replace the cancellation-only value with an `AttemptState` containing the
existing sticky cancel signal plus an atomic monotonic phase. Keep the map keyed
by attempt ID and keep duplicate-attempt rejection.

The state transition locations are:

1. map reservation -> Reserved;
2. successful queue insertion -> Accepted;
3. immediately before APISIX future construction -> Executing;
4. after response/error creation -> Completed;
5. any cancellation -> Cancelled without allowing a later backward transition.

**Step 7: Bound old-stream shutdown**

Keep the existing cancel-all loop. Make the dispatcher handle mutable and wait
with:

```rust
let drain_timeout_ms = env_u64("SAG_CONNECTOR_STREAM_DRAIN_TIMEOUT_MS", 2_000)
    .clamp(100, 30_000);
if tokio::time::timeout(
    Duration::from_millis(drain_timeout_ms),
    &mut dispatch,
)
.await
.is_err()
{
    metrics::counter!("connector_stream_drain_timeout_total").increment(1);
    dispatch.abort();
    let _ = dispatch.await;
}
```

No response or active map is transferred to the next `run_tunnel_once`.

**Step 8: Run Connector tests**

```powershell
cargo fmt --package sag-connector
cargo test -p sag-connector
cargo tree -p sag-connector -e normal | Select-String "shared_storage|tokio-postgres|rusqlite"
```

Expected: tests pass and the dependency search prints nothing.

**Step 9: Commit**

```powershell
git add proxy/connectors/sag-connector/Cargo.toml proxy/connectors/sag-connector/src/main.rs Cargo.lock
git commit -m "feat: acknowledge and bound connector stream epochs"
```

### Task 5: Preserve outcome-unknown semantics through Bridge

**Files:**

- Modify: `proxy/http-tunnel-bridge/src/main.rs`
- Modify: `proxy/http-tunnel-bridge/src/queue.rs`

**Step 1: Write failing Bridge tests**

Add tests proving:

- gRPC `x-sag-outcome=unknown` is preserved as an HTTP response header/body;
- attempt ID is preserved in the error response;
- stream loss performs zero same-request retries;
- reconnect replaces a channel only for a later request;
- queued mutating work with an unresolved idempotency claim is marked failed or
  indeterminate and is not re-enqueued automatically.

**Step 2: Run tests and verify failure**

```powershell
cargo test -p http-tunnel-bridge
```

Expected: metadata-preservation tests fail against the current string-only
transport error.

**Step 3: Represent structured tunnel failure**

Replace the string-only tunnel error with a structure that can carry:

```rust
struct TunnelFailure {
    message: String,
    outcome: Option<String>,
    attempt_id: String,
    stream_epoch: Option<String>,
}
```

When tonic Forward returns an error, copy only validated ASCII metadata values.
Do not trust metadata to trigger retry; it is response classification only.

**Step 4: Return a structured HTTP error**

For outcome unknown, return a 502 response with:

- `x-sag-outcome: unknown`;
- `x-sag-attempt-id`;
- JSON body containing error, outcome, request ID, attempt ID, and trace ID.

Do not emit `Retry-After` for outcome unknown. Keep deadline errors mapped to the
existing timeout response.

**Step 5: Preserve one-attempt behavior**

Keep exactly one `client.forward(request)` call per logical request. Channel
replacement remains asynchronous preparation for a later request. Add a static
guard assertion that `for attempt in 0..2` is absent.

**Step 6: Run Bridge tests**

```powershell
cargo fmt --package http-tunnel-bridge
cargo test -p http-tunnel-bridge
pwsh -NoProfile -File scripts/ops/verify-stream-epoch-contract.ps1
```

Expected: tests and guard pass.

**Step 7: Commit**

```powershell
git add proxy/http-tunnel-bridge/src/main.rs proxy/http-tunnel-bridge/src/queue.rs scripts/ops/verify-stream-epoch-contract.ps1 scripts/ops/verify-stream-epoch-contract.sh
git commit -m "feat: expose indeterminate tunnel outcomes"
```

### Task 6: Prove the existing idempotency ledger remains authoritative

**Files:**

- Modify: `shared/storage/src/idempotency.rs` test module only unless a real bug is found
- Modify: `proxy/agents/stealth-tunnel-agent/src/grpc_server.rs` test module
- Modify: `docs/ops/request-deadline-cancellation.md`

**Step 1: Add a failing regression test for the dispatch boundary**

Using an isolated SQLite store:

1. claim a mutating operation;
2. simulate local stream enqueue success;
3. lose the stream before ForwardAccepted;
4. drop the request guard;
5. claim again with the same key/hash and a fresh attempt.

Expected assertion: the second claim is `Pending`, not `Claimed`. This proves a
missing Ack cannot release a possibly dispatched mutation.

**Step 2: Preserve existing ledger behavior**

Keep the schema states `pending` and `completed`; do not add a duplicate
`indeterminate` database state. Document the semantic distinction:

- pending before local send may be released only by its exact owner;
- after local send, pending means outcome indeterminate and is never
  automatically stolen;
- completed is replayable;
- hash mismatch is conflict.

**Step 3: Add completed replay and conflict regression assertions**

Extend existing tests so the epoch work cannot regress:

- same key/hash after completion replays the exact status, headers, and body;
- same key/different hash remains Conflict;
- elapsed time deletes only expired completed rows, never pending rows;
- Connector has no storage dependency after the change.

**Step 4: Run storage and Agent tests**

```powershell
cargo test -p shared_storage idempotency
cargo test -p stealth-tunnel-agent idempotency
```

Expected: all claim, owner, replay, conflict, and stream-loss tests pass.

**Step 5: Commit**

```powershell
git add shared/storage/src/idempotency.rs proxy/agents/stealth-tunnel-agent/src/grpc_server.rs docs/ops/request-deadline-cancellation.md
git commit -m "test: preserve idempotency across stream loss"
```

### Task 7: Add metrics, configuration, and executable fault verification

**Files:**

- Modify: `docker-compose.yml`
- Modify: `docker-compose.intra.yml`
- Modify: `.env.example`
- Modify: `.env.dualhost.example`
- Modify: `docs/ops/config-dictionary.md`
- Modify: `docs/ops/request-deadline-cancellation.md`
- Modify: `docs/ops/tunnel-loadtest-correlation.md`
- Create: `scripts/ops/test-stream-epoch-reconnect.ps1`
- Create: `scripts/ops/test-stream-epoch-reconnect.sh`

**Step 1: Add bounded-drain and handshake configuration**

Document and render:

- `SAG_CONNECTOR_REGISTER_ACK_TIMEOUT_MS=5000`;
- `SAG_CONNECTOR_STREAM_DRAIN_TIMEOUT_MS=2000`.

Keep them Connector-only. Do not add storage or database variables to Intra.

**Step 2: Add low-cardinality metrics**

Add these families without epoch/request/attempt label values:

- `agent_stream_epoch_rejected_total{reason}`;
- `agent_pending_transition_total{from,to}`;
- `agent_pending_failed_total{reason,phase}`;
- `connector_stream_handshake_total{result}`;
- `connector_attempt_transition_total{from,to}`;
- `connector_stream_cancelled_attempts_total`;
- `connector_stream_drain_timeout_total`.

Epoch, request ID, attempt ID, endpoint, and trace ID belong in structured logs.

**Step 3: Build the fault script around public/runtime interfaces**

The scripts must:

1. verify Agent and Connector metrics endpoints;
2. start a delayed read request and a keyed delayed mutation;
3. capture pending/attempt metrics;
4. restart only Connector to force a new epoch;
5. assert both old calls fail within the configured drain/transport detection
   budget instead of 58 seconds;
6. assert the new tunnel gets a different logged epoch and serves a new read;
7. repeat the mutation key and assert no second downstream hit while the claim
   is unresolved;
8. assert pending gauges return to zero and late/wrong-epoch counters are
   classified;
9. save logs, metric snapshots, IDs, epochs, timings, and mock hit counts under
   `artifacts/stream-epoch-<timestamp>/`.

The script must not delete Redis data, idempotency rows, or Docker volumes.

**Step 4: Run static and local fault checks**

```powershell
pwsh -NoProfile -File scripts/ops/verify-stream-epoch-contract.ps1
pwsh -NoProfile -File scripts/ops/verify-connector-db-independence.ps1
pwsh -NoProfile -File scripts/ops/test-stream-epoch-reconnect.ps1
```

Expected: both static guards pass; fault verification produces an artifact
directory and reports no old-request replay.

**Step 5: Commit**

```powershell
git add docker-compose.yml docker-compose.intra.yml .env.example .env.dualhost.example docs/ops/config-dictionary.md docs/ops/request-deadline-cancellation.md docs/ops/tunnel-loadtest-correlation.md scripts/ops/test-stream-epoch-reconnect.ps1 scripts/ops/test-stream-epoch-reconnect.sh
git commit -m "ops: verify stream epoch reconnection semantics"
```

### Task 8: Run the full failure matrix and release as one protocol set

**Files:**

- Verification and deployment state only
- Modify documentation only when an observed command or expectation is wrong

**Step 1: Run formatting and focused tests**

```powershell
cargo fmt --all -- --check
cargo test -p sag-tunnel-proto
cargo test -p shared_storage
cargo test -p stealth-tunnel-agent
cargo test -p sag-connector
cargo test -p http-tunnel-bridge
```

Expected: PASS in the approved build environment.

**Step 2: Run workspace compilation and tests**

```powershell
cargo check --workspace
cargo test --workspace
```

Expected: PASS. Do not downgrade a failing correctness assertion to a warning.

**Step 3: Run static architecture guards**

```powershell
pwsh -NoProfile -File scripts/ops/verify-stream-epoch-contract.ps1
pwsh -NoProfile -File scripts/ops/verify-connector-db-independence.ps1
pwsh -NoProfile -File scripts/ops/verify-timeout-chain.ps1
```

Expected: epoch fields/tags, one-attempt Bridge, Connector database independence,
and the existing deadline ladder all pass.

**Step 4: Execute the scenario matrix**

With isolated PostgreSQL, Redis, Agent, two Connector sessions for one logical
endpoint, Bridge, and a counting/delayed mock APISIX, verify:

1. RegisterAck gates readiness.
2. Session A and B receive round-robin requests.
3. Killing A fails only A-bound waiters; B continues serving.
4. A reconnects with a new epoch; an injected old-A response cannot complete a
   new attempt.
5. N lost-session waiters complete promptly and release all permits.
6. Connector cancels queued and executing work and never exceeds the 2-second
   dispatcher-drain bound.
7. No old attempt appears on the new stream.
8. A keyed mutation whose response is lost produces outcome unknown, leaves one
   pending durable claim, and creates at most one counted downstream hit.
9. A completed keyed mutation replays exactly; a changed payload conflicts.
10. No metric uses request, attempt, trace, or epoch as a label.

**Step 5: Render every deployment shape**

```powershell
docker compose -f docker-compose.yml -f docker-compose.release.yml config --quiet
docker compose -f docker-compose.edge.yml -f docker-compose.release.edge.yml config --quiet
docker compose -f docker-compose.intra.yml -f docker-compose.release.intra.yml config --quiet
```

Expected: all render; Connector contains both new timeout variables and no
database variables.

**Step 6: Perform the coordinated release**

1. Stop new ingress.
2. Drain Redis queue, Bridge synchronous work, and Agent pending waiters to zero.
3. Stop Bridge, every Connector, and every Agent.
4. Deploy the one tested protocol tuple.
5. Start all Agents.
6. Start Connectors and wait for matching RegisterAck/healthy-session metrics.
7. Start Bridge and run read, keyed-write, replay, conflict, cancel, and
   reconnect smoke tests.
8. Resume ingress only after the matrix passes.

Rollback replaces Connector, Agent, Bridge, and generated protocol code as one
set. Never live-downgrade one participant, delete pending idempotency rows, or
restore Connector database access.

**Step 7: Record final evidence**

Store package test summaries, static-guard output, Compose renders, fault
artifacts, image/source identities, and metric snapshots with the release
record. Mark ADR 0002 `Accepted and implemented` only after this evidence
passes.

**Step 8: Final commit if Git is available**

```powershell
git add docs/adr/0002-stream-epoch-and-request-outcome.md docs/plans/2026-07-26-stream-epoch-request-state-design.md docs/ops
git commit -m "docs: record tunnel epoch rollout evidence"
```

## Definition of done

- Wire epoch and actual Agent-local generation are both enforced.
- Connector readiness requires RegisterAck.
- Agent records and exposes Queued/Sent/Accepted without treating a missing Ack
  as proof of non-dispatch.
- Lost sessions fail only their own requests promptly with outcome unknown.
- Connector old-stream cancellation and drain are bounded.
- Bridge never retries the same logical request automatically.
- Existing durable idempotency behavior is preserved and tested; no second
  store exists and Connector remains database-independent.
- Multi-session routing, mTLS certificate binding, heartbeat leases, deadlines,
  cancellation, backpressure, metrics, and existing tests do not regress.
- Static guards, fault injection, full tests, Compose renders, and a coordinated
  release all pass in a build environment with a working linker.

