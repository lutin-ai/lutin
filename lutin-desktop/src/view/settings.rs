//! Egui view for [`DesktopSettings`].
//!
//! Pure rendering: takes `&mut DesktopSettings` and a small UI buffer
//! (the form for adding a new connection), renders edit controls, and
//! optionally fires a save when the user clicks "Save". The host (App)
//! owns persistence — we only mutate the in-memory struct.
//!
//! Styling follows the rest of the chrome via `lutin-ui` widgets
//! (`panel`, `card`, `text_input`, `button`).

use egui::{Color32, RichText};
use lutin_ui::prelude::*;
use lutin_ui::widget::{button, card, panel, text_input};

use crate::settings::{ConnectionProfile, DesktopSettings};

/// Snapshot of the active connection's runtime status, rendered on
/// the matching card so the user can see why a dial isn't landing
/// without having to read logs.
pub struct ConnStatus<'a> {
    pub label: &'a str,
    pub color: Color32,
    /// Reason tail for `Rejected`/`Error` (e.g. `"auth: bad signature"`).
    pub detail: Option<&'a str>,
    /// True while a dial is in flight; the card renders a spinner.
    pub connecting: bool,
    /// True when the handshake completed; the card hides Connect and
    /// shows a "connected" badge next to the profile name.
    pub connected: bool,
}

/// Transient buffer for the "add connection" form. Lives in [`App`] so
/// keystrokes survive across frames; cleared after a successful add.
#[derive(Default)]
pub struct NewConnectionForm {
    pub name: String,
    pub addr: String,
    pub token: String,
}

/// Result of one frame of the settings view.
#[derive(Default)]
pub struct SettingsAction {
    pub save_clicked: bool,
    /// Index of the card whose Connect button was clicked. Caller
    /// should mark that profile as default, persist, and dial.
    pub connect_index: Option<usize>,
    /// Index of the card whose Reconnect button was clicked. Same
    /// effect as `connect_index` — the labels are UX hints; the App
    /// runs the same path either way.
    pub reconnect_index: Option<usize>,
}

pub fn show(
    ui: &mut egui::Ui,
    settings: &mut DesktopSettings,
    form: &mut NewConnectionForm,
    save_status: Option<&str>,
    conn_status: Option<ConnStatus<'_>>,
) -> SettingsAction {
    let t = theme();
    let mut action = SettingsAction::default();

    egui::ScrollArea::vertical()
        .auto_shrink([false, false])
        .show(ui, |ui| {
            ui.add_space(t.spacing.lg);
            ui.label(RichText::new("Settings").size(20.0).strong());
            ui.add_space(t.spacing.xs);
            ui.label(
                RichText::new("Desktop-local — connection profiles for the control panel.")
                    .size(13.0)
                    .color(t.text.dim),
            );
            ui.add_space(t.spacing.xl);

            panel::Panel::new()
                .header("Control-panel connections")
                .show(ui, |ui| {
                    if settings.connections.is_empty() {
                        ui.label(
                            RichText::new("No connections yet — add one below.")
                                .color(t.text.dim)
                                .small(),
                        );
                    }

                    let mut remove_idx: Option<usize> = None;
                    let names: Vec<String> =
                        settings.connections.iter().map(|c| c.name.clone()).collect();

                    for (i, conn) in settings.connections.iter_mut().enumerate() {
                        let is_default = !names.is_empty()
                            && (settings.default == conn.name
                                || (settings.default.is_empty() && i == 0));

                        card::Card::new()
                            .padding(t.spacing.lg)
                            .show(ui, |ui| {
                                ui.set_width(ui.available_width());
                                let show_connected = is_default
                                    && conn_status.as_ref().is_some_and(|s| s.connected);
                                ui.horizontal(|ui| {
                                    ui.label(RichText::new(&conn.name).strong());
                                    if is_default {
                                        ui.add_space(t.spacing.xs);
                                        ui.label(
                                            RichText::new("default")
                                                .small()
                                                .color(t.accent.bright),
                                        );
                                    }
                                    if show_connected {
                                        ui.add_space(t.spacing.xs);
                                        ui.label(
                                            RichText::new("connected")
                                                .small()
                                                .color(t.status.success.solid),
                                        );
                                    }
                                    ui.with_layout(
                                        egui::Layout::right_to_left(egui::Align::Center),
                                        |ui| {
                                            if ui.add(button::ghost("Remove").small()).clicked() {
                                                remove_idx = Some(i);
                                            }
                                            if ui
                                                .add(button::ghost("Reconnect").small())
                                                .clicked()
                                            {
                                                action.reconnect_index = Some(i);
                                            }
                                            if !show_connected
                                                && ui
                                                    .add(button::secondary("Connect").small())
                                                    .clicked()
                                            {
                                                action.connect_index = Some(i);
                                            }
                                            if !is_default
                                                && ui
                                                    .add(button::ghost("Make default").small())
                                                    .clicked()
                                            {
                                                settings.default = conn.name.clone();
                                            }
                                        },
                                    );
                                });

                                if is_default {
                                    if let Some(status) = &conn_status {
                                        ui.add_space(t.spacing.xs);
                                        ui.horizontal(|ui| {
                                            if status.connecting {
                                                ui.add(egui::Spinner::new().size(12.0));
                                                ui.add_space(4.0);
                                            }
                                            ui.label(
                                                RichText::new(format!("status: {}", status.label))
                                                    .small()
                                                    .color(status.color),
                                            );
                                            if let Some(detail) = status.detail {
                                                ui.label(
                                                    RichText::new(format!("— {detail}"))
                                                        .small()
                                                        .color(status.color),
                                                );
                                            }
                                        });
                                    }
                                }

                                ui.add_space(t.spacing.sm);
                                field_row(ui, "Name", |ui| {
                                    ui.add(
                                        text_input::TextInput::new(&mut conn.name)
                                            .desired_width(280.0),
                                    );
                                });
                                field_row(ui, "Address", |ui| {
                                    ui.add(
                                        text_input::TextInput::new(&mut conn.addr)
                                            .hint("127.0.0.1:7878")
                                            .desired_width(280.0),
                                    );
                                });
                                field_row(ui, "Token", |ui| {
                                    ui.add(
                                        text_input::TextInput::new(&mut conn.token)
                                            .hint("paste lutin-cp-mint output")
                                            .mode(text_input::InputMode::Password)
                                            .desired_width(420.0),
                                    );
                                });
                            });
                        ui.add_space(t.spacing.sm);
                    }
                    if let Some(idx) = remove_idx {
                        let removed = settings.connections.remove(idx);
                        if settings.default == removed.name {
                            settings.default = settings
                                .connections
                                .first()
                                .map(|c| c.name.clone())
                                .unwrap_or_default();
                        }
                    }

                    ui.add_space(t.spacing.md);
                    ui.label(RichText::new("Add connection").size(13.0).color(t.text.dim));
                    ui.add_space(t.spacing.xs);
                    ui.add(
                        text_input::TextInput::new(&mut form.name)
                            .hint("name (e.g. Local)")
                            .desired_width(280.0),
                    );
                    ui.add_space(t.spacing.xs);
                    ui.add(
                        text_input::TextInput::new(&mut form.addr)
                            .hint("127.0.0.1:7878")
                            .desired_width(280.0),
                    );
                    ui.add_space(t.spacing.xs);
                    ui.add(
                        text_input::TextInput::new(&mut form.token)
                            .hint("token from lutin-cp-mint")
                            .mode(text_input::InputMode::Password)
                            .desired_width(420.0),
                    );
                    ui.add_space(t.spacing.sm);

                    let can_add = !form.name.trim().is_empty()
                        && !form.addr.trim().is_empty()
                        && !form.token.trim().is_empty()
                        && !settings
                            .connections
                            .iter()
                            .any(|c| c.name == form.name.trim());
                    let mut add_btn = button::secondary("Add");
                    if !can_add {
                        add_btn = add_btn.disabled();
                    }
                    if ui.add(add_btn).clicked() && can_add {
                        let new = ConnectionProfile {
                            name: form.name.trim().to_string(),
                            addr: form.addr.trim().to_string(),
                            token: form.token.trim().to_string(),
                        };
                        if settings.connections.is_empty() {
                            settings.default = new.name.clone();
                        }
                        settings.connections.push(new);
                        form.name.clear();
                        form.addr.clear();
                        form.token.clear();
                    }
                });

            ui.add_space(t.spacing.xl);

            ui.horizontal(|ui| {
                if ui.add(button::primary("Save")).clicked() {
                    action.save_clicked = true;
                }
                if let Some(status) = save_status {
                    ui.add_space(t.spacing.sm);
                    ui.label(RichText::new(status).color(t.text.dim).small());
                }
            });
            ui.add_space(t.spacing.xs);
            ui.label(
                RichText::new(
                    "Save persists profiles to disk. Use Connect on a card to dial it now.",
                )
                .color(t.text.dim)
                .small(),
            );
            ui.add_space(t.spacing.xl);
        });

    action
}

fn field_row<R>(ui: &mut egui::Ui, label: &str, body: impl FnOnce(&mut egui::Ui) -> R) -> R {
    let t = theme();
    ui.horizontal(|ui| {
        ui.add_sized(
            egui::vec2(90.0, 0.0),
            egui::Label::new(RichText::new(label).color(t.text.dim).small()),
        );
        body(ui)
    })
    .inner
}
