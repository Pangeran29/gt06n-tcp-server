use std::net::SocketAddr;

use async_trait::async_trait;
use tracing::{info, warn};

use crate::protocol::{
    decode_terminal_info_flags, format_bytes_hex, EngineStatus, HeartbeatPacket, LocationPacket,
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

#[derive(Debug, Default)]
pub struct LoggingEventHandler;

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
                let engine_status = match flags.engine_status_guess {
                    EngineStatus::On => "on",
                    EngineStatus::Off => "off",
                    EngineStatus::Unknown => "unknown",
                };

                info!(
                    %peer_addr,
                    device_id = device_id.unwrap_or_else(|| "unknown".to_string()),
                    terminal_info = packet.terminal_info,
                    terminal_info_bits = flags.binary,
                    oil_and_electricity_connected = flags.oil_and_electricity_connected,
                    gps_tracking_on = flags.gps_tracking_on,
                    alarm_active = flags.alarm_active,
                    charge_connected = flags.charge_connected,
                    acc_high = flags.acc_high,
                    defense_active = flags.defense_active,
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
