//! Font registration — caller calls [`install`] once at startup before
//! [`crate::theme::set_theme`] to register the proportional, monospace, and
//! named families used by the theme and icon widgets.
//!
//! Multiple presets (proportional + mono pairings) are supported via [`Preset`].
//!
//! All font assets are embedded via `include_bytes!`, so no IO is needed.

use std::sync::Arc;

use egui::epaint::text::VariationCoords;

/// Family name for the regular Material Symbols icon font.
pub const ICON_FAMILY: &str = "lutin-icons";
/// Family name for the filled Material Symbols icon font.
pub const ICON_FAMILY_FILLED: &str = "lutin-icons-filled";

/// Family name for heading-weight UI text (semibold).
pub const HEADING_FAMILY: &str = "lutin-heading";
/// Family name for bold-weight UI text.
pub const BOLD_FAMILY: &str = "lutin-bold";
/// Family name for medium-weight monospace (code-strong).
pub const CODE_STRONG_FAMILY: &str = "lutin-code-strong";
/// Family name for the brand / italic display font.
pub const DISPLAY_FAMILY: &str = "lutin-display";

const INTER: &[u8] = include_bytes!("../assets/fonts/Inter.ttf");
const OUTFIT: &[u8] = include_bytes!("../assets/fonts/Outfit.ttf");
const JBM_REGULAR: &[u8] = include_bytes!("../assets/fonts/JetBrainsMono-Regular.ttf");
const JBM_MEDIUM: &[u8] = include_bytes!("../assets/fonts/JetBrainsMono-Medium.ttf");
const FRAUNCES: &[u8] = include_bytes!("../assets/fonts/Fraunces-Italic.ttf");
const ICONS_REGULAR: &[u8] = include_bytes!("../assets/fonts/MaterialSymbolsRounded-Regular.ttf");
const ICONS_FILLED: &[u8] =
    include_bytes!("../assets/fonts/MaterialSymbolsRounded_Filled-Regular.ttf");

/// Which proportional / mono pair to use as the default UI text + code font.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
pub enum Preset {
    /// Inter (proportional) + JetBrains Mono (code). Neutral, refined.
    #[default]
    Inter,
    /// Outfit (proportional) + JetBrains Mono (code). Geometric, more brand-y.
    Outfit,
}

/// Register fonts for the given preset (proportional + mono + named families
/// for headings, bold, display, code-strong, icons). Idempotent — safe to call
/// repeatedly; each call replaces the egui font definitions.
pub fn install(ctx: &egui::Context, preset: Preset) {
    let mut fonts = egui::FontDefinitions::default();

    // Static (non-axis) faces shared by all presets.
    fonts
        .font_data
        .insert("jbm-regular".into(), Arc::new(egui::FontData::from_static(JBM_REGULAR)));
    fonts
        .font_data
        .insert("jbm-medium".into(), Arc::new(egui::FontData::from_static(JBM_MEDIUM)));
    fonts
        .font_data
        .insert("fraunces".into(), Arc::new(egui::FontData::from_static(FRAUNCES)));
    fonts
        .font_data
        .insert("icons".into(), Arc::new(egui::FontData::from_static(ICONS_REGULAR)));
    fonts
        .font_data
        .insert("icons-filled".into(), Arc::new(egui::FontData::from_static(ICONS_FILLED)));

    // Preset-specific proportional faces.
    let prop_default: &str;
    let prop_heading: &str;
    let prop_bold: &str;
    match preset {
        Preset::Inter => {
            fonts.font_data.insert(
                "inter".into(),
                Arc::new(egui::FontData::from_static(INTER).tweak(egui::FontTweak {
                    coords: VariationCoords::new([(b"wght", 450.0)]),
                    ..Default::default()
                })),
            );
            fonts.font_data.insert(
                "inter-heading".into(),
                Arc::new(egui::FontData::from_static(INTER).tweak(egui::FontTweak {
                    coords: VariationCoords::new([(b"wght", 600.0)]),
                    ..Default::default()
                })),
            );
            fonts.font_data.insert(
                "inter-bold".into(),
                Arc::new(egui::FontData::from_static(INTER).tweak(egui::FontTweak {
                    coords: VariationCoords::new([(b"wght", 700.0)]),
                    ..Default::default()
                })),
            );
            prop_default = "inter";
            prop_heading = "inter-heading";
            prop_bold = "inter-bold";
        }
        Preset::Outfit => {
            fonts
                .font_data
                .insert("outfit".into(), Arc::new(egui::FontData::from_static(OUTFIT)));
            // Outfit ships as a single static face; heading + bold fall back to it.
            prop_default = "outfit";
            prop_heading = "outfit";
            prop_bold = "outfit";
        }
    }

    // Built-in proportional / monospace families.
    fonts
        .families
        .entry(egui::FontFamily::Proportional)
        .or_default()
        .insert(0, prop_default.into());
    fonts
        .families
        .entry(egui::FontFamily::Monospace)
        .or_default()
        .insert(0, "jbm-regular".into());

    // Named families. Each includes a fallback to the proportional default so
    // missing glyphs render as '?' rather than empty.
    fonts
        .families
        .entry(egui::FontFamily::Name(HEADING_FAMILY.into()))
        .or_default()
        .extend([prop_heading.into(), prop_default.into()]);
    fonts
        .families
        .entry(egui::FontFamily::Name(BOLD_FAMILY.into()))
        .or_default()
        .extend([prop_bold.into(), prop_default.into()]);
    fonts
        .families
        .entry(egui::FontFamily::Name(CODE_STRONG_FAMILY.into()))
        .or_default()
        .extend(["jbm-medium".into(), "jbm-regular".into()]);
    fonts
        .families
        .entry(egui::FontFamily::Name(DISPLAY_FAMILY.into()))
        .or_default()
        .extend(["fraunces".into(), prop_default.into()]);
    fonts
        .families
        .entry(egui::FontFamily::Name(ICON_FAMILY.into()))
        .or_default()
        .extend(["icons".into(), prop_default.into()]);
    fonts
        .families
        .entry(egui::FontFamily::Name(ICON_FAMILY_FILLED.into()))
        .or_default()
        .extend(["icons-filled".into(), prop_default.into()]);

    ctx.set_fonts(fonts);
}
