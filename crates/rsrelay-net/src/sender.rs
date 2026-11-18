use std::net::{SocketAddr, UdpSocket};
use std::sync::{
    atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering},
    Arc,
};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use rtrb::Consumer;
use socket2::{Domain, Protocol, Socket, Type};

use rsrelay_audio::promote_worker_thread_priority;
use rsrelay_dsp::{opus_packet_can_carry_fec, AdaptiveResampler, AudioEncoder, NoiseGate};
use rsrelay_proto::{
    ChannelLayout, CodecType, FormatFlags, FrameDuration, PacketHeader, HEADER_LEN,
    MAX_PACKET_SIZE, MAX_UDP_PAYLOAD, STANDARD_SAMPLE_RATE,
};

/// Minimum hardware callback inactivity duration before emitting `FLAG_SILENCE` keepalive packets.
/// Must be significantly larger than standard OS audio callback periods (10ms–22ms) to avoid
/// injecting false silence packets between normal hardware callbacks.
const SILENCE_HEARTBEAT_THRESHOLD: Duration = Duration::from_millis(60);

#[derive(Debug, Clone)]
pub struct SenderConfig {
    pub target_addr: SocketAddr,
    pub codec: CodecType,
    pub channels: ChannelLayout,
    pub frame_duration: FrameDuration,
    pub opus_bitrate_bps: u32,
    pub enable_fec: bool,
    pub input_sample_rate: u32,
    pub noise_gate_db: Option<f32>,
}

#[derive(Debug, Default)]
pub struct SenderMetrics {
    pub packets_sent: AtomicU64,
    pub bytes_sent: AtomicU64,
    pub silence_heartbeats_sent: AtomicU64,
    pub gated_dtx_frames: AtomicU64,
}

#[derive(Debug, Clone, Copy, Default)]
pub struct SenderSnapshot {
    pub packets_sent: u64,
    pub bytes_sent: u64,
    pub silence_heartbeats_sent: u64,
    pub gated_dtx_frames: u64,
    pub muted: bool,
}

pub struct AudioSenderWorker {
    stop_flag: Arc<AtomicBool>,
    muted_flag: Arc<AtomicBool>,
    gate_db_bits: Arc<AtomicU32>,
    metrics: Arc<SenderMetrics>,
    join_handle: Option<JoinHandle<()>>,
}

impl AudioSenderWorker {
    pub fn spawn(
        config: SenderConfig,
        mut consumer: Consumer<f32>,
    ) -> Result<Self, anyhow::Error> {
        let socket = build_low_latency_udp_sender(config.target_addr)?;
        let mut encoder = AudioEncoder::new_for_frame(
            config.codec,
            config.channels,
            config.frame_duration,
            config.opus_bitrate_bps,
            config.enable_fec,
        )?;

        let ch = config.channels.channels();
        let target_frames_48k = config.frame_duration.samples_per_channel();
        let frame_duration = Duration::from_micros(config.frame_duration.duration_us());

        // If hardware input rate != 48,000 Hz (e.g. 44,100 Hz), instantiate an input resampler
        let (mut input_resampler, hw_chunk_frames) =
            if config.input_sample_rate != STANDARD_SAMPLE_RATE {
                let hw_frames = ((target_frames_48k as u64 * config.input_sample_rate as u64)
                    / STANDARD_SAMPLE_RATE as u64) as usize;
                let resampler = AdaptiveResampler::new(
                    config.input_sample_rate,
                    STANDARD_SAMPLE_RATE,
                    ch,
                    hw_frames.max(64),
                    1.01,
                )?;
                (Some(resampler), hw_frames.max(64))
            } else {
                (None, target_frames_48k)
            };

        let init_gate_bits = config.noise_gate_db.unwrap_or(f32::NAN).to_bits();
        let stop_flag = Arc::new(AtomicBool::new(false));
        let muted_flag = Arc::new(AtomicBool::new(false));
        let gate_db_bits = Arc::new(AtomicU32::new(init_gate_bits));
        let metrics = Arc::new(SenderMetrics::default());

        let stop_clone = Arc::clone(&stop_flag);
        let muted_clone = Arc::clone(&muted_flag);
        let gate_bits_clone = Arc::clone(&gate_db_bits);
        let metrics_clone = Arc::clone(&metrics);

        let join_handle = thread::Builder::new()
            .name("rsrelay-tx-worker".to_string())
            .spawn(move || {
                promote_worker_thread_priority(STANDARD_SAMPLE_RATE, target_frames_48k as u32);

                let mut last_gate_bits = init_gate_bits;
                let mut noise_gate =
                    NoiseGate::new(config.noise_gate_db, STANDARD_SAMPLE_RATE, ch);

                let hw_chunk_samples = hw_chunk_frames * ch;
                let target_samples_48k = target_frames_48k * ch;

                let mut hw_buf = vec![0.0f32; hw_chunk_samples];
                let mut resampled_staging: Vec<f32> = Vec::with_capacity(target_samples_48k * 8);
                let mut staging_read_idx: usize = 0;
                let mut resampler_out: Vec<f32> = Vec::with_capacity(target_samples_48k * 2);
                let mut encode_in = vec![0.0f32; target_samples_48k];
                let mut payload_buf = [0u8; MAX_UDP_PAYLOAD];
                let mut datagram_buf = [0u8; MAX_PACKET_SIZE];

                let mut seq: u16 = 0;
                let mut timestamp: u32 = 0;
                let mut is_first_packet = true;
                let mut last_audio_instant = Instant::now();
                let mut last_heartbeat_instant = Instant::now();
                let mut next_tick = Instant::now() + frame_duration;

                while !stop_clone.load(Ordering::Relaxed) {
                    // Apply dynamic noise gate threshold updates if changed
                    let cur_gate_bits = gate_bits_clone.load(Ordering::Relaxed);
                    if cur_gate_bits != last_gate_bits {
                        last_gate_bits = cur_gate_bits;
                        let db = f32::from_bits(cur_gate_bits);
                        noise_gate.set_threshold_db(if db.is_nan() { None } else { Some(db) });
                    }

                    // Drain available hardware samples into staging
                    while consumer.slots() >= hw_chunk_samples
                        && (resampled_staging.len() - staging_read_idx) < target_samples_48k * 4
                    {
                        if staging_read_idx > 0 {
                            let rem = resampled_staging.len() - staging_read_idx;
                            if rem == 0 {
                                resampled_staging.clear();
                            } else {
                                resampled_staging.copy_within(staging_read_idx.., 0);
                                resampled_staging.truncate(rem);
                            }
                            staging_read_idx = 0;
                        }

                        if let Ok(chunk) = consumer.read_chunk(hw_chunk_samples) {
                            let (first, second) = chunk.as_slices();
                            hw_buf[..first.len()].copy_from_slice(first);
                            if !second.is_empty() {
                                hw_buf[first.len()..].copy_from_slice(second);
                            }
                            chunk.commit_all();
                            last_audio_instant = Instant::now();
                            last_heartbeat_instant = last_audio_instant;

                            if let Some(ref mut resampler) = input_resampler {
                                if resampler
                                    .process_interleaved(&hw_buf, 1.0, &mut resampler_out)
                                    .is_ok()
                                {
                                    resampled_staging.extend_from_slice(&resampler_out);
                                }
                            } else {
                                resampled_staging.extend_from_slice(&hw_buf);
                            }
                        }
                    }

                    // Transmit all full 48kHz frames ready in staging
                    while (resampled_staging.len() - staging_read_idx) >= target_samples_48k {
                        encode_in.copy_from_slice(
                            &resampled_staging
                                [staging_read_idx..staging_read_idx + target_samples_48k],
                        );
                        staging_read_idx += target_samples_48k;
                        if staging_read_idx == resampled_staging.len() {
                            resampled_staging.clear();
                            staging_read_idx = 0;
                        }

                        let is_muted = muted_clone.load(Ordering::Relaxed);
                        let gate_active = !f32::from_bits(last_gate_bits).is_nan();
                        let is_gated_silent =
                            noise_gate.process_interleaved(&mut encode_in, is_muted);

                        if is_gated_silent && (is_muted || gate_active) {
                            // Discontinuous Transmission (DTX): send 12B FLAG_SILENCE packet
                            let mut flags = FormatFlags::new(
                                config.codec,
                                config.channels,
                                config.frame_duration,
                            );
                            flags.is_silence = true;
                            if is_first_packet {
                                flags.is_reset = true;
                                is_first_packet = false;
                            }
                            let hdr = PacketHeader {
                                seq,
                                timestamp,
                                flags,
                                payload_len: 0,
                            };
                            hdr.encode((&mut datagram_buf[..HEADER_LEN]).try_into().unwrap());
                            if socket
                                .send_to(&datagram_buf[..HEADER_LEN], config.target_addr)
                                .is_ok()
                            {
                                metrics_clone.packets_sent.fetch_add(1, Ordering::Relaxed);
                                metrics_clone
                                    .bytes_sent
                                    .fetch_add(HEADER_LEN as u64, Ordering::Relaxed);
                                metrics_clone
                                    .gated_dtx_frames
                                    .fetch_add(1, Ordering::Relaxed);
                            }
                            seq = seq.wrapping_add(1);
                            timestamp = timestamp.wrapping_add(target_frames_48k as u32);
                            continue;
                        }

                        if let Ok(payload_len) = encoder.encode(&encode_in, &mut payload_buf) {
                            let mut flags = FormatFlags::new(
                                config.codec,
                                config.channels,
                                config.frame_duration,
                            );
                            flags.has_fec = config.enable_fec
                                && config.codec == CodecType::Opus
                                && opus_packet_can_carry_fec(&payload_buf[..payload_len]);
                            if is_first_packet {
                                flags.is_reset = true;
                                is_first_packet = false;
                            }

                            let hdr = PacketHeader {
                                seq,
                                timestamp,
                                flags,
                                payload_len: payload_len as u16,
                            };
                            hdr.encode((&mut datagram_buf[..HEADER_LEN]).try_into().unwrap());
                            datagram_buf[HEADER_LEN..HEADER_LEN + payload_len]
                                .copy_from_slice(&payload_buf[..payload_len]);

                            let total_len = HEADER_LEN + payload_len;
                            if socket
                                .send_to(&datagram_buf[..total_len], config.target_addr)
                                .is_ok()
                            {
                                metrics_clone.packets_sent.fetch_add(1, Ordering::Relaxed);
                                metrics_clone
                                    .bytes_sent
                                    .fetch_add(total_len as u64, Ordering::Relaxed);
                            }

                            seq = seq.wrapping_add(1);
                            timestamp = timestamp.wrapping_add(target_frames_48k as u32);
                        }
                    }

                    // WASAPI Silence Guard: if hardware callback has genuinely paused for >= 60ms
                    // and no samples remain in staging, transmit a paced 12-byte FLAG_SILENCE heartbeat
                    // every `frame_duration` so receiver PLL stays locked.
                    if (resampled_staging.len() - staging_read_idx) < target_samples_48k
                        && last_audio_instant.elapsed() >= SILENCE_HEARTBEAT_THRESHOLD
                        && last_heartbeat_instant.elapsed() >= frame_duration
                    {
                        let mut flags = FormatFlags::new(
                            config.codec,
                            config.channels,
                            config.frame_duration,
                        );
                        flags.is_silence = true;
                        let hdr = PacketHeader {
                            seq,
                            timestamp,
                            flags,
                            payload_len: 0,
                        };
                        hdr.encode((&mut datagram_buf[..HEADER_LEN]).try_into().unwrap());
                        if socket
                            .send_to(&datagram_buf[..HEADER_LEN], config.target_addr)
                            .is_ok()
                        {
                            metrics_clone.packets_sent.fetch_add(1, Ordering::Relaxed);
                            metrics_clone
                                .bytes_sent
                                .fetch_add(HEADER_LEN as u64, Ordering::Relaxed);
                            metrics_clone
                                .silence_heartbeats_sent
                                .fetch_add(1, Ordering::Relaxed);
                        }
                        seq = seq.wrapping_add(1);
                        timestamp = timestamp.wrapping_add(target_frames_48k as u32);
                        last_heartbeat_instant += frame_duration;
                        if last_heartbeat_instant.elapsed() > frame_duration * 4 {
                            last_heartbeat_instant = Instant::now();
                        }
                    }

                    let now = Instant::now();
                    if next_tick > now {
                        thread::sleep((next_tick - now).min(Duration::from_millis(1)));
                    }
                    if Instant::now() >= next_tick {
                        next_tick = Instant::now() + (frame_duration / 2);
                    }
                }
            })?;

        Ok(Self {
            stop_flag,
            muted_flag,
            gate_db_bits,
            metrics,
            join_handle: Some(join_handle),
        })
    }

    #[inline]
    pub fn set_muted(&self, muted: bool) {
        self.muted_flag.store(muted, Ordering::Relaxed);
    }

    #[inline]
    pub fn is_muted(&self) -> bool {
        self.muted_flag.load(Ordering::Relaxed)
    }

    #[inline]
    pub fn set_noise_gate_db(&self, threshold_db: Option<f32>) {
        let bits = threshold_db.unwrap_or(f32::NAN).to_bits();
        self.gate_db_bits.store(bits, Ordering::Relaxed);
    }

    pub fn snapshot(&self) -> SenderSnapshot {
        SenderSnapshot {
            packets_sent: self.metrics.packets_sent.load(Ordering::Relaxed),
            bytes_sent: self.metrics.bytes_sent.load(Ordering::Relaxed),
            silence_heartbeats_sent: self.metrics.silence_heartbeats_sent.load(Ordering::Relaxed),
            gated_dtx_frames: self.metrics.gated_dtx_frames.load(Ordering::Relaxed),
            muted: self.muted_flag.load(Ordering::Relaxed),
        }
    }
}

impl Drop for AudioSenderWorker {
    fn drop(&mut self) {
        self.stop_flag.store(true, Ordering::Relaxed);
        if let Some(handle) = self.join_handle.take() {
            let _ = handle.join();
        }
    }
}

fn build_low_latency_udp_sender(target: SocketAddr) -> std::io::Result<UdpSocket> {
    let domain = if target.is_ipv4() {
        Domain::IPV4
    } else {
        Domain::IPV6
    };
    let socket = Socket::new(domain, Type::DGRAM, Some(Protocol::UDP))?;
    let _ = socket.set_send_buffer_size(256 * 1024);
    // Set DSCP Expedited Forwarding (EF = 46 -> TOS 0xB8) for Wi-Fi WMM Voice prioritization
    if target.is_ipv4() {
        let _ = socket.set_tos(0xB8);
    }
    let bind_addr: SocketAddr = if target.is_ipv4() {
        "0.0.0.0:0".parse().unwrap()
    } else {
        "[::]:0".parse().unwrap()
    };
    socket.bind(&bind_addr.into())?;
    socket.set_nonblocking(true)?;
    Ok(socket.into())
}
