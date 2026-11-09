//! Plugin-driven Agent configuration UI. Ownership is never runtime health.
use crate::{
    cli_bridge::{
        self, BridgeError, Cancellation, ErrorKind, GatewayInfo, Ownership, Reply, Request,
    },
    localization::Locale,
    plugin_catalog::OwnedAgent,
    text_input::{InputEvent, TextInput},
    theme::{self, Appearance, Palette},
};
use gpui::{
    Context, Entity, FocusHandle, FontWeight, IntoElement, Render, ScrollAnchor, ScrollHandle,
    SharedString, Subscription, Window, div, prelude::*, px, rgb, text,
};
use gpui_kit_material::components::{ButtonSize, ButtonVariant, MaterialButton, material_button};
use serde_json::Value;
use std::{
    collections::{BTreeMap, BTreeSet},
    net::SocketAddr,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};

#[derive(Clone, PartialEq, Eq, PartialOrd, Ord)]
struct Key {
    owner: String,
    id: String,
}
impl Key {
    fn of(adapter: &OwnedAgent) -> Self {
        Self {
            owner: adapter.owner.clone(),
            id: adapter.id.clone(),
        }
    }
}
#[derive(Default)]
struct Draft {
    config: String,
    model: Option<String>,
    observation: Option<Observation>,
}
#[derive(Clone, Copy, PartialEq, Eq, Default)]
enum SelectionState {
    Qualified,
    External,
    Unset,
    #[default]
    Unknown,
}
#[derive(Clone)]
struct PublicField {
    id: String,
    labels: BTreeMap<String, String>,
    value: Option<String>,
}
impl PublicField {
    fn label(&self, locale: Locale) -> &str {
        let key = locale.language_tag();
        self.labels
            .get(key)
            .or_else(|| self.labels.get("en"))
            .map(String::as_str)
            .unwrap_or(&self.id)
    }
}
#[derive(Clone)]
struct Observation {
    status: Ownership,
    model: Option<String>,
    selection_state: SelectionState,
    public_fields: Vec<PublicField>,
}
impl Observation {
    fn model_label(&self, locale: Locale) -> &str {
        match self.selection_state {
            SelectionState::Qualified | SelectionState::External => {
                self.model.as_deref().unwrap_or(copy(locale, Text::Unknown))
            }
            SelectionState::Unset => copy(locale, Text::Missing),
            SelectionState::Unknown => copy(locale, Text::Unknown),
        }
    }
}
#[derive(Clone, Copy, PartialEq, Eq)]
enum Mode {
    Connect,
    Model,
}
impl Mode {
    fn preview(self) -> &'static str {
        match self {
            Self::Connect => "preview",
            Self::Model => "model-preview",
        }
    }
    fn apply(self) -> &'static str {
        match self {
            Self::Connect => "connect",
            Self::Model => "model",
        }
    }
}
#[derive(Clone, PartialEq, Eq)]
struct Stamp {
    generation: u64,
    key: Key,
    config: String,
    model: Option<String>,
    address: Option<SocketAddr>,
}
struct Ticket {
    stamp: Stamp,
    mode: Mode,
    revision: String,
    changed: usize,
}
struct Catalog {
    address: SocketAddr,
    models: Vec<String>,
}
#[derive(Clone)]
enum Action {
    Select(Key),
    Details,
    Model(String),
    Refresh,
    Preview(Mode),
    Apply,
    AskDisconnect,
    Disconnect,
    Keep,
    Cancel,
}
#[derive(Clone, Copy)]
enum WorkKind {
    Refresh,
    Preview(Mode),
    Apply,
    Disconnect,
}
struct PluginData {
    observation: Observation,
    revision: Option<String>,
    changed: usize,
    restored: Option<usize>,
    preserved: Option<usize>,
}
enum Completion {
    Refresh {
        selection: Result<PluginData, BridgeError>,
        catalog: Option<Result<GatewayInfo, BridgeError>>,
    },
    Preview {
        mode: Mode,
        result: Result<PluginData, BridgeError>,
        catalog: Option<GatewayInfo>,
    },
    Mutation {
        result: Result<PluginData, BridgeError>,
        selection: Option<Result<PluginData, BridgeError>>,
    },
}
enum Notice {
    Error(BridgeError),
    Updated,
    Disconnected(usize, usize),
    ReadAgain,
    NoAdapters,
}

/// Compact, capability-driven configuration workflows shared by all active adapters.
pub(crate) struct AgentConnections {
    adapters: Vec<OwnedAgent>,
    selected: Option<Key>,
    drafts: BTreeMap<Key, Draft>,
    address: Option<SocketAddr>,
    catalog: Option<Catalog>,
    ticket: Option<Ticket>,
    generation: u64,
    confirm: Option<Stamp>,
    notice: Option<Notice>,
    locale: Locale,
    appearance: Appearance,
    config: Entity<TextInput>,
    search: Entity<TextInput>,
    query: String,
    details_open: bool,
    editor_open: bool,
    pending: Arc<AtomicBool>,
    cancellation: Option<Cancellation>,
    active: bool,
    focus: BTreeMap<String, FocusHandle>,
    scroll: ScrollHandle,
    model_scroll: ScrollHandle,
    anchors: BTreeMap<String, ScrollAnchor>,
    _subscriptions: Vec<Subscription>,
    #[cfg(feature = "visual-test")]
    fixture_mode: bool,
}
impl AgentConnections {
    pub(crate) fn new(cx: &mut Context<Self>) -> Self {
        let locale = Locale::system();
        let config = cx.new(|cx| {
            let mut input = TextInput::new(
                "agent-config",
                "",
                copy(locale, Text::DefaultPath),
                copy(locale, Text::Path),
                cx,
            );
            input.set_limit(4096);
            input
        });
        let search = cx.new(|cx| {
            let mut input = TextInput::new(
                "agent-model-search",
                "",
                copy(locale, Text::Search),
                copy(locale, Text::Search),
                cx,
            );
            input.set_limit(512);
            input
        });
        let mut subscriptions = Vec::new();
        subscriptions.push(cx.subscribe(&config, |view, input, event, cx| {
            if *event == InputEvent::Changed {
                let text = input.read(cx).text().to_owned();
                if let Some(draft) = view
                    .selected
                    .as_ref()
                    .and_then(|key| view.drafts.get_mut(key))
                    && draft.config != text
                {
                    draft.config = text;
                    draft.observation = None;
                    view.invalidate();
                }
            }
            if *event == InputEvent::Escape {
                view.act(Action::Keep, cx);
            }
            cx.notify();
        }));
        subscriptions.push(cx.subscribe(&search, |view, input, event, cx| {
            if *event == InputEvent::Changed {
                let query = input.read(cx).text().to_lowercase();
                if view.query != query {
                    view.query = query;
                    view.invalidate();
                }
            }
            if *event == InputEvent::Escape {
                view.act(Action::Keep, cx);
            }
            cx.notify();
        }));
        Self {
            adapters: Vec::new(),
            selected: None,
            drafts: BTreeMap::new(),
            address: None,
            catalog: None,
            ticket: None,
            generation: 0,
            confirm: None,
            notice: None,
            locale,
            appearance: Appearance::System,
            config,
            search,
            query: String::new(),
            details_open: false,
            editor_open: false,
            pending: Arc::new(AtomicBool::new(false)),
            cancellation: None,
            active: false,
            focus: BTreeMap::new(),
            scroll: ScrollHandle::new(),
            model_scroll: ScrollHandle::new(),
            anchors: BTreeMap::new(),
            _subscriptions: subscriptions,
            #[cfg(feature = "visual-test")]
            fixture_mode: false,
        }
    }
    pub(crate) fn set_adapters(&mut self, adapters: Vec<OwnedAgent>, cx: &mut Context<Self>) {
        const MAX_DRAFTS: usize = 1024;
        let mut seen = BTreeSet::new();
        let adapters: Vec<_> = adapters
            .into_iter()
            .filter(|a| {
                valid_label(&a.owner, 128)
                    && valid_label(&a.id, 128)
                    && valid_label(&a.name, 256)
                    && match &a.definition {
                        Some(d) => d.id == a.id && d.validate().is_ok(),
                        None => a.engine.as_ref().is_some_and(|s| valid_label(s, 128)),
                    }
                    && seen.insert(Key::of(a))
            })
            .take(MAX_DRAFTS)
            .collect();
        if self.adapters.len() == adapters.len()
            && self.adapters.iter().zip(&adapters).all(|(old, new)| {
                old.owner == new.owner
                    && old.id == new.id
                    && old.name == new.name
                    && old.engine == new.engine
                    && old.definition == new.definition
            })
        {
            return;
        }
        self.cancel();
        self.invalidate();
        self.adapters = adapters;
        let active: BTreeSet<_> = self.adapters.iter().map(Key::of).collect();
        if self
            .selected
            .as_ref()
            .is_none_or(|key| !active.contains(key))
        {
            self.selected = self.adapters.first().map(Key::of);
        }
        // Withdrawing a contribution invalidates observations, not the user's
        // explicit target or model. Reappearing keys recover their own draft.
        for draft in self.drafts.values_mut() {
            draft.observation = None;
        }
        for key in &active {
            self.drafts.entry(key.clone()).or_default();
        }
        let excess = self.drafts.len().saturating_sub(MAX_DRAFTS);
        let evicted: Vec<_> = self
            .drafts
            .keys()
            .filter(|key| !active.contains(*key) && self.selected.as_ref() != Some(*key))
            .take(excess)
            .cloned()
            .collect();
        for key in evicted {
            self.drafts.remove(&key);
        }
        self.load_config(cx);
        cx.notify();
    }
    /// Explicitly rereads the current target and live gateway catalog.
    pub(crate) fn refresh(&mut self, cx: &mut Context<Self>) {
        self.act(Action::Refresh, cx);
    }
    pub(crate) fn configure(&mut self, address: SocketAddr, cx: &mut Context<Self>) {
        let address = valid_address(address).then_some(address);
        if self.address != address {
            self.cancel();
            self.invalidate();
            self.address = address;
            self.catalog = None;
            cx.notify();
        }
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
        self.active || self.pending.load(Ordering::Acquire)
    }
    pub(crate) fn cancel(&self) {
        if let Some(cancellation) = &self.cancellation {
            cancellation.cancel();
        }
    }
    pub(crate) fn shutdown_pending(&self) -> Arc<AtomicBool> {
        self.pending.clone()
    }
    /// Synthetic presentation only. Never reads files, launches CLI, or opens sockets.
    #[cfg(feature = "visual-test")]
    pub(crate) fn preview_fixture(&mut self, scene: &str, cx: &mut Context<Self>) {
        if self.busy() {
            self.cancel();
            return;
        }
        let adapters = (0..3)
            .map(|index| OwnedAgent {
                owner: format!("agent.fixture-{index}"),
                id: format!("fixture-{index}"),
                name: format!("Fixture Agent {}", index + 1),
                engine: Some("fixture/1".into()),
                definition: None,
            })
            .collect();
        self.set_adapters(adapters, cx);
        self.fixture_mode = true;
        self.editor_open = scene != "agents";
        let address = SocketAddr::from(([127, 0, 0, 1], 8787));
        self.configure(address, cx);
        self.catalog = Some(Catalog {
            address,
            models: vec![
                "fixture/model-a".into(),
                "fixture/model-b".into(),
                "fixture/model-c".into(),
            ],
        });
        self.query.clear();
        self.search.update(cx, |input, cx| {
            input.set_text("", cx);
        });
        let model_change = scene.contains("update");
        let connected = model_change || matches!(scene, "connected" | "drifted");
        if let Some(draft) = self
            .selected
            .as_ref()
            .and_then(|key| self.drafts.get_mut(key))
        {
            draft.config = "/synthetic/agent-config".into();
            draft.model = Some(
                if model_change {
                    "fixture/model-b"
                } else {
                    "fixture/model-a"
                }
                .into(),
            );
            draft.observation = Some(Observation {
                selection_state: SelectionState::Qualified,
                public_fields: Vec::new(),
                status: if scene == "drifted" {
                    Ownership::Drifted
                } else if connected {
                    Ownership::Connected
                } else {
                    Ownership::Unmanaged
                },
                model: Some(
                    if connected {
                        "fixture/model-a"
                    } else {
                        "fixture/original"
                    }
                    .into(),
                ),
            });
        }
        self.load_config(cx);
        self.notice = None;
        if (matches!(
            scene,
            "codex" | "codex-update" | "preview" | "model-preview"
        ) || model_change)
            && let Some(stamp) = self.stamp()
        {
            self.ticket = Some(Ticket {
                stamp,
                mode: if model_change {
                    Mode::Model
                } else {
                    Mode::Connect
                },
                revision: "synthetic-preview-not-for-cli".into(),
                changed: if model_change { 1 } else { 7 },
            });
        }
        cx.notify();
    }
    fn invalidate(&mut self) {
        self.generation = self.generation.wrapping_add(1);
        self.ticket = None;
        self.confirm = None;
    }
    fn load_config(&mut self, cx: &mut Context<Self>) {
        let value = self
            .selected
            .as_ref()
            .and_then(|k| self.drafts.get(k))
            .map(|d| d.config.clone())
            .unwrap_or_default();
        self.config.update(cx, |input, cx| {
            input.set_text(&value, cx);
        });
    }
    fn stamp(&self) -> Option<Stamp> {
        let key = self.selected.clone()?;
        let draft = self.drafts.get(&key)?;
        Some(Stamp {
            generation: self.generation,
            key,
            config: draft.config.clone(),
            model: draft.model.clone(),
            address: self.address,
        })
    }
    fn observation(&self) -> Option<&Observation> {
        self.selected
            .as_ref()
            .and_then(|k| self.drafts.get(k))
            .and_then(|d| d.observation.as_ref())
    }
    fn model_available(&self) -> bool {
        self.stamp().is_some_and(|s| {
            s.model.as_ref().is_some_and(|model| {
                self.catalog.as_ref().is_some_and(|catalog| {
                    Some(catalog.address) == s.address && catalog.models.contains(model)
                })
            })
        })
    }
    fn can_apply(&self) -> bool {
        !self.busy()
            && self.model_available()
            && self
                .ticket
                .as_ref()
                .is_some_and(|t| Some(&t.stamp) == self.stamp().as_ref())
    }
    fn act(&mut self, action: Action, cx: &mut Context<Self>) {
        if matches!(action, Action::Cancel) {
            self.cancel();
            self.invalidate();
            cx.notify();
            return;
        }
        if matches!(action, Action::Keep) {
            self.confirm = None;
            cx.notify();
            return;
        }
        if self.busy() {
            return;
        }
        match action {
            Action::Details => self.details_open = !self.details_open,
            Action::Select(key) => {
                if self.selected.as_ref() == Some(&key) {
                    self.editor_open = !self.editor_open;
                    if self.editor_open && self.observation().is_none() {
                        self.start(WorkKind::Refresh, None, cx);
                    }
                    cx.notify();
                    return;
                }
                if self.drafts.contains_key(&key) {
                    self.invalidate();
                    self.selected = Some(key);
                    self.details_open = false;
                    self.editor_open = true;
                    self.notice = None;
                    self.load_config(cx);
                    self.start(WorkKind::Refresh, None, cx);
                }
            }
            Action::Model(model) => {
                if self
                    .catalog
                    .as_ref()
                    .is_some_and(|c| c.models.contains(&model))
                {
                    if let Some(draft) = self.selected.as_ref().and_then(|k| self.drafts.get_mut(k))
                    {
                        draft.model = Some(model);
                    }
                    self.invalidate();
                    self.notice = None;
                }
            }
            Action::Refresh => self.start(WorkKind::Refresh, None, cx),
            Action::Preview(mode) => {
                if self.model_available() {
                    self.start(WorkKind::Preview(mode), None, cx);
                }
            }
            Action::Apply => {
                if self.can_apply()
                    && let Some(ticket) = self.ticket.take()
                {
                    self.start(WorkKind::Apply, Some((ticket.mode, ticket.revision)), cx);
                }
            }
            Action::AskDisconnect => {
                self.ticket = None;
                self.confirm = self.stamp();
            }
            Action::Disconnect => {
                if self.confirm.as_ref() == self.stamp().as_ref() && self.confirm.is_some() {
                    self.start(WorkKind::Disconnect, None, cx);
                }
            }
            Action::Keep | Action::Cancel => {}
        }
        cx.notify();
    }
    fn start(&mut self, kind: WorkKind, approved: Option<(Mode, String)>, cx: &mut Context<Self>) {
        if self.busy() {
            return;
        }
        #[cfg(feature = "visual-test")]
        if self.fixture_mode {
            self.ticket = None;
            self.notice = Some(Notice::ReadAgain);
            cx.notify();
            return;
        }
        self.invalidate();
        let Some(stamp) = self.stamp() else {
            self.notice = Some(Notice::NoAdapters);
            return;
        };
        if matches!(kind, WorkKind::Refresh) {
            self.catalog = None;
        }
        if matches!(
            kind,
            WorkKind::Refresh | WorkKind::Apply | WorkKind::Disconnect
        ) && let Some(draft) = self.drafts.get_mut(&stamp.key)
        {
            draft.observation = None;
        }
        let cancellation = Cancellation::new();
        let job_stamp = stamp.clone();
        let operation_cancel = cancellation.clone();
        self.spawn_job(
            kind,
            stamp,
            cancellation,
            async move { perform(kind, &job_stamp, approved, operation_cancel).await },
            cx,
        );
    }
    fn spawn_job(
        &mut self,
        kind: WorkKind,
        stamp: Stamp,
        cancellation: Cancellation,
        job: impl std::future::Future<Output = Completion> + Send + 'static,
        cx: &mut Context<Self>,
    ) {
        self.notice = None;
        self.active = true;
        self.pending.store(true, Ordering::Release);
        self.cancellation = Some(cancellation.clone());
        let pending = self.pending.clone();
        // The detached weak waiter owns the background Task. Dropping a view must
        // not drop run() while the bridge still owns its global admission slot.
        let work = cx.background_executor().spawn(async move {
            let result = job.await;
            pending.store(false, Ordering::Release);
            result
        });
        cx.spawn(async move |this, cx| {
            let result = work.await;
            let _ = this.update(cx, |view, cx| {
                view.active = false;
                view.cancellation = None;
                if view.stamp().as_ref() == Some(&stamp) && !cancellation.is_cancelled() {
                    view.complete(&stamp, result);
                } else if matches!(kind, WorkKind::Apply | WorkKind::Disconnect) {
                    view.notice = Some(Notice::ReadAgain);
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    fn complete(&mut self, stamp: &Stamp, result: Completion) {
        match result {
            Completion::Refresh { selection, catalog } => {
                let error = selection.as_ref().err().copied();
                if let Ok(data) = selection
                    && let Some(draft) = self.drafts.get_mut(&stamp.key)
                {
                    draft.observation = Some(data.observation);
                }
                if let Some(catalog) = catalog {
                    match catalog {
                        Ok(info) if Some(info.address) == stamp.address => {
                            let mut models: Vec<_> = info
                                .models
                                .into_iter()
                                .map(|m| m.id)
                                .filter(|m| public_model(m))
                                .collect();
                            models.sort();
                            models.dedup();
                            self.catalog = Some(Catalog {
                                address: info.address,
                                models,
                            });
                        }
                        Ok(_) => {
                            self.notice =
                                Some(Notice::Error(BridgeError::new(ErrorKind::InvalidReply)))
                        }
                        Err(e) => self.notice = Some(Notice::Error(e)),
                    }
                }
                if let Some(error) = error {
                    self.notice = Some(Notice::Error(error));
                }
            }
            Completion::Preview {
                mode,
                result,
                catalog,
            } => {
                if let Some(info) = catalog {
                    let mut models: Vec<_> = info
                        .models
                        .into_iter()
                        .map(|m| m.id)
                        .filter(|m| public_model(m))
                        .collect();
                    models.sort();
                    models.dedup();
                    self.catalog = Some(Catalog {
                        address: info.address,
                        models,
                    });
                }
                match result {
                    Ok(data) => {
                        if let Some(draft) = self.drafts.get_mut(&stamp.key) {
                            if let Some(observation) = &mut draft.observation {
                                observation.status = data.observation.status;
                            } else {
                                draft.observation = Some(Observation {
                                    status: data.observation.status,
                                    model: None,
                                    selection_state: SelectionState::Unknown,
                                    public_fields: Vec::new(),
                                });
                            }
                        }
                        if let Some(revision) = data.revision {
                            self.ticket = Some(Ticket {
                                stamp: stamp.clone(),
                                mode,
                                revision,
                                changed: data.changed,
                            });
                        }
                    }
                    Err(e) => {
                        self.catalog = None;
                        if let Some(draft) = self.drafts.get_mut(&stamp.key) {
                            draft.observation = None;
                        }
                        self.notice = Some(Notice::Error(e));
                    }
                }
            }
            Completion::Mutation { result, selection } => {
                if let Some(Ok(data)) = selection
                    && let Some(draft) = self.drafts.get_mut(&stamp.key)
                {
                    draft.observation = Some(data.observation);
                }
                match result {
                    Ok(data) => {
                        self.notice = Some(
                            if let (Some(restored), Some(preserved)) =
                                (data.restored, data.preserved)
                            {
                                Notice::Disconnected(restored, preserved)
                            } else {
                                Notice::Updated
                            },
                        )
                    }
                    Err(e) => {
                        self.notice = Some(if e.mutation_may_have_run {
                            Notice::ReadAgain
                        } else {
                            Notice::Error(e)
                        })
                    }
                }
                if self.observation().is_none() {
                    self.notice = Some(Notice::ReadAgain);
                }
            }
        }
    }
}
impl Drop for AgentConnections {
    fn drop(&mut self) {
        self.cancel();
    }
}
fn plugin_request(
    stamp: &Stamp,
    operation: &str,
    mode: Option<Mode>,
    revision: Option<&str>,
) -> Request {
    let mut arguments = Vec::new();
    if !stamp.config.is_empty() {
        arguments.extend(["--config".into(), stamp.config.clone()]);
    }
    if let Some(mode) = mode {
        if let Some(model) = &stamp.model {
            arguments.extend(["--model".into(), model.clone()]);
        }
        if mode == Mode::Connect
            && let Some(address) = stamp.address
        {
            arguments.extend(["--listen".into(), address.to_string()]);
        }
    }
    if let Some(revision) = revision {
        arguments.extend(["--if-revision".into(), revision.to_owned()]);
    }
    Request::PluginCall {
        owner: stamp.key.owner.clone(),
        operation: operation.into(),
        arguments,
        mutation: matches!(operation, "connect" | "model" | "disconnect"),
    }
}
async fn call(
    stamp: &Stamp,
    operation: &str,
    mode: Option<Mode>,
    revision: Option<&str>,
    cancel: Cancellation,
) -> Result<PluginData, BridgeError> {
    let mutation = matches!(operation, "connect" | "model" | "disconnect");
    let response =
        cli_bridge::run(plugin_request(stamp, operation, mode, revision), cancel).await?;
    let result = match response {
        Reply::PluginResult(value) => decode(&value, &stamp.key.id, operation),
        _ => Err(BridgeError::new(ErrorKind::InvalidReply)),
    };
    result.map_err(|mut error| {
        error.mutation_may_have_run = mutation;
        error
    })
}
async fn inspect_model(stamp: &Stamp, cancel: Cancellation) -> Result<GatewayInfo, BridgeError> {
    let address = stamp
        .address
        .ok_or_else(|| BridgeError::new(ErrorKind::InvalidAddress))?;
    let model = stamp
        .model
        .as_ref()
        .ok_or_else(|| BridgeError::new(ErrorKind::InvalidModel))?;
    match cli_bridge::run(Request::Inspect { address }, cancel).await? {
        Reply::Inspected(info)
            if info.address == address && info.models.iter().any(|m| m.id == *model) =>
        {
            Ok(info)
        }
        Reply::Inspected(_) => Err(BridgeError::new(ErrorKind::ModelUnavailable)),
        _ => Err(BridgeError::new(ErrorKind::InvalidReply)),
    }
}
async fn perform(
    kind: WorkKind,
    stamp: &Stamp,
    approved: Option<(Mode, String)>,
    cancel: Cancellation,
) -> Completion {
    match kind {
        WorkKind::Refresh => {
            let selection = call(stamp, "selection", None, None, cancel.clone()).await;
            let catalog = if !cancel.is_cancelled() {
                if let Some(address) = stamp.address {
                    Some(
                        match cli_bridge::run(Request::Inspect { address }, cancel).await {
                            Ok(Reply::Inspected(info)) => Ok(info),
                            Ok(_) => Err(BridgeError::new(ErrorKind::InvalidReply)),
                            Err(e) => Err(e),
                        },
                    )
                } else {
                    None
                }
            } else {
                None
            };
            Completion::Refresh { selection, catalog }
        }
        WorkKind::Preview(mode) => match inspect_model(stamp, cancel.clone()).await {
            Ok(catalog) => Completion::Preview {
                mode,
                result: call(stamp, mode.preview(), Some(mode), None, cancel).await,
                catalog: Some(catalog),
            },
            Err(error) => Completion::Preview {
                mode,
                result: Err(error),
                catalog: None,
            },
        },
        WorkKind::Apply | WorkKind::Disconnect => {
            let result = match (kind, approved) {
                (WorkKind::Apply, Some((mode, revision))) => {
                    match inspect_model(stamp, cancel.clone()).await {
                        Ok(_) => {
                            call(
                                stamp,
                                mode.apply(),
                                Some(mode),
                                Some(&revision),
                                cancel.clone(),
                            )
                            .await
                        }
                        Err(error) => Err(error),
                    }
                }
                (WorkKind::Disconnect, None) => {
                    call(stamp, "disconnect", None, None, cancel.clone()).await
                }
                _ => Err(BridgeError::new(ErrorKind::InvalidReply)),
            };
            let selection = if !cancel.is_cancelled() {
                Some(call(stamp, "selection", None, None, cancel).await)
            } else {
                None
            };
            Completion::Mutation { result, selection }
        }
    }
}
fn decode(value: &Value, id: &str, operation: &str) -> Result<PluginData, BridgeError> {
    let bad = || BridgeError::new(ErrorKind::InvalidReply);
    if value.get("version").and_then(Value::as_str) != Some("0.2.0")
        || value.get("agent_id").and_then(Value::as_str) != Some(id)
        || value.get("operation").and_then(Value::as_str) != Some(operation)
    {
        return Err(bad());
    }
    let status = match value.get("status").and_then(Value::as_str) {
        Some("unmanaged") => Ownership::Unmanaged,
        Some("connected") => Ownership::Connected,
        Some("drifted") => Ownership::Drifted,
        Some("recovery-required") => Ownership::RecoveryRequired,
        _ => return Err(bad()),
    };
    match operation {
        "preview"
            if status != Ownership::Unmanaged
                || value.get("applied").and_then(Value::as_bool) != Some(false) =>
        {
            return Err(bad());
        }
        "model-preview"
            if status != Ownership::Connected
                || value.get("applied").and_then(Value::as_bool) != Some(false) =>
        {
            return Err(bad());
        }
        "connect" | "model"
            if status != Ownership::Connected
                || value.get("applied").and_then(Value::as_bool) != Some(true) =>
        {
            return Err(bad());
        }
        "disconnect"
            if status != Ownership::Unmanaged
                || value.get("applied").and_then(Value::as_bool) != Some(true) =>
        {
            return Err(bad());
        }
        _ => {}
    }
    let raw_model = value.pointer("/selection/model");
    let (selection_state, model) = match value.pointer("/selection/state").and_then(Value::as_str) {
        Some("qualified") => {
            let model = raw_model
                .and_then(Value::as_str)
                .filter(|m| public_model(m))
                .ok_or_else(bad)?;
            (SelectionState::Qualified, Some(model.to_owned()))
        }
        Some("external") => {
            let model = raw_model
                .and_then(Value::as_str)
                .filter(|m| valid_label(m, 512))
                .ok_or_else(bad)?;
            (SelectionState::External, Some(model.to_owned()))
        }
        Some("unset") => {
            if raw_model.is_some_and(|m| !m.is_null()) {
                return Err(bad());
            }
            (SelectionState::Unset, None)
        }
        None | Some("unknown" | "unreadable" | "not_reported") => (SelectionState::Unknown, None),
        Some(_) => return Err(bad()),
    };
    let mut public_fields = Vec::new();
    if let Some(fields) = value.get("public_fields") {
        let fields = fields
            .as_array()
            .filter(|fields| fields.len() <= 16)
            .ok_or_else(bad)?;
        let mut ids = BTreeSet::new();
        for field in fields {
            let object = field.as_object().ok_or_else(bad)?;
            if object
                .keys()
                .any(|k| !matches!(k.as_str(), "id" | "label" | "value"))
            {
                return Err(bad());
            }
            let id = field
                .get("id")
                .and_then(Value::as_str)
                .filter(|id| valid_label(id, 64) && id.is_ascii())
                .ok_or_else(bad)?;
            if !ids.insert(id) {
                return Err(bad());
            }
            let labels = field
                .get("label")
                .and_then(Value::as_object)
                .ok_or_else(bad)?;
            if labels.len() > 5 {
                return Err(bad());
            }
            let mut safe_labels = BTreeMap::new();
            for (locale, label) in labels {
                if !matches!(locale.as_str(), "en" | "zh-CN" | "zh-TW" | "ja" | "de") {
                    return Err(bad());
                }
                let label = label
                    .as_str()
                    .filter(|label| valid_label(label, 128))
                    .ok_or_else(bad)?;
                safe_labels.insert(locale.clone(), label.to_owned());
            }
            let field_value = match field.get("value") {
                Some(Value::Null) => None,
                Some(Value::String(value))
                    if value.len() <= 512 && !value.chars().any(char::is_control) =>
                {
                    Some(value.clone())
                }
                _ => return Err(bad()),
            };
            public_fields.push(PublicField {
                id: id.to_owned(),
                labels: safe_labels,
                value: field_value,
            });
        }
    }
    let revision = value
        .get("revision")
        .and_then(Value::as_str)
        .filter(|s| valid_label(s, 2048))
        .map(str::to_owned);
    let changed = if matches!(operation, "preview" | "model-preview") {
        let fields = value
            .get("changed_fields")
            .and_then(Value::as_array)
            .ok_or_else(bad)?;
        if fields.len() > 512
            || fields
                .iter()
                .any(|f| !f.as_str().is_some_and(|s| valid_label(s, 2048)))
            || revision.is_none()
        {
            return Err(bad());
        }
        fields.len()
    } else {
        0
    };
    let count = |key| {
        value
            .get(key)
            .and_then(Value::as_u64)
            .and_then(|n| usize::try_from(n).ok())
            .filter(|n| *n <= 512)
    };
    let (restored, preserved) = if operation == "disconnect" {
        let r = count("restored_fields").ok_or_else(bad)?;
        let p = count("preserved_fields").ok_or_else(bad)?;
        if r + p > 512 {
            return Err(bad());
        }
        (Some(r), Some(p))
    } else {
        (None, None)
    };
    Ok(PluginData {
        observation: Observation {
            status,
            model,
            selection_state,
            public_fields,
        },
        revision,
        changed,
        restored,
        preserved,
    })
}
fn filtered_models(models: &[String], query: &str) -> (Vec<String>, usize) {
    let mut visible = Vec::new();
    let mut matches = 0;
    for model in models {
        if model.to_lowercase().contains(query) {
            matches += 1;
            if visible.len() < 80 {
                visible.push(model.clone());
            }
        }
    }
    (visible, matches)
}
fn valid_label(value: &str, max: usize) -> bool {
    !value.is_empty() && value.len() <= max && !value.chars().any(char::is_control)
}
fn valid_address(address: SocketAddr) -> bool {
    address.ip().is_loopback()
        && address.port() != 0
        && !matches!(address,SocketAddr::V6(a) if a.scope_id()!=0 || a.flowinfo()!=0)
}
fn public_model(model: &str) -> bool {
    if model.len() > 512
        || !model.is_ascii()
        || model
            .bytes()
            .any(|b| b.is_ascii_whitespace() || b.is_ascii_control())
    {
        return false;
    }
    let Some((provider, name)) = model.split_once('/') else {
        return false;
    };
    !provider.is_empty()
        && provider.len() <= 128
        && !name.is_empty()
        && provider
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b))
        && !name.bytes().any(|b| b"\\\"'?#".contains(&b))
}

#[derive(Clone, Copy)]
enum Text {
    Title,
    FileState,
    Details,
    HideDetails,
    Hint,
    Path,
    DefaultPath,
    Search,
    Refresh,
    NoAdapters,
    Gateway,
    NoGateway,
    Unseen,
    Unknown,
    Current,
    Selected,
    Missing,
    NoModels,
    NoMatches,
    Limit,
    RetainedEndpoint,
    PreviewConnect,
    PreviewModel,
    Connect,
    Save,
    Disconnect,
    Confirm,
    Keep,
    Cancel,
    Checking,
    Changes,
    Session,
    Updated,
    Restored,
    ReadAgain,
    PreviewReady,
}
fn copy(locale: Locale, key: Text) -> &'static str {
    let strings = match key {
        Text::FileState => [
            "Configuration",
            "配置状态",
            "設定狀態",
            "設定の状態",
            "Konfiguration",
        ],
        Text::Details => [
            "Connection settings",
            "连接设置",
            "連線設定",
            "接続設定",
            "Verbindungseinstellungen",
        ],
        Text::HideDetails => [
            "Hide settings",
            "收起设置",
            "收起設定",
            "設定を閉じる",
            "Einstellungen schließen",
        ],
        Text::Title => [
            "Agent connections",
            "Agent 接入",
            "Agent 接入",
            "Agent 接続",
            "Agent-Verbindungen",
        ],
        Text::Hint => [
            "Configuration adapters. Installation and running sessions are not detected here.",
            "配置入口；这里不推断是否已安装或正在运行。",
            "設定入口；這裡不推斷是否已安裝或正在執行。",
            "設定用アダプターです。インストールや実行中の状態は判定しません。",
            "Konfigurationsadapter. Installation und laufende Sitzungen werden hier nicht erkannt.",
        ],
        Text::Path => [
            "Configuration file (optional)",
            "配置文件（可选）",
            "設定檔（選填）",
            "設定ファイル（任意）",
            "Konfigurationsdatei (optional)",
        ],
        Text::DefaultPath => [
            "Plugin default; specify an absolute path if ambiguous",
            "留空按插件规则选择；有歧义时填写绝对路径",
            "留空依插件規則選擇；有歧義時填寫絕對路徑",
            "空欄はプラグインの既定値。候補が複数なら絶対パスを指定",
            "Plugin-Standard; bei mehreren Dateien absoluten Pfad angeben",
        ],
        Text::Search => [
            "Search gateway models",
            "搜索网关模型",
            "搜尋閘道模型",
            "ゲートウェイのモデルを検索",
            "Gateway-Modelle suchen",
        ],
        Text::Refresh => [
            "Read configuration and models",
            "读取配置与模型",
            "讀取設定與模型",
            "設定とモデルを読み込む",
            "Konfiguration und Modelle lesen",
        ],
        Text::NoAdapters => [
            "No active configuration adapters. Enable an Agent plugin first.",
            "暂无活动配置入口，请先启用 Agent 插件。",
            "沒有啟用的設定入口，請先啟用 Agent 插件。",
            "有効な設定アダプターがありません。Agent プラグインを有効にしてください。",
            "Keine aktiven Konfigurationsadapter. Zuerst ein Agent-Plugin aktivieren.",
        ],
        Text::Gateway => [
            "Local gateway",
            "本机网关",
            "本機閘道",
            "ローカルゲートウェイ",
            "Lokales Gateway",
        ],
        Text::NoGateway => [
            "Start or configure the local gateway to load models.",
            "启动或配置本机网关后读取模型。",
            "啟動或設定本機閘道後讀取模型。",
            "モデルを読み込むにはローカルゲートウェイを設定・起動してください。",
            "Zum Laden der Modelle das lokale Gateway starten oder konfigurieren.",
        ],
        Text::Unseen => [
            "Configuration not checked",
            "尚未读取配置",
            "尚未讀取設定",
            "設定未確認",
            "Konfiguration nicht geprüft",
        ],
        Text::Unknown => ["Unknown", "未知", "未知", "不明", "Unbekannt"],
        Text::Current => [
            "Model in file",
            "文件中的模型",
            "檔案中的模型",
            "ファイル内のモデル",
            "Modell in der Datei",
        ],
        Text::Missing => ["Not set", "未设置", "未設定", "未設定", "Nicht gesetzt"],
        Text::Selected => [
            "Target model",
            "目标模型",
            "目標模型",
            "変更先のモデル",
            "Zielmodell",
        ],
        Text::NoModels => [
            "No models loaded from the gateway. Read again after checking the gateway.",
            "尚未从网关读到模型，请检查网关后重新读取。",
            "尚未從閘道讀到模型，請檢查閘道後重新讀取。",
            "ゲートウェイからモデルを取得できていません。接続を確認して再読込してください。",
            "Noch keine Gateway-Modelle geladen. Gateway prüfen und erneut lesen.",
        ],
        Text::Limit => [
            "Showing the first 80 matches. Search to select other models.",
            "显示前80项，请搜索以选择其它模型。",
            "顯示前80項，請搜尋以選取其他模型。",
            "最初の80件を表示しています。他のモデルは検索して選択してください。",
            "Die ersten 80 Treffer werden angezeigt. Zum Auswählen weiterer Modelle die Suche verwenden.",
        ],
        Text::RetainedEndpoint => [
            "A model change keeps the connection's existing gateway; it does not replace its endpoint.",
            "换模沿用该连接已持有的网关，不会更换接入地址。",
            "換模沿用此連線已持有的閘道，不會更換接入位址。",
            "モデル変更では既存の接続先ゲートウェイを維持します。エンドポイントは変更しません。",
            "Ein Modellwechsel behält das bestehende Gateway der Verbindung bei und ersetzt dessen Adresse nicht.",
        ],
        Text::NoMatches => [
            "No match. Narrow the search; at most 80 rows are shown.",
            "没有匹配项；最多显示 80 行，可继续搜索。",
            "沒有符合項目；最多顯示 80 行，可繼續搜尋。",
            "一致するモデルがありません。最大80件表示です。検索を絞ってください。",
            "Kein Treffer. Suche eingrenzen; höchstens 80 Zeilen werden angezeigt.",
        ],
        Text::PreviewConnect => [
            "Preview connection",
            "预览接入",
            "預覽接入",
            "接続をプレビュー",
            "Verbindung prüfen",
        ],
        Text::PreviewModel => [
            "Preview model change",
            "预览换模",
            "預覽換模",
            "モデル変更をプレビュー",
            "Modellwechsel prüfen",
        ],
        Text::Connect => ["Connect", "接入", "接入", "接続", "Verbinden"],
        Text::Save => [
            "Apply model change",
            "应用换模",
            "套用換模",
            "モデル変更を適用",
            "Modellwechsel anwenden",
        ],
        Text::Disconnect => [
            "Disconnect and restore",
            "断开并恢复",
            "中斷並還原",
            "切断して復元",
            "Trennen und wiederherstellen",
        ],
        Text::Confirm => [
            "Restore fields still owned by this connection? Later external changes are kept.",
            "恢复仍由此次接入持有的字段？之后的外部修改会保留。",
            "還原仍由此次接入持有的欄位？之後的外部修改會保留。",
            "この接続が所有する項目を復元しますか？その後の外部変更は保持します。",
            "Noch verwaltete Felder wiederherstellen? Spätere externe Änderungen bleiben erhalten.",
        ],
        Text::Keep => [
            "Keep connection",
            "保持连接",
            "保留連線",
            "接続を維持",
            "Verbindung behalten",
        ],
        Text::Cancel => [
            "Cancel operation",
            "取消操作",
            "取消操作",
            "操作をキャンセル",
            "Vorgang abbrechen",
        ],
        Text::Checking => [
            "Working…",
            "处理中…",
            "處理中…",
            "処理中…",
            "Wird verarbeitet…",
        ],
        Text::Changes => [
            "Fields to change",
            "将修改的字段数",
            "將修改的欄位數",
            "変更する項目数",
            "Zu ändernde Felder",
        ],
        Text::Session => [
            "File ownership is not runtime routing. Start a new session or restart the Agent, then verify a real request.",
            "文件归属不代表运行中的请求已走网关。请新开会话或重启 Agent，再核对真实请求。",
            "檔案歸屬不代表執行中的請求已經過閘道。請開新工作階段或重啟 Agent，再核對真實請求。",
            "ファイルの所有状態は実行中のルーティングではありません。新規セッションか再起動後に実際の要求を確認してください。",
            "Dateiverwaltung bestätigt keine laufende Weiterleitung. Neue Sitzung starten oder Agent neu starten und eine echte Anfrage prüfen.",
        ],
        Text::Updated => [
            "Configuration updated. Start a new Agent session.",
            "配置已更新，请新开 Agent 会话。",
            "設定已更新，請開啟新的 Agent 工作階段。",
            "設定を更新しました。新しい Agent セッションを開始してください。",
            "Konfiguration aktualisiert. Neue Agent-Sitzung starten.",
        ],
        Text::Restored => [
            "Restored / preserved fields",
            "恢复 / 保留的字段数",
            "還原／保留的欄位數",
            "復元／保持した項目数",
            "Wiederhergestellte / beibehaltene Felder",
        ],
        Text::ReadAgain => [
            "A change may have completed. Read configuration again before another action.",
            "配置可能已经改变，请重新读取后再操作。",
            "設定可能已變更，請重新讀取後再操作。",
            "変更が完了している可能性があります。操作前に設定を再読込してください。",
            "Eine Änderung könnte erfolgt sein. Vor weiteren Aktionen Konfiguration erneut lesen.",
        ],
        Text::PreviewReady => [
            "Preview ready. Apply uses this exact target, model and gateway snapshot.",
            "预览已就绪；应用时校验此目标、模型和网关快照。",
            "預覽已就緒；套用時核對此目標、模型與閘道快照。",
            "プレビュー完了。この対象・モデル・ゲートウェイのスナップショットを適用時に照合します。",
            "Vorschau bereit. Ziel, Modell und Gateway-Snapshot werden beim Anwenden geprüft.",
        ],
    };
    strings[match locale {
        Locale::English => 0,
        Locale::SimplifiedChinese => 1,
        Locale::TraditionalChinese => 2,
        Locale::Japanese => 3,
        Locale::German => 4,
    }]
}
fn ownership_label(locale: Locale, status: Ownership) -> &'static str {
    use crate::actions_text::ActionText;
    locale.action_text(match status {
        Ownership::Unmanaged => ActionText::Unmanaged,
        Ownership::Connected => ActionText::Connected,
        Ownership::Drifted => ActionText::Drifted,
        Ownership::RecoveryRequired => ActionText::RecoveryRequired,
    })
}
impl AgentConnections {
    fn control(
        &mut self,
        id: String,
        label: &str,
        action: Action,
        enabled: bool,
        selected: bool,
        cx: &mut Context<Self>,
    ) -> MaterialButton {
        let focus = self
            .focus
            .entry(id.clone())
            .or_insert_with(|| cx.focus_handle().tab_stop(true))
            .clone()
            .tab_stop(enabled);
        let variant = if matches!(action, Action::Apply) {
            ButtonVariant::Filled
        } else if selected {
            ButtonVariant::Tonal
        } else {
            ButtonVariant::Text
        };
        let scroll = if id.starts_with("agent-model:") {
            self.model_scroll.clone()
        } else {
            self.scroll.clone()
        };
        let anchor = self
            .anchors
            .entry(id.clone())
            .or_insert_with(|| ScrollAnchor::for_handle(scroll))
            .clone();
        material_button(SharedString::from(id), variant)
            .button_size(ButtonSize::Small)
            .rounded(px(8.0))
            .font(theme::ui_font(cx, self.locale))
            .toggle(matches!(
                action,
                Action::Select(_) | Action::Model(_) | Action::Details
            ))
            .selected(selected)
            .disabled(!enabled)
            .track_focus(&focus)
            .anchor_scroll(Some(anchor))
            .accessibility_label(label.to_owned())
            .on_click(cx.listener(move |view, _, _, cx| view.act(action.clone(), cx)))
    }
    fn button(
        &mut self,
        id: String,
        label: &str,
        action: Action,
        enabled: bool,
        selected: bool,
        cx: &mut Context<Self>,
    ) -> MaterialButton {
        self.control(id, label, action, enabled, selected, cx)
            .child(
                div()
                    .min_w_0()
                    .whitespace_normal()
                    .child(text!(label.to_owned())),
            )
    }
    fn notice_text(&self) -> Option<(bool, String)> {
        self.notice.as_ref().map(|notice| match notice {
            Notice::Error(error) => (
                true,
                self.locale
                    .action_text(crate::actions_text::error_text(error.kind))
                    .to_owned(),
            ),
            Notice::Updated => (false, copy(self.locale, Text::Updated).to_owned()),
            Notice::Disconnected(restored, preserved) => (
                false,
                format!(
                    "{}: {restored} / {preserved}",
                    copy(self.locale, Text::Restored)
                ),
            ),
            Notice::ReadAgain => (true, copy(self.locale, Text::ReadAgain).to_owned()),
            Notice::NoAdapters => (true, copy(self.locale, Text::NoAdapters).to_owned()),
        })
    }
}
impl Render for AgentConnections {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let palette = Palette::for_window(window, self.appearance);
        let busy = self.busy();
        let locale = self.locale;
        self.config.update(cx, |input, cx| {
            input.set_presentation(
                copy(locale, Text::DefaultPath),
                copy(locale, Text::Path),
                palette,
                busy,
                cx,
            )
        });
        self.search.update(cx, |input, cx| {
            input.set_presentation(
                copy(locale, Text::Search),
                copy(locale, Text::Search),
                palette,
                busy,
                cx,
            )
        });
        let narrow = window.viewport_size().width < px(600.0);
        let mut ids = BTreeSet::new();
        let mut body = div()
            .flex()
            .flex_col()
            .gap(px(12.0))
            .min_w_0()
            .pl(px(if narrow { 12.0 } else { 60.0 }))
            .pr(px(12.0))
            .pt(px(8.0))
            .pb(px(24.0))
            .bg(rgb(palette.surface));
        if self.adapters.is_empty() {
            return div()
                .id("agent-connections")
                .size_full()
                .min_h_0()
                .text_color(rgb(palette.text))
                .child(
                    div()
                        .py(px(40.0))
                        .flex()
                        .flex_col()
                        .gap(px(12.0))
                        .child(
                            div()
                                .text_size(px(16.0))
                                .font_weight(FontWeight::SEMIBOLD)
                                .child(text!(copy(locale, Text::Title))),
                        )
                        .child(
                            div()
                                .text_color(rgb(palette.muted))
                                .whitespace_normal()
                                .child(text!(copy(locale, Text::NoAdapters))),
                        ),
                );
        }
        let rows: Vec<_> = self
            .adapters
            .iter()
            .map(|adapter| {
                let key = Key::of(adapter);
                let observed = self.drafts.get(&key).and_then(|d| d.observation.as_ref());
                let status = observed
                    .map(|o| ownership_label(locale, o.status))
                    .unwrap_or(copy(locale, Text::Unseen));
                let model = observed
                    .map(|o| o.model_label(locale))
                    .unwrap_or(copy(locale, Text::Unseen));
                (
                    key,
                    adapter.name.clone(),
                    status.to_owned(),
                    model.to_owned(),
                )
            })
            .collect();
        let mut leading_rows = div().flex().flex_col();
        let mut trailing_rows = div().flex().flex_col();
        let mut after_selected = false;
        for (key, name, status, model) in rows {
            let id = format!("agent-adapter:{}:{}", key.owner, key.id);
            ids.insert(id.clone());
            let selected = self.selected.as_ref() == Some(&key);
            let label = format!(
                "{name} · {}: {status} · {}: {model}",
                copy(locale, Text::FileState),
                copy(locale, Text::Current)
            );
            let initial = name
                .chars()
                .next()
                .unwrap_or('A')
                .to_uppercase()
                .to_string();
            let identity = div()
                .flex()
                .items_center()
                .gap(px(12.0))
                .min_w_0()
                .flex_1()
                .child(
                    div()
                        .size(px(36.0))
                        .flex_none()
                        .rounded(px(9.0))
                        .bg(rgb(palette.inset))
                        .flex()
                        .items_center()
                        .justify_center()
                        .text_size(px(15.0))
                        .font_weight(FontWeight::SEMIBOLD)
                        .text_color(rgb(palette.text))
                        .child(text!(initial)),
                )
                .child(
                    div()
                        .min_w_0()
                        .flex_1()
                        .flex()
                        .flex_col()
                        .gap(px(5.0))
                        .child(
                            div()
                                .text_size(px(14.0))
                                .font_weight(FontWeight::SEMIBOLD)
                                .whitespace_normal()
                                .child(text!(name)),
                        )
                        .child(
                            div()
                                .text_size(px(12.0))
                                .text_color(rgb(palette.muted))
                                .whitespace_normal()
                                .child(text!(status)),
                        ),
                );
            let content = div()
                .w_full()
                .min_w_0()
                .flex()
                .items_center()
                .gap(px(16.0))
                .when(narrow, |row| row.flex_col().items_start().gap(px(8.0)))
                .child(identity)
                .child(
                    div()
                        .min_w_0()
                        .when(!narrow, |item| item.w(px(240.0)))
                        .when(narrow, |item| item.pl(px(48.0)))
                        .flex()
                        .flex_col()
                        .gap(px(4.0))
                        .child(
                            div()
                                .text_size(px(11.0))
                                .text_color(rgb(palette.muted))
                                .child(text!(copy(locale, Text::Current))),
                        )
                        .child(
                            div()
                                .text_size(px(13.0))
                                .whitespace_normal()
                                .child(text!(model)),
                        ),
                )
                .child(
                    div()
                        .flex_none()
                        .text_color(rgb(if selected {
                            palette.accent
                        } else {
                            palette.muted
                        }))
                        .child(text!(if selected && self.editor_open {
                            "−"
                        } else {
                            "+"
                        })),
                );
            let row = self
                .control(id, &label, Action::Select(key), !busy, selected, cx)
                .button_size(ButtonSize::Medium)
                .w_full()
                .h_auto()
                .min_h(px(76.0))
                .px(px(12.0))
                .py(px(14.0))
                .rounded(px(8.0))
                .justify_start()
                .text_color(rgb(palette.text))
                .when(selected, |row| row.bg(rgb(palette.surface)))
                .child(content);
            if after_selected {
                trailing_rows = trailing_rows.child(row);
            } else {
                leading_rows = leading_rows.child(row);
            }
            if selected {
                after_selected = true;
            }
        }
        let gateway = self
            .address
            .map(|a| format!("{}: http://{a}", copy(locale, Text::Gateway)))
            .unwrap_or_else(|| copy(locale, Text::NoGateway).to_owned());
        let details_id = "agent-details".to_owned();
        ids.insert(details_id.clone());
        let mut tools = div()
            .flex()
            .flex_wrap()
            .items_center()
            .gap(px(8.0))
            .child(self.button(
                details_id,
                copy(
                    locale,
                    if self.details_open {
                        Text::HideDetails
                    } else {
                        Text::Details
                    },
                ),
                Action::Details,
                !busy,
                self.details_open,
                cx,
            ));
        ids.insert("agent-read".into());
        tools = tools.child(self.button(
            "agent-read".into(),
            copy(locale, Text::Refresh),
            Action::Refresh,
            !busy,
            false,
            cx,
        ));
        body = body.child(tools);
        if self.details_open {
            let config_anchor = self
                .anchors
                .entry("agent-config-anchor".into())
                .or_insert_with(|| ScrollAnchor::for_handle(self.scroll.clone()))
                .clone();
            body = body.child(
                div()
                    .flex()
                    .flex_col()
                    .gap(px(10.0))
                    .child(
                        div()
                            .text_size(px(12.0))
                            .text_color(rgb(palette.muted))
                            .whitespace_normal()
                            .child(text!(gateway)),
                    )
                    .child(
                        div()
                            .text_size(px(12.0))
                            .child(text!(copy(locale, Text::Path))),
                    )
                    .child(
                        div()
                            .id("agent-config-container")
                            .anchor_scroll(Some(config_anchor))
                            .child(self.config.clone()),
                    ),
            );
            if let Some(observation) = self.observation() {
                for field in &observation.public_fields {
                    body = body.child(div().text_size(px(12.0)).whitespace_normal().child(text!(
                        format!(
                                "{}: {}",
                                field.label(locale),
                                field
                                    .value
                                    .as_deref()
                                    .unwrap_or(copy(locale, Text::Missing))
                            )
                    )));
                }
            }
            body = body.child(
                div()
                    .text_size(px(12.0))
                    .text_color(rgb(palette.muted))
                    .whitespace_normal()
                    .child(text!(copy(locale, Text::Session))),
            );
        }
        let selected = self
            .selected
            .as_ref()
            .and_then(|k| self.drafts.get(k))
            .and_then(|d| d.model.clone());
        if busy {
            let id = "agent-cancel".to_owned();
            ids.insert(id.clone());
            body = body.child(
                div()
                    .flex()
                    .items_center()
                    .flex_wrap()
                    .gap(px(8.0))
                    .child(text!(copy(locale, Text::Checking)))
                    .child(self.button(
                        id,
                        copy(locale, Text::Cancel),
                        Action::Cancel,
                        true,
                        false,
                        cx,
                    )),
            );
        }
        if let Some((error, message)) = self.notice_text() {
            body = body.child(
                div()
                    .text_size(px(13.0))
                    .text_color(rgb(if error {
                        palette.danger
                    } else {
                        palette.accent
                    }))
                    .whitespace_normal()
                    .child(text!(message)),
            );
        }
        body = body.child(self.search.clone());
        if let Some(model) = selected.as_ref() {
            body = body.child(
                div()
                    .flex()
                    .flex_wrap()
                    .items_baseline()
                    .gap(px(8.0))
                    .child(
                        div()
                            .text_size(px(12.0))
                            .text_color(rgb(palette.muted))
                            .child(text!(copy(locale, Text::Selected))),
                    )
                    .child(
                        div()
                            .text_size(px(13.0))
                            .font_weight(FontWeight::MEDIUM)
                            .text_color(rgb(palette.accent))
                            .whitespace_normal()
                            .child(text!(model.clone())),
                    ),
            );
        }

        let (models, matched) = self
            .catalog
            .as_ref()
            .filter(|c| Some(c.address) == self.address)
            .map(|c| filtered_models(&c.models, &self.query))
            .unwrap_or_default();
        if matched > 80 {
            body = body.child(
                div()
                    .text_size(px(12.0))
                    .text_color(rgb(palette.muted))
                    .whitespace_normal()
                    .child(text!(copy(locale, Text::Limit))),
            );
        }
        if self.catalog.as_ref().is_none_or(|c| c.models.is_empty()) {
            body = body.child(
                div()
                    .text_size(px(12.0))
                    .text_color(rgb(palette.muted))
                    .whitespace_normal()
                    .child(text!(copy(locale, Text::NoModels))),
            );
        } else if models.is_empty() {
            body = body.child(
                div()
                    .text_size(px(12.0))
                    .text_color(rgb(palette.muted))
                    .whitespace_normal()
                    .child(text!(copy(locale, Text::NoMatches))),
            );
        }
        let model_anchor = self
            .anchors
            .entry("model-container".into())
            .or_insert_with(|| ScrollAnchor::for_handle(self.scroll.clone()))
            .clone();
        let mut model_rows = div()
            .id("agent-model-list")
            .max_h(px(240.0))
            .overflow_y_scroll()
            .track_scroll(&self.model_scroll)
            .anchor_scroll(Some(model_anchor))
            .flex()
            .flex_col()
            .gap(px(1.0));
        for model in models {
            let id = format!("agent-model:{model}");
            ids.insert(id.clone());
            let is_selected = selected.as_deref() == Some(model.as_str());
            model_rows = model_rows.child(
                self.button(
                    id,
                    &model,
                    Action::Model(model.clone()),
                    !busy,
                    is_selected,
                    cx,
                )
                .button_size(ButtonSize::Medium)
                .w_full()
                .h_auto()
                .min_h(px(40.0))
                .px(px(12.0))
                .py(px(8.0))
                .justify_start(),
            );
        }
        body = body.child(model_rows);
        let available = !busy && self.model_available();
        let model_workflow = self
            .observation()
            .is_some_and(|o| o.status == Ownership::Connected);
        let can_model = available && model_workflow;
        let can_connect = available
            && self
                .observation()
                .is_none_or(|o| o.status == Ownership::Unmanaged);
        let mut actions = div().flex().flex_wrap().items_center().gap(px(6.0));
        for (id, label, action, enabled) in [
            (
                "agent-preview-connect",
                Text::PreviewConnect,
                Action::Preview(Mode::Connect),
                can_connect,
            ),
            (
                "agent-preview-model",
                Text::PreviewModel,
                Action::Preview(Mode::Model),
                can_model,
            ),
        ] {
            if (matches!(action, Action::Preview(Mode::Model)) && !model_workflow)
                || (matches!(action, Action::Preview(Mode::Connect)) && model_workflow)
            {
                continue;
            }
            ids.insert(id.to_owned());
            actions = actions.child(self.button(
                id.to_owned(),
                copy(locale, label),
                action,
                enabled,
                false,
                cx,
            ));
        }
        if let Some(ticket) = &self.ticket {
            body = body.child(
                div()
                    .text_size(px(12.0))
                    .text_color(rgb(palette.muted))
                    .whitespace_normal()
                    .child(text!(format!(
                        "{} · {}: {}",
                        copy(locale, Text::PreviewReady),
                        copy(locale, Text::Changes),
                        ticket.changed
                    ))),
            );
        }
        if can_model {
            body = body.child(
                div()
                    .text_size(px(12.0))
                    .text_color(rgb(palette.muted))
                    .whitespace_normal()
                    .child(text!(copy(locale, Text::RetainedEndpoint))),
            );
        }
        let label = if self.ticket.as_ref().is_some_and(|t| t.mode == Mode::Model) {
            Text::Save
        } else {
            Text::Connect
        };
        let enabled = self.can_apply();
        ids.insert("agent-apply".into());
        actions = actions.child(self.button(
            "agent-apply".into(),
            copy(locale, label),
            Action::Apply,
            enabled,
            false,
            cx,
        ));
        body = body.child(actions);
        if self.confirm.is_some() {
            let mut confirmation = div().flex().flex_col().gap(px(6.0)).child(
                div()
                    .text_size(px(13.0))
                    .whitespace_normal()
                    .child(text!(copy(locale, Text::Confirm))),
            );
            let mut buttons = div().flex().flex_wrap().gap(px(6.0));
            for (id, label, action) in [
                (
                    "agent-confirm-disconnect",
                    Text::Disconnect,
                    Action::Disconnect,
                ),
                ("agent-keep", Text::Keep, Action::Keep),
            ] {
                ids.insert(id.into());
                buttons = buttons.child(self.button(
                    id.into(),
                    copy(locale, label),
                    action,
                    !busy,
                    false,
                    cx,
                ));
            }
            confirmation = confirmation.child(buttons);
            body = body.child(confirmation);
        } else {
            let enabled = !busy
                && self
                    .observation()
                    .is_none_or(|o| o.status != Ownership::Unmanaged);
            ids.insert("agent-disconnect".into());
            body = body.child(div().flex().child(self.button(
                "agent-disconnect".into(),
                copy(locale, Text::Disconnect),
                Action::AskDisconnect,
                enabled,
                false,
                cx,
            )));
        }
        let body = div()
            .flex()
            .flex_col()
            .min_w_0()
            .pb(px(28.0))
            .child(leading_rows)
            .when(self.editor_open, |container| container.child(body))
            .child(trailing_rows)
            .child(
                div()
                    .pt(px(20.0))
                    .text_size(px(12.0))
                    .text_color(rgb(palette.muted))
                    .whitespace_normal()
                    .child(text!(copy(locale, Text::Hint))),
            );
        self.focus.retain(|id, _| ids.contains(id));
        self.anchors.retain(|id, _| {
            ids.contains(id) || id == "model-container" || id == "agent-config-anchor"
        });
        div()
            .id("agent-connections")
            .size_full()
            .min_h_0()
            .min_w_0()
            .text_color(rgb(palette.text))
            .overflow_y_scroll()
            .track_scroll(&self.scroll)
            .on_key_down(cx.listener(|view, event: &gpui::KeyDownEvent, window, cx| {
                if event.keystroke.key == "tab" {
                    if event.keystroke.modifiers.shift {
                        window.focus_prev(cx);
                    } else {
                        window.focus_next(cx);
                    }
                    if view.details_open
                        && view.config.read(cx).focus_handle().is_focused(window)
                        && let Some(anchor) = view.anchors.get("agent-config-anchor")
                    {
                        anchor.scroll_to(window, cx);
                    }
                    for (id, focus) in &view.focus {
                        if focus.is_focused(window) {
                            if id.starts_with("agent-model:")
                                && let Some(a) = view.anchors.get("model-container")
                            {
                                a.scroll_to(window, cx);
                            }
                            if let Some(anchor) = view.anchors.get(id) {
                                anchor.scroll_to(window, cx);
                            }
                            break;
                        }
                    }
                    cx.stop_propagation();
                } else if event.keystroke.key == "escape" {
                    if view.busy() {
                        view.act(Action::Cancel, cx);
                    } else {
                        view.act(Action::Keep, cx);
                    }
                    cx.stop_propagation();
                }
            }))
            .child(body)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    fn stamp() -> Stamp {
        Stamp {
            generation: 7,
            key: Key {
                owner: "agent.synthetic".into(),
                id: "synthetic".into(),
            },
            config: String::new(),
            model: Some("fixture/model-a".into()),
            address: Some("127.0.0.1:43197".parse().expect("synthetic loopback")),
        }
    }
    #[test]
    fn common_protocol_preserves_endpoint_for_model_operations_and_uses_explicit_revision() {
        let mut state = stamp();
        state.config = "/synthetic/config.json".into();
        let Request::PluginCall {
            owner,
            operation,
            arguments,
            mutation,
        } = plugin_request(&state, "connect", Some(Mode::Connect), Some("opaque-token"))
        else {
            panic!("plugin call")
        };
        assert_eq!(owner, "agent.synthetic");
        assert_eq!(operation, "connect");
        assert!(mutation);
        assert_eq!(
            arguments,
            vec![
                "--config",
                "/synthetic/config.json",
                "--model",
                "fixture/model-a",
                "--listen",
                "127.0.0.1:43197",
                "--if-revision",
                "opaque-token"
            ]
        );
        let Request::PluginCall {
            arguments,
            mutation,
            ..
        } = plugin_request(&state, "model", Some(Mode::Model), Some("opaque-token"))
        else {
            panic!("plugin call")
        };
        assert!(!arguments.iter().any(|s| s == "--listen"));
        assert!(mutation);
        let Request::PluginCall {
            arguments,
            mutation,
            ..
        } = plugin_request(&stamp(), "selection", None, None)
        else {
            panic!("plugin call")
        };
        assert!(arguments.is_empty());
        assert!(!mutation);
        let Request::PluginCall { arguments, .. } =
            plugin_request(&stamp(), "disconnect", None, None)
        else {
            panic!("plugin call")
        };
        assert!(arguments.is_empty());
    }
    #[test]
    fn replies_are_bounded_bound_to_the_expected_adapter_and_never_keep_unknown_selections() {
        let mut value = json!({"version":"0.2.0","agent_id":"synthetic","operation":"selection","status":"connected","selection":{"model":"fixture-secret-private-value","state":"unknown"},"raw_settings":"never display this"});
        let decoded = decode(&value, "synthetic", "selection").expect("bounded public projection");
        assert!(decoded.observation.model.is_none());
        value["selection"]["model"] = json!("fixture/model-a");
        assert!(
            decode(&value, "synthetic", "selection")
                .expect("unknown is private even when string looks qualified")
                .observation
                .model
                .is_none()
        );
        value["selection"]["state"] = json!("qualified");
        assert_eq!(
            decode(&value, "synthetic", "selection")
                .expect("qualified model")
                .observation
                .model
                .as_deref(),
            Some("fixture/model-a")
        );
        assert!(decode(&value, "different", "selection").is_err());
        assert!(decode(&value, "synthetic", "preview").is_err());
        value["version"] = json!("new-version");
        assert!(decode(&value, "synthetic", "selection").is_err());
        for raw in [
            "Bearer private-token",
            "vendor/key?private=fixture",
            "unqualified",
            "https://user:password@invalid",
        ] {
            assert!(!public_model(raw));
        }
    }
    #[test]
    fn public_selection_distinguishes_external_unset_and_unreadable() {
        let mut wire = json!({"version":"0.2.0","agent_id":"synthetic","operation":"selection","status":"unmanaged","selection":{"state":"external","model":"public-model-5"}});
        let external = decode(&wire, "synthetic", "selection").expect("external label");
        assert_eq!(
            external.observation.model_label(Locale::English),
            "public-model-5"
        );
        assert!(external.observation.selection_state == SelectionState::External);
        wire["selection"] = json!({"state":"unset","model":null});
        assert_eq!(
            decode(&wire, "synthetic", "selection")
                .unwrap()
                .observation
                .model_label(Locale::German),
            copy(Locale::German, Text::Missing)
        );
        wire["selection"] = json!({"state":"unreadable","model":null});
        assert_eq!(
            decode(&wire, "synthetic", "selection")
                .unwrap()
                .observation
                .model_label(Locale::German),
            copy(Locale::German, Text::Unknown)
        );
        wire["selection"] = json!({"state":"qualified","model":"public-model-5"});
        assert!(decode(&wire, "synthetic", "selection").is_err());
        wire["selection"] = json!({"state":"external","model":"unsafe\nlabel"});
        assert!(decode(&wire, "synthetic", "selection").is_err());
    }

    #[test]
    fn public_fields_are_bounded_localized_and_never_accept_malformed_metadata() {
        let wire = json!({"version":"0.2.0","agent_id":"synthetic","operation":"selection","status":"connected","selection":{"state":"unset","model":null},"public_fields":[{"id":"setting-one","label":{"en":"Setting","ja":"設定"},"value":"public-value"},{"id":"setting-two","label":{},"value":null}]});
        let decoded = decode(&wire, "synthetic", "selection").expect("public fields");
        assert_eq!(
            decoded.observation.public_fields[0].label(Locale::Japanese),
            "設定"
        );
        assert_eq!(
            decoded.observation.public_fields[0].label(Locale::German),
            "Setting"
        );
        assert_eq!(
            decoded.observation.public_fields[1].label(Locale::English),
            "setting-two"
        );
        assert!(decoded.observation.public_fields[1].value.is_none());
        for invalid in [
            json!([{"id":"duplicate","label":{},"value":null},{"id":"duplicate","label":{},"value":null}]),
            json!([{"id":"x","label":{"unsupported":"Label"},"value":null}]),
            json!([{"id":"x","label":{},"value":"unsafe\rvalue"}]),
            json!([{"id":"x","label":{},"value":"x".repeat(513)}]),
            json!([{"id":"x","label":{"en":"x".repeat(129)},"value":null}]),
            json!([{"id":"x".repeat(65),"label":{},"value":null}]),
            json!([{"id":"非ascii","label":{},"value":null}]),
            json!([{"id":"x","label":{},"value":{}}]),
            json!([{"id":"x","label":{},"value":null,"headers":{}}]),
            json!(vec![json!({"id":"x","label":{},"value":null}); 17]),
        ] {
            let mut malformed = wire.clone();
            malformed["public_fields"] = invalid;
            assert!(decode(&malformed, "synthetic", "selection").is_err());
        }
    }

    #[test]
    fn opaque_native_and_declarative_revisions_have_the_same_preview_contract() {
        for revision in ["a".repeat(64), "v1.opaque.public-parameters".into()] {
            let value = json!({"version":"0.2.0","agent_id":"synthetic","operation":"preview","status":"unmanaged","selection":{"model":"fixture/model-a"},"applied":false,"revision":revision,"changed_fields":["/model","/endpoint"]});
            let result = decode(&value, "synthetic", "preview").expect("opaque token");
            assert_eq!(result.changed, 2);
            assert_eq!(result.revision.as_deref(), value["revision"].as_str());
            let mut wrong = value;
            wrong["applied"] = json!(true);
            assert!(decode(&wrong, "synthetic", "preview").is_err());
        }
        let value = json!({"version":"0.2.0","agent_id":"synthetic","operation":"disconnect","status":"unmanaged","applied":true,"restored_fields":4,"preserved_fields":1,"selection":{"model":null}});
        let result = decode(&value, "synthetic", "disconnect").expect("counts only");
        assert_eq!((result.restored, result.preserved), (Some(4), Some(1)));
        let mut wrong = value;
        wrong["restored_fields"] = json!(9000);
        assert!(decode(&wrong, "synthetic", "disconnect").is_err());
    }
}

#[cfg(all(test, feature = "visual-test"))]
mod visual_tests {
    use super::*;
    use gpui::TestAppContext;
    use serde_json::json;
    use std::{
        sync::Mutex,
        task::{Poll, Waker},
    };
    fn adapter() -> OwnedAgent {
        OwnedAgent {
            owner: "agent.synthetic".into(),
            id: "synthetic".into(),
            name: "Fixture adapter".into(),
            engine: Some("native-fixture/1".into()),
            definition: None,
        }
    }
    fn init(cx: &mut TestAppContext) {
        cx.update(|cx| {
            gpui::init(cx);
            gpui_kit_material::init(cx);
        });
    }
    fn fixture(view: &mut AgentConnections, cx: &mut Context<AgentConnections>) {
        view.set_adapters(vec![adapter()], cx);
        view.editor_open = true;
        let address = "127.0.0.1:43197".parse().expect("synthetic loopback");
        view.configure(address, cx);
        view.catalog = Some(Catalog {
            address,
            models: vec!["fixture/model-a".into(), "fixture/model-b".into()],
        });
        let key = view.selected.clone().expect("synthetic adapter");
        let draft = view.drafts.get_mut(&key).expect("draft");
        draft.model = Some("fixture/model-a".into());
        draft.observation = Some(Observation {
            selection_state: SelectionState::Qualified,
            public_fields: Vec::new(),
            status: Ownership::Connected,
            model: Some("fixture/model-a".into()),
        });
        let stamp = view.stamp().expect("stamp");
        view.ticket = Some(Ticket {
            stamp,
            mode: Mode::Model,
            revision: "opaque".into(),
            changed: 1,
        });
    }
    #[gpui::test]
    fn target_model_address_refresh_and_adapter_changes_revoke_apply(cx: &mut TestAppContext) {
        init(cx);
        let page = cx.new(AgentConnections::new);
        page.update(cx, |view, cx| {
            fixture(view, cx);
            assert!(view.can_apply());
            view.act(Action::Model("fixture/model-b".into()), cx);
            assert!(!view.can_apply());
            assert!(view.ticket.is_none());
            fixture(view, cx);
            view.config.update(cx, |input, cx| {
                input.set_text("/synthetic/other.json", cx);
            });
        });
        cx.run_until_parked();
        page.update(cx, |view, cx| {
            assert!(!view.can_apply());
            fixture(view, cx);
            view.configure("127.0.0.1:43198".parse().expect("other loopback"), cx);
            assert!(view.ticket.is_none());
            assert!(view.catalog.is_none());
            fixture(view, cx);
            let mut changed = adapter();
            changed.name = "Renamed fixture adapter".into();
            view.set_adapters(vec![changed], cx);
            assert!(!view.can_apply());
            assert!(view.observation().is_none());
            fixture(view, cx);
            view.invalidate();
            assert!(!view.can_apply());
        });
    }
    #[gpui::test]
    fn external_file_model_is_display_only_and_does_not_replace_gateway_choice(
        cx: &mut TestAppContext,
    ) {
        init(cx);
        let page = cx.new(AgentConnections::new);
        page.update(cx,|view,cx| {
            fixture(view,cx);
            let stamp=view.stamp().expect("target");
            let wire=json!({"version":"0.2.0","agent_id":"synthetic","operation":"selection","status":"connected","selection":{"state":"external","model":"public-model-5"}});
            let data=decode(&wire,"synthetic","selection").expect("projection");
            view.complete(&stamp,Completion::Refresh{selection:Ok(data),catalog:None});
            assert_eq!(view.observation().unwrap().model_label(Locale::English),"public-model-5");
            assert_eq!(view.stamp().unwrap().model.as_deref(),Some("fixture/model-a"));
            assert!(view.model_available());
            let mut wire=wire;
            wire["selection"]["model"]=json!("fixture/external-looking-qualified");
            let data=decode(&wire,"synthetic","selection").expect("still external");
            view.complete(&stamp,Completion::Refresh{selection:Ok(data),catalog:None});
            assert_eq!(view.stamp().unwrap().model.as_deref(),Some("fixture/model-a"));
        });
    }

    #[gpui::test]
    fn identical_contributions_preserve_preview_and_do_not_cancel_pending_work(
        cx: &mut TestAppContext,
    ) {
        init(cx);
        let page = cx.new(AgentConnections::new);
        page.update(cx, |view, cx| {
            fixture(view, cx);
            let selected = view.selected.clone();
            let generation = view.generation;
            let revision = view.ticket.as_ref().expect("preview").revision.clone();
            view.set_adapters(view.adapters.clone(), cx);
            assert!(view.can_apply());
            assert!(view.selected == selected);
            assert_eq!(view.generation, generation);
            assert_eq!(
                view.ticket.as_ref().expect("retained preview").revision,
                revision
            );
            assert!(view.observation().is_some());

            // Model an admitted operation independently of preview availability.
            let cancellation = Cancellation::new();
            view.ticket = None;
            view.cancellation = Some(cancellation.clone());
            view.active = true;
            view.pending.store(true, Ordering::Release);
            view.set_adapters(view.adapters.clone(), cx);
            assert!(!cancellation.is_cancelled());
            assert!(view.pending.load(Ordering::Acquire));
            assert!(view.active);
            assert!(view.selected == selected);
            assert_eq!(view.generation, generation);
            assert!(view.observation().is_some());
            view.active = false;
            view.pending.store(false, Ordering::Release);
            view.cancellation = None;
        });
    }

    #[gpui::test]
    fn withdrawn_contributions_keep_bounded_drafts_but_never_restore_old_preview(
        cx: &mut TestAppContext,
    ) {
        init(cx);
        let page = cx.new(AgentConnections::new);
        page.update(cx, |view, cx| {
            fixture(view, cx);
            let original = adapter();
            let key = Key::of(&original);
            view.drafts.get_mut(&key).expect("draft").config = "/synthetic/explicit.json".into();
            view.set_adapters(Vec::new(), cx);
            assert!(view.selected.is_none());
            assert!(view.ticket.is_none());
            assert!(!view.can_apply());
            assert_eq!(view.drafts[&key].config, "/synthetic/explicit.json");
            assert_eq!(view.drafts[&key].model.as_deref(), Some("fixture/model-a"));
            view.set_adapters(vec![original.clone()], cx);
            assert!(view.selected.as_ref() == Some(&key));
            assert_eq!(view.config.read(cx).text(), "/synthetic/explicit.json");
            assert_eq!(
                view.stamp().expect("restored target").model.as_deref(),
                Some("fixture/model-a")
            );
            assert!(view.observation().is_none());
            assert!(view.ticket.is_none());
            assert!(!view.can_apply());

            // Repeated withdrawal with new owners cannot grow the private cache.
            for index in 0..1100 {
                let mut other = adapter();
                other.owner = format!("agent.fixture-{index}");
                other.id = format!("fixture-{index}");
                view.set_adapters(vec![original.clone(), other], cx);
                assert!(view.drafts.len() <= 1024);
                assert_eq!(view.drafts[&key].config, "/synthetic/explicit.json");
                assert_eq!(view.drafts[&key].model.as_deref(), Some("fixture/model-a"));
            }
        });
    }

    #[gpui::test]
    fn adapter_switches_keep_their_own_explicit_paths_and_never_inherit_other_targets(
        cx: &mut TestAppContext,
    ) {
        init(cx);
        let page = cx.new(AgentConnections::new);
        page.update(cx, |view, cx| {
            let first = adapter();
            let mut second = adapter();
            second.owner = "agent.other".into();
            second.id = "other".into();
            view.set_adapters(vec![first.clone(), second.clone()], cx);
            let first_key = Key::of(&first);
            let second_key = Key::of(&second);
            view.drafts.get_mut(&first_key).expect("first").config = "/synthetic/first.json".into();
            view.selected = Some(second_key.clone());
            view.load_config(cx);
            assert_eq!(view.config.read(cx).text(), "");
            assert_eq!(view.drafts[&first_key].config, "/synthetic/first.json");
            view.drafts.get_mut(&second_key).expect("second").config =
                "/synthetic/second.json".into();
            view.selected = Some(first_key);
            view.load_config(cx);
            assert_eq!(view.config.read(cx).text(), "/synthetic/first.json");
            assert_eq!(view.drafts[&second_key].config, "/synthetic/second.json");
        });
    }
    #[gpui::test]
    fn dropping_a_view_cancels_first_but_retains_the_job_until_background_cleanup(
        cx: &mut TestAppContext,
    ) {
        init(cx);
        let page = cx.new(AgentConnections::new);
        let release = Arc::new(AtomicBool::new(false));
        let waker = Arc::new(Mutex::new(None::<Waker>));
        let cancelled = Cancellation::new();
        let pending = page.update(cx, |view, _| view.shutdown_pending());
        page.update(cx, |view, cx| {
            fixture(view, cx);
            let stamp = view.stamp().expect("stamp");
            let release = release.clone();
            let waker = waker.clone();
            let job = std::future::poll_fn(move |context| {
                if release.load(Ordering::Acquire) {
                    Poll::Ready(Completion::Refresh {
                        selection: Err(BridgeError::new(ErrorKind::Cancelled)),
                        catalog: None,
                    })
                } else {
                    *waker.lock().expect("gate") = Some(context.waker().clone());
                    Poll::Pending
                }
            });
            view.spawn_job(WorkKind::Refresh, stamp, cancelled.clone(), job, cx);
        });
        cx.run_until_parked();
        assert!(pending.load(Ordering::Acquire));
        let weak = page.downgrade();
        drop(page);
        // GPUI defers entity destruction until App::finish_update flushes
        // dropped entities; draining executors alone does not run that phase.
        cx.update(|_| {});
        cx.run_until_parked();
        assert!(
            weak.upgrade().is_none(),
            "the detached waiter holds no strong view reference"
        );
        assert!(cancelled.is_cancelled());
        assert!(
            pending.load(Ordering::Acquire),
            "drop cannot claim bridge cleanup has finished"
        );
        release.store(true, Ordering::Release);
        if let Some(waker) = waker.lock().expect("gate").take() {
            waker.wake();
        }
        cx.run_until_parked();
        assert!(!pending.load(Ordering::Acquire));
    }
    #[gpui::test]
    fn search_reaches_the_entire_catalog_without_losing_selection_beyond_visible_rows(
        cx: &mut TestAppContext,
    ) {
        init(cx);
        let page = cx.new(AgentConnections::new);
        page.update(cx, |view, cx| {
            fixture(view, cx);
            let models: Vec<_> = (0..16384)
                .map(|index| format!("fixture/model-{index:05}"))
                .collect();
            let (visible, count) = filtered_models(&models, "");
            assert_eq!(count, 16384);
            assert_eq!(visible.len(), 80);
            let selected = "fixture/model-16383".to_owned();
            assert!(!visible.contains(&selected));
            let (hit, count) = filtered_models(&models, "model-16383");
            assert_eq!(hit, vec![selected.clone()]);
            assert_eq!(count, 1);
            view.catalog.as_mut().expect("catalog").models = models;
            view.act(Action::Model(selected.clone()), cx);
            let stamp = view.stamp().expect("stamp");
            view.ticket = Some(Ticket {
                stamp,
                mode: Mode::Model,
                revision: "opaque".into(),
                changed: 1,
            });
            assert!(
                view.can_apply(),
                "availability is independent of the visible 80 rows"
            );
            view.search.update(cx, |input, cx| {
                input.set_text("model-00001", cx);
            });
        });
        cx.run_until_parked();
        page.update(cx, |view, _| {
            assert_eq!(
                view.stamp().expect("stamp").model.as_deref(),
                Some("fixture/model-16383")
            );
            assert!(view.model_available());
            assert!(
                view.ticket.is_none(),
                "search invalidates preview without clearing selection"
            );
        });
    }
    #[gpui::test]
    fn visual_scenes_are_synthetic_and_cannot_launch_side_effects(cx: &mut TestAppContext) {
        init(cx);
        let page = cx.new(AgentConnections::new);
        page.update(cx, |view, cx| {
            for scene in ["agents", "codex", "codex-update", "connected"] {
                view.preview_fixture(scene, cx);
                assert_eq!(view.adapters.len(), 3);
                assert!(!view.busy());
                if matches!(scene, "codex" | "codex-update") {
                    assert!(view.can_apply());
                    view.act(Action::Apply, cx);
                }
                view.act(Action::Refresh, cx);
                assert!(!view.busy());
                assert!(!view.pending.load(Ordering::Acquire));
                assert!(view.cancellation.is_none());
            }
        });
        cx.run_until_parked();
    }
    #[gpui::test]
    fn connection_settings_disclosure_preserves_draft_preview_and_keyboard_focus(
        cx: &mut TestAppContext,
    ) {
        struct Host(Entity<AgentConnections>);
        impl Render for Host {
            fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
                div().size_full().flex().flex_col().child(self.0.clone())
            }
        }
        init(cx);
        let page = cx.new(AgentConnections::new);
        page.update(cx, |view, cx| view.preview_fixture("codex-update", cx));
        let window = cx.open_window(gpui::size(px(360.0), px(560.0)), |window, cx| {
            gpui::base::Root::new(cx.new(|_| Host(page.clone())), window, cx)
        });
        cx.run_until_parked();
        let mut visual = gpui::VisualTestContext::from_window(window.into(), cx);
        let (stamp, revision, trigger) = visual.update(|window, cx| {
            window.refresh();
            window.draw(cx).clear(cx);
            let view = page.read(cx);
            assert!(!view.details_open);
            let expected = (
                view.stamp().expect("preview target"),
                view.ticket.as_ref().expect("preview").revision.clone(),
                view.focus["agent-details"].clone(),
            );
            expected.2.focus(window, cx);
            window.refresh();
            window.draw(cx).clear(cx);
            expected
        });
        for expanded in [true, false, true, false] {
            // Native button activation happens on key release; the keystroke
            // helper only dispatches KeyDown and therefore cannot click it.
            let keystroke = visual.update(|_, cx| {
                gpui::Keystroke::parse_with_os("enter", cx.platform_info().os).unwrap()
            });
            visual.simulate_event(gpui::KeyDownEvent {
                keystroke: keystroke.clone(),
                is_held: false,
                prefer_character_input: false,
            });
            visual.simulate_event(gpui::KeyUpEvent { keystroke });
            visual.run_until_parked();
            visual.update(|window, cx| {
                window.refresh();
                window.draw(cx).clear(cx);
                let view = page.read(cx);
                assert_eq!(view.details_open, expanded);
                assert!(view.stamp().as_ref() == Some(&stamp));
                assert_eq!(view.config.read(cx).text(), "/synthetic/agent-config");
                assert_eq!(
                    view.ticket.as_ref().expect("retained preview").revision,
                    revision
                );
                assert!(
                    view.can_apply(),
                    "opening settings must not revoke a valid preview"
                );
                assert!(
                    trigger.is_focused(window),
                    "disclosure retains its keyboard focus"
                );
            });
        }
        visual.simulate_keystrokes("tab");
        visual.update(|window, cx| {
            window.refresh();
            window.draw(cx).clear(cx);
            assert!(page.read(cx).focus["agent-read"].is_focused(window));
            assert!(
                !page
                    .read(cx)
                    .config
                    .read(cx)
                    .focus_handle()
                    .is_focused(window)
            );
        });
    }

    #[gpui::test]
    fn collapsing_an_agent_keeps_its_pending_model_and_preview(cx: &mut TestAppContext) {
        init(cx);
        let page = cx.new(AgentConnections::new);
        page.update(cx, |view, cx| {
            view.preview_fixture("codex-update", cx);
            let key = view.selected.clone().expect("selected adapter");
            let stamp = view.stamp().expect("preview target");
            let revision = view.ticket.as_ref().expect("preview").revision.clone();
            for open in [false, true, false, true] {
                view.act(Action::Select(key.clone()), cx);
                assert_eq!(view.editor_open, open);
                assert!(view.stamp().as_ref() == Some(&stamp));
                assert_eq!(
                    view.ticket.as_ref().expect("preserved preview").revision,
                    revision
                );
                assert!(view.can_apply());
                assert!(!view.busy());
            }
        });
    }

    #[gpui::test]
    fn filtered_target_and_model_preview_control_survive_busy_render(cx: &mut TestAppContext) {
        struct Host(Entity<AgentConnections>);
        impl Render for Host {
            fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
                div().size_full().flex().flex_col().child(self.0.clone())
            }
        }
        init(cx);
        let page = cx.new(AgentConnections::new);
        page.update(cx, |view, cx| {
            fixture(view, cx);
            view.act(Action::Model("fixture/model-b".into()), cx);
            view.search.update(cx, |input, cx| {
                input.set_text("model-a", cx);
            });
        });
        cx.run_until_parked();
        let window = cx.open_window(gpui::size(px(360.0), px(560.0)), |window, cx| {
            gpui::base::Root::new(cx.new(|_| Host(page.clone())), window, cx)
        });
        cx.run_until_parked();
        let mut visual = gpui::VisualTestContext::from_window(window.into(), cx);
        let preview_focus = visual.update(|window, cx| {
            window.refresh();
            window.draw(cx).clear(cx);
            let focus = page.read(cx).focus["agent-preview-model"].clone();
            focus.focus(window, cx);
            focus
        });
        for busy in [false, true, false] {
            page.update(&mut visual, |view, cx| {
                // A model preview retains the inspected ownership while its worker runs.
                view.active = busy;
                cx.notify();
            });
            visual.update(|window, cx| {
                // Text snapshots record paints, not cached display-list reuse.
                window.refresh();
                window.draw(cx).clear(cx);
                let view = page.read(cx);
                assert_eq!(
                    view.stamp().expect("retained target").model.as_deref(),
                    Some("fixture/model-b")
                );
                assert!(!view.focus.contains_key("agent-model:fixture/model-b"));
                assert!(view.focus.get("agent-preview-model") == Some(&preview_focus));
                assert!(preview_focus.is_focused(window));
                let texts = gpui::base::test_support::texts(window);
                assert!(
                    texts
                        .iter()
                        .any(|text| text.visible() && text.text() == "fixture/model-b"),
                    "the target must stay visible when search hides its model row: {texts:?}"
                );
            });
        }
    }

    #[gpui::test]
    fn compact_window_renders_dynamic_adapters_and_keeps_apply_guarded(cx: &mut TestAppContext) {
        struct Host(Entity<AgentConnections>);
        impl Render for Host {
            fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
                div().size_full().flex().flex_col().child(self.0.clone())
            }
        }
        init(cx);
        let page = cx.new(AgentConnections::new);
        page.update(cx, fixture);
        let window = cx.open_window(gpui::size(px(380.0), px(620.0)), |window, cx| {
            gpui::base::Root::new(cx.new(|_| Host(page.clone())), window, cx)
        });
        cx.run_until_parked();
        let mut visual = gpui::VisualTestContext::from_window(window.into(), cx);
        visual.update(|window, cx| {
            window.draw(cx).clear(cx);
            assert!(page.read(cx).can_apply());
        });
        page.update(&mut visual, |view, cx| {
            view.search.update(cx, |input, cx| {
                input.set_text("model-b", cx);
            });
        });
        visual.run_until_parked();
        visual.update(|window, cx| {
            window.draw(cx).clear(cx);
            assert!(!page.read(cx).can_apply());
        });
    }
}
