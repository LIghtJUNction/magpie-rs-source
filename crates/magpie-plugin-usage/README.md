# Recent Usage plugin

`feature.usage` is an optional source plugin. It stores only bounded request
metadata. The Gateway does not depend on this crate or automatically enable it.

The application bundle loads `MANIFEST` into `PluginRegistry`, validates the
`commands`, `pages`, and `metadata_subscribers` contributions, constructs
`UsagePlugin::new` with the explicitly selected ProviderStore path, installs it
as a `magpie_core::request_metadata::MetadataSubscriber`, and only then calls
`acknowledge_activation`. The compatibility `usage` command dispatches to
`command::execute`; its help text is `command::HELP`. Registry ownership applies
to the command, page and observer together.

On disable, the host first stops dispatching to the withdrawn owner, drains its
accepted events using `flush`, then releases it. On re-enable a fresh plugin
instance resumes the same on-disk window. Disable never removes stored data.
The host must not construct plugins against the default store before parsing an
explicit `--store` argument.

The metadata directory appends `.usage` to the entire store filename, such as
`/abs/providers.toml.usage/`. Existing data from the first local implementation
remains readable without migration. The directory is private (0700), files are
private regular single-link files (0600), and descriptor-relative opens reject
symlinks throughout the path. Reads, appends, bounded tail repair and two-segment
rotation share an advisory process lock.

Each segment is at most 4 MiB; each record is at most 4096 bytes. The queue holds
128 events. Health reports pending/written/failed/dropped/repaired_tails counters
for the plugin instance's lifetime. A written record has completed file and
directory sync. Normal host shutdown must drop request tasks and then flush the
subscriber. Abrupt termination can lose queued events or leave a partial tail;
offline reports cannot reconstruct those lost events.

`report` and the CLI return `scope: recent_requests`. Totals cover only the
returned newest-first window, not lifetime billing. Unknown tokens stay null;
mixed windows sum reported values and expose per-field reporting counts. Sum
overflow returns null and `token_sum_overflow: true`. Failed/cancelled requests
can retain known partial usage; this is an observation rather than final billing.
No prompts, response text, headers, keys, raw errors, account identity, prices or
quota data are stored.

The protocol token decoder remains in the Gateway. This package depends only on
core's generic metadata events, never on the service or its protocol enums.
Linux is locally tested; secure storage is compiled for Linux/Android/macOS and
returns an explicit unsupported-platform error elsewhere. No other device
execution or published plugin package is implied.
