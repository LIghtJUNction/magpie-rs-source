use gpui::{Window, WindowAppearance};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Appearance {
    #[default]
    System,
    Light,
    Dark,
}

/// Opaque color tokens: no blur, gradients, animation, or extra GPU effects.
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
        if dark {
            Self {
                background: 0x171b19,
                surface: 0x202622,
                inset: 0x29312c,
                border: 0x384139,
                text: 0xeef1eb,
                muted: 0xacb7ae,
                accent: 0xa8ceb4,
                selected: 0x2c4034,
                warning: 0xf3c784,
                warning_bg: 0x3c3020,
                danger: 0xffb5a9,
                danger_bg: 0x432d29,
            }
        } else {
            Self {
                background: 0xf6f5f2,
                surface: 0xffffff,
                inset: 0xf1f0ec,
                border: 0xe0ded8,
                text: 0x232420,
                muted: 0x62665f,
                accent: 0x32634a,
                selected: 0xe6eee8,
                warning: 0x7c4d14,
                warning_bg: 0xfff1d9,
                danger: 0x9a3729,
                danger_bg: 0xfceae5,
            }
        }
    }
}
