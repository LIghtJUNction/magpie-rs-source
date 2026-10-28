//! Process-level byte comparisons against the isolated original 39 observations.

#![cfg(target_os = "linux")]

use std::{
    fs,
    os::unix::fs::{DirBuilderExt, PermissionsExt},
    path::PathBuf,
    process::{Command, Output},
    sync::atomic::{AtomicU64, Ordering},
};

static NEXT: AtomicU64 = AtomicU64::new(0);

struct Fixture {
    root: PathBuf,
}

impl Fixture {
    fn new(programs: &[&str], codex_directory: bool) -> Self {
        let root = std::env::temp_dir().join(format!(
            "magpie-candidate-cli-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::DirBuilder::new().mode(0o700).create(&root).unwrap();
        fs::create_dir(root.join("bin")).unwrap();
        fs::create_dir(root.join("home")).unwrap();
        if codex_directory {
            fs::create_dir(root.join("codex")).unwrap();
        }
        for program in programs {
            let path = root.join("bin").join(program);
            fs::write(
                &path,
                format!(
                    "#!/usr/bin/sh\nprintf called >> '{}'\nexit 97\n",
                    root.join("called").display()
                ),
            )
            .unwrap();
            fs::set_permissions(path, fs::Permissions::from_mode(0o700)).unwrap();
        }
        Self { root }
    }

    fn command(&self, arguments: &[&str]) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_magpie"));
        command
            .args(arguments)
            .env_clear()
            .env("PATH", self.root.join("bin"))
            .env("HOME", self.root.join("home"))
            .env("CODEX_HOME", self.root.join("codex"))
            .env("XDG_CONFIG_HOME", self.root.join("config"))
            .env("XDG_CACHE_HOME", self.root.join("cache"))
            .env("XDG_DATA_HOME", self.root.join("data"))
            .env("XDG_STATE_HOME", self.root.join("state"))
            .current_dir(&self.root);
        command
    }

    fn run(&self, arguments: &[&str]) -> Output {
        self.command(arguments).output().unwrap()
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.root).unwrap();
    }
}

fn golden(scenario: &str, command: &str, stream: &str) -> Vec<u8> {
    fs::read(
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/legacy_agent_candidates")
            .join(format!("{scenario}-{command}.{stream}")),
    )
    .unwrap()
}

#[test]
fn original_39_candidate_and_empty_streams_paths_and_codes_match_exactly() {
    for (programs, directory, scenario) in [
        (&[][..], false, "control-empty"),
        (&["claude"][..], false, "claude-only"),
        (&["codex"][..], false, "codex-only"),
        (&["gemini"][..], false, "gemini-only"),
        (&["claude", "codex", "gemini"][..], false, "all-three"),
        (&["codex"][..], true, "codex-only"),
        (&["claude", "codex", "gemini"][..], true, "all-three"),
    ] {
        let fixture = Fixture::new(programs, directory);
        for command in ["agents", "ls"] {
            let output = fixture.run(&[command]);
            let expected = String::from_utf8(golden(scenario, command, "stdout"))
                .unwrap()
                .replace(
                    "/fixture/codex/config.toml",
                    fixture.root.join("codex/config.toml").to_str().unwrap(),
                );
            assert_eq!(
                output.stdout,
                expected.as_bytes(),
                "{scenario} {command} directory={directory}"
            );
            assert_eq!(
                output.stderr,
                golden(scenario, command, "stderr"),
                "{scenario} {command} stderr"
            );
            assert_eq!(
                output.status.code(),
                Some(if programs.is_empty() && command == "ls" {
                    1
                } else {
                    0
                }),
                "{scenario} {command} exit"
            );
        }
        assert!(!fixture.root.join("called").exists());
        assert!(!fixture.root.join("codex/config.toml").exists());
        assert!(!fixture.root.join("home/.claude").exists());
        assert!(!fixture.root.join("home/.gemini").exists());
    }
}

#[test]
fn populated_settings_retain_the_existing_status_display() {
    let fixture = Fixture::new(&["claude", "codex", "gemini"], true);
    let config = fixture.root.join("codex/config.toml");
    let contents = b"model='fixture-model'\nmodel_provider='fixture-provider'\nmodel_reasoning_effort='high'\napi_key='private-fixture-canary'\n";
    fs::write(&config, contents).unwrap();
    for command in ["agents", "ls"] {
        let output = fixture.run(&[command]);
        assert!(output.status.success());
        assert!(output.stderr.is_empty());
        assert_eq!(
            String::from_utf8(output.stdout).unwrap(),
            format!(
                "Claude Code\n  binary: {}\n  config: not checked\nGemini CLI\n  binary: {}\n  config: not checked\nCodex\n  binary: found ({})\n  config: {} (valid_toml)\n  model: fixture-model\n  provider: fixture-provider\n  effort: high\n",
                fixture.root.join("bin/claude").display(),
                fixture.root.join("bin/gemini").display(),
                fixture.root.join("bin/codex").display(),
                config.display(),
            )
        );
    }
    assert_eq!(fs::read(&config).unwrap(), contents);
    assert!(!fixture.root.join("called").exists());
}

#[test]
fn existing_claude_gemini_or_codex_account_state_stays_outside_the_new_slice() {
    for relative in ["home/.claude", "home/.gemini", "codex"] {
        let fixture = Fixture::new(&["claude", "codex", "gemini"], false);
        let directory = fixture.root.join(relative);
        fs::create_dir(&directory).unwrap();
        let canary = directory.join("synthetic-account");
        fs::write(&canary, b"private-fixture-canary").unwrap();
        let output = fixture.run(&["ls"]);
        assert!(output.status.success());
        assert!(output.stderr.is_empty());
        let stdout = String::from_utf8(output.stdout).unwrap();
        assert!(stdout.starts_with("Claude Code\n  binary: "));
        assert!(stdout.contains("Gemini CLI\n  binary: "));
        assert!(stdout.contains(" (missing)\n"));
        assert!(!stdout.contains("private-fixture-canary"));
        assert_eq!(fs::read(canary).unwrap(), b"private-fixture-canary");
        assert!(!fixture.root.join("called").exists());
    }
}

#[test]
fn unobserved_candidate_combinations_keep_the_previous_display() {
    for programs in [
        &["claude", "codex"][..],
        &["claude", "gemini"][..],
        &["codex", "gemini"][..],
        &["opencode"][..],
    ] {
        let fixture = Fixture::new(programs, false);
        for command in ["agents", "ls"] {
            let output = fixture.run(&[command]);
            assert!(output.status.success());
            assert!(output.stderr.is_empty());
            assert!(
                String::from_utf8(output.stdout)
                    .unwrap()
                    .contains("\n  binary: ")
            );
        }
        assert!(!fixture.root.join("called").exists());
    }
}

#[test]
fn candidates_do_not_change_json_order_or_schema() {
    let fixture = Fixture::new(&["claude", "gemini"], false);
    let output = fixture.run(&["agents", "--json"]);
    assert!(output.status.success());
    assert!(output.stderr.is_empty());
    assert_eq!(
        String::from_utf8(output.stdout).unwrap(),
        format!(
            "{{\"version\":\"0.2.0\",\"items\":[{{\"id\":\"claude\",\"name\":\"Claude Code\",\"installed\":true,\"binary_path\":\"{}\",\"config_path\":null,\"config_state\":\"not_checked\"}},{{\"id\":\"gemini\",\"name\":\"Gemini CLI\",\"installed\":true,\"binary_path\":\"{}\",\"config_path\":null,\"config_state\":\"not_checked\"}}]}}\n",
            fixture.root.join("bin/claude").display(),
            fixture.root.join("bin/gemini").display(),
        )
    );
    assert!(!fixture.root.join("called").exists());
}

#[test]
fn unobserved_home_resolution_and_agent_environment_keep_the_existing_display() {
    let fixture = Fixture::new(&["codex"], false);
    let mut commands = Vec::new();
    let mut default_home = fixture.command(&["ls"]);
    default_home.env_remove("CODEX_HOME");
    commands.push(default_home);
    let mut relative_home = fixture.command(&["ls"]);
    relative_home.env("CODEX_HOME", "relative-codex");
    commands.push(relative_home);
    for key in ["CLAUDE_CONFIG_DIR", "GEMINI_API_KEY", "OPENAI_API_KEY"] {
        let mut overridden = fixture.command(&["ls"]);
        overridden.env(key, "synthetic-fixture-value");
        commands.push(overridden);
    }
    for mut command in commands {
        let output = command.output().unwrap();
        assert!(output.status.success());
        assert!(output.stderr.is_empty());
        let stdout = String::from_utf8(output.stdout).unwrap();
        assert!(stdout.starts_with("Codex\n  binary: found ("));
        assert!(stdout.contains(" (missing)\n"));
        assert!(!stdout.contains("synthetic-fixture-value"));
    }
}
