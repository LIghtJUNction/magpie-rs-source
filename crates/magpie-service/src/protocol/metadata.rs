//! Pure protocol-to-metadata extraction; no storage or plugin policy lives here.
use super::{Protocol, Usage};
use serde_json::Value;

pub(crate) fn protocol_name(protocol: Protocol) -> &'static str {
    match protocol {
        Protocol::OpenAiChat => "openai_chat",
        Protocol::OpenAiResponses => "openai_responses",
        Protocol::AnthropicMessages => "anthropic_messages",
        Protocol::Gemini => "gemini",
    }
}

/// Merge cumulative snapshots, never adding the same report twice. Only decoded
/// numeric token fields survive; arbitrary upstream extensions cannot enter a record.
pub(crate) fn observe(protocol: Protocol, value: &Value, previous: &mut Option<Usage>) {
    let raw = value
        .get("usage")
        .or_else(|| value.pointer("/response/usage"))
        .or_else(|| value.pointer("/message/usage"))
        .or_else(|| value.get("usageMetadata"));
    let Some(Ok(next)) = raw.map(|raw| Usage::decode(protocol, raw)) else {
        return;
    };
    if next == Usage::default() {
        return;
    }
    let prev = previous.get_or_insert_with(Usage::default);
    macro_rules! merge { ($($field:ident),*) => { $(if next.$field.is_some() { prev.$field = next.$field; })* }; }
    merge!(
        input_tokens,
        output_tokens,
        cached_input_tokens,
        cache_creation_input_tokens,
        reasoning_tokens
    );
    prev.total_tokens =
        next.total_tokens
            .or_else(|| match prev.input_tokens.zip(prev.output_tokens) {
                Some((input, output)) => input.checked_add(output),
                None => prev.total_tokens,
            });
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn cumulative_partial_usage_is_patched_without_double_counting() {
        let mut usage = None;
        observe(
            Protocol::AnthropicMessages,
            &json!({"message":{"usage":{"input_tokens":8,"cache_read_input_tokens":2,"output_tokens":1}}}),
            &mut usage,
        );
        for _ in 0..2 {
            observe(
                Protocol::AnthropicMessages,
                &json!({"usage":{"output_tokens":5}}),
                &mut usage,
            );
        }
        let u = usage.unwrap();
        assert_eq!(
            (u.input_tokens, u.output_tokens, u.total_tokens),
            (Some(10), Some(5), Some(15))
        );
    }
}
