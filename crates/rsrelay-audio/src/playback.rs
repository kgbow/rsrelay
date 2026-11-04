use std::sync::{
    atomic::{AtomicU32, AtomicU64, Ordering},
    Arc,
};

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::SampleFormat;
use rtrb::Consumer;

use crate::device::AudioError;

/// Active audio playback stream handle.
pub struct PlaybackStreamHandle {
    _cpal_stream: cpal::Stream,
    pub device_name: String,
    pub sample_rate: u32,
    pub channels: u16,
    volume_bits: Arc<AtomicU32>,
    peak_bits: Arc<AtomicU32>,
    underrun_count: Arc<AtomicU64>,
}

impl PlaybackStreamHandle {
    /// Dynamically adjust output volume (`0.0..=2.0`) without locking the real-time audio callback.
    #[inline]
    pub fn set_volume(&self, gain: f32) {
        self.volume_bits
            .store(gain.clamp(0.0, 2.0).to_bits(), Ordering::Relaxed);
    }

    #[inline]
    pub fn volume(&self) -> f32 {
        f32::from_bits(self.volume_bits.load(Ordering::Relaxed))
    }

    #[inline]
    pub fn peak_level(&self) -> f32 {
        f32::from_bits(self.peak_bits.load(Ordering::Relaxed))
    }

    #[inline]
    pub fn underrun_count(&self) -> u64 {
        self.underrun_count.load(Ordering::Relaxed)
    }
}

/// Start a real-time audio playback stream pulling `source_channels` interleaved `f32` samples
/// from `consumer`.
pub fn start_playback_stream(
    device_name_filter: Option<&str>,
    source_channels: u16,
    mut consumer: Consumer<f32>,
) -> Result<PlaybackStreamHandle, AudioError> {
    let host = cpal::default_host();
    let device = if let Some(filter) = device_name_filter {
        let filter_lower = filter.to_lowercase();
        host.output_devices()?
            .find(|d| {
                d.name()
                    .map(|n| n.to_lowercase().contains(&filter_lower))
                    .unwrap_or(false)
            })
            .ok_or_else(|| AudioError::DeviceNotFound(filter.to_string()))?
    } else {
        host.default_output_device()
            .ok_or_else(|| AudioError::DeviceNotFound("default output device".to_string()))?
    };

    let device_name = device.name().unwrap_or_else(|_| "Default Output".to_string());
    let supported_cfg = device.default_output_config()?;
    let sample_format = supported_cfg.sample_format();
    let config: cpal::StreamConfig = supported_cfg.into();
    let hw_sample_rate = config.sample_rate.0;
    let hw_channels = config.channels;

    let volume_bits = Arc::new(AtomicU32::new(1.0f32.to_bits()));
    let peak_bits = Arc::new(AtomicU32::new(0.0f32.to_bits()));
    let underrun_count = Arc::new(AtomicU64::new(0));

    let vol_clone = Arc::clone(&volume_bits);
    let peak_clone = Arc::clone(&peak_bits);
    let underrun_clone = Arc::clone(&underrun_count);

    let err_fn = |err| {
        tracing::error!("Audio playback stream callback error: {err}");
    };

    let stream = match sample_format {
        SampleFormat::F32 => device.build_output_stream(
            &config,
            move |out: &mut [f32], _: &cpal::OutputCallbackInfo| {
                pull_samples_f32(
                    out,
                    hw_channels,
                    source_channels,
                    &mut consumer,
                    &vol_clone,
                    &peak_clone,
                    &underrun_clone,
                );
            },
            err_fn,
            None,
        )?,
        SampleFormat::I16 => device.build_output_stream(
            &config,
            move |out: &mut [i16], _: &cpal::OutputCallbackInfo| {
                pull_samples_i16(
                    out,
                    hw_channels,
                    source_channels,
                    &mut consumer,
                    &vol_clone,
                    &peak_clone,
                    &underrun_clone,
                );
            },
            err_fn,
            None,
        )?,
        SampleFormat::U16 => device.build_output_stream(
            &config,
            move |out: &mut [u16], _: &cpal::OutputCallbackInfo| {
                pull_samples_u16(
                    out,
                    hw_channels,
                    source_channels,
                    &mut consumer,
                    &vol_clone,
                    &peak_clone,
                    &underrun_clone,
                );
            },
            err_fn,
            None,
        )?,
        other => return Err(AudioError::UnsupportedSampleFormat(other)),
    };

    stream.play()?;

    Ok(PlaybackStreamHandle {
        _cpal_stream: stream,
        device_name,
        sample_rate: hw_sample_rate,
        channels: hw_channels,
        volume_bits,
        peak_bits,
        underrun_count,
    })
}

#[inline]
fn read_ring_sample(first: &[f32], second: &[f32], idx: usize) -> f32 {
    if idx < first.len() {
        first[idx]
    } else {
        second[idx - first.len()]
    }
}

#[inline]
fn sample_for_hw_channel(
    first: &[f32],
    second: &[f32],
    frame: usize,
    hw_c: usize,
    hw_ch: usize,
    src_ch: usize,
) -> f32 {
    let base = frame * src_ch;
    if hw_ch == 1 && src_ch >= 2 {
        (read_ring_sample(first, second, base) + read_ring_sample(first, second, base + 1)) * 0.5
    } else {
        read_ring_sample(first, second, base + hw_c.min(src_ch - 1))
    }
}

#[inline]
fn pull_samples_f32(
    out: &mut [f32],
    hw_channels: u16,
    src_channels: u16,
    consumer: &mut Consumer<f32>,
    vol_bits: &AtomicU32,
    peak_bits: &AtomicU32,
    underrun_count: &AtomicU64,
) {
    let hw_ch = (hw_channels as usize).max(1);
    let src_ch = (src_channels as usize).max(1);
    let total_frames = out.len() / hw_ch;
    let available_frames = consumer.slots() / src_ch;
    let frames_to_read = total_frames.min(available_frames);
    let samples_to_read = frames_to_read * src_ch;

    let gain = f32::from_bits(vol_bits.load(Ordering::Relaxed));
    let mut max_abs = 0.0f32;

    if samples_to_read > 0 {
        if let Ok(chunk) = consumer.read_chunk(samples_to_read) {
            let (first, second) = chunk.as_slices();
            for f in 0..frames_to_read {
                for c in 0..hw_ch {
                    let raw = sample_for_hw_channel(first, second, f, c, hw_ch, src_ch);
                    let val = (raw * gain).clamp(-1.0, 1.0);
                    let abs = val.abs();
                    if abs > max_abs {
                        max_abs = abs;
                    }
                    out[f * hw_ch + c] = val;
                }
            }
            chunk.commit_all();
        }
    }

    if frames_to_read < total_frames {
        out[frames_to_read * hw_ch..].fill(0.0);
        underrun_count.fetch_add(1, Ordering::Relaxed);
    }

    peak_bits.store(max_abs.to_bits(), Ordering::Relaxed);
}

#[inline]
fn pull_samples_i16(
    out: &mut [i16],
    hw_channels: u16,
    src_channels: u16,
    consumer: &mut Consumer<f32>,
    vol_bits: &AtomicU32,
    peak_bits: &AtomicU32,
    underrun_count: &AtomicU64,
) {
    let hw_ch = (hw_channels as usize).max(1);
    let src_ch = (src_channels as usize).max(1);
    let total_frames = out.len() / hw_ch;
    let frames_to_read = total_frames.min(consumer.slots() / src_ch);
    let samples_to_read = frames_to_read * src_ch;
    let gain = f32::from_bits(vol_bits.load(Ordering::Relaxed));
    let mut max_abs = 0.0f32;

    if samples_to_read > 0 {
        if let Ok(chunk) = consumer.read_chunk(samples_to_read) {
            let (first, second) = chunk.as_slices();
            for f in 0..frames_to_read {
                for c in 0..hw_ch {
                    let raw = sample_for_hw_channel(first, second, f, c, hw_ch, src_ch);
                    let val = (raw * gain).clamp(-1.0, 1.0);
                    let abs = val.abs();
                    if abs > max_abs {
                        max_abs = abs;
                    }
                    out[f * hw_ch + c] = (val * 32767.0) as i16;
                }
            }
            chunk.commit_all();
        }
    }

    if frames_to_read < total_frames {
        out[frames_to_read * hw_ch..].fill(0);
        underrun_count.fetch_add(1, Ordering::Relaxed);
    }

    peak_bits.store(max_abs.to_bits(), Ordering::Relaxed);
}

#[inline]
fn pull_samples_u16(
    out: &mut [u16],
    hw_channels: u16,
    src_channels: u16,
    consumer: &mut Consumer<f32>,
    vol_bits: &AtomicU32,
    peak_bits: &AtomicU32,
    underrun_count: &AtomicU64,
) {
    let hw_ch = (hw_channels as usize).max(1);
    let src_ch = (src_channels as usize).max(1);
    let total_frames = out.len() / hw_ch;
    let frames_to_read = total_frames.min(consumer.slots() / src_ch);
    let samples_to_read = frames_to_read * src_ch;
    let gain = f32::from_bits(vol_bits.load(Ordering::Relaxed));
    let mut max_abs = 0.0f32;

    if samples_to_read > 0 {
        if let Ok(chunk) = consumer.read_chunk(samples_to_read) {
            let (first, second) = chunk.as_slices();
            for f in 0..frames_to_read {
                for c in 0..hw_ch {
                    let raw = sample_for_hw_channel(first, second, f, c, hw_ch, src_ch);
                    let val = (raw * gain).clamp(-1.0, 1.0);
                    let abs = val.abs();
                    if abs > max_abs {
                        max_abs = abs;
                    }
                    out[f * hw_ch + c] = ((val * 32767.0) + 32768.0) as u16;
                }
            }
            chunk.commit_all();
        }
    }

    if frames_to_read < total_frames {
        out[frames_to_read * hw_ch..].fill(32768);
        underrun_count.fetch_add(1, Ordering::Relaxed);
    }

    peak_bits.store(max_abs.to_bits(), Ordering::Relaxed);
}
