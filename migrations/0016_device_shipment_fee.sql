ALTER TABLE devices
    ADD COLUMN IF NOT EXISTS shipment_fee_idr BIGINT;

DO $$
BEGIN
    IF NOT EXISTS (
        SELECT 1
        FROM information_schema.table_constraints
        WHERE constraint_name = 'devices_shipment_fee_non_negative'
          AND table_name = 'devices'
    ) THEN
        ALTER TABLE devices
            ADD CONSTRAINT devices_shipment_fee_non_negative
            CHECK (shipment_fee_idr IS NULL OR shipment_fee_idr >= 0);
    END IF;
END $$;

ALTER TABLE telegram_payment_events
    ADD COLUMN IF NOT EXISTS device_id BIGINT REFERENCES devices(id) ON DELETE SET NULL;

ALTER TABLE telegram_payment_events
    ADD COLUMN IF NOT EXISTS imei TEXT;

UPDATE telegram_payment_events tpe
SET device_id = d.id,
    imei = d.imei
FROM telegram_users tu
JOIN devices d
  ON d.imei = tu.bound_imei
WHERE tpe.telegram_user_id = tu.telegram_user_id
  AND (tpe.device_id IS NULL OR tpe.imei IS NULL);

CREATE INDEX IF NOT EXISTS idx_midtrans_payment_events_device_id_payment_status
    ON telegram_payment_events (device_id, payment_status);
