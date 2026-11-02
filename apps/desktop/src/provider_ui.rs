//! Native Provider management. This entity owns drafts and focus only; every
//! persisted operation crosses the packaged CLI boundary into the shared core.
use crate::{
    cli_bridge::Cancellation,
    localization::Locale,
    provider_bridge::{self, Failure, Key, Provider, Request},
    provider_state::ProviderState,
    provider_text::{P, error_text, t},
    text_input::{InputEvent, TextInput},
    theme::{self, Palette},
};
use gpui::{
    App, Context, Div, Entity, FocusHandle, FontWeight, IntoElement, KeyDownEvent, Render, Role,
    ScrollAnchor, ScrollHandle, SharedString, Subscription, Task, Window, div, point, prelude::*,
    px, rgb, text,
};
use gpui_kit_material::components::{ButtonSize, ButtonVariant, MaterialButton, material_button};
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, BTreeSet},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

const FIELDS: [(&str, &str); 11] = [
    ("provider-id", "my-provider"),
    ("provider-name", ""),
    ("provider-url", "https://api.example.com/v1"),
    ("provider-env", "MY_PROVIDER_API_KEY"),
    ("provider-models", "model-id"),
    ("provider-key-id", "primary"),
    ("provider-key-name", ""),
    ("provider-key-env", "MY_PROVIDER_API_KEY"),
    ("provider-list-search", ""),
    ("provider-model-search", ""),
    ("provider-model-add", "model-id"),
];
#[derive(Clone)]
enum Page {
    List,
    Details(String),
    Form(Option<String>),
    KeyForm {
        provider: String,
        key: Option<String>,
    },
    Models(String),
}
#[derive(Clone)]
enum Confirmation {
    RemoveProvider(String),
    RemoveKey {
        provider: String,
        key: String,
    },
    Recover,
    Discard(Page),
    Close {
        previous: Option<Box<Confirmation>>,
        previous_origin: Option<String>,
    },
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ProviderCloseDecision {
    KeepEditing,
    Discard,
}
#[derive(Clone)]
enum Action {
    Refresh,
    Add,
    Open(String),
    Back,
    Edit(String),
    SaveForm,
    Protocol(&'static str),
    ToggleEnabled,
    ToggleHttp,
    ToggleProvider(String),
    RemoveProvider(String),
    Models(String),
    AddKey(String),
    EditKey { provider: String, key: String },
    SaveKey,
    ToggleKeyEnabled,
    ToggleKey { provider: String, key: String },
    RemoveKey { provider: String, key: String },
    KeyUp { provider: String, key: String },
    ToggleAll,
    ToggleModel(String),
    AddModel,
    DeleteModel(String),
    SaveModels,
    Discover(String),
    Test(String),
    Cancel,
    Recover,
    Confirm,
    Keep,
}
struct ListRow {
    id: String,
    name: String,
    base_url: String,
    enabled: bool,
    model_count: usize,
    key_count: usize,
}
struct Control {
    focus: FocusHandle,
    anchor: ScrollAnchor,
}

pub(crate) struct ProviderUi {
    state: ProviderState,
    locale: Locale,
    palette: Palette,
    compact: bool,
    page: Page,
    confirmation: Option<Confirmation>,
    confirmation_origin: Option<String>,
    inputs: BTreeMap<&'static str, Entity<TextInput>>,
    input_anchors: BTreeMap<&'static str, ScrollAnchor>,
    controls: BTreeMap<String, Control>,
    used_controls: BTreeSet<String>,
    scroll: ScrollHandle,
    focus: FocusHandle,
    pending_focus: Option<String>,
    pending_action: Option<Action>,
    task: Option<Task<()>>,
    cancellation: Option<Cancellation>,
    pending: Arc<AtomicBool>,
    cancel_requested: bool,
    notice: Option<P>,
    _subscriptions: Vec<Subscription>,
    form_original: Option<Provider>,
    key_original: Option<Key>,
    form_protocol: String,
    form_enabled: bool,
    form_http: bool,
    form_key_enabled: bool,
    form_dirty: bool,
    catalog: Vec<String>,
    selected: BTreeSet<String>,
    auto_include: bool,
    models_original: Option<Provider>,
    models_dirty: bool,
}
impl gpui::EventEmitter<ProviderCloseDecision> for ProviderUi {}
impl ProviderUi {
    pub(crate) fn new(cx: &mut Context<Self>) -> Self {
        let locale = Locale::system();
        let scroll = ScrollHandle::new();
        let mut inputs = BTreeMap::new();
        let mut input_anchors = BTreeMap::new();
        for (id, placeholder) in FIELDS {
            inputs.insert(
                id,
                cx.new(|cx| TextInput::new(id, "", placeholder, t(locale, field_label(id)), cx)),
            );
            input_anchors.insert(id, ScrollAnchor::for_handle(scroll.clone()));
        }
        let mut subscriptions = Vec::new();
        for (&id, input) in &inputs {
            subscriptions.push(cx.subscribe(input, move |view, _input, event, cx| {
                match event {
                    InputEvent::Changed => {
                        view.notice = None;
                        view.recompute_form_dirty(cx);
                        if id == "provider-model-search" || id == "provider-list-search" {
                            view.scroll.set_offset(point(px(0.), px(0.)));
                        }
                    }
                    InputEvent::Escape => {
                        view.pending_action = Some(if view.busy() {
                            Action::Cancel
                        } else {
                            Action::Back
                        })
                    }
                    // Text-field Enter never applies a write or starts a request.
                    InputEvent::Submit => {}
                }
                cx.notify();
            }));
        }
        subscriptions.push(cx.on_app_quit(|view, _| {
            view.cancel();
            let pending = view.pending.clone();
            async move {
                while pending.load(Ordering::Acquire) {
                    async_io::Timer::after(Duration::from_millis(10)).await;
                }
            }
        }));
        let mut view = Self {
            state: ProviderState::default(),
            locale,
            palette: Palette {
                background: 0xf6f6f7,
                surface: 0xffffff,
                inset: 0xf0f0f2,
                border: 0xe3e3e7,
                text: 0x202124,
                muted: 0x65666d,
                accent: 0x3e61d4,
                selected: 0xeef2ff,
                warning: 0x865818,
                warning_bg: 0xfff5e5,
                danger: 0xad393e,
                danger_bg: 0xffeeee,
            },
            compact: false,
            page: Page::List,
            confirmation: None,
            confirmation_origin: None,
            inputs,
            input_anchors,
            controls: BTreeMap::new(),
            used_controls: BTreeSet::new(),
            scroll,
            focus: cx.focus_handle(),
            pending_focus: None,
            pending_action: None,
            task: None,
            cancellation: None,
            pending: Arc::new(AtomicBool::new(false)),
            cancel_requested: false,
            notice: None,
            _subscriptions: subscriptions,
            form_original: None,
            key_original: None,
            form_protocol: "chat".into(),
            form_enabled: true,
            form_http: false,
            form_key_enabled: true,
            form_dirty: false,
            catalog: Vec::new(),
            selected: BTreeSet::new(),
            auto_include: true,
            models_original: None,
            models_dirty: false,
        };
        view.refresh(cx);
        view
    }
    pub(crate) fn set_presentation(
        &mut self,
        locale: Locale,
        palette: Palette,
        compact: bool,
        cx: &mut Context<Self>,
    ) {
        if self.locale != locale
            || self.compact != compact
            || palette_key(self.palette) != palette_key(palette)
        {
            self.locale = locale;
            self.palette = palette;
            self.compact = compact;
            cx.notify();
        }
    }
    pub(crate) fn refresh(&mut self, cx: &mut Context<Self>) {
        if !self.busy() {
            self.start(Request::list(), cx);
        }
    }
    pub(crate) fn busy(&self) -> bool {
        self.state.busy.is_some() || self.pending.load(Ordering::Acquire)
    }
    pub(crate) fn cancel(&mut self) {
        if let Some(cancel) = &self.cancellation {
            cancel.cancel();
            self.cancel_requested = true;
        }
    }
    pub(crate) fn pending(&self) -> Arc<AtomicBool> {
        self.pending.clone()
    }
    pub(crate) fn has_unsaved_changes(&self, cx: &App) -> bool {
        match self.page {
            Page::Form(_) | Page::KeyForm { .. } => self.form_has_changes(cx),
            Page::Models(_) => {
                self.models_dirty || !self.input("provider-model-add", cx).is_empty()
            }
            _ => false,
        }
    }
    pub(crate) fn close_confirmation_open(&self) -> bool {
        matches!(self.confirmation, Some(Confirmation::Close { .. }))
    }
    pub(crate) fn request_window_close_confirmation(
        &mut self,
        window: &Window,
        cx: &mut Context<Self>,
    ) {
        if self.close_confirmation_open() {
            return;
        }
        let previous = self.confirmation.take().map(Box::new);
        let previous_origin = self.confirmation_origin.take();
        self.open_confirmation(
            Confirmation::Close {
                previous,
                previous_origin,
            },
            window,
            cx,
        );
        cx.notify();
    }
    fn provider(&self, id: &str) -> Option<Provider> {
        self.state.snapshot.as_ref()?.provider(id).cloned()
    }
    fn input(&self, id: &str, cx: &App) -> String {
        self.inputs[id].read(cx).text().trim().to_owned()
    }
    fn set_input(&mut self, id: &str, value: &str, cx: &mut Context<Self>) {
        let ok = self.inputs[id].update(cx, |input, cx| input.set_text(value, cx));
        if !ok {
            self.state.error = Some(Failure {
                code: "provider_input_too_large".into(),
                uncertain: false,
            });
        }
    }
    fn request(&self, command: &'static str, id: Option<String>, data: Option<Value>) -> Request {
        Request {
            command,
            id,
            token: self.state.snapshot.as_ref().map(|s| s.token.clone()),
            data,
        }
    }
    fn local_error(&mut self, code: &str) {
        self.notice = None;
        self.state.error = Some(Failure {
            code: code.into(),
            uncertain: false,
        });
    }
    fn start(&mut self, request: Request, cx: &mut Context<Self>) {
        if !self.state.begin(&request) {
            return;
        }
        self.notice = None;
        self.cancel_requested = false;
        self.pending_focus = Some("provider-cancel".into());
        let cancel = Cancellation::new();
        self.cancellation = Some(cancel.clone());
        self.pending.store(true, Ordering::Release);
        let pending = self.pending.clone();
        let execute = request.clone();
        let work = cx.background_executor().spawn(async move {
            let result = provider_bridge::run(execute, cancel).await;
            pending.store(false, Ordering::Release);
            result
        });
        self.task = Some(cx.spawn(async move |this, cx| {
            let result = work.await;
            let _ = this.update(cx, |view, cx| {
                let ok = view.state.finish(&request, result);
                view.cancellation = None;
                view.cancel_requested = false;
                view.task = None;
                if ok {
                    match request.command {
                        "add" | "edit" | "key-put" => {
                            if let Some(id) = &request.id {
                                view.page = Page::Details(id.clone());
                            }
                            view.form_dirty = false;
                            view.notice = Some(P::Saved);
                        }
                        "remove" => {
                            view.page = Page::List;
                            view.notice = Some(P::Saved);
                        }
                        "key-remove" | "key-order" => {
                            view.notice = Some(P::Saved);
                        }
                        "models" => {
                            view.models_dirty = false;
                            view.notice = Some(P::Saved);
                            if let Some(id) = &request.id {
                                view.load_models(id);
                            }
                        }
                        "refresh" => {
                            view.notice = Some(P::Refreshed);
                            if let Page::Models(id) = view.page.clone()
                                && !view.models_dirty
                            {
                                view.load_models(&id);
                            }
                        }
                        "list" => {
                            if let Page::Models(id) = view.page.clone()
                                && !view.models_dirty
                            {
                                view.load_models(&id);
                            }
                        }
                        "test" => view.notice = Some(P::TestPassed),
                        "recover" => view.notice = Some(P::Saved),
                        _ => {}
                    }
                    view.confirmation = None;
                    view.confirmation_origin = None;
                }
                // A reread reconciles truth while retaining all unsaved form/model
                // buffers. No mutation is automatically replayed after any failure.
                view.pending_focus = Some(
                    if view.state.stale || view.state.uncertain {
                        "provider-reread"
                    } else if matches!(view.page, Page::List) {
                        "provider-add"
                    } else {
                        "provider-back"
                    }
                    .into(),
                );
                cx.notify();
            });
        }));
        cx.notify();
    }
    fn open_form(&mut self, id: Option<String>, cx: &mut Context<Self>) {
        let existing = id.as_deref().and_then(|id| self.provider(id));
        if id.is_some() && existing.is_none() {
            self.local_error("provider_not_found");
            return;
        }
        self.set_input(
            "provider-id",
            existing.as_ref().map_or("", |p| p.id.as_str()),
            cx,
        );
        self.set_input(
            "provider-name",
            existing.as_ref().map_or("", |p| p.name.as_str()),
            cx,
        );
        self.set_input(
            "provider-url",
            existing.as_ref().map_or("", |p| p.base_url.as_str()),
            cx,
        );
        self.set_input("provider-env", "", cx);
        self.set_input("provider-models", "", cx);
        self.form_protocol = existing
            .as_ref()
            .map_or("chat", |p| p.protocol.as_str())
            .to_owned();
        self.form_enabled = existing.as_ref().is_none_or(|p| p.enabled);
        self.form_http = existing.as_ref().is_some_and(|p| p.allow_insecure_http);
        self.form_original = existing;
        self.form_dirty = false;
        self.page = Page::Form(id);
        self.pending_focus = Some(
            if self.form_original.is_some() {
                "provider-name"
            } else {
                "provider-id"
            }
            .into(),
        );
        self.scroll.set_offset(point(px(0.), px(0.)));
    }
    fn open_key(&mut self, provider: String, key: Option<String>, cx: &mut Context<Self>) {
        let existing = self.provider(&provider).and_then(|p| {
            p.keys
                .into_iter()
                .find(|k| Some(&k.id) == key.as_ref() && !k.deleted)
        });
        if key.is_some() && existing.is_none() {
            self.local_error("provider_key_not_found");
            return;
        }
        self.set_input(
            "provider-key-id",
            existing.as_ref().map_or("", |k| k.id.as_str()),
            cx,
        );
        self.set_input(
            "provider-key-name",
            existing.as_ref().map_or("", |k| k.name.as_str()),
            cx,
        );
        self.set_input(
            "provider-key-env",
            existing.as_ref().map_or("", |k| k.env.as_str()),
            cx,
        );
        self.form_key_enabled = existing.as_ref().is_none_or(|k| k.enabled);
        self.key_original = existing;
        self.form_dirty = false;
        self.page = Page::KeyForm { provider, key };
        self.pending_focus = Some("provider-key-name".into());
        self.scroll.set_offset(point(px(0.), px(0.)));
    }
    fn load_models(&mut self, id: &str) {
        if let Some(p) = self.provider(id) {
            self.catalog = p.models.clone();
            self.selected = p.selected_models.iter().cloned().collect();
            self.auto_include = p.auto_include;
            self.models_original = Some(p);
            self.models_dirty = false;
        }
    }
    fn recompute_form_dirty(&mut self, cx: &App) {
        self.form_dirty = self.form_has_changes(cx);
    }
    fn form_has_changes(&self, cx: &App) -> bool {
        match &self.page {
            Page::Form(_) => {
                if let Some(original) = &self.form_original {
                    self.input("provider-name", cx) != original.name
                        || self.input("provider-url", cx) != original.base_url
                        || self.form_protocol != original.protocol
                        || self.form_enabled != original.enabled
                        || self.form_http != original.allow_insecure_http
                } else {
                    [
                        "provider-id",
                        "provider-name",
                        "provider-url",
                        "provider-env",
                        "provider-models",
                    ]
                    .iter()
                    .any(|id| !self.input(id, cx).is_empty())
                        || self.form_protocol != "chat"
                        || !self.form_enabled
                        || self.form_http
                }
            }
            Page::KeyForm { .. } => {
                if let Some(original) = &self.key_original {
                    self.input("provider-key-name", cx) != original.name
                        || self.input("provider-key-env", cx) != original.env
                        || self.form_key_enabled != original.enabled
                } else {
                    ["provider-key-id", "provider-key-name", "provider-key-env"]
                        .iter()
                        .any(|id| !self.input(id, cx).is_empty())
                        || !self.form_key_enabled
                }
            }
            _ => self.form_dirty,
        }
    }
    fn save_form(&mut self, cx: &mut Context<Self>) {
        let Page::Form(edit_id) = self.page.clone() else {
            return;
        };
        let id = self.input("provider-id", cx);
        let name = self.input("provider-name", cx);
        let url = self.input("provider-url", cx);
        if !valid_identifier(&id) || !valid_name(&name) || url.is_empty() {
            self.local_error("provider_invalid_input");
            return;
        }
        let (command, data) = if let Some(original) = &self.form_original {
            let Some(current) = self.provider(&original.id) else {
                self.local_error("provider_draft_conflict");
                return;
            };
            if provider_draft_conflicts(
                original,
                &current,
                &name,
                &url,
                &self.form_protocol,
                self.form_enabled,
                self.form_http,
            ) {
                self.local_error("provider_draft_conflict");
                return;
            }
            let mut data = serde_json::Map::new();
            if name != original.name {
                data.insert("name".into(), json!(name));
            }
            if url != original.base_url {
                data.insert("base_url".into(), json!(url));
            }
            if self.form_protocol != original.protocol {
                data.insert("protocol".into(), json!(self.form_protocol));
            }
            if self.form_enabled != original.enabled {
                data.insert("enabled".into(), json!(self.form_enabled));
            }
            if self.form_http != original.allow_insecure_http {
                data.insert("allow_insecure_http".into(), json!(self.form_http));
            }
            ("edit", Value::Object(data))
        } else {
            let env = self.input("provider-env", cx);
            let models = parse_models(&self.input("provider-models", cx));
            if (!env.is_empty() && !valid_environment(&env))
                || models.len() > 512
                || models.iter().any(|model| !valid_model(model))
                || models.iter().collect::<BTreeSet<_>>().len() != models.len()
            {
                self.local_error("provider_invalid_input");
                return;
            }
            let keys = if env.is_empty() {
                json!([])
            } else {
                json!([{"id":"primary","name":t(self.locale,P::Keys),"env":env,"enabled":true}])
            };
            (
                "add",
                json!({"name":name,"base_url":url,"protocol":self.form_protocol,"enabled":self.form_enabled,"allow_insecure_http":self.form_http,"keys":keys,"models":models,"selected_models":[],"auto_include":true}),
            )
        };
        self.start(
            self.request(command, Some(edit_id.unwrap_or(id)), Some(data)),
            cx,
        );
    }
    fn save_key(&mut self, cx: &mut Context<Self>) {
        let Page::KeyForm { provider, .. } = self.page.clone() else {
            return;
        };
        let id = self.input("provider-key-id", cx);
        let name = self.input("provider-key-name", cx);
        let env = self.input("provider-key-env", cx);
        if !valid_identifier(&id) || !valid_name(&name) || !valid_environment(&env) {
            self.local_error("provider_invalid_input");
            return;
        }
        let Some(current_provider) = self.provider(&provider) else {
            self.local_error("provider_draft_conflict");
            return;
        };
        let current = current_provider.keys.iter().find(|key| key.id == id);
        let Ok((name, env, enabled)) = merge_key_fields(
            self.key_original.as_ref(),
            current,
            &name,
            &env,
            self.form_key_enabled,
        ) else {
            self.local_error("provider_draft_conflict");
            return;
        };
        self.start(
            self.request(
                "key-put",
                Some(provider),
                Some(json!({"id":id,"name":name,"env":env,"enabled":enabled})),
            ),
            cx,
        );
    }
    fn save_models(&mut self, cx: &mut Context<Self>) {
        let Page::Models(id) = self.page.clone() else {
            return;
        };
        let Some(current) = self.provider(&id) else {
            self.local_error("provider_draft_conflict");
            return;
        };
        // After an explicit reread, preserve catalog/selection changes made by
        // another entry point unless this draft explicitly changed that value.
        let Ok((models, selected, auto)) = merge_models(
            &current,
            self.models_original.as_ref(),
            &self.catalog,
            &self.selected,
            self.auto_include,
        ) else {
            self.local_error("provider_draft_conflict");
            return;
        };
        self.start(
            self.request(
                "models",
                Some(id),
                Some(json!({"models":models,"selected_models":selected,"auto_include":auto})),
            ),
            cx,
        );
    }
    fn fallback_focus(&self) -> &'static str {
        if matches!(self.page, Page::List) {
            "provider-add"
        } else {
            "provider-back"
        }
    }
    fn open_confirmation(&mut self, confirmation: Confirmation, window: &Window, cx: &App) {
        self.confirmation_origin = self
            .controls
            .iter()
            .find(|(_, c)| c.focus.is_focused(window))
            .map(|(id, _)| id.clone())
            .or_else(|| {
                self.inputs
                    .iter()
                    .find(|(_, input)| input.read(cx).focus_handle().is_focused(window))
                    .map(|(id, _)| (*id).to_owned())
            });
        self.confirmation = Some(confirmation);
        self.pending_focus = Some("provider-keep".into());
    }
    fn dismiss_confirmation(&mut self, cx: &mut Context<Self>) {
        let confirmation = self.confirmation.take();
        self.pending_focus = Some(
            self.confirmation_origin
                .take()
                .unwrap_or_else(|| self.fallback_focus().to_owned()),
        );
        if let Some(Confirmation::Close {
            previous,
            previous_origin,
        }) = confirmation
        {
            self.confirmation = previous.map(|previous| *previous);
            self.confirmation_origin = previous_origin;
            cx.emit(ProviderCloseDecision::KeepEditing);
        }
    }
    fn go_back(&mut self, window: &Window, cx: &mut Context<Self>) {
        if self.confirmation.is_some() {
            self.dismiss_confirmation(cx);
            return;
        }
        let target = match &self.page {
            Page::List => return,
            Page::Details(_) | Page::Form(None) => Page::List,
            Page::Form(Some(id)) | Page::Models(id) => Page::Details(id.clone()),
            Page::KeyForm { provider, .. } => Page::Details(provider.clone()),
        };
        let dirty = self.has_unsaved_changes(cx);
        if dirty {
            self.open_confirmation(Confirmation::Discard(target), window, cx);
        } else {
            self.page = target;
            self.pending_focus = Some(
                if matches!(self.page, Page::List) {
                    "provider-add"
                } else {
                    "provider-back"
                }
                .into(),
            );
        }
    }
    fn act(&mut self, action: Action, window: &mut Window, cx: &mut Context<Self>) {
        if self.busy() && !matches!(action, Action::Cancel) {
            return;
        }
        match action {
            Action::Refresh => self.refresh(cx),
            Action::Cancel => self.cancel(),
            Action::Add => self.open_form(None, cx),
            Action::Open(id) => {
                self.page = Page::Details(id);
                self.pending_focus = Some("provider-back".into());
            }
            Action::Back => self.go_back(window, cx),
            Action::Edit(id) => self.open_form(Some(id), cx),
            Action::SaveForm => self.save_form(cx),
            Action::Protocol(protocol) => {
                self.form_protocol = protocol.into();
                self.form_dirty = true;
            }
            Action::ToggleEnabled => {
                self.form_enabled = !self.form_enabled;
                self.form_dirty = true;
            }
            Action::ToggleHttp => {
                self.form_http = !self.form_http;
                self.form_dirty = true;
            }
            Action::ToggleProvider(id) => {
                if let Some(p) = self.provider(&id) {
                    self.start(
                        self.request("edit", Some(id), Some(json!({"enabled":!p.enabled}))),
                        cx,
                    );
                }
            }
            Action::RemoveProvider(id) => {
                self.open_confirmation(Confirmation::RemoveProvider(id), window, cx);
            }
            Action::Models(id) => {
                self.load_models(&id);
                self.set_input("provider-model-search", "", cx);
                self.set_input("provider-model-add", "", cx);
                self.page = Page::Models(id);
                self.pending_focus = Some("provider-model-search".into());
            }
            Action::AddKey(provider) => self.open_key(provider, None, cx),
            Action::EditKey { provider, key } => self.open_key(provider, Some(key), cx),
            Action::SaveKey => self.save_key(cx),
            Action::ToggleKeyEnabled => {
                self.form_key_enabled = !self.form_key_enabled;
                self.form_dirty = true;
            }
            Action::ToggleKey { provider, key } => {
                if let Some(k) = self
                    .provider(&provider)
                    .and_then(|p| p.keys.into_iter().find(|k| k.id == key && !k.deleted))
                {
                    self.start(
                        self.request(
                            "key-put",
                            Some(provider),
                            Some(json!({"id":k.id,"name":k.name,"env":k.env,"enabled":!k.enabled})),
                        ),
                        cx,
                    );
                }
            }
            Action::RemoveKey { provider, key } => {
                self.open_confirmation(Confirmation::RemoveKey { provider, key }, window, cx);
            }
            Action::KeyUp { provider, key } => {
                if let Some(p) = self.provider(&provider) {
                    let mut ids: Vec<_> = p
                        .keys
                        .into_iter()
                        .filter(|k| !k.deleted)
                        .map(|k| k.id)
                        .collect();
                    if let Some(index) = ids.iter().position(|id| id == &key).filter(|i| *i > 0) {
                        ids.swap(index, index - 1);
                        self.start(
                            self.request("key-order", Some(provider), Some(json!({"key_ids":ids}))),
                            cx,
                        );
                    }
                }
            }
            Action::ToggleAll => {
                self.auto_include = !self.auto_include;
                self.models_dirty = true;
            }
            Action::ToggleModel(id) => {
                if !self.selected.remove(&id) {
                    self.selected.insert(id);
                }
                self.models_dirty = true;
            }
            Action::AddModel => {
                let model = self.input("provider-model-add", cx);
                if valid_model(&model) && self.catalog.len() < 512 && !self.catalog.contains(&model)
                {
                    self.catalog.push(model);
                    self.models_dirty = true;
                    self.set_input("provider-model-add", "", cx);
                } else {
                    self.local_error("provider_invalid_model");
                }
            }
            Action::DeleteModel(id) => {
                self.catalog.retain(|m| m != &id);
                self.selected.remove(&id);
                self.models_dirty = true;
            }
            Action::SaveModels => self.save_models(cx),
            Action::Discover(id) => self.start(self.request("refresh", Some(id), None), cx),
            Action::Test(id) => self.start(self.request("test", Some(id), None), cx),
            Action::Recover => {
                self.open_confirmation(Confirmation::Recover, window, cx);
            }
            Action::Keep => {
                self.dismiss_confirmation(cx);
            }
            Action::Confirm => {
                if let Some(confirmation) = self.confirmation.clone() {
                    match confirmation {
                        Confirmation::RemoveProvider(id) => {
                            self.start(self.request("remove", Some(id), None), cx)
                        }
                        Confirmation::RemoveKey { provider, key } => self.start(
                            self.request("key-remove", Some(provider), Some(json!({"key_id":key}))),
                            cx,
                        ),
                        Confirmation::Recover => self.start(
                            Request {
                                command: "recover",
                                id: None,
                                token: None,
                                data: None,
                            },
                            cx,
                        ),
                        Confirmation::Discard(target) => {
                            self.page = target;
                            self.confirmation = None;
                            self.form_dirty = false;
                            self.models_dirty = false;
                            self.confirmation_origin = None;
                            self.pending_focus = Some(self.fallback_focus().into());
                        }
                        Confirmation::Close { .. } => {
                            // Leave the draft and dialog in place until the
                            // parent has cleared both asynchronous close gates.
                            cx.emit(ProviderCloseDecision::Discard);
                        }
                    }
                }
            }
        }
        self.recompute_form_dirty(cx);
        cx.notify();
    }
    fn key_down(&mut self, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        match event.keystroke.key.as_str() {
            "tab" => {
                if event.keystroke.modifiers.shift {
                    window.focus_prev(cx);
                } else {
                    window.focus_next(cx);
                }
                self.reveal_focused(window, cx);
            }
            "escape" => self.act(
                if self.busy() {
                    Action::Cancel
                } else {
                    Action::Back
                },
                window,
                cx,
            ),
            _ => return,
        }
        cx.stop_propagation();
    }
    fn reveal_focused(&self, window: &mut Window, cx: &mut App) {
        for (id, control) in &self.controls {
            if control.focus.is_focused(window) {
                // Pinned actions are already visible and live outside the
                // scrolling body, so they must not move its scroll offset.
                if !is_footer_control(id) {
                    control.anchor.scroll_to(window, cx);
                    window.on_next_frame(|window, _| window.refresh());
                }
                return;
            }
        }
        for (id, input) in &self.inputs {
            if input.read(cx).focus_handle().is_focused(window) {
                self.input_anchors[id].scroll_to(window, cx);
                window.on_next_frame(|window, _| window.refresh());
                return;
            }
        }
    }
    fn button(
        &mut self,
        id: impl Into<String>,
        label: impl Into<SharedString>,
        action: Action,
        enabled: bool,
        selected: Option<bool>,
        cx: &mut Context<Self>,
    ) -> MaterialButton {
        let id = id.into();
        let pinned = is_footer_control(&id);
        let primary = matches!(
            id.as_str(),
            "provider-form-save" | "provider-key-save" | "provider-model-save" | "provider-add"
        );
        self.used_controls.insert(id.clone());
        let control = self.controls.entry(id.clone()).or_insert_with(|| Control {
            focus: cx.focus_handle().tab_index(0).tab_stop(true),
            anchor: ScrollAnchor::for_handle(self.scroll.clone()),
        });
        let label = label.into();
        let variant = if primary {
            ButtonVariant::Filled
        } else if selected == Some(true) {
            ButtonVariant::Tonal
        } else {
            ButtonVariant::Outlined
        };
        material_button(SharedString::from(id), variant)
            .button_size(ButtonSize::Small)
            .track_focus(&control.focus)
            .when(!pinned, |button| {
                button.anchor_scroll(Some(control.anchor.clone()))
            })
            .disabled(!enabled)
            .accessibility_label(label.clone())
            .when_some(selected, |button, on| button.toggle(true).selected(on))
            .on_click(cx.listener(move |view, _, window, cx| view.act(action.clone(), window, cx)))
            .child(text!(label))
    }

    fn field(&mut self, id: &'static str, label: P, disabled: bool, cx: &mut Context<Self>) -> Div {
        let placeholder = FIELDS
            .iter()
            .find(|(key, _)| *key == id)
            .map_or("", |(_, placeholder)| *placeholder);
        let label = t(self.locale, label);
        let palette = self.palette;
        self.inputs[id].update(cx, |input, cx| {
            input.set_presentation(placeholder, label, palette, disabled, cx)
        });
        div().w_full().min_w_0().child(
            div()
                .id(SharedString::from(format!("{id}-anchor")))
                .w_full()
                .min_w_0()
                .flex()
                .flex_col()
                .gap(px(5.))
                .anchor_scroll(Some(self.input_anchors[id].clone()))
                .child(
                    div()
                        .text_size(px(12.))
                        .text_color(rgb(palette.muted))
                        .child(text!(label)),
                )
                .child(self.inputs[id].clone()),
        )
    }
    fn caption(&self, key: P) -> Div {
        div()
            .w_full()
            .min_w_0()
            .text_size(px(12.))
            .line_height(px(18.))
            .text_color(rgb(self.palette.muted))
            .whitespace_normal()
            .child(text!(t(self.locale, key)))
    }
    fn header(&mut self, title: impl Into<SharedString>, cx: &mut Context<Self>) -> Div {
        let title = title.into();
        let back = self.button(
            "provider-back",
            t(self.locale, P::Back),
            Action::Back,
            !self.busy(),
            None,
            cx,
        );
        div()
            .w_full()
            .min_w_0()
            .flex()
            .items_center()
            .gap(px(12.))
            .child(back)
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .text_size(px(16.))
                    .font_weight(FontWeight::SEMIBOLD)
                    .truncate()
                    .child(text!(title)),
            )
    }
    fn status(&mut self, cx: &mut Context<Self>) -> Div {
        let palette = self.palette;
        let mut panel = div().w_full().min_w_0().flex().flex_col().gap(px(8.));
        if let Some(command) = self.state.busy {
            let label = if self.cancel_requested {
                P::CancelRequested
            } else {
                match command {
                    "list" | "show" => P::Reading,
                    "test" => P::Testing,
                    "refresh" => P::Discovering,
                    _ => P::Saving,
                }
            };
            let cancel = self.button(
                "provider-cancel",
                t(self.locale, P::Cancel),
                Action::Cancel,
                !self.cancel_requested,
                None,
                cx,
            );
            panel = panel.child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .gap(px(8.))
                    .id("provider-progress")
                    .role(Role::Status)
                    .child(self.caption(label))
                    .child(cancel),
            );
        }
        if let Some(failure) = self.state.error.clone() {
            panel = panel.child(
                div()
                    .px(px(10.))
                    .py(px(8.))
                    .rounded(px(5.))
                    .bg(rgb(palette.danger_bg))
                    .text_color(rgb(palette.danger))
                    .text_size(px(12.))
                    .line_height(px(18.))
                    .whitespace_normal()
                    .id("provider-error")
                    .role(Role::Alert)
                    .child(text!(error_text(self.locale, &failure.code))),
            );
        }
        if self.state.uncertain || self.state.stale {
            let reread = self.button(
                "provider-reread",
                t(self.locale, P::Reread),
                Action::Refresh,
                !self.busy(),
                None,
                cx,
            );
            panel = panel.child(
                div()
                    .flex()
                    .flex_col()
                    .gap(px(6.))
                    .child(self.caption(if self.state.uncertain {
                        P::Uncertain
                    } else {
                        P::Stale
                    }))
                    .child(div().flex().child(reread)),
            );
        }
        if self.state.error.as_ref().is_some_and(|e| {
            matches!(
                e.code.as_str(),
                "provider_recovery_required"
                    | "provider_unknown_recovery_material"
                    | "provider_recovery_conflict"
                    | "provider_lock_owner_unverifiable"
            )
        }) {
            let recover = self.button(
                "provider-recover",
                t(self.locale, P::Recovery),
                Action::Recover,
                !self.busy(),
                None,
                cx,
            );
            panel = panel.child(div().flex().child(recover));
        }
        if let Some(notice) = self.notice {
            panel = panel.child(
                div()
                    .id("provider-notice")
                    .role(Role::Status)
                    .child(self.caption(notice)),
            );
        }
        panel
    }
    fn list(&mut self, cx: &mut Context<Self>) -> Div {
        let enabled = self.state.can_write();
        let add = self.button(
            "provider-add",
            t(self.locale, P::Add),
            Action::Add,
            enabled,
            None,
            cx,
        );
        let refresh = self.button(
            "provider-refresh",
            t(self.locale, P::Refresh),
            Action::Refresh,
            !self.busy(),
            None,
            cx,
        );
        let mut content = div()
            .w_full()
            .min_w_0()
            .flex()
            .flex_col()
            .gap(px(12.))
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .gap(px(8.))
                    .flex_wrap()
                    .child(
                        div()
                            .text_size(px(16.))
                            .font_weight(FontWeight::SEMIBOLD)
                            .child(text!(t(self.locale, P::Providers))),
                    )
                    .child(div().flex().gap(px(6.)).child(refresh).child(add)),
            );
        let providers: Vec<_> = self
            .state
            .snapshot
            .as_ref()
            .map(|s| {
                s.active()
                    .map(|p| ListRow {
                        id: p.id.clone(),
                        name: p.name.clone(),
                        base_url: p.base_url.clone(),
                        enabled: p.enabled,
                        model_count: if p.auto_include {
                            p.models.len()
                        } else {
                            p.selected_models.len()
                        },
                        key_count: p.keys.iter().filter(|key| !key.deleted).count(),
                    })
                    .collect()
            })
            .unwrap_or_default();
        if providers.is_empty() {
            if self.state.snapshot.is_some() {
                content = content.child(
                    div()
                        .py(px(28.))
                        .flex()
                        .flex_col()
                        .gap(px(6.))
                        .child(
                            div()
                                .text_size(px(14.))
                                .child(text!(t(self.locale, P::NoProviders))),
                        )
                        .child(self.caption(P::NoProvidersHint)),
                );
            }
            return content;
        }
        content = content.child(self.field("provider-list-search", P::Search, self.busy(), cx));
        let filter = self.input("provider-list-search", cx).to_lowercase();
        let mut found = false;
        let mut rows = div()
            .w_full()
            .min_w_0()
            .flex()
            .flex_col()
            .border_t_1()
            .border_color(rgb(self.palette.border));
        for p in providers {
            if !filter.is_empty()
                && !format!("{} {} {}", p.name, p.id, p.base_url)
                    .to_lowercase()
                    .contains(&filter)
            {
                continue;
            }
            found = true;
            let details = self.button(
                format!("provider-open-{}", p.id),
                t(self.locale, P::Details),
                Action::Open(p.id.clone()),
                !self.busy(),
                None,
                cx,
            );
            let models = p.model_count;
            let endpoint = p
                .base_url
                .split_once("://")
                .map_or(p.base_url.as_str(), |(_, rest)| rest)
                .split('/')
                .next()
                .unwrap_or(p.base_url.as_str());
            let summary = format!("{} · {} {}", endpoint, models, t(self.locale, P::Models));
            let summary = if p.enabled {
                summary
            } else {
                format!("{} · {}", t(self.locale, P::Disabled), summary)
            };
            let name = div()
                .flex_1()
                .min_w_0()
                .flex()
                .flex_col()
                .gap(px(2.))
                .child(
                    div()
                        .text_size(px(13.))
                        .font_weight(FontWeight::SEMIBOLD)
                        .truncate()
                        .child(text!(p.name)),
                )
                .child(
                    div()
                        .text_size(px(11.))
                        .text_color(rgb(self.palette.muted))
                        .truncate()
                        .child(text!(summary)),
                );
            rows = rows.child(
                div()
                    .w_full()
                    .min_w_0()
                    .min_h(px(56.))
                    .py(px(8.))
                    .border_b_1()
                    .border_color(rgb(self.palette.border))
                    .flex()
                    .items_center()
                    .gap(px(12.))
                    .child(
                        div()
                            .size(px(28.))
                            .rounded(px(6.))
                            .bg(rgb(self.palette.inset))
                            .flex_none()
                            .flex()
                            .items_center()
                            .justify_center()
                            .text_size(px(12.))
                            .font_weight(FontWeight::SEMIBOLD)
                            .child(text!(
                                p.id.chars()
                                    .next()
                                    .unwrap_or('P')
                                    .to_ascii_uppercase()
                                    .to_string()
                            )),
                    )
                    .child(name)
                    .child(
                        div()
                            .flex_none()
                            .text_size(px(11.))
                            .text_color(rgb(self.palette.muted))
                            .whitespace_nowrap()
                            .child(text!(format!(
                                "{} {}",
                                p.key_count,
                                t(self.locale, P::Keys)
                            ))),
                    )
                    .child(details),
            );
        }
        if !found {
            rows = rows.child(self.caption(P::EmptyFilter));
        }
        content.child(rows)
    }
    fn details(&mut self, id: &str, cx: &mut Context<Self>) -> Div {
        let Some(p) = self.provider(id) else {
            return self.missing(cx);
        };
        let writable = self.state.can_write();
        let mut content = div()
            .w_full()
            .min_w_0()
            .flex()
            .flex_col()
            .gap(px(14.))
            .child(self.header(p.name.clone(), cx));
        let actions = div()
            .flex()
            .flex_wrap()
            .gap(px(6.))
            .child(self.button(
                "provider-edit",
                t(self.locale, P::Edit),
                Action::Edit(id.into()),
                writable,
                None,
                cx,
            ))
            .child(self.button(
                "provider-toggle",
                t(self.locale, if p.enabled { P::Pause } else { P::Enable }),
                Action::ToggleProvider(id.into()),
                writable,
                None,
                cx,
            ))
            .child(self.button(
                "provider-test",
                t(self.locale, P::Test),
                Action::Test(id.into()),
                writable && p.enabled,
                None,
                cx,
            ))
            .child(self.button(
                "provider-refresh-models",
                t(self.locale, P::Refresh),
                Action::Discover(id.into()),
                writable && p.enabled,
                None,
                cx,
            ))
            .child(self.button(
                "provider-remove",
                t(self.locale, P::Remove),
                Action::RemoveProvider(id.into()),
                writable,
                None,
                cx,
            ));
        content = content
            .child(
                div()
                    .text_size(px(12.))
                    .text_color(rgb(self.palette.muted))
                    .whitespace_normal()
                    .child(text!(p.base_url.clone())),
            )
            .child(actions);
        content = content.child(
            div()
                .text_size(px(12.))
                .text_color(rgb(self.palette.muted))
                .child(text!(format!(
                    "{}: {}",
                    t(self.locale, P::Protocol),
                    p.protocol
                ))),
        );
        content = content.child(self.caption(P::CodexProtocolHint));
        if !matches!(p.protocol.as_str(), "chat" | "responses") {
            content = content.child(self.caption(P::UnsupportedProtocol));
        }
        if p.catalog_error.is_some() {
            content = content.child(self.caption(P::CatalogStale));
        }
        content = content.child(if let Some(checked_at) = p.catalog_checked_at {
            div()
                .text_size(px(11.))
                .text_color(rgb(self.palette.muted))
                .whitespace_normal()
                .child(text!(format!(
                    "{}: {}",
                    t(self.locale, P::LastChecked),
                    utc_stamp(checked_at)
                )))
        } else {
            self.caption(P::NotChecked)
        });
        let model_button = self.button(
            "provider-models-open",
            t(self.locale, P::Models),
            Action::Models(id.into()),
            writable,
            None,
            cx,
        );
        let count = if p.auto_include {
            p.models.len()
        } else {
            p.selected_models.len()
        };
        content = content.child(
            div()
                .border_t_1()
                .border_color(rgb(self.palette.border))
                .pt(px(12.))
                .flex()
                .items_center()
                .justify_between()
                .gap(px(8.))
                .child(div().text_size(px(13.)).child(text!(format!(
                    "{} · {} / {}",
                    t(self.locale, P::Models),
                    count,
                    p.models.len()
                ))))
                .child(model_button),
        );
        let add_key = self.button(
            "provider-key-add",
            t(self.locale, P::AddKey),
            Action::AddKey(id.into()),
            writable,
            None,
            cx,
        );
        content = content.child(
            div()
                .border_t_1()
                .border_color(rgb(self.palette.border))
                .pt(px(12.))
                .flex()
                .items_center()
                .justify_between()
                .child(
                    div()
                        .font_weight(FontWeight::SEMIBOLD)
                        .text_size(px(13.))
                        .child(text!(t(self.locale, P::Keys))),
                )
                .child(add_key),
        );
        let active: Vec<_> = p.keys.iter().filter(|k| !k.deleted).collect();
        if p.keys.is_empty() {
            content = content.child(self.caption(P::NoKeys));
        } else if active.is_empty() {
            content = content.child(self.caption(P::CredentialError));
        }
        for (index, k) in active.iter().enumerate() {
            let edit = self.button(
                format!("provider-key-edit-{}", k.id),
                t(self.locale, P::Edit),
                Action::EditKey {
                    provider: id.into(),
                    key: k.id.clone(),
                },
                writable,
                None,
                cx,
            );
            let toggle = self.button(
                format!("provider-key-toggle-{}", k.id),
                t(self.locale, if k.enabled { P::Pause } else { P::Enable }),
                Action::ToggleKey {
                    provider: id.into(),
                    key: k.id.clone(),
                },
                writable,
                None,
                cx,
            );
            let up = self.button(
                format!("provider-key-up-{}", k.id),
                t(self.locale, P::MoveUp),
                Action::KeyUp {
                    provider: id.into(),
                    key: k.id.clone(),
                },
                writable && index > 0,
                None,
                cx,
            );
            let remove = self.button(
                format!("provider-key-remove-{}", k.id),
                t(self.locale, P::Remove),
                Action::RemoveKey {
                    provider: id.into(),
                    key: k.id.clone(),
                },
                writable,
                None,
                cx,
            );
            content = content.child(
                div()
                    .w_full()
                    .min_w_0()
                    .py(px(8.))
                    .border_b_1()
                    .border_color(rgb(self.palette.border))
                    .flex()
                    .gap(px(8.))
                    .when(self.compact, |row| row.flex_col())
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .flex()
                            .flex_col()
                            .gap(px(3.))
                            .child(
                                div()
                                    .text_size(px(13.))
                                    .font_weight(FontWeight::MEDIUM)
                                    .truncate()
                                    .child(text!(k.name.clone())),
                            )
                            .child(
                                div()
                                    .text_size(px(11.))
                                    .text_color(rgb(self.palette.muted))
                                    .whitespace_normal()
                                    .child(text!(format!(
                                        "{} · {} · {}",
                                        k.masked,
                                        k.env,
                                        t(
                                            self.locale,
                                            if k.enabled {
                                                P::Configured
                                            } else {
                                                P::Disabled
                                            }
                                        )
                                    ))),
                            ),
                    )
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .flex_wrap()
                            .gap(px(4.))
                            .child(edit)
                            .child(toggle)
                            .child(up)
                            .child(remove),
                    ),
            );
        }
        content.child(self.caption(P::NoStoredSecret))
    }
    fn provider_form(&mut self, edit: bool, cx: &mut Context<Self>) -> Div {
        let disabled = self.busy();
        let mut content = div()
            .w_full()
            .min_w_0()
            .flex()
            .flex_col()
            .gap(px(12.))
            .child(self.header(
                t(self.locale, if edit { P::EditTitle } else { P::AddTitle }),
                cx,
            ));
        content = content
            .child(
                div()
                    .w_full()
                    .min_w_0()
                    .flex()
                    .gap(px(12.))
                    .when(self.compact, |row| row.flex_col())
                    .child(div().flex_1().min_w_0().child(self.field(
                        "provider-id",
                        P::Id,
                        disabled || edit,
                        cx,
                    )))
                    .child(div().flex_1().min_w_0().child(self.field(
                        "provider-name",
                        P::Name,
                        disabled,
                        cx,
                    ))),
            )
            .child(self.field("provider-url", P::BaseUrl, disabled, cx));
        let protocols = div()
            .flex()
            .flex_wrap()
            .gap(px(6.))
            .child(self.button(
                "provider-protocol-chat",
                "Chat Completions",
                Action::Protocol("chat"),
                !disabled,
                Some(self.form_protocol == "chat"),
                cx,
            ))
            .child(self.button(
                "provider-protocol-responses",
                "Responses",
                Action::Protocol("responses"),
                !disabled,
                Some(self.form_protocol == "responses"),
                cx,
            ));
        content = content.child(
            div()
                .flex()
                .flex_col()
                .gap(px(5.))
                .child(self.caption(P::Protocol))
                .child(protocols),
        );
        content = content.child(self.caption(P::CodexProtocolHint));
        if !matches!(self.form_protocol.as_str(), "chat" | "responses") {
            content = content.child(self.caption(P::UnsupportedProtocol));
        }
        if !edit {
            content = content
                .child(self.field("provider-env", P::Environment, disabled, cx))
                .child(self.caption(P::EnvironmentHint))
                .child(self.caption(P::OptionalEnvironmentHint))
                .child(self.field("provider-models", P::ModelIds, disabled, cx));
        }
        content = content.child(div().flex().flex_wrap().gap(px(6.)).child(self.button(
            "provider-form-enabled",
            t(self.locale, P::Enable),
            Action::ToggleEnabled,
            !disabled,
            Some(self.form_enabled),
            cx,
        )));
        let http = self
            .button(
                "provider-form-http",
                if self.form_http { "✓" } else { "○" },
                Action::ToggleHttp,
                !disabled,
                Some(self.form_http),
                cx,
            )
            .aria_label(t(self.locale, P::AllowHttp));
        content = content.child(
            div()
                .w_full()
                .min_w_0()
                .flex()
                .flex_col()
                .gap(px(5.))
                .child(
                    div()
                        .w_full()
                        .min_w_0()
                        .flex()
                        .items_start()
                        .gap(px(8.))
                        .child(http)
                        .child(div().flex_1().min_w_0().child(self.caption(P::AllowHttp))),
                )
                .when(self.form_http, |c| c.child(self.caption(P::AllowHttpHint))),
        );
        if self.form_dirty {
            content = content.child(self.caption(P::Unsaved));
        }
        content
    }
    fn key_form(&mut self, edit: bool, cx: &mut Context<Self>) -> Div {
        let disabled = self.busy();
        let mut content = div()
            .w_full()
            .min_w_0()
            .flex()
            .flex_col()
            .gap(px(12.))
            .child(self.header(t(self.locale, P::KeyTitle), cx))
            .child(self.field("provider-key-id", P::KeyId, disabled || edit, cx))
            .child(self.field("provider-key-name", P::KeyName, disabled, cx))
            .child(self.field("provider-key-env", P::KeyEnvironment, disabled, cx))
            .child(self.caption(P::EnvironmentHint));
        if edit {
            content = content.child(self.caption(P::ExistingKeyHint));
        }
        content = content.child(div().flex().child(self.button(
            "provider-key-enabled",
            t(self.locale, P::Enable),
            Action::ToggleKeyEnabled,
            !disabled,
            Some(self.form_key_enabled),
            cx,
        )));
        content
    }
    fn model_picker(&mut self, id: &str, cx: &mut Context<Self>) -> Div {
        let Some(provider) = self.provider(id) else {
            return self.missing(cx);
        };
        let disabled = self.busy();
        let mut content = div()
            .w_full()
            .min_w_0()
            .flex()
            .flex_col()
            .gap(px(12.))
            .child(self.header(
                format!("{} · {}", t(self.locale, P::ModelsTitle), provider.name),
                cx,
            ));
        content = content.child(
            div()
                .flex()
                .flex_wrap()
                .gap(px(6.))
                .child(self.button(
                    "provider-model-all",
                    t(self.locale, P::SelectAll),
                    Action::ToggleAll,
                    !disabled,
                    Some(self.auto_include),
                    cx,
                ))
                .child(self.button(
                    "provider-model-refresh",
                    t(self.locale, P::Refresh),
                    Action::Discover(id.into()),
                    self.state.can_write() && !self.models_dirty && provider.enabled,
                    None,
                    cx,
                )),
        );
        if provider.catalog_error.is_some() {
            content = content.child(self.caption(P::CatalogStale));
        }
        content = content.child(self.field("provider-model-search", P::Search, disabled, cx));
        let filter = self.input("provider-model-search", cx).to_lowercase();
        let models = self.catalog.clone();
        let mut count = 0;
        let mut rows = div()
            .w_full()
            .min_w_0()
            .flex()
            .flex_col()
            .border_t_1()
            .border_color(rgb(self.palette.border));
        for model in models {
            if !filter.is_empty() && !model.to_lowercase().contains(&filter) {
                continue;
            }
            count += 1;
            let selected = self.auto_include || self.selected.contains(&model);
            let choose = self
                .button(
                    format!("provider-model-select-{model}"),
                    if selected { "✓" } else { "○" },
                    Action::ToggleModel(model.clone()),
                    !disabled && !self.auto_include,
                    Some(selected),
                    cx,
                )
                .aria_label(SharedString::from(format!(
                    "{}: {model}",
                    t(self.locale, P::SelectSome)
                )));
            let remove = self.button(
                format!("provider-model-delete-{model}"),
                t(self.locale, P::Remove),
                Action::DeleteModel(model.clone()),
                !disabled,
                None,
                cx,
            );
            rows = rows.child(
                div()
                    .w_full()
                    .min_w_0()
                    .min_h(px(40.))
                    .border_b_1()
                    .border_color(rgb(self.palette.border))
                    .flex()
                    .items_center()
                    .gap(px(8.))
                    .child(choose)
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .text_size(px(12.))
                            .whitespace_normal()
                            .child(text!(model)),
                    )
                    .child(remove),
            );
        }
        if count == 0 {
            rows = rows.child(self.caption(if self.catalog.is_empty() {
                P::NoModels
            } else {
                P::EmptyFilter
            }));
        }
        content = content
            .child(rows)
            .child(self.field("provider-model-add", P::AddModel, disabled, cx))
            .child(div().flex().child(self.button(
                "provider-model-add-button",
                t(self.locale, P::AddModel),
                Action::AddModel,
                !disabled,
                None,
                cx,
            )))
            .child(self.caption(P::ManualModelHint));
        if !self.auto_include && self.selected.is_empty() {
            content = content.child(self.caption(P::NothingSelected));
        }
        if self.models_dirty {
            content = content.child(self.caption(P::Unsaved));
        }
        content
    }
    /// Keep the existing explicit actions reachable while only their body
    /// scrolls. The action IDs, admission and confirmation flow are unchanged.
    fn action_footer(&mut self, cx: &mut Context<Self>) -> Option<Div> {
        if self.confirmation.is_some() {
            return None;
        }
        let (save_id, save_action, save_enabled, back_id, back_label) = match &self.page {
            Page::Form(id) => (
                "provider-form-save",
                Action::SaveForm,
                self.state.can_write() && (id.is_none() || self.form_dirty),
                "provider-form-cancel",
                P::Cancel,
            ),
            Page::KeyForm { key, .. } => (
                "provider-key-save",
                Action::SaveKey,
                self.state.can_write() && (key.is_none() || self.form_dirty),
                "provider-key-cancel",
                P::Cancel,
            ),
            Page::Models(id) if self.provider(id).is_some() => (
                "provider-model-save",
                Action::SaveModels,
                self.state.can_write() && self.models_dirty,
                "provider-model-cancel",
                P::Back,
            ),
            _ => return None,
        };
        let palette = self.palette;
        let save = self.button(
            save_id,
            t(self.locale, P::Save),
            save_action,
            save_enabled,
            None,
            cx,
        );
        let back = self.button(
            back_id,
            t(self.locale, back_label),
            Action::Back,
            !self.busy(),
            None,
            cx,
        );
        Some(
            div()
                .w_full()
                .min_w_0()
                .flex_none()
                .flex()
                .items_center()
                .justify_end()
                .gap(px(8.))
                .px(px(16.))
                .py(px(10.))
                .rounded(px(10.))
                .border_1()
                .border_color(rgb(palette.border))
                .bg(rgb(palette.surface))
                .child(back)
                .child(save),
        )
    }
    fn missing(&mut self, cx: &mut Context<Self>) -> Div {
        div()
            .flex()
            .flex_col()
            .gap(px(12.))
            .child(self.header(t(self.locale, P::Providers), cx))
            .child(self.caption(P::Conflict))
    }
    fn confirmation(&mut self, confirmation: &Confirmation, cx: &mut Context<Self>) -> Div {
        let (title, hint, name) = match confirmation {
            Confirmation::RemoveProvider(id) => (
                P::RemoveProviderTitle,
                P::RemoveProviderHint,
                self.provider(id).map_or_else(|| id.clone(), |p| p.name),
            ),
            Confirmation::RemoveKey { provider, key } => (
                P::RemoveKeyTitle,
                P::RemoveKeyHint,
                self.provider(provider)
                    .and_then(|p| p.keys.into_iter().find(|k| &k.id == key))
                    .map_or_else(|| key.clone(), |k| k.name),
            ),
            Confirmation::Recover => (P::RecoverTitle, P::RecoverHint, String::new()),
            Confirmation::Discard(_) => (P::Unsaved, P::DiscardHint, String::new()),
            Confirmation::Close { .. } => (P::Unsaved, P::CloseDraftHint, String::new()),
        };
        div()
            .w_full()
            .min_w_0()
            .flex()
            .flex_col()
            .gap(px(12.))
            .child(
                div()
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_size(px(16.))
                    .whitespace_normal()
                    .child(text!(t(self.locale, title))),
            )
            .when(!name.is_empty(), |c| {
                c.child(
                    div()
                        .text_size(px(13.))
                        .whitespace_normal()
                        .child(text!(name)),
                )
            })
            .child(self.caption(hint))
            .child(
                div()
                    .flex()
                    .flex_wrap()
                    .gap(px(6.))
                    .child(self.button(
                        "provider-keep",
                        t(
                            self.locale,
                            if matches!(confirmation, Confirmation::Close { .. }) {
                                P::ContinueEditing
                            } else {
                                P::Keep
                            },
                        ),
                        Action::Keep,
                        !self.busy(),
                        None,
                        cx,
                    ))
                    .child(self.button(
                        "provider-confirm",
                        t(
                            self.locale,
                            match confirmation {
                                Confirmation::Close { .. } => P::DiscardClose,
                                Confirmation::Discard(_) => P::Discard,
                                _ => P::Confirm,
                            },
                        ),
                        Action::Confirm,
                        !self.busy(),
                        None,
                        cx,
                    )),
            )
    }
}
impl Drop for ProviderUi {
    fn drop(&mut self) {
        self.cancel();
        if let Some(task) = self.task.take() {
            task.detach();
        }
    }
}
impl Render for ProviderUi {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if let Some(action) = self.pending_action.take() {
            self.act(action, window, cx);
        }
        let previously_focused = self
            .controls
            .iter()
            .find(|(_, control)| control.focus.is_focused(window))
            .map(|(id, _)| id.clone());
        self.used_controls.clear();
        let status = self.status(cx);
        let content = if let Some(confirmation) = self.confirmation.clone() {
            self.confirmation(&confirmation, cx)
        } else {
            match self.page.clone() {
                Page::List => self.list(cx),
                Page::Details(id) => self.details(&id, cx),
                Page::Form(id) => self.provider_form(id.is_some(), cx),
                Page::KeyForm { key, .. } => self.key_form(key.is_some(), cx),
                Page::Models(id) => self.model_picker(&id, cx),
            }
        };
        let footer = self.action_footer(cx);
        self.controls
            .retain(|id, _| self.used_controls.contains(id));
        let requested = self.pending_focus.take();
        let handle = requested
            .as_ref()
            .and_then(|id| {
                self.controls.get(id).map(|c| c.focus.clone()).or_else(|| {
                    self.inputs
                        .get(id.as_str())
                        .map(|i| i.read(cx).focus_handle())
                })
            })
            .or_else(|| {
                previously_focused
                    .as_ref()
                    .filter(|id| !self.controls.contains_key(*id))
                    .map(|_| self.focus.clone())
            });
        if let Some(handle) = handle {
            let weak = cx.entity().downgrade();
            window.on_next_frame(move |window, cx| {
                handle.focus(window, cx);
                let _ = weak.update(cx, |view, cx| view.reveal_focused(window, cx));
            });
        }
        div()
            .id("provider-native-root")
            .track_focus(&self.focus)
            .key_context("ProviderManager")
            .on_key_down(cx.listener(Self::key_down))
            .w_full()
            .h_full()
            .min_w_0()
            .min_h_0()
            .flex()
            .flex_col()
            .gap(px(8.))
            .overflow_hidden()
            .text_color(rgb(self.palette.text))
            .font(theme::ui_font(cx, self.locale))
            .text_size(px(13.))
            .child(
                div()
                    .id("provider-scroll")
                    .w_full()
                    .flex_1()
                    .min_w_0()
                    .min_h_0()
                    .overflow_y_scroll()
                    .track_scroll(&self.scroll)
                    .child(
                        div()
                            .w_full()
                            .min_w_0()
                            .flex()
                            .flex_col()
                            .gap(px(12.))
                            .p(px(16.))
                            .rounded(px(10.))
                            .border_1()
                            .border_color(rgb(self.palette.border))
                            .bg(rgb(self.palette.surface))
                            .child(status)
                            .child(content),
                    ),
            )
            .when_some(footer, |root, footer| root.child(footer))
    }
}
fn is_footer_control(id: &str) -> bool {
    matches!(
        id,
        "provider-form-save"
            | "provider-form-cancel"
            | "provider-key-save"
            | "provider-key-cancel"
            | "provider-model-save"
            | "provider-model-cancel"
    )
}
fn field_label(id: &str) -> P {
    match id {
        "provider-id" => P::Id,
        "provider-name" => P::Name,
        "provider-url" => P::BaseUrl,
        "provider-env" => P::Environment,
        "provider-models" => P::ModelIds,
        "provider-key-id" => P::KeyId,
        "provider-key-name" => P::KeyName,
        "provider-key-env" => P::KeyEnvironment,
        "provider-model-add" => P::AddModel,
        _ => P::Search,
    }
}
fn palette_key(p: Palette) -> [u32; 13] {
    [
        p.background,
        p.surface,
        p.inset,
        p.border,
        p.text,
        p.muted,
        p.accent,
        p.selected,
        p.warning,
        p.warning_bg,
        p.danger,
        p.danger_bg,
        0,
    ]
}
fn parse_models(input: &str) -> Vec<String> {
    input
        .split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
        .collect()
}
fn valid_identifier(value: &str) -> bool {
    value.len() <= 128
        && value
            .as_bytes()
            .first()
            .is_some_and(u8::is_ascii_alphanumeric)
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b))
        && value != "group"
}
fn valid_environment(value: &str) -> bool {
    value.len() <= 128
        && value
            .as_bytes()
            .first()
            .is_some_and(|b| b.is_ascii_alphabetic() || *b == b'_')
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_')
}
fn valid_name(value: &str) -> bool {
    !value.trim().is_empty() && value.len() <= 256 && !value.chars().any(char::is_control)
}
fn valid_model(s: &str) -> bool {
    s.len() <= 256
        && s.as_bytes().first().is_some_and(u8::is_ascii_alphanumeric)
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"._-:@/".contains(&b))
        && s.split('/').all(|p| !matches!(p, "" | "." | ".."))
}
fn conflicting_field<T: PartialEq + ?Sized>(base: &T, current: &T, draft: &T) -> bool {
    current != base && draft != base && current != draft
}

fn provider_draft_conflicts(
    base: &Provider,
    current: &Provider,
    name: &str,
    url: &str,
    protocol: &str,
    enabled: bool,
    allow_http: bool,
) -> bool {
    current.deleted
        || current.id != base.id
        || conflicting_field(base.name.as_str(), current.name.as_str(), name)
        || conflicting_field(base.base_url.as_str(), current.base_url.as_str(), url)
        || conflicting_field(base.protocol.as_str(), current.protocol.as_str(), protocol)
        || conflicting_field(&base.enabled, &current.enabled, &enabled)
        || conflicting_field(
            &base.allow_insecure_http,
            &current.allow_insecure_http,
            &allow_http,
        )
}

fn merge_key_fields(
    base: Option<&Key>,
    current: Option<&Key>,
    name: &str,
    env: &str,
    enabled: bool,
) -> Result<(String, String, bool), ()> {
    let Some(base) = base else {
        // "Add key" must not become an implicit edit of a key created by
        // another operator (or of an already retired identity).
        return if current.is_some() {
            Err(())
        } else {
            Ok((name.into(), env.into(), enabled))
        };
    };
    let current = current.ok_or(())?;
    if current.deleted
        || current.id != base.id
        || conflicting_field(base.name.as_str(), current.name.as_str(), name)
        || conflicting_field(base.env.as_str(), current.env.as_str(), env)
        || conflicting_field(&base.enabled, &current.enabled, &enabled)
    {
        return Err(());
    }
    Ok((
        if name == base.name {
            &current.name
        } else {
            name
        }
        .into(),
        if env == base.env { &current.env } else { env }.into(),
        if enabled == base.enabled {
            current.enabled
        } else {
            enabled
        },
    ))
}

fn merge_models(
    current: &Provider,
    original: Option<&Provider>,
    draft: &[String],
    selected: &BTreeSet<String>,
    auto: bool,
) -> Result<(Vec<String>, Vec<String>, bool), ()> {
    let original = original.ok_or(())?;
    let base_selected: BTreeSet<_> = original.selected_models.iter().cloned().collect();
    let current_selected: BTreeSet<_> = current.selected_models.iter().cloned().collect();
    if current.deleted
        || current.id != original.id
        // A changed exposure mode and the other operator's edited selection
        // describe conflicting visibility intent, even with different keys.
        || (current.auto_include != original.auto_include
            && auto != current.auto_include
            && selected != &base_selected)
        || (auto != original.auto_include
            && auto != current.auto_include
            && current_selected != base_selected)
        // Do not silently erase another operator's new selection by deleting
        // that model, or silently drop a draft selection of their removed model.
        || original.models.iter().any(|model| {
            (!draft.contains(model)
                && current_selected.contains(model)
                && !base_selected.contains(model))
                || (!current.models.contains(model)
                    && selected.contains(model)
                    && !base_selected.contains(model))
        })
    {
        return Err(());
    }
    let removed: BTreeSet<_> = original
        .models
        .iter()
        .filter(|m| !draft.contains(m))
        .collect();
    let mut models: Vec<_> = current
        .models
        .iter()
        .filter(|m| !removed.contains(m))
        .cloned()
        .collect();
    for model in draft {
        if !original.models.contains(model) && !models.contains(model) {
            models.push(model.clone());
        }
    }
    let mut chosen: BTreeSet<_> = current.selected_models.iter().cloned().collect();
    for model in &original.selected_models {
        if !selected.contains(model) {
            chosen.remove(model);
        }
    }
    for model in selected {
        if !original.selected_models.contains(model) {
            chosen.insert(model.clone());
        }
    }
    chosen.retain(|m| models.contains(m));
    Ok((
        models,
        chosen.into_iter().collect(),
        if auto != original.auto_include {
            auto
        } else {
            current.auto_include
        },
    ))
}

fn utc_stamp(seconds: u64) -> String {
    if seconds > 253_402_300_799 {
        return "—".to_owned();
    }
    let days = (seconds / 86_400) as i64;
    let z = days + 719_468;
    let era = z / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let mut year = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = mp + if mp < 10 { 3 } else { -9 };
    year += i64::from(month <= 2);
    let remainder = seconds % 86_400;
    format!(
        "{year:04}-{month:02}-{day:02} {:02}:{:02} UTC",
        remainder / 3600,
        (remainder % 3600) / 60
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    fn provider() -> Provider {
        Provider {
            id: "fixture".into(),
            name: "Fixture".into(),
            base_url: "http://127.0.0.1:1/v1".into(),
            protocol: "chat".into(),
            enabled: true,
            deleted: false,
            allow_insecure_http: false,
            keys: Vec::new(),
            models: vec!["a".into(), "b".into()],
            selected_models: vec!["a".into()],
            auto_include: false,
            catalog_checked_at: None,
            catalog_error: None,
        }
    }
    #[test]
    fn catalog_timestamp_formats_saved_utc_value() {
        assert_eq!(utc_stamp(0), "1970-01-01 00:00 UTC");
        assert_eq!(utc_stamp(951_782_400), "2000-02-29 00:00 UTC");
        assert_eq!(utc_stamp(u64::MAX), "—");
    }
    #[test]
    fn model_draft_merge_preserves_concurrent_additions() {
        let original = provider();
        let mut current = original.clone();
        current.models.push("external".into());
        current.selected_models.push("external".into());
        let draft = vec!["a".into(), "manual".into()];
        let selected = ["manual".into()].into_iter().collect();
        let (models, chosen, _) =
            merge_models(&current, Some(&original), &draft, &selected, false).unwrap();
        assert_eq!(models, vec!["a", "external", "manual"]);
        assert_eq!(chosen, vec!["external", "manual"]);
    }
    #[test]
    fn model_input_does_not_accept_url_or_traversal() {
        for id in ["a/../b", "a?key=x", "", "a b"] {
            assert!(!valid_model(id));
        }
        assert!(valid_model("namespace/model:latest"));
        assert!(valid_identifier("fixture.a"));
        assert!(!valid_identifier("group"));
        assert!(!valid_environment("value-with-dashes"));
        assert!(valid_environment("SYNTHETIC_KEY"));
        assert!(!valid_name("bad\nname"));
        assert_eq!(parse_models(" a, b ,,"), vec!["a", "b"]);
    }

    #[test]
    fn provider_two_operator_same_field_edits_are_rejected() {
        let base = provider();
        let mut other = base.clone();
        other.name = "Operator B name".into();
        assert!(provider_draft_conflicts(
            &base,
            &other,
            "Operator A name",
            &base.base_url,
            &base.protocol,
            base.enabled,
            base.allow_insecure_http,
        ));
        other = base.clone();
        other.base_url = "https://operator-b.invalid/v1".into();
        assert!(provider_draft_conflicts(
            &base,
            &other,
            &base.name,
            "https://operator-a.invalid/v1",
            &base.protocol,
            base.enabled,
            base.allow_insecure_http,
        ));
        // A reread must not replace the base with the new current value: the
        // unchanged original still detects the conflict on another Save.
        assert_eq!(base.name, "Fixture");
        assert_eq!(base.base_url, "http://127.0.0.1:1/v1");
    }

    #[test]
    fn provider_disjoint_or_identical_edits_are_not_false_conflicts() {
        let base = provider();
        let mut other = base.clone();
        other.base_url = "https://operator-b.invalid/v1".into();
        assert!(!provider_draft_conflicts(
            &base,
            &other,
            "Operator A name",
            &base.base_url,
            &base.protocol,
            base.enabled,
            base.allow_insecure_http,
        ));
        other.name = "Shared new name".into();
        assert!(!provider_draft_conflicts(
            &base,
            &other,
            &other.name,
            &other.base_url,
            &other.protocol,
            other.enabled,
            other.allow_insecure_http,
        ));
        other.deleted = true;
        assert!(provider_draft_conflicts(
            &base,
            &other,
            &base.name,
            &base.base_url,
            &base.protocol,
            base.enabled,
            base.allow_insecure_http,
        ));
    }

    #[test]
    fn key_two_operator_rotation_rejects_conflicts_and_keeps_unedited_fields() {
        let base = Key {
            id: "primary".into(),
            name: "Original".into(),
            env: "ORIGINAL_KEY".into(),
            enabled: true,
            deleted: false,
            masked: "••••".into(),
        };
        let mut other = base.clone();
        other.env = "OPERATOR_B_KEY".into();
        assert!(
            merge_key_fields(
                Some(&base),
                Some(&other),
                &base.name,
                "OPERATOR_A_KEY",
                true,
            )
            .is_err()
        );
        other = base.clone();
        other.name = "Operator B name".into();
        other.enabled = false;
        assert_eq!(
            merge_key_fields(
                Some(&base),
                Some(&other),
                &base.name,
                "OPERATOR_A_KEY",
                true,
            ),
            Ok(("Operator B name".into(), "OPERATOR_A_KEY".into(), false))
        );
        assert!(merge_key_fields(None, Some(&other), "New key", "NEW_KEY", true).is_err());
        assert!(merge_key_fields(Some(&base), None, "New name", "NEW_KEY", true).is_err());
        other.deleted = true;
        assert!(merge_key_fields(Some(&base), Some(&other), "New name", "NEW_KEY", true).is_err());
    }

    #[test]
    fn model_visibility_and_catalog_cross_edits_do_not_erase_other_intent() {
        let base = provider();
        let mut other = base.clone();
        other.auto_include = true;
        let selected: BTreeSet<_> = ["b".into()].into_iter().collect();
        assert!(merge_models(&other, Some(&base), &base.models, &selected, false).is_err());

        other = base.clone();
        other.selected_models = vec!["b".into()];
        let selected: BTreeSet<_> = base.selected_models.iter().cloned().collect();
        assert!(merge_models(&other, Some(&base), &base.models, &selected, true).is_err());
        assert!(merge_models(&other, Some(&base), &["a".into()], &selected, false).is_err());

        other = base.clone();
        other.models = vec!["a".into()];
        let selected: BTreeSet<_> = ["b".into()].into_iter().collect();
        assert!(merge_models(&other, Some(&base), &base.models, &selected, false).is_err());
    }
}

#[cfg(feature = "visual-test")]
#[allow(dead_code)]
impl ProviderUi {
    pub(super) fn preview_fixture(&mut self, locale: Locale, cx: &mut Context<Self>) {
        assert!(!self.pending.load(Ordering::Acquire));
        self.locale = locale;
        self.state = ProviderState::default();
        let fixture: crate::provider_bridge::Snapshot = serde_json::from_value(json!({
            "version": "0.2.0", "schema_version": 1, "revision": 7,
            "token": format!("7:{}", "0".repeat(64)),
            "providers": [
                {"id":"openai", "name":"OpenAI", "base_url":"https://api.openai.invalid/v1",
                 "protocol":"responses", "enabled":true, "deleted":false, "allow_insecure_http":false,
                 "keys":[{"id":"primary", "name":"Default", "env":"SYNTHETIC_OPENAI_KEY", "enabled":true,"deleted":false,"masked":"••••"}],
                 "models":["gpt-5.2-codex", "gpt-5.2"], "selected_models":["gpt-5.2-codex"],
                 "auto_include":false, "catalog_checked_at":1793145600, "catalog_error":null},
                {"id":"deepseek", "name":"DeepSeek", "base_url":"https://api.deepseek.invalid/v1",
                 "protocol":"chat", "enabled":true, "deleted":false, "allow_insecure_http":false,
                 "keys":[{"id":"primary", "name":"Default", "env":"SYNTHETIC_DEEPSEEK_KEY", "enabled":true,"deleted":false,"masked":"••••"}],
                 "models":["deepseek-chat", "deepseek-reasoner"], "selected_models":[],
                 "auto_include":true, "catalog_checked_at":1793145600, "catalog_error":null},
                {"id":"ollama", "name":"Ollama", "base_url":"http://127.0.0.1:11434/v1",
                 "protocol":"chat", "enabled":true, "deleted":false, "allow_insecure_http":false,
                 "keys":[], "models":["qwen3-coder:30b"], "selected_models":[],
                 "auto_include":true, "catalog_checked_at":null, "catalog_error":null}
            ]
        })).expect("static synthetic provider fixture");
        self.state.finish(&Request::list(), Ok(fixture));
        self.page = Page::List;
        cx.notify();
    }
}
