//! The shell owns page lifecycle; plugin packages own page implementations.
use crate::{localization::Locale, theme::Appearance};
use gpui::{AnyView, App, FocusHandle, SharedString, Window};
use std::{
    collections::BTreeMap,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};

type Presentation = Box<dyn Fn(Locale, Appearance, &mut App)>;
type Focus = Box<dyn Fn(&mut Window, &mut App)>;
type AppCallback = Box<dyn Fn(&mut App)>;

pub(crate) struct ExtensionPage {
    pub id: u32,
    pub owner: String,
    pub key: String,
    pub nav_id: SharedString,
    pub icon: &'static str,
    pub view: AnyView,
    pub navigation_focus: FocusHandle,
    pub title: Box<dyn Fn(Locale) -> SharedString>,
    pub intro: Box<dyn Fn(Locale) -> SharedString>,
    pub present: Presentation,
    pub refresh: AppCallback,
    pub focus: Focus,
    pub cancel: AppCallback,
    pub pending: Arc<AtomicBool>,
    #[cfg(feature = "visual-test")]
    #[allow(dead_code)] // Used by the standalone example including this module.
    pub preview: AppCallback,
}

#[derive(Default)]
pub(crate) struct ExtensionPages {
    pub pages: Vec<ExtensionPage>,
    ids: BTreeMap<(String, String), u32>,
    next: u32,
    retired: Vec<Arc<AtomicBool>>,
}
impl ExtensionPages {
    pub fn id_for(&mut self, owner: &str, key: &str) -> u32 {
        *self
            .ids
            .entry((owner.to_owned(), key.to_owned()))
            .or_insert_with(|| {
                self.next += 1;
                self.next
            })
    }
    pub fn get(&self, id: u32) -> Option<&ExtensionPage> {
        self.pages.iter().find(|page| page.id == id)
    }
    pub fn has(&self, owner: &str, key: &str) -> bool {
        self.pages
            .iter()
            .any(|page| page.owner == owner && page.key == key)
    }
    pub fn retain(&mut self, keep: impl Fn(&ExtensionPage) -> bool, cx: &mut App) {
        let mut surviving = Vec::new();
        for page in self.pages.drain(..) {
            if keep(&page) {
                surviving.push(page);
            } else {
                (page.cancel)(cx);
                self.retired.push(page.pending.clone());
            }
        }
        self.pages = surviving;
        self.retired
            .retain(|pending| pending.load(Ordering::Acquire));
    }
    pub fn cancel_all(&self, cx: &mut App) {
        for page in &self.pages {
            (page.cancel)(cx);
        }
    }
    pub fn pending_flags(&self) -> Vec<Arc<AtomicBool>> {
        self.pages
            .iter()
            .map(|page| page.pending.clone())
            .chain(self.retired.iter().cloned())
            .collect()
    }
    pub fn pending(&self) -> bool {
        self.pending_flags()
            .iter()
            .any(|pending| pending.load(Ordering::Acquire))
    }
}
