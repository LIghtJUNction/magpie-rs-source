//! Application composition. Optional packages register with the generic shell here.
use crate::{
    cli_bridge::{self, Cancellation, Reply, Request},
    extension_pages::{ExtensionPage, ExtensionPages},
    plugin_catalog::Catalog,
    theme::Appearance,
};
use gpui::{App, AppContext};
use magpie_app_plugins::{AppPlugins, FeatureRegistration};
use magpie_core::{plugins::PluginInput, provider_store::ProviderStore};
use magpie_plugin_usage_ui::{UiAppearance, UsageLoadError, UsagePage};
use std::sync::Arc;

const USAGE_OWNER: &str = "feature.usage";
const USAGE_MANIFEST: &str = include_str!("../../../crates/magpie-plugin-usage/plugin.toml");

pub(crate) fn load_catalog() -> Result<Catalog, ()> {
    let features = [FeatureRegistration {
        input: PluginInput::bundled(USAGE_OWNER, USAGE_MANIFEST),
        commands: vec![("usage".into(), "usage.report".into())],
        pages: vec![("usage".into(), "usage".into())],
        metadata_subscribers: vec![("usage".into(), "request_finished".into())],
    }];
    // The application has linked both implementations. The shared data bundle
    // itself has no dependency on either vendor's executable adapter.
    let engines = [
        magpie_plugin_agent_codex::ENGINE.to_owned(),
        magpie_plugin_agent_discovery::ENGINE.to_owned(),
    ];
    let app = AppPlugins::load_with_engines(&features, &engines).map_err(|_| ())?;
    Catalog::parse(app.to_json())
}

pub(crate) fn reconcile(pages: &mut ExtensionPages, catalog: Option<&Catalog>, cx: &mut App) {
    let contributed = catalog
        .map(|catalog| catalog.contributions.pages.as_slice())
        .unwrap_or_default();
    let implemented = |owner: &str, key: &str, command: &str| {
        owner == USAGE_OWNER && key == "usage" && command == "usage"
    };
    pages.retain(
        |page| {
            contributed.iter().any(|entry| {
                entry.owner == page.owner
                    && entry.id == page.key
                    && implemented(&entry.owner, &entry.id, &entry.command)
            })
        },
        cx,
    );
    for entry in contributed {
        if !implemented(&entry.owner, &entry.id, &entry.command)
            || pages.has(&entry.owner, &entry.id)
        {
            continue;
        }
        let id = pages.id_for(&entry.owner, &entry.id);
        let loader: magpie_plugin_usage_ui::Loader = Arc::new(|flag| {
            Box::pin(async move {
                let store =
                    ProviderStore::default_path().map_err(|_| UsageLoadError::Unavailable)?;
                let request = Request::PluginCall {
                    owner: USAGE_OWNER.into(),
                    operation: "report".into(),
                    arguments: vec![
                        "--limit".into(),
                        "100".into(),
                        "--store".into(),
                        store.to_string_lossy().into_owned(),
                    ],
                    mutation: false,
                };
                match cli_bridge::run(request, Cancellation::from_flag(flag)).await {
                    Ok(Reply::PluginResult(value)) => Ok(value),
                    Err(error) if error.kind == cli_bridge::ErrorKind::Cancelled => {
                        Err(UsageLoadError::Cancelled)
                    }
                    _ => Err(UsageLoadError::Unavailable),
                }
            })
        });
        let page = cx.new(|cx| UsagePage::new(cx, loader));
        let pending = page.read(cx).shutdown_pending();
        let refresh = page.clone();
        let focus = page.clone();
        let present = page.clone();
        let cancel = page.clone();
        #[cfg(feature = "visual-test")]
        let preview = page.clone();
        pages.pages.push(ExtensionPage {
            id,
            owner: entry.owner.clone(),
            key: entry.id.clone(),
            nav_id: format!("plugin-page:{}:{}", entry.owner, entry.id).into(),
            icon: "analytics",
            view: page.into(),
            navigation_focus: cx.focus_handle().tab_stop(true),
            title: Box::new(|locale| magpie_plugin_usage_ui::label(locale.language_tag()).into()),
            intro: Box::new(|locale| magpie_plugin_usage_ui::intro(locale.language_tag()).into()),
            present: Box::new(move |locale, appearance, cx| {
                present.update(cx, |page, cx| {
                    page.set_presentation(
                        locale.language_tag(),
                        match appearance {
                            Appearance::System => UiAppearance::System,
                            Appearance::Light => UiAppearance::Light,
                            Appearance::Dark => UiAppearance::Dark,
                        },
                        cx,
                    )
                })
            }),
            refresh: Box::new(move |cx| refresh.update(cx, |page, cx| page.refresh(cx))),
            focus: Box::new(move |window, cx| focus.update(cx, |page, cx| page.focus(window, cx))),
            cancel: Box::new(move |cx| cancel.read(cx).cancel()),
            pending,
            #[cfg(feature = "visual-test")]
            preview: Box::new(move |cx| preview.update(cx, |page, cx| page.preview_fixture(cx))),
        });
    }
}

#[cfg(feature = "visual-test")]
#[allow(dead_code)] // Used by the standalone example including this module.
pub(crate) fn preview_usage(pages: &mut ExtensionPages, cx: &mut App) -> Option<u32> {
    // Fixture registration exercises the same factory without filesystem or CLI access.
    let mut catalog = Catalog {
        version: "0.2.0".into(),
        revision: 0,
        plugins: Vec::new(),
        contributions: Default::default(),
        diagnostics: Vec::new(),
    };
    catalog
        .contributions
        .pages
        .push(crate::plugin_catalog::PageContribution {
            owner: USAGE_OWNER.into(),
            id: "usage".into(),
            name: "Usage".into(),
            command: "usage".into(),
        });
    reconcile(pages, Some(&catalog), cx);
    let entry = pages
        .pages
        .iter()
        .find(|entry| entry.owner == USAGE_OWNER)?;
    (entry.preview)(cx);
    Some(entry.id)
}
