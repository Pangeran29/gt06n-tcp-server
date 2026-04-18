use std::net::SocketAddr;
use std::sync::Arc;

use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::{Json, Router};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sqlx::Row;
use thiserror::Error;

use crate::config::Config;
use crate::db::{Database, DatabaseError};

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
    #[error("database query failed: {0}")]
    Query(#[from] sqlx::Error),
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let status = match self {
            Self::InvalidStartAt => StatusCode::BAD_REQUEST,
            Self::MissingDatabase => StatusCode::INTERNAL_SERVER_ERROR,
            Self::Database(_) | Self::Query(_) | Self::Bind(_) => {
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
}

#[derive(Debug, Serialize)]
struct ApiErrorBody {
    error: String,
}

#[derive(Debug, Deserialize)]
struct LocationsQuery {
    start_at: String,
}

#[derive(Debug, Serialize)]
struct LocationHistoryResponse {
    imei: String,
    start_at: String,
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
        let database = Database::connect(config).await?.ok_or(ApiError::MissingDatabase)?;
        let state = Arc::new(AppState {
            pool: database.pool().clone(),
        });

        let router = Router::new()
            .route("/api/devices/{imei}/locations", get(get_device_locations))
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
        axum::serve(listener, self.router).await.map_err(ApiError::Bind)
    }
}

async fn get_device_locations(
    State(state): State<Arc<AppState>>,
    Path(imei): Path<String>,
    Query(query): Query<LocationsQuery>,
) -> Result<Json<LocationHistoryResponse>, ApiError> {
    let start_at = DateTime::parse_from_rfc3339(&query.start_at)
        .map_err(|_| ApiError::InvalidStartAt)?
        .with_timezone(&Utc);

    let rows = sqlx::query(
        r#"
        SELECT server_received_at, gps_timestamp, latitude, longitude, speed_kph, course, satellite_count
        FROM device_locations
        WHERE imei = $1
          AND server_received_at >= $2
        ORDER BY server_received_at ASC
        "#,
    )
    .bind(&imei)
    .bind(start_at)
    .fetch_all(&state.pool)
    .await?;

    let points = rows
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

    Ok(Json(LocationHistoryResponse {
        imei,
        start_at: start_at.to_rfc3339(),
        points,
    }))
}
