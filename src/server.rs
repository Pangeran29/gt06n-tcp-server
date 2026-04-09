use std::io;
use std::net::SocketAddr;
use std::sync::Arc;

use bytes::BytesMut;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tracing::{debug, info, warn};

use crate::config::Config;
use crate::events::{DeviceEvent, DeviceEventHandler};
use crate::protocol::{
    decode_message, encode_ack, FrameDecoder, Gt06Message, ProtocolError, PROTOCOL_HEARTBEAT,
    PROTOCOL_LOGIN,
};

pub struct Gt06TcpServer {
    listener: TcpListener,
    config: Config,
    event_handler: Arc<dyn DeviceEventHandler>,
}

impl Gt06TcpServer {
    pub async fn bind(
        config: Config,
        event_handler: Arc<dyn DeviceEventHandler>,
    ) -> io::Result<Self> {
        let listener = TcpListener::bind(config.bind_addr).await?;
        Ok(Self {
            listener,
            config,
            event_handler,
        })
    }

    pub fn local_addr(&self) -> io::Result<SocketAddr> {
        self.listener.local_addr()
    }

    pub async fn run(self) -> io::Result<()> {
        loop {
            let (stream, peer_addr) = self.listener.accept().await?;
            let event_handler = Arc::clone(&self.event_handler);
            let read_buffer_capacity = self.config.read_buffer_capacity;

            tokio::spawn(async move {
                if let Err(error) =
                    handle_connection(stream, peer_addr, event_handler, read_buffer_capacity).await
                {
                    warn!(%peer_addr, error = %error, "connection closed with error");
                }
            });
        }
    }
}

async fn handle_connection(
    mut stream: TcpStream,
    peer_addr: SocketAddr,
    event_handler: Arc<dyn DeviceEventHandler>,
    read_buffer_capacity: usize,
) -> io::Result<()> {
    info!(%peer_addr, "device connected");
    let mut buffer = BytesMut::with_capacity(read_buffer_capacity);
    let mut device_id: Option<String> = None;

    loop {
        let bytes_read = stream.read_buf(&mut buffer).await?;
        if bytes_read == 0 {
            info!(%peer_addr, device_id = device_id.as_deref().unwrap_or("unknown"), "device disconnected");
            return Ok(());
        }

        loop {
            match FrameDecoder::next_frame(&mut buffer) {
                Ok(Some(frame)) => match decode_message(&frame) {
                    Ok(Gt06Message::Login(packet)) => {
                        let ack = encode_ack(PROTOCOL_LOGIN, frame.serial);
                        stream.write_all(&ack).await?;

                        device_id = Some(packet.imei.clone());
                        event_handler
                            .handle_event(DeviceEvent::Login {
                                peer_addr,
                                imei: packet.imei,
                                serial: frame.serial,
                            })
                            .await;
                    }
                    Ok(Gt06Message::Heartbeat(packet)) => {
                        let ack = encode_ack(PROTOCOL_HEARTBEAT, frame.serial);
                        stream.write_all(&ack).await?;

                        event_handler
                            .handle_event(DeviceEvent::Heartbeat {
                                peer_addr,
                                device_id: device_id.clone(),
                                packet,
                            })
                            .await;
                    }
                    Ok(Gt06Message::Location(packet)) => {
                        event_handler
                            .handle_event(DeviceEvent::Location {
                                peer_addr,
                                device_id: device_id.clone(),
                                packet,
                            })
                            .await;
                    }
                    Ok(Gt06Message::Unknown(packet)) => {
                        debug!(
                            %peer_addr,
                            protocol_number = format_args!("0x{:02X}", packet.protocol_number),
                            payload_len = packet.payload.len(),
                            "unsupported GT06 packet ignored"
                        );
                    }
                    Err(error) => log_protocol_error(peer_addr, &error),
                },
                Ok(None) => break,
                Err(error) => {
                    log_protocol_error(peer_addr, &error);
                }
            }
        }
    }
}

fn log_protocol_error(peer_addr: SocketAddr, error: &ProtocolError) {
    warn!(%peer_addr, error = %error, "failed to process GT06 packet");
}
