//! Native Windows `Virtual Mic` (`\\.\AudioRelayVirtualMicDeviceLink`) high-resolution kernel bridge
//! and cross-platform Echo-Free Null Virtual Mic guard sink.
//!
//! ### Key Design Points
//! 1. **Sub-millisecond High-Resolution Waitable Timer (`CREATE_WAITABLE_TIMER_HIGH_RESOLUTION`)**:
//!    Avoids the Windows 10/11 default `15.625 ms` `thread::sleep()` timer granularity trap that
//!    previously starved the kernel ringbuffer to ~32% of 48 kHz nominal rate.
//! 2. **Wall-Clock Catch-Up + Kernel Queue Watermark Closed-Loop Control**:
//!    Uses `Instant` elapsed ticks combined with the live `[capacity_bytes, queued_bytes]` status
//!    returned by `IOCTL 0x00223074` to hold the kernel capture ringbuffer tightly around `~20 ms`
//!    (`3,840` bytes @ `192,000 B/s`), preventing both underrun dropouts and latency bloat.
//! 3. **Echo-Free Guard Sink (`NullVirtualMicStream`)**:
//!    When an incoming microphone stream targets a virtual microphone on a machine that has not yet
//!    installed any virtual audio driver (Windows or macOS), `NullVirtualMicStream` consumes the
//!    jitter-buffered audio at exact 48 kHz real-time pace and updates the GUI VU peak meter **without**
//!    ever routing voice to physical speakers (0% `SystemLoopback` echo).

use std::sync::{
    atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering},
    Arc,
};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use rtrb::Consumer;

use crate::device::AudioError;

/// Display name used for the Windows `virtual-mic.sys` kernel bridge output target.
pub const VIRTUAL_MIC_OUTPUT_DEVICE_NAME: &str = "Virtual Mic (Virtual Mic for AudioRelay)";

/// Sentinel output sink name used when Virtual Mic routing is requested but no virtual audio
/// driver is installed yet. Drains incoming voice silently with live VU metering and zero echo.
pub const NULL_VIRTUAL_MIC_DEVICE_NAME: &str = "Virtual Mic (Echo-Free Guard · Driver Needed)";

/// Check whether `filter` refers specifically to the Windows `virtual-mic.sys` kernel bridge.
pub fn is_virtual_mic_device_filter(filter: &str) -> bool {
    let lower = filter.to_ascii_lowercase();
    (lower.contains("virtual mic") || lower.contains("audiorelayvirtualmic"))
        && !lower.contains("rsrelay virtual mic")
        && !lower.contains("echo-free guard")
}

/// Check whether `filter` refers to the Echo-Free Guard null sink.
pub fn is_null_virtual_mic_filter(filter: &str) -> bool {
    let lower = filter.to_ascii_lowercase();
    lower == "__rsrelay_null_vmic__" || lower.contains("echo-free guard")
}

/// Echo-free silent meter sink running at 48 kHz (used when no virtual mic driver is installed yet).
pub struct NullVirtualMicStream {
    stop_flag: Arc<AtomicBool>,
    worker: Option<JoinHandle<()>>,
}

impl NullVirtualMicStream {
    pub fn start(
        source_channels: u16,
        mut consumer: Consumer<f32>,
        volume_bits: Arc<AtomicU32>,
        peak_bits: Arc<AtomicU32>,
        underrun_count: Arc<AtomicU64>,
    ) -> Self {
        let stop_flag = Arc::new(AtomicBool::new(false));
        let stop_clone = Arc::clone(&stop_flag);

        let worker = thread::Builder::new()
            .name("rsrelay-null-vmic-guard".to_string())
            .spawn(move || {
                let src_ch = (source_channels as usize).max(1);
                let frames_per_tick = 240usize; // 5 ms @ 48 kHz
                let samples_per_tick = frames_per_tick * src_ch;
                let stream_start = Instant::now();
                let mut emitted_ticks: u64 = 0;

                while !stop_clone.load(Ordering::Relaxed) {
                    let elapsed_ns = stream_start.elapsed().as_nanos() as u64;
                    let target_ticks = elapsed_ns / 5_000_000;
                    if target_ticks > emitted_ticks {
                        let due = (target_ticks - emitted_ticks).min(8) as usize;
                        emitted_ticks = target_ticks;
                        let gain = f32::from_bits(volume_bits.load(Ordering::Relaxed));
                        for _ in 0..due {
                            let avail = consumer.slots();
                            let to_read = samples_per_tick.min(avail);
                            let mut max_abs = 0.0f32;
                            if to_read > 0 {
                                if let Ok(chunk) = consumer.read_chunk(to_read) {
                                    let (s1, s2) = chunk.as_slices();
                                    for &raw in s1.iter().chain(s2.iter()) {
                                        let v = (raw * gain).clamp(-1.0, 1.0).abs();
                                        if v > max_abs {
                                            max_abs = v;
                                        }
                                    }
                                    chunk.commit_all();
                                }
                            }
                            if to_read < samples_per_tick {
                                underrun_count.fetch_add(1, Ordering::Relaxed);
                            }
                            peak_bits.store(max_abs.to_bits(), Ordering::Relaxed);
                        }
                    }

                    let next_tick_ns = (emitted_ticks + 1) * 5_000_000;
                    let now_ns = stream_start.elapsed().as_nanos() as u64;
                    if next_tick_ns > now_ns {
                        thread::sleep(Duration::from_nanos(next_tick_ns - now_ns));
                    }
                }
            })
            .expect("spawn rsrelay-null-vmic-guard");

        Self {
            stop_flag,
            worker: Some(worker),
        }
    }
}

impl Drop for NullVirtualMicStream {
    fn drop(&mut self) {
        self.stop_flag.store(true, Ordering::Relaxed);
        if let Some(h) = self.worker.take() {
            let _ = h.join();
        }
    }
}

#[cfg(target_os = "windows")]
mod imp {
    use super::*;
    use std::ffi::c_void;
    use std::ptr::{null, null_mut};

    type HANDLE = *mut c_void;
    type BOOL = i32;
    type DWORD = u32;

    const GENERIC_READ: DWORD = 0x8000_0000;
    const GENERIC_WRITE: DWORD = 0x4000_0000;
    const FILE_SHARE_READ: DWORD = 0x0000_0001;
    const FILE_SHARE_WRITE: DWORD = 0x0000_0002;
    const OPEN_EXISTING: DWORD = 3;
    const FILE_ATTRIBUTE_NORMAL: DWORD = 0x0000_0080;
    const INVALID_HANDLE_VALUE: HANDLE = -1isize as HANDLE;

    /// High-resolution waitable timer constants (Windows 10 1803+ / Windows 11).
    const CREATE_WAITABLE_TIMER_HIGH_RESOLUTION: DWORD = 0x0000_0002;
    const TIMER_ALL_ACCESS: DWORD = 0x001F_0003;

    /// Toolhelp32 process snapshot flag for detecting concurrent `audiorelay-backend.exe`.
    const TH32CS_SNAPPROCESS: DWORD = 0x0000_0002;
    const MAX_PATH: usize = 260;

    /// Queries driver protocol version (`u32 == 1`).
    const IOCTL_VIRTUAL_MIC_GET_VERSION: DWORD = 0x0022_2EE4;
    /// Sets the kernel capture ringbuffer capacity in bytes (`u32`) when initializing.
    const IOCTL_VIRTUAL_MIC_SET_CAPACITY: DWORD = 0x0022_3070;
    /// Writes interleaved 48 kHz 16-bit stereo PCM bytes into the kernel capture ringbuffer
    /// and returns `[capacity_bytes: u32, queued_bytes: u32]` (8 bytes).
    const IOCTL_VIRTUAL_MIC_WRITE_PCM: DWORD = 0x0022_3074;

    /// 5 ms frame @ 48,000 Hz = 240 stereo frames = 480 `i16` samples = 960 bytes.
    const FRAMES_PER_TICK: usize = 240;
    const CHANNELS: usize = 2;
    const SAMPLES_PER_TICK: usize = FRAMES_PER_TICK * CHANNELS;
    const BYTES_PER_TICK: DWORD = (SAMPLES_PER_TICK * std::mem::size_of::<i16>()) as DWORD;

    /// Low watermark (10 ms = 1,920 bytes): if active capture pin falls below this and we have
    /// user-space audio ready, push one extra 5 ms frame to prevent WASAPI underrun.
    const LOW_KERNEL_QUEUED_BYTES: u32 = 1_920;
    /// High watermark (45 ms = 8,640 bytes): if kernel ringbuffer exceeds this, pause writing
    /// for a single tick so WASAPI smoothly drains excess latency without dropping samples.
    const HIGH_KERNEL_QUEUED_BYTES: u32 = 8_640;
    /// Desired kernel ringbuffer capacity (100 ms = 19,200 bytes).
    const TARGET_KERNEL_RING_CAPACITY_BYTES: u32 = 19_200;

    #[repr(C)]
    struct ProcessEntry32W {
        dw_size: DWORD,
        cnt_usage: DWORD,
        th32_process_id: DWORD,
        th32_default_heap_id: usize,
        th32_module_id: DWORD,
        cnt_threads: DWORD,
        th32_parent_process_id: DWORD,
        pc_pri_class_base: i32,
        dw_flags: DWORD,
        sz_exe_file: [u16; MAX_PATH],
    }

    #[link(name = "kernel32")]
    extern "system" {
        fn CreateFileW(
            lpFileName: *const u16,
            dwDesiredAccess: DWORD,
            dwShareMode: DWORD,
            lpSecurityAttributes: *const c_void,
            dwCreationDisposition: DWORD,
            dwFlagsAndAttributes: DWORD,
            hTemplateFile: HANDLE,
        ) -> HANDLE;

        fn DeviceIoControl(
            hDevice: HANDLE,
            dwIoControlCode: DWORD,
            lpInBuffer: *const c_void,
            nInBufferSize: DWORD,
            lpOutBuffer: *mut c_void,
            nOutBufferSize: DWORD,
            lpBytesReturned: *mut DWORD,
            lpOverlapped: *mut c_void,
        ) -> BOOL;

        fn CreateWaitableTimerExW(
            lpTimerAttributes: *const c_void,
            lpTimerName: *const u16,
            dwFlags: DWORD,
            dwDesiredAccess: DWORD,
        ) -> HANDLE;

        fn SetWaitableTimer(
            hTimer: HANDLE,
            lpDueTime: *const i64,
            lPeriod: i32,
            pfnCompletionRoutine: *const c_void,
            lpArgToCompletionRoutine: *const c_void,
            fResume: BOOL,
        ) -> BOOL;

        fn WaitForSingleObject(hHandle: HANDLE, dwMilliseconds: DWORD) -> DWORD;

        fn CreateToolhelp32Snapshot(dwFlags: DWORD, th32ProcessID: DWORD) -> HANDLE;
        fn Process32FirstW(hSnapshot: HANDLE, lppe: *mut ProcessEntry32W) -> BOOL;
        fn Process32NextW(hSnapshot: HANDLE, lppe: *mut ProcessEntry32W) -> BOOL;

        fn CloseHandle(hObject: HANDLE) -> BOOL;
    }

    struct RawHandle(HANDLE);
    unsafe impl Send for RawHandle {}

    impl RawHandle {
        #[inline]
        fn raw(&self) -> HANDLE {
            self.0
        }
    }

    impl Drop for RawHandle {
        fn drop(&mut self) {
            if !self.0.is_null() && self.0 != INVALID_HANDLE_VALUE {
                unsafe {
                    CloseHandle(self.0);
                }
            }
        }
    }

    struct HighResWaitableTimer(Option<RawHandle>);

    impl HighResWaitableTimer {
        fn new() -> Self {
            let h = unsafe {
                CreateWaitableTimerExW(
                    null(),
                    null(),
                    CREATE_WAITABLE_TIMER_HIGH_RESOLUTION,
                    TIMER_ALL_ACCESS,
                )
            };
            if h.is_null() || h == INVALID_HANDLE_VALUE {
                Self(None)
            } else {
                Self(Some(RawHandle(h)))
            }
        }

        fn sleep(&self, dur: Duration) {
            if dur.is_zero() {
                return;
            }
            if let Some(ref timer) = self.0 {
                // Negative due time = relative timeout in 100-nanosecond intervals
                let intervals_100ns = -((dur.as_nanos() / 100).max(1) as i64);
                let ok = unsafe {
                    SetWaitableTimer(timer.raw(), &intervals_100ns, 0, null(), null(), 0)
                };
                if ok != 0 {
                    let timeout_ms = (dur.as_millis() as DWORD).saturating_add(10);
                    unsafe {
                        WaitForSingleObject(timer.raw(), timeout_ms);
                    }
                    return;
                }
            }
            thread::sleep(dur);
        }
    }

    fn open_virtual_mic_handle() -> Option<RawHandle> {
        let mut path_utf16: Vec<u16> = r"\\.\AudioRelayVirtualMicDeviceLink"
            .encode_utf16()
            .collect();
        path_utf16.push(0);

        let handle = unsafe {
            CreateFileW(
                path_utf16.as_ptr(),
                GENERIC_READ | GENERIC_WRITE,
                FILE_SHARE_READ | FILE_SHARE_WRITE,
                null(),
                OPEN_EXISTING,
                FILE_ATTRIBUTE_NORMAL,
                null_mut(),
            )
        };

        if handle.is_null() || handle == INVALID_HANDLE_VALUE {
            return None;
        }

        let mut version: u32 = 0;
        let mut bytes_returned: DWORD = 0;
        let ok = unsafe {
            DeviceIoControl(
                handle,
                IOCTL_VIRTUAL_MIC_GET_VERSION,
                null(),
                0,
                (&mut version as *mut u32) as *mut c_void,
                std::mem::size_of::<u32>() as DWORD,
                &mut bytes_returned,
                null_mut(),
            )
        };

        if ok != 0 && bytes_returned == 4 && version == 1 {
            Some(RawHandle(handle))
        } else {
            unsafe {
                CloseHandle(handle);
            }
            None
        }
    }

    pub fn is_virtual_mic_driver_available() -> bool {
        open_virtual_mic_handle().is_some()
    }

    /// Detect whether AudioRelay's own `audiorelay-backend.exe` service is concurrently running,
    /// which may contend for `\\.\AudioRelayVirtualMicDeviceLink` if an active AudioRelay session is open.
    pub fn is_audiorelay_backend_running() -> bool {
        let snap = unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) };
        if snap.is_null() || snap == INVALID_HANDLE_VALUE {
            return false;
        }
        let _guard = RawHandle(snap);
        let mut entry: ProcessEntry32W = unsafe { std::mem::zeroed() };
        entry.dw_size = std::mem::size_of::<ProcessEntry32W>() as DWORD;

        let mut ok = unsafe { Process32FirstW(snap, &mut entry) };
        while ok != 0 {
            let nul_pos = entry
                .sz_exe_file
                .iter()
                .position(|&c| c == 0)
                .unwrap_or(MAX_PATH);
            let exe = String::from_utf16_lossy(&entry.sz_exe_file[..nul_pos]).to_ascii_lowercase();
            if exe == "audiorelay-backend.exe" {
                return true;
            }
            ok = unsafe { Process32NextW(snap, &mut entry) };
        }
        false
    }

    #[inline]
    fn fill_one_tick_pcm(
        consumer: &mut Consumer<f32>,
        src_ch: usize,
        gain: f32,
        pcm_i16: &mut [i16; SAMPLES_PER_TICK],
        underrun_count: &AtomicU64,
    ) -> f32 {
        let available_frames = consumer.slots() / src_ch;
        let frames_to_read = FRAMES_PER_TICK.min(available_frames);
        let samples_to_read = frames_to_read * src_ch;
        let mut max_abs = 0.0f32;

        if samples_to_read > 0 {
            if let Ok(chunk) = consumer.read_chunk(samples_to_read) {
                let (first, second) = chunk.as_slices();
                for f in 0..frames_to_read {
                    let base = f * src_ch;
                    for c in 0..CHANNELS {
                        let idx = base + c.min(src_ch - 1);
                        let raw = if idx < first.len() {
                            first[idx]
                        } else {
                            second[idx - first.len()]
                        };
                        let val = (raw * gain).clamp(-1.0, 1.0);
                        let abs = val.abs();
                        if abs > max_abs {
                            max_abs = abs;
                        }
                        pcm_i16[f * CHANNELS + c] = (val * 32767.0) as i16;
                    }
                }
                chunk.commit_all();
            }
        }

        if frames_to_read < FRAMES_PER_TICK {
            pcm_i16[frames_to_read * CHANNELS..].fill(0);
            underrun_count.fetch_add(1, Ordering::Relaxed);
        }

        max_abs
    }

    pub struct VirtualMicPlaybackStream {
        stop_flag: Arc<AtomicBool>,
        writer_thread: Option<JoinHandle<()>>,
    }

    impl VirtualMicPlaybackStream {
        pub fn start(
            source_channels: u16,
            mut consumer: Consumer<f32>,
            volume_bits: Arc<AtomicU32>,
            peak_bits: Arc<AtomicU32>,
            underrun_count: Arc<AtomicU64>,
        ) -> Result<Self, AudioError> {
            let dev_handle = open_virtual_mic_handle().ok_or_else(|| {
                AudioError::DeviceNotFound(VIRTUAL_MIC_OUTPUT_DEVICE_NAME.to_string())
            })?;

            let stop_flag = Arc::new(AtomicBool::new(false));
            let stop_clone = Arc::clone(&stop_flag);

            let writer_thread = thread::Builder::new()
                .name("rsrelay-virtual-mic-out".to_string())
                .spawn(move || {
                    let dev_handle = dev_handle;
                    crate::promote_worker_thread_priority(48_000, FRAMES_PER_TICK as u32);
                    let hi_res_timer = HighResWaitableTimer::new();

                    // Set compact initial kernel ringbuffer capacity once at startup
                    let mut cap_bytes: u32 = TARGET_KERNEL_RING_CAPACITY_BYTES;
                    let mut dummy_ret: DWORD = 0;
                    unsafe {
                        let _ = DeviceIoControl(
                            dev_handle.raw(),
                            IOCTL_VIRTUAL_MIC_SET_CAPACITY,
                            (&mut cap_bytes as *mut u32) as *const c_void,
                            std::mem::size_of::<u32>() as DWORD,
                            null_mut(),
                            0,
                            &mut dummy_ret,
                            null_mut(),
                        );
                    }

                    let src_ch = (source_channels as usize).max(1);
                    let stream_start = Instant::now();
                    let mut emitted_ticks: u64 = 0;
                    let mut pcm_i16 = [0i16; SAMPLES_PER_TICK];
                    let mut last_kernel_queued: Option<(u32, u32)> = None;

                    while !stop_clone.load(Ordering::Relaxed) {
                        let elapsed_ns = stream_start.elapsed().as_nanos() as u64;
                        let target_ticks = elapsed_ns / 5_000_000;

                        if target_ticks > emitted_ticks {
                            let due = (target_ticks - emitted_ticks).min(8) as usize;
                            emitted_ticks = target_ticks;

                            // Closed-loop watermark adjustment against WASAPI hardware clock when active:
                            // - If kernel queue exceeded 45ms, hold 1 tick so WASAPI drains smoothly.
                            // - If kernel queue dropped below 10ms and user-mode ring has samples ready,
                            //   push 1 extra tick to prevent capture underrun.
                            let mut frames_to_emit = due;
                            if let Some((cap, queued)) = last_kernel_queued {
                                if queued >= cap.saturating_sub(BYTES_PER_TICK) {
                                    // Kernel buffer is completely full (capture pin stalled/idle):
                                    // keep draining 1:1 at 48kHz so user-mode jitter buffer stays locked at ~20ms.
                                } else if queued > HIGH_KERNEL_QUEUED_BYTES {
                                    frames_to_emit = frames_to_emit.saturating_sub(1);
                                    // Clear cached watermark so the very next 5ms tick resumes writing & re-probes
                                    last_kernel_queued = None;
                                    let max_user_samples = 48_000 * src_ch * 35 / 1000;
                                    if consumer.slots() > max_user_samples {
                                        let drop_n = ((FRAMES_PER_TICK * src_ch)
                                            .min(consumer.slots())
                                            / src_ch)
                                            * src_ch;
                                        if drop_n > 0 {
                                            if let Ok(chunk) = consumer.read_chunk(drop_n) {
                                                chunk.commit_all();
                                            }
                                        }
                                    }
                                } else if queued < LOW_KERNEL_QUEUED_BYTES
                                    && consumer.slots() >= FRAMES_PER_TICK * src_ch * 2
                                    && frames_to_emit < 6
                                {
                                    frames_to_emit += 1;
                                }
                            } else {
                                // When no app is actively recording from Virtual Mic, prevent user-mode
                                // ringbuffer from ever accumulating > 35ms of stale samples
                                let max_idle_samples = 48_000 * src_ch * 35 / 1000;
                                if consumer.slots() > max_idle_samples {
                                    let raw_excess =
                                        consumer.slots() - (48_000 * src_ch * 20 / 1000);
                                    let excess = (raw_excess / src_ch) * src_ch;
                                    if excess > 0 {
                                        if let Ok(chunk) = consumer.read_chunk(excess) {
                                            chunk.commit_all();
                                        }
                                    }
                                }
                            }

                            let gain = f32::from_bits(volume_bits.load(Ordering::Relaxed));
                            let mut batch_peak = 0.0f32;

                            for _ in 0..frames_to_emit {
                                let tick_peak = fill_one_tick_pcm(
                                    &mut consumer,
                                    src_ch,
                                    gain,
                                    &mut pcm_i16,
                                    &underrun_count,
                                );
                                if tick_peak > batch_peak {
                                    batch_peak = tick_peak;
                                }

                                let mut out_status = [0u32; 2];
                                let mut bytes_returned: DWORD = 0;
                                let ok = unsafe {
                                    DeviceIoControl(
                                        dev_handle.raw(),
                                        IOCTL_VIRTUAL_MIC_WRITE_PCM,
                                        pcm_i16.as_ptr() as *const c_void,
                                        BYTES_PER_TICK,
                                        out_status.as_mut_ptr() as *mut c_void,
                                        std::mem::size_of_val(&out_status) as DWORD,
                                        &mut bytes_returned,
                                        null_mut(),
                                    )
                                };

                                if ok != 0 && bytes_returned == 8 {
                                    last_kernel_queued = Some((out_status[0], out_status[1]));
                                } else {
                                    last_kernel_queued = None;
                                }
                            }

                            if frames_to_emit > 0 {
                                peak_bits.store(batch_peak.to_bits(), Ordering::Relaxed);
                            }
                        }

                        let next_tick_ns = (emitted_ticks + 1) * 5_000_000;
                        let now_ns = stream_start.elapsed().as_nanos() as u64;
                        if next_tick_ns > now_ns {
                            hi_res_timer.sleep(Duration::from_nanos(next_tick_ns - now_ns));
                        }
                    }
                })
                .expect("spawn rsrelay-virtual-mic-out thread");

            Ok(Self {
                stop_flag,
                writer_thread: Some(writer_thread),
            })
        }
    }

    impl Drop for VirtualMicPlaybackStream {
        fn drop(&mut self) {
            self.stop_flag.store(true, Ordering::Relaxed);
            if let Some(handle) = self.writer_thread.take() {
                let _ = handle.join();
            }
        }
    }
}

#[cfg(not(target_os = "windows"))]
mod imp {
    use super::*;

    pub fn is_virtual_mic_driver_available() -> bool {
        false
    }

    pub fn is_audiorelay_backend_running() -> bool {
        false
    }

    pub struct VirtualMicPlaybackStream;

    impl VirtualMicPlaybackStream {
        pub fn start(
            _source_channels: u16,
            _consumer: Consumer<f32>,
            _volume_bits: Arc<AtomicU32>,
            _peak_bits: Arc<AtomicU32>,
            _underrun_count: Arc<AtomicU64>,
        ) -> Result<Self, AudioError> {
            Err(AudioError::DeviceNotFound(
                VIRTUAL_MIC_OUTPUT_DEVICE_NAME.to_string(),
            ))
        }
    }
}

pub use imp::{
    is_audiorelay_backend_running, is_virtual_mic_driver_available, VirtualMicPlaybackStream,
};

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        list_audio_devices, select_default_virtual_mic_output_device, start_capture_stream,
        start_playback_stream, CaptureSource,
    };
    use rtrb::RingBuffer;
    use std::time::Duration;

    #[test]
    fn test_echo_free_null_virtual_mic_guard_sink() {
        let (play_tx, play_rx) = RingBuffer::<f32>::new(48_000 * 2);
        let _tone = start_capture_stream(CaptureSource::SyntheticTone, None, 2, play_tx)
            .expect("start synthetic tone");
        let play_handle = start_playback_stream(Some(NULL_VIRTUAL_MIC_DEVICE_NAME), 2, play_rx)
            .expect("start null virtual mic guard");

        std::thread::sleep(Duration::from_millis(80));
        assert!(
            play_handle.peak_level() > 0.15,
            "NullVirtualMicStream must meter incoming voice peak while emitting zero speaker audio"
        );
    }

    #[test]
    fn test_windows_virtual_mic_end_to_end_bridge() {
        if !is_virtual_mic_driver_available() {
            return;
        }

        let devices = list_audio_devices().expect("list audio devices");
        assert!(
            devices
                .iter()
                .any(|d| d.is_output && d.name == VIRTUAL_MIC_OUTPUT_DEVICE_NAME),
            "Virtual Mic should be exposed as an output sink when driver is available"
        );
        assert!(select_default_virtual_mic_output_device().is_some());

        // 1. Open WASAPI Microphone capture on `Virtual Mic`
        let (cap_tx, mut cap_rx) = RingBuffer::<f32>::new(48_000 * 4);
        let cap_handle = start_capture_stream(
            CaptureSource::Microphone,
            Some("Virtual Mic"),
            2,
            cap_tx,
        )
        .expect("start capture stream on Virtual Mic");

        // 2. Start synthetic tone generator -> Virtual Mic high-res playback stream
        let (play_tx, play_rx) = RingBuffer::<f32>::new(48_000 * 4);
        let _tone_gen = start_capture_stream(CaptureSource::SyntheticTone, None, 2, play_tx)
            .expect("start synthetic tone");
        let play_handle = start_playback_stream(Some(VIRTUAL_MIC_OUTPUT_DEVICE_NAME), 2, play_rx)
            .expect("start playback stream to Virtual Mic");

        // Warm up 40ms so kernel ringbuffer stabilizes, then drain & measure exact 250ms throughput
        std::thread::sleep(Duration::from_millis(40));
        if let Ok(c) = cap_rx.read_chunk(cap_rx.slots()) {
            c.commit_all();
        }

        let start = Instant::now();
        let mut max_captured_peak = 0.0f32;
        let mut total_captured_samples = 0usize;
        while start.elapsed() < Duration::from_millis(250) {
            std::thread::sleep(Duration::from_millis(10));
            let avail = cap_rx.slots();
            if avail > 0 {
                if let Ok(chunk) = cap_rx.read_chunk(avail) {
                    let (s1, s2) = chunk.as_slices();
                    for &s in s1.iter().chain(s2.iter()) {
                        let abs = s.abs();
                        if abs > max_captured_peak {
                            max_captured_peak = abs;
                        }
                    }
                    total_captured_samples += avail;
                    chunk.commit_all();
                }
            }
            max_captured_peak = max_captured_peak.max(cap_handle.peak_level());
        }
        let elapsed_ms = start.elapsed().as_secs_f64() * 1000.0;
        // Nominal stereo samples @ 48,000 Hz = 96 samples/ms (~24,000 samples in 250 ms)
        let expected_samples = (elapsed_ms * 96.0) as usize;

        println!(
            "Virtual Mic High-Res E2E: elapsed={elapsed_ms:.1}ms, play_peak={:.3}, captured_samples={total_captured_samples} (expected ~{expected_samples}), captured_peak={max_captured_peak:.3}",
            play_handle.peak_level(),
        );

        assert!(
            play_handle.peak_level() > 0.15,
            "Playback stream to Virtual Mic should report active signal peak"
        );
        assert!(
            max_captured_peak > 0.15,
            "WASAPI capture on Virtual Mic should receive clean signal peak (got {max_captured_peak})"
        );
        assert!(
            total_captured_samples >= expected_samples * 85 / 100,
            "High-res pacing must sustain ~100% 48kHz sample rate without timer starvation (got {total_captured_samples}, expected >= 85% of {expected_samples})"
        );
    }
}
