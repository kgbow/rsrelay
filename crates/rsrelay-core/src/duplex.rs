//! Orthogonal Dual-Bus Cross-Machine Audio & Microphone Sharing Engine.
//!
//! Decouples cross-machine audio into two independent, zero-crosstalk semantic buses:
//! 1. **Media / Speaker Bus (`downlink_udp_port`, default `:49153`)**:
//!    - `TX`: Captures desktop `SystemLoopback` (Windows WASAPI Loopback / macOS `ScreenCaptureKit`).
//!    - `RX`: Plays out to physical headphones / speakers.
//! 2. **Voice / Virtual Mic Bus (`uplink_udp_port`, default `:49155`)**:
//!    - `TX`: Captures physical `Microphone` (automatically skipping virtual loopback inputs) with
//!      Attack/Hold/Release Noise Gate, DTX, and push-to-mute.
//!    - `RX`: Injects voice **exclusively** into a Virtual Microphone (`rsrelay Virtual Mic Sink` on macOS,
//!      `CABLE Input` / `Virtual Mic` on Windows). If no virtual audio driver is installed yet,
//!      engages `NULL_VIRTUAL_MIC_DEVICE_NAME` (Echo-Free Guard) so incoming voice is metered live
//!      without ever leaking into physical speakers or looping back into `SystemLoopback`.

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};

use anyhow::bail;
use rsrelay_audio::{
    is_null_virtual_mic_filter, select_default_virtual_mic_output_device, CaptureSource,
    NULL_VIRTUAL_MIC_DEVICE_NAME,
};
use rsrelay_proto::{
    ChannelLayout, DuplexRole, DEFAULT_AUDIO_UDP_PORT, DEFAULT_UPLINK_UDP_PORT,
};
use serde::{Deserialize, Serialize};

use crate::config::{LatencyProfile, ReceiverSessionConfig, SenderSessionConfig};
use crate::engine::{
    ReceiverSession, ReceiverSessionStatus, SenderSession, SenderSessionStatus,
};

/// Routing mode for the System Audio / Player Bus (`:49153`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum MediaBusMode {
    /// Player sharing disabled on this machine.
    Off,
    /// Capture local system audio (`SystemLoopback`) and send to peer's speaker.
    SendLoopback,
    /// Receive peer's system audio and play out to local headphones / speakers.
    ReceiveSpeaker,
    /// Simultaneously send local system audio to peer AND listen to peer's system audio.
    Bidirectional,
}

impl MediaBusMode {
    pub const fn sends(self) -> bool {
        matches!(self, Self::SendLoopback | Self::Bidirectional)
    }

    pub const fn receives(self) -> bool {
        matches!(self, Self::ReceiveSpeaker | Self::Bidirectional)
    }

    pub const fn cycle(self) -> Self {
        match self {
            Self::ReceiveSpeaker => Self::SendLoopback,
            Self::SendLoopback => Self::Bidirectional,
            Self::Bidirectional => Self::Off,
            Self::Off => Self::ReceiveSpeaker,
        }
    }
}

/// Routing mode for the Microphone / Virtual Mic Bus (`:49155`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum MicBusMode {
    /// Microphone sharing disabled on this machine.
    Off,
    /// Capture local physical microphone and send to peer's Virtual Mic.
    SendMic,
    /// Receive peer's microphone and inject into local Virtual Mic (0% speaker echo).
    ReceiveVirtualMic,
    /// Simultaneously send local physical mic to peer AND inject peer's mic into local Virtual Mic.
    Bidirectional,
}

impl MicBusMode {
    pub const fn sends(self) -> bool {
        matches!(self, Self::SendMic | Self::Bidirectional)
    }

    pub const fn receives(self) -> bool {
        matches!(self, Self::ReceiveVirtualMic | Self::Bidirectional)
    }

    pub const fn cycle(self) -> Self {
        match self {
            Self::SendMic => Self::ReceiveVirtualMic,
            Self::ReceiveVirtualMic => Self::Bidirectional,
            Self::Bidirectional => Self::Off,
            Self::Off => Self::SendMic,
        }
    }
}

/// Return the default `(MediaBusMode, MicBusMode)` pair for a given [`DuplexRole`] preset.
pub const fn preset_bus_modes_for_role(role: DuplexRole) -> (MediaBusMode, MicBusMode) {
    match role {
        DuplexRole::Host => (MediaBusMode::SendLoopback, MicBusMode::ReceiveVirtualMic),
        DuplexRole::Headset => (MediaBusMode::ReceiveSpeaker, MicBusMode::SendMic),
        DuplexRole::Symmetric => (MediaBusMode::Bidirectional, MicBusMode::Bidirectional),
    }
}

/// Configuration for a Cross-Machine Full-Duplex / Dual-Bus Session (Windows <-> macOS).
#[derive(Debug, Clone)]
pub struct DuplexSessionConfig {
    pub role: DuplexRole,
    pub media_mode: MediaBusMode,
    pub mic_mode: MicBusMode,
    pub peer_ip: IpAddr,
    pub downlink_udp_port: u16,
    pub uplink_udp_port: u16,
    pub profile: LatencyProfile,

    /// Legacy/primary TX device override (mapped to active TX bus when per-bus override is `None`).
    pub tx_device_name: Option<String>,
    /// Legacy/primary RX device override (mapped to active RX bus when per-bus override is `None`).
    pub rx_device_name: Option<String>,

    /// Explicit loopback capture device override for Media TX (`:49153`).
    pub media_tx_device: Option<String>,
    /// Explicit physical speaker output device override for Media RX (`:49153`).
    pub media_rx_device: Option<String>,
    /// Explicit physical microphone input device override for Mic TX (`:49155`).
    pub mic_tx_device: Option<String>,
    /// Explicit Virtual Mic sink override for Mic RX (`:49155`).
    pub mic_rx_device: Option<String>,

    pub media_channels: ChannelLayout,
    pub mic_channels: ChannelLayout,
    pub mic_noise_gate_db: Option<f32>,
    pub initial_volume: f32,
    pub mic_rx_gain: f32,
    pub advertise_mdns: bool,
    pub synthetic_source_override: bool,
    pub target_buffer_ms_override: Option<u16>,
    pub media_opus_bitrate_bps_override: Option<u32>,
}

impl DuplexSessionConfig {
    pub fn new(role: DuplexRole, peer_ip: IpAddr) -> Self {
        let (media_mode, mic_mode) = preset_bus_modes_for_role(role);
        Self {
            role,
            media_mode,
            mic_mode,
            peer_ip,
            downlink_udp_port: DEFAULT_AUDIO_UDP_PORT,
            uplink_udp_port: DEFAULT_UPLINK_UDP_PORT,
            profile: LatencyProfile::Balanced,
            tx_device_name: None,
            rx_device_name: None,
            media_tx_device: None,
            media_rx_device: None,
            mic_tx_device: None,
            mic_rx_device: None,
            media_channels: ChannelLayout::Stereo,
            mic_channels: ChannelLayout::Stereo,
            mic_noise_gate_db: Some(-45.0),
            initial_volume: 1.0,
            mic_rx_gain: 1.0,
            advertise_mdns: true,
            synthetic_source_override: false,
            target_buffer_ms_override: None,
            media_opus_bitrate_bps_override: None,
        }
    }

    /// Update `role` and apply its preset bus modes.
    pub fn set_role_preset(&mut self, role: DuplexRole) {
        self.role = role;
        let (media, mic) = preset_bus_modes_for_role(role);
        self.media_mode = media;
        self.mic_mode = mic;
    }
}

#[derive(Debug, Clone)]
pub struct DuplexSessionStatus {
    pub role: DuplexRole,
    pub media_mode: MediaBusMode,
    pub mic_mode: MicBusMode,
    pub media_tx: Option<SenderSessionStatus>,
    pub media_rx: Option<ReceiverSessionStatus>,
    pub mic_tx: Option<SenderSessionStatus>,
    pub mic_rx: Option<ReceiverSessionStatus>,
    /// `true` when Mic RX is either disabled or actively bridged into a real Virtual Mic driver;
    /// `false` when Mic RX fell back to the Echo-Free Guard null sink because no driver is installed.
    pub virtual_mic_ready: bool,
}

impl DuplexSessionStatus {
    /// Primary active TX stream status (for compact CLI / header RTT display).
    pub fn primary_tx(&self) -> Option<&SenderSessionStatus> {
        match self.role {
            DuplexRole::Host => self.media_tx.as_ref().or(self.mic_tx.as_ref()),
            DuplexRole::Headset | DuplexRole::Symmetric => {
                self.mic_tx.as_ref().or(self.media_tx.as_ref())
            }
        }
    }

    /// Primary active RX stream status (for compact CLI summary).
    pub fn primary_rx(&self) -> Option<&ReceiverSessionStatus> {
        match self.role {
            DuplexRole::Host => self.mic_rx.as_ref().or(self.media_rx.as_ref()),
            DuplexRole::Headset | DuplexRole::Symmetric => {
                self.media_rx.as_ref().or(self.mic_rx.as_ref())
            }
        }
    }

    /// Best available measured TCP control-plane RTT in microseconds across active TX streams.
    pub fn best_rtt_us(&self) -> Option<u64> {
        for tx in [&self.media_tx, &self.mic_tx].into_iter().flatten() {
            if tx.control.connected {
                return Some(tx.control.rtt_us);
            }
        }
        None
    }
}

/// Multi-bus bidirectional audio & virtual microphone session.
pub struct DuplexSession {
    role: DuplexRole,
    media_mode: MediaBusMode,
    mic_mode: MicBusMode,
    media_tx: Option<SenderSession>,
    media_rx: Option<ReceiverSession>,
    mic_tx: Option<SenderSession>,
    mic_rx: Option<ReceiverSession>,
    virtual_mic_ready: bool,
}

impl DuplexSession {
    pub fn start(cfg: DuplexSessionConfig) -> anyhow::Result<Self> {
        if !cfg.media_mode.sends()
            && !cfg.media_mode.receives()
            && !cfg.mic_mode.sends()
            && !cfg.mic_mode.receives()
        {
            bail!("At least one player or microphone stream direction must be enabled");
        }

        let unspecified_ip = if cfg.peer_ip.is_ipv6() {
            IpAddr::V6(Ipv6Addr::UNSPECIFIED)
        } else {
            IpAddr::V4(Ipv4Addr::UNSPECIFIED)
        };

        let mut mdns_claimed = false;
        let mut take_mdns = || {
            if cfg.advertise_mdns && !mdns_claimed {
                mdns_claimed = true;
                true
            } else {
                false
            }
        };

        let effective_buffer_ms = cfg
            .target_buffer_ms_override
            .unwrap_or_else(|| cfg.profile.default_target_buffer_ms());

        // 1. Media RX (`:49153` -> Physical Headphones / Speaker)
        let media_rx = if cfg.media_mode.receives() {
            let dev = cfg.media_rx_device.clone().or_else(|| {
                if cfg.role == DuplexRole::Headset {
                    cfg.rx_device_name.clone()
                } else {
                    None
                }
            });
            let rx_cfg = ReceiverSessionConfig {
                bind_addr: SocketAddr::new(unspecified_ip, cfg.downlink_udp_port),
                device_name: dev,
                channels: cfg.media_channels,
                target_buffer_ms: effective_buffer_ms,
                initial_volume: cfg.initial_volume,
                advertise_mdns: take_mdns(),
                node_name: format!("rsrelay-media-{:?}", cfg.role).to_lowercase(),
            };
            Some(ReceiverSession::start(rx_cfg)?)
        } else {
            None
        };

        // 2. Mic RX (`:49155` -> Virtual Microphone ONLY; Echo-Free Guard if no driver installed)
        let mut virtual_mic_ready = true;
        let mic_rx = if cfg.mic_mode.receives() {
            let explicit_vmic = cfg.mic_rx_device.clone().or_else(|| {
                if cfg.role == DuplexRole::Host {
                    cfg.rx_device_name.clone()
                } else {
                    None
                }
            });
            let resolved_vmic = match explicit_vmic {
                Some(dev) => {
                    if is_null_virtual_mic_filter(&dev) {
                        virtual_mic_ready = false;
                    }
                    dev
                }
                None => match select_default_virtual_mic_output_device() {
                    Some(auto_vmic) => auto_vmic,
                    None => {
                        // Never fall back to physical speakers when receiving a microphone stream!
                        virtual_mic_ready = false;
                        NULL_VIRTUAL_MIC_DEVICE_NAME.to_string()
                    }
                },
            };

            let rx_cfg = ReceiverSessionConfig {
                bind_addr: SocketAddr::new(unspecified_ip, cfg.uplink_udp_port),
                device_name: Some(resolved_vmic),
                channels: cfg.mic_channels,
                target_buffer_ms: effective_buffer_ms,
                initial_volume: cfg.mic_rx_gain,
                advertise_mdns: take_mdns(),
                node_name: format!("rsrelay-vmic-{:?}", cfg.role).to_lowercase(),
            };
            Some(ReceiverSession::start(rx_cfg)?)
        } else {
            None
        };

        // 3. Media TX (`SystemLoopback` -> `peer_ip:49153`)
        let media_tx = if cfg.media_mode.sends() {
            let dev = cfg.media_tx_device.clone().or_else(|| {
                if cfg.role == DuplexRole::Host {
                    cfg.tx_device_name.clone()
                } else {
                    None
                }
            });
            let tx_cfg = SenderSessionConfig {
                target_addr: SocketAddr::new(cfg.peer_ip, cfg.downlink_udp_port),
                source: if cfg.synthetic_source_override {
                    CaptureSource::SyntheticTone
                } else {
                    CaptureSource::SystemLoopback
                },
                device_name: dev,
                codec: cfg.profile.default_codec(),
                channels: cfg.media_channels,
                frame_duration: cfg.profile.default_frame_duration(),
                opus_bitrate_bps: cfg.media_opus_bitrate_bps_override.unwrap_or(128_000),
                enable_fec: cfg.profile.default_fec(),
                noise_gate_db: None,
            };
            Some(SenderSession::start(tx_cfg)?)
        } else {
            None
        };

        // 4. Mic TX (`Microphone` -> `peer_ip:49155`)
        let mic_tx = if cfg.mic_mode.sends() {
            let dev = cfg.mic_tx_device.clone().or_else(|| {
                if cfg.role != DuplexRole::Host {
                    cfg.tx_device_name.clone()
                } else {
                    None
                }
            });
            let tx_cfg = SenderSessionConfig {
                target_addr: SocketAddr::new(cfg.peer_ip, cfg.uplink_udp_port),
                source: if cfg.synthetic_source_override {
                    CaptureSource::SyntheticTone
                } else {
                    CaptureSource::Microphone
                },
                device_name: dev,
                codec: cfg.profile.default_codec(),
                channels: cfg.mic_channels,
                frame_duration: cfg.profile.default_frame_duration(),
                opus_bitrate_bps: 64_000,
                enable_fec: cfg.profile.default_fec(),
                noise_gate_db: cfg.mic_noise_gate_db,
            };
            Some(SenderSession::start(tx_cfg)?)
        } else {
            None
        };

        Ok(Self {
            role: cfg.role,
            media_mode: cfg.media_mode,
            mic_mode: cfg.mic_mode,
            media_tx,
            media_rx,
            mic_tx,
            mic_rx,
            virtual_mic_ready,
        })
    }

    pub const fn role(&self) -> DuplexRole {
        self.role
    }

    pub const fn media_mode(&self) -> MediaBusMode {
        self.media_mode
    }

    pub const fn mic_mode(&self) -> MicBusMode {
        self.mic_mode
    }

    /// Mute or unmute the outgoing Microphone TX stream (`mic_tx`), or `media_tx` if only media TX is active.
    pub fn set_tx_muted(&self, muted: bool) {
        if let Some(ref mic_tx) = self.mic_tx {
            mic_tx.set_muted(muted);
        } else if let Some(ref media_tx) = self.media_tx {
            media_tx.set_muted(muted);
        }
    }

    pub fn set_media_tx_muted(&self, muted: bool) {
        if let Some(ref media_tx) = self.media_tx {
            media_tx.set_muted(muted);
        }
    }

    pub fn is_tx_muted(&self) -> bool {
        if let Some(ref mic_tx) = self.mic_tx {
            mic_tx.is_muted()
        } else if let Some(ref media_tx) = self.media_tx {
            media_tx.is_muted()
        } else {
            false
        }
    }

    /// Adjust incoming Speaker playback volume (`0.0..=2.0`).
    pub fn set_rx_volume(&self, gain: f32) {
        if let Some(ref media_rx) = self.media_rx {
            media_rx.set_volume(gain);
        }
    }

    /// Adjust incoming Virtual Mic injection gain (`0.0..=2.0`).
    pub fn set_mic_rx_gain(&self, gain: f32) {
        if let Some(ref mic_rx) = self.mic_rx {
            mic_rx.set_volume(gain);
        }
    }

    /// Dynamically adjust outgoing microphone noise gate threshold in dBFS.
    pub fn set_tx_noise_gate_db(&self, threshold_db: Option<f32>) {
        if let Some(ref mic_tx) = self.mic_tx {
            mic_tx.set_noise_gate_db(threshold_db);
        }
    }

    pub fn status(&self) -> DuplexSessionStatus {
        DuplexSessionStatus {
            role: self.role,
            media_mode: self.media_mode,
            mic_mode: self.mic_mode,
            media_tx: self.media_tx.as_ref().map(|s| s.status()),
            media_rx: self.media_rx.as_ref().map(|s| s.status()),
            mic_tx: self.mic_tx.as_ref().map(|s| s.status()),
            mic_rx: self.mic_rx.as_ref().map(|s| s.status()),
            virtual_mic_ready: self.virtual_mic_ready,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn test_echo_free_guard_prevents_speaker_fallback_when_no_vmic_selected() {
        let lo: IpAddr = "127.0.0.1".parse().unwrap();
        let mut cfg = DuplexSessionConfig::new(DuplexRole::Host, lo);
        cfg.media_mode = MediaBusMode::Off;
        cfg.mic_mode = MicBusMode::ReceiveVirtualMic;
        cfg.uplink_udp_port = 49455;
        cfg.mic_rx_device = Some(NULL_VIRTUAL_MIC_DEVICE_NAME.to_string());
        cfg.advertise_mdns = false;

        let session = DuplexSession::start(cfg).expect("start echo-free mic_rx session");
        let st = session.status();
        assert!(!st.virtual_mic_ready);
        assert_eq!(
            st.mic_rx.as_ref().unwrap().device_name,
            NULL_VIRTUAL_MIC_DEVICE_NAME
        );
    }

    #[test]
    fn test_cross_machine_symmetric_mic_and_media_bus_isolation() {
        let lo: IpAddr = "127.0.0.1".parse().unwrap();

        // Simulate Machine A (e.g. Windows) sending BOTH System Audio (:49463) AND Physical Mic (:49465)
        let mut node_a_cfg = DuplexSessionConfig::new(DuplexRole::Symmetric, lo);
        node_a_cfg.media_mode = MediaBusMode::SendLoopback;
        node_a_cfg.mic_mode = MicBusMode::SendMic;
        node_a_cfg.downlink_udp_port = 49463;
        node_a_cfg.uplink_udp_port = 49465;
        node_a_cfg.advertise_mdns = false;
        node_a_cfg.synthetic_source_override = true;
        node_a_cfg.mic_noise_gate_db = None;

        // Simulate Machine B (e.g. macOS) receiving BOTH Speaker (:49463) AND Virtual Mic (:49465)
        let mut node_b_cfg = DuplexSessionConfig::new(DuplexRole::Symmetric, lo);
        node_b_cfg.media_mode = MediaBusMode::ReceiveSpeaker;
        node_b_cfg.mic_mode = MicBusMode::ReceiveVirtualMic;
        node_b_cfg.downlink_udp_port = 49463;
        node_b_cfg.uplink_udp_port = 49465;
        node_b_cfg.media_rx_device = Some(NULL_VIRTUAL_MIC_DEVICE_NAME.to_string());
        node_b_cfg.mic_rx_device = Some(NULL_VIRTUAL_MIC_DEVICE_NAME.to_string());
        node_b_cfg.advertise_mdns = false;

        let node_b = DuplexSession::start(node_b_cfg).expect("start Node B");
        let node_a = DuplexSession::start(node_a_cfg).expect("start Node A");

        std::thread::sleep(Duration::from_millis(160));

        let st_a = node_a.status();
        let st_b = node_b.status();

        assert!(st_a.media_tx.as_ref().unwrap().net.packets_sent > 10);
        assert!(st_a.mic_tx.as_ref().unwrap().net.packets_sent > 10);
        assert!(st_b.media_rx.as_ref().unwrap().net.packets_received > 10);
        assert!(st_b.mic_rx.as_ref().unwrap().net.packets_received > 10);
    }
}
