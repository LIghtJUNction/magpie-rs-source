use super::*;
use std::{
    fs,
    path::PathBuf,
    sync::atomic::{AtomicU64, Ordering},
};
struct Fixture(PathBuf);
impl Fixture {
    fn new() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let path = std::env::temp_dir().canonicalize().unwrap().join(format!(
            "magpie-bundle-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&path).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&path, fs::Permissions::from_mode(0o700)).unwrap();
        }
        Self(path)
    }
    fn load(&self) -> AppPlugins {
        AppPlugins::load_at(&self.0.join("plugins"), &self.0.join("plugins.toml"), &[]).unwrap()
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.0).unwrap();
    }
}
#[test]
fn pure_bundle_installs_validated_owners_and_has_no_usage_runtime() {
    let fixture = Fixture::new();
    let app = fixture.load();
    assert!(app.providers.len() >= 20);
    assert!(app.agents.len() >= 2);
    assert!(app.metadata_subscribers.is_empty());
    assert!(!app.active("feature.usage"));
    for record in app.registry.records() {
        assert_eq!(
            record.enabled,
            record.manifest.enabled_by_default,
            "fresh bundle must honor default enablement for {}",
            record.owner()
        );
        if record.enabled {
            assert!(
                matches!(
                    record.status,
                    PluginStatus::Active | PluginStatus::Pending { .. }
                ),
                "enabled bundle owner {} has unexpected status {:?}",
                record.owner(),
                record.status
            );
        } else {
            assert_eq!(
                record.status,
                PluginStatus::Off,
                "disabled bundle owner {} must remain off",
                record.owner()
            );
        }
    }
    assert!(app.registry.diagnostics().is_empty());
    assert_eq!(
        fs::read_dir(&fixture.0).unwrap().count(),
        0,
        "read-only bundle loading creates no business files"
    );
}
#[test]
fn user_provider_is_registered_and_disable_withdraws_only_its_contributions() {
    let fixture = Fixture::new();
    let dir = fixture.0.join("plugins/custom");
    fs::create_dir_all(&dir).unwrap();
    fs::write(
        dir.join("plugin.toml"),
        r#"format='magpie-plugin/1'
id='provider.synthetic'
name='Synthetic'
version='1.0.0'
api_version=1
kind='provider-preset'
enabled_by_default=true
requires=[]
provides=[]
[[contributions.providers]]
id='synthetic'
name='Synthetic'
base_url='http://127.0.0.1:9/v1'
protocol='openai_chat'
requires_key=false
source='https://example.invalid/api'
"#,
    )
    .unwrap();
    let mut app = fixture.load();
    assert!(app.active("provider.synthetic"));
    assert!(
        app.providers
            .iter()
            .any(|p| p.owner == "provider.synthetic" && p.preset.id == "synthetic")
    );
    app.registry
        .set_enabled("provider.synthetic", false, 0)
        .unwrap();
    let app = fixture.load();
    assert!(!app.active("provider.synthetic"));
    assert!(
        !app.providers
            .iter()
            .any(|p| p.owner == "provider.synthetic")
    );
}
#[test]
fn invalid_contributions_roll_back_and_unknown_features_remain_pending() {
    let fixture = Fixture::new();
    let dir = fixture.0.join("plugins");
    fs::create_dir(&dir).unwrap();
    fs::write(dir.join("unknown.toml"), "format='magpie-plugin/1'\nid='feature.unknown'\nname='Unknown'\nversion='1.0.0'\napi_version=1\nkind='feature'\nenabled_by_default=true\nrequires=[]\nprovides=[]\n[contributions]\n").unwrap();
    fs::write(dir.join("bad.toml"), "format='magpie-plugin/1'\nid='provider.bad'\nname='Bad'\nversion='1.0.0'\napi_version=1\nkind='provider-preset'\nenabled_by_default=true\nrequires=[]\nprovides=[]\n[[contributions.providers]]\nid='bad'\n").unwrap();
    let app = fixture.load();
    assert!(matches!(
        app.registry.record("feature.unknown").unwrap().status,
        PluginStatus::Pending { .. }
    ));
    assert!(matches!(
        app.registry.record("provider.bad").unwrap().status,
        PluginStatus::Failed(_)
    ));
    assert!(!app.providers.iter().any(|p| p.owner == "provider.bad"));
    assert!(app.active("agent.claude"));
}
#[cfg(unix)]
#[test]
fn user_agent_resources_reject_traversal_symlinks_and_oversize_without_echoing() {
    use std::os::unix::fs::symlink;
    let fixture = Fixture::new();
    let package = fixture.0.join("plugins/agent");
    fs::create_dir_all(&package).unwrap();
    let text = BUNDLE
        .iter()
        .find(|b| b.label == "agents/claude/plugin.toml")
        .unwrap()
        .text
        .replace("agent.claude", "agent.custom");
    fs::write(package.join("plugin.toml"), &text).unwrap();
    symlink(
        fixture.0.join("external.json"),
        package.join("definition.json"),
    )
    .unwrap();
    fs::write(fixture.0.join("external.json"), "PRIVATE_FIXTURE").unwrap();
    assert!(matches!(
        fixture
            .load()
            .registry
            .record("agent.custom")
            .unwrap()
            .status,
        PluginStatus::Failed(_)
    ));
    fs::write(
        package.join("plugin.toml"),
        text.replace("definition.json", "../external.json"),
    )
    .unwrap();
    let app = fixture.load();
    assert!(matches!(
        app.registry.record("agent.custom").unwrap().status,
        PluginStatus::Failed(_)
    ));
    assert!(!app.to_json().to_string().contains("PRIVATE_FIXTURE"));
    fs::remove_file(package.join("definition.json")).unwrap();
    fs::write(package.join("plugin.toml"), &text).unwrap();
    fs::write(package.join("definition.json"), " ".repeat(64 * 1024 + 1)).unwrap();
    assert!(matches!(
        fixture
            .load()
            .registry
            .record("agent.custom")
            .unwrap()
            .status,
        PluginStatus::Failed(_)
    ));
}

#[test]
fn a_user_manifest_cannot_claim_a_compiled_engine_factory() {
    let fixture = Fixture::new();
    let directory = fixture.0.join("plugins");
    fs::create_dir(&directory).unwrap();
    fs::write(directory.join("native.toml"), "format='magpie-plugin/1'\nid='agent.unlinked'\nname='Unlinked'\nversion='1.0.0'\napi_version=1\nkind='agent-adapter'\nenabled_by_default=true\nrequires=[]\nprovides=[]\n[[contributions.agents]]\nid='unlinked'\nname='Unlinked'\nengine='native-codex/1'\n").unwrap();
    let app = AppPlugins::load_at_with_engines(
        &directory,
        &fixture.0.join("plugins.toml"),
        &[],
        &["native-codex/1".into()],
    )
    .unwrap();
    assert!(!app.active("agent.unlinked"));
    assert!(matches!(
        app.registry.record("agent.unlinked").unwrap().status,
        PluginStatus::Pending { .. }
    ));
    assert!(app.active("agent.codex"));
}
