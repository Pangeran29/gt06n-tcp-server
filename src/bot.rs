use std::time::Duration;

use chrono::{DateTime, FixedOffset, NaiveDateTime, TimeZone, Utc};
use reqwest::{multipart, Client};
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
    Unknown(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum TelegramRegistrationStatus {
    AwaitingImei,
    Bound,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum SessionAction {
    Yes,
    No,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum TheftAlertAction {
    StreamLocation { session_id: Option<i64> },
    CheckLatestStatus { session_id: Option<i64> },
    ContactSupport { session_id: Option<i64> },
}

impl SessionAction {
    fn parse(data: &str) -> Option<Self> {
        let mut parts = data.split(':');
        let prefix = parts.next()?;
        let action = parts.next()?;

        if prefix != "engine_session" || parts.next().is_some() {
            return None;
        }

        Some(match action {
            "yes" => Self::Yes,
            "no" => Self::No,
            _ => return None,
        })
    }
}

impl TheftAlertAction {
    fn parse(data: &str) -> Option<Self> {
        let mut parts = data.split(':');
        let prefix = parts.next()?;
        let action = parts.next()?;
        let session_id = match parts.next() {
            Some(value) => Some(value.parse().ok()?),
            None => None,
        };

        if prefix != "theft_alert" || parts.next().is_some() {
            return None;
        }

        Some(match action {
            "stream_location" => Self::StreamLocation { session_id },
            "check_latest_status" => Self::CheckLatestStatus { session_id },
            "contact_support" => Self::ContactSupport { session_id },
            _ => return None,
        })
    }
}

impl BotCommand {
    pub fn parse(text: &str) -> Option<Self> {
        let command = text.split_whitespace().next()?;
        let normalized = command.split('@').next().unwrap_or(command);

        Some(match normalized {
            "/start" => Self::Start,
            "/help" => Self::Help,
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
}

const WIB_OFFSET_SECONDS: i32 = 7 * 60 * 60;
const ENGINE_ON_ALERT_COOLDOWN_SECS: i64 = 900;
const LIVE_TRACKING_BASE_URL: &str = "https://hearthbeats-client.vercel.app/live-tracking";
const ENGINE_ON_STICKER_BYTES: &[u8] = include_bytes!("../asset/AnimatedSticker.tgs");
const BIND_SUCCESS_STICKER_BYTES: &[u8] = include_bytes!("../asset/AnimatedSticker - hi.tgs");

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
        })
    }

    pub async fn run(&self) -> Result<(), BotError> {
        info!("telegram bot started");

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

            if let Some(callback_query) = update.callback_query {
                self.handle_callback_query(callback_query).await?;
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
        let last_notified = get_state_i64(self.database.pool(), "last_notified_heartbeat_id")
            .await?
            .unwrap_or(0);

        let heartbeats = fetch_new_heartbeats(self.database.pool(), last_notified).await?;

        for heartbeat in heartbeats {
            let notification_chat_ids =
                fetch_notification_chat_ids_for_imei(self.database.pool(), &heartbeat.imei)
                    .await?;

            if notification_chat_ids.is_empty() {
                set_state_i64(
                    self.database.pool(),
                    "last_notified_heartbeat_id",
                    heartbeat.id,
                )
                .await?;
                continue;
            }

            if let Some(status) = heartbeat.notification_status() {
                for chat_id in notification_chat_ids {
                    self.process_heartbeat_notification_for_chat(&heartbeat, status, chat_id)
                        .await?;
                }
            } else {
                info!(
                    imei = %heartbeat.imei,
                    engine_status_guess = %heartbeat.engine_status_guess,
                    "skipping heartbeat notification because status is not notifiable"
                );
            }

            set_state_i64(
                self.database.pool(),
                "last_notified_heartbeat_id",
                heartbeat.id,
            )
            .await?;
        }

        Ok(())
    }

    async fn process_heartbeat_notification_for_chat(
        &self,
        heartbeat: &StoredHeartbeat,
        status: &str,
        chat_id: i64,
    ) -> Result<(), BotError> {
        let existing = fetch_notification_state(self.database.pool(), &heartbeat.imei, chat_id).await?;

        if status == "on" {
            return self
                .process_engine_on_notification_for_chat(heartbeat, chat_id, existing)
                .await;
        }

        let text = format_engine_status_notification(heartbeat, status);

        match existing {
            Some(existing) if existing.last_status == status => {
                if let Some(session) =
                    fetch_latest_theft_off_session(self.database.pool(), &heartbeat.imei, chat_id)
                        .await?
                        .filter(|session| session.ride_status_message_id == Some(existing.last_message_id))
                {
                    let off_started_at = session.resolved_at.unwrap_or(heartbeat.server_received_at);
                    let off_text =
                        format_theft_engine_off_follow_up_message(off_started_at, Utc::now());

                    if let Err(error) = self
                        .edit_message_text(chat_id, existing.last_message_id, &off_text, None)
                        .await
                    {
                        warn!(
                            error = %error,
                            imei = %heartbeat.imei,
                            message_id = existing.last_message_id,
                            "failed to edit existing status message; sending a new one"
                        );
                        let message_id =
                            self.send_message_internal(chat_id, &off_text, None).await?;
                        upsert_notification_state(
                            self.database.pool(),
                            &heartbeat.imei,
                            chat_id,
                            status,
                            message_id,
                            heartbeat.id,
                        )
                        .await?;
                    } else {
                        upsert_notification_state(
                            self.database.pool(),
                            &heartbeat.imei,
                            chat_id,
                            status,
                            existing.last_message_id,
                            heartbeat.id,
                        )
                        .await?;
                    }
                } else {
                    upsert_notification_state(
                        self.database.pool(),
                        &heartbeat.imei,
                        chat_id,
                        status,
                        existing.last_message_id,
                        heartbeat.id,
                    )
                    .await?;
                }
            }
            _ => {
                let message_id = if let Some(session) =
                    fetch_active_reported_theft_session(self.database.pool(), &heartbeat.imei, chat_id)
                        .await?
                {
                    let message_id = self
                        .send_message_internal(chat_id, format_theft_engine_off_message(), None)
                        .await?;
                    set_engine_session_ride_status_message_id(
                        self.database.pool(),
                        session.id,
                        message_id,
                    )
                    .await?;
                    resolve_engine_session(self.database.pool(), session.id, "theft_engine_off")
                        .await?;
                    message_id
                } else if let Some(session) =
                    fetch_active_pending_session(self.database.pool(), &heartbeat.imei, chat_id)
                        .await?
                {
                    if let Err(error) =
                        self.clear_inline_keyboard(chat_id, session.prompt_message_id).await
                    {
                        warn!(
                            error = %error,
                            imei = %heartbeat.imei,
                            message_id = session.prompt_message_id,
                            "failed to clear pending confirmation keyboard before finishing session"
                        );
                    }

                    resolve_engine_session(self.database.pool(), session.id, "finished").await?;
                    self.send_message(chat_id, format_session_finished_message())
                        .await?;
                    let ride_summary = fetch_ride_summary(
                        self.database.pool(),
                        &heartbeat.imei,
                        session.created_at,
                        heartbeat.server_received_at,
                    )
                    .await?;
                    let latest_location =
                        fetch_latest_location_for_imei(self.database.pool(), &heartbeat.imei)
                            .await?;
                    self.send_message(
                        chat_id,
                        &format_ride_summary_message(
                            &session,
                            heartbeat.server_received_at,
                            ride_summary.as_ref(),
                            latest_location.as_ref(),
                        ),
                    )
                    .await?
                } else if let Some(session) =
                    fetch_active_confirmed_safe_session(self.database.pool(), &heartbeat.imei, chat_id)
                        .await?
                {
                    resolve_engine_session(self.database.pool(), session.id, "finished").await?;
                    self.send_message(chat_id, format_session_finished_message())
                        .await?;
                    let ride_summary = fetch_ride_summary(
                        self.database.pool(),
                        &heartbeat.imei,
                        session.created_at,
                        heartbeat.server_received_at,
                    )
                    .await?;
                    let latest_location =
                        fetch_latest_location_for_imei(self.database.pool(), &heartbeat.imei)
                            .await?;
                    self.send_message(
                        chat_id,
                        &format_ride_summary_message(
                            &session,
                            heartbeat.server_received_at,
                            ride_summary.as_ref(),
                            latest_location.as_ref(),
                        ),
                    )
                    .await?
                } else {
                    self.send_message(chat_id, &text).await?
                };

                upsert_notification_state(
                    self.database.pool(),
                    &heartbeat.imei,
                    chat_id,
                    status,
                    message_id,
                    heartbeat.id,
                )
                .await?;
            }
        }

        Ok(())
    }

    async fn process_engine_on_notification_for_chat(
        &self,
        heartbeat: &StoredHeartbeat,
        chat_id: i64,
        existing: Option<NotificationState>,
    ) -> Result<(), BotError> {
        let pending_sessions =
            fetch_active_pending_sessions(self.database.pool(), &heartbeat.imei, chat_id).await?;
        let latest_pending = pending_sessions.first();
        let last_on_heartbeat_at = match existing
            .as_ref()
            .filter(|state| state.last_status == "on")
            .map(|state| state.last_heartbeat_id)
        {
            Some(last_heartbeat_id) => {
                fetch_heartbeat_server_received_at_by_id(self.database.pool(), last_heartbeat_id)
                    .await?
            }
            None => None,
        };

        if !should_start_new_engine_on_session(
            heartbeat.server_received_at,
            last_on_heartbeat_at,
        ) {
            let message_id = latest_pending
                .map(|session| session.prompt_message_id)
                .or(existing.as_ref().map(|state| state.last_message_id))
                .unwrap_or_default();

            info!(
                imei = %heartbeat.imei,
                chat_id,
                heartbeat_id = heartbeat.id,
                message_id,
                "continuing existing engine-on session because heartbeat gap is below threshold"
            );

            upsert_notification_state(
                self.database.pool(),
                &heartbeat.imei,
                chat_id,
                "on",
                message_id,
                heartbeat.id,
            )
            .await?;

            return Ok(());
        }

        for session in &pending_sessions {
            if let Err(error) = self
                .clear_inline_keyboard(chat_id, session.prompt_message_id)
                .await
            {
                warn!(
                    error = %error,
                    imei = %heartbeat.imei,
                    chat_id,
                    session_id = session.id,
                    message_id = session.prompt_message_id,
                    "failed to clear pending confirmation keyboard; skipping new warning to avoid duplicate inline buttons"
                );

                let message_id = latest_pending
                    .map(|pending| pending.prompt_message_id)
                    .or(existing.as_ref().map(|state| state.last_message_id))
                    .unwrap_or_default();

                upsert_notification_state(
                    self.database.pool(),
                    &heartbeat.imei,
                    chat_id,
                    "on",
                    message_id,
                    heartbeat.id,
                )
                .await?;

                return Ok(());
            }

            resolve_engine_session(self.database.pool(), session.id, "finished").await?;
            self.send_message(chat_id, format_session_finished_message())
                .await?;
            let ride_summary = fetch_ride_summary(
                self.database.pool(),
                &heartbeat.imei,
                session.created_at,
                heartbeat.server_received_at,
            )
            .await?;
            let latest_location =
                fetch_latest_location_for_imei(self.database.pool(), &heartbeat.imei).await?;
            self.send_message(
                chat_id,
                &format_ride_summary_message(
                    session,
                    heartbeat.server_received_at,
                    ride_summary.as_ref(),
                    latest_location.as_ref(),
                ),
            )
            .await?;
        }

        let message_id = self.send_engine_on_confirmation(chat_id, heartbeat).await?;
        create_engine_session(
            self.database.pool(),
            &heartbeat.imei,
            chat_id,
            heartbeat.id,
            message_id,
        )
        .await?;
        upsert_notification_state(
            self.database.pool(),
            &heartbeat.imei,
            chat_id,
            "on",
            message_id,
            heartbeat.id,
        )
        .await?;

        Ok(())
    }

    async fn handle_message(&self, message: TelegramMessage) -> Result<(), BotError> {
        let Some(from) = message.from.as_ref() else {
            return Ok(());
        };
        let Some(text) = message.text.as_deref() else {
            return Ok(());
        };

        let chat_id = message.chat.id;
        let telegram_user_id = from.id;

        if let Some(command) = BotCommand::parse(text) {
            return self
                .handle_command(chat_id, telegram_user_id, command)
                .await;
        }

        let Some(user) = fetch_telegram_user_by_user_id(self.database.pool(), telegram_user_id).await?
        else {
            return Ok(());
        };

        if user.registration_status != TelegramRegistrationStatus::AwaitingImei {
            return Ok(());
        }

        let imei = text.trim();
        if !is_valid_imei(imei) {
            self.send_message(
                chat_id,
                "IMEI must be exactly 15 numeric digits. Please send a valid IMEI.",
            )
            .await?;
            return Ok(());
        }

        if user.bound_imei.is_some() {
            self.send_message(
                chat_id,
                "Your Telegram account is already bound to a device.",
            )
            .await?;
            return Ok(());
        }

        if !device_exists(self.database.pool(), imei).await? {
            self.send_message(
                chat_id,
                "That IMEI is not registered in the system yet. Please check the IMEI and try again.",
            )
            .await?;
            return Ok(());
        }

        if is_device_bound_to_another_user(self.database.pool(), imei, telegram_user_id).await? {
            self.send_message(
                chat_id,
                "That device is already bound to another Telegram user.",
            )
            .await?;
            return Ok(());
        }

        bind_telegram_user_to_imei(self.database.pool(), telegram_user_id, chat_id, imei).await?;
        self.send_message(
            chat_id,
            &format!("Success. This Telegram account is now bound to IMEI {imei}."),
        )
        .await?;
        if let Err(error) = self.send_bind_success_sticker(chat_id).await {
            warn!(error = %error, "failed to send bind-success sticker");
        }
        self.send_message_internal(
            chat_id,
            &format_start_status_message(),
            Some(start_menu_keyboard()),
        )
        .await?;

        Ok(())
    }

    async fn handle_command(
        &self,
        chat_id: i64,
        telegram_user_id: i64,
        command: BotCommand,
    ) -> Result<(), BotError> {
        let user = fetch_telegram_user_by_user_id(self.database.pool(), telegram_user_id).await?;

        match command {
            BotCommand::Start => {
                match user {
                    Some(user) if user.bound_imei.is_some() => {
                        self.send_message_internal(
                            chat_id,
                            &format_start_status_message(),
                            Some(start_menu_keyboard()),
                        )
                        .await?;
                    }
                    _ => {
                        upsert_telegram_user_registration_state(
                            self.database.pool(),
                            telegram_user_id,
                            chat_id,
                            TelegramRegistrationStatus::AwaitingImei,
                        )
                        .await?;
                        self.send_message(
                            chat_id,
                            "Welcome. Please send your device IMEI to bind this Telegram account.",
                        )
                        .await?;
                    }
                }
            }
            BotCommand::Help => {
                self.send_message(chat_id, HELP_TEXT).await?;
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

    async fn handle_callback_query(
        &self,
        callback_query: TelegramCallbackQuery,
    ) -> Result<(), BotError> {
        let Some(data) = callback_query.data.as_deref() else {
            return Ok(());
        };
        let Some(message) = callback_query.message else {
            return Ok(());
        };
        let chat_id = message.chat.id;

        if let Some(action) = TheftAlertAction::parse(data) {
            self.answer_callback_query(&callback_query.id, "", false)
                .await?;
            return self
                .handle_theft_alert_action(message, action)
                .await;
        }

        let Some(action) = SessionAction::parse(data) else {
            return Ok(());
        };
        let prompt_message_id = i64::from(message.message_id.unwrap_or_default());

        let Some(session) =
            fetch_engine_session_by_prompt_message(self.database.pool(), chat_id, prompt_message_id)
                .await?
        else {
            self.answer_callback_query(
                &callback_query.id,
                "Session not found or already inactive.",
                false,
            )
            .await?;
            return Ok(());
        };

        if session.chat_id != chat_id || session.prompt_message_id != prompt_message_id {
            self.answer_callback_query(
                &callback_query.id,
                "This session does not match the selected message.",
                false,
            )
            .await?;
            return Ok(());
        }

        if session.session_status != "pending_confirmation" {
            self.answer_callback_query(
                &callback_query.id,
                "This session already ended.",
                false,
            )
            .await?;
            return Ok(());
        }

        self.answer_callback_query(&callback_query.id, "", false)
            .await?;
        self.clear_inline_keyboard(chat_id, prompt_message_id).await?;

        match action {
            SessionAction::Yes => {
                self.send_message(chat_id, format_ride_safe_message()).await?;
                if let Err(error) = self.send_engine_on_sticker(chat_id).await {
                    warn!(error = %error, "failed to send engine-on sticker");
                }
                resolve_engine_session(self.database.pool(), session.id, "confirmed_safe").await?;
            }
            SessionAction::No => {
                self.send_message(chat_id, "🧨").await?;
                self.send_message_internal(
                    chat_id,
                    format_theft_warning_message(),
                    Some(theft_alert_keyboard(Some(session.id))),
                )
                .await?;
                resolve_engine_session(self.database.pool(), session.id, "reported_theft").await?;
            }
        }

        Ok(())
    }

    async fn handle_theft_alert_action(
        &self,
        message: TelegramMessage,
        action: TheftAlertAction,
    ) -> Result<(), BotError> {
        let chat_id = message.chat.id;
        let session_id = match &action {
            TheftAlertAction::StreamLocation { session_id }
            | TheftAlertAction::CheckLatestStatus { session_id }
            | TheftAlertAction::ContactSupport { session_id } => *session_id,
        };

        let user = fetch_telegram_user_by_chat_id(self.database.pool(), chat_id).await?;
        let bound_imei = user.as_ref().and_then(|value| value.bound_imei.as_deref());

        let session = if let Some(session_id) = session_id {
            let Some(session) = fetch_engine_session_by_id(self.database.pool(), session_id).await?
            else {
                return Ok(());
            };

            if session.chat_id != chat_id {
                return Ok(());
            }

            Some(session)
        } else {
            None
        };

        let session_imei = session.as_ref().map(|value| value.imei.clone());
        let imei = if let Some(session_imei) = session_imei.as_deref() {
            session_imei
        } else if let Some(bound_imei) = bound_imei {
            bound_imei
        } else {
            self.send_message(chat_id, "This Telegram account is not bound yet. Use /start first.")
                .await?;
            return Ok(());
        };

        match action {
            TheftAlertAction::StreamLocation { .. } => {
                let latest_location = fetch_latest_location_for_imei(self.database.pool(), imei).await?;
                let latest_session_created_at = if session.is_some() {
                    None
                } else {
                    fetch_latest_engine_session_for_imei_chat(self.database.pool(), imei, chat_id)
                        .await?
                        .map(|value| value.created_at)
                };
                let start_at = select_stream_location_start_at(
                    session.as_ref().map(|value| value.created_at),
                    latest_session_created_at,
                    latest_location.as_ref().and_then(|value| value.last_seen_at),
                );
                let live_tracking_link =
                    start_at.and_then(|value| build_live_tracking_link(imei, value));
                let text = format_stream_location_message(live_tracking_link.as_deref());
                self.send_message(chat_id, &text).await?;
            }
            TheftAlertAction::CheckLatestStatus { .. } => {
                let location = fetch_latest_location_for_imei(self.database.pool(), imei).await?;
                let latest_heartbeat = fetch_latest_heartbeat_for_imei(self.database.pool(), imei).await?;
                let fallback_session = match session {
                    Some(session) => session,
                    None => fetch_latest_engine_session_for_imei_chat(
                        self.database.pool(),
                        imei,
                        chat_id,
                    )
                    .await?
                    .unwrap_or_else(|| {
                        build_status_session(
                            imei,
                            chat_id,
                            latest_heartbeat.as_ref(),
                            location.as_ref(),
                        )
                    }),
                };
                let text = format_latest_motor_status_initial_message(
                    &fallback_session,
                    latest_heartbeat.as_ref(),
                    location.as_ref(),
                    Utc::now(),
                );
                self.send_message(chat_id, &text).await?;
            }
            TheftAlertAction::ContactSupport { .. } => {
                self.send_message(chat_id, format_contact_support_message())
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

    async fn send_message(&self, chat_id: i64, text: &str) -> Result<i64, reqwest::Error> {
        self.send_message_internal(chat_id, text, None).await
    }

    async fn send_engine_on_confirmation(
        &self,
        chat_id: i64,
        heartbeat: &StoredHeartbeat,
    ) -> Result<i64, reqwest::Error> {
        let text = format_engine_on_confirmation_message(heartbeat);
        let keyboard = engine_session_confirmation_keyboard();

        self.send_message_internal(chat_id, &text, Some(keyboard)).await
    }

    async fn send_engine_on_sticker(&self, chat_id: i64) -> Result<(), reqwest::Error> {
        let sticker_part = multipart::Part::bytes(ENGINE_ON_STICKER_BYTES.to_vec())
            .file_name("AnimatedSticker.tgs")
            .mime_str("application/x-tgsticker")?;

        let form = multipart::Form::new()
            .text("chat_id", chat_id.to_string())
            .part("sticker", sticker_part);

        let response = self
            .client
            .post(format!("{}/sendSticker", self.base_url))
            .multipart(form)
            .send()
            .await?
            .error_for_status()?;

        let _ = response.bytes().await?;
        Ok(())
    }

    async fn send_bind_success_sticker(&self, chat_id: i64) -> Result<(), reqwest::Error> {
        let sticker_part = multipart::Part::bytes(BIND_SUCCESS_STICKER_BYTES.to_vec())
            .file_name("AnimatedSticker - hi.tgs")
            .mime_str("application/x-tgsticker")?;

        let form = multipart::Form::new()
            .text("chat_id", chat_id.to_string())
            .part("sticker", sticker_part);

        let response = self
            .client
            .post(format!("{}/sendSticker", self.base_url))
            .multipart(form)
            .send()
            .await?
            .error_for_status()?;

        let _ = response.bytes().await?;
        Ok(())
    }

    async fn send_message_internal(
        &self,
        chat_id: i64,
        text: &str,
        reply_markup: Option<InlineKeyboardMarkup>,
    ) -> Result<i64, reqwest::Error> {
        let request = SendMessageRequest {
            chat_id,
            text: text.to_string(),
            reply_markup,
        };

        let response = self
            .client
            .post(format!("{}/sendMessage", self.base_url))
            .json(&request)
            .send()
            .await?
            .error_for_status()?;

        let body: TelegramResponse<TelegramMessage> = response.json().await?;
        Ok(i64::from(body.result.message_id.unwrap_or_default()))
    }

    async fn edit_message_text(
        &self,
        chat_id: i64,
        message_id: i64,
        text: &str,
        reply_markup: Option<InlineKeyboardMarkup>,
    ) -> Result<(), reqwest::Error> {
        let request = EditMessageTextRequest {
            chat_id,
            message_id,
            text: text.to_string(),
            reply_markup,
        };

        let response = self
            .client
            .post(format!("{}/editMessageText", self.base_url))
            .json(&request)
            .send()
            .await?
            .error_for_status()?;

        let _ = response.bytes().await?;
        Ok(())
    }

    async fn clear_inline_keyboard(
        &self,
        chat_id: i64,
        message_id: i64,
    ) -> Result<(), reqwest::Error> {
        self.edit_message_reply_markup(chat_id, message_id, None).await
    }

    async fn edit_message_reply_markup(
        &self,
        chat_id: i64,
        message_id: i64,
        reply_markup: Option<InlineKeyboardMarkup>,
    ) -> Result<(), reqwest::Error> {
        let request = EditMessageReplyMarkupRequest {
            chat_id,
            message_id,
            reply_markup,
        };

        let response = self
            .client
            .post(format!("{}/editMessageReplyMarkup", self.base_url))
            .json(&request)
            .send()
            .await?
            .error_for_status()?;

        let _ = response.bytes().await?;
        Ok(())
    }

    async fn answer_callback_query(
        &self,
        callback_query_id: &str,
        text: &str,
        show_alert: bool,
    ) -> Result<(), reqwest::Error> {
        let request = AnswerCallbackQueryRequest {
            callback_query_id: callback_query_id.to_string(),
            text: text.to_string(),
            show_alert,
        };

        let response = self
            .client
            .post(format!("{}/answerCallbackQuery", self.base_url))
            .json(&request)
            .send()
            .await?
            .error_for_status()?;

        let _ = response.bytes().await?;
        Ok(())
    }
}

pub const HELP_TEXT: &str = "Track your motor real time, get info when your motor on/off, tracking real time motor position, get historical riding data\n\n/start - Get the welcome message along with all feature of this bot\n/help - Get this message";

#[derive(Debug, Deserialize)]
struct TelegramResponse<T> {
    result: T,
}

#[derive(Debug, Deserialize)]
struct TelegramUpdate {
    update_id: i32,
    message: Option<TelegramMessage>,
    callback_query: Option<TelegramCallbackQuery>,
}

#[derive(Debug, Deserialize)]
struct TelegramMessage {
    chat: TelegramChat,
    from: Option<TelegramUser>,
    text: Option<String>,
    message_id: Option<i32>,
}

#[derive(Debug, Deserialize)]
struct TelegramCallbackQuery {
    id: String,
    data: Option<String>,
    message: Option<TelegramMessage>,
}

#[derive(Debug, Deserialize)]
struct TelegramChat {
    id: i64,
}

#[derive(Debug, Deserialize)]
struct TelegramUser {
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
    #[serde(skip_serializing_if = "Option::is_none")]
    reply_markup: Option<InlineKeyboardMarkup>,
}

#[derive(Debug, Serialize)]
struct EditMessageTextRequest {
    chat_id: i64,
    message_id: i64,
    text: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    reply_markup: Option<InlineKeyboardMarkup>,
}

#[derive(Debug, Serialize)]
struct EditMessageReplyMarkupRequest {
    chat_id: i64,
    message_id: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    reply_markup: Option<InlineKeyboardMarkup>,
}

#[derive(Debug, Serialize)]
struct AnswerCallbackQueryRequest {
    callback_query_id: String,
    text: String,
    show_alert: bool,
}

#[derive(Debug, Serialize, Clone)]
struct InlineKeyboardMarkup {
    inline_keyboard: Vec<Vec<InlineKeyboardButton>>,
}

#[derive(Debug, Serialize, Clone)]
struct InlineKeyboardButton {
    text: String,
    callback_data: String,
}

fn engine_session_confirmation_keyboard() -> InlineKeyboardMarkup {
    InlineKeyboardMarkup {
        inline_keyboard: vec![vec![
            InlineKeyboardButton {
                text: "Yes, it's me".to_string(),
                callback_data: "engine_session:yes".to_string(),
            },
            InlineKeyboardButton {
                text: "No, not me".to_string(),
                callback_data: "engine_session:no".to_string(),
            },
        ]],
    }
}

fn theft_alert_keyboard(session_id: Option<i64>) -> InlineKeyboardMarkup {
    InlineKeyboardMarkup {
        inline_keyboard: vec![
            vec![
                InlineKeyboardButton {
                    text: "stream location".to_string(),
                    callback_data: theft_alert_callback_data("stream_location", session_id),
                },
                InlineKeyboardButton {
                    text: "health check".to_string(),
                    callback_data: theft_alert_callback_data("check_latest_status", session_id),
                },
            ],
            vec![InlineKeyboardButton {
                text: "contact support".to_string(),
                callback_data: theft_alert_callback_data("contact_support", session_id),
            }],
        ],
    }
}

fn start_menu_keyboard() -> InlineKeyboardMarkup {
    InlineKeyboardMarkup {
        inline_keyboard: vec![vec![
            InlineKeyboardButton {
                text: "stream location".to_string(),
                callback_data: theft_alert_callback_data("stream_location", None),
            },
            InlineKeyboardButton {
                text: "health check".to_string(),
                callback_data: theft_alert_callback_data("check_latest_status", None),
            },
        ]],
    }
}

fn theft_alert_callback_data(action: &str, session_id: Option<i64>) -> String {
    match session_id {
        Some(session_id) => format!("theft_alert:{action}:{session_id}"),
        None => format!("theft_alert:{action}"),
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct StoredHeartbeat {
    pub id: i64,
    pub imei: String,
    pub server_received_at: DateTime<Utc>,
    pub terminal_info_raw: i32,
    pub terminal_info_bits: String,
    pub gps_tracking_on: bool,
    pub acc_high: Option<bool>,
    pub vibration_detected: bool,
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

#[derive(Debug, Clone, PartialEq)]
pub struct RideSummary {
    pub total_distance_km: f64,
    pub average_speed_kph: f64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NotificationState {
    pub imei: String,
    pub chat_id: i64,
    pub last_status: String,
    pub last_message_id: i64,
    pub last_heartbeat_id: i64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EngineSession {
    pub id: i64,
    pub imei: String,
    pub chat_id: i64,
    pub trigger_heartbeat_id: i64,
    pub prompt_message_id: i64,
    pub ride_status_message_id: Option<i64>,
    pub session_status: String,
    pub created_at: DateTime<Utc>,
    pub resolved_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct TelegramUserRecord {
    telegram_user_id: i64,
    chat_id: i64,
    bound_imei: Option<String>,
    registration_status: TelegramRegistrationStatus,
}

impl StoredHeartbeat {
    pub fn notification_status(&self) -> Option<&str> {
        match self.engine_status_guess.as_str() {
            "on" => Some("on"),
            "off" => Some("off"),
            _ => None,
        }
    }
}

pub fn format_heartbeat_notification(heartbeat: &StoredHeartbeat) -> String {
    format!(
        "Heartbeat update\nIMEI: {}\nServer time: {}\nEngine status: {} (heuristic)\nTerminal info: {} ({})\nVoltage level: {}\nGSM signal: {}\nGPS tracking: {}\nACC high: {}\nVibration detected: {}",
        heartbeat.imei,
        heartbeat.server_received_at.format("%Y-%m-%d %H:%M:%S UTC"),
        heartbeat.engine_status_guess,
        heartbeat.terminal_info_raw,
        heartbeat.terminal_info_bits,
        heartbeat.voltage_level,
        heartbeat.gsm_signal_strength,
        heartbeat.gps_tracking_on,
        option_bool(heartbeat.acc_high),
        heartbeat.vibration_detected
    )
}

pub fn format_engine_status_notification(heartbeat: &StoredHeartbeat, status: &str) -> String {
    let wib = FixedOffset::east_opt(WIB_OFFSET_SECONDS)
        .expect("valid WIB offset")
        .from_utc_datetime(&heartbeat.server_received_at.naive_utc());

    match status {
        "on" => format!(
            "Motor Dinyalakan\nKalau ini bukan kamu, segera cek lokasi motor.\n{}",
            wib.format("%d %b %Y • %H:%M WIB")
        ),
        "off" => format!(
            "Motor Dimatikan\nAktivitas terdeteksi pada motor kamu.\n{}",
            wib.format("%d %b %Y • %H:%M WIB")
        ),
        _ => format_heartbeat_notification(heartbeat),
    }
}

pub fn format_engine_on_confirmation_message(heartbeat: &StoredHeartbeat) -> String {
    format_engine_on_confirmation_message_with_duration(heartbeat, None)
}

pub fn format_engine_on_confirmation_message_with_duration(
    heartbeat: &StoredHeartbeat,
    started_at: Option<DateTime<Utc>>,
) -> String {
    let _ = heartbeat;
    let _ = started_at;

    format!(
        "🚨 Security Alert: Motor Turned ON\nWe detected your motor just turned ON.\nWas this you?",
    )
}

pub fn format_ride_safe_message() -> &'static str {
    "Ride safe, We are tracking you in case there's something wrong."
}

pub fn format_session_finished_message() -> &'static str {
    "Ride session ended."
}

pub fn format_theft_warning_message() -> &'static str {
    "Pay attention\nThere's indication that your motor is being theft. Use button below to track your motor."
}

pub fn format_theft_location_message(location: Option<&StoredLocation>) -> String {
    if let Some(location) = location {
        format_latest_location_message(location)
    } else {
        "Latest location\nLokasi terakhir belum tersedia.".to_string()
    }
}

pub fn format_theft_engine_off_message() -> &'static str {
    "Motor terdeteksi mati (berhenti).\nYou can still tracking your motor via GPS device battery mode."
}

pub fn format_theft_engine_off_follow_up_message(
    off_started_at: DateTime<Utc>,
    current_time: DateTime<Utc>,
) -> String {
    let wib = FixedOffset::east_opt(WIB_OFFSET_SECONDS).expect("valid WIB offset");
    let off_started_wib = wib.from_utc_datetime(&off_started_at.naive_utc());
    let duration = current_time
        .signed_duration_since(off_started_at)
        .to_std()
        .unwrap_or_default();
    let total_seconds = duration.as_secs();
    let hours = total_seconds / 3600;
    let minutes = (total_seconds % 3600) / 60;
    let seconds = total_seconds % 60;

    format!(
        "{}\nOff from: {} -> now\nDuration: {:02}:{:02}:{:02}",
        format_theft_engine_off_message(),
        off_started_wib.format("%d %b %Y %H:%M:%S WIB"),
        hours,
        minutes,
        seconds,
    )
}

pub fn format_stream_location_message(live_tracking_link: Option<&str>) -> String {
    let link = live_tracking_link
        .unwrap_or("Live tracking link is not available yet.")
        .to_string();

    format!(
        "📍 Live Tracking Activated\nTrack your motor in real-time using this link:\n{}\n\n🔗 This link is shareable — send it to someone you trust if you need help tracking.",
        link
    )
}

fn build_live_tracking_link(imei: &str, start_at: DateTime<Utc>) -> Option<String> {
    let mut url = reqwest::Url::parse(&format!("{LIVE_TRACKING_BASE_URL}/{imei}")).ok()?;
    let start_at = start_at.format("%Y-%m-%dT%H:%M:%SZ").to_string();
    url.query_pairs_mut().append_pair("start_at", &start_at);
    Some(url.into())
}

fn select_stream_location_start_at(
    explicit_session_created_at: Option<DateTime<Utc>>,
    latest_session_created_at: Option<DateTime<Utc>>,
    latest_location_last_seen_at: Option<DateTime<Utc>>,
) -> Option<DateTime<Utc>> {
    explicit_session_created_at
        .or(latest_session_created_at)
        .or(latest_location_last_seen_at)
}

pub fn format_latest_motor_status_message(
    session: &EngineSession,
    heartbeat: Option<&StoredHeartbeat>,
    location: Option<&StoredLocation>,
) -> String {
    format_latest_motor_status_message_at(session, heartbeat, location, Utc::now())
}

pub fn format_latest_motor_status_initial_message(
    session: &EngineSession,
    heartbeat: Option<&StoredHeartbeat>,
    location: Option<&StoredLocation>,
    requested_at: DateTime<Utc>,
) -> String {
    format_latest_motor_status_message_at(session, heartbeat, location, requested_at)
}

pub fn format_contact_support_message() -> &'static str {
    "1. Hubungi Call Center 110\n'Halo Polisi, saya ingin melaporkan pencurian motor yang baru saja terjadi. Posisi pelaku sedang terpantau di GPS saya. Mohon bantuan untuk pengejaran.'\n\n2. Datangi SPKT Polsek/Polres\nLangsung ke bagian SPKT (Sentra Pelayanan Kepolisian Terpadu). Tunjukkan aplikasi GPS yang sedang live kepada petugas. Polisi akan langsung berkoordinasi dengan tim Buser/Resmob untuk bergerak ke titik tersebut.\n\n3. Bawa Bukti Kepemilikan\nSiapkan STNK/BPKB (asli atau foto) dan KTP. Polisi butuh ini untuk memastikan itu benar motor Anda sebelum mereka melakukan penindakan atau penangkapan.\n\n 4. Minta Pendampingan Unit Lapangan\nSetelah melapor, minta izin untuk mendampingi petugas (di mobil patroli) atau memberikan akses akun GPS Anda kepada petugas agar mereka bisa mengejar target secara akurat.\n\n⚠️ PENTING: Jangan mendatangi lokasi GPS sendirian. Biarkan polisi yang melakukan tindakan penggerebekan demi keselamatan Anda."
}

pub fn format_ride_summary_message(
    session: &EngineSession,
    off_time: DateTime<Utc>,
    summary: Option<&RideSummary>,
    latest_location: Option<&StoredLocation>,
) -> String {
    let wib = FixedOffset::east_opt(WIB_OFFSET_SECONDS).expect("valid WIB offset");
    let started_wib = wib.from_utc_datetime(&session.created_at.naive_utc());
    let off_wib = wib.from_utc_datetime(&off_time.naive_utc());
    let duration = off_time
        .signed_duration_since(session.created_at)
        .to_std()
        .unwrap_or_default();
    let total_seconds = duration.as_secs();
    let hours = total_seconds / 3600;
    let minutes = (total_seconds % 3600) / 60;
    let seconds = total_seconds % 60;
    let total_distance_km = summary.map(|value| value.total_distance_km).unwrap_or(0.0);
    let average_speed_kph = summary.map(|value| value.average_speed_kph).unwrap_or(0.0);
    let history_link = build_history_tracking_link(&session.imei, session.created_at, off_time)
        .unwrap_or_else(|| "History link is not available yet.".to_string());
    let latest_map_link = latest_location_link(latest_location)
        .unwrap_or_else(|| "Latest map link is not available yet.".to_string());
    let duration_compact = if hours > 0 {
        format!("{hours}h {minutes}m {seconds}s")
    } else if minutes > 0 {
        format!("{minutes}m {seconds}s")
    } else {
        format!("{seconds}s")
    };

    format!(
        "🏍️ Ride Summary ({})\n⏱️ {} → {} WIB ({})\n📍 {:.2} km\n⚡ Avg Speed: {:.2} km/h\n\n🗺️ History: [View Route]\n{}\n\n📌 Last Location: [Open in Google Maps]\n{}",
        started_wib.format("%d %b %Y"),
        started_wib.format("%H:%M"),
        off_wib.format("%H:%M"),
        duration_compact,
        total_distance_km,
        average_speed_kph,
        history_link,
        latest_map_link,
    )
}

fn build_history_tracking_link(
    imei: &str,
    start_at: DateTime<Utc>,
    end_at: DateTime<Utc>,
) -> Option<String> {
    let mut url = reqwest::Url::parse(&format!("{LIVE_TRACKING_BASE_URL}/{imei}")).ok()?;
    let start_at = start_at.format("%Y-%m-%dT%H:%M:%SZ").to_string();
    let end_at = end_at.format("%Y-%m-%dT%H:%M:%SZ").to_string();
    url.query_pairs_mut().append_pair("start_at", &start_at);
    url.query_pairs_mut().append_pair("end_at", &end_at);
    Some(url.into())
}

fn latest_location_link(location: Option<&StoredLocation>) -> Option<String> {
    let location = location?;
    let latitude = location.latitude?;
    let longitude = location.longitude?;
    Some(format!(
        "https://maps.google.com/?q={latitude:.6},{longitude:.6}"
    ))
}

fn format_latest_motor_status_message_at(
    session: &EngineSession,
    heartbeat: Option<&StoredHeartbeat>,
    location: Option<&StoredLocation>,
    reference_time: DateTime<Utc>,
) -> String {
    let wib = FixedOffset::east_opt(WIB_OFFSET_SECONDS).expect("valid WIB offset");
    let map_link = latest_location_link(location)
        .unwrap_or_else(|| "Location is not available yet.".to_string());
    let engine_status = heartbeat
        .map(|value| match value.engine_status_guess.as_str() {
            "on" => "ON",
            "off" => "OFF",
            _ => "UNKNOWN",
        })
        .unwrap_or("UNKNOWN");
    let movement_status = match location.and_then(|value| value.speed_kph) {
        Some(speed) if speed > 0 => format!("Moving ({speed} km/h)"),
        Some(_) => "Stationary".to_string(),
        None => "Stationary".to_string(),
    };
    let signal_status = heartbeat
        .map(|value| connection_status_label(value.gsm_signal_strength))
        .unwrap_or("Unknown");
    let gps_tracking_status = heartbeat
        .map(|value| if value.gps_tracking_on { "Active" } else { "Inactive" })
        .unwrap_or("Unknown");
    let battery_level = heartbeat
        .map(|value| gps_battery_label(value.voltage_level).to_string())
        .unwrap_or_else(|| "Unknown".to_string());
    let last_update = heartbeat
        .map(|value| value.server_received_at)
        .into_iter()
        .chain(location.and_then(|value| value.last_seen_at))
        .max()
        .map(|value| format_relative_time_compact(reference_time, value))
        .unwrap_or_else(|| "unknown".to_string());
    let battery_warning = heartbeat
        .filter(|value| value.voltage_level == 0)
        .map(|_| {
            "\n\nWarning: GPS device battery is empty. New updates may only arrive after the motor turns on again."
        })
        .unwrap_or("");
    let session_started_wib = wib.from_utc_datetime(&session.created_at.naive_utc());
    let session_status = session.session_status.as_str();
    let session_ended = session
        .resolved_at
        .map(|value| {
            let value = wib.from_utc_datetime(&value.naive_utc());
            value.format("%H:%M:%S WIB").to_string()
        })
        .unwrap_or_else(|| "ONGOING".to_string());
    let report_time_wib = wib.from_utc_datetime(&reference_time.naive_utc());

    format!(
        "🛠️ Motor Diagnostics Report\n📅 {} — {} WIB\n\n📍 Last Known Location\n{}\n\nStatus: {}\nLast Update: {}\n\n⚙️ Engine Status\n\nEngine: {}\n\n📡 GPS & Tracking\n\nGPS Signal: {}\nTracking Mode: {}\nPower Level: {}\n\n🧾 Session Info\n\nSession State: {}\nStarted: {}\nEnded: {}{}",
        report_time_wib.format("%d %b %Y"),
        report_time_wib.format("%H:%M"),
        map_link,
        movement_status.to_uppercase(),
        last_update,
        engine_status,
        signal_status.to_uppercase(),
        gps_tracking_status.to_uppercase(),
        battery_level.to_uppercase(),
        session_status.to_uppercase(),
        session_started_wib.format("%H:%M:%S WIB"),
        session_ended,
        battery_warning,
    )
}

fn format_relative_time_compact(reference_time: DateTime<Utc>, event_time: DateTime<Utc>) -> String {
    let duration = reference_time
        .signed_duration_since(event_time)
        .to_std()
        .unwrap_or_default();
    let seconds = duration.as_secs();

    match seconds {
        0..=59 => format!("{seconds}s ago"),
        60..=3599 => {
            let minutes = seconds / 60;
            format!("{minutes}m ago")
        }
        _ => {
            let hours = seconds / 3600;
            format!("{hours}h ago")
        }
    }
}

pub fn format_ride_session_status_message(
    session: &EngineSession,
    heartbeat: &StoredHeartbeat,
) -> String {
    let start = FixedOffset::east_opt(WIB_OFFSET_SECONDS)
        .expect("valid WIB offset")
        .from_utc_datetime(&session.created_at.naive_utc());
    let duration = heartbeat
        .server_received_at
        .signed_duration_since(session.created_at)
        .to_std()
        .unwrap_or_default();
    let total_seconds = duration.as_secs();
    let hours = total_seconds / 3600;
    let minutes = (total_seconds % 3600) / 60;
    let seconds = total_seconds % 60;

    format!(
        "Current Session\nYou started riding at {}.\nIt has been {:02}:{:02}:{:02} on the road so far.\nGPS tracking is currently {} and your connection quality is {}.",
        start.format("%d %b %Y • %H:%M WIB"),
        hours,
        minutes,
        seconds,
        if heartbeat.gps_tracking_on { "on" } else { "off" },
        connection_status_label(heartbeat.gsm_signal_strength),
    )
}

fn connection_status_label(gsm_signal_strength: i32) -> &'static str {
    match gsm_signal_strength.clamp(1, 4) {
        1 => "Poor",
        2 => "Fair",
        3 => "OK",
        4 => "Excellent",
        _ => "Unknown",
    }
}

fn gps_battery_label(voltage_level: i32) -> &'static str {
    match voltage_level {
        0 => "Empty",
        1 => "Very Low",
        2 => "Low",
        3 => "Medium",
        4 => "Full",
        _ => "Unknown",
    }
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

fn format_start_status_message() -> String {
    "🛵 welcome to @tryheartbeatsbot 🛣 \n\nclick /help for more information."
        .to_string()
}

fn build_status_session(
    imei: &str,
    chat_id: i64,
    heartbeat: Option<&StoredHeartbeat>,
    location: Option<&StoredLocation>,
) -> EngineSession {
    let created_at = heartbeat
        .map(|value| value.server_received_at)
        .or_else(|| location.and_then(|value| value.last_seen_at))
        .unwrap_or_else(Utc::now);

    EngineSession {
        id: 0,
        imei: imei.to_string(),
        chat_id,
        trigger_heartbeat_id: heartbeat.map(|value| value.id).unwrap_or(0),
        prompt_message_id: 0,
        ride_status_message_id: None,
        session_status: "bound".to_string(),
        created_at,
        resolved_at: None,
    }
}

fn should_start_new_engine_on_session(
    heartbeat_time: DateTime<Utc>,
    previous_on_heartbeat_time: Option<DateTime<Utc>>,
) -> bool {
    let Some(previous_on_heartbeat_time) = previous_on_heartbeat_time else {
        return true;
    };

    heartbeat_time
        .signed_duration_since(previous_on_heartbeat_time)
        .num_seconds()
        >= ENGINE_ON_ALERT_COOLDOWN_SECS
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

fn option_bool(value: Option<bool>) -> String {
    value
        .map(|v| v.to_string())
        .unwrap_or_else(|| "unknown".to_string())
}

fn haversine_distance_km(
    start_latitude: f64,
    start_longitude: f64,
    end_latitude: f64,
    end_longitude: f64,
) -> f64 {
    let earth_radius_km = 6371.0;
    let start_latitude = start_latitude.to_radians();
    let end_latitude = end_latitude.to_radians();
    let delta_latitude = (end_latitude - start_latitude).abs();
    let delta_longitude = (end_longitude - start_longitude).to_radians().abs();

    let a = (delta_latitude / 2.0).sin().powi(2)
        + start_latitude.cos() * end_latitude.cos() * (delta_longitude / 2.0).sin().powi(2);
    let c = 2.0 * a.sqrt().atan2((1.0 - a).sqrt());

    earth_radius_km * c
}

pub async fn ensure_admin_chat_id(pool: &sqlx::PgPool, chat_id: i64) -> Result<(), sqlx::Error> {
    set_state_i64(pool, "admin_chat_id", chat_id).await
}

fn parse_registration_status(value: &str) -> Option<TelegramRegistrationStatus> {
    match value {
        "awaiting_imei" => Some(TelegramRegistrationStatus::AwaitingImei),
        "bound" => Some(TelegramRegistrationStatus::Bound),
        _ => None,
    }
}

fn registration_status_value(status: &TelegramRegistrationStatus) -> &'static str {
    match status {
        TelegramRegistrationStatus::AwaitingImei => "awaiting_imei",
        TelegramRegistrationStatus::Bound => "bound",
    }
}

fn is_valid_imei(value: &str) -> bool {
    value.len() == 15 && value.bytes().all(|byte| byte.is_ascii_digit())
}

async fn fetch_telegram_user_by_user_id(
    pool: &sqlx::PgPool,
    telegram_user_id: i64,
) -> Result<Option<TelegramUserRecord>, sqlx::Error> {
    let row = sqlx::query(
        r#"
        SELECT telegram_user_id, chat_id, bound_imei, registration_status
        FROM telegram_users
        WHERE telegram_user_id = $1
        LIMIT 1
        "#,
    )
    .bind(telegram_user_id)
    .fetch_optional(pool)
    .await?;

    Ok(row.and_then(|row| {
        let registration_status = parse_registration_status(row.get::<String, _>("registration_status").as_str())?;
        Some(TelegramUserRecord {
            telegram_user_id: row.get("telegram_user_id"),
            chat_id: row.get("chat_id"),
            bound_imei: row.get("bound_imei"),
            registration_status,
        })
    }))
}

async fn fetch_telegram_user_by_chat_id(
    pool: &sqlx::PgPool,
    chat_id: i64,
) -> Result<Option<TelegramUserRecord>, sqlx::Error> {
    let row = sqlx::query(
        r#"
        SELECT telegram_user_id, chat_id, bound_imei, registration_status
        FROM telegram_users
        WHERE chat_id = $1
        LIMIT 1
        "#,
    )
    .bind(chat_id)
    .fetch_optional(pool)
    .await?;

    Ok(row.and_then(|row| {
        let registration_status = parse_registration_status(
            row.get::<String, _>("registration_status").as_str(),
        )?;
        Some(TelegramUserRecord {
            telegram_user_id: row.get("telegram_user_id"),
            chat_id: row.get("chat_id"),
            bound_imei: row.get("bound_imei"),
            registration_status,
        })
    }))
}

async fn upsert_telegram_user_registration_state(
    pool: &sqlx::PgPool,
    telegram_user_id: i64,
    chat_id: i64,
    registration_status: TelegramRegistrationStatus,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        r#"
        INSERT INTO telegram_users (
            telegram_user_id, chat_id, bound_imei, registration_status, created_at, updated_at
        )
        VALUES ($1, $2, NULL, $3, NOW(), NOW())
        ON CONFLICT (telegram_user_id) DO UPDATE
        SET chat_id = EXCLUDED.chat_id,
            registration_status = CASE
                WHEN telegram_users.bound_imei IS NULL THEN EXCLUDED.registration_status
                ELSE telegram_users.registration_status
            END,
            updated_at = EXCLUDED.updated_at
        "#,
    )
    .bind(telegram_user_id)
    .bind(chat_id)
    .bind(registration_status_value(&registration_status))
    .execute(pool)
    .await?;

    Ok(())
}

async fn bind_telegram_user_to_imei(
    pool: &sqlx::PgPool,
    telegram_user_id: i64,
    chat_id: i64,
    imei: &str,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        r#"
        UPDATE telegram_users
        SET chat_id = $2,
            bound_imei = $3,
            registration_status = 'bound',
            updated_at = NOW()
        WHERE telegram_user_id = $1
        "#,
    )
    .bind(telegram_user_id)
    .bind(chat_id)
    .bind(imei)
    .execute(pool)
    .await?;

    Ok(())
}

async fn device_exists(pool: &sqlx::PgPool, imei: &str) -> Result<bool, sqlx::Error> {
    let row = sqlx::query(
        r#"
        SELECT 1
        FROM devices
        WHERE imei = $1
        LIMIT 1
        "#,
    )
    .bind(imei)
    .fetch_optional(pool)
    .await?;

    Ok(row.is_some())
}

async fn is_device_bound_to_another_user(
    pool: &sqlx::PgPool,
    imei: &str,
    telegram_user_id: i64,
) -> Result<bool, sqlx::Error> {
    let row = sqlx::query(
        r#"
        SELECT 1
        FROM telegram_users
        WHERE bound_imei = $1
          AND telegram_user_id <> $2
        LIMIT 1
        "#,
    )
    .bind(imei)
    .bind(telegram_user_id)
    .fetch_optional(pool)
    .await?;

    Ok(row.is_some())
}

async fn fetch_notification_chat_ids_for_imei(
    pool: &sqlx::PgPool,
    imei: &str,
) -> Result<Vec<i64>, sqlx::Error> {
    let rows = sqlx::query(
        r#"
        SELECT chat_id
        FROM telegram_users
        WHERE bound_imei = $1
          AND registration_status = 'bound'
        "#,
    )
    .bind(imei)
    .fetch_all(pool)
    .await?;

    Ok(rows.into_iter().map(|row| row.get("chat_id")).collect())
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
               gps_tracking_on, acc_high, vibration_detected, engine_status_guess,
               voltage_level, gsm_signal_strength
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
            gps_tracking_on: row.get("gps_tracking_on"),
            acc_high: row.get("acc_high"),
            vibration_detected: row.get("vibration_detected"),
            engine_status_guess: row.get("engine_status_guess"),
            voltage_level: row.get("voltage_level"),
            gsm_signal_strength: row.get("gsm_signal_strength"),
        })
        .collect())
}

pub async fn fetch_heartbeat_server_received_at_by_id(
    pool: &sqlx::PgPool,
    heartbeat_id: i64,
) -> Result<Option<DateTime<Utc>>, sqlx::Error> {
    let row = sqlx::query(
        r#"
        SELECT server_received_at
        FROM device_heartbeats
        WHERE id = $1
        LIMIT 1
        "#,
    )
    .bind(heartbeat_id)
    .fetch_optional(pool)
    .await?;

    Ok(row.map(|row| row.get("server_received_at")))
}

pub async fn fetch_notification_state(
    pool: &sqlx::PgPool,
    imei: &str,
    chat_id: i64,
) -> Result<Option<NotificationState>, sqlx::Error> {
    let row = sqlx::query(
        r#"
        SELECT imei, chat_id, last_status, last_message_id, last_heartbeat_id
        FROM telegram_device_notifications
        WHERE imei = $1 AND chat_id = $2
        "#,
    )
    .bind(imei)
    .bind(chat_id)
    .fetch_optional(pool)
    .await?;

    Ok(row.map(|row| NotificationState {
        imei: row.get("imei"),
        chat_id: row.get("chat_id"),
        last_status: row.get("last_status"),
        last_message_id: row.get("last_message_id"),
        last_heartbeat_id: row.get("last_heartbeat_id"),
    }))
}

pub async fn create_engine_session(
    pool: &sqlx::PgPool,
    imei: &str,
    chat_id: i64,
    trigger_heartbeat_id: i64,
    prompt_message_id: i64,
) -> Result<i64, sqlx::Error> {
    let row = sqlx::query(
        r#"
        INSERT INTO telegram_engine_sessions (
            imei, chat_id, trigger_heartbeat_id, prompt_message_id, ride_status_message_id,
            session_status, created_at, updated_at
        )
        VALUES ($1, $2, $3, $4, NULL, 'pending_confirmation', NOW(), NOW())
        RETURNING id
        "#,
    )
    .bind(imei)
    .bind(chat_id)
    .bind(trigger_heartbeat_id)
    .bind(prompt_message_id)
    .fetch_one(pool)
    .await?;

    Ok(row.get("id"))
}

pub async fn fetch_engine_session_by_prompt_message(
    pool: &sqlx::PgPool,
    chat_id: i64,
    prompt_message_id: i64,
) -> Result<Option<EngineSession>, sqlx::Error> {
    let row = sqlx::query(
        r#"
        SELECT id, imei, chat_id, trigger_heartbeat_id, prompt_message_id, ride_status_message_id, session_status, created_at, resolved_at
        FROM telegram_engine_sessions
        WHERE chat_id = $1 AND prompt_message_id = $2
        ORDER BY id DESC
        LIMIT 1
        "#,
    )
    .bind(chat_id)
    .bind(prompt_message_id)
    .fetch_optional(pool)
    .await?;

    Ok(row.map(|row| EngineSession {
        id: row.get("id"),
        imei: row.get("imei"),
        chat_id: row.get("chat_id"),
        trigger_heartbeat_id: row.get("trigger_heartbeat_id"),
        prompt_message_id: row.get("prompt_message_id"),
        ride_status_message_id: row.get("ride_status_message_id"),
        session_status: row.get("session_status"),
        created_at: row.get("created_at"),
        resolved_at: row.get("resolved_at"),
    }))
}

pub async fn fetch_engine_session_by_id(
    pool: &sqlx::PgPool,
    session_id: i64,
) -> Result<Option<EngineSession>, sqlx::Error> {
    let row = sqlx::query(
        r#"
        SELECT id, imei, chat_id, trigger_heartbeat_id, prompt_message_id, ride_status_message_id, session_status, created_at, resolved_at
        FROM telegram_engine_sessions
        WHERE id = $1
        LIMIT 1
        "#,
    )
    .bind(session_id)
    .fetch_optional(pool)
    .await?;

    Ok(row.map(|row| EngineSession {
        id: row.get("id"),
        imei: row.get("imei"),
        chat_id: row.get("chat_id"),
        trigger_heartbeat_id: row.get("trigger_heartbeat_id"),
        prompt_message_id: row.get("prompt_message_id"),
        ride_status_message_id: row.get("ride_status_message_id"),
        session_status: row.get("session_status"),
        created_at: row.get("created_at"),
        resolved_at: row.get("resolved_at"),
    }))
}

pub async fn fetch_latest_engine_session_for_imei_chat(
    pool: &sqlx::PgPool,
    imei: &str,
    chat_id: i64,
) -> Result<Option<EngineSession>, sqlx::Error> {
    let row = sqlx::query(
        r#"
        SELECT id, imei, chat_id, trigger_heartbeat_id, prompt_message_id, ride_status_message_id, session_status, created_at, resolved_at
        FROM telegram_engine_sessions
        WHERE imei = $1
          AND chat_id = $2
        ORDER BY created_at DESC, id DESC
        LIMIT 1
        "#,
    )
    .bind(imei)
    .bind(chat_id)
    .fetch_optional(pool)
    .await?;

    Ok(row.map(|row| EngineSession {
        id: row.get("id"),
        imei: row.get("imei"),
        chat_id: row.get("chat_id"),
        trigger_heartbeat_id: row.get("trigger_heartbeat_id"),
        prompt_message_id: row.get("prompt_message_id"),
        ride_status_message_id: row.get("ride_status_message_id"),
        session_status: row.get("session_status"),
        created_at: row.get("created_at"),
        resolved_at: row.get("resolved_at"),
    }))
}

pub async fn resolve_engine_session(
    pool: &sqlx::PgPool,
    session_id: i64,
    session_status: &str,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        r#"
        UPDATE telegram_engine_sessions
        SET session_status = $2,
            updated_at = NOW(),
            resolved_at = NOW()
        WHERE id = $1
        "#,
    )
    .bind(session_id)
    .bind(session_status)
    .execute(pool)
    .await?;

    Ok(())
}

pub async fn set_engine_session_ride_status_message_id(
    pool: &sqlx::PgPool,
    session_id: i64,
    ride_status_message_id: i64,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        r#"
        UPDATE telegram_engine_sessions
        SET ride_status_message_id = $2,
            updated_at = NOW()
        WHERE id = $1
        "#,
    )
    .bind(session_id)
    .bind(ride_status_message_id)
    .execute(pool)
    .await?;

    Ok(())
}

pub async fn fetch_active_confirmed_safe_session(
    pool: &sqlx::PgPool,
    imei: &str,
    chat_id: i64,
) -> Result<Option<EngineSession>, sqlx::Error> {
    let row = sqlx::query(
        r#"
        SELECT id, imei, chat_id, trigger_heartbeat_id, prompt_message_id, ride_status_message_id, session_status, created_at, resolved_at
        FROM telegram_engine_sessions
        WHERE imei = $1
          AND chat_id = $2
          AND session_status = 'confirmed_safe'
        ORDER BY id DESC
        LIMIT 1
        "#,
    )
    .bind(imei)
    .bind(chat_id)
    .fetch_optional(pool)
    .await?;

    Ok(row.map(|row| EngineSession {
        id: row.get("id"),
        imei: row.get("imei"),
        chat_id: row.get("chat_id"),
        trigger_heartbeat_id: row.get("trigger_heartbeat_id"),
        prompt_message_id: row.get("prompt_message_id"),
        ride_status_message_id: row.get("ride_status_message_id"),
        session_status: row.get("session_status"),
        created_at: row.get("created_at"),
        resolved_at: row.get("resolved_at"),
    }))
}

pub async fn fetch_active_reported_theft_session(
    pool: &sqlx::PgPool,
    imei: &str,
    chat_id: i64,
) -> Result<Option<EngineSession>, sqlx::Error> {
    let row = sqlx::query(
        r#"
        SELECT id, imei, chat_id, trigger_heartbeat_id, prompt_message_id, ride_status_message_id, session_status, created_at, resolved_at
        FROM telegram_engine_sessions
        WHERE imei = $1
          AND chat_id = $2
          AND session_status = 'reported_theft'
        ORDER BY id DESC
        LIMIT 1
        "#,
    )
    .bind(imei)
    .bind(chat_id)
    .fetch_optional(pool)
    .await?;

    Ok(row.map(|row| EngineSession {
        id: row.get("id"),
        imei: row.get("imei"),
        chat_id: row.get("chat_id"),
        trigger_heartbeat_id: row.get("trigger_heartbeat_id"),
        prompt_message_id: row.get("prompt_message_id"),
        ride_status_message_id: row.get("ride_status_message_id"),
        session_status: row.get("session_status"),
        created_at: row.get("created_at"),
        resolved_at: row.get("resolved_at"),
    }))
}

pub async fn fetch_latest_theft_off_session(
    pool: &sqlx::PgPool,
    imei: &str,
    chat_id: i64,
) -> Result<Option<EngineSession>, sqlx::Error> {
    let row = sqlx::query(
        r#"
        SELECT id, imei, chat_id, trigger_heartbeat_id, prompt_message_id, ride_status_message_id, session_status, created_at, resolved_at
        FROM telegram_engine_sessions
        WHERE imei = $1
          AND chat_id = $2
          AND session_status = 'theft_engine_off'
        ORDER BY id DESC
        LIMIT 1
        "#,
    )
    .bind(imei)
    .bind(chat_id)
    .fetch_optional(pool)
    .await?;

    Ok(row.map(|row| EngineSession {
        id: row.get("id"),
        imei: row.get("imei"),
        chat_id: row.get("chat_id"),
        trigger_heartbeat_id: row.get("trigger_heartbeat_id"),
        prompt_message_id: row.get("prompt_message_id"),
        ride_status_message_id: row.get("ride_status_message_id"),
        session_status: row.get("session_status"),
        created_at: row.get("created_at"),
        resolved_at: row.get("resolved_at"),
    }))
}

pub async fn fetch_active_pending_session(
    pool: &sqlx::PgPool,
    imei: &str,
    chat_id: i64,
) -> Result<Option<EngineSession>, sqlx::Error> {
    let row = sqlx::query(
        r#"
        SELECT id, imei, chat_id, trigger_heartbeat_id, prompt_message_id, ride_status_message_id, session_status, created_at, resolved_at
        FROM telegram_engine_sessions
        WHERE imei = $1
          AND chat_id = $2
          AND session_status = 'pending_confirmation'
        ORDER BY id DESC
        LIMIT 1
        "#,
    )
    .bind(imei)
    .bind(chat_id)
    .fetch_optional(pool)
    .await?;

    Ok(row.map(|row| EngineSession {
        id: row.get("id"),
        imei: row.get("imei"),
        chat_id: row.get("chat_id"),
        trigger_heartbeat_id: row.get("trigger_heartbeat_id"),
        prompt_message_id: row.get("prompt_message_id"),
        ride_status_message_id: row.get("ride_status_message_id"),
        session_status: row.get("session_status"),
        created_at: row.get("created_at"),
        resolved_at: row.get("resolved_at"),
    }))
}

pub async fn fetch_active_pending_sessions(
    pool: &sqlx::PgPool,
    imei: &str,
    chat_id: i64,
) -> Result<Vec<EngineSession>, sqlx::Error> {
    let rows = sqlx::query(
        r#"
        SELECT id, imei, chat_id, trigger_heartbeat_id, prompt_message_id, ride_status_message_id, session_status, created_at, resolved_at
        FROM telegram_engine_sessions
        WHERE imei = $1
          AND chat_id = $2
          AND session_status = 'pending_confirmation'
        ORDER BY created_at DESC, id DESC
        "#,
    )
    .bind(imei)
    .bind(chat_id)
    .fetch_all(pool)
    .await?;

    Ok(rows
        .into_iter()
        .map(|row| EngineSession {
            id: row.get("id"),
            imei: row.get("imei"),
            chat_id: row.get("chat_id"),
            trigger_heartbeat_id: row.get("trigger_heartbeat_id"),
            prompt_message_id: row.get("prompt_message_id"),
            ride_status_message_id: row.get("ride_status_message_id"),
            session_status: row.get("session_status"),
            created_at: row.get("created_at"),
            resolved_at: row.get("resolved_at"),
        })
        .collect())
}

pub async fn upsert_notification_state(
    pool: &sqlx::PgPool,
    imei: &str,
    chat_id: i64,
    last_status: &str,
    last_message_id: i64,
    last_heartbeat_id: i64,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        r#"
        INSERT INTO telegram_device_notifications (
            imei, chat_id, last_status, last_message_id, last_heartbeat_id, updated_at
        )
        VALUES ($1, $2, $3, $4, $5, NOW())
        ON CONFLICT (imei, chat_id) DO UPDATE
        SET last_status = EXCLUDED.last_status,
            last_message_id = EXCLUDED.last_message_id,
            last_heartbeat_id = EXCLUDED.last_heartbeat_id,
            updated_at = EXCLUDED.updated_at
        "#,
    )
    .bind(imei)
    .bind(chat_id)
    .bind(last_status)
    .bind(last_message_id)
    .bind(last_heartbeat_id)
    .execute(pool)
    .await?;

    Ok(())
}

pub async fn fetch_latest_heartbeat(
    pool: &sqlx::PgPool,
) -> Result<Option<StoredHeartbeat>, sqlx::Error> {
    let row = sqlx::query(
        r#"
        SELECT id, imei, server_received_at, terminal_info_raw, terminal_info_bits,
               gps_tracking_on, acc_high, vibration_detected, engine_status_guess,
               voltage_level, gsm_signal_strength
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
        gps_tracking_on: row.get("gps_tracking_on"),
        acc_high: row.get("acc_high"),
        vibration_detected: row.get("vibration_detected"),
        engine_status_guess: row.get("engine_status_guess"),
        voltage_level: row.get("voltage_level"),
        gsm_signal_strength: row.get("gsm_signal_strength"),
    }))
}

pub async fn fetch_latest_heartbeat_for_imei(
    pool: &sqlx::PgPool,
    imei: &str,
) -> Result<Option<StoredHeartbeat>, sqlx::Error> {
    let row = sqlx::query(
        r#"
        SELECT id, imei, server_received_at, terminal_info_raw, terminal_info_bits,
               gps_tracking_on, acc_high, vibration_detected, engine_status_guess,
               voltage_level, gsm_signal_strength
        FROM device_heartbeats
        WHERE imei = $1
        ORDER BY id DESC
        LIMIT 1
        "#,
    )
    .bind(imei)
    .fetch_optional(pool)
    .await?;

    Ok(row.map(|row| StoredHeartbeat {
        id: row.get("id"),
        imei: row.get("imei"),
        server_received_at: row.get("server_received_at"),
        terminal_info_raw: row.get("terminal_info_raw"),
        terminal_info_bits: row.get("terminal_info_bits"),
        gps_tracking_on: row.get("gps_tracking_on"),
        acc_high: row.get("acc_high"),
        vibration_detected: row.get("vibration_detected"),
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

pub async fn fetch_latest_location_for_imei(
    pool: &sqlx::PgPool,
    imei: &str,
) -> Result<Option<StoredLocation>, sqlx::Error> {
    let row = sqlx::query(
        r#"
        SELECT imei, last_seen_at, latest_gps_timestamp, latest_latitude, latest_longitude,
               latest_speed_kph, latest_course, latest_satellite_count
        FROM devices
        WHERE imei = $1
        LIMIT 1
        "#,
    )
    .bind(imei)
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

pub async fn fetch_ride_summary(
    pool: &sqlx::PgPool,
    imei: &str,
    started_at: DateTime<Utc>,
    ended_at: DateTime<Utc>,
) -> Result<Option<RideSummary>, sqlx::Error> {
    let rows = sqlx::query(
        r#"
        SELECT latitude, longitude
        FROM device_locations
        WHERE imei = $1
          AND server_received_at >= $2
          AND server_received_at <= $3
        ORDER BY server_received_at ASC
        "#,
    )
    .bind(imei)
    .bind(started_at)
    .bind(ended_at)
    .fetch_all(pool)
    .await?;

    if rows.len() < 2 {
        return Ok(Some(RideSummary {
            total_distance_km: 0.0,
            average_speed_kph: 0.0,
        }));
    }

    let mut total_distance_km = 0.0;
    let mut previous: Option<(f64, f64)> = None;

    for row in rows {
        let latitude: f64 = row.get("latitude");
        let longitude: f64 = row.get("longitude");

        if let Some((previous_latitude, previous_longitude)) = previous {
            total_distance_km += haversine_distance_km(
                previous_latitude,
                previous_longitude,
                latitude,
                longitude,
            );
        }

        previous = Some((latitude, longitude));
    }

    let duration_hours = ended_at
        .signed_duration_since(started_at)
        .to_std()
        .unwrap_or_default()
        .as_secs_f64()
        / 3600.0;
    let average_speed_kph = if duration_hours > 0.0 {
        total_distance_km / duration_hours
    } else {
        0.0
    };

    Ok(Some(RideSummary {
        total_distance_km,
        average_speed_kph,
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
        assert_eq!(BotCommand::parse("/latest_location@my_bot"), Some(BotCommand::Unknown("/latest_location".to_string())));
        assert_eq!(BotCommand::parse("hello"), None);
    }

    #[test]
    fn validates_imei_format() {
        assert!(is_valid_imei("866221070478388"));
        assert!(!is_valid_imei("86622107047838"));
        assert!(!is_valid_imei("86622107047838A"));
    }

    #[test]
    fn parses_session_actions() {
        assert_eq!(SessionAction::parse("engine_session:yes"), Some(SessionAction::Yes));
        assert_eq!(SessionAction::parse("engine_session:no"), Some(SessionAction::No));
        assert_eq!(SessionAction::parse("engine_session:maybe"), None);
    }

    #[test]
    fn parses_theft_alert_actions() {
        assert_eq!(
            TheftAlertAction::parse("theft_alert:stream_location:12"),
            Some(TheftAlertAction::StreamLocation { session_id: Some(12) })
        );
        assert_eq!(
            TheftAlertAction::parse("theft_alert:check_latest_status:9"),
            Some(TheftAlertAction::CheckLatestStatus { session_id: Some(9) })
        );
        assert_eq!(
            TheftAlertAction::parse("theft_alert:contact_support:5"),
            Some(TheftAlertAction::ContactSupport { session_id: Some(5) })
        );
        assert_eq!(
            TheftAlertAction::parse("theft_alert:stream_location"),
            Some(TheftAlertAction::StreamLocation { session_id: None })
        );
        assert_eq!(TheftAlertAction::parse("theft_alert:record_sound:1"), None);
    }

    #[test]
    fn formats_heartbeat_notification_message() {
        let heartbeat = StoredHeartbeat {
            id: 1,
            imei: "866221070478388".to_string(),
            server_received_at: Utc.with_ymd_and_hms(2026, 4, 13, 12, 0, 0).unwrap(),
            terminal_info_raw: 69,
            terminal_info_bits: "01000101".to_string(),
            gps_tracking_on: true,
            acc_high: Some(true),
            vibration_detected: true,
            engine_status_guess: "on".to_string(),
            voltage_level: 6,
            gsm_signal_strength: 3,
        };

        let text = format_heartbeat_notification(&heartbeat);
        assert!(text.contains("866221070478388"));
        assert!(text.contains("01000101"));
        assert!(text.contains("heuristic"));
    }

    #[test]
    fn formats_engine_status_notification_message() {
        let heartbeat = StoredHeartbeat {
            id: 1,
            imei: "866221070478388".to_string(),
            server_received_at: Utc.with_ymd_and_hms(2026, 4, 15, 9, 5, 0).unwrap(),
            terminal_info_raw: 69,
            terminal_info_bits: "01000101".to_string(),
            gps_tracking_on: true,
            acc_high: Some(true),
            vibration_detected: true,
            engine_status_guess: "on".to_string(),
            voltage_level: 6,
            gsm_signal_strength: 3,
        };

        let on_text = format_engine_status_notification(&heartbeat, "on");
        assert!(on_text.contains("Motor Dinyalakan"));
        assert!(on_text.contains("15 Apr 2026"));
        assert!(on_text.contains("16:05 WIB"));

        let off_text = format_engine_status_notification(&heartbeat, "off");
        assert!(off_text.contains("Motor Dimatikan"));
    }

    #[test]
    fn formats_theft_warning_message() {
        let text = format_theft_warning_message();
        assert!(text.contains("Pay attention"));
        assert!(text.contains("Use button below to track your motor"));
    }

    #[test]
    fn formats_theft_location_message_without_location() {
        let text = format_theft_location_message(None);
        assert!(text.contains("Latest location"));
        assert!(text.contains("Lokasi terakhir belum tersedia"));
    }

    #[test]
    fn formats_theft_engine_off_message() {
        let text = format_theft_engine_off_message();
        assert!(text.contains("Motor terdeteksi mati (berhenti)"));
        assert!(text.contains("battery mode"));
    }

    #[test]
    fn formats_theft_engine_off_follow_up_message() {
        let started = Utc.with_ymd_and_hms(2026, 4, 17, 10, 0, 0).unwrap();
        let newest = Utc.with_ymd_and_hms(2026, 4, 17, 10, 5, 7).unwrap();

        let text = format_theft_engine_off_follow_up_message(started, newest);
        assert!(text.contains("Motor terdeteksi mati (berhenti)"));
        assert!(text.contains("-> now"));
        assert!(text.contains("Duration: 00:05:07"));
    }

    #[test]
    fn formats_ride_summary_message() {
        let session = EngineSession {
            id: 1,
            imei: "866221070478388".to_string(),
            chat_id: 12345,
            trigger_heartbeat_id: 7,
            prompt_message_id: 99,
            ride_status_message_id: None,
            session_status: "finished".to_string(),
            created_at: Utc.with_ymd_and_hms(2026, 4, 17, 10, 0, 0).unwrap(),
            resolved_at: Some(Utc.with_ymd_and_hms(2026, 4, 17, 10, 5, 0).unwrap()),
        };
        let summary = RideSummary {
            total_distance_km: 3.25,
            average_speed_kph: 39.0,
        };
        let latest_location = StoredLocation {
            imei: "866221070478388".to_string(),
            last_seen_at: Some(Utc.with_ymd_and_hms(2026, 4, 17, 10, 5, 0).unwrap()),
            gps_timestamp: None,
            latitude: Some(-6.204066),
            longitude: Some(106.785514),
            speed_kph: Some(0),
            course: None,
            satellite_count: None,
        };

        let text = format_ride_summary_message(
            &session,
            Utc.with_ymd_and_hms(2026, 4, 17, 10, 5, 0).unwrap(),
            Some(&summary),
            Some(&latest_location),
        );
        assert!(text.contains("🏍️ Ride Summary (17 Apr 2026)"));
        assert!(text.contains("⏱️ 17:00 → 17:05 WIB (5m 0s)"));
        assert!(text.contains("📍 3.25 km"));
        assert!(text.contains("⚡ Avg Speed: 39.00 km/h"));
        assert!(text.contains("🗺️ History: [View Route]"));
        assert!(text.contains("https://hearthbeats-client.vercel.app/live-tracking/866221070478388?start_at=2026-04-17T10%3A00%3A00Z&end_at=2026-04-17T10%3A05%3A00Z"));
        assert!(text.contains("📌 Last Location: [Open in Google Maps]"));
        assert!(text.contains("https://maps.google.com/?q=-6.204066,106.785514"));
    }

    #[test]
    fn computes_haversine_distance() {
        let distance = haversine_distance_km(-6.204066, 106.785514, -6.204500, 106.786000);
        assert!(distance > 0.05);
    }

    #[test]
    fn formats_stream_location_message() {
        let text = format_stream_location_message(Some(
            "https://hearthbeats-client.vercel.app/live-tracking/866221070478388?start_at=2026-04-18T10%3A00%3A00Z",
        ));
        assert!(text.contains("📍 Live Tracking Activated"));
        assert!(text.contains("Track your motor in real-time using this link:"));
        assert!(text.contains("https://hearthbeats-client.vercel.app/live-tracking/866221070478388?start_at=2026-04-18T10%3A00%3A00Z"));
        assert!(text.contains("🔗 This link is shareable"));
    }

    #[test]
    fn builds_live_tracking_link_with_encoded_start_at() {
        let start_at = Utc.with_ymd_and_hms(2026, 4, 18, 10, 0, 0).unwrap();
        let link = build_live_tracking_link("866221070478388", start_at).expect("link");

        assert_eq!(
            link,
            "https://hearthbeats-client.vercel.app/live-tracking/866221070478388?start_at=2026-04-18T10%3A00%3A00Z"
        );
    }

    #[test]
    fn builds_history_tracking_link_with_encoded_start_and_end_at() {
        let start_at = Utc.with_ymd_and_hms(2026, 4, 18, 10, 0, 0).unwrap();
        let end_at = Utc.with_ymd_and_hms(2026, 4, 18, 11, 0, 0).unwrap();
        let link =
            build_history_tracking_link("866221070478388", start_at, end_at).expect("link");

        assert_eq!(
            link,
            "https://hearthbeats-client.vercel.app/live-tracking/866221070478388?start_at=2026-04-18T10%3A00%3A00Z&end_at=2026-04-18T11%3A00%3A00Z"
        );
    }

    #[test]
    fn selects_stream_location_start_time_preferring_explicit_session() {
        let explicit_session_created_at =
            Some(Utc.with_ymd_and_hms(2026, 4, 18, 10, 0, 0).unwrap());
        let latest_session_created_at =
            Some(Utc.with_ymd_and_hms(2026, 4, 18, 9, 30, 0).unwrap());
        let latest_location_last_seen_at =
            Some(Utc.with_ymd_and_hms(2026, 4, 18, 9, 45, 0).unwrap());

        let start_at = select_stream_location_start_at(
            explicit_session_created_at,
            latest_session_created_at,
            latest_location_last_seen_at,
        );

        assert_eq!(start_at, explicit_session_created_at);
    }

    #[test]
    fn selects_stream_location_start_time_preferring_latest_session_for_start_menu() {
        let latest_session_created_at =
            Some(Utc.with_ymd_and_hms(2026, 4, 18, 9, 30, 0).unwrap());
        let latest_location_last_seen_at =
            Some(Utc.with_ymd_and_hms(2026, 4, 18, 9, 45, 0).unwrap());

        let start_at = select_stream_location_start_at(
            None,
            latest_session_created_at,
            latest_location_last_seen_at,
        );

        assert_eq!(start_at, latest_session_created_at);
    }

    #[test]
    fn selects_stream_location_start_time_falling_back_to_location_last_seen() {
        let latest_location_last_seen_at =
            Some(Utc.with_ymd_and_hms(2026, 4, 18, 9, 45, 0).unwrap());

        let start_at =
            select_stream_location_start_at(None, None, latest_location_last_seen_at);

        assert_eq!(start_at, latest_location_last_seen_at);
    }

    #[test]
    fn selects_stream_location_start_time_none_when_all_sources_missing() {
        let start_at = select_stream_location_start_at(None, None, None);

        assert_eq!(start_at, None);
    }

    #[test]
    fn starts_new_engine_on_session_when_no_previous_on_heartbeat() {
        let heartbeat_time = Utc.with_ymd_and_hms(2026, 4, 19, 10, 0, 0).unwrap();
        assert!(should_start_new_engine_on_session(heartbeat_time, None));
    }

    #[test]
    fn keeps_existing_engine_on_session_within_gap_window() {
        let previous_on_heartbeat = Utc.with_ymd_and_hms(2026, 4, 19, 10, 0, 0).unwrap();
        let heartbeat_time = Utc.with_ymd_and_hms(2026, 4, 19, 10, 4, 59).unwrap();

        assert!(!should_start_new_engine_on_session(
            heartbeat_time,
            Some(previous_on_heartbeat)
        ));
    }

    #[test]
    fn starts_new_engine_on_session_at_exact_gap_threshold() {
        let previous_on_heartbeat = Utc.with_ymd_and_hms(2026, 4, 19, 10, 0, 0).unwrap();
        let heartbeat_time = Utc.with_ymd_and_hms(2026, 4, 19, 10, 15, 0).unwrap();

        assert!(should_start_new_engine_on_session(
            heartbeat_time,
            Some(previous_on_heartbeat)
        ));
    }

    #[test]
    fn starts_new_engine_on_session_after_gap_threshold() {
        let previous_on_heartbeat = Utc.with_ymd_and_hms(2026, 4, 19, 10, 0, 0).unwrap();
        let heartbeat_time = Utc.with_ymd_and_hms(2026, 4, 19, 10, 15, 1).unwrap();

        assert!(should_start_new_engine_on_session(
            heartbeat_time,
            Some(previous_on_heartbeat)
        ));
    }

    #[test]
    fn formats_latest_motor_status_message() {
        let session = EngineSession {
            id: 1,
            imei: "866221070478388".to_string(),
            chat_id: 12345,
            trigger_heartbeat_id: 7,
            prompt_message_id: 99,
            ride_status_message_id: None,
            session_status: "theft_engine_off".to_string(),
            created_at: Utc.with_ymd_and_hms(2026, 4, 17, 10, 0, 0).unwrap(),
            resolved_at: None,
        };
        let heartbeat = StoredHeartbeat {
            id: 8,
            imei: "866221070478388".to_string(),
            server_received_at: Utc.with_ymd_and_hms(2026, 4, 17, 10, 5, 7).unwrap(),
            terminal_info_raw: 69,
            terminal_info_bits: "01000101".to_string(),
            gps_tracking_on: true,
            acc_high: Some(true),
            vibration_detected: true,
            engine_status_guess: "off".to_string(),
            voltage_level: 6,
            gsm_signal_strength: 3,
        };
        let location = StoredLocation {
            imei: "866221070478388".to_string(),
            last_seen_at: Some(Utc.with_ymd_and_hms(2026, 4, 17, 10, 4, 49).unwrap()),
            gps_timestamp: None,
            latitude: Some(-6.204066),
            longitude: Some(106.785514),
            speed_kph: Some(0),
            course: None,
            satellite_count: None,
        };

        let text = format_latest_motor_status_initial_message(
            &session,
            Some(&heartbeat),
            Some(&location),
            Utc.with_ymd_and_hms(2026, 4, 17, 10, 5, 19).unwrap(),
        );
        assert!(text.contains("🛠️ Motor Diagnostics Report"));
        assert!(text.contains("📍 Last Known Location"));
        assert!(text.contains("https://maps.google.com/?q=-6.204066,106.785514"));
        assert!(text.contains("Status: STATIONARY"));
        assert!(text.contains("Last Update: 12s ago"));
        assert!(text.contains("Engine: OFF"));
        assert!(text.contains("GPS Signal: OK"));
        assert!(text.contains("Tracking Mode: ACTIVE"));
        assert!(text.contains("Power Level: UNKNOWN"));
        assert!(text.contains("Session State: THEFT_ENGINE_OFF"));
    }

    #[test]
    fn formats_initial_latest_motor_status_message() {
        let session = EngineSession {
            id: 1,
            imei: "866221070478388".to_string(),
            chat_id: 12345,
            trigger_heartbeat_id: 7,
            prompt_message_id: 99,
            ride_status_message_id: None,
            session_status: "reported_theft".to_string(),
            created_at: Utc.with_ymd_and_hms(2026, 4, 17, 10, 0, 0).unwrap(),
            resolved_at: None,
        };
        let requested_at = Utc.with_ymd_and_hms(2026, 4, 17, 10, 1, 2).unwrap();

        let text =
            format_latest_motor_status_initial_message(&session, None, None, requested_at);
        assert!(text.contains("🛠️ Motor Diagnostics Report"));
        assert!(text.contains("📍 Last Known Location"));
        assert!(text.contains("Location is not available yet."));
        assert!(text.contains("Status: STATIONARY"));
        assert!(text.contains("Last Update: unknown"));
        assert!(text.contains("Session State: REPORTED_THEFT"));
    }

    #[test]
    fn formats_health_check_battery_warning() {
        let session = EngineSession {
            id: 1,
            imei: "866221070478388".to_string(),
            chat_id: 12345,
            trigger_heartbeat_id: 7,
            prompt_message_id: 99,
            ride_status_message_id: None,
            session_status: "bound".to_string(),
            created_at: Utc.with_ymd_and_hms(2026, 4, 17, 10, 0, 0).unwrap(),
            resolved_at: None,
        };
        let heartbeat = StoredHeartbeat {
            id: 8,
            imei: "866221070478388".to_string(),
            server_received_at: Utc.with_ymd_and_hms(2026, 4, 17, 10, 5, 0).unwrap(),
            terminal_info_raw: 69,
            terminal_info_bits: "01000101".to_string(),
            gps_tracking_on: true,
            acc_high: Some(true),
            vibration_detected: true,
            engine_status_guess: "off".to_string(),
            voltage_level: 0,
            gsm_signal_strength: 3,
        };

        let text = format_latest_motor_status_initial_message(
            &session,
            Some(&heartbeat),
            None,
            Utc.with_ymd_and_hms(2026, 4, 17, 16, 5, 0).unwrap(),
        );
        assert!(text.contains("Power Level: EMPTY"));
        assert!(text.contains("Warning: GPS device battery is empty."));
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
        sqlx::query("TRUNCATE telegram_bot_state, telegram_device_notifications RESTART IDENTITY")
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
            "TRUNCATE telegram_bot_state, telegram_device_notifications, device_heartbeats, device_locations, devices RESTART IDENTITY CASCADE",
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
                terminal_info_bits, gps_tracking_on, bit_1_guess, acc_high, bit_3_guess,
                vibration_detected, bit_4_guess, engine_status_guess, voltage_level,
                gsm_signal_strength, alarm_language
            )
            VALUES
                (1, '866221070478388', NOW(), 19, '127.0.0.1:5000', 69, '01000101', true, false, true, false, true, false, 'on', 6, 3, 2),
                (1, '866221070478388', NOW(), 19, '127.0.0.1:5000', 65, '01000001', true, false, false, false, true, false, 'off', 6, 2, 2)
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

    #[tokio::test]
    async fn stores_and_restores_notification_state(
    ) -> Result<(), Box<dyn std::error::Error>> {
        let Some(database_url) = database_url() else {
            return Ok(());
        };

        let config = Config::from_pairs([
            ("DATABASE_URL", database_url.as_str()),
            ("DATABASE_MAX_CONNECTIONS", "1"),
        ]);
        let database = Database::connect(&config).await?.expect("database configured");
        sqlx::query("TRUNCATE telegram_device_notifications RESTART IDENTITY")
            .execute(database.pool())
            .await?;

        upsert_notification_state(
            database.pool(),
            "866221070478388",
            12345,
            "on",
            777,
            55,
        )
        .await?;

        let state = fetch_notification_state(database.pool(), "866221070478388", 12345)
            .await?
            .expect("state should exist");
        assert_eq!(state.last_status, "on");
        assert_eq!(state.last_message_id, 777);
        assert_eq!(state.last_heartbeat_id, 55);

        Ok(())
    }

    #[tokio::test]
    async fn creates_and_resolves_engine_session(
    ) -> Result<(), Box<dyn std::error::Error>> {
        let Some(database_url) = database_url() else {
            return Ok(());
        };

        let config = Config::from_pairs([
            ("DATABASE_URL", database_url.as_str()),
            ("DATABASE_MAX_CONNECTIONS", "1"),
        ]);
        let database = Database::connect(&config).await?.expect("database configured");
        sqlx::query("TRUNCATE telegram_engine_sessions RESTART IDENTITY")
            .execute(database.pool())
            .await?;

        let session_id =
            create_engine_session(database.pool(), "866221070478388", 12345, 88, 999).await?;
        let session = fetch_engine_session_by_prompt_message(database.pool(), 12345, 999)
            .await?
            .expect("session should exist");
        assert_eq!(session.id, session_id);
        assert_eq!(session.session_status, "pending_confirmation");

        resolve_engine_session(database.pool(), session_id, "confirmed_safe").await?;
        let resolved = fetch_engine_session_by_prompt_message(database.pool(), 12345, 999)
            .await?
            .expect("resolved session should exist");
        assert_eq!(resolved.session_status, "confirmed_safe");

        Ok(())
    }
}
