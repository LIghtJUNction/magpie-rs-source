//! The 68-agent requirement catalog and bounded, file-only PATH discovery.
//!
//! A matching filename is evidence of a candidate program, not authenticated
//! product identity, configuration support, a running process or a connection.
//! No directory contents, configuration files or executable contents are read.
//!
//! This covers only the registry and PATH portion of REQ-001/011/085. The
//! platform/configuration/instance discovery and REQ-012 adapters remain separate.
//! Unknown rules remain unchecked even if a file happens to match the catalog ID.
//! The current catalog has 16 application PATH rules, 4 editor-host-only rules,
//! and 48 unknown discovery rules. These are coverage counts, not installations.
//! Six ambiguous command names require independent product identity evidence;
//! four applications currently have no unambiguous name rule.

use std::{
    collections::BTreeMap,
    env,
    ffi::OsStr,
    fs, io,
    path::{Path, PathBuf},
};

/// Where this Rust catalog's stable identity came from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum IdOrigin {
    /// Unchanged identity from the five existing Rust discovery rules.
    ExistingRust,
    /// An explicit Magpie subcommand in ORIGINAL_PARITY.md section 7.
    OriginalHelp,
    /// A new Rust catalog identity; the original internal ID is unverified.
    CatalogAssigned,
}

/// Required integration method, independently of its implementation status.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum IntegrationContract {
    /// Native configuration, including exact ownership and recovery.
    Configuration,
    /// Editor chat/plugin configuration; finding its host proves less.
    EditorIntegration,
    /// Agent Client Protocol and its associated configuration.
    AgentClientProtocol,
    /// Application's local API, not a guessed file format.
    LocalApi,
    /// Application's own import/confirmation flow.
    ConfirmedImport,
    /// Name is retained, but the precise integration entry is still unknown.
    Unknown,
}

/// Implemented configuration behavior, not the original product's capabilities.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ConfigurationSupport {
    /// No adapter is implemented by this catalog.
    Unimplemented,
    /// Existing Codex TOML reader and partial transaction adapter elsewhere.
    ///
    /// This is a source capability reference, not permission to configure a
    /// detected file. Transactions currently support Linux, Android and macOS;
    /// other targets reject writes. No runtime availability is checked here.
    ExistingCodexPartial,
}

/// What a matching program can establish.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProgramRole {
    /// Candidate for the named application itself.
    Agent,
    /// Editor host only; its chat integration was not examined.
    EditorHost,
}

/// Documented names to probe, in order within each PATH directory.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PathRule {
    pub programs: &'static [&'static str],
    /// Public program-name evidence, or the pre-existing Rust rule.
    pub reference: &'static str,
    pub role: ProgramRole,
}

/// Discovery coverage is independent of configuration implementation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DiscoveryRule {
    Path(PathRule),
    /// No verified executable name; does not mean the application is absent.
    Unknown,
}

/// A stable requirement entry, in ORIGINAL_PARITY section 7 order.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AgentDefinition {
    pub id: &'static str,
    pub name: &'static str,
    pub id_origin: IdOrigin,
    pub contract: IntegrationContract,
    pub configuration: ConfigurationSupport,
    pub discovery: DiscoveryRule,
}

const fn program(
    programs: &'static [&'static str],
    reference: &'static str,
    role: ProgramRole,
) -> DiscoveryRule {
    DiscoveryRule::Path(PathRule {
        programs,
        reference,
        role,
    })
}

const LEGACY: &str =
    "pre-registry crates/magpie-core/src/agents.rs::installed_agents_in_path (existing rule)";
const PI: &str = "https://github.com/badlogic/pi-mono/blob/main/packages/coding-agent/package.json";
const OMP: &str =
    "https://github.com/can1357/oh-my-pi/blob/main/packages/coding-agent/package.json";
const GOOSE: &str = "https://github.com/block/goose/blob/main/crates/goose-cli/Cargo.toml";
// The official installer declares both the primary and legacy symlink names.
const CURSOR: &str = "https://cursor.com/install";
const CODE: &str = "https://code.visualstudio.com/docs/configure/command-line";
const CODIUM: &str = "https://github.com/VSCodium/vscodium/blob/master/docs/usage.md";
const COPILOT: &str = "https://docs.github.com/en/copilot/how-tos/set-up/install-copilot-cli";
const CRUSH: &str = "https://github.com/charmbracelet/crush";
const HERMES: &str = "https://github.com/NousResearch/hermes-agent/blob/main/pyproject.toml";
const KIMI: &str = "https://moonshotai.github.io/kimi-cli/en/guides/getting-started.html";
const QWEN: &str = "https://github.com/QwenLM/qwen-code/blob/main/packages/cli/package.json";
const DROID: &str = "https://docs.factory.com/droid-cli/quickstart";
const CLINE: &str = "https://docs.cline.bot/cline-cli/overview";
const QODER: &str = "https://registry.npmjs.org/@qoder-ai/qodercli/1.1.80";

macro_rules! entry {
    ($id:literal, $name:literal, $origin:ident, $contract:ident, $support:ident, $rule:expr) => {
        AgentDefinition {
            id: $id,
            name: $name,
            id_origin: IdOrigin::$origin,
            contract: IntegrationContract::$contract,
            configuration: ConfigurationSupport::$support,
            discovery: $rule,
        }
    };
}

use DiscoveryRule::Unknown;
use ProgramRole::{Agent, EditorHost};

/// Complete minimum catalog; registry presence is never installation evidence.
///
/// Names/order are frozen to spec01 and ORIGINAL_PARITY section 7. The five
/// existing IDs and eight additional explicit help IDs are preserved. Other IDs
/// are assigned here and explicitly do not claim original internal identity.
pub static CATALOG: [AgentDefinition; 68] = [
    entry!(
        "claude",
        "Claude Code",
        ExistingRust,
        Configuration,
        Unimplemented,
        program(&["claude"], LEGACY, Agent)
    ),
    entry!(
        "claude-desktop",
        "Claude Desktop",
        CatalogAssigned,
        Configuration,
        Unimplemented,
        Unknown
    ),
    entry!(
        "claude-science",
        "Claude Science",
        CatalogAssigned,
        Unknown,
        Unimplemented,
        Unknown
    ),
    entry!(
        "codex",
        "Codex",
        ExistingRust,
        Configuration,
        ExistingCodexPartial,
        program(&["codex"], LEGACY, Agent)
    ),
    entry!(
        "gemini",
        "Gemini CLI",
        ExistingRust,
        Configuration,
        Unimplemented,
        program(&["gemini"], LEGACY, Agent)
    ),
    entry!(
        "antigravity-cli",
        "Antigravity CLI",
        CatalogAssigned,
        Unknown,
        Unimplemented,
        Unknown
    ),
    entry!(
        "opencode",
        "OpenCode",
        ExistingRust,
        Configuration,
        Unimplemented,
        program(&["opencode"], LEGACY, Agent)
    ),
    entry!(
        "openchamber",
        "OpenChamber",
        CatalogAssigned,
        Configuration,
        Unimplemented,
        Unknown
    ),
    entry!(
        "mimocode",
        "MiMo Code",
        OriginalHelp,
        Configuration,
        Unimplemented,
        Unknown
    ),
    entry!(
        "deveco",
        "DevEco Code",
        OriginalHelp,
        Configuration,
        Unimplemented,
        Unknown
    ),
    entry!(
        "pi",
        "Pi",
        OriginalHelp,
        Configuration,
        Unimplemented,
        program(&["pi"], PI, Agent)
    ),
    entry!(
        "oh-my-pi",
        "oh-my-pi",
        CatalogAssigned,
        Configuration,
        Unimplemented,
        program(&["omp"], OMP, Agent)
    ),
    entry!(
        "stepcode",
        "StepCode",
        CatalogAssigned,
        Configuration,
        Unimplemented,
        Unknown
    ),
    entry!(
        "mothx",
        "MothX",
        CatalogAssigned,
        Configuration,
        Unimplemented,
        Unknown
    ),
    entry!(
        "penguin-harness",
        "PenguinHarness",
        CatalogAssigned,
        Configuration,
        Unimplemented,
        Unknown
    ),
    entry!(
        "aside",
        "Aside",
        OriginalHelp,
        Configuration,
        Unimplemented,
        Unknown
    ),
    entry!(
        "omo",
        "OmO",
        CatalogAssigned,
        Configuration,
        Unimplemented,
        Unknown
    ),
    entry!(
        "prime-agent",
        "Prime Agent",
        CatalogAssigned,
        Configuration,
        Unimplemented,
        Unknown
    ),
    entry!(
        "goose",
        "Goose",
        OriginalHelp,
        Configuration,
        Unimplemented,
        program(&["goose"], GOOSE, Agent)
    ),
    entry!(
        "cursor",
        "Cursor CLI",
        OriginalHelp,
        Configuration,
        Unimplemented,
        program(&["agent", "cursor-agent"], CURSOR, Agent)
    ),
    entry!(
        "cursor-private-inference",
        "Cursor Private Inference",
        CatalogAssigned,
        Unknown,
        Unimplemented,
        Unknown
    ),
    entry!(
        "zed",
        "Zed",
        ExistingRust,
        Configuration,
        Unimplemented,
        program(&["zed"], LEGACY, EditorHost)
    ),
    entry!(
        "vscode-chat",
        "VS Code Chat",
        CatalogAssigned,
        EditorIntegration,
        Unimplemented,
        program(&["code"], CODE, EditorHost)
    ),
    entry!(
        "vscode-insiders-chat",
        "VS Code Insiders Chat",
        CatalogAssigned,
        EditorIntegration,
        Unimplemented,
        program(&["code-insiders"], CODE, EditorHost)
    ),
    entry!(
        "vscodium-chat",
        "VSCodium Chat",
        CatalogAssigned,
        EditorIntegration,
        Unimplemented,
        program(&["codium"], CODIUM, EditorHost)
    ),
    entry!(
        "zoo-code",
        "Zoo Code",
        CatalogAssigned,
        EditorIntegration,
        Unimplemented,
        Unknown
    ),
    entry!(
        "jetbrains-air",
        "JetBrains Air",
        CatalogAssigned,
        AgentClientProtocol,
        Unimplemented,
        Unknown
    ),
    entry!(
        "copilot-jetbrains",
        "Copilot (JetBrains)",
        CatalogAssigned,
        EditorIntegration,
        Unimplemented,
        Unknown
    ),
    entry!(
        "copilot",
        "Copilot CLI",
        OriginalHelp,
        Configuration,
        Unimplemented,
        program(&["copilot"], COPILOT, Agent)
    ),
    entry!(
        "crush",
        "Crush",
        OriginalHelp,
        Configuration,
        Unimplemented,
        program(&["crush"], CRUSH, Agent)
    ),
    entry!(
        "deepseek-harness",
        "DeepSeek Harness",
        CatalogAssigned,
        Configuration,
        Unimplemented,
        Unknown
    ),
    entry!(
        "reasonix-studio",
        "Reasonix Studio",
        CatalogAssigned,
        Configuration,
        Unimplemented,
        Unknown
    ),
    entry!(
        "command-code",
        "Command Code",
        CatalogAssigned,
        Configuration,
        Unimplemented,
        Unknown
    ),
    entry!(
        "fx",
        "fx",
        CatalogAssigned,
        Configuration,
        Unimplemented,
        Unknown
    ),
    entry!(
        "devin",
        "Devin",
        CatalogAssigned,
        Configuration,
        Unimplemented,
        Unknown
    ),
    entry!(
        "hermes-agent",
        "Hermes Agent",
        CatalogAssigned,
        Configuration,
        Unimplemented,
        program(&["hermes", "hermes-agent"], HERMES, Agent)
    ),
    entry!(
        "mister-morph",
        "Mister Morph",
        CatalogAssigned,
        Configuration,
        Unimplemented,
        Unknown
    ),
    entry!(
        "kimi-code",
        "Kimi Code",
        CatalogAssigned,
        Configuration,
        Unimplemented,
        program(&["kimi"], KIMI, Agent)
    ),
    entry!(
        "jcode",
        "jcode",
        CatalogAssigned,
        Configuration,
        Unimplemented,
        Unknown
    ),
    entry!(
        "qwen-code",
        "Qwen Code",
        CatalogAssigned,
        Configuration,
        Unimplemented,
        program(&["qwen"], QWEN, Agent)
    ),
    entry!(
        "muse-code",
        "Muse Code",
        CatalogAssigned,
        Configuration,
        Unimplemented,
        Unknown
    ),
    entry!(
        "empryo",
        "Empryo",
        CatalogAssigned,
        Configuration,
        Unimplemented,
        Unknown
    ),
    entry!(
        "ante",
        "Ante",
        CatalogAssigned,
        Configuration,
        Unimplemented,
        Unknown
    ),
    entry!(
        "astrbot",
        "AstrBot",
        CatalogAssigned,
        Configuration,
        Unimplemented,
        Unknown
    ),
    entry!(
        "minimax-code",
        "MiniMax Code",
        CatalogAssigned,
        Configuration,
        Unimplemented,
        Unknown
    ),
    entry!(
        "droid",
        "Droid (Factory)",
        CatalogAssigned,
        Configuration,
        Unimplemented,
        program(&["droid"], DROID, Agent)
    ),
    entry!(
        "cline-cli",
        "Cline CLI",
        CatalogAssigned,
        Configuration,
        Unimplemented,
        program(&["cline"], CLINE, Agent)
    ),
    entry!(
        "qoder-cli",
        "Qoder CLI",
        CatalogAssigned,
        Configuration,
        Unimplemented,
        program(&["qoder", "qodercli"], QODER, Agent)
    ),
    entry!(
        "qoder-cn-cli",
        "Qoder CN CLI",
        CatalogAssigned,
        Configuration,
        Unimplemented,
        Unknown
    ),
    entry!(
        "grok-build",
        "Grok Build",
        CatalogAssigned,
        Configuration,
        Unimplemented,
        Unknown
    ),
    entry!(
        "grok-app",
        "Grok App",
        CatalogAssigned,
        Configuration,
        Unimplemented,
        Unknown
    ),
    entry!(
        "zcode",
        "ZCode",
        CatalogAssigned,
        Configuration,
        Unimplemented,
        Unknown
    ),
    entry!(
        "workbuddy",
        "WorkBuddy",
        CatalogAssigned,
        Configuration,
        Unimplemented,
        Unknown
    ),
    entry!(
        "workbuddy-ai",
        "WorkBuddy AI",
        CatalogAssigned,
        Configuration,
        Unimplemented,
        Unknown
    ),
    entry!(
        "codebuddy-code",
        "CodeBuddy Code",
        CatalogAssigned,
        Configuration,
        Unimplemented,
        Unknown
    ),
    entry!(
        "raycast",
        "Raycast",
        CatalogAssigned,
        Configuration,
        Unimplemented,
        Unknown
    ),
    entry!(
        "kiro-crew",
        "Kiro Crew",
        CatalogAssigned,
        Configuration,
        Unimplemented,
        Unknown
    ),
    entry!(
        "openpencil",
        "OpenPencil",
        CatalogAssigned,
        Configuration,
        Unimplemented,
        Unknown
    ),
    entry!(
        "newmax",
        "NewMax",
        CatalogAssigned,
        Configuration,
        Unimplemented,
        Unknown
    ),
    entry!(
        "t3-code",
        "T3 Code",
        CatalogAssigned,
        Configuration,
        Unimplemented,
        Unknown
    ),
    entry!(
        "openhanako",
        "OpenHanako",
        CatalogAssigned,
        Configuration,
        Unimplemented,
        Unknown
    ),
    entry!(
        "atomcode",
        "AtomCode",
        CatalogAssigned,
        Configuration,
        Unimplemented,
        Unknown
    ),
    entry!(
        "snow-cli",
        "Snow CLI",
        CatalogAssigned,
        Configuration,
        Unimplemented,
        Unknown
    ),
    entry!(
        "alma",
        "Alma",
        CatalogAssigned,
        LocalApi,
        Unimplemented,
        Unknown
    ),
    entry!(
        "pencil",
        "Pencil",
        CatalogAssigned,
        ConfirmedImport,
        Unimplemented,
        Unknown
    ),
    entry!(
        "cindy",
        "Cindy",
        CatalogAssigned,
        ConfirmedImport,
        Unimplemented,
        Unknown
    ),
    entry!(
        "lody",
        "Lody",
        CatalogAssigned,
        ConfirmedImport,
        Unimplemented,
        Unknown
    ),
    entry!(
        "aionui",
        "AionUi",
        CatalogAssigned,
        ConfirmedImport,
        Unimplemented,
        Unknown
    ),
];

/// Preserve the old discovery API's ordering when migrating its five results.
pub const LEGACY_AGENT_IDS: [&str; 5] = ["claude", "gemini", "opencode", "codex", "zed"];

/// Looks up a stable catalog identity, independently of installation.
#[must_use]
pub fn get(id: &str) -> Option<&'static AgentDefinition> {
    CATALOG.iter().find(|agent| agent.id == id)
}

/// Filename/permission policy; PATH separators always follow the host OS.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ExecutableStyle {
    /// Exact bare name and at least one executable mode bit; requires a Unix host.
    Unix,
    /// `.exe`, bare name, `.cmd`, `.bat`, `.com`, in this order.
    ///
    /// A fixed candidate policy, preserving the old `.exe`/bare priority and
    /// adding common shims for new agents. Legacy rules retain only `.exe`/bare.
    /// This does not emulate shell-specific PATHEXT rules.
    Windows,
}

impl ExecutableStyle {
    #[must_use]
    pub const fn native() -> Self {
        if cfg!(windows) {
            Self::Windows
        } else {
            Self::Unix
        }
    }

    fn suffixes(self, legacy: bool) -> &'static [&'static str] {
        match (self, legacy) {
            (Self::Unix, _) => &[""],
            (Self::Windows, true) => &[".exe", ""],
            (Self::Windows, false) => &[".exe", "", ".cmd", ".bat", ".com"],
        }
    }
}

/// Safety ceilings, not calibrated latency/RSS budgets. Callers can lower them.
/// Filesystem calls themselves have no deadline, including on network mounts.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ScanLimits {
    pub path_bytes: usize,
    pub path_entries: usize,
    pub entry_bytes: usize,
    pub issues: usize,
}

impl Default for ScanLimits {
    fn default() -> Self {
        Self {
            path_bytes: 65_536,
            path_entries: 128,
            entry_bytes: 4096,
            issues: 128,
        }
    }
}

/// Explicit inputs make discovery testable without changing process environment.
#[derive(Clone, Copy, Debug)]
pub struct DiscoveryInput<'a> {
    /// Split with the host's standard PATH syntax. None means PATH was absent.
    pub path: Option<&'a OsStr>,
    /// Absolute base for empty/relative PATH entries, preserving shell semantics.
    pub current_dir: &'a Path,
    pub style: ExecutableStyle,
    pub limits: ScanLimits,
}

/// Bounded retained error information; messages never include configuration data.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FileError {
    pub kind: io::ErrorKind,
    pub raw_os_error: Option<i32>,
}

impl From<io::Error> for FileError {
    fn from(error: io::Error) -> Self {
        Self {
            kind: error.kind(),
            raw_os_error: error.raw_os_error(),
        }
    }
}

/// Each supplied PATH entry's disposition; duplicate indices refer to this list.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DirectoryState {
    Ready,
    Missing,
    NotDirectory,
    Error(FileError),
    DuplicateOf(usize),
    EntryTooLong,
    /// A drive-relative Windows entry cannot be resolved from the supplied base.
    /// No implicit per-drive process working directory is consulted.
    UnresolvedPath,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DirectoryScan {
    pub path_index: usize,
    pub directory: PathBuf,
    pub state: DirectoryState,
}

/// Why an entry was never evaluated for an executable.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NotCheckedReason {
    UnknownRule,
    PathNotProvided,
}

/// Provenance of the selected file; symlinks to regular executable files count.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ExecutableEvidence {
    pub path: PathBuf,
    pub path_index: usize,
    pub program: &'static str,
    pub suffix: &'static str,
    pub name_reference: &'static str,
    /// Errors/limits before this hit could have hidden an earlier candidate.
    pub earlier_uncertainty: bool,
}

/// No variant claims connected/configured state or checks the file's identity.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Detection {
    ExecutableFound(ExecutableEvidence),
    EditorHostFound(ExecutableEvidence),
    /// A documented name also belongs to unrelated tools, or is too generic.
    /// It is deliberately excluded from the installed-agent projection.
    AmbiguousExecutable {
        evidence: ExecutableEvidence,
        reason: &'static str,
    },
    NotFoundOnPath,
    /// At least one error/skipped entry prevents a reliable negative result.
    Inconclusive,
    NotChecked(NotCheckedReason),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AgentDetection {
    pub definition: &'static AgentDefinition,
    pub detection: Detection,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CandidateIssue {
    pub agent_id: &'static str,
    pub path: PathBuf,
    pub error: FileError,
}

/// Actual operation counts, not measured time or memory.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ScanStats {
    pub path_entries: usize,
    pub unique_directories: usize,
    pub directory_metadata_calls: usize,
    pub candidate_metadata_calls: usize,
    pub non_file_candidates: usize,
    pub non_executable_candidates: usize,
    pub candidate_errors: usize,
    pub ambiguous_candidates: usize,
    pub suppressed_issues: usize,
}

/// Results stay in catalog order, regardless of filesystem enumeration order.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DiscoveryReport {
    pub agents: Vec<AgentDetection>,
    pub directories: Vec<DirectoryScan>,
    pub issues: Vec<CandidateIssue>,
    pub stats: ScanStats,
    pub path_entries_truncated: bool,
}

impl DiscoveryReport {
    /// Whether all supplied PATH entries and implemented filename rules could be
    /// checked without limits or I/O errors. Unknown rules and product identity
    /// remain outside this claim; inspect each agent's detection separately.
    #[must_use]
    pub fn path_scan_complete(&self) -> bool {
        !self.path_entries_truncated
            && self.stats.candidate_errors == 0
            && !self.directories.iter().any(|entry| {
                matches!(
                    entry.state,
                    DirectoryState::Error(_)
                        | DirectoryState::EntryTooLong
                        | DirectoryState::UnresolvedPath
                )
            })
            && !self.agents.iter().any(|agent| {
                agent.detection == Detection::NotChecked(NotCheckedReason::PathNotProvided)
            })
    }

    /// Matching program candidates only; editor hosts and uncertain misses excluded.
    pub fn executable_candidates(
        &self,
    ) -> impl Iterator<Item = (&'static AgentDefinition, &ExecutableEvidence)> {
        self.agents
            .iter()
            .filter_map(|result| match &result.detection {
                Detection::ExecutableFound(evidence) => Some((result.definition, evidence)),
                _ => None,
            })
    }
}

/// Do not promote known collisions or generic launcher names to product identity.
/// Distinctive aliases can still provide a filename candidate for the same agent.
fn ambiguous_program(id: &str, program: &str) -> Option<&'static str> {
    match (id, program) {
        ("cursor", "agent") => Some("generic launcher name; use cursor-agent"),
        ("hermes-agent", "hermes") => Some("generic product name; use hermes-agent"),
        ("pi", "pi") => Some("short command name; Pi product identity is unverified"),
        ("oh-my-pi", "omp") => Some("short command name; oh-my-pi product identity is unverified"),
        ("goose", "goose") => Some("also used by github.com/pressly/goose database migrations"),
        ("crush", "crush") => Some("also used by github.com/liljencrantz/crush shell"),
        _ => None,
    }
}

fn has_executable_mode(metadata: &fs::Metadata, style: ExecutableStyle) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        style == ExecutableStyle::Windows || metadata.permissions().mode() & 0o111 != 0
    }
    #[cfg(not(unix))]
    {
        let _ = (metadata, style);
        true
    }
}

/// Inspects only fixed candidate paths; never calls read_dir or executes a file.
///
/// PATH priority wins over unambiguous aliases and Windows suffix priority.
/// Ambiguous names are retained separately while other aliases are checked.
/// Missing files are clean misses; errors and truncation remain explicit.
/// Symlinks are followed as in the old discovery API. An executable mode bit is
/// not proof that ACLs, noexec mounts, architecture or a loader permit execution.
///
/// # Errors
/// Returns InvalidInput for a relative base or an oversized input PATH, and
/// Unsupported for Unix permission checks on a non-Unix host. Filesystem failures
/// and unresolved PATH entries stay in the report, not as "not installed".
pub fn discover(input: DiscoveryInput<'_>) -> io::Result<DiscoveryReport> {
    if !input.current_dir.is_absolute() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "current_dir must be absolute",
        ));
    }
    if input
        .path
        .is_some_and(|path| path.as_encoded_bytes().len() > input.limits.path_bytes)
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "PATH exceeds the input byte limit",
        ));
    }
    #[cfg(not(unix))]
    if input.style == ExecutableStyle::Unix {
        return Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "Unix executable permissions are unavailable on this host",
        ));
    }
    let mut report = DiscoveryReport {
        agents: Vec::with_capacity(CATALOG.len()),
        directories: Vec::new(),
        issues: Vec::new(),
        stats: ScanStats::default(),
        path_entries_truncated: false,
    };
    let mut seen = BTreeMap::new();
    if let Some(path) = input.path {
        for (index, entry) in env::split_paths(path).enumerate() {
            if index >= input.limits.path_entries {
                report.path_entries_truncated = true;
                break;
            }
            report.stats.path_entries += 1;
            // Do not retain an oversized entry; its index and rejection suffice.
            if entry.as_os_str().as_encoded_bytes().len() > input.limits.entry_bytes {
                report.directories.push(DirectoryScan {
                    path_index: index,
                    directory: PathBuf::new(),
                    state: DirectoryState::EntryTooLong,
                });
                continue;
            }
            let directory = input.current_dir.join(entry);
            let state = if !directory.is_absolute() {
                DirectoryState::UnresolvedPath
            } else if let Some(&previous) = seen.get(&directory) {
                DirectoryState::DuplicateOf(previous)
            } else {
                seen.insert(directory.clone(), index);
                report.stats.unique_directories += 1;
                report.stats.directory_metadata_calls += 1;
                match fs::metadata(&directory) {
                    Ok(metadata) if metadata.is_dir() => DirectoryState::Ready,
                    Ok(_) => DirectoryState::NotDirectory,
                    Err(error) if error.kind() == io::ErrorKind::NotFound => {
                        DirectoryState::Missing
                    }
                    Err(error) => DirectoryState::Error(error.into()),
                }
            };
            report.directories.push(DirectoryScan {
                path_index: index,
                directory,
                state,
            });
        }
    }

    for definition in &CATALOG {
        let rule = match definition.discovery {
            DiscoveryRule::Unknown => {
                report.agents.push(AgentDetection {
                    definition,
                    detection: Detection::NotChecked(NotCheckedReason::UnknownRule),
                });
                continue;
            }
            DiscoveryRule::Path(rule) => rule,
        };
        if input.path.is_none() {
            report.agents.push(AgentDetection {
                definition,
                detection: Detection::NotChecked(NotCheckedReason::PathNotProvided),
            });
            continue;
        }
        let mut uncertain = false;
        let mut found = None;
        let mut ambiguous = None;
        'directories: for directory in &report.directories {
            match directory.state {
                DirectoryState::Ready => {}
                DirectoryState::Error(_)
                | DirectoryState::EntryTooLong
                | DirectoryState::UnresolvedPath => {
                    uncertain = true;
                    continue;
                }
                _ => continue,
            }
            for &name in rule.programs {
                for &suffix in input
                    .style
                    .suffixes(definition.id_origin == IdOrigin::ExistingRust)
                {
                    let path = directory.directory.join(format!("{name}{suffix}"));
                    report.stats.candidate_metadata_calls += 1;
                    match fs::metadata(&path) {
                        Ok(metadata) if !metadata.is_file() => {
                            report.stats.non_file_candidates += 1
                        }
                        Ok(metadata) if !has_executable_mode(&metadata, input.style) => {
                            report.stats.non_executable_candidates += 1;
                        }
                        Ok(_) => {
                            let evidence = ExecutableEvidence {
                                path,
                                path_index: directory.path_index,
                                program: name,
                                suffix,
                                name_reference: rule.reference,
                                earlier_uncertainty: uncertain,
                            };
                            if let Some(reason) = ambiguous_program(definition.id, name) {
                                report.stats.ambiguous_candidates += 1;
                                if ambiguous.is_none() {
                                    ambiguous = Some((evidence, reason));
                                }
                                continue;
                            }
                            found = Some(evidence);
                            break 'directories;
                        }
                        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                        Err(error) => {
                            uncertain = true;
                            report.stats.candidate_errors += 1;
                            if report.issues.len() < input.limits.issues {
                                report.issues.push(CandidateIssue {
                                    agent_id: definition.id,
                                    path,
                                    error: error.into(),
                                });
                            } else {
                                report.stats.suppressed_issues += 1;
                            }
                        }
                    }
                }
            }
        }
        let detection = match (found, ambiguous) {
            (Some(evidence), _) => match rule.role {
                ProgramRole::Agent => Detection::ExecutableFound(evidence),
                ProgramRole::EditorHost => Detection::EditorHostFound(evidence),
            },
            (None, Some((evidence, reason))) => Detection::AmbiguousExecutable { evidence, reason },
            (None, None) if uncertain || report.path_entries_truncated => Detection::Inconclusive,
            (None, None) => Detection::NotFoundOnPath,
        };
        report.agents.push(AgentDetection {
            definition,
            detection,
        });
    }
    Ok(report)
}

/// Discovers using the process PATH and cwd, without reading HOME or agent files.
///
/// # Errors
/// Returns current-directory lookup failures and discover's input-limit errors.
pub fn discover_current() -> io::Result<DiscoveryReport> {
    let current_dir = env::current_dir()?;
    let path = env::var_os("PATH");
    discover(DiscoveryInput {
        path: path.as_deref(),
        current_dir: &current_dir,
        style: ExecutableStyle::native(),
        limits: ScanLimits::default(),
    })
}
