ALTER TABLE telegram_subscription_sanctions
    DROP CONSTRAINT IF EXISTS telegram_subscription_sanctions_last_pre_expiry_day_check;

ALTER TABLE telegram_subscription_sanctions
    ADD CONSTRAINT telegram_subscription_sanctions_last_pre_expiry_day_check
        CHECK (
            last_pre_expiry_reminded_day IS NULL
            OR last_pre_expiry_reminded_day BETWEEN 0 AND 5
        );
