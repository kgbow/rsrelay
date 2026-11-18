//! Internationalization (`i18n`) module for `rsrelay-gui` (Unified Visual Topology edition).
//!
//! Follows the Quiet Graphite copywriting guidelines:
//! - Natural, concise Chinese-first labels in `Language::Zh`
//! - Clean, concise English labels in `Language::En`
//! - Technical tokens (`Opus`, `FEC`, `PCM16`, `kbps`, `dB`, `ms`, `kHz`, `ASRC`) kept universal

use std::net::IpAddr;

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
            Self::En => "Ready — choose a preset and click Connect.".to_string(),
            Self::Zh => "就绪 — 选择场景或设备后点击「开始连接」".to_string(),
        }
    }

    pub fn status_refreshed_ports(self, in_count: usize, out_count: usize) -> String {
        match self {
            Self::En => format!("Refreshed devices: {in_count} input, {out_count} output."),
            Self::Zh => format!("已刷新音频设备：{in_count} 个输入，{out_count} 个输出"),
        }
    }

    pub fn status_hotswapped_device(self, dev_name: &str) -> String {
        match self {
            Self::En => format!("Auto-switched to default audio device '{dev_name}'."),
            Self::Zh => format!("已自动跟随默认音频设备 '{dev_name}'"),
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
            Self::En => "Audio link disconnected.".to_string(),
            Self::Zh => "已断开音频连接".to_string(),
        }
    }

    pub fn status_all_stopped_tray(self) -> String {
        match self {
            Self::En => "Disconnected from System Tray.".to_string(),
            Self::Zh => "已从系统托盘断开音频连接".to_string(),
        }
    }

    pub fn status_invalid_peer_ip(self, err: &impl std::fmt::Display) -> String {
        match self {
            Self::En => format!("Invalid peer IP address: {err}"),
            Self::Zh => format!("对端 IP 地址格式无效: {err}"),
        }
    }

    pub fn status_both_buses_off(self) -> String {
        match self {
            Self::En => "Enable at least System Audio or Mic Voice before connecting.".to_string(),
            Self::Zh => "请至少开启「电脑声音」或「麦克风语音」中的一路再连接".to_string(),
        }
    }

    pub fn status_link_active(self, peer_ip: IpAddr, summary: &str) -> String {
        match self {
            Self::En => format!("Connected <-> {peer_ip} ({summary})"),
            Self::Zh => format!("已建立连接 <-> {peer_ip} ({summary})"),
        }
    }

    pub fn status_link_failed(self, err: &impl std::fmt::Display) -> String {
        match self {
            Self::En => format!("Failed to start audio link: {err}"),
            Self::Zh => format!("启动音频连接失败: {err}"),
        }
    }

    pub fn status_copied_lan_ip(self, ip: IpAddr) -> String {
        match self {
            Self::En => format!("Copied local IP '{ip}' to clipboard."),
            Self::Zh => format!("已复制本机地址 '{ip}'"),
        }
    }

    pub fn status_selected_lan_peer(self, hostname: &str, addr_str: &str) -> String {
        match self {
            Self::En => format!("Selected peer '{hostname}' ({addr_str})"),
            Self::Zh => format!("已选择目标设备 '{hostname}' ({addr_str})"),
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

    pub fn status_installing_vmic_driver(self) -> String {
        match self {
            Self::En => "Installing virtual microphone driver...".to_string(),
            Self::Zh => "正在安装虚拟麦克风驱动，请在系统弹窗中确认授权...".to_string(),
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

    pub fn format_buffer_summary(self, target_ms: u16) -> String {
        match self {
            Self::En => format!("Target {target_ms} ms"),
            Self::Zh => format!("目标 {target_ms} ms"),
        }
    }
}

/// Compile-time verified UI string table for the Unified Visual Topology GUI.
pub struct Texts {
    // Window & System Tray
    pub tray_title: &'static str,
    pub tray_show_console: &'static str,
    pub tray_hide_console: &'static str,
    pub tray_toggle_link: &'static str,
    pub tray_mute_speaker: &'static str,
    pub tray_mute_mic: &'static str,
    pub tray_stop_all: &'static str,
    pub tray_quit: &'static str,

    // Sidebar Navigation & Footer
    pub nav_section_mode: &'static str,
    pub tab_audio_link: &'static str,
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

    // Unified Audio Link Header & Scene Presets
    pub page_link_title: &'static str,
    pub page_link_subtitle: &'static str,
    pub btn_connect: &'static str,
    pub btn_disconnect: &'static str,
    pub status_connected: &'static str,
    pub status_standby: &'static str,
    pub status_muted: &'static str,
    pub signal_idle: &'static str,
    pub signal_excellent: &'static str,
    pub signal_good: &'static str,
    pub signal_loss: &'static str,
    pub signal_hover: &'static str,

    pub preset_headset_title: &'static str,
    pub preset_headset_desc: &'static str,
    pub preset_host_title: &'static str,
    pub preset_host_desc: &'static str,
    pub preset_speaker_title: &'static str,
    pub preset_speaker_desc: &'static str,
    pub preset_mic_title: &'static str,
    pub preset_mic_desc: &'static str,

    // Visual Topology Nodes, Ports & Conduits
    pub node_local_title: &'static str,
    pub node_peer_label: &'static str,
    pub node_peer_click_hint: &'static str,
    pub bridge_live_caption: &'static str,
    pub bridge_standby_caption: &'static str,

    pub conduit_media_label: &'static str,
    pub conduit_mic_label: &'static str,
    pub flow_media_recv: &'static str,
    pub flow_media_send: &'static str,
    pub flow_media_bi: &'static str,
    pub flow_mic_send: &'static str,
    pub flow_mic_recv: &'static str,
    pub flow_mic_bi: &'static str,
    pub flow_off: &'static str,
    pub flow_pill_hover: &'static str,

    pub mute_speaker_hover: &'static str,
    pub mute_loopback_hover: &'static str,
    pub mute_mic_hover: &'static str,
    pub gate_drag_hover: &'static str,
    pub c2_gate_marker_prefix: &'static str,

    pub device_system_default: &'static str,
    pub device_default_speaker: &'static str,
    pub c2_host_tx_default_loopback: &'static str,
    pub c2_host_rx_auto_vmic: &'static str,
    pub c2_vmic_guard_option: &'static str,
    pub c2_vmic_missing_warn: &'static str,
    pub c2_vmic_audiorelay_hint: &'static str,
    pub c2_vmic_ready_prefix: &'static str,
    pub btn_install_macos_vmic: &'static str,
    pub btn_install_win_vbcable: &'static str,
    pub btn_installing_vmic: &'static str,
    pub macos_perm_required: &'static str,
    pub btn_grant_perm: &'static str,
    pub hover_grant_perm: &'static str,

    // Remote Peer Mirror Cards
    pub peer_media_recv_title: &'static str,
    pub peer_media_recv_desc: &'static str,
    pub peer_media_send_title: &'static str,
    pub peer_media_send_desc: &'static str,
    pub peer_media_bi_title: &'static str,
    pub peer_media_bi_desc: &'static str,
    pub peer_media_off_title: &'static str,
    pub peer_media_off_desc: &'static str,

    pub peer_mic_send_title: &'static str,
    pub peer_mic_send_desc: &'static str,
    pub peer_mic_recv_title: &'static str,
    pub peer_mic_recv_desc: &'static str,
    pub peer_mic_bi_title: &'static str,
    pub peer_mic_bi_desc: &'static str,
    pub peer_mic_off_title: &'static str,
    pub peer_mic_off_desc: &'static str,

    // Bottom Bar & Collapsible Pro Telemetry / Advanced Drawer
    pub label_quality_latency: &'static str,
    pub profile_ultra_low: &'static str,
    pub profile_balanced: &'static str,
    pub profile_stable: &'static str,
    pub profile_lossless: &'static str,
    pub btn_drawer_open: &'static str,
    pub btn_drawer_close: &'static str,

    pub drawer_telemetry_title: &'static str,
    pub drawer_advanced_title: &'static str,
    pub stat_buffer_level: &'static str,
    pub stat_clock_comp: &'static str,
    pub stat_loss_repaired: &'static str,
    pub stat_gated_frames: &'static str,
    pub c2_jb_chart_title: &'static str,
    pub c2_voice_chart_title: &'static str,
    pub chart_rx_idle_hint: &'static str,
    pub chart_tx_idle_hint: &'static str,

    pub label_target_addr: &'static str,
    pub combo_scan_lan: &'static str,
    pub combo_no_lan_peers: &'static str,
    pub combo_local_loopback: &'static str,
    pub combo_local_lan: &'static str,
    pub label_media_port: &'static str,
    pub label_mic_port: &'static str,
    pub label_custom_buffer: &'static str,
    pub buffer_auto_profile: &'static str,
    pub label_channels: &'static str,
    pub channel_stereo: &'static str,
    pub channel_mono: &'static str,
    pub label_opus_bitrate: &'static str,
    pub label_test_tone: &'static str,
    pub label_status_log: &'static str,

    // Tab 2 — 音频设备 (Hardware Matrix)
    pub page_hw_title: &'static str,
    pub page_hw_subtitle: &'static str,
    pub hw_input_title: &'static str,
    pub hw_input_subtitle: &'static str,
    pub hw_output_title: &'static str,
    pub hw_output_subtitle: &'static str,
    pub badge_default_in: &'static str,
    pub badge_default_out: &'static str,
    pub chk_advertise_mdns: &'static str,
}

pub const TEXTS_ZH: Texts = Texts {
    tray_title: "rsrelay — 实时音频连接",
    tray_show_console: "打开主窗口",
    tray_hide_console: "最小化到托盘",
    tray_toggle_link: "开始 / 断开音频连接",
    tray_mute_speaker: "静音 / 恢复本机播放",
    tray_mute_mic: "静音 / 恢复麦克风",
    tray_stop_all: "断开连接",
    tray_quit: "退出 rsrelay",

    nav_section_mode: "控制台",
    tab_audio_link: "音频连接",
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
    discovered_peer_hover: "点击选择此设备作为连接目标",

    page_link_title: "音频连接",
    page_link_subtitle: "直观连接局域网电脑，按需组合电脑声音收听与无线麦克风",
    btn_connect: "开始连接",
    btn_disconnect: "断开连接",
    status_connected: "已连接",
    status_standby: "待机",
    status_muted: "已静音",
    signal_idle: "等待连接",
    signal_excellent: "极佳",
    signal_good: "稳定",
    signal_loss: "有丢包",
    signal_hover: "点击展开或收起实时波形与高级参数",

    preset_headset_title: "当无线耳麦",
    preset_headset_desc: "听对方声音 · 发本机麦克风",
    preset_host_title: "作为主机电脑",
    preset_host_desc: "发本机声音 · 收进虚拟麦",
    preset_speaker_title: "仅当无线音箱",
    preset_speaker_desc: "只听对方声音 · 麦克风关闭",
    preset_mic_title: "仅当无线麦克风",
    preset_mic_desc: "只发本机麦 · 声音通道关闭",

    node_local_title: "本机",
    node_peer_label: "目标电脑",
    node_peer_click_hint: "点击切换局域网设备或手动输入 IP",
    bridge_live_caption: "UDP 低延迟实时传输中",
    bridge_standby_caption: "就绪 · 点击右上角开始连接",

    conduit_media_label: "电脑声音",
    conduit_mic_label: "麦克风语音",
    flow_media_recv: "听对方声音",
    flow_media_send: "发本机声音",
    flow_media_bi: "双向共享",
    flow_mic_send: "发本机麦克风",
    flow_mic_recv: "收进虚拟麦",
    flow_mic_bi: "双向互享",
    flow_off: "已关闭",
    flow_pill_hover: "点击切换此通道流向或开关",

    mute_speaker_hover: "点击图标静音 / 恢复本机耳机播放",
    mute_loopback_hover: "点击图标静音 / 恢复发送本机系统声音",
    mute_mic_hover: "点击图标静音 / 恢复本机麦克风",
    gate_drag_hover: "左右拖动 ▼ 游标直接调节噪声门阈值（低于此音量的环境底噪自动静音省流）",
    c2_gate_marker_prefix: "▼ 噪声门",

    device_system_default: "系统默认麦克风",
    device_default_speaker: "系统默认扬声器",
    c2_host_tx_default_loopback: "系统默认内录",
    c2_host_rx_auto_vmic: "自动 · 虚拟麦克风",
    c2_vmic_guard_option: "防回声保护 (待安装虚拟麦)",
    c2_vmic_missing_warn: "未检测到虚拟声卡 · 已启用防回声保护",
    c2_vmic_audiorelay_hint: "检测到 AudioRelay 后台运行中，如遇抢占可退出它",
    c2_vmic_ready_prefix: "会议/游戏麦请选：",
    btn_install_macos_vmic: "一键安装虚拟麦",
    btn_install_win_vbcable: "安装 VB-CABLE",
    btn_installing_vmic: "正在安装...",
    macos_perm_required: "需要系统内录权限",
    btn_grant_perm: "点击授权系统内录",
    hover_grant_perm: "打开 macOS 系统设置授权录屏与系统录音权限",

    peer_media_recv_title: "对端系统内录",
    peer_media_recv_desc: "采集对方电脑正在播放的游戏、影音或会议声，传到本机耳机收听",
    peer_media_send_title: "对端耳机 / 扬声器",
    peer_media_send_desc: "把本机正在播放的系统声音实时推送到对方电脑播放",
    peer_media_bi_title: "对端内录 ⇄ 扬声器",
    peer_media_bi_desc: "两台电脑同时互相收听对方的系统声音",
    peer_media_off_title: "声音通道已关闭",
    peer_media_off_desc: "当前不传输系统声音，点击中间胶囊按钮可开启",

    peer_mic_send_title: "对端虚拟麦克风",
    peer_mic_send_desc: "在对方微信 / 腾讯会议 / Discord / 游戏中选虚拟麦，即可用本机麦说话",
    peer_mic_recv_title: "对端物理麦克风",
    peer_mic_recv_desc: "接收对方耳麦语音并注入本机虚拟麦（不外放零回声）",
    peer_mic_bi_title: "对端物理麦 ⇄ 虚拟麦",
    peer_mic_bi_desc: "两台电脑互相把物理麦克风注入对方的虚拟麦克风",
    peer_mic_off_title: "麦克风通道已关闭",
    peer_mic_off_desc: "当前不传输麦克风语音，点击中间胶囊按钮可开启",

    label_quality_latency: "音质与延迟",
    profile_ultra_low: "极低延迟",
    profile_balanced: "均衡抗抖动",
    profile_stable: "穿墙稳定",
    profile_lossless: "无损 PCM",
    btn_drawer_open: "实时波形与高级参数",
    btn_drawer_close: "收起波形与高级参数",

    drawer_telemetry_title: "实时声学与网络遥测",
    drawer_advanced_title: "高级网络与声学参数",
    stat_buffer_level: "缓冲水位",
    stat_clock_comp: "时钟补偿",
    stat_loss_repaired: "丢包 / 修复",
    stat_gated_frames: "降噪省流帧",
    c2_jb_chart_title: "抖动缓冲水位 · 最近 20 秒",
    c2_voice_chart_title: "人声 / 音频包络 · 最近 20 秒",
    chart_rx_idle_hint: "连接接收后将在此显示实时抖动缓冲曲线",
    chart_tx_idle_hint: "连接发送后将在此显示实时声学包络波形",

    label_target_addr: "目标电脑地址",
    combo_scan_lan: "↻ 重新扫描局域网设备",
    combo_no_lan_peers: "暂未发现其他局域网设备 (可直接输入 IP)",
    combo_local_loopback: "本机回环测试",
    combo_local_lan: "本机局域网节点",
    label_media_port: "声音通道 UDP 端口",
    label_mic_port: "麦克风通道 UDP 端口",
    label_custom_buffer: "自定义目标缓冲",
    buffer_auto_profile: "跟随延迟档位",
    label_channels: "声音声道",
    channel_stereo: "立体声",
    channel_mono: "单声道",
    label_opus_bitrate: "Opus 声音码率",
    label_test_tone: "440Hz 测试正弦波",
    label_status_log: "最近状态",

    page_hw_title: "音频设备",
    page_hw_subtitle: "查看本机输入采集与输出播放硬件、默认路由及局域网自动发现设置",
    hw_input_title: "输入设备",
    hw_input_subtitle: "本机麦克风与虚拟音频采集硬件",
    hw_output_title: "输出设备",
    hw_output_subtitle: "本机扬声器、耳机与虚拟声卡接收端",
    badge_default_in: "默认",
    badge_default_out: "默认",
    chk_advertise_mdns: "在局域网广播此节点 (供其他电脑自动发现)",
};

pub const TEXTS_EN: Texts = Texts {
    tray_title: "rsrelay — Audio Link",
    tray_show_console: "Open Window",
    tray_hide_console: "Minimize to Tray",
    tray_toggle_link: "Connect / Disconnect Audio Link",
    tray_mute_speaker: "Mute / Unmute Speaker",
    tray_mute_mic: "Mute / Unmute Microphone",
    tray_stop_all: "Disconnect",
    tray_quit: "Quit rsrelay",

    nav_section_mode: "WORKSPACE",
    tab_audio_link: "Audio Link",
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
    discovered_peer_hover: "Click to select this device as target peer",

    page_link_title: "Audio Link",
    page_link_subtitle: "Visually route desktop audio and microphone voice across your LAN",
    btn_connect: "Connect",
    btn_disconnect: "Disconnect",
    status_connected: "Connected",
    status_standby: "Standby",
    status_muted: "Muted",
    signal_idle: "Standby",
    signal_excellent: "Excellent",
    signal_good: "Good",
    signal_loss: "Loss",
    signal_hover: "Click to toggle live waveforms & advanced parameters",

    preset_headset_title: "Wireless Headset",
    preset_headset_desc: "Listen to peer · Send mic",
    preset_host_title: "Host PC",
    preset_host_desc: "Send audio · Recv Virtual Mic",
    preset_speaker_title: "Speaker Only",
    preset_speaker_desc: "Listen to peer · Mic off",
    preset_mic_title: "Mic Only",
    preset_mic_desc: "Send mic · Audio off",

    node_local_title: "This Machine",
    node_peer_label: "Target Peer",
    node_peer_click_hint: "Click to choose peer or enter IP",
    bridge_live_caption: "Low-Latency UDP Streaming",
    bridge_standby_caption: "Ready · Click Connect to start",

    conduit_media_label: "System Audio",
    conduit_mic_label: "Mic Voice",
    flow_media_recv: "Listen",
    flow_media_send: "Send Audio",
    flow_media_bi: "Both Ways",
    flow_mic_send: "Send Mic",
    flow_mic_recv: "Into VMic",
    flow_mic_bi: "Both Ways",
    flow_off: "Off",
    flow_pill_hover: "Click to change direction or disable channel",

    mute_speaker_hover: "Click icon to mute / unmute local playback",
    mute_loopback_hover: "Click icon to mute / unmute local system audio send",
    mute_mic_hover: "Click icon to mute / unmute microphone",
    gate_drag_hover: "Drag ▼ marker left/right to set Noise Gate threshold",
    c2_gate_marker_prefix: "▼ Gate",

    device_system_default: "System Default Mic",
    device_default_speaker: "System Default Speaker",
    c2_host_tx_default_loopback: "Default System Audio",
    c2_host_rx_auto_vmic: "Auto · Virtual Mic",
    c2_vmic_guard_option: "Echo-Free Guard (Driver Needed)",
    c2_vmic_missing_warn: "No virtual mic · Echo-Free Guard active",
    c2_vmic_audiorelay_hint: "AudioRelay running; quit it if Virtual Mic contends",
    c2_vmic_ready_prefix: "Select mic in apps: ",
    btn_install_macos_vmic: "Install Virtual Mic",
    btn_install_win_vbcable: "Install VB-CABLE",
    btn_installing_vmic: "Installing...",
    macos_perm_required: "Audio Permission Needed",
    btn_grant_perm: "Grant Audio Permission",
    hover_grant_perm: "Open macOS System Settings for Screen & System Audio Recording",

    peer_media_recv_title: "Peer System Audio",
    peer_media_recv_desc: "Streams peer's game, video, and desktop audio to your headphones",
    peer_media_send_title: "Peer Headphones / Speaker",
    peer_media_send_desc: "Streams this machine's system audio to play on the peer",
    peer_media_bi_title: "Peer Loopback ⇄ Speaker",
    peer_media_bi_desc: "Both machines share and listen to each other's system audio",
    peer_media_off_title: "Audio Channel Off",
    peer_media_off_desc: "No desktop audio streamed; click the center pill to enable",

    peer_mic_send_title: "Peer Virtual Microphone",
    peer_mic_send_desc: "Pick Virtual Mic in peer's Zoom, Discord, or games to speak from here",
    peer_mic_recv_title: "Peer Physical Microphone",
    peer_mic_recv_desc: "Routes peer's headset mic into this machine's Virtual Mic (echo-free)",
    peer_mic_bi_title: "Peer Mic ⇄ Virtual Mic",
    peer_mic_bi_desc: "Both machines bridge physical mics into each other's Virtual Mic",
    peer_mic_off_title: "Mic Channel Off",
    peer_mic_off_desc: "No microphone voice streamed; click the center pill to enable",

    label_quality_latency: "Quality & Latency",
    profile_ultra_low: "Ultra-Low",
    profile_balanced: "Balanced",
    profile_stable: "Stable",
    profile_lossless: "Lossless PCM",
    btn_drawer_open: "Waveforms & Advanced",
    btn_drawer_close: "Hide Advanced",

    drawer_telemetry_title: "Live Audio & Network Telemetry",
    drawer_advanced_title: "Advanced Network & Audio",
    stat_buffer_level: "Buffer Level",
    stat_clock_comp: "Clock Drift",
    stat_loss_repaired: "Lost / Fixed",
    stat_gated_frames: "Gated Frames",
    c2_jb_chart_title: "Jitter Buffer · Last 20s",
    c2_voice_chart_title: "Audio Envelope · Last 20s",
    chart_rx_idle_hint: "Buffer curve appears while receiving audio",
    chart_tx_idle_hint: "Voice/audio envelope appears while streaming",

    label_target_addr: "Target Peer Address",
    combo_scan_lan: "↻ Rescan LAN Devices",
    combo_no_lan_peers: "No other LAN peers found yet (enter IP manually)",
    combo_local_loopback: "Localhost Loopback",
    combo_local_lan: "This Machine (LAN)",
    label_media_port: "Audio UDP Port",
    label_mic_port: "Mic UDP Port",
    label_custom_buffer: "Custom Buffer",
    buffer_auto_profile: "Auto (Profile)",
    label_channels: "Audio Channels",
    channel_stereo: "Stereo",
    channel_mono: "Mono",
    label_opus_bitrate: "Opus Bitrate",
    label_test_tone: "440Hz Test Tone",
    label_status_log: "Latest Status",

    page_hw_title: "Audio Devices",
    page_hw_subtitle:
        "Inspect local capture and playback hardware, default routing, and LAN discovery",
    hw_input_title: "Input Devices",
    hw_input_subtitle: "Local microphone and virtual capture hardware",
    hw_output_title: "Output Devices",
    hw_output_subtitle: "Local speaker, headphone, and virtual mic sinks",
    badge_default_in: "Default",
    badge_default_out: "Default",
    chk_advertise_mdns: "Advertise node on LAN (Auto-Discovery)",
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
        assert!(!en.page_link_title.is_empty());
        assert!(!zh.page_link_title.is_empty());
        assert_ne!(en.tab_audio_link, zh.tab_audio_link);
        assert_ne!(
            Language::En.status_ready(),
            Language::Zh.status_ready()
        );
    }
}
