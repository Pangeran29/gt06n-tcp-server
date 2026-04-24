CREATE TABLE IF NOT EXISTS telegram_subscriptions (
    id BIGSERIAL PRIMARY KEY,
    telegram_user_id BIGINT NOT NULL REFERENCES telegram_users (telegram_user_id),
    chat_id BIGINT NOT NULL,
    plan_code TEXT NOT NULL DEFAULT 'monthly_stars',
    status TEXT NOT NULL,
    current_period_start_at TIMESTAMPTZ,
    current_period_end_at TIMESTAMPTZ,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    CONSTRAINT telegram_subscriptions_status_check
        CHECK (status IN ('active', 'expired', 'canceled', 'past_due')),
    CONSTRAINT telegram_subscriptions_telegram_user_plan_unique
        UNIQUE (telegram_user_id, plan_code)
);

CREATE INDEX IF NOT EXISTS idx_telegram_subscriptions_user_status
    ON telegram_subscriptions (telegram_user_id, status);

CREATE INDEX IF NOT EXISTS idx_telegram_subscriptions_current_period_end_at
    ON telegram_subscriptions (current_period_end_at);

CREATE TABLE IF NOT EXISTS telegram_payment_events (
    id BIGSERIAL PRIMARY KEY,
    telegram_user_id BIGINT NOT NULL REFERENCES telegram_users (telegram_user_id),
    chat_id BIGINT NOT NULL,
    subscription_id BIGINT REFERENCES telegram_subscriptions (id),
    payment_kind TEXT NOT NULL,
    payment_status TEXT NOT NULL,
    plan_code TEXT NOT NULL DEFAULT 'monthly_stars',
    currency TEXT NOT NULL DEFAULT 'XTR',
    stars_amount BIGINT NOT NULL,
    period_days INT NOT NULL DEFAULT 30,
    invoice_payload TEXT UNIQUE,
    telegram_payment_charge_id TEXT UNIQUE,
    provider_payment_charge_id TEXT,
    paid_at TIMESTAMPTZ,
    raw_successful_payment JSONB,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    CONSTRAINT telegram_payment_events_status_check
        CHECK (payment_status IN ('pending', 'paid', 'failed', 'refunded')),
    CONSTRAINT telegram_payment_events_currency_check
        CHECK (currency = 'XTR'),
    CONSTRAINT telegram_payment_events_stars_amount_check
        CHECK (stars_amount > 0),
    CONSTRAINT telegram_payment_events_period_days_check
        CHECK (period_days > 0)
);

CREATE INDEX IF NOT EXISTS idx_telegram_payment_events_telegram_user_id
    ON telegram_payment_events (telegram_user_id);

CREATE INDEX IF NOT EXISTS idx_telegram_payment_events_subscription_id
    ON telegram_payment_events (subscription_id);

CREATE INDEX IF NOT EXISTS idx_telegram_payment_events_payment_status
    ON telegram_payment_events (payment_status);
