use super::*;
#[cfg(any(target_os = "linux", target_os = "android", target_os = "macos"))]
use std::fs;
#[cfg(any(target_os = "linux", target_os = "android", target_os = "macos"))]
use std::sync::atomic::{AtomicU64, Ordering};

fn manifest(id: &str, requires: &[&str], provides: &[&str]) -> PluginInput {
    PluginInput::bundled(
        "synthetic",
        format!(
            r#"
format = "magpie-plugin/1"
id = "{id}"
name = "Synthetic fixture"
version = "0.1.0"
api_version = 1
kind = "feature"
enabled_by_default = true
requires = {requires:?}
provides = {provides:?}
[[contributions.pages]]
id = "test-page"
"#
        ),
    )
}

#[cfg(any(target_os = "linux", target_os = "android", target_os = "macos"))]
struct Fixture(PathBuf);
#[cfg(any(target_os = "linux", target_os = "android", target_os = "macos"))]
impl Fixture {
    fn new() -> Self {
        static SEQUENCE: AtomicU64 = AtomicU64::new(0);
        let path = std::env::temp_dir().join(format!(
            "magpie-plugin-fixture-{}-{}",
            std::process::id(),
            SEQUENCE.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&path).expect("create unique fixture directory");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&path, fs::Permissions::from_mode(0o700))
                .expect("make fixture directory private");
        }
        Self(path)
    }
    fn registry(&self) -> PluginRegistry {
        PluginRegistry::load(
            [manifest("test.root", &[], &["test-service"])],
            &self.0.join("packages"),
            &self.0.join("plugins.toml"),
        )
        .expect("load synthetic fixture")
    }
    fn write_state(&self, bytes: &[u8]) {
        use std::io::Write;
        let mut options = fs::OpenOptions::new();
        options.write(true).create(true).truncate(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        options
            .open(self.0.join("plugins.toml"))
            .expect("open fixture state")
            .write_all(bytes)
            .expect("write fixture state");
    }
}
#[cfg(any(target_os = "linux", target_os = "android", target_os = "macos"))]
impl Drop for Fixture {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.0).expect("remove only this test's fixture");
    }
}

#[test]
fn parsing_is_not_registration_and_dependencies_wait_for_acknowledgement() {
    let mut registry = PluginRegistry::from_manifests([
        manifest("test.root", &[], &["test-service"]),
        manifest("test.child", &["test-service"], &["child-service"]),
        manifest("test.leaf", &["child-service"], &[]),
    ])
    .unwrap();
    assert!(registry.contributions().is_empty());
    assert_eq!(
        registry
            .pending_contributions()
            .iter()
            .map(|c| c.owner)
            .collect::<Vec<_>>(),
        ["test.root"]
    );
    assert_eq!(
        registry.acknowledge_activation("test.child"),
        Err(PluginError::NotReady)
    );
    registry.acknowledge_activation("test.root").unwrap();
    registry.acknowledge_activation("test.child").unwrap();
    registry.acknowledge_activation("test.leaf").unwrap();
    assert_eq!(registry.contributions().len(), 3);
    let withdrawn = registry.set_enabled("test.root", false, 0).unwrap();
    assert_eq!(withdrawn, ["test.leaf", "test.child", "test.root"]);
    assert!(registry.contributions().is_empty());
    assert_eq!(
        registry.record("test.root").unwrap().status,
        PluginStatus::Off
    );
    registry.set_enabled("test.root", true, 1).unwrap();
    assert_eq!(registry.pending_contributions().len(), 1);
    assert!(registry.contributions().is_empty());
}

#[test]
fn failures_are_isolated_and_failed_owners_are_returned_for_rollback() {
    let mut registry = PluginRegistry::from_manifests([
        manifest("test.root", &[], &["service"]),
        manifest("test.child", &["service"], &[]),
        manifest("test.other", &[], &[]),
    ])
    .unwrap();
    registry.acknowledge_activation("test.root").unwrap();
    registry.acknowledge_activation("test.child").unwrap();
    registry.acknowledge_activation("test.other").unwrap();
    assert_eq!(
        registry.fail_activation("test.root").unwrap(),
        ["test.child", "test.root"]
    );
    assert_eq!(
        registry
            .contributions()
            .iter()
            .map(|c| c.owner)
            .collect::<Vec<_>>(),
        ["test.other"]
    );
    assert_eq!(
        registry.record("test.root").unwrap().status,
        PluginStatus::Failed(PluginError::Apply)
    );
}

#[test]
fn duplicate_ids_never_shadow_bundles_or_register() {
    let mut registry = PluginRegistry::from_manifests([
        manifest("test.same", &[], &[]),
        manifest("test.same", &[], &[]),
        manifest("test.other", &[], &[]),
    ])
    .unwrap();
    assert_eq!(
        registry
            .records()
            .iter()
            .filter(|r| r.status == PluginStatus::Failed(PluginError::DuplicateId))
            .count(),
        2
    );
    assert_eq!(
        registry.acknowledge_activation("test.same"),
        Err(PluginError::NotReady)
    );
    assert_eq!(
        registry.set_enabled("test.same", true, 0),
        Err(PluginError::DuplicateId)
    );
    registry.acknowledge_activation("test.other").unwrap();
}

#[test]
fn node_mcp_and_javascript_are_explicitly_unsupported() {
    for kind in ["community-provider", "mcp-server", "gateway-middleware"] {
        let mut input = manifest("test.external", &[], &[]);
        input.text = input
            .text
            .replace("kind = \"feature\"", &format!("kind = \"{kind}\""));
        let mut registry = PluginRegistry::from_manifests([input]).unwrap();
        assert_eq!(registry.records()[0].status, PluginStatus::Unsupported);
        assert_eq!(
            registry.acknowledge_activation("test.external"),
            Err(PluginError::NotReady)
        );
        registry.set_enabled("test.external", false, 0).unwrap();
        assert_eq!(registry.records()[0].status, PluginStatus::Off);
        registry.set_enabled("test.external", true, 1).unwrap();
        assert_eq!(registry.records()[0].status, PluginStatus::Unsupported);
    }
}

#[test]
fn invalid_envelopes_are_isolated_and_errors_do_not_echo_payloads() {
    let valid = manifest("test.valid", &[], &[]);
    let mut incompatible = valid.clone();
    incompatible.text = incompatible
        .text
        .replace("api_version = 1", "api_version = 99");
    let mut bad = valid.clone();
    bad.text = "SECRET_TOKEN=unclosed[".into();
    let registry = PluginRegistry::from_manifests([valid, incompatible, bad]).unwrap();
    assert_eq!(registry.records().len(), 1);
    assert_eq!(registry.diagnostics().len(), 2);
    assert!(!format!("{:?}", registry).contains("SECRET_TOKEN"));
    for diagnostic in registry.diagnostics() {
        assert!(!diagnostic.error.to_string().contains("SECRET_TOKEN"));
    }
    for id in ["Test.id", "test..id", "test/id", ".test", "test.1id", ""] {
        assert!(
            PluginManifest::parse(&manifest(id, &[], &[]).text).is_err(),
            "ID {id:?}"
        );
    }
    let mut bad_version = manifest("test.id", &[], &[]);
    bad_version.text = bad_version.text.replace("0.1.0", "01.0.0");
    assert!(PluginManifest::parse(&bad_version.text).is_err());
}

#[test]
fn cycles_remain_pending_and_host_service_withdrawal_cascades() {
    let mut registry = PluginRegistry::from_manifests([
        manifest("test.a", &["service-b"], &["service-a"]),
        manifest("test.b", &["service-a"], &["service-b"]),
    ])
    .unwrap();
    assert!(registry.pending_contributions().is_empty());
    registry.set_host_services(["service-b".into()]).unwrap();
    registry.acknowledge_activation("test.a").unwrap();
    // b is not activated: removing the external service invalidates a.
    assert_eq!(registry.set_host_services([]).unwrap(), ["test.a"]);
    assert!(registry.contributions().is_empty());
}

#[test]
fn same_service_from_multiple_owners_survives_one_owner_disabling() {
    let mut registry = PluginRegistry::from_manifests([
        manifest("test.a", &[], &["service"]),
        manifest("test.b", &[], &["service"]),
        manifest("test.child", &["service"], &[]),
    ])
    .unwrap();
    for id in ["test.a", "test.b", "test.child"] {
        registry.acknowledge_activation(id).unwrap();
    }
    assert_eq!(
        registry.set_enabled("test.a", false, 0).unwrap(),
        ["test.a"]
    );
    assert_eq!(
        registry.record("test.child").unwrap().status,
        PluginStatus::Active
    );
}

#[test]
fn new_declarative_provider_and_agent_need_no_core_vendor_changes() {
    let provider = manifest("provider.synthetic", &[], &["fixture-provider"]);
    let mut provider = PluginInput::bundled(
        "fixture-provider",
        provider
            .text
            .replace("kind = \"feature\"", "kind = \"provider-preset\""),
    );
    provider.text.push_str(
        "\n[[contributions.providers]]\nid = 'fixture'\nbase_url = 'http://127.0.0.1:1'\n",
    );
    let agent = PluginInput::bundled(
        "fixture-agent",
        manifest("agent.synthetic", &["fixture-provider"], &[])
            .text
            .replace("kind = \"feature\"", "kind = \"agent-adapter\""),
    );
    let mut registry = PluginRegistry::from_manifests([provider, agent]).unwrap();
    let provider_input = registry.pending_contributions();
    assert_eq!(
        provider_input[0].manifest.document()["contributions"]["providers"]
            .as_array_of_tables()
            .unwrap()
            .len(),
        1
    );
    registry
        .acknowledge_activation("provider.synthetic")
        .unwrap();
    registry.acknowledge_activation("agent.synthetic").unwrap();
    assert_eq!(registry.contributions().len(), 2);
}

#[cfg(any(target_os = "linux", target_os = "android", target_os = "macos"))]
#[test]
fn user_discovery_state_restart_conflicts_and_data_preservation() {
    let fixture = Fixture::new();
    fs::create_dir_all(fixture.0.join("packages/new")).unwrap();
    fs::write(
        fixture.0.join("packages/new/plugin.toml"),
        manifest("test.user", &[], &[]).text,
    )
    .unwrap();
    fs::write(
        fixture.0.join("packages/bad.toml"),
        "SYNTHETIC_SECRET=not[valid",
    )
    .unwrap();
    fs::write(fixture.0.join("user-data"), "retain me").unwrap();
    let mut first = fixture.registry();
    let mut stale = fixture.registry();
    assert_eq!(first.records().len(), 2);
    assert_eq!(first.diagnostics().len(), 1);
    assert!(matches!(
        first.record("test.user").unwrap().source,
        PluginSource::User(_)
    ));
    first.acknowledge_activation("test.root").unwrap();
    assert_eq!(
        first.set_enabled("test.root", false, 0).unwrap(),
        ["test.root"]
    );
    assert_eq!(
        stale.set_enabled("test.root", true, 0),
        Err(PluginError::Conflict)
    );
    let mut reloaded = fixture.registry();
    assert_eq!(reloaded.revision(), 1);
    assert_eq!(
        reloaded.record("test.root").unwrap().status,
        PluginStatus::Off
    );
    reloaded.set_enabled("test.root", true, 1).unwrap();
    reloaded.acknowledge_activation("test.root").unwrap();
    assert_eq!(reloaded.reload().unwrap(), ["test.root"]);
    assert!(reloaded.contributions().is_empty());
    assert_eq!(
        fs::read_to_string(fixture.0.join("user-data")).unwrap(),
        "retain me"
    );
    use std::os::unix::fs::PermissionsExt;
    assert_eq!(
        fs::metadata(fixture.0.join("plugins.toml"))
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o600
    );
    let mut bytes = fs::read(fixture.0.join("plugins.toml")).unwrap();
    bytes.extend_from_slice(b"\n# external edit, unchanged revision\n");
    fixture.write_state(&bytes);
    assert_eq!(
        reloaded.set_enabled("test.root", false, 2),
        Err(PluginError::Conflict)
    );
}

#[cfg(any(target_os = "linux", target_os = "android", target_os = "macos"))]
#[test]
fn corrupt_state_is_preserved_and_failed_reload_keeps_previous_registry() {
    let fixture = Fixture::new();
    let mut registry = fixture.registry();
    registry.acknowledge_activation("test.root").unwrap();
    fixture.write_state(b"PRIVATE_SYNTHETIC_VALUE=broken[");
    assert_eq!(registry.reload(), Err(PluginError::InvalidFormat));
    assert_eq!(
        registry.record("test.root").unwrap().status,
        PluginStatus::Active
    );
    assert_eq!(
        registry.set_enabled("test.root", false, 0),
        Err(PluginError::Conflict)
    );
    assert!(matches!(
        PluginRegistry::load(
            [],
            &fixture.0.join("packages"),
            &fixture.0.join("plugins.toml")
        ),
        Err(PluginError::InvalidFormat)
    ));
    assert_eq!(
        fs::read(fixture.0.join("plugins.toml")).unwrap(),
        b"PRIVATE_SYNTHETIC_VALUE=broken["
    );
}

#[cfg(any(target_os = "linux", target_os = "android", target_os = "macos"))]
#[test]
fn symlinks_hardlinks_and_public_state_are_rejected_without_touching_targets() {
    use std::os::unix::fs::{PermissionsExt, symlink};
    let fixture = Fixture::new();
    fixture.write_state(&encode_state(0, &BTreeMap::new()));
    let state = fixture.0.join("plugins.toml");
    let target = fixture.0.join("retained.toml");
    fs::rename(&state, &target).unwrap();
    symlink(&target, &state).unwrap();
    assert!(matches!(
        PluginRegistry::load([], &fixture.0.join("packages"), &state),
        Err(PluginError::UnsafePath)
    ));
    fs::remove_file(&state).unwrap();
    fs::hard_link(&target, &state).unwrap();
    assert!(matches!(
        PluginRegistry::load([], &fixture.0.join("packages"), &state),
        Err(PluginError::NotAFile)
    ));
    fs::remove_file(&state).unwrap();
    fs::rename(&target, &state).unwrap();
    fs::set_permissions(&state, fs::Permissions::from_mode(0o644)).unwrap();
    assert!(matches!(
        PluginRegistry::load([], &fixture.0.join("packages"), &state),
        Err(PluginError::UnsafePath)
    ));
    fs::create_dir_all(fixture.0.join("packages")).unwrap();
    symlink(&state, fixture.0.join("packages/link.toml")).unwrap();
    let mut registry = PluginRegistry::from_manifests([]).unwrap();
    registry.user_directory = Some(fixture.0.join("packages"));
    registry.discover().unwrap();
    assert!(registry.records().is_empty());
    assert_eq!(registry.diagnostics()[0].error, PluginError::UnsafePath);
}

#[test]
fn bounded_envelopes_and_revision_overflow_do_not_mutate_state() {
    let input = manifest("test.root", &[], &[]);
    let mut huge = input.clone();
    huge.text.push_str(&" ".repeat(MAX_BYTES));
    assert_eq!(
        PluginManifest::parse(&huge.text).unwrap_err(),
        PluginError::TooLarge
    );
    let too_many = std::iter::repeat_n(input.clone(), MAX_PLUGINS + 1);
    assert!(matches!(
        PluginRegistry::from_manifests(too_many),
        Err(PluginError::Limit)
    ));
    let mut registry = PluginRegistry::from_manifests([input]).unwrap();
    registry.revision = i64::MAX as u64;
    assert_eq!(
        registry.set_enabled("test.root", false, i64::MAX as u64),
        Err(PluginError::RevisionOverflow)
    );
    assert!(registry.records()[0].enabled);
}

#[cfg(any(target_os = "linux", target_os = "android", target_os = "macos"))]
#[test]
fn state_lock_contention_reports_busy_without_changing_preferences() {
    use rustix::fs::{FlockOperation, flock};
    use std::os::unix::fs::OpenOptionsExt;
    let fixture = Fixture::new();
    let mut registry = fixture.registry();
    let lock = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .open(fixture.0.join(".plugins.toml.lock"))
        .unwrap();
    flock(&lock, FlockOperation::NonBlockingLockExclusive).unwrap();
    assert_eq!(
        registry.set_enabled("test.root", false, 0),
        Err(PluginError::Busy)
    );
    assert_eq!(registry.revision(), 0);
    assert!(registry.records()[0].enabled);
    assert!(!fixture.0.join("plugins.toml").exists());
    flock(&lock, FlockOperation::Unlock).unwrap();
    registry.set_enabled("test.root", false, 0).unwrap();
}

#[cfg(any(target_os = "linux", target_os = "android", target_os = "macos"))]
#[test]
fn discovery_limits_fail_instead_of_claiming_partial_success() {
    let fixture = Fixture::new();
    let directory = fixture.0.join("packages");
    fs::create_dir(&directory).unwrap();
    for index in 0..=MAX_SCAN_ENTRIES {
        fs::write(directory.join(format!("ignored-{index}.txt")), "").unwrap();
    }
    assert!(matches!(
        PluginRegistry::load([], &directory, &fixture.0.join("plugins.toml")),
        Err(PluginError::Limit)
    ));
}

#[test]
fn invalid_state_schema_and_service_lists_are_rejected_without_echoing_values() {
    for state in [
        "format='magpie-plugin-state/1'\nrevision=-1\n[enabled]",
        "format='magpie-plugin-state/1'\nrevision=0\n[enabled]\n\"test.id\"='PRIVATE_VALUE'",
        "format='magpie-plugin-state/1'\nrevision=0\nunknown='PRIVATE_VALUE'\n[enabled]",
        "format='magpie-plugin-state/9'\nrevision=0\n[enabled]",
    ] {
        let error = parse_state(Some(state.as_bytes())).unwrap_err();
        assert!(!error.to_string().contains("PRIVATE_VALUE"));
    }
    assert!(
        PluginManifest::parse(&manifest("test.id", &["service", "service"], &[]).text).is_err()
    );
    assert!(PluginManifest::parse(&manifest("test.id", &["service/invalid"], &[]).text).is_err());
    let registry = PluginRegistry::from_manifests([manifest("test.id", &[], &[])]).unwrap();
    assert!(!format!("{registry:?}").contains("snapshot"));
}

#[test]
fn manifest_versions_reject_ambiguous_identifiers() {
    for version in [
        "1.2.3",
        "1.2.3-beta.1",
        "0.0.0+build.001",
        "2.3.4-rc.1+build-42",
    ] {
        assert!(valid_version(version), "version {version}");
    }
    for version in [
        "1.2",
        "1.2.3.4",
        "01.2.3",
        "1.2.3-",
        "1.2.3-.",
        "1.2.3-01",
        "1.2.3+",
        "1.2.3+build+extra",
    ] {
        assert!(!valid_version(version), "version {version}");
    }
}
