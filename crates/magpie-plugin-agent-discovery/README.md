# Agent discovery catalog plugin

`feature.agent-discovery` supplies the compiled `agent-discovery/1` inventory
engine declared in `plugins/features/agent-discovery/plugin.toml`. The
application registers the engine explicitly and shows inventory only while the
Registry owner is active. Disabling it hides inventory; it does not delete
configuration or recovery data.

`agent_catalog` retains all 68 requirement identities and their original order,
name rules, ambiguity and uncertainty states. These are required products and
filename candidates, not 68 implemented or active configuration adapters.
`agents` supplies the existing bounded installed-agent discovery entry points.
It performs metadata-only PATH scans without executing candidates or reading
account files. Codex's actual configuration/discovery adapter is a separate
plugin and is not a dependency of this catalog.

All existing catalog/discovery tests have moved with the code. The full parent
requirement set remains in force; this source package does not reduce the 99
requirements to the few currently implemented adapters.
