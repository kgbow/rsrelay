//! `rsrelay-audio`: Cross-platform lock-free audio capture and playback abstraction.

pub mod capture;
pub mod device;
pub mod macos_sck;
pub mod macos_virtual_mic;
pub mod playback;
pub mod windows_loopback;
pub mod windows_virtual_mic;

pub use capture::{start_capture_stream, CaptureStreamHandle};
pub use device::{
    is_virtual_mic_input_device_name, is_virtual_mic_output_sink_name, list_audio_devices,
    select_default_virtual_mic_output_device, select_virtual_mic_output_from_devices,
    AudioDeviceInfo, AudioError, CaptureSource,
};
pub use macos_sck::interleave_planar_to_ringbuffer;
pub use macos_virtual_mic::{
    install_macos_virtual_mic_driver, is_macos_rsrelay_driver_installed,
    launch_windows_vbcable_install, MACOS_RSRELAY_VMIC_INPUT_NAME, MACOS_RSRELAY_VMIC_SINK_NAME,
};
pub use playback::{start_playback_stream, PlaybackStreamHandle};
pub use windows_virtual_mic::{
    is_audiorelay_backend_running, is_null_virtual_mic_filter, is_virtual_mic_driver_available,
    NULL_VIRTUAL_MIC_DEVICE_NAME, VIRTUAL_MIC_OUTPUT_DEVICE_NAME,
};

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
    fn timeEndPeriod(uPeriod: u32) -> u32;
}

/// RAII guard returned by [`promote_worker_thread_priority`] that restores both
/// OS real-time thread priority (e.g. Windows MMCSS `AvRevertMmThreadCharacteristics`)
/// and Windows `winmm` 1ms timer resolution (`timeEndPeriod(1)`) when the worker thread exits.
pub struct WorkerPriorityGuard {
    #[cfg(target_os = "windows")]
    timer_period_active: bool,
    rt_handle: Option<audio_thread_priority::RtPriorityHandle>,
}

impl Drop for WorkerPriorityGuard {
    fn drop(&mut self) {
        if let Some(handle) = self.rt_handle.take() {
            let _ = audio_thread_priority::demote_current_thread_from_real_time(handle);
        }
        #[cfg(target_os = "windows")]
        if self.timer_period_active {
            unsafe {
                let _ = timeEndPeriod(1);
            }
        }
    }
}

/// Promote the calling worker thread (encoder/packetizer or decoder/resampler) to OS real-time priority.
///
/// Uses Windows MMCSS (`AvSetMmThreadCharacteristics`) + `timeBeginPeriod(1)` 1ms kernel timer resolution,
/// or macOS Mach `THREAD_TIME_CONSTRAINT_POLICY`. Keep the returned [`WorkerPriorityGuard`] alive on the
/// worker thread stack so OS handles and timer resolution are cleanly released on thread exit.
#[must_use = "keep WorkerPriorityGuard alive for the duration of the real-time worker loop"]
pub fn promote_worker_thread_priority(sample_rate: u32, buffer_frames: u32) -> WorkerPriorityGuard {
    #[cfg(target_os = "windows")]
    let timer_period_active = unsafe {
        // Ensure 1ms scheduler timer resolution on Windows 10/11 for 5ms UDP packet pacing
        timeBeginPeriod(1) == 0
    };

    let rt_handle =
        match audio_thread_priority::promote_current_thread_to_real_time(buffer_frames, sample_rate)
        {
            Ok(handle) => Some(handle),
            Err(err) => {
                tracing::debug!("Could not promote worker thread to real-time priority: {err:?}");
                None
            }
        };

    WorkerPriorityGuard {
        #[cfg(target_os = "windows")]
        timer_period_active,
        rt_handle,
    }
}
