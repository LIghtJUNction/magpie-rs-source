#![cfg(any(target_os = "linux", target_os = "android", target_os = "macos"))]

use magpie_core::codex_transaction as public_transaction;
use magpie_core::codex_transaction::{ConnectionStatus, TransactionError};
use std::fs::{self, DirBuilder, OpenOptions};
use std::io::Write;
use std::net::{Ipv6Addr, SocketAddr, SocketAddrV6};
use std::os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt, PermissionsExt, symlink};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};
use toml_edit::{DocumentMut, Item, value};

// Compile the same implementation with test-only fault points. The existing
// integration cases continue to exercise the public library build.
#[path = "../src/codex_transaction.rs"]
pub mod crash_candidate;

use crash_candidate::test_support::{self, Action, Point};

// Keep the harness's fork/exec window outside any open transaction descriptor.
// Operations on independent fixtures retain concurrent shared read access.
fn prepare_connect(
    path: &Path,
    model: &str,
    gateway: SocketAddr,
) -> Result<public_transaction::PreparedConnect, TransactionError> {
    let _scope = test_support::descriptor_scope();
    public_transaction::prepare_connect(path, model, gateway)
}

fn commit_connect(
    plan: public_transaction::PreparedConnect,
) -> Result<ConnectionStatus, TransactionError> {
    let _scope = test_support::descriptor_scope();
    public_transaction::commit_connect(plan)
}

fn prepare_model_update(
    path: &Path,
    model: &str,
) -> Result<public_transaction::PreparedModelUpdate, TransactionError> {
    let _scope = test_support::descriptor_scope();
    public_transaction::prepare_model_update(path, model)
}

fn commit_model_update(
    plan: public_transaction::PreparedModelUpdate,
) -> Result<public_transaction::ModelUpdateOutcome, TransactionError> {
    let _scope = test_support::descriptor_scope();
    public_transaction::commit_model_update(plan)
}

fn connection_status(path: &Path) -> Result<ConnectionStatus, TransactionError> {
    let _scope = test_support::descriptor_scope();
    public_transaction::connection_status(path)
}

fn disconnect(path: &Path) -> Result<public_transaction::DisconnectOutcome, TransactionError> {
    let _scope = test_support::descriptor_scope();
    public_transaction::disconnect(path)
}

static NEXT: AtomicU64 = AtomicU64::new(0);
const STATE_BYTE_LIMIT: u64 = 128 * 1024 * 1024;
const STATE_ENTRY_LIMIT: usize = 256;

const ORIGINAL: &str = r#"# configuration header retained verbatim
model = 'user/original' # keep the model comment
model_provider = "user" # keep the provider comment
model_reasoning_effort = "high"
unknown_extension = ["keep", "its order"]

[profiles.work]
model = "profile/unchanged"

[model_providers.user]
name = "User Provider"
base_url = "https://provider.invalid/v1"
env_key = "SYNTHETIC_KEY_REFERENCE"

[model_providers.user.http_headers]
X-Team = "before"
Authorization = "SYNTHETIC_PRIVATE_SENTINEL_NEVER_PRINT"
"#;

struct Fixture {
    directory: PathBuf,
    config: PathBuf,
}

impl Fixture {
    fn new(bytes: Option<&[u8]>) -> Self {
        let clock = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let directory = std::env::temp_dir().canonicalize().unwrap().join(format!(
            "magpie-codex-transaction-{}-{clock}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        DirBuilder::new().mode(0o700).create(&directory).unwrap();
        let config = directory.join("config.toml");
        if let Some(bytes) = bytes {
            write_private(&config, bytes);
        }
        Self { directory, config }
    }

    fn state(&self) -> PathBuf {
        self.directory.join(".config.toml.magpie-state")
    }

    fn text(&self) -> String {
        fs::read_to_string(&self.config).unwrap()
    }

    fn document(&self) -> DocumentMut {
        self.text().parse().unwrap()
    }

    fn connect(&self) {
        let plan = prepare_connect(&self.config, "fixture/vendor-model-v2", endpoint()).unwrap();
        assert_eq!(commit_connect(plan).unwrap(), ConnectionStatus::Connected);
    }

    fn journal(&self) -> DocumentMut {
        fs::read_to_string(self.state().join("journal.toml"))
            .unwrap()
            .parse()
            .unwrap()
    }

    fn set_phase(&self, phase: &str) {
        let mut document = self.journal();
        document["phase"] = value(phase);
        fs::write(self.state().join("journal.toml"), document.to_string()).unwrap();
    }

    fn image(&self, suffix: &str) -> Vec<u8> {
        let journal = self.journal();
        let key = match suffix {
            "disconnect-before" | "disconnected" => "disconnect_session",
            "update-before" | "update-after" => "update_session",
            "connected" if journal.contains_key("connected_session") => "connected_session",
            _ => "session",
        };
        let session = journal[key].as_str().unwrap();
        fs::read(self.state().join(format!("{session}-{suffix}.toml"))).unwrap()
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::set_permissions(&self.directory, fs::Permissions::from_mode(0o700));
        let _ = fs::remove_dir_all(&self.directory);
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

fn endpoint() -> SocketAddr {
    "127.0.0.1:3425".parse().unwrap()
}

fn crash_at(fixture: &Fixture, operation: &str, point: &str, occurrence: usize) {
    let mut command = std::process::Command::new(std::env::current_exe().unwrap());
    command
        .args(["--exact", "transaction_crash_worker", "--ignored"])
        .env("MAGPIE_TRANSACTION_TEST_ROOT", &fixture.directory)
        .env("MAGPIE_TRANSACTION_TEST_OPERATION", operation)
        .env("MAGPIE_TRANSACTION_TEST_POINT", point)
        .env("MAGPIE_TRANSACTION_TEST_OCCURRENCE", occurrence.to_string());
    let child = {
        let _scope = test_support::spawn_scope();
        command
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .unwrap()
    };
    let output = child.wait_with_output().unwrap();
    assert_eq!(
        output.status.code(),
        Some(test_support::CRASH_EXIT),
        "worker did not reach {operation}/{point}: {}",
        String::from_utf8_lossy(&output.stderr),
    );
    assert!(
        fixture.state().join("lock").is_file(),
        "the persistent lock names remain after an abrupt exit",
    );
}

fn state_usage(fixture: &Fixture) -> (u64, usize) {
    fs::read_dir(fixture.state())
        .unwrap()
        .fold((0, 0), |(bytes, count), entry| {
            let meta = fs::symlink_metadata(entry.unwrap().path()).unwrap();
            assert!(meta.is_file(), "fixture state must contain only files");
            (bytes + meta.len(), count + 1)
        })
}

fn fill_state_bytes(fixture: &Fixture, remaining: u64) -> PathBuf {
    let padding = fixture.state().join("unknown-operator-data");
    let used = state_usage(fixture).0;
    let length = STATE_BYTE_LIMIT
        .checked_sub(remaining + used)
        .expect("fixture must have room for padding");
    let file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&padding)
        .unwrap();
    // A sparse file exercises the storage accounting without allocating 128 MiB.
    file.set_len(length).unwrap();
    padding
}

#[test]
#[ignore = "only spawned with an isolated fixture by crash-boundary tests"]
fn transaction_crash_worker() {
    let root = PathBuf::from(
        std::env::var_os("MAGPIE_TRANSACTION_TEST_ROOT").expect("isolated fixture is required"),
    );
    assert_eq!(
        root.parent(),
        Some(std::env::temp_dir().canonicalize().unwrap().as_path()),
        "worker must only use a temporary synthetic fixture",
    );
    assert!(
        root.file_name()
            .unwrap()
            .to_str()
            .unwrap()
            .starts_with("magpie-codex-transaction-"),
        "worker requires the test fixture namespace",
    );
    let config = root.join("config.toml");
    let state = root.join(".config.toml.magpie-state");
    let occurrence = std::env::var("MAGPIE_TRANSACTION_TEST_OCCURRENCE")
        .unwrap()
        .parse()
        .unwrap();
    let point = std::env::var("MAGPIE_TRANSACTION_TEST_POINT").unwrap();
    let (point, path) = match point.as_str() {
        "stage" => (Point::StageSynced, state.as_path()),
        "image" => (Point::ImageSynced, state.as_path()),
        "install" => (Point::BeforeInstall, config.as_path()),
        "parent-sync" => (Point::BeforeDirectorySync, root.as_path()),
        _ => panic!("unknown fixture crash point"),
    };
    test_support::arm(point, path, occurrence, Action::Exit);
    match std::env::var("MAGPIE_TRANSACTION_TEST_OPERATION")
        .unwrap()
        .as_str()
    {
        "connect" => {
            let plan =
                crash_candidate::prepare_connect(&config, "fixture/vendor-model-v2", endpoint())
                    .unwrap();
            crash_candidate::commit_connect(plan).unwrap();
        }
        "disconnect" => {
            crash_candidate::disconnect(&config).unwrap();
        }
        "update" => {
            let plan =
                crash_candidate::prepare_model_update(&config, "fixture/updated-model").unwrap();
            crash_candidate::commit_model_update(plan).unwrap();
        }
        _ => panic!("unknown fixture operation"),
    }
    panic!("operation completed without reaching the armed crash point");
}

#[test]
fn model_update_retains_first_original_across_multiple_models_and_disconnect() {
    for original in [Some(ORIGINAL.as_bytes()), None] {
        let fixture = Fixture::new(original);
        fixture.connect();
        let first_session = fixture.journal()["session"].as_str().unwrap().to_owned();
        let original_image = fixture.image("original");
        for model in ["fixture/second", "other/third", "fixture/vendor-model-v2"] {
            let plan = prepare_model_update(&fixture.config, model).unwrap();
            assert_eq!(plan.gateway_addr(), endpoint());
            assert_eq!(plan.changed_fields(), &["model"]);
            let revision = plan.revision().to_owned();
            let outcome = commit_model_update(plan).unwrap();
            assert!(outcome.changed);
            assert_eq!(outcome.status, ConnectionStatus::Connected);
            assert_ne!(outcome.revision, revision);
            assert_eq!(fixture.document()["model"].as_str(), Some(model));
            assert_eq!(
                fixture.journal()["session"].as_str(),
                Some(first_session.as_str())
            );
            assert_eq!(fixture.image("original"), original_image);
            assert_eq!(fixture.journal()["version"].as_integer(), Some(2));
        }
        let outcome = disconnect(&fixture.config).unwrap();
        assert_eq!(outcome.restored_fields, 7);
        assert_eq!(outcome.preserved_fields, 0);
        assert_eq!(
            connection_status(&fixture.config).unwrap(),
            ConnectionStatus::Unmanaged
        );
        assert_eq!(fs::read(&fixture.config).ok().as_deref(), original);
    }
}

#[test]
fn model_update_preserves_user_comments_profiles_headers_and_effort() {
    let fixture = Fixture::new(Some(ORIGINAL.as_bytes()));
    fixture.connect();
    let before = fixture
        .text()
        .replace("X-Team = \"before\"", "X-Team = \"after\"");
    let before =
        format!("# comment added while connected\n{before}\n# retained trailing comment\n");
    fs::write(&fixture.config, &before).unwrap();
    let plan = prepare_model_update(&fixture.config, "fixture/updated-model").unwrap();
    commit_model_update(plan).unwrap();
    assert_eq!(
        fixture.text(),
        before.replace("fixture/vendor-model-v2", "fixture/updated-model")
    );
    let updated = fixture.document();
    assert_eq!(updated["model_reasoning_effort"].as_str(), Some("high"));
    assert_eq!(
        updated["profiles"]["work"]["model"].as_str(),
        Some("profile/unchanged")
    );
    assert_eq!(
        updated["model_providers"]["user"]["http_headers"]["X-Team"].as_str(),
        Some("after")
    );
    disconnect(&fixture.config).unwrap();
    let restored = fixture.text();
    assert!(restored.contains("# comment added while connected"));
    assert!(restored.contains("# retained trailing comment"));
    assert!(restored.contains("X-Team = \"after\""));
    assert_eq!(fixture.document()["model"].as_str(), Some("user/original"));
}

#[test]
fn model_update_preview_and_same_model_apply_do_not_write_or_expose_values() {
    let fixture = Fixture::new(Some(ORIGINAL.as_bytes()));
    fixture.connect();
    let before = fixture.text();
    let journal = fs::read(fixture.state().join("journal.toml")).unwrap();
    let identity = fs::metadata(&fixture.config).unwrap().ino();
    let usage = state_usage(&fixture);
    let plan = prepare_model_update(&fixture.config, "fixture/vendor-model-v2").unwrap();
    assert!(plan.changed_fields().is_empty());
    let revision = plan.revision().to_owned();
    let debug = format!("{plan:?}");
    assert!(!debug.contains("SYNTHETIC_PRIVATE_SENTINEL_NEVER_PRINT"));
    assert!(!debug.contains("fixture/vendor-model-v2"));
    let outcome = commit_model_update(plan).unwrap();
    assert!(!outcome.changed);
    assert_eq!(outcome.revision, revision);
    assert_eq!(fixture.text(), before);
    assert_eq!(fs::metadata(&fixture.config).unwrap().ino(), identity);
    assert_eq!(
        fs::read(fixture.state().join("journal.toml")).unwrap(),
        journal
    );
    assert_eq!(state_usage(&fixture), usage);
}

#[test]
fn model_update_rejects_a_stale_plan_after_another_update_even_if_model_cycles_back() {
    let fixture = Fixture::new(Some(ORIGINAL.as_bytes()));
    fixture.connect();
    let stale = prepare_model_update(&fixture.config, "fixture/stale-choice").unwrap();
    for model in ["fixture/other-choice", "fixture/vendor-model-v2"] {
        commit_model_update(prepare_model_update(&fixture.config, model).unwrap()).unwrap();
    }
    let before = fixture.text();
    assert_eq!(
        commit_model_update(stale),
        Err(TransactionError::OriginalChanged)
    );
    assert_eq!(fixture.text(), before);
    disconnect(&fixture.config).unwrap();
    assert_eq!(fixture.text(), ORIGINAL);
}

#[test]
fn model_update_preserves_external_edits_before_preparation_and_before_commit() {
    for before_preparation in [false, true] {
        let fixture = Fixture::new(Some(ORIGINAL.as_bytes()));
        fixture.connect();
        let plan = if before_preparation {
            None
        } else {
            Some(prepare_model_update(&fixture.config, "fixture/next").unwrap())
        };
        let external = fixture
            .text()
            .replace("fixture/vendor-model-v2", "user/external");
        fs::write(&fixture.config, &external).unwrap();
        if let Some(plan) = plan {
            assert_eq!(
                commit_model_update(plan),
                Err(TransactionError::OriginalChanged)
            );
        } else {
            assert_eq!(
                prepare_model_update(&fixture.config, "fixture/next").unwrap_err(),
                TransactionError::OriginalChanged
            );
        }
        assert_eq!(fixture.text(), external);
        assert_eq!(
            connection_status(&fixture.config).unwrap(),
            ConnectionStatus::Drifted
        );
        disconnect(&fixture.config).unwrap();
        assert_eq!(fixture.document()["model"].as_str(), Some("user/external"));
    }
}

#[test]
fn model_update_does_not_adopt_a_user_owned_magpie_provider() {
    let original = b"model = 'private/model'\nmodel_provider = 'magpie'\n[model_providers.magpie]\nname = 'My private endpoint'\nbase_url = 'https://user.invalid/v1'\n[model_providers.magpie.http_headers]\nAuthorization = 'SYNTHETIC_PRIVATE_SENTINEL_NEVER_PRINT'\n";
    let fixture = Fixture::new(Some(original));
    for _ in 0..2 {
        assert_eq!(
            connection_status(&fixture.config).unwrap(),
            ConnectionStatus::Unmanaged
        );
        assert_eq!(
            prepare_model_update(&fixture.config, "fixture/next").unwrap_err(),
            TransactionError::NotManaged
        );
        assert_eq!(
            disconnect(&fixture.config),
            Err(TransactionError::NotManaged)
        );
        assert_eq!(fs::read(&fixture.config).unwrap(), original);
        assert!(!fixture.state().exists());
    }
}

#[test]
fn model_update_preinstall_failure_retains_previous_connection_and_recovery_point() {
    let fixture = Fixture::new(Some(ORIGINAL.as_bytes()));
    fixture.connect();
    let before = fixture.text();
    let journal = fs::read(fixture.state().join("journal.toml")).unwrap();
    let plan = crash_candidate::prepare_model_update(&fixture.config, "fixture/next").unwrap();
    test_support::arm(Point::BeforeInstall, &fixture.config, 1, Action::Fail);
    assert_eq!(
        crash_candidate::commit_model_update(plan),
        Err(crash_candidate::TransactionError::Write)
    );
    assert_eq!(fixture.text(), before);
    assert_eq!(
        fs::read(fixture.state().join("journal.toml")).unwrap(),
        journal
    );
    assert_eq!(
        connection_status(&fixture.config).unwrap(),
        ConnectionStatus::Connected
    );
    disconnect(&fixture.config).unwrap();
    assert_eq!(fixture.text(), ORIGINAL);
}

#[test]
fn model_update_interrupted_before_or_after_install_can_restore_the_first_original() {
    for point in ["install", "parent-sync"] {
        let fixture = Fixture::new(Some(ORIGINAL.as_bytes()));
        fixture.connect();
        // The pending update is deliberately not the first update in the chain.
        commit_model_update(prepare_model_update(&fixture.config, "fixture/second").unwrap())
            .unwrap();
        crash_at(&fixture, "update", point, 1);
        assert_eq!(fixture.journal()["phase"].as_str(), Some("update-prepared"));
        assert_eq!(
            connection_status(&fixture.config).unwrap(),
            ConnectionStatus::RecoveryRequired
        );
        assert_eq!(
            prepare_model_update(&fixture.config, "fixture/third").unwrap_err(),
            TransactionError::RecoveryRequired
        );
        disconnect(&fixture.config).unwrap();
        assert_eq!(fixture.text(), ORIGINAL);
        assert_eq!(
            connection_status(&fixture.config).unwrap(),
            ConnectionStatus::Unmanaged
        );
    }
}

#[test]
fn model_update_ambiguous_recovery_and_tampered_images_never_overwrite_configuration() {
    for tamper_image in [false, true] {
        let fixture = Fixture::new(Some(ORIGINAL.as_bytes()));
        fixture.connect();
        crash_at(&fixture, "update", "parent-sync", 1);
        if tamper_image {
            let journal = fixture.journal();
            let session = journal["update_session"].as_str().unwrap();
            let image = fixture.state().join(format!("{session}-update-after.toml"));
            fs::write(image, b"model = 'fixture/tampered'\n").unwrap();
        } else {
            let external = fixture
                .text()
                .replace("fixture/updated-model", "user/newer");
            fs::write(&fixture.config, external).unwrap();
        }
        let before = fixture.text();
        assert_eq!(
            disconnect(&fixture.config),
            Err(if tamper_image {
                TransactionError::StateInvalid
            } else {
                TransactionError::RecoveryRequired
            })
        );
        assert_eq!(fixture.text(), before);
    }
}

#[test]
fn model_update_directory_sync_failure_reports_uncertainty_and_retains_recovery() {
    let fixture = Fixture::new(Some(ORIGINAL.as_bytes()));
    fixture.connect();
    let plan = crash_candidate::prepare_model_update(&fixture.config, "fixture/next").unwrap();
    test_support::arm(
        Point::BeforeDirectorySync,
        &fixture.directory,
        1,
        Action::Fail,
    );
    assert_eq!(
        crash_candidate::commit_model_update(plan),
        Err(crash_candidate::TransactionError::CommitUncertain)
    );
    assert_eq!(fixture.document()["model"].as_str(), Some("fixture/next"));
    assert_eq!(
        connection_status(&fixture.config).unwrap(),
        ConnectionStatus::RecoveryRequired
    );
    disconnect(&fixture.config).unwrap();
    assert_eq!(fixture.text(), ORIGINAL);
}

#[test]
fn model_update_pending_or_final_journal_sync_failure_retains_sufficient_recovery_images() {
    // Lock durability and recovery-image durability precede both publications.
    for occurrence in [3, 4] {
        let fixture = Fixture::new(Some(ORIGINAL.as_bytes()));
        fixture.connect();
        let plan = crash_candidate::prepare_model_update(&fixture.config, "fixture/next").unwrap();
        test_support::arm(
            Point::BeforeDirectorySync,
            &fixture.state(),
            occurrence,
            Action::Fail,
        );
        let expected = if occurrence == 3 {
            crash_candidate::TransactionError::RecoveryRequired
        } else {
            crash_candidate::TransactionError::CommitUncertain
        };
        assert_eq!(crash_candidate::commit_model_update(plan), Err(expected));
        assert_eq!(
            fixture.document()["model"].as_str(),
            Some(if occurrence == 3 {
                "fixture/vendor-model-v2"
            } else {
                "fixture/next"
            })
        );
        assert_eq!(
            connection_status(&fixture.config).unwrap(),
            if occurrence == 3 {
                ConnectionStatus::RecoveryRequired
            } else {
                ConnectionStatus::Connected
            }
        );
        disconnect(&fixture.config).unwrap();
        assert_eq!(fixture.text(), ORIGINAL);
    }
}

#[test]
fn model_update_budget_rejection_is_read_only_and_completed_updates_do_not_accumulate_images() {
    let fixture = Fixture::new(Some(ORIGINAL.as_bytes()));
    fixture.connect();
    for index in 0..20 {
        let model = format!("fixture/model-{index}");
        commit_model_update(prepare_model_update(&fixture.config, &model).unwrap()).unwrap();
        assert!(
            state_usage(&fixture).1 <= 6,
            "completed updates must retire their transient images"
        );
    }
    let padding = fill_state_bytes(&fixture, 1024);
    let before = fixture.text();
    let plan = prepare_model_update(&fixture.config, "fixture/over-budget").unwrap();
    assert_eq!(
        commit_model_update(plan),
        Err(TransactionError::StorageLimit)
    );
    assert_eq!(fixture.text(), before);
    fs::remove_file(padding).unwrap();
    disconnect(&fixture.config).unwrap();
    assert_eq!(fixture.text(), ORIGINAL);
}

#[test]
fn prepare_is_read_only_and_debug_never_exposes_values() {
    let fixture = Fixture::new(Some(ORIGINAL.as_bytes()));
    let model = "fixture/SYNTHETIC_MODEL_SENTINEL";
    let plan = prepare_connect(&fixture.config, model, endpoint()).unwrap();
    assert!(plan.config_existed());
    assert_eq!(plan.changed_fields().len(), 7);
    let debug = format!("{plan:?}");
    for private in [
        "SYNTHETIC_PRIVATE_SENTINEL_NEVER_PRINT",
        "user/original",
        model,
    ] {
        assert!(!debug.contains(private));
    }
    assert_eq!(fixture.text(), ORIGINAL);
    assert!(!fixture.state().exists());
    assert_eq!(fs::read_dir(&fixture.directory).unwrap().count(), 1);
}

#[test]
fn connect_disconnect_restores_exact_original_bytes_and_uses_private_files() {
    let fixture = Fixture::new(Some(ORIGINAL.as_bytes()));
    fs::set_permissions(&fixture.config, fs::Permissions::from_mode(0o644)).unwrap();
    fixture.connect();
    let document = fixture.document();
    assert_eq!(document["model"].as_str(), Some("fixture/vendor-model-v2"));
    assert_eq!(document["model_provider"].as_str(), Some("magpie"));
    let provider = document["model_providers"]["magpie"].as_table().unwrap();
    assert_eq!(provider.len(), 5);
    assert_eq!(
        provider["base_url"].as_str(),
        Some("http://127.0.0.1:3425/v1")
    );
    assert_eq!(provider["wire_api"].as_str(), Some("responses"));
    assert_eq!(provider["requires_openai_auth"].as_bool(), Some(false));
    assert_eq!(provider["supports_websockets"].as_bool(), Some(false));
    assert!(!provider.contains_key("env_key"));
    assert!(!document.contains_key("features"));
    assert!(fixture.text().contains("# keep the model comment"));
    assert!(
        fixture
            .text()
            .contains("# configuration header retained verbatim")
    );
    assert!(fixture.text().contains("# keep the provider comment"));
    assert_eq!(fixture.image("original"), ORIGINAL.as_bytes());
    assert_eq!(
        fs::metadata(fixture.state()).unwrap().permissions().mode() & 0o777,
        0o700
    );
    for entry in fs::read_dir(fixture.state()).unwrap() {
        assert_eq!(
            entry.unwrap().metadata().unwrap().permissions().mode() & 0o777,
            0o600
        );
    }
    assert_eq!(
        fs::metadata(&fixture.config).unwrap().permissions().mode() & 0o777,
        0o600
    );
    assert_eq!(
        connection_status(&fixture.config).unwrap(),
        ConnectionStatus::Connected
    );
    let result = disconnect(&fixture.config).unwrap();
    assert_eq!(result.restored_fields, 7);
    assert_eq!(result.preserved_fields, 0);
    assert!(!result.config_removed);
    assert_eq!(fixture.text(), ORIGINAL);
    assert_eq!(
        connection_status(&fixture.config).unwrap(),
        ConnectionStatus::Unmanaged
    );
    let legacy_lock = fs::metadata(fixture.state().join("lock")).unwrap();
    let native_lock = fs::metadata(fixture.state().join("lock-v2")).unwrap();
    assert_eq!(
        (legacy_lock.dev(), legacy_lock.ino()),
        (native_lock.dev(), native_lock.ino())
    );
    assert_eq!(native_lock.nlink(), 2);
    assert!(fs::read_dir(&fixture.directory).unwrap().all(|entry| {
        !entry
            .unwrap()
            .file_name()
            .to_string_lossy()
            .starts_with(".magpie-stage-")
    }));
}

#[test]
fn connect_retains_quoted_key_spelling_spacing_and_leading_comments() {
    let original = "# leading model note\n\"model\"  =  'original/model' # inline model note\n# leading provider note\n'model_provider' = 'original'\n";
    let fixture = Fixture::new(Some(original.as_bytes()));
    fixture.connect();
    let connected = fixture.text();
    assert!(connected.contains("# leading model note\n\"model\"  =  "));
    assert!(connected.contains("# leading provider note\n'model_provider' = "));
    assert!(connected.contains("# inline model note"));
    disconnect(&fixture.config).unwrap();
    assert_eq!(fixture.text(), original);
}

#[test]
fn disconnect_preserves_unrelated_header_profile_unknown_fields_and_comments() {
    let fixture = Fixture::new(Some(ORIGINAL.as_bytes()));
    fixture.connect();
    let edited = fixture.text().replace(
        "X-Team = \"before\"",
        "# external header note\nX-Team = \"external-after\"",
    );
    fs::write(&fixture.config, &edited).unwrap();
    assert_eq!(
        connection_status(&fixture.config).unwrap(),
        ConnectionStatus::Connected
    );
    let result = disconnect(&fixture.config).unwrap();
    assert_eq!(result.restored_fields, 7);
    let document = fixture.document();
    assert_eq!(document["model"].as_str(), Some("user/original"));
    assert_eq!(document["model_provider"].as_str(), Some("user"));
    assert_eq!(
        document["model_providers"]["user"]["http_headers"]["X-Team"].as_str(),
        Some("external-after")
    );
    assert_eq!(
        document["profiles"]["work"]["model"].as_str(),
        Some("profile/unchanged")
    );
    assert!(document.contains_key("unknown_extension"));
    assert!(fixture.text().contains("# external header note"));
    assert!(fixture.text().contains("# keep the model comment"));
    assert!(
        !document["model_providers"]
            .as_table()
            .unwrap()
            .contains_key("magpie")
    );
}

#[test]
fn disconnect_preserves_external_changes_to_owned_values() {
    let fixture = Fixture::new(Some(ORIGINAL.as_bytes()));
    fixture.connect();
    let mut document = fixture.document();
    document["model"] = value("external/their-model");
    document["model_providers"]["magpie"]["base_url"] = value("http://127.0.0.1:9999/v1");
    fs::write(&fixture.config, document.to_string()).unwrap();
    assert_eq!(
        connection_status(&fixture.config).unwrap(),
        ConnectionStatus::Drifted
    );
    let result = disconnect(&fixture.config).unwrap();
    assert_eq!(result.restored_fields, 5);
    assert_eq!(result.preserved_fields, 2);
    let document = fixture.document();
    assert_eq!(document["model"].as_str(), Some("external/their-model"));
    assert_eq!(document["model_provider"].as_str(), Some("user"));
    let provider = document["model_providers"]["magpie"].as_table().unwrap();
    assert_eq!(provider.len(), 1);
    assert_eq!(
        provider["base_url"].as_str(),
        Some("http://127.0.0.1:9999/v1")
    );
}

#[test]
fn disconnect_keeps_added_provider_fields_and_owned_field_comments() {
    let fixture = Fixture::new(Some(b"# original header\n"));
    fixture.connect();
    let edited = fixture
        .text()
        .replace(
            "model_provider = \"magpie\"",
            "model_provider = \"magpie\" # external selection note",
        )
        .replace(
            "[model_providers.magpie]",
            "# external provider note\n[model_providers.magpie]",
        )
        .replace(
            "wire_api = \"responses\"",
            "wire_api = \"responses\" # external wire note",
        );
    let edited = format!("{edited}\n[model_providers.magpie.http_headers]\nX-New = 'external'\n");
    fs::write(&fixture.config, edited).unwrap();
    disconnect(&fixture.config).unwrap();
    let document = fixture.document();
    assert!(!document.contains_key("model"));
    assert!(!document.contains_key("model_provider"));
    assert_eq!(
        document["model_providers"]["magpie"]["http_headers"]["X-New"].as_str(),
        Some("external")
    );
    for comment in [
        "# original header",
        "# external provider note",
        "# external selection note",
        "# external wire note",
    ] {
        assert!(fixture.text().contains(comment), "lost a fixture comment");
    }
}

#[test]
fn absent_original_is_removed_only_when_no_external_edit_exists() {
    let fixture = Fixture::new(None);
    fixture.connect();
    let result = disconnect(&fixture.config).unwrap();
    assert!(result.config_removed);
    assert!(!fixture.config.exists());

    let edited_fixture = Fixture::new(None);
    edited_fixture.connect();
    fs::write(
        &edited_fixture.config,
        format!("# external note\nunknown = 42\n{}", edited_fixture.text()),
    )
    .unwrap();
    let result = disconnect(&edited_fixture.config).unwrap();
    assert!(!result.config_removed);
    assert_eq!(edited_fixture.document()["unknown"].as_integer(), Some(42));
    assert!(edited_fixture.text().contains("# external note"));
    assert!(!edited_fixture.document().contains_key("model_providers"));
}

#[test]
fn externally_deleted_file_is_preserved_as_deleted() {
    let fixture = Fixture::new(Some(ORIGINAL.as_bytes()));
    fixture.connect();
    fs::remove_file(&fixture.config).unwrap();
    assert_eq!(
        connection_status(&fixture.config).unwrap(),
        ConnectionStatus::Drifted
    );
    let result = disconnect(&fixture.config).unwrap();
    assert_eq!(result.restored_fields, 0);
    assert_eq!(result.preserved_fields, 7);
    assert!(!result.config_removed);
    assert!(!fixture.config.exists());
}

#[test]
fn changed_since_prepare_is_rejected_without_overwriting_external_bytes() {
    let fixture = Fixture::new(Some(ORIGINAL.as_bytes()));
    let plan = prepare_connect(&fixture.config, "fixture/a", endpoint()).unwrap();
    let external = format!("# external edit\n{ORIGINAL}");
    fs::write(&fixture.config, &external).unwrap();
    assert_eq!(
        commit_connect(plan).unwrap_err(),
        TransactionError::OriginalChanged
    );
    assert_eq!(fixture.text(), external);
    assert_eq!(
        connection_status(&fixture.config).unwrap(),
        ConnectionStatus::Unmanaged
    );
}

#[test]
fn same_bytes_replaced_inode_is_still_a_conflict() {
    let fixture = Fixture::new(Some(ORIGINAL.as_bytes()));
    let plan = prepare_connect(&fixture.config, "fixture/a", endpoint()).unwrap();
    let replacement = fixture.directory.join("replacement");
    write_private(&replacement, ORIGINAL.as_bytes());
    fs::rename(replacement, &fixture.config).unwrap();
    assert_eq!(
        commit_connect(plan).unwrap_err(),
        TransactionError::OriginalChanged
    );
    assert_eq!(fixture.text(), ORIGINAL);
}

#[test]
fn two_prepared_plans_cannot_overwrite_one_another() {
    let fixture = Fixture::new(Some(ORIGINAL.as_bytes()));
    let first = prepare_connect(&fixture.config, "fixture/a", endpoint()).unwrap();
    let second = prepare_connect(&fixture.config, "fixture/b", endpoint()).unwrap();
    commit_connect(first).unwrap();
    assert_eq!(
        commit_connect(second).unwrap_err(),
        TransactionError::AlreadyManaged
    );
    assert_eq!(fixture.document()["model"].as_str(), Some("fixture/a"));
}

#[test]
fn cooperative_or_stale_lock_is_never_broken_automatically() {
    let fixture = Fixture::new(Some(ORIGINAL.as_bytes()));
    DirBuilder::new()
        .mode(0o700)
        .create(fixture.state())
        .unwrap();
    write_private(&fixture.state().join("lock"), b"synthetic stale lock");
    assert_eq!(
        prepare_connect(&fixture.config, "fixture/a", endpoint()).unwrap_err(),
        TransactionError::LockBusy
    );
    assert_eq!(
        connection_status(&fixture.config).unwrap_err(),
        TransactionError::LockBusy
    );
    assert_eq!(fixture.text(), ORIGINAL);
    assert!(fixture.state().join("lock").exists());
}

#[test]
fn malformed_utf8_duplicate_keys_and_invalid_toml_never_mutate() {
    for (bytes, expected) in [
        (
            &b"model = 'a'\nmodel = 'b'\n"[..],
            TransactionError::InvalidToml,
        ),
        (
            &b"model = [\"SYNTHETIC_PRIVATE_SENTINEL\""[..],
            TransactionError::InvalidToml,
        ),
        (&b"\xff\xfe"[..], TransactionError::InvalidUtf8),
    ] {
        let fixture = Fixture::new(Some(bytes));
        let error = prepare_connect(&fixture.config, "fixture/a", endpoint()).unwrap_err();
        assert_eq!(error, expected);
        assert!(!format!("{error:?} {error}").contains("SYNTHETIC_PRIVATE_SENTINEL"));
        assert_eq!(fs::read(&fixture.config).unwrap(), bytes);
        assert!(!fixture.state().exists());
    }
}

#[test]
fn oversized_configuration_is_refused_before_reading_it() {
    let fixture = Fixture::new(Some(b""));
    OpenOptions::new()
        .write(true)
        .open(&fixture.config)
        .unwrap()
        .set_len(4 * 1024 * 1024 + 1)
        .unwrap();
    assert_eq!(
        prepare_connect(&fixture.config, "fixture/a", endpoint()).unwrap_err(),
        TransactionError::TooLarge
    );
    assert_eq!(
        fs::metadata(&fixture.config).unwrap().len(),
        4 * 1024 * 1024 + 1
    );
    assert!(!fixture.state().exists());
}

#[test]
fn unknown_provider_or_unsupported_toml_shape_requires_reconciliation() {
    for (bytes, expected) in [
        (
            &b"[model_providers.magpie]\nname='user-owned'\n"[..],
            TransactionError::UnknownProvider,
        ),
        (
            &b"model_providers = { user = { name = 'inline' } }\n"[..],
            TransactionError::UnsupportedShape,
        ),
        (&b"model = 12\n"[..], TransactionError::UnsupportedShape),
    ] {
        let fixture = Fixture::new(Some(bytes));
        assert_eq!(
            prepare_connect(&fixture.config, "fixture/a", endpoint()).unwrap_err(),
            expected
        );
        assert_eq!(fs::read(&fixture.config).unwrap(), bytes);
    }
}

#[test]
fn only_literal_loopback_endpoints_and_bounded_model_ids_are_accepted() {
    let fixture = Fixture::new(None);
    for endpoint in ["0.0.0.0:3425", "192.0.2.1:3425", "127.0.0.1:0", "[::]:3425"] {
        assert_eq!(
            prepare_connect(&fixture.config, "fixture/a", endpoint.parse().unwrap()).unwrap_err(),
            TransactionError::InvalidGateway
        );
    }
    let scoped = SocketAddr::V6(SocketAddrV6::new(Ipv6Addr::LOCALHOST, 3425, 0, 1));
    assert_eq!(
        prepare_connect(&fixture.config, "fixture/a", scoped).unwrap_err(),
        TransactionError::InvalidGateway
    );
    for model in [
        "no-provider",
        "/empty",
        "empty/",
        "p/model\nkey",
        "p/a b",
        "p/model?secret=1",
        "p/model#fragment",
    ] {
        assert_eq!(
            prepare_connect(&fixture.config, model, endpoint()).unwrap_err(),
            TransactionError::InvalidModel
        );
    }
    assert_eq!(
        prepare_connect(
            &fixture.config,
            &format!("p/{}", "a".repeat(512)),
            endpoint()
        )
        .unwrap_err(),
        TransactionError::InvalidModel
    );
    for model in [
        "vendor/model-v1",
        "vendor/org/model:revision",
        "v.end-or_1/model@v2",
    ] {
        assert!(prepare_connect(&fixture.config, model, endpoint()).is_ok());
    }
    let plan =
        prepare_connect(&fixture.config, "fixture/a", "[::1]:3425".parse().unwrap()).unwrap();
    commit_connect(plan).unwrap();
    assert_eq!(
        fixture.document()["model_providers"]["magpie"]["base_url"].as_str(),
        Some("http://[::1]:3425/v1")
    );
}

#[test]
fn symlink_target_parent_state_and_nonregular_or_hardlinked_files_are_refused() {
    let fixture = Fixture::new(None);
    let real = fixture.directory.join("real.toml");
    write_private(&real, ORIGINAL.as_bytes());
    symlink(&real, &fixture.config).unwrap();
    assert_eq!(
        prepare_connect(&fixture.config, "fixture/a", endpoint()).unwrap_err(),
        TransactionError::Symlink
    );
    fs::remove_file(&fixture.config).unwrap();
    fs::hard_link(&real, &fixture.config).unwrap();
    assert_eq!(
        prepare_connect(&fixture.config, "fixture/a", endpoint()).unwrap_err(),
        TransactionError::NotAFile
    );
    fs::remove_file(&fixture.config).unwrap();
    fs::create_dir(&fixture.config).unwrap();
    assert_eq!(
        prepare_connect(&fixture.config, "fixture/a", endpoint()).unwrap_err(),
        TransactionError::NotAFile
    );
    fs::remove_dir(&fixture.config).unwrap();
    let link = fixture.directory.join("linked-parent");
    symlink(&fixture.directory, &link).unwrap();
    assert_eq!(
        prepare_connect(&link.join("config.toml"), "fixture/a", endpoint()).unwrap_err(),
        TransactionError::Symlink
    );
    symlink(&fixture.directory, fixture.state()).unwrap();
    assert_eq!(
        prepare_connect(&fixture.config, "fixture/a", endpoint()).unwrap_err(),
        TransactionError::Symlink
    );
}

#[test]
fn read_only_or_shared_write_locations_are_refused_even_when_test_process_is_root() {
    let fixture = Fixture::new(Some(ORIGINAL.as_bytes()));
    fs::set_permissions(&fixture.config, fs::Permissions::from_mode(0o400)).unwrap();
    assert_eq!(
        prepare_connect(&fixture.config, "fixture/a", endpoint()).unwrap_err(),
        TransactionError::ReadOnly
    );
    fs::set_permissions(&fixture.config, fs::Permissions::from_mode(0o600)).unwrap();
    fs::set_permissions(&fixture.directory, fs::Permissions::from_mode(0o500)).unwrap();
    assert_eq!(
        prepare_connect(&fixture.config, "fixture/a", endpoint()).unwrap_err(),
        TransactionError::ReadOnly
    );
    fs::set_permissions(&fixture.directory, fs::Permissions::from_mode(0o770)).unwrap();
    assert_eq!(
        prepare_connect(&fixture.config, "fixture/a", endpoint()).unwrap_err(),
        TransactionError::UnsafePath
    );
    fs::set_permissions(&fixture.directory, fs::Permissions::from_mode(0o700)).unwrap();
    assert_eq!(fixture.text(), ORIGINAL);
}

#[test]
fn externally_readable_state_or_journal_is_refused() {
    let fixture = Fixture::new(Some(ORIGINAL.as_bytes()));
    DirBuilder::new()
        .mode(0o755)
        .create(fixture.state())
        .unwrap();
    assert_eq!(
        prepare_connect(&fixture.config, "fixture/a", endpoint()).unwrap_err(),
        TransactionError::StateInvalid
    );
    fs::set_permissions(fixture.state(), fs::Permissions::from_mode(0o700)).unwrap();
    fixture.connect();
    fs::set_permissions(
        fixture.state().join("journal.toml"),
        fs::Permissions::from_mode(0o644),
    )
    .unwrap();
    assert_eq!(
        connection_status(&fixture.config).unwrap_err(),
        TransactionError::StateInvalid
    );
    let connected = fixture.text();
    assert_eq!(
        disconnect(&fixture.config).unwrap_err(),
        TransactionError::StateInvalid
    );
    assert_eq!(fixture.text(), connected);
}

#[test]
fn invalid_current_toml_is_never_replaced_by_a_recovery_snapshot() {
    let fixture = Fixture::new(Some(ORIGINAL.as_bytes()));
    fixture.connect();
    let broken = b"model = [\"SYNTHETIC_PRIVATE_SENTINEL\"";
    fs::write(&fixture.config, broken).unwrap();
    assert_eq!(
        connection_status(&fixture.config).unwrap_err(),
        TransactionError::InvalidToml
    );
    assert_eq!(
        disconnect(&fixture.config).unwrap_err(),
        TransactionError::InvalidToml
    );
    assert_eq!(fs::read(&fixture.config).unwrap(), broken);
    assert_eq!(fixture.journal()["phase"].as_str(), Some("connected"));
}

#[test]
fn pending_connect_before_replacement_can_be_aborted_without_touching_target() {
    let fixture = Fixture::new(Some(ORIGINAL.as_bytes()));
    fixture.connect();
    fixture.set_phase("prepared");
    fs::write(&fixture.config, ORIGINAL).unwrap();
    assert_eq!(
        connection_status(&fixture.config).unwrap(),
        ConnectionStatus::RecoveryRequired
    );
    let result = disconnect(&fixture.config).unwrap();
    assert_eq!(result.restored_fields, 0);
    assert_eq!(fixture.text(), ORIGINAL);
    assert_eq!(
        connection_status(&fixture.config).unwrap(),
        ConnectionStatus::Unmanaged
    );
}

#[test]
fn pending_connect_after_replacement_recovers_using_persisted_ownership() {
    let fixture = Fixture::new(Some(ORIGINAL.as_bytes()));
    fixture.connect();
    fixture.set_phase("prepared");
    assert_eq!(
        connection_status(&fixture.config).unwrap(),
        ConnectionStatus::RecoveryRequired
    );
    assert_eq!(disconnect(&fixture.config).unwrap().restored_fields, 7);
    assert_eq!(fixture.text(), ORIGINAL);
}

#[test]
fn ambiguous_pending_connect_refuses_to_guess() {
    let fixture = Fixture::new(Some(ORIGINAL.as_bytes()));
    fixture.connect();
    fixture.set_phase("prepared");
    let changed = format!("# external concurrent change\n{}", fixture.text());
    fs::write(&fixture.config, &changed).unwrap();
    assert_eq!(
        disconnect(&fixture.config).unwrap_err(),
        TransactionError::RecoveryRequired
    );
    assert_eq!(fixture.text(), changed);
    assert_eq!(fixture.journal()["phase"].as_str(), Some("prepared"));
}

#[test]
fn pending_disconnect_before_and_after_replacement_are_recoverable() {
    let fixture = Fixture::new(Some(ORIGINAL.as_bytes()));
    fixture.connect();
    disconnect(&fixture.config).unwrap();
    let before = fixture.image("disconnect-before");
    fixture.set_phase("disconnect-prepared");
    fs::write(&fixture.config, before).unwrap();
    assert_eq!(disconnect(&fixture.config).unwrap().restored_fields, 7);
    assert_eq!(fixture.text(), ORIGINAL);
    fixture.set_phase("disconnect-prepared");
    assert_eq!(disconnect(&fixture.config).unwrap().restored_fields, 7);
    assert_eq!(fixture.journal()["phase"].as_str(), Some("disconnected"));
}

#[test]
fn unpublished_disconnect_images_do_not_block_a_fresh_attempt() {
    let fixture = Fixture::new(Some(ORIGINAL.as_bytes()));
    fixture.connect();
    let connected_journal = fixture.journal().to_string();
    disconnect(&fixture.config).unwrap();
    let disconnected = fixture.journal();
    let orphan_session = disconnected["disconnect_session"].as_str().unwrap();
    let before = fixture.image("disconnect-before");
    // Recreate the durable state before the disconnect journal was published.
    // The orphan is deliberately truncated, as an interrupted image write can be.
    fs::write(
        fixture
            .state()
            .join(format!("{orphan_session}-disconnected.toml")),
        b"partial",
    )
    .unwrap();
    fs::write(fixture.state().join("journal.toml"), connected_journal).unwrap();
    fs::write(&fixture.config, before).unwrap();
    assert_eq!(disconnect(&fixture.config).unwrap().restored_fields, 7);
    assert_eq!(fixture.text(), ORIGINAL);
}

#[test]
fn ambiguous_pending_disconnect_preserves_external_edits() {
    let fixture = Fixture::new(Some(ORIGINAL.as_bytes()));
    fixture.connect();
    disconnect(&fixture.config).unwrap();
    fixture.set_phase("disconnect-prepared");
    let external = format!("# external edit after interruption\n{ORIGINAL}");
    fs::write(&fixture.config, &external).unwrap();
    assert_eq!(
        disconnect(&fixture.config).unwrap_err(),
        TransactionError::RecoveryRequired
    );
    assert_eq!(fixture.text(), external);
}

#[test]
fn corrupted_pending_restore_image_is_rejected_before_target_replacement() {
    let fixture = Fixture::new(Some(ORIGINAL.as_bytes()));
    fixture.connect();
    disconnect(&fixture.config).unwrap();
    let before = fixture.image("disconnect-before");
    let journal = fixture.journal();
    let session = journal["disconnect_session"].as_str().unwrap();
    fs::write(
        fixture.state().join(format!("{session}-disconnected.toml")),
        b"model = [broken",
    )
    .unwrap();
    fs::write(&fixture.config, &before).unwrap();
    fixture.set_phase("disconnect-prepared");
    assert_eq!(
        disconnect(&fixture.config).unwrap_err(),
        TransactionError::StateInvalid
    );
    assert_eq!(fs::read(&fixture.config).unwrap(), before);
}

#[test]
fn state_binding_and_snapshot_validation_prevent_restore_from_mismatched_records() {
    let fixture = Fixture::new(Some(ORIGINAL.as_bytes()));
    fixture.connect();
    let connected = fixture.text();
    let mut journal = fixture.journal();
    journal["target"] = value("unrelated-target");
    fs::write(fixture.state().join("journal.toml"), journal.to_string()).unwrap();
    assert_eq!(
        disconnect(&fixture.config).unwrap_err(),
        TransactionError::StateInvalid
    );
    assert_eq!(fixture.text(), connected);

    let other = Fixture::new(Some(ORIGINAL.as_bytes()));
    other.connect();
    let journal = other.journal();
    let session = journal["session"].as_str().unwrap();
    let connected_path = other.state().join(format!("{session}-connected.toml"));
    fs::write(connected_path, "model='unrelated/model'\n").unwrap();
    assert_eq!(
        disconnect(&other.config).unwrap_err(),
        TransactionError::StateInvalid
    );
}

#[test]
fn completed_disconnect_allows_a_fresh_connection_and_retains_only_latest_cycle() {
    let fixture = Fixture::new(Some(ORIGINAL.as_bytes()));
    fixture.connect();
    let first_session = fixture.journal()["session"].as_str().unwrap().to_owned();
    disconnect(&fixture.config).unwrap();
    let plan = prepare_connect(&fixture.config, "another/arbitrary-model", endpoint()).unwrap();
    commit_connect(plan).unwrap();
    assert_eq!(
        fixture.document()["model"].as_str(),
        Some("another/arbitrary-model")
    );
    assert!(fs::read_dir(fixture.state()).unwrap().all(|entry| {
        !entry
            .unwrap()
            .file_name()
            .to_string_lossy()
            .starts_with(&first_session)
    }));
    disconnect(&fixture.config).unwrap();
    assert_eq!(fixture.text(), ORIGINAL);
}

#[test]
fn external_deletion_and_container_shape_changes_are_preserved() {
    let fixture = Fixture::new(Some(ORIGINAL.as_bytes()));
    fixture.connect();
    let mut document = fixture.document();
    document.remove("model");
    document["model_providers"]["magpie"] = Item::Value(ValueForTest::inline());
    fs::write(&fixture.config, document.to_string()).unwrap();
    let result = disconnect(&fixture.config).unwrap();
    assert_eq!(result.restored_fields, 1);
    assert_eq!(result.preserved_fields, 6);
    let document = fixture.document();
    assert!(!document.contains_key("model"));
    assert!(document["model_providers"]["magpie"].is_inline_table());
}

struct ValueForTest;

impl ValueForTest {
    fn inline() -> toml_edit::Value {
        let mut table = toml_edit::InlineTable::new();
        table.insert("external", toml_edit::Value::from(true));
        toml_edit::Value::InlineTable(table)
    }
}

#[test]
fn recovery_syncs_parent_before_finalizing_replaced_or_removed_target() {
    for original in [Some(ORIGINAL.as_bytes()), None] {
        let fixture = Fixture::new(original);
        fixture.connect();
        crash_at(&fixture, "disconnect", "parent-sync", 1);
        assert_eq!(
            fs::read(&fixture.config).ok().as_deref(),
            original,
            "child must exit after the target rename or unlink",
        );
        assert_eq!(
            fixture.journal()["phase"].as_str(),
            Some("disconnect-prepared"),
        );
        let pending_journal = fs::read(fixture.state().join("journal.toml")).unwrap();
        test_support::arm(
            Point::BeforeDirectorySync,
            &fixture.directory,
            1,
            Action::Fail,
        );
        assert_eq!(
            crash_candidate::disconnect(&fixture.config).unwrap_err(),
            crash_candidate::TransactionError::CommitUncertain,
            "recovery must retry the unconfirmed parent-directory sync",
        );
        assert_eq!(
            fs::read(fixture.state().join("journal.toml")).unwrap(),
            pending_journal,
            "failed sync must not publish a finished journal",
        );
        crash_candidate::disconnect(&fixture.config).unwrap();
        assert_eq!(fixture.journal()["phase"].as_str(), Some("disconnected"));
        assert_eq!(fs::read(&fixture.config).ok().as_deref(), original);
    }
}

#[test]
fn abrupt_connect_fragments_stay_private_counted_and_unknown_files_survive() {
    let fixture = Fixture::new(Some(ORIGINAL.as_bytes()));
    for point in ["stage", "image", "stage"] {
        crash_at(&fixture, "connect", point, 1);
        assert_eq!(
            connection_status(&fixture.config).unwrap(),
            ConnectionStatus::Unmanaged,
        );
        assert_eq!(fixture.text(), ORIGINAL);
        assert!(!fixture.state().join("journal.toml").exists());
    }
    let fragments: Vec<_> = fs::read_dir(fixture.state())
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .filter(|path| {
            !matches!(
                path.file_name().and_then(|name| name.to_str()),
                Some("lock" | "lock-v2")
            )
        })
        .collect();
    assert_eq!(fragments.len(), 4, "three stages and one complete image");
    for path in &fragments {
        assert_eq!(
            fs::metadata(path).unwrap().permissions().mode() & 0o777,
            0o600,
        );
    }
    assert!(fs::read_dir(&fixture.directory).unwrap().all(|entry| {
        !entry
            .unwrap()
            .file_name()
            .to_string_lossy()
            .starts_with(".magpie-stage-")
    }));
    // Even a name matching the stage convention is not proof of ownership.
    let unknown = fixture
        .state()
        .join(format!(".magpie-stage-{:032x}-{:08x}-{:016x}", 1, 2, 3));
    write_private(&unknown, b"operator-owned synthetic data");
    let padding = fill_state_bytes(&fixture, 0);
    let before = state_usage(&fixture);
    let plan = prepare_connect(&fixture.config, "fixture/a", endpoint()).unwrap();
    assert_eq!(
        commit_connect(plan).unwrap_err(),
        TransactionError::StorageLimit
    );
    assert_eq!(state_usage(&fixture), before);
    assert_eq!(
        fs::read(&unknown).unwrap(),
        b"operator-owned synthetic data",
    );
    assert!(fragments.iter().all(|path| path.is_file()));
    assert_eq!(fixture.text(), ORIGINAL);
    fs::remove_file(padding).unwrap();
    fixture.connect();
    disconnect(&fixture.config).unwrap();
    assert_eq!(fixture.text(), ORIGINAL);
    assert!(fragments.iter().all(|path| path.is_file()));
    assert!(unknown.is_file(), "unknown stage-shaped files are retained");
}

#[test]
fn pending_disconnect_reserves_replacement_and_final_journal_before_writing() {
    let fixture = Fixture::new(Some(ORIGINAL.as_bytes()));
    fixture.connect();
    let connected = fixture.text();
    crash_at(&fixture, "disconnect", "stage", 2);
    assert_eq!(fixture.text(), connected, "replacement has not happened");
    assert_eq!(
        fixture.journal()["phase"].as_str(),
        Some("disconnect-prepared"),
    );
    let padding = fill_state_bytes(&fixture, ORIGINAL.len() as u64);
    let before = state_usage(&fixture);
    assert_eq!(
        disconnect(&fixture.config).unwrap_err(),
        TransactionError::StorageLimit,
        "space for a config stage alone is insufficient without a journal reservation",
    );
    assert_eq!(
        state_usage(&fixture),
        before,
        "no extra fragment may be created"
    );
    assert_eq!(fixture.text(), connected);
    assert_eq!(
        fixture.journal()["phase"].as_str(),
        Some("disconnect-prepared"),
    );
    fs::remove_file(padding).unwrap();
    disconnect(&fixture.config).unwrap();
    assert_eq!(fixture.text(), ORIGINAL);
}

#[test]
fn pending_disconnect_after_replacement_checks_journal_space_and_entry_limit() {
    for byte_limit in [true, false] {
        let fixture = Fixture::new(Some(ORIGINAL.as_bytes()));
        fixture.connect();
        crash_at(&fixture, "disconnect", "parent-sync", 1);
        let mut unknown = Vec::new();
        if byte_limit {
            unknown.push(fill_state_bytes(&fixture, 0));
        } else {
            let existing = state_usage(&fixture).1;
            // Both lock entries are permanent; publishing a journal needs
            // one extra slot even though it replaces the old journal.
            for index in existing..STATE_ENTRY_LIMIT {
                let path = fixture.state().join(format!("unknown-{index}"));
                write_private(&path, b"retain");
                unknown.push(path);
            }
        }
        let before = state_usage(&fixture);
        assert_eq!(
            disconnect(&fixture.config).unwrap_err(),
            TransactionError::StorageLimit,
        );
        assert_eq!(state_usage(&fixture), before);
        assert_eq!(fixture.text(), ORIGINAL);
        assert_eq!(
            fixture.journal()["phase"].as_str(),
            Some("disconnect-prepared"),
        );
        for path in unknown {
            assert!(path.is_file(), "unknown data must survive budget refusal");
            fs::remove_file(path).unwrap();
        }
        disconnect(&fixture.config).unwrap();
        assert_eq!(fixture.journal()["phase"].as_str(), Some("disconnected"));
    }
}

#[test]
fn interrupted_connect_abort_journal_is_also_budgeted() {
    let fixture = Fixture::new(Some(ORIGINAL.as_bytes()));
    crash_at(&fixture, "connect", "install", 1);
    assert_eq!(fixture.text(), ORIGINAL);
    assert_eq!(fixture.journal()["phase"].as_str(), Some("prepared"));
    // Interrupt recovery itself after it writes its abort-journal stage.
    crash_at(&fixture, "disconnect", "stage", 1);
    let padding = fill_state_bytes(&fixture, 0);
    let before = state_usage(&fixture);
    assert_eq!(
        disconnect(&fixture.config).unwrap_err(),
        TransactionError::StorageLimit,
    );
    assert_eq!(state_usage(&fixture), before);
    assert_eq!(fixture.journal()["phase"].as_str(), Some("prepared"));
    assert_eq!(fixture.text(), ORIGINAL);
    fs::remove_file(padding).unwrap();
    disconnect(&fixture.config).unwrap();
    assert_eq!(fixture.journal()["phase"].as_str(), Some("aborted"));
    assert_eq!(fixture.text(), ORIGINAL);
}

#[test]
fn interrupted_connect_after_replacement_reserves_all_disconnect_fragments() {
    let fixture = Fixture::new(Some(ORIGINAL.as_bytes()));
    // First parent sync creates the state directory; second follows the target
    // replacement, while the durable journal still says prepared.
    crash_at(&fixture, "connect", "parent-sync", 2);
    let connected = fixture.text();
    assert_ne!(connected, ORIGINAL);
    assert_eq!(fixture.journal()["phase"].as_str(), Some("prepared"));
    let padding = fill_state_bytes(&fixture, connected.len() as u64 + ORIGINAL.len() as u64);
    let before = state_usage(&fixture);
    assert_eq!(
        disconnect(&fixture.config).unwrap_err(),
        TransactionError::StorageLimit,
        "reserving only the before/after images misses staging and journals",
    );
    assert_eq!(state_usage(&fixture), before);
    assert_eq!(fixture.text(), connected);
    assert_eq!(fixture.journal()["phase"].as_str(), Some("prepared"));
    fs::remove_file(padding).unwrap();
    disconnect(&fixture.config).unwrap();
    assert_eq!(fixture.text(), ORIGINAL);
}
