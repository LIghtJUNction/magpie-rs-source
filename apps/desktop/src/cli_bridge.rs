//! Bounded, versioned adapter to the Magpie CLI shipped beside the desktop app.
//!
//! Configuration changes use plugin-owned CLI commands. Arbitrary CLI diagnostics
//! never cross this boundary. Agent-specific protocols belong to their plugins.
//! Keep this module independent of GPUI so its process protocol can be tested
//! without a window server or any real user configuration.

use std::{
    collections::HashSet,
    env,
    ffi::OsString,
    fmt, fs,
    net::SocketAddr,
    path::{Component, Path, PathBuf},
    process::Stdio,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU8, Ordering},
    },
    time::{Duration, Instant, SystemTime},
};

use async_io::Timer;
use async_process::Command;
use futures_lite::{AsyncRead, AsyncReadExt, future};
use serde::Deserialize;

#[path = "cli_bridge/owned_gateway.rs"]
pub(crate) mod owned_gateway;

const PROTOCOL_VERSION: &str = "0.2.0";
const VERSION_LINE: &[u8] = b"magpie 0.2.0";
const STDOUT_LIMIT: usize = 32 * 1024 * 1024;
const STDERR_LIMIT: usize = 8 * 1024;
const MAX_MODELS: usize = magpie_core::provider_store::MAX_TOTAL_MODELS;

// 0: idle; 1: admitted request; 2: cleanup could not be proved complete.
// A dropped request never admits an unbounded succession of orphaned children.
static ADMISSION: AtomicU8 = AtomicU8::new(0);
#[cfg(test)]
pub(crate) static PROCESS_TEST_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Request {
    PluginToggle {
        owner: String,
        enabled: bool,
        revision: u64,
    },
    PluginCall {
        owner: String,
        operation: String,
        arguments: Vec<String>,
        mutation: bool,
    },
    RefreshProvider {
        store: PathBuf,
        id: String,
        revision: u64,
    },
    Inspect {
        address: SocketAddr,
    },
}

impl Request {
    pub const fn is_mutation(&self) -> bool {
        match self {
            Self::RefreshProvider { .. } | Self::PluginToggle { .. } => true,
            Self::PluginCall { mutation, .. } => *mutation,
            Self::Inspect { .. } => false,
        }
    }

    fn validate(&self) -> Result<(), BridgeError> {
        match self {
            Self::PluginToggle {
                owner, revision, ..
            } => {
                if !valid_plugin_id(owner) || *revision == u64::MAX {
                    return Err(BridgeError::new(ErrorKind::ConfigInvalid));
                }
            }
            Self::PluginCall {
                owner,
                operation,
                arguments,
                ..
            } => {
                if !valid_plugin_id(owner)
                    || !valid_plugin_id(operation)
                    || arguments.len() > 64
                    || arguments.iter().any(|argument| {
                        argument.len() > 4096 || argument.chars().any(char::is_control)
                    })
                    || arguments.iter().map(String::len).sum::<usize>() > 64 * 1024
                {
                    return Err(BridgeError::new(ErrorKind::ConfigInvalid));
                }
            }
            Self::RefreshProvider {
                store,
                id,
                revision,
            } => {
                validate_config(store)?;
                if !magpie_core::catalog::valid_provider_id(id) || *revision == u64::MAX {
                    return Err(BridgeError::new(ErrorKind::ConfigInvalid));
                }
            }
            Self::Inspect { address } => validate_address(*address)?,
        }
        if self.is_mutation() && !transactions_supported() {
            return Err(BridgeError::new(ErrorKind::PlatformUnsupported));
        }
        Ok(())
    }

    fn arguments(&self) -> Vec<OsString> {
        fn words(values: &[&str]) -> Vec<OsString> {
            values.iter().map(OsString::from).collect()
        }
        match self {
            Self::PluginToggle {
                owner,
                enabled,
                revision,
            } => words(&[
                "plugins",
                if *enabled { "enable" } else { "disable" },
                owner,
                "--if-revision",
                &revision.to_string(),
                "--json",
            ]),
            Self::PluginCall {
                owner,
                operation,
                arguments,
                ..
            } => {
                let mut args = words(&["plugins", "call", owner, operation, "--json"]);
                args.extend(arguments.iter().map(OsString::from));
                args
            }
            Self::RefreshProvider {
                store,
                id,
                revision,
            } => {
                let mut args = words(&["providers", "refresh", id, "--store"]);
                args.push(store.as_os_str().to_owned());
                args.extend(words(&["--if-revision", &revision.to_string(), "--json"]));
                args
            }
            Self::Inspect { address } => {
                let mut args = words(&["gateway", "inspect", "--listen"]);
                args.extend([address.to_string().into(), "--json".into()]);
                args
            }
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ModelInfo {
    pub id: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GatewayInfo {
    pub address: SocketAddr,
    pub models: Vec<ModelInfo>,
    /// Reported by the running service, never inferred from the CLI version.
    pub server_version: Option<String>,
    pub instance_id: Option<String>,
    pub config_source: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Ownership {
    Unmanaged,
    Connected,
    Drifted,
    RecoveryRequired,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Reply {
    Plugins(serde_json::Value),
    PluginResult(serde_json::Value),
    ProviderRefreshed { provider_id: String, revision: u64 },
    Inspected(GatewayInfo),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ErrorKind {
    Busy,
    InvalidAddress,
    InvalidConfigPath,
    InvalidModel,
    CliMissing,
    CliOverrideInvalid,
    CliNotExecutable,
    CliVersionMismatch,
    CliChanged,
    LaunchFailed,
    TimedOut,
    Cancelled,
    OutputTooLarge,
    InvalidReply,
    CliRejected,
    GatewayUnavailable,
    ModelUnavailable,
    ConfigConflict,
    ConfigInvalid,
    ConfigReadOnly,
    RecoveryRequired,
    AlreadyManaged,
    NotManaged,
    PlatformUnsupported,
    CleanupPending,
}

impl ErrorKind {
    pub const fn code(self) -> &'static str {
        match self {
            Self::Busy => "bridge_busy",
            Self::InvalidAddress => "invalid_gateway_address",
            Self::InvalidConfigPath => "invalid_config_path",
            Self::InvalidModel => "invalid_model_id",
            Self::CliMissing => "cli_missing",
            Self::CliOverrideInvalid => "cli_override_invalid",
            Self::CliNotExecutable => "cli_not_executable",
            Self::CliVersionMismatch => "cli_version_mismatch",
            Self::CliChanged => "cli_changed",
            Self::LaunchFailed => "cli_launch_failed",
            Self::TimedOut => "operation_timed_out",
            Self::Cancelled => "operation_cancelled",
            Self::OutputTooLarge => "cli_output_too_large",
            Self::InvalidReply => "cli_reply_invalid",
            Self::CliRejected => "cli_request_rejected",
            Self::GatewayUnavailable => "gateway_unavailable",
            Self::ModelUnavailable => "model_unavailable",
            Self::ConfigConflict => "config_conflict",
            Self::ConfigInvalid => "config_invalid",
            Self::ConfigReadOnly => "config_read_only",
            Self::RecoveryRequired => "recovery_required",
            Self::AlreadyManaged => "already_managed",
            Self::NotManaged => "not_managed",
            Self::PlatformUnsupported => "platform_unsupported",
            Self::CleanupPending => "bridge_cleanup_pending",
        }
    }
}

/// Contains static categories only, never CLI stderr, paths, or configuration.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BridgeError {
    pub kind: ErrorKind,
    /// A write command was spawned. Refresh ownership before another write;
    /// cancellation, a deadline, or an invalid reply cannot establish rollback.
    pub mutation_may_have_run: bool,
}

impl BridgeError {
    pub const fn new(kind: ErrorKind) -> Self {
        Self {
            kind,
            mutation_may_have_run: false,
        }
    }

    pub const fn code(self) -> &'static str {
        self.kind.code()
    }

    fn after_spawn(kind: ErrorKind, mutation: bool) -> Self {
        Self {
            kind,
            mutation_may_have_run: mutation,
        }
    }
}

impl fmt::Display for BridgeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.code())?;
        if self.mutation_may_have_run {
            f.write_str("; refresh ownership before retrying")?;
        }
        Ok(())
    }
}

impl std::error::Error for BridgeError {}

#[derive(Clone, Debug, Default)]
pub struct Cancellation(Arc<AtomicBool>);

impl Cancellation {
    pub fn new() -> Self {
        Self::default()
    }

    pub(crate) fn from_flag(flag: Arc<AtomicBool>) -> Self {
        Self(flag)
    }

    pub fn cancel(&self) {
        self.0.store(true, Ordering::Release);
    }

    pub fn is_cancelled(&self) -> bool {
        self.0.load(Ordering::Acquire)
    }
}

pub const fn transactions_supported() -> bool {
    cfg!(any(
        target_os = "linux",
        target_os = "android",
        target_os = "macos"
    ))
}

fn valid_plugin_id(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"._-".contains(&byte))
}

fn plugin_json(bytes: &[u8]) -> Result<serde_json::Value, BridgeError> {
    if bytes.len() > 4 * 1024 * 1024 {
        return Err(BridgeError::new(ErrorKind::OutputTooLarge));
    }
    let value: serde_json::Value =
        serde_json::from_slice(bytes).map_err(|_| BridgeError::new(ErrorKind::InvalidReply))?;
    if value.get("version").and_then(serde_json::Value::as_str) != Some(PROTOCOL_VERSION) {
        return Err(BridgeError::new(ErrorKind::CliVersionMismatch));
    }
    Ok(value)
}

pub fn parse_address(value: &str) -> Result<SocketAddr, BridgeError> {
    let address = value
        .trim()
        .parse()
        .map_err(|_| BridgeError::new(ErrorKind::InvalidAddress))?;
    validate_address(address)?;
    Ok(address)
}

fn validate_address(address: SocketAddr) -> Result<(), BridgeError> {
    let scoped_ipv6 =
        matches!(address, SocketAddr::V6(ipv6) if ipv6.scope_id() != 0 || ipv6.flowinfo() != 0);
    if !address.ip().is_loopback() || address.port() == 0 || scoped_ipv6 {
        Err(BridgeError::new(ErrorKind::InvalidAddress))
    } else {
        Ok(())
    }
}

fn validate_config(path: &Path) -> Result<(), BridgeError> {
    // The frozen CLI uses std::env::args, which requires UTF-8 arguments.
    if !path.is_absolute()
        || path.to_str().is_none()
        || path.as_os_str().as_encoded_bytes().len() > 4096
        || path.as_os_str().as_encoded_bytes().contains(&0)
        || path
            .components()
            .any(|part| matches!(part, Component::ParentDir))
    {
        Err(BridgeError::new(ErrorKind::InvalidConfigPath))
    } else {
        Ok(())
    }
}

#[derive(Clone, Copy)]
struct Limits {
    request: Duration,
    version: Duration,
    cleanup: Duration,
    stdout: usize,
    stderr: usize,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            request: Duration::from_secs(20),
            version: Duration::from_secs(3),
            cleanup: Duration::from_secs(2),
            stdout: STDOUT_LIMIT,
            stderr: STDERR_LIMIT,
        }
    }
}

struct Admission {
    safe_to_release: bool,
    global: bool,
    isolated_poison: Option<Arc<AtomicBool>>,
}

impl Admission {
    fn acquire() -> Result<Self, BridgeError> {
        match ADMISSION.compare_exchange(0, 1, Ordering::AcqRel, Ordering::Acquire) {
            Ok(_) => Ok(Self {
                safe_to_release: true,
                global: true,
                isolated_poison: None,
            }),
            Err(2) => Err(BridgeError::new(ErrorKind::CleanupPending)),
            Err(_) => Err(BridgeError::new(ErrorKind::Busy)),
        }
    }

    // A single Gateway supervisor owns its own bounded probes. Its long-lived
    // child must never hold, reset, or bypass an active short-command admission.
    fn isolated(poison: Arc<AtomicBool>) -> Self {
        Self {
            safe_to_release: true,
            global: false,
            isolated_poison: Some(poison),
        }
    }
}

impl Drop for Admission {
    fn drop(&mut self) {
        if self.global {
            ADMISSION.store(if self.safe_to_release { 0 } else { 2 }, Ordering::Release);
        } else if !self.safe_to_release
            && let Some(poison) = &self.isolated_poison
        {
            poison.store(true, Ordering::Release);
        }
    }
}

#[derive(PartialEq, Eq)]
struct ExecutableStamp {
    len: u64,
    modified: SystemTime,
    #[cfg(unix)]
    device: u64,
    #[cfg(unix)]
    inode: u64,
}

struct Executable {
    path: PathBuf,
    stamp: ExecutableStamp,
}

impl Executable {
    fn open(path: &Path) -> Result<Self, BridgeError> {
        let path = path
            .canonicalize()
            .map_err(|_| BridgeError::new(ErrorKind::CliMissing))?;
        let stamp = executable_stamp(&path)?;
        Ok(Self { path, stamp })
    }

    fn verify_unchanged(&self) -> Result<(), BridgeError> {
        if executable_stamp(&self.path).ok().as_ref() != Some(&self.stamp) {
            Err(BridgeError::new(ErrorKind::CliChanged))
        } else {
            Ok(())
        }
    }
}

fn executable_stamp(path: &Path) -> Result<ExecutableStamp, BridgeError> {
    let metadata = fs::metadata(path).map_err(|_| BridgeError::new(ErrorKind::CliNotExecutable))?;
    if !metadata.is_file() {
        return Err(BridgeError::new(ErrorKind::CliNotExecutable));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if metadata.permissions().mode() & 0o111 == 0 {
            return Err(BridgeError::new(ErrorKind::CliNotExecutable));
        }
    }
    #[cfg(windows)]
    if !path
        .extension()
        .is_some_and(|extension| extension.eq_ignore_ascii_case("exe"))
    {
        return Err(BridgeError::new(ErrorKind::CliNotExecutable));
    }
    let modified = metadata
        .modified()
        .map_err(|_| BridgeError::new(ErrorKind::CliNotExecutable))?;
    #[cfg(unix)]
    use std::os::unix::fs::MetadataExt;
    Ok(ExecutableStamp {
        len: metadata.len(),
        modified,
        #[cfg(unix)]
        device: metadata.dev(),
        #[cfg(unix)]
        inode: metadata.ino(),
    })
}

fn packaged_candidates(desktop: &Path, mac_bundle: bool, windows: bool) -> Vec<PathBuf> {
    let Some(directory) = desktop.parent() else {
        return Vec::new();
    };
    let executable = if windows { "magpie.exe" } else { "magpie" };
    let mut candidates = vec![directory.join(executable)];
    if mac_bundle
        && directory.file_name().is_some_and(|name| name == "MacOS")
        && let Some(contents) = directory
            .parent()
            .filter(|parent| parent.file_name().is_some_and(|name| name == "Contents"))
    {
        candidates.push(contents.join("Helpers").join(executable));
        candidates.push(contents.join("Resources").join("bin").join(executable));
    }
    candidates
}

fn resolve_from(desktop: &Path, explicit: Option<OsString>) -> Result<Executable, BridgeError> {
    if let Some(explicit) = explicit {
        let path = PathBuf::from(explicit);
        if !path.is_absolute() || path.as_os_str().as_encoded_bytes().contains(&0) {
            return Err(BridgeError::new(ErrorKind::CliOverrideInvalid));
        }
        // An invalid explicit override never falls through to another binary.
        return Executable::open(&path);
    }
    for path in packaged_candidates(desktop, cfg!(target_os = "macos"), cfg!(windows)) {
        match fs::symlink_metadata(&path) {
            Ok(_) => return Executable::open(&path),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(_) => return Err(BridgeError::new(ErrorKind::CliNotExecutable)),
        }
    }
    Err(BridgeError::new(ErrorKind::CliMissing))
}

/// Run on the GUI's background executor, retaining the task until it completes.
/// Use `Cancellation::cancel` instead of dropping a running future. Dropping a
/// future kills its child but intentionally locks admission until app restart:
/// it cannot prove that OS cleanup has completed.
pub async fn run(request: Request, cancellation: Cancellation) -> Result<Reply, BridgeError> {
    request.validate()?;
    let admission = Admission::acquire()?;
    if cancellation.is_cancelled() {
        return Err(BridgeError::new(ErrorKind::Cancelled));
    }
    let desktop = env::current_exe().map_err(|_| BridgeError::new(ErrorKind::CliMissing))?;
    let executable = resolve_from(&desktop, env::var_os("MAGPIE_CLI_PATH"))?;
    run_resolved(
        request,
        cancellation,
        executable,
        Limits::default(),
        admission,
    )
    .await
}

async fn run_resolved(
    request: Request,
    cancellation: Cancellation,
    executable: Executable,
    limits: Limits,
    mut admission: Admission,
) -> Result<Reply, BridgeError> {
    request.validate()?;
    let deadline = Instant::now() + limits.request;
    let version = capture(
        &executable,
        &["--version".into()],
        false,
        &cancellation,
        deadline.min(Instant::now() + limits.version),
        limits,
        &mut admission,
    )
    .await?;
    let version_line = version
        .stdout
        .strip_suffix(b"\n")
        .unwrap_or(&version.stdout);
    let version_line = version_line.strip_suffix(b"\r").unwrap_or(version_line);
    if !version.success || !version.stderr.is_empty() || version_line != VERSION_LINE {
        return Err(BridgeError::new(ErrorKind::CliVersionMismatch));
    }

    let output = capture(
        &executable,
        &request.arguments(),
        request.is_mutation(),
        &cancellation,
        deadline,
        limits,
        &mut admission,
    )
    .await?;
    decode(&request, output)
}

// Not Debug: captured bytes can contain malicious output or private values.
struct Captured {
    success: bool,
    stdout: Vec<u8>,
    stderr: Vec<u8>,
}

async fn read_bounded(
    mut pipe: impl AsyncRead + Unpin,
    limit: usize,
) -> Result<Vec<u8>, ErrorKind> {
    let mut output = Vec::with_capacity(limit.min(4096));
    let mut buffer = [0_u8; 4096];
    loop {
        let read = pipe
            .read(&mut buffer)
            .await
            .map_err(|_| ErrorKind::InvalidReply)?;
        if read == 0 {
            return Ok(output);
        }
        if read > limit.saturating_sub(output.len()) {
            return Err(ErrorKind::OutputTooLarge);
        }
        output.extend_from_slice(&buffer[..read]);
    }
}

async fn stop_guard(cancellation: &Cancellation, deadline: Instant) -> Result<Captured, ErrorKind> {
    loop {
        if cancellation.is_cancelled() {
            return Err(ErrorKind::Cancelled);
        }
        if Instant::now() >= deadline {
            return Err(ErrorKind::TimedOut);
        }
        Timer::at(deadline.min(Instant::now() + Duration::from_millis(10))).await;
    }
}

async fn capture(
    executable: &Executable,
    arguments: &[OsString],
    mutation: bool,
    cancellation: &Cancellation,
    deadline: Instant,
    limits: Limits,
    admission: &mut Admission,
) -> Result<Captured, BridgeError> {
    if cancellation.is_cancelled() {
        return Err(BridgeError::new(ErrorKind::Cancelled));
    }
    if Instant::now() >= deadline {
        return Err(BridgeError::new(ErrorKind::TimedOut));
    }
    executable.verify_unchanged()?;
    let mut native = std::process::Command::new(&executable.path);
    native.args(arguments);
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        native.process_group(0);
    }
    let mut command = Command::from(native);
    command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .reap_on_drop(true);
    #[cfg(windows)]
    {
        use async_process::windows::CommandExt;
        // CREATE_NO_WINDOW: a packaged GUI action must not flash a console.
        command.creation_flags(0x0800_0000);
    }
    let mut child = command
        .spawn()
        .map_err(|_| BridgeError::new(ErrorKind::LaunchFailed))?;
    admission.safe_to_release = false;
    let stdout = child.stdout.take();
    let stderr = child.stderr.take();
    let result = {
        // Dropping this future closes Unix async pipes. There are no reader
        // threads to join if a descendant keeps its inherited writer open.
        let collect = async {
            let stdout = stdout.ok_or(ErrorKind::InvalidReply)?;
            let stderr = stderr.ok_or(ErrorKind::InvalidReply)?;
            let (stdout, stderr) = future::try_zip(
                read_bounded(stdout, limits.stdout),
                read_bounded(stderr, limits.stderr),
            )
            .await?;
            let status = child.status().await.map_err(|_| ErrorKind::InvalidReply)?;
            Ok(Captured {
                success: status.success(),
                stdout,
                stderr,
            })
        };
        future::race(collect, stop_guard(cancellation, deadline)).await
    };
    match result {
        Ok(output) => {
            admission.safe_to_release = true; // status() reaped this direct child.
            Ok(output)
        }
        Err(kind) => {
            #[cfg(unix)]
            if let Some(group) = i32::try_from(child.id())
                .ok()
                .and_then(rustix::process::Pid::from_raw)
            {
                // The child was placed in its own process group before exec.
                // Stop ordinary descendants too, without invoking a shell.
                let _ = rustix::process::kill_process_group(group, rustix::process::Signal::KILL);
            }
            let _ = child.kill(); // Already-exited children are reaped below too.
            let reaped = future::race(async { child.status().await.is_ok() }, async {
                Timer::after(limits.cleanup).await;
                false
            })
            .await;
            // On Windows async-process uses blocking::Unblock for pipes. A
            // descendant may retain a writer after direct-child termination.
            // Never admit another request after an aborted capture there: at
            // most this request's two blocking reads can remain outstanding.
            // A native overlapped-pipe backend is needed for retry without a
            // restart on Windows. Successful captures are unaffected.
            admission.safe_to_release = reaped && !cfg!(windows);
            Err(BridgeError::after_spawn(kind, mutation))
        }
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct InspectWire {
    version: String,
    gateway: GatewayWire,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct GatewayWire {
    address: String,
    mode: String,
    provider_health: String,
    models: Vec<ModelWire>,
    #[serde(default)]
    server_version: Option<String>,
    #[serde(default)]
    instance_id: Option<String>,
    #[serde(default)]
    config_source: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ModelWire {
    id: String,
    source: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ErrorWire {
    version: String,
    error: ErrorDetailWire,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ErrorDetailWire {
    code: String,
    message: String,
}

fn catalog_model(model: &str) -> bool {
    let Some((provider, name)) = model.split_once('/') else {
        return false;
    };
    !provider.is_empty()
        && !name.is_empty()
        && model.len() <= 512
        && provider.len() <= 128
        && provider
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"._-".contains(&byte))
        && !model
            .chars()
            .any(|ch| ch.is_whitespace() || ch.is_control())
        && !name.bytes().any(|byte| b"\\\"'?#".contains(&byte))
}

fn identity_field(value: &Option<String>) -> bool {
    value.as_ref().is_none_or(|value| {
        !value.is_empty()
            && value.len() <= 128
            && value
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || b"._-+".contains(&byte))
    })
}

fn decode(request: &Request, captured: Captured) -> Result<Reply, BridgeError> {
    let error = |kind| BridgeError::after_spawn(kind, request.is_mutation());
    if !captured.success {
        if !captured.stdout.is_empty() {
            return Err(error(ErrorKind::InvalidReply));
        }
        let wire: ErrorWire =
            serde_json::from_slice(&captured.stderr).map_err(|_| error(ErrorKind::CliRejected))?;
        if wire.version != PROTOCOL_VERSION {
            return Err(error(ErrorKind::CliVersionMismatch));
        }
        return Err(error(classify_error(&wire.error)));
    }
    if !captured.stderr.is_empty() {
        return Err(error(ErrorKind::InvalidReply));
    }
    match request {
        Request::PluginToggle { .. } => {
            let value = plugin_json(&captured.stdout).map_err(|e| error(e.kind))?;
            Ok(Reply::Plugins(value))
        }
        Request::PluginCall {
            owner, operation, ..
        } => {
            if captured.stdout.len() > 4 * 1024 * 1024 {
                return Err(error(ErrorKind::OutputTooLarge));
            }
            #[derive(Deserialize)]
            #[serde(deny_unknown_fields)]
            struct PluginCallWire {
                version: String,
                plugin_id: String,
                operation: String,
                result: serde_json::Value,
            }
            let wire: PluginCallWire = serde_json::from_slice(&captured.stdout)
                .map_err(|_| error(ErrorKind::InvalidReply))?;
            if wire.version != PROTOCOL_VERSION {
                return Err(error(ErrorKind::CliVersionMismatch));
            }
            if wire.plugin_id != *owner || wire.operation != *operation {
                return Err(error(ErrorKind::InvalidReply));
            }
            Ok(Reply::PluginResult(wire.result))
        }
        Request::RefreshProvider { id, revision, .. } => {
            #[derive(Deserialize)]
            #[serde(deny_unknown_fields)]
            struct Refreshed {
                version: String,
                operation: String,
                provider_id: String,
                revision: u64,
            }
            let wire: Refreshed = serde_json::from_slice(&captured.stdout)
                .map_err(|_| error(ErrorKind::InvalidReply))?;
            if wire.version != PROTOCOL_VERSION
                || wire.operation != "providers.refresh"
                || &wire.provider_id != id
                || wire.revision != revision + 1
            {
                return Err(error(ErrorKind::InvalidReply));
            }
            Ok(Reply::ProviderRefreshed {
                provider_id: wire.provider_id,
                revision: wire.revision,
            })
        }
        Request::Inspect { address } => {
            let wire: InspectWire = serde_json::from_slice(&captured.stdout)
                .map_err(|_| error(ErrorKind::InvalidReply))?;
            if wire.version != PROTOCOL_VERSION {
                return Err(error(ErrorKind::CliVersionMismatch));
            }
            let actual =
                parse_address(&wire.gateway.address).map_err(|_| error(ErrorKind::InvalidReply))?;
            if actual != *address
                || wire.gateway.mode != "forward_candidate"
                || wire.gateway.provider_health != "not_probed"
                || wire.gateway.models.len() > MAX_MODELS
                || !identity_field(&wire.gateway.server_version)
                || !identity_field(&wire.gateway.instance_id)
                || !identity_field(&wire.gateway.config_source)
            {
                return Err(error(ErrorKind::InvalidReply));
            }
            let mut seen = HashSet::with_capacity(wire.gateway.models.len());
            for model in &wire.gateway.models {
                if model.source != "configured"
                    || !catalog_model(&model.id)
                    || !seen.insert(&model.id)
                {
                    return Err(error(ErrorKind::InvalidReply));
                }
            }
            Ok(Reply::Inspected(GatewayInfo {
                address: actual,
                server_version: wire.gateway.server_version,
                instance_id: wire.gateway.instance_id,
                config_source: wire.gateway.config_source,
                models: wire
                    .gateway
                    .models
                    .into_iter()
                    .map(|model| ModelInfo { id: model.id })
                    .collect(),
            }))
        }
    }
}

fn classify_error(error: &ErrorDetailWire) -> ErrorKind {
    match error.code.as_str() {
        "gateway_inspect_failed" => ErrorKind::GatewayUnavailable,
        "gateway_probe_failed" if error.message == "gateway_model_not_configured" => {
            ErrorKind::ModelUnavailable
        }
        "gateway_probe_failed" => ErrorKind::GatewayUnavailable,
        "config_conflict" => ErrorKind::ConfigConflict,
        "config_not_a_file"
        | "config_symlink"
        | "config_too_large"
        | "config_invalid_utf8"
        | "config_read_error"
        | "config_parse_error" => ErrorKind::ConfigInvalid,
        "config_transaction_error" => match error.message.as_str() {
            "configuration or containing directory is read-only" => ErrorKind::ConfigReadOnly,
            "configuration changed since it was read; no stale snapshot was applied"
            | "another transaction or a stale cooperative lock blocks this operation"
            | "an unowned Magpie provider already exists; explicit reconciliation is required" => {
                ErrorKind::ConfigConflict
            }
            "a pending transaction requires recovery; keep the private recovery files"
            | "target replacement may have completed; inspect recovery status before retrying"
            | "private transaction state is invalid or does not belong to this target"
            | "private transaction storage limit reached; review retained recovery files" => {
                ErrorKind::RecoveryRequired
            }
            "this configuration is already managed; disconnect before connecting again" => {
                ErrorKind::AlreadyManaged
            }
            "no active Magpie ownership record exists for this configuration" => {
                ErrorKind::NotManaged
            }
            "secure native configuration mutation is not implemented on this platform" => {
                ErrorKind::PlatformUnsupported
            }
            "configuration path or parent directory is unsafe"
            | "configuration parent directory does not exist"
            | "configuration or recovery image is not a regular single-link file"
            | "symlink configuration paths or recovery files are refused"
            | "configuration exceeds the transaction size limit"
            | "configuration is not valid UTF-8"
            | "configuration is not valid TOML; original bytes were preserved"
            | "configuration or recovery state could not be read"
            | "a managed configuration field has an unsupported TOML shape" => {
                ErrorKind::ConfigInvalid
            }
            _ => ErrorKind::CliRejected,
        },
        _ => ErrorKind::CliRejected,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{Value, json};

    fn address() -> SocketAddr {
        "127.0.0.1:3425".parse().expect("test address")
    }

    fn config() -> PathBuf {
        if cfg!(windows) {
            PathBuf::from(r"C:\isolated-fixture\config.toml")
        } else {
            PathBuf::from("/isolated-fixture/config.toml")
        }
    }

    fn plugin_call(mutation: bool, operation: &str) -> Request {
        Request::PluginCall {
            owner: "agent.fixture".into(),
            operation: operation.into(),
            arguments: vec!["--model".into(), "local/model".into()],
            mutation,
        }
    }

    fn plugin_envelope(operation: &str) -> Value {
        json!({"version":"0.2.0", "plugin_id":"agent.fixture", "operation":operation,
            "result":{"selection":{"model":"local/model"},"metadataInput":true}})
    }

    fn gateway_json() -> Value {
        json!({
            "version":"0.2.0",
            "gateway":{
                "address":"127.0.0.1:3425", "mode":"forward_candidate",
                "provider_health":"not_probed", "models":[{"id":"local/model","source":"configured"}]
            }
        })
    }

    fn parse_ok(request: &Request, value: Value) -> Result<Reply, BridgeError> {
        decode(
            request,
            Captured {
                success: true,
                stdout: serde_json::to_vec(&value).expect("fixture JSON"),
                stderr: vec![],
            },
        )
    }

    #[test]
    fn provider_refresh_reply_is_bound_to_provider_and_next_revision() {
        let request = Request::RefreshProvider {
            store: config(),
            id: "fixture".into(),
            revision: 12,
        };
        let good = json!({"version":"0.2.0","operation":"providers.refresh","provider_id":"fixture","revision":13});
        assert!(matches!(
            parse_ok(&request, good.clone()),
            Ok(Reply::ProviderRefreshed { revision: 13, .. })
        ));
        for (key, value) in [
            ("provider_id", json!("other")),
            ("revision", json!(12)),
            ("operation", json!("providers.update")),
        ] {
            let mut bad = good.clone();
            bad[key] = value;
            assert!(parse_ok(&request, bad).is_err());
        }
        assert!(request.is_mutation());
        assert_eq!(request.arguments()[1], "refresh");
    }

    #[test]
    fn accepts_only_literal_loopback_and_nonzero_ports() {
        for valid in [
            "127.0.0.1:1",
            "127.2.3.4:65535",
            "[::1]:3425",
            " 127.0.0.1:3425 ",
        ] {
            assert!(parse_address(valid).is_ok(), "{valid}");
        }
        for invalid in [
            "localhost:3425",
            "https://127.0.0.1:3425",
            "127.0.0.1:0",
            "0.0.0.0:3425",
            "192.168.1.1:3425",
            "[::]:3425",
            "[::ffff:127.0.0.1]:3425",
            "[::1%5]:3425",
            "127.0.0.1:3425/path",
            "127.0.0.1:3425\n--apply",
        ] {
            assert_eq!(
                parse_address(invalid).expect_err(invalid).kind,
                ErrorKind::InvalidAddress
            );
        }
        assert!(
            validate_address(SocketAddr::V6(std::net::SocketAddrV6::new(
                std::net::Ipv6Addr::LOCALHOST,
                3425,
                1,
                0
            )))
            .is_err()
        );
    }

    #[test]
    fn config_arguments_must_be_explicit_absolute_utf8_paths() {
        assert!(validate_config(&config()).is_ok());
        assert!(validate_config(Path::new("config.toml")).is_err());
        assert!(validate_config(&config().join("..").join("other.toml")).is_err());
        assert!(validate_config(Path::new("/tmp/\0config.toml")).is_err());
        assert!(validate_config(&config().join("x".repeat(4097))).is_err());
        #[cfg(unix)]
        {
            use std::os::unix::ffi::OsStringExt;
            assert!(
                validate_config(&PathBuf::from(OsString::from_vec(b"/tmp/\xff".to_vec()))).is_err()
            );
        }
    }

    #[test]
    fn argv_preserves_literal_paths_and_model_values_without_shell() {
        let path = if cfg!(windows) {
            r"C:\isolated path\$(literal).toml"
        } else {
            "/isolated path/$(literal).toml"
        };
        let request = Request::PluginCall {
            owner: "agent.fixture".into(),
            operation: "preview".into(),
            arguments: vec![
                "--config".into(),
                path.into(),
                "--model".into(),
                "a/model;$(literal)".into(),
            ],
            mutation: false,
        };
        assert_eq!(
            request.arguments(),
            [
                "plugins",
                "call",
                "agent.fixture",
                "preview",
                "--json",
                "--config",
                path,
                "--model",
                "a/model;$(literal)"
            ]
            .map(OsString::from)
        );
        assert!(!request.is_mutation());
        assert!(plugin_call(true, "connect").is_mutation());
        assert!(plugin_call(true, "disconnect").is_mutation());
    }

    #[test]
    fn packaged_paths_never_use_search_path() {
        assert_eq!(
            packaged_candidates(Path::new("/opt/Magpie/magpie-desktop"), false, false),
            vec![PathBuf::from("/opt/Magpie/magpie")]
        );
        assert_eq!(
            packaged_candidates(Path::new("/app/magpie-desktop.exe"), false, true),
            vec![PathBuf::from("/app/magpie.exe")]
        );
        assert_eq!(
            packaged_candidates(
                Path::new("/Applications/Magpie.app/Contents/MacOS/magpie-desktop"),
                true,
                false
            ),
            vec![
                PathBuf::from("/Applications/Magpie.app/Contents/MacOS/magpie"),
                PathBuf::from("/Applications/Magpie.app/Contents/Helpers/magpie"),
                PathBuf::from("/Applications/Magpie.app/Contents/Resources/bin/magpie"),
            ]
        );
        assert_eq!(
            resolve_from(Path::new("/unused/desktop"), Some("magpie".into()))
                .err()
                .expect("relative override")
                .kind,
            ErrorKind::CliOverrideInvalid
        );
    }

    #[test]
    fn validates_inspect_and_allows_unicode_catalog_search() {
        let request = Request::Inspect { address: address() };
        let mut value = gateway_json();
        value["gateway"]["models"] = json!([
            {"id":"local/中文模型","source":"configured"},
            {"id":"local/model","source":"configured"}
        ]);
        let Reply::Inspected(gateway) = parse_ok(&request, value).expect("unicode catalog") else {
            panic!("wrong reply")
        };
        assert_eq!(gateway.models[0].id, "local/中文模型");
        assert_eq!(gateway.address, address());
        let mut empty = gateway_json();
        empty["gateway"]["models"] = json!([]);
        assert!(parse_ok(&request, empty).is_ok());
    }

    #[test]
    fn rejects_wrong_inspect_address_mode_health_source_and_version() {
        let request = Request::Inspect { address: address() };
        for (field, value) in [
            ("address", "127.0.0.1:3426"),
            ("address", "localhost:3425"),
            ("mode", "stub"),
            ("provider_health", "healthy"),
        ] {
            let mut wire = gateway_json();
            wire["gateway"][field] = value.into();
            assert_eq!(
                parse_ok(&request, wire).expect_err(field).kind,
                ErrorKind::InvalidReply
            );
        }
        let mut wire = gateway_json();
        wire["gateway"]["models"][0]["source"] = "discovered".into();
        assert_eq!(
            parse_ok(&request, wire).expect_err("source").kind,
            ErrorKind::InvalidReply
        );
        let mut wire = gateway_json();
        wire["version"] = "0.3.0".into();
        assert_eq!(
            parse_ok(&request, wire).expect_err("version").kind,
            ErrorKind::CliVersionMismatch
        );
    }

    #[test]
    fn rejects_duplicate_catalog_ids_invalid_ids_and_large_catalogs() {
        let request = Request::Inspect { address: address() };
        let mut wire = gateway_json();
        wire["gateway"]["models"] =
            json!([{"id":"p/m","source":"configured"},{"id":"p/m","source":"configured"}]);
        assert_eq!(
            parse_ok(&request, wire).expect_err("duplicate IDs").kind,
            ErrorKind::InvalidReply
        );
        for invalid in [
            "missing-provider",
            "p/m\nsecret",
            "p/m?key=secret",
            "p/",
            "/m",
        ] {
            let mut wire = gateway_json();
            wire["gateway"]["models"][0]["id"] = invalid.into();
            assert_eq!(
                parse_ok(&request, wire).expect_err("ID").kind,
                ErrorKind::InvalidReply
            );
        }
        let mut wire = gateway_json();
        wire["gateway"]["models"] = Value::Array(
            (0..=MAX_MODELS)
                .map(|i| json!({"id":format!("p/{i}"),"source":"configured"}))
                .collect(),
        );
        assert_eq!(
            parse_ok(&request, wire).expect_err("bounded models").kind,
            ErrorKind::InvalidReply
        );
    }

    #[test]
    fn generic_plugin_envelope_is_bound_to_owner_operation_and_version() {
        for mutation in [false, true] {
            let request = plugin_call(mutation, "metadataInput");
            let good = plugin_envelope("metadataInput");
            assert_eq!(
                parse_ok(&request, good.clone()).unwrap(),
                Reply::PluginResult(good["result"].clone())
            );
            for (field, value, kind) in [
                ("plugin_id", "agent.other", ErrorKind::InvalidReply),
                ("operation", "other", ErrorKind::InvalidReply),
                ("version", "0.1.9", ErrorKind::CliVersionMismatch),
            ] {
                let mut wrong = good.clone();
                wrong[field] = json!(value);
                let error = parse_ok(&request, wrong).unwrap_err();
                assert_eq!(error.kind, kind, "{field}");
                assert_eq!(error.mutation_may_have_run, mutation);
            }
        }
    }

    #[test]
    fn generic_plugin_envelopes_bound_bytes_and_reject_ambiguous_metadata_without_raw_diagnostics()
    {
        let secret = "PRIVATE_METADATA_INPUT_CANARY";
        for mutation in [false, true] {
            let request = plugin_call(mutation, "metadataInput");
            let mut duplicate_owner = plugin_envelope("metadataInput").to_string();
            duplicate_owner.insert_str(1, "\"plugin_id\":\"agent.other\",");
            let mut duplicate_operation = plugin_envelope("metadataInput").to_string();
            duplicate_operation.insert_str(1, "\"operation\":\"other\",");
            let mut duplicate_version = plugin_envelope("metadataInput").to_string();
            duplicate_version.insert_str(1, "\"version\":\"0.1.9\",");
            let mut raw_input = plugin_envelope("metadataInput");
            raw_input["raw_config"] = json!(secret);
            let mut missing_result = plugin_envelope("metadataInput");
            missing_result.as_object_mut().unwrap().remove("result");
            let mut wrong_owner_type = plugin_envelope("metadataInput");
            wrong_owner_type["plugin_id"] = json!({"raw":secret});
            let mut deep = plugin_envelope("metadataInput");
            let mut nested = json!(secret);
            for _ in 0..130 {
                nested = json!([nested]);
            }
            deep["result"] = nested;
            for bytes in [
                duplicate_owner.into_bytes(),
                duplicate_operation.into_bytes(),
                duplicate_version.into_bytes(),
                raw_input.to_string().into_bytes(),
                missing_result.to_string().into_bytes(),
                wrong_owner_type.to_string().into_bytes(),
                deep.to_string().into_bytes(),
                b"{} {}".to_vec(),
                vec![0xff],
            ] {
                let error = decode(
                    &request,
                    Captured {
                        success: true,
                        stdout: bytes,
                        stderr: vec![],
                    },
                )
                .unwrap_err();
                assert_eq!(error.kind, ErrorKind::InvalidReply);
                assert_eq!(error.mutation_may_have_run, mutation);
                assert!(!format!("{error:?} {error}").contains(secret));
            }
            let good = serde_json::to_vec(&plugin_envelope("metadataInput")).unwrap();
            let mut exact = vec![b' '; 4 * 1024 * 1024 - good.len()];
            exact.extend(good);
            assert!(
                decode(
                    &request,
                    Captured {
                        success: true,
                        stdout: exact.clone(),
                        stderr: vec![]
                    }
                )
                .is_ok()
            );
            exact.push(b' ');
            let error = decode(
                &request,
                Captured {
                    success: true,
                    stdout: exact,
                    stderr: vec![],
                },
            )
            .unwrap_err();
            assert_eq!(error.kind, ErrorKind::OutputTooLarge);
            assert_eq!(error.mutation_may_have_run, mutation);
        }
    }

    #[test]
    fn redacts_untrusted_diagnostics_and_preserves_mutation_uncertainty() {
        let secret = "secret-fixture-canary-api-key-123";
        for request in [
            plugin_call(false, "selection"),
            plugin_call(true, "connect"),
            plugin_call(true, "disconnect"),
        ] {
            let stderr = serde_json::to_vec(
                &json!({"version":"0.2.0","error":{"code":"untrusted_code","message":secret}}),
            )
            .expect("error fixture");
            let error = decode(
                &request,
                Captured {
                    success: false,
                    stdout: vec![],
                    stderr,
                },
            )
            .expect_err("CLI error");
            assert_eq!(error.kind, ErrorKind::CliRejected);
            assert_eq!(error.mutation_may_have_run, request.is_mutation());
            assert!(!format!("{error:?} {error}").contains(secret));
            assert!(!error.code().contains("untrusted"));
        }
        let error = decode(
            &plugin_call(false, "selection"),
            Captured {
                success: false,
                stdout: vec![],
                stderr: secret.as_bytes().to_vec(),
            },
        )
        .expect_err("not JSON");
        assert!(!format!("{error:?} {error}").contains(secret));
    }

    #[test]
    fn static_error_mapping_handles_readonly_drift_missing_models_and_recovery() {
        for (code, message, expected) in [
            (
                "config_transaction_error",
                "configuration or containing directory is read-only",
                ErrorKind::ConfigReadOnly,
            ),
            (
                "config_transaction_error",
                "configuration changed since it was read; no stale snapshot was applied",
                ErrorKind::ConfigConflict,
            ),
            (
                "gateway_probe_failed",
                "gateway_model_not_configured",
                ErrorKind::ModelUnavailable,
            ),
            (
                "gateway_inspect_failed",
                "arbitrary text is discarded",
                ErrorKind::GatewayUnavailable,
            ),
            (
                "config_transaction_error",
                "target replacement may have completed; inspect recovery status before retrying",
                ErrorKind::RecoveryRequired,
            ),
        ] {
            assert_eq!(
                classify_error(&ErrorDetailWire {
                    code: code.into(),
                    message: message.into()
                }),
                expected
            );
        }
    }

    #[test]
    fn never_accepts_error_exit_with_success_stdout_or_success_with_stderr() {
        for (success, stdout, stderr) in [
            (false, b"{}".to_vec(), b"{}".to_vec()),
            (
                true,
                serde_json::to_vec(&plugin_envelope("selection")).expect("fixture"),
                b"warning: private value".to_vec(),
            ),
        ] {
            assert_eq!(
                decode(
                    &plugin_call(false, "selection"),
                    Captured {
                        success,
                        stdout,
                        stderr
                    }
                )
                .expect_err("channels")
                .kind,
                ErrorKind::InvalidReply
            );
        }
    }

    #[cfg(unix)]
    mod processes {
        use super::*;
        use std::os::unix::fs::PermissionsExt;

        struct Fixture {
            directory: PathBuf,
            executable: PathBuf,
        }

        impl Fixture {
            fn new() -> Self {
                let nonce = SystemTime::now()
                    .duration_since(SystemTime::UNIX_EPOCH)
                    .expect("clock")
                    .as_nanos();
                let directory = env::temp_dir().join(format!(
                    "magpie-ui-bridge-test-{}-{nonce}",
                    std::process::id()
                ));
                fs::create_dir(&directory).expect("isolated fixture directory");
                fs::set_permissions(&directory, fs::Permissions::from_mode(0o700))
                    .expect("private fixture");
                let source = directory.join("fixture.rs");
                let executable = directory.join("magpie");
                fs::write(&source, FIXTURE_SOURCE).expect("fixture source");
                let compiled = std::process::Command::new(
                    env::var_os("RUSTC").unwrap_or_else(|| "rustc".into()),
                )
                .arg("--edition=2024")
                .arg("-O")
                .arg(&source)
                .arg("-o")
                .arg(&executable)
                .output()
                .expect("Rust compiler for isolated CLI fixture");
                assert!(
                    compiled.status.success(),
                    "fixture compilation: {}",
                    String::from_utf8_lossy(&compiled.stderr)
                );
                Self {
                    directory,
                    executable,
                }
            }

            fn mode(&self, mode: &str) {
                fs::write(self.directory.join("mode"), mode).expect("fixture mode");
                let _ = fs::remove_file(self.directory.join("operations"));
                let _ = fs::remove_file(self.directory.join("pid"));
            }

            fn operations(&self) -> String {
                fs::read_to_string(self.directory.join("operations")).unwrap_or_default()
            }

            fn request(
                &self,
                request: Request,
                cancellation: Cancellation,
                limits: Limits,
            ) -> impl std::future::Future<Output = Result<Reply, BridgeError>> {
                let executable = Executable::open(&self.executable).expect("fixture executable");
                async move {
                    let admission = Admission::acquire()?;
                    run_resolved(request, cancellation, executable, limits, admission).await
                }
            }

            fn assert_direct_child_reaped(&self) {
                let Ok(pid) = fs::read_to_string(self.directory.join("pid")) else {
                    return;
                };
                let exists = std::process::Command::new("/bin/kill")
                    .args(["-0", pid.trim()])
                    .stdin(Stdio::null())
                    .stdout(Stdio::null())
                    .stderr(Stdio::null())
                    .status()
                    .expect("read-only child existence probe")
                    .success();
                assert!(
                    !exists,
                    "direct child must be killed and reaped before returning"
                );
            }
        }

        impl Drop for Fixture {
            fn drop(&mut self) {
                let _ = fs::remove_dir_all(&self.directory);
            }
        }

        fn limits() -> Limits {
            Limits {
                request: Duration::from_millis(180),
                version: Duration::from_millis(120),
                cleanup: Duration::from_millis(300),
                stdout: 4096,
                stderr: 512,
            }
        }

        // All child-process cases live in one test so the application-wide
        // admission gate is exercised without test runner races or env changes.
        #[test]
        fn real_isolated_process_protocol_deadline_cancel_output_and_reaping() {
            let _guard = PROCESS_TEST_LOCK
                .lock()
                .unwrap_or_else(|error| error.into_inner());
            let fixture = Fixture::new();
            future::block_on(async {
                fixture.mode("ok");
                let packaged = resolve_from(&fixture.directory.join("magpie-desktop"), None)
                    .expect("sibling CLI");
                assert_eq!(packaged.path, fixture.executable);
                assert_eq!(
                    resolve_from(
                        &fixture.directory.join("magpie-desktop"),
                        Some(fixture.directory.join("missing").into_os_string())
                    )
                    .err()
                    .expect("explicit missing override")
                    .kind,
                    ErrorKind::CliMissing
                );
                for request in [
                    Request::Inspect { address: address() },
                    plugin_call(false, "preview"),
                    plugin_call(true, "connect"),
                    plugin_call(false, "selection"),
                    plugin_call(true, "disconnect"),
                ] {
                    let reply = fixture
                        .request(request.clone(), Cancellation::new(), limits())
                        .await;
                    assert!(reply.is_ok(), "fixture operation {request:?}: {reply:?}");
                    fixture.assert_direct_child_reaped();
                }
                let operations = fixture.operations();
                assert!(operations.contains("preview"));
                assert!(operations.contains("connect"));
                assert!(operations.contains("selection"));
                assert!(operations.contains("disconnect"));

                fixture.mode("ok");
                fixture
                    .request(
                        plugin_call(false, "selection"),
                        Cancellation::new(),
                        limits(),
                    )
                    .await
                    .expect("offline status");
                fixture
                    .request(
                        plugin_call(true, "disconnect"),
                        Cancellation::new(),
                        limits(),
                    )
                    .await
                    .expect("offline disconnect");
                assert!(!fixture.operations().contains("inspect"));

                fixture.mode("wrong-version");
                let error = fixture
                    .request(plugin_call(true, "connect"), Cancellation::new(), limits())
                    .await
                    .expect_err("wrong version");
                assert_eq!(error.kind, ErrorKind::CliVersionMismatch);
                assert!(!error.mutation_may_have_run);
                assert!(fixture.operations().is_empty());

                fixture.mode("changed-executable");
                assert_eq!(
                    fixture
                        .request(
                            plugin_call(false, "selection"),
                            Cancellation::new(),
                            limits()
                        )
                        .await
                        .expect_err("identity changed")
                        .kind,
                    ErrorKind::CliChanged
                );
                assert!(fixture.operations().is_empty());

                fixture.mode("missing-model");
                let Reply::Inspected(gateway) = fixture
                    .request(
                        Request::Inspect { address: address() },
                        Cancellation::new(),
                        limits(),
                    )
                    .await
                    .expect("reported catalog")
                else {
                    panic!("inspect reply")
                };
                assert_eq!(
                    gateway.models,
                    vec![ModelInfo {
                        id: "local/removed".into()
                    }]
                );
                assert!(!fixture.operations().contains("plugins"));

                for mode in ["oversized-stdout", "oversized-stderr"] {
                    fixture.mode(mode);
                    let start = Instant::now();
                    let error = fixture
                        .request(
                            plugin_call(false, "selection"),
                            Cancellation::new(),
                            limits(),
                        )
                        .await
                        .expect_err(mode);
                    assert_eq!(error.kind, ErrorKind::OutputTooLarge);
                    assert!(start.elapsed() < Duration::from_secs(1));
                    fixture.assert_direct_child_reaped();
                }

                for mode in ["sleep", "inherited-pipe", "escaped-inherited-pipe"] {
                    fixture.mode(mode);
                    let start = Instant::now();
                    let error = fixture
                        .request(
                            plugin_call(false, "selection"),
                            Cancellation::new(),
                            limits(),
                        )
                        .await
                        .expect_err(mode);
                    assert_eq!(error.kind, ErrorKind::TimedOut);
                    assert!(!error.mutation_may_have_run);
                    assert!(
                        start.elapsed() < Duration::from_millis(650),
                        "deadline must not wait for descendant-held stdout/stderr"
                    );
                    fixture.assert_direct_child_reaped();
                    eprintln!(
                        "bounded fixture {mode}: {} ms; direct child reaped",
                        start.elapsed().as_millis()
                    );
                }

                fixture.mode("sleep");
                let cancellation = Cancellation::new();
                let cancel = cancellation.clone();
                let start = Instant::now();
                let (result, ()) = future::zip(
                    fixture.request(
                        plugin_call(false, "selection"),
                        cancellation,
                        Limits {
                            request: Duration::from_secs(3),
                            ..limits()
                        },
                    ),
                    async {
                        Timer::after(Duration::from_millis(60)).await;
                        cancel.cancel();
                    },
                )
                .await;
                assert_eq!(
                    result.expect_err("cancelled child").kind,
                    ErrorKind::Cancelled
                );
                assert!(start.elapsed() < Duration::from_millis(650));
                fixture.assert_direct_child_reaped();
                eprintln!(
                    "bounded fixture cancellation: {} ms; direct child reaped",
                    start.elapsed().as_millis()
                );

                for request in [
                    plugin_call(true, "connect"),
                    plugin_call(true, "disconnect"),
                ] {
                    fixture.mode("mutation-sleep");
                    let error = fixture
                        .request(request, Cancellation::new(), limits())
                        .await
                        .expect_err("write deadline");
                    assert_eq!(error.kind, ErrorKind::TimedOut);
                    assert!(
                        error.mutation_may_have_run,
                        "a timed out write must not promise no changes"
                    );
                    fixture.assert_direct_child_reaped();
                }

                fixture.mode("mutation-sleep");
                let cancellation = Cancellation::new();
                let cancel = cancellation.clone();
                let (result, ()) = future::zip(
                    fixture.request(
                        plugin_call(true, "connect"),
                        cancellation,
                        Limits {
                            request: Duration::from_secs(3),
                            ..limits()
                        },
                    ),
                    async {
                        Timer::after(Duration::from_millis(100)).await;
                        cancel.cancel();
                    },
                )
                .await;
                assert!(result.expect_err("cancelled write").mutation_may_have_run);
                fixture.assert_direct_child_reaped();

                fixture.mode("slow-version");
                let error = fixture
                    .request(plugin_call(true, "connect"), Cancellation::new(), limits())
                    .await
                    .expect_err("version deadline");
                assert_eq!(error.kind, ErrorKind::TimedOut);
                assert!(!error.mutation_may_have_run);
                fixture.assert_direct_child_reaped();

                fixture.mode("invalid-json");
                assert_eq!(
                    fixture
                        .request(
                            plugin_call(false, "selection"),
                            Cancellation::new(),
                            limits()
                        )
                        .await
                        .expect_err("invalid JSON")
                        .kind,
                    ErrorKind::InvalidReply
                );
                fixture.mode("secret-error");
                let error = fixture
                    .request(
                        plugin_call(false, "selection"),
                        Cancellation::new(),
                        limits(),
                    )
                    .await
                    .expect_err("secret diagnostic");
                assert_eq!(error.kind, ErrorKind::CliRejected);
                assert!(!format!("{error:?} {error}").contains("PRIVATE_CANARY"));

                fixture.mode("sleep");
                let cancellation = Cancellation::new();
                let cancel = cancellation.clone();
                let (result, second) = future::zip(
                    fixture.request(
                        plugin_call(false, "selection"),
                        cancellation,
                        Limits {
                            request: Duration::from_secs(3),
                            ..limits()
                        },
                    ),
                    async {
                        Timer::after(Duration::from_millis(40)).await;
                        let second = fixture
                            .request(
                                plugin_call(false, "selection"),
                                Cancellation::new(),
                                limits(),
                            )
                            .await;
                        cancel.cancel();
                        second
                    },
                )
                .await;
                assert_eq!(
                    second.expect_err("duplicate admission").kind,
                    ErrorKind::Busy
                );
                assert_eq!(result.expect_err("cancel first").kind, ErrorKind::Cancelled);
                fixture.assert_direct_child_reaped();

                fixture.mode("ok");
                fixture
                    .request(
                        plugin_call(false, "selection"),
                        Cancellation::new(),
                        limits(),
                    )
                    .await
                    .expect("retry after reaping");
                let cancelled = Cancellation::new();
                cancelled.cancel();
                fixture.mode("ok");
                assert_eq!(
                    fixture
                        .request(plugin_call(true, "connect"), cancelled, limits())
                        .await
                        .expect_err("pre-cancel")
                        .kind,
                    ErrorKind::Cancelled
                );
                assert!(fixture.operations().is_empty());

                // The deliberately escaped synthetic descendant has a fixed
                // 800ms lifetime; it cannot outlive this isolated test cleanup.
                Timer::after(Duration::from_millis(850)).await;
            });
        }

        const FIXTURE_SOURCE: &str = r###"
use std::{env,fs,io::Write,process::{Command,Stdio},thread,time::Duration};
fn main() {
 let args:Vec<String>=env::args().skip(1).collect();
 if args.first().map(String::as_str)==Some("--hold") {thread::sleep(Duration::from_millis(800));return;}
 let executable=env::current_exe().unwrap();
 let root=executable.parent().unwrap();
 let mode=fs::read_to_string(root.join("mode")).unwrap();
 if args==["--version"] {
   if mode=="slow-version" {fs::write(root.join("pid"),std::process::id().to_string()).unwrap();thread::sleep(Duration::from_secs(3));}
   if mode=="changed-executable" {let copy=root.join("replacement");fs::copy(&executable,&copy).unwrap();fs::rename(copy,&executable).unwrap();}
   println!("magpie {}",if mode=="wrong-version" {"0.1.9"} else {"0.2.0"});return;
 }
 fs::write(root.join("pid"),std::process::id().to_string()).unwrap();
 let mut log=fs::OpenOptions::new().create(true).append(true).open(root.join("operations")).unwrap();
 writeln!(log,"{args:?}").unwrap();
 let inspect=args.first().map(String::as_str)==Some("gateway");
 let operation=args.get(3).map(String::as_str).unwrap_or("");
 let mutation=matches!(operation,"connect"|"disconnect");
 if mode=="sleep" || (mode=="mutation-sleep"&&mutation) {thread::sleep(Duration::from_secs(3));}
 if mode=="oversized-stdout" {std::io::stdout().write_all(&vec![b'x';1024*1024]).unwrap();thread::sleep(Duration::from_secs(3));}
 if mode=="oversized-stderr" {std::io::stderr().write_all(&vec![b'x';1024*1024]).unwrap();thread::sleep(Duration::from_secs(3));}
 if mode=="inherited-pipe"||mode=="escaped-inherited-pipe" {
   let mut child=Command::new(&executable);child.arg("--hold").stdin(Stdio::null()).stdout(Stdio::inherit()).stderr(Stdio::inherit());
   if mode=="escaped-inherited-pipe" {use std::os::unix::process::CommandExt;child.process_group(0);}
   child.spawn().unwrap();
 }
 if mode=="invalid-json" {println!("{{}} {{}}");return;}
 if mode=="secret-error" {eprintln!("{{\"version\":\"0.2.0\",\"error\":{{\"code\":\"untrusted\",\"message\":\"PRIVATE_CANARY\"}}}}");std::process::exit(2);}
 if inspect {
   let model=if mode=="missing-model" {"local/removed"} else {"local/model"};
   println!("{{\"version\":\"0.2.0\",\"gateway\":{{\"address\":\"127.0.0.1:3425\",\"mode\":\"forward_candidate\",\"provider_health\":\"not_probed\",\"models\":[{{\"id\":\"{model}\",\"source\":\"configured\"}}]}}}}");
 } else {
   println!("{{\"version\":\"0.2.0\",\"plugin_id\":\"agent.fixture\",\"operation\":\"{operation}\",\"result\":{{\"selection\":{{\"model\":\"local/model\"}}}}}}");
 }
}
"###;
    }
}
