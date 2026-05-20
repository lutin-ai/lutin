pub mod parser;
pub mod renderer;

pub use parser::parse_markdown;
pub use renderer::MarkdownRenderer;
pub use renderer::show_link_confirmation_modal;

/// A widget that displays rendered markdown content.
///
/// Parsing happens once at creation; rendering reuses the pre-parsed AST.
#[derive(Debug, Clone)]
pub struct Markdown {
    renderer: MarkdownRenderer,
}

impl Markdown {
    pub fn new(text: &str) -> Self {
        let elements = parse_markdown(text);
        Self {
            renderer: MarkdownRenderer::new(elements),
        }
    }

    pub fn show(&self, ui: &mut egui::Ui) -> egui::Response {
        self.renderer.show(ui)
    }
}
