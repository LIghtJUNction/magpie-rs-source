//! Independently compiled Usage UI with an explicit host data-loading capability.
mod page;
pub mod report;
mod theme;
mod usage_text;

pub use magpie_ui_widgets::UiAppearance;
pub use page::UsagePage;
use std::{
    future::Future,
    pin::Pin,
    sync::{Arc, atomic::AtomicBool},
};

/// An asynchronous host operation; the host must observe cancellation promptly.
pub type LoadFuture =
    Pin<Box<dyn Future<Output = Result<serde_json::Value, UsageLoadError>> + Send>>;
/// Host capability. The plugin never opens accounts, invokes CLI commands or reads files.
pub type Loader = Arc<dyn Fn(Arc<AtomicBool>) -> LoadFuture + Send + Sync>;

/// Static errors avoid displaying host paths, credentials or raw command output.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UsageLoadError {
    Unavailable,
    Cancelled,
    InvalidReport,
}
impl std::fmt::Display for UsageLoadError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Unavailable => "usage history unavailable",
            Self::Cancelled => "usage loading cancelled",
            Self::InvalidReport => "invalid usage report",
        })
    }
}
impl std::error::Error for UsageLoadError {}

/// Localized navigation label for the host's explicitly registered page factory.
pub fn label(locale_code: &str) -> &'static str {
    usage_text::Locale::parse(locale_code).usage_text(usage_text::UsageText::Title)
}

/// Localized description of the recorded forwarded-request history scope.
pub fn intro(locale_code: &str) -> &'static str {
    usage_text::Locale::parse(locale_code).usage_text(usage_text::UsageText::Intro)
}

#[cfg(test)]
mod label_tests {
    #[test]
    fn labels_use_plugin_locales_with_english_fallback() {
        for (code, expected) in [
            ("en", "Usage"),
            ("zh-CN", "用量"),
            ("zh-TW", "用量"),
            ("ja", "使用量"),
            ("de", "Nutzung"),
            ("unknown", "Usage"),
        ] {
            assert_eq!(super::label(code), expected, "{code}");
        }
        assert!(super::intro("en").contains("recorded forwarded requests"));
        assert_eq!(super::intro("unknown"), super::intro("en"));
    }
}
