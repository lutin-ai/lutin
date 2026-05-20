//! Sharp-edged toggle switch — IDE-feel chunky checkbox-style switch.
//!
//! Caller owns `bool` state. Track + knob are rectangular with `theme.radii.default`
//! corners (sharp by default). Accent fill when on, raised surface when off.

use egui::{Color32, Rect, Response, Sense, Ui, Widget, lerp, vec2};

use crate::theme::theme;

/// Free-function entry point. `pub fn toggle(ui, on) -> Response`.
pub fn toggle(ui: &mut Ui, on: &mut bool) -> Response {
    Toggle::new(on).ui(ui)
}

/// Optional per-instance color overrides for [`Toggle`]. Any field left `None`
/// falls back to the active theme.
#[derive(Clone, Copy, Debug, Default)]
pub struct ToggleColors {
    pub on: Option<Color32>,
    pub off: Option<Color32>,
    pub knob: Option<Color32>,
}

#[must_use = "widgets do nothing until added to a Ui"]
pub struct Toggle<'a> {
    on: &'a mut bool,
    text: egui::WidgetText,
    width: f32,
    height: f32,
    colors: Option<ToggleColors>,
    animation_speed: f32,
}

impl<'a> Toggle<'a> {
    pub fn new(on: &'a mut bool) -> Self {
        Self {
            on,
            text: egui::WidgetText::default(),
            width: 36.0,
            height: 20.0,
            colors: None,
            animation_speed: 1.0,
        }
    }

    pub fn text(mut self, text: impl Into<egui::WidgetText>) -> Self {
        self.text = text.into();
        self
    }

    pub fn size(mut self, width: f32, height: f32) -> Self {
        self.width = width;
        self.height = height;
        self
    }

    pub fn colors(mut self, colors: ToggleColors) -> Self {
        self.colors = Some(colors);
        self
    }

    pub fn animation_speed(mut self, speed: f32) -> Self {
        self.animation_speed = speed;
        self
    }

    fn paint(&self, ui: &Ui, rect: Rect, t: f32) {
        let painter = ui.painter();
        let theme = theme();

        let overrides = self.colors.unwrap_or_default();
        let on_color = overrides.on.unwrap_or(theme.accent.default);
        let off_color = overrides.off.unwrap_or(theme.surface.raised);
        let knob_color = overrides.knob.unwrap_or(theme.text.bright);
        let border = theme.border.default;
        let radius = egui::CornerRadius::same(theme.radii.full as u8);

        let bg = Color32::from_rgba_unmultiplied(
            lerp((off_color.r() as f32)..=(on_color.r() as f32), t) as u8,
            lerp((off_color.g() as f32)..=(on_color.g() as f32), t) as u8,
            lerp((off_color.b() as f32)..=(on_color.b() as f32), t) as u8,
            255,
        );

        painter.rect_filled(rect, radius, bg);
        painter.rect_stroke(
            rect,
            radius,
            egui::Stroke::new(1.0, border),
            egui::StrokeKind::Inside,
        );

        // Knob: square chunk, ~70% of track height, inset by 2px, slides edge-to-edge.
        let inset = 2.0_f32;
        let knob_size = (rect.height() - inset * 2.0).max(2.0);
        let travel = (rect.width() - inset * 2.0 - knob_size).max(0.0);
        let knob_x = rect.left() + inset + travel * t;
        let knob_y = rect.top() + inset;
        let knob_rect = Rect::from_min_size(
            egui::pos2(knob_x, knob_y),
            egui::vec2(knob_size, knob_size),
        );
        painter.rect_filled(knob_rect, radius, knob_color);
    }
}

impl<'a> Widget for Toggle<'a> {
    fn ui(mut self, ui: &mut Ui) -> Response {
        let track_size = vec2(self.width, self.height);
        let label_text = self.text.text().to_owned();
        let text = std::mem::take(&mut self.text);
        let label_galley = if text.is_empty() {
            None
        } else {
            Some(text.into_galley(
                ui,
                Some(egui::TextWrapMode::Extend),
                f32::INFINITY,
                egui::TextStyle::Button,
            ))
        };
        let spacing = ui.spacing().item_spacing.x;
        let total_size = match &label_galley {
            Some(galley) => vec2(
                track_size.x + spacing + galley.size().x,
                track_size.y.max(galley.size().y),
            ),
            None => track_size,
        };

        let (rect, mut response) = ui.allocate_exact_size(total_size, Sense::click());

        if response.clicked() {
            *self.on = !*self.on;
            response.mark_changed();
        }

        let target = if *self.on { 1.0 } else { 0.0 };
        let duration = if self.animation_speed > 0.0 {
            0.12 / self.animation_speed
        } else {
            0.0
        };
        let t = ui
            .ctx()
            .animate_value_with_time(response.id.with("toggle"), target, duration)
            .clamp(0.0, 1.0);

        let track_rect = Rect::from_min_size(
            egui::pos2(rect.left(), rect.center().y - track_size.y * 0.5),
            track_size,
        );

        if ui.is_rect_visible(track_rect) {
            self.paint(ui, track_rect, t);
        }

        if let Some(galley) = label_galley {
            let theme = theme();
            let text_pos = egui::pos2(
                track_rect.right() + spacing,
                rect.center().y - galley.size().y * 0.5,
            );
            ui.painter().galley(text_pos, galley, theme.text.bright);
        }

        response.widget_info(|| {
            egui::WidgetInfo::selected(
                egui::WidgetType::Checkbox,
                ui.is_enabled(),
                *self.on,
                &label_text,
            )
        });

        response
    }
}
