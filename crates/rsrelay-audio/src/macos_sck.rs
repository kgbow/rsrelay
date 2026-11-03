//! macOS system audio capture utilities (`ScreenCaptureKit` planar conversion & CoreAudio loopback discovery).

use rtrb::Producer;

/// Convert non-interleaved (Planar: `[L0, L1, ...], [R0, R1, ...]`) `f32` audio buffers
/// delivered by macOS `ScreenCaptureKit` (`CMSampleBuffer` / `AudioBufferList`) directly
/// into an interleaved SPSC ring buffer (`[L0, R0, L1, R1, ...]`) without heap allocations.
///
/// Returns the number of interleaved samples written.
#[inline]
pub fn interleave_planar_to_ringbuffer(
    planar_channels: &[&[f32]],
    target_channels: usize,
    producer: &mut Producer<f32>,
) -> usize {
    if planar_channels.is_empty() || target_channels == 0 {
        return 0;
    }

    let frames = planar_channels[0].len();
    let available = producer.slots();
    let frames_to_write = (available / target_channels).min(frames);
    let samples_to_write = frames_to_write * target_channels;

    if samples_to_write == 0 {
        return 0;
    }

    if let Ok(mut chunk) = producer.write_chunk(samples_to_write) {
        let (first, second) = chunk.as_mut_slices();
        let mut out_idx = 0;

        for frame_idx in 0..frames_to_write {
            for ch in 0..target_channels {
                let src_ch = ch.min(planar_channels.len() - 1);
                let sample = planar_channels[src_ch][frame_idx];
                if out_idx < first.len() {
                    first[out_idx] = sample;
                } else {
                    second[out_idx - first.len()] = sample;
                }
                out_idx += 1;
            }
        }
        chunk.commit_all();
        samples_to_write
    } else {
        0
    }
}

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

        let written = interleave_planar_to_ringbuffer(&planar, 2, &mut prod);
        assert_eq!(written, 6);

        let chunk = cons.read_chunk(6).unwrap();
        let (first, _) = chunk.as_slices();
        assert_eq!(first, &[0.1, -0.1, 0.2, -0.2, 0.3, -0.3]);
        chunk.commit_all();
    }
}
