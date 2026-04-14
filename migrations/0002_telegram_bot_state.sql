CREATE TABLE IF NOT EXISTS telegram_bot_state (
    state_key TEXT PRIMARY KEY,
    state_value TEXT NOT NULL,
    updated_at TIMESTAMPTZ NOT NULL
);
