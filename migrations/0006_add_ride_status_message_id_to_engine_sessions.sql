ALTER TABLE telegram_engine_sessions
    ADD COLUMN IF NOT EXISTS ride_status_message_id BIGINT;
