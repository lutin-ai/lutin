//! Themed modal dialog — wraps [`egui::Modal`] with our surfaces, optional
//! title, and keyboard/backdrop dismissal. Per design doc 17: only the
//! primary action gets the accent focus ring; that styling is the consumer's
//! job (use [`crate::widget::button::primary`]).

use egui::{Context, Id, Ui};

use crate::theme::theme;

pub struct Modal {
    id: Id,
    title: Option<egui::WidgetText>,
}

impl Modal {
    pub fn new(id: Id) -> Self {
        Self { id, title: None }
    }

    pub fn title(mut self, title: impl Into<egui::WidgetText>) -> Self {
        self.title = Some(title.into());
        self
    }

    /// Show the modal and return its [`Response`](ModalResponse). Body runs
    /// inside the themed card.
    pub fn show<R>(
        self,
        ctx: &Context,
        body: impl FnOnce(&mut Ui) -> R,
    ) -> ModalResponse<R> {
        let t = theme();
        let pad = t.spacing.lg as i8;

        let frame = egui::Frame::new()
            .fill(t.surface.deep)
            .stroke(egui::Stroke::new(1.0, t.border.default))
            .corner_radius(t.radii.md)
            .inner_margin(0);

        let resp = egui::Modal::new(self.id)
            .backdrop_color(egui::Color32::from_black_alpha(140))
            .frame(frame)
            .show(ctx, |ui| {
                ui.set_min_width(360.0);
                if let Some(title) = self.title {
                    let r = t.radii.md as u8;
                    egui::Frame::new()
                        .fill(t.panel.header_bg)
                        .corner_radius(egui::CornerRadius {
                            nw: r,
                            ne: r,
                            sw: 0,
                            se: 0,
                        })
                        .inner_margin(egui::Margin::symmetric(pad, t.spacing.sm as i8))
                        .show(ui, |ui| {
                            ui.set_width(ui.available_width());
                            ui.label(
                                egui::RichText::new(title.text())
                                    .size(13.0)
                                    .color(t.text.bright),
                            );
                        });
                }
                egui::Frame::new()
                    .inner_margin(pad)
                    .show(ui, |ui| body(ui))
                    .inner
            });

        let should_close = resp.should_close();
        ModalResponse {
            inner: resp.inner,
            should_close,
        }
    }
}

pub struct ModalResponse<T> {
    pub inner: T,
    /// True if the user dismissed the modal (Esc, backdrop click, or
    /// `ui.close()`). Owner should set its open-flag to false.
    pub should_close: bool,
}
