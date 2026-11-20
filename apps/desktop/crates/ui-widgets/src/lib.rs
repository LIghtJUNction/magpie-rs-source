//! Shared native widgets, independent of the desktop shell and feature plugins.
mod input_model;
mod text_input;
mod theme;

pub use text_input::{InputEvent, InputTheme, TextInput};
pub use theme::workspace_scheme;

/// Window appearance preference shared by independently compiled UI plugins.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum UiAppearance {
    #[default]
    System,
    Light,
    Dark,
}
