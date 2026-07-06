use chrono::{DateTime, FixedOffset, NaiveDate, Utc};
use reqwest::Client;
use sqlx::Row;
use thiserror::Error;
use tracing::warn;

use crate::config::Config;
use crate::db::{Database, DatabaseError};
use crate::midtrans::{SubscriptionPlan, MIDTRANS_BASIC_PLAN_CODE, MIDTRANS_OJOL_PLAN_CODE};
use crate::telegram_messages as messages;

const WIB_OFFSET_SECONDS: i32 = 7 * 60 * 60;
pub const SUBSCRIPTION_PRE_EXPIRY_REMINDER_DAYS: i64 = 5;
pub const SUBSCRIPTION_DAILY_FINE_IDR: i64 = 1_000;
pub const SUBSCRIPTION_MAX_FINE_DAYS: i64 = 7;
pub const SUBSCRIPTION_MAX_FINE_IDR: i64 = SUBSCRIPTION_DAILY_FINE_IDR * SUBSCRIPTION_MAX_FINE_DAYS;
pub const CUSTOMER_REFERENCED_DEVICE_FEE_IDR: i64 = 10_000;

#[derive(Debug, Error)]
pub enum SubscriptionMaintenanceError {
    #[error("database is not configured")]
    MissingDatabase,
    #[error("failed to connect to database: {0}")]
    Database(#[from] DatabaseError),
    #[error("database query failed: {0}")]
    Query(#[from] sqlx::Error),
    #[error("telegram bot token is not configured")]
    MissingTelegramBotToken,
    #[error("telegram api request failed: {0}")]
    Http(#[from] reqwest::Error),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SubscriptionPaymentQuote {
    pub base_amount_idr: i64,
    pub customer_reference_fee_idr: i64,
    pub shipment_fee_idr: i64,
    pub fine_amount_idr: i64,
    pub total_amount_idr: i64,
    pub overdue_days: i64,
    pub withdrawal_required: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SubscriptionMaintenanceAction {
    None,
    PreExpiryReminder,
    OverdueReminder {
        overdue_days: i64,
        fine_amount_idr: i64,
    },
    MarkWithdrawalRequired {
        overdue_days: i64,
        fine_amount_idr: i64,
    },
}

#[derive(Debug, Clone)]
struct SubscriptionMaintenanceRecord {
    subscription_id: i64,
    telegram_user_id: i64,
    chat_id: i64,
    current_period_end_at: DateTime<Utc>,
    last_pre_expiry_reminded_for_period_end_at: Option<DateTime<Utc>>,
    last_overdue_reminded_day: Option<i32>,
    withdrawal_required: bool,
}

#[derive(Debug, serde::Serialize)]
struct TelegramSendMessageRequest {
    chat_id: i64,
    text: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    reply_markup: Option<TelegramInlineKeyboardMarkup>,
}

#[derive(Debug, serde::Serialize)]
struct TelegramInlineKeyboardMarkup {
    inline_keyboard: Vec<Vec<TelegramInlineKeyboardButton>>,
}

#[derive(Debug, serde::Serialize)]
struct TelegramInlineKeyboardButton {
    text: String,
    callback_data: String,
}

pub async fn run_subscription_maintenance_from_config(
    config: &Config,
) -> Result<(), SubscriptionMaintenanceError> {
    let database = Database::connect(config)
        .await?
        .ok_or(SubscriptionMaintenanceError::MissingDatabase)?;
    let telegram_bot_token = config
        .telegram_bot_token
        .as_deref()
        .ok_or(SubscriptionMaintenanceError::MissingTelegramBotToken)?;
    run_subscription_maintenance(database.pool(), telegram_bot_token, Utc::now()).await
}

pub async fn run_subscription_maintenance(
    pool: &sqlx::PgPool,
    telegram_bot_token: &str,
    now: DateTime<Utc>,
) -> Result<(), SubscriptionMaintenanceError> {
    let client = Client::new();
    let records = fetch_subscription_maintenance_records(pool).await?;

    for record in records {
        let action = resolve_subscription_maintenance_action(
            record.current_period_end_at,
            now,
            record.last_pre_expiry_reminded_for_period_end_at,
            record.last_overdue_reminded_day,
            record.withdrawal_required,
        );

        match action {
            SubscriptionMaintenanceAction::None => {}
            SubscriptionMaintenanceAction::PreExpiryReminder => {
                send_telegram_message(
                    &client,
                    telegram_bot_token,
                    record.chat_id,
                    format_pre_expiry_subscription_reminder_message(),
                    Some(subscription_payment_keyboard()),
                )
                .await?;
                mark_pre_expiry_reminder_sent(pool, &record, record.current_period_end_at, now)
                    .await?;
            }
            SubscriptionMaintenanceAction::OverdueReminder {
                overdue_days,
                fine_amount_idr,
            } => {
                send_telegram_message(
                    &client,
                    telegram_bot_token,
                    record.chat_id,
                    &format_overdue_subscription_reminder_message(fine_amount_idr),
                    Some(subscription_payment_keyboard()),
                )
                .await?;
                mark_overdue_reminder_sent(pool, &record, overdue_days, fine_amount_idr, now)
                    .await?;
            }
            SubscriptionMaintenanceAction::MarkWithdrawalRequired {
                overdue_days,
                fine_amount_idr,
            } => {
                mark_withdrawal_required(pool, &record, overdue_days, fine_amount_idr, now).await?;
            }
        }
    }

    Ok(())
}

pub async fn build_subscription_payment_quote(
    pool: &sqlx::PgPool,
    telegram_user_id: i64,
    plan: SubscriptionPlan,
    now: DateTime<Utc>,
) -> Result<SubscriptionPaymentQuote, sqlx::Error> {
    let device_row = sqlx::query(
        r#"
        SELECT d.id,
               d.referenced_by_customer_id IS NOT NULL AS has_customer_referenced_device,
               d.shipment_fee_idr
        FROM telegram_users tu
        LEFT JOIN devices d
          ON d.imei = tu.bound_imei
        WHERE tu.telegram_user_id = $1
        LIMIT 1
        "#,
    )
    .bind(telegram_user_id)
    .fetch_optional(pool)
    .await?;

    let has_customer_referenced_device = device_row
        .as_ref()
        .map(|row| row.get::<bool, _>("has_customer_referenced_device"))
        .unwrap_or(false);
    let shipment_fee_idr = device_row
        .as_ref()
        .and_then(|row| row.get::<Option<i64>, _>("shipment_fee_idr"))
        .unwrap_or(0);
    let prior_paid_payment_exists = if let Some(row) = device_row.as_ref() {
        let device_id = row.get::<Option<i64>, _>("id");
        if let Some(device_id) = device_id {
            sqlx::query_scalar::<_, bool>(
                r#"
                SELECT EXISTS (
                    SELECT 1
                    FROM telegram_payment_events
                    WHERE device_id = $1
                      AND payment_status = 'paid'
                )
                "#,
            )
            .bind(device_id)
            .fetch_one(pool)
            .await?
        } else {
            false
        }
    } else {
        false
    };

    let current_period_end_at = sqlx::query(
        r#"
        SELECT current_period_end_at
        FROM telegram_subscriptions
        WHERE telegram_user_id = $1
        LIMIT 1
        "#,
    )
    .bind(telegram_user_id)
    .fetch_optional(pool)
    .await?
    .and_then(|row| row.get::<Option<DateTime<Utc>>, _>("current_period_end_at"));

    let overdue_days = current_period_end_at
        .map(|period_end| overdue_days_wib(period_end, now))
        .unwrap_or(0);
    let fine_amount_idr = fine_amount_for_overdue_days(overdue_days);
    let customer_reference_fee_idr = if has_customer_referenced_device {
        CUSTOMER_REFERENCED_DEVICE_FEE_IDR
    } else {
        0
    };
    let shipment_fee_idr = if prior_paid_payment_exists {
        0
    } else {
        shipment_fee_idr
    };

    Ok(SubscriptionPaymentQuote {
        base_amount_idr: plan.price_idr,
        customer_reference_fee_idr,
        shipment_fee_idr,
        fine_amount_idr,
        total_amount_idr: plan.price_idr
            + customer_reference_fee_idr
            + shipment_fee_idr
            + fine_amount_idr,
        overdue_days,
        withdrawal_required: overdue_days > SUBSCRIPTION_MAX_FINE_DAYS,
    })
}

pub fn resolve_subscription_maintenance_action(
    current_period_end_at: DateTime<Utc>,
    now: DateTime<Utc>,
    last_pre_expiry_reminded_for_period_end_at: Option<DateTime<Utc>>,
    last_overdue_reminded_day: Option<i32>,
    withdrawal_required: bool,
) -> SubscriptionMaintenanceAction {
    let days_until_expiry = days_until_expiry_wib(current_period_end_at, now);
    if days_until_expiry == SUBSCRIPTION_PRE_EXPIRY_REMINDER_DAYS
        && last_pre_expiry_reminded_for_period_end_at != Some(current_period_end_at)
    {
        return SubscriptionMaintenanceAction::PreExpiryReminder;
    }

    let overdue_days = overdue_days_wib(current_period_end_at, now);
    if overdue_days == 0 {
        return SubscriptionMaintenanceAction::None;
    }

    let fine_amount_idr = fine_amount_for_overdue_days(overdue_days);
    if overdue_days <= SUBSCRIPTION_MAX_FINE_DAYS {
        if last_overdue_reminded_day != Some(overdue_days as i32) {
            return SubscriptionMaintenanceAction::OverdueReminder {
                overdue_days,
                fine_amount_idr,
            };
        }
        return SubscriptionMaintenanceAction::None;
    }

    if !withdrawal_required {
        return SubscriptionMaintenanceAction::MarkWithdrawalRequired {
            overdue_days,
            fine_amount_idr,
        };
    }

    SubscriptionMaintenanceAction::None
}

pub fn format_pre_expiry_subscription_reminder_message() -> &'static str {
    messages::MSG_47_SUBSCRIPTION_PRE_EXPIRY_REMINDER
}

pub fn format_overdue_subscription_reminder_message(fine_amount_idr: i64) -> String {
    messages::msg_48_subscription_overdue_reminder(fine_amount_idr)
}

pub fn format_idr(amount: i64) -> String {
    messages::format_idr(amount)
}

pub fn fine_amount_for_overdue_days(overdue_days: i64) -> i64 {
    overdue_days.clamp(0, SUBSCRIPTION_MAX_FINE_DAYS) * SUBSCRIPTION_DAILY_FINE_IDR
}

fn days_until_expiry_wib(current_period_end_at: DateTime<Utc>, now: DateTime<Utc>) -> i64 {
    let current_date = wib_date(now);
    let expiry_date = wib_date(current_period_end_at);
    expiry_date.signed_duration_since(current_date).num_days()
}

fn overdue_days_wib(current_period_end_at: DateTime<Utc>, now: DateTime<Utc>) -> i64 {
    let current_date = wib_date(now);
    let expiry_date = wib_date(current_period_end_at);
    current_date
        .signed_duration_since(expiry_date)
        .num_days()
        .max(0)
}

fn wib_date(value: DateTime<Utc>) -> NaiveDate {
    let wib = FixedOffset::east_opt(WIB_OFFSET_SECONDS).expect("valid WIB offset");
    value.with_timezone(&wib).date_naive()
}

async fn fetch_subscription_maintenance_records(
    pool: &sqlx::PgPool,
) -> Result<Vec<SubscriptionMaintenanceRecord>, sqlx::Error> {
    let rows = sqlx::query(
        r#"
        SELECT ts.id AS subscription_id,
               ts.telegram_user_id,
               ts.chat_id,
               ts.current_period_end_at,
               tss.last_pre_expiry_reminded_for_period_end_at,
               tss.last_overdue_reminded_day,
               COALESCE(tss.withdrawal_required, FALSE) AS withdrawal_required
        FROM telegram_subscriptions ts
        LEFT JOIN telegram_subscription_sanctions tss
          ON tss.subscription_id = ts.id
        WHERE ts.plan_code IN ($1, $2)
          AND ts.current_period_end_at IS NOT NULL
          AND ts.status IN ('active', 'past_due')
        ORDER BY ts.current_period_end_at ASC, ts.id ASC
        "#,
    )
    .bind(MIDTRANS_BASIC_PLAN_CODE)
    .bind(MIDTRANS_OJOL_PLAN_CODE)
    .fetch_all(pool)
    .await?;

    Ok(rows
        .into_iter()
        .filter_map(|row| {
            Some(SubscriptionMaintenanceRecord {
                subscription_id: row.get("subscription_id"),
                telegram_user_id: row.get("telegram_user_id"),
                chat_id: row.get("chat_id"),
                current_period_end_at: row
                    .get::<Option<DateTime<Utc>>, _>("current_period_end_at")?,
                last_pre_expiry_reminded_for_period_end_at: row
                    .get("last_pre_expiry_reminded_for_period_end_at"),
                last_overdue_reminded_day: row.get("last_overdue_reminded_day"),
                withdrawal_required: row.get("withdrawal_required"),
            })
        })
        .collect())
}

async fn mark_pre_expiry_reminder_sent(
    pool: &sqlx::PgPool,
    record: &SubscriptionMaintenanceRecord,
    period_end_at: DateTime<Utc>,
    now: DateTime<Utc>,
) -> Result<(), sqlx::Error> {
    upsert_sanction_state(pool, record, Some(period_end_at), None, 0, false, None, now).await
}

async fn mark_overdue_reminder_sent(
    pool: &sqlx::PgPool,
    record: &SubscriptionMaintenanceRecord,
    overdue_days: i64,
    fine_amount_idr: i64,
    now: DateTime<Utc>,
) -> Result<(), sqlx::Error> {
    set_subscription_status(pool, record.subscription_id, "past_due").await?;
    upsert_sanction_state(
        pool,
        record,
        record.last_pre_expiry_reminded_for_period_end_at,
        Some(overdue_days as i32),
        fine_amount_idr,
        false,
        None,
        now,
    )
    .await
}

async fn mark_withdrawal_required(
    pool: &sqlx::PgPool,
    record: &SubscriptionMaintenanceRecord,
    overdue_days: i64,
    fine_amount_idr: i64,
    now: DateTime<Utc>,
) -> Result<(), sqlx::Error> {
    set_subscription_status(pool, record.subscription_id, "past_due").await?;
    upsert_sanction_state(
        pool,
        record,
        record.last_pre_expiry_reminded_for_period_end_at,
        Some(overdue_days as i32),
        fine_amount_idr,
        true,
        Some(now),
        now,
    )
    .await
}

async fn set_subscription_status(
    pool: &sqlx::PgPool,
    subscription_id: i64,
    status: &str,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        r#"
        UPDATE telegram_subscriptions
        SET status = $2,
            updated_at = NOW()
        WHERE id = $1
        "#,
    )
    .bind(subscription_id)
    .bind(status)
    .execute(pool)
    .await?;

    Ok(())
}

async fn upsert_sanction_state(
    pool: &sqlx::PgPool,
    record: &SubscriptionMaintenanceRecord,
    last_pre_expiry_reminded_for_period_end_at: Option<DateTime<Utc>>,
    last_overdue_reminded_day: Option<i32>,
    fine_amount_idr: i64,
    withdrawal_required: bool,
    withdrawal_required_at: Option<DateTime<Utc>>,
    now: DateTime<Utc>,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        r#"
        INSERT INTO telegram_subscription_sanctions (
            subscription_id, telegram_user_id, chat_id,
            last_pre_expiry_reminded_for_period_end_at, last_overdue_reminded_day,
            fine_amount_idr, withdrawal_required, withdrawal_required_at,
            resolved_at, created_at, updated_at
        )
        VALUES ($1, $2, $3, $4, $5, $6, $7, $8, NULL, NOW(), $9)
        ON CONFLICT (subscription_id) DO UPDATE
        SET telegram_user_id = EXCLUDED.telegram_user_id,
            chat_id = EXCLUDED.chat_id,
            last_pre_expiry_reminded_for_period_end_at = COALESCE(
                EXCLUDED.last_pre_expiry_reminded_for_period_end_at,
                telegram_subscription_sanctions.last_pre_expiry_reminded_for_period_end_at
            ),
            last_overdue_reminded_day = COALESCE(
                EXCLUDED.last_overdue_reminded_day,
                telegram_subscription_sanctions.last_overdue_reminded_day
            ),
            fine_amount_idr = EXCLUDED.fine_amount_idr,
            withdrawal_required = EXCLUDED.withdrawal_required,
            withdrawal_required_at = COALESCE(
                EXCLUDED.withdrawal_required_at,
                telegram_subscription_sanctions.withdrawal_required_at
            ),
            resolved_at = NULL,
            updated_at = EXCLUDED.updated_at
        "#,
    )
    .bind(record.subscription_id)
    .bind(record.telegram_user_id)
    .bind(record.chat_id)
    .bind(last_pre_expiry_reminded_for_period_end_at)
    .bind(last_overdue_reminded_day)
    .bind(fine_amount_idr)
    .bind(withdrawal_required)
    .bind(withdrawal_required_at)
    .bind(now)
    .execute(pool)
    .await?;

    Ok(())
}

async fn send_telegram_message(
    client: &Client,
    token: &str,
    chat_id: i64,
    text: &str,
    reply_markup: Option<TelegramInlineKeyboardMarkup>,
) -> Result<(), reqwest::Error> {
    let request = TelegramSendMessageRequest {
        chat_id,
        text: text.to_string(),
        reply_markup,
    };

    let response = client
        .post(format!("https://api.telegram.org/bot{token}/sendMessage"))
        .json(&request)
        .send()
        .await?
        .error_for_status()?;
    let _ = response.bytes().await?;
    Ok(())
}

fn subscription_payment_keyboard() -> TelegramInlineKeyboardMarkup {
    TelegramInlineKeyboardMarkup {
        inline_keyboard: vec![vec![TelegramInlineKeyboardButton {
            text: messages::BTN_15_SUBSCRIBE.to_string(),
            callback_data: messages::CALLBACK_1_PAYMENT_SUBSCRIBE.to_string(),
        }]],
    }
}

pub async fn resolve_subscription_sanction(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    subscription_id: i64,
) -> Result<(), sqlx::Error> {
    if let Err(error) = sqlx::query(
        r#"
        UPDATE telegram_subscription_sanctions
        SET fine_amount_idr = 0,
            withdrawal_required = FALSE,
            withdrawal_required_at = NULL,
            resolved_at = NOW(),
            updated_at = NOW()
        WHERE subscription_id = $1
        "#,
    )
    .bind(subscription_id)
    .execute(&mut **tx)
    .await
    {
        warn!(
            error = %error,
            subscription_id,
            "failed to resolve subscription sanction state"
        );
        return Err(error);
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use chrono::{TimeZone, Utc};

    use super::*;

    #[test]
    fn resolves_pre_expiry_reminder_once_at_d_minus_five() {
        let period_end = Utc.with_ymd_and_hms(2026, 6, 29, 17, 0, 0).unwrap();
        let now = Utc.with_ymd_and_hms(2026, 6, 25, 1, 0, 0).unwrap();

        assert_eq!(
            resolve_subscription_maintenance_action(period_end, now, None, None, false),
            SubscriptionMaintenanceAction::PreExpiryReminder
        );
        assert_eq!(
            resolve_subscription_maintenance_action(period_end, now, Some(period_end), None, false),
            SubscriptionMaintenanceAction::None
        );
    }

    #[test]
    fn resolves_overdue_reminders_for_days_one_to_seven_once_per_day() {
        let period_end = Utc.with_ymd_and_hms(2026, 6, 29, 17, 0, 0).unwrap();
        let now = Utc.with_ymd_and_hms(2026, 7, 3, 1, 0, 0).unwrap();

        assert_eq!(
            resolve_subscription_maintenance_action(period_end, now, None, None, false),
            SubscriptionMaintenanceAction::OverdueReminder {
                overdue_days: 3,
                fine_amount_idr: 3_000,
            }
        );
        assert_eq!(
            resolve_subscription_maintenance_action(period_end, now, None, Some(3), false),
            SubscriptionMaintenanceAction::None
        );
    }

    #[test]
    fn caps_fine_and_marks_withdrawal_after_day_seven() {
        let period_end = Utc.with_ymd_and_hms(2026, 6, 29, 17, 0, 0).unwrap();
        let now = Utc.with_ymd_and_hms(2026, 7, 8, 1, 0, 0).unwrap();

        assert_eq!(fine_amount_for_overdue_days(9), SUBSCRIPTION_MAX_FINE_IDR);
        assert_eq!(
            resolve_subscription_maintenance_action(period_end, now, None, Some(7), false),
            SubscriptionMaintenanceAction::MarkWithdrawalRequired {
                overdue_days: 8,
                fine_amount_idr: SUBSCRIPTION_MAX_FINE_IDR,
            }
        );
        assert_eq!(
            resolve_subscription_maintenance_action(period_end, now, None, Some(7), true),
            SubscriptionMaintenanceAction::None
        );
    }

    #[test]
    fn formats_subscription_reminders() {
        assert!(format_pre_expiry_subscription_reminder_message().contains("Rp 1.000 per hari"));
        assert!(format_overdue_subscription_reminder_message(3_000).contains("Denda saat ini: Rp 3.000"));
    }

    #[test]
    fn subscription_reminder_keyboard_uses_subscribe_callback() {
        let keyboard = subscription_payment_keyboard();
        assert_eq!(keyboard.inline_keyboard.len(), 1);
        assert_eq!(keyboard.inline_keyboard[0].len(), 1);
        assert_eq!(
            keyboard.inline_keyboard[0][0].text,
            messages::BTN_15_SUBSCRIBE
        );
        assert_eq!(
            keyboard.inline_keyboard[0][0].callback_data,
            messages::CALLBACK_1_PAYMENT_SUBSCRIBE
        );
    }
}
