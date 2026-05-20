//! Themed 1px divider. Horizontal (default, fills width) or vertical
//! (fills height). Optional inset trims both ends; optional centred label
//! splits the line with text in the middle.

use egui::{Color32, Response, Sense, Stroke, Ui, Widget, WidgetText};

use crate::theme::theme;

/// Orientation of a [`Divider`] line.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Direction {
    #[default]
    Horizontal,
    Vertical,
}

const THICKNESS: f32 = 1.0;
const LABEL_FONT: f32 = 11.0;
const LABEL_GAP: f32 = 6.0;

/// Horizontal 1px divider spanning available width.
pub fn horizontal(ui: &mut Ui) -> Response {
    Divider::new().ui(ui)
}

/// Vertical 1px divider spanning available height.
pub fn vertical(ui: &mut Ui) -> Response {
    Divider::new().vertical().ui(ui)
}

#[derive(Default)]
pub struct Divider {
    direction: Direction,
    inset: f32,
    color: Option<Color32>,
    label: Option<WidgetText>,
}

impl Divider {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn direction(mut self, direction: Direction) -> Self {
        self.direction = direction;
        self
    }

    /// Convenience: equivalent to `.direction(Direction::Vertical)`.
    pub fn vertical(self) -> Self {
        self.direction(Direction::Vertical)
    }

    /// Trim `px` from each end of the line.
    pub fn inset(mut self, px: f32) -> Self {
        self.inset = px;
        self
    }

    pub fn color(mut self, c: Color32) -> Self {
        self.color = Some(c);
        self
    }

    /// Centred label (horizontal dividers only; ignored for vertical).
    pub fn label(mut self, text: impl Into<WidgetText>) -> Self {
        self.label = Some(text.into());
        self
    }
}

impl Widget for Divider {
    fn ui(self, ui: &mut Ui) -> Response {
        let t = theme();
        let line_color = self.color.unwrap_or(t.border.default);
        let stroke = Stroke::new(THICKNESS, line_color);

        match self.direction {
            Direction::Horizontal => {
            let avail = ui.available_width();
            let v_pad = t.spacing.xs;

            if let Some(label) = self.label {
                let galley = label.into_galley(
                    ui,
                    Some(egui::TextWrapMode::Extend),
                    f32::INFINITY,
                    egui::FontId::proportional(LABEL_FONT),
                );
                let text_size = galley.size();
                let height = text_size.y.max(THICKNESS) + v_pad * 2.0;
                let (rect, response) =
                    ui.allocate_exact_size(egui::vec2(avail, height), Sense::hover());

                if ui.is_rect_visible(rect) {
                    let cy = rect.center().y;
                    let left_start = rect.left() + self.inset;
                    let right_end = rect.right() - self.inset;
                    let text_x_min = rect.center().x - text_size.x / 2.0;
                    let text_x_max = rect.center().x + text_size.x / 2.0;
                    let line_left_end = (text_x_min - LABEL_GAP).max(left_start);
                    let line_right_start = (text_x_max + LABEL_GAP).min(right_end);

                    let painter = ui.painter();
                    if line_left_end > left_start {
                        painter.line_segment(
                            [egui::pos2(left_start, cy), egui::pos2(line_left_end, cy)],
                            stroke,
                        );
                    }
                    if right_end > line_right_start {
                        painter.line_segment(
                            [egui::pos2(line_right_start, cy), egui::pos2(right_end, cy)],
                            stroke,
                        );
                    }
                    let text_pos = egui::pos2(text_x_min, cy - text_size.y / 2.0);
                    painter.galley(text_pos, galley, t.text.dim);
                }

                response
            } else {
                let height = THICKNESS + v_pad * 2.0;
                let (rect, response) =
                    ui.allocate_exact_size(egui::vec2(avail, height), Sense::hover());
                if ui.is_rect_visible(rect) {
                    let cy = rect.center().y;
                    ui.painter().line_segment(
                        [
                            egui::pos2(rect.left() + self.inset, cy),
                            egui::pos2(rect.right() - self.inset, cy),
                        ],
                        stroke,
                    );
                }
                response
            }
            }
            Direction::Vertical => {
            let avail = ui.available_height();
            let h_pad = t.spacing.xs;
            let width = THICKNESS + h_pad * 2.0;
            let (rect, response) =
                ui.allocate_exact_size(egui::vec2(width, avail), Sense::hover());
            if ui.is_rect_visible(rect) {
                let cx = rect.center().x;
                ui.painter().line_segment(
                    [
                        egui::pos2(cx, rect.top() + self.inset),
                        egui::pos2(cx, rect.bottom() - self.inset),
                    ],
                    stroke,
                );
            }
            response
            }
        }
    }
}
