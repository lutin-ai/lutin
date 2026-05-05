//! Design tokens + component palettes, swappable at runtime.
//!
//! Global access via [`theme()`]; swap via [`set_theme`]. Backed by
//! [`arc_swap::ArcSwap`] so reads are pointer-load cheap and swaps are atomic.

pub mod components;
pub mod dark;
pub mod tokens;

use std::sync::{Arc, OnceLock};

use arc_swap::ArcSwap;

use components::{
    BadgeTheme, ButtonTheme, CardTheme, InputTheme, NavTheme, PanelTheme, ScrollbarTheme,
    TooltipTheme,
};
use tokens::{
    AccentColors, BorderColors, Fonts, Radii, Spacing, StatusColors, SurfaceColors, SyntaxColors,
    TextColors,
};

#[derive(Clone, Debug)]
pub struct Theme {
    pub surface: SurfaceColors,
    pub text: TextColors,
    pub border: BorderColors,
    pub accent: AccentColors,
    pub status: StatusColors,
    pub syntax: SyntaxColors,
    pub spacing: Spacing,
    pub radii: Radii,
    pub button: ButtonTheme,
    pub input: InputTheme,
    pub badge: BadgeTheme,
    pub card: CardTheme,
    pub nav: NavTheme,
    pub panel: PanelTheme,
    pub tooltip: TooltipTheme,
    pub scrollbar: ScrollbarTheme,
    pub fonts: Fonts,
}

static GLOBAL: OnceLock<ArcSwap<Theme>> = OnceLock::new();

/// Active theme. Cheap: pointer read + thread-local epoch tick.
///
/// # Panics
/// Panics if no theme has been installed via [`set_theme`].
#[inline(always)]
pub fn theme() -> arc_swap::Guard<Arc<Theme>> {
    GLOBAL
        .get()
        .expect("lutin-ui: theme not initialised — call set_theme first")
        .load()
}

/// Install a theme and apply it to the given egui context.
///
/// Subsequent calls swap atomically; existing `Guard`s remain valid.
pub fn set_theme(theme: Theme, ctx: &egui::Context) {
    ctx.set_visuals(build_visuals(&theme));
    ctx.set_global_style(build_style(&theme, &ctx.global_style()));

    let arc = Arc::new(theme);
    if let Some(swap) = GLOBAL.get() {
        swap.store(arc);
    } else {
        let _ = GLOBAL.set(ArcSwap::new(arc));
    }
}

fn build_visuals(t: &Theme) -> egui::Visuals {
    let mut v = egui::Visuals::dark();

    v.panel_fill = t.surface.abyss;
    v.window_fill = t.surface.abyss;
    v.extreme_bg_color = t.surface.void;
    v.faint_bg_color = t.surface.deep;
    v.code_bg_color = t.surface.raised;

    v.override_text_color = None;

    v.selection.bg_fill = t.accent.glow_strong;
    v.selection.stroke = egui::Stroke::new(1.0, t.text.bright);

    let r = egui::CornerRadius::same(t.radii.default as u8);

    v.widgets.noninteractive.bg_fill = t.surface.base;
    v.widgets.noninteractive.fg_stroke = egui::Stroke::new(1.0, t.text.default);
    v.widgets.noninteractive.bg_stroke = egui::Stroke::new(1.0, t.border.subtle);
    v.widgets.noninteractive.corner_radius = r;

    v.widgets.inactive.bg_fill = t.surface.elevated;
    v.widgets.inactive.fg_stroke = egui::Stroke::new(1.0, t.text.dim);
    v.widgets.inactive.bg_stroke = egui::Stroke::new(1.0, t.border.default);
    v.widgets.inactive.corner_radius = r;

    v.widgets.hovered.bg_fill = t.surface.raised;
    v.widgets.hovered.fg_stroke = egui::Stroke::new(1.0, t.text.bright);
    v.widgets.hovered.bg_stroke = egui::Stroke::new(1.0, t.border.strong);
    v.widgets.hovered.corner_radius = r;

    v.widgets.active.bg_fill = t.surface.overlay;
    v.widgets.active.fg_stroke = egui::Stroke::new(1.0, t.text.bright);
    v.widgets.active.bg_stroke = egui::Stroke::new(1.0, t.accent.default);
    v.widgets.active.corner_radius = r;

    v.window_corner_radius = egui::CornerRadius::same(t.radii.lg as u8);
    v.window_stroke = egui::Stroke::new(1.0, t.border.subtle);

    v
}

fn build_style(_t: &Theme, base: &egui::Style) -> egui::Style {
    let mut style = base.clone();
    style.spacing.item_spacing = egui::vec2(8.0, 6.0);
    style.spacing.button_padding = egui::vec2(12.0, 6.0);
    style.spacing.interact_size = egui::vec2(40.0, 28.0);
    style.spacing.combo_width = 160.0;
    style.spacing.scroll.floating_allocated_width = style.spacing.scroll.bar_width;
    style
}
