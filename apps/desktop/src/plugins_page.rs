//! Core plugin management: registration state, enablement and withdrawal.
#[path = "plugin_text.rs"]
mod plugin_text;
use plugin_text::PluginText as T;

use crate::{
    cli_bridge::{self, Cancellation, Reply, Request},
    localization::Locale,
    plugin_catalog::{Catalog, PluginSource},
    text_input::{InputEvent, TextInput},
    theme::{Appearance, Palette},
};
use gpui::{
    Context, Entity, EventEmitter, FontWeight, Render, Role, ScrollHandle, Subscription, Window,
    div, prelude::*, px, rgb, text,
};
use gpui_kit_material::components::{ButtonSize, ButtonVariant, material_button};
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};

pub(crate) type CatalogLoader = fn() -> Result<Catalog, ()>;
pub(crate) struct RegistryChanged;
pub(crate) struct PluginsPage {
    catalog: Option<Catalog>,
    loader: CatalogLoader,
    locale: Locale,
    appearance: Appearance,
    busy: bool,
    closing: bool,
    error: bool,
    cancellation: Option<Cancellation>,
    pending: Arc<AtomicBool>,
    search: Entity<TextInput>,
    query: String,
    scroll: ScrollHandle,
    _subscription: Subscription,
}
impl EventEmitter<RegistryChanged> for PluginsPage {}
impl PluginsPage {
    pub fn new(loader: CatalogLoader, cx: &mut Context<Self>) -> Self {
        let search = cx.new(|cx| TextInput::new("plugin-search", "", "", "", cx));
        let subscription = cx.subscribe(&search, |view, input, event, cx| {
            if *event == InputEvent::Changed {
                view.query = input.read(cx).text().to_lowercase();
                cx.notify();
            }
        });
        Self {
            catalog: None,
            loader,
            locale: Locale::system(),
            appearance: Appearance::System,
            busy: false,
            closing: false,
            error: false,
            cancellation: None,
            pending: Arc::new(AtomicBool::new(false)),
            search,
            query: String::new(),
            scroll: ScrollHandle::new(),
            _subscription: subscription,
        }
    }
    pub fn catalog(&self) -> Option<&Catalog> {
        self.catalog.as_ref()
    }
    #[cfg(feature = "visual-test")]
    pub(crate) fn preview_fixture(&mut self, catalog: Catalog, cx: &mut Context<Self>) {
        self.catalog = Some(catalog);
        self.busy = false;
        self.error = false;
        cx.emit(RegistryChanged);
        cx.notify();
    }
    pub fn busy(&self) -> bool {
        self.busy
    }
    pub(crate) fn set_closing(&mut self, closing: bool, cx: &mut Context<Self>) {
        self.closing = closing;
        if closing {
            self.cancel();
        } else {
            self.refresh(cx);
        }
    }
    pub fn shutdown_pending(&self) -> Arc<AtomicBool> {
        self.pending.clone()
    }
    pub fn cancel(&self) {
        if let Some(cancellation) = &self.cancellation {
            cancellation.cancel();
        }
    }
    pub fn set_presentation(
        &mut self,
        locale: Locale,
        appearance: Appearance,
        cx: &mut Context<Self>,
    ) {
        if self.locale != locale || self.appearance != appearance {
            self.locale = locale;
            self.appearance = appearance;
            cx.notify();
        }
    }
    pub fn refresh(&mut self, cx: &mut Context<Self>) {
        self.start(None, cx);
    }
    /// Observe the small enablement file; installing/changing packages can also
    /// be refreshed explicitly. Watching never opens business or account data.
    pub fn watch(&mut self, cx: &mut Context<Self>) {
        let Ok(path) = magpie_core::plugins::PluginRegistry::default_state_path() else {
            return;
        };
        cx.spawn(async move |this, cx| {
            let mut previous = None;
            let mut initialized = false;
            loop {
                async_io::Timer::after(std::time::Duration::from_millis(750)).await;
                let path = path.clone();
                let stamp = cx
                    .background_executor()
                    .spawn(async move {
                        std::fs::symlink_metadata(path)
                            .ok()
                            .map(|metadata| (metadata.len(), metadata.modified().ok()))
                    })
                    .await;
                let changed = !initialized || stamp != previous;
                previous = stamp;
                initialized = true;
                if this
                    .update(cx, |view, cx| {
                        if changed {
                            view.refresh(cx);
                        }
                    })
                    .is_err()
                {
                    break;
                }
            }
        })
        .detach();
    }
    fn start(&mut self, toggle: Option<(String, bool, u64)>, cx: &mut Context<Self>) {
        if self.busy || self.closing {
            return;
        }
        self.busy = true;
        self.pending.store(true, Ordering::Release);
        let cancellation = Cancellation::new();
        self.cancellation = Some(cancellation.clone());
        let pending = self.pending.clone();
        let loader = self.loader;
        let operation = cx.background_executor().spawn(async move {
            let result = if let Some((owner, enabled, revision)) = toggle {
                match cli_bridge::run(
                    Request::PluginToggle {
                        owner,
                        enabled,
                        revision,
                    },
                    cancellation,
                )
                .await
                {
                    Ok(Reply::Plugins(value)) => {
                        Catalog::parse(value).map(|catalog| (catalog, false))
                    }
                    _ => loader().map(|catalog| (catalog, true)),
                }
            } else {
                loader().map(|catalog| (catalog, false))
            };
            pending.store(false, Ordering::Release);
            result
        });
        cx.spawn(async move |this, cx| {
            let result = operation.await;
            let _ = this.update(cx, |view, cx| {
                view.busy = false;
                view.cancellation = None;
                match result {
                    Ok((catalog, error)) => {
                        view.catalog = Some(catalog);
                        view.error = error;
                    }
                    Err(()) => {
                        view.error = true;
                        view.catalog = None;
                    }
                }
                cx.emit(RegistryChanged);
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }
}
impl Drop for PluginsPage {
    fn drop(&mut self) {
        self.cancel();
    }
}
impl Render for PluginsPage {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let palette = Palette::for_window(window, self.appearance);
        let search_label = self.locale.plugin_text(T::Search);
        self.search.update(cx, |input, cx| {
            input.set_presentation(search_label, search_label, palette, false, cx)
        });
        let entries = self
            .catalog
            .as_ref()
            .map(|c| c.plugins.clone())
            .unwrap_or_default();
        let revision = self.catalog.as_ref().map_or(0, |c| c.revision);
        let busy = self.busy;
        let rows = entries
            .iter()
            .filter(|record| {
                self.query.is_empty()
                    || [&record.id, &record.name, &record.kind]
                        .iter()
                        .any(|s| s.to_lowercase().contains(&self.query))
            })
            .map(|record| {
                let id = record.id.clone();
                let enabled = !record.enabled;
                let state = match record.status.as_str() {
                    "active" => self.locale.plugin_text(T::Active),
                    "off" => self.locale.plugin_text(T::Off),
                    "failed" => self.locale.plugin_text(T::Failed),
                    "pending" => self.locale.plugin_text(T::Pending),
                    "unsupported" => self.locale.plugin_text(T::Unsupported),
                    _ => self.locale.plugin_text(T::Unknown),
                };
                div()
                    .id(gpui::SharedString::from(format!(
                        "plugin-row:{}",
                        record.id
                    )))
                    .py(px(14.))
                    .flex()
                    .flex_wrap()
                    .items_center()
                    .gap(px(12.))
                    .child(
                        div()
                            .flex_1()
                            .min_w(px(160.))
                            .flex()
                            .flex_col()
                            .gap(px(4.))
                            .child(
                                div()
                                    .font_weight(FontWeight::MEDIUM)
                                    .child(text!(record.name.clone())),
                            )
                            .child(
                                div()
                                    .text_size(px(11.))
                                    .text_color(rgb(palette.muted))
                                    .child(text!(format!(
                                        "{} · {} · {}",
                                        record.id, record.version, record.kind
                                    ))),
                            )
                            .child(
                                div()
                                    .flex()
                                    .flex_wrap()
                                    .items_center()
                                    .gap(px(8.))
                                    .child(
                                        div()
                                            .text_size(px(10.))
                                            .px(px(6.))
                                            .py(px(2.))
                                            .rounded(px(4.))
                                            .bg(rgb(palette.selected))
                                            .child(text!(source_badge(
                                                self.locale,
                                                &record.source
                                            ))),
                                    )
                                    .child(
                                        div()
                                            .text_size(px(12.))
                                            .text_color(rgb(palette.muted))
                                            .child(text!(state)),
                                    ),
                            )
                            .when_some(source_details(&record.source), |row, source| {
                                row.child(
                                    div()
                                        .text_size(px(11.))
                                        .text_color(rgb(palette.muted))
                                        .whitespace_normal()
                                        .child(text!(source)),
                                )
                            }),
                    )
                    .child(
                        material_button(
                            gpui::SharedString::from(format!("plugin-toggle:{}", record.id)),
                            ButtonVariant::Text,
                        )
                        .button_size(ButtonSize::Small)
                        .disabled(busy)
                        .on_click(cx.listener(move |view, _, _, cx| {
                            view.start(Some((id.clone(), enabled, revision)), cx)
                        }))
                        .child(text!(if enabled {
                            self.locale.plugin_text(T::Enable)
                        } else {
                            self.locale.plugin_text(T::Disable)
                        })),
                    )
            })
            .collect::<Vec<_>>();
        let diagnostics = self
            .catalog
            .as_ref()
            .map(|catalog| catalog.diagnostics.as_slice())
            .unwrap_or_default();
        let diagnostic_rows = diagnostics
            .iter()
            .take(10)
            .enumerate()
            .map(|(index, diagnostic)| {
                div()
                    .id(gpui::SharedString::from(format!(
                        "plugin-diagnostic:{index}"
                    )))
                    .py(px(8.))
                    .flex()
                    .flex_col()
                    .gap(px(4.))
                    .child(
                        div()
                            .flex()
                            .flex_wrap()
                            .items_center()
                            .gap(px(8.))
                            .child(
                                div()
                                    .text_size(px(10.))
                                    .px(px(6.))
                                    .py(px(2.))
                                    .rounded(px(4.))
                                    .bg(rgb(palette.selected))
                                    .child(text!(source_badge(self.locale, &diagnostic.source))),
                            )
                            .when_some(source_details(&diagnostic.source), |row, source| {
                                row.child(
                                    div()
                                        .text_size(px(11.))
                                        .text_color(rgb(palette.muted))
                                        .whitespace_normal()
                                        .child(text!(source)),
                                )
                            }),
                    )
                    .child(
                        div()
                            .text_size(px(12.))
                            .text_color(rgb(palette.warning))
                            .whitespace_normal()
                            .child(text!(diagnostic.error.clone())),
                    )
            })
            .collect::<Vec<_>>();
        let diagnostic_section = div()
            .id("plugin-registration-diagnostics")
            .flex()
            .flex_col()
            .gap(px(4.))
            .when(!diagnostics.is_empty(), |section| {
                section
                    .child(div().font_weight(FontWeight::MEDIUM).child(text!(format!(
                        "{}: {} · {} {}/{}",
                        self.locale.plugin_text(T::Diagnostics),
                        diagnostics.len(),
                        self.locale.plugin_text(T::DiagnosticsShown),
                        diagnostics.len().min(10),
                        diagnostics.len()
                    ))))
                    .child(
                        div()
                            .text_size(px(11.))
                            .text_color(rgb(palette.muted))
                            .whitespace_normal()
                            .child(text!(self.locale.plugin_text(T::DiagnosticsHint))),
                    )
                    .children(diagnostic_rows)
            });
        div()
            .size_full()
            .min_w_0()
            .flex()
            .flex_col()
            .gap(px(14.))
            .child(
                div()
                    .flex()
                    .flex_wrap()
                    .items_center()
                    .gap(px(12.))
                    .child(div().flex_1().min_w(px(150.)).child(self.search.clone()))
                    .child(
                        material_button("plugins-refresh", ButtonVariant::Text)
                            .button_size(ButtonSize::Small)
                            .disabled(busy)
                            .on_click(cx.listener(|view, _, _, cx| view.refresh(cx)))
                            .child(text!(self.locale.plugin_text(T::Refresh))),
                    ),
            )
            .when(busy, |root| {
                root.child(
                    div()
                        .id("plugin-read-status")
                        .role(Role::Status)
                        .text_color(rgb(palette.muted))
                        .child(text!(self.locale.plugin_text(T::Loading))),
                )
            })
            .when(self.error, |root| {
                root.child(
                    div()
                        .id("plugin-error-status")
                        .role(Role::Status)
                        .text_color(rgb(palette.warning))
                        .whitespace_normal()
                        .child(text!(self.locale.plugin_text(T::Error))),
                )
            })
            .child(
                div()
                    .id("plugin-list")
                    .flex_1()
                    .min_h_0()
                    .min_w_0()
                    .overflow_y_scroll()
                    .track_scroll(&self.scroll)
                    .child(diagnostic_section)
                    .children(rows),
            )
            .child(
                div()
                    .text_size(px(11.))
                    .text_color(rgb(palette.muted))
                    .whitespace_normal()
                    .child(text!(self.locale.plugin_text(T::Hint))),
            )
    }
}

fn source_badge(locale: Locale, source: &PluginSource) -> &'static str {
    locale.plugin_text(match source.kind.as_str() {
        "bundled" => T::Bundled,
        "user" => T::Local,
        _ => T::UnknownSource,
    })
}

fn source_details(source: &PluginSource) -> Option<String> {
    let label = source.label.as_deref();
    let path = (source.kind == "user")
        .then_some(source.path.as_deref())
        .flatten();
    match (label, path) {
        (Some(label), Some(path)) if label != path => Some(format!("{label} · {path}")),
        (Some(label), _) => Some(label.to_owned()),
        (_, Some(path)) => Some(path.to_owned()),
        _ => None,
    }
}
