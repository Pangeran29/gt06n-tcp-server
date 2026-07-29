CREATE TABLE IF NOT EXISTS device_distance_odometer (
    imei TEXT PRIMARY KEY REFERENCES devices(imei) ON DELETE CASCADE,
    total_distance_meters DOUBLE PRECISION NOT NULL DEFAULT 0,
    last_processed_location_id BIGINT REFERENCES device_locations(id) ON DELETE SET NULL,
    last_server_received_at TIMESTAMPTZ,
    last_latitude DOUBLE PRECISION,
    last_longitude DOUBLE PRECISION,
    last_speed_kph INTEGER,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    CONSTRAINT device_distance_odometer_total_non_negative
        CHECK (total_distance_meters >= 0)
);

CREATE TABLE IF NOT EXISTS device_distance_daily (
    imei TEXT NOT NULL REFERENCES devices(imei) ON DELETE CASCADE,
    distance_date DATE NOT NULL,
    distance_meters DOUBLE PRECISION NOT NULL DEFAULT 0,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    PRIMARY KEY (imei, distance_date),
    CONSTRAINT device_distance_daily_total_non_negative
        CHECK (distance_meters >= 0)
);

CREATE INDEX IF NOT EXISTS idx_device_distance_daily_imei_date_desc
    ON device_distance_daily (imei, distance_date DESC);
