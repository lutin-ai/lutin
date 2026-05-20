//! Tool-call card.
//!
//! Single-row collapsed header with a status tag pill, glyph, summary string,
//! optional meta + spinner, and a chevron. Expands to show args + output (with
//! custom body renderers for `file_edit` / `file_write`). When `show_approval`
//! is set, an Approve / Deny row is rendered after the card.
//!
//! The widget takes a [`ToolCallView`] — caller maps their domain types in.
//! No engine types touched.

use egui::Ui;
use serde_json::Value;

use crate::theme::theme;
use crate::widget::{button, divider, terminal};

// ---------------------------------------------------------------------------
// Public view types
// ---------------------------------------------------------------------------

/// Lifecycle state of a tool call. Drives the status tag, glyph, and meta text.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum ToolStatus {
    /// Provider is streaming arguments.
    Streaming,
    /// Awaiting user approval (set `show_approval` on `show` to render
    /// Approve/Deny buttons).
    Pending,
    /// Approved and currently executing.
    Executing,
    /// Completed (use `is_error` on the view to mark a tool-reported error).
    Done,
    /// User denied the call.
    Denied,
    /// Host/runtime failure (set `error_text` on the view for the banner).
    Failed,
}

/// Args payload — fully parsed JSON, partial bytes from a streaming call, or
/// none.
pub enum ToolArgs<'a> {
    Json(&'a Value),
    Partial(&'a str),
    None,
}

/// All inputs the tool-call card needs. Caller builds this from their own
/// domain types.
pub struct ToolCallView<'a> {
    pub call_id: &'a str,
    pub name: &'a str,
    pub status: ToolStatus,
    pub args: ToolArgs<'a>,
    pub output: Option<&'a str>,
    /// True when `output` represents a tool-reported error (renders "Error" in
    /// the result section instead of "Result").
    pub is_error: bool,
    /// Banner text for `Failed` state (host/runtime failure).
    pub error_text: Option<&'a str>,
    /// Image URIs to render under the result block. Use `bytes://<key>` after
    /// installing the bytes via `ctx.include_bytes`, or any URI egui's image
    /// loader recognises.
    pub images: &'a [String],
}

/// Action emitted from the Approve / Deny buttons. The caller threads these
/// back into their own state machine — the widget does nothing on its own.
#[derive(Debug)]
pub enum ToolCallAction {
    None,
    Approve(String),
    Deny(String),
}

// ---------------------------------------------------------------------------
// Render
// ---------------------------------------------------------------------------

/// Tool names that render expanded by default. Override per-card by toggling
/// the chevron.
const DEFAULT_EXPANDED_TOOLS: &[&str] = &["new_chat"];

fn default_expanded(name: &str) -> bool {
    DEFAULT_EXPANDED_TOOLS.contains(&name)
}

pub fn show(ui: &mut Ui, view: &ToolCallView<'_>, show_approval: bool) -> ToolCallAction {
    let t = theme();
    let mut action = ToolCallAction::None;

    let id = ui.auto_id_with(("tc_expanded", view.call_id));
    let mut expanded =
        ui.memory(|mem| mem.data.get_temp::<bool>(id).unwrap_or_else(|| default_expanded(view.name)));

    let display_status = display_status(view);
    let (tag_label, tag_kind) = tag_for(view.name);

    let row_resp = terminal::card_terminal().show(ui, |ui| {
        let w = ui.available_width();
        ui.set_width(w);
        ui.set_max_width(w);
        ui.horizontal(|ui| {
            terminal::tag_pill_filled(ui, tag_label, tag_kind);
            ui.add_space(t.spacing.sm);

            let (glyph, glyph_color) = status_glyph(display_status);
            ui.label(
                egui::RichText::new(glyph)
                    .family(t.fonts.code.clone())
                    .size(13.0)
                    .color(glyph_color),
            );
            ui.add_space(t.spacing.sm);

            let target = match &view.args {
                ToolArgs::Json(args) => super::tool_summary::summary(view.name, args)
                    .unwrap_or_else(|| view.name.to_string()),
                ToolArgs::Partial(_) | ToolArgs::None => view.name.to_string(),
            };
            ui.add(
                egui::Label::new(
                    egui::RichText::new(target)
                        .size(12.0)
                        .color(t.text.default)
                        .family(t.fonts.code.clone()),
                )
                .truncate(),
            );

            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let caret = if expanded { "\u{25BE}" } else { "\u{203A}" };
                ui.label(
                    egui::RichText::new(caret)
                        .family(t.fonts.code.clone())
                        .size(11.0)
                        .color(t.text.muted),
                );
                ui.add_space(t.spacing.sm);
                if let Some(meta) = meta_text(view) {
                    ui.label(
                        egui::RichText::new(meta)
                            .size(10.5)
                            .color(t.text.dim)
                            .family(t.fonts.code.clone()),
                    );
                }
                if matches!(view.status, ToolStatus::Executing) {
                    ui.add_space(t.spacing.xs);
                    ui.add(egui::Spinner::new().size(11.0).color(t.accent.default));
                    ui.ctx().request_repaint();
                }
            });
        });
    });

    if matches!(display_status, DisplayStatus::Error) {
        let r = row_resp.response.rect;
        ui.painter().rect_filled(
            egui::Rect::from_min_size(r.min, egui::vec2(2.0, r.height())),
            0.0,
            t.status.error.solid,
        );
    }

    let click_resp = ui.interact(
        row_resp.response.rect,
        egui::Id::new(("tc_row", view.call_id)),
        egui::Sense::click(),
    );
    if click_resp.hovered() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
    }
    if click_resp.clicked() {
        expanded = !expanded;
        ui.memory_mut(|mem| mem.data.insert_temp(id, expanded));
    }

    if let Some(err) = view.error_text {
        ui.add_space(t.spacing.xs);
        super::error::show_inline(ui, err);
    }

    if expanded {
        ui.add_space(t.spacing.xs);
        let bar_color = family_color(tag_kind);
        let detail_resp = egui::Frame::new()
            .fill(t.surface.deep)
            .corner_radius(0.0)
            .inner_margin(egui::Margin {
                left: (t.spacing.md + 2.0) as i8,
                right: t.spacing.md as i8,
                top: t.spacing.sm as i8,
                bottom: t.spacing.sm as i8,
            })
            .show(ui, |ui| {
                let w = ui.available_width();
                ui.set_width(w);
                ui.set_max_width(w);

                match &view.args {
                    ToolArgs::Json(args) => {
                        let handled = match super::tool_summary::pick_renderer(view.name) {
                            Some(renderer) => renderer(ui, args),
                            None => false,
                        };
                        if !handled {
                            render_json_args(ui, args);
                        }
                    }
                    ToolArgs::Partial(s) => {
                        ui.label(
                            egui::RichText::new(*s)
                                .size(12.0)
                                .color(t.text.dim)
                                .family(t.fonts.code.clone()),
                        );
                    }
                    ToolArgs::None => {}
                }

                if let Some(out) = view.output {
                    ui.add_space(t.spacing.xs);
                    divider::horizontal(ui);
                    ui.add_space(t.spacing.xs);

                    let result_color = if view.is_error {
                        t.status.error.solid
                    } else {
                        t.status.success.solid
                    };
                    let label = if view.is_error { "Error" } else { "Result" };
                    ui.label(
                        egui::RichText::new(label)
                            .size(12.0)
                            .color(result_color)
                            .strong(),
                    );
                    ui.add_space(t.spacing.xs);

                    let content_frame = egui::Frame::new()
                        .fill(t.surface.abyss)
                        .corner_radius(t.radii.sm)
                        .inner_margin(t.spacing.sm);
                    content_frame.show(ui, |ui| {
                        let w = ui.available_width();
                        ui.set_width(w);
                        ui.set_max_width(w);
                        ui.add(
                            egui::Label::new(
                                egui::RichText::new(out)
                                    .size(12.0)
                                    .color(t.text.default)
                                    .family(t.fonts.code.clone()),
                            )
                            .wrap(),
                        );
                    });
                }

                if !view.images.is_empty() {
                    ui.add_space(t.spacing.xs);
                    for uri in view.images {
                        let max_w = ui.available_width().min(640.0);
                        ui.add(
                            egui::Image::new(uri)
                                .max_width(max_w)
                                .corner_radius(t.radii.sm),
                        );
                        ui.add_space(t.spacing.xs);
                    }
                }
            });

        let r = detail_resp.response.rect;
        ui.painter().rect_filled(
            egui::Rect::from_min_size(r.min, egui::vec2(2.0, r.height())),
            0.0,
            bar_color,
        );
    }

    if show_approval {
        ui.add_space(t.spacing.sm);
        ui.horizontal(|ui| {
            if ui.add(button::primary("Approve").small()).clicked() {
                action = ToolCallAction::Approve(view.call_id.to_string());
            }
            ui.add_space(t.spacing.sm);
            if ui.add(button::danger("Deny").small()).clicked() {
                action = ToolCallAction::Deny(view.call_id.to_string());
            }
        });
    }

    action
}

// ---------------------------------------------------------------------------
// Status & display derivation
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, PartialEq, Eq)]
enum DisplayStatus {
    Active,
    Done,
    Error,
}

fn display_status(view: &ToolCallView<'_>) -> DisplayStatus {
    if view.is_error
        || view.error_text.is_some()
        || matches!(view.status, ToolStatus::Denied | ToolStatus::Failed)
    {
        DisplayStatus::Error
    } else if matches!(
        view.status,
        ToolStatus::Streaming | ToolStatus::Pending | ToolStatus::Executing
    ) {
        DisplayStatus::Active
    } else {
        DisplayStatus::Done
    }
}

fn status_glyph(s: DisplayStatus) -> (&'static str, egui::Color32) {
    let t = theme();
    match s {
        DisplayStatus::Done => ("\u{2713}", t.status.success.solid),
        DisplayStatus::Error => ("\u{2717}", t.status.error.solid),
        DisplayStatus::Active => ("\u{203A}", t.accent.default),
    }
}

fn tag_for(name: &str) -> (&'static str, terminal::TagKind) {
    use terminal::TagKind;
    match name {
        "file_read" => ("READ", TagKind::Read),
        "file_grep" => ("GREP", TagKind::Info),
        "file_glob" => ("GLOB", TagKind::Info),
        "file_list" | "file_tree" => ("LIST", TagKind::Read),
        "image_view" => ("VIEW", TagKind::Read),
        "file_write" => ("WRITE", TagKind::Write),
        "file_edit" => ("EDIT", TagKind::Write),
        "shell" => ("BASH", TagKind::Warn),
        "http_request" | "screenshot_url" | "web_search" => ("WEB", TagKind::Info),
        "spawn_agent" | "message_agent" | "get_agent" | "stop_agent" => ("AGENT", TagKind::Agent),
        "load_skill" | "unload_skill" => ("SKILL", TagKind::Agent),
        "start_workflow" => ("FLOW", TagKind::Agent),
        "new_chat" => ("CHAT", TagKind::Read),
        "wait" => ("WAIT", TagKind::Neutral),
        _ => ("TOOL", TagKind::Neutral),
    }
}

fn family_color(kind: terminal::TagKind) -> egui::Color32 {
    let t = theme();
    match kind {
        terminal::TagKind::Read => t.status.success.solid,
        terminal::TagKind::Write => t.status.warning.solid,
        terminal::TagKind::Agent => t.status.purple.solid,
        terminal::TagKind::Neutral => t.text.dim,
        terminal::TagKind::Error => t.status.error.solid,
        terminal::TagKind::Warn => t.accent.default,
        terminal::TagKind::Info => t.status.info.solid,
    }
}

fn meta_text(view: &ToolCallView<'_>) -> Option<String> {
    if let Some(out) = view.output {
        if out.is_empty() {
            return None;
        }
        return Some(format_size(out.len()));
    }
    match view.status {
        ToolStatus::Pending => Some("awaiting approval".into()),
        ToolStatus::Denied => Some("denied".into()),
        _ => None,
    }
}

fn format_size(bytes: usize) -> String {
    if bytes < 1024 {
        format!("{bytes}b")
    } else if bytes < 1024 * 1024 {
        format!("{:.1}kb", bytes as f32 / 1024.0)
    } else {
        format!("{:.1}mb", bytes as f32 / (1024.0 * 1024.0))
    }
}

// ---------------------------------------------------------------------------
// JSON arg rendering
// ---------------------------------------------------------------------------

const FIELD_LABEL_WIDTH: f32 = 92.0;

fn field_row(ui: &mut Ui, key: &str, value: &Value) {
    let t = theme();
    let val_str = match value {
        Value::String(s) => s.clone(),
        other => other.to_string(),
    };

    ui.horizontal_top(|ui| {
        ui.add_space(t.spacing.xs);
        ui.scope(|ui| {
            ui.set_min_width(FIELD_LABEL_WIDTH);
            ui.set_max_width(FIELD_LABEL_WIDTH);
            ui.label(
                egui::RichText::new(key.to_uppercase())
                    .size(10.5)
                    .color(t.text.muted)
                    .family(t.fonts.code.clone()),
            );
        });
        ui.add(
            egui::Label::new(
                egui::RichText::new(val_str)
                    .size(12.0)
                    .color(t.text.default)
                    .family(t.fonts.code.clone()),
            )
            .wrap(),
        );
    });
}

fn field_separator(ui: &mut Ui) {
    let t = theme();
    ui.add_space(t.spacing.xs);
    let rect = ui.available_rect_before_wrap();
    let y = rect.min.y;
    let (x0, x1) = (rect.min.x, rect.max.x);
    ui.painter()
        .hline(x0..=x1, y, egui::Stroke::new(1.0, t.border.subtle));
    ui.add_space(t.spacing.xs);
}

/// Default JSON-args body. Used by `show` when `tool_summary::pick_renderer`
/// returns `None`. Exposed so callers can use the same formatting in custom
/// surfaces (e.g. an approval prompt).
pub fn render_json_args(ui: &mut Ui, args: &Value) {
    let t = theme();
    match args {
        Value::Object(map) if !map.is_empty() => {
            let last_idx = map.len().saturating_sub(1);
            for (i, (key, value)) in map.iter().enumerate() {
                field_row(ui, key, value);
                if i != last_idx {
                    field_separator(ui);
                }
            }
        }
        Value::Null => {
            ui.label(
                egui::RichText::new("(no arguments)")
                    .size(12.0)
                    .color(t.text.muted)
                    .italics(),
            );
        }
        other => {
            let text = serde_json::to_string_pretty(other).unwrap_or_else(|_| other.to_string());
            let frame = egui::Frame::new()
                .fill(t.surface.abyss)
                .corner_radius(t.radii.sm)
                .inner_margin(t.spacing.sm);
            frame.show(ui, |ui| {
                let w = ui.available_width();
                ui.set_max_width(w);
                ui.add(
                    egui::Label::new(
                        egui::RichText::new(text)
                            .size(12.0)
                            .color(t.text.default)
                            .family(t.fonts.code.clone()),
                    )
                    .wrap(),
                );
            });
        }
    }
}
