use std::path::PathBuf;
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};

static NEXT_FIXTURE: AtomicU64 = AtomicU64::new(0);

struct Fixture(PathBuf);

impl Fixture {
    fn new(contents: &str) -> Self {
        let path = std::env::temp_dir().join(format!(
            "magpie-cli-catalog-{}-{}.toml",
            std::process::id(),
            NEXT_FIXTURE.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::write(&path, contents).expect("write isolated catalog fixture");
        Self(path)
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

#[test]
fn lists_configured_models_from_an_explicit_catalog() {
    let fixture =
        Fixture::new("[[providers]]\nid = 'local'\nname = 'Local'\nmodels = ['alpha', 'beta']\n");
    let result = Command::new(env!("CARGO_BIN_EXE_magpie"))
        .args(["models", "--json", "--catalog"])
        .arg(&fixture.0)
        .output()
        .expect("run CLI");
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    let stdout = String::from_utf8(result.stdout).expect("UTF-8 catalog output");
    assert!(stdout.contains("\"local/alpha\""), "{stdout}");
    assert!(stdout.contains("\"local/beta\""), "{stdout}");
    assert!(result.stderr.is_empty());
}

#[test]
fn invalid_catalog_is_a_failure_with_a_structured_error() {
    let fixture = Fixture::new("[[providers]\n");
    let result = Command::new(env!("CARGO_BIN_EXE_magpie"))
        .args(["models", "--catalog"])
        .arg(&fixture.0)
        .arg("--json")
        .output()
        .expect("run CLI");
    assert!(!result.status.success());
    assert!(result.stdout.is_empty());
    let stderr = String::from_utf8(result.stderr).expect("UTF-8 error output");
    assert!(stderr.contains("\"code\":\"catalog_error\""), "{stderr}");
}

#[test]
fn refuses_public_listener_before_reporting_readiness() {
    let result = Command::new(env!("CARGO_BIN_EXE_magpie"))
        .args(["serve", "--listen", "0.0.0.0:0"])
        .output()
        .expect("run CLI");
    assert!(!result.status.success());
    assert!(result.stdout.is_empty());
    let stderr = String::from_utf8(result.stderr).expect("UTF-8 error output");
    assert!(
        stderr.contains("Gateway only binds to loopback"),
        "{stderr}"
    );
}

#[cfg(unix)]
#[test]
fn agent_listing_uses_installed_executables_and_only_selected_settings() {
    use std::os::unix::fs::PermissionsExt;
    struct AgentFixture(PathBuf);
    impl Drop for AgentFixture {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
    let fixture = AgentFixture(std::env::temp_dir().join(format!(
        "magpie-cli-agents-{}-{}",
        std::process::id(),
        NEXT_FIXTURE.fetch_add(1, Ordering::Relaxed)
    )));
    let bin = fixture.0.join("bin");
    let home = fixture.0.join("codex-home");
    std::fs::create_dir_all(&bin).unwrap();
    std::fs::create_dir(&home).unwrap();
    for name in ["codex", "claude"] {
        let path = bin.join(name);
        std::fs::write(&path, "#!/bin/sh\nexit 0\n").unwrap();
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700)).unwrap();
    }
    std::fs::write(home.join("config.toml"), "model = 'example-model'\nmodel_provider = 'example'\nmodel_reasoning_effort = 'high'\napi_key = 'private-fixture-value'\n").unwrap();
    let result = Command::new(env!("CARGO_BIN_EXE_magpie"))
        .args(["agents", "--json"])
        .env("PATH", &bin)
        .env("HOME", &fixture.0)
        .env("CODEX_HOME", &home)
        .output()
        .unwrap();
    assert!(result.status.success());
    let stdout = String::from_utf8(result.stdout).unwrap();
    for expected in [
        "\"id\":\"claude\"",
        "\"id\":\"codex\"",
        "\"model\":\"example-model\"",
        "\"model_provider\":\"example\"",
        "\"reasoning_effort\":\"high\"",
    ] {
        assert!(stdout.contains(expected), "{stdout}");
    }
    for absent in ["\"id\":\"gemini\"", "private-fixture-value", "api_key"] {
        assert!(!stdout.contains(absent), "{stdout}");
    }
}
