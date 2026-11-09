//! Thin application shell: core navigation, preferences and owned page lifecycles.
use crate::{
    agent_connections::AgentConnections,
    extension_pages::ExtensionPages,
    gateway_page::{GatewayEvent, GatewayPage},
    localization::{Locale, Text},
    plugin_bundle,
    plugins_page::{PluginsPage, RegistryChanged},
    preferences::{self, Preferences},
    provider_text::ProviderText as P,
    providers::{ProviderEvent, ProvidersPage},
    theme::{self, Appearance, Palette},
};
use gpui::{
    App, Context, Div, Entity, FocusHandle, FontWeight, IntoElement, Render, Role, ScrollHandle,
    SharedString, Stateful, Subscription, Task, Window, div, prelude::*, px, rgb, svg, text,
};
use gpui_kit_material::{
    components::{
        ButtonSize, ButtonTone, ButtonVariant, IconButton, IconButtonVariant, MaterialButton,
        material_button,
    },
    icons::Symbol,
};
use std::{
    net::SocketAddr,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

#[derive(Clone, Copy, PartialEq, Eq)]
enum Page {
    Agents,
    Providers,
    Gateway,
    Plugins,
    Settings,
    Extension(u32),
}
#[derive(Clone, Copy)]
enum Action {
    Page(Page),
    Refresh,
    Language(Locale),
    Appearance(Appearance),
    SavePreferences,
    ReloadPreferences,
}
#[derive(Clone, Copy)]
enum Label {
    Providers,
    Gateway,
    Plugins,
    Navigation,
    Closing,
    Unsaved,
    Retry,
    Reload,
    RegistryUnavailable,
    Workspace,
    AgentsIntro,
    ProvidersIntro,
    GatewayIntro,
    PluginsIntro,
    SettingsIntro,
    AppearanceHint,
    LanguageHint,
}
fn label(locale: Locale, key: Label) -> &'static str {
    let labels = match key {
        Label::Workspace => [
            "Local workspace",
            "本地工作区",
            "本機工作區",
            "ローカルワークスペース",
            "Lokaler Arbeitsbereich",
        ],
        Label::AgentsIntro => [
            "Your tools. Your choice of model.",
            "为你的工具，选择合适的模型。",
            "為你的工具，選擇合適的模型。",
            "いつものツールに、使いたいモデルを。",
            "Deine Werkzeuge. Deine Modellwahl.",
        ],
        Label::ProvidersIntro => [
            "Manage the model services your Agents use.",
            "管理 Agent 使用的模型服务。",
            "管理 Agent 使用的模型服務。",
            "Agent が使うモデルサービスを管理します。",
            "Modelldienste für deine Agents verwalten.",
        ],
        Label::GatewayIntro => [
            "One local endpoint for your model services.",
            "一个本地地址，连接你的模型服务。",
            "一個本機位址，連接你的模型服務。",
            "モデルサービスをひとつのローカル接続先に。",
            "Ein lokaler Zugang zu deinen Modelldiensten.",
        ],
        Label::PluginsIntro => [
            "Choose the capabilities in your workspace.",
            "按需启用工作区的功能。",
            "依需要啟用工作區的功能。",
            "ワークスペースに必要な機能を選びます。",
            "Funktionen für deinen Arbeitsbereich auswählen.",
        ],
        Label::SettingsIntro => [
            "Make this workspace feel like yours.",
            "让工作区符合你的使用习惯。",
            "讓工作區符合你的使用習慣。",
            "使い慣れたワークスペースに整えます。",
            "Den Arbeitsbereich nach deinen Wünschen einrichten.",
        ],
        Label::AppearanceHint => [
            "Follow your device, or choose a light or dark theme.",
            "跟随系统，或选择喜欢的明暗主题。",
            "跟隨系統，或選擇喜歡的明暗主題。",
            "システム設定に合わせるか、明暗を選べます。",
            "Systemeinstellung übernehmen oder Hell/Dunkel wählen.",
        ],
        Label::LanguageHint => [
            "Change the interface language at any time.",
            "随时切换界面语言。",
            "隨時切換介面語言。",
            "表示言語はいつでも変更できます。",
            "Die Sprache der Oberfläche jederzeit ändern.",
        ],
        Label::Providers => ["Providers", "供应商", "供應商", "プロバイダー", "Anbieter"],
        Label::Gateway => ["Gateway", "网关", "閘道", "ゲートウェイ", "Gateway"],
        Label::Plugins => ["Plugins", "插件", "外掛程式", "プラグイン", "Plugins"],
        Label::Navigation => ["Navigation", "导航", "導覽", "ナビゲーション", "Navigation"],
        Label::Closing => [
            "Finishing pending operations…",
            "正在完成待处理操作…",
            "正在完成待處理操作…",
            "処理の完了を待っています…",
            "Laufende Vorgänge werden beendet…",
        ],
        Label::Unsaved => [
            "Unsaved preferences",
            "偏好尚未保存",
            "偏好尚未儲存",
            "設定は未保存です",
            "Ungespeicherte Einstellungen",
        ],
        Label::Retry => [
            "Save again",
            "重新保存",
            "重新儲存",
            "再保存",
            "Erneut speichern",
        ],
        Label::Reload => [
            "Reload preferences",
            "重新读取偏好",
            "重新讀取偏好",
            "設定を再読み込み",
            "Einstellungen neu laden",
        ],
        Label::RegistryUnavailable => [
            "Plugin state has not been read. Open Plugins to refresh.",
            "插件状态尚未读取，打开插件页刷新。",
            "外掛程式狀態尚未讀取，請開啟外掛程式頁重新整理。",
            "プラグインの状態をまだ読み込めていません。プラグイン画面で更新してください。",
            "Plugin-Status noch nicht gelesen. Zum Aktualisieren Plugins öffnen.",
        ],
    };
    labels[match locale {
        Locale::English => 0,
        Locale::SimplifiedChinese => 1,
        Locale::TraditionalChinese => 2,
        Locale::Japanese => 3,
        Locale::German => 4,
    }]
}

/// Hover labels for the collapsed rail use the same localized destination names as ARIA.
struct NavigationTooltip {
    label: SharedString,
    palette: Palette,
}
impl Render for NavigationTooltip {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div()
            .id("navigation-tooltip")
            .role(Role::Tooltip)
            .px(px(8.0))
            .py(px(6.0))
            .rounded(px(6.0))
            .text_size(px(12.0))
            .text_color(rgb(self.palette.background))
            .bg(rgb(self.palette.text))
            .child(text!(self.label.clone()))
    }
}

/// Native application root with vendor-independent pages and explicit shutdown ownership.
pub struct MagpieWindow {
    page: Page,
    locale: Locale,
    appearance: Appearance,
    connections: Entity<AgentConnections>,
    providers: Entity<ProvidersPage>,
    gateway: Entity<GatewayPage>,
    plugins: Entity<PluginsPage>,
    extensions: ExtensionPages,
    address: SocketAddr,
    gateway_configured: bool,
    normal_runtime: bool,
    preferences_ready: bool,
    preferences_snapshot: Option<Preferences>,
    preferences_error: Option<&'static str>,
    preferences_dirty: bool,
    preferences_task: Option<Task<()>>,
    preferences_pending: Arc<AtomicBool>,
    pending_address: Option<SocketAddr>,
    focus: FocusHandle,
    settings_scroll: ScrollHandle,
    focus_after_navigation: bool,
    close_handler_installed: bool,
    close_requested: bool,
    gateway_closing_started: bool,
    close_discard_confirmed: bool,
    discard_prompt: bool,
    _subscriptions: Vec<Subscription>,
}
impl MagpieWindow {
    /// Starts preferences and plugin discovery; Gateway autostart waits for valid preferences.
    pub fn new(cx: &mut Context<Self>) -> Self {
        Self::build(true, cx)
    }
    /// Creates a non-running root; no preferences, registry watch or Gateway startup occurs.
    #[cfg(feature = "visual-test")]
    pub fn new_for_preview(cx: &mut Context<Self>) -> Self {
        Self::build(false, cx)
    }
    fn build(normal_runtime: bool, cx: &mut Context<Self>) -> Self {
        let connections = cx.new(AgentConnections::new);
        #[cfg(feature = "visual-test")]
        let providers = if normal_runtime {
            cx.new(ProvidersPage::new)
        } else {
            cx.new(ProvidersPage::new_for_preview)
        };
        #[cfg(not(feature = "visual-test"))]
        let providers = cx.new(ProvidersPage::new);
        let gateway = cx.new(GatewayPage::new);
        let plugins = cx.new(|cx| PluginsPage::new(plugin_bundle::load_catalog, cx));
        let mut subscriptions = vec![
            cx.observe(&connections, |_, _, cx| cx.notify()),
            cx.observe(&providers, |_, _, cx| cx.notify()),
            cx.observe(&gateway, |_, _, cx| cx.notify()),
            cx.observe(&plugins, |_, _, cx| cx.notify()),
        ];
        subscriptions.push(
            cx.subscribe(&plugins, |view, plugins, _: &RegistryChanged, cx| {
                if !view.normal_runtime || view.close_requested {
                    cx.notify();
                    return;
                }
                let catalog = plugins.read(cx).catalog().cloned();
                plugin_bundle::reconcile(&mut view.extensions, catalog.as_ref(), cx);
                let providers = catalog
                    .as_ref()
                    .map(|c| c.contributions.providers.clone())
                    .unwrap_or_default();
                let agents = catalog
                    .as_ref()
                    .map(|c| c.contributions.agents.clone())
                    .unwrap_or_default();
                view.providers
                    .update(cx, |page, cx| page.set_presets(providers, cx));
                view.connections
                    .update(cx, |page, cx| page.set_adapters(agents, cx));
                if let Page::Extension(id) = view.page
                    && view.extensions.get(id).is_none()
                {
                    view.page = Page::Agents;
                    view.focus_after_navigation = true;
                }
                cx.notify();
            }),
        );
        subscriptions.push(cx.subscribe(&providers, |view, _, event, cx| {
            match event {
                ProviderEvent::KeepEditing => {
                    if view.close_requested && !view.gateway_closing_started {
                        view.close_requested = false;
                        view.close_discard_confirmed = false;
                        view.discard_prompt = false;
                        if view.normal_runtime {
                            // Resuming also refreshes changes observed while the close gate was held.
                            view.plugins
                                .update(cx, |page, cx| page.set_closing(false, cx));
                        }
                    }
                }
                ProviderEvent::DiscardWindow => {
                    if view.close_requested {
                        view.close_discard_confirmed = true;
                        view.discard_prompt = false;
                        view.advance_close(cx);
                    }
                }
            }
            cx.notify();
        }));
        subscriptions.push(cx.subscribe(&gateway, |view, _, event, cx| {
            match event {
                GatewayEvent::Changed => cx.notify(),
                GatewayEvent::SaveAddress(address) => {
                    if !view.normal_runtime
                        || !view.preferences_ready
                        || view.preferences_task.is_some()
                        || view.close_requested
                    {
                        view.gateway
                            .update(cx, |page, cx| page.address_saved(false, cx));
                    } else {
                        view.pending_address = Some(*address);
                        view.save_preferences(cx);
                    }
                }
            }
            cx.notify();
        }));
        subscriptions.push(cx.on_app_quit(|view, cx| {
            view.close_requested = true;
            view.plugins
                .update(cx, |page, cx| page.set_closing(true, cx));
            view.cancel_waiters(cx);
            view.gateway_closing_started = true;
            view.gateway.update(cx, |page, cx| page.begin_closing(cx));
            let mut flags = view.waiter_flags(cx);
            if let Some(flag) = view.gateway.read(cx).shutdown_pending() {
                flags.push(flag);
            }
            async move {
                // GPUI bounds direct app quit to its 200ms budget. Normal close waits fully below.
                while flags.iter().any(|flag| flag.load(Ordering::Acquire)) {
                    async_io::Timer::after(Duration::from_millis(10)).await;
                }
            }
        }));
        let mut view = Self {
            page: Page::Agents,
            locale: Locale::system(),
            appearance: Appearance::System,
            connections,
            providers,
            gateway,
            plugins,
            extensions: ExtensionPages::default(),
            address: magpie_core::gateway::default_gateway_addr(),
            gateway_configured: false,
            normal_runtime,
            preferences_ready: !normal_runtime,
            preferences_snapshot: None,
            preferences_error: None,
            preferences_dirty: false,
            preferences_task: None,
            preferences_pending: Arc::new(AtomicBool::new(false)),
            pending_address: None,
            focus: cx.focus_handle(),
            settings_scroll: ScrollHandle::new(),
            focus_after_navigation: false,
            close_handler_installed: false,
            close_requested: false,
            gateway_closing_started: false,
            close_discard_confirmed: false,
            discard_prompt: false,
            _subscriptions: subscriptions,
        };
        if normal_runtime {
            view.load_preferences(cx);
            view.plugins.update(cx, |page, cx| {
                page.refresh(cx);
                page.watch(cx);
            });
        }
        view
    }
    /// Restores root keyboard focus without selecting a vendor-specific control.
    pub fn focus(&self, window: &mut Window, cx: &mut App) {
        if let Page::Extension(id) = self.page
            && let Some(page) = self.extensions.get(id)
        {
            (page.focus)(window, cx);
        } else {
            self.focus.focus(window, cx);
        }
    }
    fn waiter_flags(&self, cx: &App) -> Vec<Arc<AtomicBool>> {
        let mut flags = vec![
            self.connections.read(cx).shutdown_pending(),
            self.providers.read(cx).shutdown_pending(),
            self.plugins.read(cx).shutdown_pending(),
            self.preferences_pending.clone(),
        ];
        flags.extend(self.extensions.pending_flags());
        flags
    }
    fn cancel_waiters(&self, cx: &mut App) {
        self.connections.read(cx).cancel();
        self.providers.read(cx).cancel();
        self.plugins.read(cx).cancel();
        self.extensions.cancel_all(cx);
    }
    fn begin_close(&mut self, cx: &mut Context<Self>) {
        if !self.close_requested {
            self.close_requested = true;
            self.plugins
                .update(cx, |page, cx| page.set_closing(true, cx));
            self.cancel_waiters(cx);
        }
        self.advance_close(cx);
        cx.notify();
    }
    fn advance_close(&mut self, cx: &mut Context<Self>) {
        // Keep the owned gateway and its lease alive throughout draft confirmation.
        // Cancelled editor/configuration work must finish before inspecting its draft.
        if !self.close_requested || self.gateway_closing_started || self.waiters_pending(cx) {
            return;
        }
        if !self.close_discard_confirmed && self.providers.read(cx).has_unsaved_changes(cx) {
            if !self.discard_prompt {
                self.discard_prompt = true;
                self.page = Page::Providers;
                self.providers
                    .update(cx, |page, cx| page.request_window_discard(cx));
            }
        } else {
            self.gateway_closing_started = true;
            self.gateway.update(cx, |page, cx| page.begin_closing(cx));
        }
    }
    fn waiters_pending(&self, cx: &App) -> bool {
        self.preferences_task.is_some()
            || self.connections.read(cx).busy()
            || self.providers.read(cx).busy()
            || self.plugins.read(cx).busy()
            || self.extensions.pending()
            || self
                .waiter_flags(cx)
                .iter()
                .any(|flag| flag.load(Ordering::Acquire))
    }
    fn close_pending(&self, cx: &App) -> bool {
        self.waiters_pending(cx) || self.gateway.read(cx).pending()
    }
    fn configure(&mut self, address: SocketAddr, autostart: bool, cx: &mut Context<Self>) {
        self.address = address;
        self.gateway_configured = true;
        self.gateway.update(cx, |page, cx| {
            page.configure(
                address,
                autostart && self.normal_runtime && !self.close_requested,
                cx,
            )
        });
        self.connections
            .update(cx, |page, cx| page.configure(address, cx));
    }
    fn load_preferences(&mut self, cx: &mut Context<Self>) {
        if !self.normal_runtime || self.preferences_task.is_some() || self.close_requested {
            return;
        }
        self.preferences_ready = false;
        self.preferences_pending.store(true, Ordering::Release);
        let pending = self.preferences_pending.clone();
        let read = cx.background_executor().spawn(async move {
            let result = preferences::path().and_then(|path| preferences::load(&path));
            pending.store(false, Ordering::Release);
            result
        });
        self.preferences_task = Some(cx.spawn(async move |this, cx| {
            let result = read.await;
            let _ = this.update(cx, |view, cx| {
                view.preferences_task = None;
                view.preferences_ready = true;
                match result {
                    Ok(value) => {
                        let values = value.as_ref().map(Preferences::values).transpose();
                        let address = value.as_ref().map(Preferences::gateway_address).transpose();
                        match (values, address) {
                            (Ok(values), Ok(address)) => {
                                if let Some((locale, appearance)) = values {
                                    view.locale = locale;
                                    view.appearance = appearance;
                                }
                                let first = !view.gateway_configured;
                                view.configure(
                                    address
                                        .unwrap_or_else(magpie_core::gateway::default_gateway_addr),
                                    first,
                                    cx,
                                );
                                view.preferences_snapshot = value;
                                view.preferences_error = None;
                                view.preferences_dirty = false;
                            }
                            _ => {
                                view.preferences_error = Some("preferences_invalid");
                                if !view.gateway_configured {
                                    view.configure(view.address, false, cx);
                                }
                            }
                        }
                    }
                    Err(error) => {
                        view.preferences_error = Some(error);
                        if !view.gateway_configured {
                            view.configure(view.address, false, cx);
                        }
                    }
                }
                cx.notify();
            });
        }));
    }
    fn save_preferences(&mut self, cx: &mut Context<Self>) {
        if !self.normal_runtime
            || !self.preferences_ready
            || self.preferences_task.is_some()
            || self.close_requested
        {
            return;
        }
        let address = self.pending_address.unwrap_or(self.address);
        let value = Preferences::new(self.locale, self.appearance).with_gateway_address(address);
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
                        if view.pending_address.take().is_some() {
                            view.gateway
                                .update(cx, |page, cx| page.address_saved(true, cx));
                            view.configure(address, false, cx);
                        }
                        view.preferences_snapshot = Some(saved);
                        view.preferences_error = None;
                        view.preferences_dirty = false;
                    }
                    Err(error) => {
                        if view.pending_address.take().is_some() {
                            view.gateway
                                .update(cx, |page, cx| page.address_saved(false, cx));
                        }
                        view.preferences_error = Some(error);
                        view.preferences_dirty = true;
                    }
                }
                cx.notify();
            });
        }));
        cx.notify();
    }
    fn act(&mut self, action: Action, window: &mut Window, cx: &mut Context<Self>) {
        if self.close_requested {
            return;
        }
        match action {
            Action::Page(page) => {
                let entering_gateway = self.page != Page::Gateway && page == Page::Gateway;
                self.page = page;
                self.focus_after_navigation = true;
                if self.normal_runtime {
                    if let Page::Extension(id) = page
                        && let Some(page) = self.extensions.get(id)
                    {
                        (page.refresh)(cx);
                    }
                    if entering_gateway {
                        self.gateway.update(cx, |page, cx| page.refresh(cx));
                    }
                }
            }
            Action::Refresh if self.normal_runtime => {
                if self.page == Page::Agents {
                    self.connections.update(cx, |page, cx| page.refresh(cx));
                }
                self.gateway.update(cx, |page, cx| page.refresh(cx));
                self.plugins.update(cx, |page, cx| page.refresh(cx));
                self.providers.update(cx, |page, cx| page.refresh(cx));
                if let Page::Extension(id) = self.page
                    && let Some(page) = self.extensions.get(id)
                {
                    (page.refresh)(cx);
                }
            }
            Action::Language(locale)
                if self.preferences_ready && self.preferences_task.is_none() =>
            {
                self.locale = locale;
                self.preferences_dirty = true;
                self.save_preferences(cx);
            }
            Action::Appearance(appearance)
                if self.preferences_ready && self.preferences_task.is_none() =>
            {
                self.appearance = appearance;
                self.preferences_dirty = true;
                self.save_preferences(cx);
            }
            Action::SavePreferences => self.save_preferences(cx),
            Action::ReloadPreferences => self.load_preferences(cx),
            _ => {}
        }
        if self.focus_after_navigation {
            self.focus_after_navigation = false;
            self.focus(window, cx);
        }
        cx.notify();
    }
    fn title(&self, page: Page) -> SharedString {
        match page {
            Page::Agents => self.locale.text(Text::Agents).into(),
            Page::Providers => label(self.locale, Label::Providers).into(),
            Page::Gateway => label(self.locale, Label::Gateway).into(),
            Page::Plugins => label(self.locale, Label::Plugins).into(),
            Page::Settings => self.locale.text(Text::Settings).into(),
            Page::Extension(id) => self
                .extensions
                .get(id)
                .map(|page| (page.title)(self.locale))
                .unwrap_or_default(),
        }
    }
    fn navigation(&self) -> Vec<(Page, SharedString, &'static str, SharedString)> {
        let mut pages = vec![
            (
                Page::Agents,
                "agents-page".into(),
                "terminal",
                self.title(Page::Agents),
            ),
            (
                Page::Providers,
                "providers-page".into(),
                "cloud",
                self.title(Page::Providers),
            ),
            (
                Page::Gateway,
                "gateway-page".into(),
                "hub",
                self.title(Page::Gateway),
            ),
        ];
        pages.extend(self.extensions.pages.iter().map(|page| {
            (
                Page::Extension(page.id),
                page.nav_id.clone(),
                page.icon,
                (page.title)(self.locale),
            )
        }));
        pages.extend([
            (
                Page::Plugins,
                "plugins-page".into(),
                "extension",
                self.title(Page::Plugins),
            ),
            (
                Page::Settings,
                "settings-page".into(),
                "settings",
                self.title(Page::Settings),
            ),
        ]);
        pages
    }
    fn button(
        &self,
        id: impl Into<SharedString>,
        label: impl Into<SharedString>,
        action: Action,
        enabled: bool,
        cx: &Context<Self>,
    ) -> MaterialButton {
        let label = label.into();
        let selected = match action {
            Action::Language(locale) => self.locale == locale,
            Action::Appearance(appearance) => self.appearance == appearance,
            Action::Page(page) => self.page == page,
            _ => false,
        };
        let button = material_button(
            id.into(),
            if selected {
                ButtonVariant::Tonal
            } else {
                ButtonVariant::Text
            },
        )
        .button_size(ButtonSize::Small)
        .rounded(px(8.0))
        .font(theme::ui_font(cx, self.locale))
        .toggle(matches!(
            action,
            Action::Language(_) | Action::Appearance(_) | Action::Page(_)
        ))
        .selected(selected)
        .disabled(!enabled || self.close_requested)
        .accessibility_label(label.clone())
        .on_click(cx.listener(move |view, _, window, cx| view.act(action, window, cx)))
        .child(text!(label));
        if let Action::Page(Page::Extension(id)) = action
            && let Some(page) = self.extensions.get(id)
        {
            button.track_focus(&page.navigation_focus)
        } else {
            button
        }
    }
    fn rail_icon(
        &self,
        page: Page,
        id: SharedString,
        icon: &'static str,
        title: SharedString,
        palette: Palette,
        cx: &Context<Self>,
    ) -> Stateful<Div> {
        let active = self.page == page;
        let button: MaterialButton = IconButton::new(
            id.clone(),
            Symbol::named(icon).expect("navigation icon is embedded"),
            title.clone(),
        )
        .button_size(ButtonSize::ExtraSmall)
        .size(px(36.0))
        .tone(ButtonTone::Neutral)
        .variant(if active {
            IconButtonVariant::Tonal
        } else {
            IconButtonVariant::Standard
        })
        .toggle(true)
        .selected(active)
        .disabled(self.close_requested)
        .on_click(cx.listener(move |view, _, window, cx| view.act(Action::Page(page), window, cx)))
        .into();
        let button = if let Page::Extension(id) = page
            && let Some(page) = self.extensions.get(id)
        {
            button.track_focus(&page.navigation_focus)
        } else {
            button
        };
        div()
            .id(SharedString::from(format!("rail-item:{id}")))
            .w_full()
            .h(px(40.0))
            .flex_none()
            .flex()
            .items_center()
            .justify_center()
            .tooltip(move |_, cx| {
                cx.new(|_| NavigationTooltip {
                    label: title.clone(),
                    palette,
                })
                .into()
            })
            .child(button)
    }
    fn settings(&self, palette: Palette, cx: &Context<Self>) -> Stateful<Div> {
        let editable = self.preferences_ready && self.preferences_task.is_none();
        let section = |title: &'static str, hint: &'static str| {
            div()
                .flex()
                .flex_col()
                .gap(px(7.0))
                .child(
                    div()
                        .text_size(px(15.0))
                        .font_weight(FontWeight::SEMIBOLD)
                        .child(text!(title)),
                )
                .child(
                    div()
                        .text_size(px(13.0))
                        .text_color(rgb(palette.muted))
                        .whitespace_normal()
                        .child(text!(hint)),
                )
        };
        div()
            .id("settings-scroll")
            .size_full()
            .min_h_0()
            .overflow_y_scroll()
            .track_scroll(&self.settings_scroll)
            .pb(px(32.0))
            .flex()
            .flex_col()
            .gap(px(32.0))
            .child(
                div()
                    .max_w(px(760.0))
                    .flex()
                    .flex_col()
                    .gap(px(18.0))
                    .child(section(
                        self.locale.text(Text::Appearance),
                        label(self.locale, Label::AppearanceHint),
                    ))
                    .child(
                        div().flex().flex_wrap().gap(px(8.0)).children(
                            [
                                (Appearance::System, Text::System, "theme-system"),
                                (Appearance::Light, Text::Light, "theme-light"),
                                (Appearance::Dark, Text::Dark, "theme-dark"),
                            ]
                            .map(|(appearance, key, id)| {
                                self.button(
                                    id,
                                    self.locale.text(key),
                                    Action::Appearance(appearance),
                                    editable,
                                    cx,
                                )
                                .icon(
                                    Symbol::named(match appearance {
                                        Appearance::System => "desktop_windows",
                                        Appearance::Light => "light_mode",
                                        Appearance::Dark => "dark_mode",
                                    })
                                    .expect("theme icon is embedded"),
                                )
                                .min_w(px(112.0))
                                .h(px(48.0))
                            }),
                        ),
                    ),
            )
            .child(
                div()
                    .max_w(px(760.0))
                    .border_t_1()
                    .border_color(rgb(palette.border))
                    .pt(px(28.0))
                    .flex()
                    .flex_col()
                    .gap(px(18.0))
                    .child(section(
                        self.locale.text(Text::Language),
                        label(self.locale, Label::LanguageHint),
                    ))
                    .child(
                        div()
                            .flex()
                            .flex_wrap()
                            .gap(px(8.0))
                            .children(Locale::ALL.map(|locale| {
                                self.button(
                                    locale.id(),
                                    locale.name(),
                                    Action::Language(locale),
                                    editable,
                                    cx,
                                )
                            })),
                    ),
            )
            .child(self.preference_status(palette))
            .when(
                self.preferences_error.is_some() || self.preferences_dirty,
                |column| {
                    column.child(
                        div()
                            .flex()
                            .flex_wrap()
                            .gap(px(8.0))
                            .child(self.button(
                                "preferences-retry",
                                label(self.locale, Label::Retry),
                                Action::SavePreferences,
                                editable,
                                cx,
                            ))
                            .child(self.button(
                                "preferences-reload",
                                label(self.locale, Label::Reload),
                                Action::ReloadPreferences,
                                self.preferences_task.is_none(),
                                cx,
                            )),
                    )
                },
            )
    }
    fn preference_status(&self, palette: Palette) -> Stateful<Div> {
        let message = if self.preferences_task.is_some() {
            self.locale.provider_text(P::PreferencesSaving)
        } else if self.preferences_error.is_some() {
            self.locale.provider_text(P::PreferenceError)
        } else if self.preferences_dirty {
            label(self.locale, Label::Unsaved)
        } else if self.preferences_snapshot.is_some() {
            self.locale.provider_text(P::SavedPreferences)
        } else {
            self.locale.provider_text(P::AutoSavePreferences)
        };
        div()
            .id("preferences-status")
            .role(Role::Status)
            .text_size(px(12.0))
            .text_color(rgb(if self.preferences_error.is_some() {
                palette.warning
            } else {
                palette.muted
            }))
            .child(text!(message))
    }
}
impl Render for MagpieWindow {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if !self.close_handler_installed {
            self.close_handler_installed = true;
            let weak = cx.entity().downgrade();
            window.on_window_should_close(cx, move |_, cx| {
                let _ = weak.update(cx, |view, cx| view.begin_close(cx));
                false
            });
        }
        if self.close_requested {
            self.advance_close(cx);
            if self.gateway_closing_started && !self.close_pending(cx) {
                window.remove_window();
            } else if !self.discard_prompt {
                window.request_animation_frame();
            }
        }
        if self.gateway_configured {
            self.gateway
                .update(cx, |page, cx| page.configure(self.address, false, cx));
        }
        // This helper changes Material globals only when the effective appearance changes.
        // Initialization belongs to the entrypoint, never to every render frame.
        theme::apply_material_theme(self.appearance, window, cx);
        let palette = Palette::for_window(window, self.appearance);
        self.connections.update(cx, |page, cx| {
            page.set_presentation(self.locale, self.appearance, cx)
        });
        self.providers.update(cx, |page, cx| {
            page.set_presentation(self.locale, self.appearance, cx)
        });
        self.gateway.update(cx, |page, cx| {
            page.set_presentation(
                self.locale,
                self.appearance,
                self.preferences_ready
                    && self.preferences_task.is_none()
                    && !self.close_requested
                    && self.normal_runtime,
                cx,
            )
        });
        self.plugins.update(cx, |page, cx| {
            page.set_presentation(self.locale, self.appearance, cx)
        });
        for page in &self.extensions.pages {
            (page.present)(self.locale, self.appearance, cx);
        }
        if self.focus_after_navigation {
            self.focus_after_navigation = false;
            self.focus(window, cx);
        }
        let width = window.viewport_size().width;
        let wide = width >= px(1040.0);
        let compact = width < px(600.0);
        let short = window.viewport_size().height < px(480.0);
        let gutter = if wide {
            32.0
        } else if compact {
            16.0
        } else {
            24.0
        };
        let pages = self.navigation();
        let brand = || {
            div()
                .flex()
                .items_center()
                .gap(px(10.0))
                .child(
                    svg()
                        .path("icons/magpie.svg")
                        .size(px(28.0))
                        .text_color(rgb(palette.text)),
                )
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .gap(px(2.0))
                        .child(
                            div()
                                .text_size(px(19.0))
                                .font_weight(FontWeight::SEMIBOLD)
                                .child(text!("Magpie")),
                        )
                        .child(
                            div()
                                .text_size(px(11.0))
                                .text_color(rgb(palette.muted))
                                .child(text!(label(self.locale, Label::Workspace))),
                        ),
                )
        };
        let mut root = div()
            .id("magpie-shell")
            .track_focus(&self.focus)
            .tab_group()
            .tab_stop(false)
            .size_full()
            .min_w_0()
            .flex()
            .font(theme::ui_font(cx, self.locale))
            .text_size(px(13.0))
            .text_color(rgb(palette.text))
            .bg(rgb(palette.background));
        if wide {
            let nav = |page, id: SharedString, icon, title: SharedString| {
                let active = self.page == page;
                self.button(id, title, Action::Page(page), true, cx)
                    .button_size(ButtonSize::Medium)
                    .icon(Symbol::named(icon).expect("navigation icon is embedded"))
                    .w_full()
                    .h(px(42.0))
                    .px(px(12.0))
                    .rounded(px(8.0))
                    .justify_start()
                    .text_size(px(13.0))
                    .font_weight(if active {
                        FontWeight::SEMIBOLD
                    } else {
                        FontWeight::NORMAL
                    })
                    .text_color(rgb(if active {
                        palette.accent
                    } else {
                        palette.muted
                    }))
            };
            root = root.child(
                div()
                    .id("workspace-navigation")
                    .role(Role::Navigation)
                    .aria_label(label(self.locale, Label::Navigation))
                    .w(px(216.0))
                    .h_full()
                    .flex_none()
                    .bg(rgb(palette.surface))
                    .border_r_1()
                    .border_color(rgb(palette.border))
                    .px(px(16.0))
                    .py(px(28.0))
                    .flex()
                    .flex_col()
                    .gap(px(32.0))
                    .child(div().px(px(8.0)).child(brand()))
                    .child(
                        div()
                            .id("navigation-items")
                            .flex_1()
                            .min_h_0()
                            .overflow_y_scroll()
                            .flex()
                            .flex_col()
                            .gap(px(5.0))
                            .children(pages.iter().filter(|p| p.0 != Page::Settings).map(
                                |(page, id, icon, title)| {
                                    nav(*page, id.clone(), *icon, title.clone())
                                },
                            )),
                    )
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .gap(px(14.0))
                            .child(nav(
                                Page::Settings,
                                "settings-page".into(),
                                "settings",
                                self.title(Page::Settings),
                            ))
                            .child(
                                div()
                                    .px(px(12.0))
                                    .text_size(px(11.0))
                                    .text_color(rgb(palette.muted))
                                    .child(text!(format!("Magpie  {}", env!("CARGO_PKG_VERSION")))),
                            ),
                    ),
            );
        } else if !compact {
            root = root.child(
                div()
                    .id("collapsed-navigation")
                    .role(Role::Navigation)
                    .aria_label(label(self.locale, Label::Navigation))
                    .w(px(64.0))
                    .h_full()
                    .flex_none()
                    .bg(rgb(palette.surface))
                    .border_r_1()
                    .border_color(rgb(palette.border))
                    .px(px(6.0))
                    .py(px(24.0))
                    .flex()
                    .flex_col()
                    .gap(px(26.0))
                    .child(
                        div().flex_none().flex().justify_center().child(
                            svg()
                                .path("icons/magpie.svg")
                                .size(px(27.0))
                                .text_color(rgb(palette.text)),
                        ),
                    )
                    .child(
                        div()
                            .id("collapsed-navigation-items")
                            .flex_1()
                            .min_h_0()
                            .overflow_y_scroll()
                            .flex()
                            .flex_col()
                            .gap(px(8.0))
                            .children(pages.iter().map(|(page, id, icon, title)| {
                                self.rail_icon(*page, id.clone(), icon, title.clone(), palette, cx)
                            })),
                    ),
            );
        } else {
            root = root.flex_col();
        }
        let intro: SharedString = match self.page {
            Page::Agents => label(self.locale, Label::AgentsIntro).into(),
            Page::Providers => label(self.locale, Label::ProvidersIntro).into(),
            Page::Gateway => label(self.locale, Label::GatewayIntro).into(),
            Page::Plugins => label(self.locale, Label::PluginsIntro).into(),
            Page::Settings => label(self.locale, Label::SettingsIntro).into(),
            Page::Extension(id) => self
                .extensions
                .get(id)
                .map(|page| (page.intro)(self.locale).into())
                .unwrap_or_default(),
        };
        let header = div()
            .px(px(gutter))
            .pt(px(if short {
                14.0
            } else if compact {
                24.0
            } else {
                36.0
            }))
            .pb(px(if short { 12.0 } else { 24.0 }))
            .flex_none()
            .flex()
            .items_start()
            .justify_between()
            .gap(px(12.0))
            .child(
                div()
                    .min_w_0()
                    .flex_1()
                    .flex()
                    .flex_col()
                    .gap(px(8.0))
                    .child(
                        div()
                            .text_size(px(if compact { 24.0 } else { 28.0 }))
                            .font_weight(FontWeight::SEMIBOLD)
                            .child(text!(self.title(self.page))),
                    )
                    .when(!short, |header| {
                        header.child(
                            div()
                                .text_size(px(13.0))
                                .text_color(rgb(palette.muted))
                                .whitespace_normal()
                                .child(text!(intro)),
                        )
                    }),
            )
            .child(
                IconButton::new(
                    "refresh",
                    Symbol::named("refresh").expect("refresh icon is embedded"),
                    self.locale.text(Text::Refresh),
                )
                .button_size(ButtonSize::Small)
                .size(px(40.0))
                .tone(ButtonTone::Neutral)
                .disabled(!self.normal_runtime || self.close_requested)
                .on_click(cx.listener(|view, _, window, cx| view.act(Action::Refresh, window, cx))),
            );
        let mut column = div()
            .flex_1()
            .min_w_0()
            .min_h_0()
            .flex()
            .flex_col()
            .child(header);
        let body = if self.close_requested && !self.discard_prompt {
            div().child(
                div()
                    .id("shell-closing")
                    .p(px(24.0))
                    .role(Role::Status)
                    .child(text!(label(self.locale, Label::Closing))),
            )
        } else {
            match self.page {
                Page::Agents => div().size_full().child(self.connections.clone()),
                Page::Providers => div().size_full().child(self.providers.clone()),
                Page::Gateway => div().size_full().child(self.gateway.clone()),
                Page::Plugins => div().size_full().child(self.plugins.clone()),
                Page::Settings => div().size_full().child(self.settings(palette, cx)),
                Page::Extension(id) => div()
                    .size_full()
                    .when_some(self.extensions.get(id), |body, page| {
                        body.child(page.view.clone())
                    }),
            }
        };
        column = column.child(
            div()
                .flex_1()
                .min_h_0()
                .min_w_0()
                .px(px(gutter))
                .pt(px(8.0))
                .child(body),
        );
        if self.plugins.read(cx).catalog().is_none() && self.normal_runtime {
            column = column.child(
                div()
                    .id("registry-unavailable")
                    .role(Role::Alert)
                    .px(px(16.0))
                    .py(px(6.0))
                    .text_size(px(12.0))
                    .text_color(rgb(palette.warning))
                    .child(text!(label(self.locale, Label::RegistryUnavailable))),
            );
        }
        root = root.child(column);
        if compact {
            root = root.child(
                div()
                    .id("compact-navigation")
                    .role(Role::Navigation)
                    .aria_label(label(self.locale, Label::Navigation))
                    .flex_none()
                    .w_full()
                    .min_h(px(60.0))
                    .bg(rgb(palette.surface))
                    .border_t_1()
                    .border_color(rgb(palette.border))
                    .px(px(6.0))
                    .py(px(8.0))
                    .flex()
                    .items_center()
                    .justify_between()
                    .gap(px(2.0))
                    .children(pages.iter().map(|(page, id, icon, title)| {
                        self.rail_icon(*page, id.clone(), icon, title.clone(), palette, cx)
                            .flex_1()
                            .min_w_0()
                    })),
            );
        }
        root
    }
}

#[cfg(feature = "visual-test")]
impl MagpieWindow {
    /// Installs deterministic scene data only from the caller's isolated fixture root.
    pub fn preview_fixture(
        &mut self,
        root: &std::path::Path,
        scene: &str,
        locale_code: &str,
        dark: bool,
        cx: &mut Context<Self>,
    ) {
        if self.normal_runtime || self.preferences_task.is_some() {
            return;
        }
        self.locale = match locale_code {
            "zh-CN" | "zh-cn" | "locale-zh-cn" => Locale::SimplifiedChinese,
            "zh-TW" | "zh-tw" | "locale-zh-tw" => Locale::TraditionalChinese,
            "ja" | "locale-ja" => Locale::Japanese,
            "de" | "locale-de" => Locale::German,
            _ => Locale::English,
        };
        self.appearance = if dark {
            Appearance::Dark
        } else {
            Appearance::Light
        };
        self.preferences_ready = true;
        self.preferences_error = None;
        self.preferences_dirty = false;
        self.preferences_snapshot =
            Some(Preferences::new(self.locale, self.appearance).with_gateway_address(self.address));
        let catalog = if root.is_absolute() {
            magpie_app_plugins::AppPlugins::load_at(
                &root.join("config/plugins"),
                &root.join("config/plugins.toml"),
                &[],
            )
            .ok()
            .and_then(|app| crate::plugin_catalog::Catalog::parse(app.to_json()).ok())
        } else {
            None
        };
        let presets = catalog
            .as_ref()
            .map(|c| c.contributions.providers.clone())
            .unwrap_or_default();
        self.providers.update(cx, |page, cx| {
            page.set_presentation(self.locale, self.appearance, cx);
            if root.is_absolute()
                && let Ok(store) = magpie_core::provider_store::ProviderStore::load(
                    &root.join("config/providers.toml"),
                )
            {
                page.preview_load(store, presets, scene == "provider-presets", cx);
            }
            page.preview_fixture(
                matches!(scene, "provider-editor" | "provider-credentials"),
                scene == "provider-credentials",
                false,
                cx,
            );
        });
        if let Some(catalog) = catalog {
            self.plugins
                .update(cx, |page, cx| page.preview_fixture(catalog, cx));
        }
        self.connections.update(cx, |page, cx| {
            page.set_presentation(self.locale, self.appearance, cx);
            page.preview_fixture(scene, cx);
        });
        self.gateway.update(cx, |page, cx| {
            page.set_presentation(self.locale, self.appearance, false, cx);
            page.preview_fixture(scene == "gateway-external", scene == "gateway-error", cx);
        });
        self.page = match scene {
            "providers" | "provider-editor" | "provider-credentials" | "provider-presets" => {
                Page::Providers
            }
            "settings" => Page::Settings,
            "gateway" | "gateway-external" | "gateway-error" => Page::Gateway,
            "plugins" => Page::Plugins,
            "usage" => plugin_bundle::preview_usage(&mut self.extensions, cx)
                .map(Page::Extension)
                .unwrap_or(Page::Agents),
            _ => Page::Agents,
        };
        cx.notify();
    }
}

#[cfg(all(test, feature = "visual-test"))]
mod tests {
    use super::*;
    use gpui::TestAppContext;
    use std::sync::atomic::AtomicU64;

    fn init(cx: &mut TestAppContext) {
        cx.update(|cx| {
            gpui::init(cx);
            gpui_kit_material::init(cx);
        });
    }

    fn isolated_root() -> std::path::PathBuf {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        std::env::temp_dir().join(format!(
            "magpie-shell-fixture-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ))
    }

    #[gpui::test]
    fn public_preview_scenes_do_not_start_runtime_or_write_state(cx: &mut TestAppContext) {
        init(cx);
        for (scene, expected) in [
            ("agents", Page::Agents),
            ("codex", Page::Agents),
            ("codex-update", Page::Agents),
            ("providers", Page::Providers),
            ("provider-editor", Page::Providers),
            ("provider-credentials", Page::Providers),
            ("provider-presets", Page::Providers),
            ("settings", Page::Settings),
            ("gateway", Page::Gateway),
            ("gateway-external", Page::Gateway),
            ("gateway-error", Page::Gateway),
            ("plugins", Page::Plugins),
            ("usage", Page::Extension(0)),
        ] {
            let root = isolated_root();
            assert!(!root.exists());
            let view = cx.new(MagpieWindow::new_for_preview);
            cx.run_until_parked();
            view.update(cx, |view, cx| {
                view.preview_fixture(&root, scene, "zh-CN", true, cx);
            });
            cx.run_until_parked();
            view.update(cx, |view, cx| {
                if matches!(expected, Page::Extension(_)) {
                    assert!(matches!(view.page, Page::Extension(_)));
                } else {
                    assert!(view.page == expected, "scene {scene}");
                }
                assert!(matches!(view.locale, Locale::SimplifiedChinese));
                assert!(matches!(view.appearance, Appearance::Dark));
                assert!(!view.normal_runtime);
                assert!(!view.gateway_configured);
                assert!(!view.gateway.read(cx).pending());
                assert!(view.preferences_task.is_none());
                assert!(!view.close_pending(cx));
                assert!(view.plugins.read(cx).catalog().is_some());
                assert!(!root.exists(), "preview must remain read-only");
            });
        }
    }

    #[gpui::test]
    fn close_waits_for_owned_work_and_provider_keep_editing_reopens_gate(cx: &mut TestAppContext) {
        init(cx);
        let view = cx.new(MagpieWindow::new_for_preview);
        cx.run_until_parked();
        view.update(cx, |view, cx| {
            view.preferences_pending.store(true, Ordering::Release);
            view.begin_close(cx);
            assert!(view.close_requested);
            assert!(view.close_pending(cx));
            assert!(!view.gateway_closing_started);
            assert!(!view.gateway.read(cx).closing_for_preview());
            view.providers
                .update(cx, |_, cx| cx.emit(ProviderEvent::KeepEditing));
        });
        cx.run_until_parked();
        view.update(cx, |view, cx| {
            assert!(!view.close_requested);
            assert!(!view.plugins.read(cx).busy());
            assert!(!view.close_discard_confirmed);
            view.preferences_pending.store(false, Ordering::Release);
            view.begin_close(cx);
            assert!(view.close_requested);
            assert!(!view.close_pending(cx));
            view.providers
                .update(cx, |_, cx| cx.emit(ProviderEvent::DiscardWindow));
        });
        cx.run_until_parked();
        view.update(cx, |view, _| assert!(view.close_discard_confirmed));
    }
    #[gpui::test]
    fn provider_keep_preserves_gateway_lease_until_explicit_discard(cx: &mut TestAppContext) {
        use crate::cli_bridge::owned_gateway::{GatewayState, OwnedGateway};
        use magpie_core::provider_store::{Provider, ProviderProtocol, ProviderStore};
        struct Fixture(std::path::PathBuf);
        impl Drop for Fixture {
            fn drop(&mut self) {
                let _ = std::fs::remove_dir_all(&self.0);
            }
        }
        init(cx);
        let fixture = Fixture(isolated_root());
        assert!(!fixture.0.exists());
        let path = fixture.0.join("config/providers.toml");
        ProviderStore::create(
            &path,
            Provider {
                id: "fixture-openai".into(),
                name: "Fixture service".into(),
                base_url: "http://127.0.0.1:43197/v1".into(),
                protocol: ProviderProtocol::OpenAiChat,
                api_key_env: None,
                credential_ref: None,
                models: vec!["fixture/model-a".into()],
                enabled: true,
            },
            0,
        )
        .expect("isolated public configuration");
        let original = std::fs::read(&path).expect("fixture bytes");
        let view = cx.new(MagpieWindow::new_for_preview);
        cx.run_until_parked();
        // An unpolled runner exercises the real stop signal and admission flag
        // without spawning a CLI or opening a socket. Process/reap tests live in the bridge.
        let (owner, _runner) = OwnedGateway::begin("127.0.0.1:43197".parse().unwrap(), true);
        let pending = owner.shutdown_pending();
        view.update(cx, |view, cx| {
            view.preview_fixture(&fixture.0, "provider-credentials", "en", false, cx);
            view.gateway
                .update(cx, |page, cx| page.preview_controller(owner.clone(), cx));
        });
        cx.run_until_parked();
        view.update(cx, |view, cx| {
            assert!(view.providers.read(cx).has_unsaved_changes(cx));
            view.begin_close(cx);
            assert!(view.discard_prompt);
            assert!(!view.gateway_closing_started);
            assert!(!view.gateway.read(cx).closing_for_preview());
            assert!(pending.load(Ordering::Acquire));
            assert!(matches!(&*owner.state(), GatewayState::Checking));
            view.providers
                .update(cx, |_, cx| cx.emit(ProviderEvent::KeepEditing));
        });
        cx.run_until_parked();
        view.update(cx, |view, cx| {
            assert!(!view.close_requested);
            assert!(!view.gateway.read(cx).closing_for_preview());
            assert!(view.providers.read(cx).has_unsaved_changes(cx));
            assert!(pending.load(Ordering::Acquire));
            assert!(matches!(&*owner.state(), GatewayState::Checking));
            view.begin_close(cx);
            assert!(view.discard_prompt);
            view.providers
                .update(cx, |_, cx| cx.emit(ProviderEvent::DiscardWindow));
        });
        cx.run_until_parked();
        view.update(cx, |view, cx| {
            assert!(view.gateway_closing_started);
            assert!(view.gateway.read(cx).closing_for_preview());
            assert!(matches!(&*owner.state(), GatewayState::Stopping));
            assert!(view.close_pending(cx), "stop must await lease cleanup");
            pending.store(false, Ordering::Release); // Synthetic runner completion.
            assert!(!view.close_pending(cx));
        });
        assert_eq!(
            std::fs::read(&path).unwrap(),
            original,
            "discard retains saved business data"
        );
    }
}
