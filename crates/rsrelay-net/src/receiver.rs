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
        let current_channels = config.output_channels;
        let mut current_frame_dur = FrameDuration::Ms5;
        let ch = current_channels.channels();
        let frames_per_pkt = current_frame_dur.samples_per_channel();

        let mut decoder = AudioDecoder::new(current_codec, current_channels)?;
        let mut resampler = AdaptiveResampler::new(
            STANDARD_SAMPLE_RATE,
            config.output_sample_rate,
            ch,
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

                let target_rtrb_frames = ((config.target_buffer_ms as usize)
                    * (config.output_sample_rate as usize))
                    / 1000;
                let mut drift_ctrl = ClockDriftController::new(target_rtrb_frames.max(480));
                let mut current_rel_ratio = 1.0f64;

                promote_worker_thread_priority(config.output_sample_rate, frames_per_pkt as u32);

                let mut udp_buf = [0u8; MAX_PACKET_SIZE];
                let mut pkt_payload = [0u8; MAX_UDP_PAYLOAD];
                let mut decoded_pcm = vec![0.0f32; frames_per_pkt * ch];
                let mut resampled_pcm = Vec::with_capacity(frames_per_pkt * ch * 2);

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

                                    // Reconfigure pipeline on the fly if sender changed codec or frame size
                                    if hdr.flags.codec != current_codec
                                        || hdr.flags.frame_duration != current_frame_dur
                                    {
                                        current_codec = hdr.flags.codec;
                                        current_frame_dur = hdr.flags.frame_duration;
                                        let new_prefetch = ((config.target_buffer_ms as u64 * 1000)
                                            / current_frame_dur.duration_us())
                                        .max(2)
                                            as usize;
                                        jitter.set_target_prefetch(new_prefetch);

                                        if let Ok(new_dec) =
                                            AudioDecoder::new(current_codec, current_channels)
                                        {
                                            decoder = new_dec;
                                        }
                                        let new_frames = current_frame_dur.samples_per_channel();
                                        decoded_pcm.resize(new_frames * ch, 0.0);
                                        if let Ok(new_res) = AdaptiveResampler::new(
                                            STANDARD_SAMPLE_RATE,
                                            config.output_sample_rate,
                                            ch,
                                            new_frames,
                                            1.01,
                                        ) {
                                            resampler = new_res;
                                        }
                                    }

                                    jitter.push(hdr, payload);
                                }
                            }
                            Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => break,
                            Err(_) => break,
                        }
                    }

                    // 2. When clock-drift-modulated frame deadline arrives, pop from JitterBuffer,
                    //    decode, and resample into the playback ring buffer.
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
                            * current_rel_ratio)
                            as u64;
                        let step_duration = Duration::from_nanos(dynamic_step_ns.max(500_000));
                        next_deadline += step_duration;
                        if now > next_deadline + step_duration * 4 {
                            // Recover timer if OS suspended or thread was delayed
                            next_deadline = now + step_duration;
                        }

                        let outcome = jitter.pop(&mut pkt_payload);
                        let has_frame = match outcome {
                            PopOutcome::Ready { payload_len, .. } => decoder
                                .decode(&pkt_payload[..payload_len], &mut decoded_pcm)
                                .is_ok(),
                            PopOutcome::SilenceHeartbeat { .. } => {
                                decoded_pcm.fill(0.0);
                                true
                            }
                            PopOutcome::MissingWithFec { fec_payload_len } => decoder
                                .conceal_loss(
                                    Some(&pkt_payload[..fec_payload_len]),
                                    &mut decoded_pcm,
                                )
                                .is_ok(),
                            PopOutcome::MissingNoFec => {
                                decoder.conceal_loss(None, &mut decoded_pcm).is_ok()
                            }
                            PopOutcome::Buffering => false,
                        };

                        if has_frame {
                            let rtrb_queued_samples = config
                                .ringbuffer_capacity_samples
                                .saturating_sub(producer.slots());
                            let rtrb_queued_frames = rtrb_queued_samples / ch.max(1);
                            let jb_frames_48k =
                                jitter.occupied_count() * current_frame_dur.samples_per_channel();
                            let jb_queued_frames = (jb_frames_48k
                                * (config.output_sample_rate as usize))
                                / (STANDARD_SAMPLE_RATE as usize);
                            let total_buffered_frames = rtrb_queued_frames + jb_queued_frames;

                            let rel_ratio = drift_ctrl.update(total_buffered_frames);
                            current_rel_ratio = rel_ratio;

                            if resampler
                                .process_interleaved(&decoded_pcm, rel_ratio, &mut resampled_pcm)
                                .is_ok()
                            {
                                let avail = producer.slots();
                                let to_write = resampled_pcm.len().min(avail);
                                if to_write > 0 {
                                    if let Ok(mut chunk) = producer.write_chunk(to_write) {
                                        let (first, second) = chunk.as_mut_slices();
                                        first.copy_from_slice(&resampled_pcm[..first.len()]);
                                        if !second.is_empty() {
                                            second.copy_from_slice(
                                                &resampled_pcm[first.len()..to_write],
                                            );
                                        }
                                        chunk.commit_all();
                                    }
                                }
                            }

                            // Update atomic telemetry
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

                            let level_ms = (drift_ctrl.smoothed_frames() as f32 * 1000.0)
                                / (config.output_sample_rate as f32);
                            metrics_clone
                                .buffer_level_ms_bits
                                .store(level_ms.to_bits(), Ordering::Relaxed);
                            metrics_clone
                                .resample_ratio_bits
                                .store(rel_ratio.to_bits(), Ordering::Relaxed);
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
}
