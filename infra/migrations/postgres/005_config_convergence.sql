-- Durable desired-generation, Agent apply acknowledgements, and APISIX outbox.
-- Apply after 004_idempotency_reconciliation.sql.

CREATE TABLE IF NOT EXISTS config_state (
  id SMALLINT PRIMARY KEY CHECK (id = 1),
  generation BIGINT NOT NULL DEFAULT 0 CHECK (generation >= 0),
  updated_at_ms BIGINT NOT NULL DEFAULT 0 CHECK (updated_at_ms >= 0)
);

INSERT INTO config_state (id, generation, updated_at_ms)
VALUES (1, 0, 0)
ON CONFLICT (id) DO NOTHING;

CREATE TABLE IF NOT EXISTS agent_config_applies (
  agent_id TEXT PRIMARY KEY CHECK (length(agent_id) > 0),
  applied_generation BIGINT NOT NULL CHECK (applied_generation >= 0),
  snapshot_hash TEXT,
  applied_at_ms BIGINT NOT NULL CHECK (applied_at_ms >= 0),
  reported_at_ms BIGINT NOT NULL CHECK (reported_at_ms >= 0),
  CONSTRAINT agent_config_applies_snapshot_hash_format CHECK (
    snapshot_hash IS NULL OR snapshot_hash ~ '^[0-9a-f]{64}$'
  )
);

ALTER TABLE agent_config_applies
  ADD COLUMN IF NOT EXISTS snapshot_hash TEXT;

-- Existing prerelease tables may already contain a nullable fingerprint column.
-- NOT VALID keeps this migration deployable while still constraining new writes.
DO $migration$
BEGIN
  IF NOT EXISTS (
    SELECT 1
    FROM pg_constraint
    WHERE conrelid = 'agent_config_applies'::regclass
      AND conname = 'agent_config_applies_snapshot_hash_format'
  ) THEN
    ALTER TABLE agent_config_applies
      ADD CONSTRAINT agent_config_applies_snapshot_hash_format CHECK (
        snapshot_hash IS NULL OR snapshot_hash ~ '^[0-9a-f]{64}$'
      ) NOT VALID;
  END IF;
END
$migration$;

CREATE INDEX IF NOT EXISTS idx_agent_config_applies_generation
  ON agent_config_applies(applied_generation, reported_at_ms);

CREATE INDEX IF NOT EXISTS idx_agent_config_applies_reported
  ON agent_config_applies(reported_at_ms);

CREATE TABLE IF NOT EXISTS config_sync_jobs (
  job_id TEXT PRIMARY KEY CHECK (length(job_id) > 0),
  generation BIGINT NOT NULL CHECK (generation >= 0),
  target TEXT NOT NULL CHECK (length(target) > 0),
  resource_type TEXT NOT NULL CHECK (length(resource_type) > 0),
  resource_id TEXT NOT NULL CHECK (length(resource_id) > 0),
  app_id TEXT NOT NULL CHECK (length(app_id) > 0),
  operation TEXT NOT NULL CHECK (operation IN ('UPSERT', 'DELETE')),
  payload_json TEXT,
  status TEXT NOT NULL DEFAULT 'PENDING'
    CHECK (status IN ('PENDING', 'APPLIED', 'FAILED')),
  attempt_count BIGINT NOT NULL DEFAULT 0 CHECK (attempt_count >= 0),
  next_attempt_at_ms BIGINT NOT NULL DEFAULT 0 CHECK (next_attempt_at_ms >= 0),
  last_error TEXT,
  lease_owner TEXT,
  lease_expires_at_ms BIGINT,
  superseded_by_generation BIGINT,
  created_at_ms BIGINT NOT NULL CHECK (created_at_ms >= 0),
  updated_at_ms BIGINT NOT NULL CHECK (updated_at_ms >= 0),
  applied_at_ms BIGINT,
  CONSTRAINT config_sync_jobs_lease_pair CHECK (
    (lease_owner IS NULL AND lease_expires_at_ms IS NULL)
    OR (lease_owner IS NOT NULL AND length(lease_owner) > 0 AND lease_expires_at_ms IS NOT NULL)
  ),
  CONSTRAINT config_sync_jobs_superseded_generation CHECK (
    superseded_by_generation IS NULL OR superseded_by_generation > generation
  ),
  CONSTRAINT config_sync_jobs_applied_at CHECK (
    applied_at_ms IS NULL OR applied_at_ms >= 0
  )
);

CREATE UNIQUE INDEX IF NOT EXISTS uq_config_sync_jobs_resource_generation
  ON config_sync_jobs(target, resource_type, resource_id, generation);

-- Backfill the invariant before creating the partial unique index so this
-- migration remains safe if an early prerelease build already wrote jobs.
UPDATE config_sync_jobs AS older
SET superseded_by_generation = latest.generation
FROM (
  SELECT target, resource_type, resource_id, MAX(generation) AS generation
  FROM config_sync_jobs
  GROUP BY target, resource_type, resource_id
) AS latest
WHERE older.target = latest.target
  AND older.resource_type = latest.resource_type
  AND older.resource_id = latest.resource_id
  AND older.generation < latest.generation
  AND older.superseded_by_generation IS NULL;

CREATE UNIQUE INDEX IF NOT EXISTS uq_config_sync_jobs_current_resource
  ON config_sync_jobs(target, resource_type, resource_id)
  WHERE superseded_by_generation IS NULL;

CREATE INDEX IF NOT EXISTS idx_config_sync_jobs_due
  ON config_sync_jobs(next_attempt_at_ms, generation, created_at_ms)
  WHERE status IN ('PENDING', 'FAILED') AND superseded_by_generation IS NULL;

CREATE INDEX IF NOT EXISTS idx_config_sync_jobs_resource
  ON config_sync_jobs(target, resource_type, resource_id, generation DESC);
