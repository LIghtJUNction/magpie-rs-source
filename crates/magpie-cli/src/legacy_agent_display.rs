//! The observed Magpie 0.1.1948 `agents`/`ls` text slice, not a discovery adapter.
//!
//! Evidence: 36-original-cli-behavior.md and 39-original-cli-candidate-behavior.md.
//! The latter adds single Claude/Codex/Gemini candidates and the all-three set
//! without settings. Unobserved combinations/configurations stay out of scope.
//! Incomplete discovery is never converted into the original empty-`ls` response.
//!
//! Callers supply a core report and an explicitly inspected Codex directory;
//! there is no environment lookup, account/config content read or execution.
//! `not detected` means no positive evidence, including unimplemented rules,
//! not proof that all 68 products were checked or are absent.
//! Directory evidence does not attest that the directory is empty. Existing
//! config contents/settings and other applications' state directories remain
//! unexamined; matching their original populated status lines is not claimed.
//!
//! Integration: add `mod legacy_agent_display;` in the binary, then call
//! `render(TextCommand::Agents, &report, &codex_directory)` only for bare text
//! commands. Forward both returned streams and the returned exit code without
//! adding another prefix. Keep `agents --json` and GUI discovery on their
//! existing paths. No parser or process exit behavior is changed by this file.

use std::{
    error::Error,
    fmt, fs, io,
    path::{Path, PathBuf},
};

use magpie_core::agent_catalog::{AgentDefinition, Detection, DiscoveryReport, NotCheckedReason};

// Stable Rust IDs in the original's observed display order, not new identities.
const ORIGINAL_ORDER: [&str; 68] = [
    "claude",
    "claude-desktop",
    "claude-science",
    "codex",
    "gemini",
    "antigravity-cli",
    "opencode",
    "openchamber",
    "mimocode",
    "deveco",
    "pi",
    "stepcode",
    "mothx",
    "penguin-harness",
    "aside",
    "omo",
    "prime-agent",
    "goose",
    "cursor",
    "cursor-private-inference",
    "zed",
    "vscode-chat",
    "vscode-insiders-chat",
    "vscodium-chat",
    "zoo-code",
    "copilot-jetbrains",
    "jetbrains-air",
    "copilot",
    "crush",
    "deepseek-harness",
    "reasonix-studio",
    "command-code",
    "fx",
    "oh-my-pi",
    "devin",
    "hermes-agent",
    "mister-morph",
    "kimi-code",
    "qwen-code",
    "muse-code",
    "empryo",
    "ante",
    "astrbot",
    "jcode",
    "minimax-code",
    "droid",
    "cline-cli",
    "qoder-cli",
    "qoder-cn-cli",
    "grok-build",
    "grok-app",
    "zcode",
    "workbuddy",
    "workbuddy-ai",
    "codebuddy-code",
    "pencil",
    "openpencil",
    "newmax",
    "raycast",
    "kiro-crew",
    "t3-code",
    "openhanako",
    "atomcode",
    "snow-cli",
    "alma",
    "cindy",
    "aionui",
    "lody",
];

/// Bare text commands; existing structured CLI commands are outside this module.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TextCommand {
    Agents,
    Ls,
}

/// Directory evidence only; it never establishes installation or valid settings.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CodexDirectory {
    /// An explicit metadata lookup returned NotFound.
    Missing,
    /// A directory exists; the joined config path need not exist.
    Present { config_path: PathBuf },
    /// No usable directory evidence was supplied; absence is not established.
    NotChecked,
}

/// Complete stream contents and exit code for the observed text slice.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TextOutput {
    pub stdout: String,
    pub stderr: String,
    pub exit_code: u8,
    /// Rules not implemented by core, even when the PATH scan was complete.
    ///
    /// Retain these diagnostics separately from the byte-compatible text.
    /// They do not mean those applications are uninstalled.
    pub unchecked_agent_ids: Vec<&'static str>,
}

/// Conditions that must not be rendered as a clean empty result.
///
/// These errors are Rust slice diagnostics, not observed original CLI errors.
/// The caller handles them separately from the exit-1 empty `ls` output.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DisplayError {
    /// The report does not contain exactly the expected 68 unique identities.
    CatalogMismatch,
    /// Missing PATH, failed metadata, limits or an unchecked Codex directory.
    IncompleteDiscovery,
    /// A candidate exists, but original populated-list formatting was not observed.
    UnobservedCandidate { agent_id: &'static str },
    /// A matching filename cannot establish the candidate's product identity.
    UnverifiedCandidate { agent_id: &'static str },
    /// Original bytes were not observed for this combination of candidates/state.
    UnobservedCombination,
    /// A path cannot be safely represented as one absolute UTF-8 terminal value.
    UnrepresentableConfigPath,
}

impl fmt::Display for DisplayError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::CatalogMismatch => {
                formatter.write_str("agent report does not match the observed 68-entry catalog")
            }
            Self::IncompleteDiscovery => {
                formatter.write_str("agent discovery is incomplete; absence is unknown")
            }
            Self::UnobservedCandidate { agent_id } => write!(
                formatter,
                "populated-list formatting for {agent_id} is outside the observed CLI slice"
            ),
            Self::UnverifiedCandidate { agent_id } => {
                write!(formatter, "candidate identity for {agent_id} is unverified")
            }
            Self::UnobservedCombination => {
                formatter.write_str("candidate combination is outside the observed CLI slice")
            }
            Self::UnrepresentableConfigPath => {
                formatter.write_str("config path cannot be represented safely in the text listing")
            }
        }
    }
}

impl Error for DisplayError {}

/// Inspects one explicit absolute directory using metadata, without reading files.
///
/// No HOME fallback, directory creation, config parsing or executable probing is
/// performed. An existing directory is only the observed directory-presence hint;
/// its contents and any model/effort settings remain unexamined.
///
/// # Errors
/// Returns InvalidInput for relative paths, symlinks or non-directories, and
/// propagates metadata errors other than NotFound. Callers must not translate
/// these errors into [`CodexDirectory::Missing`].
pub fn inspect_codex_directory(home: &Path) -> io::Result<CodexDirectory> {
    if !home.is_absolute() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "Codex directory must be an explicit absolute path",
        ));
    }
    match fs::symlink_metadata(home) {
        Ok(metadata) if metadata.is_dir() => Ok(CodexDirectory::Present {
            config_path: home.join("config.toml"),
        }),
        Ok(_) => Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "Codex path is not a direct directory",
        )),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(CodexDirectory::Missing),
        Err(error) => Err(error),
    }
}

/// Checks the empty state used by observation 39, without reading file contents.
///
/// Claude/Gemini state directories must be absent. Codex may be absent or a
/// direct empty directory. Any existing state, including an account or unrelated
/// file, keeps the caller on its previous display path.
///
/// # Errors
/// Rejects relative paths and unsafe Codex directories, and propagates lookup
/// errors. An inspection failure must not establish an empty candidate state.
pub fn inspect_empty_candidate_state(home: &Path, codex_home: &Path) -> io::Result<bool> {
    if !home.is_absolute() || !codex_home.is_absolute() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "candidate state requires explicit absolute home directories",
        ));
    }
    for directory in [home.join(".claude"), home.join(".gemini")] {
        match fs::symlink_metadata(directory) {
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Ok(_) => return Ok(false),
            Err(error) => return Err(error),
        }
    }
    match inspect_codex_directory(codex_home)? {
        CodexDirectory::Missing => Ok(true),
        CodexDirectory::Present { .. } => {
            Ok(fs::read_dir(codex_home)?.next().transpose()?.is_none())
        }
        CodexDirectory::NotChecked => Ok(false),
    }
}

/// Renders only the two observed no-executable states without any filesystem I/O.
///
/// A directory-only Codex row reports `CLI not found` only after a reliable
/// `NotFoundOnPath` result. Merely receiving a candidate config path is not
/// directory evidence. Unknown discovery rules stay in `unchecked_agent_ids`.
///
/// # Errors
/// Rejects mismatched catalogs, incomplete scans, unchecked directories,
/// ambiguous candidates, all populated executable/host states, and unsafe paths.
/// An out-of-scope error is not an empty listing or permission to change JSON.
pub fn render(
    command: TextCommand,
    report: &DiscoveryReport,
    codex_directory: &CodexDirectory,
) -> Result<TextOutput, DisplayError> {
    render_observed(command, report, codex_directory, None)
}

/// Renders observation 39's candidates after an empty-state inspection succeeds.
///
/// The caller must first establish [`inspect_empty_candidate_state`] for the
/// same explicit HOME/CODEX_HOME. The config path remains available even when
/// CODEX_HOME does not exist. This renderer itself performs no filesystem I/O.
///
/// # Errors
/// Rejects incomplete discovery, unobserved candidate combinations and unsafe
/// config paths. Actual settings remain the caller's existing display behavior.
pub fn render_unconfigured_candidates(
    command: TextCommand,
    report: &DiscoveryReport,
    codex_directory: &CodexDirectory,
    codex_config_path: &Path,
) -> Result<TextOutput, DisplayError> {
    render_observed(command, report, codex_directory, Some(codex_config_path))
}

fn render_observed(
    command: TextCommand,
    report: &DiscoveryReport,
    codex_directory: &CodexDirectory,
    candidate_config_path: Option<&Path>,
) -> Result<TextOutput, DisplayError> {
    if report.agents.len() != ORIGINAL_ORDER.len()
        || ORIGINAL_ORDER.iter().any(|id| {
            report
                .agents
                .iter()
                .filter(|agent| agent.definition.id == *id)
                .count()
                != 1
        })
    {
        return Err(DisplayError::CatalogMismatch);
    }
    if !report.path_scan_complete()
        || !report.issues.is_empty()
        || matches!(codex_directory, CodexDirectory::NotChecked)
    {
        return Err(DisplayError::IncompleteDiscovery);
    }
    let mut unchecked_agent_ids = Vec::new();
    let mut unobserved = None;
    let mut candidates = Vec::new();
    for agent in &report.agents {
        match &agent.detection {
            Detection::NotFoundOnPath => {}
            Detection::NotChecked(NotCheckedReason::UnknownRule) => {
                unchecked_agent_ids.push(agent.definition.id);
            }
            Detection::ExecutableFound(evidence) => {
                if evidence.earlier_uncertainty {
                    return Err(DisplayError::IncompleteDiscovery);
                }
                if candidate_config_path.is_some()
                    && matches!(agent.definition.id, "claude" | "codex" | "gemini")
                {
                    candidates.push(agent.definition.id);
                } else {
                    unobserved.get_or_insert(DisplayError::UnobservedCandidate {
                        agent_id: agent.definition.id,
                    });
                }
            }
            Detection::EditorHostFound(evidence) => {
                if evidence.earlier_uncertainty {
                    return Err(DisplayError::IncompleteDiscovery);
                }
                unobserved.get_or_insert(DisplayError::UnobservedCandidate {
                    agent_id: agent.definition.id,
                });
            }
            Detection::AmbiguousExecutable { evidence, .. } => {
                if evidence.earlier_uncertainty {
                    return Err(DisplayError::IncompleteDiscovery);
                }
                unobserved.get_or_insert(DisplayError::UnverifiedCandidate {
                    agent_id: agent.definition.id,
                });
            }
            Detection::Inconclusive | Detection::NotChecked(NotCheckedReason::PathNotProvided) => {
                return Err(DisplayError::IncompleteDiscovery);
            }
        }
    }
    if let Some(error) = unobserved {
        return Err(error);
    }
    candidates.sort_unstable();
    if !candidates.is_empty()
        && (!matches!(
            candidates.as_slice(),
            ["claude"] | ["codex"] | ["gemini"] | ["claude", "codex", "gemini"]
        ) || (matches!(codex_directory, CodexDirectory::Present { .. })
            && !candidates.contains(&"codex")))
    {
        return Err(DisplayError::UnobservedCombination);
    }
    let config_path = if candidates.contains(&"codex") {
        Some(config_path_text(
            candidate_config_path.ok_or(DisplayError::UnrepresentableConfigPath)?,
        )?)
    } else {
        match codex_directory {
            CodexDirectory::Missing => None,
            CodexDirectory::Present { config_path } => {
                if !report.agents.iter().any(|agent| {
                    agent.definition.id == "codex" && agent.detection == Detection::NotFoundOnPath
                }) {
                    return Err(DisplayError::IncompleteDiscovery);
                }
                Some(config_path_text(config_path)?)
            }
            CodexDirectory::NotChecked => return Err(DisplayError::IncompleteDiscovery),
        }
    };
    let mut rows = Vec::new();
    for id in ORIGINAL_ORDER {
        let definition = report
            .agents
            .iter()
            .find(|agent| agent.definition.id == id)
            .map(|agent| agent.definition)
            .ok_or(DisplayError::CatalogMismatch)?;
        let (state, detail) = if candidates.contains(&id) {
            match id {
                "claude" => ("—  ·  effort —", Some("~/.claude/settings.json")),
                "codex" => ("—  ·  effort —", config_path),
                "gemini" => ("auth —  ·  —", Some("~/.gemini/settings.json")),
                _ => return Err(DisplayError::UnobservedCombination),
            }
        } else if id == "codex" && config_path.is_some() {
            ("CLI not found", config_path)
        } else {
            ("not detected", None)
        };
        if command == TextCommand::Agents || detail.is_some() {
            rows.push((display_name(definition), state, detail.unwrap_or("")));
        }
    }
    if rows.is_empty() {
        return Ok(TextOutput {
            stdout: String::new(),
            stderr: "magpie: no supported agents found on this machine\n".to_owned(),
            exit_code: 1,
            unchecked_agent_ids,
        });
    }
    let name_width = rows
        .iter()
        .map(|row| row.0.chars().count())
        .max()
        .unwrap_or(0);
    let state_width = rows
        .iter()
        .map(|row| row.1.chars().count())
        .max()
        .unwrap_or(0);
    let mut stdout = String::new();
    for (name, state, detail) in rows {
        stdout.push_str(&format!(
            "  {name:<name_width$}  {state:<state_width$}  {detail}\n"
        ));
    }
    Ok(TextOutput {
        stdout,
        stderr: String::new(),
        exit_code: 0,
        unchecked_agent_ids,
    })
}

fn config_path_text(path: &Path) -> Result<&str, DisplayError> {
    path.to_str()
        .filter(|text| !text.chars().any(char::is_control))
        .filter(|_| path.is_absolute())
        .ok_or(DisplayError::UnrepresentableConfigPath)
}

fn display_name(definition: &AgentDefinition) -> &'static str {
    match definition.id {
        "oh-my-pi" => "omp",
        "vscode-chat" => "VS Code",
        "vscode-insiders-chat" => "VS Code Insiders",
        "vscodium-chat" => "VSCodium",
        "droid" => "Droid",
        "cline-cli" => "Cline",
        "qoder-cli" => "Qoder",
        "qoder-cn-cli" => "Qoder CN",
        "codebuddy-code" => "CodeBuddy",
        _ => definition.name,
    }
}
