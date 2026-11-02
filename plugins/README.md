# Magpie plugin contract

Core contains the registry and generic engines. Applications compose bundled
manifests from independent packages and inject them into the registry. Core
contains no bundled vendor, product or Usage manifest and does not discover them
by Cargo feature. This document describes the API-1 declarative foundation;
Node/npm plugins, external executable hosts, migration, sign-in and JavaScript
middleware remain required future work, not working features of this foundation.

```toml
format = "magpie-plugin/1"
id = "provider.example"
name = "Example"
version = "0.1.0"
api_version = 1
kind = "provider-preset"
enabled_by_default = true
requires = []
provides = ["provider-preset"]

[[contributions.providers]]
id = "example"
name = "Example"
base_url = "http://127.0.0.1:9999/v1"
protocol = "openai_chat"
requires_key = true
source = "https://example.invalid/documentation"
```

IDs and service names are lowercase ASCII dot-separated components beginning
with a letter, then letters/digits/underscore/hyphen, at most 128 bytes. IDs are
unique across bundled and user manifests; **all** matching duplicate IDs fail,
so user packages cannot silently replace bundled owners. The format and API must
match exactly. Versions have three unsigned numeric components and optional
prerelease/build identifiers. Envelope parsing validates metadata, not every
contribution's schema or external endpoint compatibility.

Kinds remain distinct: `provider-preset`, `agent-adapter`, `feature`,
`community-provider`, `mcp-server`, `gateway-middleware`. The last three are
explicitly `unsupported` in this foundation and cannot be acknowledged active.
Disabling them changes the status to `off`; enabling does not install or run them.
An unrecognized kind/API/format yields a sanitized discovery diagnostic, not a
healthy record. Invalid declarations do not prevent valid siblings loading.

The original TOML document is passed to the application through
`PluginManifest::document()`. Registry imposes no vendor or feature schema.
Provider packages use `[[contributions.providers]]`; Agent packages use the
schema accepted by the generic Agent engine. Features can use
`[[contributions.pages]]`, `[[contributions.commands]]` and
`[[contributions.metadata_subscribers]]`. Typed engines must reject malformed,
unsupported or colliding contribution IDs before activation. A contribution may
not shadow another owner's page, action, adapter or preset. Settings declarations
can live under `contributions`; they do not make configuration editing executable.
Unknown contributions must cause registration failure or remain pending in the
host, never silently count as a complete activation.

## Host integration

The public API is `magpie_core::plugins`:

- `PluginInput::bundled(label, text)` injects application-composed bundles.
- `PluginRegistry::from_manifests(inputs)` creates an in-memory registry.
- `PluginRegistry::load(inputs, user_directory, state_path)` reads user manifests
  and persisted preferences. Both paths must be absolute. Default helpers use
  the same config directory as ProviderStore: `plugins/` and `plugins.toml`.
- `records()`, `record(id)`, `diagnostics()` and `revision()` are read-only state.
  Every record includes validated metadata, source, kind, enabled preference and
  lifecycle; `owner()` is its manifest ID. Unparseable envelopes appear in
  `diagnostics()` with source and sanitized error, rather than guessed IDs.
- `set_host_services(iter<String>)` declares services actually installed by the
  application. Services provided by a plugin count only after activation. Several
  plugins may provide the same service; withdrawing one retains the others.
- `pending_contributions()` returns only declarations ready for registration.
  Each carries `owner`, `source`, `kind` and `manifest`. Call typed engines to
  validate and register **all** contributions transactionally, then call
  `acknowledge_activation(id)`. Repeat to activate newly satisfied dependencies.
  Registry parsing alone never yields `active`. Dependency cycles or missing
  services stay `pending` with `waiting_for` service names.
- `contributions()` exposes only acknowledged active owners, for normal dispatch.
- If registration fails, roll back everything partially registered by that owner
  and call `fail_activation(id)`. It marks a sanitized failure and returns all
  affected owners in reverse activation order. Do not pass raw engine errors,
  TOML excerpts or secret-bearing payloads into ordinary diagnostics.
- `set_enabled(id, bool, expected_revision)` persists the preference, recomputes
  dependencies and returns owners to withdraw, dependents first. The host must
  synchronously dispose these owners' registrations **before** serving another
  UI action or request. This includes pages, commands, adapters and observers;
  disabling Usage must detach its observer, not merely hide its page. Closing
  already in-flight work follows the relevant engine's cancellation contract.
- `reload()` rereads manifests and preferences. On success it returns **all**
  previously active owners for reverse disposal and requires fresh registration.
  On error it preserves the previous registry and returns a readable failure;
  the caller must display the failed refresh rather than report a healthy reload.

The acknowledgement interface makes registration/disposal an explicit host
responsibility. This foundation does not execute Apply code, catch host panics,
perform engine rollback itself, enforce essential/in-use feature locks or own
live observer handles. Hosts must implement those policies and transactional
cleanup before claiming those acceptance cases. `fail_activation` can represent
an isolated host failure, but its existence is not evidence that arbitrary
executable plugins are isolated or sandboxed. No automatic package installation
or external process launch occurs.

Turning a plugin off retracts contributions; it never deletes credentials,
accounts, user configuration or plugin data. Unknown stored preference IDs are
retained for reinstall. Failed registrations retry after an explicit off/on or
reload. Enablement revision is separate from ephemeral runtime activation.

## Files and limits

User discovery reads only `<user_directory>/*.toml` and
`<user_directory>/<package>/plugin.toml`; it does not recurse into repository,
cache or npm trees. A missing directory is empty, but an unreadable directory is
an error. Missing manifest files inside unrelated directories are ignored.
Discovery accepts at most 256 manifests total, scans at most 1024 directory
entries, caps each file at 256 KiB, and caps each service list at 64 entries.
One invalid file remains a source-specific diagnostic. Reaching a global bound
returns an error instead of claiming that a partial scan was complete.

Private state uses `format = "magpie-plugin-state/1"`, a nonnegative revision and
an `[enabled]` table with quoted plugin IDs. It rejects unknown state fields,
malformed flags, incompatible formats and oversized state. Missing is distinct
from broken. Errors never include document excerpts. `Debug` for manifests,
inputs and registries excludes contribution payloads and raw private state.
Do not put credentials in manifests; store secure references with the credential
engine when needed.

Linux, Android and macOS use descriptor-relative no-follow opens, reject
symlinks in parent paths, reject non-regular or hardlinked inputs, use a private
0600 lock/state file, an advisory nonblocking lock, expected revision and an
exact-byte snapshot comparison, then same-directory atomic rename and fsync.
New directories use 0700. Existing file protections are never widened. State
files with any group/other permission are rejected. Manifests may be world
readable. External writers ignoring the lock can race the final check/rename;
observed conflicts preserve the external file. An fsync failure after replacement
returns `CommitUncertain`; reload before retrying. Secure on-disk loading and
mutation on other platforms are explicitly unsupported here; in-memory injection
still works. Windows support remains required and must not be reported complete.

## Local verification

Synthetic fixtures only; no account credentials or provider calls are used.
Build products remain outside the checkout:

```sh
CARGO_INCREMENTAL=0 CARGO_PROFILE_DEV_DEBUG=0 \
CARGO_TARGET_DIR=/home/lightjunction/.cache/magpie-rs-build/smoke-20261102/target-cli \
cargo +1.99.1 test --locked -p magpie-core plugins:: --lib
```

Tests cover parsed-versus-active states, dependency waits/cascading reverse
withdrawal, duplicate owners, independent failures, new declarative Agent/Provider
inputs, unsupported executable kinds, shared services, persisted off/on and
restart, stale revision and same-revision external edits, corrupt state retention,
no-follow paths, hardlinks, private permissions, bounds and revision overflow.
These checks prove the generic registry foundation; GUI/CLI engine integration,
actual executable plugin execution and real OS/device acceptance are separate.
