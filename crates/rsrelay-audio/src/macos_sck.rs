//! macOS `ScreenCaptureKit` native driverless system audio capture & planar-to-interleaved converter.

#[cfg(target_os = "macos")]
use std::ffi::c_void;
#[cfg(target_os = "macos")]
use std::sync::{
    atomic::{AtomicU32, Ordering},
    Arc,
};

use rtrb::Producer;

/// Convert non-interleaved (Planar: `[L0, L1, ...], [R0, R1, ...]`) `f32` audio buffers
/// delivered by macOS `ScreenCaptureKit` (`CMSampleBuffer` / `AudioBufferList`) directly
/// into an interleaved SPSC ring buffer (`[L0, R0, L1, R1, ...]`) without heap allocations.
///
/// Returns `(samples_written, peak_amplitude)`.
#[inline]
pub fn interleave_planar_to_ringbuffer(
    planar_channels: &[&[f32]],
    target_channels: usize,
    producer: &mut Producer<f32>,
) -> (usize, f32) {
    if planar_channels.is_empty() || target_channels == 0 {
        return (0, 0.0);
    }

    let frames = planar_channels[0].len();
    let available = producer.slots();
    let frames_to_write = (available / target_channels).min(frames);
    let samples_to_write = frames_to_write * target_channels;

    if samples_to_write == 0 {
        return (0, 0.0);
    }

    let mut max_abs = 0.0f32;
    if let Ok(mut chunk) = producer.write_chunk(samples_to_write) {
        let (first, second) = chunk.as_mut_slices();
        let mut out_idx = 0;

        for frame_idx in 0..frames_to_write {
            for ch in 0..target_channels {
                let sample = if target_channels == 1 && planar_channels.len() >= 2 {
                    (planar_channels[0][frame_idx] + planar_channels[1][frame_idx]) * 0.5
                } else {
                    let src_ch = ch.min(planar_channels.len() - 1);
                    planar_channels[src_ch][frame_idx]
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
        (samples_to_write, max_abs)
    } else {
        (0, 0.0)
    }
}

#[cfg(target_os = "macos")]
mod native {
    use super::*;

    type SckCallback = extern "C" fn(
        user_ctx: *mut c_void,
        ch0: *const f32,
        ch1: *const f32,
        num_frames: usize,
        num_channels: u32,
    );

    extern "C" {
        fn rsrelay_sck_preflight_permission() -> bool;
        fn rsrelay_sck_request_permission() -> bool;
        fn rsrelay_sck_stream_start(
            sample_rate: u32,
            channels: u32,
            exclude_current_process: bool,
            callback: SckCallback,
            user_ctx: *mut c_void,
        ) -> *mut c_void;
        fn rsrelay_sck_stream_stop(session_ptr: *mut c_void);
    }

    struct SckCallbackContext {
        producer: Producer<f32>,
        target_channels: usize,
        peak_bits: Arc<AtomicU32>,
    }

    extern "C" fn sck_audio_trampoline(
        user_ctx: *mut c_void,
        ch0: *const f32,
        ch1: *const f32,
        num_frames: usize,
        num_channels: u32,
    ) {
        if user_ctx.is_null() || ch0.is_null() || num_frames == 0 {
            return;
        }
        let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| unsafe {
            let ctx = &mut *(user_ctx as *mut SckCallbackContext);
            let slice0 = std::slice::from_raw_parts(ch0, num_frames);
            let slice1 = if num_channels > 1 && !ch1.is_null() {
                std::slice::from_raw_parts(ch1, num_frames)
            } else {
                slice0
            };
            let planar: [&[f32]; 2] = [slice0, slice1];
            let (_written, peak) =
                interleave_planar_to_ringbuffer(&planar, ctx.target_channels, &mut ctx.producer);
            ctx.peak_bits.store(peak.to_bits(), Ordering::Relaxed);
        }));
    }

    /// Check whether macOS Screen/Audio Capture TCC permission is currently granted.
    pub fn has_screen_capture_permission() -> bool {
        unsafe { rsrelay_sck_preflight_permission() }
    }

    /// Prompt the user for macOS Screen/Audio Capture TCC permission if not yet granted.
    pub fn request_screen_capture_permission() -> bool {
        unsafe { rsrelay_sck_request_permission() }
    }

    /// Active macOS `ScreenCaptureKit` system audio capture stream.
    pub struct MacosSckStream {
        session_ptr: *mut c_void,
        ctx_ptr: *mut SckCallbackContext,
    }

    // Safety: SCK session pointer and heap context are only accessed by the serial GCD audio queue
    // while running, and torn down synchronously inside `rsrelay_sck_stream_stop` on Drop.
    unsafe impl Send for MacosSckStream {}

    impl MacosSckStream {
        pub fn start(
            sample_rate: u32,
            target_channels: u16,
            producer: Producer<f32>,
            peak_bits: Arc<AtomicU32>,
        ) -> Result<Self, Producer<f32>> {
            let boxed_ctx = Box::new(SckCallbackContext {
                producer,
                target_channels: (target_channels as usize).max(1),
                peak_bits,
            });
            let ctx_ptr = Box::into_raw(boxed_ctx);

            let session_ptr = unsafe {
                rsrelay_sck_stream_start(
                    sample_rate,
                    target_channels as u32,
                    true, // excludesCurrentProcessAudio = true to prevent feedback loops
                    sck_audio_trampoline,
                    ctx_ptr as *mut c_void,
                )
            };

            if session_ptr.is_null() {
                let recovered = unsafe { Box::from_raw(ctx_ptr) };
                return Err(recovered.producer);
            }

            Ok(Self {
                session_ptr,
                ctx_ptr,
            })
        }
    }

    impl Drop for MacosSckStream {
        fn drop(&mut self) {
            unsafe {
                if !self.session_ptr.is_null() {
                    rsrelay_sck_stream_stop(self.session_ptr);
                    self.session_ptr = std::ptr::null_mut();
                }
                if !self.ctx_ptr.is_null() {
                    let _ = Box::from_raw(self.ctx_ptr);
                    self.ctx_ptr = std::ptr::null_mut();
                }
            }
        }
    }
}

#[cfg(target_os = "macos")]
pub use native::{
    has_screen_capture_permission, request_screen_capture_permission, MacosSckStream,
};

#[cfg(test)]
mod tests {
    use super::*;
    use rtrb::RingBuffer;

    #[test]
    fn test_planar_to_interleaved_ringbuffer() {
        let (mut prod, mut cons) = RingBuffer::<f32>::new(16);
        let left = [0.1f32, 0.2, 0.3];
        let right = [-0.1f32, -0.2, -0.3];
        let planar: [&[f32]; 2] = [&left, &right];

        let (written, peak) = interleave_planar_to_ringbuffer(&planar, 2, &mut prod);
        assert_eq!(written, 6);
        assert!((peak - 0.3).abs() < 1e-5);

        let chunk = cons.read_chunk(6).unwrap();
        let (first, _) = chunk.as_slices();
        assert_eq!(first, &[0.1, -0.1, 0.2, -0.2, 0.3, -0.3]);
        chunk.commit_all();
    }
}
