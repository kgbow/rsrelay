#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

pub mod i18n;
pub mod macos_bundle;
pub mod theme;
pub mod widgets;

use std::net::{IpAddr, SocketAddr};
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc, Mutex,
};
use std::thread;
use std::time::Duration;

use eframe::egui::{self, Align, Color32, FontFamily, RichText, Rounding, Sense, Stroke, Vec2};
use rsrelay_core::{
    detect_local_lan_ip, has_system_loopback_permission, list_audio_devices,
    request_system_loopback_permission, AudioDeviceInfo, CaptureSource, ChannelLayout,
    DiscoveredPeer, DiscoveryService, DuplexRole, DuplexSession, DuplexSessionConfig,
    LatencyProfile, ReceiverSession, ReceiverSessionConfig, SenderSession, SenderSessionConfig,
    DEFAULT_AUDIO_UDP_PORT, DEFAULT_UPLINK_UDP_PORT,
};
use tray_icon::{
    menu::{Menu, MenuEvent, MenuItem},
    Icon, TrayIcon, TrayIconBuilder,
};

use crate::i18n::Language;
use crate::theme::{metrics, palette};
use crate::widgets::{
    draw_sparkline, draw_vu_meter, latency_profile_selector, metric_tile, status_pill,
    transport_cta_button, SparklineBuffer, VuMeterState,
};

/// Top-level studio workspace view tabs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WorkspaceTab {
    /// Side-by-side independent Transmitter (TX) and Receiver (RX) studio consoles.
    DualConsole,
    /// Simultaneous bidirectional Wireless Headset mode (System Audio Downlink + Mic Uplink).
    FullDuplex,
    /// Local CoreAudio / WASAPI hardware device matrix and mDNS LAN discovery inspector.
    HardwareMatrix,
}

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
                rgba[idx] = 18;
                rgba[idx + 1] = 23;
                rgba[idx + 2] = 34;
                rgba[idx + 3] = 255;

                // Subtle outer ring highlight
                if dist_sq >= 14 * 14 {
                    rgba[idx] = 0;
                    rgba[idx + 1] = 212;
                    rgba[idx + 2] = 170;
                    rgba[idx + 3] = 180;
                }

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

/// Window visibility & GPU framebuffer lifecycle state for System Tray mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrayWindowState {
    /// Normal studio GUI window (`960x650`) with full GPU Retina swapchain.
    Visible,
    /// Intermediate multi-frame transition that shrinks the viewport to `1x1` and swaps buffers
    /// so macOS `WindowServer` / Metal / OpenGL deallocates the ~75 MB Retina `IOSurface`
    /// backbuffers before the window is ordered out.
    ShrinkingToTray { frames_remaining: u8 },
    /// Window is hidden (`Visible(false)` at `1x1` size) and running silently in the system tray
    /// with zero UI widget rendering and minimal RAM/GPU footprint.
    HiddenInTray,
}

fn global_tray_event_queue() -> &'static (
    std::sync::mpsc::Sender<tray_icon::menu::MenuId>,
    Mutex<std::sync::mpsc::Receiver<tray_icon::menu::MenuId>>,
) {
    static QUEUE: std::sync::OnceLock<(
        std::sync::mpsc::Sender<tray_icon::menu::MenuId>,
        Mutex<std::sync::mpsc::Receiver<tray_icon::menu::MenuId>>,
    )> = std::sync::OnceLock::new();
    QUEUE.get_or_init(|| {
        let (tx, rx) = std::sync::mpsc::channel();
        (tx, Mutex::new(rx))
    })
}

fn global_egui_wakeup_ctx() -> &'static Mutex<Option<egui::Context>> {
    static CTX: std::sync::OnceLock<Mutex<Option<egui::Context>>> = std::sync::OnceLock::new();
    CTX.get_or_init(|| Mutex::new(None))
}

/// Release unused heap pages back to the OS kernel and toggle macOS Accessory (menu-bar-only)
/// activation policy when running headless in the system tray.
fn apply_os_background_tray_mode(in_tray_only: bool) {
    #[cfg(target_os = "macos")]
    unsafe {
        use std::ffi::{c_char, c_void};

        extern "C" {
            fn pthread_main_np() -> i32;
            fn objc_getClass(name: *const c_char) -> *mut c_void;
            fn sel_registerName(name: *const c_char) -> *mut c_void;
            fn objc_msgSend();
            fn malloc_zone_pressure_relief(zone: *mut c_void, goal: usize) -> usize;
        }

        if pthread_main_np() == 1 {
            let ns_app_cls = objc_getClass(c"NSApplication".as_ptr());
            if !ns_app_cls.is_null() {
                let shared_sel = sel_registerName(c"sharedApplication".as_ptr());
                let msg_send_id: unsafe extern "C" fn(*mut c_void, *mut c_void) -> *mut c_void =
                    std::mem::transmute(objc_msgSend as *const ());
                let ns_app = msg_send_id(ns_app_cls, shared_sel);
                if !ns_app.is_null() {
                    // 0 = NSApplicationActivationPolicyRegular, 1 = NSApplicationActivationPolicyAccessory
                    let policy: isize = if in_tray_only { 1 } else { 0 };
                    let policy_sel = sel_registerName(c"setActivationPolicy:".as_ptr());
                    let msg_send_policy: unsafe extern "C" fn(
                        *mut c_void,
                        *mut c_void,
                        isize,
                    ) -> bool = std::mem::transmute(objc_msgSend as *const ());
                    let _ = msg_send_policy(ns_app, policy_sel, policy);

                    if !in_tray_only {
                        let act_sel = sel_registerName(c"activateIgnoringOtherApps:".as_ptr());
                        let msg_send_act: unsafe extern "C" fn(*mut c_void, *mut c_void, bool) =
                            std::mem::transmute(objc_msgSend as *const ());
                        msg_send_act(ns_app, act_sel, true);
                    }
                }
            }
        }

        if in_tray_only {
            let _ = malloc_zone_pressure_relief(std::ptr::null_mut(), 0);
        }
    }

    #[cfg(target_os = "windows")]
    if in_tray_only {
        extern "C" {
            fn _heapmin() -> i32;
        }
        unsafe {
            let _ = _heapmin();
        }
    }
}

struct SystemTrayHandles {
    _tray: TrayIcon,
    title_item: MenuItem,
    toggle_window_item: MenuItem,
    toggle_tx_item: MenuItem,
    toggle_rx_item: MenuItem,
    toggle_duplex_item: MenuItem,
    stop_all_item: MenuItem,
    quit_item: MenuItem,
    toggle_window_id: tray_icon::menu::MenuId,
    toggle_tx_id: tray_icon::menu::MenuId,
    toggle_rx_id: tray_icon::menu::MenuId,
    toggle_duplex_id: tray_icon::menu::MenuId,
    stop_all_id: tray_icon::menu::MenuId,
    quit_id: tray_icon::menu::MenuId,
}

impl SystemTrayHandles {
    fn set_language(&self, lang: Language, window_hidden: bool) {
        let t = lang.texts();
        self.title_item.set_text(t.tray_title);
        self.toggle_window_item.set_text(if window_hidden {
            t.tray_show_console
        } else {
            t.tray_hide_console
        });
        self.toggle_tx_item.set_text(t.tray_toggle_tx);
        self.toggle_rx_item.set_text(t.tray_toggle_rx);
        self.toggle_duplex_item.set_text(t.tray_toggle_duplex);
        self.stop_all_item.set_text(t.tray_stop_all);
        self.quit_item.set_text(t.tray_quit);
    }
}

fn init_system_tray(lang: Language) -> anyhow::Result<SystemTrayHandles> {
    let (rgba, w, h) = build_app_icon_rgba();
    let icon = Icon::from_rgba(rgba, w, h)?;
    let t = lang.texts();

    let menu = Menu::new();
    let title_item = MenuItem::new(t.tray_title, false, None);
    let toggle_window_item = MenuItem::new(t.tray_hide_console, true, None);
    let toggle_tx_item = MenuItem::new(t.tray_toggle_tx, true, None);
    let toggle_rx_item = MenuItem::new(t.tray_toggle_rx, true, None);
    let toggle_duplex_item = MenuItem::new(t.tray_toggle_duplex, true, None);
    let stop_all_item = MenuItem::new(t.tray_stop_all, true, None);
    let quit_item = MenuItem::new(t.tray_quit, true, None);

    let toggle_window_id = toggle_window_item.id().clone();
    let toggle_tx_id = toggle_tx_item.id().clone();
    let toggle_rx_id = toggle_rx_item.id().clone();
    let toggle_duplex_id = toggle_duplex_item.id().clone();
    let stop_all_id = stop_all_item.id().clone();
    let quit_id = quit_item.id().clone();

    menu.append(&title_item)?;
    menu.append(&toggle_window_item)?;
    menu.append(&toggle_tx_item)?;
    menu.append(&toggle_rx_item)?;
    menu.append(&toggle_duplex_item)?;
    menu.append(&stop_all_item)?;
    menu.append(&quit_item)?;

    // Install event handler that forwards tray menu clicks and immediately wakes up egui even when hidden
    static HANDLER_INSTALLED: AtomicBool = AtomicBool::new(false);
    if !HANDLER_INSTALLED.swap(true, Ordering::SeqCst) {
        MenuEvent::set_event_handler(Some(|event: MenuEvent| {
            let (tx, _) = global_tray_event_queue();
            let _ = tx.send(event.id);
            if let Ok(guard) = global_egui_wakeup_ctx().lock() {
                if let Some(ref ctx) = *guard {
                    ctx.request_repaint();
                }
            }
        }));
    }

    let tray = TrayIconBuilder::new()
        .with_menu(Box::new(menu))
        .with_tooltip("rsrelay — Studio Audio Relay")
        .with_icon(icon)
        .build()?;

    Ok(SystemTrayHandles {
        _tray: tray,
        title_item,
        toggle_window_item,
        toggle_tx_item,
        toggle_rx_item,
        toggle_duplex_item,
        stop_all_item,
        quit_item,
        toggle_window_id,
        toggle_tx_id,
        toggle_rx_id,
        toggle_duplex_id,
        stop_all_id,
        quit_id,
    })
}

pub struct RsRelayGuiApp {
    pub lang: Language,
    pub active_tab: WorkspaceTab,
    theme_initialized: bool,
    pub local_lan_ip: Option<IpAddr>,
    pub sck_permission_granted: bool,

    pub devices: Vec<AudioDeviceInfo>,
    pending_devices: Arc<Mutex<Option<(Vec<AudioDeviceInfo>, bool)>>>,
    polling_devices: Arc<AtomicBool>,
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

    // Real-time studio VU meters & telemetry sparklines
    tx_vu_meter: VuMeterState,
    rx_vu_meter: VuMeterState,
    duplex_tx_vu_meter: VuMeterState,
    duplex_rx_vu_meter: VuMeterState,
    rx_buffer_sparkline: SparklineBuffer,
    duplex_buffer_sparkline: SparklineBuffer,

    // Status message & tray
    pub auto_follow_default_device: bool,
    last_device_poll: std::time::Instant,
    pub last_status_msg: String,
    pub tray_window_state: TrayWindowState,
    pub quit_requested: bool,
    tray: Option<SystemTrayHandles>,
}

impl RsRelayGuiApp {
    pub fn new() -> anyhow::Result<Self> {
        Self::new_with_tray(true)
    }

    pub fn new_with_tray(enable_tray: bool) -> anyhow::Result<Self> {
        let lang = Language::detect_system();
        let devices = list_audio_devices().unwrap_or_default();
        let tray = if enable_tray {
            init_system_tray(lang).ok()
        } else {
            None
        };
        let local_lan_ip = detect_local_lan_ip();
        let sck_permission_granted = has_system_loopback_permission();

        Ok(Self {
            lang,
            active_tab: WorkspaceTab::DualConsole,
            theme_initialized: false,
            local_lan_ip,
            sck_permission_granted,
            devices,
            pending_devices: Arc::new(Mutex::new(None)),
            polling_devices: Arc::new(AtomicBool::new(false)),
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
            tx_vu_meter: VuMeterState::default(),
            rx_vu_meter: VuMeterState::default(),
            duplex_tx_vu_meter: VuMeterState::default(),
            duplex_rx_vu_meter: VuMeterState::default(),
            rx_buffer_sparkline: SparklineBuffer::new(64),
            duplex_buffer_sparkline: SparklineBuffer::new(64),
            auto_follow_default_device: true,
            last_device_poll: std::time::Instant::now(),
            last_status_msg: lang.status_ready(),
            tray_window_state: TrayWindowState::Visible,
            quit_requested: false,
            tray,
        })
    }

    /// Switch the UI language and update system tray items & status message.
    pub fn set_language(&mut self, lang: Language) {
        self.lang = lang;
        let hidden = self.tray_window_state != TrayWindowState::Visible;
        if let Some(ref tray) = self.tray {
            tray.set_language(lang, hidden);
        }
        self.last_status_msg = lang.status_language_switched();
    }

    /// Shrink the window to `1x1` to flush the ~75 MB Retina `IOSurface` swapchain and hide
    /// into the OS system tray for ultra-low-memory background operation.
    pub fn enter_tray_background_mode(&mut self, ctx: &egui::Context) {
        if self.tray_window_state == TrayWindowState::HiddenInTray {
            return;
        }
        self.tray_window_state = TrayWindowState::ShrinkingToTray {
            frames_remaining: 3,
        };
        if let Some(ref tray) = self.tray {
            tray.toggle_window_item
                .set_text(self.lang.texts().tray_show_console);
        }
        self.last_status_msg = self.lang.status_entered_tray_mode();
        ctx.send_viewport_cmd(egui::ViewportCommand::MinInnerSize(Vec2::new(1.0, 1.0)));
        ctx.send_viewport_cmd(egui::ViewportCommand::InnerSize(Vec2::new(1.0, 1.0)));
        ctx.request_repaint();
    }

    /// Restore the full `960x650` Studio Console window from system tray background mode.
    pub fn restore_from_tray(&mut self, ctx: &egui::Context) {
        self.tray_window_state = TrayWindowState::Visible;
        apply_os_background_tray_mode(false);
        if let Some(ref tray) = self.tray {
            tray.toggle_window_item
                .set_text(self.lang.texts().tray_hide_console);
        }
        self.last_status_msg = self.lang.status_restored_from_tray();
        ctx.send_viewport_cmd(egui::ViewportCommand::Visible(true));
        ctx.send_viewport_cmd(egui::ViewportCommand::MinInnerSize(Vec2::new(820.0, 540.0)));
        ctx.send_viewport_cmd(egui::ViewportCommand::InnerSize(Vec2::new(960.0, 650.0)));
        ctx.send_viewport_cmd(egui::ViewportCommand::Focus);
        ctx.request_repaint();
    }

    /// Update the top line of the system tray menu with real-time stream status.
    fn sync_tray_status_title(&self) {
        let Some(ref tray) = self.tray else {
            return;
        };
        let status_line = if let Some(ref dpx) = self.active_duplex {
            let st = dpx.status();
            format!(
                "● DUPLEX LIVE — TX: {} | RX Buf: {:.1} ms",
                st.tx.net.packets_sent, st.rx.net.buffer_level_ms
            )
        } else if self.active_sender.is_some() && self.active_receiver.is_some() {
            let tx_pkts = self
                .active_sender
                .as_ref()
                .map(|s| s.status().net.packets_sent)
                .unwrap_or(0);
            let rx_buf = self
                .active_receiver
                .as_ref()
                .map(|r| r.status().net.buffer_level_ms)
                .unwrap_or(0.0);
            format!("● TX+RX LIVE — Sent: {tx_pkts} | Buf: {rx_buf:.1} ms")
        } else if let Some(ref tx) = self.active_sender {
            let st = tx.status();
            format!("● TX LIVE — {} pkts ({} Hz)", st.net.packets_sent, st.sample_rate)
        } else if let Some(ref rx) = self.active_receiver {
            let st = rx.status();
            format!(
                "● RX LIVE — Buf: {:.1} ms ({} pkts)",
                st.net.buffer_level_ms, st.net.packets_received
            )
        } else {
            self.lang.texts().tray_title.to_string()
        };
        tray.title_item.set_text(status_line);
    }

    /// Trigger an asynchronous hardware audio device enumeration on a background thread
    /// so the UI render thread never blocks on OS audio driver queries.
    pub fn refresh_audio_devices(&mut self, manual: bool) {
        if self.polling_devices.swap(true, Ordering::Relaxed) {
            return;
        }
        let pending = Arc::clone(&self.pending_devices);
        let polling = Arc::clone(&self.polling_devices);
        thread::spawn(move || {
            let new_devices = list_audio_devices().unwrap_or_default();
            if let Ok(mut guard) = pending.lock() {
                *guard = Some((new_devices, manual));
            }
            polling.store(false, Ordering::Relaxed);
        });
    }

    /// Apply any completed background audio device enumeration result.
    fn apply_pending_device_updates(&mut self) {
        let maybe_update = self
            .pending_devices
            .lock()
            .ok()
            .and_then(|mut guard| guard.take());
        let Some((new_devices, manual)) = maybe_update else {
            return;
        };

        let in_count = new_devices.iter().filter(|d| d.is_input).count();
        let out_count = new_devices.iter().filter(|d| d.is_output).count();

        let new_default_in = new_devices
            .iter()
            .find(|d| d.is_default_input)
            .map(|d| d.name.clone());
        let new_default_out = new_devices
            .iter()
            .find(|d| d.is_default_output)
            .map(|d| d.name.clone());

        self.devices = new_devices;
        self.local_lan_ip = detect_local_lan_ip();
        self.sck_permission_granted = has_system_loopback_permission();

        if manual {
            self.last_status_msg = self.lang.status_refreshed_ports(in_count, out_count);
        }

        if self.auto_follow_default_device || manual {
            if self.selected_rx_device.is_none() {
                if let (Some(ref rx), Some(ref def_out)) = (&self.active_receiver, &new_default_out)
                {
                    if &rx.status().device_name != def_out {
                        self.active_receiver = None;
                        self.toggle_receiver();
                        self.last_status_msg = self.lang.status_hotswapped_output(def_out);
                    }
                }
            }

            if self.selected_tx_device.is_none()
                && self.selected_source == CaptureSource::Microphone
            {
                if let (Some(ref tx), Some(ref def_in)) = (&self.active_sender, &new_default_in) {
                    if &tx.status().device_name != def_in {
                        self.active_sender = None;
                        self.toggle_sender();
                        self.last_status_msg = self.lang.status_hotswapped_input(def_in);
                    }
                }
            }
        }
    }

    pub fn trigger_lan_scan(&mut self) {
        if self.scanning_lan.load(Ordering::Relaxed) {
            return;
        }
        self.scanning_lan.store(true, Ordering::Relaxed);
        self.last_status_msg = self.lang.status_scanning_lan();
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

    pub fn stop_all_streams(&mut self) {
        self.active_sender = None;
        self.active_receiver = None;
        self.active_duplex = None;
        self.rx_buffer_sparkline.clear();
        self.duplex_buffer_sparkline.clear();
        self.last_status_msg = self.lang.status_all_stopped();
    }

    pub fn toggle_sender(&mut self) {
        if self.active_sender.is_some() {
            self.active_sender = None;
            self.last_status_msg = self.lang.status_sender_stopped();
            return;
        }

        // Stop Full-Duplex session if active to avoid device/port conflicts
        self.active_duplex = None;

        let target_addr: SocketAddr = match self.target_addr_input.trim().parse() {
            Ok(addr) => addr,
            Err(e) => {
                self.last_status_msg = self.lang.status_invalid_target_addr(&e);
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
                self.sck_permission_granted = has_system_loopback_permission();
                let st = session.status();
                self.last_status_msg = self.lang.status_sender_streaming(
                    &st.device_name,
                    st.sample_rate,
                    target_addr,
                );
                self.active_sender = Some(session);
            }
            Err(e) => {
                self.sck_permission_granted = has_system_loopback_permission();
                self.last_status_msg = self.lang.status_sender_failed(&e);
            }
        }
    }

    pub fn toggle_receiver(&mut self) {
        if self.active_receiver.is_some() {
            self.active_receiver = None;
            self.rx_buffer_sparkline.clear();
            self.last_status_msg = self.lang.status_receiver_stopped();
            return;
        }

        // Stop Full-Duplex session if active to avoid UDP bind port conflicts
        self.active_duplex = None;

        let bind_addr: SocketAddr = match self.bind_addr_input.trim().parse() {
            Ok(addr) => addr,
            Err(e) => {
                self.last_status_msg = self.lang.status_invalid_bind_addr(&e);
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
                self.rx_buffer_sparkline.clear();
                self.last_status_msg = self.lang.status_receiver_listening(
                    bind_addr,
                    &st.device_name,
                    st.sample_rate,
                );
                self.active_receiver = Some(session);
            }
            Err(e) => {
                self.last_status_msg = self.lang.status_receiver_failed(&e);
            }
        }
    }

    pub fn toggle_duplex(&mut self) {
        if self.active_duplex.is_some() {
            self.active_duplex = None;
            self.duplex_buffer_sparkline.clear();
            self.last_status_msg = self.lang.status_duplex_stopped();
            return;
        }

        // Stop standalone single-direction sessions before binding duplex ports
        self.active_sender = None;
        self.active_receiver = None;

        let peer_ip: IpAddr = match self.duplex_peer_ip_input.trim().parse() {
            Ok(ip) => ip,
            Err(e) => {
                self.last_status_msg = self.lang.status_invalid_peer_ip(&e);
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
                self.duplex_buffer_sparkline.clear();
                self.last_status_msg = self.lang.status_duplex_active(
                    self.duplex_role,
                    peer_ip,
                    &st.tx.device_name,
                    &st.rx.device_name,
                );
                self.active_duplex = Some(session);
            }
            Err(e) => {
                self.last_status_msg = self.lang.status_duplex_failed(&e);
            }
        }
    }

    /// Sample live audio meters and sparklines on each UI frame.
    fn update_live_telemetry(&mut self) {
        if let Some(ref tx) = self.active_sender {
            let st = tx.status();
            self.tx_vu_meter.update(st.peak_level, true);
        } else {
            self.tx_vu_meter.update(0.0, false);
        }

        if let Some(ref rx) = self.active_receiver {
            let st = rx.status();
            self.rx_vu_meter.update(st.peak_level, true);
            if st.net.packets_received > 0 {
                self.rx_buffer_sparkline.push(st.net.buffer_level_ms);
            }
        } else {
            self.rx_vu_meter.update(0.0, false);
        }

        if let Some(ref dpx) = self.active_duplex {
            let st = dpx.status();
            self.duplex_tx_vu_meter.update(st.tx.peak_level, true);
            self.duplex_rx_vu_meter.update(st.rx.peak_level, true);
            if st.rx.net.packets_received > 0 {
                self.duplex_buffer_sparkline.push(st.rx.net.buffer_level_ms);
            }
        } else {
            self.duplex_tx_vu_meter.update(0.0, false);
            self.duplex_rx_vu_meter.update(0.0, false);
        }
    }

    fn render_top_header(&mut self, ctx: &egui::Context) {
        let t = self.lang.texts();
        egui::TopBottomPanel::top("studio_header_bar")
            .frame(
                egui::Frame::none()
                    .fill(palette::BG_SURFACE)
                    .stroke(Stroke::new(1.0_f32, palette::BORDER_SUBTLE))
                    .inner_margin(egui::Margin::symmetric(18.0, 11.0)),
            )
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    // Left: Brand Logo Badge + Title
                    let (logo_rect, _) = ui.allocate_exact_size(Vec2::splat(28.0), Sense::hover());
                    if ui.is_rect_visible(logo_rect) {
                        let p = ui.painter();
                        p.circle(
                            logo_rect.center(),
                            13.5,
                            palette::BG_TRENCH,
                            Stroke::new(1.5_f32, palette::ACCENT_MINT),
                        );
                        let bars = [5.0f32, 11.0, 16.0, 10.0, 14.0];
                        for (i, &bh) in bars.iter().enumerate() {
                            let bx = logo_rect.center().x - 6.0 + (i as f32) * 3.0;
                            let by0 = logo_rect.center().y - bh * 0.5;
                            let by1 = logo_rect.center().y + bh * 0.5;
                            p.line_segment(
                                [egui::pos2(bx, by0), egui::pos2(bx, by1)],
                                Stroke::new(1.8_f32, palette::ACCENT_MINT),
                            );
                        }
                    }

                    ui.vertical(|ui| {
                        ui.spacing_mut().item_spacing.y = 0.0;
                        ui.horizontal(|ui| {
                            ui.label(
                                RichText::new("rsrelay")
                                    .size(16.5)
                                    .color(palette::TEXT_PRIMARY)
                                    .strong(),
                            );
                            egui::Frame::none()
                                .fill(palette::ACCENT_MINT_GLOW)
                                .stroke(Stroke::new(1.0_f32, palette::ACCENT_MINT))
                                .rounding(Rounding::same(4.0))
                                .inner_margin(egui::Margin::symmetric(5.0, 1.0))
                                .show(ui, |ui| {
                                    ui.label(
                                        RichText::new("STUDIO DSP")
                                            .size(9.0)
                                            .family(FontFamily::Monospace)
                                            .color(palette::ACCENT_MINT)
                                            .strong(),
                                    );
                                });
                        });
                        ui.label(
                            RichText::new(t.app_subtitle)
                                .size(10.5)
                                .color(palette::TEXT_MUTED),
                        );
                    });

                    ui.add_space(16.0);

                    // Center: Segmented Workspace Mode Switcher
                    egui::Frame::none()
                        .fill(palette::BG_TRENCH)
                        .stroke(Stroke::new(1.0_f32, palette::BORDER_SUBTLE))
                        .rounding(Rounding::same(metrics::RADIUS_WIDGET))
                        .inner_margin(egui::Margin::same(3.0))
                        .show(ui, |ui| {
                            ui.spacing_mut().item_spacing.x = 4.0;
                            let tabs = [
                                (WorkspaceTab::DualConsole, t.tab_dual_console),
                                (WorkspaceTab::FullDuplex, t.tab_full_duplex),
                                (WorkspaceTab::HardwareMatrix, t.tab_hardware_matrix),
                            ];
                            for (tab, label) in tabs {
                                let selected = self.active_tab == tab;
                                let fill = if selected {
                                    palette::BG_ACTIVE
                                } else {
                                    Color32::TRANSPARENT
                                };
                                let fg = if selected {
                                    palette::ACCENT_MINT
                                } else {
                                    palette::TEXT_SECONDARY
                                };
                                let stroke = if selected {
                                    Stroke::new(1.0_f32, palette::ACCENT_MINT)
                                } else {
                                    Stroke::NONE
                                };
                                if ui
                                    .add(
                                        egui::Button::new(
                                            RichText::new(label).size(12.0).color(fg).strong(),
                                        )
                                        .fill(fill)
                                        .stroke(stroke)
                                        .rounding(Rounding::same(5.0)),
                                    )
                                    .clicked()
                                {
                                    self.active_tab = tab;
                                }
                            }
                        });

                    // Right: Language Switcher, Tray Background Mode, Quick LAN Scan & Refresh actions + Live Stream Indicators
                    ui.with_layout(egui::Layout::right_to_left(Align::Center), |ui| {
                        if self.tray.is_some()
                            && ui
                                .button(
                                    RichText::new(t.btn_minimize_tray)
                                        .size(11.5)
                                        .color(palette::ACCENT_CYAN)
                                        .strong(),
                                )
                                .on_hover_text(t.btn_minimize_tray_hover)
                                .clicked()
                        {
                            self.enter_tray_background_mode(ctx);
                        }

                        if ui
                            .button(
                                RichText::new(t.lang_toggle_btn)
                                    .size(11.5)
                                    .color(palette::ACCENT_MINT)
                                    .strong(),
                            )
                            .on_hover_text(t.lang_toggle_hover)
                            .clicked()
                        {
                            self.set_language(self.lang.toggle());
                        }

                        if ui
                            .button(RichText::new(t.btn_refresh_ports).size(12.0))
                            .on_hover_text(t.btn_refresh_ports_hover)
                            .clicked()
                        {
                            self.refresh_audio_devices(true);
                        }

                        let scanning = self.scanning_lan.load(Ordering::Relaxed);
                        let scan_label = if scanning {
                            t.btn_scanning_lan
                        } else {
                            t.btn_scan_lan
                        };
                        if ui
                            .add_enabled(
                                !scanning,
                                egui::Button::new(RichText::new(scan_label).size(12.0)),
                            )
                            .on_hover_text(t.btn_scan_lan_hover)
                            .clicked()
                        {
                            self.trigger_lan_scan();
                        }

                        if self.active_duplex.is_some() {
                            status_pill(ui, "DUPLEX LIVE", true, palette::ACCENT_INDIGO);
                        } else {
                            if self.active_receiver.is_some() {
                                status_pill(ui, "RX LIVE", true, palette::ACCENT_CYAN);
                            }
                            if self.active_sender.is_some() {
                                status_pill(ui, "TX LIVE", true, palette::ACCENT_MINT);
                            }
                        }

                        if let Some(ip) = self.local_lan_ip {
                            let ip_label = format!("LAN: {ip}");
                            if ui
                                .add(
                                    egui::Button::new(
                                        RichText::new(ip_label)
                                            .size(11.0)
                                            .family(FontFamily::Monospace)
                                            .color(palette::ACCENT_CYAN),
                                    )
                                    .fill(palette::BG_TRENCH)
                                    .stroke(Stroke::new(1.0_f32, palette::BORDER_SUBTLE))
                                    .rounding(Rounding::same(metrics::RADIUS_PILL)),
                                )
                                .on_hover_text(t.lan_ip_hover)
                                .clicked()
                            {
                                ui.ctx().copy_text(ip.to_string());
                                self.last_status_msg = self.lang.status_copied_lan_ip(ip);
                            }
                        }
                    });
                });
            });
    }

    fn render_bottom_footer(&mut self, ctx: &egui::Context) {
        let t = self.lang.texts();
        egui::TopBottomPanel::bottom("studio_footer_bar")
            .frame(
                egui::Frame::none()
                    .fill(palette::BG_SURFACE)
                    .stroke(Stroke::new(1.0_f32, palette::BORDER_SUBTLE))
                    .inner_margin(egui::Margin::symmetric(18.0, 7.0)),
            )
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    let any_active = self.active_sender.is_some()
                        || self.active_receiver.is_some()
                        || self.active_duplex.is_some();
                    let dot_color = if any_active {
                        palette::ACCENT_MINT
                    } else {
                        palette::TEXT_MUTED
                    };
                    let (dot_rect, _) = ui.allocate_exact_size(Vec2::splat(8.0), Sense::hover());
                    ui.painter().circle_filled(dot_rect.center(), 4.0, dot_color);

                    ui.label(
                        RichText::new(&self.last_status_msg)
                            .size(11.5)
                            .color(palette::TEXT_SECONDARY),
                    );

                    ui.with_layout(egui::Layout::right_to_left(Align::Center), |ui| {
                        if any_active
                            && ui
                                .add(
                                    egui::Button::new(
                                        RichText::new(t.btn_stop_all)
                                            .size(11.0)
                                            .color(Color32::WHITE)
                                            .strong(),
                                    )
                                    .fill(palette::STATUS_DANGER_DIM)
                                    .stroke(Stroke::new(1.0_f32, palette::STATUS_DANGER)),
                                )
                                .clicked()
                        {
                            self.stop_all_streams();
                        }

                        ui.checkbox(
                            &mut self.auto_follow_default_device,
                            RichText::new(t.chk_auto_follow_hotplug)
                                .size(11.5)
                                .color(palette::TEXT_SECONDARY),
                        );

                        let in_cnt = self.devices.iter().filter(|d| d.is_input).count();
                        let out_cnt = self.devices.iter().filter(|d| d.is_output).count();
                        ui.label(
                            RichText::new(format!("{in_cnt} IN / {out_cnt} OUT"))
                                .size(11.0)
                                .family(FontFamily::Monospace)
                                .color(palette::TEXT_MUTED),
                        );
                    });
                });
            });
    }

    fn render_lan_discovery_strip(&mut self, ui: &mut egui::Ui) {
        let peers_snapshot = self
            .discovered_peers
            .lock()
            .ok()
            .map(|g| g.clone())
            .unwrap_or_default();

        if peers_snapshot.is_empty() {
            return;
        }

        let t = self.lang.texts();
        theme::trench_frame().show(ui, |ui| {
            ui.horizontal_wrapped(|ui| {
                ui.label(
                    RichText::new(t.discovered_lan_peers)
                        .size(10.5)
                        .family(FontFamily::Monospace)
                        .color(palette::ACCENT_CYAN)
                        .strong(),
                );
                ui.add_space(4.0);
                for peer in &peers_snapshot {
                    if let Some(ip) = peer.addresses.first() {
                        let addr_str = format!("{ip}:{}", peer.udp_port);
                        let is_selected = self.target_addr_input == addr_str
                            || self.duplex_peer_ip_input == ip.to_string();
                        let chip_label =
                            format!("{} ({}) • {}", peer.hostname, peer.os, addr_str);
                        let stroke_col = if is_selected {
                            palette::ACCENT_MINT
                        } else {
                            palette::BORDER_WIDGET
                        };
                        if ui
                            .add(
                                egui::Button::new(
                                    RichText::new(chip_label)
                                        .size(11.5)
                                        .family(FontFamily::Monospace)
                                        .color(if is_selected {
                                            palette::ACCENT_MINT
                                        } else {
                                            palette::TEXT_PRIMARY
                                        }),
                                )
                                .stroke(Stroke::new(1.0_f32, stroke_col))
                                .rounding(Rounding::same(metrics::RADIUS_PILL)),
                            )
                            .on_hover_text(t.discovered_peer_hover)
                            .clicked()
                        {
                            self.target_addr_input = addr_str.clone();
                            self.duplex_peer_ip_input = ip.to_string();
                            self.last_status_msg =
                                self.lang.status_selected_lan_peer(&peer.hostname, &addr_str);
                        }
                    }
                }
            });
        });
        ui.add_space(metrics::SPACE_SM);
    }

    fn render_dual_console_tab(&mut self, ui: &mut egui::Ui) {
        let lang = self.lang;
        let t = lang.texts();
        ui.columns(2, |columns| {
            // =========================================================================
            // LEFT COLUMN: 01 // TRANSMITTER (SENDER)
            // =========================================================================
            let tx_running = self.active_sender.is_some();
            theme::studio_card_frame(tx_running, palette::ACCENT_MINT).show(
                &mut columns[0],
                |ui| {
                    // Card Header
                    ui.horizontal(|ui| {
                        ui.vertical(|ui| {
                            ui.spacing_mut().item_spacing.y = 2.0;
                            ui.label(
                                RichText::new(t.tx_card_title)
                                    .size(13.5)
                                    .family(FontFamily::Monospace)
                                    .color(palette::TEXT_PRIMARY)
                                    .strong(),
                            );
                            ui.label(
                                RichText::new(t.tx_card_subtitle)
                                    .size(11.0)
                                    .color(palette::TEXT_MUTED),
                            );
                        });
                        ui.with_layout(egui::Layout::right_to_left(Align::Center), |ui| {
                            status_pill(
                                ui,
                                if tx_running {
                                    t.status_streaming
                                } else {
                                    t.status_standby
                                },
                                tx_running,
                                palette::ACCENT_MINT,
                            );
                        });
                    });

                    ui.add_space(metrics::SPACE_SM);
                    ui.separator();
                    ui.add_space(metrics::SPACE_XS);

                    // Aligned Form Grid
                    egui::Grid::new("sender_form_grid")
                        .num_columns(2)
                        .spacing([12.0, 9.0])
                        .min_col_width(metrics::LABEL_COL_WIDTH)
                        .show(ui, |ui| {
                            ui.label(
                                RichText::new(t.label_target_ip_port)
                                    .color(palette::TEXT_SECONDARY),
                            );
                            ui.add(
                                egui::TextEdit::singleline(&mut self.target_addr_input)
                                    .font(egui::TextStyle::Monospace)
                                    .desired_width(ui.available_width()),
                            );
                            ui.end_row();

                            ui.label(
                                RichText::new(t.label_audio_source)
                                    .color(palette::TEXT_SECONDARY),
                            );
                            egui::ComboBox::from_id_salt("source_combo")
                                .width(ui.available_width())
                                .selected_text(match self.selected_source {
                                    CaptureSource::SystemLoopback => t.source_system_loopback,
                                    CaptureSource::Microphone => t.source_microphone,
                                    CaptureSource::SyntheticTone => t.source_synthetic_tone,
                                })
                                .show_ui(ui, |ui| {
                                    ui.selectable_value(
                                        &mut self.selected_source,
                                        CaptureSource::SystemLoopback,
                                        t.source_system_loopback,
                                    );
                                    ui.selectable_value(
                                        &mut self.selected_source,
                                        CaptureSource::Microphone,
                                        t.source_microphone,
                                    );
                                    ui.selectable_value(
                                        &mut self.selected_source,
                                        CaptureSource::SyntheticTone,
                                        t.source_synthetic_tone,
                                    );
                                });
                            ui.end_row();

                            ui.label(
                                RichText::new(t.label_input_device)
                                    .color(palette::TEXT_SECONDARY),
                            );
                            ui.horizontal(|ui| {
                                let current_label = self
                                    .selected_tx_device
                                    .as_deref()
                                    .unwrap_or(t.device_system_default);
                                let prev_dev = self.selected_tx_device.clone();
                                egui::ComboBox::from_id_salt("tx_device_combo")
                                    .width((ui.available_width() - 32.0).max(120.0))
                                    .selected_text(current_label)
                                    .show_ui(ui, |ui| {
                                        ui.selectable_value(
                                            &mut self.selected_tx_device,
                                            None,
                                            t.device_system_default,
                                        );
                                        for dev in &self.devices {
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
                                if self.selected_tx_device != prev_dev
                                    && self.active_sender.is_some()
                                {
                                    self.active_sender = None;
                                    self.toggle_sender();
                                }
                                if ui
                                    .small_button("↻")
                                    .on_hover_text(t.hover_refresh_input_ports)
                                    .clicked()
                                {
                                    self.refresh_audio_devices(true);
                                }
                            });
                            ui.end_row();

                            ui.label(
                                RichText::new(t.label_opus_bitrate)
                                    .color(palette::TEXT_SECONDARY),
                            );
                            ui.add_enabled(
                                self.selected_profile != LatencyProfile::LosslessPcm,
                                egui::Slider::new(&mut self.opus_bitrate_kbps, 32..=256)
                                    .suffix(" kbps"),
                            );
                            ui.end_row();
                        });

                    if cfg!(target_os = "macos")
                        && self.selected_source == CaptureSource::SystemLoopback
                        && self.selected_tx_device.is_none()
                    {
                        ui.add_space(metrics::SPACE_XS);
                        ui.horizontal_wrapped(|ui| {
                            if self.sck_permission_granted {
                                status_pill(
                                    ui,
                                    t.macos_sck_ready,
                                    true,
                                    palette::ACCENT_MINT,
                                );
                            } else {
                                status_pill(
                                    ui,
                                    t.macos_perm_required,
                                    true,
                                    palette::STATUS_WARN,
                                );
                                if ui
                                    .button(
                                        RichText::new(t.btn_grant_perm)
                                            .size(11.5)
                                            .color(palette::ACCENT_MINT)
                                            .strong(),
                                    )
                                    .on_hover_text(t.hover_grant_perm)
                                    .clicked()
                                {
                                    let _ = macos_bundle::install_macos_app_bundle();
                                    self.sck_permission_granted =
                                        request_system_loopback_permission();
                                    self.last_status_msg = if self.sck_permission_granted {
                                        lang.status_sck_granted()
                                    } else {
                                        lang.status_sck_prompted()
                                    };
                                }
                                if ui
                                    .button(
                                        RichText::new(t.btn_install_macos_app)
                                            .size(11.5)
                                            .color(palette::ACCENT_CYAN),
                                    )
                                    .on_hover_text(t.hover_install_macos_app)
                                    .clicked()
                                {
                                    match macos_bundle::install_macos_app_bundle() {
                                        Ok(path) => {
                                            macos_bundle::reveal_in_finder(&path);
                                            self.last_status_msg = lang
                                                .status_installed_macos_app(&path.display().to_string());
                                        }
                                        Err(e) => {
                                            self.last_status_msg =
                                                format!("Failed to install rsrelay.app: {e}");
                                        }
                                    }
                                }
                            }
                        });
                    }

                    ui.add_space(metrics::SPACE_SM);
                    ui.label(
                        RichText::new(t.section_latency_profile)
                            .size(10.5)
                            .family(FontFamily::Monospace)
                            .color(palette::TEXT_MUTED)
                            .strong(),
                    );
                    latency_profile_selector(ui, &mut self.selected_profile, lang);

                    ui.add_space(metrics::SPACE_SM);
                    if transport_cta_button(
                        ui,
                        t.btn_start_tx,
                        t.btn_stop_tx,
                        tx_running,
                        palette::ACCENT_MINT,
                    ) {
                        self.toggle_sender();
                    }

                    ui.add_space(metrics::SPACE_SM);
                    ui.separator();
                    ui.add_space(metrics::SPACE_XS);

                    // Studio VU Meter
                    draw_vu_meter(
                        ui,
                        t.vu_input_capture,
                        &self.tx_vu_meter,
                        tx_running,
                        false,
                        None,
                    );

                    ui.add_space(metrics::SPACE_SM);

                    // 3-Column Bento Telemetry Grid
                    let (pkts_str, kb_str, hb_str, rtt_str, rbuf_str, rtt_col) =
                        if let Some(ref sender) = self.active_sender {
                            let st = sender.status();
                            let pkts = format!("{}", st.net.packets_sent);
                            let kb = format!("{:.1} KB sent", (st.net.bytes_sent as f64) / 1024.0);
                            let hb = format!("{} frames", st.net.silence_heartbeats_sent);
                            if st.control.connected {
                                (
                                    pkts,
                                    kb,
                                    hb,
                                    format!("{:.2} ms", (st.control.rtt_us as f64) / 1000.0),
                                    format!("Remote Buf: {:.1} ms", st.control.remote_buffer_ms),
                                    palette::ACCENT_CYAN,
                                )
                            } else {
                                (
                                    pkts,
                                    kb,
                                    hb,
                                    "UDP Direct".to_string(),
                                    format!("{} Hz Stereo", st.sample_rate),
                                    palette::TEXT_PRIMARY,
                                )
                            }
                        } else {
                            (
                                "0".to_string(),
                                "0.0 KB sent".to_string(),
                                "0 frames".to_string(),
                                "Offline".to_string(),
                                t.awaiting_tx.to_string(),
                                palette::TEXT_MUTED,
                            )
                        };

                    ui.columns(3, |cols| {
                        metric_tile(
                            &mut cols[0],
                            t.tile_packets_tx,
                            &pkts_str,
                            &kb_str,
                            if tx_running {
                                palette::ACCENT_MINT
                            } else {
                                palette::TEXT_MUTED
                            },
                        );
                        metric_tile(
                            &mut cols[1],
                            t.tile_silence_heartbeat,
                            &hb_str,
                            t.tile_silence_sub,
                            if tx_running {
                                palette::TEXT_PRIMARY
                            } else {
                                palette::TEXT_MUTED
                            },
                        );
                        metric_tile(
                            &mut cols[2],
                            t.tile_control_telemetry,
                            &rtt_str,
                            &rbuf_str,
                            rtt_col,
                        );
                    });
                },
            );

            // =========================================================================
            // RIGHT COLUMN: 02 // RECEIVER (LISTENER)
            // =========================================================================
            let rx_running = self.active_receiver.is_some();
            theme::studio_card_frame(rx_running, palette::ACCENT_CYAN).show(
                &mut columns[1],
                |ui| {
                    // Card Header
                    ui.horizontal(|ui| {
                        ui.vertical(|ui| {
                            ui.spacing_mut().item_spacing.y = 2.0;
                            ui.label(
                                RichText::new(t.rx_card_title)
                                    .size(13.5)
                                    .family(FontFamily::Monospace)
                                    .color(palette::TEXT_PRIMARY)
                                    .strong(),
                            );
                            ui.label(
                                RichText::new(t.rx_card_subtitle)
                                    .size(11.0)
                                    .color(palette::TEXT_MUTED),
                            );
                        });
                        ui.with_layout(egui::Layout::right_to_left(Align::Center), |ui| {
                            status_pill(
                                ui,
                                if rx_running {
                                    t.status_listening
                                } else {
                                    t.status_standby
                                },
                                rx_running,
                                palette::ACCENT_CYAN,
                            );
                        });
                    });

                    ui.add_space(metrics::SPACE_SM);
                    ui.separator();
                    ui.add_space(metrics::SPACE_XS);

                    // Aligned Form Grid
                    egui::Grid::new("receiver_form_grid")
                        .num_columns(2)
                        .spacing([12.0, 9.0])
                        .min_col_width(metrics::LABEL_COL_WIDTH)
                        .show(ui, |ui| {
                            ui.label(
                                RichText::new(t.label_bind_ip_port).color(palette::TEXT_SECONDARY),
                            );
                            ui.horizontal(|ui| {
                                let lan_ip = self.local_lan_ip;
                                let text_w = if lan_ip.is_some() {
                                    (ui.available_width() - 92.0).max(110.0)
                                } else {
                                    ui.available_width()
                                };
                                ui.add(
                                    egui::TextEdit::singleline(&mut self.bind_addr_input)
                                        .font(egui::TextStyle::Monospace)
                                        .desired_width(text_w),
                                );
                                if let Some(ip) = lan_ip {
                                    let port = self
                                        .bind_addr_input
                                        .trim()
                                        .parse::<SocketAddr>()
                                        .map(|a| a.port())
                                        .unwrap_or(DEFAULT_AUDIO_UDP_PORT);
                                    let endpoint = format!("{ip}:{port}");
                                    if ui
                                        .small_button(
                                            RichText::new(t.btn_copy_lan)
                                                .size(11.0)
                                                .color(palette::ACCENT_CYAN),
                                        )
                                        .on_hover_text(lang.hover_copy_rx_endpoint(&endpoint))
                                        .clicked()
                                    {
                                        ui.ctx().copy_text(endpoint.clone());
                                        self.last_status_msg =
                                            lang.status_copied_rx_endpoint(&endpoint);
                                    }
                                }
                            });
                            ui.end_row();

                            ui.label(
                                RichText::new(t.label_output_device).color(palette::TEXT_SECONDARY),
                            );
                            ui.horizontal(|ui| {
                                let current_label = self
                                    .selected_rx_device
                                    .as_deref()
                                    .unwrap_or(t.device_system_default);
                                let prev_dev = self.selected_rx_device.clone();
                                egui::ComboBox::from_id_salt("rx_device_combo")
                                    .width((ui.available_width() - 32.0).max(120.0))
                                    .selected_text(current_label)
                                    .show_ui(ui, |ui| {
                                        ui.selectable_value(
                                            &mut self.selected_rx_device,
                                            None,
                                            t.device_system_default,
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
                                if self.selected_rx_device != prev_dev
                                    && self.active_receiver.is_some()
                                {
                                    self.active_receiver = None;
                                    self.toggle_receiver();
                                }
                                if ui
                                    .small_button("↻")
                                    .on_hover_text(t.hover_refresh_output_ports)
                                    .clicked()
                                {
                                    self.refresh_audio_devices(true);
                                }
                            });
                            ui.end_row();

                            ui.label(
                                RichText::new(t.label_target_buffer).color(palette::TEXT_SECONDARY),
                            );
                            ui.add(
                                egui::Slider::new(&mut self.target_buffer_ms, 5..=80)
                                    .suffix(" ms"),
                            );
                            ui.end_row();

                            ui.label(
                                RichText::new(t.label_output_gain).color(palette::TEXT_SECONDARY),
                            );
                            if ui
                                .add(
                                    egui::Slider::new(&mut self.volume, 0.0..=2.0)
                                        .custom_formatter(|n, _| format!("{:.0}%", n * 100.0)),
                                )
                                .changed()
                            {
                                if let Some(ref rx) = self.active_receiver {
                                    rx.set_volume(self.volume);
                                }
                                if let Some(ref dpx) = self.active_duplex {
                                    dpx.set_rx_volume(self.volume);
                                }
                            }
                            ui.end_row();
                        });

                    ui.add_space(metrics::SPACE_XS);
                    ui.checkbox(
                        &mut self.advertise_mdns,
                        RichText::new(t.chk_advertise_mdns)
                            .size(12.0)
                            .color(palette::TEXT_SECONDARY),
                    );

                    ui.add_space(metrics::SPACE_SM);
                    if transport_cta_button(
                        ui,
                        t.btn_start_rx,
                        t.btn_stop_rx,
                        rx_running,
                        palette::ACCENT_CYAN,
                    ) {
                        self.toggle_receiver();
                    }

                    ui.add_space(metrics::SPACE_SM);
                    ui.separator();
                    ui.add_space(metrics::SPACE_XS);

                    // Studio VU Meter
                    draw_vu_meter(
                        ui,
                        t.vu_output_playback,
                        &self.rx_vu_meter,
                        rx_running,
                        false,
                        None,
                    );

                    ui.add_space(metrics::SPACE_XS);

                    // Real-time Jitter Buffer Sparkline
                    let (buf_summary, pkts_rx, jitter_str, loss_str, fec_plc_str, asrc_str, buf_str, loss_col) =
                        if let Some(ref receiver) = self.active_receiver {
                            let st = receiver.status();
                            let loss_c = if st.net.packets_lost > 0 {
                                palette::STATUS_WARN
                            } else {
                                palette::ACCENT_MINT
                            };
                            (
                                lang.format_buffer_summary(
                                    Some(st.net.buffer_level_ms),
                                    self.target_buffer_ms,
                                ),
                                format!("{}", st.net.packets_received),
                                format!("Jitter: {} µs", st.net.jitter_us),
                                format!("Lost: {}", st.net.packets_lost),
                                format!("FEC: {} | PLC: {}", st.net.fec_recovered, st.net.plc_frames),
                                format!("{:.5}x", st.net.resample_ratio),
                                format!("Watermark: {:.1} ms", st.net.buffer_level_ms),
                                loss_c,
                            )
                        } else {
                            (
                                lang.format_buffer_summary(None, self.target_buffer_ms),
                                "0".to_string(),
                                "Jitter: 0 µs".to_string(),
                                "Lost: 0".to_string(),
                                "FEC: 0 | PLC: 0".to_string(),
                                "1.00000x".to_string(),
                                "Watermark: 0.0 ms".to_string(),
                                palette::TEXT_MUTED,
                            )
                        };

                    draw_sparkline(
                        ui,
                        t.sparkline_jitter_watermark,
                        &buf_summary,
                        t.sparkline_awaiting,
                        &self.rx_buffer_sparkline,
                        0.0,
                        (self.target_buffer_ms as f32 * 2.2).max(40.0),
                        Some(self.target_buffer_ms as f32),
                        palette::ACCENT_CYAN,
                    );

                    ui.add_space(metrics::SPACE_XS);

                    ui.columns(3, |cols| {
                        metric_tile(
                            &mut cols[0],
                            t.tile_packets_rx,
                            &pkts_rx,
                            &jitter_str,
                            if rx_running {
                                palette::ACCENT_CYAN
                            } else {
                                palette::TEXT_MUTED
                            },
                        );
                        metric_tile(
                            &mut cols[1],
                            t.tile_loss_fec_plc,
                            &loss_str,
                            &fec_plc_str,
                            loss_col,
                        );
                        metric_tile(
                            &mut cols[2],
                            t.tile_pi_clock_asrc,
                            &asrc_str,
                            &buf_str,
                            if rx_running {
                                palette::ACCENT_MINT
                            } else {
                                palette::TEXT_MUTED
                            },
                        );
                    });
                },
            );
        });
    }

    fn render_full_duplex_tab(&mut self, ui: &mut egui::Ui) {
        let lang = self.lang;
        let t = lang.texts();
        let dpx_running = self.active_duplex.is_some();

        theme::studio_card_frame(dpx_running, palette::ACCENT_INDIGO).show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.vertical(|ui| {
                    ui.spacing_mut().item_spacing.y = 2.0;
                    ui.label(
                        RichText::new(t.duplex_card_title)
                            .size(14.0)
                            .family(FontFamily::Monospace)
                            .color(palette::TEXT_PRIMARY)
                            .strong(),
                    );
                    ui.label(
                        RichText::new(t.duplex_card_subtitle)
                            .size(11.5)
                            .color(palette::TEXT_MUTED),
                    );
                });
                ui.with_layout(egui::Layout::right_to_left(Align::Center), |ui| {
                    status_pill(
                        ui,
                        if dpx_running {
                            t.status_duplex_active
                        } else {
                            t.status_standby
                        },
                        dpx_running,
                        palette::ACCENT_INDIGO,
                    );
                });
            });

            ui.add_space(metrics::SPACE_SM);
            ui.separator();
            ui.add_space(metrics::SPACE_SM);

            // Role Selector Cards (Host PC vs Wireless Headset)
            ui.columns(2, |cols| {
                let roles = [
                    (DuplexRole::Host, t.role_host_title, t.role_host_desc),
                    (DuplexRole::Headset, t.role_headset_title, t.role_headset_desc),
                ];
                for (idx, (role, title, desc)) in roles.iter().enumerate() {
                    let selected = self.duplex_role == *role;
                    let fill = if selected {
                        palette::BG_ACTIVE
                    } else {
                        palette::BG_TRENCH
                    };
                    let border = if selected {
                        palette::ACCENT_INDIGO
                    } else {
                        palette::BORDER_SUBTLE
                    };
                    let stroke_w: f32 = if selected { 1.5 } else { 1.0 };
                    let resp = egui::Frame::none()
                        .fill(fill)
                        .stroke(Stroke::new(stroke_w, border))
                        .rounding(Rounding::same(metrics::RADIUS_WIDGET))
                        .inner_margin(egui::Margin::symmetric(12.0, 8.0))
                        .show(&mut cols[idx], |ui| {
                            ui.set_min_width(ui.available_width());
                            ui.vertical(|ui| {
                                ui.spacing_mut().item_spacing.y = 2.0;
                                ui.label(
                                    RichText::new(*title)
                                        .size(12.0)
                                        .color(if selected {
                                            palette::ACCENT_INDIGO
                                        } else {
                                            palette::TEXT_PRIMARY
                                        })
                                        .strong(),
                                );
                                ui.label(
                                    RichText::new(*desc)
                                        .size(10.5)
                                        .family(FontFamily::Monospace)
                                        .color(palette::TEXT_SECONDARY),
                                );
                            });
                        })
                        .response
                        .interact(Sense::click());
                    if resp.clicked() {
                        self.duplex_role = *role;
                    }
                    if resp.hovered() {
                        cols[idx].ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
                    }
                }
            });

            ui.add_space(metrics::SPACE_SM);

            // 2-Column Parameter Grid
            ui.columns(2, |cols| {
                egui::Grid::new("duplex_left_grid")
                    .num_columns(2)
                    .spacing([12.0, 8.0])
                    .min_col_width(110.0)
                    .show(&mut cols[0], |ui| {
                        ui.label(RichText::new(t.label_peer_ip).color(palette::TEXT_SECONDARY));
                        ui.add(
                            egui::TextEdit::singleline(&mut self.duplex_peer_ip_input)
                                .font(egui::TextStyle::Monospace)
                                .desired_width(ui.available_width()),
                        );
                        ui.end_row();

                        ui.label(
                            RichText::new(t.label_udp_ports_dl_ul).color(palette::TEXT_SECONDARY),
                        );
                        ui.horizontal(|ui| {
                            ui.label(RichText::new("DL:").size(11.0).color(palette::TEXT_MUTED));
                            ui.add(
                                egui::DragValue::new(&mut self.duplex_downlink_port)
                                    .range(1024..=65535),
                            );
                            ui.add_space(8.0);
                            ui.label(RichText::new("UL:").size(11.0).color(palette::TEXT_MUTED));
                            ui.add(
                                egui::DragValue::new(&mut self.duplex_uplink_port)
                                    .range(1024..=65535),
                            );
                        });
                        ui.end_row();

                        ui.label(
                            RichText::new(t.label_tx_input_device).color(palette::TEXT_SECONDARY),
                        );
                        ui.horizontal(|ui| {
                            let current_tx = self
                                .selected_tx_device
                                .as_deref()
                                .unwrap_or(t.device_system_default);
                            let prev_tx = self.selected_tx_device.clone();
                            egui::ComboBox::from_id_salt("duplex_tx_device_combo")
                                .width((ui.available_width() - 32.0).max(120.0))
                                .selected_text(current_tx)
                                .show_ui(ui, |ui| {
                                    ui.selectable_value(
                                        &mut self.selected_tx_device,
                                        None,
                                        t.device_system_default,
                                    );
                                    for dev in &self.devices {
                                        let eligible = dev.is_input
                                            || (cfg!(target_os = "windows")
                                                && self.duplex_role == DuplexRole::Host
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
                            if self.selected_tx_device != prev_tx && self.active_duplex.is_some() {
                                self.active_duplex = None;
                                self.toggle_duplex();
                            }
                            if ui
                                .small_button("↻")
                                .on_hover_text(t.btn_refresh_ports_hover)
                                .clicked()
                            {
                                self.refresh_audio_devices(true);
                            }
                        });
                        ui.end_row();
                    });

                egui::Grid::new("duplex_right_grid")
                    .num_columns(2)
                    .spacing([12.0, 8.0])
                    .min_col_width(110.0)
                    .show(&mut cols[1], |ui| {
                        ui.label(
                            RichText::new(t.label_rx_output_device).color(palette::TEXT_SECONDARY),
                        );
                        ui.horizontal(|ui| {
                            let current_rx = self
                                .selected_rx_device
                                .as_deref()
                                .unwrap_or(t.device_system_default);
                            let prev_rx = self.selected_rx_device.clone();
                            egui::ComboBox::from_id_salt("duplex_rx_device_combo")
                                .width((ui.available_width() - 32.0).max(120.0))
                                .selected_text(current_rx)
                                .show_ui(ui, |ui| {
                                    ui.selectable_value(
                                        &mut self.selected_rx_device,
                                        None,
                                        t.device_system_default,
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
                            if self.selected_rx_device != prev_rx && self.active_duplex.is_some() {
                                self.active_duplex = None;
                                self.toggle_duplex();
                            }
                            if ui
                                .small_button("↻")
                                .on_hover_text(t.btn_refresh_ports_hover)
                                .clicked()
                            {
                                self.refresh_audio_devices(true);
                            }
                        });
                        ui.end_row();

                        ui.label(
                            RichText::new(t.label_mic_noise_gate).color(palette::TEXT_SECONDARY),
                        );
                        if ui
                            .add(
                                egui::Slider::new(&mut self.duplex_gate_db, -65.0..=-20.0)
                                    .suffix(" dBFS"),
                            )
                            .changed()
                        {
                            if let Some(ref dpx) = self.active_duplex {
                                dpx.set_tx_noise_gate_db(Some(self.duplex_gate_db));
                            }
                        }
                        ui.end_row();

                        ui.label(
                            RichText::new(t.label_output_gain_mic).color(palette::TEXT_SECONDARY),
                        );
                        ui.horizontal(|ui| {
                            if ui
                                .add(
                                    egui::Slider::new(&mut self.volume, 0.0..=2.0)
                                        .custom_formatter(|n, _| format!("{:.0}%", n * 100.0)),
                                )
                                .changed()
                            {
                                if let Some(ref rx) = self.active_receiver {
                                    rx.set_volume(self.volume);
                                }
                                if let Some(ref dpx) = self.active_duplex {
                                    dpx.set_rx_volume(self.volume);
                                }
                            }
                            let mute_label_col = if self.duplex_mic_muted {
                                palette::STATUS_WARN
                            } else {
                                palette::TEXT_PRIMARY
                            };
                            if ui
                                .checkbox(
                                    &mut self.duplex_mic_muted,
                                    RichText::new(t.chk_mute_mic).color(mute_label_col),
                                )
                                .changed()
                            {
                                if let Some(ref dpx) = self.active_duplex {
                                    dpx.set_tx_muted(self.duplex_mic_muted);
                                }
                            }
                        });
                        ui.end_row();
                    });
            });

            if cfg!(target_os = "macos")
                && self.duplex_role == DuplexRole::Host
                && self.selected_tx_device.is_none()
            {
                ui.add_space(metrics::SPACE_XS);
                ui.horizontal_wrapped(|ui| {
                    if self.sck_permission_granted {
                        status_pill(ui, t.macos_sck_ready, true, palette::ACCENT_MINT);
                    } else {
                        status_pill(ui, t.macos_perm_required, true, palette::STATUS_WARN);
                        if ui
                            .button(
                                RichText::new(t.btn_grant_perm)
                                    .size(11.5)
                                    .color(palette::ACCENT_MINT)
                                    .strong(),
                            )
                            .on_hover_text(t.hover_grant_perm)
                            .clicked()
                        {
                            let _ = macos_bundle::install_macos_app_bundle();
                            self.sck_permission_granted = request_system_loopback_permission();
                            self.last_status_msg = if self.sck_permission_granted {
                                lang.status_sck_granted()
                            } else {
                                lang.status_sck_prompted()
                            };
                        }
                        if ui
                            .button(
                                RichText::new(t.btn_install_macos_app)
                                    .size(11.5)
                                    .color(palette::ACCENT_CYAN),
                            )
                            .on_hover_text(t.hover_install_macos_app)
                            .clicked()
                        {
                            match macos_bundle::install_macos_app_bundle() {
                                Ok(path) => {
                                    macos_bundle::reveal_in_finder(&path);
                                    self.last_status_msg =
                                        lang.status_installed_macos_app(&path.display().to_string());
                                }
                                Err(e) => {
                                    self.last_status_msg =
                                        format!("Failed to install rsrelay.app: {e}");
                                }
                            }
                        }
                    }
                });
            }

            ui.add_space(metrics::SPACE_SM);
            latency_profile_selector(ui, &mut self.selected_profile, lang);

            ui.add_space(metrics::SPACE_SM);
            if transport_cta_button(
                ui,
                t.btn_start_duplex,
                t.btn_stop_duplex,
                dpx_running,
                palette::ACCENT_INDIGO,
            ) {
                self.toggle_duplex();
            }

            ui.add_space(metrics::SPACE_SM);
            ui.separator();
            ui.add_space(metrics::SPACE_SM);

            // Bidirectional TX & RX Telemetry Racks
            let st_opt = self.active_duplex.as_ref().map(|d| d.status());
            ui.columns(2, |cols| {
                // Left: Outgoing TX Pipeline
                theme::trench_frame().show(&mut cols[0], |ui| {
                    let gate_marker = if self.duplex_role == DuplexRole::Headset {
                        Some(self.duplex_gate_db)
                    } else {
                        None
                    };
                    draw_vu_meter(
                        ui,
                        t.vu_duplex_tx,
                        &self.duplex_tx_vu_meter,
                        dpx_running,
                        self.duplex_mic_muted,
                        gate_marker,
                    );
                    ui.add_space(metrics::SPACE_XS);
                    let (dev_str, pkts_str, dtx_str) = if let Some(ref st) = st_opt {
                        (
                            format!("{} ({} Hz)", st.tx.device_name, st.tx.sample_rate),
                            format!("{} pkts", st.tx.net.packets_sent),
                            format!("Gated DTX: {}", st.tx.net.gated_dtx_frames),
                        )
                    } else {
                        (
                            t.status_standby.to_string(),
                            "0 pkts".to_string(),
                            "Gated DTX: 0".to_string(),
                        )
                    };
                    ui.columns(2, |mc| {
                        metric_tile(
                            &mut mc[0],
                            t.tile_tx_capture_device,
                            &dev_str,
                            "48kHz Opus Low-Delay",
                            palette::ACCENT_MINT,
                        );
                        metric_tile(
                            &mut mc[1],
                            t.tile_uplink_telemetry,
                            &pkts_str,
                            &dtx_str,
                            palette::TEXT_PRIMARY,
                        );
                    });
                });

                // Right: Incoming RX Pipeline
                theme::trench_frame().show(&mut cols[1], |ui| {
                    draw_vu_meter(
                        ui,
                        t.vu_duplex_rx,
                        &self.duplex_rx_vu_meter,
                        dpx_running,
                        false,
                        None,
                    );
                    ui.add_space(metrics::SPACE_XS);
                    let (buf_str, asrc_str, rx_pkts) = if let Some(ref st) = st_opt {
                        (
                            format!("Buf: {:.1} ms", st.rx.net.buffer_level_ms),
                            format!("{:.5}x", st.rx.net.resample_ratio),
                            format!(
                                "{} pkts (Lost: {})",
                                st.rx.net.packets_received, st.rx.net.packets_lost
                            ),
                        )
                    } else {
                        (
                            "Buf: 0.0 ms".to_string(),
                            "1.00000x".to_string(),
                            "0 pkts (Lost: 0)".to_string(),
                        )
                    };
                    ui.columns(2, |mc| {
                        metric_tile(
                            &mut mc[0],
                            t.tile_downlink_packets,
                            &rx_pkts,
                            &buf_str,
                            palette::ACCENT_CYAN,
                        );
                        metric_tile(
                            &mut mc[1],
                            t.tile_pi_clock_asrc,
                            &asrc_str,
                            t.tile_drift_comp,
                            palette::ACCENT_MINT,
                        );
                    });
                });
            });
        });
    }

    fn render_hardware_matrix_tab(&mut self, ui: &mut egui::Ui) {
        let t = self.lang.texts();
        ui.columns(2, |cols| {
            // Left: Input Capture Hardware
            theme::studio_card_frame(false, palette::BORDER_SUBTLE).show(&mut cols[0], |ui| {
                ui.label(
                    RichText::new(t.hw_input_title)
                        .size(13.0)
                        .family(FontFamily::Monospace)
                        .color(palette::ACCENT_MINT)
                        .strong(),
                );
                ui.add_space(metrics::SPACE_XS);
                ui.separator();
                ui.add_space(metrics::SPACE_XS);

                for dev in self.devices.iter().filter(|d| d.is_input) {
                    theme::trench_frame().show(ui, |ui| {
                        ui.horizontal(|ui| {
                            ui.label(
                                RichText::new(&dev.name)
                                    .color(palette::TEXT_PRIMARY)
                                    .strong(),
                            );
                            ui.with_layout(egui::Layout::right_to_left(Align::Center), |ui| {
                                if dev.is_default_input {
                                    status_pill(ui, t.badge_default_in, true, palette::ACCENT_MINT);
                                }
                                ui.label(
                                    RichText::new(format!(
                                        "{} Hz • {} ch",
                                        dev.default_sample_rate.unwrap_or(48_000),
                                        dev.default_channels.unwrap_or(2)
                                    ))
                                    .size(11.0)
                                    .family(FontFamily::Monospace)
                                    .color(palette::TEXT_SECONDARY),
                                );
                            });
                        });
                    });
                    ui.add_space(4.0);
                }
            });

            // Right: Output Playback Hardware
            theme::studio_card_frame(false, palette::BORDER_SUBTLE).show(&mut cols[1], |ui| {
                ui.label(
                    RichText::new(t.hw_output_title)
                        .size(13.0)
                        .family(FontFamily::Monospace)
                        .color(palette::ACCENT_CYAN)
                        .strong(),
                );
                ui.add_space(metrics::SPACE_XS);
                ui.separator();
                ui.add_space(metrics::SPACE_XS);

                for dev in self.devices.iter().filter(|d| d.is_output) {
                    theme::trench_frame().show(ui, |ui| {
                        ui.horizontal(|ui| {
                            ui.label(
                                RichText::new(&dev.name)
                                    .color(palette::TEXT_PRIMARY)
                                    .strong(),
                            );
                            ui.with_layout(egui::Layout::right_to_left(Align::Center), |ui| {
                                if dev.is_default_output {
                                    status_pill(ui, t.badge_default_out, true, palette::ACCENT_CYAN);
                                }
                                ui.label(
                                    RichText::new(format!(
                                        "{} Hz • {} ch",
                                        dev.default_sample_rate.unwrap_or(48_000),
                                        dev.default_channels.unwrap_or(2)
                                    ))
                                    .size(11.0)
                                    .family(FontFamily::Monospace)
                                    .color(palette::TEXT_SECONDARY),
                                );
                            });
                        });
                    });
                    ui.add_space(4.0);
                }
            });
        });
    }
    pub fn render_ui(&mut self, ctx: &egui::Context) {
        if !self.theme_initialized {
            theme::apply_studio_theme(ctx);
            if let Ok(mut guard) = global_egui_wakeup_ctx().lock() {
                *guard = Some(ctx.clone());
            }
            self.theme_initialized = true;
        }

        // Intercept window [X] Close button to minimize into the system tray instead of killing active streams
        if self.tray.is_some()
            && !self.quit_requested
            && ctx.input(|i| i.viewport().close_requested())
        {
            ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
            self.enter_tray_background_mode(ctx);
        }

        // Poll system tray menu events
        let tray_ids = self.tray.as_ref().map(|t| {
            (
                t.toggle_window_id.clone(),
                t.toggle_tx_id.clone(),
                t.toggle_rx_id.clone(),
                t.toggle_duplex_id.clone(),
                t.stop_all_id.clone(),
                t.quit_id.clone(),
            )
        });
        if let Some((
            toggle_window_id,
            toggle_tx_id,
            toggle_rx_id,
            toggle_duplex_id,
            stop_all_id,
            quit_id,
        )) = tray_ids
        {
            let mut events = Vec::new();
            if let Ok(rx_guard) = global_tray_event_queue().1.lock() {
                while let Ok(id) = rx_guard.try_recv() {
                    events.push(id);
                }
            }
            for event_id in events {
                if event_id == toggle_window_id {
                    if self.tray_window_state == TrayWindowState::Visible {
                        self.enter_tray_background_mode(ctx);
                    } else {
                        self.restore_from_tray(ctx);
                    }
                } else if event_id == toggle_tx_id {
                    self.toggle_sender();
                } else if event_id == toggle_rx_id {
                    self.toggle_receiver();
                } else if event_id == toggle_duplex_id {
                    self.toggle_duplex();
                } else if event_id == stop_all_id {
                    self.stop_all_streams();
                    self.last_status_msg = self.lang.status_all_stopped_tray();
                } else if event_id == quit_id {
                    self.quit_requested = true;
                    self.stop_all_streams();
                    ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                }
            }
        }

        // Periodically poll for hot-plugged audio devices every 2.5 seconds on a background thread
        if self.last_device_poll.elapsed() >= Duration::from_millis(2500) {
            self.last_device_poll = std::time::Instant::now();
            self.refresh_audio_devices(false);
        }
        self.apply_pending_device_updates();
        self.sync_tray_status_title();

        // Handle System Tray Background Mode state transitions (purges ~75 MB Retina IOSurface swapchain)
        match self.tray_window_state {
            TrayWindowState::ShrinkingToTray { frames_remaining } => {
                if frames_remaining > 1 {
                    self.tray_window_state = TrayWindowState::ShrinkingToTray {
                        frames_remaining: frames_remaining - 1,
                    };
                    ctx.request_repaint();
                } else {
                    self.tray_window_state = TrayWindowState::HiddenInTray;
                    ctx.send_viewport_cmd(egui::ViewportCommand::Visible(false));
                    apply_os_background_tray_mode(true);
                    ctx.request_repaint_after(Duration::from_millis(500));
                }
                return;
            }
            TrayWindowState::HiddenInTray => {
                ctx.request_repaint_after(Duration::from_millis(500));
                return;
            }
            TrayWindowState::Visible => {}
        }

        // Request continuous repaint at ~30 Hz when a stream is active so VU meters and sparklines animate smoothly
        if self.active_sender.is_some()
            || self.active_receiver.is_some()
            || self.active_duplex.is_some()
            || self.scanning_lan.load(Ordering::Relaxed)
        {
            ctx.request_repaint_after(Duration::from_millis(33));
        }

        self.update_live_telemetry();

        self.render_top_header(ctx);
        self.render_bottom_footer(ctx);

        egui::CentralPanel::default()
            .frame(
                egui::Frame::none()
                    .fill(palette::BG_CANVAS)
                    .inner_margin(egui::Margin::symmetric(18.0, 14.0)),
            )
            .show(ctx, |ui| {
                egui::ScrollArea::vertical()
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        self.render_lan_discovery_strip(ui);

                        match self.active_tab {
                            WorkspaceTab::DualConsole => self.render_dual_console_tab(ui),
                            WorkspaceTab::FullDuplex => self.render_full_duplex_tab(ui),
                            WorkspaceTab::HardwareMatrix => self.render_hardware_matrix_tab(ui),
                        }
                    });
            });
    }
}

impl eframe::App for RsRelayGuiApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.render_ui(ctx);
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

    // Support `--install-app` flag on macOS to package and install `/Applications/rsrelay.app`
    if std::env::args().any(|a| a == "--install-app") {
        let path = macos_bundle::install_macos_app_bundle()?;
        println!("Installed macOS app bundle at: {}", path.display());
        return Ok(());
    }

    let start_in_tray = std::env::args().any(|a| a == "--tray" || a == "--background");

    let (icon_rgba, icon_w, icon_h) = build_app_icon_rgba();
    let viewport = if start_in_tray {
        egui::ViewportBuilder::default()
            .with_title("rsrelay — Studio Audio Relay")
            .with_inner_size([1.0, 1.0])
            .with_min_inner_size([1.0, 1.0])
            .with_visible(false)
            .with_icon(egui::IconData {
                rgba: icon_rgba,
                width: icon_w,
                height: icon_h,
            })
    } else {
        egui::ViewportBuilder::default()
            .with_title("rsrelay — Studio Audio Relay")
            .with_inner_size([960.0, 650.0])
            .with_min_inner_size([820.0, 540.0])
            .with_icon(egui::IconData {
                rgba: icon_rgba,
                width: icon_w,
                height: icon_h,
            })
    };

    let native_options = eframe::NativeOptions {
        viewport,
        depth_buffer: 0,
        stencil_buffer: 0,
        multisampling: 0,
        ..Default::default()
    };

    let mut app = RsRelayGuiApp::new()?;
    let args: Vec<String> = std::env::args().collect();
    for (idx, arg) in args.iter().enumerate() {
        if (arg == "--lang" || arg == "-l") && idx + 1 < args.len() {
            if let Some(lang) = Language::from_locale_str(&args[idx + 1]) {
                app.set_language(lang);
            }
        } else if let Some(val) = arg.strip_prefix("--lang=") {
            if let Some(lang) = Language::from_locale_str(val) {
                app.set_language(lang);
            }
        }
    }

    eframe::run_native(
        "rsrelay — Studio Audio Relay",
        native_options,
        Box::new(move |cc| {
            theme::apply_studio_theme(&cc.egui_ctx);
            if let Ok(mut guard) = global_egui_wakeup_ctx().lock() {
                *guard = Some(cc.egui_ctx.clone());
            }
            app.theme_initialized = true;
            if start_in_tray {
                app.enter_tray_background_mode(&cc.egui_ctx);
            }
            Ok(Box::new(app))
        }),
    )
    .map_err(|e| anyhow::anyhow!("eframe GUI error: {e}"))?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_headless_render_all_workspace_tabs_bilingual() {
        let mut app = RsRelayGuiApp::new_with_tray(false).expect("init app");
        let ctx = egui::Context::default();

        for lang in [Language::En, Language::Zh] {
            app.set_language(lang);
            assert_eq!(app.lang, lang);

            for tab in [
                WorkspaceTab::DualConsole,
                WorkspaceTab::FullDuplex,
                WorkspaceTab::HardwareMatrix,
            ] {
                app.active_tab = tab;
                let _ = ctx.run(egui::RawInput::default(), |ctx| {
                    app.render_ui(ctx);
                });
            }
        }
    }

    #[test]
    fn test_tray_background_mode_state_machine() {
        let mut app = RsRelayGuiApp::new_with_tray(false).expect("init app");
        let ctx = egui::Context::default();
        assert_eq!(app.tray_window_state, TrayWindowState::Visible);

        let _ = ctx.run(egui::RawInput::default(), |ctx| {
            app.enter_tray_background_mode(ctx);
        });
        assert_eq!(
            app.tray_window_state,
            TrayWindowState::ShrinkingToTray {
                frames_remaining: 3
            }
        );

        // Run 3 shrink frames -> transitions to HiddenInTray
        for _ in 0..3 {
            let _ = ctx.run(egui::RawInput::default(), |ctx| {
                app.render_ui(ctx);
            });
        }
        assert_eq!(app.tray_window_state, TrayWindowState::HiddenInTray);

        // Restore back to Visible
        let _ = ctx.run(egui::RawInput::default(), |ctx| {
            app.restore_from_tray(ctx);
        });
        assert_eq!(app.tray_window_state, TrayWindowState::Visible);
    }
}

