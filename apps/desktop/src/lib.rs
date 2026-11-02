//! Native Magpie desktop presentation shared with the offscreen review tool.

mod actions_state;
mod actions_text;
mod assets;
mod cli_bridge;
mod config_stamp;
mod gateway_process;
mod input_model;
mod localization;
mod provider_bridge;
mod provider_state;
mod provider_text;
mod provider_ui;
mod snapshot;
mod text_input;
mod theme;
mod ui;

pub use assets::Assets;
pub use ui::MagpieWindow;

pub use theme::init_material_theme;
