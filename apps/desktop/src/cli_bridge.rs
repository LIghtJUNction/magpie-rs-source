//! Bounded, versioned adapter to the Magpie CLI shipped beside the desktop app.
//!
//! The GUI never parses or writes Codex TOML. Success replies contain ownership
//! and field names only; arbitrary CLI diagnostics never cross this boundary.
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

const PROTOCOL_VERSION: &str = "0.2.0";
const VERSION_LINE: &[u8] = b"magpie 0.2.0";
const STDOUT_LIMIT: usize = 256 * 1024;
const STDERR_LIMIT: usize = 8 * 1024;
const MAX_MODELS: usize = 512;
const OWNED_FIELDS: [&str; 7] = [
    "model",
    "model_provider",
    "model_providers.magpie.name",
    "model_providers.magpie.base_url",
    "model_providers.magpie.wire_api",
    "model_providers.magpie.requires_openai_auth",
    "model_providers.magpie.supports_websockets",
];

// 0: idle; 1: admitted request; 2: cleanup could not be proved complete.
// A dropped request never admits an unbounded succession of orphaned children.
static ADMISSION: AtomicU8 = AtomicU8::new(0);

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Request {
    Inspect {
        address: SocketAddr,
    },
    Preview {
        config: PathBuf,
        model: String,
        address: SocketAddr,
    },
    Apply {
        config: PathBuf,
        model: String,
        address: SocketAddr,
    },
    Status {
        config: PathBuf,
    },
    Disconnect {
        config: PathBuf,
    },
}

impl Request {
    pub const fn is_mutation(&self) -> bool {
        matches!(self, Self::Apply { .. } | Self::Disconnect { .. })
    }

    fn validate(&self) -> Result<(), BridgeError> {
        match self {
            Self::Inspect { address } => validate_address(*address)?,
            Self::Preview {
                config,
                model,
                address,
            }
            | Self::Apply {
                config,
                model,
                address,
            } => {
                validate_config(config)?;
                validate_address(*address)?;
                if !writable_model(model) {
                    return Err(BridgeError::new(ErrorKind::InvalidModel));
                }
                if !transactions_supported() {
                    return Err(BridgeError::new(ErrorKind::PlatformUnsupported));
                }
            }
            Self::Status { config } | Self::Disconnect { config } => validate_config(config)?,
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
            Self::Inspect { address } => {
                let mut args = words(&["gateway", "inspect", "--listen"]);
                args.extend([address.to_string().into(), "--json".into()]);
                args
            }
            Self::Preview {
                config,
                model,
                address,
            }
            | Self::Apply {
                config,
                model,
                address,
            } => {
                let mut args = words(&["agents", "connect", "codex", "--config"]);
                args.push(config.as_os_str().to_owned());
                args.extend(words(&["--model", model, "--gateway"]));
                args.push(address.to_string().into());
                args.push(
                    if self.is_mutation() {
                        "--apply"
                    } else {
                        "--dry-run"
                    }
                    .into(),
                );
                args.push("--json".into());
                args
            }
            Self::Status { config } | Self::Disconnect { config } => {
                let command = if self.is_mutation() {
                    "disconnect"
                } else {
                    "connection-status"
                };
                let mut args = words(&["agents", command, "codex", "--config"]);
                args.push(config.as_os_str().to_owned());
                args.push("--json".into());
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
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PreviewInfo {
    pub config_existed: bool,
    pub changed_fields: Vec<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Ownership {
    Unmanaged,
    Connected,
    Drifted,
    RecoveryRequired,
}

impl Ownership {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Unmanaged => "unmanaged",
            Self::Connected => "connected",
            Self::Drifted => "drifted",
            Self::RecoveryRequired => "recovery-required",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DisconnectInfo {
    pub restored_fields: usize,
    pub preserved_fields: usize,
    pub config_removed: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Reply {
    Inspected(GatewayInfo),
    Previewed(PreviewInfo),
    Applied(PreviewInfo),
    Status(Ownership),
    Disconnected(DisconnectInfo),
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

/// Frozen core transaction validation. Searching/displaying Unicode catalog
/// entries remains allowed; writing a non-ASCII model ID is not yet supported.
pub fn writable_model(model: &str) -> bool {
    if model.is_empty()
        || model.len() > 512
        || !model.is_ascii()
        || model
            .bytes()
            .any(|byte| byte.is_ascii_whitespace() || byte.is_ascii_control())
    {
        return false;
    }
    let Some((provider, name)) = model.split_once('/') else {
        return false;
    };
    !provider.is_empty()
        && !name.is_empty()
        && provider.len() <= 128
        && provider
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"._-".contains(&byte))
        && !name.bytes().any(|byte| b"\\\"'?#".contains(&byte))
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
}

impl Admission {
    fn acquire() -> Result<Self, BridgeError> {
        match ADMISSION.compare_exchange(0, 1, Ordering::AcqRel, Ordering::Acquire) {
            Ok(_) => Ok(Self {
                safe_to_release: true,
            }),
            Err(2) => Err(BridgeError::new(ErrorKind::CleanupPending)),
            Err(_) => Err(BridgeError::new(ErrorKind::Busy)),
        }
    }
}

impl Drop for Admission {
    fn drop(&mut self) {
        ADMISSION.store(if self.safe_to_release { 0 } else { 2 }, Ordering::Release);
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

    // The frozen status/disconnect contract must remain usable offline. Query
    // and writes involving a model additionally prove inspect capability and
    // re-check the chosen catalog entry on this same executable each time.
    if let Request::Preview { model, address, .. } | Request::Apply { model, address, .. } =
        &request
    {
        let inspect = Request::Inspect { address: *address };
        let output = capture(
            &executable,
            &inspect.arguments(),
            false,
            &cancellation,
            deadline,
            limits,
            &mut admission,
        )
        .await?;
        let Reply::Inspected(gateway) = decode(&inspect, output)? else {
            return Err(BridgeError::new(ErrorKind::InvalidReply));
        };
        if !gateway.models.iter().any(|item| item.id == *model) {
            return Err(BridgeError::new(ErrorKind::ModelUnavailable));
        }
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
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ModelWire {
    id: String,
    source: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ConnectWire {
    operation: String,
    candidate: bool,
    dry_run: bool,
    writes_performed: bool,
    config_existed: bool,
    state: String,
    gateway_readiness: String,
    provider_readiness: String,
    agent_execution: String,
    changed_fields: Vec<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct StatusWire {
    operation: String,
    state: String,
    gateway_readiness: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct DisconnectWire {
    operation: String,
    restored_fields: usize,
    preserved_fields: usize,
    config_removed: bool,
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
                models: wire
                    .gateway
                    .models
                    .into_iter()
                    .map(|model| ModelInfo { id: model.id })
                    .collect(),
            }))
        }
        Request::Preview { .. } | Request::Apply { .. } => {
            let wire: ConnectWire = serde_json::from_slice(&captured.stdout)
                .map_err(|_| error(ErrorKind::InvalidReply))?;
            let apply = request.is_mutation();
            if wire.operation != "agents.connect"
                || !wire.candidate
                || wire.dry_run == apply
                || wire.writes_performed != apply
                || wire.state != if apply { "connected" } else { "prepared" }
                || wire.gateway_readiness
                    != if apply {
                        "listener_and_configured_model_checked"
                    } else {
                        "not_checked"
                    }
                || wire.provider_readiness != "not_probed"
                || wire.agent_execution != "not_checked"
                || wire.changed_fields.len() > OWNED_FIELDS.len()
            {
                return Err(error(ErrorKind::InvalidReply));
            }
            let mut seen = HashSet::new();
            if wire
                .changed_fields
                .iter()
                .any(|field| !OWNED_FIELDS.contains(&field.as_str()) || !seen.insert(field))
            {
                return Err(error(ErrorKind::InvalidReply));
            }
            let preview = PreviewInfo {
                config_existed: wire.config_existed,
                changed_fields: wire.changed_fields,
            };
            Ok(if apply {
                Reply::Applied(preview)
            } else {
                Reply::Previewed(preview)
            })
        }
        Request::Status { .. } => {
            let wire: StatusWire = serde_json::from_slice(&captured.stdout)
                .map_err(|_| error(ErrorKind::InvalidReply))?;
            if wire.operation != "agents.connection-status"
                || wire.gateway_readiness != "not_checked"
            {
                return Err(error(ErrorKind::InvalidReply));
            }
            let ownership = match wire.state.as_str() {
                "unmanaged" => Ownership::Unmanaged,
                "connected" => Ownership::Connected,
                "drifted" => Ownership::Drifted,
                "recovery-required" => Ownership::RecoveryRequired,
                _ => return Err(error(ErrorKind::InvalidReply)),
            };
            Ok(Reply::Status(ownership))
        }
        Request::Disconnect { .. } => {
            let wire: DisconnectWire = serde_json::from_slice(&captured.stdout)
                .map_err(|_| error(ErrorKind::InvalidReply))?;
            if wire.operation != "agents.disconnect"
                || wire.restored_fields > OWNED_FIELDS.len()
                || wire.preserved_fields > OWNED_FIELDS.len()
                || ![0, OWNED_FIELDS.len()]
                    .contains(&(wire.restored_fields + wire.preserved_fields))
                || (wire.config_removed
                    && (wire.preserved_fields != 0 || wire.restored_fields != OWNED_FIELDS.len()))
            {
                return Err(error(ErrorKind::InvalidReply));
            }
            Ok(Reply::Disconnected(DisconnectInfo {
                restored_fields: wire.restored_fields,
                preserved_fields: wire.preserved_fields,
                config_removed: wire.config_removed,
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

    fn preview() -> Request {
        Request::Preview {
            config: config(),
            model: "local/model".into(),
            address: address(),
        }
    }

    fn apply() -> Request {
        Request::Apply {
            config: config(),
            model: "local/model".into(),
            address: address(),
        }
    }

    fn status() -> Request {
        Request::Status { config: config() }
    }

    fn disconnect() -> Request {
        Request::Disconnect { config: config() }
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

    fn connect_json(apply: bool) -> Value {
        json!({
            "operation":"agents.connect", "candidate":true, "dry_run":!apply,
            "writes_performed":apply, "config_existed":true,
            "state":if apply { "connected" } else { "prepared" },
            "gateway_readiness":if apply { "listener_and_configured_model_checked" } else { "not_checked" },
            "provider_readiness":"not_probed", "agent_execution":"not_checked",
            "changed_fields":["model","model_provider"]
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
    fn matches_frozen_ascii_write_validation() {
        for valid in [
            "local/model",
            "a_b.c-d/model/submodel:latest",
            "a/model;$(literal)",
        ] {
            assert!(writable_model(valid), "{valid}");
        }
        for invalid in [
            "",
            "model",
            "/model",
            "local/",
            "local/model name",
            "local/模型",
            "local/line\nbreak",
            "local/m?key",
            "local/m#secret",
            "local/m\\path",
            "local/\"secret\"",
            "local/'secret'",
            "bad provider/model",
        ] {
            assert!(!writable_model(invalid), "{invalid}");
        }
        assert!(writable_model(&format!("p/{}", "x".repeat(510))));
        assert!(!writable_model(&format!("p/{}", "x".repeat(511))));
        assert!(writable_model(&format!("{}/model", "p".repeat(128))));
        assert!(!writable_model(&format!("{}/model", "p".repeat(129))));
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
            PathBuf::from(r"C:\isolated path\$(literal).toml")
        } else {
            PathBuf::from("/isolated path/$(literal).toml")
        };
        let request = Request::Preview {
            config: path.clone(),
            model: "a/model;$(literal)".into(),
            address: address(),
        };
        assert_eq!(
            request.arguments(),
            vec![
                OsString::from("agents"),
                "connect".into(),
                "codex".into(),
                "--config".into(),
                path.into_os_string(),
                "--model".into(),
                "a/model;$(literal)".into(),
                "--gateway".into(),
                "127.0.0.1:3425".into(),
                "--dry-run".into(),
                "--json".into(),
            ]
        );
        assert!(!request.is_mutation());
        assert!(apply().is_mutation());
        assert!(disconnect().is_mutation());
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
        assert!(!writable_model(&gateway.models[0].id));
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
    fn refuses_duplicate_json_keys_unknown_fields_and_multiple_objects() {
        for bytes in [
            br#"{"operation":"agents.connection-status","state":"connected","state":"unmanaged","gateway_readiness":"not_checked"}"#.as_slice(),
            br#"{"operation":"agents.connection-status","state":"connected","gateway_readiness":"not_checked","extra":"secret"}"#.as_slice(),
            br#"{"operation":"agents.connection-status","state":"connected","gateway_readiness":"not_checked"}{}"#.as_slice(),
            br#"{"operation":"agents.connection-status","state":"connected"}"#.as_slice(),
            br#"{"operation":"agents.connection-status","state":true,"gateway_readiness":"not_checked"}"#.as_slice(),
            b"\xff".as_slice(),
        ] {
            assert_eq!(decode(&status(), Captured { success:true, stdout:bytes.to_vec(), stderr:vec![] }).expect_err("strict schema").kind, ErrorKind::InvalidReply);
        }
    }

    #[test]
    fn parses_preview_and_apply_without_inventing_transaction_version_field() {
        assert_eq!(
            parse_ok(&preview(), connect_json(false)).expect("preview"),
            Reply::Previewed(PreviewInfo {
                config_existed: true,
                changed_fields: vec!["model".into(), "model_provider".into()]
            })
        );
        assert!(matches!(
            parse_ok(&apply(), connect_json(true)),
            Ok(Reply::Applied(_))
        ));
        let mut wire = connect_json(false);
        wire["version"] = "0.2.0".into();
        assert_eq!(
            parse_ok(&preview(), wire)
                .expect_err("not frozen schema")
                .kind,
            ErrorKind::InvalidReply
        );
    }

    #[test]
    fn preview_can_only_return_known_field_names_without_values() {
        for fields in [
            json!(["model", "model"]),
            json!(["api_key"]),
            json!([{"field":"model","old":"secret"}]),
            json!(["model=secret"]),
        ] {
            let mut wire = connect_json(false);
            wire["changed_fields"] = fields;
            assert_eq!(
                parse_ok(&preview(), wire)
                    .expect_err("field allowlist")
                    .kind,
                ErrorKind::InvalidReply
            );
        }
    }

    #[test]
    fn refuses_inconsistent_transaction_success_claims() {
        for (field, value) in [
            ("candidate", json!(false)),
            ("dry_run", json!(false)),
            ("writes_performed", json!(true)),
            ("state", json!("connected")),
            ("provider_readiness", json!("online")),
            ("agent_execution", json!("executed")),
            ("gateway_readiness", json!("healthy")),
        ] {
            let mut wire = connect_json(false);
            wire[field] = value;
            let error = parse_ok(&preview(), wire).expect_err(field);
            assert_eq!(error.kind, ErrorKind::InvalidReply);
            assert!(!error.mutation_may_have_run);
        }
        let error = parse_ok(&apply(), connect_json(false)).expect_err("apply is never dry run");
        assert_eq!(error.kind, ErrorKind::InvalidReply);
        assert!(error.mutation_may_have_run);
    }

    #[test]
    fn status_is_exact_ownership_and_never_network_health() {
        for state in [
            Ownership::Unmanaged,
            Ownership::Connected,
            Ownership::Drifted,
            Ownership::RecoveryRequired,
        ] {
            assert_eq!(parse_ok(&status(), json!({"operation":"agents.connection-status","state":state.as_str(),"gateway_readiness":"not_checked"})).expect("ownership"), Reply::Status(state));
        }
        for state in [
            "online",
            "healthy",
            "prepared",
            "unknown",
            "recovery_required",
        ] {
            assert_eq!(parse_ok(&status(), json!({"operation":"agents.connection-status","state":state,"gateway_readiness":"not_checked"})).expect_err("unknown state").kind, ErrorKind::InvalidReply);
        }
    }

    #[test]
    fn disconnect_validates_counts_and_supports_aborted_prepared_transaction() {
        for (restored, preserved, removed) in [(7, 0, true), (2, 5, false), (0, 0, false)] {
            assert_eq!(parse_ok(&disconnect(), json!({"operation":"agents.disconnect","restored_fields":restored,"preserved_fields":preserved,"config_removed":removed})).expect("disconnect"), Reply::Disconnected(DisconnectInfo { restored_fields:restored, preserved_fields:preserved, config_removed:removed }));
        }
        for (restored, preserved, removed) in [
            (8, 0, false),
            (7, 1, false),
            (1, 1, true),
            (1, 1, false),
            (0, 0, true),
        ] {
            assert_eq!(parse_ok(&disconnect(), json!({"operation":"agents.disconnect","restored_fields":restored,"preserved_fields":preserved,"config_removed":removed})).expect_err("counts").kind, ErrorKind::InvalidReply);
        }
    }

    #[test]
    fn redacts_untrusted_diagnostics_and_preserves_mutation_uncertainty() {
        let secret = "secret-fixture-canary-api-key-123";
        for request in [status(), apply(), disconnect()] {
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
            &status(),
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
            (true, serde_json::to_vec(&json!({"operation":"agents.connection-status","state":"connected","gateway_readiness":"not_checked"})).expect("fixture"),b"warning: private value".to_vec()),
        ] {
            assert_eq!(decode(&status(), Captured {success,stdout,stderr}).expect_err("channels").kind, ErrorKind::InvalidReply);
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
                    preview(),
                    apply(),
                    status(),
                    disconnect(),
                ] {
                    let reply = fixture
                        .request(request.clone(), Cancellation::new(), limits())
                        .await;
                    assert!(reply.is_ok(), "fixture operation {request:?}: {reply:?}");
                    fixture.assert_direct_child_reaped();
                }
                let operations = fixture.operations();
                assert!(operations.contains("--dry-run"));
                assert!(operations.contains("--apply"));
                assert!(operations.contains("connection-status"));
                assert!(operations.contains("disconnect"));

                fixture.mode("ok");
                fixture
                    .request(status(), Cancellation::new(), limits())
                    .await
                    .expect("offline status");
                fixture
                    .request(disconnect(), Cancellation::new(), limits())
                    .await
                    .expect("offline disconnect");
                assert!(!fixture.operations().contains("inspect"));

                fixture.mode("wrong-version");
                let error = fixture
                    .request(apply(), Cancellation::new(), limits())
                    .await
                    .expect_err("wrong version");
                assert_eq!(error.kind, ErrorKind::CliVersionMismatch);
                assert!(!error.mutation_may_have_run);
                assert!(fixture.operations().is_empty());

                fixture.mode("changed-executable");
                assert_eq!(
                    fixture
                        .request(status(), Cancellation::new(), limits())
                        .await
                        .expect_err("identity changed")
                        .kind,
                    ErrorKind::CliChanged
                );
                assert!(fixture.operations().is_empty());

                fixture.mode("missing-model");
                let error = fixture
                    .request(apply(), Cancellation::new(), limits())
                    .await
                    .expect_err("removed catalog entry");
                assert_eq!(error.kind, ErrorKind::ModelUnavailable);
                assert!(!error.mutation_may_have_run);
                assert!(!fixture.operations().contains("--apply"));

                for mode in ["oversized-stdout", "oversized-stderr"] {
                    fixture.mode(mode);
                    let start = Instant::now();
                    let error = fixture
                        .request(status(), Cancellation::new(), limits())
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
                        .request(status(), Cancellation::new(), limits())
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
                        status(),
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

                for request in [apply(), disconnect()] {
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
                        apply(),
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
                    .request(apply(), Cancellation::new(), limits())
                    .await
                    .expect_err("version deadline");
                assert_eq!(error.kind, ErrorKind::TimedOut);
                assert!(!error.mutation_may_have_run);
                fixture.assert_direct_child_reaped();

                fixture.mode("invalid-json");
                assert_eq!(
                    fixture
                        .request(status(), Cancellation::new(), limits())
                        .await
                        .expect_err("invalid JSON")
                        .kind,
                    ErrorKind::InvalidReply
                );
                fixture.mode("secret-error");
                let error = fixture
                    .request(status(), Cancellation::new(), limits())
                    .await
                    .expect_err("secret diagnostic");
                assert_eq!(error.kind, ErrorKind::CliRejected);
                assert!(!format!("{error:?} {error}").contains("PRIVATE_CANARY"));

                fixture.mode("sleep");
                let cancellation = Cancellation::new();
                let cancel = cancellation.clone();
                let (result, second) = future::zip(
                    fixture.request(
                        status(),
                        cancellation,
                        Limits {
                            request: Duration::from_secs(3),
                            ..limits()
                        },
                    ),
                    async {
                        Timer::after(Duration::from_millis(40)).await;
                        let second = fixture
                            .request(status(), Cancellation::new(), limits())
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
                    .request(status(), Cancellation::new(), limits())
                    .await
                    .expect("retry after reaping");
                let cancelled = Cancellation::new();
                cancelled.cancel();
                fixture.mode("ok");
                assert_eq!(
                    fixture
                        .request(apply(), cancelled, limits())
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
 let apply=args.iter().any(|a|a=="--apply");
 let disconnect=args.get(1).map(String::as_str)==Some("disconnect");
 if mode=="sleep" || (mode=="mutation-sleep"&&(apply||disconnect)) {thread::sleep(Duration::from_secs(3));}
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
 } else if args.get(1).map(String::as_str)==Some("connect") {
   println!("{{\"operation\":\"agents.connect\",\"candidate\":true,\"dry_run\":{},\"writes_performed\":{},\"config_existed\":true,\"state\":\"{}\",\"gateway_readiness\":\"{}\",\"provider_readiness\":\"not_probed\",\"agent_execution\":\"not_checked\",\"changed_fields\":[\"model\"]}}",!apply,apply,if apply {"connected"} else {"prepared"},if apply {"listener_and_configured_model_checked"} else {"not_checked"});
 } else if disconnect {println!("{{\"operation\":\"agents.disconnect\",\"restored_fields\":7,\"preserved_fields\":0,\"config_removed\":false}}");}
 else {println!("{{\"operation\":\"agents.connection-status\",\"state\":\"connected\",\"gateway_readiness\":\"not_checked\"}}");}
}
"###;
    }
}
