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
    install_macos_virtual_mic_driver, is_macos_rsrelay_driver_installed,
    is_virtual_mic_input_device_name, is_virtual_mic_output_sink_name,
    launch_windows_vbcable_install, list_audio_devices, preset_bus_modes_for_role,
    request_system_loopback_permission, select_virtual_mic_output_from_devices, AudioDeviceInfo,
    ChannelLayout, DiscoveredPeer, DiscoveryScanner, DiscoveryService, DuplexRole, DuplexSession,
    DuplexSessionConfig, LatencyProfile, MediaBusMode, MicBusMode, DEFAULT_AUDIO_UDP_PORT,
    DEFAULT_UPLINK_UDP_PORT, MACOS_RSRELAY_VMIC_INPUT_NAME,
};
use tray_icon::{
    menu::{Menu, MenuEvent, MenuItem},
    Icon, TrayIcon, TrayIconBuilder,
};

use crate::i18n::Language;
use crate::theme::{metrics, palette, ThemeMode};
use crate::widgets::{
    compact_stereo_vu_meter, dbfs_to_norm, draw_card_icon_tile, draw_conduit_lane, draw_hairline,
    draw_interactive_gate_vu_meter, draw_mute_icon_button, draw_signal_health_pill, draw_sparkline,
    draw_stats_row_3, draw_vector_icon, drawer_toggle_button, flexible_select_pill,
    format_with_commas, header_action_button, header_status_chip, inline_volume_slider,
    latency_profile_selector, linear_to_dbfs, paint_text_vcenter, pill_toggle,
    quiet_mono_input_pill, quiet_slider_u16, scene_preset_card, segmented_control, settings_row,
    status_chip, ConduitDirection, IconKind, SparklineBuffer, VuMeterState,
};

/// Top-level workspace navigation modes (streamlined to 2 unified workspaces).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WorkspaceTab {
    /// Unified Visual Dual-Node Audio & Microphone Link (`音频连接`).
    AudioLink,
    /// Local CoreAudio / WASAPI hardware device inspector (`音频设备`).
    HardwareMatrix,
}

/// Intuitive 1-click scene presets covering both full-duplex and 1-way audio/mic scenarios.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScenePreset {
    /// Listen to peer's desktop audio + send local physical mic (`当无线耳麦`).
    Headset,
    /// Send local desktop audio + receive peer's mic into Virtual Mic (`作为主机电脑`).
    Host,
    /// Only listen to peer's desktop audio, mic off (`仅当无线音箱`).
    SpeakerOnly,
    /// Only send local physical mic to peer's Virtual Mic, speaker off (`仅当无线麦克风`).
    MicOnly,
}

impl ScenePreset {
    pub const fn bus_modes(self) -> (DuplexRole, MediaBusMode, MicBusMode) {
        match self {
            Self::Headset => (
                DuplexRole::Headset,
                MediaBusMode::ReceiveSpeaker,
                MicBusMode::SendMic,
            ),
            Self::Host => (
                DuplexRole::Host,
                MediaBusMode::SendLoopback,
                MicBusMode::ReceiveVirtualMic,
            ),
            Self::SpeakerOnly => (
                DuplexRole::Headset,
                MediaBusMode::ReceiveSpeaker,
                MicBusMode::Off,
            ),
            Self::MicOnly => (DuplexRole::Headset, MediaBusMode::Off, MicBusMode::SendMic),
        }
    }

    pub const fn from_bus_modes(media: MediaBusMode, mic: MicBusMode) -> Option<Self> {
        match (media, mic) {
            (MediaBusMode::ReceiveSpeaker, MicBusMode::SendMic) => Some(Self::Headset),
            (MediaBusMode::SendLoopback, MicBusMode::ReceiveVirtualMic) => Some(Self::Host),
            (MediaBusMode::ReceiveSpeaker, MicBusMode::Off) => Some(Self::SpeakerOnly),
            (MediaBusMode::Off, MicBusMode::SendMic) => Some(Self::MicOnly),
            _ => None,
        }
    }
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

            if dx <= 14
                && dy <= 14
                && (dx <= 10 || dy <= 10 || (dx - 10) * (dx - 10) + (dy - 10) * (dy - 10) <= 16)
            {
                rgba[idx] = 28;
                rgba[idx + 1] = 28;
                rgba[idx + 2] = 31;
                rgba[idx + 3] = 255;

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
    /// Normal desktop window (`1220x760`).
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
                return 0;
            }
        }
        1
    }

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

    pub fn set_hidden_in_tray(hidden: bool) {
        WINDOWS_HIDDEN_IN_TRAY.store(hidden, Ordering::Relaxed);
        if hidden {
            ensure_background_pump_thread();
            wake_main_window();
        }
    }

    pub fn wake_main_window() {
        let hwnd = GLOBAL_MAIN_HWND.load(Ordering::Relaxed);
        if hwnd != 0 && unsafe { IsWindow(hwnd) != 0 } {
            unsafe {
                PostMessageW(hwnd, WM_PAINT, 0, 0);
            }
        }
    }

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
    toggle_link_item: MenuItem,
    mute_speaker_item: MenuItem,
    mute_mic_item: MenuItem,
    stop_all_item: MenuItem,
    quit_item: MenuItem,
    toggle_window_id: tray_icon::menu::MenuId,
    toggle_link_id: tray_icon::menu::MenuId,
    mute_speaker_id: tray_icon::menu::MenuId,
    mute_mic_id: tray_icon::menu::MenuId,
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
        self.toggle_link_item.set_text(t.tray_toggle_link);
        self.mute_speaker_item.set_text(t.tray_mute_speaker);
        self.mute_mic_item.set_text(t.tray_mute_mic);
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
    let toggle_link_item = MenuItem::new(t.tray_toggle_link, true, None);
    let mute_speaker_item = MenuItem::new(t.tray_mute_speaker, true, None);
    let mute_mic_item = MenuItem::new(t.tray_mute_mic, true, None);
    let stop_all_item = MenuItem::new(t.tray_stop_all, true, None);
    let quit_item = MenuItem::new(t.tray_quit, true, None);

    let toggle_window_id = toggle_window_item.id().clone();
    let toggle_link_id = toggle_link_item.id().clone();
    let mute_speaker_id = mute_speaker_item.id().clone();
    let mute_mic_id = mute_mic_item.id().clone();
    let stop_all_id = stop_all_item.id().clone();
    let quit_id = quit_item.id().clone();

    if let Ok(mut id_guard) = global_tray_toggle_window_id().lock() {
        *id_guard = Some(toggle_window_id.clone());
    }

    menu.append(&title_item)?;
    menu.append(&toggle_window_item)?;
    menu.append(&toggle_link_item)?;
    menu.append(&mute_speaker_item)?;
    menu.append(&mute_mic_item)?;
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
        toggle_link_item,
        mute_speaker_item,
        mute_mic_item,
        stop_all_item,
        quit_item,
        toggle_window_id,
        toggle_link_id,
        mute_speaker_id,
        mute_mic_id,
        stop_all_id,
        quit_id,
    })
}

fn format_elapsed_hms(started_at: Option<Instant>) -> String {
    let secs = started_at.map(|t| t.elapsed().as_secs()).unwrap_or(0);
    let h = secs / 3600;
    let m = (secs % 3600) / 60;
    let s = secs % 60;
    format!("{h:02}:{m:02}:{s:02}")
}

pub fn split_host_port(input: &str, default_port: u16) -> (String, String) {
    let trimmed = input.trim();
    if let Some((host, port)) = trimmed.rsplit_once(':') {
        (host.to_string(), format!(":{port}"))
    } else if trimmed.is_empty() {
        ("0.0.0.0".to_string(), format!(":{default_port}"))
    } else {
        (trimmed.to_string(), format!(":{default_port}"))
    }
}

pub fn parse_socket_addr_flexible(
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
    pending_devices: Arc<Mutex<Option<(Vec<AudioDeviceInfo>, Option<IpAddr>, bool)>>>,
    polling_devices: Arc<AtomicBool>,
    pub discovered_peers: Arc<Mutex<Vec<DiscoveredPeer>>>,
    pub scanning_lan: Arc<AtomicBool>,
    pub target_addr_manually_edited: bool,
    last_lan_scan: Instant,
    lan_advertiser: Option<DiscoveryService>,

    // Unified Audio Link session & topology controls
    pub target_addr_input: String,
    pub duplex_peer_ip_input: String,
    pub duplex_role: DuplexRole,
    pub duplex_media_mode: MediaBusMode,
    pub duplex_mic_mode: MicBusMode,
    pub duplex_media_tx_device: Option<String>,
    pub duplex_media_rx_device: Option<String>,
    pub duplex_mic_tx_device: Option<String>,
    pub duplex_mic_rx_device: Option<String>,

    pub selected_profile: LatencyProfile,
    pub volume: f32,
    pub speaker_muted: bool,
    pub duplex_mic_muted: bool,
    pub duplex_gate_db: f32,

    // Collapsible Pro Telemetry & Advanced Drawer controls
    pub show_advanced_drawer: bool,
    pub duplex_downlink_port: u16,
    pub duplex_uplink_port: u16,
    pub media_port_input: String,
    pub mic_port_input: String,
    pub custom_buffer_enabled: bool,
    pub target_buffer_ms: u16,
    pub opus_bitrate_kbps: u16,
    pub duplex_rx_channels: ChannelLayout,
    pub use_test_tone: bool,
    pub advertise_mdns: bool,

    pub active_duplex: Option<DuplexSession>,
    duplex_started_at: Option<Instant>,
    pub installing_vmic_driver: Arc<AtomicBool>,
    pending_vmic_install_result: Arc<Mutex<Option<Result<String, String>>>>,
    pub audiorelay_conflict_cached: bool,

    // Real-time VU meters & 20s sparkline buffers
    duplex_rx_vu_meter: VuMeterState,
    duplex_tx_vu_meter: VuMeterState,
    duplex_buffer_sparkline: SparklineBuffer,
    duplex_tx_envelope_sparkline: SparklineBuffer,

    // Visual verification & tray lifecycle
    pub demo_mode: bool,
    pub screenshot_target: Option<String>,
    screenshot_frames: u8,
    pub auto_follow_default_device: bool,
    last_device_poll: Instant,
    pub last_status_msg: String,
    pub tray_window_state: TrayWindowState,
    pending_restore_focus: bool,
    pub quit_requested: bool,
    last_tray_title: String,
    last_tray_title_update: Instant,
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

        let initial_role = DuplexRole::Headset;
        let (init_media_mode, init_mic_mode) = preset_bus_modes_for_role(initial_role);
        let audiorelay_conflict_cached = false;

        let mut app = Self {
            lang,
            theme_mode: ThemeMode::System,
            active_tab: WorkspaceTab::AudioLink,
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
            duplex_peer_ip_input: "127.0.0.1".to_string(),
            duplex_role: initial_role,
            duplex_media_mode: init_media_mode,
            duplex_mic_mode: init_mic_mode,
            duplex_media_tx_device: None,
            duplex_media_rx_device: None,
            duplex_mic_tx_device: None,
            duplex_mic_rx_device: None,
            selected_profile: LatencyProfile::Balanced,
            volume: 1.0,
            speaker_muted: false,
            duplex_mic_muted: false,
            duplex_gate_db: -42.0,
            show_advanced_drawer: false,
            duplex_downlink_port: DEFAULT_AUDIO_UDP_PORT,
            duplex_uplink_port: DEFAULT_UPLINK_UDP_PORT,
            media_port_input: DEFAULT_AUDIO_UDP_PORT.to_string(),
            mic_port_input: DEFAULT_UPLINK_UDP_PORT.to_string(),
            custom_buffer_enabled: false,
            target_buffer_ms: 20,
            opus_bitrate_kbps: 128,
            duplex_rx_channels: ChannelLayout::Stereo,
            use_test_tone: false,
            advertise_mdns: true,
            active_duplex: None,
            duplex_started_at: None,
            installing_vmic_driver: Arc::new(AtomicBool::new(false)),
            pending_vmic_install_result: Arc::new(Mutex::new(None)),
            audiorelay_conflict_cached,
            duplex_rx_vu_meter: VuMeterState::default(),
            duplex_tx_vu_meter: VuMeterState::default(),
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
            last_tray_title: String::new(),
            last_tray_title_update: Instant::now() - Duration::from_secs(5),
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
            self.last_tray_title = lang.texts().tray_title.to_string();
        }
        self.last_status_msg = lang.status_language_switched();
    }

    pub fn apply_scene_preset(&mut self, preset: ScenePreset) {
        let (role, media, mic) = preset.bus_modes();
        self.duplex_role = role;
        self.duplex_media_mode = media;
        self.duplex_mic_mode = mic;
        if self.active_duplex.is_some() {
            self.restart_active_session();
        }
    }

    fn sync_role_from_bus_modes(&mut self) {
        self.duplex_role = match (self.duplex_media_mode, self.duplex_mic_mode) {
            (MediaBusMode::SendLoopback, MicBusMode::ReceiveVirtualMic)
            | (MediaBusMode::SendLoopback, MicBusMode::Off)
            | (MediaBusMode::Off, MicBusMode::ReceiveVirtualMic) => DuplexRole::Host,
            (MediaBusMode::Bidirectional, _) | (_, MicBusMode::Bidirectional) => {
                DuplexRole::Symmetric
            }
            _ => DuplexRole::Headset,
        };
    }

    pub fn toggle_speaker_mute(&mut self) {
        self.speaker_muted = !self.speaker_muted;
        if let Some(ref dpx) = self.active_duplex {
            let eff_vol = if self.speaker_muted { 0.0 } else { self.volume };
            dpx.set_rx_volume(eff_vol);
            if !self.duplex_media_mode.receives() && self.duplex_media_mode.sends() {
                dpx.set_media_tx_muted(self.speaker_muted);
            }
        }
    }

    pub fn toggle_mic_mute(&mut self) {
        self.duplex_mic_muted = !self.duplex_mic_muted;
        if let Some(ref dpx) = self.active_duplex {
            dpx.set_tx_muted(self.duplex_mic_muted);
            if !self.duplex_mic_mode.sends() && self.duplex_mic_mode.receives() {
                dpx.set_mic_rx_gain(if self.duplex_mic_muted {
                    0.0
                } else {
                    self.volume
                });
            }
        }
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
        if self.tray.is_some() {
            self.refresh_audio_devices(false);
            if !self.demo_mode && self.active_duplex.is_none() {
                self.trigger_lan_scan_internal(false);
            }
        }
        ctx.request_repaint();
        #[cfg(target_os = "windows")]
        win32_tray::wake_main_window();
    }

    fn sync_tray_status_title(&mut self) {
        let Some(ref tray) = self.tray else {
            return;
        };
        if let Some(ref dpx) = self.active_duplex {
            if self.last_tray_title_update.elapsed() < Duration::from_millis(1000) {
                return;
            }
            self.last_tray_title_update = Instant::now();
            let st = dpx.status();
            let tx_pkts = st.primary_tx().map(|t| t.net.packets_sent).unwrap_or(0);
            let rx_buf = st.primary_rx().map(|r| r.net.buffer_level_ms).unwrap_or(0.0);
            let status_line = format!("● LIVE — TX: {tx_pkts} | Buf: {rx_buf:.1} ms");
            if status_line != self.last_tray_title {
                tray.title_item.set_text(&status_line);
                self.last_tray_title = status_line;
            }
        } else {
            let idle_title = self.lang.texts().tray_title;
            if self.last_tray_title != idle_title {
                tray.title_item.set_text(idle_title);
                self.last_tray_title.clear();
                self.last_tray_title.push_str(idle_title);
            }
        }
    }

    pub fn refresh_audio_devices(&mut self, manual: bool) {
        if self.polling_devices.swap(true, Ordering::Relaxed) {
            return;
        }
        let pending = Arc::clone(&self.pending_devices);
        let polling = Arc::clone(&self.polling_devices);
        thread::spawn(move || {
            let new_devices = list_audio_devices().unwrap_or_default();
            let lan_ip = detect_local_lan_ip();
            if let Ok(mut guard) = pending.lock() {
                *guard = Some((new_devices, lan_ip, manual));
            }
            polling.store(false, Ordering::Relaxed);
            if let Ok(guard) = global_egui_wakeup_ctx().lock() {
                if let Some(ref ctx) = *guard {
                    ctx.request_repaint();
                }
            }
        });
    }

    fn apply_pending_device_updates(&mut self) {
        if let Some(install_res) = self
            .pending_vmic_install_result
            .lock()
            .ok()
            .and_then(|mut g| g.take())
        {
            match install_res {
                Ok(detail) => {
                    self.last_status_msg = self.lang.status_vmic_driver_installed(&detail);
                    self.devices = list_audio_devices().unwrap_or_default();
                    if self.active_duplex.is_some() {
                        self.restart_active_session();
                    }
                }
                Err(err) => {
                    self.last_status_msg = self.lang.status_vmic_driver_failed(&err);
                }
            }
        }

        let maybe_update = self
            .pending_devices
            .lock()
            .ok()
            .and_then(|mut guard| guard.take());
        let Some((new_devices, new_lan_ip, manual)) = maybe_update else {
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
        if !self.demo_mode {
            self.local_lan_ip = new_lan_ip;
        }
        self.sck_permission_granted = has_system_loopback_permission();

        if let Some(ref dpx) = self.active_duplex {
            let st = dpx.status();
            // Auto-bind real Virtual Mic driver if one just became available (pure slice check)
            if !st.virtual_mic_ready
                && select_virtual_mic_output_from_devices(&self.devices).is_some()
            {
                self.restart_active_session();
                return;
            }

            // Auto-follow OS default speaker or microphone on hotplug
            if self.auto_follow_default_device {
                let mut should_hotswap = None;
                if self.duplex_media_rx_device.is_none() {
                    if let (Some(ref m_rx), Some(ref def_out)) = (&st.media_rx, &new_default_out) {
                        if &m_rx.device_name != def_out {
                            should_hotswap = Some(def_out.clone());
                        }
                    }
                }
                if should_hotswap.is_none() && self.duplex_mic_tx_device.is_none() {
                    if let (Some(ref v_tx), Some(ref def_in)) = (&st.mic_tx, &new_default_in) {
                        if !is_virtual_mic_input_device_name(def_in) && &v_tx.device_name != def_in
                        {
                            should_hotswap = Some(def_in.clone());
                        }
                    }
                }
                if let Some(dev_name) = should_hotswap {
                    self.restart_active_session();
                    self.last_status_msg = self.lang.status_hotswapped_device(&dev_name);
                    return;
                }
            }
        }

        if manual {
            self.last_status_msg = self.lang.status_refreshed_ports(in_count, out_count);
        }
    }

    fn sync_lan_advertiser(&mut self) {
        let port = self.duplex_downlink_port;
        if self.advertise_mdns {
            if let Some(ref mut svc) = self.lan_advertiser {
                if svc.is_advertising() {
                    svc.set_advertised_port(port);
                } else {
                    let _ = svc.advertise("rsrelay-desktop", port);
                }
            } else if let Ok(mut svc) = DiscoveryService::new() {
                let _ = svc.advertise("rsrelay-desktop", port);
                self.lan_advertiser = Some(svc);
            }
        } else if let Some(ref mut svc) = self.lan_advertiser {
            // Unregister mDNS & stop UDP responder while keeping the underlying ServiceDaemon
            // alive for zero-churn reuse on future scans or re-advertisement.
            svc.unadvertise();
        }
    }

    pub fn trigger_lan_scan(&mut self) {
        self.trigger_lan_scan_internal(true);
    }

    fn trigger_lan_scan_internal(&mut self, manual: bool) {
        if self.scanning_lan.swap(true, Ordering::Relaxed) {
            return;
        }
        self.last_lan_scan = Instant::now();
        if manual {
            self.last_status_msg = self.lang.status_scanning_lan();
        }
        if self.lan_advertiser.is_none() {
            self.lan_advertiser = DiscoveryService::new().ok();
        }
        let scanner = self
            .lan_advertiser
            .as_ref()
            .map(|svc| svc.scanner())
            .unwrap_or_else(DiscoveryScanner::udp_only);
        let peers_arc = Arc::clone(&self.discovered_peers);
        let scanning_flag = Arc::clone(&self.scanning_lan);

        thread::spawn(move || {
            let local_ips: Vec<IpAddr> = detect_all_local_ipv4s()
                .into_iter()
                .map(IpAddr::V4)
                .collect();
            if let Ok(found) = scanner.scan(Duration::from_millis(900)) {
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
            scanning_flag.store(false, Ordering::Relaxed);
            if let Ok(guard) = global_egui_wakeup_ctx().lock() {
                if let Some(ref ctx) = *guard {
                    ctx.request_repaint();
                }
            }
        });
    }

    fn apply_auto_discovered_target(&mut self) {
        if self.demo_mode || self.target_addr_manually_edited || self.active_duplex.is_some() {
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

    pub fn build_target_combo_options(&self) -> Vec<(String, String, bool)> {
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
            let addr_val = format!("{lan_ip}:{}", self.duplex_downlink_port);
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
        self.active_duplex = None;
        self.duplex_started_at = None;
        self.duplex_buffer_sparkline.clear();
        self.duplex_tx_envelope_sparkline.clear();
        self.last_status_msg = self.lang.status_all_stopped();
    }

    fn restart_active_session(&mut self) {
        if self.active_duplex.is_some() {
            let prev_started = self.duplex_started_at;
            self.active_duplex = None;
            if self.duplex_media_mode == MediaBusMode::Off
                && self.duplex_mic_mode == MicBusMode::Off
            {
                self.duplex_started_at = None;
                self.last_status_msg = self.lang.status_all_stopped();
                return;
            }
            self.start_link_session_internal();
            if self.active_duplex.is_some() && prev_started.is_some() {
                self.duplex_started_at = prev_started;
            }
        }
    }

    pub fn trigger_install_virtual_mic_driver(&mut self) {
        if self.installing_vmic_driver.swap(true, Ordering::Relaxed) {
            return;
        }
        self.last_status_msg = self.lang.status_installing_vmic_driver();
        let flag = Arc::clone(&self.installing_vmic_driver);
        let result_slot = Arc::clone(&self.pending_vmic_install_result);
        thread::spawn(move || {
            let res = if cfg!(target_os = "macos") {
                install_macos_virtual_mic_driver()
            } else {
                launch_windows_vbcable_install()
            };
            if let Ok(mut guard) = result_slot.lock() {
                *guard = Some(res);
            }
            flag.store(false, Ordering::Relaxed);
        });
    }

    pub fn toggle_duplex(&mut self) {
        if self.active_duplex.is_some() {
            self.stop_all_streams();
            return;
        }
        self.start_link_session_internal();
    }

    fn start_link_session_internal(&mut self) {
        if self.duplex_media_mode == MediaBusMode::Off && self.duplex_mic_mode == MicBusMode::Off {
            self.last_status_msg = self.lang.status_both_buses_off();
            return;
        }

        if let Ok(p) = self.media_port_input.trim().parse::<u16>() {
            if p > 0 {
                self.duplex_downlink_port = p;
            }
        }
        self.media_port_input = self.duplex_downlink_port.to_string();

        if let Ok(p) = self.mic_port_input.trim().parse::<u16>() {
            if p > 0 {
                self.duplex_uplink_port = p;
            }
        }
        self.mic_port_input = self.duplex_uplink_port.to_string();

        let trimmed_peer = self.duplex_peer_ip_input.trim();
        let peer_ip_res: Result<IpAddr, std::net::AddrParseError> = if trimmed_peer.is_empty() {
            Ok(IpAddr::from([127, 0, 0, 1]))
        } else {
            trimmed_peer.parse::<IpAddr>().or_else(|_| {
                parse_socket_addr_flexible(
                    trimmed_peer,
                    IpAddr::from([127, 0, 0, 1]),
                    self.duplex_downlink_port,
                )
                .map(|sa| sa.ip())
            })
        };

        let peer_ip: IpAddr = match peer_ip_res {
            Ok(ip) => {
                self.duplex_peer_ip_input = ip.to_string();
                self.target_addr_input = format!("{ip}:{}", self.duplex_downlink_port);
                ip
            }
            Err(e) => {
                self.last_status_msg = self.lang.status_invalid_peer_ip(&e);
                return;
            }
        };

        self.sync_role_from_bus_modes();
        self.sync_lan_advertiser();

        let mut cfg = DuplexSessionConfig::new(self.duplex_role, peer_ip);
        cfg.media_mode = self.duplex_media_mode;
        cfg.mic_mode = self.duplex_mic_mode;
        cfg.downlink_udp_port = self.duplex_downlink_port;
        cfg.uplink_udp_port = self.duplex_uplink_port;
        cfg.media_tx_device = self.duplex_media_tx_device.clone();
        cfg.media_rx_device = self.duplex_media_rx_device.clone();
        cfg.mic_tx_device = self.duplex_mic_tx_device.clone();
        cfg.mic_rx_device = self.duplex_mic_rx_device.clone();
        cfg.media_channels = self.duplex_rx_channels;
        cfg.mic_channels = ChannelLayout::Stereo;
        cfg.profile = self.selected_profile;
        cfg.mic_noise_gate_db = Some(self.duplex_gate_db);
        cfg.initial_volume = if self.speaker_muted { 0.0 } else { self.volume };
        cfg.mic_rx_gain = if self.duplex_mic_muted && !self.duplex_mic_mode.sends() {
            0.0
        } else {
            self.volume
        };
        cfg.advertise_mdns = self.advertise_mdns
            && !self
                .lan_advertiser
                .as_ref()
                .map(|s| s.is_advertising())
                .unwrap_or(false);
        cfg.synthetic_source_override = self.use_test_tone;
        cfg.target_buffer_ms_override = if self.custom_buffer_enabled {
            Some(self.target_buffer_ms)
        } else {
            None
        };
        cfg.media_opus_bitrate_bps_override = Some((self.opus_bitrate_kbps as u32) * 1000);

        match DuplexSession::start(cfg) {
            Ok(session) => {
                self.sck_permission_granted = has_system_loopback_permission();
                session.set_tx_muted(self.duplex_mic_muted);
                if !self.duplex_media_mode.receives() && self.duplex_media_mode.sends() {
                    session.set_media_tx_muted(self.speaker_muted);
                }
                let st = session.status();
                let mut parts = Vec::new();
                if let Some(ref rx) = st.media_rx {
                    parts.push(format!("🎧 {}", rx.device_name));
                } else if let Some(ref tx) = st.media_tx {
                    parts.push(format!("🔊 {}", tx.device_name));
                }
                if let Some(ref tx) = st.mic_tx {
                    parts.push(format!("🎙️ {}", tx.device_name));
                } else if let Some(ref rx) = st.mic_rx {
                    parts.push(format!("🎤 {}", rx.device_name));
                }
                let summary = parts.join(" · ");
                self.duplex_started_at = Some(Instant::now());
                self.duplex_buffer_sparkline.clear();
                self.duplex_tx_envelope_sparkline.clear();
                self.last_status_msg = self.lang.status_link_active(peer_ip, &summary);
                self.active_duplex = Some(session);
            }
            Err(e) => {
                self.sck_permission_granted = has_system_loopback_permission();
                self.last_status_msg = self.lang.status_link_failed(&e);
            }
        }
    }

    pub fn enable_demo_preview(&mut self) {
        self.demo_mode = true;
        self.local_lan_ip = "192.168.3.151".parse().ok();
        self.target_addr_input = "192.168.3.110:49153".to_string();
        self.duplex_peer_ip_input = "192.168.3.110".to_string();
        self.selected_profile = LatencyProfile::Balanced;
        if let Ok(mut guard) = self.discovered_peers.lock() {
            *guard = vec![
                DiscoveredPeer {
                    instance_name: "Studio-PC".to_string(),
                    hostname: "Studio-PC".to_string(),
                    addresses: vec!["192.168.3.110".parse().unwrap()],
                    udp_port: 49153,
                    version: "0.4.2".to_string(),
                    os: "windows".to_string(),
                },
                DiscoveredPeer {
                    instance_name: "MacBook-Air".to_string(),
                    hostname: "MacBook-Air".to_string(),
                    addresses: vec!["192.168.3.128".parse().unwrap()],
                    udp_port: 49153,
                    version: "0.4.2".to_string(),
                    os: "macos".to_string(),
                },
                DiscoveredPeer {
                    instance_name: "Kevins-iPad".to_string(),
                    hostname: "Kevin's iPad".to_string(),
                    addresses: vec!["192.168.3.142".parse().unwrap()],
                    udp_port: 49153,
                    version: "0.4.2".to_string(),
                    os: "ios".to_string(),
                },
            ];
        }

        self.duplex_rx_vu_meter.smoothed_db = -18.4;
        self.duplex_rx_vu_meter.peak_hold_db = -13.2;
        self.duplex_rx_vu_meter.smoothed_db_r = -20.1;
        self.duplex_rx_vu_meter.peak_hold_db_r = -14.8;

        self.duplex_tx_vu_meter.smoothed_db = -14.2;
        self.duplex_tx_vu_meter.peak_hold_db = -10.8;
        self.duplex_tx_vu_meter.smoothed_db_r = -15.6;
        self.duplex_tx_vu_meter.peak_hold_db_r = -11.9;

        for i in 0..64 {
            let jb = 18.5 + 1.4 * ((i as f32 * 0.18).sin()) + 0.6 * ((i as f32 * 0.43).cos());
            self.duplex_buffer_sparkline.push(jb);
            let env = if (10..26).contains(&i) || (36..56).contains(&i) {
                -17.0 + 5.5 * ((i as f32 * 0.45).sin())
            } else {
                -54.0
            };
            self.duplex_tx_envelope_sparkline.push(env);
        }
    }

    fn update_live_telemetry(&mut self) {
        if self.demo_mode && self.active_duplex.is_none() {
            return;
        }

        if let Some(ref dpx) = self.active_duplex {
            let st = dpx.status();
            let media_peak = st
                .media_rx
                .as_ref()
                .map(|r| r.peak_level)
                .or_else(|| st.media_tx.as_ref().map(|t| t.peak_level))
                .unwrap_or(0.0);
            let media_active = st.media_rx.is_some() || st.media_tx.is_some();
            self.duplex_rx_vu_meter.update(media_peak, media_active);

            if let Some(rx_for_jb) = st.media_rx.as_ref().or(st.mic_rx.as_ref()) {
                if rx_for_jb.net.packets_received > 0 {
                    self.duplex_buffer_sparkline.push(rx_for_jb.net.buffer_level_ms);
                }
            }

            let mic_peak = st
                .mic_tx
                .as_ref()
                .map(|t| t.peak_level)
                .unwrap_or(0.0)
                .max(st.mic_rx.as_ref().map(|r| r.peak_level).unwrap_or(0.0));
            let mic_active = st.mic_tx.is_some() || st.mic_rx.is_some();
            self.duplex_tx_vu_meter.update(mic_peak, mic_active);

            let active_env_peak = if mic_active { mic_peak } else { media_peak };
            let env_db = if self.duplex_mic_muted && st.mic_rx.is_none() && mic_active {
                -58.0
            } else {
                linear_to_dbfs(active_env_peak).max(-56.0)
            };
            self.duplex_tx_envelope_sparkline.push(env_db);
        } else {
            self.duplex_tx_vu_meter.update(0.0, false);
            self.duplex_rx_vu_meter.update(0.0, false);
        }
    }

    fn resolve_peer_name_and_icon(&self, ip_part: &str) -> (String, IconKind) {
        if let Ok(guard) = self.discovered_peers.lock() {
            for p in guard.iter() {
                if p.addresses.iter().any(|a| a.to_string() == ip_part) {
                    let os_lower = p.os.to_ascii_lowercase();
                    let icon = if os_lower.contains("mac") || os_lower.contains("darwin") {
                        IconKind::Laptop
                    } else if os_lower.contains("ios") || os_lower.contains("android") {
                        IconKind::Mobile
                    } else {
                        IconKind::Desktop
                    };
                    return (p.hostname.clone(), icon);
                }
            }
        }
        if ip_part == "127.0.0.1" || ip_part == "localhost" {
            let lbl = match self.lang {
                Language::Zh => "本机回环测试".to_string(),
                Language::En => "Localhost".to_string(),
            };
            (lbl, IconKind::Laptop)
        } else {
            (ip_part.to_string(), IconKind::Desktop)
        }
    }

    fn app_recording_mic_name_for_sink(sink_name: &str) -> String {
        let lower = sink_name.to_ascii_lowercase();
        if lower.contains("rsrelay virtual mic") {
            MACOS_RSRELAY_VMIC_INPUT_NAME.to_string()
        } else if lower.contains("cable input") {
            sink_name.replace("CABLE Input", "CABLE Output")
        } else if lower.contains("voicemeeter input") {
            "VoiceMeeter Out B1".to_string()
        } else {
            sink_name.to_string()
        }
    }

    // =========================================================================
    // Left Navigation Sidebar
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
                // Bottom Footer Panel inside Sidebar
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
                        let ip_str = self
                            .local_lan_ip
                            .map(|ip| ip.to_string())
                            .unwrap_or_else(|| "127.0.0.1".to_string());
                        {
                            let (ip_row, _) = ui.allocate_exact_size(
                                Vec2::new(ui.available_width(), 36.0),
                                Sense::hover(),
                            );
                            let cy = ip_row.center().y;
                            paint_text_vcenter(
                                ui.painter(),
                                Pos2::new(ip_row.min.x, cy - 8.5),
                                Align::Min,
                                t.sidebar_local_addr,
                                FontId::new(11.0, FontFamily::Proportional),
                                palette::TEXT_MUTED(),
                            );
                            paint_text_vcenter(
                                ui.painter(),
                                Pos2::new(ip_row.min.x, cy + 8.5),
                                Align::Min,
                                &ip_str,
                                FontId::new(12.5, FontFamily::Monospace),
                                palette::TEXT_PRIMARY(),
                            );
                            let copy_rect = Rect::from_center_size(
                                Pos2::new(ip_row.max.x - 12.0, cy),
                                Vec2::splat(24.0),
                            );
                            let copy_resp = ui.interact(
                                copy_rect,
                                ui.id().with("sidebar_copy_lan_ip"),
                                Sense::click(),
                            );
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
                        }

                        ui.add_space(10.0);

                        {
                            let (auto_row, _) = ui.allocate_exact_size(
                                Vec2::new(ui.available_width(), 22.0),
                                Sense::hover(),
                            );
                            paint_text_vcenter(
                                ui.painter(),
                                Pos2::new(auto_row.min.x, auto_row.center().y),
                                Align::Min,
                                t.chk_auto_follow_hotplug,
                                FontId::new(12.5, FontFamily::Proportional),
                                palette::TEXT_SECONDARY(),
                            );
                            let mut toggle_ui = ui.new_child(
                                egui::UiBuilder::new()
                                    .max_rect(auto_row)
                                    .layout(egui::Layout::right_to_left(Align::Center)),
                            );
                            pill_toggle(&mut toggle_ui, &mut self.auto_follow_default_device);
                        }

                        ui.add_space(10.0);

                        // Theme Mode 3-Way Segmented Control
                        {
                            let theme_seg_w = ui.available_width().max(150.0);
                            let (t_rect, _) =
                                ui.allocate_exact_size(Vec2::new(theme_seg_w, 28.0), Sense::hover());
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
                                    paint_text_vcenter(
                                        p,
                                        opt_rect.center(),
                                        Align::Center,
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

                        // Language Segmented Control + Minimize to Tray Button
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
                                    paint_text_vcenter(
                                        p,
                                        opt_rect.center(),
                                        Align::Center,
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

                // Upper Sidebar Content (Brand, Streamlined 2-Tab Nav, Nearby Devices)
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
                        {
                            let (brand_row, _) = ui.allocate_exact_size(
                                Vec2::new(ui.available_width(), 32.0),
                                Sense::hover(),
                            );
                            let cy = brand_row.center().y;
                            let mark_rect = Rect::from_center_size(
                                Pos2::new(brand_row.min.x + 20.0, cy),
                                Vec2::splat(28.0),
                            );
                            if ui.is_rect_visible(brand_row) {
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
                                paint_text_vcenter(
                                    p,
                                    Pos2::new(mark_rect.max.x + 10.0, cy - 7.5),
                                    Align::Min,
                                    "rsrelay",
                                    FontId::new(15.0, FontFamily::Proportional),
                                    palette::TEXT_PRIMARY(),
                                );
                                paint_text_vcenter(
                                    p,
                                    Pos2::new(mark_rect.max.x + 10.0, cy + 8.0),
                                    Align::Min,
                                    concat!("v", env!("CARGO_PKG_VERSION")),
                                    FontId::new(11.0, FontFamily::Monospace),
                                    palette::TEXT_MUTED(),
                                );
                            }
                        }

                        ui.add_space(20.0);

                        {
                            let (sec_row, _) = ui.allocate_exact_size(
                                Vec2::new(ui.available_width(), 18.0),
                                Sense::hover(),
                            );
                            paint_text_vcenter(
                                ui.painter(),
                                Pos2::new(sec_row.min.x + 8.0, sec_row.center().y),
                                Align::Min,
                                t.nav_section_mode,
                                FontId::new(11.0, FontFamily::Proportional),
                                palette::TEXT_MUTED(),
                            );
                        }
                        ui.add_space(4.0);

                        let link_live = self.active_duplex.is_some() || self.demo_mode;
                        let nav_items = [
                            (
                                WorkspaceTab::AudioLink,
                                IconKind::Headset,
                                t.tab_audio_link,
                                link_live,
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
                                Vec2::new(ui.available_width(), 35.0),
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
                                let cy = nav_rect.center().y;
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
                                    Pos2::new(nav_rect.min.x + 18.0, cy),
                                    Vec2::splat(16.0),
                                );
                                draw_vector_icon(p, icon_rect, icon, fg);
                                paint_text_vcenter(
                                    p,
                                    Pos2::new(nav_rect.min.x + 36.0, cy),
                                    Align::Min,
                                    label,
                                    FontId::new(13.0, FontFamily::Proportional),
                                    fg,
                                );
                                if is_live {
                                    let dot_pos = Pos2::new(nav_rect.max.x - 13.0, cy);
                                    p.circle_filled(dot_pos, 6.0, palette::ACCENT_TINT());
                                    p.circle_filled(dot_pos, 3.0, palette::ACCENT());
                                }
                            }
                            ui.add_space(2.0);
                        }

                        ui.add_space(22.0);

                        // Nearby LAN Peers Header Row (`24px` deterministic height)
                        {
                            let (nearby_hdr, _) = ui.allocate_exact_size(
                                Vec2::new(ui.available_width(), 24.0),
                                Sense::hover(),
                            );
                            let cy = nearby_hdr.center().y;
                            paint_text_vcenter(
                                ui.painter(),
                                Pos2::new(nearby_hdr.min.x + 8.0, cy),
                                Align::Min,
                                t.nav_section_nearby,
                                FontId::new(11.0, FontFamily::Proportional),
                                palette::TEXT_MUTED(),
                            );
                            let ref_rect = Rect::from_center_size(
                                Pos2::new(nearby_hdr.max.x - 12.0, cy),
                                Vec2::splat(24.0),
                            );
                            let ref_resp = ui.interact(
                                ref_rect,
                                ui.id().with("sidebar_scan_lan_btn"),
                                Sense::click(),
                            );
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
                                draw_vector_icon(p, ref_rect.shrink(5.0), IconKind::Refresh, col);
                            }
                            if ref_resp.on_hover_text(t.btn_scan_lan_hover).clicked() {
                                self.trigger_lan_scan();
                            }
                        }
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

        let current_ip = self.duplex_peer_ip_input.trim().to_string();

        if peers_snapshot.is_empty() {
            let (peer_name, peer_icon) = self.resolve_peer_name_and_icon(&current_ip);
            if self.draw_sidebar_peer_row(ui, &peer_name, &current_ip, peer_icon, true) {
                self.active_tab = WorkspaceTab::AudioLink;
            }
            if let Some(lan_ip) = self.local_lan_ip {
                let lan_str = lan_ip.to_string();
                if lan_str != current_ip {
                    let local_label = self.lang.texts().combo_local_lan;
                    if self.draw_sidebar_peer_row(ui, local_label, &lan_str, IconKind::Desktop, false)
                    {
                        self.select_peer_ip(&lan_str, local_label);
                    }
                }
            }
            return;
        }

        for peer in &peers_snapshot {
            if let Some(ip) = peer.addresses.first() {
                let ip_str = ip.to_string();
                let is_selected = current_ip == ip_str;
                let os_lower = peer.os.to_ascii_lowercase();
                let icon = if os_lower.contains("mac") || os_lower.contains("darwin") {
                    IconKind::Laptop
                } else if os_lower.contains("ios") || os_lower.contains("android") {
                    IconKind::Mobile
                } else {
                    IconKind::Desktop
                };
                if self.draw_sidebar_peer_row(ui, &peer.hostname, &ip_str, icon, is_selected) {
                    self.select_peer_ip(&ip_str, &peer.hostname);
                }
            }
        }
    }

    fn select_peer_ip(&mut self, ip_str: &str, peer_name: &str) {
        let addr_str = format!("{ip_str}:{}", self.duplex_downlink_port);
        self.duplex_peer_ip_input = ip_str.to_string();
        self.target_addr_input = addr_str.clone();
        self.target_addr_manually_edited = true;
        self.active_tab = WorkspaceTab::AudioLink;
        self.last_status_msg = self.lang.status_selected_lan_peer(peer_name, &addr_str);
        if self.active_duplex.is_some() {
            self.restart_active_session();
        }
    }

    fn draw_sidebar_peer_row(
        &self,
        ui: &mut egui::Ui,
        name: &str,
        ip_str: &str,
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
                p.rect_filled(rect, Rounding::same(8.0), palette::SIDEBAR_HOVER_BG());
            }

            let os_rect = Rect::from_center_size(
                Pos2::new(rect.min.x + 23.0, rect.center().y),
                Vec2::splat(26.0),
            );
            p.rect_filled(os_rect, Rounding::same(7.0), palette::BG_RAISED());
            draw_vector_icon(p, os_rect.shrink(6.0), icon, palette::TEXT_SECONDARY());

            let text_clip = Rect::from_min_max(
                Pos2::new(rect.min.x + 44.0, rect.min.y + 2.0),
                Pos2::new(rect.max.x - 22.0, rect.max.y - 2.0),
            );
            let tp = p.with_clip_rect(text_clip);
            let cy = rect.center().y;
            paint_text_vcenter(
                &tp,
                Pos2::new(rect.min.x + 46.0, cy - 8.0),
                Align::Min,
                name,
                FontId::new(12.5, FontFamily::Proportional),
                palette::TEXT_PRIMARY(),
            );
            paint_text_vcenter(
                &tp,
                Pos2::new(rect.min.x + 46.0, cy + 8.0),
                Align::Min,
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
    // Tab 1: Unified Visual Audio Link (`音频连接`)
    // =========================================================================

    fn render_audio_link_tab(&mut self, ui: &mut egui::Ui) {
        let lang = self.lang;
        let t = lang.texts();
        let link_running = self.active_duplex.is_some() || self.demo_mode;
        let st_opt = self.active_duplex.as_ref().map(|d| d.status());

        // Compute signal health summary for top header pill
        let rtt_ms: Option<f32> = st_opt
            .as_ref()
            .and_then(|st| st.best_rtt_us().map(|us| (us as f32) / 1000.0))
            .or_else(|| if self.demo_mode { Some(0.42) } else { None });
        let total_lost: u64 = st_opt
            .as_ref()
            .map(|st| {
                st.media_rx
                    .as_ref()
                    .map(|r| r.net.packets_lost)
                    .unwrap_or(0)
                    + st.mic_rx.as_ref().map(|r| r.net.packets_lost).unwrap_or(0)
            })
            .unwrap_or(0);
        let signal_label = if !link_running {
            t.signal_idle
        } else if total_lost > 0 {
            t.signal_loss
        } else if rtt_ms.map(|m| m < 18.0).unwrap_or(true) {
            t.signal_excellent
        } else {
            t.signal_good
        };

        // 1. Top Page Header (`48px` deterministic row so left title & right controls share exact `cy`)
        {
            let (hdr_rect, _) =
                ui.allocate_exact_size(Vec2::new(ui.available_width(), 48.0), Sense::hover());
            let hdr_cy = hdr_rect.center().y;
            if ui.is_rect_visible(hdr_rect) {
                let p = ui.painter();
                paint_text_vcenter(
                    p,
                    Pos2::new(hdr_rect.min.x, hdr_cy - 11.5),
                    Align::Min,
                    t.page_link_title,
                    FontId::new(20.0, FontFamily::Proportional),
                    palette::TEXT_PRIMARY(),
                );
                paint_text_vcenter(
                    p,
                    Pos2::new(hdr_rect.min.x, hdr_cy + 12.5),
                    Align::Min,
                    t.page_link_subtitle,
                    FontId::new(13.0, FontFamily::Proportional),
                    palette::TEXT_SECONDARY(),
                );
            }

            let mut right_ui = ui.new_child(
                egui::UiBuilder::new()
                    .max_rect(hdr_rect)
                    .layout(egui::Layout::right_to_left(Align::Center)),
            );
            right_ui.spacing_mut().item_spacing.x = 10.0;
            if header_action_button(&mut right_ui, t.btn_connect, t.btn_disconnect, link_running) {
                if self.demo_mode && self.active_duplex.is_none() {
                    self.demo_mode = false;
                } else {
                    self.toggle_duplex();
                }
            }
            if link_running {
                let elapsed = if self.demo_mode && self.active_duplex.is_none() {
                    "00:12:05".to_string()
                } else {
                    format_elapsed_hms(self.duplex_started_at)
                };
                header_status_chip(&mut right_ui, t.status_connected, Some(&elapsed), true);
            } else {
                header_status_chip(&mut right_ui, t.status_standby, None, false);
            }
            if draw_signal_health_pill(
                &mut right_ui,
                link_running,
                rtt_ms,
                total_lost,
                signal_label,
                t.signal_hover,
            ) {
                self.show_advanced_drawer = !self.show_advanced_drawer;
            }
        }

        ui.add_space(14.0);

        // 2. 4-Column Scene Quick-Preset Strip (1-Click Unified Role & One-Way/Duplex Selection)
        let active_preset =
            ScenePreset::from_bus_modes(self.duplex_media_mode, self.duplex_mic_mode);
        let presets = [
            (
                ScenePreset::Headset,
                "preset_headset",
                IconKind::Headset,
                t.preset_headset_title,
                t.preset_headset_desc,
            ),
            (
                ScenePreset::Host,
                "preset_host",
                IconKind::Desktop,
                t.preset_host_title,
                t.preset_host_desc,
            ),
            (
                ScenePreset::SpeakerOnly,
                "preset_speaker",
                IconKind::SpeakerWave,
                t.preset_speaker_title,
                t.preset_speaker_desc,
            ),
            (
                ScenePreset::MicOnly,
                "preset_mic",
                IconKind::Microphone,
                t.preset_mic_title,
                t.preset_mic_desc,
            ),
        ];

        let mut chosen_preset = None;
        ui.columns(4, |cols| {
            for (idx, &(preset, id_salt, icon, title, desc)) in presets.iter().enumerate() {
                if scene_preset_card(
                    &mut cols[idx],
                    id_salt,
                    icon,
                    title,
                    desc,
                    active_preset == Some(preset),
                ) {
                    chosen_preset = Some(preset);
                }
            }
        });
        if let Some(p) = chosen_preset {
            self.apply_scene_preset(p);
        }

        ui.add_space(14.0);

        // 3. Hero Interactive Visual Dual-Node Signal Topology Card
        self.render_visual_topology_card(ui, link_running, st_opt.as_ref());

        ui.add_space(14.0);

        // 4. Compact Bottom Bar (`30px` deterministic inner rect so label, segments & drawer button share exact `cy`)
        let prev_profile = self.selected_profile;
        egui::Frame::none()
            .fill(palette::BG_PANEL())
            .stroke(Stroke::new(1.0_f32, palette::LINE()))
            .rounding(Rounding::same(metrics::RADIUS_CARD))
            .inner_margin(egui::Margin::symmetric(20.0, 11.0))
            .show(ui, |ui| {
                let (bar_rect, _) =
                    ui.allocate_exact_size(Vec2::new(ui.available_width(), 30.0), Sense::hover());
                let bar_cy = bar_rect.center().y;
                let lbl_font = FontId::new(12.5, FontFamily::Proportional);
                let lbl_w = ui
                    .painter()
                    .layout_no_wrap(
                        t.label_quality_latency.to_string(),
                        lbl_font.clone(),
                        palette::TEXT_SECONDARY(),
                    )
                    .size()
                    .x;
                paint_text_vcenter(
                    ui.painter(),
                    Pos2::new(bar_rect.min.x, bar_cy),
                    Align::Min,
                    t.label_quality_latency,
                    lbl_font,
                    palette::TEXT_SECONDARY(),
                );

                let seg_area = Rect::from_min_max(
                    Pos2::new(bar_rect.min.x + lbl_w + 14.0, bar_rect.min.y),
                    bar_rect.max,
                );
                let mut left_ui = ui.new_child(
                    egui::UiBuilder::new()
                        .max_rect(seg_area)
                        .layout(egui::Layout::left_to_right(Align::Center)),
                );
                latency_profile_selector(&mut left_ui, &mut self.selected_profile, lang);

                let mut right_ui = ui.new_child(
                    egui::UiBuilder::new()
                        .max_rect(bar_rect)
                        .layout(egui::Layout::right_to_left(Align::Center)),
                );
                right_ui.spacing_mut().item_spacing.x = 12.0;
                if drawer_toggle_button(
                    &mut right_ui,
                    self.show_advanced_drawer,
                    t.btn_drawer_open,
                    t.btn_drawer_close,
                ) {
                    self.show_advanced_drawer = !self.show_advanced_drawer;
                }

                if !self.show_advanced_drawer && !self.last_status_msg.is_empty() {
                    let is_err = self.last_status_msg.contains("失败")
                        || self.last_status_msg.contains("无效")
                        || self.last_status_msg.contains("请至少")
                        || self.last_status_msg.contains("Failed")
                        || self.last_status_msg.contains("Invalid")
                        || self.last_status_msg.contains("Enable at least");
                    if is_err {
                        let err_g = right_ui.painter().layout_no_wrap(
                            self.last_status_msg.clone(),
                            FontId::new(11.5, FontFamily::Proportional),
                            palette::STATUS_DANGER(),
                        );
                        let (err_rect, _) = right_ui
                            .allocate_exact_size(Vec2::new(err_g.size().x, 30.0), Sense::hover());
                        crate::widgets::paint_galley_vcenter(
                            right_ui.painter(),
                            err_rect.min.x,
                            bar_cy,
                            err_g,
                            palette::STATUS_DANGER(),
                        );
                    }
                }
            });

        if self.selected_profile != prev_profile && self.active_duplex.is_some() {
            self.restart_active_session();
        }

        // 5. Collapsible Pro Telemetry & Advanced Settings Drawer
        let drawer_anim = ui.ctx().animate_bool_with_time(
            ui.id().with("pro_telemetry_drawer_anim"),
            self.show_advanced_drawer,
            0.16,
        );
        if drawer_anim > 0.01 {
            ui.add_space(14.0 * drawer_anim);
            if self.show_advanced_drawer {
                self.render_advanced_telemetry_drawer(ui, link_running, st_opt.as_ref());
            }
        }
    }

    fn render_visual_topology_card(
        &mut self,
        ui: &mut egui::Ui,
        link_running: bool,
        st_opt: Option<&rsrelay_core::DuplexSessionStatus>,
    ) {
        let lang = self.lang;
        let t = lang.texts();
        let time_secs = ui.ctx().input(|i| i.time);

        let mut restart_requested = false;
        let mut install_vmic_requested = false;

        egui::Frame::none()
            .fill(palette::BG_PANEL())
            .stroke(Stroke::new(1.0_f32, palette::LINE()))
            .rounding(Rounding::same(metrics::RADIUS_CARD))
            .inner_margin(egui::Margin::symmetric(20.0, 18.0))
            .show(ui, |ui| {
                let avail_w = ui.available_width().max(640.0);
                let total_h = 314.0;
                let (topo_rect, _) =
                    ui.allocate_exact_size(Vec2::new(avail_w, total_h), Sense::hover());

                let bridge_w = (topo_rect.width() * 0.25).clamp(166.0, 232.0);
                let node_w = ((topo_rect.width() - bridge_w) * 0.5).max(200.0);

                let left_x0 = topo_rect.min.x;
                let left_x1 = left_x0 + node_w;
                let bridge_x0 = left_x1;
                let bridge_x1 = bridge_x0 + bridge_w;
                let right_x0 = bridge_x1;
                let right_x1 = topo_rect.max.x;

                let header_y0 = topo_rect.min.y;
                let header_y1 = header_y0 + 46.0;
                let slot1_y0 = header_y1 + 12.0;
                let slot1_y1 = slot1_y0 + 120.0;
                let slot2_y0 = slot1_y1 + 16.0;
                let slot2_y1 = slot2_y0 + 120.0;

                // =============================================================
                // BAND 0: Node Headers (Local Machine | Bridge Status | Peer Picker)
                // =============================================================
                let local_hdr_rect =
                    Rect::from_min_max(Pos2::new(left_x0, header_y0), Pos2::new(left_x1, header_y1));
                let bridge_hdr_rect = Rect::from_min_max(
                    Pos2::new(bridge_x0, header_y0),
                    Pos2::new(bridge_x1, header_y1),
                );
                let peer_hdr_rect = Rect::from_min_max(
                    Pos2::new(right_x0, header_y0),
                    Pos2::new(right_x1, header_y1),
                );

                // 0A. Local Node Header
                {
                    let local_ip = self
                        .local_lan_ip
                        .map(|ip| ip.to_string())
                        .unwrap_or_else(|| "127.0.0.1".to_string());
                    let p = ui.painter();
                    p.rect(
                        local_hdr_rect,
                        Rounding::same(10.0),
                        palette::BG_RAISED(),
                        Stroke::new(1.0_f32, palette::LINE()),
                    );
                    let ic_rect = Rect::from_center_size(
                        Pos2::new(local_hdr_rect.min.x + 24.0, local_hdr_rect.center().y),
                        Vec2::splat(28.0),
                    );
                    p.rect_filled(ic_rect, Rounding::same(7.0), palette::BG_PANEL());
                    let local_icon = if cfg!(target_os = "macos") {
                        IconKind::Laptop
                    } else {
                        IconKind::Desktop
                    };
                    draw_vector_icon(p, ic_rect.shrink(6.0), local_icon, palette::TEXT_PRIMARY());

                    let lcy = local_hdr_rect.center().y;
                    paint_text_vcenter(
                        p,
                        Pos2::new(ic_rect.max.x + 10.0, lcy - 8.0),
                        Align::Min,
                        t.node_local_title,
                        FontId::new(13.0, FontFamily::Proportional),
                        palette::TEXT_PRIMARY(),
                    );
                    paint_text_vcenter(
                        p,
                        Pos2::new(ic_rect.max.x + 10.0, lcy + 8.5),
                        Align::Min,
                        &local_ip,
                        FontId::new(11.0, FontFamily::Monospace),
                        palette::TEXT_MUTED(),
                    );

                    let copy_rect = Rect::from_center_size(
                        Pos2::new(local_hdr_rect.max.x - 22.0, local_hdr_rect.center().y),
                        Vec2::splat(26.0),
                    );
                    let copy_resp =
                        ui.interact(copy_rect, ui.id().with("topo_copy_local_ip"), Sense::click());
                    if copy_resp.hovered() {
                        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
                        ui.painter()
                            .rect_filled(copy_rect, Rounding::same(6.0), palette::BG_HOVER());
                    }
                    draw_vector_icon(
                        ui.painter(),
                        copy_rect.shrink(6.0),
                        IconKind::Copy,
                        if copy_resp.hovered() {
                            palette::TEXT_PRIMARY()
                        } else {
                            palette::TEXT_MUTED()
                        },
                    );
                    if copy_resp.on_hover_text(t.lan_ip_hover).clicked() {
                        ui.ctx().copy_text(local_ip);
                        if let Some(ip) = self.local_lan_ip {
                            self.last_status_msg = lang.status_copied_lan_ip(ip);
                        }
                    }
                }

                // 0B. Bridge Center Caption
                {
                    let p = ui.painter();
                    let cap = if link_running {
                        t.bridge_live_caption
                    } else {
                        t.bridge_standby_caption
                    };
                    paint_text_vcenter(
                        p,
                        bridge_hdr_rect.center(),
                        Align::Center,
                        cap,
                        FontId::new(11.5, FontFamily::Proportional),
                        if link_running {
                            palette::ACCENT()
                        } else {
                            palette::TEXT_MUTED()
                        },
                    );
                }

                // 0C. Interactive Target Peer Header Card
                {
                    let live_src_ip = st_opt.and_then(|st| {
                        st.media_rx
                            .as_ref()
                            .and_then(|r| r.net.source_addr)
                            .or_else(|| st.mic_rx.as_ref().and_then(|r| r.net.source_addr))
                            .map(|sa| sa.ip().to_string())
                    });
                    let display_ip = if (self.duplex_peer_ip_input.trim() == "127.0.0.1"
                        || self.duplex_peer_ip_input.trim().is_empty())
                        && live_src_ip.is_some()
                    {
                        live_src_ip.unwrap()
                    } else {
                        self.duplex_peer_ip_input.trim().to_string()
                    };
                    let (peer_name, peer_icon) = self.resolve_peer_name_and_icon(&display_ip);

                    let peer_resp = ui.interact(
                        peer_hdr_rect,
                        ui.id().with("topo_peer_picker_card"),
                        Sense::click(),
                    );
                    if peer_resp.hovered() {
                        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
                    }
                    let peer_popup_id = ui.make_persistent_id("topo_peer_picker_popup");
                    if peer_resp.clicked() {
                        ui.memory_mut(|mem| mem.toggle_popup(peer_popup_id));
                    }

                    let p = ui.painter();
                    let peer_bg = if peer_resp.hovered() {
                        palette::BG_HOVER()
                    } else {
                        palette::BG_RAISED()
                    };
                    let peer_border = if link_running {
                        palette::ACCENT().gamma_multiply(0.45)
                    } else if peer_resp.hovered() {
                        palette::LINE_STRONG()
                    } else {
                        palette::INSET_HIGHLIGHT()
                    };
                    p.rect(
                        peer_hdr_rect,
                        Rounding::same(10.0),
                        peer_bg,
                        Stroke::new(1.0_f32, peer_border),
                    );

                    let ic_rect = Rect::from_center_size(
                        Pos2::new(peer_hdr_rect.min.x + 24.0, peer_hdr_rect.center().y),
                        Vec2::splat(28.0),
                    );
                    p.rect_filled(ic_rect, Rounding::same(7.0), palette::BG_PANEL());
                    draw_vector_icon(p, ic_rect.shrink(6.0), peer_icon, palette::TEXT_PRIMARY());

                    // Online badge dot
                    let badge_pos = Pos2::new(ic_rect.max.x - 1.5, ic_rect.max.y - 1.5);
                    p.circle_filled(badge_pos, 4.8, peer_bg);
                    p.circle_filled(
                        badge_pos,
                        3.2,
                        if link_running {
                            palette::ACCENT()
                        } else {
                            palette::DOT_OFFLINE()
                        },
                    );

                    let cy = peer_hdr_rect.center().y;
                    let text_clip = Rect::from_min_max(
                        Pos2::new(ic_rect.max.x + 10.0, peer_hdr_rect.min.y + 2.0),
                        Pos2::new(peer_hdr_rect.max.x - 28.0, peer_hdr_rect.max.y - 2.0),
                    );
                    let tp = p.with_clip_rect(text_clip);
                    paint_text_vcenter(
                        &tp,
                        Pos2::new(ic_rect.max.x + 10.0, cy - 8.0),
                        Align::Min,
                        format!("{} · {peer_name}", t.node_peer_label),
                        FontId::new(12.5, FontFamily::Proportional),
                        palette::TEXT_PRIMARY(),
                    );
                    paint_text_vcenter(
                        &tp,
                        Pos2::new(ic_rect.max.x + 10.0, cy + 8.5),
                        Align::Min,
                        &display_ip,
                        FontId::new(11.0, FontFamily::Monospace),
                        palette::TEXT_MUTED(),
                    );

                    // Dropdown chevron (symmetric around `cy`)
                    let cx = peer_hdr_rect.max.x - 16.0;
                    let chev = Stroke::new(1.5_f32, palette::TEXT_SECONDARY());
                    p.line_segment(
                        [Pos2::new(cx - 3.8, cy - 1.75), Pos2::new(cx, cy + 1.75)],
                        chev,
                    );
                    p.line_segment(
                        [Pos2::new(cx, cy + 1.75), Pos2::new(cx + 3.8, cy - 1.75)],
                        chev,
                    );

                    let _ = peer_resp.clone().on_hover_text(t.node_peer_click_hint);

                    let combo_opts = self.build_target_combo_options();
                    let mut picked_peer: Option<(String, String)> = None;
                    let mut manual_ip_changed = false;
                    let mut rescan_clicked = false;

                    egui::popup_below_widget(
                        ui,
                        peer_popup_id,
                        &peer_resp,
                        egui::PopupCloseBehavior::CloseOnClickOutside,
                        |ui| {
                            ui.set_min_width(256.0);
                            ui.label(
                                RichText::new(t.label_target_addr)
                                    .size(11.5)
                                    .color(palette::TEXT_MUTED()),
                            );
                            let prev_ip = self.duplex_peer_ip_input.clone();
                            quiet_mono_input_pill(
                                ui,
                                "topo_peer_ip_edit",
                                &mut self.duplex_peer_ip_input,
                            );
                            if self.duplex_peer_ip_input != prev_ip {
                                manual_ip_changed = true;
                            }
                            ui.add_space(4.0);
                            ui.separator();
                            let any_remote = combo_opts.iter().any(|(_, _, r)| *r);
                            if !any_remote {
                                ui.label(
                                    RichText::new(t.combo_no_lan_peers)
                                        .size(11.0)
                                        .color(palette::TEXT_MUTED()),
                                );
                                ui.separator();
                            }
                            for (addr_val, display_lbl, is_remote) in &combo_opts {
                                let (host, _) = split_host_port(addr_val, DEFAULT_AUDIO_UDP_PORT);
                                let selected = self.duplex_peer_ip_input.trim() == host;
                                let prefix = if *is_remote { "● " } else { "○ " };
                                if ui
                                    .selectable_label(
                                        selected,
                                        RichText::new(format!("{prefix}{display_lbl}"))
                                            .size(12.0)
                                            .color(if selected || *is_remote {
                                                palette::ACCENT()
                                            } else {
                                                palette::TEXT_PRIMARY()
                                            }),
                                    )
                                    .clicked()
                                {
                                    picked_peer = Some((host, display_lbl.clone()));
                                    ui.memory_mut(|mem| mem.close_popup());
                                }
                            }
                            ui.separator();
                            if ui.selectable_label(false, t.combo_scan_lan).clicked() {
                                rescan_clicked = true;
                                ui.memory_mut(|mem| mem.close_popup());
                            }
                        },
                    );

                    if manual_ip_changed {
                        self.target_addr_manually_edited = true;
                        let h = self.duplex_peer_ip_input.trim();
                        if !h.is_empty() {
                            self.target_addr_input = format!("{h}:{}", self.duplex_downlink_port);
                        }
                    }
                    if let Some((host, full_lbl)) = picked_peer {
                        let short_name = full_lbl.split(" · ").next().unwrap_or(&host).to_string();
                        self.select_peer_ip(&host, &short_name);
                    }
                    if rescan_clicked {
                        self.trigger_lan_scan();
                    }
                }

                // =============================================================
                // BAND 1: Upper Audio Bus (电脑声音: Speaker / Loopback `:49153`)
                // =============================================================
                let left_media_rect =
                    Rect::from_min_max(Pos2::new(left_x0, slot1_y0), Pos2::new(left_x1, slot1_y1));
                let bridge_media_rect = Rect::from_min_max(
                    Pos2::new(bridge_x0, slot1_y0),
                    Pos2::new(bridge_x1, slot1_y1),
                );
                let right_media_rect = Rect::from_min_max(
                    Pos2::new(right_x0, slot1_y0),
                    Pos2::new(right_x1, slot1_y1),
                );

                let media_mode = self.duplex_media_mode;
                let media_enabled = media_mode != MediaBusMode::Off;
                let media_live = link_running && media_enabled;

                // 1A. Left Local Audio Port Box
                {
                    let border_c = if media_live && !self.speaker_muted {
                        palette::ACCENT().gamma_multiply(0.35)
                    } else {
                        palette::LINE()
                    };
                    ui.painter().rect(
                        left_media_rect,
                        Rounding::same(11.0),
                        palette::BG_RAISED(),
                        Stroke::new(1.0_f32, border_c),
                    );

                    let inner = left_media_rect.shrink2(Vec2::new(13.0, 11.0));
                    let mut child = ui.new_child(
                        egui::UiBuilder::new()
                            .max_rect(inner)
                            .layout(egui::Layout::top_down(Align::Min)),
                    );
                    child.spacing_mut().item_spacing.y = 7.0;

                    child.horizontal(|ui| {
                        ui.spacing_mut().item_spacing.x = 8.0;
                        let mute_icon = if media_mode == MediaBusMode::SendLoopback {
                            IconKind::SpeakerWave
                        } else {
                            IconKind::Headset
                        };
                        let mute_hint = if media_mode == MediaBusMode::SendLoopback {
                            t.mute_loopback_hover
                        } else {
                            t.mute_speaker_hover
                        };
                        if draw_mute_icon_button(
                            ui,
                            "local_media_mute_btn",
                            mute_icon,
                            media_live,
                            self.speaker_muted,
                            mute_hint,
                        ) {
                            self.toggle_speaker_mute();
                        }

                        if media_mode == MediaBusMode::SendLoopback {
                            let default_lpbk = t.c2_host_tx_default_loopback.to_string();
                            let mut lpbk_opts = vec![(None, default_lpbk.clone())];
                            for dev in &self.devices {
                                let eligible = if cfg!(target_os = "windows") {
                                    dev.is_output && !is_virtual_mic_output_sink_name(&dev.name)
                                } else {
                                    dev.is_input && !is_virtual_mic_input_device_name(&dev.name)
                                };
                                if eligible {
                                    lpbk_opts.push((Some(dev.name.clone()), dev.name.clone()));
                                }
                            }
                            let cur_lbl = self
                                .duplex_media_tx_device
                                .clone()
                                .unwrap_or(default_lpbk);
                            if flexible_select_pill(
                                ui,
                                "topo_media_tx_dev",
                                &mut self.duplex_media_tx_device,
                                &cur_lbl,
                                &lpbk_opts,
                            ) {
                                restart_requested = true;
                            }
                        } else {
                            let default_spk = t.device_default_speaker.to_string();
                            let mut spk_opts = vec![(None, default_spk.clone())];
                            for dev in &self.devices {
                                if dev.is_output && !is_virtual_mic_output_sink_name(&dev.name) {
                                    spk_opts.push((Some(dev.name.clone()), dev.name.clone()));
                                }
                            }
                            let cur_lbl =
                                self.duplex_media_rx_device.clone().unwrap_or(default_spk);
                            if flexible_select_pill(
                                ui,
                                "topo_media_rx_dev",
                                &mut self.duplex_media_rx_device,
                                &cur_lbl,
                                &spk_opts,
                            ) {
                                restart_requested = true;
                            }
                        }
                    });

                    // Middle control: Volume slider (when receiving) or macOS SCK permission / format badge
                    if media_mode.receives() {
                        if inline_volume_slider(
                            &mut child,
                            "topo_spk_vol",
                            &mut self.volume,
                            self.speaker_muted,
                            t.status_muted,
                        ) {
                            if self.speaker_muted && self.volume > 0.02 {
                                self.speaker_muted = false;
                            }
                            if let Some(ref dpx) = self.active_duplex {
                                dpx.set_rx_volume(if self.speaker_muted {
                                    0.0
                                } else {
                                    self.volume
                                });
                                dpx.set_mic_rx_gain(self.volume);
                            }
                        }
                    } else if media_mode == MediaBusMode::SendLoopback {
                        child.horizontal(|ui| {
                            if cfg!(target_os = "macos") && !self.sck_permission_granted {
                                if ui
                                    .button(
                                        RichText::new(format!("⚠ {}", t.btn_grant_perm))
                                            .size(11.0)
                                            .color(palette::STATUS_WARN()),
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
                            } else {
                                let spec = match self.selected_profile {
                                    LatencyProfile::LosslessPcm => "PCM16 · 48 kHz · Stereo",
                                    _ => "Opus · 48 kHz · Stereo",
                                };
                                ui.label(
                                    RichText::new(spec)
                                        .size(11.0)
                                        .family(FontFamily::Monospace)
                                        .color(palette::TEXT_MUTED()),
                                );
                            }
                        });
                    } else {
                        child.label(
                            RichText::new(t.flow_off)
                                .size(11.5)
                                .color(palette::TEXT_MUTED()),
                        );
                    }

                    // Bottom live stereo VU meter
                    compact_stereo_vu_meter(
                        &mut child,
                        &self.duplex_rx_vu_meter,
                        media_live,
                        self.speaker_muted,
                    );
                }

                // 1B. Center Animated Signal Conduit (电脑声音)
                {
                    let (media_dir, pill_lbl) = match media_mode {
                        MediaBusMode::ReceiveSpeaker => {
                            (ConduitDirection::RightToLeft, t.flow_media_recv)
                        }
                        MediaBusMode::SendLoopback => {
                            (ConduitDirection::LeftToRight, t.flow_media_send)
                        }
                        MediaBusMode::Bidirectional => {
                            (ConduitDirection::Bidirectional, t.flow_media_bi)
                        }
                        MediaBusMode::Off => (ConduitDirection::Off, t.flow_off),
                    };
                    let media_lvl = dbfs_to_norm(self.duplex_rx_vu_meter.smoothed_db);
                    let pill_resp = draw_conduit_lane(
                        ui,
                        bridge_media_rect,
                        "media_conduit",
                        t.conduit_media_label,
                        pill_lbl,
                        media_dir,
                        link_running,
                        self.speaker_muted,
                        media_lvl,
                        time_secs,
                        t.flow_pill_hover,
                    );

                    let popup_id = ui.make_persistent_id("media_conduit_flow_popup");
                    if pill_resp.clicked() {
                        ui.memory_mut(|mem| mem.toggle_popup(popup_id));
                    }
                    let mut next_media_mode = None;
                    egui::popup_below_widget(
                        ui,
                        popup_id,
                        &pill_resp,
                        egui::PopupCloseBehavior::CloseOnClick,
                        |ui| {
                            ui.set_min_width(156.0);
                            let opts = [
                                (MediaBusMode::ReceiveSpeaker, t.flow_media_recv),
                                (MediaBusMode::SendLoopback, t.flow_media_send),
                                (MediaBusMode::Bidirectional, t.flow_media_bi),
                                (MediaBusMode::Off, t.flow_off),
                            ];
                            for (m_val, m_txt) in opts {
                                let sel = self.duplex_media_mode == m_val;
                                if ui
                                    .selectable_label(
                                        sel,
                                        RichText::new(m_txt).size(12.5).color(if sel {
                                            palette::ACCENT()
                                        } else {
                                            palette::TEXT_PRIMARY()
                                        }),
                                    )
                                    .clicked()
                                    && !sel
                                {
                                    next_media_mode = Some(m_val);
                                }
                            }
                        },
                    );
                    if let Some(m_new) = next_media_mode {
                        self.duplex_media_mode = m_new;
                        self.sync_role_from_bus_modes();
                        restart_requested = true;
                    }
                }

                // 1C. Right Peer Mirror Card (对端声音节点)
                {
                    let (p_icon, p_title, p_desc) = match media_mode {
                        MediaBusMode::ReceiveSpeaker => (
                            IconKind::SpeakerWave,
                            t.peer_media_recv_title,
                            t.peer_media_recv_desc,
                        ),
                        MediaBusMode::SendLoopback => (
                            IconKind::Headset,
                            t.peer_media_send_title,
                            t.peer_media_send_desc,
                        ),
                        MediaBusMode::Bidirectional => (
                            IconKind::SendReceive,
                            t.peer_media_bi_title,
                            t.peer_media_bi_desc,
                        ),
                        MediaBusMode::Off => (
                            IconKind::SpeakerDevice,
                            t.peer_media_off_title,
                            t.peer_media_off_desc,
                        ),
                    };
                    let codec_tag = match self.selected_profile {
                        LatencyProfile::LosslessPcm => "PCM16 · 48kHz",
                        LatencyProfile::UltraLow => "Opus · 5ms",
                        LatencyProfile::Balanced => "Opus · FEC",
                        LatencyProfile::Stable => "Opus · 10ms",
                    };
                    Self::draw_peer_mirror_slot(
                        ui,
                        right_media_rect,
                        p_icon,
                        p_title,
                        p_desc,
                        if media_enabled { Some(codec_tag) } else { None },
                        media_live && !self.speaker_muted,
                        media_enabled,
                    );
                }

                // =============================================================
                // BAND 2: Lower Mic Bus (麦克风语音: Mic / Virtual Mic `:49155`)
                // =============================================================
                let left_mic_rect =
                    Rect::from_min_max(Pos2::new(left_x0, slot2_y0), Pos2::new(left_x1, slot2_y1));
                let bridge_mic_rect = Rect::from_min_max(
                    Pos2::new(bridge_x0, slot2_y0),
                    Pos2::new(bridge_x1, slot2_y1),
                );
                let right_mic_rect = Rect::from_min_max(
                    Pos2::new(right_x0, slot2_y0),
                    Pos2::new(right_x1, slot2_y1),
                );

                let mic_mode = self.duplex_mic_mode;
                let mic_enabled = mic_mode != MicBusMode::Off;
                let mic_live = link_running && mic_enabled;
                let auto_vmic = if mic_mode == MicBusMode::ReceiveVirtualMic {
                    select_virtual_mic_output_from_devices(&self.devices)
                } else {
                    None
                };

                // 2A. Left Local Mic / Virtual Mic Port Box
                {
                    let border_c = if mic_live && !self.duplex_mic_muted {
                        palette::ACCENT().gamma_multiply(0.35)
                    } else {
                        palette::LINE()
                    };
                    ui.painter().rect(
                        left_mic_rect,
                        Rounding::same(11.0),
                        palette::BG_RAISED(),
                        Stroke::new(1.0_f32, border_c),
                    );

                    let inner = left_mic_rect.shrink2(Vec2::new(13.0, 11.0));
                    let mut child = ui.new_child(
                        egui::UiBuilder::new()
                            .max_rect(inner)
                            .layout(egui::Layout::top_down(Align::Min)),
                    );
                    child.spacing_mut().item_spacing.y = 8.0;

                    child.horizontal(|ui| {
                        ui.spacing_mut().item_spacing.x = 8.0;
                        if draw_mute_icon_button(
                            ui,
                            "local_mic_mute_btn",
                            IconKind::Microphone,
                            mic_live,
                            self.duplex_mic_muted,
                            t.mute_mic_hover,
                        ) {
                            self.toggle_mic_mute();
                        }

                        if mic_mode == MicBusMode::ReceiveVirtualMic {
                            let default_vmic_lbl = if let Some(ref vname) = auto_vmic {
                                format!("{} ({vname})", t.c2_host_rx_auto_vmic)
                            } else {
                                t.c2_vmic_guard_option.to_string()
                            };
                            let mut vmic_opts = vec![(None, default_vmic_lbl.clone())];
                            for dev in &self.devices {
                                if dev.is_output && is_virtual_mic_output_sink_name(&dev.name) {
                                    vmic_opts.push((Some(dev.name.clone()), dev.name.clone()));
                                }
                            }
                            let cur_vmic = self
                                .duplex_mic_rx_device
                                .clone()
                                .unwrap_or(default_vmic_lbl);
                            if flexible_select_pill(
                                ui,
                                "topo_mic_rx_dev",
                                &mut self.duplex_mic_rx_device,
                                &cur_vmic,
                                &vmic_opts,
                            ) {
                                restart_requested = true;
                            }
                        } else {
                            let default_mic = t.device_system_default.to_string();
                            let mut mic_opts = vec![(None, default_mic.clone())];
                            for dev in &self.devices {
                                if dev.is_input && !is_virtual_mic_input_device_name(&dev.name) {
                                    mic_opts.push((Some(dev.name.clone()), dev.name.clone()));
                                }
                            }
                            let cur_mic =
                                self.duplex_mic_tx_device.clone().unwrap_or(default_mic);
                            if flexible_select_pill(
                                ui,
                                "topo_mic_tx_dev",
                                &mut self.duplex_mic_tx_device,
                                &cur_mic,
                                &mic_opts,
                            ) {
                                restart_requested = true;
                            }
                        }
                    });

                    // Bottom area: Draggable Gate VU meter (when sending mic) OR Virtual Mic status + meter (when receiving VMic)
                    if mic_mode.sends() {
                        if draw_interactive_gate_vu_meter(
                            &mut child,
                            "topo_mic_gate_vu",
                            &self.duplex_tx_vu_meter,
                            mic_live,
                            self.duplex_mic_muted,
                            &mut self.duplex_gate_db,
                            t.c2_gate_marker_prefix,
                            t.gate_drag_hover,
                        ) {
                            if let Some(ref dpx) = self.active_duplex {
                                dpx.set_tx_noise_gate_db(Some(self.duplex_gate_db));
                            }
                        }
                    } else if mic_mode == MicBusMode::ReceiveVirtualMic {
                        let effective_vmic = self
                            .duplex_mic_rx_device
                            .clone()
                            .or_else(|| auto_vmic.clone());
                        let is_installing = self.installing_vmic_driver.load(Ordering::Relaxed);

                        child.horizontal(|ui| {
                            if let Some(ref sink_name) = effective_vmic {
                                let app_mic = Self::app_recording_mic_name_for_sink(sink_name);
                                ui.label(
                                    RichText::new(format!(
                                        "✓ {}{}",
                                        t.c2_vmic_ready_prefix, app_mic
                                    ))
                                    .size(11.0)
                                    .color(palette::ACCENT()),
                                );
                            } else {
                                ui.label(
                                    RichText::new(t.c2_vmic_missing_warn)
                                        .size(10.5)
                                        .color(palette::STATUS_WARN()),
                                );
                                ui.with_layout(egui::Layout::right_to_left(Align::Center), |ui| {
                                    let btn_txt = if is_installing {
                                        t.btn_installing_vmic
                                    } else if cfg!(target_os = "macos") {
                                        t.btn_install_macos_vmic
                                    } else {
                                        t.btn_install_win_vbcable
                                    };
                                    if ui
                                        .add_enabled(
                                            !is_installing,
                                            egui::Button::new(
                                                RichText::new(btn_txt)
                                                    .size(11.0)
                                                    .color(palette::TEXT_PRIMARY()),
                                            ),
                                        )
                                        .clicked()
                                    {
                                        install_vmic_requested = true;
                                    }
                                });
                            }
                        });

                        compact_stereo_vu_meter(
                            &mut child,
                            &self.duplex_tx_vu_meter,
                            mic_live,
                            self.duplex_mic_muted,
                        );
                    } else {
                        child.label(
                            RichText::new(t.flow_off)
                                .size(11.5)
                                .color(palette::TEXT_MUTED()),
                        );
                        compact_stereo_vu_meter(&mut child, &self.duplex_tx_vu_meter, false, false);
                    }
                }

                // 2B. Center Animated Signal Conduit (麦克风语音)
                {
                    let (mic_dir, pill_lbl) = match mic_mode {
                        MicBusMode::SendMic => (ConduitDirection::LeftToRight, t.flow_mic_send),
                        MicBusMode::ReceiveVirtualMic => {
                            (ConduitDirection::RightToLeft, t.flow_mic_recv)
                        }
                        MicBusMode::Bidirectional => {
                            (ConduitDirection::Bidirectional, t.flow_mic_bi)
                        }
                        MicBusMode::Off => (ConduitDirection::Off, t.flow_off),
                    };
                    // Only pulse bright beads when voice is above noise gate (when sending)
                    let voice_above_gate = !mic_mode.sends()
                        || self.duplex_tx_vu_meter.smoothed_db >= self.duplex_gate_db;
                    let mic_lvl = if voice_above_gate {
                        dbfs_to_norm(self.duplex_tx_vu_meter.smoothed_db)
                    } else {
                        0.0
                    };
                    let pill_resp = draw_conduit_lane(
                        ui,
                        bridge_mic_rect,
                        "mic_conduit",
                        t.conduit_mic_label,
                        pill_lbl,
                        mic_dir,
                        link_running && voice_above_gate,
                        self.duplex_mic_muted,
                        mic_lvl,
                        time_secs + 0.33,
                        t.flow_pill_hover,
                    );

                    let popup_id = ui.make_persistent_id("mic_conduit_flow_popup");
                    if pill_resp.clicked() {
                        ui.memory_mut(|mem| mem.toggle_popup(popup_id));
                    }
                    let mut next_mic_mode = None;
                    egui::popup_below_widget(
                        ui,
                        popup_id,
                        &pill_resp,
                        egui::PopupCloseBehavior::CloseOnClick,
                        |ui| {
                            ui.set_min_width(156.0);
                            let opts = [
                                (MicBusMode::SendMic, t.flow_mic_send),
                                (MicBusMode::ReceiveVirtualMic, t.flow_mic_recv),
                                (MicBusMode::Bidirectional, t.flow_mic_bi),
                                (MicBusMode::Off, t.flow_off),
                            ];
                            for (v_val, v_txt) in opts {
                                let sel = self.duplex_mic_mode == v_val;
                                if ui
                                    .selectable_label(
                                        sel,
                                        RichText::new(v_txt).size(12.5).color(if sel {
                                            palette::ACCENT()
                                        } else {
                                            palette::TEXT_PRIMARY()
                                        }),
                                    )
                                    .clicked()
                                    && !sel
                                {
                                    next_mic_mode = Some(v_val);
                                }
                            }
                        },
                    );
                    if let Some(v_new) = next_mic_mode {
                        self.duplex_mic_mode = v_new;
                        self.sync_role_from_bus_modes();
                        restart_requested = true;
                    }
                }

                // 2C. Right Peer Mirror Card (对端麦克风节点)
                {
                    let (p_icon, p_title, p_desc) = match mic_mode {
                        MicBusMode::SendMic => (
                            IconKind::Microphone,
                            t.peer_mic_send_title,
                            t.peer_mic_send_desc,
                        ),
                        MicBusMode::ReceiveVirtualMic => (
                            IconKind::Headset,
                            t.peer_mic_recv_title,
                            t.peer_mic_recv_desc,
                        ),
                        MicBusMode::Bidirectional => (
                            IconKind::SendReceive,
                            t.peer_mic_bi_title,
                            t.peer_mic_bi_desc,
                        ),
                        MicBusMode::Off => (
                            IconKind::Microphone,
                            t.peer_mic_off_title,
                            t.peer_mic_off_desc,
                        ),
                    };
                    let mic_tag = if mic_mode.sends() {
                        Some("Opus 64k · DTX")
                    } else if mic_mode.receives() {
                        Some("Zero-Echo VMic")
                    } else {
                        None
                    };
                    Self::draw_peer_mirror_slot(
                        ui,
                        right_mic_rect,
                        p_icon,
                        p_title,
                        p_desc,
                        mic_tag,
                        mic_live && !self.duplex_mic_muted,
                        mic_enabled,
                    );
                }
            });

        if install_vmic_requested {
            self.trigger_install_virtual_mic_driver();
        }
        if restart_requested && self.active_duplex.is_some() {
            self.restart_active_session();
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn draw_peer_mirror_slot(
        ui: &mut egui::Ui,
        rect: Rect,
        icon: IconKind,
        title: &str,
        desc: &str,
        mono_badge: Option<&str>,
        is_live: bool,
        is_enabled: bool,
    ) {
        let border_c = if is_live {
            palette::ACCENT().gamma_multiply(0.35)
        } else {
            palette::LINE()
        };
        let p = ui.painter();
        p.rect(
            rect,
            Rounding::same(11.0),
            palette::BG_RAISED(),
            Stroke::new(1.0_f32, border_c),
        );

        let inner = rect.shrink2(Vec2::new(14.0, 12.0));
        let mut child = ui.new_child(
            egui::UiBuilder::new()
                .max_rect(inner)
                .layout(egui::Layout::top_down(Align::Min)),
        );
        child.spacing_mut().item_spacing.y = 8.0;

        {
            let (hdr_rect, _) = child.allocate_exact_size(
                Vec2::new(child.available_width(), 32.0),
                Sense::hover(),
            );
            let hcy = hdr_rect.center().y;
            let ic_rect =
                Rect::from_center_size(Pos2::new(hdr_rect.min.x + 16.0, hcy), Vec2::splat(32.0));
            let p = child.painter();
            p.rect_filled(
                ic_rect,
                Rounding::same(8.0),
                if is_live {
                    palette::ACCENT_TINT()
                } else {
                    palette::BG_PANEL()
                },
            );
            draw_vector_icon(
                p,
                ic_rect.shrink(7.5),
                icon,
                if is_live {
                    palette::ACCENT()
                } else if is_enabled {
                    palette::TEXT_SECONDARY()
                } else {
                    palette::TEXT_MUTED()
                },
            );

            paint_text_vcenter(
                p,
                Pos2::new(ic_rect.max.x + 10.0, hcy),
                Align::Min,
                title,
                FontId::new(13.0, FontFamily::Proportional),
                if is_enabled {
                    palette::TEXT_PRIMARY()
                } else {
                    palette::TEXT_MUTED()
                },
            );

            if let Some(badge) = mono_badge {
                paint_text_vcenter(
                    p,
                    Pos2::new(hdr_rect.max.x, hcy),
                    Align::Max,
                    badge,
                    FontId::new(10.5, FontFamily::Monospace),
                    if is_live {
                        palette::ACCENT()
                    } else {
                        palette::TEXT_MUTED()
                    },
                );
            }
        }

        child.label(
            RichText::new(desc).size(11.5).color(if is_enabled {
                palette::TEXT_SECONDARY()
            } else {
                palette::TEXT_MUTED()
            }),
        );
    }

    fn render_advanced_telemetry_drawer(
        &mut self,
        ui: &mut egui::Ui,
        link_running: bool,
        st_opt: Option<&rsrelay_core::DuplexSessionStatus>,
    ) {
        let lang = self.lang;
        let t = lang.texts();
        let effective_buf_ms = if self.custom_buffer_enabled {
            self.target_buffer_ms
        } else {
            self.selected_profile.default_target_buffer_ms()
        };

        let mut restart_requested = false;

        ui.columns(2, |cols| {
            // LEFT CARD: Real-time Jitter & Voice Waveforms + DSP Stats
            theme::studio_card_frame(link_running, palette::ACCENT()).show(&mut cols[0], |ui| {
                {
                    let (ch_rect, _) = ui.allocate_exact_size(
                        Vec2::new(ui.available_width(), 48.0),
                        Sense::hover(),
                    );
                    if ui.is_rect_visible(ch_rect) {
                        let p = ui.painter();
                        let cy = ch_rect.center().y;
                        paint_text_vcenter(
                            p,
                            Pos2::new(ch_rect.min.x + 20.0, cy),
                            Align::Min,
                            t.drawer_telemetry_title,
                            FontId::new(14.0, FontFamily::Proportional),
                            palette::TEXT_PRIMARY(),
                        );
                        if cfg!(target_os = "windows") && self.audiorelay_conflict_cached {
                            paint_text_vcenter(
                                p,
                                Pos2::new(ch_rect.max.x - 20.0, cy),
                                Align::Max,
                                t.c2_vmic_audiorelay_hint,
                                FontId::new(10.5, FontFamily::Proportional),
                                palette::STATUS_WARN(),
                            );
                        }
                    }
                }

                let rx_stat_ref =
                    st_opt.and_then(|st| st.media_rx.as_ref().or(st.mic_rx.as_ref()));
                let (buf_val, asrc_val, third_cap, third_val, third_unit) =
                    if let Some(rx_s) = rx_stat_ref {
                        let repaired = rx_s.net.fec_recovered + rx_s.net.plc_frames;
                        (
                            format!("{:.1}", rx_s.net.buffer_level_ms),
                            format!("{:.5}", rx_s.net.resample_ratio),
                            t.stat_loss_repaired,
                            format!("{}", rx_s.net.packets_lost),
                            format!("/ {repaired}"),
                        )
                    } else if let Some(tx_s) =
                        st_opt.and_then(|st| st.mic_tx.as_ref().or(st.media_tx.as_ref()))
                    {
                        (
                            "—".to_string(),
                            "1.00000".to_string(),
                            t.stat_gated_frames,
                            format_with_commas(tx_s.net.gated_dtx_frames),
                            "".to_string(),
                        )
                    } else if self.demo_mode {
                        (
                            "18.6".to_string(),
                            "1.00001".to_string(),
                            t.stat_loss_repaired,
                            "0".to_string(),
                            "/ 4".to_string(),
                        )
                    } else {
                        (
                            "—".to_string(),
                            "1.00000".to_string(),
                            t.stat_loss_repaired,
                            "0".to_string(),
                            "/ 0".to_string(),
                        )
                    };
                let target_buf_unit = format!("/ {effective_buf_ms} ms");

                draw_stats_row_3(
                    ui,
                    [
                        (t.stat_buffer_level, &buf_val, &target_buf_unit),
                        (t.stat_clock_comp, &asrc_val, "×"),
                        (third_cap, &third_val, &third_unit),
                    ],
                    link_running,
                );

                let jb_summary = lang.format_buffer_summary(effective_buf_ms);
                draw_sparkline(
                    ui,
                    t.c2_jb_chart_title,
                    &jb_summary,
                    t.chart_rx_idle_hint,
                    &self.duplex_buffer_sparkline,
                    0.0,
                    (effective_buf_ms as f32 * 2.0).max(32.0),
                    Some(effective_buf_ms as f32),
                    palette::ACCENT(),
                );

                draw_sparkline(
                    ui,
                    t.c2_voice_chart_title,
                    &format!("{} kbps", self.opus_bitrate_kbps),
                    t.chart_tx_idle_hint,
                    &self.duplex_tx_envelope_sparkline,
                    -60.0,
                    0.0,
                    if self.duplex_mic_mode.sends() {
                        Some(self.duplex_gate_db)
                    } else {
                        None
                    },
                    palette::ACCENT(),
                );
            });

            // RIGHT CARD: Advanced Network & Codec Settings
            theme::studio_card_frame(false, palette::LINE()).show(&mut cols[1], |ui| {
                {
                    let (ch_rect, _) = ui.allocate_exact_size(
                        Vec2::new(ui.available_width(), 48.0),
                        Sense::hover(),
                    );
                    if ui.is_rect_visible(ch_rect) {
                        paint_text_vcenter(
                            ui.painter(),
                            Pos2::new(ch_rect.min.x + 20.0, ch_rect.center().y),
                            Align::Min,
                            t.drawer_advanced_title,
                            FontId::new(14.0, FontFamily::Proportional),
                            palette::TEXT_PRIMARY(),
                        );
                    }
                }
                draw_hairline(ui);

                let prev_m_port = self.media_port_input.clone();
                settings_row(ui, t.label_media_port, true, |ui| {
                    quiet_mono_input_pill(ui, "adv_media_port", &mut self.media_port_input);
                });
                if self.media_port_input != prev_m_port {
                    if let Ok(p) = self.media_port_input.trim().parse::<u16>() {
                        if p > 1024 {
                            self.duplex_downlink_port = p;
                            self.sync_lan_advertiser();
                        }
                    }
                }

                let prev_v_port = self.mic_port_input.clone();
                settings_row(ui, t.label_mic_port, true, |ui| {
                    quiet_mono_input_pill(ui, "adv_mic_port", &mut self.mic_port_input);
                });
                if self.mic_port_input != prev_v_port {
                    if let Ok(p) = self.mic_port_input.trim().parse::<u16>() {
                        if p > 1024 {
                            self.duplex_uplink_port = p;
                        }
                    }
                }

                settings_row(ui, t.label_custom_buffer, true, |ui| {
                    ui.spacing_mut().item_spacing.x = 10.0;
                    if self.custom_buffer_enabled {
                        let buf_fmt = format!("{} ms", self.target_buffer_ms);
                        if quiet_slider_u16(ui, &mut self.target_buffer_ms, 5..=80, &buf_fmt) {
                            restart_requested = true;
                        }
                    } else {
                        let txt = format!(
                            "{} ({} ms)",
                            t.buffer_auto_profile,
                            self.selected_profile.default_target_buffer_ms()
                        );
                        let g = ui.painter().layout_no_wrap(
                            txt,
                            FontId::new(12.0, FontFamily::Proportional),
                            palette::TEXT_MUTED(),
                        );
                        let (lbl_r, _) =
                            ui.allocate_exact_size(Vec2::new(g.size().x, 22.0), Sense::hover());
                        crate::widgets::paint_galley_vcenter(
                            ui.painter(),
                            lbl_r.min.x,
                            lbl_r.center().y,
                            g,
                            palette::TEXT_MUTED(),
                        );
                    }
                    if pill_toggle(ui, &mut self.custom_buffer_enabled) {
                        restart_requested = true;
                    }
                });

                settings_row(ui, t.label_channels, true, |ui| {
                    let ch_opts = [
                        (ChannelLayout::Stereo, t.channel_stereo),
                        (ChannelLayout::Mono, t.channel_mono),
                    ];
                    if segmented_control(ui, "adv_channels_seg", &mut self.duplex_rx_channels, &ch_opts)
                    {
                        restart_requested = true;
                    }
                });

                let br_fmt = format!("{} kbps", self.opus_bitrate_kbps);
                settings_row(ui, t.label_opus_bitrate, true, |ui| {
                    if quiet_slider_u16(ui, &mut self.opus_bitrate_kbps, 64..=256, &br_fmt) {
                        restart_requested = true;
                    }
                });

                settings_row(ui, t.label_test_tone, true, |ui| {
                    if pill_toggle(ui, &mut self.use_test_tone) {
                        restart_requested = true;
                    }
                });

                settings_row(ui, t.label_status_log, false, |ui| {
                    let g = ui.painter().layout_no_wrap(
                        self.last_status_msg.clone(),
                        FontId::new(11.5, FontFamily::Proportional),
                        palette::TEXT_SECONDARY(),
                    );
                    let max_w = ui.available_width().max(60.0);
                    let (lbl_r, _) = ui.allocate_exact_size(
                        Vec2::new(g.size().x.min(max_w), 22.0),
                        Sense::hover(),
                    );
                    let cp = ui.painter().with_clip_rect(lbl_r);
                    crate::widgets::paint_galley_vcenter(
                        &cp,
                        lbl_r.min.x,
                        lbl_r.center().y,
                        g,
                        palette::TEXT_SECONDARY(),
                    );
                });
            });
        });

        if restart_requested && self.active_duplex.is_some() {
            self.restart_active_session();
        }
    }

    // =========================================================================
    // Tab 2: 音频设备 (`HardwareMatrix` Tab)
    // =========================================================================

    fn render_hardware_matrix_tab(&mut self, ui: &mut egui::Ui) {
        let t = self.lang.texts();

        {
            let (hdr_rect, _) =
                ui.allocate_exact_size(Vec2::new(ui.available_width(), 48.0), Sense::hover());
            let hcy = hdr_rect.center().y;
            if ui.is_rect_visible(hdr_rect) {
                let p = ui.painter();
                paint_text_vcenter(
                    p,
                    Pos2::new(hdr_rect.min.x, hcy - 11.5),
                    Align::Min,
                    t.page_hw_title,
                    FontId::new(20.0, FontFamily::Proportional),
                    palette::TEXT_PRIMARY(),
                );
                paint_text_vcenter(
                    p,
                    Pos2::new(hdr_rect.min.x, hcy + 12.5),
                    Align::Min,
                    t.page_hw_subtitle,
                    FontId::new(13.0, FontFamily::Proportional),
                    palette::TEXT_SECONDARY(),
                );
            }
            let mut right_ui = ui.new_child(
                egui::UiBuilder::new()
                    .max_rect(hdr_rect)
                    .layout(egui::Layout::right_to_left(Align::Center)),
            );
            right_ui.spacing_mut().item_spacing.x = 8.0;
            if right_ui
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
            if (cfg!(target_os = "macos") && !is_macos_rsrelay_driver_installed())
                || (cfg!(target_os = "windows")
                    && select_virtual_mic_output_from_devices(&self.devices).is_none())
            {
                let installing = self.installing_vmic_driver.load(Ordering::Relaxed);
                let drv_btn = if installing {
                    t.btn_installing_vmic
                } else if cfg!(target_os = "macos") {
                    t.btn_install_macos_vmic
                } else {
                    t.btn_install_win_vbcable
                };
                if right_ui
                    .add_enabled(
                        !installing,
                        egui::Button::new(RichText::new(drv_btn).size(12.5).color(palette::ACCENT())),
                    )
                    .clicked()
                {
                    self.trigger_install_virtual_mic_driver();
                }
            }
        }

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
                    .inner_margin(egui::Margin::symmetric(20.0, 16.0))
                    .show(ui, |ui| {
                        let (ch_rect, _) = ui.allocate_exact_size(
                            Vec2::new(ui.available_width(), 36.0),
                            Sense::hover(),
                        );
                        let cy = ch_rect.center().y;
                        let mut left_ui = ui.new_child(
                            egui::UiBuilder::new()
                                .max_rect(ch_rect)
                                .layout(egui::Layout::left_to_right(Align::Center)),
                        );
                        draw_card_icon_tile(&mut left_ui, IconKind::Microphone);
                        let p = ui.painter();
                        paint_text_vcenter(
                            p,
                            Pos2::new(ch_rect.min.x + 44.0, cy - 8.5),
                            Align::Min,
                            t.hw_input_title,
                            FontId::new(15.0, FontFamily::Proportional),
                            palette::TEXT_PRIMARY(),
                        );
                        paint_text_vcenter(
                            p,
                            Pos2::new(ch_rect.min.x + 44.0, cy + 9.0),
                            Align::Min,
                            t.hw_input_subtitle,
                            FontId::new(11.5, FontFamily::Proportional),
                            palette::TEXT_MUTED(),
                        );
                        let mut right_ui = ui.new_child(
                            egui::UiBuilder::new()
                                .max_rect(ch_rect)
                                .layout(egui::Layout::right_to_left(Align::Center)),
                        );
                        status_chip(
                            &mut right_ui,
                            &format!("{}", in_devices.len()),
                            None,
                            !in_devices.is_empty(),
                        );
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
                    .inner_margin(egui::Margin::symmetric(20.0, 16.0))
                    .show(ui, |ui| {
                        let (ch_rect, _) = ui.allocate_exact_size(
                            Vec2::new(ui.available_width(), 36.0),
                            Sense::hover(),
                        );
                        let cy = ch_rect.center().y;
                        let mut left_ui = ui.new_child(
                            egui::UiBuilder::new()
                                .max_rect(ch_rect)
                                .layout(egui::Layout::left_to_right(Align::Center)),
                        );
                        draw_card_icon_tile(&mut left_ui, IconKind::SpeakerDevice);
                        let p = ui.painter();
                        paint_text_vcenter(
                            p,
                            Pos2::new(ch_rect.min.x + 44.0, cy - 8.5),
                            Align::Min,
                            t.hw_output_title,
                            FontId::new(15.0, FontFamily::Proportional),
                            palette::TEXT_PRIMARY(),
                        );
                        paint_text_vcenter(
                            p,
                            Pos2::new(ch_rect.min.x + 44.0, cy + 9.0),
                            Align::Min,
                            t.hw_output_subtitle,
                            FontId::new(11.5, FontFamily::Proportional),
                            palette::TEXT_MUTED(),
                        );
                        let mut right_ui = ui.new_child(
                            egui::UiBuilder::new()
                                .max_rect(ch_rect)
                                .layout(egui::Layout::right_to_left(Align::Center)),
                        );
                        status_chip(
                            &mut right_ui,
                            &format!("{}", out_devices.len()),
                            None,
                            !out_devices.is_empty(),
                        );
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
                t.toggle_link_id.clone(),
                t.mute_speaker_id.clone(),
                t.mute_mic_id.clone(),
                t.stop_all_id.clone(),
                t.quit_id.clone(),
            )
        });
        if let Some((
            toggle_window_id,
            toggle_link_id,
            mute_speaker_id,
            mute_mic_id,
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
                } else if event_id == toggle_link_id {
                    self.toggle_duplex();
                } else if event_id == mute_speaker_id {
                    self.toggle_speaker_mute();
                } else if event_id == mute_mic_id {
                    self.toggle_mic_mute();
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

        let window_visible = self.tray_window_state == TrayWindowState::Visible;
        if (window_visible || self.active_duplex.is_some())
            && self.last_device_poll.elapsed() >= Duration::from_millis(3000)
        {
            self.last_device_poll = Instant::now();
            self.refresh_audio_devices(false);
        }
        if window_visible
            && !self.demo_mode
            && self.active_duplex.is_none()
            && self.last_lan_scan.elapsed() >= Duration::from_secs(8)
        {
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

        if self.active_duplex.is_some() || self.demo_mode {
            ctx.request_repaint_after(Duration::from_millis(16));
        }

        self.update_live_telemetry();

        self.render_sidebar(ctx);

        egui::CentralPanel::default()
            .frame(
                egui::Frame::none()
                    .fill(palette::BG_CANVAS())
                    .inner_margin(egui::Margin {
                        left: metrics::MAIN_PADDING_X,
                        right: metrics::MAIN_PADDING_X,
                        top: metrics::MAIN_PADDING_Y,
                        bottom: 20.0,
                    }),
            )
            .show(ctx, |ui| {
                egui::ScrollArea::vertical()
                    .auto_shrink([false, false])
                    .show(ui, |ui| match self.active_tab {
                        WorkspaceTab::AudioLink => self.render_audio_link_tab(ui),
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
        raw.push(0u8);
        let row_start = y * (w as usize);
        for c in &img.pixels[row_start..row_start + (w as usize)] {
            raw.extend_from_slice(&c.to_array());
        }
    }

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
    let init_h = std::env::args()
        .find_map(|a| a.strip_prefix("--height=").and_then(|v| v.parse::<f32>().ok()))
        .unwrap_or(760.0);

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
            .with_inner_size([1220.0, init_h])
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
                "2" | "hw" | "devices" | "matrix" => app.active_tab = WorkspaceTab::HardwareMatrix,
                _ => app.active_tab = WorkspaceTab::AudioLink,
            }
        } else if let Some(preset_val) = arg.strip_prefix("--preset=") {
            match preset_val.to_ascii_lowercase().as_str() {
                "headset" => app.apply_scene_preset(ScenePreset::Headset),
                "host" => app.apply_scene_preset(ScenePreset::Host),
                "speaker" => app.apply_scene_preset(ScenePreset::SpeakerOnly),
                "mic" => app.apply_scene_preset(ScenePreset::MicOnly),
                "bi" | "symmetric" => {
                    app.duplex_role = DuplexRole::Symmetric;
                    app.duplex_media_mode = MediaBusMode::Bidirectional;
                    app.duplex_mic_mode = MicBusMode::Bidirectional;
                }
                _ => {}
            }
        } else if arg == "--drawer" {
            app.show_advanced_drawer = true;
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

                thread::sleep(Duration::from_millis(500));
                let _ = tx.send(toggle_id);
                if let Ok(guard) = global_egui_wakeup_ctx().lock() {
                    if let Some(ref ctx) = *guard {
                        ctx.request_repaint();
                    }
                }
                #[cfg(target_os = "windows")]
                win32_tray::wake_main_window();

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
    fn test_headless_render_all_workspace_tabs_and_presets_bilingual() {
        let mut app = RsRelayGuiApp::new_with_tray(false).expect("init app");
        let ctx = egui::Context::default();

        for lang in [Language::En, Language::Zh] {
            app.set_language(lang);
            assert_eq!(app.lang, lang);

            for mode in [ThemeMode::System, ThemeMode::Light, ThemeMode::Dark] {
                app.theme_mode = mode;
                for tab in [WorkspaceTab::AudioLink, WorkspaceTab::HardwareMatrix] {
                    app.active_tab = tab;
                    for preset in [
                        ScenePreset::Headset,
                        ScenePreset::Host,
                        ScenePreset::SpeakerOnly,
                        ScenePreset::MicOnly,
                    ] {
                        app.apply_scene_preset(preset);
                        for drawer_open in [false, true] {
                            app.show_advanced_drawer = drawer_open;
                            let _ = ctx.run(egui::RawInput::default(), |ctx| {
                                app.render_ui(ctx);
                            });
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn test_scene_presets_and_mute_toggles() {
        let mut app = RsRelayGuiApp::new_with_tray(false).expect("init app");
        assert_eq!(
            ScenePreset::from_bus_modes(app.duplex_media_mode, app.duplex_mic_mode),
            Some(ScenePreset::Headset)
        );

        app.apply_scene_preset(ScenePreset::SpeakerOnly);
        assert_eq!(app.duplex_media_mode, MediaBusMode::ReceiveSpeaker);
        assert_eq!(app.duplex_mic_mode, MicBusMode::Off);
        assert_eq!(
            ScenePreset::from_bus_modes(app.duplex_media_mode, app.duplex_mic_mode),
            Some(ScenePreset::SpeakerOnly)
        );

        app.apply_scene_preset(ScenePreset::Host);
        assert_eq!(app.duplex_role, DuplexRole::Host);
        assert_eq!(app.duplex_media_mode, MediaBusMode::SendLoopback);
        assert_eq!(app.duplex_mic_mode, MicBusMode::ReceiveVirtualMic);

        assert!(!app.speaker_muted);
        app.toggle_speaker_mute();
        assert!(app.speaker_muted);

        assert!(!app.duplex_mic_muted);
        app.toggle_mic_mute();
        assert!(app.duplex_mic_muted);
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

        // Verify background device polling and LAN scanning stay idle while hidden in tray
        app.last_device_poll = Instant::now() - Duration::from_secs(30);
        app.last_lan_scan = Instant::now() - Duration::from_secs(30);
        let _ = ctx.run(egui::RawInput::default(), |ctx| {
            app.render_ui(ctx);
        });
        assert!(!app.polling_devices.load(Ordering::Relaxed));
        assert!(!app.scanning_lan.load(Ordering::Relaxed));

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

        app.apply_auto_discovered_target();
        assert_eq!(app.target_addr_input, "192.168.3.110:49153");
        assert_eq!(app.duplex_peer_ip_input, "192.168.3.110");

        let opts = app.build_target_combo_options();
        assert!(opts
            .iter()
            .any(|(addr, _, is_lan)| addr == "192.168.3.110:49153" && *is_lan));
        assert!(opts
            .iter()
            .any(|(addr, _, is_lan)| addr == "192.168.3.151:49153" && *is_lan));

        let parsed = parse_socket_addr_flexible(
            "192.168.3.151",
            IpAddr::from([127, 0, 0, 1]),
            DEFAULT_AUDIO_UDP_PORT,
        )
        .expect("parse bare IP");
        assert_eq!(parsed.to_string(), "192.168.3.151:49153");
    }
}
