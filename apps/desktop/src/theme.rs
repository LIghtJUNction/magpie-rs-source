use std::sync::OnceLock;

use gpui::{App, Font, FontFallbacks, Window, WindowAppearance, font};

use crate::localization::Locale;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Appearance {
    #[default]
    System,
    Light,
    Dark,
}

/// Application surfaces use the same Material semantic scheme as the controls.
#[derive(Clone, Copy)]
pub struct Palette {
    pub background: u32,
    pub surface: u32,
    pub inset: u32,
    pub border: u32,
    pub text: u32,
    pub muted: u32,
    pub accent: u32,
    pub selected: u32,
    pub warning: u32,
    pub warning_bg: u32,
    pub danger: u32,
    pub danger_bg: u32,
}

impl Palette {
    pub fn for_window(window: &Window, appearance: Appearance) -> Self {
        let dark = match appearance {
            Appearance::System => matches!(
                window.appearance(),
                WindowAppearance::Dark | WindowAppearance::VibrantDark
            ),
            Appearance::Light => false,
            Appearance::Dark => true,
        };
        use gpui_kit_material::color::{
            ColorRole as C,
            themes::{Contrast, ThemeId, ThemeMode},
        };
        let scheme = ThemeId::Neutral.scheme(
            if dark {
                ThemeMode::Dark
            } else {
                ThemeMode::Light
            },
            Contrast::Standard,
        );
        let color = |role| scheme.get(role).to_argb() & 0xffffff;
        Self {
            background: color(C::SURFACE),
            surface: color(C::SURFACE_CONTAINER_LOW),
            inset: color(C::SURFACE_CONTAINER_HIGH),
            border: color(C::OUTLINE_VARIANT),
            text: color(C::ON_SURFACE),
            muted: color(C::ON_SURFACE_VARIANT),
            accent: color(C::PRIMARY),
            selected: color(C::SECONDARY_CONTAINER),
            warning: color(C::WARNING),
            warning_bg: color(C::WARNING_BG),
            danger: color(C::ERROR),
            danger_bg: color(C::ERROR_CONTAINER),
        }
    }
}

/// Use concrete families that this GPUI text backend actually enumerated.
///
/// A CSS comma-separated stack is one (usually missing) family in GPUI. The
/// primary face and explicit CJK/emoji fallbacks therefore remain separate.
/// Font enumeration happens once for this desktop process, not on each paint;
/// install system fonts before launching the app, then restart after changes.
pub fn ui_font(cx: &App, locale: Locale) -> Font {
    static FONTS: OnceLock<[Font; 5]> = OnceLock::new();
    let fonts = FONTS.get_or_init(|| {
        let families = cx.text_system().all_font_names();
        Locale::ALL.map(|locale| font_for_families(&families, locale))
    });
    let index = match locale {
        Locale::English => 0,
        Locale::SimplifiedChinese => 1,
        Locale::TraditionalChinese => 2,
        Locale::Japanese => 3,
        Locale::German => 4,
    };
    fonts[index].clone()
}

/// Shared with the standalone native font probe; never invokes Fontconfig or
/// an external process. Unknown family names are not passed to the shaper.
pub fn font_for_families(families: &[String], locale: Locale) -> Font {
    let find = |name: &str| families.iter().find(|family| family.as_str() == name);
    let primary_candidates: &[&str] = if cfg!(target_os = "windows") {
        &[
            "Segoe UI Variable Text",
            "Segoe UI",
            "Arial",
            "Liberation Sans",
        ]
    } else if cfg!(target_os = "macos") {
        &["SF Pro Text", "Helvetica Neue", "Helvetica", "Arial"]
    } else {
        &[
            "Inter",
            "Adwaita Sans",
            "Ubuntu Sans",
            "Cantarell",
            "Ubuntu",
            "Noto Sans",
            "DejaVu Sans",
            "Liberation Sans",
            "Arial",
            "Noto Sans CJK SC",
            "Noto Sans SC",
            "Noto Sans CJK TC",
            "Noto Sans TC",
            "Noto Sans CJK JP",
            "Noto Sans JP",
            "WenQuanYi Micro Hei",
        ]
    };
    let primary = primary_candidates.iter().find_map(|name| find(name));
    // The reserved platform alias is the final fallback only when no named
    // sans family is available. It is interpreted by GPUI, not as a CSS name.
    let mut result = font(primary.map_or(".SystemUIFont", String::as_str).to_owned());
    let simplified = [
        "Noto Sans CJK SC",
        "Noto Sans SC",
        "Source Han Sans SC",
        "Source Han Sans CN",
        "Microsoft YaHei UI",
        "Microsoft YaHei",
        "PingFang SC",
        "WenQuanYi Micro Hei",
        "WenQuanYi Zen Hei",
    ];
    let traditional = [
        "Noto Sans CJK TC",
        "Noto Sans TC",
        "Source Han Sans TC",
        "Microsoft JhengHei UI",
        "Microsoft JhengHei",
        "PingFang TC",
        "Heiti TC",
    ];
    let japanese = [
        "Noto Sans CJK JP",
        "Noto Sans JP",
        "Source Han Sans JP",
        "Yu Gothic UI",
        "Yu Gothic",
        "Hiragino Sans",
        "Hiragino Kaku Gothic ProN",
        "Meiryo",
        "IPAexGothic",
        "IPAGothic",
    ];
    let groups: [&[&str]; 3] = match locale {
        Locale::TraditionalChinese => [&traditional, &simplified, &japanese],
        Locale::Japanese => [&japanese, &simplified, &traditional],
        _ => [&simplified, &traditional, &japanese],
    };
    let mut fallbacks = Vec::new();
    for group in groups {
        // One installed sans family per language is sufficient; use the
        // preferred locale first to avoid borrowing another region's glyphs.
        if let Some(family) = group.iter().find_map(|name| find(name))
            && family.as_str() != result.family.as_ref()
            && !fallbacks.contains(family)
        {
            fallbacks.push(family.clone());
        }
    }
    if let Some(emoji) = ["Apple Color Emoji", "Segoe UI Emoji", "Noto Color Emoji"]
        .into_iter()
        .find_map(find)
    {
        fallbacks.push(emoji.clone());
    }
    if !fallbacks.is_empty() {
        result.fallbacks = Some(FontFallbacks::from_fonts(fallbacks));
    }
    result
}

/// Initialize Material and Kit colors before their first root is rendered.
pub fn init_material_theme(window: Option<&Window>, cx: &mut App) {
    use gpui_kit_material::{
        color::themes::{Contrast, ThemeId, ThemeMode},
        theme,
    };
    let dark = window.map_or_else(
        || theme::system_dark(cx),
        |window| {
            matches!(
                window.appearance(),
                WindowAppearance::Dark | WindowAppearance::VibrantDark
            )
        },
    );
    let mode = if dark {
        ThemeMode::Dark
    } else {
        ThemeMode::Light
    };
    theme::init_component_theme(ThemeId::Neutral.scheme(mode, Contrast::Standard), 1.0, cx);
}

pub(crate) fn apply_material_theme(appearance: Appearance, window: &mut Window, cx: &mut App) {
    use gpui_kit_material::{
        color::themes::{Contrast, ThemeId, ThemeMode},
        theme,
    };
    let dark = match appearance {
        Appearance::System => matches!(
            window.appearance(),
            WindowAppearance::Dark | WindowAppearance::VibrantDark
        ),
        Appearance::Dark => true,
        Appearance::Light => false,
    };
    if cx
        .try_global::<AppliedMaterialMode>()
        .is_some_and(|mode| mode.0 == dark)
    {
        return;
    }
    cx.set_global(AppliedMaterialMode(dark));
    let scheme = ThemeId::Neutral.scheme(
        if dark {
            ThemeMode::Dark
        } else {
            ThemeMode::Light
        },
        Contrast::Standard,
    );
    theme::init_component_theme(scheme, 1.0, cx);
    window.refresh();
}

struct AppliedMaterialMode(bool);
impl gpui::Global for AppliedMaterialMode {}

impl From<Palette> for magpie_ui_widgets::InputTheme {
    fn from(palette: Palette) -> Self {
        Self {
            surface: palette.surface,
            inset: palette.inset,
            border: palette.border,
            text: palette.text,
            muted: palette.muted,
            accent: palette.accent,
            selected: palette.selected,
        }
    }
}
