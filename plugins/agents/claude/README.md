# Claude Code adapter plugin

`plugin.toml` declares an independently enabled Agent contribution. The host loads
its relative `definition.json` resource with `AgentDefinition::from_manifest_json`;
core has no Claude names, configuration keys or path discovery logic.

The common contribution contract is:

```toml
[[contributions.agents]]
id = "claude"
engine = "declarative-config/1"
definition = "definition.json"
```

The definition contains `schema_version`, `id`, `name`, `config_paths`, `format`,
`selection: { path, strip_prefix? }`, `reject_existing: [path]`, and
`fields: [{ path, value }]`. A path is an array of literal JSON keys, so dots and
slashes are not ambiguous. Values are scalars. `${model}` substitutes the unified
model ID, including in path segments; `${gateway}` substitutes the validated
loopback HTTP origin. The Registry must resolve/validate relative plugin resources
and environment/default discovery hints before registration. `config_paths` are
hints only; core opens only the explicit absolute target passed by the caller.

API in `magpie_core::agent_connection`:

- `read_selection(&definition, &path)` returns explicit `model` and ownership `status`.
- `prepare_connect(&definition, &path, model, SocketAddr)` writes nothing.
- `commit_connect(plan)` consumes the plan and compares exact bytes, file identity
  and ownership state before installing. `plan.revision()` is an opaque token;
  `changed_fields()` contains JSON pointers only. Across CLI invocations, prepare
  again and compare the provided revision before committing that fresh plan.
- `prepare_model_update(&definition, &path, model)` / `commit_model_update(plan)`
  retain the original recovery point and the previous endpoint.
- `disconnect(&definition, &path)` restores unchanged owned values and preserves
  external edits. An unchanged file restores the original bytes exactly.

Changing a definition while connected invalidates its ownership binding. Keep the
exact definition with the host's activated plugin version until disconnect; do not
replace it silently. Disabling a plugin withdraws its contribution from new work,
but must not delete its recovery directory or prevent explicit disconnect using
that retained definition. No Agent process is launched by this plugin.

## Protocol evidence

Reviewed 2026-11-02, no real account or Agent authentication files accessed:

- Fixed Magpie adapter source
  [`claude.go` at f1151226262022743bd103a78af41e1004cc6055](https://github.com/yetone/magpie/blob/f1151226262022743bd103a78af41e1004cc6055/internal/agent/claude.go).
- Official [Claude settings](https://code.claude.com/docs/en/settings),
  [model configuration](https://code.claude.com/docs/en/model-config), and
  [LLM gateway configuration](https://code.claude.com/docs/en/llm-gateway).

The adapter points the Anthropic origin at loopback, sets the main/default/fast
model slots and subagent slot, uses **dummy** local client authentication, and
disables the cloud-provider environment switches in this file. Dummy credentials
are never upstream credentials. Every overwritten value remains in private local
recovery data. A process-level environment variable, project settings, managed
settings, CLI flags or pre-existing running session can override this file; file
ownership status does not attest to runtime routing or installed Agent behavior.

## Verification boundary

Synthetic Linux filesystem tests cover preview, repeated model changes, exact
restore, external drift, stale revisions, malformed/duplicate JSON, private
materials and crash recovery. Native transaction code uses OS locks on Linux,
Android and macOS; Android/macOS device behavior is unverified. Windows mutations
return `UnsupportedPlatform`; cross-compilation is reported separately. No live
Claude invocation, GUI integration or network request is asserted here.

Private adjacent state is bounded (32 MiB / 256 entries; an individual recovery
journal is at most 4 MiB). Interrupted ambiguous state is retained and blocks writes.
Noncooperating writers must be quiescent: atomic rename is not a filesystem CAS.
