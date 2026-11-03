//! `rsrelay-net`: Real-time UDP audio transport, DSCP QoS socket prioritization,
//! and zero-configuration mDNS service discovery.

pub mod discovery;
pub mod receiver;
pub mod sender;

pub use discovery::{DiscoveredPeer, DiscoveryError, DiscoveryService};
pub use receiver::{AudioReceiverWorker, ReceiverConfig, ReceiverMetrics, ReceiverSnapshot};
pub use sender::{AudioSenderWorker, SenderConfig, SenderMetrics, SenderSnapshot};
