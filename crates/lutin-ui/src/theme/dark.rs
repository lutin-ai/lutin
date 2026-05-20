//! Dark IDE-feel preset: orange accent, neutral grays, sharp corners.

use egui::Color32;

use super::Theme;
use super::components::{
    BadgeColors, BadgeTheme, ButtonColors, ButtonTheme, CardTheme, InputTheme, NavTheme,
    PanelTheme, ScrollbarTheme, TooltipTheme,
};
use super::tokens::{
    AccentColors, BorderColors, Fonts, Radii, Spacing, StatusColor, StatusColors, SurfaceColors,
    SyntaxColors, TextColors,
};

fn spec_radii() -> Radii {
    Radii {
        sm: 6.0,
        default: 6.0,
        md: 10.0,
        lg: 14.0,
        xl: 18.0,
        // egui's CornerRadius is u8; cap at 64 (any half-height ≤ 64 yields a pill).
        full: 64.0,
    }
}

pub fn dark() -> Theme {
    let surface = SurfaceColors {
        void: Color32::from_rgb(0x10, 0x10, 0x12),
        abyss: Color32::from_rgb(0x07, 0x07, 0x09),
        deep: Color32::from_rgb(0x0c, 0x0c, 0x0f),
        base: Color32::from_rgb(0x12, 0x12, 0x15),
        elevated: Color32::from_rgb(0x1a, 0x1a, 0x1e),
        raised: Color32::from_rgb(0x22, 0x22, 0x27),
        overlay: Color32::from_rgb(0x2a, 0x2a, 0x30),
    };

    let text = TextColors {
        muted: Color32::from_rgb(0x4a, 0x4a, 0x54),
        dim: Color32::from_rgb(0x72, 0x72, 0x80),
        default: Color32::from_rgb(0xb0, 0xb0, 0xbc),
        bright: Color32::from_rgb(0xec, 0xec, 0xf0),
    };

    let border = BorderColors {
        subtle: Color32::from_rgba_premultiplied(10, 10, 11, 12),
        default: Color32::from_rgb(0x26, 0x26, 0x2c),
        strong: Color32::from_rgb(0x36, 0x36, 0x3e),
        focus: Color32::from_rgb(0xff, 0x8c, 0x42),
    };

    // Orange accent. Default = #ff8c42 (warm techy orange, JetBrains-ish).
    let accent = AccentColors {
        muted: Color32::from_rgb(0xc9, 0x6a, 0x2c),
        default: Color32::from_rgb(0xff, 0x8c, 0x42),
        bright: Color32::from_rgb(0xff, 0xa8, 0x69),
        glow: Color32::from_rgba_premultiplied(0x1f, 0x11, 0x08, 0x1a),
        glow_strong: Color32::from_rgba_premultiplied(0x3e, 0x22, 0x10, 0x33),
    };

    let status = StatusColors {
        success: StatusColor {
            solid: Color32::from_rgb(0x34, 0xd2, 0x7b),
            solid_hover: Color32::from_rgb(0x34, 0xd2, 0x7b),
            solid_active: Color32::from_rgb(0x34, 0xd2, 0x7b),
            dim: Color32::from_rgba_premultiplied(0x04, 0x11, 0x0a, 0x14),
            border: Color32::from_rgb(0x1a, 0x6b, 0x3e),
        },
        warning: StatusColor {
            solid: Color32::from_rgb(0xe5, 0xa8, 0x3b),
            solid_hover: Color32::from_rgb(0xe5, 0xa8, 0x3b),
            solid_active: Color32::from_rgb(0xe5, 0xa8, 0x3b),
            dim: Color32::from_rgba_premultiplied(0x12, 0x0d, 0x05, 0x14),
            border: Color32::from_rgb(0x73, 0x54, 0x1e),
        },
        error: StatusColor {
            solid: Color32::from_rgb(0xef, 0x55, 0x55),
            solid_hover: Color32::from_rgb(0xdc, 0x26, 0x26),
            solid_active: Color32::from_rgb(0xb9, 0x1c, 0x1c),
            dim: Color32::from_rgba_premultiplied(0x13, 0x07, 0x07, 0x14),
            border: Color32::from_rgb(0x78, 0x2b, 0x2b),
        },
        info: StatusColor {
            solid: Color32::from_rgb(0x60, 0xa5, 0xfa),
            solid_hover: Color32::from_rgb(0x60, 0xa5, 0xfa),
            solid_active: Color32::from_rgb(0x60, 0xa5, 0xfa),
            dim: Color32::from_rgba_premultiplied(0x09, 0x10, 0x1c, 0x1a),
            border: Color32::from_rgb(0x2e, 0x47, 0x78),
        },
        neutral: StatusColor {
            solid: Color32::from_rgb(0x72, 0x72, 0x80),
            solid_hover: Color32::from_rgb(0x72, 0x72, 0x80),
            solid_active: Color32::from_rgb(0x72, 0x72, 0x80),
            dim: Color32::from_rgb(0x12, 0x12, 0x15),
            border: Color32::from_rgb(0x26, 0x26, 0x2c),
        },
        orange: StatusColor {
            solid: Color32::from_rgb(0xe0, 0x8a, 0x3e),
            solid_hover: Color32::from_rgb(0xe0, 0x8a, 0x3e),
            solid_active: Color32::from_rgb(0xe0, 0x8a, 0x3e),
            dim: Color32::from_rgba_premultiplied(0x12, 0x0b, 0x05, 0x14),
            border: Color32::from_rgb(0x70, 0x45, 0x1f),
        },
        purple: StatusColor {
            solid: Color32::from_rgb(0xa0, 0x7a, 0xe8),
            solid_hover: Color32::from_rgb(0xa0, 0x7a, 0xe8),
            solid_active: Color32::from_rgb(0xa0, 0x7a, 0xe8),
            dim: Color32::from_rgba_premultiplied(0x0d, 0x0a, 0x13, 0x14),
            border: Color32::from_rgb(0x50, 0x3d, 0x74),
        },
    };

    let syntax = SyntaxColors {
        keyword: Color32::from_rgb(0xc0, 0x84, 0xfc),
        function: Color32::from_rgb(0x60, 0xa5, 0xfa),
        string: Color32::from_rgb(0x4a, 0xde, 0x80),
        comment: Color32::from_rgb(0x4a, 0x4d, 0x6a),
        operator: Color32::from_rgb(0x9b, 0x9f, 0xb8),
        property: Color32::from_rgb(0x38, 0xbd, 0xf8),
        number: Color32::from_rgb(0xfb, 0xbf, 0x24),
        r#type: Color32::from_rgb(0xf4, 0x72, 0xb6),
        variable: Color32::from_rgb(0xcc, 0xce, 0xdd),
    };

    let button = ButtonTheme {
        primary: ButtonColors {
            bg: accent.default,
            bg_hover: accent.bright,
            bg_active: accent.muted,
            bg_disabled: surface.raised,
            text: Color32::BLACK,
            text_hover: Color32::BLACK,
            text_disabled: text.muted,
            border: accent.default,
            border_hover: accent.bright,
        },
        secondary: ButtonColors {
            bg: surface.elevated,
            bg_hover: surface.raised,
            bg_active: surface.overlay,
            bg_disabled: surface.elevated,
            text: text.bright,
            text_hover: text.bright,
            text_disabled: text.muted,
            border: border.default,
            border_hover: border.strong,
        },
        ghost: ButtonColors {
            bg: Color32::TRANSPARENT,
            bg_hover: surface.elevated,
            bg_active: surface.raised,
            bg_disabled: Color32::TRANSPARENT,
            text: text.default,
            text_hover: text.bright,
            text_disabled: text.muted,
            border: Color32::TRANSPARENT,
            border_hover: Color32::TRANSPARENT,
        },
        danger: ButtonColors {
            bg: status.error.solid,
            bg_hover: status.error.solid_hover,
            bg_active: status.error.solid_active,
            bg_disabled: surface.raised,
            text: Color32::WHITE,
            text_hover: Color32::WHITE,
            text_disabled: text.muted,
            border: status.error.solid,
            border_hover: status.error.solid_hover,
        },
    };

    let input = InputTheme {
        bg: surface.deep,
        bg_focused: surface.base,
        border: border.default,
        border_hover: border.strong,
        border_focused: accent.default,
        text: text.bright,
        placeholder: text.muted,
        selection: accent.glow_strong,
        cursor: accent.bright,
    };

    let badge = BadgeTheme {
        ok: BadgeColors {
            bg: status.success.dim,
            text: status.success.solid,
            border: status.success.border,
        },
        warn: BadgeColors {
            bg: status.warning.dim,
            text: status.warning.solid,
            border: status.warning.border,
        },
        bad: BadgeColors {
            bg: status.error.dim,
            text: status.error.solid,
            border: status.error.border,
        },
        neutral: BadgeColors {
            bg: status.neutral.dim,
            text: status.neutral.solid,
            border: status.neutral.border,
        },
    };

    let card = CardTheme {
        bg: surface.elevated,
        bg_hover: surface.raised,
        border: border.subtle,
        border_hover: border.default,
    };

    let nav = NavTheme {
        bg: surface.deep,
        item_hover: surface.elevated,
        item_active: surface.raised,
        text: text.dim,
        text_active: text.bright,
        icon: text.dim,
        icon_active: accent.bright,
    };

    let panel = PanelTheme {
        bg: surface.abyss,
        border: Color32::TRANSPARENT,
        header_bg: surface.deep,
    };

    let tooltip = TooltipTheme {
        bg: surface.overlay,
        text: text.bright,
        border: border.default,
    };

    let scrollbar = ScrollbarTheme {
        track: Color32::TRANSPARENT,
        thumb: surface.raised,
        thumb_hover: surface.overlay,
    };

    Theme {
        surface,
        text,
        border,
        accent,
        status,
        syntax,
        spacing: Spacing::default(),
        radii: spec_radii(),
        button,
        input,
        badge,
        card,
        nav,
        panel,
        tooltip,
        scrollbar,
        fonts: Fonts::default(),
    }
}
