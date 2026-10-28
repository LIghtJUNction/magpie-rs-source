//! Tests the existing desktop data adapter, not a GPUI window. Environment
//! changes apply only to an isolated child process; no unsafe set_var is used.
#![cfg(any(target_os = "linux", target_os = "android", target_os = "macos"))]

#[allow(dead_code)]
#[path = "../../../apps/desktop/src/snapshot.rs"]
mod desktop_snapshot;

use magpie_core::agents::{self, ConfigFileState};
use magpie_core::codex_transaction::{
    ConnectionStatus, commit_connect, connection_status, disconnect, prepare_connect,
};
use std::fs::{self, DirBuilder, OpenOptions};
use std::io::Write;
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

static NEXT_FIXTURE: AtomicU64 = AtomicU64::new(0);
const CHILD_FIXTURE_ENV: &str = "MAGPIE_DESKTOP_SNAPSHOT_TEST_FIXTURE";
const VERIFIED: &[u8] =
    b"desktop adapter and CLI discovery matched before/after commit/disconnect\n";
const ORIGINAL: &str = r#"# Synthetic adapter fixture; never a user's configuration.
model = "original/model"
model_provider = "original-provider"
model_reasoning_effort = "medium"

[model_providers.original-provider]
name = "Synthetic Original"
base_url = "https://fixture.invalid/v1"

[model_providers.original-provider.http_headers]
X-Fixture = "unchanged"
"#;

struct Fixture(PathBuf);

impl Fixture {
    fn new() -> Self {
        let timestamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("fixture clock")
            .as_nanos();
        let directory = std::env::temp_dir()
            .canonicalize()
            .expect("fixture temporary directory")
            .join(format!(
                "magpie-desktop-snapshot-{}-{timestamp}-{}",
                std::process::id(),
                NEXT_FIXTURE.fetch_add(1, Ordering::Relaxed)
            ));
        DirBuilder::new()
            .mode(0o700)
            .create(&directory)
            .expect("create isolated fixture");
        for child in ["bin", "codex-home"] {
            DirBuilder::new()
                .mode(0o700)
                .create(directory.join(child))
                .expect("create fixture subdirectory");
        }
        write_new(
            &directory.join("codex-home/config.toml"),
            ORIGINAL.as_bytes(),
            0o600,
        );
        // Discovery inspects only executable metadata. This marker deliberately
        // has no interpreter or executable format and must never be launched.
        write_new(
            &directory.join("bin/codex"),
            b"synthetic executable metadata fixture; must never execute\n",
            0o700,
        );
        Self(directory)
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn write_new(path: &Path, bytes: &[u8], mode: u32) {
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(mode)
        .open(path)
        .expect("create fixture file");
    file.write_all(bytes).expect("write fixture file");
}

#[test]
fn desktop_snapshot_and_cli_discovery_share_committed_and_restored_configuration() {
    let fixture = Fixture::new();
    let output = Command::new(std::env::current_exe().expect("current test executable"))
        .args([
            "--ignored",
            "--exact",
            "isolated_snapshot_worker",
            "--nocapture",
            "--test-threads=1",
        ])
        .env_clear()
        .env(CHILD_FIXTURE_ENV, &fixture.0)
        .env("CODEX_HOME", fixture.0.join("codex-home"))
        .env("PATH", fixture.0.join("bin"))
        .current_dir(&fixture.0)
        .output()
        .expect("launch isolated adapter test worker");
    assert!(
        output.status.success(),
        "isolated adapter worker failed:\nstdout: {}\nstderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );
    assert_eq!(
        fs::read(fixture.0.join("adapter-verified")).expect("child must run all adapter checks"),
        VERIFIED,
        "a child runner that accidentally selected zero tests must not pass",
    );
    assert_eq!(
        fs::read_to_string(fixture.0.join("codex-home/config.toml")).unwrap(),
        ORIGINAL,
        "worker must complete the reversible transaction",
    );
}

#[test]
#[ignore = "invoked only by the parent fixture with an isolated child environment"]
fn isolated_snapshot_worker() {
    let fixture = PathBuf::from(
        std::env::var_os(CHILD_FIXTURE_ENV).expect("isolated fixture environment is required"),
    );
    let codex_home = fixture.join("codex-home");
    assert_eq!(
        std::env::var_os("CODEX_HOME").unwrap(),
        codex_home.as_os_str()
    );
    assert_eq!(
        std::env::var_os("PATH").unwrap(),
        fixture.join("bin").as_os_str()
    );
    assert!(std::env::var_os("HOME").is_none());
    assert!(std::env::var_os("USERPROFILE").is_none());
    let path = codex_home.join("config.toml");
    assert_eq!(fs::read_to_string(&path).unwrap(), ORIGINAL);

    assert_shared_selection(&fixture, "original/model", "original-provider", "medium");
    let plan = prepare_connect(
        &path,
        "fixture-upstream/model-a",
        "127.0.0.1:3425".parse().unwrap(),
    )
    .unwrap();
    assert_eq!(fs::read_to_string(&path).unwrap(), ORIGINAL);
    assert_shared_selection(&fixture, "original/model", "original-provider", "medium");

    assert_eq!(commit_connect(plan).unwrap(), ConnectionStatus::Connected);
    assert_eq!(
        connection_status(&path).unwrap(),
        ConnectionStatus::Connected
    );
    assert_shared_selection(&fixture, "fixture-upstream/model-a", "magpie", "medium");

    let outcome = disconnect(&path).unwrap();
    assert_eq!(outcome.restored_fields, 7);
    assert_eq!(outcome.preserved_fields, 0);
    assert_eq!(
        connection_status(&path).unwrap(),
        ConnectionStatus::Unmanaged
    );
    assert_shared_selection(&fixture, "original/model", "original-provider", "medium");
    assert_eq!(fs::read_to_string(path).unwrap(), ORIGINAL);
    write_new(&fixture.join("adapter-verified"), VERIFIED, 0o600);
}

fn assert_shared_selection(fixture: &Path, model: &str, provider: &str, effort: &str) {
    let discovery = agents::discover_codex();
    let snapshot = desktop_snapshot::Snapshot::read();
    assert_eq!(snapshot.agents.len(), 1, "fixture PATH exposes only Codex");
    let desktop = &snapshot.agents[0];
    assert_eq!(desktop.id, "codex");
    assert_eq!(desktop.executable, fixture.join("bin/codex"));
    assert_eq!(
        discovery.executable.as_deref(),
        Some(desktop.executable.as_path())
    );

    let config = desktop
        .config
        .as_ref()
        .expect("Codex adapter configuration");
    assert_eq!(
        config.path.as_deref(),
        Some(fixture.join("codex-home/config.toml").as_path())
    );
    assert_eq!(config.path, discovery.config_path);
    assert_eq!(config.state, ConfigFileState::ValidToml);
    assert_eq!(config.state, discovery.config_state);
    assert!(config.fields_readable);
    assert_eq!(config.error_line, None);
    assert!(
        !desktop.last_known,
        "a successful fresh read must not be stale"
    );

    let settings = desktop
        .settings
        .as_ref()
        .expect("readable adapter selection");
    assert_eq!(settings.model, discovery.selection.model);
    assert_eq!(settings.provider, discovery.selection.model_provider);
    assert_eq!(settings.effort, discovery.selection.model_reasoning_effort);
    assert_eq!(settings.model.as_deref(), Some(model));
    assert_eq!(settings.provider.as_deref(), Some(provider));
    assert_eq!(settings.effort.as_deref(), Some(effort));
}
