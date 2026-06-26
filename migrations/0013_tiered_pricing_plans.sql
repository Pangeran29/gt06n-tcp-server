ALTER TABLE telegram_users
    ADD COLUMN IF NOT EXISTS pricing_tier TEXT;

UPDATE telegram_users
SET pricing_tier = 'basic'
WHERE pricing_tier IS NULL;

ALTER TABLE telegram_users
    ALTER COLUMN pricing_tier SET DEFAULT 'basic';

ALTER TABLE telegram_users
    ALTER COLUMN pricing_tier SET NOT NULL;

DO $$
BEGIN
    IF NOT EXISTS (
        SELECT 1
        FROM information_schema.table_constraints
        WHERE constraint_name = 'telegram_users_pricing_tier_check'
          AND table_name = 'telegram_users'
    ) THEN
        ALTER TABLE telegram_users
            ADD CONSTRAINT telegram_users_pricing_tier_check
            CHECK (pricing_tier IN ('basic', 'ojol'));
    END IF;
END $$;

UPDATE telegram_subscriptions
SET plan_code = 'monthly_basic'
WHERE plan_code = 'monthly_stars';

UPDATE telegram_payment_events
SET plan_code = 'monthly_basic'
WHERE plan_code = 'monthly_stars';

ALTER TABLE telegram_subscriptions
    ALTER COLUMN plan_code SET DEFAULT 'monthly_basic';

ALTER TABLE telegram_payment_events
    ALTER COLUMN plan_code SET DEFAULT 'monthly_basic';
