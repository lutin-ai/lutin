//! Primitive design tokens — colours, spacing, radii.

use egui::Color32;

/// Background surfaces, ordered darkest → lightest.
#[derive(Clone, Debug)]
pub struct SurfaceColors {
    /// App frame.
    pub void: Color32,
    /// Floating panel bg.
    pub abyss: Color32,
    /// Elevated content.
    pub deep: Color32,
    /// Inputs, default surfaces.
    pub base: Color32,
    /// Hover targets.
    pub elevated: Color32,
    /// Badges, code blocks.
    pub raised: Color32,
    /// Hover-on-interactive, selection bg.
    pub overlay: Color32,
}

#[derive(Clone, Debug)]
pub struct TextColors {
    pub muted: Color32,
    pub dim: Color32,
    pub default: Color32,
    pub bright: Color32,
}

#[derive(Clone, Debug)]
pub struct BorderColors {
    pub subtle: Color32,
    pub default: Color32,
    pub strong: Color32,
    pub focus: Color32,
}

#[derive(Clone, Debug)]
pub struct AccentColors {
    pub muted: Color32,
    pub default: Color32,
    pub bright: Color32,
    /// Translucent halo for hover.
    pub glow: Color32,
    /// Translucent halo for selection / focus.
    pub glow_strong: Color32,
}

#[derive(Clone, Debug)]
pub struct StatusColor {
    pub solid: Color32,
    pub solid_hover: Color32,
    pub solid_active: Color32,
    pub dim: Color32,
    pub border: Color32,
}

#[derive(Clone, Debug)]
pub struct StatusColors {
    pub success: StatusColor,
    pub warning: StatusColor,
    pub error: StatusColor,
    pub info: StatusColor,
    pub neutral: StatusColor,
    pub orange: StatusColor,
    pub purple: StatusColor,
}

#[derive(Clone, Debug)]
pub struct SyntaxColors {
    pub keyword: Color32,
    pub function: Color32,
    pub string: Color32,
    pub comment: Color32,
    pub operator: Color32,
    pub property: Color32,
    pub number: Color32,
    pub r#type: Color32,
    pub variable: Color32,
}

#[derive(Clone, Copy, Debug)]
pub struct Spacing {
    pub xs: f32,
    pub sm: f32,
    pub md: f32,
    pub lg: f32,
    pub xl: f32,
    pub xxl: f32,
    pub xxxl: f32,
}

impl Default for Spacing {
    fn default() -> Self {
        Self {
            xs: 4.0,
            sm: 8.0,
            md: 12.0,
            lg: 16.0,
            xl: 20.0,
            xxl: 24.0,
            xxxl: 32.0,
        }
    }
}

/// Font role bindings. Each field is the [`egui::FontFamily`] to use for a
/// given UI role; defaults reference the named families registered by
/// [`crate::font::install`].
#[derive(Clone, Debug)]
pub struct Fonts {
    /// Default proportional UI text.
    pub text: egui::FontFamily,
    /// Default monospace (code).
    pub code: egui::FontFamily,
    /// Headings (semibold).
    pub heading: egui::FontFamily,
    /// Bold UI text.
    pub bold: egui::FontFamily,
    /// Medium-weight monospace.
    pub code_strong: egui::FontFamily,
    /// Brand / italic display.
    pub display: egui::FontFamily,
    /// Material Symbols regular.
    pub icon: egui::FontFamily,
    /// Material Symbols filled.
    pub icon_filled: egui::FontFamily,
}

impl Default for Fonts {
    fn default() -> Self {
        Self {
            text: egui::FontFamily::Proportional,
            code: egui::FontFamily::Monospace,
            heading: egui::FontFamily::Name(crate::font::HEADING_FAMILY.into()),
            bold: egui::FontFamily::Name(crate::font::BOLD_FAMILY.into()),
            code_strong: egui::FontFamily::Name(crate::font::CODE_STRONG_FAMILY.into()),
            display: egui::FontFamily::Name(crate::font::DISPLAY_FAMILY.into()),
            icon: egui::FontFamily::Name(crate::font::ICON_FAMILY.into()),
            icon_filled: egui::FontFamily::Name(crate::font::ICON_FAMILY_FILLED.into()),
        }
    }
}

/// Corner radii. IDE-feel default = all zero (sharp corners).
#[derive(Clone, Copy, Debug)]
pub struct Radii {
    pub sm: f32,
    pub default: f32,
    pub md: f32,
    pub lg: f32,
    pub xl: f32,
    pub full: f32,
}

impl Default for Radii {
    fn default() -> Self {
        Self {
            sm: 0.0,
            default: 0.0,
            md: 0.0,
            lg: 0.0,
            xl: 0.0,
            full: 0.0,
        }
    }
}
