use std::time::Duration;

use chrono::{DateTime, NaiveDateTime, Utc};
use reqwest::Client;
use serde::{Deserialize, Serialize};
use sqlx::Row;
use thiserror::Error;
use tokio::time::sleep;
use tracing::{error, info, warn};

use crate::config::Config;
use crate::db::Database;

#[derive(Debug, Error)]
pub enum BotError {
    #[error("telegram bot token is not configured")]
    MissingToken,
    #[error("database is not configured")]
    MissingDatabase,
    #[error("failed to connect to database: {0}")]
    Database(#[from] crate::db::DatabaseError),
    #[error("database query failed: {0}")]
    Query(#[from] sqlx::Error),
    #[error("telegram api request failed: {0}")]
    Http(#[from] reqwest::Error),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BotCommand {
    Start,
    Help,
    BindMe,
    LastHeartbeat,
    LatestLocation,
    Unknown(String),
}

impl BotCommand {
    pub fn parse(text: &str) -> Option<Self> {
        let command = text.split_whitespace().next()?;
        let normalized = command.split('@').next().unwrap_or(command);

        Some(match normalized {
            "/start" => Self::Start,
            "/help" => Self::Help,
            "/bind_me" => Self::BindMe,
            "/last_heartbeat" => Self::LastHeartbeat,
            "/latest_location" => Self::LatestLocation,
            other if other.starts_with('/') => Self::Unknown(other.to_string()),
            _ => return None,
        })
    }
}

#[derive(Debug, Clone)]
pub struct TelegramBot {
    database: Database,
    client: Client,
    base_url: String,
    poll_timeout_secs: u64,
    heartbeat_poll_interval_ms: u64,
    preconfigured_admin_chat_id: Option<i64>,
}

impl TelegramBot {
    pub async fn from_config(config: &Config) -> Result<Self, BotError> {
        let database = Database::connect(config).await?.ok_or(BotError::MissingDatabase)?;
        let token = config
            .telegram_bot_token
            .as_ref()
            .ok_or(BotError::MissingToken)?;

        Ok(Self {
            database,
            client: Client::new(),
            base_url: format!("https://api.telegram.org/bot{token}"),
            poll_timeout_secs: config.telegram_poll_timeout_secs,
            heartbeat_poll_interval_ms: config.telegram_heartbeat_poll_interval_ms,
            preconfigured_admin_chat_id: config.telegram_admin_chat_id,
        })
    }

    pub async fn run(&self) -> Result<(), BotError> {
        info!("telegram bot started");

        if let Some(chat_id) = self.preconfigured_admin_chat_id {
            ensure_admin_chat_id(self.database.pool(), chat_id).await?;
        }

        loop {
            if let Err(error) = self.process_updates().await {
                error!(error = %error, "telegram update polling failed");
            }

            if let Err(error) = self.process_heartbeat_notifications().await {
                error!(error = %error, "heartbeat notification polling failed");
            }

            sleep(Duration::from_millis(self.heartbeat_poll_interval_ms)).await;
        }
    }

    async fn process_updates(&self) -> Result<(), BotError> {
        let offset = get_state_i64(self.database.pool(), "last_telegram_update_id")
            .await?
            .map(|value| value + 1);

        let updates = self.get_updates(offset).await?;

        for update in updates {
            if let Some(message) = update.message {
                self.handle_message(message).await?;
            }

            set_state_i64(
                self.database.pool(),
                "last_telegram_update_id",
                i64::from(update.update_id),
            )
            .await?;
        }

        Ok(())
    }

    async fn process_heartbeat_notifications(&self) -> Result<(), BotError> {
        let admin_chat_id = if let Some(chat_id) = self.preconfigured_admin_chat_id {
            Some(chat_id)
        } else {
            get_state_i64(self.database.pool(), "admin_chat_id").await?
        };

        let Some(admin_chat_id) = admin_chat_id else {
            warn!("telegram admin chat is not configured; heartbeat notifications are paused");
            return Ok(());
        };

        let last_notified = get_state_i64(self.database.pool(), "last_notified_heartbeat_id")
            .await?
            .unwrap_or(0);

        let heartbeats = fetch_new_heartbeats(self.database.pool(), last_notified).await?;

        for heartbeat in heartbeats {
            let text = format_heartbeat_notification(&heartbeat);
            self.send_message(admin_chat_id, &text).await?;
            set_state_i64(
                self.database.pool(),
                "last_notified_heartbeat_id",
                heartbeat.id,
            )
            .await?;
        }

        Ok(())
    }

    async fn handle_message(&self, message: TelegramMessage) -> Result<(), BotError> {
        let Some(text) = message.text.as_deref() else {
            return Ok(());
        };

        let Some(command) = BotCommand::parse(text) else {
            return Ok(());
        };

        let chat_id = message.chat.id;

        match command {
            BotCommand::Start => {
                self.send_message(
                    chat_id,
                    "GT06 bot is online. Use /help to see available commands.",
                )
                .await?;
            }
            BotCommand::Help => {
                self.send_message(chat_id, HELP_TEXT).await?;
            }
            BotCommand::BindMe => {
                ensure_admin_chat_id(self.database.pool(), chat_id).await?;
                self.send_message(
                    chat_id,
                    "This chat is now bound as the heartbeat notification target.",
                )
                .await?;
            }
            BotCommand::LastHeartbeat => {
                let response = if let Some(heartbeat) =
                    fetch_latest_heartbeat(self.database.pool()).await?
                {
                    format_heartbeat_notification(&heartbeat)
                } else {
                    "No heartbeat data is stored yet.".to_string()
                };

                self.send_message(chat_id, &response).await?;
            }
            BotCommand::LatestLocation => {
                let response = if let Some(location) =
                    fetch_latest_location(self.database.pool()).await?
                {
                    format_latest_location_message(&location)
                } else {
                    "No location data is stored yet.".to_string()
                };

                self.send_message(chat_id, &response).await?;
            }
            BotCommand::Unknown(command) => {
                self.send_message(
                    chat_id,
                    &format!("Unknown command: {command}. Use /help to see available commands."),
                )
                .await?;
            }
        }

        Ok(())
    }

    async fn get_updates(
        &self,
        offset: Option<i64>,
    ) -> Result<Vec<TelegramUpdate>, reqwest::Error> {
        let request = GetUpdatesRequest {
            offset,
            timeout: Some(self.poll_timeout_secs),
        };

        let response = self
            .client
            .post(format!("{}/getUpdates", self.base_url))
            .json(&request)
            .send()
            .await?
            .error_for_status()?;

        let body: TelegramResponse<Vec<TelegramUpdate>> = response.json().await?;
        Ok(body.result)
    }

    async fn send_message(&self, chat_id: i64, text: &str) -> Result<(), reqwest::Error> {
        let request = SendMessageRequest {
            chat_id,
            text: text.to_string(),
        };

        let response = self
            .client
            .post(format!("{}/sendMessage", self.base_url))
            .json(&request)
            .send()
            .await?
            .error_for_status()?;

        let _: TelegramResponse<TelegramMessage> = response.json().await?;
        Ok(())
    }
}

pub const HELP_TEXT: &str = "/start - confirm the bot is online\n/help - show available commands\n/bind_me - make this chat the heartbeat notification target\n/last_heartbeat - show the most recent stored heartbeat\n/latest_location - show the latest stored device location";

#[derive(Debug, Deserialize)]
struct TelegramResponse<T> {
    result: T,
}

#[derive(Debug, Deserialize)]
struct TelegramUpdate {
    update_id: i32,
    message: Option<TelegramMessage>,
}

#[derive(Debug, Deserialize)]
struct TelegramMessage {
    chat: TelegramChat,
    text: Option<String>,
}

#[derive(Debug, Deserialize)]
struct TelegramChat {
    id: i64,
}

#[derive(Debug, Serialize)]
struct GetUpdatesRequest {
    #[serde(skip_serializing_if = "Option::is_none")]
    offset: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    timeout: Option<u64>,
}

#[derive(Debug, Serialize)]
struct SendMessageRequest {
    chat_id: i64,
    text: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct StoredHeartbeat {
    pub id: i64,
    pub imei: String,
    pub server_received_at: DateTime<Utc>,
    pub terminal_info_raw: i32,
    pub terminal_info_bits: String,
    pub engine_status_guess: String,
    pub voltage_level: i32,
    pub gsm_signal_strength: i32,
}

#[derive(Debug, Clone, PartialEq)]
pub struct StoredLocation {
    pub imei: String,
    pub last_seen_at: Option<DateTime<Utc>>,
    pub gps_timestamp: Option<NaiveDateTime>,
    pub latitude: Option<f64>,
    pub longitude: Option<f64>,
    pub speed_kph: Option<i32>,
    pub course: Option<i32>,
    pub satellite_count: Option<i32>,
}

pub fn format_heartbeat_notification(heartbeat: &StoredHeartbeat) -> String {
    format!(
        "Heartbeat update\nIMEI: {}\nServer time: {}\nEngine status: {} (heuristic)\nTerminal info: {} ({})\nVoltage level: {}\nGSM signal: {}",
        heartbeat.imei,
        heartbeat.server_received_at.format("%Y-%m-%d %H:%M:%S UTC"),
        heartbeat.engine_status_guess,
        heartbeat.terminal_info_raw,
        heartbeat.terminal_info_bits,
        heartbeat.voltage_level,
        heartbeat.gsm_signal_strength
    )
}

pub fn format_latest_location_message(location: &StoredLocation) -> String {
    let gps_timestamp = location
        .gps_timestamp
        .map(|value| value.format("%Y-%m-%d %H:%M:%S").to_string())
        .unwrap_or_else(|| "unknown".to_string());
    let last_seen_at = location
        .last_seen_at
        .map(|value| value.format("%Y-%m-%d %H:%M:%S UTC").to_string())
        .unwrap_or_else(|| "unknown".to_string());

    format!(
        "Latest location\nIMEI: {}\nGPS time: {}\nServer last seen: {}\nLatitude: {}\nLongitude: {}\nSpeed: {} km/h\nCourse: {}°\nSatellites: {}",
        location.imei,
        gps_timestamp,
        last_seen_at,
        option_f64(location.latitude),
        option_f64(location.longitude),
        option_i32(location.speed_kph),
        option_i32(location.course),
        option_i32(location.satellite_count)
    )
}

fn option_f64(value: Option<f64>) -> String {
    value
        .map(|v| format!("{v:.6}"))
        .unwrap_or_else(|| "unknown".to_string())
}

fn option_i32(value: Option<i32>) -> String {
    value
        .map(|v| v.to_string())
        .unwrap_or_else(|| "unknown".to_string())
}

pub async fn ensure_admin_chat_id(pool: &sqlx::PgPool, chat_id: i64) -> Result<(), sqlx::Error> {
    set_state_i64(pool, "admin_chat_id", chat_id).await
}

pub async fn get_state_i64(pool: &sqlx::PgPool, key: &str) -> Result<Option<i64>, sqlx::Error> {
    let row = sqlx::query("SELECT state_value FROM telegram_bot_state WHERE state_key = $1")
        .bind(key)
        .fetch_optional(pool)
        .await?;

    Ok(row.and_then(|row| row.try_get::<String, _>("state_value").ok()?.parse().ok()))
}

pub async fn set_state_i64(pool: &sqlx::PgPool, key: &str, value: i64) -> Result<(), sqlx::Error> {
    sqlx::query(
        r#"
        INSERT INTO telegram_bot_state (state_key, state_value, updated_at)
        VALUES ($1, $2, NOW())
        ON CONFLICT (state_key) DO UPDATE
        SET state_value = EXCLUDED.state_value,
            updated_at = EXCLUDED.updated_at
        "#,
    )
    .bind(key)
    .bind(value.to_string())
    .execute(pool)
    .await?;

    Ok(())
}

pub async fn fetch_new_heartbeats(
    pool: &sqlx::PgPool,
    after_id: i64,
) -> Result<Vec<StoredHeartbeat>, sqlx::Error> {
    let rows = sqlx::query(
        r#"
        SELECT id, imei, server_received_at, terminal_info_raw, terminal_info_bits,
               engine_status_guess, voltage_level, gsm_signal_strength
        FROM device_heartbeats
        WHERE id > $1
        ORDER BY id ASC
        LIMIT 100
        "#,
    )
    .bind(after_id)
    .fetch_all(pool)
    .await?;

    Ok(rows
        .into_iter()
        .map(|row| StoredHeartbeat {
            id: row.get("id"),
            imei: row.get("imei"),
            server_received_at: row.get("server_received_at"),
            terminal_info_raw: row.get("terminal_info_raw"),
            terminal_info_bits: row.get("terminal_info_bits"),
            engine_status_guess: row.get("engine_status_guess"),
            voltage_level: row.get("voltage_level"),
            gsm_signal_strength: row.get("gsm_signal_strength"),
        })
        .collect())
}

pub async fn fetch_latest_heartbeat(
    pool: &sqlx::PgPool,
) -> Result<Option<StoredHeartbeat>, sqlx::Error> {
    let row = sqlx::query(
        r#"
        SELECT id, imei, server_received_at, terminal_info_raw, terminal_info_bits,
               engine_status_guess, voltage_level, gsm_signal_strength
        FROM device_heartbeats
        ORDER BY id DESC
        LIMIT 1
        "#,
    )
    .fetch_optional(pool)
    .await?;

    Ok(row.map(|row| StoredHeartbeat {
        id: row.get("id"),
        imei: row.get("imei"),
        server_received_at: row.get("server_received_at"),
        terminal_info_raw: row.get("terminal_info_raw"),
        terminal_info_bits: row.get("terminal_info_bits"),
        engine_status_guess: row.get("engine_status_guess"),
        voltage_level: row.get("voltage_level"),
        gsm_signal_strength: row.get("gsm_signal_strength"),
    }))
}

pub async fn fetch_latest_location(
    pool: &sqlx::PgPool,
) -> Result<Option<StoredLocation>, sqlx::Error> {
    let row = sqlx::query(
        r#"
        SELECT imei, last_seen_at, latest_gps_timestamp, latest_latitude, latest_longitude,
               latest_speed_kph, latest_course, latest_satellite_count
        FROM devices
        WHERE latest_latitude IS NOT NULL AND latest_longitude IS NOT NULL
        ORDER BY last_seen_at DESC
        LIMIT 1
        "#,
    )
    .fetch_optional(pool)
    .await?;

    Ok(row.map(|row| StoredLocation {
        imei: row.get("imei"),
        last_seen_at: row.get("last_seen_at"),
        gps_timestamp: row.get("latest_gps_timestamp"),
        latitude: row.get("latest_latitude"),
        longitude: row.get("latest_longitude"),
        speed_kph: row.get("latest_speed_kph"),
        course: row.get("latest_course"),
        satellite_count: row.get("latest_satellite_count"),
    }))
}

#[cfg(test)]
mod tests {
    use std::env;

    use chrono::{TimeZone, Utc};

    use super::*;
    use crate::config::Config;
    use crate::db::Database;

    fn database_url() -> Option<String> {
        env::var("GT06_TEST_DATABASE_URL").ok()
    }

    #[test]
    fn parses_commands() {
        assert_eq!(BotCommand::parse("/start"), Some(BotCommand::Start));
        assert_eq!(BotCommand::parse("/help"), Some(BotCommand::Help));
        assert_eq!(BotCommand::parse("/bind_me"), Some(BotCommand::BindMe));
        assert_eq!(
            BotCommand::parse("/latest_location@my_bot"),
            Some(BotCommand::LatestLocation)
        );
        assert_eq!(BotCommand::parse("hello"), None);
    }

    #[test]
    fn formats_heartbeat_notification_message() {
        let heartbeat = StoredHeartbeat {
            id: 1,
            imei: "866221070478388".to_string(),
            server_received_at: Utc.with_ymd_and_hms(2026, 4, 13, 12, 0, 0).unwrap(),
            terminal_info_raw: 69,
            terminal_info_bits: "01000101".to_string(),
            engine_status_guess: "on".to_string(),
            voltage_level: 6,
            gsm_signal_strength: 3,
        };

        let text = format_heartbeat_notification(&heartbeat);
        assert!(text.contains("866221070478388"));
        assert!(text.contains("01000101"));
        assert!(text.contains("heuristic"));
    }

    #[tokio::test]
    async fn stores_and_restores_bot_state() -> Result<(), Box<dyn std::error::Error>> {
        let Some(database_url) = database_url() else {
            return Ok(());
        };

        let config = Config::from_pairs([
            ("DATABASE_URL", database_url.as_str()),
            ("DATABASE_MAX_CONNECTIONS", "1"),
        ]);
        let database = Database::connect(&config).await?.expect("database configured");
        sqlx::query("TRUNCATE telegram_bot_state RESTART IDENTITY")
            .execute(database.pool())
            .await?;

        set_state_i64(database.pool(), "last_telegram_update_id", 42).await?;
        set_state_i64(database.pool(), "last_notified_heartbeat_id", 77).await?;

        assert_eq!(
            get_state_i64(database.pool(), "last_telegram_update_id").await?,
            Some(42)
        );
        assert_eq!(
            get_state_i64(database.pool(), "last_notified_heartbeat_id").await?,
            Some(77)
        );

        Ok(())
    }

    #[tokio::test]
    async fn fetches_new_heartbeats_without_resending_old_rows(
    ) -> Result<(), Box<dyn std::error::Error>> {
        let Some(database_url) = database_url() else {
            return Ok(());
        };

        let config = Config::from_pairs([
            ("DATABASE_URL", database_url.as_str()),
            ("DATABASE_MAX_CONNECTIONS", "1"),
        ]);
        let database = Database::connect(&config).await?.expect("database configured");
        sqlx::query(
            "TRUNCATE telegram_bot_state, device_heartbeats, device_locations, devices RESTART IDENTITY CASCADE",
        )
        .execute(database.pool())
        .await?;

        sqlx::query(
            r#"
            INSERT INTO devices (imei, first_seen_at, last_seen_at, created_at, updated_at)
            VALUES ('866221070478388', NOW(), NOW(), NOW(), NOW())
            "#,
        )
        .execute(database.pool())
        .await?;

        sqlx::query(
            r#"
            INSERT INTO device_heartbeats (
                device_id, imei, server_received_at, protocol_number, peer_addr, terminal_info_raw,
                terminal_info_bits, oil_and_electricity_connected, gps_tracking_on, alarm_active,
                charge_connected, acc_high, defense_active, engine_status_guess, voltage_level,
                gsm_signal_strength, alarm_language
            )
            VALUES
                (1, '866221070478388', NOW(), 19, '127.0.0.1:5000', 69, '01000101', true, false, true, false, true, false, 'on', 6, 3, 2),
                (1, '866221070478388', NOW(), 19, '127.0.0.1:5000', 5, '00000101', true, false, true, false, false, false, 'off', 6, 2, 2)
            "#,
        )
        .execute(database.pool())
        .await?;

        let first_batch = fetch_new_heartbeats(database.pool(), 0).await?;
        assert_eq!(first_batch.len(), 2);

        let second_batch = fetch_new_heartbeats(database.pool(), first_batch[1].id).await?;
        assert!(second_batch.is_empty());

        Ok(())
    }
}
