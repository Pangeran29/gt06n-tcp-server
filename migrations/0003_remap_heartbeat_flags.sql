DO $$
BEGIN
    IF EXISTS (
        SELECT 1
        FROM information_schema.columns
        WHERE table_name = 'device_heartbeats' AND column_name = 'acc_high'
    ) THEN
        ALTER TABLE device_heartbeats RENAME COLUMN acc_high TO vibration_detected;
    END IF;
END $$;

DO $$
BEGIN
    IF EXISTS (
        SELECT 1
        FROM information_schema.columns
        WHERE table_name = 'device_heartbeats' AND column_name = 'gps_tracking_on'
    ) THEN
        ALTER TABLE device_heartbeats RENAME COLUMN gps_tracking_on TO bit_1_guess;
    END IF;
END $$;

DO $$
BEGIN
    IF EXISTS (
        SELECT 1
        FROM information_schema.columns
        WHERE table_name = 'device_heartbeats' AND column_name = 'alarm_active'
    ) THEN
        ALTER TABLE device_heartbeats RENAME COLUMN alarm_active TO acc_high;
    END IF;
END $$;

DO $$
BEGIN
    IF EXISTS (
        SELECT 1
        FROM information_schema.columns
        WHERE table_name = 'device_heartbeats' AND column_name = 'charge_connected'
    ) THEN
        ALTER TABLE device_heartbeats RENAME COLUMN charge_connected TO bit_3_guess;
    END IF;
END $$;

DO $$
BEGIN
    IF EXISTS (
        SELECT 1
        FROM information_schema.columns
        WHERE table_name = 'device_heartbeats' AND column_name = 'defense_active'
    ) THEN
        ALTER TABLE device_heartbeats RENAME COLUMN defense_active TO bit_4_guess;
    END IF;
END $$;

DO $$
BEGIN
    IF EXISTS (
        SELECT 1
        FROM information_schema.columns
        WHERE table_name = 'device_heartbeats' AND column_name = 'oil_and_electricity_connected'
    ) THEN
        ALTER TABLE device_heartbeats RENAME COLUMN oil_and_electricity_connected TO gps_tracking_on;
    END IF;
END $$;

DO $$
BEGIN
    IF EXISTS (
        SELECT 1
        FROM information_schema.columns
        WHERE table_name = 'devices' AND column_name = 'latest_acc_high'
    ) THEN
        ALTER TABLE devices RENAME COLUMN latest_acc_high TO latest_vibration_detected;
    END IF;
END $$;

DO $$
BEGIN
    IF EXISTS (
        SELECT 1
        FROM information_schema.columns
        WHERE table_name = 'devices' AND column_name = 'latest_gps_tracking_on'
    ) THEN
        ALTER TABLE devices RENAME COLUMN latest_gps_tracking_on TO latest_bit_1_guess;
    END IF;
END $$;

DO $$
BEGIN
    IF EXISTS (
        SELECT 1
        FROM information_schema.columns
        WHERE table_name = 'devices' AND column_name = 'latest_alarm_active'
    ) THEN
        ALTER TABLE devices RENAME COLUMN latest_alarm_active TO latest_acc_high;
    END IF;
END $$;

DO $$
BEGIN
    IF EXISTS (
        SELECT 1
        FROM information_schema.columns
        WHERE table_name = 'devices' AND column_name = 'latest_charge_connected'
    ) THEN
        ALTER TABLE devices RENAME COLUMN latest_charge_connected TO latest_bit_3_guess;
    END IF;
END $$;

DO $$
BEGIN
    IF EXISTS (
        SELECT 1
        FROM information_schema.columns
        WHERE table_name = 'devices' AND column_name = 'latest_defense_active'
    ) THEN
        ALTER TABLE devices RENAME COLUMN latest_defense_active TO latest_bit_4_guess;
    END IF;
END $$;

DO $$
BEGIN
    IF EXISTS (
        SELECT 1
        FROM information_schema.columns
        WHERE table_name = 'devices' AND column_name = 'latest_oil_and_electricity_connected'
    ) THEN
        ALTER TABLE devices RENAME COLUMN latest_oil_and_electricity_connected TO latest_gps_tracking_on;
    END IF;
END $$;
