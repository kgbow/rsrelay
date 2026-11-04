//! "Quiet Graphite" (Dark) & "Quiet Daylight" (Light) Design System for `rsrelay-gui`.
//!
//! Supports three theme modes (`ThemeMode::System`, `ThemeMode::Light`, `ThemeMode::Dark`):
//! - **Dark ("Quiet Graphite")**: Neutral graphite surfaces (`#0E0E10` canvas, `#0B0B0D` sidebar, `#151517` cards, `#1C1C1F` raised controls)
//! - **Light ("Quiet Daylight")**: Warm architectural paper surfaces (`#F4F4F6` canvas, `#ECECEF` sidebar, `#FFFFFF` cards, `#F1F1F4` raised controls)
//! - Single signal accent (`#3CE0A0` mint in dark / `#10B981` emerald in light) reserved strictly for live/healthy states
//! - Hairline dividers (`1px`) instead of nested bordered boxes
//! - Monospace typography reserved for numeric telemetry and network addresses

use std::sync::atomic::{AtomicBool, Ordering};

use eframe::egui::{self, Color32, FontFamily, FontId, Margin, Rounding, Stroke, TextStyle, Vec2};

/// User-selectable theme preference (`跟随系统` / `浅色` / `深色`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ThemeMode {
    /// Follow the operating system's dark/light appearance (`跟随系统`).
    #[default]
    System,
    /// Always use the light "Quiet Daylight" appearance (`浅色`).
    Light,
    /// Always use the dark "Quiet Graphite" appearance (`深色`).
    Dark,
}

impl ThemeMode {
    /// Parse a CLI or config string (`"system"`, `"auto"`, `"light"`, `"dark"`) into a [`ThemeMode`].
    pub fn from_str_opt(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "system" | "auto" | "os" => Some(Self::System),
            "light" | "day" | "white" => Some(Self::Light),
            "dark" | "night" | "black" => Some(Self::Dark),
            _ => None,
        }
    }
}

static ACTIVE_THEME_IS_DARK: AtomicBool = AtomicBool::new(true);

/// Returns `true` when the currently active resolved theme is Dark ("Quiet Graphite").
#[inline]
pub fn is_dark_theme() -> bool {
    ACTIVE_THEME_IS_DARK.load(Ordering::Relaxed)
}

/// Explicitly set whether the resolved theme is Dark (`true`) or Light (`false`).
#[inline]
pub fn set_active_dark_theme(is_dark: bool) {
    ACTIVE_THEME_IS_DARK.store(is_dark, Ordering::Relaxed);
}

/// Semantic color tokens that dynamically resolve to **Quiet Graphite** (Dark) or **Quiet Daylight** (Light).
#[allow(non_snake_case)]
pub mod palette {
    use eframe::egui::Color32;

    use super::is_dark_theme;

    #[inline]
    fn pick(dark: Color32, light: Color32) -> Color32 {
        if is_dark_theme() {
            dark
        } else {
            light
        }
    }

    // === Canvas, Sidebar & Surface Hierarchy ===
    /// Main workspace canvas (`#0E0E10` dark / `#F4F4F6` light)
    #[inline]
    pub fn BG_CANVAS() -> Color32 {
        pick(
            Color32::from_rgb(14, 14, 16),
            Color32::from_rgb(244, 244, 246),
        )
    }

    /// Left navigation sidebar (`#0B0B0D` dark / `#ECECEF` light)
    #[inline]
    pub fn BG_SIDEBAR() -> Color32 {
        pick(
            Color32::from_rgb(11, 11, 13),
            Color32::from_rgb(236, 236, 239),
        )
    }

    /// Card panel surface (`#151517` dark / `#FFFFFF` light)
    #[inline]
    pub fn BG_PANEL() -> Color32 {
        pick(
            Color32::from_rgb(21, 21, 23),
            Color32::from_rgb(255, 255, 255),
        )
    }

    /// Raised control fill for selects, icon tiles, secondary buttons, active nav (`#1C1C1F` dark / `#F1F1F4` light)
    #[inline]
    pub fn BG_RAISED() -> Color32 {
        pick(
            Color32::from_rgb(28, 28, 31),
            Color32::from_rgb(241, 241, 244),
        )
    }

    /// Hover surface & active segmented pill fill (`#232327` dark / `#FFFFFF` light)
    #[inline]
    pub fn BG_HOVER() -> Color32 {
        pick(
            Color32::from_rgb(35, 35, 39),
            Color32::from_rgb(255, 255, 255),
        )
    }

    /// Sidebar item subtle hover fill (`#111114` dark / `#E4E4E9` light)
    #[inline]
    pub fn SIDEBAR_HOVER_BG() -> Color32 {
        pick(
            Color32::from_rgb(17, 17, 20),
            Color32::from_rgb(228, 228, 233),
        )
    }

    // Backwards-compatible surface aliases
    #[inline]
    pub fn BG_SURFACE() -> Color32 {
        BG_PANEL()
    }
    #[inline]
    pub fn BG_ELEVATED() -> Color32 {
        BG_RAISED()
    }
    #[inline]
    pub fn BG_ACTIVE() -> Color32 {
        BG_HOVER()
    }
    #[inline]
    pub fn BG_TRENCH() -> Color32 {
        BG_CANVAS()
    }

    // === Hairlines & Borders ===
    /// Primary 1px hairline divider (`#222225` dark / `#E5E5EA` light)
    #[inline]
    pub fn LINE() -> Color32 {
        pick(
            Color32::from_rgb(34, 34, 37),
            Color32::from_rgb(229, 229, 234),
        )
    }

    /// Stronger hairline for ghost buttons, popups, and dashed guides (`#2C2C30` dark / `#D1D1D8` light)
    #[inline]
    pub fn LINE_STRONG() -> Color32 {
        pick(
            Color32::from_rgb(44, 44, 48),
            Color32::from_rgb(209, 209, 216),
        )
    }

    /// Subtle inset border on raised controls (`#26262A` dark / `#DCDCE2` light)
    #[inline]
    pub fn INSET_HIGHLIGHT() -> Color32 {
        pick(
            Color32::from_rgb(38, 38, 42),
            Color32::from_rgb(220, 220, 226),
        )
    }

    #[inline]
    pub fn BORDER_SUBTLE() -> Color32 {
        LINE()
    }
    #[inline]
    pub fn BORDER_WIDGET() -> Color32 {
        LINE_STRONG()
    }
    #[inline]
    pub fn BORDER_ACCENT() -> Color32 {
        ACCENT()
    }

    // === Signal Accent & Status Colors ===
    /// Live / healthy signal mint (`#3CE0A0` dark / `#0D9F6E` light)
    #[inline]
    pub fn ACCENT() -> Color32 {
        pick(
            Color32::from_rgb(60, 224, 160),
            Color32::from_rgb(13, 159, 110),
        )
    }

    /// Darker mint for pressed states (`#2BB880` dark / `#057A55` light)
    #[inline]
    pub fn ACCENT_DIM() -> Color32 {
        pick(
            Color32::from_rgb(43, 184, 128),
            Color32::from_rgb(5, 122, 85),
        )
    }

    /// Mint tint for `.chip.live` (`#1A3229` dark / `#DEF7EC` light)
    #[inline]
    pub fn ACCENT_TINT() -> Color32 {
        pick(
            Color32::from_rgb(26, 50, 41),
            Color32::from_rgb(222, 247, 236),
        )
    }

    /// Idle chip & meter track fill (`#202024` dark / `#EAEAEF` light)
    #[inline]
    pub fn CHIP_IDLE_BG() -> Color32 {
        pick(
            Color32::from_rgb(32, 32, 36),
            Color32::from_rgb(234, 234, 239),
        )
    }

    /// Active peer highlight in sidebar (`#151518` dark / `#E0E0E6` light)
    #[inline]
    pub fn PEER_ACTIVE_BG() -> Color32 {
        pick(
            Color32::from_rgb(21, 21, 24),
            Color32::from_rgb(224, 224, 230),
        )
    }

    /// Slider track fill (`#27272B` dark / `#DCDCE2` light)
    #[inline]
    pub fn SLIDER_TRACK_BG() -> Color32 {
        pick(
            Color32::from_rgb(39, 39, 43),
            Color32::from_rgb(220, 220, 226),
        )
    }

    // Aliases mapped to the unified accent
    #[inline]
    pub fn ACCENT_MINT() -> Color32 {
        ACCENT()
    }
    #[inline]
    pub fn ACCENT_MINT_DIM() -> Color32 {
        ACCENT_DIM()
    }
    #[inline]
    pub fn ACCENT_MINT_GLOW() -> Color32 {
        ACCENT_TINT()
    }
    #[inline]
    pub fn ACCENT_CYAN() -> Color32 {
        ACCENT()
    }
    #[inline]
    pub fn ACCENT_CYAN_GLOW() -> Color32 {
        ACCENT_TINT()
    }
    #[inline]
    pub fn ACCENT_INDIGO() -> Color32 {
        ACCENT()
    }

    /// Nominal status
    #[inline]
    pub fn STATUS_OK() -> Color32 {
        ACCENT()
    }

    /// Meter headroom warning zone (`#F2B84B` dark / `#D97706` light)
    #[inline]
    pub fn STATUS_WARN() -> Color32 {
        pick(
            Color32::from_rgb(242, 184, 75),
            Color32::from_rgb(217, 119, 6),
        )
    }

    /// Meter clipping zone & Stop button square indicator (`#FF6B5B` dark / `#E11D48` light)
    #[inline]
    pub fn STATUS_DANGER() -> Color32 {
        pick(
            Color32::from_rgb(255, 107, 91),
            Color32::from_rgb(225, 29, 72),
        )
    }

    #[inline]
    pub fn STATUS_DANGER_DIM() -> Color32 {
        pick(
            Color32::from_rgb(190, 68, 56),
            Color32::from_rgb(190, 18, 60),
        )
    }

    /// Inactive peer dot (`#4A4A50` dark / `#A1A1AA` light)
    #[inline]
    pub fn DOT_OFFLINE() -> Color32 {
        pick(
            Color32::from_rgb(74, 74, 80),
            Color32::from_rgb(161, 161, 170),
        )
    }

    /// Toggle off track (`#2C2C31` dark / `#D4D4D8` light)
    #[inline]
    pub fn TOGGLE_OFF_BG() -> Color32 {
        pick(
            Color32::from_rgb(44, 44, 49),
            Color32::from_rgb(212, 212, 216),
        )
    }

    /// Toggle off knob (`#8A8A92` dark / `#FFFFFF` light)
    #[inline]
    pub fn TOGGLE_OFF_KNOB() -> Color32 {
        pick(
            Color32::from_rgb(138, 138, 146),
            Color32::from_rgb(255, 255, 255),
        )
    }

    // === Typography & Primary CTA Hierarchy ===
    /// Primary text (`#EDEDEE` dark / `#18181B` light)
    #[inline]
    pub fn TEXT_PRIMARY() -> Color32 {
        pick(
            Color32::from_rgb(237, 237, 238),
            Color32::from_rgb(24, 24, 27),
        )
    }

    /// Secondary labels & descriptions (`#9C9CA3` dark / `#52525B` light)
    #[inline]
    pub fn TEXT_SECONDARY() -> Color32 {
        pick(
            Color32::from_rgb(156, 156, 163),
            Color32::from_rgb(82, 82, 91),
        )
    }

    /// Muted captions, units, and scale ticks (`#66666D` dark / `#71717A` light)
    #[inline]
    pub fn TEXT_MUTED() -> Color32 {
        pick(
            Color32::from_rgb(102, 102, 109),
            Color32::from_rgb(113, 113, 122),
        )
    }

    /// Primary action button fill (`#EDEDEE` dark / `#18181B` light)
    #[inline]
    pub fn BTN_PRIMARY_BG() -> Color32 {
        pick(
            Color32::from_rgb(237, 237, 238),
            Color32::from_rgb(24, 24, 27),
        )
    }

    /// Primary action button hover fill (`#FFFFFF` dark / `#27272A` light)
    #[inline]
    pub fn BTN_PRIMARY_HOVER_BG() -> Color32 {
        pick(Color32::WHITE, Color32::from_rgb(39, 39, 42))
    }

    /// Primary action button foreground text & icon (`#0E0E10` dark / `#FFFFFF` light)
    #[inline]
    pub fn BTN_PRIMARY_FG() -> Color32 {
        pick(Color32::from_rgb(14, 14, 16), Color32::WHITE)
    }

    /// Peak-hold tick mark in VU meters (`#FFFFFF` dark / `#18181B` light)
    #[inline]
    pub fn PEAK_HOLD_TICK() -> Color32 {
        pick(Color32::WHITE, Color32::from_rgb(24, 24, 27))
    }
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

fn build_style_for_polarity(base: &egui::Style, is_dark: bool) -> egui::Style {
    let prev = is_dark_theme();
    set_active_dark_theme(is_dark);

    let mut style = base.clone();

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
    style.spacing.menu_margin = Margin::same(6.0);
    style.spacing.button_padding = Vec2::new(12.0, 6.0);
    style.spacing.interact_size = Vec2::new(40.0, 30.0);
    style.spacing.slider_width = 150.0;
    style.spacing.combo_width = 190.0;

    let mut visuals = if is_dark {
        egui::Visuals::dark()
    } else {
        egui::Visuals::light()
    };
    visuals.dark_mode = is_dark;
    visuals.override_text_color = Some(palette::TEXT_PRIMARY());
    visuals.panel_fill = palette::BG_CANVAS();
    // Popup menus (`ComboBox`, `popup_below_widget`, tooltips) use `window_fill`
    visuals.window_fill = if is_dark {
        palette::BG_RAISED()
    } else {
        palette::BG_PANEL()
    };
    visuals.faint_bg_color = palette::BG_CANVAS();
    visuals.extreme_bg_color = palette::BG_RAISED();
    visuals.code_bg_color = palette::BG_RAISED();

    visuals.window_rounding = Rounding::same(metrics::RADIUS_CARD);
    visuals.menu_rounding = Rounding::same(metrics::RADIUS_BUTTON);
    visuals.window_stroke = Stroke::new(1.0_f32, palette::LINE_STRONG());

    // Non-interactive surfaces (frames, labels, separators)
    visuals.widgets.noninteractive.bg_fill = palette::BG_PANEL();
    visuals.widgets.noninteractive.weak_bg_fill = palette::BG_PANEL();
    visuals.widgets.noninteractive.bg_stroke = Stroke::new(1.0_f32, palette::LINE());
    visuals.widgets.noninteractive.fg_stroke = Stroke::new(1.0_f32, palette::TEXT_SECONDARY());
    visuals.widgets.noninteractive.rounding = Rounding::same(metrics::RADIUS_SELECT);

    // Inactive interactive widgets (selects, secondary buttons, text inputs)
    visuals.widgets.inactive.bg_fill = palette::BG_RAISED();
    visuals.widgets.inactive.weak_bg_fill = palette::BG_RAISED();
    visuals.widgets.inactive.bg_stroke = Stroke::new(1.0_f32, palette::LINE());
    visuals.widgets.inactive.fg_stroke = Stroke::new(1.0_f32, palette::TEXT_PRIMARY());
    visuals.widgets.inactive.rounding = Rounding::same(metrics::RADIUS_SELECT);

    // Hovered widgets
    visuals.widgets.hovered.bg_fill = palette::BG_HOVER();
    visuals.widgets.hovered.weak_bg_fill = palette::BG_HOVER();
    visuals.widgets.hovered.bg_stroke = Stroke::new(1.0_f32, palette::LINE_STRONG());
    visuals.widgets.hovered.fg_stroke = Stroke::new(1.2_f32, palette::TEXT_PRIMARY());
    visuals.widgets.hovered.rounding = Rounding::same(metrics::RADIUS_SELECT);

    // Active / pressed widgets
    visuals.widgets.active.bg_fill = palette::BG_HOVER();
    visuals.widgets.active.weak_bg_fill = palette::BG_HOVER();
    visuals.widgets.active.bg_stroke = Stroke::new(1.0_f32, palette::LINE_STRONG());
    visuals.widgets.active.fg_stroke = Stroke::new(1.2_f32, palette::TEXT_PRIMARY());
    visuals.widgets.active.rounding = Rounding::same(metrics::RADIUS_SELECT);

    // Open combo box / popup state
    visuals.widgets.open.bg_fill = palette::BG_HOVER();
    visuals.widgets.open.weak_bg_fill = palette::BG_HOVER();
    visuals.widgets.open.bg_stroke = Stroke::new(1.0_f32, palette::LINE_STRONG());
    visuals.widgets.open.fg_stroke = Stroke::new(1.0_f32, palette::TEXT_PRIMARY());
    visuals.widgets.open.rounding = Rounding::same(metrics::RADIUS_SELECT);

    // Selection highlight inside dropdown menus & text inputs
    visuals.selection.bg_fill = palette::ACCENT_TINT();
    visuals.selection.stroke = Stroke::new(1.0_f32, palette::ACCENT());

    style.visuals = visuals;
    set_active_dark_theme(prev);
    style
}

/// Initialize both Dark ("Quiet Graphite") and Light ("Quiet Daylight") styles on an `egui::Context`.
pub fn apply_studio_theme(ctx: &egui::Context) {
    configure_cjk_fallback_fonts(ctx);
    let base = (*ctx.style()).clone();
    let dark_style = build_style_for_polarity(&base, true);
    let light_style = build_style_for_polarity(&base, false);
    ctx.set_style_of(egui::Theme::Dark, dark_style);
    ctx.set_style_of(egui::Theme::Light, light_style);
    sync_theme_mode(ctx, ThemeMode::System);
}

/// Synchronize the active [`ThemeMode`] (`System`, `Light`, `Dark`) with `egui::Context` and `palette`.
/// Returns the resolved `is_dark` boolean for the current frame.
pub fn sync_theme_mode(ctx: &egui::Context, mode: ThemeMode) -> bool {
    let pref = match mode {
        ThemeMode::System => egui::ThemePreference::System,
        ThemeMode::Light => egui::ThemePreference::Light,
        ThemeMode::Dark => egui::ThemePreference::Dark,
    };
    ctx.set_theme(pref);

    let is_dark = match mode {
        ThemeMode::Dark => true,
        ThemeMode::Light => false,
        ThemeMode::System => ctx
            .system_theme()
            .map(|t| t == egui::Theme::Dark)
            .unwrap_or(true),
    };
    set_active_dark_theme(is_dark);
    is_dark
}

/// Construct a Quiet Graphite / Quiet Daylight card frame (`radius 14px`, `1px` hairline border).
/// Never uses neon borders on active cards; active status is communicated via `.chip.live`.
pub fn studio_card_frame(_is_active: bool, _active_border: Color32) -> egui::Frame {
    egui::Frame::none()
        .fill(palette::BG_PANEL())
        .stroke(Stroke::new(1.0_f32, palette::LINE()))
        .rounding(Rounding::same(metrics::RADIUS_CARD))
        .inner_margin(Margin::same(0.0))
}

/// Construct a padded card frame when internal full-bleed hairlines are not needed.
pub fn padded_card_frame() -> egui::Frame {
    egui::Frame::none()
        .fill(palette::BG_PANEL())
        .stroke(Stroke::new(1.0_f32, palette::LINE()))
        .rounding(Rounding::same(metrics::RADIUS_CARD))
        .inner_margin(Margin::symmetric(
            metrics::CARD_PADDING_X,
            metrics::CARD_PADDING_Y,
        ))
}

/// Construct a subtle raised row or well frame.
pub fn trench_frame() -> egui::Frame {
    egui::Frame::none()
        .fill(palette::BG_RAISED())
        .stroke(Stroke::new(1.0_f32, palette::LINE()))
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

        // 1. Simulate Windows Light Mode (`system_theme = Some(Theme::Light)`) with `ThemeMode::System`
        let mut raw_light = egui::RawInput::default();
        raw_light.system_theme = Some(egui::Theme::Light);
        let _ = ctx.run(raw_light.clone(), |ctx| {
            let is_dark = sync_theme_mode(ctx, ThemeMode::System);
            assert!(!is_dark);
            assert!(!ctx.style().visuals.dark_mode);
            assert_eq!(ctx.style().visuals.window_fill, palette::BG_PANEL());
            assert_ne!(palette::BG_PANEL(), palette::TEXT_PRIMARY());
            egui::CentralPanel::default().show(ctx, |ui| {
                ui.label("外置麦克风 (48000 Hz) -> 外置耳机");
            });
        });

        // 2. Force `ThemeMode::Dark` even when OS reports `Theme::Light`
        let _ = ctx.run(raw_light, |ctx| {
            let is_dark = sync_theme_mode(ctx, ThemeMode::Dark);
            assert!(is_dark);
            assert!(ctx.style().visuals.dark_mode);
            assert_eq!(ctx.style().visuals.window_fill, palette::BG_RAISED());
            assert_ne!(palette::BG_RAISED(), palette::TEXT_PRIMARY());
        });

        // 3. Force `ThemeMode::Light` even when OS reports `Theme::Dark`
        let mut raw_dark = egui::RawInput::default();
        raw_dark.system_theme = Some(egui::Theme::Dark);
        let _ = ctx.run(raw_dark, |ctx| {
            let is_dark = sync_theme_mode(ctx, ThemeMode::Light);
            assert!(!is_dark);
            assert!(!ctx.style().visuals.dark_mode);
            assert_eq!(ctx.style().visuals.window_fill, palette::BG_PANEL());
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
