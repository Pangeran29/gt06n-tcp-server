use std::net::SocketAddr;
use std::sync::Arc;

use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, patch, post};
use axum::{Json, Router};
use chrono::{DateTime, Duration, FixedOffset, NaiveDate, TimeZone, Utc};
use reqwest::{multipart, Client};
use serde::{Deserialize, Serialize};
use sqlx::Row;
use thiserror::Error;
use tracing::warn;

use crate::bot::fetch_ride_summary;
use crate::config::Config;
use crate::db::{Database, DatabaseError};
use crate::midtrans::{
    apply_midtrans_webhook, map_midtrans_status, verify_midtrans_signature,
    MidtransWebhookApplyOutcome, MidtransWebhookNotification,
};
use crate::subscription_maintenance::subscription_lifecycle_state;
use crate::telegram_messages::{self, msg_46_payment_success};

const PAYMENT_SUCCESS_STICKER_BYTES: &[u8] =
    include_bytes!("../asset/AnimatedSticker - payment success.tgs");
const WIB_OFFSET_SECONDS: i32 = 7 * 60 * 60;

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
    #[error("invalid date query parameter; expected YYYY-MM-DD")]
    InvalidDate,
    #[error("invalid sort_order query parameter; expected asc or desc")]
    InvalidSortOrder,
    #[error("invalid sim_card_expiration_date; expected YYYY-MM-DD or null")]
    InvalidSimCardExpirationDate,
    #[error("device not found")]
    DeviceNotFound,
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
            | Self::InvalidDate
            | Self::InvalidSortOrder
            | Self::InvalidSimCardExpirationDate
            | Self::InvalidMidtransSignature
            | Self::UnsupportedMidtransStatus => StatusCode::BAD_REQUEST,
            Self::DeviceNotFound => StatusCode::NOT_FOUND,
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

#[derive(Debug, Deserialize)]
struct SessionsQuery {
    date: String,
}

#[derive(Debug, Deserialize)]
struct SubscriptionsQuery {
    sort_order: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SortOrder {
    Asc,
    Desc,
}

impl SortOrder {
    fn parse(value: Option<&str>) -> Result<Self, ApiError> {
        match value.unwrap_or("asc").to_ascii_lowercase().as_str() {
            "asc" => Ok(Self::Asc),
            "desc" => Ok(Self::Desc),
            _ => Err(ApiError::InvalidSortOrder),
        }
    }

    fn as_str(self) -> &'static str {
        match self {
            Self::Asc => "asc",
            Self::Desc => "desc",
        }
    }
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

#[derive(Debug, Serialize)]
struct DeviceSessionsResponse {
    imei: String,
    date: String,
    timezone: &'static str,
    sessions: Vec<DeviceSessionResponse>,
}

#[derive(Debug, Serialize)]
struct DeviceDailySummaryResponse {
    imei: String,
    date: String,
    timezone: &'static str,
    generated_at: String,
    customer_name: Option<String>,
    total_distance_km: f64,
    riding_seconds: u64,
    average_speed_kph: f64,
}

#[derive(Debug, Serialize)]
struct DeviceActivityResponse {
    imei: String,
    last_seen_at: String,
    latest_engine_status: Option<String>,
    latest_voltage_level: Option<i32>,
    battery_reported_at: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
struct ServiceRecommendationResponse {
    code: &'static str,
    interval_km: i64,
    title: &'static str,
    items: Vec<&'static str>,
}

#[derive(Debug, Serialize)]
struct ServiceMilestoneResponse {
    milestone_number: i64,
    milestone_km: i64,
    achieved_on: String,
    recommendation_code: &'static str,
    recommendation_label: String,
}

#[derive(Debug, Serialize)]
struct DeviceServiceResponse {
    imei: String,
    timezone: &'static str,
    generated_at: String,
    total_tracked_distance_km: f64,
    next_milestone_km: i64,
    distance_remaining_km: f64,
    next_recommendation: ServiceRecommendationResponse,
    milestones: Vec<ServiceMilestoneResponse>,
}

#[derive(Debug, Serialize)]
struct DeviceSessionResponse {
    id: i64,
    started_at: String,
    ended_at: Option<String>,
    state: &'static str,
    duration_seconds: i64,
    distance_km: f64,
}

#[derive(Debug, Serialize)]
struct SubscriptionResponse {
    telegram_user_id: i64,
    chat_id: i64,
    bound_imei: Option<String>,
    customer_name: Option<String>,
    customer_phone_number: Option<String>,
    pricing_tier: String,
    plan_code: String,
    status: String,
    current_period_start_at: Option<String>,
    current_period_end_at: Option<String>,
    first_subscribed_at: Option<String>,
    subscribed_days: Option<i64>,
    lifecycle_stage: &'static str,
    lifecycle_day: Option<i64>,
    reminder_sent: Option<bool>,
    fine_amount_idr: i64,
    withdrawal_required: bool,
}

#[derive(Debug, Serialize)]
struct DeviceSimCardResponse {
    id: i64,
    imei: String,
    sim_card: Option<String>,
    sim_card_expiration_date: Option<String>,
}

#[derive(Debug, Serialize)]
struct DeviceMonitoringResponse {
    id: i64,
    imei: String,
    customer_name: Option<String>,
    customer_phone_number: Option<String>,
    latest_latitude: Option<f64>,
    latest_longitude: Option<f64>,
    last_seen_at: String,
}

#[derive(Debug, Deserialize)]
struct UpdateSimCardExpirationRequest {
    sim_card_expiration_date: Option<String>,
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
            .route("/api/devices", get(get_devices))
            .route("/api/devices/{imei}/activity", get(get_device_activity))
            .route(
                "/api/devices/{imei}/daily-summary",
                get(get_device_daily_summary),
            )
            .route("/api/devices/{imei}/service", get(get_device_service))
            .route("/api/devices/{imei}/sessions", get(get_device_sessions))
            .route(
                "/api/devices/{imei}/locations/latest",
                get(get_device_latest_location),
            )
            .route("/api/devices/{imei}/locations", get(get_device_locations))
            .route("/api/devices/sim-cards", get(get_device_sim_cards))
            .route(
                "/api/devices/{imei}/sim-card-expiration",
                patch(update_device_sim_card_expiration),
            )
            .route("/api/subscriptions", get(get_subscriptions))
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

async fn get_subscriptions(
    State(state): State<Arc<AppState>>,
    Query(query): Query<SubscriptionsQuery>,
) -> Result<Json<Vec<SubscriptionResponse>>, ApiError> {
    let sort_order = SortOrder::parse(query.sort_order.as_deref())?;
    let rows = sqlx::query(
        r#"
        SELECT ts.telegram_user_id,
               ts.chat_id,
               tu.bound_imei,
               customer.name AS customer_name,
               customer.phone_number AS customer_phone_number,
               COALESCE(d.pricing_tier, 'basic') AS pricing_tier,
               ts.plan_code,
               ts.status,
               ts.current_period_start_at,
               ts.current_period_end_at,
               payments.first_subscribed_at,
               sanctions.last_pre_expiry_reminded_for_period_end_at,
               sanctions.last_pre_expiry_reminded_day,
               sanctions.last_overdue_reminded_day,
               COALESCE(sanctions.withdrawal_required, FALSE) AS withdrawal_required,
               sanctions.resolved_at AS sanction_resolved_at
        FROM telegram_subscriptions ts
        JOIN telegram_users tu
          ON tu.telegram_user_id = ts.telegram_user_id
        LEFT JOIN devices d
          ON d.imei = tu.bound_imei
        LEFT JOIN LATERAL (
            SELECT name, phone_number
            FROM customers
            WHERE imei = tu.bound_imei
            ORDER BY updated_at DESC, id DESC
            LIMIT 1
        ) customer ON TRUE
        LEFT JOIN (
            SELECT telegram_user_id, MIN(paid_at) AS first_subscribed_at
            FROM telegram_payment_events
            WHERE payment_status = 'paid'
              AND paid_at IS NOT NULL
            GROUP BY telegram_user_id
        ) payments
          ON payments.telegram_user_id = ts.telegram_user_id
        LEFT JOIN telegram_subscription_sanctions sanctions
          ON sanctions.subscription_id = ts.id
        ORDER BY
            CASE WHEN $1 = 'asc' THEN ts.current_period_end_at END ASC NULLS LAST,
            CASE WHEN $1 = 'desc' THEN ts.current_period_end_at END DESC NULLS LAST,
            ts.telegram_user_id ASC
        "#,
    )
    .bind(sort_order.as_str())
    .fetch_all(&state.pool)
    .await?;

    let now = Utc::now();
    let subscriptions = rows
        .into_iter()
        .map(|row| {
            let period_start =
                row.get::<Option<DateTime<Utc>>, _>("current_period_start_at");
            let period_end = row.get::<Option<DateTime<Utc>>, _>("current_period_end_at");
            let first_subscribed_at =
                row.get::<Option<DateTime<Utc>>, _>("first_subscribed_at");
            let lifecycle = subscription_lifecycle_state(
                period_end,
                now,
                row.get("last_pre_expiry_reminded_for_period_end_at"),
                row.get("last_pre_expiry_reminded_day"),
                row.get("last_overdue_reminded_day"),
                row.get("withdrawal_required"),
                row.get("sanction_resolved_at"),
            );

            SubscriptionResponse {
                telegram_user_id: row.get("telegram_user_id"),
                chat_id: row.get("chat_id"),
                bound_imei: row.get("bound_imei"),
                customer_name: row.get("customer_name"),
                customer_phone_number: row.get("customer_phone_number"),
                pricing_tier: row.get("pricing_tier"),
                plan_code: row.get("plan_code"),
                status: row.get("status"),
                current_period_start_at: period_start.map(|value| value.to_rfc3339()),
                current_period_end_at: period_end.map(|value| value.to_rfc3339()),
                first_subscribed_at: first_subscribed_at.map(|value| value.to_rfc3339()),
                subscribed_days: first_subscribed_at
                    .map(|value| complete_subscribed_days(value, now)),
                lifecycle_stage: lifecycle.stage.as_str(),
                lifecycle_day: lifecycle.day,
                reminder_sent: lifecycle.reminder_sent,
                fine_amount_idr: lifecycle.fine_amount_idr,
                withdrawal_required: lifecycle.withdrawal_required,
            }
        })
        .collect();

    Ok(Json(subscriptions))
}

async fn get_devices(
    State(state): State<Arc<AppState>>,
) -> Result<Json<Vec<DeviceMonitoringResponse>>, ApiError> {
    let rows = sqlx::query(
        r#"
        SELECT d.id,
               d.imei,
               customer.name AS customer_name,
               customer.phone_number AS customer_phone_number,
               d.latest_latitude,
               d.latest_longitude,
               d.last_seen_at
        FROM devices d
        LEFT JOIN LATERAL (
            SELECT name, phone_number
            FROM customers
            WHERE imei = d.imei
            ORDER BY updated_at DESC, id DESC
            LIMIT 1
        ) customer ON TRUE
        ORDER BY d.last_seen_at DESC, d.imei ASC
        "#,
    )
    .fetch_all(&state.pool)
    .await?;

    let devices = rows
        .into_iter()
        .map(|row| DeviceMonitoringResponse {
            id: row.get("id"),
            imei: row.get("imei"),
            customer_name: row.get("customer_name"),
            customer_phone_number: row.get("customer_phone_number"),
            latest_latitude: row.get("latest_latitude"),
            latest_longitude: row.get("latest_longitude"),
            last_seen_at: row
                .get::<DateTime<Utc>, _>("last_seen_at")
                .to_rfc3339(),
        })
        .collect();

    Ok(Json(devices))
}

async fn get_device_sim_cards(
    State(state): State<Arc<AppState>>,
) -> Result<Json<Vec<DeviceSimCardResponse>>, ApiError> {
    let rows = sqlx::query(
        r#"
        SELECT id, imei, sim_card, sim_card_expiration_date
        FROM devices
        ORDER BY sim_card_expiration_date ASC NULLS LAST, imei ASC
        "#,
    )
    .fetch_all(&state.pool)
    .await?;

    let devices = rows
        .into_iter()
        .map(|row| {
            let expiration_date =
                row.get::<Option<NaiveDate>, _>("sim_card_expiration_date");

            DeviceSimCardResponse {
                id: row.get("id"),
                imei: row.get("imei"),
                sim_card: row.get("sim_card"),
                sim_card_expiration_date: expiration_date
                    .map(|value| value.format("%Y-%m-%d").to_string()),
            }
        })
        .collect();

    Ok(Json(devices))
}

async fn update_device_sim_card_expiration(
    State(state): State<Arc<AppState>>,
    Path(imei): Path<String>,
    Json(request): Json<UpdateSimCardExpirationRequest>,
) -> Result<Json<DeviceSimCardResponse>, ApiError> {
    let expiration_date = parse_optional_expiration_date(request.sim_card_expiration_date)?;
    let row = sqlx::query(
        r#"
        UPDATE devices
        SET sim_card_expiration_date = $2,
            updated_at = NOW()
        WHERE imei = $1
        RETURNING id, imei, sim_card, sim_card_expiration_date
        "#,
    )
    .bind(&imei)
    .bind(expiration_date)
    .fetch_optional(&state.pool)
    .await?
    .ok_or(ApiError::DeviceNotFound)?;

    let expiration_date = row.get::<Option<NaiveDate>, _>("sim_card_expiration_date");
    Ok(Json(DeviceSimCardResponse {
        id: row.get("id"),
        imei: row.get("imei"),
        sim_card: row.get("sim_card"),
        sim_card_expiration_date: expiration_date
            .map(|value| value.format("%Y-%m-%d").to_string()),
    }))
}

fn parse_optional_expiration_date(value: Option<String>) -> Result<Option<NaiveDate>, ApiError> {
    value
        .map(|value| {
            NaiveDate::parse_from_str(&value, "%Y-%m-%d")
                .map_err(|_| ApiError::InvalidSimCardExpirationDate)
        })
        .transpose()
}

fn complete_subscribed_days(first_subscribed_at: DateTime<Utc>, now: DateTime<Utc>) -> i64 {
    now.signed_duration_since(first_subscribed_at)
        .num_days()
        .max(0)
}

fn wib_day_bounds(
    date: &str,
) -> Result<(NaiveDate, DateTime<Utc>, DateTime<Utc>), ApiError> {
    let date = NaiveDate::parse_from_str(date, "%Y-%m-%d").map_err(|_| ApiError::InvalidDate)?;
    let wib = FixedOffset::east_opt(WIB_OFFSET_SECONDS).expect("WIB offset must be valid");
    let local_start = date
        .and_hms_opt(0, 0, 0)
        .expect("midnight must be valid");
    let start_at = wib
        .from_local_datetime(&local_start)
        .single()
        .expect("fixed WIB offset must resolve one local datetime")
        .with_timezone(&Utc);
    let end_at = start_at + Duration::days(1);

    Ok((date, start_at, end_at))
}

async fn get_device_activity(
    State(state): State<Arc<AppState>>,
    Path(imei): Path<String>,
) -> Result<Json<DeviceActivityResponse>, ApiError> {
    let row = sqlx::query(
        r#"
        SELECT last_seen_at, last_heartbeat_at, latest_engine_status_guess, latest_voltage_level
        FROM devices
        WHERE imei = $1
        "#,
    )
    .bind(&imei)
    .fetch_optional(&state.pool)
    .await?
    .ok_or(ApiError::DeviceNotFound)?;
    let last_seen_at = row.get::<DateTime<Utc>, _>("last_seen_at");
    let last_heartbeat_at = row.get::<Option<DateTime<Utc>>, _>("last_heartbeat_at");
    let latest_engine_status = row.get::<Option<String>, _>("latest_engine_status_guess");
    let latest_voltage_level = row.get::<Option<i32>, _>("latest_voltage_level");

    Ok(Json(DeviceActivityResponse {
        imei,
        last_seen_at: last_seen_at.to_rfc3339(),
        latest_engine_status,
        latest_voltage_level,
        battery_reported_at: last_heartbeat_at.map(|value| value.to_rfc3339()),
    }))
}

async fn get_device_daily_summary(
    State(state): State<Arc<AppState>>,
    Path(imei): Path<String>,
    Query(query): Query<SessionsQuery>,
) -> Result<Json<DeviceDailySummaryResponse>, ApiError> {
    let (date, day_start, day_end) = wib_day_bounds(&query.date)?;
    let now = Utc::now();
    let effective_day_end = day_end.min(now);
    let customer_name = sqlx::query_scalar::<_, String>(
        r#"
        SELECT name
        FROM customers
        WHERE imei = $1
        ORDER BY updated_at DESC, id DESC
        LIMIT 1
        "#,
    )
    .bind(&imei)
    .fetch_optional(&state.pool)
    .await?;
    let rows = sqlx::query(
        r#"
        SELECT created_at, resolved_at
        FROM telegram_engine_sessions
        WHERE imei = $1
          AND created_at < $3
          AND COALESCE(resolved_at, $4) > $2
        ORDER BY created_at ASC, id ASC
        "#,
    )
    .bind(&imei)
    .bind(day_start)
    .bind(day_end)
    .bind(now)
    .fetch_all(&state.pool)
    .await?;

    let mut total_distance_km = 0.0;
    let mut riding_seconds = 0_u64;

    for row in rows {
        let started_at = row.get::<DateTime<Utc>, _>("created_at").max(day_start);
        let ended_at = row
            .get::<Option<DateTime<Utc>>, _>("resolved_at")
            .unwrap_or(effective_day_end)
            .min(effective_day_end);

        if started_at >= ended_at {
            continue;
        }

        if let Some(summary) =
            fetch_ride_summary(&state.pool, &imei, started_at, ended_at).await?
        {
            total_distance_km += summary.total_distance_km;
            riding_seconds += summary.riding_seconds;
        }
    }

    let riding_hours = riding_seconds as f64 / 3600.0;
    let average_speed_kph = if riding_hours > 0.0 {
        total_distance_km / riding_hours
    } else {
        0.0
    };

    Ok(Json(DeviceDailySummaryResponse {
        imei,
        date: date.format("%Y-%m-%d").to_string(),
        timezone: "Asia/Jakarta",
        generated_at: now.to_rfc3339(),
        customer_name,
        total_distance_km,
        riding_seconds,
        average_speed_kph,
    }))
}

async fn get_device_service(
    State(state): State<Arc<AppState>>,
    Path(imei): Path<String>,
) -> Result<Json<DeviceServiceResponse>, ApiError> {
    let device_exists =
        sqlx::query_scalar::<_, bool>("SELECT EXISTS (SELECT 1 FROM devices WHERE imei = $1)")
            .bind(&imei)
            .fetch_one(&state.pool)
            .await?;
    if !device_exists {
        return Err(ApiError::DeviceNotFound);
    }

    let total_distance_meters = sqlx::query_scalar::<_, f64>(
        r#"
        SELECT total_distance_meters
        FROM device_distance_odometer
        WHERE imei = $1
        "#,
    )
    .bind(&imei)
    .fetch_optional(&state.pool)
    .await?
    .unwrap_or(0.0)
    .max(0.0);
    let daily_rows = sqlx::query(
        r#"
        SELECT distance_date, distance_meters
        FROM device_distance_daily
        WHERE imei = $1
        ORDER BY distance_date ASC
        "#,
    )
    .bind(&imei)
    .fetch_all(&state.pool)
    .await?;
    let daily_distances = daily_rows
        .into_iter()
        .map(|row| {
            (
                row.get::<NaiveDate, _>("distance_date"),
                row.get::<f64, _>("distance_meters").max(0.0),
            )
        })
        .collect::<Vec<_>>();

    let total_tracked_distance_km = total_distance_meters / 1000.0;
    let completed_milestones = (total_tracked_distance_km / 1000.0).floor() as i64;
    let next_milestone_number = completed_milestones + 1;
    let next_milestone_km = next_milestone_number * 1000;
    let distance_remaining_km =
        (next_milestone_km as f64 - total_tracked_distance_km).max(0.0);

    Ok(Json(DeviceServiceResponse {
        imei,
        timezone: "Asia/Jakarta",
        generated_at: Utc::now().to_rfc3339(),
        total_tracked_distance_km,
        next_milestone_km,
        distance_remaining_km,
        next_recommendation: service_recommendation(next_milestone_km),
        milestones: build_service_milestones(&daily_distances, completed_milestones),
    }))
}

async fn get_device_sessions(
    State(state): State<Arc<AppState>>,
    Path(imei): Path<String>,
    Query(query): Query<SessionsQuery>,
) -> Result<Json<DeviceSessionsResponse>, ApiError> {
    let (date, day_start, day_end) = wib_day_bounds(&query.date)?;
    let now = Utc::now();
    let rows = sqlx::query(
        r#"
        SELECT id, created_at, resolved_at
        FROM telegram_engine_sessions
        WHERE imei = $1
          AND created_at < $3
          AND COALESCE(resolved_at, $4) > $2
        ORDER BY created_at DESC, id DESC
        "#,
    )
    .bind(&imei)
    .bind(day_start)
    .bind(day_end)
    .bind(now)
    .fetch_all(&state.pool)
    .await?;

    let mut sessions = Vec::with_capacity(rows.len());

    for row in rows {
        let id = row.get("id");
        let started_at = row.get::<DateTime<Utc>, _>("created_at");
        let ended_at = row.get::<Option<DateTime<Utc>>, _>("resolved_at");
        let effective_end = ended_at.unwrap_or(now);
        let summary = fetch_ride_summary(&state.pool, &imei, started_at, effective_end).await?;

        sessions.push(DeviceSessionResponse {
            id,
            started_at: started_at.to_rfc3339(),
            ended_at: ended_at.map(|value| value.to_rfc3339()),
            state: if ended_at.is_some() {
                "completed"
            } else {
                "ongoing"
            },
            duration_seconds: effective_end
                .signed_duration_since(started_at)
                .num_seconds()
                .max(0),
            distance_km: summary
                .map(|value| value.total_distance_km)
                .unwrap_or(0.0),
        });
    }

    Ok(Json(DeviceSessionsResponse {
        imei,
        date: date.format("%Y-%m-%d").to_string(),
        timezone: "Asia/Jakarta",
        sessions,
    }))
}

fn build_service_milestones(
    daily_distances: &[(NaiveDate, f64)],
    completed_milestones: i64,
) -> Vec<ServiceMilestoneResponse> {
    let mut cumulative_km = 0.0;
    let mut next_milestone_number = 1_i64;
    let mut milestones = Vec::new();

    for (date, distance_meters) in daily_distances {
        cumulative_km += distance_meters / 1000.0;

        while next_milestone_number <= completed_milestones
            && cumulative_km + f64::EPSILON >= (next_milestone_number * 1000) as f64
        {
            let milestone_km = next_milestone_number * 1000;
            let recommendation = service_recommendation(milestone_km);
            milestones.push(ServiceMilestoneResponse {
                milestone_number: next_milestone_number,
                milestone_km,
                achieved_on: date.format("%Y-%m-%d").to_string(),
                recommendation_code: recommendation.code,
                recommendation_label: format!(
                    "Rekomendasi interval {} km",
                    format_km_grouped(recommendation.interval_km)
                ),
            });
            next_milestone_number += 1;
        }
    }

    milestones.into_iter().rev().take(20).collect()
}

fn service_recommendation(milestone_km: i64) -> ServiceRecommendationResponse {
    if milestone_km % 20_000 == 0 {
        return ServiceRecommendationResponse {
            code: "service_20000",
            interval_km: 20_000,
            title: "Rekomendasi servis besar 20.000 km",
            items: vec![
                "Ganti V-belt (untuk motor matic)",
                "Ganti roller CVT jika aus (untuk motor matic)",
                "Ganti coolant (jika menggunakan radiator)",
                "Ganti filter udara",
                "Ganti busi",
                "Cek bearing roda",
                "Cek shockbreaker",
                "Cek kompresi mesin",
                "Cek rantai keteng",
                "Lakukan pemeriksaan menyeluruh seluruh sistem motor",
                "Servis besar direkomendasikan pada rentang 20.000–25.000 km",
            ],
        };
    }

    if milestone_km % 10_000 == 0 {
        return ServiceRecommendationResponse {
            code: "service_10000",
            interval_km: 10_000,
            title: "Rekomendasi servis 10.000 km",
            items: vec![
                "Lakukan semua pemeriksaan interval 5.000 km",
                "Ganti filter udara jika diperlukan",
                "Ganti busi jika diperlukan",
                "Ganti minyak rem",
                "Setel celah klep (valve clearance)",
                "Servis throttle body atau injektor",
                "Cek coolant (jika menggunakan radiator)",
            ],
        };
    }

    if milestone_km % 5_000 == 0 {
        return ServiceRecommendationResponse {
            code: "service_5000",
            interval_km: 5_000,
            title: "Rekomendasi servis 5.000 km",
            items: vec![
                "Lakukan semua pemeriksaan interval 3.000 km",
                "Ganti oli gardan (untuk motor matic)",
                "Cek V-belt dan roller CVT (untuk motor matic)",
                "Cek gear dan rantai (untuk motor manual)",
                "Cek suspensi depan dan belakang",
                "Cek bearing roda",
                "Cek sistem kemudi",
            ],
        };
    }

    if milestone_km % 3_000 == 0 {
        return ServiceRecommendationResponse {
            code: "service_3000",
            interval_km: 3_000,
            title: "Rekomendasi servis 3.000 km",
            items: vec![
                "Lakukan semua pemeriksaan interval 1.000 km",
                "Ganti oli mesin",
                "Bersihkan filter udara",
                "Cek busi",
                "Cek throttle body atau karburator",
                "Setel rantai (untuk motor manual)",
                "Cek kampas rem",
                "Cek baut-baut penting",
            ],
        };
    }

    ServiceRecommendationResponse {
        code: "service_1000",
        interval_km: 1_000,
        title: "Rekomendasi servis 1.000 km",
        items: vec![
            "Cek kondisi dan ganti oli mesin sesuai kebutuhan",
            "Cek tekanan dan kondisi ban",
            "Cek rem depan dan belakang",
            "Cek lampu, klakson, dan sein",
            "Cek aki atau baterai",
            "Cek kebocoran oli atau cairan",
            "Cek rantai dan pelumas rantai (untuk motor manual)",
        ],
    }
}

fn format_km_grouped(value: i64) -> String {
    let digits = value.to_string();
    let mut grouped = String::with_capacity(digits.len() + digits.len() / 3);

    for (index, character) in digits.chars().enumerate() {
        if index > 0 && (digits.len() - index) % 3 == 0 {
            grouped.push('.');
        }
        grouped.push(character);
    }

    grouped
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

async fn get_device_latest_location(
    State(state): State<Arc<AppState>>,
    Path(imei): Path<String>,
) -> Result<Json<LocationHistoryResponse>, ApiError> {
    let row = sqlx::query(
        r#"
        SELECT server_received_at, gps_timestamp, latitude, longitude, speed_kph, course, satellite_count
        FROM device_locations
        WHERE imei = $1
        ORDER BY server_received_at DESC, id DESC
        LIMIT 1
        "#,
    )
    .bind(&imei)
    .fetch_optional(&state.pool)
    .await?;

    let point = row.map(|row| LocationPoint {
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
    });
    let latest_server_received_at = point
        .as_ref()
        .map(|location| location.server_received_at.clone());
    let start_at = latest_server_received_at
        .clone()
        .unwrap_or_else(|| Utc::now().to_rfc3339());

    Ok(Json(LocationHistoryResponse {
        imei,
        start_at,
        latest_server_received_at,
        points: point.into_iter().collect(),
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
                send_telegram_sticker(
                    &state.http_client,
                    token,
                    subscription.chat_id,
                    PAYMENT_SUCCESS_STICKER_BYTES,
                    telegram_messages::STICKER_5_PAYMENT_SUCCESS_FILE_NAME,
                )
                .await?;
                let text = msg_46_payment_success(subscription.current_period_end_at);
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

#[cfg(test)]
mod tests {
    use chrono::{NaiveDate, TimeZone, Utc};

    use super::{
        build_service_milestones, complete_subscribed_days, parse_optional_expiration_date,
        service_recommendation, wib_day_bounds, ApiError, SortOrder,
    };

    #[test]
    fn subscription_sort_order_defaults_to_ascending() {
        assert_eq!(SortOrder::parse(None).unwrap(), SortOrder::Asc);
        assert_eq!(SortOrder::parse(Some("asc")).unwrap(), SortOrder::Asc);
        assert_eq!(SortOrder::parse(Some("DESC")).unwrap(), SortOrder::Desc);
    }

    #[test]
    fn subscription_sort_order_rejects_unsupported_values() {
        assert!(matches!(
            SortOrder::parse(Some("newest")),
            Err(ApiError::InvalidSortOrder)
        ));
    }

    #[test]
    fn subscribed_days_counts_only_complete_days() {
        let first_subscribed_at = Utc.with_ymd_and_hms(2026, 1, 1, 12, 0, 0).unwrap();
        let now = Utc.with_ymd_and_hms(2026, 1, 3, 11, 59, 59).unwrap();

        assert_eq!(complete_subscribed_days(first_subscribed_at, now), 1);
    }

    #[test]
    fn subscribed_days_does_not_return_negative_values() {
        let first_subscribed_at = Utc.with_ymd_and_hms(2026, 1, 2, 0, 0, 0).unwrap();
        let now = Utc.with_ymd_and_hms(2026, 1, 1, 0, 0, 0).unwrap();

        assert_eq!(complete_subscribed_days(first_subscribed_at, now), 0);
    }

    #[test]
    fn parses_or_clears_sim_card_expiration_date() {
        assert_eq!(
            parse_optional_expiration_date(Some("2026-08-31".to_string()))
                .unwrap()
                .unwrap()
                .to_string(),
            "2026-08-31"
        );
        assert_eq!(parse_optional_expiration_date(None).unwrap(), None);
    }

    #[test]
    fn rejects_invalid_sim_card_expiration_date() {
        assert!(matches!(
            parse_optional_expiration_date(Some("31-08-2026".to_string())),
            Err(ApiError::InvalidSimCardExpirationDate)
        ));
    }

    #[test]
    fn converts_wib_date_to_exclusive_utc_bounds() {
        let (date, start_at, end_at) = wib_day_bounds("2026-07-28").unwrap();

        assert_eq!(date.to_string(), "2026-07-28");
        assert_eq!(
            start_at,
            Utc.with_ymd_and_hms(2026, 7, 27, 17, 0, 0).unwrap()
        );
        assert_eq!(
            end_at,
            Utc.with_ymd_and_hms(2026, 7, 28, 17, 0, 0).unwrap()
        );
    }

    #[test]
    fn rejects_invalid_session_date() {
        assert!(matches!(
            wib_day_bounds("28-07-2026"),
            Err(ApiError::InvalidDate)
        ));
    }

    #[test]
    fn service_recommendation_uses_highest_matching_interval() {
        assert_eq!(service_recommendation(1_000).code, "service_1000");
        assert_eq!(service_recommendation(3_000).code, "service_3000");
        assert_eq!(service_recommendation(15_000).code, "service_5000");
        assert_eq!(service_recommendation(30_000).code, "service_10000");
        assert_eq!(service_recommendation(40_000).code, "service_20000");
    }

    #[test]
    fn service_milestones_use_crossing_date_and_keep_latest_twenty() {
        let daily = vec![
            (
                NaiveDate::from_ymd_opt(2026, 7, 28).unwrap(),
                2_500_000.0,
            ),
            (
                NaiveDate::from_ymd_opt(2026, 7, 29).unwrap(),
                22_500_000.0,
            ),
        ];
        let milestones = build_service_milestones(&daily, 25);

        assert_eq!(milestones.len(), 20);
        assert_eq!(milestones[0].milestone_km, 25_000);
        assert_eq!(milestones[0].achieved_on, "2026-07-29");
        assert_eq!(milestones[19].milestone_km, 6_000);
    }

    #[test]
    fn service_milestones_are_empty_before_first_threshold() {
        let daily = vec![(
            NaiveDate::from_ymd_opt(2026, 7, 29).unwrap(),
            999_000.0,
        )];

        assert!(build_service_milestones(&daily, 0).is_empty());
    }
}

async fn send_telegram_sticker(
    client: &Client,
    token: &str,
    chat_id: i64,
    sticker_bytes: &[u8],
    file_name: &str,
) -> Result<(), reqwest::Error> {
    let sticker_part = multipart::Part::bytes(sticker_bytes.to_vec())
        .file_name(file_name.to_string())
        .mime_str("application/x-tgsticker")?;

    let form = multipart::Form::new()
        .text("chat_id", chat_id.to_string())
        .part("sticker", sticker_part);

    let response = client
        .post(format!("https://api.telegram.org/bot{token}/sendSticker"))
        .multipart(form)
        .send()
        .await?
        .error_for_status()?;
    let _ = response.bytes().await?;
    Ok(())
}
