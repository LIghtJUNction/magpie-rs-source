//! Provider state adapter shared by the CLI and the bounded forwarding service.
//!
//! It never owns a second Provider database. Every new route is selected from a
//! freshly validated core snapshot; catalog/test operations commit through CAS.

use crate::protocol::Protocol as WireProtocol;
use futures_util::StreamExt;
use hyper::{StatusCode, header};
use magpie_core::providers::{
    Protocol, Provider, ProviderMutation, ProviderSnapshot, ProviderStore, is_valid_model_id,
    is_valid_provider_id, resolve_credential,
};
use serde_json::{Value, json};
use std::{
    io,
    net::{IpAddr, SocketAddr},
    path::{Path, PathBuf},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

const CATALOG_BYTES: usize = 1024 * 1024;
const CATALOG_ITEMS: usize = 512;
const MODEL_LIST_BYTES: usize = 2 * 1024 * 1024;
const OPERATION_TIMEOUT: Duration = Duration::from_secs(10);

/// A path, never an independently cached Provider state.
#[derive(Clone, Debug)]
pub struct ProviderSource {
    path: PathBuf,
    identity: String,
}

impl ProviderSource {
    pub fn new(path: PathBuf) -> io::Result<Self> {
        if !path.is_absolute() {
            return Err(invalid("provider_store_absolute_path_required"));
        }
        let store = ProviderStore::new(&path);
        store.load().map_err(|error| invalid(error.code()))?;
        let identity = store.identity().map_err(|error| invalid(error.code()))?;
        Ok(Self { path, identity })
    }

    pub(crate) fn snapshot(&self) -> Result<ProviderSnapshot, ServiceError> {
        ProviderStore::new(&self.path)
            .load()
            .map_err(|error| ServiceError::new(StatusCode::SERVICE_UNAVAILABLE, error.code()))
    }

    pub(crate) fn metadata(&self) -> Result<Value, ServiceError> {
        let snapshot = self.snapshot()?;
        Ok(self.snapshot_metadata(&snapshot))
    }

    fn snapshot_metadata(&self, snapshot: &ProviderSnapshot) -> Value {
        json!({"state_source":"provider_store", "store_identity":self.identity,
            "state_revision":snapshot.revision, "state_token":snapshot.token})
    }

    pub(crate) fn models(&self) -> Result<Value, ServiceError> {
        let snapshot = self.snapshot()?;
        let mut catalog = models_json(&snapshot)?;
        for (key, value) in self.snapshot_metadata(&snapshot).as_object().unwrap() {
            catalog[key] = value.clone();
        }
        Ok(catalog)
    }
}

/// Contains request credentials; deliberately has no Debug implementation.
pub(crate) struct ProviderRoute {
    pub provider_id: String,
    pub model: String,
    pub protocol: WireProtocol,
    pub endpoint: String,
    pub headers: header::HeaderMap,
    pub revision: u64,
    pub key_id: Option<String>,
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct ServiceError {
    pub status: StatusCode,
    pub code: &'static str,
}

impl ServiceError {
    const fn new(status: StatusCode, code: &'static str) -> Self {
        Self { status, code }
    }
}

/// Public model list only; configured models are not claimed to be online.
pub(crate) fn models_json(snapshot: &ProviderSnapshot) -> Result<Value, ServiceError> {
    let mut items = Vec::new();
    let mut size = 128usize;
    for provider in &snapshot.providers {
        if provider.deleted || !provider.enabled {
            continue;
        }
        for model in provider.enabled_models() {
            let native = vec![provider.protocol.as_str()];
            let clients = ["chat", "responses", "messages", "gemini"];
            let item = json!({
                "id": format!("{}/{model}", provider.id),
                "object":"model", "owned_by":provider.id,
                "name":model,"provider_name":provider.name,
                "magpie": {
                    "source":"configured", "provider_health":"not_probed",
                    "capabilities":"not_probed", "native_endpoints":native,
                    "client_endpoints":clients,
                    "catalog_stale":provider.catalog_error.is_some(),
                    "catalog_checked_at":provider.catalog_checked_at,
                    "state_revision":snapshot.revision,
                    "protocol":provider.protocol.as_str(),
                    "protocol_integrated":true,
                    "model_discovery_integrated":matches!(provider.protocol,Protocol::Chat|Protocol::Responses)
                }
            });
            size = size.saturating_add(item.to_string().len() + 1);
            if size > MODEL_LIST_BYTES {
                return Err(ServiceError::new(
                    StatusCode::SERVICE_UNAVAILABLE,
                    "provider_catalog_response_limit",
                ));
            }
            items.push(item);
        }
    }
    Ok(json!({"object":"list","data":items,"state_revision":snapshot.revision}))
}

pub(crate) fn route(
    snapshot: &ProviderSnapshot,
    public_model: &str,
    stream: bool,
    listener: SocketAddr,
) -> Result<ProviderRoute, ServiceError> {
    let (id, model) = public_model
        .split_once('/')
        .ok_or_else(|| ServiceError::new(StatusCode::NOT_FOUND, "model_not_found"))?;
    let provider = snapshot
        .provider(id)
        .filter(|provider| !provider.deleted)
        .ok_or_else(|| ServiceError::new(StatusCode::NOT_FOUND, "provider_not_found"))?;
    if !provider.enabled {
        return Err(ServiceError::new(
            StatusCode::SERVICE_UNAVAILABLE,
            "provider_disabled",
        ));
    }
    if !provider.enabled_models().contains(&model) {
        return Err(ServiceError::new(StatusCode::NOT_FOUND, "model_not_found"));
    }
    let protocol = match provider.protocol {
        Protocol::Chat => WireProtocol::OpenAiChat,
        Protocol::Responses => WireProtocol::OpenAiResponses,
        Protocol::Messages => WireProtocol::AnthropicMessages,
        Protocol::Gemini => WireProtocol::Gemini,
    };
    let suffix = match provider.protocol {
        Protocol::Chat => "chat/completions".to_owned(),
        Protocol::Responses => "responses".to_owned(),
        Protocol::Messages => "messages".to_owned(),
        Protocol::Gemini => {
            // Encode one model path parameter, including slash/@/colon/dot.
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
            format!("models/{encoded}:{action}")
        }
    };
    let endpoint = endpoint(provider, &suffix)?;
    let parsed = reqwest::Url::parse(&endpoint)
        .map_err(|_| ServiceError::new(StatusCode::SERVICE_UNAVAILABLE, "provider_url_invalid"))?;
    let loopback_target = parsed
        .host_str()
        .map(|host| host.trim_matches(['[', ']']))
        .is_some_and(|host| {
            host.eq_ignore_ascii_case("localhost")
                || host.parse::<IpAddr>().is_ok_and(|ip| ip.is_loopback())
        });
    if loopback_target && parsed.port_or_known_default() == Some(listener.port()) {
        return Err(ServiceError::new(
            StatusCode::SERVICE_UNAVAILABLE,
            "provider_self_target",
        ));
    }
    let (headers, key_id) = credential_headers(provider)?;
    Ok(ProviderRoute {
        provider_id: provider.id.clone(),
        model: model.to_owned(),
        protocol,
        endpoint,
        headers,
        revision: snapshot.revision,
        key_id,
    })
}

fn endpoint(provider: &Provider, suffix: &str) -> Result<String, ServiceError> {
    // The core validates intent; reqwest validates the actual transport URL too.
    let base = reqwest::Url::parse(&provider.base_url)
        .map_err(|_| ServiceError::new(StatusCode::SERVICE_UNAVAILABLE, "provider_url_invalid"))?;
    if !matches!(base.scheme(), "http" | "https")
        || !base.username().is_empty()
        || base.password().is_some()
        || base.query().is_some()
        || base.fragment().is_some()
        || base.host_str().is_none()
    {
        return Err(ServiceError::new(
            StatusCode::SERVICE_UNAVAILABLE,
            "provider_url_invalid",
        ));
    }
    Ok(format!(
        "{}/{suffix}",
        provider.base_url.trim_end_matches('/')
    ))
}

fn credential_headers(
    provider: &Provider,
) -> Result<(header::HeaderMap, Option<String>), ServiceError> {
    let mut headers = header::HeaderMap::new();
    if provider.protocol == Protocol::Messages {
        headers.insert(
            "anthropic-version",
            header::HeaderValue::from_static("2023-06-01"),
        );
    }
    if let Some(key) = provider.active_keys().next() {
        let secret = resolve_credential(key)
            .map_err(|error| ServiceError::new(StatusCode::SERVICE_UNAVAILABLE, error.code()))?;
        let (name, credential) = match provider.protocol {
            Protocol::Messages => ("x-api-key", secret.expose().to_owned()),
            Protocol::Gemini => ("x-goog-api-key", secret.expose().to_owned()),
            Protocol::Chat | Protocol::Responses => {
                ("authorization", format!("Bearer {}", secret.expose()))
            }
        };
        let mut value = header::HeaderValue::from_str(&credential).map_err(|_| {
            ServiceError::new(
                StatusCode::SERVICE_UNAVAILABLE,
                "provider_credential_invalid",
            )
        })?;
        value.set_sensitive(true);
        headers.insert(header::HeaderName::from_static(name), value);
        return Ok((headers, Some(key.id.clone())));
    }
    if !provider.keys.is_empty() {
        return Err(ServiceError::new(
            StatusCode::SERVICE_UNAVAILABLE,
            "provider_keys_paused",
        ));
    }
    // No-key local inference is valid. An upstream requiring auth returns its
    // sanitized 401; key presence is never invented merely to make a row look ready.
    Ok((headers, None))
}

#[derive(Clone, Debug)]
pub struct OperationReport {
    pub kind: &'static str,
    pub ok: bool,
    pub code: &'static str,
    pub http_status: Option<u16>,
    pub elapsed_ms: u64,
}

impl OperationReport {
    pub fn to_json(&self) -> Value {
        json!({"kind":self.kind,"ok":self.ok,"code":self.code,
            "http_status":self.http_status,"elapsed_ms":self.elapsed_ms})
    }
}

pub struct ProviderOperation {
    pub snapshot: ProviderSnapshot,
    pub report: OperationReport,
}

/// Explicit catalog fetch. Failures persist a controlled stale marker while
/// retaining every model from the last successful catalog.
pub fn refresh(
    store: &ProviderStore,
    expected: &str,
    id: &str,
) -> Result<ProviderOperation, &'static str> {
    let snapshot = checked_snapshot(store, expected)?;
    let provider = active_provider(&snapshot, id)?;
    let start = Instant::now();
    let outcome = run_network(async { discover(provider).await });
    let elapsed_ms = elapsed_ms(start);
    let (mutation, report) = match outcome {
        Ok((models, status)) => (
            ProviderMutation::CatalogSuccess {
                provider_id: id.to_owned(),
                models,
                checked_at: SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_secs(),
            },
            OperationReport {
                kind: "refresh",
                ok: true,
                code: "provider_catalog_refreshed",
                http_status: Some(status),
                elapsed_ms,
            },
        ),
        Err(error) => (
            ProviderMutation::CatalogFailure {
                provider_id: id.to_owned(),
                code: catalog_failure_code(error.code).to_owned(),
            },
            OperationReport {
                kind: "refresh",
                ok: false,
                code: error.code,
                http_status: error.status,
                elapsed_ms,
            },
        ),
    };
    let snapshot = store
        .apply(expected, mutation)
        .map_err(|error| error.code())?;
    Ok(ProviderOperation { snapshot, report })
}

/// Exactly one low-output request, never a retry under another key or protocol.
pub fn test(
    store: &ProviderStore,
    expected: &str,
    id: &str,
) -> Result<ProviderOperation, &'static str> {
    let snapshot = checked_snapshot(store, expected)?;
    let provider = active_provider(&snapshot, id)?;
    let start = Instant::now();
    let outcome = run_network(async { test_request(provider).await });
    // Keep the result attributable to the state the user reviewed. A concurrent
    // edit is a conflict, not an invitation to overwrite or validate new data.
    let snapshot = checked_snapshot(store, expected)?;
    let report = match outcome {
        Ok(status) => OperationReport {
            kind: "test",
            ok: true,
            code: "provider_test_passed",
            http_status: Some(status),
            elapsed_ms: elapsed_ms(start),
        },
        Err(error) => OperationReport {
            kind: "test",
            ok: false,
            code: error.code,
            http_status: error.status,
            elapsed_ms: elapsed_ms(start),
        },
    };
    Ok(ProviderOperation { snapshot, report })
}

fn catalog_failure_code(code: &str) -> &'static str {
    match code {
        "provider_timeout" => "timeout",
        "provider_http_401" => "unauthorized",
        "provider_http_403" => "forbidden",
        "provider_http_429" => "rate_limited",
        "provider_catalog_invalid"
        | "provider_content_type_mismatch"
        | "provider_response_too_large" => "invalid_response",
        "provider_protocol_not_integrated" => "not_supported",
        "provider_credential_unavailable"
        | "provider_credential_invalid"
        | "provider_keys_paused" => "credential_unavailable",
        "provider_transport_error" | "provider_transport_initialization_failed" => "network_error",
        _ => "upstream_error",
    }
}

fn checked_snapshot(
    store: &ProviderStore,
    expected: &str,
) -> Result<ProviderSnapshot, &'static str> {
    let snapshot = store.load().map_err(|error| error.code())?;
    if snapshot.token != expected {
        return Err("provider_revision_conflict");
    }
    Ok(snapshot)
}

fn active_provider<'a>(
    snapshot: &'a ProviderSnapshot,
    id: &str,
) -> Result<&'a Provider, &'static str> {
    let provider = snapshot
        .provider(id)
        .filter(|provider| !provider.deleted)
        .ok_or("provider_not_found")?;
    if !provider.enabled {
        return Err("provider_disabled");
    }
    Ok(provider)
}

fn elapsed_ms(start: Instant) -> u64 {
    start.elapsed().as_millis().min(u64::MAX as u128) as u64
}

#[derive(Clone, Copy)]
struct NetworkError {
    code: &'static str,
    status: Option<u16>,
}

impl NetworkError {
    const fn new(code: &'static str) -> Self {
        Self { code, status: None }
    }
    fn upstream(status: StatusCode) -> Self {
        Self {
            code: match status.as_u16() {
                401 => "provider_http_401",
                403 => "provider_http_403",
                404 => "provider_http_404",
                429 => "provider_http_429",
                500..=599 => "provider_http_5xx",
                300..=399 => "provider_redirect_refused",
                _ => "provider_http_error",
            },
            status: Some(status.as_u16()),
        }
    }
}

fn run_network<T>(
    future: impl std::future::Future<Output = Result<T, NetworkError>>,
) -> Result<T, NetworkError> {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .max_blocking_threads(2)
        .build()
        .map_err(|_| NetworkError::new("provider_transport_initialization_failed"))?
        .block_on(async {
            tokio::time::timeout(OPERATION_TIMEOUT, future)
                .await
                .map_err(|_| NetworkError::new("provider_timeout"))?
        })
}

fn client() -> Result<reqwest::Client, NetworkError> {
    reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .retry(reqwest::retry::never())
        .no_proxy()
        .connect_timeout(Duration::from_secs(3))
        .timeout(OPERATION_TIMEOUT)
        .pool_max_idle_per_host(0)
        .http1_only()
        .build()
        .map_err(|_| NetworkError::new("provider_transport_initialization_failed"))
}

fn native_protocol(provider: &Provider) -> Result<(), NetworkError> {
    if !matches!(provider.protocol, Protocol::Chat | Protocol::Responses) {
        return Err(NetworkError::new("provider_protocol_not_integrated"));
    }
    Ok(())
}

fn request_parts(
    provider: &Provider,
    suffix: &str,
) -> Result<(String, header::HeaderMap), NetworkError> {
    native_protocol(provider)?;
    let url = endpoint(provider, suffix).map_err(|error| NetworkError::new(error.code))?;
    let (headers, _) =
        credential_headers(provider).map_err(|error| NetworkError::new(error.code))?;
    Ok((url, headers))
}

async fn discover(provider: &Provider) -> Result<(Vec<String>, u16), NetworkError> {
    let (url, headers) = request_parts(provider, "models")?;
    let response = client()?
        .get(url)
        .headers(headers.clone())
        .header(header::ACCEPT, "application/json")
        .send()
        .await
        .map_err(transport_failure)?;
    let status = response.status();
    let bytes = response_bytes(response).await?;
    let value: Value = serde_json::from_slice(&bytes)
        .map_err(|_| NetworkError::new("provider_catalog_invalid"))?;
    let items = value
        .get("data")
        .and_then(Value::as_array)
        .filter(|items| items.len() <= CATALOG_ITEMS)
        .ok_or_else(|| NetworkError::new("provider_catalog_invalid"))?;
    let mut models = Vec::with_capacity(items.len());
    for item in items {
        let id = item
            .get("id")
            .and_then(Value::as_str)
            .filter(|id| is_valid_model_id(id))
            .ok_or_else(|| NetworkError::new("provider_catalog_invalid"))?;
        // A successful catalog is still untrusted input. A provider echoing
        // its Authorization value into a model identifier must not publish that
        // credential through the shared catalog or persist it as public metadata.
        let reflected_credential = headers
            .get(header::AUTHORIZATION)
            .and_then(|value| value.to_str().ok())
            .and_then(|value| value.strip_prefix("Bearer "))
            .is_some_and(|secret| id.contains(secret));
        if reflected_credential || models.iter().any(|existing| existing == id) {
            return Err(NetworkError::new("provider_catalog_invalid"));
        }
        models.push(id.to_owned());
    }
    Ok((models, status.as_u16()))
}

async fn test_request(provider: &Provider) -> Result<u16, NetworkError> {
    let model = provider
        .enabled_models()
        .first()
        .copied()
        .ok_or_else(|| NetworkError::new("provider_test_model_required"))?;
    let (suffix, body) = match provider.protocol {
        Protocol::Chat => (
            "chat/completions",
            json!({"model":model,"messages":[{"role":"user","content":"Reply OK."}],"max_tokens":1,"stream":false}),
        ),
        Protocol::Responses => (
            "responses",
            json!({"model":model,"input":"Reply OK.","max_output_tokens":16,"stream":false}),
        ),
        _ => return Err(NetworkError::new("provider_protocol_not_integrated")),
    };
    let (url, headers) = request_parts(provider, suffix)?;
    let response = client()?
        .post(url)
        .headers(headers)
        .header(header::CONTENT_TYPE, "application/json")
        .header(header::ACCEPT, "application/json")
        .body(body.to_string())
        .send()
        .await
        .map_err(transport_failure)?;
    let status = response.status();
    let bytes = response_bytes(response).await?;
    match crate::forward::protocol_response_outcome(
        &bytes,
        provider.protocol == Protocol::Responses,
    ) {
        Some(true) => Ok(status.as_u16()),
        _ => Err(NetworkError {
            code: "provider_test_incomplete_or_invalid",
            status: Some(status.as_u16()),
        }),
    }
}

fn transport_failure(error: reqwest::Error) -> NetworkError {
    NetworkError::new(if error.is_timeout() {
        "provider_timeout"
    } else {
        "provider_transport_error"
    })
}

async fn response_bytes(response: reqwest::Response) -> Result<Vec<u8>, NetworkError> {
    if !response.status().is_success() {
        // Do not read/echo an error body, Location, cookie, or upstream parser text.
        return Err(NetworkError::upstream(response.status()));
    }
    if response.headers().contains_key(header::CONTENT_ENCODING)
        || !response
            .headers()
            .get(header::CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .is_some_and(|value| {
                value
                    .split(';')
                    .next()
                    .is_some_and(|media| media.trim().eq_ignore_ascii_case("application/json"))
            })
    {
        return Err(NetworkError::new("provider_content_type_mismatch"));
    }
    if response
        .content_length()
        .is_some_and(|length| length > CATALOG_BYTES as u64)
    {
        return Err(NetworkError::new("provider_response_too_large"));
    }
    let mut stream = response.bytes_stream();
    let mut bytes = Vec::new();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(transport_failure)?;
        if bytes.len().saturating_add(chunk.len()) > CATALOG_BYTES {
            return Err(NetworkError::new("provider_response_too_large"));
        }
        bytes.extend_from_slice(&chunk);
    }
    Ok(bytes)
}

fn invalid(code: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, code)
}

/// Inspect a running Provider-backed Gateway against an explicit local store.
///
/// This only observes configuration and implemented entry protocols. It does
/// not probe an upstream, reserve a revision, mutate state or authenticate the
/// local process. Legacy inspection retains its separate frozen envelope.
pub fn inspect_gateway(address: SocketAddr, path: &Path) -> Result<String, &'static str> {
    if !address.ip().is_loopback() || address.port() == 0 || !path.is_absolute() {
        return Err("provider_inspect_argument_invalid");
    }
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|_| "provider_gateway_unavailable")?
        .block_on(async {
            tokio::time::timeout(
                Duration::from_secs(5),
                inspect_gateway_async(address, &ProviderStore::new(path)),
            )
            .await
            .map_err(|_| "provider_gateway_inspect_timeout")?
        })
}

async fn inspect_gateway_async(
    address: SocketAddr,
    store: &ProviderStore,
) -> Result<String, &'static str> {
    let client = reqwest::Client::builder()
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .retry(reqwest::retry::never())
        .timeout(Duration::from_secs(3))
        .build()
        .map_err(|_| "provider_gateway_unavailable")?;
    let health = inspect_json(&client, address, "/health", 64 * 1024).await?;
    validate_health(&health)?;
    match health.get("state_source").and_then(Value::as_str) {
        Some("provider_store") => {}
        None if health.get("state_source").is_none()
            && ["store_identity", "state_revision", "state_token"]
                .iter()
                .all(|field| health.get(field).is_none()) =>
        {
            return Err("provider_gateway_unmanaged");
        }
        _ => return Err("provider_gateway_invalid_response"),
    }
    let identity = store.identity().map_err(|error| error.code())?;
    let before = store.load().map_err(|error| error.code())?;
    validate_marker(&health, &identity, &before)?;
    let catalog = inspect_json(&client, address, "/v1/models", MODEL_LIST_BYTES).await?;
    validate_marker(&catalog, &identity, &before)?;
    let models = inspect_catalog(&catalog, &before)?;
    let after = store.load().map_err(|error| error.code())?;
    if after.token != before.token {
        return Err("provider_catalog_changed");
    }
    Ok(json!({"version":magpie_core::VERSION, "provider_gateway":{
        "address":address.to_string(), "mode":"provider_store",
        "store_identity":identity, "revision":before.revision, "token":before.token,
        "provider_health":"not_probed", "models":models
    }})
    .to_string())
}

async fn inspect_json(
    client: &reqwest::Client,
    address: SocketAddr,
    path: &'static str,
    limit: usize,
) -> Result<Value, &'static str> {
    let response = client
        .get(format!("http://{address}{path}"))
        .send()
        .await
        .map_err(|_| "provider_gateway_unavailable")?;
    if response.status() != StatusCode::OK {
        return Err("provider_gateway_unavailable");
    }
    if response.headers().contains_key(header::CONTENT_ENCODING)
        || !response
            .headers()
            .get(header::CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .and_then(|value| value.split(';').next())
            .is_some_and(|value| value.trim().eq_ignore_ascii_case("application/json"))
        || response
            .content_length()
            .is_some_and(|length| length > limit as u64)
    {
        return Err("provider_gateway_invalid_response");
    }
    let mut stream = response.bytes_stream();
    let mut bytes = Vec::new();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|_| "provider_gateway_unavailable")?;
        if bytes.len().saturating_add(chunk.len()) > limit {
            return Err("provider_gateway_invalid_response");
        }
        bytes.extend_from_slice(&chunk);
    }
    let value = serde_json::from_slice(&bytes).map_err(|_| "provider_gateway_invalid_response")?;
    reject_inspection_duplicate_keys(&bytes)?;
    Ok(value)
}

// serde_json first validates syntax/depth. This second pass rejects repeated
// decoded object names instead of silently accepting a last-key-wins marker.
fn reject_inspection_duplicate_keys(bytes: &[u8]) -> Result<(), &'static str> {
    let mut stack: Vec<Option<std::collections::BTreeSet<String>>> = Vec::new();
    let mut index = 0;
    while index < bytes.len() {
        match bytes[index] {
            b'{' => stack.push(Some(std::collections::BTreeSet::new())),
            b'[' => stack.push(None),
            b'}' | b']' => {
                stack.pop();
            }
            b'"' => {
                let start = index;
                index += 1;
                while index < bytes.len() {
                    if bytes[index] == b'\\' {
                        index += 2;
                    } else if bytes[index] == b'"' {
                        break;
                    } else {
                        index += 1;
                    }
                }
                let mut next = index + 1;
                while bytes.get(next).is_some_and(u8::is_ascii_whitespace) {
                    next += 1;
                }
                if bytes.get(next) == Some(&b':') {
                    let key: String = serde_json::from_slice(&bytes[start..=index])
                        .map_err(|_| "provider_gateway_invalid_response")?;
                    if let Some(Some(keys)) = stack.last_mut() {
                        if !keys.insert(key) {
                            return Err("provider_gateway_invalid_response");
                        }
                    }
                }
            }
            _ => {}
        }
        index += 1;
    }
    Ok(())
}

fn validate_health(health: &Value) -> Result<(), &'static str> {
    if health.get("status").and_then(Value::as_str) != Some("ok")
        || health.get("mode").and_then(Value::as_str) != Some("forward_candidate")
        || health.get("provider_health").and_then(Value::as_str) != Some("not_probed")
        || !health.get("limits").is_some_and(|limits| {
            limits.get("active_requests").and_then(Value::as_u64) == Some(4)
                && limits.get("waiting_requests").and_then(Value::as_u64) == Some(0)
                && limits.get("connections").and_then(Value::as_u64) == Some(16)
        })
        || !health.get("transport_counters").is_some_and(|counters| {
            ["active", "completed", "failed", "cancelled"]
                .iter()
                .all(|field| counters.get(field).and_then(Value::as_u64).is_some())
        })
    {
        return Err("provider_gateway_invalid_response");
    }
    Ok(())
}

fn validate_marker(
    value: &Value,
    identity: &str,
    snapshot: &ProviderSnapshot,
) -> Result<(), &'static str> {
    let remote_identity = value
        .get("store_identity")
        .and_then(Value::as_str)
        .filter(|identity| {
            identity
                .strip_prefix("provider-store-v1:")
                .is_some_and(|hash| {
                    hash.len() == 64
                        && hash
                            .bytes()
                            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
                })
        })
        .ok_or("provider_gateway_invalid_response")?;
    let revision = value
        .get("state_revision")
        .and_then(Value::as_u64)
        .filter(|revision| *revision <= i64::MAX as u64)
        .ok_or("provider_gateway_invalid_response")?;
    let token = value
        .get("state_token")
        .and_then(Value::as_str)
        .filter(|token| {
            token.split_once(':').is_some_and(|(number, hash)| {
                number == revision.to_string()
                    && hash.len() == 64
                    && hash
                        .bytes()
                        .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
            })
        })
        .ok_or("provider_gateway_invalid_response")?;
    if value.get("state_source").and_then(Value::as_str) != Some("provider_store") {
        return Err("provider_gateway_invalid_response");
    }
    if remote_identity != identity {
        return Err("provider_store_mismatch");
    }
    if revision != snapshot.revision || token != snapshot.token {
        return Err("provider_catalog_changed");
    }
    Ok(())
}

fn inspect_catalog(
    catalog: &Value,
    snapshot: &ProviderSnapshot,
) -> Result<Vec<Value>, &'static str> {
    if catalog.get("object").and_then(Value::as_str) != Some("list") {
        return Err("provider_gateway_invalid_response");
    }
    let rows = catalog
        .get("data")
        .and_then(Value::as_array)
        .filter(|rows| rows.len() <= CATALOG_ITEMS)
        .ok_or("provider_gateway_invalid_response")?;
    let expected_count: usize = snapshot
        .providers
        .iter()
        .filter(|provider| provider.enabled && !provider.deleted)
        .map(|provider| provider.enabled_models().len())
        .sum();
    if rows.len() != expected_count {
        return Err("provider_gateway_invalid_response");
    }
    let mut seen = std::collections::BTreeSet::new();
    let mut models = Vec::with_capacity(rows.len());
    for row in rows {
        let id = row
            .get("id")
            .and_then(Value::as_str)
            .ok_or("provider_gateway_invalid_response")?;
        let (provider_id, model) = id
            .split_once('/')
            .ok_or("provider_gateway_invalid_response")?;
        if !is_valid_provider_id(provider_id) || !is_valid_model_id(model) || !seen.insert(id) {
            return Err("provider_gateway_invalid_response");
        }
        let provider = snapshot
            .provider(provider_id)
            .filter(|provider| {
                provider.enabled && !provider.deleted && provider.enabled_models().contains(&model)
            })
            .ok_or("provider_gateway_invalid_response")?;
        let metadata = row
            .get("magpie")
            .ok_or("provider_gateway_invalid_response")?;
        if metadata.get("source").and_then(Value::as_str) != Some("configured")
            || metadata.get("provider_health").and_then(Value::as_str) != Some("not_probed")
        {
            return Err("provider_gateway_invalid_response");
        }
        let native = endpoint_list(metadata, "native_endpoints", 1)?;
        let clients = endpoint_list(metadata, "client_endpoints", 4)?;
        if native
            .iter()
            .any(|endpoint| *endpoint != provider.protocol.as_str())
            || native.iter().any(|endpoint| !clients.contains(endpoint))
            || (native.is_empty() && !clients.is_empty())
        {
            return Err("provider_gateway_invalid_response");
        }
        models.push(json!({"id":id,"source":"configured","native_endpoints":native,"client_endpoints":clients}));
    }
    Ok(models)
}

fn endpoint_list<'a>(
    value: &'a Value,
    field: &str,
    maximum: usize,
) -> Result<Vec<&'a str>, &'static str> {
    let values = value
        .get(field)
        .and_then(Value::as_array)
        .filter(|values| values.len() <= maximum)
        .ok_or("provider_gateway_invalid_response")?;
    let mut endpoints = Vec::with_capacity(values.len());
    for value in values {
        let endpoint = value
            .as_str()
            .filter(|endpoint| matches!(*endpoint, "chat" | "responses" | "messages" | "gemini"))
            .ok_or("provider_gateway_invalid_response")?;
        if endpoints.contains(&endpoint) {
            return Err("provider_gateway_invalid_response");
        }
        endpoints.push(endpoint);
    }
    Ok(endpoints)
}
