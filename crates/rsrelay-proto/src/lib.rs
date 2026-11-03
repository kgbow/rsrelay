//! `rsrelay-proto`: Zero-overhead wire protocol definitions for `rsrelay`.
//!
//! Includes:
//! - 12-byte custom RTP-lite UDP audio packet header (`PacketHeader`)
//! - RFC 1982 sequence number arithmetic (`seq_diff`)
//! - TCP control plane & mDNS discovery message schemas (`ControlMessage`)

pub mod control;
pub mod header;

pub use control::{
    ControlMessage, NodeCapabilities, DEFAULT_AUDIO_UDP_PORT, DEFAULT_CONTROL_PORT,
    MDNS_SERVICE_TYPE,
};
pub use header::{
    seq_diff, ChannelLayout, CodecType, FormatFlags, FrameDuration, PacketHeader, ProtoError,
    HEADER_LEN, MAGIC, MAX_PACKET_SIZE, MAX_UDP_PAYLOAD, PROTOCOL_VERSION, STANDARD_SAMPLE_RATE,
};
