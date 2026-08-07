//! Wire format for SonicSpan PCM datagrams.
//!
//! A datagram is a fixed 19-byte header followed by little-endian `f32`
//! samples:
//!
//! ```text
//! +--------+---------+------------+-------------+----------+-------------+
//! | magic  | version | sequence   | sample rate | channels | payload len |
//! | (4 B)  | (1 B)   | u32 LE     | u32 LE      | u16 LE   | u32 LE      |
//! +--------+---------+------------+-------------+----------+-------------+
//! | payload (f32 samples, little-endian)                                 |
//! +------------------------------------------------------------------------+
//! ```

use thiserror::Error;

/// Magic bytes identifying a SonicSpan datagram.
pub const MAGIC: [u8; 4] = *b"SSP1";

/// Current protocol version.
pub const VERSION: u8 = 1;

/// Size of the fixed header in bytes.
pub const HEADER_SIZE: usize = 4 + 1 + 4 + 4 + 2 + 4;

/// Maximum payload size that still fits comfortably inside a single UDP
/// datagram without IP fragmentation (1500-byte MTU minus headers).
pub const MAX_PAYLOAD_BYTES: usize = 1200;

/// A single PCM audio packet carried in one UDP datagram.
#[derive(Debug, Clone, PartialEq)]
pub struct PcmPacket {
    /// Monotonically increasing packet sequence number.
    pub sequence: u32,
    /// Sample rate of the audio, in Hz.
    pub sample_rate: u32,
    /// Number of interleaved channels.
    pub channels: u16,
    /// Interleaved `f32` samples in the `[-1.0, 1.0]` range.
    pub samples: Vec<f32>,
}

/// Errors produced while encoding or decoding a packet.
#[derive(Debug, Error, PartialEq)]
pub enum ProtocolError {
    #[error("invalid magic, expected {MAGIC:?}")]
    InvalidMagic,

    #[error("unsupported protocol version {0}")]
    UnsupportedVersion(u8),

    #[error("packet too short: {0} bytes (header is {HEADER_SIZE} bytes)")]
    TooShort(usize),

    #[error("payload length mismatch: header says {expected} bytes, buffer has {actual}")]
    PayloadLengthMismatch { expected: usize, actual: usize },

    #[error("payload length {0} is not a multiple of 4 (f32 size)")]
    UnalignedPayload(usize),
}

impl PcmPacket {
    /// Serialize this packet into `out`.
    pub fn encode(&self, out: &mut Vec<u8>) {
        out.extend_from_slice(&MAGIC);
        out.push(VERSION);
        out.extend_from_slice(&self.sequence.to_le_bytes());
        out.extend_from_slice(&self.sample_rate.to_le_bytes());
        out.extend_from_slice(&self.channels.to_le_bytes());
        out.extend_from_slice(&((self.samples.len() * 4) as u32).to_le_bytes());
        for sample in &self.samples {
            out.extend_from_slice(&sample.to_le_bytes());
        }
    }

    /// Deserialize a packet from a received datagram.
    pub fn decode(buf: &[u8]) -> Result<Self, ProtocolError> {
        if buf.len() < HEADER_SIZE {
            return Err(ProtocolError::TooShort(buf.len()));
        }
        if buf[..4] != MAGIC {
            return Err(ProtocolError::InvalidMagic);
        }
        if buf[4] != VERSION {
            return Err(ProtocolError::UnsupportedVersion(buf[4]));
        }

        let sequence = u32::from_le_bytes(buf[5..9].try_into().unwrap());
        let sample_rate = u32::from_le_bytes(buf[9..13].try_into().unwrap());
        let channels = u16::from_le_bytes(buf[13..15].try_into().unwrap());
        let payload_len = u32::from_le_bytes(buf[15..19].try_into().unwrap()) as usize;

        if !payload_len.is_multiple_of(4) {
            return Err(ProtocolError::UnalignedPayload(payload_len));
        }
        if payload_len > buf.len() - HEADER_SIZE {
            return Err(ProtocolError::PayloadLengthMismatch {
                expected: payload_len,
                actual: buf.len() - HEADER_SIZE,
            });
        }

        let samples = buf[HEADER_SIZE..HEADER_SIZE + payload_len]
            .chunks_exact(4)
            .map(|c| f32::from_le_bytes(c.try_into().unwrap()))
            .collect();

        Ok(Self {
            sequence,
            sample_rate,
            channels,
            samples,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_packet() -> PcmPacket {
        PcmPacket {
            sequence: 42,
            sample_rate: 48_000,
            channels: 2,
            samples: vec![0.0, 0.5, -0.5, 1.0, -1.0, 0.125],
        }
    }

    #[test]
    fn round_trip() {
        let packet = sample_packet();
        let mut bytes = Vec::new();
        packet.encode(&mut bytes);
        assert_eq!(bytes.len(), HEADER_SIZE + packet.samples.len() * 4);

        let decoded = PcmPacket::decode(&bytes).unwrap();
        assert_eq!(decoded, packet);
    }

    #[test]
    fn empty_payload_round_trip() {
        let packet = PcmPacket {
            sequence: 0,
            sample_rate: 44_100,
            channels: 1,
            samples: vec![],
        };
        let mut bytes = Vec::new();
        packet.encode(&mut bytes);
        assert_eq!(bytes.len(), HEADER_SIZE);
        assert_eq!(PcmPacket::decode(&bytes).unwrap(), packet);
    }

    #[test]
    fn truncated_packet_rejected() {
        let err = PcmPacket::decode(&[0u8; 10]).unwrap_err();
        assert_eq!(err, ProtocolError::TooShort(10));
    }

    #[test]
    fn bad_magic_rejected() {
        let mut bytes = Vec::new();
        sample_packet().encode(&mut bytes);
        bytes[0] = b'X';
        assert_eq!(
            PcmPacket::decode(&bytes).unwrap_err(),
            ProtocolError::InvalidMagic
        );
    }

    #[test]
    fn bad_version_rejected() {
        let mut bytes = Vec::new();
        sample_packet().encode(&mut bytes);
        bytes[4] = 99;
        assert_eq!(
            PcmPacket::decode(&bytes).unwrap_err(),
            ProtocolError::UnsupportedVersion(99)
        );
    }

    #[test]
    fn unaligned_payload_rejected() {
        let mut bytes = Vec::new();
        sample_packet().encode(&mut bytes);
        bytes[15] = 2; // payload length 2, not a multiple of 4
        assert_eq!(
            PcmPacket::decode(&bytes).unwrap_err(),
            ProtocolError::UnalignedPayload(2)
        );
    }

    #[test]
    fn short_payload_rejected() {
        let mut bytes = Vec::new();
        sample_packet().encode(&mut bytes);
        bytes[15] = 0xF8; // huge aligned claimed payload length (0x7FFFFFF8)
        bytes[16] = 0xFF;
        bytes[17] = 0xFF;
        bytes[18] = 0x7F;
        assert!(matches!(
            PcmPacket::decode(&bytes).unwrap_err(),
            ProtocolError::PayloadLengthMismatch { .. }
        ));
    }
}
