//! Windows WASAPI Loopback helpers and optional silent render stream keepalive.

use cpal::traits::{DeviceTrait, StreamTrait};
use cpal::SampleFormat;
use crate::device::AudioError;

/// Optional silent render stream for Windows WASAPI Loopback.
///
/// On Windows, when no application is actively playing audio, the WASAPI audio engine
/// pauses the render clock and stops invoking `AUDCLNT_STREAMFLAGS_LOOPBACK` capture
/// callbacks. Keeping a low-rate zero-amplitude shared-mode output stream open forces
/// WASAPI to continue delivering regular callbacks with zero latency on audio resume.
pub struct SilentKeepaliveStream {
    _stream: cpal::Stream,
}

impl SilentKeepaliveStream {
    pub fn start(output_device: &cpal::Device) -> Result<Self, AudioError> {
        let supported = output_device.default_output_config()?;
        let sample_format = supported.sample_format();
        let config: cpal::StreamConfig = supported.into();
        let err_fn = |err| {
            tracing::debug!("WASAPI silent keepalive stream notice: {err}");
        };
        let stream = match sample_format {
            SampleFormat::F32 => output_device.build_output_stream(
                &config,
                move |data: &mut [f32], _: &cpal::OutputCallbackInfo| {
                    data.fill(0.0);
                },
                err_fn,
                None,
            )?,
            SampleFormat::I16 => output_device.build_output_stream(
                &config,
                move |data: &mut [i16], _: &cpal::OutputCallbackInfo| {
                    data.fill(0);
                },
                err_fn,
                None,
            )?,
            SampleFormat::I32 => output_device.build_output_stream(
                &config,
                move |data: &mut [i32], _: &cpal::OutputCallbackInfo| {
                    data.fill(0);
                },
                err_fn,
                None,
            )?,
            SampleFormat::U16 => output_device.build_output_stream(
                &config,
                move |data: &mut [u16], _: &cpal::OutputCallbackInfo| {
                    data.fill(32768);
                },
                err_fn,
                None,
            )?,
            other => return Err(AudioError::UnsupportedSampleFormat(other)),
        };
        stream.play()?;
        Ok(Self { _stream: stream })
    }
}
