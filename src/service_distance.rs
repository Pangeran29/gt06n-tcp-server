use chrono::{DateTime, FixedOffset, NaiveDate, Utc};
use sqlx::{Postgres, Row, Transaction};

use crate::config::Config;
use crate::db::{Database, DatabaseError};

const WIB_OFFSET_SECONDS: i32 = 7 * 60 * 60;
const MIN_SEGMENT_METERS: f64 = 5.0;
const MAX_POINT_GAP_SECONDS: i64 = 5 * 60;
const MAX_PLAUSIBLE_SPEED_KPH: f64 = 180.0;
const MIN_REPORTED_MOVING_SPEED_KPH: i32 = 2;

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct DistanceLocationPoint {
    pub id: i64,
    pub server_received_at: DateTime<Utc>,
    pub latitude: f64,
    pub longitude: f64,
    pub speed_kph: i32,
}

#[derive(Debug, Clone, Copy, PartialEq)]
struct PreviousDistancePoint {
    id: i64,
    server_received_at: DateTime<Utc>,
    latitude: f64,
    longitude: f64,
    speed_kph: i32,
}

pub async fn process_location_distance(
    tx: &mut Transaction<'_, Postgres>,
    imei: &str,
    point: DistanceLocationPoint,
) -> Result<f64, sqlx::Error> {
    sqlx::query("SELECT pg_advisory_xact_lock(hashtext($1)::bigint)")
        .bind(imei)
        .execute(&mut **tx)
        .await?;

    let previous = sqlx::query(
        r#"
        SELECT last_processed_location_id, last_server_received_at, last_latitude,
               last_longitude, last_speed_kph
        FROM device_distance_odometer
        WHERE imei = $1
        FOR UPDATE
        "#,
    )
    .bind(imei)
    .fetch_optional(&mut **tx)
    .await?
    .and_then(|row| {
        Some(PreviousDistancePoint {
            id: row.try_get("last_processed_location_id").ok()?,
            server_received_at: row.try_get("last_server_received_at").ok()?,
            latitude: row.try_get("last_latitude").ok()?,
            longitude: row.try_get("last_longitude").ok()?,
            speed_kph: row.try_get("last_speed_kph").ok()?,
        })
    });

    if previous.map(|value| point.id <= value.id).unwrap_or(false) {
        return Ok(0.0);
    }

    let accepted_meters = previous
        .and_then(|value| accepted_segment_meters(value, point))
        .unwrap_or(0.0);

    sqlx::query(
        r#"
        INSERT INTO device_distance_odometer (
            imei, total_distance_meters, last_processed_location_id,
            last_server_received_at, last_latitude, last_longitude, last_speed_kph,
            created_at, updated_at
        )
        VALUES ($1, $2, $3, $4, $5, $6, $7, NOW(), NOW())
        ON CONFLICT (imei) DO UPDATE
        SET total_distance_meters =
                device_distance_odometer.total_distance_meters + EXCLUDED.total_distance_meters,
            last_processed_location_id = EXCLUDED.last_processed_location_id,
            last_server_received_at = EXCLUDED.last_server_received_at,
            last_latitude = EXCLUDED.last_latitude,
            last_longitude = EXCLUDED.last_longitude,
            last_speed_kph = EXCLUDED.last_speed_kph,
            updated_at = NOW()
        "#,
    )
    .bind(imei)
    .bind(accepted_meters)
    .bind(point.id)
    .bind(point.server_received_at)
    .bind(point.latitude)
    .bind(point.longitude)
    .bind(point.speed_kph)
    .execute(&mut **tx)
    .await?;

    if accepted_meters > 0.0 {
        let distance_date = wib_date(point.server_received_at);
        sqlx::query(
            r#"
            INSERT INTO device_distance_daily (
                imei, distance_date, distance_meters, created_at, updated_at
            )
            VALUES ($1, $2, $3, NOW(), NOW())
            ON CONFLICT (imei, distance_date) DO UPDATE
            SET distance_meters =
                    device_distance_daily.distance_meters + EXCLUDED.distance_meters,
                updated_at = NOW()
            "#,
        )
        .bind(imei)
        .bind(distance_date)
        .bind(accepted_meters)
        .execute(&mut **tx)
        .await?;
    }

    Ok(accepted_meters)
}

pub async fn backfill_all_device_distances(
    pool: &sqlx::PgPool,
) -> Result<(), sqlx::Error> {
    let imeis = sqlx::query_scalar::<_, String>("SELECT imei FROM devices ORDER BY imei ASC")
        .fetch_all(pool)
        .await?;

    for imei in imeis {
        backfill_device_distance(pool, &imei).await?;
    }

    Ok(())
}

pub async fn backfill_device_distance(
    pool: &sqlx::PgPool,
    imei: &str,
) -> Result<(), sqlx::Error> {
    let rows = sqlx::query(
        r#"
        SELECT id, server_received_at, latitude, longitude, speed_kph
        FROM device_locations
        WHERE imei = $1
        ORDER BY id ASC
        "#,
    )
    .bind(imei)
    .fetch_all(pool)
    .await?;
    let points = rows
        .into_iter()
        .map(|row| DistanceLocationPoint {
            id: row.get("id"),
            server_received_at: row.get("server_received_at"),
            latitude: row.get("latitude"),
            longitude: row.get("longitude"),
            speed_kph: row.get("speed_kph"),
        })
        .collect::<Vec<_>>();

    let mut tx = pool.begin().await?;
    sqlx::query("DELETE FROM device_distance_daily WHERE imei = $1")
        .bind(imei)
        .execute(&mut *tx)
        .await?;
    sqlx::query("DELETE FROM device_distance_odometer WHERE imei = $1")
        .bind(imei)
        .execute(&mut *tx)
        .await?;

    for point in points {
        process_location_distance(&mut tx, imei, point).await?;
    }

    tx.commit().await?;
    Ok(())
}

pub async fn run_service_distance_backfill_from_config(
    config: &Config,
) -> Result<(), DatabaseError> {
    let database = Database::connect(config)
        .await?
        .ok_or(DatabaseError::NotConfigured)?;
    backfill_all_device_distances(database.pool())
        .await
        .map_err(DatabaseError::Query)
}

fn accepted_segment_meters(
    previous: PreviousDistancePoint,
    current: DistanceLocationPoint,
) -> Option<f64> {
    if !valid_coordinates(previous.latitude, previous.longitude)
        || !valid_coordinates(current.latitude, current.longitude)
    {
        return None;
    }

    let gap_seconds = current
        .server_received_at
        .signed_duration_since(previous.server_received_at)
        .num_seconds();
    if !(1..=MAX_POINT_GAP_SECONDS).contains(&gap_seconds) {
        return None;
    }

    if previous.speed_kph < MIN_REPORTED_MOVING_SPEED_KPH
        && current.speed_kph < MIN_REPORTED_MOVING_SPEED_KPH
    {
        return None;
    }

    let distance_meters = haversine_distance_meters(
        previous.latitude,
        previous.longitude,
        current.latitude,
        current.longitude,
    );
    if distance_meters < MIN_SEGMENT_METERS {
        return None;
    }

    let implied_speed_kph = distance_meters / 1000.0 / (gap_seconds as f64 / 3600.0);
    if implied_speed_kph > MAX_PLAUSIBLE_SPEED_KPH {
        return None;
    }

    Some(distance_meters)
}

fn valid_coordinates(latitude: f64, longitude: f64) -> bool {
    latitude.is_finite()
        && longitude.is_finite()
        && (-90.0..=90.0).contains(&latitude)
        && (-180.0..=180.0).contains(&longitude)
}

fn haversine_distance_meters(
    start_latitude: f64,
    start_longitude: f64,
    end_latitude: f64,
    end_longitude: f64,
) -> f64 {
    let earth_radius_meters = 6_371_000.0;
    let start_latitude = start_latitude.to_radians();
    let end_latitude = end_latitude.to_radians();
    let delta_latitude = end_latitude - start_latitude;
    let delta_longitude = (end_longitude - start_longitude).to_radians();
    let a = (delta_latitude / 2.0).sin().powi(2)
        + start_latitude.cos() * end_latitude.cos() * (delta_longitude / 2.0).sin().powi(2);
    let c = 2.0 * a.sqrt().atan2((1.0 - a).sqrt());

    earth_radius_meters * c
}

fn wib_date(timestamp: DateTime<Utc>) -> NaiveDate {
    let wib = FixedOffset::east_opt(WIB_OFFSET_SECONDS).expect("WIB offset must be valid");
    timestamp.with_timezone(&wib).date_naive()
}

#[cfg(test)]
mod tests {
    use chrono::{TimeZone, Utc};

    use super::{
        accepted_segment_meters, wib_date, DistanceLocationPoint, PreviousDistancePoint,
    };

    fn previous_point() -> PreviousDistancePoint {
        PreviousDistancePoint {
            id: 1,
            server_received_at: Utc.with_ymd_and_hms(2026, 7, 28, 16, 59, 50).unwrap(),
            latitude: -6.204066,
            longitude: 106.785514,
            speed_kph: 30,
        }
    }

    fn current_point() -> DistanceLocationPoint {
        DistanceLocationPoint {
            id: 2,
            server_received_at: Utc.with_ymd_and_hms(2026, 7, 28, 17, 0, 0).unwrap(),
            latitude: -6.204500,
            longitude: 106.786000,
            speed_kph: 32,
        }
    }

    #[test]
    fn accepts_plausible_moving_segment() {
        assert!(accepted_segment_meters(previous_point(), current_point()).is_some());
    }

    #[test]
    fn rejects_stationary_jitter() {
        let mut previous = previous_point();
        previous.speed_kph = 0;
        let mut current = current_point();
        current.speed_kph = 0;

        assert_eq!(accepted_segment_meters(previous, current), None);
    }

    #[test]
    fn rejects_disconnected_and_implausible_segments() {
        let mut disconnected = current_point();
        disconnected.server_received_at =
            Utc.with_ymd_and_hms(2026, 7, 28, 17, 10, 0).unwrap();
        assert_eq!(
            accepted_segment_meters(previous_point(), disconnected),
            None
        );

        let mut jump = current_point();
        jump.latitude = -7.0;
        assert_eq!(accepted_segment_meters(previous_point(), jump), None);
    }

    #[test]
    fn attributes_segments_to_wib_date() {
        assert_eq!(
            wib_date(current_point().server_received_at).to_string(),
            "2026-07-29"
        );
    }
}
