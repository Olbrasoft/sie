-- Pending speech only. Completed notification bodies are deleted, never archived.
CREATE TABLE IF NOT EXISTS notification_queue (
    id BIGINT GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    payload JSONB NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    retry_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    attempts INTEGER NOT NULL DEFAULT 0
);

-- Quota metadata contains credential references, never actual API keys or text.
CREATE TABLE IF NOT EXISTS notification_key_usage (
    reference TEXT PRIMARY KEY,
    counter_month DATE NOT NULL DEFAULT date_trunc('month', NOW() AT TIME ZONE 'UTC')::date,
    characters BIGINT NOT NULL DEFAULT 0 CHECK (characters >= 0),
    cooldown_until TIMESTAMPTZ,
    disabled BOOLEAN NOT NULL DEFAULT FALSE
);
