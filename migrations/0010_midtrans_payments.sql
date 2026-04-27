ALTER TABLE IF EXISTS telegram_payment_events
    RENAME TO telegram_payment_events_legacy_stars;

ALTER TABLE IF EXISTS telegram_payment_events_legacy_stars
    RENAME CONSTRAINT telegram_payment_events_pkey TO telegram_payment_events_legacy_stars_pkey;
ALTER TABLE IF EXISTS telegram_payment_events_legacy_stars
    RENAME CONSTRAINT telegram_payment_events_invoice_payload_key TO telegram_payment_events_legacy_stars_invoice_payload_key;
ALTER TABLE IF EXISTS telegram_payment_events_legacy_stars
    RENAME CONSTRAINT telegram_payment_events_telegram_payment_charge_id_key TO telegram_payment_events_legacy_stars_telegram_payment_charge_id_key;
ALTER INDEX IF EXISTS idx_telegram_payment_events_telegram_user_id
    RENAME TO idx_legacy_stars_payment_events_telegram_user_id;
ALTER INDEX IF EXISTS idx_telegram_payment_events_subscription_id
    RENAME TO idx_legacy_stars_payment_events_subscription_id;
ALTER INDEX IF EXISTS idx_telegram_payment_events_payment_status
    RENAME TO idx_legacy_stars_payment_events_payment_status;

CREATE TABLE IF NOT EXISTS telegram_payment_events (
    id BIGSERIAL PRIMARY KEY,
    telegram_user_id BIGINT NOT NULL REFERENCES telegram_users (telegram_user_id),
    chat_id BIGINT NOT NULL,
    subscription_id BIGINT REFERENCES telegram_subscriptions (id),
    payment_provider TEXT NOT NULL DEFAULT 'midtrans',
    payment_kind TEXT NOT NULL,
    payment_status TEXT NOT NULL,
    plan_code TEXT NOT NULL DEFAULT 'monthly_stars',
    currency TEXT NOT NULL DEFAULT 'IDR',
    gross_amount_idr BIGINT NOT NULL,
    period_days INT NOT NULL DEFAULT 30,
    provider_order_id TEXT NOT NULL UNIQUE,
    provider_transaction_id TEXT UNIQUE,
    payment_type TEXT,
    payment_url TEXT,
    qr_url TEXT,
    expires_at TIMESTAMPTZ,
    paid_at TIMESTAMPTZ,
    notified_at TIMESTAMPTZ,
    raw_create_response JSONB,
    raw_webhook_notification JSONB,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    CONSTRAINT telegram_payment_events_provider_check
        CHECK (payment_provider IN ('midtrans')),
    CONSTRAINT telegram_payment_events_status_check
        CHECK (payment_status IN ('pending', 'paid', 'expired', 'failed', 'refunded')),
    CONSTRAINT telegram_payment_events_currency_check
        CHECK (currency = 'IDR'),
    CONSTRAINT telegram_payment_events_gross_amount_check
        CHECK (gross_amount_idr > 0),
    CONSTRAINT telegram_payment_events_period_days_check
        CHECK (period_days > 0)
);

CREATE INDEX IF NOT EXISTS idx_midtrans_payment_events_telegram_user_id
    ON telegram_payment_events (telegram_user_id);

CREATE INDEX IF NOT EXISTS idx_midtrans_payment_events_subscription_id
    ON telegram_payment_events (subscription_id);

CREATE INDEX IF NOT EXISTS idx_midtrans_payment_events_payment_status
    ON telegram_payment_events (payment_status);

CREATE INDEX IF NOT EXISTS idx_midtrans_payment_events_provider_order_id
    ON telegram_payment_events (provider_order_id);
