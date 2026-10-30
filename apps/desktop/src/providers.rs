//! Native Provider management backed by the same store used by CLI and gateway.
use std::{
    collections::{BTreeMap, BTreeSet},
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};

use gpui::{
    AccessibleAction, App, Context, Div, Entity, FocusHandle, FontWeight, IntoElement,
    KeyDownEvent, Render, Role, ScrollAnchor, ScrollHandle, SharedString, Stateful, Subscription,
    Task, Toggled, Window, div, point, prelude::*, px, rgb, text,
};
use magpie_core::{
    credentials::{self, SaveMode, SecretChange},
    provider_store::{Provider, ProviderProtocol, ProviderStore, StoreError},
};

use crate::{
    assets,
    cli_bridge::{self, Cancellation, Reply, Request},
    localization::Locale,
    provider_text::ProviderText as P,
    text_input::{InputEvent, TextInput},
    theme::{Appearance, Palette},
};

const FIELDS: [(&str, P, &str); 6] = [
    ("provider-name", P::Name, "My provider"),
    ("provider-id", P::Id, "my-provider"),
    ("provider-url", P::Url, "https://api.example.com/v1"),
    ("provider-key", P::Key, ""),
    ("provider-env", P::Environment, "PROVIDER_API_KEY"),
    ("provider-models", P::Models, "model-a, model-b"),
];

#[derive(Clone)]
enum Action {
    Add,
    RefreshModels(String),
    CancelRefresh,
    Edit(String),
    Toggle(String, bool),
    Delete(String),
    ConfirmDelete(String),
    CancelDelete,
    Cancel,
    Save,
    Refresh,
    Protocol(ProviderProtocol),
    Enabled,
    ClearKey,
    ToggleDisabled,
}

struct Editor {
    original: Option<Provider>,
    revision: u64,
    protocol: ProviderProtocol,
    enabled: bool,
    clear_key: bool,
}

pub struct ProvidersPage {
    locale: Locale,
    appearance: Appearance,
    path: Result<PathBuf, StoreError>,
    store: Option<ProviderStore>,
    editor: Option<Editor>,
    delete: Option<String>,
    notice: Option<(bool, String)>,
    task: Option<Task<()>>,
    shutdown_pending: Arc<AtomicBool>,
    cancellation: Option<Cancellation>,
    refresh_failures: BTreeSet<String>,
    disabled_expanded: bool,
    fields: BTreeMap<&'static str, Entity<TextInput>>,
    search: Entity<TextInput>,
    query: String,
    focus: FocusHandle,
    buttons: BTreeMap<String, FocusHandle>,
    anchors: BTreeMap<String, ScrollAnchor>,
    scroll: ScrollHandle,
    editor_scroll: ScrollHandle,
    pending: Option<Action>,
    pending_focus: Option<FocusHandle>,
    _subscriptions: Vec<Subscription>,
}

impl ProvidersPage {
    pub(crate) fn new(cx: &mut Context<Self>) -> Self {
        Self::at_path(ProviderStore::default_path(), cx)
    }

    fn at_path(path: Result<PathBuf, StoreError>, cx: &mut Context<Self>) -> Self {
        let locale = Locale::system();
        let fields = FIELDS
            .into_iter()
            .map(|(id, label, placeholder)| {
                let input = cx.new(|cx| {
                    let mut input =
                        TextInput::new(id, "", placeholder, locale.provider_text(label), cx);
                    input.set_secret(id == "provider-key");
                    if id == "provider-models" {
                        input.set_limit(magpie_core::provider_store::MAX_PROVIDER_MODELS * 514);
                    }
                    if id == "provider-key" {
                        input.set_limit(8192);
                    }
                    input
                });
                (id, input)
            })
            .collect::<BTreeMap<_, _>>();
        let search = cx.new(|cx| {
            TextInput::new(
                "provider-search",
                "",
                locale.provider_text(P::Search),
                locale.provider_text(P::Search),
                cx,
            )
        });
        let mut subscriptions = Vec::new();
        for input in fields.values() {
            subscriptions.push(cx.subscribe(input, |view, _, event, cx| {
                match event {
                    InputEvent::Changed => {}
                    // A field's Enter advances no destructive action. The Save
                    // button and Ctrl/Cmd+S are the explicit commit controls.
                    InputEvent::Submit => {}
                    InputEvent::Escape => view.pending = Some(Action::Cancel),
                }
                cx.notify();
            }));
        }
        subscriptions.push(cx.subscribe(&search, |view, input, event, cx| {
            if *event == InputEvent::Changed {
                view.query = input.read(cx).text().to_lowercase();
                if !view.query.is_empty() {
                    view.disabled_expanded = true;
                }
                view.scroll.set_offset(point(px(0.0), px(0.0)));
            }
            cx.notify();
        }));
        let mut view = Self {
            locale,
            appearance: Appearance::System,
            path,
            store: None,
            editor: None,
            delete: None,
            notice: None,
            task: None,
            shutdown_pending: Arc::new(AtomicBool::new(false)),
            cancellation: None,
            refresh_failures: BTreeSet::new(),
            disabled_expanded: false,
            fields,
            search,
            query: String::new(),
            focus: cx.focus_handle(),
            buttons: BTreeMap::new(),
            anchors: BTreeMap::new(),
            scroll: ScrollHandle::new(),
            editor_scroll: ScrollHandle::new(),
            pending: None,
            pending_focus: None,
            _subscriptions: subscriptions,
        };
        for id in [
            "provider-cancel-refresh",
            "provider-add",
            "provider-refresh",
            "provider-save",
            "provider-cancel",
            "provider-enabled",
            "provider-clear-key",
            "provider-confirm-delete",
            "provider-cancel-delete",
            "provider-disabled-toggle",
        ] {
            view.ensure_button(id.to_owned(), cx);
        }
        for protocol in ProviderProtocol::ALL {
            view.ensure_button(format!("provider-protocol:{}", protocol.as_str()), cx);
        }
        for (id, _, _) in FIELDS {
            view.anchors.insert(
                id.into(),
                ScrollAnchor::for_handle(view.editor_scroll.clone()),
            );
        }
        view.refresh(cx);
        view
    }

    pub(crate) fn set_presentation(
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

    pub(crate) fn busy(&self) -> bool {
        self.task.is_some()
    }

    pub(crate) fn shutdown_pending(&self) -> Arc<AtomicBool> {
        self.shutdown_pending.clone()
    }

    pub(crate) fn refresh(&mut self, cx: &mut Context<Self>) {
        if self.busy() {
            return;
        }
        let path = self.path.clone();
        self.shutdown_pending.store(true, Ordering::Release);
        let pending = self.shutdown_pending.clone();
        let operation = cx.background_executor().spawn(async move {
            let result = path.and_then(|path| ProviderStore::load(&path));
            pending.store(false, Ordering::Release);
            result
        });
        self.task = Some(cx.spawn(async move |this, cx| {
            let result = operation.await;
            let _ = this.update(cx, |view, cx| {
                view.task = None;
                match result {
                    Ok(store) => {
                        if let Some(editor) = &mut view.editor {
                            let unchanged = editor.original.as_ref().is_none_or(|original| {
                                store.provider(&original.id) == Some(original)
                            });
                            if unchanged {
                                editor.revision = store.revision();
                                view.notice = None;
                            } else {
                                view.notice =
                                    Some((true, view.locale.provider_text(P::Conflict).to_owned()));
                            }
                        } else {
                            view.notice = None;
                        }
                        view.set_store(store, cx);
                    }
                    Err(error) => view.notice = Some((true, error.to_string())),
                }
                cx.notify();
            });
        }));
        cx.notify();
    }

    #[cfg(feature = "visual-test")]
    pub fn preview_fixture(
        &mut self,
        editor: bool,
        credentials: bool,
        compact: bool,
        cx: &mut Context<Self>,
    ) {
        self.editor = None;
        self.notice = None;
        self.delete = None;
        self.pending_focus = None;
        self.disabled_expanded = false;
        self.scroll.set_offset(point(px(0.0), px(0.0)));
        if editor {
            let provider = self
                .store
                .as_ref()
                .and_then(|store| store.provider("fixture-openai"))
                .cloned();
            if let Some(provider) = provider {
                self.edit(Some(provider), cx);
                if credentials {
                    self.fields["provider-key"].update(cx, |input, cx| {
                        input.set_text("synthetic-fixture-key-not-a-secret", cx);
                    });
                    // Headless texture capture has no native display-link focus
                    // events. Start this distinct scene at the credential fields.
                    self.pending_focus = None;
                    self.editor_scroll
                        .set_offset(point(px(0.0), px(if compact { -350.0 } else { -310.0 })));
                }
            }
        }
        cx.notify();
    }

    fn ensure_button(&mut self, id: String, cx: &mut Context<Self>) {
        let scroll = if id.starts_with("provider-row:")
            || id.contains("delete")
            || id == "provider-disabled-toggle"
        {
            &self.scroll
        } else {
            &self.editor_scroll
        };
        self.anchors
            .entry(id.clone())
            .or_insert_with(|| ScrollAnchor::for_handle(scroll.clone()));
        self.buttons
            .entry(id)
            .or_insert_with(|| cx.focus_handle().tab_index(0).tab_stop(true));
    }

    fn set_store(&mut self, store: ProviderStore, cx: &mut Context<Self>) {
        // Removed providers release their focus handles. Handles for surviving
        // IDs stay stable through sorting, filtering, edits and refreshes.
        self.buttons.retain(|id, _| {
            !id.starts_with("provider-row:")
                || store
                    .providers()
                    .iter()
                    .any(|provider| id.starts_with(&format!("provider-row:{}:", provider.id)))
        });
        self.anchors
            .retain(|id, _| !id.starts_with("provider-row:") || self.buttons.contains_key(id));
        for provider in store.providers() {
            for action in ["edit", "toggle", "delete", "models"] {
                self.ensure_button(format!("provider-row:{}:{action}", provider.id), cx);
            }
        }
        self.store = Some(store);
    }

    fn matches_query(&self, provider: &Provider) -> bool {
        self.query.is_empty()
            || provider.id.to_lowercase().contains(&self.query)
            || provider.name.to_lowercase().contains(&self.query)
            || provider
                .models
                .iter()
                .any(|model| model.to_lowercase().contains(&self.query))
    }

    fn row_focus(&self, id: &str, action: &str, cx: &App) -> FocusHandle {
        self.store
            .as_ref()
            .and_then(|store| store.provider(id))
            .filter(|provider| {
                self.matches_query(provider) && (provider.enabled || self.disabled_expanded)
            })
            .and_then(|_| self.buttons.get(&format!("provider-row:{id}:{action}")))
            .cloned()
            .unwrap_or_else(|| self.search.read(cx).focus_handle())
    }

    fn edit(&mut self, original: Option<Provider>, cx: &mut Context<Self>) {
        let Some(store) = &self.store else {
            return;
        };
        let values = [
            ("provider-name", original.as_ref().map_or("", |p| &p.name)),
            ("provider-id", original.as_ref().map_or("", |p| &p.id)),
            (
                "provider-url",
                original.as_ref().map_or("", |p| &p.base_url),
            ),
            (
                "provider-env",
                original
                    .as_ref()
                    .and_then(|p| p.api_key_env.as_deref())
                    .unwrap_or(""),
            ),
            ("provider-key", ""),
        ];
        let mut accepted = true;
        for (id, value) in values {
            accepted &= self.fields[id].update(cx, |input, cx| input.set_text(value, cx));
        }
        let models = original
            .as_ref()
            .map(|p| p.models.join(", "))
            .unwrap_or_default();
        accepted &=
            self.fields["provider-models"].update(cx, |input, cx| input.set_text(&models, cx));
        if !accepted {
            self.notice = Some((true, self.locale.provider_text(P::Error).to_owned()));
            return;
        }
        self.editor = Some(Editor {
            revision: store.revision(),
            protocol: original
                .as_ref()
                .map_or(ProviderProtocol::OpenAiChat, |p| p.protocol),
            enabled: original.as_ref().is_none_or(|p| p.enabled),
            original,
            clear_key: false,
        });
        self.delete = None;
        self.notice = None;
        self.editor_scroll.set_offset(point(px(0.0), px(0.0)));
        self.pending_focus = Some(self.fields["provider-name"].read(cx).focus_handle());
    }

    fn input(&self, id: &str, cx: &App) -> String {
        self.fields[id].read(cx).text().trim().to_owned()
    }

    fn save(&mut self, cx: &mut Context<Self>) {
        let Some(editor) = &self.editor else {
            return;
        };
        let Ok(path) = &self.path else {
            return;
        };
        let id = editor
            .original
            .as_ref()
            .map(|p| p.id.clone())
            .unwrap_or_else(|| self.input("provider-id", cx));
        let name = self.input("provider-name", cx);
        if id.is_empty() || name.is_empty() {
            self.notice = Some((true, self.locale.provider_text(P::NameRequired).to_owned()));
            self.pending_focus = Some(
                self.fields[if name.is_empty() {
                    "provider-name"
                } else {
                    "provider-id"
                }]
                .read(cx)
                .focus_handle(),
            );
            return;
        }
        let env = self.input("provider-env", cx);
        let key = self.fields["provider-key"].read(cx).text().to_owned();
        let change = if !key.is_empty() {
            SecretChange::Replace(key)
        } else if editor.clear_key {
            SecretChange::Clear
        } else {
            SecretChange::Keep
        };
        let provider = Provider {
            id,
            name,
            base_url: self.input("provider-url", cx),
            protocol: editor.protocol,
            api_key_env: (!env.is_empty()).then_some(env),
            credential_ref: editor
                .original
                .as_ref()
                .and_then(|p| p.credential_ref.clone()),
            models: self
                .input("provider-models", cx)
                .split(',')
                .map(str::trim)
                .filter(|model| !model.is_empty())
                .map(str::to_owned)
                .collect(),
            enabled: editor.enabled,
        };
        let mode = if editor.original.is_some() {
            SaveMode::Update
        } else {
            SaveMode::Create
        };
        let revision = editor.revision;
        let path = path.clone();
        let saved_id = provider.id.clone();
        self.shutdown_pending.store(true, Ordering::Release);
        let pending = self.shutdown_pending.clone();
        let operation = cx.background_executor().spawn(async move {
            let result = credentials::save_provider(&path, provider, revision, change, mode);
            pending.store(false, Ordering::Release);
            result
        });
        self.notice = None;
        self.task = Some(cx.spawn(async move |this, cx| {
            let result = operation.await;
            let _ = this.update(cx, |view, cx| {
                view.task = None;
                match result {
                    Ok(outcome) => {
                        view.set_store(outcome.store, cx);
                        if view
                            .store
                            .as_ref()
                            .and_then(|store| store.provider(&saved_id))
                            .is_some_and(|provider| !provider.enabled)
                        {
                            view.disabled_expanded = true;
                        }
                        view.editor = None;
                        view.fields["provider-key"].update(cx, |input, cx| {
                            input.set_text("", cx);
                        });
                        view.notice = Some((
                            outcome.cleanup_pending,
                            view.locale
                                .provider_text(if outcome.cleanup_pending {
                                    P::KeyringWarning
                                } else {
                                    P::Saved
                                })
                                .to_owned(),
                        ));
                        view.pending_focus = Some(view.row_focus(&saved_id, "edit", cx));
                    }
                    Err(error) => {
                        view.notice = Some((
                            true,
                            format!("{} {}", view.locale.provider_text(P::Error), error),
                        ))
                    }
                }
                cx.notify();
            });
        }));
    }

    pub(crate) fn cancel(&self) {
        if let Some(cancellation) = &self.cancellation {
            cancellation.cancel();
        }
    }

    fn refresh_models(&mut self, id: String, cx: &mut Context<Self>) {
        let (Ok(path), Some(store)) = (&self.path, &self.store) else {
            return;
        };
        let path = path.clone();
        let request = Request::RefreshProvider {
            store: path.clone(),
            id: id.clone(),
            revision: store.revision(),
        };
        let cancellation = Cancellation::new();
        self.cancellation = Some(cancellation.clone());
        self.notice = None;
        self.shutdown_pending.store(true, Ordering::Release);
        let pending = self.shutdown_pending.clone();
        let operation = cx.background_executor().spawn(async move {
            let result = cli_bridge::run(request, cancellation).await;
            // A spawned mutation may have completed before its reply was interrupted.
            let store = ProviderStore::load(&path);
            pending.store(false, Ordering::Release);
            (result, store)
        });
        self.task = Some(cx.spawn(async move |this, cx| {
            let (result, store) = operation.await;
            let _ = this.update(cx, |view, cx| {
                view.task = None;
                view.cancellation = None;
                let verified = match (&result, &store) {
                    (
                        Ok(Reply::ProviderRefreshed {
                            provider_id,
                            revision,
                        }),
                        Ok(store),
                    ) => {
                        provider_id == &id
                            && store.revision() >= *revision
                            && store.provider(&id).is_some()
                    }
                    _ => false,
                };
                let reloaded = store.is_ok();
                if let Ok(store) = store {
                    view.set_store(store, cx);
                }
                if verified {
                    view.refresh_failures.remove(&id);
                    view.notice = Some((false, view.locale.provider_text(P::Saved).to_owned()));
                } else {
                    view.refresh_failures.insert(id.clone());
                    let message = view
                        .locale
                        .provider_text(if reloaded {
                            P::RefreshFailed
                        } else {
                            P::RefreshUnreadable
                        })
                        .to_owned();
                    let message = match result {
                        Err(error) => format!(
                            "{} {}",
                            view.locale
                                .action_text(crate::actions_text::error_text(error.kind)),
                            message
                        ),
                        _ => message,
                    };
                    view.notice = Some((true, message));
                }
                view.pending_focus = Some(view.row_focus(&id, "models", cx));
                cx.notify();
            });
        }));
    }

    fn mutate(&mut self, id: String, enabled: Option<bool>, cx: &mut Context<Self>) {
        let (Ok(path), Some(store)) = (&self.path, &self.store) else {
            return;
        };
        let path = path.clone();
        let revision = store.revision();
        let focus_id = id.clone();
        self.shutdown_pending.store(true, Ordering::Release);
        let pending = self.shutdown_pending.clone();
        let operation = cx.background_executor().spawn(async move {
            let result = if let Some(enabled) = enabled {
                ProviderStore::set_enabled(&path, &id, enabled, revision)
                    .map(|store| (store, false))
                    .map_err(|error| error.to_string())
            } else {
                credentials::remove_provider(&path, &id, revision)
                    .map(|outcome| (outcome.store, outcome.cleanup_pending))
                    .map_err(|error| error.to_string())
            };
            pending.store(false, Ordering::Release);
            result
        });
        self.notice = None;
        self.task = Some(cx.spawn(async move |this, cx| {
            let result = operation.await;
            let _ = this.update(cx, |view, cx| {
                view.task = None;
                match result {
                    Ok((store, cleanup)) => {
                        view.set_store(store, cx);
                        if enabled == Some(false) {
                            view.disabled_expanded = true;
                        }
                        view.delete = None;
                        view.notice = Some((
                            cleanup,
                            view.locale
                                .provider_text(if cleanup { P::KeyringWarning } else { P::Saved })
                                .to_owned(),
                        ));
                        view.pending_focus = Some(view.row_focus(&focus_id, "toggle", cx));
                    }
                    Err(error) => {
                        view.notice = Some((
                            true,
                            format!("{} {error}", view.locale.provider_text(P::Error)),
                        ))
                    }
                }
                cx.notify();
            });
        }));
    }

    fn act(&mut self, action: Action, cx: &mut Context<Self>) {
        if self.busy() && !matches!(action, Action::CancelRefresh) {
            return;
        }
        match action {
            Action::Add => self.edit(None, cx),
            Action::RefreshModels(id) => self.refresh_models(id, cx),
            Action::CancelRefresh => self.cancel(),
            Action::Edit(id) => {
                if let Some(provider) = self
                    .store
                    .as_ref()
                    .and_then(|store| store.provider(&id))
                    .cloned()
                {
                    self.edit(Some(provider), cx);
                }
            }
            Action::Toggle(id, enabled) => self.mutate(id, Some(enabled), cx),
            Action::Delete(id) => {
                self.delete = Some(id);
                self.pending_focus = self.buttons.get("provider-cancel-delete").cloned();
            }
            Action::ConfirmDelete(id) => self.mutate(id, None, cx),
            Action::CancelDelete => {
                if let Some(id) = self.delete.take() {
                    self.pending_focus = Some(self.row_focus(&id, "delete", cx));
                }
            }
            Action::Cancel => {
                if let Some(editor) = self.editor.take() {
                    self.pending_focus = Some(
                        editor
                            .original
                            .as_ref()
                            .map(|provider| self.row_focus(&provider.id, "edit", cx))
                            .unwrap_or_else(|| self.buttons["provider-add"].clone()),
                    );
                }
                self.fields["provider-key"].update(cx, |input, cx| {
                    input.set_text("", cx);
                });
                self.notice = None;
            }
            Action::Save => self.save(cx),
            Action::Refresh => self.refresh(cx),
            Action::Protocol(protocol) => {
                if let Some(editor) = &mut self.editor {
                    editor.protocol = protocol;
                }
            }
            Action::Enabled => {
                if let Some(editor) = &mut self.editor {
                    editor.enabled = !editor.enabled;
                }
            }
            Action::ClearKey => {
                if let Some(editor) = &mut self.editor {
                    editor.clear_key = !editor.clear_key;
                }
            }
            Action::ToggleDisabled => {
                self.disabled_expanded = !self.disabled_expanded;
                self.delete = None;
            }
        }
        cx.notify();
    }

    fn key_down(&mut self, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        let key = &event.keystroke;
        match key.key.as_str() {
            "escape" if self.editor.is_some() || self.delete.is_some() => self.act(
                if self.delete.is_some() {
                    Action::CancelDelete
                } else {
                    Action::Cancel
                },
                cx,
            ),
            "s" if key.modifiers.control || key.modifiers.platform => self.act(Action::Save, cx),
            "r" if key.modifiers.control || key.modifiers.platform => self.refresh(cx),
            "f5" => self.refresh(cx),
            "tab" => {
                if key.modifiers.shift {
                    window.focus_prev(cx);
                } else {
                    window.focus_next(cx);
                }
                for (id, input) in &self.fields {
                    if input.read(cx).focus_handle().is_focused(window) {
                        if let Some(anchor) = self.anchors.get(*id) {
                            anchor.scroll_to(window, cx);
                        }
                        break;
                    }
                }
                for (id, handle) in &self.buttons {
                    if handle.is_focused(window) {
                        if let Some(anchor) = self.anchors.get(id) {
                            anchor.scroll_to(window, cx);
                        }
                        break;
                    }
                }
            }
            _ => return,
        }
        cx.stop_propagation();
    }

    fn button(
        &self,
        id: String,
        label: &str,
        action: Action,
        selected: bool,
        palette: Palette,
        cx: &mut Context<Self>,
    ) -> Stateful<Div> {
        let busy = self.busy() && !matches!(action, Action::CancelRefresh);
        let danger = matches!(action, Action::Delete(_) | Action::ConfirmDelete(_))
            || (matches!(action, Action::ClearKey) && selected);
        let weak = cx.weak_entity();
        let accessible_action = action.clone();
        div()
            .id(SharedString::from(id.clone()))
            .when_some(self.buttons.get(&id), |button, focus| {
                button.track_focus(&focus.clone().tab_stop(!busy))
            })
            .anchor_scroll(self.anchors.get(&id).cloned())
            .tab_stop(!busy)
            .tab_index(0)
            .role(if busy { Role::Status } else { Role::Button })
            .aria_label(label.to_owned())
            .when(
                matches!(
                    action,
                    Action::Protocol(_) | Action::Enabled | Action::ClearKey
                ),
                |button| {
                    button.aria_toggled(if selected {
                        Toggled::True
                    } else {
                        Toggled::False
                    })
                },
            )
            .h(px(34.0))
            .px(px(10.0))
            .flex_none()
            .flex()
            .items_center()
            .justify_center()
            .rounded(px(5.0))
            .border_2()
            .border_color(gpui::rgba(0))
            .bg(rgb(if selected {
                palette.selected
            } else {
                palette.surface
            }))
            .text_color(rgb(if danger {
                palette.danger
            } else if selected {
                palette.accent
            } else {
                palette.text
            }))
            .text_size(px(13.0))
            .whitespace_nowrap()
            .when(busy, |button| button.opacity(0.55))
            .when(!busy, |button| {
                button
                    .cursor_pointer()
                    .hover(move |style| style.bg(rgb(palette.inset)))
                    .focus_visible(move |style| style.border_color(rgb(palette.accent)))
                    .on_click(cx.listener(move |view, _, _, cx| view.act(action.clone(), cx)))
                    .on_a11y_action(AccessibleAction::Click, move |_, _, cx| {
                        let _ = weak.update(cx, |view, cx| view.act(accessible_action.clone(), cx));
                    })
            })
            .child(text!(label.to_owned()))
    }

    fn row(
        &self,
        provider: &Provider,
        wide: bool,
        palette: Palette,
        cx: &mut Context<Self>,
    ) -> Div {
        let host = provider
            .base_url
            .split_once("://")
            .map_or(provider.base_url.as_str(), |(_, address)| address)
            .split('/')
            .next()
            .unwrap_or_default();
        let credential = self
            .locale
            .provider_text(if provider.api_key_env.is_some() {
                P::EnvironmentTag
            } else if provider.credential_ref.is_some() {
                P::KeyStored
            } else {
                P::NoKey
            });
        let summary = format!(
            "{host} · {} {} · {credential}",
            provider.models.len(),
            self.locale.provider_text(P::ModelsCount)
        );
        let identity = div()
            .flex_1()
            .min_w_0()
            .flex()
            .flex_col()
            .gap(px(3.0))
            .line_height(px(17.0))
            .child(
                div()
                    .font_weight(FontWeight::SEMIBOLD)
                    .truncate()
                    .child(text!(provider.name.clone())),
            )
            .child(
                div()
                    .text_size(px(11.0))
                    .text_color(rgb(palette.muted))
                    .truncate()
                    .child(text!(summary)),
            );
        let state = div()
            .text_size(px(11.0))
            .flex_none()
            .text_color(rgb(if provider.enabled {
                palette.accent
            } else {
                palette.muted
            }))
            .child(text!(self.locale.provider_text(if provider.enabled {
                P::Enabled
            } else {
                P::Disabled
            })));
        let controls = div()
            .flex()
            .flex_wrap()
            .items_center()
            .gap(px(4.0))
            .child(self.button(
                format!("provider-row:{}:models", provider.id),
                self.locale.provider_text(P::RefreshModels),
                Action::RefreshModels(provider.id.clone()),
                false,
                palette,
                cx,
            ))
            .child(self.button(
                format!("provider-row:{}:toggle", provider.id),
                self.locale.provider_text(if provider.enabled {
                    P::Disable
                } else {
                    P::Enable
                }),
                Action::Toggle(provider.id.clone(), !provider.enabled),
                false,
                palette,
                cx,
            ))
            .child(self.button(
                format!("provider-row:{}:edit", provider.id),
                self.locale.provider_text(P::Edit),
                Action::Edit(provider.id.clone()),
                false,
                palette,
                cx,
            ))
            .child(self.button(
                format!("provider-row:{}:delete", provider.id),
                self.locale.provider_text(P::Delete),
                Action::Delete(provider.id.clone()),
                false,
                palette,
                cx,
            ));
        let content = if wide {
            div()
                .flex()
                .items_center()
                .gap(px(16.0))
                .child(identity)
                .child(
                    div()
                        .flex_none()
                        .max_w(px(540.0))
                        .flex()
                        .items_center()
                        .gap(px(12.0))
                        .child(state)
                        .child(controls),
                )
        } else {
            div()
                .flex()
                .flex_col()
                .gap(px(6.0))
                .child(
                    div()
                        .flex()
                        .items_start()
                        .gap(px(16.0))
                        .child(identity)
                        .child(state),
                )
                .child(controls)
        };
        div()
            .w_full()
            .min_w_0()
            .flex_none()
            .px(px(16.0))
            .py(px(12.0))
            .flex()
            .flex_col()
            .gap(px(10.0))
            .border_b_1()
            .border_color(rgb(palette.border))
            .child(content)
            .when(self.refresh_failures.contains(&provider.id), |row| {
                row.child(
                    div()
                        .text_size(px(11.0))
                        .text_color(rgb(palette.warning))
                        .whitespace_normal()
                        .child(text!(self.locale.provider_text(P::ReviewModels))),
                )
            })
            .when(self.delete.as_deref() == Some(&provider.id), |row| {
                row.child(
                    div()
                        .p(px(12.0))
                        .rounded(px(6.0))
                        .bg(rgb(palette.danger_bg))
                        .flex()
                        .flex_col()
                        .gap(px(8.0))
                        .child(text!(self.locale.provider_text(P::DeleteHint)))
                        .child(
                            div()
                                .flex()
                                .flex_wrap()
                                .gap(px(6.0))
                                .child(self.button(
                                    "provider-confirm-delete".into(),
                                    self.locale.provider_text(P::ConfirmDelete),
                                    Action::ConfirmDelete(provider.id.clone()),
                                    false,
                                    palette,
                                    cx,
                                ))
                                .child(self.button(
                                    "provider-cancel-delete".into(),
                                    self.locale.provider_text(P::Cancel),
                                    Action::CancelDelete,
                                    false,
                                    palette,
                                    cx,
                                )),
                        ),
                )
            })
    }

    fn editor(&self, palette: Palette, cx: &mut Context<Self>) -> Div {
        let editor = self
            .editor
            .as_ref()
            .expect("editor exists only in editor view");
        let mut form = div().flex().flex_col().gap(px(16.0));
        for (id, label, _) in FIELDS {
            if id == "provider-key" {
                form = form.child(
                    div()
                        .flex()
                        .flex_col()
                        .gap(px(6.0))
                        .child(text!(self.locale.provider_text(P::Protocol)))
                        .child(div().flex().flex_wrap().gap(px(4.0)).children(
                            ProviderProtocol::ALL.map(|protocol| {
                                self.button(
                                    format!("provider-protocol:{}", protocol.as_str()),
                                    protocol_name(protocol),
                                    Action::Protocol(protocol),
                                    editor.protocol == protocol,
                                    palette,
                                    cx,
                                )
                            }),
                        )),
                );
            }
            let hint = match id {
                "provider-key" => Some(P::KeyHint),
                "provider-env" => Some(P::EnvironmentHint),
                "provider-models" => Some(P::ModelsHint),
                _ => None,
            };
            form = form.child(
                div()
                    .id(SharedString::from(format!("field:{id}")))
                    .anchor_scroll(self.anchors.get(id).cloned())
                    .min_w_0()
                    .flex()
                    .flex_col()
                    .gap(px(6.0))
                    .child(
                        div()
                            .text_size(px(12.0))
                            .font_weight(FontWeight::MEDIUM)
                            .child(text!(self.locale.provider_text(label))),
                    )
                    .child(self.fields[id].clone())
                    .when_some(hint, |field, hint| {
                        field.child(
                            div()
                                .text_size(px(11.0))
                                .line_height(px(16.0))
                                .text_color(rgb(palette.muted))
                                .whitespace_normal()
                                .child(text!(self.locale.provider_text(hint))),
                        )
                    }),
            );
            if id == "provider-key"
                && editor
                    .original
                    .as_ref()
                    .is_some_and(|p| p.credential_ref.is_some())
            {
                form = form.child(self.button(
                    "provider-clear-key".into(),
                    self.locale.provider_text(if editor.clear_key {
                        P::KeepKey
                    } else {
                        P::RemoveKey
                    }),
                    Action::ClearKey,
                    editor.clear_key,
                    palette,
                    cx,
                ));
            }
        }
        form = form.child(self.button(
            "provider-enabled".into(),
            self.locale.provider_text(if editor.enabled {
                P::Enabled
            } else {
                P::Disabled
            }),
            Action::Enabled,
            editor.enabled,
            palette,
            cx,
        ));
        div()
            .flex_1()
            .min_h_0()
            .min_w_0()
            .flex()
            .flex_col()
            .child(
                div()
                    .id("provider-editor-scroll")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .track_scroll(&self.editor_scroll)
                    .p(px(20.0))
                    .child(form),
            )
            .child(
                div()
                    .flex_none()
                    .px(px(20.0))
                    .py(px(12.0))
                    .border_t_1()
                    .border_color(rgb(palette.border))
                    .flex()
                    .flex_wrap()
                    .gap(px(6.0))
                    .child(
                        self.button(
                            "provider-save".into(),
                            self.locale.provider_text(if self.busy() {
                                P::Saving
                            } else {
                                P::Save
                            }),
                            Action::Save,
                            true,
                            palette,
                            cx,
                        ),
                    )
                    .child(self.button(
                        "provider-cancel".into(),
                        self.locale.provider_text(P::Cancel),
                        Action::Cancel,
                        false,
                        palette,
                        cx,
                    )),
            )
    }
}

impl Render for ProvidersPage {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if let Some(action) = self.pending.take() {
            self.act(action, cx);
        }
        if let Some(focus) = self.pending_focus.take() {
            let anchor = self
                .fields
                .iter()
                .find_map(|(id, input)| {
                    (input.read(cx).focus_handle() == focus)
                        .then(|| self.anchors.get(*id).cloned())
                        .flatten()
                })
                .or_else(|| {
                    self.buttons.iter().find_map(|(id, handle)| {
                        (handle == &focus)
                            .then(|| self.anchors.get(id).cloned())
                            .flatten()
                    })
                });
            window.on_next_frame(move |window, cx| {
                focus.focus(window, cx);
                if let Some(anchor) = anchor {
                    anchor.scroll_to(window, cx);
                }
            });
        }
        let palette = Palette::for_window(window, self.appearance);
        let wide = f32::from(window.viewport_size().width) >= 800.0;
        let busy = self.busy();
        for (id, label, placeholder) in FIELDS {
            let immutable_id =
                id == "provider-id" && self.editor.as_ref().is_some_and(|e| e.original.is_some());
            self.fields[id].update(cx, |input, cx| {
                input.set_presentation(
                    placeholder,
                    self.locale.provider_text(label),
                    palette,
                    busy || immutable_id,
                    cx,
                )
            });
        }
        self.search.update(cx, |input, cx| {
            input.set_presentation(
                self.locale.provider_text(P::Search),
                self.locale.provider_text(P::Search),
                palette,
                false,
                cx,
            )
        });
        let title = self.locale.provider_text(
            if self.editor.as_ref().is_some_and(|e| e.original.is_some()) {
                P::EditTitle
            } else {
                P::AddTitle
            },
        );
        let header = div()
            .flex_none()
            .p(px(16.0))
            .flex()
            .flex_wrap()
            .items_center()
            .gap(px(8.0))
            .when(self.editor.is_some(), |row| {
                row.child(
                    div()
                        .flex_1()
                        .font_weight(FontWeight::SEMIBOLD)
                        .child(text!(title)),
                )
            })
            .when(self.editor.is_none(), |row| {
                row.child(div().flex_1().min_w(px(150.0)).child(self.search.clone()))
            })
            .child(self.button(
                "provider-refresh".into(),
                self.locale.provider_text(P::Refresh),
                Action::Refresh,
                false,
                palette,
                cx,
            ))
            .when(self.cancellation.is_some(), |row| {
                row.child(
                    div()
                        .text_size(px(11.0))
                        .child(text!(self.locale.provider_text(P::RefreshingModels))),
                )
            })
            .when(self.cancellation.is_some(), |row| {
                row.child(self.button(
                    "provider-cancel-refresh".into(),
                    self.locale.provider_text(P::Cancel),
                    Action::CancelRefresh,
                    false,
                    palette,
                    cx,
                ))
            })
            .when(self.editor.is_none() && self.store.is_some(), |row| {
                row.child(self.button(
                    "provider-add".into(),
                    self.locale.provider_text(P::Add),
                    Action::Add,
                    true,
                    palette,
                    cx,
                ))
            });
        let body = if self.editor.is_some() {
            self.editor(palette, cx)
        } else {
            let providers = self
                .store
                .as_ref()
                .map(|store| store.providers())
                .unwrap_or_default();
            let matched = providers
                .iter()
                .filter(|provider| self.matches_query(provider))
                .collect::<Vec<_>>();
            let disabled_count = matched.iter().filter(|provider| !provider.enabled).count();
            let empty_text = if self.store.is_none() && busy {
                P::Loading
            } else if providers.is_empty() {
                P::Empty
            } else {
                P::NoMatches
            };
            div().flex_1().min_h_0().min_w_0().child(
                div()
                    .id("providers-scroll")
                    .size_full()
                    .overflow_y_scroll()
                    .track_scroll(&self.scroll)
                    .when(matched.is_empty(), |body| {
                        body.child(
                            div()
                                .p(px(24.0))
                                .flex()
                                .flex_col()
                                .gap(px(12.0))
                                .child(
                                    div()
                                        .font_weight(FontWeight::SEMIBOLD)
                                        .child(text!(self.locale.provider_text(empty_text))),
                                )
                                .when(providers.is_empty() && self.store.is_some(), |body| {
                                    body.child(
                                        div()
                                            .text_color(rgb(palette.muted))
                                            .whitespace_normal()
                                            .child(text!(self.locale.provider_text(P::EmptyHint))),
                                    )
                                }),
                        )
                    })
                    .children(
                        matched
                            .iter()
                            .filter(|provider| provider.enabled)
                            .map(|provider| self.row(provider, wide, palette, cx)),
                    )
                    .when(disabled_count > 0, |body| {
                        body.child(
                            div().px(px(6.0)).py(px(6.0)).child(
                                self.button(
                                    "provider-disabled-toggle".into(),
                                    &format!(
                                        "{} ({disabled_count})",
                                        self.locale.provider_text(P::DisabledProviders)
                                    ),
                                    Action::ToggleDisabled,
                                    false,
                                    palette,
                                    cx,
                                )
                                .w_full()
                                .justify_start()
                                .aria_expanded(self.disabled_expanded)
                                .child(
                                    gpui::svg()
                                        .data(assets::CHEVRON)
                                        .size(px(14.0))
                                        .flex_none()
                                        .text_color(rgb(palette.muted))
                                        .when(self.disabled_expanded, |icon| {
                                            icon.with_transformation(gpui::Transformation::rotate(
                                                gpui::radians(std::f32::consts::FRAC_PI_2),
                                            ))
                                        }),
                                ),
                            ),
                        )
                        .when(self.disabled_expanded, |body| {
                            body.children(
                                matched
                                    .iter()
                                    .filter(|provider| !provider.enabled)
                                    .map(|provider| self.row(provider, wide, palette, cx)),
                            )
                        })
                    }),
            )
        };
        div()
            .id("providers-page")
            .track_focus(&self.focus)
            .on_key_down(cx.listener(Self::key_down))
            .size_full()
            .min_w_0()
            .flex()
            .flex_col()
            .rounded(px(8.0))
            .bg(rgb(palette.surface))
            .overflow_hidden()
            .child(header)
            .when_some(self.notice.as_ref(), |page, (error, message)| {
                page.child(
                    div()
                        .id("provider-notice")
                        .role(Role::Status)
                        .flex_none()
                        .mx(px(16.0))
                        .mb(px(8.0))
                        .p(px(10.0))
                        .rounded(px(5.0))
                        .bg(rgb(if *error {
                            palette.warning_bg
                        } else {
                            palette.selected
                        }))
                        .text_color(rgb(if *error {
                            palette.warning
                        } else {
                            palette.accent
                        }))
                        .text_size(px(12.0))
                        .whitespace_normal()
                        .child(text!(message.clone())),
                )
            })
            .child(body)
            .child(
                div()
                    .flex_none()
                    .px(px(16.0))
                    .py(px(10.0))
                    .border_t_1()
                    .border_color(rgb(palette.border))
                    .text_size(px(11.0))
                    .line_height(px(16.0))
                    .text_color(rgb(palette.muted))
                    .whitespace_normal()
                    .child(text!(self.locale.provider_text(P::Restart))),
            )
    }
}

fn protocol_name(protocol: ProviderProtocol) -> &'static str {
    match protocol {
        ProviderProtocol::OpenAiChat => "OpenAI Chat",
        ProviderProtocol::OpenAiResponses => "OpenAI Responses",
        ProviderProtocol::AnthropicMessages => "Anthropic Messages",
        ProviderProtocol::Gemini => "Gemini",
    }
}

#[cfg(all(test, feature = "visual-test"))]
mod tests {
    use super::*;
    use gpui::TestAppContext;
    use std::{
        fs,
        sync::atomic::{AtomicU64, Ordering},
    };

    static NEXT: AtomicU64 = AtomicU64::new(0);
    struct Fixture(PathBuf);
    impl Fixture {
        fn new() -> Self {
            let path = std::env::temp_dir().join(format!(
                "magpie-provider-ui-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            let mut builder = fs::DirBuilder::new();
            #[cfg(unix)]
            {
                use std::os::unix::fs::DirBuilderExt;
                builder.mode(0o700);
            }
            builder.create(&path).unwrap();
            Self(path)
        }
        fn path(&self) -> PathBuf {
            self.0.join("providers.toml")
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            fs::remove_dir_all(&self.0).unwrap();
        }
    }

    fn type_field(
        view: &mut ProvidersPage,
        id: &str,
        value: &str,
        cx: &mut Context<ProvidersPage>,
    ) {
        assert!(view.fields[id].update(cx, |input, cx| input.set_text(value, cx)));
    }

    #[gpui::test]
    fn form_actions_persist_create_disable_and_delete(cx: &mut TestAppContext) {
        let fixture = Fixture::new();
        let page = cx.new(|cx| ProvidersPage::at_path(Ok(fixture.path()), cx));
        cx.run_until_parked();
        page.update(cx, |view, cx| {
            view.act(Action::Add, cx);
            type_field(view, "provider-id", "local", cx);
            type_field(view, "provider-name", "Local fixture", cx);
            type_field(view, "provider-url", "http://127.0.0.1:8099/v1", cx);
            type_field(view, "provider-models", "model-a, model-b", cx);
            view.act(Action::Save, cx);
        });
        cx.run_until_parked();
        let saved = ProviderStore::load(&fixture.path()).unwrap();
        assert_eq!(
            saved.provider("local").unwrap().models,
            ["model-a", "model-b"]
        );
        assert_eq!(saved.revision(), 1);
        page.update(cx, |view, cx| {
            view.act(Action::Toggle("local".into(), false), cx)
        });
        cx.run_until_parked();
        page.update(cx, |view, _| {
            assert!(
                view.disabled_expanded,
                "newly disabled row stays available for keyboard focus"
            );
            assert!(!view.shutdown_pending.load(Ordering::Acquire));
        });
        assert!(
            !ProviderStore::load(&fixture.path())
                .unwrap()
                .provider("local")
                .unwrap()
                .enabled
        );
        page.update(cx, |view, cx| view.act(Action::Delete("local".into()), cx));
        assert!(
            ProviderStore::load(&fixture.path())
                .unwrap()
                .provider("local")
                .is_some(),
            "opening confirmation must not delete"
        );
        page.update(cx, |view, cx| {
            view.act(Action::ConfirmDelete("local".into()), cx)
        });
        cx.run_until_parked();
        assert!(
            ProviderStore::load(&fixture.path())
                .unwrap()
                .providers()
                .is_empty()
        );
    }

    #[gpui::test]
    fn saved_provider_hidden_by_search_returns_focus_to_search(cx: &mut TestAppContext) {
        let fixture = Fixture::new();
        let page = cx.new(|cx| ProvidersPage::at_path(Ok(fixture.path()), cx));
        cx.run_until_parked();
        page.update(cx, |view, cx| {
            view.act(Action::Add, cx);
            type_field(view, "provider-id", "local", cx);
            type_field(view, "provider-name", "Original", cx);
            type_field(view, "provider-url", "http://127.0.0.1:8099/v1", cx);
            view.act(Action::Save, cx);
        });
        cx.run_until_parked();
        page.update(cx, |view, cx| {
            view.query = "original".into();
            view.act(Action::Edit("local".into()), cx);
            type_field(view, "provider-name", "Renamed", cx);
            view.act(Action::Save, cx);
        });
        cx.run_until_parked();
        page.update(cx, |view, cx| {
            assert_eq!(view.query, "original");
            assert_eq!(
                view.store.as_ref().unwrap().provider("local").unwrap().name,
                "Renamed"
            );
            assert!(view.pending_focus.as_ref() == Some(&view.search.read(cx).focus_handle()));
        });
    }

    #[gpui::test]
    fn stale_editor_preserves_draft_and_external_change(cx: &mut TestAppContext) {
        let fixture = Fixture::new();
        let provider = Provider {
            id: "local".into(),
            name: "Before".into(),
            base_url: "http://127.0.0.1:8099/v1".into(),
            protocol: ProviderProtocol::OpenAiChat,
            api_key_env: None,
            credential_ref: None,
            models: vec!["model-a".into()],
            enabled: true,
        };
        ProviderStore::create(&fixture.path(), provider.clone(), 0).unwrap();
        let page = cx.new(|cx| ProvidersPage::at_path(Ok(fixture.path()), cx));
        cx.run_until_parked();
        page.update(cx, |view, cx| {
            view.act(Action::Edit("local".into()), cx);
            type_field(view, "provider-name", "Unsaved draft", cx);
        });
        let mut external = provider;
        external.name = "Written by CLI".into();
        ProviderStore::update(&fixture.path(), external, 1).unwrap();
        page.update(cx, |view, cx| view.act(Action::Save, cx));
        cx.run_until_parked();
        assert_eq!(
            ProviderStore::load(&fixture.path())
                .unwrap()
                .provider("local")
                .unwrap()
                .name,
            "Written by CLI"
        );
        page.update(cx, |view, cx| {
            assert_eq!(view.input("provider-name", cx), "Unsaved draft");
            assert!(view.editor.is_some());
            assert!(view.notice.as_ref().unwrap().0);
            view.refresh(cx);
        });
        cx.run_until_parked();
        page.update(cx, |view, cx| {
            assert_eq!(view.input("provider-name", cx), "Unsaved draft");
            assert_eq!(
                view.editor.as_ref().unwrap().revision,
                1,
                "refresh cannot silently rebase an edited provider"
            );
            type_field(view, "provider-key", "synthetic-secret", cx);
            view.act(Action::Cancel, cx);
            assert_eq!(view.fields["provider-key"].read(cx).text(), "");
        });
    }
}
