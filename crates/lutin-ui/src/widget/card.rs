//! Themed card container. Clickable / hoverable content unit for list rows,
//! dashboard tiles, agent / file entries. Maps to
//! [`crate::theme::components::CardTheme`]. Distinct from [`super::panel`] —
//! panel is a layout container, card wraps a single content unit.

use egui::{Color32, InnerResponse, Sense, Shape, Ui};

use crate::theme::theme;

/// Interaction/selection state of a [`Card`]. `Selected` implies clickability —
/// a non-interactive card cannot be selected.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum CardState {
    #[default]
    Default,
    Clickable,
    Selected,
}

impl CardState {
    fn is_clickable(self) -> bool {
        matches!(self, CardState::Clickable | CardState::Selected)
    }

    fn is_selected(self) -> bool {
        matches!(self, CardState::Selected)
    }
}

#[derive(Clone, Copy, Debug, Default)]
pub struct Card {
    state: CardState,
    padding: Option<f32>,
    fill: Option<Color32>,
}

impl Card {
    pub fn new() -> Self {
        Self::default()
    }

    /// Set state explicitly.
    pub fn state(mut self, state: CardState) -> Self {
        self.state = state;
        self
    }

    /// Enable hover/click affordance (sense clicks, hover-styled). Promotes the
    /// state from `Default` to `Clickable`; leaves `Selected` untouched.
    pub fn clickable(mut self) -> Self {
        if matches!(self.state, CardState::Default) {
            self.state = CardState::Clickable;
        }
        self
    }

    /// Render with accent border + raised fill. Selection requires the card to
    /// be clickable; `selected(true)` upgrades a `Default` card to `Selected`.
    pub fn selected(mut self, selected: bool) -> Self {
        self.state = match (selected, self.state) {
            (true, _) => CardState::Selected,
            (false, CardState::Selected) => CardState::Clickable,
            (false, other) => other,
        };
        self
    }

    /// Override symmetric inner padding.
    pub fn padding(mut self, p: f32) -> Self {
        self.padding = Some(p);
        self
    }

    /// Override base fill colour.
    pub fn fill(mut self, c: Color32) -> Self {
        self.fill = Some(c);
        self
    }

    pub fn show<R>(
        self,
        ui: &mut Ui,
        body: impl FnOnce(&mut Ui) -> R,
    ) -> InnerResponse<R> {
        let t = theme();
        let pad = self.padding.unwrap_or(t.spacing.md);
        let radius = t.radii.md;
        let interactive = self.state.is_clickable();
        let selected = self.state.is_selected();
        let sense = if interactive {
            Sense::click()
        } else {
            Sense::hover()
        };

        let outer_min = ui.cursor().min;
        let max_avail = ui.available_rect_before_wrap();
        let inner_max = egui::Rect::from_min_max(
            outer_min + egui::vec2(pad, pad),
            egui::pos2(max_avail.max.x - pad, max_avail.max.y - pad),
        );

        // Reserve a shape slot to back-fill once we know the content rect.
        let bg_idx = ui.painter().add(Shape::Noop);

        let mut child = ui.new_child(
            egui::UiBuilder::new()
                .max_rect(inner_max)
                .layout(*ui.layout()),
        );
        let inner = body(&mut child);
        let content_rect = child.min_rect();

        let outer_rect = egui::Rect::from_min_max(
            outer_min,
            egui::pos2(content_rect.max.x + pad, content_rect.max.y + pad),
        );

        let response = ui.interact(outer_rect, ui.next_auto_id(), sense);
        ui.skip_ahead_auto_ids(1);
        ui.advance_cursor_after_rect(outer_rect);

        if ui.is_rect_visible(outer_rect) {
            let hovered = interactive && response.hovered();

            let bg = if let Some(c) = self.fill {
                c
            } else if selected {
                t.surface.elevated
            } else if hovered {
                t.card.bg_hover
            } else {
                t.card.bg
            };

            let border_color = if selected {
                t.accent.default
            } else if hovered {
                t.card.border_hover
            } else {
                t.card.border
            };

            let stroke_width = if selected { 1.5 } else { 1.0 };

            let shape = egui::epaint::RectShape::new(
                outer_rect,
                radius,
                bg,
                egui::Stroke::new(stroke_width, border_color),
                egui::StrokeKind::Inside,
            );
            ui.painter().set(bg_idx, shape);
        }

        InnerResponse::new(inner, response)
    }
}
