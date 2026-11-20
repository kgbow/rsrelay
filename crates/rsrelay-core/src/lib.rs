//! `rsrelay-core`: Unified real-time audio relay engine orchestrating capture, DSP, and network transport.

pub mod config;
pub mod duplex;
pub mod engine;

pub use config::{LatencyProfile, ReceiverSessionConfig, SenderSessionConfig};
pub use duplex::{
    preset_bus_modes_for_role, DuplexSession, DuplexSessionConfig, DuplexSessionStatus,
    MediaBusMode, MicBusMode,
};
pub use engine::{ReceiverSession, ReceiverSessionStatus, SenderSession, SenderSessionStatus};
pub use rsrelay_audio::{
    has_system_loopback_permission, install_macos_virtual_mic_driver,
    is_audiorelay_backend_running, is_macos_rsrelay_driver_installed, is_null_virtual_mic_filter,
    is_virtual_mic_driver_available, is_virtual_mic_input_device_name,
    is_virtual_mic_output_sink_name, launch_windows_vbcable_install, list_audio_devices,
    request_system_loopback_permission, select_default_virtual_mic_output_device,
    select_virtual_mic_output_from_devices, AudioDeviceInfo, CaptureSource,
    MACOS_RSRELAY_VMIC_INPUT_NAME, MACOS_RSRELAY_VMIC_SINK_NAME, NULL_VIRTUAL_MIC_DEVICE_NAME,
    VIRTUAL_MIC_OUTPUT_DEVICE_NAME,
};
pub use rsrelay_net::{
    detect_all_local_ipv4s, detect_local_lan_ip, DiscoveredPeer, DiscoveryScanner,
    DiscoveryService,
};
pub use rsrelay_proto::{
    ChannelLayout, CodecType, DuplexRole, FrameDuration, DEFAULT_AUDIO_UDP_PORT,
    DEFAULT_CONTROL_PORT, DEFAULT_DISCOVERY_UDP_PORT, DEFAULT_UPLINK_CONTROL_PORT,
    DEFAULT_UPLINK_UDP_PORT,
};
