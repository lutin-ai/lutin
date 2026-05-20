//! Themed table — sticky header, hairline row dividers, no zebra. Per
//! design doc 17 §Table.
//!
//! ```rust,ignore
//! use lutin_ui::widget::table::{Table, Align, Width};
//! Table::new()
//!     .column("#", Align::Right, 40.0)
//!     .column("Idea", Align::Left, Width::Flex)
//!     .column("Status", Align::Left, 120.0)
//!     .show(ui, rows.len(), |idx, row| {
//!         row.cell(|ui| { ui.label(format!("{}", idx + 1)); });
//!         row.cell(|ui| { ui.label(&rows[idx].idea); });
//!         row.cell(|ui| { ui.add(badge::ok("ready")); });
//!     });
//! ```

use egui::{Align as EguiAlign, FontId, Layout, Rect, Sense, Ui};

use crate::theme::theme;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Align {
    Left,
    Center,
    Right,
}

#[derive(Clone, Copy, Debug)]
pub enum Width {
    Fixed(f32),
    Flex,
}

impl From<f32> for Width {
    fn from(value: f32) -> Self {
        Width::Fixed(value)
    }
}

struct Column {
    name: String,
    align: Align,
    width: Width,
}

const ROW_HEIGHT: f32 = 34.0;
const HEADER_HEIGHT: f32 = 30.0;
const HEADER_TEXT: f32 = 10.5;
const CELL_PAD_X: f32 = 14.0;

pub struct Table {
    columns: Vec<Column>,
    row_height: f32,
}

impl Default for Table {
    fn default() -> Self {
        Self::new()
    }
}

impl Table {
    pub fn new() -> Self {
        Self {
            columns: Vec::new(),
            row_height: ROW_HEIGHT,
        }
    }

    pub fn column(
        mut self,
        name: impl Into<String>,
        align: Align,
        width: impl Into<Width>,
    ) -> Self {
        self.columns.push(Column {
            name: name.into(),
            align,
            width: width.into(),
        });
        self
    }

    pub fn row_height(mut self, h: f32) -> Self {
        self.row_height = h;
        self
    }

    pub fn show(
        self,
        ui: &mut Ui,
        num_rows: usize,
        mut row_fn: impl FnMut(usize, &mut RowBuilder<'_>),
    ) {
        let t = theme();

        egui::Frame::new()
            .fill(t.surface.void)
            .stroke(egui::Stroke::new(1.0, t.border.default))
            .corner_radius(t.radii.md)
            .inner_margin(0)
            .show(ui, |ui| {
                ui.set_width(ui.available_width());
                let widths = resolve_widths(&self.columns, ui.available_width());

                draw_header(ui, &self.columns, &widths);

                egui::ScrollArea::vertical()
                    .auto_shrink([false, true])
                    .show(ui, |ui| {
                        for idx in 0..num_rows {
                            let is_last = idx + 1 == num_rows;
                            draw_row(
                                ui,
                                idx,
                                &self.columns,
                                &widths,
                                self.row_height,
                                is_last,
                                &mut row_fn,
                            );
                        }
                    });
            });
    }
}

fn resolve_widths(columns: &[Column], available: f32) -> Vec<f32> {
    let fixed_total: f32 = columns
        .iter()
        .filter_map(|c| match c.width {
            Width::Fixed(w) => Some(w),
            Width::Flex => None,
        })
        .sum();
    let flex_count = columns
        .iter()
        .filter(|c| matches!(c.width, Width::Flex))
        .count();
    let flex_each = if flex_count == 0 {
        0.0
    } else {
        (available - fixed_total).max(0.0) / flex_count as f32
    };
    columns
        .iter()
        .map(|c| match c.width {
            Width::Fixed(w) => w,
            Width::Flex => flex_each,
        })
        .collect()
}

fn cell_layout(align: Align) -> Layout {
    match align {
        Align::Left => Layout::left_to_right(EguiAlign::Center),
        Align::Center => Layout::centered_and_justified(egui::Direction::LeftToRight),
        Align::Right => Layout::right_to_left(EguiAlign::Center),
    }
}

fn draw_header(ui: &mut Ui, columns: &[Column], widths: &[f32]) {
    let t = theme();
    let row_width: f32 = widths.iter().sum();
    let (rect, _) = ui.allocate_exact_size(
        egui::vec2(row_width.max(ui.available_width()), HEADER_HEIGHT),
        Sense::hover(),
    );
    let r = t.radii.md as u8;
    let header_corners = egui::CornerRadius {
        nw: r,
        ne: r,
        sw: 0,
        se: 0,
    };
    ui.painter()
        .rect_filled(rect, header_corners, t.surface.elevated);
    ui.painter().hline(
        rect.x_range(),
        rect.max.y,
        egui::Stroke::new(1.0, t.border.default),
    );

    let mut x = rect.min.x;
    for (col, &w) in columns.iter().zip(widths.iter()) {
        let cell_rect = Rect::from_min_size(egui::pos2(x, rect.min.y), egui::vec2(w, HEADER_HEIGHT));
        x += w;
        let inner = cell_rect.shrink2(egui::vec2(CELL_PAD_X, 0.0));
        let mut child = ui.new_child(
            egui::UiBuilder::new()
                .max_rect(inner)
                .layout(cell_layout(col.align)),
        );
        child.label(
            egui::RichText::new(col.name.to_uppercase())
                .font(FontId::new(HEADER_TEXT, t.fonts.text.clone()))
                .color(t.text.dim)
                .strong(),
        );
    }
}

fn draw_row(
    ui: &mut Ui,
    idx: usize,
    columns: &[Column],
    widths: &[f32],
    row_height: f32,
    is_last: bool,
    row_fn: &mut dyn FnMut(usize, &mut RowBuilder<'_>),
) {
    let t = theme();
    let row_width: f32 = widths.iter().sum();
    let (rect, response) = ui.allocate_exact_size(
        egui::vec2(row_width.max(ui.available_width()), row_height),
        Sense::hover(),
    );

    if response.hovered() {
        ui.painter().rect_filled(rect, 0.0, t.surface.elevated);
    }
    if !is_last {
        ui.painter().hline(
            rect.x_range(),
            rect.max.y,
            egui::Stroke::new(1.0, t.border.subtle),
        );
    }

    let mut builder = RowBuilder {
        ui,
        columns,
        widths,
        row_rect: rect,
        cell_idx: 0,
    };
    row_fn(idx, &mut builder);
}

pub struct RowBuilder<'a> {
    ui: &'a mut Ui,
    columns: &'a [Column],
    widths: &'a [f32],
    row_rect: Rect,
    cell_idx: usize,
}

impl<'a> RowBuilder<'a> {
    pub fn cell<R>(&mut self, content: impl FnOnce(&mut Ui) -> R) -> Option<R> {
        let idx = self.cell_idx;
        if idx >= self.columns.len() {
            return None;
        }
        let x_start: f32 = self.row_rect.min.x + self.widths[..idx].iter().sum::<f32>();
        let w = self.widths[idx];
        let align = self.columns[idx].align;
        self.cell_idx += 1;

        let cell_rect = Rect::from_min_size(
            egui::pos2(x_start, self.row_rect.min.y),
            egui::vec2(w, self.row_rect.height()),
        );
        let inner = cell_rect.shrink2(egui::vec2(CELL_PAD_X, 0.0));
        let mut child = self.ui.new_child(
            egui::UiBuilder::new()
                .max_rect(inner)
                .layout(cell_layout(align)),
        );
        Some(content(&mut child))
    }
}
