//! Per-component palettes derived from tokens. Keep these tied to *generic*
//! widgets only — workflow-specific component themes (chat, top-bar, sidebar,
//! ...) live with their widget.

use egui::Color32;

#[derive(Clone, Debug)]
pub struct ButtonColors {
    pub bg: Color32,
    pub bg_hover: Color32,
    pub bg_active: Color32,
    pub bg_disabled: Color32,
    pub text: Color32,
    pub text_hover: Color32,
    pub text_disabled: Color32,
    pub border: Color32,
    pub border_hover: Color32,
}

#[derive(Clone, Debug)]
pub struct ButtonTheme {
    pub primary: ButtonColors,
    pub secondary: ButtonColors,
    pub ghost: ButtonColors,
    pub danger: ButtonColors,
}

#[derive(Clone, Debug)]
pub struct InputTheme {
    pub bg: Color32,
    pub bg_focused: Color32,
    pub border: Color32,
    pub border_hover: Color32,
    pub border_focused: Color32,
    pub text: Color32,
    pub placeholder: Color32,
    pub selection: Color32,
    pub cursor: Color32,
}

#[derive(Clone, Debug)]
pub struct BadgeColors {
    pub bg: Color32,
    pub text: Color32,
    pub border: Color32,
}

#[derive(Clone, Debug)]
pub struct BadgeTheme {
    pub ok: BadgeColors,
    pub warn: BadgeColors,
    pub bad: BadgeColors,
    pub neutral: BadgeColors,
}

#[derive(Clone, Debug)]
pub struct CardTheme {
    pub bg: Color32,
    pub bg_hover: Color32,
    pub border: Color32,
    pub border_hover: Color32,
}

#[derive(Clone, Debug)]
pub struct NavTheme {
    pub bg: Color32,
    pub item_hover: Color32,
    pub item_active: Color32,
    pub text: Color32,
    pub text_active: Color32,
    pub icon: Color32,
    pub icon_active: Color32,
}

#[derive(Clone, Debug)]
pub struct PanelTheme {
    pub bg: Color32,
    pub border: Color32,
    pub header_bg: Color32,
}

#[derive(Clone, Debug)]
pub struct TooltipTheme {
    pub bg: Color32,
    pub text: Color32,
    pub border: Color32,
}

#[derive(Clone, Debug)]
pub struct ScrollbarTheme {
    pub track: Color32,
    pub thumb: Color32,
    pub thumb_hover: Color32,
}
