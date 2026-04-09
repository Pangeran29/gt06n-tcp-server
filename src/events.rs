use std::net::SocketAddr;

use async_trait::async_trait;
use tracing::{info, warn};

use crate::protocol::{HeartbeatPacket, LocationPacket};

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
                info!(
                    %peer_addr,
                    device_id = device_id.unwrap_or_else(|| "unknown".to_string()),
                    terminal_info = packet.terminal_info,
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
                    latitude = packet.latitude,
                    longitude = packet.longitude,
                    speed_kph = packet.speed_kph,
                    course = packet.course,
                    satellite_count = packet.satellite_count,
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
