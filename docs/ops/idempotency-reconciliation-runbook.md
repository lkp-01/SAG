# Idempotency reconciliation runbook

## Safety contract

The mutation ledger is a delivery-safety mechanism, not an exactly-once claim. Its legal transitions are:

```text
claimed -> dispatched -> completed
claimed -> completed
dispatched -> indeterminate
indeterminate -> completed_by_operator
indeterminate -> released_by_operator
```

`dispatched` and `indeterminate` are never expired, stolen, deleted by cleanup, or automatically re-sent. A legacy `pending` row is conservatively read and migrated as `indeterminate`. Only the exact owner and state version may release a `claimed` row before transport send.

## Triage

1. List records older than five minutes with `GET /api/v1/idempotency/indeterminate?min_age_ms=300000`.
2. Fetch the record with `GET /api/v1/idempotency/{scope_key}`. Record its `state_version`, attempt ID, request hash, dispatch time, trace/audit evidence, and upstream transaction evidence.
3. Do not infer “not executed” from a timeout, disconnect, missing response, or absent local log. Ask the upstream system of record using its transaction/reference identifiers.
4. Have a second operator review the evidence and the proposed reason. Keep incident/ticket references in the reason.

All endpoints require a current admin/boss JWT. Token version and current database roles are revalidated; a revoked or stale JWT fails closed. Each mutation also requires an exact confirmation word and state-version CAS.

## Confirm an upstream completion

Use this only when authoritative upstream evidence proves the side effect completed and the response can be reconstructed:

```powershell
$env:SAG_ADMIN_URL = "https://admin.example"
$env:SAG_RECONCILE_ADMIN_TOKEN = "<short-lived-admin-token>"
cargo run -p control-plane-admin --bin reconcile_idempotency -- complete <scope_key> <state_version> 200 "INC-123 upstream receipt verified" '{"ok":true}' --confirm COMPLETE
```

This writes `completed_by_operator`, the result, a SHA-256 result hash, operator identity, reason, timestamps, and a reconciliation event in one database transaction. A retry can then replay the stored result without another side effect.

## Confirm no execution and release

Use this only when authoritative upstream evidence proves that execution did not begin:

```powershell
cargo run -p control-plane-admin --bin reconcile_idempotency -- release <scope_key> <state_version> "INC-123 upstream absence and reservation logs verified" --confirm RELEASE
```

`released_by_operator` is a terminal evidence state. It does not silently reuse the same idempotency key; the caller must make an explicitly reviewed new operation with a new key. This preserves the original reconciliation record.

## Failure handling

- HTTP `409` means another transport completion or operator won the CAS. Fetch the record again; never repeat the decision blindly.
- HTTP `401/403` means the operator token is absent, stale, revoked, or not privileged. Do not bypass authorization with forwarded identity headers.
- HTTP `503` means authoritative storage/auth state is unavailable. The operation has not been reconciled; restore the dependency and retry with the same expected version after re-reading.
- A duplicate reconciliation event ID or audit insert error rolls back the state transition.

PostgreSQL backup/restore, incident evidence, and the reconciliation event table must follow the same retention and access-control policy as management audit logs.
