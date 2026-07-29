# ADR 0002: Fence Tunnel Sessions and Fail Requests Explicitly

## Status

Proposed on 2026-07-26. Awaiting approval.

## Context

The data plane already has request/attempt separation, absolute deadlines,
cancellation, an Agent-local Connector generation, heartbeat leases, and a
durable Agent-side idempotency ledger. It does not yet expose the stream
incarnation on the wire, acknowledge registration/request acceptance, or return
an explicit request phase when a stream disappears.

The desired availability model is fail-fast with safe caller-controlled retry,
not transparent cross-stream replay.

## Decision

1. Connector creates a fresh opaque UUID `stream_epoch` for every tunnel run.
2. Agent keeps its local numeric generation as the authoritative in-process
   fence and requires the wire epoch to match it.
3. Add RegisterAck and ForwardAccepted protocol messages.
4. Track Agent pending phases `Queued`, `Sent`, and `Accepted`; stream removal
   sends an explicit `StreamLost` failure to matching waiters.
5. Track Connector attempt phases and bound old-stream dispatcher drain.
6. Do not migrate or replay old attempts on a new epoch.
7. Keep Bridge at one Forward attempt. Surface stream loss as outcome unknown.
8. Reuse the existing Edge idempotency ledger. Never release a possibly
   dispatched mutation claim merely because an acknowledgement or response was
   lost.
9. Deploy Connector, Agent, Bridge, and generated protocol code as one drained
   release; live mixed semantic versions are unsupported.

## Consequences

### Positive

- Both sides can identify and reject stale-session traffic.
- Connector readiness is explicit rather than inferred from gRPC setup.
- Agent waiters fail promptly with a meaningful phase.
- Reconnection latency has a bounded old-stream drain component.
- Existing downstream idempotency propagation and Agent durability remain the
  single mutation-safety mechanism.

### Negative

- The protocol participants must be upgraded and rolled back together.
- More state transitions and fault tests are required.
- Availability is deliberately sacrificed for unresolved mutating operations:
  they remain pending until reconciled or completed.
- An acceptance acknowledgement improves diagnosis but cannot prove non-delivery
  when absent.

## Rejected alternatives

- **Agent-local generation only:** insufficient for Connector-side validation
  and readiness acknowledgement.
- **Automatic resume:** requires a durable delivery/replay subsystem and is out
  of scope.
- **A second idempotency store or Connector database:** duplicates authority and
  violates Connector database independence.

## References

- `docs/plans/2026-07-26-stream-epoch-request-state-design.md`
- `docs/plans/2026-07-26-request-deadline-cancellation-design.md`
- `docs/plans/2026-07-26-connector-postgres-decoupling-revision-design.md`

