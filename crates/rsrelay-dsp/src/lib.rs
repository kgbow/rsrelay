//! `rsrelay-dsp`: Real-time audio encoding, decoding, adaptive jitter buffering,
//! and PI-controlled dynamic resampling for clock drift compensation.

pub mod codec;
pub mod drift;
pub mod jitter;
pub mod resampler;

pub use codec::{AudioDecoder, AudioEncoder, CodecError};
pub use drift::ClockDriftController;
pub use jitter::{JitterBuffer, JitterStats, PopOutcome, JB_CAPACITY};
pub use resampler::{AdaptiveResampler, ResampleError};
