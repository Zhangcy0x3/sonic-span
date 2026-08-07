//! Wire format for SonicSpan audio datagrams.
//!
//! A datagram is a fixed 20-byte header followed by the codec payload:
//!
//! ```text
//! +--------+---------+---------+------------+-------------+----------+-------------+
//! | magic  | version | codec   | sequence   | sample rate | channels | payload len |
//! | (4 B)  | (1 B)   | (1 B)   | u32 LE     | u32 LE      | u16 LE   | u32 LE      |
//! +--------+---------+---------+------------+-------------+----------+-------------+
//! | payload (codec-dependent)                                                  |
//! +----------------------------------------------------------------------------+
//! ```
//!
//! `Codec::Pcm` payloads are little-endian `f32` samples in the `[-1.0, 1.0]`
//! range. `Codec::Opus` payloads are single Opus packets (see
//! `span-core::codec`).

use thiserror::Error;

/// Magic bytes identifying a SonicSpan datagram.
pub const MAGIC: [u8; 4] = *b"SSP1";

/// Current protocol version.
pub const VERSION: u8 = 2;

/// Size of the fixed header in bytes.
pub const HEADER_SIZE: usize = 4 + 1 + 1 + 4 + 4 + 2 + 4;

/// Maximum *uncompressed* payload size that still fits comfortably inside a
/// single UDP datagram without IP fragmentation (1500-byte MTU minus headers).
pub const MAX_PAYLOAD_BYTES: usize = 1200;

/// Payload codec carried in a packet.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Codec {
    /// Raw little-endian `f32` samples, one per interleaved frame slot.
    Pcm,
    /// One Opus packet (see `span-core::codec`).
    Opus,
}

impl Codec {
    pub fn to_u8(self) -> u8 {
        match self {
            Self::Pcm => 0,
            Self::Opus => 1,
        }
    }

    pub fn from_u8(value: u8) -> Option<Self> {
        match value {
            0 => Some(Self::Pcm),
            1 => Some(Self::Opus),
            _ => None,
        }
    }
}

/// A single audio packet carried in one datagram.
#[derive(Debug, Clone, PartialEq)]
pub struct PcmPacket {
    /// Codec used for `payload`.
    pub codec: Codec,
    /// Monotonically increasing packet sequence number.
    pub sequence: u32,
    /// Sample rate of the audio, in Hz.
    pub sample_rate: u32,
    /// Number of interleaved channels.
    pub channels: u16,
    /// Codec-dependent payload bytes.
    pub payload: Vec<u8>,
}

impl PcmPacket {
    /// Build a packet carrying raw interleaved `f32` samples.
    pub fn from_pcm(sequence: u32, sample_rate: u32, channels: u16, samples: &[f32]) -> Self {
        let mut payload = Vec::with_capacity(samples.len() * 4);
        for sample in samples {
            payload.extend_from_slice(&sample.to_le_bytes());
        }
        Self {
            codec: Codec::Pcm,
            sequence,
            sample_rate,
            channels,
            payload,
        }
    }

    /// Build a packet carrying one Opus-encoded frame.
    pub fn from_opus(sequence: u32, sample_rate: u32, channels: u16, payload: Vec<u8>) -> Self {
        Self {
            codec: Codec::Opus,
            sequence,
            sample_rate,
            channels,
            payload,
        }
    }

    /// Decode a `Codec::Pcm` payload into interleaved `f32` samples.
    pub fn samples(&self) -> Option<Vec<f32>> {
        if self.codec != Codec::Pcm {
            return None;
        }
        Some(
            self.payload
                .chunks_exact(4)
                .map(|chunk| f32::from_le_bytes(chunk.try_into().unwrap()))
                .collect(),
        )
    }

    /// Serialize this packet into `out`.
    pub fn encode(&self, out: &mut Vec<u8>) {
        out.extend_from_slice(&MAGIC);
        out.push(VERSION);
        out.push(self.codec.to_u8());
        out.extend_from_slice(&self.sequence.to_le_bytes());
        out.extend_from_slice(&self.sample_rate.to_le_bytes());
        out.extend_from_slice(&self.channels.to_le_bytes());
        out.extend_from_slice(&(self.payload.len() as u32).to_le_bytes());
        out.extend_from_slice(&self.payload);
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
        let codec = Codec::from_u8(buf[5]).ok_or(ProtocolError::InvalidCodec(buf[5]))?;

        let sequence = u32::from_le_bytes(buf[6..10].try_into().unwrap());
        let sample_rate = u32::from_le_bytes(buf[10..14].try_into().unwrap());
        let channels = u16::from_le_bytes(buf[14..16].try_into().unwrap());
        let payload_len = u32::from_le_bytes(buf[16..20].try_into().unwrap()) as usize;

        if codec == Codec::Pcm && !payload_len.is_multiple_of(4) {
            return Err(ProtocolError::UnalignedPayload(payload_len));
        }
        if payload_len > buf.len() - HEADER_SIZE {
            return Err(ProtocolError::PayloadLengthMismatch {
                expected: payload_len,
                actual: buf.len() - HEADER_SIZE,
            });
        }

        Ok(Self {
            codec,
            sequence,
            sample_rate,
            channels,
            payload: buf[HEADER_SIZE..HEADER_SIZE + payload_len].to_vec(),
        })
    }
}

/// Errors produced while encoding or decoding a packet.
#[derive(Debug, Error, PartialEq)]
pub enum ProtocolError {
    #[error("invalid magic, expected {MAGIC:?}")]
    InvalidMagic,

    #[error("unsupported protocol version {0}")]
    UnsupportedVersion(u8),

    #[error("unknown codec {0}")]
    InvalidCodec(u8),

    #[error("packet too short: {0} bytes (header is {HEADER_SIZE} bytes)")]
    TooShort(usize),

    #[error("payload length mismatch: header says {expected} bytes, buffer has {actual}")]
    PayloadLengthMismatch { expected: usize, actual: usize },

    #[error("PCM payload length {0} is not a multiple of 4 (f32 size)")]
    UnalignedPayload(usize),
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_packet() -> PcmPacket {
        PcmPacket::from_pcm(42, 48_000, 2, &[0.0, 0.5, -0.5, 1.0, -1.0, 0.125])
    }

    #[test]
    fn round_trip() {
        let packet = sample_packet();
        let mut bytes = Vec::new();
        packet.encode(&mut bytes);
        assert_eq!(bytes.len(), HEADER_SIZE + packet.payload.len());

        let decoded = PcmPacket::decode(&bytes).unwrap();
        assert_eq!(decoded, packet);
        assert_eq!(
            decoded.samples().unwrap(),
            vec![0.0, 0.5, -0.5, 1.0, -1.0, 0.125]
        );
    }

    #[test]
    fn empty_payload_round_trip() {
        let packet = PcmPacket::from_pcm(0, 44_100, 1, &[]);
        let mut bytes = Vec::new();
        packet.encode(&mut bytes);
        assert_eq!(bytes.len(), HEADER_SIZE);
        assert_eq!(PcmPacket::decode(&bytes).unwrap(), packet);
    }

    #[test]
    fn opus_payload_round_trip() {
        let packet = PcmPacket::from_opus(7, 48_000, 2, vec![1, 2, 3, 4, 5]);
        let mut bytes = Vec::new();
        packet.encode(&mut bytes);
        let decoded = PcmPacket::decode(&bytes).unwrap();
        assert_eq!(decoded, packet);
        assert_eq!(decoded.codec, Codec::Opus);
        assert_eq!(decoded.samples(), None);
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
    fn bad_codec_rejected() {
        let mut bytes = Vec::new();
        sample_packet().encode(&mut bytes);
        bytes[5] = 99;
        assert_eq!(
            PcmPacket::decode(&bytes).unwrap_err(),
            ProtocolError::InvalidCodec(99)
        );
    }

    #[test]
    fn unaligned_payload_rejected() {
        let mut bytes = Vec::new();
        sample_packet().encode(&mut bytes);
        bytes[16] = 2; // PCM payload length 2, not a multiple of 4
        assert_eq!(
            PcmPacket::decode(&bytes).unwrap_err(),
            ProtocolError::UnalignedPayload(2)
        );
    }

    #[test]
    fn short_payload_rejected() {
        let mut bytes = Vec::new();
        sample_packet().encode(&mut bytes);
        bytes[16] = 0xF8; // huge aligned claimed payload length (0x7FFFFFF8)
        bytes[17] = 0xFF;
        bytes[18] = 0xFF;
        bytes[19] = 0x7F;
        assert!(matches!(
            PcmPacket::decode(&bytes).unwrap_err(),
            ProtocolError::PayloadLengthMismatch { .. }
        ));
    }
}
