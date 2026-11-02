# Kimi CLI 1.52 (Python)

This **disabled by default** adapter targets the archived Python `kimi-cli`
1.52.0 snapshot `9ab1286b8fe4e6bcd116949a27ce5e0ac3389c82`. It does **not**
claim compatibility with the successor TypeScript Kimi Code or its new paths and
schemas. Explicitly enable `agent.kimi-cli` in the plugin host before configuring.

The only path hint is `${home}/.kimi/config.toml`. Alternatively select the
absolute existing file passed to Python `kimi --config-file PATH`; the engine
never creates parent directories or selects an account. It owns `default_model`,
a reserved `providers.magpie` OpenAI legacy/chat-completions provider, a reserved
`models.magpie` model, and the response reserve in `loop_control`.
Pre-existing `magpie` provider/model aliases are refused before writes.

The provider base is `${gateway}/v1`; its `api_key` is the public local marker
`magpie-local`, not an upstream credential. Provider credentials belong to the
Gateway. The actual public qualified model goes in `models.magpie.model`;
`default_model` selects the local alias `magpie`. Context 32768 and response
reserve 8192 are explicit **local budgeting policy**, not measured model limits
or provider metadata. Use only models supporting that budget, and validate the
budget for your deployment. Image/thinking capabilities are deliberately not
invented. Existing accounts/services/hooks/MCP and unrelated fields remain owned
by the user; those optional services can still contact their original endpoints.
CLI model/config flags and environment overrides may bypass persisted selection;
this adapter does not modify process environment or claim those overrides absent.

Connect/model changes preserve unknown TOML fields and comments. Disconnect
restores the exact first bytes when there is no external drift; otherwise it
restores only still-owned scalars and keeps external edits. Private adjacent
recovery journals remain plaintext locally. In-use disable is rejected by the
host; explicit disconnect/recovery remains possible. Restart the Agent to load
new configuration; process hot reload, authentication and Agent execution are
not inferred from a successful configuration transaction.

## Pinned primary sources

- [Project version](https://github.com/MoonshotAI/kimi-cli/blob/9ab1286b8fe4e6bcd116949a27ce5e0ac3389c82/pyproject.toml)
- [Config path, flags, default/model/provider fields](https://github.com/MoonshotAI/kimi-cli/blob/9ab1286b8fe4e6bcd116949a27ce5e0ac3389c82/docs/en/configuration/config-files.md)
- [Required provider/model schema](https://github.com/MoonshotAI/kimi-cli/blob/9ab1286b8fe4e6bcd116949a27ce5e0ac3389c82/src/kimi_cli/config.py)
- [openai_legacy dispatch and environment overlays](https://github.com/MoonshotAI/kimi-cli/blob/9ab1286b8fe4e6bcd116949a27ce5e0ac3389c82/src/kimi_cli/llm.py)

Acceptance uses isolated synthetic files and actual loopback Gateway requests
constructed from the resulting configuration. It does not launch Kimi CLI or
read installed account files. This counts as one version-scoped configuration
adapter, not current Kimi Code support or completion of the full Agent catalog.
