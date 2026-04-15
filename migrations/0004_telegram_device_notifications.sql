CREATE TABLE IF NOT EXISTS telegram_device_notifications (
    imei TEXT NOT NULL,
    chat_id BIGINT NOT NULL,
    last_status TEXT NOT NULL,
    last_message_id BIGINT NOT NULL,
    last_heartbeat_id BIGINT NOT NULL,
    updated_at TIMESTAMPTZ NOT NULL,
    PRIMARY KEY (imei, chat_id)
);
