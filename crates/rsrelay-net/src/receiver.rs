use std::net::{SocketAddr, UdpSocket};
use std::sync::{
    atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering},
    Arc, Mutex,
};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use rtrb::Producer;
use socket2::{Domain, Protocol, Socket, Type};

use rsrelay_audio::promote_worker_thread_priority;
use rsrelay_dsp::{
    AdaptiveResampler, AudioDecoder, ClockDriftController, JitterBuffer, PopOutcome,
};
use rsrelay_proto::{
    ChannelLayout, CodecType, FrameDuration, PacketHeader, MAX_PACKET_SIZE, MAX_UDP_PAYLOAD,
    STANDARD_SAMPLE_RATE,
};

#[derive(Debug, Clone)]
pub struct ReceiverConfig {
    pub bind_addr: SocketAddr,
    pub target_buffer_ms: u16,
    pub output_sample_rate: u32,
    pub output_channels: ChannelLayout,
    pub ringbuffer_capacity_samples: usize,
}

#[derive(Debug, Default)]
pub struct ReceiverMetrics {
    pub packets_received: AtomicU64,
    pub packets_lost: AtomicU64,
    pub fec_recovered: AtomicU64,
    pub plc_frames: AtomicU64,
    pub jitter_us: AtomicU64,
    pub buffer_level_ms_bits: AtomicU32,
    pub resample_ratio_bits: AtomicU64,
    pub source_addr: Mutex<Option<SocketAddr>>,
}

#[derive(Debug, Clone, Copy, Default)]
pub struct ReceiverSnapshot {
    pub packets_received: u64,
    pub packets_lost: u64,
    pub fec_recovered: u64,
    pub plc_frames: u64,
    pub jitter_us: u64,
    pub buffer_level_ms: f32,
    pub resample_ratio: f64,
    pub source_addr: Option<SocketAddr>,
}

pub struct AudioReceiverWorker {
    stop_flag: Arc<AtomicBool>,
    metrics: Arc<ReceiverMetrics>,
    join_handle: Option<JoinHandle<()>>,
}

impl AudioReceiverWorker {
    pub fn spawn(
        config: ReceiverConfig,
        mut producer: Producer<f32>,
    ) -> Result<Self, anyhow::Error> {
        let socket = build_low_latency_udp_receiver(config.bind_addr)?;

        let mut current_codec = CodecType::Opus;
        let out_channels = config.output_channels;
        let mut wire_channels = out_channels;
        let mut current_frame_dur = FrameDuration::Ms5;
        let out_ch = out_channels.channels().max(1);
        let mut frames_per_pkt = current_frame_dur.samples_per_channel();

        let mut decoder = AudioDecoder::new(current_codec, wire_channels)?;
        let mut resampler = AdaptiveResampler::new(
            STANDARD_SAMPLE_RATE,
            config.output_sample_rate,
            out_ch,
            frames_per_pkt,
            1.01,
        )?;

        let stop_flag = Arc::new(AtomicBool::new(false));
        let metrics = Arc::new(ReceiverMetrics::default());
        metrics
            .resample_ratio_bits
            .store(1.0f64.to_bits(), Ordering::Relaxed);

        let stop_clone = Arc::clone(&stop_flag);
        let metrics_clone = Arc::clone(&metrics);

        let join_handle = thread::Builder::new()
            .name("rsrelay-rx-worker".to_string())
            .spawn(move || {
                let prefetch_packets = ((config.target_buffer_ms as u64 * 1000)
                    / current_frame_dur.duration_us())
                .max(2) as usize;

                let mut jitter = JitterBuffer::new(prefetch_packets);

                // Stage 1 PI loop: locks packet pop rate (`next_deadline`) strictly to Sender clock
                // using ONLY `JitterBuffer` occupancy so `rtrb` can never squeeze `JitterBuffer` to 0.
                let jb_target_frames_48k = ((config.target_buffer_ms as usize
                    * STANDARD_SAMPLE_RATE as usize)
                    / 1000)
                    .max(frames_per_pkt * 2);
                let mut jb_ctrl = ClockDriftController::new(jb_target_frames_48k);
                let mut current_jb_ratio = 1.0f64;

                // Stage 2 PI loop: locks ASRC output sample rate strictly to Receiver DAC hardware clock
                // using `rtrb` occupancy with feed-forward decoupling (`r_asrc = jb_ratio * dac_ratio`).
                let rtrb_target_ms = (config.target_buffer_ms as usize).clamp(14, 24);
                let rtrb_target_frames =
                    ((rtrb_target_ms * config.output_sample_rate as usize) / 1000).max(480);
                let max_rtrb_frames =
                    (rtrb_target_frames * 4).max((config.output_sample_rate as usize) / 8);
                let mut rtrb_ctrl = ClockDriftController::new(rtrb_target_frames);
                let mut was_buffering = true;

                promote_worker_thread_priority(config.output_sample_rate, frames_per_pkt as u32);

                let mut udp_buf = [0u8; MAX_PACKET_SIZE];
                let mut pkt_payload = [0u8; MAX_UDP_PAYLOAD];
                let mut wire_decoded_pcm = vec![0.0f32; frames_per_pkt * wire_channels.channels()];
                let mut decoded_pcm = vec![0.0f32; frames_per_pkt * out_ch];
                let mut resampled_pcm = Vec::with_capacity(frames_per_pkt * out_ch * 2);

                let base_step_us = current_frame_dur.duration_us();
                let mut next_deadline = Instant::now() + Duration::from_micros(base_step_us);
                let mut current_src: Option<SocketAddr> = None;
                let mut last_packet_at: Option<Instant> = None;

                while !stop_clone.load(Ordering::Relaxed) {
                    // 1. Drain all arrived UDP datagrams from the OS socket into JitterBuffer
                    loop {
                        match socket.recv_from(&mut udp_buf) {
                            Ok((len, src)) => {
                                if let Ok((hdr, payload)) =
                                    PacketHeader::decode_packet(&udp_buf[..len])
                                {
                                    last_packet_at = Some(Instant::now());
                                    if current_src != Some(src) {
                                        current_src = Some(src);
                                        if let Ok(mut guard) = metrics_clone.source_addr.lock() {
                                            *guard = Some(src);
                                        }
                                    }

                                    // Reconfigure pipeline on the fly if sender changed codec, frame size, or channels
                                    if hdr.flags.codec != current_codec
                                        || hdr.flags.frame_duration != current_frame_dur
                                        || hdr.flags.channels != wire_channels
                                    {
                                        let dur_changed =
                                            hdr.flags.frame_duration != current_frame_dur;
                                        current_codec = hdr.flags.codec;
                                        current_frame_dur = hdr.flags.frame_duration;
                                        wire_channels = hdr.flags.channels;
                                        frames_per_pkt = current_frame_dur.samples_per_channel();

                                        let new_prefetch = ((config.target_buffer_ms as u64 * 1000)
                                            / current_frame_dur.duration_us())
                                        .max(2)
                                            as usize;
                                        jitter.set_target_prefetch(new_prefetch);

                                        let new_jb_target = ((config.target_buffer_ms as usize
                                            * STANDARD_SAMPLE_RATE as usize)
                                            / 1000)
                                            .max(frames_per_pkt * 2);
                                        jb_ctrl = ClockDriftController::new(new_jb_target);
                                        current_jb_ratio = 1.0;

                                        if let Ok(new_dec) =
                                            AudioDecoder::new(current_codec, wire_channels)
                                        {
                                            decoder = new_dec;
                                        }
                                        wire_decoded_pcm
                                            .resize(frames_per_pkt * wire_channels.channels(), 0.0);
                                        decoded_pcm.resize(frames_per_pkt * out_ch, 0.0);

                                        if dur_changed {
                                            if let Ok(new_res) = AdaptiveResampler::new(
                                                STANDARD_SAMPLE_RATE,
                                                config.output_sample_rate,
                                                out_ch,
                                                frames_per_pkt,
                                                1.01,
                                            ) {
                                                resampler = new_res;
                                            }
                                        }
                                    }

                                    let prev_buffering = jitter.is_buffering();
                                    jitter.push(hdr, payload);
                                    if prev_buffering && !jitter.is_buffering() {
                                        // Phase-align pop timer half a frame after prefetch completion
                                        // for maximum jitter margin on both sides of packet arrivals.
                                        let half_step_ns =
                                            (current_frame_dur.duration_us() * 1000) / 2;
                                        next_deadline =
                                            Instant::now() + Duration::from_nanos(half_step_ns);
                                    }
                                }
                            }
                            Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => break,
                            Err(_) => break,
                        }
                    }

                    // 2. When Sender-locked frame deadline arrives, pop from JitterBuffer,
                    //    decode, and ASRC-resample to Receiver DAC clock into playback ring buffer.
                    let now = Instant::now();
                    if now >= next_deadline {
                        if current_src.is_some()
                            && last_packet_at
                                .map(|t| now.saturating_duration_since(t) > Duration::from_secs(3))
                                .unwrap_or(false)
                        {
                            current_src = None;
                            if let Ok(mut guard) = metrics_clone.source_addr.lock() {
                                *guard = None;
                            }
                        }

                        let dynamic_step_ns = ((current_frame_dur.duration_us() as f64)
                            * 1000.0
                            * current_jb_ratio)
                            as u64;
                        let step_duration = Duration::from_nanos(dynamic_step_ns.max(500_000));
                        next_deadline += step_duration;
                        if now > next_deadline + step_duration * 4 {
                            // Recover timer if OS suspended or thread was delayed
                            next_deadline = now + step_duration;
                        }

                        let jb_frames_before_pop = jitter.occupied_count() * frames_per_pkt;
                        let outcome = jitter.pop(&mut pkt_payload);
                        let wire_ch = wire_channels.channels().max(1);
                        let has_frame = match outcome {
                            PopOutcome::Ready { payload_len, .. } => {
                                if wire_ch == out_ch {
                                    decoder
                                        .decode(&pkt_payload[..payload_len], &mut decoded_pcm)
                                        .is_ok()
                                } else if decoder
                                    .decode(&pkt_payload[..payload_len], &mut wire_decoded_pcm)
                                    .is_ok()
                                {
                                    convert_interleaved_channels(
                                        &wire_decoded_pcm,
                                        wire_ch,
                                        &mut decoded_pcm,
                                        out_ch,
                                    );
                                    true
                                } else {
                                    false
                                }
                            }
                            PopOutcome::SilenceHeartbeat { .. } => {
                                decoded_pcm.fill(0.0);
                                true
                            }
                            PopOutcome::MissingWithFec { fec_payload_len } => {
                                if wire_ch == out_ch {
                                    decoder
                                        .conceal_loss(
                                            Some(&pkt_payload[..fec_payload_len]),
                                            &mut decoded_pcm,
                                        )
                                        .is_ok()
                                } else if decoder
                                    .conceal_loss(
                                        Some(&pkt_payload[..fec_payload_len]),
                                        &mut wire_decoded_pcm,
                                    )
                                    .is_ok()
                                {
                                    convert_interleaved_channels(
                                        &wire_decoded_pcm,
                                        wire_ch,
                                        &mut decoded_pcm,
                                        out_ch,
                                    );
                                    true
                                } else {
                                    false
                                }
                            }
                            PopOutcome::MissingNoFec => {
                                if wire_ch == out_ch {
                                    decoder.conceal_loss(None, &mut decoded_pcm).is_ok()
                                } else if decoder.conceal_loss(None, &mut wire_decoded_pcm).is_ok()
                                {
                                    convert_interleaved_channels(
                                        &wire_decoded_pcm,
                                        wire_ch,
                                        &mut decoded_pcm,
                                        out_ch,
                                    );
                                    true
                                } else {
                                    false
                                }
                            }
                            PopOutcome::Buffering => {
                                if !was_buffering {
                                    was_buffering = true;
                                    jb_ctrl.reset();
                                    rtrb_ctrl.reset();
                                    current_jb_ratio = 1.0;
                                }
                                false
                            }
                        };

                        if has_frame {
                            let priming = if was_buffering {
                                was_buffering = false;
                                jb_ctrl.reset();
                                rtrb_ctrl.reset();
                                true
                            } else {
                                false
                            };

                            // Stage 1: JitterBuffer watermark -> packet pop interval ratio
                            let jb_ratio = jb_ctrl.update(jb_frames_before_pop);
                            current_jb_ratio = jb_ratio;

                            let rtrb_queued_samples = config
                                .ringbuffer_capacity_samples
                                .saturating_sub(producer.slots());
                            let rtrb_queued_frames = rtrb_queued_samples / out_ch;
                            let expected_out_frames = (frames_per_pkt
                                * (config.output_sample_rate as usize))
                                / (STANDARD_SAMPLE_RATE as usize);

                            // Stage 2: rtrb hardware watermark -> DAC ratio, decoupled from jb_ratio
                            let measured_rtrb_frames = if priming {
                                rtrb_target_frames
                            } else {
                                rtrb_queued_frames + (expected_out_frames / 2)
                            };
                            let dac_ratio = rtrb_ctrl.update(measured_rtrb_frames);
                            let r_asrc = (jb_ratio * dac_ratio).clamp(0.995, 1.005);

                            if let Ok(out_frames) =
                                resampler.process_interleaved(&decoded_pcm, r_asrc, &mut resampled_pcm)
                            {
                                if priming && rtrb_queued_frames + out_frames < rtrb_target_frames {
                                    // Pre-prime rtrb with silence cushion so the very first OS
                                    // audio callback has contiguous frames without startup underrun.
                                    let pad_frames =
                                        rtrb_target_frames - (rtrb_queued_frames + out_frames);
                                    let max_pad_frames = producer
                                        .slots()
                                        .saturating_sub(resampled_pcm.len())
                                        / out_ch;
                                    let pad_samples = pad_frames.min(max_pad_frames) * out_ch;
                                    if pad_samples > 0 {
                                        if let Ok(mut chunk) = producer.write_chunk(pad_samples) {
                                            let (s1, s2) = chunk.as_mut_slices();
                                            s1.fill(0.0);
                                            s2.fill(0.0);
                                            chunk.commit_all();
                                        }
                                    }
                                }

                                if rtrb_queued_frames > max_rtrb_frames {
                                    // Host audio callback stalled (>125ms): skip pushing to cap stale
                                    // latency and avoid saturating the 500ms ring buffer.
                                    rtrb_ctrl.reset();
                                } else {
                                    // Always write whole multi-channel frames only; never truncate
                                    // mid-packet or split stereo [L, R] sample pairs.
                                    let avail_samples = (producer.slots() / out_ch) * out_ch;
                                    if avail_samples >= resampled_pcm.len()
                                        && !resampled_pcm.is_empty()
                                    {
                                        if let Ok(mut chunk) =
                                            producer.write_chunk(resampled_pcm.len())
                                        {
                                            let (first, second) = chunk.as_mut_slices();
                                            first.copy_from_slice(&resampled_pcm[..first.len()]);
                                            if !second.is_empty() {
                                                second.copy_from_slice(
                                                    &resampled_pcm[first.len()..],
                                                );
                                            }
                                            chunk.commit_all();
                                        }
                                    }
                                }
                            }

                            // Update atomic telemetry: report real JitterBuffer cushion (ms) & net ASRC ratio
                            let jb_stats = jitter.stats();
                            metrics_clone
                                .packets_received
                                .store(jb_stats.packets_received, Ordering::Relaxed);
                            metrics_clone
                                .packets_lost
                                .store(jb_stats.packets_lost, Ordering::Relaxed);
                            metrics_clone
                                .fec_recovered
                                .store(jb_stats.fec_recovered_frames, Ordering::Relaxed);
                            metrics_clone
                                .plc_frames
                                .store(jb_stats.plc_frames, Ordering::Relaxed);
                            metrics_clone
                                .jitter_us
                                .store(jb_stats.jitter_estimate_us as u64, Ordering::Relaxed);

                            let level_ms = (jb_ctrl.smoothed_frames() as f32 * 1000.0)
                                / (STANDARD_SAMPLE_RATE as f32);
                            metrics_clone
                                .buffer_level_ms_bits
                                .store(level_ms.to_bits(), Ordering::Relaxed);
                            metrics_clone
                                .resample_ratio_bits
                                .store(r_asrc.to_bits(), Ordering::Relaxed);
                        }
                    } else {
                        let sleep_dur =
                            (next_deadline - now).min(Duration::from_micros(500));
                        thread::sleep(sleep_dur);
                    }
                }
            })?;

        Ok(Self {
            stop_flag,
            metrics,
            join_handle: Some(join_handle),
        })
    }

    pub fn metrics_arc(&self) -> Arc<ReceiverMetrics> {
        Arc::clone(&self.metrics)
    }

    pub fn snapshot(&self) -> ReceiverSnapshot {
        ReceiverSnapshot {
            packets_received: self.metrics.packets_received.load(Ordering::Relaxed),
            packets_lost: self.metrics.packets_lost.load(Ordering::Relaxed),
            fec_recovered: self.metrics.fec_recovered.load(Ordering::Relaxed),
            plc_frames: self.metrics.plc_frames.load(Ordering::Relaxed),
            jitter_us: self.metrics.jitter_us.load(Ordering::Relaxed),
            buffer_level_ms: f32::from_bits(
                self.metrics.buffer_level_ms_bits.load(Ordering::Relaxed),
            ),
            resample_ratio: f64::from_bits(
                self.metrics.resample_ratio_bits.load(Ordering::Relaxed),
            ),
            source_addr: self.metrics.source_addr.lock().ok().and_then(|g| *g),
        }
    }
}

impl Drop for AudioReceiverWorker {
    fn drop(&mut self) {
        self.stop_flag.store(true, Ordering::Relaxed);
        if let Some(handle) = self.join_handle.take() {
            let _ = handle.join();
        }
    }
}

#[inline]
fn convert_interleaved_channels(
    src: &[f32],
    src_ch: usize,
    dst: &mut [f32],
    dst_ch: usize,
) {
    let frames = (src.len() / src_ch.max(1)).min(dst.len() / dst_ch.max(1));
    for f in 0..frames {
        let s_base = f * src_ch;
        let d_base = f * dst_ch;
        if dst_ch == 1 && src_ch >= 2 {
            dst[d_base] = (src[s_base] + src[s_base + 1]) * 0.5;
        } else {
            for c in 0..dst_ch {
                dst[d_base + c] = src[s_base + c.min(src_ch - 1)];
            }
        }
    }
}

fn build_low_latency_udp_receiver(bind_addr: SocketAddr) -> std::io::Result<UdpSocket> {
    let domain = if bind_addr.is_ipv4() {
        Domain::IPV4
    } else {
        Domain::IPV6
    };
    let socket = Socket::new(domain, Type::DGRAM, Some(Protocol::UDP))?;
    let _ = socket.set_reuse_address(true);
    let _ = socket.set_recv_buffer_size(256 * 1024);
    socket.bind(&bind_addr.into())?;
    socket.set_nonblocking(true)?;
    Ok(socket.into())
}

#[cfg(test)]
mod tests {
    use super::*;
    use rsrelay_proto::{FormatFlags, HEADER_LEN};

    #[test]
    fn test_receiver_records_source_addr_on_packet() {
        let probe = UdpSocket::bind("127.0.0.1:0").expect("bind probe");
        let port = probe.local_addr().unwrap().port();
        drop(probe);

        let bind_addr: SocketAddr = format!("127.0.0.1:{port}").parse().unwrap();
        let (producer, _consumer) = rtrb::RingBuffer::new(9600);
        let cfg = ReceiverConfig {
            bind_addr,
            output_sample_rate: 48_000,
            output_channels: ChannelLayout::Stereo,
            target_buffer_ms: 20,
            ringbuffer_capacity_samples: 9600,
        };
        let worker = AudioReceiverWorker::spawn(cfg, producer).expect("spawn receiver");
        assert!(worker.snapshot().source_addr.is_none());

        let tx_sock = UdpSocket::bind("127.0.0.1:0").expect("bind tx");
        let tx_local_addr = tx_sock.local_addr().unwrap();

        let mut flags = FormatFlags::new(CodecType::Opus, ChannelLayout::Stereo, FrameDuration::Ms5);
        flags.is_silence = true;
        let hdr = PacketHeader {
            seq: 1,
            timestamp: 0,
            flags,
            payload_len: 0,
        };
        let mut buf = [0u8; HEADER_LEN];
        hdr.encode(&mut buf);
        tx_sock.send_to(&buf, bind_addr).expect("send packet");

        let start = Instant::now();
        let mut recorded = None;
        while start.elapsed() < Duration::from_millis(500) {
            if let Some(src) = worker.snapshot().source_addr {
                recorded = Some(src);
                break;
            }
            thread::sleep(Duration::from_millis(10));
        }

        assert_eq!(recorded, Some(tx_local_addr));
    }

    #[test]
    fn test_decoupled_asrc_preserves_jitter_cushion_when_rtrb_preloaded() {
        let probe = UdpSocket::bind("127.0.0.1:0").expect("bind probe");
        let port = probe.local_addr().unwrap().port();
        drop(probe);

        let bind_addr: SocketAddr = format!("127.0.0.1:{port}").parse().unwrap();
        let ring_cap = 48_000 * 2 / 2;
        let (mut producer, mut consumer) = rtrb::RingBuffer::new(ring_cap);

        // Preload ~25ms (1200 stereo frames = 2400 samples) into rtrb BEFORE streaming starts,
        // simulating the exact condition where rtrb exceeds target_buffer_ms (20ms).
        // Previously, this squeezed JitterBuffer down to 0 packets and caused massive false losses.
        if let Ok(mut chunk) = producer.write_chunk(2400) {
            let (s1, s2) = chunk.as_mut_slices();
            s1.fill(0.0);
            s2.fill(0.0);
            chunk.commit_all();
        }

        let cfg = ReceiverConfig {
            bind_addr,
            output_sample_rate: 48_000,
            output_channels: ChannelLayout::Stereo,
            target_buffer_ms: 20,
            ringbuffer_capacity_samples: ring_cap,
        };
        let worker = AudioReceiverWorker::spawn(cfg, producer).expect("spawn receiver");

        let tx_sock = UdpSocket::bind("127.0.0.1:0").expect("bind tx");
        let mut enc = rsrelay_dsp::AudioEncoder::new_for_frame(
            CodecType::Opus,
            ChannelLayout::Stereo,
            FrameDuration::Ms5,
            128_000,
            true,
        )
        .expect("opus encoder");

        let frame_pcm = vec![0.15f32; 240 * 2];
        let mut payload = [0u8; MAX_UDP_PAYLOAD];
        let mut datagram = [0u8; MAX_PACKET_SIZE];

        // Stream 60 packets (300ms) at 5ms intervals while draining rtrb slightly slower (-500 ppm)
        let start = Instant::now();
        for seq in 0..60u16 {
            let plen = enc.encode(&frame_pcm, &mut payload).expect("encode");
            let flags = FormatFlags::new(CodecType::Opus, ChannelLayout::Stereo, FrameDuration::Ms5);
            let hdr = PacketHeader {
                seq,
                timestamp: (seq as u32) * 240,
                flags,
                payload_len: plen as u16,
            };
            hdr.encode((&mut datagram[..HEADER_LEN]).try_into().unwrap());
            datagram[HEADER_LEN..HEADER_LEN + plen].copy_from_slice(&payload[..plen]);
            tx_sock
                .send_to(&datagram[..HEADER_LEN + plen], bind_addr)
                .expect("send packet");

            // Simulate slightly slow DAC callback draining ~478 stereo samples (~239 frames) per 5ms
            if seq >= 4 {
                let to_drain = (consumer.slots().min(478) / 2) * 2;
                if to_drain > 0 {
                    if let Ok(c) = consumer.read_chunk(to_drain) {
                        c.commit_all();
                    }
                }
            }

            let next_t = Duration::from_micros(((seq as u64) + 1) * 5_000);
            let elapsed = start.elapsed();
            if next_t > elapsed {
                thread::sleep(next_t - elapsed);
            }
        }

        let snap = worker.snapshot();
        assert!(
            snap.packets_received >= 55,
            "expected >= 55 packets received, got {}",
            snap.packets_received
        );
        assert_eq!(
            snap.packets_lost, 0,
            "JitterBuffer must keep its cushion and incur 0 false packet losses (got {})",
            snap.packets_lost
        );
        assert!(
            snap.buffer_level_ms >= 12.0,
            "JitterBuffer cushion should stay healthy near 20ms, got {:.1} ms",
            snap.buffer_level_ms
        );
    }
}
