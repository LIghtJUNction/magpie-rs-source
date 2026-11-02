# Native Codex agent plugin

`agent.codex` supplies the compiled `native-codex/1` adapter declared in
`plugins/agents/codex/plugin.toml`. Its source owns Codex TOML selection,
configuration preview, reversible connection/model updates, recovery and
Codex-specific path/executable discovery. It does not depend on the optional
68-name discovery catalog.

The application must explicitly register this compiled engine and check the
Registry owner before exposing normal Codex commands. Parsing the manifest is
not engine registration. Disable must preserve all configuration, journals,
plaintext recovery images, locks and abandoned fragments. The application must
keep explicit disconnect/recovery reachable, or reject disabling an in-use
adapter. A status error is not evidence that a target is unmanaged.

Existing entry points remain in `agents`, `codex_config` and
`codex_transaction`. `connection_status(path)` returns Unmanaged, Connected,
Drifted or RecoveryRequired. `disconnect(path)` restores only fields still
owned by the existing transaction. The private on-disk state format and paths
are unchanged, so existing recovery material remains usable.

The transaction implementation and its complete crash/locking/recovery suite
were moved without deleting test definitions or weakening assertions. Fault
injection and descriptor/fork coordination now use vendor-neutral
`magpie_core::test_support`, enabled only by the test-support feature in this
package's dev-dependencies. The core engine no longer borrows Codex code for its
tests. All account/configuration fixtures are isolated synthetic files.

Generic path/home helpers and ConfigFileState come from core's config_probe;
gateway defaults come from core's gateway API. Native secure writes retain the
existing Linux/Android/macOS support boundary; other targets return an explicit
unsupported-platform error. Local Linux tests do not imply other device runs.

The existing desktop snapshot/imports can consume this package. The old
hardcoded Codex panel/factory is an application migration still to be completed
by the generic connection UI; moving this crate alone does not complete that UI
migration or the full 99-requirement product contract.

## Uniform plugin command contract

`command::execute(&[String]) -> Result<serde_json::Value, String>` provides
`selection`, `preview`, `connect`, `model-preview`, `model`, and `disconnect`.
The first argument is the operation. `command::configuration_path` accepts the
same complete argument vector and resolves its validated target for the host's
private target association before apply. The CLI registers these functions under
`plugins call agent.codex`; the runtime owner gate stays in application composition.

- `selection` and `disconnect` accept only optional `--config ABS`.
- `preview` and `connect` require `--model provider/model` and
  `--listen LOOPBACK:PORT`, with optional `--config ABS`.
- `model-preview` and `model` require `--model provider/model`, reject `--listen`,
  and preserve the connected endpoint.
- `connect` and `model` require the exact `--if-revision` from the corresponding
  preview. It is an opaque lowercase 64-hex digest, covering the operation,
  model/endpoint intent, target identity, file contents and ownership context.
  Apply prepares a fresh plan and compares before committing. Future operations
  need a new preview. No target/configuration/recovery files are created by preview.

If `--config` is omitted, this package resolves its own Codex discovery default
(`CODEX_HOME/config.toml`, otherwise the usual home-based config). The inferred
path is omitted from JSON. Explicit paths must be absolute and bounded.

Responses use the same Agent contract as the declarative engine:
`version`, `agent_id`, `operation`, `status`,
`selection:{model,state}`, `selection_scope`, and where applicable
`revision`, `changed_fields`, `applied`, and disconnect's restoration counts.
Unqualified configured models are `model:null,state:unknown`; no configured
Provider, credential, original value or source excerpt is emitted. Disconnect
never reads back and publishes restored original values. Repeating disconnect
against confirmed unmanaged state is an explicit no-op, leaving user-owned fields
untouched. Unsafe or uncertain state still fails closed.

This command layer edits configuration only. It performs no Agent launch,
upstream request, credential access or gateway readiness check. Existing legacy
CLI commands retain their separate behavior and network probes. Application
composition records custom targets before mutation, refuses ordinary calls while
off, retains the in-use disable guard, and keeps explicit disconnect available
while the plugin is disabled. None of these states attest to an installed/running
Agent using the configured endpoint.
