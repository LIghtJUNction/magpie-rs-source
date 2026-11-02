#![cfg(any(target_os = "linux", target_os = "android", target_os = "macos"))]

use magpie_core::provider_store::{
    MAX_PROVIDER_MODELS, MAX_TOTAL_MODELS, Provider, ProviderProtocol, ProviderStore, StoreError,
};
use std::fs::{self, DirBuilder, OpenOptions};
use std::os::unix::fs::{DirBuilderExt, PermissionsExt, symlink};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Barrier};
use std::time::{SystemTime, UNIX_EPOCH};

static NEXT: AtomicU64 = AtomicU64::new(0);

struct Fixture {
    directory: PathBuf,
    path: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let timestamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let directory = std::env::temp_dir().canonicalize().unwrap().join(format!(
            "magpie-provider-store-{}-{timestamp}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed),
        ));
        DirBuilder::new().mode(0o700).create(&directory).unwrap();
        Self {
            path: directory.join("providers.toml"),
            directory,
        }
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::set_permissions(&self.directory, fs::Permissions::from_mode(0o700));
        let _ = fs::remove_dir_all(&self.directory);
    }
}

fn provider(id: &str) -> Provider {
    Provider {
        id: id.into(),
        name: "Local fixture · 本地".into(),
        base_url: "http://127.0.0.1:18421/v1".into(),
        protocol: ProviderProtocol::OpenAiChat,
        api_key_env: Some("MAGPIE_SYNTHETIC_KEY".into()),
        credential_ref: None,
        models: vec!["tiny/model-a".into(), "model-b:latest".into()],
        enabled: true,
    }
}

#[test]
fn create_edit_pause_resume_and_remove_survive_reloads() {
    let fixture = Fixture::new();
    let empty = ProviderStore::load(&fixture.path).unwrap();
    assert_eq!(empty.revision(), 0);
    assert!(empty.providers().is_empty());
    assert!(!fixture.path.exists());

    let created = ProviderStore::create(&fixture.path, provider("local"), 0).unwrap();
    assert_eq!(created, ProviderStore::load(&fixture.path).unwrap());
    assert_eq!(created.revision(), 1);

    let mut edited = created.provider("local").unwrap().clone();
    edited.name = "Edited fixture".into();
    edited.api_key_env = None;
    edited.credential_ref = Some("magpie/provider/local/key-2".into());
    edited.protocol = ProviderProtocol::OpenAiResponses;
    let updated = ProviderStore::update(&fixture.path, edited.clone(), 1).unwrap();
    assert_eq!(updated.provider("local"), Some(&edited));
    assert_eq!(updated, ProviderStore::load(&fixture.path).unwrap());

    let paused = ProviderStore::set_enabled(&fixture.path, "local", false, 2).unwrap();
    assert!(!paused.provider("local").unwrap().enabled);
    assert_eq!(paused.provider("local").unwrap().models, edited.models);
    assert_eq!(
        paused.provider("local").unwrap().credential_ref,
        edited.credential_ref
    );
    let resumed = ProviderStore::set_enabled(&fixture.path, "local", true, 3).unwrap();
    assert!(resumed.provider("local").unwrap().enabled);
    let removed = ProviderStore::remove(&fixture.path, "local", 4).unwrap();
    assert_eq!(removed.revision(), 5);
    assert!(removed.providers().is_empty());
    assert_eq!(removed, ProviderStore::load(&fixture.path).unwrap());
    assert_eq!(
        fs::metadata(&fixture.path).unwrap().permissions().mode() & 0o777,
        0o600
    );
}

#[test]
fn a_stale_editor_cannot_overwrite_or_delete_another_clients_change() {
    let fixture = Fixture::new();
    ProviderStore::create(&fixture.path, provider("local"), 0).unwrap();
    let newer = ProviderStore::create(&fixture.path, provider("second"), 1).unwrap();
    let saved = fs::read(&fixture.path).unwrap();
    assert_eq!(
        ProviderStore::update(&fixture.path, provider("local"), 1),
        Err(StoreError::Conflict)
    );
    assert_eq!(
        ProviderStore::remove(&fixture.path, "local", 1),
        Err(StoreError::Conflict)
    );
    assert_eq!(
        ProviderStore::set_enabled(&fixture.path, "local", false, 1),
        Err(StoreError::Conflict)
    );
    assert_eq!(saved, fs::read(&fixture.path).unwrap());
    assert_eq!(newer, ProviderStore::load(&fixture.path).unwrap());
}

#[test]
fn simultaneous_writers_apply_only_one_update_to_the_same_revision() {
    let fixture = Fixture::new();
    ProviderStore::create(&fixture.path, provider("initial"), 0).unwrap();
    let barrier = Arc::new(Barrier::new(2));
    let writers: Vec<_> = ["a", "b"]
        .into_iter()
        .map(|id| {
            let path = fixture.path.clone();
            let barrier = barrier.clone();
            std::thread::spawn(move || {
                barrier.wait();
                ProviderStore::create(&path, provider(id), 1)
            })
        })
        .collect();
    let results: Vec<_> = writers
        .into_iter()
        .map(|writer| writer.join().unwrap())
        .collect();
    assert_eq!(results.iter().filter(|result| result.is_ok()).count(), 1);
    assert!(
        results
            .iter()
            .filter_map(|result| result.as_ref().err())
            .all(|error| matches!(error, StoreError::Busy | StoreError::Conflict))
    );
    let reloaded = ProviderStore::load(&fixture.path).unwrap();
    assert_eq!(reloaded.revision(), 2);
    assert_eq!(reloaded.providers().len(), 2);
}

#[test]
fn invalid_edits_and_duplicate_ids_preserve_the_saved_file() {
    let fixture = Fixture::new();
    ProviderStore::create(&fixture.path, provider("local"), 0).unwrap();
    let original = fs::read(&fixture.path).unwrap();
    assert_eq!(
        ProviderStore::create(&fixture.path, provider("local"), 1),
        Err(StoreError::DuplicateProvider)
    );
    assert_eq!(
        ProviderStore::update(&fixture.path, provider("missing"), 1),
        Err(StoreError::ProviderNotFound)
    );
    assert_eq!(
        ProviderStore::remove(&fixture.path, "missing", 1),
        Err(StoreError::ProviderNotFound)
    );
    let mut invalid = provider("local");
    invalid.models.push(invalid.models[0].clone());
    assert_eq!(
        ProviderStore::update(&fixture.path, invalid, 1),
        Err(StoreError::InvalidField("models"))
    );
    assert_eq!(original, fs::read(&fixture.path).unwrap());
}

#[test]
fn corrupt_unknown_or_future_configuration_never_becomes_an_empty_store() {
    let fixture = Fixture::new();
    for source in [
        "",
        "version = 2\nrevision = 1\nproviders = []\n",
        "version = 1\nrevision = 0\nproviders = []\n",
        "version = 1\nrevision = 1\nproviders = []\napi_key = 'SECRET_SENTINEL'\n",
        "version = 1\nrevision = 1\nproviders = [SECRET_SENTINEL]\n",
    ] {
        fs::write(&fixture.path, source).unwrap();
        let error = ProviderStore::load(&fixture.path).unwrap_err();
        assert!(!format!("{error:?} {error}").contains("SECRET_SENTINEL"));
        assert!(ProviderStore::create(&fixture.path, provider("local"), 0).is_err());
        assert_eq!(fs::read_to_string(&fixture.path).unwrap(), source);
    }
}

#[test]
fn malformed_and_credential_bearing_urls_are_rejected_before_writing() {
    let fixture = Fixture::new();
    for url in [
        "",
        "file:///tmp/config",
        "http://relay.example/v1",
        "http://127.0.0.1:00/v1",
        "https://user:SECRET_SENTINEL@relay.example/v1",
        "https://relay.example/v1?key=SECRET_SENTINEL",
        "https://relay.example/v1#fragment",
        "https://relay.example:65536/v1",
        "https://relay.example/v1\nAuthorization:secret",
        "https://relay.example\\@127.0.0.1/v1",
        "http://[::1]:0/v1",
        "https://[broken/v1",
        "https://-bad.example/v1",
        "https://relay.example/%",
        "https://relay.example/%ZZ",
        "https://relay.example/v1/chat/completions",
        "https://relay.example/v1/responses",
        "https://relay.example/v1/messages",
    ] {
        let mut invalid = provider("local");
        invalid.base_url = url.into();
        let error = ProviderStore::create(&fixture.path, invalid, 0).unwrap_err();
        assert_eq!(error, StoreError::InvalidField("base_url"), "{url}");
        assert!(!format!("{error:?} {error}").contains("SECRET_SENTINEL"));
        assert!(!fixture.path.exists());
    }
    for url in [
        "https://api.openai.com/v1",
        "https://api.anthropic.com",
        "https://generativelanguage.googleapis.com/v1beta",
        "http://localhost:11434/v1",
        "http://127.0.0.2:8080/v1",
        "http://[::1]:1234/v1",
        "https://relay.example:8443/tenant%20name/v1/",
    ] {
        let mut valid = provider("local");
        valid.base_url = url.into();
        assert_eq!(valid.validate(), Ok(()), "{url}");
    }
}

#[test]
fn credential_sources_and_model_boundaries_are_validated() {
    let mut value = provider("local");
    value.credential_ref = Some("keyring:local/1".into());
    assert_eq!(
        value.validate(),
        Err(StoreError::InvalidField("credential_source"))
    );
    value.api_key_env = Some("INVALID=SECRET_SENTINEL".into());
    assert_eq!(
        value.validate(),
        Err(StoreError::InvalidField("api_key_env"))
    );
    value.api_key_env = None;
    value.credential_ref = Some("bad\nreference".into());
    assert_eq!(
        value.validate(),
        Err(StoreError::InvalidField("credential_ref"))
    );
    value.credential_ref = None;
    value.models = (0..MAX_PROVIDER_MODELS)
        .map(|index| format!("model-{index}"))
        .collect();
    assert_eq!(value.validate(), Ok(()));
    value.models.push("extra".into());
    assert_eq!(value.validate(), Err(StoreError::InvalidField("models")));
    value.models = vec!["vendor/../model".into()];
    assert_eq!(value.validate(), Err(StoreError::InvalidField("models")));
    value.models.clear();
    assert_eq!(value.validate(), Ok(()));
}

#[test]
fn linked_targets_parents_and_lock_files_are_refused() {
    let fixture = Fixture::new();
    let other = fixture.directory.join("other.toml");
    fs::write(&other, "DO_NOT_CHANGE").unwrap();
    symlink(&other, &fixture.path).unwrap();
    assert_eq!(
        ProviderStore::load(&fixture.path),
        Err(StoreError::UnsafePath)
    );
    assert_eq!(
        ProviderStore::create(&fixture.path, provider("local"), 0),
        Err(StoreError::UnsafePath)
    );
    assert_eq!(fs::read_to_string(&other).unwrap(), "DO_NOT_CHANGE");
    fs::remove_file(&fixture.path).unwrap();

    let linked_parent = fixture.directory.join("linked");
    symlink(&fixture.directory, &linked_parent).unwrap();
    assert_eq!(
        ProviderStore::load(&linked_parent.join("providers.toml")),
        Err(StoreError::UnsafePath)
    );
    assert_eq!(
        ProviderStore::create(&linked_parent.join("providers.toml"), provider("local"), 0),
        Err(StoreError::UnsafePath)
    );

    fs::remove_file(fixture.directory.join(".providers.toml.lock")).unwrap();
    symlink(&other, fixture.directory.join(".providers.toml.lock")).unwrap();
    assert_eq!(
        ProviderStore::create(&fixture.path, provider("local"), 0),
        Err(StoreError::UnsafePath)
    );
    assert_eq!(fs::read_to_string(&other).unwrap(), "DO_NOT_CHANGE");
}

#[test]
fn nonregular_hardlinked_and_oversized_files_are_refused() {
    let fixture = Fixture::new();
    fs::create_dir(&fixture.path).unwrap();
    assert_eq!(
        ProviderStore::load(&fixture.path),
        Err(StoreError::NotAFile)
    );
    fs::remove_dir(&fixture.path).unwrap();
    fs::write(&fixture.path, "fixture").unwrap();
    fs::hard_link(&fixture.path, fixture.directory.join("alias.toml")).unwrap();
    assert_eq!(
        ProviderStore::load(&fixture.path),
        Err(StoreError::NotAFile)
    );
    fs::remove_file(fixture.directory.join("alias.toml")).unwrap();
    OpenOptions::new()
        .write(true)
        .open(&fixture.path)
        .unwrap()
        .set_len(1024 * 1024 + 1)
        .unwrap();
    assert_eq!(
        ProviderStore::load(&fixture.path),
        Err(StoreError::TooLarge)
    );
}

#[test]
fn unsafe_and_read_only_directories_are_not_written() {
    let fixture = Fixture::new();
    for mode in [0o777, 0o775, 0o500] {
        fs::set_permissions(&fixture.directory, fs::Permissions::from_mode(mode)).unwrap();
        assert_eq!(
            ProviderStore::create(&fixture.path, provider("local"), 0),
            Err(StoreError::UnsafePath)
        );
        assert!(!fixture.path.exists());
    }
}

#[test]
fn missing_private_directories_are_created_and_traversal_is_rejected() {
    let fixture = Fixture::new();
    let nested = fixture
        .directory
        .join("configuration/magpie/providers.toml");
    assert_eq!(ProviderStore::load(&nested).unwrap().revision(), 0);
    assert!(!nested.parent().unwrap().exists());
    ProviderStore::create(&nested, provider("local"), 0).unwrap();
    assert_eq!(
        fs::metadata(nested.parent().unwrap())
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o700
    );
    let traversal = fixture.directory.join("not-created/../providers.toml");
    assert_eq!(ProviderStore::load(&traversal), Err(StoreError::UnsafePath));
    assert_eq!(
        ProviderStore::create(&traversal, provider("local"), 0),
        Err(StoreError::UnsafePath)
    );
    assert!(!fixture.directory.join("not-created").exists());
}

#[test]
fn all_four_protocols_round_trip() {
    let fixture = Fixture::new();
    for (revision, protocol) in ProviderProtocol::ALL.into_iter().enumerate() {
        let mut value = provider(&format!("provider-{revision}"));
        value.protocol = protocol;
        assert_eq!(ProviderProtocol::parse(protocol.as_str()), Ok(protocol));
        ProviderStore::create(&fixture.path, value, revision as u64).unwrap();
    }
    let loaded = ProviderStore::load(&fixture.path).unwrap();
    assert_eq!(
        loaded
            .providers()
            .iter()
            .map(|provider| provider.protocol)
            .collect::<Vec<_>>(),
        ProviderProtocol::ALL
    );
}

#[test]
fn aggregate_model_limit_rejects_overflow_without_truncation() {
    let fixture = Fixture::new();
    let mut revision = 0;
    for index in 0..MAX_TOTAL_MODELS / MAX_PROVIDER_MODELS {
        let mut value = provider(&format!("relay-{index}"));
        value.models = (0..MAX_PROVIDER_MODELS)
            .map(|model| format!("model-{model}"))
            .collect();
        revision = ProviderStore::create(&fixture.path, value, revision)
            .unwrap()
            .revision();
    }
    let saved = fs::read(&fixture.path).unwrap();
    let mut overflow = provider("overflow");
    overflow.models = vec!["one-more".into()];
    assert_eq!(
        ProviderStore::create(&fixture.path, overflow, revision),
        Err(StoreError::TotalModelLimit)
    );
    assert_eq!(saved, fs::read(&fixture.path).unwrap());
    let loaded = ProviderStore::load(&fixture.path).unwrap();
    assert_eq!(
        loaded
            .providers()
            .iter()
            .map(|provider| provider.models.len())
            .sum::<usize>(),
        MAX_TOTAL_MODELS
    );
}
