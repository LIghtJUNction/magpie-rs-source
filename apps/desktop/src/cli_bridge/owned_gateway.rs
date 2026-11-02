//! One owned, long-lived Gateway child. This is deliberately outside the short
//! CLI command admission and its 20-second deadline. A private ready event plus
//! a live child and matching health identity establish ownership; a port never
//! does. Dropping the child's stdin lease also handles a desktop process crash.

use super::*;
use std::sync::Mutex;

const READY_LIMIT: usize = 4096;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum GatewayFailure {
    Bridge(ErrorKind),
    AddressInUse,
    IdentityMismatch,
    Exited,
    CleanupPending,
}

impl From<BridgeError> for GatewayFailure {
    fn from(error: BridgeError) -> Self {
        if error.kind == ErrorKind::CleanupPending {
            Self::CleanupPending
        } else {
            Self::Bridge(error.kind)
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum GatewayState {
    Stopped,
    Checking,
    Starting,
    Owned {
        info: GatewayInfo,
        health_error: Option<GatewayFailure>,
        refreshing: bool,
    },
    External(GatewayInfo),
    Stopping,
    Failed(GatewayFailure),
}

impl GatewayState {
    pub(crate) fn info(&self) -> Option<&GatewayInfo> {
        match self {
            Self::Owned { info, .. } | Self::External(info) => Some(info),
            _ => None,
        }
    }

    pub(crate) fn can_start(&self) -> bool {
        matches!(self, Self::Stopped | Self::Failed(_))
            && !matches!(self, Self::Failed(GatewayFailure::CleanupPending))
    }

    pub(crate) fn can_stop(&self) -> bool {
        matches!(self, Self::Checking | Self::Starting | Self::Owned { .. })
    }
}

struct Shared {
    state: Mutex<Arc<GatewayState>>,
    stop: Cancellation,
    refresh: AtomicBool,
    pending: Arc<AtomicBool>,
    cleanup_unproven: Arc<AtomicBool>,
}

impl Shared {
    fn set(&self, state: GatewayState) {
        *self.state.lock().unwrap_or_else(|error| error.into_inner()) = Arc::new(state);
    }
}

/// A control handle, never a PID. Only its associated runner may stop a child.
#[derive(Clone)]
pub(crate) struct OwnedGateway(Arc<Shared>);

pub(crate) struct GatewayRunner {
    shared: Arc<Shared>,
    address: SocketAddr,
    start_if_absent: bool,
    #[cfg(test)]
    providers: Option<PathBuf>,
}

impl OwnedGateway {
    #[cfg(test)]
    pub(crate) fn isolated_store(address: SocketAddr, path: PathBuf) -> (Self, GatewayRunner) {
        let (handle, mut runner) = Self::begin(address, true);
        runner.providers = Some(path);
        (handle, runner)
    }

    pub(crate) fn begin(address: SocketAddr, start_if_absent: bool) -> (Self, GatewayRunner) {
        let shared = Arc::new(Shared {
            state: Mutex::new(Arc::new(GatewayState::Checking)),
            stop: Cancellation::new(),
            refresh: AtomicBool::new(false),
            pending: Arc::new(AtomicBool::new(true)),
            cleanup_unproven: Arc::new(AtomicBool::new(false)),
        });
        (
            Self(shared.clone()),
            GatewayRunner {
                shared,
                address,
                start_if_absent,
                #[cfg(test)]
                providers: None,
            },
        )
    }

    pub(crate) fn state(&self) -> Arc<GatewayState> {
        self.0
            .state
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .clone()
    }

    pub(crate) fn stop(&self) {
        if self.pending() {
            self.0.stop.cancel();
            self.0.set(GatewayState::Stopping);
        }
    }

    pub(crate) fn refresh(&self) {
        if self.0.cleanup_unproven.load(Ordering::Acquire)
            || matches!(
                &*self.state(),
                GatewayState::Owned {
                    refreshing: false,
                    ..
                }
            )
        {
            self.0.refresh.store(true, Ordering::Release);
        }
    }

    pub(crate) fn pending(&self) -> bool {
        self.0.pending.load(Ordering::Acquire)
    }

    pub(crate) fn shutdown_pending(&self) -> Arc<AtomicBool> {
        self.0.pending.clone()
    }
}

struct Completion(Arc<Shared>);
impl Drop for Completion {
    fn drop(&mut self) {
        let mut state = self
            .0
            .state
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        if matches!(
            &**state,
            GatewayState::Checking
                | GatewayState::Starting
                | GatewayState::Owned { .. }
                | GatewayState::Stopping
        ) {
            *state = Arc::new(GatewayState::Failed(GatewayFailure::CleanupPending));
        }
        self.0.pending.store(false, Ordering::Release);
    }
}

impl GatewayRunner {
    pub(crate) async fn run(self) {
        let _completion = Completion(self.shared.clone());
        let result = async {
            validate_address(self.address)?;
            let desktop =
                env::current_exe().map_err(|_| BridgeError::new(ErrorKind::CliMissing))?;
            let executable = resolve_from(&desktop, env::var_os("MAGPIE_CLI_PATH"))?;
            self.run_resolved(executable, Limits::default()).await
        }
        .await;
        if let Err(error) = result {
            self.shared
                .set(if error == GatewayFailure::Bridge(ErrorKind::Cancelled) {
                    GatewayState::Stopped
                } else {
                    GatewayState::Failed(error)
                });
        }
    }

    async fn run_resolved(
        &self,
        executable: Executable,
        limits: Limits,
    ) -> Result<(), GatewayFailure> {
        let deadline = Instant::now() + limits.request;
        let mut admission = Admission::isolated(self.shared.cleanup_unproven.clone());
        let version = capture(
            &executable,
            &["--version".into()],
            false,
            &self.shared.stop,
            deadline.min(Instant::now() + limits.version),
            limits,
            &mut admission,
        )
        .await;
        if !admission.safe_to_release {
            return Err(GatewayFailure::CleanupPending);
        }
        let version = version?;
        let line = version
            .stdout
            .strip_suffix(b"\n")
            .unwrap_or(&version.stdout);
        let line = line.strip_suffix(b"\r").unwrap_or(line);
        if !version.success || !version.stderr.is_empty() || line != VERSION_LINE {
            return Err(GatewayFailure::Bridge(ErrorKind::CliVersionMismatch));
        }
        match self.inspect(&executable, limits, deadline).await {
            Ok(info) => {
                if self.shared.stop.is_cancelled() {
                    return Err(GatewayFailure::Bridge(ErrorKind::Cancelled));
                }
                self.shared.set(GatewayState::External(info));
                return Ok(());
            }
            Err(GatewayFailure::Bridge(ErrorKind::GatewayUnavailable)) => {}
            Err(error) => return Err(error),
        }
        if !self.start_if_absent {
            self.shared.set(GatewayState::Stopped);
            return Ok(());
        }
        if self.shared.stop.is_cancelled() {
            return Err(GatewayFailure::Bridge(ErrorKind::Cancelled));
        }
        self.shared.set(GatewayState::Starting);
        let result = self.serve(&executable, limits).await;
        if result == Err(GatewayFailure::AddressInUse) && !self.shared.stop.is_cancelled() {
            // A listener may have appeared after the first probe. Its health is
            // useful information, never authority to adopt or terminate it.
            if let Ok(info) = self
                .inspect(&executable, limits, Instant::now() + limits.request)
                .await
            {
                self.shared.set(GatewayState::External(info));
                return Ok(());
            }
        }
        result
    }

    async fn inspect(
        &self,
        executable: &Executable,
        limits: Limits,
        deadline: Instant,
    ) -> Result<GatewayInfo, GatewayFailure> {
        let request = Request::Inspect {
            address: self.address,
        };
        let mut admission = Admission::isolated(self.shared.cleanup_unproven.clone());
        let output = capture(
            executable,
            &request.arguments(),
            false,
            &self.shared.stop,
            deadline,
            limits,
            &mut admission,
        )
        .await;
        if !admission.safe_to_release {
            return Err(GatewayFailure::CleanupPending);
        }
        match decode(&request, output?)? {
            Reply::Inspected(info) => Ok(info),
            _ => Err(GatewayFailure::Bridge(ErrorKind::InvalidReply)),
        }
    }

    async fn serve(&self, executable: &Executable, limits: Limits) -> Result<(), GatewayFailure> {
        executable.verify_unchanged()?;
        let mut native = std::process::Command::new(&executable.path);
        native.args([
            "serve",
            "--supervised",
            "--listen",
            &self.address.to_string(),
            "--json",
        ]);
        #[cfg(test)]
        if let Some(path) = &self.providers {
            validate_config(path)?;
            native.arg("--providers").arg(path);
        }
        #[cfg(unix)]
        {
            use std::os::unix::process::CommandExt;
            native.process_group(0);
        }
        let mut command = Command::from(native);
        command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .reap_on_drop(true);
        #[cfg(windows)]
        {
            use async_process::windows::CommandExt;
            command.creation_flags(0x0800_0000);
        }
        let mut child = command
            .spawn()
            .map_err(|_| GatewayFailure::Bridge(ErrorKind::LaunchFailed))?;
        let lease = child.stdin.take();
        let mut stdout = child
            .stdout
            .take()
            .ok_or(GatewayFailure::Bridge(ErrorKind::InvalidReply))?;
        let mut stderr = child
            .stderr
            .take()
            .ok_or(GatewayFailure::Bridge(ErrorKind::InvalidReply))?;
        let mut diagnostics = Vec::new();
        let deadline = Instant::now() + limits.request;
        let result = {
            let startup = async {
                let event = future::race(ready_line(&mut stdout), async {
                    append_bounded(&mut stderr, &mut diagnostics, limits.stderr).await?;
                    Err(serve_failure(&diagnostics))
                })
                .await?;
                let Some(event) = event else {
                    append_bounded(&mut stderr, &mut diagnostics, limits.stderr).await?;
                    return Err(serve_failure(&diagnostics));
                };
                Ready::decode(&event, self.address)
            };
            future::race(startup, async {
                let error = match stop_guard(&self.shared.stop, deadline).await {
                    Err(error) => error,
                    Ok(_) => ErrorKind::InvalidReply,
                };
                Err(GatewayFailure::Bridge(error))
            })
            .await
        };
        // capture() owns cancellation and proves cleanup for its probe child.
        // An outer timeout must not drop that cleanup future halfway through.
        let result = match result {
            Ok(ready) => {
                async {
                    if child
                        .try_status()
                        .map_err(|_| GatewayFailure::Exited)?
                        .is_some()
                    {
                        return Err(GatewayFailure::Exited);
                    }
                    let info = self.inspect(executable, limits, deadline).await?;
                    if !ready.matches(&info) {
                        return Err(GatewayFailure::IdentityMismatch);
                    }
                    if child
                        .try_status()
                        .map_err(|_| GatewayFailure::Exited)?
                        .is_some()
                    {
                        return Err(GatewayFailure::Exited);
                    }
                    Ok((ready, info))
                }
                .await
            }
            Err(error) => Err(error),
        };
        let result = match result {
            Ok((ready, info)) => {
                self.shared.set(GatewayState::Owned {
                    info: info.clone(),
                    health_error: None,
                    refreshing: false,
                });
                let running = self.monitor(&mut child, executable, &ready, info, limits);
                future::race(running, unexpected_output(&mut stdout, &mut stderr)).await
            }
            Err(error) => Err(error),
        };

        // Revoke the lease first. Kill is only a bounded fallback on this exact
        // child handle, never a process found by its address or public identity.
        self.shared.set(GatewayState::Stopping);
        drop(lease);
        let clean_deadline = Instant::now() + limits.cleanup;
        let exited = future::race(async { child.status().await.is_ok() }, async {
            Timer::at(clean_deadline).await;
            false
        })
        .await;
        if !exited {
            let _ = child.kill();
            if !future::race(async { child.status().await.is_ok() }, async {
                Timer::after(limits.cleanup).await;
                false
            })
            .await
            {
                return Err(GatewayFailure::CleanupPending);
            }
        }
        // Prove pipe EOF too, including Windows blocking pipe readers. A child
        // that left inherited writers behind cannot admit repeated starts.
        let pipes_closed = future::race(
            async {
                future::try_zip(
                    read_bounded(stdout, READY_LIMIT),
                    read_bounded(stderr, limits.stderr),
                )
                .await
                .is_ok()
            },
            async {
                Timer::after(limits.cleanup).await;
                false
            },
        )
        .await;
        if !pipes_closed {
            return Err(GatewayFailure::CleanupPending);
        }
        if self.shared.cleanup_unproven.load(Ordering::Acquire)
            || result == Err(GatewayFailure::CleanupPending)
        {
            return Err(GatewayFailure::CleanupPending);
        }
        if self.shared.stop.is_cancelled() {
            self.shared.set(GatewayState::Stopped);
            Ok(())
        } else {
            result
        }
    }

    async fn monitor(
        &self,
        child: &mut async_process::Child,
        executable: &Executable,
        ready: &Ready,
        mut info: GatewayInfo,
        limits: Limits,
    ) -> Result<(), GatewayFailure> {
        loop {
            if self.shared.stop.is_cancelled() {
                return Ok(());
            }
            if child
                .try_status()
                .map_err(|_| GatewayFailure::Exited)?
                .is_some()
            {
                return Err(GatewayFailure::Exited);
            }
            if self.shared.refresh.swap(false, Ordering::AcqRel) {
                self.shared.set(GatewayState::Owned {
                    info: info.clone(),
                    health_error: None,
                    refreshing: true,
                });
                let result = self
                    .inspect(executable, limits, Instant::now() + limits.request)
                    .await;
                if result == Err(GatewayFailure::CleanupPending) {
                    return Err(GatewayFailure::CleanupPending);
                }
                if self.shared.stop.is_cancelled() {
                    return Ok(());
                }
                let health_error = match result {
                    Ok(current) if ready.matches(&current) => {
                        info = current;
                        None
                    }
                    Ok(_) => Some(GatewayFailure::IdentityMismatch),
                    Err(GatewayFailure::CleanupPending) => {
                        return Err(GatewayFailure::CleanupPending);
                    }
                    Err(error) => Some(error),
                };
                self.shared.set(GatewayState::Owned {
                    info: info.clone(),
                    health_error,
                    refreshing: false,
                });
            }
            Timer::after(Duration::from_millis(100)).await;
        }
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Ready {
    event: String,
    protocol_version: u32,
    server_version: String,
    address: String,
    instance_id: String,
    config_source: String,
}

impl Ready {
    fn decode(bytes: &[u8], address: SocketAddr) -> Result<Self, GatewayFailure> {
        let ready: Self = serde_json::from_slice(bytes)
            .map_err(|_| GatewayFailure::Bridge(ErrorKind::InvalidReply))?;
        if ready.event != "gateway_ready"
            || ready.protocol_version != 1
            || parse_address(&ready.address).ok() != Some(address)
            || ready.config_source != "provider_store"
            || !identity_field(&Some(ready.instance_id.clone()))
            || !identity_field(&Some(ready.server_version.clone()))
        {
            return Err(GatewayFailure::Bridge(ErrorKind::InvalidReply));
        }
        Ok(ready)
    }

    fn matches(&self, info: &GatewayInfo) -> bool {
        info.server_version.as_deref() == Some(&self.server_version)
            && info.instance_id.as_deref() == Some(&self.instance_id)
            && info.config_source.as_deref() == Some(&self.config_source)
    }
}

async fn ready_line(
    reader: &mut (impl AsyncRead + Unpin),
) -> Result<Option<Vec<u8>>, GatewayFailure> {
    let mut line = Vec::new();
    loop {
        let mut byte = [0; 1];
        let count = reader
            .read(&mut byte)
            .await
            .map_err(|_| GatewayFailure::Bridge(ErrorKind::InvalidReply))?;
        if count == 0 {
            return if line.is_empty() {
                Ok(None)
            } else {
                Err(GatewayFailure::Bridge(ErrorKind::InvalidReply))
            };
        }
        if byte[0] == b'\n' {
            return Ok(Some(line));
        }
        if line.len() == READY_LIMIT {
            return Err(GatewayFailure::Bridge(ErrorKind::OutputTooLarge));
        }
        line.push(byte[0]);
    }
}

async fn append_bounded(
    reader: &mut (impl AsyncRead + Unpin),
    output: &mut Vec<u8>,
    limit: usize,
) -> Result<(), GatewayFailure> {
    let mut buffer = [0; 1024];
    loop {
        let count = reader
            .read(&mut buffer)
            .await
            .map_err(|_| GatewayFailure::Bridge(ErrorKind::InvalidReply))?;
        if count == 0 {
            return Ok(());
        }
        if count > limit.saturating_sub(output.len()) {
            return Err(GatewayFailure::Bridge(ErrorKind::OutputTooLarge));
        }
        output.extend_from_slice(&buffer[..count]);
    }
}

async fn unexpected_output(
    stdout: &mut (impl AsyncRead + Unpin),
    stderr: &mut (impl AsyncRead + Unpin),
) -> Result<(), GatewayFailure> {
    async fn watch(reader: &mut (impl AsyncRead + Unpin)) -> Result<(), GatewayFailure> {
        let mut byte = [0; 1];
        match reader.read(&mut byte).await {
            Ok(0) => std::future::pending().await, // monitor() observes process exit.
            _ => Err(GatewayFailure::Bridge(ErrorKind::InvalidReply)),
        }
    }
    future::race(watch(stdout), watch(stderr)).await
}

fn serve_failure(bytes: &[u8]) -> GatewayFailure {
    match serde_json::from_slice::<ErrorWire>(bytes) {
        Ok(error)
            if error.version == PROTOCOL_VERSION
                && error.error.code == "gateway_address_in_use" =>
        {
            GatewayFailure::AddressInUse
        }
        Ok(error) if error.version != PROTOCOL_VERSION => {
            GatewayFailure::Bridge(ErrorKind::CliVersionMismatch)
        }
        Ok(_) => GatewayFailure::Bridge(ErrorKind::CliRejected),
        Err(_) => GatewayFailure::Bridge(ErrorKind::InvalidReply),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn address() -> SocketAddr {
        "127.0.0.1:3425".parse().unwrap()
    }
    fn ready() -> serde_json::Value {
        json!({"event":"gateway_ready","protocol_version":1,"server_version":"0.2.0",
            "address":"127.0.0.1:3425","instance_id":"fixture-one","config_source":"provider_store"})
    }

    #[test]
    fn ready_requires_private_protocol_address_and_matching_health_identity() {
        let event = Ready::decode(&serde_json::to_vec(&ready()).unwrap(), address()).unwrap();
        let mut info = GatewayInfo {
            address: address(),
            models: vec![],
            server_version: Some("0.2.0".into()),
            instance_id: Some("fixture-one".into()),
            config_source: Some("provider_store".into()),
        };
        assert!(event.matches(&info));
        info.instance_id = Some("unrelated-service".into());
        assert!(!event.matches(&info));
        info.instance_id = Some("fixture-one".into());
        info.server_version = None;
        assert!(!event.matches(&info));
        for (field, value) in [
            ("event", json!("bound")),
            ("protocol_version", json!(2)),
            ("address", json!("0.0.0.0:3425")),
            ("config_source", json!("legacy_forward")),
            ("instance_id", json!("bad\nidentity")),
            ("server_version", json!("")),
        ] {
            let mut wire = ready();
            wire[field] = value;
            assert!(
                Ready::decode(&serde_json::to_vec(&wire).unwrap(), address()).is_err(),
                "{field}"
            );
        }
        let mut wire = ready();
        wire["secret"] = json!("must-not-cross-boundary");
        assert!(Ready::decode(&serde_json::to_vec(&wire).unwrap(), address()).is_err());
    }

    #[test]
    fn incomplete_or_oversized_ready_and_secret_diagnostics_never_become_ownership() {
        future::block_on(async {
            assert!(
                ready_line(&mut &b"{\"event\":\"gateway_ready\"}"[..])
                    .await
                    .is_err()
            );
            assert!(
                ready_line(&mut vec![b'x'; READY_LIMIT + 1].as_slice())
                    .await
                    .is_err()
            );
            assert_eq!(ready_line(&mut &b""[..]).await.unwrap(), None);
        });
        let error = serve_failure(
            br#"{"version":"0.2.0","error":{"code":"untrusted","message":"PRIVATE_CANARY"}}"#,
        );
        assert_eq!(error, GatewayFailure::Bridge(ErrorKind::CliRejected));
        assert!(!format!("{error:?}").contains("PRIVATE_CANARY"));
        assert!(!GatewayState::Failed(GatewayFailure::CleanupPending).can_start());
        assert!(GatewayState::Stopped.can_start());
        let (owner, _runner) = OwnedGateway::begin(address(), true);
        {
            let mut probe = Admission::isolated(owner.0.cleanup_unproven.clone());
            probe.safe_to_release = false; // A dropped in-flight capture.
        }
        owner.stop();
        drop(Completion(owner.0.clone()));
        assert_eq!(
            &*owner.state(),
            &GatewayState::Failed(GatewayFailure::CleanupPending)
        );
        assert!(
            !owner.state().can_start(),
            "Stop must not clear unproved probe cleanup"
        );
    }

    #[cfg(unix)]
    mod processes {
        use super::*;
        use std::os::unix::fs::PermissionsExt;

        struct Fixture {
            root: PathBuf,
            executable: PathBuf,
        }
        impl Fixture {
            fn new() -> Self {
                let nonce = SystemTime::now()
                    .duration_since(SystemTime::UNIX_EPOCH)
                    .unwrap()
                    .as_nanos();
                let root = env::temp_dir().join(format!(
                    "magpie-owned-gateway-test-{}-{nonce}",
                    std::process::id()
                ));
                fs::create_dir(&root).unwrap();
                fs::set_permissions(&root, fs::Permissions::from_mode(0o700)).unwrap();
                let source = root.join("fixture.rs");
                fs::write(&source, SOURCE).unwrap();
                let executable = root.join("magpie");
                let output = std::process::Command::new(
                    env::var_os("RUSTC").unwrap_or_else(|| "rustc".into()),
                )
                .arg("--edition=2024")
                .arg("-O")
                .arg(&source)
                .arg("-o")
                .arg(&executable)
                .output()
                .unwrap();
                assert!(
                    output.status.success(),
                    "{}",
                    String::from_utf8_lossy(&output.stderr)
                );
                Self { root, executable }
            }
            fn mode(&self, mode: &str) {
                fs::write(self.root.join("mode"), mode).unwrap();
                for name in [
                    "running",
                    "serve-pid",
                    "lease-closed",
                    "probe-start",
                    "probe-pid",
                ] {
                    let _ = fs::remove_file(self.root.join(name));
                }
            }
            fn executable(&self) -> Executable {
                Executable::open(&self.executable).unwrap()
            }
            fn assert_reaped(&self) {
                for file in ["serve-pid", "probe-pid"] {
                    if let Ok(pid) = fs::read_to_string(self.root.join(file)) {
                        assert!(
                            !std::process::Command::new("/bin/kill")
                                .args(["-0", pid.trim()])
                                .stdout(Stdio::null())
                                .stderr(Stdio::null())
                                .status()
                                .unwrap()
                                .success()
                        );
                    }
                }
            }
        }
        impl Drop for Fixture {
            fn drop(&mut self) {
                let _ = fs::remove_dir_all(&self.root);
            }
        }
        fn limits() -> Limits {
            Limits {
                request: Duration::from_millis(400),
                version: Duration::from_millis(200),
                cleanup: Duration::from_millis(300),
                stdout: 4096,
                stderr: 512,
            }
        }

        async fn run(
            runner: GatewayRunner,
            executable: Executable,
            limits: Limits,
        ) -> Result<(), GatewayFailure> {
            let _completion = Completion(runner.shared.clone());
            let result = runner.run_resolved(executable, limits).await;
            if let Err(error) = result {
                runner.shared.set(GatewayState::Failed(error));
            }
            result
        }

        async fn wait_owned(handle: &OwnedGateway) {
            let deadline = Instant::now() + Duration::from_secs(4);
            while !matches!(&*handle.state(), GatewayState::Owned { .. }) {
                assert!(
                    Instant::now() < deadline && handle.pending(),
                    "not running: {:?}",
                    handle.state()
                );
                Timer::after(Duration::from_millis(10)).await;
            }
        }

        #[test]
        fn owned_gateway_lease_lifetime_independent_admission_external_and_failures() {
            let _lock = PROCESS_TEST_LOCK
                .lock()
                .unwrap_or_else(|error| error.into_inner());
            let fixture = Fixture::new();
            future::block_on(async {
                fixture.mode("ok");
                let (owner, runner) = OwnedGateway::begin(address(), true);
                let (result, ()) =
                    future::zip(run(runner, fixture.executable(), limits()), async {
                        wait_owned(&owner).await;
                        // Owning a long-lived service does not hold the global
                        // short-command gate, nor does a local probe reset it.
                        let admitted = Admission::acquire().unwrap();
                        owner.refresh();
                        Timer::after(Duration::from_millis(80)).await;
                        assert_eq!(ADMISSION.load(Ordering::Acquire), 1);
                        drop(admitted);
                        let _another = Admission::acquire().unwrap();
                        Timer::after(limits().request + Duration::from_millis(100)).await;
                        assert!(
                            matches!(&*owner.state(), GatewayState::Owned { .. }),
                            "long service inherited a short request deadline"
                        );
                        owner.stop();
                    })
                    .await;
                assert_eq!(result, Ok(()));
                assert_eq!(&*owner.state(), &GatewayState::Stopped);
                assert!(!owner.pending());
                assert!(
                    fixture.root.join("lease-closed").exists(),
                    "graceful stop must close stdin before kill"
                );
                fixture.assert_reaped();

                fixture.mode("slow-health");
                let (owner, runner) = OwnedGateway::begin(address(), true);
                let (result, ()) =
                    future::zip(run(runner, fixture.executable(), limits()), async {
                        let deadline = Instant::now() + Duration::from_secs(3);
                        while !fixture.root.join("probe-start").exists() {
                            assert!(Instant::now() < deadline && owner.pending());
                            Timer::after(Duration::from_millis(5)).await;
                        }
                        owner.stop();
                    })
                    .await;
                assert_eq!(result, Ok(()));
                assert_eq!(&*owner.state(), &GatewayState::Stopped);
                assert!(
                    owner.state().can_start(),
                    "ordinary cancellation must finish probe cleanup before allowing retry"
                );
                fixture.assert_reaped();

                fixture.mode("external");
                let (external, runner) = OwnedGateway::begin(address(), true);
                run(runner, fixture.executable(), limits()).await.unwrap();
                assert!(matches!(&*external.state(), GatewayState::External(_)));
                assert!(!external.state().can_stop());
                external.stop();
                assert!(matches!(&*external.state(), GatewayState::External(_)));
                assert!(
                    !fixture.root.join("serve-pid").exists(),
                    "external service must not spawn or be adopted"
                );

                for (mode, expected) in [
                    ("mismatch", GatewayFailure::IdentityMismatch),
                    ("address-in-use", GatewayFailure::AddressInUse),
                    ("silent", GatewayFailure::Bridge(ErrorKind::TimedOut)),
                    (
                        "wrong-version",
                        GatewayFailure::Bridge(ErrorKind::CliVersionMismatch),
                    ),
                    (
                        "extra-output",
                        GatewayFailure::Bridge(ErrorKind::InvalidReply),
                    ),
                ] {
                    fixture.mode(mode);
                    let (handle, runner) = OwnedGateway::begin(address(), true);
                    assert_eq!(
                        run(runner, fixture.executable(), limits()).await,
                        Err(expected),
                        "{mode}"
                    );
                    assert!(matches!(&*handle.state(), GatewayState::Failed(_)));
                    assert!(!handle.pending());
                    fixture.assert_reaped();
                }

                fixture.mode("ok");
                let (handle, runner) = OwnedGateway::begin(address(), false);
                run(runner, fixture.executable(), limits()).await.unwrap();
                assert_eq!(&*handle.state(), &GatewayState::Stopped);
                assert!(
                    !fixture.root.join("serve-pid").exists(),
                    "checking must not start a gateway"
                );
            });
        }

        #[test]
        #[ignore = "requires a CLI rebuilt from this checkout; set MAGPIE_TEST_CLI to its absolute path"]
        fn real_cli_gateway_owner_stop_and_external_readonly() {
            let _lock = PROCESS_TEST_LOCK
                .lock()
                .unwrap_or_else(|error| error.into_inner());
            let executable_path =
                PathBuf::from(env::var_os("MAGPIE_TEST_CLI").expect("MAGPIE_TEST_CLI"));
            let fixture = Fixture::new();
            let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
            let address = listener.local_addr().unwrap();
            drop(listener);
            let (owner, mut runner) = OwnedGateway::begin(address, true);
            runner.providers = Some(fixture.root.join("providers.toml"));
            future::block_on(async {
                let (result, ()) = future::zip(run(runner, Executable::open(&executable_path).unwrap(), Limits::default()), async {
                    wait_owned(&owner).await;
                    let (external, runner) = OwnedGateway::begin(address, true);
                    run(runner, Executable::open(&executable_path).unwrap(), Limits::default()).await.unwrap();
                    assert!(matches!(&*external.state(), GatewayState::External(_)));
                    external.stop();
                    assert!(std::net::TcpStream::connect(address).is_ok());
                    assert!(matches!(&*owner.state(), GatewayState::Owned { info, .. } if info.server_version.as_deref() == Some("0.2.0")));
                    owner.stop();
                }).await;
                assert_eq!(result, Ok(()));
            });
            assert!(!owner.pending());
            assert_eq!(&*owner.state(), &GatewayState::Stopped);
            assert!(
                std::net::TcpStream::connect(address).is_err(),
                "owned listener must be gone before shutdown completes"
            );
        }

        const SOURCE: &str = r###"
use std::{env,fs,io::{Read,Write}};
fn main() {
 let args:Vec<String>=env::args().skip(1).collect();
 let exe=env::current_exe().unwrap(); let root=exe.parent().unwrap();
 let mode=fs::read_to_string(root.join("mode")).unwrap_or_else(|_|"ok".into());
 if args==["--version"] {println!("magpie {}",if mode=="wrong-version" {"0.0.0"} else {"0.2.0"});return;}
 if args.first().map(String::as_str)==Some("gateway") {
  if mode=="slow-health" && root.join("running").exists() {
   fs::write(root.join("probe-pid"),std::process::id().to_string()).unwrap();
   fs::write(root.join("probe-start"),b"1").unwrap();
   std::thread::sleep(std::time::Duration::from_secs(3));
  }
  if mode=="external" || root.join("running").exists() {
   println!(r#"{{"version":"0.2.0","gateway":{{"address":"127.0.0.1:3425","mode":"forward_candidate","provider_health":"not_probed","models":[],"server_version":"0.2.0","instance_id":"fixture-one","config_source":"provider_store"}}}}"#);
  } else {eprintln!(r#"{{"version":"0.2.0","error":{{"code":"gateway_inspect_failed","message":"offline"}}}}"#);std::process::exit(1);}
  return;
 }
 assert_eq!(&args[..], &["serve","--supervised","--listen","127.0.0.1:3425","--json"]);
 fs::write(root.join("serve-pid"),std::process::id().to_string()).unwrap();
 if mode=="address-in-use" {eprintln!(r#"{{"version":"0.2.0","error":{{"code":"gateway_address_in_use","message":"occupied"}}}}"#);std::process::exit(1);}
 fs::write(root.join("running"),b"1").unwrap();
 if mode!="silent" {
  println!(r#"{{"event":"gateway_ready","protocol_version":1,"server_version":"0.2.0","address":"127.0.0.1:3425","instance_id":"{}","config_source":"provider_store"}}"#, if mode=="mismatch" {"unrelated"} else {"fixture-one"});
  if mode=="extra-output" {println!("PRIVATE_CANARY");}
  std::io::stdout().flush().unwrap();
 }
 let mut bytes=Vec::new();std::io::stdin().read_to_end(&mut bytes).unwrap();
 fs::remove_file(root.join("running")).unwrap();fs::write(root.join("lease-closed"),b"closed").unwrap();
}
"###;
    }
}
