use gpui::{Window, WindowAppearance};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Appearance {
    #[default]
    System,
    Light,
    Dark,
}

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
        if dark {
            Self {
                background: 0x1b1b1f,
                surface: 0x232327,
                inset: 0x2c2b31,
                border: 0x37363d,
                text: 0xeae9ee,
                muted: 0xa4a1ad,
                accent: 0xc5b4ee,
                selected: 0x373041,
                warning: 0xe8ba80,
            }
        } else {
            Self {
                background: 0xf5f5f7,
                surface: 0xffffff,
                inset: 0xf0f0f3,
                border: 0xe1e0e6,
                text: 0x25242b,
                muted: 0x6f6c79,
                accent: 0x7148a5,
                selected: 0xeee8f6,
                warning: 0x8e591e,
            }
        }
    }
}
