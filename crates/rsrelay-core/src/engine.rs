use rtrb::RingBuffer;

use rsrelay_audio::{
    start_capture_stream, start_playback_stream, CaptureStreamHandle, PlaybackStreamHandle,
};
use rsrelay_net::{
    AudioReceiverWorker, AudioSenderWorker, DiscoveryService, ReceiverConfig, ReceiverSnapshot,
    SenderConfig, SenderSnapshot,
};

use crate::config::{ReceiverSessionConfig, SenderSessionConfig};

/// Live status snapshot for an active `SenderSession`.
#[derive(Debug, Clone)]
pub struct SenderSessionStatus {
    pub device_name: String,
    pub sample_rate: u32,
    pub peak_level: f32,
    pub net: SenderSnapshot,
}

/// High-level sender session managing audio capture, lock-free SPSC ring buffer, and UDP encoding worker.
pub struct SenderSession {
    capture: CaptureStreamHandle,
    worker: AudioSenderWorker,
}

impl SenderSession {
    pub fn start(cfg: SenderSessionConfig) -> anyhow::Result<Self> {
        let ch_count = cfg.channels.channels() as u16;
        // Pre-allocate 500ms of lock-free ring buffer capacity (~192 KB for 48kHz stereo f32)
        let ring_capacity = 48_000 * (ch_count as usize) / 2;
        let (producer, consumer) = RingBuffer::<f32>::new(ring_capacity);

        let capture = start_capture_stream(
            cfg.source,
            cfg.device_name.as_deref(),
            ch_count,
            producer,
        )?;

        let sender_cfg = SenderConfig {
            target_addr: cfg.target_addr,
            codec: cfg.codec,
            channels: cfg.channels,
            frame_duration: cfg.frame_duration,
            opus_bitrate_bps: cfg.opus_bitrate_bps,
            enable_fec: cfg.enable_fec,
            input_sample_rate: capture.sample_rate,
        };

        let worker = AudioSenderWorker::spawn(sender_cfg, consumer)?;

        Ok(Self { capture, worker })
    }

    pub fn status(&self) -> SenderSessionStatus {
        SenderSessionStatus {
            device_name: self.capture.device_name.clone(),
            sample_rate: self.capture.sample_rate,
            peak_level: self.capture.peak_level(),
            net: self.worker.snapshot(),
        }
    }
}

/// Live status snapshot for an active `ReceiverSession`.
#[derive(Debug, Clone)]
pub struct ReceiverSessionStatus {
    pub device_name: String,
    pub sample_rate: u32,
    pub peak_level: f32,
    pub volume: f32,
    pub audio_underruns: u64,
    pub net: ReceiverSnapshot,
}

/// High-level receiver session managing UDP jitter buffer, Opus FEC/PLC, PI clock-drift ASRC,
/// lock-free ring buffer, real-time speaker playback, and optional mDNS advertisement.
pub struct ReceiverSession {
    playback: PlaybackStreamHandle,
    worker: AudioReceiverWorker,
    _mdns: Option<DiscoveryService>,
}

impl ReceiverSession {
    pub fn start(cfg: ReceiverSessionConfig) -> anyhow::Result<Self> {
        let ch_count = cfg.channels.channels() as u16;
        // Pre-allocate 500ms of lock-free playback ring buffer
        let ring_capacity = 48_000 * (ch_count as usize) / 2;
        let (producer, consumer) = RingBuffer::<f32>::new(ring_capacity);

        let playback = start_playback_stream(cfg.device_name.as_deref(), ch_count, consumer)?;
        playback.set_volume(cfg.initial_volume);

        let rx_cfg = ReceiverConfig {
            bind_addr: cfg.bind_addr,
            target_buffer_ms: cfg.target_buffer_ms,
            output_sample_rate: playback.sample_rate,
            output_channels: cfg.channels,
            ringbuffer_capacity_samples: ring_capacity,
        };

        let worker = AudioReceiverWorker::spawn(rx_cfg, producer)?;

        let mdns = if cfg.advertise_mdns {
            match DiscoveryService::new() {
                Ok(mut svc) => {
                    if let Err(e) = svc.advertise(&cfg.node_name, cfg.bind_addr.port()) {
                        tracing::warn!("mDNS advertisement failed: {e}");
                    }
                    Some(svc)
                }
                Err(e) => {
                    tracing::warn!("Failed to start mDNS daemon: {e}");
                    None
                }
            }
        } else {
            None
        };

        Ok(Self {
            playback,
            worker,
            _mdns: mdns,
        })
    }

    pub fn set_volume(&self, gain: f32) {
        self.playback.set_volume(gain);
    }

    pub fn status(&self) -> ReceiverSessionStatus {
        ReceiverSessionStatus {
            device_name: self.playback.device_name.clone(),
            sample_rate: self.playback.sample_rate,
            peak_level: self.playback.peak_level(),
            volume: self.playback.volume(),
            audio_underruns: self.playback.underrun_count(),
            net: self.worker.snapshot(),
        }
    }
}
