use base64::engine::general_purpose::STANDARD;
use base64::Engine;
use chrono::{DateTime, FixedOffset, TimeZone, Utc};
use reqwest::Client;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha512};
use sqlx::Row;
use thiserror::Error;

use crate::config::Config;

pub const MIDTRANS_PLAN_CODE: &str = "monthly_stars";
pub const MIDTRANS_PAYMENT_PROVIDER: &str = "midtrans";
pub const MIDTRANS_PAYMENT_KIND: &str = "snap_subscription";
pub const MIDTRANS_CURRENCY: &str = "IDR";
pub const MIDTRANS_PERIOD_DAYS: i32 = 30;

#[derive(Debug, Error)]
pub enum MidtransError {
    #[error("midtrans server key is not configured")]
    MissingServerKey,
    #[error("midtrans request failed: {0}")]
    Http(#[from] reqwest::Error),
    #[error("midtrans json parsing failed: {0}")]
    Json(#[from] serde_json::Error),
    #[error("database query failed: {0}")]
    Query(#[from] sqlx::Error),
    #[error("midtrans snap response did not include a payment URL: {0}")]
    MissingPaymentUrl(String),
}

#[derive(Debug, Clone)]
pub struct MidtransClient {
    client: Client,
    server_key: String,
    snap_base_url: String,
    price_idr: i64,
    expiry_hours: i64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MidtransCreatedPayment {
    pub order_id: String,
    pub transaction_id: Option<String>,
    pub payment_type: Option<String>,
    pub payment_url: String,
    pub raw_response: String,
    pub expires_at: DateTime<Utc>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MidtransSubscriptionRecord {
    pub id: i64,
    pub telegram_user_id: i64,
    pub chat_id: i64,
    pub current_period_end_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MidtransWebhookApplyOutcome {
    Paid(MidtransSubscriptionRecord),
    Ignored,
    UnknownOrder,
}

#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct MidtransWebhookNotification {
    pub order_id: String,
    pub status_code: String,
    pub gross_amount: String,
    pub signature_key: String,
    pub transaction_status: String,
    #[serde(default)]
    pub transaction_id: Option<String>,
    #[serde(default)]
    pub payment_type: Option<String>,
    #[serde(default)]
    pub fraud_status: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MidtransPaymentStatus {
    Pending,
    Paid,
    Expired,
    Failed,
    Refunded,
}

#[derive(Debug, Serialize)]
struct MidtransSnapRequest {
    transaction_details: MidtransTransactionDetails,
    item_details: Vec<MidtransItemDetail>,
    expiry: MidtransSnapExpiry,
}

#[derive(Debug, Serialize)]
struct MidtransTransactionDetails {
    order_id: String,
    gross_amount: i64,
}

#[derive(Debug, Serialize)]
struct MidtransItemDetail {
    id: String,
    price: i64,
    quantity: i32,
    name: String,
}

#[derive(Debug, Serialize)]
struct MidtransSnapExpiry {
    start_time: String,
    duration: i64,
    unit: String,
}

#[derive(Debug, Deserialize, Serialize)]
struct MidtransSnapResponse {
    token: String,
    redirect_url: String,
}

impl MidtransClient {
    pub fn from_config(config: &Config) -> Option<Self> {
        let server_key = config.midtrans_server_key.clone()?;
        Some(Self {
            client: Client::new(),
            server_key,
            snap_base_url: if config.midtrans_is_production {
                "https://app.midtrans.com".to_string()
            } else {
                "https://app.sandbox.midtrans.com".to_string()
            },
            price_idr: config.midtrans_plan_price_idr,
            expiry_hours: config.midtrans_payment_expiry_hours,
        })
    }

    pub fn price_idr(&self) -> i64 {
        self.price_idr
    }

    pub fn expiry_hours(&self) -> i64 {
        self.expiry_hours
    }

    pub async fn create_snap_transaction(
        &self,
        order_id: &str,
        created_at: DateTime<Utc>,
    ) -> Result<MidtransCreatedPayment, MidtransError> {
        let request = MidtransSnapRequest {
            transaction_details: MidtransTransactionDetails {
                order_id: order_id.to_string(),
                gross_amount: self.price_idr,
            },
            item_details: vec![MidtransItemDetail {
                id: MIDTRANS_PLAN_CODE.to_string(),
                price: self.price_idr,
                quantity: 1,
                name: "Heartbeats 30 Days Access".to_string(),
            }],
            expiry: MidtransSnapExpiry {
                start_time: created_at.format("%Y-%m-%d %H:%M:%S %z").to_string(),
                duration: self.expiry_hours,
                unit: "hour".to_string(),
            },
        };

        let response = self
            .client
            .post(format!("{}/snap/v1/transactions", self.snap_base_url))
            .header("Accept", "application/json")
            .header("Content-Type", "application/json")
            .header(
                "Authorization",
                format!("Basic {}", STANDARD.encode(format!("{}:", self.server_key))),
            )
            .json(&request)
            .send()
            .await?
            .error_for_status()?;

        let body: serde_json::Value = response.json().await?;
        let raw_response = serde_json::to_string(&body)?;
        let snap: MidtransSnapResponse = serde_json::from_value(body.clone())?;
        if snap.redirect_url.trim().is_empty() {
            return Err(MidtransError::MissingPaymentUrl(raw_response));
        }

        Ok(MidtransCreatedPayment {
            order_id: order_id.to_string(),
            transaction_id: Some(snap.token),
            payment_type: Some("snap".to_string()),
            payment_url: snap.redirect_url,
            raw_response,
            expires_at: created_at + chrono::Duration::hours(self.expiry_hours),
        })
    }
}

pub fn build_midtrans_order_id(telegram_user_id: i64, created_at: DateTime<Utc>) -> String {
    format!("hb-{telegram_user_id}-{}", created_at.timestamp_millis())
}

pub fn verify_midtrans_signature(
    server_key: &str,
    order_id: &str,
    status_code: &str,
    gross_amount: &str,
    signature_key: &str,
) -> bool {
    let mut hasher = Sha512::new();
    hasher.update(format!("{order_id}{status_code}{gross_amount}{server_key}"));
    let expected = format!("{:x}", hasher.finalize());
    expected.eq_ignore_ascii_case(signature_key)
}

pub fn map_midtrans_status(transaction_status: &str) -> Option<MidtransPaymentStatus> {
    Some(match transaction_status {
        "settlement" | "capture" => MidtransPaymentStatus::Paid,
        "pending" => MidtransPaymentStatus::Pending,
        "expire" => MidtransPaymentStatus::Expired,
        "deny" | "cancel" | "failure" => MidtransPaymentStatus::Failed,
        "refund" | "partial_refund" => MidtransPaymentStatus::Refunded,
        _ => return None,
    })
}

pub fn format_midtrans_payment_message(payment_url: &str, expires_at: DateTime<Utc>) -> String {
    let wib = FixedOffset::east_opt(7 * 60 * 60).expect("valid WIB offset");
    let expires_at = wib.from_utc_datetime(&expires_at.naive_utc());
    let escaped_payment_url = payment_url
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;");
    format!(
        "🔒 Heartbeats Monthly Access\nRp 50.000 — 30 Days\n\nTo activate your subscription, complete your payment using the link below:\n<tg-spoiler>{escaped_payment_url}</tg-spoiler>\n\n⏳ Payment link expires: {}",
        expires_at.format("%d %b %Y %H:%M WIB")
    )
}

pub async fn create_pending_midtrans_payment(
    pool: &sqlx::PgPool,
    telegram_user_id: i64,
    chat_id: i64,
    order_id: &str,
    gross_amount_idr: i64,
    expires_at: DateTime<Utc>,
) -> Result<i64, sqlx::Error> {
    let row = sqlx::query(
        r#"
        INSERT INTO telegram_payment_events (
            telegram_user_id, chat_id, subscription_id, payment_provider, payment_kind,
            payment_status, plan_code, currency, gross_amount_idr, period_days,
            provider_order_id, expires_at, created_at, updated_at
        )
        VALUES ($1, $2, NULL, $3, $4, 'pending', $5, $6, $7, $8, $9, $10, NOW(), NOW())
        RETURNING id
        "#,
    )
    .bind(telegram_user_id)
    .bind(chat_id)
    .bind(MIDTRANS_PAYMENT_PROVIDER)
    .bind(MIDTRANS_PAYMENT_KIND)
    .bind(MIDTRANS_PLAN_CODE)
    .bind(MIDTRANS_CURRENCY)
    .bind(gross_amount_idr)
    .bind(MIDTRANS_PERIOD_DAYS)
    .bind(order_id)
    .bind(expires_at)
    .fetch_one(pool)
    .await?;

    Ok(row.get("id"))
}

pub async fn mark_midtrans_payment_created(
    pool: &sqlx::PgPool,
    order_id: &str,
    created: &MidtransCreatedPayment,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        r#"
        UPDATE telegram_payment_events
        SET provider_transaction_id = $2,
            payment_type = $3,
            payment_url = $4,
            raw_create_response = $5::jsonb,
            updated_at = NOW()
        WHERE provider_order_id = $1
        "#,
    )
    .bind(order_id)
    .bind(&created.transaction_id)
    .bind(&created.payment_type)
    .bind(&created.payment_url)
    .bind(&created.raw_response)
    .execute(pool)
    .await?;

    Ok(())
}

pub async fn apply_midtrans_webhook(
    pool: &sqlx::PgPool,
    notification: &MidtransWebhookNotification,
    mapped_status: MidtransPaymentStatus,
    received_at: DateTime<Utc>,
) -> Result<MidtransWebhookApplyOutcome, sqlx::Error> {
    let raw_notification =
        serde_json::to_string(notification).map_err(|_| sqlx::Error::RowNotFound)?;
    let mut tx = pool.begin().await?;
    let payment_row = sqlx::query(
        r#"
        SELECT id, telegram_user_id, chat_id, subscription_id, payment_status,
               gross_amount_idr, period_days
        FROM telegram_payment_events
        WHERE provider_order_id = $1
        FOR UPDATE
        "#,
    )
    .bind(&notification.order_id)
    .fetch_optional(&mut *tx)
    .await?;
    let Some(payment_row) = payment_row else {
        tx.commit().await?;
        return Ok(MidtransWebhookApplyOutcome::UnknownOrder);
    };

    let payment_id: i64 = payment_row.get("id");
    let telegram_user_id: i64 = payment_row.get("telegram_user_id");
    let chat_id: i64 = payment_row.get("chat_id");
    let current_status: String = payment_row.get("payment_status");
    let gross_amount_idr: i64 = payment_row.get("gross_amount_idr");
    let period_days: i32 = payment_row.get("period_days");

    if current_status == "paid" {
        tx.commit().await?;
        return Ok(MidtransWebhookApplyOutcome::Ignored);
    }

    if current_status != "pending" {
        sqlx::query(
            r#"
            UPDATE telegram_payment_events
            SET provider_transaction_id = COALESCE($2, provider_transaction_id),
                payment_type = COALESCE($3, payment_type),
                raw_webhook_notification = $4::jsonb,
                updated_at = NOW()
            WHERE id = $1
            "#,
        )
        .bind(payment_id)
        .bind(&notification.transaction_id)
        .bind(&notification.payment_type)
        .bind(raw_notification)
        .execute(&mut *tx)
        .await?;

        tx.commit().await?;
        return Ok(MidtransWebhookApplyOutcome::Ignored);
    }

    let new_status = match mapped_status {
        MidtransPaymentStatus::Pending => "pending",
        MidtransPaymentStatus::Paid => "paid",
        MidtransPaymentStatus::Expired => "expired",
        MidtransPaymentStatus::Failed => "failed",
        MidtransPaymentStatus::Refunded => "refunded",
    };

    if mapped_status != MidtransPaymentStatus::Paid {
        sqlx::query(
            r#"
            UPDATE telegram_payment_events
            SET payment_status = $2,
                provider_transaction_id = COALESCE($3, provider_transaction_id),
                payment_type = COALESCE($4, payment_type),
                raw_webhook_notification = $5::jsonb,
                updated_at = NOW()
            WHERE id = $1
            "#,
        )
        .bind(payment_id)
        .bind(new_status)
        .bind(&notification.transaction_id)
        .bind(&notification.payment_type)
        .bind(raw_notification)
        .execute(&mut *tx)
        .await?;

        tx.commit().await?;
        return Ok(MidtransWebhookApplyOutcome::Ignored);
    }

    let notified_gross_amount = notification
        .gross_amount
        .parse::<f64>()
        .map(|value| value.round() as i64)
        .map_err(|_| sqlx::Error::RowNotFound)?;
    if notified_gross_amount < gross_amount_idr {
        tx.commit().await?;
        return Ok(MidtransWebhookApplyOutcome::Ignored);
    }

    let existing_subscription = sqlx::query(
        r#"
        SELECT id, current_period_end_at
        FROM telegram_subscriptions
        WHERE telegram_user_id = $1
          AND plan_code = $2
        FOR UPDATE
        "#,
    )
    .bind(telegram_user_id)
    .bind(MIDTRANS_PLAN_CODE)
    .fetch_optional(&mut *tx)
    .await?;

    let base_time = existing_subscription
        .as_ref()
        .and_then(|row| row.get::<Option<DateTime<Utc>>, _>("current_period_end_at"))
        .filter(|value| *value > received_at)
        .unwrap_or(received_at);
    let period_end = base_time + chrono::Duration::days(i64::from(period_days));

    let subscription_row = sqlx::query(
        r#"
        INSERT INTO telegram_subscriptions (
            telegram_user_id, chat_id, plan_code, status,
            current_period_start_at, current_period_end_at, created_at, updated_at
        )
        VALUES ($1, $2, $3, 'active', $4, $5, NOW(), NOW())
        ON CONFLICT (telegram_user_id, plan_code) DO UPDATE
        SET chat_id = EXCLUDED.chat_id,
            status = EXCLUDED.status,
            current_period_start_at = EXCLUDED.current_period_start_at,
            current_period_end_at = EXCLUDED.current_period_end_at,
            updated_at = EXCLUDED.updated_at
        RETURNING id, telegram_user_id, chat_id, current_period_end_at
        "#,
    )
    .bind(telegram_user_id)
    .bind(chat_id)
    .bind(MIDTRANS_PLAN_CODE)
    .bind(base_time)
    .bind(period_end)
    .fetch_one(&mut *tx)
    .await?;

    let subscription = MidtransSubscriptionRecord {
        id: subscription_row.get("id"),
        telegram_user_id: subscription_row.get("telegram_user_id"),
        chat_id: subscription_row.get("chat_id"),
        current_period_end_at: subscription_row.get("current_period_end_at"),
    };

    sqlx::query(
        r#"
        UPDATE telegram_payment_events
        SET subscription_id = $2,
            payment_status = 'paid',
            provider_transaction_id = COALESCE($3, provider_transaction_id),
            payment_type = COALESCE($4, payment_type),
            paid_at = $5,
            raw_webhook_notification = $6::jsonb,
            updated_at = NOW()
        WHERE id = $1
        "#,
    )
    .bind(payment_id)
    .bind(subscription.id)
    .bind(&notification.transaction_id)
    .bind(&notification.payment_type)
    .bind(received_at)
    .bind(raw_notification)
    .execute(&mut *tx)
    .await?;

    tx.commit().await?;
    Ok(MidtransWebhookApplyOutcome::Paid(subscription))
}

#[cfg(test)]
mod tests {
    use chrono::{TimeZone, Utc};

    use super::*;

    #[test]
    fn verifies_midtrans_signature() {
        let server_key = "server-key";
        let order_id = "hb-12345-1777111200000";
        let status_code = "200";
        let gross_amount = "2000.00";
        let mut hasher = Sha512::new();
        hasher.update(format!("{order_id}{status_code}{gross_amount}{server_key}"));
        let signature = format!("{:x}", hasher.finalize());

        assert!(verify_midtrans_signature(
            server_key,
            order_id,
            status_code,
            gross_amount,
            &signature
        ));
        assert!(!verify_midtrans_signature(
            server_key,
            order_id,
            status_code,
            gross_amount,
            "invalid"
        ));
    }

    #[test]
    fn maps_midtrans_statuses() {
        assert_eq!(
            map_midtrans_status("settlement"),
            Some(MidtransPaymentStatus::Paid)
        );
        assert_eq!(
            map_midtrans_status("capture"),
            Some(MidtransPaymentStatus::Paid)
        );
        assert_eq!(
            map_midtrans_status("pending"),
            Some(MidtransPaymentStatus::Pending)
        );
        assert_eq!(
            map_midtrans_status("expire"),
            Some(MidtransPaymentStatus::Expired)
        );
        assert_eq!(
            map_midtrans_status("deny"),
            Some(MidtransPaymentStatus::Failed)
        );
        assert_eq!(
            map_midtrans_status("refund"),
            Some(MidtransPaymentStatus::Refunded)
        );
        assert_eq!(map_midtrans_status("unknown"), None);
    }

    #[test]
    fn builds_midtrans_order_id_with_telegram_user_id() {
        let created_at = Utc.with_ymd_and_hms(2026, 4, 25, 10, 0, 0).unwrap();
        let order_id = build_midtrans_order_id(1_104_647_539, created_at);

        assert!(order_id.starts_with("hb-1104647539-"));
        assert_eq!(order_id, build_midtrans_order_id(1_104_647_539, created_at));
        assert_ne!(
            order_id,
            build_midtrans_order_id(
                1_104_647_539,
                created_at + chrono::Duration::milliseconds(1)
            )
        );
    }
}
