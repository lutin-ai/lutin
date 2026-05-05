//! Terminal-aesthetic primitives composed by the chat widgets and other
//! mono/IDE-feel surfaces. Status dots, tag pills, section headers, full-bleed
//! turn separator, and the standard `card_terminal` frame.

use egui::{Color32, Sense, Stroke, Ui};

use crate::theme::theme;

// ---------------------------------------------------------------------------
// Status dot
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum DotStatus {
    Idle,
    Active,
    Done,
    Error,
}

impl DotStatus {
    fn color(self) -> Color32 {
        let t = theme();
        match self {
            DotStatus::Idle => t.text.dim,
            DotStatus::Active => t.status.orange.solid,
            DotStatus::Done => t.status.success.solid,
            DotStatus::Error => t.status.error.solid,
        }
    }
}

const DOT_SIZE: f32 = 8.0;

pub fn status_dot(ui: &mut Ui, kind: DotStatus) {
    status_dot_sized(ui, kind, DOT_SIZE);
}

pub fn status_dot_sized(ui: &mut Ui, kind: DotStatus, size: f32) {
    let (rect, _) = ui.allocate_exact_size(egui::vec2(size, size), Sense::hover());
    let pulse = if matches!(kind, DotStatus::Active) {
        let phase = ui.ctx().input(|i| i.time) * 2.0;
        ((phase.sin() + 1.0) / 2.0 * 0.5 + 0.5) as f32
    } else {
        1.0
    };
    ui.painter()
        .circle_filled(rect.center(), size / 2.0, kind.color().gamma_multiply(pulse));
    if matches!(kind, DotStatus::Active) {
        ui.ctx().request_repaint();
    }
}

// ---------------------------------------------------------------------------
// Tag pill
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum TagKind {
    Read,
    Write,
    Agent,
    Neutral,
    Error,
    Warn,
    Info,
}

impl TagKind {
    fn colors(self) -> (Color32, Color32) {
        let t = theme();
        match self {
            TagKind::Read => (t.status.success.dim, t.status.success.solid),
            TagKind::Write => (t.status.orange.dim, t.status.orange.solid),
            TagKind::Agent => (t.status.purple.dim, t.status.purple.solid),
            TagKind::Neutral => (t.status.neutral.dim, t.text.dim),
            TagKind::Error => (t.status.error.dim, t.status.error.solid),
            TagKind::Warn => (t.status.warning.dim, t.status.warning.solid),
            TagKind::Info => (t.status.info.dim, t.status.info.solid),
        }
    }
}

pub fn tag_pill(ui: &mut Ui, label: &str, kind: TagKind) {
    let t = theme();
    let (bg, fg) = kind.colors();
    tag_pill_colored(ui, label, bg, fg, &t.fonts.code);
}

/// Filled / inverse-video tag — solid family color background + near-black
/// foreground, sharp corners. Used by the tool-call row.
pub fn tag_pill_filled(ui: &mut Ui, label: &str, kind: TagKind) {
    let t = theme();
    let bg = match kind {
        TagKind::Read => t.status.success.solid,
        TagKind::Write => t.status.warning.solid,
        TagKind::Agent => t.status.purple.solid,
        TagKind::Neutral => t.text.dim,
        TagKind::Error => t.status.error.solid,
        TagKind::Warn => t.accent.default,
        TagKind::Info => t.status.info.solid,
    };
    let frame = egui::Frame::new()
        .fill(bg)
        .corner_radius(0.0)
        .inner_margin(egui::Margin::symmetric(8, 3));
    frame.show(ui, |ui| {
        ui.label(
            egui::RichText::new(label)
                .size(10.5)
                .color(t.surface.abyss)
                .strong()
                .family(t.fonts.code.clone()),
        );
    });
}

pub fn tag_pill_colored(
    ui: &mut Ui,
    label: &str,
    bg: Color32,
    fg: Color32,
    font: &egui::FontFamily,
) {
    let frame = egui::Frame::new()
        .fill(bg)
        .corner_radius(3.0)
        .inner_margin(egui::vec2(6.0, 1.5));
    frame.show(ui, |ui| {
        ui.label(
            egui::RichText::new(label)
                .size(10.0)
                .color(fg)
                .strong()
                .family(font.clone()),
        );
    });
}

// ---------------------------------------------------------------------------
// Section header — `<label>` left, optional right meta. All mono.
// ---------------------------------------------------------------------------

pub fn section_header(ui: &mut Ui, label: &str) {
    section_header_meta(ui, label, None, None);
}

pub fn section_header_meta(
    ui: &mut Ui,
    label: &str,
    meta: Option<&str>,
    color: Option<Color32>,
) {
    let t = theme();
    let label_color = color.unwrap_or(t.text.dim);
    ui.horizontal(|ui| {
        ui.label(
            egui::RichText::new(label)
                .size(12.0)
                .color(label_color)
                .family(t.fonts.code.clone()),
        );
        if let Some(m) = meta {
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.label(
                    egui::RichText::new(m)
                        .size(11.0)
                        .color(t.text.dim)
                        .family(t.fonts.code.clone()),
                );
            });
        }
    });
    ui.add_space(t.spacing.xs);
}

/// Accent-orange section header — diamond glyph + lowercase label.
pub fn section_header_accent(ui: &mut Ui, label: &str) {
    let t = theme();
    let accent = t.status.orange.solid;
    ui.horizontal(|ui| {
        ui.label(
            egui::RichText::new("\u{25C6}")
                .size(11.0)
                .color(accent)
                .family(t.fonts.code.clone()),
        );
        ui.add_space(t.spacing.xs);
        ui.label(
            egui::RichText::new(label)
                .size(12.0)
                .color(accent)
                .family(t.fonts.code.clone()),
        );
    });
    ui.add_space(t.spacing.xs);
}

// ---------------------------------------------------------------------------
// Turn separator — full-bleed hairline that escapes the parent panel inset.
// ---------------------------------------------------------------------------

/// Default panel inset used by most surfaces. Override via [`turn_separator_pad`]
/// when the parent uses a different inner margin.
pub const TURN_SEPARATOR_PAD: f32 = 24.0;

pub fn turn_separator(ui: &mut Ui) {
    turn_separator_pad(ui, TURN_SEPARATOR_PAD);
}

pub fn turn_separator_pad(ui: &mut Ui, pad: f32) {
    let t = theme();
    let avail = ui.available_rect_before_wrap();
    let y = avail.min.y;
    let clip = ui.clip_rect();
    let extended = egui::Rect::from_min_max(
        egui::pos2(clip.min.x - pad, clip.min.y - 100.0),
        egui::pos2(clip.max.x + pad, clip.max.y + 100.0),
    );
    let painter = ui.painter().clone().with_clip_rect(extended);
    painter.hline(
        (avail.min.x - pad)..=(avail.max.x + pad),
        y,
        Stroke::new(1.0, t.border.default),
    );
    ui.add_space(t.spacing.md);
}

// ---------------------------------------------------------------------------
// Card terminal — `surface.deep` + `border.subtle` + `radii.md`. Shared by the
// tool-call row and other mono cards.
// ---------------------------------------------------------------------------

pub fn card_terminal() -> egui::Frame {
    let t = theme();
    egui::Frame::new()
        .fill(t.surface.deep)
        .stroke(Stroke::new(1.0, t.border.subtle))
        .corner_radius(t.radii.md)
        .inner_margin(egui::vec2(t.spacing.md, t.spacing.sm))
}
