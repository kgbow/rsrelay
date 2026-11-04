//! `rsrelay-core`: Unified real-time audio relay engine orchestrating capture, DSP, and network transport.

pub mod config;
pub mod duplex;
pub mod engine;

pub use config::{LatencyProfile, ReceiverSessionConfig, SenderSessionConfig};
pub use duplex::{DuplexSession, DuplexSessionConfig, DuplexSessionStatus};
pub use engine::{ReceiverSession, ReceiverSessionStatus, SenderSession, SenderSessionStatus};
pub use rsrelay_audio::{list_audio_devices, AudioDeviceInfo, CaptureSource};
pub use rsrelay_net::{DiscoveredPeer, DiscoveryService};
pub use rsrelay_proto::{
    ChannelLayout, CodecType, DuplexRole, FrameDuration, DEFAULT_AUDIO_UDP_PORT,
    DEFAULT_CONTROL_PORT, DEFAULT_UPLINK_CONTROL_PORT, DEFAULT_UPLINK_UDP_PORT,
};
