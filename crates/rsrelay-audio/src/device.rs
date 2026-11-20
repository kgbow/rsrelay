use cpal::traits::{DeviceTrait, HostTrait};
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::windows_virtual_mic::{is_virtual_mic_driver_available, VIRTUAL_MIC_OUTPUT_DEVICE_NAME};

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
    /// Generate a synthetic phase-continuous 440Hz/660Hz stereo sine wave (for zero-dependency E2E testing).
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

/// Check whether an input device name belongs to a virtual microphone / loopback cable
/// rather than a physical hardware microphone.
///
/// Used when auto-selecting a physical microphone (`CaptureSource::Microphone` with `None` filter)
/// so `rsrelay` does not accidentally capture from `Virtual Mic` / `CABLE Output` / `rsrelay Virtual Mic`
/// when the user has set the virtual mic as their OS default recording device.
pub fn is_virtual_mic_input_device_name(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    [
        "virtual mic",
        "rsrelay virtual mic",
        "cable output",
        "vb-audio",
        "blackhole",
        "soundflower",
        "steam streaming microphone",
        "voicemeeter out",
        "virtual audio",
    ]
    .iter()
    .any(|kw| lower.contains(kw))
}

/// Check whether an output device name is a virtual microphone bridge sink rather than a physical speaker.
pub fn is_virtual_mic_output_sink_name(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    [
        "rsrelay virtual mic sink",
        "virtual mic",
        "cable input",
        "vb-audio",
        "blackhole",
        "soundflower",
        "voicemeeter input",
        "echo-free guard",
    ]
    .iter()
    .any(|kw| lower.contains(kw))
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

    // When the Windows Virtual Mic kernel driver (`\\.\AudioRelayVirtualMicDeviceLink`) is active,
    // expose it as an output destination so RX streams and Duplex Mic RX can write directly
    // into the virtual microphone capture endpoint without playing out to speakers.
    if is_virtual_mic_driver_available()
        && !infos.iter().any(|d| {
            d.is_output
                && d.name.to_ascii_lowercase().contains("virtual mic")
                && !d.name.to_ascii_lowercase().contains("rsrelay virtual mic")
        })
    {
        infos.push(AudioDeviceInfo {
            name: VIRTUAL_MIC_OUTPUT_DEVICE_NAME.to_string(),
            is_input: false,
            is_output: true,
            is_default_input: false,
            is_default_output: false,
            default_sample_rate: Some(48_000),
            default_channels: Some(2),
        });
    }

    Ok(infos)
}

/// Select the preferred virtual microphone output sink from an already-enumerated slice of devices.
///
/// Performs pure in-memory matching with zero OS audio host queries or kernel handle opens,
/// making it safe to call inside 60 FPS GUI render loops.
///
/// Priority order:
/// 1. `rsrelay Virtual Mic Sink` (dedicated macOS CoreAudio HAL split bridge — hardware-clocked, zero speaker hijack)
/// 2. `CABLE Input` / `VB-Audio` (hardware-clocked WASAPI/CoreAudio virtual cable — zero `audiorelay-backend` contention)
/// 3. `Virtual Mic (Virtual Mic for AudioRelay)` (high-resolution Windows kernel bridge)
/// 4. `BlackHole` / `Soundflower` / `VoiceMeeter Input` / `Virtual Audio`
pub fn select_virtual_mic_output_from_devices(devices: &[AudioDeviceInfo]) -> Option<String> {
    // 1. Dedicated rsrelay HAL sink or hardware-clocked VB-CABLE first
    for keyword in ["rsrelay virtual mic sink", "cable input", "vb-audio"] {
        if let Some(dev) = devices
            .iter()
            .find(|d| d.is_output && d.name.to_ascii_lowercase().contains(keyword))
        {
            return Some(dev.name.clone());
        }
    }

    // 2. Native Windows Virtual Mic kernel bridge (appended by `list_audio_devices` when active)
    if let Some(dev) = devices
        .iter()
        .find(|d| d.is_output && d.name == VIRTUAL_MIC_OUTPUT_DEVICE_NAME)
    {
        return Some(dev.name.clone());
    }

    // 3. Other virtual loopback cables (BlackHole, Soundflower, VoiceMeeter, Virtual Audio Driver)
    for keyword in [
        "virtual mic",
        "blackhole",
        "soundflower",
        "voicemeeter input",
        "virtual audio",
    ] {
        if let Some(dev) = devices
            .iter()
            .find(|d| d.is_output && d.name.to_ascii_lowercase().contains(keyword))
        {
            return Some(dev.name.clone());
        }
    }

    None
}

/// Automatically select the preferred virtual microphone output sink across Windows and macOS.
///
/// Enumerates host audio devices once and delegates to [`select_virtual_mic_output_from_devices`].
pub fn select_default_virtual_mic_output_device() -> Option<String> {
    let devices = list_audio_devices().ok()?;
    select_virtual_mic_output_from_devices(&devices)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_select_virtual_mic_output_from_devices_priority() {
        let devices = vec![
            AudioDeviceInfo {
                name: "Speakers (Realtek Audio)".to_string(),
                is_input: false,
                is_output: true,
                is_default_input: false,
                is_default_output: true,
                default_sample_rate: Some(48_000),
                default_channels: Some(2),
            },
            AudioDeviceInfo {
                name: VIRTUAL_MIC_OUTPUT_DEVICE_NAME.to_string(),
                is_input: false,
                is_output: true,
                is_default_input: false,
                is_default_output: false,
                default_sample_rate: Some(48_000),
                default_channels: Some(2),
            },
            AudioDeviceInfo {
                name: "CABLE Input (VB-Audio Virtual Cable)".to_string(),
                is_input: false,
                is_output: true,
                is_default_input: false,
                is_default_output: false,
                default_sample_rate: Some(48_000),
                default_channels: Some(2),
            },
        ];

        assert_eq!(
            select_virtual_mic_output_from_devices(&devices).as_deref(),
            Some("CABLE Input (VB-Audio Virtual Cable)")
        );
        assert_eq!(
            select_virtual_mic_output_from_devices(&devices[..2]).as_deref(),
            Some(VIRTUAL_MIC_OUTPUT_DEVICE_NAME)
        );
        assert_eq!(select_virtual_mic_output_from_devices(&devices[..1]), None);
    }
}
