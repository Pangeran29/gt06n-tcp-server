CREATE TABLE IF NOT EXISTS telegram_subscription_sanctions (
    id BIGSERIAL PRIMARY KEY,
    subscription_id BIGINT NOT NULL REFERENCES telegram_subscriptions (id) ON DELETE CASCADE,
    telegram_user_id BIGINT NOT NULL REFERENCES telegram_users (telegram_user_id),
    chat_id BIGINT NOT NULL,
    last_pre_expiry_reminded_for_period_end_at TIMESTAMPTZ,
    last_overdue_reminded_day INT,
    fine_amount_idr BIGINT NOT NULL DEFAULT 0,
    withdrawal_required BOOLEAN NOT NULL DEFAULT FALSE,
    withdrawal_required_at TIMESTAMPTZ,
    resolved_at TIMESTAMPTZ,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    CONSTRAINT telegram_subscription_sanctions_subscription_unique
        UNIQUE (subscription_id),
    CONSTRAINT telegram_subscription_sanctions_fine_non_negative
        CHECK (fine_amount_idr >= 0),
    CONSTRAINT telegram_subscription_sanctions_last_overdue_day_check
        CHECK (last_overdue_reminded_day IS NULL OR last_overdue_reminded_day >= 1)
);

CREATE INDEX IF NOT EXISTS idx_subscription_sanctions_user
    ON telegram_subscription_sanctions (telegram_user_id);

CREATE INDEX IF NOT EXISTS idx_subscription_sanctions_withdrawal
    ON telegram_subscription_sanctions (withdrawal_required)
    WHERE withdrawal_required = TRUE;
