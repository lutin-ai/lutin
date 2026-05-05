use pulldown_cmark::{Alignment, CodeBlockKind, Event, HeadingLevel, Options, Parser, Tag, TagEnd};

#[derive(Debug, Clone)]
pub enum InlineElement {
    Text(String),
    Strong(Vec<InlineElement>),
    Emphasis(Vec<InlineElement>),
    Strikethrough(Vec<InlineElement>),
    Code(String),
    Link {
        text: Vec<InlineElement>,
        url: String,
    },
}

#[derive(Debug, Clone)]
pub enum ListItem {
    Simple(Vec<InlineElement>),
    Task {
        checked: bool,
        content: Vec<InlineElement>,
    },
    Nested {
        content: Vec<InlineElement>,
        sublist: Box<MarkdownElement>,
    },
}

#[derive(Debug, Clone)]
pub struct TableCell {
    pub content: Vec<InlineElement>,
    pub alignment: Alignment,
}

#[derive(Debug, Clone)]
pub enum MarkdownElement {
    Heading {
        level: u8,
        content: Vec<InlineElement>,
    },
    Paragraph(Vec<InlineElement>),
    CodeBlock {
        language: Option<String>,
        code: String,
    },
    Quote(Vec<MarkdownElement>),
    UnorderedList(Vec<ListItem>),
    OrderedList {
        start: u64,
        items: Vec<ListItem>,
    },
    Table {
        headers: Vec<TableCell>,
        rows: Vec<Vec<TableCell>>,
    },
    HorizontalRule,
    Footnote {
        label: String,
        content: Vec<InlineElement>,
    },
}

pub fn parse_markdown(markdown: &str) -> Vec<MarkdownElement> {
    let mut options = Options::empty();
    options.insert(Options::ENABLE_TABLES);
    options.insert(Options::ENABLE_FOOTNOTES);
    options.insert(Options::ENABLE_STRIKETHROUGH);
    options.insert(Options::ENABLE_TASKLISTS);

    let parser = Parser::new_ext(markdown, options);
    let mut elements = Vec::new();
    let mut stack: Vec<ParserState> = vec![ParserState::Root];
    let mut inline_stack: Vec<InlineState> = Vec::new();

    for event in parser {
        handle_event(event, &mut elements, &mut stack, &mut inline_stack);
    }

    elements
}

fn handle_event(
    event: Event<'_>,
    elements: &mut Vec<MarkdownElement>,
    stack: &mut Vec<ParserState>,
    inline_stack: &mut Vec<InlineState>,
) {
    match event {
        Event::Start(tag) => handle_start_tag(tag, stack, inline_stack),
        Event::End(tag_end) => handle_end_tag(tag_end, elements, stack, inline_stack),
        Event::Text(text) => {
            if stack
                .iter()
                .any(|s| matches!(s, ParserState::CodeBlock { .. }))
            {
                handle_codeblock_text(&text, stack);
            } else {
                handle_text(&text, inline_stack);
            }
        }
        Event::Code(code) => handle_inline_code(&code, inline_stack),
        Event::SoftBreak | Event::HardBreak => handle_text("\n", inline_stack),
        Event::Rule => elements.push(MarkdownElement::HorizontalRule),
        Event::TaskListMarker(checked) => {
            if let Some(InlineState::TaskMarker(marker)) = inline_stack.last_mut() {
                *marker = Some(checked);
            }
        }
        _ => {}
    }
}

fn handle_start_tag(
    tag: Tag<'_>,
    stack: &mut Vec<ParserState>,
    inline_stack: &mut Vec<InlineState>,
) {
    match tag {
        Tag::Heading { level, .. } => {
            stack.push(ParserState::Heading(heading_level_to_u8(level)));
        }
        Tag::Paragraph => {
            stack.push(ParserState::Paragraph);
        }
        Tag::CodeBlock(kind) => {
            let lang = match kind {
                CodeBlockKind::Fenced(lang) if !lang.is_empty() => Some(lang.to_string()),
                _ => None,
            };
            stack.push(ParserState::CodeBlock {
                language: lang,
                code: String::new(),
            });
        }
        Tag::BlockQuote(_) => {
            stack.push(ParserState::Quote {
                elements: Vec::new(),
            });
        }
        Tag::List(start) => {
            let in_list_item = stack
                .iter()
                .any(|s| matches!(s, ParserState::ListItem { .. }));
            if in_list_item && !inline_stack.is_empty() {
                let inline_content = collect_inline_elements(inline_stack);
                if !inline_content.is_empty() {
                    let paragraph = MarkdownElement::Paragraph(inline_content);
                    if let Some(ParserState::ListItem { elements }) = stack
                        .iter_mut()
                        .rev()
                        .find(|s| matches!(s, ParserState::ListItem { .. }))
                    {
                        elements.push(paragraph);
                    }
                }
            }
            stack.push(ParserState::List {
                ordered: start,
                items: Vec::new(),
            });
        }
        Tag::Item => {
            inline_stack.push(InlineState::TaskMarker(None));
            stack.push(ParserState::ListItem {
                elements: Vec::new(),
            });
        }
        Tag::Strong => {
            inline_stack.push(InlineState::Strong(Vec::new()));
        }
        Tag::Emphasis => {
            inline_stack.push(InlineState::Emphasis(Vec::new()));
        }
        Tag::Strikethrough => {
            inline_stack.push(InlineState::Strikethrough(Vec::new()));
        }
        Tag::Link { dest_url, .. } => {
            inline_stack.push(InlineState::Link {
                url: dest_url.to_string(),
                text: Vec::new(),
            });
        }
        Tag::Table(alignments) => {
            stack.push(ParserState::Table {
                alignments: alignments.to_vec(),
                headers: Vec::new(),
                rows: Vec::new(),
                current_row: Vec::new(),
            });
        }
        Tag::TableHead => {
            stack.push(ParserState::TableHead);
        }
        Tag::TableRow => {
            stack.push(ParserState::TableRow);
        }
        Tag::TableCell => {
            stack.push(ParserState::TableCell);
        }
        Tag::FootnoteDefinition(label) => {
            stack.push(ParserState::Footnote(label.to_string()));
        }
        _ => {}
    }
}

fn handle_end_tag(
    tag_end: TagEnd,
    elements: &mut Vec<MarkdownElement>,
    stack: &mut Vec<ParserState>,
    inline_stack: &mut Vec<InlineState>,
) {
    match tag_end {
        TagEnd::Heading(_) => {
            if let Some(ParserState::Heading(level)) = stack.pop() {
                let content = collect_inline_elements(inline_stack);
                elements.push(MarkdownElement::Heading { level, content });
            }
        }
        TagEnd::Paragraph => {
            if let Some(ParserState::Paragraph) = stack.pop() {
                let content = collect_inline_elements(inline_stack);
                if !content.is_empty() {
                    let in_footnote =
                        stack.iter().any(|s| matches!(s, ParserState::Footnote(_)));
                    if in_footnote {
                        for elem in content {
                            inline_stack.push(InlineState::Direct(vec![elem]));
                        }
                    } else {
                        let paragraph = MarkdownElement::Paragraph(content);
                        push_element_to_container(paragraph, stack, elements);
                    }
                }
            }
        }
        TagEnd::CodeBlock => {
            if let Some(ParserState::CodeBlock { language, code }) = stack.pop() {
                elements.push(MarkdownElement::CodeBlock { language, code });
            }
        }
        TagEnd::BlockQuote(_) => {
            if let Some(ParserState::Quote {
                elements: quote_elements,
            }) = stack.pop()
            {
                elements.push(MarkdownElement::Quote(quote_elements));
            }
        }
        TagEnd::List(_) => {
            if let Some(ParserState::List { ordered, items }) = stack.pop() {
                let list_element = if let Some(start) = ordered {
                    MarkdownElement::OrderedList { start, items }
                } else {
                    MarkdownElement::UnorderedList(items)
                };
                push_element_to_container(list_element, stack, elements);
            }
        }
        TagEnd::Item => {
            if let Some(ParserState::ListItem {
                elements: item_elements,
            }) = stack.pop()
            {
                let task_marker_idx = inline_stack
                    .iter()
                    .position(|s| matches!(s, InlineState::TaskMarker(_)));
                let task_marker = if let Some(idx) = task_marker_idx {
                    if let InlineState::TaskMarker(checked) = &inline_stack[idx] {
                        *checked
                    } else {
                        None
                    }
                } else {
                    None
                };

                if let Some(idx) = task_marker_idx {
                    inline_stack.remove(idx);
                }

                let inline_content = collect_inline_elements(inline_stack);

                let item = if item_elements.is_empty() && !inline_content.is_empty() {
                    if let Some(checked) = task_marker {
                        ListItem::Task {
                            checked,
                            content: inline_content,
                        }
                    } else {
                        ListItem::Simple(inline_content)
                    }
                } else if !item_elements.is_empty() {
                    if let Some(checked) = task_marker {
                        ListItem::Task {
                            checked,
                            content: inline_content,
                        }
                    } else {
                        let sublist = item_elements
                            .iter()
                            .find(|e| {
                                matches!(
                                    e,
                                    MarkdownElement::UnorderedList(_)
                                        | MarkdownElement::OrderedList { .. }
                                )
                            })
                            .cloned();

                        if let Some(sublist) = sublist {
                            let parent_content =
                                if let Some(MarkdownElement::Paragraph(para_content)) =
                                    item_elements.first()
                                {
                                    para_content.clone()
                                } else {
                                    inline_content
                                };
                            ListItem::Nested {
                                content: parent_content,
                                sublist: Box::new(sublist),
                            }
                        } else {
                            let content =
                                if let Some(MarkdownElement::Paragraph(para_content)) =
                                    item_elements.first()
                                {
                                    para_content.clone()
                                } else {
                                    inline_content
                                };
                            ListItem::Simple(content)
                        }
                    }
                } else if let Some(checked) = task_marker {
                    ListItem::Task {
                        checked,
                        content: Vec::new(),
                    }
                } else {
                    ListItem::Simple(Vec::new())
                };

                if let Some(ParserState::List { items, .. }) = stack.last_mut() {
                    items.push(item);
                }
            }
        }
        TagEnd::Strong => {
            if let Some(InlineState::Strong(content)) = inline_stack.pop() {
                push_inline_element(InlineElement::Strong(content), inline_stack);
            }
        }
        TagEnd::Emphasis => {
            if let Some(InlineState::Emphasis(content)) = inline_stack.pop() {
                push_inline_element(InlineElement::Emphasis(content), inline_stack);
            }
        }
        TagEnd::Strikethrough => {
            if let Some(InlineState::Strikethrough(content)) = inline_stack.pop() {
                push_inline_element(InlineElement::Strikethrough(content), inline_stack);
            }
        }
        TagEnd::Link => {
            if let Some(InlineState::Link { url, text }) = inline_stack.pop() {
                push_inline_element(InlineElement::Link { text, url }, inline_stack);
            }
        }
        TagEnd::TableHead => {
            stack.pop(); // TableHead
        }
        TagEnd::TableRow => {
            if let Some(ParserState::TableRow) = stack.pop() {
                if let Some(ParserState::Table {
                    current_row, rows, ..
                }) = stack.last_mut()
                {
                    if !current_row.is_empty() {
                        rows.push(std::mem::take(current_row));
                    }
                }
            }
        }
        TagEnd::TableCell => {
            if let Some(ParserState::TableCell) = stack.pop() {
                let content = collect_inline_elements(inline_stack);
                let is_head = stack.iter().any(|s| matches!(s, ParserState::TableHead));

                if let Some(state) = stack
                    .iter_mut()
                    .rev()
                    .find(|s| matches!(s, ParserState::Table { .. }))
                {
                    if let ParserState::Table {
                        alignments,
                        headers,
                        current_row,
                        ..
                    } = state
                    {
                        let col_idx = if is_head {
                            headers.len()
                        } else {
                            current_row.len()
                        };
                        let alignment =
                            alignments.get(col_idx).copied().unwrap_or(Alignment::None);
                        let cell = TableCell { content, alignment };
                        if is_head {
                            headers.push(cell);
                        } else {
                            current_row.push(cell);
                        }
                    }
                }
            }
        }
        TagEnd::Table => {
            if let Some(ParserState::Table { headers, rows, .. }) = stack.pop() {
                elements.push(MarkdownElement::Table { headers, rows });
            }
        }
        TagEnd::FootnoteDefinition => {
            if let Some(ParserState::Footnote(label)) = stack.pop() {
                let content = collect_inline_elements(inline_stack);
                elements.push(MarkdownElement::Footnote { label, content });
            }
        }
        _ => {}
    }
}

fn handle_text(text: &str, inline_stack: &mut Vec<InlineState>) {
    push_inline_element(InlineElement::Text(text.to_string()), inline_stack);
}

fn handle_codeblock_text(text: &str, stack: &mut Vec<ParserState>) {
    if let Some(ParserState::CodeBlock { code, .. }) = stack.last_mut() {
        code.push_str(text);
    }
}

fn handle_inline_code(code: &str, inline_stack: &mut Vec<InlineState>) {
    push_inline_element(InlineElement::Code(code.to_string()), inline_stack);
}

fn push_inline_element(element: InlineElement, inline_stack: &mut Vec<InlineState>) {
    let insert_idx = inline_stack
        .iter()
        .rposition(|s| !matches!(s, InlineState::TaskMarker(_)));

    if let Some(idx) = insert_idx {
        match &mut inline_stack[idx] {
            InlineState::Strong(content)
            | InlineState::Emphasis(content)
            | InlineState::Strikethrough(content)
            | InlineState::Direct(content) => {
                content.push(element);
            }
            InlineState::Link { text, .. } => {
                text.push(element);
            }
            InlineState::TaskMarker(_) => unreachable!(),
        }
    } else {
        inline_stack.push(InlineState::Direct(vec![element]));
    }
}

fn collect_inline_elements(inline_stack: &mut Vec<InlineState>) -> Vec<InlineElement> {
    let mut result = Vec::new();
    for state in inline_stack.drain(..) {
        match state {
            InlineState::Strong(content) => result.push(InlineElement::Strong(content)),
            InlineState::Emphasis(content) => result.push(InlineElement::Emphasis(content)),
            InlineState::Strikethrough(content) => {
                result.push(InlineElement::Strikethrough(content))
            }
            InlineState::Link { text, url } => result.push(InlineElement::Link { text, url }),
            InlineState::Direct(content) => result.extend(content),
            InlineState::TaskMarker(_) => {}
        }
    }
    result
}

fn push_element_to_container(
    element: MarkdownElement,
    stack: &mut Vec<ParserState>,
    elements: &mut Vec<MarkdownElement>,
) {
    if let Some(state) = stack
        .iter_mut()
        .rev()
        .find(|s| matches!(s, ParserState::Quote { .. } | ParserState::ListItem { .. }))
    {
        match state {
            ParserState::Quote {
                elements: container,
            }
            | ParserState::ListItem {
                elements: container,
            } => {
                container.push(element);
            }
            _ => unreachable!(),
        }
    } else {
        elements.push(element);
    }
}

#[derive(Debug)]
enum ParserState {
    Root,
    Heading(u8),
    Paragraph,
    CodeBlock {
        language: Option<String>,
        code: String,
    },
    Quote {
        elements: Vec<MarkdownElement>,
    },
    List {
        ordered: Option<u64>,
        items: Vec<ListItem>,
    },
    ListItem {
        elements: Vec<MarkdownElement>,
    },
    Table {
        alignments: Vec<Alignment>,
        headers: Vec<TableCell>,
        rows: Vec<Vec<TableCell>>,
        current_row: Vec<TableCell>,
    },
    TableHead,
    TableRow,
    TableCell,
    Footnote(String),
}

#[derive(Debug)]
enum InlineState {
    Strong(Vec<InlineElement>),
    Emphasis(Vec<InlineElement>),
    Strikethrough(Vec<InlineElement>),
    Link {
        url: String,
        text: Vec<InlineElement>,
    },
    Direct(Vec<InlineElement>),
    TaskMarker(Option<bool>),
}

fn heading_level_to_u8(level: HeadingLevel) -> u8 {
    match level {
        HeadingLevel::H1 => 1,
        HeadingLevel::H2 => 2,
        HeadingLevel::H3 => 3,
        HeadingLevel::H4 => 4,
        HeadingLevel::H5 => 5,
        HeadingLevel::H6 => 6,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bullet_list_parsed_as_separate_items() {
        let md = "- First item\n- Second item\n- Third item";
        let elements = parse_markdown(md);
        assert_eq!(elements.len(), 1, "should produce one element: {elements:#?}");
        match &elements[0] {
            MarkdownElement::UnorderedList(items) => {
                assert_eq!(items.len(), 3, "should have 3 items: {items:#?}");
            }
            other => panic!("expected UnorderedList, got: {other:#?}"),
        }
    }

    #[test]
    fn asterisk_bullet_list_parsed() {
        let md = "* First item\n* Second item\n* Third item";
        let elements = parse_markdown(md);
        assert_eq!(elements.len(), 1);
        match &elements[0] {
            MarkdownElement::UnorderedList(items) => {
                assert_eq!(items.len(), 3);
            }
            other => panic!("expected UnorderedList, got: {other:#?}"),
        }
    }

    #[test]
    fn bullet_list_after_paragraph() {
        let md = "Here is a list:\n- Item one\n- Item two";
        let elements = parse_markdown(md);
        assert_eq!(elements.len(), 2, "paragraph + list: {elements:#?}");
        assert!(matches!(&elements[0], MarkdownElement::Paragraph(_)));
        match &elements[1] {
            MarkdownElement::UnorderedList(items) => {
                assert_eq!(items.len(), 2);
            }
            other => panic!("expected UnorderedList, got: {other:#?}"),
        }
    }

    #[test]
    fn unicode_bullet_not_a_list() {
        // LLMs sometimes use Unicode bullets instead of markdown - or *
        let md = "Here is a list:\n\u{2022} First\n\u{2022} Second\n\u{2022} Third";
        let elements = parse_markdown(md);
        // This should NOT be parsed as a list — it's just text
        eprintln!("unicode bullet result: {elements:#?}");
        for el in &elements {
            assert!(!matches!(el, MarkdownElement::UnorderedList(_)),
                "unicode bullets should not produce UnorderedList");
        }
    }

    #[test]
    fn debug_typical_llm_bullet_output() {
        // Typical LLM output patterns
        let patterns = [
            "Here are the items:\n- Item 1\n- Item 2\n- Item 3",
            "Here are the items:\n\n- Item 1\n- Item 2\n- Item 3",
            "- Item 1\n- Item 2\n- Item 3",
            "* Item 1\n* Item 2\n* Item 3",
        ];
        for md in patterns {
            let elements = parse_markdown(md);
            let has_list = elements.iter().any(|e| matches!(e, MarkdownElement::UnorderedList(_)));
            assert!(has_list, "should parse as list: {md:?}\n  got: {elements:#?}");
        }
    }
}
