//! Resolves the selected Provider from the same bounded store as CLI and GUI.

use super::{
    ACTIVE_LIMIT, BODY_TIMEOUT, CATALOG_RESPONSE_LIMIT, ForwardConfig, IDLE_TIMEOUT, Protocol,
    invalid,
};
use hyper::{StatusCode, header};
use magpie_core::provider_store::{Provider, ProviderProtocol, ProviderStore};
use serde_json::json;
use std::{io, net::SocketAddr, path::PathBuf, sync::Arc, time::Duration};
use tokio::{sync::Semaphore, time::timeout};

pub(super) enum ForwardSource {
    Legacy {
        config: ForwardConfig,
        headers: header::HeaderMap,
    },
    Store {
        path: PathBuf,
        readers: Arc<Semaphore>,
    },
}

pub(super) struct Target {
    pub provider_id: String,
    pub model: String,
    pub protocol: Option<Protocol>,
    pub headers: header::HeaderMap,
    base_url: String,
    legacy: bool,
}

#[derive(Debug)]
pub(super) struct SourceError {
    pub status: StatusCode,
    pub code: &'static str,
}

impl SourceError {
    fn unavailable() -> Self {
        Self {
            status: StatusCode::SERVICE_UNAVAILABLE,
            code: "provider_configuration_unavailable",
        }
    }

    fn unknown_model() -> Self {
        Self {
            status: StatusCode::NOT_FOUND,
            code: "model_not_found",
        }
    }
}

impl ForwardSource {
    pub fn legacy(config: ForwardConfig) -> Self {
        Self::Legacy {
            config,
            headers: header::HeaderMap::new(),
        }
    }

    pub fn store(path: PathBuf) -> io::Result<Self> {
        ProviderStore::load(&path).map_err(|_| invalid("provider_store_unavailable"))?;
        Ok(Self::Store {
            path,
            readers: Arc::new(Semaphore::new(ACTIVE_LIMIT)),
        })
    }

    pub fn initialize(self, address: SocketAddr) -> io::Result<Self> {
        match self {
            Self::Legacy { config, .. } => {
                if address == config.upstream {
                    return Err(invalid("forward_config_self_target"));
                }
                let secret = config
                    .api_key_env
                    .as_deref()
                    .map(read_environment_key)
                    .transpose()?;
                if secret.as_ref().is_some_and(|secret| secret.len() > 4096) {
                    return Err(invalid("provider_credential_invalid"));
                }
                let headers = credential_headers(secret, config.upstream_protocol)?;
                Ok(Self::Legacy { config, headers })
            }
            store => Ok(store),
        }
    }

    pub fn typed_errors(&self) -> bool {
        match self {
            Self::Legacy { config, .. } => config.upstream_protocol.is_some(),
            Self::Store { .. } => true,
        }
    }

    pub async fn models(&self) -> Result<Vec<u8>, SourceError> {
        match self {
            Self::Legacy { config, .. } => {
                serde_json::to_vec(&config.models_json()).map_err(|_| SourceError::unavailable())
            }
            Self::Store { path, readers } => {
                with_store(path, readers, |store| serialize_store_catalog(&store)).await
            }
        }
    }

    pub async fn target(
        &self,
        public_model: &str,
        address: SocketAddr,
    ) -> Result<Target, SourceError> {
        match self {
            Self::Legacy { config, headers } => {
                let model = public_model
                    .strip_prefix(&format!("{}/", config.provider_id))
                    .filter(|model| config.models.iter().any(|known| known == model))
                    .ok_or_else(SourceError::unknown_model)?;
                Ok(Target {
                    provider_id: config.provider_id.clone(),
                    model: model.to_owned(),
                    protocol: config.upstream_protocol,
                    headers: headers.clone(),
                    base_url: format!("http://{}/v1", config.upstream),
                    legacy: true,
                })
            }
            Self::Store { path, readers } => {
                let public_model = public_model.to_owned();
                let reload_path = path.clone();
                with_store(path, readers, move |store| {
                    resolve_stored_target(
                        store,
                        &reload_path,
                        &public_model,
                        address,
                        provider_secret,
                    )
                })
                .await
            }
        }
    }
}

fn serialize_store_catalog(store: &ProviderStore) -> Result<Vec<u8>, SourceError> {
    let mut output = br#"{"object":"list","data":["#.to_vec();
    let mut first = true;
    for provider in store.providers().iter().filter(|provider| provider.enabled) {
        for model in &provider.models {
            // Serialize only one bounded row at a time. Building an expanded
            // Value tree and encoding it twice multiplies a large catalog's
            // memory use without adding information.
            let row = serde_json::to_vec(&json!({
                "id": format!("{}/{model}", provider.id),
                "object": "model", "created": 0,
                "owned_by": provider.id, "name": model,
                "provider_name": provider.name,
                "magpie": {"source": "configured", "provider_health": "not_probed",
                    "native_protocol": provider.protocol.as_str()}
            }))
            .map_err(|_| SourceError::unavailable())?;
            let needed = output.len() + row.len() + usize::from(!first) + 2;
            if needed > CATALOG_RESPONSE_LIMIT {
                return Err(SourceError::unavailable());
            }
            if needed > output.capacity() {
                let capacity = needed.next_power_of_two().min(CATALOG_RESPONSE_LIMIT);
                output
                    .try_reserve_exact(capacity - output.len())
                    .map_err(|_| SourceError::unavailable())?;
            }
            if !first {
                output.push(b',');
            }
            output.extend_from_slice(&row);
            first = false;
        }
    }
    output.extend_from_slice(b"]}");
    Ok(output)
}

fn resolve_stored_target(
    mut store: ProviderStore,
    path: &std::path::Path,
    public_model: &str,
    address: SocketAddr,
    mut load_secret: impl FnMut(&Provider) -> io::Result<Option<String>>,
) -> Result<Target, SourceError> {
    let (id, model) = public_model
        .split_once('/')
        .ok_or_else(SourceError::unknown_model)?;
    for attempt in 0..2 {
        let provider = store
            .provider(id)
            .filter(|provider| {
                provider.enabled && provider.models.iter().any(|known| known == model)
            })
            .ok_or_else(SourceError::unknown_model)?;
        let base_url = checked_base_url(&provider.base_url, Some(address))
            .map_err(|_| SourceError::unavailable())?;
        let protocol = provider_protocol(provider.protocol);
        match load_secret(provider) {
            Ok(secret) => {
                let headers = credential_headers(secret, Some(protocol))
                    .map_err(|_| SourceError::unavailable())?;
                return Ok(Target {
                    provider_id: provider.id.clone(),
                    model: model.to_owned(),
                    protocol: Some(protocol),
                    headers,
                    base_url,
                    legacy: false,
                });
            }
            Err(_) if attempt == 0 => {
                // A key rotation commits a new reference before deleting the
                // old key. If we observed the old snapshot just before that
                // deletion, reselect once from the new configuration. This is
                // not a retry of an upstream request and never invents a keyless
                // fallback for an unchanged, missing or locked credential.
                let current = ProviderStore::load(path).map_err(|_| SourceError::unavailable())?;
                if current.revision() == store.revision() && current.provider(id) == Some(provider)
                {
                    return Err(SourceError::unavailable());
                }
                store = current;
            }
            Err(_) => return Err(SourceError::unavailable()),
        }
    }
    Err(SourceError::unavailable())
}

async fn with_store<T: Send + 'static>(
    path: &std::path::Path,
    readers: &Arc<Semaphore>,
    read: impl FnOnce(ProviderStore) -> Result<T, SourceError> + Send + 'static,
) -> Result<T, SourceError> {
    let permit = readers
        .clone()
        .try_acquire_owned()
        .map_err(|_| SourceError::unavailable())?;
    let path = path.to_owned();
    let worker = tokio::task::spawn_blocking(move || {
        // If the caller times out or disconnects, a blocked keyring operation
        // retains its permit until it exits. New requests cannot accumulate an
        // unbounded background queue behind an unavailable credential service.
        let _permit = permit;
        let store = ProviderStore::load(&path).map_err(|_| SourceError::unavailable())?;
        read(store)
    });
    timeout(BODY_TIMEOUT, worker)
        .await
        .map_err(|_| SourceError::unavailable())?
        .map_err(|_| SourceError::unavailable())?
}

fn provider_protocol(protocol: ProviderProtocol) -> Protocol {
    match protocol {
        ProviderProtocol::OpenAiChat => Protocol::OpenAiChat,
        ProviderProtocol::OpenAiResponses => Protocol::OpenAiResponses,
        ProviderProtocol::AnthropicMessages => Protocol::AnthropicMessages,
        ProviderProtocol::Gemini => Protocol::Gemini,
    }
}

fn provider_secret(provider: &Provider) -> io::Result<Option<String>> {
    if let Some(reference) = provider.credential_ref.as_deref() {
        return magpie_core::credentials::read(reference).map(Some);
    }
    provider
        .api_key_env
        .as_deref()
        .map(read_environment_key)
        .transpose()
}

/// Selected Provider credentials only. Call outside the async reactor/UI thread.
pub(crate) fn provider_headers(provider: &Provider) -> io::Result<header::HeaderMap> {
    credential_headers(
        provider_secret(provider)?,
        Some(provider_protocol(provider.protocol)),
    )
}

/// Shared transport policy for forwarding and explicit catalog refreshes.
pub(crate) fn transport_client() -> io::Result<reqwest::Client> {
    reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .no_proxy()
        .retry(reqwest::retry::never())
        .connect_timeout(Duration::from_secs(3))
        .read_timeout(IDLE_TIMEOUT)
        .pool_max_idle_per_host(ACTIVE_LIMIT)
        .pool_idle_timeout(Duration::from_secs(30))
        .http1_only()
        .build()
        .map_err(|_| invalid("provider_transport_initialization_failed"))
}

pub(crate) fn provider_models_url(provider: &Provider) -> io::Result<String> {
    let base = checked_base_url(&provider.base_url, None)?;
    Ok(format!(
        "{}/models",
        api_base_url(&base, provider_protocol(provider.protocol))?
    ))
}

fn read_environment_key(name: &str) -> io::Result<String> {
    std::env::var(name).map_err(|_| invalid("provider_credential_unavailable"))
}

fn credential_headers(
    secret: Option<String>,
    protocol: Option<Protocol>,
) -> io::Result<header::HeaderMap> {
    let mut headers = header::HeaderMap::new();
    if let Some(secret) = secret {
        if secret.is_empty() || secret.len() > 8192 {
            return Err(invalid("provider_credential_invalid"));
        }
        let (name, secret) = match protocol {
            Some(Protocol::AnthropicMessages) => ("x-api-key", secret),
            Some(Protocol::Gemini) => ("x-goog-api-key", secret),
            _ => ("authorization", format!("Bearer {secret}")),
        };
        let mut value = header::HeaderValue::from_str(&secret)
            .map_err(|_| invalid("provider_credential_invalid"))?;
        value.set_sensitive(true);
        headers.insert(header::HeaderName::from_static(name), value);
    }
    if protocol == Some(Protocol::AnthropicMessages) {
        headers.insert(
            "anthropic-version",
            header::HeaderValue::from_static("2023-06-01"),
        );
    }
    Ok(headers)
}

fn checked_base_url(base: &str, address: Option<SocketAddr>) -> io::Result<String> {
    let mut url = reqwest::Url::parse(base).map_err(|_| invalid("provider_url_invalid"))?;
    if url.query().is_some()
        || url.fragment().is_some()
        || !url.username().is_empty()
        || url.password().is_some()
    {
        return Err(invalid("provider_url_invalid"));
    }
    // The only HTTP hostname accepted by the store has a fixed local meaning;
    // do not ask DNS, where a rebind could redirect its credential off machine.
    if url.scheme() == "http" && url.host_str() == Some("localhost") {
        url.set_host(Some("127.0.0.1"))
            .map_err(|_| invalid("provider_url_invalid"))?;
    }
    let host = url
        .host_str()
        .ok_or_else(|| invalid("provider_url_invalid"))?;
    let ip = host
        .trim_matches(['[', ']'])
        .parse::<std::net::IpAddr>()
        .ok();
    if url.scheme() != "https" && !(url.scheme() == "http" && ip.is_some_and(|ip| ip.is_loopback()))
    {
        return Err(invalid("provider_url_requires_tls"));
    }
    if ip.is_some_and(|ip| ip.is_loopback())
        && address.is_some_and(|address| url.port_or_known_default() == Some(address.port()))
    {
        return Err(invalid("forward_config_self_target"));
    }
    Ok(url.to_string().trim_end_matches('/').to_owned())
}

impl Target {
    pub fn url(&self, protocol: Protocol, stream: bool) -> io::Result<String> {
        if self.legacy {
            // Preserve the original four fixed paths for --forward fixtures.
            let origin = self.base_url.trim_end_matches("/v1");
            return endpoint_url(origin, protocol, &self.model, stream);
        }
        endpoint_url(&self.base_url, protocol, &self.model, stream)
    }
}

fn api_base_url(base: &str, protocol: Protocol) -> io::Result<String> {
    let base = base.trim_end_matches('/');
    let url = reqwest::Url::parse(base).map_err(|_| invalid("provider_url_invalid"))?;
    let path = url.path();
    let version = match protocol {
        Protocol::OpenAiChat | Protocol::OpenAiResponses => {
            if path.is_empty() || path == "/" {
                "/v1"
            } else {
                ""
            }
        }
        Protocol::AnthropicMessages => {
            if path.ends_with("/v1") {
                ""
            } else {
                "/v1"
            }
        }
        Protocol::Gemini => {
            if path.ends_with("/v1beta") || path.ends_with("/v1") {
                ""
            } else {
                "/v1beta"
            }
        }
    };
    Ok(format!("{base}{version}"))
}

fn endpoint_url(base: &str, protocol: Protocol, model: &str, stream: bool) -> io::Result<String> {
    let base = api_base_url(base, protocol)?;
    Ok(match protocol {
        Protocol::OpenAiChat => format!("{base}/chat/completions"),
        Protocol::OpenAiResponses => format!("{base}/responses"),
        Protocol::AnthropicMessages => format!("{base}/messages"),
        Protocol::Gemini => {
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
            format!("{base}/models/{encoded}:{action}")
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    struct StoredFixture(std::path::PathBuf);

    #[cfg(unix)]
    impl StoredFixture {
        fn new() -> Self {
            use std::os::unix::fs::DirBuilderExt;
            use std::sync::atomic::{AtomicUsize, Ordering};
            static NEXT: AtomicUsize = AtomicUsize::new(0);
            let directory = std::env::temp_dir().canonicalize().unwrap().join(format!(
                "magpie-forward-vault-{}-{}-{}",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_nanos(),
                NEXT.fetch_add(1, Ordering::Relaxed),
            ));
            std::fs::DirBuilder::new()
                .mode(0o700)
                .create(&directory)
                .unwrap();
            let path = directory.join("providers.toml");
            ProviderStore::create(
                &path,
                Provider {
                    id: "fixture".into(),
                    name: "Synthetic fixture".into(),
                    base_url: "http://127.0.0.1:18421/v1".into(),
                    protocol: ProviderProtocol::OpenAiChat,
                    credential_ref: Some(
                        "provider/fixture/00000000000000000000000000000000".into(),
                    ),
                    api_key_env: None,
                    models: vec!["model".into()],
                    enabled: true,
                },
                0,
            )
            .unwrap();
            Self(path)
        }
    }

    #[cfg(unix)]
    impl Drop for StoredFixture {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(self.0.parent().unwrap());
        }
    }

    #[cfg(unix)]
    #[test]
    fn maximum_catalog_with_escaped_names_is_complete_and_within_its_encoded_bound() {
        use magpie_core::provider_store::{MAX_PROVIDER_MODELS, MAX_TOTAL_MODELS};
        let fixture = StoredFixture::new();
        let path = fixture.0.parent().unwrap().join("large-catalog.toml");
        let models: Vec<String> = (0..MAX_PROVIDER_MODELS)
            .map(|index| format!("{index:05}{}", "m".repeat(45)))
            .collect();
        assert!(models.iter().all(|model| model.len() == 50));
        let mut store = ProviderStore::load(&path).unwrap();
        for index in 0..MAX_TOTAL_MODELS / MAX_PROVIDER_MODELS {
            let revision = store.revision();
            store = ProviderStore::create(
                &path,
                Provider {
                    id: format!("p{index}{}", "x".repeat(126)),
                    name: "\"\\".repeat(128),
                    base_url: "https://example.test/v1".into(),
                    protocol: ProviderProtocol::AnthropicMessages,
                    api_key_env: None,
                    credential_ref: None,
                    models: models.clone(),
                    enabled: true,
                },
                revision,
            )
            .unwrap();
        }
        assert!(std::fs::metadata(&path).unwrap().len() < 1024 * 1024);
        let encoded = serialize_store_catalog(&store).unwrap();
        assert!(encoded.len() > 16 * 1024 * 1024);
        assert!(encoded.len() <= CATALOG_RESPONSE_LIMIT);
        assert!(encoded.capacity() <= CATALOG_RESPONSE_LIMIT);
        let parsed: serde_json::Value = serde_json::from_slice(&encoded).unwrap();
        let rows = parsed["data"].as_array().unwrap();
        assert_eq!(rows.len(), MAX_TOTAL_MODELS);
        assert_eq!(rows[0]["provider_name"], "\"\\".repeat(128));
        // Even ignoring the file-size bound, increasing both copies of every
        // model ID from 50 to its maximum 256 bytes stays below 32 MiB. IDs are
        // validated ASCII, and a 256-byte display name expands to at most 512.
        let maximum_row = serde_json::to_vec(&rows[0]).unwrap().len() + 2 * (256 - 50);
        assert!((maximum_row + 1) * MAX_TOTAL_MODELS + 26 < CATALOG_RESPONSE_LIMIT);
    }

    #[cfg(unix)]
    #[test]
    fn removed_old_key_reselects_the_committed_replacement_once() {
        let fixture = StoredFixture::new();
        let store = ProviderStore::load(&fixture.0).unwrap();
        let mut reads = 0;
        let target = resolve_stored_target(
            store,
            &fixture.0,
            "fixture/model",
            "127.0.0.1:3425".parse().unwrap(),
            |provider| {
                reads += 1;
                if reads == 1 {
                    let mut updated = provider.clone();
                    updated.credential_ref =
                        Some("provider/fixture/11111111111111111111111111111111".into());
                    ProviderStore::update(&fixture.0, updated, 1).unwrap();
                    return Err(invalid("synthetic_old_key_removed"));
                }
                assert_eq!(
                    provider.credential_ref.as_deref(),
                    Some("provider/fixture/11111111111111111111111111111111")
                );
                Ok(Some("synthetic-new-key".into()))
            },
        )
        .unwrap();
        assert_eq!(reads, 2);
        assert_eq!(target.headers["authorization"], "Bearer synthetic-new-key");
    }

    #[cfg(unix)]
    #[test]
    fn a_concurrent_disable_stops_reselection_before_reading_another_key() {
        let fixture = StoredFixture::new();
        let store = ProviderStore::load(&fixture.0).unwrap();
        let mut reads = 0;
        let failure = resolve_stored_target(
            store,
            &fixture.0,
            "fixture/model",
            "127.0.0.1:3425".parse().unwrap(),
            |_| {
                reads += 1;
                ProviderStore::set_enabled(&fixture.0, "fixture", false, 1).unwrap();
                Err(invalid("synthetic_old_key_removed"))
            },
        )
        .err()
        .unwrap();
        assert_eq!(reads, 1);
        assert_eq!(failure.status, StatusCode::NOT_FOUND);
    }

    #[cfg(unix)]
    #[test]
    fn stable_missing_key_fails_without_retries_or_keyless_fallback() {
        let fixture = StoredFixture::new();
        let store = ProviderStore::load(&fixture.0).unwrap();
        let mut reads = 0;
        let failure = resolve_stored_target(
            store,
            &fixture.0,
            "fixture/model",
            "127.0.0.1:3425".parse().unwrap(),
            |_| {
                reads += 1;
                Err(invalid("synthetic_locked_keyring"))
            },
        )
        .err()
        .unwrap();
        assert_eq!(reads, 1);
        assert_eq!(failure.status, StatusCode::SERVICE_UNAVAILABLE);
    }

    #[test]
    fn endpoint_roots_preserve_tls_proxy_prefixes_and_gemini_model_boundaries() {
        assert_eq!(
            endpoint_url("https://example.test", Protocol::OpenAiChat, "model", false).unwrap(),
            "https://example.test/v1/chat/completions"
        );
        assert_eq!(
            endpoint_url(
                "https://example.test/proxy/v1/",
                Protocol::OpenAiResponses,
                "model",
                false
            )
            .unwrap(),
            "https://example.test/proxy/v1/responses"
        );
        assert_eq!(
            endpoint_url(
                "https://example.test/anthropic",
                Protocol::AnthropicMessages,
                "model",
                false
            )
            .unwrap(),
            "https://example.test/anthropic/v1/messages"
        );
        assert_eq!(
            endpoint_url(
                "https://example.test/v1",
                Protocol::AnthropicMessages,
                "model",
                false
            )
            .unwrap(),
            "https://example.test/v1/messages"
        );
        assert_eq!(
            endpoint_url(
                "https://example.test/proxy/v1beta",
                Protocol::Gemini,
                "namespace/model:trial",
                true
            )
            .unwrap(),
            "https://example.test/proxy/v1beta/models/namespace%2Fmodel%3Atrial:streamGenerateContent?alt=sse"
        );
    }

    #[test]
    fn plaintext_is_literal_loopback_and_localhost_never_uses_dns() {
        let gateway = "127.0.0.1:3425".parse().unwrap();
        assert_eq!(
            checked_base_url("http://localhost:18421/v1", Some(gateway)).unwrap(),
            "http://127.0.0.1:18421/v1"
        );
        assert_eq!(
            checked_base_url("https://example.test/v1", Some(gateway)).unwrap(),
            "https://example.test/v1"
        );
        for url in [
            "http://example.test/v1",
            "http://192.168.1.10/v1",
            "file:///tmp/fixture",
            "http://127.0.0.1:3425/v1",
            "https://fixture:private@example.test/v1",
            "https://example.test/v1?key=fixture",
            "https://example.test/v1#fixture",
        ] {
            assert!(checked_base_url(url, Some(gateway)).is_err(), "{url}");
        }
    }

    #[test]
    fn credential_headers_are_sensitive_and_do_not_accept_header_injection() {
        let headers = credential_headers(
            Some("fixture-secret".into()),
            Some(Protocol::AnthropicMessages),
        )
        .unwrap();
        assert!(headers["x-api-key"].is_sensitive());
        assert!(!headers.contains_key("authorization"));
        assert_eq!(headers["anthropic-version"], "2023-06-01");
        for key in ["", "fixture\r\nx-injected: true"] {
            let error = credential_headers(Some(key.into()), None).unwrap_err();
            assert_eq!(error.to_string(), "provider_credential_invalid");
            assert!(!format!("{error:?}").contains(key) || key.is_empty());
        }
    }
}
