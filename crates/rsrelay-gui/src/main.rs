use std::net::SocketAddr;

use rsrelay_core::{
    list_audio_devices, AudioDeviceInfo, CaptureSource, ChannelLayout, DiscoveredPeer,
    LatencyProfile, ReceiverSession, ReceiverSessionConfig, SenderSession, SenderSessionConfig,
    DEFAULT_AUDIO_UDP_PORT,
};

/// Pure-Rust reactive view-model for the desktop GUI and system tray controller.
///
/// Decouples state transitions from the rendering layer (`iced` / `Slint` + `tray-icon`),
/// allowing the window surface to be completely dropped when minimized to the system tray
/// (< 8 MB background memory footprint).
pub struct AppViewModel {
    pub devices: Vec<AudioDeviceInfo>,
    pub discovered_peers: Vec<DiscoveredPeer>,
    pub selected_profile: LatencyProfile,
    pub selected_source: CaptureSource,
    pub target_addr: SocketAddr,
    pub bind_addr: SocketAddr,
    pub volume: f32,
    pub active_sender: Option<SenderSession>,
    pub active_receiver: Option<ReceiverSession>,
}

impl AppViewModel {
    pub fn new() -> anyhow::Result<Self> {
        let devices = list_audio_devices().unwrap_or_default();
        Ok(Self {
            devices,
            discovered_peers: Vec::new(),
            selected_profile: LatencyProfile::Balanced,
            selected_source: CaptureSource::SystemLoopback,
            target_addr: SocketAddr::from(([127, 0, 0, 1], DEFAULT_AUDIO_UDP_PORT)),
            bind_addr: SocketAddr::from(([0, 0, 0, 0], DEFAULT_AUDIO_UDP_PORT)),
            volume: 1.0,
            active_sender: None,
            active_receiver: None,
        })
    }

    pub fn toggle_sender(&mut self) -> anyhow::Result<()> {
        if self.active_sender.is_some() {
            self.active_sender = None;
            return Ok(());
        }
        let cfg = SenderSessionConfig {
            target_addr: self.target_addr,
            source: self.selected_source,
            device_name: None,
            codec: self.selected_profile.default_codec(),
            channels: ChannelLayout::Stereo,
            frame_duration: self.selected_profile.default_frame_duration(),
            opus_bitrate_bps: 128_000,
            enable_fec: self.selected_profile.default_fec(),
        };
        self.active_sender = Some(SenderSession::start(cfg)?);
        Ok(())
    }

    pub fn toggle_receiver(&mut self) -> anyhow::Result<()> {
        if self.active_receiver.is_some() {
            self.active_receiver = None;
            return Ok(());
        }
        let cfg = ReceiverSessionConfig {
            bind_addr: self.bind_addr,
            device_name: None,
            channels: ChannelLayout::Stereo,
            target_buffer_ms: self.selected_profile.default_target_buffer_ms(),
            initial_volume: self.volume,
            advertise_mdns: true,
            node_name: "rsrelay-desktop".to_string(),
        };
        self.active_receiver = Some(ReceiverSession::start(cfg)?);
        Ok(())
    }
}

fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt::init();
    let vm = AppViewModel::new()?;
    println!(
        "rsrelay-gui view-model initialized ({} local audio devices detected).",
        vm.devices.len()
    );
    println!("Use `cargo run -p rsrelay-cli -- --help` for full CLI operations (send, recv, discover, self-test).");
    Ok(())
}
