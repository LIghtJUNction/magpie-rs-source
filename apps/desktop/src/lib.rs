//! Native Magpie desktop presentation shared with the offscreen review tool.

mod actions_text;
mod agent_connections;
mod assets;
mod cli_bridge;
mod extension_pages;
mod gateway_page;
mod gateway_text;
mod localization;
mod plugin_bundle;
mod plugin_catalog;
mod plugins_page;
mod preferences;
mod provider_text;
mod providers;
mod shell;
mod text_input;
mod theme;

pub use assets::Assets;
pub use localization::Locale;
pub use shell::MagpieWindow;
pub use theme::{Appearance, font_for_families, init_material_theme};

#[cfg(feature = "visual-test")]
pub use gateway_page::GatewayPage;
#[cfg(feature = "visual-test")]
pub use providers::ProvidersPage;
