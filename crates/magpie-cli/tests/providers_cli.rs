#![cfg(unix)]

use std::{
    io::Write,
    os::unix::fs::PermissionsExt,
    path::PathBuf,
    process::{Command, Output, Stdio},
    sync::atomic::{AtomicU64, Ordering},
};

use magpie_core::provider_store::ProviderStore;
use serde_json::Value;

static NEXT: AtomicU64 = AtomicU64::new(0);

struct Fixture(PathBuf);

impl Fixture {
    fn new() -> Self {
        let path = std::env::temp_dir().canonicalize().unwrap().join(format!(
            "magpie-provider-cli-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir(&path).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700)).unwrap();
        Self(path)
    }

    fn command(&self) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_magpie"));
        command.env_clear().env("MAGPIE_CONFIG_DIR", &self.0).env(
            "DBUS_SESSION_BUS_ADDRESS",
            "unix:path=/nonexistent-magpie-fixture-bus",
        );
        command
    }

    fn run(&self, arguments: &[&str]) -> Output {
        self.command().args(arguments).output().unwrap()
    }

    fn success(&self, arguments: &[&str]) -> Value {
        let result = self.run(arguments);
        assert!(
            result.status.success(),
            "{}",
            String::from_utf8_lossy(&result.stderr)
        );
        serde_json::from_slice(&result.stdout).unwrap()
    }

    fn add(&self, id: &str) -> Value {
        self.success(&[
            "providers",
            "add",
            id,
            "--name",
            "Fixture 服务",
            "--url",
            "http://127.0.0.1:41973/v1",
            "--model",
            "alpha",
            "--json",
        ])
    }

    fn store(&self) -> ProviderStore {
        ProviderStore::load(&self.0.join("providers.toml")).unwrap()
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[test]
fn cli_crud_and_enabled_models_use_one_persistent_store() {
    let fixture = Fixture::new();
    let empty = fixture.success(&["providers", "--json"]);
    assert_eq!(empty["revision"], 0);
    assert_eq!(empty["providers"].as_array().unwrap().len(), 0);
    fixture.add("a");
    fixture.add("b");
    let catalog = fixture.success(&["models", "--json"]);
    assert_eq!(catalog["data"].as_array().unwrap().len(), 2);
    assert_eq!(catalog["data"][0]["id"], "a/alpha");
    assert_eq!(catalog["data"][1]["id"], "b/alpha");
    fixture.success(&["providers", "disable", "a", "--json"]);
    assert!(!fixture.store().provider("a").unwrap().enabled);
    assert_eq!(
        fixture.success(&["models", "--json"])["data"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    fixture.success(&[
        "providers",
        "edit",
        "b",
        "--name",
        "Renamed",
        "--model",
        "beta",
        "--json",
    ]);
    let store = fixture.store();
    let provider = store.provider("b").unwrap();
    assert_eq!(provider.name, "Renamed");
    assert_eq!(provider.base_url, "http://127.0.0.1:41973/v1");
    assert_eq!(provider.models, ["beta"]);
    let refused = fixture.run(&["providers", "remove", "b", "--json"]);
    assert!(!refused.status.success());
    assert!(fixture.store().provider("b").is_some());
    fixture.success(&["providers", "remove", "b", "--yes", "--json"]);
    fixture.success(&["providers", "enable", "a", "--json"]);
    assert_eq!(
        fixture.success(&["models", "--json"])["data"][0]["id"],
        "a/alpha"
    );
}

#[test]
fn stale_and_invalid_updates_preserve_existing_bytes() {
    let fixture = Fixture::new();
    fixture.add("a");
    let path = fixture.0.join("providers.toml");
    let before = std::fs::read(&path).unwrap();
    for arguments in [
        vec![
            "providers",
            "edit",
            "a",
            "--name",
            "Old editor",
            "--if-revision",
            "0",
            "--json",
        ],
        vec![
            "providers",
            "edit",
            "a",
            "--url",
            "http://example.com/v1",
            "--json",
        ],
        vec![
            "providers",
            "edit",
            "a",
            "--url",
            "https://user:synthetic-secret@example.com/v1",
            "--json",
        ],
        vec![
            "providers",
            "edit",
            "a",
            "--model",
            "same",
            "--model",
            "same",
            "--json",
        ],
    ] {
        let result = fixture.run(&arguments);
        assert!(!result.status.success());
        assert!(!String::from_utf8_lossy(&result.stderr).contains("synthetic-secret"));
        assert_eq!(std::fs::read(&path).unwrap(), before);
    }
}

#[test]
fn corrupt_configuration_is_never_replaced_by_an_empty_list() {
    let fixture = Fixture::new();
    let path = fixture.0.join("providers.toml");
    std::fs::write(&path, "version = ???\n").unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
    let before = std::fs::read(&path).unwrap();
    let output = fixture.run(&["providers", "--json"]);
    assert!(!output.status.success());
    assert!(output.stdout.is_empty());
    assert_eq!(std::fs::read(&path).unwrap(), before);
}

#[test]
fn help_at_any_position_does_not_write_configuration() {
    let fixture = Fixture::new();
    for args in [
        vec!["providers", "add", "--help"],
        vec!["providers", "edit", "missing", "--help"],
        vec!["providers", "remove", "--help"],
    ] {
        let result = fixture.run(&args);
        assert!(result.status.success());
        assert!(String::from_utf8_lossy(&result.stdout).contains("--key-stdin"));
        assert!(!fixture.0.join("providers.toml").exists());
    }
}

#[test]
fn environment_reference_is_public_but_its_value_is_not() {
    let fixture = Fixture::new();
    fixture.add("a");
    let result = fixture
        .command()
        .env("MAGPIE_FIXTURE_KEY", "synthetic-private-secret")
        .args([
            "providers",
            "edit",
            "a",
            "--key-env",
            "MAGPIE_FIXTURE_KEY",
            "--json",
        ])
        .output()
        .unwrap();
    assert!(result.status.success());
    let output = String::from_utf8(result.stdout).unwrap();
    assert!(output.contains("MAGPIE_FIXTURE_KEY"));
    assert!(!output.contains("synthetic-private-secret"));
    fixture.success(&[
        "providers",
        "edit",
        "a",
        "--name",
        "Keep reference",
        "--json",
    ]);
    assert_eq!(
        fixture
            .store()
            .provider("a")
            .unwrap()
            .api_key_env
            .as_deref(),
        Some("MAGPIE_FIXTURE_KEY")
    );
    fixture.success(&["providers", "edit", "a", "--clear-key", "--json"]);
    assert!(fixture.store().provider("a").unwrap().api_key_env.is_none());
}

#[test]
fn locked_or_absent_keyring_does_not_save_keyless_configuration() {
    let fixture = Fixture::new();
    let mut child = fixture
        .command()
        .args([
            "providers",
            "add",
            "a",
            "--name",
            "Fixture",
            "--url",
            "https://example.com/v1",
            "--model",
            "tiny",
            "--key-stdin",
            "--json",
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(b"synthetic-private-secret\n")
        .unwrap();
    let result = child.wait_with_output().unwrap();
    assert!(!result.status.success());
    assert!(!String::from_utf8_lossy(&result.stderr).contains("synthetic-private-secret"));
    assert!(fixture.store().providers().is_empty());
}
