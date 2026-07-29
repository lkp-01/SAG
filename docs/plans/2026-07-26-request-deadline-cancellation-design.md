# Request Deadline, Cancellation, and Idempotency Design

## Status

Accepted for implementation on 2026-07-26.

## First-principles invariants

1. A request has one logical identity and one absolute deadline. Every queue, semaphore, network call, and retry must consume the same remaining budget.
2. Cancellation is advisory resource reclamation. It can stop queued or in-flight work, but it cannot undo an already committed side effect.
3. Non-read requests are never retried merely because a response was lost. They require an idempotency key and an atomic durable claim before dispatch.
4. A completed idempotent response may be replayed. A still-pending durable claim is never automatically stolen because its side effect may already have committed.
5. A pending waiter represents exactly one transport attempt. Its registration, capacity permit, cleanup, and cancel message have one RAII lifetime.
6. A late response may never complete another attempt and may never be silent.
7. Optimizations such as coalescing and retry are removable when they weaken correctness.

## Chosen architecture

- Extend the tunnel protocol with `attempt_id`, `deadline_unix_ms`, `idempotency_key`, and `CancelRequest`.
- Bridge creates the absolute deadline, propagates a trace ID, requires an idempotency key for mutating methods, and performs one gRPC attempt only.
- Agent uses a semaphore permit and generation-aware pending guard. Dropping the guard removes only its own attempt and emits a best-effort cancel.
- Agent owns a PostgreSQL/SQLite idempotency ledger because Connector is intentionally database-independent. The ledger atomically claims mutating operations and stores completed responses. A crash-left `pending` claim blocks automatic replay and exposes an indeterminate result rather than duplicating a side effect.
- Connector tracks cancellation handles by `attempt_id`, rejects expired queued work, and applies the remaining deadline to reqwest. It remains independent of PostgreSQL.
- Connector response-body errors become explicit 502/504 responses.
- Policy request coalescing is removed; the existing cache and concurrency semaphore remain.

## Trade-offs

- The durable ledger provides at-most-once gateway dispatch plus replay of known completed results. True exactly-once business effects still require the downstream business service to honor the propagated `Idempotency-Key`; no gateway can atomically commit an unrelated business database transaction.
- A crash-left pending claim sacrifices availability for safety. Operators can reconcile or expire it manually after checking the business system.
- Automatic Bridge retry is removed. Read retry can be reintroduced later only with the same deadline and a fresh attempt ID.
- Agent horizontal scaling uses `SAG_TUNNEL_ENDPOINTS`: every Connector establishes one stream to every explicit Agent address and divides its configured total capacity across those streams. A random load-balancer address alone is still invalid because it cannot guarantee that every Agent owns a return path.

## Verification targets

- Repeated mutating request with the same key executes Connector forwarding once and replays the stored result.
- Same key with a different payload returns conflict.
- Bridge/Agent timeout emits Connector cancel and frees the pending permit.
- Cancel before dispatch prevents APISIX traffic; cancel during HTTP drops the reqwest future.
- An expired queue item never reaches Connector/APISIX.
- Response-body failure never returns the upstream success status.
- Metrics expose pending current, late responses, cancels, deadline expiry, idempotency outcomes, and timeout stage.
