//! Compact native Gateway controls. The model stores no PID and never presents
//! a listening port as ownership. Address edits have an explicit save boundary.

use std::{
    collections::BTreeMap,
    net::SocketAddr,
    sync::{Arc, atomic::AtomicBool},
    time::Duration,
};

use gpui::{
    AccessibleAction, App, ClipboardItem, Context, Div, Entity, EventEmitter, FocusHandle,
    FontWeight, IntoElement, KeyDownEvent, Render, Role, ScrollAnchor, ScrollHandle, Stateful,
    Subscription, Task, Window, div, prelude::*, px, rgb, text,
};

use crate::{
    actions_text::error_text,
    cli_bridge::{
        self,
        owned_gateway::{GatewayFailure, GatewayState, OwnedGateway},
    },
    gateway_text::GatewayText as G,
    localization::Locale,
    text_input::{InputEvent, TextInput},
    theme::{Appearance, Palette},
};

#[derive(Clone, Copy)]
enum Action {
    Start,
    Stop,
    Check,
    SaveAddress,
    Copy,
}

pub(crate) enum GatewayEvent {
    SaveAddress(SocketAddr),
    Changed,
}

pub struct GatewayPage {
    locale: Locale,
    appearance: Appearance,
    address: SocketAddr,
    address_input: Entity<TextInput>,
    initialized: bool,
    closing: bool,
    preferences_writable: bool,
    saving: Option<SocketAddr>,
    save_notice: Option<bool>,
    controller: Option<OwnedGateway>,
    task: Option<Task<()>>,
    state: Arc<GatewayState>,
    focus: FocusHandle,
    buttons: BTreeMap<&'static str, FocusHandle>,
    scroll: ScrollHandle,
    anchors: BTreeMap<&'static str, ScrollAnchor>,
    copied: bool,
    _subscription: Subscription,
}

impl EventEmitter<GatewayEvent> for GatewayPage {}

impl GatewayPage {
    pub(crate) fn new(cx: &mut Context<Self>) -> Self {
        let locale = Locale::system();
        let address = magpie_core::gateway::default_gateway_addr();
        let address_input = cx.new(|cx| {
            TextInput::new(
                "gateway-listen-address",
                &address.to_string(),
                "127.0.0.1:3425",
                locale.gateway_text(G::Address),
                cx,
            )
        });
        let subscription = cx.subscribe(&address_input, |view, _, event, cx| {
            if *event == InputEvent::Changed {
                view.save_notice = None;
            }
            cx.notify();
        });
        let scroll = ScrollHandle::new();
        let mut buttons = BTreeMap::new();
        let mut anchors = BTreeMap::new();
        for id in [
            "gateway-primary",
            "gateway-check",
            "gateway-save-address",
            "gateway-copy",
        ] {
            buttons.insert(id, cx.focus_handle().tab_index(0).tab_stop(true));
            anchors.insert(id, ScrollAnchor::for_handle(scroll.clone()));
        }
        anchors.insert(
            "gateway-listen-address",
            ScrollAnchor::for_handle(scroll.clone()),
        );
        Self {
            locale,
            appearance: Appearance::System,
            address,
            address_input,
            initialized: false,
            closing: false,
            preferences_writable: false,
            saving: None,
            save_notice: None,
            controller: None,
            task: None,
            state: Arc::new(GatewayState::Stopped),
            focus: cx.focus_handle(),
            buttons,
            scroll,
            anchors,
            copied: false,
            _subscription: subscription,
        }
    }

    pub(crate) fn configure(
        &mut self,
        address: SocketAddr,
        autostart: bool,
        cx: &mut Context<Self>,
    ) {
        if self.initialized {
            // A changed file can be reloaded while an owned process is alive.
            // Keep both the runtime address and its input stable until stopped;
            // the parent reapplies the saved preference after lease cleanup.
            if self.pending()
                || matches!(
                    &*self.state,
                    GatewayState::Failed(GatewayFailure::CleanupPending)
                )
            {
                return;
            }
            if self.address != address {
                let clean = cli_bridge::parse_address(self.address_input.read(cx).text()).ok()
                    == Some(self.address);
                self.address = address;
                if clean {
                    self.address_input.update(cx, |input, cx| {
                        input.set_text(&address.to_string(), cx);
                    });
                }
                if !self.pending() {
                    self.controller = None;
                    self.state = Arc::new(GatewayState::Stopped);
                }
                cx.notify();
                cx.emit(GatewayEvent::Changed);
            }
            return;
        }
        self.initialized = true;
        self.address = address;
        self.address_input.update(cx, |input, cx| {
            input.set_text(&address.to_string(), cx);
        });
        if autostart {
            self.start(true, cx);
        }
        cx.notify();
    }

    pub(crate) fn set_presentation(
        &mut self,
        locale: Locale,
        appearance: Appearance,
        writable: bool,
        cx: &mut Context<Self>,
    ) {
        if self.locale != locale
            || self.appearance != appearance
            || self.preferences_writable != writable
        {
            self.locale = locale;
            self.appearance = appearance;
            self.preferences_writable = writable;
            cx.notify();
        }
    }

    pub(crate) fn pending(&self) -> bool {
        self.controller.as_ref().is_some_and(OwnedGateway::pending)
    }

    pub(crate) fn stop(&self) {
        if let Some(controller) = &self.controller {
            controller.stop();
        }
    }

    pub(crate) fn begin_closing(&mut self, cx: &mut Context<Self>) {
        self.closing = true;
        self.stop();
        cx.notify();
    }

    pub(crate) fn shutdown_pending(&self) -> Option<Arc<AtomicBool>> {
        self.controller.as_ref().map(OwnedGateway::shutdown_pending)
    }

    pub(crate) fn refresh(&mut self, cx: &mut Context<Self>) {
        if matches!(&*self.state, GatewayState::Owned { .. }) {
            if let Some(controller) = &self.controller {
                controller.refresh();
            }
        } else if self.can_check() {
            self.start(false, cx);
        }
    }

    pub(crate) fn address_saved(&mut self, success: bool, cx: &mut Context<Self>) {
        if let Some(address) = self.saving.take() {
            if success {
                self.address = address;
                self.address_input.update(cx, |input, cx| {
                    input.set_text(&address.to_string(), cx);
                });
                self.state = Arc::new(GatewayState::Stopped);
                self.controller = None;
                self.copied = false;
                cx.emit(GatewayEvent::Changed);
            }
            self.save_notice = Some(success);
        }
        cx.notify();
    }

    fn start(&mut self, start_if_absent: bool, cx: &mut Context<Self>) {
        if self.closing
            || self.pending()
            || self.task.is_some()
            || self.saving.is_some()
            || matches!(
                &*self.state,
                GatewayState::Failed(GatewayFailure::CleanupPending)
            )
        {
            return;
        }
        let (controller, runner) = OwnedGateway::begin(self.address, start_if_absent);
        self.state = controller.state();
        self.controller = Some(controller.clone());
        self.copied = false;
        let operation = cx.background_executor().spawn(runner.run());
        self.task = Some(cx.spawn(async move |this, cx| {
            let mut operation = std::pin::pin!(operation);
            loop {
                let complete = futures_lite::future::race(
                    async {
                        operation.as_mut().await;
                        true
                    },
                    async {
                        async_io::Timer::after(Duration::from_millis(150)).await;
                        false
                    },
                )
                .await;
                let state = controller.state();
                if this
                    .update(cx, |view, cx| {
                        if !Arc::ptr_eq(&view.state, &state) {
                            view.state = state;
                            cx.emit(GatewayEvent::Changed);
                            cx.notify();
                        }
                        if complete {
                            view.task = None;
                            cx.notify();
                        }
                    })
                    .is_err()
                {
                    controller.stop();
                    break;
                }
                if complete {
                    break;
                }
            }
        }));
        cx.emit(GatewayEvent::Changed);
        cx.notify();
    }

    fn can_check(&self) -> bool {
        !self.closing
            && !matches!(
                &*self.state,
                GatewayState::Checking
                    | GatewayState::Starting
                    | GatewayState::Stopping
                    | GatewayState::Owned {
                        refreshing: true,
                        ..
                    }
                    | GatewayState::Failed(GatewayFailure::CleanupPending)
            )
            && self.saving.is_none()
    }

    fn can_edit_address(&self) -> bool {
        !self.closing
            && !self.pending()
            && self.task.is_none()
            && self.saving.is_none()
            && !matches!(&*self.state, GatewayState::Owned { .. })
            && !matches!(
                &*self.state,
                GatewayState::Failed(GatewayFailure::CleanupPending)
            )
    }

    fn can_save(&self, cx: &App) -> bool {
        self.preferences_writable
            && self.can_edit_address()
            && cli_bridge::parse_address(self.address_input.read(cx).text())
                .is_ok_and(|address| address != self.address)
    }

    fn enabled(&self, action: Action, cx: &App) -> bool {
        if self.closing && !matches!(action, Action::Stop) {
            return false;
        }
        match action {
            Action::Start => {
                self.state.can_start()
                    && !self.pending()
                    && self.task.is_none()
                    && self.saving.is_none()
            }
            Action::Stop => self.state.can_stop(),
            Action::Check => self.can_check(),
            Action::SaveAddress => self.can_save(cx),
            Action::Copy => self.state.info().is_some(),
        }
    }

    fn act(&mut self, action: Action, cx: &mut Context<Self>) {
        if !self.enabled(action, cx) {
            return;
        }
        match action {
            Action::Start => self.start(true, cx),
            Action::Stop => self.stop(),
            Action::Check => self.refresh(cx),
            Action::SaveAddress => {
                if let Ok(address) = cli_bridge::parse_address(self.address_input.read(cx).text()) {
                    self.saving = Some(address);
                    self.save_notice = None;
                    cx.emit(GatewayEvent::SaveAddress(address));
                }
            }
            Action::Copy => {
                if let Some(info) = self.state.info() {
                    cx.write_to_clipboard(ClipboardItem::new_string(format!(
                        "http://{}/v1",
                        info.address
                    )));
                    self.copied = true;
                }
            }
        }
        cx.notify();
    }

    fn button(
        &self,
        id: &'static str,
        label: &str,
        action: Action,
        primary: bool,
        palette: Palette,
        cx: &mut Context<Self>,
    ) -> Stateful<Div> {
        let enabled = self.enabled(action, cx);
        let weak = cx.weak_entity();
        div()
            .id(id)
            .track_focus(&self.buttons[id].clone().tab_stop(enabled))
            .anchor_scroll(self.anchors.get(id).cloned())
            .tab_stop(enabled)
            .tab_index(0)
            .role(if enabled { Role::Button } else { Role::Status })
            .aria_label(label.to_owned())
            .h(px(34.0))
            .px(px(10.0))
            .flex_none()
            .flex()
            .items_center()
            .justify_center()
            .rounded(px(5.0))
            .border_2()
            .border_color(gpui::rgba(0))
            .bg(rgb(if primary {
                palette.selected
            } else {
                palette.surface
            }))
            .text_color(rgb(if primary {
                palette.accent
            } else {
                palette.text
            }))
            .text_size(px(13.0))
            .whitespace_nowrap()
            .when(!enabled, |button| button.opacity(0.5))
            .when(enabled, |button| {
                button
                    .cursor_pointer()
                    .hover(move |style| style.bg(rgb(palette.inset)))
                    .focus_visible(move |style| style.border_color(rgb(palette.accent)))
                    .on_click(cx.listener(move |view, _, _, cx| view.act(action, cx)))
                    .on_a11y_action(AccessibleAction::Click, move |_, _, cx| {
                        let _ = weak.update(cx, |view, cx| view.act(action, cx));
                    })
            })
            .child(text!(label.to_owned()))
    }

    fn key_down(&mut self, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        let key = &event.keystroke;
        if key.key == "tab" {
            if key.modifiers.shift {
                window.focus_prev(cx);
            } else {
                window.focus_next(cx);
            }
            if self
                .address_input
                .read(cx)
                .focus_handle()
                .is_focused(window)
            {
                self.anchors["gateway-listen-address"].scroll_to(window, cx);
            }
            for (id, handle) in &self.buttons {
                if handle.is_focused(window) {
                    self.anchors[id].scroll_to(window, cx);
                }
            }
            cx.stop_propagation();
        }
        // GPUI on_click handles Enter / Space on key release. Activating on
        // keydown too would start then immediately cancel the same service.
    }

    fn caption(&self, key: G, palette: Palette) -> Div {
        div()
            .text_size(px(12.0))
            .line_height(px(18.0))
            .text_color(rgb(palette.muted))
            .child(text!(self.locale.gateway_text(key)))
    }

    fn failure_text(&self, failure: GatewayFailure) -> &'static str {
        match failure {
            GatewayFailure::Bridge(kind) => self.locale.action_text(error_text(kind)),
            GatewayFailure::AddressInUse => self.locale.gateway_text(G::AddressConflict),
            GatewayFailure::IdentityMismatch => self.locale.gateway_text(G::IdentityMismatch),
            GatewayFailure::Exited => self.locale.gateway_text(G::Exited),
            GatewayFailure::CleanupPending => self.locale.gateway_text(G::CleanupPending),
        }
    }
}

impl Render for GatewayPage {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let palette = Palette::for_window(window, self.appearance);
        let compact = window.viewport_size().width < px(620.0);
        let edit_address = self.can_edit_address();
        self.address_input.update(cx, |input, cx| {
            input.set_presentation(
                "127.0.0.1:3425",
                self.locale.gateway_text(G::Address),
                palette,
                !edit_address,
                cx,
            )
        });
        let (label, description) = match &*self.state {
            GatewayState::Stopped => (G::Stopped, G::StartupHint),
            GatewayState::Checking => (G::Checking, G::StartupHint),
            GatewayState::Starting => (G::Starting, G::StartupHint),
            GatewayState::Stopping => (G::Stopping, G::OwnedHint),
            GatewayState::Owned {
                health_error: Some(_),
                ..
            } => (G::Failed, G::HealthFailed),
            GatewayState::Owned {
                refreshing: true, ..
            } => (G::Checking, G::OwnedHint),
            GatewayState::Owned { .. } => (G::Running, G::OwnedHint),
            GatewayState::External(_) => (G::External, G::ExternalHint),
            GatewayState::Failed(_) => (G::Failed, G::StartupHint),
        };
        let primary_action = if self.state.can_stop() {
            Action::Stop
        } else {
            Action::Start
        };
        let primary_label = if matches!(
            &*self.state,
            GatewayState::Checking | GatewayState::Starting
        ) {
            G::Cancel
        } else if self.state.can_stop() {
            G::Stop
        } else {
            G::Start
        };
        let show_primary = !matches!(&*self.state, GatewayState::External(_));
        if (!show_primary || !self.enabled(primary_action, cx))
            && self.buttons["gateway-primary"].is_focused(window)
            || !edit_address
                && self
                    .address_input
                    .read(cx)
                    .focus_handle()
                    .is_focused(window)
        {
            let handle = if self.enabled(Action::Check, cx) {
                self.buttons["gateway-check"].clone()
            } else {
                self.focus.clone()
            };
            window.on_next_frame(move |window, cx| handle.focus(window, cx));
        }
        let status_color = if matches!(
            &*self.state,
            GatewayState::Failed(_)
                | GatewayState::Owned {
                    health_error: Some(_),
                    ..
                }
        ) {
            palette.danger
        } else if matches!(&*self.state, GatewayState::Owned { .. }) {
            palette.accent
        } else {
            palette.muted
        };
        let info = self.state.info();
        let version = info
            .and_then(|info| info.server_version.as_deref())
            .unwrap_or(self.locale.gateway_text(G::Unknown));
        let endpoint = format!(
            "http://{}/v1",
            info.map_or(self.address, |info| info.address)
        );
        let metadata = |label: G, value: String| {
            div()
                .min_w_0()
                .flex_1()
                .flex()
                .flex_col()
                .gap(px(4.0))
                .child(self.caption(label, palette))
                .child(div().text_size(px(13.0)).child(text!(value)))
        };
        let card = div()
            .w_full()
            .flex()
            .flex_col()
            .rounded(px(7.0))
            .border_1()
            .border_color(rgb(palette.border))
            .bg(rgb(palette.surface))
            .child(
                div()
                    .p(px(16.0))
                    .flex()
                    .flex_wrap()
                    .items_center()
                    .gap(px(10.0))
                    .child(
                        div()
                            .flex_1()
                            .min_w(px(150.0))
                            .flex()
                            .flex_col()
                            .gap(px(6.0))
                            .child(
                                div()
                                    .text_size(px(14.0))
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .child(text!(self.locale.gateway_text(G::Title))),
                            )
                            .child(
                                div()
                                    .id("gateway-state")
                                    .role(Role::Status)
                                    .text_size(px(12.0))
                                    .text_color(rgb(status_color))
                                    .child(text!(self.locale.gateway_text(label))),
                            ),
                    )
                    .child(
                        div()
                            .flex()
                            .flex_wrap()
                            .gap(px(4.0))
                            .when(show_primary, |row| {
                                row.child(self.button(
                                    "gateway-primary",
                                    self.locale.gateway_text(primary_label),
                                    primary_action,
                                    true,
                                    palette,
                                    cx,
                                ))
                            })
                            .child(self.button(
                                "gateway-check",
                                self.locale.gateway_text(G::Check),
                                Action::Check,
                                false,
                                palette,
                                cx,
                            )),
                    ),
            )
            .child(
                div()
                    .px(px(16.0))
                    .pb(px(14.0))
                    .child(self.caption(description, palette)),
            )
            .when_some(
                match &*self.state {
                    GatewayState::Failed(error) => Some(*error),
                    _ => None,
                },
                |card, error| {
                    card.child(
                        div()
                            .id("gateway-error")
                            .mx(px(16.0))
                            .mb(px(12.0))
                            .p(px(10.0))
                            .rounded(px(5.0))
                            .bg(rgb(palette.danger_bg))
                            .text_color(rgb(palette.danger))
                            .text_size(px(12.0))
                            .line_height(px(18.0))
                            .role(Role::Alert)
                            .child(text!(self.failure_text(error))),
                    )
                },
            )
            .child(
                div()
                    .px(px(16.0))
                    .py(px(12.0))
                    .border_t_1()
                    .border_color(rgb(palette.border))
                    .flex()
                    .flex_wrap()
                    .items_center()
                    .gap(px(8.0))
                    .child(
                        div()
                            .flex_1()
                            .min_w(px(180.0))
                            .flex()
                            .flex_col()
                            .gap(px(4.0))
                            .child(self.caption(G::Endpoint, palette))
                            .child(div().text_size(px(13.0)).child(text!(endpoint))),
                    )
                    .child(
                        self.button(
                            "gateway-copy",
                            self.locale
                                .gateway_text(if self.copied { G::Copied } else { G::Copy }),
                            Action::Copy,
                            false,
                            palette,
                            cx,
                        ),
                    ),
            )
            .child(
                div()
                    .p(px(16.0))
                    .border_t_1()
                    .border_color(rgb(palette.border))
                    .flex()
                    .gap(px(16.0))
                    .child(metadata(G::ServiceVersion, version.to_owned()))
                    .child(metadata(
                        G::ConfiguredModels,
                        info.map(|info| info.models.len().to_string())
                            .unwrap_or_else(|| "—".into()),
                    )),
            )
            .child(
                div()
                    .px(px(16.0))
                    .pb(px(14.0))
                    .child(self.caption(G::ModelsHint, palette)),
            );
        let address = div()
            .w_full()
            .flex()
            .flex_col()
            .gap(px(10.0))
            .mt(px(22.0))
            .child(
                div()
                    .text_size(px(13.0))
                    .font_weight(FontWeight::SEMIBOLD)
                    .child(text!(self.locale.gateway_text(G::Address))),
            )
            .child(
                div()
                    .flex()
                    .gap(px(8.0))
                    .when(compact, |row| row.flex_col())
                    .child(
                        div()
                            .id("gateway-address-field")
                            .flex_1()
                            .min_w_0()
                            .anchor_scroll(self.anchors.get("gateway-listen-address").cloned())
                            .child(self.address_input.clone()),
                    )
                    .child(self.button(
                        "gateway-save-address",
                        self.locale.gateway_text(if self.saving.is_some() {
                            G::Saving
                        } else {
                            G::SaveAddress
                        }),
                        Action::SaveAddress,
                        false,
                        palette,
                        cx,
                    )),
            )
            .child(self.caption(
                if edit_address {
                    G::AddressHint
                } else {
                    G::AddressLocked
                },
                palette,
            ))
            .when(
                edit_address
                    && cli_bridge::parse_address(self.address_input.read(cx).text()).is_err(),
                |block| {
                    block.child(
                        div()
                            .id("gateway-address-invalid")
                            .role(Role::Alert)
                            .text_size(px(12.0))
                            .line_height(px(18.0))
                            .text_color(rgb(palette.danger))
                            .child(text!(self.locale.action_text(error_text(
                                cli_bridge::ErrorKind::InvalidAddress
                            )))),
                    )
                },
            )
            .when_some(self.save_notice, |block, saved| {
                block.child(
                    div()
                        .id("gateway-address-notice")
                        .role(if saved { Role::Status } else { Role::Alert })
                        .text_size(px(12.0))
                        .line_height(px(18.0))
                        .text_color(rgb(if saved { palette.muted } else { palette.danger }))
                        .child(text!(self.locale.gateway_text(if saved {
                            G::Saved
                        } else {
                            G::PreferenceError
                        }))),
                )
            });
        div()
            .id("gateway-page-content")
            .track_focus(&self.focus)
            .size_full()
            .min_h_0()
            .overflow_y_scroll()
            .track_scroll(&self.scroll)
            .on_key_down(cx.listener(Self::key_down))
            .px(px(if compact { 16.0 } else { 24.0 }))
            .pb(px(16.0))
            .text_color(rgb(palette.text))
            .child(div().w_full().max_w(px(820.0)).child(card).child(address))
    }
}

#[cfg(feature = "visual-test")]
impl GatewayPage {
    pub fn preview_fixture(&mut self, external: bool, failed: bool, cx: &mut Context<Self>) {
        // These are explicitly synthetic scene fixtures. Production startup and
        // supervision are validated separately with the actual CLI process.
        self.initialized = true;
        let info = cli_bridge::GatewayInfo {
            address: self.address,
            models: vec![cli_bridge::ModelInfo {
                id: "preview/model".into(),
            }],
            server_version: Some("0.2.0".into()),
            instance_id: Some("preview-scene".into()),
            config_source: Some("provider_store".into()),
        };
        self.state = Arc::new(if failed {
            GatewayState::Failed(GatewayFailure::AddressInUse)
        } else if external {
            GatewayState::External(info)
        } else {
            GatewayState::Owned {
                info,
                health_error: None,
                refreshing: false,
            }
        });
        cx.notify();
    }
}

#[cfg(all(test, feature = "visual-test"))]
mod tests {
    use super::*;
    use gpui::{AppContext, TestAppContext};

    #[gpui::test]
    fn closing_does_not_admit_a_new_gateway_after_previous_cleanup(cx: &mut TestAppContext) {
        let page = cx.new(GatewayPage::new);
        page.update(cx, |view, cx| {
            view.configure(magpie_core::gateway::default_gateway_addr(), false, cx);
            view.preferences_writable = true;
            assert!(view.enabled(Action::Start, cx));
            view.begin_closing(cx);
            // The gateway may finish before Provider / preference cleanup.
            view.controller = None;
            view.state = Arc::new(GatewayState::Stopped);
            for action in [Action::Start, Action::Check, Action::SaveAddress] {
                assert!(!view.enabled(action, cx));
                view.act(action, cx);
            }
            view.start(true, cx); // Internal call is gated as well as buttons.
            view.refresh(cx);
            assert!(view.task.is_none());
            assert!(view.controller.is_none());
        });
    }

    #[gpui::test]
    fn address_reload_waits_for_owned_cleanup_and_failed_save_retains_draft(
        cx: &mut TestAppContext,
    ) {
        let page = cx.new(GatewayPage::new);
        page.update(cx, |view, cx| {
            let old = magpie_core::gateway::default_gateway_addr();
            let next = "127.0.0.1:4321".parse().unwrap();
            view.configure(old, false, cx);
            let (owner, _unstarted_runner) = OwnedGateway::begin(old, true);
            view.controller = Some(owner);
            view.configure(next, false, cx);
            assert_eq!(view.address, old);
            assert_eq!(view.address_input.read(cx).text(), old.to_string());
            view.controller = None; // Model completed owned cleanup.
            view.configure(next, false, cx);
            assert_eq!(view.address, next);
            assert_eq!(view.address_input.read(cx).text(), next.to_string());
            view.preferences_writable = true;
            view.address_input.update(cx, |input, cx| {
                input.set_text("127.0.0.1:5432", cx);
            });
            view.act(Action::SaveAddress, cx);
            assert!(view.saving.is_some());
            view.address_saved(false, cx);
            assert_eq!(view.address, next);
            assert_eq!(view.address_input.read(cx).text(), "127.0.0.1:5432");
            assert_eq!(view.save_notice, Some(false));
            view.act(Action::SaveAddress, cx);
            view.address_saved(true, cx);
            assert_eq!(view.address.to_string(), "127.0.0.1:5432");
            assert_eq!(view.save_notice, Some(true));
            view.state = Arc::new(GatewayState::Failed(GatewayFailure::CleanupPending));
            view.configure(old, false, cx);
            assert_eq!(
                view.address.to_string(),
                "127.0.0.1:5432",
                "preference reload must not clear a cleanup quarantine"
            );
            assert!(!view.can_edit_address());
            assert!(!view.enabled(Action::Start, cx));
            assert!(!view.enabled(Action::SaveAddress, cx));
            assert!(!view.enabled(Action::Check, cx));
        });
    }
}
