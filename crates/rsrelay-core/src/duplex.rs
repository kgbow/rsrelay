use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};

use rsrelay_audio::CaptureSource;
use rsrelay_proto::{
    ChannelLayout, DuplexRole, DEFAULT_AUDIO_UDP_PORT, DEFAULT_UPLINK_UDP_PORT,
};

use crate::config::{LatencyProfile, ReceiverSessionConfig, SenderSessionConfig};
use crate::engine::{
    ReceiverSession, ReceiverSessionStatus, SenderSession, SenderSessionStatus,
};

/// Configuration for a Full-Duplex Bidirectional Session (Wireless Headset Mode).
///
/// - **`DuplexRole::Host` (Main Computer)**:
///   - **TX (Downlink -> `peer_ip:downlink_udp_port`)**: Captures `SystemLoopback` (stereo 48kHz) and streams to Headset.
///   - **RX (Uplink <- `0.0.0.0:uplink_udp_port`)**: Listens for Headset's Microphone stream and plays to local speaker or virtual mic (`VB-Cable` / `BlackHole`).
/// - **`DuplexRole::Headset` (Wireless Headset Device)**:
///   - **RX (Downlink <- `0.0.0.0:downlink_udp_port`)**: Listens for Host's System Audio and plays to local headphones.
///   - **TX (Uplink -> `peer_ip:uplink_udp_port`)**: Captures local `Microphone` (with Attack/Hold/Release Noise Gate & Mute) and streams back to Host.
#[derive(Debug, Clone)]
pub struct DuplexSessionConfig {
    pub role: DuplexRole,
    pub peer_ip: IpAddr,
    pub downlink_udp_port: u16,
    pub uplink_udp_port: u16,
    pub profile: LatencyProfile,
    pub tx_device_name: Option<String>,
    pub rx_device_name: Option<String>,
    pub mic_noise_gate_db: Option<f32>,
    pub initial_volume: f32,
    pub advertise_mdns: bool,
    pub synthetic_source_override: bool,
}

impl DuplexSessionConfig {
    pub fn new(role: DuplexRole, peer_ip: IpAddr) -> Self {
        Self {
            role,
            peer_ip,
            downlink_udp_port: DEFAULT_AUDIO_UDP_PORT,
            uplink_udp_port: DEFAULT_UPLINK_UDP_PORT,
            profile: LatencyProfile::Balanced,
            tx_device_name: None,
            rx_device_name: None,
            mic_noise_gate_db: Some(-45.0),
            initial_volume: 1.0,
            advertise_mdns: true,
            synthetic_source_override: false,
        }
    }
}

#[derive(Debug, Clone)]
pub struct DuplexSessionStatus {
    pub role: DuplexRole,
    pub tx: SenderSessionStatus,
    pub rx: ReceiverSessionStatus,
}

/// Full-duplex bidirectional audio session combining simultaneous TX and RX pipelines.
pub struct DuplexSession {
    role: DuplexRole,
    tx_session: SenderSession,
    rx_session: ReceiverSession,
}

impl DuplexSession {
    pub fn start(cfg: DuplexSessionConfig) -> anyhow::Result<Self> {
        let (tx_target_port, rx_bind_port, tx_source, tx_gate_db, opus_bitrate_bps) = match cfg.role
        {
            DuplexRole::Host => (
                cfg.downlink_udp_port,
                cfg.uplink_udp_port,
                if cfg.synthetic_source_override {
                    CaptureSource::SyntheticTone
                } else {
                    CaptureSource::SystemLoopback
                },
                None, // Host system audio stream is not noise-gated by default
                128_000,
            ),
            DuplexRole::Headset => (
                cfg.uplink_udp_port,
                cfg.downlink_udp_port,
                if cfg.synthetic_source_override {
                    CaptureSource::SyntheticTone
                } else {
                    CaptureSource::Microphone
                },
                cfg.mic_noise_gate_db,
                64_000, // Voice uplink requires lower bitrate while preserving full 48kHz clarity
            ),
        };

        let unspecified_ip = if cfg.peer_ip.is_ipv6() {
            IpAddr::V6(Ipv6Addr::UNSPECIFIED)
        } else {
            IpAddr::V4(Ipv4Addr::UNSPECIFIED)
        };

        let rx_cfg = ReceiverSessionConfig {
            bind_addr: SocketAddr::new(unspecified_ip, rx_bind_port),
            device_name: cfg.rx_device_name,
            channels: ChannelLayout::Stereo,
            target_buffer_ms: cfg.profile.default_target_buffer_ms(),
            initial_volume: cfg.initial_volume,
            advertise_mdns: cfg.advertise_mdns,
            node_name: format!("rsrelay-duplex-{:?}", cfg.role).to_lowercase(),
        };
        let rx_session = ReceiverSession::start(rx_cfg)?;

        let tx_cfg = SenderSessionConfig {
            target_addr: SocketAddr::new(cfg.peer_ip, tx_target_port),
            source: tx_source,
            device_name: cfg.tx_device_name,
            codec: cfg.profile.default_codec(),
            channels: ChannelLayout::Stereo,
            frame_duration: cfg.profile.default_frame_duration(),
            opus_bitrate_bps,
            enable_fec: cfg.profile.default_fec(),
            noise_gate_db: tx_gate_db,
        };
        let tx_session = SenderSession::start(tx_cfg)?;

        Ok(Self {
            role: cfg.role,
            tx_session,
            rx_session,
        })
    }

    pub const fn role(&self) -> DuplexRole {
        self.role
    }

    /// Mute or unmute the outgoing TX audio stream (e.g. Headset Microphone Push-to-Mute).
    pub fn set_tx_muted(&self, muted: bool) {
        self.tx_session.set_muted(muted);
    }

    pub fn is_tx_muted(&self) -> bool {
        self.tx_session.is_muted()
    }

    /// Adjust the incoming RX playback volume (`0.0..=2.0`).
    pub fn set_rx_volume(&self, gain: f32) {
        self.rx_session.set_volume(gain);
    }

    /// Dynamically adjust the outgoing microphone noise gate threshold in dBFS.
    pub fn set_tx_noise_gate_db(&self, threshold_db: Option<f32>) {
        self.tx_session.set_noise_gate_db(threshold_db);
    }

    pub fn status(&self) -> DuplexSessionStatus {
        DuplexSessionStatus {
            role: self.role,
            tx: self.tx_session.status(),
            rx: self.rx_session.status(),
        }
    }
}
