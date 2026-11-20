# OpenManus source 3309bf4

This **disabled by default** adapter targets FoundationAgents/OpenManus source
`3309bf4e416fb1c74b008f3e86494439a31bad53`, not OpenHands or a guessed numbered
release. Explicitly enable `agent.openmanus` before configuring.

OpenManus resolves `PROJECT_ROOT/config/config.toml` (or its example fallback).
There is no verified universal per-user install location, so `config_paths` is
empty. Select an **existing absolute** `<OpenManus checkout>/config/config.toml`
explicitly. Do not connect the example fallback: create your working config from
it yourself. No install-directory or account discovery is claimed.

The adapter owns the primary `[llm]` and standard `[llm.vision]` `model`,
`base_url`, `api_key`, `api_type` scalars. Both select the same public qualified
Gateway model. Base URL `${gateway}/v1` and `api_type = ""` select the upstream
OpenAI-compatible client branch; `magpie-local` is a public local marker, never
an upstream credential. The Gateway handles upstream credentials. Existing
max_tokens, temperature, custom named LLM overrides and browser/search/MCP
settings remain unchanged. **Choose a model supporting the required tool/vision
features and existing token budgets**; this adapter does not invent capability
metadata or guarantee compatibility with every configured task. Custom named
LLM overrides or auxiliary browser/search services may use other endpoints.

Unknown TOML fields/comments survive model changes. With no external changes,
disconnect restores exact original bytes; with drift only still-owned scalars
are restored. Adjacent recovery images are private local plaintext. The host
rejects in-use disable and preserves recovery data. Restart OpenManus after
configuration changes. Successful file configuration is not a claim that
OpenManus, browser automation, vision models or its sandbox were run/tested.

## Pinned primary sources

- [Configuration resolver, default/overrides mapping](https://github.com/FoundationAgents/OpenManus/blob/3309bf4e416fb1c74b008f3e86494439a31bad53/app/config.py)
- [Primary and vision example settings](https://github.com/FoundationAgents/OpenManus/blob/3309bf4e416fb1c74b008f3e86494439a31bad53/config/config.example.toml)
- [Named-model fallback and OpenAI client selection](https://github.com/FoundationAgents/OpenManus/blob/3309bf4e416fb1c74b008f3e86494439a31bad53/app/llm.py)

Acceptance uses synthetic fixtures plus real loopback Gateway HTTP requests from
the generated file fields. It does not launch OpenManus or access real account
credentials. This adapter is separate from OpenHands; current OpenHands CLI uses
serialized Agent settings and is deliberately not assigned an obsolete TOML
mapping. Full 99-requirement/catalog coverage remains outstanding.
