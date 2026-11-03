/// Proportional-Integral (PI) Clock Drift Controller for Asynchronous Sample Rate Conversion (ASRC).
///
/// Hardware crystal oscillators on two different machines (e.g., Windows PC and MacBook)
/// typically drift by ±20 ppm to ±150 ppm (48,000 Hz vs 48,004 Hz). Without continuous
/// compensation, the receiver ring buffer will slowly overflow (increasing latency) or
/// underflow (causing audio dropouts).
///
/// This controller low-pass filters the instantaneous buffer watermark using an Exponential
/// Moving Average (EMA) to reject short-term network jitter, and computes a smooth relative
/// resampling ratio `r ∈ [0.9980, 1.0020]` (< 3.5 cents pitch shift, imperceptible to human ears).
#[derive(Debug, Clone)]
pub struct ClockDriftController {
    target_frames: f64,
    smoothed_frames: f64,
    ema_alpha: f64,
    kp: f64,
    ki: f64,
    integral: f64,
    max_deviation: f64,
    last_ratio: f64,
    initialized: bool,
}

impl ClockDriftController {
    /// Create a new PI clock drift controller targeting `target_frames` audio frames in the playback ring buffer.
    pub fn new(target_frames: usize) -> Self {
        let target = (target_frames.max(120)) as f64;
        Self {
            target_frames: target,
            smoothed_frames: target,
            ema_alpha: 0.005,
            kp: 0.0006,
            ki: 0.000002,
            integral: 0.0,
            max_deviation: 0.0020, // ±2000 ppm (±0.2%) max adjustment
            last_ratio: 1.0,
            initialized: false,
        }
    }

    /// Update the controller with the current total buffered frames (`rtrb_frames + jitter_frames`)
    /// and return the relative resampling ratio (`output_samples / input_samples`).
    ///
    /// - If `current_frames > target_frames` (buffer is filling up; sender clock is faster):
    ///   returns `ratio < 1.0` to produce slightly fewer samples and drain the excess.
    /// - If `current_frames < target_frames` (buffer is draining; sender clock is slower):
    ///   returns `ratio > 1.0` to stretch slightly and replenish the buffer.
    #[inline]
    pub fn update(&mut self, current_frames: usize) -> f64 {
        let measured = current_frames as f64;
        if !self.initialized {
            self.smoothed_frames = measured;
            self.initialized = true;
        } else {
            self.smoothed_frames =
                self.ema_alpha * measured + (1.0 - self.ema_alpha) * self.smoothed_frames;
        }

        // Normalized watermark error: positive means buffer is too full
        let error = (self.smoothed_frames - self.target_frames) / self.target_frames;

        // Anti-windup integral accumulation
        self.integral = (self.integral + self.ki * error)
            .clamp(-self.max_deviation * 0.5, self.max_deviation * 0.5);

        let adjustment = (self.kp * error + self.integral)
            .clamp(-self.max_deviation, self.max_deviation);

        self.last_ratio = 1.0 - adjustment;
        self.last_ratio
    }

    pub const fn smoothed_frames(&self) -> f64 {
        self.smoothed_frames
    }

    pub const fn last_ratio(&self) -> f64 {
        self.last_ratio
    }

    pub fn reset(&mut self) {
        self.smoothed_frames = self.target_frames;
        self.integral = 0.0;
        self.last_ratio = 1.0;
        self.initialized = false;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_pi_controller_direction() {
        let mut ctrl = ClockDriftController::new(960); // 20ms at 48kHz

        // Simulate buffer consistently higher than target (sender clock faster)
        let mut ratio = 1.0;
        for _ in 0..200 {
            ratio = ctrl.update(1440);
        }
        assert!(
            ratio < 1.0 && ratio >= 0.998,
            "expected ratio < 1.0 when buffer is over-full, got {ratio}"
        );

        ctrl.reset();
        // Simulate buffer consistently lower than target (sender clock slower)
        for _ in 0..200 {
            ratio = ctrl.update(480);
        }
        assert!(
            ratio > 1.0 && ratio <= 1.002,
            "expected ratio > 1.0 when buffer is under-full, got {ratio}"
        );
    }
}
