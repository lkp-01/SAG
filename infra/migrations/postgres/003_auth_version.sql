-- Forward-compatible authorization versioning for multi-instance revocation.
ALTER TABLE users
  ADD COLUMN IF NOT EXISTS auth_version BIGINT NOT NULL DEFAULT 1;

ALTER TABLE users
  ADD COLUMN IF NOT EXISTS updated_at_ms BIGINT NOT NULL DEFAULT 0;

UPDATE users
SET updated_at_ms = (EXTRACT(EPOCH FROM clock_timestamp()) * 1000)::BIGINT
WHERE updated_at_ms = 0;

DO $$
BEGIN
  IF NOT EXISTS (
    SELECT 1 FROM pg_constraint WHERE conname = 'users_auth_version_positive'
  ) THEN
    ALTER TABLE users
      ADD CONSTRAINT users_auth_version_positive CHECK (auth_version > 0) NOT VALID;
  END IF;
END $$;

ALTER TABLE users VALIDATE CONSTRAINT users_auth_version_positive;
CREATE INDEX IF NOT EXISTS idx_users_id_auth_version
  ON users(id, auth_version);
