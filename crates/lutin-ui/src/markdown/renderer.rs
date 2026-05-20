use super::parser::{InlineElement, ListItem, MarkdownElement};
use crate::theme::theme;
use crate::widget::{
    list::List,
    table::{Align as TblAlign, Table, Width},
};
use egui::{Color32, RichText};
use pulldown_cmark::Alignment;
use std::sync::OnceLock;
use syntect::highlighting::ThemeSet;
use syntect::parsing::SyntaxSet;

// ---------------------------------------------------------------------------
// Syntect singletons — loaded once, reused forever
// ---------------------------------------------------------------------------

fn syntax_set() -> &'static SyntaxSet {
    static SS: OnceLock<SyntaxSet> = OnceLock::new();
    SS.get_or_init(SyntaxSet::load_defaults_newlines)
}

fn highlight_theme() -> &'static syntect::highlighting::Theme {
    static HT: OnceLock<syntect::highlighting::Theme> = OnceLock::new();
    HT.get_or_init(|| {
        let ts = ThemeSet::load_defaults();
        ts.themes["base16-ocean.dark"].clone()
    })
}

// ---------------------------------------------------------------------------
// Renderer
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
struct TableCache {
    headers: Vec<String>,
    rows: Vec<Vec<String>>,
    aligns: Vec<TblAlign>,
}

#[derive(Debug, Clone)]
pub struct MarkdownRenderer {
    elements: Vec<MarkdownElement>,
    code_highlights: Vec<(usize, Vec<Vec<(Color32, String)>>)>,
    tables: Vec<(usize, TableCache)>,
}

impl MarkdownRenderer {
    pub fn new(elements: Vec<MarkdownElement>) -> Self {
        let code_highlights = elements
            .iter()
            .enumerate()
            .filter_map(|(i, el)| {
                if let MarkdownElement::CodeBlock { language: Some(lang), code } = el {
                    let trimmed = code.trim_end_matches('\n');
                    highlight_code(trimmed, lang).map(|h| (i, h))
                } else {
                    None
                }
            })
            .collect();

        let tables = elements
            .iter()
            .enumerate()
            .filter_map(|(i, el)| match el {
                MarkdownElement::Table { headers, rows } => {
                    let aligns: Vec<TblAlign> = headers
                        .iter()
                        .map(|c| match c.alignment {
                            Alignment::Left | Alignment::None => TblAlign::Left,
                            Alignment::Center => TblAlign::Center,
                            Alignment::Right => TblAlign::Right,
                        })
                        .collect();
                    let h: Vec<String> = headers
                        .iter()
                        .map(|c| {
                            let mut s = String::new();
                            for el in &c.content {
                                inline_to_text_buf(el, &mut s);
                            }
                            s
                        })
                        .collect();
                    let r: Vec<Vec<String>> = rows
                        .iter()
                        .map(|row| {
                            row.iter()
                                .map(|c| {
                                    let mut s = String::new();
                                    for el in &c.content {
                                        inline_to_text_buf(el, &mut s);
                                    }
                                    s
                                })
                                .collect()
                        })
                        .collect();
                    Some((
                        i,
                        TableCache {
                            headers: h,
                            rows: r,
                            aligns,
                        },
                    ))
                }
                _ => None,
            })
            .collect();

        Self { elements, code_highlights, tables }
    }

    pub fn show(&self, ui: &mut egui::Ui) -> egui::Response {
        let t = theme();

        ui.spacing_mut().item_spacing = egui::vec2(0.0, 0.0);

        ui.scope_builder(egui::UiBuilder::new(), |ui| {
            let count = self.elements.len();
            for (i, element) in self.elements.iter().enumerate() {
                self.render_element(ui, element, i);
                if i != count - 1 {
                    let spacing = match element {
                        MarkdownElement::HorizontalRule => t.spacing.xl,
                        _ => t.spacing.md,
                    };
                    ui.add_space(spacing);
                }
            }
        })
        .response
    }

    fn render_element(&self, ui: &mut egui::Ui, element: &MarkdownElement, element_idx: usize) {
        match element {
            MarkdownElement::Heading { level, content } => {
                self.render_heading(ui, *level, content);
            }
            MarkdownElement::Paragraph(content) => {
                self.render_paragraph(ui, content);
            }
            MarkdownElement::CodeBlock { language, code } => {
                let cached = self.code_highlights.iter().find(|(i, _)| *i == element_idx).map(|(_, h)| h);
                self.render_code_block(ui, language.as_deref(), code, cached);
            }
            MarkdownElement::Quote(content) => {
                self.render_quote(ui, content);
            }
            MarkdownElement::UnorderedList(items) => {
                self.render_list(ui, items, None);
            }
            MarkdownElement::OrderedList { start, items } => {
                self.render_list(ui, items, Some(*start));
            }
            MarkdownElement::Table { .. } => {
                if let Some((_, cache)) = self.tables.iter().find(|(i, _)| *i == element_idx) {
                    self.render_table(ui, cache);
                }
            }
            MarkdownElement::HorizontalRule => {
                self.render_horizontal_rule(ui);
            }
            MarkdownElement::Footnote { label, content } => {
                self.render_footnote(ui, label, content);
            }
        }
    }

    // -----------------------------------------------------------------------
    // Headings
    // -----------------------------------------------------------------------

    fn render_heading(&self, ui: &mut egui::Ui, level: u8, content: &[InlineElement]) {
        let t = theme();

        match level {
            1 => {
                let size = 24.0;
                let text: String = content.iter().map(inline_to_text).collect();
                ui.label(
                    RichText::new(text)
                        .color(t.text.bright)
                        .family(t.fonts.heading.clone())
                        .size(size),
                );
            }
            2 => {
                let size = 19.0;
                let text: String = content.iter().map(inline_to_text).collect();
                ui.label(
                    RichText::new(text)
                        .color(t.text.bright)
                        .family(t.fonts.heading.clone())
                        .size(size),
                );
                ui.add_space(4.0);
                let w = ui.available_width();
                let (rect, _) =
                    ui.allocate_exact_size(egui::vec2(w, 1.0), egui::Sense::hover());
                ui.painter().rect_filled(rect, 0.0, t.border.subtle);
            }
            3 => {
                let size = 15.0;
                let text: String = content.iter().map(inline_to_text).collect();
                ui.label(
                    RichText::new(text)
                        .color(t.text.bright)
                        .family(t.fonts.heading.clone())
                        .size(size),
                );
            }
            4 => {
                let size = 12.5;
                let text: String = content.iter().map(inline_to_text).collect();
                ui.label(
                    RichText::new(text.to_uppercase())
                        .color(t.text.dim)
                        .family(t.fonts.text.clone())
                        .size(size)
                        .strong(),
                );
            }
            _ => {
                let size = 11.5;
                let text: String = content.iter().map(inline_to_text).collect();
                ui.label(
                    RichText::new(text.to_uppercase())
                        .color(t.text.muted)
                        .family(t.fonts.text.clone())
                        .size(size)
                        .strong(),
                );
            }
        }

        ui.add_space(t.spacing.sm * 0.5);
    }

    // -----------------------------------------------------------------------
    // Paragraphs
    // -----------------------------------------------------------------------

    fn render_paragraph(&self, ui: &mut egui::Ui, content: &[InlineElement]) {
        let t = theme();
        ui.horizontal_wrapped(|ui| {
            ui.spacing_mut().item_spacing.y = 1.0;
            for element in content {
                self.render_inline(ui, element, t.text.default, None, false);
            }
        });
    }

    // -----------------------------------------------------------------------
    // Inline elements
    // -----------------------------------------------------------------------

    fn render_inline(
        &self,
        ui: &mut egui::Ui,
        element: &InlineElement,
        color: Color32,
        size: Option<f32>,
        bold: bool,
    ) {
        let t = theme();

        match element {
            InlineElement::Text(text) => {
                let mut rich = RichText::new(text).color(color);
                if let Some(s) = size {
                    rich = rich.size(s);
                }
                if bold {
                    rich = rich.strong();
                }
                ui.label(rich);
            }
            InlineElement::Strong(content) => {
                let strong_color = if bold { color } else { t.text.bright };
                for inner in content {
                    let mut rich = RichText::new(inline_to_text(inner))
                        .color(strong_color)
                        .strong();
                    if let Some(s) = size {
                        rich = rich.size(s);
                    }
                    ui.label(rich);
                }
            }
            InlineElement::Emphasis(content) => {
                let em_color = if bold { color } else { t.text.bright };
                for inner in content {
                    let mut rich = RichText::new(inline_to_text(inner))
                        .color(em_color)
                        .italics();
                    if let Some(s) = size {
                        rich = rich.size(s);
                    }
                    ui.label(rich);
                }
            }
            InlineElement::Strikethrough(content) => {
                for inner in content {
                    let mut rich = RichText::new(inline_to_text(inner))
                        .color(t.text.muted)
                        .strikethrough();
                    if let Some(s) = size {
                        rich = rich.size(s);
                    }
                    ui.label(rich);
                }
            }
            InlineElement::Code(code) => {
                let rich = RichText::new(code)
                    .color(t.accent.bright)
                    .family(t.fonts.code.clone())
                    .size(size.unwrap_or(13.0))
                    .underline();
                ui.label(rich);
            }
            InlineElement::Link { text, url } => {
                let link_text: String = text.iter().map(inline_to_text).collect();
                let label_size = size.unwrap_or(14.0);
                let rich = RichText::new(&link_text)
                    .color(t.accent.default)
                    .size(label_size);
                let response = ui.add(egui::Label::new(rich).sense(egui::Sense::click()));

                let rect = response.rect;
                let painter = ui.painter();
                let y = rect.bottom() - 1.0;
                if response.hovered() {
                    painter.line_segment(
                        [egui::pos2(rect.left(), y), egui::pos2(rect.right(), y)],
                        egui::Stroke::new(1.0, t.accent.bright),
                    );
                } else {
                    let mut x = rect.left();
                    while x < rect.right() {
                        let x2 = (x + 1.5).min(rect.right());
                        painter.line_segment(
                            [egui::pos2(x, y), egui::pos2(x2, y)],
                            egui::Stroke::new(1.0, t.accent.default),
                        );
                        x += 3.0;
                    }
                }

                if response.clicked() {
                    let is_safe = url.starts_with("https://") || url.starts_with("http://");
                    if is_safe {
                        let _ = webbrowser::open(url);
                    } else {
                        set_pending_link(ui.ctx(), url.clone());
                    }
                }
            }
        }
    }

    // -----------------------------------------------------------------------
    // Code blocks — flat surface.deep, accent left bar, header row.
    // -----------------------------------------------------------------------

    fn render_code_block(
        &self,
        ui: &mut egui::Ui,
        language: Option<&str>,
        code: &str,
        cached_highlight: Option<&Vec<Vec<(Color32, String)>>>,
    ) {
        let t = theme();
        let trimmed = code.trim_end_matches('\n');
        let line_count = trimmed.lines().count().max(1);

        let outer = ui.available_width();
        let bar_w = 2.0;
        let pad = t.spacing.md;

        ui.horizontal(|ui| {
            let line_h: f32 = 13.0 * 1.35;
            let total_h: f32 = (line_count as f32) * line_h + pad * 2.0;
            let (bar_rect, _) =
                ui.allocate_exact_size(egui::vec2(bar_w, total_h), egui::Sense::hover());
            ui.painter().rect_filled(bar_rect, 0.0, t.accent.default);

            let inner_w = (outer - bar_w).max(0.0);
            let frame = egui::Frame::new()
                .fill(t.surface.deep)
                .inner_margin(egui::Margin::same(pad as i8))
                .corner_radius(0.0);

            frame.show(ui, |ui| {
                ui.set_width(inner_w - 2.0 * pad);
                ui.set_max_width(inner_w - 2.0 * pad);

                if let Some(lang) = language {
                    let inner_w_avail = inner_w - 2.0 * pad;
                    let cursor = ui.cursor().min;
                    let pos = egui::pos2(cursor.x + inner_w_avail, cursor.y);
                    ui.painter().text(
                        pos,
                        egui::Align2::RIGHT_TOP,
                        lang.to_uppercase(),
                        egui::FontId::new(10.5, t.fonts.code.clone()),
                        t.text.muted,
                    );
                }

                if let Some(highlighted) = cached_highlight {
                    ui.vertical(|ui| {
                        ui.spacing_mut().item_spacing.y = 0.0;
                        ui.spacing_mut().interact_size.y = 0.0;
                        for line_spans in highlighted {
                            ui.horizontal_wrapped(|ui| {
                                ui.spacing_mut().item_spacing = egui::vec2(0.0, 0.0);
                                ui.spacing_mut().interact_size.y = 0.0;
                                for (color, text) in line_spans {
                                    ui.label(
                                        RichText::new(text)
                                            .font(egui::FontId::new(13.0, t.fonts.code.clone()))
                                            .color(*color),
                                    );
                                }
                            });
                        }
                    });
                } else {
                    ui.vertical(|ui| {
                        ui.spacing_mut().item_spacing.y = 0.0;
                        ui.spacing_mut().interact_size.y = 0.0;
                        for line in trimmed.lines() {
                            ui.label(
                                RichText::new(line)
                                    .font(egui::FontId::new(13.0, t.fonts.code.clone()))
                                    .color(t.text.default),
                            );
                        }
                    });
                }
            });
        });
    }

    // -----------------------------------------------------------------------
    // Block quotes — 2px accent.muted left bar, italic dim, narrower.
    // -----------------------------------------------------------------------

    fn render_quote(&self, ui: &mut egui::Ui, content: &[MarkdownElement]) {
        let t = theme();

        ui.horizontal(|ui| {
            let avail = ui.available_width();
            let max_w = avail * 0.92;
            let bar_w = 2.0;

            let start = ui.cursor().min;
            let bar_painter = ui.painter().clone();

            ui.add_space(bar_w);
            ui.add_space(t.spacing.md);

            let inner_w = (max_w - bar_w - t.spacing.md).max(0.0);
            ui.allocate_ui_with_layout(
                egui::vec2(inner_w, 0.0),
                egui::Layout::top_down(egui::Align::Min),
                |ui| {
                    for element in content {
                        match element {
                            MarkdownElement::Paragraph(inlines) => {
                                ui.horizontal_wrapped(|ui| {
                                    for el in inlines {
                                        let txt = inline_to_text(el);
                                        ui.label(
                                            RichText::new(txt)
                                                .color(t.text.dim)
                                                .italics(),
                                        );
                                    }
                                });
                            }
                            _ => self.render_element(ui, element, usize::MAX),
                        }
                    }
                },
            );

            let end_y = ui.cursor().top();
            let height = (end_y - start.y).max(20.0);
            bar_painter.rect_filled(
                egui::Rect::from_min_size(start, egui::vec2(bar_w, height)),
                0.0,
                t.accent.muted,
            );
        });
    }

    // -----------------------------------------------------------------------
    // Lists
    // -----------------------------------------------------------------------

    fn render_list(&self, ui: &mut egui::Ui, items: &[ListItem], ordered_start: Option<u64>) {
        let list = match ordered_start {
            Some(start) => {
                let labels: Vec<String> = (0..items.len())
                    .map(|i| format!("{}", start + i as u64))
                    .collect();
                List::custom(labels)
            }
            None => List::unordered(),
        };
        list.show(ui, items.len(), |idx, ui| {
            self.render_list_item_body(ui, &items[idx]);
        });
    }

    fn render_list_item_body(&self, ui: &mut egui::Ui, item: &ListItem) {
        let t = theme();
        match item {
            ListItem::Simple(content) => {
                let (is_task, checked, stripped) = detect_task(content);
                let source: &[InlineElement] = if is_task { &stripped } else { content };
                ui.horizontal_wrapped(|ui| {
                    if is_task {
                        let (glyph, color) = if checked {
                            ("\u{25A0}", t.accent.default)
                        } else {
                            ("\u{25A1}", t.text.dim)
                        };
                        ui.label(
                            RichText::new(glyph)
                                .family(t.fonts.code.clone())
                                .color(color)
                                .size(14.0),
                        );
                        ui.add_space(t.spacing.sm);
                    }
                    for element in source {
                        self.render_inline(ui, element, t.text.default, None, false);
                    }
                });
            }
            ListItem::Task { checked, content } => {
                ui.horizontal_wrapped(|ui| {
                    let (glyph, color) = if *checked {
                        ("\u{25A0}", t.accent.default)
                    } else {
                        ("\u{25A1}", t.text.dim)
                    };
                    ui.label(
                        RichText::new(glyph)
                            .family(t.fonts.code.clone())
                            .color(color)
                            .size(14.0),
                    );
                    ui.add_space(t.spacing.sm);
                    for element in content {
                        self.render_inline(ui, element, t.text.default, None, false);
                    }
                });
            }
            ListItem::Nested { content, sublist } => {
                ui.horizontal_wrapped(|ui| {
                    for element in content {
                        self.render_inline(ui, element, t.text.default, None, false);
                    }
                });
                ui.add_space(t.spacing.sm);
                self.render_element(ui, sublist, usize::MAX);
            }
        }
    }

    // -----------------------------------------------------------------------
    // Tables — box-drawing characters via painter.
    // -----------------------------------------------------------------------

    fn render_table(&self, ui: &mut egui::Ui, cache: &TableCache) {
        let t = theme();
        let mut table = Table::new();
        for (i, header) in cache.headers.iter().enumerate() {
            let align = cache.aligns.get(i).copied().unwrap_or(TblAlign::Left);
            table = table.column(header.clone(), align, Width::Flex);
        }
        let rows = &cache.rows;
        table.show(ui, rows.len(), |idx, row| {
            for cell in &rows[idx] {
                row.cell(|ui| {
                    ui.label(RichText::new(cell).color(t.text.default));
                });
            }
        });
    }

    // -----------------------------------------------------------------------
    // Horizontal rule — full width hairline + centered diamond glyph.
    // -----------------------------------------------------------------------

    fn render_horizontal_rule(&self, ui: &mut egui::Ui) {
        let t = theme();
        let width = ui.available_width();
        let (rect, _) = ui.allocate_exact_size(egui::vec2(width, 14.0), egui::Sense::hover());
        let painter = ui.painter();
        let mid_y = rect.center().y;
        painter.line_segment(
            [egui::pos2(rect.left(), mid_y), egui::pos2(rect.right(), mid_y)],
            egui::Stroke::new(1.0, t.border.subtle),
        );

        let glyph_w = 14.0;
        let center_x = rect.center().x;
        painter.rect_filled(
            egui::Rect::from_min_size(
                egui::pos2(center_x - glyph_w * 0.5, rect.top()),
                egui::vec2(glyph_w, rect.height()),
            ),
            0.0,
            t.surface.abyss,
        );
        painter.text(
            rect.center(),
            egui::Align2::CENTER_CENTER,
            "\u{25C6}",
            egui::FontId::new(11.0, t.fonts.code.clone()),
            t.accent.default,
        );
    }

    // -----------------------------------------------------------------------
    // Footnotes
    // -----------------------------------------------------------------------

    fn render_footnote(
        &self,
        ui: &mut egui::Ui,
        label: &str,
        content: &[InlineElement],
    ) {
        let t = theme();
        ui.horizontal(|ui| {
            ui.label(
                RichText::new(format!("[{}]", label))
                    .color(t.accent.default)
                    .family(t.fonts.code.clone())
                    .size(12.0),
            );
            ui.add_space(4.0);
            ui.horizontal_wrapped(|ui| {
                for element in content {
                    self.render_inline(ui, element, t.text.dim, Some(12.0), false);
                }
            });
        });
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn detect_task(content: &[InlineElement]) -> (bool, bool, Vec<InlineElement>) {
    let Some(InlineElement::Text(first)) = content.first() else {
        return (false, false, Vec::new());
    };
    let trimmed = first.trim_start();
    let (checked, rest) = if let Some(r) = trimmed.strip_prefix("[x]").or_else(|| trimmed.strip_prefix("[X]")) {
        (true, r)
    } else if let Some(r) = trimmed.strip_prefix("[ ]") {
        (false, r)
    } else {
        return (false, false, Vec::new());
    };
    let rest = rest.trim_start();
    let mut out: Vec<InlineElement> = Vec::with_capacity(content.len());
    out.push(InlineElement::Text(rest.to_string()));
    out.extend(content.iter().skip(1).cloned());
    (true, checked, out)
}

fn inline_to_text_buf(element: &InlineElement, buf: &mut String) {
    match element {
        InlineElement::Text(t) => buf.push_str(t),
        InlineElement::Strong(c) | InlineElement::Emphasis(c) | InlineElement::Strikethrough(c) => {
            for inner in c {
                inline_to_text_buf(inner, buf);
            }
        }
        InlineElement::Code(c) => buf.push_str(c),
        InlineElement::Link { text, .. } => {
            for inner in text {
                inline_to_text_buf(inner, buf);
            }
        }
    }
}

fn inline_to_text(element: &InlineElement) -> String {
    let mut buf = String::new();
    inline_to_text_buf(element, &mut buf);
    buf
}

/// Syntax-highlight code using syntect.
/// Returns `None` if the language isn't recognized.
fn highlight_code(code: &str, language: &str) -> Option<Vec<Vec<(Color32, String)>>> {
    let ss = syntax_set();
    let syntax = ss.find_syntax_by_token(language)?;
    let ht = highlight_theme();

    let mut highlighter = syntect::easy::HighlightLines::new(syntax, ht);
    let mut result = Vec::new();

    for line in syntect::util::LinesWithEndings::from(code) {
        let spans = highlighter.highlight_line(line, ss).ok()?;
        let line_spans: Vec<(Color32, String)> = spans
            .into_iter()
            .map(|(style, text)| {
                let fg = style.foreground;
                let color = Color32::from_rgb(fg.r, fg.g, fg.b);
                let stripped = text.strip_suffix('\n').unwrap_or(text);
                (color, stripped.to_string())
            })
            .collect();
        result.push(line_spans);
    }

    Some(result)
}

// ---------------------------------------------------------------------------
// Link confirmation modal — for non-http(s) URLs
// ---------------------------------------------------------------------------

static PENDING_LINK_ID: std::sync::LazyLock<egui::Id> =
    std::sync::LazyLock::new(|| egui::Id::new("__pending_link_url"));

fn set_pending_link(ctx: &egui::Context, url: String) {
    ctx.data_mut(|d| d.insert_temp(*PENDING_LINK_ID, url));
}

/// Show a confirmation modal if a non-http link was clicked.
/// Call this once per frame from a top-level location (e.g. `App::ui`).
pub fn show_link_confirmation_modal(ctx: &egui::Context) {
    let pending: Option<String> = ctx.data(|d| d.get_temp(*PENDING_LINK_ID));
    let Some(url) = pending else { return };

    let t = theme();
    let frame = egui::Frame::new()
        .fill(t.surface.overlay)
        .stroke(egui::Stroke::new(1.0, t.border.default))
        .corner_radius(t.radii.lg)
        .inner_margin(t.spacing.xl);

    let modal = egui::Modal::new(egui::Id::new("link_confirm_modal")).frame(frame);

    let resp = modal.show(ctx, |ui| {
        ui.set_max_width(420.0);

        ui.label(
            RichText::new("Open external link?")
                .size(18.0)
                .color(t.text.bright)
                .strong(),
        );

        ui.add_space(t.spacing.md);

        ui.label(
            RichText::new("This link uses a non-standard scheme and may interact with local applications:")
                .size(13.0)
                .color(t.text.default),
        );

        ui.add_space(t.spacing.sm);

        egui::Frame::new()
            .fill(t.surface.raised)
            .corner_radius(t.radii.sm)
            .inner_margin(t.spacing.sm)
            .show(ui, |ui| {
                ui.label(
                    RichText::new(&url)
                        .size(13.0)
                        .color(t.accent.bright)
                        .family(t.fonts.code.clone()),
                );
            });

        ui.add_space(t.spacing.lg);

        let mut open = false;
        ui.horizontal(|ui| {
            if ui
                .add(crate::widget::button::ghost("Cancel"))
                .clicked()
            {
                ui.close();
            }
            ui.add_space(t.spacing.sm);
            if ui
                .add(crate::widget::button::primary("Open"))
                .clicked()
            {
                open = true;
                ui.close();
            }
        });
        open
    });

    if resp.should_close() || resp.inner {
        ctx.data_mut(|d| { d.remove_temp::<String>(*PENDING_LINK_ID); });
        if resp.inner {
            let _ = webbrowser::open(&url);
        }
    }
}
