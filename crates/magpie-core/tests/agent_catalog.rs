//! Synthetic file-only tests; no process environment or account configuration changes.

use std::{
    collections::BTreeSet,
    env,
    ffi::OsString,
    fs,
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
    time::{SystemTime, UNIX_EPOCH},
};

use magpie_core::agent_catalog::{
    CATALOG, ConfigurationSupport, Detection, DirectoryState, DiscoveryInput, DiscoveryReport,
    DiscoveryRule, ExecutableStyle, IdOrigin, IntegrationContract, LEGACY_AGENT_IDS,
    NotCheckedReason, ProgramRole, ScanLimits, discover, get,
};
use magpie_core::agents::{self, InstalledAgent, discover_installed_agents_with};

static NEXT: AtomicU64 = AtomicU64::new(0);

// Independent expectations keep aliases and host-only detections from being
// accidentally validated by fixtures derived solely from the implementation.
const VERIFIED_PATH_RULES: &[(&str, &[&str], ProgramRole)] = &[
    ("claude", &["claude"], ProgramRole::Agent),
    ("codex", &["codex"], ProgramRole::Agent),
    ("gemini", &["gemini"], ProgramRole::Agent),
    ("opencode", &["opencode"], ProgramRole::Agent),
    ("pi", &["pi"], ProgramRole::Agent),
    ("oh-my-pi", &["omp"], ProgramRole::Agent),
    ("goose", &["goose"], ProgramRole::Agent),
    ("cursor", &["agent", "cursor-agent"], ProgramRole::Agent),
    ("zed", &["zed"], ProgramRole::EditorHost),
    ("vscode-chat", &["code"], ProgramRole::EditorHost),
    (
        "vscode-insiders-chat",
        &["code-insiders"],
        ProgramRole::EditorHost,
    ),
    ("vscodium-chat", &["codium"], ProgramRole::EditorHost),
    ("copilot", &["copilot"], ProgramRole::Agent),
    ("crush", &["crush"], ProgramRole::Agent),
    (
        "hermes-agent",
        &["hermes", "hermes-agent"],
        ProgramRole::Agent,
    ),
    ("kimi-code", &["kimi"], ProgramRole::Agent),
    ("qwen-code", &["qwen"], ProgramRole::Agent),
    ("droid", &["droid"], ProgramRole::Agent),
    ("cline-cli", &["cline"], ProgramRole::Agent),
    ("qoder-cli", &["qoder", "qodercli"], ProgramRole::Agent),
];

const AMBIGUOUS_NAMES: &[(&str, &str)] = &[
    ("cursor", "agent"),
    ("hermes-agent", "hermes"),
    ("pi", "pi"),
    ("oh-my-pi", "omp"),
    ("goose", "goose"),
    ("crush", "crush"),
];

struct Fixture(PathBuf);

impl Fixture {
    fn new() -> Self {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("fixture clock")
            .as_nanos();
        let root = env::temp_dir().join(format!(
            "magpie-agent-catalog-{}-{nonce}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed),
        ));
        fs::create_dir(&root).expect("create unique synthetic fixture");
        Self(root)
    }

    fn directory(&self, name: &str) -> PathBuf {
        let path = self.0.join(name);
        fs::create_dir(&path).expect("create fixture PATH directory");
        path
    }

    fn file(directory: &Path, name: &str) -> PathBuf {
        let path = directory.join(name);
        fs::write(&path, b"synthetic file; never execute\n").expect("write synthetic candidate");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&path, fs::Permissions::from_mode(0o755))
                .expect("set fixture mode");
        }
        path
    }

    fn candidate(directory: &Path, program: &str) -> PathBuf {
        Self::file(
            directory,
            &format!("{program}{}", if cfg!(windows) { ".exe" } else { "" }),
        )
    }

    fn scan(&self, directories: &[&Path]) -> DiscoveryReport {
        let path = env::join_paths(directories).expect("join synthetic PATH");
        self.scan_path(
            Some(&path),
            ExecutableStyle::native(),
            ScanLimits::default(),
        )
    }

    fn scan_path(
        &self,
        path: Option<&std::ffi::OsStr>,
        style: ExecutableStyle,
        limits: ScanLimits,
    ) -> DiscoveryReport {
        discover(DiscoveryInput {
            path,
            current_dir: &self.0,
            style,
            limits,
        })
        .expect("valid synthetic discovery input")
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        if let Err(error) = fs::remove_dir_all(&self.0) {
            eprintln!("fixture cleanup {}: {error}", self.0.display());
        }
    }
}

fn detection<'a>(report: &'a DiscoveryReport, id: &str) -> &'a Detection {
    &report
        .agents
        .iter()
        .find(|result| result.definition.id == id)
        .expect("catalog identity in report")
        .detection
}

#[test]
fn all_68_names_and_ids_are_frozen_and_unique() {
    let expected = [
        ("claude", "Claude Code"),
        ("claude-desktop", "Claude Desktop"),
        ("claude-science", "Claude Science"),
        ("codex", "Codex"),
        ("gemini", "Gemini CLI"),
        ("antigravity-cli", "Antigravity CLI"),
        ("opencode", "OpenCode"),
        ("openchamber", "OpenChamber"),
        ("mimocode", "MiMo Code"),
        ("deveco", "DevEco Code"),
        ("pi", "Pi"),
        ("oh-my-pi", "oh-my-pi"),
        ("stepcode", "StepCode"),
        ("mothx", "MothX"),
        ("penguin-harness", "PenguinHarness"),
        ("aside", "Aside"),
        ("omo", "OmO"),
        ("prime-agent", "Prime Agent"),
        ("goose", "Goose"),
        ("cursor", "Cursor CLI"),
        ("cursor-private-inference", "Cursor Private Inference"),
        ("zed", "Zed"),
        ("vscode-chat", "VS Code Chat"),
        ("vscode-insiders-chat", "VS Code Insiders Chat"),
        ("vscodium-chat", "VSCodium Chat"),
        ("zoo-code", "Zoo Code"),
        ("jetbrains-air", "JetBrains Air"),
        ("copilot-jetbrains", "Copilot (JetBrains)"),
        ("copilot", "Copilot CLI"),
        ("crush", "Crush"),
        ("deepseek-harness", "DeepSeek Harness"),
        ("reasonix-studio", "Reasonix Studio"),
        ("command-code", "Command Code"),
        ("fx", "fx"),
        ("devin", "Devin"),
        ("hermes-agent", "Hermes Agent"),
        ("mister-morph", "Mister Morph"),
        ("kimi-code", "Kimi Code"),
        ("jcode", "jcode"),
        ("qwen-code", "Qwen Code"),
        ("muse-code", "Muse Code"),
        ("empryo", "Empryo"),
        ("ante", "Ante"),
        ("astrbot", "AstrBot"),
        ("minimax-code", "MiniMax Code"),
        ("droid", "Droid (Factory)"),
        ("cline-cli", "Cline CLI"),
        ("qoder-cli", "Qoder CLI"),
        ("qoder-cn-cli", "Qoder CN CLI"),
        ("grok-build", "Grok Build"),
        ("grok-app", "Grok App"),
        ("zcode", "ZCode"),
        ("workbuddy", "WorkBuddy"),
        ("workbuddy-ai", "WorkBuddy AI"),
        ("codebuddy-code", "CodeBuddy Code"),
        ("raycast", "Raycast"),
        ("kiro-crew", "Kiro Crew"),
        ("openpencil", "OpenPencil"),
        ("newmax", "NewMax"),
        ("t3-code", "T3 Code"),
        ("openhanako", "OpenHanako"),
        ("atomcode", "AtomCode"),
        ("snow-cli", "Snow CLI"),
        ("alma", "Alma"),
        ("pencil", "Pencil"),
        ("cindy", "Cindy"),
        ("lody", "Lody"),
        ("aionui", "AionUi"),
    ];
    let actual: Vec<_> = CATALOG.iter().map(|agent| (agent.id, agent.name)).collect();
    assert_eq!(actual, expected, "minimum requirement catalog/order");
    assert_eq!(
        CATALOG
            .iter()
            .map(|agent| agent.id)
            .collect::<BTreeSet<_>>()
            .len(),
        68
    );
    assert!(get("absent-agent").is_none());
    for id in LEGACY_AGENT_IDS {
        assert_eq!(
            get(id).expect("legacy ID retained").id_origin,
            IdOrigin::ExistingRust
        );
    }
}

#[test]
fn declared_contracts_never_grant_configuration_support() {
    let implemented: Vec<_> = CATALOG
        .iter()
        .filter(|agent| agent.configuration != ConfigurationSupport::Unimplemented)
        .map(|agent| agent.id)
        .collect();
    assert_eq!(implemented, ["codex"]);
    assert_eq!(
        get("codex").expect("Codex").configuration,
        ConfigurationSupport::ExistingCodexPartial
    );
    assert_eq!(
        get("alma").expect("Alma").contract,
        IntegrationContract::LocalApi
    );
    for id in ["pencil", "cindy", "lody", "aionui"] {
        assert_eq!(
            get(id).expect("import agent").contract,
            IntegrationContract::ConfirmedImport
        );
    }
    for id in [
        "claude-science",
        "antigravity-cli",
        "cursor-private-inference",
    ] {
        assert_eq!(
            get(id).expect("unknown contract").contract,
            IntegrationContract::Unknown
        );
    }
}

#[test]
fn verified_program_names_and_roles_match_independent_expectations() {
    let actual: Vec<_> = CATALOG
        .iter()
        .filter_map(|agent| match agent.discovery {
            DiscoveryRule::Path(rule) => {
                assert!(
                    !rule.reference.is_empty(),
                    "missing command evidence for {}",
                    agent.id
                );
                for program in rule.programs {
                    assert!(!program.is_empty());
                    assert!(
                        Path::new(program)
                            .file_name()
                            .is_some_and(|name| name == *program)
                    );
                    assert!(
                        !program.contains(['/', '\\']),
                        "a command must be one filename"
                    );
                }
                Some((agent.id, rule.programs, rule.role))
            }
            DiscoveryRule::Unknown => None,
        })
        .collect();
    assert_eq!(actual, VERIFIED_PATH_RULES);
}

#[test]
fn each_documented_alias_is_discoverable_without_its_preferred_name() {
    for &(id, programs, role) in VERIFIED_PATH_RULES {
        for program in programs.iter().copied() {
            let fixture = Fixture::new();
            let directory = fixture.directory("bin");
            let candidate = Fixture::candidate(&directory, program);
            let report = fixture.scan(&[&directory]);
            if AMBIGUOUS_NAMES.contains(&(id, program)) {
                let Detection::AmbiguousExecutable { evidence, reason } = detection(&report, id)
                else {
                    panic!("{id}/{program}: ambiguous names cannot establish product identity");
                };
                assert_eq!(evidence.path, candidate);
                assert!(!reason.is_empty());
                continue;
            }
            let evidence = match (role, detection(&report, id)) {
                (ProgramRole::Agent, Detection::ExecutableFound(evidence))
                | (ProgramRole::EditorHost, Detection::EditorHostFound(evidence)) => evidence,
                (_, actual) => panic!("{id}/{program}: unexpected result {actual:?}"),
            };
            assert_eq!(evidence.program, program);
            assert_eq!(evidence.path, candidate);
        }
    }
}

#[test]
fn names_in_the_requirement_catalog_do_not_become_guessed_commands() {
    let fixture = Fixture::new();
    let directory = fixture.directory("bin");
    for agent in &CATALOG {
        if agent.discovery == DiscoveryRule::Unknown {
            Fixture::candidate(&directory, agent.id);
        }
    }
    let report = fixture.scan(&[&directory]);
    assert_eq!(report.executable_candidates().count(), 0);
    for agent in &CATALOG {
        if agent.discovery == DiscoveryRule::Unknown {
            assert_eq!(
                *detection(&report, agent.id),
                Detection::NotChecked(NotCheckedReason::UnknownRule)
            );
        }
    }
}

#[test]
fn all_verified_candidates_are_found_and_unknown_rules_remain_unchecked() {
    let fixture = Fixture::new();
    let directory = fixture.directory("all");
    let expected_agents = [
        "claude",
        "codex",
        "gemini",
        "opencode",
        "cursor",
        "copilot",
        "hermes-agent",
        "kimi-code",
        "qwen-code",
        "droid",
        "cline-cli",
        "qoder-cli",
    ];
    let expected_hosts = [
        "zed",
        "vscode-chat",
        "vscode-insiders-chat",
        "vscodium-chat",
    ];
    for id in expected_agents.into_iter().chain(expected_hosts) {
        let DiscoveryRule::Path(rule) = get(id).expect("known candidate").discovery else {
            panic!("verified candidate {id} needs a rule");
        };
        let program = rule
            .programs
            .iter()
            .copied()
            .find(|&name| !AMBIGUOUS_NAMES.contains(&(id, name)))
            .expect("unambiguous candidate");
        Fixture::candidate(&directory, program);
    }
    let report = fixture.scan(&[&directory]);
    assert_eq!(
        report.agents.len(),
        68,
        "registration remains separate from detection"
    );
    assert_eq!(
        report
            .executable_candidates()
            .map(|(agent, _)| agent.id)
            .collect::<Vec<_>>(),
        expected_agents
    );
    for id in expected_hosts {
        assert!(
            matches!(detection(&report, id), Detection::EditorHostFound(_)),
            "{id}"
        );
    }
    for result in &report.agents {
        if result.definition.discovery == DiscoveryRule::Unknown {
            assert_eq!(
                result.detection,
                Detection::NotChecked(NotCheckedReason::UnknownRule)
            );
        }
    }
    assert_eq!(report.stats.directory_metadata_calls, 1);
    assert_eq!(report.stats.candidate_errors, 0);
    assert!(report.issues.is_empty());
}

#[test]
fn partial_path_can_migrate_the_five_legacy_results_without_loss() {
    let fixture = Fixture::new();
    let first = fixture.directory("first");
    let second = fixture.directory("second");
    let mut expected = Vec::new();
    for (index, id) in LEGACY_AGENT_IDS.iter().enumerate() {
        let directory = if index % 2 == 0 { &first } else { &second };
        expected.push((
            *id,
            get(id).expect("legacy agent").name,
            Fixture::candidate(directory, id),
        ));
    }
    let report = fixture.scan(&[&first, &second]);
    let migrated: Vec<_> = LEGACY_AGENT_IDS
        .into_iter()
        .filter_map(|id| match detection(&report, id) {
            Detection::ExecutableFound(evidence) | Detection::EditorHostFound(evidence) => {
                Some((id, get(id).expect("legacy").name, evidence.path.clone()))
            }
            _ => None,
        })
        .collect();
    assert_eq!(
        migrated, expected,
        "old names, identities, selected files and order"
    );
    assert_eq!(report.executable_candidates().count(), 4);
}

#[test]
fn empty_directory_and_missing_directory_are_clean_path_misses() {
    let fixture = Fixture::new();
    let empty = fixture.directory("empty");
    let missing = fixture.0.join("missing");
    let report = fixture.scan(&[&missing, &empty]);
    assert_eq!(report.directories[0].state, DirectoryState::Missing);
    assert_eq!(report.directories[1].state, DirectoryState::Ready);
    assert_eq!(*detection(&report, "codex"), Detection::NotFoundOnPath);
    assert_eq!(report.executable_candidates().count(), 0);
    assert!(report.issues.is_empty());
}

#[test]
fn absent_path_is_not_an_empty_path_entry() {
    let fixture = Fixture::new();
    let codex = Fixture::candidate(&fixture.0, "codex");
    let absent = fixture.scan_path(None, ExecutableStyle::native(), ScanLimits::default());
    assert_eq!(
        *detection(&absent, "codex"),
        Detection::NotChecked(NotCheckedReason::PathNotProvided)
    );
    assert_eq!(absent.stats.candidate_metadata_calls, 0);
    let empty = fixture.scan_path(
        Some(std::ffi::OsStr::new("")),
        ExecutableStyle::native(),
        ScanLimits::default(),
    );
    let Detection::ExecutableFound(evidence) = detection(&empty, "codex") else {
        panic!("empty PATH entry means explicit cwd")
    };
    assert_eq!(evidence.path, codex);
}

#[test]
fn duplicates_are_recorded_without_reprobing_and_first_path_wins() {
    let fixture = Fixture::new();
    let first = fixture.directory("first");
    let second = fixture.directory("second");
    let chosen = Fixture::candidate(&first, "codex");
    Fixture::candidate(&second, "codex");
    let report = fixture.scan(&[&first, &first, &second]);
    let unique = fixture.scan(&[&first, &second]);
    assert_eq!(report.directories[1].state, DirectoryState::DuplicateOf(0));
    assert_eq!(report.stats.directory_metadata_calls, 2);
    assert_eq!(
        report.stats.candidate_metadata_calls,
        unique.stats.candidate_metadata_calls
    );
    let Detection::ExecutableFound(evidence) = detection(&report, "codex") else {
        panic!("first candidate")
    };
    assert_eq!(evidence.path, chosen);
    assert_eq!(evidence.path_index, 0);
    assert_eq!(evidence.program, "codex");
    assert!(evidence.name_reference.contains("existing rule"));
}

#[test]
fn non_files_do_not_hide_later_valid_candidates() {
    let fixture = Fixture::new();
    let first = fixture.directory("first");
    let second = fixture.directory("second");
    let name = format!("codex{}", if cfg!(windows) { ".exe" } else { "" });
    fs::create_dir(first.join(name)).expect("directory masquerading as executable");
    let selected = Fixture::candidate(&second, "codex");
    let report = fixture.scan(&[&first, &second]);
    assert_eq!(report.stats.non_file_candidates, 1);
    let Detection::ExecutableFound(evidence) = detection(&report, "codex") else {
        panic!("later executable")
    };
    assert_eq!(evidence.path, selected);
    assert!(!evidence.earlier_uncertainty);
}

#[cfg(unix)]
#[test]
fn ordinary_non_executable_files_are_rejected() {
    use std::os::unix::fs::PermissionsExt;
    let fixture = Fixture::new();
    let directory = fixture.directory("bin");
    let file = Fixture::candidate(&directory, "codex");
    fs::set_permissions(file, fs::Permissions::from_mode(0o644))
        .expect("synthetic non-executable mode");
    let report = fixture.scan(&[&directory]);
    assert_eq!(*detection(&report, "codex"), Detection::NotFoundOnPath);
    assert_eq!(report.stats.non_executable_candidates, 1);
}

#[test]
fn catalog_order_is_stable_despite_reversed_file_creation_order() {
    let fixture = Fixture::new();
    let directory = fixture.directory("bin");
    for id in ["qoder-cli", "codex", "claude"] {
        let DiscoveryRule::Path(rule) = get(id).expect("known agent").discovery else {
            panic!("PATH rule")
        };
        Fixture::candidate(&directory, rule.programs[0]);
    }
    let first = fixture.scan(&[&directory]);
    let second = fixture.scan(&[&directory]);
    assert_eq!(first, second, "same explicit input has stable report/order");
    assert_eq!(
        first
            .executable_candidates()
            .map(|(agent, _)| agent.id)
            .collect::<Vec<_>>(),
        ["claude", "codex", "qoder-cli"]
    );
}

#[test]
fn windows_suffixes_and_aliases_obey_path_priority() {
    let fixture = Fixture::new();
    let first = fixture.directory("first");
    let second = fixture.directory("second");
    Fixture::file(&first, "qwen.cmd");
    Fixture::file(&second, "qwen.exe");
    let path = env::join_paths([&first, &second]).expect("synthetic PATH");
    let report = fixture.scan_path(Some(&path), ExecutableStyle::Windows, ScanLimits::default());
    let Detection::ExecutableFound(evidence) = detection(&report, "qwen-code") else {
        panic!("npm shim")
    };
    assert_eq!(evidence.path, first.join("qwen.cmd"));
    assert_eq!(evidence.suffix, ".cmd");
    Fixture::file(&first, "qwen");
    Fixture::file(&first, "qwen.exe");
    let report = fixture.scan_path(Some(&path), ExecutableStyle::Windows, ScanLimits::default());
    let Detection::ExecutableFound(evidence) = detection(&report, "qwen-code") else {
        panic!("exe priority")
    };
    assert_eq!(evidence.path, first.join("qwen.exe"));
    Fixture::file(&first, "cursor-agent.bat");
    Fixture::file(&second, "agent.exe");
    let report = fixture.scan_path(Some(&path), ExecutableStyle::Windows, ScanLimits::default());
    let Detection::ExecutableFound(evidence) = detection(&report, "cursor") else {
        panic!("alias")
    };
    assert_eq!(evidence.program, "cursor-agent");
    assert_eq!(evidence.path_index, 0);
}

#[test]
fn every_supported_windows_suffix_is_checked_without_executing_it() {
    for suffix in [".exe", "", ".cmd", ".bat", ".com"] {
        let fixture = Fixture::new();
        let directory = fixture.directory("bin");
        let path = Fixture::file(&directory, &format!("qwen{suffix}"));
        let search = env::join_paths([&directory]).expect("synthetic PATH");
        let report = fixture.scan_path(
            Some(&search),
            ExecutableStyle::Windows,
            ScanLimits::default(),
        );
        let Detection::ExecutableFound(evidence) = detection(&report, "qwen-code") else {
            panic!("suffix {suffix}")
        };
        assert_eq!(evidence.path, path);
        assert_eq!(evidence.suffix, suffix);
    }
}

#[test]
fn truncated_path_returns_inconclusive_instead_of_claiming_absence() {
    let fixture = Fixture::new();
    let empty = fixture.directory("empty");
    let beyond_limit = fixture.directory("beyond-limit");
    Fixture::candidate(&beyond_limit, "codex");
    let path = env::join_paths([&empty, &beyond_limit]).expect("synthetic PATH");
    let limits = ScanLimits {
        path_entries: 1,
        ..ScanLimits::default()
    };
    let report = fixture.scan_path(Some(&path), ExecutableStyle::native(), limits);
    assert!(report.path_entries_truncated);
    assert_eq!(*detection(&report, "codex"), Detection::Inconclusive);
    assert_eq!(report.stats.path_entries, 1);
    assert_eq!(report.stats.directory_metadata_calls, 1);
}

#[test]
fn truncation_after_a_hit_does_not_cast_doubt_on_its_path_priority() {
    let fixture = Fixture::new();
    let first = fixture.directory("first");
    let later = fixture.directory("later");
    let selected = Fixture::candidate(&first, "codex");
    let path = env::join_paths([&first, &later]).expect("synthetic PATH");
    let report = fixture.scan_path(
        Some(&path),
        ExecutableStyle::native(),
        ScanLimits {
            path_entries: 1,
            ..ScanLimits::default()
        },
    );
    let Detection::ExecutableFound(evidence) = detection(&report, "codex") else {
        panic!("the unscanned tail cannot precede this hit");
    };
    assert!(report.path_entries_truncated);
    assert_eq!(evidence.path, selected);
    assert!(!evidence.earlier_uncertainty);
}

#[test]
fn a_zero_entry_budget_does_not_claim_that_agents_are_absent() {
    let fixture = Fixture::new();
    let path = env::join_paths([&fixture.0]).expect("synthetic PATH");
    let report = fixture.scan_path(
        Some(&path),
        ExecutableStyle::native(),
        ScanLimits {
            path_entries: 0,
            ..ScanLimits::default()
        },
    );
    assert_eq!(*detection(&report, "codex"), Detection::Inconclusive);
    assert!(report.path_entries_truncated);
    assert!(report.directories.is_empty());
    assert_eq!(report.stats.directory_metadata_calls, 0);
    assert_eq!(report.stats.candidate_metadata_calls, 0);
}

#[cfg(not(unix))]
#[test]
fn unix_permission_checks_are_not_silently_accepted_on_other_hosts() {
    let fixture = Fixture::new();
    let path = env::join_paths([&fixture.0]).expect("synthetic PATH");
    let error = discover(DiscoveryInput {
        path: Some(&path),
        current_dir: &fixture.0,
        style: ExecutableStyle::Unix,
        limits: ScanLimits::default(),
    })
    .expect_err("Unix executable permissions need a Unix host");
    assert_eq!(error.kind(), std::io::ErrorKind::Unsupported);
}

#[cfg(windows)]
#[test]
fn drive_relative_entries_do_not_use_hidden_process_state() {
    let fixture = Fixture::new();
    let other_drive = ["C:bin", "D:bin"]
        .into_iter()
        .find(|entry| !fixture.0.join(entry).is_absolute())
        .expect("a drive-relative path requires more than one explicit base");
    let report = fixture.scan_path(
        Some(std::ffi::OsStr::new(other_drive)),
        ExecutableStyle::Windows,
        ScanLimits::default(),
    );
    assert_eq!(report.directories[0].state, DirectoryState::UnresolvedPath);
    assert_eq!(*detection(&report, "codex"), Detection::Inconclusive);
    assert_eq!(report.stats.directory_metadata_calls, 0);
    assert_eq!(report.stats.candidate_metadata_calls, 0);
}

#[test]
fn skipped_entries_only_affect_hits_that_follow_them() {
    let fixture = Fixture::new();
    let directory = fixture.directory("bin");
    let selected = Fixture::candidate(&directory, "codex");
    let oversized = OsString::from("x".repeat(directory.as_os_str().as_encoded_bytes().len() + 1));
    let limits = ScanLimits {
        entry_bytes: directory.as_os_str().as_encoded_bytes().len(),
        ..ScanLimits::default()
    };
    for (entries, earlier_uncertainty) in [
        ([oversized.as_os_str(), directory.as_os_str()], true),
        ([directory.as_os_str(), oversized.as_os_str()], false),
    ] {
        let path = env::join_paths(entries).expect("synthetic PATH");
        let report = fixture.scan_path(Some(&path), ExecutableStyle::native(), limits);
        let Detection::ExecutableFound(evidence) = detection(&report, "codex") else {
            panic!("valid entry remains eligible");
        };
        assert_eq!(evidence.path, selected);
        assert_eq!(evidence.earlier_uncertainty, earlier_uncertainty);
        assert_eq!(report.stats.directory_metadata_calls, 1);
    }
}

#[test]
fn too_long_entry_is_reported_without_retaining_or_probing_it() {
    let fixture = Fixture::new();
    let path = OsString::from("123456789");
    let limits = ScanLimits {
        entry_bytes: 8,
        ..ScanLimits::default()
    };
    let report = fixture.scan_path(Some(&path), ExecutableStyle::native(), limits);
    assert_eq!(report.directories[0].state, DirectoryState::EntryTooLong);
    assert!(report.directories[0].directory.as_os_str().is_empty());
    assert_eq!(*detection(&report, "codex"), Detection::Inconclusive);
    assert_eq!(report.stats.directory_metadata_calls, 0);
}

#[test]
fn oversized_path_and_relative_base_are_input_errors() {
    let fixture = Fixture::new();
    let input = DiscoveryInput {
        path: Some(std::ffi::OsStr::new("123456789")),
        current_dir: &fixture.0,
        style: ExecutableStyle::native(),
        limits: ScanLimits {
            path_bytes: 8,
            ..ScanLimits::default()
        },
    };
    assert_eq!(
        discover(input).expect_err("oversized PATH").kind(),
        std::io::ErrorKind::InvalidInput
    );
    assert_eq!(
        discover(DiscoveryInput {
            path: None,
            current_dir: Path::new("relative"),
            style: ExecutableStyle::native(),
            limits: ScanLimits::default(),
        })
        .expect_err("relative cwd")
        .kind(),
        std::io::ErrorKind::InvalidInput
    );
}

#[test]
fn relative_entries_are_resolved_only_against_explicit_base() {
    let fixture = Fixture::new();
    let bin = fixture.directory("bin");
    let path = Fixture::candidate(&bin, "codex");
    let report = fixture.scan_path(
        Some(std::ffi::OsStr::new("bin")),
        ExecutableStyle::native(),
        ScanLimits::default(),
    );
    let Detection::ExecutableFound(evidence) = detection(&report, "codex") else {
        panic!("relative entry")
    };
    assert_eq!(evidence.path, path);
}

#[test]
fn a_path_entry_that_is_a_file_is_recorded() {
    let fixture = Fixture::new();
    let file = Fixture::file(&fixture.0, "not-a-directory");
    let report = fixture.scan(&[&file]);
    assert_eq!(report.directories[0].state, DirectoryState::NotDirectory);
    assert_eq!(*detection(&report, "codex"), Detection::NotFoundOnPath);
    assert_eq!(report.stats.candidate_metadata_calls, 0);
}

#[cfg(unix)]
#[test]
fn symlink_candidates_and_broken_targets_preserve_old_discovery_semantics() {
    use std::os::unix::fs::symlink;
    let fixture = Fixture::new();
    let directory = fixture.directory("bin");
    let real = Fixture::file(&fixture.0, "real-codex");
    symlink(&real, directory.join("codex")).expect("symlink to executable");
    symlink(fixture.0.join("nonexistent"), directory.join("claude")).expect("broken symlink");
    let report = fixture.scan(&[&directory]);
    let Detection::ExecutableFound(evidence) = detection(&report, "codex") else {
        panic!("regular target")
    };
    assert_eq!(evidence.path, directory.join("codex"));
    assert_eq!(*detection(&report, "claude"), Detection::NotFoundOnPath);
}

#[cfg(unix)]
#[test]
fn candidate_io_errors_and_suppression_are_visible() {
    use std::os::unix::fs::symlink;
    let fixture = Fixture::new();
    let first = fixture.directory("first");
    let second = fixture.directory("second");
    for name in ["codex", "claude"] {
        symlink(name, first.join(name)).expect("deterministic synthetic symlink loop");
    }
    Fixture::candidate(&second, "codex");
    let path = env::join_paths([&first, &second]).expect("synthetic PATH");
    let report = fixture.scan_path(
        Some(&path),
        ExecutableStyle::native(),
        ScanLimits {
            issues: 1,
            ..ScanLimits::default()
        },
    );
    assert_eq!(*detection(&report, "claude"), Detection::Inconclusive);
    let Detection::ExecutableFound(evidence) = detection(&report, "codex") else {
        panic!("later candidate")
    };
    assert!(evidence.earlier_uncertainty);
    assert_eq!(report.stats.candidate_errors, 2);
    assert_eq!(report.issues.len(), 1);
    assert_eq!(report.stats.suppressed_issues, 1);
    assert_eq!(report.issues[0].agent_id, "claude");
    assert!(report.issues[0].error.raw_os_error.is_some());
}

#[cfg(unix)]
#[test]
fn directory_io_errors_are_distinct_from_missing_directories() {
    use std::os::unix::fs::symlink;
    let fixture = Fixture::new();
    let loop_path = fixture.0.join("loop");
    symlink("loop", &loop_path).expect("synthetic directory symlink loop");
    let report = fixture.scan(&[&loop_path]);
    assert!(matches!(
        report.directories[0].state,
        DirectoryState::Error(_)
    ));
    assert_eq!(*detection(&report, "codex"), Detection::Inconclusive);
    assert_eq!(report.stats.directory_metadata_calls, 1);
    assert_eq!(report.stats.candidate_metadata_calls, 0);
}

#[cfg(unix)]
#[test]
fn non_utf8_path_components_are_supported_without_lossy_conversion() {
    use std::os::unix::ffi::OsStringExt;
    let fixture = Fixture::new();
    let directory = fixture.0.join(OsString::from_vec(b"bin-\xff".to_vec()));
    fs::create_dir(&directory).expect("non-UTF8 synthetic directory");
    let file = Fixture::candidate(&directory, "codex");
    let report = fixture.scan(&[&directory]);
    let Detection::ExecutableFound(evidence) = detection(&report, "codex") else {
        panic!("non-UTF8 path")
    };
    assert_eq!(evidence.path, file);
}

fn installed_in(
    fixture: &Fixture,
    directories: &[&Path],
) -> magpie_core::agents::InstalledAgentDiscovery {
    let path = env::join_paths(directories).expect("synthetic PATH");
    discover_installed_agents_with(DiscoveryInput {
        path: Some(&path),
        current_dir: &fixture.0,
        style: ExecutableStyle::native(),
        limits: ScanLimits::default(),
    })
    .expect("valid synthetic PATH")
}

#[test]
fn actual_projection_preserves_five_legacy_rows_before_appending_new_candidates() {
    let fixture = Fixture::new();
    let first = fixture.directory("first");
    let second = fixture.directory("second");
    let mut expected = Vec::new();
    for (index, (id, name)) in [
        ("claude", "Claude Code"),
        ("gemini", "Gemini CLI"),
        ("opencode", "OpenCode"),
        ("codex", "Codex"),
        ("zed", "Zed"),
    ]
    .into_iter()
    .enumerate()
    {
        let directory = if index % 2 == 0 { &first } else { &second };
        expected.push(InstalledAgent {
            id,
            name,
            executable: Fixture::candidate(directory, id),
        });
        if index % 2 == 0 {
            Fixture::candidate(&second, id);
        }
    }
    for (id, name, program) in [
        ("cursor", "Cursor CLI", "cursor-agent"),
        ("copilot", "Copilot CLI", "copilot"),
        ("hermes-agent", "Hermes Agent", "hermes-agent"),
        ("kimi-code", "Kimi Code", "kimi"),
        ("qwen-code", "Qwen Code", "qwen"),
        ("droid", "Droid (Factory)", "droid"),
        ("cline-cli", "Cline CLI", "cline"),
        ("qoder-cli", "Qoder CLI", "qoder"),
    ] {
        expected.push(InstalledAgent {
            id,
            name,
            executable: Fixture::candidate(&first, program),
        });
    }
    let discovery = installed_in(&fixture, &[&first, &second]);
    assert_eq!(discovery.agents, expected);
    assert!(discovery.report.path_scan_complete());
    assert!(matches!(
        detection(&discovery.report, "zed"),
        Detection::EditorHostFound(_)
    ));
}

#[test]
fn ambiguous_products_editor_hosts_and_unknown_ids_are_not_installed_agents() {
    let fixture = Fixture::new();
    let directory = fixture.directory("bin");
    for (_, name) in AMBIGUOUS_NAMES {
        Fixture::candidate(&directory, name);
    }
    for name in [
        "code",
        "code-insiders",
        "codium",
        "claude-science",
        "aside",
        "cursor",
        "qwen-code",
    ] {
        Fixture::candidate(&directory, name);
    }
    let discovery = installed_in(&fixture, &[&directory]);
    assert!(discovery.agents.is_empty());
    assert_eq!(discovery.report.stats.ambiguous_candidates, 6);
    for (id, _) in AMBIGUOUS_NAMES {
        assert!(matches!(
            detection(&discovery.report, id),
            Detection::AmbiguousExecutable { .. }
        ));
    }
    assert!(
        discovery.report.path_scan_complete(),
        "all names checked, product identities still unresolved"
    );
}

#[test]
fn distinctive_aliases_remain_visible_after_generic_unrelated_launchers() {
    let fixture = Fixture::new();
    let unrelated = fixture.directory("unrelated");
    let actual = fixture.directory("actual");
    Fixture::candidate(&unrelated, "agent");
    Fixture::candidate(&unrelated, "hermes");
    let cursor = Fixture::candidate(&actual, "cursor-agent");
    let hermes = Fixture::candidate(&actual, "hermes-agent");
    let discovery = installed_in(&fixture, &[&unrelated, &actual]);
    assert_eq!(
        discovery
            .agents
            .iter()
            .map(|agent| (agent.id, &agent.executable))
            .collect::<Vec<_>>(),
        [("cursor", &cursor), ("hermes-agent", &hermes)]
    );
    assert_eq!(discovery.report.stats.ambiguous_candidates, 2);
}

#[test]
fn legacy_windows_names_keep_the_old_exe_and_bare_priority() {
    let fixture = Fixture::new();
    let first = fixture.directory("first");
    let second = fixture.directory("second");
    for id in LEGACY_AGENT_IDS {
        for suffix in [".cmd", ".bat", ".com"] {
            Fixture::file(&first, &format!("{id}{suffix}"));
        }
        Fixture::file(&second, &format!("{id}.exe"));
    }
    let shim = Fixture::file(&first, "qwen.cmd");
    Fixture::file(&second, "qwen.exe");
    let path = env::join_paths([&first, &second]).expect("synthetic Windows names");
    let input = DiscoveryInput {
        path: Some(&path),
        current_dir: &fixture.0,
        style: ExecutableStyle::Windows,
        limits: ScanLimits::default(),
    };
    let discovery = discover_installed_agents_with(input).expect("valid Windows filename policy");
    for (agent, id) in discovery.agents.iter().zip(LEGACY_AGENT_IDS) {
        assert_eq!(agent.id, id);
        assert_eq!(agent.executable, second.join(format!("{id}.exe")));
    }
    assert_eq!(discovery.agents.len(), 6);
    assert_eq!(discovery.agents[5].executable, shim);
    for id in LEGACY_AGENT_IDS {
        let bare = Fixture::file(&first, id);
        let discovery = discover_installed_agents_with(input).expect("bare legacy name");
        assert_eq!(
            discovery
                .agents
                .iter()
                .find(|agent| agent.id == id)
                .expect("legacy row")
                .executable,
            bare
        );
        let exe = Fixture::file(&first, &format!("{id}.exe"));
        let discovery =
            discover_installed_agents_with(input).expect("exe beats bare in same directory");
        assert_eq!(
            discovery
                .agents
                .iter()
                .find(|agent| agent.id == id)
                .expect("legacy row")
                .executable,
            exe
        );
    }
}

#[test]
fn actual_projection_keeps_limit_and_missing_path_diagnostics() {
    let fixture = Fixture::new();
    let first = fixture.directory("first");
    let second = fixture.directory("second");
    Fixture::candidate(&first, "codex");
    Fixture::candidate(&second, "qwen");
    let path = env::join_paths([&first, &second]).expect("synthetic PATH");
    let input = DiscoveryInput {
        path: Some(&path),
        current_dir: &fixture.0,
        style: ExecutableStyle::native(),
        limits: ScanLimits {
            path_entries: 1,
            ..ScanLimits::default()
        },
    };
    let discovery = discover_installed_agents_with(input).expect("bounded scan");
    assert_eq!(
        discovery
            .agents
            .iter()
            .map(|agent| agent.id)
            .collect::<Vec<_>>(),
        ["codex"]
    );
    assert!(!discovery.report.path_scan_complete());
    assert!(discovery.report.path_entries_truncated);
    assert_eq!(
        *detection(&discovery.report, "qwen-code"),
        Detection::Inconclusive
    );
    let absent = discover_installed_agents_with(DiscoveryInput {
        path: None,
        ..input
    })
    .expect("absent PATH");
    assert!(absent.agents.is_empty());
    assert!(!absent.report.path_scan_complete());
    assert_eq!(
        *detection(&absent.report, "codex"),
        Detection::NotChecked(NotCheckedReason::PathNotProvided)
    );
    let invalid = discover_installed_agents_with(DiscoveryInput {
        limits: ScanLimits {
            path_bytes: 0,
            ..ScanLimits::default()
        },
        ..input
    })
    .expect_err("input limits are errors, not empty successful lists");
    assert_eq!(invalid.kind(), std::io::ErrorKind::InvalidInput);
}

#[cfg(unix)]
#[test]
fn actual_projection_does_not_promote_a_hit_after_an_unreadable_earlier_candidate() {
    use std::os::unix::fs::symlink;
    let fixture = Fixture::new();
    let first = fixture.directory("first");
    let second = fixture.directory("second");
    symlink("codex", first.join("codex")).expect("synthetic candidate metadata failure");
    let later = Fixture::candidate(&second, "codex");
    let discovery = installed_in(&fixture, &[&first, &second]);
    assert!(
        discovery.agents.is_empty(),
        "unknown first PATH choice cannot become a selected executable"
    );
    assert!(!discovery.report.path_scan_complete());
    let Detection::ExecutableFound(evidence) = detection(&discovery.report, "codex") else {
        panic!("later file remains visible in diagnostics");
    };
    assert_eq!(evidence.path, later);
    assert!(evidence.earlier_uncertainty);
    assert_eq!(discovery.report.issues.len(), 1);
}

#[test]
fn public_entry_point_reads_only_the_childs_synthetic_process_path() {
    let fixture = Fixture::new();
    let first = fixture.directory("first");
    let second = fixture.directory("second");
    Fixture::candidate(&first, "codex");
    Fixture::candidate(&first, "zed");
    Fixture::candidate(&first, "agent");
    Fixture::candidate(&first, "code");
    Fixture::candidate(&second, "qwen");
    let mixed = env::join_paths([&first, &second]).expect("synthetic PATH");
    let mut entries = vec![first.as_path(); ScanLimits::default().path_entries];
    entries.push(&second);
    let truncated = env::join_paths(entries).expect("over-budget synthetic PATH");
    let scenarios = vec![
        ("mixed", Some(mixed)),
        ("relative", Some(OsString::from("first"))),
        ("truncated", Some(truncated)),
        ("absent", None),
    ];
    #[cfg(unix)]
    let scenarios = {
        let mut scenarios = scenarios;
        scenarios.push((
            "oversized",
            Some(OsString::from(
                "x".repeat(ScanLimits::default().path_bytes + 1),
            )),
        ));
        scenarios
    };
    for (scenario, path) in scenarios {
        let mut child = std::process::Command::new(env::current_exe().expect("test executable"));
        child
            .args(["--exact", "synthetic_process_path_child", "--nocapture"])
            .env("MAGPIE_AGENT_PATH_TEST_SCENARIO", scenario)
            .env_remove("PATH")
            .current_dir(&fixture.0);
        if let Some(path) = path {
            child.env("PATH", path);
        }
        let output = child.output().expect("run child with isolated environment");
        assert!(
            output.status.success(),
            "child stdout: {}\nstderr: {}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert_eq!(
            stderr.contains("ambiguous executable names omitted"),
            matches!(scenario, "mixed" | "relative" | "truncated")
        );
        assert_eq!(
            stderr.contains("PATH scan incomplete"),
            matches!(scenario, "truncated" | "absent")
        );
        assert_eq!(stderr.contains("PATH scan failed"), scenario == "oversized");
    }
}

#[test]
fn synthetic_process_path_child() {
    let Ok(scenario) = env::var("MAGPIE_AGENT_PATH_TEST_SCENARIO") else {
        return;
    };
    if scenario == "oversized" {
        let error = agents::discover_installed_agents_report().expect_err("oversized process PATH");
        assert_eq!(error.kind(), std::io::ErrorKind::InvalidInput);
        assert!(agents::discover_installed_agents().is_empty());
        return;
    }
    let discovery = agents::discover_installed_agents_report().expect("isolated process report");
    let expected = match scenario.as_str() {
        "mixed" => vec!["codex", "zed", "qwen-code"],
        "relative" => vec!["codex", "zed"],
        "truncated" => vec!["codex", "zed"],
        "absent" => vec![],
        _ => panic!("unknown synthetic child scenario"),
    };
    assert_eq!(
        discovery.report.path_scan_complete(),
        matches!(scenario.as_str(), "mixed" | "relative")
    );
    assert_eq!(
        discovery
            .agents
            .iter()
            .map(|agent| agent.id)
            .collect::<Vec<_>>(),
        expected
    );
    assert_eq!(agents::discover_installed_agents(), discovery.agents);
}
