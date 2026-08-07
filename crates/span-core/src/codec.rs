//! Opus encoding/decoding (feature `opus`).
//!
//! Wraps the [`opus`] crate with a small, SonicSpan-shaped API: interleaved
//! `f32` frames in, Opus packets out, and back again. Packets are always one
//! 20 ms frame, so encoder and decoder frame sizes agree on both ends.

use crate::errors::AudioError;
use opus::{Application, Channels, Decoder, Encoder};

/// Opus frame duration used by SonicSpan.
pub const OPUS_FRAME_DURATION_MS: u32 = 20;

/// Maximum size of a single Opus packet (larger than the 1275-byte spec
/// maximum, so we never have to worry about truncation).
pub const MAX_OPUS_PACKET_BYTES: usize = 4000;

fn channel_setting(channels: u16) -> Result<Channels, AudioError> {
    match channels {
        1 => Ok(Channels::Mono),
        2 => Ok(Channels::Stereo),
        other => Err(AudioError::ProcessingError(format!(
            "Opus supports 1 or 2 channels, got {other}"
        ))),
    }
}

/// Frames per channel in one 20 ms Opus frame at `sample_rate`.
pub fn opus_frame_size(sample_rate: u32) -> usize {
    sample_rate as usize * OPUS_FRAME_DURATION_MS as usize / 1000
}

/// A fixed-configuration Opus encoder.
pub struct OpusEncoder {
    inner: Encoder,
    sample_rate: u32,
    channels: u16,
}

impl OpusEncoder {
    pub fn new(sample_rate: u32, channels: u16) -> Result<Self, AudioError> {
        let inner = Encoder::new(sample_rate, channel_setting(channels)?, Application::Audio)
            .map_err(|e| {
                AudioError::ProcessingError(format!("failed to create Opus encoder: {e}"))
            })?;
        Ok(Self {
            inner,
            sample_rate,
            channels,
        })
    }

    pub fn sample_rate(&self) -> u32 {
        self.sample_rate
    }

    pub fn channels(&self) -> u16 {
        self.channels
    }

    /// Frames per channel for one encoded packet.
    pub fn frame_size(&self) -> usize {
        opus_frame_size(self.sample_rate)
    }

    /// Encode interleaved `f32` samples (one 20 ms frame) into an Opus packet.
    ///
    /// `pcm` must contain exactly `frame_size() * channels` samples.
    pub fn encode(&mut self, pcm: &[f32]) -> Result<Vec<u8>, AudioError> {
        self.inner
            .encode_vec_float(pcm, MAX_OPUS_PACKET_BYTES)
            .map_err(|e| AudioError::ProcessingError(format!("Opus encode failed: {e}")))
    }
}

/// A fixed-configuration Opus decoder.
pub struct OpusDecoder {
    inner: Decoder,
    sample_rate: u32,
    channels: u16,
}

impl OpusDecoder {
    pub fn new(sample_rate: u32, channels: u16) -> Result<Self, AudioError> {
        let inner = Decoder::new(sample_rate, channel_setting(channels)?).map_err(|e| {
            AudioError::ProcessingError(format!("failed to create Opus decoder: {e}"))
        })?;
        Ok(Self {
            inner,
            sample_rate,
            channels,
        })
    }

    pub fn sample_rate(&self) -> u32 {
        self.sample_rate
    }

    pub fn channels(&self) -> u16 {
        self.channels
    }

    /// Decode one Opus packet into interleaved `f32` samples.
    pub fn decode(&mut self, packet: &[u8]) -> Result<Vec<f32>, AudioError> {
        // Give the decoder enough room for a generous frame (120 ms); the
        // packet itself carries its real duration.
        let capacity = self.sample_rate as usize * 120 / 1000 * self.channels as usize;
        let mut out = vec![0.0f32; capacity];
        let frames_per_channel = self
            .inner
            .decode_float(packet, &mut out, false)
            .map_err(|e| AudioError::ProcessingError(format!("Opus decode failed: {e}")))?;
        out.truncate(frames_per_channel * self.channels as usize);
        Ok(out)
    }

    /// Conceal lost packets using Opus's built-in packet loss concealment.
    ///
    /// libopus fills the entire output buffer with concealment frames, so this
    /// returns up to 120 ms of synthesized audio.
    pub fn conceal_loss(&mut self) -> Result<Vec<f32>, AudioError> {
        self.decode(&[])
    }
}

#[cfg(all(test, feature = "opus"))]
mod tests {
    use super::*;

    fn sine(sample_rate: u32, channels: u16, frames: usize) -> Vec<f32> {
        (0..frames * channels as usize)
            .map(|i| {
                let frame = i / channels as usize;
                let amplitude = if channels as usize > 1 && i % 2 == 1 {
                    0.5
                } else {
                    1.0
                };
                amplitude
                    * ((frame as f32) * 440.0 * std::f32::consts::TAU / sample_rate as f32).sin()
            })
            .collect()
    }

    #[test]
    fn encode_decode_round_trip() {
        let sample_rate = 48_000;
        let channels = 2;
        let mut encoder = OpusEncoder::new(sample_rate, channels).unwrap();
        let mut decoder = OpusDecoder::new(sample_rate, channels).unwrap();

        let pcm = sine(sample_rate, channels, encoder.frame_size());
        let packet = encoder.encode(&pcm).unwrap();
        assert!(!packet.is_empty());
        assert!(packet.len() < pcm.len()); // compression kicks in for silence-free audio

        let decoded = decoder.decode(&packet).unwrap();
        assert_eq!(decoded.len(), pcm.len());

        // Opus is lossy but the correlation with the input should stay high.
        let correlation = best_correlation(&pcm, &decoded, 960);
        assert!(correlation > 0.95, "correlation was {correlation}");
    }

    #[test]
    fn conceal_loss_produces_a_frame() {
        let sample_rate = 48_000;
        let channels = 1;
        let mut encoder = OpusEncoder::new(sample_rate, channels).unwrap();
        let mut decoder = OpusDecoder::new(sample_rate, channels).unwrap();

        let pcm = sine(sample_rate, channels, encoder.frame_size());
        let packet = encoder.encode(&pcm).unwrap();
        let first = decoder.decode(&packet).unwrap();
        let concealed = decoder.conceal_loss().unwrap();

        assert_eq!(first.len(), pcm.len());
        assert_eq!(
            concealed.len(),
            sample_rate as usize * 120 / 1000 * channels as usize
        );
        assert!(concealed.iter().all(|s| s.is_finite()));
    }

    fn correlation(a: &[f32], b: &[f32]) -> f64 {
        let mean_a: f64 = a.iter().map(|&s| s as f64).sum::<f64>() / a.len() as f64;
        let mean_b: f64 = b.iter().map(|&s| s as f64).sum::<f64>() / b.len() as f64;
        let mut num = 0.0;
        let mut den_a = 0.0;
        let mut den_b = 0.0;
        for (&x, &y) in a.iter().zip(b) {
            let x = x as f64 - mean_a;
            let y = y as f64 - mean_b;
            num += x * y;
            den_a += x * x;
            den_b += y * y;
        }
        num / (den_a * den_b).sqrt()
    }

    /// Best cross-correlation over a lag window, because Opus's lookahead
    /// shifts the decoded signal relative to the input.
    fn best_correlation(a: &[f32], b: &[f32], max_lag: usize) -> f64 {
        let mut best = f64::MIN;
        for lag in -(max_lag as i64)..=max_lag as i64 {
            let (x, y, len) = if lag >= 0 {
                let lag = lag as usize;
                let len = a.len().saturating_sub(lag).min(b.len());
                (&a[lag..lag + len], &b[..len], len)
            } else {
                let lag = (-lag) as usize;
                let len = a.len().min(b.len().saturating_sub(lag));
                (&a[..len], &b[lag..lag + len], len)
            };
            if len > 0 {
                best = best.max(correlation(x, y));
            }
        }
        best
    }
}
