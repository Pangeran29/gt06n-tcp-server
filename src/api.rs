use std::net::SocketAddr;
use std::sync::Arc;

use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use chrono::{DateTime, Utc};
use reqwest::Client;
use serde::{Deserialize, Serialize};
use sqlx::Row;
use thiserror::Error;
use tracing::warn;

use crate::bot::format_payment_success_message;
use crate::config::Config;
use crate::db::{Database, DatabaseError};
use crate::midtrans::{
    apply_midtrans_webhook, map_midtrans_status, verify_midtrans_signature,
    MidtransWebhookApplyOutcome, MidtransWebhookNotification,
};

#[derive(Debug, Error)]
pub enum ApiError {
    #[error("database is not configured")]
    MissingDatabase,
    #[error("failed to connect to database: {0}")]
    Database(#[from] DatabaseError),
    #[error("failed to bind http api listener: {0}")]
    Bind(#[from] std::io::Error),
    #[error("invalid start_at query parameter")]
    InvalidStartAt,
    #[error("invalid end_at query parameter")]
    InvalidEndAt,
    #[error("database query failed: {0}")]
    Query(#[from] sqlx::Error),
    #[error("telegram api request failed: {0}")]
    Http(#[from] reqwest::Error),
    #[error("midtrans server key is not configured")]
    MissingMidtransServerKey,
    #[error("invalid midtrans signature")]
    InvalidMidtransSignature,
    #[error("unsupported midtrans transaction status")]
    UnsupportedMidtransStatus,
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let status = match self {
            Self::InvalidStartAt
            | Self::InvalidEndAt
            | Self::InvalidMidtransSignature
            | Self::UnsupportedMidtransStatus => StatusCode::BAD_REQUEST,
            Self::MissingDatabase | Self::MissingMidtransServerKey => {
                StatusCode::INTERNAL_SERVER_ERROR
            }
            Self::Database(_) | Self::Query(_) | Self::Bind(_) | Self::Http(_) => {
                StatusCode::INTERNAL_SERVER_ERROR
            }
        };
        let body = Json(ApiErrorBody {
            error: self.to_string(),
        });

        (status, body).into_response()
    }
}

#[derive(Debug, Clone)]
pub struct HttpApiServer {
    bind_addr: SocketAddr,
    router: Router,
}

#[derive(Debug, Clone)]
struct AppState {
    pool: sqlx::PgPool,
    midtrans_server_key: Option<String>,
    telegram_bot_token: Option<String>,
    http_client: Client,
}

#[derive(Debug, Serialize)]
struct ApiErrorBody {
    error: String,
}

#[derive(Debug, Deserialize)]
struct LocationsQuery {
    start_at: String,
    end_at: Option<String>,
}

#[derive(Debug, Serialize)]
struct LocationHistoryResponse {
    imei: String,
    start_at: String,
    latest_server_received_at: Option<String>,
    points: Vec<LocationPoint>,
}

#[derive(Debug, Serialize)]
struct LocationPoint {
    server_received_at: String,
    gps_timestamp: String,
    latitude: f64,
    longitude: f64,
    speed_kph: i32,
    course: i32,
    satellite_count: i32,
}

impl HttpApiServer {
    pub async fn from_config(config: &Config) -> Result<Self, ApiError> {
        let database = Database::connect(config)
            .await?
            .ok_or(ApiError::MissingDatabase)?;
        let state = Arc::new(AppState {
            pool: database.pool().clone(),
            midtrans_server_key: config.midtrans_server_key.clone(),
            telegram_bot_token: config.telegram_bot_token.clone(),
            http_client: Client::new(),
        });

        let router = Router::new()
            .route("/api/devices/{imei}/locations", get(get_device_locations))
            .route("/api/midtrans/webhook", post(post_midtrans_webhook))
            .with_state(state);

        Ok(Self {
            bind_addr: config.http_api_bind_addr,
            router,
        })
    }

    pub fn bind_addr(&self) -> SocketAddr {
        self.bind_addr
    }

    pub async fn run(self) -> Result<(), ApiError> {
        let listener = tokio::net::TcpListener::bind(self.bind_addr).await?;
        axum::serve(listener, self.router)
            .await
            .map_err(ApiError::Bind)
    }
}

#[derive(Debug, Serialize)]
struct MidtransWebhookResponse {
    ok: bool,
    status: &'static str,
}

#[derive(Debug, Serialize)]
struct TelegramSendMessageRequest {
    chat_id: i64,
    text: String,
}

async fn get_device_locations(
    State(state): State<Arc<AppState>>,
    Path(imei): Path<String>,
    Query(query): Query<LocationsQuery>,
) -> Result<Json<LocationHistoryResponse>, ApiError> {
    let start_at = DateTime::parse_from_rfc3339(&query.start_at)
        .map_err(|_| ApiError::InvalidStartAt)?
        .with_timezone(&Utc);
    let end_at = query
        .end_at
        .as_deref()
        .map(|value| {
            DateTime::parse_from_rfc3339(value)
                .map_err(|_| ApiError::InvalidEndAt)
                .map(|value| value.with_timezone(&Utc))
        })
        .transpose()?;

    let rows = sqlx::query(
        r#"
        SELECT server_received_at, gps_timestamp, latitude, longitude, speed_kph, course, satellite_count
        FROM device_locations
        WHERE imei = $1
          AND server_received_at >= $2
          AND ($3::timestamptz IS NULL OR server_received_at <= $3)
        ORDER BY server_received_at ASC
        "#,
    )
    .bind(&imei)
    .bind(start_at)
    .bind(end_at)
    .fetch_all(&state.pool)
    .await?;

    let points: Vec<LocationPoint> = rows
        .into_iter()
        .map(|row| LocationPoint {
            server_received_at: row
                .get::<DateTime<Utc>, _>("server_received_at")
                .to_rfc3339(),
            gps_timestamp: row
                .get::<chrono::NaiveDateTime, _>("gps_timestamp")
                .format("%Y-%m-%dT%H:%M:%S")
                .to_string(),
            latitude: row.get("latitude"),
            longitude: row.get("longitude"),
            speed_kph: row.get("speed_kph"),
            course: row.get("course"),
            satellite_count: row.get("satellite_count"),
        })
        .collect();

    let latest_server_received_at = points
        .last()
        .map(|point: &LocationPoint| point.server_received_at.clone());

    Ok(Json(LocationHistoryResponse {
        imei,
        start_at: start_at.to_rfc3339(),
        latest_server_received_at,
        points,
    }))
}

async fn post_midtrans_webhook(
    State(state): State<Arc<AppState>>,
    Json(notification): Json<MidtransWebhookNotification>,
) -> Result<Json<MidtransWebhookResponse>, ApiError> {
    let server_key = state
        .midtrans_server_key
        .as_deref()
        .ok_or(ApiError::MissingMidtransServerKey)?;

    if !verify_midtrans_signature(
        server_key,
        &notification.order_id,
        &notification.status_code,
        &notification.gross_amount,
        &notification.signature_key,
    ) {
        return Err(ApiError::InvalidMidtransSignature);
    }

    let mapped_status = map_midtrans_status(&notification.transaction_status)
        .ok_or(ApiError::UnsupportedMidtransStatus)?;
    let outcome =
        apply_midtrans_webhook(&state.pool, &notification, mapped_status, Utc::now()).await?;

    let status = match outcome {
        MidtransWebhookApplyOutcome::Paid(subscription) => {
            if let Some(token) = state.telegram_bot_token.as_deref() {
                let text = format_payment_success_message(subscription.current_period_end_at);
                send_telegram_message(&state.http_client, token, subscription.chat_id, &text)
                    .await?;
            }
            "paid"
        }
        MidtransWebhookApplyOutcome::Ignored => "ignored",
        MidtransWebhookApplyOutcome::UnknownOrder => {
            warn!(
                order_id = %notification.order_id,
                "ignoring Midtrans webhook because order id is not known locally"
            );
            "unknown_order"
        }
    };

    Ok(Json(MidtransWebhookResponse { ok: true, status }))
}

async fn send_telegram_message(
    client: &Client,
    token: &str,
    chat_id: i64,
    text: &str,
) -> Result<(), reqwest::Error> {
    let request = TelegramSendMessageRequest {
        chat_id,
        text: text.to_string(),
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
