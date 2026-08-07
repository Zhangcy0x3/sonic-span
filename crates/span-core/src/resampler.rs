//! Sample-rate conversion and clock-drift compensation.
//!
//! Sender and receiver clocks are never perfectly aligned, so a receiver's
//! jitter buffer slowly fills or drains over time. [`DriftCompensator`] nudges
//! a [`LinearResampler`]'s ratio from the jitter buffer occupancy: too much
//! buffered audio means the sender clock is fast, so consume input faster;
//! too little means the opposite. The adjustment is bounded to ±2% so pitch
//! shift stays inaudible.

/// Maximum allowed deviation from the nominal resampling ratio.
pub const MAX_DRIFT_RATIO: f64 = 0.02;

/// Latency-adjustment step applied on each underrun/overrun observation.
const DRIFT_STEP: f64 = 0.0005;

/// A minimal linear-interpolation sample-rate converter.
///
/// Quality is intentionally simple: it is meant for small ratio corrections
/// around 1.0 (clock drift), not arbitrary rate conversion.
pub struct LinearResampler {
    /// Input samples consumed per output sample (`input_rate / output_rate`).
    ratio: f64,
    /// Fractional position of the next output sample within the current input
    /// segment.
    pos: f64,
}

impl LinearResampler {
    pub fn new(input_rate: u32, output_rate: u32) -> Self {
        Self {
            ratio: input_rate as f64 / output_rate as f64,
            pos: 0.0,
        }
    }

    pub fn set_rates(&mut self, input_rate: u32, output_rate: u32) {
        self.ratio = input_rate as f64 / output_rate as f64;
    }

    pub fn set_ratio(&mut self, ratio: f64) {
        self.ratio = ratio;
    }

    pub fn ratio(&self) -> f64 {
        self.ratio
    }

    /// Append `input` resampled to `output`.
    pub fn process(&mut self, input: &[f32], output: &mut Vec<f32>) {
        if input.is_empty() {
            return;
        }
        let len = input.len() as f64;
        while self.pos < len {
            let idx = self.pos.floor() as usize;
            let frac = (self.pos - idx as f64) as f32;
            if idx + 1 < input.len() {
                let a = input[idx];
                let b = input[idx + 1];
                output.push(a + (b - a) * frac);
            } else {
                output.push(input[idx]);
            }
            self.pos += self.ratio;
        }
        // Carry the fractional position into the next segment.
        self.pos -= len;
    }
}

/// Keeps a jitter buffer near its target latency by adjusting the resampler
/// ratio as clocks drift apart.
pub struct DriftCompensator {
    resampler: LinearResampler,
    base_ratio: f64,
}

impl DriftCompensator {
    pub fn new(input_rate: u32, output_rate: u32) -> Self {
        let base_ratio = input_rate as f64 / output_rate as f64;
        Self {
            resampler: LinearResampler::new(input_rate, output_rate),
            base_ratio,
        }
    }

    pub fn set_rates(&mut self, input_rate: u32, output_rate: u32) {
        self.base_ratio = input_rate as f64 / output_rate as f64;
        self.resampler.set_rates(input_rate, output_rate);
    }

    pub fn current_ratio(&self) -> f64 {
        self.resampler.ratio()
    }

    /// Append `input` resampled at the current drift-adjusted ratio.
    pub fn process(&mut self, input: &[f32], output: &mut Vec<f32>) {
        self.resampler.process(input, output);
    }

    /// Adjust the playback ratio from jitter buffer occupancy.
    ///
    /// `occupancy_frames` above `target_frames` + slack means audio is
    /// accumulating (sender clock fast): speed up by consuming input faster.
    /// Below target means the receiver clock is fast: slow down.
    pub fn adjust_for_occupancy(&mut self, occupancy_frames: usize, target_frames: usize) {
        let slack = (target_frames / 2).max(1);
        let min = self.base_ratio * (1.0 - MAX_DRIFT_RATIO);
        let max = self.base_ratio * (1.0 + MAX_DRIFT_RATIO);
        let ratio = self.resampler.ratio();
        if occupancy_frames > target_frames + slack {
            self.resampler
                .set_ratio((ratio * (1.0 + DRIFT_STEP)).min(max));
        } else if occupancy_frames + slack < target_frames {
            self.resampler
                .set_ratio((ratio * (1.0 - DRIFT_STEP)).max(min));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resamples_to_expected_length() {
        let mut resampler = LinearResampler::new(48_000, 44_100);
        let input: Vec<f32> = (0..48_000)
            .map(|i| ((i as f32) * 440.0 * std::f32::consts::TAU / 48_000.0).sin())
            .collect();
        let mut output = Vec::new();
        resampler.process(&input, &mut output);

        let expected = 44_100;
        assert!(
            (output.len() as i64 - expected as i64).abs() < 64,
            "expected ~{expected} samples, got {}",
            output.len()
        );
        assert!(output.iter().all(|s| s.is_finite()));
    }

    #[test]
    fn drift_adjustment_moves_in_the_right_direction() {
        let mut drift = DriftCompensator::new(48_000, 48_000);
        let base = drift.current_ratio();

        // Occupancy well above target: speed up (ratio rises).
        drift.adjust_for_occupancy(10_000, 2_000);
        assert!(drift.current_ratio() > base);

        // Occupancy far below target: slow down.
        drift.adjust_for_occupancy(0, 2_000);
        assert!(drift.current_ratio() < base);
    }

    #[test]
    fn drift_is_bounded() {
        let mut drift = DriftCompensator::new(48_000, 44_100);
        for _ in 0..100_000 {
            drift.adjust_for_occupancy(10_000, 2_000);
        }
        let ratio = drift.current_ratio();
        let base = 48_000.0 / 44_100.0;
        assert!(ratio <= base * (1.0 + MAX_DRIFT_RATIO) + 1e-9);
    }
}
