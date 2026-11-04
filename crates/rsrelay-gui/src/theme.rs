//! "Sonic Obsidian" (声学黑曜石) Design System for `rsrelay-gui`.
//!
//! Defines semantic color tokens, spatial scales, typography helpers, and global
//! `egui::Visuals` configuration tailored for ultra-low-latency pro-audio telemetry.

use eframe::egui::{self, Color32, FontFamily, FontId, Margin, Rounding, Stroke, TextStyle, Vec2};

/// Semantic color palette for the **Sonic Obsidian** studio theme.
pub mod palette {
    use eframe::egui::Color32;

    // === Canvas & Surface Hierarchy ===
    /// Root window canvas background (`#0B0E14` Deep Obsidian)
    pub const BG_CANVAS: Color32 = Color32::from_rgb(11, 14, 20);
    /// Primary card / module surface (`#121722` Studio Slate)
    pub const BG_SURFACE: Color32 = Color32::from_rgb(18, 23, 34);
    /// Elevated interactive surface / hover card (`#192030`)
    pub const BG_ELEVATED: Color32 = Color32::from_rgb(25, 32, 48);
    /// Active / highlighted control fill (`#222B40`)
    pub const BG_ACTIVE: Color32 = Color32::from_rgb(34, 43, 64);
    /// Recessed trench for inputs, meters, and telemetry tiles (`#0D111A`)
    pub const BG_TRENCH: Color32 = Color32::from_rgb(13, 17, 26);

    // === Borders & Hairlines ===
    /// Subtle 1px card border (`#21293A`)
    pub const BORDER_SUBTLE: Color32 = Color32::from_rgb(33, 41, 58);
    /// Interactive widget border (`#2E3A52`)
    pub const BORDER_WIDGET: Color32 = Color32::from_rgb(46, 58, 82);
    /// High-contrast focus / active border (`#00D4AA`)
    pub const BORDER_ACCENT: Color32 = Color32::from_rgb(0, 212, 170);

    // === Brand & Signal Accents ===
    /// Primary Acoustic Mint (`#00D4AA`) — Healthy signal, active TX, primary CTA
    pub const ACCENT_MINT: Color32 = Color32::from_rgb(0, 212, 170);
    /// Darker Mint for button fills (`#00A886`)
    pub const ACCENT_MINT_DIM: Color32 = Color32::from_rgb(0, 168, 134);
    /// Soft Mint tint for active card backgrounds
    pub const ACCENT_MINT_GLOW: Color32 = Color32::from_rgba_premultiplied(0, 42, 34, 65);

    /// Telemetry Cyan (`#38BDF8`) — Network RX, ASRC clock ratio, mDNS discovery
    pub const ACCENT_CYAN: Color32 = Color32::from_rgb(56, 189, 248);
    /// Soft Cyan tint for active receiver badges
    pub const ACCENT_CYAN_GLOW: Color32 = Color32::from_rgba_premultiplied(11, 38, 50, 65);

    /// Full-Duplex Indigo (`#818CF8`) — Bidirectional wireless headset link
    pub const ACCENT_INDIGO: Color32 = Color32::from_rgb(129, 140, 248);

    // === Acoustic & Telemetry Status Colors ===
    /// Nominal audio zone (`-60..-12 dBFS`) & healthy link (`#10B981`)
    pub const STATUS_OK: Color32 = Color32::from_rgb(16, 185, 129);
    /// Headroom warning zone (`-12..-3 dBFS`), FEC recovery, or moderate jitter (`#F59E0B`)
    pub const STATUS_WARN: Color32 = Color32::from_rgb(245, 158, 11);
    /// Clipping zone (`-3..0 dBFS`), packet loss, underrun, or Stop action (`#EF4444`)
    pub const STATUS_DANGER: Color32 = Color32::from_rgb(239, 68, 68);
    /// Darker Crimson for Stop button fill (`#B91C1C`)
    pub const STATUS_DANGER_DIM: Color32 = Color32::from_rgb(185, 28, 28);

    // === Typography Hierarchy ===
    /// Crisp primary text (`#F1F5F9`)
    pub const TEXT_PRIMARY: Color32 = Color32::from_rgb(241, 245, 249);
    /// Secondary control labels (`#94A3B8`)
    pub const TEXT_SECONDARY: Color32 = Color32::from_rgb(148, 163, 184);
    /// Muted hints, units, and inactive metrics (`#64748B`)
    pub const TEXT_MUTED: Color32 = Color32::from_rgb(100, 116, 139);
}

/// Spatial and geometric tokens (8px baseline grid).
pub mod metrics {
    pub const SPACE_XS: f32 = 4.0;
    pub const SPACE_SM: f32 = 8.0;
    pub const SPACE_MD: f32 = 12.0;
    pub const SPACE_LG: f32 = 16.0;
    pub const SPACE_XL: f32 = 22.0;

    pub const RADIUS_CARD: f32 = 10.0;
    pub const RADIUS_WIDGET: f32 = 6.0;
    pub const RADIUS_PILL: f32 = 14.0;

    pub const LABEL_COL_WIDTH: f32 = 108.0;
}

#[cfg(unix)]
fn map_font_file_readonly(path: &str) -> Option<&'static [u8]> {
    use std::os::unix::io::AsRawFd;

    extern "C" {
        fn mmap(
            addr: *mut std::ffi::c_void,
            len: usize,
            prot: i32,
            flags: i32,
            fd: i32,
            offset: i64,
        ) -> *mut std::ffi::c_void;
    }

    const PROT_READ: i32 = 0x01;
    const MAP_PRIVATE: i32 = 0x02;

    let file = std::fs::File::open(path).ok()?;
    let len = usize::try_from(file.metadata().ok()?.len()).ok()?;
    if len == 0 {
        return None;
    }

    // SAFETY: Mapping a read-only system font file for the lifetime of the process.
    // Because it is mapped `PROT_READ | MAP_PRIVATE`, pages are backed directly by
    // the OS unified page cache (`Clean` file-backed memory) with 0 KB of dirty heap.
    unsafe {
        let ptr = mmap(
            std::ptr::null_mut(),
            len,
            PROT_READ,
            MAP_PRIVATE,
            file.as_raw_fd(),
            0,
        );
        if ptr.is_null() || ptr == usize::MAX as *mut std::ffi::c_void {
            return None;
        }
        Some(std::slice::from_raw_parts(ptr.cast::<u8>(), len))
    }
}

fn load_cjk_font_static_slice() -> Option<(&'static str, &'static [u8])> {
    static CJK_FONT_CACHE: std::sync::OnceLock<Option<(&'static str, &'static [u8])>> =
        std::sync::OnceLock::new();

    *CJK_FONT_CACHE.get_or_init(|| {
        let candidate_paths: &[&'static str] = &[
            // macOS system CJK fonts (prefer compact 22MB Hiragino / Arial Unicode before 53MB STHeiti)
            "/System/Library/Fonts/Hiragino Sans GB.ttc",
            "/System/Library/Fonts/Supplemental/Arial Unicode.ttf",
            "/System/Library/Fonts/STHeiti Medium.ttc",
            "/System/Library/Fonts/STHeiti Light.ttc",
            "/System/Library/Fonts/PingFang.ttc",
            // Windows 10/11 system CJK fonts
            r"C:\Windows\Fonts\msyh.ttc",
            r"C:\Windows\Fonts\msyh.ttf",
            r"C:\Windows\Fonts\simhei.ttf",
            r"C:\Windows\Fonts\simsun.ttc",
            // Linux CJK fallback fonts
            "/usr/share/fonts/opentype/noto/NotoSansCJK-Regular.ttc",
            "/usr/share/fonts/truetype/wqy/wqy-microhei.ttc",
        ];

        for &path in candidate_paths {
            #[cfg(unix)]
            if let Some(mapped) = map_font_file_readonly(path) {
                return Some((path, mapped));
            }

            if let Ok(bytes) = std::fs::read(path) {
                let leaked: &'static [u8] = Box::leak(bytes.into_boxed_slice());
                return Some((path, leaked));
            }
        }
        None
    })
}

/// Load native OS CJK fallback fonts (macOS `Hiragino` / `STHeiti` / `Arial Unicode`,
/// Windows `Microsoft YaHei` / `SimHei`) so localized audio device names (e.g. `"外置麦克风"`,
/// `"外置耳机"`, `"扬声器"`) and Chinese UI text render crisply without missing-glyph tofu boxes.
///
/// Uses zero-copy `FontData::from_static` backed by read-only `mmap` on Unix so `epaint`
/// borrows the OS page cache directly (`ab_glyph::FontRef`) without allocating or cloning
/// multi-megabyte font buffers on the heap.
pub fn configure_cjk_fallback_fonts(ctx: &egui::Context) {
    if let Some((path, bytes)) = load_cjk_font_static_slice() {
        let mut fonts = egui::FontDefinitions::default();
        fonts.font_data.insert(
            "system_cjk".to_owned(),
            egui::FontData::from_static(bytes),
        );
        if let Some(prop) = fonts.families.get_mut(&FontFamily::Proportional) {
            prop.push("system_cjk".to_owned());
        }
        if let Some(mono) = fonts.families.get_mut(&FontFamily::Monospace) {
            mono.push("system_cjk".to_owned());
        }
        ctx.set_fonts(fonts);
        tracing::debug!("Mapped native CJK fallback font zero-copy from '{path}'");
    }
}

/// Apply the **Sonic Obsidian** design system to an `egui::Context`.
pub fn apply_studio_theme(ctx: &egui::Context) {
    configure_cjk_fallback_fonts(ctx);
    let mut style = (*ctx.style()).clone();

    // Typography scale
    style.text_styles.insert(
        TextStyle::Heading,
        FontId::new(16.5, FontFamily::Proportional),
    );
    style
        .text_styles
        .insert(TextStyle::Body, FontId::new(13.0, FontFamily::Proportional));
    style.text_styles.insert(
        TextStyle::Button,
        FontId::new(13.0, FontFamily::Proportional),
    );
    style.text_styles.insert(
        TextStyle::Small,
        FontId::new(11.5, FontFamily::Proportional),
    );
    style.text_styles.insert(
        TextStyle::Monospace,
        FontId::new(12.5, FontFamily::Monospace),
    );

    // Global spacing & padding
    style.spacing.item_spacing = Vec2::new(metrics::SPACE_SM, metrics::SPACE_SM);
    style.spacing.window_margin = Margin::same(metrics::SPACE_LG);
    style.spacing.button_padding = Vec2::new(12.0, 6.0);
    style.spacing.interact_size = Vec2::new(44.0, 28.0);
    style.spacing.slider_width = 150.0;
    style.spacing.combo_width = 210.0;

    let mut visuals = egui::Visuals::dark();
    visuals.dark_mode = true;
    visuals.override_text_color = Some(palette::TEXT_PRIMARY);
    visuals.panel_fill = palette::BG_CANVAS;
    visuals.window_fill = palette::BG_SURFACE;
    visuals.faint_bg_color = palette::BG_TRENCH;
    visuals.extreme_bg_color = palette::BG_TRENCH;
    visuals.code_bg_color = palette::BG_TRENCH;

    visuals.window_rounding = Rounding::same(metrics::RADIUS_CARD);
    visuals.menu_rounding = Rounding::same(metrics::RADIUS_WIDGET);
    visuals.window_stroke = Stroke::new(1.0_f32, palette::BORDER_SUBTLE);

    // Non-interactive surfaces (frames, labels, separators)
    visuals.widgets.noninteractive.bg_fill = palette::BG_SURFACE;
    visuals.widgets.noninteractive.weak_bg_fill = palette::BG_SURFACE;
    visuals.widgets.noninteractive.bg_stroke = Stroke::new(1.0_f32, palette::BORDER_SUBTLE);
    visuals.widgets.noninteractive.fg_stroke = Stroke::new(1.0_f32, palette::TEXT_SECONDARY);
    visuals.widgets.noninteractive.rounding = Rounding::same(metrics::RADIUS_WIDGET);

    // Inactive interactive widgets (buttons, combo boxes, text inputs, sliders)
    visuals.widgets.inactive.bg_fill = palette::BG_ELEVATED;
    visuals.widgets.inactive.weak_bg_fill = palette::BG_ELEVATED;
    visuals.widgets.inactive.bg_stroke = Stroke::new(1.0_f32, palette::BORDER_WIDGET);
    visuals.widgets.inactive.fg_stroke = Stroke::new(1.0_f32, palette::TEXT_PRIMARY);
    visuals.widgets.inactive.rounding = Rounding::same(metrics::RADIUS_WIDGET);

    // Hovered widgets
    visuals.widgets.hovered.bg_fill = palette::BG_ACTIVE;
    visuals.widgets.hovered.weak_bg_fill = palette::BG_ACTIVE;
    visuals.widgets.hovered.bg_stroke = Stroke::new(1.0_f32, palette::ACCENT_MINT);
    visuals.widgets.hovered.fg_stroke = Stroke::new(1.5_f32, Color32::WHITE);
    visuals.widgets.hovered.rounding = Rounding::same(metrics::RADIUS_WIDGET);

    // Active / pressed widgets
    visuals.widgets.active.bg_fill = palette::ACCENT_MINT_DIM;
    visuals.widgets.active.weak_bg_fill = palette::ACCENT_MINT_DIM;
    visuals.widgets.active.bg_stroke = Stroke::new(1.0_f32, palette::ACCENT_MINT);
    visuals.widgets.active.fg_stroke = Stroke::new(1.5_f32, Color32::WHITE);
    visuals.widgets.active.rounding = Rounding::same(metrics::RADIUS_WIDGET);

    // Open combo box / menu state
    visuals.widgets.open.bg_fill = palette::BG_ACTIVE;
    visuals.widgets.open.weak_bg_fill = palette::BG_ACTIVE;
    visuals.widgets.open.bg_stroke = Stroke::new(1.0_f32, palette::ACCENT_CYAN);
    visuals.widgets.open.fg_stroke = Stroke::new(1.0_f32, Color32::WHITE);
    visuals.widgets.open.rounding = Rounding::same(metrics::RADIUS_WIDGET);

    // Selection highlight
    visuals.selection.bg_fill = Color32::from_rgba_unmultiplied(0, 212, 170, 55);
    visuals.selection.stroke = Stroke::new(1.0_f32, palette::ACCENT_MINT);

    style.visuals = visuals;
    ctx.set_style(style);
}

/// Construct a styled studio card `egui::Frame`, optionally tinted when a stream is live.
pub fn studio_card_frame(is_active: bool, active_border: Color32) -> egui::Frame {
    let border_color = if is_active {
        active_border
    } else {
        palette::BORDER_SUBTLE
    };
    let width: f32 = if is_active { 1.5 } else { 1.0 };
    egui::Frame::none()
        .fill(palette::BG_SURFACE)
        .stroke(Stroke::new(width, border_color))
        .rounding(Rounding::same(metrics::RADIUS_CARD))
        .inner_margin(Margin::same(metrics::SPACE_LG))
}

/// Construct a recessed sub-panel frame (for telemetry dashboards, peer lists, etc.).
pub fn trench_frame() -> egui::Frame {
    egui::Frame::none()
        .fill(palette::BG_TRENCH)
        .stroke(Stroke::new(1.0_f32, palette::BORDER_SUBTLE))
        .rounding(Rounding::same(metrics::RADIUS_WIDGET))
        .inner_margin(Margin::symmetric(12.0, 10.0))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_studio_theme_and_cjk_font_rendering() {
        let ctx = egui::Context::default();
        apply_studio_theme(&ctx);
        let _ = ctx.run(egui::RawInput::default(), |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                ui.label("外置麦克风 (48000 Hz) -> 外置耳机");
            });
        });
        ctx.fonts(|f| {
            assert!(
                f.has_glyph(&FontId::proportional(14.0), '中'),
                "Proportional font must have CJK glyph '中'"
            );
            assert!(
                f.has_glyph(&FontId::monospace(14.0), '音'),
                "Monospace font must have CJK glyph '音'"
            );
        });
    }
}

