//! Windows WASAPI Loopback helpers and optional silent render stream keepalive.

use cpal::traits::{DeviceTrait, StreamTrait};
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
        let config: cpal::StreamConfig = supported.into();
        let stream = output_device.build_output_stream(
            &config,
            move |data: &mut [f32], _: &cpal::OutputCallbackInfo| {
                data.fill(0.0);
            },
            move |err| {
                tracing::debug!("WASAPI silent keepalive stream notice: {err}");
            },
            None,
        )?;
        stream.play()?;
        Ok(Self { _stream: stream })
    }
}
