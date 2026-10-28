use std::fs::{self, File};
use std::io::{self, BufRead, BufReader, Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::thread;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use magpie_core::catalog::Catalog;
use magpie_service::{GatewayLimits, GatewayServer};

const CHILD_MODE: &str = "MAGPIE_CATALOG_HTTP_TEST_MODE";
const CHILD_CATALOG: &str = "MAGPIE_CATALOG_HTTP_TEST_FILE";
const ADDRESS_MARKER: &str = "MAGPIE_CATALOG_HTTP_ADDRESS=";
const EMPTY_MODELS_JSON: &str = "{\"object\":\"list\",\"data\":[]}";
static NEXT_FILE: AtomicUsize = AtomicUsize::new(0);

struct TempCatalog(PathBuf);

impl TempCatalog {
    fn new(contents: &str) -> Self {
        let sequence = NEXT_FILE.fetch_add(1, Ordering::Relaxed);
        let timestamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("test clock is after the Unix epoch")
            .as_nanos();
        let path = std::env::temp_dir().join(format!(
            "magpie-catalog-http-{}-{timestamp}-{sequence}.toml",
            std::process::id()
        ));
        let mut file = File::create_new(&path).expect("create isolated catalog fixture");
        file.write_all(contents.as_bytes())
            .expect("write catalog fixture");
        Self(path)
    }
}

impl Drop for TempCatalog {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.0);
    }
}

struct ServerProcess {
    child: Child,
}

impl ServerProcess {
    fn start(mode: &str, catalog: Option<&Path>) -> (Self, SocketAddr) {
        let mut command = child_command(mode, catalog);
        command.stdout(Stdio::piped()).stderr(Stdio::inherit());
        let mut server = Self {
            child: command.spawn().expect("spawn isolated HTTP server"),
        };
        let stdout = server.child.stdout.take().expect("capture server address");
        for line in BufReader::new(stdout).lines() {
            let line = line.expect("read server startup");
            if let Some((_, address)) = line.split_once(ADDRESS_MARKER) {
                let address = address.parse().expect("parse bound loopback address");
                return (server, address);
            }
        }
        panic!("server exited before announcing its address");
    }
}

impl Drop for ServerProcess {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn child_command(mode: &str, catalog: Option<&Path>) -> Command {
    let mut command =
        Command::new(std::env::current_exe().expect("locate integration test binary"));
    command
        .args(["--exact", "catalog_server_process", "--nocapture"])
        .env(CHILD_MODE, mode);
    if let Some(path) = catalog {
        command.env(CHILD_CATALOG, path);
    } else {
        command.env_remove(CHILD_CATALOG);
    }
    command
}

// A child test runs the real, indefinite worker loop; its parent owns termination.
#[test]
fn catalog_server_process() -> Result<(), Box<dyn std::error::Error>> {
    let Ok(mode) = std::env::var(CHILD_MODE) else {
        return Ok(());
    };
    let address = "127.0.0.1:0".parse()?;
    let limits = GatewayLimits::default();
    let server = match mode.as_str() {
        "configured" => {
            let path = std::env::var_os(CHILD_CATALOG).expect("child has catalog fixture path");
            let catalog = Catalog::load(Path::new(&path))?;
            GatewayServer::bind_with_catalog(address, limits, catalog)?
        }
        "empty" => GatewayServer::bind_with_limits(address, limits)?,
        _ => return Err("unknown catalog HTTP test mode".into()),
    };
    println!("\n{ADDRESS_MARKER}{}", server.local_addr()?);
    io::stdout().flush()?;
    server.run()?;
    Ok(())
}

struct Response {
    headers: String,
    body: String,
}

fn request(address: SocketAddr, request: &str) -> Response {
    let mut stream = TcpStream::connect_timeout(&address, Duration::from_secs(5))
        .expect("connect to real Gateway");
    stream
        .set_read_timeout(Some(Duration::from_secs(5)))
        .expect("bound HTTP read time");
    stream
        .set_write_timeout(Some(Duration::from_secs(5)))
        .expect("bound HTTP write time");
    stream
        .write_all(request.as_bytes())
        .expect("send HTTP request");
    let mut response = String::new();
    stream
        .read_to_string(&mut response)
        .expect("receive HTTP response");
    let (headers, body) = response
        .split_once("\r\n\r\n")
        .expect("HTTP response has headers and body");
    Response {
        headers: headers.to_owned(),
        body: body.to_owned(),
    }
}

#[test]
fn three_configured_providers_are_served_over_real_http_get_head_and_workers() {
    let fixture = TempCatalog::new(
        r#"[[providers]]
id = "local"
name = "Local \"Lab\" \\ \u672C"
models = ["shared", "namespace/model:latest"]
[[providers]]
id = "remote"
name = "Remote"
models = ["shared"]
[[providers]]
id = "third"
name = "Third"
models = ["c"]
"#,
    );
    let (_server, address) = ServerProcess::start("configured", Some(&fixture.0));
    let expected = concat!(
        r#"{"object":"list","data":[{"id":"local/shared","object":"model","created":0,"owned_by":"local","name":"shared","provider_name":"Local \"Lab\" \\ "#,
        "\u{672c}",
        r#""},{"id":"local/namespace/model:latest","object":"model","created":0,"owned_by":"local","name":"namespace/model:latest","provider_name":"Local \"Lab\" \\ "#,
        "\u{672c}",
        r#""},{"id":"remote/shared","object":"model","created":0,"owned_by":"remote","name":"shared","provider_name":"Remote"},{"id":"third/c","object":"model","created":0,"owned_by":"third","name":"c","provider_name":"Third"}]}"#
    );
    let get = request(
        address,
        "GET /v1/models?client=fixture HTTP/1.1\r\nHost: localhost\r\n\r\n",
    );
    assert!(get.headers.starts_with("HTTP/1.1 200 OK\r\n"));
    assert!(
        get.headers
            .contains("Content-Type: application/json; charset=utf-8\r\n")
    );
    assert!(
        get.headers
            .contains(&format!("Content-Length: {}\r\n", expected.len()))
    );
    assert_eq!(get.body, expected);

    let head = request(
        address,
        "HEAD /v1/models HTTP/1.1\r\nHost: localhost\r\n\r\n",
    );
    assert_eq!(head.headers, get.headers);
    assert!(head.body.is_empty(), "HEAD must not send catalog bytes");

    let workers = (0..8)
        .map(|_| {
            thread::spawn(move || {
                request(
                    address,
                    "GET /v1/models HTTP/1.1\r\nHost: localhost\r\n\r\n",
                )
            })
        })
        .collect::<Vec<_>>();
    for worker in workers {
        let response = worker.join().expect("HTTP client completes");
        assert!(response.headers.starts_with("HTTP/1.1 200 OK\r\n"));
        assert_eq!(response.body, expected);
    }
}

#[test]
fn existing_bind_with_limits_still_serves_an_empty_catalog() {
    let (_server, address) = ServerProcess::start("empty", None);
    let response = request(
        address,
        "GET /v1/models HTTP/1.1\r\nHost: localhost\r\n\r\n",
    );
    assert!(response.headers.starts_with("HTTP/1.1 200 OK\r\n"));
    assert_eq!(response.body, EMPTY_MODELS_JSON);
}

#[test]
fn malformed_catalog_fails_before_startup_without_leaking_toml() {
    let fixture =
        TempCatalog::new("[[providers]]\nid='local'\nname=\"fixture_secret_value\nmodels=['a']");
    let output = child_command("configured", Some(&fixture.0))
        .output()
        .expect("run server with malformed catalog");
    assert!(!output.status.success(), "bad configuration must fail");
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("InvalidToml"),
        "catalog load must explain parsing failure: {stderr}"
    );
    assert!(!stdout.contains(ADDRESS_MARKER));
    assert!(!stdout.contains("fixture_secret_value"));
    assert!(!stderr.contains("fixture_secret_value"));
}

#[test]
fn catalog_does_not_enable_forwarding_or_allow_request_bodies() {
    let fixture = TempCatalog::new("[[providers]]\nid='local'\nname='Local'\nmodels=['a']");
    let (_server, address) = ServerProcess::start("configured", Some(&fixture.0));
    let response = request(
        address,
        "GET /v1/chat/completions HTTP/1.1\r\nHost: localhost\r\n\r\n",
    );
    assert!(
        response
            .headers
            .starts_with("HTTP/1.1 501 Not Implemented\r\n")
    );
    assert!(response.body.contains("\"not_implemented\""));
    for method in ["GET", "HEAD"] {
        let response = request(
            address,
            &format!(
                "{method} /v1/models HTTP/1.1\r\nHost: localhost\r\nContent-Length: 1\r\n\r\nx"
            ),
        );
        assert!(response.headers.starts_with("HTTP/1.1 400 Bad Request\r\n"));
        if method == "HEAD" {
            assert!(response.body.is_empty());
        } else {
            assert!(response.body.contains("body_not_allowed"));
        }
    }
}

#[test]
fn configured_catalog_still_rejects_nonloopback_listeners() {
    let error = GatewayServer::bind_with_catalog(
        "0.0.0.0:0".parse().expect("parse public test address"),
        GatewayLimits::default(),
        Catalog::default(),
    )
    .expect_err("public listener requires a separate authorization design");
    assert_eq!(error.kind(), io::ErrorKind::InvalidInput);
    assert!(error.to_string().contains("loopback"));
}
