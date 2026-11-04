//! "Quiet Graphite" Design System for `rsrelay-gui`.
//!
//! Implements the Quiet Graphite visual language (Frames C & C2):
//! - Neutral graphite surfaces (`#0E0E10` canvas, `#0B0B0D` sidebar, `#151517` cards, `#1C1C1F` raised controls)
//! - Single signal accent (`#3CE0A0` mint) reserved strictly for live/healthy states
//! - Hairline dividers (`rgba(255, 255, 255, 0.06)`) instead of nested bordered boxes
//! - Monospace typography reserved for numeric telemetry and network addresses

use eframe::egui::{self, Color32, FontFamily, FontId, Margin, Rounding, Stroke, TextStyle, Vec2};

/// Semantic color tokens for the **Quiet Graphite** theme.
pub mod palette {
    use eframe::egui::Color32;

    // === Canvas, Sidebar & Surface Hierarchy ===
    /// Main workspace canvas (`#0E0E10`)
    pub const BG_CANVAS: Color32 = Color32::from_rgb(14, 14, 16);
    /// Left navigation sidebar (`#0B0B0D`)
    pub const BG_SIDEBAR: Color32 = Color32::from_rgb(11, 11, 13);
    /// Card panel surface (`#151517`)
    pub const BG_PANEL: Color32 = Color32::from_rgb(21, 21, 23);
    /// Raised control fill for selects, icon tiles, secondary buttons, active nav (`#1C1C1F`)
    pub const BG_RAISED: Color32 = Color32::from_rgb(28, 28, 31);
    /// Hover surface & active segmented pill fill (`#232327`)
    pub const BG_HOVER: Color32 = Color32::from_rgb(35, 35, 39);

    // Backwards-compatible surface aliases
    pub const BG_SURFACE: Color32 = BG_PANEL;
    pub const BG_ELEVATED: Color32 = BG_RAISED;
    pub const BG_ACTIVE: Color32 = BG_HOVER;
    pub const BG_TRENCH: Color32 = BG_CANVAS;

    // === Hairlines & Borders (Pre-composited on #151517 / #0E0E10 to avoid linear-alpha washout) ===
    /// Primary 1px hairline divider (`#222225`, equivalent to `rgba(255, 255, 255, 0.06)` on `#151517`)
    pub const LINE: Color32 = Color32::from_rgb(34, 34, 37);
    /// Stronger hairline for ghost buttons and dashed guides (`#2C2C30`, equivalent to `rgba(255, 255, 255, 0.10)`)
    pub const LINE_STRONG: Color32 = Color32::from_rgb(44, 44, 48);
    /// Subtle top inset highlight on cards and raised controls (`#26262A`)
    pub const INSET_HIGHLIGHT: Color32 = Color32::from_rgb(38, 38, 42);

    pub const BORDER_SUBTLE: Color32 = LINE;
    pub const BORDER_WIDGET: Color32 = LINE_STRONG;
    pub const BORDER_ACCENT: Color32 = Color32::from_rgb(60, 224, 160);

    // === Signal Accent & Status Colors ===
    /// Live / healthy signal mint (`#3CE0A0`)
    pub const ACCENT: Color32 = Color32::from_rgb(60, 224, 160);
    /// Darker mint for pressed states (`#2BB880`)
    pub const ACCENT_DIM: Color32 = Color32::from_rgb(43, 184, 128);
    /// Pre-composited mint tint for `.chip.live` on `#151517` (`rgba(60, 224, 160, 0.12)` -> `#1A3229`)
    pub const ACCENT_TINT: Color32 = Color32::from_rgb(26, 50, 41);
    /// Idle chip & meter track fill on `#151517` (`rgba(255, 255, 255, 0.05)` -> `#202024`)
    pub const CHIP_IDLE_BG: Color32 = Color32::from_rgb(32, 32, 36);
    /// Active peer highlight in `#0B0B0D` sidebar (`rgba(255, 255, 255, 0.035)` -> `#151518`)
    pub const PEER_ACTIVE_BG: Color32 = Color32::from_rgb(21, 21, 24);
    /// Slider track fill on `#151517` (`rgba(255, 255, 255, 0.08)` -> `#27272B`)
    pub const SLIDER_TRACK_BG: Color32 = Color32::from_rgb(39, 39, 43);

    // Aliases mapped to the unified Quiet Graphite accent
    pub const ACCENT_MINT: Color32 = ACCENT;
    pub const ACCENT_MINT_DIM: Color32 = ACCENT_DIM;
    pub const ACCENT_MINT_GLOW: Color32 = ACCENT_TINT;
    pub const ACCENT_CYAN: Color32 = ACCENT;
    pub const ACCENT_CYAN_GLOW: Color32 = ACCENT_TINT;
    pub const ACCENT_INDIGO: Color32 = ACCENT;

    /// Nominal status (`#3CE0A0`)
    pub const STATUS_OK: Color32 = ACCENT;
    /// Meter headroom warning zone (`#F2B84B`, top 14%)
    pub const STATUS_WARN: Color32 = Color32::from_rgb(242, 184, 75);
    /// Meter clipping zone & Stop button square indicator (`#FF6B5B`)
    pub const STATUS_DANGER: Color32 = Color32::from_rgb(255, 107, 91);
    pub const STATUS_DANGER_DIM: Color32 = Color32::from_rgb(190, 68, 56);

    /// Inactive peer dot & toggle off track (`#4A4A50` / `#2C2C31`)
    pub const DOT_OFFLINE: Color32 = Color32::from_rgb(74, 74, 80);
    pub const TOGGLE_OFF_BG: Color32 = Color32::from_rgb(44, 44, 49);
    pub const TOGGLE_OFF_KNOB: Color32 = Color32::from_rgb(138, 138, 146);

    // === Typography Hierarchy ===
    /// Primary text & primary CTA button fill (`#EDEDEE`)
    pub const TEXT_PRIMARY: Color32 = Color32::from_rgb(237, 237, 238);
    /// Secondary labels & descriptions (`#9C9CA3`)
    pub const TEXT_SECONDARY: Color32 = Color32::from_rgb(156, 156, 163);
    /// Muted captions, units, and scale ticks (`#66666D`)
    pub const TEXT_MUTED: Color32 = Color32::from_rgb(102, 102, 109);
}

/// Spatial and geometric tokens matching Quiet Graphite (Frames C & C2).
pub mod metrics {
    pub const SIDEBAR_WIDTH: f32 = 232.0;

    pub const SPACE_XS: f32 = 4.0;
    pub const SPACE_SM: f32 = 8.0;
    pub const SPACE_MD: f32 = 12.0;
    pub const SPACE_LG: f32 = 16.0;
    pub const SPACE_XL: f32 = 20.0;

    pub const CARD_PADDING_X: f32 = 20.0;
    pub const CARD_PADDING_Y: f32 = 18.0;
    pub const MAIN_PADDING_X: f32 = 28.0;
    pub const MAIN_PADDING_Y: f32 = 22.0;
    pub const SETTING_ROW_HEIGHT: f32 = 50.0;

    pub const RADIUS_CARD: f32 = 14.0;
    pub const RADIUS_BUTTON: f32 = 8.0;
    pub const RADIUS_SELECT: f32 = 7.0;
    pub const RADIUS_WIDGET: f32 = 8.0;
    pub const RADIUS_SEG_INNER: f32 = 6.0;
    pub const RADIUS_CHIP: f32 = 10.0;
    pub const RADIUS_PILL: f32 = 10.0;

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
            // macOS system CJK fonts
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

/// Load native OS CJK fallback fonts so Chinese UI text and localized device names render crisply.
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

/// Apply the **Quiet Graphite** design system to an `egui::Context`.
pub fn apply_studio_theme(ctx: &egui::Context) {
    configure_cjk_fallback_fonts(ctx);
    let mut style = (*ctx.style()).clone();

    // Typography scale matching Quiet Graphite (H1 20, Card Title 15, Body 13, Caption 11.5, Mono 12.5)
    style.text_styles.insert(
        TextStyle::Heading,
        FontId::new(20.0, FontFamily::Proportional),
    );
    style
        .text_styles
        .insert(TextStyle::Body, FontId::new(13.0, FontFamily::Proportional));
    style.text_styles.insert(
        TextStyle::Button,
        FontId::new(12.5, FontFamily::Proportional),
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
    style.spacing.item_spacing = Vec2::new(8.0, 6.0);
    style.spacing.window_margin = Margin::same(metrics::SPACE_LG);
    style.spacing.button_padding = Vec2::new(12.0, 6.0);
    style.spacing.interact_size = Vec2::new(40.0, 30.0);
    style.spacing.slider_width = 150.0;
    style.spacing.combo_width = 190.0;

    let mut visuals = egui::Visuals::dark();
    visuals.dark_mode = true;
    visuals.override_text_color = Some(palette::TEXT_PRIMARY);
    visuals.panel_fill = palette::BG_CANVAS;
    visuals.window_fill = palette::BG_PANEL;
    visuals.faint_bg_color = palette::BG_CANVAS;
    visuals.extreme_bg_color = palette::BG_RAISED;
    visuals.code_bg_color = palette::BG_RAISED;

    visuals.window_rounding = Rounding::same(metrics::RADIUS_CARD);
    visuals.menu_rounding = Rounding::same(metrics::RADIUS_BUTTON);
    visuals.window_stroke = Stroke::new(1.0_f32, palette::LINE_STRONG);

    // Non-interactive surfaces (frames, labels, separators)
    visuals.widgets.noninteractive.bg_fill = palette::BG_PANEL;
    visuals.widgets.noninteractive.weak_bg_fill = palette::BG_PANEL;
    visuals.widgets.noninteractive.bg_stroke = Stroke::new(1.0_f32, palette::LINE);
    visuals.widgets.noninteractive.fg_stroke = Stroke::new(1.0_f32, palette::TEXT_SECONDARY);
    visuals.widgets.noninteractive.rounding = Rounding::same(metrics::RADIUS_SELECT);

    // Inactive interactive widgets (selects, secondary buttons, text inputs)
    visuals.widgets.inactive.bg_fill = palette::BG_RAISED;
    visuals.widgets.inactive.weak_bg_fill = palette::BG_RAISED;
    visuals.widgets.inactive.bg_stroke = Stroke::new(1.0_f32, palette::LINE);
    visuals.widgets.inactive.fg_stroke = Stroke::new(1.0_f32, palette::TEXT_PRIMARY);
    visuals.widgets.inactive.rounding = Rounding::same(metrics::RADIUS_SELECT);

    // Hovered widgets
    visuals.widgets.hovered.bg_fill = palette::BG_HOVER;
    visuals.widgets.hovered.weak_bg_fill = palette::BG_HOVER;
    visuals.widgets.hovered.bg_stroke = Stroke::new(1.0_f32, palette::LINE_STRONG);
    visuals.widgets.hovered.fg_stroke = Stroke::new(1.2_f32, palette::TEXT_PRIMARY);
    visuals.widgets.hovered.rounding = Rounding::same(metrics::RADIUS_SELECT);

    // Active / pressed widgets
    visuals.widgets.active.bg_fill = palette::BG_HOVER;
    visuals.widgets.active.weak_bg_fill = palette::BG_HOVER;
    visuals.widgets.active.bg_stroke = Stroke::new(1.0_f32, palette::LINE_STRONG);
    visuals.widgets.active.fg_stroke = Stroke::new(1.2_f32, palette::TEXT_PRIMARY);
    visuals.widgets.active.rounding = Rounding::same(metrics::RADIUS_SELECT);

    // Open combo box / popup state
    visuals.widgets.open.bg_fill = palette::BG_HOVER;
    visuals.widgets.open.weak_bg_fill = palette::BG_HOVER;
    visuals.widgets.open.bg_stroke = Stroke::new(1.0_f32, palette::LINE_STRONG);
    visuals.widgets.open.fg_stroke = Stroke::new(1.0_f32, palette::TEXT_PRIMARY);
    visuals.widgets.open.rounding = Rounding::same(metrics::RADIUS_SELECT);

    // Selection highlight
    visuals.selection.bg_fill = palette::ACCENT_TINT;
    visuals.selection.stroke = Stroke::new(1.0_f32, palette::ACCENT);

    style.visuals = visuals;
    ctx.set_style(style);
}

/// Construct a Quiet Graphite card frame (`#151517`, radius `14px`, `1px` hairline border).
/// Never uses neon borders on active cards; active status is communicated via `.chip.live`.
pub fn studio_card_frame(_is_active: bool, _active_border: Color32) -> egui::Frame {
    egui::Frame::none()
        .fill(palette::BG_PANEL)
        .stroke(Stroke::new(1.0_f32, palette::LINE))
        .rounding(Rounding::same(metrics::RADIUS_CARD))
        .inner_margin(Margin::same(0.0))
}

/// Construct a padded Quiet Graphite card frame when internal full-bleed hairlines are not needed.
pub fn padded_card_frame() -> egui::Frame {
    egui::Frame::none()
        .fill(palette::BG_PANEL)
        .stroke(Stroke::new(1.0_f32, palette::LINE))
        .rounding(Rounding::same(metrics::RADIUS_CARD))
        .inner_margin(Margin::symmetric(
            metrics::CARD_PADDING_X,
            metrics::CARD_PADDING_Y,
        ))
}

/// Construct a subtle raised row or well frame.
pub fn trench_frame() -> egui::Frame {
    egui::Frame::none()
        .fill(palette::BG_RAISED)
        .stroke(Stroke::new(1.0_f32, palette::LINE))
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
