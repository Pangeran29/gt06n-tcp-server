ALTER TABLE devices
    ADD COLUMN IF NOT EXISTS pricing_tier TEXT;

UPDATE devices d
SET pricing_tier = COALESCE(tu.pricing_tier, 'basic')
FROM telegram_users tu
WHERE tu.bound_imei = d.imei
  AND d.pricing_tier IS NULL;

UPDATE devices
SET pricing_tier = 'basic'
WHERE pricing_tier IS NULL;

ALTER TABLE devices
    ALTER COLUMN pricing_tier SET DEFAULT 'basic';

ALTER TABLE devices
    ALTER COLUMN pricing_tier SET NOT NULL;

DO $$
BEGIN
    IF NOT EXISTS (
        SELECT 1
        FROM information_schema.table_constraints
        WHERE constraint_name = 'devices_pricing_tier_check'
          AND table_name = 'devices'
    ) THEN
        ALTER TABLE devices
            ADD CONSTRAINT devices_pricing_tier_check
            CHECK (pricing_tier IN ('basic', 'ojol'));
    END IF;
END $$;

ALTER TABLE telegram_users
    DROP CONSTRAINT IF EXISTS telegram_users_pricing_tier_check;

ALTER TABLE telegram_users
    DROP COLUMN IF EXISTS pricing_tier;
