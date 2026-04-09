use bytes::{Buf, BytesMut};
use thiserror::Error;

const START_BYTES: [u8; 2] = [0x78, 0x78];
const STOP_BYTES: [u8; 2] = [0x0D, 0x0A];

pub const PROTOCOL_LOGIN: u8 = 0x01;
pub const PROTOCOL_LOCATION: u8 = 0x12;
pub const PROTOCOL_HEARTBEAT: u8 = 0x13;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Frame {
    pub protocol_number: u8,
    pub payload: Vec<u8>,
    pub serial: u16,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LoginPacket {
    pub imei: String,
    pub extra_data: Vec<u8>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HeartbeatPacket {
    pub terminal_info: u8,
    pub voltage_level: u8,
    pub gsm_signal_strength: u8,
    pub alarm_language: u16,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GpsTimestamp {
    pub year: u16,
    pub month: u8,
    pub day: u8,
    pub hour: u8,
    pub minute: u8,
    pub second: u8,
}

#[derive(Debug, Clone, PartialEq)]
pub struct LocationPacket {
    pub timestamp: GpsTimestamp,
    pub gps_info_length: u8,
    pub satellite_count: u8,
    pub latitude: f64,
    pub longitude: f64,
    pub speed_kph: u8,
    pub course: u16,
    pub course_status: u16,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnknownPacket {
    pub protocol_number: u8,
    pub payload: Vec<u8>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Gt06Message {
    Login(LoginPacket),
    Heartbeat(HeartbeatPacket),
    Location(LocationPacket),
    Unknown(UnknownPacket),
}

#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum ProtocolError {
    #[error("invalid frame length {0}")]
    InvalidLength(usize),
    #[error("invalid frame stop bytes")]
    InvalidStopBytes,
    #[error("checksum mismatch")]
    ChecksumMismatch,
    #[error("payload is invalid for protocol 0x{protocol_number:02X}: {message}")]
    InvalidPayload {
        protocol_number: u8,
        message: &'static str,
    },
}

pub struct FrameDecoder;

impl FrameDecoder {
    pub fn next_frame(buffer: &mut BytesMut) -> Result<Option<Frame>, ProtocolError> {
        loop {
            if buffer.len() < 2 {
                return Ok(None);
            }

            if buffer[0..2] != START_BYTES {
                buffer.advance(1);
                continue;
            }

            if buffer.len() < 5 {
                return Ok(None);
            }

            let length = buffer[2] as usize;
            if length < 5 {
                buffer.advance(1);
                return Err(ProtocolError::InvalidLength(length));
            }

            let total_frame_len = length + 5;
            if buffer.len() < total_frame_len {
                return Ok(None);
            }

            if buffer[total_frame_len - 2..total_frame_len] != STOP_BYTES {
                buffer.advance(1);
                return Err(ProtocolError::InvalidStopBytes);
            }

            let crc_index = total_frame_len - 4;
            let calculated_crc = crc16_x25(&buffer[2..crc_index]);
            let reported_crc = u16::from_be_bytes([buffer[crc_index], buffer[crc_index + 1]]);
            if calculated_crc != reported_crc {
                buffer.advance(1);
                return Err(ProtocolError::ChecksumMismatch);
            }

            let frame_bytes = buffer.split_to(total_frame_len);
            let protocol_number = frame_bytes[3];
            let payload_len = length - 5;
            let payload_start = 4;
            let payload_end = payload_start + payload_len;
            let payload = frame_bytes[payload_start..payload_end].to_vec();
            let serial = u16::from_be_bytes([frame_bytes[payload_end], frame_bytes[payload_end + 1]]);

            return Ok(Some(Frame {
                protocol_number,
                payload,
                serial,
            }));
        }
    }
}

pub fn decode_message(frame: &Frame) -> Result<Gt06Message, ProtocolError> {
    match frame.protocol_number {
        PROTOCOL_LOGIN => decode_login(frame),
        PROTOCOL_HEARTBEAT => decode_heartbeat(frame),
        PROTOCOL_LOCATION => decode_location(frame),
        other => Ok(Gt06Message::Unknown(UnknownPacket {
            protocol_number: other,
            payload: frame.payload.clone(),
        })),
    }
}

pub fn encode_ack(protocol_number: u8, serial: u16) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(10);
    bytes.extend_from_slice(&START_BYTES);
    bytes.push(0x05);
    bytes.push(protocol_number);
    bytes.extend_from_slice(&serial.to_be_bytes());

    let crc = crc16_x25(&bytes[2..]);
    bytes.extend_from_slice(&crc.to_be_bytes());
    bytes.extend_from_slice(&STOP_BYTES);
    bytes
}

pub fn crc16_x25(data: &[u8]) -> u16 {
    let mut crc = 0xFFFFu16;

    for byte in data {
        crc ^= *byte as u16;
        for _ in 0..8 {
            if crc & 0x0001 != 0 {
                crc = (crc >> 1) ^ 0x8408;
            } else {
                crc >>= 1;
            }
        }
    }

    !crc
}

fn decode_login(frame: &Frame) -> Result<Gt06Message, ProtocolError> {
    if frame.payload.len() < 8 {
        return Err(ProtocolError::InvalidPayload {
            protocol_number: frame.protocol_number,
            message: "login payload must be at least 8 bytes",
        });
    }

    Ok(Gt06Message::Login(LoginPacket {
        imei: decode_bcd_imei(&frame.payload[..8]),
        extra_data: frame.payload[8..].to_vec(),
    }))
}

pub fn format_bytes_hex(bytes: &[u8]) -> String {
    let mut output = String::with_capacity(bytes.len().saturating_mul(3).saturating_sub(1));

    for (index, byte) in bytes.iter().enumerate() {
        if index > 0 {
            output.push(' ');
        }
        let hi = byte >> 4;
        let lo = byte & 0x0F;
        output.push(nibble_to_hex(hi));
        output.push(nibble_to_hex(lo));
    }

    output
}

fn nibble_to_hex(nibble: u8) -> char {
    match nibble {
        0..=9 => char::from(b'0' + nibble),
        10..=15 => char::from(b'A' + (nibble - 10)),
        _ => unreachable!("nibble must be in range 0..=15"),
    }
}

fn decode_heartbeat(frame: &Frame) -> Result<Gt06Message, ProtocolError> {
    if frame.payload.len() < 5 {
        return Err(ProtocolError::InvalidPayload {
            protocol_number: frame.protocol_number,
            message: "heartbeat payload must be at least 5 bytes",
        });
    }

    Ok(Gt06Message::Heartbeat(HeartbeatPacket {
        terminal_info: frame.payload[0],
        voltage_level: frame.payload[1],
        gsm_signal_strength: frame.payload[2],
        alarm_language: u16::from_be_bytes([frame.payload[3], frame.payload[4]]),
    }))
}

fn decode_location(frame: &Frame) -> Result<Gt06Message, ProtocolError> {
    if frame.payload.len() < 18 {
        return Err(ProtocolError::InvalidPayload {
            protocol_number: frame.protocol_number,
            message: "location payload must be at least 18 bytes",
        });
    }

    let timestamp = GpsTimestamp {
        year: 2000 + bcd_to_decimal(frame.payload[0]) as u16,
        month: bcd_to_decimal(frame.payload[1]),
        day: bcd_to_decimal(frame.payload[2]),
        hour: bcd_to_decimal(frame.payload[3]),
        minute: bcd_to_decimal(frame.payload[4]),
        second: bcd_to_decimal(frame.payload[5]),
    };

    let gps_info = frame.payload[6];
    let gps_info_length = gps_info >> 4;
    let satellite_count = gps_info & 0x0F;

    let raw_latitude = u32::from_be_bytes([
        frame.payload[7],
        frame.payload[8],
        frame.payload[9],
        frame.payload[10],
    ]);
    let raw_longitude = u32::from_be_bytes([
        frame.payload[11],
        frame.payload[12],
        frame.payload[13],
        frame.payload[14],
    ]);
    let speed_kph = frame.payload[15];
    let course_status = u16::from_be_bytes([frame.payload[16], frame.payload[17]]);

    let mut latitude = raw_latitude as f64 / 1_800_000.0;
    let mut longitude = raw_longitude as f64 / 1_800_000.0;

    if course_status & 0x0400 != 0 {
        latitude = -latitude;
    }
    if course_status & 0x0800 != 0 {
        longitude = -longitude;
    }

    Ok(Gt06Message::Location(LocationPacket {
        timestamp,
        gps_info_length,
        satellite_count,
        latitude,
        longitude,
        speed_kph,
        course: course_status & 0x03FF,
        course_status,
    }))
}

fn decode_bcd_imei(bytes: &[u8]) -> String {
    let mut digits = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        digits.push(char::from(b'0' + ((byte >> 4) & 0x0F)));
        digits.push(char::from(b'0' + (byte & 0x0F)));
    }

    if digits.len() == 16 && digits.starts_with('0') {
        digits.remove(0);
    }

    digits
}

fn bcd_to_decimal(byte: u8) -> u8 {
    ((byte >> 4) & 0x0F) * 10 + (byte & 0x0F)
}

#[cfg(test)]
mod tests {
    use bytes::BytesMut;

    use super::{
        crc16_x25, decode_message, encode_ack, format_bytes_hex, FrameDecoder, Gt06Message,
        ProtocolError,
        PROTOCOL_HEARTBEAT, PROTOCOL_LOCATION, PROTOCOL_LOGIN,
    };

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

    #[test]
    fn decodes_login_frame() {
        let frame = build_frame(
            PROTOCOL_LOGIN,
            &[0x01, 0x23, 0x45, 0x67, 0x89, 0x01, 0x23, 0x45],
            1,
        );
        let mut buffer = BytesMut::from(frame.as_slice());
        let parsed = FrameDecoder::next_frame(&mut buffer)
            .unwrap()
            .expect("expected frame");
        let message = decode_message(&parsed).unwrap();

        match message {
            Gt06Message::Login(packet) => {
                assert_eq!(packet.imei, "123456789012345");
                assert!(packet.extra_data.is_empty());
            }
            other => panic!("expected login packet, got {other:?}"),
        }
    }

    #[test]
    fn decodes_login_frame_with_extra_data() {
        let frame = build_frame(
            PROTOCOL_LOGIN,
            &[0x01, 0x23, 0x45, 0x67, 0x89, 0x01, 0x23, 0x45, 0x42, 0x01],
            1,
        );
        let mut buffer = BytesMut::from(frame.as_slice());
        let parsed = FrameDecoder::next_frame(&mut buffer)
            .unwrap()
            .expect("expected frame");
        let message = decode_message(&parsed).unwrap();

        match message {
            Gt06Message::Login(packet) => {
                assert_eq!(packet.imei, "123456789012345");
                assert_eq!(packet.extra_data, vec![0x42, 0x01]);
            }
            other => panic!("expected login packet, got {other:?}"),
        }
    }

    #[test]
    fn decodes_heartbeat_frame() {
        let frame = build_frame(PROTOCOL_HEARTBEAT, &[0x01, 0x05, 0x04, 0x00, 0x02], 2);
        let mut buffer = BytesMut::from(frame.as_slice());
        let parsed = FrameDecoder::next_frame(&mut buffer)
            .unwrap()
            .expect("expected frame");
        let message = decode_message(&parsed).unwrap();

        match message {
            Gt06Message::Heartbeat(packet) => {
                assert_eq!(packet.terminal_info, 0x01);
                assert_eq!(packet.voltage_level, 0x05);
                assert_eq!(packet.gsm_signal_strength, 0x04);
                assert_eq!(packet.alarm_language, 0x0002);
            }
            other => panic!("expected heartbeat packet, got {other:?}"),
        }
    }

    #[test]
    fn decodes_location_frame() {
        let payload = [
            0x24, 0x04, 0x09, 0x12, 0x34, 0x56, 0xC8, 0x00, 0xA9, 0xE3, 0x58, 0x01, 0x2A,
            0x66, 0x10, 0x3C, 0x00, 0x1E, 0x01, 0xCC, 0x00, 0x01, 0x00, 0x2A, 0x00, 0x00,
            0x01,
        ];
        let frame = build_frame(PROTOCOL_LOCATION, &payload, 3);
        let mut buffer = BytesMut::from(frame.as_slice());
        let parsed = FrameDecoder::next_frame(&mut buffer)
            .unwrap()
            .expect("expected frame");
        let message = decode_message(&parsed).unwrap();

        match message {
            Gt06Message::Location(packet) => {
                assert_eq!(packet.timestamp.year, 2024);
                assert_eq!(packet.timestamp.month, 4);
                assert_eq!(packet.timestamp.day, 9);
                assert_eq!(packet.timestamp.hour, 12);
                assert_eq!(packet.timestamp.minute, 34);
                assert_eq!(packet.timestamp.second, 56);
                assert_eq!(packet.gps_info_length, 12);
                assert_eq!(packet.satellite_count, 8);
                assert!((packet.latitude - 6.185_435_6).abs() < 0.001);
                assert!((packet.longitude - 10.864_364_4).abs() < 0.001);
                assert_eq!(packet.speed_kph, 60);
                assert_eq!(packet.course, 30);
            }
            other => panic!("expected location packet, got {other:?}"),
        }
    }

    #[test]
    fn encodes_ack_frame() {
        let ack = encode_ack(PROTOCOL_LOGIN, 1);
        assert_eq!(ack.len(), 10);
        assert_eq!(&ack[0..4], &[0x78, 0x78, 0x05, PROTOCOL_LOGIN]);
        assert_eq!(&ack[4..6], &[0x00, 0x01]);
        assert_eq!(&ack[8..10], &[0x0D, 0x0A]);
    }

    #[test]
    fn rejects_invalid_checksum() {
        let mut frame = build_frame(PROTOCOL_LOGIN, &[0x01, 0x23, 0x45, 0x67, 0x89, 0x01, 0x23, 0x45], 1);
        let checksum_index = frame.len() - 4;
        frame[checksum_index] ^= 0xFF;

        let mut buffer = BytesMut::from(frame.as_slice());
        let error = FrameDecoder::next_frame(&mut buffer).expect_err("checksum should fail");
        assert_eq!(error, ProtocolError::ChecksumMismatch);
    }

    #[test]
    fn waits_for_more_data_when_frame_is_truncated() {
        let frame = build_frame(PROTOCOL_LOGIN, &[0x01, 0x23, 0x45, 0x67, 0x89, 0x01, 0x23, 0x45], 1);
        let mut buffer = BytesMut::from(&frame[..frame.len() - 2]);
        let result = FrameDecoder::next_frame(&mut buffer).unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn formats_bytes_as_hex() {
        assert_eq!(format_bytes_hex(&[0x78, 0x78, 0x0D, 0x01]), "78 78 0D 01");
    }
}
