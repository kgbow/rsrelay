//! Native Windows WASAPI `GetMixFormat()` capture (Loopback & Microphone) and playback streams.
//!
//! Why this exists alongside `cpal 0.15`:
//! In `cpal 0.15`, `wasapi::device::config_to_waveformatextensible` discards the endpoint's real
//! `dwChannelMask` from `IAudioClient::GetMixFormat()` and substitutes `KSAUDIO_SPEAKER_DIRECTOUT (0)`.
//! On Windows 10/11 endpoints with Spatial Sound (Windows Sonic / Dolby Atmos / DTS:X), 5.1/7.1
//! virtual surround headsets, APO enhancers, Bluetooth profiles, or Loopback capture, WASAPI's
//! `IAudioClient::IsFormatSupported` rejects `dwChannelMask = 0` with `S_FALSE`, causing `cpal` to
//! fail with `BuildStreamError::StreamConfigNotSupported`.
//!
//! This module initializes `IAudioClient` in shared mode with the **exact, unmodified `WAVEFORMATEX`**
//! returned by `GetMixFormat()`, which Windows WASAPI guarantees to accept on every active endpoint.

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
use windows::core::GUID;
use windows::Win32::Devices::Properties;
use windows::Win32::Foundation::{CloseHandle, HANDLE, RPC_E_CHANGED_MODE, WAIT_OBJECT_0};
use windows::Win32::Media::{
    Audio::{
        self, IAudioCaptureClient, IAudioClient, IAudioRenderClient, IMMDevice,
        IMMDeviceEnumerator, MMDeviceEnumerator, AUDCLNT_BUFFERFLAGS_SILENT,
        AUDCLNT_SHAREMODE_SHARED, AUDCLNT_STREAMFLAGS_EVENTCALLBACK, AUDCLNT_STREAMFLAGS_LOOPBACK,
        AUDCLNT_S_BUFFER_EMPTY, DEVICE_STATE_ACTIVE, WAVEFORMATEX, WAVEFORMATEXTENSIBLE,
        WAVE_FORMAT_PCM,
    },
    KernelStreaming::{
        KSDATAFORMAT_SUBTYPE_PCM, WAVE_FORMAT_EXTENSIBLE,
    },
    Multimedia::{KSDATAFORMAT_SUBTYPE_IEEE_FLOAT, WAVE_FORMAT_IEEE_FLOAT},
};
use windows::Win32::System::Com::{
    CoCreateInstance, CoInitializeEx, CoTaskMemFree, CoUninitialize, StructuredStorage,
    CLSCTX_ALL, COINIT_MULTITHREADED, STGM_READ,
};
use windows::Win32::System::Threading::{
    CreateEventW, SetEvent, WaitForMultipleObjects, WaitForSingleObject,
};
use windows::Win32::System::Variant::VT_LPWSTR;

use crate::device::{is_virtual_mic_input_device_name, AudioError, CaptureSource};

/// RAII guard for COM initialization on a dedicated worker thread.
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

unsafe fn read_device_friendly_name(device: &IMMDevice) -> String {
    let Ok(store) = device.OpenPropertyStore(STGM_READ) else {
        return "Windows Audio Device".to_string();
    };
    let Ok(mut prop) =
        store.GetValue(&Properties::DEVPKEY_Device_FriendlyName as *const _ as *const _)
    else {
        return "Windows Audio Device".to_string();
    };

    let raw = &prop.as_raw().Anonymous.Anonymous;
    let name = if raw.vt == VT_LPWSTR.0 {
        let ptr_u16 = *(&raw.Anonymous as *const _ as *const *const u16);
        if ptr_u16.is_null() {
            "Windows Audio Device".to_string()
        } else {
            let mut len = 0usize;
            while *ptr_u16.add(len) != 0 {
                len += 1;
            }
            let slice_u16 = slice::from_raw_parts(ptr_u16, len);
            OsString::from_wide(slice_u16)
                .to_string_lossy()
                .into_owned()
        }
    } else {
        "Windows Audio Device".to_string()
    };

    let _ = StructuredStorage::PropVariantClear(&mut prop);
    name
}

unsafe fn select_imm_device(
    data_flow: Audio::EDataFlow,
    name_filter: Option<&str>,
    avoid_virtual_mic_inputs: bool,
) -> Result<(IMMDevice, String), String> {
    let enumerator: IMMDeviceEnumerator =
        CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL)
            .map_err(|e| format!("CoCreateInstance(MMDeviceEnumerator) failed: {e}"))?;

    if let Some(filter) = name_filter {
        let filter_lower = filter.to_lowercase();
        let collection = enumerator
            .EnumAudioEndpoints(data_flow, DEVICE_STATE_ACTIVE)
            .map_err(|e| format!("EnumAudioEndpoints failed: {e}"))?;
        let count = collection
            .GetCount()
            .map_err(|e| format!("IMMDeviceCollection::GetCount failed: {e}"))?;
        for idx in 0..count {
            if let Ok(dev) = collection.Item(idx) {
                let name = read_device_friendly_name(&dev);
                if name.to_lowercase().contains(&filter_lower) {
                    return Ok((dev, name));
                }
            }
        }
        return Err(format!("no active Windows endpoint matching '{filter}'"));
    }

    let default_dev = enumerator
        .GetDefaultAudioEndpoint(data_flow, Audio::eConsole)
        .map_err(|e| format!("GetDefaultAudioEndpoint failed: {e}"))?;
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

struct NativeKeepaliveGuard {
    client: IAudioClient,
    _render: IAudioRenderClient,
}

impl Drop for NativeKeepaliveGuard {
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
) -> Option<NativeKeepaliveGuard> {
    let client: IAudioClient = device.Activate(CLSCTX_ALL, None).ok()?;
    client
        .Initialize(AUDCLNT_SHAREMODE_SHARED, 0, 200_000, 0, mix_fmt_ptr, None)
        .ok()?;
    let frames = client.GetBufferSize().ok()?;
    let render: IAudioRenderClient = client.GetService().ok()?;
    if let Ok(buf) = render.GetBuffer(frames) {
        let _ = render.ReleaseBuffer(frames, AUDCLNT_BUFFERFLAGS_SILENT.0 as u32);
        let _ = buf;
    }
    client.Start().ok()?;
    Some(NativeKeepaliveGuard {
        client,
        _render: render,
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
        let (init_tx, init_rx) = mpsc::sync_channel::<Result<(String, u32), String>>(1);

        let worker = thread::Builder::new()
            .name(if is_loopback {
                "rsrelay-wasapi-loopback".to_string()
            } else {
                "rsrelay-wasapi-mic".to_string()
            })
            .spawn(move || {
                let _com = match ComGuard::init_mta() {
                    Ok(g) => g,
                    Err(e) => {
                        let _ = init_tx.send(Err(e));
                        return;
                    }
                };

                let init_res = unsafe {
                    init_capture_pipeline(
                        is_loopback,
                        filter_owned.as_deref(),
                    )
                };

                let (
                    device_name,
                    parsed_fmt,
                    audio_client,
                    capture_client,
                    audio_event,
                    _keepalive,
                ) = match init_res {
                    Ok(v) => v,
                    Err(e) => {
                        let _ = init_tx.send(Err(e));
                        return;
                    }
                };

                let _prio_guard =
                    crate::promote_worker_thread_priority(parsed_fmt.sample_rate, 240);

                if let Err(e) = unsafe { audio_client.Start() } {
                    let _ = init_tx.send(Err(format!("IAudioClient::Start failed: {e}")));
                    return;
                }

                let _ = init_tx.send(Ok((device_name, parsed_fmt.sample_rate)));

                run_capture_loop(
                    is_loopback,
                    &stop_flag_cl,
                    &stop_event_cl,
                    audio_event.as_ref(),
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
            Ok(Err(detail)) => {
                let _ = worker.join();
                Err(AudioError::Wasapi {
                    device: device_name_filter
                        .unwrap_or("Default Windows Device")
                        .to_string(),
                    detail,
                })
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

unsafe fn init_capture_pipeline(
    is_loopback: bool,
    filter: Option<&str>,
) -> Result<
    (
        String,
        ParsedMixFormat,
        IAudioClient,
        IAudioCaptureClient,
        Option<EventHandle>,
        Option<NativeKeepaliveGuard>,
    ),
    String,
> {
    let data_flow = if is_loopback {
        Audio::eRender
    } else {
        Audio::eCapture
    };
    let (device, device_name) = select_imm_device(data_flow, filter, !is_loopback)?;

    // Attempt 1: Event-driven shared mode with exact GetMixFormat()
    let base_flag = if is_loopback {
        AUDCLNT_STREAMFLAGS_LOOPBACK
    } else {
        0
    };

    let client1: IAudioClient = device
        .Activate(CLSCTX_ALL, None)
        .map_err(|e| format!("IMMDevice::Activate(IAudioClient) failed on '{device_name}': {e}"))?;
    let mix_fmt = MixFormatPtr(
        client1
            .GetMixFormat()
            .map_err(|e| format!("IAudioClient::GetMixFormat failed on '{device_name}': {e}"))?,
    );
    let parsed_fmt = parse_mix_format(mix_fmt.0)?;

    let keepalive = if is_loopback {
        start_native_silent_keepalive(&device, mix_fmt.0)
    } else {
        None
    };

    let init_event_res = client1.Initialize(
        AUDCLNT_SHAREMODE_SHARED,
        base_flag | AUDCLNT_STREAMFLAGS_EVENTCALLBACK,
        0,
        0,
        mix_fmt.0,
        None,
    );

    let (audio_client, audio_event) = if init_event_res.is_ok() {
        let ev = EventHandle::new_auto_reset()?;
        if client1.SetEventHandle(ev.raw()).is_ok() {
            (client1, Some(ev))
        } else {
            // Fall back to polling mode with a fresh IAudioClient
            let client2: IAudioClient = device
                .Activate(CLSCTX_ALL, None)
                .map_err(|e| format!("Activate fallback failed: {e}"))?;
            client2
                .Initialize(AUDCLNT_SHAREMODE_SHARED, base_flag, 200_000, 0, mix_fmt.0, None)
                .map_err(|e| {
                    format!(
                        "IAudioClient::Initialize polling fallback failed on '{device_name}': {e}"
                    )
                })?;
            (client2, None)
        }
    } else {
        // Older drivers or certain loopback endpoints reject EVENTCALLBACK + LOOPBACK;
        // retry cleanly in polling mode with a fresh IAudioClient and the exact GetMixFormat().
        drop(client1);
        let client2: IAudioClient = device
            .Activate(CLSCTX_ALL, None)
            .map_err(|e| format!("Activate fallback failed on '{device_name}': {e}"))?;
        client2
            .Initialize(AUDCLNT_SHAREMODE_SHARED, base_flag, 200_000, 0, mix_fmt.0, None)
            .map_err(|e| {
                format!(
                    "IAudioClient::Initialize failed on '{device_name}' ({}Hz, {}ch, {:?}): {e}",
                    parsed_fmt.sample_rate, parsed_fmt.channels, parsed_fmt.sample_format
                )
            })?;
        (client2, None)
    };

    let capture_client: IAudioCaptureClient = audio_client
        .GetService()
        .map_err(|e| format!("GetService(IAudioCaptureClient) failed on '{device_name}': {e}"))?;

    Ok((
        device_name,
        parsed_fmt,
        audio_client,
        capture_client,
        audio_event,
        keepalive,
    ))
}

#[allow(clippy::too_many_arguments)]
fn run_capture_loop(
    is_loopback: bool,
    stop_flag: &AtomicBool,
    stop_event: &EventHandle,
    audio_event: Option<&EventHandle>,
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
        if let Some(ev) = audio_event {
            let handles = [stop_event.raw(), ev.raw()];
            let wait = unsafe { WaitForMultipleObjects(&handles, false, event_wait_ms) };
            if wait == WAIT_OBJECT_0 {
                break;
            }
        } else {
            let wait = unsafe { WaitForSingleObject(stop_event.raw(), 3) };
            if wait == WAIT_OBJECT_0 {
                break;
            }
        }

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

            let is_silent = (flags & (AUDCLNT_BUFFERFLAGS_SILENT.0 as u32)) != 0 || data_ptr.is_null();
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
                                    let s0 = decode_sample_at(raw_bytes, frame_off, fmt.sample_format);
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
        let (init_tx, init_rx) = mpsc::sync_channel::<Result<(String, u32, u16), String>>(1);

        let worker = thread::Builder::new()
            .name("rsrelay-wasapi-playback".to_string())
            .spawn(move || {
                let _com = match ComGuard::init_mta() {
                    Ok(g) => g,
                    Err(e) => {
                        let _ = init_tx.send(Err(e));
                        return;
                    }
                };

                let init_res = unsafe { init_playback_pipeline(filter_owned.as_deref()) };
                let (
                    device_name,
                    parsed_fmt,
                    max_frames,
                    audio_client,
                    render_client,
                    audio_event,
                ) = match init_res {
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
                    let _ = init_tx.send(Err(format!("IAudioClient::Start failed: {e}")));
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
            Ok(Err(detail)) => {
                let _ = worker.join();
                Err(AudioError::Wasapi {
                    device: device_name_filter
                        .unwrap_or("Default Windows Output")
                        .to_string(),
                    detail,
                })
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

unsafe fn init_playback_pipeline(
    filter: Option<&str>,
) -> Result<
    (
        String,
        ParsedMixFormat,
        u32,
        IAudioClient,
        IAudioRenderClient,
        Option<EventHandle>,
    ),
    String,
> {
    let (device, device_name) = select_imm_device(Audio::eRender, filter, false)?;

    let client1: IAudioClient = device
        .Activate(CLSCTX_ALL, None)
        .map_err(|e| format!("IMMDevice::Activate(IAudioClient) failed on '{device_name}': {e}"))?;
    let mix_fmt = MixFormatPtr(
        client1
            .GetMixFormat()
            .map_err(|e| format!("IAudioClient::GetMixFormat failed on '{device_name}': {e}"))?,
    );
    let parsed_fmt = parse_mix_format(mix_fmt.0)?;

    let init_event_res = client1.Initialize(
        AUDCLNT_SHAREMODE_SHARED,
        AUDCLNT_STREAMFLAGS_EVENTCALLBACK,
        0,
        0,
        mix_fmt.0,
        None,
    );

    let (audio_client, audio_event) = if init_event_res.is_ok() {
        let ev = EventHandle::new_auto_reset()?;
        if client1.SetEventHandle(ev.raw()).is_ok() {
            (client1, Some(ev))
        } else {
            let client2: IAudioClient = device
                .Activate(CLSCTX_ALL, None)
                .map_err(|e| format!("Activate fallback failed: {e}"))?;
            client2
                .Initialize(AUDCLNT_SHAREMODE_SHARED, 0, 200_000, 0, mix_fmt.0, None)
                .map_err(|e| format!("Initialize polling fallback failed on '{device_name}': {e}"))?;
            (client2, None)
        }
    } else {
        drop(client1);
        let client2: IAudioClient = device
            .Activate(CLSCTX_ALL, None)
            .map_err(|e| format!("Activate fallback failed on '{device_name}': {e}"))?;
        client2
            .Initialize(AUDCLNT_SHAREMODE_SHARED, 0, 200_000, 0, mix_fmt.0, None)
            .map_err(|e| {
                format!(
                    "IAudioClient::Initialize failed on '{device_name}' ({}Hz, {}ch, {:?}): {e}",
                    parsed_fmt.sample_rate, parsed_fmt.channels, parsed_fmt.sample_format
                )
            })?;
        (client2, None)
    };

    let max_frames = audio_client
        .GetBufferSize()
        .map_err(|e| format!("GetBufferSize failed on '{device_name}': {e}"))?;
    let render_client: IAudioRenderClient = audio_client
        .GetService()
        .map_err(|e| format!("GetService(IAudioRenderClient) failed on '{device_name}': {e}"))?;

    Ok((
        device_name,
        parsed_fmt,
        max_frames,
        audio_client,
        render_client,
        audio_event,
    ))
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
        let out_bytes = unsafe { slice::from_raw_parts_mut(data_ptr, total_frames * fmt.block_align) };
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
