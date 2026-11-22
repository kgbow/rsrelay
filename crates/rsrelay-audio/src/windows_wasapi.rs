//! Native Windows WASAPI device enumeration, `GetMixFormat()` capture (Loopback & Microphone),
//! and shared-mode playback streams.
//!
//! ### Why native WASAPI is used directly on Windows instead of `cpal 0.15`:
//! 1. **Zero `audiodg.exe` RPC/Handle Churn during Device Enumeration**:
//!    `cpal 0.15`'s `Host::input_devices()` / `output_devices()` activates `IAudioClient` and invokes
//!    ~60 `IsFormatSupported()` RPCs per endpoint on every refresh, triggering handle leaks inside
//!    `audiodg.exe` with certain vendor APOs (e.g. AMD HD Audio) and filtering out Spatial Sound /
//!    24-bit endpoints when `dwChannelMask = 0` returns `S_FALSE`.
//!    [`list_audio_devices_native`] reads `DEVPKEY_Device_FriendlyName` and `PKEY_AudioEngine_DeviceFormat`
//!    straight from the MMDevice Property Store in-process with **zero `audiodg.exe` RPCs**.
//! 2. **Exact `GetMixFormat()` Stream Initialization**:
//!    `cpal 0.15` discards `dwChannelMask` from `GetMixFormat()` and substitutes `KSAUDIO_SPEAKER_DIRECTOUT (0)`,
//!    which fails on Spatial Sound (Windows Sonic / Dolby Atmos), 5.1/7.1 virtual surround, and 24-bit
//!    packed PCM endpoints, and masks `HRESULT` errors (such as `0x800705AA` `ERROR_NO_SYSTEM_RESOURCES`)
//!    as `StreamConfigNotSupported`. This module initializes `IAudioClient` directly with the unmodified
//!    `WAVEFORMATEX*` from `GetMixFormat()`.
//! 3. **Continuous Zero-Copy Loopback Silent Keepalive**:
//!    Maintains a 100ms silent shared-mode render buffer topped up via `AUDCLNT_BUFFERFLAGS_SILENT`
//!    whenever padding drops below 50%, preventing WASAPI from pausing the Loopback capture clock
//!    during desktop silence without spawning an extra thread.

#![cfg(target_os = "windows")]

use std::ffi::OsString;
use std::os::windows::ffi::OsStringExt;
use std::ptr;
use std::slice;
use std::sync::{
    atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering},
    mpsc, Arc,
};
use std::thread::{self, JoinHandle};

use rtrb::{Consumer, Producer};
use windows::core::{GUID, HRESULT, PWSTR};
use windows::Win32::Devices::Properties;
use windows::Win32::Foundation::{
    CloseHandle, HANDLE, RPC_E_CHANGED_MODE, WAIT_OBJECT_0, WAIT_TIMEOUT,
};
use windows::Win32::Media::{
    Audio::{
        self, IAudioCaptureClient, IAudioClient, IAudioRenderClient, IMMDevice,
        IMMDeviceEnumerator, MMDeviceEnumerator, PKEY_AudioEngine_DeviceFormat,
        PKEY_AudioEngine_OEMFormat, AUDCLNT_BUFFERFLAGS_SILENT, AUDCLNT_SHAREMODE_SHARED,
        AUDCLNT_STREAMFLAGS_EVENTCALLBACK, AUDCLNT_STREAMFLAGS_LOOPBACK, AUDCLNT_S_BUFFER_EMPTY,
        DEVICE_STATE_ACTIVE, WAVEFORMATEX, WAVEFORMATEXTENSIBLE, WAVE_FORMAT_PCM,
    },
    KernelStreaming::{KSDATAFORMAT_SUBTYPE_PCM, WAVE_FORMAT_EXTENSIBLE},
    Multimedia::{KSDATAFORMAT_SUBTYPE_IEEE_FLOAT, WAVE_FORMAT_IEEE_FLOAT},
};
use windows::Win32::System::Com::{
    CoCreateInstance, CoInitializeEx, CoTaskMemFree, CoUninitialize, CLSCTX_ALL,
    COINIT_MULTITHREADED, STGM_READ,
};
use windows::Win32::System::Threading::{
    CreateEventW, SetEvent, WaitForMultipleObjects, WaitForSingleObject,
};
use windows::Win32::System::Variant::{VT_BLOB, VT_LPWSTR};
use windows::Win32::UI::Shell::PropertiesSystem::{IPropertyStore, PROPERTYKEY};

use crate::device::{is_virtual_mic_input_device_name, AudioDeviceInfo, AudioError, CaptureSource};
use crate::windows_virtual_mic::{is_virtual_mic_driver_available, VIRTUAL_MIC_OUTPUT_DEVICE_NAME};

const HRESULT_ELEMENT_NOT_FOUND: HRESULT = HRESULT(0x80070490u32 as i32);
const HRESULT_NO_SYSTEM_RESOURCES: HRESULT = HRESULT(0x800705AAu32 as i32);
const HRESULT_RPC_SERVER_UNAVAILABLE: HRESULT = HRESULT(0x800706BAu32 as i32);
const AUDCLNT_E_SERVICE_NOT_RUNNING: HRESULT = HRESULT(0x8889000Fu32 as i32);

enum WasapiInitError {
    DeviceNotFound(String),
    Wasapi { device: String, detail: String },
}

impl From<WasapiInitError> for AudioError {
    fn from(err: WasapiInitError) -> Self {
        match err {
            WasapiInitError::DeviceNotFound(d) => AudioError::DeviceNotFound(d),
            WasapiInitError::Wasapi { device, detail } => AudioError::Wasapi { device, detail },
        }
    }
}

fn format_wasapi_err(context: &str, err: &windows::core::Error) -> String {
    let code = err.code();
    if code == HRESULT_NO_SYSTEM_RESOURCES {
        format!(
            "{context}: {err} (audiodg.exe handle limit reached — run `Restart-Service Audiosrv -Force` then `Start-Service Audiosrv` as Administrator)"
        )
    } else if code == AUDCLNT_E_SERVICE_NOT_RUNNING || code == HRESULT_RPC_SERVER_UNAVAILABLE {
        format!(
            "{context}: {err} (Windows Audio service `Audiosrv` is not running — run `Start-Service Audiosrv` as Administrator)"
        )
    } else {
        format!("{context}: {err}")
    }
}

/// RAII guard for COM initialization on the calling thread.
struct ComGuard {
    should_uninit: bool,
}

impl ComGuard {
    fn init_mta() -> Result<Self, String> {
        let hr = unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) };
        if hr.is_ok() {
            Ok(Self {
                should_uninit: true,
            })
        } else if hr == RPC_E_CHANGED_MODE {
            Ok(Self {
                should_uninit: false,
            })
        } else {
            Err(format!("CoInitializeEx failed: {hr:?}"))
        }
    }
}

impl Drop for ComGuard {
    fn drop(&mut self) {
        if self.should_uninit {
            unsafe {
                CoUninitialize();
            }
        }
    }
}

/// RAII guard for a `CoTaskMemAlloc`-allocated `PWSTR` (e.g. `IMMDevice::GetId`).
struct CoTaskPwstr(PWSTR);

impl Drop for CoTaskPwstr {
    fn drop(&mut self) {
        if !self.0.is_null() {
            unsafe {
                CoTaskMemFree(Some(self.0 .0.cast()));
            }
        }
    }
}

/// RAII guard for a Win32 `HANDLE` (events, etc.).
struct EventHandle(HANDLE);
unsafe impl Send for EventHandle {}
unsafe impl Sync for EventHandle {}

impl EventHandle {
    fn new_auto_reset() -> Result<Self, String> {
        let h = unsafe { CreateEventW(None, false, false, None) }
            .map_err(|e| format!("CreateEventW failed: {e}"))?;
        Ok(Self(h))
    }

    fn new_manual_reset() -> Result<Self, String> {
        let h = unsafe { CreateEventW(None, true, false, None) }
            .map_err(|e| format!("CreateEventW failed: {e}"))?;
        Ok(Self(h))
    }

    #[inline]
    fn raw(&self) -> HANDLE {
        self.0
    }

    #[inline]
    fn signal(&self) {
        unsafe {
            let _ = SetEvent(self.0);
        }
    }
}

impl Drop for EventHandle {
    fn drop(&mut self) {
        if !self.0.is_invalid() {
            unsafe {
                let _ = CloseHandle(self.0);
            }
        }
    }
}

/// RAII guard for `WAVEFORMATEX*` allocated by `IAudioClient::GetMixFormat`.
struct MixFormatPtr(*mut WAVEFORMATEX);

impl Drop for MixFormatPtr {
    fn drop(&mut self) {
        if !self.0.is_null() {
            unsafe {
                CoTaskMemFree(Some(self.0.cast()));
            }
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum NativeSampleFormat {
    F32,
    I16,
    I24Packed,
    I32,
    U8,
}

#[derive(Debug, Clone, Copy)]
struct ParsedMixFormat {
    sample_rate: u32,
    channels: u16,
    block_align: usize,
    sample_format: NativeSampleFormat,
}

fn cmp_guid(a: &GUID, b: &GUID) -> bool {
    (a.data1, a.data2, a.data3, a.data4) == (b.data1, b.data2, b.data3, b.data4)
}

unsafe fn parse_mix_format(ptr: *const WAVEFORMATEX) -> Result<ParsedMixFormat, String> {
    let wf = &*ptr;
    let channels = wf.nChannels;
    let sample_rate = wf.nSamplesPerSec;
    let bits = wf.wBitsPerSample;
    let block_align = wf.nBlockAlign as usize;
    let tag = wf.wFormatTag as u32;

    if channels == 0 || sample_rate == 0 || block_align == 0 {
        return Err(format!(
            "invalid mix format: ch={channels}, rate={sample_rate}, align={block_align}"
        ));
    }

    let sample_format = match (tag, bits) {
        (t, 32) if t == WAVE_FORMAT_IEEE_FLOAT => NativeSampleFormat::F32,
        (t, 16) if t == WAVE_FORMAT_PCM => NativeSampleFormat::I16,
        (t, 24) if t == WAVE_FORMAT_PCM => NativeSampleFormat::I24Packed,
        (t, 32) if t == WAVE_FORMAT_PCM => NativeSampleFormat::I32,
        (t, 8) if t == WAVE_FORMAT_PCM => NativeSampleFormat::U8,
        (t, _) if t == WAVE_FORMAT_EXTENSIBLE => {
            let ext = &*(ptr as *const WAVEFORMATEXTENSIBLE);
            let sub = ext.SubFormat;
            if bits == 32 && cmp_guid(&sub, &KSDATAFORMAT_SUBTYPE_IEEE_FLOAT) {
                NativeSampleFormat::F32
            } else if cmp_guid(&sub, &KSDATAFORMAT_SUBTYPE_PCM) {
                match bits {
                    8 => NativeSampleFormat::U8,
                    16 => NativeSampleFormat::I16,
                    24 => NativeSampleFormat::I24Packed,
                    32 => NativeSampleFormat::I32,
                    other => {
                        return Err(format!("unsupported PCM extensible bit depth: {other}"));
                    }
                }
            } else {
                return Err(format!(
                    "unsupported WAVEFORMATEXTENSIBLE SubFormat: {sub:?} ({bits}-bit)"
                ));
            }
        }
        (other_tag, other_bits) => {
            return Err(format!(
                "unsupported WASAPI format tag={other_tag:#x}, bits={other_bits}"
            ));
        }
    };

    Ok(ParsedMixFormat {
        sample_rate,
        channels,
        block_align,
        sample_format,
    })
}

unsafe fn read_device_id(device: &IMMDevice) -> Option<String> {
    let raw = device.GetId().ok()?;
    if raw.is_null() {
        return None;
    }
    let guard = CoTaskPwstr(raw);
    let mut len = 0usize;
    while *guard.0 .0.add(len) != 0 {
        len += 1;
    }
    let slice_u16 = slice::from_raw_parts(guard.0 .0, len);
    Some(OsString::from_wide(slice_u16).to_string_lossy().into_owned())
}

unsafe fn read_friendly_name_from_store(store: &IPropertyStore, fallback: &str) -> String {
    let Ok(prop) = store.GetValue(&Properties::DEVPKEY_Device_FriendlyName as *const _ as *const _)
    else {
        return fallback.to_string();
    };
    let raw = &prop.as_raw().Anonymous.Anonymous;
    if raw.vt == VT_LPWSTR.0 {
        let ptr_u16 = raw.Anonymous.pwszVal;
        if !ptr_u16.is_null() {
            let mut len = 0usize;
            while *ptr_u16.add(len) != 0 {
                len += 1;
            }
            let slice_u16 = slice::from_raw_parts(ptr_u16, len);
            return OsString::from_wide(slice_u16)
                .to_string_lossy()
                .into_owned();
        }
    }
    fallback.to_string()
}

unsafe fn read_device_friendly_name(device: &IMMDevice) -> String {
    let Ok(store) = device.OpenPropertyStore(STGM_READ) else {
        return "Windows Audio Device".to_string();
    };
    read_friendly_name_from_store(&store, "Windows Audio Device")
}

unsafe fn extract_rate_channels_from_prop(
    store: &IPropertyStore,
    key: &PROPERTYKEY,
) -> Option<(u32, u16)> {
    let prop = store.GetValue(key).ok()?;
    let raw = &prop.as_raw().Anonymous.Anonymous;
    if raw.vt != VT_BLOB.0 {
        return None;
    }
    let blob = raw.Anonymous.blob;
    if blob.pBlobData.is_null() || (blob.cbSize as usize) < std::mem::size_of::<WAVEFORMATEX>() {
        return None;
    }
    let wf = ptr::read_unaligned(blob.pBlobData as *const WAVEFORMATEX);
    if wf.nSamplesPerSec > 0 && wf.nChannels > 0 {
        Some((wf.nSamplesPerSec, wf.nChannels))
    } else {
        None
    }
}

/// Read friendly name, nominal sample rate, and channel count purely from the MMDevice Property Store
/// (`PKEY_AudioEngine_DeviceFormat` / `PKEY_AudioEngine_OEMFormat`) without activating `IAudioClient`
/// or making RPC calls to `audiodg.exe`. Only falls back to `GetMixFormat()` if both property keys
/// are absent on a custom kernel-streaming filter.
unsafe fn read_device_info_zero_churn(
    device: &IMMDevice,
    fallback_name: &str,
) -> (String, Option<u32>, Option<u16>) {
    if let Ok(store) = device.OpenPropertyStore(STGM_READ) {
        let name = read_friendly_name_from_store(&store, fallback_name);
        if let Some((rate, ch)) =
            extract_rate_channels_from_prop(&store, &PKEY_AudioEngine_DeviceFormat)
                .or_else(|| extract_rate_channels_from_prop(&store, &PKEY_AudioEngine_OEMFormat))
        {
            return (name, Some(rate), Some(ch));
        }

        // Rare fallback for bare KS virtual endpoints without PropertyStore format blobs
        if let Ok(client) = device.Activate::<IAudioClient>(CLSCTX_ALL, None) {
            if let Ok(mix_ptr) = client.GetMixFormat() {
                let mix = MixFormatPtr(mix_ptr);
                let wf = &*mix.0;
                if wf.nSamplesPerSec > 0 && wf.nChannels > 0 {
                    return (name, Some(wf.nSamplesPerSec), Some(wf.nChannels));
                }
            }
        }
        return (name, None, None);
    }

    (fallback_name.to_string(), None, None)
}

/// Enumerate all active Windows input and output audio endpoints with zero `IsFormatSupported`
/// probing and zero `audiodg.exe` handle churn.
pub fn list_audio_devices_native() -> Result<Vec<AudioDeviceInfo>, AudioError> {
    let _com = ComGuard::init_mta().map_err(|detail| AudioError::Wasapi {
        device: "MMDeviceEnumerator".to_string(),
        detail,
    })?;

    unsafe {
        let enumerator: IMMDeviceEnumerator =
            CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL).map_err(|e| {
                AudioError::Wasapi {
                    device: "MMDeviceEnumerator".to_string(),
                    detail: format!("CoCreateInstance failed: {e}"),
                }
            })?;

        let default_in_id = enumerator
            .GetDefaultAudioEndpoint(Audio::eCapture, Audio::eConsole)
            .ok()
            .and_then(|d| read_device_id(&d));
        let default_out_id = enumerator
            .GetDefaultAudioEndpoint(Audio::eRender, Audio::eConsole)
            .ok()
            .and_then(|d| read_device_id(&d));

        let mut infos = Vec::new();

        if let Ok(col_in) = enumerator.EnumAudioEndpoints(Audio::eCapture, DEVICE_STATE_ACTIVE) {
            let count = col_in.GetCount().unwrap_or(0);
            for idx in 0..count {
                if let Ok(dev) = col_in.Item(idx) {
                    let dev_id = read_device_id(&dev);
                    let (name, rate, ch) = read_device_info_zero_churn(&dev, "Unknown Input");
                    infos.push(AudioDeviceInfo {
                        is_default_input: dev_id.is_some() && dev_id == default_in_id,
                        is_default_output: false,
                        name,
                        is_input: true,
                        is_output: false,
                        default_sample_rate: rate,
                        default_channels: ch,
                    });
                }
            }
        }

        if let Ok(col_out) = enumerator.EnumAudioEndpoints(Audio::eRender, DEVICE_STATE_ACTIVE) {
            let count = col_out.GetCount().unwrap_or(0);
            for idx in 0..count {
                if let Ok(dev) = col_out.Item(idx) {
                    let dev_id = read_device_id(&dev);
                    let (name, rate, ch) = read_device_info_zero_churn(&dev, "Unknown Output");
                    infos.push(AudioDeviceInfo {
                        is_default_input: false,
                        is_default_output: dev_id.is_some() && dev_id == default_out_id,
                        name,
                        is_input: false,
                        is_output: true,
                        default_sample_rate: rate,
                        default_channels: ch,
                    });
                }
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

unsafe fn select_imm_device(
    data_flow: Audio::EDataFlow,
    name_filter: Option<&str>,
    avoid_virtual_mic_inputs: bool,
) -> Result<(IMMDevice, String), WasapiInitError> {
    let enumerator: IMMDeviceEnumerator = CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL)
        .map_err(|e| WasapiInitError::Wasapi {
        device: "MMDeviceEnumerator".to_string(),
        detail: format!("CoCreateInstance(MMDeviceEnumerator) failed: {e}"),
    })?;

    if let Some(filter) = name_filter {
        let filter_lower = filter.to_lowercase();
        let collection = enumerator
            .EnumAudioEndpoints(data_flow, DEVICE_STATE_ACTIVE)
            .map_err(|e| WasapiInitError::Wasapi {
                device: filter.to_string(),
                detail: format!("EnumAudioEndpoints failed: {e}"),
            })?;
        let count = collection
            .GetCount()
            .map_err(|e| WasapiInitError::Wasapi {
                device: filter.to_string(),
                detail: format!("IMMDeviceCollection::GetCount failed: {e}"),
            })?;
        for idx in 0..count {
            if let Ok(dev) = collection.Item(idx) {
                let name = read_device_friendly_name(&dev);
                if name.to_lowercase().contains(&filter_lower) {
                    return Ok((dev, name));
                }
            }
        }
        return Err(WasapiInitError::DeviceNotFound(filter.to_string()));
    }

    let fallback_label = if data_flow == Audio::eRender {
        "default Windows output device"
    } else {
        "default microphone input"
    };

    let default_dev = enumerator
        .GetDefaultAudioEndpoint(data_flow, Audio::eConsole)
        .map_err(|e| {
            if e.code() == HRESULT_ELEMENT_NOT_FOUND {
                WasapiInitError::DeviceNotFound(fallback_label.to_string())
            } else {
                WasapiInitError::Wasapi {
                    device: fallback_label.to_string(),
                    detail: format_wasapi_err("GetDefaultAudioEndpoint failed", &e),
                }
            }
        })?;
    let default_name = read_device_friendly_name(&default_dev);

    if avoid_virtual_mic_inputs && is_virtual_mic_input_device_name(&default_name) {
        if let Ok(collection) = enumerator.EnumAudioEndpoints(data_flow, DEVICE_STATE_ACTIVE) {
            if let Ok(count) = collection.GetCount() {
                for idx in 0..count {
                    if let Ok(dev) = collection.Item(idx) {
                        let name = read_device_friendly_name(&dev);
                        if !is_virtual_mic_input_device_name(&name) {
                            return Ok((dev, name));
                        }
                    }
                }
            }
        }
    }

    Ok((default_dev, default_name))
}

#[inline]
fn decode_sample_at(bytes: &[u8], offset: usize, fmt: NativeSampleFormat) -> f32 {
    match fmt {
        NativeSampleFormat::F32 => {
            if offset + 4 <= bytes.len() {
                f32::from_le_bytes([
                    bytes[offset],
                    bytes[offset + 1],
                    bytes[offset + 2],
                    bytes[offset + 3],
                ])
            } else {
                0.0
            }
        }
        NativeSampleFormat::I16 => {
            if offset + 2 <= bytes.len() {
                (i16::from_le_bytes([bytes[offset], bytes[offset + 1]]) as f32) / 32768.0
            } else {
                0.0
            }
        }
        NativeSampleFormat::I24Packed => {
            if offset + 3 <= bytes.len() {
                let b0 = bytes[offset] as i32;
                let b1 = (bytes[offset + 1] as i32) << 8;
                let b2 = (bytes[offset + 2] as i8 as i32) << 16;
                ((b0 | b1 | b2) as f32) / 8_388_608.0
            } else {
                0.0
            }
        }
        NativeSampleFormat::I32 => {
            if offset + 4 <= bytes.len() {
                let v = i32::from_le_bytes([
                    bytes[offset],
                    bytes[offset + 1],
                    bytes[offset + 2],
                    bytes[offset + 3],
                ]);
                (v as f32) / 2_147_483_648.0
            } else {
                0.0
            }
        }
        NativeSampleFormat::U8 => {
            if offset < bytes.len() {
                ((bytes[offset] as f32) - 128.0) / 128.0
            } else {
                0.0
            }
        }
    }
}

#[inline]
fn encode_sample_at(bytes: &mut [u8], offset: usize, sample: f32, fmt: NativeSampleFormat) {
    let clamped = sample.clamp(-1.0, 1.0);
    match fmt {
        NativeSampleFormat::F32 => {
            if offset + 4 <= bytes.len() {
                bytes[offset..offset + 4].copy_from_slice(&clamped.to_le_bytes());
            }
        }
        NativeSampleFormat::I16 => {
            if offset + 2 <= bytes.len() {
                let v = (clamped * 32767.0) as i16;
                bytes[offset..offset + 2].copy_from_slice(&v.to_le_bytes());
            }
        }
        NativeSampleFormat::I24Packed => {
            if offset + 3 <= bytes.len() {
                let v = (clamped * 8_388_607.0) as i32;
                bytes[offset] = (v & 0xFF) as u8;
                bytes[offset + 1] = ((v >> 8) & 0xFF) as u8;
                bytes[offset + 2] = ((v >> 16) & 0xFF) as u8;
            }
        }
        NativeSampleFormat::I32 => {
            if offset + 4 <= bytes.len() {
                let v = (clamped * 2_147_483_520.0) as i32;
                bytes[offset..offset + 4].copy_from_slice(&v.to_le_bytes());
            }
        }
        NativeSampleFormat::U8 => {
            if offset < bytes.len() {
                bytes[offset] = ((clamped * 127.0) + 128.0).clamp(0.0, 255.0) as u8;
            }
        }
    }
}

#[inline]
fn bytes_per_sample(fmt: NativeSampleFormat) -> usize {
    match fmt {
        NativeSampleFormat::F32 | NativeSampleFormat::I32 => 4,
        NativeSampleFormat::I24Packed => 3,
        NativeSampleFormat::I16 => 2,
        NativeSampleFormat::U8 => 1,
    }
}

struct NativeKeepaliveStream {
    client: IAudioClient,
    render: IAudioRenderClient,
    buffer_frames: u32,
}

impl NativeKeepaliveStream {
    /// Top up the 100ms shared-mode render FIFO with `AUDCLNT_BUFFERFLAGS_SILENT` whenever
    /// less than half of the buffer remains queued, keeping the WASAPI render clock continuously
    /// running during desktop silence with zero memory writes and zero extra threads.
    #[inline]
    unsafe fn top_up_silence(&self) {
        if let Ok(padding) = self.client.GetCurrentPadding() {
            if padding < self.buffer_frames / 2 {
                let avail = self.buffer_frames - padding;
                if let Ok(_buf) = self.render.GetBuffer(avail) {
                    let _ = self
                        .render
                        .ReleaseBuffer(avail, AUDCLNT_BUFFERFLAGS_SILENT.0 as u32);
                }
            }
        }
    }
}

impl Drop for NativeKeepaliveStream {
    fn drop(&mut self) {
        unsafe {
            let _ = self.client.Stop();
        }
    }
}

/// Start an optional silent render client on the exact same `IMMDevice` using its native
/// `GetMixFormat()`, keeping WASAPI Loopback callbacks alive even when the desktop is silent.
unsafe fn start_native_silent_keepalive(
    device: &IMMDevice,
    mix_fmt_ptr: *const WAVEFORMATEX,
) -> Option<NativeKeepaliveStream> {
    let client: IAudioClient = device.Activate(CLSCTX_ALL, None).ok()?;
    // 100 ms shared-mode buffer (1,000,000 * 100ns); topped up inline in `run_capture_loop`
    client
        .Initialize(AUDCLNT_SHAREMODE_SHARED, 0, 1_000_000, 0, mix_fmt_ptr, None)
        .ok()?;
    let buffer_frames = client.GetBufferSize().ok()?.max(1);
    let render: IAudioRenderClient = client.GetService().ok()?;
    if let Ok(_buf) = render.GetBuffer(buffer_frames) {
        let _ = render.ReleaseBuffer(buffer_frames, AUDCLNT_BUFFERFLAGS_SILENT.0 as u32);
    }
    client.Start().ok()?;
    Some(NativeKeepaliveStream {
        client,
        render,
        buffer_frames,
    })
}

/// Active native Windows WASAPI capture stream (Loopback or Microphone).
pub struct WasapiCaptureStream {
    stop_flag: Arc<AtomicBool>,
    stop_event: Arc<EventHandle>,
    worker: Option<JoinHandle<()>>,
    pub device_name: String,
    pub sample_rate: u32,
}

impl WasapiCaptureStream {
    pub fn start(
        source: CaptureSource,
        device_name_filter: Option<&str>,
        target_channels: u16,
        producer: Producer<f32>,
        peak_bits: Arc<AtomicU32>,
    ) -> Result<Self, AudioError> {
        let is_loopback = source == CaptureSource::SystemLoopback;
        let filter_owned = device_name_filter.map(|s| s.to_string());

        let stop_flag = Arc::new(AtomicBool::new(false));
        let stop_event = Arc::new(EventHandle::new_manual_reset().map_err(|detail| {
            AudioError::Wasapi {
                device: filter_owned
                    .clone()
                    .unwrap_or_else(|| "Windows Default".to_string()),
                detail,
            }
        })?);

        let stop_flag_cl = Arc::clone(&stop_flag);
        let stop_event_cl = Arc::clone(&stop_event);
        let (init_tx, init_rx) = mpsc::sync_channel::<Result<(String, u32), WasapiInitError>>(1);

        let worker = thread::Builder::new()
            .name(if is_loopback {
                "rsrelay-wasapi-loopback".to_string()
            } else {
                "rsrelay-wasapi-mic".to_string()
            })
            .spawn(move || {
                let _com = match ComGuard::init_mta() {
                    Ok(g) => g,
                    Err(detail) => {
                        let _ = init_tx.send(Err(WasapiInitError::Wasapi {
                            device: "COM".to_string(),
                            detail,
                        }));
                        return;
                    }
                };

                let init_res = unsafe { init_capture_pipeline(is_loopback, filter_owned.as_deref()) };

                let CapturePipeline {
                    device_name,
                    parsed_fmt,
                    audio_client,
                    capture_client,
                    audio_event,
                    keepalive,
                } = match init_res {
                    Ok(v) => v,
                    Err(e) => {
                        let _ = init_tx.send(Err(e));
                        return;
                    }
                };

                let _prio_guard =
                    crate::promote_worker_thread_priority(parsed_fmt.sample_rate, 240);

                if let Err(e) = unsafe { audio_client.Start() } {
                    let _ = init_tx.send(Err(WasapiInitError::Wasapi {
                        device: device_name,
                        detail: format_wasapi_err("IAudioClient::Start failed", &e),
                    }));
                    return;
                }

                let _ = init_tx.send(Ok((device_name, parsed_fmt.sample_rate)));

                run_capture_loop(
                    is_loopback,
                    &stop_flag_cl,
                    &stop_event_cl,
                    audio_event.as_ref(),
                    keepalive.as_ref(),
                    &capture_client,
                    parsed_fmt,
                    target_channels,
                    producer,
                    &peak_bits,
                );

                unsafe {
                    let _ = audio_client.Stop();
                }
            })
            .map_err(|e| AudioError::Wasapi {
                device: "WASAPI Capture".to_string(),
                detail: format!("failed to spawn capture thread: {e}"),
            })?;

        match init_rx.recv() {
            Ok(Ok((device_name, sample_rate))) => Ok(Self {
                stop_flag,
                stop_event,
                worker: Some(worker),
                device_name,
                sample_rate,
            }),
            Ok(Err(err)) => {
                let _ = worker.join();
                Err(err.into())
            }
            Err(_) => {
                let _ = worker.join();
                Err(AudioError::Wasapi {
                    device: device_name_filter
                        .unwrap_or("Default Windows Device")
                        .to_string(),
                    detail: "WASAPI capture thread terminated unexpectedly during init".to_string(),
                })
            }
        }
    }
}

impl Drop for WasapiCaptureStream {
    fn drop(&mut self) {
        self.stop_flag.store(true, Ordering::Relaxed);
        self.stop_event.signal();
        if let Some(handle) = self.worker.take() {
            let _ = handle.join();
        }
    }
}

struct CapturePipeline {
    device_name: String,
    parsed_fmt: ParsedMixFormat,
    audio_client: IAudioClient,
    capture_client: IAudioCaptureClient,
    audio_event: Option<EventHandle>,
    keepalive: Option<NativeKeepaliveStream>,
}

unsafe fn init_capture_pipeline(
    is_loopback: bool,
    filter: Option<&str>,
) -> Result<CapturePipeline, WasapiInitError> {
    let data_flow = if is_loopback {
        Audio::eRender
    } else {
        Audio::eCapture
    };
    let (device, device_name) = select_imm_device(data_flow, filter, !is_loopback)?;

    let base_flag = if is_loopback {
        AUDCLNT_STREAMFLAGS_LOOPBACK
    } else {
        0
    };

    let client1: IAudioClient = device.Activate(CLSCTX_ALL, None).map_err(|e| {
        WasapiInitError::Wasapi {
            device: device_name.clone(),
            detail: format_wasapi_err("IMMDevice::Activate(IAudioClient) failed", &e),
        }
    })?;
    let mix_fmt = MixFormatPtr(client1.GetMixFormat().map_err(|e| WasapiInitError::Wasapi {
        device: device_name.clone(),
        detail: format_wasapi_err("IAudioClient::GetMixFormat failed", &e),
    })?);
    let parsed_fmt = parse_mix_format(mix_fmt.0).map_err(|detail| WasapiInitError::Wasapi {
        device: device_name.clone(),
        detail,
    })?;

    // Initialize the primary capture stream BEFORE opening the optional silent keepalive render
    // stream so the primary stream always has first claim on audio engine resources.
    let init_event_res = client1.Initialize(
        AUDCLNT_SHAREMODE_SHARED,
        base_flag | AUDCLNT_STREAMFLAGS_EVENTCALLBACK,
        0,
        0,
        mix_fmt.0,
        None,
    );

    let (audio_client, audio_event) = if init_event_res.is_ok() {
        let ev = EventHandle::new_auto_reset().map_err(|detail| WasapiInitError::Wasapi {
            device: device_name.clone(),
            detail,
        })?;
        if client1.SetEventHandle(ev.raw()).is_ok() {
            (client1, Some(ev))
        } else {
            drop(client1);
            let client2: IAudioClient = device.Activate(CLSCTX_ALL, None).map_err(|e| {
                WasapiInitError::Wasapi {
                    device: device_name.clone(),
                    detail: format_wasapi_err("Activate polling fallback failed", &e),
                }
            })?;
            client2
                .Initialize(AUDCLNT_SHAREMODE_SHARED, base_flag, 200_000, 0, mix_fmt.0, None)
                .map_err(|e| WasapiInitError::Wasapi {
                    device: device_name.clone(),
                    detail: format_wasapi_err("IAudioClient::Initialize polling fallback failed", &e),
                })?;
            (client2, None)
        }
    } else {
        // Older drivers or certain loopback endpoints reject EVENTCALLBACK + LOOPBACK;
        // retry cleanly in polling mode with a fresh IAudioClient and the exact GetMixFormat().
        drop(client1);
        let client2: IAudioClient = device.Activate(CLSCTX_ALL, None).map_err(|e| {
            WasapiInitError::Wasapi {
                device: device_name.clone(),
                detail: format_wasapi_err("Activate fallback failed", &e),
            }
        })?;
        client2
            .Initialize(AUDCLNT_SHAREMODE_SHARED, base_flag, 200_000, 0, mix_fmt.0, None)
            .map_err(|e| {
                let ctx = format!(
                    "IAudioClient::Initialize failed ({}Hz, {}ch, {:?})",
                    parsed_fmt.sample_rate, parsed_fmt.channels, parsed_fmt.sample_format
                );
                WasapiInitError::Wasapi {
                    device: device_name.clone(),
                    detail: format_wasapi_err(&ctx, &e),
                }
            })?;
        (client2, None)
    };

    let capture_client: IAudioCaptureClient =
        audio_client
            .GetService()
            .map_err(|e| WasapiInitError::Wasapi {
                device: device_name.clone(),
                detail: format_wasapi_err("GetService(IAudioCaptureClient) failed", &e),
            })?;

    let keepalive = if is_loopback {
        start_native_silent_keepalive(&device, mix_fmt.0)
    } else {
        None
    };

    Ok(CapturePipeline {
        device_name,
        parsed_fmt,
        audio_client,
        capture_client,
        audio_event,
        keepalive,
    })
}

#[allow(clippy::too_many_arguments)]
fn run_capture_loop(
    is_loopback: bool,
    stop_flag: &AtomicBool,
    stop_event: &EventHandle,
    audio_event: Option<&EventHandle>,
    keepalive: Option<&NativeKeepaliveStream>,
    capture_client: &IAudioCaptureClient,
    fmt: ParsedMixFormat,
    target_channels: u16,
    mut producer: Producer<f32>,
    peak_bits: &AtomicU32,
) {
    let hw_ch = (fmt.channels as usize).max(1);
    let tgt_ch = (target_channels as usize).max(1);
    let bps = bytes_per_sample(fmt.sample_format);
    let event_wait_ms = if is_loopback { 5 } else { 50 };

    while !stop_flag.load(Ordering::Relaxed) {
        let timed_out = if let Some(ev) = audio_event {
            let handles = [stop_event.raw(), ev.raw()];
            let wait = unsafe { WaitForMultipleObjects(&handles, false, event_wait_ms) };
            if wait == WAIT_OBJECT_0 {
                break;
            }
            wait == WAIT_TIMEOUT
        } else {
            let wait = unsafe { WaitForSingleObject(stop_event.raw(), 3) };
            if wait == WAIT_OBJECT_0 {
                break;
            }
            true
        };

        if let Some(ka) = keepalive {
            unsafe {
                ka.top_up_silence();
            }
        }

        let mut read_any_packet = false;
        loop {
            let packet_frames = match unsafe { capture_client.GetNextPacketSize() } {
                Ok(0) => break,
                Ok(n) => n,
                Err(_) => return,
            };

            let mut data_ptr: *mut u8 = ptr::null_mut();
            let mut frames_available: u32 = packet_frames;
            let mut flags: u32 = 0;

            let res = unsafe {
                capture_client.GetBuffer(
                    &mut data_ptr,
                    &mut frames_available,
                    &mut flags,
                    None,
                    None,
                )
            };

            match res {
                Ok(()) => {}
                Err(e) if e.code() == AUDCLNT_S_BUFFER_EMPTY => break,
                Err(_) => return,
            }

            read_any_packet = true;
            let is_silent =
                (flags & (AUDCLNT_BUFFERFLAGS_SILENT.0 as u32)) != 0 || data_ptr.is_null();
            let frames = frames_available as usize;
            let available_frames = producer.slots() / tgt_ch;
            let frames_to_write = frames.min(available_frames);
            let samples_to_write = frames_to_write * tgt_ch;

            if samples_to_write > 0 {
                let mut max_abs = 0.0f32;
                if let Ok(mut chunk) = producer.write_chunk(samples_to_write) {
                    let (first, second) = chunk.as_mut_slices();
                    if is_silent {
                        first.fill(0.0);
                        second.fill(0.0);
                    } else {
                        let byte_len = frames * fmt.block_align;
                        let raw_bytes = unsafe { slice::from_raw_parts(data_ptr, byte_len) };
                        let mut out_idx = 0usize;
                        for f in 0..frames_to_write {
                            let frame_off = f * fmt.block_align;
                            for c in 0..tgt_ch {
                                let sample = if tgt_ch == 1 && hw_ch >= 2 {
                                    let s0 =
                                        decode_sample_at(raw_bytes, frame_off, fmt.sample_format);
                                    let s1 = decode_sample_at(
                                        raw_bytes,
                                        frame_off + bps,
                                        fmt.sample_format,
                                    );
                                    (s0 + s1) * 0.5
                                } else {
                                    let src_c = c.min(hw_ch - 1);
                                    decode_sample_at(
                                        raw_bytes,
                                        frame_off + src_c * bps,
                                        fmt.sample_format,
                                    )
                                };
                                let abs = sample.abs();
                                if abs > max_abs {
                                    max_abs = abs;
                                }
                                if out_idx < first.len() {
                                    first[out_idx] = sample;
                                } else {
                                    second[out_idx - first.len()] = sample;
                                }
                                out_idx += 1;
                            }
                        }
                    }
                    chunk.commit_all();
                    peak_bits.store(max_abs.to_bits(), Ordering::Relaxed);
                }
            }

            if unsafe { capture_client.ReleaseBuffer(frames_available) }.is_err() {
                return;
            }
        }

        if timed_out && !read_any_packet && audio_event.is_some() {
            peak_bits.store(0.0f32.to_bits(), Ordering::Relaxed);
        }
    }
}

/// Active native Windows WASAPI playback stream using the endpoint's exact `GetMixFormat()`.
pub struct WasapiPlaybackStream {
    stop_flag: Arc<AtomicBool>,
    stop_event: Arc<EventHandle>,
    worker: Option<JoinHandle<()>>,
    pub device_name: String,
    pub sample_rate: u32,
    pub channels: u16,
}

impl WasapiPlaybackStream {
    pub fn start(
        device_name_filter: Option<&str>,
        source_channels: u16,
        consumer: Consumer<f32>,
        volume_bits: Arc<AtomicU32>,
        peak_bits: Arc<AtomicU32>,
        underrun_count: Arc<AtomicU64>,
    ) -> Result<Self, AudioError> {
        let filter_owned = device_name_filter.map(|s| s.to_string());
        let stop_flag = Arc::new(AtomicBool::new(false));
        let stop_event = Arc::new(EventHandle::new_manual_reset().map_err(|detail| {
            AudioError::Wasapi {
                device: filter_owned
                    .clone()
                    .unwrap_or_else(|| "Windows Default Output".to_string()),
                detail,
            }
        })?);

        let stop_flag_cl = Arc::clone(&stop_flag);
        let stop_event_cl = Arc::clone(&stop_event);
        let (init_tx, init_rx) =
            mpsc::sync_channel::<Result<(String, u32, u16), WasapiInitError>>(1);

        let worker = thread::Builder::new()
            .name("rsrelay-wasapi-playback".to_string())
            .spawn(move || {
                let _com = match ComGuard::init_mta() {
                    Ok(g) => g,
                    Err(detail) => {
                        let _ = init_tx.send(Err(WasapiInitError::Wasapi {
                            device: "COM".to_string(),
                            detail,
                        }));
                        return;
                    }
                };

                let init_res = unsafe { init_playback_pipeline(filter_owned.as_deref()) };
                let PlaybackPipeline {
                    device_name,
                    parsed_fmt,
                    max_frames,
                    audio_client,
                    render_client,
                    audio_event,
                } = match init_res {
                    Ok(v) => v,
                    Err(e) => {
                        let _ = init_tx.send(Err(e));
                        return;
                    }
                };

                let _prio_guard =
                    crate::promote_worker_thread_priority(parsed_fmt.sample_rate, 240);

                // Prime initial buffer with silence before Start() to avoid initial glitch
                unsafe {
                    if let Ok(buf) = render_client.GetBuffer(max_frames) {
                        let _ = render_client
                            .ReleaseBuffer(max_frames, AUDCLNT_BUFFERFLAGS_SILENT.0 as u32);
                        let _ = buf;
                    }
                }

                if let Err(e) = unsafe { audio_client.Start() } {
                    let _ = init_tx.send(Err(WasapiInitError::Wasapi {
                        device: device_name,
                        detail: format_wasapi_err("IAudioClient::Start failed", &e),
                    }));
                    return;
                }

                let _ = init_tx.send(Ok((
                    device_name,
                    parsed_fmt.sample_rate,
                    parsed_fmt.channels,
                )));

                run_playback_loop(
                    &stop_flag_cl,
                    &stop_event_cl,
                    audio_event.as_ref(),
                    &audio_client,
                    &render_client,
                    parsed_fmt,
                    max_frames,
                    source_channels,
                    consumer,
                    &volume_bits,
                    &peak_bits,
                    &underrun_count,
                );

                unsafe {
                    let _ = audio_client.Stop();
                }
            })
            .map_err(|e| AudioError::Wasapi {
                device: "WASAPI Playback".to_string(),
                detail: format!("failed to spawn playback thread: {e}"),
            })?;

        match init_rx.recv() {
            Ok(Ok((device_name, sample_rate, channels))) => Ok(Self {
                stop_flag,
                stop_event,
                worker: Some(worker),
                device_name,
                sample_rate,
                channels,
            }),
            Ok(Err(err)) => {
                let _ = worker.join();
                Err(err.into())
            }
            Err(_) => {
                let _ = worker.join();
                Err(AudioError::Wasapi {
                    device: device_name_filter
                        .unwrap_or("Default Windows Output")
                        .to_string(),
                    detail: "WASAPI playback thread terminated unexpectedly during init"
                        .to_string(),
                })
            }
        }
    }
}

impl Drop for WasapiPlaybackStream {
    fn drop(&mut self) {
        self.stop_flag.store(true, Ordering::Relaxed);
        self.stop_event.signal();
        if let Some(handle) = self.worker.take() {
            let _ = handle.join();
        }
    }
}

struct PlaybackPipeline {
    device_name: String,
    parsed_fmt: ParsedMixFormat,
    max_frames: u32,
    audio_client: IAudioClient,
    render_client: IAudioRenderClient,
    audio_event: Option<EventHandle>,
}

unsafe fn init_playback_pipeline(
    filter: Option<&str>,
) -> Result<PlaybackPipeline, WasapiInitError> {
    let (device, device_name) = select_imm_device(Audio::eRender, filter, false)?;

    let client1: IAudioClient = device.Activate(CLSCTX_ALL, None).map_err(|e| {
        WasapiInitError::Wasapi {
            device: device_name.clone(),
            detail: format_wasapi_err("IMMDevice::Activate(IAudioClient) failed", &e),
        }
    })?;
    let mix_fmt = MixFormatPtr(client1.GetMixFormat().map_err(|e| WasapiInitError::Wasapi {
        device: device_name.clone(),
        detail: format_wasapi_err("IAudioClient::GetMixFormat failed", &e),
    })?);
    let parsed_fmt = parse_mix_format(mix_fmt.0).map_err(|detail| WasapiInitError::Wasapi {
        device: device_name.clone(),
        detail,
    })?;

    let init_event_res = client1.Initialize(
        AUDCLNT_SHAREMODE_SHARED,
        AUDCLNT_STREAMFLAGS_EVENTCALLBACK,
        0,
        0,
        mix_fmt.0,
        None,
    );

    let (audio_client, audio_event) = if init_event_res.is_ok() {
        let ev = EventHandle::new_auto_reset().map_err(|detail| WasapiInitError::Wasapi {
            device: device_name.clone(),
            detail,
        })?;
        if client1.SetEventHandle(ev.raw()).is_ok() {
            (client1, Some(ev))
        } else {
            drop(client1);
            let client2: IAudioClient = device.Activate(CLSCTX_ALL, None).map_err(|e| {
                WasapiInitError::Wasapi {
                    device: device_name.clone(),
                    detail: format_wasapi_err("Activate polling fallback failed", &e),
                }
            })?;
            client2
                .Initialize(AUDCLNT_SHAREMODE_SHARED, 0, 200_000, 0, mix_fmt.0, None)
                .map_err(|e| WasapiInitError::Wasapi {
                    device: device_name.clone(),
                    detail: format_wasapi_err("Initialize polling fallback failed", &e),
                })?;
            (client2, None)
        }
    } else {
        drop(client1);
        let client2: IAudioClient = device.Activate(CLSCTX_ALL, None).map_err(|e| {
            WasapiInitError::Wasapi {
                device: device_name.clone(),
                detail: format_wasapi_err("Activate fallback failed", &e),
            }
        })?;
        client2
            .Initialize(AUDCLNT_SHAREMODE_SHARED, 0, 200_000, 0, mix_fmt.0, None)
            .map_err(|e| {
                let ctx = format!(
                    "IAudioClient::Initialize failed ({}Hz, {}ch, {:?})",
                    parsed_fmt.sample_rate, parsed_fmt.channels, parsed_fmt.sample_format
                );
                WasapiInitError::Wasapi {
                    device: device_name.clone(),
                    detail: format_wasapi_err(&ctx, &e),
                }
            })?;
        (client2, None)
    };

    let max_frames = audio_client
        .GetBufferSize()
        .map_err(|e| WasapiInitError::Wasapi {
            device: device_name.clone(),
            detail: format_wasapi_err("GetBufferSize failed", &e),
        })?;
    let render_client: IAudioRenderClient =
        audio_client
            .GetService()
            .map_err(|e| WasapiInitError::Wasapi {
                device: device_name.clone(),
                detail: format_wasapi_err("GetService(IAudioRenderClient) failed", &e),
            })?;

    Ok(PlaybackPipeline {
        device_name,
        parsed_fmt,
        max_frames,
        audio_client,
        render_client,
        audio_event,
    })
}

#[allow(clippy::too_many_arguments)]
fn run_playback_loop(
    stop_flag: &AtomicBool,
    stop_event: &EventHandle,
    audio_event: Option<&EventHandle>,
    audio_client: &IAudioClient,
    render_client: &IAudioRenderClient,
    fmt: ParsedMixFormat,
    max_frames: u32,
    source_channels: u16,
    mut consumer: Consumer<f32>,
    volume_bits: &AtomicU32,
    peak_bits: &AtomicU32,
    underrun_count: &AtomicU64,
) {
    let hw_ch = (fmt.channels as usize).max(1);
    let src_ch = (source_channels as usize).max(1);
    let bps = bytes_per_sample(fmt.sample_format);

    while !stop_flag.load(Ordering::Relaxed) {
        if let Some(ev) = audio_event {
            let handles = [stop_event.raw(), ev.raw()];
            let wait = unsafe { WaitForMultipleObjects(&handles, false, 50) };
            if wait == WAIT_OBJECT_0 {
                break;
            }
        } else {
            let wait = unsafe { WaitForSingleObject(stop_event.raw(), 3) };
            if wait == WAIT_OBJECT_0 {
                break;
            }
        }

        let padding = match unsafe { audio_client.GetCurrentPadding() } {
            Ok(p) => p,
            Err(_) => break,
        };
        let frames_available = max_frames.saturating_sub(padding);
        if frames_available == 0 {
            continue;
        }

        let data_ptr = match unsafe { render_client.GetBuffer(frames_available) } {
            Ok(p) if !p.is_null() => p,
            _ => break,
        };

        let total_frames = frames_available as usize;
        let out_bytes =
            unsafe { slice::from_raw_parts_mut(data_ptr, total_frames * fmt.block_align) };
        let avail_frames = consumer.slots() / src_ch;
        let frames_to_read = total_frames.min(avail_frames);
        let samples_to_read = frames_to_read * src_ch;
        let gain = f32::from_bits(volume_bits.load(Ordering::Relaxed));
        let mut max_abs = 0.0f32;

        // Zero entire buffer first so underrun tail and extra surround channels (>2ch) stay silent
        out_bytes.fill(if fmt.sample_format == NativeSampleFormat::U8 {
            128
        } else {
            0
        });

        if samples_to_read > 0 {
            if let Ok(chunk) = consumer.read_chunk(samples_to_read) {
                let (first, second) = chunk.as_slices();
                for f in 0..frames_to_read {
                    let src_base = f * src_ch;
                    let dst_frame = f * fmt.block_align;
                    // Map stereo/mono audio onto front L/R channels (ch 0..min(hw_ch, 2)) so 5.1/7.1
                    // endpoints play clean front-left/front-right stereo without center/LFE artifacts.
                    let active_out_ch = hw_ch.min(2);
                    for c in 0..active_out_ch {
                        let raw = if active_out_ch == 1 && src_ch >= 2 {
                            let s0 = if src_base < first.len() {
                                first[src_base]
                            } else {
                                second[src_base - first.len()]
                            };
                            let s1 = if src_base + 1 < first.len() {
                                first[src_base + 1]
                            } else {
                                second[src_base + 1 - first.len()]
                            };
                            (s0 + s1) * 0.5
                        } else {
                            let idx = src_base + c.min(src_ch - 1);
                            if idx < first.len() {
                                first[idx]
                            } else {
                                second[idx - first.len()]
                            }
                        };
                        let val = (raw * gain).clamp(-1.0, 1.0);
                        let abs = val.abs();
                        if abs > max_abs {
                            max_abs = abs;
                        }
                        encode_sample_at(out_bytes, dst_frame + c * bps, val, fmt.sample_format);
                    }
                }
                chunk.commit_all();
            }
        }

        if frames_to_read < total_frames {
            underrun_count.fetch_add(1, Ordering::Relaxed);
        }
        peak_bits.store(max_abs.to_bits(), Ordering::Relaxed);

        if unsafe { render_client.ReleaseBuffer(frames_available, 0) }.is_err() {
            break;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_windows_list_audio_devices_native_zero_churn() {
        for _ in 0..50 {
            let devs = list_audio_devices_native().expect("list Windows audio devices natively");
            std::hint::black_box(devs);
        }
    }

    #[test]
    fn test_nonexistent_device_filter_returns_device_not_found() {
        let (tx, _rx) = rtrb::RingBuffer::<f32>::new(960);
        let peak = Arc::new(AtomicU32::new(0));
        let res = WasapiCaptureStream::start(
            CaptureSource::Microphone,
            Some("__nonexistent_rsrelay_device_xyz__"),
            2,
            tx,
            peak,
        );
        assert!(matches!(res, Err(AudioError::DeviceNotFound(_))));
    }

    #[test]
    fn test_native_sample_format_encode_decode_roundtrip() {
        for fmt in [
            NativeSampleFormat::F32,
            NativeSampleFormat::I16,
            NativeSampleFormat::I24Packed,
            NativeSampleFormat::I32,
            NativeSampleFormat::U8,
        ] {
            let bps = bytes_per_sample(fmt);
            let mut buf = vec![0u8; bps * 3];
            let inputs = [-0.75f32, 0.0f32, 0.625f32];
            for (i, &val) in inputs.iter().enumerate() {
                encode_sample_at(&mut buf, i * bps, val, fmt);
            }
            let tol = if fmt == NativeSampleFormat::U8 {
                0.02
            } else {
                0.001
            };
            for (i, &expected) in inputs.iter().enumerate() {
                let actual = decode_sample_at(&buf, i * bps, fmt);
                assert!(
                    (actual - expected).abs() < tol,
                    "format {fmt:?} mismatch at {i}: expected {expected}, got {actual}"
                );
            }
        }
    }
}
