use std::{
    collections::BTreeMap,
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

use gpui::{
    AccessibleAction, App, ClipboardItem, Context, Div, Entity, FocusHandle, FontWeight,
    IntoElement, KeyDownEvent, Render, Role, ScrollAnchor, ScrollHandle, SharedString, Stateful,
    Subscription, Task, Toggled, Window, div, img, point, prelude::*, px, rgb, svg, text,
};
use gpui_kit_material::{
    components::{
        ButtonSize, ButtonVariant, MaterialButton, NavIcon, NavItem, NavigationRail,
        material_button,
    },
    icons::Symbol,
};
use magpie_core::agents::ConfigFileState;

use crate::{
    actions_state::{ActionsState, JobKind, Notice},
    actions_text::{ActionText as A, error_text},
    assets,
    cli_bridge::{self, Ownership},
    gateway_process::{Error as GatewayError, GatewayProcess},
    localization::{Locale, Text},
    provider_ui::{ProviderCloseDecision, ProviderUi},
    snapshot::{Agent, Snapshot},
    text_input::{InputEvent, TextInput},
    theme::{self, Appearance, Palette},
};

const BREAKPOINT: f32 = 720.0;
const DOWN: &[u8] = br#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24" fill="none" stroke="white" stroke-width="1.7" stroke-linecap="round" stroke-linejoin="round"><path d="m6 9 6 6 6-6"/></svg>"#;
const CLOSE: &[u8] = br#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24" fill="none" stroke="white" stroke-width="1.7" stroke-linecap="round"><path d="m6 6 12 12M6 18 18 6"/></svg>"#;
const NAVIGATION: &[u8] = br#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24" fill="none" stroke="white" stroke-width="1.7" stroke-linecap="round"><path d="M4 6h16M4 12h16M4 18h16"/></svg>"#;

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
    StartGateway,
    StopGateway,
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
    ToggleNavigation,
}

/// Native agent browser with explicit Codex CLI transactions.
pub struct MagpieWindow {
    snapshot: Option<Snapshot>,
    refresh_task: Option<Task<()>>,
    page: Page,
    locale: Locale,
    appearance: Appearance,
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
    providers: Entity<ProviderUi>,
    provider_busy: bool,
    provider_pending: Arc<AtomicBool>,
    restore_root_focus: bool,
    actions: ActionsState,
    gateway_input: Entity<TextInput>,
    gateway: Option<GatewayProcess>,
    gateway_busy: bool,
    gateway_error: Option<GatewayError>,
    search_input: Entity<TextInput>,
    _subscriptions: Vec<Subscription>,
    model_focus: BTreeMap<String, FocusHandle>,
    model_scroll: ScrollHandle,
    model_anchor: ScrollAnchor,
    pending_action: Option<Action>,
    pending_focus: Option<&'static str>,
    check_after_refresh: bool,
    diagnostics: bool,
    preview_fields: bool,
    gateway_settings: bool,
    navigation_open: bool,
    return_focus: Option<&'static str>,
    close_handler_installed: bool,
    close_requested: bool,
    close_discard_confirmed: bool,
    shutdown_pending: Arc<AtomicBool>,
}

impl MagpieWindow {
    /// Starts one background discovery; never modifies an Agent's files.
    pub fn new(cx: &mut Context<Self>) -> Self {
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
            "gateway-start",
            "gateway-stop",
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
        let providers = cx.new(ProviderUi::new);
        let provider_busy = providers.read(cx).busy();
        let provider_pending = providers.read(cx).pending();
        let mut subscriptions = vec![
            cx.observe(&providers, |view, provider, cx| {
                view.provider_busy = provider.read(cx).busy();
                if !view.provider_is_busy()
                    && view.check_after_refresh
                    && view.refresh_task.is_none()
                    && view.page == Page::Agents
                    && view.agent("codex").is_some()
                    && !view.close_requested
                {
                    view.check_after_refresh = false;
                    view.start_job(JobKind::Status, cx);
                }
                cx.notify();
            }),
            cx.subscribe(&providers, |view, _, decision, cx| {
                match decision {
                    ProviderCloseDecision::KeepEditing => {
                        view.close_requested = false;
                        view.close_discard_confirmed = false;
                    }
                    ProviderCloseDecision::Discard => {
                        view.close_discard_confirmed = true;
                        view.close_requested = true;
                    }
                }
                cx.notify();
            }),
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
                        view.model_scroll.set_offset(point(px(0.0), px(0.0)));
                    }
                    InputEvent::Submit => view.pending_action = Some(Action::Run(JobKind::Preview)),
                    InputEvent::Escape => view.pending_action = Some(Action::Escape),
                }
                cx.notify();
            }),
        ];
        subscriptions.push(cx.on_app_quit(|view, cx| {
            view.gateway.take();
            view.actions.cancel();
            view.providers.update(cx, |provider, _| provider.cancel());
            let pending = view.shutdown_pending.clone();
            let provider_pending = view.provider_pending.clone();
            async move {
                // Direct application quit has GPUI's 200ms budget; normal
                // window close below waits for the full bridge cleanup.
                while pending.load(Ordering::Acquire) || provider_pending.load(Ordering::Acquire) {
                    async_io::Timer::after(Duration::from_millis(10)).await;
                }
            }
        }));
        let model_anchor = ScrollAnchor::for_handle(detail_scroll.clone());
        let mut view = Self {
            snapshot: None,
            refresh_task: None,
            page: Page::Agents,
            locale: Locale::system(),
            appearance: Appearance::System,
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
            providers,
            provider_busy,
            provider_pending,
            restore_root_focus: false,
            actions,
            gateway_input,
            gateway: None,
            gateway_busy: false,
            gateway_error: None,
            search_input,
            _subscriptions: subscriptions,
            model_focus: BTreeMap::new(),
            model_scroll: ScrollHandle::new(),
            model_anchor,
            pending_action: None,
            pending_focus: None,
            check_after_refresh: true,
            diagnostics: false,
            preview_fields: false,
            gateway_settings: false,
            navigation_open: false,
            return_focus: None,
            close_handler_installed: false,
            close_requested: false,
            close_discard_confirmed: false,
            shutdown_pending: Arc::new(AtomicBool::new(false)),
        };
        view.refresh(cx);
        view
    }

    pub fn focus(&self, window: &mut Window, cx: &mut App) {
        self.focus.focus(window, cx);
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
                view.snapshot = Some(snapshot);
                view.refresh_task = None;
                if view.check_after_refresh && !view.provider_is_busy() {
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
        self.navigation_open = false;
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
                self.model_scroll.scroll_to_item(index);
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
            let row_control = self.page == Page::Agents
                && (matches!(*id, "codex-model" | "codex-connect" | "codex-disconnect")
                    || (*id == "cancel-operation" && self.details != Some("codex")));
            if row_control {
                if let Some(index) = self.row_index("codex") {
                    self.roster_scroll.scroll_to_item(index);
                    window.refresh();
                }
                return;
            }
            if matches!(
                *id,
                "agents-page"
                    | "providers-page"
                    | "gateway-page"
                    | "settings-page"
                    | "refresh"
                    | "more-pages"
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
            Action::StartGateway => self.start_gateway(cx),
            Action::StopGateway => self.stop_gateway(cx),
            Action::Refresh => {
                if self.navigation_open {
                    self.pending_focus = Some("more-pages");
                }
                self.navigation_open = false;
                if self.page == Page::Providers {
                    self.providers
                        .update(cx, |provider, cx| provider.refresh(cx));
                    self.provider_busy = self.providers.read(cx).busy();
                    cx.notify();
                    return;
                }
                if self.page == Page::Gateway {
                    self.start_job(JobKind::Inspect, cx);
                    cx.notify();
                    return;
                }
                self.actions.invalidate_preview();
                self.check_after_refresh = self.page == Page::Agents;
                if self.check_after_refresh {
                    self.actions.ownership = None;
                }
                self.refresh(cx);
            }
            Action::Page(page) => {
                let entering = self.page != page && page == Page::Agents;
                let from_navigation = self.navigation_open;
                self.page = page;
                self.navigation_open = false;
                if from_navigation {
                    self.pending_focus = Some("more-pages");
                }
                if page == Page::Gateway {
                    self.gateway_settings = true;
                }
                if entering {
                    self.check_connection(cx);
                }
            }
            Action::Details(id) => {
                if self.details == Some(id) {
                    self.close_details(window);
                } else if self.agent(id).is_some() {
                    self.details = Some(id);
                    self.return_focus = None;
                    self.navigation_open = false;
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
                self.navigation_open = false;
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
            Action::Language(locale) => self.locale = locale,
            Action::Appearance(appearance) => {
                self.appearance = appearance;
                theme::apply_material_theme(appearance, window, cx);
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
            Action::Cancel => {
                self.actions.cancel();
                self.providers.update(cx, |provider, _| provider.cancel());
            }
            Action::ToggleDiagnostics => self.diagnostics = !self.diagnostics,
            Action::ToggleFields => self.preview_fields = !self.preview_fields,
            Action::ToggleGateway => self.gateway_settings = !self.gateway_settings,
            Action::ToggleNavigation => {
                self.navigation_open = !self.navigation_open;
                self.pending_focus = Some("more-pages");
            }
            Action::Escape => {
                if self.navigation_open {
                    self.navigation_open = false;
                    self.pending_focus = Some("more-pages");
                } else if self.provider_is_busy() {
                    self.providers.update(cx, |provider, _| provider.cancel());
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
                if self.navigation_open
                    || self.provider_is_busy()
                    || self.actions.busy().is_some()
                    || (self.page == Page::Agents && self.details.is_some()) =>
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
    ) -> MaterialButton {
        let label = label.into();
        let enabled = self.action_enabled(action);
        let toggled = match action {
            Action::Page(page) => Some(page == self.page),
            Action::Language(locale) => Some(locale == self.locale),
            Action::Appearance(appearance) => Some(appearance == self.appearance),
            Action::ToggleDiagnostics => Some(self.diagnostics),
            Action::ToggleFields => Some(self.preview_fields),
            Action::ToggleGateway => Some(self.gateway_settings),
            Action::ToggleNavigation => Some(self.navigation_open),
            _ => None,
        };
        let primary = matches!(
            action,
            Action::StartGateway | Action::Run(JobKind::Apply | JobKind::Preview)
        );
        let variant = if primary {
            ButtonVariant::Filled
        } else if toggled == Some(true) {
            ButtonVariant::Tonal
        } else {
            ButtonVariant::Text
        };
        material_button(id, variant)
            .button_size(ButtonSize::Small)
            .disabled(!enabled)
            .accessibility_label(label.clone())
            .when_some(self.control_focus.get(id), |button, handle| {
                button.track_focus(handle)
            })
            .when_some(self.scroll_anchors.get(id), |button, anchor| {
                button.anchor_scroll(Some(anchor.clone()))
            })
            .when_some(toggled, |button, selected| {
                button.toggle(true).selected(selected)
            })
            .on_click(cx.listener(move |view, _, window, cx| view.act(action, window, cx)))
            .when_some(icon, |button, data| {
                button.child(
                    svg()
                        .data(data)
                        .size(px(20.0))
                        .text_color(rgb(palette.muted)),
                )
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
                .size(px(22.0))
                .text_color(rgb(palette.text))
                .into_any_element(),
            Some(path) => img(path).size(px(22.0)).into_any_element(),
            None => svg()
                .data(assets::TERMINAL)
                .size(px(22.0))
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
            .rounded(px(20.0))
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
            .gap(px(10.0))
            .child(
                div()
                    .size(px(22.0))
                    .flex_none()
                    .child(Self::icon(agent, palette)),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .flex_col()
                    .gap(px(1.0))
                    .child(
                        div()
                            .text_size(px(14.0))
                            .line_height(px(18.0))
                            .font_weight(FontWeight::MEDIUM)
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
                            .line_height(px(16.0))
                            .text_color(rgb(status_color))
                            .child(text!(status)),
                    ),
            );
        let mut controls = div()
            .flex()
            .items_center()
            .gap(px(10.0))
            .min_w_0()
            .when(self.compact, |row| row.w_full().pl(px(32.0)).justify_end());
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
                .w(px(196.0))
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
                let checked = self.actions.ownership == Some(Ownership::Connected);
                let unknown = self.actions.ownership.is_none()
                    || matches!(
                        self.actions.ownership,
                        Some(Ownership::Drifted | Ownership::RecoveryRequired)
                    );
                controls = controls.child(
                    self.button(id, "", None, action, palette, cx)
                        .w(px(38.0))
                        .h(px(32.0))
                        .px(px(1.0))
                        .role(if enabled { Role::Switch } else { Role::Status })
                        .aria_label(format!(
                            "{} · {} · {}",
                            agent.name,
                            self.locale.action_text(label),
                            self.locale.action_text(self.ownership_label())
                        ))
                        .aria_toggled(if unknown {
                            Toggled::Mixed
                        } else if checked {
                            Toggled::True
                        } else {
                            Toggled::False
                        })
                        .bg(rgb(if selected {
                            palette.inset
                        } else {
                            palette.surface
                        }))
                        .child(
                            div()
                                .w(px(32.0))
                                .h(px(20.0))
                                .p(px(2.0))
                                .rounded(px(20.0))
                                .flex()
                                .items_center()
                                .when(checked, |track| track.justify_end())
                                .when(!checked, |track| track.justify_start())
                                .bg(rgb(if checked {
                                    palette.text
                                } else {
                                    palette.border
                                }))
                                .child(div().size(px(16.0)).rounded(px(16.0)).bg(rgb(
                                    if checked {
                                        palette.surface
                                    } else {
                                        palette.muted
                                    },
                                ))),
                        ),
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
            .min_h(px(56.0))
            .px(px(14.0))
            .py(px(9.0))
            .bg(rgb(if selected {
                palette.inset
            } else {
                palette.surface
            }))
            .flex()
            .items_center()
            .gap(px(12.0))
            .when(self.compact, |row| {
                row.flex_col().items_stretch().gap(px(8.0))
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
                panel.max_h(px(self.ordered_agents().len() as f32 * 57.0))
            })
            .child(rows)
    }

    fn config_notice(&self, agent: &Agent, palette: Palette) -> Div {
        let (color, background) = Self::config_colors(agent, palette);
        div()
            .w_full()
            .min_w_0()
            .p(px(12.0))
            .rounded(px(12.0))
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
                    .rounded(px(12.0))
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
            .h(px(64.0))
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
                            .rounded(px(12.0))
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
                        .child(self.caption(Text::SessionOnly, palette)),
                ),
        )
    }
}

impl MagpieWindow {
    fn page_label(&self, page: Page) -> &'static str {
        match page {
            Page::Agents => self.locale.text(Text::Agents),
            Page::Providers => self.locale.action_text(A::ProvidersPage),
            Page::Gateway => self.locale.action_text(A::GatewayPage),
            Page::Settings => self.locale.text(Text::Settings),
        }
    }

    fn page_control(page: Page) -> &'static str {
        match page {
            Page::Agents => "agents-page",
            Page::Providers => "providers-page",
            Page::Gateway => "gateway-page",
            Page::Settings => "settings-page",
        }
    }

    fn navigation_button(
        &self,
        page: Page,
        vertical: bool,
        palette: Palette,
        cx: &mut Context<Self>,
    ) -> MaterialButton {
        self.button(
            Self::page_control(page),
            self.page_label(page),
            None,
            Action::Page(page),
            palette,
            cx,
        )
        .min_w_0()
        .when(vertical, |button| {
            button.w_full().justify_start().whitespace_normal()
        })
    }

    fn brand(palette: Palette) -> Div {
        div()
            .flex()
            .items_center()
            .gap(px(7.0))
            .flex_none()
            .child(
                svg()
                    .path("icons/magpie.svg")
                    .size(px(21.0))
                    .text_color(rgb(palette.text)),
            )
            .child(
                div()
                    .text_size(px(17.0))
                    .line_height(px(22.0))
                    .font_weight(FontWeight::SEMIBOLD)
                    .child(text!("magpie")),
            )
    }

    fn gateway_page(&self, palette: Palette, cx: &mut Context<Self>) -> Div {
        let mut content = div()
            .w_full()
            .min_w_0()
            .flex()
            .flex_col()
            .gap(px(12.0))
            .p(px(if self.compact { 12.0 } else { 16.0 }))
            .child(self.gateway_panel(palette, cx))
            .child(self.action_caption(A::HealthUnprobed, palette));
        let status = if self.gateway_busy {
            A::GatewayWorking
        } else if self.gateway.is_some() {
            A::GatewayRunning
        } else {
            A::GatewayNotManaged
        };
        let lifecycle = div()
            .w_full()
            .flex()
            .flex_col()
            .gap(px(12.0))
            .p(px(16.0))
            .rounded(px(20.0))
            .bg(rgb(palette.selected))
            .child(
                div()
                    .font_weight(FontWeight::SEMIBOLD)
                    .child(text!(self.locale.action_text(status))),
            )
            .child(self.action_caption(A::GatewayLifecycleHint, palette))
            .child(
                div()
                    .flex()
                    .flex_wrap()
                    .gap(px(8.0))
                    .child(self.action_button(
                        "gateway-start",
                        A::GatewayStart,
                        Action::StartGateway,
                        palette,
                        cx,
                    ))
                    .child(self.action_button(
                        "gateway-stop",
                        A::GatewayStop,
                        Action::StopGateway,
                        palette,
                        cx,
                    )),
            );
        content = content.child(lifecycle);
        if let Some(error) = self.gateway_error {
            let key = match error {
                GatewayError::Address => A::InvalidAddressError,
                GatewayError::Cli => A::CliMissingError,
                GatewayError::Timeout => A::TimeoutError,
                GatewayError::Startup => A::GatewayStartError,
                GatewayError::Exited => A::GatewayExited,
                GatewayError::Cleanup => A::CleanupError,
            };
            content = content.child(self.action_message(
                "gateway-lifecycle-error",
                self.locale.action_text(key),
                true,
                palette,
            ));
        }
        if let Some(error) = self.actions.error {
            content = content.child(
                self.action_message(
                    "gateway-page-error",
                    format!(
                        "{} [{}]",
                        self.locale.action_text(error_text(error.kind)),
                        error.code()
                    ),
                    true,
                    palette,
                )
                .role(Role::Alert)
                .bg(rgb(palette.danger_bg))
                .text_color(rgb(palette.danger)),
            );
        }
        if let Some(kind) = self.actions.busy() {
            content = content.child(
                div()
                    .flex()
                    .flex_wrap()
                    .items_center()
                    .gap(px(8.0))
                    .child(
                        self.action_message(
                            "gateway-page-progress",
                            self.locale.action_text(Self::progress_label(kind)),
                            false,
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
        }
        match &self.actions.catalog {
            None => content = content.child(self.action_caption(A::NoCatalog, palette)),
            Some(catalog) => {
                if !self.actions.catalog_is_current() {
                    content = content.child(self.action_message(
                        "gateway-page-stale",
                        self.locale.action_text(A::StaleCatalog),
                        true,
                        palette,
                    ));
                }
                content = content.child(
                    div()
                        .flex()
                        .items_center()
                        .gap(px(8.0))
                        .child(self.action_caption(A::Configured, palette).flex_1())
                        .child(
                            div()
                                .text_size(px(12.0))
                                .text_color(rgb(palette.muted))
                                .child(text!(catalog.models.len().to_string())),
                        ),
                );
                if catalog.models.is_empty() {
                    content = content.child(self.action_caption(A::NoModels, palette));
                } else {
                    content = content.child(div().w_full().min_w_0().flex().flex_col().children(
                        catalog.models.iter().map(|model| {
                            div()
                                .w_full()
                                .min_w_0()
                                .py(px(8.0))
                                .border_b_1()
                                .border_color(rgb(palette.border))
                                .whitespace_normal()
                                .child(text!(model.id.clone()))
                        }),
                    ));
                }
            }
        }
        self.panel(palette).h_full().child(
            div()
                .id("gateway-page-scroll")
                .flex_1()
                .min_h_0()
                .min_w_0()
                .overflow_y_scroll()
                .track_scroll(&self.detail_scroll)
                .child(content),
        )
    }
}

impl Render for MagpieWindow {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if !self.close_handler_installed {
            self.close_handler_installed = true;
            self._subscriptions
                .push(cx.observe_window_appearance(window, |view, window, cx| {
                    if view.appearance == Appearance::System {
                        theme::apply_material_theme(view.appearance, window, cx);
                    }
                    cx.notify();
                }));
            let weak = cx.entity().downgrade();
            window.on_window_should_close(cx, move |window, cx| {
                weak.update(cx, |view, cx| view.request_window_close(window, cx))
                    .unwrap_or(true)
            });
        }
        self.provider_busy = self.providers.read(cx).busy();
        if self.close_requested && self.actions.busy().is_none() && !self.provider_is_busy() {
            if self.close_discard_confirmed || !self.providers.read(cx).has_unsaved_changes(cx) {
                window.remove_window();
            } else {
                self.show_provider_close_confirmation(window, cx);
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
        let disabled = self.gateway_busy
            || self.actions.busy().is_some()
            || self.refresh_task.is_some()
            || self.provider_is_busy();
        let gateway_locked = disabled || self.gateway.is_some();
        let locale = self.locale;
        self.providers.update(cx, |provider, cx| {
            provider.set_presentation(locale, palette, self.compact, cx);
        });
        self.gateway_input.update(cx, |input, cx| {
            if input.text() != self.actions.address {
                input.set_text(&self.actions.address, cx);
            }
            input.set_presentation(
                "127.0.0.1:3425",
                locale.action_text(A::Gateway),
                palette,
                gateway_locked,
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
        let gutter = if self.compact { 12.0 } else { 24.0 };
        let wide = window.viewport_size().width >= px(1280.0);
        if !self.compact && self.navigation_open {
            self.navigation_open = false;
            Self::focus_next_frame(
                self.control_focus[Self::page_control(self.page)].clone(),
                window,
            );
        }
        // Responsive navigation has one active instance of each focus handle.
        // If a focused desktop tab disappears on resize, keep focus on its
        // compact navigation trigger, not on an unmounted element.
        if self.compact
            && !self.navigation_open
            && !disabled
            && [
                "agents-page",
                "providers-page",
                "gateway-page",
                "settings-page",
            ]
            .iter()
            .any(|id| {
                self.control_focus
                    .get(id)
                    .is_some_and(|handle| handle.is_focused(window))
            })
        {
            Self::focus_next_frame(self.control_focus["more-pages"].clone(), window);
        }
        let mut header = div()
            .h(px(64.0))
            .flex_none()
            .px(px(gutter))
            .flex()
            .items_center()
            .justify_between()
            .gap(px(8.0));
        if wide {
            header = header.child(
                div()
                    .flex_1()
                    .min_w_0()
                    .text_size(px(22.0))
                    .font_weight(FontWeight::MEDIUM)
                    .child(text!(self.page_label(self.page))),
            );
        } else {
            header = header.child(Self::brand(palette));
            if self.compact {
                header = header.child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .text_size(px(13.0))
                        .text_color(rgb(palette.muted))
                        .truncate()
                        .child(text!(self.page_label(self.page))),
                );
            } else {
                header = header.child(
                    div()
                        .id("top-navigation")
                        .role(Role::Navigation)
                        .flex()
                        .items_center()
                        .gap(px(3.0))
                        .min_w_0()
                        .children(
                            [Page::Agents, Page::Providers, Page::Gateway]
                                .map(|page| self.navigation_button(page, false, palette, cx)),
                        ),
                );
            }
        }
        let mut window_actions = div().flex().items_center().gap(px(2.0)).flex_none();
        if self.compact {
            window_actions = window_actions.child(
                self.button(
                    "more-pages",
                    "",
                    Some(if self.navigation_open {
                        CLOSE
                    } else {
                        NAVIGATION
                    }),
                    Action::ToggleNavigation,
                    palette,
                    cx,
                )
                .w(px(32.0))
                .px(px(6.0))
                .bg(rgb(palette.background))
                .aria_expanded(self.navigation_open)
                .aria_label(self.locale.action_text(A::Roadmap)),
            );
        }
        window_actions = window_actions.child(
            self.button(
                "refresh",
                "",
                Some(assets::REFRESH),
                Action::Refresh,
                palette,
                cx,
            )
            .w(px(32.0))
            .px(px(6.0))
            .bg(rgb(palette.background))
            .aria_label(self.locale.text(Text::Refresh)),
        );
        if !wide && !self.compact {
            window_actions = window_actions.child(
                self.button(
                    "settings-page",
                    "",
                    Some(assets::SETTINGS),
                    Action::Page(Page::Settings),
                    palette,
                    cx,
                )
                .w(px(32.0))
                .px(px(6.0))
                .bg(rgb(palette.background))
                .aria_label(self.locale.text(Text::Settings)),
            );
        }
        header = header.child(window_actions);
        let drawer = div()
            .id("navigation-drawer")
            .role(Role::Navigation)
            .w_full()
            .min_w_0()
            .flex()
            .flex_col()
            .gap(px(6.0))
            .p(px(12.0))
            .border_b_1()
            .border_color(rgb(palette.border))
            .bg(rgb(palette.background))
            .children(
                [Page::Agents, Page::Providers, Page::Gateway, Page::Settings]
                    .map(|page| self.navigation_button(page, true, palette, cx)),
            );
        let mut main = div()
            .flex_1()
            .min_w_0()
            .min_h_0()
            .flex()
            .flex_col()
            .child(header);
        if self.navigation_open && self.compact {
            // The drawer is the only body while open: Tab cannot enter hidden
            // page controls. The page entities and all drafts remain alive.
            main = main.child(
                div()
                    .id("navigation-scroll")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .child(drawer),
            );
        } else {
            let body = match self.page {
                Page::Agents => self.agents_page(palette, cx),
                Page::Providers => div()
                    .size_full()
                    .min_w_0()
                    .min_h_0()
                    .child(self.providers.clone()),
                Page::Gateway => self.gateway_page(palette, cx),
                Page::Settings => self.settings_page(palette, cx),
            };
            let mut body_container = div()
                .flex_1()
                .min_h_0()
                .min_w_0()
                .px(px(gutter))
                .pb(px(16.0))
                .pt(px(8.0))
                .flex()
                .flex_col()
                .gap(px(12.0));
            if self.page == Page::Agents {
                let scan_status = if self.refresh_task.is_some() {
                    self.locale.text(Text::Refreshing).to_owned()
                } else {
                    self.locale.scan_summary(
                        self.snapshot
                            .as_ref()
                            .map_or(0, |snapshot| snapshot.agents.len()),
                    )
                };
                body_container = body_container.child(
                    div()
                        .flex()
                        .flex_wrap()
                        .items_center()
                        .gap(px(8.0))
                        .child(self.action_caption(A::WorkspaceIntro, palette).flex_1())
                        .child(
                            div()
                                .id("scan-status")
                                .role(Role::Status)
                                .flex_none()
                                .text_size(px(11.0))
                                .text_color(rgb(palette.muted))
                                .child(text!(scan_status)),
                        ),
                );
            } else if self.page == Page::Settings {
                body_container =
                    body_container.child(self.caption(Text::Preferences, palette).flex_none());
            }
            main = main.child(body_container.child(div().flex_1().min_h_0().min_w_0().child(body)));
        }
        let pages = [Page::Agents, Page::Providers, Page::Gateway, Page::Settings];
        let icons = ["terminal", "cloud", "hub", "settings"];
        let items = pages
            .into_iter()
            .zip(icons)
            .map(|(page, icon)| {
                NavItem::new(
                    Self::page_control(page),
                    self.page_label(page),
                    NavIcon::symbol(
                        Symbol::named(icon).expect("pinned Material navigation symbol"),
                    ),
                )
                .disabled(!self.action_enabled(Action::Page(page)))
            })
            .collect::<Vec<_>>();
        let sidebar = div()
            .w(px(220.0))
            .h_full()
            .flex_none()
            .flex()
            .flex_col()
            .child(
                div()
                    .flex_none()
                    .h(px(64.0))
                    .px(px(24.0))
                    .flex()
                    .items_center()
                    .child(Self::brand(palette)),
            )
            .child(
                NavigationRail::new("material-navigation", self.locale.action_text(A::Roadmap))
                    .expanded(true)
                    .expanded_width(220.0)
                    .items(items)
                    .selected(Self::page_control(self.page))
                    .on_change(cx.listener(move |view, value: &SharedString, window, cx| {
                        if let Some(page) = pages
                            .into_iter()
                            .find(|page| Self::page_control(*page) == value.as_ref())
                        {
                            view.act(Action::Page(page), window, cx);
                        }
                    })),
            );

        div()
            .id("magpie")
            .track_focus(&self.focus)
            .tab_group()
            .tab_stop(false)
            .on_key_down(cx.listener(Self::key_down))
            .size_full()
            .flex()
            .font(theme::ui_font(cx, self.locale))
            .text_size(px(15.0))
            .line_height(px(22.0))
            .text_color(rgb(palette.text))
            .bg(rgb(palette.background))
            .when(wide, |root| root.child(sidebar))
            .child(main)
    }
}

impl MagpieWindow {
    fn request_window_close(&mut self, window: &Window, cx: &mut Context<Self>) -> bool {
        if !self.close_requested {
            self.close_discard_confirmed = false;
        }
        if self.actions.busy().is_some() || self.provider_is_busy() {
            self.close_requested = true;
            self.actions.cancel();
            self.providers.update(cx, |provider, _| provider.cancel());
            cx.notify();
            false
        } else if !self.close_discard_confirmed && self.providers.read(cx).has_unsaved_changes(cx) {
            self.close_requested = true;
            self.show_provider_close_confirmation(window, cx);
            false
        } else {
            true
        }
    }

    fn show_provider_close_confirmation(&mut self, window: &Window, cx: &mut Context<Self>) {
        if self.providers.read(cx).close_confirmation_open() {
            return;
        }
        self.page = Page::Providers;
        self.navigation_open = false;
        self.pending_focus = None;
        self.restore_root_focus = false;
        self.providers.update(cx, |provider, cx| {
            provider.request_window_close_confirmation(window, cx)
        });
        cx.notify();
    }

    fn provider_is_busy(&self) -> bool {
        self.provider_busy || self.provider_pending.load(Ordering::Acquire)
    }

    fn action_enabled(&self, action: Action) -> bool {
        if self.close_requested || (self.refresh_task.is_some() && self.check_after_refresh) {
            return false;
        }
        if matches!(action, Action::Escape) {
            return true;
        }
        if self.gateway_busy {
            return false;
        }
        if self.actions.busy().is_some() || self.provider_is_busy() {
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
            Action::StartGateway => self.gateway.is_none(),
            Action::StopGateway => self.gateway.is_some(),
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

    fn start_gateway(&mut self, cx: &mut Context<Self>) {
        let Ok(store) = crate::provider_bridge::store_path() else {
            self.gateway_error = Some(GatewayError::Startup);
            cx.notify();
            return;
        };
        self.gateway_busy = true;
        self.gateway_error = None;
        let address = self.actions.address.clone();
        let work = cx
            .background_executor()
            .spawn(async move { GatewayProcess::start(&address, &store).await });
        cx.spawn(async move |this, cx| {
            let result = work.await;
            let _ = this.update(cx, |view, cx| {
                view.gateway_busy = false;
                match result {
                    Ok(gateway) => {
                        view.actions.set_address(gateway.address.to_string());
                        view.gateway = Some(gateway);
                        view.start_job(JobKind::Inspect, cx);
                        view.watch_gateway(cx);
                    }
                    Err(error) => view.gateway_error = Some(error),
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    fn stop_gateway(&mut self, cx: &mut Context<Self>) {
        let Some(gateway) = self.gateway.take() else {
            return;
        };
        self.gateway_busy = true;
        self.actions.invalidate_preview();
        self.actions.catalog = None;
        let work = cx.background_executor().spawn(gateway.stop());
        cx.spawn(async move |this, cx| {
            let result = work.await;
            let _ = this.update(cx, |view, cx| {
                view.gateway_busy = false;
                view.gateway_error = result.err();
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    fn watch_gateway(&self, cx: &mut Context<Self>) {
        cx.spawn(async move |this, cx| {
            loop {
                async_io::Timer::after(Duration::from_millis(500)).await;
                let keep_watching = this
                    .update(cx, |view, cx| {
                        let Some(gateway) = view.gateway.as_mut() else {
                            return false;
                        };
                        if gateway.is_running() {
                            return true;
                        }
                        view.gateway = None;
                        view.gateway_error = Some(GatewayError::Exited);
                        view.actions.invalidate_preview();
                        view.actions.catalog = None;
                        cx.notify();
                        false
                    })
                    .unwrap_or(false);
                if !keep_watching {
                    break;
                }
            }
        })
        .detach();
    }

    fn check_connection(&mut self, cx: &mut Context<Self>) {
        self.actions.invalidate_preview();
        self.actions.ownership = None;
        self.check_after_refresh = true;
        self.refresh(cx);
    }

    fn start_job(&mut self, kind: JobKind, cx: &mut Context<Self>) {
        if self.refresh_task.is_some() || self.actions.busy().is_some() || self.provider_is_busy() {
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
                if let Some(catalog) = &view.actions.catalog {
                    view.model_focus
                        .retain(|id, _| catalog.models.iter().any(|model| &model.id == id));
                    for model in &catalog.models {
                        view.model_focus
                            .entry(model.id.clone())
                            .or_insert_with(|| cx.focus_handle().tab_index(0).tab_stop(true));
                    }
                }
                view.pending_focus = Some(if view.page == Page::Gateway {
                    "query-models"
                } else if view.details != Some("codex") {
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
            .rounded(px(12.0))
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
    ) -> MaterialButton {
        self.button(
            id,
            self.locale.action_text(label),
            None,
            action,
            palette,
            cx,
        )
        .whitespace_normal()
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
            .min_h(px(36.0))
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
            .whitespace_normal()
            .when(!enabled, |row| row.opacity(0.6))
            .when(enabled, |row| {
                row.cursor_pointer()
                    .focus_visible(move |style| style.border_color(rgb(palette.accent)))
                    .hover(move |style| style.bg(rgb(palette.inset)))
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
                    .whitespace_normal()
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
        let mut preview = div()
            .w_full()
            .min_w_0()
            .p(px(12.0))
            .rounded(px(12.0))
            .border_1()
            .border_color(rgb(palette.border))
            .bg(rgb(palette.inset))
            .flex()
            .flex_col()
            .gap(px(8.0))
            .child(self.action_caption(A::PreviewHint, palette))
            .child(self.review_field(
                self.locale.text(Text::Model),
                ticket.binding.model.clone(),
                palette,
            ))
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
            .child(self.action_caption(A::Preserve, palette))
            .child(self.action_button(
                "preview-fields",
                A::Fields,
                Action::ToggleFields,
                palette,
                cx,
            ));
        if self.preview_fields {
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
                    A::Save,
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
                let rows = div()
                    .id("configured-models")
                    .w_full()
                    .min_w_0()
                    .max_h(px(152.0))
                    .flex()
                    .flex_col()
                    .overflow_y_scroll()
                    .track_scroll(&self.model_scroll)
                    .children(
                        models
                            .iter()
                            .map(|model| self.model_option(&model.id, palette, cx)),
                    );
                choices = choices.child(
                    div()
                        .id("models-field")
                        .w_full()
                        .min_w_0()
                        .anchor_scroll(Some(self.model_anchor.clone()))
                        .border_1()
                        .border_color(rgb(palette.border))
                        .rounded(px(12.0))
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
        if self
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
                    .child(self.action_caption(A::GatewayHint, palette)),
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
                    .rounded(px(12.0))
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
                Some(Notice::Connected | Notice::Disconnected(_))
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
                    .rounded(px(12.0))
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
