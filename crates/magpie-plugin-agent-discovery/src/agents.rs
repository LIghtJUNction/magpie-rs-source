//! Optional installed-agent inventory over the bounded legacy discovery catalog.
use crate::agent_catalog::{
    self, Detection, DiscoveryInput, DiscoveryReport, ExecutableStyle, LEGACY_AGENT_IDS, ScanLimits,
};
#[cfg(test)]
use std::ffi::OsStr;
use std::{env, io, path::PathBuf};

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
