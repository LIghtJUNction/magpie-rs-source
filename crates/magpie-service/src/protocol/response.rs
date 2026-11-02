use super::{model::*, request};
use serde_json::{Value, json};

/// A converted body and the single final observation for caller accounting.
#[derive(Clone, Debug, PartialEq)]
pub struct ConvertedResponse {
    pub body: Value,
    pub usage: Option<Usage>,
    pub outcome: Outcome,
}

pub(super) struct DecodedResponse {
    pub parts: Vec<Part>,
    pub usage: Option<Usage>,
    pub outcome: Outcome,
}

/// Converts one completed HTTP response without I/O or credential access.
///
/// Native bodies are preserved. Cross-protocol bodies reject unrepresentable
/// fields, multiple candidates, unknown stops, and signed/encrypted reasoning.
///
/// # Errors
/// Returns a safe schema/compatibility error; no payload is copied into errors.
pub fn convert_response(
    source: Protocol,
    target: Protocol,
    body: &Value,
    context: &ResponseContext,
) -> Result<ConvertedResponse> {
    object(body, "response")?;
    if body.get("error").is_some_and(|v| !v.is_null()) {
        let error = upstream_error(body);
        return Ok(ConvertedResponse {
            body: error_body(target, &error),
            usage: None,
            outcome: Outcome::Failed(error.category),
        });
    }
    if source == target {
        let usage = usage_field(source, body)?
            .map(|v| Usage::decode(source, v))
            .transpose()?;
        let finish = native_finish(source, body)?;
        return Ok(ConvertedResponse {
            body: body.clone(),
            usage,
            outcome: Outcome::Finished(finish),
        });
    }
    let response = decode(source, body)?;
    let body = match &response.outcome {
        Outcome::Finished(finish) => render(
            target,
            &response.parts,
            finish,
            response.usage.as_ref(),
            context,
        )?,
        Outcome::Failed(category) => error_body(
            target,
            &GatewayError {
                category: *category,
                status: 502,
            },
        ),
        Outcome::Cancelled => return Err(unsupported("response.cancelled")),
    };
    Ok(ConvertedResponse {
        body,
        usage: response.usage,
        outcome: response.outcome,
    })
}

fn usage_field(protocol: Protocol, body: &Value) -> Result<Option<&Value>> {
    object(body, "response")?;
    Ok(body
        .get(if protocol == Protocol::Gemini {
            "usageMetadata"
        } else {
            "usage"
        })
        .filter(|v| !v.is_null()))
}

pub(super) fn decode(protocol: Protocol, body: &Value) -> Result<DecodedResponse> {
    let usage = usage_field(protocol, body)?
        .map(|v| Usage::decode(protocol, v))
        .transpose()?;
    let parts = match protocol {
        Protocol::OpenAiChat => {
            fields(
                body,
                &["id", "object", "created", "model", "choices", "usage"],
                "response",
            )?;
            let choice = one(
                required(body, "choices", "response.choices")?,
                "response.choices",
            )?;
            fields(
                choice,
                &["index", "message", "finish_reason", "logprobs"],
                "response.choice",
            )?;
            index_zero(choice)?;
            if choice.get("logprobs").is_some_and(|v| !v.is_null()) {
                return Err(unsupported("response.logprobs"));
            }
            let message = required(choice, "message", "response.message")?;
            fields(
                message,
                &["role", "content", "tool_calls", "refusal", "annotations"],
                "response.message",
            )?;
            if text(message, "role", "response.role")? != "assistant" {
                return Err(unsupported("response.role"));
            }
            if message
                .get("annotations")
                .is_some_and(|v| v.as_array().is_none_or(|a| !a.is_empty()))
            {
                return Err(unsupported("response.annotations"));
            }
            let mut parts =
                request::decode_parts(protocol, message.get("content").unwrap_or(&Value::Null))?;
            if let Some(calls) = message.get("tool_calls").filter(|x| !x.is_null()) {
                for call in array(calls, "response.tool_calls")? {
                    fields(call, &["id", "type", "function"], "response.tool_call")?;
                    if text(call, "type", "tool_call.type")? != "function" {
                        return Err(unsupported("tool_call.type"));
                    }
                    let function = required(call, "function", "tool_call.function")?;
                    fields(function, &["name", "arguments"], "tool_call.function")?;
                    let arguments = text(function, "arguments", "tool_call.arguments")?.to_owned();
                    argument_object(&arguments)?;
                    parts.push(Part::Call {
                        id: Some(text(call, "id", "tool_call.id")?.to_owned()),
                        name: text(function, "name", "tool_call.name")?.to_owned(),
                        arguments,
                    });
                }
            }
            if let Some(refusal) = optional_string(message, "refusal", "response.refusal")? {
                parts.push(Part::Refusal(refusal));
            }
            parts
        }
        Protocol::OpenAiResponses => {
            fields(
                body,
                &[
                    "id",
                    "object",
                    "created_at",
                    "model",
                    "status",
                    "output",
                    "usage",
                    "error",
                    "incomplete_details",
                ],
                "response",
            )?;
            decode_response_output(required(body, "output", "response.output")?)?
        }
        Protocol::AnthropicMessages => {
            fields(
                body,
                &[
                    "id",
                    "type",
                    "role",
                    "model",
                    "content",
                    "stop_reason",
                    "stop_sequence",
                    "usage",
                ],
                "response",
            )?;
            if text(body, "role", "response.role")? != "assistant" {
                return Err(unsupported("response.role"));
            }
            request::decode_parts(protocol, required(body, "content", "response.content")?)?
        }
        Protocol::Gemini => {
            fields(
                body,
                &["candidates", "usageMetadata", "modelVersion", "responseId"],
                "response",
            )?;
            let candidate = one(
                required(body, "candidates", "response.candidates")?,
                "response.candidates",
            )?;
            fields(
                candidate,
                &["index", "content", "finishReason"],
                "response.candidate",
            )?;
            index_zero(candidate)?;
            let content = required(candidate, "content", "response.content")?;
            fields(content, &["role", "parts"], "response.content")?;
            if content.get("role").is_some_and(|v| v != "model") {
                return Err(unsupported("response.role"));
            }
            request::decode_parts(protocol, required(content, "parts", "response.parts")?)?
        }
    };
    let mut finish = native_finish(protocol, body)?;
    if protocol == Protocol::Gemini
        && finish == FinishReason::Stop
        && parts.iter().any(|p| matches!(p, Part::Call { .. }))
    {
        finish = FinishReason::ToolCalls;
    }
    Ok(DecodedResponse {
        parts,
        usage,
        outcome: Outcome::Finished(finish),
    })
}

pub(super) fn one<'a>(values: &'a Value, at: &'static str) -> Result<&'a Value> {
    let values = array(values, at)?;
    if values.len() != 1 {
        return Err(unsupported(at));
    }
    Ok(&values[0])
}

pub(super) fn index_zero(value: &Value) -> Result<()> {
    if value.get("index").is_some_and(|v| v.as_u64() != Some(0)) {
        return Err(unsupported("response.candidate.index"));
    }
    Ok(())
}

pub(super) fn decode_response_output(output: &Value) -> Result<Vec<Part>> {
    let mut parts = Vec::new();
    for item in array(output, "response.output")? {
        match text(item, "type", "response.output.type")? {
            "message" => {
                fields(
                    item,
                    &["type", "id", "role", "status", "content"],
                    "response.output.message",
                )?;
                if text(item, "role", "response.role")? != "assistant" {
                    return Err(unsupported("response.role"));
                }
                if item.get("status").is_some_and(|v| v != "completed") {
                    return Err(unsupported("response.output.status"));
                }
                parts.extend(request::decode_parts(
                    Protocol::OpenAiResponses,
                    required(item, "content", "response.content")?,
                )?);
            }
            "function_call" => {
                fields(
                    item,
                    &["type", "id", "status", "call_id", "name", "arguments"],
                    "response.output.function_call",
                )?;
                if item.get("status").is_some_and(|v| v != "completed") {
                    return Err(unsupported("response.output.status"));
                }
                let arguments = text(item, "arguments", "tool_call.arguments")?.to_owned();
                argument_object(&arguments)?;
                parts.push(Part::Call {
                    id: Some(text(item, "call_id", "tool_call.call_id")?.to_owned()),
                    name: text(item, "name", "tool_call.name")?.to_owned(),
                    arguments,
                });
            }
            "reasoning" => {
                fields(
                    item,
                    &["type", "id", "summary", "encrypted_content", "status"],
                    "response.output.reasoning",
                )?;
                if item.get("encrypted_content").is_some_and(|v| !v.is_null()) {
                    return Err(unsupported("response.reasoning.encrypted_content"));
                }
                for summary in array(
                    required(item, "summary", "response.reasoning.summary")?,
                    "response.reasoning.summary",
                )? {
                    fields(summary, &["type", "text"], "response.reasoning.summary")?;
                    if text(summary, "type", "response.reasoning.summary.type")? != "summary_text" {
                        return Err(unsupported("response.reasoning.summary.type"));
                    }
                    parts.push(Part::Reasoning {
                        text: text(summary, "text", "response.reasoning.text")?.to_owned(),
                        signature: None,
                    });
                }
            }
            _ => return Err(unsupported("response.output.type")),
        }
    }
    Ok(parts)
}

pub(super) fn native_finish(protocol: Protocol, body: &Value) -> Result<FinishReason> {
    match protocol {
        Protocol::OpenAiChat => {
            let choice = one(
                required(body, "choices", "response.choices")?,
                "response.choices",
            )?;
            parse_finish(
                protocol,
                text(choice, "finish_reason", "response.finish_reason")?,
                None,
            )
        }
        Protocol::OpenAiResponses => match text(body, "status", "response.status")? {
            "completed" => {
                let tools = body
                    .get("output")
                    .and_then(Value::as_array)
                    .is_some_and(|items| {
                        items
                            .iter()
                            .any(|v| v.get("type") == Some(&json!("function_call")))
                    });
                Ok(if tools {
                    FinishReason::ToolCalls
                } else {
                    FinishReason::Stop
                })
            }
            "incomplete" => {
                let details = required(body, "incomplete_details", "response.incomplete_details")?;
                fields(details, &["reason"], "response.incomplete_details")?;
                match text(details, "reason", "response.incomplete_details.reason")? {
                    "max_output_tokens" => Ok(FinishReason::Length),
                    "content_filter" => Ok(FinishReason::ContentFilter),
                    _ => Err(unsupported("response.incomplete_details.reason")),
                }
            }
            _ => Err(unsupported("response.status")),
        },
        Protocol::AnthropicMessages => parse_finish(
            protocol,
            text(body, "stop_reason", "response.stop_reason")?,
            body.get("stop_sequence"),
        ),
        Protocol::Gemini => {
            let candidate = one(
                required(body, "candidates", "response.candidates")?,
                "response.candidates",
            )?;
            parse_finish(
                protocol,
                text(candidate, "finishReason", "response.finishReason")?,
                None,
            )
        }
    }
}

pub(super) fn parse_finish(
    protocol: Protocol,
    reason: &str,
    sequence: Option<&Value>,
) -> Result<FinishReason> {
    match (protocol, reason) {
        (Protocol::OpenAiChat, "stop")
        | (Protocol::AnthropicMessages, "end_turn")
        | (Protocol::Gemini, "STOP") => Ok(FinishReason::Stop),
        (Protocol::OpenAiChat, "length")
        | (Protocol::AnthropicMessages, "max_tokens")
        | (Protocol::Gemini, "MAX_TOKENS") => Ok(FinishReason::Length),
        (Protocol::OpenAiChat, "tool_calls") | (Protocol::AnthropicMessages, "tool_use") => {
            Ok(FinishReason::ToolCalls)
        }
        (Protocol::OpenAiChat, "content_filter") | (Protocol::Gemini, "SAFETY") => {
            Ok(FinishReason::ContentFilter)
        }
        (Protocol::AnthropicMessages, "stop_sequence") => Ok(FinishReason::StopSequence(
            string(
                sequence.ok_or_else(|| fail(ErrorKind::InvalidInput, "response.stop_sequence"))?,
                "response.stop_sequence",
            )?
            .to_owned(),
        )),
        _ => Err(unsupported("response.finish_reason")),
    }
}

pub(super) fn finish_value(protocol: Protocol, reason: &FinishReason) -> Result<&'static str> {
    match (protocol, reason) {
        (Protocol::OpenAiChat, FinishReason::Stop) => Ok("stop"),
        (Protocol::OpenAiChat, FinishReason::Length) => Ok("length"),
        (Protocol::OpenAiChat, FinishReason::ToolCalls) => Ok("tool_calls"),
        (Protocol::OpenAiChat, FinishReason::ContentFilter) => Ok("content_filter"),
        (Protocol::AnthropicMessages, FinishReason::Stop) => Ok("end_turn"),
        (Protocol::AnthropicMessages, FinishReason::Length) => Ok("max_tokens"),
        (Protocol::AnthropicMessages, FinishReason::ToolCalls) => Ok("tool_use"),
        (Protocol::AnthropicMessages, FinishReason::StopSequence(_)) => Ok("stop_sequence"),
        (Protocol::Gemini, FinishReason::Stop | FinishReason::ToolCalls) => Ok("STOP"),
        (Protocol::Gemini, FinishReason::Length) => Ok("MAX_TOKENS"),
        (Protocol::Gemini, FinishReason::ContentFilter) => Ok("SAFETY"),
        (Protocol::OpenAiResponses, FinishReason::Stop | FinishReason::ToolCalls) => {
            Ok("completed")
        }
        (Protocol::OpenAiResponses, FinishReason::Length | FinishReason::ContentFilter) => {
            Ok("incomplete")
        }
        _ => Err(unsupported("response.finish_reason")),
    }
}

pub(super) fn render(
    protocol: Protocol,
    parts: &[Part],
    finish: &FinishReason,
    usage: Option<&Usage>,
    context: &ResponseContext,
) -> Result<Value> {
    let finish_wire = finish_value(protocol, finish)?;
    let mut output = match protocol {
        Protocol::OpenAiChat => {
            let mut texts = String::new();
            let mut calls = Vec::new();
            let mut refusal = None;
            let mut seen_call = false;
            for part in parts {
                match part {
                    Part::Text(text) => {
                        if seen_call {
                            return Err(unsupported("response.interleaved_text_and_tools"));
                        }
                        texts.push_str(text);
                    }
                    Part::Call {
                        id,
                        name,
                        arguments,
                    } => {
                        seen_call = true;
                        argument_object(arguments)?;
                        calls.push(json!({"id":id.as_deref().ok_or_else(|| unsupported("tool_call.missing_id"))?,"type":"function","function":{"name":name,"arguments":arguments}}));
                    }
                    Part::Refusal(text) => {
                        if refusal.is_some() {
                            return Err(unsupported("response.multiple_refusals"));
                        }
                        refusal = Some(text);
                    }
                    _ => return Err(unsupported("response.content")),
                }
            }
            let mut message = json!({"role":"assistant","content":if texts.is_empty() && !calls.is_empty() {Value::Null} else {json!(texts)}});
            if !calls.is_empty() {
                message["tool_calls"] = json!(calls);
            }
            if let Some(refusal) = refusal {
                message["refusal"] = json!(refusal);
            }
            json!({"id":context.id,"object":"chat.completion","created":context.created,"model":context.model,"choices":[{"index":0,"message":message,"finish_reason":finish_wire}]})
        }
        Protocol::OpenAiResponses => {
            let mut items = Vec::new();
            let mut content = Vec::new();
            for part in parts {
                match part {
                    Part::Call {
                        id,
                        name,
                        arguments,
                    } => {
                        flush_output(&mut items, &mut content, context);
                        argument_object(arguments)?;
                        items.push(json!({"type":"function_call","id":format!("{}_item_{}",context.id,items.len()),"status":"completed","call_id":id.as_deref().ok_or_else(|| unsupported("tool_call.missing_id"))?,"name":name,"arguments":arguments}));
                    }
                    Part::Reasoning {
                        text,
                        signature: None,
                    } => {
                        flush_output(&mut items, &mut content, context);
                        items.push(json!({"type":"reasoning","id":format!("{}_item_{}",context.id,items.len()),"summary":[{"type":"summary_text","text":text}]}));
                    }
                    _ => {
                        let mut converted = request::encode_parts(
                            protocol,
                            Role::Assistant,
                            std::slice::from_ref(part),
                        )?;
                        for value in &mut converted {
                            if value.get("type") == Some(&json!("output_text")) {
                                value["annotations"] = json!([]);
                            }
                        }
                        content.extend(converted);
                    }
                }
            }
            flush_output(&mut items, &mut content, context);
            let mut body = json!({"id":context.id,"object":"response","created_at":context.created,"model":context.model,"status":finish_wire,"output":items,"error":null,"incomplete_details":null});
            if matches!(finish, FinishReason::Length | FinishReason::ContentFilter) {
                body["incomplete_details"] = json!({"reason":if *finish == FinishReason::Length {"max_output_tokens"} else {"content_filter"}});
            }
            body
        }
        Protocol::AnthropicMessages => {
            if usage.is_none_or(|u| u.input_tokens.is_none() || u.output_tokens.is_none()) {
                return Err(unsupported("usage.required_anthropic_counts"));
            }
            json!({"id":context.id,"type":"message","role":"assistant","model":context.model,"content":request::encode_parts(protocol,Role::Assistant,parts)?,"stop_reason":finish_wire,"stop_sequence":match finish {FinishReason::StopSequence(value) => json!(value),_ => Value::Null}})
        }
        Protocol::Gemini => {
            json!({"responseId":context.id,"modelVersion":context.model,"candidates":[{"index":0,"content":{"role":"model","parts":request::encode_parts(protocol,Role::Assistant,parts)?},"finishReason":finish_wire}]})
        }
    };
    if let Some(usage) = usage {
        output[if protocol == Protocol::Gemini {
            "usageMetadata"
        } else {
            "usage"
        }] = usage.encode(protocol)?;
    }
    Ok(output)
}

fn flush_output(items: &mut Vec<Value>, content: &mut Vec<Value>, context: &ResponseContext) {
    if !content.is_empty() {
        items.push(json!({"type":"message","id":format!("{}_item_{}",context.id,items.len()),"status":"completed","role":"assistant","content":std::mem::take(content)}));
    }
}

/// Encodes a classified error without returning raw upstream messages or bodies.
#[must_use]
pub fn error_body(protocol: Protocol, error: &GatewayError) -> Value {
    let message = error.code();
    match protocol {
        Protocol::OpenAiChat | Protocol::OpenAiResponses => {
            json!({"error":{"type":match error.category {
            ErrorCategory::RateLimit | ErrorCategory::Quota => "rate_limit_error",
            ErrorCategory::UpstreamServer | ErrorCategory::Internal | ErrorCategory::Network | ErrorCategory::Timeout => "server_error",
            _ => "invalid_request_error",
        },"code":message,"message":message,"param":null}})
        }
        Protocol::AnthropicMessages => json!({"type":"error","error":{"type":match error.category {
            ErrorCategory::Authentication => "authentication_error",
            ErrorCategory::Permission => "permission_error",
            ErrorCategory::ModelNotFound => "not_found_error",
            ErrorCategory::Quota | ErrorCategory::RateLimit => "rate_limit_error",
            ErrorCategory::UpstreamClient | ErrorCategory::Compatibility => "invalid_request_error",
            ErrorCategory::UpstreamServer => "overloaded_error",
            _ => "api_error",
        },"message":message}}),
        Protocol::Gemini => {
            json!({"error":{"code":error.status,"message":message,"status":match error.category {
                ErrorCategory::Authentication => "UNAUTHENTICATED",
                ErrorCategory::Permission => "PERMISSION_DENIED",
                ErrorCategory::ModelNotFound => "NOT_FOUND",
                ErrorCategory::Quota | ErrorCategory::RateLimit => "RESOURCE_EXHAUSTED",
            ErrorCategory::UpstreamClient | ErrorCategory::Compatibility => "INVALID_ARGUMENT",
                ErrorCategory::Timeout => "DEADLINE_EXCEEDED",
                ErrorCategory::Network | ErrorCategory::UpstreamServer => "UNAVAILABLE",
                ErrorCategory::Internal => "INTERNAL",
            }}})
        }
    }
}

pub(super) fn upstream_error(body: &Value) -> GatewayError {
    let error = body.get("error").unwrap_or(body);
    let code = error
        .get("code")
        .and_then(Value::as_str)
        .or_else(|| error.get("type").and_then(Value::as_str))
        .or_else(|| error.get("status").and_then(Value::as_str))
        .unwrap_or("");
    let (category, status) = match code {
        "authentication_error" | "invalid_api_key" | "UNAUTHENTICATED" => {
            (ErrorCategory::Authentication, 401)
        }
        "permission_error" | "PERMISSION_DENIED" => (ErrorCategory::Permission, 403),
        "not_found_error" | "model_not_found" | "NOT_FOUND" => (ErrorCategory::ModelNotFound, 404),
        "insufficient_quota" | "quota_exceeded" => (ErrorCategory::Quota, 429),
        "rate_limit_error" | "rate_limit_exceeded" | "RESOURCE_EXHAUSTED" => {
            (ErrorCategory::RateLimit, 429)
        }
        "invalid_request_error" | "INVALID_ARGUMENT" => (ErrorCategory::UpstreamClient, 400),
        "timeout_error" | "DEADLINE_EXCEEDED" => (ErrorCategory::Timeout, 504),
        _ => (ErrorCategory::UpstreamServer, 502),
    };
    GatewayError { category, status }
}
