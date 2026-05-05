//! Realistic — visual reference for the Lutin desktop UI, ported from
//! `mockups/02_modern.html`. Unlike `gallery`, this example is a *working*
//! mockup of the actual app shell: topbar, left rail, chat, right rail,
//! composer.
//!
//! Run: `cargo run --example realistic`

use eframe::egui;
use lutin_ui::font::{self, Preset};
use lutin_ui::prelude::*;
use lutin_ui::widget::{
    badge, button, divider, kbd, list, table, text_input, timeline,
};

fn main() -> eframe::Result<()> {
    let opts = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([1400.0, 900.0])
            .with_min_inner_size([1100.0, 700.0])
            .with_title("lutin-ui — realistic"),
        ..Default::default()
    };
    eframe::run_native(
        "lutin-ui realistic",
        opts,
        Box::new(|cc| {
            font::install(&cc.egui_ctx, Preset::Inter);
            set_theme(dark(), &cc.egui_ctx);
            Ok(Box::new(Realistic::default()))
        }),
    )
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Persona {
    Strat,
    Crit,
    Arch,
    Eng,
    Res,
    Pm,
}

impl Persona {
    fn label(self) -> &'static str {
        match self {
            Persona::Strat => "strat",
            Persona::Crit => "crit",
            Persona::Arch => "arch",
            Persona::Eng => "eng",
            Persona::Res => "res",
            Persona::Pm => "pm",
        }
    }

    fn color(self) -> egui::Color32 {
        // Soft persona accents tuned to the mockup. Picked outside the theme
        // since we don't have a per-persona palette token.
        match self {
            Persona::Strat => egui::Color32::from_rgb(155, 193, 163),
            Persona::Crit => egui::Color32::from_rgb(207, 140, 140),
            Persona::Arch => egui::Color32::from_rgb(158, 181, 216),
            Persona::Eng => egui::Color32::from_rgb(212, 183, 135),
            Persona::Res => egui::Color32::from_rgb(185, 169, 211),
            Persona::Pm => egui::Color32::from_rgb(158, 197, 197),
        }
    }
}

struct AgentRow {
    name: &'static str,
    persona: Persona,
    ctx: &'static str,
    down: &'static str,
    up: &'static str,
}

const AGENTS: &[AgentRow] = &[
    AgentRow { name: "Exploring Business Ideas",  persona: Persona::Strat, ctx: "82k", down: "14", up: "9"  },
    AgentRow { name: "market-sizing-recon",       persona: Persona::Res,   ctx: "41k", down: "22", up: "5"  },
    AgentRow { name: "critique-saas-vertical",    persona: Persona::Crit,  ctx: "38k", down: "11", up: "7"  },
    AgentRow { name: "pressure-test-margins",     persona: Persona::Crit,  ctx: "29k", down: "9",  up: "4"  },
    AgentRow { name: "arch-platform-sketch",      persona: Persona::Arch,  ctx: "54k", down: "18", up: "12" },
    AgentRow { name: "eng-prototype-rust",        persona: Persona::Eng,   ctx: "61k", down: "26", up: "19" },
    AgentRow { name: "finops-unit-economics",     persona: Persona::Pm,    ctx: "22k", down: "7",  up: "3"  },
    AgentRow { name: "competitor-scrape",         persona: Persona::Res,   ctx: "47k", down: "31", up: "2"  },
    AgentRow { name: "moat-analysis",             persona: Persona::Strat, ctx: "19k", down: "5",  up: "4"  },
    AgentRow { name: "go-to-market-channels",     persona: Persona::Pm,    ctx: "33k", down: "12", up: "6"  },
    AgentRow { name: "red-team-assumptions",      persona: Persona::Crit,  ctx: "44k", down: "17", up: "8"  },
    AgentRow { name: "customer-interview-sim",    persona: Persona::Res,   ctx: "28k", down: "10", up: "5"  },
    AgentRow { name: "tech-feasibility-llm",      persona: Persona::Arch,  ctx: "37k", down: "14", up: "9"  },
    AgentRow { name: "regulatory-scan",           persona: Persona::Res,   ctx: "15k", down: "4",  up: "2"  },
    AgentRow { name: "pricing-strategy",          persona: Persona::Pm,    ctx: "21k", down: "8",  up: "5"  },
    AgentRow { name: "narrative-pitch-draft",     persona: Persona::Strat, ctx: "12k", down: "3",  up: "3"  },
    AgentRow { name: "ranking-synth",             persona: Persona::Strat, ctx: "58k", down: "21", up: "14" },
    AgentRow { name: "final-verdict-judge",       persona: Persona::Crit,  ctx: "66k", down: "19", up: "11" },
];

#[derive(Clone, Copy, PartialEq, Eq)]
enum NavTab {
    Chats,
    Files,
    Recall,
    Secrets,
    Settings,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum ActivityFilter {
    All,
    Agents,
    System,
}

struct Realistic {
    nav: NavTab,
    composer: String,
    activity_filter: ActivityFilter,
    selected_agent: usize,
    selected_project: usize,
}

impl Default for Realistic {
    fn default() -> Self {
        Self {
            nav: NavTab::Chats,
            composer: String::new(),
            activity_filter: ActivityFilter::All,
            selected_agent: 0,
            selected_project: 1,
        }
    }
}

impl eframe::App for Realistic {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let abyss = theme().surface.abyss;
        let void = theme().surface.void;
        let border = theme().border.default;

        // Topbar (44px)
        egui::Panel::top("topbar")
            .exact_size(44.0)
            .frame(egui::Frame::new()
                .fill(abyss)
                .inner_margin(egui::Margin::symmetric(16, 0))
                .stroke(egui::Stroke::new(1.0, border)))
            .show_inside(ui, |ui| self.topbar(ui));

        // Composer (64px)
        egui::Panel::bottom("composer")
            .exact_size(64.0)
            .frame(egui::Frame::new()
                .fill(abyss)
                .inner_margin(egui::Margin { left: 32, right: 32, top: 12, bottom: 12 })
                .stroke(egui::Stroke::new(1.0, border)))
            .show_inside(ui, |ui| self.composer(ui));

        // Left rail (240px)
        egui::Panel::left("left_rail")
            .exact_size(240.0)
            .resizable(false)
            .frame(egui::Frame::new()
                .fill(abyss)
                .stroke(egui::Stroke::new(1.0, border)))
            .show_inside(ui, |ui| self.left_rail(ui));

        // Right rail (320px)
        egui::Panel::right("right_rail")
            .exact_size(320.0)
            .resizable(false)
            .frame(egui::Frame::new()
                .fill(abyss)
                .stroke(egui::Stroke::new(1.0, border)))
            .show_inside(ui, |ui| self.right_rail(ui));

        // Main column
        egui::CentralPanel::default()
            .frame(egui::Frame::new().fill(void))
            .show_inside(ui, |ui| self.main_column(ui));
    }
}

impl Realistic {
    // ─────────────────────────── Top bar ──────────────────────────────
    fn topbar(&mut self, ui: &mut egui::Ui) {
        let t = theme();
        ui.horizontal_centered(|ui| {
            // Brand mark (small accent square) + name
            let (mark_rect, _) = ui.allocate_exact_size(egui::vec2(18.0, 18.0), egui::Sense::hover());
            ui.painter().rect_filled(mark_rect, t.radii.sm, t.accent.default);
            let inner = mark_rect.shrink(4.0);
            ui.painter().rect_filled(inner, 0.0, t.surface.abyss);

            ui.add_space(8.0);
            ui.label(
                egui::RichText::new("lutin")
                    .color(t.text.bright)
                    .family(t.fonts.heading.clone())
                    .size(14.0),
            );

            ui.add_space(16.0);

            // Nav
            for (tab, label) in [
                (NavTab::Chats, "chats"),
                (NavTab::Files, "files"),
                (NavTab::Recall, "recall"),
                (NavTab::Secrets, "secrets"),
                (NavTab::Settings, "settings"),
            ] {
                let active = self.nav == tab;
                let (color, bg) = if active {
                    (t.text.bright, t.surface.elevated)
                } else {
                    (t.text.dim, egui::Color32::TRANSPARENT)
                };
                let resp = ui.add(
                    egui::Label::new(egui::RichText::new(label).color(color).size(13.0))
                        .sense(egui::Sense::click()),
                );
                let rect = resp.rect.expand2(egui::vec2(6.0, 4.0));
                ui.painter().rect_filled(rect, t.radii.sm, bg);
                // Re-paint label on top of bg (cheap workaround).
                ui.painter().text(
                    rect.center(),
                    egui::Align2::CENTER_CENTER,
                    label,
                    egui::FontId::new(13.0, t.fonts.text.clone()),
                    color,
                );
                if resp.clicked() {
                    self.nav = tab;
                }
                ui.add_space(2.0);
            }

            // Right side
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                // connected indicator
                let (dot_rect, _) =
                    ui.allocate_exact_size(egui::vec2(8.0, 8.0), egui::Sense::hover());
                let center = dot_rect.center();
                ui.painter()
                    .circle_filled(center, 3.0, t.status.success.solid);
                ui.label(
                    egui::RichText::new("connected")
                        .color(t.text.dim)
                        .size(12.0),
                );
                ui.add_space(8.0);
                ui.add(kbd::kbd("⌘ K"));
            });
        });
    }

    // ─────────────────────────── Left rail ──────────────────────────────
    fn left_rail(&mut self, ui: &mut egui::Ui) {
        let t = theme();
        ui.add_space(16.0);
        // Projects label
        ui.horizontal(|ui| {
            ui.add_space(20.0);
            ui.label(
                egui::RichText::new("PROJECTS")
                    .size(11.0)
                    .color(t.text.muted)
                    .strong(),
            );
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.add_space(20.0);
                if ui
                    .add(
                        egui::Label::new(
                            egui::RichText::new("+").color(t.text.muted).size(14.0),
                        )
                        .sense(egui::Sense::click()),
                    )
                    .clicked()
                {}
            });
        });
        ui.add_space(8.0);

        // Project rows
        for (i, name) in ["default", "Business Ideas"].iter().enumerate() {
            let active = self.selected_project == i;
            let bg = if active { t.surface.elevated } else { egui::Color32::TRANSPARENT };
            let text_color = if active { t.text.bright } else { t.text.dim };

            let resp = egui::Frame::new()
                .fill(bg)
                .corner_radius(t.radii.sm)
                .inner_margin(egui::Margin { left: 20, right: 12, top: 6, bottom: 6 })
                .show(ui, |ui| {
                    ui.horizontal(|ui| {
                        ui.label(
                            egui::RichText::new("▸")
                                .color(if active { t.accent.default } else { t.text.muted })
                                .size(11.0),
                        );
                        ui.add_space(4.0);
                        ui.label(
                            egui::RichText::new(*name)
                                .color(text_color)
                                .size(13.0),
                        );
                    });
                })
                .response
                .interact(egui::Sense::click());
            if resp.clicked() {
                self.selected_project = i;
            }
            ui.add_space(1.0);
        }

        ui.add_space(10.0);
        divider::horizontal(ui);
        ui.add_space(6.0);

        // Agents table
        ui.add_space(4.0);
        egui::ScrollArea::vertical()
            .id_salt("agents_scroll")
            .auto_shrink([false, false])
            .show(ui, |ui| {
                ui.add_space(2.0);
                table::Table::new()
                    .column("Agent", table::Align::Left, table::Width::Flex)
                    .column("Persona", table::Align::Left, 60.0)
                    .column("ctx", table::Align::Right, 44.0)
                    .column("↓", table::Align::Right, 28.0)
                    .column("↑", table::Align::Right, 28.0)
                    .row_height(36.0)
                    .show(ui, AGENTS.len(), |idx, row| {
                        let a = &AGENTS[idx];
                        let active = self.selected_agent == idx;
                        row.cell(|ui| {
                            let color = if active { t.text.bright } else { t.text.default };
                            ui.label(
                                egui::RichText::new(a.name)
                                    .color(color)
                                    .size(12.5),
                            );
                        });
                        row.cell(|ui| {
                            persona_pill(ui, a.persona);
                        });
                        row.cell(|ui| {
                            ui.label(
                                egui::RichText::new(a.ctx)
                                    .family(t.fonts.code.clone())
                                    .color(t.text.muted)
                                    .size(11.0),
                            );
                        });
                        row.cell(|ui| {
                            ui.label(
                                egui::RichText::new(a.down)
                                    .family(t.fonts.code.clone())
                                    .color(t.text.muted)
                                    .size(11.0),
                            );
                        });
                        row.cell(|ui| {
                            ui.label(
                                egui::RichText::new(a.up)
                                    .family(t.fonts.code.clone())
                                    .color(t.text.muted)
                                    .size(11.0),
                            );
                        });
                    });
            });
    }

    // ─────────────────────────── Main column ──────────────────────────────
    fn main_column(&mut self, ui: &mut egui::Ui) {
        let t = theme();

        // Chat header (sticky, ~64px)
        egui::Frame::new()
            .fill(t.surface.abyss)
            .inner_margin(egui::Margin { left: 32, right: 32, top: 16, bottom: 14 })
            .stroke(egui::Stroke::new(1.0, t.border.default))
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.vertical(|ui| {
                        ui.horizontal(|ui| {
                            ui.label(
                                egui::RichText::new("BUSINESS IDEAS")
                                    .size(10.5)
                                    .color(t.text.muted)
                                    .strong(),
                            );
                            ui.label(
                                egui::RichText::new("/")
                                    .size(10.5)
                                    .color(t.text.muted),
                            );
                            ui.label(
                                egui::RichText::new("CHAT")
                                    .size(10.5)
                                    .color(t.text.muted)
                                    .strong(),
                            );
                        });
                        ui.add_space(2.0);
                        ui.label(
                            egui::RichText::new("Exploring Business Ideas in Tech Fields")
                                .size(16.0)
                                .color(t.text.bright)
                                .family(t.fonts.heading.clone())
                                .strong(),
                        );
                    });

                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui.add(button::primary("+ New run").small()).clicked() {}
                        if ui.add(button::secondary("Share").small()).clicked() {}
                        if ui.add(button::ghost("History").small()).clicked() {}
                        if ui.add(button::ghost("Outline").small()).clicked() {}
                    });
                });
            });

        egui::ScrollArea::vertical()
            .id_salt("chat_scroll")
            .auto_shrink([false, false])
            .show(ui, |ui| {
                egui::Frame::new()
                    .inner_margin(egui::Margin { left: 32, right: 32, top: 28, bottom: 24 })
                    .show(ui, |ui| {
                        ui.set_max_width(920.0);
                        self.message_strat(ui);
                        ui.add_space(24.0);
                        self.message_judge(ui);
                    });
            });
    }

    fn message_strat(&self, ui: &mut egui::Ui) {
        let t = theme();
        msg_meta(ui, "strat", "Exploring Business Ideas", "19:21 · 4.2s · 1,847 tok");
        ui.add_space(6.0);
        let prose = "Got #5 critique back from red-team-assumptions. Folding it into the synthesis pass below alongside the margin-pressure findings. Three of the original ten areas dropped on hard economic constraints; one upgraded after the channel re-think.";
        ui.label(
            egui::RichText::new(prose)
                .color(t.text.default)
                .size(14.0),
        );
    }

    fn message_judge(&self, ui: &mut egui::Ui) {
        let t = theme();
        msg_meta(ui, "judge", "final-verdict-judge", "19:23 · 7.8s · 3,204 tok");
        ui.add_space(8.0);

        // FINAL VERDICT section heading w/ kicker badge
        section_heading(ui, "FINAL VERDICT", "10 areas, 6 survivors pressure-tested");
        ui.add_space(2.0);
        ui.label(
            egui::RichText::new(
                "Survivors ranked by adjusted margin × moat-strength × time-to-revenue. \
                 All six carry conditional verdicts — none are unconditional buys.",
            )
            .color(t.text.dim)
            .size(13.0),
        );

        ui.add_space(14.0);

        // Idea cards
        let ideas: &[(u32, &str, &str, &str)] = &[
            (
                1,
                "AI-augmented Legal Ops for mid-market firms",
                "Mid-market GCs will pay $40–80k ARR for clause-level workflow tooling once one design partner ships measurable hours-saved data.",
                "Survives. Distribution risk is real; build only with a signed design partner.",
            ),
            (
                2,
                "Agent-native CI for embedded / firmware teams",
                "Hardware-in-the-loop CI is painful enough that teams will tolerate a thicker agent runtime in exchange for flake-rate reduction below 2%.",
                "Survives. Narrow ICP; defensible if we own the simulator integrations.",
            ),
            (
                3,
                "Vertical RAG for clinical trial protocol authoring",
                "CRO medical writers will adopt a tool that cites primary literature inline if it shaves ≥30% off first-draft time and survives audit review.",
                "Survives. Regulatory load is the moat and the killer; depends on QA partner.",
            ),
        ];
        for (n, title, lba, verdict) in ideas {
            idea_card(ui, *n, title, lba, verdict);
            ui.add_space(10.0);
        }

        ui.add_space(8.0);

        // Code block — read-only multiline TextEdit styled as code, in surface.void
        let code = "# scoring weights — adjust before next pass\n\
                    margin_w      = 0.40\n\
                    moat_w        = 0.35\n\
                    ttr_w         = 0.15   # time-to-revenue\n\
                    distrib_w     = 0.10\n\
                    kill_floor    = 0.42   # anything below ⇒ ✕ Kill\n";
        egui::Frame::new()
            .fill(t.surface.void)
            .stroke(egui::Stroke::new(1.0, t.border.default))
            .corner_radius(t.radii.md)
            .inner_margin(egui::Margin { left: 16, right: 16, top: 14, bottom: 14 })
            .show(ui, |ui| {
                ui.set_width(ui.available_width());
                let mut buf = code.to_string();
                ui.add(
                    egui::TextEdit::multiline(&mut buf)
                        .font(egui::FontId::new(12.0, t.fonts.code.clone()))
                        .text_color(t.text.default)
                        .frame(egui::Frame::NONE)
                        .interactive(false)
                        .desired_width(f32::INFINITY)
                        .desired_rows(6),
                );
            });

        ui.add_space(20.0);

        // META section heading + ordered list
        section_heading(ui, "META", "Honest meta-observations");
        ui.add_space(8.0);

        let meta_items = [
            "The \"6 of 50\" survival rate distorts the true standing supply. Agentic premiums are doing a lot of the lifting in the score.",
            "Two of the six survivors (#4 Agent-ops, #5 Skills-marketplace) are riding the same wave. We are double-counting timing risk; pick one.",
            "Regulated verticals (#3, #6) keep scoring well because the moat term punishes everything else. Sensitivity check on moat_w overdue.",
            "The critic agents are converging — red-team-assumptions and pressure-test-margins agreed on 9/10 verdicts, suspicious diversity collapse by run 3.",
            "What I'd want to test next: rerun with moat_w = 0.20 and distrib_w = 0.30 — distribution is doing the most damage.",
        ];
        list::List::ordered().show(ui, meta_items.len(), |i, ui| {
            ui.label(
                egui::RichText::new(meta_items[i])
                    .color(theme().text.default)
                    .size(13.0),
            );
        });
    }

    // ─────────────────────────── Right rail ──────────────────────────────
    fn right_rail(&mut self, ui: &mut egui::Ui) {
        let t = theme();
        // Header
        egui::Frame::new()
            .fill(t.surface.abyss)
            .inner_margin(egui::Margin::symmetric(16, 14))
            .stroke(egui::Stroke::new(1.0, t.border.default))
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.label(
                        egui::RichText::new("Activity")
                            .color(t.text.bright)
                            .family(t.fonts.heading.clone())
                            .size(13.0)
                            .strong(),
                    );
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        for (filter, label) in [
                            (ActivityFilter::System, "System"),
                            (ActivityFilter::Agents, "Agents"),
                            (ActivityFilter::All, "All"),
                        ] {
                            let on = self.activity_filter == filter;
                            let color = if on { t.text.bright } else { t.text.muted };
                            let bg = if on { t.surface.elevated } else { egui::Color32::TRANSPARENT };
                            let resp = egui::Frame::new()
                                .fill(bg)
                                .corner_radius(t.radii.sm)
                                .inner_margin(egui::Margin::symmetric(8, 3))
                                .show(ui, |ui| {
                                    ui.label(egui::RichText::new(label).color(color).size(11.0));
                                })
                                .response
                                .interact(egui::Sense::click());
                            if resp.clicked() {
                                self.activity_filter = filter;
                            }
                            ui.add_space(2.0);
                        }
                    });
                });
            });

        // Timeline
        egui::ScrollArea::vertical()
            .id_salt("timeline_scroll")
            .auto_shrink([false, false])
            .show(ui, |ui| {
                ui.add_space(8.0);
                egui::Frame::new()
                    .inner_margin(egui::Margin::symmetric(16, 8))
                    .show(ui, |ui| {
                        let entries = [
                            timeline::Entry {
                                kind: timeline::EventKind::Spawned,
                                event: "Spawned",
                                agent: "final-verdict-judge",
                                detail: "judge persona · context 66k",
                                time: "19:23:04 · 12s ago",
                            },
                            timeline::Entry {
                                kind: timeline::EventKind::Finished,
                                event: "Finished",
                                agent: "red-team-assumptions",
                                detail: "returned 9 verdicts · 2.1k tok",
                                time: "19:22:51",
                            },
                            timeline::Entry {
                                kind: timeline::EventKind::Finished,
                                event: "Finished",
                                agent: "pressure-test-margins",
                                detail: "flagged 4 ideas below kill_floor",
                                time: "19:22:38",
                            },
                            timeline::Entry {
                                kind: timeline::EventKind::Spawned,
                                event: "Spawned",
                                agent: "ranking-synth",
                                detail: "strat persona · merged 6 streams",
                                time: "19:22:14",
                            },
                            timeline::Entry {
                                kind: timeline::EventKind::Failed,
                                event: "Failed",
                                agent: "competitor-scrape",
                                detail: "rate-limited · retried 2/3",
                                time: "19:20:48",
                            },
                            timeline::Entry {
                                kind: timeline::EventKind::Finished,
                                event: "Started",
                                agent: "session",
                                detail: "project: Business Ideas",
                                time: "19:17:54",
                            },
                        ];
                        timeline::Timeline::new().show(ui, &entries);
                    });
            });
    }

    // ─────────────────────────── Composer ──────────────────────────────
    fn composer(&mut self, ui: &mut egui::Ui) {
        let t = theme();
        ui.horizontal_centered(|ui| {
            // Model selector pill
            egui::Frame::new()
                .fill(t.surface.elevated)
                .stroke(egui::Stroke::new(1.0, t.border.strong))
                .corner_radius(t.radii.md)
                .inner_margin(egui::Margin::symmetric(10, 6))
                .show(ui, |ui| {
                    ui.horizontal(|ui| {
                        let (dot_rect, _) = ui
                            .allocate_exact_size(egui::vec2(10.0, 10.0), egui::Sense::hover());
                        ui.painter().circle_filled(
                            dot_rect.center(),
                            3.5,
                            t.accent.default,
                        );
                        ui.label(
                            egui::RichText::new("claude-opus-4-7")
                                .family(t.fonts.code.clone())
                                .color(t.text.bright)
                                .size(11.5),
                        );
                        ui.label(
                            egui::RichText::new("▾").color(t.text.muted).size(9.0),
                        );
                    });
                });

            ui.add_space(8.0);

            // Token meter (right side, but draw it after input via right-to-left)
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                // Send button — primary square
                if ui.add(button::primary("→").small()).clicked() {
                    self.composer.clear();
                }
                ui.add_space(6.0);

                ui.label(
                    egui::RichText::new("●")
                        .color(t.accent.default)
                        .size(11.0),
                );
                ui.label(
                    egui::RichText::new("82,104 / 200k ctx")
                        .family(t.fonts.code.clone())
                        .color(t.text.muted)
                        .size(11.0),
                );
                ui.add_space(8.0);

                // Input flexes to fill remaining space
                let avail = ui.available_width();
                ui.add(
                    text_input::TextInput::new(&mut self.composer)
                        .hint("Send a message, drop a file, or @mention an agent…")
                        .desired_width(avail),
                );
            });
        });
    }
}

// ─────────────────────────── helpers ──────────────────────────────

fn persona_pill(ui: &mut egui::Ui, persona: Persona) {
    let t = theme();
    let color = persona.color();
    // Subtle tinted bg matching the persona color, ~10% opacity.
    let bg = egui::Color32::from_rgba_unmultiplied(color.r(), color.g(), color.b(), 26);
    egui::Frame::new()
        .fill(bg)
        .corner_radius(t.radii.sm)
        .inner_margin(egui::Margin::symmetric(6, 2))
        .show(ui, |ui| {
            ui.label(
                egui::RichText::new(persona.label())
                    .color(color)
                    .size(10.0)
                    .strong(),
            );
        });
}

fn msg_meta(ui: &mut egui::Ui, role: &str, who: &str, ts: &str) {
    let t = theme();
    ui.horizontal(|ui| {
        ui.label(
            egui::RichText::new(role.to_uppercase())
                .color(t.accent.bright)
                .family(t.fonts.bold.clone())
                .size(11.0)
                .strong(),
        );
        ui.label(
            egui::RichText::new(who)
                .color(t.text.bright)
                .size(12.5)
                .strong(),
        );
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            ui.label(
                egui::RichText::new(ts)
                    .family(t.fonts.code.clone())
                    .color(t.text.muted)
                    .size(10.5),
            );
        });
    });
}

fn section_heading(ui: &mut egui::Ui, kicker: &str, title: &str) {
    let t = theme();
    ui.add_space(8.0);
    ui.horizontal(|ui| {
        // Kicker badge — accent-tinted
        egui::Frame::new()
            .fill(t.accent.glow)
            .corner_radius(t.radii.sm)
            .inner_margin(egui::Margin::symmetric(8, 3))
            .show(ui, |ui| {
                ui.label(
                    egui::RichText::new(kicker)
                        .color(t.accent.bright)
                        .family(t.fonts.code.clone())
                        .size(10.0)
                        .strong(),
                );
            });
        ui.add_space(4.0);
        ui.label(
            egui::RichText::new(title)
                .color(t.text.bright)
                .family(t.fonts.heading.clone())
                .size(18.0)
                .strong(),
        );
    });
}

fn idea_card(ui: &mut egui::Ui, num: u32, title: &str, lba: &str, verdict: &str) {
    let t = theme();
    egui::Frame::new()
        .fill(t.surface.abyss)
        .stroke(egui::Stroke::new(1.0, t.border.default))
        .corner_radius(t.radii.md)
        .inner_margin(egui::Margin { left: 16, right: 16, top: 14, bottom: 14 })
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            // Head
            ui.horizontal(|ui| {
                ui.label(
                    egui::RichText::new(format!("{num}"))
                        .family(t.fonts.code.clone())
                        .color(t.text.muted)
                        .size(11.0),
                );
                ui.add_space(4.0);
                ui.label(
                    egui::RichText::new(title)
                        .color(t.text.bright)
                        .family(t.fonts.heading.clone())
                        .size(14.0)
                        .strong(),
                );
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.add(badge::ok("● Conditional"));
                });
            });
            ui.add_space(8.0);
            divider::horizontal(ui);
            ui.add_space(8.0);

            // Key/value grid
            egui::Grid::new(format!("idea_grid_{num}"))
                .num_columns(2)
                .spacing(egui::vec2(16.0, 6.0))
                .show(ui, |ui| {
                    ui.label(
                        egui::RichText::new("LOAD-BEARING ASSUMPTION")
                            .color(t.text.muted)
                            .size(10.5)
                            .strong(),
                    );
                    ui.label(
                        egui::RichText::new(lba)
                            .color(t.text.default)
                            .size(12.5),
                    );
                    ui.end_row();

                    ui.label(
                        egui::RichText::new("VERDICT")
                            .color(t.text.muted)
                            .size(10.5)
                            .strong(),
                    );
                    ui.label(
                        egui::RichText::new(verdict)
                            .color(t.text.dim)
                            .size(12.5),
                    );
                    ui.end_row();
                });
        });
}
