# Application plugin composition

This crate embeds declarative source packages and registers validated
contributions in the generic core Registry. It has no normal dependency on Usage,
Codex, discovery, service or desktop runtime crates. The application supplies its
implemented feature factories and native engines. It does not install npm
packages, execute plugin scripts, read Agent account files or open business logs.

`build.rs` reads `plugins/**/plugin.toml` in sorted relative-path order. It embeds
referenced adjacent Agent `definition.json` resources too. Rust string literals
are escaped by Rust formatting, never interpolated as executable source. The scan
rejects symlinks and caps depth at 8, entries at 4096, packages at 256, manifests at
256 KiB, Agent resources at 64 KiB and total embedded text at 8 MiB. New resource
paths and the source root are declared to Cargo for rebuild tracking.

## Host API

- `AppPlugins::load()` registers bundled and user declarative Provider/Agent
  packages without optional feature runtimes. Compiled native engines stay pending.
- `load_with_features(&[FeatureRegistration])` additionally registers host-supplied
  commands, page slots and event subscription factories. Each feature's manifest
  is supplied through `PluginInput::bundled`; registration must match the host's
  `(name, handler)`, `(page_id, command)` and `(subscriber_id, event)` capabilities.
- `load_with_engines(features, &[String])` also registers compiled bundled adapters
  and discovery catalogs for the explicitly implemented engine IDs. The host must
  actually link and dispatch those implementations. A user manifest cannot claim
  a compiled engine factory merely by copying its engine name.
- `load_at(directory, state, features)` and `load_at_with_engines(...)` use explicit
  absolute paths for tests and independent hosts. Defaults share ProviderStore's
  config root: user packages in `plugins/`, preferences in `plugins.toml`.

Only typed validated contributions are acknowledged active. Provider fields pass
`ProviderPreset::validate`; declarative Agent resources pass
`AgentDefinition::from_manifest_json` and contribution-ID matching. Unknown
features/native engines stay pending, malformed/colliding contributions fail
with complete owner rollback, and Node/MCP/JavaScript kinds remain unsupported.
Disabling retracts contributions after a fresh load and preserves package/data
files. This crate does not by itself dispose live host handles; runtime hosts must
implement that lifecycle.

`AppPlugins` exposes `registry`, `providers`, `agents`, `native_agents`, `pages`,
`commands`, `metadata_subscribers`, and `discovery`. Every projection includes an
owner. `to_json()` produces the CLI catalog:

```text
{
  version, revision,
  plugins: [{id, owner, name, version, api_version, kind, source,
             enabled, status, waiting_for, error}],
  contributions: {
    providers: [{owner, id, name, base_url, protocol, requires_key, source}],
    agents: [{owner, id, name, engine, definition}],
    pages: [{owner, id, name, command}],
    commands: [{owner, name, handler}],
    metadata_subscribers: [{owner, id, event}],
    discovery: [{owner, id, engine, entries, claim}]
  },
  diagnostics: [{source, error}]
}
```

`source` is `{kind:"bundled",label}` or `{kind:"user",path}`. Declarative Agent
entries contain their validated public definition and `declarative-config/1`;
native Agent entries contain their actual engine ID and `definition:null`.
Discovery's 68 filename candidates do not claim 68 functioning adapters. No raw
manifest documents or Agent configuration contents are emitted in this catalog.

## CLI integration

```sh
magpie plugins list --json
magpie plugins disable feature.usage --if-revision 0 --json
magpie plugins enable feature.usage --if-revision 1 --json
magpie plugins call feature.usage report --json --limit 100 --store /ABS/providers.toml
magpie plugins call agent.claude preview --config /ABS/settings.json --model fixture/test --listen 127.0.0.1:3425 --json
```

Successful calls return `{version,plugin_id,operation,result}`. Failures are
nonzero with static JSON diagnostics on stderr when `--json` is selected. The old
`magpie usage` spelling goes through active-owner dispatch while retaining its old
report JSON shape. Neither listing nor report queries create business logs.

CLI owns the concrete Usage factory. Its generic `MetadataDispatcher` enqueues
at most 128 bounded metadata events via `try_send`, with no request-completion
filesystem IO or blocking channel operations. The background host loads and
registers the **current** Registry after each event is dequeued. It withdraws off
owners and flushes their accepted writes, registers re-enabled factories, isolates
callback/factory panics by owner and fails closed on unreadable registry state.
An owner quarantined by a callback failure retries after a preference revision
change or a host restart. A flush barrier drains accepted events and child flushes
with an 8-second caller deadline; health must also be checked for losses/errors.
Health exposes dispatcher accepted/pending/dispatched/skipped/dropped/failed and
only allowlisted numeric plugin counters plus runtime status, never plugin event
payloads. Usage's own bounded writer queue remains a separate durability boundary.
A disabled period is not backfilled on re-enable.

Before Agent apply, the CLI privately pins the exact validated definition and
explicit target under `plugin-bindings/`. Targets and definitions are bounded;
Agent configuration/recovery bytes are not copied there. Existing definition pins
are not overwritten by a failed connect or plugin upgrade. Disable checks retained
targets and resolved defaults, rejects connected/drifted/pending or uncertain
ownership, and preserves explicit disconnect using the pinned original definition
even if the source user package is subsequently removed. Native Codex target
associations are retained separately; its legacy commands and discovery inventory
have independent owner gates. These checks cover this host's known targets and
are not a universal dependency/in-use lock across arbitrary external callers.
Concurrent configuration operations still use their respective transaction locks.

Secure external resource and private binding IO currently use descriptor-relative
no-follow paths on Linux/Android/macOS. Windows disk mutation support and real
macOS/Android device execution remain unverified/unsupported as indicated by the
underlying engines. Full Node/npm lifecycle, external executable isolation and
subscription migration remain separate work. Desktop rendering is outside this
crate and was not changed for this integration.

## Verification

Use Rust 1.99.1 with `--locked --offline`; set `CARGO_INCREMENTAL=0`,
`CARGO_PROFILE_DEV_DEBUG=0` and a `CARGO_TARGET_DIR` outside the checkout.
`magpie-app-plugins` unit tests and CLI `plugin_runtime` tests use private synthetic
fixtures. The runtime test gates a real loopback upstream request until a separate
CLI process disables Usage, proves the in-flight completion is not recorded, then
re-enables recording and verifies the earlier record remains. Other tests cover
revision conflicts, read-only queries, retained-definition disconnect after source
removal, independent native/discovery gates, bounded dispatch, flush ordering and
failure isolation. No real account or external provider is used.
