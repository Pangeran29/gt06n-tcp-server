use std::time::Duration;

use chrono::{DateTime, Datelike, FixedOffset, NaiveDate, NaiveDateTime, TimeZone, Utc};
use reqwest::{multipart, Client};
use serde::{Deserialize, Serialize};
use sqlx::Row;
use thiserror::Error;
use tokio::time::sleep;
use tracing::{error, info, warn};

use crate::config::Config;
use crate::db::Database;
use crate::midtrans::{
    build_midtrans_order_id, create_pending_midtrans_payment,
    format_midtrans_payment_message_with_quote, mark_midtrans_payment_created, parse_pricing_tier,
    MidtransClient, MIDTRANS_BASIC_PLAN_CODE, MIDTRANS_OJOL_PLAN_CODE,
};
use crate::subscription_maintenance::build_subscription_payment_quote;
use crate::telegram_messages as messages;

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
    #[error("midtrans payment is not configured")]
    MissingMidtransConfig,
    #[error("midtrans integration failed: {0}")]
    Midtrans(#[from] crate::midtrans::MidtransError),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BotCommand {
    Start,
    Help,
    PaySupport,
    Terms,
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
enum PaymentAction {
    Subscribe,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum TheftAlertAction {
    StreamLocation { session_id: Option<i64> },
    CheckLatestStatus { session_id: Option<i64> },
    ContactSupport { session_id: Option<i64> },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AnalyticsKind {
    Sessions,
    Metrics,
    TotalKm,
    TotalDrivingTime,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AnalyticsRange {
    Select,
    Today,
    Yesterday,
    Month,
    Custom,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct AnalyticsAction {
    kind: AnalyticsKind,
    range: AnalyticsRange,
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

impl PaymentAction {
    fn parse(data: &str) -> Option<Self> {
        let mut parts = data.split(':');
        let prefix = parts.next()?;
        let action = parts.next()?;

        if prefix != "payment" {
            return None;
        }

        match action {
            "subscribe" if parts.next().is_none() => Some(Self::Subscribe),
            "buy" if parts.next() == Some("monthly") && parts.next().is_none() => {
                Some(Self::Subscribe)
            }
            _ => None,
        }
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

    fn requires_active_subscription(&self) -> bool {
        matches!(
            self,
            Self::StreamLocation { .. } | Self::CheckLatestStatus { .. }
        )
    }
}

impl AnalyticsKind {
    fn parse(value: &str) -> Option<Self> {
        Some(match value {
            "sessions" => Self::Sessions,
            "metrics" => Self::Metrics,
            "km" => Self::TotalKm,
            "time" => Self::TotalDrivingTime,
            _ => return None,
        })
    }

    fn callback_value(self) -> &'static str {
        match self {
            Self::Sessions => "sessions",
            Self::Metrics => "metrics",
            Self::TotalKm => "km",
            Self::TotalDrivingTime => "time",
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::Sessions => "driving session",
            Self::Metrics => "metrics",
            Self::TotalKm => "total km",
            Self::TotalDrivingTime => "total driving time",
        }
    }
}

impl AnalyticsRange {
    fn parse(value: &str) -> Option<Self> {
        Some(match value {
            "select" => Self::Select,
            "today" => Self::Today,
            "yesterday" => Self::Yesterday,
            "month" => Self::Month,
            "custom" => Self::Custom,
            _ => return None,
        })
    }

    fn callback_value(self) -> &'static str {
        match self {
            Self::Select => "select",
            Self::Today => "today",
            Self::Yesterday => "yesterday",
            Self::Month => "month",
            Self::Custom => "custom",
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::Select => messages::BTN_10_RANGE_SELECT,
            Self::Today => messages::BTN_11_RANGE_TODAY,
            Self::Yesterday => messages::BTN_12_RANGE_YESTERDAY,
            Self::Month => messages::BTN_13_RANGE_THIS_MONTH,
            Self::Custom => messages::BTN_14_RANGE_CUSTOM,
        }
    }
}

impl AnalyticsAction {
    fn parse(data: &str) -> Option<Self> {
        let mut parts = data.split(':');
        let prefix = parts.next()?;
        let kind = AnalyticsKind::parse(parts.next()?)?;
        let range = AnalyticsRange::parse(parts.next()?)?;

        if prefix != "analytics" || parts.next().is_some() {
            return None;
        }

        Some(Self { kind, range })
    }
}

impl BotCommand {
    pub fn parse(text: &str) -> Option<Self> {
        let command = text.split_whitespace().next()?;
        let normalized = command.split('@').next().unwrap_or(command);

        Some(match normalized {
            "/start" => Self::Start,
            "/help" => Self::Help,
            "/paysupport" => Self::PaySupport,
            "/terms" => Self::Terms,
            other if other.starts_with('/') => Self::Unknown(other.to_string()),
            _ => return None,
        })
    }
}

#[derive(Debug, Clone)]
pub struct TelegramBot {
    database: Database,
    base_url: String,
    client: Client,
    midtrans: Option<MidtransClient>,
    poll_timeout_secs: u64,
    heartbeat_poll_interval_ms: u64,
}

const WIB_OFFSET_SECONDS: i32 = 7 * 60 * 60;
const ENGINE_ON_ALERT_COOLDOWN_SECS: i64 = 10 * 60;
const STALE_ENGINE_SESSION_TIMEOUT_SECS: i64 = ENGINE_ON_ALERT_COOLDOWN_SECS;
const RIDING_TIME_MOVING_SPEED_KPH: i32 = 3;
const RIDING_TIME_MAX_POINT_GAP_SECS: i64 = 5 * 60;
const LIVE_TRACKING_BASE_URL: &str = "https://hearthbeats-client.vercel.app/live-tracking";
const ENGINE_ON_STICKER_BYTES: &[u8] = include_bytes!("../asset/AnimatedSticker.tgs");
const BIND_SUCCESS_STICKER_BYTES: &[u8] = include_bytes!("../asset/AnimatedSticker - hi.tgs");
const NOT_SUBSCRIBED_STICKER_BYTES: &[u8] = include_bytes!("../asset/AnimatedSticker - no.tgs");
const THEFT_WARNING_STICKER_BYTES: &[u8] =
    include_bytes!("../asset/AnimatedSticker - not my motor.tgs");

impl TelegramBot {
    pub async fn from_config(config: &Config) -> Result<Self, BotError> {
        let database = Database::connect(config)
            .await?
            .ok_or(BotError::MissingDatabase)?;
        let token = config
            .telegram_bot_token
            .as_ref()
            .ok_or(BotError::MissingToken)?;

        Ok(Self {
            database,
            base_url: format!("https://api.telegram.org/bot{token}"),
            client: Client::new(),
            midtrans: MidtransClient::from_config(config),
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

            if let Err(error) = self.process_stale_engine_sessions().await {
                error!(error = %error, "stale engine session cleanup failed");
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
            let notification_recipients =
                fetch_notification_recipients_for_imei(self.database.pool(), &heartbeat.imei)
                    .await?;

            if notification_recipients.is_empty() {
                set_state_i64(
                    self.database.pool(),
                    "last_notified_heartbeat_id",
                    heartbeat.id,
                )
                .await?;
                continue;
            }

            if let Some(status) = heartbeat.notification_status() {
                for recipient in notification_recipients {
                    if recipient.has_active_subscription {
                        if let Err(error) = self
                            .process_heartbeat_notification_for_chat(
                                &heartbeat,
                                status,
                                recipient.chat_id,
                            )
                            .await
                        {
                            warn!(
                                error = %error,
                                imei = %heartbeat.imei,
                                heartbeat_id = heartbeat.id,
                                chat_id = recipient.chat_id,
                                "failed to process heartbeat notification for chat; continuing with remaining recipients"
                            );
                        }
                    } else if status == "off" {
                        if let Err(error) = self
                            .process_inactive_subscription_notification_for_chat(
                                &heartbeat,
                                status,
                                recipient.chat_id,
                            )
                            .await
                        {
                            warn!(
                                error = %error,
                                imei = %heartbeat.imei,
                                heartbeat_id = heartbeat.id,
                                chat_id = recipient.chat_id,
                                "failed to process inactive subscription heartbeat notification for chat; continuing with remaining recipients"
                            );
                        }
                        self.finish_inactive_subscription_sessions(
                            &heartbeat.imei,
                            recipient.chat_id,
                            heartbeat.server_received_at,
                        )
                        .await?;
                    } else if let Err(error) = self
                        .process_inactive_subscription_notification_for_chat(
                            &heartbeat,
                            status,
                            recipient.chat_id,
                        )
                        .await
                    {
                        warn!(
                            error = %error,
                            imei = %heartbeat.imei,
                            heartbeat_id = heartbeat.id,
                            chat_id = recipient.chat_id,
                            "failed to process inactive subscription heartbeat notification for chat; continuing with remaining recipients"
                        );
                    }
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

    async fn process_stale_engine_sessions(&self) -> Result<(), BotError> {
        let sessions = fetch_all_active_engine_sessions(self.database.pool()).await?;
        let mut session_groups =
            std::collections::BTreeMap::<(String, i64), Vec<EngineSession>>::new();

        for session in sessions {
            session_groups
                .entry((session.imei.clone(), session.chat_id))
                .or_default()
                .push(session);
        }

        let reference_time = Utc::now();
        for ((imei, chat_id), sessions) in session_groups {
            let Some(latest_heartbeat) =
                fetch_latest_heartbeat_for_imei(self.database.pool(), &imei).await?
            else {
                continue;
            };

            if !should_finish_stale_engine_session(
                reference_time,
                latest_heartbeat.server_received_at,
            ) {
                continue;
            }

            info!(
                imei = %imei,
                chat_id,
                latest_heartbeat_id = latest_heartbeat.id,
                latest_heartbeat_at = %latest_heartbeat.server_received_at,
                active_session_count = sessions.len(),
                "finishing stale engine sessions after heartbeat timeout"
            );

            self.finish_active_engine_sessions(
                chat_id,
                &imei,
                &sessions,
                latest_heartbeat.server_received_at,
                false,
            )
            .await?;
        }

        Ok(())
    }

    async fn process_inactive_subscription_notification_for_chat(
        &self,
        heartbeat: &StoredHeartbeat,
        status: &str,
        chat_id: i64,
    ) -> Result<(), BotError> {
        let existing =
            fetch_notification_state(self.database.pool(), &heartbeat.imei, chat_id).await?;

        if existing
            .as_ref()
            .map(|state| state.last_status.as_str() == status)
            .unwrap_or(false)
        {
            upsert_notification_state(
                self.database.pool(),
                &heartbeat.imei,
                chat_id,
                status,
                existing
                    .map(|state| state.last_message_id)
                    .unwrap_or_default(),
                heartbeat.id,
            )
            .await?;
            return Ok(());
        }

        let message_id = self
            .send_message_internal(
                chat_id,
                &format_inactive_subscription_engine_status_message(heartbeat, status),
                Some(subscription_payment_keyboard()),
                None,
            )
            .await?;

        upsert_notification_state(
            self.database.pool(),
            &heartbeat.imei,
            chat_id,
            status,
            message_id,
            heartbeat.id,
        )
        .await?;

        Ok(())
    }

    async fn process_heartbeat_notification_for_chat(
        &self,
        heartbeat: &StoredHeartbeat,
        status: &str,
        chat_id: i64,
    ) -> Result<(), BotError> {
        let existing =
            fetch_notification_state(self.database.pool(), &heartbeat.imei, chat_id).await?;

        if status == "on" {
            return self
                .process_engine_on_notification_for_chat(heartbeat, chat_id, existing)
                .await;
        }

        let text = format_engine_status_notification(heartbeat, status);

        match existing {
            Some(existing) if existing.last_status == status => {
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
            _ => {
                let active_sessions =
                    fetch_active_engine_sessions(self.database.pool(), &heartbeat.imei, chat_id)
                        .await?;
                let message_id = if let Some(message_id) = self
                    .finish_active_engine_sessions(
                        chat_id,
                        &heartbeat.imei,
                        &active_sessions,
                        heartbeat.server_received_at,
                        true,
                    )
                    .await?
                {
                    message_id
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
        let active_sessions =
            fetch_active_engine_sessions(self.database.pool(), &heartbeat.imei, chat_id).await?;
        let latest_pending = active_sessions
            .iter()
            .rev()
            .find(|session| session.session_status == "pending_confirmation");
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

        if !should_start_new_engine_on_session(heartbeat.server_received_at, last_on_heartbeat_at) {
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

        self.finish_active_engine_sessions(
            chat_id,
            &heartbeat.imei,
            &active_sessions,
            heartbeat.server_received_at,
            false,
        )
        .await?;

        let message_id = self.send_engine_on_confirmation(chat_id, heartbeat).await?;
        create_engine_session(
            self.database.pool(),
            &heartbeat.imei,
            chat_id,
            heartbeat.id,
            message_id,
            heartbeat.server_received_at,
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

    async fn finish_active_engine_sessions(
        &self,
        chat_id: i64,
        imei: &str,
        sessions: &[EngineSession],
        ended_at: DateTime<Utc>,
        send_theft_engine_off_alert: bool,
    ) -> Result<Option<i64>, BotError> {
        let mut last_message_id = None;

        for session in sessions {
            if session.session_status == "pending_confirmation" {
                if let Err(error) = self
                    .clear_inline_keyboard(chat_id, session.prompt_message_id)
                    .await
                {
                    warn!(
                        error = %error,
                        imei = %imei,
                        chat_id,
                        session_id = session.id,
                        message_id = session.prompt_message_id,
                        "failed to clear pending confirmation keyboard before finishing session"
                    );
                }
            }

            let message_id = self
                .finish_ride_session_and_send_summary(
                    chat_id,
                    imei,
                    session,
                    ended_at,
                    send_theft_engine_off_alert,
                )
                .await?;
            last_message_id = Some(message_id);
        }

        Ok(last_message_id)
    }

    async fn finish_inactive_subscription_sessions(
        &self,
        imei: &str,
        chat_id: i64,
        ended_at: DateTime<Utc>,
    ) -> Result<(), BotError> {
        let sessions = fetch_active_engine_sessions(self.database.pool(), imei, chat_id).await?;

        for session in sessions {
            if session.session_status == "pending_confirmation" {
                if let Err(error) = self
                    .clear_inline_keyboard(chat_id, session.prompt_message_id)
                    .await
                {
                    warn!(
                        error = %error,
                        imei = %imei,
                        chat_id,
                        session_id = session.id,
                        message_id = session.prompt_message_id,
                        "failed to clear pending confirmation keyboard before silently finishing inactive subscription session"
                    );
                }
            }

            resolve_engine_session_at(self.database.pool(), session.id, "finished", ended_at)
                .await?;
        }

        Ok(())
    }

    async fn finish_ride_session_and_send_summary(
        &self,
        chat_id: i64,
        imei: &str,
        session: &EngineSession,
        ended_at: DateTime<Utc>,
        send_theft_engine_off_alert: bool,
    ) -> Result<i64, BotError> {
        if let Some(message_id) = session.ride_status_message_id {
            resolve_engine_session_at(self.database.pool(), session.id, "finished", ended_at)
                .await?;
            return Ok(message_id);
        }

        self.send_message(chat_id, format_session_finished_message())
            .await?;
        let ride_summary =
            fetch_ride_summary(self.database.pool(), imei, session.created_at, ended_at).await?;
        let latest_location = fetch_latest_location_for_imei(self.database.pool(), imei).await?;

        if send_theft_engine_off_alert && session.session_status == "reported_theft" {
            self.send_message(
                chat_id,
                &format_theft_engine_off_message(latest_location.as_ref(), ended_at, Utc::now()),
            )
            .await?;
        }

        let message_id = self
            .send_message(
                chat_id,
                &format_ride_summary_message(
                    session,
                    ended_at,
                    ride_summary.as_ref(),
                    latest_location.as_ref(),
                ),
            )
            .await?;
        set_engine_session_ride_status_message_id(self.database.pool(), session.id, message_id)
            .await?;
        resolve_engine_session(self.database.pool(), session.id, "finished").await?;

        Ok(message_id)
    }

    async fn handle_message(&self, message: TelegramMessage) -> Result<(), BotError> {
        let Some(from) = message.from.as_ref() else {
            return Ok(());
        };
        let chat_id = message.chat.id;
        let telegram_user_id = from.id;

        let Some(text) = message.text.as_deref() else {
            return Ok(());
        };

        if let Some(command) = BotCommand::parse(text) {
            return self
                .handle_command(chat_id, telegram_user_id, command)
                .await;
        }

        let Some(user) =
            fetch_telegram_user_by_user_id(self.database.pool(), telegram_user_id).await?
        else {
            return Ok(());
        };

        if let Some(kind) = get_pending_analytics_kind(self.database.pool(), chat_id).await? {
            return self
                .handle_custom_analytics_input(chat_id, telegram_user_id, &user, kind, text)
                .await;
        }

        if user.registration_status != TelegramRegistrationStatus::AwaitingImei {
            return Ok(());
        }

        let imei = text.trim();
        if !is_valid_imei(imei) {
            self.send_message(chat_id, messages::MSG_1_BIND_INVALID_IMEI)
                .await?;
            return Ok(());
        }

        if user.bound_imei.is_some() {
            self.send_message(chat_id, messages::MSG_2_BIND_ALREADY_BOUND)
                .await?;
            return Ok(());
        }

        if !device_exists(self.database.pool(), imei).await? {
            self.send_message(chat_id, messages::MSG_3_BIND_DEVICE_NOT_FOUND)
                .await?;
            return Ok(());
        }

        if is_device_bound_to_another_user(self.database.pool(), imei, telegram_user_id).await? {
            self.send_message(chat_id, messages::MSG_4_BIND_DEVICE_ALREADY_TAKEN)
                .await?;
            return Ok(());
        }

        bind_telegram_user_to_imei(self.database.pool(), telegram_user_id, chat_id, imei).await?;
        self.send_message(chat_id, &messages::msg_5_bind_success(imei))
            .await?;
        if let Err(error) = self.send_bind_success_sticker(chat_id).await {
            warn!(error = %error, "failed to send bind-success sticker");
        }
        self.send_subscription_required_menu(chat_id).await?;

        Ok(())
    }

    async fn handle_custom_analytics_input(
        &self,
        chat_id: i64,
        telegram_user_id: i64,
        user: &TelegramUserRecord,
        kind: AnalyticsKind,
        text: &str,
    ) -> Result<(), BotError> {
        let Some(imei) = user.bound_imei.as_deref() else {
            clear_pending_analytics_kind(self.database.pool(), chat_id).await?;
            self.send_message(chat_id, messages::MSG_6_NOT_BOUND_USE_START)
                .await?;
            return Ok(());
        };

        if !has_active_subscription(self.database.pool(), telegram_user_id, Utc::now()).await? {
            clear_pending_analytics_kind(self.database.pool(), chat_id).await?;
            self.send_subscription_required_menu(chat_id).await?;
            return Ok(());
        }

        let range = match parse_custom_analytics_range(kind, text) {
            Some(range) if range.started_at < range.ended_at => range,
            _ => {
                let error_message = match kind {
                    AnalyticsKind::Sessions => messages::MSG_7_ANALYTICS_INVALID_DATE,
                    _ => messages::MSG_8_ANALYTICS_INVALID_RANGE,
                };
                self.send_message(chat_id, error_message).await?;
                return Ok(());
            }
        };

        clear_pending_analytics_kind(self.database.pool(), chat_id).await?;
        self.send_analytics_report(chat_id, imei, kind, range, Utc::now())
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
            BotCommand::Start => match user {
                Some(user) if user.bound_imei.is_some() => {
                    let is_active = has_active_subscription(
                        self.database.pool(),
                        user.telegram_user_id,
                        Utc::now(),
                    )
                    .await?;
                    if is_active {
                        self.send_message_internal(
                            chat_id,
                            &format_start_status_message(),
                            Some(subscribed_start_menu_keyboard()),
                            None,
                        )
                        .await?;
                    } else {
                        if let Err(error) = self.send_not_subscribed_sticker(chat_id).await {
                            warn!(error = %error, "failed to send not-subscribed sticker");
                        }
                        self.send_subscription_required_menu(chat_id).await?;
                    }
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
                        messages::MSG_9_START_BIND_PROMPT,
                    )
                    .await?;
                }
            },
            BotCommand::Help => {
                self.send_message(chat_id, messages::MSG_10_HELP).await?;
            }
            BotCommand::PaySupport => {
                self.send_message(chat_id, messages::MSG_11_PAY_SUPPORT)
                    .await?;
            }
            BotCommand::Terms => {
                self.send_message(chat_id, messages::MSG_12_TERMS).await?;
            }
            BotCommand::Unknown(command) => {
                self.send_message(chat_id, &messages::msg_13_unknown_command(&command))
                    .await?;
            }
        }

        Ok(())
    }

    async fn handle_payment_action(
        &self,
        callback_query: TelegramCallbackQuery,
        action: PaymentAction,
    ) -> Result<(), BotError> {
        let Some(message) = callback_query.message else {
            self.answer_callback_query(
                &callback_query.id,
                messages::TOAST_1_OPEN_BOT_CHAT,
                false,
            )
            .await?;
            return Ok(());
        };

        let chat_id = message.chat.id;
        let telegram_user_id = callback_query.from.id;

        match action {
            PaymentAction::Subscribe => {
                if !self
                    .ensure_bound_for_payment(&callback_query.id, telegram_user_id)
                    .await?
                {
                    return Ok(());
                }

                let midtrans = self
                    .midtrans
                    .as_ref()
                    .ok_or(BotError::MissingMidtransConfig)?;
                let user = fetch_telegram_user_by_user_id(self.database.pool(), telegram_user_id)
                    .await?
                    .ok_or_else(|| crate::midtrans::MidtransError::InvalidPricingTier("missing_user".to_string()))?;
                let bound_device = resolve_bound_device_for_user(self.database.pool(), &user).await?;
                let plan = resolve_subscription_plan_for_user(
                    self.database.pool(),
                    midtrans,
                    &user,
                )
                .await?;
                let payment_menu_message_id = i64::from(message.message_id.unwrap_or_default());
                let created_at = Utc::now();
                let order_id = build_midtrans_order_id(telegram_user_id, created_at);
                let expires_at = created_at + chrono::Duration::hours(midtrans.expiry_hours());
                let payment_quote = build_subscription_payment_quote(
                    self.database.pool(),
                    telegram_user_id,
                    plan,
                    created_at,
                )
                .await?;

                create_pending_midtrans_payment(
                    self.database.pool(),
                    telegram_user_id,
                    chat_id,
                    bound_device.id,
                    &bound_device.imei,
                    plan.plan_code,
                    &order_id,
                    payment_quote.total_amount_idr,
                    expires_at,
                )
                .await?;

                let created = midtrans
                    .create_snap_transaction(
                        plan,
                        &order_id,
                        created_at,
                        payment_quote.base_amount_idr + payment_quote.customer_reference_fee_idr,
                        payment_quote.shipment_fee_idr,
                        payment_quote.total_amount_idr,
                        payment_quote.fine_amount_idr,
                    )
                    .await?;
                mark_midtrans_payment_created(self.database.pool(), &order_id, &created).await?;

                self.answer_callback_query(&callback_query.id, "", false)
                    .await?;
                let payment_message = format_midtrans_payment_message_with_quote(
                    plan,
                    &created.payment_url,
                    created.expires_at,
                    payment_quote.base_amount_idr + payment_quote.customer_reference_fee_idr,
                    payment_quote.shipment_fee_idr,
                    payment_quote.fine_amount_idr,
                    payment_quote.total_amount_idr,
                );
                self.send_message_html(chat_id, &payment_message).await?;
                if let Err(error) = self
                    .clear_inline_keyboard(chat_id, payment_menu_message_id)
                    .await
                {
                    warn!(
                        error = %error,
                        chat_id,
                        message_id = payment_menu_message_id,
                        "failed to clear subscription payment keyboard"
                    );
                }
            }
        }

        Ok(())
    }

    async fn send_subscription_required_menu(&self, chat_id: i64) -> Result<(), BotError> {
        self.send_message_internal(
            chat_id,
            messages::MSG_15_SUBSCRIPTION_MENU,
            Some(subscription_payment_keyboard()),
            None,
        )
        .await?;

        Ok(())
    }

    async fn ensure_active_subscription_for_callback(
        &self,
        callback_query_id: &str,
        chat_id: i64,
        telegram_user_id: i64,
        message_id: Option<i32>,
    ) -> Result<bool, BotError> {
        if has_active_subscription(self.database.pool(), telegram_user_id, Utc::now()).await? {
            return Ok(true);
        }

        self.answer_callback_query(
            callback_query_id,
            messages::TOAST_2_SUBSCRIPTION_REQUIRED,
            false,
        )
            .await?;

        if let Some(message_id) = message_id {
            let message_id = i64::from(message_id);
            if let Err(error) = self.clear_inline_keyboard(chat_id, message_id).await {
                warn!(
                    error = %error,
                    chat_id,
                    message_id,
                    "failed to clear protected feature keyboard for inactive subscription"
                );
            }
        }

        self.send_subscription_required_menu(chat_id).await?;

        Ok(false)
    }

    async fn ensure_bound_for_payment(
        &self,
        callback_query_id: &str,
        telegram_user_id: i64,
    ) -> Result<bool, BotError> {
        let user = fetch_telegram_user_by_user_id(self.database.pool(), telegram_user_id).await?;
        let is_bound = user
            .as_ref()
            .map(|user| {
                user.bound_imei.is_some()
                    && user.registration_status == TelegramRegistrationStatus::Bound
            })
            .unwrap_or(false);

        if !is_bound {
            self.answer_callback_query(
                callback_query_id,
                messages::TOAST_3_BIND_FIRST,
                false,
            )
            .await?;
        }

        Ok(is_bound)
    }

    async fn handle_callback_query(
        &self,
        callback_query: TelegramCallbackQuery,
    ) -> Result<(), BotError> {
        let Some(data) = callback_query.data.as_deref() else {
            return Ok(());
        };
        if let Some(action) = PaymentAction::parse(data) {
            return self.handle_payment_action(callback_query, action).await;
        }

        let Some(message) = callback_query.message else {
            return Ok(());
        };
        let chat_id = message.chat.id;

        if let Some(action) = AnalyticsAction::parse(data) {
            if !self
                .ensure_active_subscription_for_callback(
                    &callback_query.id,
                    chat_id,
                    callback_query.from.id,
                    message.message_id,
                )
                .await?
            {
                return Ok(());
            }

            self.answer_callback_query(&callback_query.id, "", false)
                .await?;
            if action.range != AnalyticsRange::Select {
                if let Some(message_id) = message.message_id {
                    if let Err(error) = self
                        .clear_inline_keyboard(chat_id, i64::from(message_id))
                        .await
                    {
                        warn!(
                            error = %error,
                            chat_id,
                            message_id,
                            "failed to clear analytics range keyboard"
                        );
                    }
                }
            }
            return self
                .handle_analytics_action(chat_id, callback_query.from.id, action)
                .await;
        }

        if let Some(action) = TheftAlertAction::parse(data) {
            if action.requires_active_subscription()
                && !self
                    .ensure_active_subscription_for_callback(
                        &callback_query.id,
                        chat_id,
                        callback_query.from.id,
                        message.message_id,
                    )
                    .await?
            {
                return Ok(());
            }

            self.answer_callback_query(&callback_query.id, "", false)
                .await?;
            return self.handle_theft_alert_action(message, action).await;
        }

        let Some(action) = SessionAction::parse(data) else {
            return Ok(());
        };
        let prompt_message_id = i64::from(message.message_id.unwrap_or_default());

        if !self
            .ensure_active_subscription_for_callback(
                &callback_query.id,
                chat_id,
                callback_query.from.id,
                message.message_id,
            )
            .await?
        {
            return Ok(());
        }

        let Some(session) = fetch_engine_session_by_prompt_message(
            self.database.pool(),
            chat_id,
            prompt_message_id,
        )
        .await?
        else {
            self.answer_callback_query(
                &callback_query.id,
                messages::TOAST_4_SESSION_NOT_FOUND,
                false,
            )
            .await?;
            return Ok(());
        };

        if session.chat_id != chat_id || session.prompt_message_id != prompt_message_id {
            self.answer_callback_query(
                &callback_query.id,
                messages::TOAST_5_SESSION_MISMATCH,
                false,
            )
            .await?;
            return Ok(());
        }

        if session.session_status != "pending_confirmation" {
            self.answer_callback_query(
                &callback_query.id,
                messages::TOAST_6_SESSION_ALREADY_ENDED,
                false,
            )
                .await?;
            return Ok(());
        }

        self.answer_callback_query(&callback_query.id, "", false)
            .await?;
        self.clear_inline_keyboard(chat_id, prompt_message_id)
            .await?;

        match action {
            SessionAction::Yes => {
                self.send_message(chat_id, format_ride_safe_message())
                    .await?;
                if let Err(error) = self.send_engine_on_sticker(chat_id).await {
                    warn!(error = %error, "failed to send engine-on sticker");
                }
                update_engine_session_status(self.database.pool(), session.id, "confirmed_safe")
                    .await?;
            }
            SessionAction::No => {
                self.send_message_internal(
                    chat_id,
                    format_theft_warning_message(),
                    Some(theft_alert_keyboard(Some(session.id))),
                    None,
                )
                .await?;
                if let Err(error) = self.send_theft_warning_sticker(chat_id).await {
                    warn!(error = %error, "failed to send theft-warning sticker");
                }
                update_engine_session_status(self.database.pool(), session.id, "reported_theft")
                    .await?;
            }
        }

        Ok(())
    }

    async fn handle_analytics_action(
        &self,
        chat_id: i64,
        telegram_user_id: i64,
        action: AnalyticsAction,
    ) -> Result<(), BotError> {
        let Some(user) =
            fetch_telegram_user_by_user_id(self.database.pool(), telegram_user_id).await?
        else {
            self.send_message(chat_id, messages::MSG_6_NOT_BOUND_USE_START)
                .await?;
            return Ok(());
        };

        let Some(imei) = user.bound_imei.as_deref() else {
            self.send_message(chat_id, messages::MSG_6_NOT_BOUND_USE_START)
                .await?;
            return Ok(());
        };

        if action.range == AnalyticsRange::Select {
            let text = messages::msg_36_choose_range_for(action.kind.label());
            if should_remember_analytics_message(action.kind) {
                self.send_remembered_analytics_message(
                    chat_id,
                    action.kind,
                    AnalyticsMessageSlot::Selector,
                    &text,
                    Some(analytics_range_keyboard(action.kind)),
                )
                .await?;
            } else {
                self.send_message_internal(
                    chat_id,
                    &text,
                    Some(analytics_range_keyboard(action.kind)),
                    None,
                )
                .await?;
            }
            return Ok(());
        }

        if action.range == AnalyticsRange::Custom {
            set_pending_analytics_kind(self.database.pool(), chat_id, action.kind).await?;
            let message = match action.kind {
                AnalyticsKind::Sessions => messages::MSG_37_ANALYTICS_CUSTOM_DATE_PROMPT,
                _ => messages::MSG_38_ANALYTICS_CUSTOM_RANGE_PROMPT,
            };
            if should_remember_analytics_message(action.kind) {
                self.send_remembered_analytics_message(
                    chat_id,
                    action.kind,
                    AnalyticsMessageSlot::Selector,
                    message,
                    None,
                )
                .await?;
            } else {
                self.send_message(chat_id, message).await?;
            }
            return Ok(());
        }

        if action.kind == AnalyticsKind::Sessions && action.range == AnalyticsRange::Month {
            self.send_remembered_analytics_message(
                chat_id,
                action.kind,
                AnalyticsMessageSlot::Selector,
                messages::MSG_39_ANALYTICS_SESSIONS_MONTH_UNSUPPORTED,
                Some(analytics_range_keyboard(action.kind)),
            )
            .await?;
            return Ok(());
        }

        let range = resolve_preset_analytics_range(action.range, Utc::now())
            .expect("preset analytics range should resolve");
        self.send_analytics_report(chat_id, imei, action.kind, range, Utc::now())
            .await?;

        Ok(())
    }

    async fn send_analytics_report(
        &self,
        chat_id: i64,
        imei: &str,
        kind: AnalyticsKind,
        range: AnalyticsDateRange,
        reference_time: DateTime<Utc>,
    ) -> Result<(), BotError> {
        let text = match kind {
            AnalyticsKind::Sessions => {
                let sessions = fetch_analytics_sessions(
                    self.database.pool(),
                    imei,
                    chat_id,
                    range.started_at,
                    range.ended_at,
                    reference_time,
                )
                .await?;
                let mut session_reports = Vec::with_capacity(sessions.len());
                let effective_range_end = reference_time.min(range.ended_at);

                for session in sessions {
                    let clipped_start = session.created_at.max(range.started_at);
                    let clipped_end = session
                        .resolved_at
                        .unwrap_or(effective_range_end)
                        .min(effective_range_end);
                    let summary = if clipped_start < clipped_end {
                        fetch_ride_summary(self.database.pool(), imei, clipped_start, clipped_end)
                            .await?
                    } else {
                        Some(RideSummary {
                            total_distance_km: 0.0,
                            riding_seconds: 0,
                            average_speed_kph: 0.0,
                        })
                    };

                    session_reports.push(AnalyticsSessionReport {
                        session,
                        clipped_start,
                        clipped_end,
                        total_distance_km: summary
                            .as_ref()
                            .map(|value| value.total_distance_km)
                            .unwrap_or(0.0),
                        riding_seconds: summary
                            .as_ref()
                            .map(|value| value.riding_seconds)
                            .unwrap_or(0),
                        route_link: build_history_tracking_link(imei, clipped_start, clipped_end),
                    });
                }

                let full_day_route_end = range
                    .ended_at
                    .checked_sub_signed(chrono::Duration::seconds(1))
                    .unwrap_or(range.ended_at);
                let full_day_route_link =
                    build_history_tracking_link(imei, range.started_at, full_day_route_end);

                format_driving_sessions_report(
                    &range,
                    &session_reports,
                    full_day_route_link.as_deref(),
                    reference_time,
                )
            }
            AnalyticsKind::Metrics => {
                let sessions = fetch_analytics_sessions(
                    self.database.pool(),
                    imei,
                    chat_id,
                    range.started_at,
                    range.ended_at,
                    reference_time,
                )
                .await?;
                let effective_range_end = reference_time.min(range.ended_at);
                let summary = fetch_analytics_ride_summary(
                    self.database.pool(),
                    imei,
                    &sessions,
                    range.started_at,
                    effective_range_end,
                )
                .await?;

                format_metrics_report(&range, Some(&summary))
            }
            AnalyticsKind::TotalKm => {
                let sessions = fetch_analytics_sessions(
                    self.database.pool(),
                    imei,
                    chat_id,
                    range.started_at,
                    range.ended_at,
                    reference_time,
                )
                .await?;
                let effective_range_end = reference_time.min(range.ended_at);
                let summary = fetch_analytics_ride_summary(
                    self.database.pool(),
                    imei,
                    &sessions,
                    range.started_at,
                    effective_range_end,
                )
                .await?;

                format_total_km_report(&range, Some(&summary))
            }
            AnalyticsKind::TotalDrivingTime => {
                let sessions = fetch_analytics_sessions(
                    self.database.pool(),
                    imei,
                    chat_id,
                    range.started_at,
                    range.ended_at,
                    reference_time,
                )
                .await?;
                let effective_range_end = reference_time.min(range.ended_at);
                let summary = fetch_analytics_ride_summary(
                    self.database.pool(),
                    imei,
                    &sessions,
                    range.started_at,
                    effective_range_end,
                )
                .await?;
                let total_seconds = summary.riding_seconds;
                format_total_driving_time_report(&range, total_seconds)
            }
        };

        if should_remember_analytics_message(kind) {
            self.delete_remembered_analytics_message(chat_id, kind, AnalyticsMessageSlot::Selector)
                .await?;
            self.send_remembered_analytics_message(
                chat_id,
                kind,
                AnalyticsMessageSlot::Report,
                &text,
                None,
            )
            .await?;
        } else {
            self.send_message(chat_id, &text).await?;
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
            let Some(session) =
                fetch_engine_session_by_id(self.database.pool(), session_id).await?
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
            self.send_message(chat_id, messages::MSG_6_NOT_BOUND_USE_START)
                .await?;
            return Ok(());
        };

        match action {
            TheftAlertAction::StreamLocation { .. } => {
                // Use a timestamp from device_locations so the URL always includes a GPS point.
                let start_at = fetch_latest_location_received_at(self.database.pool(), imei).await?;
                let live_tracking_link =
                    start_at.and_then(|value| build_live_tracking_link(imei, value));
                let text = format_stream_location_message(live_tracking_link.as_deref());
                self.send_message(chat_id, &text).await?;
            }
            TheftAlertAction::CheckLatestStatus { .. } => {
                let location = fetch_latest_location_for_imei(self.database.pool(), imei).await?;
                let latest_heartbeat =
                    fetch_latest_heartbeat_for_imei(self.database.pool(), imei).await?;
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
        self.send_message_internal(chat_id, text, None, None).await
    }

    async fn send_message_html(&self, chat_id: i64, text: &str) -> Result<i64, reqwest::Error> {
        self.send_message_internal(chat_id, text, None, Some("HTML"))
            .await
    }

    async fn send_engine_on_confirmation(
        &self,
        chat_id: i64,
        heartbeat: &StoredHeartbeat,
    ) -> Result<i64, reqwest::Error> {
        let text = format_engine_on_confirmation_message(heartbeat);
        let keyboard = engine_session_confirmation_keyboard();

        self.send_message_internal(chat_id, &text, Some(keyboard), None)
            .await
    }

    async fn send_engine_on_sticker(&self, chat_id: i64) -> Result<(), reqwest::Error> {
        let sticker_part = multipart::Part::bytes(ENGINE_ON_STICKER_BYTES.to_vec())
            .file_name(messages::STICKER_1_ENGINE_ON_FILE_NAME)
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
            .file_name(messages::STICKER_2_BIND_SUCCESS_FILE_NAME)
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

    async fn send_not_subscribed_sticker(&self, chat_id: i64) -> Result<(), reqwest::Error> {
        let sticker_part = multipart::Part::bytes(NOT_SUBSCRIBED_STICKER_BYTES.to_vec())
            .file_name(messages::STICKER_3_NOT_SUBSCRIBED_FILE_NAME)
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

    async fn send_theft_warning_sticker(&self, chat_id: i64) -> Result<(), reqwest::Error> {
        let sticker_part = multipart::Part::bytes(THEFT_WARNING_STICKER_BYTES.to_vec())
            .file_name(messages::STICKER_4_THEFT_WARNING_FILE_NAME)
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
        parse_mode: Option<&str>,
    ) -> Result<i64, reqwest::Error> {
        let request = SendMessageRequest {
            chat_id,
            text: text.to_string(),
            reply_markup,
            parse_mode: parse_mode.map(ToString::to_string),
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

    async fn clear_inline_keyboard(
        &self,
        chat_id: i64,
        message_id: i64,
    ) -> Result<(), reqwest::Error> {
        self.edit_message_reply_markup(chat_id, message_id, None)
            .await
    }

    async fn delete_message(&self, chat_id: i64, message_id: i64) -> Result<(), reqwest::Error> {
        let request = DeleteMessageRequest {
            chat_id,
            message_id,
        };

        let response = self
            .client
            .post(format!("{}/deleteMessage", self.base_url))
            .json(&request)
            .send()
            .await?
            .error_for_status()?;

        let _ = response.bytes().await?;
        Ok(())
    }

    async fn delete_remembered_analytics_message(
        &self,
        chat_id: i64,
        kind: AnalyticsKind,
        slot: AnalyticsMessageSlot,
    ) -> Result<(), BotError> {
        let message_id =
            get_last_analytics_message_id(self.database.pool(), chat_id, kind, slot).await?;

        if let Some(message_id) = message_id {
            if let Err(error) = self.delete_message(chat_id, message_id).await {
                warn!(
                    error = %error,
                    chat_id,
                    message_id,
                    analytics_kind = kind.callback_value(),
                    analytics_slot = slot.state_value(),
                    "failed to delete previous analytics message; continuing"
                );
            }

            clear_last_analytics_message_id(self.database.pool(), chat_id, kind, slot).await?;
        }

        Ok(())
    }

    async fn send_remembered_analytics_message(
        &self,
        chat_id: i64,
        kind: AnalyticsKind,
        slot: AnalyticsMessageSlot,
        text: &str,
        reply_markup: Option<InlineKeyboardMarkup>,
    ) -> Result<(), BotError> {
        self.delete_remembered_analytics_message(chat_id, kind, slot)
            .await?;

        let message_id = self
            .send_message_internal(chat_id, text, reply_markup, None)
            .await?;
        set_last_analytics_message_id(self.database.pool(), chat_id, kind, slot, message_id)
            .await?;

        Ok(())
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

pub const HELP_TEXT: &str = messages::MSG_10_HELP;
pub const PAY_SUPPORT_TEXT: &str = messages::MSG_11_PAY_SUPPORT;
pub const TERMS_TEXT: &str = messages::MSG_12_TERMS;

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
    from: TelegramUser,
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
    #[serde(skip_serializing_if = "Option::is_none")]
    parse_mode: Option<String>,
}

#[derive(Debug, Serialize)]
struct EditMessageReplyMarkupRequest {
    chat_id: i64,
    message_id: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    reply_markup: Option<InlineKeyboardMarkup>,
}

#[derive(Debug, Serialize)]
struct DeleteMessageRequest {
    chat_id: i64,
    message_id: i64,
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
                text: messages::BTN_1_ENGINE_CONFIRM_YES.to_string(),
                callback_data: "engine_session:yes".to_string(),
            },
            InlineKeyboardButton {
                text: messages::BTN_2_ENGINE_CONFIRM_NO.to_string(),
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
                    text: messages::BTN_3_THEFT_STREAM_LOCATION.to_string(),
                    callback_data: theft_alert_callback_data("stream_location", session_id),
                },
                InlineKeyboardButton {
                    text: messages::BTN_4_THEFT_HEALTH_CHECK.to_string(),
                    callback_data: theft_alert_callback_data("check_latest_status", session_id),
                },
            ],
            vec![InlineKeyboardButton {
                text: messages::BTN_5_THEFT_CONTACT_SUPPORT.to_string(),
                callback_data: theft_alert_callback_data("contact_support", session_id),
            }],
        ],
    }
}

fn subscribed_start_menu_keyboard() -> InlineKeyboardMarkup {
    InlineKeyboardMarkup {
        inline_keyboard: vec![
            vec![
                InlineKeyboardButton {
                    text: messages::BTN_6_MENU_LIVE_TRACKING.to_string(),
                    callback_data: theft_alert_callback_data("stream_location", None),
                },
                InlineKeyboardButton {
                    text: messages::BTN_7_MENU_STATUS_TERKINI.to_string(),
                    callback_data: theft_alert_callback_data("check_latest_status", None),
                },
            ],
            vec![
                InlineKeyboardButton {
                    text: messages::BTN_8_MENU_HISTORY_PERJALANAN.to_string(),
                    callback_data: analytics_callback_data(
                        AnalyticsKind::Sessions,
                        AnalyticsRange::Select,
                    ),
                },
                InlineKeyboardButton {
                    text: messages::BTN_9_MENU_AKTIVITAS_KENDARAAN.to_string(),
                    callback_data: analytics_callback_data(
                        AnalyticsKind::Metrics,
                        AnalyticsRange::Select,
                    ),
                },
            ],
        ],
    }
}

fn analytics_range_keyboard(kind: AnalyticsKind) -> InlineKeyboardMarkup {
    let inline_keyboard = match kind {
        AnalyticsKind::Sessions => vec![
            vec![
                InlineKeyboardButton {
                    text: AnalyticsRange::Today.label().to_string(),
                    callback_data: analytics_callback_data(kind, AnalyticsRange::Today),
                },
                InlineKeyboardButton {
                    text: AnalyticsRange::Yesterday.label().to_string(),
                    callback_data: analytics_callback_data(kind, AnalyticsRange::Yesterday),
                },
            ],
            vec![InlineKeyboardButton {
                text: AnalyticsRange::Custom.label().to_string(),
                callback_data: analytics_callback_data(kind, AnalyticsRange::Custom),
            }],
        ],
        _ => vec![
            vec![
                InlineKeyboardButton {
                    text: AnalyticsRange::Today.label().to_string(),
                    callback_data: analytics_callback_data(kind, AnalyticsRange::Today),
                },
                InlineKeyboardButton {
                    text: AnalyticsRange::Yesterday.label().to_string(),
                    callback_data: analytics_callback_data(kind, AnalyticsRange::Yesterday),
                },
            ],
            vec![
                InlineKeyboardButton {
                    text: AnalyticsRange::Month.label().to_string(),
                    callback_data: analytics_callback_data(kind, AnalyticsRange::Month),
                },
                InlineKeyboardButton {
                    text: AnalyticsRange::Custom.label().to_string(),
                    callback_data: analytics_callback_data(kind, AnalyticsRange::Custom),
                },
            ],
        ],
    };

    InlineKeyboardMarkup { inline_keyboard }
}

fn subscription_payment_keyboard() -> InlineKeyboardMarkup {
    InlineKeyboardMarkup {
        inline_keyboard: vec![vec![InlineKeyboardButton {
            text: messages::BTN_15_SUBSCRIBE.to_string(),
            callback_data: messages::CALLBACK_1_PAYMENT_SUBSCRIBE.to_string(),
        }]],
    }
}

fn theft_alert_callback_data(action: &str, session_id: Option<i64>) -> String {
    match session_id {
        Some(session_id) => format!("theft_alert:{action}:{session_id}"),
        None => format!("theft_alert:{action}"),
    }
}

fn analytics_callback_data(kind: AnalyticsKind, range: AnalyticsRange) -> String {
    format!(
        "analytics:{}:{}",
        kind.callback_value(),
        range.callback_value()
    )
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
    pub riding_seconds: u64,
    pub average_speed_kph: f64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AnalyticsDateRange {
    pub label: String,
    pub started_at: DateTime<Utc>,
    pub ended_at: DateTime<Utc>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AnalyticsSession {
    pub id: i64,
    pub session_status: String,
    pub created_at: DateTime<Utc>,
    pub resolved_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct AnalyticsSessionReport {
    pub session: AnalyticsSession,
    pub clipped_start: DateTime<Utc>,
    pub clipped_end: DateTime<Utc>,
    pub total_distance_km: f64,
    pub riding_seconds: u64,
    pub route_link: Option<String>,
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
pub struct TelegramSubscriptionRecord {
    pub id: i64,
    pub telegram_user_id: i64,
    pub chat_id: i64,
    pub plan_code: String,
    pub status: String,
    pub current_period_start_at: Option<DateTime<Utc>>,
    pub current_period_end_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct NotificationRecipient {
    chat_id: i64,
    has_active_subscription: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct BoundDeviceRecord {
    id: i64,
    imei: String,
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
    messages::msg_17_heartbeat_notification(heartbeat)
}

pub fn format_engine_status_notification(heartbeat: &StoredHeartbeat, status: &str) -> String {
    messages::msg_16_engine_status_notification(heartbeat, status)
}

pub fn format_inactive_subscription_engine_status_message(
    heartbeat: &StoredHeartbeat,
    status: &str,
) -> String {
    let _ = heartbeat;
    messages::msg_18_inactive_subscription_engine_status_message(status)
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
    messages::msg_22_engine_on_confirmation()
}

pub fn format_ride_safe_message() -> &'static str {
    messages::MSG_23_RIDE_SAFE
}

pub fn format_session_finished_message() -> &'static str {
    messages::MSG_24_SESSION_FINISHED
}

pub fn format_theft_warning_message() -> &'static str {
    messages::MSG_25_THEFT_WARNING
}

pub fn format_theft_location_message(location: Option<&StoredLocation>) -> String {
    if let Some(location) = location {
        format_latest_location_message(location)
    } else {
        messages::MSG_26_THEFT_LOCATION_MISSING.to_string()
    }
}

pub fn format_theft_engine_off_message(
    latest_location: Option<&StoredLocation>,
    engine_off_at: DateTime<Utc>,
    current_time: DateTime<Utc>,
) -> String {
    let _ = current_time;
    messages::msg_27_theft_engine_off_message(latest_location, engine_off_at)
}

pub fn format_stream_location_message(live_tracking_link: Option<&str>) -> String {
    messages::msg_28_stream_location_message(live_tracking_link)
}

fn build_live_tracking_link(imei: &str, start_at: DateTime<Utc>) -> Option<String> {
    let mut url = reqwest::Url::parse(&format!("{LIVE_TRACKING_BASE_URL}/{imei}")).ok()?;
    let start_at = start_at.format("%Y-%m-%dT%H:%M:%SZ").to_string();
    url.query_pairs_mut().append_pair("start_at", &start_at);
    Some(url.into())
}

pub fn format_latest_motor_status_message(
    session: &EngineSession,
    heartbeat: Option<&StoredHeartbeat>,
    location: Option<&StoredLocation>,
) -> String {
    messages::msg_33_latest_motor_status_message(session, heartbeat, location, Utc::now())
}

pub fn format_latest_motor_status_initial_message(
    session: &EngineSession,
    heartbeat: Option<&StoredHeartbeat>,
    location: Option<&StoredLocation>,
    requested_at: DateTime<Utc>,
) -> String {
    messages::msg_33_latest_motor_status_message(session, heartbeat, location, requested_at)
}

pub fn format_contact_support_message() -> &'static str {
    messages::MSG_29_CONTACT_SUPPORT
}

pub fn format_ride_summary_message(
    session: &EngineSession,
    off_time: DateTime<Utc>,
    summary: Option<&RideSummary>,
    latest_location: Option<&StoredLocation>,
) -> String {
    messages::msg_30_ride_summary_message(session, off_time, summary, latest_location)
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
    messages::msg_33_latest_motor_status_message(session, heartbeat, location, reference_time)
}

fn format_relative_time_compact(
    reference_time: DateTime<Utc>,
    event_time: DateTime<Utc>,
) -> String {
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
        "Sesi Saat Ini\nKamu mulai berkendara pada {}.\nTotal waktu di jalan sejauh ini {:02}:{:02}:{:02}.\nGPS tracking saat ini {} dan kualitas koneksi kamu {}.",
        start.format("%d %b %Y - %H:%M WIB"),
        hours,
        minutes,
        seconds,
        if heartbeat.gps_tracking_on { "aktif" } else { "mati" },
        connection_status_label(heartbeat.gsm_signal_strength),
    )
}

fn connection_status_label(gsm_signal_strength: i32) -> &'static str {
    match gsm_signal_strength.clamp(1, 4) {
        1 => "Lemah",
        2 => "Cukup",
        3 => "Baik",
        4 => "Sangat Baik",
        _ => "Tidak diketahui",
    }
}

fn gps_battery_label(voltage_level: i32) -> &'static str {
    match voltage_level {
        0 => "Habis",
        1 => "Sangat Rendah",
        2 => "Rendah",
        3 => "Sedang",
        4 => "Penuh",
        _ => "Tidak diketahui",
    }
}

pub fn format_latest_location_message(location: &StoredLocation) -> String {
    messages::msg_34_latest_location_message(location)
}

fn format_start_status_message() -> String {
    messages::MSG_14_START_STATUS.to_string()
}

fn format_subscription_menu_message() -> String {
    messages::MSG_15_SUBSCRIPTION_MENU.to_string()
}

fn resolve_preset_analytics_range(
    range: AnalyticsRange,
    reference_time: DateTime<Utc>,
) -> Option<AnalyticsDateRange> {
    let wib = FixedOffset::east_opt(WIB_OFFSET_SECONDS).expect("valid WIB offset");
    let reference_wib = reference_time.with_timezone(&wib);
    let today = reference_wib.date_naive();
    let today_start = wib_datetime_to_utc(today.and_hms_opt(0, 0, 0)?);

    match range {
        AnalyticsRange::Today => Some(AnalyticsDateRange {
            label: "Hari ini".to_string(),
            started_at: today_start,
            ended_at: reference_time,
        }),
        AnalyticsRange::Yesterday => {
            let yesterday = today.checked_sub_signed(chrono::Duration::days(1))?;
            let started_at = wib_datetime_to_utc(yesterday.and_hms_opt(0, 0, 0)?);
            let ended_at = today_start;

            Some(AnalyticsDateRange {
                label: "Kemarin".to_string(),
                started_at,
                ended_at,
            })
        }
        AnalyticsRange::Month => {
            let month_start_date = NaiveDate::from_ymd_opt(today.year(), today.month(), 1)?;
            Some(AnalyticsDateRange {
                label: "Bulan ini".to_string(),
                started_at: wib_datetime_to_utc(month_start_date.and_hms_opt(0, 0, 0)?),
                ended_at: reference_time,
            })
        }
        AnalyticsRange::Select | AnalyticsRange::Custom => None,
    }
}

fn parse_custom_analytics_range(kind: AnalyticsKind, value: &str) -> Option<AnalyticsDateRange> {
    if kind == AnalyticsKind::Sessions {
        return parse_custom_analytics_single_date(value);
    }

    let (start, end) = value.trim().split_once(" to ")?;
    let started_at = parse_wib_date_start(start.trim())?;
    let ended_at = parse_wib_date_end(end.trim())?;

    Some(AnalyticsDateRange {
        label: "Rentang custom".to_string(),
        started_at,
        ended_at,
    })
}

fn parse_custom_analytics_single_date(value: &str) -> Option<AnalyticsDateRange> {
    let date = NaiveDate::parse_from_str(value.trim(), "%Y-%m-%d").ok()?;
    Some(analytics_single_day_range(date, "Tanggal custom"))
}

fn analytics_single_day_range(date: NaiveDate, label: &str) -> AnalyticsDateRange {
    let started_at = wib_datetime_to_utc(date.and_hms_opt(0, 0, 0).expect("valid start of day"));
    let ended_at = wib_datetime_to_utc(
        date.checked_add_signed(chrono::Duration::days(1))
            .expect("valid next day")
            .and_hms_opt(0, 0, 0)
            .expect("valid start of next day"),
    );

    AnalyticsDateRange {
        label: label.to_string(),
        started_at,
        ended_at,
    }
}

fn parse_wib_date_start(value: &str) -> Option<DateTime<Utc>> {
    let parsed = NaiveDate::parse_from_str(value, "%Y-%m-%d").ok()?;
    Some(wib_datetime_to_utc(parsed.and_hms_opt(0, 0, 0)?))
}

fn parse_wib_date_end(value: &str) -> Option<DateTime<Utc>> {
    let parsed = NaiveDate::parse_from_str(value, "%Y-%m-%d").ok()?;
    Some(wib_datetime_to_utc(
        parsed
            .checked_add_signed(chrono::Duration::days(1))?
            .and_hms_opt(0, 0, 0)?,
    ))
}

fn wib_datetime_to_utc(value: NaiveDateTime) -> DateTime<Utc> {
    let wib = FixedOffset::east_opt(WIB_OFFSET_SECONDS).expect("valid WIB offset");
    wib.from_local_datetime(&value)
        .single()
        .expect("WIB has no ambiguous local datetime")
        .with_timezone(&Utc)
}

fn format_analytics_range_label(range: &AnalyticsDateRange) -> String {
    let wib = FixedOffset::east_opt(WIB_OFFSET_SECONDS).expect("valid WIB offset");
    let started_at = range.started_at.with_timezone(&wib);
    let ended_at = range.ended_at.with_timezone(&wib);

    format!(
        "{}\n{} - {} WIB",
        range.label,
        started_at.format("%d %b %Y %H:%M"),
        ended_at.format("%d %b %Y %H:%M")
    )
}

fn format_duration_compact_from_seconds(total_seconds: u64) -> String {
    let hours = total_seconds / 3600;
    let minutes = (total_seconds % 3600) / 60;
    let seconds = total_seconds % 60;

    if hours > 0 {
        format!("{hours}h {minutes}m {seconds}s")
    } else if minutes > 0 {
        format!("{minutes}m {seconds}s")
    } else {
        format!("{seconds}s")
    }
}

fn format_duration_minutes_from_seconds(total_seconds: u64) -> String {
    let total_minutes = total_seconds / 60;
    let hours = total_minutes / 60;
    let minutes = total_minutes % 60;

    if hours > 0 {
        format!("{hours}h {minutes}m")
    } else {
        format!("{minutes}m")
    }
}

#[cfg(test)]
fn clipped_session_seconds(
    session: &AnalyticsSession,
    range_start: DateTime<Utc>,
    range_end: DateTime<Utc>,
) -> u64 {
    let effective_end = session.resolved_at.unwrap_or(range_end);
    let clipped_start = session.created_at.max(range_start);
    let clipped_end = effective_end.min(range_end);

    clipped_end
        .signed_duration_since(clipped_start)
        .to_std()
        .map(|duration| duration.as_secs())
        .unwrap_or(0)
}

#[cfg(test)]
fn total_clipped_session_seconds(
    sessions: &[AnalyticsSession],
    range_start: DateTime<Utc>,
    range_end: DateTime<Utc>,
) -> u64 {
    sessions
        .iter()
        .map(|session| clipped_session_seconds(session, range_start, range_end))
        .sum()
}

fn format_driving_sessions_report(
    range: &AnalyticsDateRange,
    sessions: &[AnalyticsSessionReport],
    full_day_route_link: Option<&str>,
    _reference_time: DateTime<Utc>,
) -> String {
    let _ = _reference_time;
    messages::msg_40_driving_sessions_report(range, sessions, full_day_route_link)
}

fn format_total_km_report(range: &AnalyticsDateRange, summary: Option<&RideSummary>) -> String {
    messages::msg_41_total_km_report(range, summary)
}

fn format_metrics_report(range: &AnalyticsDateRange, summary: Option<&RideSummary>) -> String {
    messages::msg_42_metrics_report(range, summary)
}

fn format_ride_stats_date_range(range: &AnalyticsDateRange) -> String {
    let wib = FixedOffset::east_opt(WIB_OFFSET_SECONDS).expect("valid WIB offset");
    let started_at = range.started_at.with_timezone(&wib);
    let ended_at = range
        .ended_at
        .checked_sub_signed(chrono::Duration::seconds(1))
        .unwrap_or(range.ended_at)
        .with_timezone(&wib);

    if started_at.date_naive() == ended_at.date_naive() {
        return started_at.format("%d %b %Y").to_string();
    }

    if started_at.year() == ended_at.year() {
        format!(
            "{} → {}",
            started_at.format("%d %b"),
            ended_at.format("%d %b %Y")
        )
    } else {
        format!(
            "{} → {}",
            started_at.format("%d %b %Y"),
            ended_at.format("%d %b %Y")
        )
    }
}

fn format_total_driving_time_report(range: &AnalyticsDateRange, total_seconds: u64) -> String {
    messages::msg_45_total_driving_time_report(range, total_seconds)
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

pub fn format_payment_success_message(current_period_end_at: Option<DateTime<Utc>>) -> String {
    messages::msg_46_payment_success(current_period_end_at)
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

fn should_finish_stale_engine_session(
    reference_time: DateTime<Utc>,
    latest_heartbeat_time: DateTime<Utc>,
) -> bool {
    reference_time
        .signed_duration_since(latest_heartbeat_time)
        .num_seconds()
        >= STALE_ENGINE_SESSION_TIMEOUT_SECS
}

fn option_f64(value: Option<f64>) -> String {
    value
        .map(|v| format!("{v:.6}"))
        .unwrap_or_else(|| "tidak diketahui".to_string())
}

fn option_i32(value: Option<i32>) -> String {
    value
        .map(|v| v.to_string())
        .unwrap_or_else(|| "tidak diketahui".to_string())
}

fn option_bool(value: Option<bool>) -> String {
    value
        .map(|v| v.to_string())
        .unwrap_or_else(|| "tidak diketahui".to_string())
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

fn total_route_distance_km(points: &[(f64, f64)]) -> f64 {
    let points = filter_gps_spike_outliers(points);

    points
        .windows(2)
        .map(|window| {
            let (start_latitude, start_longitude) = window[0];
            let (end_latitude, end_longitude) = window[1];

            haversine_distance_km(start_latitude, start_longitude, end_latitude, end_longitude)
        })
        .sum()
}

fn total_riding_seconds(points: &[(DateTime<Utc>, i32)]) -> u64 {
    points
        .windows(2)
        .filter_map(|window| {
            let (previous_time, previous_speed) = window[0];
            let (current_time, current_speed) = window[1];

            if previous_speed < RIDING_TIME_MOVING_SPEED_KPH
                || current_speed < RIDING_TIME_MOVING_SPEED_KPH
            {
                return None;
            }

            let gap_seconds = current_time
                .signed_duration_since(previous_time)
                .num_seconds();
            if !(1..=RIDING_TIME_MAX_POINT_GAP_SECS).contains(&gap_seconds) {
                return None;
            }

            Some(gap_seconds as u64)
        })
        .sum()
}

fn filter_gps_spike_outliers(points: &[(f64, f64)]) -> Vec<(f64, f64)> {
    if points.len() < 3 {
        return points.to_vec();
    }

    const SPIKE_KM: f64 = 0.08;

    let mut cleaned = Vec::with_capacity(points.len());
    cleaned.push(points[0]);

    for index in 1..points.len() - 1 {
        let previous = points[index - 1];
        let current = points[index];
        let next = points[index + 1];

        let distance_from_previous =
            haversine_distance_km(previous.0, previous.1, current.0, current.1);
        let distance_to_next = haversine_distance_km(current.0, current.1, next.0, next.1);
        let neighbour_distance = haversine_distance_km(previous.0, previous.1, next.0, next.1);
        let is_spike = distance_from_previous > SPIKE_KM
            && distance_to_next > SPIKE_KM
            && neighbour_distance < distance_from_previous.max(distance_to_next) * 0.5;

        if !is_spike {
            cleaned.push(current);
        }
    }

    cleaned.push(points[points.len() - 1]);
    cleaned
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
        let registration_status =
            parse_registration_status(row.get::<String, _>("registration_status").as_str())?;
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
        let registration_status =
            parse_registration_status(row.get::<String, _>("registration_status").as_str())?;
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

async fn resolve_subscription_plan_for_user(
    pool: &sqlx::PgPool,
    midtrans: &MidtransClient,
    user: &TelegramUserRecord,
) -> Result<crate::midtrans::SubscriptionPlan, BotError> {
    let bound_device = resolve_bound_device_for_user(pool, user).await?;

    let row = sqlx::query(
        r#"
        SELECT pricing_tier
        FROM devices
        WHERE imei = $1
        LIMIT 1
        "#,
    )
    .bind(&bound_device.imei)
    .fetch_optional(pool)
    .await?;

    let pricing_tier = row
        .and_then(|row| row.try_get::<String, _>("pricing_tier").ok())
        .ok_or_else(|| crate::midtrans::MidtransError::InvalidPricingTier(format!("missing_device_pricing_tier:{}", bound_device.imei)))?;
    let tier = parse_pricing_tier(&pricing_tier)?;

    Ok(midtrans.subscription_plan_for_tier(tier))
}

async fn resolve_bound_device_for_user(
    pool: &sqlx::PgPool,
    user: &TelegramUserRecord,
) -> Result<BoundDeviceRecord, BotError> {
    let imei = user
        .bound_imei
        .as_deref()
        .ok_or_else(|| crate::midtrans::MidtransError::InvalidPricingTier("missing_bound_device".to_string()))?;

    let row = sqlx::query(
        r#"
        SELECT id, imei
        FROM devices
        WHERE imei = $1
        LIMIT 1
        "#,
    )
    .bind(imei)
    .fetch_optional(pool)
    .await?;

    let row = row.ok_or_else(|| {
        crate::midtrans::MidtransError::InvalidPricingTier(format!("missing_bound_device_record:{imei}"))
    })?;

    Ok(BoundDeviceRecord {
        id: row.get("id"),
        imei: row.get("imei"),
    })
}

async fn fetch_notification_recipients_for_imei(
    pool: &sqlx::PgPool,
    imei: &str,
) -> Result<Vec<NotificationRecipient>, sqlx::Error> {
    let rows = sqlx::query(
        r#"
        SELECT tu.chat_id,
               EXISTS (
                   SELECT 1
                   FROM telegram_subscriptions ts
                   WHERE ts.telegram_user_id = tu.telegram_user_id
                     AND ts.plan_code IN ($2, $3)
                     AND ts.status = 'active'
                     AND ts.current_period_end_at > NOW()
               ) AS has_active_subscription
        FROM telegram_users tu
        WHERE tu.bound_imei = $1
          AND tu.registration_status = 'bound'
        ORDER BY tu.chat_id ASC
        "#,
    )
    .bind(imei)
    .bind(MIDTRANS_BASIC_PLAN_CODE)
    .bind(MIDTRANS_OJOL_PLAN_CODE)
    .fetch_all(pool)
    .await?;

    Ok(rows
        .into_iter()
        .map(|row| NotificationRecipient {
            chat_id: row.get("chat_id"),
            has_active_subscription: row.get("has_active_subscription"),
        })
        .collect())
}

async fn has_active_subscription(
    pool: &sqlx::PgPool,
    telegram_user_id: i64,
    reference_time: DateTime<Utc>,
) -> Result<bool, sqlx::Error> {
    let row = sqlx::query(
        r#"
        SELECT 1
        FROM telegram_subscriptions
        WHERE telegram_user_id = $1
          AND plan_code IN ($2, $3)
          AND status = 'active'
          AND current_period_end_at > $4
        LIMIT 1
        "#,
    )
    .bind(telegram_user_id)
    .bind(MIDTRANS_BASIC_PLAN_CODE)
    .bind(MIDTRANS_OJOL_PLAN_CODE)
    .bind(reference_time)
    .fetch_optional(pool)
    .await?;

    Ok(row.is_some())
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

pub async fn get_state_string(
    pool: &sqlx::PgPool,
    key: &str,
) -> Result<Option<String>, sqlx::Error> {
    let row = sqlx::query("SELECT state_value FROM telegram_bot_state WHERE state_key = $1")
        .bind(key)
        .fetch_optional(pool)
        .await?;

    Ok(row.map(|row| row.get("state_value")))
}

pub async fn set_state_string(
    pool: &sqlx::PgPool,
    key: &str,
    value: &str,
) -> Result<(), sqlx::Error> {
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
    .bind(value)
    .execute(pool)
    .await?;

    Ok(())
}

pub async fn delete_state_key(pool: &sqlx::PgPool, key: &str) -> Result<(), sqlx::Error> {
    sqlx::query("DELETE FROM telegram_bot_state WHERE state_key = $1")
        .bind(key)
        .execute(pool)
        .await?;

    Ok(())
}

fn pending_analytics_state_key(chat_id: i64) -> String {
    format!("analytics_pending:{chat_id}")
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AnalyticsMessageSlot {
    Selector,
    Report,
}

impl AnalyticsMessageSlot {
    fn state_value(self) -> &'static str {
        match self {
            Self::Selector => "selector",
            Self::Report => "report",
        }
    }
}

fn should_remember_analytics_message(kind: AnalyticsKind) -> bool {
    matches!(kind, AnalyticsKind::Sessions | AnalyticsKind::Metrics)
}

fn analytics_message_state_key(
    chat_id: i64,
    kind: AnalyticsKind,
    slot: AnalyticsMessageSlot,
) -> String {
    format!(
        "analytics_message:{chat_id}:{}:{}",
        kind.callback_value(),
        slot.state_value()
    )
}

async fn set_last_analytics_message_id(
    pool: &sqlx::PgPool,
    chat_id: i64,
    kind: AnalyticsKind,
    slot: AnalyticsMessageSlot,
    message_id: i64,
) -> Result<(), sqlx::Error> {
    let key = analytics_message_state_key(chat_id, kind, slot);
    set_state_i64(pool, &key, message_id).await
}

async fn get_last_analytics_message_id(
    pool: &sqlx::PgPool,
    chat_id: i64,
    kind: AnalyticsKind,
    slot: AnalyticsMessageSlot,
) -> Result<Option<i64>, sqlx::Error> {
    let key = analytics_message_state_key(chat_id, kind, slot);
    get_state_i64(pool, &key).await
}

async fn clear_last_analytics_message_id(
    pool: &sqlx::PgPool,
    chat_id: i64,
    kind: AnalyticsKind,
    slot: AnalyticsMessageSlot,
) -> Result<(), sqlx::Error> {
    let key = analytics_message_state_key(chat_id, kind, slot);
    delete_state_key(pool, &key).await
}

async fn get_pending_analytics_kind(
    pool: &sqlx::PgPool,
    chat_id: i64,
) -> Result<Option<AnalyticsKind>, sqlx::Error> {
    let key = pending_analytics_state_key(chat_id);

    Ok(get_state_string(pool, &key)
        .await?
        .and_then(|value| AnalyticsKind::parse(&value)))
}

async fn set_pending_analytics_kind(
    pool: &sqlx::PgPool,
    chat_id: i64,
    kind: AnalyticsKind,
) -> Result<(), sqlx::Error> {
    let key = pending_analytics_state_key(chat_id);
    set_state_string(pool, &key, kind.callback_value()).await
}

async fn clear_pending_analytics_kind(
    pool: &sqlx::PgPool,
    chat_id: i64,
) -> Result<(), sqlx::Error> {
    let key = pending_analytics_state_key(chat_id);
    delete_state_key(pool, &key).await
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
    created_at: DateTime<Utc>,
) -> Result<i64, sqlx::Error> {
    let row = sqlx::query(
        r#"
        INSERT INTO telegram_engine_sessions (
            imei, chat_id, trigger_heartbeat_id, prompt_message_id, ride_status_message_id,
            session_status, created_at, updated_at
        )
        VALUES ($1, $2, $3, $4, NULL, 'pending_confirmation', $5, NOW())
        RETURNING id
        "#,
    )
    .bind(imei)
    .bind(chat_id)
    .bind(trigger_heartbeat_id)
    .bind(prompt_message_id)
    .bind(created_at)
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

pub async fn update_engine_session_status(
    pool: &sqlx::PgPool,
    session_id: i64,
    session_status: &str,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        r#"
        UPDATE telegram_engine_sessions
        SET session_status = $2,
            updated_at = NOW()
        WHERE id = $1
        "#,
    )
    .bind(session_id)
    .bind(session_status)
    .execute(pool)
    .await?;

    Ok(())
}

pub async fn resolve_engine_session(
    pool: &sqlx::PgPool,
    session_id: i64,
    session_status: &str,
) -> Result<(), sqlx::Error> {
    resolve_engine_session_at(pool, session_id, session_status, Utc::now()).await
}

pub async fn resolve_engine_session_at(
    pool: &sqlx::PgPool,
    session_id: i64,
    session_status: &str,
    resolved_at: DateTime<Utc>,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        r#"
        UPDATE telegram_engine_sessions
        SET session_status = $2,
            updated_at = $3,
            resolved_at = $3
        WHERE id = $1
        "#,
    )
    .bind(session_id)
    .bind(session_status)
    .bind(resolved_at)
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

pub async fn fetch_active_engine_sessions(
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
          AND session_status IN ('pending_confirmation', 'confirmed_safe', 'reported_theft')
        ORDER BY created_at ASC, id ASC
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

pub async fn fetch_all_active_engine_sessions(
    pool: &sqlx::PgPool,
) -> Result<Vec<EngineSession>, sqlx::Error> {
    let rows = sqlx::query(
        r#"
        SELECT id, imei, chat_id, trigger_heartbeat_id, prompt_message_id, ride_status_message_id, session_status, created_at, resolved_at
        FROM telegram_engine_sessions
        WHERE session_status IN ('pending_confirmation', 'confirmed_safe', 'reported_theft')
        ORDER BY imei ASC, chat_id ASC, created_at ASC, id ASC
        "#,
    )
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

pub async fn fetch_latest_location_received_at(
    pool: &sqlx::PgPool,
    imei: &str,
) -> Result<Option<DateTime<Utc>>, sqlx::Error> {
    sqlx::query_scalar(
        r#"
        SELECT server_received_at
        FROM device_locations
        WHERE imei = $1
        ORDER BY server_received_at DESC, id DESC
        LIMIT 1
        "#,
    )
    .bind(imei)
    .fetch_optional(pool)
    .await
}

pub async fn fetch_ride_summary(
    pool: &sqlx::PgPool,
    imei: &str,
    started_at: DateTime<Utc>,
    ended_at: DateTime<Utc>,
) -> Result<Option<RideSummary>, sqlx::Error> {
    let rows = sqlx::query(
        r#"
        SELECT server_received_at, latitude, longitude, speed_kph
        FROM device_locations
        WHERE imei = $1
          AND server_received_at >= $2
          AND server_received_at <= $3
        ORDER BY server_received_at ASC, id ASC
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
            riding_seconds: 0,
            average_speed_kph: 0.0,
        }));
    }

    let location_points = rows
        .into_iter()
        .map(|row| {
            (
                row.get::<DateTime<Utc>, _>("server_received_at"),
                row.get::<f64, _>("latitude"),
                row.get::<f64, _>("longitude"),
                row.get::<i32, _>("speed_kph"),
            )
        })
        .collect::<Vec<_>>();
    let coordinate_points = location_points
        .iter()
        .map(|(_, latitude, longitude, _)| (*latitude, *longitude))
        .collect::<Vec<_>>();
    let speed_points = location_points
        .iter()
        .map(|(server_received_at, _, _, speed_kph)| (*server_received_at, *speed_kph))
        .collect::<Vec<_>>();
    let total_distance_km = total_route_distance_km(&coordinate_points);
    let riding_seconds = total_riding_seconds(&speed_points);

    let riding_hours = riding_seconds as f64 / 3600.0;
    let average_speed_kph = if riding_hours > 0.0 {
        total_distance_km / riding_hours
    } else {
        0.0
    };

    Ok(Some(RideSummary {
        total_distance_km,
        riding_seconds,
        average_speed_kph,
    }))
}

async fn fetch_analytics_ride_summary(
    pool: &sqlx::PgPool,
    imei: &str,
    sessions: &[AnalyticsSession],
    range_start: DateTime<Utc>,
    range_end: DateTime<Utc>,
) -> Result<RideSummary, sqlx::Error> {
    let mut total_distance_km = 0.0;
    let mut total_riding_seconds = 0;

    for session in sessions {
        let clipped_start = session.created_at.max(range_start);
        let clipped_end = session.resolved_at.unwrap_or(range_end).min(range_end);

        if clipped_start >= clipped_end {
            continue;
        }

        let summary = fetch_ride_summary(pool, imei, clipped_start, clipped_end).await?;
        total_distance_km += summary
            .as_ref()
            .map(|value| value.total_distance_km)
            .unwrap_or(0.0);
        total_riding_seconds += summary
            .as_ref()
            .map(|value| value.riding_seconds)
            .unwrap_or(0);
    }

    let riding_hours = total_riding_seconds as f64 / 3600.0;
    let average_speed_kph = if riding_hours > 0.0 {
        total_distance_km / riding_hours
    } else {
        0.0
    };

    Ok(RideSummary {
        total_distance_km,
        riding_seconds: total_riding_seconds,
        average_speed_kph,
    })
}

pub async fn fetch_analytics_sessions(
    pool: &sqlx::PgPool,
    imei: &str,
    chat_id: i64,
    started_at: DateTime<Utc>,
    ended_at: DateTime<Utc>,
    reference_time: DateTime<Utc>,
) -> Result<Vec<AnalyticsSession>, sqlx::Error> {
    let rows = sqlx::query(
        r#"
        SELECT id, session_status, created_at, resolved_at
        FROM telegram_engine_sessions
        WHERE imei = $1
          AND chat_id = $2
          AND created_at < $4
          AND COALESCE(resolved_at, $5) > $3
        ORDER BY created_at ASC, id ASC
        "#,
    )
    .bind(imei)
    .bind(chat_id)
    .bind(started_at)
    .bind(ended_at)
    .bind(reference_time)
    .fetch_all(pool)
    .await?;

    Ok(rows
        .into_iter()
        .map(|row| AnalyticsSession {
            id: row.get("id"),
            session_status: row.get("session_status"),
            created_at: row.get("created_at"),
            resolved_at: row.get("resolved_at"),
        })
        .collect())
}

#[cfg(test)]
mod tests {
    use std::env;

    use chrono::{TimeZone, Utc};

    use super::*;
    use crate::config::Config;
    use crate::db::Database;
    use crate::midtrans::{
        apply_midtrans_webhook, format_midtrans_payment_message_with_quote, MidtransPaymentStatus,
        MidtransWebhookApplyOutcome, MidtransWebhookNotification, PricingTier, SubscriptionPlan,
        MIDTRANS_BASIC_PLAN_CODE, MIDTRANS_OJOL_PLAN_CODE,
    };
    use crate::subscription_maintenance::{
        build_subscription_payment_quote, CUSTOMER_REFERENCED_DEVICE_FEE_IDR,
        SUBSCRIPTION_MAX_FINE_IDR,
    };

    fn database_url() -> Option<String> {
        env::var("GT06_TEST_DATABASE_URL").ok()
    }

    fn basic_plan(price_idr: i64) -> SubscriptionPlan {
        SubscriptionPlan {
            tier: PricingTier::Basic,
            plan_code: MIDTRANS_BASIC_PLAN_CODE,
            price_idr,
        }
    }

    fn ojol_plan(price_idr: i64) -> SubscriptionPlan {
        SubscriptionPlan {
            tier: PricingTier::Ojol,
            plan_code: MIDTRANS_OJOL_PLAN_CODE,
            price_idr,
        }
    }

    async fn fetch_device_id(
        pool: &sqlx::PgPool,
        imei: &str,
    ) -> Result<i64, sqlx::Error> {
        sqlx::query_scalar("SELECT id FROM devices WHERE imei = $1")
            .bind(imei)
            .fetch_one(pool)
            .await
    }

    #[test]
    fn parses_commands() {
        assert_eq!(BotCommand::parse("/start"), Some(BotCommand::Start));
        assert_eq!(BotCommand::parse("/help"), Some(BotCommand::Help));
        assert_eq!(
            BotCommand::parse("/paysupport"),
            Some(BotCommand::PaySupport)
        );
        assert_eq!(BotCommand::parse("/terms"), Some(BotCommand::Terms));
        assert_eq!(
            BotCommand::parse("/latest_location@my_bot"),
            Some(BotCommand::Unknown("/latest_location".to_string()))
        );
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
        assert_eq!(
            SessionAction::parse("engine_session:yes"),
            Some(SessionAction::Yes)
        );
        assert_eq!(
            SessionAction::parse("engine_session:no"),
            Some(SessionAction::No)
        );
        assert_eq!(SessionAction::parse("engine_session:maybe"), None);
    }

    #[test]
    fn parses_theft_alert_actions() {
        assert_eq!(
            TheftAlertAction::parse("theft_alert:stream_location:12"),
            Some(TheftAlertAction::StreamLocation {
                session_id: Some(12)
            })
        );
        assert_eq!(
            TheftAlertAction::parse("theft_alert:check_latest_status:9"),
            Some(TheftAlertAction::CheckLatestStatus {
                session_id: Some(9)
            })
        );
        assert_eq!(
            TheftAlertAction::parse("theft_alert:contact_support:5"),
            Some(TheftAlertAction::ContactSupport {
                session_id: Some(5)
            })
        );
        assert_eq!(
            TheftAlertAction::parse("theft_alert:stream_location"),
            Some(TheftAlertAction::StreamLocation { session_id: None })
        );
        assert_eq!(TheftAlertAction::parse("theft_alert:record_sound:1"), None);
    }

    #[test]
    fn parses_payment_actions() {
        assert_eq!(
            PaymentAction::parse("payment:subscribe"),
            Some(PaymentAction::Subscribe)
        );
        assert_eq!(
            PaymentAction::parse("payment:buy:monthly"),
            Some(PaymentAction::Subscribe)
        );
        assert_eq!(PaymentAction::parse("payment:refund"), None);
        assert_eq!(PaymentAction::parse("payment:subscribe:extra"), None);
        assert_eq!(PaymentAction::parse("payment:buy:yearly"), None);
        assert_eq!(PaymentAction::parse("payment:buy:monthly:extra"), None);
    }

    #[test]
    fn parses_analytics_actions() {
        assert_eq!(
            AnalyticsAction::parse("analytics:sessions:today"),
            Some(AnalyticsAction {
                kind: AnalyticsKind::Sessions,
                range: AnalyticsRange::Today,
            })
        );
        assert_eq!(
            AnalyticsAction::parse("analytics:km:custom"),
            Some(AnalyticsAction {
                kind: AnalyticsKind::TotalKm,
                range: AnalyticsRange::Custom,
            })
        );
        assert_eq!(
            AnalyticsAction::parse("analytics:time:select"),
            Some(AnalyticsAction {
                kind: AnalyticsKind::TotalDrivingTime,
                range: AnalyticsRange::Select,
            })
        );
        assert_eq!(
            AnalyticsAction::parse("analytics:metrics:month"),
            Some(AnalyticsAction {
                kind: AnalyticsKind::Metrics,
                range: AnalyticsRange::Month,
            })
        );
        assert_eq!(AnalyticsAction::parse("analytics:distance:today"), None);
        assert_eq!(AnalyticsAction::parse("analytics:km:today:extra"), None);
    }

    #[test]
    fn subscribed_start_menu_uses_status_terkini_label() {
        let keyboard = subscribed_start_menu_keyboard();

        assert_eq!(keyboard.inline_keyboard[0][1].text, "Status terkini");
        assert_eq!(keyboard.inline_keyboard[1][1].text, "Aktivitas Kendaraan");
    }

    #[test]
    fn builds_analytics_message_state_keys() {
        assert_eq!(
            analytics_message_state_key(
                12345,
                AnalyticsKind::Sessions,
                AnalyticsMessageSlot::Selector
            ),
            "analytics_message:12345:sessions:selector"
        );
        assert_eq!(
            analytics_message_state_key(
                12345,
                AnalyticsKind::Metrics,
                AnalyticsMessageSlot::Report
            ),
            "analytics_message:12345:metrics:report"
        );
    }

    #[test]
    fn resolves_analytics_preset_ranges_in_wib() {
        let reference_time = Utc.with_ymd_and_hms(2026, 5, 16, 8, 40, 9).unwrap();

        let today = resolve_preset_analytics_range(AnalyticsRange::Today, reference_time).unwrap();
        assert_eq!(
            today.started_at,
            Utc.with_ymd_and_hms(2026, 5, 15, 17, 0, 0).unwrap()
        );
        assert_eq!(today.ended_at, reference_time);

        let yesterday =
            resolve_preset_analytics_range(AnalyticsRange::Yesterday, reference_time).unwrap();
        assert_eq!(
            yesterday.started_at,
            Utc.with_ymd_and_hms(2026, 5, 14, 17, 0, 0).unwrap()
        );
        assert_eq!(
            yesterday.ended_at,
            Utc.with_ymd_and_hms(2026, 5, 15, 17, 0, 0).unwrap()
        );

        let month = resolve_preset_analytics_range(AnalyticsRange::Month, reference_time).unwrap();
        assert_eq!(
            month.started_at,
            Utc.with_ymd_and_hms(2026, 4, 30, 17, 0, 0).unwrap()
        );
        assert_eq!(month.ended_at, reference_time);
    }

    #[test]
    fn parses_custom_analytics_date_range_as_full_wib_days() {
        let range =
            parse_custom_analytics_range(AnalyticsKind::Metrics, "2026-05-16 to 2026-05-17")
                .unwrap();

        assert_eq!(
            range.started_at,
            Utc.with_ymd_and_hms(2026, 5, 15, 17, 0, 0).unwrap()
        );
        assert_eq!(
            range.ended_at,
            Utc.with_ymd_and_hms(2026, 5, 17, 17, 0, 0).unwrap()
        );
        assert!(parse_custom_analytics_range(AnalyticsKind::Metrics, "2026-05-16 08:00").is_none());
        assert!(parse_custom_analytics_range(
            AnalyticsKind::Metrics,
            "2026-05-16 08:00 to 2026-05-16 18:30"
        )
        .is_none());
        assert!(
            parse_custom_analytics_range(AnalyticsKind::Metrics, "16-05-2026 to 17-05-2026")
                .is_none()
        );
    }

    #[test]
    fn parses_custom_history_date_as_single_full_wib_day() {
        let range = parse_custom_analytics_range(AnalyticsKind::Sessions, "2026-05-16").unwrap();

        assert_eq!(
            range.started_at,
            Utc.with_ymd_and_hms(2026, 5, 15, 17, 0, 0).unwrap()
        );
        assert_eq!(
            range.ended_at,
            Utc.with_ymd_and_hms(2026, 5, 16, 17, 0, 0).unwrap()
        );
        assert!(
            parse_custom_analytics_range(AnalyticsKind::Sessions, "2026-05-16 to 2026-05-17")
                .is_none()
        );
    }

    #[test]
    fn formats_driving_time_duration() {
        assert_eq!(format_duration_compact_from_seconds(45), "45s");
        assert_eq!(format_duration_compact_from_seconds(125), "2m 5s");
        assert_eq!(format_duration_compact_from_seconds(3661), "1h 1m 1s");
    }

    #[test]
    fn formats_combined_metrics_report() {
        let range = AnalyticsDateRange {
            label: "Rentang custom".to_string(),
            started_at: Utc.with_ymd_and_hms(2026, 5, 16, 1, 0, 0).unwrap(),
            ended_at: Utc.with_ymd_and_hms(2026, 5, 16, 3, 0, 0).unwrap(),
        };
        let summary = RideSummary {
            total_distance_km: 42.0,
            riding_seconds: 7200,
            average_speed_kph: 21.0,
        };

        let text = format_metrics_report(&range, Some(&summary));

        assert_eq!(
            text,
            "🏍️ Statistik Perjalanan — Rentang custom\n\n16 May 2026 • 42.00 km ditempuh • 2h 0m waktu berkendara • 21.0 km/jam kecepatan rata-rata\n\n⚠️ Jangan lupa rutin cek kondisi motor demi keamanan, termasuk oli mesin, tekanan ban, dan rem."
        );
    }

    #[test]
    fn formats_ride_stats_multi_day_date_range_with_arrow() {
        let range = AnalyticsDateRange {
            label: "Custom Range".to_string(),
            started_at: Utc.with_ymd_and_hms(2026, 4, 30, 17, 0, 0).unwrap(),
            ended_at: Utc.with_ymd_and_hms(2026, 5, 16, 17, 0, 0).unwrap(),
        };

        assert_eq!(format_ride_stats_date_range(&range), "01 May → 16 May 2026");
    }

    #[test]
    fn formats_driving_sessions_report_with_active_session() {
        let range = AnalyticsDateRange {
            label: "Hari ini".to_string(),
            started_at: Utc.with_ymd_and_hms(2026, 5, 16, 0, 0, 0).unwrap(),
            ended_at: Utc.with_ymd_and_hms(2026, 5, 16, 3, 0, 0).unwrap(),
        };
        let sessions = vec![
            AnalyticsSessionReport {
                session: AnalyticsSession {
                    id: 1,
                    session_status: "finished".to_string(),
                    created_at: Utc.with_ymd_and_hms(2026, 5, 16, 0, 30, 0).unwrap(),
                    resolved_at: Some(Utc.with_ymd_and_hms(2026, 5, 16, 1, 0, 0).unwrap()),
                },
                clipped_start: Utc.with_ymd_and_hms(2026, 5, 16, 0, 30, 0).unwrap(),
                clipped_end: Utc.with_ymd_and_hms(2026, 5, 16, 1, 0, 0).unwrap(),
                total_distance_km: 2.35,
                riding_seconds: 900,
                route_link: Some("https://example.test/route?start_at=1&end_at=2".to_string()),
            },
            AnalyticsSessionReport {
                session: AnalyticsSession {
                    id: 2,
                    session_status: "confirmed_safe".to_string(),
                    created_at: Utc.with_ymd_and_hms(2026, 5, 16, 2, 0, 0).unwrap(),
                    resolved_at: None,
                },
                clipped_start: Utc.with_ymd_and_hms(2026, 5, 16, 2, 0, 0).unwrap(),
                clipped_end: Utc.with_ymd_and_hms(2026, 5, 16, 2, 30, 0).unwrap(),
                total_distance_km: 0.74,
                riding_seconds: 1800,
                route_link: Some("https://example.test/route?start_at=3&end_at=4".to_string()),
            },
        ];

        let text = format_driving_sessions_report(
            &range,
            &sessions,
            Some("https://example.test/full-day-route"),
            Utc.with_ymd_and_hms(2026, 5, 16, 2, 30, 0).unwrap(),
        );

        assert!(text.contains("🛣️ Laporan Perjalanan — 16 May 2026"));
        assert!(text.contains("2 sesi • 3.09 km ditempuh • 45m waktu berkendara"));
        assert!(text.contains("Perjalanan terpanjang: 09:00 → MASIH BERJALAN"));
        assert!(text.contains("1. 07:30 → 08:00 • 15m • 2.35 km"));
        assert!(text.contains("2. 09:00 → MASIH BERJALAN • 30m • 0.74 km"));
        assert!(text.contains("MASIH BERJALAN"));
        assert!(text.contains("📍 Rute Seharian\nhttps://example.test/full-day-route"));
        assert!(!text.contains("Route:"));
        assert!(!text.contains("https://example.test/route?start_at=3&end_at=4"));
    }

    #[test]
    fn driving_sessions_report_includes_all_single_day_sessions() {
        let range = AnalyticsDateRange {
            label: "Tanggal custom".to_string(),
            started_at: Utc.with_ymd_and_hms(2026, 5, 16, 0, 0, 0).unwrap(),
            ended_at: Utc.with_ymd_and_hms(2026, 5, 17, 0, 0, 0).unwrap(),
        };
        let sessions = (0..=10)
            .map(|index| {
                let created_at = Utc
                    .with_ymd_and_hms(2026, 5, 16, index as u32, 0, 0)
                    .unwrap();
                let resolved_at = created_at + chrono::Duration::minutes(30);

                AnalyticsSessionReport {
                    session: AnalyticsSession {
                        id: index as i64 + 1,
                        session_status: "finished".to_string(),
                        created_at,
                        resolved_at: Some(resolved_at),
                    },
                    clipped_start: created_at,
                    clipped_end: resolved_at,
                    total_distance_km: index as f64,
                    riding_seconds: 60,
                    route_link: Some(format!("https://example.test/route/{index}")),
                }
            })
            .collect::<Vec<_>>();

        let text = format_driving_sessions_report(
            &range,
            &sessions,
            Some("https://example.test/full-day-route"),
            Utc.with_ymd_and_hms(2026, 5, 16, 12, 0, 0).unwrap(),
        );

        assert!(text.contains("10. 16:00 → 16:30"));
        assert!(text.contains("11. 17:00 → 17:30"));
        assert!(!text.contains("Showing 10 of 11 sessions"));
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
        assert!(text.contains("perkiraan"));
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
    fn formats_inactive_subscription_engine_status_messages() {
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

        let on_text = format_inactive_subscription_engine_status_message(&heartbeat, "on");
        assert!(on_text.contains("Motor Dinyalakan"));
        assert!(on_text.contains("Perpanjang langganan"));
        assert!(on_text.contains("live tracking"));
        assert!(!on_text.contains("15 Apr 2026"));
        assert!(!on_text.contains("Activity was detected"));

        let off_text = format_inactive_subscription_engine_status_message(&heartbeat, "off");
        assert!(off_text.contains("Motor Dimatikan"));
        assert!(off_text.contains("Perpanjang langganan"));
    }

    #[test]
    fn formats_engine_on_confirmation_message() {
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

        assert_eq!(
            format_engine_on_confirmation_message(&heartbeat),
            "🚨 Engine ON Terdeteksi\n\nMotor Anda baru saja dinyalakan.\nApakah ini Anda?"
        );
    }

    #[test]
    fn formats_theft_warning_message() {
        let text = format_theft_warning_message();
        assert_eq!(
            text,
            "🚨 INDIKASI PENCURIAN\n\nMotor ini dinyalakan bukan oleh Anda. ⚠️ Gerak cepat ya, beberapa menit pertama itu penting banget kalau kejadian pencurian.\n\nTap tombol di bawah untuk mulai live tracking."
        );
    }

    #[test]
    fn formats_theft_location_message_without_location() {
        let text = format_theft_location_message(None);
        assert!(text.contains("Lokasi Terakhir"));
        assert!(text.contains("Lokasi terakhir belum tersedia"));
    }

    #[test]
    fn formats_midtrans_payment_message_with_effective_service_price() {
        let expires_at = Utc.with_ymd_and_hms(2026, 7, 4, 1, 0, 0).unwrap();
        let message = format_midtrans_payment_message_with_quote(
            basic_plan(35_000),
            "https://pay.example.test/order?a=1&b=2",
            expires_at,
            45_000,
            0,
            0,
            45_000,
        );

        assert!(message.contains("Heartbeats Basic"));
        assert!(message.contains("Rp 45.000 - 30 Hari"));
        assert!(!message.contains("Customer"));
        assert!(!message.contains("add"));
        assert!(!message.contains("Denda telat"));
        assert!(!message.contains("Total:"));
        assert!(message.contains("https://pay.example.test/order?a=1&amp;b=2"));
    }

    #[test]
    fn formats_midtrans_payment_message_with_late_sanction_total() {
        let expires_at = Utc.with_ymd_and_hms(2026, 7, 4, 1, 0, 0).unwrap();
        let message = format_midtrans_payment_message_with_quote(
            ojol_plan(30_000),
            "https://pay.example.test/order",
            expires_at,
            45_000,
            0,
            3_000,
            48_000,
        );

        assert!(message.contains("Heartbeats Ojol"));
        assert!(message.contains("Rp 45.000 - 30 Hari"));
        assert!(message.contains("Denda telat: Rp 3.000"));
        assert!(message.contains("Total: Rp 48.000"));
        assert!(!message.contains("Customer"));
    }

    #[test]
    fn formats_midtrans_payment_message_with_shipment_fee_total() {
        let expires_at = Utc.with_ymd_and_hms(2026, 7, 4, 1, 0, 0).unwrap();
        let message = format_midtrans_payment_message_with_quote(
            basic_plan(35_000),
            "https://pay.example.test/order",
            expires_at,
            35_000,
            15_000,
            0,
            50_000,
        );

        assert!(message.contains("Biaya pengiriman: Rp 15.000"));
        assert!(message.contains("Total: Rp 50.000"));
    }

    #[test]
    fn formats_theft_engine_off_message() {
        let started = Utc.with_ymd_and_hms(2026, 4, 17, 10, 0, 0).unwrap();
        let newest = Utc.with_ymd_and_hms(2026, 4, 17, 10, 5, 7).unwrap();
        let location = StoredLocation {
            imei: "866221070478388".to_string(),
            last_seen_at: Some(started),
            gps_timestamp: None,
            latitude: Some(-6.216754),
            longitude: Some(106.768455),
            speed_kph: Some(0),
            course: None,
            satellite_count: None,
        };

        let text = format_theft_engine_off_message(Some(&location), started, newest);
        assert_eq!(
            text,
            "🚨 ALERT PENCURIAN\n\nMesin motor kamu baru saja mati di situasi yang terindikasi pencurian.\n\n📍 Lokasi Terakhir Diketahui:\nhttps://maps.google.com/?q=-6.216754,106.768455\n\nGPS masih terus aktif dalam mode baterai selama daya perangkat masih ada.\n\nMesin mati terdeteksi pada 17 Apr 2026 17:00 WIB.\n\n⚠️ Segera ambil tindakan: cek live location, bagikan akses tracking, atau hubungi pihak berwajib kalau diperlukan."
        );
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
            total_distance_km: 0.69,
            riding_seconds: 276,
            average_speed_kph: 15.77,
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
        assert_eq!(
            text,
            "Ringkasan Perjalanan — 17 Apr 2026\n\n🏍️ Jarak tempuh 0.69 km\n⏱️ Waktu berkendara 4m 36s\n⚡ Kecepatan rata-rata 15.77 km/jam\n\n17:00 → 17:05 WIB\n\n🗺️ Lihat Rute\nhttps://hearthbeats-client.vercel.app/live-tracking/866221070478388?start_at=2026-04-17T10%3A00%3A00Z&end_at=2026-04-17T10%3A05%3A00Z\n\n📍 Lokasi Terakhir\nhttps://maps.google.com/?q=-6.204066,106.785514"
        );
    }

    #[test]
    fn computes_haversine_distance() {
        let distance = haversine_distance_km(-6.204066, 106.785514, -6.204500, 106.786000);
        assert!(distance > 0.05);
    }

    #[test]
    fn ignores_single_point_gps_spikes_in_route_distance() {
        let normal_start = (-6.204066, 106.785514);
        let spike = (-6.000000, 106.000000);
        let normal_end = (-6.204500, 106.786000);

        let raw_distance = haversine_distance_km(normal_start.0, normal_start.1, spike.0, spike.1)
            + haversine_distance_km(spike.0, spike.1, normal_end.0, normal_end.1);
        let cleaned_distance = total_route_distance_km(&[normal_start, spike, normal_end]);
        let expected_distance =
            haversine_distance_km(normal_start.0, normal_start.1, normal_end.0, normal_end.1);

        assert!(raw_distance > cleaned_distance * 10.0);
        assert!((cleaned_distance - expected_distance).abs() < 0.001);
    }

    #[test]
    fn calculates_riding_time_from_moving_gps_points() {
        let base = Utc.with_ymd_and_hms(2026, 5, 16, 1, 0, 0).unwrap();
        let points = vec![
            (base, 0),
            (base + chrono::Duration::minutes(1), 3),
            (base + chrono::Duration::minutes(3), 4),
            (base + chrono::Duration::minutes(4), 2),
            (base + chrono::Duration::minutes(5), 5),
            (base + chrono::Duration::minutes(11), 5),
            (base + chrono::Duration::minutes(12), 5),
        ];

        assert_eq!(total_riding_seconds(&points), 180);
        assert_eq!(total_riding_seconds(&points[..1]), 0);
    }

    #[test]
    fn formats_stream_location_message() {
        let text = format_stream_location_message(Some(
            "https://hearthbeats-client.vercel.app/live-tracking/866221070478388?start_at=2026-04-18T10%3A00%3A00Z",
        ));
        assert_eq!(
            text,
            "📍 Live Tracking Siap\n\nPantau motor kamu secara real-time di sini:\nhttps://hearthbeats-client.vercel.app/live-tracking/866221070478388?start_at=2026-04-18T10%3A00%3A00Z\n\nLink ini bisa kamu bagikan ke orang yang kamu percaya buat bantu mantau motor."
        );
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
        let link = build_history_tracking_link("866221070478388", start_at, end_at).expect("link");

        assert_eq!(
            link,
            "https://hearthbeats-client.vercel.app/live-tracking/866221070478388?start_at=2026-04-18T10%3A00%3A00Z&end_at=2026-04-18T11%3A00%3A00Z"
        );
    }

    #[test]
    fn starts_new_engine_on_session_when_no_previous_on_heartbeat() {
        let heartbeat_time = Utc.with_ymd_and_hms(2026, 4, 19, 10, 0, 0).unwrap();
        assert!(should_start_new_engine_on_session(heartbeat_time, None));
    }

    #[test]
    fn keeps_existing_engine_on_session_within_gap_window() {
        let previous_on_heartbeat = Utc.with_ymd_and_hms(2026, 4, 19, 10, 0, 0).unwrap();
        let heartbeat_time = Utc.with_ymd_and_hms(2026, 4, 19, 10, 9, 59).unwrap();

        assert!(!should_start_new_engine_on_session(
            heartbeat_time,
            Some(previous_on_heartbeat)
        ));
    }

    #[test]
    fn starts_new_engine_on_session_at_exact_gap_threshold() {
        let previous_on_heartbeat = Utc.with_ymd_and_hms(2026, 4, 19, 10, 0, 0).unwrap();
        let heartbeat_time = Utc.with_ymd_and_hms(2026, 4, 19, 10, 10, 0).unwrap();

        assert!(should_start_new_engine_on_session(
            heartbeat_time,
            Some(previous_on_heartbeat)
        ));
    }

    #[test]
    fn starts_new_engine_on_session_after_gap_threshold() {
        let previous_on_heartbeat = Utc.with_ymd_and_hms(2026, 4, 19, 10, 0, 0).unwrap();
        let heartbeat_time = Utc.with_ymd_and_hms(2026, 4, 19, 10, 10, 1).unwrap();

        assert!(should_start_new_engine_on_session(
            heartbeat_time,
            Some(previous_on_heartbeat)
        ));
    }

    #[test]
    fn detects_stale_engine_session_at_heartbeat_timeout() {
        let latest_heartbeat = Utc.with_ymd_and_hms(2026, 4, 19, 10, 0, 0).unwrap();

        assert!(!should_finish_stale_engine_session(
            Utc.with_ymd_and_hms(2026, 4, 19, 10, 9, 59).unwrap(),
            latest_heartbeat
        ));
        assert!(should_finish_stale_engine_session(
            Utc.with_ymd_and_hms(2026, 4, 19, 10, 10, 0).unwrap(),
            latest_heartbeat
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
            session_status: "reported_theft".to_string(),
            created_at: Utc.with_ymd_and_hms(2026, 4, 17, 10, 0, 0).unwrap(),
            resolved_at: Some(Utc.with_ymd_and_hms(2026, 4, 17, 10, 5, 7).unwrap()),
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
        assert_eq!(
            text,
            "📍 Status Motor\n\nhttps://maps.google.com/?q=-6.204066,106.785514\n\nDIAM • Diperbarui 12dtk lalu\nMesin: OFF • GPS: BAIK • Daya: TIDAK DIKETAHUI\n\nSesi terakhir selesai pada 17:05:07 WIB."
        );
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

        let text = format_latest_motor_status_initial_message(&session, None, None, requested_at);
        assert!(text.contains("Lokasi belum tersedia."));
        assert!(text.contains("TIDAK DIKETAHUI • Diperbarui tidak diketahui"));
        assert!(text.contains("Sesi aktif sejak 17:00:00 WIB"));
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
        assert!(text.contains("Daya: HABIS"));
        assert!(text.contains(
            "⚠️ Baterai GPS habis. Update baru kemungkinan akan masuk lagi setelah motor dinyalakan."
        ));
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
        let database = Database::connect(&config)
            .await?
            .expect("database configured");
        sqlx::query("TRUNCATE telegram_bot_state, telegram_device_notifications RESTART IDENTITY")
            .execute(database.pool())
            .await?;

        set_state_i64(database.pool(), "last_telegram_update_id", 42).await?;
        set_state_i64(database.pool(), "last_notified_heartbeat_id", 77).await?;
        set_pending_analytics_kind(database.pool(), 12345, AnalyticsKind::TotalKm).await?;

        assert_eq!(
            get_state_i64(database.pool(), "last_telegram_update_id").await?,
            Some(42)
        );
        assert_eq!(
            get_state_i64(database.pool(), "last_notified_heartbeat_id").await?,
            Some(77)
        );
        assert_eq!(
            get_pending_analytics_kind(database.pool(), 12345).await?,
            Some(AnalyticsKind::TotalKm)
        );
        clear_pending_analytics_kind(database.pool(), 12345).await?;
        assert_eq!(
            get_pending_analytics_kind(database.pool(), 12345).await?,
            None
        );

        set_last_analytics_message_id(
            database.pool(),
            12345,
            AnalyticsKind::Sessions,
            AnalyticsMessageSlot::Selector,
            9001,
        )
        .await?;
        set_last_analytics_message_id(
            database.pool(),
            12345,
            AnalyticsKind::Metrics,
            AnalyticsMessageSlot::Report,
            9002,
        )
        .await?;
        assert_eq!(
            get_last_analytics_message_id(
                database.pool(),
                12345,
                AnalyticsKind::Sessions,
                AnalyticsMessageSlot::Selector,
            )
            .await?,
            Some(9001)
        );
        assert_eq!(
            get_last_analytics_message_id(
                database.pool(),
                12345,
                AnalyticsKind::Metrics,
                AnalyticsMessageSlot::Report,
            )
            .await?,
            Some(9002)
        );
        clear_last_analytics_message_id(
            database.pool(),
            12345,
            AnalyticsKind::Sessions,
            AnalyticsMessageSlot::Selector,
        )
        .await?;
        assert_eq!(
            get_last_analytics_message_id(
                database.pool(),
                12345,
                AnalyticsKind::Sessions,
                AnalyticsMessageSlot::Selector,
            )
            .await?,
            None
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
        let database = Database::connect(&config)
            .await?
            .expect("database configured");
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
    async fn stores_and_restores_notification_state() -> Result<(), Box<dyn std::error::Error>> {
        let Some(database_url) = database_url() else {
            return Ok(());
        };

        let config = Config::from_pairs([
            ("DATABASE_URL", database_url.as_str()),
            ("DATABASE_MAX_CONNECTIONS", "1"),
        ]);
        let database = Database::connect(&config)
            .await?
            .expect("database configured");
        sqlx::query("TRUNCATE telegram_device_notifications RESTART IDENTITY")
            .execute(database.pool())
            .await?;

        upsert_notification_state(database.pool(), "866221070478388", 12345, "on", 777, 55).await?;

        let state = fetch_notification_state(database.pool(), "866221070478388", 12345)
            .await?
            .expect("state should exist");
        assert_eq!(state.last_status, "on");
        assert_eq!(state.last_message_id, 777);
        assert_eq!(state.last_heartbeat_id, 55);

        Ok(())
    }

    #[tokio::test]
    async fn creates_and_resolves_engine_session() -> Result<(), Box<dyn std::error::Error>> {
        let Some(database_url) = database_url() else {
            return Ok(());
        };

        let config = Config::from_pairs([
            ("DATABASE_URL", database_url.as_str()),
            ("DATABASE_MAX_CONNECTIONS", "1"),
        ]);
        let database = Database::connect(&config)
            .await?
            .expect("database configured");
        sqlx::query("TRUNCATE telegram_engine_sessions RESTART IDENTITY")
            .execute(database.pool())
            .await?;

        let started_at = Utc.with_ymd_and_hms(2026, 4, 24, 10, 0, 0).unwrap();
        let session_id = create_engine_session(
            database.pool(),
            "866221070478388",
            12345,
            88,
            999,
            started_at,
        )
        .await?;
        let session = fetch_engine_session_by_prompt_message(database.pool(), 12345, 999)
            .await?
            .expect("session should exist");
        assert_eq!(session.id, session_id);
        assert_eq!(session.session_status, "pending_confirmation");
        assert_eq!(session.created_at, started_at);
        assert_eq!(session.resolved_at, None);

        update_engine_session_status(database.pool(), session_id, "confirmed_safe").await?;
        let confirmed = fetch_engine_session_by_prompt_message(database.pool(), 12345, 999)
            .await?
            .expect("confirmed session should exist");
        assert_eq!(confirmed.session_status, "confirmed_safe");
        assert_eq!(confirmed.resolved_at, None);

        resolve_engine_session(database.pool(), session_id, "finished").await?;
        let resolved = fetch_engine_session_by_prompt_message(database.pool(), 12345, 999)
            .await?
            .expect("finished session should exist");
        assert_eq!(resolved.session_status, "finished");
        assert!(resolved.resolved_at.is_some());

        Ok(())
    }

    #[tokio::test]
    async fn checks_active_subscription_state() -> Result<(), Box<dyn std::error::Error>> {
        let Some(database_url) = database_url() else {
            return Ok(());
        };

        let config = Config::from_pairs([
            ("DATABASE_URL", database_url.as_str()),
            ("DATABASE_MAX_CONNECTIONS", "1"),
        ]);
        let database = Database::connect(&config)
            .await?
            .expect("database configured");
        let active_user_id = 8_881_000_001_i64;
        let expired_user_id = 8_881_000_002_i64;
        let missing_user_id = 8_881_000_003_i64;
        let reference_time = Utc.with_ymd_and_hms(2026, 4, 25, 10, 0, 0).unwrap();

        sqlx::query("DELETE FROM telegram_subscriptions WHERE telegram_user_id BETWEEN $1 AND $2")
            .bind(active_user_id)
            .bind(missing_user_id)
            .execute(database.pool())
            .await?;
        sqlx::query("DELETE FROM telegram_users WHERE telegram_user_id BETWEEN $1 AND $2")
            .bind(active_user_id)
            .bind(missing_user_id)
            .execute(database.pool())
            .await?;

        sqlx::query(
            r#"
            INSERT INTO telegram_users (
                telegram_user_id, chat_id, bound_imei, registration_status, created_at, updated_at
            )
            VALUES
                ($1, $1, '999888777666551', 'bound', NOW(), NOW()),
                ($2, $2, '999888777666552', 'bound', NOW(), NOW()),
                ($3, $3, '999888777666553', 'bound', NOW(), NOW())
            "#,
        )
        .bind(active_user_id)
        .bind(expired_user_id)
        .bind(missing_user_id)
        .execute(database.pool())
        .await?;

        sqlx::query(
            r#"
            INSERT INTO telegram_subscriptions (
                telegram_user_id, chat_id, plan_code, status,
                current_period_start_at, current_period_end_at, created_at, updated_at
            )
            VALUES
                ($1, $1, $3, 'active', $4, $5, NOW(), NOW()),
                ($2, $2, $3, 'active', $4, $6, NOW(), NOW())
            "#,
        )
        .bind(active_user_id)
        .bind(expired_user_id)
        .bind(MIDTRANS_BASIC_PLAN_CODE)
        .bind(reference_time - chrono::Duration::days(1))
        .bind(reference_time + chrono::Duration::days(1))
        .bind(reference_time - chrono::Duration::seconds(1))
        .execute(database.pool())
        .await?;

        assert!(has_active_subscription(database.pool(), active_user_id, reference_time).await?);
        assert!(!has_active_subscription(database.pool(), expired_user_id, reference_time).await?);
        assert!(!has_active_subscription(database.pool(), missing_user_id, reference_time).await?);

        Ok(())
    }

    #[tokio::test]
    async fn builds_subscription_payment_quote_with_late_sanctions(
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
            .expect("database configured");
        let active_user_id = 8_885_000_001_i64;
        let late_user_id = 8_885_000_002_i64;
        let very_late_user_id = 8_885_000_003_i64;
        let now = Utc.with_ymd_and_hms(2026, 7, 4, 1, 0, 0).unwrap();
        sqlx::query(
            "DELETE FROM telegram_subscription_sanctions WHERE telegram_user_id BETWEEN $1 AND $2",
        )
        .bind(active_user_id)
        .bind(very_late_user_id)
        .execute(database.pool())
        .await?;
        sqlx::query("DELETE FROM telegram_subscriptions WHERE telegram_user_id BETWEEN $1 AND $2")
            .bind(active_user_id)
            .bind(very_late_user_id)
            .execute(database.pool())
            .await?;
        sqlx::query("DELETE FROM telegram_users WHERE telegram_user_id BETWEEN $1 AND $2")
            .bind(active_user_id)
            .bind(very_late_user_id)
            .execute(database.pool())
            .await?;

        sqlx::query(
            r#"
            INSERT INTO telegram_users (
                telegram_user_id, chat_id, bound_imei, registration_status, created_at, updated_at
            )
            VALUES
                ($1, $1, '999888777666561', 'bound', NOW(), NOW()),
                ($2, $2, '999888777666562', 'bound', NOW(), NOW()),
                ($3, $3, '999888777666563', 'bound', NOW(), NOW())
            "#,
        )
        .bind(active_user_id)
        .bind(late_user_id)
        .bind(very_late_user_id)
        .execute(database.pool())
        .await?;

        sqlx::query(
            r#"
            INSERT INTO telegram_subscriptions (
                telegram_user_id, chat_id, plan_code, status,
                current_period_start_at, current_period_end_at, created_at, updated_at
            )
            VALUES
                ($1, $1, $4, 'active', $5, $6, NOW(), NOW()),
                ($2, $2, $4, 'active', $5, $7, NOW(), NOW()),
                ($3, $3, $4, 'active', $5, $8, NOW(), NOW())
            "#,
        )
        .bind(active_user_id)
        .bind(late_user_id)
        .bind(very_late_user_id)
        .bind(MIDTRANS_BASIC_PLAN_CODE)
        .bind(now - chrono::Duration::days(40))
        .bind(now + chrono::Duration::days(1))
        .bind(Utc.with_ymd_and_hms(2026, 6, 30, 17, 0, 0).unwrap())
        .bind(Utc.with_ymd_and_hms(2026, 6, 24, 17, 0, 0).unwrap())
        .execute(database.pool())
        .await?;

        let active_quote = build_subscription_payment_quote(
            database.pool(),
            active_user_id,
            basic_plan(35_000),
            now,
        )
        .await?;
        let late_quote = build_subscription_payment_quote(
            database.pool(),
            late_user_id,
            basic_plan(35_000),
            now,
        )
        .await?;
        let very_late_quote = build_subscription_payment_quote(
            database.pool(),
            very_late_user_id,
            basic_plan(35_000),
            now,
        )
        .await?;

        assert_eq!(active_quote.total_amount_idr, 35_000);
        assert_eq!(active_quote.customer_reference_fee_idr, 0);
        assert_eq!(late_quote.fine_amount_idr, 3_000);
        assert_eq!(late_quote.total_amount_idr, 38_000);
        assert_eq!(very_late_quote.fine_amount_idr, SUBSCRIPTION_MAX_FINE_IDR);
        assert_eq!(very_late_quote.total_amount_idr, 42_000);

        Ok(())
    }

    #[tokio::test]
    async fn builds_subscription_payment_quote_with_customer_referenced_device_fee(
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
            .expect("database configured");
        let normal_user_id = 8_885_000_004_i64;
        let referenced_user_id = 8_885_000_005_i64;
        let late_referenced_user_id = 8_885_000_006_i64;
        let normal_imei = "999888777666564";
        let referenced_imei = "999888777666565";
        let late_referenced_imei = "999888777666566";
        let now = Utc.with_ymd_and_hms(2026, 7, 4, 1, 0, 0).unwrap();
        sqlx::query(
            "DELETE FROM telegram_subscription_sanctions WHERE telegram_user_id BETWEEN $1 AND $2",
        )
        .bind(normal_user_id)
        .bind(late_referenced_user_id)
        .execute(database.pool())
        .await?;
        sqlx::query("DELETE FROM telegram_subscriptions WHERE telegram_user_id BETWEEN $1 AND $2")
            .bind(normal_user_id)
            .bind(late_referenced_user_id)
            .execute(database.pool())
            .await?;
        sqlx::query("DELETE FROM telegram_users WHERE telegram_user_id BETWEEN $1 AND $2")
            .bind(normal_user_id)
            .bind(late_referenced_user_id)
            .execute(database.pool())
            .await?;
        sqlx::query("DELETE FROM devices WHERE imei = ANY($1)")
            .bind(vec![normal_imei, referenced_imei, late_referenced_imei])
            .execute(database.pool())
            .await?;
        sqlx::query("DELETE FROM customers WHERE id_card LIKE 'quote-test-%'")
            .execute(database.pool())
            .await?;

        let customer_id: i64 = sqlx::query_scalar(
            r#"
            INSERT INTO customers (
                name, phone_number, address, imei, id_card, created_at, updated_at
            )
            VALUES ('Quote Test Customer', '081234567890', 'Jakarta', $1, 'quote-test-1', NOW(), NOW())
            RETURNING id
            "#,
        )
        .bind(referenced_imei)
        .fetch_one(database.pool())
        .await?;

        sqlx::query(
            r#"
            INSERT INTO devices (
                imei, first_seen_at, last_seen_at, latest_peer_addr,
                referenced_by_customer_id, created_at, updated_at
            )
            VALUES
                ($1, NOW(), NOW(), 'test', NULL, NOW(), NOW()),
                ($2, NOW(), NOW(), 'test', $4, NOW(), NOW()),
                ($3, NOW(), NOW(), 'test', $4, NOW(), NOW())
            "#,
        )
        .bind(normal_imei)
        .bind(referenced_imei)
        .bind(late_referenced_imei)
        .bind(customer_id)
        .execute(database.pool())
        .await?;

        sqlx::query(
            r#"
            INSERT INTO telegram_users (
                telegram_user_id, chat_id, bound_imei, registration_status, created_at, updated_at
            )
            VALUES
                ($1, $1, $4, 'bound', NOW(), NOW()),
                ($2, $2, $5, 'bound', NOW(), NOW()),
                ($3, $3, $6, 'bound', NOW(), NOW())
            "#,
        )
        .bind(normal_user_id)
        .bind(referenced_user_id)
        .bind(late_referenced_user_id)
        .bind(normal_imei)
        .bind(referenced_imei)
        .bind(late_referenced_imei)
        .execute(database.pool())
        .await?;

        sqlx::query(
            r#"
            INSERT INTO telegram_subscriptions (
                telegram_user_id, chat_id, plan_code, status,
                current_period_start_at, current_period_end_at, created_at, updated_at
            )
            VALUES ($1, $1, $2, 'active', $3, $4, NOW(), NOW())
            "#,
        )
        .bind(late_referenced_user_id)
        .bind(MIDTRANS_BASIC_PLAN_CODE)
        .bind(now - chrono::Duration::days(40))
        .bind(Utc.with_ymd_and_hms(2026, 6, 30, 17, 0, 0).unwrap())
        .execute(database.pool())
        .await?;

        let normal_quote = build_subscription_payment_quote(
            database.pool(),
            normal_user_id,
            basic_plan(35_000),
            now,
        )
        .await?;
        let referenced_quote = build_subscription_payment_quote(
            database.pool(),
            referenced_user_id,
            basic_plan(35_000),
            now,
        )
        .await?;
        let late_referenced_quote = build_subscription_payment_quote(
            database.pool(),
            late_referenced_user_id,
            basic_plan(35_000),
            now,
        )
        .await?;

        assert_eq!(normal_quote.customer_reference_fee_idr, 0);
        assert_eq!(normal_quote.total_amount_idr, 35_000);
        assert_eq!(
            referenced_quote.customer_reference_fee_idr,
            CUSTOMER_REFERENCED_DEVICE_FEE_IDR
        );
        assert_eq!(referenced_quote.total_amount_idr, 45_000);
        assert_eq!(
            late_referenced_quote.customer_reference_fee_idr,
            CUSTOMER_REFERENCED_DEVICE_FEE_IDR
        );
        assert_eq!(late_referenced_quote.fine_amount_idr, 3_000);
        assert_eq!(late_referenced_quote.total_amount_idr, 48_000);

        Ok(())
    }

    #[tokio::test]
    async fn builds_subscription_payment_quote_for_ojol_tier(
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
            .expect("database configured");
        let telegram_user_id = 8_885_000_010_i64;
        let now = Utc.with_ymd_and_hms(2026, 7, 4, 1, 0, 0).unwrap();

        sqlx::query("DELETE FROM telegram_subscriptions WHERE telegram_user_id = $1")
            .bind(telegram_user_id)
            .execute(database.pool())
            .await?;
        sqlx::query("DELETE FROM telegram_users WHERE telegram_user_id = $1")
            .bind(telegram_user_id)
            .execute(database.pool())
            .await?;
        sqlx::query("DELETE FROM devices WHERE imei = '999888777666567'")
            .execute(database.pool())
            .await?;

        sqlx::query(
            r#"
            INSERT INTO devices (
                imei, first_seen_at, last_seen_at, latest_peer_addr, pricing_tier, created_at, updated_at
            )
            VALUES ('999888777666567', NOW(), NOW(), '127.0.0.1:5000', 'ojol', NOW(), NOW())
            "#
        )
        .execute(database.pool())
        .await?;

        sqlx::query(
            r#"
            INSERT INTO telegram_users (
                telegram_user_id, chat_id, bound_imei, registration_status, created_at, updated_at
            )
            VALUES ($1, $1, '999888777666567', 'bound', NOW(), NOW())
            "#,
        )
        .bind(telegram_user_id)
        .execute(database.pool())
        .await?;

        let quote =
            build_subscription_payment_quote(database.pool(), telegram_user_id, ojol_plan(30_000), now)
                .await?;

        assert_eq!(quote.base_amount_idr, 30_000);
        assert_eq!(quote.fine_amount_idr, 0);
        assert_eq!(quote.total_amount_idr, 30_000);

        Ok(())
    }

    #[tokio::test]
    async fn adds_shipment_fee_only_on_first_device_payment(
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
            .expect("database configured");
        let telegram_user_id = 8_885_000_030_i64;
        let imei = "999888777666569";
        let now = Utc.with_ymd_and_hms(2026, 7, 4, 1, 0, 0).unwrap();

        sqlx::query("DELETE FROM telegram_payment_events WHERE telegram_user_id = $1")
            .bind(telegram_user_id)
            .execute(database.pool())
            .await?;
        sqlx::query("DELETE FROM telegram_subscriptions WHERE telegram_user_id = $1")
            .bind(telegram_user_id)
            .execute(database.pool())
            .await?;
        sqlx::query("DELETE FROM telegram_users WHERE telegram_user_id = $1")
            .bind(telegram_user_id)
            .execute(database.pool())
            .await?;
        sqlx::query("DELETE FROM devices WHERE imei = $1")
            .bind(imei)
            .execute(database.pool())
            .await?;

        sqlx::query(
            r#"
            INSERT INTO devices (
                imei, first_seen_at, last_seen_at, latest_peer_addr, pricing_tier, shipment_fee_idr, created_at, updated_at
            )
            VALUES ($1, NOW(), NOW(), '127.0.0.1:5000', 'basic', 15000, NOW(), NOW())
            "#,
        )
        .bind(imei)
        .execute(database.pool())
        .await?;

        sqlx::query(
            r#"
            INSERT INTO telegram_users (
                telegram_user_id, chat_id, bound_imei, registration_status, created_at, updated_at
            )
            VALUES ($1, $1, $2, 'bound', NOW(), NOW())
            "#,
        )
        .bind(telegram_user_id)
        .bind(imei)
        .execute(database.pool())
        .await?;

        let first_quote =
            build_subscription_payment_quote(database.pool(), telegram_user_id, basic_plan(35_000), now)
                .await?;
        assert_eq!(first_quote.shipment_fee_idr, 15_000);
        assert_eq!(first_quote.total_amount_idr, 50_000);

        let device_id = fetch_device_id(database.pool(), imei).await?;
        sqlx::query(
            r#"
            INSERT INTO telegram_payment_events (
                telegram_user_id, chat_id, device_id, imei, subscription_id, payment_provider, payment_kind,
                payment_status, plan_code, currency, gross_amount_idr, period_days,
                provider_order_id, provider_transaction_id, payment_type, paid_at,
                raw_webhook_notification, created_at, updated_at
            )
            VALUES (
                $1, $1, $2, $3, NULL, 'midtrans', 'snap_subscription',
                'paid', 'monthly_basic', 'IDR', 50000, 30,
                $4, $5, 'qris', $6, $7::jsonb, NOW(), NOW()
            )
            "#,
        )
        .bind(telegram_user_id)
        .bind(device_id)
        .bind(imei)
        .bind("shipment-paid-order")
        .bind("shipment-paid-transaction")
        .bind(now)
        .bind(r#"{"transaction_status":"settlement","gross_amount":"50000.00"}"#)
        .execute(database.pool())
        .await?;

        let second_quote =
            build_subscription_payment_quote(database.pool(), telegram_user_id, basic_plan(35_000), now)
                .await?;
        assert_eq!(second_quote.shipment_fee_idr, 0);
        assert_eq!(second_quote.total_amount_idr, 35_000);

        Ok(())
    }

    #[tokio::test]
    async fn applies_overdue_sanction_even_when_next_payment_plan_changes(
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
            .expect("database configured");
        let telegram_user_id = 8_885_000_020_i64;
        let chat_id = 8_885_000_021_i64;
        let imei = "999888777666568";
        let now = Utc.with_ymd_and_hms(2026, 7, 4, 1, 0, 0).unwrap();

        sqlx::query("DELETE FROM telegram_subscription_sanctions WHERE telegram_user_id = $1")
            .bind(telegram_user_id)
            .execute(database.pool())
            .await?;
        sqlx::query("DELETE FROM telegram_payment_events WHERE telegram_user_id = $1")
            .bind(telegram_user_id)
            .execute(database.pool())
            .await?;
        sqlx::query("DELETE FROM telegram_subscriptions WHERE telegram_user_id = $1")
            .bind(telegram_user_id)
            .execute(database.pool())
            .await?;
        sqlx::query("DELETE FROM telegram_users WHERE telegram_user_id = $1")
            .bind(telegram_user_id)
            .execute(database.pool())
            .await?;
        sqlx::query("DELETE FROM devices WHERE imei = $1")
            .bind(imei)
            .execute(database.pool())
            .await?;

        sqlx::query(
            r#"
            INSERT INTO devices (
                imei, first_seen_at, last_seen_at, latest_peer_addr, pricing_tier, created_at, updated_at
            )
            VALUES ($1, NOW(), NOW(), '127.0.0.1:5000', 'ojol', NOW(), NOW())
            "#,
        )
        .bind(imei)
        .execute(database.pool())
        .await?;

        sqlx::query(
            r#"
            INSERT INTO telegram_users (
                telegram_user_id, chat_id, bound_imei, registration_status, created_at, updated_at
            )
            VALUES ($1, $2, $3, 'bound', NOW(), NOW())
            "#,
        )
        .bind(telegram_user_id)
        .bind(chat_id)
        .bind(imei)
        .execute(database.pool())
        .await?;

        sqlx::query(
            r#"
            INSERT INTO telegram_subscriptions (
                telegram_user_id, chat_id, plan_code, status,
                current_period_start_at, current_period_end_at, created_at, updated_at
            )
            VALUES ($1, $2, 'monthly_basic', 'active', $3, $4, NOW(), NOW())
            "#,
        )
        .bind(telegram_user_id)
        .bind(chat_id)
        .bind(now - chrono::Duration::days(40))
        .bind(Utc.with_ymd_and_hms(2026, 6, 30, 17, 0, 0).unwrap())
        .execute(database.pool())
        .await?;

        let quote =
            build_subscription_payment_quote(database.pool(), telegram_user_id, ojol_plan(30_000), now)
                .await?;

        assert_eq!(quote.base_amount_idr, 30_000);
        assert_eq!(quote.fine_amount_idr, 3_000);
        assert_eq!(quote.total_amount_idr, 33_000);

        Ok(())
    }

    #[tokio::test]
    async fn fetches_notification_recipients_with_subscription_state(
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
            .expect("database configured");
        let first_user_id = 8_882_000_001_i64;
        let last_user_id = 8_882_000_005_i64;
        let imei = "999888777666554";

        sqlx::query("DELETE FROM telegram_subscriptions WHERE telegram_user_id BETWEEN $1 AND $2")
            .bind(first_user_id)
            .bind(last_user_id)
            .execute(database.pool())
            .await?;
        sqlx::query("DELETE FROM telegram_users WHERE telegram_user_id BETWEEN $1 AND $2")
            .bind(first_user_id)
            .bind(last_user_id)
            .execute(database.pool())
            .await?;

        sqlx::query(
            r#"
            INSERT INTO telegram_users (
                telegram_user_id, chat_id, bound_imei, registration_status, created_at, updated_at
            )
            VALUES
                ($1, 91001, $6, 'bound', NOW(), NOW()),
                ($2, 91002, $6, 'bound', NOW(), NOW()),
                ($3, 91003, $6, 'bound', NOW(), NOW()),
                ($4, 91004, $6, 'awaiting_imei', NOW(), NOW()),
                ($5, 91005, NULL, 'bound', NOW(), NOW())
            "#,
        )
        .bind(first_user_id)
        .bind(first_user_id + 1)
        .bind(first_user_id + 2)
        .bind(first_user_id + 3)
        .bind(last_user_id)
        .bind(imei)
        .execute(database.pool())
        .await?;

        sqlx::query(
            r#"
            INSERT INTO telegram_subscriptions (
                telegram_user_id, chat_id, plan_code, status,
                current_period_start_at, current_period_end_at, created_at, updated_at
            )
            VALUES
                ($1, 91001, $6, 'active', NOW() - INTERVAL '1 day', NOW() + INTERVAL '1 day', NOW(), NOW()),
                ($2, 91002, $6, 'active', NOW() - INTERVAL '2 days', NOW() - INTERVAL '1 day', NOW(), NOW()),
                ($4, 91004, $6, 'active', NOW() - INTERVAL '1 day', NOW() + INTERVAL '1 day', NOW(), NOW()),
                ($5, 91005, $6, 'active', NOW() - INTERVAL '1 day', NOW() + INTERVAL '1 day', NOW(), NOW())
            "#,
        )
        .bind(first_user_id)
        .bind(first_user_id + 1)
        .bind(first_user_id + 2)
        .bind(first_user_id + 3)
        .bind(last_user_id)
        .bind(MIDTRANS_BASIC_PLAN_CODE)
        .execute(database.pool())
        .await?;

        let recipients = fetch_notification_recipients_for_imei(database.pool(), imei).await?;
        assert_eq!(
            recipients,
            vec![
                NotificationRecipient {
                    chat_id: 91001,
                    has_active_subscription: true,
                },
                NotificationRecipient {
                    chat_id: 91002,
                    has_active_subscription: false,
                },
                NotificationRecipient {
                    chat_id: 91003,
                    has_active_subscription: false,
                },
            ]
        );

        Ok(())
    }

    #[tokio::test]
    async fn resolves_engine_session_at_specific_time() -> Result<(), Box<dyn std::error::Error>> {
        let Some(database_url) = database_url() else {
            return Ok(());
        };

        let config = Config::from_pairs([
            ("DATABASE_URL", database_url.as_str()),
            ("DATABASE_MAX_CONNECTIONS", "1"),
        ]);
        let database = Database::connect(&config)
            .await?
            .expect("database configured");
        let chat_id = 8_883_000_001_i64;
        let started_at = Utc.with_ymd_and_hms(2026, 4, 25, 9, 0, 0).unwrap();
        let ended_at = Utc.with_ymd_and_hms(2026, 4, 25, 10, 0, 0).unwrap();

        sqlx::query("DELETE FROM telegram_engine_sessions WHERE chat_id = $1")
            .bind(chat_id)
            .execute(database.pool())
            .await?;

        let session_id = create_engine_session(
            database.pool(),
            "999888777666557",
            chat_id,
            123,
            456,
            started_at,
        )
        .await?;

        resolve_engine_session_at(database.pool(), session_id, "finished", ended_at).await?;
        let session = fetch_engine_session_by_prompt_message(database.pool(), chat_id, 456)
            .await?
            .expect("session should exist");

        assert_eq!(session.session_status, "finished");
        assert_eq!(session.resolved_at, Some(ended_at));

        Ok(())
    }

    #[tokio::test]
    async fn fetches_analytics_sessions_with_active_and_overlapping_rows(
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
            .expect("database configured");
        let imei = "999888777666558";
        let chat_id = 8_884_000_001_i64;

        sqlx::query("DELETE FROM telegram_engine_sessions WHERE imei = $1 AND chat_id = $2")
            .bind(imei)
            .bind(chat_id)
            .execute(database.pool())
            .await?;

        let range_start = Utc.with_ymd_and_hms(2026, 5, 16, 1, 0, 0).unwrap();
        let range_end = Utc.with_ymd_and_hms(2026, 5, 16, 3, 0, 0).unwrap();
        let reference_time = Utc.with_ymd_and_hms(2026, 5, 16, 2, 30, 0).unwrap();

        sqlx::query(
            r#"
            INSERT INTO telegram_engine_sessions (
                imei, chat_id, trigger_heartbeat_id, prompt_message_id, ride_status_message_id,
                session_status, created_at, updated_at, resolved_at
            )
            VALUES
                ($1, $2, 1, 101, NULL, 'finished', $3, NOW(), $4),
                ($1, $2, 2, 102, NULL, 'confirmed_safe', $5, NOW(), NULL),
                ($1, $2, 3, 103, NULL, 'finished', $6, NOW(), $7)
            "#,
        )
        .bind(imei)
        .bind(chat_id)
        .bind(Utc.with_ymd_and_hms(2026, 5, 16, 0, 30, 0).unwrap())
        .bind(Utc.with_ymd_and_hms(2026, 5, 16, 1, 30, 0).unwrap())
        .bind(Utc.with_ymd_and_hms(2026, 5, 16, 2, 0, 0).unwrap())
        .bind(Utc.with_ymd_and_hms(2026, 5, 15, 22, 0, 0).unwrap())
        .bind(Utc.with_ymd_and_hms(2026, 5, 15, 23, 0, 0).unwrap())
        .execute(database.pool())
        .await?;

        let sessions = fetch_analytics_sessions(
            database.pool(),
            imei,
            chat_id,
            range_start,
            range_end,
            reference_time,
        )
        .await?;
        let total_seconds = total_clipped_session_seconds(&sessions, range_start, reference_time);

        assert_eq!(sessions.len(), 2);
        assert_eq!(total_seconds, 3600);

        Ok(())
    }

    #[tokio::test]
    async fn fetches_total_km_from_device_locations() -> Result<(), Box<dyn std::error::Error>> {
        let Some(database_url) = database_url() else {
            return Ok(());
        };

        let config = Config::from_pairs([
            ("DATABASE_URL", database_url.as_str()),
            ("DATABASE_MAX_CONNECTIONS", "1"),
        ]);
        let database = Database::connect(&config)
            .await?
            .expect("database configured");
        let imei = "999888777666559";

        sqlx::query("DELETE FROM device_locations WHERE imei = $1")
            .bind(imei)
            .execute(database.pool())
            .await?;
        sqlx::query("DELETE FROM devices WHERE imei = $1")
            .bind(imei)
            .execute(database.pool())
            .await?;

        let device_id: i64 = sqlx::query_scalar(
            r#"
            INSERT INTO devices (imei, first_seen_at, last_seen_at, created_at, updated_at)
            VALUES ($1, NOW(), NOW(), NOW(), NOW())
            RETURNING id
            "#,
        )
        .bind(imei)
        .fetch_one(database.pool())
        .await?;

        let started_at = Utc.with_ymd_and_hms(2026, 5, 16, 1, 0, 0).unwrap();
        let ended_at = Utc.with_ymd_and_hms(2026, 5, 16, 2, 0, 0).unwrap();

        sqlx::query(
            r#"
            INSERT INTO device_locations (
                device_id, imei, server_received_at, gps_timestamp, protocol_number,
                packet_family, latitude, longitude, speed_kph, course, course_status,
                satellite_count, gps_info_length, extra_data_hex, peer_addr
            )
            VALUES
                ($1, $2, $3, $3::timestamp, 18, 'location', -6.204066, 106.785514, 0, 0, 0, 8, 12, '', '127.0.0.1:5000'),
                ($1, $2, $4, $4::timestamp, 18, 'location', -6.204500, 106.786000, 0, 0, 0, 8, 12, '', '127.0.0.1:5000')
            "#,
        )
        .bind(device_id)
        .bind(imei)
        .bind(started_at)
        .bind(ended_at)
        .execute(database.pool())
        .await?;

        let summary = fetch_ride_summary(database.pool(), imei, started_at, ended_at)
            .await?
            .expect("summary");

        assert!(summary.total_distance_km > 0.05);

        Ok(())
    }

    #[tokio::test]
    async fn summarizes_analytics_distance_only_during_clipped_sessions(
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
            .expect("database configured");
        let imei = "999888777666560";

        sqlx::query("DELETE FROM device_locations WHERE imei = $1")
            .bind(imei)
            .execute(database.pool())
            .await?;
        sqlx::query("DELETE FROM devices WHERE imei = $1")
            .bind(imei)
            .execute(database.pool())
            .await?;

        let device_id: i64 = sqlx::query_scalar(
            r#"
            INSERT INTO devices (imei, first_seen_at, last_seen_at, created_at, updated_at)
            VALUES ($1, NOW(), NOW(), NOW(), NOW())
            RETURNING id
            "#,
        )
        .bind(imei)
        .fetch_one(database.pool())
        .await?;

        let outside_before = Utc.with_ymd_and_hms(2026, 5, 16, 1, 20, 0).unwrap();
        let inside_start = Utc.with_ymd_and_hms(2026, 5, 16, 1, 40, 0).unwrap();
        let inside_end = Utc.with_ymd_and_hms(2026, 5, 16, 1, 50, 0).unwrap();
        let outside_after = Utc.with_ymd_and_hms(2026, 5, 16, 2, 10, 0).unwrap();

        sqlx::query(
            r#"
            INSERT INTO device_locations (
                device_id, imei, server_received_at, gps_timestamp, protocol_number,
                packet_family, latitude, longitude, speed_kph, course, course_status,
                satellite_count, gps_info_length, extra_data_hex, peer_addr
            )
            VALUES
                ($1, $2, $3, $3::timestamp, 18, 'location', -6.000000, 106.000000, 0, 0, 0, 8, 12, '', '127.0.0.1:5000'),
                ($1, $2, $4, $4::timestamp, 18, 'location', -6.204066, 106.785514, 0, 0, 0, 8, 12, '', '127.0.0.1:5000'),
                ($1, $2, $5, $5::timestamp, 18, 'location', -6.204500, 106.786000, 0, 0, 0, 8, 12, '', '127.0.0.1:5000'),
                ($1, $2, $6, $6::timestamp, 18, 'location', -7.000000, 107.000000, 0, 0, 0, 8, 12, '', '127.0.0.1:5000')
            "#,
        )
        .bind(device_id)
        .bind(imei)
        .bind(outside_before)
        .bind(inside_start)
        .bind(inside_end)
        .bind(outside_after)
        .execute(database.pool())
        .await?;

        let range_start = Utc.with_ymd_and_hms(2026, 5, 16, 1, 30, 0).unwrap();
        let range_end = Utc.with_ymd_and_hms(2026, 5, 16, 2, 30, 0).unwrap();
        let sessions = vec![AnalyticsSession {
            id: 1,
            session_status: "finished".to_string(),
            created_at: Utc.with_ymd_and_hms(2026, 5, 16, 1, 0, 0).unwrap(),
            resolved_at: Some(Utc.with_ymd_and_hms(2026, 5, 16, 2, 0, 0).unwrap()),
        }];

        let ride_only_summary =
            fetch_analytics_ride_summary(database.pool(), imei, &sessions, range_start, range_end)
                .await?;
        let raw_range_summary = fetch_ride_summary(database.pool(), imei, range_start, range_end)
            .await?
            .expect("raw range summary");
        let expected_distance = haversine_distance_km(-6.204066, 106.785514, -6.204500, 106.786000);

        assert!((ride_only_summary.total_distance_km - expected_distance).abs() < 0.001);
        assert!(raw_range_summary.total_distance_km > ride_only_summary.total_distance_km * 10.0);
        assert!((ride_only_summary.average_speed_kph - (expected_distance * 2.0)).abs() < 0.001);

        Ok(())
    }

    #[tokio::test]
    async fn fetches_latest_live_tracking_start_from_location_history(
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
            .expect("database configured");
        let imei = "999888777666558";
        let location_received_at = Utc.with_ymd_and_hms(2026, 7, 28, 1, 43, 16).unwrap();
        let later_heartbeat_at = Utc.with_ymd_and_hms(2026, 7, 28, 2, 8, 17).unwrap();

        sqlx::query("DELETE FROM device_locations WHERE imei = $1")
            .bind(imei)
            .execute(database.pool())
            .await?;
        sqlx::query("DELETE FROM devices WHERE imei = $1")
            .bind(imei)
            .execute(database.pool())
            .await?;

        let device_id: i64 = sqlx::query_scalar(
            r#"
            INSERT INTO devices (imei, first_seen_at, last_seen_at, created_at, updated_at)
            VALUES ($1, $2, $3, $2, $3)
            RETURNING id
            "#,
        )
        .bind(imei)
        .bind(location_received_at)
        .bind(later_heartbeat_at)
        .fetch_one(database.pool())
        .await?;

        sqlx::query(
            r#"
            INSERT INTO device_locations (
                device_id, imei, server_received_at, gps_timestamp, protocol_number,
                packet_family, latitude, longitude, speed_kph, course, course_status,
                satellite_count, gps_info_length, extra_data_hex, peer_addr
            )
            VALUES ($1, $2, $3, $3::timestamp, 18, 'location', -6.204066, 106.785514, 0, 0, 0, 8, 12, '', '127.0.0.1:5000')
            "#,
        )
        .bind(device_id)
        .bind(imei)
        .bind(location_received_at)
        .execute(database.pool())
        .await?;

        let start_at = fetch_latest_location_received_at(database.pool(), imei).await?;

        assert_eq!(start_at, Some(location_received_at));
        assert_ne!(start_at, Some(later_heartbeat_at));
        Ok(())
    }

    #[tokio::test]
    async fn records_midtrans_payment_and_extends_subscription_idempotently(
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
            .expect("database configured");
        let telegram_user_id = 8_880_000_001_i64;
        let chat_id = 8_880_000_002_i64;

        sqlx::query(
            "DELETE FROM telegram_payment_events WHERE telegram_user_id = $1 OR chat_id = $2",
        )
        .bind(telegram_user_id)
        .bind(chat_id)
        .execute(database.pool())
        .await?;
        sqlx::query("DELETE FROM telegram_subscriptions WHERE telegram_user_id = $1")
            .bind(telegram_user_id)
            .execute(database.pool())
            .await?;
        sqlx::query("DELETE FROM telegram_users WHERE telegram_user_id = $1")
            .bind(telegram_user_id)
            .execute(database.pool())
            .await?;
        sqlx::query("DELETE FROM devices WHERE imei = '999888777666555'")
            .execute(database.pool())
            .await?;

        sqlx::query(
            r#"
            INSERT INTO devices (
                imei, first_seen_at, last_seen_at, latest_peer_addr, pricing_tier, created_at, updated_at
            )
            VALUES ('999888777666555', NOW(), NOW(), '127.0.0.1:5000', 'basic', NOW(), NOW())
            "#,
        )
        .execute(database.pool())
        .await?;

        sqlx::query(
            r#"
            INSERT INTO telegram_users (
                telegram_user_id, chat_id, bound_imei, registration_status, created_at, updated_at
            )
            VALUES ($1, $2, '999888777666555', 'bound', NOW(), NOW())
            "#,
        )
        .bind(telegram_user_id)
        .bind(chat_id)
        .execute(database.pool())
        .await?;

        let first_paid_at = Utc.with_ymd_and_hms(2026, 4, 25, 10, 0, 0).unwrap();
        let first_order_id = build_midtrans_order_id(telegram_user_id, first_paid_at);
        let device_id = fetch_device_id(database.pool(), "999888777666555").await?;
        create_pending_midtrans_payment(
            database.pool(),
            telegram_user_id,
            chat_id,
            device_id,
            "999888777666555",
            MIDTRANS_BASIC_PLAN_CODE,
            &first_order_id,
            2_000,
            first_paid_at + chrono::Duration::hours(24),
        )
        .await?;

        let first_notification = MidtransWebhookNotification {
            order_id: first_order_id.clone(),
            status_code: "200".to_string(),
            gross_amount: "2000.00".to_string(),
            signature_key: "test-signature".to_string(),
            transaction_status: "settlement".to_string(),
            transaction_id: Some("midtrans-transaction-first".to_string()),
            payment_type: Some("qris".to_string()),
            fraud_status: None,
        };

        let first_subscription = apply_midtrans_webhook(
            database.pool(),
            &first_notification,
            MidtransPaymentStatus::Paid,
            first_paid_at,
        )
        .await?;
        let MidtransWebhookApplyOutcome::Paid(first_subscription) = first_subscription else {
            panic!("paid webhook should activate subscription");
        };
        let first_subscription_id = first_subscription.id;
        let first_end = first_subscription
            .current_period_end_at
            .expect("first payment should set period end");
        assert_eq!(first_subscription.plan_code, MIDTRANS_BASIC_PLAN_CODE);
        assert_eq!(
            first_end.signed_duration_since(first_paid_at).num_days(),
            30
        );

        let duplicate_subscription = apply_midtrans_webhook(
            database.pool(),
            &first_notification,
            MidtransPaymentStatus::Paid,
            first_paid_at + chrono::Duration::minutes(5),
        )
        .await?;
        assert_eq!(duplicate_subscription, MidtransWebhookApplyOutcome::Ignored);

        sqlx::query(
            r#"
            INSERT INTO telegram_subscription_sanctions (
                subscription_id, telegram_user_id, chat_id,
                last_pre_expiry_reminded_for_period_end_at,
                last_pre_expiry_reminded_day, last_overdue_reminded_day,
                fine_amount_idr, withdrawal_required, created_at, updated_at
            )
            VALUES ($1, $2, $3, $4, 0, 3, 3000, FALSE, NOW(), NOW())
            "#,
        )
        .bind(first_subscription_id)
        .bind(telegram_user_id)
        .bind(chat_id)
        .bind(first_end)
        .execute(database.pool())
        .await?;

        let second_paid_at = first_paid_at + chrono::Duration::days(1);
        sqlx::query("UPDATE devices SET pricing_tier = 'ojol', updated_at = NOW() WHERE imei = '999888777666555'")
            .execute(database.pool())
            .await?;
        let second_order_id = build_midtrans_order_id(telegram_user_id, second_paid_at);
        create_pending_midtrans_payment(
            database.pool(),
            telegram_user_id,
            chat_id,
            device_id,
            "999888777666555",
            MIDTRANS_OJOL_PLAN_CODE,
            &second_order_id,
            2_000,
            second_paid_at + chrono::Duration::hours(24),
        )
        .await?;

        let second_notification = MidtransWebhookNotification {
            order_id: second_order_id,
            status_code: "200".to_string(),
            gross_amount: "2000.00".to_string(),
            signature_key: "test-signature".to_string(),
            transaction_status: "settlement".to_string(),
            transaction_id: Some("midtrans-transaction-second".to_string()),
            payment_type: Some("qris".to_string()),
            fraud_status: None,
        };

        let second_subscription = apply_midtrans_webhook(
            database.pool(),
            &second_notification,
            MidtransPaymentStatus::Paid,
            second_paid_at,
        )
        .await?;
        let MidtransWebhookApplyOutcome::Paid(second_subscription) = second_subscription else {
            panic!("second payment should extend subscription");
        };
        assert_eq!(second_subscription.id, first_subscription_id);
        assert_eq!(second_subscription.plan_code, MIDTRANS_OJOL_PLAN_CODE);
        let second_end = second_subscription
            .current_period_end_at
            .expect("second payment should set period end");

        assert_eq!(
            second_end.signed_duration_since(first_end).num_days(),
            30
        );
        assert!(second_end > first_end);

        let sanction_row = sqlx::query(
            r#"
            SELECT last_pre_expiry_reminded_for_period_end_at,
                   last_pre_expiry_reminded_day,
                   last_overdue_reminded_day,
                   fine_amount_idr,
                   withdrawal_required,
                   resolved_at
            FROM telegram_subscription_sanctions
            WHERE subscription_id = $1
            "#,
        )
        .bind(first_subscription_id)
        .fetch_one(database.pool())
        .await?;
        assert_eq!(
            sanction_row
                .get::<Option<DateTime<Utc>>, _>("last_pre_expiry_reminded_for_period_end_at"),
            None
        );
        assert_eq!(
            sanction_row.get::<Option<i32>, _>("last_pre_expiry_reminded_day"),
            None
        );
        assert_eq!(
            sanction_row.get::<Option<i32>, _>("last_overdue_reminded_day"),
            None
        );
        assert_eq!(sanction_row.get::<i64, _>("fine_amount_idr"), 0);
        assert!(!sanction_row.get::<bool, _>("withdrawal_required"));
        assert!(
            sanction_row
                .get::<Option<DateTime<Utc>>, _>("resolved_at")
                .is_some()
        );

        let subscription_rows: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM telegram_subscriptions WHERE telegram_user_id = $1",
        )
        .bind(telegram_user_id)
        .fetch_one(database.pool())
        .await?;
        assert_eq!(subscription_rows, 1);

        let current_plan_code: String = sqlx::query_scalar(
            "SELECT plan_code FROM telegram_subscriptions WHERE telegram_user_id = $1",
        )
        .bind(telegram_user_id)
        .fetch_one(database.pool())
        .await?;
        assert_eq!(current_plan_code, MIDTRANS_OJOL_PLAN_CODE);

        let current_period_start_at: DateTime<Utc> = sqlx::query_scalar(
            "SELECT current_period_start_at FROM telegram_subscriptions WHERE telegram_user_id = $1",
        )
        .bind(telegram_user_id)
        .fetch_one(database.pool())
        .await?;
        assert_eq!(current_period_start_at, first_paid_at);

        let expired_at = first_paid_at + chrono::Duration::hours(2);
        let expired_order_id = build_midtrans_order_id(telegram_user_id, expired_at);
        create_pending_midtrans_payment(
            database.pool(),
            telegram_user_id,
            chat_id,
            device_id,
            "999888777666555",
            MIDTRANS_BASIC_PLAN_CODE,
            &expired_order_id,
            2_000,
            expired_at + chrono::Duration::hours(24),
        )
        .await?;
        let expired_notification = MidtransWebhookNotification {
            order_id: expired_order_id,
            status_code: "407".to_string(),
            gross_amount: "2000.00".to_string(),
            signature_key: "test-signature".to_string(),
            transaction_status: "expire".to_string(),
            transaction_id: Some("midtrans-transaction-expired".to_string()),
            payment_type: Some("qris".to_string()),
            fraud_status: None,
        };
        let expired_subscription = apply_midtrans_webhook(
            database.pool(),
            &expired_notification,
            MidtransPaymentStatus::Expired,
            expired_at,
        )
        .await?;
        assert_eq!(expired_subscription, MidtransWebhookApplyOutcome::Ignored);

        let paid_events: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM telegram_payment_events WHERE telegram_user_id = $1 AND payment_status = 'paid'",
        )
        .bind(telegram_user_id)
        .fetch_one(database.pool())
        .await?;
        assert_eq!(paid_events, 2);

        Ok(())
    }

    #[tokio::test]
    async fn accepts_midtrans_paid_webhook_with_customer_fee(
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
            .expect("database configured");
        let telegram_user_id = 8_880_000_011_i64;
        let chat_id = 8_880_000_012_i64;

        sqlx::query(
            "DELETE FROM telegram_payment_events WHERE telegram_user_id = $1 OR chat_id = $2",
        )
        .bind(telegram_user_id)
        .bind(chat_id)
        .execute(database.pool())
        .await?;
        sqlx::query("DELETE FROM telegram_subscriptions WHERE telegram_user_id = $1")
            .bind(telegram_user_id)
            .execute(database.pool())
            .await?;
        sqlx::query("DELETE FROM telegram_users WHERE telegram_user_id = $1")
            .bind(telegram_user_id)
            .execute(database.pool())
            .await?;
        sqlx::query("DELETE FROM devices WHERE imei = '999888777666556'")
            .execute(database.pool())
            .await?;

        sqlx::query(
            r#"
            INSERT INTO devices (
                imei, first_seen_at, last_seen_at, latest_peer_addr, pricing_tier, created_at, updated_at
            )
            VALUES ('999888777666556', NOW(), NOW(), '127.0.0.1:5000', 'basic', NOW(), NOW())
            "#,
        )
        .execute(database.pool())
        .await?;

        sqlx::query(
            r#"
            INSERT INTO telegram_users (
                telegram_user_id, chat_id, bound_imei, registration_status, created_at, updated_at
            )
            VALUES ($1, $2, '999888777666556', 'bound', NOW(), NOW())
            "#,
        )
        .bind(telegram_user_id)
        .bind(chat_id)
        .execute(database.pool())
        .await?;

        let paid_at = Utc.with_ymd_and_hms(2026, 4, 25, 10, 0, 0).unwrap();
        let order_id = build_midtrans_order_id(telegram_user_id, paid_at);
        let device_id = fetch_device_id(database.pool(), "999888777666556").await?;
        create_pending_midtrans_payment(
            database.pool(),
            telegram_user_id,
            chat_id,
            device_id,
            "999888777666556",
            MIDTRANS_BASIC_PLAN_CODE,
            &order_id,
            2_000,
            paid_at + chrono::Duration::hours(24),
        )
        .await?;

        let notification = MidtransWebhookNotification {
            order_id,
            status_code: "200".to_string(),
            gross_amount: "2015.00".to_string(),
            signature_key: "test-signature".to_string(),
            transaction_status: "settlement".to_string(),
            transaction_id: Some("midtrans-transaction-with-fee".to_string()),
            payment_type: Some("qris".to_string()),
            fraud_status: None,
        };

        let outcome = apply_midtrans_webhook(
            database.pool(),
            &notification,
            MidtransPaymentStatus::Paid,
            paid_at,
        )
        .await?;
        assert!(matches!(outcome, MidtransWebhookApplyOutcome::Paid(_)));

        let payment_status: String = sqlx::query_scalar(
            "SELECT payment_status FROM telegram_payment_events WHERE provider_order_id = $1",
        )
        .bind(&notification.order_id)
        .fetch_one(database.pool())
        .await?;
        assert_eq!(payment_status, "paid");
        let stored_payment_device: (Option<i64>, Option<String>) = sqlx::query_as(
            "SELECT device_id, imei FROM telegram_payment_events WHERE provider_order_id = $1",
        )
        .bind(&notification.order_id)
        .fetch_one(database.pool())
        .await?;
        assert_eq!(stored_payment_device.0, Some(device_id));
        assert_eq!(stored_payment_device.1.as_deref(), Some("999888777666556"));

        Ok(())
    }
}
