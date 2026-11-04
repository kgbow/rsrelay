//! Internationalization (`i18n`) module for `rsrelay-gui`.
//!
//! Supports English (`Language::En`) and Simplified Chinese (`Language::Zh`) with
//! moderate, pro-audio-friendly localization where common technical terms (`TX`, `RX`,
//! `UDP`, `LAN`, `mDNS`, `Opus`, `PCM`, `FEC`, `PLC`, `ASRC`, `Jitter Buffer`, `Loopback`,
//! `Mic`, `dBFS`, `Hz`, `ms`) are preserved alongside natural Chinese labels.

use std::net::{IpAddr, SocketAddr};
use std::process::Command;

use rsrelay_core::DuplexRole;

/// Supported UI languages.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Language {
    /// English (`en`)
    En,
    /// Simplified Chinese (`zh-CN`)
    Zh,
}

impl Default for Language {
    fn default() -> Self {
        Self::detect_system()
    }
}

impl Language {
    /// Auto-detect preferred UI language from environment variables (`RSRELAY_LANG`,
    /// `LC_ALL`, `LC_MESSAGES`, `LANG`) or native OS locale settings (macOS `AppleLanguages`,
    /// Windows `GetUserDefaultUILanguage`).
    pub fn detect_system() -> Self {
        if let Ok(val) = std::env::var("RSRELAY_LANG") {
            if let Some(lang) = Self::from_locale_str(&val) {
                return lang;
            }
        }

        for key in ["LC_ALL", "LC_MESSAGES", "LANG"] {
            if let Ok(val) = std::env::var(key) {
                let trimmed = val.trim();
                if !trimmed.is_empty()
                    && !trimmed.eq_ignore_ascii_case("c")
                    && !trimmed.to_ascii_lowercase().starts_with("c.")
                    && !trimmed.eq_ignore_ascii_case("posix")
                {
                    if let Some(lang) = Self::from_locale_str(trimmed) {
                        return lang;
                    }
                }
            }
        }

        #[cfg(target_os = "macos")]
        {
            if let Ok(output) = Command::new("defaults")
                .args(["read", "-g", "AppleLanguages"])
                .output()
            {
                if output.status.success() {
                    let stdout = String::from_utf8_lossy(&output.stdout).to_ascii_lowercase();
                    for line in stdout.lines() {
                        let clean = line.trim().trim_matches(&['"', ',', '(', ')', ' '][..]);
                        if clean.starts_with("zh") {
                            return Self::Zh;
                        }
                        if clean.starts_with("en") {
                            return Self::En;
                        }
                    }
                }
            }
        }

        #[cfg(target_os = "windows")]
        {
            extern "system" {
                fn GetUserDefaultUILanguage() -> u16;
            }
            // PRIMARYLANGID(lang_id) == LANG_CHINESE (0x04)
            let lang_id = unsafe { GetUserDefaultUILanguage() };
            if (lang_id & 0x03ff) == 0x04 {
                return Self::Zh;
            }
        }

        Self::En
    }

    /// Parse a locale string (e.g. `"zh_CN.UTF-8"`, `"zh-Hans"`, `"en_US"`) into a [`Language`].
    pub fn from_locale_str(s: &str) -> Option<Self> {
        let lower = s.trim().to_ascii_lowercase();
        if lower.starts_with("zh") || lower.contains("chinese") || lower == "cn" {
            Some(Self::Zh)
        } else if lower.starts_with("en") || lower.contains("english") {
            Some(Self::En)
        } else {
            None
        }
    }

    /// Toggle between `Language::En` and `Language::Zh`.
    pub const fn toggle(self) -> Self {
        match self {
            Self::En => Self::Zh,
            Self::Zh => Self::En,
        }
    }

    /// Short ISO language code (`"en"` or `"zh"`).
    pub const fn code(self) -> &'static str {
        match self {
            Self::En => "en",
            Self::Zh => "zh",
        }
    }

    /// Return the static UI text table for this language.
    pub const fn texts(self) -> &'static Texts {
        match self {
            Self::En => &TEXTS_EN,
            Self::Zh => &TEXTS_ZH,
        }
    }

    // =========================================================================
    // Dynamic Status & Tooltip Formatters
    // =========================================================================

    pub fn status_ready(self) -> String {
        match self {
            Self::En => "Ready. Select a mode or scan LAN to begin streaming.".to_string(),
            Self::Zh => "就绪。请选择工作模式或扫描 LAN 开始音频传输。".to_string(),
        }
    }

    pub fn status_refreshed_ports(self, in_count: usize, out_count: usize) -> String {
        match self {
            Self::En => format!(
                "Refreshed audio ports: {in_count} input(s), {out_count} output(s) detected."
            ),
            Self::Zh => format!(
                "已刷新音频设备：检测到 {in_count} 个输入 (IN)，{out_count} 个输出 (OUT)。"
            ),
        }
    }

    pub fn status_hotswapped_output(self, dev_name: &str) -> String {
        match self {
            Self::En => format!("Hot-swapped output to newly connected default: '{dev_name}'"),
            Self::Zh => format!("已热切换输出设备至新连接的默认设备：'{dev_name}'"),
        }
    }

    pub fn status_hotswapped_input(self, dev_name: &str) -> String {
        match self {
            Self::En => format!("Hot-swapped input to newly connected default: '{dev_name}'"),
            Self::Zh => format!("已热切换输入设备至新连接的默认设备：'{dev_name}'"),
        }
    }

    pub fn status_scanning_lan(self) -> String {
        match self {
            Self::En => "Scanning local network via mDNS (_rsrelay._udp.local.)...".to_string(),
            Self::Zh => "正在通过 mDNS (_rsrelay._udp.local.) 扫描局域网...".to_string(),
        }
    }

    pub fn status_all_stopped(self) -> String {
        match self {
            Self::En => "All audio streams stopped.".to_string(),
            Self::Zh => "已停止所有音频流。".to_string(),
        }
    }

    pub fn status_all_stopped_tray(self) -> String {
        match self {
            Self::En => "All streams stopped from System Tray.".to_string(),
            Self::Zh => "已通过系统托盘停止所有音频流。".to_string(),
        }
    }

    pub fn status_sender_stopped(self) -> String {
        match self {
            Self::En => "Sender stopped.".to_string(),
            Self::Zh => "发送端 (TX) 已停止。".to_string(),
        }
    }

    pub fn status_invalid_target_addr(self, err: & impl std::fmt::Display) -> String {
        match self {
            Self::En => format!("Invalid Target IP:Port: {err}"),
            Self::Zh => format!("目标 IP:Port 格式无效: {err}"),
        }
    }

    pub fn status_sender_streaming(
        self,
        device_name: &str,
        sample_rate: u32,
        target_addr: SocketAddr,
    ) -> String {
        match self {
            Self::En => format!("Streaming '{device_name}' ({sample_rate} Hz) -> {target_addr}"),
            Self::Zh => format!("正在发送 '{device_name}' ({sample_rate} Hz) -> {target_addr}"),
        }
    }

    pub fn status_sender_failed(self, err: &impl std::fmt::Display) -> String {
        match self {
            Self::En => format!("Failed to start sender: {err}"),
            Self::Zh => format!("启动发送端 (TX) 失败: {err}"),
        }
    }

    pub fn status_receiver_stopped(self) -> String {
        match self {
            Self::En => "Receiver stopped.".to_string(),
            Self::Zh => "接收端 (RX) 已停止。".to_string(),
        }
    }

    pub fn status_invalid_bind_addr(self, err: &impl std::fmt::Display) -> String {
        match self {
            Self::En => format!("Invalid Bind IP:Port: {err}"),
            Self::Zh => format!("监听 IP:Port 格式无效: {err}"),
        }
    }

    pub fn status_receiver_listening(
        self,
        bind_addr: SocketAddr,
        device_name: &str,
        sample_rate: u32,
    ) -> String {
        match self {
            Self::En => format!(
                "Listening on {bind_addr} -> Output '{device_name}' ({sample_rate} Hz)"
            ),
            Self::Zh => format!(
                "正在监听 {bind_addr} -> 输出设备 '{device_name}' ({sample_rate} Hz)"
            ),
        }
    }

    pub fn status_receiver_failed(self, err: &impl std::fmt::Display) -> String {
        match self {
            Self::En => format!("Failed to start receiver: {err}"),
            Self::Zh => format!("启动接收端 (RX) 失败: {err}"),
        }
    }

    pub fn status_duplex_stopped(self) -> String {
        match self {
            Self::En => "Full-Duplex session stopped.".to_string(),
            Self::Zh => "Full-Duplex 双向会话已停止。".to_string(),
        }
    }

    pub fn status_invalid_peer_ip(self, err: &impl std::fmt::Display) -> String {
        match self {
            Self::En => format!("Invalid Peer IP address: {err}"),
            Self::Zh => format!("对端 Peer IP 地址无效: {err}"),
        }
    }

    pub fn status_duplex_active(
        self,
        role: DuplexRole,
        peer_ip: IpAddr,
        tx_dev: &str,
        rx_dev: &str,
    ) -> String {
        match self {
            Self::En => format!(
                "Full-Duplex [{role:?}] active <-> {peer_ip} (TX: '{tx_dev}', RX: '{rx_dev}')"
            ),
            Self::Zh => format!(
                "Full-Duplex [{role:?}] 运行中 <-> {peer_ip} (TX: '{tx_dev}', RX: '{rx_dev}')"
            ),
        }
    }

    pub fn status_duplex_failed(self, err: &impl std::fmt::Display) -> String {
        match self {
            Self::En => format!("Failed to start Full-Duplex session: {err}"),
            Self::Zh => format!("启动 Full-Duplex 会话失败: {err}"),
        }
    }

    pub fn status_copied_lan_ip(self, ip: IpAddr) -> String {
        match self {
            Self::En => format!("Copied local LAN IP '{ip}' to clipboard."),
            Self::Zh => format!("已复制本机 LAN IP '{ip}' 到剪贴板。"),
        }
    }

    pub fn status_copied_rx_endpoint(self, endpoint: &str) -> String {
        match self {
            Self::En => format!("Copied Receiver LAN endpoint '{endpoint}' to clipboard."),
            Self::Zh => format!("已复制接收端 LAN 地址 '{endpoint}' 到剪贴板。"),
        }
    }

    pub fn hover_copy_rx_endpoint(self, endpoint: &str) -> String {
        match self {
            Self::En => {
                format!("Copy this Receiver's LAN address ({endpoint}) for the Sender machine")
            }
            Self::Zh => format!("复制此接收端的 LAN 地址 ({endpoint}) 供发送端连接"),
        }
    }

    pub fn status_selected_lan_peer(self, hostname: &str, addr_str: &str) -> String {
        match self {
            Self::En => format!("Selected LAN target '{hostname}' ({addr_str})"),
            Self::Zh => format!("已选择局域网目标节点 '{hostname}' ({addr_str})"),
        }
    }

    pub fn status_sck_granted(self) -> String {
        match self {
            Self::En => "macOS ScreenCaptureKit permission granted.".to_string(),
            Self::Zh => "macOS ScreenCaptureKit 系统录音权限已授权。".to_string(),
        }
    }

    pub fn status_sck_prompted(self) -> String {
        match self {
            Self::En => "Prompted macOS Screen & System Audio Recording permission. Enable in System Settings and restart if needed.".to_string(),
            Self::Zh => "已请求 macOS 录屏与系统录音权限，请在系统设置中开启授权（必要时重启应用）。".to_string(),
        }
    }

    pub fn status_language_switched(self) -> String {
        match self {
            Self::En => "Switched language to English.".to_string(),
            Self::Zh => "界面语言已切换为中文。".to_string(),
        }
    }

    pub fn format_buffer_summary(self, current_ms: Option<f32>, target_ms: u16) -> String {
        match (self, current_ms) {
            (Self::En, Some(cur)) => format!("{cur:.1} ms (Target {target_ms} ms)"),
            (Self::Zh, Some(cur)) => format!("{cur:.1} ms (目标 {target_ms} ms)"),
            (Self::En, None) => format!("Target {target_ms} ms"),
            (Self::Zh, None) => format!("目标 {target_ms} ms"),
        }
    }
}

/// Compile-time verified UI string table.
pub struct Texts {
    // Window & System Tray
    pub tray_title: &'static str,
    pub tray_stop_all: &'static str,
    pub tray_quit: &'static str,

    // Top Header Bar
    pub app_subtitle: &'static str,
    pub tab_dual_console: &'static str,
    pub tab_full_duplex: &'static str,
    pub tab_hardware_matrix: &'static str,
    pub lang_toggle_btn: &'static str,
    pub lang_toggle_hover: &'static str,
    pub btn_refresh_ports: &'static str,
    pub btn_refresh_ports_hover: &'static str,
    pub btn_scanning_lan: &'static str,
    pub btn_scan_lan: &'static str,
    pub btn_scan_lan_hover: &'static str,
    pub lan_ip_hover: &'static str,

    // Bottom Footer Bar
    pub btn_stop_all: &'static str,
    pub chk_auto_follow_hotplug: &'static str,

    // LAN Discovery Strip
    pub discovered_lan_peers: &'static str,
    pub discovered_peer_hover: &'static str,

    // Common Status & Device Labels
    pub status_streaming: &'static str,
    pub status_listening: &'static str,
    pub status_duplex_active: &'static str,
    pub status_standby: &'static str,
    pub device_system_default: &'static str,

    // 01 // Transmitter (TX) Card
    pub tx_card_title: &'static str,
    pub tx_card_subtitle: &'static str,
    pub label_target_ip_port: &'static str,
    pub label_audio_source: &'static str,
    pub source_system_loopback: &'static str,
    pub source_microphone: &'static str,
    pub source_synthetic_tone: &'static str,
    pub label_input_device: &'static str,
    pub hover_refresh_input_ports: &'static str,
    pub label_opus_bitrate: &'static str,
    pub macos_sck_ready: &'static str,
    pub macos_perm_required: &'static str,
    pub btn_grant_perm: &'static str,
    pub hover_grant_perm: &'static str,
    pub section_latency_profile: &'static str,
    pub profile_ultra_low: &'static str,
    pub profile_balanced: &'static str,
    pub profile_stable: &'static str,
    pub profile_lossless: &'static str,
    pub btn_start_tx: &'static str,
    pub btn_stop_tx: &'static str,
    pub vu_input_capture: &'static str,
    pub tile_packets_tx: &'static str,
    pub tile_silence_heartbeat: &'static str,
    pub tile_silence_sub: &'static str,
    pub tile_control_telemetry: &'static str,
    pub awaiting_tx: &'static str,

    // 02 // Receiver (RX) Card
    pub rx_card_title: &'static str,
    pub rx_card_subtitle: &'static str,
    pub label_bind_ip_port: &'static str,
    pub btn_copy_lan: &'static str,
    pub label_output_device: &'static str,
    pub hover_refresh_output_ports: &'static str,
    pub label_target_buffer: &'static str,
    pub label_output_gain: &'static str,
    pub chk_advertise_mdns: &'static str,
    pub btn_start_rx: &'static str,
    pub btn_stop_rx: &'static str,
    pub vu_output_playback: &'static str,
    pub sparkline_jitter_watermark: &'static str,
    pub sparkline_awaiting: &'static str,
    pub tile_packets_rx: &'static str,
    pub tile_loss_fec_plc: &'static str,
    pub tile_pi_clock_asrc: &'static str,

    // 03 // Full-Duplex Headset Tab
    pub duplex_card_title: &'static str,
    pub duplex_card_subtitle: &'static str,
    pub role_host_title: &'static str,
    pub role_host_desc: &'static str,
    pub role_headset_title: &'static str,
    pub role_headset_desc: &'static str,
    pub label_peer_ip: &'static str,
    pub label_udp_ports_dl_ul: &'static str,
    pub label_tx_input_device: &'static str,
    pub label_rx_output_device: &'static str,
    pub label_mic_noise_gate: &'static str,
    pub label_output_gain_mic: &'static str,
    pub chk_mute_mic: &'static str,
    pub btn_start_duplex: &'static str,
    pub btn_stop_duplex: &'static str,
    pub vu_duplex_tx: &'static str,
    pub tile_tx_capture_device: &'static str,
    pub tile_uplink_telemetry: &'static str,
    pub vu_duplex_rx: &'static str,
    pub tile_downlink_packets: &'static str,
    pub tile_drift_comp: &'static str,

    // Hardware Matrix Tab
    pub hw_input_title: &'static str,
    pub hw_output_title: &'static str,
    pub badge_default_in: &'static str,
    pub badge_default_out: &'static str,
}

pub const TEXTS_EN: Texts = Texts {
    tray_title: "rsrelay — Ultra-Low-Latency Audio Relay",
    tray_stop_all: "Stop All Audio Streams",
    tray_quit: "Quit rsrelay",

    app_subtitle: "Ultra-Low-Latency Real-Time Audio Relay",
    tab_dual_console: "Dual Console (TX / RX)",
    tab_full_duplex: "Full-Duplex Headset",
    tab_hardware_matrix: "Audio Matrix & LAN",
    lang_toggle_btn: "🌐 中文",
    lang_toggle_hover: "Switch UI language to Chinese (切换为中文)",
    btn_refresh_ports: "↻ Ports",
    btn_refresh_ports_hover: "Re-enumerate OS audio input/output hardware",
    btn_scanning_lan: "◎ Scanning...",
    btn_scan_lan: "◎ Scan LAN",
    btn_scan_lan_hover: "Discover active rsrelay receivers on LAN via mDNS",
    lan_ip_hover: "Click to copy this machine's local LAN IPv4 address",

    btn_stop_all: "■ Stop All",
    chk_auto_follow_hotplug: "Auto-Follow Hot-Plug",

    discovered_lan_peers: "◎ DISCOVERED LAN PEERS",
    discovered_peer_hover: "Click to bind this peer IP:Port as target",

    status_streaming: "STREAMING",
    status_listening: "LISTENING",
    status_duplex_active: "DUPLEX LINK ACTIVE",
    status_standby: "STANDBY",
    device_system_default: "System Default (Auto)",

    tx_card_title: "01 // TRANSMITTER (SENDER)",
    tx_card_subtitle: "System Loopback / Mic Capture -> UDP Opus/PCM",
    label_target_ip_port: "Target IP:Port",
    label_audio_source: "Audio Source",
    source_system_loopback: "System Loopback (SCK / WASAPI)",
    source_microphone: "Microphone Input",
    source_synthetic_tone: "Synthetic 440Hz/660Hz Reference Tone",
    label_input_device: "Input Device",
    hover_refresh_input_ports: "Refresh input ports",
    label_opus_bitrate: "Opus Bitrate",
    macos_sck_ready: "macOS SCK Driverless Loopback Ready",
    macos_perm_required: "macOS Audio Permission Required",
    btn_grant_perm: "Grant Permission",
    hover_grant_perm:
        "Trigger macOS System Settings > Privacy & Security > Screen & System Audio Recording prompt",
    section_latency_profile: "LATENCY & CODEC PROFILE",
    profile_ultra_low: "Ultra-Low",
    profile_balanced: "Balanced",
    profile_stable: "Stable",
    profile_lossless: "Lossless",
    btn_start_tx: "Start Transmitter",
    btn_stop_tx: "Stop Transmitter",
    vu_input_capture: "INPUT CAPTURE LEVEL",
    tile_packets_tx: "PACKETS TX",
    tile_silence_heartbeat: "SILENCE HEARTBEAT",
    tile_silence_sub: "Zero-payload keepalive",
    tile_control_telemetry: "CONTROL TELEMETRY",
    awaiting_tx: "Awaiting TX",

    rx_card_title: "02 // RECEIVER (LISTENER)",
    rx_card_subtitle: "Adaptive Jitter Buffer + PI Clock ASRC -> Speaker",
    label_bind_ip_port: "Bind IP:Port",
    btn_copy_lan: "Copy LAN",
    label_output_device: "Output Device",
    hover_refresh_output_ports: "Refresh output ports",
    label_target_buffer: "Target Buffer",
    label_output_gain: "Output Gain",
    chk_advertise_mdns: "Advertise receiver on LAN via mDNS (_rsrelay._udp.local.)",
    btn_start_rx: "Start Receiver",
    btn_stop_rx: "Stop Receiver",
    vu_output_playback: "OUTPUT PLAYBACK LEVEL",
    sparkline_jitter_watermark: "JITTER BUFFER WATERMARK",
    sparkline_awaiting: "Awaiting live stream telemetry...",
    tile_packets_rx: "PACKETS RX",
    tile_loss_fec_plc: "LOSS & FEC / PLC",
    tile_pi_clock_asrc: "PI CLOCK ASRC",

    duplex_card_title: "03 // FULL-DUPLEX WIRELESS HEADSET CONSOLE",
    duplex_card_subtitle:
        "Simultaneous Stereo System Audio Downlink + Noise-Gated Microphone Uplink",
    role_host_title: "HOST WORKSTATION (MAIN PC)",
    role_host_desc: "TX: System Audio Loopback (:49153)  •  RX: Remote Mic Uplink (:49155)",
    role_headset_title: "WIRELESS HEADSET (SECONDARY DEVICE)",
    role_headset_desc: "RX: Host System Audio (:49153)  •  TX: Local Mic with Noise Gate (:49155)",
    label_peer_ip: "Peer IP Address",
    label_udp_ports_dl_ul: "UDP Ports (DL / UL)",
    label_tx_input_device: "TX Input Device",
    label_rx_output_device: "RX Output Device",
    label_mic_noise_gate: "Mic Noise Gate",
    label_output_gain_mic: "Output Gain & Mic",
    chk_mute_mic: "Mute Mic",
    btn_start_duplex: "Start Full-Duplex Headset Session",
    btn_stop_duplex: "Stop Full-Duplex Session",
    vu_duplex_tx: "OUTGOING TX STREAM (CYAN LINE = NOISE GATE)",
    tile_tx_capture_device: "TX CAPTURE DEVICE",
    tile_uplink_telemetry: "UPLINK TELEMETRY",
    vu_duplex_rx: "INCOMING RX PLAYBACK",
    tile_downlink_packets: "DOWNLINK PACKETS",
    tile_drift_comp: "Drift compensation",

    hw_input_title: "INPUT CAPTURE HARDWARE (COREAUDIO / WASAPI)",
    hw_output_title: "OUTPUT PLAYBACK HARDWARE (SPEAKERS / HEADPHONES)",
    badge_default_in: "DEFAULT IN",
    badge_default_out: "DEFAULT OUT",
};

pub const TEXTS_ZH: Texts = Texts {
    tray_title: "rsrelay — 超低延迟实时音频中继",
    tray_stop_all: "停止所有音频流 (Stop All)",
    tray_quit: "退出 rsrelay (Quit)",

    app_subtitle: "超低延迟实时音频中继 (Real-Time Audio Relay)",
    tab_dual_console: "收发控制台 (TX / RX)",
    tab_full_duplex: "Full-Duplex 无线耳麦",
    tab_hardware_matrix: "音频设备 & LAN",
    lang_toggle_btn: "🌐 EN",
    lang_toggle_hover: "切换界面语言为英文 (Switch to English)",
    btn_refresh_ports: "↻ 刷新设备",
    btn_refresh_ports_hover: "重新扫描系统音频输入/输出硬件设备",
    btn_scanning_lan: "◎ 扫描中...",
    btn_scan_lan: "◎ 扫描 LAN",
    btn_scan_lan_hover: "通过 mDNS 扫描局域网内活跃的 rsrelay 接收端",
    lan_ip_hover: "点击复制本机局域网 (LAN) IPv4 地址",

    btn_stop_all: "■ 全部停止",
    chk_auto_follow_hotplug: "热插拔自动跟随 (Hot-Plug)",

    discovered_lan_peers: "◎ 已发现 LAN 节点 (PEERS)",
    discovered_peer_hover: "点击将此节点 IP:Port 设为目标地址",

    status_streaming: "发送中 LIVE",
    status_listening: "监听中 LIVE",
    status_duplex_active: "双向链路运行中",
    status_standby: "待机 STANDBY",
    device_system_default: "系统默认 (System Default)",

    tx_card_title: "01 // 发送端 (TRANSMITTER / TX)",
    tx_card_subtitle: "系统内录 Loopback / Mic 采集 -> UDP Opus/PCM",
    label_target_ip_port: "目标 IP:Port",
    label_audio_source: "音频源 (Source)",
    source_system_loopback: "系统内录 Loopback (SCK / WASAPI)",
    source_microphone: "麦克风输入 (Mic Input)",
    source_synthetic_tone: "内置 440Hz/660Hz 测试信号 (Test Tone)",
    label_input_device: "输入设备 (Input)",
    hover_refresh_input_ports: "刷新输入设备列表",
    label_opus_bitrate: "Opus 码率 (Bitrate)",
    macos_sck_ready: "macOS SCK 免驱内录已就绪",
    macos_perm_required: "需要 macOS 系统录音权限",
    btn_grant_perm: "授权权限",
    hover_grant_perm: "打开 macOS 系统设置 > 隐私与安全性 > 录屏与系统录音授权提示",
    section_latency_profile: "延迟与编码档位 (LATENCY PROFILE)",
    profile_ultra_low: "极低延迟 (Ultra)",
    profile_balanced: "均衡 (Balanced)",
    profile_stable: "稳定抗抖 (Stable)",
    profile_lossless: "无损 (Lossless)",
    btn_start_tx: "启动发送端 (Start TX)",
    btn_stop_tx: "停止发送端 (Stop TX)",
    vu_input_capture: "输入采集电平 (INPUT LEVEL)",
    tile_packets_tx: "已发包 (PACKETS TX)",
    tile_silence_heartbeat: "静音心跳 (HEARTBEAT)",
    tile_silence_sub: "零负载保活帧 (Keepalive)",
    tile_control_telemetry: "控制遥测 (TELEMETRY)",
    awaiting_tx: "等待发送 (Awaiting TX)",

    rx_card_title: "02 // 接收端 (RECEIVER / RX)",
    rx_card_subtitle: "自适应 Jitter Buffer + PI 时钟同步 ASRC -> 扬声器",
    label_bind_ip_port: "监听 IP:Port",
    btn_copy_lan: "复制 LAN",
    label_output_device: "输出设备 (Output)",
    hover_refresh_output_ports: "刷新输出设备列表",
    label_target_buffer: "目标缓冲 (Buffer)",
    label_output_gain: "输出增益 (Gain)",
    chk_advertise_mdns: "通过 mDNS 在局域网广播此接收端 (_rsrelay._udp.local.)",
    btn_start_rx: "启动接收端 (Start RX)",
    btn_stop_rx: "停止接收端 (Stop RX)",
    vu_output_playback: "输出播放电平 (OUTPUT LEVEL)",
    sparkline_jitter_watermark: "缓冲水位曲线 (JITTER BUFFER)",
    sparkline_awaiting: "等待实时音频流数据...",
    tile_packets_rx: "已收包 (PACKETS RX)",
    tile_loss_fec_plc: "丢包与恢复 (LOSS / FEC)",
    tile_pi_clock_asrc: "时钟漂移补偿 (PI ASRC)",

    duplex_card_title: "03 // FULL-DUPLEX 双向无线耳麦控制台",
    duplex_card_subtitle:
        "立体声系统音频下行 (Downlink) + 噪声门控 Mic 上行 (Uplink) 双向同步传输",
    role_host_title: "主机端 HOST (主电脑 Main PC)",
    role_host_desc: "TX: 系统音频内录 (:49153)  •  RX: 接收远端 Mic 上行 (:49155)",
    role_headset_title: "无线耳麦端 HEADSET (副机 / 笔记本)",
    role_headset_desc: "RX: 接收主机系统音频 (:49153)  •  TX: 本地 Mic 降噪回传 (:49155)",
    label_peer_ip: "对端 IP (Peer IP)",
    label_udp_ports_dl_ul: "UDP 端口 (DL / UL)",
    label_tx_input_device: "TX 输入设备",
    label_rx_output_device: "RX 输出设备",
    label_mic_noise_gate: "Mic 噪声门 (Gate)",
    label_output_gain_mic: "输出增益 & Mic",
    chk_mute_mic: "静音 Mic",
    btn_start_duplex: "启动 Full-Duplex 双向耳麦会话",
    btn_stop_duplex: "停止 Full-Duplex 会话",
    vu_duplex_tx: "上行发送电平 TX (青色线 = 噪声门阈值)",
    tile_tx_capture_device: "TX 采集设备",
    tile_uplink_telemetry: "上行遥测 (UPLINK)",
    vu_duplex_rx: "下行播放电平 RX (PLAYBACK)",
    tile_downlink_packets: "下行收包 (DOWNLINK)",
    tile_drift_comp: "晶振漂移补偿 (Drift Comp)",

    hw_input_title: "输入采集硬件设备 (INPUT / COREAUDIO / WASAPI)",
    hw_output_title: "输出播放硬件设备 (OUTPUT / 扬声器 & 耳机)",
    badge_default_in: "默认输入 (DEFAULT IN)",
    badge_default_out: "默认输出 (DEFAULT OUT)",
};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_locale_parsing_and_toggle() {
        assert_eq!(Language::from_locale_str("zh_CN.UTF-8"), Some(Language::Zh));
        assert_eq!(Language::from_locale_str("zh-Hans-CN"), Some(Language::Zh));
        assert_eq!(Language::from_locale_str("en_US.UTF-8"), Some(Language::En));
        assert_eq!(Language::from_locale_str("C.UTF-8"), None);

        assert_eq!(Language::En.toggle(), Language::Zh);
        assert_eq!(Language::Zh.toggle(), Language::En);
        assert_eq!(Language::En.code(), "en");
        assert_eq!(Language::Zh.code(), "zh");
    }

    #[test]
    fn test_texts_completeness() {
        let en = Language::En.texts();
        let zh = Language::Zh.texts();
        assert!(!en.tx_card_title.is_empty());
        assert!(!zh.tx_card_title.is_empty());
        assert_ne!(en.tab_dual_console, zh.tab_dual_console);
        assert_ne!(
            Language::En.status_ready(),
            Language::Zh.status_ready()
        );
    }
}
