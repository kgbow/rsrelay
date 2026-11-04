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

/// Check whether OS-level permission for native system audio loopback is currently granted.
///
/// - **macOS 13+**: Checks `ScreenCaptureKit` TCC permission (`CGPreflightScreenCaptureAccess`).
/// - **Windows 10/11**: Always returns `true` (WASAPI Loopback does not require extra TCC permissions).
pub fn has_system_loopback_permission() -> bool {
    #[cfg(target_os = "macos")]
    {
        macos_sck::has_screen_capture_permission()
    }
    #[cfg(not(target_os = "macos"))]
    {
        true
    }
}

/// Request OS-level permission for native system audio loopback if not yet granted.
///
/// - **macOS 13+**: Triggers the system TCC prompt (`CGRequestScreenCaptureAccess`) and opens
///   *System Settings > Privacy & Security > Screen & System Audio Recording* so the user can
///   toggle the switch directly.
/// - **Windows 10/11**: Always returns `true`.
pub fn request_system_loopback_permission() -> bool {
    #[cfg(target_os = "macos")]
    {
        let granted = macos_sck::request_screen_capture_permission();
        if !granted {
            let _ = std::process::Command::new("open")
                .arg("x-apple.systempreferences:com.apple.preference.security?Privacy_ScreenCapture")
                .spawn();
        }
        granted
    }
    #[cfg(not(target_os = "macos"))]
    {
        true
    }
}

#[cfg(target_os = "windows")]
#[link(name = "winmm")]
extern "system" {
    fn timeBeginPeriod(uPeriod: u32) -> u32;
}

/// Promote the calling worker thread (encoder/packetizer or decoder/resampler) to OS real-time priority.
///
/// Uses Windows MMCSS (`"Pro Audio"`) + `timeBeginPeriod(1)` 1ms kernel timer resolution,
/// or macOS Mach `THREAD_TIME_CONSTRAINT_POLICY`.
pub fn promote_worker_thread_priority(sample_rate: u32, buffer_frames: u32) {
    #[cfg(target_os = "windows")]
    unsafe {
        // Ensure 1ms scheduler timer resolution on Windows 10/11 for 5ms UDP packet pacing
        let _ = timeBeginPeriod(1);
    }

    if let Err(err) =
        audio_thread_priority::promote_current_thread_to_real_time(buffer_frames, sample_rate)
    {
        tracing::debug!("Could not promote worker thread to real-time priority: {err:?}");
    }
}
