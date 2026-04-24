use std::io;
use std::sync::Arc;

use async_trait::async_trait;
use gt06n_tcp_server::config::Config;
use gt06n_tcp_server::events::{DeviceEvent, DeviceEventHandler};
use gt06n_tcp_server::protocol::{
    crc16_x25, PROTOCOL_HEARTBEAT, PROTOCOL_LOCATION, PROTOCOL_LOGIN,
};
use gt06n_tcp_server::server::Gt06TcpServer;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::sync::{Mutex, Notify};
use tokio::time::{timeout, Duration};

#[derive(Default)]
struct RecordingHandler {
    events: Mutex<Vec<DeviceEvent>>,
    notify: Notify,
}

#[async_trait]
impl DeviceEventHandler for RecordingHandler {
    async fn handle_event(&self, event: DeviceEvent) {
        self.events.lock().await.push(event);
        self.notify.notify_waiters();
    }
}

fn build_frame(protocol_number: u8, payload: &[u8], serial: u16) -> Vec<u8> {
    let mut bytes = Vec::new();
    let length = payload.len() + 5;
    bytes.extend_from_slice(&[0x78, 0x78, length as u8, protocol_number]);
    bytes.extend_from_slice(payload);
    bytes.extend_from_slice(&serial.to_be_bytes());
    let crc = crc16_x25(&bytes[2..]);
    bytes.extend_from_slice(&crc.to_be_bytes());
    bytes.extend_from_slice(&[0x0D, 0x0A]);
    bytes
}

#[tokio::test]
async fn accepts_login_heartbeat_and_location_packets() -> io::Result<()> {
    let handler = Arc::new(RecordingHandler::default());
    let config = Config::from_pairs([
        ("GT06_BIND_ADDR", "127.0.0.1:0"),
        ("GT06_READ_BUFFER_CAPACITY", "1024"),
    ]);

    let server = Gt06TcpServer::bind(config, handler.clone()).await?;
    let addr = server.local_addr()?;
    let server_task = tokio::spawn(server.run());

    let mut client = TcpStream::connect(addr).await?;

    let login_frame = build_frame(
        PROTOCOL_LOGIN,
        &[0x01, 0x23, 0x45, 0x67, 0x89, 0x01, 0x23, 0x45, 0x42],
        1,
    );
    client.write_all(&login_frame).await?;

    let mut ack = [0u8; 10];
    client.read_exact(&mut ack).await?;
    assert_eq!(&ack[0..6], &[0x78, 0x78, 0x05, PROTOCOL_LOGIN, 0x00, 0x01]);

    let heartbeat_frame = build_frame(PROTOCOL_HEARTBEAT, &[0x01, 0x05, 0x04, 0x00, 0x02], 2);
    client.write_all(&heartbeat_frame).await?;
    client.read_exact(&mut ack).await?;
    assert_eq!(
        &ack[0..6],
        &[0x78, 0x78, 0x05, PROTOCOL_HEARTBEAT, 0x00, 0x02]
    );

    let location_payload = [
        0x24, 0x04, 0x09, 0x12, 0x34, 0x56, 0xC8, 0x00, 0xA9, 0xE3, 0x58, 0x01, 0x2A, 0x66, 0x10,
        0x3C, 0x00, 0x1E, 0x01, 0xCC, 0x00, 0x01, 0x00, 0x2A, 0x00, 0x00, 0x01,
    ];
    let location_frame = build_frame(PROTOCOL_LOCATION, &location_payload, 3);
    client.write_all(&location_frame).await?;

    timeout(Duration::from_secs(2), async {
        loop {
            if handler.events.lock().await.len() >= 3 {
                break;
            }
            handler.notify.notified().await;
        }
    })
    .await
    .expect("expected device events to be recorded");

    let events = handler.events.lock().await.clone();
    assert!(matches!(events[0], DeviceEvent::Login { .. }));
    assert!(matches!(events[1], DeviceEvent::Heartbeat { .. }));
    match &events[2] {
        DeviceEvent::Location {
            device_id, packet, ..
        } => {
            assert_eq!(device_id.as_deref(), Some("123456789012345"));
            assert!((packet.latitude - 6.185_435_6).abs() < 0.001);
            assert!((packet.longitude - 10.864_364_4).abs() < 0.001);
            assert_eq!(packet.speed_kph, 60);
        }
        other => panic!("expected location event, got {other:?}"),
    }

    server_task.abort();
    let _ = server_task.await;

    Ok(())
}
