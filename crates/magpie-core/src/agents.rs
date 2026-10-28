use std::env;
use std::ffi::OsStr;
#[cfg(unix)]
use std::fs;
use std::io;
use std::path::PathBuf;

use crate::agent_catalog::{
    self, Detection, DiscoveryInput, DiscoveryReport, ExecutableStyle, LEGACY_AGENT_IDS, ScanLimits,
};
use crate::codex_config::{self, CodexConfigError, CodexSelection};
use crate::paths;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConfigFileState {
    Unavailable,
    Missing,
    ValidToml,
    InvalidToml,
    TooLarge,
    InvalidUtf8,
    Unreadable,
    NotAFile,
    Symlink,
}

impl ConfigFileState {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Unavailable => "unavailable",
            Self::Missing => "missing",
            Self::ValidToml => "valid_toml",
            Self::InvalidToml => "invalid_toml",
            Self::TooLarge => "too_large",
            Self::InvalidUtf8 => "invalid_utf8",
            Self::Unreadable => "unreadable",
            Self::NotAFile => "not_a_file",
            Self::Symlink => "symlink",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CodexDiscovery {
    pub executable: Option<PathBuf>,
    pub config_path: Option<PathBuf>,
    pub config_state: ConfigFileState,
    pub config_error_line: Option<usize>,
    /// Explicit selections from the same config read that determined its state.
    ///
    /// Empty when the file is unavailable, missing, or invalid. No credentials
    /// or provider definitions are included.
    pub selection: CodexSelection,
}

/// An agent executable found on PATH, without a configuration or connection claim.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InstalledAgent {
    /// Stable application ID; an executable alias can have a different name.
    pub id: &'static str,
    /// Human-readable application name.
    pub name: &'static str,
    /// The executable selected using PATH order.
    pub executable: PathBuf,
}

/// Registry-backed filename candidates for the CLI and desktop.
///
/// Preserves the five legacy IDs/names/order (including the Zed host row), then
/// appends unambiguous application candidates in catalog order. New editor hosts
/// and ambiguous filenames are excluded. No configuration/connection is implied.
///
/// This compatibility view is not an exhaustive installation report. Incomplete
/// scans and omitted ambiguous names produce a bounded stderr diagnostic; use
/// discover_installed_agents_report for structured details. Hits whose PATH
/// priority is uncertain are also omitted rather than presented as selected.
#[must_use]
pub fn discover_installed_agents() -> Vec<InstalledAgent> {
    match discover_installed_agents_report() {
        Ok(discovery) => {
            if !discovery.report.path_scan_complete() {
                eprintln!(
                    "magpie: agent PATH scan incomplete; showing only hits with known PATH priority, absence is unknown"
                );
            }
            if discovery.report.stats.ambiguous_candidates > 0 {
                eprintln!(
                    "magpie: ambiguous executable names omitted from agent discovery; product identity is unverified"
                );
            }
            discovery.agents
        }
        Err(error) => {
            eprintln!(
                "magpie: agent PATH scan failed ({:?}); no complete result is available",
                error.kind()
            );
            Vec::new()
        }
    }
}

/// Actual candidates plus the complete bounded scan diagnostics.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InstalledAgentDiscovery {
    pub agents: Vec<InstalledAgent>,
    pub report: DiscoveryReport,
}

/// Explicit-input equivalent of the CLI/desktop discovery entry point.
///
/// Uses filesystem metadata only; never executes a candidate or reads account
/// files. The report retains unknown/host/ambiguous states, errors and limits.
///
/// # Errors
/// Returns the catalog discovery input validation errors.
pub fn discover_installed_agents_with(
    input: DiscoveryInput<'_>,
) -> io::Result<InstalledAgentDiscovery> {
    let report = agent_catalog::discover(input)?;
    let legacy = LEGACY_AGENT_IDS.iter().filter_map(|id| {
        report
            .agents
            .iter()
            .find(|agent| agent.definition.id == *id)
    });
    let additional = report
        .agents
        .iter()
        .filter(|agent| !LEGACY_AGENT_IDS.contains(&agent.definition.id));
    let agents = legacy
        .chain(additional)
        .filter_map(|agent| {
            let evidence = match &agent.detection {
                Detection::ExecutableFound(evidence) => evidence,
                Detection::EditorHostFound(evidence)
                    if LEGACY_AGENT_IDS.contains(&agent.definition.id) =>
                {
                    evidence
                }
                _ => return None,
            };
            if evidence.earlier_uncertainty {
                return None;
            }
            Some(InstalledAgent {
                id: agent.definition.id,
                name: agent.definition.name,
                executable: evidence.path.clone(),
            })
        })
        .collect();
    Ok(InstalledAgentDiscovery { agents, report })
}

/// Process PATH/cwd discovery with diagnostics, without reading HOME/config.
///
/// # Errors
/// Returns current-directory lookup or catalog input validation errors.
pub fn discover_installed_agents_report() -> io::Result<InstalledAgentDiscovery> {
    let current_dir = env::current_dir()?;
    let path = env::var_os("PATH");
    discover_installed_agents_with(DiscoveryInput {
        path: path.as_deref(),
        current_dir: &current_dir,
        style: ExecutableStyle::native(),
        limits: ScanLimits::default(),
    })
}

pub fn discover_codex() -> CodexDiscovery {
    let executable = find_codex_executable();
    let config_path = codex_config_path();
    let (config_state, config_error_line, selection) =
        config_path.as_deref().map(inspect_config_file).unwrap_or((
            ConfigFileState::Unavailable,
            None,
            CodexSelection::default(),
        ));

    CodexDiscovery {
        executable,
        config_path,
        config_state,
        config_error_line,
        selection,
    }
}

pub fn find_codex_executable() -> Option<PathBuf> {
    find_executable("codex")
}

pub fn codex_config_path() -> Option<PathBuf> {
    let home = env::var_os("CODEX_HOME")
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .or_else(|| paths::user_home_dir().map(|home| home.join(".codex")))?;

    let home = if home.is_absolute() {
        home
    } else {
        env::current_dir().ok()?.join(home)
    };

    Some(home.join("config.toml"))
}

fn inspect_config_file(path: &std::path::Path) -> (ConfigFileState, Option<usize>, CodexSelection) {
    let (state, line) = match codex_config::read_selection(path) {
        Ok(Some(selection)) => return (ConfigFileState::ValidToml, None, selection),
        Ok(None) => (ConfigFileState::Missing, None),
        Err(CodexConfigError::InvalidToml { line }) => (ConfigFileState::InvalidToml, line),
        Err(CodexConfigError::TooLarge) => (ConfigFileState::TooLarge, None),
        Err(CodexConfigError::InvalidUtf8) => (ConfigFileState::InvalidUtf8, None),
        Err(CodexConfigError::NotAFile) => (ConfigFileState::NotAFile, None),
        Err(CodexConfigError::Symlink) => (ConfigFileState::Symlink, None),
        Err(_) => (ConfigFileState::Unreadable, None),
    };
    (state, line, CodexSelection::default())
}

fn find_executable(name: &str) -> Option<PathBuf> {
    let path = env::var_os("PATH")?;
    find_executable_in_path(name, &path)
}

fn find_executable_in_path(name: &str, path: &OsStr) -> Option<PathBuf> {
    for directory in env::split_paths(path) {
        #[cfg(windows)]
        let candidates = [directory.join(format!("{name}.exe")), directory.join(name)];
        #[cfg(not(windows))]
        let candidates = [directory.join(name)];

        if let Some(candidate) = candidates
            .into_iter()
            .find(|candidate| is_executable(candidate))
        {
            return Some(candidate);
        }
    }
    None
}

fn is_executable(path: &std::path::Path) -> bool {
    if !path.is_file() {
        return false;
    }

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::metadata(path).is_ok_and(|metadata| metadata.permissions().mode() & 0o111 != 0)
    }

    #[cfg(not(unix))]
    {
        true
    }
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::Path;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::{SystemTime, UNIX_EPOCH};

    use super::*;

    static NEXT_FIXTURE: AtomicUsize = AtomicUsize::new(0);

    struct Fixture(PathBuf);

    impl Fixture {
        fn new() -> Self {
            let sequence = NEXT_FIXTURE.fetch_add(1, Ordering::Relaxed);
            let timestamp = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("test clock is after the Unix epoch")
                .as_nanos();
            let root = std::env::temp_dir().join(format!(
                "magpie-discovery-{}-{timestamp}-{sequence}",
                std::process::id()
            ));
            fs::create_dir(&root).expect("create isolated discovery directory");
            Self(root)
        }

        fn executable(directory: &Path, name: &str) -> PathBuf {
            let path = directory.join(format!("{name}{}", env::consts::EXE_SUFFIX));
            fs::write(&path, b"fixture-secret-content-is-never-read")
                .expect("write fake executable");
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                fs::set_permissions(&path, fs::Permissions::from_mode(0o700))
                    .expect("make fixture executable");
            }
            path
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn installed_agents_in_path(path: &OsStr) -> Vec<InstalledAgent> {
        let current_dir = env::current_dir().expect("test working directory");
        discover_installed_agents_with(DiscoveryInput {
            path: Some(path),
            current_dir: &current_dir,
            style: ExecutableStyle::native(),
            limits: ScanLimits::default(),
        })
        .expect("valid synthetic PATH")
        .agents
    }

    #[test]
    fn configured_selection_and_discovery_state_come_from_one_read_path() {
        let fixture = Fixture::new();
        let config = fixture.0.join("config.toml");
        fs::write(
            &config,
            b"model='selected'\nmodel_provider='native'\nmodel_reasoning_effort='medium'\napi_key='fixture-secret'",
        )
        .expect("write Codex fixture");
        let (state, line, selection) = inspect_config_file(&config);
        assert_eq!(state, ConfigFileState::ValidToml);
        assert_eq!(line, None);
        assert_eq!(selection.model.as_deref(), Some("selected"));
        assert_eq!(selection.model_provider.as_deref(), Some("native"));
        assert_eq!(selection.model_reasoning_effort.as_deref(), Some("medium"));
        assert!(!format!("{selection:?}").contains("fixture-secret"));
    }

    #[test]
    fn invalid_or_missing_config_does_not_produce_fake_selections() {
        let fixture = Fixture::new();
        let config = fixture.0.join("config.toml");
        assert_eq!(
            inspect_config_file(&config),
            (ConfigFileState::Missing, None, CodexSelection::default())
        );
        fs::write(
            &config,
            b"model='would-be-selected'\napi_key=\"fixture-secret-unclosed\n",
        )
        .expect("write malformed Codex fixture");
        assert_eq!(
            inspect_config_file(&config),
            (
                ConfigFileState::InvalidToml,
                Some(2),
                CodexSelection::default()
            )
        );
        assert_eq!(
            inspect_config_file(&fixture.0),
            (ConfigFileState::NotAFile, None, CodexSelection::default())
        );
    }

    #[test]
    fn installed_agents_include_only_known_executables_in_path_order() {
        let fixture = Fixture::new();
        let first = fixture.0.join("first");
        let second = fixture.0.join("second");
        fs::create_dir(&first).expect("create first PATH directory");
        fs::create_dir(&second).expect("create second PATH directory");
        let claude = Fixture::executable(&first, "claude");
        let gemini = Fixture::executable(&second, "gemini");
        let opencode = Fixture::executable(&second, "opencode");
        let codex = Fixture::executable(&first, "codex");
        Fixture::executable(&second, "codex");
        let zed = Fixture::executable(&second, "zed");
        Fixture::executable(&first, "unknown-agent");
        let path = env::join_paths([&first, &second]).expect("join isolated PATH entries");
        let installed = installed_agents_in_path(&path);
        assert_eq!(
            installed,
            vec![
                InstalledAgent {
                    id: "claude",
                    name: "Claude Code",
                    executable: claude
                },
                InstalledAgent {
                    id: "gemini",
                    name: "Gemini CLI",
                    executable: gemini
                },
                InstalledAgent {
                    id: "opencode",
                    name: "OpenCode",
                    executable: opencode
                },
                InstalledAgent {
                    id: "codex",
                    name: "Codex",
                    executable: codex
                },
                InstalledAgent {
                    id: "zed",
                    name: "Zed",
                    executable: zed
                },
            ]
        );
        assert!(!format!("{installed:?}").contains("fixture-secret-content"));
    }

    #[test]
    fn empty_installation_does_not_invent_agent_rows() {
        let fixture = Fixture::new();
        let directory = fixture.0.join(format!("codex{}", env::consts::EXE_SUFFIX));
        fs::create_dir(&directory).expect("create directory named like an executable");
        assert!(installed_agents_in_path(fixture.0.as_os_str()).is_empty());
    }

    #[cfg(unix)]
    #[test]
    fn nonexecutable_files_are_not_installed_agents() {
        let fixture = Fixture::new();
        let path = fixture.0.join("claude");
        fs::write(&path, b"fixture-config-data").expect("write nonexecutable fixture");
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600))
            .expect("clear executable bits");
        assert!(installed_agents_in_path(fixture.0.as_os_str()).is_empty());
    }
}
