# Production hardening rollout

**Current state:** repository implementation is complete under workstation/static acceptance. Production qualification, Linux fault injection, managed PostgreSQL/Redis failover, three steady runs, and the two-hour soak are pending external infrastructure and credentials.

## Preconditions

- restore a recognizable Git worktree and build immutable images from a recorded 40-character SHA;
- take and restore-test a PostgreSQL backup in an isolated environment;
- provide qualified external PostgreSQL, Redis, and secret/certificate management; Compose is not a production HA database/cache platform;
- establish dashboards, alerts, on-call ownership, an approved fault adapter, short-lived operator credentials, and raw artifact storage;
- stop rollout if identity, mTLS, deadline, cancellation, idempotency, audit, or fail-closed evidence is missing.

## Ordered release

1. Apply additive migrations in order: `001_init.sql`, `002_audit_hardening.sql`, `003_auth_version.sql`, `004_idempotency_reconciliation.sql`. Validate against a restored production snapshot and prove the old binary still starts and reads/writes. Migration 004 temporarily accepts legacy `pending`; new readers treat it as `indeterminate`.
2. Release Wave 0 identity, PostgreSQL pool, audit, and invariant hardening. Observe forged/missing identity rejection, pool wait/acquire failures, audit queue depth/drops, and resource caps.
3. Release Wave 1 Redis queue scripts/workers. Drain old consumer groups first, deploy new Lua/state-machine code, then enable workers. Verify submitted count equals terminal classifications and PEL recovery before raising traffic.
4. Execute Wave 2 full-chain gates and determine the first repeatable saturation knee. Production concurrency is at most 70% of that measured knee and must satisfy the memory budget; repository defaults are not a capacity claim.
5. Stop admission, drain Bridge/Agent/Connector requests, then perform the Task 13 coordinated protocol upgrade. Upgrade Agent and Connector as a compatible set, verify RegisterAck and new stream epochs, and reject old-epoch traffic before reopening admission.
6. Add the second complete Bridge→Agent path, the ready Auth/Policy peers, APISIX peers, and external HA dependencies. Run each fault scenario separately; do not infer combined-fault tolerance.
7. Enable JWT `auth_version` enforcement and bounded invalidation caches. Prove two Auth instances reject a remotely revoked token within the declared TTL/SLO before continuing.
8. Enable idempotency reconciliation API/CLI for a restricted operator group. Rehearse dual approval, evidence capture, CAS conflict, transactional event audit, and break-glass review.
9. Run three 10–15 minute `full_chain` steady tests and a 120-minute soak, followed by the complete fault matrix. Publish capacity/HA claims only from passing immutable artifacts.

## Rollback boundaries

- Application images may roll back only to versions that reject forged identity headers, preserve mTLS validation, propagate absolute deadlines/cancellation, and understand the deployed database/protocol state.
- Do not roll back past `auth_version` enforcement after relying on remote revocation. A legacy verifier could accept revoked tokens.
- Do not roll back to code that automatically retries or steals `dispatched`, legacy `pending`, or `indeterminate` mutations.
- Do not downgrade one side of the stream-epoch protocol while live streams remain. Drain, coordinate both sides, and open a fresh epoch.
- Migrations 003/004 are forward-only during an incident. Leave additive columns/tables in place; roll back the application only within the compatible boundary. Constraint removal or data reversal requires a separately reviewed migration and restored-backup rehearsal.
- Queue worker rollback requires admission stop and PEL/group drain. Never delete Redis streams/groups to make rollback appear clean.
- If schema/protocol compatibility cannot be proven, stop admission and restore the full previous release plus its database backup into a new isolated environment; do not perform an in-place destructive downgrade.

## Promotion record

Attach commands, timestamps, host/kernel and network shape, Git SHA, image digests, migration output, config snapshot, raw load/fault artifacts, Prometheus/resource snapshots, request/job classifications, reconciliation exercises, approval identities, and all non-run checks with their concrete blockers.

