# Usage UI plugin

Package: `magpie-plugin-usage-ui`. It depends on the shared widgets, public core
validators, serde, GPUI, Material and chrono. It does not depend on the desktop
shell or CLI bridge.

```rust,ignore
use magpie_plugin_usage_ui::{Loader, UsageLoadError, UsagePage, UiAppearance};

// Inside cx.new:
UsagePage::new(cx, loader);
// Inside page.update:
page.set_presentation("zh-CN", UiAppearance::Dark, cx);
page.refresh(cx);
// Focus the existing search field without rebuilding the page:
page.focus(window, cx);
```

`Loader = Arc<dyn Fn(Arc<AtomicBool>) -> LoadFuture + Send + Sync>`;
`LoadFuture = Pin<Box<dyn Future<Output = Result<serde_json::Value,
UsageLoadError>> + Send>>`. The host owns the data source, permission checks,
subprocess lifecycle and source size cap. It must observe the cancellation flag
with `Ordering::Acquire` and cancel its corresponding operation. Dropping the
page sets the flag with `Ordering::Release`. The page does not own a cancellable
Task: a detached weak-Entity waiter retains the background operation through
completion, even if the page is withdrawn or its plugin is disabled.

`UsagePage::busy() -> bool` reports refresh activity. Before withdrawing a page,
retain `UsagePage::shutdown_pending() -> Arc<AtomicBool>` and call
`UsagePage::cancel()`. Only the background operation clears the pending flag,
after the loader has returned and completed host cleanup. Before stopping the
runtime on root-window exit, the composition owner must cancel its pages and
keep driving the runtime until every retained pending flag reads false with
`Ordering::Acquire`. Dropping a page or cancelling it does not clear pending.
Cancelled refreshes do not overwrite the last report or a preview fixture.

A refresh is single-flight. Failed refreshes preserve the last successful report.
The plugin validates the unchanged `0.2.0` recent_requests JSON contract itself,
with a 1 MiB serialized limit and at most 100 records. Unknown tokens and costs
remain unknown. `report::{Report, Record, Totals, Tokens, ParseError, parse,
parse_value, token_sum}` are public for host adapters; host errors remain static
and never display raw process output.

Navigation labels are exposed by `label(locale_code) -> &'static str`;
`intro(locale_code) -> &'static str` describes the recent recorded forwarded-request
scope. Both are pure functions, independent of shell localization.

Locales: `en`, `zh-CN`, `zh-TW`/`zh-Hant`, `ja`, `de`; unknown codes use English.
`UiAppearance::{System, Light, Dark}` is shared with widgets. The plugin computes
its own Neutral Material semantic palette; the host initializes the Material
component theme and supplies overall typography as before.

The `visual-test` feature forwards `gpui/test-support` and widget visual tests,
and exposes `UsagePage::preview_fixture(cx)` for the host preview renderer.
The original shell Usage files remain for the integration owner to remove.
