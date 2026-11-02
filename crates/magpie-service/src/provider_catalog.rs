//! Explicit, bounded model discovery from the selected Provider.
//!
//! Reads only model metadata and replaces the selected Provider's model list
//! after every page has been validated. Failed requests, incomplete pagination
//! and concurrent edits keep the existing configuration intact.

use std::{collections::BTreeSet, fmt, path::Path, time::Duration};

use magpie_core::{
    catalog::valid_model_id,
    provider_store::{MAX_PROVIDER_MODELS, ProviderProtocol, ProviderStore, StoreError},
};
use serde_json::Value;

use crate::forward::source::{provider_headers, provider_models_url, transport_client};

const MAX_PAGE_BYTES: usize = 1024 * 1024;
const MAX_TOTAL_BYTES: usize = 4 * MAX_PAGE_BYTES;
const MAX_PAGES: usize = 8;
const MAX_CURSOR_BYTES: usize = 1024;
const REQUEST_TIMEOUT: Duration = Duration::from_secs(15);

#[derive(Debug, PartialEq, Eq)]
pub enum CatalogError {
    Store(StoreError),
    CredentialUnavailable,
    InvalidEndpoint,
    Transport,
    Timeout,
    HttpStatus(u16),
    RedirectRefused,
    ResponseTooLarge,
    InvalidResponse,
    EmptyCatalog,
    ModelLimit,
    PaginationUnsupported,
    PaginationLimit,
    AsyncContext,
}

impl fmt::Display for CatalogError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Store(error) => error.fmt(formatter),
            Self::HttpStatus(status) => write!(formatter, "model catalog returned HTTP {status}; existing models were retained"),
            _ => formatter.write_str(match self {
                Self::CredentialUnavailable => "the selected Provider's credential is unavailable; existing models were retained",
                Self::InvalidEndpoint => "the Provider model catalog URL is invalid",
                Self::Transport => "the model catalog could not be reached; existing models were retained",
                Self::Timeout => "model catalog refresh exceeded its time limit; existing models were retained",
                Self::RedirectRefused => "model catalog redirects are refused; check the Provider API base URL",
                Self::ResponseTooLarge => "model catalog response exceeds its size limit; existing models were retained",
                Self::InvalidResponse => "model catalog response is invalid; existing models were retained",
                Self::EmptyCatalog => "model catalog contains no usable models; existing models were retained",
                Self::ModelLimit => "model catalog exceeds the 4096-model limit; existing models were retained",
                Self::PaginationUnsupported => "model catalog pagination is incomplete or unsupported; existing models were retained",
                Self::PaginationLimit => "model catalog exceeds the eight-page limit; existing models were retained",
                Self::AsyncContext => "run model catalog refresh on a blocking background thread",
                Self::Store(_) | Self::HttpStatus(_) => unreachable!(),
            }),
        }
    }
}

impl std::error::Error for CatalogError {}

impl From<StoreError> for CatalogError {
    fn from(error: StoreError) -> Self {
        Self::Store(error)
    }
}

/// Refreshes model IDs only after an explicit user action. Call from the CLI or
/// a blocking background thread, outside an existing Tokio runtime. The HTTP
/// exchange is bounded to 15 seconds, eight pages and 4 MiB in total. Credential
/// access uses the same system vault and selected-Provider rules as forwarding.
pub fn refresh_models(
    path: &Path,
    id: &str,
    expected_revision: u64,
) -> Result<ProviderStore, CatalogError> {
    if tokio::runtime::Handle::try_current().is_ok() {
        return Err(CatalogError::AsyncContext);
    }
    let store = ProviderStore::load(path)?;
    if store.revision() != expected_revision {
        return Err(StoreError::Conflict.into());
    }
    let mut provider = store
        .provider(id)
        .cloned()
        .ok_or(StoreError::ProviderNotFound)?;
    let endpoint = provider_models_url(&provider).map_err(|_| CatalogError::InvalidEndpoint)?;
    let headers = match provider_headers(&provider) {
        Ok(headers) => headers,
        Err(_) => {
            if ProviderStore::load(path)?.revision() != expected_revision {
                return Err(StoreError::Conflict.into());
            }
            return Err(CatalogError::CredentialUnavailable);
        }
    };
    // System credential access can involve an interactive unlock. If another
    // client edited the configuration meanwhile, request a fresh user action
    // instead of silently contacting a new address or using a different key.
    if ProviderStore::load(path)?.revision() != expected_revision {
        return Err(StoreError::Conflict.into());
    }
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|_| CatalogError::Transport)?;
    let downloaded = runtime.block_on(async {
        tokio::time::timeout(
            REQUEST_TIMEOUT,
            download_models(&endpoint, provider.protocol, headers),
        )
        .await
        .map_err(|_| CatalogError::Timeout)?
    });
    // A timed-out OS DNS lookup can still occupy Tokio's blocking resolver.
    // Dropping a runtime would wait indefinitely for it; a bounded shutdown
    // keeps that worker from defeating the HTTP deadline at the call boundary.
    runtime.shutdown_timeout(Duration::from_millis(100));
    provider.models = downloaded?;
    // The original revision is still the precondition. A successful download
    // must not overwrite edits made while the network request was in flight.
    ProviderStore::update(path, provider, expected_revision).map_err(Into::into)
}

async fn download_models(
    endpoint: &str,
    protocol: ProviderProtocol,
    headers: hyper::HeaderMap,
) -> Result<Vec<String>, CatalogError> {
    let client = transport_client().map_err(|_| CatalogError::Transport)?;
    let endpoint = reqwest::Url::parse(endpoint).map_err(|_| CatalogError::InvalidEndpoint)?;
    let mut cursor: Option<String> = None;
    let mut seen_cursors = BTreeSet::new();
    let mut seen_models = BTreeSet::new();
    let mut models = Vec::new();
    let mut total_bytes = 0;
    for page_index in 0..MAX_PAGES {
        let mut url = endpoint.clone();
        match protocol {
            ProviderProtocol::AnthropicMessages => {
                url.query_pairs_mut().append_pair("limit", "1000");
                if let Some(cursor) = &cursor {
                    url.query_pairs_mut().append_pair("after_id", cursor);
                }
            }
            ProviderProtocol::Gemini => {
                url.query_pairs_mut().append_pair("pageSize", "1000");
                if let Some(cursor) = &cursor {
                    url.query_pairs_mut().append_pair("pageToken", cursor);
                }
            }
            _ => {}
        }
        let mut response = client
            .get(url)
            .headers(headers.clone())
            .header(hyper::header::ACCEPT, "application/json")
            .timeout(REQUEST_TIMEOUT)
            .send()
            .await
            .map_err(network_error)?;
        if response.status().is_redirection() {
            return Err(CatalogError::RedirectRefused);
        }
        if !response.status().is_success() {
            return Err(CatalogError::HttpStatus(response.status().as_u16()));
        }
        if response
            .content_length()
            .is_some_and(|size| size > MAX_PAGE_BYTES as u64)
        {
            return Err(CatalogError::ResponseTooLarge);
        }
        let mut bytes = Vec::new();
        while let Some(chunk) = response.chunk().await.map_err(network_error)? {
            if bytes.len() + chunk.len() > MAX_PAGE_BYTES
                || total_bytes + chunk.len() > MAX_TOTAL_BYTES
            {
                return Err(CatalogError::ResponseTooLarge);
            }
            total_bytes += chunk.len();
            bytes.extend_from_slice(&chunk);
        }
        let page = parse_page(&bytes, protocol)?;
        for model in page.models {
            if seen_models.insert(model.clone()) {
                if models.len() == MAX_PROVIDER_MODELS {
                    return Err(CatalogError::ModelLimit);
                }
                models.push(model);
            }
        }
        let Some(next) = page.next else {
            return if models.is_empty() {
                Err(CatalogError::EmptyCatalog)
            } else {
                Ok(models)
            };
        };
        if !seen_cursors.insert(next.clone()) {
            return Err(CatalogError::PaginationUnsupported);
        }
        if page_index + 1 == MAX_PAGES {
            return Err(CatalogError::PaginationLimit);
        }
        cursor = Some(next);
    }
    Err(CatalogError::PaginationLimit)
}

fn network_error(error: reqwest::Error) -> CatalogError {
    if error.is_timeout() {
        CatalogError::Timeout
    } else {
        CatalogError::Transport
    }
}

struct Page {
    models: Vec<String>,
    next: Option<String>,
}

fn parse_page(bytes: &[u8], protocol: ProviderProtocol) -> Result<Page, CatalogError> {
    let document: Value =
        serde_json::from_slice(bytes).map_err(|_| CatalogError::InvalidResponse)?;
    let object = document.as_object().ok_or(CatalogError::InvalidResponse)?;
    if object.get("error").is_some_and(|error| !error.is_null()) {
        return Err(CatalogError::InvalidResponse);
    }
    let gemini = protocol == ProviderProtocol::Gemini;
    let entries = object
        .get(if gemini { "models" } else { "data" })
        .and_then(Value::as_array)
        .ok_or(CatalogError::InvalidResponse)?;
    if entries.len() > MAX_PROVIDER_MODELS {
        return Err(CatalogError::ModelLimit);
    }
    let mut models = Vec::new();
    for entry in entries {
        let id = entry
            .get(if gemini { "name" } else { "id" })
            .and_then(Value::as_str)
            .ok_or(CatalogError::InvalidResponse)?;
        let id = if gemini {
            id.strip_prefix("models/")
                .ok_or(CatalogError::InvalidResponse)?
        } else {
            id
        };
        if !valid_model_id(id) {
            return Err(CatalogError::InvalidResponse);
        }
        if gemini {
            if let Some(methods) = entry.get("supportedGenerationMethods") {
                let methods = methods.as_array().ok_or(CatalogError::InvalidResponse)?;
                if methods.iter().any(|method| !method.is_string()) {
                    return Err(CatalogError::InvalidResponse);
                }
                if !methods
                    .iter()
                    .any(|method| method.as_str() == Some("generateContent"))
                {
                    continue;
                }
            }
        }
        models.push(id.to_owned());
    }
    let next = match protocol {
        ProviderProtocol::AnthropicMessages => {
            match object.get("has_more").and_then(Value::as_bool) {
                Some(true) => {
                    let last_id = cursor_value(object.get("last_id"))?
                        .ok_or(CatalogError::PaginationUnsupported)?;
                    if entries
                        .last()
                        .and_then(|entry| entry.get("id"))
                        .and_then(Value::as_str)
                        != Some(last_id.as_str())
                    {
                        return Err(CatalogError::PaginationUnsupported);
                    }
                    Some(last_id)
                }
                Some(false) => None,
                None => return Err(CatalogError::InvalidResponse),
            }
        }
        ProviderProtocol::Gemini => cursor_value(object.get("nextPageToken"))?,
        _ => {
            let unknown_page = ["next", "next_cursor", "next_page_token", "nextPageToken"]
                .iter()
                .any(|key| {
                    object
                        .get(*key)
                        .is_some_and(|value| !value.is_null() && value.as_str() != Some(""))
                });
            if unknown_page
                || object
                    .get("has_more")
                    .is_some_and(|value| value.as_bool() != Some(false))
            {
                return Err(CatalogError::PaginationUnsupported);
            }
            None
        }
    };
    if next.is_some() && entries.is_empty() {
        return Err(CatalogError::PaginationUnsupported);
    }
    Ok(Page { models, next })
}

fn cursor_value(value: Option<&Value>) -> Result<Option<String>, CatalogError> {
    match value {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(value)) if value.is_empty() => Ok(None),
        Some(Value::String(value))
            if value.len() <= MAX_CURSOR_BYTES && !value.chars().any(char::is_control) =>
        {
            Ok(Some(value.clone()))
        }
        _ => Err(CatalogError::PaginationUnsupported),
    }
}
