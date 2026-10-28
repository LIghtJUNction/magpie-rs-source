//! The observed Magpie 0.1.1948 `agents`/`ls` text slice, not a discovery adapter.
//!
//! Evidence: 36-original-cli-behavior.md, observations 15, 16, 43 and 44.
//! Only the no-candidate and directory-only Codex cases have original evidence.
//! Other positive candidates return an explicit out-of-scope error. Incomplete
//! discovery is never converted into the original empty-`ls` response.
//!
//! Callers supply a core report and an explicitly inspected Codex directory;
//! there is no environment lookup, account/config read, execution or printing.
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
    for agent in &report.agents {
        match &agent.detection {
            Detection::NotFoundOnPath => {}
            Detection::NotChecked(NotCheckedReason::UnknownRule) => {
                unchecked_agent_ids.push(agent.definition.id);
            }
            Detection::ExecutableFound(evidence) | Detection::EditorHostFound(evidence) => {
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
    let config_path = match codex_directory {
        CodexDirectory::Missing => None,
        CodexDirectory::Present { config_path } => {
            if !report.agents.iter().any(|agent| {
                agent.definition.id == "codex" && agent.detection == Detection::NotFoundOnPath
            }) {
                return Err(DisplayError::IncompleteDiscovery);
            }
            let path = config_path
                .to_str()
                .filter(|path| !path.chars().any(char::is_control))
                .filter(|_| config_path.is_absolute())
                .ok_or(DisplayError::UnrepresentableConfigPath)?;
            Some(path)
        }
        CodexDirectory::NotChecked => return Err(DisplayError::IncompleteDiscovery),
    };
    let mut rows = Vec::new();
    for id in ORIGINAL_ORDER {
        let definition = report
            .agents
            .iter()
            .find(|agent| agent.definition.id == id)
            .map(|agent| agent.definition)
            .ok_or(DisplayError::CatalogMismatch)?;
        let detail = (id == "codex").then_some(config_path).flatten();
        if command == TextCommand::Agents || detail.is_some() {
            rows.push((
                display_name(definition),
                if detail.is_some() {
                    "CLI not found"
                } else {
                    "not detected"
                },
                detail.unwrap_or(""),
            ));
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
    let state_width = rows.iter().map(|row| row.1.len()).max().unwrap_or(0);
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
