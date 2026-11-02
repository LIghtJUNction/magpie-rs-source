//! Private, bounded recent-request metadata, separate from billing and body recording.
//!
//! Two 4 MiB JSONL segments retain a recent window. A 128-record queue is
//! best effort; health exposes losses since this server started. Each accepted
//! write is synced before it is counted as written. Supervised shutdown drains
//! the queue; a kill can lose queued records or leave one incomplete tail.

pub mod command;

use serde_json::{Value, json};
use std::{
    io,
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
        mpsc::{self, SyncSender},
    },
    time::{Duration, Instant},
};

#[cfg(any(target_os = "linux", target_os = "android", target_os = "macos"))]
mod native;

/// Manifest registered by the application bundle, never auto-loaded by the Gateway.
pub const MANIFEST: &str = include_str!("../plugin.toml");
/// Stable activation owner for this plugin's command, page and observer.
pub const PLUGIN_ID: &str = "feature.usage";

const FILE_LIMIT: u64 = 4 * 1024 * 1024;
const RECORD_LIMIT: usize = 4096;
const QUEUE_LIMIT: usize = 128;

/// Appends `.usage` to the entire store path so differently named stores stay isolated.
pub fn path_for_store(store: &Path) -> PathBuf {
    let mut path = store.as_os_str().to_os_string();
    path.push(".usage");
    PathBuf::from(path)
}

#[cfg(test)]
fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .try_into()
        .unwrap_or(u64::MAX)
}

use RequestMetadata as Record;
use TokenUsage as Usage;
use magpie_core::request_metadata::MetadataSubscriber;
pub use magpie_core::request_metadata::{RequestMetadata, RequestOutcome, TokenUsage};

fn valid_record(record: &Record) -> bool {
    record.schema == 1
        && !record.id.is_empty()
        && record.id.len() <= 128
        && record
            .id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"_-".contains(&b))
        && magpie_core::catalog::valid_provider_id(&record.provider_id)
        && magpie_core::catalog::valid_model_id(&record.upstream_model)
        && record.model == format!("{}/{}", record.provider_id, record.upstream_model)
        && [record.protocol.as_str(), record.upstream_protocol.as_str()]
            .iter()
            .all(|protocol| {
                !protocol.is_empty()
                    && protocol.len() <= 64
                    && protocol.bytes().all(|b| b.is_ascii_graphic())
            })
}
#[derive(Default)]
struct Counters {
    pending: AtomicU64,
    written: AtomicU64,
    failed: AtomicU64,
    dropped: AtomicU64,
    repaired_tails: AtomicU64,
}

enum Message {
    Record(Box<Record>),
    Flush(SyncSender<()>),
}

#[derive(Clone)]
pub struct UsagePlugin {
    sender: Option<SyncSender<Message>>,
    counters: Arc<Counters>,
}

impl UsagePlugin {
    pub fn new(store: &Path) -> Self {
        let path = path_for_store(store);
        let (sender, receiver) = mpsc::sync_channel(QUEUE_LIMIT);
        let counters = Arc::new(Counters::default());
        let progress = counters.clone();
        let worker = std::thread::Builder::new()
            .name("magpie-usage".into())
            .spawn(move || {
                for message in receiver {
                    match message {
                        Message::Record(record) => {
                            match append(&path, &record) {
                                Ok(repaired) => {
                                    progress.written.fetch_add(1, Ordering::Relaxed);
                                    progress
                                        .repaired_tails
                                        .fetch_add(u64::from(repaired), Ordering::Relaxed);
                                }
                                Err(_) => {
                                    progress.failed.fetch_add(1, Ordering::Relaxed);
                                }
                            }
                            progress.pending.fetch_sub(1, Ordering::Release);
                        }
                        Message::Flush(done) => {
                            let _ = done.send(());
                        }
                    }
                }
            });
        let sender = if worker.is_ok() {
            Some(sender)
        } else {
            counters.failed.fetch_add(1, Ordering::Relaxed);
            None
        };
        Self { sender, counters }
    }

    fn record(&self, record: Record) {
        self.counters.pending.fetch_add(1, Ordering::Relaxed);
        if self
            .sender
            .as_ref()
            .is_none_or(|sender| sender.try_send(Message::Record(Box::new(record))).is_err())
        {
            self.counters.dropped.fetch_add(1, Ordering::Relaxed);
            self.counters.pending.fetch_sub(1, Ordering::Release);
        }
    }

    pub fn health(&self) -> Value {
        json!({"scope":"recent_requests", "mode":"metadata_only", "queue_capacity":QUEUE_LIMIT,
            "pending":self.counters.pending.load(Ordering::Acquire),
            "written":self.counters.written.load(Ordering::Relaxed),
            "failed":self.counters.failed.load(Ordering::Relaxed),
            "dropped":self.counters.dropped.load(Ordering::Relaxed),
            "repaired_tails":self.counters.repaired_tails.load(Ordering::Relaxed),
            "counter_scope":"server_lifetime", "segment_bytes":FILE_LIMIT, "segments":2,
            "durability":"sync_per_record_after_queue", "full_usage_accounting":false})
    }

    /// Call after request tasks have been dropped. A barrier confirms all earlier writes.
    pub fn flush(&self) -> io::Result<()> {
        let Some(sender) = &self.sender else {
            return Err(invalid("usage_worker_unavailable"));
        };
        let (done, receipt) = mpsc::sync_channel(1);
        let deadline = Instant::now() + Duration::from_secs(3);
        let mut message = Message::Flush(done);
        loop {
            match sender.try_send(message) {
                Ok(()) => break,
                Err(mpsc::TrySendError::Full(returned)) if Instant::now() < deadline => {
                    message = returned;
                    std::thread::sleep(Duration::from_millis(5));
                }
                Err(_) => return Err(invalid("usage_flush_failed")),
            }
        }
        receipt
            .recv_timeout(deadline.saturating_duration_since(Instant::now()))
            .map_err(|_| invalid("usage_flush_failed"))
    }
}

impl MetadataSubscriber for UsagePlugin {
    fn on_request_finished(&self, event: &RequestMetadata) {
        self.record(event.clone());
    }
    fn health(&self) -> Value {
        UsagePlugin::health(self)
    }
    fn flush(&self) -> io::Result<()> {
        UsagePlugin::flush(self)
    }
}

fn invalid(code: &'static str) -> io::Error {
    io::Error::other(code)
}

fn append(path: &Path, record: &Record) -> io::Result<bool> {
    if !valid_record(record) {
        return Err(invalid("usage_invalid_record"));
    }
    let mut bytes = serde_json::to_vec(record)?;
    bytes.push(b'\n');
    if bytes.len() > RECORD_LIMIT {
        return Err(invalid("usage_record_too_large"));
    }
    #[cfg(any(target_os = "linux", target_os = "android", target_os = "macos"))]
    {
        native::append(path, &bytes)
    }
    #[cfg(not(any(target_os = "linux", target_os = "android", target_os = "macos")))]
    {
        let _ = (path, bytes);
        Err(invalid("usage_platform_unsupported"))
    }
}

fn read_segments(path: &Path) -> io::Result<Vec<Vec<u8>>> {
    #[cfg(any(target_os = "linux", target_os = "android", target_os = "macos"))]
    {
        native::read(path)
    }
    #[cfg(not(any(target_os = "linux", target_os = "android", target_os = "macos")))]
    {
        let _ = path;
        Err(invalid("usage_platform_unsupported"))
    }
}

/// Reads metadata offline without loading Provider configuration or credentials.
/// Totals describe only the returned recent window; unknown counts remain null.
///
/// # Errors
/// Rejects unsafe paths/files, busy locks, invalid limits and oversized segments.
pub fn report(store: &Path, limit: usize) -> io::Result<Value> {
    if !store.is_absolute() || !(1..=1000).contains(&limit) {
        return Err(invalid("usage_invalid_request"));
    }
    let segments = read_segments(&path_for_store(store))?;
    let mut records: Vec<Record> = Vec::with_capacity(limit + 1);
    let mut incomplete = 0u64;
    for bytes in segments {
        for line in bytes.split_inclusive(|b| *b == b'\n') {
            if line.len() > RECORD_LIMIT {
                return Err(invalid("usage_record_too_large"));
            }
            if line.last() != Some(&b'\n') {
                incomplete += 1;
                continue;
            }
            let record = serde_json::from_slice::<Record>(line)
                .ok()
                .filter(valid_record);
            match record {
                Some(record) => {
                    let position = records
                        .binary_search_by(|old| {
                            old.started_at_ms
                                .cmp(&record.started_at_ms)
                                .then_with(|| old.id.cmp(&record.id))
                        })
                        .unwrap_or_else(|position| position);
                    records.insert(position, record);
                    if records.len() > limit {
                        records.remove(0);
                    }
                }
                None => incomplete += 1,
            }
        }
    }
    // Append order can differ from request start order across concurrent servers.
    records.reverse();
    let completed = records
        .iter()
        .filter(|r| matches!(r.outcome, RequestOutcome::Completed))
        .count();
    let failed = records
        .iter()
        .filter(|r| matches!(r.outcome, RequestOutcome::Failed))
        .count();
    let cancelled = records
        .iter()
        .filter(|r| matches!(r.outcome, RequestOutcome::Cancelled))
        .count();
    let sum = |field: fn(&Usage) -> Option<u64>| {
        let values: Vec<_> = records
            .iter()
            .filter_map(|r| r.usage.as_ref().and_then(field))
            .collect();
        let total = if values.is_empty() {
            None
        } else {
            values.iter().try_fold(0u64, |sum, n| sum.checked_add(*n))
        };
        (total, values.len())
    };
    let (input, input_reported) = sum(|u| u.input_tokens);
    let (output, output_reported) = sum(|u| u.output_tokens);
    let (total, total_reported) = sum(|u| u.total_tokens);
    Ok(
        json!({"version":"0.2.0", "scope":"recent_requests", "limit":limit,
        "incomplete_records":incomplete, "totals":{"requests":records.len(), "completed":completed,
        "failed":failed,"cancelled":cancelled,"reported_requests":records.iter().filter(|r| r.usage.is_some()).count(),
        "input_tokens":input,"output_tokens":output,"total_tokens":total,
        "input_tokens_reported":input_reported,"output_tokens_reported":output_reported,"total_tokens_reported":total_reported,
        "token_sum_overflow":(input.is_none() && input_reported > 0) || (output.is_none() && output_reported > 0) || (total.is_none() && total_reported > 0)},
        "records":records}),
    )
}

#[cfg(all(
    test,
    any(target_os = "linux", target_os = "android", target_os = "macos")
))]
mod tests {
    use super::*;
    use std::{
        fs,
        io::Write,
        os::unix::fs::{DirBuilderExt, PermissionsExt, symlink},
        process::Command,
    };

    struct Fixture(PathBuf);
    impl Fixture {
        fn new() -> Self {
            static NEXT: AtomicU64 = AtomicU64::new(0);
            let path = std::env::temp_dir().canonicalize().unwrap().join(format!(
                "magpie-usage-{}-{}-{}",
                std::process::id(),
                now_ms(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            fs::DirBuilder::new().mode(0o700).create(&path).unwrap();
            Self(path.join("providers.toml"))
        }
        fn directory(&self) -> PathBuf {
            path_for_store(&self.0)
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(self.0.parent().unwrap());
        }
    }
    fn record(id: &str, usage: Option<Usage>) -> Record {
        Record {
            schema: 1,
            id: id.into(),
            started_at_ms: now_ms(),
            duration_ms: 1,
            provider_id: "fixture".into(),
            model: "fixture/test".into(),
            upstream_model: "test".into(),
            protocol: "openai_chat".into(),
            upstream_protocol: "openai_chat".into(),
            stream: false,
            outcome: RequestOutcome::Completed,
            usage,
        }
    }

    #[test]
    fn registry_activation_and_withdrawal_gate_the_plugin_observer_without_deleting_data() {
        use magpie_core::plugins::{PluginInput, PluginRegistry};
        let fixture = Fixture::new();
        let mut registry =
            PluginRegistry::from_manifests([PluginInput::bundled("usage", MANIFEST)]).unwrap();
        assert!(
            registry.contributions().is_empty(),
            "parsing does not activate the observer"
        );
        let pending = registry.pending_contributions();
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].owner, PLUGIN_ID);
        for name in ["commands", "pages", "metadata_subscribers"] {
            assert!(
                pending[0].manifest.document()["contributions"][name]
                    .as_array_of_tables()
                    .is_some()
            );
        }
        let plugin = UsagePlugin::new(&fixture.0);
        registry.acknowledge_activation(PLUGIN_ID).unwrap();
        assert_eq!(registry.contributions().len(), 1);
        plugin.on_request_finished(&record("before_disable", None));
        plugin.flush().unwrap();
        let original = super::report(&fixture.0, 100).unwrap();
        let withdrawn = registry
            .set_enabled(PLUGIN_ID, false, registry.revision())
            .unwrap();
        assert_eq!(withdrawn, vec![PLUGIN_ID.to_owned()]);
        assert!(registry.contributions().is_empty());
        drop(plugin);
        assert_eq!(super::report(&fixture.0, 100).unwrap(), original);
        registry
            .set_enabled(PLUGIN_ID, true, registry.revision())
            .unwrap();
        assert!(
            registry.contributions().is_empty(),
            "re-enable requires fresh real registration"
        );
        let plugin = UsagePlugin::new(&fixture.0);
        registry.acknowledge_activation(PLUGIN_ID).unwrap();
        plugin.on_request_finished(&record("after_enable", None));
        plugin.flush().unwrap();
        assert_eq!(
            super::report(&fixture.0, 100).unwrap()["totals"]["requests"],
            2
        );
    }

    #[test]
    fn unknown_totals_are_not_invented() {
        let fixture = Fixture::new();
        append(&fixture.directory(), &record("unknown", None)).unwrap();
        let report = super::report(&fixture.0, 100).unwrap();
        assert_eq!(report["totals"]["reported_requests"], 0);
        for field in ["input_tokens", "output_tokens", "total_tokens"] {
            assert!(report["totals"][field].is_null());
        }
        append(
            &fixture.directory(),
            &record(
                "partial",
                Some(Usage {
                    input_tokens: Some(7),
                    ..Usage::default()
                }),
            ),
        )
        .unwrap();
        let report = super::report(&fixture.0, 100).unwrap();
        assert_eq!(report["totals"]["requests"], 2);
        assert_eq!(report["totals"]["reported_requests"], 1);
        assert_eq!(report["totals"]["input_tokens"], 7);
        assert_eq!(report["totals"]["input_tokens_reported"], 1);
        assert!(report["totals"]["output_tokens"].is_null());
        assert!(report["totals"]["total_tokens"].is_null());
    }

    #[test]
    fn maximum_supported_metadata_fits_frontend_budget_and_overflow_is_explicit() {
        let fixture = Fixture::new();
        for index in 0..100 {
            let mut record = record(
                &format!("{index:0128}"),
                Some(Usage {
                    input_tokens: Some(u64::MAX),
                    output_tokens: Some(u64::MAX),
                    total_tokens: Some(u64::MAX),
                    cached_input_tokens: Some(u64::MAX),
                    cache_creation_input_tokens: Some(u64::MAX),
                    reasoning_tokens: Some(u64::MAX),
                }),
            );
            record.provider_id = "p".repeat(128);
            record.upstream_model = "m".repeat(256);
            record.model = format!("{}/{}", record.provider_id, record.upstream_model);
            record.started_at_ms = u64::MAX - index;
            record.duration_ms = u64::MAX;
            record.protocol = "anthropic_messages".into();
            record.upstream_protocol = "anthropic_messages".into();
            append(&fixture.directory(), &record).unwrap();
        }
        let report = super::report(&fixture.0, 100).unwrap();
        assert!(
            serde_json::to_vec(&report).unwrap().len() < 1024 * 1024,
            "limit=100 stays inside the UI boundary for maximum currently valid IDs and counts"
        );
        assert_eq!(report["totals"]["requests"], 100);
        assert_eq!(report["totals"]["reported_requests"], 100);
        assert_eq!(report["totals"]["token_sum_overflow"], true);
        for field in ["input_tokens", "output_tokens", "total_tokens"] {
            assert!(report["totals"][field].is_null());
        }
        assert_eq!(report["records"][0]["started_at_ms"], u64::MAX);
        assert_eq!(report["records"][99]["started_at_ms"], u64::MAX - 99);
    }

    #[test]
    fn different_store_extensions_have_independent_recent_windows() {
        let fixture = Fixture::new();
        let other = fixture.0.with_extension("json");
        append(&fixture.directory(), &record("toml_store", None)).unwrap();
        append(&path_for_store(&other), &record("json_store", None)).unwrap();
        assert_eq!(
            super::report(&fixture.0, 100).unwrap()["records"][0]["id"],
            "toml_store"
        );
        assert_eq!(
            super::report(&other, 100).unwrap()["records"][0]["id"],
            "json_store"
        );
    }

    #[test]
    fn flush_persists_and_bad_tail_is_reported_then_repaired_before_append() {
        let fixture = Fixture::new();
        let recorder = UsagePlugin::new(&fixture.0);
        recorder.record(record("first", None));
        recorder.flush().unwrap();
        assert_eq!(recorder.health()["written"], 1);
        assert_eq!(
            fs::metadata(fixture.directory())
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o700
        );
        let path = fixture.directory().join("current.jsonl");
        assert_eq!(
            fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
        fs::OpenOptions::new()
            .append(true)
            .open(&path)
            .unwrap()
            .write_all(b"{broken tail")
            .unwrap();
        assert_eq!(
            super::report(&fixture.0, 100).unwrap()["incomplete_records"],
            1
        );
        assert!(append(&fixture.directory(), &record("second", None)).unwrap());
        assert_eq!(
            super::report(&fixture.0, 100).unwrap()["totals"]["requests"],
            2
        );
        assert_eq!(
            super::report(&fixture.0, 100).unwrap()["incomplete_records"],
            0
        );
        drop(recorder);
        assert_eq!(
            super::report(&fixture.0, 1).unwrap()["records"]
                .as_array()
                .unwrap()
                .len(),
            1
        );
        for limit in [0, 1001] {
            assert!(super::report(&fixture.0, limit).is_err());
        }
    }

    #[test]
    fn unsafe_files_directories_links_and_injected_payloads_are_rejected() {
        let fixture = Fixture::new();
        append(&fixture.directory(), &record("first", None)).unwrap();
        let dir = fixture.directory();
        let file = dir.join("current.jsonl");
        let safe = fs::read(&file).unwrap();
        fs::set_permissions(&file, fs::Permissions::from_mode(0o644)).unwrap();
        assert!(super::report(&fixture.0, 100).is_err());
        assert!(append(&dir, &record("no", None)).is_err());
        fs::set_permissions(&file, fs::Permissions::from_mode(0o600)).unwrap();
        fs::rename(&file, dir.join("saved")).unwrap();
        symlink("saved", &file).unwrap();
        assert!(super::report(&fixture.0, 100).is_err());
        assert!(append(&dir, &record("no", None)).is_err());
        fs::remove_file(&file).unwrap();
        fs::hard_link(dir.join("saved"), &file).unwrap();
        assert!(super::report(&fixture.0, 100).is_err());
        fs::remove_file(&file).unwrap();
        fs::rename(dir.join("saved"), &file).unwrap();
        let mut injected: Value = serde_json::from_slice(&safe).unwrap();
        injected["prompt"] = json!("private-body");
        fs::write(&file, format!("{injected}\n")).unwrap();
        let result = super::report(&fixture.0, 100).unwrap();
        assert_eq!(result["incomplete_records"], 1);
        assert!(!result.to_string().contains("private-body"));
        fs::set_permissions(&dir, fs::Permissions::from_mode(0o755)).unwrap();
        assert!(super::report(&fixture.0, 100).is_err());
        fs::set_permissions(&dir, fs::Permissions::from_mode(0o700)).unwrap();
        let alias = fixture.0.parent().unwrap().join("alias");
        symlink(&dir, &alias).unwrap();
        assert!(native::read(&alias).is_err());
        assert!(native::read(&alias.join("nested")).is_err());
        assert!(report(Path::new("relative.toml"), 1).is_err());
    }

    #[test]
    fn rotation_and_size_caps_keep_a_recent_window() {
        let fixture = Fixture::new();
        append(&fixture.directory(), &record("first", None)).unwrap();
        let file = fixture.directory().join("current.jsonl");
        // Fill with bounded invalid lines to reach a rotation boundary cheaply.
        let mut data = vec![b'\n'; FILE_LIMIT as usize];
        let encoded = serde_json::to_vec(&record("last", None)).unwrap();
        data[..encoded.len()].copy_from_slice(&encoded);
        fs::write(&file, data).unwrap();
        append(&fixture.directory(), &record("new", None)).unwrap();
        assert!(fixture.directory().join("previous.jsonl").exists());
        fs::OpenOptions::new()
            .write(true)
            .open(fixture.directory().join("previous.jsonl"))
            .unwrap()
            .set_len(FILE_LIMIT + 1)
            .unwrap();
        assert!(super::report(&fixture.0, 100).is_err());
    }

    #[test]
    fn bounded_queue_exposes_drops_while_a_competing_process_holds_the_lock() {
        let fixture = Fixture::new();
        append(&fixture.directory(), &record("initial", None)).unwrap();
        let lock = fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(fixture.directory().join("lock"))
            .unwrap();
        rustix::fs::flock(&lock, rustix::fs::FlockOperation::LockExclusive).unwrap();
        let recorder = UsagePlugin::new(&fixture.0);
        for n in 0..200 {
            recorder.record(record(&format!("queued_{n}"), None));
        }
        let before = recorder.health();
        assert!(
            before["dropped"].as_u64().unwrap() >= 71,
            "queue stays bounded: {before}"
        );
        drop(lock);
        recorder.flush().unwrap();
        let after = recorder.health();
        assert_eq!(after["pending"], 0);
        assert_eq!(
            after["written"].as_u64().unwrap()
                + after["failed"].as_u64().unwrap()
                + after["dropped"].as_u64().unwrap(),
            200
        );
    }

    #[test]
    fn writer_process() {
        let Ok(path) = std::env::var("MAGPIE_USAGE_TEST_STORE") else {
            return;
        };
        let tag = std::env::var("MAGPIE_USAGE_TEST_TAG").unwrap();
        for n in 0..30 {
            append(
                &path_for_store(Path::new(&path)),
                &record(&format!("{tag}_{n}"), None),
            )
            .unwrap();
        }
    }

    #[test]
    fn independent_processes_append_without_interleaving_or_loss() {
        let fixture = Fixture::new();
        // Establish the lock before launching competing children.
        append(&fixture.directory(), &record("initial", None)).unwrap();
        let mut children: Vec<_> = (0..4)
            .map(|tag| {
                Command::new(std::env::current_exe().unwrap())
                    .args(["--exact", "tests::writer_process"])
                    .env("MAGPIE_USAGE_TEST_STORE", &fixture.0)
                    .env("MAGPIE_USAGE_TEST_TAG", format!("child_{tag}"))
                    .stdout(std::process::Stdio::null())
                    .spawn()
                    .unwrap()
            })
            .collect();
        for child in &mut children {
            assert!(child.wait().unwrap().success());
        }
        let report = super::report(&fixture.0, 1000).unwrap();
        assert_eq!(report["incomplete_records"], 0);
        assert_eq!(report["totals"]["requests"], 121);
        let ids: std::collections::BTreeSet<_> = report["records"]
            .as_array()
            .unwrap()
            .iter()
            .map(|r| r["id"].as_str().unwrap())
            .collect();
        assert_eq!(ids.len(), 121);
    }
}
