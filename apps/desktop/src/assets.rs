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
        let own = ICONS
            .iter()
            .find(|(name, _)| *name == path)
            .map(|(_, bytes)| Cow::Borrowed(*bytes));
        match own {
            Some(bytes) => Ok(Some(bytes)),
            None => gpui_kit_material::icons::Assets::new().load(path),
        }
    }

    fn list(&self, path: &str) -> Result<Vec<SharedString>> {
        Ok(ICONS
            .iter()
            .filter(|(name, _)| name.starts_with(path))
            .map(|(name, _)| (*name).into())
            .collect())
    }
}

pub const CHEVRON: &[u8] = br#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24" fill="none" stroke="white" stroke-width="1.7" stroke-linecap="round" stroke-linejoin="round"><path d="m9 5 7 7-7 7"/></svg>"#;
