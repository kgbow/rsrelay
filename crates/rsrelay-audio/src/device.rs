use cpal::traits::{DeviceTrait, HostTrait};
use serde::{Deserialize, Serialize};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum AudioError {
    #[error("no suitable audio device found: {0}")]
    DeviceNotFound(String),
    #[error("cpal devices error: {0}")]
    Devices(#[from] cpal::DevicesError),
    #[error("cpal default stream config error: {0}")]
    DefaultConfig(#[from] cpal::DefaultStreamConfigError),
    #[error("cpal build stream error: {0}")]
    BuildStream(#[from] cpal::BuildStreamError),
    #[error("cpal play stream error: {0}")]
    PlayStream(#[from] cpal::PlayStreamError),
    #[error("unsupported sample format: {0:?}")]
    UnsupportedSampleFormat(cpal::SampleFormat),
}

/// Audio capture source mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum CaptureSource {
    /// Capture desktop/system playback audio (WASAPI Loopback on Windows, ScreenCaptureKit / Loopback on macOS).
    SystemLoopback,
    /// Capture from a microphone or line-in input device.
    Microphone,
    /// Generate a synthetic phase-continuous 440Hz/880Hz stereo sine wave (for zero-dependency E2E testing).
    SyntheticTone,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AudioDeviceInfo {
    pub name: String,
    pub is_input: bool,
    pub is_output: bool,
    pub is_default_input: bool,
    pub is_default_output: bool,
    pub default_sample_rate: Option<u32>,
    pub default_channels: Option<u16>,
}

/// Enumerate all available input and output audio devices on the current host.
pub fn list_audio_devices() -> Result<Vec<AudioDeviceInfo>, AudioError> {
    let host = cpal::default_host();
    let default_in_name = host.default_input_device().and_then(|d| d.name().ok());
    let default_out_name = host.default_output_device().and_then(|d| d.name().ok());

    let mut infos = Vec::new();

    if let Ok(inputs) = host.input_devices() {
        for dev in inputs {
            let name = dev.name().unwrap_or_else(|_| "Unknown Input".to_string());
            let cfg = dev.default_input_config().ok();
            infos.push(AudioDeviceInfo {
                is_default_input: default_in_name.as_deref() == Some(name.as_str()),
                is_default_output: false,
                name,
                is_input: true,
                is_output: false,
                default_sample_rate: cfg.as_ref().map(|c| c.sample_rate().0),
                default_channels: cfg.as_ref().map(|c| c.channels()),
            });
        }
    }

    if let Ok(outputs) = host.output_devices() {
        for dev in outputs {
            let name = dev.name().unwrap_or_else(|_| "Unknown Output".to_string());
            let cfg = dev.default_output_config().ok();
            infos.push(AudioDeviceInfo {
                is_default_input: false,
                is_default_output: default_out_name.as_deref() == Some(name.as_str()),
                name,
                is_input: false,
                is_output: true,
                default_sample_rate: cfg.as_ref().map(|c| c.sample_rate().0),
                default_channels: cfg.as_ref().map(|c| c.channels()),
            });
        }
    }

    Ok(infos)
}
