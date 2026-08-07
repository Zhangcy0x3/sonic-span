//! A dynamic jitter buffer for networked audio.
//!
//! Network delivery is lossy and can reorder packets, so a receiver cannot
//! simply play samples in arrival order. [`JitterBuffer`] reorders out-of-order
//! packets within a bounded window, detects gaps and reports them as lost
//! (the caller sees silence), and adaptively grows or shrinks its target
//! latency so that it only buffers as much as the current network jitter
//! requires.

use std::collections::BTreeMap;
use std::time::Duration;

use crate::buffer::AudioRingBuffer;

/// Maximum number of packets held for reordering before they are declared lost.
pub const MAX_REORDER_PACKETS: usize = 64;

/// Tuning parameters for a [`JitterBuffer`].
#[derive(Debug, Clone, Copy)]
pub struct JitterBufferConfig {
    pub sample_rate: u32,
    pub channels: u16,
    pub initial_latency: Duration,
    pub min_latency: Duration,
    pub max_latency: Duration,
}

impl JitterBufferConfig {
    pub fn new(sample_rate: u32, channels: u16) -> Self {
        Self {
            sample_rate,
            channels,
            initial_latency: Duration::from_millis(60),
            min_latency: Duration::from_millis(20),
            max_latency: Duration::from_millis(200),
        }
    }
}

/// Cumulative receive statistics.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct JitterStats {
    pub packets_received: u64,
    pub packets_lost: u64,
    pub late_packets: u64,
    pub underruns: u64,
    pub overruns: u64,
    pub latency: Duration,
}

/// Reorders, buffers, and loss-detects audio packets.
pub struct JitterBuffer {
    config: JitterBufferConfig,
    ring: AudioRingBuffer,
    /// Packets that arrived before their turn (out-of-order delivery).
    pending: BTreeMap<u32, Vec<f32>>,
    next_expected: Option<u32>,
    target_frames: usize,
    stats: JitterStats,
    started: bool,
}

impl JitterBuffer {
    pub fn new(config: JitterBufferConfig) -> Self {
        // Ring sized for the maximum latency plus reorder-window headroom.
        let max_frames = duration_frames(config.sample_rate, config.max_latency);
        let capacity =
            max_frames.saturating_add(MAX_REORDER_PACKETS * config.channels as usize * 960);
        let target_frames = duration_frames(config.sample_rate, config.initial_latency);
        Self {
            config,
            ring: AudioRingBuffer::new(capacity.max(1024)),
            pending: BTreeMap::new(),
            next_expected: None,
            target_frames: target_frames.max(1),
            stats: JitterStats {
                latency: config.initial_latency,
                ..Default::default()
            },
            started: false,
        }
    }

    /// Push one packet's interleaved samples, keyed by its sequence number.
    pub fn push(&mut self, sequence: u32, samples: Vec<f32>) {
        self.stats.packets_received += 1;
        self.started = true;

        match self.next_expected {
            None => {
                self.next_expected = Some(sequence.wrapping_add(1));
                self.push_into_ring(&samples);
            }
            Some(expected) if sequence == expected => {
                self.push_into_ring(&samples);
                self.next_expected = Some(expected.wrapping_add(1));
                self.drain_pending();
            }
            Some(expected) if sequence < expected => {
                self.stats.late_packets += 1;
            }
            Some(expected) => {
                // A jump larger than the reorder window is a real loss: count
                // the missing packets and jump ahead instead of waiting.
                if sequence.wrapping_sub(expected) > MAX_REORDER_PACKETS as u32 {
                    self.stats.packets_lost += u64::from(sequence.wrapping_sub(expected));
                    self.push_into_ring(&samples);
                    self.next_expected = Some(sequence.wrapping_add(1));
                    self.drain_pending();
                    return;
                }
                self.pending.insert(sequence, samples);
                self.drain_pending();
                self.trim_pending();
            }
        }
    }

    /// Read up to `out.len()` frames, filling any shortfall with silence.
    /// Returns the number of real frames read.
    pub fn read(&mut self, out: &mut [f32]) -> usize {
        // If the ring is empty but reordered packets are already waiting, the
        // missing sequence numbers are effectively lost: jump to the oldest
        // waiting packet so playback does not stall on an in-flight gap.
        if self.ring.is_empty() {
            if let Some(expected) = self.next_expected {
                if let Some(&first) = self.pending.keys().next() {
                    if first != expected {
                        self.stats.packets_lost += u64::from(first.wrapping_sub(expected));
                        self.next_expected = Some(first);
                        self.drain_pending();
                    }
                }
            }
        }
        let n = self.ring.pop(out);
        for slot in &mut out[n..] {
            *slot = 0.0;
        }
        if self.started && n < out.len() {
            self.stats.underruns += 1;
            self.adapt_latency(true);
        } else if self.started && n == out.len() {
            self.adapt_latency(false);
        }
        n
    }

    /// Frames currently sitting in the buffer (playable + reorder window).
    pub fn buffered_frames(&self) -> usize {
        let pending_frames: usize = self.pending.values().map(Vec::len).sum();
        self.ring.len() + pending_frames
    }

    /// Current target latency in frames.
    pub fn target_frames(&self) -> usize {
        self.target_frames
    }

    pub fn config(&self) -> JitterBufferConfig {
        self.config
    }

    pub fn stats(&self) -> JitterStats {
        let mut stats = self.stats;
        stats.latency = frames_duration(self.config.sample_rate, self.target_frames);
        stats
    }

    fn push_into_ring(&mut self, samples: &[f32]) {
        let pushed = self.ring.push(samples);
        if pushed < samples.len() {
            self.stats.overruns += (samples.len() - pushed) as u64;
        }
    }

    fn drain_pending(&mut self) {
        while let Some(expected) = self.next_expected {
            if let Some(samples) = self.pending.remove(&expected) {
                self.push_into_ring(&samples);
                self.next_expected = Some(expected.wrapping_add(1));
                continue;
            }
            // The next packet is missing, but something far ahead has already
            // arrived: declare the gap lost and jump to it.
            if let Some(&first) = self.pending.keys().next() {
                if first.wrapping_sub(expected) > MAX_REORDER_PACKETS as u32 {
                    self.stats.packets_lost += u64::from(first.wrapping_sub(expected));
                    self.next_expected = Some(first);
                    continue;
                }
            }
            break;
        }
    }

    fn trim_pending(&mut self) {
        while self.pending.len() > MAX_REORDER_PACKETS {
            if let Some(&oldest) = self.pending.keys().next() {
                self.pending.remove(&oldest);
                self.stats.packets_lost += 1;
            }
        }
    }

    /// Grow the target after an underrun, shrink it when the buffer holds far
    /// more than needed.
    fn adapt_latency(&mut self, underrun: bool) {
        let min = duration_frames(self.config.sample_rate, self.config.min_latency).max(1);
        let max = duration_frames(self.config.sample_rate, self.config.max_latency);
        let step = duration_frames(self.config.sample_rate, Duration::from_millis(5)).max(1);
        if underrun {
            self.target_frames = (self.target_frames + step).min(max);
        } else if self.buffered_frames() > self.target_frames * 2 {
            self.target_frames = self.target_frames.saturating_sub(step).max(min);
        }
    }
}

fn duration_frames(sample_rate: u32, duration: Duration) -> usize {
    (sample_rate as f64 * duration.as_secs_f64()) as usize
}

fn frames_duration(sample_rate: u32, frames: usize) -> Duration {
    Duration::from_secs_f64(frames as f64 / sample_rate as f64)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config() -> JitterBufferConfig {
        JitterBufferConfig::new(48_000, 2)
    }

    fn packet(tag: f32, frames: usize) -> Vec<f32> {
        (0..frames * 2).map(|i| tag + i as f32 * 0.001).collect()
    }

    #[test]
    fn reorders_out_of_order_packets() {
        let mut jitter = JitterBuffer::new(config());
        let frames = 100;
        jitter.push(1, packet(1.0, frames));
        jitter.push(2, packet(2.0, frames));
        jitter.push(4, packet(4.0, frames));
        jitter.push(3, packet(3.0, frames)); // arrives late but in-window

        let mut out = vec![0.0f32; frames * 2 * 4];
        assert_eq!(jitter.read(&mut out), frames * 2 * 4);
        assert_eq!(out[0], 1.0);
        assert_eq!(out[frames * 2], 2.0);
        assert_eq!(out[frames * 4], 3.0);
        assert_eq!(out[frames * 6], 4.0);
        assert_eq!(jitter.stats().late_packets, 0);
        assert_eq!(jitter.stats().packets_lost, 0);
    }

    #[test]
    fn reports_lost_packets_and_plays_waiting_audio() {
        let mut jitter = JitterBuffer::new(config());
        let frames = 100;
        jitter.push(1, packet(1.0, frames));
        jitter.push(2, packet(2.0, frames));
        jitter.push(5, packet(5.0, frames)); // packets 3 and 4 are lost

        let mut out = vec![0.0f32; frames * 2 * 5];
        let n1 = jitter.read(&mut out); // packets 1 and 2
        let n2 = jitter.read(&mut out[n1..]); // packet 5, gap declared lost
        assert_eq!(n1, frames * 2 * 2);
        assert_eq!(n2, frames * 2);
        assert_eq!(out[frames * 2], 2.0);
        assert_eq!(out[n1], 5.0);
        assert_eq!(jitter.stats().packets_lost, 2);
    }

    #[test]
    fn drops_late_packets() {
        let mut jitter = JitterBuffer::new(config());
        let frames = 10;
        jitter.push(1, packet(1.0, frames));
        jitter.push(2, packet(2.0, frames));
        jitter.push(1, packet(9.0, frames)); // replay of an old packet

        assert_eq!(jitter.stats().late_packets, 1);
        let mut out = vec![0.0f32; frames * 2 * 2];
        jitter.read(&mut out);
        assert_eq!(out[0], 1.0);
        assert_eq!(out[frames * 2], 2.0);
    }

    #[test]
    fn grows_latency_after_underrun() {
        let mut jitter = JitterBuffer::new(config());
        let before = jitter.target_frames();
        jitter.push(1, packet(1.0, 10));
        let mut out = vec![0.0f32; 48_000 * 2]; // far more than buffered
        jitter.read(&mut out);
        assert!(jitter.target_frames() > before);
        assert_eq!(jitter.stats().underruns, 1);
    }

    #[test]
    fn exposes_buffered_frames() {
        let mut jitter = JitterBuffer::new(config());
        assert_eq!(jitter.buffered_frames(), 0);
        jitter.push(1, packet(1.0, 100));
        assert_eq!(jitter.buffered_frames(), 200);
    }
}
