use std::borrow::Cow;

use gpui::{AssetSource, Result, SharedString};

pub struct Assets;

const ICONS: [(&str, &[u8]); 5] = [
    (
        "icons/magpie.svg",
        include_bytes!("../assets/icons/magpie.svg"),
    ),
    (
        "icons/codex-color.svg",
        include_bytes!("../assets/icons/codex-color.svg"),
    ),
    (
        "icons/claudecode-color.svg",
        include_bytes!("../assets/icons/claudecode-color.svg"),
    ),
    (
        "icons/geminicli-color.svg",
        include_bytes!("../assets/icons/geminicli-color.svg"),
    ),
    (
        "icons/opencode.svg",
        include_bytes!("../assets/icons/opencode.svg"),
    ),
];

impl AssetSource for Assets {
    fn load(&self, path: &str) -> Result<Option<Cow<'static, [u8]>>> {
        Ok(ICONS
            .iter()
            .find(|(name, _)| *name == path)
            .map(|(_, bytes)| Cow::Borrowed(*bytes)))
    }

    fn list(&self, path: &str) -> Result<Vec<SharedString>> {
        Ok(ICONS
            .iter()
            .filter(|(name, _)| name.starts_with(path))
            .map(|(name, _)| (*name).into())
            .collect())
    }
}

pub const REFRESH: &[u8] = br#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24" fill="none" stroke="white" stroke-width="1.7" stroke-linecap="round" stroke-linejoin="round"><path d="M20 5v5h-5M4 19v-5h5"/><path d="M6.1 7a7.5 7.5 0 0 1 12.4-1L20 10M4 14l1.5 4A7.5 7.5 0 0 0 18 17"/></svg>"#;
pub const CHEVRON: &[u8] = br#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24" fill="none" stroke="white" stroke-width="1.7" stroke-linecap="round" stroke-linejoin="round"><path d="m9 5 7 7-7 7"/></svg>"#;
pub const SETTINGS: &[u8] = br#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24" fill="none" stroke="white" stroke-width="1.7" stroke-linecap="round" stroke-linejoin="round"><path d="m10 3-.8 2.2-2 .9-2.2-.5-2 3.4L4.7 11v2L3 15l2 3.4 2.2-.5 2 .9L10 21h4l.8-2.2 2-.9 2.2.5 2-3.4-1.7-2v-2L21 9l-2-3.4-2.2.5-2-.9L14 3z"/><circle cx="12" cy="12" r="3"/></svg>"#;
pub const TERMINAL: &[u8] = br#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24" fill="none" stroke="white" stroke-width="1.7" stroke-linecap="round" stroke-linejoin="round"><rect x="2" y="3" width="20" height="18" rx="4"/><path d="m6 8 4 4-4 4m7 0h5"/></svg>"#;
