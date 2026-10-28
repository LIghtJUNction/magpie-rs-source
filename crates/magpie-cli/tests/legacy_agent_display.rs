//! Synthetic tests for the independently includable text slice; no CLI wiring.
//!
//! Golden files reproduce observations 43 and 15 from
//! 36-original-cli-behavior.md, including final newlines and trailing spaces.
//! Recorded stdout SHA-256:
//! empty: 8e01e78fb86e3fb8cd2493aeecccde601b3d898c3a9d2e1fd1589b97b854fa0f
//! codex directory: b80e0f4d6d53183bf12666ff1e5487a732d47c18bd4fd2a6ba5dde25ca2aab86
//! No process-global environment is changed; only explicit fixture paths are used.

#[path = "../src/legacy_agent_display.rs"]
mod legacy_agent_display;

use std::{
    env, fs, io,
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
    time::{SystemTime, UNIX_EPOCH},
};

use legacy_agent_display::{
    CodexDirectory, DisplayError, TextCommand, inspect_codex_directory,
    inspect_empty_candidate_state, render, render_unconfigured_candidates,
};
use magpie_core::agent_catalog::{
    Detection, DiscoveryInput, DiscoveryReport, ExecutableStyle, NotCheckedReason, ScanLimits,
    discover,
};

const EMPTY_AGENTS: &str = include_str!("fixtures/legacy_agent_display/agents-empty.stdout");
#[cfg(unix)]
const CODEX_DIRECTORY_AGENTS: &str =
    include_str!("fixtures/legacy_agent_display/agents-codex-directory.stdout");
#[cfg(unix)]
const CODEX_LS: &str = "  Codex  CLI not found  /fixture/codex/config.toml\n";
static NEXT: AtomicU64 = AtomicU64::new(0);

struct Fixture {
    root: PathBuf,
    bin: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("fixture clock")
            .as_nanos();
        let root = env::temp_dir().join(format!(
            "magpie-legacy-agent-display-{}-{nonce}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed),
        ));
        #[cfg(unix)]
        let builder = {
            use std::os::unix::fs::DirBuilderExt;
            let mut builder = fs::DirBuilder::new();
            builder.mode(0o700);
            builder
        };
        #[cfg(not(unix))]
        let builder = fs::DirBuilder::new();
        builder.create(&root).expect("create private fixture");
        let bin = root.join("bin");
        builder.create(&bin).expect("create empty synthetic PATH");
        Self { root, bin }
    }

    fn report(&self) -> DiscoveryReport {
        let path = env::join_paths([&self.bin]).expect("join fixture PATH");
        self.scan(Some(&path), ScanLimits::default())
    }

    fn scan(&self, path: Option<&std::ffi::OsStr>, limits: ScanLimits) -> DiscoveryReport {
        discover(DiscoveryInput {
            path,
            current_dir: &self.root,
            style: ExecutableStyle::native(),
            limits,
        })
        .expect("valid explicit discovery input")
    }

    fn executable(&self, name: &str) {
        let path = self.bin.join(format!("{name}{}", env::consts::EXE_SUFFIX));
        fs::write(&path, b"#!/bin/sh\nexit 97\n").expect("write synthetic candidate");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(path, fs::Permissions::from_mode(0o700))
                .expect("make synthetic candidate executable");
        }
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        if let Err(error) = fs::remove_dir_all(&self.root) {
            eprintln!("fixture cleanup {}: {error}", self.root.display());
        }
    }
}

fn observed_codex_directory() -> CodexDirectory {
    CodexDirectory::Present {
        config_path: PathBuf::from("/fixture/codex/config.toml"),
    }
}

#[test]
fn empty_agents_match_all_68_original_rows_byte_for_byte() {
    let fixture = Fixture::new();
    let output = render(
        TextCommand::Agents,
        &fixture.report(),
        &CodexDirectory::Missing,
    )
    .expect("observed empty state");
    assert_eq!(output.stdout, EMPTY_AGENTS);
    assert_eq!(output.stdout.lines().count(), 68);
    assert_eq!(output.exit_code, 0);
    assert!(output.stderr.is_empty());
    assert_eq!(output.unchecked_agent_ids.len(), 48);
    assert!(output.unchecked_agent_ids.contains(&"claude-desktop"));
    assert!(!output.unchecked_agent_ids.contains(&"codex"));
}

#[test]
fn empty_ls_keeps_stdout_empty_and_returns_the_original_error_and_exit_one() {
    let fixture = Fixture::new();
    let output = render(TextCommand::Ls, &fixture.report(), &CodexDirectory::Missing)
        .expect("observed no-positive-evidence state");
    assert!(output.stdout.is_empty());
    assert_eq!(
        output.stderr,
        "magpie: no supported agents found on this machine\n"
    );
    assert_eq!(output.exit_code, 1);
    assert_eq!(output.unchecked_agent_ids.len(), 48);
}

#[cfg(unix)]
#[test]
fn codex_directory_matches_the_original_agents_and_ls_streams() {
    let fixture = Fixture::new();
    let report = fixture.report();
    let codex = observed_codex_directory();
    for (command, expected) in [
        (TextCommand::Agents, CODEX_DIRECTORY_AGENTS),
        (TextCommand::Ls, CODEX_LS),
    ] {
        let output = render(command, &report, &codex).expect("directory-only state");
        assert_eq!(output.stdout, expected);
        assert!(output.stderr.is_empty());
        assert_eq!(output.exit_code, 0);
    }
}

#[test]
fn metadata_probe_distinguishes_missing_and_empty_directories_without_writing() {
    let fixture = Fixture::new();
    let home = fixture.root.join("codex");
    assert_eq!(
        inspect_codex_directory(&home).expect("missing directory"),
        CodexDirectory::Missing
    );
    assert!(!home.exists(), "inspection must not create the directory");
    fs::create_dir(&home).expect("create empty synthetic Codex directory");
    let codex = inspect_codex_directory(&home).expect("directory metadata");
    assert_eq!(
        codex,
        CodexDirectory::Present {
            config_path: home.join("config.toml")
        }
    );
    let output = render(TextCommand::Ls, &fixture.report(), &codex).expect("Codex hint");
    assert!(output.stdout.contains("CLI not found"));
    assert!(
        output
            .stdout
            .contains(&home.join("config.toml").display().to_string())
    );
    assert!(fs::read_dir(&home).unwrap().next().is_none());
}

#[test]
fn probe_does_not_parse_or_disclose_config_and_account_files() {
    let fixture = Fixture::new();
    let home = fixture.root.join("codex");
    fs::create_dir(&home).expect("create synthetic Codex directory");
    let secret = b"PRIVATE_FIXTURE_CANARY; invalid TOML; must not be parsed";
    let config = home.join("config.toml");
    let account = home.join("auth.json");
    fs::write(&config, secret).unwrap();
    fs::write(&account, secret).unwrap();
    let codex = inspect_codex_directory(&home).expect("directory metadata only");
    let output = render(TextCommand::Ls, &fixture.report(), &codex).expect("directory hint");
    assert!(!output.stdout.contains("PRIVATE_FIXTURE_CANARY"));
    assert!(output.stderr.is_empty());
    assert_eq!(fs::read(config).unwrap(), secret);
    assert_eq!(fs::read(account).unwrap(), secret);
}

#[test]
fn relative_and_non_directory_inputs_are_errors_not_missing_evidence() {
    assert_eq!(
        inspect_codex_directory(Path::new("codex"))
            .expect_err("relative path must not consult cwd")
            .kind(),
        io::ErrorKind::InvalidInput
    );
    let fixture = Fixture::new();
    let file = fixture.root.join("not-a-directory");
    fs::write(&file, b"synthetic").unwrap();
    assert_eq!(
        inspect_codex_directory(&file)
            .expect_err("not a direct directory")
            .kind(),
        io::ErrorKind::InvalidInput
    );
}

#[cfg(unix)]
#[test]
fn symlink_directory_and_metadata_errors_are_not_clean_absence() {
    use std::os::unix::fs::symlink;
    let fixture = Fixture::new();
    let link = fixture.root.join("link");
    symlink(&fixture.bin, &link).unwrap();
    assert_eq!(
        inspect_codex_directory(&link)
            .expect_err("do not follow final directory symlink")
            .kind(),
        io::ErrorKind::InvalidInput
    );
    let loop_path = fixture.root.join("loop");
    symlink("loop", &loop_path).unwrap();
    assert!(inspect_codex_directory(&loop_path.join("child")).is_err());
}

#[test]
fn absent_path_and_limits_are_incomplete_not_empty_ls() {
    let fixture = Fixture::new();
    let path = env::join_paths([&fixture.bin]).unwrap();
    for report in [
        fixture.scan(None, ScanLimits::default()),
        fixture.scan(
            Some(&path),
            ScanLimits {
                path_entries: 0,
                ..ScanLimits::default()
            },
        ),
    ] {
        for command in [TextCommand::Agents, TextCommand::Ls] {
            assert_eq!(
                render(command, &report, &CodexDirectory::Missing),
                Err(DisplayError::IncompleteDiscovery)
            );
        }
    }
}

#[cfg(unix)]
#[test]
fn a_candidate_after_a_metadata_failure_does_not_hide_incomplete_discovery() {
    use std::os::unix::fs::symlink;
    let fixture = Fixture::new();
    let first = fixture.root.join("first");
    fs::create_dir(&first).unwrap();
    symlink("codex", first.join("codex")).unwrap();
    fixture.executable("codex");
    let path = env::join_paths([&first, &fixture.bin]).unwrap();
    let report = fixture.scan(Some(&path), ScanLimits::default());
    assert!(!report.path_scan_complete());
    for command in [TextCommand::Agents, TextCommand::Ls] {
        assert_eq!(
            render(command, &report, &CodexDirectory::Missing),
            Err(DisplayError::IncompleteDiscovery)
        );
    }
}

#[test]
fn unchecked_directory_and_uncertain_detection_are_not_absence() {
    let fixture = Fixture::new();
    let mut report = fixture.report();
    assert_eq!(
        render(TextCommand::Ls, &report, &CodexDirectory::NotChecked),
        Err(DisplayError::IncompleteDiscovery)
    );
    report.agents[0].detection = Detection::Inconclusive;
    assert_eq!(
        render(TextCommand::Ls, &report, &CodexDirectory::Missing),
        Err(DisplayError::IncompleteDiscovery)
    );
}

#[test]
fn directory_hint_does_not_claim_cli_absence_when_codex_rule_is_unchecked() {
    let fixture = Fixture::new();
    let mut report = fixture.report();
    report
        .agents
        .iter_mut()
        .find(|agent| agent.definition.id == "codex")
        .unwrap()
        .detection = Detection::NotChecked(NotCheckedReason::UnknownRule);
    assert_eq!(
        render(TextCommand::Ls, &report, &observed_codex_directory()),
        Err(DisplayError::IncompleteDiscovery)
    );
}

#[test]
fn missing_duplicate_and_extra_catalog_entries_are_rejected() {
    let fixture = Fixture::new();
    let full = fixture.report();
    let mut missing = full.clone();
    missing.agents.pop();
    let mut duplicate = full.clone();
    duplicate.agents[1] = duplicate.agents[0].clone();
    let mut extra = full.clone();
    extra.agents.push(extra.agents[0].clone());
    for report in [missing, duplicate, extra] {
        assert_eq!(
            render(TextCommand::Agents, &report, &CodexDirectory::Missing),
            Err(DisplayError::CatalogMismatch)
        );
    }
}

#[test]
fn report_order_does_not_change_the_observed_presentation_order() {
    let fixture = Fixture::new();
    let mut report = fixture.report();
    report.agents.reverse();
    let output = render(TextCommand::Agents, &report, &CodexDirectory::Missing).unwrap();
    assert_eq!(output.stdout, EMPTY_AGENTS);
}

#[test]
fn executable_and_editor_host_candidates_do_not_invent_populated_original_output() {
    for (program, agent_id) in [("codex", "codex"), ("code", "vscode-chat")] {
        let fixture = Fixture::new();
        fixture.executable(program);
        let report = fixture.report();
        assert_eq!(
            render(TextCommand::Ls, &report, &CodexDirectory::Missing),
            Err(DisplayError::UnobservedCandidate { agent_id })
        );
    }
}

#[test]
fn ambiguous_names_are_not_an_empty_success_or_no_agents_error() {
    let fixture = Fixture::new();
    fixture.executable("pi");
    assert_eq!(
        render(TextCommand::Ls, &fixture.report(), &CodexDirectory::Missing),
        Err(DisplayError::UnverifiedCandidate { agent_id: "pi" })
    );
}

#[cfg(unix)]
#[test]
fn candidate_programs_are_never_executed() {
    use std::os::unix::fs::PermissionsExt;
    let fixture = Fixture::new();
    let marker = fixture.root.join("candidate-executed");
    let candidate = fixture.bin.join("codex");
    fs::write(
        &candidate,
        format!("#!/bin/sh\nprintf executed > '{}'\n", marker.display()),
    )
    .unwrap();
    fs::set_permissions(candidate, fs::Permissions::from_mode(0o700)).unwrap();
    assert_eq!(
        render(TextCommand::Ls, &fixture.report(), &CodexDirectory::Missing),
        Err(DisplayError::UnobservedCandidate { agent_id: "codex" })
    );
    assert!(!marker.exists());
}

#[cfg(unix)]
#[test]
fn paths_with_terminal_controls_non_utf8_or_relative_bases_are_rejected() {
    use std::ffi::OsString;
    use std::os::unix::ffi::OsStringExt;
    let fixture = Fixture::new();
    let report = fixture.report();
    for config_path in [
        PathBuf::from("/fixture/codex\ninjected/config.toml"),
        PathBuf::from(OsString::from_vec(b"/fixture/\xff/config.toml".to_vec())),
        PathBuf::from("relative/config.toml"),
    ] {
        assert_eq!(
            render(
                TextCommand::Ls,
                &report,
                &CodexDirectory::Present { config_path }
            ),
            Err(DisplayError::UnrepresentableConfigPath)
        );
    }
}

#[test]
fn errors_are_explicit_slice_diagnostics_not_the_original_empty_message() {
    let error = DisplayError::IncompleteDiscovery;
    let diagnostic = error.to_string();
    assert!(diagnostic.contains("incomplete"));
    assert!(!diagnostic.contains("no supported agents found"));
    let _: &dyn std::error::Error = &error;
}

#[cfg(unix)]
#[test]
fn candidate_renderer_matches_39_original_bytes_without_path_substitution() {
    for (programs, directory, scenario) in [
        (&["claude"][..], CodexDirectory::Missing, "claude-only"),
        (&["codex"][..], CodexDirectory::Missing, "codex-only"),
        (&["gemini"][..], CodexDirectory::Missing, "gemini-only"),
        (
            &["claude", "codex", "gemini"][..],
            CodexDirectory::Missing,
            "all-three",
        ),
        (&["codex"][..], observed_codex_directory(), "codex-only"),
        (
            &["claude", "codex", "gemini"][..],
            observed_codex_directory(),
            "all-three",
        ),
    ] {
        let fixture = Fixture::new();
        for program in programs {
            fixture.executable(program);
        }
        let report = fixture.report();
        for (command, name) in [(TextCommand::Agents, "agents"), (TextCommand::Ls, "ls")] {
            let output = render_unconfigured_candidates(
                command,
                &report,
                &directory,
                Path::new("/fixture/codex/config.toml"),
            )
            .unwrap();
            let expected = fs::read(
                Path::new(env!("CARGO_MANIFEST_DIR"))
                    .join("tests/fixtures/legacy_agent_candidates")
                    .join(format!("{scenario}-{name}.stdout")),
            )
            .unwrap();
            assert_eq!(output.stdout.as_bytes(), expected, "{scenario} {name}");
            assert_eq!(output.exit_code, 0);
            assert!(output.stderr.is_empty());
            assert_eq!(output.unchecked_agent_ids.len(), 48);
        }
    }
}

#[test]
fn empty_candidate_inspection_requires_absent_or_explicitly_empty_state() {
    let fixture = Fixture::new();
    let home = fixture.root.join("home");
    fs::create_dir(&home).unwrap();
    let codex = fixture.root.join("codex");
    assert!(inspect_empty_candidate_state(&home, &codex).unwrap());
    fs::create_dir(&codex).unwrap();
    assert!(inspect_empty_candidate_state(&home, &codex).unwrap());
    let account = codex.join("auth.json");
    fs::write(&account, b"PRIVATE_SYNTHETIC_ACCOUNT").unwrap();
    assert!(!inspect_empty_candidate_state(&home, &codex).unwrap());
    assert_eq!(fs::read(&account).unwrap(), b"PRIVATE_SYNTHETIC_ACCOUNT");
    fs::remove_file(&account).unwrap();
    for name in [".claude", ".gemini"] {
        let path = home.join(name);
        fs::create_dir(&path).unwrap();
        assert!(!inspect_empty_candidate_state(&home, &codex).unwrap());
        fs::remove_dir(&path).unwrap();
    }
    assert!(inspect_empty_candidate_state(Path::new("relative"), &codex).is_err());
}

#[cfg(unix)]
#[test]
fn new_candidate_slice_does_not_accept_uncertain_or_unobserved_states() {
    let fixture = Fixture::new();
    fixture.executable("codex");
    let path = Path::new("/fixture/codex/config.toml");
    let report = fixture.report();
    for command in [TextCommand::Agents, TextCommand::Ls] {
        assert_eq!(
            render_unconfigured_candidates(command, &report, &CodexDirectory::NotChecked, path),
            Err(DisplayError::IncompleteDiscovery)
        );
        let mut uncertain = report.clone();
        uncertain
            .agents
            .iter_mut()
            .find(|agent| agent.definition.id == "codex")
            .unwrap()
            .detection = Detection::Inconclusive;
        assert_eq!(
            render_unconfigured_candidates(command, &uncertain, &CodexDirectory::Missing, path),
            Err(DisplayError::IncompleteDiscovery)
        );
        assert_eq!(
            render_unconfigured_candidates(
                command,
                &report,
                &CodexDirectory::Missing,
                Path::new("/fixture/codex\ncontrol/config.toml")
            ),
            Err(DisplayError::UnrepresentableConfigPath)
        );
    }
    fixture.executable("claude");
    assert_eq!(
        render_unconfigured_candidates(
            TextCommand::Ls,
            &fixture.report(),
            &CodexDirectory::Missing,
            path
        ),
        Err(DisplayError::UnobservedCombination)
    );
}
