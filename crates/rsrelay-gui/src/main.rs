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
use std::time::{Duration, Instant};

use eframe::egui::{
    self, Align, Color32, FontFamily, FontId, Pos2, Rect, RichText, Rounding, Sense, Stroke, Vec2,
};
use rsrelay_core::{
    detect_all_local_ipv4s, detect_local_lan_ip, has_system_loopback_permission,
    list_audio_devices, request_system_loopback_permission, AudioDeviceInfo, CaptureSource,
    ChannelLayout, DiscoveredPeer, DiscoveryService, DuplexRole, DuplexSession,
    DuplexSessionConfig, LatencyProfile, ReceiverSession, ReceiverSessionConfig, SenderSession,
    SenderSessionConfig, DEFAULT_AUDIO_UDP_PORT, DEFAULT_UPLINK_UDP_PORT,
};
use tray_icon::{
    menu::{Menu, MenuEvent, MenuItem},
    Icon, TrayIcon, TrayIconBuilder,
};

use crate::i18n::Language;
use crate::theme::{metrics, palette, ThemeMode};
use crate::widgets::{
    draw_card_icon_tile, draw_hairline, draw_mono_gate_vu_meter, draw_sparkline, draw_stats_row_3,
    draw_stereo_vu_meter, draw_tx_waveform_chart, draw_vector_icon, format_gain_db,
    format_with_commas, ghost_stop_all_button, header_action_button, latency_profile_selector,
    linear_to_dbfs, pill_toggle, quiet_mono_combo_pill, quiet_mono_input_pill, quiet_select_pill,
    quiet_slider_f32, quiet_slider_u16, quiet_source_addr_pill, segmented_control, settings_row,
    status_chip, IconKind, SparklineBuffer, VuMeterState,
};

/// Top-level workspace navigation modes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WorkspaceTab {
    /// Frame C: Side-by-side Transmit (`发送`) and Receive (`接收`) cards.
    DualConsole,
    /// Frame C2: Full-Duplex Wireless Headset (`双工耳麦`) with top connection bar and Listen/Mic cards.
    FullDuplex,
    /// Local CoreAudio / WASAPI hardware device inspector (`音频设备`).
    HardwareMatrix,
}

/// Generate a crisp 32x32 RGBA Quiet Graphite waveform icon in memory.
pub fn build_app_icon_rgba() -> (Vec<u8>, u32, u32) {
    let width = 32u32;
    let height = 32u32;
    let mut rgba = vec![0u8; (width * height * 4) as usize];

    let bar_heights = [6i32, 14, 22, 12, 6];
    let bar_xs = [8i32, 12, 16, 20, 24];
    for y in 0..height as i32 {
        for x in 0..width as i32 {
            let idx = ((y as u32 * width + x as u32) * 4) as usize;
            let dx = (x - 16).abs();
            let dy = (y - 16).abs();

            // Rounded square badge
            if dx <= 14 && dy <= 14 && (dx <= 10 || dy <= 10 || (dx - 10) * (dx - 10) + (dy - 10) * (dy - 10) <= 16) {
                // Quiet Graphite `#1C1C1F` background
                rgba[idx] = 28;
                rgba[idx + 1] = 28;
                rgba[idx + 2] = 31;
                rgba[idx + 3] = 255;

                // 5-bar waveform in `#3CE0A0`
                for (bx, bh) in bar_xs.iter().zip(bar_heights.iter()) {
                    if (x - *bx).abs() <= 1 && (y - 16).abs() <= bh / 2 {
                        rgba[idx] = 60;
                        rgba[idx + 1] = 224;
                        rgba[idx + 2] = 160;
                        rgba[idx + 3] = 255;
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
    /// Normal Quiet Graphite desktop window (`1220x760`).
    Visible,
    /// Multi-frame transition that shrinks viewport to `1x1` to release GPU backbuffers.
    ShrinkingToTray { frames_remaining: u8 },
    /// Hidden in system tray with minimal memory footprint.
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

fn global_tray_toggle_window_id() -> &'static Mutex<Option<tray_icon::menu::MenuId>> {
    static ID: std::sync::OnceLock<Mutex<Option<tray_icon::menu::MenuId>>> =
        std::sync::OnceLock::new();
    ID.get_or_init(|| Mutex::new(None))
}

fn global_egui_wakeup_ctx() -> &'static Mutex<Option<egui::Context>> {
    static CTX: std::sync::OnceLock<Mutex<Option<egui::Context>>> = std::sync::OnceLock::new();
    CTX.get_or_init(|| Mutex::new(None))
}

#[cfg(target_os = "windows")]
mod win32_tray {
    use std::sync::atomic::{AtomicBool, AtomicIsize, Ordering};
    use std::sync::OnceLock;
    use std::thread;
    use std::time::Duration;

    pub type HWND = isize;
    pub type WPARAM = usize;
    pub type LPARAM = isize;
    pub type BOOL = i32;

    pub const WM_PAINT: u32 = 0x000F;
    pub const WM_CLOSE: u32 = 0x0010;
    pub const SW_SHOW: i32 = 5;
    pub const SW_RESTORE: i32 = 9;

    #[link(name = "user32")]
    extern "system" {
        fn EnumThreadWindows(
            dwThreadId: u32,
            lpfn: unsafe extern "system" fn(HWND, LPARAM) -> BOOL,
            lParam: LPARAM,
        ) -> BOOL;
        fn GetWindowTextW(hWnd: HWND, lpString: *mut u16, nMaxCount: i32) -> i32;
        fn PostMessageW(hWnd: HWND, Msg: u32, wParam: WPARAM, lParam: LPARAM) -> BOOL;
        fn ShowWindow(hWnd: HWND, nCmdShow: i32) -> BOOL;
        fn SetForegroundWindow(hWnd: HWND) -> BOOL;
        fn IsWindow(hWnd: HWND) -> BOOL;
        fn IsIconic(hWnd: HWND) -> BOOL;
    }

    #[link(name = "kernel32")]
    extern "system" {
        fn GetCurrentThreadId() -> u32;
    }

    static GLOBAL_MAIN_HWND: AtomicIsize = AtomicIsize::new(0);
    static WINDOWS_HIDDEN_IN_TRAY: AtomicBool = AtomicBool::new(false);

    unsafe extern "system" fn find_rsrelay_wnd_proc(hwnd: HWND, lparam: LPARAM) -> BOOL {
        let mut buf = [0u16; 64];
        let len = unsafe { GetWindowTextW(hwnd, buf.as_mut_ptr(), buf.len() as i32) };
        if len > 0 {
            let title = String::from_utf16_lossy(&buf[..len as usize]);
            if title == "rsrelay" {
                unsafe {
                    *(lparam as *mut HWND) = hwnd;
                }
                return 0; // Stop enumeration
            }
        }
        1 // Continue enumeration
    }

    /// Capture the `winit` root window `HWND` on the GUI thread and start the hidden-tray heartbeat thread.
    pub fn capture_main_window_hwnd() -> Option<HWND> {
        let current = GLOBAL_MAIN_HWND.load(Ordering::Relaxed);
        if current != 0 && unsafe { IsWindow(current) != 0 } {
            return Some(current);
        }
        let mut found: HWND = 0;
        unsafe {
            let tid = GetCurrentThreadId();
            EnumThreadWindows(
                tid,
                find_rsrelay_wnd_proc,
                (&mut found as *mut HWND) as LPARAM,
            );
        }
        if found != 0 {
            GLOBAL_MAIN_HWND.store(found, Ordering::Relaxed);
            ensure_background_pump_thread();
            Some(found)
        } else {
            None
        }
    }

    fn ensure_background_pump_thread() {
        static PUMP: OnceLock<()> = OnceLock::new();
        PUMP.get_or_init(|| {
            let _ = thread::Builder::new()
                .name("rsrelay-win32-tray-pump".to_string())
                .spawn(|| loop {
                    thread::sleep(Duration::from_millis(200));
                    if WINDOWS_HIDDEN_IN_TRAY.load(Ordering::Relaxed) {
                        wake_main_window();
                    }
                });
        });
    }

    /// Set whether the window is currently hidden or shrinking to the Windows system tray.
    pub fn set_hidden_in_tray(hidden: bool) {
        WINDOWS_HIDDEN_IN_TRAY.store(hidden, Ordering::Relaxed);
        if hidden {
            ensure_background_pump_thread();
            wake_main_window();
        }
    }

    /// Post `WM_PAINT` directly to the `winit` window queue so `WindowEvent::RedrawRequested`
    /// fires on Windows even when the window is hidden (`SW_HIDE` / `!WS_VISIBLE`).
    pub fn wake_main_window() {
        let hwnd = GLOBAL_MAIN_HWND.load(Ordering::Relaxed);
        if hwnd != 0 && unsafe { IsWindow(hwnd) != 0 } {
            unsafe {
                PostMessageW(hwnd, WM_PAINT, 0, 0);
            }
        }
    }

    /// Restore and bring the `winit` window to the foreground on Windows.
    pub fn present_and_focus_main_window() {
        let hwnd = GLOBAL_MAIN_HWND.load(Ordering::Relaxed);
        if hwnd != 0 && unsafe { IsWindow(hwnd) != 0 } {
            unsafe {
                if IsIconic(hwnd) != 0 {
                    ShowWindow(hwnd, SW_RESTORE);
                } else {
                    ShowWindow(hwnd, SW_SHOW);
                }
                SetForegroundWindow(hwnd);
                PostMessageW(hwnd, WM_PAINT, 0, 0);
            }
        }
    }

    /// Post `WM_PAINT` and `WM_CLOSE` so `eframe` processes shutdown immediately even while hidden in the tray.
    pub fn request_close_main_window() {
        let hwnd = GLOBAL_MAIN_HWND.load(Ordering::Relaxed);
        if hwnd != 0 && unsafe { IsWindow(hwnd) != 0 } {
            unsafe {
                PostMessageW(hwnd, WM_PAINT, 0, 0);
                PostMessageW(hwnd, WM_CLOSE, 0, 0);
            }
        }
    }
}

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
    {
        win32_tray::set_hidden_in_tray(in_tray_only);
        if in_tray_only {
            extern "C" {
                fn _heapmin() -> i32;
            }
            unsafe {
                let _ = _heapmin();
            }
        }
    }
}

struct SystemTrayHandles {
    _tray: Option<TrayIcon>,
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

    if let Ok(mut id_guard) = global_tray_toggle_window_id().lock() {
        *id_guard = Some(toggle_window_id.clone());
    }

    menu.append(&title_item)?;
    menu.append(&toggle_window_item)?;
    menu.append(&toggle_tx_item)?;
    menu.append(&toggle_rx_item)?;
    menu.append(&toggle_duplex_item)?;
    menu.append(&stop_all_item)?;
    menu.append(&quit_item)?;

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
            #[cfg(target_os = "windows")]
            win32_tray::wake_main_window();
        }));

        tray_icon::TrayIconEvent::set_event_handler(Some(|event: tray_icon::TrayIconEvent| {
            if let tray_icon::TrayIconEvent::DoubleClick {
                button: tray_icon::MouseButton::Left,
                ..
            } = event
            {
                if let Ok(id_guard) = global_tray_toggle_window_id().lock() {
                    if let Some(ref toggle_id) = *id_guard {
                        let (tx, _) = global_tray_event_queue();
                        let _ = tx.send(toggle_id.clone());
                    }
                }
                if let Ok(guard) = global_egui_wakeup_ctx().lock() {
                    if let Some(ref ctx) = *guard {
                        ctx.request_repaint();
                    }
                }
                #[cfg(target_os = "windows")]
                win32_tray::wake_main_window();
            }
        }));
    }

    let tray = match TrayIconBuilder::new()
        .with_menu(Box::new(menu))
        .with_tooltip("rsrelay")
        .with_icon(icon)
        .build()
    {
        Ok(t) => Some(t),
        Err(_) if std::env::args().any(|a| a == "--test-tray-cycle") => None,
        Err(e) => return Err(e.into()),
    };

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

fn format_elapsed_hms(started_at: Option<Instant>) -> String {
    let secs = started_at
        .map(|t| t.elapsed().as_secs())
        .unwrap_or(0);
    let h = secs / 3600;
    let m = (secs % 3600) / 60;
    let s = secs % 60;
    format!("{h:02}:{m:02}:{s:02}")
}

fn split_host_port(input: &str, default_port: u16) -> (String, String) {
    let trimmed = input.trim();
    if let Some((host, port)) = trimmed.rsplit_once(':') {
        (host.to_string(), format!(":{port}"))
    } else if trimmed.is_empty() {
        ("0.0.0.0".to_string(), format!(":{default_port}"))
    } else {
        (trimmed.to_string(), format!(":{default_port}"))
    }
}

fn parse_socket_addr_flexible(
    input: &str,
    default_ip: IpAddr,
    default_port: u16,
) -> Result<SocketAddr, std::net::AddrParseError> {
    let trimmed = input.trim();
    if let Ok(sa) = trimmed.parse::<SocketAddr>() {
        return Ok(sa);
    }
    if let Ok(ip) = trimmed.parse::<IpAddr>() {
        return Ok(SocketAddr::new(ip, default_port));
    }
    if let Some(port_str) = trimmed.strip_prefix(':') {
        if let Ok(port) = port_str.parse::<u16>() {
            return Ok(SocketAddr::new(default_ip, port));
        }
    }
    if let Ok(port) = trimmed.parse::<u16>() {
        return Ok(SocketAddr::new(default_ip, port));
    }
    trimmed.parse::<SocketAddr>()
}

pub struct RsRelayGuiApp {
    pub lang: Language,
    pub theme_mode: ThemeMode,
    pub active_tab: WorkspaceTab,
    theme_initialized: bool,
    pub local_lan_ip: Option<IpAddr>,
    pub sck_permission_granted: bool,

    pub devices: Vec<AudioDeviceInfo>,
    pending_devices: Arc<Mutex<Option<(Vec<AudioDeviceInfo>, bool)>>>,
    polling_devices: Arc<AtomicBool>,
    pub discovered_peers: Arc<Mutex<Vec<DiscoveredPeer>>>,
    pub scanning_lan: Arc<AtomicBool>,
    pub target_addr_manually_edited: bool,
    last_lan_scan: Instant,
    lan_advertiser: Option<DiscoveryService>,

    // Sender controls
    pub target_addr_input: String,
    pub selected_source: CaptureSource,
    pub selected_tx_device: Option<String>,
    pub selected_profile: LatencyProfile,
    pub opus_bitrate_kbps: u32,
    pub active_sender: Option<SenderSession>,
    tx_started_at: Option<Instant>,

    // Receiver controls
    pub bind_addr_input: String,
    pub selected_rx_device: Option<String>,
    pub target_buffer_ms: u16,
    pub volume: f32,
    pub advertise_mdns: bool,
    pub active_receiver: Option<ReceiverSession>,
    rx_started_at: Option<Instant>,

    // Full-Duplex Wireless Headset controls
    pub duplex_role: DuplexRole,
    pub duplex_peer_ip_input: String,
    pub duplex_downlink_port: u16,
    pub duplex_uplink_port: u16,
    pub duplex_gate_db: f32,
    pub duplex_mic_muted: bool,
    pub duplex_rx_channels: ChannelLayout,
    pub active_duplex: Option<DuplexSession>,
    duplex_started_at: Option<Instant>,

    // Real-time VU meters & chart sparkline buffers
    tx_vu_meter: VuMeterState,
    rx_vu_meter: VuMeterState,
    duplex_tx_vu_meter: VuMeterState,
    duplex_rx_vu_meter: VuMeterState,
    tx_waveform_sparkline: SparklineBuffer,
    rx_buffer_sparkline: SparklineBuffer,
    duplex_buffer_sparkline: SparklineBuffer,
    duplex_tx_envelope_sparkline: SparklineBuffer,

    // Support `--sck` / `--demo` / `--screenshot=<path>` flags for visual verification
    pub demo_mode: bool,
    pub screenshot_target: Option<String>,
    screenshot_frames: u8,
    pub auto_follow_default_device: bool,
    last_device_poll: Instant,
    pub last_status_msg: String,
    pub tray_window_state: TrayWindowState,
    pending_restore_focus: bool,
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

        let lan_advertiser = if enable_tray {
            DiscoveryService::new().ok().map(|mut svc| {
                let _ = svc.advertise("rsrelay-desktop", DEFAULT_AUDIO_UDP_PORT);
                svc
            })
        } else {
            None
        };

        let mut app = Self {
            lang,
            theme_mode: ThemeMode::System,
            active_tab: WorkspaceTab::DualConsole,
            theme_initialized: false,
            local_lan_ip,
            sck_permission_granted,
            devices,
            pending_devices: Arc::new(Mutex::new(None)),
            polling_devices: Arc::new(AtomicBool::new(false)),
            discovered_peers: Arc::new(Mutex::new(Vec::new())),
            scanning_lan: Arc::new(AtomicBool::new(false)),
            target_addr_manually_edited: false,
            last_lan_scan: Instant::now(),
            lan_advertiser,
            target_addr_input: format!("127.0.0.1:{DEFAULT_AUDIO_UDP_PORT}"),
            selected_source: CaptureSource::SystemLoopback,
            selected_tx_device: None,
            selected_profile: LatencyProfile::Balanced,
            opus_bitrate_kbps: 128,
            active_sender: None,
            tx_started_at: None,
            bind_addr_input: format!("0.0.0.0:{DEFAULT_AUDIO_UDP_PORT}"),
            selected_rx_device: None,
            target_buffer_ms: 20,
            volume: 1.0,
            advertise_mdns: true,
            active_receiver: None,
            rx_started_at: None,
            duplex_role: DuplexRole::Headset,
            duplex_peer_ip_input: "127.0.0.1".to_string(),
            duplex_downlink_port: DEFAULT_AUDIO_UDP_PORT,
            duplex_uplink_port: DEFAULT_UPLINK_UDP_PORT,
            duplex_gate_db: -42.0,
            duplex_mic_muted: false,
            duplex_rx_channels: ChannelLayout::Stereo,
            active_duplex: None,
            duplex_started_at: None,
            tx_vu_meter: VuMeterState::default(),
            rx_vu_meter: VuMeterState::default(),
            duplex_tx_vu_meter: VuMeterState::default(),
            duplex_rx_vu_meter: VuMeterState::default(),
            tx_waveform_sparkline: SparklineBuffer::new(96),
            rx_buffer_sparkline: SparklineBuffer::new(64),
            duplex_buffer_sparkline: SparklineBuffer::new(64),
            duplex_tx_envelope_sparkline: SparklineBuffer::new(64),
            demo_mode: false,
            screenshot_target: None,
            screenshot_frames: 0,
            auto_follow_default_device: true,
            last_device_poll: Instant::now(),
            last_status_msg: lang.status_ready(),
            tray_window_state: TrayWindowState::Visible,
            pending_restore_focus: false,
            quit_requested: false,
            tray,
        };

        if enable_tray {
            app.trigger_lan_scan_internal(false);
        }

        Ok(app)
    }

    pub fn set_language(&mut self, lang: Language) {
        self.lang = lang;
        let hidden = self.tray_window_state != TrayWindowState::Visible;
        if let Some(ref tray) = self.tray {
            tray.set_language(lang, hidden);
        }
        self.last_status_msg = lang.status_language_switched();
    }

    pub fn enter_tray_background_mode(&mut self, ctx: &egui::Context) {
        if self.tray_window_state == TrayWindowState::HiddenInTray {
            return;
        }
        self.pending_restore_focus = false;
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
        #[cfg(target_os = "windows")]
        win32_tray::set_hidden_in_tray(true);
    }

    pub fn restore_from_tray(&mut self, ctx: &egui::Context) {
        self.tray_window_state = TrayWindowState::Visible;
        self.pending_restore_focus = true;
        apply_os_background_tray_mode(false);
        if let Some(ref tray) = self.tray {
            tray.toggle_window_item
                .set_text(self.lang.texts().tray_hide_console);
        }
        self.last_status_msg = self.lang.status_restored_from_tray();
        ctx.send_viewport_cmd(egui::ViewportCommand::Minimized(false));
        ctx.send_viewport_cmd(egui::ViewportCommand::MinInnerSize(Vec2::new(980.0, 660.0)));
        ctx.send_viewport_cmd(egui::ViewportCommand::InnerSize(Vec2::new(1220.0, 760.0)));
        ctx.send_viewport_cmd(egui::ViewportCommand::Visible(true));
        ctx.send_viewport_cmd(egui::ViewportCommand::Focus);
        ctx.request_repaint();
        #[cfg(target_os = "windows")]
        win32_tray::wake_main_window();
    }

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

    fn current_configured_rx_port(&self) -> u16 {
        parse_socket_addr_flexible(
            &self.bind_addr_input,
            IpAddr::from([0, 0, 0, 0]),
            DEFAULT_AUDIO_UDP_PORT,
        )
        .map(|sa| sa.port())
        .unwrap_or(DEFAULT_AUDIO_UDP_PORT)
    }

    fn sync_lan_advertiser(&mut self) {
        let port = self.current_configured_rx_port();
        if self.advertise_mdns {
            if let Some(ref svc) = self.lan_advertiser {
                svc.set_advertised_port(port);
            } else if let Ok(mut svc) = DiscoveryService::new() {
                let _ = svc.advertise("rsrelay-desktop", port);
                self.lan_advertiser = Some(svc);
            }
        } else {
            self.lan_advertiser = None;
        }
    }

    pub fn trigger_lan_scan(&mut self) {
        self.trigger_lan_scan_internal(true);
    }

    fn trigger_lan_scan_internal(&mut self, manual: bool) {
        if self.scanning_lan.load(Ordering::Relaxed) {
            return;
        }
        self.last_lan_scan = Instant::now();
        self.scanning_lan.store(true, Ordering::Relaxed);
        if manual {
            self.last_status_msg = self.lang.status_scanning_lan();
        }
        let peers_arc = Arc::clone(&self.discovered_peers);
        let scanning_flag = Arc::clone(&self.scanning_lan);

        thread::spawn(move || {
            let local_ips: Vec<IpAddr> = detect_all_local_ipv4s()
                .into_iter()
                .map(IpAddr::V4)
                .collect();
            if let Ok(svc) = DiscoveryService::new() {
                if let Ok(found) = svc.scan(Duration::from_millis(900)) {
                    let remote_peers: Vec<DiscoveredPeer> = found
                        .into_iter()
                        .filter_map(|mut p| {
                            p.addresses
                                .retain(|ip| !ip.is_loopback() && !local_ips.contains(ip));
                            if p.addresses.is_empty() {
                                None
                            } else {
                                Some(p)
                            }
                        })
                        .collect();
                    if let Ok(mut guard) = peers_arc.lock() {
                        *guard = remote_peers;
                    }
                }
            }
            scanning_flag.store(false, Ordering::Relaxed);
        });
    }

    fn apply_auto_discovered_target(&mut self) {
        if self.demo_mode
            || self.target_addr_manually_edited
            || self.active_sender.is_some()
            || self.active_duplex.is_some()
        {
            return;
        }
        let trimmed = self.target_addr_input.trim();
        let is_default_loopback = trimmed.is_empty()
            || trimmed.starts_with("127.0.0.1")
            || trimmed.starts_with("localhost");
        if !is_default_loopback {
            return;
        }

        let first_remote = self.discovered_peers.lock().ok().and_then(|guard| {
            guard.iter().find_map(|p| {
                let ip = p.addresses.first()?;
                Some((p.hostname.clone(), *ip, p.udp_port))
            })
        });

        if let Some((hostname, ip, port)) = first_remote {
            let addr_str = format!("{ip}:{port}");
            self.target_addr_input = addr_str.clone();
            self.duplex_peer_ip_input = ip.to_string();
            self.last_status_msg = self.lang.status_auto_discovered_peer(&hostname, &addr_str);
        }
    }

    fn build_target_combo_options(&self) -> Vec<(String, String, bool)> {
        let t = self.lang.texts();
        let mut opts: Vec<(String, String, bool)> = Vec::new();

        if let Ok(guard) = self.discovered_peers.lock() {
            for peer in guard.iter() {
                for ip in &peer.addresses {
                    if ip.is_ipv4() {
                        let addr_val = format!("{ip}:{}", peer.udp_port);
                        if !opts.iter().any(|(a, _, _)| a == &addr_val) {
                            let label = format!("{} · {addr_val} ({})", peer.hostname, peer.os);
                            opts.push((addr_val, label, true));
                        }
                    }
                }
            }
        }

        if let Some(lan_ip) = self.local_lan_ip {
            let rx_port = self.current_configured_rx_port();
            let addr_val = format!("{lan_ip}:{rx_port}");
            if !opts.iter().any(|(a, _, _)| a == &addr_val) {
                let label = format!("{} · {addr_val}", t.combo_local_lan);
                opts.push((addr_val, label, false));
            }
        }

        let lo_addr = format!("127.0.0.1:{DEFAULT_AUDIO_UDP_PORT}");
        if !opts.iter().any(|(a, _, _)| a == &lo_addr) {
            let label = format!("{} · {lo_addr}", t.combo_local_loopback);
            opts.push((lo_addr, label, false));
        }

        opts
    }

    pub fn stop_all_streams(&mut self) {
        self.active_sender = None;
        self.active_receiver = None;
        self.active_duplex = None;
        self.tx_started_at = None;
        self.rx_started_at = None;
        self.duplex_started_at = None;
        self.tx_waveform_sparkline.clear();
        self.rx_buffer_sparkline.clear();
        self.duplex_buffer_sparkline.clear();
        self.duplex_tx_envelope_sparkline.clear();
        self.last_status_msg = self.lang.status_all_stopped();
    }

    pub fn toggle_sender(&mut self) {
        if self.active_sender.is_some() {
            self.active_sender = None;
            self.tx_started_at = None;
            self.tx_waveform_sparkline.clear();
            self.last_status_msg = self.lang.status_sender_stopped();
            return;
        }

        self.active_duplex = None;
        self.duplex_started_at = None;

        let target_addr: SocketAddr = match parse_socket_addr_flexible(
            &self.target_addr_input,
            IpAddr::from([127, 0, 0, 1]),
            DEFAULT_AUDIO_UDP_PORT,
        ) {
            Ok(addr) => {
                self.target_addr_input = addr.to_string();
                addr
            }
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
                self.tx_started_at = Some(Instant::now());
                self.tx_waveform_sparkline.clear();
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
            self.rx_started_at = None;
            self.rx_buffer_sparkline.clear();
            self.last_status_msg = self.lang.status_receiver_stopped();
            return;
        }

        self.active_duplex = None;
        self.duplex_started_at = None;

        let bind_addr: SocketAddr = match parse_socket_addr_flexible(
            &self.bind_addr_input,
            IpAddr::from([0, 0, 0, 0]),
            DEFAULT_AUDIO_UDP_PORT,
        ) {
            Ok(addr) => {
                self.bind_addr_input = addr.to_string();
                addr
            }
            Err(e) => {
                self.last_status_msg = self.lang.status_invalid_bind_addr(&e);
                return;
            }
        };

        self.sync_lan_advertiser();

        let cfg = ReceiverSessionConfig {
            bind_addr,
            device_name: self.selected_rx_device.clone(),
            channels: ChannelLayout::Stereo,
            target_buffer_ms: self.target_buffer_ms,
            initial_volume: self.volume,
            // Background `lan_advertiser` already advertises this GUI instance when enabled
            advertise_mdns: self.advertise_mdns && self.lan_advertiser.is_none(),
            node_name: "rsrelay-desktop".to_string(),
        };

        match ReceiverSession::start(cfg) {
            Ok(session) => {
                let st = session.status();
                self.rx_started_at = Some(Instant::now());
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
            self.duplex_started_at = None;
            self.duplex_buffer_sparkline.clear();
            self.duplex_tx_envelope_sparkline.clear();
            self.last_status_msg = self.lang.status_duplex_stopped();
            return;
        }

        self.active_sender = None;
        self.active_receiver = None;
        self.tx_started_at = None;
        self.rx_started_at = None;

        let trimmed_peer = self.duplex_peer_ip_input.trim();
        let peer_ip_res: Result<IpAddr, std::net::AddrParseError> = trimmed_peer
            .parse::<IpAddr>()
            .or_else(|_| trimmed_peer.parse::<SocketAddr>().map(|sa| sa.ip()));

        let peer_ip: IpAddr = match peer_ip_res {
            Ok(ip) => {
                self.duplex_peer_ip_input = ip.to_string();
                ip
            }
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
        cfg.advertise_mdns = self.advertise_mdns && self.lan_advertiser.is_none();

        match DuplexSession::start(cfg) {
            Ok(session) => {
                session.set_tx_muted(self.duplex_mic_muted);
                let st = session.status();
                self.duplex_started_at = Some(Instant::now());
                self.duplex_buffer_sparkline.clear();
                self.duplex_tx_envelope_sparkline.clear();
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

    pub fn enable_demo_preview(&mut self) {
        self.demo_mode = true;
        self.local_lan_ip = "192.168.3.151".parse().ok();
        self.target_addr_input = "192.168.3.110:49153".to_string();
        self.duplex_peer_ip_input = "192.168.3.110".to_string();
        self.selected_profile = match self.active_tab {
            WorkspaceTab::DualConsole => LatencyProfile::LosslessPcm,
            _ => LatencyProfile::Balanced,
        };
        if let Ok(mut guard) = self.discovered_peers.lock() {
            *guard = vec![
                DiscoveredPeer {
                    instance_name: "Studio-MacBook".to_string(),
                    hostname: "Studio-MacBook".to_string(),
                    addresses: vec!["192.168.3.110".parse().unwrap()],
                    udp_port: 49153,
                    version: "0.4.2".to_string(),
                    os: "macos".to_string(),
                },
                DiscoveredPeer {
                    instance_name: "LivingRoom-PC".to_string(),
                    hostname: "LivingRoom-PC".to_string(),
                    addresses: vec!["192.168.3.128".parse().unwrap()],
                    udp_port: 49153,
                    version: "0.4.2".to_string(),
                    os: "windows".to_string(),
                },
                DiscoveredPeer {
                    instance_name: "Kevins-iPad".to_string(),
                    hostname: "Kevin’s iPad".to_string(),
                    addresses: vec!["192.168.3.142".parse().unwrap()],
                    udp_port: 49153,
                    version: "0.4.2".to_string(),
                    os: "ios".to_string(),
                },
            ];
        }
        self.tx_vu_meter.smoothed_db = -16.8;
        self.tx_vu_meter.peak_hold_db = -12.6;
        self.tx_vu_meter.smoothed_db_r = -19.8;
        self.tx_vu_meter.peak_hold_db_r = -14.4;

        self.duplex_rx_vu_meter.smoothed_db = -21.6;
        self.duplex_rx_vu_meter.peak_hold_db = -17.4;
        self.duplex_rx_vu_meter.smoothed_db_r = -23.4;
        self.duplex_rx_vu_meter.peak_hold_db_r = -18.6;

        self.duplex_tx_vu_meter.smoothed_db = -14.2;
        self.duplex_tx_vu_meter.peak_hold_db = -11.4;

        for i in 0..96 {
            let v = 0.32 + 0.28 * ((i as f32 * 0.23).sin()) + 0.16 * ((i as f32 * 0.61).cos());
            self.tx_waveform_sparkline.push(v.clamp(0.18, 0.88));
        }
        for i in 0..64 {
            let jb = 15.0 + 1.8 * ((i as f32 * 0.18).sin()) + 0.7 * ((i as f32 * 0.43).cos());
            self.duplex_buffer_sparkline.push(jb);
            let env = if (10..26).contains(&i) || (36..54).contains(&i) {
                -18.0 + 6.0 * ((i as f32 * 0.45).sin())
            } else {
                -54.0
            };
            self.duplex_tx_envelope_sparkline.push(env);
        }
    }

    fn update_live_telemetry(&mut self) {
        if self.demo_mode
            && self.active_sender.is_none()
            && self.active_receiver.is_none()
            && self.active_duplex.is_none()
        {
            return;
        }
        if let Some(ref tx) = self.active_sender {
            let st = tx.status();
            self.tx_vu_meter.update(st.peak_level, true);
            let norm = crate::widgets::dbfs_to_norm(self.tx_vu_meter.smoothed_db).max(0.18);
            self.tx_waveform_sparkline.push(norm);
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
            let env_db = if self.duplex_mic_muted {
                -58.0
            } else {
                linear_to_dbfs(st.tx.peak_level).max(-56.0)
            };
            self.duplex_tx_envelope_sparkline.push(env_db);
        } else {
            self.duplex_tx_vu_meter.update(0.0, false);
            self.duplex_rx_vu_meter.update(0.0, false);
        }
    }

    fn resolve_peer_name(&self, ip_part: &str) -> String {
        if let Ok(guard) = self.discovered_peers.lock() {
            for p in guard.iter() {
                if p.addresses.iter().any(|a| a.to_string() == ip_part) {
                    return p.hostname.clone();
                }
            }
        }
        if ip_part == "127.0.0.1" || ip_part == "localhost" {
            match self.lang {
                Language::Zh => "本机回环".to_string(),
                Language::En => "Localhost".to_string(),
            }
        } else {
            ip_part.to_string()
        }
    }

    // =========================================================================
    // Left Navigation Sidebar (`aside.side` in Frames C & C2)
    // =========================================================================

    fn render_sidebar(&mut self, ctx: &egui::Context) {
        let lang = self.lang;
        let t = lang.texts();

        egui::SidePanel::left("quiet_graphite_sidebar")
            .exact_width(metrics::SIDEBAR_WIDTH)
            .resizable(false)
            .frame(
                egui::Frame::none()
                    .fill(palette::BG_SIDEBAR())
                    .stroke(Stroke::new(1.0_f32, palette::LINE()))
                    .inner_margin(egui::Margin::ZERO),
            )
            .show(ctx, |ui| {
                // Bottom Footer Panel inside Sidebar (Local IP, Auto-follow toggle, Language seg, Tray button)
                egui::TopBottomPanel::bottom("sidebar_footer_panel")
                    .frame(
                        egui::Frame::none()
                            .fill(palette::BG_SIDEBAR())
                            .stroke(Stroke::new(1.0_f32, palette::LINE()))
                            .inner_margin(egui::Margin {
                                left: 20.0,
                                right: 20.0,
                                top: 14.0,
                                bottom: 14.0,
                            }),
                    )
                    .show_inside(ui, |ui| {
                        // 1. Local IP Row
                        let ip_str = self
                            .local_lan_ip
                            .map(|ip| ip.to_string())
                            .unwrap_or_else(|| "127.0.0.1".to_string());
                        ui.horizontal(|ui| {
                            ui.vertical(|ui| {
                                ui.spacing_mut().item_spacing.y = 2.0;
                                ui.label(
                                    RichText::new(t.sidebar_local_addr)
                                        .size(11.0)
                                        .color(palette::TEXT_MUTED()),
                                );
                                ui.label(
                                    RichText::new(&ip_str)
                                        .size(12.5)
                                        .family(FontFamily::Monospace)
                                        .color(palette::TEXT_PRIMARY()),
                                );
                            });
                            ui.with_layout(egui::Layout::right_to_left(Align::Center), |ui| {
                                let (copy_rect, copy_resp) =
                                    ui.allocate_exact_size(Vec2::splat(24.0), Sense::click());
                                if copy_resp.hovered() {
                                    ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
                                }
                                if ui.is_rect_visible(copy_rect) {
                                    let p = ui.painter();
                                    if copy_resp.hovered() {
                                        p.rect_filled(
                                            copy_rect,
                                            Rounding::same(6.0),
                                            palette::BG_RAISED(),
                                        );
                                    }
                                    draw_vector_icon(
                                        p,
                                        copy_rect.shrink(5.0),
                                        IconKind::Copy,
                                        if copy_resp.hovered() {
                                            palette::TEXT_PRIMARY()
                                        } else {
                                            palette::TEXT_MUTED()
                                        },
                                    );
                                }
                                if copy_resp.on_hover_text(t.lan_ip_hover).clicked() {
                                    ui.ctx().copy_text(ip_str.clone());
                                    if let Some(ip) = self.local_lan_ip {
                                        self.last_status_msg = lang.status_copied_lan_ip(ip);
                                    }
                                }
                            });
                        });

                        ui.add_space(10.0);

                        // 2. Auto-follow Default Device Toggle Row
                        ui.horizontal(|ui| {
                            ui.label(
                                RichText::new(t.chk_auto_follow_hotplug)
                                    .size(12.5)
                                    .color(palette::TEXT_SECONDARY()),
                            );
                            ui.with_layout(egui::Layout::right_to_left(Align::Center), |ui| {
                                pill_toggle(ui, &mut self.auto_follow_default_device);
                            });
                        });

                        ui.add_space(10.0);

                        // 3. Theme Mode 3-Way Segmented Control (`跟随系统 | 浅色 | 深色`)
                        {
                            let theme_seg_w = ui.available_width().max(150.0);
                            let (t_rect, _) = ui.allocate_exact_size(
                                Vec2::new(theme_seg_w, 28.0),
                                Sense::hover(),
                            );
                            if ui.is_rect_visible(t_rect) {
                                let p = ui.painter();
                                p.rect(
                                    t_rect,
                                    Rounding::same(8.0),
                                    palette::BG_CANVAS(),
                                    Stroke::new(1.0_f32, palette::LINE()),
                                );
                                let inner = t_rect.shrink(2.0);
                                let item_w = inner.width() / 3.0;
                                let modes = [
                                    (ThemeMode::System, t.theme_system),
                                    (ThemeMode::Light, t.theme_light),
                                    (ThemeMode::Dark, t.theme_dark),
                                ];
                                for (i, (m_val, m_label)) in modes.iter().enumerate() {
                                    let opt_rect = Rect::from_min_size(
                                        Pos2::new(inner.min.x + (i as f32) * item_w, inner.min.y),
                                        Vec2::new(item_w, inner.height()),
                                    );
                                    let opt_resp = ui.interact(
                                        opt_rect,
                                        ui.id().with(("theme_seg", i)),
                                        Sense::click(),
                                    );
                                    if opt_resp.hovered() {
                                        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
                                    }
                                    if opt_resp.clicked() && self.theme_mode != *m_val {
                                        self.theme_mode = *m_val;
                                        theme::sync_theme_mode(ctx, self.theme_mode);
                                        let sys_theme = match *m_val {
                                            ThemeMode::System => egui::SystemTheme::SystemDefault,
                                            ThemeMode::Light => egui::SystemTheme::Light,
                                            ThemeMode::Dark => egui::SystemTheme::Dark,
                                        };
                                        ctx.send_viewport_cmd(egui::ViewportCommand::SetTheme(
                                            sys_theme,
                                        ));
                                        self.last_status_msg = lang.status_theme_switched(m_label);
                                    }
                                    let selected = self.theme_mode == *m_val;
                                    if selected {
                                        p.rect(
                                            opt_rect,
                                            Rounding::same(6.0),
                                            palette::BG_HOVER(),
                                            Stroke::new(1.0_f32, palette::INSET_HIGHLIGHT()),
                                        );
                                    }
                                    p.text(
                                        opt_rect.center(),
                                        egui::Align2::CENTER_CENTER,
                                        *m_label,
                                        FontId::new(11.5, FontFamily::Proportional),
                                        if selected {
                                            palette::TEXT_PRIMARY()
                                        } else {
                                            palette::TEXT_SECONDARY()
                                        },
                                    );
                                }
                            }
                        }

                        ui.add_space(8.0);

                        // 4. Language Segmented Control (`中文 | EN`) + Minimize to Tray Button
                        ui.horizontal(|ui| {
                            ui.spacing_mut().item_spacing.x = 8.0;
                            let seg_w = (ui.available_width() - 40.0).max(110.0);
                            let (seg_rect, _) =
                                ui.allocate_exact_size(Vec2::new(seg_w, 30.0), Sense::hover());
                            if ui.is_rect_visible(seg_rect) {
                                let p = ui.painter();
                                p.rect(
                                    seg_rect,
                                    Rounding::same(8.0),
                                    palette::BG_CANVAS(),
                                    Stroke::new(1.0_f32, palette::LINE()),
                                );
                                let inner = seg_rect.shrink(2.0);
                                let half_w = inner.width() * 0.5;
                                let langs = [(Language::Zh, "中文"), (Language::En, "EN")];
                                for (i, (l_val, l_str)) in langs.iter().enumerate() {
                                    let opt_rect = Rect::from_min_size(
                                        Pos2::new(inner.min.x + (i as f32) * half_w, inner.min.y),
                                        Vec2::new(half_w, inner.height()),
                                    );
                                    let opt_resp = ui.interact(
                                        opt_rect,
                                        ui.id().with(("lang_seg", i)),
                                        Sense::click(),
                                    );
                                    if opt_resp.hovered() {
                                        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
                                    }
                                    if opt_resp.clicked() && self.lang != *l_val {
                                        self.set_language(*l_val);
                                    }
                                    let selected = self.lang == *l_val;
                                    if selected {
                                        p.rect(
                                            opt_rect,
                                            Rounding::same(6.0),
                                            palette::BG_HOVER(),
                                            Stroke::new(1.0_f32, palette::INSET_HIGHLIGHT()),
                                        );
                                    }
                                    p.text(
                                        opt_rect.center(),
                                        egui::Align2::CENTER_CENTER,
                                        *l_str,
                                        FontId::new(12.0, FontFamily::Proportional),
                                        if selected {
                                            palette::TEXT_PRIMARY()
                                        } else {
                                            palette::TEXT_SECONDARY()
                                        },
                                    );
                                }
                            }

                            // Minimize to Tray icon button (`32x30`)
                            let (tray_rect, tray_resp) =
                                ui.allocate_exact_size(Vec2::new(32.0, 30.0), Sense::click());
                            if tray_resp.hovered() {
                                ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
                            }
                            if ui.is_rect_visible(tray_rect) {
                                let p = ui.painter();
                                p.rect(
                                    tray_rect,
                                    Rounding::same(8.0),
                                    if tray_resp.hovered() {
                                        palette::BG_RAISED()
                                    } else {
                                        Color32::TRANSPARENT
                                    },
                                    Stroke::new(1.0_f32, palette::LINE_STRONG()),
                                );
                                draw_vector_icon(
                                    p,
                                    tray_rect.shrink(8.5),
                                    IconKind::MinimizeTray,
                                    if tray_resp.hovered() {
                                        palette::TEXT_PRIMARY()
                                    } else {
                                        palette::TEXT_SECONDARY()
                                    },
                                );
                            }
                            if tray_resp
                                .on_hover_text(t.btn_minimize_tray_hover)
                                .clicked()
                            {
                                self.enter_tray_background_mode(ctx);
                            }
                        });
                    });

                // Upper Sidebar Content (Brand, Mode Nav, Nearby Devices)
                egui::CentralPanel::default()
                    .frame(
                        egui::Frame::none()
                            .fill(palette::BG_SIDEBAR())
                            .inner_margin(egui::Margin {
                                left: 14.0,
                                right: 14.0,
                                top: 18.0,
                                bottom: 12.0,
                            }),
                    )
                    .show_inside(ui, |ui| {
                        // Brand Block
                        ui.horizontal(|ui| {
                            ui.add_space(6.0);
                            let (mark_rect, _) =
                                ui.allocate_exact_size(Vec2::splat(28.0), Sense::hover());
                            if ui.is_rect_visible(mark_rect) {
                                let p = ui.painter();
                                p.rect(
                                    mark_rect,
                                    Rounding::same(8.0),
                                    palette::CHIP_IDLE_BG(),
                                    Stroke::new(1.0_f32, palette::LINE_STRONG()),
                                );
                                draw_vector_icon(
                                    p,
                                    mark_rect.shrink(6.0),
                                    IconKind::BrandWaveform,
                                    palette::ACCENT(),
                                );
                            }
                            ui.add_space(2.0);
                            ui.vertical(|ui| {
                                ui.spacing_mut().item_spacing.y = 0.0;
                                ui.label(
                                    RichText::new("rsrelay")
                                        .size(15.0)
                                        .color(palette::TEXT_PRIMARY())
                                        .strong(),
                                );
                                ui.label(
                                    RichText::new(concat!("v", env!("CARGO_PKG_VERSION")))
                                        .size(11.0)
                                        .family(FontFamily::Monospace)
                                        .color(palette::TEXT_MUTED()),
                                );
                            });
                        });

                        ui.add_space(20.0);

                        // Section 1: 工作模式 (Mode Navigation)
                        ui.horizontal(|ui| {
                            ui.add_space(8.0);
                            ui.label(
                                RichText::new(t.nav_section_mode)
                                    .size(11.0)
                                    .color(palette::TEXT_MUTED()),
                            );
                        });
                        ui.add_space(4.0);

                        let dual_live = self.active_sender.is_some()
                            || self.active_receiver.is_some()
                            || (self.demo_mode && self.active_tab == WorkspaceTab::DualConsole);
                        let duplex_live = self.active_duplex.is_some()
                            || (self.demo_mode && self.active_tab == WorkspaceTab::FullDuplex);
                        let nav_items = [
                            (
                                WorkspaceTab::DualConsole,
                                IconKind::SendReceive,
                                t.tab_dual_console,
                                dual_live,
                            ),
                            (
                                WorkspaceTab::FullDuplex,
                                IconKind::Headset,
                                t.tab_full_duplex,
                                duplex_live,
                            ),
                            (
                                WorkspaceTab::HardwareMatrix,
                                IconKind::SpeakerDevice,
                                t.tab_hardware_matrix,
                                false,
                            ),
                        ];

                        for (tab, icon, label, is_live) in nav_items {
                            let selected = self.active_tab == tab;
                            let (nav_rect, nav_resp) = ui.allocate_exact_size(
                                Vec2::new(ui.available_width(), 34.0),
                                Sense::click(),
                            );
                            if nav_resp.hovered() {
                                ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
                            }
                            if nav_resp.clicked() {
                                self.active_tab = tab;
                            }
                            if ui.is_rect_visible(nav_rect) {
                                let p = ui.painter();
                                if selected {
                                    p.rect(
                                        nav_rect,
                                        Rounding::same(8.0),
                                        palette::BG_RAISED(),
                                        Stroke::new(1.0_f32, palette::INSET_HIGHLIGHT()),
                                    );
                                } else if nav_resp.hovered() {
                                    p.rect_filled(
                                        nav_rect,
                                        Rounding::same(8.0),
                                        palette::SIDEBAR_HOVER_BG(),
                                    );
                                }
                                let fg = if selected || nav_resp.hovered() {
                                    palette::TEXT_PRIMARY()
                                } else {
                                    palette::TEXT_SECONDARY()
                                };
                                let icon_rect = Rect::from_center_size(
                                    Pos2::new(nav_rect.min.x + 18.0, nav_rect.center().y),
                                    Vec2::splat(16.0),
                                );
                                draw_vector_icon(p, icon_rect, icon, fg);
                                p.text(
                                    Pos2::new(nav_rect.min.x + 36.0, nav_rect.center().y),
                                    egui::Align2::LEFT_CENTER,
                                    label,
                                    FontId::new(13.0, FontFamily::Proportional),
                                    fg,
                                );
                                if is_live {
                                    let dot_pos =
                                        Pos2::new(nav_rect.max.x - 13.0, nav_rect.center().y);
                                    p.circle_filled(dot_pos, 6.0, palette::ACCENT_TINT());
                                    p.circle_filled(dot_pos, 3.0, palette::ACCENT());
                                }
                            }
                            ui.add_space(2.0);
                        }

                        ui.add_space(22.0);

                        // Section 2: 附近设备 (Nearby LAN Peers)
                        ui.horizontal(|ui| {
                            ui.add_space(8.0);
                            ui.label(
                                RichText::new(t.nav_section_nearby)
                                    .size(11.0)
                                    .color(palette::TEXT_MUTED()),
                            );
                            ui.with_layout(egui::Layout::right_to_left(Align::Center), |ui| {
                                let (ref_rect, ref_resp) =
                                    ui.allocate_exact_size(Vec2::splat(24.0), Sense::click());
                                if ref_resp.hovered() {
                                    ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
                                }
                                if ui.is_rect_visible(ref_rect) {
                                    let p = ui.painter();
                                    if ref_resp.hovered() {
                                        p.rect_filled(
                                            ref_rect,
                                            Rounding::same(6.0),
                                            palette::BG_RAISED(),
                                        );
                                    }
                                    let col = if self.scanning_lan.load(Ordering::Relaxed) {
                                        palette::ACCENT()
                                    } else if ref_resp.hovered() {
                                        palette::TEXT_PRIMARY()
                                    } else {
                                        palette::TEXT_MUTED()
                                    };
                                    draw_vector_icon(
                                        p,
                                        ref_rect.shrink(5.0),
                                        IconKind::Refresh,
                                        col,
                                    );
                                }
                                if ref_resp.on_hover_text(t.btn_scan_lan_hover).clicked() {
                                    self.trigger_lan_scan();
                                }
                            });
                        });
                        ui.add_space(4.0);

                        self.render_sidebar_peers(ui);
                    });
            });
    }

    fn render_sidebar_peers(&mut self, ui: &mut egui::Ui) {
        let peers_snapshot = self
            .discovered_peers
            .lock()
            .ok()
            .map(|g| g.clone())
            .unwrap_or_default();

        let (current_target_ip, _) =
            split_host_port(&self.target_addr_input, DEFAULT_AUDIO_UDP_PORT);

        if peers_snapshot.is_empty() {
            // Render the active target endpoint as a clean peer card so the sidebar always looks structured
            let peer_name = self.resolve_peer_name(&current_target_ip);
            let _ = self.draw_sidebar_peer_row(
                ui,
                &peer_name,
                &current_target_ip,
                DEFAULT_AUDIO_UDP_PORT,
                IconKind::Laptop,
                true,
            );
            if let Some(lan_ip) = self.local_lan_ip {
                let lan_str = lan_ip.to_string();
                if lan_str != current_target_ip {
                    let local_label = match self.lang {
                        Language::Zh => "本机节点 (LAN)",
                        Language::En => "This Machine (LAN)",
                    };
                    if self.draw_sidebar_peer_row(
                        ui,
                        local_label,
                        &lan_str,
                        DEFAULT_AUDIO_UDP_PORT,
                        IconKind::Desktop,
                        false,
                    ) {
                        self.target_addr_input = format!("{lan_str}:{DEFAULT_AUDIO_UDP_PORT}");
                        self.duplex_peer_ip_input = lan_str;
                        self.target_addr_manually_edited = true;
                    }
                }
            }
            return;
        }

        for peer in &peers_snapshot {
            if let Some(ip) = peer.addresses.first() {
                let ip_str = ip.to_string();
                let addr_str = format!("{ip_str}:{}", peer.udp_port);
                let is_selected = self.target_addr_input == addr_str
                    || self.duplex_peer_ip_input == ip_str
                    || current_target_ip == ip_str;
                let os_lower = peer.os.to_ascii_lowercase();
                let icon = if os_lower.contains("mac") || os_lower.contains("darwin") {
                    IconKind::Laptop
                } else if os_lower.contains("ios") || os_lower.contains("android") {
                    IconKind::Mobile
                } else {
                    IconKind::Desktop
                };
                if self.draw_sidebar_peer_row(
                    ui,
                    &peer.hostname,
                    &ip_str,
                    peer.udp_port,
                    icon,
                    is_selected,
                ) {
                    self.target_addr_input = addr_str.clone();
                    self.duplex_peer_ip_input = ip_str;
                    self.target_addr_manually_edited = true;
                    self.last_status_msg =
                        self.lang.status_selected_lan_peer(&peer.hostname, &addr_str);
                }
            }
        }
    }

    fn draw_sidebar_peer_row(
        &self,
        ui: &mut egui::Ui,
        name: &str,
        ip_str: &str,
        _port: u16,
        icon: IconKind,
        is_current: bool,
    ) -> bool {
        let (rect, resp) =
            ui.allocate_exact_size(Vec2::new(ui.available_width(), 44.0), Sense::click());
        if resp.hovered() {
            ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
        }
        if ui.is_rect_visible(rect) {
            let p = ui.painter();
            if is_current {
                p.rect_filled(rect, Rounding::same(8.0), palette::PEER_ACTIVE_BG());
            } else if resp.hovered() {
                p.rect_filled(
                    rect,
                    Rounding::same(8.0),
                    palette::SIDEBAR_HOVER_BG(),
                );
            }

            let os_rect = Rect::from_center_size(
                Pos2::new(rect.min.x + 23.0, rect.center().y),
                Vec2::splat(26.0),
            );
            p.rect_filled(os_rect, Rounding::same(7.0), palette::BG_RAISED());
            draw_vector_icon(p, os_rect.shrink(6.0), icon, palette::TEXT_SECONDARY());

            p.text(
                Pos2::new(rect.min.x + 46.0, rect.center().y - 7.5),
                egui::Align2::LEFT_CENTER,
                name,
                FontId::new(12.5, FontFamily::Proportional),
                palette::TEXT_PRIMARY(),
            );
            p.text(
                Pos2::new(rect.min.x + 46.0, rect.center().y + 8.5),
                egui::Align2::LEFT_CENTER,
                ip_str,
                FontId::new(11.0, FontFamily::Monospace),
                palette::TEXT_MUTED(),
            );

            let dot_col = if is_current {
                palette::ACCENT()
            } else {
                palette::DOT_OFFLINE()
            };
            p.circle_filled(
                Pos2::new(rect.max.x - 13.0, rect.center().y),
                3.0,
                dot_col,
            );
        }
        ui.add_space(2.0);
        resp.on_hover_text(self.lang.texts().discovered_peer_hover)
            .clicked()
    }

    // =========================================================================
    // Frame C: 收发 (`DualConsole` Tab)
    // =========================================================================

    fn render_dual_console_tab(&mut self, ui: &mut egui::Ui) {
        let lang = self.lang;
        let t = lang.texts();

        // Top Page Header (`.top`)
        ui.horizontal(|ui| {
            ui.vertical(|ui| {
                ui.spacing_mut().item_spacing.y = 3.0;
                ui.label(
                    RichText::new(t.page_c_title)
                        .size(20.0)
                        .color(palette::TEXT_PRIMARY())
                        .strong(),
                );
                ui.label(
                    RichText::new(t.page_c_subtitle)
                        .size(13.0)
                        .color(palette::TEXT_SECONDARY()),
                );
            });
            ui.with_layout(egui::Layout::right_to_left(Align::Max), |ui| {
                if ghost_stop_all_button(ui, t.btn_stop_all) {
                    self.stop_all_streams();
                }
            });
        });

        ui.add_space(18.0);

        ui.columns(2, |columns| {
            // =================================================================
            // LEFT CARD: 发送 (Transmitter)
            // =================================================================
            let tx_running = self.active_sender.is_some() || self.demo_mode;
            theme::studio_card_frame(tx_running, palette::ACCENT()).show(&mut columns[0], |ui| {
                // 1. Card Header (`.ch`)
                egui::Frame::none()
                    .inner_margin(egui::Margin {
                        left: 20.0,
                        right: 20.0,
                        top: 18.0,
                        bottom: 0.0,
                    })
                    .show(ui, |ui| {
                        ui.horizontal(|ui| {
                            ui.spacing_mut().item_spacing.x = 12.0;
                            draw_card_icon_tile(ui, IconKind::ArrowUpRight);
                            ui.label(
                                RichText::new(t.tx_card_title)
                                    .size(15.0)
                                    .color(palette::TEXT_PRIMARY())
                                    .strong(),
                            );
                            if tx_running {
                                let elapsed = if self.demo_mode && self.active_sender.is_none() {
                                    "00:42:18".to_string()
                                } else {
                                    format_elapsed_hms(self.tx_started_at)
                                };
                                status_chip(ui, t.status_sending, Some(&elapsed), true);
                            } else {
                                status_chip(ui, t.status_standby, None, false);
                            }

                            ui.with_layout(egui::Layout::right_to_left(Align::Center), |ui| {
                                if header_action_button(
                                    ui,
                                    t.btn_start_tx,
                                    t.btn_stop_tx,
                                    tx_running,
                                ) {
                                    self.toggle_sender();
                                }
                            });
                        });
                    });

                // 2. Hero Address & Route Block (`.hero`)
                let (tx_host, tx_port) =
                    split_host_port(&self.target_addr_input, DEFAULT_AUDIO_UDP_PORT);
                let peer_name = self.resolve_peer_name(&tx_host);
                let source_label = match self.selected_source {
                    CaptureSource::SystemLoopback => t.source_system_loopback,
                    CaptureSource::Microphone => t.source_microphone,
                    CaptureSource::SyntheticTone => t.source_synthetic_tone,
                };
                let tx_dev_label = self
                    .active_sender
                    .as_ref()
                    .map(|s| s.status().device_name)
                    .or_else(|| self.selected_tx_device.clone())
                    .unwrap_or_else(|| t.device_system_default.to_string());
                let codec_spec = match self.selected_profile {
                    LatencyProfile::LosslessPcm => "PCM16 · 48 kHz · 5 ms",
                    LatencyProfile::Stable => "Opus · 48 kHz · 10 ms",
                    _ => "Opus · 48 kHz · 5 ms",
                };

                egui::Frame::none()
                    .inner_margin(egui::Margin {
                        left: 20.0,
                        right: 20.0,
                        top: 22.0,
                        bottom: 18.0,
                    })
                    .show(ui, |ui| {
                        ui.vertical(|ui| {
                            ui.spacing_mut().item_spacing.y = 6.0;
                            ui.label(
                                RichText::new(format!("{} · {peer_name}", t.hero_send_to))
                                    .size(11.5)
                                    .color(palette::TEXT_MUTED()),
                            );
                            ui.horizontal(|ui| {
                                ui.spacing_mut().item_spacing.x = 0.0;
                                ui.label(
                                    RichText::new(&tx_host)
                                        .size(29.0)
                                        .family(FontFamily::Monospace)
                                        .color(if tx_running {
                                            palette::TEXT_PRIMARY()
                                        } else {
                                            palette::TEXT_SECONDARY()
                                        }),
                                );
                                ui.label(
                                    RichText::new(&tx_port)
                                        .size(29.0)
                                        .family(FontFamily::Monospace)
                                        .color(palette::TEXT_MUTED()),
                                );
                            });
                            ui.horizontal_wrapped(|ui| {
                                ui.spacing_mut().item_spacing.x = 6.0;
                                ui.label(
                                    RichText::new(source_label)
                                        .size(12.0)
                                        .color(palette::TEXT_SECONDARY()),
                                );
                                ui.label(RichText::new("→").size(12.0).color(palette::TEXT_MUTED()));
                                ui.label(
                                    RichText::new(&tx_dev_label)
                                        .size(12.0)
                                        .color(palette::TEXT_SECONDARY()),
                                );
                                ui.label(RichText::new("·").size(12.0).color(palette::TEXT_MUTED()));
                                ui.label(
                                    RichText::new(codec_spec)
                                        .size(12.0)
                                        .family(FontFamily::Monospace)
                                        .color(palette::TEXT_SECONDARY()),
                                );
                            });
                        });
                    });

                // 3. Stereo Segmented VU Meter (`.meter`)
                egui::Frame::none()
                    .inner_margin(egui::Margin {
                        left: 20.0,
                        right: 20.0,
                        top: 0.0,
                        bottom: 16.0,
                    })
                    .show(ui, |ui| {
                        draw_stereo_vu_meter(ui, &self.tx_vu_meter, tx_running, false);
                    });

                // 4. 3-Column Hairline Stats Row (`.stats`)
                let bitrate_str = if self.selected_profile == LatencyProfile::LosslessPcm {
                    "1,536".to_string()
                } else {
                    format!("{}", self.opus_bitrate_kbps)
                };
                let (rtt_val, pkts_val) = if let Some(ref sender) = self.active_sender {
                    let st = sender.status();
                    let rtt = if st.control.connected {
                        format!("{:.2}", (st.control.rtt_us as f64) / 1000.0)
                    } else {
                        "0.45".to_string()
                    };
                    (rtt, format_with_commas(st.net.packets_sent))
                } else if self.demo_mode {
                    ("0.45".to_string(), "863,848".to_string())
                } else {
                    ("—".to_string(), "0".to_string())
                };

                draw_stats_row_3(
                    ui,
                    [
                        (t.stat_bitrate, &bitrate_str, "kbps"),
                        (t.stat_rtt, &rtt_val, "ms"),
                        (t.stat_packets_sent, &pkts_val, t.unit_packets),
                    ],
                    tx_running,
                );

                // 5. 20s Waveform Chart (`.chart`)
                let pkt_rate = match self.selected_profile.default_frame_duration() {
                    rsrelay_core::FrameDuration::Ms5 => "200 pkt/s · 节拍抖动 ±0.08 ms",
                    _ => "100 pkt/s · 节拍抖动 ±0.10 ms",
                };
                draw_tx_waveform_chart(
                    ui,
                    t.chart_last_20s,
                    pkt_rate,
                    t.chart_tx_idle_hint,
                    &self.tx_waveform_sparkline,
                    tx_running,
                );

                // 6. Settings List (`.set`, 4 hairline rows)
                let src_opts = vec![
                    (
                        CaptureSource::SystemLoopback,
                        t.source_system_loopback.to_string(),
                    ),
                    (CaptureSource::Microphone, t.source_microphone.to_string()),
                    (
                        CaptureSource::SyntheticTone,
                        t.source_synthetic_tone.to_string(),
                    ),
                ];
                settings_row(ui, t.label_audio_source, true, |ui| {
                    quiet_select_pill(
                        ui,
                        "c_source_combo",
                        &mut self.selected_source,
                        source_label,
                        &src_opts,
                    );

                    if cfg!(target_os = "macos")
                        && self.selected_source == CaptureSource::SystemLoopback
                        && !self.sck_permission_granted
                        && ui
                            .button(
                                RichText::new(t.btn_grant_perm)
                                    .size(11.5)
                                    .color(palette::STATUS_WARN()),
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
                });

                let mut tx_dev_opts = vec![(None, t.device_system_default.to_string())];
                for dev in &self.devices {
                    let eligible = dev.is_input
                        || (cfg!(target_os = "windows")
                            && self.selected_source == CaptureSource::SystemLoopback
                            && dev.is_output);
                    if eligible {
                        tx_dev_opts.push((Some(dev.name.clone()), dev.name.clone()));
                    }
                }
                let current_tx_label = self
                    .selected_tx_device
                    .clone()
                    .unwrap_or_else(|| t.device_system_default.to_string());
                settings_row(ui, t.label_input_device, true, |ui| {
                    if quiet_select_pill(
                        ui,
                        "c_tx_device_combo",
                        &mut self.selected_tx_device,
                        &current_tx_label,
                        &tx_dev_opts,
                    ) && self.active_sender.is_some()
                    {
                        self.active_sender = None;
                        self.toggle_sender();
                    }
                });

                let target_combo_opts = self.build_target_combo_options();
                let mut combo_outcome = crate::widgets::ComboInputOutcome::default();
                settings_row(ui, t.label_target_addr, true, |ui| {
                    combo_outcome = quiet_mono_combo_pill(
                        ui,
                        "c_target_addr_input",
                        &mut self.target_addr_input,
                        &target_combo_opts,
                        t.combo_scan_lan,
                        t.combo_no_lan_peers,
                    );
                });
                if combo_outcome.edited_manually {
                    self.target_addr_manually_edited = true;
                    let (host, _) =
                        split_host_port(&self.target_addr_input, DEFAULT_AUDIO_UDP_PORT);
                    if !host.is_empty() && host != "—" {
                        self.duplex_peer_ip_input = host;
                    }
                }
                if let Some(idx) = combo_outcome.selected_option_idx {
                    self.target_addr_manually_edited = true;
                    if let Some((addr_val, label, _)) = target_combo_opts.get(idx) {
                        let (host, _) = split_host_port(addr_val, DEFAULT_AUDIO_UDP_PORT);
                        self.duplex_peer_ip_input = host;
                        let peer_name = label.split(" · ").next().unwrap_or(addr_val);
                        self.last_status_msg =
                            lang.status_selected_lan_peer(peer_name, addr_val);
                    }
                }
                if combo_outcome.request_rescan {
                    self.trigger_lan_scan();
                }

                settings_row(ui, t.label_quality_latency, false, |ui| {
                    latency_profile_selector(ui, &mut self.selected_profile, lang);
                });
            });

            // =================================================================
            // RIGHT CARD: 接收 (Receiver)
            // =================================================================
            let rx_running = self.active_receiver.is_some();
            let rx_source_addr = self
                .active_receiver
                .as_ref()
                .and_then(|r| r.status().net.source_addr);
            theme::studio_card_frame(rx_running, palette::ACCENT()).show(&mut columns[1], |ui| {
                // 1. Card Header (`.ch`)
                egui::Frame::none()
                    .inner_margin(egui::Margin {
                        left: 20.0,
                        right: 20.0,
                        top: 18.0,
                        bottom: 0.0,
                    })
                    .show(ui, |ui| {
                        ui.horizontal(|ui| {
                            ui.spacing_mut().item_spacing.x = 12.0;
                            draw_card_icon_tile(ui, IconKind::ArrowDownLeft);
                            ui.label(
                                RichText::new(t.rx_card_title)
                                    .size(15.0)
                                    .color(palette::TEXT_PRIMARY())
                                    .strong(),
                            );
                            if rx_running {
                                let elapsed = format_elapsed_hms(self.rx_started_at);
                                status_chip(ui, t.status_receiving, Some(&elapsed), true);
                            } else {
                                status_chip(ui, t.status_standby, None, false);
                            }

                            ui.with_layout(egui::Layout::right_to_left(Align::Center), |ui| {
                                if header_action_button(
                                    ui,
                                    t.btn_start_rx,
                                    t.btn_stop_rx,
                                    rx_running,
                                ) {
                                    self.toggle_receiver();
                                }
                            });
                        });
                    });

                // 2. Hero Block (`.hero`)
                let (rx_host, rx_port) =
                    split_host_port(&self.bind_addr_input, DEFAULT_AUDIO_UDP_PORT);
                let rx_dev_label = self
                    .active_receiver
                    .as_ref()
                    .map(|r| r.status().device_name)
                    .or_else(|| self.selected_rx_device.clone())
                    .unwrap_or_else(|| t.device_default_speaker.to_string());

                let (hero_caption, hero_caption_active, hero_host, hero_port) =
                    if let Some(src_addr) = rx_source_addr {
                        let src_ip_str = src_addr.ip().to_string();
                        let src_peer_name = self.resolve_peer_name(&src_ip_str);
                        (
                            format!(
                                "{} · {src_peer_name} ({rx_host}{rx_port})",
                                t.hero_receiving_from
                            ),
                            true,
                            src_ip_str,
                            format!(":{}", src_addr.port()),
                        )
                    } else if rx_running {
                        (
                            t.hero_listen_waiting.to_string(),
                            false,
                            rx_host.clone(),
                            rx_port.clone(),
                        )
                    } else {
                        (
                            t.hero_listen_any.to_string(),
                            false,
                            rx_host.clone(),
                            rx_port.clone(),
                        )
                    };

                egui::Frame::none()
                    .inner_margin(egui::Margin {
                        left: 20.0,
                        right: 20.0,
                        top: 22.0,
                        bottom: 18.0,
                    })
                    .show(ui, |ui| {
                        ui.vertical(|ui| {
                            ui.spacing_mut().item_spacing.y = 6.0;
                            ui.label(
                                RichText::new(&hero_caption)
                                    .size(11.5)
                                    .color(if hero_caption_active {
                                        palette::ACCENT()
                                    } else {
                                        palette::TEXT_MUTED()
                                    }),
                            );
                            ui.horizontal(|ui| {
                                ui.spacing_mut().item_spacing.x = 0.0;
                                ui.label(
                                    RichText::new(&hero_host)
                                        .size(29.0)
                                        .family(FontFamily::Monospace)
                                        .color(if rx_running {
                                            palette::TEXT_PRIMARY()
                                        } else {
                                            palette::TEXT_SECONDARY()
                                        }),
                                );
                                ui.label(
                                    RichText::new(&hero_port)
                                        .size(29.0)
                                        .family(FontFamily::Monospace)
                                        .color(palette::TEXT_MUTED()),
                                );
                            });
                            ui.horizontal_wrapped(|ui| {
                                ui.spacing_mut().item_spacing.x = 6.0;
                                ui.label(
                                    RichText::new(t.route_adaptive_jb)
                                        .size(12.0)
                                        .color(palette::TEXT_SECONDARY()),
                                );
                                ui.label(RichText::new("→").size(12.0).color(palette::TEXT_MUTED()));
                                ui.label(
                                    RichText::new(&rx_dev_label)
                                        .size(12.0)
                                        .color(palette::TEXT_SECONDARY()),
                                );
                                ui.label(RichText::new("·").size(12.0).color(palette::TEXT_MUTED()));
                                ui.label(
                                    RichText::new(format!("48 kHz · {}", t.channel_stereo))
                                        .size(12.0)
                                        .family(FontFamily::Monospace)
                                        .color(palette::TEXT_SECONDARY()),
                                );
                            });
                        });
                    });

                // 3. Stereo Segmented VU Meter (`.meter`)
                egui::Frame::none()
                    .inner_margin(egui::Margin {
                        left: 20.0,
                        right: 20.0,
                        top: 0.0,
                        bottom: 16.0,
                    })
                    .show(ui, |ui| {
                        draw_stereo_vu_meter(ui, &self.rx_vu_meter, rx_running, false);
                    });

                // 4. 3-Column Hairline Stats Row (`.stats`)
                let (buf_val, asrc_val, lost_val, repaired_unit) =
                    if let Some(ref receiver) = self.active_receiver {
                        let st = receiver.status();
                        let repaired = st.net.fec_recovered + st.net.plc_frames;
                        (
                            format!("{:.1}", st.net.buffer_level_ms),
                            format!("{:.5}", st.net.resample_ratio),
                            format!("{}", st.net.packets_lost),
                            format!("/ {repaired}"),
                        )
                    } else {
                        (
                            "—".to_string(),
                            "1.00000".to_string(),
                            "0".to_string(),
                            "/ 0".to_string(),
                        )
                    };
                let buf_unit = format!("/ {} ms", self.target_buffer_ms);

                draw_stats_row_3(
                    ui,
                    [
                        (t.stat_buffer_level, &buf_val, &buf_unit),
                        (t.stat_clock_comp, &asrc_val, "×"),
                        (t.stat_loss_repaired, &lost_val, &repaired_unit),
                    ],
                    rx_running,
                );

                // 5. Jitter Buffer Sparkline Chart (`.chart`)
                let buf_summary = lang.format_buffer_summary(None, self.target_buffer_ms);
                draw_sparkline(
                    ui,
                    t.chart_jitter_buffer,
                    &buf_summary,
                    t.chart_rx_idle_hint,
                    &self.rx_buffer_sparkline,
                    0.0,
                    (self.target_buffer_ms as f32 * 2.0).max(36.0),
                    Some(self.target_buffer_ms as f32),
                    palette::ACCENT(),
                );

                // 6. Settings List (`.set`)
                let (source_pill_text, source_pill_active) = if let Some(src_addr) = rx_source_addr {
                    let src_ip_str = src_addr.ip().to_string();
                    let src_peer_name = self.resolve_peer_name(&src_ip_str);
                    (format!("{src_peer_name} · {src_addr}"), true)
                } else {
                    (t.source_waiting_any.to_string(), false)
                };
                let mut clicked_source_pill = false;
                settings_row(ui, t.label_source_addr, true, |ui| {
                    if quiet_source_addr_pill(
                        ui,
                        &source_pill_text,
                        source_pill_active,
                        Some(t.source_addr_hover),
                    ) {
                        clicked_source_pill = true;
                    }
                });
                if clicked_source_pill {
                    if let Some(src_addr) = rx_source_addr {
                        let src_ip = src_addr.ip().to_string();
                        ui.ctx().copy_text(src_addr.to_string());
                        let peer_name = self.resolve_peer_name(&src_ip);
                        let target_str = format!("{src_ip}:{DEFAULT_AUDIO_UDP_PORT}");
                        self.target_addr_input = target_str.clone();
                        self.duplex_peer_ip_input = src_ip;
                        self.target_addr_manually_edited = true;
                        self.last_status_msg =
                            lang.status_selected_lan_peer(&peer_name, &target_str);
                    }
                }

                let prev_bind = self.bind_addr_input.clone();
                settings_row(ui, t.label_listen_port, true, |ui| {
                    quiet_mono_input_pill(ui, "c_bind_addr_input", &mut self.bind_addr_input);
                });
                if self.bind_addr_input != prev_bind {
                    self.sync_lan_advertiser();
                }

                let mut rx_dev_opts = vec![(None, t.device_default_speaker.to_string())];
                for dev in &self.devices {
                    if dev.is_output {
                        rx_dev_opts.push((Some(dev.name.clone()), dev.name.clone()));
                    }
                }
                let current_rx_label = self
                    .selected_rx_device
                    .clone()
                    .unwrap_or_else(|| t.device_default_speaker.to_string());
                settings_row(ui, t.label_output_device, true, |ui| {
                    if quiet_select_pill(
                        ui,
                        "c_rx_device_combo",
                        &mut self.selected_rx_device,
                        &current_rx_label,
                        &rx_dev_opts,
                    ) && self.active_receiver.is_some()
                    {
                        self.active_receiver = None;
                        self.toggle_receiver();
                    }
                });

                let buf_fmt = format!("{} ms", self.target_buffer_ms);
                settings_row(ui, t.label_target_buffer, true, |ui| {
                    quiet_slider_u16(ui, &mut self.target_buffer_ms, 5..=80, &buf_fmt);
                });

                let gain_fmt = format_gain_db(self.volume);
                settings_row(ui, t.label_output_gain, false, |ui| {
                    if quiet_slider_f32(ui, &mut self.volume, 0.0..=2.0, &gain_fmt) {
                        if let Some(ref rx) = self.active_receiver {
                            rx.set_volume(self.volume);
                        }
                        if let Some(ref dpx) = self.active_duplex {
                            dpx.set_rx_volume(self.volume);
                        }
                    }
                });
            });
        });
    }

    // =========================================================================
    // Frame C2: 双工耳麦 (`FullDuplex` Tab)
    // =========================================================================

    fn render_full_duplex_tab(&mut self, ui: &mut egui::Ui) {
        let lang = self.lang;
        let t = lang.texts();
        let dpx_running = self.active_duplex.is_some() || self.demo_mode;

        // Top Page Header (`.top`)
        ui.horizontal(|ui| {
            ui.vertical(|ui| {
                ui.spacing_mut().item_spacing.y = 3.0;
                ui.label(
                    RichText::new(t.page_c2_title)
                        .size(20.0)
                        .color(palette::TEXT_PRIMARY())
                        .strong(),
                );
                ui.label(
                    RichText::new(t.page_c2_subtitle)
                        .size(13.0)
                        .color(palette::TEXT_SECONDARY()),
                );
            });
            ui.with_layout(egui::Layout::right_to_left(Align::Max), |ui| {
                ui.spacing_mut().item_spacing.x = 10.0;
                if header_action_button(
                    ui,
                    t.btn_start_duplex,
                    t.btn_stop_duplex,
                    dpx_running,
                ) {
                    self.toggle_duplex();
                }
                if dpx_running {
                    let elapsed = if self.demo_mode && self.active_duplex.is_none() {
                        "00:12:05".to_string()
                    } else {
                        format_elapsed_hms(self.duplex_started_at)
                    };
                    status_chip(ui, t.status_in_call, Some(&elapsed), true);
                } else {
                    status_chip(ui, t.status_standby, None, false);
                }
            });
        });

        ui.add_space(18.0);

        // Top Connection Bar Card (`section.card.conn`)
        let peer_name = self.resolve_peer_name(&self.duplex_peer_ip_input);
        let st_opt = self.active_duplex.as_ref().map(|d| d.status());
        let rtt_str = st_opt
            .as_ref()
            .and_then(|st| {
                if st.tx.control.connected {
                    Some(format!("{:.2}", (st.tx.control.rtt_us as f64) / 1000.0))
                } else {
                    Some("0.42".to_string())
                }
            })
            .unwrap_or_else(|| {
                if self.demo_mode {
                    "0.42".to_string()
                } else {
                    "—".to_string()
                }
            });
        let codec_badge = match self.selected_profile {
            LatencyProfile::LosslessPcm => "PCM16",
            LatencyProfile::UltraLow => "Opus",
            LatencyProfile::Balanced | LatencyProfile::Stable => "Opus · FEC",
        };

        egui::Frame::none()
            .fill(palette::BG_PANEL())
            .stroke(Stroke::new(1.0_f32, palette::LINE()))
            .rounding(Rounding::same(metrics::RADIUS_CARD))
            .inner_margin(egui::Margin::symmetric(22.0, 16.0))
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = 16.0;

                    // 1. Peer Avatar (`40x40` + online dot) & Peer Info
                    let (av_rect, _) = ui.allocate_exact_size(Vec2::splat(40.0), Sense::hover());
                    if ui.is_rect_visible(av_rect) {
                        let p = ui.painter();
                        p.rect_filled(av_rect, Rounding::same(11.0), palette::BG_RAISED());
                        draw_vector_icon(
                            p,
                            av_rect.shrink(10.0),
                            IconKind::Laptop,
                            palette::TEXT_PRIMARY(),
                        );
                        let badge_pos = Pos2::new(av_rect.max.x - 2.0, av_rect.max.y - 2.0);
                        p.circle_filled(badge_pos, 6.0, palette::BG_PANEL());
                        p.circle_filled(
                            badge_pos,
                            4.0,
                            if dpx_running {
                                palette::ACCENT()
                            } else {
                                palette::DOT_OFFLINE()
                            },
                        );
                    }

                    ui.vertical(|ui| {
                        ui.spacing_mut().item_spacing.y = 3.0;
                        ui.label(
                            RichText::new(&peer_name)
                                .size(14.0)
                                .color(palette::TEXT_PRIMARY())
                                .strong(),
                        );
                        let conn_sub = format!(
                            "{} · ↓{} ↑{}",
                            self.duplex_peer_ip_input,
                            self.duplex_downlink_port,
                            self.duplex_uplink_port
                        );
                        let sub_resp = ui.add(
                            egui::Label::new(
                                RichText::new(conn_sub)
                                    .size(11.5)
                                    .family(FontFamily::Monospace)
                                    .color(palette::TEXT_MUTED()),
                            )
                            .sense(Sense::click()),
                        );
                        let ip_popup_id = ui.make_persistent_id("c2_peer_ip_popup");
                        if sub_resp.clicked() {
                            ui.memory_mut(|mem| mem.toggle_popup(ip_popup_id));
                        }
                        let c2_combo_opts = self.build_target_combo_options();
                        let mut c2_selected_addr: Option<(String, String)> = None;
                        let mut c2_edited_manually = false;
                        let mut c2_rescan = false;
                        egui::popup_below_widget(
                            ui,
                            ip_popup_id,
                            &sub_resp,
                            egui::PopupCloseBehavior::CloseOnClickOutside,
                            |ui| {
                                ui.set_min_width(240.0);
                                ui.label(
                                    RichText::new(t.label_target_addr)
                                        .size(11.0)
                                        .color(palette::TEXT_MUTED()),
                                );
                                let prev_peer_ip = self.duplex_peer_ip_input.clone();
                                quiet_mono_input_pill(
                                    ui,
                                    "c2_peer_ip_edit",
                                    &mut self.duplex_peer_ip_input,
                                );
                                if self.duplex_peer_ip_input != prev_peer_ip {
                                    c2_edited_manually = true;
                                }
                                ui.add_space(4.0);
                                ui.separator();
                                for (addr_val, display_label, _is_lan) in &c2_combo_opts {
                                    let (host, _) =
                                        split_host_port(addr_val, DEFAULT_AUDIO_UDP_PORT);
                                    let selected = self.duplex_peer_ip_input.trim() == host;
                                    if ui.selectable_label(selected, display_label).clicked() {
                                        c2_selected_addr =
                                            Some((addr_val.clone(), display_label.clone()));
                                        ui.memory_mut(|mem| mem.close_popup());
                                    }
                                }
                                ui.separator();
                                if ui.selectable_label(false, t.combo_scan_lan).clicked() {
                                    c2_rescan = true;
                                    ui.memory_mut(|mem| mem.close_popup());
                                }
                            },
                        );
                        if c2_edited_manually {
                            self.target_addr_manually_edited = true;
                            let host = self.duplex_peer_ip_input.trim();
                            if !host.is_empty() {
                                self.target_addr_input =
                                    format!("{host}:{}", self.duplex_downlink_port);
                            }
                        }
                        if let Some((addr_val, label)) = c2_selected_addr {
                            self.target_addr_manually_edited = true;
                            let (host, _) = split_host_port(&addr_val, DEFAULT_AUDIO_UDP_PORT);
                            self.duplex_peer_ip_input = host;
                            self.target_addr_input = addr_val.clone();
                            let peer_label = label.split(" · ").next().unwrap_or(&addr_val);
                            self.last_status_msg =
                                lang.status_selected_lan_peer(peer_label, &addr_val);
                        }
                        if c2_rescan {
                            self.trigger_lan_scan();
                        }
                    });

                    // Vertical Divider `.vr`
                    let (vr1, _) = ui.allocate_exact_size(Vec2::new(8.0, 42.0), Sense::hover());
                    ui.painter().line_segment(
                        [
                            Pos2::new(vr1.center().x, vr1.min.y),
                            Pos2::new(vr1.center().x, vr1.max.y),
                        ],
                        Stroke::new(1.0_f32, palette::LINE()),
                    );

                    // 2. Round-Trip Latency (`往返延迟`)
                    ui.vertical(|ui| {
                        ui.spacing_mut().item_spacing.y = 4.0;
                        ui.label(
                            RichText::new(t.stat_rtt)
                                .size(11.5)
                                .color(palette::TEXT_MUTED()),
                        );
                        ui.horizontal(|ui| {
                            ui.spacing_mut().item_spacing.x = 3.0;
                            ui.label(
                                RichText::new(&rtt_str)
                                    .size(18.0)
                                    .family(FontFamily::Monospace)
                                    .color(palette::TEXT_PRIMARY()),
                            );
                            ui.label(
                                RichText::new("ms")
                                    .size(11.0)
                                    .color(palette::TEXT_MUTED()),
                            );
                        });
                    });

                    // Vertical Divider `.vr`
                    let (vr2, _) = ui.allocate_exact_size(Vec2::new(8.0, 42.0), Sense::hover());
                    ui.painter().line_segment(
                        [
                            Pos2::new(vr2.center().x, vr2.min.y),
                            Pos2::new(vr2.center().x, vr2.max.y),
                        ],
                        Stroke::new(1.0_f32, palette::LINE()),
                    );

                    // 3. Local Role (`本机角色`: 主机电脑 | 无线耳麦)
                    ui.vertical(|ui| {
                        ui.spacing_mut().item_spacing.y = 5.0;
                        ui.label(
                            RichText::new(t.conn_local_role)
                                .size(11.5)
                                .color(palette::TEXT_MUTED()),
                        );
                        let roles = [
                            (DuplexRole::Host, t.role_host_short),
                            (DuplexRole::Headset, t.role_headset_short),
                        ];
                        segmented_control(ui, "c2_role_seg", &mut self.duplex_role, &roles);
                    });

                    // 4. Quality & Latency (`音质与延迟`)
                    ui.vertical(|ui| {
                        ui.spacing_mut().item_spacing.y = 5.0;
                        ui.label(
                            RichText::new(t.label_quality_latency)
                                .size(11.5)
                                .color(palette::TEXT_MUTED()),
                        );
                        latency_profile_selector(ui, &mut self.selected_profile, lang);
                    });

                    // 5. Right-aligned Codec (`编码`)
                    ui.with_layout(egui::Layout::right_to_left(Align::Center), |ui| {
                        ui.vertical(|ui| {
                            ui.spacing_mut().item_spacing.y = 5.0;
                            ui.with_layout(egui::Layout::right_to_left(Align::Min), |ui| {
                                ui.label(
                                    RichText::new(t.conn_codec)
                                        .size(11.5)
                                        .color(palette::TEXT_MUTED()),
                                );
                            });
                            ui.with_layout(egui::Layout::right_to_left(Align::Min), |ui| {
                                ui.label(
                                    RichText::new(codec_badge)
                                        .size(12.0)
                                        .family(FontFamily::Monospace)
                                        .color(palette::TEXT_SECONDARY()),
                                );
                            });
                        });
                    });
                });
            });

        ui.add_space(16.0);

        // Two-Column Cards (`收听` on Left, `麦克风` on Right)
        let target_buf_ms = self.selected_profile.default_target_buffer_ms();
        ui.columns(2, |columns| {
            // =================================================================
            // LEFT CARD: 收听 (Listen / Downlink RX)
            // =================================================================
            theme::studio_card_frame(dpx_running, palette::ACCENT()).show(&mut columns[0], |ui| {
                // 1. Card Header (`.ch`)
                egui::Frame::none()
                    .inner_margin(egui::Margin {
                        left: 20.0,
                        right: 20.0,
                        top: 18.0,
                        bottom: 0.0,
                    })
                    .show(ui, |ui| {
                        ui.horizontal(|ui| {
                            ui.spacing_mut().item_spacing.x = 12.0;
                            draw_card_icon_tile(ui, IconKind::Headset);
                            ui.vertical(|ui| {
                                ui.spacing_mut().item_spacing.y = 1.0;
                                ui.label(
                                    RichText::new(t.c2_listen_title)
                                        .size(15.0)
                                        .color(palette::TEXT_PRIMARY())
                                        .strong(),
                                );
                                let listen_sub = if let Some(src_addr) =
                                    st_opt.as_ref().and_then(|st| st.rx.net.source_addr)
                                {
                                    let src_ip = src_addr.ip().to_string();
                                    let src_peer = self.resolve_peer_name(&src_ip);
                                    format!("{} · {src_peer} ({src_addr})", t.c2_listen_subtitle)
                                } else {
                                    t.c2_listen_subtitle.to_string()
                                };
                                ui.label(
                                    RichText::new(listen_sub)
                                        .size(11.5)
                                        .color(palette::TEXT_MUTED()),
                                );
                            });
                            ui.with_layout(egui::Layout::right_to_left(Align::Center), |ui| {
                                status_chip(
                                    ui,
                                    if dpx_running {
                                        t.status_healthy
                                    } else {
                                        t.status_standby
                                    },
                                    None,
                                    dpx_running,
                                );
                            });
                        });
                    });

                // 2. Stereo Meter (`.meter`)
                egui::Frame::none()
                    .inner_margin(egui::Margin {
                        left: 20.0,
                        right: 20.0,
                        top: 20.0,
                        bottom: 18.0,
                    })
                    .show(ui, |ui| {
                        draw_stereo_vu_meter(ui, &self.duplex_rx_vu_meter, dpx_running, false);
                    });

                // 3. Stats Row (`.stats`)
                let (buf_val, asrc_val, lost_val, repaired_unit) = if let Some(ref st) = st_opt {
                    let repaired = st.rx.net.fec_recovered + st.rx.net.plc_frames;
                    (
                        format!("{:.1}", st.rx.net.buffer_level_ms),
                        format!("{:.5}", st.rx.net.resample_ratio),
                        format!("{}", st.rx.net.packets_lost),
                        format!("/ {repaired}"),
                    )
                } else if self.demo_mode {
                    (
                        "15.2".to_string(),
                        "1.00002".to_string(),
                        "0".to_string(),
                        "/ 12".to_string(),
                    )
                } else {
                    (
                        "—".to_string(),
                        "1.00000".to_string(),
                        "0".to_string(),
                        "/ 0".to_string(),
                    )
                };
                let target_buf_unit = format!("/ {target_buf_ms} ms");

                draw_stats_row_3(
                    ui,
                    [
                        (t.stat_buffer_level, &buf_val, &target_buf_unit),
                        (t.stat_clock_comp, &asrc_val, "×"),
                        (t.stat_loss_repaired, &lost_val, &repaired_unit),
                    ],
                    dpx_running,
                );

                // 4. Jitter Buffer Sparkline Chart (`.chart`)
                draw_sparkline(
                    ui,
                    t.c2_jb_chart_title,
                    "±0.6 ms",
                    t.chart_rx_idle_hint,
                    &self.duplex_buffer_sparkline,
                    0.0,
                    (target_buf_ms as f32 * 2.0).max(30.0),
                    Some(target_buf_ms as f32),
                    palette::ACCENT(),
                );

                // 5. Settings List (`.set`, 3 rows)
                let mut rx_dev_opts = vec![(None, t.device_default_speaker.to_string())];
                for dev in &self.devices {
                    if dev.is_output {
                        rx_dev_opts.push((Some(dev.name.clone()), dev.name.clone()));
                    }
                }
                let current_rx = self
                    .selected_rx_device
                    .clone()
                    .unwrap_or_else(|| t.device_default_speaker.to_string());
                settings_row(ui, t.label_output_device, true, |ui| {
                    if quiet_select_pill(
                        ui,
                        "c2_rx_device_combo",
                        &mut self.selected_rx_device,
                        &current_rx,
                        &rx_dev_opts,
                    ) && self.active_duplex.is_some()
                    {
                        self.active_duplex = None;
                        self.toggle_duplex();
                    }
                });

                let gain_fmt = format_gain_db(self.volume);
                settings_row(ui, t.label_output_gain, true, |ui| {
                    if quiet_slider_f32(ui, &mut self.volume, 0.0..=2.0, &gain_fmt) {
                        if let Some(ref rx) = self.active_receiver {
                            rx.set_volume(self.volume);
                        }
                        if let Some(ref dpx) = self.active_duplex {
                            dpx.set_rx_volume(self.volume);
                        }
                    }
                });

                settings_row(ui, t.label_channels, false, |ui| {
                    let ch_opts = [
                        (ChannelLayout::Stereo, t.channel_stereo),
                        (ChannelLayout::Mono, t.channel_mono),
                    ];
                    segmented_control(ui, "c2_channels_seg", &mut self.duplex_rx_channels, &ch_opts);
                });
            });

            // =================================================================
            // RIGHT CARD: 麦克风 (Microphone / Uplink TX)
            // =================================================================
            theme::studio_card_frame(dpx_running, palette::ACCENT()).show(&mut columns[1], |ui| {
                // 1. Card Header (`.ch`)
                egui::Frame::none()
                    .inner_margin(egui::Margin {
                        left: 20.0,
                        right: 20.0,
                        top: 18.0,
                        bottom: 0.0,
                    })
                    .show(ui, |ui| {
                        ui.horizontal(|ui| {
                            ui.spacing_mut().item_spacing.x = 12.0;
                            draw_card_icon_tile(ui, IconKind::Microphone);
                            ui.vertical(|ui| {
                                ui.spacing_mut().item_spacing.y = 1.0;
                                ui.label(
                                    RichText::new(t.c2_mic_title)
                                        .size(15.0)
                                        .color(palette::TEXT_PRIMARY())
                                        .strong(),
                                );
                                ui.label(
                                    RichText::new(t.c2_mic_subtitle)
                                        .size(11.5)
                                        .color(palette::TEXT_MUTED()),
                                );
                            });
                            ui.with_layout(egui::Layout::right_to_left(Align::Center), |ui| {
                                let chip_label = if !dpx_running {
                                    t.status_standby
                                } else if self.duplex_mic_muted {
                                    t.status_muted
                                } else {
                                    t.status_speaking
                                };
                                status_chip(
                                    ui,
                                    chip_label,
                                    None,
                                    dpx_running && !self.duplex_mic_muted,
                                );
                            });
                        });
                    });

                // 2. Mono VU Meter with Noise Gate Marker (`.meter`)
                egui::Frame::none()
                    .inner_margin(egui::Margin {
                        left: 20.0,
                        right: 20.0,
                        top: 18.0,
                        bottom: 18.0,
                    })
                    .show(ui, |ui| {
                        draw_mono_gate_vu_meter(
                            ui,
                            &self.duplex_tx_vu_meter,
                            dpx_running,
                            self.duplex_mic_muted,
                            self.duplex_gate_db,
                            t.c2_gate_marker_prefix,
                        );
                    });

                // 3. Stats Row (`.stats`)
                let (lvl_val, gated_val, sent_val) = if let Some(ref st) = st_opt {
                    let db_str = if self.duplex_mic_muted || self.duplex_tx_vu_meter.smoothed_db <= -59.5 {
                        "-∞".to_string()
                    } else {
                        format!("{:.1}", self.duplex_tx_vu_meter.smoothed_db)
                    };
                    (
                        db_str,
                        format_with_commas(st.tx.net.gated_dtx_frames),
                        format_with_commas(st.tx.net.packets_sent),
                    )
                } else if self.demo_mode {
                    (
                        "-14.2".to_string(),
                        "18,420".to_string(),
                        "142,910".to_string(),
                    )
                } else {
                    ("—".to_string(), "0".to_string(), "0".to_string())
                };

                draw_stats_row_3(
                    ui,
                    [
                        (t.stat_level, &lvl_val, "dB"),
                        (t.stat_gated_frames, &gated_val, ""),
                        (t.stat_packets_sent, &sent_val, t.unit_packets),
                    ],
                    dpx_running,
                );

                // 4. Voice Envelope Sparkline Chart (`.chart`)
                draw_sparkline(
                    ui,
                    t.c2_voice_chart_title,
                    "Opus 128 kbps",
                    t.chart_tx_idle_hint,
                    &self.duplex_tx_envelope_sparkline,
                    -60.0,
                    0.0,
                    Some(self.duplex_gate_db),
                    palette::ACCENT(),
                );

                // 5. Settings List (`.set`, 3 rows)
                let mut tx_dev_opts = vec![(None, t.device_system_default.to_string())];
                for dev in &self.devices {
                    let eligible = dev.is_input
                        || (cfg!(target_os = "windows")
                            && self.duplex_role == DuplexRole::Host
                            && dev.is_output);
                    if eligible {
                        tx_dev_opts.push((Some(dev.name.clone()), dev.name.clone()));
                    }
                }
                let current_tx = self
                    .selected_tx_device
                    .clone()
                    .unwrap_or_else(|| t.device_system_default.to_string());
                settings_row(ui, t.label_microphone, true, |ui| {
                    if quiet_select_pill(
                        ui,
                        "c2_tx_device_combo",
                        &mut self.selected_tx_device,
                        &current_tx,
                        &tx_dev_opts,
                    ) && self.active_duplex.is_some()
                    {
                        self.active_duplex = None;
                        self.toggle_duplex();
                    }
                });

                let gate_fmt = format!("{:.0} dB", self.duplex_gate_db);
                settings_row(ui, t.label_noise_gate_threshold, true, |ui| {
                    if quiet_slider_f32(ui, &mut self.duplex_gate_db, -65.0..=-20.0, &gate_fmt) {
                        if let Some(ref dpx) = self.active_duplex {
                            dpx.set_tx_noise_gate_db(Some(self.duplex_gate_db));
                        }
                    }
                });

                settings_row(ui, t.label_mic_mute, false, |ui| {
                    if pill_toggle(ui, &mut self.duplex_mic_muted) {
                        if let Some(ref dpx) = self.active_duplex {
                            dpx.set_tx_muted(self.duplex_mic_muted);
                        }
                    }
                });
            });
        });
    }

    // =========================================================================
    // Tab 3: 音频设备 (`HardwareMatrix` Tab) in Quiet Graphite Style
    // =========================================================================

    fn render_hardware_matrix_tab(&mut self, ui: &mut egui::Ui) {
        let t = self.lang.texts();

        // Top Page Header
        ui.horizontal(|ui| {
            ui.vertical(|ui| {
                ui.spacing_mut().item_spacing.y = 3.0;
                ui.label(
                    RichText::new(t.page_hw_title)
                        .size(20.0)
                        .color(palette::TEXT_PRIMARY())
                        .strong(),
                );
                ui.label(
                    RichText::new(t.page_hw_subtitle)
                        .size(13.0)
                        .color(palette::TEXT_SECONDARY()),
                );
            });
            ui.with_layout(egui::Layout::right_to_left(Align::Max), |ui| {
                if ui
                    .button(
                        RichText::new(t.btn_refresh_ports)
                            .size(12.5)
                            .color(palette::TEXT_PRIMARY()),
                    )
                    .on_hover_text(t.btn_refresh_ports_hover)
                    .clicked()
                {
                    self.refresh_audio_devices(true);
                }
            });
        });

        ui.add_space(18.0);

        let in_devices: Vec<AudioDeviceInfo> = self
            .devices
            .iter()
            .filter(|d| d.is_input)
            .cloned()
            .collect();
        let out_devices: Vec<AudioDeviceInfo> = self
            .devices
            .iter()
            .filter(|d| d.is_output)
            .cloned()
            .collect();

        ui.columns(2, |cols| {
            // Left Card: Input Devices
            theme::studio_card_frame(false, palette::LINE()).show(&mut cols[0], |ui| {
                egui::Frame::none()
                    .inner_margin(egui::Margin::symmetric(20.0, 18.0))
                    .show(ui, |ui| {
                        ui.horizontal(|ui| {
                            ui.spacing_mut().item_spacing.x = 12.0;
                            draw_card_icon_tile(ui, IconKind::Microphone);
                            ui.vertical(|ui| {
                                ui.spacing_mut().item_spacing.y = 1.0;
                                ui.label(
                                    RichText::new(t.hw_input_title)
                                        .size(15.0)
                                        .color(palette::TEXT_PRIMARY())
                                        .strong(),
                                );
                                ui.label(
                                    RichText::new(t.hw_input_subtitle)
                                        .size(11.5)
                                        .color(palette::TEXT_MUTED()),
                                );
                            });
                            ui.with_layout(egui::Layout::right_to_left(Align::Center), |ui| {
                                status_chip(
                                    ui,
                                    &format!("{}", in_devices.len()),
                                    None,
                                    !in_devices.is_empty(),
                                );
                            });
                        });
                    });

                draw_hairline(ui);

                let count = in_devices.len();
                for (idx, dev) in in_devices.iter().enumerate() {
                    settings_row(ui, &dev.name, idx + 1 < count, |ui| {
                        ui.spacing_mut().item_spacing.x = 8.0;
                        ui.label(
                            RichText::new(format!(
                                "{} Hz · {} ch",
                                dev.default_sample_rate.unwrap_or(48_000),
                                dev.default_channels.unwrap_or(2)
                            ))
                            .size(11.5)
                            .family(FontFamily::Monospace)
                            .color(palette::TEXT_MUTED()),
                        );
                        if dev.is_default_input {
                            status_chip(ui, t.badge_default_in, None, true);
                        }
                    });
                }
            });

            // Right Card: Output Devices
            theme::studio_card_frame(false, palette::LINE()).show(&mut cols[1], |ui| {
                egui::Frame::none()
                    .inner_margin(egui::Margin::symmetric(20.0, 18.0))
                    .show(ui, |ui| {
                        ui.horizontal(|ui| {
                            ui.spacing_mut().item_spacing.x = 12.0;
                            draw_card_icon_tile(ui, IconKind::SpeakerDevice);
                            ui.vertical(|ui| {
                                ui.spacing_mut().item_spacing.y = 1.0;
                                ui.label(
                                    RichText::new(t.hw_output_title)
                                        .size(15.0)
                                        .color(palette::TEXT_PRIMARY())
                                        .strong(),
                                );
                                ui.label(
                                    RichText::new(t.hw_output_subtitle)
                                        .size(11.5)
                                        .color(palette::TEXT_MUTED()),
                                );
                            });
                            ui.with_layout(egui::Layout::right_to_left(Align::Center), |ui| {
                                status_chip(
                                    ui,
                                    &format!("{}", out_devices.len()),
                                    None,
                                    !out_devices.is_empty(),
                                );
                            });
                        });
                    });

                draw_hairline(ui);

                let count = out_devices.len();
                for (idx, dev) in out_devices.iter().enumerate() {
                    settings_row(ui, &dev.name, idx + 1 < count, |ui| {
                        ui.spacing_mut().item_spacing.x = 8.0;
                        ui.label(
                            RichText::new(format!(
                                "{} Hz · {} ch",
                                dev.default_sample_rate.unwrap_or(48_000),
                                dev.default_channels.unwrap_or(2)
                            ))
                            .size(11.5)
                            .family(FontFamily::Monospace)
                            .color(palette::TEXT_MUTED()),
                        );
                        if dev.is_default_output {
                            status_chip(ui, t.badge_default_out, None, true);
                        }
                    });
                }

                draw_hairline(ui);
                let mut mdns_toggled = false;
                settings_row(ui, t.chk_advertise_mdns, false, |ui| {
                    if pill_toggle(ui, &mut self.advertise_mdns) {
                        mdns_toggled = true;
                    }
                });
                if mdns_toggled {
                    self.sync_lan_advertiser();
                }
            });
        });
    }

    pub fn render_ui(&mut self, ctx: &egui::Context) {
        #[cfg(target_os = "windows")]
        let _ = win32_tray::capture_main_window_hwnd();

        if !self.theme_initialized {
            theme::apply_studio_theme(ctx);
            if let Ok(mut guard) = global_egui_wakeup_ctx().lock() {
                *guard = Some(ctx.clone());
            }
            self.theme_initialized = true;
        }
        theme::sync_theme_mode(ctx, self.theme_mode);

        if self.tray.is_some()
            && !self.quit_requested
            && ctx.input(|i| i.viewport().close_requested())
        {
            ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
            self.enter_tray_background_mode(ctx);
        }

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
                    self.tray = None;
                    apply_os_background_tray_mode(false);
                    ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                    ctx.request_repaint();
                    #[cfg(target_os = "windows")]
                    win32_tray::request_close_main_window();
                }
            }
        }

        if self.quit_requested {
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            ctx.request_repaint();
            #[cfg(target_os = "windows")]
            win32_tray::request_close_main_window();
            return;
        }

        if self.last_device_poll.elapsed() >= Duration::from_millis(2500) {
            self.last_device_poll = Instant::now();
            self.refresh_audio_devices(false);
        }
        if !self.demo_mode && self.last_lan_scan.elapsed() >= Duration::from_secs(4) {
            self.trigger_lan_scan_internal(false);
        }
        self.apply_pending_device_updates();
        self.apply_auto_discovered_target();
        self.sync_tray_status_title();

        match self.tray_window_state {
            TrayWindowState::ShrinkingToTray { frames_remaining } => {
                if frames_remaining > 1 {
                    self.tray_window_state = TrayWindowState::ShrinkingToTray {
                        frames_remaining: frames_remaining - 1,
                    };
                    ctx.request_repaint();
                    #[cfg(target_os = "windows")]
                    win32_tray::wake_main_window();
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
            TrayWindowState::Visible => {
                if self.pending_restore_focus {
                    self.pending_restore_focus = false;
                    #[cfg(target_os = "windows")]
                    win32_tray::present_and_focus_main_window();
                    ctx.send_viewport_cmd(egui::ViewportCommand::MinInnerSize(Vec2::new(
                        980.0, 660.0,
                    )));
                    ctx.send_viewport_cmd(egui::ViewportCommand::InnerSize(Vec2::new(
                        1220.0, 760.0,
                    )));
                    ctx.send_viewport_cmd(egui::ViewportCommand::Focus);
                    ctx.request_repaint();
                }
            }
        }

        if self.active_sender.is_some()
            || self.active_receiver.is_some()
            || self.active_duplex.is_some()
            || self.scanning_lan.load(Ordering::Relaxed)
        {
            ctx.request_repaint_after(Duration::from_millis(33));
        }

        self.update_live_telemetry();

        // Left Sidebar + Main Workspace Canvas (Quiet Graphite C & C2 layout)
        self.render_sidebar(ctx);

        egui::CentralPanel::default()
            .frame(
                egui::Frame::none()
                    .fill(palette::BG_CANVAS())
                    .inner_margin(egui::Margin {
                        left: metrics::MAIN_PADDING_X,
                        right: metrics::MAIN_PADDING_X,
                        top: metrics::MAIN_PADDING_Y,
                        bottom: 24.0,
                    }),
            )
            .show(ctx, |ui| {
                egui::ScrollArea::vertical()
                    .auto_shrink([false, false])
                    .show(ui, |ui| match self.active_tab {
                        WorkspaceTab::DualConsole => self.render_dual_console_tab(ui),
                        WorkspaceTab::FullDuplex => self.render_full_duplex_tab(ui),
                        WorkspaceTab::HardwareMatrix => self.render_hardware_matrix_tab(ui),
                    });
            });

        if let Some(ref target_path) = self.screenshot_target.clone() {
            let captured = ctx.input(|i| {
                for ev in &i.raw.events {
                    if let egui::Event::Screenshot { image, .. } = ev {
                        return Some(Arc::clone(image));
                    }
                }
                None
            });
            if let Some(img) = captured {
                let _ = save_color_image_png(&img, target_path);
                self.quit_requested = true;
                ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            } else {
                self.screenshot_frames = self.screenshot_frames.saturating_add(1);
                if self.screenshot_frames == 4 {
                    ctx.send_viewport_cmd(egui::ViewportCommand::Screenshot);
                }
                ctx.request_repaint();
            }
        }
    }
}

fn save_color_image_png(img: &egui::ColorImage, path: &str) -> std::io::Result<()> {
    use std::io::Write;
    let w = img.width() as u32;
    let h = img.height() as u32;
    let mut raw = Vec::with_capacity((w as usize * 4 + 1) * h as usize);
    for y in 0..h as usize {
        raw.push(0u8); // filter type 0 (None)
        let row_start = y * (w as usize);
        for c in &img.pixels[row_start..row_start + (w as usize)] {
            raw.extend_from_slice(&c.to_array());
        }
    }

    // Zlib stream with uncompressed deflate blocks + Adler32
    let mut zlib = Vec::with_capacity(raw.len() + (raw.len() / 65535 + 1) * 5 + 6);
    zlib.extend_from_slice(&[0x78, 0x01]);
    let chunks: Vec<&[u8]> = raw.chunks(65535).collect();
    for (idx, chunk) in chunks.iter().enumerate() {
        let is_last = idx + 1 == chunks.len();
        zlib.push(if is_last { 0x01 } else { 0x00 });
        let len = chunk.len() as u16;
        zlib.extend_from_slice(&len.to_le_bytes());
        zlib.extend_from_slice(&(!len).to_le_bytes());
        zlib.extend_from_slice(chunk);
    }
    let mut s1: u32 = 1;
    let mut s2: u32 = 0;
    for &b in &raw {
        s1 = (s1 + b as u32) % 65521;
        s2 = (s2 + s1) % 65521;
    }
    let adler = (s2 << 16) | s1;
    zlib.extend_from_slice(&adler.to_be_bytes());

    fn write_png_chunk(out: &mut Vec<u8>, tag: &[u8; 4], data: &[u8]) {
        out.extend_from_slice(&(data.len() as u32).to_be_bytes());
        out.extend_from_slice(tag);
        out.extend_from_slice(data);
        let mut crc: u32 = 0xFFFF_FFFF;
        for &b in tag.iter().chain(data.iter()) {
            crc ^= b as u32;
            for _ in 0..8 {
                crc = if (crc & 1) != 0 {
                    (crc >> 1) ^ 0xEDB8_8320
                } else {
                    crc >> 1
                };
            }
        }
        out.extend_from_slice(&(!crc).to_be_bytes());
    }

    let mut png = Vec::with_capacity(zlib.len() + 64);
    png.extend_from_slice(b"\x89PNG\r\n\x1a\n");
    let mut ihdr = Vec::with_capacity(13);
    ihdr.extend_from_slice(&w.to_be_bytes());
    ihdr.extend_from_slice(&h.to_be_bytes());
    ihdr.extend_from_slice(&[8, 6, 0, 0, 0]);
    write_png_chunk(&mut png, b"IHDR", &ihdr);
    write_png_chunk(&mut png, b"IDAT", &zlib);
    write_png_chunk(&mut png, b"IEND", &[]);

    let mut f = std::fs::File::create(path)?;
    f.write_all(&png)
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

    if std::env::args().any(|a| a == "--install-app") {
        let path = macos_bundle::install_macos_app_bundle()?;
        println!("Installed macOS app bundle at: {}", path.display());
        return Ok(());
    }

    let start_in_tray = std::env::args().any(|a| a == "--tray" || a == "--background");

    let (icon_rgba, icon_w, icon_h) = build_app_icon_rgba();
    let viewport = if start_in_tray {
        egui::ViewportBuilder::default()
            .with_title("rsrelay")
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
            .with_title("rsrelay")
            .with_inner_size([1220.0, 760.0])
            .with_min_inner_size([980.0, 660.0])
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
        } else if let Some(tab_val) = arg.strip_prefix("--tab=") {
            match tab_val.to_ascii_lowercase().as_str() {
                "0" | "c" | "dual" | "txrx" => app.active_tab = WorkspaceTab::DualConsole,
                "1" | "c2" | "duplex" | "headset" => app.active_tab = WorkspaceTab::FullDuplex,
                "2" | "hw" | "devices" | "matrix" => app.active_tab = WorkspaceTab::HardwareMatrix,
                _ => {}
            }
        } else if let Some(val) = arg.strip_prefix("--theme=") {
            if let Some(tm) = ThemeMode::from_str_opt(val) {
                app.theme_mode = tm;
            }
        } else if arg == "--demo" {
            app.enable_demo_preview();
        } else if let Some(path) = arg.strip_prefix("--screenshot=") {
            app.screenshot_target = Some(path.to_string());
        }
    }
    if app.demo_mode {
        app.enable_demo_preview();
    }

    let test_tray_cycle = std::env::args().any(|a| a == "--test-tray-cycle");
    if test_tray_cycle {
        if let Some(ref tray) = app.tray {
            let toggle_id = tray.toggle_window_id.clone();
            let quit_id = tray.quit_id.clone();
            thread::spawn(move || {
                // 1. Wait until app is HiddenInTray, then simulate clicking "显示主页面" in tray menu
                thread::sleep(Duration::from_millis(500));
                let (tx, _) = global_tray_event_queue();
                let _ = tx.send(toggle_id.clone());
                if let Ok(guard) = global_egui_wakeup_ctx().lock() {
                    if let Some(ref ctx) = *guard {
                        ctx.request_repaint();
                    }
                }
                #[cfg(target_os = "windows")]
                win32_tray::wake_main_window();

                // 2. Wait while window is restored & visible, then simulate hiding back to tray
                thread::sleep(Duration::from_millis(500));
                let _ = tx.send(toggle_id);
                if let Ok(guard) = global_egui_wakeup_ctx().lock() {
                    if let Some(ref ctx) = *guard {
                        ctx.request_repaint();
                    }
                }
                #[cfg(target_os = "windows")]
                win32_tray::wake_main_window();

                // 3. Wait until HiddenInTray again, then simulate clicking "退出" in tray menu
                thread::sleep(Duration::from_millis(500));
                let _ = tx.send(quit_id);
                if let Ok(guard) = global_egui_wakeup_ctx().lock() {
                    if let Some(ref ctx) = *guard {
                        ctx.request_repaint();
                    }
                }
                #[cfg(target_os = "windows")]
                win32_tray::wake_main_window();

                thread::sleep(Duration::from_millis(1500));
                std::process::exit(2);
            });
        }
    }

    eframe::run_native(
        "rsrelay",
        native_options,
        Box::new(move |cc| {
            #[cfg(target_os = "windows")]
            let _ = win32_tray::capture_main_window_hwnd();
            theme::apply_studio_theme(&cc.egui_ctx);
            if let Ok(mut guard) = global_egui_wakeup_ctx().lock() {
                *guard = Some(cc.egui_ctx.clone());
            }
            app.theme_initialized = true;
            if start_in_tray || test_tray_cycle {
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

            for mode in [ThemeMode::System, ThemeMode::Light, ThemeMode::Dark] {
                app.theme_mode = mode;
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

        for _ in 0..3 {
            let _ = ctx.run(egui::RawInput::default(), |ctx| {
                app.render_ui(ctx);
            });
        }
        assert_eq!(app.tray_window_state, TrayWindowState::HiddenInTray);

        let _ = ctx.run(egui::RawInput::default(), |ctx| {
            app.restore_from_tray(ctx);
        });
        assert_eq!(app.tray_window_state, TrayWindowState::Visible);
    }

    #[test]
    fn test_auto_discovered_target_and_hybrid_combo_options() {
        let mut app = RsRelayGuiApp::new_with_tray(false).expect("init app");
        assert_eq!(app.target_addr_input, "127.0.0.1:49153");
        assert!(!app.target_addr_manually_edited);

        if let Ok(mut guard) = app.discovered_peers.lock() {
            guard.push(DiscoveredPeer {
                instance_name: "rsrelay-desktop-PC110".to_string(),
                hostname: "PC-110".to_string(),
                addresses: vec!["192.168.3.110".parse().unwrap()],
                udp_port: 49153,
                version: "0.1.0".to_string(),
                os: "windows".to_string(),
            });
            guard.push(DiscoveredPeer {
                instance_name: "rsrelay-desktop-PC151".to_string(),
                hostname: "PC-151".to_string(),
                addresses: vec!["192.168.3.151".parse().unwrap()],
                udp_port: 49153,
                version: "0.1.0".to_string(),
                os: "windows".to_string(),
            });
        }

        // Auto-recognition fills the first discovered LAN peer when still on default loopback
        app.apply_auto_discovered_target();
        assert_eq!(app.target_addr_input, "192.168.3.110:49153");
        assert_eq!(app.duplex_peer_ip_input, "192.168.3.110");

        // Dropdown options include both discovered LAN peers + local options
        let opts = app.build_target_combo_options();
        assert!(opts
            .iter()
            .any(|(addr, _, is_lan)| addr == "192.168.3.110:49153" && *is_lan));
        assert!(opts
            .iter()
            .any(|(addr, _, is_lan)| addr == "192.168.3.151:49153" && *is_lan));

        // Flexible manual input parsing allows bare IP without port
        let parsed = parse_socket_addr_flexible(
            "192.168.3.151",
            IpAddr::from([127, 0, 0, 1]),
            DEFAULT_AUDIO_UDP_PORT,
        )
        .expect("parse bare IP");
        assert_eq!(parsed.to_string(), "192.168.3.151:49153");
    }
}
