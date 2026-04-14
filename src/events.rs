use std::net::SocketAddr;
use std::sync::Arc;

use async_trait::async_trait;
use tracing::{info, warn};

use crate::db::Database;
use crate::protocol::{
    decode_terminal_info_flags, format_bytes_hex, resolve_acc_high, resolve_engine_status_guess,
    EngineStatus, HeartbeatPacket, LocationPacket,
};

#[derive(Debug, Clone, PartialEq)]
pub enum DeviceEvent {
    Login {
        peer_addr: SocketAddr,
        imei: String,
        serial: u16,
    },
    Heartbeat {
        peer_addr: SocketAddr,
        device_id: Option<String>,
        packet: HeartbeatPacket,
    },
    Location {
        peer_addr: SocketAddr,
        device_id: Option<String>,
        packet: LocationPacket,
    },
}

#[async_trait]
pub trait DeviceEventHandler: Send + Sync {
    async fn handle_event(&self, event: DeviceEvent);
}

#[derive(Default)]
pub struct CompositeEventHandler {
    handlers: Vec<Arc<dyn DeviceEventHandler>>,
}

impl CompositeEventHandler {
    pub fn new(handlers: Vec<Arc<dyn DeviceEventHandler>>) -> Self {
        Self { handlers }
    }
}

#[async_trait]
impl DeviceEventHandler for CompositeEventHandler {
    async fn handle_event(&self, event: DeviceEvent) {
        for handler in &self.handlers {
            handler.handle_event(event.clone()).await;
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct LoggingEventHandler {
    database: Option<Database>,
}

impl LoggingEventHandler {
    pub fn new(database: Option<Database>) -> Self {
        Self { database }
    }
}

#[async_trait]
impl DeviceEventHandler for LoggingEventHandler {
    async fn handle_event(&self, event: DeviceEvent) {
        match event {
            DeviceEvent::Login {
                peer_addr,
                imei,
                serial,
            } => {
                info!(%peer_addr, %imei, serial, "device login accepted");
            }
            DeviceEvent::Heartbeat {
                peer_addr,
                device_id,
                packet,
            } => {
                let flags = decode_terminal_info_flags(packet.terminal_info);
                let previous_acc_high = match (&self.database, device_id.as_deref()) {
                    (Some(database), Some(imei)) => {
                        database.fetch_previous_acc_high(imei).await.unwrap_or(None)
                    }
                    _ => None,
                };
                let effective_acc_high = resolve_acc_high(flags.acc_high, previous_acc_high);
                let engine_status = match resolve_engine_status_guess(effective_acc_high) {
                    EngineStatus::On => "on",
                    EngineStatus::Off => "off",
                    EngineStatus::Unknown => "unknown",
                };

                info!(
                    %peer_addr,
                    device_id = device_id.unwrap_or_else(|| "unknown".to_string()),
                    terminal_info = packet.terminal_info,
                    terminal_info_bits = flags.binary,
                    gps_tracking_on = flags.gps_tracking_on,
                    bit_1_guess = flags.bit_1_guess,
                    acc_high = effective_acc_high,
                    bit_3_guess = flags.bit_3_guess,
                    vibration_detected = flags.vibration_detected,
                    bit_4_guess = flags.bit_4_guess,
                    engine_status_guess = engine_status,
                    voltage_level = packet.voltage_level,
                    gsm_signal_strength = packet.gsm_signal_strength,
                    alarm_language = packet.alarm_language,
                    "heartbeat received"
                );
            }
            DeviceEvent::Location {
                peer_addr,
                device_id,
                packet,
            } => {
                info!(
                    %peer_addr,
                    device_id = device_id.unwrap_or_else(|| "unknown".to_string()),
                    gps_timestamp = %format!(
                        "{:04}-{:02}-{:02} {:02}:{:02}:{:02}",
                        packet.timestamp.year,
                        packet.timestamp.month,
                        packet.timestamp.day,
                        packet.timestamp.hour,
                        packet.timestamp.minute,
                        packet.timestamp.second
                    ),
                    latitude = packet.latitude,
                    longitude = packet.longitude,
                    speed_kph = packet.speed_kph,
                    course = packet.course,
                    course_status = packet.course_status,
                    satellite_count = packet.satellite_count,
                    extra_data_hex = %format_bytes_hex(&packet.extra_data),
                    "location received"
                );
            }
        }
    }
}

#[derive(Debug, Default)]
pub struct NoopEventHandler;

#[async_trait]
impl DeviceEventHandler for NoopEventHandler {
    async fn handle_event(&self, event: DeviceEvent) {
        warn!(?event, "event dropped by NoopEventHandler");
    }
}
