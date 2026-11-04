//! Custom Quiet Graphite widgets and vector iconography for `rsrelay-gui` (Frames C & C2).
//!
//! Includes:
//! - Crisp `egui::Painter` vector icons (brand waveform, nav icons, OS badges, transport icons)
//! - Segmented Stereo (`L`/`R`) and Mono Noise-Gate (`M`) VU Meters with `-60..0 dB` linear scale
//! - Symmetric bar waveform chart (TX) and area sparkline chart (RX / Duplex) with dashed guides
//! - 3-column hairline stats strip, segmented pill controls, pill toggles, and Quiet Graphite sliders

use std::collections::VecDeque;
use std::time::Instant;

use eframe::egui::{
    self, Align, Color32, FontFamily, FontId, Pos2, Rect, RichText, Rounding, Sense, Stroke, Ui,
    Vec2,
};
use rsrelay_core::LatencyProfile;

use crate::i18n::Language;
use crate::theme::{metrics, palette};

/// Format an integer with thousands separators (e.g. `863848` -> `"863,848"`).
pub fn format_with_commas(mut n: u64) -> String {
    if n < 1000 {
        return n.to_string();
    }
    let mut parts = Vec::new();
    while n >= 1000 {
        parts.push(format!("{:03}", n % 1000));
        n /= 1000;
    }
    parts.push(n.to_string());
    parts.reverse();
    parts.join(",")
}

/// Format linear gain (`0.0..=2.0`, where `1.0` = `0.0 dB`) as a dB string matching C & C2.
pub fn format_gain_db(gain: f32) -> String {
    if gain <= 0.01 {
        "-∞ dB".to_string()
    } else {
        let db = 20.0 * gain.log10();
        if db.abs() < 0.05 {
            "0.0 dB".to_string()
        } else if db > 0.0 {
            format!("+{db:.1} dB")
        } else {
            format!("{db:.1} dB")
        }
    }
}

/// Convert linear amplitude (`0.0..=1.0`) to logarithmic dBFS (`-60.0..=0.0`).
pub fn linear_to_dbfs(linear: f32) -> f32 {
    if linear <= 0.001 {
        -60.0
    } else {
        (20.0 * linear.log10()).clamp(-60.0, 0.0)
    }
}

/// Normalize a dBFS value in `[-60.0, 0.0]` to `[0.0, 1.0]`.
pub fn dbfs_to_norm(db: f32) -> f32 {
    ((db.clamp(-60.0, 0.0) + 60.0) / 60.0).clamp(0.0, 1.0)
}

// =============================================================================
// Vector Iconography (Painted directly via egui::Painter, zero external assets)
// =============================================================================

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IconKind {
    BrandWaveform,
    SendReceive,
    Headset,
    SpeakerDevice,
    ArrowUpRight,
    ArrowDownLeft,
    Microphone,
    Laptop,
    Desktop,
    Mobile,
    Refresh,
    Copy,
    MinimizeTray,
}

/// Paint a crisp 1.8px-stroke vector icon inside `rect`.
pub fn draw_vector_icon(painter: &egui::Painter, rect: Rect, kind: IconKind, color: Color32) {
    let c = rect.center();
    let s = (rect.width().min(rect.height()) * 0.5).max(4.0);
    let stroke = Stroke::new(1.65_f32, color);

    match kind {
        IconKind::BrandWaveform => {
            // 5 vertical equalizer bars matching the C/C2 brand mark
            let heights = [0.22, 0.54, 0.78, 0.42, 0.18];
            let xs = [-0.62, -0.31, 0.0, 0.31, 0.62];
            for (&nx, &nh) in xs.iter().zip(heights.iter()) {
                let x = c.x + nx * s;
                let hh = nh * s;
                painter.line_segment(
                    [Pos2::new(x, c.y - hh), Pos2::new(x, c.y + hh)],
                    Stroke::new(1.85_f32, color),
                );
            }
        }
        IconKind::SendReceive => {
            // Top arrow pointing left, bottom arrow pointing right
            let y_top = c.y - s * 0.36;
            let y_bot = c.y + s * 0.36;
            let x_left = c.x - s * 0.68;
            let x_right = c.x + s * 0.68;
            painter.line_segment([Pos2::new(x_left, y_top), Pos2::new(x_right, y_top)], stroke);
            painter.line_segment(
                [
                    Pos2::new(x_left + s * 0.32, y_top - s * 0.28),
                    Pos2::new(x_left, y_top),
                ],
                stroke,
            );
            painter.line_segment(
                [
                    Pos2::new(x_left + s * 0.32, y_top + s * 0.28),
                    Pos2::new(x_left, y_top),
                ],
                stroke,
            );

            painter.line_segment([Pos2::new(x_left, y_bot), Pos2::new(x_right, y_bot)], stroke);
            painter.line_segment(
                [
                    Pos2::new(x_right - s * 0.32, y_bot - s * 0.28),
                    Pos2::new(x_right, y_bot),
                ],
                stroke,
            );
            painter.line_segment(
                [
                    Pos2::new(x_right - s * 0.32, y_bot + s * 0.28),
                    Pos2::new(x_right, y_bot),
                ],
                stroke,
            );
        }
        IconKind::Headset => {
            // Headband arch + left/right earcups
            let r = s * 0.68;
            let mut arch_pts = Vec::with_capacity(13);
            for i in 0..=12 {
                let angle = std::f32::consts::PI + (i as f32 / 12.0) * std::f32::consts::PI;
                arch_pts.push(Pos2::new(
                    c.x + r * angle.cos(),
                    c.y + s * 0.05 + r * angle.sin(),
                ));
            }
            painter.add(egui::Shape::line(arch_pts, stroke));
            let cup_w = s * 0.34;
            let cup_h = s * 0.56;
            let left_cup = Rect::from_min_size(
                Pos2::new(c.x - r - cup_w * 0.3, c.y + s * 0.02),
                Vec2::new(cup_w, cup_h),
            );
            let right_cup = Rect::from_min_size(
                Pos2::new(c.x + r - cup_w * 0.7, c.y + s * 0.02),
                Vec2::new(cup_w, cup_h),
            );
            painter.rect_stroke(left_cup, Rounding::same(2.0), stroke);
            painter.rect_stroke(right_cup, Rounding::same(2.0), stroke);
        }
        IconKind::SpeakerDevice => {
            let box_rect = Rect::from_center_size(c, Vec2::new(s * 1.15, s * 1.55));
            painter.rect_stroke(box_rect, Rounding::same(2.0), stroke);
            painter.circle_stroke(Pos2::new(c.x, c.y + s * 0.22), s * 0.30, stroke);
            painter.circle_filled(Pos2::new(c.x, c.y - s * 0.38), 1.4, color);
        }
        IconKind::ArrowUpRight => {
            let p0 = Pos2::new(c.x - s * 0.45, c.y + s * 0.45);
            let p1 = Pos2::new(c.x + s * 0.45, c.y - s * 0.45);
            painter.line_segment([p0, p1], stroke);
            painter.line_segment([Pos2::new(c.x - s * 0.15, p1.y), p1], stroke);
            painter.line_segment([Pos2::new(p1.x, c.y + s * 0.15), p1], stroke);
        }
        IconKind::ArrowDownLeft => {
            let p0 = Pos2::new(c.x + s * 0.45, c.y - s * 0.45);
            let p1 = Pos2::new(c.x - s * 0.45, c.y + s * 0.45);
            painter.line_segment([p0, p1], stroke);
            painter.line_segment([Pos2::new(c.x + s * 0.15, p1.y), p1], stroke);
            painter.line_segment([Pos2::new(p1.x, c.y - s * 0.15), p1], stroke);
        }
        IconKind::Microphone => {
            let cap = Rect::from_center_size(
                Pos2::new(c.x, c.y - s * 0.22),
                Vec2::new(s * 0.54, s * 0.92),
            );
            painter.rect_stroke(cap, Rounding::same(s * 0.27), stroke);
            let u_r = s * 0.54;
            let mut u_pts = Vec::with_capacity(11);
            for i in 0..=10 {
                let angle = (i as f32 / 10.0) * std::f32::consts::PI;
                u_pts.push(Pos2::new(
                    c.x + u_r * angle.cos(),
                    c.y - s * 0.08 + u_r * angle.sin(),
                ));
            }
            painter.add(egui::Shape::line(u_pts, stroke));
            painter.line_segment(
                [
                    Pos2::new(c.x, c.y + s * 0.46),
                    Pos2::new(c.x, c.y + s * 0.76),
                ],
                stroke,
            );
        }
        IconKind::Laptop => {
            let scr = Rect::from_center_size(
                Pos2::new(c.x, c.y - s * 0.12),
                Vec2::new(s * 1.35, s * 0.92),
            );
            painter.rect_stroke(scr, Rounding::same(2.0), stroke);
            painter.line_segment(
                [
                    Pos2::new(c.x - s * 0.82, c.y + s * 0.55),
                    Pos2::new(c.x + s * 0.82, c.y + s * 0.55),
                ],
                stroke,
            );
        }
        IconKind::Desktop => {
            let scr = Rect::from_center_size(
                Pos2::new(c.x, c.y - s * 0.16),
                Vec2::new(s * 1.40, s * 0.96),
            );
            painter.rect_stroke(scr, Rounding::same(2.0), stroke);
            painter.line_segment(
                [
                    Pos2::new(c.x, c.y + s * 0.32),
                    Pos2::new(c.x, c.y + s * 0.62),
                ],
                stroke,
            );
            painter.line_segment(
                [
                    Pos2::new(c.x - s * 0.38, c.y + s * 0.62),
                    Pos2::new(c.x + s * 0.38, c.y + s * 0.62),
                ],
                stroke,
            );
        }
        IconKind::Mobile => {
            let pad = Rect::from_center_size(c, Vec2::new(s * 1.05, s * 1.50));
            painter.rect_stroke(pad, Rounding::same(2.2), stroke);
            painter.circle_filled(Pos2::new(c.x, c.y + s * 0.48), 1.3, color);
        }
        IconKind::Refresh => {
            let r = s * 0.62;
            let mut pts = Vec::with_capacity(14);
            for i in 0..=13 {
                let angle = -0.35 + (i as f32 / 13.0) * (std::f32::consts::TAU * 0.78);
                pts.push(Pos2::new(c.x + r * angle.cos(), c.y + r * angle.sin()));
            }
            painter.add(egui::Shape::line(pts, stroke));
            let tip = Pos2::new(c.x + r * (-0.35_f32).cos(), c.y + r * (-0.35_f32).sin());
            painter.line_segment([tip, Pos2::new(tip.x - s * 0.34, tip.y - s * 0.06)], stroke);
            painter.line_segment([tip, Pos2::new(tip.x + s * 0.04, tip.y - s * 0.36)], stroke);
        }
        IconKind::Copy => {
            let front = Rect::from_min_size(
                Pos2::new(c.x - s * 0.22, c.y - s * 0.22),
                Vec2::new(s * 0.90, s * 0.90),
            );
            painter.rect_stroke(front, Rounding::same(2.0), stroke);
            let back_pts = [
                Pos2::new(c.x - s * 0.22, c.y + s * 0.32),
                Pos2::new(c.x - s * 0.58, c.y + s * 0.32),
                Pos2::new(c.x - s * 0.58, c.y - s * 0.58),
                Pos2::new(c.x + s * 0.32, c.y - s * 0.58),
                Pos2::new(c.x + s * 0.32, c.y - s * 0.22),
            ];
            painter.add(egui::Shape::line(back_pts.to_vec(), stroke));
        }
        IconKind::MinimizeTray => {
            // Two diagonal inward arrows matching the sidebar tray button in C/C2
            let d0 = s * 0.14;
            let d1 = s * 0.64;
            // Top-right arrow pointing down-left
            painter.line_segment(
                [Pos2::new(c.x + d1, c.y - d1), Pos2::new(c.x + d0, c.y - d0)],
                stroke,
            );
            painter.line_segment(
                [
                    Pos2::new(c.x + d0, c.y - d0),
                    Pos2::new(c.x + d0 + s * 0.36, c.y - d0),
                ],
                stroke,
            );
            painter.line_segment(
                [
                    Pos2::new(c.x + d0, c.y - d0),
                    Pos2::new(c.x + d0, c.y - d0 - s * 0.36),
                ],
                stroke,
            );
            // Bottom-left arrow pointing up-right
            painter.line_segment(
                [Pos2::new(c.x - d1, c.y + d1), Pos2::new(c.x - d0, c.y + d0)],
                stroke,
            );
            painter.line_segment(
                [
                    Pos2::new(c.x - d0, c.y + d0),
                    Pos2::new(c.x - d0 - s * 0.36, c.y + d0),
                ],
                stroke,
            );
            painter.line_segment(
                [
                    Pos2::new(c.x - d0, c.y + d0),
                    Pos2::new(c.x - d0, c.y + d0 + s * 0.36),
                ],
                stroke,
            );
        }
    }
}

/// Draw a `32x32` raised rounded icon tile (`.ch .ic` in C & C2).
pub fn draw_card_icon_tile(ui: &mut Ui, kind: IconKind) {
    let (rect, _) = ui.allocate_exact_size(Vec2::splat(32.0), Sense::hover());
    if ui.is_rect_visible(rect) {
        let p = ui.painter();
        p.rect(
            rect,
            Rounding::same(9.0),
            palette::BG_RAISED(),
            Stroke::new(1.0_f32, palette::INSET_HIGHLIGHT()),
        );
        draw_vector_icon(p, rect.shrink(8.0), kind, palette::TEXT_PRIMARY());
    }
}

// =============================================================================
// Status Chips & Action Buttons
// =============================================================================

/// Render a Quiet Graphite status chip (`.chip.live` or `.chip.idle`).
/// Allocates a single atomic `Rect` so the indicator dot always stays on the left in both LTR and RTL parents.
pub fn status_chip(ui: &mut Ui, label: &str, mono_suffix: Option<&str>, active: bool) {
    let bg = if active {
        palette::ACCENT_TINT()
    } else {
        palette::CHIP_IDLE_BG()
    };
    let fg = if active {
        palette::ACCENT()
    } else {
        palette::TEXT_SECONDARY()
    };

    let prop_font = FontId::new(11.0, FontFamily::Proportional);
    let mono_font = FontId::new(11.0, FontFamily::Monospace);
    let p = ui.painter();
    let label_galley = p.layout_no_wrap(label.to_string(), prop_font.clone(), fg);
    let mut total_w = 8.5 + 5.5 + 6.0 + label_galley.size().x + 8.5;

    let suffix_galleys = mono_suffix.map(|mono| {
        let dot_g = p.layout_no_wrap("·".to_string(), prop_font.clone(), fg);
        let mono_g = p.layout_no_wrap(mono.to_string(), mono_font.clone(), fg);
        total_w += 5.0 + dot_g.size().x + 5.0 + mono_g.size().x;
        (dot_g, mono_g)
    });

    let (rect, _) = ui.allocate_exact_size(Vec2::new(total_w, 20.0), Sense::hover());
    if ui.is_rect_visible(rect) {
        let p = ui.painter();
        p.rect_filled(rect, Rounding::same(metrics::RADIUS_CHIP), bg);
        let cy = rect.center().y;
        let mut x = rect.min.x + 8.5;
        p.circle_filled(Pos2::new(x + 2.5, cy), 2.5, fg);
        x += 5.5 + 6.0;
        let lbl_w = label_galley.size().x;
        p.galley(
            Pos2::new(x, cy - label_galley.size().y * 0.5),
            label_galley,
            fg,
        );
        x += lbl_w;

        if let Some((dot_g, mono_g)) = suffix_galleys {
            x += 5.0;
            let dw = dot_g.size().x;
            p.galley(Pos2::new(x, cy - dot_g.size().y * 0.5), dot_g, fg);
            x += dw + 5.0;
            p.galley(Pos2::new(x, cy - mono_g.size().y * 0.5), mono_g, fg);
        }
    }
}

/// Backwards-compatible status pill wrapper.
pub fn status_pill(ui: &mut Ui, text: &str, active: bool, _accent: Color32) {
    status_chip(ui, text, None, active);
}

/// Render a Quiet Graphite card header Start/Stop button:
/// - Start (`.btn.pri`): near-white `#EDEDEE` fill with `#0E0E10` dark text and play icon
/// - Stop (`.btn.sec`): raised `#1C1C1F` fill with a coral `#FF6B5B` square and `#EDEDEE` text
pub fn header_action_button(
    ui: &mut Ui,
    start_label: &str,
    stop_label: &str,
    is_running: bool,
) -> bool {
    let text = if is_running { stop_label } else { start_label };
    let font_id = FontId::new(12.5, FontFamily::Proportional);
    let text_galley = ui
        .painter()
        .layout_no_wrap(text.to_string(), font_id.clone(), Color32::WHITE);
    let btn_w = (text_galley.size().x + 42.0).max(74.0);
    let btn_h = 31.0;

    let (rect, resp) = ui.allocate_exact_size(Vec2::new(btn_w, btn_h), Sense::click());
    if resp.hovered() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
    }

    if ui.is_rect_visible(rect) {
        let p = ui.painter();
        if is_running {
            let fill = if resp.hovered() {
                palette::BG_HOVER()
            } else {
                palette::BG_RAISED()
            };
            p.rect(
                rect,
                Rounding::same(metrics::RADIUS_BUTTON),
                fill,
                Stroke::new(1.0_f32, palette::LINE_STRONG()),
            );
            let sq_rect = Rect::from_center_size(
                Pos2::new(rect.min.x + 17.0, rect.center().y),
                Vec2::splat(9.0),
            );
            p.rect_filled(sq_rect, Rounding::same(2.0), palette::STATUS_DANGER());
            p.text(
                Pos2::new(rect.min.x + 29.0, rect.center().y),
                egui::Align2::LEFT_CENTER,
                text,
                font_id,
                palette::TEXT_PRIMARY(),
            );
        } else {
            let fill = if resp.hovered() {
                palette::BTN_PRIMARY_HOVER_BG()
            } else {
                palette::BTN_PRIMARY_BG()
            };
            let fg = palette::BTN_PRIMARY_FG();
            p.rect_filled(rect, Rounding::same(metrics::RADIUS_BUTTON), fill);
            // Play triangle
            let cx = rect.min.x + 17.5;
            let cy = rect.center().y;
            let tri = vec![
                Pos2::new(cx - 3.2, cy - 4.2),
                Pos2::new(cx + 4.4, cy),
                Pos2::new(cx - 3.2, cy + 4.2),
            ];
            p.add(egui::Shape::convex_polygon(tri, fg, Stroke::NONE));
            p.text(
                Pos2::new(rect.min.x + 28.0, rect.center().y),
                egui::Align2::LEFT_CENTER,
                text,
                font_id,
                fg,
            );
        }
    }

    resp.clicked()
}

/// Render a ghost button with `1px` `LINE_STRONG` border (e.g., `"全部停止"` in Frame C).
pub fn ghost_stop_all_button(ui: &mut Ui, label: &str) -> bool {
    let font_id = FontId::new(12.5, FontFamily::Proportional);
    let galley = ui
        .painter()
        .layout_no_wrap(label.to_string(), font_id.clone(), palette::TEXT_SECONDARY());
    let btn_w = (galley.size().x + 40.0).max(86.0);
    let (rect, resp) = ui.allocate_exact_size(Vec2::new(btn_w, 31.0), Sense::click());
    if resp.hovered() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
    }
    if ui.is_rect_visible(rect) {
        let p = ui.painter();
        let fill = if resp.hovered() {
            palette::BG_RAISED()
        } else {
            Color32::TRANSPARENT
        };
        let fg = if resp.hovered() {
            palette::TEXT_PRIMARY()
        } else {
            palette::TEXT_SECONDARY()
        };
        p.rect(
            rect,
            Rounding::same(metrics::RADIUS_BUTTON),
            fill,
            Stroke::new(1.0_f32, palette::LINE_STRONG()),
        );
        let sq = Rect::from_center_size(
            Pos2::new(rect.min.x + 16.5, rect.center().y),
            Vec2::splat(7.5),
        );
        p.rect_filled(sq, Rounding::same(1.5), fg);
        p.text(
            Pos2::new(rect.min.x + 27.5, rect.center().y),
            egui::Align2::LEFT_CENTER,
            label,
            font_id,
            fg,
        );
    }
    resp.clicked()
}

/// Backwards-compatible transport CTA button.
pub fn transport_cta_button(
    ui: &mut Ui,
    start_label: &str,
    stop_label: &str,
    is_running: bool,
    _accent_color: Color32,
) -> bool {
    header_action_button(ui, start_label, stop_label, is_running)
}

// =============================================================================
// Stereo & Mono Segmented VU Meters with -60..0 dB Scale
// =============================================================================

/// Stateful ballistics tracker for a Studio VU Meter (attack/release smoothing + peak hold).
#[derive(Debug, Clone)]
pub struct VuMeterState {
    pub smoothed_db: f32,
    pub peak_hold_db: f32,
    pub smoothed_db_r: f32,
    pub peak_hold_db_r: f32,
    last_peak_time: Instant,
    last_update: Instant,
    phase: f32,
}

impl Default for VuMeterState {
    fn default() -> Self {
        let now = Instant::now();
        Self {
            smoothed_db: -60.0,
            peak_hold_db: -60.0,
            smoothed_db_r: -60.0,
            peak_hold_db_r: -60.0,
            last_peak_time: now,
            last_update: now,
            phase: 0.0,
        }
    }
}

impl VuMeterState {
    /// Update meter ballistics with a new linear peak sample (`0.0..=1.0`).
    pub fn update(&mut self, linear_peak: f32, active: bool) {
        let now = Instant::now();
        let dt = now
            .duration_since(self.last_update)
            .as_secs_f32()
            .clamp(0.001, 0.25);
        self.last_update = now;
        self.phase = (self.phase + dt * 4.7) % std::f32::consts::TAU;

        if !active {
            self.smoothed_db = -60.0;
            self.peak_hold_db = -60.0;
            self.smoothed_db_r = -60.0;
            self.peak_hold_db_r = -60.0;
            return;
        }

        let target_db = linear_to_dbfs(linear_peak);
        let stereo_offset = if target_db > -55.0 {
            (self.phase.sin() * 1.4) - 0.8
        } else {
            0.0
        };
        let target_db_r = (target_db + stereo_offset).clamp(-60.0, 0.0);

        // Fast attack, smooth exponential release (studio PPM ballistics)
        if target_db >= self.smoothed_db {
            self.smoothed_db = target_db;
        } else {
            self.smoothed_db = (self.smoothed_db - 38.0 * dt).max(target_db);
        }

        if target_db_r >= self.smoothed_db_r {
            self.smoothed_db_r = target_db_r;
        } else {
            self.smoothed_db_r = (self.smoothed_db_r - 38.0 * dt).max(target_db_r);
        }

        // Peak hold for 650ms, then decay at 22 dB/s
        if target_db >= self.peak_hold_db {
            self.peak_hold_db = target_db;
            self.last_peak_time = now;
        } else if now.duration_since(self.last_peak_time).as_secs_f32() > 0.65 {
            self.peak_hold_db = (self.peak_hold_db - 22.0 * dt).max(self.smoothed_db);
        }

        if target_db_r >= self.peak_hold_db_r {
            self.peak_hold_db_r = target_db_r;
        } else if now.duration_since(self.last_peak_time).as_secs_f32() > 0.65 {
            self.peak_hold_db_r = (self.peak_hold_db_r - 22.0 * dt).max(self.smoothed_db_r);
        }
    }
}

fn draw_segmented_meter_track(
    painter: &egui::Painter,
    track_rect: Rect,
    active_norm: f32,
    peak_norm: f32,
    gate_norm: Option<f32>,
) {
    // 6px high dark track (`#202024`)
    painter.rect_filled(track_rect, Rounding::same(3.0), palette::CHIP_IDLE_BG());

    let w = track_rect.width();
    if active_norm > 0.005 && w > 4.0 {
        let seg_w = 3.0_f32;
        let gap = 1.0_f32;
        let step = seg_w + gap;
        let max_x = track_rect.min.x + (active_norm * w).min(w);
        let mut x = track_rect.min.x;

        while x + 1.0 <= max_x {
            let x_end = (x + seg_w).min(max_x);
            let pos_norm = (x - track_rect.min.x) / w;
            let col = if pos_norm >= 0.94 {
                palette::STATUS_DANGER()
            } else if pos_norm >= 0.78 {
                palette::STATUS_WARN()
            } else {
                palette::ACCENT()
            };
            let seg_rect = Rect::from_min_max(
                Pos2::new(x, track_rect.min.y),
                Pos2::new(x_end, track_rect.max.y),
            );
            painter.rect_filled(seg_rect, Rounding::same(0.8), col);
            x += step;
        }
    }

    // 2px crisp peak-hold tick
    if peak_norm > 0.015 {
        let px = (track_rect.min.x + peak_norm * w).clamp(track_rect.min.x, track_rect.max.x - 2.0);
        let peak_rect = Rect::from_min_size(
            Pos2::new(px, track_rect.min.y),
            Vec2::new(2.0, track_rect.height()),
        );
        painter.rect_filled(peak_rect, Rounding::same(1.0), palette::PEAK_HOLD_TICK());
    }

    // Optional Noise Gate marker line extending 4px above and below the track
    if let Some(gn) = gate_norm {
        let gx = track_rect.min.x + gn.clamp(0.0, 1.0) * w;
        painter.line_segment(
            [
                Pos2::new(gx, track_rect.min.y - 4.0),
                Pos2::new(gx, track_rect.max.y + 4.0),
            ],
            Stroke::new(1.5_f32, palette::TEXT_PRIMARY().gamma_multiply(0.6)),
        );
    }
}

fn draw_db_scale_row(painter: &egui::Painter, scale_rect: Rect) {
    let ticks: &[(&str, f32)] = &[
        ("-60", 0.0),
        ("-48", 0.20),
        ("-36", 0.40),
        ("-24", 0.60),
        ("-12", 0.80),
        ("-6", 0.90),
        ("0 dB", 1.0),
    ];
    let font = FontId::new(10.0, FontFamily::Monospace);
    for &(label, norm) in ticks {
        let x = scale_rect.min.x + norm * scale_rect.width();
        let align = if norm <= 0.01 {
            egui::Align2::LEFT_CENTER
        } else if norm >= 0.99 {
            egui::Align2::RIGHT_CENTER
        } else {
            egui::Align2::CENTER_CENTER
        };
        painter.text(
            Pos2::new(x, scale_rect.center().y),
            align,
            label,
            font.clone(),
            palette::TEXT_MUTED(),
        );
    }
}

/// Draw the Quiet Graphite Stereo (`L` / `R`) segmented VU meter with `-60..0 dB` scale (Frames C & C2).
pub fn draw_stereo_vu_meter(ui: &mut Ui, meter: &VuMeterState, active: bool, muted: bool) {
    let avail_w = ui.available_width();
    let total_h = 44.0;
    let (rect, _) = ui.allocate_exact_size(Vec2::new(avail_w, total_h), Sense::hover());

    if ui.is_rect_visible(rect) {
        let p = ui.painter();
        let label_w = 18.0;
        let track_x0 = rect.min.x + label_w;
        let track_w = (rect.width() - label_w).max(20.0);
        let font = FontId::new(10.0, FontFamily::Monospace);

        let (norm_l, peak_l, norm_r, peak_r) = if active && !muted {
            (
                dbfs_to_norm(meter.smoothed_db),
                dbfs_to_norm(meter.peak_hold_db),
                dbfs_to_norm(meter.smoothed_db_r),
                dbfs_to_norm(meter.peak_hold_db_r),
            )
        } else {
            (0.0, 0.0, 0.0, 0.0)
        };

        // Row L
        let l_y = rect.min.y + 5.0;
        p.text(
            Pos2::new(rect.min.x, l_y),
            egui::Align2::LEFT_CENTER,
            "L",
            font.clone(),
            palette::TEXT_MUTED(),
        );
        let track_l = Rect::from_min_size(Pos2::new(track_x0, l_y - 3.0), Vec2::new(track_w, 6.0));
        draw_segmented_meter_track(p, track_l, norm_l, peak_l, None);

        // Row R
        let r_y = rect.min.y + 21.0;
        p.text(
            Pos2::new(rect.min.x, r_y),
            egui::Align2::LEFT_CENTER,
            "R",
            font,
            palette::TEXT_MUTED(),
        );
        let track_r = Rect::from_min_size(Pos2::new(track_x0, r_y - 3.0), Vec2::new(track_w, 6.0));
        draw_segmented_meter_track(p, track_r, norm_r, peak_r, None);

        // Scale Row
        let scale_rect = Rect::from_min_size(
            Pos2::new(track_x0, rect.min.y + 30.0),
            Vec2::new(track_w, 14.0),
        );
        draw_db_scale_row(p, scale_rect);
    }
}

/// Draw the Quiet Graphite Mono (`M`) VU meter with floating `"▼ 噪声门 -42 dB"` marker (Frame C2 Right Card).
pub fn draw_mono_gate_vu_meter(
    ui: &mut Ui,
    meter: &VuMeterState,
    active: bool,
    muted: bool,
    gate_threshold_db: f32,
    gate_label_prefix: &str,
) {
    let avail_w = ui.available_width();
    let total_h = 46.0;
    let (rect, _) = ui.allocate_exact_size(Vec2::new(avail_w, total_h), Sense::hover());

    if ui.is_rect_visible(rect) {
        let p = ui.painter();
        let label_w = 18.0;
        let track_x0 = rect.min.x + label_w;
        let track_w = (rect.width() - label_w).max(20.0);

        let gate_norm = dbfs_to_norm(gate_threshold_db);
        let gate_x = (track_x0 + gate_norm * track_w).clamp(track_x0 + 4.0, rect.max.x - 64.0);
        let gate_text = format!("{gate_label_prefix} {:.0} dB", gate_threshold_db);
        p.text(
            Pos2::new(gate_x - 4.0, rect.min.y + 6.0),
            egui::Align2::LEFT_CENTER,
            gate_text,
            FontId::new(10.5, FontFamily::Proportional),
            palette::TEXT_SECONDARY(),
        );

        let (norm_m, peak_m) = if active && !muted {
            (
                dbfs_to_norm(meter.smoothed_db),
                dbfs_to_norm(meter.peak_hold_db),
            )
        } else {
            (0.0, 0.0)
        };

        let m_y = rect.min.y + 21.0;
        p.text(
            Pos2::new(rect.min.x, m_y),
            egui::Align2::LEFT_CENTER,
            "M",
            FontId::new(10.0, FontFamily::Monospace),
            palette::TEXT_MUTED(),
        );
        let track_m = Rect::from_min_size(Pos2::new(track_x0, m_y - 3.0), Vec2::new(track_w, 6.0));
        draw_segmented_meter_track(p, track_m, norm_m, peak_m, Some(gate_norm));

        let scale_rect = Rect::from_min_size(
            Pos2::new(track_x0, rect.min.y + 31.0),
            Vec2::new(track_w, 14.0),
        );
        draw_db_scale_row(p, scale_rect);
    }
}

/// Backwards-compatible VU meter entry point.
pub fn draw_vu_meter(
    ui: &mut Ui,
    _label: &str,
    meter: &VuMeterState,
    active: bool,
    muted: bool,
    gate_threshold_db: Option<f32>,
) {
    if let Some(gate_db) = gate_threshold_db {
        draw_mono_gate_vu_meter(ui, meter, active, muted, gate_db, "▼ 噪声门");
    } else {
        draw_stereo_vu_meter(ui, meter, active, muted);
    }
}

// =============================================================================
// 3-Column Hairline Stats Row & Charts
// =============================================================================

/// Draw a full-bleed `1px` horizontal hairline across the current `ui` clip/available width.
pub fn draw_hairline(ui: &mut Ui) {
    let avail_w = ui.available_width();
    let (rect, _) = ui.allocate_exact_size(Vec2::new(avail_w, 1.0), Sense::hover());
    if ui.is_rect_visible(rect) {
        ui.painter().line_segment(
            [
                Pos2::new(rect.min.x, rect.center().y),
                Pos2::new(rect.max.x, rect.center().y),
            ],
            Stroke::new(1.0_f32, palette::LINE()),
        );
    }
}

/// Render the 3-column `.stats` strip divided by `1px` vertical and horizontal hairlines.
pub fn draw_stats_row_3(ui: &mut Ui, stats: [(&str, &str, &str); 3], active: bool) {
    let avail_w = ui.available_width();
    let row_h = 66.0;
    let (rect, _) = ui.allocate_exact_size(Vec2::new(avail_w, row_h), Sense::hover());

    if ui.is_rect_visible(rect) {
        let p = ui.painter();
        let line_stroke = Stroke::new(1.0_f32, palette::LINE());
        // Top and bottom hairlines
        p.line_segment(
            [Pos2::new(rect.min.x, rect.min.y), Pos2::new(rect.max.x, rect.min.y)],
            line_stroke,
        );
        p.line_segment(
            [Pos2::new(rect.min.x, rect.max.y), Pos2::new(rect.max.x, rect.max.y)],
            line_stroke,
        );

        let col_w = rect.width() / 3.0;
        let val_color = if active {
            palette::TEXT_PRIMARY()
        } else {
            palette::TEXT_SECONDARY()
        };

        for (i, &(caption, val, unit)) in stats.iter().enumerate() {
            let x0 = rect.min.x + (i as f32) * col_w;
            if i > 0 {
                p.line_segment(
                    [Pos2::new(x0, rect.min.y), Pos2::new(x0, rect.max.y)],
                    line_stroke,
                );
            }
            let pad_x = x0 + 20.0;
            p.text(
                Pos2::new(pad_x, rect.min.y + 21.0),
                egui::Align2::LEFT_CENTER,
                caption,
                FontId::new(11.5, FontFamily::Proportional),
                palette::TEXT_MUTED(),
            );

            let val_font = FontId::new(16.0, FontFamily::Monospace);
            let val_galley = p.layout_no_wrap(val.to_string(), val_font.clone(), val_color);
            let val_pos = Pos2::new(pad_x, rect.min.y + 43.0 - val_galley.size().y * 0.5);
            let val_w = val_galley.size().x;
            p.galley(val_pos, val_galley, val_color);

            if !unit.is_empty() {
                p.text(
                    Pos2::new(pad_x + val_w + 4.0, rect.min.y + 44.5),
                    egui::Align2::LEFT_CENTER,
                    unit,
                    FontId::new(11.0, FontFamily::Proportional),
                    palette::TEXT_MUTED(),
                );
            }
        }
    }
}

/// Fixed-capacity rolling history buffer for real-time sparkline telemetry.
#[derive(Debug, Clone)]
pub struct SparklineBuffer {
    pub samples: VecDeque<f32>,
    capacity: usize,
}

impl SparklineBuffer {
    pub fn new(capacity: usize) -> Self {
        Self {
            samples: VecDeque::with_capacity(capacity),
            capacity,
        }
    }

    pub fn push(&mut self, value: f32) {
        if self.samples.len() >= self.capacity {
            self.samples.pop_front();
        }
        self.samples.push_back(value);
    }

    pub fn clear(&mut self) {
        self.samples.clear();
    }

    fn effective_capacity(&self) -> usize {
        self.capacity.max(self.samples.len()).max(2)
    }
}

/// Draw the symmetric vertical bar waveform chart for Frame C `发送` card (`.chart`).
pub fn draw_tx_waveform_chart(
    ui: &mut Ui,
    left_caption: &str,
    right_caption: &str,
    idle_placeholder: &str,
    history: &SparklineBuffer,
    active: bool,
) {
    let avail_w = ui.available_width();
    let total_h = 104.0;
    let (rect, _) = ui.allocate_exact_size(Vec2::new(avail_w, total_h), Sense::hover());

    if ui.is_rect_visible(rect) {
        let p = ui.painter();
        // Bottom hairline
        p.line_segment(
            [Pos2::new(rect.min.x, rect.max.y), Pos2::new(rect.max.x, rect.max.y)],
            Stroke::new(1.0_f32, palette::LINE()),
        );

        let inner_x0 = rect.min.x + 20.0;
        let inner_x1 = rect.max.x - 20.0;
        let head_y = rect.min.y + 22.0;

        p.text(
            Pos2::new(inner_x0, head_y),
            egui::Align2::LEFT_CENTER,
            left_caption,
            FontId::new(11.5, FontFamily::Proportional),
            palette::TEXT_MUTED(),
        );
        p.text(
            Pos2::new(inner_x1, head_y),
            egui::Align2::RIGHT_CENTER,
            right_caption,
            FontId::new(11.5, FontFamily::Monospace),
            palette::TEXT_MUTED(),
        );

        let chart_rect = Rect::from_min_max(
            Pos2::new(inner_x0, rect.min.y + 38.0),
            Pos2::new(inner_x1, rect.max.y - 14.0),
        );

        if active && !history.samples.is_empty() {
            let bar_w = 2.4_f32;
            let step = 4.7_f32;
            let num_bars = ((chart_rect.width() / step).floor() as usize).max(16);
            let cy = chart_rect.center().y;
            let max_h = chart_rect.height() * 0.92;

            for i in 0..num_bars {
                let t = i as f32 / ((num_bars - 1).max(1) as f32);
                let sample_idx = (i * history.samples.len()) / num_bars;
                let norm = history
                    .samples
                    .get(sample_idx)
                    .copied()
                    .unwrap_or(0.35)
                    .clamp(0.12, 0.96);

                let bh = (norm * max_h).max(6.0);
                let x = chart_rect.min.x + (i as f32) * step;
                let bar_rect =
                    Rect::from_min_size(Pos2::new(x, cy - bh * 0.5), Vec2::new(bar_w, bh));

                let alpha = ((0.20 + 0.80 * t.powf(0.85)) * 255.0) as u8;
                let [r, g, b, _] = palette::ACCENT().to_array();
                p.rect_filled(
                    bar_rect,
                    Rounding::same(1.2),
                    Color32::from_rgba_unmultiplied(r, g, b, alpha),
                );
            }
        } else {
            draw_idle_chart_grid(p, chart_rect, 0.5, idle_placeholder);
        }
    }
}

fn draw_idle_chart_grid(
    painter: &egui::Painter,
    chart_rect: Rect,
    dashed_y_norm: f32,
    placeholder: &str,
) {
    // Subtle vertical grid lines every 45px
    let mut gx = chart_rect.min.x;
    while gx <= chart_rect.max.x {
        painter.line_segment(
            [Pos2::new(gx, chart_rect.min.y), Pos2::new(gx, chart_rect.max.y)],
            Stroke::new(1.0_f32, palette::BG_RAISED()),
        );
        gx += 45.0;
    }

    // Horizontal dashed line
    let dy = chart_rect.max.y - dashed_y_norm.clamp(0.1, 0.9) * chart_rect.height();
    draw_dashed_hline(
        painter,
        chart_rect.min.x,
        chart_rect.max.x,
        dy,
        palette::LINE_STRONG(),
    );

    if !placeholder.is_empty() {
        let font = FontId::new(12.0, FontFamily::Proportional);
        let galley = painter.layout_no_wrap(placeholder.to_string(), font.clone(), palette::TEXT_MUTED());
        let bg_rect = Rect::from_center_size(
            chart_rect.center(),
            Vec2::new(galley.size().x + 20.0, galley.size().y + 6.0),
        );
        painter.rect_filled(bg_rect, Rounding::same(4.0), palette::BG_PANEL());
        painter.text(
            chart_rect.center(),
            egui::Align2::CENTER_CENTER,
            placeholder,
            font,
            palette::TEXT_MUTED(),
        );
    }
}

fn draw_dashed_hline(painter: &egui::Painter, x0: f32, x1: f32, y: f32, color: Color32) {
    let dash = 3.5_f32;
    let gap = 4.0_f32;
    let mut x = x0;
    let stroke = Stroke::new(1.0_f32, color);
    while x < x1 {
        let xe = (x + dash).min(x1);
        painter.line_segment([Pos2::new(x, y), Pos2::new(xe, y)], stroke);
        x += dash + gap;
    }
}

/// Draw the Quiet Graphite area sparkline chart with dashed target line (Frames C & C2).
#[allow(clippy::too_many_arguments)]
pub fn draw_sparkline(
    ui: &mut Ui,
    title: &str,
    right_summary: &str,
    empty_placeholder: &str,
    history: &SparklineBuffer,
    min_val: f32,
    max_val: f32,
    target_line: Option<f32>,
    stroke_color: Color32,
) {
    let avail_w = ui.available_width();
    let total_h = 104.0;
    let (rect, _) = ui.allocate_exact_size(Vec2::new(avail_w, total_h), Sense::hover());

    if ui.is_rect_visible(rect) {
        let p = ui.painter();
        p.line_segment(
            [Pos2::new(rect.min.x, rect.max.y), Pos2::new(rect.max.x, rect.max.y)],
            Stroke::new(1.0_f32, palette::LINE()),
        );

        let inner_x0 = rect.min.x + 20.0;
        let inner_x1 = rect.max.x - 20.0;
        let head_y = rect.min.y + 22.0;

        p.text(
            Pos2::new(inner_x0, head_y),
            egui::Align2::LEFT_CENTER,
            title,
            FontId::new(11.5, FontFamily::Proportional),
            palette::TEXT_MUTED(),
        );
        p.text(
            Pos2::new(inner_x1, head_y),
            egui::Align2::RIGHT_CENTER,
            right_summary,
            FontId::new(11.5, FontFamily::Monospace),
            palette::TEXT_MUTED(),
        );

        let chart_rect = Rect::from_min_max(
            Pos2::new(inner_x0, rect.min.y + 38.0),
            Pos2::new(inner_x1, rect.max.y - 14.0),
        );
        let span = (max_val - min_val).max(0.0001);
        let target_norm = target_line
            .map(|t| ((t - min_val) / span).clamp(0.1, 0.9))
            .unwrap_or(0.5);

        if history.samples.len() >= 2 {
            let n = history.effective_capacity();
            let mut points = Vec::with_capacity(history.samples.len());
            for (idx, &val) in history.samples.iter().enumerate() {
                let x_norm = idx as f32 / ((n - 1).max(1) as f32);
                let y_norm = ((val - min_val) / span).clamp(0.06, 0.94);
                let x = chart_rect.min.x + x_norm * chart_rect.width();
                let y = chart_rect.max.y - y_norm * chart_rect.height();
                points.push(Pos2::new(x, y));
            }

            let [sr, sg, sb, _] = stroke_color.to_array();
            let [br, bg, bb, _] = palette::BG_PANEL().to_array();
            let cols = (chart_rect.width().ceil() as usize).max(32);
            let col_w = chart_rect.width() / (cols as f32);
            let bands = 6usize;

            for c_idx in 0..cols {
                let x0 = chart_rect.min.x + (c_idx as f32) * col_w;
                let x_center = x0 + col_w * 0.5;
                if let Some(y_curve) = interpolate_y_at_x(&points, x_center) {
                    let h_span = (chart_rect.max.y - y_curve).max(0.0);
                    if h_span > 0.5 {
                        let band_h = h_span / (bands as f32);
                        for b_idx in 0..bands {
                            let by0 = y_curve + (b_idx as f32) * band_h;
                            let by1 = (by0 + band_h + 0.4).min(chart_rect.max.y);
                            let fade = 1.0 - (b_idx as f32 + 0.5) / (bands as f32);
                            let mix = 0.20 * fade * fade;
                            let r = (br as f32 * (1.0 - mix) + sr as f32 * mix) as u8;
                            let g = (bg as f32 * (1.0 - mix) + sg as f32 * mix) as u8;
                            let b = (bb as f32 * (1.0 - mix) + sb as f32 * mix) as u8;
                            p.rect_filled(
                                Rect::from_min_max(Pos2::new(x0, by0), Pos2::new(x0 + col_w + 0.4, by1)),
                                Rounding::ZERO,
                                Color32::from_rgb(r, g, b),
                            );
                        }
                    }
                }
            }

            // Dashed reference line
            let ty = chart_rect.max.y - target_norm * chart_rect.height();
            draw_dashed_hline(
                p,
                chart_rect.min.x,
                chart_rect.max.x,
                ty,
                palette::LINE_STRONG(),
            );

            p.add(egui::Shape::line(
                points,
                Stroke::new(1.6_f32, stroke_color),
            ));
        } else {
            draw_idle_chart_grid(p, chart_rect, target_norm, empty_placeholder);
        }
    }
}

fn interpolate_y_at_x(points: &[Pos2], x: f32) -> Option<f32> {
    if points.len() < 2 {
        return None;
    }
    if x < points.first()?.x || x > points.last()?.x {
        return None;
    }
    for pair in points.windows(2) {
        let p0 = pair[0];
        let p1 = pair[1];
        if x >= p0.x && x <= p1.x {
            let dx = (p1.x - p0.x).max(0.001);
            let t = (x - p0.x) / dx;
            return Some(p0.y + t * (p1.y - p0.y));
        }
    }
    None
}

// =============================================================================
// Quiet Graphite Form Controls: Segmented Pill, Toggle Switch, Slider, Row
// =============================================================================

/// Render a Quiet Graphite `.seg` segmented pill control (`#0E0E10` well, `#232327` active segment).
/// Allocates a single atomic `Rect` so it aligns cleanly in both LTR and RTL parent layouts.
pub fn segmented_control<T: Copy + PartialEq>(
    ui: &mut Ui,
    id_salt: &str,
    current: &mut T,
    options: &[(T, &str)],
) -> bool {
    let mut changed = false;
    let font = FontId::new(12.0, FontFamily::Proportional);
    let mut seg_widths = Vec::with_capacity(options.len());
    for &(_, label) in options {
        let galley = ui
            .painter()
            .layout_no_wrap(label.to_string(), font.clone(), Color32::WHITE);
        seg_widths.push((galley.size().x + 22.0).max(46.0));
    }
    let inner_gaps = (options.len().saturating_sub(1) as f32) * 2.0;
    let total_w: f32 = seg_widths.iter().sum::<f32>() + inner_gaps + 4.0;
    let total_h = 30.0;

    let (outer_rect, _) = ui.allocate_exact_size(Vec2::new(total_w, total_h), Sense::hover());
    if ui.is_rect_visible(outer_rect) {
        let p = ui.painter();
        p.rect(
            outer_rect,
            Rounding::same(8.0),
            palette::BG_CANVAS(),
            Stroke::new(1.0_f32, palette::LINE()),
        );

        let mut x = outer_rect.min.x + 2.0;
        let y = outer_rect.min.y + 2.0;
        for (idx, &(val, label)) in options.iter().enumerate() {
            let w = seg_widths[idx];
            let seg_rect = Rect::from_min_size(Pos2::new(x, y), Vec2::new(w, 26.0));
            let resp = ui.interact(seg_rect, ui.id().with((id_salt, idx)), Sense::click());
            if resp.hovered() {
                ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
            }
            let selected = *current == val;
            if resp.clicked() && !selected {
                *current = val;
                changed = true;
            }
            if selected {
                p.rect(
                    seg_rect,
                    Rounding::same(metrics::RADIUS_SEG_INNER),
                    palette::BG_HOVER(),
                    Stroke::new(1.0_f32, palette::INSET_HIGHLIGHT()),
                );
            }
            let fg = if selected || resp.hovered() {
                palette::TEXT_PRIMARY()
            } else {
                palette::TEXT_SECONDARY()
            };
            p.text(
                seg_rect.center(),
                egui::Align2::CENTER_CENTER,
                label,
                font.clone(),
                fg,
            );
            x += w + 2.0;
        }
    }
    changed
}

/// Render a 4-option segmented selector for `LatencyProfile` matching `.seg` in C & C2.
pub fn latency_profile_selector(ui: &mut Ui, current: &mut LatencyProfile, lang: Language) {
    let t = lang.texts();
    let options = [
        (LatencyProfile::UltraLow, t.profile_ultra_low),
        (LatencyProfile::Balanced, t.profile_balanced),
        (LatencyProfile::Stable, t.profile_stable),
        (LatencyProfile::LosslessPcm, t.profile_lossless),
    ];
    segmented_control(ui, "latency_profile_seg", current, &options);
}

/// Render a Quiet Graphite `30x18` pill toggle switch (`.toggle` in C & C2).
pub fn pill_toggle(ui: &mut Ui, on: &mut bool) -> bool {
    let (rect, resp) = ui.allocate_exact_size(Vec2::new(30.0, 18.0), Sense::click());
    if resp.hovered() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
    }
    let mut changed = false;
    if resp.clicked() {
        *on = !*on;
        changed = true;
    }
    if ui.is_rect_visible(rect) {
        let p = ui.painter();
        let bg = if *on {
            palette::ACCENT()
        } else {
            palette::TOGGLE_OFF_BG()
        };
        let knob_col = if *on {
            palette::BG_CANVAS()
        } else {
            palette::TOGGLE_OFF_KNOB()
        };
        p.rect_filled(rect, Rounding::same(9.0), bg);
        let kx = if *on {
            rect.max.x - 9.0
        } else {
            rect.min.x + 9.0
        };
        p.circle_filled(Pos2::new(kx, rect.center().y), 7.0, knob_col);
    }
    changed
}

/// Render a Quiet Graphite slider (`150px` 4px track on left + `14px` white thumb + `52px` mono value on right).
pub fn quiet_slider_f32(
    ui: &mut Ui,
    value: &mut f32,
    range: std::ops::RangeInclusive<f32>,
    formatted_val: &str,
) -> bool {
    let mut changed = false;
    let track_w = 150.0;
    let gap = 12.0;
    let val_w = 54.0;
    let total_w = track_w + gap + val_w;

    let (full_rect, _) = ui.allocate_exact_size(Vec2::new(total_w, 24.0), Sense::hover());
    let s_rect = Rect::from_min_size(full_rect.min, Vec2::new(track_w, full_rect.height()));
    let val_rect = Rect::from_min_size(
        Pos2::new(full_rect.min.x + track_w + gap, full_rect.min.y),
        Vec2::new(val_w, full_rect.height()),
    );

    let resp = ui.interact(s_rect, ui.next_auto_id(), Sense::click_and_drag());
    if resp.hovered() || resp.dragged() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
    }
    let min = *range.start();
    let max = *range.end();
    let span = (max - min).max(0.0001);

    if (resp.dragged() || resp.clicked()) && s_rect.width() > 1.0 {
        if let Some(pos) = resp.interact_pointer_pos() {
            let norm = ((pos.x - s_rect.min.x) / s_rect.width()).clamp(0.0, 1.0);
            let new_v = min + norm * span;
            if (new_v - *value).abs() > 1e-4 {
                *value = new_v;
                changed = true;
            }
        }
    }

    if ui.is_rect_visible(full_rect) {
        let p = ui.painter();
        let norm = ((*value - min) / span).clamp(0.0, 1.0);
        let bar_rect = Rect::from_min_size(
            Pos2::new(s_rect.min.x, s_rect.center().y - 2.0),
            Vec2::new(s_rect.width(), 4.0),
        );
        p.rect_filled(bar_rect, Rounding::same(2.0), palette::SLIDER_TRACK_BG());
        let fill_rect = Rect::from_min_size(
            bar_rect.min,
            Vec2::new(norm * bar_rect.width(), bar_rect.height()),
        );
        p.rect_filled(fill_rect, Rounding::same(2.0), palette::TEXT_SECONDARY());
        let thumb_x = bar_rect.min.x + norm * bar_rect.width();
        let thumb_center = Pos2::new(thumb_x, bar_rect.center().y);
        p.circle_filled(thumb_center, 7.0, Color32::from_rgb(248, 248, 250));
        p.circle_stroke(thumb_center, 7.0, Stroke::new(1.0_f32, palette::LINE_STRONG()));

        p.text(
            Pos2::new(val_rect.max.x, val_rect.center().y),
            egui::Align2::RIGHT_CENTER,
            formatted_val,
            FontId::new(12.0, FontFamily::Monospace),
            palette::TEXT_PRIMARY(),
        );
    }
    changed
}

/// Render a Quiet Graphite integer `u16` slider.
pub fn quiet_slider_u16(
    ui: &mut Ui,
    value: &mut u16,
    range: std::ops::RangeInclusive<u16>,
    formatted_val: &str,
) -> bool {
    let mut f_val = *value as f32;
    let f_min = *range.start() as f32;
    let f_max = *range.end() as f32;
    if quiet_slider_f32(ui, &mut f_val, f_min..=f_max, formatted_val) {
        *value = f_val.round().clamp(f_min, f_max) as u16;
        true
    } else {
        false
    }
}

/// Render a Quiet Graphite `.sel` dropdown selector pill (`#1C1C1F` fill, `7px` radius, `#EDEDEE` text + chevron).
pub fn quiet_select_pill<T: Clone + PartialEq>(
    ui: &mut Ui,
    id_salt: &str,
    current: &mut T,
    selected_label: &str,
    options: &[(T, String)],
) -> bool {
    let mut changed = false;
    let font = FontId::new(12.5, FontFamily::Proportional);
    let galley = ui
        .painter()
        .layout_no_wrap(selected_label.to_string(), font.clone(), palette::TEXT_PRIMARY());
    let pill_w = (galley.size().x + 36.0).clamp(88.0, 250.0);
    let pill_h = 30.0;

    let (rect, resp) = ui.allocate_exact_size(Vec2::new(pill_w, pill_h), Sense::click());
    if resp.hovered() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
    }

    let popup_id = ui.make_persistent_id(id_salt);
    if resp.clicked() {
        ui.memory_mut(|mem| mem.toggle_popup(popup_id));
    }

    if ui.is_rect_visible(rect) {
        let p = ui.painter();
        let fill = if resp.hovered() {
            palette::BG_HOVER()
        } else {
            palette::BG_RAISED()
        };
        p.rect(
            rect,
            Rounding::same(metrics::RADIUS_SELECT),
            fill,
            Stroke::new(1.0_f32, palette::INSET_HIGHLIGHT()),
        );
        p.text(
            Pos2::new(rect.min.x + 12.0, rect.center().y),
            egui::Align2::LEFT_CENTER,
            selected_label,
            font,
            palette::TEXT_PRIMARY(),
        );
        // Chevron `⌄` on right
        let cx = rect.max.x - 14.0;
        let cy = rect.center().y;
        let chev_stroke = Stroke::new(1.5_f32, palette::TEXT_MUTED());
        p.line_segment(
            [Pos2::new(cx - 3.5, cy - 1.5), Pos2::new(cx, cy + 2.0)],
            chev_stroke,
        );
        p.line_segment(
            [Pos2::new(cx, cy + 2.0), Pos2::new(cx + 3.5, cy - 1.5)],
            chev_stroke,
        );
    }

    egui::popup_below_widget(
        ui,
        popup_id,
        &resp,
        egui::PopupCloseBehavior::CloseOnClick,
        |ui| {
            ui.set_min_width(pill_w.max(180.0));
            for (val, label) in options {
                let is_sel = *current == *val;
                let r = ui.selectable_label(
                    is_sel,
                    RichText::new(label)
                        .size(12.5)
                        .color(if is_sel {
                            palette::ACCENT()
                        } else {
                            palette::TEXT_PRIMARY()
                        }),
                );
                if r.clicked() && !is_sel {
                    *current = val.clone();
                    changed = true;
                }
            }
        },
    );

    changed
}

/// Render a Quiet Graphite `.sel.mono` input pill (`#1C1C1F` fill, `7px` radius, `12px` mono `#EDEDEE`).
pub fn quiet_mono_input_pill(ui: &mut Ui, id_salt: &str, text: &mut String) {
    let font = FontId::new(12.0, FontFamily::Monospace);
    let galley = ui
        .painter()
        .layout_no_wrap(text.clone(), font.clone(), palette::TEXT_PRIMARY());
    let pill_w = (galley.size().x + 36.0).clamp(136.0, 224.0);
    let pill_h = 30.0;

    let (rect, _) = ui.allocate_exact_size(Vec2::new(pill_w, pill_h), Sense::hover());
    if ui.is_rect_visible(rect) {
        ui.painter().rect(
            rect,
            Rounding::same(metrics::RADIUS_SELECT),
            palette::BG_RAISED(),
            Stroke::new(1.0_f32, palette::INSET_HIGHLIGHT()),
        );
    }

    let inner_rect = rect.shrink2(Vec2::new(10.0, 4.0));
    let mut child_ui = ui.new_child(
        egui::UiBuilder::new()
            .max_rect(inner_rect)
            .layout(egui::Layout::left_to_right(Align::Center)),
    );
    child_ui.push_id(id_salt, |ui| {
        ui.add(
            egui::TextEdit::singleline(text)
                .frame(false)
                .clip_text(false)
                .font(font)
                .text_color(palette::TEXT_PRIMARY())
                .desired_width(inner_rect.width()),
        );
    });
}

/// Render a single `.set .row` setting line (`52px` high, label on left, right-aligned control, optional bottom hairline).
pub fn settings_row(
    ui: &mut Ui,
    label: &str,
    has_bottom_border: bool,
    add_right_control: impl FnOnce(&mut Ui),
) {
    let avail_w = ui.available_width();
    let row_h = metrics::SETTING_ROW_HEIGHT;
    let (row_rect, _) = ui.allocate_exact_size(Vec2::new(avail_w, row_h), Sense::hover());

    if ui.is_rect_visible(row_rect) {
        let p = ui.painter();
        p.text(
            Pos2::new(row_rect.min.x + 20.0, row_rect.center().y),
            egui::Align2::LEFT_CENTER,
            label,
            FontId::new(12.5, FontFamily::Proportional),
            palette::TEXT_SECONDARY(),
        );
        if has_bottom_border {
            p.line_segment(
                [
                    Pos2::new(row_rect.min.x + 20.0, row_rect.max.y),
                    Pos2::new(row_rect.max.x - 20.0, row_rect.max.y),
                ],
                Stroke::new(1.0_f32, palette::LINE()),
            );
        }
    }

    let right_rect = Rect::from_min_max(
        Pos2::new(row_rect.min.x + 116.0, row_rect.min.y),
        Pos2::new(row_rect.max.x - 20.0, row_rect.max.y),
    );
    let mut child_ui = ui.new_child(
        egui::UiBuilder::new()
            .max_rect(right_rect)
            .layout(egui::Layout::right_to_left(Align::Center)),
    );
    add_right_control(&mut child_ui);
}

/// Backwards-compatible metric tile helper.
pub fn metric_tile(
    ui: &mut Ui,
    title: &str,
    primary_value: &str,
    sub_value: &str,
    value_color: Color32,
) {
    egui::Frame::none()
        .fill(palette::BG_RAISED())
        .stroke(Stroke::new(1.0_f32, palette::LINE()))
        .rounding(Rounding::same(metrics::RADIUS_WIDGET))
        .inner_margin(egui::Margin::symmetric(10.0, 7.0))
        .show(ui, |ui| {
            ui.set_min_width(ui.available_width());
            ui.vertical(|ui| {
                ui.spacing_mut().item_spacing.y = 2.0;
                ui.label(RichText::new(title).size(11.0).color(palette::TEXT_MUTED()));
                ui.label(
                    RichText::new(primary_value)
                        .size(14.5)
                        .family(FontFamily::Monospace)
                        .color(value_color)
                        .strong(),
                );
                ui.label(
                    RichText::new(sub_value)
                        .size(11.0)
                        .family(FontFamily::Monospace)
                        .color(palette::TEXT_SECONDARY()),
                );
            });
        });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_linear_to_dbfs_and_norm() {
        assert_eq!(linear_to_dbfs(0.0), -60.0);
        assert!((linear_to_dbfs(1.0) - 0.0).abs() < 1e-4);
        assert!((linear_to_dbfs(0.5) - (-6.0206)).abs() < 0.05);

        assert_eq!(dbfs_to_norm(-60.0), 0.0);
        assert_eq!(dbfs_to_norm(0.0), 1.0);
        assert!((dbfs_to_norm(-30.0) - 0.5).abs() < 1e-4);
    }

    #[test]
    fn test_vu_meter_ballistics_and_sparkline() {
        let mut meter = VuMeterState::default();
        meter.update(0.5, true);
        assert!(meter.smoothed_db > -7.0 && meter.smoothed_db < -5.5);
        assert_eq!(meter.peak_hold_db, meter.smoothed_db);

        meter.update(0.0, false);
        assert_eq!(meter.smoothed_db, -60.0);

        let mut spark = SparklineBuffer::new(4);
        for v in [10.0, 15.0, 20.0, 25.0, 30.0] {
            spark.push(v);
        }
        assert_eq!(spark.samples.len(), 4);
        assert_eq!(spark.samples[0], 15.0);
        assert_eq!(spark.samples[3], 30.0);
    }

    #[test]
    fn test_format_helpers() {
        assert_eq!(format_with_commas(0), "0");
        assert_eq!(format_with_commas(999), "999");
        assert_eq!(format_with_commas(863_848), "863,848");
        assert_eq!(format_gain_db(1.0), "0.0 dB");
    }
}
