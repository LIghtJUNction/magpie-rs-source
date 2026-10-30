//! Native Magpie desktop presentation shared with the offscreen review tool.

mod actions_state;
mod actions_text;
mod assets;
mod cli_bridge;
mod config_stamp;
mod gateway_page;
mod gateway_text;
mod input_model;
mod localization;
mod preferences;
mod provider_text;
mod providers;
mod snapshot;
mod text_input;
mod theme;
mod ui;

pub use assets::Assets;
pub use ui::MagpieWindow;

#[cfg(feature = "visual-test")]
pub use gateway_page::GatewayPage;
#[cfg(feature = "visual-test")]
pub use providers::ProvidersPage;
