use gpui::{
    AccessibleAction, App, ClipboardItem, Context, Div, ElementId, FocusHandle, FontWeight,
    IntoElement, KeyDownEvent, Render, Role, SharedString, Stateful, Task, Window, div, img,
    prelude::*, px, rgb, svg, text,
};
use magpie_core::agents::ConfigFileState;

use crate::{
    assets,
    localization::{Locale, Text},
    snapshot::{Agent, Snapshot},
    theme::{Appearance, Palette},
};

#[derive(Clone, Copy, PartialEq, Eq)]
enum Page {
    Agents,
    Settings,
}

#[derive(Clone, Copy)]
enum Action {
    Refresh,
    Page(Page),
    Details(usize),
    CopyPath(usize),
    LanguageMenu,
    Language(Locale),
    Appearance(Appearance),
}

/// A compact native view of installed agents and their local configuration.
pub struct MagpieWindow {
    snapshot: Option<Snapshot>,
    refresh_task: Option<Task<()>>,
    page: Page,
    locale: Locale,
    appearance: Appearance,
    details: Option<usize>,
    language_menu: bool,
    copied: Option<usize>,
    focus: FocusHandle,
    language_focus: FocusHandle,
}

impl MagpieWindow {
    /// Starts a background discovery without changing agent configuration.
    pub fn new(cx: &mut Context<Self>) -> Self {
        let mut view = Self {
            snapshot: None,
            refresh_task: None,
            page: Page::Agents,
            locale: Locale::system(),
            appearance: Appearance::System,
            details: None,
            language_menu: false,
            copied: None,
            focus: cx.focus_handle(),
            language_focus: cx.focus_handle(),
        };
        view.refresh(cx);
        view
    }

    /// Places initial keyboard focus inside the application.
    pub fn focus(&self, window: &mut Window, cx: &mut App) {
        self.focus.focus(window, cx);
    }

    fn refresh(&mut self, cx: &mut Context<Self>) {
        if self.refresh_task.is_some() {
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
                view.snapshot = Some(snapshot);
                view.refresh_task = None;
                cx.notify();
            });
        }));
        cx.notify();
    }

    fn act(&mut self, action: Action, window: &mut Window, cx: &mut Context<Self>) {
        match action {
            Action::Refresh => self.refresh(cx),
            Action::Page(page) => {
                self.page = page;
                self.language_menu = false;
            }
            Action::Details(index) => {
                self.details = if self.details == Some(index) {
                    None
                } else {
                    Some(index)
                };
            }
            Action::CopyPath(index) => {
                if let Some(path) = self
                    .snapshot
                    .as_ref()
                    .and_then(|s| s.agents.get(index))
                    .and_then(|a| a.config.as_ref())
                    .and_then(|c| c.path.as_ref())
                {
                    cx.write_to_clipboard(ClipboardItem::new_string(path.display().to_string()));
                    self.copied = Some(index);
                }
            }
            Action::LanguageMenu => {
                self.language_menu = !self.language_menu;
            }
            Action::Language(locale) => {
                self.locale = locale;
                self.language_menu = false;
                self.language_focus.focus(window, cx);
            }
            Action::Appearance(appearance) => {
                self.appearance = appearance;
            }
        }
        cx.notify();
    }

    fn key_down(&mut self, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        let key = &event.keystroke;
        match key.key.as_str() {
            "f5" => self.refresh(cx),
            "r" if key.modifiers.control || key.modifiers.platform => self.refresh(cx),
            "escape" => {
                if self.language_menu {
                    self.language_menu = false;
                    self.language_focus.focus(window, cx);
                } else {
                    self.details = None;
                }
                cx.notify();
            }
            "tab" => {
                if key.modifiers.shift {
                    window.focus_prev(cx);
                } else {
                    window.focus_next(cx);
                }
            }
            _ => return,
        }
        cx.stop_propagation();
    }

    fn button(
        &self,
        id: impl Into<ElementId>,
        label: impl Into<SharedString>,
        icon: Option<&'static [u8]>,
        action: Action,
        palette: Palette,
        cx: &mut Context<Self>,
    ) -> Stateful<Div> {
        let label = label.into();
        let disabled = matches!(action, Action::Refresh) && self.refresh_task.is_some();
        let selected = match action {
            Action::Page(page) => page == self.page,
            Action::Language(locale) => locale == self.locale,
            Action::Appearance(appearance) => appearance == self.appearance,
            _ => false,
        };
        let view = cx.entity().downgrade();
        div()
            .id(id)
            .role(Role::Button)
            .aria_label(label.clone())
            .tab_index(0)
            .tab_stop(!disabled)
            .h(px(30.0))
            .px(px(10.0))
            .flex()
            .items_center()
            .justify_center()
            .gap(px(6.0))
            .rounded(px(6.0))
            .border_1()
            .border_color(gpui::rgba(0))
            .bg(rgb(if selected {
                palette.selected
            } else {
                palette.background
            }))
            .text_color(rgb(if selected {
                palette.accent
            } else {
                palette.muted
            }))
            .text_size(px(12.0))
            .whitespace_nowrap()
            .when(disabled, |b| b.opacity(0.5))
            .when(!disabled, |b| b.cursor_pointer())
            .hover(move |s| s.bg(rgb(palette.inset)).text_color(rgb(palette.text)))
            .focus_visible(move |s| s.border_color(rgb(palette.accent)))
            .when_some(icon, |b, data| b.child(svg().data(data).size(px(16.0))))
            .when(!label.is_empty(), |b| b.child(text!(label)))
            .on_click(cx.listener(move |view, _, window, cx| {
                if !disabled {
                    view.act(action, window, cx);
                }
            }))
            .on_key_down(cx.listener(move |view, event: &KeyDownEvent, window, cx| {
                if matches!(event.keystroke.key.as_str(), "enter" | "space") {
                    if !disabled {
                        view.act(action, window, cx);
                    }
                    cx.stop_propagation();
                }
            }))
            .on_a11y_action(AccessibleAction::Click, move |_, window, cx| {
                let _ = view.update(cx, |view, cx| {
                    if !disabled {
                        view.act(action, window, cx);
                    }
                });
            })
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

    fn detail(
        &self,
        id: &'static str,
        label: Text,
        value: String,
        palette: Palette,
    ) -> Stateful<Div> {
        div()
            .id(id)
            .flex()
            .flex_col()
            .gap(px(6.0))
            .min_w_0()
            .child(
                div()
                    .text_size(px(11.0))
                    .text_color(rgb(palette.muted))
                    .child(text!(self.locale.text(label))),
            )
            .child(
                div()
                    .text_size(px(12.0))
                    .whitespace_normal()
                    .child(text!(value)),
            )
    }

    fn agent_row(
        &self,
        index: usize,
        agent: &Agent,
        compact: bool,
        palette: Palette,
        cx: &mut Context<Self>,
    ) -> Stateful<Div> {
        let warning = agent.config.as_ref().is_some_and(|c| {
            !matches!(
                c.state,
                ConfigFileState::ValidToml | ConfigFileState::Missing
            ) || !c.fields_readable
        });
        let icon = match agent.icon {
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
        };
        let identity = div()
            .flex()
            .items_center()
            .gap(px(12.0))
            .flex_1()
            .min_w_0()
            .child(icon)
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap(px(5.0))
                    .min_w_0()
                    .child(
                        div()
                            .text_size(px(14.0))
                            .font_weight(FontWeight::SEMIBOLD)
                            .child(text!(agent.name)),
                    )
                    .child(
                        div()
                            .text_size(px(12.0))
                            .text_color(rgb(if warning {
                                palette.warning
                            } else {
                                palette.muted
                            }))
                            .child(text!(self.locale.text(Self::config_label(agent)))),
                    ),
            );
        let mut row = div()
            .flex()
            .items_center()
            .gap(px(14.0))
            .w_full()
            .child(identity);
        if let Some(settings) = &agent.settings {
            let model = settings
                .model
                .as_deref()
                .unwrap_or(self.locale.text(Text::AgentDefault));
            row = row.child(
                div()
                    .id("selected-model")
                    .w(px(if compact { 240.0 } else { 220.0 }))
                    .min_w_0()
                    .flex()
                    .flex_col()
                    .gap(px(4.0))
                    .px(px(12.0))
                    .py(px(9.0))
                    .rounded(px(6.0))
                    .bg(rgb(palette.inset))
                    .child(
                        div()
                            .text_size(px(10.0))
                            .text_color(rgb(palette.muted))
                            .child(text!(self.locale.text(if agent.last_known {
                                Text::LastKnown
                            } else {
                                Text::Model
                            }))),
                    )
                    .child(
                        div()
                            .text_size(px(13.0))
                            .font_weight(FontWeight::MEDIUM)
                            .whitespace_normal()
                            .child(text!(model.to_owned())),
                    )
                    .when_some(settings.effort.as_ref(), |field, effort| {
                        field.child(
                            div()
                                .text_size(px(11.0))
                                .text_color(rgb(palette.muted))
                                .child(text!(format!(
                                    "{} · {effort}",
                                    self.locale.text(Text::Effort)
                                ))),
                        )
                    }),
            );
        }
        let expanded = self.details == Some(index);
        let details = self
            .button(
                ("details", index),
                self.locale.text(if expanded {
                    Text::HideDetails
                } else {
                    Text::Details
                }),
                Some(if expanded {
                    assets::DOWN
                } else {
                    assets::CHEVRON
                }),
                Action::Details(index),
                palette,
                cx,
            )
            .aria_expanded(expanded)
            .bg(rgb(palette.surface));
        let header = div().p(px(16.0)).flex().gap(px(12.0)).items_center();
        let header = if compact {
            header
                .flex_col()
                .items_start()
                .child(row.flex_col().items_start())
                .child(details)
        } else {
            header.child(row).child(details)
        };
        let mut container = div()
            .id(agent.id)
            .role(Role::Group)
            .aria_label(agent.name)
            .flex()
            .flex_col()
            .border_b_1()
            .border_color(rgb(palette.border))
            .child(header);
        if expanded {
            let mut fields = div()
                .flex()
                .flex_col()
                .gap(px(14.0))
                .px(px(18.0))
                .pb(px(18.0))
                .child(div().h(px(1.0)).bg(rgb(palette.border)))
                .child(self.detail(
                    "executable",
                    Text::Executable,
                    agent.executable.display().to_string(),
                    palette,
                ));
            if let Some(config) = &agent.config {
                fields = fields.child(
                    div()
                        .text_size(px(12.0))
                        .text_color(rgb(if warning {
                            palette.warning
                        } else {
                            palette.muted
                        }))
                        .child(text!(self.locale.text(Self::config_hint(agent)))),
                );
                if let Some(line) = config.error_line {
                    fields = fields.child(text!(format!(
                        "{} {line}",
                        self.locale.text(Text::ErrorLine)
                    )));
                }
                if let Some(path) = &config.path {
                    fields = fields
                        .child(self.detail(
                            "configuration-path",
                            Text::ConfigPath,
                            path.display().to_string(),
                            palette,
                        ))
                        .child(div().flex().child(self.button(
                            ("copy-path", index),
                            self.locale.text(if self.copied == Some(index) {
                                Text::Copied
                            } else {
                                Text::CopyPath
                            }),
                            None,
                            Action::CopyPath(index),
                            palette,
                            cx,
                        )));
                }
            }
            if let Some(settings) = &agent.settings {
                if let Some(provider) = &settings.provider {
                    fields = fields.child(self.detail(
                        "provider",
                        Text::Provider,
                        provider.clone(),
                        palette,
                    ));
                }
                fields = fields.child(
                    div()
                        .text_size(px(11.0))
                        .text_color(rgb(palette.muted))
                        .child(text!(self.locale.text(if agent.last_known {
                            Text::LastKnownHint
                        } else {
                            Text::ConfigSource
                        }))),
                );
            }
            container = container.child(fields);
        }
        container
    }

    fn agents_page(&self, compact: bool, palette: Palette, cx: &mut Context<Self>) -> Div {
        let mut content = div().flex().flex_col().gap(px(18.0)).child(
            div()
                .text_size(px(13.0))
                .text_color(rgb(palette.muted))
                .child(text!(self.locale.text(Text::Intro))),
        );
        if let Some(snapshot) = &self.snapshot {
            if snapshot.agents.is_empty() {
                return content.child(
                    div()
                        .py(px(72.0))
                        .flex()
                        .flex_col()
                        .gap(px(10.0))
                        .items_center()
                        .child(
                            svg()
                                .data(assets::TERMINAL)
                                .size(px(32.0))
                                .text_color(rgb(palette.muted)),
                        )
                        .child(
                            div()
                                .font_weight(FontWeight::MEDIUM)
                                .child(text!(self.locale.text(Text::EmptyTitle))),
                        )
                        .child(
                            div()
                                .max_w(px(360.0))
                                .text_size(px(12.0))
                                .text_color(rgb(palette.muted))
                                .child(text!(self.locale.text(Text::EmptyHint))),
                        ),
                );
            }
            content = content.child(
                div()
                    .text_size(px(11.0))
                    .text_color(rgb(palette.muted))
                    .child(text!(self.locale.text(Text::LocalAgents))),
            );
            let mut rows = div()
                .rounded(px(10.0))
                .border_1()
                .border_color(rgb(palette.border))
                .bg(rgb(palette.surface))
                .overflow_hidden();
            for (index, agent) in snapshot.agents.iter().enumerate() {
                rows = rows.child(self.agent_row(index, agent, compact, palette, cx));
            }
            content.child(rows)
        } else {
            content.child(
                div()
                    .h(px(96.0))
                    .rounded(px(10.0))
                    .border_1()
                    .border_color(rgb(palette.border))
                    .bg(rgb(palette.surface))
                    .p(px(20.0))
                    .text_color(rgb(palette.muted))
                    .child(text!(self.locale.text(Text::Refreshing))),
            )
        }
    }

    fn settings_page(&self, palette: Palette, cx: &mut Context<Self>) -> Div {
        div()
            .flex()
            .flex_col()
            .gap(px(24.0))
            .child(
                div()
                    .text_size(px(13.0))
                    .text_color(rgb(palette.muted))
                    .child(text!(self.locale.text(Text::Preferences))),
            )
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap(px(12.0))
                    .p(px(20.0))
                    .rounded(px(10.0))
                    .border_1()
                    .border_color(rgb(palette.border))
                    .bg(rgb(palette.surface))
                    .child(
                        div()
                            .font_weight(FontWeight::MEDIUM)
                            .child(text!(self.locale.text(Text::Appearance))),
                    )
                    .child(
                        div().flex().flex_wrap().gap(px(6.0)).children(
                            [
                                ("theme-system", Appearance::System, Text::System),
                                ("theme-light", Appearance::Light, Text::Light),
                                ("theme-dark", Appearance::Dark, Text::Dark),
                            ]
                            .map(|(id, appearance, label)| {
                                self.button(
                                    id,
                                    self.locale.text(label),
                                    None,
                                    Action::Appearance(appearance),
                                    palette,
                                    cx,
                                )
                            }),
                        ),
                    ),
            )
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap(px(8.0))
                    .child(
                        div()
                            .font_weight(FontWeight::MEDIUM)
                            .child(text!(self.locale.text(Text::About))),
                    )
                    .child(
                        div()
                            .text_size(px(12.0))
                            .text_color(rgb(palette.muted))
                            .child(text!(self.locale.text(Text::AboutHint))),
                    ),
            )
    }
}

impl Render for MagpieWindow {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let palette = Palette::for_window(window, self.appearance);
        let compact = window.viewport_size().width < px(650.0);
        let mut language = div().relative().child(
            self.button(
                "language",
                self.locale.name(),
                Some(assets::GLOBE),
                Action::LanguageMenu,
                palette,
                cx,
            )
            .track_focus(&self.language_focus)
            .aria_expanded(self.language_menu)
            .aria_label(self.locale.text(Text::Language))
            .child(svg().data(assets::DOWN).size(px(12.0))),
        );
        if self.language_menu {
            let options = Locale::ALL.map(|locale| {
                self.button(
                    locale.id(),
                    locale.name(),
                    None,
                    Action::Language(locale),
                    palette,
                    cx,
                )
                .role(Role::MenuItem)
                .w_full()
                .justify_start()
                .bg(rgb(if locale == self.locale {
                    palette.selected
                } else {
                    palette.surface
                }))
            });
            language = language.child(
                div()
                    .id("language-options")
                    .role(Role::Menu)
                    .aria_label(self.locale.text(Text::Language))
                    .absolute()
                    .bottom(px(36.0))
                    .right_0()
                    .w(px(190.0))
                    .p(px(5.0))
                    .border_1()
                    .border_color(rgb(palette.border))
                    .bg(rgb(palette.surface))
                    .rounded(px(8.0))
                    .shadow_md()
                    .on_mouse_down_out(cx.listener(|view, _, _, cx| {
                        view.language_menu = false;
                        cx.notify();
                    }))
                    .children(options),
            );
        }
        let body = match self.page {
            Page::Agents => self.agents_page(compact, palette, cx),
            Page::Settings => self.settings_page(palette, cx),
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
            .text_color(rgb(palette.text))
            .bg(rgb(palette.background))
            .child(
                div()
                    .h(px(60.0))
                    .flex_none()
                    .px(px(if compact { 14.0 } else { 24.0 }))
                    .flex()
                    .items_center()
                    .justify_between()
                    .gap(px(10.0))
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap(px(8.0))
                            .child(
                                svg()
                                    .path("icons/magpie.svg")
                                    .size(px(26.0))
                                    .text_color(rgb(palette.text)),
                            )
                            .when(!compact, |brand| {
                                brand.child(
                                    div()
                                        .text_size(px(18.0))
                                        .font_weight(FontWeight::SEMIBOLD)
                                        .child(text!("magpie")),
                                )
                            }),
                    )
                    .child(
                        div()
                            .flex()
                            .gap(px(5.0))
                            .child(
                                self.button(
                                    "agents-tab",
                                    self.locale.text(Text::Agents),
                                    None,
                                    Action::Page(Page::Agents),
                                    palette,
                                    cx,
                                )
                                .role(Role::Tab)
                                .aria_selected(self.page == Page::Agents),
                            )
                            .child(
                                self.button(
                                    "settings-tab",
                                    self.locale.text(Text::Settings),
                                    Some(assets::SETTINGS),
                                    Action::Page(Page::Settings),
                                    palette,
                                    cx,
                                )
                                .role(Role::Tab)
                                .aria_selected(self.page == Page::Settings),
                            ),
                    )
                    .child(
                        self.button(
                            "refresh",
                            if compact {
                                ""
                            } else {
                                self.locale.text(Text::Refresh)
                            },
                            Some(assets::REFRESH),
                            Action::Refresh,
                            palette,
                            cx,
                        )
                        .aria_label(self.locale.text(Text::Refresh)),
                    ),
            )
            .child(
                div()
                    .id("page-scroll")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .px(px(if compact { 14.0 } else { 24.0 }))
                    .pt(px(16.0))
                    .pb(px(24.0))
                    .child(body),
            )
            .child(
                div()
                    .flex_none()
                    .px(px(if compact { 12.0 } else { 24.0 }))
                    .py(px(10.0))
                    .flex()
                    .items_center()
                    .justify_between()
                    .gap(px(8.0))
                    .child(
                        div()
                            .id("scan-status")
                            .role(Role::Status)
                            .text_size(px(11.0))
                            .text_color(rgb(palette.muted))
                            .child(text!(if self.refresh_task.is_some() {
                                self.locale.text(Text::Refreshing).to_owned()
                            } else {
                                format!(
                                    "v{} · {}",
                                    env!("CARGO_PKG_VERSION"),
                                    self.locale.text(Text::Preview)
                                )
                            })),
                    )
                    .child(language),
            )
    }
}
