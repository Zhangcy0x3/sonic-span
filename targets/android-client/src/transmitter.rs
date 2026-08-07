//! Platform-agnostic transmit pipeline for the Android client.
//!
//! Feeds interleaved `f32` PCM in, gets ready-to-send SonicSpan packets out.
//! Opus input is accumulated into one 20 ms frame per packet; PCM is chunked
//! into packets that stay under the UDP datagram limit.

use span_core::codec::{opus_frame_size, OpusEncoder};
use span_core::errors::AudioError;
use span_transport::protocol::{Codec, PcmPacket, MAX_PAYLOAD_BYTES};

#[derive(Debug, Clone, Copy)]
pub struct TransmitterConfig {
    pub sample_rate: u32,
    pub channels: u16,
    pub codec: Codec,
}

impl Default for TransmitterConfig {
    fn default() -> Self {
        Self {
            sample_rate: 48_000,
            channels: 1,
            codec: Codec::Opus,
        }
    }
}

/// Converts PCM into SonicSpan packets.
pub struct Transmitter {
    config: TransmitterConfig,
    encoder: Option<OpusEncoder>,
    frames_per_packet: usize,
    pending: Vec<f32>,
    sequence: u32,
}

impl Transmitter {
    pub fn new(config: TransmitterConfig) -> Result<Self, AudioError> {
        let encoder = (config.codec == Codec::Opus)
            .then(|| OpusEncoder::new(config.sample_rate, config.channels))
            .transpose()?;
        let frames_per_packet = match config.codec {
            Codec::Opus => opus_frame_size(config.sample_rate),
            // Keep PCM packets under the 1200-byte UDP payload limit once
            // interleaved channels are accounted for.
            Codec::Pcm => (MAX_PAYLOAD_BYTES / 4) / config.channels.max(1) as usize,
        };
        Ok(Self {
            config,
            encoder,
            frames_per_packet,
            pending: Vec::new(),
            sequence: 0,
        })
    }

    /// Feed interleaved `f32` samples and get ready-to-send packets back.
    pub fn process_pcm(&mut self, samples: &[f32]) -> Result<Vec<PcmPacket>, AudioError> {
        let frame_samples = self.frames_per_packet * self.config.channels as usize;
        self.pending.extend_from_slice(samples);

        let mut packets = Vec::new();
        while self.pending.len() >= frame_samples {
            let chunk: Vec<f32> = self.pending.drain(..frame_samples).collect();
            let packet = match self.config.codec {
                Codec::Pcm => PcmPacket::from_pcm(
                    self.next_sequence(),
                    self.config.sample_rate,
                    self.config.channels,
                    &chunk,
                ),
                Codec::Opus => {
                    let payload = self.encoder.as_mut().unwrap().encode(&chunk)?;
                    PcmPacket::from_opus(
                        self.next_sequence(),
                        self.config.sample_rate,
                        self.config.channels,
                        payload,
                    )
                }
            };
            packets.push(packet);
        }
        Ok(packets)
    }

    fn next_sequence(&mut self) -> u32 {
        let sequence = self.sequence;
        self.sequence = self.sequence.wrapping_add(1);
        sequence
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use span_core::codec::OpusDecoder;

    fn sine(sample_rate: u32, samples: usize) -> Vec<f32> {
        (0..samples)
            .map(|i| ((i as f32) * 440.0 * std::f32::consts::TAU / sample_rate as f32).sin() * 0.5)
            .collect()
    }

    #[test]
    fn opus_transmit_round_trip() {
        let mut transmitter = Transmitter::new(TransmitterConfig::default()).unwrap();
        let mut decoder = OpusDecoder::new(48_000, 1).unwrap();
        let input = sine(48_000, 48_000); // one second

        let mut packets = Vec::new();
        for chunk in input.chunks(960) {
            packets.extend(transmitter.process_pcm(chunk).unwrap());
        }

        assert_eq!(packets.len(), 50); // 50 × 20 ms frames

        let decoded = decoder.decode(&packets[0].payload).unwrap();
        assert_eq!(decoded.len(), 960);
        assert!(decoded.iter().all(|s| s.is_finite()));
    }

    #[test]
    fn pcm_transmit_chunks_under_datagram_limit() {
        let mut transmitter = Transmitter::new(TransmitterConfig {
            sample_rate: 48_000,
            channels: 2,
            codec: Codec::Pcm,
        })
        .unwrap();
        let packets = transmitter.process_pcm(&sine(48_000, 48_000 * 2)).unwrap();

        assert!(!packets.is_empty());
        for packet in &packets {
            assert!(packet.payload.len() <= MAX_PAYLOAD_BYTES);
            assert_eq!(packet.samples().unwrap().len() % 2, 0);
        }
        // Sequence numbers are contiguous across packets.
        for (i, packet) in packets.iter().enumerate() {
            assert_eq!(packet.sequence, i as u32);
        }
    }

    #[test]
    fn accumulates_partial_opus_frames() {
        let mut transmitter = Transmitter::new(TransmitterConfig::default()).unwrap();
        // Less than one 20 ms frame (960 samples).
        assert!(transmitter
            .process_pcm(&sine(48_000, 500))
            .unwrap()
            .is_empty());
        // The remainder completes the frame on the next call.
        let packets = transmitter.process_pcm(&sine(48_000, 460)).unwrap();
        assert_eq!(packets.len(), 1);
        assert!(!packets[0].payload.is_empty());
    }
}
