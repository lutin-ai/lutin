//! Themed text input. Caller owns the `String` buffer.
//!
//! Builder borrows `&mut Ui`, returns the inner `TextEdit`'s [`egui::Response`]
//! so callers can react to focus/edit/submit. Supports placeholder, password
//! masking, multiline, and explicit width.

use egui::{Response, Ui, Widget};

use crate::theme::theme;

const FONT_SIZE: f32 = 14.0;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum InputMode {
    #[default]
    SingleLine,
    Multiline,
    Password,
}

pub struct TextInput<'a> {
    text: &'a mut String,
    hint: &'a str,
    mode: InputMode,
    desired_width: Option<f32>,
}

impl<'a> TextInput<'a> {
    pub fn new(text: &'a mut String) -> Self {
        Self {
            text,
            hint: "",
            mode: InputMode::SingleLine,
            desired_width: None,
        }
    }

    pub fn hint(mut self, hint: &'a str) -> Self {
        self.hint = hint;
        self
    }

    pub fn mode(mut self, mode: InputMode) -> Self {
        self.mode = mode;
        self
    }

    pub fn desired_width(mut self, width: f32) -> Self {
        self.desired_width = Some(width);
        self
    }
}

impl Widget for TextInput<'_> {
    fn ui(self, ui: &mut Ui) -> Response {
        let t = theme();
        let padding = egui::vec2(t.spacing.md, t.spacing.sm);
        let rounding = t.radii.md;
        let width = self.desired_width.unwrap_or(ui.available_width());

        let id = ui.auto_id_with("lutin_text_input");
        let has_focus = ui.memory(|m| m.has_focus(id));

        let (bg, border_color) = if has_focus {
            (t.input.bg_focused, t.input.border_focused)
        } else {
            (t.input.bg, t.input.border)
        };

        let font = egui::FontId::new(FONT_SIZE, egui::FontFamily::Proportional);
        let hint = egui::RichText::new(self.hint)
            .color(t.input.placeholder)
            .size(FONT_SIZE);

        egui::Frame::new()
            .fill(bg)
            .stroke(egui::Stroke::new(1.0, border_color))
            .corner_radius(rounding)
            .inner_margin(padding)
            .show(ui, |ui| {
                ui.set_width(width - padding.x * 2.0 - 2.0);

                let mut edit = if matches!(self.mode, InputMode::Multiline) {
                    egui::TextEdit::multiline(self.text)
                } else {
                    egui::TextEdit::singleline(self.text)
                }
                .id(id)
                .hint_text(hint)
                .text_color(t.input.text)
                .font(font)
                .frame(egui::Frame::NONE)
                .desired_width(f32::INFINITY);

                if matches!(self.mode, InputMode::Password) {
                    edit = edit.password(true);
                }

                ui.add(edit)
            })
            .inner
    }
}
