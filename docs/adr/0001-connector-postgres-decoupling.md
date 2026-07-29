# ADR-0001: Remove Direct PostgreSQL Access from sag-connector

- Status: Accepted
- Date: 2026-07-25

## Context

The business data plane uses the outbound reverse tunnel, while APISIX management uses VPN/internal DNS. The Connector additionally connects to Edge PostgreSQL solely for per-hop audit and fault persistence. That expands credentials and couples Connector startup to central database reachability.

## Decision

Remove `shared_storage` and all database writes from `sag-connector`. The
Connector is database-independent and has no durable state; it is not a
stateless request function. It retains the per-tunnel cancellation registry,
bounded accept queue, absolute deadline and `attempt_id` handling, idempotency
key propagation, and one outbound tunnel per explicit Agent endpoint.

Keep tunnel-forward audit and the durable idempotency ledger at
`stealth-tunnel-agent`, ingress audit at `http-tunnel-bridge`, and hop
observability in Connector Prometheus metrics. Every production Agent replica
uses the same Edge PostgreSQL ledger; SQLite is only valid for a single Agent.
Keep the control-plane-to-APISIX VPN path unchanged. Bind Edge PostgreSQL to
loopback for host administration while Edge containers continue using the
Docker network.

## Consequences

- Connector can start and forward without PostgreSQL.
- Intranet deployment no longer needs Edge database credentials or Edge TCP/5432 access.
- Durable `service=sag-connector` audit/fault rows stop being produced.
- Existing Agent/bridge audit rows and Connector metrics remain.
- PostgreSQL failure before an Agent idempotency claim fails a mutating request
  closed without Connector dispatch. Failure after dispatch leaves an
  indeterminate durable `pending` claim that must not be stolen automatically.
- The deadline/cancellation protocol generation is not safe to mix with the
  pre-change Connector/Agent/Bridge generation during live traffic, despite
  additive protobuf fields.
- A future compliance requirement for Connector-local durable events requires a separate batched telemetry design.

## Rollback

Roll back Connector, Agent, and Bridge as one previously verified compatible
release while traffic is stopped and drained. Do not restore Connector database
credentials or cross-host PostgreSQL exposure. The additive idempotency table
may remain; no destructive schema rollback is required.

## References

- [`2026-07-26-connector-postgres-decoupling-revision-design.md`](../plans/2026-07-26-connector-postgres-decoupling-revision-design.md)
- [`2026-07-26-request-deadline-cancellation-design.md`](../plans/2026-07-26-request-deadline-cancellation-design.md)
