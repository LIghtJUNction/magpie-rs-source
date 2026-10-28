use std::{
    fs,
    path::PathBuf,
    process::{Command, Output},
    sync::atomic::{AtomicU64, Ordering},
};

static NEXT: AtomicU64 = AtomicU64::new(0);

struct Fixture {
    root: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let root = std::env::temp_dir().join(format!(
            "magpie-legacy-cli-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&root).unwrap();
        fs::create_dir(root.join("bin")).unwrap();
        fs::create_dir(root.join("home")).unwrap();
        Self { root }
    }

    fn run(&self, arguments: &[&str]) -> Output {
        Command::new(env!("CARGO_BIN_EXE_magpie"))
            .args(arguments)
            .env_clear()
            .env("PATH", self.root.join("bin"))
            .env("HOME", self.root.join("home"))
            .env("CODEX_HOME", self.root.join("codex"))
            .current_dir(&self.root)
            .output()
            .unwrap()
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.root).unwrap();
    }
}

#[test]
fn empty_agents_and_ls_match_observed_original_streams() {
    let fixture = Fixture::new();
    let agents = fixture.run(&["agents"]);
    assert!(agents.status.success());
    assert!(agents.stderr.is_empty());
    assert_eq!(
        agents.stdout,
        include_bytes!("fixtures/legacy_agent_display/agents-empty.stdout")
    );

    let list = fixture.run(&["ls"]);
    assert_eq!(list.status.code(), Some(1));
    assert!(list.stdout.is_empty());
    assert_eq!(
        list.stderr,
        b"magpie: no supported agents found on this machine\n"
    );

    let json = fixture.run(&["agents", "--json"]);
    assert!(json.status.success());
    assert!(json.stderr.is_empty());
    assert_eq!(json.stdout, b"{\"version\":\"0.2.0\",\"items\":[]}\n");
}

#[test]
fn codex_directory_is_a_state_hint_not_an_installed_executable() {
    let fixture = Fixture::new();
    fs::create_dir(fixture.root.join("codex")).unwrap();
    let list = fixture.run(&["ls"]);
    assert!(list.status.success());
    assert!(list.stderr.is_empty());
    assert_eq!(
        String::from_utf8(list.stdout).unwrap(),
        format!(
            "  Codex  CLI not found  {}\n",
            fixture.root.join("codex/config.toml").display()
        )
    );
    assert!(!fixture.root.join("codex/config.toml").exists());

    let json = fixture.run(&["agents", "--json"]);
    assert!(json.status.success());
    assert_eq!(json.stdout, b"{\"version\":\"0.2.0\",\"items\":[]}\n");
}
