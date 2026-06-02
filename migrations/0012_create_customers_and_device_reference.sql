CREATE TABLE IF NOT EXISTS customers (
    id BIGSERIAL PRIMARY KEY,
    name TEXT NOT NULL,
    phone_number TEXT NOT NULL,
    address TEXT NOT NULL,
    imei TEXT NOT NULL,
    id_card TEXT NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

ALTER TABLE devices
    ADD COLUMN IF NOT EXISTS referenced_by_customer_id BIGINT;

DO $$
BEGIN
    IF NOT EXISTS (
        SELECT 1
        FROM information_schema.table_constraints
        WHERE constraint_name = 'devices_referenced_by_customer_id_fkey'
          AND table_name = 'devices'
    ) THEN
        ALTER TABLE devices
            ADD CONSTRAINT devices_referenced_by_customer_id_fkey
            FOREIGN KEY (referenced_by_customer_id)
            REFERENCES customers(id)
            ON DELETE SET NULL;
    END IF;
END $$;

CREATE INDEX IF NOT EXISTS idx_devices_referenced_by_customer_id
    ON devices (referenced_by_customer_id);
