#[cfg(not(target_os = "windows"))]
use std::sync::Mutex;
use std::sync::{
    atomic::{AtomicBool, AtomicU32, Ordering},
    Arc,
};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

#[cfg(not(any(target_os = "macos", target_os = "windows")))]
use cpal::traits::HostTrait;
#[cfg(not(target_os = "windows"))]
use cpal::traits::{DeviceTrait, StreamTrait};
#[cfg(not(target_os = "windows"))]
use cpal::SampleFormat;
use rtrb::Producer;

use crate::device::{AudioError, CaptureSource};

/// Active audio capture stream handle.
///
/// Dropping this handle stops the underlying OS audio stream or synthetic generator thread.
pub struct CaptureStreamHandle {
    #[cfg(not(target_os = "windows"))]
    _cpal_stream: Option<cpal::Stream>,
    #[cfg(target_os = "macos")]
    _sck_stream: Option<crate::macos_sck::MacosSckStream>,
    #[cfg(target_os = "windows")]
    _wasapi_stream: Option<crate::windows_wasapi::WasapiCaptureStream>,
    stop_flag: Arc<AtomicBool>,
    tone_thread: Option<JoinHandle<()>>,
    pub device_name: String,
    pub sample_rate: u32,
    pub channels: u16,
    peak_bits: Arc<AtomicU32>,
}

impl CaptureStreamHandle {
    /// Read the most recent peak amplitude (`0.0..=1.0`) measured in the capture callback.
    #[inline]
    pub fn peak_level(&self) -> f32 {
        f32::from_bits(self.peak_bits.load(Ordering::Relaxed))
    }
}

impl Drop for CaptureStreamHandle {
    fn drop(&mut self) {
        self.stop_flag.store(true, Ordering::Relaxed);
        if let Some(handle) = self.tone_thread.take() {
            let _ = handle.join();
        }
    }
}

/// Start an audio capture stream pushing interleaved `f32` samples at `target_channels`
/// into the lock-free `rtrb::Producer<f32>`.
pub fn start_capture_stream(
    source: CaptureSource,
    device_name_filter: Option<&str>,
    target_channels: u16,
    #[allow(unused_mut)] mut producer: Producer<f32>,
) -> Result<CaptureStreamHandle, AudioError> {
    let peak_bits = Arc::new(AtomicU32::new(0.0f32.to_bits()));
    let stop_flag = Arc::new(AtomicBool::new(false));

    if source == CaptureSource::SyntheticTone {
        let stop = Arc::clone(&stop_flag);
        let peak = Arc::clone(&peak_bits);
        let sample_rate = 48_000u32;
        let channels = target_channels;

        let handle = thread::Builder::new()
            .name("rsrelay-synth-tone".to_string())
            .spawn(move || {
                let frame_step = Duration::from_micros(5_000); // 5ms = 240 frames @ 48kHz
                let frames_per_tick = 240usize;
                let mut phase_l = 0.0f32;
                let mut phase_r = 0.0f32;
                let inc_l = 2.0 * std::f32::consts::PI * 440.0 / (sample_rate as f32);
                let inc_r = 2.0 * std::f32::consts::PI * 660.0 / (sample_rate as f32);
                let mut next_deadline = Instant::now() + frame_step;

                while !stop.load(Ordering::Relaxed) {
                    let needed = frames_per_tick * (channels as usize);
                    if let Ok(mut chunk) = producer.write_chunk(needed) {
                        let (first, second) = chunk.as_mut_slices();
                        let mut idx = 0;
                        for _ in 0..frames_per_tick {
                            let l = phase_l.sin() * 0.25;
                            let r = phase_r.sin() * 0.25;
                            phase_l = (phase_l + inc_l).rem_euclid(2.0 * std::f32::consts::PI);
                            phase_r = (phase_r + inc_r).rem_euclid(2.0 * std::f32::consts::PI);

                            for ch in 0..channels {
                                let s = if ch == 0 { l } else { r };
                                if idx < first.len() {
                                    first[idx] = s;
                                } else {
                                    second[idx - first.len()] = s;
                                }
                                idx += 1;
                            }
                        }
                        chunk.commit_all();
                        peak.store(0.25f32.to_bits(), Ordering::Relaxed);
                    }

                    let now = Instant::now();
                    if next_deadline > now {
                        thread::sleep(next_deadline - now);
                    }
                    next_deadline += frame_step;
                }
            })
            .expect("spawn synthetic tone thread");

        return Ok(CaptureStreamHandle {
            #[cfg(not(target_os = "windows"))]
            _cpal_stream: None,
            #[cfg(target_os = "macos")]
            _sck_stream: None,
            #[cfg(target_os = "windows")]
            _wasapi_stream: None,
            stop_flag,
            tone_thread: Some(handle),
            device_name: "Synthetic 440Hz/660Hz Stereo Generator".to_string(),
            sample_rate,
            channels,
            peak_bits,
        });
    }

    // On Windows, always use the direct native WASAPI `GetMixFormat()` capture pipeline:
    // - Zero `IsFormatSupported` RPC storms into `audiodg.exe`
    // - Exact `dwChannelMask` & 24-bit/32-bit/5.1/7.1 compatibility for both default and named devices
    // - Continuous inline `AUDCLNT_BUFFERFLAGS_SILENT` loopback keepalive with zero extra threads
    #[cfg(target_os = "windows")]
    {
        let wasapi = crate::windows_wasapi::WasapiCaptureStream::start(
            source,
            device_name_filter,
            target_channels,
            producer,
            Arc::clone(&peak_bits),
        )?;
        let device_name = wasapi.device_name.clone();
        let sample_rate = wasapi.sample_rate;
        Ok(CaptureStreamHandle {
            _wasapi_stream: Some(wasapi),
            stop_flag,
            tone_thread: None,
            device_name,
            sample_rate,
            channels: target_channels,
            peak_bits,
        })
    }

    // On macOS 13+, prefer native driverless ScreenCaptureKit for SystemLoopback when no explicit device filter is set
    #[cfg(target_os = "macos")]
    if source == CaptureSource::SystemLoopback && device_name_filter.is_none() {
        let permitted = crate::macos_sck::has_screen_capture_permission()
            || crate::macos_sck::request_screen_capture_permission();
        if permitted {
            match crate::macos_sck::MacosSckStream::start(
                48_000,
                target_channels,
                producer,
                Arc::clone(&peak_bits),
            ) {
                Ok(sck_stream) => {
                    tracing::info!("Started native macOS ScreenCaptureKit driverless system audio loopback (48000 Hz)");
                    return Ok(CaptureStreamHandle {
                        _cpal_stream: None,
                        _sck_stream: Some(sck_stream),
                        stop_flag,
                        tone_thread: None,
                        device_name: "macOS ScreenCaptureKit System Audio".to_string(),
                        sample_rate: 48_000,
                        channels: target_channels,
                        peak_bits,
                    });
                }
                Err(recovered_producer) => {
                    tracing::warn!(
                        "ScreenCaptureKit stream start failed; falling back to CoreAudio loopback/input"
                    );
                    producer = recovered_producer;
                }
            }
        } else {
            tracing::warn!(
                "macOS Screen/Audio Capture TCC permission not yet granted; falling back to CoreAudio loopback/input (grant permission in System Settings > Privacy & Security > Screen & System Audio Recording)"
            );
        }
    }

    #[cfg(not(target_os = "windows"))]
    {
        let host = cpal::default_host();
        let (device, device_name, supported_cfg) =
            select_capture_device(&host, source, device_name_filter)?;

        let sample_format = supported_cfg.sample_format();
        let config: cpal::StreamConfig = supported_cfg.into();
        let hw_sample_rate = config.sample_rate.0;
        let hw_channels = config.channels;

        let producer_slot = Arc::new(Mutex::new(Some(producer)));

        let mut build_res = try_build_cpal_input_stream(
            &device,
            &config,
            sample_format,
            hw_channels,
            target_channels,
            &producer_slot,
            &peak_bits,
        );

        if matches!(
            build_res,
            Err(AudioError::BuildStream(
                cpal::BuildStreamError::StreamConfigNotSupported
            )) | Err(AudioError::UnsupportedSampleFormat(_))
        ) && sample_format != SampleFormat::I16
        {
            if let Ok(fallback_stream) = try_build_cpal_input_stream(
                &device,
                &config,
                SampleFormat::I16,
                hw_channels,
                target_channels,
                &producer_slot,
                &peak_bits,
            ) {
                tracing::info!(
                    "cpal capture on '{device_name}' succeeded using I16 fallback (default was {sample_format:?})"
                );
                build_res = Ok(fallback_stream);
            }
        }

        let stream = build_res?;
        stream.play()?;

        Ok(CaptureStreamHandle {
            _cpal_stream: Some(stream),
            #[cfg(target_os = "macos")]
            _sck_stream: None,
            stop_flag,
            tone_thread: None,
            device_name,
            sample_rate: hw_sample_rate,
            channels: target_channels,
            peak_bits,
        })
    }
}

#[cfg(not(target_os = "windows"))]
fn try_build_cpal_input_stream(
    device: &cpal::Device,
    config: &cpal::StreamConfig,
    sample_format: SampleFormat,
    hw_channels: u16,
    target_channels: u16,
    producer_slot: &Arc<Mutex<Option<Producer<f32>>>>,
    peak_bits: &Arc<AtomicU32>,
) -> Result<cpal::Stream, AudioError> {
    let slot = Arc::clone(producer_slot);
    let peak_clone = Arc::clone(peak_bits);
    let err_fn = |err| {
        tracing::error!("Audio capture stream callback error: {err}");
    };

    let stream = match sample_format {
        SampleFormat::F32 => {
            let mut local_prod: Option<Producer<f32>> = None;
            device.build_input_stream(
                config,
                move |data: &[f32], _: &cpal::InputCallbackInfo| {
                    if local_prod.is_none() {
                        local_prod = slot.lock().ok().and_then(|mut g| g.take());
                    }
                    if let Some(ref mut prod) = local_prod {
                        push_converted_samples(
                            data,
                            hw_channels,
                            target_channels,
                            prod,
                            &peak_clone,
                        );
                    }
                },
                err_fn,
                None,
            )?
        }
        SampleFormat::I16 => {
            let mut local_prod: Option<Producer<f32>> = None;
            device.build_input_stream(
                config,
                move |data: &[i16], _: &cpal::InputCallbackInfo| {
                    if local_prod.is_none() {
                        local_prod = slot.lock().ok().and_then(|mut g| g.take());
                    }
                    if let Some(ref mut prod) = local_prod {
                        push_converted_i16(data, hw_channels, target_channels, prod, &peak_clone);
                    }
                },
                err_fn,
                None,
            )?
        }
        SampleFormat::I32 => {
            let mut local_prod: Option<Producer<f32>> = None;
            device.build_input_stream(
                config,
                move |data: &[i32], _: &cpal::InputCallbackInfo| {
                    if local_prod.is_none() {
                        local_prod = slot.lock().ok().and_then(|mut g| g.take());
                    }
                    if let Some(ref mut prod) = local_prod {
                        push_converted_i32(data, hw_channels, target_channels, prod, &peak_clone);
                    }
                },
                err_fn,
                None,
            )?
        }
        SampleFormat::U16 => {
            let mut local_prod: Option<Producer<f32>> = None;
            device.build_input_stream(
                config,
                move |data: &[u16], _: &cpal::InputCallbackInfo| {
                    if local_prod.is_none() {
                        local_prod = slot.lock().ok().and_then(|mut g| g.take());
                    }
                    if let Some(ref mut prod) = local_prod {
                        push_converted_u16(data, hw_channels, target_channels, prod, &peak_clone);
                    }
                },
                err_fn,
                None,
            )?
        }
        other => return Err(AudioError::UnsupportedSampleFormat(other)),
    };
    Ok(stream)
}

#[cfg(not(target_os = "windows"))]
fn select_capture_device(
    host: &cpal::Host,
    source: CaptureSource,
    name_filter: Option<&str>,
) -> Result<(cpal::Device, String, cpal::SupportedStreamConfig), AudioError> {
    match source {
        CaptureSource::SystemLoopback => {
            #[cfg(target_os = "macos")]
            {
                use crate::device::macos_coreaudio::{
                    default_cpal_device_with_name, find_cpal_device_by_name,
                };

                if let Some(filter) = name_filter {
                    let filter_lower = filter.to_lowercase();
                    if let Some((dev, name)) = find_cpal_device_by_name(host, true, |n| {
                        n.to_lowercase().contains(&filter_lower)
                    }) {
                        let cfg = dev.default_input_config()?;
                        return Ok((dev, name, cfg));
                    }
                }

                for keyword in ["blackhole", "loopback", "soundflower", "monitor"] {
                    if let Some((dev, name)) = find_cpal_device_by_name(host, true, |n| {
                        let lower = n.to_lowercase();
                        lower.contains(keyword) && !lower.contains("rsrelay virtual mic")
                    }) {
                        let cfg = dev.default_input_config()?;
                        return Ok((dev, name, cfg));
                    }
                }

                let (dev, name) = default_cpal_device_with_name(host, true).ok_or_else(|| {
                    AudioError::DeviceNotFound(
                        "macOS loopback or default input device".to_string(),
                    )
                })?;
                let cfg = dev.default_input_config()?;
                return Ok((dev, name, cfg));
            }

            #[cfg(not(target_os = "macos"))]
            {
                if let Some(filter) = name_filter {
                    let filter_lower = filter.to_lowercase();
                    if let Some(dev) = host.input_devices()?.find(|d| {
                        d.name()
                            .map(|n| n.to_lowercase().contains(&filter_lower))
                            .unwrap_or(false)
                    }) {
                        let name = dev.name().unwrap_or_else(|_| "Default Device".to_string());
                        let cfg = dev.default_input_config()?;
                        return Ok((dev, name, cfg));
                    }
                }

                for keyword in ["blackhole", "loopback", "soundflower", "monitor"] {
                    if let Some(dev) = host.input_devices()?.find(|d| {
                        d.name()
                            .map(|n| {
                                let lower = n.to_lowercase();
                                lower.contains(keyword) && !lower.contains("rsrelay virtual mic")
                            })
                            .unwrap_or(false)
                    }) {
                        let name = dev.name().unwrap_or_else(|_| "Default Device".to_string());
                        let cfg = dev.default_input_config()?;
                        return Ok((dev, name, cfg));
                    }
                }

                let dev = host.default_input_device().ok_or_else(|| {
                    AudioError::DeviceNotFound("loopback or default input device".to_string())
                })?;
                let name = dev.name().unwrap_or_else(|_| "Default Device".to_string());
                let cfg = dev.default_input_config()?;
                Ok((dev, name, cfg))
            }
        }
        CaptureSource::Microphone => {
            #[cfg(target_os = "macos")]
            {
                use crate::device::macos_coreaudio::{
                    default_cpal_device_with_name, find_cpal_device_by_name,
                };

                let (dev, dev_name) = if let Some(filter) = name_filter {
                    let filter_lower = filter.to_lowercase();
                    find_cpal_device_by_name(host, true, |n| {
                        n.to_lowercase().contains(&filter_lower)
                    })
                    .ok_or_else(|| AudioError::DeviceNotFound(filter.to_string()))?
                } else {
                    let default_opt = default_cpal_device_with_name(host, true);
                    let default_is_virtual = default_opt
                        .as_ref()
                        .map(|(_, n)| crate::device::is_virtual_mic_input_device_name(n))
                        .unwrap_or(false);

                    if !default_is_virtual {
                        default_opt.ok_or_else(|| {
                            AudioError::DeviceNotFound("default microphone input".to_string())
                        })?
                    } else if let Some(phys) = find_cpal_device_by_name(host, true, |n| {
                        !crate::device::is_virtual_mic_input_device_name(n)
                    }) {
                        phys
                    } else {
                        default_opt.ok_or_else(|| {
                            AudioError::DeviceNotFound("default microphone input".to_string())
                        })?
                    }
                };
                let cfg = dev.default_input_config()?;
                return Ok((dev, dev_name, cfg));
            }

            #[cfg(not(target_os = "macos"))]
            {
                let dev = if let Some(filter) = name_filter {
                    let filter_lower = filter.to_lowercase();
                    host.input_devices()?
                        .find(|d| {
                            d.name()
                                .map(|n| n.to_lowercase().contains(&filter_lower))
                                .unwrap_or(false)
                        })
                        .ok_or_else(|| AudioError::DeviceNotFound(filter.to_string()))?
                } else {
                    let default_dev = host.default_input_device();
                    let default_is_virtual = default_dev
                        .as_ref()
                        .and_then(|d| d.name().ok())
                        .map(|n| crate::device::is_virtual_mic_input_device_name(&n))
                        .unwrap_or(false);

                    if !default_is_virtual {
                        if let Some(d) = default_dev {
                            d
                        } else {
                            return Err(AudioError::DeviceNotFound(
                                "default microphone input".to_string(),
                            ));
                        }
                    } else if let Some(phys) = host.input_devices()?.find(|d| {
                        d.name()
                            .map(|n| !crate::device::is_virtual_mic_input_device_name(&n))
                            .unwrap_or(false)
                    }) {
                        phys
                    } else {
                        default_dev.ok_or_else(|| {
                            AudioError::DeviceNotFound("default microphone input".to_string())
                        })?
                    }
                };
                let dev_name = dev.name().unwrap_or_else(|_| "Default Device".to_string());
                let cfg = dev.default_input_config()?;
                Ok((dev, dev_name, cfg))
            }
        }
        CaptureSource::SyntheticTone => unreachable!("handled above"),
    }
}

#[cfg(not(target_os = "windows"))]
#[inline]
fn push_converted_samples(
    data: &[f32],
    hw_channels: u16,
    target_channels: u16,
    producer: &mut Producer<f32>,
    peak_bits: &AtomicU32,
) {
    let hw_ch = (hw_channels as usize).max(1);
    let tgt_ch = (target_channels as usize).max(1);
    let frames = data.len() / hw_ch;
    let available_frames = producer.slots() / tgt_ch;
    let frames_to_write = frames.min(available_frames);
    let samples_to_write = frames_to_write * tgt_ch;

    if samples_to_write == 0 {
        return;
    }

    let mut max_abs = 0.0f32;
    if let Ok(mut chunk) = producer.write_chunk(samples_to_write) {
        let (first, second) = chunk.as_mut_slices();
        let mut out_idx = 0;

        for f in 0..frames_to_write {
            let base = f * hw_ch;
            for c in 0..tgt_ch {
                let sample = if tgt_ch == 1 && hw_ch >= 2 {
                    (data[base] + data[base + 1]) * 0.5
                } else {
                    data[base + c.min(hw_ch - 1)]
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
        chunk.commit_all();
        peak_bits.store(max_abs.to_bits(), Ordering::Relaxed);
    }
}

#[cfg(not(target_os = "windows"))]
#[inline]
fn push_converted_i16(
    data: &[i16],
    hw_channels: u16,
    target_channels: u16,
    producer: &mut Producer<f32>,
    peak_bits: &AtomicU32,
) {
    let hw_ch = (hw_channels as usize).max(1);
    let tgt_ch = (target_channels as usize).max(1);
    let frames = data.len() / hw_ch;
    let frames_to_write = frames.min(producer.slots() / tgt_ch);
    let samples_to_write = frames_to_write * tgt_ch;

    if samples_to_write == 0 {
        return;
    }

    let mut max_abs = 0.0f32;
    if let Ok(mut chunk) = producer.write_chunk(samples_to_write) {
        let (first, second) = chunk.as_mut_slices();
        let mut out_idx = 0;
        for f in 0..frames_to_write {
            let base = f * hw_ch;
            for c in 0..tgt_ch {
                let sample = if tgt_ch == 1 && hw_ch >= 2 {
                    ((data[base] as f32) + (data[base + 1] as f32)) / 65536.0
                } else {
                    (data[base + c.min(hw_ch - 1)] as f32) / 32768.0
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
        chunk.commit_all();
        peak_bits.store(max_abs.to_bits(), Ordering::Relaxed);
    }
}

#[cfg(not(target_os = "windows"))]
#[inline]
fn push_converted_i32(
    data: &[i32],
    hw_channels: u16,
    target_channels: u16,
    producer: &mut Producer<f32>,
    peak_bits: &AtomicU32,
) {
    let hw_ch = (hw_channels as usize).max(1);
    let tgt_ch = (target_channels as usize).max(1);
    let frames = data.len() / hw_ch;
    let frames_to_write = frames.min(producer.slots() / tgt_ch);
    let samples_to_write = frames_to_write * tgt_ch;

    if samples_to_write == 0 {
        return;
    }

    let mut max_abs = 0.0f32;
    if let Ok(mut chunk) = producer.write_chunk(samples_to_write) {
        let (first, second) = chunk.as_mut_slices();
        let mut out_idx = 0;
        for f in 0..frames_to_write {
            let base = f * hw_ch;
            for c in 0..tgt_ch {
                let sample = if tgt_ch == 1 && hw_ch >= 2 {
                    ((data[base] as f64) + (data[base + 1] as f64)) as f32 / 4_294_967_296.0
                } else {
                    (data[base + c.min(hw_ch - 1)] as f32) / 2_147_483_648.0
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
        chunk.commit_all();
        peak_bits.store(max_abs.to_bits(), Ordering::Relaxed);
    }
}

#[cfg(not(target_os = "windows"))]
#[inline]
fn push_converted_u16(
    data: &[u16],
    hw_channels: u16,
    target_channels: u16,
    producer: &mut Producer<f32>,
    peak_bits: &AtomicU32,
) {
    let hw_ch = (hw_channels as usize).max(1);
    let tgt_ch = (target_channels as usize).max(1);
    let frames = data.len() / hw_ch;
    let frames_to_write = frames.min(producer.slots() / tgt_ch);
    let samples_to_write = frames_to_write * tgt_ch;

    if samples_to_write == 0 {
        return;
    }

    let mut max_abs = 0.0f32;
    if let Ok(mut chunk) = producer.write_chunk(samples_to_write) {
        let (first, second) = chunk.as_mut_slices();
        let mut out_idx = 0;
        for f in 0..frames_to_write {
            let base = f * hw_ch;
            for c in 0..tgt_ch {
                let sample = if tgt_ch == 1 && hw_ch >= 2 {
                    let s0 = ((data[base] as f32) - 32768.0) / 32768.0;
                    let s1 = ((data[base + 1] as f32) - 32768.0) / 32768.0;
                    (s0 + s1) * 0.5
                } else {
                    ((data[base + c.min(hw_ch - 1)] as f32) - 32768.0) / 32768.0
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
        chunk.commit_all();
        peak_bits.store(max_abs.to_bits(), Ordering::Relaxed);
    }
}
