CREATE TABLE IF NOT EXISTS devices (
    id BIGSERIAL PRIMARY KEY,
    imei TEXT NOT NULL UNIQUE,
    first_seen_at TIMESTAMPTZ NOT NULL,
    last_seen_at TIMESTAMPTZ NOT NULL,
    last_login_at TIMESTAMPTZ,
    last_heartbeat_at TIMESTAMPTZ,
    last_location_at TIMESTAMPTZ,
    latest_protocol_number INTEGER,
    latest_packet_family TEXT,
    latest_gps_timestamp TIMESTAMP,
    latest_latitude DOUBLE PRECISION,
    latest_longitude DOUBLE PRECISION,
    latest_speed_kph INTEGER,
    latest_course INTEGER,
    latest_course_status INTEGER,
    latest_satellite_count INTEGER,
    latest_gps_info_length INTEGER,
    latest_location_extra_data_hex TEXT,
    latest_terminal_info_raw INTEGER,
    latest_terminal_info_bits TEXT,
    latest_oil_and_electricity_connected BOOLEAN,
    latest_gps_tracking_on BOOLEAN,
    latest_alarm_active BOOLEAN,
    latest_charge_connected BOOLEAN,
    latest_acc_high BOOLEAN,
    latest_defense_active BOOLEAN,
    latest_engine_status_guess TEXT,
    latest_voltage_level INTEGER,
    latest_gsm_signal_strength INTEGER,
    latest_alarm_language INTEGER,
    latest_peer_addr TEXT,
    created_at TIMESTAMPTZ NOT NULL,
    updated_at TIMESTAMPTZ NOT NULL
);

CREATE TABLE IF NOT EXISTS device_locations (
    id BIGSERIAL PRIMARY KEY,
    device_id BIGINT NOT NULL REFERENCES devices(id) ON DELETE CASCADE,
    imei TEXT NOT NULL,
    server_received_at TIMESTAMPTZ NOT NULL,
    gps_timestamp TIMESTAMP NOT NULL,
    protocol_number INTEGER NOT NULL,
    packet_family TEXT NOT NULL,
    latitude DOUBLE PRECISION NOT NULL,
    longitude DOUBLE PRECISION NOT NULL,
    speed_kph INTEGER NOT NULL,
    course INTEGER NOT NULL,
    course_status INTEGER NOT NULL,
    satellite_count INTEGER NOT NULL,
    gps_info_length INTEGER NOT NULL,
    extra_data_hex TEXT NOT NULL,
    peer_addr TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS device_heartbeats (
    id BIGSERIAL PRIMARY KEY,
    device_id BIGINT NOT NULL REFERENCES devices(id) ON DELETE CASCADE,
    imei TEXT NOT NULL,
    server_received_at TIMESTAMPTZ NOT NULL,
    protocol_number INTEGER NOT NULL,
    peer_addr TEXT NOT NULL,
    terminal_info_raw INTEGER NOT NULL,
    terminal_info_bits TEXT NOT NULL,
    oil_and_electricity_connected BOOLEAN NOT NULL,
    gps_tracking_on BOOLEAN NOT NULL,
    alarm_active BOOLEAN NOT NULL,
    charge_connected BOOLEAN NOT NULL,
    acc_high BOOLEAN,
    defense_active BOOLEAN NOT NULL,
    engine_status_guess TEXT NOT NULL,
    voltage_level INTEGER NOT NULL,
    gsm_signal_strength INTEGER NOT NULL,
    alarm_language INTEGER NOT NULL
);

CREATE INDEX IF NOT EXISTS idx_devices_last_seen_at ON devices (last_seen_at DESC);
CREATE INDEX IF NOT EXISTS idx_device_locations_device_id_server_received_at ON device_locations (device_id, server_received_at DESC);
CREATE INDEX IF NOT EXISTS idx_device_locations_imei_server_received_at ON device_locations (imei, server_received_at DESC);
CREATE INDEX IF NOT EXISTS idx_device_heartbeats_device_id_server_received_at ON device_heartbeats (device_id, server_received_at DESC);
CREATE INDEX IF NOT EXISTS idx_device_heartbeats_imei_server_received_at ON device_heartbeats (imei, server_received_at DESC);
