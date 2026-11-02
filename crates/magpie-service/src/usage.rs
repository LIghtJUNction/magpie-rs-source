//! Bounded local request metadata. Prompts, replies, headers and keys never enter this log.
use crate::protocol::{Protocol, Usage};
use serde_json::{Value, json};
use std::{
    fs::{self, OpenOptions},
    io::{self, BufRead, BufReader, Write},
    path::{Path, PathBuf},
    sync::{Arc, atomic::{AtomicU64, Ordering}, mpsc::{self, SyncSender}},
    time::{SystemTime, UNIX_EPOCH},
};

const FILE_LIMIT: u64 = 4 * 1024 * 1024;
const RECORD_LIMIT: usize = 4096;

pub fn path_for_store(store: &Path) -> PathBuf { store.with_extension("usage.jsonl") }

pub(crate) fn now_ms() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_millis().try_into().unwrap_or(u64::MAX)
}

#[derive(Clone)]
pub(crate) struct Recorder {
    sender: SyncSender<Value>,
    pub(crate) dropped: Arc<AtomicU64>,
}

impl Recorder {
    pub(crate) fn new(store: &Path) -> io::Result<Self> {
        let path = path_for_store(store);
        let (sender, receiver) = mpsc::sync_channel(128);
        let dropped = Arc::new(AtomicU64::new(0));
        let failures = dropped.clone();
        std::thread::Builder::new().name("magpie-usage".into()).spawn(move || {
            for event in receiver {
                if append(&path, &event).is_err() { failures.fetch_add(1, Ordering::Relaxed); }
            }
        })?;
        Ok(Self { sender, dropped })
    }

    pub(crate) fn record(&self, event: Value) {
        if self.sender.try_send(event).is_err() { self.dropped.fetch_add(1, Ordering::Relaxed); }
    }
}

fn private_file(path: &Path, append: bool) -> io::Result<fs::File> {
    if !path.is_absolute() { return Err(io::Error::other("usage_path_not_absolute")); }
    let mut options = OpenOptions::new();
    options.create(true).read(true).write(true).append(append);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
        // libc O_NOFOLLOW values are supplied by rustix through the service dependency.
        options.custom_flags(rustix::fs::OFlags::NOFOLLOW.bits() as i32);
    }
    let file = options.open(path)?;
    if !file.metadata()?.is_file() { return Err(io::Error::other("usage_not_a_file")); }
    Ok(file)
}

fn append(path: &Path, event: &Value) -> io::Result<()> {
    let mut bytes = serde_json::to_vec(event)?;
    if bytes.len() >= RECORD_LIMIT { return Err(io::Error::other("usage_record_too_large")); }
    bytes.push(b'\n');
    let parent = path.parent().ok_or_else(|| io::Error::other("usage_parent_missing"))?;
    if !parent.exists() {
        let mut builder = fs::DirBuilder::new(); builder.recursive(true);
        #[cfg(unix)] { use std::os::unix::fs::DirBuilderExt; builder.mode(0o700); }
        builder.create(parent)?;
    }
    let lock = private_file(&path.with_extension("lock"), false)?;
    #[cfg(unix)]
    rustix::fs::flock(&lock, rustix::fs::FlockOperation::LockExclusive).map_err(io::Error::from)?;
    let file = private_file(path, true)?;
    if file.metadata()?.len() + bytes.len() as u64 > FILE_LIMIT {
        drop(file);
        fs::rename(path, path.with_extension("previous.jsonl"))?;
    }
    let mut file = private_file(path, true)?;
    file.write_all(&bytes)?;
    file.sync_data()?;
    drop(lock);
    Ok(())
}

/// Returns the newest bounded window, plus explicit token reporting coverage.
pub fn report(store: &Path, limit: usize) -> io::Result<Value> {
    if !store.is_absolute() || !(1..=1000).contains(&limit) { return Err(io::Error::other("usage_invalid_request")); }
    let path = path_for_store(store);
    let mut records = std::collections::VecDeque::with_capacity(limit);
    let mut incomplete = 0;
    for file in [path.with_extension("previous.jsonl"), path] {
        let metadata = match fs::symlink_metadata(&file) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
            Err(error) => return Err(error),
        };
        if !metadata.is_file() || metadata.len() > FILE_LIMIT { return Err(io::Error::other("usage_invalid_file")); }
        let mut reader = BufReader::new(fs::File::open(file)?);
        let mut line = Vec::new();
        loop {
            line.clear();
            let n = std::io::Read::take(&mut reader, RECORD_LIMIT as u64 + 1).read_until(b'\n', &mut line)?;
            if n == 0 { break; }
            if n > RECORD_LIMIT { return Err(io::Error::other("usage_record_too_large")); }
            if line.last() != Some(&b'\n') { incomplete += 1; break; }
            let record: Value = serde_json::from_slice(&line)?;
            if record.get("schema").and_then(Value::as_u64) != Some(1) { return Err(io::Error::other("usage_invalid_record")); }
            if records.len() == limit { records.pop_front(); }
            records.push_back(record);
        }
    }
    let completed = records.iter().filter(|r| r["outcome"] == "completed").count();
    let failed = records.iter().filter(|r| r["outcome"] == "failed").count();
    let cancelled = records.iter().filter(|r| r["outcome"] == "cancelled").count();
    let sum = |field: &str| records.iter().filter_map(|r| r["usage"][field].as_u64()).fold(0u64, u64::saturating_add);
    let reported = records.iter().filter(|r| r["usage"]["total_tokens"].is_u64()).count();
    let tokens = (reported > 0).then(|| sum("total_tokens"));
    Ok(json!({"version":magpie_core::VERSION,"scope":"recent_requests","limit":limit,
        "incomplete_records":incomplete,"totals":{"requests":records.len(),"completed":completed,"failed":failed,"cancelled":cancelled,
        "reported_requests":reported,"total_tokens":tokens,"input_tokens":sum("input_tokens"),"output_tokens":sum("output_tokens")},
        "records":records.into_iter().rev().collect::<Vec<_>>() }))
}

pub(crate) fn usage_json(usage: &Option<Usage>) -> Value {
    match usage {
        None => Value::Null,
        Some(u) => json!({"input_tokens":u.input_tokens,"output_tokens":u.output_tokens,"total_tokens":u.total_tokens,
            "cached_input_tokens":u.cached_input_tokens,"cache_creation_input_tokens":u.cache_creation_input_tokens,"reasoning_tokens":u.reasoning_tokens}),
    }
}

pub(crate) fn observe(protocol: Protocol, value: &Value, previous: &mut Option<Usage>) {
    let raw = value.get("usage").or_else(|| value.pointer("/response/usage"))
        .or_else(|| value.pointer("/message/usage")).or_else(|| value.get("usageMetadata"));
    let Some(Ok(next)) = raw.map(|raw| Usage::decode(protocol, raw)) else { return; };
    let prev = previous.get_or_insert_with(Usage::default);
    macro_rules! merge { ($($field:ident),*) => { $(if next.$field.is_some() { prev.$field = next.$field; })* }; }
    merge!(input_tokens, output_tokens, cached_input_tokens, cache_creation_input_tokens, reasoning_tokens);
    prev.total_tokens = next.total_tokens.or_else(|| prev.input_tokens.zip(prev.output_tokens).and_then(|(i,o)| i.checked_add(o)));
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn cumulative_stream_usage_is_not_summed_or_invented() {
        let mut usage = None;
        observe(Protocol::AnthropicMessages, &json!({"message":{"usage":{"input_tokens":8,"cache_read_input_tokens":2,"output_tokens":1}}}), &mut usage);
        observe(Protocol::AnthropicMessages, &json!({"usage":{"output_tokens":5}}), &mut usage);
        observe(Protocol::AnthropicMessages, &json!({"usage":{"output_tokens":5}}), &mut usage);
        let usage = usage.unwrap(); assert_eq!(usage.input_tokens, Some(10)); assert_eq!(usage.output_tokens, Some(5)); assert_eq!(usage.total_tokens, Some(15));
        let mut absent = None; observe(Protocol::OpenAiChat, &json!({"choices":[]}), &mut absent); assert!(absent.is_none());
    }
    #[test]
    fn records_survive_restart_and_incomplete_tail_is_reported() {
        let root = std::env::temp_dir().join(format!("magpie-usage-{}-{}",std::process::id(),now_ms()));
        fs::create_dir(&root).unwrap(); let store = root.join("providers.toml"); let path = path_for_store(&store);
        append(&path, &json!({"schema":1,"outcome":"completed","usage":{"total_tokens":10}})).unwrap();
        append(&path, &json!({"schema":1,"outcome":"failed","usage":null})).unwrap();
        private_file(&path,true).unwrap().write_all(b"{partial").unwrap();
        let r = report(&store,200).unwrap(); assert_eq!(r["totals"]["requests"],2); assert_eq!(r["totals"]["total_tokens"],10); assert_eq!(r["totals"]["reported_requests"],1); assert_eq!(r["incomplete_records"],1);
        assert_eq!(report(&store,1).unwrap()["totals"]["total_tokens"],Value::Null);
        fs::remove_dir_all(root).unwrap();
    }
}
