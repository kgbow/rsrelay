//! Internationalization (`i18n`) module for `rsrelay-gui` (Quiet Graphite edition).
//!
//! Follows the Quiet Graphite copywriting guidelines:
//! - Natural, concise Chinese-first labels in `Language::Zh` (matching Frames C & C2)
//! - Clean, concise English labels in `Language::En`
//! - Technical tokens (`Opus`, `FEC`, `PCM16`, `kbps`, `dB`, `ms`, `kHz`, `pkt/s`) kept universal

use std::net::{IpAddr, SocketAddr};

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
    /// Auto-detect preferred UI language from environment variables or OS locale.
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
            if let Ok(output) = std::process::Command::new("defaults")
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
            let lang_id = unsafe { GetUserDefaultUILanguage() };
            if (lang_id & 0x03ff) == 0x04 {
                return Self::Zh;
            }
        }

        Self::Zh
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
            Self::En => "Ready.".to_string(),
            Self::Zh => "就绪".to_string(),
        }
    }

    pub fn status_refreshed_ports(self, in_count: usize, out_count: usize) -> String {
        match self {
            Self::En => format!("Refreshed devices: {in_count} input, {out_count} output."),
            Self::Zh => format!("已刷新音频设备：{in_count} 个输入，{out_count} 个输出"),
        }
    }

    pub fn status_hotswapped_output(self, dev_name: &str) -> String {
        match self {
            Self::En => format!("Switched output to default device '{dev_name}'"),
            Self::Zh => format!("已自动切换输出设备至 '{dev_name}'"),
        }
    }

    pub fn status_hotswapped_input(self, dev_name: &str) -> String {
        match self {
            Self::En => format!("Switched input to default device '{dev_name}'"),
            Self::Zh => format!("已自动切换输入设备至 '{dev_name}'"),
        }
    }

    pub fn status_scanning_lan(self) -> String {
        match self {
            Self::En => "Scanning nearby LAN devices...".to_string(),
            Self::Zh => "正在扫描局域网内的附近设备...".to_string(),
        }
    }

    pub fn status_all_stopped(self) -> String {
        match self {
            Self::En => "All audio streams stopped.".to_string(),
            Self::Zh => "已停止全部音频流".to_string(),
        }
    }

    pub fn status_all_stopped_tray(self) -> String {
        match self {
            Self::En => "All streams stopped from System Tray.".to_string(),
            Self::Zh => "已从系统托盘停止全部音频流".to_string(),
        }
    }

    pub fn status_sender_stopped(self) -> String {
        match self {
            Self::En => "Transmitter stopped.".to_string(),
            Self::Zh => "已停止发送".to_string(),
        }
    }

    pub fn status_invalid_target_addr(self, err: &impl std::fmt::Display) -> String {
        match self {
            Self::En => format!("Invalid target address: {err}"),
            Self::Zh => format!("目标地址格式无效: {err}"),
        }
    }

    pub fn status_sender_streaming(
        self,
        device_name: &str,
        sample_rate: u32,
        target_addr: SocketAddr,
    ) -> String {
        match self {
            Self::En => format!("Sending '{device_name}' ({sample_rate} Hz) -> {target_addr}"),
            Self::Zh => format!("正在发送 '{device_name}' ({sample_rate} Hz) -> {target_addr}"),
        }
    }

    pub fn status_sender_failed(self, err: &impl std::fmt::Display) -> String {
        match self {
            Self::En => format!("Failed to start sender: {err}"),
            Self::Zh => format!("启动发送失败: {err}"),
        }
    }

    pub fn status_receiver_stopped(self) -> String {
        match self {
            Self::En => "Receiver stopped.".to_string(),
            Self::Zh => "已停止接收".to_string(),
        }
    }

    pub fn status_invalid_bind_addr(self, err: &impl std::fmt::Display) -> String {
        match self {
            Self::En => format!("Invalid listen address: {err}"),
            Self::Zh => format!("监听地址格式无效: {err}"),
        }
    }

    pub fn status_receiver_listening(
        self,
        bind_addr: SocketAddr,
        device_name: &str,
        sample_rate: u32,
    ) -> String {
        match self {
            Self::En => format!("Listening on {bind_addr} -> '{device_name}' ({sample_rate} Hz)"),
            Self::Zh => format!("正在监听 {bind_addr} -> '{device_name}' ({sample_rate} Hz)"),
        }
    }

    pub fn status_receiver_failed(self, err: &impl std::fmt::Display) -> String {
        match self {
            Self::En => format!("Failed to start receiver: {err}"),
            Self::Zh => format!("启动接收失败: {err}"),
        }
    }

    pub fn status_duplex_stopped(self) -> String {
        match self {
            Self::En => "Duplex call ended.".to_string(),
            Self::Zh => "已结束双工通话".to_string(),
        }
    }

    pub fn status_invalid_peer_ip(self, err: &impl std::fmt::Display) -> String {
        match self {
            Self::En => format!("Invalid peer IP: {err}"),
            Self::Zh => format!("对端 IP 地址无效: {err}"),
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
            Self::En => {
                format!("Duplex [{role:?}] active <-> {peer_ip} (TX: '{tx_dev}', RX: '{rx_dev}')")
            }
            Self::Zh => {
                format!("双工耳麦 [{role:?}] 通话中 <-> {peer_ip} (TX: '{tx_dev}', RX: '{rx_dev}')")
            }
        }
    }

    pub fn status_duplex_failed(self, err: &impl std::fmt::Display) -> String {
        match self {
            Self::En => format!("Failed to start duplex session: {err}"),
            Self::Zh => format!("启动双工耳麦失败: {err}"),
        }
    }

    pub fn status_copied_lan_ip(self, ip: IpAddr) -> String {
        match self {
            Self::En => format!("Copied local IP '{ip}' to clipboard."),
            Self::Zh => format!("已复制本机地址 '{ip}'"),
        }
    }

    pub fn status_copied_rx_endpoint(self, endpoint: &str) -> String {
        match self {
            Self::En => format!("Copied receiver endpoint '{endpoint}' to clipboard."),
            Self::Zh => format!("已复制接收地址 '{endpoint}'"),
        }
    }

    pub fn hover_copy_rx_endpoint(self, endpoint: &str) -> String {
        match self {
            Self::En => format!("Copy receiver address ({endpoint})"),
            Self::Zh => format!("复制接收端地址 ({endpoint})"),
        }
    }

    pub fn status_selected_lan_peer(self, hostname: &str, addr_str: &str) -> String {
        match self {
            Self::En => format!("Selected peer '{hostname}' ({addr_str})"),
            Self::Zh => format!("已选择附近设备 '{hostname}' ({addr_str})"),
        }
    }

    pub fn status_auto_discovered_peer(self, hostname: &str, addr_str: &str) -> String {
        match self {
            Self::En => format!("Auto-discovered LAN peer '{hostname}' ({addr_str})"),
            Self::Zh => format!("已自动识别局域网设备 '{hostname}' ({addr_str})"),
        }
    }

    pub fn status_sck_granted(self) -> String {
        match self {
            Self::En => "macOS ScreenCaptureKit permission granted.".to_string(),
            Self::Zh => "macOS 系统内录权限已授权".to_string(),
        }
    }

    pub fn status_sck_prompted(self) -> String {
        match self {
            Self::En => "Opened macOS Screen & System Audio Recording settings.".to_string(),
            Self::Zh => "已打开 macOS『录屏与系统录音』权限设置".to_string(),
        }
    }

    pub fn status_installed_macos_app(self, path: &str) -> String {
        match self {
            Self::En => format!("Installed '{path}' and revealed in Finder."),
            Self::Zh => format!("已安装 '{path}' 并在 Finder 中打开"),
        }
    }

    pub fn status_installing_vmic_driver(self) -> String {
        match self {
            Self::En => "Installing virtual microphone driver...".to_string(),
            Self::Zh => "正在编译并安装虚拟麦克风驱动，请在系统弹窗中确认授权...".to_string(),
        }
    }

    pub fn status_vmic_driver_installed(self, detail: &str) -> String {
        match self {
            Self::En => format!("Virtual microphone driver ready ({detail})."),
            Self::Zh => format!("虚拟麦克风驱动已就绪 ({detail})"),
        }
    }

    pub fn status_vmic_driver_failed(self, err: &str) -> String {
        match self {
            Self::En => format!("Virtual mic install failed: {err}"),
            Self::Zh => format!("虚拟麦克风安装失败: {err}"),
        }
    }

    pub fn status_language_switched(self) -> String {
        match self {
            Self::En => "Switched language to English.".to_string(),
            Self::Zh => "已切换为中文".to_string(),
        }
    }

    pub fn status_theme_switched(self, mode_label: &str) -> String {
        match self {
            Self::En => format!("Theme set to {mode_label}."),
            Self::Zh => format!("已切换主题模式：{mode_label}"),
        }
    }

    pub fn status_entered_tray_mode(self) -> String {
        match self {
            Self::En => "Minimized to system tray.".to_string(),
            Self::Zh => "已最小化到系统托盘运行".to_string(),
        }
    }

    pub fn status_restored_from_tray(self) -> String {
        match self {
            Self::En => "Restored window from system tray.".to_string(),
            Self::Zh => "已从系统托盘恢复窗口".to_string(),
        }
    }

    pub fn format_buffer_summary(self, current_ms: Option<f32>, target_ms: u16) -> String {
        match (self, current_ms) {
            (Self::En, Some(_)) | (Self::En, None) => format!("Target {target_ms} ms"),
            (Self::Zh, Some(_)) | (Self::Zh, None) => format!("目标 {target_ms} ms"),
        }
    }
}

/// Compile-time verified UI string table for Quiet Graphite (Frames C & C2).
pub struct Texts {
    // Window & System Tray
    pub tray_title: &'static str,
    pub tray_show_console: &'static str,
    pub tray_hide_console: &'static str,
    pub tray_toggle_tx: &'static str,
    pub tray_toggle_rx: &'static str,
    pub tray_toggle_duplex: &'static str,
    pub tray_stop_all: &'static str,
    pub tray_quit: &'static str,

    // Sidebar Navigation & Footer
    pub nav_section_mode: &'static str,
    pub tab_dual_console: &'static str,
    pub tab_full_duplex: &'static str,
    pub tab_hardware_matrix: &'static str,
    pub nav_section_nearby: &'static str,
    pub sidebar_local_addr: &'static str,
    pub chk_auto_follow_hotplug: &'static str,
    pub theme_system: &'static str,
    pub theme_light: &'static str,
    pub theme_dark: &'static str,
    pub btn_minimize_tray_hover: &'static str,
    pub lan_ip_hover: &'static str,
    pub btn_scan_lan_hover: &'static str,
    pub btn_refresh_ports: &'static str,
    pub btn_refresh_ports_hover: &'static str,
    pub discovered_peer_hover: &'static str,

    // Page Headers (C, C2, Audio Devices)
    pub page_c_title: &'static str,
    pub page_c_subtitle: &'static str,
    pub page_c2_title: &'static str,
    pub page_c2_subtitle: &'static str,
    pub page_hw_title: &'static str,
    pub page_hw_subtitle: &'static str,
    pub btn_stop_all: &'static str,

    // Status Chips & Common Labels
    pub status_sending: &'static str,
    pub status_receiving: &'static str,
    pub status_in_call: &'static str,
    pub status_healthy: &'static str,
    pub status_speaking: &'static str,
    pub status_muted: &'static str,
    pub status_standby: &'static str,
    pub device_system_default: &'static str,
    pub device_default_speaker: &'static str,
    pub channel_stereo: &'static str,
    pub channel_mono: &'static str,

    // Frame C — Left Card: 发送 (Transmitter)
    pub tx_card_title: &'static str,
    pub btn_start_tx: &'static str,
    pub btn_stop_tx: &'static str,
    pub hero_send_to: &'static str,
    pub stat_bitrate: &'static str,
    pub stat_rtt: &'static str,
    pub stat_packets_sent: &'static str,
    pub unit_packets: &'static str,
    pub chart_last_20s: &'static str,
    pub chart_tx_idle_hint: &'static str,
    pub label_audio_source: &'static str,
    pub source_system_loopback: &'static str,
    pub source_microphone: &'static str,
    pub source_synthetic_tone: &'static str,
    pub label_input_device: &'static str,
    pub label_target_addr: &'static str,
    pub combo_scan_lan: &'static str,
    pub combo_no_lan_peers: &'static str,
    pub combo_local_loopback: &'static str,
    pub combo_local_lan: &'static str,
    pub label_quality_latency: &'static str,
    pub profile_ultra_low: &'static str,
    pub profile_balanced: &'static str,
    pub profile_stable: &'static str,
    pub profile_lossless: &'static str,
    pub macos_sck_ready: &'static str,
    pub macos_perm_required: &'static str,
    pub btn_grant_perm: &'static str,
    pub hover_grant_perm: &'static str,
    pub btn_install_macos_app: &'static str,
    pub hover_install_macos_app: &'static str,

    // Frame C — Right Card: 接收 (Receiver)
    pub rx_card_title: &'static str,
    pub btn_start_rx: &'static str,
    pub btn_stop_rx: &'static str,
    pub hero_listen_any: &'static str,
    pub hero_listen_waiting: &'static str,
    pub hero_receiving_from: &'static str,
    pub label_source_addr: &'static str,
    pub source_waiting_any: &'static str,
    pub source_addr_hover: &'static str,
    pub route_adaptive_jb: &'static str,
    pub stat_buffer_level: &'static str,
    pub stat_clock_comp: &'static str,
    pub stat_loss_repaired: &'static str,
    pub chart_jitter_buffer: &'static str,
    pub chart_rx_idle_hint: &'static str,
    pub label_listen_port: &'static str,
    pub label_output_device: &'static str,
    pub label_target_buffer: &'static str,
    pub label_output_gain: &'static str,
    pub chk_advertise_mdns: &'static str,

    // Frame C2 — 双工耳麦 / 跨机共享 (Full-Duplex & Symmetric Win+Mac Sharing)
    pub btn_start_duplex: &'static str,
    pub btn_stop_duplex: &'static str,
    pub conn_local_role: &'static str,
    pub role_host_short: &'static str,
    pub role_headset_short: &'static str,
    pub role_symmetric_short: &'static str,
    pub conn_codec: &'static str,
    pub label_bus_direction: &'static str,
    pub bus_off: &'static str,
    pub media_mode_send: &'static str,
    pub media_mode_recv: &'static str,
    pub media_mode_bi: &'static str,
    pub mic_mode_send: &'static str,
    pub mic_mode_recv: &'static str,
    pub mic_mode_bi: &'static str,
    pub c2_listen_title: &'static str,
    pub c2_listen_subtitle: &'static str,
    pub c2_media_bi_title: &'static str,
    pub c2_media_bi_subtitle: &'static str,
    pub c2_media_off_title: &'static str,
    pub c2_media_off_subtitle: &'static str,
    pub c2_host_rx_title: &'static str,
    pub c2_host_rx_subtitle: &'static str,
    pub c2_mic_bi_title: &'static str,
    pub c2_mic_bi_subtitle: &'static str,
    pub c2_mic_off_title: &'static str,
    pub c2_mic_off_subtitle: &'static str,
    pub c2_host_rx_device_label: &'static str,
    pub c2_host_rx_auto_vmic: &'static str,
    pub c2_vmic_guard_option: &'static str,
    pub c2_vmic_missing_warn: &'static str,
    pub c2_vmic_audiorelay_hint: &'static str,
    pub c2_vmic_ready_prefix: &'static str,
    pub btn_install_macos_vmic: &'static str,
    pub btn_install_win_vbcable: &'static str,
    pub btn_installing_vmic: &'static str,
    pub c2_jb_chart_title: &'static str,
    pub label_channels: &'static str,
    pub c2_mic_title: &'static str,
    pub c2_mic_subtitle: &'static str,
    pub c2_host_tx_title: &'static str,
    pub c2_host_tx_subtitle: &'static str,
    pub c2_host_tx_device_label: &'static str,
    pub c2_host_tx_default_loopback: &'static str,
    pub c2_gate_marker_prefix: &'static str,
    pub stat_level: &'static str,
    pub stat_gated_frames: &'static str,
    pub c2_voice_chart_title: &'static str,
    pub label_microphone: &'static str,
    pub label_noise_gate_threshold: &'static str,
    pub label_mic_mute: &'static str,

    // Tab 3 — 音频设备 (Hardware Matrix)
    pub hw_input_title: &'static str,
    pub hw_input_subtitle: &'static str,
    pub hw_output_title: &'static str,
    pub hw_output_subtitle: &'static str,
    pub badge_default_in: &'static str,
    pub badge_default_out: &'static str,
}

pub const TEXTS_ZH: Texts = Texts {
    tray_title: "rsrelay — 实时音频中继",
    tray_show_console: "打开主窗口",
    tray_hide_console: "最小化到托盘",
    tray_toggle_tx: "开始 / 停止发送",
    tray_toggle_rx: "开始 / 停止接收",
    tray_toggle_duplex: "开始 / 结束双工通话",
    tray_stop_all: "全部停止",
    tray_quit: "退出 rsrelay",

    nav_section_mode: "工作模式",
    tab_dual_console: "收发",
    tab_full_duplex: "双工耳麦",
    tab_hardware_matrix: "音频设备",
    nav_section_nearby: "附近设备",
    sidebar_local_addr: "本机地址",
    chk_auto_follow_hotplug: "自动跟随默认设备",
    theme_system: "跟随系统",
    theme_light: "浅色",
    theme_dark: "深色",
    btn_minimize_tray_hover: "最小化到托盘",
    lan_ip_hover: "点击复制本机局域网地址",
    btn_scan_lan_hover: "扫描局域网内的附近设备",
    btn_refresh_ports: "刷新设备",
    btn_refresh_ports_hover: "重新枚举本机音频输入与输出设备",
    discovered_peer_hover: "点击选择此设备作为目标地址",

    page_c_title: "收发",
    page_c_subtitle: "把本机声音推送到局域网内的另一台设备，或接收它传来的声音",
    page_c2_title: "双工耳麦",
    page_c2_subtitle: "把这台设备当作另一台电脑的无线耳麦：听它的声音，同时把你的麦克风传回去",
    page_hw_title: "音频设备",
    page_hw_subtitle: "管理本机输入采集与输出播放硬件、默认路由及局域网广播设置",
    btn_stop_all: "全部停止",

    status_sending: "正在发送",
    status_receiving: "正在接收",
    status_in_call: "通话中",
    status_healthy: "正常",
    status_speaking: "正在说话",
    status_muted: "已静音",
    status_standby: "待机",
    device_system_default: "系统默认输入",
    device_default_speaker: "系统默认扬声器",
    channel_stereo: "立体声",
    channel_mono: "单声道",

    tx_card_title: "发送",
    btn_start_tx: "开始发送",
    btn_stop_tx: "停止",
    hero_send_to: "发送到",
    stat_bitrate: "码率",
    stat_rtt: "往返延迟",
    stat_packets_sent: "已发送",
    unit_packets: "包",
    chart_last_20s: "最近 20 秒",
    chart_tx_idle_hint: "开始发送后将在此显示音频波形",
    label_audio_source: "音频来源",
    source_system_loopback: "系统内录",
    source_microphone: "麦克风",
    source_synthetic_tone: "测试信号 (440Hz)",
    label_input_device: "输入设备",
    label_target_addr: "目标地址",
    combo_scan_lan: "↻ 重新扫描局域网设备",
    combo_no_lan_peers: "暂未发现其他局域网设备 (可手动输入)",
    combo_local_loopback: "本机回环",
    combo_local_lan: "本机节点 (LAN)",
    label_quality_latency: "音质与延迟",
    profile_ultra_low: "极低延迟",
    profile_balanced: "均衡",
    profile_stable: "稳定",
    profile_lossless: "无损",
    macos_sck_ready: "macOS 内录已就绪",
    macos_perm_required: "需要系统录音权限",
    btn_grant_perm: "授权权限",
    hover_grant_perm: "打开 macOS 系统设置授权系统录音权限",
    btn_install_macos_app: "安装到应用程序",
    hover_install_macos_app: "将 rsrelay.app 安装到 /Applications 并在 Finder 中显示",

    rx_card_title: "接收",
    btn_start_rx: "开始接收",
    btn_stop_rx: "停止",
    hero_listen_any: "监听地址 · 接受任意来源",
    hero_listen_waiting: "监听地址 · 等待来源接入",
    hero_receiving_from: "正在接收来自",
    label_source_addr: "来源地址",
    source_waiting_any: "等待接入 (任意来源)",
    source_addr_hover: "点击复制此来源地址并设为发送目标",
    route_adaptive_jb: "自适应抖动缓冲",
    stat_buffer_level: "缓冲水位",
    stat_clock_comp: "时钟补偿",
    stat_loss_repaired: "丢包 / 修复",
    chart_jitter_buffer: "抖动缓冲",
    chart_rx_idle_hint: "开始接收后将在此显示缓冲曲线",
    label_listen_port: "监听端口",
    label_output_device: "输出设备",
    label_target_buffer: "目标缓冲",
    label_output_gain: "输出增益",
    chk_advertise_mdns: "在局域网广播此节点 (自动发现)",

    btn_start_duplex: "开始通话",
    btn_stop_duplex: "结束通话",
    conn_local_role: "本机角色",
    role_host_short: "主机电脑",
    role_headset_short: "无线耳麦",
    role_symmetric_short: "双机互享",
    conn_codec: "编码",
    label_bus_direction: "通道流向",
    bus_off: "关闭",
    media_mode_send: "发内录",
    media_mode_recv: "收听",
    media_mode_bi: "双向",
    mic_mode_send: "发本机麦",
    mic_mode_recv: "进虚拟麦",
    mic_mode_bi: "双向",
    c2_listen_title: "播放器收听",
    c2_listen_subtitle: "播放器通道 · 收听对端声音至本机耳机",
    c2_media_bi_title: "播放器双向共享",
    c2_media_bi_subtitle: "播放器通道 · 同时发送本机内录并收听对端声音",
    c2_media_off_title: "播放器通道 (未启用)",
    c2_media_off_subtitle: "仅使用右侧麦克风共享通道",
    c2_host_rx_title: "虚拟麦克风接收",
    c2_host_rx_subtitle: "麦克风通道 · 接收对端语音并注入虚拟麦 (零外放回声)",
    c2_mic_bi_title: "麦克风双向互享",
    c2_mic_bi_subtitle: "麦克风通道 · 发送物理麦克风 + 接收进本机虚拟麦",
    c2_mic_off_title: "麦克风通道 (未启用)",
    c2_mic_off_subtitle: "仅使用左侧播放器共享通道",
    c2_host_rx_device_label: "虚拟麦注入目标",
    c2_host_rx_auto_vmic: "自动 · 虚拟麦克风 (零回声)",
    c2_vmic_guard_option: "防回声静音保护 (待安装虚拟麦)",
    c2_vmic_missing_warn: "未检测到虚拟声卡 · 已启用防回声保护 (不外放对端语音)",
    c2_vmic_audiorelay_hint: "提示：检测到 AudioRelay 后台正在运行，如遇抢占可退出 AudioRelay (驱动仍保留)",
    c2_vmic_ready_prefix: "会议/浏览器麦克风请选：",
    btn_install_macos_vmic: "一键安装虚拟麦克风",
    btn_install_win_vbcable: "安装 VB-CABLE 驱动",
    btn_installing_vmic: "正在安装...",
    c2_jb_chart_title: "抖动缓冲 · 最近 20 秒",
    label_channels: "声道",
    c2_mic_title: "麦克风发送",
    c2_mic_subtitle: "麦克风通道 · 采集本机物理麦发往对端虚拟麦",
    c2_host_tx_title: "系统声音发送",
    c2_host_tx_subtitle: "播放器通道 · 采集本机系统内录发往对端耳机",
    c2_host_tx_device_label: "系统内录声卡",
    c2_host_tx_default_loopback: "系统默认扬声器 (内录)",
    c2_gate_marker_prefix: "▼ 噪声门",
    stat_level: "电平",
    stat_gated_frames: "静音帧（已省流）",
    c2_voice_chart_title: "人声包络 · 最近 20 秒",
    label_microphone: "本机物理麦",
    label_noise_gate_threshold: "噪声门阈值",
    label_mic_mute: "麦克风静音",

    hw_input_title: "输入设备",
    hw_input_subtitle: "本机麦克风与环回采集硬件",
    hw_output_title: "输出设备",
    hw_output_subtitle: "本机扬声器与耳机播放硬件",
    badge_default_in: "默认",
    badge_default_out: "默认",
};

pub const TEXTS_EN: Texts = Texts {
    tray_title: "rsrelay — Audio Relay",
    tray_show_console: "Open Window",
    tray_hide_console: "Minimize to Tray",
    tray_toggle_tx: "Start / Stop Transmit",
    tray_toggle_rx: "Start / Stop Receive",
    tray_toggle_duplex: "Start / End Duplex Call",
    tray_stop_all: "Stop All",
    tray_quit: "Quit rsrelay",

    nav_section_mode: "MODE",
    tab_dual_console: "Send & Receive",
    tab_full_duplex: "Duplex Headset",
    tab_hardware_matrix: "Audio Devices",
    nav_section_nearby: "NEARBY DEVICES",
    sidebar_local_addr: "Local Address",
    chk_auto_follow_hotplug: "Auto-follow default",
    theme_system: "System",
    theme_light: "Light",
    theme_dark: "Dark",
    btn_minimize_tray_hover: "Minimize to system tray",
    lan_ip_hover: "Click to copy local LAN IP address",
    btn_scan_lan_hover: "Scan LAN for nearby devices",
    btn_refresh_ports: "Refresh Devices",
    btn_refresh_ports_hover: "Re-enumerate system audio input and output devices",
    discovered_peer_hover: "Click to select this peer as target",

    page_c_title: "Send & Receive",
    page_c_subtitle:
        "Stream this machine's audio to another device on the LAN, or listen to incoming audio",
    page_c2_title: "Duplex Headset",
    page_c2_subtitle:
        "Use this device as a wireless headset for another computer: listen to its audio and send your mic back",
    page_hw_title: "Audio Devices",
    page_hw_subtitle:
        "Inspect local capture and playback hardware, default routing, and LAN discovery settings",
    btn_stop_all: "Stop All",

    status_sending: "Sending",
    status_receiving: "Receiving",
    status_in_call: "In Call",
    status_healthy: "Healthy",
    status_speaking: "Speaking",
    status_muted: "Muted",
    status_standby: "Standby",
    device_system_default: "System Default Input",
    device_default_speaker: "System Default Speaker",
    channel_stereo: "Stereo",
    channel_mono: "Mono",

    tx_card_title: "Transmit",
    btn_start_tx: "Start Sending",
    btn_stop_tx: "Stop",
    hero_send_to: "Send to",
    stat_bitrate: "Bitrate",
    stat_rtt: "Round-Trip",
    stat_packets_sent: "Sent",
    unit_packets: "pkts",
    chart_last_20s: "Last 20s",
    chart_tx_idle_hint: "Audio waveform will appear when sending",
    label_audio_source: "Audio Source",
    source_system_loopback: "System Loopback",
    source_microphone: "Microphone",
    source_synthetic_tone: "Test Tone (440Hz)",
    label_input_device: "Input Device",
    label_target_addr: "Target Address",
    combo_scan_lan: "↻ Rescan LAN Devices",
    combo_no_lan_peers: "No other LAN peers found yet (manual input ready)",
    combo_local_loopback: "Localhost",
    combo_local_lan: "This Machine (LAN)",
    label_quality_latency: "Quality & Latency",
    profile_ultra_low: "Ultra-Low",
    profile_balanced: "Balanced",
    profile_stable: "Stable",
    profile_lossless: "Lossless",
    macos_sck_ready: "macOS SCK Ready",
    macos_perm_required: "Audio Permission Needed",
    btn_grant_perm: "Grant Permission",
    hover_grant_perm: "Open macOS System Settings for Screen & System Audio Recording",
    btn_install_macos_app: "Install App",
    hover_install_macos_app: "Install /Applications/rsrelay.app and reveal in Finder",

    rx_card_title: "Receive",
    btn_start_rx: "Start Receiving",
    btn_stop_rx: "Stop",
    hero_listen_any: "Listen Address · Any Source",
    hero_listen_waiting: "Listen Address · Waiting for Source",
    hero_receiving_from: "Receiving from",
    label_source_addr: "Source Address",
    source_waiting_any: "Waiting (Any Source)",
    source_addr_hover: "Click to copy source address and use as target",
    route_adaptive_jb: "Adaptive Jitter Buffer",
    stat_buffer_level: "Buffer Level",
    stat_clock_comp: "Clock Drift",
    stat_loss_repaired: "Lost / Fixed",
    chart_jitter_buffer: "Jitter Buffer",
    chart_rx_idle_hint: "Buffer curve will appear when receiving",
    label_listen_port: "Listen Port",
    label_output_device: "Output Device",
    label_target_buffer: "Target Buffer",
    label_output_gain: "Output Gain",
    chk_advertise_mdns: "Advertise node on LAN (Auto-Discovery)",

    btn_start_duplex: "Start Call",
    btn_stop_duplex: "End Call",
    conn_local_role: "Local Role",
    role_host_short: "Host PC",
    role_headset_short: "Headset",
    role_symmetric_short: "Dual Share",
    conn_codec: "Codec",
    label_bus_direction: "Bus Flow",
    bus_off: "Off",
    media_mode_send: "Send",
    media_mode_recv: "Listen",
    media_mode_bi: "Both",
    mic_mode_send: "Send Mic",
    mic_mode_recv: "To VMic",
    mic_mode_bi: "Both",
    c2_listen_title: "Player Listen",
    c2_listen_subtitle: "Media Bus · Play peer system audio to headphones",
    c2_media_bi_title: "Player Bidirectional",
    c2_media_bi_subtitle: "Media Bus · Send local loopback & listen to peer",
    c2_media_off_title: "Player Bus (Off)",
    c2_media_off_subtitle: "Only Microphone Bus active",
    c2_host_rx_title: "Virtual Mic Bridge",
    c2_host_rx_subtitle: "Mic Bus · Inject peer voice into Virtual Mic (echo-free)",
    c2_mic_bi_title: "Mic Bidirectional",
    c2_mic_bi_subtitle: "Mic Bus · Send physical mic & receive into Virtual Mic",
    c2_mic_off_title: "Mic Bus (Off)",
    c2_mic_off_subtitle: "Only Player Bus active",
    c2_host_rx_device_label: "Virtual Mic Sink",
    c2_host_rx_auto_vmic: "Auto · Virtual Mic (Echo-Free)",
    c2_vmic_guard_option: "Echo-Free Guard (Driver Needed)",
    c2_vmic_missing_warn: "No virtual mic detected · Echo-Free Guard active (muted from speakers)",
    c2_vmic_audiorelay_hint: "Tip: AudioRelay is running in background; quit AudioRelay if Virtual Mic contends",
    c2_vmic_ready_prefix: "Select mic in apps: ",
    btn_install_macos_vmic: "Install Virtual Mic",
    btn_install_win_vbcable: "Install VB-CABLE",
    btn_installing_vmic: "Installing...",
    c2_jb_chart_title: "Jitter Buffer · Last 20s",
    label_channels: "Channels",
    c2_mic_title: "Mic Transmit",
    c2_mic_subtitle: "Mic Bus · Stream physical mic to peer Virtual Mic",
    c2_host_tx_title: "System Audio Send",
    c2_host_tx_subtitle: "Media Bus · Stream local loopback to peer headphones",
    c2_host_tx_device_label: "Loopback Source",
    c2_host_tx_default_loopback: "Default Speaker (Loopback)",
    c2_gate_marker_prefix: "▼ Gate",
    stat_level: "Level",
    stat_gated_frames: "Gated Frames (Saved)",
    c2_voice_chart_title: "Voice Envelope · Last 20s",
    label_microphone: "Physical Mic",
    label_noise_gate_threshold: "Noise Gate",
    label_mic_mute: "Mute Mic",

    hw_input_title: "Input Devices",
    hw_input_subtitle: "Local microphone and loopback capture hardware",
    hw_output_title: "Output Devices",
    hw_output_subtitle: "Local speaker and headphone playback hardware",
    badge_default_in: "Default",
    badge_default_out: "Default",
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
