CREATE TABLE IF NOT EXISTS telegram_engine_sessions (
    id BIGSERIAL PRIMARY KEY,
    imei TEXT NOT NULL,
    chat_id BIGINT NOT NULL,
    trigger_heartbeat_id BIGINT NOT NULL,
    prompt_message_id BIGINT NOT NULL,
    ride_status_message_id BIGINT,
    session_status TEXT NOT NULL,
    created_at TIMESTAMPTZ NOT NULL,
    updated_at TIMESTAMPTZ NOT NULL,
    resolved_at TIMESTAMPTZ
);

CREATE INDEX IF NOT EXISTS idx_telegram_engine_sessions_imei_chat_created_at
    ON telegram_engine_sessions (imei, chat_id, created_at DESC);
