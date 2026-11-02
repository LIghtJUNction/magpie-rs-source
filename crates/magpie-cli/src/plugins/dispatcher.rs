//! Bounded application-owned event dispatch with fresh registry state after every dequeue.
use magpie_app_plugins::AppPlugins;
use magpie_core::request_metadata::{MetadataSubscriber, RequestMetadata};
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, BTreeSet},
    io,
    panic::{AssertUnwindSafe, catch_unwind},
    sync::{
        Arc, Mutex,
        atomic::{AtomicU64, Ordering},
        mpsc::{self, SyncSender, TrySendError},
    },
    time::{Duration, Instant},
};

const CAPACITY: usize = 128;
type Loader = Arc<dyn Fn() -> Result<AppPlugins, String> + Send + Sync>;
/// A factory already implemented by the application; never an executable manifest path.
pub struct SubscriberFactory {
    pub owner: String,
    pub create: Arc<dyn Fn() -> Arc<dyn MetadataSubscriber> + Send + Sync>,
}
#[derive(Default)]
struct Counters {
    pending: AtomicU64,
    accepted: AtomicU64,
    dispatched: AtomicU64,
    skipped: AtomicU64,
    dropped: AtomicU64,
    failed: AtomicU64,
    plugins: Mutex<Value>,
}
enum Message {
    Event(Box<RequestMetadata>),
    Flush(SyncSender<bool>),
}
/// The service sees only this generic subscriber, never a concrete Usage dependency.
pub struct MetadataDispatcher {
    sender: Option<SyncSender<Message>>,
    counters: Arc<Counters>,
}
impl MetadataDispatcher {
    pub fn new(loader: Loader, factories: Vec<SubscriberFactory>) -> Self {
        let (sender, receiver) = mpsc::sync_channel(CAPACITY);
        let counters = Arc::new(Counters::default());
        let progress = counters.clone();
        let worker = std::thread::Builder::new()
            .name("magpie-plugin-host".into())
            .spawn(move || {
                let mut host = Host {
                    loader,
                    factories,
                    active: BTreeMap::new(),
                    quarantined: BTreeMap::new(),
                    revision: 0,
                    counters: progress,
                };
                for message in receiver {
                    match message {
                        Message::Event(event) => {
                            // State is loaded after the event leaves the queue, never before recv.
                            host.refresh();
                            let mut handled = false;
                            let owners: Vec<_> = host.active.keys().cloned().collect();
                            for owner in owners {
                                if let Some(plugin) = host.active.get(&owner) {
                                    if catch_unwind(AssertUnwindSafe(|| {
                                        plugin.on_request_finished(&event)
                                    }))
                                    .is_err()
                                    {
                                        host.counters.failed.fetch_add(1, Ordering::Relaxed);
                                        host.quarantined.insert(owner.clone(), host.revision);
                                        host.retire(&owner);
                                    } else {
                                        handled = true;
                                    }
                                }
                            }
                            if handled {
                                host.counters.dispatched.fetch_add(1, Ordering::Relaxed);
                            } else {
                                host.counters.skipped.fetch_add(1, Ordering::Relaxed);
                            }
                            host.counters.pending.fetch_sub(1, Ordering::Release);
                            host.publish_health();
                        }
                        Message::Flush(done) => {
                            host.refresh();
                            let mut success = true;
                            for plugin in host.active.values() {
                                if !matches!(
                                    catch_unwind(AssertUnwindSafe(|| plugin.flush())),
                                    Ok(Ok(()))
                                ) {
                                    success = false;
                                    host.counters.failed.fetch_add(1, Ordering::Relaxed);
                                }
                            }
                            host.publish_health();
                            let _ = done.send(success);
                        }
                    }
                }
                let owners: Vec<_> = host.active.keys().cloned().collect();
                for owner in owners {
                    host.retire(&owner);
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
}
struct Host {
    loader: Loader,
    factories: Vec<SubscriberFactory>,
    active: BTreeMap<String, Arc<dyn MetadataSubscriber>>,
    quarantined: BTreeMap<String, u64>,
    revision: u64,
    counters: Arc<Counters>,
}
impl Host {
    fn refresh(&mut self) {
        let snapshot = catch_unwind(AssertUnwindSafe(|| (self.loader)()));
        let desired: BTreeSet<_> = match snapshot {
            Ok(Ok(app)) => {
                self.revision = app.registry.revision();
                app.metadata_subscribers
                    .iter()
                    .filter(|s| s.event == "request_finished" && app.active(&s.owner))
                    .map(|s| s.owner.clone())
                    .collect()
            }
            _ => {
                self.counters.failed.fetch_add(1, Ordering::Relaxed);
                BTreeSet::new()
            }
        };
        let old: Vec<_> = self
            .active
            .keys()
            .filter(|owner| !desired.contains(*owner))
            .cloned()
            .collect();
        for owner in old {
            self.retire(&owner);
        }
        // A deliberate off state permits a later explicit re-enable to retry a failed owner.
        self.quarantined
            .retain(|owner, revision| desired.contains(owner) && *revision == self.revision);
        for owner in desired {
            if self.active.contains_key(&owner) || self.quarantined.contains_key(&owner) {
                continue;
            }
            let Some(factory) = self.factories.iter().find(|f| f.owner == owner) else {
                self.counters.failed.fetch_add(1, Ordering::Relaxed);
                continue;
            };
            match catch_unwind(AssertUnwindSafe(|| (factory.create)())) {
                Ok(plugin) => {
                    self.active.insert(owner, plugin);
                }
                Err(_) => {
                    self.counters.failed.fetch_add(1, Ordering::Relaxed);
                    self.quarantined.insert(owner, self.revision);
                }
            }
        }
    }
    fn retire(&mut self, owner: &str) {
        if let Some(plugin) = self.active.remove(owner) {
            if !matches!(
                catch_unwind(AssertUnwindSafe(|| plugin.flush())),
                Ok(Ok(()))
            ) {
                self.counters.failed.fetch_add(1, Ordering::Relaxed);
            }
        }
    }
    fn publish_health(&self) {
        let mut payload = serde_json::Map::new();
        for owner in self.quarantined.keys() {
            payload.insert(owner.clone(), json!({"status":"failed"}));
        }
        for (owner, plugin) in &self.active {
            let mut health = match catch_unwind(AssertUnwindSafe(|| plugin.health())) {
                Ok(value) => sanitize_health(value),
                Err(_) => {
                    self.counters.failed.fetch_add(1, Ordering::Relaxed);
                    json!({"failed":true})
                }
            };
            health["status"] = json!("active");
            payload.insert(owner.clone(), health);
        }
        *self
            .counters
            .plugins
            .lock()
            .expect("dispatcher health mutex poisoned") = Value::Object(payload);
    }
}
fn sanitize_health(value: Value) -> Value {
    let mut result = serde_json::Map::new();
    for key in [
        "pending",
        "written",
        "failed",
        "dropped",
        "repaired_tails",
        "queue_capacity",
    ] {
        if let Some(n) = value.get(key).and_then(Value::as_u64) {
            result.insert(key.into(), json!(n));
        }
    }
    Value::Object(result)
}
impl MetadataSubscriber for MetadataDispatcher {
    fn on_request_finished(&self, event: &RequestMetadata) {
        // Bound allocation in the request-completion path even for an invalid caller event.
        if event.schema != 1
            || event.id.len() > 128
            || event.provider_id.len() > 128
            || event.model.len() > 512
            || event.upstream_model.len() > 256
            || event.protocol.len() > 64
            || event.upstream_protocol.len() > 64
        {
            self.counters.dropped.fetch_add(1, Ordering::Relaxed);
            return;
        }
        self.counters.pending.fetch_add(1, Ordering::Relaxed);
        if self.sender.as_ref().is_none_or(|sender| {
            sender
                .try_send(Message::Event(Box::new(event.clone())))
                .is_err()
        }) {
            self.counters.dropped.fetch_add(1, Ordering::Relaxed);
            self.counters.pending.fetch_sub(1, Ordering::Release);
        } else {
            self.counters.accepted.fetch_add(1, Ordering::Relaxed);
        }
    }
    fn health(&self) -> Value {
        json!({"mode":"plugin_dispatcher","queue_capacity":CAPACITY,"pending":self.counters.pending.load(Ordering::Acquire),"accepted":self.counters.accepted.load(Ordering::Relaxed),"dispatched":self.counters.dispatched.load(Ordering::Relaxed),"skipped":self.counters.skipped.load(Ordering::Relaxed),"dropped":self.counters.dropped.load(Ordering::Relaxed),"failed":self.counters.failed.load(Ordering::Relaxed),"plugins":*self.counters.plugins.lock().expect("dispatcher health mutex poisoned")})
    }
    fn flush(&self) -> io::Result<()> {
        let sender = self
            .sender
            .as_ref()
            .ok_or_else(|| io::Error::other("plugin_dispatcher_unavailable"))?;
        let (done, receipt) = mpsc::sync_channel(1);
        let deadline = Instant::now() + Duration::from_secs(8);
        let mut message = Message::Flush(done);
        loop {
            match sender.try_send(message) {
                Ok(()) => break,
                Err(TrySendError::Full(next)) if Instant::now() < deadline => {
                    message = next;
                    std::thread::sleep(Duration::from_millis(2));
                }
                Err(_) => return Err(io::Error::other("plugin_dispatcher_flush_failed")),
            }
        }
        match receipt.recv_timeout(deadline.saturating_duration_since(Instant::now())) {
            Ok(true) => Ok(()),
            _ => Err(io::Error::other("plugin_dispatcher_flush_failed")),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use magpie_core::{plugins::PluginInput, request_metadata::RequestOutcome};
    fn event(id: &str) -> RequestMetadata {
        RequestMetadata {
            schema: 1,
            id: id.into(),
            started_at_ms: 1,
            duration_ms: 1,
            provider_id: "fixture".into(),
            model: "fixture/test".into(),
            upstream_model: "test".into(),
            protocol: "openai_chat".into(),
            upstream_protocol: "openai_chat".into(),
            stream: false,
            outcome: RequestOutcome::Completed,
            usage: None,
        }
    }
    fn snapshot(include_other: bool) -> AppPlugins {
        let mut features = vec![magpie_app_plugins::FeatureRegistration {
            input: PluginInput::bundled(
                "test",
                "format='magpie-plugin/1'\nid='feature.fixture'\nname='Fixture'\nversion='1.0.0'\napi_version=1\nkind='feature'\nenabled_by_default=true\nrequires=[]\nprovides=[]\n[[contributions.metadata_subscribers]]\nid='fixture'\nevent='request_finished'",
            ),
            commands: vec![],
            pages: vec![],
            metadata_subscribers: vec![("fixture".into(), "request_finished".into())],
        }];
        if include_other {
            let mut extra = features[0].clone();
            extra.input.text = extra
                .input
                .text
                .replace("feature.fixture", "feature.other")
                .replace("id='fixture'", "id='other'");
            extra.metadata_subscribers = vec![("other".into(), "request_finished".into())];
            features.push(extra);
        }
        // Paths are selected by the test and never written by snapshot loading.
        let root = std::env::temp_dir()
            .canonicalize()
            .unwrap()
            .join(format!("magpie-dispatch-readonly-{}", std::process::id()));
        AppPlugins::load_at(&root.join("plugins"), &root.join("plugins.toml"), &features).unwrap()
    }
    struct Counter(Arc<AtomicU64>);
    impl MetadataSubscriber for Counter {
        fn on_request_finished(&self, _: &RequestMetadata) {
            self.0.fetch_add(1, Ordering::Relaxed);
        }
        fn health(&self) -> Value {
            json!({"written":self.0.load(Ordering::Relaxed),"raw_secret":"PRIVATE_FIXTURE"})
        }
    }
    #[test]
    fn queue_is_bounded_nonblocking_and_flush_is_an_ordered_barrier() {
        let (entered, received) = mpsc::sync_channel(1);
        let (release, gate) = mpsc::sync_channel(1);
        let gate = Arc::new(Mutex::new(gate));
        let initial = Arc::new(AtomicU64::new(0));
        let loader: Loader = Arc::new(move || {
            if initial.fetch_add(1, Ordering::Relaxed) == 0 {
                entered.send(()).unwrap();
                gate.lock()
                    .unwrap()
                    .recv_timeout(Duration::from_secs(4))
                    .unwrap();
            }
            Ok(snapshot(false))
        });
        let count = Arc::new(AtomicU64::new(0));
        let factory_count = count.clone();
        let dispatcher = MetadataDispatcher::new(
            loader,
            vec![SubscriberFactory {
                owner: "feature.fixture".into(),
                create: Arc::new(move || Arc::new(Counter(factory_count.clone()))),
            }],
        );
        dispatcher.on_request_finished(&event("first"));
        received.recv_timeout(Duration::from_secs(2)).unwrap();
        let started = Instant::now();
        for index in 0..200 {
            dispatcher.on_request_finished(&event(&format!("event_{index}")));
        }
        assert!(started.elapsed() < Duration::from_secs(1));
        assert!(dispatcher.health()["dropped"].as_u64().unwrap() >= 72);
        release.send(()).unwrap();
        dispatcher.flush().unwrap();
        let health = dispatcher.health();
        assert_eq!(health["pending"], 0);
        assert_eq!(
            count.load(Ordering::Relaxed),
            health["accepted"].as_u64().unwrap()
        );
        assert!(!health.to_string().contains("PRIVATE_FIXTURE"));
    }
    struct Panics;
    impl MetadataSubscriber for Panics {
        fn on_request_finished(&self, _: &RequestMetadata) {
            std::panic::panic_any(());
        }
    }
    #[test]
    fn callback_panic_retracts_only_the_failed_owner_and_keeps_host_alive() {
        let count = Arc::new(AtomicU64::new(0));
        let factory_count = count.clone();
        let dispatcher = MetadataDispatcher::new(
            Arc::new(|| Ok(snapshot(true))),
            vec![
                SubscriberFactory {
                    owner: "feature.fixture".into(),
                    create: Arc::new(|| Arc::new(Panics)),
                },
                SubscriberFactory {
                    owner: "feature.other".into(),
                    create: Arc::new(move || Arc::new(Counter(factory_count.clone()))),
                },
            ],
        );
        dispatcher.on_request_finished(&event("first"));
        dispatcher.on_request_finished(&event("second"));
        dispatcher.flush().unwrap();
        let health = dispatcher.health();
        assert_eq!(health["failed"], 1);
        assert_eq!(health["skipped"], 0);
        assert_eq!(count.load(Ordering::Relaxed), 2);
        assert_eq!(health["plugins"]["feature.fixture"]["status"], "failed");
        assert_eq!(health["pending"], 0);
    }
    #[test]
    fn registry_errors_fail_closed_and_invalid_event_allocations_are_rejected() {
        let dispatcher = MetadataDispatcher::new(Arc::new(|| Err("PRIVATE_ERROR".into())), vec![]);
        let mut invalid = event("first");
        invalid.model = "x".repeat(513);
        dispatcher.on_request_finished(&invalid);
        dispatcher.on_request_finished(&event("valid"));
        dispatcher.flush().unwrap();
        let health = dispatcher.health();
        assert_eq!(health["dropped"], 1);
        assert_eq!(health["skipped"], 1);
        assert!(!health.to_string().contains("PRIVATE_ERROR"));
    }
}
