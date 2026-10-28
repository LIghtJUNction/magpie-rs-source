//! Synthetic loopback HTTP acceptance for the real forwarding server.
//! No Cargo invocation, account files, global environment mutation, or external APIs.

use bytes::Bytes;
use futures_util::StreamExt;
use http_body_util::{BodyExt, StreamBody};
use hyper::{Response, body::Frame, server::conn::http1, service::service_fn};
use hyper_util::rt::TokioIo;
use magpie_service::{
    forward::{ForwardConfig, ForwardServer},
    protocol::Protocol,
};
use serde_json::{Value, json};
use std::{
    convert::Infallible,
    io::{self, BufRead, BufReader, Write},
    net::SocketAddr,
    process::{Child, Command, Stdio},
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
        mpsc as blocking_mpsc,
    },
    thread,
    time::Duration,
};
use tokio::{
    sync::{Semaphore, mpsc},
    task::{JoinHandle, JoinSet},
    time::{Instant, timeout},
};

const CHILD_CONFIG: &str = "MAGPIE_FORWARD_HTTP_TEST_CONFIG";
const ADDRESS_MARKER: &str = "MAGPIE_FORWARD_HTTP_ADDRESS=";
const KEY_ENV: &str = "MAGPIE_FORWARD_HTTP_TEST_KEY";
const KEY: &str = "synthetic-provider-key";
const PRIVATE: &str = "synthetic-upstream-private-body";
const TEXT: &str = "\u{4e2d}\u{1f642}";
const PUBLIC_MODEL: &str = "fixture/namespace/model:trial";
const MODEL: &str = "namespace/model:trial";
const PROTOCOLS: [(Protocol, &str); 4] = [
    (Protocol::OpenAiChat, "openai_chat"),
    (Protocol::OpenAiResponses, "openai_responses"),
    (Protocol::AnthropicMessages, "anthropic_messages"),
    (Protocol::Gemini, "gemini"),
];

struct ServerProcess(Child);

impl ServerProcess {
    fn start(config: Value) -> (Self, SocketAddr) {
        let mut child = Self(
            Command::new(std::env::current_exe().expect("test executable"))
                .args(["--exact", "forward_server_process", "--nocapture"])
                .env(CHILD_CONFIG, config.to_string())
                .env(KEY_ENV, KEY)
                .stdout(Stdio::piped())
                .stderr(Stdio::inherit())
                .spawn()
                .expect("spawn real forwarding loop"),
        );
        let stdout = child.0.stdout.take().expect("child stdout");
        let (sender, receiver) = blocking_mpsc::sync_channel(1);
        thread::spawn(move || {
            for line in BufReader::new(stdout).lines() {
                let Ok(line) = line else { break };
                if let Some((_, address)) = line.split_once(ADDRESS_MARKER) {
                    let _ = sender.send(address.parse::<SocketAddr>().expect("bound address"));
                    break;
                }
            }
        });
        let address = receiver
            .recv_timeout(Duration::from_secs(5))
            .expect("bounded gateway startup");
        (child, address)
    }
}

impl Drop for ServerProcess {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

#[test]
fn forward_server_process() -> io::Result<()> {
    let Ok(config) = std::env::var(CHILD_CONFIG) else {
        return Ok(());
    };
    let server = ForwardServer::bind(
        "127.0.0.1:0".parse().expect("literal loopback"),
        ForwardConfig::from_json(config.as_bytes())?,
    )?;
    println!("\n{ADDRESS_MARKER}{}", server.local_addr()?);
    io::stdout().flush()?;
    server.run()
}

#[derive(Clone)]
struct Reply {
    status: u16,
    media: &'static str,
    chunks: Vec<Bytes>,
    gate: Option<Arc<Semaphore>>,
    tail: Bytes,
    broken: bool,
}

impl Reply {
    fn json(body: Value) -> Self {
        Self {
            status: 200,
            media: "application/json",
            chunks: vec![Bytes::from(body.to_string())],
            gate: None,
            tail: Bytes::new(),
            broken: false,
        }
    }

    fn sse(bytes: Vec<u8>) -> Self {
        Self {
            media: "text/event-stream",
            chunks: vec![Bytes::from(bytes)],
            ..Self::json(json!({}))
        }
    }
}

struct Recorded {
    path: String,
    headers: hyper::HeaderMap,
    body: Value,
}

struct Fixture {
    address: SocketAddr,
    received: mpsc::Receiver<Recorded>,
    requests: Arc<AtomicUsize>,
    live_bodies: Arc<AtomicUsize>,
    task: JoinHandle<()>,
}

struct ReplyBody {
    chunks: std::vec::IntoIter<Bytes>,
    gate: Option<Arc<Semaphore>>,
    tail: Bytes,
    broken: bool,
    live: Arc<AtomicUsize>,
}

impl Drop for ReplyBody {
    fn drop(&mut self) {
        self.live.fetch_sub(1, Ordering::SeqCst);
    }
}

impl Fixture {
    async fn start(reply: Reply) -> Self {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind isolated upstream");
        let address = listener.local_addr().expect("upstream address");
        let requests = Arc::new(AtomicUsize::new(0));
        let live_bodies = Arc::new(AtomicUsize::new(0));
        let (sender, received) = mpsc::channel(64);
        let count = requests.clone();
        let live = live_bodies.clone();
        let task = tokio::spawn(async move {
            let mut workers = JoinSet::new();
            loop {
                tokio::select! {
                    accepted = listener.accept() => {
                        let (socket, _) = accepted.expect("fixture accepts");
                        let reply = reply.clone();
                        let sender = sender.clone();
                        let count = count.clone();
                        let live = live.clone();
                        workers.spawn(async move {
                            let service = service_fn(move |request: hyper::Request<hyper::body::Incoming>| {
                                let reply = reply.clone();
                                let sender = sender.clone();
                                let count = count.clone();
                                let live = live.clone();
                                async move {
                                    count.fetch_add(1, Ordering::SeqCst);
                                    let (parts, body) = request.into_parts();
                                    let bytes = body.collect().await.expect("synthetic request body").to_bytes();
                                    sender.send(Recorded {
                                        path: parts.uri.to_string(),
                                        headers: parts.headers,
                                        body: serde_json::from_slice(&bytes).expect("forwarded JSON"),
                                    }).await.expect("fixture request receiver");
                                    live.fetch_add(1, Ordering::SeqCst);
                                    let body = ReplyBody {
                                        chunks: reply.chunks.into_iter(), gate: reply.gate,
                                        tail: reply.tail, broken: reply.broken,
                                        live,
                                    };
                                    let stream = futures_util::stream::unfold(body, |mut body| async move {
                                        if let Some(bytes) = body.chunks.next() {
                                            return Some((Ok::<_, io::Error>(Frame::data(bytes)), body));
                                        }
                                        if let Some(gate) = body.gate.take() {
                                            gate.acquire_owned().await.expect("fixture gate").forget();
                                            let bytes = std::mem::take(&mut body.tail);
                                            if !bytes.is_empty() {
                                                return Some((Ok(Frame::data(bytes)), body));
                                            }
                                        }
                                        if body.broken {
                                            // Let HTTP headers and the first increment flush
                                            // before simulating a mid-body transport error.
                                            tokio::time::sleep(Duration::from_millis(25)).await;
                                            body.broken = false;
                                            return Some((Err(io::Error::new(io::ErrorKind::ConnectionReset, "synthetic transport reset")), body));
                                        }
                                        None
                                    });
                                    let mut response = Response::new(StreamBody::new(stream));
                                    *response.status_mut() = hyper::StatusCode::from_u16(reply.status).expect("fixture status");
                                    response.headers_mut().insert("content-type", hyper::header::HeaderValue::from_static(reply.media));
                                    response.headers_mut().insert("x-fixture-private", hyper::header::HeaderValue::from_static(PRIVATE));
                                    response.headers_mut().insert("set-cookie", hyper::header::HeaderValue::from_static(PRIVATE));
                                    response.headers_mut().insert("location", hyper::header::HeaderValue::from_static("http://127.0.0.1:1/never-follow"));
                                    Ok::<_, Infallible>(response)
                                }
                            });
                            let _ = http1::Builder::new().keep_alive(false).serve_connection(TokioIo::new(socket), service).await;
                        });
                    }
                    Some(result) = workers.join_next(), if !workers.is_empty() => {
                        result.expect("fixture worker");
                    }
                }
            }
        });
        Self {
            address,
            received,
            requests,
            live_bodies,
            task,
        }
    }

    async fn recorded(&mut self) -> Recorded {
        timeout(Duration::from_secs(5), self.received.recv())
            .await
            .expect("bounded upstream receipt")
            .expect("recorded upstream request")
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        // Dropping the accept task also drops its JoinSet and all live fixtures.
        self.task.abort();
    }
}

fn config(address: SocketAddr, protocol: Option<&str>) -> Value {
    let mut config = json!({
        "version":1,"provider_id":"fixture",
        "base_url":format!("http://{address}/v1"),"models":[MODEL],
        "api_key_env":KEY_ENV
    });
    if let Some(protocol) = protocol {
        config["upstream_protocol"] = json!(protocol);
    }
    config
}

fn client() -> reqwest::Client {
    reqwest::Client::builder()
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .retry(reqwest::retry::never())
        .timeout(Duration::from_secs(8))
        .build()
        .expect("synthetic HTTP client")
}

fn entry_path(protocol: Protocol, stream: bool) -> String {
    match protocol {
        Protocol::OpenAiChat => "/v1/chat/completions".into(),
        Protocol::OpenAiResponses => "/v1/responses".into(),
        Protocol::AnthropicMessages => "/v1/messages".into(),
        Protocol::Gemini => format!(
            "/v1beta/models/fixture%2Fnamespace%2Fmodel%3Atrial:{}",
            if stream {
                "streamGenerateContent?alt=sse"
            } else {
                "generateContent"
            }
        ),
    }
}

fn request_body(protocol: Protocol, stream: bool) -> Value {
    let mut body = match protocol {
        Protocol::OpenAiChat => {
            json!({"model":PUBLIC_MODEL,"messages":[{"role":"user","content":TEXT}],"max_completion_tokens":64})
        }
        Protocol::OpenAiResponses => {
            json!({"model":PUBLIC_MODEL,"input":[{"role":"user","content":TEXT}],"max_output_tokens":64})
        }
        Protocol::AnthropicMessages => {
            json!({"model":PUBLIC_MODEL,"messages":[{"role":"user","content":TEXT}],"max_tokens":64})
        }
        Protocol::Gemini => {
            json!({"contents":[{"role":"user","parts":[{"text":TEXT}]}],"generationConfig":{"maxOutputTokens":64}})
        }
    };
    if protocol != Protocol::Gemini {
        body["stream"] = json!(stream);
    }
    body
}

fn message_item() -> Value {
    json!({"id":"item_text","type":"message","role":"assistant","status":"completed","content":[{"type":"output_text","text":TEXT,"annotations":[]}]})
}

fn response_body(protocol: Protocol) -> Value {
    match protocol {
        Protocol::OpenAiChat => {
            json!({"id":"fixture","object":"chat.completion","created":1700000000,"model":MODEL,"choices":[{"index":0,"message":{"role":"assistant","content":TEXT},"finish_reason":"stop"}],"usage":{"prompt_tokens":7,"completion_tokens":3,"total_tokens":10}})
        }
        Protocol::OpenAiResponses => {
            json!({"id":"fixture","object":"response","created_at":1700000000,"model":MODEL,"status":"completed","output":[message_item()],"error":null,"incomplete_details":null,"usage":{"input_tokens":7,"output_tokens":3,"total_tokens":10}})
        }
        Protocol::AnthropicMessages => {
            json!({"id":"fixture","type":"message","role":"assistant","model":MODEL,"content":[{"type":"text","text":TEXT}],"stop_reason":"end_turn","stop_sequence":null,"usage":{"input_tokens":7,"output_tokens":3}})
        }
        Protocol::Gemini => {
            json!({"responseId":"fixture","modelVersion":MODEL,"candidates":[{"index":0,"content":{"role":"model","parts":[{"text":TEXT}]},"finishReason":"STOP"}],"usageMetadata":{"promptTokenCount":7,"candidatesTokenCount":3,"totalTokenCount":10}})
        }
    }
}

fn event(name: &str, value: Value) -> Vec<u8> {
    if name.is_empty() {
        format!("data: {value}\n\n").into_bytes()
    } else {
        format!("event: {name}\ndata: {value}\n\n").into_bytes()
    }
}

fn chat_event(delta: Value, finish: Value) -> Vec<u8> {
    event(
        "",
        json!({"id":"fixture","object":"chat.completion.chunk","created":1700000000,"model":MODEL,"choices":[{"index":0,"delta":delta,"finish_reason":finish}]}),
    )
}

fn stream_body(protocol: Protocol) -> Vec<u8> {
    let events = match protocol {
        Protocol::OpenAiChat => return [
            chat_event(json!({"role":"assistant","content":TEXT}), Value::Null),
            chat_event(json!({}), json!("stop")),
            event("", json!({"choices":[],"usage":{"prompt_tokens":7,"completion_tokens":3,"total_tokens":10}})),
            b"data: [DONE]\n\n".to_vec(),
        ].concat(),
        Protocol::AnthropicMessages => vec![
            ("message_start", json!({"type":"message_start","message":{"id":"fixture","type":"message","role":"assistant","model":MODEL,"content":[],"stop_reason":null,"stop_sequence":null,"usage":{"input_tokens":7,"output_tokens":0}}})),
            ("content_block_start", json!({"type":"content_block_start","index":0,"content_block":{"type":"text","text":""}})),
            ("content_block_delta", json!({"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":TEXT}})),
            ("content_block_stop", json!({"type":"content_block_stop","index":0})),
            ("message_delta", json!({"type":"message_delta","delta":{"stop_reason":"end_turn","stop_sequence":null},"usage":{"output_tokens":3}})),
            ("message_stop", json!({"type":"message_stop"})),
        ],
        Protocol::Gemini => return event("", response_body(protocol)),
        Protocol::OpenAiResponses => {
            let mut shell = response_body(protocol);
            shell["status"] = json!("in_progress");
            shell["output"] = json!([]);
            shell.as_object_mut().expect("response object").remove("usage");
            let events = [
                ("response.created", json!({"response":shell})),
                ("response.output_item.added", json!({"output_index":0,"item":{"id":"item_text","type":"message","role":"assistant","status":"in_progress","content":[]}})),
                ("response.content_part.added", json!({"item_id":"item_text","output_index":0,"content_index":0,"part":{"type":"output_text","text":"","annotations":[]}})),
                ("response.output_text.delta", json!({"item_id":"item_text","output_index":0,"content_index":0,"delta":TEXT})),
                ("response.output_text.done", json!({"item_id":"item_text","output_index":0,"content_index":0,"text":TEXT})),
                ("response.content_part.done", json!({"item_id":"item_text","output_index":0,"content_index":0,"part":{"type":"output_text","text":TEXT,"annotations":[]}})),
                ("response.output_item.done", json!({"output_index":0,"item":message_item()})),
                ("response.completed", json!({"response":response_body(protocol)})),
            ];
            return events.into_iter().enumerate().flat_map(|(sequence, (name, mut value))| {
                value["type"] = json!(name);
                value["sequence_number"] = json!(sequence);
                event(name, value)
            }).collect();
        }
    };
    events
        .into_iter()
        .flat_map(|(name, value)| event(name, value))
        .collect()
}

async fn send(
    client: &reqwest::Client,
    address: SocketAddr,
    protocol: Protocol,
    stream: bool,
    body: Value,
) -> reqwest::Response {
    client
        .post(format!("http://{address}{}", entry_path(protocol, stream)))
        .header("content-type", "application/json")
        .header("authorization", "Bearer synthetic-client-key")
        .header("x-api-key", "synthetic-client-anthropic-key")
        .header("x-goog-api-key", "synthetic-client-gemini-key")
        .body(body.to_string())
        .send()
        .await
        .expect("gateway HTTP reply")
}

async fn health(client: &reqwest::Client, address: SocketAddr) -> Value {
    let bytes = client
        .get(format!("http://{address}/health"))
        .send()
        .await
        .expect("health HTTP")
        .bytes()
        .await
        .expect("health body");
    serde_json::from_slice(&bytes).expect("health JSON")
}

async fn counters_settle(
    client: &reqwest::Client,
    address: SocketAddr,
    completed: u64,
    failed: u64,
    cancelled: u64,
) {
    let deadline = Instant::now() + Duration::from_secs(4);
    loop {
        let value = health(client, address).await;
        let counters = &value["transport_counters"];
        if counters["active"] == 0
            && counters["completed"] == completed
            && counters["failed"] == failed
            && counters["cancelled"] == cancelled
        {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "counters did not settle: {value}"
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

#[test]
fn config_protocol_selection_is_optional_and_explicit() {
    let address = "127.0.0.1:9".parse().expect("fixture address");
    let legacy = config(address, None);
    assert!(ForwardConfig::from_json(legacy.to_string().as_bytes()).is_ok());
    for (_, protocol) in PROTOCOLS {
        let selected = config(address, Some(protocol));
        assert!(ForwardConfig::from_json(selected.to_string().as_bytes()).is_ok());
    }
    for invalid in ["", "chat", "messages", "gemini-native", "unknown"] {
        let selected = config(address, Some(invalid));
        assert!(ForwardConfig::from_json(selected.to_string().as_bytes()).is_err());
    }
}

fn assert_request(recorded: &Recorded, protocol: Protocol, stream: bool) {
    let expected_path = match protocol {
        Protocol::Gemini => format!(
            "/v1beta/models/namespace%2Fmodel%3Atrial:{}",
            if stream {
                "streamGenerateContent?alt=sse"
            } else {
                "generateContent"
            }
        ),
        _ => entry_path(protocol, stream),
    };
    assert_eq!(recorded.path, expected_path, "upstream endpoint");
    assert_eq!(
        recorded.headers["accept"],
        if stream {
            "text/event-stream"
        } else {
            "application/json"
        }
    );
    if matches!(protocol, Protocol::OpenAiChat | Protocol::OpenAiResponses) {
        assert_eq!(recorded.headers["authorization"], format!("Bearer {KEY}"));
        assert!(!recorded.headers.contains_key("x-api-key"));
        assert!(!recorded.headers.contains_key("x-goog-api-key"));
    } else if protocol == Protocol::AnthropicMessages {
        assert_eq!(recorded.headers["x-api-key"], KEY);
        assert_eq!(recorded.headers["anthropic-version"], "2023-06-01");
        assert!(!recorded.headers.contains_key("authorization"));
        assert!(!recorded.headers.contains_key("x-goog-api-key"));
    } else {
        assert_eq!(recorded.headers["x-goog-api-key"], KEY);
        assert!(!recorded.headers.contains_key("authorization"));
        assert!(!recorded.headers.contains_key("x-api-key"));
    }
    if protocol == Protocol::Gemini {
        assert!(recorded.body.get("model").is_none());
        assert!(recorded.body.get("stream").is_none());
        assert_eq!(recorded.body["contents"][0]["parts"][0]["text"], TEXT);
        assert_eq!(recorded.body["generationConfig"]["maxOutputTokens"], 64);
    } else {
        assert_eq!(
            recorded.body["model"], MODEL,
            "only provider prefix is removed"
        );
        assert_eq!(recorded.body["stream"], stream);
        match protocol {
            Protocol::OpenAiChat => {
                assert_eq!(recorded.body["messages"][0]["content"][0]["text"], TEXT);
                assert_eq!(recorded.body["max_completion_tokens"], 64);
                if stream {
                    assert_eq!(
                        recorded.body["stream_options"],
                        json!({"include_usage":true})
                    );
                }
            }
            Protocol::OpenAiResponses => {
                assert_eq!(recorded.body["input"][0]["content"][0]["text"], TEXT);
                assert_eq!(recorded.body["max_output_tokens"], 64);
            }
            Protocol::AnthropicMessages => {
                assert_eq!(recorded.body["messages"][0]["content"][0]["text"], TEXT);
                assert_eq!(recorded.body["max_tokens"], 64);
            }
            Protocol::Gemini => unreachable!(),
        }
    }
}

fn assert_json(body: &Value, protocol: Protocol) {
    let (text, model, usage) = match protocol {
        Protocol::OpenAiChat => {
            assert_eq!(body["choices"][0]["finish_reason"], "stop");
            (
                &body["choices"][0]["message"]["content"],
                &body["model"],
                &body["usage"],
            )
        }
        Protocol::OpenAiResponses => {
            assert_eq!(body["status"], "completed");
            (
                &body["output"][0]["content"][0]["text"],
                &body["model"],
                &body["usage"],
            )
        }
        Protocol::AnthropicMessages => {
            assert_eq!(body["stop_reason"], "end_turn");
            (&body["content"][0]["text"], &body["model"], &body["usage"])
        }
        Protocol::Gemini => {
            assert_eq!(body["candidates"][0]["finishReason"], "STOP");
            (
                &body["candidates"][0]["content"]["parts"][0]["text"],
                &body["modelVersion"],
                &body["usageMetadata"],
            )
        }
    };
    assert_eq!(text, TEXT);
    assert_eq!(model, PUBLIC_MODEL);
    match protocol {
        Protocol::OpenAiChat => assert_eq!(
            usage,
            &json!({"prompt_tokens":7,"completion_tokens":3,"total_tokens":10})
        ),
        Protocol::OpenAiResponses => assert_eq!(
            usage,
            &json!({"input_tokens":7,"output_tokens":3,"total_tokens":10})
        ),
        Protocol::AnthropicMessages => {
            assert_eq!(usage["input_tokens"], 7);
            assert_eq!(usage["output_tokens"], 3);
        }
        Protocol::Gemini => {
            assert_eq!(usage["promptTokenCount"], 7);
            assert_eq!(usage["candidatesTokenCount"], 3);
            assert_eq!(usage["totalTokenCount"], 10);
        }
    }
}

fn sse_values(text: &str) -> Vec<Value> {
    text.lines()
        .filter_map(|line| line.strip_prefix("data: "))
        .filter(|data| *data != "[DONE]")
        .map(|data| serde_json::from_str(data).expect("downstream SSE JSON"))
        .collect()
}

fn assert_sse(text: &str, protocol: Protocol) {
    let values = sse_values(text);
    let (delta_path, terminal_type) = match protocol {
        Protocol::OpenAiChat => ("/choices/0/delta/content", None),
        Protocol::OpenAiResponses => ("/delta", Some("response.completed")),
        Protocol::AnthropicMessages => ("/delta/text", Some("message_stop")),
        Protocol::Gemini => ("/candidates/0/content/parts/0/text", None),
    };
    let text_delta: String = values
        .iter()
        .filter_map(|value| value.pointer(delta_path).and_then(Value::as_str))
        .collect();
    assert_eq!(text_delta, TEXT, "unchanged text increments");
    if let Some(kind) = terminal_type {
        assert_eq!(
            values.iter().filter(|value| value["type"] == kind).count(),
            1
        );
    } else if protocol == Protocol::OpenAiChat {
        assert_eq!(text.matches("data: [DONE]").count(), 1);
    } else {
        assert_eq!(
            values
                .iter()
                .filter(|value| value.pointer("/candidates/0/finishReason") == Some(&json!("STOP")))
                .count(),
            1
        );
    }
    let usage = match protocol {
        Protocol::OpenAiResponses => values
            .iter()
            .find_map(|value| value.pointer("/response/usage")),
        Protocol::Gemini => values.iter().find_map(|value| value.get("usageMetadata")),
        _ => values.iter().rev().find_map(|value| value.get("usage")),
    }
    .expect("final usage");
    match protocol {
        Protocol::OpenAiChat => assert_eq!(usage["total_tokens"], 10),
        Protocol::OpenAiResponses => assert_eq!(usage["total_tokens"], 10),
        Protocol::AnthropicMessages => {
            assert_eq!(usage["input_tokens"], 7);
            assert_eq!(usage["output_tokens"], 3);
        }
        Protocol::Gemini => assert_eq!(usage["totalTokenCount"], 10),
    }
    assert!(!text.contains("protocol_compatibility_error"), "{text}");
}

#[tokio::test(flavor = "current_thread")]
async fn all_twelve_cross_protocol_pairs_forward_json_once_with_model_and_auth() {
    let http = client();
    for (upstream, name) in PROTOCOLS {
        for (entry, _) in PROTOCOLS {
            if upstream == entry {
                continue;
            }
            let mut fixture = Fixture::start(Reply::json(response_body(upstream))).await;
            let (_server, address) = ServerProcess::start(config(fixture.address, Some(name)));
            let response = send(&http, address, entry, false, request_body(entry, false)).await;
            assert_eq!(response.status(), 200, "{entry:?}->{upstream:?}");
            assert_eq!(response.headers()["x-magpie-provider"], "fixture");
            assert_eq!(response.headers()["x-magpie-model"], PUBLIC_MODEL);
            assert!(!response.headers().contains_key("x-fixture-private"));
            let body: Value = serde_json::from_slice(&response.bytes().await.expect("JSON body"))
                .expect("client JSON");
            assert_json(&body, entry);
            let recorded = fixture.recorded().await;
            assert_request(&recorded, upstream, false);
            assert_eq!(
                fixture.requests.load(Ordering::SeqCst),
                1,
                "no protocol probing"
            );
            counters_settle(&http, address, 1, 0, 0).await;
        }
    }
}

#[tokio::test(flavor = "current_thread")]
async fn all_twelve_cross_protocol_pairs_forward_bounded_sse_and_one_terminal() {
    let http = client();
    for (upstream, name) in PROTOCOLS {
        for (entry, _) in PROTOCOLS {
            if upstream == entry {
                continue;
            }
            let raw = stream_body(upstream);
            let mut reply = Reply::sse(raw.clone());
            // Exercise fragmented UTF-8 and a transport frame above the 16 KiB
            // converter slice limit without requiring a large full transcript.
            let mut comment = b":".to_vec();
            comment.extend(std::iter::repeat_n(b'x', 20 * 1024));
            comment.extend_from_slice(b"\r\n\r\n");
            reply.chunks = vec![Bytes::from(comment)];
            reply
                .chunks
                .extend(raw.chunks(7).map(Bytes::copy_from_slice));
            let mut fixture = Fixture::start(reply).await;
            let (_server, address) = ServerProcess::start(config(fixture.address, Some(name)));
            let response = send(&http, address, entry, true, request_body(entry, true)).await;
            assert_eq!(response.status(), 200, "{entry:?}->{upstream:?}");
            assert_eq!(response.headers()["content-type"], "text/event-stream");
            let text = response.text().await.expect("complete converted SSE");
            assert_sse(&text, entry);
            assert_request(&fixture.recorded().await, upstream, true);
            assert_eq!(fixture.requests.load(Ordering::SeqCst), 1);
            counters_settle(&http, address, 1, 0, 0).await;
        }
    }
}

#[tokio::test(flavor = "current_thread")]
async fn omitted_protocol_retains_native_extensions_bytes_and_bearer_auth() {
    let http = client();
    for protocol in [Protocol::OpenAiChat, Protocol::OpenAiResponses] {
        let mut native = response_body(protocol);
        native["private_provider_extension"] = json!({"opaque":[1,2,3]});
        let wire = format!(" \n{native}\n").into_bytes();
        let mut reply = Reply::json(native);
        reply.chunks = vec![Bytes::from(wire.clone())];
        let mut fixture = Fixture::start(reply).await;
        let (_server, address) = ServerProcess::start(config(fixture.address, None));
        let mut body = request_body(protocol, false);
        body["provider_request_extension"] = json!({"opaque":true});
        let response = send(&http, address, protocol, false, body.clone()).await;
        assert_eq!(response.status(), 200);
        assert_eq!(
            response.bytes().await.expect("native bytes").as_ref(),
            &wire
        );
        let recorded = fixture.recorded().await;
        body["model"] = json!(MODEL);
        assert_eq!(recorded.body, body, "native request only replaces model");
        assert_eq!(recorded.headers["authorization"], format!("Bearer {KEY}"));
        assert!(!recorded.headers.contains_key("x-api-key"));
        assert!(!recorded.headers.contains_key("x-goog-api-key"));
        counters_settle(&http, address, 1, 0, 0).await;
    }
}

#[tokio::test(flavor = "current_thread")]
async fn native_sse_keeps_raw_framing_for_all_four_entries() {
    let http = client();
    for (protocol, name) in PROTOCOLS {
        let wire = stream_body(protocol);
        let fixture = Fixture::start(Reply::sse(wire.clone())).await;
        let explicit = if matches!(protocol, Protocol::OpenAiChat | Protocol::OpenAiResponses) {
            None
        } else {
            Some(name)
        };
        let (_server, address) = ServerProcess::start(config(fixture.address, explicit));
        let response = send(&http, address, protocol, true, request_body(protocol, true)).await;
        assert_eq!(response.status(), 200);
        assert_eq!(response.bytes().await.expect("native SSE").as_ref(), &wire);
        counters_settle(&http, address, 1, 0, 0).await;
    }
}

#[tokio::test(flavor = "current_thread")]
async fn unsupported_translation_and_unsafe_requests_never_reach_upstream() {
    let http = client();
    let fixture = Fixture::start(Reply::json(response_body(Protocol::AnthropicMessages))).await;
    let (_server, address) =
        ServerProcess::start(config(fixture.address, Some("anthropic_messages")));
    for field in ["fixture_unknown_field", "previous_response_id"] {
        let mut body = request_body(Protocol::OpenAiChat, false);
        body[field] = json!(PRIVATE);
        let response = send(&http, address, Protocol::OpenAiChat, false, body).await;
        assert_eq!(response.status(), 422);
        let text = response.text().await.expect("safe compatibility error");
        assert!(text.contains("protocol_compatibility_error"));
        assert!(!text.contains(PRIVATE));
        assert!(!text.contains(field));
    }
    for name in ["origin", "forwarded", "x-forwarded-for"] {
        let response = http
            .post(format!("http://{address}/v1/chat/completions"))
            .header("content-type", "application/json")
            .header(name, "null")
            .body(request_body(Protocol::OpenAiChat, false).to_string())
            .send()
            .await
            .expect("security rejection");
        assert_eq!(response.status(), 403);
    }
    for path in [
        "/v1/messages/count_tokens",
        "/v1beta/models/fixture/x:countTokens",
    ] {
        let response = http
            .post(format!("http://{address}{path}"))
            .header("content-type", "application/json")
            .body("{}")
            .send()
            .await
            .expect("unsupported endpoint");
        assert_eq!(response.status(), 501);
    }
    let response = http.post(format!("http://{address}/v1beta/models/fixture%2Fnamespace%2Fmodel%3Atrial:generateContent?key={PRIVATE}"))
        .header("content-type", "application/json").body(request_body(Protocol::Gemini, false).to_string())
        .send().await.expect("query rejection");
    assert_eq!(response.status(), 400);
    assert_eq!(fixture.requests.load(Ordering::SeqCst), 0);
    counters_settle(&http, address, 0, 0, 0).await;
}

#[tokio::test(flavor = "current_thread")]
async fn upstream_http_errors_keep_status_category_and_hide_headers_and_body() {
    let http = client();
    for (status, category) in [
        (401, "authentication_error"),
        (403, "permission_error"),
        (404, "not_found_error"),
        (429, "rate_limit_error"),
        (500, "upstream_server_error"),
        (302, "upstream_server_error"),
    ] {
        let mut reply = Reply::json(json!({"error":{"message":PRIVATE}}));
        reply.status = status;
        let fixture = Fixture::start(reply).await;
        let (_server, address) = ServerProcess::start(config(fixture.address, Some("openai_chat")));
        let response = send(
            &http,
            address,
            Protocol::Gemini,
            false,
            request_body(Protocol::Gemini, false),
        )
        .await;
        assert_eq!(
            response.status().as_u16(),
            if status == 302 { 502 } else { status }
        );
        for header in ["location", "set-cookie", "x-fixture-private"] {
            assert!(!response.headers().contains_key(header));
        }
        let text = response.text().await.expect("safe HTTP error");
        assert!(text.contains(category), "{text}");
        assert!(!text.contains(PRIVATE));
        assert!(!text.contains(KEY));
        assert_eq!(fixture.requests.load(Ordering::SeqCst), 1);
        counters_settle(&http, address, 0, 1, 0).await;
    }
}

#[tokio::test(flavor = "current_thread")]
async fn upstream_incomplete_or_error_sse_yields_safe_error_without_success() {
    let http = client();
    for entry in [
        Protocol::OpenAiResponses,
        Protocol::AnthropicMessages,
        Protocol::Gemini,
    ] {
        for broken in [false, true] {
            let mut bytes = chat_event(json!({"role":"assistant","content":TEXT}), Value::Null);
            if !broken {
                bytes.extend(event(
                    "error",
                    json!({"error":{"message":PRIVATE,"type":"rate_limit_error"}}),
                ));
            }
            let mut reply = Reply::sse(bytes);
            reply.broken = broken;
            let fixture = Fixture::start(reply).await;
            let (_server, address) =
                ServerProcess::start(config(fixture.address, Some("openai_chat")));
            let response = send(&http, address, entry, true, request_body(entry, true)).await;
            assert_eq!(response.status(), 200, "headers already committed");
            let text = response.text().await.expect("safe SSE termination");
            assert!(
                text.contains("error") || text.contains("response.failed"),
                "{text}"
            );
            assert!(!text.contains(PRIVATE));
            assert!(!text.contains(KEY));
            assert!(!text.contains("response.completed"));
            assert!(!text.contains("message_stop"));
            assert!(!text.contains("finishReason"));
            counters_settle(&http, address, 0, 1, 0).await;
        }
    }
    // Clean HTTP EOF without a semantic finish is also a failure.
    let fixture =
        Fixture::start(Reply::sse(chat_event(json!({"content":TEXT}), Value::Null))).await;
    let (_server, address) = ServerProcess::start(config(fixture.address, Some("openai_chat")));
    let response = send(
        &http,
        address,
        Protocol::OpenAiResponses,
        true,
        request_body(Protocol::OpenAiResponses, true),
    )
    .await;
    let text = response.text().await.expect("EOF error SSE");
    assert!(text.contains("protocol_compatibility_error"));
    assert!(!text.contains("response.completed"));
    counters_settle(&http, address, 0, 1, 0).await;
}

#[tokio::test(flavor = "current_thread")]
async fn caller_disconnect_closes_upstream_and_releases_attempt() {
    let http = client();
    let gate = Arc::new(Semaphore::new(0));
    let mut reply = Reply::sse(chat_event(
        json!({"role":"assistant","content":TEXT}),
        Value::Null,
    ));
    reply.gate = Some(gate);
    let mut fixture = Fixture::start(reply).await;
    let (_server, address) = ServerProcess::start(config(fixture.address, Some("openai_chat")));
    let response = send(
        &http,
        address,
        Protocol::OpenAiResponses,
        true,
        request_body(Protocol::OpenAiResponses, true),
    )
    .await;
    let mut body = response.bytes_stream();
    body.next()
        .await
        .expect("first increment")
        .expect("read increment");
    fixture.recorded().await;
    assert_eq!(fixture.live_bodies.load(Ordering::SeqCst), 1);
    drop(body);
    counters_settle(&http, address, 0, 0, 1).await;
    timeout(Duration::from_secs(3), async {
        while fixture.live_bodies.load(Ordering::SeqCst) != 0 {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("upstream body closes after downstream disconnect");
}

#[tokio::test(flavor = "current_thread")]
async fn openai_tool_history_and_response_keep_call_id_and_exact_argument_string() {
    const ARGUMENTS: &str = " { \"n\": 9007199254740993, \"s\": \"\\u4e2d\" } ";
    let http = client();
    let mut reply = response_body(Protocol::OpenAiResponses);
    reply["output"] = json!([{"type":"function_call","id":"item_call","status":"completed","call_id":"call_fixture","name":"weather","arguments":ARGUMENTS}]);
    let mut fixture = Fixture::start(Reply::json(reply)).await;
    let (_server, address) =
        ServerProcess::start(config(fixture.address, Some("openai_responses")));
    let mut body = request_body(Protocol::OpenAiChat, false);
    body["tools"] = json!([{"type":"function","function":{"name":"weather","parameters":{"type":"object","properties":{"n":{"type":"integer"},"s":{"type":"string"}}},"strict":false}}]);
    body["messages"].as_array_mut().expect("request history").extend([
        json!({"role":"assistant","content":null,"tool_calls":[{"id":"call_fixture","type":"function","function":{"name":"weather","arguments":ARGUMENTS}}]}),
        json!({"role":"tool","tool_call_id":"call_fixture","content":"synthetic tool result"}),
    ]);
    let response = send(&http, address, Protocol::OpenAiChat, false, body).await;
    assert_eq!(response.status(), 200);
    let value: Value =
        serde_json::from_slice(&response.bytes().await.expect("tool reply")).expect("JSON");
    assert_eq!(value["choices"][0]["finish_reason"], "tool_calls");
    assert_eq!(
        value["choices"][0]["message"]["tool_calls"][0]["id"],
        "call_fixture"
    );
    assert_eq!(
        value["choices"][0]["message"]["tool_calls"][0]["function"]["arguments"],
        ARGUMENTS
    );
    let recorded = fixture.recorded().await;
    assert_eq!(recorded.body["input"][1]["call_id"], "call_fixture");
    assert_eq!(recorded.body["input"][1]["arguments"], ARGUMENTS);
    assert_eq!(recorded.body["input"][2]["call_id"], "call_fixture");
    assert_eq!(recorded.body["input"][2]["output"], "synthetic tool result");
    assert_eq!(recorded.body["tools"][0]["strict"], false);
    counters_settle(&http, address, 1, 0, 0).await;
}

#[tokio::test(flavor = "current_thread")]
async fn chat_and_messages_tool_sse_preserves_identifiers_and_large_integer_arguments() {
    const ARGUMENTS: &str = " { \"n\": 9007199254740993, \"s\": \"\\u4e2d\" } ";
    let http = client();
    let (first, last) = ARGUMENTS.split_at(20);
    for (upstream, name, entry) in [
        (
            Protocol::OpenAiChat,
            "openai_chat",
            Protocol::AnthropicMessages,
        ),
        (
            Protocol::AnthropicMessages,
            "anthropic_messages",
            Protocol::OpenAiChat,
        ),
    ] {
        let bytes = if upstream == Protocol::OpenAiChat {
            [
                chat_event(json!({"role":"assistant","tool_calls":[{"index":0,"id":"call_fixture","type":"function","function":{"name":"weather","arguments":first}}]}), Value::Null),
                chat_event(json!({"tool_calls":[{"index":0,"function":{"arguments":last}}]}), Value::Null),
                chat_event(json!({}), json!("tool_calls")),
                event("", json!({"choices":[],"usage":{"prompt_tokens":7,"completion_tokens":3,"total_tokens":10}})),
                b"data: [DONE]\n\n".to_vec(),
            ].concat()
        } else {
            [
                event("message_start", json!({"type":"message_start","message":{"id":"fixture","type":"message","role":"assistant","model":MODEL,"content":[],"stop_reason":null,"stop_sequence":null,"usage":{"input_tokens":7,"output_tokens":0}}})),
                event("content_block_start", json!({"type":"content_block_start","index":0,"content_block":{"type":"tool_use","id":"call_fixture","name":"weather","input":{}}})),
                event("content_block_delta", json!({"type":"content_block_delta","index":0,"delta":{"type":"input_json_delta","partial_json":first}})),
                event("content_block_delta", json!({"type":"content_block_delta","index":0,"delta":{"type":"input_json_delta","partial_json":last}})),
                event("content_block_stop", json!({"type":"content_block_stop","index":0})),
                event("message_delta", json!({"type":"message_delta","delta":{"stop_reason":"tool_use","stop_sequence":null},"usage":{"output_tokens":3}})),
                event("message_stop", json!({"type":"message_stop"})),
            ].concat()
        };
        let fixture = Fixture::start(Reply::sse(bytes)).await;
        let (_server, address) = ServerProcess::start(config(fixture.address, Some(name)));
        let response = send(&http, address, entry, true, request_body(entry, true)).await;
        assert_eq!(response.status(), 200);
        let text = response.text().await.expect("tool SSE");
        let values = sse_values(&text);
        let (argument_path, id_path) = if entry == Protocol::OpenAiChat {
            (
                "/choices/0/delta/tool_calls/0/function/arguments",
                "/choices/0/delta/tool_calls/0/id",
            )
        } else {
            ("/delta/partial_json", "/content_block/id")
        };
        let arguments: String = values
            .iter()
            .filter_map(|value| value.pointer(argument_path).and_then(Value::as_str))
            .collect();
        assert_eq!(arguments, ARGUMENTS);
        let parsed: Value = serde_json::from_str(&arguments).expect("valid complete tool object");
        assert_eq!(parsed["n"].to_string(), "9007199254740993");
        assert!(values.iter().any(|value| value.pointer(id_path).and_then(Value::as_str) == Some("call_fixture")));
        assert!(text.contains("weather"));
        assert!(!text.contains("protocol_compatibility_error"));
        counters_settle(&http, address, 1, 0, 0).await;
    }
}

#[tokio::test(flavor = "current_thread")]
async fn request_event_and_reply_limits_fail_without_exposing_payloads() {
    let http = client();
    let fixture = Fixture::start(Reply::json(response_body(Protocol::AnthropicMessages))).await;
    let (_server, address) =
        ServerProcess::start(config(fixture.address, Some("anthropic_messages")));
    let response = http
        .post(format!("http://{address}/v1/chat/completions"))
        .header("content-type", "application/json")
        .body(vec![b'x'; 1024 * 1024 + 1])
        .send()
        .await
        .expect("oversized request rejection");
    assert_eq!(response.status(), 413);
    assert_eq!(fixture.requests.load(Ordering::SeqCst), 0);

    let mut oversized = b"data: ".to_vec();
    oversized.extend(std::iter::repeat_n(b'x', 4 * 1024 * 1024));
    let fixture = Fixture::start(Reply::sse(oversized)).await;
    let (_server, address) = ServerProcess::start(config(fixture.address, Some("openai_chat")));
    let response = send(
        &http,
        address,
        Protocol::OpenAiResponses,
        true,
        request_body(Protocol::OpenAiResponses, true),
    )
    .await;
    let text = response.text().await.expect("safe oversized event error");
    assert!(text.contains("protocol_compatibility_error"));
    assert!(
        text.len() < 4096,
        "oversized upstream bytes must not be forwarded"
    );
    counters_settle(&http, address, 0, 1, 0).await;

    for reply in [
        Reply {
            media: "text/plain",
            ..Reply::json(json!({"echo":PRIVATE}))
        },
        Reply {
            chunks: vec![Bytes::from_static(b"{invalid upstream JSON")],
            ..Reply::json(json!({}))
        },
        Reply::json(json!({"choices":[{"message":{"content":PRIVATE},"finish_reason":"unknown"}]})),
        Reply {
            chunks: vec![Bytes::from(vec![b'x'; 4 * 1024 * 1024 + 1])],
            ..Reply::json(json!({}))
        },
    ] {
        let fixture = Fixture::start(reply).await;
        let (_server, address) = ServerProcess::start(config(fixture.address, Some("openai_chat")));
        let response = send(
            &http,
            address,
            Protocol::AnthropicMessages,
            false,
            request_body(Protocol::AnthropicMessages, false),
        )
        .await;
        assert_eq!(response.status(), 502);
        let text = response.text().await.expect("safe upstream JSON error");
        assert!(text.contains("protocol_compatibility_error"), "{text}");
        assert!(!text.contains(PRIVATE));
        counters_settle(&http, address, 0, 1, 0).await;
    }
}

#[tokio::test(flavor = "current_thread")]
async fn omitted_protocol_rejects_new_entrypoints_without_contacting_upstream() {
    let http = client();
    let fixture = Fixture::start(Reply::json(response_body(Protocol::OpenAiChat))).await;
    let (_server, address) = ServerProcess::start(config(fixture.address, None));
    for protocol in [Protocol::AnthropicMessages, Protocol::Gemini] {
        let response = send(
            &http,
            address,
            protocol,
            false,
            request_body(protocol, false),
        )
        .await;
        assert_eq!(
            response.status(),
            501,
            "legacy config must reject {protocol:?}"
        );
        let body = response.text().await.expect("error response");
        assert!(body.contains("endpoint_not_implemented"));
    }
    assert_eq!(fixture.requests.load(Ordering::SeqCst), 0);
}
