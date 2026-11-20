//! Configuration adapters are exercised through the generic CLI and a real loopback Gateway.
//! No upstream Agent executable or installed credentials are used.
#![cfg(any(target_os = "linux", target_os = "android", target_os = "macos"))]
use magpie_core::provider_store::{Provider, ProviderProtocol, ProviderStore};
use serde_json::{Value, json};
use std::{
    fs,
    io::{BufRead, BufReader, Read, Write},
    net::{SocketAddr, TcpListener, TcpStream},
    os::unix::fs::{DirBuilderExt, PermissionsExt},
    path::PathBuf,
    process::{Child, Command, Output, Stdio},
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
        let root = std::env::temp_dir().canonicalize().unwrap().join(format!(
            "magpie-toml-agent-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::DirBuilder::new().mode(0o700).create(&root).unwrap();
        Self(root)
    }
    fn command(&self) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_magpie"));
        command
            .env_clear()
            .env("MAGPIE_CONFIG_DIR", &self.0)
            .env("HOME", &self.0);
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
    fn call(&self, owner: &str, operation: &str, args: &[&str]) -> Value {
        let mut all = vec!["plugins", "call", owner, operation];
        all.extend(args);
        all.push("--json");
        self.json(&all)["result"].clone()
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
fn start(f: &Fixture) -> (ChildGuard, SocketAddr) {
    let mut child = ChildGuard(
        f.command()
            .args(["serve", "--supervised", "--json", "--listen", "127.0.0.1:0"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap(),
    );
    let stdout = child.0.stdout.take().unwrap();
    let (tx, rx) = mpsc::sync_channel(1);
    thread::spawn(move || {
        let mut line = String::new();
        BufReader::new(stdout).read_line(&mut line).unwrap();
        tx.send(line).unwrap();
    });
    let ready: Value =
        serde_json::from_str(&rx.recv_timeout(Duration::from_secs(5)).unwrap()).unwrap();
    (child, ready["address"].as_str().unwrap().parse().unwrap())
}
fn forbidden(output: Output, code: &str) {
    assert!(!output.status.success());
    let error: Value = serde_json::from_slice(&output.stderr).unwrap();
    assert_eq!(error["error"]["code"], code);
}
fn upstream(listener: TcpListener, expected: Vec<&'static str>) -> thread::JoinHandle<()> {
    thread::spawn(move || {
        listener.set_nonblocking(true).unwrap();
        for model in expected {
            let deadline = Instant::now() + Duration::from_secs(8);
            let mut socket = loop {
                match listener.accept() {
                    Ok((s, _)) => break s,
                    Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                        assert!(
                            Instant::now() < deadline,
                            "request for {model} did not arrive"
                        );
                        thread::sleep(Duration::from_millis(2));
                    }
                    Err(e) => panic!("loopback accept: {e}"),
                }
            };
            socket
                .set_read_timeout(Some(Duration::from_secs(3)))
                .unwrap();
            let mut reader = BufReader::new(socket.try_clone().unwrap());
            let mut first = String::new();
            reader.read_line(&mut first).unwrap();
            assert!(first.starts_with("POST /v1/chat/completions "), "{first}");
            let mut length = 0;
            loop {
                let mut line = String::new();
                assert_ne!(reader.read_line(&mut line).unwrap(), 0);
                if line == "\r\n" {
                    break;
                }
                if let Some(n) = line.to_ascii_lowercase().strip_prefix("content-length:") {
                    length = n.trim().parse::<usize>().unwrap();
                }
            }
            assert!((1..65536).contains(&length));
            let mut bytes = vec![0; length];
            reader.read_exact(&mut bytes).unwrap();
            let body: Value = serde_json::from_slice(&bytes).unwrap();
            assert_eq!(
                body["model"], model,
                "public model maps to exact upstream selection"
            );
            let body = json!({"id":"synthetic", "object":"chat.completion", "model":model, "choices":[{"index":0,"message":{"role":"assistant","content":format!("fixture-{model}")},"finish_reason":"stop"}]}).to_string();
            write!(socket, "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).unwrap();
        }
    })
}
fn request_from_config(model: &str, base: &str, key: &str, expected: &str) {
    assert_eq!(key, "magpie-local");
    let authority = base
        .strip_prefix("http://")
        .unwrap()
        .strip_suffix("/v1")
        .unwrap();
    let address: SocketAddr = authority.parse().unwrap();
    assert!(address.ip().is_loopback());
    let mut socket = TcpStream::connect_timeout(&address, Duration::from_secs(3)).unwrap();
    socket
        .set_read_timeout(Some(Duration::from_secs(3)))
        .unwrap();
    let body = json!({"model":model,"messages":[{"role":"user","content":"synthetic fixture"}]})
        .to_string();
    write!(socket, "POST /v1/chat/completions HTTP/1.0\r\nHost: {authority}\r\nAuthorization: Bearer {key}\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}", body.len()).unwrap();
    let mut response = String::new();
    socket
        .take(1024 * 1024)
        .read_to_string(&mut response)
        .unwrap();
    assert!(response.contains("200 OK"), "{response}");
    let body: Value = serde_json::from_str(response.split_once("\r\n\r\n").unwrap().1).unwrap();
    assert_eq!(
        body["choices"][0]["message"]["content"],
        format!("fixture-{expected}")
    );
}

#[test]
fn explicitly_enabled_versioned_toml_plugins_route_both_models_and_restore_original_config() {
    for (owner, name, original, expected) in [
        (
            "agent.kimi-cli",
            "Kimi CLI 1.52 (Python)",
            "# original user comment\ndefault_model = 'old' # default comment\nunknown = { nested = [1, 2, 3] }\n[models.old]\nprovider='original'\nmodel='old'\nmax_context_size=32768\n[providers.original]\ntype='openai_legacy'\nbase_url='https://example.invalid/v1'\napi_key='SYNTHETIC_PRIVATE'\n",
            vec!["a", "b"],
        ),
        (
            "agent.openmanus",
            "OpenManus source 3309bf4",
            "# original user comment\nunknown = { nested = [1, 2, 3] }\n[llm]\nmodel='old' # selected comment\nbase_url='https://example.invalid/v1'\napi_key='SYNTHETIC_PRIVATE'\nmax_tokens=4096\ntemperature=0.2\n[llm.vision]\nmodel='old-vision'\nbase_url='https://example.invalid/vision'\napi_key='SYNTHETIC_VISION_PRIVATE'\n",
            vec!["a", "a", "b", "b"],
        ),
    ] {
        let f = Fixture::new();
        let path = f.0.join("synthetic.toml");
        fs::write(&path, original).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
        let config = path.to_str().unwrap();
        let list = f.json(&["plugins", "list", "--json"]);
        let plugin = list["plugins"]
            .as_array()
            .unwrap()
            .iter()
            .find(|p| p["id"] == owner)
            .unwrap();
        assert_eq!(plugin["name"], name);
        forbidden(
            f.command()
                .args([
                    "plugins",
                    "call",
                    owner,
                    "selection",
                    "--config",
                    config,
                    "--json",
                ])
                .output()
                .unwrap(),
            "plugin_not_active",
        );
        assert_eq!(fs::read(&path).unwrap(), original.as_bytes());
        f.json(&["plugins", "enable", owner, "--json"]);
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        ProviderStore::create(
            &f.0.join("providers.toml"),
            Provider {
                id: "fixture".into(),
                name: "Synthetic".into(),
                base_url: format!("http://{}/v1", listener.local_addr().unwrap()),
                protocol: ProviderProtocol::OpenAiChat,
                api_key_env: None,
                credential_ref: None,
                models: vec!["a".into(), "b".into()],
                enabled: true,
            },
            0,
        )
        .unwrap();
        let server = upstream(listener, expected);
        let (mut gateway, address) = start(&f);
        let address = address.to_string();
        for (index, model) in ["fixture/a", "fixture/b"].into_iter().enumerate() {
            let preview_op = if index == 0 {
                "preview"
            } else {
                "model-preview"
            };
            let mut preview_args = vec!["--config", config, "--model", model];
            if index == 0 {
                preview_args.extend(["--listen", &address]);
            }
            let preview = f.call(owner, preview_op, &preview_args);
            assert!(!preview.to_string().contains("SYNTHETIC_PRIVATE"));
            let revision = preview["revision"].as_str().unwrap();
            let mut apply = preview_args.clone();
            apply.extend(["--if-revision", revision]);
            f.call(owner, if index == 0 { "connect" } else { "model" }, &apply);
            assert_eq!(
                f.call(owner, "selection", &["--config", config])["selection"]["model"],
                model
            );
            let text = fs::read_to_string(&path).unwrap();
            assert!(text.contains("# original user comment"));
            assert!(text.contains("unknown = { nested = [1, 2, 3] }"));
            let doc: toml_edit::DocumentMut = text.parse().unwrap();
            if owner == "agent.kimi-cli" {
                let alias = doc["default_model"].as_str().unwrap();
                let selected = &doc["models"][alias];
                let provider = &doc["providers"][selected["provider"].as_str().unwrap()];
                assert_eq!(provider["type"].as_str(), Some("openai_legacy"));
                request_from_config(
                    selected["model"].as_str().unwrap(),
                    provider["base_url"].as_str().unwrap(),
                    provider["api_key"].as_str().unwrap(),
                    if index == 0 { "a" } else { "b" },
                );
            } else {
                for selected in [&doc["llm"], &doc["llm"]["vision"]] {
                    assert_eq!(selected["api_type"].as_str(), Some(""));
                    request_from_config(
                        selected["model"].as_str().unwrap(),
                        selected["base_url"].as_str().unwrap(),
                        selected["api_key"].as_str().unwrap(),
                        if index == 0 { "a" } else { "b" },
                    );
                }
            }
        }
        forbidden(
            f.command()
                .args(["plugins", "disable", owner, "--json"])
                .output()
                .unwrap(),
            "plugin_in_use",
        );
        f.call(owner, "disconnect", &["--config", config]);
        assert_eq!(fs::read(&path).unwrap(), original.as_bytes());
        f.json(&["plugins", "disable", owner, "--json"]);
        assert_eq!(fs::read(&path).unwrap(), original.as_bytes());
        drop(gateway.0.stdin.take());
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            if let Some(status) = gateway.0.try_wait().unwrap() {
                assert!(status.success());
                break;
            }
            assert!(Instant::now() < deadline, "supervised Gateway shutdown");
            thread::sleep(Duration::from_millis(5));
        }
        server.join().unwrap();
    }
}

#[test]
fn reserved_kimi_blocks_are_never_adopted_and_openmanus_requires_an_explicit_checkout_file() {
    let f = Fixture::new();
    f.json(&["plugins", "enable", "agent.kimi-cli", "--json"]);
    for original in [
        "# reserved user provider\n[providers.magpie]\napi_key = 'SYNTHETIC_PRIVATE'\n",
        "# reserved user model\n[models.magpie]\nmodel = 'SYNTHETIC_PRIVATE'\n",
    ] {
        let path = f.0.join("reserved.toml");
        fs::write(&path, original).unwrap();
        let before: Vec<_> = fs::read_dir(&f.0)
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .collect();
        let output = f
            .command()
            .args([
                "plugins",
                "call",
                "agent.kimi-cli",
                "preview",
                "--config",
                path.to_str().unwrap(),
                "--model",
                "fixture/a",
                "--listen",
                "127.0.0.1:12345",
                "--json",
            ])
            .output()
            .unwrap();
        assert!(!String::from_utf8_lossy(&output.stderr).contains("SYNTHETIC_PRIVATE"));
        forbidden(output, "config_conflict");
        assert_eq!(fs::read(&path).unwrap(), original.as_bytes());
        let after: Vec<_> = fs::read_dir(&f.0)
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .collect();
        assert_eq!(before, after, "preview creates no recovery/config files");
    }
    f.json(&["plugins", "enable", "agent.openmanus", "--json"]);
    let before = fs::read_dir(&f.0).unwrap().count();
    let output = f
        .command()
        .args(["plugins", "call", "agent.openmanus", "selection", "--json"])
        .output()
        .unwrap();
    assert!(
        !output.status.success(),
        "no guessed global OpenManus config path"
    );
    assert_eq!(fs::read_dir(&f.0).unwrap().count(), before);
}
