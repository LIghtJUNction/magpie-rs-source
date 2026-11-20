//! Real isolated CLI processes and loopback requests; no account files or external endpoints.
use serde_json::{Value, json};
use std::{
    fs,
    io::{BufRead, BufReader, Read, Write},
    net::{SocketAddr, TcpListener, TcpStream},
    path::PathBuf,
    process::{Child, Command, Stdio},
    sync::{
        atomic::{AtomicU64, Ordering},
        mpsc,
    },
    thread,
    time::{Duration, Instant},
};
struct Fixture(PathBuf);
impl Fixture {
    fn new() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let path = std::env::temp_dir().canonicalize().unwrap().join(format!(
            "magpie-plugin-runtime-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&path).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&path, fs::Permissions::from_mode(0o700)).unwrap();
        }
        Self(path)
    }
    fn command(&self) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_magpie"));
        command.env_clear().env("MAGPIE_CONFIG_DIR", &self.0);
        command
    }
    fn json(&self, args: &[&str]) -> Value {
        let output = self.command().args(args).output().unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        serde_json::from_slice(&output.stdout).unwrap()
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.0).unwrap();
    }
}
struct ChildGuard(Child);
impl Drop for ChildGuard {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}
fn start(fixture: &Fixture) -> (ChildGuard, SocketAddr) {
    let mut child = ChildGuard(
        fixture
            .command()
            .args(["serve", "--supervised", "--json", "--listen", "127.0.0.1:0"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap(),
    );
    let out = child.0.stdout.take().unwrap();
    let (tx, rx) = mpsc::sync_channel(1);
    thread::spawn(move || {
        let mut line = String::new();
        BufReader::new(out).read_line(&mut line).unwrap();
        tx.send(line).unwrap();
    });
    let ready: Value =
        serde_json::from_str(&rx.recv_timeout(Duration::from_secs(5)).unwrap()).unwrap();
    let address = ready["address"].as_str().unwrap().parse().unwrap();
    (child, address)
}
fn request(address: SocketAddr, body: Option<&str>) -> String {
    let mut socket = TcpStream::connect_timeout(&address, Duration::from_secs(2)).unwrap();
    socket
        .set_read_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    match body { Some(body) => write!(socket, "POST /v1/chat/completions HTTP/1.0\r\nHost: {address}\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}", body.len()).unwrap(), None => write!(socket, "GET /health HTTP/1.0\r\nHost: {address}\r\n\r\n").unwrap() }
    let mut response = String::new();
    socket.read_to_string(&mut response).unwrap();
    assert!(response.contains("200 OK"), "{response}");
    response
}
fn wait_health(address: SocketAddr, key: &str, expected: u64) -> Value {
    let deadline = Instant::now() + Duration::from_secs(4);
    loop {
        let response = request(address, None);
        let value: Value =
            serde_json::from_str(response.split_once("\r\n\r\n").unwrap().1).unwrap();
        if value["metadata_observer"][key].as_u64() == Some(expected) {
            return value;
        }
        assert!(Instant::now() < deadline, "dispatcher {key}: {value}");
        thread::sleep(Duration::from_millis(5));
    }
}
fn read_upstream(socket: &mut TcpStream) {
    socket
        .set_read_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    let mut reader = BufReader::new(socket.try_clone().unwrap());
    let mut length = 0;
    loop {
        let mut line = String::new();
        reader.read_line(&mut line).unwrap();
        if line == "\r\n" {
            break;
        }
        if let Some(n) = line.to_ascii_lowercase().strip_prefix("content-length:") {
            length = n.trim().parse().unwrap();
        }
    }
    let mut body = vec![0; length];
    reader.read_exact(&mut body).unwrap();
    let body: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(body["model"], "test");
}
#[test]
fn list_and_usage_queries_are_read_only_and_toggles_have_revision_guards() {
    let fixture = Fixture::new();
    let list = fixture.json(&["plugins", "list", "--json"]);
    assert_eq!(list["revision"], 0);
    assert!(list["contributions"]["providers"].as_array().unwrap().len() >= 20);
    assert!(list["contributions"]["agents"].as_array().unwrap().len() >= 2);
    assert_eq!(list["contributions"]["pages"][0]["owner"], "feature.usage");
    let report = fixture.json(&[
        "plugins",
        "call",
        "feature.usage",
        "report",
        "--json",
        "--limit",
        "100",
    ]);
    assert_eq!(report["plugin_id"], "feature.usage");
    assert_eq!(report["operation"], "report");
    assert_eq!(report["result"]["totals"]["requests"], 0);
    assert_eq!(
        fs::read_dir(&fixture.0).unwrap().count(),
        0,
        "list and offline reads create no business logs"
    );
    let disabled = fixture.json(&[
        "plugins",
        "disable",
        "feature.usage",
        "--if-revision",
        "0",
        "--json",
    ]);
    assert_eq!(disabled["revision"], 1);
    assert!(
        disabled["contributions"]["pages"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    let stale = fixture
        .command()
        .args([
            "plugins",
            "enable",
            "feature.usage",
            "--if-revision",
            "0",
            "--json",
        ])
        .output()
        .unwrap();
    assert!(!stale.status.success());
    let error: Value = serde_json::from_slice(&stale.stderr).unwrap();
    assert_eq!(error["error"]["code"], "plugin_conflict");
    assert!(
        !fixture
            .command()
            .args(["usage", "--json"])
            .output()
            .unwrap()
            .status
            .success()
    );
    fixture.json(&[
        "plugins",
        "enable",
        "feature.usage",
        "--if-revision",
        "1",
        "--json",
    ]);
    assert_eq!(fixture.json(&["usage", "--json"])["totals"]["requests"], 0);
}
#[test]
fn running_gateway_respects_cross_process_disable_at_completion_and_reenable_preserves_old_data() {
    use magpie_core::provider_store::{Provider, ProviderProtocol, ProviderStore};
    let fixture = Fixture::new();
    let store = fixture.0.join("providers.toml");
    let upstream = TcpListener::bind("127.0.0.1:0").unwrap();
    ProviderStore::create(
        &store,
        Provider {
            id: "fixture".into(),
            name: "Synthetic".into(),
            base_url: format!("http://{}/v1", upstream.local_addr().unwrap()),
            protocol: ProviderProtocol::OpenAiChat,
            api_key_env: None,
            credential_ref: None,
            models: vec!["test".into()],
            enabled: true,
        },
        0,
    )
    .unwrap();
    let (entered_tx, entered_rx) = mpsc::sync_channel(1);
    let (resume_tx, resume_rx) = mpsc::sync_channel(1);
    let upstream_thread = thread::spawn(move || {
        for index in 0..3 {
            let (mut socket, _) = upstream.accept().unwrap();
            read_upstream(&mut socket);
            if index == 1 {
                entered_tx.send(()).unwrap();
                resume_rx.recv_timeout(Duration::from_secs(5)).unwrap();
            }
            let body = json!({"id":"synthetic","object":"chat.completion","model":"test","choices":[{"index":0,"message":{"role":"assistant","content":"PRIVATE_REPLY"},"finish_reason":"stop"}],"usage":{"prompt_tokens":2,"completion_tokens":1,"total_tokens":3}}).to_string();
            write!(socket,"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",body.len()).unwrap();
        }
    });
    let (mut child, address) = start(&fixture);
    let body =
        r#"{"model":"fixture/test","messages":[{"role":"user","content":"PRIVATE_PROMPT"}]}"#;
    request(address, Some(body));
    wait_health(address, "dispatched", 1);
    let first_deadline = Instant::now() + Duration::from_secs(3);
    loop {
        if magpie_plugin_usage::report(&store, 100).is_ok_and(|v| v["totals"]["requests"] == 1) {
            break;
        }
        assert!(Instant::now() < first_deadline);
        thread::sleep(Duration::from_millis(5));
    }
    let first = magpie_plugin_usage::report(&store, 100).unwrap();
    let original_id = first["records"][0]["id"].clone();
    let in_flight = thread::spawn(move || request(address, Some(body)));
    entered_rx.recv_timeout(Duration::from_secs(3)).unwrap();
    fixture.json(&["plugins", "disable", "feature.usage", "--json"]);
    resume_tx.send(()).unwrap();
    in_flight.join().unwrap();
    let health = wait_health(address, "skipped", 1);
    assert_eq!(health["metadata_observer"]["failed"], 0);
    assert_eq!(
        magpie_plugin_usage::report(&store, 100).unwrap()["totals"]["requests"],
        1,
        "in-flight completion must load the new off state"
    );
    fixture.json(&["plugins", "enable", "feature.usage", "--json"]);
    request(address, Some(body));
    wait_health(address, "dispatched", 2);
    drop(child.0.stdin.take());
    let deadline = Instant::now() + Duration::from_secs(4);
    loop {
        if let Some(status) = child.0.try_wait().unwrap() {
            assert!(status.success());
            break;
        }
        assert!(Instant::now() < deadline);
        thread::sleep(Duration::from_millis(5));
    }
    let report = fixture.json(&["plugins", "call", "feature.usage", "report", "--json"]);
    assert_eq!(report["result"]["totals"]["requests"], 2);
    assert!(
        report["result"]["records"]
            .as_array()
            .unwrap()
            .iter()
            .any(|r| r["id"] == original_id)
    );
    assert!(!report.to_string().contains("PRIVATE_"));
    upstream_thread.join().unwrap();
}
#[test]
fn connected_agent_refuses_disable_and_disconnect_keeps_exact_original_bytes() {
    let fixture = Fixture::new();
    let path = fixture.0.join("synthetic-agent.json");
    let original = b"{\"user_owned\":\"PRIVATE_CONFIG\"}\n";
    fs::write(&path, original).unwrap();
    let config = path.to_str().unwrap();
    let preview = fixture.json(&[
        "plugins",
        "call",
        "agent.claude",
        "preview",
        "--config",
        config,
        "--model",
        "fixture/test",
        "--listen",
        "127.0.0.1:3425",
        "--json",
    ]);
    let revision = preview["result"]["revision"].as_str().unwrap();
    fixture.json(&[
        "plugins",
        "call",
        "agent.claude",
        "connect",
        "--config",
        config,
        "--model",
        "fixture/test",
        "--listen",
        "127.0.0.1:3425",
        "--if-revision",
        revision,
        "--json",
    ]);
    let refused = fixture
        .command()
        .args(["plugins", "disable", "agent.claude", "--json"])
        .output()
        .unwrap();
    assert!(!refused.status.success());
    assert!(!String::from_utf8_lossy(&refused.stderr).contains("PRIVATE_CONFIG"));
    fixture.json(&[
        "plugins",
        "call",
        "agent.claude",
        "disconnect",
        "--config",
        config,
        "--json",
    ]);
    assert_eq!(fs::read(&path).unwrap(), original);
    let disabled = fixture.json(&["plugins", "disable", "agent.claude", "--json"]);
    assert!(
        !disabled["contributions"]["agents"]
            .as_array()
            .unwrap()
            .iter()
            .any(|a| a["owner"] == "agent.claude")
    );
    let result = fixture.json(&[
        "plugins",
        "call",
        "agent.claude",
        "disconnect",
        "--config",
        config,
        "--json",
    ]);
    assert!(!result.to_string().contains("PRIVATE_CONFIG"));
}

#[test]
fn native_adapter_and_candidate_inventory_are_independently_gated() {
    let fixture = Fixture::new();
    let list = fixture.json(&["plugins", "list", "--json"]);
    assert!(
        list["plugins"]
            .as_array()
            .unwrap()
            .iter()
            .any(|p| p["id"] == "agent.codex" && p["status"] == "active")
    );
    fixture.json(&["plugins", "disable", "agent.codex", "--json"]);
    let status = fixture
        .command()
        .args(["agents", "status", "codex", "--json"])
        .output()
        .unwrap();
    assert!(!status.status.success());
    assert_eq!(
        serde_json::from_slice::<Value>(&status.stderr).unwrap()["error"]["code"],
        "plugin_not_active"
    );
    let list = fixture.json(&["plugins", "list", "--json"]);
    assert!(
        list["plugins"]
            .as_array()
            .unwrap()
            .iter()
            .any(|p| p["id"] == "feature.agent-discovery" && p["status"] == "active")
    );
    fixture.json(&["plugins", "disable", "feature.agent-discovery", "--json"]);
    let inventory = fixture
        .command()
        .args(["agents", "--json"])
        .output()
        .unwrap();
    assert!(!inventory.status.success());
    fixture.json(&["plugins", "enable", "agent.codex", "--json"]);
    fixture.json(&["plugins", "enable", "feature.agent-discovery", "--json"]);
}

#[test]
fn retained_definition_restores_a_removed_user_adapter_without_reading_its_new_definition() {
    let fixture = Fixture::new();
    let package = fixture.0.join("plugins/synthetic");
    fs::create_dir_all(&package).unwrap();
    let mut definition: Value = serde_json::from_str(include_str!(
        "../../../plugins/agents/claude/definition.json"
    ))
    .unwrap();
    definition["id"] = json!("synthetic");
    definition["name"] = json!("Synthetic adapter");
    fs::write(package.join("definition.json"), definition.to_string()).unwrap();
    fs::write(package.join("plugin.toml"), "format='magpie-plugin/1'\nid='agent.synthetic'\nname='Synthetic'\nversion='1.0.0'\napi_version=1\nkind='agent-adapter'\nenabled_by_default=true\nrequires=[]\nprovides=[]\n[[contributions.agents]]\nid='synthetic'\nengine='declarative-config/1'\ndefinition='definition.json'\n").unwrap();
    let path = fixture.0.join("synthetic-config.json");
    let original = b"{\"unrelated\":\"PRIVATE_ORIGINAL\"}\n";
    fs::write(&path, original).unwrap();
    let config = path.to_str().unwrap();
    let preview = fixture.json(&[
        "plugins",
        "call",
        "agent.synthetic",
        "preview",
        "--config",
        config,
        "--model",
        "fixture/test",
        "--listen",
        "127.0.0.1:3425",
        "--json",
    ]);
    fixture.json(&[
        "plugins",
        "call",
        "agent.synthetic",
        "connect",
        "--config",
        config,
        "--model",
        "fixture/test",
        "--listen",
        "127.0.0.1:3425",
        "--if-revision",
        preview["result"]["revision"].as_str().unwrap(),
        "--json",
    ]);
    let pin = fixture.0.join("plugin-bindings/agent.synthetic.json");
    assert!(
        !fs::read_to_string(&pin)
            .unwrap()
            .contains("PRIVATE_ORIGINAL")
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            fs::metadata(&pin).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }
    fs::remove_dir_all(&package).unwrap();
    fixture.json(&[
        "plugins",
        "call",
        "agent.synthetic",
        "disconnect",
        "--config",
        config,
        "--json",
    ]);
    assert_eq!(fs::read(path).unwrap(), original);
}

#[test]
fn native_codex_uniform_commands_complete_the_revision_bound_lifecycle_and_restore_original_bytes()
{
    let fixture = Fixture::new();
    let path = fixture.0.join("native-codex.toml");
    let original = b"# preserve exactly\nmodel = 'original-external-model'\nmodel_provider = 'original'\nunrelated = 'PRIVATE_FIXTURE_VALUE'\n";
    fs::write(&path, original).unwrap();
    let config = path.to_str().unwrap();
    let selection = fixture.json(&[
        "plugins",
        "call",
        "agent.codex",
        "selection",
        "--config",
        config,
        "--json",
    ]);
    assert_eq!(
        selection["result"]["selection"]["model"],
        "original-external-model"
    );
    assert_eq!(selection["result"]["selection"]["state"], "external");
    assert!(!selection.to_string().contains("PRIVATE_FIXTURE_VALUE"));
    let preview = fixture.json(&[
        "plugins",
        "call",
        "agent.codex",
        "preview",
        "--config",
        config,
        "--model",
        "fixture/first",
        "--listen",
        "127.0.0.1:33425",
        "--json",
    ]);
    let revision = preview["result"]["revision"].as_str().unwrap();
    assert_eq!(revision.len(), 64);
    assert!(revision.bytes().all(|b| b.is_ascii_hexdigit()));
    assert_eq!(preview["result"]["selection_scope"], "proposed");
    assert_eq!(preview["result"]["applied"], false);
    assert!(
        !preview["result"]["changed_fields"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    assert_eq!(fs::read(&path).unwrap(), original);
    assert_eq!(
        fs::read_dir(&fixture.0).unwrap().count(),
        1,
        "preview creates no recovery or host files"
    );
    let changed_intent = fixture
        .command()
        .args([
            "plugins",
            "call",
            "agent.codex",
            "connect",
            "--config",
            config,
            "--model",
            "fixture/other",
            "--listen",
            "127.0.0.1:33425",
            "--if-revision",
            revision,
            "--json",
        ])
        .output()
        .unwrap();
    assert!(!changed_intent.status.success());
    assert_eq!(
        serde_json::from_slice::<Value>(&changed_intent.stderr).unwrap()["error"]["code"],
        "config_conflict"
    );
    assert_eq!(fs::read(&path).unwrap(), original);
    let connected = fixture.json(&[
        "plugins",
        "call",
        "agent.codex",
        "connect",
        "--config",
        config,
        "--model",
        "fixture/first",
        "--listen",
        "127.0.0.1:33425",
        "--if-revision",
        revision,
        "--json",
    ]);
    assert_eq!(connected["result"]["status"], "connected");
    assert_eq!(connected["result"]["revision"], revision);
    let owned = fs::read_to_string(&path).unwrap();
    let document: magpie_core::plugins::DocumentMut = owned.parse().unwrap();
    let endpoint = document["model_providers"]["magpie"]["base_url"]
        .as_str()
        .unwrap()
        .to_owned();
    let refused = fixture
        .command()
        .args(["plugins", "disable", "agent.codex", "--json"])
        .output()
        .unwrap();
    assert!(!refused.status.success());
    assert_eq!(
        serde_json::from_slice::<Value>(&refused.stderr).unwrap()["error"]["code"],
        "plugin_in_use"
    );
    let model_preview = fixture.json(&[
        "plugins",
        "call",
        "agent.codex",
        "model-preview",
        "--config",
        config,
        "--model",
        "fixture/second",
        "--json",
    ]);
    assert_eq!(fs::read_to_string(&path).unwrap(), owned);
    assert_eq!(model_preview["result"]["changed_fields"], json!(["model"]));
    let model_revision = model_preview["result"]["revision"].as_str().unwrap();
    assert_eq!(model_revision.len(), 64);
    let rejected_endpoint = fixture
        .command()
        .args([
            "plugins",
            "call",
            "agent.codex",
            "model",
            "--config",
            config,
            "--model",
            "fixture/second",
            "--listen",
            "127.0.0.1:9",
            "--if-revision",
            model_revision,
            "--json",
        ])
        .output()
        .unwrap();
    assert!(!rejected_endpoint.status.success());
    assert_eq!(fs::read_to_string(&path).unwrap(), owned);
    let updated = fixture.json(&[
        "plugins",
        "call",
        "agent.codex",
        "model",
        "--config",
        config,
        "--model",
        "fixture/second",
        "--if-revision",
        model_revision,
        "--json",
    ]);
    assert_eq!(updated["result"]["selection"]["model"], "fixture/second");
    assert_eq!(updated["result"]["selection_scope"], "applied");
    let document: magpie_core::plugins::DocumentMut =
        fs::read_to_string(&path).unwrap().parse().unwrap();
    assert_eq!(
        document["model_providers"]["magpie"]["base_url"]
            .as_str()
            .unwrap(),
        endpoint
    );
    let disconnected = fixture.json(&[
        "plugins",
        "call",
        "agent.codex",
        "disconnect",
        "--config",
        config,
        "--json",
    ]);
    assert_eq!(disconnected["result"]["status"], "unmanaged");
    assert_eq!(disconnected["result"]["selection_scope"], "not_reported");
    assert_eq!(fs::read(&path).unwrap(), original);
    for value in [
        &preview,
        &connected,
        &model_preview,
        &updated,
        &disconnected,
    ] {
        assert!(!value.to_string().contains("PRIVATE_FIXTURE_VALUE"));
    }
    fixture.json(&["plugins", "disable", "agent.codex", "--json"]);
    fixture.json(&[
        "plugins",
        "call",
        "agent.codex",
        "disconnect",
        "--config",
        config,
        "--json",
    ]);
    let disabled = fixture
        .command()
        .args([
            "plugins",
            "call",
            "agent.codex",
            "selection",
            "--config",
            config,
            "--json",
        ])
        .output()
        .unwrap();
    assert!(!disabled.status.success());
}

#[test]
fn native_codex_command_default_path_is_plugin_owned_and_inferred_path_is_not_emitted() {
    let fixture = Fixture::new();
    let path = fixture.0.join("config.toml");
    let original = b"# synthetic default\nunrelated='PRIVATE_DEFAULT'\n";
    fs::write(&path, original).unwrap();
    let execute = |args: &[&str]| -> Value {
        let output = fixture
            .command()
            .env("CODEX_HOME", &fixture.0)
            .args(args)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        serde_json::from_slice(&output.stdout).unwrap()
    };
    let preview = execute(&[
        "plugins",
        "call",
        "agent.codex",
        "preview",
        "--model",
        "fixture/default",
        "--listen",
        "127.0.0.1:3425",
        "--json",
    ]);
    assert!(preview["result"].get("config_path").is_none());
    assert_eq!(fs::read(&path).unwrap(), original);
    let revision = preview["result"]["revision"].as_str().unwrap();
    execute(&[
        "plugins",
        "call",
        "agent.codex",
        "connect",
        "--model",
        "fixture/default",
        "--listen",
        "127.0.0.1:3425",
        "--if-revision",
        revision,
        "--json",
    ]);
    let selection = execute(&["plugins", "call", "agent.codex", "selection", "--json"]);
    assert_eq!(selection["result"]["selection"]["model"], "fixture/default");
    assert!(selection["result"].get("config_path").is_none());
    execute(&["plugins", "call", "agent.codex", "disconnect", "--json"]);
    assert_eq!(fs::read(path).unwrap(), original);
}

#[test]
fn native_codex_rejects_missing_stale_and_same_bytes_replaced_preview_revisions() {
    let fixture = Fixture::new();
    let path = fixture.0.join("revision.toml");
    let original = b"# PRIVATE_TOKEN\nmodel='old'\n";
    fs::write(&path, original).unwrap();
    let config = path.to_str().unwrap();
    let missing = fixture
        .command()
        .args([
            "plugins",
            "call",
            "agent.codex",
            "connect",
            "--config",
            config,
            "--model",
            "fixture/test",
            "--listen",
            "127.0.0.1:3425",
            "--json",
        ])
        .output()
        .unwrap();
    assert!(!missing.status.success());
    assert_eq!(
        serde_json::from_slice::<Value>(&missing.stderr).unwrap()["error"]["code"],
        "revision_required"
    );
    let preview = fixture.json(&[
        "plugins",
        "call",
        "agent.codex",
        "preview",
        "--config",
        config,
        "--model",
        "fixture/test",
        "--listen",
        "127.0.0.1:3425",
        "--json",
    ]);
    let revision = preview["result"]["revision"].as_str().unwrap();
    let replacement = fixture.0.join("replacement.toml");
    fs::write(&replacement, original).unwrap();
    fs::rename(replacement, &path).unwrap();
    let stale = fixture
        .command()
        .args([
            "plugins",
            "call",
            "agent.codex",
            "connect",
            "--config",
            config,
            "--model",
            "fixture/test",
            "--listen",
            "127.0.0.1:3425",
            "--if-revision",
            revision,
            "--json",
        ])
        .output()
        .unwrap();
    assert!(!stale.status.success());
    assert_eq!(
        serde_json::from_slice::<Value>(&stale.stderr).unwrap()["error"]["code"],
        "config_conflict"
    );
    assert_eq!(fs::read(path).unwrap(), original);
    assert!(!String::from_utf8_lossy(&stale.stderr).contains("PRIVATE_TOKEN"));
}
