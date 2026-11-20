//! Real isolated CLI children: readiness, identity, ownership and bounded exit.

use serde_json::Value;
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

static NEXT: AtomicU64 = AtomicU64::new(0);

struct Fixture(PathBuf);
impl Fixture {
    fn new() -> Self {
        let path = std::env::temp_dir().canonicalize().unwrap().join(format!(
            "magpie-supervised-{}-{}",
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
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

struct OwnedChild(Child);
impl Drop for OwnedChild {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn start(fixture: &Fixture, address: SocketAddr) -> (OwnedChild, Value) {
    let mut child = OwnedChild(
        fixture
            .command()
            .args([
                "serve",
                "--supervised",
                "--json",
                "--listen",
                &address.to_string(),
            ])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap(),
    );
    let stdout = child.0.stdout.take().unwrap();
    let (sender, receiver) = mpsc::sync_channel(1);
    thread::spawn(move || {
        let mut line = String::new();
        let result = BufReader::new(stdout).take(4097).read_line(&mut line);
        let _ = sender.send((result, line));
    });
    let (result, line) = receiver
        .recv_timeout(Duration::from_secs(5))
        .expect("bounded ready event");
    result.unwrap();
    assert!(line.len() <= 4096);
    let ready: Value = serde_json::from_str(&line).expect("one structured ready event");
    assert_eq!(ready["event"], "gateway_ready");
    assert_eq!(ready["protocol_version"], 1);
    assert_eq!(ready["server_version"], magpie_core::VERSION);
    assert_eq!(ready["config_source"], "provider_store");
    assert!(!ready.to_string().contains(fixture.0.to_str().unwrap()));
    assert!(child.0.try_wait().unwrap().is_none());
    (child, ready)
}

fn health(address: SocketAddr) -> Value {
    let mut stream = TcpStream::connect_timeout(&address, Duration::from_secs(2)).unwrap();
    stream
        .set_read_timeout(Some(Duration::from_secs(2)))
        .unwrap();
    stream
        .set_write_timeout(Some(Duration::from_secs(2)))
        .unwrap();
    write!(stream, "GET /health HTTP/1.0\r\nHost: {address}\r\n\r\n").unwrap();
    let mut response = String::new();
    stream.take(16384).read_to_string(&mut response).unwrap();
    assert!(response.starts_with("HTTP/1.0 200") || response.starts_with("HTTP/1.1 200"));
    serde_json::from_str(response.split_once("\r\n\r\n").unwrap().1).unwrap()
}

fn stop(child: &mut OwnedChild) {
    drop(child.0.stdin.take());
    let deadline = Instant::now() + Duration::from_secs(3);
    loop {
        if let Some(status) = child.0.try_wait().unwrap() {
            assert!(status.success());
            return;
        }
        assert!(Instant::now() < deadline, "owner EOF must stop the child");
        thread::sleep(Duration::from_millis(10));
    }
}

#[test]
fn ready_health_and_inspect_report_the_same_server_and_owner_eof_releases_listener() {
    let fixture = Fixture::new();
    let (mut child, ready) = start(&fixture, "127.0.0.1:0".parse().unwrap());
    let address: SocketAddr = ready["address"].as_str().unwrap().parse().unwrap();
    let status = health(address);
    for field in ["server_version", "address", "instance_id", "config_source"] {
        assert_eq!(ready[field], status[field]);
    }
    let inspection = fixture
        .command()
        .args([
            "gateway",
            "inspect",
            "--listen",
            &address.to_string(),
            "--json",
        ])
        .output()
        .unwrap();
    assert!(
        inspection.status.success(),
        "{}",
        String::from_utf8_lossy(&inspection.stderr)
    );
    let inspection: Value = serde_json::from_slice(&inspection.stdout).unwrap();
    for field in ["server_version", "address", "instance_id", "config_source"] {
        assert_eq!(ready[field], inspection["gateway"][field]);
    }
    assert_eq!(inspection["gateway"]["models"], serde_json::json!([]));
    // A stalled connection must not prevent closing the listening socket.
    let _idle = TcpStream::connect(address).unwrap();
    stop(&mut child);
    let rebound = TcpListener::bind(address).expect("stop released the listener");
    drop(rebound);
    let (mut restarted, next) = start(&fixture, address);
    assert_ne!(next["instance_id"], ready["instance_id"]);
    stop(&mut restarted);
    assert!(
        !fixture.0.join("providers.toml").exists(),
        "lifecycle never writes configuration"
    );
}

#[test]
fn occupied_listener_and_corrupt_store_never_emit_ready_or_stop_another_service() {
    let fixture = Fixture::new();
    let (mut owned, ready) = start(&fixture, "127.0.0.1:0".parse().unwrap());
    let address: SocketAddr = ready["address"].as_str().unwrap().parse().unwrap();
    let rejected = fixture
        .command()
        .args([
            "serve",
            "--supervised",
            "--json",
            "--listen",
            &address.to_string(),
        ])
        .output()
        .unwrap();
    assert!(!rejected.status.success());
    assert!(rejected.stdout.is_empty());
    let error: Value = serde_json::from_slice(&rejected.stderr).unwrap();
    assert_eq!(error["error"]["code"], "gateway_address_in_use");
    assert_eq!(health(address)["instance_id"], ready["instance_id"]);
    stop(&mut owned);

    let store = fixture.0.join("providers.toml");
    fs::write(&store, "INVALID_STORE_PRIVATE_CANARY").unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&store, fs::Permissions::from_mode(0o600)).unwrap();
    }
    let rejected = fixture
        .command()
        .args(["serve", "--supervised", "--json", "--listen", "127.0.0.1:0"])
        .output()
        .unwrap();
    assert!(!rejected.status.success());
    assert!(rejected.stdout.is_empty());
    assert!(!String::from_utf8_lossy(&rejected.stderr).contains("PRIVATE_CANARY"));
    assert_eq!(
        fs::read_to_string(store).unwrap(),
        "INVALID_STORE_PRIVATE_CANARY"
    );
}

#[test]
fn supervision_rejects_legacy_modes_and_incomplete_protocol_flags() {
    let fixture = Fixture::new();
    for arguments in [
        vec!["serve", "--supervised"],
        vec!["serve", "--json"],
        vec!["serve", "--supervised", "--json", "--catalog", "unused"],
        vec!["serve", "--supervised", "--json", "--forward", "unused"],
    ] {
        let output = fixture.command().args(arguments).output().unwrap();
        assert!(!output.status.success());
        assert!(output.stdout.is_empty());
    }
}

#[test]
#[cfg(any(target_os = "linux", target_os = "android", target_os = "macos"))]
fn usage_cli_reads_real_requests_offline_after_supervised_shutdown_and_restart() {
    use magpie_core::provider_store::{Provider, ProviderProtocol, ProviderStore};
    let fixture = Fixture::new();
    let upstream = TcpListener::bind("127.0.0.1:0").unwrap();
    let upstream_address = upstream.local_addr().unwrap();
    let store = fixture.0.join("providers.toml");
    ProviderStore::create(
        &store,
        Provider {
            id: "fixture".into(),
            name: "Synthetic".into(),
            base_url: format!("http://{upstream_address}/v1"),
            protocol: ProviderProtocol::OpenAiChat,
            api_key_env: None,
            credential_ref: None,
            models: vec!["test".into()],
            enabled: true,
        },
        0,
    )
    .unwrap();
    // Two independent lifetimes use the same store and metadata directory.
    let worker = thread::spawn(move || {
        for with_usage in [true, false] {
            let (mut socket, _) = upstream.accept().unwrap();
            socket
                .set_read_timeout(Some(Duration::from_secs(3)))
                .unwrap();
            let mut reader = BufReader::new(socket.try_clone().unwrap());
            let mut length = 0;
            loop {
                let mut line = String::new();
                reader.read_line(&mut line).unwrap();
                if line == "\r\n" {
                    break;
                }
                if let Some(value) = line.to_ascii_lowercase().strip_prefix("content-length:") {
                    length = value.trim().parse().unwrap();
                }
            }
            let mut bytes = vec![0; length];
            reader.read_exact(&mut bytes).unwrap();
            let request: Value = serde_json::from_slice(&bytes).unwrap();
            assert_eq!(request["model"], "test");
            let mut body = serde_json::json!({"id":"fixture","object":"chat.completion","model":"test",
                "choices":[{"index":0,"message":{"role":"assistant","content":"PRIVATE_REPLY"},"finish_reason":"stop"}]});
            if with_usage {
                body["usage"] =
                    serde_json::json!({"prompt_tokens":7,"completion_tokens":3,"total_tokens":10});
            }
            let body = body.to_string();
            write!(socket,"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",body.len()).unwrap();
        }
    });
    let mut ids = std::collections::BTreeSet::new();
    for count in 1..=2 {
        let (mut child, ready) = start(&fixture, "127.0.0.1:0".parse().unwrap());
        let address: SocketAddr = ready["address"].as_str().unwrap().parse().unwrap();
        let mut socket = TcpStream::connect(address).unwrap();
        socket
            .set_read_timeout(Some(Duration::from_secs(3)))
            .unwrap();
        let body =
            r#"{"model":"fixture/test","messages":[{"role":"user","content":"PRIVATE_PROMPT"}]}"#;
        write!(socket,"POST /v1/chat/completions HTTP/1.0\r\nHost: {address}\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}",body.len()).unwrap();
        let mut reply = String::new();
        socket.read_to_string(&mut reply).unwrap();
        assert!(reply.contains("200 OK"), "{reply}");
        stop(&mut child); // No wait for health: shutdown itself must drain writes.
        let output = fixture
            .command()
            .args(["usage", "--json", "--limit", "100"])
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let report: Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(report["version"], "0.2.0");
        assert_eq!(report["scope"], "recent_requests");
        assert_eq!(report["totals"]["requests"], count);
        assert_eq!(report["totals"]["completed"], count);
        assert_eq!(report["totals"]["reported_requests"], 1);
        assert_eq!(report["totals"]["total_tokens"], 10);
        assert_eq!(report["incomplete_records"], 0);
        assert!(ids.insert(report["records"][0]["id"].as_str().unwrap().to_owned()));
        let explicit = fixture
            .command()
            .args(["usage", "--json", "--store", store.to_str().unwrap()])
            .output()
            .unwrap();
        assert_eq!(
            serde_json::from_slice::<Value>(&explicit.stdout).unwrap(),
            report
        );
        for private in [
            "PRIVATE_PROMPT",
            "PRIVATE_REPLY",
            "authorization",
            "messages",
        ] {
            assert!(!String::from_utf8_lossy(&output.stdout).contains(private));
        }
    }
    worker.join().unwrap();
}

#[test]
fn usage_cli_validates_limits_and_missing_store_is_read_only() {
    let fixture = Fixture::new();
    for arguments in [
        vec!["usage", "--json", "--limit", "0"],
        vec!["usage", "--json", "--limit", "1001"],
        vec!["usage", "--json", "--limit", "wrong"],
        vec!["usage", "--json", "--limit"],
        vec!["usage", "--json", "--store", "relative.toml"],
        vec!["usage", "--json", "--json"],
        vec!["usage", "--json", "--limit", "5", "--limit", "6"],
        vec!["usage"],
    ] {
        let output = fixture.command().args(arguments).output().unwrap();
        assert!(!output.status.success());
        assert!(output.stdout.is_empty());
    }
    #[cfg(any(target_os = "linux", target_os = "android", target_os = "macos"))]
    {
        let output = fixture
            .command()
            .args(["usage", "--json"])
            .output()
            .unwrap();
        assert!(output.status.success());
        let report: Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(report["totals"]["requests"], 0);
        assert!(report["totals"]["input_tokens"].is_null());
        assert!(report["totals"]["output_tokens"].is_null());
        assert!(report["totals"]["total_tokens"].is_null());
        assert_eq!(
            fs::read_dir(&fixture.0).unwrap().count(),
            0,
            "read never creates files"
        );
    }
}

#[test]
#[cfg(any(target_os = "linux", target_os = "android", target_os = "macos"))]
fn owner_shutdown_persists_inflight_sse_cancellation_with_partial_usage() {
    use magpie_core::provider_store::{Provider, ProviderProtocol, ProviderStore};
    let fixture = Fixture::new();
    let upstream = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = upstream.local_addr().unwrap();
    ProviderStore::create(
        &fixture.0.join("providers.toml"),
        Provider {
            id: "fixture".into(),
            name: "Synthetic".into(),
            base_url: format!("http://{address}/v1"),
            protocol: ProviderProtocol::OpenAiChat,
            api_key_env: None,
            credential_ref: None,
            models: vec!["test".into()],
            enabled: true,
        },
        0,
    )
    .unwrap();
    let worker = thread::spawn(move || {
        let (mut socket, _) = upstream.accept().unwrap();
        socket
            .set_read_timeout(Some(Duration::from_secs(4)))
            .unwrap();
        let mut reader = BufReader::new(socket.try_clone().unwrap());
        let mut length = 0;
        loop {
            let mut line = String::new();
            reader.read_line(&mut line).unwrap();
            if line == "\r\n" {
                break;
            }
            if let Some(value) = line.to_ascii_lowercase().strip_prefix("content-length:") {
                length = value.trim().parse().unwrap();
            }
        }
        reader.read_exact(&mut vec![0; length]).unwrap();
        let frame = "data: {\"choices\":[{\"index\":0,\"delta\":{\"content\":\"PARTIAL_REPLY\"},\"finish_reason\":null}],\"usage\":{\"prompt_tokens\":7,\"completion_tokens\":1,\"total_tokens\":8}}\n\n";
        write!(socket,"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nTransfer-Encoding: chunked\r\n\r\n{:x}\r\n{frame}\r\n",frame.len()).unwrap();
        let mut byte = [0];
        match socket.read(&mut byte) {
            Ok(0) => {}
            Err(error) if error.kind() == std::io::ErrorKind::ConnectionReset => {}
            result => panic!("gateway must close its upstream: {result:?}"),
        }
    });
    let (mut child, ready) = start(&fixture, "127.0.0.1:0".parse().unwrap());
    let address: SocketAddr = ready["address"].as_str().unwrap().parse().unwrap();
    let mut socket = TcpStream::connect(address).unwrap();
    socket
        .set_read_timeout(Some(Duration::from_secs(3)))
        .unwrap();
    let body = r#"{"model":"fixture/test","stream":true,"messages":[{"role":"user","content":"PRIVATE_PROMPT"}]}"#;
    write!(socket,"POST /v1/chat/completions HTTP/1.0\r\nHost: {address}\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}",body.len()).unwrap();
    let mut reader = BufReader::new(socket);
    let mut reply = String::new();
    while !reply.contains("PARTIAL_REPLY") {
        assert!(
            reader.read_line(&mut reply).unwrap() > 0,
            "delta before shutdown"
        );
    }
    stop(&mut child);
    worker.join().unwrap();
    let output = fixture
        .command()
        .args(["usage", "--json"])
        .output()
        .unwrap();
    assert!(output.status.success());
    let report: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(report["totals"]["requests"], 1);
    assert_eq!(report["totals"]["cancelled"], 1);
    assert_eq!(report["records"][0]["outcome"], "cancelled");
    assert_eq!(report["totals"]["total_tokens"], 8);
    assert!(!String::from_utf8_lossy(&output.stdout).contains("PARTIAL_REPLY"));
}
