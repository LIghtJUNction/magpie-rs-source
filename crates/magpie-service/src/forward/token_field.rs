//! One bounded Chat token-field retry, before any response reaches the caller.
//!
//! The vendor's diagnostic is data: it may select the alternate known field,
//! never a URL, credential, arbitrary rewrite, or an unbounded retry policy.

use super::{IDLE_TIMEOUT, Protocol, REQUEST_LIMIT};
use bytes::Bytes;
use hyper::{StatusCode, header};
use serde_json::Value;
use tokio::time::{Instant, timeout_at};

const ERROR_LIMIT: usize = 16 * 1024;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum Field {
    Tokens,
    CompletionTokens,
}

impl Field {
    pub(super) fn in_request(protocol: Protocol, value: &Value) -> Option<Self> {
        if protocol != Protocol::OpenAiChat {
            return None;
        }
        match (
            value.get("max_tokens").is_some(),
            value.get("max_completion_tokens").is_some(),
        ) {
            (true, false) => Some(Self::Tokens),
            (false, true) => Some(Self::CompletionTokens),
            // A conflicting pair is the caller's error; never choose a value.
            _ => None,
        }
    }

    fn name(self) -> &'static str {
        match self {
            Self::Tokens => "max_tokens",
            Self::CompletionTokens => "max_completion_tokens",
        }
    }

    fn other(self) -> Self {
        match self {
            Self::Tokens => Self::CompletionTokens,
            Self::CompletionTokens => Self::Tokens,
        }
    }
}

pub(super) struct Payload {
    bytes: Bytes,
    field: Option<Field>,
}

impl Payload {
    /// `bytes` are the already validated, serialized JSON request. Retrying
    /// changes just its top-level key; all other bytes remain identical.
    pub(super) fn new(bytes: Vec<u8>, field: Option<Field>, url: &str) -> Self {
        let mut payload = Self {
            bytes: bytes.into(),
            field,
        };
        if field == Some(Field::Tokens)
            && openai_host(url)
            && let Some(bytes) = rename_key(&payload.bytes, "max_tokens", "max_completion_tokens")
        {
            payload.bytes = bytes;
            payload.field = Some(Field::CompletionTokens);
        }
        payload
    }

    pub(super) fn len(&self) -> usize {
        self.bytes.len()
    }

    fn rename(&mut self, wanted: Field) -> bool {
        let Some(current) = self.field else {
            return false;
        };
        if current == wanted {
            return false;
        }
        let Some(bytes) = rename_key(&self.bytes, current.name(), wanted.name()) else {
            return false;
        };
        if bytes.len() > REQUEST_LIMIT {
            return false;
        }
        self.bytes = bytes;
        self.field = Some(wanted);
        true
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum SendFailure {
    Timeout,
    Network,
}

pub(super) async fn send(
    client: &reqwest::Client,
    url: &str,
    headers: &header::HeaderMap,
    mut payload: Payload,
    stream: bool,
    deadline: Instant,
) -> Result<reqwest::Response, SendFailure> {
    let mut retried = false;
    loop {
        if Instant::now() >= deadline {
            return Err(SendFailure::Timeout);
        }
        let request = client
            .post(url)
            .headers(headers.clone())
            .header(header::CONTENT_TYPE, "application/json")
            .header(
                header::ACCEPT,
                if stream {
                    "text/event-stream"
                } else {
                    "application/json"
                },
            )
            .body(payload.bytes.clone());
        let limit = deadline.min(Instant::now() + IDLE_TIMEOUT);
        let mut response = timeout_at(limit, request.send())
            .await
            .map_err(|_| SendFailure::Timeout)?
            .map_err(|failure| {
                if failure.is_timeout() {
                    SendFailure::Timeout
                } else {
                    SendFailure::Network
                }
            })?;
        if !retried
            && response.status() == StatusCode::BAD_REQUEST
            && let Some(current) = payload.field
            && let Some(body) = diagnostic(&mut response, deadline).await
            && wanted(&body, current)
            && payload.rename(current.other())
        {
            // The first response is dropped here. The next attempt uses the
            // same destination, headers, permit and overall deadline.
            retried = true;
            continue;
        }
        return Ok(response);
    }
}

async fn diagnostic(response: &mut reqwest::Response, deadline: Instant) -> Option<Vec<u8>> {
    if response.headers().contains_key(header::CONTENT_ENCODING)
        || response
            .content_length()
            .is_some_and(|length| length > ERROR_LIMIT as u64)
    {
        return None;
    }
    let mut body = Vec::new();
    loop {
        let limit = deadline.min(Instant::now() + IDLE_TIMEOUT);
        let chunk = timeout_at(limit, response.chunk()).await.ok()?.ok()?;
        let Some(chunk) = chunk else {
            return Some(body);
        };
        if chunk.len() > ERROR_LIMIT.saturating_sub(body.len()) {
            return None;
        }
        body.extend_from_slice(&chunk);
    }
}

fn openai_host(url: &str) -> bool {
    let Ok(url) = reqwest::Url::parse(url) else {
        return false;
    };
    let Some(host) = url.host_str() else {
        return false;
    };
    // A single final DNS root dot doesn't change the host's ownership.
    let host = host.strip_suffix('.').unwrap_or(host);
    host == "openai.com" || host.ends_with(".openai.com")
}

fn wanted(body: &[u8], current: Field) -> bool {
    let Ok(value) = serde_json::from_slice::<Value>(body) else {
        return false;
    };
    let Some(error) = value.get("error").and_then(Value::as_object) else {
        return false;
    };
    if error
        .get("param")
        .is_some_and(|param| !param.is_null() && param.as_str() != Some(current.name()))
    {
        return false;
    }
    let Some(message) = error.get("message").and_then(Value::as_str) else {
        return false;
    };
    let lower = message.to_ascii_lowercase();
    let words: Vec<_> = lower
        .split(|c: char| !c.is_ascii_alphanumeric() && c != '_')
        .filter(|word| !word.is_empty())
        .collect();
    let wanted = current.other().name();
    let mut instruction = None;
    for (index, pair) in words.windows(2).enumerate() {
        if pair[0] != "use" || ![current.name(), wanted].contains(&pair[1]) {
            continue;
        }
        if pair[1] != wanted
            || instruction.is_some()
            || words[index.saturating_sub(2)..index]
                .iter()
                .any(|word| ["not", "never", "t"].contains(word))
            || words
                .get(index + 2)
                .is_some_and(|word| ["or", "and"].contains(word))
        {
            return false;
        }
        instruction = Some(index);
    }
    let Some(instruction) = instruction else {
        return false;
    };
    if words[..instruction]
        .iter()
        .any(|word| ["value", "values", "limit", "range", "exceeds"].contains(word))
    {
        return false;
    }
    // A numeric limit error mentioning both fields is not a refusal of the
    // field itself. Require an explicit unsupported/unknown diagnosis tied to
    // the field actually sent, before the instruction to use its alternative.
    words[..instruction]
        .iter()
        .enumerate()
        .any(|(index, word)| {
            if *word != current.name() {
                return false;
            }
            let before = &words[index.saturating_sub(4)..index];
            let after = &words[index + 1..(index + 6).min(instruction)];
            before.iter().any(|word| {
                ["unsupported", "unknown", "unrecognized", "unrecognised"].contains(word)
            }) || after.windows(2).any(|pair| {
                pair[0] == "not"
                    && [
                        "supported",
                        "allowed",
                        "recognized",
                        "recognised",
                        "accepted",
                    ]
                    .contains(&pair[1])
            })
        })
}

/// Locate a key only at object depth one in valid serialized JSON. Scanning
/// quoted strings prevents nested keys or text containing a key from matching.
fn rename_key(bytes: &[u8], old: &str, new: &str) -> Option<Bytes> {
    let size = bytes.len().checked_sub(old.len())?.checked_add(new.len())?;
    let mut depth = 0_usize;
    let mut cursor = 0;
    while cursor < bytes.len() {
        match bytes[cursor] {
            b'{' | b'[' => depth += 1,
            b'}' | b']' => depth = depth.checked_sub(1)?,
            b'"' => {
                let start = cursor + 1;
                cursor = start;
                while cursor < bytes.len() && bytes[cursor] != b'"' {
                    if bytes[cursor] == b'\\' {
                        cursor += 1;
                    }
                    cursor += 1;
                }
                if cursor >= bytes.len() {
                    return None;
                }
                if depth == 1
                    && &bytes[start..cursor] == old.as_bytes()
                    && bytes.get(cursor + 1) == Some(&b':')
                {
                    let mut renamed = Vec::with_capacity(size);
                    renamed.extend_from_slice(&bytes[..start]);
                    renamed.extend_from_slice(new.as_bytes());
                    renamed.extend_from_slice(&bytes[cursor..]);
                    return Some(renamed.into());
                }
            }
            _ => {}
        }
        cursor += 1;
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn openai_policy_matches_the_actual_host_and_only_chat() {
        for url in [
            "https://openai.com/v1/chat/completions",
            "https://api.openai.com/v1/chat/completions",
            "https://API.OPENAI.COM/v1/chat/completions",
            "https://api.openai.com./v1/chat/completions",
        ] {
            assert!(openai_host(url), "{url}");
            let value = json!({"max_tokens":64});
            let payload = Payload::new(
                serde_json::to_vec(&value).unwrap(),
                Field::in_request(Protocol::OpenAiChat, &value),
                url,
            );
            assert_eq!(&payload.bytes[..], br#"{"max_completion_tokens":64}"#);
        }
        for url in [
            "https://notopenai.com/v1",
            "https://openai.com.example.org/v1",
            "https://openai.com@relay.example/v1",
            "https://example.openai.azure.com/v1",
            "https://api.deepseek.com/v1",
            "https://open.bigmodel.cn/api/paas/v4",
            "http://127.0.0.1:12345/v1",
        ] {
            assert!(!openai_host(url), "{url}");
        }
        for value in [
            json!({"max_tokens":64,"max_completion_tokens":32}),
            json!({"messages":[]}),
        ] {
            assert!(Field::in_request(Protocol::OpenAiChat, &value).is_none());
        }
        assert!(
            Field::in_request(Protocol::AnthropicMessages, &json!({"max_tokens":64})).is_none()
        );
    }

    #[test]
    fn key_rename_preserves_every_value_and_ignores_nested_or_quoted_names() {
        let raw = br#"{"a":{"max_tokens":11},"b":[{"max_tokens":12}],"c":"escaped \\\"max_tokens\\\": still text","max_tokens":1.234567890123456789e+3,"z":99999999999999999999999999999}"#;
        let renamed = rename_key(raw, "max_tokens", "max_completion_tokens").unwrap();
        let expected = br#"{"a":{"max_tokens":11},"b":[{"max_tokens":12}],"c":"escaped \\\"max_tokens\\\": still text","max_completion_tokens":1.234567890123456789e+3,"z":99999999999999999999999999999}"#;
        assert_eq!(&renamed[..], expected);
        assert_eq!(
            &rename_key(&renamed, "max_completion_tokens", "max_tokens").unwrap()[..],
            raw
        );
    }

    #[test]
    fn diagnostic_requires_a_specific_unambiguous_replacement() {
        for (current, message) in [
            (
                Field::Tokens,
                "Unsupported parameter: 'max_tokens' is not supported with this model. Use 'max_completion_tokens' instead.",
            ),
            (
                Field::CompletionTokens,
                "Unknown parameter max_completion_tokens. Use max_tokens instead.",
            ),
            (
                Field::CompletionTokens,
                "max_completion_tokens is not accepted; please use max_tokens.",
            ),
        ] {
            assert!(
                wanted(
                    &serde_json::to_vec(&json!({"error":{"message":message}})).unwrap(),
                    current
                ),
                "{message}"
            );
        }
        for message in [
            "max_tokens must be less than 4096; use max_completion_tokens instead",
            "Unsupported value for max_tokens: 4096. Use max_completion_tokens instead.",
            "max_tokens value is not supported. Use max_completion_tokens instead.",
            "Unsupported max_tokens. Do not use max_completion_tokens.",
            "Unsupported max_tokens. Use max_completion_tokens or max_tokens.",
            "Unsupported max_tokens. Use max_completion_tokens and max_tokens.",
            "Unsupported max_tokens. Use max_completion_tokens. Use max_tokens instead.",
            "Unsupported parameter temperature. Use max_completion_tokens.",
            "Unsupported parameter max_tokens. Try a smaller limit.",
            "max_completion_tokens is not supported. Use max_tokens instead.",
        ] {
            assert!(
                !wanted(
                    &serde_json::to_vec(&json!({"error":{"message":message}})).unwrap(),
                    Field::Tokens
                ),
                "{message}"
            );
        }
        assert!(!wanted(br#"{"error":{"message":"Unsupported max_tokens; use max_completion_tokens","param":"temperature"}}"#, Field::Tokens));
        assert!(!wanted(
            b"Unsupported max_tokens; use max_completion_tokens",
            Field::Tokens
        ));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn retry_uses_the_original_deadline_and_cancels_the_second_send() {
        use http_body_util::Full;
        use hyper::{Response, server::conn::http1, service::service_fn};
        use hyper_util::rt::TokioIo;
        use std::{
            convert::Infallible,
            sync::{
                Arc,
                atomic::{AtomicUsize, Ordering},
            },
            time::Duration,
        };
        use tokio::task::JoinSet;

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let requests = Arc::new(AtomicUsize::new(0));
        let count = requests.clone();
        let task = tokio::spawn(async move {
            let mut workers = JoinSet::new();
            loop {
                tokio::select! {
                    accepted = listener.accept() => {
                        let (socket, _) = accepted.unwrap();
                        let count = count.clone();
                        workers.spawn(async move {
                            let service = service_fn(move |_| {
                                let first = count.fetch_add(1, Ordering::SeqCst) == 0;
                                async move {
                                    tokio::time::sleep(Duration::from_millis(if first {100} else {300})).await;
                                    let body: &'static [u8] = if first {
                                        br#"{"error":{"message":"Unsupported max_tokens. Use max_completion_tokens instead."}}"#
                                    } else {
                                        b"{}"
                                    };
                                    let mut response = Response::new(Full::new(Bytes::from_static(body)));
                                    *response.status_mut() = if first { StatusCode::BAD_REQUEST } else { StatusCode::OK };
                                    Ok::<_, Infallible>(response)
                                }
                            });
                            let _ = http1::Builder::new().keep_alive(false).serve_connection(TokioIo::new(socket), service).await;
                        });
                    }
                    Some(_) = workers.join_next(), if !workers.is_empty() => {}
                }
            }
        });
        let url = format!("http://{address}/v1/chat/completions");
        let client = super::super::source::transport_client().unwrap();
        let headers = header::HeaderMap::new();
        let deadline = Instant::now() + Duration::from_millis(300);
        let result = send(
            &client,
            &url,
            &headers,
            Payload::new(br#"{"max_tokens":64}"#.to_vec(), Some(Field::Tokens), &url),
            false,
            deadline,
        )
        .await;
        assert_eq!(result.err(), Some(SendFailure::Timeout));
        assert_eq!(requests.load(Ordering::SeqCst), 2);
        let expired = send(
            &client,
            &url,
            &headers,
            Payload::new(br#"{"max_tokens":64}"#.to_vec(), Some(Field::Tokens), &url),
            false,
            deadline,
        )
        .await;
        assert_eq!(expired.err(), Some(SendFailure::Timeout));
        assert_eq!(requests.load(Ordering::SeqCst), 2);
        task.abort();
        let _ = task.await;
    }
}
