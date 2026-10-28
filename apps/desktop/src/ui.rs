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
use magpie_core::agents::ConfigFileState;

use crate::{
    actions_state::{ActionsState, JobKind, Notice},
    actions_text::{ActionText as A, error_text},
    assets,
    cli_bridge::{self, Ownership},
    localization::{Locale, Text},
    snapshot::{Agent, Snapshot},
    text_input::{InputEvent, TextInput},
    theme::{Appearance, Palette},
};

const BREAKPOINT: f32 = 720.0;
const BACK: &[u8] = br#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24" fill="none" stroke="white" stroke-width="1.7" stroke-linecap="round" stroke-linejoin="round"><path d="m10 5-7 7 7 7M3 12h18"/></svg>"#;
const CLOSE: &[u8] = br#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24" fill="none" stroke="white" stroke-width="1.7" stroke-linecap="round"><path d="m6 6 12 12M6 18 18 6"/></svg>"#;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Page {
    Agents,
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
    model_scroll: ScrollHandle,
    model_anchor: ScrollAnchor,
    pending_action: Option<Action>,
    pending_focus: Option<&'static str>,
    check_after_refresh: bool,
    diagnostics: bool,
    preview_fields: bool,
    close_handler_installed: bool,
    close_requested: bool,
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
                        view.model_scroll.set_offset(point(px(0.0), px(0.0)));
                    }
                    InputEvent::Submit => view.pending_action = Some(Action::Run(JobKind::Preview)),
                    InputEvent::Escape => view.pending_action = Some(Action::Escape),
                }
                cx.notify();
            }),
        ];
        subscriptions.push(cx.on_app_quit(|view, _| {
            view.actions.cancel();
            let pending = view.shutdown_pending.clone();
            async move {
                // Direct application quit has GPUI's 200ms budget; normal
                // window close below waits for the full bridge cleanup.
                while pending.load(Ordering::Acquire) {
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
            model_scroll: ScrollHandle::new(),
            model_anchor,
            pending_action: None,
            pending_focus: None,
            check_after_refresh: false,
            diagnostics: false,
            preview_fields: false,
            close_handler_installed: false,
            close_requested: false,
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
                if view.check_after_refresh {
                    view.check_after_refresh = false;
                    // Discovery can remove Codex while a mutation is finishing.
                    // Keep its unresolved state, but do not start a hidden job
                    // without a rendered cancel/status control.
                    if view.page == Page::Agents && view.details == Some("codex") {
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

    fn focus_next_frame(handle: FocusHandle, window: &mut Window) {
        // On narrow windows the destination element is not in the old tree.
        window.on_next_frame(move |window, cx| handle.focus(window, cx));
    }

    fn reveal_focused(&self, window: &mut Window, cx: &mut App) {
        if let Some(snapshot) = &self.snapshot {
            for (index, agent) in snapshot.agents.iter().enumerate() {
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
                self.model_scroll.scroll_to_item(index);
                self.model_anchor.scroll_to(window, cx);
                window.on_next_frame(|window, _| window.refresh());
                return;
            }
        }
        for (id, handle) in &self.control_focus {
            if handle.is_focused(window) {
                if let Some(anchor) = self.scroll_anchors.get(id) {
                    anchor.scroll_to(window, cx);
                    // The pinned API changes offset in a next-frame callback.
                    // Request the paint that makes that changed offset visible.
                    window.on_next_frame(|window, _| window.refresh());
                }
                break;
            }
        }
    }

    fn close_details(&mut self, window: &mut Window) {
        if let Some(id) = self.details.take() {
            let handle = self.agent_focus.get(id).unwrap_or(&self.focus).clone();
            let index = self
                .snapshot
                .as_ref()
                .and_then(|snapshot| snapshot.agents.iter().position(|agent| agent.id == id));
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
    }

    fn act(&mut self, action: Action, window: &mut Window, cx: &mut Context<Self>) {
        if !self.action_enabled(action) {
            return;
        }
        match action {
            Action::Refresh => {
                self.actions.invalidate_preview();
                self.check_after_refresh = self.details == Some("codex");
                if self.check_after_refresh {
                    self.actions.ownership = None;
                }
                self.refresh(cx);
            }
            Action::Page(page) => {
                let entering =
                    self.page != page && page == Page::Agents && self.details == Some("codex");
                self.page = page;
                if entering {
                    self.check_connection(cx);
                }
            }
            Action::Details(id) => {
                if self.details == Some(id) {
                    self.close_details(window);
                } else if self.agent(id).is_some() {
                    self.details = Some(id);
                    self.copied = None;
                    self.detail_scroll.set_offset(point(px(0.0), px(0.0)));
                    Self::focus_next_frame(self.detail_focus.clone(), window);
                    if id == "codex" {
                        self.check_connection(cx);
                    }
                }
            }
            Action::CloseDetails => self.close_details(window),
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
            Action::Appearance(appearance) => self.appearance = appearance,
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
            Action::Escape => {
                if self.actions.busy().is_some() {
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
            "escape" if self.page == Page::Agents && self.details.is_some() => {
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
            .h(px(if self.compact { 40.0 } else { 36.0 }))
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
                button.child(svg().data(data).size(px(16.0)))
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

    fn agent_row(&self, agent: &Agent, palette: Palette, cx: &mut Context<Self>) -> Stateful<Div> {
        let selected = self.details == Some(agent.id);
        let (status_color, _) = Self::config_colors(agent, palette);
        let label = format!(
            "{} · {}{}",
            agent.name,
            self.locale.text(Self::config_label(agent)),
            if selected {
                format!(" · {}", self.locale.text(Text::Selected))
            } else {
                String::new()
            }
        );
        self.button(agent.id, "", None, Action::Details(agent.id), palette, cx)
            .aria_label(label)
            .aria_expanded(selected)
            .when_some(self.agent_focus.get(agent.id), |row, focus| {
                row.track_focus(
                    &focus
                        .clone()
                        .tab_stop(self.actions.busy().is_none() && !self.check_after_refresh),
                )
            })
            .relative()
            .w_full()
            .min_h(px(68.0))
            .h_auto()
            .rounded_none()
            .px(px(12.0))
            .py(px(10.0))
            .justify_start()
            .gap(px(12.0))
            .bg(rgb(if selected {
                palette.selected
            } else {
                palette.surface
            }))
            .hover(move |style| {
                style.bg(rgb(if selected {
                    palette.selected
                } else {
                    palette.inset
                }))
            })
            .when(selected, |row| {
                row.child(
                    div()
                        .absolute()
                        .left_0()
                        .top(px(10.0))
                        .bottom(px(10.0))
                        .w(px(3.0))
                        .rounded(px(2.0))
                        .bg(rgb(palette.accent)),
                )
            })
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
                            .text_color(rgb(palette.text))
                            .child(text!(agent.name)),
                    )
                    .child(
                        div()
                            .w_full()
                            .text_size(px(12.0))
                            .line_height(px(16.0))
                            .whitespace_normal()
                            .text_color(rgb(status_color))
                            .child(text!(self.locale.text(Self::config_label(agent)))),
                    ),
            )
            .child(
                div()
                    .absolute()
                    .bottom_0()
                    .left(px(12.0))
                    .right(px(12.0))
                    .h(px(1.0))
                    .bg(rgb(palette.border)),
            )
            .child(
                svg()
                    .data(assets::CHEVRON)
                    .size(px(12.0))
                    .text_color(rgb(if selected {
                        palette.accent
                    } else {
                        palette.muted
                    })),
            )
    }

    fn roster(&self, snapshot: &Snapshot, palette: Palette, cx: &mut Context<Self>) -> Div {
        let mut rows = div()
            .id("roster-scroll")
            .flex_1()
            .min_h_0()
            .flex()
            .flex_col()
            .overflow_y_scroll()
            .track_scroll(&self.roster_scroll);
        for agent in &snapshot.agents {
            rows = rows.child(self.agent_row(agent, palette, cx));
        }
        self.panel(palette)
            .h_full()
            .child(
                div()
                    .h(px(40.0))
                    .flex_none()
                    .px(px(16.0))
                    .flex()
                    .items_center()
                    .justify_between()
                    .border_b_1()
                    .border_color(rgb(palette.border))
                    .child(self.caption(Text::Installed, palette))
                    .child(
                        div()
                            .h(px(20.0))
                            .px(px(6.0))
                            .rounded(px(4.0))
                            .bg(rgb(palette.inset))
                            .text_size(px(12.0))
                            .line_height(px(20.0))
                            .text_color(rgb(palette.muted))
                            .child(text!(snapshot.agents.len().to_string())),
                    ),
            )
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
        let identity = div()
            .flex()
            .items_center()
            .gap(px(10.0))
            .min_w_0()
            .child(
                div()
                    .size(px(28.0))
                    .flex_none()
                    .child(Self::icon(agent, palette)),
            )
            .child(
                div()
                    .min_w_0()
                    .text_size(px(18.0))
                    .line_height(px(24.0))
                    .font_weight(FontWeight::SEMIBOLD)
                    .child(text!(agent.name)),
            );
        let mut header = div()
            .min_h(px(64.0))
            .flex_none()
            .px(px(16.0))
            .py(px(10.0))
            .flex()
            .items_center()
            .justify_between()
            .gap(px(8.0))
            .border_b_1()
            .border_color(rgb(palette.border))
            .child(identity);
        if !self.compact {
            header = header.child(
                self.button(
                    "close-details",
                    "",
                    Some(CLOSE),
                    Action::CloseDetails,
                    palette,
                    cx,
                )
                .aria_label(self.locale.text(Text::HideDetails))
                .track_focus(
                    &self
                        .detail_focus
                        .clone()
                        .tab_stop(self.actions.busy().is_none() && !self.check_after_refresh),
                ),
            );
        }
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
            content = content.child(self.model_values(agent, palette));
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
            if let Some(ownership) = self.actions.ownership.filter(|_| agent.id == "codex") {
                content = content.child(
                    div()
                        .text_size(px(11.0))
                        .text_color(rgb(palette.muted))
                        .child(text!(format!("CLI · {}", ownership.as_str()))),
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
        self.panel(palette).h_full().child(header).child(
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

    fn welcome_inspector(&self, palette: Palette) -> Div {
        self.panel(palette)
            .h_full()
            .p(px(24.0))
            .justify_center()
            .items_center()
            .gap(px(12.0))
            .child(
                svg()
                    .data(assets::TERMINAL)
                    .size(px(32.0))
                    .text_color(rgb(palette.muted)),
            )
            .child(
                div()
                    .text_size(px(16.0))
                    .font_weight(FontWeight::MEDIUM)
                    .child(text!(self.locale.text(Text::PickAgent))),
            )
            .child(
                div()
                    .w_full()
                    .max_w(px(280.0))
                    .text_size(px(13.0))
                    .line_height(px(20.0))
                    .text_color(rgb(palette.muted))
                    .whitespace_normal()
                    .child(text!(self.locale.text(Text::PickAgentHint))),
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
        if snapshot.agents.is_empty() {
            return self.empty(palette, cx);
        }
        let selected = self.details.and_then(|id| self.agent(id));
        if self.compact {
            return match selected {
                Some(agent) => self.inspector(agent, palette, cx),
                None => self.roster(snapshot, palette, cx),
            };
        }
        let inspector = match selected {
            Some(agent) => self.inspector(agent, palette, cx),
            None => self.welcome_inspector(palette),
        };
        div()
            .w_full()
            .h_full()
            .min_h_0()
            .flex()
            .gap(px(16.0))
            .child(
                div()
                    .w(px(296.0))
                    .h_full()
                    .flex_none()
                    .child(self.roster(snapshot, palette, cx)),
            )
            .child(div().flex_1().min_w_0().h_full().child(inspector))
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

impl Render for MagpieWindow {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if !self.close_handler_installed {
            self.close_handler_installed = true;
            let weak = cx.entity().downgrade();
            window.on_window_should_close(cx, move |_, cx| {
                weak.update(cx, |view, cx| {
                    if view.actions.busy().is_some() {
                        view.close_requested = true;
                        view.actions.cancel();
                        cx.notify();
                        false
                    } else {
                        true
                    }
                })
                .unwrap_or(true)
            });
        }
        if self.close_requested && self.actions.busy().is_none() {
            window.remove_window();
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
        let was_compact = self.compact;
        self.compact = window.viewport_size().width < px(BREAKPOINT);
        if !was_compact
            && self.compact
            && self.page == Page::Agents
            && self.details.is_some()
            && self
                .agent_focus
                .values()
                .any(|handle| handle.is_focused(window))
        {
            Self::focus_next_frame(self.detail_focus.clone(), window);
        }
        if self.page == Page::Agents
            && self
                .agent_focus
                .iter()
                .any(|(id, handle)| handle.is_focused(window) && self.agent(id).is_none())
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
                disabled,
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
        let page_title = if self.page == Page::Agents {
            Text::LocalAgents
        } else {
            Text::Settings
        };
        let page_hint = if self.page == Page::Agents {
            Text::Intro
        } else {
            Text::Preferences
        };
        let body = match self.page {
            Page::Agents => self.agents_page(palette, cx),
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
        let heading = if self.compact && self.page == Page::Agents && self.details.is_some() {
            div()
                .min_h(px(48.0))
                .flex_none()
                .flex()
                .items_center()
                .justify_between()
                .gap(px(12.0))
                .child(
                    self.button(
                        "back-details",
                        self.locale.text(Text::BackToAgents),
                        Some(BACK),
                        Action::CloseDetails,
                        palette,
                        cx,
                    )
                    .track_focus(
                        &self
                            .detail_focus
                            .clone()
                            .tab_stop(self.actions.busy().is_none() && !self.check_after_refresh),
                    ),
                )
                .child(
                    self.button(
                        "refresh",
                        "",
                        Some(assets::REFRESH),
                        Action::Refresh,
                        palette,
                        cx,
                    )
                    .aria_label(self.locale.text(
                        if self.refresh_task.is_some() {
                            Text::Refreshing
                        } else {
                            Text::Refresh
                        },
                    )),
                )
        } else {
            div()
                .flex_none()
                .flex()
                .items_start()
                .justify_between()
                .gap(px(12.0))
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .flex()
                        .flex_col()
                        .gap(px(4.0))
                        .child(
                            div()
                                .w_full()
                                .text_size(px(if self.compact { 22.0 } else { 24.0 }))
                                .line_height(px(28.0))
                                .font_weight(FontWeight::SEMIBOLD)
                                .whitespace_normal()
                                .child(text!(self.locale.text(page_title))),
                        )
                        .child(self.caption(page_hint, palette)),
                )
                .when(self.page == Page::Agents, |heading| {
                    heading.child(
                        self.button(
                            "refresh",
                            if self.compact {
                                ""
                            } else {
                                self.locale.text(Text::Refresh)
                            },
                            Some(assets::REFRESH),
                            Action::Refresh,
                            palette,
                            cx,
                        )
                        .aria_label(self.locale.text(
                            if self.refresh_task.is_some() {
                                Text::Refreshing
                            } else {
                                Text::Refresh
                            },
                        )),
                    )
                })
        };
        div()
            .id("magpie")
            .track_focus(&self.focus)
            .tab_group()
            .tab_stop(false)
            .on_key_down(cx.listener(Self::key_down))
            .size_full()
            .flex()
            .flex_col()
            .font_family(if cfg!(target_os = "windows") {
                "Segoe UI"
            } else if cfg!(target_os = "macos") {
                "Helvetica Neue"
            } else {
                "Noto Sans"
            })
            .text_size(px(13.0))
            .line_height(px(20.0))
            .text_color(rgb(palette.text))
            .bg(rgb(palette.background))
            .child(
                div()
                    .h(px(56.0))
                    .flex_none()
                    .px(px(gutter))
                    .flex()
                    .items_center()
                    .justify_between()
                    .gap(px(8.0))
                    .border_b_1()
                    .border_color(rgb(palette.border))
                    .child(
                        div()
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
                            ),
                    )
                    .child(
                        div()
                            .flex()
                            .gap(px(4.0))
                            .child(self.button(
                                "agents-page",
                                self.locale.text(Text::Agents),
                                None,
                                Action::Page(Page::Agents),
                                palette,
                                cx,
                            ))
                            .child(self.button(
                                "settings-page",
                                self.locale.text(Text::Settings),
                                Some(assets::SETTINGS),
                                Action::Page(Page::Settings),
                                palette,
                                cx,
                            )),
                    ),
            )
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
            )
    }
}

impl MagpieWindow {
    fn action_enabled(&self, action: Action) -> bool {
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
            Action::ConfirmDisconnect => self.actions.can_disconnect(),
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
                if let Some(catalog) = &view.actions.catalog {
                    view.model_focus
                        .retain(|id, _| catalog.models.iter().any(|model| &model.id == id));
                    for model in &catalog.models {
                        view.model_focus
                            .entry(model.id.clone())
                            .or_insert_with(|| cx.focus_handle().tab_index(0).tab_stop(true));
                    }
                }
                view.pending_focus = Some(if view.actions.preview.is_some() {
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
        .min_h(px(if self.compact { 40.0 } else { 36.0 }))
        .py(px(6.0))
        .whitespace_normal()
        .when(
            enabled && matches!(action, Action::Run(JobKind::Apply)),
            |button| {
                button
                    .bg(rgb(palette.accent))
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
            .child(text!(model.to_owned()))
    }

    fn connection_controls(&self, palette: Palette, cx: &mut Context<Self>) -> Div {
        let state_label = match self.actions.ownership {
            None => A::Unknown,
            Some(Ownership::Unmanaged) => A::Unmanaged,
            Some(Ownership::Connected) => A::Connected,
            Some(Ownership::Drifted) => A::Drifted,
            Some(Ownership::RecoveryRequired) => A::RecoveryRequired,
        };
        let mut group = div().w_full().min_w_0().flex().flex_col().gap(px(12.0));
        group = group.child(
            div()
                .flex()
                .flex_col()
                .gap(px(6.0))
                .child(
                    div()
                        .flex()
                        .flex_wrap()
                        .items_center()
                        .justify_between()
                        .gap(px(8.0))
                        .child(
                            div()
                                .id("ownership-status")
                                .role(Role::Status)
                                .min_w_0()
                                .flex_1()
                                .text_size(px(13.0))
                                .font_weight(FontWeight::MEDIUM)
                                .whitespace_normal()
                                .child(text!(format!(
                                    "{} · {}",
                                    self.locale.action_text(A::Ownership),
                                    self.locale.action_text(state_label)
                                ))),
                        )
                        .child(self.action_button(
                            "check-connection",
                            A::StatusRefresh,
                            Action::CheckConnection,
                            palette,
                            cx,
                        )),
                )
                .child(self.action_caption(A::OwnershipHint, palette)),
        );
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
        } else if self.check_after_refresh
            || self.actions.reconcile_needed()
            || (matches!(
                self.actions.notice,
                Some(Notice::Connected | Notice::Disconnected(_))
            ) && self.actions.snapshot_failed())
        {
            group = group.child(self.action_message(
                "reconcile-status",
                self.locale.action_text(A::Checking),
                true,
                palette,
            ));
        } else if let Some(notice) = &self.actions.notice {
            let key = match notice {
                Notice::Connected => Some(A::ConnectDone),
                Notice::Disconnected(_) => Some(A::DisconnectDone),
                Notice::StatusVerified => Some(A::Verified),
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
        if self
            .actions
            .ownership
            .is_some_and(|state| state != Ownership::Unmanaged)
        {
            group = group.child(self.action_caption(A::ManagedHint, palette));
            if self.actions.disconnect_confirmation {
                group = group.child(
                    div()
                        .w_full()
                        .min_w_0()
                        .p(px(12.0))
                        .rounded(px(6.0))
                        .border_1()
                        .border_color(rgb(palette.warning))
                        .flex()
                        .flex_col()
                        .gap(px(10.0))
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
            } else {
                group = group.child(div().flex().child(self.action_button(
                    "disconnect",
                    A::Disconnect,
                    Action::ConfirmDisconnect,
                    palette,
                    cx,
                )));
            }
        }
        group = group.child(div().h(px(1.0)).bg(rgb(palette.border)));
        group = group.child(
            div()
                .w_full()
                .min_w_0()
                .flex()
                .flex_col()
                .gap(px(6.0))
                .child(self.action_caption(A::Gateway, palette))
                .child(
                    div()
                        .w_full()
                        .min_w_0()
                        .flex()
                        .gap(px(8.0))
                        .when(self.compact, |row| row.flex_col())
                        .child(
                            div()
                                .id("gateway-field")
                                .flex_1()
                                .min_w_0()
                                .anchor_scroll(self.scroll_anchors.get("gateway-address").cloned())
                                .child(self.gateway_input.clone()),
                        )
                        .child(self.action_button(
                            "query-models",
                            A::Query,
                            Action::Run(JobKind::Inspect),
                            palette,
                            cx,
                        )),
                )
                .child(self.action_caption(A::GatewayHint, palette)),
        );
        if let Some(catalog) = &self.actions.catalog {
            if !self.actions.catalog_is_current() {
                group = group.child(self.action_message(
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
            group = group.child(
                div()
                    .w_full()
                    .min_w_0()
                    .flex()
                    .flex_col()
                    .gap(px(6.0))
                    .child(self.action_caption(A::Search, palette))
                    .child(
                        div()
                            .id("search-field")
                            .w_full()
                            .min_w_0()
                            .anchor_scroll(self.scroll_anchors.get("model-search").cloned())
                            .child(self.search_input.clone()),
                    ),
            );
            group = group.child(self.action_caption(A::HealthUnprobed, palette));
            group = group.child(
                div()
                    .text_size(px(12.0))
                    .text_color(rgb(palette.muted))
                    .child(text!(format!(
                        "{} · {}",
                        self.locale.action_text(A::Configured),
                        catalog.models.len()
                    ))),
            );
            let models = self.actions.visible_models();
            if models.is_empty() {
                group = group.child(self.action_caption(
                    if catalog.models.is_empty() {
                        A::NoModels
                    } else {
                        A::NoMatches
                    },
                    palette,
                ));
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
                group = group.child(
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
        } else {
            group = group.child(self.action_caption(A::NoCatalog, palette));
        }
        if let Some(model) = &self.actions.selected {
            group = group.child(
                div()
                    .w_full()
                    .min_w_0()
                    .text_size(px(12.0))
                    .line_height(px(18.0))
                    .whitespace_normal()
                    .child(text!(format!(
                        "{} · {model}",
                        self.locale.action_text(A::SelectedModel)
                    ))),
            );
            if !self.actions.selected_available() {
                group = group.child(self.action_message(
                    "selected-model-unavailable",
                    self.locale.action_text(A::SelectedMissing),
                    true,
                    palette,
                ));
            } else if !cli_bridge::writable_model(model) {
                group = group.child(self.action_message(
                    "model-write-limit",
                    self.locale.action_text(A::UnicodeModel),
                    true,
                    palette,
                ));
            }
        }
        if let Some(ticket) = &self.actions.preview {
            let mut preview = div()
                .w_full()
                .min_w_0()
                .p(px(12.0))
                .rounded(px(6.0))
                .border_1()
                .border_color(rgb(palette.accent))
                .bg(rgb(palette.selected))
                .flex()
                .flex_col()
                .gap(px(10.0))
                .child(
                    div()
                        .font_weight(FontWeight::SEMIBOLD)
                        .child(text!(self.locale.action_text(A::PreviewTitle))),
                )
                .child(self.action_caption(A::PreviewHint, palette))
                .child(self.field(Text::Model, ticket.binding.model.clone(), palette, false))
                .child(
                    div()
                        .w_full()
                        .min_w_0()
                        .whitespace_normal()
                        .child(text!(format!(
                            "{} · {}",
                            self.locale.action_text(A::Gateway),
                            ticket.binding.address
                        ))),
                )
                .child(self.field(
                    Text::ConfigPath,
                    ticket.binding.config.display().to_string(),
                    palette,
                    false,
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
            group = group.child(preview);
        } else {
            group = group.child(div().flex().child(self.action_button(
                "preview-connect",
                A::Preview,
                Action::Run(JobKind::Preview),
                palette,
                cx,
            )));
        }
        group
    }
}
