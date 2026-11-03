//! `rsrelay-audio`: Cross-platform lock-free audio capture and playback abstraction.

pub mod capture;
pub mod device;
pub mod macos_sck;
pub mod playback;
pub mod windows_loopback;

pub use capture::{start_capture_stream, CaptureStreamHandle};
pub use device::{list_audio_devices, AudioDeviceInfo, AudioError, CaptureSource};
pub use macos_sck::interleave_planar_to_ringbuffer;
pub use playback::{start_playback_stream, PlaybackStreamHandle};

/// Promote the calling worker thread (encoder/packetizer or decoder/resampler) to OS real-time priority.
///
/// Uses Windows MMCSS (`"Pro Audio"`) or macOS Mach `THREAD_TIME_CONSTRAINT_POLICY`.
pub fn promote_worker_thread_priority(sample_rate: u32, buffer_frames: u32) {
    if let Err(err) =
        audio_thread_priority::promote_current_thread_to_real_time(buffer_frames, sample_rate)
    {
        tracing::debug!("Could not promote worker thread to real-time priority: {err:?}");
    }
}
