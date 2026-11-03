use std::net::SocketAddr;

use rsrelay_audio::CaptureSource;
use rsrelay_proto::{ChannelLayout, CodecType, FrameDuration};
use serde::{Deserialize, Serialize};

/// Pre-tuned latency and quality profiles.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum LatencyProfile {
    /// 5ms Opus frames, 10ms jitter buffer (~15-20ms E2E). Best for 5GHz Wi-Fi or Ethernet.
    UltraLow,
    /// 5ms Opus frames + FEC, 20ms jitter buffer (~25-30ms E2E). Recommended default.
    Balanced,
    /// 10ms Opus frames + FEC, 40ms jitter buffer (~50ms E2E). Best for congested/multi-wall Wi-Fi.
    Stable,
    /// 5ms Uncompressed 16-bit 48kHz Stereo PCM (`960 B` payload < 1460 MTU), 15ms buffer.
    LosslessPcm,
}

impl LatencyProfile {
    pub const fn default_codec(self) -> CodecType {
        match self {
            Self::UltraLow | Self::Balanced | Self::Stable => CodecType::Opus,
            Self::LosslessPcm => CodecType::PcmI16Le,
        }
    }

    pub const fn default_frame_duration(self) -> FrameDuration {
        match self {
            Self::UltraLow | Self::Balanced | Self::LosslessPcm => FrameDuration::Ms5,
            Self::Stable => FrameDuration::Ms10,
        }
    }

    pub const fn default_target_buffer_ms(self) -> u16 {
        match self {
            Self::UltraLow => 10,
            Self::LosslessPcm => 15,
            Self::Balanced => 20,
            Self::Stable => 40,
        }
    }

    pub const fn default_fec(self) -> bool {
        match self {
            Self::UltraLow | Self::LosslessPcm => false,
            Self::Balanced | Self::Stable => true,
        }
    }
}

#[derive(Debug, Clone)]
pub struct SenderSessionConfig {
    pub target_addr: SocketAddr,
    pub source: CaptureSource,
    pub device_name: Option<String>,
    pub codec: CodecType,
    pub channels: ChannelLayout,
    pub frame_duration: FrameDuration,
    pub opus_bitrate_bps: u32,
    pub enable_fec: bool,
}

#[derive(Debug, Clone)]
pub struct ReceiverSessionConfig {
    pub bind_addr: SocketAddr,
    pub device_name: Option<String>,
    pub channels: ChannelLayout,
    pub target_buffer_ms: u16,
    pub initial_volume: f32,
    pub advertise_mdns: bool,
    pub node_name: String,
}
