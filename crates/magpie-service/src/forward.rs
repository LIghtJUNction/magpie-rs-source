//! Bounded local-provider candidate with optional, explicitly selected translation.
//! Not the full gateway contract: no account routing, prompt logging, or Usage accounting.

use crate::protocol::{
    self, ErrorCategory, ErrorKind, FinishReason, GatewayError, Limits, Outcome, Protocol,
    ResponseContext, SseConverter, StreamBatch,
};
use bytes::{Bytes, BytesMut};
use futures_util::{Stream, StreamExt, TryStreamExt};
use http_body_util::{BodyExt, Full, StreamBody, combinators::UnsyncBoxBody};
use hyper::{
    Request, Response, StatusCode,
    body::{Frame, Incoming},
    header,
    server::conn::http1,
    service::service_fn,
};
use hyper_util::rt::{TokioIo, TokioTimer};
use serde_json::{Value, json};
use std::{
    convert::Infallible,
    fs::File,
    io::{self, Read},
    net::{SocketAddr, TcpListener},
    path::Path,
    pin::Pin,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use tokio::{
    sync::{OwnedSemaphorePermit, Semaphore},
    time::{Instant, timeout, timeout_at},
};

const CONFIG_LIMIT: u64 = 64 * 1024;
const REQUEST_LIMIT: usize = 1024 * 1024;
const JSON_RESPONSE_LIMIT: usize = 4 * 1024 * 1024;
// Responses terminal events can include the complete output; a tiny delta-size
// cap would reject otherwise valid long responses. This is still a finite cap.
const EVENT_LIMIT: usize = 4 * 1024 * 1024;
const CHUNK_LIMIT: usize = 16 * 1024;
const ACTIVE_LIMIT: usize = 4;
const CONNECTION_LIMIT: usize = 16;
const BODY_TIMEOUT: Duration = Duration::from_secs(5);
const IDLE_TIMEOUT: Duration = Duration::from_secs(10);
const TOTAL_TIMEOUT: Duration = Duration::from_secs(120);
type Body = UnsyncBoxBody<Bytes, io::Error>;
type ByteStream = Pin<Box<dyn Stream<Item = Result<Bytes, reqwest::Error>> + Send>>;

/// Public configuration only. The credential itself is never stored here.
///
/// Optional JSON `upstream_protocol`: `openai_chat`, `openai_responses`,
/// `anthropic_messages`, or `gemini`. Omission retains native forwarding and
/// legacy Bearer authentication. `base_url` remains the literal loopback `/v1`
/// URL; Gemini endpoints are selected on the same origin under `/v1beta`.
#[derive(Clone, Debug)]
pub struct ForwardConfig {
    provider_id: String,
    upstream: SocketAddr,
    models: Vec<String>,
    api_key_env: Option<String>,
    upstream_protocol: Option<Protocol>,
}

impl ForwardConfig {
    pub fn load(path: &Path) -> io::Result<Self> {
        let metadata =
            std::fs::symlink_metadata(path).map_err(|_| invalid("forward_config_unreadable"))?;
        if !metadata.is_file() || metadata.file_type().is_symlink() || metadata.len() > CONFIG_LIMIT
        {
            return Err(invalid("forward_config_not_a_bounded_regular_file"));
        }
        let mut bytes = Vec::new();
        File::open(path)
            .map_err(|_| invalid("forward_config_unreadable"))?
            .take(CONFIG_LIMIT + 1)
            .read_to_end(&mut bytes)
            .map_err(|_| invalid("forward_config_unreadable"))?;
        if bytes.len() as u64 > CONFIG_LIMIT {
            return Err(invalid("forward_config_too_large"));
        }
        Self::from_json(&bytes)
    }

    pub fn from_json(bytes: &[u8]) -> io::Result<Self> {
        if bytes.len() as u64 > CONFIG_LIMIT {
            return Err(invalid("forward_config_too_large"));
        }
        let value: Value =
            serde_json::from_slice(bytes).map_err(|_| invalid("forward_config_invalid_json"))?;
        let object = value
            .as_object()
            .ok_or_else(|| invalid("forward_config_must_be_object"))?;
        if object.keys().any(|key| {
            ![
                "version",
                "provider_id",
                "base_url",
                "models",
                "api_key_env",
                "upstream_protocol",
            ]
            .contains(&key.as_str())
        }) || object.get("version").and_then(Value::as_u64) != Some(1)
        {
            return Err(invalid("forward_config_unknown_field_or_version"));
        }
        let provider_id = string_field(&value, "provider_id")?.to_owned();
        if !valid_id(&provider_id) || provider_id.contains('/') || provider_id.len() > 64 {
            return Err(invalid("forward_config_invalid_provider_id"));
        }
        let base = string_field(&value, "base_url")?;
        let upstream: SocketAddr = base
            .strip_prefix("http://")
            .and_then(|s| s.strip_suffix("/v1"))
            .ok_or_else(|| invalid("candidate_requires_literal_loopback_http_base_url"))?
            .parse()
            .map_err(|_| invalid("candidate_requires_literal_loopback_http_base_url"))?;
        if !upstream.ip().is_loopback() || upstream.port() == 0 {
            return Err(invalid("candidate_requires_literal_loopback_http_base_url"));
        }
        let items = value
            .get("models")
            .and_then(Value::as_array)
            .ok_or_else(|| invalid("forward_config_models_required"))?;
        if items.is_empty() || items.len() > 128 {
            return Err(invalid("forward_config_models_limit"));
        }
        let mut models = Vec::new();
        for item in items {
            let model = item
                .as_str()
                .filter(|s| valid_id(s))
                .ok_or_else(|| invalid("forward_config_invalid_model"))?;
            if models.iter().any(|existing| existing == model) {
                return Err(invalid("forward_config_duplicate_model"));
            }
            models.push(model.to_owned());
        }
        let api_key_env = match value.get("api_key_env") {
            None => None,
            Some(item) => {
                let name = item
                    .as_str()
                    .ok_or_else(|| invalid("forward_config_invalid_credential_reference"))?;
                if name.is_empty()
                    || name.len() > 128
                    || !name
                        .bytes()
                        .all(|b| b.is_ascii_uppercase() || b.is_ascii_digit() || b == b'_')
                {
                    return Err(invalid("forward_config_invalid_credential_reference"));
                }
                Some(name.to_owned())
            }
        };
        let upstream_protocol = match value.get("upstream_protocol") {
            None => None,
            Some(Value::String(name)) => Some(match name.as_str() {
                "openai_chat" => Protocol::OpenAiChat,
                "openai_responses" => Protocol::OpenAiResponses,
                "anthropic_messages" => Protocol::AnthropicMessages,
                "gemini" => Protocol::Gemini,
                _ => return Err(invalid("forward_config_invalid_upstream_protocol")),
            }),
            Some(_) => return Err(invalid("forward_config_invalid_upstream_protocol")),
        };
        Ok(Self {
            provider_id,
            upstream,
            models,
            api_key_env,
            upstream_protocol,
        })
    }

    fn models_json(&self) -> Value {
        json!({"object":"list","data":self.models.iter().map(|model| json!({
            "id":format!("{}/{model}",self.provider_id),"object":"model","owned_by":self.provider_id,
            "magpie":{"source":"configured","provider_health":"not_probed","capabilities":"not_probed"}
        })).collect::<Vec<_>>()})
    }
}

fn string_field<'a>(value: &'a Value, field: &str) -> io::Result<&'a str> {
    value
        .get(field)
        .and_then(Value::as_str)
        .ok_or_else(|| invalid("forward_config_missing_string_field"))
}
fn valid_id(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 256
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"/._:-".contains(&b))
}
fn invalid(code: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, code)
}
fn transport_error() -> io::Error {
    io::Error::new(io::ErrorKind::UnexpectedEof, "upstream_stream_incomplete")
}

/// CLI and desktop can host this service without initializing a display or GPU.
pub struct ForwardServer {
    listener: TcpListener,
    config: ForwardConfig,
}
impl ForwardServer {
    pub fn bind(address: SocketAddr, config: ForwardConfig) -> io::Result<Self> {
        if !address.ip().is_loopback() {
            return Err(invalid("candidate_requires_loopback_listener"));
        }
        let listener = TcpListener::bind(address)?;
        listener.set_nonblocking(true)?;
        Ok(Self { listener, config })
    }
    pub fn local_addr(&self) -> io::Result<SocketAddr> {
        self.listener.local_addr()
    }
    pub fn run(self) -> io::Result<()> {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()?
            .block_on(self.run_async())
    }
    async fn run_async(self) -> io::Result<()> {
        let address = self.listener.local_addr()?;
        if address == self.config.upstream {
            return Err(invalid("forward_config_self_target"));
        }
        let mut defaults = header::HeaderMap::new();
        if let Some(name) = &self.config.api_key_env {
            let secret =
                std::env::var(name).map_err(|_| invalid("provider_credential_unavailable"))?;
            if secret.is_empty() || secret.len() > 4096 {
                return Err(invalid("provider_credential_invalid"));
            }
            let (name, secret) = match self.config.upstream_protocol {
                Some(Protocol::AnthropicMessages) => ("x-api-key", secret),
                Some(Protocol::Gemini) => ("x-goog-api-key", secret),
                _ => ("authorization", format!("Bearer {secret}")),
            };
            let mut value = header::HeaderValue::from_str(&secret)
                .map_err(|_| invalid("provider_credential_invalid"))?;
            value.set_sensitive(true);
            defaults.insert(header::HeaderName::from_static(name), value);
        }
        if self.config.upstream_protocol == Some(Protocol::AnthropicMessages) {
            defaults.insert(
                "anthropic-version",
                header::HeaderValue::from_static("2023-06-01"),
            );
        }
        let client = reqwest::Client::builder()
            .default_headers(defaults)
            .redirect(reqwest::redirect::Policy::none())
            .no_proxy()
            .retry(reqwest::retry::never())
            .connect_timeout(Duration::from_secs(3))
            .read_timeout(IDLE_TIMEOUT)
            .pool_max_idle_per_host(ACTIVE_LIMIT)
            .pool_idle_timeout(Duration::from_secs(30))
            .http1_only()
            .build()
            .map_err(|_| invalid("provider_transport_initialization_failed"))?;
        let requests = Arc::new(Semaphore::new(ACTIVE_LIMIT));
        let connections = Arc::new(Semaphore::new(CONNECTION_LIMIT));
        let state = Arc::new(State {
            config: self.config,
            address,
            client,
            requests,
            stats: Arc::new(Stats::default()),
        });
        let listener = tokio::net::TcpListener::from_std(self.listener)?;
        loop {
            // A permit is acquired before accept: live connection tasks are bounded.
            let permit = connections
                .clone()
                .acquire_owned()
                .await
                .map_err(|_| invalid("listener_closed"))?;
            let (socket, peer) = listener.accept().await?;
            if !peer.ip().is_loopback() {
                continue;
            }
            let state = state.clone();
            tokio::spawn(async move {
                let _connection_permit = permit;
                let service = service_fn(move |request| handle(request, state.clone()));
                let mut builder = http1::Builder::new();
                builder
                    .keep_alive(false)
                    .max_headers(64)
                    .max_buf_size(32 * 1024)
                    .timer(TokioTimer::new())
                    .header_read_timeout(Duration::from_secs(2));
                // Also bounds a client that stops reading a response forever.
                let _ = timeout(
                    TOTAL_TIMEOUT + Duration::from_secs(5),
                    builder.serve_connection(TokioIo::new(socket), service),
                )
                .await;
            });
        }
    }
}

#[derive(Default)]
struct Stats {
    active: AtomicU64,
    completed: AtomicU64,
    failed: AtomicU64,
    cancelled: AtomicU64,
    next_request: AtomicU64,
}
struct State {
    config: ForwardConfig,
    address: SocketAddr,
    client: reqwest::Client,
    requests: Arc<Semaphore>,
    stats: Arc<Stats>,
}
struct Attempt {
    stats: Arc<Stats>,
    finished: bool,
    _permit: OwnedSemaphorePermit,
}
impl Attempt {
    fn new(stats: Arc<Stats>, permit: OwnedSemaphorePermit) -> Self {
        stats.active.fetch_add(1, Ordering::Relaxed);
        Self {
            stats,
            finished: false,
            _permit: permit,
        }
    }
    fn finish(&mut self, success: bool) {
        if !self.finished {
            let counter = if success {
                &self.stats.completed
            } else {
                &self.stats.failed
            };
            counter.fetch_add(1, Ordering::Relaxed);
            self.finished = true;
        }
    }
}
impl Drop for Attempt {
    fn drop(&mut self) {
        self.stats.active.fetch_sub(1, Ordering::Relaxed);
        if !self.finished {
            self.stats.cancelled.fetch_add(1, Ordering::Relaxed);
        }
    }
}

struct EntryRoute {
    protocol: Protocol,
    // Gemini owns model/stream selection in the URL, never in the JSON body.
    gemini: Option<(String, bool)>,
}

impl EntryRoute {
    fn parse(path: &str) -> Option<Self> {
        let protocol = match path {
            "/v1/chat/completions" => Protocol::OpenAiChat,
            "/v1/responses" => Protocol::OpenAiResponses,
            "/v1/messages" => Protocol::AnthropicMessages,
            _ => {
                let (model, action) = path.strip_prefix("/v1beta/models/")?.rsplit_once(':')?;
                let stream = match action {
                    "generateContent" => false,
                    "streamGenerateContent" => true,
                    _ => return None,
                };
                let mut decoded = Vec::with_capacity(model.len());
                let mut bytes = model.bytes();
                while let Some(byte) = bytes.next() {
                    decoded.push(if byte == b'%' {
                        let high = (bytes.next()? as char).to_digit(16)?;
                        let low = (bytes.next()? as char).to_digit(16)?;
                        (high * 16 + low) as u8
                    } else {
                        byte
                    });
                }
                let model = String::from_utf8(decoded).ok()?;
                if !valid_id(&model) {
                    return None;
                }
                return Some(Self {
                    protocol: Protocol::Gemini,
                    gemini: Some((model, stream)),
                });
            }
        };
        Some(Self {
            protocol,
            gemini: None,
        })
    }
}

fn upstream_url(upstream: SocketAddr, protocol: Protocol, model: &str, stream: bool) -> String {
    match protocol {
        Protocol::OpenAiChat => format!("http://{upstream}/v1/chat/completions"),
        Protocol::OpenAiResponses => format!("http://{upstream}/v1/responses"),
        Protocol::AnthropicMessages => format!("http://{upstream}/v1/messages"),
        Protocol::Gemini => {
            // Model IDs are bounded ASCII. Encode a single path parameter so a
            // slash, colon or dot segment cannot select another HTTP endpoint.
            let encoded: String = model
                .bytes()
                .map(|byte| {
                    if byte.is_ascii_alphanumeric() || b"_-".contains(&byte) {
                        (byte as char).to_string()
                    } else {
                        format!("%{byte:02X}")
                    }
                })
                .collect();
            let action = if stream {
                "streamGenerateContent?alt=sse"
            } else {
                "generateContent"
            };
            format!("http://{upstream}/v1beta/models/{encoded}:{action}")
        }
    }
}

fn response_context(state: &State, model: &str) -> ResponseContext {
    let created = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    let sequence = state.stats.next_request.fetch_add(1, Ordering::Relaxed);
    ResponseContext {
        id: format!("magpie_{}_{created}_{sequence}", std::process::id()),
        model: model.to_owned(),
        created,
    }
}

fn outcome_success(outcome: &Outcome, client: Protocol) -> bool {
    match outcome {
        Outcome::Finished(FinishReason::Length | FinishReason::ContentFilter)
            if client == Protocol::OpenAiResponses =>
        {
            false
        }
        Outcome::Finished(_) => true,
        _ => false,
    }
}

fn error_category(status: StatusCode) -> ErrorCategory {
    match status {
        StatusCode::UNAUTHORIZED => ErrorCategory::Authentication,
        StatusCode::FORBIDDEN => ErrorCategory::Permission,
        StatusCode::NOT_FOUND => ErrorCategory::ModelNotFound,
        StatusCode::TOO_MANY_REQUESTS => ErrorCategory::RateLimit,
        StatusCode::REQUEST_TIMEOUT | StatusCode::GATEWAY_TIMEOUT => ErrorCategory::Timeout,
        status if status.is_client_error() => ErrorCategory::Compatibility,
        _ => ErrorCategory::UpstreamServer,
    }
}

fn protocol_error(
    protocol: Protocol,
    status: StatusCode,
    category: ErrorCategory,
) -> Response<Body> {
    json_response(
        status,
        protocol::error_body(
            protocol,
            &GatewayError {
                category,
                status: status.as_u16(),
            },
        ),
    )
}

async fn handle(
    request: Request<Incoming>,
    state: Arc<State>,
) -> Result<Response<Body>, Infallible> {
    Ok(handle_inner(request, state).await)
}
async fn handle_inner(mut request: Request<Incoming>, state: Arc<State>) -> Response<Body> {
    let route = EntryRoute::parse(request.uri().path());
    let client = route
        .as_ref()
        .map_or(Protocol::OpenAiChat, |route| route.protocol);
    let typed_errors = state.config.upstream_protocol.is_some()
        || matches!(client, Protocol::AnthropicMessages | Protocol::Gemini);
    let error = |status: StatusCode, code: &'static str| {
        if typed_errors {
            let category = match code {
                "upstream_transport_error" => ErrorCategory::Network,
                "upstream_content_type_mismatch"
                | "upstream_json_incomplete_or_too_large"
                | "converted_response_too_large" => ErrorCategory::Compatibility,
                "upstream_http_error" if status.is_client_error() => match status {
                    StatusCode::UNAUTHORIZED
                    | StatusCode::FORBIDDEN
                    | StatusCode::NOT_FOUND
                    | StatusCode::TOO_MANY_REQUESTS => error_category(status),
                    _ => ErrorCategory::UpstreamClient,
                },
                _ => error_category(status),
            };
            protocol_error(client, status, category)
        } else {
            error(status, code)
        }
    };
    // Real peer was checked at accept. Untrusted proxy/browser headers do not grant local privilege.
    let headers = request.headers();
    if headers.contains_key(header::ORIGIN)
        || headers.contains_key("forwarded")
        || headers
            .keys()
            .any(|key| key.as_str().starts_with("x-forwarded-"))
    {
        return error(StatusCode::FORBIDDEN, "untrusted_origin_or_proxy");
    }
    if headers.get_all(header::HOST).iter().count() != 1
        || headers
            .get(header::HOST)
            .and_then(|h| h.to_str().ok())
            .and_then(|h| h.parse::<SocketAddr>().ok())
            != Some(state.address)
        || request.uri().scheme().is_some()
        || request.uri().authority().is_some()
    {
        return error(StatusCode::FORBIDDEN, "invalid_local_host");
    }
    if headers.contains_key(header::UPGRADE) {
        return error(StatusCode::UPGRADE_REQUIRED, "websocket_not_supported");
    }
    if headers.contains_key("x-magpie-account") || headers.contains_key("x-magpie-session") {
        return error(
            StatusCode::NOT_IMPLEMENTED,
            "account_and_session_routing_not_implemented",
        );
    }
    let path = request.uri().path().to_owned();
    if request.uri().query().is_some()
        && !(route
            .as_ref()
            .is_some_and(|route| route.gemini.as_ref().is_some_and(|(_, stream)| *stream))
            && request.uri().query() == Some("alt=sse"))
    {
        return error(StatusCode::BAD_REQUEST, "query_not_supported");
    }
    if request.method() == hyper::Method::GET {
        return match path.as_str() {
            "/health" => json_response(
                StatusCode::OK,
                json!({"status":"ok","mode":"forward_candidate",
                "provider_health":"not_probed","limits":{"active_requests":ACTIVE_LIMIT,"waiting_requests":0,"connections":CONNECTION_LIMIT,
                    "request_bytes":REQUEST_LIMIT,"json_response_bytes":JSON_RESPONSE_LIMIT,"sse_event_bytes":EVENT_LIMIT,"forward_chunk_bytes":CHUNK_LIMIT},
                "counter_semantics":"upstream_protocol_completion_not_client_delivery_or_usage",
                "transport_counters":{"active":state.stats.active.load(Ordering::Relaxed),
                    "completed":state.stats.completed.load(Ordering::Relaxed),"failed":state.stats.failed.load(Ordering::Relaxed),
                    "cancelled":state.stats.cancelled.load(Ordering::Relaxed)},"usage_accounting":"not_implemented"}),
            ),
            "/v1/models" => json_response(StatusCode::OK, state.config.models_json()),
            _ => error(StatusCode::NOT_IMPLEMENTED, "endpoint_not_implemented"),
        };
    }
    if request.method() != hyper::Method::POST {
        return error(StatusCode::METHOD_NOT_ALLOWED, "method_not_allowed");
    }
    let Some(route) = route else {
        return error(StatusCode::NOT_IMPLEMENTED, "endpoint_not_implemented");
    };
    if headers.contains_key(header::CONTENT_ENCODING)
        || !headers
            .get(header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .is_some_and(|v| {
                v.split(';')
                    .next()
                    .is_some_and(|s| s.trim().eq_ignore_ascii_case("application/json"))
            })
    {
        return error(StatusCode::UNSUPPORTED_MEDIA_TYPE, "json_body_required");
    }
    let permit = match state.requests.clone().try_acquire_owned() {
        Ok(permit) => permit,
        Err(_) => return error(StatusCode::SERVICE_UNAVAILABLE, "gateway_concurrency_limit"),
    };
    if headers
        .get(header::CONTENT_LENGTH)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.parse::<u64>().ok())
        .is_some_and(|n| n > REQUEST_LIMIT as u64)
    {
        return error(StatusCode::PAYLOAD_TOO_LARGE, "request_body_too_large");
    }
    let bytes = match timeout(BODY_TIMEOUT, read_request(request.body_mut())).await {
        Ok(Ok(bytes)) => bytes,
        Ok(Err(code)) => return error(StatusCode::PAYLOAD_TOO_LARGE, code),
        Err(_) => return error(StatusCode::REQUEST_TIMEOUT, "request_body_timeout"),
    };
    let mut value: Value = match serde_json::from_slice(&bytes) {
        Ok(value) => value,
        Err(_) => return error(StatusCode::BAD_REQUEST, "invalid_json"),
    };
    let Some(object) = value.as_object() else {
        return error(StatusCode::BAD_REQUEST, "json_object_required");
    };
    let public_model = match &route.gemini {
        Some((model, _)) => {
            if object.contains_key("model") || object.contains_key("stream") {
                return error(
                    StatusCode::BAD_REQUEST,
                    "gemini_model_and_stream_are_url_fields",
                );
            }
            model.clone()
        }
        None => match object.get("model").and_then(Value::as_str) {
            Some(model) => model.to_owned(),
            None => return error(StatusCode::BAD_REQUEST, "model_required"),
        },
    };
    let Some(model) = public_model
        .strip_prefix(&format!("{}/", state.config.provider_id))
        .filter(|model| state.config.models.iter().any(|known| known == model))
        .map(str::to_owned)
    else {
        return error(StatusCode::NOT_FOUND, "model_not_found");
    };
    let stream = match (route.gemini.as_ref(), object.get("stream")) {
        (Some((_, stream)), _) => *stream,
        (None, None) => false,
        (None, Some(Value::Bool(value))) => *value,
        (None, Some(_)) => {
            return attributed(
                error(StatusCode::BAD_REQUEST, "stream_must_be_boolean"),
                &state.config.provider_id,
                &public_model,
            );
        }
    };
    let provider = state.config.upstream_protocol.unwrap_or(client);
    let translated = provider != client;
    if provider == Protocol::Gemini && matches!(model.as_str(), "." | "..") {
        return attributed(
            error(StatusCode::UNPROCESSABLE_ENTITY, "invalid_gemini_model"),
            &state.config.provider_id,
            &public_model,
        );
    }
    if translated {
        value = match protocol::convert_request(client, provider, &value, &model) {
            Ok(value) => value,
            Err(failure) => {
                let status = match failure.kind {
                    ErrorKind::InvalidInput | ErrorKind::InvalidJson | ErrorKind::InvalidUtf8 => {
                        StatusCode::BAD_REQUEST
                    }
                    ErrorKind::LimitExceeded => StatusCode::PAYLOAD_TOO_LARGE,
                    _ => StatusCode::UNPROCESSABLE_ENTITY,
                };
                return attributed(
                    protocol_error(client, status, ErrorCategory::Compatibility),
                    &state.config.provider_id,
                    &public_model,
                );
            }
        };
        if provider != Protocol::Gemini {
            value["stream"] = json!(stream);
        }
        if stream && provider == Protocol::OpenAiChat {
            value["stream_options"] = json!({"include_usage":true});
        }
    } else if client != Protocol::Gemini {
        value["model"] = Value::String(model.clone());
    }
    let payload = match serde_json::to_vec(&value) {
        Ok(payload) => payload,
        Err(_) => return error(StatusCode::BAD_REQUEST, "invalid_json"),
    };
    if payload.len() > REQUEST_LIMIT {
        return attributed(
            error(
                StatusCode::PAYLOAD_TOO_LARGE,
                "converted_request_body_too_large",
            ),
            &state.config.provider_id,
            &public_model,
        );
    }
    // Both request bytes and parsed values are bounded and released before response streaming.
    drop(bytes);
    drop(value);
    let mut attempt = Attempt::new(state.stats.clone(), permit);
    let deadline = Instant::now() + TOTAL_TIMEOUT;
    let upstream = timeout(
        IDLE_TIMEOUT,
        state
            .client
            .post(upstream_url(
                state.config.upstream,
                provider,
                &model,
                stream,
            ))
            .header(header::CONTENT_TYPE, "application/json")
            .header(
                header::ACCEPT,
                if stream {
                    "text/event-stream"
                } else {
                    "application/json"
                },
            )
            .body(payload)
            .send(),
    )
    .await;
    let upstream = match upstream {
        Ok(Ok(response)) => response,
        Ok(Err(failure)) => {
            attempt.finish(false);
            return attributed(
                error(
                    if typed_errors && failure.is_timeout() {
                        StatusCode::GATEWAY_TIMEOUT
                    } else {
                        StatusCode::BAD_GATEWAY
                    },
                    if typed_errors && failure.is_timeout() {
                        "upstream_header_timeout"
                    } else {
                        "upstream_transport_error"
                    },
                ),
                &state.config.provider_id,
                &public_model,
            );
        }
        Err(_) => {
            attempt.finish(false);
            return attributed(
                error(StatusCode::GATEWAY_TIMEOUT, "upstream_header_timeout"),
                &state.config.provider_id,
                &public_model,
            );
        }
    };
    if !upstream.status().is_success() {
        // Upstream error bodies may echo prompts or keys. Do not relay them or Location/Set-Cookie.
        let status = upstream.status();
        let safe_status = if status.is_redirection() {
            StatusCode::BAD_GATEWAY
        } else {
            status
        };
        attempt.finish(false);
        return attributed(
            error(
                safe_status,
                if status.is_redirection() {
                    "upstream_redirect_refused"
                } else {
                    "upstream_http_error"
                },
            ),
            &state.config.provider_id,
            &public_model,
        );
    }
    let media = upstream
        .headers()
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .split(';')
        .next()
        .unwrap_or("")
        .trim()
        .to_ascii_lowercase();
    if upstream.headers().contains_key(header::CONTENT_ENCODING)
        || (stream && media != "text/event-stream")
        || (!stream && media != "application/json")
    {
        attempt.finish(false);
        return attributed(
            error(StatusCode::BAD_GATEWAY, "upstream_content_type_mismatch"),
            &state.config.provider_id,
            &public_model,
        );
    }
    if !stream {
        if translated || matches!(client, Protocol::AnthropicMessages | Protocol::Gemini) {
            let context = response_context(&state, &public_model);
            let collected = timeout_at(deadline, read_json_bytes(upstream)).await;
            let bytes = match collected {
                Ok(Ok(bytes)) => bytes,
                failure => {
                    attempt.finish(false);
                    let (status, category) = match failure {
                        Err(_) | Ok(Err(ResponseReadError::Timeout)) => {
                            (StatusCode::GATEWAY_TIMEOUT, ErrorCategory::Timeout)
                        }
                        Ok(Err(ResponseReadError::Network)) => {
                            (StatusCode::BAD_GATEWAY, ErrorCategory::Network)
                        }
                        _ => (StatusCode::BAD_GATEWAY, ErrorCategory::Compatibility),
                    };
                    return attributed(
                        protocol_error(client, status, category),
                        &state.config.provider_id,
                        &public_model,
                    );
                }
            };
            let converted = serde_json::from_slice::<Value>(&bytes)
                .ok()
                .and_then(|value| {
                    protocol::convert_response(provider, client, &value, &context).ok()
                });
            let response = match converted {
                Some(converted) => {
                    if let Outcome::Failed(category) = converted.outcome {
                        attempt.finish(false);
                        protocol_error(client, StatusCode::BAD_GATEWAY, category)
                    } else {
                        let success = outcome_success(&converted.outcome, client);
                        match serde_json::to_vec(&converted.body) {
                            Ok(bytes) if bytes.len() <= JSON_RESPONSE_LIMIT => {
                                attempt.finish(success);
                                bytes_response(
                                    StatusCode::OK,
                                    "application/json",
                                    Bytes::from(bytes),
                                )
                            }
                            _ => {
                                attempt.finish(false);
                                error(StatusCode::BAD_GATEWAY, "converted_response_too_large")
                            }
                        }
                    }
                }
                None => {
                    attempt.finish(false);
                    protocol_error(
                        client,
                        StatusCode::BAD_GATEWAY,
                        ErrorCategory::Compatibility,
                    )
                }
            };
            return attributed(response, &state.config.provider_id, &public_model);
        }
        let collected = timeout_at(deadline, read_json_response(upstream)).await;
        let validated = collected.ok().and_then(Result::ok).and_then(|bytes| {
            protocol_response_outcome(&bytes, path == "/v1/responses")
                .map(|success| (bytes, success))
        });
        return match validated {
            Some((bytes, success)) => {
                attempt.finish(success);
                attributed(
                    bytes_response(StatusCode::OK, "application/json", bytes),
                    &state.config.provider_id,
                    &public_model,
                )
            }
            _ => {
                attempt.finish(false);
                attributed(
                    error(
                        StatusCode::BAD_GATEWAY,
                        "upstream_json_incomplete_or_too_large",
                    ),
                    &state.config.provider_id,
                    &public_model,
                )
            }
        };
    }
    if translated {
        let limits = Limits {
            max_chunk_bytes: CHUNK_LIMIT,
            max_event_bytes: EVENT_LIMIT,
            max_output_bytes: JSON_RESPONSE_LIMIT,
            ..Limits::default()
        };
        let converter = match SseConverter::new(
            provider,
            client,
            response_context(&state, &public_model),
            limits,
        ) {
            Ok(converter) => converter,
            Err(_) => {
                attempt.finish(false);
                return attributed(
                    protocol_error(
                        client,
                        StatusCode::BAD_GATEWAY,
                        ErrorCategory::Compatibility,
                    ),
                    &state.config.provider_id,
                    &public_model,
                );
            }
        };
        let context = ConvertedStreamContext {
            upstream: Some(Box::pin(upstream.bytes_stream())),
            pending: Bytes::new(),
            output: Bytes::new(),
            frames: Vec::new().into_iter(),
            converter,
            client,
            attempt,
            deadline,
            stopped: false,
        };
        let stream = futures_util::stream::unfold(context, |mut context| async move {
            loop {
                if !context.output.is_empty() {
                    let length = context.output.len().min(CHUNK_LIMIT);
                    let bytes = context.output.split_to(length);
                    return Some((Ok(bytes), context));
                }
                if let Some(frame) = context.frames.next() {
                    context.output = Bytes::from(frame);
                    continue;
                }
                if context.stopped {
                    return None;
                }
                if !context.pending.is_empty() {
                    let length = context.pending.len().min(CHUNK_LIMIT);
                    let bytes = context.pending.split_to(length);
                    match context.converter.push(&bytes) {
                        Ok(batch) => context.accept(batch),
                        Err(_) => {
                            if !context.fail(ErrorCategory::Compatibility) {
                                return Some((Err(transport_error()), context));
                            }
                        }
                    }
                    continue;
                }
                let limit = context.deadline.min(Instant::now() + IDLE_TIMEOUT);
                let upstream = context.upstream.as_mut()?;
                match timeout_at(limit, upstream.next()).await {
                    Ok(Some(Ok(bytes))) => context.pending = bytes,
                    Ok(None) => match context.converter.finish() {
                        Ok(batch) => context.accept(batch),
                        Err(_) => {
                            if !context.fail(ErrorCategory::Compatibility) {
                                return Some((Err(transport_error()), context));
                            }
                        }
                    },
                    failure => {
                        let category = match failure {
                            Err(_) => ErrorCategory::Timeout,
                            Ok(Some(Err(error))) if error.is_timeout() => ErrorCategory::Timeout,
                            _ => ErrorCategory::Network,
                        };
                        if !context.fail(category) {
                            return Some((Err(transport_error()), context));
                        }
                    }
                }
            }
        });
        return stream_response(stream, &state.config.provider_id, &public_model);
    }
    let context = StreamContext {
        upstream: Some(Box::pin(upstream.bytes_stream())),
        pending: Bytes::new(),
        output: Bytes::new(),
        attempt,
        parser: SseCompletion::for_protocol(client),
        deadline,
        stopped: false,
    };
    let stream = futures_util::stream::unfold(context, |mut context| async move {
        if context.stopped {
            return None;
        }
        loop {
            if !context.output.is_empty() {
                let length = context.output.len().min(CHUNK_LIMIT);
                let bytes = context.output.split_to(length);
                return Some((Ok(bytes), context));
            }
            if context.parser.completed {
                // A CR already completes the event. Its optional LF may still
                // be in the next transport chunk and belongs to the raw output.
                if context.parser.pending_cr && context.parser.cr_ends_event {
                    if context.pending.is_empty() {
                        let limit = context.deadline.min(Instant::now() + IDLE_TIMEOUT);
                        let upstream = context.upstream.as_mut()?;
                        if let Ok(Some(Ok(bytes))) = timeout_at(limit, upstream.next()).await {
                            context.pending = bytes;
                        }
                    }
                    if context.pending.first() == Some(&b'\n') {
                        context.pending = Bytes::new();
                        context.upstream = None;
                        context.parser.pending_cr = false;
                        return Some((Ok(Bytes::from_static(b"\n")), context));
                    }
                }
                return None;
            }
            if context.pending.is_empty() {
                let limit = context.deadline.min(Instant::now() + IDLE_TIMEOUT);
                let upstream = context.upstream.as_mut()?;
                match timeout_at(limit, upstream.next()).await {
                    Ok(Some(Ok(bytes))) => {
                        context.pending = bytes;
                    }
                    Ok(None) if context.parser.finish_native() => {
                        context.attempt.finish(context.parser.successful);
                        return None;
                    }
                    _ => {
                        context.attempt.finish(false);
                        context.stopped = true;
                        return Some((Err(transport_error()), context));
                    }
                }
            }
            while !context.pending.is_empty() {
                let byte = context.pending.split_to(1)[0];
                match context.parser.push(byte) {
                    Ok(Some(event)) => {
                        context.output = event;
                        if context.parser.completed {
                            // Protocol completion is known now, not only when HTTP closes.
                            context.attempt.finish(context.parser.successful);
                            if !context.parser.pending_cr {
                                context.upstream = None;
                                context.pending = Bytes::new();
                            }
                        }
                        break;
                    }
                    Ok(None) => {}
                    Err(()) => {
                        // Never expose an upstream error event which could echo a key/prompt.
                        context.attempt.finish(false);
                        context.stopped = true;
                        return Some((Err(transport_error()), context));
                    }
                }
            }
        }
    });
    stream_response(stream, &state.config.provider_id, &public_model)
}

fn stream_response(
    stream: impl Stream<Item = Result<Bytes, io::Error>> + Send + 'static,
    provider: &str,
    model: &str,
) -> Response<Body> {
    let body = StreamBody::new(stream.map_ok(Frame::data)).boxed_unsync();
    let mut response = Response::new(body);
    response.headers_mut().insert(
        header::CONTENT_TYPE,
        header::HeaderValue::from_static("text/event-stream"),
    );
    response.headers_mut().insert(
        header::CACHE_CONTROL,
        header::HeaderValue::from_static("no-store"),
    );
    attributed(response, provider, model)
}

async fn read_request(body: &mut Incoming) -> Result<Vec<u8>, &'static str> {
    let mut bytes = Vec::new();
    while let Some(frame) = body.frame().await {
        let frame = frame.map_err(|_| "request_body_incomplete")?;
        if let Ok(data) = frame.into_data() {
            if bytes.len().saturating_add(data.len()) > REQUEST_LIMIT {
                return Err("request_body_too_large");
            }
            bytes.extend_from_slice(&data);
        }
    }
    Ok(bytes)
}
async fn read_json_response(response: reqwest::Response) -> Result<Bytes, ()> {
    let bytes = read_json_bytes(response).await.map_err(|_| ())?;
    let value: Value = serde_json::from_slice(&bytes).map_err(|_| ())?;
    if !value.is_object() || value.get("error").is_some_and(|error| !error.is_null()) {
        return Err(());
    }
    Ok(bytes)
}

enum ResponseReadError {
    Limit,
    Network,
    Timeout,
}

async fn read_json_bytes(response: reqwest::Response) -> Result<Bytes, ResponseReadError> {
    if response
        .content_length()
        .is_some_and(|n| n > JSON_RESPONSE_LIMIT as u64)
    {
        return Err(ResponseReadError::Limit);
    }
    let mut stream = response.bytes_stream();
    let mut bytes = BytesMut::new();
    while let Some(data) = stream.next().await {
        let data = data.map_err(|failure| {
            if failure.is_timeout() {
                ResponseReadError::Timeout
            } else {
                ResponseReadError::Network
            }
        })?;
        if bytes.len().saturating_add(data.len()) > JSON_RESPONSE_LIMIT {
            return Err(ResponseReadError::Limit);
        }
        bytes.extend_from_slice(&data);
    }
    Ok(bytes.freeze())
}

fn protocol_response_outcome(bytes: &[u8], responses: bool) -> Option<bool> {
    let value = serde_json::from_slice::<Value>(bytes).ok()?;
    if responses {
        responses_outcome(&value)
    } else {
        (value.get("object").and_then(Value::as_str) == Some("chat.completion")
            && value
                .get("choices")
                .and_then(Value::as_array)
                .is_some_and(|choices| {
                    !choices.is_empty()
                        && choices.iter().all(|choice| {
                            choice.get("message").is_some_and(Value::is_object)
                                && choice.get("finish_reason").is_some_and(valid_finish_reason)
                        })
                }))
        .then_some(true)
    }
}
fn responses_outcome(value: &Value) -> Option<bool> {
    if value.get("object").and_then(Value::as_str) != Some("response")
        || !value.get("output").is_some_and(Value::is_array)
        || value.get("error").is_some_and(|error| !error.is_null())
    {
        return None;
    }
    match value.get("status").and_then(Value::as_str) {
        Some("completed") => Some(true),
        // These are valid protocol terminal results, not a broken transport.
        Some("incomplete")
            if matches!(
                value
                    .pointer("/incomplete_details/reason")
                    .and_then(Value::as_str),
                Some("max_output_tokens" | "content_filter")
            ) =>
        {
            Some(false)
        }
        _ => None,
    }
}

fn valid_finish_reason(value: &Value) -> bool {
    matches!(
        value.as_str(),
        Some("stop" | "length" | "tool_calls" | "content_filter" | "function_call")
    )
}

struct ConvertedStreamContext {
    upstream: Option<ByteStream>,
    pending: Bytes,
    output: Bytes,
    frames: std::vec::IntoIter<Vec<u8>>,
    converter: SseConverter,
    client: Protocol,
    attempt: Attempt,
    deadline: Instant,
    stopped: bool,
}

impl ConvertedStreamContext {
    fn accept(&mut self, batch: StreamBatch) {
        self.frames = batch.frames.into_iter();
        if let Some(end) = batch.end {
            self.attempt
                .finish(outcome_success(&end.outcome, self.client));
            self.stopped = true;
            self.upstream = None;
            self.pending = Bytes::new();
        }
    }

    fn fail(&mut self, category: ErrorCategory) -> bool {
        self.upstream = None;
        self.pending = Bytes::new();
        self.stopped = true;
        self.attempt.finish(false);
        match self.converter.abort(GatewayError {
            category,
            status: if category == ErrorCategory::Timeout {
                504
            } else {
                502
            },
        }) {
            Ok(batch) => {
                self.frames = batch.frames.into_iter();
                true
            }
            Err(_) => false,
        }
    }
}

impl Drop for ConvertedStreamContext {
    fn drop(&mut self) {
        if !self.attempt.finished {
            // Dropping the HTTP body drops this state: no detached producer or
            // queue survives a caller disconnect. Attempt records cancellation.
            let _ = self.converter.cancel();
        }
    }
}

struct StreamContext {
    upstream: Option<ByteStream>,
    pending: Bytes,
    output: Bytes,
    attempt: Attempt,
    parser: SseCompletion,
    deadline: Instant,
    stopped: bool,
}
/// Incremental terminal-event validation. It never reconstructs or modifies response text.
struct SseCompletion {
    event: Vec<u8>,
    line_start: usize,
    pending_cr: bool,
    cr_ends_event: bool,
    first_event: bool,
    responses: bool,
    protocol: Protocol,
    completed: bool,
    successful: bool,
    native_started: bool,
    native_finished: bool,
    started_choices: [bool; 128],
    finished_choices: [bool; 128],
}
impl SseCompletion {
    fn new(responses: bool) -> Self {
        Self {
            event: Vec::new(),
            line_start: 0,
            pending_cr: false,
            cr_ends_event: false,
            first_event: true,
            responses,
            protocol: if responses {
                Protocol::OpenAiResponses
            } else {
                Protocol::OpenAiChat
            },
            completed: false,
            successful: false,
            native_started: false,
            native_finished: false,
            started_choices: [false; 128],
            finished_choices: [false; 128],
        }
    }
    fn for_protocol(protocol: Protocol) -> Self {
        Self {
            protocol,
            ..Self::new(protocol == Protocol::OpenAiResponses)
        }
    }
    fn finish_native(&mut self) -> bool {
        if self.protocol == Protocol::Gemini && self.native_finished && self.event.is_empty() {
            self.completed = true;
            self.successful = true;
            true
        } else {
            false
        }
    }
    fn observe_native(&mut self, value: &Value, event_type: Option<&str>) -> Result<(), ()> {
        if value.get("error").is_some_and(|error| !error.is_null()) {
            return Err(());
        }
        if self.protocol == Protocol::Gemini {
            if let Some(candidates) = value.get("candidates").and_then(Value::as_array) {
                for candidate in candidates {
                    let index = candidate.get("index").and_then(Value::as_u64).unwrap_or(0);
                    if index >= 128 {
                        return Err(());
                    }
                    self.started_choices[index as usize] = true;
                    if let Some(reason) = candidate.get("finishReason").filter(|v| !v.is_null()) {
                        if !matches!(
                            reason.as_str(),
                            Some("STOP" | "MAX_TOKENS" | "SAFETY" | "RECITATION")
                        ) {
                            return Err(());
                        }
                        self.finished_choices[index as usize] = true;
                    }
                }
                self.native_finished = self.started_choices.iter().any(|started| *started)
                    && self
                        .started_choices
                        .iter()
                        .zip(self.finished_choices)
                        .all(|(started, finished)| !*started || finished);
            }
            return Ok(());
        }
        let kind = value.get("type").and_then(Value::as_str).ok_or(())?;
        if event_type.is_some_and(|event| event != kind) {
            return Err(());
        }
        match kind {
            "ping" => {}
            "message_start" if !self.native_started => {
                let message = value.get("message").ok_or(())?;
                if message.get("type").and_then(Value::as_str) != Some("message")
                    || message.get("role").and_then(Value::as_str) != Some("assistant")
                    || message.get("error").is_some_and(|error| !error.is_null())
                {
                    return Err(());
                }
                self.native_started = true;
            }
            "content_block_start" | "content_block_delta" | "content_block_stop"
                if self.native_started && !self.native_finished =>
            {
                let index = value
                    .get("index")
                    .and_then(Value::as_u64)
                    .filter(|i| *i < 128)
                    .ok_or(())? as usize;
                match kind {
                    "content_block_start" if !self.started_choices[index] => {
                        self.started_choices[index] = true;
                    }
                    "content_block_delta"
                        if self.started_choices[index] && !self.finished_choices[index] => {}
                    "content_block_stop"
                        if self.started_choices[index] && !self.finished_choices[index] =>
                    {
                        self.finished_choices[index] = true;
                    }
                    _ => return Err(()),
                }
            }
            "message_delta" if self.native_started && !self.native_finished => {
                if let Some(reason) = value.pointer("/delta/stop_reason").filter(|v| !v.is_null()) {
                    if !matches!(
                        reason.as_str(),
                        Some("end_turn" | "max_tokens" | "stop_sequence" | "tool_use")
                    ) {
                        return Err(());
                    }
                    if self
                        .started_choices
                        .iter()
                        .zip(self.finished_choices)
                        .any(|(started, finished)| *started && !finished)
                    {
                        return Err(());
                    }
                    self.native_finished = true;
                }
            }
            "message_stop" if self.native_started && self.native_finished => {
                self.completed = true;
                self.successful = true;
            }
            _ => return Err(()),
        }
        Ok(())
    }
    fn push(&mut self, byte: u8) -> Result<Option<Bytes>, ()> {
        if self.pending_cr {
            self.pending_cr = false;
            if byte == b'\n' {
                if self.cr_ends_event {
                    return Ok(Some(Bytes::from_static(b"\n")));
                }
                self.append(byte)?;
                self.line_start = self.event.len();
                return Ok(None);
            }
        }
        self.append(byte)?;
        match byte {
            b'\r' => {
                let event = self.end_line(self.event.len() - 1)?;
                self.pending_cr = true;
                self.cr_ends_event = event.is_some();
                return Ok(event);
            }
            b'\n' => return self.end_line(self.event.len() - 1),
            _ => {}
        }
        Ok(None)
    }
    fn append(&mut self, byte: u8) -> Result<(), ()> {
        if self.event.len() == EVENT_LIMIT {
            return Err(());
        }
        self.event.push(byte);
        Ok(())
    }
    fn end_line(&mut self, content_end: usize) -> Result<Option<Bytes>, ()> {
        let line = &self.event[self.line_start..content_end];
        let empty = line.is_empty()
            || (self.first_event && self.line_start == 0 && line == b"\xef\xbb\xbf");
        self.line_start = self.event.len();
        if empty {
            let raw = std::str::from_utf8(&self.event).map_err(|_| ())?;
            let text = if self.first_event {
                raw.strip_prefix('\u{feff}').unwrap_or(raw)
            } else {
                raw
            };
            self.first_event = false;
            let event_type = text
                .split(['\r', '\n'])
                .filter_map(|line| {
                    let (field, value) = line.split_once(':').unwrap_or((line, ""));
                    (field == "event").then(|| value.strip_prefix(' ').unwrap_or(value))
                })
                .next_back();
            if matches!(event_type, Some("error" | "response.failed")) {
                return Err(());
            }
            let data = text
                .split(['\r', '\n'])
                .filter_map(|line| {
                    line.strip_prefix("data:")
                        .map(|s| s.strip_prefix(' ').unwrap_or(s))
                })
                .collect::<Vec<_>>()
                .join("\n");
            if !data.is_empty() {
                if matches!(
                    self.protocol,
                    Protocol::AnthropicMessages | Protocol::Gemini
                ) {
                    let value: Value = serde_json::from_str(&data).map_err(|_| ())?;
                    let event_type = event_type.map(str::to_owned);
                    self.observe_native(&value, event_type.as_deref())?;
                } else if !self.responses && data == "[DONE]" {
                    if !self.started_choices.iter().any(|started| *started)
                        || self
                            .started_choices
                            .iter()
                            .zip(self.finished_choices)
                            .any(|(started, finished)| *started && !finished)
                    {
                        return Err(());
                    }
                    self.completed = true;
                    self.successful = true;
                } else {
                    let value: Value = serde_json::from_str(&data).map_err(|_| ())?;
                    if value.get("error").is_some_and(|error| !error.is_null())
                        || value
                            .pointer("/response/error")
                            .is_some_and(|error| !error.is_null())
                    {
                        return Err(());
                    }
                    let data_type = value.get("type").and_then(Value::as_str);
                    if self.responses
                        && !data_type.is_some_and(|kind| kind.starts_with("response."))
                    {
                        return Err(());
                    }
                    if event_type.is_some_and(|kind| {
                        kind.starts_with("response.") && Some(kind) != data_type
                    }) {
                        return Err(());
                    }
                    match data_type {
                        Some("response.completed" | "response.incomplete") if self.responses => {
                            let outcome =
                                responses_outcome(value.get("response").ok_or(())?).ok_or(())?;
                            if outcome != (data_type == Some("response.completed")) {
                                return Err(());
                            }
                            self.completed = true;
                            self.successful = outcome;
                        }
                        Some("response.failed" | "response.incomplete" | "error") => return Err(()),
                        _ => {}
                    }
                    if !self.responses {
                        let choices = value.get("choices").and_then(Value::as_array).ok_or(())?;
                        if choices.len() > self.started_choices.len() {
                            return Err(());
                        }
                        for choice in choices {
                            let index = choice
                                .get("index")
                                .and_then(Value::as_u64)
                                .filter(|index| *index < 128)
                                .ok_or(())? as usize;
                            let finish = choice
                                .get("finish_reason")
                                .filter(|reason| !reason.is_null());
                            if finish.is_some_and(|reason| !valid_finish_reason(reason))
                                || (!choice.get("delta").is_some_and(Value::is_object)
                                    && finish.is_none())
                            {
                                return Err(());
                            }
                            self.started_choices[index] = true;
                            if finish.is_some() {
                                self.finished_choices[index] = true;
                            }
                        }
                    }
                }
            }
            self.line_start = 0;
            return Ok(Some(Bytes::from(std::mem::take(&mut self.event))));
        }
        Ok(None)
    }
}

fn error(status: StatusCode, code: &'static str) -> Response<Body> {
    json_response(
        status,
        json!({"error":{"type":"magpie_gateway_error","code":code,"message":code}}),
    )
}
fn json_response(status: StatusCode, value: Value) -> Response<Body> {
    bytes_response(status, "application/json", Bytes::from(value.to_string()))
}
fn bytes_response(status: StatusCode, media: &'static str, bytes: Bytes) -> Response<Body> {
    let body = Full::new(bytes)
        .map_err(|never| match never {})
        .boxed_unsync();
    let mut response = Response::new(body);
    *response.status_mut() = status;
    response.headers_mut().insert(
        header::CONTENT_TYPE,
        header::HeaderValue::from_static(media),
    );
    response.headers_mut().insert(
        header::CACHE_CONTROL,
        header::HeaderValue::from_static("no-store"),
    );
    response
}
fn attributed(mut response: Response<Body>, provider: &str, model: &str) -> Response<Body> {
    // Both values came from bounded ASCII config IDs, never raw caller headers.
    if let Ok(value) = header::HeaderValue::from_str(provider) {
        response.headers_mut().insert("x-magpie-provider", value);
    }
    if let Ok(value) = header::HeaderValue::from_str(model) {
        response.headers_mut().insert("x-magpie-model", value);
    }
    response
}

/// Validate local listener/catalog before an explicit Agent config commit.
/// This does not claim the provider is reachable or supports a given protocol.
pub fn probe_model(address: SocketAddr, model: &str) -> io::Result<()> {
    if inspected_models(address)?.iter().any(|id| id == model) {
        Ok(())
    } else {
        Err(invalid("gateway_model_not_configured"))
    }
}

/// Inspect the local forwarder and configured models without probing its provider.
///
/// # Errors
/// Returns a static diagnostic for invalid addresses, transport or catalog data.
pub fn inspect_gateway(address: SocketAddr) -> io::Result<String> {
    let models = inspected_models(address)?;
    Ok(json!({
        "version": magpie_core::VERSION,
        "gateway": {
            "address": address.to_string(),
            "mode": "forward_candidate",
            "provider_health": "not_probed",
            "models": models.into_iter().map(|id| json!({
                "id": id, "source": "configured"
            })).collect::<Vec<_>>()
        }
    })
    .to_string())
}

fn inspected_models(address: SocketAddr) -> io::Result<Vec<String>> {
    if !address.ip().is_loopback() || address.port() == 0 {
        return Err(invalid("candidate_requires_loopback_listener"));
    }
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|_| invalid("gateway_probe_failed"))?
        .block_on(async {
            let client = reqwest::Client::builder()
                .no_proxy()
                .redirect(reqwest::redirect::Policy::none())
                .retry(reqwest::retry::never())
                .timeout(Duration::from_secs(3))
                .build()
                .map_err(|_| invalid("gateway_probe_failed"))?;
            let response = client
                .get(format!("http://{address}/health"))
                .send()
                .await
                .map_err(|_| invalid("gateway_not_ready"))?;
            if !response.status().is_success() {
                return Err(invalid("gateway_not_ready"));
            }
            let bytes = read_json_response(response)
                .await
                .map_err(|_| invalid("gateway_probe_invalid"))?;
            let value: Value =
                serde_json::from_slice(&bytes).map_err(|_| invalid("gateway_probe_invalid"))?;
            if value.get("mode").and_then(Value::as_str) != Some("forward_candidate") {
                return Err(invalid("gateway_forwarder_not_ready"));
            }
            let response = client
                .get(format!("http://{address}/v1/models"))
                .send()
                .await
                .map_err(|_| invalid("gateway_not_ready"))?;
            if !response.status().is_success() {
                return Err(invalid("gateway_not_ready"));
            }
            let bytes = read_json_response(response)
                .await
                .map_err(|_| invalid("gateway_probe_invalid"))?;
            let value: Value =
                serde_json::from_slice(&bytes).map_err(|_| invalid("gateway_probe_invalid"))?;
            let items = value
                .get("data")
                .and_then(Value::as_array)
                .filter(|items| !items.is_empty() && items.len() <= 128)
                .ok_or_else(|| invalid("gateway_probe_invalid"))?;
            let mut models = Vec::with_capacity(items.len());
            for item in items {
                let id = item
                    .get("id")
                    .and_then(Value::as_str)
                    .ok_or_else(|| invalid("gateway_probe_invalid"))?;
                let (provider, model) = id
                    .split_once('/')
                    .ok_or_else(|| invalid("gateway_probe_invalid"))?;
                if !valid_id(provider)
                    || provider.len() > 64
                    || !valid_id(model)
                    || models.iter().any(|known| known == id)
                {
                    return Err(invalid("gateway_probe_invalid"));
                }
                models.push(id.to_owned());
            }
            Ok(models)
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn config_has_no_secret_url_or_unbounded_model_surface() {
        let good = br#"{"version":1,"provider_id":"local","base_url":"http://127.0.0.1:9999/v1","models":["fixture-a"]}"#;
        assert!(ForwardConfig::from_json(good).is_ok());
        for url in [
            "https://example.com/v1",
            "http://localhost:9/v1",
            "http://127.0.0.1:9/v1?secret=x",
            "http://user:key@127.0.0.1:9/v1",
        ] {
            let mut value: Value = serde_json::from_slice(good).unwrap();
            value["base_url"] = json!(url);
            assert!(ForwardConfig::from_json(value.to_string().as_bytes()).is_err());
        }
    }
    #[test]
    fn incremental_sse_handles_split_utf8_crlf_and_terminal_errors() {
        let text = "data: {\"choices\":[{\"index\":0,\"delta\":{\"content\":\"你好\"},\"finish_reason\":\"stop\"}]}\r\n\r\ndata: [DONE]\r\n\r\n";
        let mut parser = SseCompletion::new(false);
        for byte in text.as_bytes() {
            parser.push(*byte).unwrap();
        }
        assert!(parser.completed);
        let mut parser = SseCompletion::new(true);
        assert!(
            b"data: {\"type\":\"response.failed\"}\n\n"
                .iter()
                .try_for_each(|byte| parser.push(*byte).map(|_| ()))
                .is_err()
        );
        let mut parser = SseCompletion::new(false);
        assert!(
            vec![b'x'; EVENT_LIMIT + 1]
                .iter()
                .try_for_each(|byte| parser.push(*byte).map(|_| ()))
                .is_err()
        );
    }

    #[test]
    fn sse_retains_raw_delimiters_and_split_initial_bom() {
        let chat = "data: {\"choices\":[{\"index\":0,\"delta\":{\"content\":\"ok\"},\"finish_reason\":\"stop\"}]}\n\ndata: [DONE]\n\n";
        let responses = "event: response.completed\ndata: {\"type\":\"response.completed\",\"response\":{\"object\":\"response\",\"output\":[],\"status\":\"completed\",\"error\":null}}\n\n";
        for (is_responses, text) in [(false, chat), (true, responses)] {
            for endings in [
                &["\n"][..],
                &["\r"][..],
                &["\r\n"][..],
                // A CR immediately followed by LF is one line ending, so
                // do not generate them as two adjacent empty-line delimiters.
                &["\r", "\r\n", "\n"][..],
            ] {
                for bom in [false, true] {
                    let mut raw = if bom {
                        b"\xef\xbb\xbf".to_vec()
                    } else {
                        Vec::new()
                    };
                    for (index, line) in text.split_terminator('\n').enumerate() {
                        raw.extend_from_slice(line.as_bytes());
                        raw.extend_from_slice(endings[index % endings.len()].as_bytes());
                    }
                    let mut parser = SseCompletion::new(is_responses);
                    let mut forwarded = Vec::new();
                    for byte in &raw {
                        if let Some(event) = parser.push(*byte).expect("valid SSE byte") {
                            forwarded.extend_from_slice(&event);
                        }
                    }
                    assert!(parser.completed && parser.successful, "raw input: {raw:?}");
                    assert_eq!(forwarded, raw, "framing and BOM must remain byte exact");
                }
            }
        }
    }

    #[test]
    fn sse_cr_mixed_and_bom_errors_release_no_prefix() {
        for responses in [false, true] {
            for raw in [
                b": ping\rdata: {\"error\":{\"message\":\"fixture-secret\"}}\r\r\n\n".as_slice(),
                b": ping\rdata: {\"error\":{\"message\":\"fixture-secret\"}}\n\r\n\n",
                b"\xef\xbb\xbfdata: {\"error\":{\"message\":\"fixture-secret\"}}\n\n",
                b": ping\revent: error\rdata: {\"message\":\"fixture-secret\"}\r\r\n\n",
                b": ping\revent: error\r\ndata: {\"message\":\"fixture-secret\"}\n\r\n\n",
                b"\xef\xbb\xbfevent: error\ndata: {\"message\":\"fixture-secret\"}\n\n",
            ] {
                let mut parser = SseCompletion::new(responses);
                let mut rejected = false;
                for byte in raw {
                    match parser.push(*byte) {
                        Ok(None) => {}
                        Ok(Some(_)) => panic!("error event prefix escaped: {raw:?}"),
                        Err(()) => {
                            rejected = true;
                            break;
                        }
                    }
                }
                assert!(rejected, "error must be rejected: {raw:?}");
                assert!(!parser.completed);
            }
        }
    }

    #[test]
    fn inspection_rejects_non_loopback_and_zero_port_before_network_io() {
        for address in ["0.0.0.0:3425", "192.0.2.1:3425", "127.0.0.1:0", "[::1]:0"] {
            let address = address.parse().expect("fixture socket address");
            for error in [
                inspect_gateway(address).expect_err("invalid inspection address"),
                probe_model(address, "local/fixture").expect_err("invalid probe address"),
            ] {
                assert_eq!(error.to_string(), "candidate_requires_loopback_listener");
            }
        }
    }
}
