/// Real-time Attack/Hold/Release Noise Gate and Soft Limiter for full-duplex microphone uplink.
///
/// Suppresses background room noise (fan hum, keyboard noise) when the speaker is silent
/// and signals Discontinuous Transmission (`FLAG_SILENCE`) when fully closed to save bandwidth.
#[derive(Debug, Clone)]
pub struct NoiseGate {
    enabled: bool,
    threshold_linear: f32,
    attack_coeff: f32,
    release_coeff: f32,
    hold_frames_max: usize,
    hold_counter: usize,
    current_gain: f32,
    channels: usize,
}

impl NoiseGate {
    /// Create a new `NoiseGate`.
    ///
    /// - `threshold_db`: Gate open threshold in dBFS (e.g. `-45.0`). If `<= -90.0`, the gate stays open.
    /// - `sample_rate`: e.g. `48_000`.
    /// - `channels`: `1` or `2`.
    pub fn new(threshold_db: Option<f32>, sample_rate: u32, channels: usize) -> Self {
        let sr = (sample_rate.max(8_000)) as f32;
        let (enabled, threshold_linear) = match threshold_db {
            Some(db) if db > -90.0 => (true, 10.0f32.powf(db.clamp(-85.0, -5.0) / 20.0)),
            _ => (false, 0.0),
        };

        // 2ms fast attack, 80ms smooth release, 120ms hold window
        let attack_samples = 0.002 * sr;
        let release_samples = 0.080 * sr;
        let hold_frames_max = (0.120 * sr) as usize;

        Self {
            enabled,
            threshold_linear,
            attack_coeff: (-1.0 / attack_samples).exp(),
            release_coeff: (-1.0 / release_samples).exp(),
            hold_frames_max,
            hold_counter: 0,
            current_gain: if enabled { 0.0 } else { 1.0 },
            channels: channels.max(1),
        }
    }

    pub fn set_threshold_db(&mut self, threshold_db: Option<f32>) {
        match threshold_db {
            Some(db) if db > -90.0 => {
                self.enabled = true;
                self.threshold_linear = 10.0f32.powf(db.clamp(-85.0, -5.0) / 20.0);
            }
            _ => {
                self.enabled = false;
                self.threshold_linear = 0.0;
                self.current_gain = 1.0;
            }
        }
    }

    /// Process an interleaved audio frame in-place.
    ///
    /// Returns `true` if the frame is completely silent/gated (`DTX` eligible: sender can send
    /// a 12-byte `FLAG_SILENCE` heartbeat instead of encoding audio payload).
    #[inline]
    pub fn process_interleaved(&mut self, samples: &mut [f32], force_mute: bool) -> bool {
        if force_mute {
            for s in samples.iter_mut() {
                self.current_gain *= self.release_coeff;
                *s *= self.current_gain;
            }
            return self.current_gain < 1e-4;
        }

        if !self.enabled {
            self.current_gain = 1.0;
            let mut max_abs = 0.0f32;
            for &s in samples.iter() {
                let a = s.abs();
                if a > max_abs {
                    max_abs = a;
                }
            }
            return max_abs < 1e-5;
        }

        let frames = samples.len() / self.channels;
        let mut max_out = 0.0f32;

        for f in 0..frames {
            let base = f * self.channels;
            let mut frame_peak = 0.0f32;
            for c in 0..self.channels {
                let a = samples[base + c].abs();
                if a > frame_peak {
                    frame_peak = a;
                }
            }

            let target_gain = if frame_peak >= self.threshold_linear {
                self.hold_counter = self.hold_frames_max;
                1.0
            } else if self.hold_counter > 0 {
                self.hold_counter -= 1;
                1.0
            } else {
                0.0
            };

            if target_gain > self.current_gain {
                self.current_gain =
                    self.attack_coeff * self.current_gain + (1.0 - self.attack_coeff) * target_gain;
            } else {
                self.current_gain = self.release_coeff * self.current_gain
                    + (1.0 - self.release_coeff) * target_gain;
            }

            for c in 0..self.channels {
                let out = soft_limit(samples[base + c] * self.current_gain);
                samples[base + c] = out;
                let a = out.abs();
                if a > max_out {
                    max_out = a;
                }
            }
        }

        max_out < 1e-4
    }
}

/// Smooth soft-knee limiter preventing harsh clipping above ±0.90.
#[inline]
pub fn soft_limit(x: f32) -> f32 {
    let abs = x.abs();
    if abs <= 0.90 {
        x
    } else {
        let excess = abs - 0.90;
        let compressed = 0.90 + 0.10 * (excess / 0.10).tanh();
        compressed.copysign(x)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_noise_gate_suppresses_quiet_floor_and_opens_on_voice() {
        let mut gate = NoiseGate::new(Some(-35.0), 48_000, 2); // -35 dBFS ≈ 0.01778

        // Quiet noise floor (-60 dBFS = 0.001) should be gated to silence
        let mut quiet = vec![0.001f32; 480];
        let is_silent = gate.process_interleaved(&mut quiet, false);
        assert!(is_silent, "quiet background noise should be gated");

        // Active speech (-12 dBFS = 0.25) should open the gate
        let mut speech = vec![0.25f32; 480];
        let is_silent = gate.process_interleaved(&mut speech, false);
        assert!(!is_silent, "active speech should open the noise gate");
        assert!(speech.last().unwrap().abs() > 0.20);
    }
}
