use gpui::{Window, WindowAppearance};
use magpie_ui_widgets::{InputTheme, UiAppearance};
#[derive(Clone, Copy)]
pub(crate) struct Palette {
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
}

impl Palette {
    pub fn for_window(window: &Window, appearance: UiAppearance) -> Self {
        let dark = match appearance {
            UiAppearance::System => matches!(
                window.appearance(),
                WindowAppearance::Dark | WindowAppearance::VibrantDark
            ),
            UiAppearance::Light => false,
            UiAppearance::Dark => true,
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
        }
    }
}

impl From<Palette> for InputTheme {
    fn from(p: Palette) -> Self {
        Self {
            surface: p.surface,
            inset: p.inset,
            border: p.border,
            text: p.text,
            muted: p.muted,
            accent: p.accent,
            selected: p.selected,
        }
    }
}
