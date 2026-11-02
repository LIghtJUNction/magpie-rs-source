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
