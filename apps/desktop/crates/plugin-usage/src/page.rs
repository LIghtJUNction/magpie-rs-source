//! Local request history with explicit unknown usage and no message bodies.
use crate::{
    Loader, UiAppearance, UsageLoadError,
    report::{self as usage, Record, Report, Tokens},
    theme::Palette,
    usage_text::{Locale, UsageText as U},
};
use gpui::{
    App, Context, Div, Entity, FocusHandle, FontWeight, Render, Role, ScrollHandle, Window, div,
    point, prelude::*, px, rgb, text,
};
use gpui_kit_material::components::{ButtonSize, ButtonVariant, MaterialButton, material_button};
use magpie_ui_widgets::{InputEvent, TextInput};
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};

pub struct UsagePage {
    locale: Locale,
    loader: Loader,
    appearance: UiAppearance,
    report: Option<Report>,
    error: bool,
    busy: bool,
    shutdown_pending: Arc<AtomicBool>,
    cancellation: Option<Arc<AtomicBool>>,
    query: String,
    status: Option<&'static str>,
    search: Entity<TextInput>,
    scroll: ScrollHandle,
    focus: FocusHandle,
    _subscription: gpui::Subscription,
}

impl UsagePage {
    pub fn new(cx: &mut Context<Self>, loader: Loader) -> Self {
        let locale = Locale::English;
        let search = cx.new(|cx| {
            let mut input = TextInput::new(
                "usage-search",
                "",
                locale.usage_text(U::Search),
                locale.usage_text(U::Search),
                cx,
            );
            input.set_limit(256);
            input
        });
        let subscription = cx.subscribe(&search, |view, input, event, cx| {
            if *event == InputEvent::Changed {
                view.query = input.read(cx).text().to_lowercase();
                view.scroll.set_offset(point(px(0.0), px(0.0)));
                cx.notify();
            }
        });
        Self {
            locale,
            loader,
            appearance: UiAppearance::System,
            report: None,
            error: false,
            busy: false,
            shutdown_pending: Arc::new(AtomicBool::new(false)),
            cancellation: None,
            query: String::new(),
            status: None,
            search,
            scroll: ScrollHandle::new(),
            focus: cx.focus_handle(),
            _subscription: subscription,
        }
    }

    pub fn set_presentation(
        &mut self,
        locale_code: &str,
        appearance: UiAppearance,
        cx: &mut Context<Self>,
    ) {
        let locale = Locale::parse(locale_code);
        if self.locale != locale || self.appearance != appearance {
            self.locale = locale;
            self.appearance = appearance;
            cx.notify();
        }
    }

    pub fn focus(&self, window: &mut Window, cx: &mut App) {
        let focus = self.search.read(cx).focus_handle();
        window.on_next_frame(move |window, cx| focus.focus(window, cx));
    }

    /// Returns whether the current refresh is still awaiting completion.
    pub fn busy(&self) -> bool {
        self.busy
    }

    /// Retains a cleanup signal beyond the page's lifetime.
    ///
    /// The host must cancel first, then keep its runtime alive until an Acquire
    /// load returns false. Only the background operation clears this signal.
    pub fn shutdown_pending(&self) -> Arc<AtomicBool> {
        self.shutdown_pending.clone()
    }

    /// Requests cancellation without dropping the host operation or its waiter.
    pub fn cancel(&self) {
        if let Some(cancellation) = &self.cancellation {
            cancellation.store(true, Ordering::Release);
        }
    }

    pub fn refresh(&mut self, cx: &mut Context<Self>) {
        if self.busy {
            return;
        }
        self.busy = true;
        self.shutdown_pending.store(true, Ordering::Release);
        let cancellation = Arc::new(AtomicBool::new(false));
        self.cancellation = Some(cancellation.clone());
        let loader = self.loader.clone();
        let pending = self.shutdown_pending.clone();
        let operation = cx.background_executor().spawn(async move {
            let result = match loader(cancellation).await {
                Ok(value) => {
                    usage::parse_value(&value, 100).map_err(|_| UsageLoadError::InvalidReport)
                }
                Err(error) => Err(error),
            };
            // The host future has returned after subprocess cleanup. Do not make
            // shutdown depend on this weak page still existing or repainting.
            pending.store(false, Ordering::Release);
            result
        });
        // Retain the operation even after the Entity is removed. Cancelling a
        // page-owned Task here would drop the host bridge future mid-cleanup.
        cx.spawn(async move |this, cx| {
            let result = operation.await;
            let _ = this.update(cx, |view, cx| {
                view.busy = false;
                let cancelled = view
                    .cancellation
                    .take()
                    .is_some_and(|flag| flag.load(Ordering::Acquire));
                if !cancelled {
                    match result {
                        Ok(report) => {
                            view.report = Some(report);
                            view.error = false;
                        }
                        _ => view.error = true,
                    }
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    fn matches_query(&self, record: &Record) -> bool {
        self.query.is_empty()
            || [&record.provider_id, &record.model, &record.id]
                .iter()
                .any(|s| s.to_lowercase().contains(&self.query))
    }

    fn button(
        &self,
        id: &'static str,
        label: String,
        status: Option<Option<&'static str>>,
        cx: &mut Context<Self>,
    ) -> MaterialButton {
        let selected = status.is_some_and(|value| value == self.status);
        material_button(
            id,
            if selected {
                ButtonVariant::Tonal
            } else {
                ButtonVariant::Text
            },
        )
        .button_size(ButtonSize::Small)
        .accessibility_label(label.clone())
        .disabled(status.is_none() && self.busy)
        .when_some(status, |button, _| button.toggle(true).selected(selected))
        .on_click(cx.listener(move |view, _, _, cx| {
            if let Some(status) = status {
                view.status = status;
                view.scroll.set_offset(point(px(0.0), px(0.0)));
                cx.notify();
            } else {
                view.refresh(cx);
            }
        }))
        .child(text!(label))
    }

    fn caption(&self, key: U, palette: Palette) -> Div {
        div()
            .text_size(px(12.0))
            .line_height(px(18.0))
            .text_color(rgb(palette.muted))
            .whitespace_normal()
            .child(text!(self.locale.usage_text(key)))
    }

    fn statistic(
        &self,
        key: U,
        value: Option<u64>,
        coverage: Option<(usize, usize)>,
        palette: Palette,
    ) -> Div {
        div()
            .min_w(px(120.0))
            .min_h(px(84.0))
            .flex_1()
            .flex()
            .flex_col()
            .gap(px(6.0))
            .child(self.caption(key, palette))
            .child(
                div()
                    .text_size(px(28.0))
                    .line_height(px(34.0))
                    .font_weight(FontWeight::MEDIUM)
                    .child(text!(format_number(value, self.locale))),
            )
            .when_some(coverage, |card, (known, total)| {
                card.child(
                    div()
                        .text_size(px(11.0))
                        .text_color(rgb(palette.muted))
                        .child(text!(format!(
                            "{} {known}/{total}",
                            self.locale.usage_text(U::Reported)
                        ))),
                )
            })
    }

    fn row(&self, record: &Record, palette: Palette, compact: bool) -> gpui::Stateful<Div> {
        let outcome = match record.outcome.as_str() {
            "completed" => U::Completed,
            "failed" => U::Failed,
            _ => U::Cancelled,
        };
        let color = if record.outcome == "failed" {
            palette.danger
        } else {
            palette.muted
        };
        let time = i64::try_from(record.started_at_ms)
            .ok()
            .and_then(chrono::DateTime::<chrono::Utc>::from_timestamp_millis)
            .map(|time| {
                time.with_timezone(&chrono::Local)
                    .format(if self.locale == Locale::German {
                        "%d.%m. %H:%M:%S %:z"
                    } else {
                        "%m-%d %H:%M:%S %:z"
                    })
                    .to_string()
            })
            .unwrap_or_else(|| "—".into());
        let metrics = div()
            .flex()
            .gap(px(16.0))
            .when(compact, |row| row.w_full())
            .when(!compact, |row| row.w(px(296.0)).flex_none())
            .children(
                [
                    (
                        U::Input,
                        (|t: &Tokens| t.input_tokens) as fn(&Tokens) -> Option<u64>,
                    ),
                    (U::Output, |t| t.output_tokens),
                    (U::Total, |t| t.total_tokens),
                ]
                .map(|(key, field)| {
                    div()
                        .flex_1()
                        .min_w_0()
                        .flex()
                        .flex_col()
                        .gap(px(5.0))
                        .text_right()
                        .child(
                            div()
                                .text_size(px(10.0))
                                .line_height(px(16.0))
                                .text_color(rgb(palette.muted))
                                .whitespace_normal()
                                .child(text!(self.locale.usage_text(key))),
                        )
                        .child(
                            div()
                                .text_size(px(16.0))
                                .font_weight(FontWeight::MEDIUM)
                                .child(text!(format_number(
                                    record.usage.as_ref().and_then(field),
                                    self.locale,
                                ))),
                        )
                }),
            );
        div()
            .id(gpui::SharedString::from(format!(
                "usage-record:{}",
                record.id
            )))
            .role(Role::ListItem)
            .w_full()
            .min_w_0()
            .py(px(18.0))
            .border_b_1()
            .border_color(rgb(palette.border))
            .flex()
            .items_center()
            .gap(px(24.0))
            .when(compact, |row| row.flex_col().items_stretch().gap(px(14.0)))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .flex_col()
                    .gap(px(6.0))
                    .child(
                        div()
                            .flex()
                            .flex_wrap()
                            .items_center()
                            .gap(px(10.0))
                            .child(
                                div()
                                    .min_w_0()
                                    .text_size(px(14.0))
                                    .font_weight(FontWeight::MEDIUM)
                                    .whitespace_normal()
                                    .child(text!(record.model.clone())),
                            )
                            .child(
                                div()
                                    .text_size(px(11.0))
                                    .text_color(rgb(color))
                                    .child(text!(self.locale.usage_text(outcome))),
                            ),
                    )
                    .child(
                        div()
                            .text_size(px(11.0))
                            .line_height(px(17.0))
                            .text_color(rgb(palette.muted))
                            .whitespace_normal()
                            .child(text!(format!(
                                "{}: {} · {} · {}",
                                self.locale.usage_text(U::Provider),
                                record.provider_id,
                                record.protocol,
                                self.locale.usage_text(if record.stream {
                                    U::Stream
                                } else {
                                    U::Json
                                })
                            ))),
                    )
                    .child(
                        div()
                            .flex()
                            .flex_wrap()
                            .gap(px(12.0))
                            .text_size(px(11.0))
                            .text_color(rgb(palette.muted))
                            .child(text!(time))
                            .child(text!(format!(
                                "{} {} ms",
                                self.locale.usage_text(U::Duration),
                                record.duration_ms
                            ))),
                    )
                    .child(
                        div()
                            .text_size(px(10.0))
                            .text_color(rgb(palette.muted))
                            .truncate()
                            .child(text!(record.id.clone())),
                    ),
            )
            .child(metrics)
    }
}

#[cfg(feature = "visual-test")]
impl UsagePage {
    pub fn preview_fixture(&mut self, cx: &mut Context<Self>) {
        self.cancel();
        let records = [("completed", Some(7), Some(2), Some(9)), ("failed", None, None, None), ("cancelled", Some(5), None, None)]
            .into_iter().enumerate().map(|(index, (outcome, input, output, total))| serde_json::json!({
                "schema":1, "id":format!("synthetic-request-{index}"),
                "started_at_ms":1793602801000u64-index as u64*1000, "duration_ms":230,
                "provider_id":"fixture", "model":format!("fixture/model-{}", if index == 1 { "b" } else { "a" }),
                "upstream_model":if index == 1 { "model-b" } else { "model-a" },
                "protocol":"openai_responses", "upstream_protocol":"openai_responses",
                "stream":index!=0, "outcome":outcome,
                "usage": if input.is_some() { serde_json::json!({"input_tokens":input,"output_tokens":output,"total_tokens":total}) } else { serde_json::Value::Null }
            })).collect::<Vec<_>>();
        let wire = serde_json::json!({"version":"0.2.0","scope":"recent_requests","limit":100,
            "incomplete_records":0,"totals":{"requests":3,"completed":1,"failed":1,"cancelled":1,
                "reported_requests":2,"input_tokens":12,"output_tokens":2,"total_tokens":9},"records":records});
        self.report = Some(
            usage::parse(&serde_json::to_vec(&wire).expect("fixture JSON"), 100)
                .expect("valid synthetic Usage"),
        );
        self.error = false;
        self.query.clear();
        self.status = None;
        self.search.update(cx, |input, cx| {
            input.set_text("", cx);
        });
        self.scroll.set_offset(point(px(0.0), px(0.0)));
        cx.notify();
    }
}

impl Drop for UsagePage {
    fn drop(&mut self) {
        self.cancel();
    }
}

impl Render for UsagePage {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let palette = Palette::for_window(window, self.appearance);
        let compact = window.viewport_size().width < px(720.0);
        self.search.update(cx, |input, cx| {
            input.set_presentation(
                self.locale.usage_text(U::Search),
                self.locale.usage_text(U::Search),
                palette,
                false,
                cx,
            )
        });
        let records: Vec<_> = self
            .report
            .as_ref()
            .map(|r| r.records.iter().filter(|r| self.matches_query(r)).collect())
            .unwrap_or_default();
        let filters = [
            ("usage-all", None, U::All),
            ("usage-completed", Some("completed"), U::Completed),
            ("usage-failed", Some("failed"), U::Failed),
            ("usage-cancelled", Some("cancelled"), U::Cancelled),
        ];
        let chips =
            div()
                .flex()
                .flex_wrap()
                .gap(px(4.0))
                .children(filters.map(|(id, status, label)| {
                    let n = records
                        .iter()
                        .filter(|r| status.is_none_or(|status| r.outcome == status))
                        .count();
                    self.button(
                        id,
                        format!(
                            "{} {}",
                            self.locale.usage_text(label),
                            format_number(self.report.as_ref().map(|_| n as u64), self.locale)
                        ),
                        Some(status),
                        cx,
                    )
                }));
        let visible: Vec<_> = records
            .iter()
            .copied()
            .filter(|r| self.status.is_none_or(|status| r.outcome == status))
            .collect();
        let summary = div()
            .py(px(24.0))
            .flex()
            .flex_wrap()
            .gap(px(20.0))
            .child(self.statistic(
                U::Requests,
                self.report.as_ref().map(|_| visible.len() as u64),
                None,
                palette,
            ))
            .children(
                [
                    (
                        U::Input,
                        (|t: &Tokens| t.input_tokens) as fn(&Tokens) -> Option<u64>,
                    ),
                    (U::Output, |t| t.output_tokens),
                    (U::Total, |t| t.total_tokens),
                ]
                .map(|(key, field)| {
                    let (value, coverage) = token_statistic(&visible, field);
                    self.statistic(key, value, self.report.as_ref().map(|_| coverage), palette)
                }),
            );
        let empty = if self.report.as_ref().is_some_and(|r| !r.records.is_empty()) {
            U::NoMatches
        } else {
            U::Empty
        };
        let mut content = div()
            .id("usage-history-scroll")
            .flex_1()
            .min_w_0()
            .min_h_0()
            .overflow_y_scroll()
            .track_scroll(&self.scroll)
            .child(summary)
            .child(
                div()
                    .pb(px(20.0))
                    .border_b_1()
                    .border_color(rgb(palette.border))
                    .child(self.caption(U::CostUnknown, palette)),
            );
        if self.report.is_some() && visible.is_empty() {
            content = content.child(
                div()
                    .py(px(36.0))
                    .flex()
                    .flex_col()
                    .gap(px(10.0))
                    .child(text!(self.locale.usage_text(empty)))
                    .when(matches!(empty, U::Empty), |body| {
                        body.child(self.caption(U::EmptyHint, palette))
                    }),
            );
        } else if self.report.is_some() {
            content = content.child(
                div().id("usage-records").role(Role::List).children(
                    visible
                        .into_iter()
                        .map(|record| self.row(record, palette, compact)),
                ),
            );
        }
        content = content.child(
            div()
                .py(px(20.0))
                .flex()
                .flex_col()
                .gap(px(6.0))
                .child(self.caption(U::Privacy, palette))
                .child(self.caption(U::LocalTime, palette)),
        );
        div()
            .id("usage-page")
            .track_focus(&self.focus)
            .size_full()
            .min_w_0()
            .flex()
            .flex_col()
            .overflow_hidden()
            .text_color(rgb(palette.text))
            .child(
                div()
                    .flex_none()
                    .pb(px(16.0))
                    .flex()
                    .flex_col()
                    .gap(px(10.0))
                    .child(
                        div()
                            .flex()
                            .flex_wrap()
                            .items_center()
                            .gap(px(8.0))
                            .child(div().flex_1().min_w(px(180.0)).child(self.search.clone()))
                            .child(
                                self.button(
                                    "usage-refresh",
                                    self.locale
                                        .usage_text(if self.busy { U::Loading } else { U::Refresh })
                                        .into(),
                                    None,
                                    cx,
                                ),
                            ),
                    )
                    .child(chips),
            )
            .when(self.error, |body| {
                body.child(
                    div()
                        .id("usage-read-error")
                        .p(px(12.0))
                        .rounded(px(8.0))
                        .role(Role::Alert)
                        .bg(rgb(palette.warning_bg))
                        .text_color(rgb(palette.warning))
                        .whitespace_normal()
                        .child(text!(format!(
                            "{} {}",
                            self.locale.usage_text(U::ReadFailed),
                            if self.report.is_some() {
                                self.locale.usage_text(U::Stale)
                            } else {
                                ""
                            }
                        ))),
                )
            })
            .when_some(
                self.report.as_ref().filter(|r| r.incomplete_records > 0),
                |body, report| {
                    body.child(
                        div()
                            .py(px(8.0))
                            .text_color(rgb(palette.warning))
                            .child(text!(format!(
                                "{}: {}",
                                self.locale.usage_text(U::Incomplete),
                                report.incomplete_records
                            ))),
                    )
                },
            )
            .child(content)
    }
}

// Each token field has its own coverage: a record can report input without output.
fn token_statistic(
    records: &[&Record],
    field: fn(&Tokens) -> Option<u64>,
) -> (Option<u64>, (usize, usize)) {
    let known = records
        .iter()
        .filter(|r| r.usage.as_ref().and_then(field).is_some())
        .count();
    (
        usage::token_sum(records.iter().copied(), field),
        (known, records.len()),
    )
}

fn format_number(value: Option<u64>, locale: Locale) -> String {
    let Some(value) = value else {
        return "—".into();
    };
    let digits = value.to_string();
    let separator = if locale == Locale::German { '.' } else { ',' };
    let mut result = String::with_capacity(digits.len() + digits.len() / 3);
    for (index, ch) in digits.chars().enumerate() {
        if index > 0 && (digits.len() - index).is_multiple_of(3) {
            result.push(separator);
        }
        result.push(ch);
    }
    result
}

#[cfg(all(test, feature = "visual-test"))]
mod cleanup_tests {
    use super::*;
    use gpui::AppContext;
    use std::{
        future::poll_fn,
        sync::Mutex,
        task::{Poll, Waker},
    };

    struct Probe {
        cleaned: Arc<AtomicBool>,
        dropped_early: Arc<AtomicBool>,
    }
    impl Drop for Probe {
        fn drop(&mut self) {
            if !self.cleaned.load(Ordering::Acquire) {
                self.dropped_early.store(true, Ordering::Release);
            }
        }
    }

    #[derive(Default)]
    struct Host {
        release: AtomicBool,
        cleaned: Arc<AtomicBool>,
        dropped_early: Arc<AtomicBool>,
        cancellation: Mutex<Option<Arc<AtomicBool>>>,
        waker: Mutex<Option<Waker>>,
    }
    impl Host {
        fn loader(self: &Arc<Self>) -> Loader {
            let host = self.clone();
            Arc::new(move |flag| {
                let host = host.clone();
                Box::pin(async move {
                    *host.cancellation.lock().unwrap() = Some(flag);
                    let _probe = Probe {
                        cleaned: host.cleaned.clone(),
                        dropped_early: host.dropped_early.clone(),
                    };
                    poll_fn(|cx| {
                        if host.release.load(Ordering::Acquire) {
                            host.cleaned.store(true, Ordering::Release);
                            Poll::Ready(Err(UsageLoadError::Cancelled))
                        } else {
                            *host.waker.lock().unwrap() = Some(cx.waker().clone());
                            Poll::Pending
                        }
                    })
                    .await
                })
            })
        }
        fn finish_cleanup(&self) {
            self.release.store(true, Ordering::Release);
            self.waker
                .lock()
                .unwrap()
                .take()
                .expect("running host waiter")
                .wake();
        }
        fn cancelled(&self) -> bool {
            self.cancellation
                .lock()
                .unwrap()
                .as_ref()
                .expect("host started")
                .load(Ordering::Acquire)
        }
    }

    #[gpui::test]
    fn preview_token_statistics_have_independent_known_record_coverage(
        cx: &mut gpui::TestAppContext,
    ) {
        let page = cx.new(|cx| {
            UsagePage::new(
                cx,
                Arc::new(|_| Box::pin(async { Err(UsageLoadError::Unavailable) })),
            )
        });
        page.update(cx, |page, cx| page.preview_fixture(cx));
        page.read_with(cx, |page, _| {
            let report = page.report.as_ref().expect("preview report");
            let records: Vec<_> = report.records.iter().collect();
            assert_eq!(
                token_statistic(&records, |t| t.input_tokens),
                (Some(12), (2, 3))
            );
            assert_eq!(
                token_statistic(&records, |t| t.output_tokens),
                (Some(2), (1, 3))
            );
            assert_eq!(
                token_statistic(&records, |t| t.total_tokens),
                (Some(9), (1, 3))
            );
            assert_eq!(
                report.totals.reported_requests, 2,
                "wire contract remains unchanged"
            );

            let unknown = [&report.records[1]];
            let (value, coverage) = token_statistic(&unknown, |t| t.input_tokens);
            assert_eq!((value, coverage), (None, (0, 1)));
            assert_eq!(format_number(value, Locale::English), "—");
            assert_eq!(token_statistic(&[], |t| t.input_tokens), (None, (0, 0)));

            let mut zero = report.records[0].clone();
            zero.usage.as_mut().unwrap().input_tokens = Some(0);
            assert_eq!(
                token_statistic(&[&zero], |t| t.input_tokens),
                (Some(0), (1, 1)),
                "a reported zero is known"
            );
        });
    }

    #[gpui::test]
    fn dropping_page_keeps_host_operation_until_cleanup_returns(cx: &mut gpui::TestAppContext) {
        let host = Arc::new(Host::default());
        let page = cx.new(|cx| UsagePage::new(cx, host.loader()));
        page.update(cx, |page, cx| page.refresh(cx));
        let pending = page.read_with(cx, |page, _| page.shutdown_pending());
        cx.run_until_parked();
        assert!(pending.load(Ordering::Acquire));
        drop(page);
        cx.update(|_| {});
        cx.run_until_parked();
        assert!(host.cancelled(), "page drop must request host cancellation");
        assert!(
            !host.dropped_early.load(Ordering::Acquire),
            "host future must remain alive"
        );
        assert!(
            pending.load(Ordering::Acquire),
            "page drop must not pretend cleanup finished"
        );
        host.finish_cleanup();
        cx.run_until_parked();
        assert!(host.cleaned.load(Ordering::Acquire));
        assert!(!pending.load(Ordering::Acquire));
        assert!(!host.dropped_early.load(Ordering::Acquire));
    }

    #[gpui::test]
    fn preview_cancels_without_dropping_or_overwriting_pending_host_result(
        cx: &mut gpui::TestAppContext,
    ) {
        let host = Arc::new(Host::default());
        let page = cx.new(|cx| UsagePage::new(cx, host.loader()));
        page.update(cx, |page, cx| page.refresh(cx));
        cx.run_until_parked();
        page.update(cx, |page, cx| {
            page.preview_fixture(cx);
            assert!(page.busy());
            assert!(page.shutdown_pending().load(Ordering::Acquire));
        });
        assert!(host.cancelled());
        host.finish_cleanup();
        cx.run_until_parked();
        page.read_with(cx, |page, _| {
            assert!(!page.busy());
            assert!(!page.shutdown_pending().load(Ordering::Acquire));
            assert_eq!(page.report.as_ref().unwrap().records.len(), 3);
            assert!(
                !page.error,
                "cancelled host result must not replace preview"
            );
        });
        assert!(!host.dropped_early.load(Ordering::Acquire));
    }
}
