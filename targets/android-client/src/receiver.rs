//! Platform-agnostic receiver pipeline for the Android client.
//!
//! Mirrors the desktop `receive` path: protocol decode → Opus/PCM decode →
//! dynamic jitter buffer → drift-compensated resample. The result is a plain
//! `Vec<f32>` that the platform layer plays (on Android via `AudioTrack`).

use std::time::Duration;

use span_core::codec::OpusDecoder;
use span_core::errors::AudioError;
use span_core::jitter::{JitterBuffer, JitterBufferConfig, JitterStats};
use span_core::resampler::DriftCompensator;
use span_transport::protocol::{Codec, PcmPacket};

/// Frames read from the jitter buffer per `process_packet` call.
const READ_CHUNK_FRAMES: usize = 4096;

#[derive(Debug, Clone, Copy)]
pub struct ReceiverConfig {
    /// Initial jitter buffer latency; grows/shrinks with network conditions.
    pub buffer_latency: Duration,
}

impl Default for ReceiverConfig {
    fn default() -> Self {
        Self {
            buffer_latency: Duration::from_millis(60),
        }
    }
}

/// Cumulative receiver statistics, surfaced to the UI.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct ReceiverStats {
    pub packets_received: u64,
    pub packets_lost: u64,
    pub late_packets: u64,
    pub underruns: u64,
    pub latency_ms: u64,
}

/// Decodes, reorders, and resamples incoming packets into playable samples.
pub struct Receiver {
    decoder: Option<OpusDecoder>,
    jitter: Option<JitterBuffer>,
    resampler: Option<DriftCompensator>,
    /// Output sample rate (the Android `AudioTrack` rate; same as the stream).
    output_rate: u32,
    chunk: Vec<f32>,
    config: ReceiverConfig,
    stats: ReceiverStats,
}

impl Receiver {
    pub fn new(config: ReceiverConfig) -> Self {
        Self {
            decoder: None,
            jitter: None,
            resampler: None,
            output_rate: 0,
            chunk: Vec::new(),
            config,
            stats: ReceiverStats::default(),
        }
    }

    /// Feed one decoded network packet through the pipeline and return the
    /// samples ready for playback (empty while the jitter buffer is filling).
    pub fn process_packet(&mut self, packet: &PcmPacket) -> Result<Vec<f32>, AudioError> {
        let (sample_rate, channels) = (packet.sample_rate, packet.channels);
        let needs_init = self.jitter.is_none() || self.output_rate != sample_rate;
        if needs_init {
            self.output_rate = sample_rate;
            self.decoder = (packet.codec == Codec::Opus)
                .then(|| OpusDecoder::new(sample_rate, channels))
                .transpose()?;
            self.jitter = Some(JitterBuffer::new(JitterBufferConfig {
                sample_rate,
                channels,
                initial_latency: self.config.buffer_latency,
                ..JitterBufferConfig::new(sample_rate, channels)
            }));
            self.resampler = Some(DriftCompensator::new(sample_rate, sample_rate));
            self.chunk = vec![0.0f32; READ_CHUNK_FRAMES * channels as usize];
        }

        let samples = match packet.codec {
            Codec::Pcm => packet.samples().unwrap_or_default(),
            Codec::Opus => self.decoder.as_mut().unwrap().decode(&packet.payload)?,
        };

        let jitter = self.jitter.as_mut().unwrap();
        jitter.push(packet.sequence, samples);
        let n = jitter.read(&mut self.chunk);
        if n == 0 {
            self.sync_stats();
            return Ok(Vec::new());
        }

        let resampler = self.resampler.as_mut().unwrap();
        resampler.adjust_for_occupancy(jitter.buffered_frames(), jitter.target_frames());
        let mut out = Vec::with_capacity(n);
        resampler.process(&self.chunk[..n], &mut out);
        self.sync_stats();
        Ok(out)
    }

    pub fn stats(&self) -> ReceiverStats {
        self.stats
    }

    fn sync_stats(&mut self) {
        let jitter_stats: JitterStats = self.jitter.as_ref().map(|j| j.stats()).unwrap_or_default();
        self.stats = ReceiverStats {
            packets_received: jitter_stats.packets_received,
            packets_lost: jitter_stats.packets_lost,
            late_packets: jitter_stats.late_packets,
            underruns: jitter_stats.underruns,
            latency_ms: jitter_stats.latency.as_millis() as u64,
        };
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use span_core::codec::OpusEncoder;

    fn pcm_packet(sequence: u32, tag: f32) -> PcmPacket {
        let samples: Vec<f32> = (0..1920).map(|i| tag + i as f32 * 0.001).collect();
        PcmPacket::from_pcm(sequence, 48_000, 2, &samples)
    }

    #[test]
    fn reorders_and_plays_pcm() {
        let mut receiver = Receiver::new(ReceiverConfig::default());
        let mut played = Vec::new();
        for seq in [1u32, 2, 4, 3] {
            played.extend(
                receiver
                    .process_packet(&pcm_packet(seq, seq as f32))
                    .unwrap(),
            );
        }
        // All four packets play, in sequence order.
        assert!(!played.is_empty());
        assert!(played.iter().any(|&s| (s - 1.0).abs() < 0.01));
        assert!(played.iter().any(|&s| (s - 4.0).abs() < 0.01));
        assert_eq!(receiver.stats().packets_received, 4);
        assert_eq!(receiver.stats().packets_lost, 0);
    }

    #[test]
    fn decodes_opus_packets() {
        let mut encoder = OpusEncoder::new(48_000, 2).unwrap();
        let mut receiver = Receiver::new(ReceiverConfig::default());
        let frame: Vec<f32> = (0..960 * 2)
            .map(|i| ((i as f32) * 0.01).sin() * 0.5)
            .collect();
        let payload = encoder.encode(&frame).unwrap();

        let packet = PcmPacket::from_opus(1, 48_000, 2, payload);
        let out = receiver.process_packet(&packet).unwrap();
        assert!(!out.is_empty());
        assert!(out.iter().all(|s| s.is_finite()));
        assert_eq!(receiver.stats().packets_received, 1);
    }

    #[test]
    fn tracks_lost_packets() {
        let mut receiver = Receiver::new(ReceiverConfig::default());
        receiver.process_packet(&pcm_packet(1, 1.0)).unwrap();
        receiver.process_packet(&pcm_packet(2, 2.0)).unwrap();
        // Sequence 5 means 3 and 4 were lost; they are reported once playback
        // stalls on the gap for the grace period.
        for _ in 0..8 {
            let _ = receiver.process_packet(&pcm_packet(5, 5.0)).unwrap();
        }
        assert_eq!(receiver.stats().packets_lost, 2);
    }
}
