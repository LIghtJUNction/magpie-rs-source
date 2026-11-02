# OpenCode adapter plugin

An independent `magpie-plugin/1` `agent-adapter` contribution. The host reads
`definition.json` and registers it with the generic core transaction API. See the
[common API and definition contract](../claude/README.md) for the typed interface,
preview revision checks, recovery and platform limits. The host owns discovery,
activation/deactivation and UI/CLI dispatch; the core has no OpenCode special cases.

Reviewed 2026-11-02 using the fixed Magpie
[`agents.go` adapter](https://github.com/yetone/magpie/blob/f1151226262022743bd103a78af41e1004cc6055/internal/agent/agents.go)
and official [custom provider documentation](https://opencode.ai/docs/providers/)
and [configuration documentation](https://opencode.ai/docs/config/).

The JSONC definition writes a custom `magpie` provider using
`@ai-sdk/openai-compatible`, its loopback `/v1` URL, a dummy local `apiKey`, and a
model entry keyed by the **complete unified model ID**. Both main and small-model
references have the OpenCode form `magpie/<provider>/<model>`; the read API strips
only the outer prefix while all owned fields match. A pre-existing `provider.magpie`
without this engine's ownership journal is rejected, including an empty block.
Switching models retains previous model entries until disconnect; the ownership
journal tracks each owned leaf so external headers, model metadata and additional
models survive a drifted disconnect. Unknown fields and comments are preserved.

This plugin targets the documented `npm` / AI SDK custom provider protocol; it
makes no compatibility claim for alternate schema generations. Duplicate keys,
malformed JSONC, non-object roots, missing files and scalar/object type conflicts
are rejected without replacing the configuration. This plugin never reads or edits
OpenCode's separate authentication store. Runtime OpenCode version acceptance,
CLI/GUI integration and request routing remain separate integration checks.
