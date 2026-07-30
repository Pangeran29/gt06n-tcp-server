CREATE TABLE IF NOT EXISTS fuel_calibrations (
    id BIGSERIAL PRIMARY KEY,
    imei TEXT NOT NULL REFERENCES devices(imei) ON DELETE CASCADE,
    status TEXT NOT NULL DEFAULT 'active',
    started_at TIMESTAMPTZ NOT NULL,
    start_distance_meters DOUBLE PRECISION NOT NULL,
    completed_at TIMESTAMPTZ,
    end_distance_meters DOUBLE PRECISION,
    distance_meters DOUBLE PRECISION,
    liters DOUBLE PRECISION,
    total_cost_idr BIGINT,
    fuel_type TEXT,
    riding_seconds BIGINT,
    engine_on_seconds BIGINT,
    trip_count BIGINT,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    CONSTRAINT fuel_calibrations_status_valid
        CHECK (status IN ('active', 'completed', 'invalidated')),
    CONSTRAINT fuel_calibrations_start_distance_non_negative
        CHECK (start_distance_meters >= 0),
    CONSTRAINT fuel_calibrations_end_distance_non_negative
        CHECK (end_distance_meters IS NULL OR end_distance_meters >= 0),
    CONSTRAINT fuel_calibrations_distance_non_negative
        CHECK (distance_meters IS NULL OR distance_meters >= 0),
    CONSTRAINT fuel_calibrations_liters_positive
        CHECK (liters IS NULL OR liters > 0),
    CONSTRAINT fuel_calibrations_cost_non_negative
        CHECK (total_cost_idr IS NULL OR total_cost_idr >= 0),
    CONSTRAINT fuel_calibrations_metrics_non_negative
        CHECK (
            (riding_seconds IS NULL OR riding_seconds >= 0)
            AND (engine_on_seconds IS NULL OR engine_on_seconds >= 0)
            AND (trip_count IS NULL OR trip_count >= 0)
        ),
    CONSTRAINT fuel_calibrations_completed_fields
        CHECK (
            status <> 'completed'
            OR (
                completed_at IS NOT NULL
                AND end_distance_meters IS NOT NULL
                AND distance_meters IS NOT NULL
                AND liters IS NOT NULL
                AND riding_seconds IS NOT NULL
                AND engine_on_seconds IS NOT NULL
                AND trip_count IS NOT NULL
            )
        )
);

CREATE UNIQUE INDEX IF NOT EXISTS idx_fuel_calibrations_one_active_per_device
    ON fuel_calibrations (imei)
    WHERE status = 'active';

CREATE INDEX IF NOT EXISTS idx_fuel_calibrations_imei_completed_desc
    ON fuel_calibrations (imei, completed_at DESC, id DESC)
    WHERE status = 'completed';
