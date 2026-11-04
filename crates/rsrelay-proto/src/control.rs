use serde::{Deserialize, Serialize};
use crate::header::{ChannelLayout, CodecType, FrameDuration};

/// mDNS-SD service type used for zero-configuration LAN discovery.
pub const MDNS_SERVICE_TYPE: &str = "_rsrelay._udp.local.";

/// Default TCP control port (downlink / primary stream).
pub const DEFAULT_CONTROL_PORT: u16 = 49152;

/// Default UDP audio stream port (downlink / primary stream).
pub const DEFAULT_AUDIO_UDP_PORT: u16 = 49153;

/// Default TCP control port for full-duplex return microphone uplink.
pub const DEFAULT_UPLINK_CONTROL_PORT: u16 = 49154;

/// Default UDP audio port for full-duplex return microphone uplink.
pub const DEFAULT_UPLINK_UDP_PORT: u16 = 49155;

/// Compute the companion TCP control port for a given UDP audio stream port.
#[inline]
pub const fn companion_control_port(audio_udp_port: u16) -> u16 {
    match audio_udp_port {
        DEFAULT_AUDIO_UDP_PORT => DEFAULT_CONTROL_PORT,
        DEFAULT_UPLINK_UDP_PORT => DEFAULT_UPLINK_CONTROL_PORT,
        p if p > 1024 => p - 1,
        p => p.saturating_add(1),
    }
}

/// Role in a full-duplex bidirectional session.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum DuplexRole {
    /// Main desktop/PC: sends System Loopback audio (downlink) and receives remote Microphone (uplink).
    Host,
    /// Secondary device acting as wireless headset: receives System Audio (downlink) and sends local Microphone (uplink).
    Headset,
}

/// Node capabilities advertised via mDNS TXT records and TCP handshake.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NodeCapabilities {
    pub can_send_loopback: bool,
    pub can_send_mic: bool,
    pub can_receive_speaker: bool,
}

impl Default for NodeCapabilities {
    fn default() -> Self {
        Self {
            can_send_loopback: true,
            can_send_mic: true,
            can_receive_speaker: true,
        }
    }
}

/// Control plane messages exchanged over TCP (framed with a 4-byte Big-Endian length prefix).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ControlMessage {
    /// Initial handshake greeting.
    Hello {
        protocol_version: u8,
        node_id: String,
        hostname: String,
        os: String,
        audio_udp_port: u16,
        capabilities: NodeCapabilities,
    },
    /// Negotiate or update an active audio stream.
    StartStream {
        codec: CodecType,
        channels: ChannelLayout,
        frame_duration: FrameDuration,
        opus_bitrate_bps: u32,
        receiver_udp_port: u16,
        target_buffer_ms: u16,
    },
    /// Gracefully stop the active stream.
    StopStream {
        reason: String,
    },
    /// NTP-style clock sync ping initiated by sender or receiver.
    Ping {
        seq: u32,
        client_send_us: u64,
    },
    /// NTP-style clock sync pong response.
    Pong {
        seq: u32,
        client_send_us: u64,
        server_recv_us: u64,
        server_send_us: u64,
    },
    /// Receiver real-time quality & clock-drift telemetry reported back to sender.
    ReceiverTelemetry {
        rtt_us: u64,
        jitter_us: u64,
        packets_received: u64,
        packets_lost: u64,
        plc_frames: u64,
        fec_recovered_frames: u64,
        buffer_level_ms: f32,
        resample_ratio: f64,
    },
}
