//! Model refresh against isolated local HTTP fixtures. No real credentials or
//! Provider accounts are read, and no process-wide environment is modified.
#![cfg(any(target_os = "linux", target_os = "macos", target_os = "android"))]

use magpie_core::provider_store::{
    MAX_PROVIDER_MODELS, Provider, ProviderProtocol, ProviderStore, StoreError,
};
use magpie_service::provider_catalog::{CatalogError, refresh_models};
use serde_json::{Value, json};
use std::{
    collections::VecDeque,
    fs,
    io::{Read, Write},
    net::{SocketAddr, TcpListener},
    os::unix::fs::DirBuilderExt,
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU64, Ordering},
        mpsc::{Receiver, channel},
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

static NEXT: AtomicU64 = AtomicU64::new(0);
const PRIVATE: &str = "PRIVATE_UPSTREAM_ERROR_NEVER_PRINT";

struct StoreFixture {
    directory: PathBuf,
    path: PathBuf,
}

impl StoreFixture {
    fn new(protocol: ProviderProtocol, url: String) -> Self {
        let directory = std::env::temp_dir().canonicalize().unwrap().join(format!(
            "magpie-catalog-refresh-{}-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos(),
            NEXT.fetch_add(1, Ordering::Relaxed),
        ));
        fs::DirBuilder::new()
            .mode(0o700)
            .create(&directory)
            .unwrap();
        let path = directory.join("providers.toml");
        ProviderStore::create(
            &path,
            Provider {
                id: "fixture".into(),
                name: "Synthetic catalog fixture".into(),
                base_url: url,
                protocol,
                api_key_env: None,
                credential_ref: None,
                models: vec!["existing-model".into()],
                enabled: true,
            },
            0,
        )
        .unwrap();
        Self { directory, path }
    }
}

impl Drop for StoreFixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.directory);
    }
}

struct Reply {
    status: u16,
    body: Vec<u8>,
    extra_headers: String,
    chunked: bool,
}

impl Reply {
    fn json(value: Value) -> Self {
        Self {
            status: 200,
            body: serde_json::to_vec(&value).unwrap(),
            extra_headers: String::new(),
            chunked: false,
        }
    }
}

struct CatalogServer {
    address: SocketAddr,
    requests: Receiver<String>,
    stopped: Arc<AtomicBool>,
    worker: Option<JoinHandle<()>>,
}

impl CatalogServer {
    fn start(replies: Vec<Reply>) -> Self {
        Self::with_handler(replies, |_| {})
    }

    fn with_handler(
        replies: Vec<Reply>,
        mut before_reply: impl FnMut(&str) + Send + 'static,
    ) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let address = listener.local_addr().unwrap();
        let (sender, requests) = channel();
        let stopped = Arc::new(AtomicBool::new(false));
        let stopping = stopped.clone();
        let worker = thread::spawn(move || {
            let mut replies = VecDeque::from(replies);
            let deadline = Instant::now() + Duration::from_secs(20);
            while !stopping.load(Ordering::Acquire) && Instant::now() < deadline {
                let (mut socket, _) = match listener.accept() {
                    Ok(connection) => connection,
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(2));
                        continue;
                    }
                    Err(error) => panic!("fixture accept: {error}"),
                };
                socket
                    .set_read_timeout(Some(Duration::from_secs(2)))
                    .unwrap();
                socket
                    .set_write_timeout(Some(Duration::from_secs(2)))
                    .unwrap();
                let mut request = Vec::new();
                let mut buffer = [0; 1024];
                while !request.windows(4).any(|bytes| bytes == b"\r\n\r\n") {
                    let count = socket.read(&mut buffer).unwrap();
                    if count == 0 || request.len() + count > 32 * 1024 {
                        break;
                    }
                    request.extend_from_slice(&buffer[..count]);
                }
                let request = String::from_utf8(request).unwrap();
                sender.send(request.clone()).unwrap();
                before_reply(&request);
                let reply = replies.pop_front().unwrap_or_else(|| Reply {
                    status: 500,
                    body: Vec::new(),
                    extra_headers: String::new(),
                    chunked: false,
                });
                let framing = if reply.chunked {
                    "Transfer-Encoding: chunked\r\n".to_owned()
                } else {
                    format!("Content-Length: {}\r\n", reply.body.len())
                };
                let header = format!(
                    "HTTP/1.1 {} Fixture\r\nConnection: close\r\nContent-Type: application/json\r\n{}{}\r\n",
                    reply.status, framing, reply.extra_headers
                );
                if socket.write_all(header.as_bytes()).is_err() {
                    continue;
                }
                if reply.chunked {
                    for chunk in reply.body.chunks(16384) {
                        if write!(socket, "{:x}\r\n", chunk.len()).is_err()
                            || socket.write_all(chunk).is_err()
                            || socket.write_all(b"\r\n").is_err()
                        {
                            break;
                        }
                    }
                    let _ = socket.write_all(b"0\r\n\r\n");
                } else {
                    let _ = socket.write_all(&reply.body);
                }
            }
        });
        Self {
            address,
            requests,
            stopped,
            worker: Some(worker),
        }
    }

    fn base(&self, suffix: &str) -> String {
        format!("http://{}{suffix}", self.address)
    }

    fn request(&self) -> String {
        self.requests.recv_timeout(Duration::from_secs(5)).unwrap()
    }
}

impl Drop for CatalogServer {
    fn drop(&mut self) {
        self.stopped.store(true, Ordering::Release);
        if let Some(worker) = self.worker.take() {
            worker.join().unwrap();
        }
    }
}

#[test]
fn all_four_protocols_request_their_real_catalog_and_only_replace_models() {
    for protocol in ProviderProtocol::ALL {
        let (payload, suffix, expected_path) = match protocol {
            ProviderProtocol::OpenAiChat | ProviderProtocol::OpenAiResponses => (
                json!({"object":"list", "data":[{"id":"new-model"},{"id":"vendor/model@latest"},{"id":"new-model"}]}),
                "/proxy/v1",
                "/proxy/v1/models",
            ),
            ProviderProtocol::AnthropicMessages => (
                json!({"data":[{"id":"new-model"},{"id":"vendor/model@latest"}],"has_more":false,"last_id":"vendor/model@latest"}),
                "/anthropic",
                "/anthropic/v1/models?limit=1000",
            ),
            ProviderProtocol::Gemini => (
                json!({"models":[
                    {"name":"models/new-model","supportedGenerationMethods":["generateContent"]},
                    {"name":"models/embedding","supportedGenerationMethods":["embedContent"]},
                    {"name":"models/vendor/model@latest","supportedGenerationMethods":["generateContent","countTokens"]}
                ]}),
                "/google/v1beta",
                "/google/v1beta/models?pageSize=1000",
            ),
        };
        let server = CatalogServer::start(vec![Reply::json(payload)]);
        let fixture = StoreFixture::new(protocol, server.base(suffix));
        let original = ProviderStore::load(&fixture.path).unwrap();
        let refreshed = refresh_models(&fixture.path, "fixture", original.revision()).unwrap();
        let mut expected = original.provider("fixture").unwrap().clone();
        expected.models = vec!["new-model".into(), "vendor/model@latest".into()];
        assert_eq!(refreshed.provider("fixture"), Some(&expected));
        assert_eq!(refreshed.revision(), original.revision() + 1);
        assert_eq!(ProviderStore::load(&fixture.path).unwrap(), refreshed);
        let request = server.request();
        assert!(
            request.starts_with(&format!("GET {expected_path} HTTP/1.1\r\n")),
            "{request}"
        );
        assert!(
            request
                .to_ascii_lowercase()
                .contains("accept: application/json\r\n")
        );
        if protocol == ProviderProtocol::AnthropicMessages {
            assert!(request.to_ascii_lowercase().contains("anthropic-version:"));
        }
    }
}

#[test]
fn anthropic_and_gemini_pagination_commit_only_the_complete_list() {
    for protocol in [
        ProviderProtocol::AnthropicMessages,
        ProviderProtocol::Gemini,
    ] {
        let replies = if protocol == ProviderProtocol::AnthropicMessages {
            vec![
                Reply::json(json!({"data":[{"id":"first"}],"has_more":true,"last_id":"first"})),
                Reply::json(json!({"data":[{"id":"second"}],"has_more":false,"last_id":"second"})),
            ]
        } else {
            vec![
                Reply::json(
                    json!({"models":[{"name":"models/first"}],"nextPageToken":"next /?token"}),
                ),
                Reply::json(json!({"models":[{"name":"models/second"}]})),
            ]
        };
        let server = CatalogServer::start(replies);
        let fixture = StoreFixture::new(protocol, server.base(""));
        let refreshed = refresh_models(&fixture.path, "fixture", 1).unwrap();
        assert_eq!(
            refreshed.provider("fixture").unwrap().models,
            ["first", "second"]
        );
        server.request();
        let next = server.request();
        if protocol == ProviderProtocol::AnthropicMessages {
            assert!(next.starts_with("GET /v1/models?limit=1000&after_id=first HTTP/1.1"));
        } else {
            assert!(next.starts_with(
                "GET /v1beta/models?pageSize=1000&pageToken=next+%2F%3Ftoken HTTP/1.1"
            ));
        }
    }
}

#[test]
fn http_errors_bad_json_empty_and_unknown_pagination_keep_the_existing_catalog() {
    let cases = [
        (
            Reply {
                status: 401,
                body: PRIVATE.as_bytes().into(),
                extra_headers: String::new(),
                chunked: false,
            },
            CatalogError::HttpStatus(401),
        ),
        (
            Reply {
                status: 200,
                body: PRIVATE.as_bytes().into(),
                extra_headers: String::new(),
                chunked: false,
            },
            CatalogError::InvalidResponse,
        ),
        (Reply::json(json!({"data":[]})), CatalogError::EmptyCatalog),
        (
            Reply::json(json!({"data":[{"id":"new"}],"has_more":true,"last_id":"new"})),
            CatalogError::PaginationUnsupported,
        ),
        (
            Reply::json(json!({"data":[{"id":"new"}],"next":"https://untrusted.example/models"})),
            CatalogError::PaginationUnsupported,
        ),
        (
            Reply::json(json!({"data":[{"id":"valid"},{"id":"../invalid"}]})),
            CatalogError::InvalidResponse,
        ),
    ];
    for (reply, expected) in cases {
        let server = CatalogServer::start(vec![reply]);
        let fixture = StoreFixture::new(ProviderProtocol::OpenAiChat, server.base("/v1"));
        let original = fs::read(&fixture.path).unwrap();
        let error = refresh_models(&fixture.path, "fixture", 1).unwrap_err();
        assert_eq!(error, expected);
        assert!(!format!("{error:?}: {error}").contains(PRIVATE));
        assert_eq!(fs::read(&fixture.path).unwrap(), original);
    }
}

#[test]
fn an_incomplete_or_repeated_page_does_not_save_a_partial_catalog() {
    for second in [
        Reply {
            status: 503,
            body: PRIVATE.as_bytes().into(),
            extra_headers: String::new(),
            chunked: false,
        },
        Reply::json(json!({"models":[{"name":"models/second"}],"nextPageToken":"repeat"})),
    ] {
        let server = CatalogServer::start(vec![
            Reply::json(json!({"models":[{"name":"models/first"}],"nextPageToken":"repeat"})),
            second,
        ]);
        let fixture = StoreFixture::new(ProviderProtocol::Gemini, server.base("/v1beta"));
        let original = fs::read(&fixture.path).unwrap();
        assert!(matches!(
            refresh_models(&fixture.path, "fixture", 1),
            Err(CatalogError::HttpStatus(503) | CatalogError::PaginationUnsupported)
        ));
        assert_eq!(fs::read(&fixture.path).unwrap(), original);
    }
}

#[test]
fn redirects_never_contact_the_redirect_destination() {
    let trap = TcpListener::bind("127.0.0.1:0").unwrap();
    trap.set_nonblocking(true).unwrap();
    let server = CatalogServer::start(vec![Reply {
        status: 302,
        body: Vec::new(),
        extra_headers: format!(
            "Location: http://{}/private?token={}\r\n",
            trap.local_addr().unwrap(),
            PRIVATE
        ),
        chunked: false,
    }]);
    let fixture = StoreFixture::new(ProviderProtocol::OpenAiChat, server.base("/v1"));
    let original = fs::read(&fixture.path).unwrap();
    let error = refresh_models(&fixture.path, "fixture", 1).unwrap_err();
    assert_eq!(error, CatalogError::RedirectRefused);
    assert!(!format!("{error:?}: {error}").contains(PRIVATE));
    assert_eq!(
        trap.accept().unwrap_err().kind(),
        std::io::ErrorKind::WouldBlock
    );
    assert_eq!(fs::read(&fixture.path).unwrap(), original);
}

#[test]
fn declared_and_chunked_oversized_responses_are_bounded_without_saving() {
    for chunked in [false, true] {
        let server = CatalogServer::start(vec![Reply {
            status: 200,
            body: vec![b' '; 1024 * 1024 + 1],
            extra_headers: String::new(),
            chunked,
        }]);
        let fixture = StoreFixture::new(ProviderProtocol::OpenAiChat, server.base("/v1"));
        let original = fs::read(&fixture.path).unwrap();
        assert_eq!(
            refresh_models(&fixture.path, "fixture", 1),
            Err(CatalogError::ResponseTooLarge)
        );
        assert_eq!(fs::read(&fixture.path).unwrap(), original);
    }
}

#[test]
fn large_relay_catalog_is_not_truncated_and_overflow_preserves_prior_models() {
    for count in [MAX_PROVIDER_MODELS, MAX_PROVIDER_MODELS + 1] {
        let data: Vec<_> = (0..count)
            .map(|index| json!({"id":format!("model-{index}")}))
            .collect();
        let server = CatalogServer::start(vec![Reply::json(json!({"data":data}))]);
        let fixture = StoreFixture::new(ProviderProtocol::OpenAiResponses, server.base("/v1"));
        let original = fs::read(&fixture.path).unwrap();
        let result = refresh_models(&fixture.path, "fixture", 1);
        if count == MAX_PROVIDER_MODELS {
            let loaded = result.unwrap();
            assert_eq!(
                loaded.provider("fixture").unwrap().models.len(),
                MAX_PROVIDER_MODELS
            );
            assert_eq!(
                loaded.provider("fixture").unwrap().models.last().unwrap(),
                &format!("model-{}", count - 1)
            );
        } else {
            assert_eq!(result, Err(CatalogError::ModelLimit));
            assert_eq!(fs::read(&fixture.path).unwrap(), original);
        }
    }
}

#[test]
fn concurrent_edits_while_downloading_are_preserved_by_the_revision_check() {
    let fixture = StoreFixture::new(
        ProviderProtocol::OpenAiChat,
        "http://127.0.0.1:18421/v1".into(),
    );
    let changed_path = fixture.path.clone();
    let server = CatalogServer::with_handler(
        vec![Reply::json(json!({"data":[{"id":"new-model"}]}))],
        move |_| {
            let store = ProviderStore::load(&changed_path).unwrap();
            let mut provider = store.provider("fixture").unwrap().clone();
            provider.name = "Edited during refresh".into();
            provider.enabled = false;
            ProviderStore::update(&changed_path, provider, store.revision()).unwrap();
        },
    );
    let store = ProviderStore::load(&fixture.path).unwrap();
    let mut provider = store.provider("fixture").unwrap().clone();
    provider.base_url = server.base("/v1");
    let prepared = ProviderStore::update(&fixture.path, provider, store.revision()).unwrap();
    assert_eq!(
        refresh_models(&fixture.path, "fixture", prepared.revision()),
        Err(CatalogError::Store(StoreError::Conflict))
    );
    let actual = ProviderStore::load(&fixture.path).unwrap();
    assert_eq!(
        actual.provider("fixture").unwrap().name,
        "Edited during refresh"
    );
    assert!(!actual.provider("fixture").unwrap().enabled);
    assert_eq!(
        actual.provider("fixture").unwrap().models,
        ["existing-model"]
    );
}

#[test]
fn stale_revision_or_missing_credential_does_not_send_a_request() {
    let server = CatalogServer::start(vec![Reply::json(json!({"data":[{"id":"new-model"}]}))]);
    let fixture = StoreFixture::new(ProviderProtocol::OpenAiChat, server.base("/v1"));
    assert_eq!(
        refresh_models(&fixture.path, "fixture", 0),
        Err(CatalogError::Store(StoreError::Conflict))
    );
    assert!(server.requests.try_recv().is_err());

    let store = ProviderStore::load(&fixture.path).unwrap();
    let mut provider = store.provider("fixture").unwrap().clone();
    let absent = format!(
        "MAGPIE_REFRESH_TEST_ABSENT_{}_{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    );
    assert!(std::env::var_os(&absent).is_none());
    provider.api_key_env = Some(absent);
    let prepared = ProviderStore::update(&fixture.path, provider, store.revision()).unwrap();
    let original = fs::read(&fixture.path).unwrap();
    assert_eq!(
        refresh_models(&fixture.path, "fixture", prepared.revision()),
        Err(CatalogError::CredentialUnavailable)
    );
    assert!(server.requests.try_recv().is_err());
    assert_eq!(fs::read(&fixture.path).unwrap(), original);
}
