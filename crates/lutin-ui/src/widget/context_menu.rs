//! Themed context menu attached to an [`egui::Response`]. Opens on secondary
//! (right) click. Caller drives item rendering through a closure receiving a
//! [`ContextMenu`] handle; each `item` call paints a themed row and returns
//! `true` on click. Optional shortcut hint string renders dim-aligned right.

use egui::{Response, Ui};

use super::button::{self, ThemedButton};
use crate::theme::theme;

/// Detect a secondary (right) click within a response's rect. Useful when the
/// response itself does not sense clicks (e.g. scope responses).
pub fn detect_secondary_click(ui: &Ui, response: &Response) -> bool {
    ui.input(|i| {
        i.pointer.secondary_clicked()
            && i.pointer
                .interact_pos()
                .is_some_and(|p| response.rect.contains(p))
    })
}

/// Handle passed to the context-menu body closure.
pub struct ContextMenu<'a, 'u> {
    ui: &'a mut Ui,
    _marker: std::marker::PhantomData<&'u ()>,
}

impl<'a> ContextMenu<'a, '_> {
    /// Render a labelled item. Returns `true` when clicked this frame.
    pub fn item(&mut self, label: &str) -> bool {
        self.render_default(label, None)
    }

    /// Render an item with a trailing shortcut hint (e.g. `"Ctrl+C"`).
    pub fn item_with_shortcut(&mut self, label: &str, shortcut: &str) -> bool {
        self.render_default(label, Some(shortcut))
    }

    /// Render a destructive item in danger styling.
    pub fn danger(&mut self, label: &str) -> bool {
        self.render_danger(label, None)
    }

    /// Render a destructive item with a trailing shortcut hint.
    pub fn danger_with_shortcut(&mut self, label: &str, shortcut: &str) -> bool {
        self.render_danger(label, Some(shortcut))
    }

    /// Insert a horizontal separator between groups of items.
    pub fn separator(&mut self) {
        self.ui.separator();
    }

    fn render_default(&mut self, label: &str, shortcut: Option<&str>) -> bool {
        self.render_with(button::ghost(label), shortcut)
    }

    fn render_danger(&mut self, label: &str, shortcut: Option<&str>) -> bool {
        self.render_with(button::danger(label), shortcut)
    }

    fn render_with(&mut self, btn: ThemedButton, shortcut: Option<&str>) -> bool {
        let btn: ThemedButton = btn
            .small()
            .full_width()
            .align(button::ButtonAlign::Left);

        let response = self.ui.add(btn);

        if let Some(sc) = shortcut {
            let t = theme();
            let painter = self.ui.painter_at(response.rect);
            let font_id = egui::TextStyle::Small.resolve(self.ui.style());
            let galley = painter.layout_no_wrap(sc.to_owned(), font_id, t.text.dim);
            let pos = egui::pos2(
                response.rect.right() - t.spacing.sm - galley.size().x,
                response.rect.center().y - galley.size().y / 2.0,
            );
            painter.galley(pos, galley, t.text.dim);
        }

        response.clicked()
    }
}

/// Builder for a themed context menu attached to a [`Response`].
pub struct ContextMenuBuilder<'a> {
    response: &'a Response,
    open_now: bool,
    min_width: f32,
    max_width: f32,
}

impl<'a> ContextMenuBuilder<'a> {
    pub fn new(response: &'a Response) -> Self {
        Self {
            response,
            open_now: false,
            min_width: 120.0,
            max_width: 220.0,
        }
    }

    /// Force the menu open this frame (e.g. when a custom click detector
    /// fired). Without this, the menu opens on right-click within the
    /// response's rect, as managed by egui's popup state.
    pub fn always_open(mut self) -> Self {
        self.open_now = true;
        self
    }

    pub fn min_width(mut self, w: f32) -> Self {
        self.min_width = w;
        self
    }

    pub fn max_width(mut self, w: f32) -> Self {
        self.max_width = w;
        self
    }

    /// Show the menu, invoking `body` to render items. Returns `true` if the
    /// menu was visible this frame.
    pub fn show<R>(self, body: impl FnOnce(&mut ContextMenu<'_, '_>) -> R) -> Option<R> {
        let mut popup = egui::Popup::context_menu(self.response)
            .layout(egui::Layout::top_down_justified(egui::Align::Min))
            .close_behavior(egui::PopupCloseBehavior::CloseOnClick);

        if self.open_now {
            popup = popup.open_memory(Some(egui::SetOpenCommand::Bool(true)));
        }

        let inner = popup.show(|ui| {
                ui.set_min_width(self.min_width);
                ui.set_max_width(self.max_width);
                let mut handle = ContextMenu {
                    ui,
                    _marker: std::marker::PhantomData,
                };
                body(&mut handle)
            });

        inner.map(|r| r.inner)
    }
}

/// Attach a themed context menu to `response`. Opens on right-click within the
/// response rect, or whenever `force_open` is true this frame.
pub fn show<R>(
    response: &Response,
    force_open: bool,
    body: impl FnOnce(&mut ContextMenu<'_, '_>) -> R,
) -> Option<R> {
    let mut builder = ContextMenuBuilder::new(response);
    if force_open {
        builder = builder.always_open();
    }
    builder.show(body)
}

/// Visual tone for a menu item.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ItemTone {
    #[default]
    Default,
    Danger,
}

/// Declarative menu item — borrowed strings keep call sites zero-alloc and
/// `const`-friendly for static menus.
#[derive(Clone, Debug)]
pub struct ContextMenuItem<'a> {
    pub id: &'a str,
    pub label: &'a str,
    /// Optional Material Symbols glyph rendered before the label.
    pub icon: Option<&'static str>,
    pub tone: ItemTone,
}

/// Show a static-list context menu attached to `response`. Returns the clicked
/// item's `id` for the frame the click occurred in. Dispatches through the
/// closure-based [`show`] under the hood, so theming + open behaviour match.
pub fn show_items<'a>(
    response: &Response,
    force_open: bool,
    items: &[ContextMenuItem<'a>],
) -> Option<&'a str> {
    let mut clicked: Option<&'a str> = None;
    show(response, force_open, |menu| {
        for item in items {
            let icon = item.icon.map(super::icon::icon);
            let btn: ThemedButton = match item.tone {
                ItemTone::Danger => button::danger(item.label),
                ItemTone::Default => button::ghost(item.label),
            }
            .small()
            .full_width()
            .align(button::ButtonAlign::Left);
            let btn = if let Some(g) = icon { btn.icon(g) } else { btn };
            if menu.ui.add(btn).clicked() {
                clicked = Some(item.id);
            }
        }
    });
    clicked
}
