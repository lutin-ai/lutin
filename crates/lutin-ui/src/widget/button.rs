//! Themed button widget. Variants map to [`crate::theme::components::ButtonTheme`]
//! palettes (primary, secondary, ghost, danger). Caller owns state; widget
//! borrows `&mut Ui` and returns `egui::Response`.

use egui::{Response, Sense, Ui, Vec2, Widget};

use crate::theme::components::ButtonColors;
use crate::theme::{Theme, theme};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ButtonAlign {
    Left,
    #[default]
    Center,
    Right,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Variant {
    Primary,
    Secondary,
    Ghost,
    Danger,
}

fn variant_colors(t: &Theme, variant: Variant) -> &ButtonColors {
    match variant {
        Variant::Primary => &t.button.primary,
        Variant::Secondary => &t.button.secondary,
        Variant::Ghost => &t.button.ghost,
        Variant::Danger => &t.button.danger,
    }
}

pub struct ThemedButton {
    text: egui::WidgetText,
    variant: Variant,
    icon: Option<egui::WidgetText>,
    min_size: Vec2,
    small: bool,
    enabled: bool,
    full_width: bool,
    align: ButtonAlign,
}

impl ThemedButton {
    pub fn new(text: impl Into<egui::WidgetText>) -> Self {
        Self {
            text: text.into(),
            variant: Variant::Primary,
            icon: None,
            min_size: Vec2::ZERO,
            small: false,
            enabled: true,
            full_width: false,
            align: ButtonAlign::default(),
        }
    }

    pub fn variant(mut self, variant: Variant) -> Self {
        self.variant = variant;
        self
    }

    pub fn icon(mut self, icon: impl Into<egui::WidgetText>) -> Self {
        self.icon = Some(icon.into());
        self
    }

    pub fn small(mut self) -> Self {
        self.small = true;
        self
    }

    pub fn min_size(mut self, size: Vec2) -> Self {
        self.min_size = size;
        self
    }

    pub fn disabled(mut self) -> Self {
        self.enabled = false;
        self
    }

    pub fn full_width(mut self) -> Self {
        self.full_width = true;
        self
    }

    pub fn align(mut self, align: ButtonAlign) -> Self {
        self.align = align;
        self
    }
}

impl Widget for ThemedButton {
    fn ui(self, ui: &mut Ui) -> Response {
        let t = theme();
        let colors = variant_colors(&t, self.variant);
        let rounding = t.radii.sm;

        let padding = if self.small {
            egui::vec2(t.spacing.md, t.spacing.sm)
        } else {
            egui::vec2(t.spacing.lg, t.spacing.md)
        };

        let total_extra = padding + padding;
        let wrap_width = ui.available_width() - total_extra.x;
        let galley = self
            .text
            .into_galley(ui, None, wrap_width, egui::TextStyle::Button);

        let icon_galley = self
            .icon
            .map(|i| i.into_galley(ui, None, wrap_width, egui::TextStyle::Button));
        let icon_extra = icon_galley
            .as_ref()
            .map(|g| g.size().x + t.spacing.sm)
            .unwrap_or(0.0);

        let mut desired_size = total_extra + galley.size();
        desired_size.x += icon_extra;
        if self.full_width {
            desired_size.x = ui.available_width();
        }
        desired_size.x = desired_size.x.max(self.min_size.x);
        desired_size.y = desired_size.y.max(self.min_size.y);

        let content_height = desired_size.y;
        let (rect, response) = ui.allocate_at_least(desired_size, Sense::click());
        let rect = rect.with_max_y(rect.min.y + content_height);

        if ui.is_rect_visible(rect) {
            let (bg, text_color, border) = if !self.enabled {
                (
                    colors.bg_disabled,
                    colors.text_disabled,
                    egui::Stroke::NONE,
                )
            } else if response.is_pointer_button_down_on() {
                (
                    colors.bg_active,
                    colors.text_hover,
                    egui::Stroke::new(1.0, colors.border_hover),
                )
            } else if response.hovered() {
                (
                    colors.bg_hover,
                    colors.text_hover,
                    egui::Stroke::new(1.0, colors.border_hover),
                )
            } else {
                (
                    colors.bg,
                    colors.text,
                    egui::Stroke::new(1.0, colors.border),
                )
            };

            ui.painter()
                .rect(rect, rounding, bg, border, egui::StrokeKind::Outside);

            let content_rect = rect.shrink2(padding);
            let content_width = galley.size().x + icon_extra;
            let mut x = match self.align {
                ButtonAlign::Left => content_rect.min.x,
                ButtonAlign::Center => {
                    content_rect.min.x + (content_rect.width() - content_width) / 2.0
                }
                ButtonAlign::Right => content_rect.max.x - content_width,
            };

            if let Some(icon_g) = icon_galley {
                let icon_pos = egui::pos2(x, content_rect.center().y - icon_g.size().y / 2.0);
                let w = icon_g.size().x;
                ui.painter().galley(icon_pos, icon_g, text_color);
                x += w + t.spacing.sm;
            }

            let text_pos = egui::pos2(x, content_rect.center().y - galley.size().y / 2.0);
            ui.painter().galley(text_pos, galley, text_color);
        }

        response
    }
}

pub fn primary(text: impl Into<egui::WidgetText>) -> ThemedButton {
    ThemedButton::new(text).variant(Variant::Primary)
}

pub fn secondary(text: impl Into<egui::WidgetText>) -> ThemedButton {
    ThemedButton::new(text).variant(Variant::Secondary)
}

pub fn ghost(text: impl Into<egui::WidgetText>) -> ThemedButton {
    ThemedButton::new(text).variant(Variant::Ghost)
}

pub fn danger(text: impl Into<egui::WidgetText>) -> ThemedButton {
    ThemedButton::new(text).variant(Variant::Danger)
}

/// Square ghost button containing only an icon glyph. Sized to `spacing.xxl`.
pub fn icon_button(icon: impl Into<egui::WidgetText>) -> ThemedButton {
    let size = theme().spacing.xxl;
    ThemedButton::new(icon)
        .variant(Variant::Ghost)
        .min_size(Vec2::splat(size))
}
