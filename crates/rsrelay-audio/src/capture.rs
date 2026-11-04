use std::sync::{
    atomic::{AtomicBool, AtomicU32, Ordering},
    Arc,
};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::SampleFormat;
use rtrb::Producer;

use crate::device::{AudioError, CaptureSource};
use crate::windows_loopback::SilentKeepaliveStream;

/// Active audio capture stream handle.
///
/// Dropping this handle stops the underlying OS audio stream or synthetic generator thread.
pub struct CaptureStreamHandle {
    _cpal_stream: Option<cpal::Stream>,
    #[cfg(target_os = "macos")]
    _sck_stream: Option<crate::macos_sck::MacosSckStream>,
    _keepalive: Option<SilentKeepaliveStream>,
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
    mut producer: Producer<f32>,
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
            _cpal_stream: None,
            #[cfg(target_os = "macos")]
            _sck_stream: None,
            _keepalive: None,
            stop_flag,
            tone_thread: Some(handle),
            device_name: "Synthetic 440Hz/660Hz Stereo Generator".to_string(),
            sample_rate,
            channels,
            peak_bits,
        });
    }

    // On macOS 13+, prefer native driverless ScreenCaptureKit for SystemLoopback when no explicit device filter is set
    #[cfg(target_os = "macos")]
    if source == CaptureSource::SystemLoopback && device_name_filter.is_none() {
        if crate::macos_sck::has_screen_capture_permission() {
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
                        _keepalive: None,
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
            tracing::info!(
                "macOS Screen/Audio Capture TCC permission not yet granted; falling back to CoreAudio input (grant permission in System Settings > Privacy & Security > Screen & System Audio Recording)"
            );
        }
    }

    let host = cpal::default_host();

    // Select device based on source and platform
    let (device, supported_cfg, is_windows_loopback) = select_capture_device(
        &host,
        source,
        device_name_filter,
    )?;

    let device_name = device.name().unwrap_or_else(|_| "Default Device".to_string());
    let sample_format = supported_cfg.sample_format();
    let config: cpal::StreamConfig = supported_cfg.into();
    let hw_sample_rate = config.sample_rate.0;
    let hw_channels = config.channels;

    // On Windows Loopback, start a silent output keepalive stream so WASAPI never stalls during silence
    let keepalive = if is_windows_loopback {
        SilentKeepaliveStream::start(&device).ok()
    } else {
        None
    };

    let peak_clone = Arc::clone(&peak_bits);
    let err_fn = |err| {
        tracing::error!("Audio capture stream callback error: {err}");
    };

    let stream = match sample_format {
        SampleFormat::F32 => device.build_input_stream(
            &config,
            move |data: &[f32], _: &cpal::InputCallbackInfo| {
                push_converted_samples(data, hw_channels, target_channels, &mut producer, &peak_clone);
            },
            err_fn,
            None,
        )?,
        SampleFormat::I16 => device.build_input_stream(
            &config,
            move |data: &[i16], _: &cpal::InputCallbackInfo| {
                push_converted_i16(data, hw_channels, target_channels, &mut producer, &peak_clone);
            },
            err_fn,
            None,
        )?,
        SampleFormat::U16 => device.build_input_stream(
            &config,
            move |data: &[u16], _: &cpal::InputCallbackInfo| {
                push_converted_u16(data, hw_channels, target_channels, &mut producer, &peak_clone);
            },
            err_fn,
            None,
        )?,
        other => return Err(AudioError::UnsupportedSampleFormat(other)),
    };

    stream.play()?;

    Ok(CaptureStreamHandle {
        _cpal_stream: Some(stream),
        #[cfg(target_os = "macos")]
        _sck_stream: None,
        _keepalive: keepalive,
        stop_flag,
        tone_thread: None,
        device_name,
        sample_rate: hw_sample_rate,
        channels: target_channels,
        peak_bits,
    })
}

fn select_capture_device(
    host: &cpal::Host,
    source: CaptureSource,
    name_filter: Option<&str>,
) -> Result<(cpal::Device, cpal::SupportedStreamConfig, bool), AudioError> {
    match source {
        CaptureSource::SystemLoopback => {
            #[cfg(target_os = "windows")]
            {
                // On Windows WASAPI, building an input stream on an Output Device enables AUDCLNT_STREAMFLAGS_LOOPBACK
                let device = if let Some(filter) = name_filter {
                    let filter_lower = filter.to_lowercase();
                    host.output_devices()?
                        .find(|d| {
                            d.name()
                                .map(|n| n.to_lowercase().contains(&filter_lower))
                                .unwrap_or(false)
                        })
                        .ok_or_else(|| AudioError::DeviceNotFound(filter.to_string()))?
                } else {
                    host.default_output_device().ok_or_else(|| {
                        AudioError::DeviceNotFound("default Windows output device".to_string())
                    })?
                };
                let cfg = device.default_output_config()?;
                return Ok((device, cfg, true));
            }

            #[cfg(not(target_os = "windows"))]
            {
                // On macOS / Unix: look for a loopback device (e.g., BlackHole, Loopback, Soundflower)
                // or match the user's filter, falling back to default input device
                if let Some(filter) = name_filter {
                    let filter_lower = filter.to_lowercase();
                    if let Some(dev) = host.input_devices()?.find(|d| {
                        d.name()
                            .map(|n| n.to_lowercase().contains(&filter_lower))
                            .unwrap_or(false)
                    }) {
                        let cfg = dev.default_input_config()?;
                        return Ok((dev, cfg, false));
                    }
                }

                for keyword in ["blackhole", "loopback", "soundflower", "virtual", "monitor"] {
                    if let Some(dev) = host.input_devices()?.find(|d| {
                        d.name()
                            .map(|n| n.to_lowercase().contains(keyword))
                            .unwrap_or(false)
                    }) {
                        let cfg = dev.default_input_config()?;
                        return Ok((dev, cfg, false));
                    }
                }

                let dev = host.default_input_device().ok_or_else(|| {
                    AudioError::DeviceNotFound(
                        "macOS loopback or default input device".to_string(),
                    )
                })?;
                let cfg = dev.default_input_config()?;
                Ok((dev, cfg, false))
            }
        }
        CaptureSource::Microphone => {
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
                host.default_input_device().ok_or_else(|| {
                    AudioError::DeviceNotFound("default microphone input".to_string())
                })?
            };
            let cfg = dev.default_input_config()?;
            Ok((dev, cfg, false))
        }
        CaptureSource::SyntheticTone => unreachable!("handled above"),
    }
}

/// Wait-free conversion and ringbuffer insertion for `f32` hardware buffers.
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
