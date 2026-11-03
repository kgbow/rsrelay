use rubato::{FastFixedIn, PolynomialDegree, Resampler};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum ResampleError {
    #[error("rubato construction error: {0}")]
    Construct(#[from] rubato::ResamplerConstructionError),
    #[error("rubato processing error: {0}")]
    Process(#[from] rubato::ResampleError),
}

/// Zero-allocation wrapper around `rubato::FastFixedIn<f32>` that converts between
/// interleaved stereo/mono audio frames and `rubato`'s planar buffers, while supporting
/// real-time relative ratio adjustment (`set_resample_ratio_relative`).
pub struct AdaptiveResampler {
    inner: FastFixedIn<f32>,
    channels: usize,
    chunk_frames: usize,
    input_planar: Vec<Vec<f32>>,
    output_planar: Vec<Vec<f32>>,
}

impl AdaptiveResampler {
    /// Create a new resampler for `chunk_frames` input frames per channel.
    ///
    /// - `input_sample_rate`: e.g. `48_000` (or `44_100` on a 44.1kHz sender device)
    /// - `output_sample_rate`: e.g. `48_000`
    /// - `max_relative_ratio`: e.g. `1.01` (allows ±1% dynamic adjustment)
    pub fn new(
        input_sample_rate: u32,
        output_sample_rate: u32,
        channels: usize,
        chunk_frames: usize,
        max_relative_ratio: f64,
    ) -> Result<Self, ResampleError> {
        let base_ratio = (output_sample_rate as f64) / (input_sample_rate as f64);
        let inner = FastFixedIn::<f32>::new(
            base_ratio,
            max_relative_ratio.max(1.005),
            PolynomialDegree::Cubic,
            chunk_frames,
            channels,
        )?;

        let max_out_frames = inner.output_frames_max();
        let input_planar = vec![vec![0.0f32; chunk_frames]; channels];
        let output_planar = vec![vec![0.0f32; max_out_frames]; channels];

        Ok(Self {
            inner,
            channels,
            chunk_frames,
            input_planar,
            output_planar,
        })
    }

    pub const fn chunk_frames(&self) -> usize {
        self.chunk_frames
    }

    /// Process exactly `chunk_frames * channels` interleaved input samples with dynamic
    /// relative clock-drift ratio `relative_ratio` (e.g. `0.9995..=1.0005`), writing
    /// the resulting interleaved samples into `out_interleaved`.
    pub fn process_interleaved(
        &mut self,
        input_interleaved: &[f32],
        relative_ratio: f64,
        out_interleaved: &mut Vec<f32>,
    ) -> Result<usize, ResampleError> {
        debug_assert_eq!(input_interleaved.len(), self.chunk_frames * self.channels);

        // De-interleave into pre-allocated planar buffers
        for frame in 0..self.chunk_frames {
            for ch in 0..self.channels {
                self.input_planar[ch][frame] = input_interleaved[frame * self.channels + ch];
            }
        }

        // Smoothly ramp the resampling ratio across the chunk
        if (relative_ratio - 1.0).abs() > 1e-7 {
            self.inner.set_resample_ratio_relative(relative_ratio, true)?;
        } else {
            self.inner.set_resample_ratio_relative(1.0, true)?;
        }

        let (_in_used, out_frames) =
            self.inner
                .process_into_buffer(&self.input_planar, &mut self.output_planar, None)?;

        let total_out_samples = out_frames * self.channels;
        out_interleaved.clear();
        if out_interleaved.capacity() < total_out_samples {
            out_interleaved.reserve(total_out_samples - out_interleaved.len());
        }

        // Re-interleave into output buffer
        for frame in 0..out_frames {
            for ch in 0..self.channels {
                out_interleaved.push(self.output_planar[ch][frame]);
            }
        }

        Ok(out_frames)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_adaptive_resampler_stereo() {
        let mut resampler = AdaptiveResampler::new(48_000, 48_000, 2, 240, 1.01)
            .expect("should initialize resampler");
        let input = vec![0.25f32; 240 * 2];
        let mut output = Vec::with_capacity(512);

        let frames = resampler
            .process_interleaved(&input, 1.0005, &mut output)
            .expect("should resample");
        assert!(frames >= 235 && frames <= 245);
        assert_eq!(output.len(), frames * 2);
    }
}
