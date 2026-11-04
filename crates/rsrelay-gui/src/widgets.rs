//! Custom pro-audio and real-time telemetry widgets for `rsrelay-gui`.
//!
//! Includes:
//! - Logarithmic dBFS Studio VU Meter with Peak-Hold ballistics & Noise Gate threshold marker
//! - Rolling real-time telemetry sparkline graph (Jitter Buffer watermark / signal history)
//! - Bento telemetry readout tiles, pulsing status pills, and Latency Profile selector cards

use std::collections::VecDeque;
use std::time::Instant;

use eframe::egui::{
    self, Align, Color32, FontFamily, FontId, Pos2, Rect, RichText, Rounding, Sense, Stroke, Ui,
    Vec2,
};
use rsrelay_core::LatencyProfile;

use crate::i18n::Language;
use crate::theme::{metrics, palette};

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

/// Stateful ballistics tracker for a Studio VU Meter (attack/release smoothing + peak hold).
#[derive(Debug, Clone)]
pub struct VuMeterState {
    pub smoothed_db: f32,
    pub peak_hold_db: f32,
    last_peak_time: Instant,
    last_update: Instant,
}

impl Default for VuMeterState {
    fn default() -> Self {
        let now = Instant::now();
        Self {
            smoothed_db: -60.0,
            peak_hold_db: -60.0,
            last_peak_time: now,
            last_update: now,
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

        if !active {
            self.smoothed_db = -60.0;
            self.peak_hold_db = -60.0;
            return;
        }

        let target_db = linear_to_dbfs(linear_peak);

        // Fast attack, smooth exponential release (studio PPM ballistics)
        if target_db >= self.smoothed_db {
            self.smoothed_db = target_db;
        } else {
            let release_rate_db_per_sec = 38.0;
            self.smoothed_db = (self.smoothed_db - release_rate_db_per_sec * dt).max(target_db);
        }

        // Peak hold for 650ms, then decay at 22 dB/s
        if target_db >= self.peak_hold_db {
            self.peak_hold_db = target_db;
            self.last_peak_time = now;
        } else if now.duration_since(self.last_peak_time).as_secs_f32() > 0.65 {
            self.peak_hold_db = (self.peak_hold_db - 22.0 * dt).max(self.smoothed_db);
        }
    }
}

/// Draw a segmented logarithmic dBFS Studio VU Meter with Peak-Hold and optional Noise Gate marker.
pub fn draw_vu_meter(
    ui: &mut Ui,
    label: &str,
    meter: &VuMeterState,
    active: bool,
    muted: bool,
    gate_threshold_db: Option<f32>,
) {
    ui.vertical(|ui| {
        ui.spacing_mut().item_spacing.y = 3.0;

        // Top row: label on left, numeric dBFS readout + peak hold on right
        ui.horizontal(|ui| {
            ui.label(
                RichText::new(label)
                    .size(11.5)
                    .color(palette::TEXT_SECONDARY)
                    .strong(),
            );

            ui.with_layout(egui::Layout::right_to_left(Align::Center), |ui| {
                let db_str = if !active {
                    "OFFLINE".to_string()
                } else if muted {
                    "MUTED".to_string()
                } else if meter.smoothed_db <= -59.5 {
                    "-∞ dBFS".to_string()
                } else {
                    format!("{:>5.1} dBFS", meter.smoothed_db)
                };

                let readout_color = if !active || muted {
                    palette::TEXT_MUTED
                } else if meter.smoothed_db > -3.0 {
                    palette::STATUS_DANGER
                } else if meter.smoothed_db > -12.0 {
                    palette::STATUS_WARN
                } else {
                    palette::ACCENT_MINT
                };

                ui.label(
                    RichText::new(db_str)
                        .family(FontFamily::Monospace)
                        .size(11.5)
                        .color(readout_color)
                        .strong(),
                );

                if active && !muted && meter.peak_hold_db > -58.0 {
                    ui.label(
                        RichText::new(format!("PK {:.1}", meter.peak_hold_db))
                            .family(FontFamily::Monospace)
                            .size(10.5)
                            .color(palette::TEXT_MUTED),
                    );
                }
            });
        });

        // Segmented LED bar canvas
        let desired_size = Vec2::new(ui.available_width(), 16.0);
        let (rect, _response) = ui.allocate_exact_size(desired_size, Sense::hover());

        if ui.is_rect_visible(rect) {
            let painter = ui.painter();

            // Recessed dark trench background
            painter.rect(
                rect,
                Rounding::same(4.0),
                palette::BG_TRENCH,
                Stroke::new(1.0_f32, palette::BORDER_SUBTLE),
            );

            let inner = rect.shrink2(Vec2::new(4.0, 3.0));
            let num_segments = 36usize;
            let gap = 2.0f32;
            let total_gaps = gap * (num_segments.saturating_sub(1) as f32);
            let seg_w = ((inner.width() - total_gaps) / (num_segments as f32)).max(2.0);

            let active_norm = if active && !muted {
                dbfs_to_norm(meter.smoothed_db)
            } else {
                0.0
            };
            let peak_norm = if active && !muted {
                dbfs_to_norm(meter.peak_hold_db)
            } else {
                0.0
            };

            let peak_seg_idx = ((peak_norm * (num_segments as f32)).floor() as usize)
                .min(num_segments.saturating_sub(1));

            for i in 0..num_segments {
                let seg_norm_end = (i + 1) as f32 / (num_segments as f32);
                let seg_db = -60.0 + seg_norm_end * 60.0;

                let x0 = inner.min.x + (i as f32) * (seg_w + gap);
                let seg_rect =
                    Rect::from_min_size(Pos2::new(x0, inner.min.y), Vec2::new(seg_w, inner.height()));

                let lit = active
                    && !muted
                    && active_norm >= (i as f32 / (num_segments as f32))
                    && active_norm > 0.005;
                let is_peak_hold = active && !muted && peak_norm > 0.02 && i == peak_seg_idx;

                let base_color = if seg_db > -3.0 {
                    palette::STATUS_DANGER
                } else if seg_db > -12.0 {
                    palette::STATUS_WARN
                } else {
                    palette::ACCENT_MINT
                };

                let fill = if lit || is_peak_hold {
                    base_color
                } else {
                    let [r, g, b, _] = base_color.to_array();
                    Color32::from_rgb(
                        (r as u16 / 9 + 14) as u8,
                        (g as u16 / 9 + 18) as u8,
                        (b as u16 / 9 + 28) as u8,
                    )
                };

                painter.rect_filled(seg_rect, Rounding::same(1.5), fill);
            }

            // Optional Noise Gate threshold marker
            if let Some(gate_db) = gate_threshold_db {
                let gate_norm = dbfs_to_norm(gate_db);
                let gx = inner.min.x + gate_norm * inner.width();
                painter.line_segment(
                    [Pos2::new(gx, rect.min.y), Pos2::new(gx, rect.max.y)],
                    Stroke::new(1.5_f32, palette::ACCENT_CYAN),
                );
            }
        }
    });
}

/// Fixed-capacity rolling history buffer for real-time sparkline telemetry.
#[derive(Debug, Clone)]
pub struct SparklineBuffer {
    samples: VecDeque<f32>,
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

/// Draw a sleek studio telemetry sparkline chart with optional target reference line.
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
    ui.vertical(|ui| {
        ui.spacing_mut().item_spacing.y = 3.0;
        ui.horizontal(|ui| {
            ui.label(
                RichText::new(title)
                    .size(11.0)
                    .color(palette::TEXT_SECONDARY)
                    .strong(),
            );
            ui.with_layout(egui::Layout::right_to_left(Align::Center), |ui| {
                ui.label(
                    RichText::new(right_summary)
                        .family(FontFamily::Monospace)
                        .size(11.0)
                        .color(stroke_color),
                );
            });
        });

        let desired = Vec2::new(ui.available_width(), 38.0);
        let (rect, _) = ui.allocate_exact_size(desired, Sense::hover());

        if ui.is_rect_visible(rect) {
            let painter = ui.painter();
            painter.rect(
                rect,
                Rounding::same(4.0),
                palette::BG_TRENCH,
                Stroke::new(1.0_f32, palette::BORDER_SUBTLE),
            );

            let inner = rect.shrink2(Vec2::new(4.0, 4.0));
            let span = (max_val - min_val).max(0.0001);

            // Subtle target horizontal line (e.g. target jitter buffer watermark)
            if let Some(target) = target_line {
                let norm = ((target - min_val) / span).clamp(0.0, 1.0);
                let y = inner.max.y - norm * inner.height();
                painter.line_segment(
                    [Pos2::new(inner.min.x, y), Pos2::new(inner.max.x, y)],
                    Stroke::new(
                        1.0_f32,
                        Color32::from_rgba_unmultiplied(148, 163, 184, 50),
                    ),
                );
            }

            if history.samples.len() >= 2 {
                let n = history.effective_capacity();
                let mut points = Vec::with_capacity(history.samples.len());
                for (idx, &val) in history.samples.iter().enumerate() {
                    let x_norm = idx as f32 / ((n - 1).max(1) as f32);
                    let y_norm = ((val - min_val) / span).clamp(0.0, 1.0);
                    let x = inner.min.x + x_norm * inner.width();
                    let y = inner.max.y - y_norm * inner.height();
                    points.push(Pos2::new(x, y));
                }

                let [r, g, b, _] = stroke_color.to_array();
                let bar_w: f32 = (inner.width() / (n as f32)).max(1.5);
                let fill_stroke = Stroke::new(bar_w, Color32::from_rgba_unmultiplied(r, g, b, 28));
                for pt in &points {
                    painter.line_segment([*pt, Pos2::new(pt.x, inner.max.y)], fill_stroke);
                }

                painter.add(egui::Shape::line(points, Stroke::new(1.6_f32, stroke_color)));
            } else {
                painter.text(
                    inner.center(),
                    egui::Align2::CENTER_CENTER,
                    empty_placeholder,
                    FontId::new(11.0, FontFamily::Proportional),
                    palette::TEXT_MUTED,
                );
            }
        }
    });
}

/// Render a rounded status pill badge with a glowing dot indicator.
pub fn status_pill(ui: &mut Ui, text: &str, active: bool, accent: Color32) {
    let bg = if active {
        let [r, g, b, _] = accent.to_array();
        Color32::from_rgba_unmultiplied(r, g, b, 32)
    } else {
        palette::BG_TRENCH
    };
    let border = if active {
        accent
    } else {
        palette::BORDER_SUBTLE
    };
    let fg = if active {
        accent
    } else {
        palette::TEXT_MUTED
    };

    egui::Frame::none()
        .fill(bg)
        .stroke(Stroke::new(1.0_f32, border))
        .rounding(Rounding::same(metrics::RADIUS_PILL))
        .inner_margin(egui::Margin::symmetric(10.0, 4.0))
        .show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 6.0;
                let (dot_rect, _) = ui.allocate_exact_size(Vec2::splat(7.0), Sense::hover());
                ui.painter().circle_filled(dot_rect.center(), 3.5, fg);
                ui.label(
                    RichText::new(text)
                        .size(11.0)
                        .family(FontFamily::Monospace)
                        .color(fg)
                        .strong(),
                );
            });
        });
}

/// Render a Bento-style recessed metric readout card.
pub fn metric_tile(
    ui: &mut Ui,
    title: &str,
    primary_value: &str,
    sub_value: &str,
    value_color: Color32,
) {
    egui::Frame::none()
        .fill(palette::BG_TRENCH)
        .stroke(Stroke::new(1.0_f32, palette::BORDER_SUBTLE))
        .rounding(Rounding::same(metrics::RADIUS_WIDGET))
        .inner_margin(egui::Margin::symmetric(10.0, 7.0))
        .show(ui, |ui| {
            ui.set_min_width(ui.available_width());
            ui.vertical(|ui| {
                ui.spacing_mut().item_spacing.y = 2.0;
                ui.label(
                    RichText::new(title)
                        .size(10.0)
                        .color(palette::TEXT_MUTED)
                        .strong(),
                );
                ui.label(
                    RichText::new(primary_value)
                        .size(13.5)
                        .family(FontFamily::Monospace)
                        .color(value_color)
                        .strong(),
                );
                ui.label(
                    RichText::new(sub_value)
                        .size(10.5)
                        .family(FontFamily::Monospace)
                        .color(palette::TEXT_SECONDARY),
                );
            });
        });
}

/// Render a prominent studio transport Start/Stop CTA button.
pub fn transport_cta_button(
    ui: &mut Ui,
    start_label: &str,
    stop_label: &str,
    is_running: bool,
    accent_color: Color32,
) -> bool {
    let label_str = if is_running {
        format!("■  {stop_label}")
    } else {
        format!("▶  {start_label}")
    };

    let fill = if is_running {
        palette::STATUS_DANGER_DIM
    } else {
        let [r, g, b, _] = accent_color.to_array();
        Color32::from_rgb(
            (r as u16 * 3 / 4) as u8,
            (g as u16 * 3 / 4) as u8,
            (b as u16 * 3 / 4) as u8,
        )
    };

    let stroke_col = if is_running {
        palette::STATUS_DANGER
    } else {
        accent_color
    };

    let btn = egui::Button::new(
        RichText::new(label_str)
            .size(13.5)
            .color(Color32::WHITE)
            .strong(),
    )
    .fill(fill)
    .stroke(Stroke::new(1.0_f32, stroke_col))
    .rounding(Rounding::same(metrics::RADIUS_WIDGET))
    .min_size(Vec2::new(ui.available_width(), 34.0));

    ui.add(btn).clicked()
}

/// Render a 4-option segmented pill/card selector for `LatencyProfile`.
pub fn latency_profile_selector(ui: &mut Ui, current: &mut LatencyProfile, lang: Language) {
    let t = lang.texts();
    let options = [
        (LatencyProfile::UltraLow, t.profile_ultra_low, "5ms Opus • 10ms"),
        (LatencyProfile::Balanced, t.profile_balanced, "5ms+FEC • 20ms"),
        (LatencyProfile::Stable, t.profile_stable, "10ms+FEC • 40ms"),
        (LatencyProfile::LosslessPcm, t.profile_lossless, "5ms PCM16 • 15ms"),
    ];

    ui.columns(4, |cols| {
        for (idx, (profile, title, spec)) in options.iter().enumerate() {
            let selected = *current == *profile;
            let fill = if selected {
                palette::ACCENT_MINT_GLOW
            } else {
                palette::BG_TRENCH
            };
            let border = if selected {
                palette::ACCENT_MINT
            } else {
                palette::BORDER_SUBTLE
            };
            let title_col = if selected {
                palette::ACCENT_MINT
            } else {
                palette::TEXT_PRIMARY
            };
            let stroke_w: f32 = if selected { 1.5 } else { 1.0 };

            let resp = egui::Frame::none()
                .fill(fill)
                .stroke(Stroke::new(stroke_w, border))
                .rounding(Rounding::same(metrics::RADIUS_WIDGET))
                .inner_margin(egui::Margin::symmetric(6.0, 5.0))
                .show(&mut cols[idx], |ui| {
                    ui.set_min_width(ui.available_width());
                    ui.vertical_centered(|ui| {
                        ui.spacing_mut().item_spacing.y = 1.0;
                        ui.label(RichText::new(*title).size(11.5).color(title_col).strong());
                        ui.label(
                            RichText::new(*spec)
                                .size(9.5)
                                .family(FontFamily::Monospace)
                                .color(palette::TEXT_MUTED),
                        );
                    });
                })
                .response
                .interact(Sense::click());

            if resp.clicked() {
                *current = *profile;
            }
            if resp.hovered() {
                cols[idx]
                    .ctx()
                    .set_cursor_icon(egui::CursorIcon::PointingHand);
            }
        }
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
}

