//! Window-independent state and orchestration for the frozen Magpie CLI contract.
//! Processes and private configuration observations run only in background Jobs.

use crate::{
    cli_bridge::{
        self, BridgeError, Cancellation, DisconnectInfo, ErrorKind, GatewayInfo, ModelInfo,
        Ownership, PreviewInfo, Reply, Request,
    },
    config_stamp::ConfigStamp,
};
use std::{
    future::Future,
    net::SocketAddr,
    path::{Path, PathBuf},
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum JobKind {
    Inspect,
    Preview,
    Apply,
    Status,
    Disconnect,
}
impl JobKind {
    pub const fn is_mutation(self) -> bool {
        matches!(self, Self::Apply | Self::Disconnect)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Binding {
    pub config: PathBuf,
    pub address: SocketAddr,
    pub model: String,
    pub catalog_generation: u64,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PreviewTicket {
    pub binding: Binding,
    pub info: PreviewInfo,
    stamp: ConfigStamp,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Notice {
    CatalogUpdated,
    PreviewReady,
    Connected,
    Disconnected(DisconnectInfo),
    StatusVerified,
    MutationUncertain,
}
struct Busy {
    id: u64,
    kind: JobKind,
    generation: u64,
    cancellation: Cancellation,
}

pub struct ActionsState {
    pub address: String,
    pub search: String,
    pub selected: Option<String>,
    pub catalog: Option<GatewayInfo>,
    pub ownership: Option<Ownership>,
    pub preview: Option<PreviewTicket>,
    pub error: Option<BridgeError>,
    pub notice: Option<Notice>,
    pub uncertain: bool,
    pub disconnect_confirmation: bool,
    config_path: Option<PathBuf>,
    catalog_valid: bool,
    catalog_generation: u64,
    generation: u64,
    next_job: u64,
    busy: Option<Busy>,
    needs_status: bool,
    needs_snapshot: bool,
    snapshot_ok: bool,
}

impl Default for ActionsState {
    fn default() -> Self {
        Self {
            address: "127.0.0.1:3425".to_owned(),
            search: String::new(),
            selected: None,
            catalog: None,
            ownership: None,
            preview: None,
            error: None,
            notice: None,
            uncertain: false,
            disconnect_confirmation: false,
            config_path: None,
            catalog_valid: false,
            catalog_generation: 0,
            generation: 0,
            next_job: 0,
            busy: None,
            needs_status: false,
            needs_snapshot: false,
            snapshot_ok: false,
        }
    }
}
impl Drop for ActionsState {
    fn drop(&mut self) {
        self.cancel();
    }
}

impl ActionsState {
    pub fn config_path(&self) -> Option<&Path> {
        self.config_path.as_deref()
    }
    pub fn busy(&self) -> Option<JobKind> {
        self.busy.as_ref().map(|job| job.kind)
    }
    pub fn cancel(&self) {
        if let Some(job) = &self.busy {
            job.cancellation.cancel();
        }
    }
    pub fn invalidate_preview(&mut self) {
        self.preview = None;
        self.disconnect_confirmation = false;
        self.generation = self.generation.wrapping_add(1);
        if matches!(self.notice, Some(Notice::PreviewReady)) {
            self.notice = None;
        }
    }
    pub fn set_config_path(&mut self, path: Option<PathBuf>) -> bool {
        if self.busy.is_some()
            || self.config_path == path
            || self.uncertain
            || self.needs_status
            || self.needs_snapshot
        {
            return false;
        }
        self.config_path = path;
        self.ownership = None;
        self.snapshot_ok = false;
        self.error = None;
        self.notice = None;
        self.invalidate_preview();
        true
    }
    pub fn set_address(&mut self, value: String) -> bool {
        if self.busy.is_some() || self.address == value {
            return false;
        }
        self.address = value;
        self.catalog_valid = false;
        self.invalidate_preview();
        true
    }
    pub fn set_search(&mut self, value: String) -> bool {
        if self.busy.is_some() || self.search == value {
            return false;
        }
        self.search = value;
        self.invalidate_preview();
        true
    }
    pub fn select_model(&mut self, value: String) -> bool {
        if self.busy.is_some()
            || !self.catalog_is_current()
            || self.selected.as_ref() == Some(&value)
            || !self
                .catalog
                .as_ref()
                .is_some_and(|catalog| catalog.models.iter().any(|model| model.id == value))
        {
            return false;
        }
        self.selected = Some(value);
        self.invalidate_preview();
        true
    }
    pub fn visible_models(&self) -> Vec<&ModelInfo> {
        let needle = self.search.to_lowercase();
        self.catalog
            .as_ref()
            .map(|catalog| {
                catalog
                    .models
                    .iter()
                    .filter(|model| model.id.to_lowercase().contains(&needle))
                    .collect()
            })
            .unwrap_or_default()
    }
    pub fn catalog_is_current(&self) -> bool {
        self.catalog_valid
            && self.catalog.as_ref().is_some_and(|catalog| {
                cli_bridge::parse_address(&self.address).ok() == Some(catalog.address)
            })
    }
    pub fn selected_available(&self) -> bool {
        self.catalog_is_current()
            && self.selected.as_ref().is_some_and(|selected| {
                self.catalog
                    .as_ref()
                    .is_some_and(|catalog| catalog.models.iter().any(|model| &model.id == selected))
            })
    }
    fn writes_ready(&self) -> bool {
        self.busy.is_none()
            && self.snapshot_ok
            && !self.needs_snapshot
            && !self.needs_status
            && !self.uncertain
            && self.config_path.is_some()
            && cli_bridge::transactions_supported()
    }
    pub fn can_preview(&self) -> bool {
        self.writes_ready()
            && self.ownership == Some(Ownership::Unmanaged)
            && self.selected_available()
            && self
                .selected
                .as_deref()
                .is_some_and(cli_bridge::writable_model)
    }
    pub fn can_apply(&self) -> bool {
        self.can_preview()
            && self
                .preview
                .as_ref()
                .is_some_and(|ticket| self.binding().as_ref() == Some(&ticket.binding))
    }
    pub fn can_disconnect(&self) -> bool {
        self.writes_ready()
            && matches!(
                self.ownership,
                Some(Ownership::Connected | Ownership::Drifted | Ownership::RecoveryRequired)
            )
    }
    pub fn reconcile_needed(&self) -> bool {
        self.needs_status
    }
    pub fn snapshot_failed(&self) -> bool {
        !self.snapshot_ok || self.needs_snapshot
    }
    pub fn acknowledge_snapshot_refresh(&mut self, success: bool) {
        self.snapshot_ok = success;
        if success {
            self.needs_snapshot = false;
        } else {
            self.invalidate_preview();
        }
        self.finish_reconciliation();
    }
    fn finish_reconciliation(&mut self) {
        if self.uncertain && self.snapshot_ok && !self.needs_snapshot && !self.needs_status {
            self.uncertain = false;
            self.notice = Some(Notice::StatusVerified);
        }
    }
    fn binding(&self) -> Option<Binding> {
        Some(Binding {
            config: self.config_path.clone()?,
            address: cli_bridge::parse_address(&self.address).ok()?,
            model: self.selected.clone()?,
            catalog_generation: self.catalog_generation,
        })
    }
    fn gate_error(&self) -> BridgeError {
        BridgeError::new(if !cli_bridge::transactions_supported() {
            ErrorKind::PlatformUnsupported
        } else if self.uncertain || self.needs_status || self.needs_snapshot {
            ErrorKind::RecoveryRequired
        } else if !self.snapshot_ok {
            ErrorKind::ConfigInvalid
        } else if self.ownership == Some(Ownership::RecoveryRequired) {
            ErrorKind::RecoveryRequired
        } else if self
            .ownership
            .is_some_and(|state| state != Ownership::Unmanaged)
        {
            ErrorKind::AlreadyManaged
        } else if !self.selected_available() {
            ErrorKind::ModelUnavailable
        } else if !self
            .selected
            .as_deref()
            .is_some_and(cli_bridge::writable_model)
        {
            ErrorKind::InvalidModel
        } else {
            ErrorKind::ConfigConflict
        })
    }
    pub fn begin(&mut self, kind: JobKind, config: PathBuf) -> Result<Job, BridgeError> {
        if self.busy.is_some() {
            return Err(BridgeError::new(ErrorKind::Busy));
        }
        let result = self.prepare(kind, config);
        if let Err(error) = &result {
            self.error = Some(*error);
        }
        result
    }
    fn prepare(&mut self, kind: JobKind, config: PathBuf) -> Result<Job, BridgeError> {
        if kind != JobKind::Inspect
            && (!config.is_absolute() || self.config_path.as_ref() != Some(&config))
        {
            return Err(BridgeError::new(ErrorKind::InvalidConfigPath));
        }
        let operation = match kind {
            JobKind::Inspect => {
                let address = cli_bridge::parse_address(&self.address)?;
                self.catalog_valid = false;
                self.invalidate_preview();
                Operation::Inspect(address)
            }
            JobKind::Preview => {
                if !self.can_preview() {
                    return Err(self.gate_error());
                }
                self.preview = None;
                Operation::Preview(self.binding().expect("preview gate binds the target"))
            }
            JobKind::Apply => {
                if !self.can_apply() {
                    return Err(self.gate_error());
                }
                Operation::Apply(self.preview.take().expect("apply gate requires a ticket"))
            }
            JobKind::Status => Operation::Status(config),
            JobKind::Disconnect => {
                if !self.can_disconnect() || !self.disconnect_confirmation {
                    return Err(self.gate_error());
                }
                self.disconnect_confirmation = false;
                self.preview = None;
                Operation::Disconnect(config)
            }
        };
        let reconciliation = kind == JobKind::Status && self.needs_status;
        if !reconciliation {
            self.error = None;
            self.notice = None;
        }
        self.next_job = self.next_job.wrapping_add(1);
        let cancellation = Cancellation::new();
        self.busy = Some(Busy {
            id: self.next_job,
            kind,
            generation: self.generation,
            cancellation: cancellation.clone(),
        });
        Ok(Job {
            id: self.next_job,
            kind,
            generation: self.generation,
            cancellation,
            operation,
            reconciliation,
        })
    }
    /// False means obsolete data was suppressed. A mutation still sets pending
    /// observation flags and callers must refresh even for a false result.
    pub fn complete(&mut self, result: JobResult) -> bool {
        let Some(busy) = &self.busy else {
            return false;
        };
        if busy.id != result.id || busy.kind != result.kind {
            return false;
        }
        let current = busy.generation == result.generation && self.generation == result.generation;
        self.busy = None;
        if result.kind.is_mutation() {
            self.preview = None;
            self.ownership = None;
            self.needs_status = true;
            self.needs_snapshot = true;
            self.snapshot_ok = false;
        }
        if !current {
            if result.kind.is_mutation() {
                self.uncertain = true;
                self.notice = Some(Notice::MutationUncertain);
            }
            return false;
        }
        match result.output {
            Ok(Output::Inspected(catalog)) => {
                self.catalog = Some(catalog);
                self.catalog_valid = true;
                self.catalog_generation = self.catalog_generation.wrapping_add(1);
                self.preview = None;
                self.notice = Some(Notice::CatalogUpdated);
            }
            Ok(Output::Previewed(ticket)) => {
                if self.binding().as_ref() != Some(&ticket.binding) {
                    self.error = Some(BridgeError::new(ErrorKind::ConfigConflict));
                    return false;
                }
                self.preview = Some(ticket);
                self.notice = Some(Notice::PreviewReady);
            }
            Ok(Output::Applied) => self.notice = Some(Notice::Connected),
            Ok(Output::Disconnected(info)) => self.notice = Some(Notice::Disconnected(info)),
            Ok(Output::Status(ownership)) => {
                self.ownership = Some(ownership);
                self.needs_status = false;
                if ownership != Ownership::Unmanaged {
                    self.preview = None;
                }
                let changed = (matches!(self.notice, Some(Notice::Connected))
                    && ownership != Ownership::Connected)
                    || (matches!(self.notice, Some(Notice::Disconnected(_)))
                        && ownership != Ownership::Unmanaged);
                if !result.reconciliation || changed {
                    self.notice = Some(Notice::StatusVerified);
                }
                self.finish_reconciliation();
            }
            Err(error) => {
                self.error = Some(error);
                if result.kind == JobKind::Status {
                    self.ownership = None;
                    self.preview = None;
                }
                if error.mutation_may_have_run {
                    self.uncertain = true;
                    self.notice = Some(Notice::MutationUncertain);
                }
            }
        }
        true
    }
}

enum Operation {
    Inspect(SocketAddr),
    Preview(Binding),
    Apply(PreviewTicket),
    Status(PathBuf),
    Disconnect(PathBuf),
}
enum Output {
    Inspected(GatewayInfo),
    Previewed(PreviewTicket),
    Applied,
    Status(Ownership),
    Disconnected(DisconnectInfo),
}
pub struct Job {
    id: u64,
    kind: JobKind,
    generation: u64,
    cancellation: Cancellation,
    operation: Operation,
    reconciliation: bool,
}
pub struct JobResult {
    pub kind: JobKind,
    id: u64,
    generation: u64,
    output: Result<Output, BridgeError>,
    reconciliation: bool,
}

fn cancelled(token: &Cancellation) -> Result<(), BridgeError> {
    if token.is_cancelled() {
        Err(BridgeError::new(ErrorKind::Cancelled))
    } else {
        Ok(())
    }
}
fn changed() -> BridgeError {
    BridgeError::new(ErrorKind::ConfigConflict)
}
fn invalid_reply() -> BridgeError {
    BridgeError::new(ErrorKind::InvalidReply)
}

impl Job {
    pub async fn execute(self) -> JobResult {
        self.execute_with(cli_bridge::run).await
    }
    async fn execute_with<F, Fut>(self, mut run: F) -> JobResult
    where
        F: FnMut(Request, Cancellation) -> Fut,
        Fut: Future<Output = Result<Reply, BridgeError>>,
    {
        let output = async {
            cancelled(&self.cancellation)?;
            match self.operation {
                Operation::Inspect(address) => {
                    match run(Request::Inspect { address }, self.cancellation.clone()).await? {
                        Reply::Inspected(info) => Ok(Output::Inspected(info)),
                        _ => Err(invalid_reply()),
                    }
                }
                Operation::Status(config) => {
                    match run(Request::Status { config }, self.cancellation.clone()).await? {
                        Reply::Status(state) => Ok(Output::Status(state)),
                        _ => Err(invalid_reply()),
                    }
                }
                Operation::Disconnect(config) => {
                    match run(Request::Disconnect { config }, self.cancellation.clone()).await? {
                        Reply::Disconnected(info) => Ok(Output::Disconnected(info)),
                        _ => Err(BridgeError {
                            kind: ErrorKind::InvalidReply,
                            mutation_may_have_run: true,
                        }),
                    }
                }
                Operation::Preview(binding) => {
                    let stamp = ConfigStamp::capture(&binding.config)?;
                    cancelled(&self.cancellation)?;
                    let reply = run(
                        Request::Preview {
                            config: binding.config.clone(),
                            model: binding.model.clone(),
                            address: binding.address,
                        },
                        self.cancellation.clone(),
                    )
                    .await?;
                    let Reply::Previewed(info) = reply else {
                        return Err(invalid_reply());
                    };
                    cancelled(&self.cancellation)?;
                    if ConfigStamp::capture(&binding.config)? != stamp
                        || stamp.exists() != info.config_existed
                    {
                        return Err(changed());
                    }
                    Ok(Output::Previewed(PreviewTicket {
                        binding,
                        info,
                        stamp,
                    }))
                }
                Operation::Apply(ticket) => {
                    if ConfigStamp::capture(&ticket.binding.config)? != ticket.stamp {
                        return Err(changed());
                    }
                    cancelled(&self.cancellation)?;
                    let reply = run(
                        Request::Preview {
                            config: ticket.binding.config.clone(),
                            model: ticket.binding.model.clone(),
                            address: ticket.binding.address,
                        },
                        self.cancellation.clone(),
                    )
                    .await?;
                    let Reply::Previewed(info) = reply else {
                        return Err(invalid_reply());
                    };
                    if info != ticket.info
                        || ConfigStamp::capture(&ticket.binding.config)? != ticket.stamp
                    {
                        return Err(changed());
                    }
                    cancelled(&self.cancellation)?;
                    match run(
                        Request::Apply {
                            config: ticket.binding.config,
                            model: ticket.binding.model,
                            address: ticket.binding.address,
                        },
                        self.cancellation.clone(),
                    )
                    .await?
                    {
                        Reply::Applied(_) => Ok(Output::Applied),
                        _ => Err(BridgeError {
                            kind: ErrorKind::InvalidReply,
                            mutation_may_have_run: true,
                        }),
                    }
                }
            }
        }
        .await;
        JobResult {
            kind: self.kind,
            id: self.id,
            generation: self.generation,
            output,
            reconciliation: self.reconciliation,
        }
    }
}

#[cfg(all(
    test,
    any(target_os = "linux", target_os = "android", target_os = "macos")
))]
mod tests {
    use super::*;
    use futures_lite::future::{block_on, ready};
    use std::{
        fs,
        sync::atomic::{AtomicU64, Ordering},
    };
    static NEXT: AtomicU64 = AtomicU64::new(0);
    struct Fixture(PathBuf);
    impl Fixture {
        fn new() -> Self {
            let root = std::env::temp_dir().join(format!(
                "magpie-state-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            fs::create_dir(&root).unwrap();
            let root = root.canonicalize().unwrap();
            fs::write(
                root.join("config.toml"),
                "model = 'old'\nreasoning_effort = 'high'\n",
            )
            .unwrap();
            Self(root)
        }
        fn path(&self) -> PathBuf {
            self.0.join("config.toml")
        }
        fn state(&self) -> ActionsState {
            let mut state = ActionsState::default();
            state.set_config_path(Some(self.path()));
            state.acknowledge_snapshot_refresh(true);
            state.ownership = Some(Ownership::Unmanaged);
            state.catalog = Some(catalog(&["local/model", "local/other", "local/中文"]));
            state.catalog_valid = true;
            state.catalog_generation = 1;
            state.select_model("local/model".into());
            state
        }
        fn preview(&self, state: &mut ActionsState) {
            let job = state.begin(JobKind::Preview, self.path()).unwrap();
            let result = block_on(job.execute_with(|request, _| {
                assert!(matches!(request, Request::Preview { .. }));
                ready(Ok(Reply::Previewed(info())))
            }));
            assert!(state.complete(result));
            assert!(state.can_apply());
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }
    fn catalog(ids: &[&str]) -> GatewayInfo {
        GatewayInfo {
            address: "127.0.0.1:3425".parse().unwrap(),
            models: ids
                .iter()
                .map(|id| ModelInfo { id: (*id).into() })
                .collect(),
        }
    }
    fn info() -> PreviewInfo {
        PreviewInfo {
            config_existed: true,
            changed_fields: vec!["model".into()],
        }
    }
    fn status(fixture: &Fixture, state: &mut ActionsState, ownership: Ownership) {
        let job = state.begin(JobKind::Status, fixture.path()).unwrap();
        let result = block_on(job.execute_with(|request, _| {
            assert!(matches!(request, Request::Status { .. }));
            ready(Ok(Reply::Status(ownership)))
        }));
        assert!(state.complete(result));
    }
    fn apply_result(
        fixture: &Fixture,
        state: &mut ActionsState,
        reply: Result<Reply, BridgeError>,
    ) {
        fixture.preview(state);
        let job = state.begin(JobKind::Apply, fixture.path()).unwrap();
        let result = block_on(job.execute_with(|request, _| {
            ready(if matches!(request, Request::Preview { .. }) {
                Ok(Reply::Previewed(info()))
            } else {
                reply.clone()
            })
        }));
        assert!(state.complete(result));
    }

    #[test]
    fn initial_state_has_no_catalog_selection_or_fake_status() {
        let state = ActionsState::default();
        assert!(state.catalog.is_none());
        assert!(state.selected.is_none());
        assert!(state.ownership.is_none());
        assert!(!state.can_preview());
        assert!(!state.can_disconnect());
        assert!(!state.can_apply());
    }
    #[test]
    fn cjk_search_does_not_claim_ascii_writer_support() {
        let fixture = Fixture::new();
        let mut state = fixture.state();
        assert!(state.set_search("中文".into()));
        assert_eq!(state.visible_models()[0].id, "local/中文");
        assert!(state.select_model("local/中文".into()));
        assert!(state.selected_available());
        assert!(!state.can_preview());
    }
    #[test]
    fn all_relevant_inputs_invalidate_the_preview() {
        let fixture = Fixture::new();
        for choice in 0..4 {
            let mut state = fixture.state();
            fixture.preview(&mut state);
            assert!(match choice {
                0 => state.set_address("127.0.0.1:4000".into()),
                1 => state.set_search("model".into()),
                2 => state.select_model("local/other".into()),
                _ => state.set_config_path(Some(fixture.0.join("other.toml"))),
            });
            assert!(state.preview.is_none());
            assert!(!state.can_apply());
        }
    }
    #[test]
    fn removed_model_keeps_selection_but_blocks_preview() {
        let fixture = Fixture::new();
        let mut state = fixture.state();
        let job = state.begin(JobKind::Inspect, fixture.path()).unwrap();
        state.complete(block_on(job.execute_with(|_, _| {
            ready(Ok(Reply::Inspected(catalog(&["local/other"]))))
        })));
        assert_eq!(state.selected.as_deref(), Some("local/model"));
        assert!(!state.selected_available());
        assert!(!state.can_preview());
    }
    #[test]
    fn failed_query_keeps_input_and_previous_catalog_but_marks_it_stale() {
        let fixture = Fixture::new();
        let mut state = fixture.state();
        state.set_search("model".into());
        let job = state.begin(JobKind::Inspect, fixture.path()).unwrap();
        state.complete(block_on(job.execute_with(|_, _| {
            ready(Err(BridgeError::new(ErrorKind::GatewayUnavailable)))
        })));
        assert_eq!(state.search, "model");
        assert_eq!(state.selected.as_deref(), Some("local/model"));
        assert!(state.catalog.is_some());
        assert!(!state.catalog_is_current());
        assert!(!state.can_preview());
    }
    #[test]
    fn duplicate_and_busy_edits_are_rejected_cancel_waits_for_completion() {
        let fixture = Fixture::new();
        let mut state = fixture.state();
        let job = state.begin(JobKind::Inspect, fixture.path()).unwrap();
        assert!(matches!(
            state.begin(JobKind::Inspect, fixture.path()),
            Err(BridgeError {
                kind: ErrorKind::Busy,
                ..
            })
        ));
        assert!(!state.set_address("127.0.0.1:4000".into()));
        assert!(!state.set_search("different".into()));
        assert!(!state.select_model("local/other".into()));
        state.cancel();
        assert_eq!(state.busy(), Some(JobKind::Inspect));
        let result = block_on(job.execute_with(
            |_, _| -> std::future::Ready<Result<Reply, BridgeError>> {
                panic!("cancelled job must not run")
            },
        ));
        state.complete(result);
        assert!(state.busy().is_none());
        assert_eq!(state.error.unwrap().kind, ErrorKind::Cancelled);
    }
    #[test]
    fn obsolete_request_id_cannot_clear_current_busy() {
        let fixture = Fixture::new();
        let mut state = fixture.state();
        let job = state.begin(JobKind::Inspect, fixture.path()).unwrap();
        let mut result =
            block_on(job.execute_with(|_, _| ready(Ok(Reply::Inspected(catalog(&[]))))));
        result.id = result.id.wrapping_add(1);
        assert!(!state.complete(result));
        assert_eq!(state.busy(), Some(JobKind::Inspect));
    }
    #[test]
    fn obsolete_generation_cannot_publish_catalog() {
        let fixture = Fixture::new();
        let mut state = fixture.state();
        let job = state.begin(JobKind::Inspect, fixture.path()).unwrap();
        state.invalidate_preview();
        let result = block_on(job.execute_with(|_, _| ready(Ok(Reply::Inspected(catalog(&[]))))));
        assert!(!state.complete(result));
        assert!(state.busy().is_none());
        assert_eq!(state.catalog.as_ref().unwrap().models.len(), 3);
        assert!(!state.catalog_is_current());
    }
    #[test]
    fn preview_rejects_config_changed_during_dry_run() {
        let fixture = Fixture::new();
        let mut state = fixture.state();
        let job = state.begin(JobKind::Preview, fixture.path()).unwrap();
        let result = block_on(job.execute_with(|_, _| {
            fs::write(fixture.path(), "model = 'external'\n").unwrap();
            ready(Ok(Reply::Previewed(info())))
        }));
        state.complete(result);
        assert!(state.preview.is_none());
        assert_eq!(state.error.unwrap().kind, ErrorKind::ConfigConflict);
    }
    #[test]
    fn saving_rejects_config_changed_since_preview_before_calling_cli() {
        let fixture = Fixture::new();
        let mut state = fixture.state();
        fixture.preview(&mut state);
        fs::write(fixture.path(), "model = 'external'\n").unwrap();
        let job = state.begin(JobKind::Apply, fixture.path()).unwrap();
        let result = block_on(job.execute_with(
            |_, _| -> std::future::Ready<Result<Reply, BridgeError>> {
                panic!("stale stamp must prevent CLI")
            },
        ));
        state.complete(result);
        assert_eq!(state.error.unwrap().kind, ErrorKind::ConfigConflict);
        assert!(state.reconcile_needed());
    }
    #[test]
    fn saving_rechecks_stamp_after_second_dry_run() {
        let fixture = Fixture::new();
        let mut state = fixture.state();
        fixture.preview(&mut state);
        let job = state.begin(JobKind::Apply, fixture.path()).unwrap();
        let mut calls = 0;
        let result = block_on(job.execute_with(|request, _| {
            calls += 1;
            assert!(matches!(request, Request::Preview { .. }));
            fs::write(fixture.path(), "model = 'external'\n").unwrap();
            ready(Ok(Reply::Previewed(info())))
        }));
        state.complete(result);
        assert_eq!(calls, 1);
        assert_eq!(state.error.unwrap().kind, ErrorKind::ConfigConflict);
    }
    #[test]
    fn saving_rejects_changed_cli_preview() {
        let fixture = Fixture::new();
        let mut state = fixture.state();
        fixture.preview(&mut state);
        let job = state.begin(JobKind::Apply, fixture.path()).unwrap();
        let result = block_on(job.execute_with(|request, _| {
            assert!(matches!(request, Request::Preview { .. }));
            let mut changed = info();
            changed.changed_fields.push("model_provider".into());
            ready(Ok(Reply::Previewed(changed)))
        }));
        state.complete(result);
        assert_eq!(state.error.unwrap().kind, ErrorKind::ConfigConflict);
    }
    #[test]
    fn successful_apply_consumes_ticket_and_needs_both_actual_observations() {
        let fixture = Fixture::new();
        let mut state = fixture.state();
        apply_result(&fixture, &mut state, Ok(Reply::Applied(info())));
        assert!(state.preview.is_none());
        assert!(state.reconcile_needed());
        assert!(state.snapshot_failed());
        assert!(!state.can_disconnect());
        state.acknowledge_snapshot_refresh(true);
        assert!(!state.can_disconnect());
        status(&fixture, &mut state, Ownership::Connected);
        assert!(state.can_disconnect());
        assert_eq!(state.notice, Some(Notice::Connected));
    }
    #[test]
    fn uncertain_write_keeps_error_until_two_fresh_successful_observations() {
        let fixture = Fixture::new();
        let mut state = fixture.state();
        apply_result(
            &fixture,
            &mut state,
            Err(BridgeError {
                kind: ErrorKind::TimedOut,
                mutation_may_have_run: true,
            }),
        );
        assert!(state.uncertain);
        assert!(!state.set_config_path(None));
        state.acknowledge_snapshot_refresh(false);
        status(&fixture, &mut state, Ownership::Connected);
        assert!(state.uncertain);
        assert!(!state.can_disconnect());
        state.acknowledge_snapshot_refresh(true);
        assert!(!state.uncertain);
        assert_eq!(state.notice, Some(Notice::StatusVerified));
        assert_eq!(state.error.unwrap().kind, ErrorKind::TimedOut);
        assert!(state.can_disconnect());
    }
    #[test]
    fn disconnect_requires_confirmation_and_does_not_inspect_gateway() {
        let fixture = Fixture::new();
        let mut state = fixture.state();
        state.ownership = Some(Ownership::Drifted);
        state.set_address("not an address".into());
        assert!(state.can_disconnect());
        assert!(state.begin(JobKind::Disconnect, fixture.path()).is_err());
        state.disconnect_confirmation = true;
        let job = state.begin(JobKind::Disconnect, fixture.path()).unwrap();
        let result = block_on(job.execute_with(|request, _| {
            assert!(matches!(request, Request::Disconnect { .. }));
            ready(Ok(Reply::Disconnected(DisconnectInfo {
                restored_fields: 6,
                preserved_fields: 1,
                config_removed: false,
            })))
        }));
        state.complete(result);
        assert!(state.reconcile_needed());
        state.acknowledge_snapshot_refresh(true);
        status(&fixture, &mut state, Ownership::Unmanaged);
        assert!(matches!(state.notice, Some(Notice::Disconnected(_))));
        assert!(!state.can_disconnect());
    }
    #[test]
    fn status_failure_and_failed_snapshot_keep_writes_blocked() {
        let fixture = Fixture::new();
        let mut state = fixture.state();
        apply_result(&fixture, &mut state, Ok(Reply::Applied(info())));
        let job = state.begin(JobKind::Status, fixture.path()).unwrap();
        state.complete(block_on(job.execute_with(|_, _| {
            ready(Err(BridgeError::new(ErrorKind::CliMissing)))
        })));
        state.acknowledge_snapshot_refresh(false);
        assert!(!state.can_disconnect());
        assert!(state.reconcile_needed());
        assert!(!state.set_config_path(None));
    }
    #[test]
    fn changed_post_operation_ownership_does_not_leave_false_success_notice() {
        let fixture = Fixture::new();
        let mut state = fixture.state();
        apply_result(&fixture, &mut state, Ok(Reply::Applied(info())));
        state.acknowledge_snapshot_refresh(true);
        status(&fixture, &mut state, Ownership::Drifted);
        assert_eq!(state.notice, Some(Notice::StatusVerified));
        assert_eq!(state.ownership, Some(Ownership::Drifted));
    }
    #[test]
    fn stale_mutation_result_still_demands_reconciliation() {
        let fixture = Fixture::new();
        let mut state = fixture.state();
        fixture.preview(&mut state);
        let job = state.begin(JobKind::Apply, fixture.path()).unwrap();
        state.invalidate_preview();
        let result = block_on(job.execute_with(|request, _| {
            ready(Ok(if matches!(request, Request::Preview { .. }) {
                Reply::Previewed(info())
            } else {
                Reply::Applied(info())
            }))
        }));
        assert!(!state.complete(result));
        assert!(state.uncertain);
        assert!(state.reconcile_needed());
        assert!(state.snapshot_failed());
    }

    /// Invoked alone by tests/desktop_actions.py, with child-only fixture env.
    #[test]
    #[ignore = "requires the isolated desktop_actions.py coordinator and real CLI/Codex"]
    fn real_cli_native_codex_roundtrip() {
        use crate::snapshot::{Agent, Snapshot};
        use magpie_core::agents::ConfigFileState;
        use std::{
            env,
            io::Write,
            os::unix::fs::{MetadataExt, OpenOptionsExt},
            thread,
            time::{Duration, Instant},
        };

        fn refresh(state: &mut ActionsState, path: &Path) -> Agent {
            let agent = Snapshot::read()
                .agents
                .into_iter()
                .find(|agent| agent.id == "codex")
                .expect("isolated PATH must discover the installed Codex");
            let config = agent.config.as_ref().expect("Codex has a config path");
            assert_eq!(config.path.as_deref(), Some(path));
            assert!(!agent.last_known, "must observe a fresh Snapshot");
            let fresh = config.state == ConfigFileState::Missing || config.fields_readable;
            assert!(fresh, "isolated Codex config must be readable or missing");
            state.set_config_path(Some(path.to_path_buf()));
            assert_eq!(state.config_path(), Some(path));
            state.acknowledge_snapshot_refresh(fresh);
            agent
        }

        fn execute(state: &mut ActionsState, path: &Path, kind: JobKind) {
            let job = state
                .begin(kind, path.to_path_buf())
                .unwrap_or_else(|error| panic!("real CLI {kind:?} admission: {error}"));
            assert!(
                state.complete(block_on(job.execute())),
                "{kind:?} was stale"
            );
            assert!(state.busy().is_none(), "{kind:?} remained busy");
            assert!(
                state.error.is_none(),
                "real CLI {kind:?} failed: {:?}",
                state.error
            );
        }

        fn read_marker(path: &Path, token: &str, phase: &str) -> serde_json::Value {
            let metadata = fs::symlink_metadata(path).expect("fixture marker metadata");
            assert!(metadata.is_file() && !metadata.file_type().is_symlink());
            assert!(metadata.len() <= 4096, "fixture marker is bounded");
            let value: serde_json::Value =
                serde_json::from_slice(&fs::read(path).expect("read fixture marker"))
                    .expect("fixture marker JSON");
            assert_eq!(value["token"].as_str(), Some(token));
            assert_eq!(value["phase"].as_str(), Some(phase));
            value
        }

        fn write_marker(root: &Path, token: &str, phase: &str) {
            let temporary = root.join(format!(".{phase}.pending"));
            let mut file = fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o600)
                .open(&temporary)
                .expect("create unique fixture marker");
            let bytes = serde_json::to_vec(&serde_json::json!({
                "token": token, "phase": phase
            }))
            .expect("serialize fixture marker");
            file.write_all(&bytes).expect("write fixture marker");
            drop(file);
            assert!(!root.join(phase).exists(), "marker must be published once");
            fs::rename(temporary, root.join(phase)).expect("publish complete fixture marker");
        }

        // Check isolation before discovery, ConfigStamp, or any CLI invocation.
        let root = PathBuf::from(
            env::var_os("MAGPIE_DESKTOP_ACTIONS_ROOT").expect("coordinator fixture root"),
        );
        assert!(root.is_absolute());
        assert_eq!(root.canonicalize().expect("canonical fixture root"), root);
        assert!(
            root.file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.starts_with("magpie-desktop-actions-"))
        );
        let metadata = fs::symlink_metadata(&root).expect("fixture root metadata");
        assert!(metadata.is_dir() && metadata.mode() & 0o077 == 0);
        assert_eq!(metadata.uid(), rustix::process::geteuid().as_raw());
        for (variable, directory) in [
            ("HOME", "home"),
            ("CODEX_HOME", "codex"),
            ("XDG_CONFIG_HOME", "xdg"),
            ("XDG_CACHE_HOME", "cache"),
            ("TMPDIR", "tmp"),
        ] {
            assert_eq!(
                env::var_os(variable),
                Some(root.join(directory).into_os_string())
            );
            let path = root.join(directory);
            assert_eq!(
                path.canonicalize().expect("private fixture directory"),
                path
            );
        }
        assert_eq!(
            env::current_dir().expect("fixture working directory"),
            root.join("work")
        );
        let cli = PathBuf::from(env::var_os("MAGPIE_CLI_PATH").expect("explicit real CLI"));
        assert!(cli.is_absolute() && cli.is_file());
        let token = env::var("MAGPIE_DESKTOP_ACTIONS_TOKEN").expect("coordinator fixture token");
        assert!(token.len() == 48 && token.bytes().all(|byte| byte.is_ascii_hexdigit()));
        let manifest = read_marker(&root.join("fixture"), &token, "fixture");
        let existing = match manifest["case"].as_str() {
            Some("existing") => true,
            Some("missing") => false,
            _ => panic!("unknown isolated fixture case"),
        };
        let address = manifest["gateway"].as_str().expect("fixture gateway");
        cli_bridge::parse_address(address).expect("literal loopback fixture gateway");
        let config = root.join("codex/config.toml");
        let original = if existing {
            let metadata = fs::symlink_metadata(&config).expect("initial fixture config");
            assert!(metadata.is_file() && !metadata.file_type().is_symlink());
            assert!(metadata.len() <= 4096);
            Some(fs::read(&config).expect("read initial fixture bytes"))
        } else {
            assert!(!config.try_exists().expect("initial config absence"));
            None
        };
        let original_stamp = ConfigStamp::capture(&config).expect("initial private observation");
        let mut state = ActionsState::default();
        let initial = refresh(&mut state, &config);
        assert_eq!(
            initial.config.as_ref().expect("initial config state").state,
            if existing {
                ConfigFileState::ValidToml
            } else {
                ConfigFileState::Missing
            }
        );
        state.set_address(address.to_owned());
        execute(&mut state, &config, JobKind::Inspect);
        assert!(state.catalog_is_current());
        assert!(
            state
                .catalog
                .as_ref()
                .expect("real catalog")
                .models
                .iter()
                .any(|model| model.id == "local/native-fixture")
        );
        execute(&mut state, &config, JobKind::Status);
        assert_eq!(state.ownership, Some(Ownership::Unmanaged));
        assert!(state.select_model("local/native-fixture".into()));
        assert!(state.can_preview());
        execute(&mut state, &config, JobKind::Preview);
        assert!(state.can_apply());
        assert_eq!(
            state.preview.as_ref().expect("real preview").stamp,
            original_stamp
        );
        assert_eq!(
            ConfigStamp::capture(&config).expect("post-preview stamp"),
            original_stamp
        );
        assert!(
            if let Some(bytes) = &original {
                fs::read(&config).expect("post-preview bytes") == *bytes
            } else {
                !config.try_exists().expect("preview must not create config")
            },
            "preview must preserve the exact original bytes or absence"
        );
        assert!(
            !root
                .join("codex/.config.toml.magpie-state")
                .try_exists()
                .expect("preview transaction-state lookup"),
            "preview must not create transaction state"
        );
        execute(&mut state, &config, JobKind::Apply);
        assert!(state.reconcile_needed() && state.snapshot_failed());
        let connected = refresh(&mut state, &config);
        let settings = connected
            .settings
            .as_ref()
            .expect("fresh connected selection");
        assert_eq!(settings.model.as_deref(), Some("local/native-fixture"));
        assert_eq!(settings.provider.as_deref(), Some("magpie"));
        assert_eq!(
            settings.effort.as_deref(),
            initial
                .settings
                .as_ref()
                .and_then(|settings| settings.effort.as_deref())
        );
        execute(&mut state, &config, JobKind::Status);
        assert_eq!(state.ownership, Some(Ownership::Connected));
        assert!(!state.reconcile_needed() && !state.snapshot_failed());
        assert!(state.can_disconnect());
        write_marker(&root, &token, "connected");

        // The coordinator runs Codex against this exact config, then reaps its
        // gateway before publishing the marker. Disconnect must work offline.
        let stopped = root.join("gateway-stopped");
        let deadline = Instant::now() + Duration::from_secs(90);
        while !stopped.try_exists().expect("gateway marker lookup") {
            assert!(
                Instant::now() < deadline,
                "coordinator gateway-stop deadline"
            );
            thread::sleep(Duration::from_millis(20));
        }
        read_marker(&stopped, &token, "gateway-stopped");
        state.disconnect_confirmation = true;
        execute(&mut state, &config, JobKind::Disconnect);
        assert!(state.reconcile_needed() && state.snapshot_failed());
        let restored = refresh(&mut state, &config);
        execute(&mut state, &config, JobKind::Status);
        assert_eq!(state.ownership, Some(Ownership::Unmanaged));
        assert!(!state.reconcile_needed() && !state.snapshot_failed());
        assert!(!state.can_disconnect());
        assert_eq!(
            restored
                .config
                .as_ref()
                .expect("restored config state")
                .state,
            initial.config.as_ref().expect("initial config state").state
        );
        if let Some(bytes) = original {
            assert!(fs::read(&config).expect("restored fixture bytes") == bytes);
            let expected = initial.settings.as_ref().expect("original selection");
            let actual = restored.settings.as_ref().expect("restored selection");
            assert_eq!(actual.model, expected.model);
            assert_eq!(actual.provider, expected.provider);
            assert_eq!(actual.effort, expected.effort);
        } else {
            assert!(!config.try_exists().expect("restored config absence"));
            assert!(restored.settings.is_none());
        }
        write_marker(&root, &token, "restored");
    }
}

#[cfg(all(
    test,
    not(any(target_os = "linux", target_os = "android", target_os = "macos"))
))]
mod unsupported_platform_tests {
    use super::*;
    #[test]
    fn writes_are_disabled_but_read_only_inspect_is_admitted() {
        let mut state = ActionsState::default();
        assert!(!state.can_preview());
        assert!(!state.can_disconnect());
        assert!(state.begin(JobKind::Inspect, PathBuf::new()).is_ok());
    }
}
