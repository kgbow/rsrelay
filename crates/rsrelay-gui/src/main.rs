use std::net::{IpAddr, SocketAddr};
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc, Mutex,
};
use std::thread;
use std::time::Duration;

use eframe::egui;
use rsrelay_core::{
    list_audio_devices, AudioDeviceInfo, CaptureSource, ChannelLayout, DiscoveredPeer,
    DiscoveryService, DuplexRole, DuplexSession, DuplexSessionConfig, LatencyProfile,
    ReceiverSession, ReceiverSessionConfig, SenderSession, SenderSessionConfig,
    DEFAULT_AUDIO_UDP_PORT, DEFAULT_UPLINK_UDP_PORT,
};
use tray_icon::{
    menu::{Menu, MenuEvent, MenuItem},
    Icon, TrayIcon, TrayIconBuilder,
};

/// Generate a crisp 32x32 RGBA waveform icon in memory (zero external asset files needed).
pub fn build_app_icon_rgba() -> (Vec<u8>, u32, u32) {
    let width = 32u32;
    let height = 32u32;
    let mut rgba = vec![0u8; (width * height * 4) as usize];

    let bar_heights = [6i32, 12, 20, 26, 18, 24, 14, 8];
    for y in 0..height as i32 {
        for x in 0..width as i32 {
            let idx = ((y as u32 * width + x as u32) * 4) as usize;
            let dx = x - 16;
            let dy = y - 16;
            let dist_sq = dx * dx + dy * dy;

            if dist_sq <= 15 * 15 {
                // Dark indigo circular badge background
                rgba[idx] = 28;
                rgba[idx + 1] = 33;
                rgba[idx + 2] = 53;
                rgba[idx + 3] = 255;

                // Equalizer bars inside badge
                if (4..28).contains(&x) && (x - 4) % 3 != 2 {
                    let bar_idx = ((x - 4) / 3) as usize;
                    if bar_idx < bar_heights.len() {
                        let half_h = bar_heights[bar_idx] / 2;
                        if dy.abs() <= half_h {
                            rgba[idx] = 0;
                            rgba[idx + 1] = 212;
                            rgba[idx + 2] = 170;
                            rgba[idx + 3] = 255;
                        }
                    }
                }
            }
        }
    }
    (rgba, width, height)
}

struct SystemTrayHandles {
    _tray: TrayIcon,
    stop_all_id: tray_icon::menu::MenuId,
    quit_id: tray_icon::menu::MenuId,
}

fn init_system_tray() -> anyhow::Result<SystemTrayHandles> {
    let (rgba, w, h) = build_app_icon_rgba();
    let icon = Icon::from_rgba(rgba, w, h)?;

    let menu = Menu::new();
    let title_item = MenuItem::new("rsrelay — Ultra-Low-Latency Audio Relay", false, None);
    let stop_all_item = MenuItem::new("Stop All Audio Streams", true, None);
    let quit_item = MenuItem::new("Quit rsrelay", true, None);

    let stop_all_id = stop_all_item.id().clone();
    let quit_id = quit_item.id().clone();

    menu.append(&title_item)?;
    menu.append(&stop_all_item)?;
    menu.append(&quit_item)?;

    let tray = TrayIconBuilder::new()
        .with_menu(Box::new(menu))
        .with_tooltip("rsrelay — Audio Relay")
        .with_icon(icon)
        .build()?;

    Ok(SystemTrayHandles {
        _tray: tray,
        stop_all_id,
        quit_id,
    })
}

pub struct RsRelayGuiApp {
    pub devices: Vec<AudioDeviceInfo>,
    pub discovered_peers: Arc<Mutex<Vec<DiscoveredPeer>>>,
    pub scanning_lan: Arc<AtomicBool>,

    // Sender controls
    pub target_addr_input: String,
    pub selected_source: CaptureSource,
    pub selected_tx_device: Option<String>,
    pub selected_profile: LatencyProfile,
    pub opus_bitrate_kbps: u32,
    pub active_sender: Option<SenderSession>,

    // Receiver controls
    pub bind_addr_input: String,
    pub selected_rx_device: Option<String>,
    pub target_buffer_ms: u16,
    pub volume: f32,
    pub advertise_mdns: bool,
    pub active_receiver: Option<ReceiverSession>,

    // Full-Duplex Wireless Headset controls
    pub duplex_role: DuplexRole,
    pub duplex_peer_ip_input: String,
    pub duplex_downlink_port: u16,
    pub duplex_uplink_port: u16,
    pub duplex_gate_db: f32,
    pub duplex_mic_muted: bool,
    pub active_duplex: Option<DuplexSession>,

    // Status message & tray
    pub last_status_msg: String,
    tray: Option<SystemTrayHandles>,
}

impl RsRelayGuiApp {
    pub fn new() -> anyhow::Result<Self> {
        let devices = list_audio_devices().unwrap_or_default();
        let tray = init_system_tray().ok();

        Ok(Self {
            devices,
            discovered_peers: Arc::new(Mutex::new(Vec::new())),
            scanning_lan: Arc::new(AtomicBool::new(false)),
            target_addr_input: format!("127.0.0.1:{DEFAULT_AUDIO_UDP_PORT}"),
            selected_source: CaptureSource::SystemLoopback,
            selected_tx_device: None,
            selected_profile: LatencyProfile::Balanced,
            opus_bitrate_kbps: 128,
            active_sender: None,
            bind_addr_input: format!("0.0.0.0:{DEFAULT_AUDIO_UDP_PORT}"),
            selected_rx_device: None,
            target_buffer_ms: 20,
            volume: 1.0,
            advertise_mdns: true,
            active_receiver: None,
            duplex_role: DuplexRole::Headset,
            duplex_peer_ip_input: "127.0.0.1".to_string(),
            duplex_downlink_port: DEFAULT_AUDIO_UDP_PORT,
            duplex_uplink_port: DEFAULT_UPLINK_UDP_PORT,
            duplex_gate_db: -45.0,
            duplex_mic_muted: false,
            active_duplex: None,
            last_status_msg: "Ready. Select a mode below to start streaming.".to_string(),
            tray,
        })
    }

    pub fn trigger_lan_scan(&mut self) {
        if self.scanning_lan.load(Ordering::Relaxed) {
            return;
        }
        self.scanning_lan.store(true, Ordering::Relaxed);
        let peers_arc = Arc::clone(&self.discovered_peers);
        let scanning_flag = Arc::clone(&self.scanning_lan);

        thread::spawn(move || {
            if let Ok(svc) = DiscoveryService::new() {
                if let Ok(found) = svc.scan(Duration::from_millis(1500)) {
                    if let Ok(mut guard) = peers_arc.lock() {
                        *guard = found;
                    }
                }
            }
            scanning_flag.store(false, Ordering::Relaxed);
        });
    }

    pub fn toggle_sender(&mut self) {
        if self.active_sender.is_some() {
            self.active_sender = None;
            self.last_status_msg = "Sender stopped.".to_string();
            return;
        }

        let target_addr: SocketAddr = match self.target_addr_input.trim().parse() {
            Ok(addr) => addr,
            Err(e) => {
                self.last_status_msg = format!("Invalid Target IP:Port: {e}");
                return;
            }
        };

        let cfg = SenderSessionConfig {
            target_addr,
            source: self.selected_source,
            device_name: self.selected_tx_device.clone(),
            codec: self.selected_profile.default_codec(),
            channels: ChannelLayout::Stereo,
            frame_duration: self.selected_profile.default_frame_duration(),
            opus_bitrate_bps: self.opus_bitrate_kbps * 1000,
            enable_fec: self.selected_profile.default_fec(),
            noise_gate_db: None,
        };

        match SenderSession::start(cfg) {
            Ok(session) => {
                let st = session.status();
                self.last_status_msg = format!(
                    "Streaming '{}' ({} Hz) -> {}",
                    st.device_name, st.sample_rate, target_addr
                );
                self.active_sender = Some(session);
            }
            Err(e) => {
                self.last_status_msg = format!("Failed to start sender: {e}");
            }
        }
    }

    pub fn toggle_receiver(&mut self) {
        if self.active_receiver.is_some() {
            self.active_receiver = None;
            self.last_status_msg = "Receiver stopped.".to_string();
            return;
        }

        let bind_addr: SocketAddr = match self.bind_addr_input.trim().parse() {
            Ok(addr) => addr,
            Err(e) => {
                self.last_status_msg = format!("Invalid Bind IP:Port: {e}");
                return;
            }
        };

        let cfg = ReceiverSessionConfig {
            bind_addr,
            device_name: self.selected_rx_device.clone(),
            channels: ChannelLayout::Stereo,
            target_buffer_ms: self.target_buffer_ms,
            initial_volume: self.volume,
            advertise_mdns: self.advertise_mdns,
            node_name: "rsrelay-desktop".to_string(),
        };

        match ReceiverSession::start(cfg) {
            Ok(session) => {
                let st = session.status();
                self.last_status_msg = format!(
                    "Listening on {} -> Output '{}' ({} Hz)",
                    bind_addr, st.device_name, st.sample_rate
                );
                self.active_receiver = Some(session);
            }
            Err(e) => {
                self.last_status_msg = format!("Failed to start receiver: {e}");
            }
        }
    }

    pub fn toggle_duplex(&mut self) {
        if self.active_duplex.is_some() {
            self.active_duplex = None;
            self.last_status_msg = "Full-Duplex session stopped.".to_string();
            return;
        }

        // Stop standalone single-direction sessions before binding duplex ports
        self.active_sender = None;
        self.active_receiver = None;

        let peer_ip: IpAddr = match self.duplex_peer_ip_input.trim().parse() {
            Ok(ip) => ip,
            Err(e) => {
                self.last_status_msg = format!("Invalid Peer IP address: {e}");
                return;
            }
        };

        let mut cfg = DuplexSessionConfig::new(self.duplex_role, peer_ip);
        cfg.downlink_udp_port = self.duplex_downlink_port;
        cfg.uplink_udp_port = self.duplex_uplink_port;
        cfg.tx_device_name = self.selected_tx_device.clone();
        cfg.rx_device_name = self.selected_rx_device.clone();
        cfg.profile = self.selected_profile;
        cfg.mic_noise_gate_db = Some(self.duplex_gate_db);
        cfg.initial_volume = self.volume;

        match DuplexSession::start(cfg) {
            Ok(session) => {
                session.set_tx_muted(self.duplex_mic_muted);
                let st = session.status();
                self.last_status_msg = format!(
                    "Full-Duplex [{:?}] active <-> {} (TX: '{}', RX: '{}')",
                    self.duplex_role, peer_ip, st.tx.device_name, st.rx.device_name
                );
                self.active_duplex = Some(session);
            }
            Err(e) => {
                self.last_status_msg = format!("Failed to start Full-Duplex session: {e}");
            }
        }
    }
}

impl eframe::App for RsRelayGuiApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        // Poll system tray menu events
        if let Some(ref tray) = self.tray {
            while let Ok(event) = MenuEvent::receiver().try_recv() {
                if event.id == tray.stop_all_id {
                    self.active_sender = None;
                    self.active_receiver = None;
                    self.active_duplex = None;
                    self.last_status_msg = "All streams stopped from System Tray.".to_string();
                } else if event.id == tray.quit_id {
                    self.active_sender = None;
                    self.active_receiver = None;
                    self.active_duplex = None;
                    ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                }
            }
        }

        // Request continuous repaint at ~30 Hz when a stream is active so VU meters and telemetry animate smoothly
        if self.active_sender.is_some()
            || self.active_receiver.is_some()
            || self.active_duplex.is_some()
            || self.scanning_lan.load(Ordering::Relaxed)
        {
            ctx.request_repaint_after(Duration::from_millis(33));
        }

        egui::CentralPanel::default().show(ctx, |ui| {
            ui.horizontal(|ui| {
                ui.heading("rsrelay — Ultra-Low-Latency Audio Relay");
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.button("Refresh Devices").clicked() {
                        self.devices = list_audio_devices().unwrap_or_default();
                    }
                    let scanning = self.scanning_lan.load(Ordering::Relaxed);
                    if ui
                        .add_enabled(!scanning, egui::Button::new(if scanning { "Scanning LAN..." } else { "Scan LAN (mDNS)" }))
                        .clicked()
                    {
                        self.trigger_lan_scan();
                    }
                });
            });

            ui.label(egui::RichText::new(&self.last_status_msg).small().weak());
            ui.separator();

            // Discovered LAN Peers section
            if let Ok(peers) = self.discovered_peers.lock() {
                if !peers.is_empty() {
                    ui.group(|ui| {
                        ui.label(egui::RichText::new("Discovered LAN Receivers (Click to select target):").strong());
                        ui.horizontal_wrapped(|ui| {
                            for peer in peers.iter() {
                                if let Some(ip) = peer.addresses.first() {
                                    let addr_str = format!("{ip}:{}", peer.udp_port);
                                    let btn_label = format!("{} ({}) [{}]", peer.hostname, peer.os, addr_str);
                                    if ui.button(btn_label).clicked() {
                                        self.target_addr_input = addr_str;
                                        self.duplex_peer_ip_input = ip.to_string();
                                    }
                                }
                            }
                        });
                    });
                    ui.add_space(6.0);
                }
            }

            ui.columns(2, |columns| {
                // === LEFT COLUMN: SENDER ===
                columns[0].group(|ui| {
                    ui.heading("1. Sender (Transmit Audio)");
                    ui.add_space(4.0);

                    ui.horizontal(|ui| {
                        ui.label("Target IP:Port:");
                        ui.text_edit_singleline(&mut self.target_addr_input);
                    });

                    ui.horizontal(|ui| {
                        ui.label("Audio Source:");
                        egui::ComboBox::from_id_salt("source_combo")
                            .selected_text(match self.selected_source {
                                CaptureSource::SystemLoopback => "System Loopback (SCK / WASAPI)",
                                CaptureSource::Microphone => "Microphone Input",
                                CaptureSource::SyntheticTone => "Synthetic 440Hz/660Hz Tone",
                            })
                            .show_ui(ui, |ui| {
                                ui.selectable_value(
                                    &mut self.selected_source,
                                    CaptureSource::SystemLoopback,
                                    "System Loopback (SCK / WASAPI)",
                                );
                                ui.selectable_value(
                                    &mut self.selected_source,
                                    CaptureSource::Microphone,
                                    "Microphone Input",
                                );
                                ui.selectable_value(
                                    &mut self.selected_source,
                                    CaptureSource::SyntheticTone,
                                    "Synthetic 440Hz/660Hz Tone",
                                );
                            });
                    });

                    ui.horizontal(|ui| {
                        ui.label("Input Device:");
                        let current_label = self
                            .selected_tx_device
                            .as_deref()
                            .unwrap_or("System Default (Auto)");
                        let prev_dev = self.selected_tx_device.clone();
                        egui::ComboBox::from_id_salt("tx_device_combo")
                            .selected_text(current_label)
                            .show_ui(ui, |ui| {
                                ui.selectable_value(
                                    &mut self.selected_tx_device,
                                    None,
                                    "System Default (Auto)",
                                );
                                for dev in &self.devices {
                                    // Show input devices (or output devices for Windows WASAPI Loopback)
                                    let eligible = dev.is_input
                                        || (cfg!(target_os = "windows")
                                            && self.selected_source
                                                == CaptureSource::SystemLoopback
                                            && dev.is_output);
                                    if eligible {
                                        let label = format!(
                                            "{} ({} Hz)",
                                            dev.name,
                                            dev.default_sample_rate.unwrap_or(48_000)
                                        );
                                        ui.selectable_value(
                                            &mut self.selected_tx_device,
                                            Some(dev.name.clone()),
                                            label,
                                        );
                                    }
                                }
                            });
                        if self.selected_tx_device != prev_dev && self.active_sender.is_some() {
                            // Live hot-swap input device when changed while running
                            self.active_sender = None;
                            self.toggle_sender();
                        }
                    });

                    ui.horizontal(|ui| {
                        ui.label("Latency Profile:");
                        egui::ComboBox::from_id_salt("profile_combo")
                            .selected_text(match self.selected_profile {
                                LatencyProfile::UltraLow => "Ultra-Low (5ms Opus, 10ms Buf)",
                                LatencyProfile::Balanced => "Balanced (5ms Opus + FEC, 20ms Buf)",
                                LatencyProfile::Stable => "Stable (10ms Opus + FEC, 40ms Buf)",
                                LatencyProfile::LosslessPcm => "Lossless (5ms 16-bit PCM, 15ms Buf)",
                            })
                            .show_ui(ui, |ui| {
                                ui.selectable_value(
                                    &mut self.selected_profile,
                                    LatencyProfile::UltraLow,
                                    "Ultra-Low (5ms Opus, 10ms Buf)",
                                );
                                ui.selectable_value(
                                    &mut self.selected_profile,
                                    LatencyProfile::Balanced,
                                    "Balanced (5ms Opus + FEC, 20ms Buf)",
                                );
                                ui.selectable_value(
                                    &mut self.selected_profile,
                                    LatencyProfile::Stable,
                                    "Stable (10ms Opus + FEC, 40ms Buf)",
                                );
                                ui.selectable_value(
                                    &mut self.selected_profile,
                                    LatencyProfile::LosslessPcm,
                                    "Lossless (5ms 16-bit PCM, 15ms Buf)",
                                );
                            });
                    });

                    ui.add_space(6.0);
                    let tx_running = self.active_sender.is_some();
                    let btn_text = if tx_running {
                        "Stop Sender"
                    } else {
                        "Start Sender"
                    };
                    if ui.button(egui::RichText::new(btn_text).strong()).clicked() {
                        self.toggle_sender();
                    }

                    if let Some(ref sender) = self.active_sender {
                        let st = sender.status();
                        ui.separator();
                        ui.label(format!("Source: {} ({} Hz)", st.device_name, st.sample_rate));
                        ui.add(
                            egui::ProgressBar::new(st.peak_level.clamp(0.0, 1.0))
                                .text(format!("Input Level: {:.1}%", st.peak_level * 100.0)),
                        );
                        ui.label(format!(
                            "Packets Sent: {} ({:.1} KB)",
                            st.net.packets_sent,
                            (st.net.bytes_sent as f64) / 1024.0
                        ));
                        ui.label(format!("Silence Heartbeats: {}", st.net.silence_heartbeats_sent));
                        if st.control.connected {
                            ui.label(format!(
                                "Control RTT: {:.2} ms | Remote Buf: {:.1} ms",
                                (st.control.rtt_us as f64) / 1000.0,
                                st.control.remote_buffer_ms
                            ));
                        }
                    }
                });

                // === RIGHT COLUMN: RECEIVER ===
                columns[1].group(|ui| {
                    ui.heading("2. Receiver (Listen & Play)");
                    ui.add_space(4.0);

                    ui.horizontal(|ui| {
                        ui.label("Bind IP:Port:");
                        ui.text_edit_singleline(&mut self.bind_addr_input);
                    });

                    ui.horizontal(|ui| {
                        ui.label("Output Device:");
                        let current_label = self
                            .selected_rx_device
                            .as_deref()
                            .unwrap_or("System Default (Auto)");
                        let prev_dev = self.selected_rx_device.clone();
                        egui::ComboBox::from_id_salt("rx_device_combo")
                            .selected_text(current_label)
                            .show_ui(ui, |ui| {
                                ui.selectable_value(
                                    &mut self.selected_rx_device,
                                    None,
                                    "System Default (Auto)",
                                );
                                for dev in &self.devices {
                                    if dev.is_output {
                                        let label = format!(
                                            "{} ({} Hz)",
                                            dev.name,
                                            dev.default_sample_rate.unwrap_or(48_000)
                                        );
                                        ui.selectable_value(
                                            &mut self.selected_rx_device,
                                            Some(dev.name.clone()),
                                            label,
                                        );
                                    }
                                }
                            });
                        if self.selected_rx_device != prev_dev && self.active_receiver.is_some() {
                            // Live hot-swap output playback device when changed while running
                            self.active_receiver = None;
                            self.toggle_receiver();
                        }
                    });

                    ui.horizontal(|ui| {
                        ui.label("Target Buffer:");
                        ui.add(egui::Slider::new(&mut self.target_buffer_ms, 5..=80).suffix(" ms"));
                    });

                    ui.horizontal(|ui| {
                        ui.label("Output Volume:");
                        if ui
                            .add(egui::Slider::new(&mut self.volume, 0.0..=2.0))
                            .changed()
                        {
                            if let Some(ref rx) = self.active_receiver {
                                rx.set_volume(self.volume);
                            }
                        }
                    });

                    ui.checkbox(&mut self.advertise_mdns, "Advertise on LAN via mDNS");

                    ui.add_space(6.0);
                    let rx_running = self.active_receiver.is_some();
                    let btn_text = if rx_running {
                        "Stop Receiver"
                    } else {
                        "Start Receiver"
                    };
                    if ui.button(egui::RichText::new(btn_text).strong()).clicked() {
                        self.toggle_receiver();
                    }

                    if let Some(ref receiver) = self.active_receiver {
                        let st = receiver.status();
                        ui.separator();
                        ui.label(format!("Output: {} ({} Hz)", st.device_name, st.sample_rate));
                        ui.add(
                            egui::ProgressBar::new(st.peak_level.clamp(0.0, 1.0))
                                .text(format!("Output Level: {:.1}%", st.peak_level * 100.0)),
                        );
                        ui.label(format!(
                            "Recv: {} pkts | Lost: {} | FEC: {} | PLC: {}",
                            st.net.packets_received,
                            st.net.packets_lost,
                            st.net.fec_recovered,
                            st.net.plc_frames
                        ));
                        ui.label(format!(
                            "Smoothed Buffer: {:.1} ms | Clock Drift ASRC: {:.5}x",
                            st.net.buffer_level_ms, st.net.resample_ratio
                        ));
                    }
                });
            });

            ui.add_space(6.0);
            // === FULL-DUPLEX WIRELESS HEADSET SECTION ===
            ui.group(|ui| {
                ui.heading("3. Full-Duplex Wireless Headset Mode (Simultaneous System Audio + Mic)");
                ui.horizontal_wrapped(|ui| {
                    ui.label("Role:");
                    egui::ComboBox::from_id_salt("duplex_role_combo")
                        .selected_text(match self.duplex_role {
                            DuplexRole::Host => "Host PC (Send System Audio :49153 + Recv Mic :49155)",
                            DuplexRole::Headset => "Wireless Headset (Recv System Audio :49153 + Send Mic :49155)",
                        })
                        .show_ui(ui, |ui| {
                            ui.selectable_value(
                                &mut self.duplex_role,
                                DuplexRole::Host,
                                "Host PC (Send System Audio :49153 + Recv Mic :49155)",
                            );
                            ui.selectable_value(
                                &mut self.duplex_role,
                                DuplexRole::Headset,
                                "Wireless Headset (Recv System Audio :49153 + Send Mic :49155)",
                            );
                        });

                    ui.label("Peer IP:");
                    ui.add(egui::TextEdit::singleline(&mut self.duplex_peer_ip_input).desired_width(110.0));

                    ui.label("Downlink Port:");
                    ui.add(egui::DragValue::new(&mut self.duplex_downlink_port).range(1024..=65535));

                    ui.label("Uplink Port:");
                    ui.add(egui::DragValue::new(&mut self.duplex_uplink_port).range(1024..=65535));

                    ui.label("Mic Gate:");
                    ui.add(egui::Slider::new(&mut self.duplex_gate_db, -65.0..=-20.0).suffix(" dB"));

                    if ui.checkbox(&mut self.duplex_mic_muted, "Mute Outgoing Mic").changed() {
                        if let Some(ref dpx) = self.active_duplex {
                            dpx.set_tx_muted(self.duplex_mic_muted);
                        }
                    }

                    let dpx_running = self.active_duplex.is_some();
                    let btn_label = if dpx_running {
                        "Stop Full-Duplex"
                    } else {
                        "Start Full-Duplex Headset"
                    };
                    if ui.button(egui::RichText::new(btn_label).strong()).clicked() {
                        self.toggle_duplex();
                    }
                });

                if let Some(ref duplex) = self.active_duplex {
                    let st = duplex.status();
                    ui.separator();
                    ui.columns(2, |cols| {
                        cols[0].add(
                            egui::ProgressBar::new(st.tx.peak_level.clamp(0.0, 1.0)).text(format!(
                                "TX ({}) Sent: {} pkts (Gated DTX: {})",
                                st.tx.device_name, st.tx.net.packets_sent, st.tx.net.gated_dtx_frames
                            )),
                        );
                        cols[1].add(
                            egui::ProgressBar::new(st.rx.peak_level.clamp(0.0, 1.0)).text(format!(
                                "RX ({}) Recv: {} pkts (Buf: {:.1}ms, ASRC: {:.5}x)",
                                st.rx.device_name,
                                st.rx.net.packets_received,
                                st.rx.net.buffer_level_ms,
                                st.rx.net.resample_ratio
                            )),
                        );
                    });
                }
            });

            ui.separator();
            ui.collapsing(
                format!("Detected Local Audio Hardware ({} devices)", self.devices.len()),
                |ui| {
                    for dev in &self.devices {
                        let dir = if dev.is_input { "IN " } else { "OUT" };
                        let def = if dev.is_default_input || dev.is_default_output {
                            " [DEFAULT]"
                        } else {
                            ""
                        };
                        ui.label(format!(
                            "[{}] {} ({:?} Hz, {:?} ch){}",
                            dir, dev.name, dev.default_sample_rate, dev.default_channels, def
                        ));
                    }
                },
            );
        });
    }
}

fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    // Support `--check` flag for headless CI / terminal verification
    if std::env::args().any(|a| a == "--check") {
        let (rgba, w, h) = build_app_icon_rgba();
        let devices = list_audio_devices().unwrap_or_default();
        println!(
            "rsrelay-gui headless check passed: icon={}x{} ({} bytes), devices={}",
            w,
            h,
            rgba.len(),
            devices.len()
        );
        return Ok(());
    }

    let (icon_rgba, icon_w, icon_h) = build_app_icon_rgba();
    let viewport = egui::ViewportBuilder::default()
        .with_inner_size([780.0, 460.0])
        .with_min_inner_size([640.0, 380.0])
        .with_icon(egui::IconData {
            rgba: icon_rgba,
            width: icon_w,
            height: icon_h,
        });

    let native_options = eframe::NativeOptions {
        viewport,
        ..Default::default()
    };

    let app = RsRelayGuiApp::new()?;
    eframe::run_native(
        "rsrelay — Audio Relay",
        native_options,
        Box::new(|_cc| Ok(Box::new(app))),
    )
    .map_err(|e| anyhow::anyhow!("eframe GUI error: {e}"))?;

    Ok(())
}
