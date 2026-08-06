# Connector Health and Configuration Convergence Design

## Status

Accepted for implementation on 2026-08-04. The user requested both a plan and
immediate implementation. This design records the audited baseline so the work
extends the existing session-epoch hardening instead of rebuilding it.

Implemented in the current worktree on 2026-08-04. Production rollout remains
gated on live APISIX/Redis fault exercises and load-based timeout tuning.

## Audited baseline

The tunnel already has a 2-second Connector heartbeat, a 10-second Agent lease,
one-second lease reaping, multiple sessions per logical endpoint, an Agent-local
generation, a wire `stream_epoch`, registration acknowledgement, request
acceptance acknowledgement, generation-aware pending cleanup, late-response
fencing, bounded Connector drain, and a durable mutation idempotency ledger.

The remaining health gaps are narrower but important:

- eligibility checks heartbeat age only; it does not check whether the session
  sender is closed or currently able to accept a message;
- an Agent-side send failure does not synchronously revoke the failed session;
- Connector protocol/read failures can return before common cancellation and
  drain cleanup;
- response/probe timeouts do not trip the affected session;
- there is no synthetic Agent -> Connector dispatcher -> Agent round trip;
- an ordinary no-Connector `Unavailable` can surface as HTTP 502 rather than
  the required 503.

The configuration plane currently has no durable generation, Agent apply ACK,
outbox, delete tombstone, or actual APISIX diff. Its periodic reconciliation is
only a blind PUT of currently known apps. The in-process route cache version is
not a configuration version and resets on restart.

## Decisions

### Connector session truth

A session is eligible only when all of these are true:

1. its heartbeat lease is fresh;
2. its gRPC stream is still registered and has not been revoked;
3. its Agent-to-Connector sender is open and has capacity;
4. after the probe phase is enabled, its most recent dispatcher probe is within
   the configured freshness window.

All removal paths call one generation-aware revoke operation. Revocation removes
only that session, signals its stream to close, and resolves every waiter bound
to that generation with an explicit stream-lost outcome. A failed `try_send`
proves that the message was not enqueued, so the same attempt may be offered to
another healthy session. Once a request is `Sent`, `Accepted`, or waiting for a
response, a disconnect or timeout is outcome-unknown and must never cause an
automatic cross-stream replay. This preserves the existing idempotency safety
model for mutations.

The active probe uses dedicated `HealthProbe` / `HealthProbeAck` frames gated by
the `health-probe-v1` registration capability and the normal bounded Connector
accept/dispatch queue. The Connector acknowledges locally without contacting
APISIX only after the dispatcher executes the probe. This detects a live
heartbeat task with a stalled request dispatcher while avoiding an artificial
dependency on an application upstream. Probe IDs and epochs are fenced and
never become metric labels.

### Desired configuration and APISIX ownership

The authoritative APISIX resource remains one managed route per `app_id`.
APISIX-safe app IDs retain `sag-route-{app_id}`; unsafe or overlong IDs use a
bounded SHA-256-derived `sag-route-v2-*` ID so the former lossy sanitizer cannot
collide. A route is desired only when the app has at
least one `tunnel_routes` row and an `intranet_upstreams` row. The current
`api_routes` table remains management metadata in this change; making its
method/path rows generate APISIX route objects is a separate design.

Three durable tables form the convergence state:

- `config_state`: singleton desired `generation` and update time;
- `agent_config_applies`: each Agent's monotonic applied generation, canonical
  snapshot fingerprint, and apply / report times;
- `config_sync_jobs`: durable APISIX UPSERT or DELETE work with
  `PENDING/APPLIED/FAILED`, attempts, retry time, and last error.

Every tunnel-route or intranet-upstream mutation commits its domain change,
audit record, one generation increment, and affected outbox jobs in the same
database transaction. A DELETE job is the tombstone. Changing a host from one
app to another creates work for both apps; the old app receives DELETE only
when no other tunnel route still owns it. Newer work for the same APISIX
resource supersedes older non-terminal work so a delayed old UPSERT cannot
resurrect a deleted route.

### Versioned Agent apply and acknowledgement

`GET /api/v1/agent/routes` keeps its existing JSON array for compatibility with
the admin frontends and scripts. The rows and generation are read from one
consistent database snapshot, and the generation is returned in
`X-SAG-Config-Generation`. This is a versioned snapshot without a breaking body
change.

The Agent builds and validates the replacement map first, then stages it under
its route lock while route resolution and readiness remain fail-closed for that
generation. It POSTs an acknowledgement to `/api/v1/agent/routes/ack`; only
after one endpoint durably commits the ACK does the Agent atomically publish
the staged map and `applied_generation`:

```json
{
  "agent_id": "edge-agent-a",
  "applied_generation": 42,
  "applied_at_ms": 1785840000000,
  "snapshot_hash": "<64 lowercase SHA-256 hex characters>"
}
```

`SAG_AGENT_INSTANCE_ID` is the stable identity; the process hostname is the
fallback. Older snapshots and ACKs never move state backwards. ACK delivery is
retried independently of snapshot staging; an ACK outage leaves the new map
unservable rather than exposing a version that cannot survive restart. On
restart, the Agent restores the durable
generation and fingerprint as a floor but serves no routes and remains unready
until it fetches the matching snapshot or a newer generation. A stale fallback
therefore cannot become authoritative merely because the process restarted.
Staging a newer generation deliberately hides the previously published routes
until the new ACK commits. This is a fail-closed availability tradeoff: an ACK
outage can return 503, but it cannot expose or later roll back an uncommitted
configuration.

Every configured control-plane sync endpoint and the Agent's storage backend
must use the same durable PostgreSQL cluster (or the same SQLite file in a
single-host deployment). An ACK from an endpoint backed by an unrelated
database is not a valid restart fence and is an unsupported topology.

### Outbox worker and real reconciliation

The APISIX worker leases due jobs in small batches, applies idempotent PUT or
DELETE operations, treats DELETE 404 as success, and persists success or a
bounded exponential-backoff failure. Work is serialized per managed resource;
PostgreSQL multi-replica deployments use row locking/advisory locking rather
than relying on one process's memory.

The reconciler reads the APISIX Admin API and compares a canonical expected
subset with actual managed `sag-route-*` resources. Missing or different routes
enqueue UPSERT; extra managed routes enqueue DELETE; unmanaged routes are never
modified. Drift creates metrics and durable work instead of only a log line.

An APISIX request that times out has an unknown external outcome. The worker
retains the per-app lease for a configurable isolation window before allowing a
new generation to run. Because the APISIX Admin API has no generation-aware
CAS, this is a finite quarantine rather than a mathematical fence: operations
must configure the isolation window above APISIX's server-side hard execution
deadline. A write that violates that assumption is detected and eventually
repaired by periodic reconciliation; live delayed-write fault evidence remains
a production release gate.

## Error and retry semantics

- Heartbeat expiry, gRPC EOF/read failure, closed sender, Connector outbound
  failure, failed probe, or configured response timeout revokes that generation.
- A pre-enqueue channel failure may select another session with the same attempt
  because no Connector accepted the message.
- After enqueue, failures return HTTP 503. Mutations retain their durable
  indeterminate claim and require caller/downstream idempotency reconciliation.
- APISIX mutation handlers return after the desired state and outbox commit;
  APISIX availability does not decide whether the control-plane write is lost.
- Reconciliation and outbox retries never operate on non-SAG APISIX resources.

## Rollout

1. Deploy schema and backward-compatible storage readers.
2. Deploy atomic generation/outbox writes and APISIX worker/delete support.
3. Deploy version-aware Agent apply/ACK.
4. Deploy the coordinated tunnel protocol/probe wave to Agent and Connector.
5. Enable probe-based revocation conservatively, then tune interval, timeout,
   and failure threshold with fault and load tests.
6. Enable actual APISIX diff reconciliation and alerts.

The protocol probe wave is capability-negotiated: deploy Connector support
first, then enable probes on Agents. The Agent binary default remains disabled
for mixed-version rolling upgrades. Database and HTTP changes are backward
compatible and can precede it.

## Success criteria

- Closed/full send channels are never reported healthy or selected for new
  traffic.
- Every session-ending path runs generation-specific cleanup and wakes its
  waiters promptly.
- A stalled Connector dispatcher is removed within the configured probe budget.
- No-Connector and outcome-unknown paths return HTTP 503; no post-send request
  is transparently replayed.
- Route mutation, generation, audit, and APISIX job commit or roll back together.
- Agent applied generations are observable and monotonic.
- APISIX failures survive process restart, deletes cannot leave ghost routes,
  and periodic reconciliation repairs missing, changed, and extra managed
  routes without touching unmanaged configuration.
