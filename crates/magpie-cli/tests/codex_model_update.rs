//! Model updates through isolated CLI children and a local synthetic Gateway.
#![cfg(any(target_os = "linux", target_os = "android", target_os = "macos"))]

use magpie_core::codex_transaction::{
    commit_connect, commit_model_update, prepare_connect, prepare_model_update,
};
use magpie_core::provider_store::{Provider, ProviderProtocol, ProviderStore};
use magpie_service::forward::ForwardServer;
use serde_json::Value;
use std::fs::{self, DirBuilder, OpenOptions};
use std::io::Write;
use std::net::SocketAddr;
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc;
use std::thread::{self, JoinHandle};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

static NEXT: AtomicU64 = AtomicU64::new(0);
const ORIGINAL: &str = "# synthetic configuration\nmodel = 'user/original'\nmodel_provider = 'user'\nmodel_reasoning_effort = 'high'\n[model_providers.user]\nbase_url = 'https://user.invalid/v1'\n[model_providers.user.http_headers]\nAuthorization = 'SYNTHETIC_PRIVATE_CANARY'\n";

struct Fixture(PathBuf);
impl Fixture {
    fn new(contents: &str) -> Self {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path = std::env::temp_dir().canonicalize().unwrap().join(format!(
            "magpie-model-update-cli-{}-{now}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed),
        ));
        DirBuilder::new().mode(0o700).create(&path).unwrap();
        write_private(&path.join("config.toml"), contents.as_bytes());
        Self(path)
    }
    fn config(&self) -> PathBuf {
        self.0.join("config.toml")
    }
    fn text(&self) -> String {
        fs::read_to_string(self.config()).unwrap()
    }
    fn connect(&self, address: SocketAddr) {
        commit_connect(prepare_connect(&self.config(), "fixture/initial", address).unwrap())
            .unwrap();
    }
    fn command(&self) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_magpie"));
        command
            .env_clear()
            .current_dir(&self.0)
            .env("MAGPIE_CONFIG_DIR", &self.0);
        command
    }
    fn update(&self, model: &str, mode: &str, revision: Option<&str>) -> Output {
        let mut command = self.command();
        command
            .args(["agents", "set-model", "codex", "--model", model, "--config"])
            .arg(self.config())
            .args([mode, "--json"]);
        if let Some(revision) = revision {
            command.args(["--if-revision", revision]);
        }
        command.output().unwrap()
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}
fn write_private(path: &Path, bytes: &[u8]) {
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)
        .unwrap();
    file.write_all(bytes).unwrap();
}

struct Gateway {
    address: SocketAddr,
    owner: Option<UnixStream>,
    worker: Option<JoinHandle<std::io::Result<()>>>,
}
impl Gateway {
    fn start(fixture: &Fixture, models: &[&str]) -> Self {
        let path = fixture.0.join("providers.toml");
        ProviderStore::create(
            &path,
            Provider {
                id: "fixture".into(),
                name: "Synthetic Fixture".into(),
                protocol: ProviderProtocol::OpenAiResponses,
                base_url: "http://127.0.0.1:1/v1".into(),
                api_key_env: None,
                credential_ref: None,
                models: models.iter().map(|s| (*s).to_owned()).collect(),
                enabled: true,
            },
            0,
        )
        .unwrap();
        let server = ForwardServer::bind_store("127.0.0.1:0".parse().unwrap(), path).unwrap();
        let address = server.local_addr().unwrap();
        let (owner, lease) = UnixStream::pair().unwrap();
        let (sender, receiver) = mpsc::sync_channel(1);
        let worker = thread::spawn(move || {
            server.run_supervised(lease, |_| {
                sender
                    .send(())
                    .map_err(|_| std::io::Error::other("fixture owner disappeared"))
            })
        });
        receiver
            .recv_timeout(Duration::from_secs(5))
            .expect("bounded Gateway ready event");
        Self {
            address,
            owner: Some(owner),
            worker: Some(worker),
        }
    }
}
impl Drop for Gateway {
    fn drop(&mut self) {
        drop(self.owner.take());
        if let Some(worker) = self.worker.take() {
            worker.join().unwrap().unwrap();
        }
    }
}

fn receipt(output: Output) -> Value {
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(output.stderr.is_empty());
    assert!(!String::from_utf8_lossy(&output.stdout).contains("SYNTHETIC_PRIVATE_CANARY"));
    let receipt: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(receipt["operation"], "agents.set-model");
    let revision = receipt["revision"].as_str().unwrap();
    assert_eq!(revision.len(), 64);
    assert!(
        revision
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    );
    receipt
}

fn rejected(output: Output, code: &str) {
    assert!(!output.status.success());
    assert!(output.stdout.is_empty());
    assert!(!String::from_utf8_lossy(&output.stderr).contains("SYNTHETIC_PRIVATE_CANARY"));
    let error: Value = serde_json::from_slice(&output.stderr).unwrap();
    assert_eq!(error["error"]["code"], code);
}

#[test]
fn preview_apply_same_model_and_disconnect_preserve_first_user_configuration() {
    let fixture = Fixture::new(ORIGINAL);
    let gateway = Gateway::start(&fixture, &["initial", "next"]);
    fixture.connect(gateway.address);
    let before = fixture.text();
    let journal_path = fixture.0.join(".config.toml.magpie-state/journal.toml");
    let journal = fs::read(&journal_path).unwrap();
    let preview = receipt(fixture.update("fixture/next", "--dry-run", None));
    assert_eq!(preview["dry_run"], true);
    assert_eq!(preview["writes_performed"], false);
    assert_eq!(preview["state"], "connected");
    assert_eq!(preview["gateway_readiness"], "not_checked");
    assert_eq!(preview["gateway_address"], gateway.address.to_string());
    assert_eq!(preview["changed_fields"], serde_json::json!(["model"]));
    assert_eq!(fixture.text(), before);
    assert_eq!(fs::read(&journal_path).unwrap(), journal);

    let applied = receipt(fixture.update("fixture/next", "--apply", preview["revision"].as_str()));
    assert_eq!(applied["writes_performed"], true);
    assert_eq!(applied["state"], "connected");
    assert_eq!(
        applied["gateway_readiness"],
        "listener_and_configured_model_checked"
    );
    assert_ne!(applied["revision"], preview["revision"]);
    assert_eq!(applied["provider_readiness"], "not_probed");
    assert_eq!(applied["agent_execution"], "not_checked");
    assert_eq!(
        fixture.text(),
        before.replace("fixture/initial", "fixture/next")
    );
    let preview = receipt(fixture.update("fixture/next", "--dry-run", None));
    assert_eq!(preview["revision"], applied["revision"]);
    let same = receipt(fixture.update("fixture/next", "--apply", preview["revision"].as_str()));
    assert_eq!(same["writes_performed"], false);
    assert_eq!(same["changed_fields"], serde_json::json!([]));
    assert_eq!(same["revision"], preview["revision"]);
    let output = fixture
        .command()
        .args(["agents", "disconnect", "codex", "--config"])
        .arg(fixture.config())
        .arg("--json")
        .output()
        .unwrap();
    assert!(output.status.success());
    assert_eq!(fixture.text(), ORIGINAL);
}

#[test]
fn stale_cli_preview_rejects_unrelated_edits_same_bytes_replacement_and_other_updates() {
    for change in ["unrelated-edit", "replacement", "other-update"] {
        let fixture = Fixture::new(ORIGINAL);
        // Rejection must happen before a network probe. No service listens here.
        fixture.connect("127.0.0.1:1".parse().unwrap());
        let preview = receipt(fixture.update("fixture/next", "--dry-run", None));
        match change {
            "unrelated-edit" => fs::write(
                fixture.config(),
                format!("# new unrelated edit\n{}", fixture.text()),
            )
            .unwrap(),
            "replacement" => {
                let replacement = fixture.0.join("replacement.toml");
                write_private(&replacement, fixture.text().as_bytes());
                fs::rename(replacement, fixture.config()).unwrap();
            }
            _ => {
                commit_model_update(
                    prepare_model_update(&fixture.config(), "fixture/another").unwrap(),
                )
                .unwrap();
            }
        }
        let current = fixture.text();
        rejected(
            fixture.update("fixture/next", "--apply", preview["revision"].as_str()),
            "config_transaction_error",
        );
        assert_eq!(fixture.text(), current);
        let fresh = receipt(fixture.update("fixture/next", "--dry-run", None));
        assert_ne!(fresh["revision"], preview["revision"]);
    }
}

#[test]
fn absent_gateway_model_rejects_update_without_configuration_or_recovery_writes() {
    let fixture = Fixture::new(ORIGINAL);
    let gateway = Gateway::start(&fixture, &["initial"]);
    fixture.connect(gateway.address);
    let before = fixture.text();
    let journal = fs::read(fixture.0.join(".config.toml.magpie-state/journal.toml")).unwrap();
    let preview = receipt(fixture.update("fixture/missing", "--dry-run", None));
    rejected(
        fixture.update("fixture/missing", "--apply", preview["revision"].as_str()),
        "gateway_probe_failed",
    );
    assert_eq!(fixture.text(), before);
    assert_eq!(
        fs::read(fixture.0.join(".config.toml.magpie-state/journal.toml")).unwrap(),
        journal
    );
}

#[test]
fn user_owned_magpie_provider_stays_unmanaged_on_status_update_and_disconnect() {
    let original = "model = 'user/model'\nmodel_provider = 'magpie'\n[model_providers.magpie]\nname = 'My Provider'\nbase_url = 'https://user.invalid/v1'\n[model_providers.magpie.http_headers]\nAuthorization = 'SYNTHETIC_PRIVATE_CANARY'\n";
    let fixture = Fixture::new(original);
    for operation in ["connection-status", "disconnect", "connection-status"] {
        let output = fixture
            .command()
            .args(["agents", operation, "codex", "--config"])
            .arg(fixture.config())
            .arg("--json")
            .output()
            .unwrap();
        if operation == "disconnect" {
            rejected(output, "config_transaction_error");
        } else {
            assert!(output.status.success());
            let status: Value = serde_json::from_slice(&output.stdout).unwrap();
            assert_eq!(status["state"], "unmanaged");
        }
        assert_eq!(fixture.text(), original);
        assert!(!fixture.0.join(".config.toml.magpie-state").exists());
    }
    rejected(
        fixture.update("fixture/next", "--dry-run", None),
        "config_transaction_error",
    );
    assert_eq!(fixture.text(), original);
}
