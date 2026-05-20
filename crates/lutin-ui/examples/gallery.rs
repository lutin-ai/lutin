//! Gallery — visual smoke test for every widget in `lutin-ui`.
//!
//! Run: `cargo run --example gallery`

use eframe::egui;
use lutin_ui::font::{self, Preset};
use lutin_ui::markdown::show_link_confirmation_modal;
use lutin_ui::prelude::*;
use lutin_ui::widget::icon::{
    self, ICON_DELETE, ICON_FOLDER, ICON_HOME, ICON_PERSON, ICON_SEARCH, ICON_SETTINGS,
};
use lutin_ui::widget::{
    badge, button, card,
    chat::{
        error as chat_error, thinking, tool_call,
        tool_call::{ToolArgs, ToolCallView, ToolStatus},
    },
    context_menu, divider, dropdown, kbd, list, modal, panel, table, terminal, text_input,
    timeline, toggle,
};

fn main() -> eframe::Result<()> {
    let opts = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([960.0, 720.0])
            .with_title("lutin-ui — gallery"),
        ..Default::default()
    };
    eframe::run_native(
        "lutin-ui gallery",
        opts,
        Box::new(|cc| {
            // Fonts must be registered before the theme is applied — theme
            // resolution reads named families.
            font::install(&cc.egui_ctx, Preset::Inter);
            set_theme(dark(), &cc.egui_ctx);
            Ok(Box::new(Gallery::default()))
        }),
    )
}

#[derive(Default)]
struct Gallery {
    text: String,
    multiline: String,
    password: String,
    toggle_a: bool,
    toggle_b: bool,
    selected_lang: Lang,
    card_selected: usize,
    last_action: String,
    preset: Preset,
    modal_open: bool,
}

#[derive(Default, Clone, Copy, PartialEq, Eq)]
enum Lang {
    #[default]
    Rust,
    Go,
    Zig,
    Ts,
}

impl eframe::App for Gallery {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        egui::ScrollArea::vertical().show(ui, |ui| {
                ui.heading("lutin-ui — gallery");
                ui.label(
                    egui::RichText::new("orange-accent · sharp corners · IDE feel")
                        .color(theme().text.dim),
                );
                ui.add_space(12.0);

                ui.horizontal(|ui| {
                    ui.label("font preset:");
                    let label = match self.preset {
                        Preset::Inter => "Inter",
                        Preset::Outfit => "Outfit",
                    };
                    if ui.add(button::secondary(label).small()).clicked() {
                        self.preset = match self.preset {
                            Preset::Inter => Preset::Outfit,
                            Preset::Outfit => Preset::Inter,
                        };
                        font::install(&ctx, self.preset);
                    }
                });
                ui.add_space(8.0);

                self.section_fonts(ui);
                self.section_icons(ui);

                if !self.last_action.is_empty() {
                    ui.label(
                        egui::RichText::new(format!("→ {}", self.last_action))
                            .color(theme().accent.bright),
                    );
                    ui.add_space(8.0);
                }

                self.section_buttons(ui);
                self.section_text_input(ui);
                self.section_toggle(ui);
                self.section_dropdown(ui);
                self.section_badge(ui);
                self.section_kbd(ui);
                self.section_modal(ui);
                self.section_table(ui);
                self.section_list(ui);
                self.section_timeline(ui);
                self.section_divider(ui);
                self.section_card(ui);
                self.section_panel(ui);
                self.section_context_menu(ui);
                self.section_markdown(ui);
                self.section_terminal(ui);
                self.section_chat(ui);
        });
        show_link_confirmation_modal(&ctx);
    }
}

impl Gallery {
    fn header(ui: &mut egui::Ui, label: &str) {
        ui.add_space(16.0);
        ui.label(
            egui::RichText::new(label)
                .size(13.0)
                .color(theme().text.bright),
        );
        divider::horizontal(ui);
        ui.add_space(4.0);
    }

    fn section_fonts(&mut self, ui: &mut egui::Ui) {
        Self::header(ui, "FONTS");
        let f = &theme().fonts.clone();
        ui.label(egui::RichText::new("text — default proportional").family(f.text.clone()));
        ui.label(egui::RichText::new("heading — semibold").family(f.heading.clone()));
        ui.label(egui::RichText::new("bold — heavy").family(f.bold.clone()));
        ui.label(egui::RichText::new("monospace fn() {}").family(f.code.clone()));
        ui.label(egui::RichText::new("code-strong fn() {}").family(f.code_strong.clone()));
        ui.label(egui::RichText::new("display — brand italic").family(f.display.clone()));
    }

    fn section_icons(&mut self, ui: &mut egui::Ui) {
        Self::header(ui, "ICONS");
        ui.horizontal(|ui| {
            for c in [
                ICON_HOME,
                ICON_SETTINGS,
                ICON_SEARCH,
                ICON_PERSON,
                ICON_FOLDER,
                ICON_DELETE,
            ] {
                ui.label(icon::icon(c));
            }
            ui.add_space(12.0);
            for c in [
                ICON_HOME,
                ICON_SETTINGS,
                ICON_SEARCH,
                ICON_PERSON,
                ICON_FOLDER,
                ICON_DELETE,
            ] {
                ui.label(icon::icon_filled(c));
            }
        });
    }

    fn section_buttons(&mut self, ui: &mut egui::Ui) {
        Self::header(ui, "BUTTON");
        ui.horizontal_wrapped(|ui| {
            if ui.add(button::primary("Primary")).clicked() {
                self.last_action = "primary clicked".into();
            }
            if ui.add(button::secondary("Secondary")).clicked() {
                self.last_action = "secondary clicked".into();
            }
            if ui.add(button::ghost("Ghost")).clicked() {
                self.last_action = "ghost clicked".into();
            }
            if ui.add(button::danger("Danger")).clicked() {
                self.last_action = "danger clicked".into();
            }
            ui.add(button::primary("Disabled").disabled());
            ui.add(button::secondary("Small").small());
        });
    }

    fn section_text_input(&mut self, ui: &mut egui::Ui) {
        Self::header(ui, "TEXT INPUT");
        ui.add(text_input::TextInput::new(&mut self.text).hint("type something").desired_width(280.0));
        ui.add(text_input::TextInput::new(&mut self.password).hint("password").mode(text_input::InputMode::Password).desired_width(280.0));
        ui.add(text_input::TextInput::new(&mut self.multiline).hint("multiline…").mode(text_input::InputMode::Multiline).desired_width(420.0));
    }

    fn section_toggle(&mut self, ui: &mut egui::Ui) {
        Self::header(ui, "TOGGLE");
        ui.horizontal(|ui| {
            ui.add(toggle::Toggle::new(&mut self.toggle_a).text("Enable thing"));
            ui.add_space(20.0);
            ui.add(toggle::Toggle::new(&mut self.toggle_b).text("Another flag"));
            ui.add_space(20.0);
            toggle::toggle(ui, &mut self.toggle_a);
        });
    }

    fn section_dropdown(&mut self, ui: &mut egui::Ui) {
        Self::header(ui, "DROPDOWN");
        ui.horizontal(|ui| {
            let opts = [
                (Lang::Rust, "Rust"),
                (Lang::Go, "Go"),
                (Lang::Zig, "Zig"),
                (Lang::Ts, "TypeScript"),
            ];
            dropdown::dropdown(ui, "lang", &mut self.selected_lang, &opts, 200.0);
            ui.add_space(20.0);
            dropdown::show_transparent(ui, "transparent_lang", "transparent variant", 200.0, |ui| {
                ui.label("popup body");
            });
        });
    }

    fn section_badge(&mut self, ui: &mut egui::Ui) {
        Self::header(ui, "BADGE");
        ui.horizontal_wrapped(|ui| {
            ui.add(badge::ok("ready"));
            ui.add(badge::warn("staged"));
            ui.add(badge::bad("failed"));
            ui.add(badge::neutral("idle"));
            ui.add_space(12.0);
            ui.add(badge::ok("outline").style(badge::BadgeStyle::Outline));
            ui.add(badge::bad("outline").style(badge::BadgeStyle::Outline));
        });
    }

    fn section_kbd(&mut self, ui: &mut egui::Ui) {
        Self::header(ui, "KBD");
        ui.horizontal_wrapped(|ui| {
            ui.add(kbd::kbd("⌘K"));
            ui.add_space(6.0);
            ui.add(kbd::kbd("Esc"));
            ui.add_space(6.0);
            ui.add(kbd::kbd("⇧⌘P"));
            ui.add_space(6.0);
            ui.label("inline hint");
            ui.add(kbd::kbd("Enter"));
        });
    }

    fn section_modal(&mut self, ui: &mut egui::Ui) {
        Self::header(ui, "MODAL");
        if ui.add(button::primary("Open modal")).clicked() {
            self.modal_open = true;
        }
        if self.modal_open {
            let resp = modal::Modal::new(egui::Id::new("gallery_modal"))
                .title("Confirm action")
                .show(&ui.ctx().clone(), |ui| {
                    ui.label("Are you sure you want to do the thing?");
                    ui.add_space(12.0);
                    let mut confirmed = false;
                    let mut cancelled = false;
                    ui.horizontal(|ui| {
                        if ui.add(button::secondary("Cancel")).clicked() {
                            cancelled = true;
                        }
                        if ui.add(button::primary("Confirm")).clicked() {
                            confirmed = true;
                        }
                    });
                    (confirmed, cancelled)
                });
            let (confirmed, cancelled) = resp.inner;
            if confirmed {
                self.last_action = "modal: confirmed".into();
                self.modal_open = false;
            } else if cancelled {
                self.last_action = "modal: cancelled".into();
                self.modal_open = false;
            } else if resp.should_close {
                self.modal_open = false;
            }
        }
    }

    fn section_table(&mut self, ui: &mut egui::Ui) {
        Self::header(ui, "TABLE");
        let rows = [
            ("Plan migration", "ready", 0.92_f32),
            ("Refactor auth", "staged", 0.74),
            ("Replace cache", "failed", 0.31),
            ("Add tracing", "idle", 0.55),
        ];
        table::Table::new()
            .column("#", table::Align::Right, 40.0)
            .column("Idea", table::Align::Left, table::Width::Flex)
            .column("Status", table::Align::Left, 100.0)
            .column("Score", table::Align::Right, 64.0)
            .show(ui, rows.len(), |idx, row| {
                row.cell(|ui| {
                    ui.label(format!("{}", idx + 1));
                });
                row.cell(|ui| {
                    ui.label(rows[idx].0);
                });
                row.cell(|ui| {
                    let badge_widget = match rows[idx].1 {
                        "ready" => badge::ok(rows[idx].1),
                        "staged" => badge::warn(rows[idx].1),
                        "failed" => badge::bad(rows[idx].1),
                        _ => badge::neutral(rows[idx].1),
                    };
                    ui.add(badge_widget);
                });
                row.cell(|ui| {
                    ui.label(format!("{:.2}", rows[idx].2));
                });
            });
    }

    fn section_list(&mut self, ui: &mut egui::Ui) {
        Self::header(ui, "LIST");
        let items = [
            "The \"6 of 50\" survival rate distorts the true standing supply. Agentic premiums are doing a lot of the lifting in the score.",
            "Two of the six survivors are riding the same wave. We are double-counting timing risk; pick one.",
            "Regulated verticals keep scoring well because the moat term punishes everything else. Sensitivity check overdue.",
            "The critic agents are converging — diversity collapsed by run 3.",
            "None of the killed ideas look obviously bad in isolation. Most died from comparison, not from being unworkable.",
            "What I'd want to test next: rerun with moat_w = 0.20 and distrib_w = 0.30 — distribution is doing the most damage.",
        ];
        list::List::ordered().show(ui, items.len(), |i, ui| {
            ui.label(items[i]);
        });
        ui.add_space(8.0);
        ui.label(
            egui::RichText::new("unordered:")
                .color(theme().text.dim)
                .size(11.0),
        );
        list::List::unordered().show(ui, 3, |i, ui| {
            ui.label(format!("bullet item {}", i + 1));
        });
    }

    fn section_timeline(&mut self, ui: &mut egui::Ui) {
        Self::header(ui, "TIMELINE");
        let entries = [
            timeline::Entry {
                kind: timeline::EventKind::Spawned,
                event: "spawned",
                agent: "strat-1",
                detail: "kicked off /design",
                time: "19:23:04 · 12s ago",
            },
            timeline::Entry {
                kind: timeline::EventKind::Default,
                event: "tool",
                agent: "eng-2",
                detail: "ran cargo check (workspace)",
                time: "19:23:18 · 4s ago",
            },
            timeline::Entry {
                kind: timeline::EventKind::Finished,
                event: "finished",
                agent: "strat-1",
                detail: "produced 3 candidates",
                time: "19:23:31 · just now",
            },
            timeline::Entry {
                kind: timeline::EventKind::Failed,
                event: "failed",
                agent: "crit-1",
                detail: "tool error: rate limit",
                time: "19:23:35 · just now",
            },
        ];
        timeline::Timeline::new().show(ui, &entries);
    }

    fn section_divider(&mut self, ui: &mut egui::Ui) {
        Self::header(ui, "DIVIDER");
        ui.label("plain horizontal:");
        divider::horizontal(ui);
        ui.add_space(6.0);
        ui.label("with label:");
        ui.add(divider::Divider::new().label("section"));
        ui.add_space(6.0);
        ui.label("inset 40px:");
        ui.add(divider::Divider::new().inset(40.0));
        ui.add_space(6.0);
        ui.horizontal(|ui| {
            ui.label("vertical →");
            ui.allocate_ui_with_layout(
                egui::vec2(20.0, 60.0),
                egui::Layout::top_down(egui::Align::Center),
                |ui| divider::vertical(ui),
            );
            ui.label("← vertical");
        });
    }

    fn section_card(&mut self, ui: &mut egui::Ui) {
        Self::header(ui, "CARD");
        ui.horizontal_wrapped(|ui| {
            for i in 0..4 {
                let resp = card::Card::new()
                    .clickable()
                    .selected(self.card_selected == i)
                    .show(ui, |ui| {
                        ui.set_min_size(egui::vec2(140.0, 60.0));
                        ui.label(format!("Card {i}"));
                        ui.label(
                            egui::RichText::new("click to select")
                                .color(theme().text.dim)
                                .size(11.0),
                        );
                    });
                if resp.response.clicked() {
                    self.card_selected = i;
                    self.last_action = format!("card {i} selected");
                }
            }
        });
    }

    fn section_panel(&mut self, ui: &mut egui::Ui) {
        Self::header(ui, "PANEL");
        panel::Panel::new().header("Panel with header").show(ui, |ui| {
            ui.label("Panels group related content. Header is themed via panel.header_bg.");
            ui.label("Body content lives below.");
        });
        ui.add_space(8.0);
        panel::Panel::new().show(ui, |ui| {
            ui.label("Headerless panel — same theme.panel.bg fill, no header strip.");
        });
    }

    fn section_context_menu(&mut self, ui: &mut egui::Ui) {
        Self::header(ui, "CONTEXT MENU");
        let resp = ui.add(button::secondary("right-click me"));
        context_menu::show(&resp, false, |menu| {
            if menu.item("Open") {
                self.last_action = "ctx: Open".into();
            }
            if menu.item_with_shortcut("Copy", "Ctrl+C") {
                self.last_action = "ctx: Copy".into();
            }
            if menu.item_with_shortcut("Paste", "Ctrl+V") {
                self.last_action = "ctx: Paste".into();
            }
            menu.separator();
            if menu.danger_with_shortcut("Delete", "Del") {
                self.last_action = "ctx: Delete".into();
            }
        });
    }

    fn section_markdown(&mut self, ui: &mut egui::Ui) {
        Self::header(ui, "MARKDOWN");
        let md = Markdown::new(MARKDOWN_SAMPLE);
        md.show(ui);
    }

    fn section_terminal(&mut self, ui: &mut egui::Ui) {
        Self::header(ui, "TERMINAL");
        ui.horizontal(|ui| {
            terminal::status_dot(ui, terminal::DotStatus::Idle);
            ui.add_space(4.0);
            terminal::status_dot(ui, terminal::DotStatus::Active);
            ui.add_space(4.0);
            terminal::status_dot(ui, terminal::DotStatus::Done);
            ui.add_space(4.0);
            terminal::status_dot(ui, terminal::DotStatus::Error);
        });
        ui.add_space(8.0);
        ui.horizontal(|ui| {
            terminal::tag_pill_filled(ui, "READ", terminal::TagKind::Read);
            ui.add_space(4.0);
            terminal::tag_pill_filled(ui, "WRITE", terminal::TagKind::Write);
            ui.add_space(4.0);
            terminal::tag_pill_filled(ui, "AGENT", terminal::TagKind::Agent);
            ui.add_space(4.0);
            terminal::tag_pill_filled(ui, "BASH", terminal::TagKind::Warn);
            ui.add_space(4.0);
            terminal::tag_pill_filled(ui, "WEB", terminal::TagKind::Info);
            ui.add_space(4.0);
            terminal::tag_pill_filled(ui, "TOOL", terminal::TagKind::Neutral);
        });
        ui.add_space(8.0);
        terminal::section_header(ui, "section header");
        terminal::section_header_accent(ui, "accent header");
    }

    fn section_chat(&mut self, ui: &mut egui::Ui) {
        Self::header(ui, "CHAT — ERROR");
        chat_error::show_inline(ui, "Inline: tool exited with status 137");
        ui.add_space(8.0);
        chat_error::show_block(ui, "Block: connection lost — falling back to local cache");

        Self::header(ui, "CHAT — THINKING");
        thinking::show(
            ui,
            "demo-streaming",
            "Considering whether to apply the patch directly or run the test suite first…",
            true,
        );
        ui.add_space(8.0);
        thinking::show(
            ui,
            "demo-complete",
            "**Decision:** apply patch then run targeted tests. Full suite is too slow at this stage.",
            false,
        );

        Self::header(ui, "CHAT — TOOL CALL");
        let read_args = serde_json::json!({ "path": "src/main.rs" });
        let _ = tool_call::show(
            ui,
            &ToolCallView {
                call_id: "demo-read",
                name: "file_read",
                status: ToolStatus::Done,
                args: ToolArgs::Json(&read_args),
                output: Some("fn main() {\n    println!(\"hello\");\n}\n"),
                is_error: false,
                error_text: None,
                images: &[],
            },
            false,
        );
        ui.add_space(8.0);

        let edit_args = serde_json::json!({
            "path": "src/lib.rs",
            "old_string": "fn old() {}\n",
            "new_string": "fn new() {\n    todo!()\n}\n"
        });
        let _ = tool_call::show(
            ui,
            &ToolCallView {
                call_id: "demo-edit",
                name: "file_edit",
                status: ToolStatus::Pending,
                args: ToolArgs::Json(&edit_args),
                output: None,
                is_error: false,
                error_text: None,
                images: &[],
            },
            true,
        );
        ui.add_space(8.0);

        let shell_args = serde_json::json!({ "command": "cargo test" });
        let _ = tool_call::show(
            ui,
            &ToolCallView {
                call_id: "demo-shell",
                name: "shell",
                status: ToolStatus::Failed,
                args: ToolArgs::Json(&shell_args),
                output: None,
                is_error: true,
                error_text: Some("process killed: out of memory"),
                images: &[],
            },
            false,
        );
    }
}

const MARKDOWN_SAMPLE: &str = r#"# Exploring Business Ideas

Got **#5 critique** back from *red-team-assumptions*. Folding it into the synthesis pass below alongside the margin-pressure findings.

## 10 areas, 6 survivors pressure-tested

Survivors ranked by `adjusted margin × moat-strength × time-to-revenue`.

### Top three

1. AI-augmented Legal Ops for mid-market firms
2. Agent-native CI for embedded / firmware teams
3. Vertical RAG for clinical trial protocol authoring

- Conditional verdicts only — none are unconditional buys
- ~~Dropped on hard economic constraints~~
- [x] Survives margin floor
- [ ] Awaiting design partner

> Distribution risk is real; build only with a signed design partner.

```rust
fn verdict(idea: &Idea) -> Verdict {
    if idea.margin > FLOOR { Verdict::Survives } else { Verdict::Drops }
}
```

| Idea | Margin | Verdict |
| --- | --- | --- |
| Legal Ops | 40-80k ARR | Conditional |
| Embedded CI | <2% flake | Conditional |
| Clinical RAG | 30% time saved | Conditional |

---

See [the project page](https://example.com) for context.
"#;
