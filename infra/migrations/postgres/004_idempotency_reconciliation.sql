-- Additive, rolling-upgrade-safe idempotency reconciliation state.
-- `pending` remains accepted by the constraint until every old binary has
-- drained; new readers conservatively treat any late legacy pending row as
-- indeterminate and never steal it.

ALTER TABLE idempotency_records
  ADD COLUMN IF NOT EXISTS state_version BIGINT NOT NULL DEFAULT 1,
  ADD COLUMN IF NOT EXISTS dispatched_at_ms BIGINT,
  ADD COLUMN IF NOT EXISTS completed_at_ms BIGINT,
  ADD COLUMN IF NOT EXISTS reconciled_by TEXT,
  ADD COLUMN IF NOT EXISTS reconcile_reason TEXT,
  ADD COLUMN IF NOT EXISTS result_hash TEXT;

UPDATE idempotency_records
SET state = 'indeterminate',
    state_version = GREATEST(state_version, 1),
    updated_at_ms = GREATEST(updated_at_ms, created_at_ms)
WHERE state = 'pending';

DO $$
DECLARE
  constraint_name TEXT;
BEGIN
  FOR constraint_name IN
    SELECT conname
    FROM pg_constraint
    WHERE conrelid = 'idempotency_records'::regclass
      AND contype = 'c'
      AND pg_get_constraintdef(oid) LIKE '%state%'
  LOOP
    EXECUTE format(
      'ALTER TABLE idempotency_records DROP CONSTRAINT %I',
      constraint_name
    );
  END LOOP;
END $$;

ALTER TABLE idempotency_records
  ADD CONSTRAINT idempotency_records_state_v2_check
  CHECK (state IN (
    'pending',
    'claimed',
    'dispatched',
    'completed',
    'indeterminate',
    'completed_by_operator',
    'released_by_operator'
  )) NOT VALID;

ALTER TABLE idempotency_records
  VALIDATE CONSTRAINT idempotency_records_state_v2_check;

CREATE INDEX IF NOT EXISTS idx_idempotency_reconciliation_queue
  ON idempotency_records(state, updated_at_ms)
  WHERE state = 'indeterminate';

CREATE TABLE IF NOT EXISTS idempotency_reconciliation_events (
  event_id TEXT PRIMARY KEY,
  scope_key TEXT NOT NULL,
  previous_state TEXT NOT NULL,
  new_state TEXT NOT NULL,
  previous_version BIGINT NOT NULL,
  new_version BIGINT NOT NULL,
  reconciled_by TEXT NOT NULL,
  reconcile_reason TEXT NOT NULL,
  result_hash TEXT,
  created_at_ms BIGINT NOT NULL
);

CREATE INDEX IF NOT EXISTS idx_idempotency_reconciliation_events_scope
  ON idempotency_reconciliation_events(scope_key, created_at_ms);
