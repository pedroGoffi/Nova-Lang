-- Future PostgreSQL backend (not used by the memory-backed example yet).
-- Never store raw session tokens. Store SHA-256 digests indexed per session.
CREATE SCHEMA IF NOT EXISTS auth;
CREATE TABLE IF NOT EXISTS auth.user_account (
    id BIGINT GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    username TEXT NOT NULL UNIQUE,
    password_hash TEXT NOT NULL,
    role TEXT NOT NULL DEFAULT 'USER',
    enabled BOOLEAN NOT NULL DEFAULT TRUE,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE TABLE IF NOT EXISTS auth.session (
    token_sha256 CHAR(64) PRIMARY KEY,
    user_id BIGINT NOT NULL REFERENCES auth.user_account(id) ON DELETE CASCADE,
    expires_at TIMESTAMPTZ NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    revoked_at TIMESTAMPTZ
);
CREATE INDEX IF NOT EXISTS auth_session_user_active_idx
    ON auth.session(user_id,expires_at) WHERE revoked_at IS NULL;
-- Example cleanup job run periodically in the database scheduler:
-- DELETE FROM auth.session WHERE expires_at <= now() OR revoked_at IS NOT NULL;
