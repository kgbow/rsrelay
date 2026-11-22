#[cfg(not(any(target_os = "macos", target_os = "windows")))]
use cpal::traits::{DeviceTrait, HostTrait};
use serde::{Deserialize, Serialize};
use thiserror::Error;

#[cfg(not(any(target_os = "macos", target_os = "windows")))]
use crate::windows_virtual_mic::is_virtual_mic_driver_available;
use crate::windows_virtual_mic::VIRTUAL_MIC_OUTPUT_DEVICE_NAME;

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
    #[error("WASAPI audio error ({device}): {detail}")]
    Wasapi { device: String, detail: String },
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
///
/// - **Windows**: Queries `IMMDeviceEnumerator` + MMDevice Property Store (`PKEY_AudioEngine_DeviceFormat`)
///   directly in-process with zero `audiodg.exe` `IsFormatSupported` RPC churn and full visibility for
///   Spatial Sound / 5.1 / 7.1 / 24-bit endpoints.
/// - **macOS**: Queries CoreAudio (`AudioObjectGetPropertyData`) directly and strictly `CFRelease`s
///   every `CFStringRef`, bypassing a severe `CFStringRef` memory leak and redundant `AudioUnit`
///   instantiation inside `cpal 0.15`'s macOS `Device::name()` / `supported_configs()`.
pub fn list_audio_devices() -> Result<Vec<AudioDeviceInfo>, AudioError> {
    #[cfg(target_os = "windows")]
    {
        crate::windows_wasapi::list_audio_devices_native()
    }

    #[cfg(target_os = "macos")]
    {
        macos_coreaudio::list_audio_devices_native()
    }

    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    {
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
}

#[cfg(target_os = "macos")]
pub(crate) mod macos_coreaudio {
    use std::ffi::c_void;
    use std::ptr::null;

    use cpal::traits::HostTrait;

    use super::{AudioDeviceInfo, AudioError};

    type AudioObjectID = u32;
    type AudioDeviceID = u32;
    type OSStatus = i32;
    type CFTypeRef = *const c_void;
    type CFStringRef = *const c_void;
    type CFIndex = isize;

    const SYSTEM_OBJECT: AudioObjectID = 1;
    const SCOPE_GLOBAL: u32 = u32::from_be_bytes(*b"glob");
    const SCOPE_INPUT: u32 = u32::from_be_bytes(*b"inpt");
    const SCOPE_OUTPUT: u32 = u32::from_be_bytes(*b"outp");
    const ELEMENT_MAIN: u32 = 0;

    const PROP_DEVICES: u32 = u32::from_be_bytes(*b"dev#");
    const PROP_DEFAULT_INPUT: u32 = u32::from_be_bytes(*b"dIn ");
    const PROP_DEFAULT_OUTPUT: u32 = u32::from_be_bytes(*b"dOut");
    const PROP_OBJECT_NAME: u32 = u32::from_be_bytes(*b"lnam");
    const PROP_STREAM_CONFIG: u32 = u32::from_be_bytes(*b"slay");
    const PROP_NOMINAL_SAMPLE_RATE: u32 = u32::from_be_bytes(*b"nsrt");

    #[repr(C)]
    struct AudioObjectPropertyAddress {
        m_selector: u32,
        m_scope: u32,
        m_element: u32,
    }

    #[repr(C)]
    #[derive(Clone, Copy)]
    struct AudioBuffer {
        m_number_channels: u32,
        m_data_byte_size: u32,
        m_data: *mut c_void,
    }

    #[repr(C)]
    struct AudioBufferListHeader {
        m_number_buffers: u32,
        m_buffers: [AudioBuffer; 1],
    }

    #[repr(C)]
    #[derive(Clone, Copy)]
    struct CFRange {
        location: CFIndex,
        length: CFIndex,
    }

    extern "C" {
        fn AudioObjectGetPropertyDataSize(
            inObjectID: AudioObjectID,
            inAddress: *const AudioObjectPropertyAddress,
            inQualifierDataSize: u32,
            inQualifierData: *const c_void,
            outDataSize: *mut u32,
        ) -> OSStatus;

        fn AudioObjectGetPropertyData(
            inObjectID: AudioObjectID,
            inAddress: *const AudioObjectPropertyAddress,
            inQualifierDataSize: u32,
            inQualifierData: *const c_void,
            ioDataSize: *mut u32,
            outData: *mut c_void,
        ) -> OSStatus;

        fn CFStringGetLength(theString: CFStringRef) -> CFIndex;
        fn CFStringGetCharacters(theString: CFStringRef, range: CFRange, buffer: *mut u16);
        fn CFRelease(cf: CFTypeRef);
        fn objc_autoreleasePoolPush() -> *mut c_void;
        fn objc_autoreleasePoolPop(pool: *mut c_void);
    }

    struct AutoreleasePoolGuard(*mut c_void);

    impl AutoreleasePoolGuard {
        #[inline]
        fn new() -> Self {
            Self(unsafe { objc_autoreleasePoolPush() })
        }
    }

    impl Drop for AutoreleasePoolGuard {
        #[inline]
        fn drop(&mut self) {
            if !self.0.is_null() {
                unsafe {
                    objc_autoreleasePoolPop(self.0);
                }
            }
        }
    }

    struct CfRefGuard(CFTypeRef);

    impl Drop for CfRefGuard {
        fn drop(&mut self) {
            if !self.0.is_null() {
                unsafe {
                    CFRelease(self.0);
                }
            }
        }
    }

    fn query_device_ids() -> Option<Vec<AudioDeviceID>> {
        let addr = AudioObjectPropertyAddress {
            m_selector: PROP_DEVICES,
            m_scope: SCOPE_GLOBAL,
            m_element: ELEMENT_MAIN,
        };
        unsafe {
            let mut byte_size: u32 = 0;
            if AudioObjectGetPropertyDataSize(SYSTEM_OBJECT, &addr, 0, null(), &mut byte_size) != 0
            {
                return None;
            }
            let count = (byte_size as usize) / std::mem::size_of::<AudioDeviceID>();
            if count == 0 {
                return Some(Vec::new());
            }
            let mut ids = vec![0u32; count];
            let mut actual_bytes = (count * std::mem::size_of::<AudioDeviceID>()) as u32;
            if AudioObjectGetPropertyData(
                SYSTEM_OBJECT,
                &addr,
                0,
                null(),
                &mut actual_bytes,
                ids.as_mut_ptr().cast::<c_void>(),
            ) != 0
            {
                return None;
            }
            ids.truncate((actual_bytes as usize) / std::mem::size_of::<AudioDeviceID>());
            Some(ids)
        }
    }

    fn query_default_device_id(selector: u32) -> Option<AudioDeviceID> {
        let addr = AudioObjectPropertyAddress {
            m_selector: selector,
            m_scope: SCOPE_GLOBAL,
            m_element: ELEMENT_MAIN,
        };
        let mut dev_id: AudioDeviceID = 0;
        let mut size = std::mem::size_of::<AudioDeviceID>() as u32;
        let status = unsafe {
            AudioObjectGetPropertyData(
                SYSTEM_OBJECT,
                &addr,
                0,
                null(),
                &mut size,
                (&mut dev_id as *mut AudioDeviceID).cast::<c_void>(),
            )
        };
        if status == 0 && dev_id != 0 {
            Some(dev_id)
        } else {
            None
        }
    }

    fn query_stream_channels(dev_id: AudioDeviceID, scope: u32) -> u16 {
        let addr = AudioObjectPropertyAddress {
            m_selector: PROP_STREAM_CONFIG,
            m_scope: scope,
            m_element: ELEMENT_MAIN,
        };
        unsafe {
            let mut byte_size: u32 = 0;
            if AudioObjectGetPropertyDataSize(dev_id, &addr, 0, null(), &mut byte_size) != 0
                || (byte_size as usize) < std::mem::size_of::<u32>()
            {
                return 0;
            }
            // Allocate 8-byte aligned words so casting to `AudioBufferListHeader` is strictly aligned
            let words = ((byte_size as usize) + 7) / 8;
            let mut storage = vec![0u64; words.max(2)];
            let mut actual_bytes = (storage.len() * 8) as u32;
            if AudioObjectGetPropertyData(
                dev_id,
                &addr,
                0,
                null(),
                &mut actual_bytes,
                storage.as_mut_ptr().cast::<c_void>(),
            ) != 0
            {
                return 0;
            }
            let abl = storage.as_ptr().cast::<AudioBufferListHeader>();
            let n_bufs = (*abl).m_number_buffers as usize;
            if n_bufs == 0 {
                return 0;
            }
            let header_offset = std::mem::offset_of!(AudioBufferListHeader, m_buffers);
            let max_bufs =
                (actual_bytes as usize).saturating_sub(header_offset) / std::mem::size_of::<AudioBuffer>();
            let safe_bufs = n_bufs.min(max_bufs);
            let buf_slice = std::slice::from_raw_parts((*abl).m_buffers.as_ptr(), safe_bufs);
            let total_ch: u32 = buf_slice.iter().map(|b| b.m_number_channels).sum();
            u16::try_from(total_ch).unwrap_or(u16::MAX)
        }
    }

    fn query_nominal_sample_rate(dev_id: AudioDeviceID, scope: u32) -> Option<u32> {
        for candidate_scope in [scope, SCOPE_GLOBAL] {
            let addr = AudioObjectPropertyAddress {
                m_selector: PROP_NOMINAL_SAMPLE_RATE,
                m_scope: candidate_scope,
                m_element: ELEMENT_MAIN,
            };
            let mut rate: f64 = 0.0;
            let mut size = std::mem::size_of::<f64>() as u32;
            let status = unsafe {
                AudioObjectGetPropertyData(
                    dev_id,
                    &addr,
                    0,
                    null(),
                    &mut size,
                    (&mut rate as *mut f64).cast::<c_void>(),
                )
            };
            if status == 0 && rate > 1000.0 && rate.is_finite() {
                return Some(rate.round() as u32);
            }
        }
        None
    }

    pub(crate) fn query_device_name_leak_free(dev_id: AudioDeviceID) -> Option<String> {
        let addr = AudioObjectPropertyAddress {
            m_selector: PROP_OBJECT_NAME,
            m_scope: SCOPE_GLOBAL,
            m_element: ELEMENT_MAIN,
        };
        let mut cf_str: CFStringRef = null();
        let mut size = std::mem::size_of::<CFStringRef>() as u32;
        let status = unsafe {
            AudioObjectGetPropertyData(
                dev_id,
                &addr,
                0,
                null(),
                &mut size,
                (&mut cf_str as *mut CFStringRef).cast::<c_void>(),
            )
        };
        if status != 0 || cf_str.is_null() {
            return None;
        }
        // Guarantee `CFRelease(cf_str)` is invoked when `_guard` leaves scope!
        let _guard = CfRefGuard(cf_str);
        unsafe {
            let utf16_len = CFStringGetLength(cf_str);
            if utf16_len <= 0 {
                return Some(String::new());
            }
            let mut utf16 = vec![0u16; utf16_len as usize];
            CFStringGetCharacters(
                cf_str,
                CFRange {
                    location: 0,
                    length: utf16_len,
                },
                utf16.as_mut_ptr(),
            );
            Some(String::from_utf16_lossy(&utf16))
        }
    }

    pub(crate) fn list_audio_devices_native() -> Result<Vec<AudioDeviceInfo>, AudioError> {
        let _pool = AutoreleasePoolGuard::new();
        let ids = query_device_ids().unwrap_or_default();
        let default_in_id = query_default_device_id(PROP_DEFAULT_INPUT);
        let default_out_id = query_default_device_id(PROP_DEFAULT_OUTPUT);

        let mut infos = Vec::with_capacity(ids.len() * 2);

        // Preserve grouping: all input endpoints first, then all output endpoints
        let mut cached = Vec::with_capacity(ids.len());
        for id in ids {
            let in_ch = query_stream_channels(id, SCOPE_INPUT);
            let out_ch = query_stream_channels(id, SCOPE_OUTPUT);
            if in_ch == 0 && out_ch == 0 {
                continue;
            }
            let name = query_device_name_leak_free(id).unwrap_or_else(|| format!("AudioDevice {id}"));
            cached.push((id, name, in_ch, out_ch));
        }

        for &(id, ref name, in_ch, _) in &cached {
            if in_ch > 0 {
                infos.push(AudioDeviceInfo {
                    name: name.clone(),
                    is_input: true,
                    is_output: false,
                    is_default_input: default_in_id == Some(id),
                    is_default_output: false,
                    default_sample_rate: query_nominal_sample_rate(id, SCOPE_INPUT),
                    default_channels: Some(in_ch),
                });
            }
        }

        for &(id, ref name, _, out_ch) in &cached {
            if out_ch > 0 {
                infos.push(AudioDeviceInfo {
                    name: name.clone(),
                    is_input: false,
                    is_output: true,
                    is_default_input: false,
                    is_default_output: default_out_id == Some(id),
                    default_sample_rate: query_nominal_sample_rate(id, SCOPE_OUTPUT),
                    default_channels: Some(out_ch),
                });
            }
        }

        Ok(infos)
    }

    /// Resolve the OS default input or output `cpal::Device` together with its leak-free CoreAudio name,
    /// without ever invoking `cpal::Device::name()` (which leaks `CFStringRef` on macOS).
    pub(crate) fn default_cpal_device_with_name(
        host: &cpal::Host,
        is_input: bool,
    ) -> Option<(cpal::Device, String)> {
        let _pool = AutoreleasePoolGuard::new();
        let default_id = query_default_device_id(if is_input {
            PROP_DEFAULT_INPUT
        } else {
            PROP_DEFAULT_OUTPUT
        })?;
        let name = query_device_name_leak_free(default_id)
            .unwrap_or_else(|| "Default Device".to_string());
        let dev = if is_input {
            host.default_input_device()?
        } else {
            host.default_output_device()?
        };
        Some((dev, name))
    }

    /// Find the first `cpal::Device` matching `predicate(device_name)` in the requested direction
    /// without invoking `cpal::Device::name()` (zero `CFStringRef` leaks and zero `AudioUnit` probes).
    pub(crate) fn find_cpal_device_by_name(
        host: &cpal::Host,
        is_input: bool,
        predicate: impl Fn(&str) -> bool,
    ) -> Option<(cpal::Device, String)> {
        let _pool = AutoreleasePoolGuard::new();
        let ids = query_device_ids()?;
        let cpal_devs: Vec<cpal::Device> = host.devices().ok()?.collect();
        if ids.len() != cpal_devs.len() {
            return None;
        }
        let scope = if is_input { SCOPE_INPUT } else { SCOPE_OUTPUT };
        for (id, dev) in ids.into_iter().zip(cpal_devs) {
            if query_stream_channels(id, scope) == 0 {
                continue;
            }
            if let Some(name) = query_device_name_leak_free(id) {
                if predicate(&name) {
                    return Some((dev, name));
                }
            }
        }
        None
    }
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

    #[cfg(target_os = "macos")]
    #[test]
    fn test_macos_list_audio_devices_zero_cfstring_leak() {
        #[repr(C)]
        #[derive(Default)]
        struct MallocStatistics {
            blocks_in_use: u32,
            size_in_use: usize,
            max_size_in_use: usize,
            size_allocated: usize,
        }

        extern "C" {
            fn malloc_default_zone() -> *mut std::ffi::c_void;
            fn malloc_zone_statistics(zone: *mut std::ffi::c_void, stats: *mut MallocStatistics);
        }

        fn current_heap_blocks_and_bytes() -> (u32, usize) {
            unsafe {
                let mut stats = MallocStatistics::default();
                malloc_zone_statistics(malloc_default_zone(), &mut stats);
                (stats.blocks_in_use, stats.size_in_use)
            }
        }

        // Warm up xzone per-CPU magazine slabs first (2,000 iterations)
        let first = list_audio_devices().expect("list macOS audio devices");
        assert!(
            !first.is_empty(),
            "macOS host should report at least one audio device"
        );
        for _ in 0..2_000 {
            std::hint::black_box(list_audio_devices().expect("warmup"));
        }

        let (_blocks_before, bytes_before) = current_heap_blocks_and_bytes();
        for _ in 0..6_000 {
            let devs = list_audio_devices().expect("list macOS audio devices");
            std::hint::black_box(devs);
        }
        let (_blocks_after, bytes_after) = current_heap_blocks_and_bytes();

        let leaked_bytes = bytes_after.saturating_sub(bytes_before);
        // 6,000 iterations * ~5 devices = 30,000 queries. With cpal's CFString leak, this leaked
        // >1.2 MB. Allow up to 192 KB for macOS xzone per-CPU 16KB slab initialization on core migration.
        assert!(
            leaked_bytes < 192 * 1024,
            "list_audio_devices() must not leak CFStrings on macOS (got +{leaked_bytes} bytes across 6,000 post-warmup iterations)"
        );
    }
}
