use serde_json::{Map, Value, json};
use std::{collections::BTreeSet, error::Error, fmt};

/// Wire formats accepted by the pure converter.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash)]
pub enum Protocol {
    OpenAiChat,
    OpenAiResponses,
    AnthropicMessages,
    Gemini,
}

/// Stable conversion failure classes; values never contain user payloads.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum ErrorKind {
    InvalidInput,
    UnsupportedField,
    UnsupportedSemantics,
    LimitExceeded,
    InvalidUtf8,
    InvalidJson,
    InvalidSequence,
    IncompleteStream,
    Closed,
}

/// A safe error identifying a static schema location without reflecting input.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ConversionError {
    pub kind: ErrorKind,
    pub location: &'static str,
}

impl fmt::Display for ConversionError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let code = match self.kind {
            ErrorKind::InvalidInput => "invalid_input",
            ErrorKind::UnsupportedField => "unsupported_field",
            ErrorKind::UnsupportedSemantics => "unsupported_semantics",
            ErrorKind::LimitExceeded => "limit_exceeded",
            ErrorKind::InvalidUtf8 => "invalid_utf8",
            ErrorKind::InvalidJson => "invalid_json",
            ErrorKind::InvalidSequence => "invalid_sequence",
            ErrorKind::IncompleteStream => "incomplete_stream",
            ErrorKind::Closed => "closed",
        };
        write!(f, "{code} at {}", self.location)
    }
}

impl Error for ConversionError {}

pub(super) type Result<T> = std::result::Result<T, ConversionError>;

pub(super) fn fail(kind: ErrorKind, location: &'static str) -> ConversionError {
    ConversionError { kind, location }
}

pub(super) fn unsupported(location: &'static str) -> ConversionError {
    fail(ErrorKind::UnsupportedSemantics, location)
}

pub(super) fn object<'a>(v: &'a Value, at: &'static str) -> Result<&'a Map<String, Value>> {
    v.as_object()
        .ok_or_else(|| fail(ErrorKind::InvalidInput, at))
}

pub(super) fn array<'a>(v: &'a Value, at: &'static str) -> Result<&'a [Value]> {
    v.as_array()
        .map(Vec::as_slice)
        .ok_or_else(|| fail(ErrorKind::InvalidInput, at))
}

pub(super) fn string<'a>(v: &'a Value, at: &'static str) -> Result<&'a str> {
    v.as_str().ok_or_else(|| fail(ErrorKind::InvalidInput, at))
}

pub(super) fn required<'a>(v: &'a Value, key: &str, at: &'static str) -> Result<&'a Value> {
    v.get(key).ok_or_else(|| fail(ErrorKind::InvalidInput, at))
}

pub(super) fn text<'a>(v: &'a Value, key: &str, at: &'static str) -> Result<&'a str> {
    string(required(v, key, at)?, at)
}

pub(super) fn fields(v: &Value, allowed: &[&str], at: &'static str) -> Result<()> {
    if object(v, at)?
        .keys()
        .any(|key| !allowed.contains(&key.as_str()))
    {
        return Err(fail(ErrorKind::UnsupportedField, at));
    }
    Ok(())
}

pub(super) fn parse_json(bytes: &[u8], at: &'static str) -> Result<Value> {
    serde_json::from_slice(bytes).map_err(|_| fail(ErrorKind::InvalidJson, at))
}

pub(super) fn argument_object(arguments: &str) -> Result<Value> {
    let value = parse_json(arguments.as_bytes(), "tool.arguments")?;
    object(&value, "tool.arguments")?;
    reject_duplicate_argument_keys(arguments)?;
    Ok(value)
}

fn reject_duplicate_argument_keys(arguments: &str) -> Result<()> {
    enum Container {
        Object {
            keys: BTreeSet<String>,
            key_next: bool,
        },
        Array,
    }
    // serde_json already validated nesting, escapes, and token syntax. This
    // second lexical pass prevents its map representation from erasing duplicate
    // keys while projecting a JSON argument string into another protocol's object.
    let bytes = arguments.as_bytes();
    let mut stack = Vec::new();
    let mut offset = 0;
    while offset < bytes.len() {
        match bytes[offset] {
            b'{' => stack.push(Container::Object {
                keys: BTreeSet::new(),
                key_next: true,
            }),
            b'[' => stack.push(Container::Array),
            b'}' | b']' => {
                stack.pop();
            }
            b',' => {
                if let Some(Container::Object { key_next, .. }) = stack.last_mut() {
                    *key_next = true;
                }
            }
            b':' => {
                if let Some(Container::Object { key_next, .. }) = stack.last_mut() {
                    *key_next = false;
                }
            }
            b'"' => {
                let start = offset;
                offset += 1;
                while offset < bytes.len() && bytes[offset] != b'"' {
                    if bytes[offset] == b'\\' {
                        offset += 1;
                    }
                    offset += 1;
                }
                if let Some(Container::Object {
                    keys,
                    key_next: true,
                }) = stack.last_mut()
                {
                    let slice = arguments
                        .get(start..=offset)
                        .ok_or_else(|| fail(ErrorKind::InvalidJson, "tool.arguments"))?;
                    let key: String = serde_json::from_str(slice)
                        .map_err(|_| fail(ErrorKind::InvalidJson, "tool.arguments"))?;
                    if !keys.insert(key) {
                        return Err(unsupported("tool.arguments.duplicate_key"));
                    }
                }
            }
            _ => {}
        }
        offset += 1;
    }
    Ok(())
}

/// Token counts are optional: absence is unknown, not zero.
///
/// Input includes cached input; output includes reasoning tokens. Anthropic's
/// separate cache creation/read input and Gemini's separate thought output are
/// included when decoding their wire formats. `total_tokens` is the provider's
/// total when available, otherwise a checked sum when both sides are known.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct Usage {
    pub input_tokens: Option<u64>,
    pub output_tokens: Option<u64>,
    pub total_tokens: Option<u64>,
    pub cached_input_tokens: Option<u64>,
    pub cache_creation_input_tokens: Option<u64>,
    pub reasoning_tokens: Option<u64>,
}

fn count(v: &Value, key: &str) -> Result<Option<u64>> {
    match v.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(value) => value
            .as_u64()
            .map(Some)
            .ok_or_else(|| fail(ErrorKind::InvalidInput, "usage.count")),
    }
}

fn checked_sum(a: Option<u64>, b: Option<u64>) -> Result<Option<u64>> {
    a.zip(b)
        .map(|(a, b)| {
            a.checked_add(b)
                .ok_or_else(|| fail(ErrorKind::InvalidInput, "usage.overflow"))
        })
        .transpose()
}

impl Usage {
    pub(super) fn decode(protocol: Protocol, v: &Value) -> Result<Self> {
        let mut usage = Self::default();
        match protocol {
            Protocol::OpenAiChat | Protocol::OpenAiResponses => {
                let chat = protocol == Protocol::OpenAiChat;
                let (input, output, input_details, output_details) = if chat {
                    (
                        "prompt_tokens",
                        "completion_tokens",
                        "prompt_tokens_details",
                        "completion_tokens_details",
                    )
                } else {
                    (
                        "input_tokens",
                        "output_tokens",
                        "input_tokens_details",
                        "output_tokens_details",
                    )
                };
                fields(
                    v,
                    &[input, output, "total_tokens", input_details, output_details],
                    "usage",
                )?;
                usage.input_tokens = count(v, input)?;
                usage.output_tokens = count(v, output)?;
                usage.total_tokens = count(v, "total_tokens")?;
                if let Some(details) = v.get(input_details).filter(|x| !x.is_null()) {
                    fields(
                        details,
                        &["cached_tokens", "audio_tokens"],
                        "usage.input_details",
                    )?;
                    if count(details, "audio_tokens")?.is_some_and(|n| n != 0) {
                        return Err(unsupported("usage.audio_tokens"));
                    }
                    usage.cached_input_tokens = count(details, "cached_tokens")?;
                }
                if let Some(details) = v.get(output_details).filter(|x| !x.is_null()) {
                    fields(
                        details,
                        &[
                            "reasoning_tokens",
                            "audio_tokens",
                            "accepted_prediction_tokens",
                            "rejected_prediction_tokens",
                        ],
                        "usage.output_details",
                    )?;
                    for key in [
                        "audio_tokens",
                        "accepted_prediction_tokens",
                        "rejected_prediction_tokens",
                    ] {
                        if count(details, key)?.is_some_and(|n| n != 0) {
                            return Err(unsupported("usage.output_details"));
                        }
                    }
                    usage.reasoning_tokens = count(details, "reasoning_tokens")?;
                }
            }
            Protocol::AnthropicMessages => {
                fields(
                    v,
                    &[
                        "input_tokens",
                        "output_tokens",
                        "cache_read_input_tokens",
                        "cache_creation_input_tokens",
                    ],
                    "usage",
                )?;
                let input = count(v, "input_tokens")?;
                usage.cached_input_tokens = count(v, "cache_read_input_tokens")?;
                usage.cache_creation_input_tokens = count(v, "cache_creation_input_tokens")?;
                usage.input_tokens = input
                    .map(|n| {
                        n.checked_add(usage.cached_input_tokens.unwrap_or(0))
                            .and_then(|n| {
                                n.checked_add(usage.cache_creation_input_tokens.unwrap_or(0))
                            })
                            .ok_or_else(|| fail(ErrorKind::InvalidInput, "usage.overflow"))
                    })
                    .transpose()?;
                usage.output_tokens = count(v, "output_tokens")?;
            }
            Protocol::Gemini => {
                fields(
                    v,
                    &[
                        "promptTokenCount",
                        "candidatesTokenCount",
                        "totalTokenCount",
                        "cachedContentTokenCount",
                        "thoughtsTokenCount",
                    ],
                    "usage",
                )?;
                usage.input_tokens = count(v, "promptTokenCount")?;
                usage.reasoning_tokens = count(v, "thoughtsTokenCount")?;
                usage.output_tokens = count(v, "candidatesTokenCount")?
                    .map(|n| {
                        n.checked_add(usage.reasoning_tokens.unwrap_or(0))
                            .ok_or_else(|| fail(ErrorKind::InvalidInput, "usage.overflow"))
                    })
                    .transpose()?;
                usage.total_tokens = count(v, "totalTokenCount")?;
                usage.cached_input_tokens = count(v, "cachedContentTokenCount")?;
            }
        }
        if usage.total_tokens.is_none() {
            usage.total_tokens = checked_sum(usage.input_tokens, usage.output_tokens)?;
        }
        Ok(usage)
    }

    pub(super) fn encode(&self, protocol: Protocol) -> Result<Value> {
        let mut out = Map::new();
        let mut put = |key: &str, n: Option<u64>| {
            if let Some(n) = n {
                out.insert(key.to_owned(), json!(n));
            }
        };
        match protocol {
            Protocol::OpenAiChat | Protocol::OpenAiResponses => {
                if self.cache_creation_input_tokens.is_some_and(|n| n != 0) {
                    return Err(unsupported("usage.cache_creation_input_tokens"));
                }
                let chat = protocol == Protocol::OpenAiChat;
                put(
                    if chat {
                        "prompt_tokens"
                    } else {
                        "input_tokens"
                    },
                    self.input_tokens,
                );
                put(
                    if chat {
                        "completion_tokens"
                    } else {
                        "output_tokens"
                    },
                    self.output_tokens,
                );
                put("total_tokens", self.total_tokens);
                if let Some(n) = self.cached_input_tokens {
                    out.insert(
                        if chat {
                            "prompt_tokens_details"
                        } else {
                            "input_tokens_details"
                        }
                        .to_owned(),
                        json!({"cached_tokens":n}),
                    );
                }
                if let Some(n) = self.reasoning_tokens {
                    out.insert(
                        if chat {
                            "completion_tokens_details"
                        } else {
                            "output_tokens_details"
                        }
                        .to_owned(),
                        json!({"reasoning_tokens":n}),
                    );
                }
            }
            Protocol::AnthropicMessages => {
                if self.reasoning_tokens.is_some_and(|n| n != 0) {
                    return Err(unsupported("usage.reasoning_tokens"));
                }
                let input = self
                    .input_tokens
                    .map(|n| {
                        n.checked_sub(self.cached_input_tokens.unwrap_or(0))
                            .and_then(|n| {
                                n.checked_sub(self.cache_creation_input_tokens.unwrap_or(0))
                            })
                            .ok_or_else(|| fail(ErrorKind::InvalidInput, "usage.cache_tokens"))
                    })
                    .transpose()?;
                put("input_tokens", input);
                put("output_tokens", self.output_tokens);
                put("cache_read_input_tokens", self.cached_input_tokens);
                put(
                    "cache_creation_input_tokens",
                    self.cache_creation_input_tokens,
                );
                // Anthropic has no independent total counter. Only a redundant
                // total can be represented without losing information.
                if self.total_tokens.is_some()
                    && self.total_tokens != checked_sum(self.input_tokens, self.output_tokens)?
                {
                    return Err(unsupported("usage.total_tokens"));
                }
            }
            Protocol::Gemini => {
                if self.cache_creation_input_tokens.is_some_and(|n| n != 0) {
                    return Err(unsupported("usage.cache_creation_input_tokens"));
                }
                put("promptTokenCount", self.input_tokens);
                put(
                    "candidatesTokenCount",
                    self.output_tokens
                        .map(|n| {
                            n.checked_sub(self.reasoning_tokens.unwrap_or(0))
                                .ok_or_else(|| {
                                    fail(ErrorKind::InvalidInput, "usage.reasoning_tokens")
                                })
                        })
                        .transpose()?,
                );
                put("totalTokenCount", self.total_tokens);
                put("cachedContentTokenCount", self.cached_input_tokens);
                put("thoughtsTokenCount", self.reasoning_tokens);
            }
        }
        Ok(Value::Object(out))
    }
}

/// A successful semantic stop, distinct from transport EOF or cancellation.
#[derive(Clone, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum FinishReason {
    Stop,
    StopSequence(String),
    Length,
    ToolCalls,
    ContentFilter,
}

/// Final state of a conversion; an interrupted stream cannot become success.
#[derive(Clone, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum Outcome {
    Finished(FinishReason),
    Failed(ErrorCategory),
    Cancelled,
}

/// Error categories safe to expose without copying an upstream error message.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum ErrorCategory {
    Compatibility,
    Authentication,
    Permission,
    ModelNotFound,
    Quota,
    RateLimit,
    UpstreamClient,
    UpstreamServer,
    Network,
    Timeout,
    Internal,
}

/// Caller-provided HTTP classification; raw upstream bodies are never echoed.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct GatewayError {
    pub category: ErrorCategory,
    pub status: u16,
}

impl GatewayError {
    pub(super) fn code(&self) -> &'static str {
        match self.category {
            ErrorCategory::Compatibility => "protocol_compatibility_error",
            ErrorCategory::Authentication => "authentication_error",
            ErrorCategory::Permission => "permission_error",
            ErrorCategory::ModelNotFound => "not_found_error",
            ErrorCategory::Quota => "quota_exceeded",
            ErrorCategory::RateLimit => "rate_limit_error",
            ErrorCategory::UpstreamClient => "upstream_request_error",
            ErrorCategory::UpstreamServer => "upstream_server_error",
            ErrorCategory::Network => "network_error",
            ErrorCategory::Timeout => "timeout_error",
            ErrorCategory::Internal => "internal_error",
        }
    }
}

/// Identity supplied by the caller when a target wire format requires it.
///
/// `created` is a Unix timestamp in seconds. No identifier or timestamp is
/// inferred from prompts, credentials, or the local clock.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ResponseContext {
    pub id: String,
    pub model: String,
    pub created: u64,
}

/// Explicit memory/work bounds for one converter, independent of stream length.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Limits {
    pub max_chunk_bytes: usize,
    pub max_event_bytes: usize,
    pub max_output_bytes: usize,
    pub max_tool_argument_bytes: usize,
    pub max_blocks: usize,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            max_chunk_bytes: 64 * 1024,
            max_event_bytes: 4 * 1024 * 1024,
            max_output_bytes: 4 * 1024 * 1024,
            max_tool_argument_bytes: 256 * 1024,
            max_blocks: 128,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum Role {
    System,
    Developer,
    User,
    Assistant,
    Tool,
}

#[derive(Clone, Debug, PartialEq)]
pub(super) enum Part {
    Text(String),
    Image {
        url: String,
        detail: Option<String>,
    },
    InlineImage {
        mime: String,
        data: String,
    },
    Call {
        id: Option<String>,
        name: String,
        arguments: String,
    },
    Result {
        id: Option<String>,
        name: Option<String>,
        output: Value,
        is_error: bool,
    },
    Reasoning {
        text: String,
        signature: Option<String>,
    },
    Refusal(String),
}

#[derive(Clone, Debug, PartialEq)]
pub(super) struct Message {
    pub role: Role,
    pub parts: Vec<Part>,
}

pub(super) fn role(value: &str) -> Result<Role> {
    match value {
        "system" => Ok(Role::System),
        "developer" => Ok(Role::Developer),
        "user" => Ok(Role::User),
        "assistant" | "model" => Ok(Role::Assistant),
        "tool" => Ok(Role::Tool),
        _ => Err(unsupported("message.role")),
    }
}

pub(super) fn role_name(role: Role) -> &'static str {
    match role {
        Role::System => "system",
        Role::Developer => "developer",
        Role::User => "user",
        Role::Assistant => "assistant",
        Role::Tool => "tool",
    }
}

pub(super) fn optional_string(v: &Value, key: &str, at: &'static str) -> Result<Option<String>> {
    v.get(key)
        .filter(|x| !x.is_null())
        .map(|x| string(x, at).map(str::to_owned))
        .transpose()
}

pub(super) fn put_optional(out: &mut Value, key: &str, value: Option<Value>) {
    if let Some(value) = value {
        out[key] = value;
    }
}
