use std::{
    collections::{BTreeMap, BTreeSet},
    net::SocketAddr,
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

use gpui::{
    AccessibleAction, App, ClipboardItem, Context, Div, Entity, FocusHandle, FontWeight,
    IntoElement, KeyDownEvent, Render, Role, ScrollAnchor, ScrollHandle, ScrollStrategy,
    SharedString, Stateful, Subscription, Task, Toggled, UniformListScrollHandle, Window, div, img,
    point, prelude::*, px, rgb, svg, text, uniform_list,
};
use magpie_core::agents::ConfigFileState;

use crate::{
    actions_state::{ActionsState, JobKind, Notice},
    actions_text::{ActionText as A, error_text},
    assets,
    cli_bridge::{self, Ownership},
    gateway_page::{GatewayEvent, GatewayPage},
    gateway_text::GatewayText as G,
    localization::{Locale, Text},
    preferences::{self, Preferences},
    provider_text::ProviderText as P,
    providers::ProvidersPage,
    snapshot::{Agent, Snapshot},
    text_input::{InputEvent, TextInput},
    theme::{self, Appearance, Palette},
};

const BREAKPOINT: f32 = 720.0;
const DOWN: &[u8] = br#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24" fill="none" stroke="white" stroke-width="1.7" stroke-linecap="round" stroke-linejoin="round"><path d="m6 9 6 6 6-6"/></svg>"#;
const CLOSE: &[u8] = br#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24" fill="none" stroke="white" stroke-width="1.7" stroke-linecap="round"><path d="m6 6 12 12M6 18 18 6"/></svg>"#;

const CHECK: &[u8] = br#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24" fill="none" stroke="white" stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round"><path d="m5 12 4 4L19 6"/></svg>"#;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Page {
    Agents,
    Providers,
    Gateway,
    Settings,
}

#[derive(Clone, Copy)]
enum Action {
    Refresh,
    Page(Page),
    Details(&'static str),
    CloseDetails,
    CopyPath(&'static str),
    Language(Locale),
    Appearance(Appearance),
    Run(JobKind),
    CheckConnection,
    ConfirmDisconnect,
    KeepConnection,
    DismissPreview,
    Cancel,
    Escape,
    ToggleDiagnostics,
    ToggleFields,
    OpenModels,
    OpenConnection,
    OpenDisconnect,
    ToggleGateway,
    ToggleRoadmap,
}

/// Native agent browser with explicit Codex CLI transactions.
pub struct MagpieWindow {
    snapshot: Option<Snapshot>,
    refresh_task: Option<Task<()>>,
    page: Page,
    locale: Locale,
    appearance: Appearance,
    providers: Entity<ProvidersPage>,
    gateway: Entity<GatewayPage>,
    gateway_address: SocketAddr,
    gateway_autostart: bool,
    gateway_configured: bool,
    gateway_catalog_invalid: bool,
    gateway_address_to_actions: Option<SocketAddr>,
    pending_gateway_address: Option<SocketAddr>,
    preferences_task: Option<Task<()>>,
    preferences_snapshot: Option<Preferences>,
    preferences_error: Option<&'static str>,
    preferences_ready: bool,
    details: Option<&'static str>,
    copied: Option<&'static str>,
    compact: bool,
    editor_height: f32,
    focus: FocusHandle,
    detail_focus: FocusHandle,
    agent_focus: BTreeMap<&'static str, FocusHandle>,
    control_focus: BTreeMap<&'static str, FocusHandle>,
    scroll_anchors: BTreeMap<&'static str, ScrollAnchor>,
    roster_scroll: ScrollHandle,
    detail_scroll: ScrollHandle,
    settings_scroll: ScrollHandle,
    restore_root_focus: bool,
    actions: ActionsState,
    gateway_input: Entity<TextInput>,
    search_input: Entity<TextInput>,
    _subscriptions: Vec<Subscription>,
    model_focus: BTreeMap<String, FocusHandle>,
    model_scroll: UniformListScrollHandle,
    model_anchor: ScrollAnchor,
    pending_action: Option<Action>,
    pending_focus: Option<&'static str>,
    check_after_refresh: bool,
    diagnostics: bool,
    preview_fields: bool,
    gateway_settings: bool,
    roadmap: bool,
    return_focus: Option<&'static str>,
    close_handler_installed: bool,
    close_requested: bool,
    shutdown_pending: Arc<AtomicBool>,
    preferences_pending: Arc<AtomicBool>,
}

impl MagpieWindow {
    /// Loads local preferences, discovers Agents and supervises a local Gateway.
    /// Agent configuration writes still require explicit preview and confirmation.
    #[cfg_attr(feature = "visual-test", allow(dead_code))]
    pub fn new(cx: &mut Context<Self>) -> Self {
        Self::with_gateway_autostart(true, cx)
    }

    #[cfg(feature = "visual-test")]
    pub fn new_for_preview(cx: &mut Context<Self>) -> Self {
        Self::with_gateway_autostart(false, cx)
    }

    fn with_gateway_autostart(gateway_autostart: bool, cx: &mut Context<Self>) -> Self {
        let roster_scroll = ScrollHandle::new();
        let detail_scroll = ScrollHandle::new();
        let settings_scroll = ScrollHandle::new();
        let mut control_focus = BTreeMap::new();
        let mut scroll_anchors = BTreeMap::new();
        for id in ["theme-system", "theme-light", "theme-dark"]
            .into_iter()
            .chain(Locale::ALL.map(Locale::id))
        {
            control_focus.insert(id, cx.focus_handle().tab_index(0).tab_stop(true));
            scroll_anchors.insert(id, ScrollAnchor::for_handle(settings_scroll.clone()));
        }
        control_focus.insert(
            "copy-config-path",
            cx.focus_handle().tab_index(0).tab_stop(true),
        );
        scroll_anchors.insert(
            "copy-config-path",
            ScrollAnchor::for_handle(detail_scroll.clone()),
        );
        control_focus.insert(
            "empty-refresh",
            cx.focus_handle().tab_index(0).tab_stop(true),
        );
        scroll_anchors.insert(
            "empty-refresh",
            ScrollAnchor::for_handle(roster_scroll.clone()),
        );
        let actions = ActionsState::default();
        let locale = Locale::system();
        let gateway_input = cx.new(|cx| {
            TextInput::new(
                "gateway-address",
                &actions.address,
                "127.0.0.1:3425",
                locale.action_text(A::Gateway),
                cx,
            )
        });
        let search_input = cx.new(|cx| {
            TextInput::new(
                "model-search",
                "",
                locale.action_text(A::SearchPlaceholder),
                locale.action_text(A::Search),
                cx,
            )
        });
        for (id, input) in [
            ("gateway-address", &gateway_input),
            ("model-search", &search_input),
        ] {
            control_focus.insert(id, input.read(cx).focus_handle());
            scroll_anchors.insert(id, ScrollAnchor::for_handle(detail_scroll.clone()));
        }
        for id in [
            "agents-page",
            "providers-page",
            "gateway-page",
            "settings-page",
            "refresh",
            "more-pages",
            "gateway-settings",
            "codex-model",
            "codex-connect",
            "codex-disconnect",
            "close-details",
            "query-models",
            "preview-connect",
            "apply-connect",
            "edit-preview",
            "cancel-operation",
            "check-connection",
            "disconnect",
            "confirm-disconnect",
            "keep-connection",
            "diagnostics",
            "preview-fields",
        ] {
            control_focus.insert(id, cx.focus_handle().tab_index(0).tab_stop(true));
            scroll_anchors.insert(id, ScrollAnchor::for_handle(detail_scroll.clone()));
        }
        let mut subscriptions = vec![
            cx.subscribe(&gateway_input, |view, input, event, cx| {
                match event {
                    InputEvent::Changed => {
                        view.actions.set_address(input.read(cx).text().to_owned());
                    }
                    InputEvent::Submit => view.pending_action = Some(Action::Run(JobKind::Inspect)),
                    InputEvent::Escape => view.pending_action = Some(Action::Escape),
                }
                cx.notify();
            }),
            cx.subscribe(&search_input, |view, input, event, cx| {
                match event {
                    InputEvent::Changed => {
                        view.actions.set_search(input.read(cx).text().to_owned());
                        view.model_scroll.scroll_to_item(0, ScrollStrategy::Top);
                    }
                    InputEvent::Submit => view.pending_action = Some(Action::Run(JobKind::Preview)),
                    InputEvent::Escape => view.pending_action = Some(Action::Escape),
                }
                cx.notify();
            }),
        ];
        subscriptions.push(cx.on_app_quit(|view, cx| {
            view.actions.cancel();
            view.providers.read(cx).cancel();
            view.gateway.update(cx, |page, cx| page.begin_closing(cx));
            let pending = view.shutdown_pending.clone();
            let providers_pending = view.providers.read(cx).shutdown_pending();
            let preferences_pending = view.preferences_pending.clone();
            let gateway_pending = view.gateway.read(cx).shutdown_pending();
            async move {
                // Direct application quit has GPUI's 200ms budget; normal
                // window close below waits for the full bridge cleanup.
                while pending.load(Ordering::Acquire)
                    || providers_pending.load(Ordering::Acquire)
                    || preferences_pending.load(Ordering::Acquire)
                    || gateway_pending
                        .as_ref()
                        .is_some_and(|pending| pending.load(Ordering::Acquire))
                {
                    async_io::Timer::after(Duration::from_millis(10)).await;
                }
            }
        }));
        let model_anchor = ScrollAnchor::for_handle(detail_scroll.clone());
        let providers = cx.new(ProvidersPage::new);
        let gateway = cx.new(GatewayPage::new);
        subscriptions.push(cx.observe(&providers, |_, _, cx| cx.notify()));
        subscriptions.push(cx.observe(&gateway, |_, _, cx| cx.notify()));
        subscriptions.push(cx.subscribe(&gateway, |view, _, event, cx| {
            match event {
                GatewayEvent::Changed => view.gateway_catalog_invalid = true,
                GatewayEvent::SaveAddress(address) => {
                    if view.preferences_task.is_some() || view.close_requested {
                        view.gateway
                            .update(cx, |page, cx| page.address_saved(false, cx));
                    } else {
                        view.pending_gateway_address = Some(*address);
                        view.save_preferences(cx);
                    }
                }
            }
            cx.notify();
        }));
        let mut view = Self {
            snapshot: None,
            refresh_task: None,
            page: Page::Agents,
            locale: Locale::system(),
            appearance: Appearance::System,
            providers,
            gateway,
            gateway_address: magpie_core::gateway::default_gateway_addr(),
            gateway_autostart,
            gateway_configured: false,
            gateway_catalog_invalid: false,
            gateway_address_to_actions: None,
            pending_gateway_address: None,
            preferences_task: None,
            preferences_snapshot: None,
            preferences_error: None,
            preferences_ready: false,
            details: None,
            copied: None,
            compact: false,
            editor_height: 350.0,
            focus: cx.focus_handle(),
            detail_focus: cx.focus_handle().tab_index(0).tab_stop(true),
            agent_focus: BTreeMap::new(),
            control_focus,
            scroll_anchors,
            roster_scroll,
            detail_scroll,
            settings_scroll,
            restore_root_focus: false,
            actions,
            gateway_input,
            search_input,
            _subscriptions: subscriptions,
            model_focus: BTreeMap::new(),
            model_scroll: UniformListScrollHandle::new(),
            model_anchor,
            pending_action: None,
            pending_focus: None,
            check_after_refresh: true,
            diagnostics: false,
            preview_fields: false,
            gateway_settings: false,
            roadmap: false,
            return_focus: None,
            close_handler_installed: false,
            close_requested: false,
            shutdown_pending: Arc::new(AtomicBool::new(false)),
            preferences_pending: Arc::new(AtomicBool::new(false)),
        };
        view.refresh(cx);
        view.load_preferences(cx);
        view
    }

    pub fn focus(&self, window: &mut Window, cx: &mut App) {
        self.focus.focus(window, cx);
    }

    fn load_preferences(&mut self, cx: &mut Context<Self>) {
        if self.preferences_task.is_some() {
            return;
        }
        let read = cx
            .background_executor()
            .spawn(async { preferences::path().and_then(|path| preferences::load(&path)) });
        self.preferences_task = Some(cx.spawn(async move |this, cx| {
            let result = read.await;
            let _ = this.update(cx, |view, cx| {
                view.preferences_task = None;
                view.preferences_ready = true;
                match result {
                    Ok(preferences) => {
                        if let Some(value) = &preferences {
                            match value.values() {
                                Ok((locale, appearance)) => {
                                    view.locale = locale;
                                    view.appearance = appearance;
                                }
                                Err(error) => view.preferences_error = Some(error),
                            }
                        }
                        let address = preferences
                            .as_ref()
                            .and_then(|value| value.gateway_address().ok())
                            .unwrap_or_else(magpie_core::gateway::default_gateway_addr);
                        if !view.gateway_configured {
                            view.gateway_address_to_actions = Some(address);
                        }
                        view.gateway_configured = true;
                        view.gateway_address = address;
                        view.gateway.update(cx, |page, cx| {
                            page.configure(
                                address,
                                view.gateway_autostart && !view.close_requested,
                                cx,
                            )
                        });
                        view.preferences_snapshot = preferences;
                        view.preferences_error = None;
                    }
                    Err(error) => {
                        view.preferences_error = Some(error);
                        view.gateway.update(cx, |page, cx| {
                            page.configure(view.gateway_address, false, cx)
                        });
                    }
                }
                cx.notify();
            });
        }));
        cx.notify();
    }

    fn save_preferences(&mut self, cx: &mut Context<Self>) {
        if self.preferences_task.is_some() {
            return;
        }
        let value = Preferences::new(self.locale, self.appearance)
            .with_gateway_address(self.pending_gateway_address.unwrap_or(self.gateway_address));
        let expected = self.preferences_snapshot.clone();
        let saved = value.clone();
        self.preferences_pending.store(true, Ordering::Release);
        let pending = self.preferences_pending.clone();
        let write = cx.background_executor().spawn(async move {
            let result = preferences::path()
                .and_then(|path| preferences::save(&path, expected.as_ref(), &value));
            pending.store(false, Ordering::Release);
            result
        });
        self.preferences_task = Some(cx.spawn(async move |this, cx| {
            let result = write.await;
            let _ = this.update(cx, |view, cx| {
                view.preferences_task = None;
                match result {
                    Ok(()) => {
                        if let Some(address) = view.pending_gateway_address.take() {
                            if cli_bridge::parse_address(&view.actions.address).ok()
                                == Some(view.gateway_address)
                            {
                                view.gateway_address_to_actions = Some(address);
                            }
                            view.gateway_address = address;
                            view.gateway
                                .update(cx, |page, cx| page.address_saved(true, cx));
                        }
                        view.preferences_snapshot = Some(saved);
                        view.preferences_error = None;
                    }
                    Err(error) => {
                        if view.pending_gateway_address.take().is_some() {
                            view.gateway
                                .update(cx, |page, cx| page.address_saved(false, cx));
                        }
                        view.preferences_error = Some(error);
                    }
                }
                cx.notify();
            });
        }));
        cx.notify();
    }

    fn refresh(&mut self, cx: &mut Context<Self>) {
        if self.refresh_task.is_some() || self.actions.busy().is_some() {
            return;
        }
        self.copied = None;
        let scan = cx.background_executor().spawn(async { Snapshot::read() });
        self.refresh_task = Some(cx.spawn(async move |this, cx| {
            let mut snapshot = scan.await;
            let _ = this.update(cx, |view, cx| {
                if let Some(previous) = &view.snapshot {
                    snapshot.retain_last_readable(previous);
                }
                // Vector order can change during a scan. Identity, selection and
                // focus are deliberately tied to stable Agent IDs instead.
                // Keep the five known handles even when an Agent disappears.
                // Render can then detect focus on a removed, unselected row.
                for agent in &snapshot.agents {
                    view.agent_focus
                        .entry(agent.id)
                        .or_insert_with(|| cx.focus_handle().tab_index(0).tab_stop(true));
                }
                if view
                    .details
                    .is_some_and(|id| !snapshot.agents.iter().any(|agent| agent.id == id))
                {
                    view.details = None;
                    view.restore_root_focus = view.page == Page::Agents;
                }
                let codex = snapshot.agents.iter().find(|agent| agent.id == "codex");
                let path = codex
                    .and_then(|agent| agent.config.as_ref())
                    .and_then(|config| config.path.clone());
                view.actions.set_config_path(path.clone());
                let fresh = codex.is_some_and(|agent| {
                    !agent.last_known
                        && agent.config.as_ref().is_some_and(|config| {
                            config.state == ConfigFileState::Missing || config.fields_readable
                        })
                }) && view.actions.config_path().map(PathBuf::from) == path;
                view.actions.acknowledge_snapshot_refresh(fresh);
                view.actions.set_current_model(
                    codex
                        .and_then(|agent| agent.settings.as_ref())
                        .and_then(|settings| settings.model.clone()),
                );
                view.snapshot = Some(snapshot);
                view.refresh_task = None;
                if view.check_after_refresh {
                    view.check_after_refresh = false;
                    // Discovery can remove Codex while a mutation is finishing.
                    // The unified Codex row exposes progress and cancellation
                    // even when its editor is collapsed. If discovery removes
                    // it, keep the unresolved state without a hidden job.
                    if view.page == Page::Agents && view.agent("codex").is_some() {
                        view.start_job(JobKind::Status, cx);
                    }
                }
                cx.notify();
            });
        }));
        cx.notify();
    }

    fn agent(&self, id: &str) -> Option<&Agent> {
        self.snapshot
            .as_ref()
            .and_then(|snapshot| snapshot.agents.iter().find(|agent| agent.id == id))
    }

    fn ordered_agents(&self) -> Vec<&Agent> {
        let mut agents: Vec<_> = self
            .snapshot
            .as_ref()
            .into_iter()
            .flat_map(|snapshot| snapshot.agents.iter())
            .collect();
        // Presentation only: prioritize the implemented connection workflow.
        // The discovery result and stable IDs remain unchanged.
        agents.sort_by_key(|agent| agent.id != "codex");
        agents
    }

    fn row_index(&self, id: &str) -> Option<usize> {
        self.ordered_agents()
            .iter()
            .position(|agent| agent.id == id)
    }

    fn open_codex(&mut self, origin: &'static str, window: &mut Window, cx: &mut Context<Self>) {
        if self.agent("codex").is_none() {
            return;
        }
        let entering = self.details != Some("codex");
        self.details = Some("codex");
        self.return_focus = Some(origin);
        self.roadmap = false;
        if entering {
            self.diagnostics = false;
            self.detail_scroll.set_offset(point(px(0.0), px(0.0)));
            self.check_connection(cx);
        }
        Self::focus_next_frame(self.detail_focus.clone(), window);
        if let Some(index) = self.row_index("codex") {
            self.roster_scroll.scroll_to_item(index);
        }
    }

    fn focus_next_frame(handle: FocusHandle, window: &mut Window) {
        // On narrow windows the destination element is not in the old tree.
        window.on_next_frame(move |window, cx| handle.focus(window, cx));
    }

    fn reveal_focused(&self, window: &mut Window, cx: &mut App) {
        if self.snapshot.is_some() {
            for (index, agent) in self.ordered_agents().iter().enumerate() {
                if self
                    .agent_focus
                    .get(agent.id)
                    .is_some_and(|handle| handle.is_focused(window))
                {
                    // Rows are direct children of this scroller. GPUI scrolls
                    // only the minimal amount needed to reveal the whole row.
                    self.roster_scroll.scroll_to_item(index);
                    window.refresh();
                    return;
                }
            }
        }
        for (index, model) in self.actions.visible_models().iter().enumerate() {
            if self
                .model_focus
                .get(&model.id)
                .is_some_and(|handle| handle.is_focused(window))
            {
                if let Some(index) = self.row_index("codex") {
                    self.roster_scroll.scroll_to_item(index);
                }
                self.model_scroll
                    .scroll_to_item(index, ScrollStrategy::Nearest);
                self.model_anchor.scroll_to(window, cx);
                window.on_next_frame(|window, _| window.refresh());
                return;
            }
        }
        if self.detail_focus.is_focused(window) {
            if let Some(index) = self.details.and_then(|id| self.row_index(id)) {
                self.roster_scroll.scroll_to_item(index);
                window.refresh();
            }
            return;
        }
        for (id, handle) in &self.control_focus {
            if !handle.is_focused(window) {
                continue;
            }
            let row_control = matches!(*id, "codex-model" | "codex-connect" | "codex-disconnect")
                || (*id == "cancel-operation" && self.details != Some("codex"));
            if row_control {
                if let Some(index) = self.row_index("codex") {
                    self.roster_scroll.scroll_to_item(index);
                    window.refresh();
                }
                return;
            }
            if matches!(
                *id,
                "agents-page" | "providers-page" | "settings-page" | "refresh" | "more-pages"
            ) {
                return;
            }
            if self.page == Page::Agents
                && let Some(index) = self.details.and_then(|id| self.row_index(id))
            {
                self.roster_scroll.scroll_to_item(index);
            }
            if let Some(anchor) = self.scroll_anchors.get(id) {
                anchor.scroll_to(window, cx);
                window.on_next_frame(|window, _| window.refresh());
            }
            return;
        }
    }

    fn close_details(&mut self, window: &mut Window) {
        if let Some(id) = self.details.take() {
            let origin = self.return_focus.take().map(|origin| {
                let managed = self
                    .actions
                    .ownership
                    .is_some_and(|state| state != Ownership::Unmanaged);
                if (origin == "codex-connect"
                    && (managed || !self.action_enabled(Action::OpenConnection)))
                    || (origin == "codex-disconnect"
                        && (!managed || !self.action_enabled(Action::OpenDisconnect)))
                {
                    "codex-model"
                } else {
                    origin
                }
            });
            let handle = origin
                .and_then(|id| self.control_focus.get(id))
                .or_else(|| self.agent_focus.get(id))
                .unwrap_or(&self.focus)
                .clone();
            let index = self.row_index(id);
            let scroll = self.roster_scroll.clone();
            window.on_next_frame(move |window, cx| {
                handle.focus(window, cx);
                if let Some(index) = index {
                    scroll.scroll_to_item(index);
                    window.refresh();
                }
            });
        }
        self.copied = None;
        self.diagnostics = false;
    }

    fn act(&mut self, action: Action, window: &mut Window, cx: &mut Context<Self>) {
        if !self.action_enabled(action) {
            return;
        }
        match action {
            Action::Refresh => {
                if self.page == Page::Gateway {
                    self.gateway.update(cx, |page, cx| page.refresh(cx));
                    return;
                }
                if self.page == Page::Providers {
                    self.providers.update(cx, |page, cx| page.refresh(cx));
                    return;
                }
                if self.page == Page::Settings {
                    self.load_preferences(cx);
                    return;
                }
                if self.roadmap {
                    self.pending_focus = Some("more-pages");
                }
                self.roadmap = false;
                self.actions.invalidate_preview();
                self.check_after_refresh = self.page == Page::Agents;
                if self.check_after_refresh {
                    self.actions.ownership = None;
                }
                self.refresh(cx);
            }
            Action::Page(page) => {
                let entering = self.page != page && page == Page::Agents;
                let entering_gateway = self.page != page && page == Page::Gateway;
                self.page = page;
                self.roadmap = false;
                if entering {
                    self.check_connection(cx);
                }
                if page == Page::Providers {
                    self.providers.update(cx, |page, cx| page.refresh(cx));
                }
                if entering_gateway {
                    self.gateway.update(cx, |page, cx| page.refresh(cx));
                }
            }
            Action::Details(id) => {
                if self.details == Some(id) {
                    self.close_details(window);
                } else if self.agent(id).is_some() {
                    self.details = Some(id);
                    self.return_focus = None;
                    self.roadmap = false;
                    self.diagnostics = false;
                    self.copied = None;
                    self.detail_scroll.set_offset(point(px(0.0), px(0.0)));
                    Self::focus_next_frame(self.detail_focus.clone(), window);
                    if id == "codex" {
                        self.check_connection(cx);
                    }
                }
            }
            Action::CloseDetails => self.close_details(window),
            Action::OpenModels => self.open_codex("codex-model", window, cx),
            Action::OpenConnection => self.open_codex("codex-connect", window, cx),
            Action::OpenDisconnect => {
                // This is the same explicit confirmation as the expanded
                // action. It never invokes a mutation from the row itself.
                self.details = Some("codex");
                self.return_focus = Some("codex-disconnect");
                self.roadmap = false;
                self.actions.invalidate_preview();
                self.actions.disconnect_confirmation = true;
                self.pending_focus = Some("confirm-disconnect");
                if let Some(index) = self.row_index("codex") {
                    self.roster_scroll.scroll_to_item(index);
                }
            }
            Action::CopyPath(id) => {
                if let Some(path) = self
                    .agent(id)
                    .and_then(|agent| agent.config.as_ref())
                    .and_then(|config| config.path.as_ref())
                {
                    // Native text wrapping leaves the original clipboard value intact.
                    cx.write_to_clipboard(ClipboardItem::new_string(path.display().to_string()));
                    self.copied = Some(id);
                }
            }
            Action::Language(locale) => {
                self.locale = locale;
                self.save_preferences(cx);
            }
            Action::Appearance(appearance) => {
                self.appearance = appearance;
                self.save_preferences(cx);
            }
            Action::Run(kind) => self.start_job(kind, cx),
            Action::CheckConnection => self.check_connection(cx),
            Action::ConfirmDisconnect => {
                self.actions.invalidate_preview();
                self.actions.disconnect_confirmation = true;
                self.pending_focus = Some("confirm-disconnect");
            }
            Action::KeepConnection => {
                self.actions.disconnect_confirmation = false;
                self.pending_focus = Some("disconnect");
            }
            Action::DismissPreview => {
                self.actions.invalidate_preview();
                self.pending_focus = Some("model-search");
            }
            Action::Cancel => self.actions.cancel(),
            Action::ToggleDiagnostics => self.diagnostics = !self.diagnostics,
            Action::ToggleFields => self.preview_fields = !self.preview_fields,
            Action::ToggleGateway => self.gateway_settings = !self.gateway_settings,
            Action::ToggleRoadmap => {
                self.roadmap = !self.roadmap;
                self.pending_focus = Some("more-pages");
            }
            Action::Escape => {
                if self.roadmap {
                    self.roadmap = false;
                    self.pending_focus = Some("more-pages");
                } else if self.actions.busy().is_some() {
                    self.actions.cancel();
                } else if self.actions.disconnect_confirmation {
                    self.actions.disconnect_confirmation = false;
                    self.pending_focus = Some("disconnect");
                } else if self.actions.preview.is_some() {
                    self.actions.invalidate_preview();
                    self.pending_focus = Some("model-search");
                } else {
                    self.close_details(window);
                }
            }
        }
        cx.notify();
    }

    fn key_down(&mut self, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        let key = &event.keystroke;
        match key.key.as_str() {
            "f5" => self.act(Action::Refresh, window, cx),
            "r" if key.modifiers.control || key.modifiers.platform => {
                self.act(Action::Refresh, window, cx)
            }
            "escape"
                if self.roadmap
                    || (self.page == Page::Agents
                        && (self.details.is_some() || self.actions.busy().is_some())) =>
            {
                self.act(Action::Escape, window, cx);
            }
            "tab" => {
                if key.modifiers.shift {
                    window.focus_prev(cx);
                } else {
                    window.focus_next(cx);
                }
                self.reveal_focused(window, cx);
            }
            _ => return,
        }
        cx.stop_propagation();
    }

    fn button(
        &self,
        id: &'static str,
        label: impl Into<SharedString>,
        icon: Option<&'static [u8]>,
        action: Action,
        palette: Palette,
        cx: &mut Context<Self>,
    ) -> Stateful<Div> {
        let label = label.into();
        let busy = !self.action_enabled(action);
        let toggled = match action {
            Action::Page(page) => Some(page == self.page),
            Action::Language(locale) => Some(locale == self.locale),
            Action::Appearance(appearance) => Some(appearance == self.appearance),
            Action::ToggleDiagnostics => Some(self.diagnostics),
            Action::ToggleFields => Some(self.preview_fields),
            Action::ToggleGateway => Some(self.gateway_settings),
            Action::ToggleRoadmap => Some(self.roadmap),
            _ => None,
        };
        let selected = toggled == Some(true);
        let view = cx.entity().downgrade();
        div()
            .id(id)
            .when_some(self.control_focus.get(id), |button, handle| {
                button.track_focus(&handle.clone().tab_stop(!busy))
            })
            .when_some(self.scroll_anchors.get(id), |button, anchor| {
                button.anchor_scroll(Some(anchor.clone()))
            })
            // This GPUI revision has no Div disabled-state builder. A busy
            // refresh becomes a non-actionable status, with no click handlers.
            .role(if busy { Role::Status } else { Role::Button })
            .aria_label(label.clone())
            .tab_index(0)
            .tab_stop(!busy)
            .when_some(toggled, |button, selected| {
                button.aria_toggled(if selected {
                    Toggled::True
                } else {
                    Toggled::False
                })
            })
            .h(px(if self.compact { 40.0 } else { 34.0 }))
            .flex_none()
            .px(px(10.0))
            .flex()
            .items_center()
            .justify_center()
            .gap(px(6.0))
            .rounded(px(6.0))
            .border_2()
            .border_color(gpui::rgba(0))
            .bg(rgb(if selected {
                palette.selected
            } else {
                palette.surface
            }))
            .text_color(rgb(if selected {
                palette.accent
            } else {
                palette.muted
            }))
            .text_size(px(13.0))
            .line_height(px(18.0))
            .whitespace_nowrap()
            .when(busy, |button| button.opacity(0.6))
            .when(!busy, |button| {
                button
                    .cursor_pointer()
                    .hover(move |style| {
                        style
                            .bg(rgb(if selected {
                                palette.selected
                            } else {
                                palette.inset
                            }))
                            .text_color(rgb(palette.text))
                    })
                    .focus_visible(move |style| style.border_color(rgb(palette.accent)))
                    // GPUI's on_click already handles Enter/Space on keyup.
                    // A second keydown activation would run the action twice.
                    .on_click(cx.listener(move |view, _, window, cx| {
                        view.act(action, window, cx);
                    }))
                    .on_a11y_action(AccessibleAction::Click, move |_, window, cx| {
                        let _ = view.update(cx, |view, cx| view.act(action, window, cx));
                    })
            })
            .when_some(icon, |button, data| {
                button.child(svg().data(data).size(px(16.0)).flex_none().text_color(rgb(
                    if selected {
                        palette.accent
                    } else {
                        palette.muted
                    },
                )))
            })
            .when(!label.is_empty(), |button| button.child(text!(label)))
    }

    fn config_label(agent: &Agent) -> Text {
        let Some(config) = &agent.config else {
            return Text::Installed;
        };
        match config.state {
            ConfigFileState::ValidToml if !config.fields_readable => Text::ConfigUnsupported,
            ConfigFileState::ValidToml => Text::ConfigReady,
            ConfigFileState::Missing => Text::ConfigMissing,
            ConfigFileState::InvalidToml => Text::ConfigInvalid,
            ConfigFileState::TooLarge => Text::ConfigLarge,
            ConfigFileState::InvalidUtf8 => Text::ConfigEncoding,
            ConfigFileState::Unreadable => Text::ConfigUnreadable,
            ConfigFileState::NotAFile => Text::ConfigNotFile,
            ConfigFileState::Symlink => Text::ConfigSymlink,
            ConfigFileState::Unavailable => Text::ConfigUnavailable,
        }
    }

    fn config_hint(agent: &Agent) -> Text {
        let Some(config) = &agent.config else {
            return Text::OtherAgentHint;
        };
        match config.state {
            ConfigFileState::ValidToml if !config.fields_readable => Text::UnsupportedHint,
            ConfigFileState::ValidToml => Text::ValidHint,
            ConfigFileState::Missing => Text::MissingHint,
            ConfigFileState::InvalidToml => Text::InvalidHint,
            ConfigFileState::TooLarge => Text::TooLargeHint,
            ConfigFileState::InvalidUtf8 => Text::Utf8Hint,
            ConfigFileState::Unreadable => Text::UnreadableHint,
            ConfigFileState::NotAFile => Text::NotAFileHint,
            ConfigFileState::Symlink => Text::SymlinkHint,
            ConfigFileState::Unavailable => Text::UnavailableHint,
        }
    }

    fn config_problem(agent: &Agent) -> bool {
        agent
            .config
            .as_ref()
            .is_some_and(|config| match config.state {
                ConfigFileState::Missing => false,
                ConfigFileState::ValidToml => !config.fields_readable,
                _ => true,
            })
    }

    fn config_colors(agent: &Agent, palette: Palette) -> (u32, u32) {
        if agent.config.as_ref().is_some_and(|config| {
            matches!(
                config.state,
                ConfigFileState::InvalidToml | ConfigFileState::InvalidUtf8
            )
        }) {
            (palette.danger, palette.danger_bg)
        } else if Self::config_problem(agent) {
            (palette.warning, palette.warning_bg)
        } else {
            (palette.muted, palette.inset)
        }
    }

    fn icon(agent: &Agent, palette: Palette) -> gpui::AnyElement {
        match agent.icon {
            Some("icons/opencode.svg") => svg()
                .path("icons/opencode.svg")
                .size(px(28.0))
                .text_color(rgb(palette.text))
                .into_any_element(),
            Some(path) => img(path).size(px(28.0)).into_any_element(),
            None => svg()
                .data(assets::TERMINAL)
                .size(px(28.0))
                .text_color(rgb(palette.text))
                .into_any_element(),
        }
    }

    fn caption(&self, key: Text, palette: Palette) -> Div {
        div()
            .text_size(px(12.0))
            .line_height(px(16.0))
            .text_color(rgb(palette.muted))
            .whitespace_normal()
            .child(text!(self.locale.text(key)))
    }

    fn field(&self, key: Text, value: String, palette: Palette, large: bool) -> Div {
        div()
            .w_full()
            .min_w_0()
            .flex()
            .flex_col()
            .gap(px(4.0))
            .child(self.caption(key, palette))
            .child(
                div()
                    .w_full()
                    .min_w_0()
                    .text_size(px(if large { 20.0 } else { 13.0 }))
                    .line_height(px(if large { 28.0 } else { 20.0 }))
                    .when(large, |value| value.font_weight(FontWeight::MEDIUM))
                    // GPUI wraps long unbroken runs at glyph boundaries when
                    // the width is definite. Do not alter the user's string.
                    .whitespace_normal()
                    .child(text!(value)),
            )
    }

    fn panel(&self, palette: Palette) -> Div {
        div()
            .min_w_0()
            .min_h_0()
            .flex()
            .flex_col()
            .rounded(px(10.0))
            .border_1()
            .border_color(rgb(palette.border))
            .bg(rgb(palette.surface))
            .overflow_hidden()
    }

    fn ownership_label(&self) -> A {
        match self.actions.ownership {
            None => A::RowUnknown,
            Some(Ownership::Unmanaged) => A::Unmanaged,
            Some(Ownership::Connected) => A::Connected,
            Some(Ownership::Drifted) => A::Drifted,
            Some(Ownership::RecoveryRequired) => A::RecoveryRequired,
        }
    }

    fn progress_label(kind: JobKind) -> A {
        match kind {
            JobKind::Inspect => A::Querying,
            JobKind::Preview => A::Previewing,
            JobKind::Apply => A::Saving,
            JobKind::Status => A::Checking,
            JobKind::Disconnect => A::Restoring,
        }
    }

    fn agent_row(&self, agent: &Agent, palette: Palette, cx: &mut Context<Self>) -> Div {
        let selected = self.details == Some(agent.id);
        let codex = agent.id == "codex";
        let (config_color, _) = Self::config_colors(agent, palette);
        let status = if !codex {
            self.locale.action_text(A::ReadOnly).to_owned()
        } else if let Some(kind) = self.actions.busy() {
            self.locale
                .action_text(Self::progress_label(kind))
                .to_owned()
        } else if self.check_after_refresh && self.refresh_task.is_some() {
            self.locale.action_text(A::Checking).to_owned()
        } else if self.actions.uncertain || self.actions.reconcile_needed() {
            self.locale.action_text(A::NeedsReview).to_owned()
        } else if agent.last_known {
            self.locale.text(Text::LastKnown).to_owned()
        } else if Self::config_problem(agent) {
            self.locale.text(Self::config_label(agent)).to_owned()
        } else if self.actions.error.is_some() && self.actions.ownership.is_none() {
            self.locale.action_text(A::RowUnknown).to_owned()
        } else {
            self.locale.action_text(self.ownership_label()).to_owned()
        };
        let status_color = if codex
            && (self.actions.uncertain
                || self.actions.reconcile_needed()
                || matches!(
                    self.actions.ownership,
                    Some(Ownership::Drifted | Ownership::RecoveryRequired)
                )) {
            palette.warning
        } else {
            config_color
        };
        let identity = div()
            .flex_1()
            .min_w_0()
            .flex()
            .items_center()
            .gap(px(12.0))
            .child(
                div()
                    .size(px(28.0))
                    .flex_none()
                    .child(Self::icon(agent, palette)),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .flex_col()
                    .gap(px(2.0))
                    .child(
                        div()
                            .text_size(px(14.0))
                            .line_height(px(20.0))
                            .font_weight(FontWeight::SEMIBOLD)
                            .child(text!(agent.name)),
                    )
                    .child(
                        div()
                            .id(SharedString::from(format!("agent-status:{}", agent.id)))
                            .role(Role::Status)
                            .w_full()
                            .min_w_0()
                            .whitespace_normal()
                            .text_size(px(12.0))
                            .line_height(px(18.0))
                            .text_color(rgb(status_color))
                            .child(text!(status)),
                    ),
            );
        let mut controls = div()
            .flex()
            .items_center()
            .gap(px(10.0))
            .min_w_0()
            .when(self.compact, |row| row.w_full().pl(px(40.0)).justify_end());
        if codex {
            // A draft selection is never advertised as the current saved model.
            let model = agent
                .settings
                .as_ref()
                .and_then(|settings| settings.model.as_deref())
                .unwrap_or(self.locale.action_text(A::ChooseModel));
            let picker = self
                .button("codex-model", "", None, Action::OpenModels, palette, cx)
                .aria_label(format!(
                    "{} · {} · {model}",
                    agent.name,
                    self.locale.text(Text::Model)
                ))
                .aria_expanded(selected)
                .w(px(220.0))
                .min_w_0()
                .justify_between()
                .bg(rgb(palette.inset))
                .text_color(rgb(palette.text))
                .when(self.compact, |button| button.flex_1().w_auto())
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .truncate()
                        .child(text!(model.to_owned())),
                )
                .child(
                    svg()
                        .data(DOWN)
                        .size(px(14.0))
                        .flex_none()
                        .text_color(rgb(palette.muted)),
                );
            controls = controls.child(picker);
            if self.actions.busy().is_some() && !selected {
                controls = controls.child(self.action_button(
                    "cancel-operation",
                    A::Cancel,
                    Action::Cancel,
                    palette,
                    cx,
                ));
            } else {
                let managed = self
                    .actions
                    .ownership
                    .is_some_and(|state| state != Ownership::Unmanaged);
                let (id, label, action) = if managed {
                    (
                        "codex-disconnect",
                        A::DisconnectShort,
                        Action::OpenDisconnect,
                    )
                } else {
                    ("codex-connect", A::ConnectShort, Action::OpenConnection)
                };
                let enabled = self.action_enabled(action);
                controls = controls.child(
                    self.action_button(id, label, action, palette, cx)
                        .min_w(px(if self.compact { 76.0 } else { 80.0 }))
                        .when(!managed && enabled, |button| {
                            button
                                .bg(rgb(palette.text))
                                .text_color(rgb(palette.surface))
                        }),
                );
            }
        } else {
            controls = controls.child(
                self.button(
                    agent.id,
                    self.locale.action_text(A::Details),
                    Some(assets::CHEVRON),
                    Action::Details(agent.id),
                    palette,
                    cx,
                )
                .aria_label(format!(
                    "{} · {}",
                    agent.name,
                    self.locale.action_text(A::Details)
                ))
                .aria_expanded(selected)
                .when_some(self.agent_focus.get(agent.id), |button, focus| {
                    button.track_focus(
                        &focus
                            .clone()
                            .tab_stop(self.action_enabled(Action::Details(agent.id))),
                    )
                })
                .text_color(rgb(palette.muted)),
            );
        }
        let row = div()
            .w_full()
            .min_w_0()
            .min_h(px(72.0))
            .px(px(16.0))
            .py(px(12.0))
            .bg(rgb(if selected {
                palette.inset
            } else {
                palette.surface
            }))
            .flex()
            .items_center()
            .gap(px(16.0))
            .when(self.compact, |row| {
                row.flex_col().items_stretch().gap(px(10.0))
            })
            .child(identity)
            .child(controls);
        div()
            .w_full()
            .min_w_0()
            .flex_none()
            .flex()
            .flex_col()
            .border_b_1()
            .border_color(rgb(palette.border))
            .child(row)
            .when(selected, |row| {
                row.child(self.inspector(agent, palette, cx))
            })
    }

    fn roster(&self, _snapshot: &Snapshot, palette: Palette, cx: &mut Context<Self>) -> Div {
        let rows = div()
            .id("roster-scroll")
            .flex_1()
            .min_h_0()
            .min_w_0()
            .flex()
            .flex_col()
            .overflow_y_scroll()
            .track_scroll(&self.roster_scroll)
            .children(
                self.ordered_agents()
                    .into_iter()
                    .map(|agent| self.agent_row(agent, palette, cx)),
            );
        self.panel(palette)
            .h_full()
            .when(!self.compact && self.details.is_none(), |panel| {
                panel.max_h(px(self.ordered_agents().len() as f32 * 73.0))
            })
            .child(rows)
    }

    fn config_notice(&self, agent: &Agent, palette: Palette) -> Div {
        let (color, background) = Self::config_colors(agent, palette);
        div()
            .w_full()
            .min_w_0()
            .p(px(12.0))
            .rounded(px(6.0))
            .bg(rgb(background))
            .text_color(rgb(color))
            .flex()
            .flex_col()
            .gap(px(6.0))
            .child(
                div()
                    .text_size(px(13.0))
                    .line_height(px(18.0))
                    .font_weight(FontWeight::SEMIBOLD)
                    .whitespace_normal()
                    .child(text!(self.locale.text(Self::config_label(agent)))),
            )
            .child(
                div()
                    .w_full()
                    .text_size(px(12.0))
                    .line_height(px(18.0))
                    .whitespace_normal()
                    .child(text!(self.locale.text(Self::config_hint(agent)))),
            )
            .when_some(
                agent.config.as_ref().and_then(|config| config.error_line),
                |notice, line| {
                    notice.child(div().text_size(px(12.0)).line_height(px(18.0)).child(text!(
                        format!("{} {line}", self.locale.text(Text::ErrorLine))
                    )))
                },
            )
    }

    fn model_values(&self, agent: &Agent, palette: Palette) -> Div {
        let mut group = div().w_full().min_w_0().flex().flex_col().gap(px(12.0));
        if let Some(settings) = &agent.settings {
            if agent.last_known {
                group = group
                    .p(px(12.0))
                    .rounded(px(6.0))
                    .bg(rgb(palette.warning_bg))
                    .child(
                        div()
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(rgb(palette.warning))
                            .text_size(px(13.0))
                            .line_height(px(18.0))
                            .child(text!(self.locale.text(Text::LastKnown))),
                    )
                    .child(
                        div()
                            .w_full()
                            .text_color(rgb(palette.warning))
                            .text_size(px(12.0))
                            .line_height(px(18.0))
                            .whitespace_normal()
                            .child(text!(self.locale.text(Text::LastKnownHint))),
                    );
            }
            let unset = self.locale.text(Text::NotSet);
            group = group.child(self.field(
                Text::Model,
                settings.model.as_deref().unwrap_or(unset).to_owned(),
                palette,
                true,
            ));
            let provider = self.field(
                Text::Provider,
                settings.provider.as_deref().unwrap_or(unset).to_owned(),
                palette,
                false,
            );
            let effort = self.field(
                Text::Effort,
                settings.effort.as_deref().unwrap_or(unset).to_owned(),
                palette,
                false,
            );
            group = group.child(
                div()
                    .w_full()
                    .min_w_0()
                    .flex()
                    .gap(px(16.0))
                    .when(self.compact, |columns| columns.flex_col().gap(px(12.0)))
                    .child(div().flex_1().min_w_0().child(provider))
                    .child(div().flex_1().min_w_0().child(effort)),
            );
            if !agent.last_known {
                group = group.child(self.caption(Text::ConfigSource, palette));
            }
        } else {
            group = group.child(self.caption(Text::NoReadableValues, palette));
        }
        group
    }

    fn inspector(&self, agent: &Agent, palette: Palette, cx: &mut Context<Self>) -> Div {
        let title = if agent.id != "codex" {
            self.locale.action_text(A::Details)
        } else if self.actions.preview.is_some() {
            self.locale.action_text(A::PreviewTitle)
        } else if self.actions.disconnect_confirmation {
            self.locale.action_text(A::Disconnect)
        } else {
            self.locale.action_text(A::ChooseModel)
        };
        let header = div()
            .h(px(44.0))
            .flex_none()
            .px(px(16.0))
            .flex()
            .items_center()
            .justify_between()
            .gap(px(8.0))
            .child(
                div()
                    .text_size(px(13.0))
                    .font_weight(FontWeight::SEMIBOLD)
                    .flex_1()
                    .child(text!(title)),
            )
            .when(agent.id == "codex", |header| {
                header.child(
                    self.button(
                        "check-connection",
                        "",
                        Some(assets::REFRESH),
                        Action::CheckConnection,
                        palette,
                        cx,
                    )
                    .aria_label(self.locale.action_text(A::StatusRefresh)),
                )
            })
            .child(
                self.button(
                    "close-details",
                    "",
                    Some(CLOSE),
                    Action::CloseDetails,
                    palette,
                    cx,
                )
                .aria_label(self.locale.action_text(A::ClosePanel))
                .track_focus(
                    &self
                        .detail_focus
                        .clone()
                        .tab_stop(self.action_enabled(Action::CloseDetails)),
                ),
            );
        let mut content = div()
            .w_full()
            .min_w_0()
            .p(px(16.0))
            .flex()
            .flex_col()
            .gap(px(16.0));
        if agent.config.is_some() {
            if Self::config_problem(agent)
                || agent
                    .config
                    .as_ref()
                    .is_some_and(|config| config.state == ConfigFileState::Missing)
            {
                content = content.child(self.config_notice(agent, palette));
            }
            if agent.id == "codex" {
                content = content.child(self.connection_controls(palette, cx));
            }
        } else {
            content = content.child(self.caption(Text::OtherAgentHint, palette));
        }
        content = content.child(
            self.button(
                "diagnostics",
                self.locale.action_text(A::Diagnostics),
                None,
                Action::ToggleDiagnostics,
                palette,
                cx,
            )
            .w_full()
            .h_auto()
            .min_h(px(36.0))
            .py(px(6.0))
            .whitespace_normal(),
        );
        if self.diagnostics {
            content = content.child(self.action_caption(A::CurrentConfig, palette));
            content = content.child(self.model_values(agent, palette));
            if agent.id == "codex" {
                content = content.child(self.action_caption(A::OwnershipHint, palette));
            }
            if let Some(ownership) = self.actions.ownership.filter(|_| agent.id == "codex") {
                content = content.child(
                    div()
                        .text_size(px(11.0))
                        .text_color(rgb(palette.muted))
                        .child(text!(format!(
                            "{} · CLI {}",
                            self.locale.action_text(A::Ownership),
                            ownership.as_str()
                        ))),
                );
            }
            content = content.child(div().h(px(1.0)).flex_none().bg(rgb(palette.border)));
            // The path and its real action precede lower-priority executable metadata.
            if let Some(path) = agent
                .config
                .as_ref()
                .and_then(|config| config.path.as_ref())
            {
                let copied = self.copied == Some(agent.id);
                let copy = self
                    .button(
                        "copy-config-path",
                        self.locale
                            .text(if copied { Text::Copied } else { Text::CopyPath }),
                        None,
                        Action::CopyPath(agent.id),
                        palette,
                        cx,
                    )
                    .aria_label(format!(
                        "{} · {}",
                        self.locale
                            .text(if copied { Text::Copied } else { Text::CopyPath }),
                        agent.name
                    ));
                let value = div()
                    .w_full()
                    .min_w_0()
                    .text_size(px(13.0))
                    .line_height(px(20.0))
                    .whitespace_normal()
                    .child(text!(path.display().to_string()));
                let field = div().w_full().min_w_0().flex().flex_col().gap(px(4.0));
                let field = if self.compact {
                    field
                        .child(self.caption(Text::ConfigPath, palette))
                        .child(value)
                        .child(div().flex().child(copy))
                } else {
                    field
                        .child(
                            div()
                                .flex()
                                .items_center()
                                .justify_between()
                                .flex_wrap()
                                .gap(px(8.0))
                                .child(self.caption(Text::ConfigPath, palette))
                                .child(copy),
                        )
                        .child(value)
                };
                content = content.child(field.when(copied, |field| {
                    field.child(
                        div()
                            .id("copy-status")
                            .role(Role::Status)
                            .text_size(px(12.0))
                            .text_color(rgb(palette.accent))
                            .child(text!(self.locale.text(Text::Copied))),
                    )
                }));
            }
            content = content.child(self.field(
                Text::Executable,
                agent.executable.display().to_string(),
                palette,
                false,
            ));
        }
        div()
            .w_full()
            .min_w_0()
            .h(px(self.editor_height))
            .flex()
            .flex_col()
            .border_t_1()
            .border_color(rgb(palette.border))
            .bg(rgb(palette.surface))
            .child(header)
            .child(
                div()
                    .id("inspector-scroll")
                    .flex_1()
                    .min_h_0()
                    .min_w_0()
                    .overflow_y_scroll()
                    .track_scroll(&self.detail_scroll)
                    .child(content),
            )
    }

    fn loading(&self, palette: Palette) -> Div {
        let mut placeholder = self
            .panel(palette)
            .h_full()
            .p(px(20.0))
            .gap(px(20.0))
            .child(
                div()
                    .id("initial-scan")
                    .role(Role::Status)
                    .text_size(px(13.0))
                    .text_color(rgb(palette.muted))
                    .child(text!(self.locale.text(Text::Refreshing))),
            );
        for _ in 0..3 {
            placeholder = placeholder.child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(12.0))
                    .child(
                        div()
                            .size(px(28.0))
                            .flex_none()
                            .rounded(px(6.0))
                            .bg(rgb(palette.inset)),
                    )
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .gap(px(8.0))
                            .child(
                                div()
                                    .w(px(120.0))
                                    .h(px(10.0))
                                    .rounded(px(4.0))
                                    .bg(rgb(palette.inset)),
                            )
                            .child(
                                div()
                                    .w(px(180.0))
                                    .h(px(8.0))
                                    .rounded(px(4.0))
                                    .bg(rgb(palette.inset)),
                            ),
                    ),
            );
        }
        placeholder
    }

    fn empty(&self, palette: Palette, cx: &mut Context<Self>) -> Div {
        self.panel(palette).h_full().child(
            div()
                .id("empty-scroll")
                .flex_1()
                .min_h_0()
                .overflow_y_scroll()
                .track_scroll(&self.roster_scroll)
                .child(
                    div()
                        .w_full()
                        .min_w_0()
                        .min_h_full()
                        .p(px(24.0))
                        .flex()
                        .flex_col()
                        .justify_center()
                        .items_center()
                        .text_center()
                        .gap(px(12.0))
                        .child(
                            svg()
                                .data(assets::TERMINAL)
                                .size(px(32.0))
                                .text_color(rgb(palette.muted)),
                        )
                        .child(
                            div()
                                .w_full()
                                .text_size(px(18.0))
                                .line_height(px(26.0))
                                .font_weight(FontWeight::MEDIUM)
                                .whitespace_normal()
                                .child(text!(self.locale.text(Text::EmptyTitle))),
                        )
                        .child(self.caption(Text::EmptyHint, palette).max_w(px(420.0)))
                        .child(
                            self.caption(Text::SupportedAgents, palette)
                                .max_w(px(420.0)),
                        )
                        .child(div().flex().child(self.button(
                            "empty-refresh",
                            self.locale.text(Text::Refresh),
                            Some(assets::REFRESH),
                            Action::Refresh,
                            palette,
                            cx,
                        ))),
                ),
        )
    }

    fn agents_page(&self, palette: Palette, cx: &mut Context<Self>) -> Div {
        let Some(snapshot) = &self.snapshot else {
            return self.loading(palette);
        };
        let content = if snapshot.agents.is_empty() {
            self.empty(palette, cx)
        } else {
            self.roster(snapshot, palette, cx)
        };
        if self.agent("codex").is_none()
            && (self.actions.reconcile_needed() || self.actions.uncertain)
        {
            return div()
                .size_full()
                .min_h_0()
                .flex()
                .flex_col()
                .gap(px(12.0))
                .child(
                    self.action_message(
                        "missing-codex-reconcile",
                        self.locale.action_text(A::MissingCodexReview),
                        true,
                        palette,
                    )
                    .role(Role::Alert)
                    .flex_none(),
                )
                .child(content.flex_1().min_h_0());
        }
        content
    }

    fn settings_page(&self, palette: Palette, cx: &mut Context<Self>) -> Div {
        let appearance = div().flex().flex_wrap().gap(px(8.0)).children(
            [
                ("theme-system", Appearance::System, Text::System),
                ("theme-light", Appearance::Light, Text::Light),
                ("theme-dark", Appearance::Dark, Text::Dark),
            ]
            .map(|(id, appearance, key)| {
                self.button(
                    id,
                    self.locale.text(key),
                    None,
                    Action::Appearance(appearance),
                    palette,
                    cx,
                )
            }),
        );
        let languages = div()
            .flex()
            .flex_wrap()
            .gap(px(8.0))
            .children(Locale::ALL.map(|locale| {
                self.button(
                    locale.id(),
                    locale.name(),
                    None,
                    Action::Language(locale),
                    palette,
                    cx,
                )
            }));
        self.panel(palette).h_full().child(
            div()
                .id("settings-scroll")
                .flex_1()
                .min_h_0()
                .overflow_y_scroll()
                .track_scroll(&self.settings_scroll)
                .child(
                    div()
                        .w_full()
                        .min_w_0()
                        .p(px(if self.compact { 16.0 } else { 24.0 }))
                        .flex()
                        .flex_col()
                        .gap(px(24.0))
                        .child(
                            div()
                                .flex()
                                .flex_col()
                                .gap(px(12.0))
                                .child(
                                    div()
                                        .font_weight(FontWeight::SEMIBOLD)
                                        .child(text!(self.locale.text(Text::Appearance))),
                                )
                                .child(appearance),
                        )
                        .child(div().h(px(1.0)).bg(rgb(palette.border)))
                        .child(
                            div()
                                .flex()
                                .flex_col()
                                .gap(px(12.0))
                                .child(
                                    div()
                                        .font_weight(FontWeight::SEMIBOLD)
                                        .child(text!(self.locale.text(Text::Language))),
                                )
                                .child(languages),
                        )
                        .child(
                            div()
                                .id("preferences-status")
                                .role(Role::Status)
                                .text_size(px(12.0))
                                .text_color(rgb(if self.preferences_error.is_some() {
                                    palette.warning
                                } else {
                                    palette.muted
                                }))
                                .child(text!(self.locale.provider_text(
                                    if self.preferences_task.is_some() {
                                        P::PreferencesSaving
                                    } else if self.preferences_error.is_some() {
                                        P::PreferenceError
                                    } else if self.preferences_snapshot.is_some() {
                                        P::SavedPreferences
                                    } else {
                                        P::AutoSavePreferences
                                    }
                                ))),
                        )
                        .when_some(self.preferences_error, |column, error| {
                            column.child(
                                div()
                                    .text_size(px(11.0))
                                    .text_color(rgb(palette.muted))
                                    .child(text!(error)),
                            )
                        }),
                ),
        )
    }
}

impl Render for MagpieWindow {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if !self.close_handler_installed {
            self.close_handler_installed = true;
            let weak = cx.entity().downgrade();
            window.on_window_should_close(cx, move |_, cx| {
                weak.update(cx, |view, cx| {
                    if view.actions.busy().is_some()
                        || view.providers.read(cx).busy()
                        || view.preferences_task.is_some()
                        || view.gateway.read(cx).pending()
                    {
                        view.close_requested = true;
                        view.actions.cancel();
                        view.providers.read(cx).cancel();
                        view.gateway.update(cx, |page, cx| page.begin_closing(cx));
                        cx.notify();
                        false
                    } else {
                        true
                    }
                })
                .unwrap_or(true)
            });
        }
        if self.close_requested
            && self.actions.busy().is_none()
            && !self.providers.read(cx).busy()
            && self.preferences_task.is_none()
            && !self.gateway.read(cx).pending()
        {
            window.remove_window();
        }
        if self.actions.busy().is_none() {
            if let Some(address) = self.gateway_address_to_actions.take()
                && !self.actions.updating_connection()
            {
                self.actions.set_address(address.to_string());
                self.gateway_input.update(cx, |input, cx| {
                    input.set_text(&address.to_string(), cx);
                });
            }
            if self.gateway_catalog_invalid {
                self.gateway_catalog_invalid = false;
                self.actions.invalidate_catalog();
            }
        }
        if let Some(action) = self.pending_action.take() {
            self.act(action, window, cx);
        }
        if let Some(handle) = self
            .pending_focus
            .take()
            .and_then(|id| self.control_focus.get(id))
            .cloned()
        {
            let weak = cx.entity().downgrade();
            window.on_next_frame(move |window, cx| {
                handle.focus(window, cx);
                let _ = weak.update(cx, |view, cx| view.reveal_focused(window, cx));
            });
        }
        self.compact = window.viewport_size().width < px(BREAKPOINT);
        self.editor_height = (f32::from(window.viewport_size().height) - 210.0).clamp(150.0, 400.0);
        if self.page == Page::Agents
            && self
                .agent_focus
                .iter()
                .any(|(id, handle)| handle.is_focused(window) && self.agent(id).is_none())
        {
            self.restore_root_focus = true;
        }
        if self.page == Page::Agents
            && self.agent("codex").is_none()
            && [
                "codex-model",
                "codex-connect",
                "codex-disconnect",
                "cancel-operation",
            ]
            .iter()
            .any(|id| {
                self.control_focus
                    .get(id)
                    .is_some_and(|handle| handle.is_focused(window))
            })
        {
            self.restore_root_focus = true;
        }
        if self.page == Page::Agents
            && self
                .snapshot
                .as_ref()
                .is_some_and(|snapshot| !snapshot.agents.is_empty())
            && self
                .control_focus
                .get("empty-refresh")
                .is_some_and(|handle| handle.is_focused(window))
        {
            self.restore_root_focus = true;
        }
        if self.restore_root_focus {
            self.restore_root_focus = false;
            Self::focus_next_frame(self.focus.clone(), window);
        }
        let palette = Palette::for_window(window, self.appearance);
        let disabled = self.actions.busy().is_some() || self.refresh_task.is_some();
        let locale = self.locale;
        self.gateway_input.update(cx, |input, cx| {
            if input.text() != self.actions.address {
                input.set_text(&self.actions.address, cx);
            }
            input.set_presentation(
                "127.0.0.1:3425",
                locale.action_text(A::Gateway),
                palette,
                disabled || self.actions.managed_address_verified(),
                cx,
            );
        });
        self.search_input.update(cx, |input, cx| {
            if input.text() != self.actions.search {
                input.set_text(&self.actions.search, cx);
            }
            input.set_presentation(
                locale.action_text(A::SearchPlaceholder),
                locale.action_text(A::Search),
                palette,
                disabled,
                cx,
            );
        });
        let gutter = if self.compact { 16.0 } else { 24.0 };
        let wide_navigation = window.viewport_size().width >= px(1080.0);
        let small_toolbar = window.viewport_size().width < px(620.0);
        if small_toolbar
            && !disabled
            && !self.roadmap
            && self
                .control_focus
                .get("refresh")
                .is_some_and(|handle| handle.is_focused(window))
        {
            Self::focus_next_frame(self.control_focus["more-pages"].clone(), window);
        }
        self.providers.update(cx, |page, cx| {
            page.set_presentation(self.locale, self.appearance, cx)
        });
        self.gateway.update(cx, |page, cx| {
            if self.gateway_configured {
                page.configure(self.gateway_address, false, cx);
            }
            page.set_presentation(
                self.locale,
                self.appearance,
                self.preferences_ready && self.preferences_task.is_none() && !self.close_requested,
                cx,
            );
        });
        let body = match self.page {
            Page::Agents => self.agents_page(palette, cx),
            Page::Providers => div().size_full().child(self.providers.clone()),
            Page::Gateway => div().size_full().child(self.gateway.clone()),
            Page::Settings => self.settings_page(palette, cx),
        };
        let scan_status = if self.refresh_task.is_some() {
            self.locale.text(Text::Refreshing).to_owned()
        } else {
            self.locale.scan_summary(
                self.snapshot
                    .as_ref()
                    .map_or(0, |snapshot| snapshot.agents.len()),
            )
        };
        let future_pages = [
            A::RoutesPage,
            A::UsagePage,
            A::SessionsPage,
            A::LibraryPage,
            A::PluginsPage,
        ];
        let navigation = div()
            .flex()
            .items_center()
            .gap(px(2.0))
            .min_w_0()
            .when(wide_navigation, |nav| {
                nav.flex_col().items_stretch().w_full().gap(px(6.0)).child(
                    div()
                        .px(px(10.0))
                        .pt(px(8.0))
                        .pb(px(4.0))
                        .text_size(px(10.0))
                        .text_color(rgb(palette.muted))
                        .child(text!(self.locale.provider_text(P::Work))),
                )
            })
            .child(
                self.button(
                    "agents-page",
                    self.locale.text(Text::Agents),
                    None,
                    Action::Page(Page::Agents),
                    palette,
                    cx,
                )
                .px(px(if self.compact { 6.0 } else { 10.0 }))
                .when(wide_navigation, |button| button.justify_start())
                .bg(rgb(if self.page == Page::Agents {
                    palette.selected
                } else {
                    palette.background
                }))
                .text_color(rgb(if self.page == Page::Agents {
                    palette.accent
                } else {
                    palette.muted
                }))
                .border_color(gpui::rgba(0)),
            )
            .when(wide_navigation, |nav| {
                nav.child(
                    div()
                        .px(px(10.0))
                        .pt(px(18.0))
                        .pb(px(4.0))
                        .text_size(px(10.0))
                        .text_color(rgb(palette.muted))
                        .child(text!(self.locale.provider_text(P::ModelsGroup))),
                )
            })
            .child(
                self.button(
                    "providers-page",
                    self.locale.action_text(A::ProvidersPage),
                    None,
                    Action::Page(Page::Providers),
                    palette,
                    cx,
                )
                .px(px(if self.compact { 6.0 } else { 10.0 }))
                .when(wide_navigation, |button| button.justify_start())
                .bg(rgb(if self.page == Page::Providers {
                    palette.selected
                } else {
                    palette.background
                })),
            )
            .child(
                self.button(
                    "gateway-page",
                    self.locale.action_text(A::GatewayPage),
                    None,
                    Action::Page(Page::Gateway),
                    palette,
                    cx,
                )
                .px(px(if self.compact { 6.0 } else { 10.0 }))
                .when(wide_navigation, |button| button.justify_start())
                .bg(rgb(if self.page == Page::Gateway {
                    palette.selected
                } else {
                    palette.background
                })),
            )
            .child(
                self.button(
                    "more-pages",
                    self.locale.action_text(A::More),
                    Some(DOWN),
                    Action::ToggleRoadmap,
                    palette,
                    cx,
                )
                .px(px(if self.compact { 6.0 } else { 10.0 }))
                .when(wide_navigation, |button| button.justify_start())
                .aria_expanded(self.roadmap)
                .aria_label(format!(
                    "{} · {}",
                    self.locale.action_text(A::More),
                    self.locale.action_text(A::Roadmap)
                ))
                .bg(rgb(palette.background)),
            );
        let roadmap_panel = div()
            .id("roadmap-panel")
            .flex_none()
            .max_h(px(180.0))
            .overflow_y_scroll()
            .px(px(gutter))
            .py(px(12.0))
            .border_b_1()
            .border_color(rgb(palette.border))
            .bg(rgb(palette.surface))
            .when(small_toolbar, |panel| {
                panel.child(self.button(
                    "refresh",
                    self.locale.text(Text::Refresh),
                    Some(assets::REFRESH),
                    Action::Refresh,
                    palette,
                    cx,
                ))
            })
            .child(
                div()
                    .flex()
                    .flex_wrap()
                    .gap(px(12.0))
                    .children(future_pages.map(|key| {
                        div()
                            .id(SharedString::from(format!("future-panel:{}", key as usize)))
                            .role(Role::Status)
                            .text_size(px(12.0))
                            .text_color(rgb(palette.muted))
                            .child(text!(format!(
                                "{} · {}",
                                self.locale.action_text(key),
                                self.locale.action_text(A::Planned)
                            )))
                    })),
            )
            .child(self.action_caption(A::RoadmapHint, palette).mt(px(10.0)));
        let window_actions = div()
            .flex()
            .items_center()
            .gap(px(2.0))
            .flex_none()
            .when(!small_toolbar, |row| {
                row.child(
                    self.button(
                        "refresh",
                        "",
                        Some(assets::REFRESH),
                        Action::Refresh,
                        palette,
                        cx,
                    )
                    .bg(rgb(palette.background))
                    .aria_label(self.locale.text(
                        if self.refresh_task.is_some() {
                            Text::Refreshing
                        } else {
                            Text::Refresh
                        },
                    )),
                )
            })
            .when(!wide_navigation, |actions| {
                actions.child(
                    self.button(
                        "settings-page",
                        "",
                        Some(assets::SETTINGS),
                        Action::Page(Page::Settings),
                        palette,
                        cx,
                    )
                    .px(px(8.0))
                    .bg(rgb(palette.background))
                    .aria_label(self.locale.text(Text::Settings)),
                )
            });
        let heading = div()
            .flex_none()
            .min_w_0()
            .flex()
            .items_center()
            .gap(px(8.0))
            .min_h(px(24.0))
            .child(match self.page {
                Page::Agents => self.action_caption(A::WorkspaceIntro, palette),
                Page::Providers => div()
                    .text_size(px(12.0))
                    .text_color(rgb(palette.muted))
                    .child(text!(self.locale.provider_text(P::Intro))),
                Page::Settings => self.caption(Text::Preferences, palette),
                Page::Gateway => div()
                    .text_size(px(12.0))
                    .text_color(rgb(palette.muted))
                    .child(text!(self.locale.gateway_text(G::Intro))),
            });
        let brand = div()
            .flex()
            .items_center()
            .gap(px(8.0))
            .flex_none()
            .child(
                svg()
                    .path("icons/magpie.svg")
                    .size(px(24.0))
                    .text_color(rgb(palette.text)),
            )
            .child(
                div()
                    .text_size(px(18.0))
                    .font_weight(FontWeight::SEMIBOLD)
                    .child(text!("magpie")),
            );
        let main_header;
        let sidebar;
        if wide_navigation {
            sidebar = Some(
                div()
                    .w(px(172.0))
                    .h_full()
                    .flex_none()
                    .p(px(12.0))
                    .flex()
                    .flex_col()
                    .gap(px(16.0))
                    .border_r_1()
                    .border_color(rgb(palette.border))
                    .child(brand)
                    .child(navigation)
                    .child(div().flex_1())
                    .child(
                        self.button(
                            "settings-page",
                            self.locale.text(Text::Settings),
                            Some(assets::SETTINGS),
                            Action::Page(Page::Settings),
                            palette,
                            cx,
                        )
                        .justify_start(),
                    ),
            );
            main_header = div()
                .min_h(px(56.0))
                .flex_none()
                .px(px(gutter))
                .flex()
                .items_center()
                .justify_between()
                .child(
                    div()
                        .text_size(px(16.0))
                        .font_weight(FontWeight::SEMIBOLD)
                        .child(text!(match self.page {
                            Page::Agents => self.locale.text(Text::Agents),
                            Page::Providers => self.locale.action_text(A::ProvidersPage),
                            Page::Gateway => self.locale.action_text(A::GatewayPage),
                            Page::Settings => self.locale.text(Text::Settings),
                        })),
                )
                .child(window_actions);
        } else {
            sidebar = None;
            main_header = div()
                .flex_none()
                .px(px(gutter))
                .py(px(10.0))
                .flex()
                .flex_col()
                .gap(px(8.0))
                .border_b_1()
                .border_color(rgb(palette.border))
                .child(
                    div()
                        .flex()
                        .items_center()
                        .justify_between()
                        .child(brand)
                        .child(window_actions),
                )
                .child(navigation);
        }
        let content = div()
            .id("magpie")
            .size_full()
            .flex()
            .flex_col()
            .font(theme::ui_font(cx, self.locale))
            .text_size(px(13.0))
            .line_height(px(20.0))
            .text_color(rgb(palette.text))
            .bg(rgb(palette.background))
            .child(main_header)
            .when(self.roadmap, |root| root.child(roadmap_panel))
            .child(
                div()
                    .flex_1()
                    .min_h_0()
                    .min_w_0()
                    .px(px(gutter))
                    .py(px(16.0))
                    .flex()
                    .flex_col()
                    .gap(px(12.0))
                    .child(heading)
                    .child(div().flex_1().min_h_0().min_w_0().child(body)),
            )
            .child(
                div()
                    .min_h(px(28.0))
                    .flex_none()
                    .px(px(gutter))
                    .py(px(4.0))
                    .flex()
                    .items_center()
                    .justify_between()
                    .gap(px(8.0))
                    .border_t_1()
                    .border_color(rgb(palette.border))
                    .child(
                        div()
                            .id("scan-status")
                            .role(Role::Status)
                            .flex_1()
                            .min_w_0()
                            .text_size(px(11.0))
                            .line_height(px(16.0))
                            .text_color(rgb(palette.muted))
                            .whitespace_normal()
                            .child(text!(scan_status)),
                    )
                    .child(
                        div()
                            .flex_none()
                            .text_size(px(11.0))
                            .line_height(px(16.0))
                            .text_color(rgb(palette.muted))
                            .child(text!(format!(
                                "{} · v{}",
                                self.locale.action_text(A::LocalConfig),
                                env!("CARGO_PKG_VERSION")
                            ))),
                    ),
            );
        div()
            .id("magpie-shell")
            .track_focus(&self.focus)
            .tab_group()
            .tab_stop(false)
            .on_key_down(cx.listener(Self::key_down))
            .size_full()
            .flex()
            .bg(rgb(palette.background))
            .font(theme::ui_font(cx, self.locale))
            .text_size(px(13.0))
            .text_color(rgb(palette.text))
            .when_some(sidebar, |shell, sidebar| shell.child(sidebar))
            .child(div().flex_1().min_w_0().h_full().child(content))
    }
}

impl MagpieWindow {
    fn action_enabled(&self, action: Action) -> bool {
        if matches!(action, Action::Language(_) | Action::Appearance(_))
            && (!self.preferences_ready || self.preferences_task.is_some())
        {
            return false;
        }
        if self.close_requested || (self.refresh_task.is_some() && self.check_after_refresh) {
            return false;
        }
        if matches!(action, Action::Escape) {
            return true;
        }
        if self.actions.busy().is_some() {
            return matches!(action, Action::Cancel);
        }
        if matches!(action, Action::Cancel) {
            return false;
        }
        if self.refresh_task.is_some()
            && matches!(
                action,
                Action::Refresh
                    | Action::CheckConnection
                    | Action::Run(_)
                    | Action::ConfirmDisconnect
                    | Action::OpenDisconnect
                    | Action::OpenModels
                    | Action::OpenConnection
            )
        {
            return false;
        }
        match action {
            Action::Run(JobKind::Preview) => {
                self.actions.can_preview() && !self.actions.disconnect_confirmation
            }
            Action::Run(JobKind::Apply) => {
                self.actions.can_apply() && !self.actions.disconnect_confirmation
            }
            Action::Run(JobKind::Disconnect) => {
                self.actions.can_disconnect() && self.actions.disconnect_confirmation
            }
            Action::Run(JobKind::Status) | Action::CheckConnection => {
                self.actions.config_path().is_some()
            }
            Action::ConfirmDisconnect | Action::OpenDisconnect => self.actions.can_disconnect(),
            Action::OpenModels | Action::OpenConnection => self.agent("codex").is_some(),
            _ => true,
        }
    }

    fn check_connection(&mut self, cx: &mut Context<Self>) {
        self.actions.invalidate_preview();
        self.actions.ownership = None;
        self.check_after_refresh = true;
        self.refresh(cx);
    }

    fn start_job(&mut self, kind: JobKind, cx: &mut Context<Self>) {
        if self.refresh_task.is_some() || self.actions.busy().is_some() {
            return;
        }
        let config = self
            .actions
            .config_path()
            .map(PathBuf::from)
            .unwrap_or_default();
        let Ok(job) = self.actions.begin(kind, config) else {
            cx.notify();
            return;
        };
        self.pending_focus = Some("cancel-operation");
        self.shutdown_pending.store(true, Ordering::Release);
        let pending = self.shutdown_pending.clone();
        // Detached weak waiter survives normal window teardown; the close gate
        // cancels first and waits for the admitted job's actual cleanup.
        let work = cx.background_executor().spawn(async move {
            let result = job.execute().await;
            pending.store(false, Ordering::Release);
            result
        });
        cx.spawn(async move |this, cx| {
            let result = work.await;
            let kind = result.kind;
            let _ = this.update(cx, |view, cx| {
                view.actions.complete(result);
                view.gateway_input.update(cx, |input, cx| {
                    input.set_text(&view.actions.address, cx);
                });
                if let Some(catalog) = &view.actions.catalog {
                    let ids: BTreeSet<_> = catalog
                        .models
                        .iter()
                        .map(|model| model.id.as_str())
                        .collect();
                    view.model_focus.retain(|id, _| ids.contains(id.as_str()));
                    for model in &catalog.models {
                        view.model_focus
                            .entry(model.id.clone())
                            .or_insert_with(|| cx.focus_handle().tab_index(0).tab_stop(true));
                    }
                }
                view.pending_focus = Some(if view.details != Some("codex") {
                    "codex-model"
                } else if view.actions.preview.is_some() {
                    "apply-connect"
                } else if kind == JobKind::Inspect && view.actions.catalog_is_current() {
                    "model-search"
                } else {
                    "check-connection"
                });
                // Never skip reconciliation merely because the callback was
                // stale or the mutation failed before a successful reply.
                if kind.is_mutation() && !view.close_requested {
                    view.check_after_refresh = true;
                    view.refresh(cx);
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    fn action_caption(&self, key: A, palette: Palette) -> Div {
        div()
            .w_full()
            .min_w_0()
            .text_size(px(12.0))
            .line_height(px(18.0))
            .text_color(rgb(palette.muted))
            .whitespace_normal()
            .child(text!(self.locale.action_text(key)))
    }

    fn action_message(
        &self,
        id: &'static str,
        message: impl Into<SharedString>,
        warning: bool,
        palette: Palette,
    ) -> Stateful<Div> {
        div()
            .id(id)
            .role(Role::Status)
            .w_full()
            .min_w_0()
            .p(px(10.0))
            .rounded(px(6.0))
            .bg(rgb(if warning {
                palette.warning_bg
            } else {
                palette.inset
            }))
            .text_color(rgb(if warning {
                palette.warning
            } else {
                palette.muted
            }))
            .text_size(px(12.0))
            .line_height(px(18.0))
            .whitespace_normal()
            .child(text!(message.into()))
    }

    fn action_button(
        &self,
        id: &'static str,
        label: A,
        action: Action,
        palette: Palette,
        cx: &mut Context<Self>,
    ) -> Stateful<Div> {
        let enabled = self.action_enabled(action);
        self.button(
            id,
            self.locale.action_text(label),
            None,
            action,
            palette,
            cx,
        )
        .h_auto()
        .min_h(px(if self.compact { 40.0 } else { 34.0 }))
        .py(px(6.0))
        .whitespace_normal()
        .when(
            enabled && matches!(action, Action::Run(JobKind::Apply | JobKind::Preview)),
            |button| {
                button
                    .bg(rgb(palette.text))
                    .text_color(rgb(palette.surface))
            },
        )
    }

    fn select_model(&mut self, id: String, cx: &mut Context<Self>) {
        if self.refresh_task.is_none() && self.actions.select_model(id) {
            self.preview_fields = false;
            cx.notify();
        }
    }

    fn model_option(&self, model: &str, palette: Palette, cx: &mut Context<Self>) -> Stateful<Div> {
        let selected = self.actions.selected.as_deref() == Some(model);
        let enabled = self.actions.busy().is_none()
            && self.refresh_task.is_none()
            && self.actions.catalog_is_current();
        let click_id = model.to_owned();
        let a11y_id = click_id.clone();
        let keyboard_id = click_id.clone();
        let weak = cx.entity().downgrade();
        div()
            .id(SharedString::from(format!("model-option:{model}")))
            .role(if enabled { Role::Button } else { Role::Status })
            .aria_label(format!(
                "{}{}",
                model,
                if selected {
                    format!(" · {}", self.locale.text(Text::Selected))
                } else {
                    String::new()
                }
            ))
            .aria_toggled(if selected {
                Toggled::True
            } else {
                Toggled::False
            })
            .tab_index(0)
            .tab_stop(enabled)
            .when_some(self.model_focus.get(model), |row, focus| {
                row.track_focus(&focus.clone().tab_stop(enabled))
            })
            .h(px(38.0))
            .w_full()
            .min_w_0()
            .flex_none()
            .px(px(10.0))
            .py(px(7.0))
            .border_2()
            .border_color(gpui::rgba(0))
            .rounded(px(4.0))
            .bg(rgb(if selected {
                palette.selected
            } else {
                palette.surface
            }))
            .text_color(rgb(if selected {
                palette.accent
            } else {
                palette.text
            }))
            .text_size(px(13.0))
            .line_height(px(18.0))
            .truncate()
            .when(!enabled, |row| row.opacity(0.6))
            .when(enabled, |row| {
                row.cursor_pointer()
                    .focus_visible(move |style| style.border_color(rgb(palette.accent)))
                    .hover(move |style| style.bg(rgb(palette.inset)))
                    .on_key_down(cx.listener(move |view, event: &KeyDownEvent, window, cx| {
                        view.model_key(&keyboard_id, event, window, cx)
                    }))
                    .on_click(
                        cx.listener(move |view, _, _, cx| view.select_model(click_id.clone(), cx)),
                    )
                    .on_a11y_action(AccessibleAction::Click, move |_, _, cx| {
                        let _ = weak.update(cx, |view, cx| view.select_model(a11y_id.clone(), cx));
                    })
            })
            .flex()
            .items_start()
            .gap(px(8.0))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .child(text!(model.to_owned())),
            )
            .when(selected, |row| {
                row.child(
                    svg()
                        .data(CHECK)
                        .size(px(16.0))
                        .flex_none()
                        .text_color(rgb(palette.accent)),
                )
            })
    }

    fn model_key(
        &mut self,
        model: &str,
        event: &KeyDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let models = self.actions.visible_models();
        let Some(index) = models.iter().position(|item| item.id == model) else {
            return;
        };
        let next = match event.keystroke.key.as_str() {
            "down" => (index + 1).min(models.len() - 1),
            "up" => index.saturating_sub(1),
            "home" => 0,
            "end" => models.len() - 1,
            _ => return,
        };
        if let Some(focus) = self.model_focus.get(&models[next].id).cloned() {
            self.model_scroll
                .scroll_to_item(next, ScrollStrategy::Nearest);
            Self::focus_next_frame(focus, window);
            cx.notify();
        }
        cx.stop_propagation();
    }

    fn review_field(&self, label: &'static str, value: String, palette: Palette) -> Div {
        div()
            .w_full()
            .min_w_0()
            .flex()
            .gap(px(12.0))
            .when(self.compact, |row| row.flex_col().gap(px(2.0)))
            .child(
                div()
                    .w(px(108.0))
                    .flex_none()
                    .text_size(px(12.0))
                    .line_height(px(18.0))
                    .text_color(rgb(palette.muted))
                    .whitespace_normal()
                    .child(text!(label)),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .text_size(px(13.0))
                    .line_height(px(18.0))
                    .whitespace_normal()
                    .child(text!(value)),
            )
    }

    fn preview_panel(&self, palette: Palette, cx: &mut Context<Self>) -> Option<Div> {
        let ticket = self.actions.preview.as_ref()?;
        let update = ticket.binding.ownership == Ownership::Connected;
        let mut preview = div()
            .w_full()
            .min_w_0()
            .p(px(if update { 8.0 } else { 12.0 }))
            .rounded(px(6.0))
            .border_1()
            .border_color(rgb(palette.border))
            .bg(rgb(palette.inset))
            .flex()
            .flex_col()
            .gap(px(if update { 5.0 } else { 8.0 }))
            .child(self.action_caption(
                if update {
                    A::UpdateHint
                } else {
                    A::PreviewHint
                },
                palette,
            ))
            .child(self.review_field(
                self.locale.text(Text::Model),
                ticket.binding.model.clone(),
                palette,
            ));
        if update {
            preview = preview.child(
                div()
                    .text_size(px(12.0))
                    .line_height(px(18.0))
                    .text_color(rgb(palette.muted))
                    .child(text!(format!(
                        "{} · {}",
                        self.locale.action_text(A::Gateway),
                        ticket.binding.address
                    ))),
            );
        } else {
            preview = preview
                .child(self.review_field(
                    self.locale.action_text(A::Gateway),
                    ticket.binding.address.to_string(),
                    palette,
                ))
                .child(self.review_field(
                    self.locale.text(Text::ConfigPath),
                    ticket.binding.config.display().to_string(),
                    palette,
                ))
                .child(self.action_caption(
                    if ticket.info.config_existed {
                        A::KeepConfig
                    } else {
                        A::CreateConfig
                    },
                    palette,
                ))
                .child(self.action_caption(A::Preserve, palette));
        }
        preview = preview.child(self.action_button(
            "preview-fields",
            if update { A::UpdateDetails } else { A::Fields },
            Action::ToggleFields,
            palette,
            cx,
        ));
        if self.preview_fields {
            if update {
                preview = preview.child(self.review_field(
                    self.locale.text(Text::ConfigPath),
                    ticket.binding.config.display().to_string(),
                    palette,
                ));
            }
            for field in &ticket.info.changed_fields {
                preview = preview.child(
                    div()
                        .w_full()
                        .min_w_0()
                        .text_size(px(11.0))
                        .line_height(px(16.0))
                        .whitespace_normal()
                        .child(text!(field.clone())),
                );
            }
        }
        preview = preview.child(
            div()
                .flex()
                .flex_wrap()
                .gap(px(8.0))
                .child(self.action_button(
                    "apply-connect",
                    if update { A::SaveModel } else { A::Save },
                    Action::Run(JobKind::Apply),
                    palette,
                    cx,
                ))
                .child(self.action_button(
                    "edit-preview",
                    A::Edit,
                    Action::DismissPreview,
                    palette,
                    cx,
                )),
        );
        Some(preview)
    }

    fn model_picker(&self, palette: Palette, cx: &mut Context<Self>) -> Div {
        let mut choices = div().flex_1().min_w_0().flex().flex_col().gap(px(8.0));
        if let Some(catalog) = &self.actions.catalog {
            if !self.actions.catalog_is_current() {
                choices = choices.child(self.action_message(
                    "stale-catalog",
                    format!(
                        "{} · {}",
                        self.locale.action_text(A::StaleCatalog),
                        catalog.address
                    ),
                    true,
                    palette,
                ));
            }
            choices = choices.child(
                div()
                    .id("search-field")
                    .w_full()
                    .min_w_0()
                    .anchor_scroll(self.scroll_anchors.get("model-search").cloned())
                    .child(self.search_input.clone()),
            );
            let models = self.actions.visible_models();
            if models.is_empty() {
                choices = choices.child(
                    self.action_caption(
                        if catalog.models.is_empty() {
                            A::NoModels
                        } else {
                            A::NoMatches
                        },
                        palette,
                    )
                    .py(px(12.0)),
                );
            } else {
                let rows = uniform_list(
                    "configured-models",
                    models.len(),
                    cx.processor(move |view, range: std::ops::Range<usize>, _, cx| {
                        let models = view.actions.visible_models();
                        range
                            .filter_map(|index| models.get(index))
                            .map(|model| view.model_option(&model.id, palette, cx))
                            .collect::<Vec<_>>()
                    }),
                )
                .w_full()
                .min_w_0()
                .h(px((models.len().min(4) as f32) * 38.0))
                .track_scroll(&self.model_scroll);
                choices = choices.child(
                    div()
                        .id("models-field")
                        .w_full()
                        .min_w_0()
                        .anchor_scroll(Some(self.model_anchor.clone()))
                        .border_1()
                        .border_color(rgb(palette.border))
                        .rounded(px(6.0))
                        .overflow_hidden()
                        .child(rows),
                );
            }
            choices = choices.child(
                div()
                    .text_size(px(11.0))
                    .line_height(px(16.0))
                    .text_color(rgb(palette.muted))
                    .child(text!(format!(
                        "{} · {}",
                        self.locale.action_text(A::Configured),
                        catalog.models.len()
                    ))),
            );
        } else {
            choices = choices.child(self.action_caption(A::NoCatalog, palette).py(px(20.0)));
        }
        let mut summary = div()
            .w(px(232.0))
            .min_w_0()
            .flex_none()
            .flex()
            .flex_col()
            .gap(px(8.0))
            .when(self.compact, |column| column.w_full())
            .child(self.action_caption(A::SelectedModel, palette));
        if let Some(model) = &self.actions.selected {
            summary = summary.child(
                div()
                    .w_full()
                    .min_w_0()
                    .text_size(px(13.0))
                    .line_height(px(20.0))
                    .font_weight(FontWeight::SEMIBOLD)
                    .whitespace_normal()
                    .child(text!(model.clone())),
            );
            if !self.actions.selected_available() {
                summary = summary.child(self.action_message(
                    "selected-model-unavailable",
                    self.locale.action_text(A::SelectedMissing),
                    true,
                    palette,
                ));
            } else if !cli_bridge::writable_model(model) {
                summary = summary.child(self.action_message(
                    "model-write-limit",
                    self.locale.action_text(A::UnicodeModel),
                    true,
                    palette,
                ));
            }
        } else {
            summary = summary.child(self.action_caption(A::PickerHint, palette));
        }
        summary = summary
            .child(self.action_caption(A::Preserve, palette))
            .child(div().flex_1().min_h(px(8.0)));
        if self.actions.updating_connection() {
            summary = summary
                .child(self.action_caption(A::UpdateHint, palette))
                .child(self.action_button(
                    "preview-connect",
                    A::PreviewUpdate,
                    Action::Run(JobKind::Preview),
                    palette,
                    cx,
                ))
                .child(self.action_button(
                    "disconnect",
                    A::Disconnect,
                    Action::ConfirmDisconnect,
                    palette,
                    cx,
                ));
        } else if self
            .actions
            .ownership
            .is_some_and(|state| state != Ownership::Unmanaged)
        {
            summary = summary
                .child(self.action_caption(A::ManagedHint, palette))
                .child(self.action_button(
                    "disconnect",
                    A::Disconnect,
                    Action::ConfirmDisconnect,
                    palette,
                    cx,
                ));
        } else {
            summary = summary.child(self.action_button(
                "preview-connect",
                A::Preview,
                Action::Run(JobKind::Preview),
                palette,
                cx,
            ));
        }
        div()
            .w_full()
            .min_w_0()
            .flex()
            .gap(px(18.0))
            .when(self.compact, |columns| columns.flex_col().gap(px(14.0)))
            .child(choices)
            .child(summary)
    }

    fn gateway_panel(&self, palette: Palette, cx: &mut Context<Self>) -> Div {
        let visible = self.gateway_settings
            || self.actions.catalog.is_none()
            || !self.actions.catalog_is_current();
        let mut panel = div()
            .w_full()
            .min_w_0()
            .border_t_1()
            .border_color(rgb(palette.border))
            .pt(px(10.0))
            .flex()
            .flex_col()
            .gap(px(8.0))
            .child(
                div()
                    .w_full()
                    .min_w_0()
                    .flex()
                    .items_center()
                    .justify_between()
                    .flex_wrap()
                    .gap(px(8.0))
                    .child(
                        self.action_button(
                            "gateway-settings",
                            A::GatewaySettings,
                            Action::ToggleGateway,
                            palette,
                            cx,
                        )
                        .aria_expanded(visible),
                    )
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap(px(8.0))
                            .min_w_0()
                            .when(!self.compact && !visible, |row| {
                                row.child(
                                    div()
                                        .max_w(px(220.0))
                                        .truncate()
                                        .text_size(px(11.0))
                                        .text_color(rgb(palette.muted))
                                        .child(text!(self.actions.address.clone())),
                                )
                            })
                            .child(self.action_button(
                                "query-models",
                                A::Query,
                                Action::Run(JobKind::Inspect),
                                palette,
                                cx,
                            )),
                    ),
            );
        if visible {
            panel = panel.child(
                div()
                    .w_full()
                    .min_w_0()
                    .flex()
                    .flex_col()
                    .gap(px(6.0))
                    .child(self.action_caption(A::Gateway, palette))
                    .child(
                        div()
                            .id("gateway-field")
                            .w_full()
                            .min_w_0()
                            .anchor_scroll(self.scroll_anchors.get("gateway-address").cloned())
                            .child(self.gateway_input.clone()),
                    )
                    .child(self.action_caption(
                        if self.actions.managed_address_verified() {
                            A::ManagedGatewayHint
                        } else {
                            A::GatewayHint
                        },
                        palette,
                    )),
            );
        }
        panel
    }

    fn connection_controls(&self, palette: Palette, cx: &mut Context<Self>) -> Div {
        let mut group = div().w_full().min_w_0().flex().flex_col().gap(px(12.0));
        if self.actions.uncertain {
            group = group.child(self.action_message(
                "uncertain-result",
                self.locale.action_text(A::Uncertain),
                true,
                palette,
            ));
        }
        if let Some(error) = self.actions.error {
            group = group.child(
                div()
                    .id("connection-error")
                    .role(Role::Alert)
                    .w_full()
                    .min_w_0()
                    .p(px(10.0))
                    .rounded(px(6.0))
                    .bg(rgb(palette.danger_bg))
                    .text_color(rgb(palette.danger))
                    .text_size(px(12.0))
                    .line_height(px(18.0))
                    .whitespace_normal()
                    .child(text!(format!(
                        "{} [{}]",
                        self.locale.action_text(error_text(error.kind)),
                        error.code()
                    ))),
            );
        }
        if let Some(kind) = self.actions.busy() {
            let key = match kind {
                JobKind::Inspect => A::Querying,
                JobKind::Preview => A::Previewing,
                JobKind::Apply => A::Saving,
                JobKind::Status => A::Checking,
                JobKind::Disconnect => A::Restoring,
            };
            group = group.child(
                div()
                    .w_full()
                    .flex()
                    .flex_wrap()
                    .gap(px(8.0))
                    .items_center()
                    .child(
                        self.action_message(
                            "operation-status",
                            self.locale.action_text(key),
                            kind.is_mutation(),
                            palette,
                        )
                        .flex_1(),
                    )
                    .child(self.action_button(
                        "cancel-operation",
                        A::Cancel,
                        Action::Cancel,
                        palette,
                        cx,
                    )),
            );
        } else if self.check_after_refresh && self.refresh_task.is_some() {
            group = group.child(self.action_message(
                "reconcile-status",
                self.locale.action_text(A::Checking),
                true,
                palette,
            ));
        } else if self.actions.reconcile_needed()
            || (matches!(
                self.actions.notice,
                Some(Notice::Connected | Notice::ModelUpdated | Notice::Disconnected(_))
            ) && self.actions.snapshot_failed())
        {
            if !self.actions.uncertain {
                group = group.child(self.action_message(
                    "reconcile-status",
                    self.locale.action_text(A::NeedsReview),
                    true,
                    palette,
                ));
            }
        } else if let Some(notice) = &self.actions.notice {
            let key = match notice {
                Notice::Connected => Some(A::ConnectDone),
                Notice::ModelUpdated => Some(A::UpdateDone),
                Notice::Disconnected(_) => Some(A::DisconnectDone),
                Notice::StatusVerified => None,
                _ => None,
            };
            if let Some(key) = key {
                group = group.child(self.action_message(
                    "operation-result",
                    self.locale.action_text(key),
                    false,
                    palette,
                ));
            }
        }
        if !cli_bridge::transactions_supported() {
            group = group.child(self.action_message(
                "platform-limit",
                self.locale.action_text(A::WindowsHint),
                true,
                palette,
            ));
        }
        if let Some(preview) = self.preview_panel(palette, cx) {
            return group.child(preview);
        }
        if self.actions.disconnect_confirmation {
            return group.child(
                div()
                    .w_full()
                    .min_w_0()
                    .p(px(12.0))
                    .rounded(px(6.0))
                    .border_1()
                    .border_color(rgb(palette.border))
                    .bg(rgb(palette.inset))
                    .flex()
                    .flex_col()
                    .gap(px(12.0))
                    .child(self.action_caption(A::DisconnectHint, palette))
                    .child(
                        div()
                            .flex()
                            .flex_wrap()
                            .gap(px(8.0))
                            .child(self.action_button(
                                "confirm-disconnect",
                                A::ConfirmDisconnect,
                                Action::Run(JobKind::Disconnect),
                                palette,
                                cx,
                            ))
                            .child(self.action_button(
                                "keep-connection",
                                A::KeepConnection,
                                Action::KeepConnection,
                                palette,
                                cx,
                            )),
                    ),
            );
        }
        group
            .child(self.action_caption(A::HealthUnprobed, palette))
            .child(self.model_picker(palette, cx))
            .child(self.gateway_panel(palette, cx))
    }
}
