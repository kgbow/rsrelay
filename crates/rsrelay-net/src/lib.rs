//! `rsrelay-net`: Real-time UDP audio transport, DSCP QoS socket prioritization,
//! and zero-configuration mDNS + UDP LAN service discovery.

pub mod control_session;
pub mod discovery;
pub mod receiver;
pub mod sender;

pub use control_session::{ControlClient, ControlServer, ControlTelemetrySnapshot};
pub use discovery::{
    detect_all_local_ipv4s, detect_local_lan_ip, DiscoveredPeer, DiscoveryError, DiscoveryScanner,
    DiscoveryService,
};
pub use receiver::{AudioReceiverWorker, ReceiverConfig, ReceiverMetrics, ReceiverSnapshot};
pub use sender::{AudioSenderWorker, SenderConfig, SenderMetrics, SenderSnapshot};
