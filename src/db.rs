use std::time::Duration;

use async_trait::async_trait;
use chrono::{DateTime, NaiveDateTime, Utc};
use sqlx::postgres::{PgPoolOptions, PgQueryResult};
use sqlx::{PgPool, Postgres, Row, Transaction};
use thiserror::Error;
use tracing::{error, warn};

use crate::config::Config;
use crate::events::DeviceEventHandler;
use crate::events::{CompositeEventHandler, DeviceEvent, LoggingEventHandler};
use crate::protocol::{
    decode_terminal_info_flags, format_bytes_hex, resolve_acc_high, resolve_engine_status_guess,
    EngineStatus, GpsTimestamp,
};

pub static MIGRATOR: sqlx::migrate::Migrator = sqlx::migrate!("./migrations");

#[derive(Debug, Error)]
pub enum DatabaseError {
    #[error("database is not configured")]
    NotConfigured,
    #[error("failed to connect to postgres: {0}")]
    Connect(#[source] sqlx::Error),
    #[error("failed to run migrations: {0}")]
    Migrate(#[source] sqlx::migrate::MigrateError),
    #[error("database operation failed: {0}")]
    Query(#[from] sqlx::Error),
}

#[derive(Debug, Clone)]
pub struct Database {
    pool: PgPool,
}

impl Database {
    pub async fn connect(config: &Config) -> Result<Option<Self>, DatabaseError> {
        let Some(database_url) = &config.database_url else {
            return Ok(None);
        };

        let pool = PgPoolOptions::new()
            .max_connections(config.database_max_connections)
            .acquire_timeout(Duration::from_millis(config.database_write_timeout_ms))
            .connect(database_url)
            .await
            .map_err(DatabaseError::Connect)?;

        MIGRATOR.run(&pool).await.map_err(DatabaseError::Migrate)?;

        Ok(Some(Self { pool }))
    }

    pub fn pool(&self) -> &PgPool {
        &self.pool
    }

    pub async fn fetch_previous_acc_high(&self, imei: &str) -> Result<Option<bool>, sqlx::Error> {
        let row = sqlx::query(
            r#"
            SELECT acc_high
            FROM device_heartbeats
            WHERE imei = $1 AND acc_high IS NOT NULL
            ORDER BY id DESC
            LIMIT 1
            "#,
        )
        .bind(imei)
        .fetch_optional(&self.pool)
        .await?;

        Ok(row.and_then(|row| row.try_get::<Option<bool>, _>("acc_high").ok().flatten()))
    }
}

#[derive(Debug, Clone)]
pub struct DatabaseEventHandler {
    database: Database,
}

impl DatabaseEventHandler {
    pub fn new(database: Database) -> Self {
        Self { database }
    }
}

#[async_trait]
impl DeviceEventHandler for DatabaseEventHandler {
    async fn handle_event(&self, event: DeviceEvent) {
        if let Err(error) = persist_event(self.database.pool(), event).await {
            error!(error = %error, "failed to persist device event");
        }
    }
}

pub async fn build_event_handler(
    config: &Config,
) -> Result<std::sync::Arc<dyn DeviceEventHandler>, DatabaseError> {
    if let Some(database) = Database::connect(config).await? {
        let logging = std::sync::Arc::new(LoggingEventHandler::new(Some(database.clone())));
        let database_handler = std::sync::Arc::new(DatabaseEventHandler::new(database));
        let composite = CompositeEventHandler::new(vec![logging, database_handler]);
        Ok(std::sync::Arc::new(composite))
    } else {
        warn!("DATABASE_URL is not set; running without PostgreSQL persistence");
        Ok(std::sync::Arc::new(LoggingEventHandler::new(None)))
    }
}

async fn persist_event(pool: &PgPool, event: DeviceEvent) -> Result<(), sqlx::Error> {
    match event {
        DeviceEvent::Login {
            peer_addr,
            imei,
            serial: _,
        } => {
            let mut tx = pool.begin().await?;
            upsert_device_login(&mut tx, &imei, &peer_addr.to_string(), Utc::now()).await?;
            tx.commit().await?;
        }
        DeviceEvent::Heartbeat {
            peer_addr,
            device_id,
            packet,
        } => {
            let Some(imei) = device_id else {
                warn!(%peer_addr, "skipping heartbeat persistence because device_id is unknown");
                return Ok(());
            };

            let mut tx = pool.begin().await?;
            let server_received_at = Utc::now();
            let device_pk =
                upsert_device_last_seen(&mut tx, &imei, &peer_addr.to_string(), server_received_at)
                    .await?;
            insert_heartbeat(
                &mut tx,
                device_pk,
                &imei,
                &peer_addr.to_string(),
                server_received_at,
                &packet,
            )
            .await?;
            update_device_latest_heartbeat(
                &mut tx,
                device_pk,
                &imei,
                &peer_addr.to_string(),
                server_received_at,
                &packet,
            )
            .await?;
            tx.commit().await?;
        }
        DeviceEvent::Location {
            peer_addr,
            device_id,
            packet,
        } => {
            let Some(imei) = device_id else {
                warn!(%peer_addr, "skipping location persistence because device_id is unknown");
                return Ok(());
            };

            let mut tx = pool.begin().await?;
            let server_received_at = Utc::now();
            let device_pk =
                upsert_device_last_seen(&mut tx, &imei, &peer_addr.to_string(), server_received_at)
                    .await?;
            insert_location(
                &mut tx,
                device_pk,
                &imei,
                &peer_addr.to_string(),
                server_received_at,
                &packet,
            )
            .await?;
            update_device_latest_location(
                &mut tx,
                device_pk,
                &peer_addr.to_string(),
                server_received_at,
                &packet,
            )
            .await?;
            tx.commit().await?;
        }
    }

    Ok(())
}

async fn upsert_device_login(
    tx: &mut Transaction<'_, Postgres>,
    imei: &str,
    peer_addr: &str,
    now: DateTime<Utc>,
) -> Result<i64, sqlx::Error> {
    let row = sqlx::query(
        r#"
        INSERT INTO devices (
            imei, first_seen_at, last_seen_at, last_login_at, latest_peer_addr, created_at, updated_at
        )
        VALUES ($1, $2, $2, $2, $3, $2, $2)
        ON CONFLICT (imei) DO UPDATE
        SET last_seen_at = EXCLUDED.last_seen_at,
            last_login_at = EXCLUDED.last_login_at,
            latest_peer_addr = EXCLUDED.latest_peer_addr,
            updated_at = EXCLUDED.updated_at
        RETURNING id
        "#,
    )
    .bind(imei)
    .bind(now)
    .bind(peer_addr)
    .fetch_one(&mut **tx)
    .await?;

    Ok(row.get("id"))
}

async fn upsert_device_last_seen(
    tx: &mut Transaction<'_, Postgres>,
    imei: &str,
    peer_addr: &str,
    now: DateTime<Utc>,
) -> Result<i64, sqlx::Error> {
    let row = sqlx::query(
        r#"
        INSERT INTO devices (
            imei, first_seen_at, last_seen_at, latest_peer_addr, created_at, updated_at
        )
        VALUES ($1, $2, $2, $3, $2, $2)
        ON CONFLICT (imei) DO UPDATE
        SET last_seen_at = EXCLUDED.last_seen_at,
            latest_peer_addr = EXCLUDED.latest_peer_addr,
            updated_at = EXCLUDED.updated_at
        RETURNING id
        "#,
    )
    .bind(imei)
    .bind(now)
    .bind(peer_addr)
    .fetch_one(&mut **tx)
    .await?;

    Ok(row.get("id"))
}

async fn insert_location(
    tx: &mut Transaction<'_, Postgres>,
    device_pk: i64,
    imei: &str,
    peer_addr: &str,
    server_received_at: DateTime<Utc>,
    packet: &crate::protocol::LocationPacket,
) -> Result<PgQueryResult, sqlx::Error> {
    sqlx::query(
        r#"
        INSERT INTO device_locations (
            device_id, imei, server_received_at, gps_timestamp, protocol_number, packet_family,
            latitude, longitude, speed_kph, course, course_status, satellite_count, gps_info_length,
            extra_data_hex, peer_addr
        )
        VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15)
        "#,
    )
    .bind(device_pk)
    .bind(imei)
    .bind(server_received_at)
    .bind(gps_timestamp_to_naive(&packet.timestamp))
    .bind(extended_protocol_number(packet))
    .bind(packet_family(packet))
    .bind(packet.latitude)
    .bind(packet.longitude)
    .bind(i32::from(packet.speed_kph))
    .bind(i32::from(packet.course))
    .bind(i32::from(packet.course_status))
    .bind(i32::from(packet.satellite_count))
    .bind(i32::from(packet.gps_info_length))
    .bind(format_bytes_hex(&packet.extra_data))
    .bind(peer_addr)
    .execute(&mut **tx)
    .await
}

async fn update_device_latest_location(
    tx: &mut Transaction<'_, Postgres>,
    device_pk: i64,
    peer_addr: &str,
    server_received_at: DateTime<Utc>,
    packet: &crate::protocol::LocationPacket,
) -> Result<PgQueryResult, sqlx::Error> {
    sqlx::query(
        r#"
        UPDATE devices
        SET last_seen_at = $2,
            last_location_at = $2,
            latest_peer_addr = $3,
            latest_protocol_number = $4,
            latest_packet_family = $5,
            latest_gps_timestamp = $6,
            latest_latitude = $7,
            latest_longitude = $8,
            latest_speed_kph = $9,
            latest_course = $10,
            latest_course_status = $11,
            latest_satellite_count = $12,
            latest_gps_info_length = $13,
            latest_location_extra_data_hex = $14,
            updated_at = $2
        WHERE id = $1
        "#,
    )
    .bind(device_pk)
    .bind(server_received_at)
    .bind(peer_addr)
    .bind(extended_protocol_number(packet))
    .bind(packet_family(packet))
    .bind(gps_timestamp_to_naive(&packet.timestamp))
    .bind(packet.latitude)
    .bind(packet.longitude)
    .bind(i32::from(packet.speed_kph))
    .bind(i32::from(packet.course))
    .bind(i32::from(packet.course_status))
    .bind(i32::from(packet.satellite_count))
    .bind(i32::from(packet.gps_info_length))
    .bind(format_bytes_hex(&packet.extra_data))
    .execute(&mut **tx)
    .await
}

async fn insert_heartbeat(
    tx: &mut Transaction<'_, Postgres>,
    device_pk: i64,
    imei: &str,
    peer_addr: &str,
    server_received_at: DateTime<Utc>,
    packet: &crate::protocol::HeartbeatPacket,
) -> Result<PgQueryResult, sqlx::Error> {
    let flags = decode_terminal_info_flags(packet.terminal_info);
    let effective_acc_high =
        resolve_acc_high(flags.acc_high, fetch_previous_acc_high(tx, imei).await?);
    let engine_status_guess = resolve_engine_status_guess(effective_acc_high);

    sqlx::query(
        r#"
        INSERT INTO device_heartbeats (
            device_id, imei, server_received_at, protocol_number, peer_addr, terminal_info_raw,
            terminal_info_bits, gps_tracking_on, bit_1_guess, acc_high, bit_3_guess,
            vibration_detected, bit_4_guess, engine_status_guess, voltage_level,
            gsm_signal_strength, alarm_language
        )
        VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15, $16, $17)
        "#,
    )
    .bind(device_pk)
    .bind(imei)
    .bind(server_received_at)
    .bind(i32::from(crate::protocol::PROTOCOL_HEARTBEAT))
    .bind(peer_addr)
    .bind(i32::from(packet.terminal_info))
    .bind(flags.binary)
    .bind(flags.gps_tracking_on)
    .bind(flags.bit_1_guess)
    .bind(effective_acc_high)
    .bind(flags.bit_3_guess)
    .bind(flags.vibration_detected)
    .bind(flags.bit_4_guess)
    .bind(engine_status_as_str(engine_status_guess))
    .bind(i32::from(packet.voltage_level))
    .bind(i32::from(packet.gsm_signal_strength))
    .bind(i32::from(packet.alarm_language))
    .execute(&mut **tx)
    .await
}

async fn update_device_latest_heartbeat(
    tx: &mut Transaction<'_, Postgres>,
    device_pk: i64,
    imei: &str,
    peer_addr: &str,
    server_received_at: DateTime<Utc>,
    packet: &crate::protocol::HeartbeatPacket,
) -> Result<PgQueryResult, sqlx::Error> {
    let flags = decode_terminal_info_flags(packet.terminal_info);
    let effective_acc_high =
        resolve_acc_high(flags.acc_high, fetch_previous_acc_high(tx, imei).await?);
    let engine_status_guess = resolve_engine_status_guess(effective_acc_high);

    sqlx::query(
        r#"
        UPDATE devices
        SET last_seen_at = $2,
            last_heartbeat_at = $2,
            latest_peer_addr = $3,
            latest_protocol_number = $4,
            latest_packet_family = $5,
            latest_terminal_info_raw = $6,
            latest_terminal_info_bits = $7,
            latest_gps_tracking_on = $8,
            latest_bit_1_guess = $9,
            latest_acc_high = $10,
            latest_bit_3_guess = $11,
            latest_vibration_detected = $12,
            latest_bit_4_guess = $13,
            latest_engine_status_guess = $14,
            latest_voltage_level = $15,
            latest_gsm_signal_strength = $16,
            latest_alarm_language = $17,
            updated_at = $2
        WHERE id = $1
        "#,
    )
    .bind(device_pk)
    .bind(server_received_at)
    .bind(peer_addr)
    .bind(i32::from(crate::protocol::PROTOCOL_HEARTBEAT))
    .bind("heartbeat")
    .bind(i32::from(packet.terminal_info))
    .bind(flags.binary)
    .bind(flags.gps_tracking_on)
    .bind(flags.bit_1_guess)
    .bind(effective_acc_high)
    .bind(flags.bit_3_guess)
    .bind(flags.vibration_detected)
    .bind(flags.bit_4_guess)
    .bind(engine_status_as_str(engine_status_guess))
    .bind(i32::from(packet.voltage_level))
    .bind(i32::from(packet.gsm_signal_strength))
    .bind(i32::from(packet.alarm_language))
    .execute(&mut **tx)
    .await
}

pub async fn fetch_previous_acc_high(
    tx: &mut Transaction<'_, Postgres>,
    imei: &str,
) -> Result<Option<bool>, sqlx::Error> {
    let row = sqlx::query(
        r#"
        SELECT acc_high
        FROM device_heartbeats
        WHERE imei = $1 AND acc_high IS NOT NULL
        ORDER BY id DESC
        LIMIT 1
        "#,
    )
    .bind(imei)
    .fetch_optional(&mut **tx)
    .await?;

    Ok(row.and_then(|row| row.try_get::<Option<bool>, _>("acc_high").ok().flatten()))
}

fn packet_family(packet: &crate::protocol::LocationPacket) -> &'static str {
    if packet.extra_data.is_empty() {
        "location"
    } else {
        "extended_location"
    }
}

fn extended_protocol_number(packet: &crate::protocol::LocationPacket) -> i32 {
    if packet.extra_data.is_empty() {
        i32::from(crate::protocol::PROTOCOL_LOCATION)
    } else {
        i32::from(crate::protocol::PROTOCOL_EXTENDED_LOCATION)
    }
}

fn engine_status_as_str(status: EngineStatus) -> &'static str {
    match status {
        EngineStatus::On => "on",
        EngineStatus::Off => "off",
        EngineStatus::Unknown => "unknown",
    }
}

pub fn gps_timestamp_to_naive(timestamp: &GpsTimestamp) -> NaiveDateTime {
    chrono::NaiveDate::from_ymd_opt(
        i32::from(timestamp.year),
        u32::from(timestamp.month),
        u32::from(timestamp.day),
    )
    .and_then(|date| {
        date.and_hms_opt(
            u32::from(timestamp.hour),
            u32::from(timestamp.minute),
            u32::from(timestamp.second),
        )
    })
    .unwrap_or_else(|| Utc::now().naive_utc())
}

#[cfg(test)]
mod tests {
    use std::env;

    use chrono::{DateTime, Datelike, Timelike, Utc};
    use sqlx::Row;

    use super::{engine_status_as_str, gps_timestamp_to_naive, Database, MIGRATOR};
    use crate::config::Config;
    use crate::events::DeviceEvent;
    use crate::protocol::{EngineStatus, GpsTimestamp, HeartbeatPacket, LocationPacket};

    fn database_url() -> Option<String> {
        env::var("GT06_TEST_DATABASE_URL").ok()
    }

    #[test]
    fn converts_gps_timestamp_to_naive_datetime() {
        let naive = gps_timestamp_to_naive(&GpsTimestamp {
            year: 2026,
            month: 4,
            day: 12,
            hour: 14,
            minute: 5,
            second: 9,
        });

        assert_eq!(naive.year(), 2026);
        assert_eq!(naive.month(), 4);
        assert_eq!(naive.day(), 12);
        assert_eq!(naive.hour(), 14);
        assert_eq!(naive.minute(), 5);
        assert_eq!(naive.second(), 9);
    }

    #[test]
    fn maps_engine_status_to_storage_value() {
        assert_eq!(engine_status_as_str(EngineStatus::On), "on");
        assert_eq!(engine_status_as_str(EngineStatus::Off), "off");
        assert_eq!(engine_status_as_str(EngineStatus::Unknown), "unknown");
    }

    #[tokio::test]
    async fn migration_bootstrap_succeeds_on_clean_database(
    ) -> Result<(), Box<dyn std::error::Error>> {
        let Some(database_url) = database_url() else {
            return Ok(());
        };

        let config = Config::from_pairs([
            ("DATABASE_URL", database_url.as_str()),
            ("DATABASE_MAX_CONNECTIONS", "1"),
        ]);
        let database = Database::connect(&config)
            .await?
            .expect("database should be configured");

        MIGRATOR.run(database.pool()).await?;
        let row = sqlx::query(
            "SELECT to_regclass('public.devices') AS devices, to_regclass('public.device_locations') AS locations, to_regclass('public.device_heartbeats') AS heartbeats, to_regclass('public.telegram_subscriptions') AS telegram_subscriptions, to_regclass('public.telegram_payment_events') AS telegram_payment_events"
        )
        .fetch_one(database.pool())
        .await?;

        assert_eq!(
            row.try_get::<Option<String>, _>("devices")?,
            Some("devices".to_string())
        );
        assert_eq!(
            row.try_get::<Option<String>, _>("locations")?,
            Some("device_locations".to_string())
        );
        assert_eq!(
            row.try_get::<Option<String>, _>("heartbeats")?,
            Some("device_heartbeats".to_string())
        );
        assert_eq!(
            row.try_get::<Option<String>, _>("telegram_subscriptions")?,
            Some("telegram_subscriptions".to_string())
        );
        assert_eq!(
            row.try_get::<Option<String>, _>("telegram_payment_events")?,
            Some("telegram_payment_events".to_string())
        );

        Ok(())
    }

    #[tokio::test]
    async fn stores_midtrans_subscription_payment_state() -> Result<(), Box<dyn std::error::Error>>
    {
        let Some(database_url) = database_url() else {
            return Ok(());
        };

        let config = Config::from_pairs([
            ("DATABASE_URL", database_url.as_str()),
            ("DATABASE_MAX_CONNECTIONS", "1"),
        ]);
        let database = Database::connect(&config)
            .await?
            .expect("database should be configured");

        let telegram_user_id = 9_990_000_001_i64;
        let chat_id = 9_990_000_002_i64;
        let period_start = Utc::now();
        let period_end = period_start + chrono::Duration::days(30);
        let order_id = format!("hb-{telegram_user_id}-{}", period_start.timestamp_millis());
        let transaction_id = format!("midtrans-transaction-{telegram_user_id}");

        sqlx::query(
            r#"
            INSERT INTO telegram_users (
                telegram_user_id, chat_id, bound_imei, registration_status, created_at, updated_at
            )
            VALUES ($1, $2, NULL, 'bound', NOW(), NOW())
            ON CONFLICT (telegram_user_id) DO UPDATE
            SET chat_id = EXCLUDED.chat_id,
                registration_status = EXCLUDED.registration_status,
                updated_at = EXCLUDED.updated_at
            "#,
        )
        .bind(telegram_user_id)
        .bind(chat_id)
        .execute(database.pool())
        .await?;

        let subscription_id: i64 = sqlx::query_scalar(
            r#"
            INSERT INTO telegram_subscriptions (
                telegram_user_id, chat_id, plan_code, status,
                current_period_start_at, current_period_end_at, created_at, updated_at
            )
            VALUES ($1, $2, 'monthly_stars', 'active', $3, $4, NOW(), NOW())
            ON CONFLICT (telegram_user_id, plan_code) DO UPDATE
            SET chat_id = EXCLUDED.chat_id,
                status = EXCLUDED.status,
                current_period_start_at = EXCLUDED.current_period_start_at,
                current_period_end_at = EXCLUDED.current_period_end_at,
                updated_at = EXCLUDED.updated_at
            RETURNING id
            "#,
        )
        .bind(telegram_user_id)
        .bind(chat_id)
        .bind(period_start)
        .bind(period_end)
        .fetch_one(database.pool())
        .await?;

        sqlx::query(
            "DELETE FROM telegram_payment_events WHERE provider_order_id = $1 OR provider_transaction_id = $2",
        )
        .bind(&order_id)
        .bind(&transaction_id)
        .execute(database.pool())
        .await?;

        sqlx::query(
            r#"
            INSERT INTO telegram_payment_events (
                telegram_user_id, chat_id, subscription_id, payment_provider, payment_kind,
                payment_status, plan_code, currency, gross_amount_idr, period_days,
                provider_order_id, provider_transaction_id, payment_type, paid_at,
                raw_webhook_notification, created_at, updated_at
            )
            VALUES (
                $1, $2, $3, 'midtrans', 'snap_subscription',
                'paid', 'monthly_stars', 'IDR', 2000, 30,
                $4, $5, 'qris', $6, $7::jsonb, NOW(), NOW()
            )
            "#,
        )
        .bind(telegram_user_id)
        .bind(chat_id)
        .bind(subscription_id)
        .bind(&order_id)
        .bind(&transaction_id)
        .bind(period_start)
        .bind(r#"{"transaction_status":"settlement","gross_amount":"2000.00"}"#)
        .execute(database.pool())
        .await?;

        let duplicate_payment = sqlx::query(
            r#"
            INSERT INTO telegram_payment_events (
                telegram_user_id, chat_id, subscription_id, payment_provider, payment_kind,
                payment_status, gross_amount_idr, provider_order_id, provider_transaction_id
            )
            VALUES ($1, $2, $3, 'midtrans', 'snap_subscription', 'paid', 2000, $4, $5)
            "#,
        )
        .bind(telegram_user_id)
        .bind(chat_id)
        .bind(subscription_id)
        .bind(format!("{order_id}-duplicate"))
        .bind(&transaction_id)
        .execute(database.pool())
        .await;
        assert!(duplicate_payment.is_err());

        let row = sqlx::query(
            r#"
            SELECT current_period_start_at, current_period_end_at
            FROM telegram_subscriptions
            WHERE id = $1
            "#,
        )
        .bind(subscription_id)
        .fetch_one(database.pool())
        .await?;
        let restored_start: DateTime<Utc> = row.get("current_period_start_at");
        let restored_end: DateTime<Utc> = row.get("current_period_end_at");
        assert_eq!(
            restored_end
                .signed_duration_since(restored_start)
                .num_days(),
            30
        );

        Ok(())
    }

    #[tokio::test]
    async fn persists_login_heartbeat_and_location_history(
    ) -> Result<(), Box<dyn std::error::Error>> {
        let Some(database_url) = database_url() else {
            return Ok(());
        };

        let config = Config::from_pairs([
            ("DATABASE_URL", database_url.as_str()),
            ("DATABASE_MAX_CONNECTIONS", "1"),
        ]);
        let database = Database::connect(&config)
            .await?
            .expect("database should be configured");
        sqlx::query(
            "TRUNCATE device_heartbeats, device_locations, devices RESTART IDENTITY CASCADE",
        )
        .execute(database.pool())
        .await?;

        super::persist_event(
            database.pool(),
            DeviceEvent::Login {
                peer_addr: "127.0.0.1:5000".parse()?,
                imei: "866221070478388".to_string(),
                serial: 1,
            },
        )
        .await?;

        super::persist_event(
            database.pool(),
            DeviceEvent::Heartbeat {
                peer_addr: "127.0.0.1:5000".parse()?,
                device_id: Some("866221070478388".to_string()),
                packet: HeartbeatPacket {
                    terminal_info: 69,
                    voltage_level: 6,
                    gsm_signal_strength: 3,
                    alarm_language: 2,
                },
            },
        )
        .await?;

        super::persist_event(
            database.pool(),
            DeviceEvent::Location {
                peer_addr: "127.0.0.1:5000".parse()?,
                device_id: Some("866221070478388".to_string()),
                packet: LocationPacket {
                    timestamp: GpsTimestamp {
                        year: 2026,
                        month: 4,
                        day: 12,
                        hour: 18,
                        minute: 30,
                        second: 1,
                    },
                    gps_info_length: 12,
                    satellite_count: 8,
                    latitude: -6.204_066_6,
                    longitude: 106.785_514_4,
                    speed_kph: 8,
                    course: 174,
                    course_status: 4270,
                    extra_data: vec![0x01, 0xFE, 0x0A],
                },
            },
        )
        .await?;

        let devices_count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM devices")
            .fetch_one(database.pool())
            .await?;
        let heartbeat_count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM device_heartbeats")
            .fetch_one(database.pool())
            .await?;
        let location_count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM device_locations")
            .fetch_one(database.pool())
            .await?;

        assert_eq!(devices_count, 1);
        assert_eq!(heartbeat_count, 1);
        assert_eq!(location_count, 1);

        let device_row = sqlx::query(
            "SELECT imei, latest_engine_status_guess, latest_latitude, latest_longitude, latest_terminal_info_raw, latest_acc_high, latest_vibration_detected FROM devices WHERE imei = $1"
        )
        .bind("866221070478388")
        .fetch_one(database.pool())
        .await?;

        assert_eq!(device_row.get::<String, _>("imei"), "866221070478388");
        assert_eq!(
            device_row.get::<String, _>("latest_engine_status_guess"),
            "on"
        );
        assert!(device_row.get::<bool, _>("latest_acc_high"));
        assert!(device_row.get::<bool, _>("latest_vibration_detected"));
        assert!((device_row.get::<f64, _>("latest_latitude") - (-6.204_066_6)).abs() < 0.001);
        assert!((device_row.get::<f64, _>("latest_longitude") - 106.785_514_4).abs() < 0.001);
        assert_eq!(device_row.get::<i32, _>("latest_terminal_info_raw"), 69);

        Ok(())
    }
}
