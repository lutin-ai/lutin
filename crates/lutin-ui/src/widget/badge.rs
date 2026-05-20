//! Themed badge widget — small inline status pill / tag. Tones map to
//! [`crate::theme::components::BadgeColors`] (ok, warn, bad, neutral).
//! Supports a filled style (subtle bg + thin border) and an outline-only
//! style (transparent bg).

use egui::{FontId, Response, Sense, Ui, Widget};

use crate::theme::components::BadgeColors;
use crate::theme::{Theme, theme};

const TEXT_SIZE: f32 = 12.0;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tone {
    Ok,
    Warn,
    Bad,
    Neutral,
}

fn tone_colors(t: &Theme, tone: Tone) -> &BadgeColors {
    match tone {
        Tone::Ok => &t.badge.ok,
        Tone::Warn => &t.badge.warn,
        Tone::Bad => &t.badge.bad,
        Tone::Neutral => &t.badge.neutral,
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum BadgeStyle {
    #[default]
    Filled,
    Outline,
}

pub struct Badge {
    text: egui::WidgetText,
    tone: Tone,
    style: BadgeStyle,
}

impl Badge {
    pub fn new(text: impl Into<egui::WidgetText>) -> Self {
        Self {
            text: text.into(),
            tone: Tone::Neutral,
            style: BadgeStyle::Filled,
        }
    }

    pub fn tone(mut self, tone: Tone) -> Self {
        self.tone = tone;
        self
    }

    /// Visual style: filled (subtle bg + border) or outline-only (transparent bg).
    pub fn style(mut self, style: BadgeStyle) -> Self {
        self.style = style;
        self
    }
}

impl Widget for Badge {
    fn ui(self, ui: &mut Ui) -> Response {
        let t = theme();
        let colors = tone_colors(&t, self.tone);
        let rounding = t.radii.full;
        let padding = egui::vec2(t.spacing.sm, t.spacing.xs);

        let font = FontId::proportional(TEXT_SIZE);
        let total_extra = padding + padding;
        let wrap_width = ui.available_width() - total_extra.x;
        let galley = self
            .text
            .into_galley(ui, Some(egui::TextWrapMode::Extend), wrap_width, font);

        let desired_size = total_extra + galley.size();
        let (rect, response) = ui.allocate_at_least(desired_size, Sense::hover());

        if ui.is_rect_visible(rect) {
            let bg = match self.style {
                BadgeStyle::Outline => egui::Color32::TRANSPARENT,
                BadgeStyle::Filled => colors.bg,
            };
            let stroke = egui::Stroke::new(1.0, colors.border);

            ui.painter()
                .rect(rect, rounding, bg, stroke, egui::StrokeKind::Inside);

            let content_rect = rect.shrink2(padding);
            let text_pos = egui::pos2(
                content_rect.min.x,
                content_rect.center().y - galley.size().y / 2.0,
            );
            ui.painter().galley(text_pos, galley, colors.text);
        }

        response
    }
}

pub fn ok(text: impl Into<egui::WidgetText>) -> Badge {
    Badge::new(text).tone(Tone::Ok)
}

pub fn warn(text: impl Into<egui::WidgetText>) -> Badge {
    Badge::new(text).tone(Tone::Warn)
}

pub fn bad(text: impl Into<egui::WidgetText>) -> Badge {
    Badge::new(text).tone(Tone::Bad)
}

pub fn neutral(text: impl Into<egui::WidgetText>) -> Badge {
    Badge::new(text).tone(Tone::Neutral)
}
