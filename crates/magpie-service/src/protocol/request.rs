use super::model::*;
use serde_json::{Value, json};

#[derive(Default)]
struct Request {
    messages: Vec<Message>,
    tools: Vec<Tool>,
    temperature: Option<Value>,
    top_p: Option<Value>,
    max_output: Option<Value>,
    stop: Option<Value>,
    effort: Option<Value>,
    tool_choice: Option<Choice>,
    parallel: Option<Value>,
    stream: Option<Value>,
}

struct Tool {
    name: String,
    description: Option<String>,
    schema: Value,
    strict: Option<bool>,
}

enum Choice {
    Auto,
    None,
    Required,
    Named(String),
}

/// Converts a request once, replacing only its model with the routed upstream ID.
///
/// Gemini carries the model in the URL, so the returned body omits `model`.
/// Native bodies retain all fields; cross-protocol bodies reject unsupported
/// fields recursively. An Anthropic target requires an explicit output limit.
///
/// # Errors
/// Returns a schema or compatibility error before any upstream request is sent.
pub fn convert_request(
    source: Protocol,
    target: Protocol,
    body: &Value,
    upstream_model: &str,
) -> Result<Value> {
    object(body, "request")?;
    if upstream_model.is_empty() {
        return Err(fail(ErrorKind::InvalidInput, "request.model"));
    }
    if source == target {
        let mut native = body.clone();
        if target != Protocol::Gemini {
            native["model"] = json!(upstream_model);
        }
        return Ok(native);
    }
    let request = decode(source, body)?;
    encode(target, request, upstream_model)
}

fn decode(protocol: Protocol, body: &Value) -> Result<Request> {
    let mut out = Request::default();
    match protocol {
        Protocol::OpenAiChat => {
            fields(
                body,
                &[
                    "model",
                    "messages",
                    "tools",
                    "tool_choice",
                    "temperature",
                    "top_p",
                    "max_tokens",
                    "max_completion_tokens",
                    "stop",
                    "reasoning_effort",
                    "parallel_tool_calls",
                    "stream",
                    "stream_options",
                ],
                "request",
            )?;
            for message in array(
                required(body, "messages", "request.messages")?,
                "request.messages",
            )? {
                fields(
                    message,
                    &["role", "content", "tool_calls", "tool_call_id", "refusal"],
                    "message",
                )?;
                let role = role(text(message, "role", "message.role")?)?;
                if role != Role::Tool && message.get("tool_call_id").is_some() {
                    return Err(unsupported("message.tool_call_id.role"));
                }
                if role != Role::Assistant && message.get("refusal").is_some_and(|v| !v.is_null()) {
                    return Err(unsupported("message.refusal.role"));
                }
                let mut parts = if role == Role::Tool {
                    vec![Part::Result {
                        id: Some(text(message, "tool_call_id", "message.tool_call_id")?.to_owned()),
                        name: None,
                        output: json!(text(message, "content", "message.content")?),
                        is_error: false,
                    }]
                } else {
                    decode_parts(protocol, message.get("content").unwrap_or(&Value::Null))?
                };
                if let Some(calls) = message.get("tool_calls").filter(|v| !v.is_null()) {
                    if role != Role::Assistant {
                        return Err(unsupported("message.tool_calls.role"));
                    }
                    for call in array(calls, "message.tool_calls")? {
                        fields(call, &["id", "type", "function"], "tool_call")?;
                        if text(call, "type", "tool_call.type")? != "function" {
                            return Err(unsupported("tool_call.type"));
                        }
                        let function = required(call, "function", "tool_call.function")?;
                        fields(function, &["name", "arguments"], "tool_call.function")?;
                        let arguments =
                            text(function, "arguments", "tool_call.arguments")?.to_owned();
                        argument_object(&arguments)?;
                        parts.push(Part::Call {
                            id: Some(text(call, "id", "tool_call.id")?.to_owned()),
                            name: text(function, "name", "tool_call.name")?.to_owned(),
                            arguments,
                        });
                    }
                }
                if let Some(refusal) = optional_string(message, "refusal", "message.refusal")? {
                    parts.push(Part::Refusal(refusal));
                }
                out.messages.push(Message { role, parts });
            }
            if body.get("max_tokens").is_some() && body.get("max_completion_tokens").is_some() {
                return Err(fail(ErrorKind::InvalidInput, "request.output_limit"));
            }
            out.max_output = body
                .get("max_completion_tokens")
                .or_else(|| body.get("max_tokens"))
                .cloned();
            out.effort = body.get("reasoning_effort").cloned();
            out.stop = body.get("stop").cloned();
            out.parallel = body.get("parallel_tool_calls").cloned();
            if let Some(options) = body.get("stream_options").filter(|x| !x.is_null()) {
                fields(options, &["include_usage"], "request.stream_options")?;
                if let Some(include) = options.get("include_usage") {
                    if include != &Value::Bool(true) {
                        return Err(unsupported("request.stream_options.include_usage"));
                    }
                }
            }
        }
        Protocol::OpenAiResponses => {
            fields(
                body,
                &[
                    "model",
                    "input",
                    "instructions",
                    "tools",
                    "tool_choice",
                    "temperature",
                    "top_p",
                    "max_output_tokens",
                    "reasoning",
                    "parallel_tool_calls",
                    "stream",
                ],
                "request",
            )?;
            if let Some(instructions) = body.get("instructions").filter(|x| !x.is_null()) {
                out.messages.push(Message {
                    role: Role::System,
                    parts: vec![Part::Text(
                        string(instructions, "request.instructions")?.to_owned(),
                    )],
                });
            }
            let input = required(body, "input", "request.input")?;
            if let Some(input) = input.as_str() {
                out.messages.push(Message {
                    role: Role::User,
                    parts: vec![Part::Text(input.to_owned())],
                });
            } else {
                for item in array(input, "request.input")? {
                    let kind = item
                        .get("type")
                        .and_then(Value::as_str)
                        .unwrap_or("message");
                    match kind {
                        "message" => {
                            fields(item, &["type", "role", "content"], "input.message")?;
                            out.messages.push(Message {
                                role: role(text(item, "role", "message.role")?)?,
                                parts: decode_parts(
                                    protocol,
                                    required(item, "content", "message.content")?,
                                )?,
                            });
                        }
                        "function_call" => {
                            fields(
                                item,
                                &["type", "call_id", "name", "arguments"],
                                "input.function_call",
                            )?;
                            let arguments =
                                text(item, "arguments", "tool_call.arguments")?.to_owned();
                            argument_object(&arguments)?;
                            out.messages.push(Message {
                                role: Role::Assistant,
                                parts: vec![Part::Call {
                                    id: Some(
                                        text(item, "call_id", "tool_call.call_id")?.to_owned(),
                                    ),
                                    name: text(item, "name", "tool_call.name")?.to_owned(),
                                    arguments,
                                }],
                            });
                        }
                        "function_call_output" => {
                            fields(
                                item,
                                &["type", "call_id", "output"],
                                "input.function_call_output",
                            )?;
                            out.messages.push(Message {
                                role: Role::Tool,
                                parts: vec![Part::Result {
                                    id: Some(
                                        text(item, "call_id", "tool_result.call_id")?.to_owned(),
                                    ),
                                    name: None,
                                    output: json!(text(item, "output", "tool_result.output")?),
                                    is_error: false,
                                }],
                            });
                        }
                        // Reasoning item IDs, encrypted content, and references
                        // are provider state, not transferable plaintext.
                        _ => return Err(unsupported("request.input.item")),
                    }
                }
            }
            out.max_output = body.get("max_output_tokens").cloned();
            out.parallel = body.get("parallel_tool_calls").cloned();
            if let Some(reasoning) = body.get("reasoning").filter(|x| !x.is_null()) {
                fields(reasoning, &["effort"], "request.reasoning")?;
                out.effort = reasoning.get("effort").cloned();
            }
        }
        Protocol::AnthropicMessages => {
            fields(
                body,
                &[
                    "model",
                    "messages",
                    "system",
                    "tools",
                    "tool_choice",
                    "temperature",
                    "top_p",
                    "max_tokens",
                    "stop_sequences",
                    "stream",
                ],
                "request",
            )?;
            if let Some(system) = body.get("system") {
                out.messages.push(Message {
                    role: Role::System,
                    parts: decode_parts(protocol, system)?,
                });
            }
            for message in array(
                required(body, "messages", "request.messages")?,
                "request.messages",
            )? {
                fields(message, &["role", "content"], "message")?;
                let r = text(message, "role", "message.role")?;
                if !matches!(r, "user" | "assistant") {
                    return Err(unsupported("message.role"));
                }
                out.messages.push(Message {
                    role: role(r)?,
                    parts: decode_parts(
                        protocol,
                        required(message, "content", "message.content")?,
                    )?,
                });
            }
            out.max_output = body.get("max_tokens").cloned();
            out.stop = body.get("stop_sequences").cloned();
        }
        Protocol::Gemini => {
            fields(
                body,
                &[
                    "contents",
                    "systemInstruction",
                    "tools",
                    "toolConfig",
                    "generationConfig",
                ],
                "request",
            )?;
            if let Some(system) = body.get("systemInstruction") {
                fields(system, &["parts"], "request.systemInstruction")?;
                out.messages.push(Message {
                    role: Role::System,
                    parts: decode_parts(protocol, required(system, "parts", "message.parts")?)?,
                });
            }
            for message in array(
                required(body, "contents", "request.contents")?,
                "request.contents",
            )? {
                fields(message, &["role", "parts"], "message")?;
                let r = message
                    .get("role")
                    .map(|x| string(x, "message.role"))
                    .transpose()?
                    .unwrap_or("user");
                if !matches!(r, "model" | "user") {
                    return Err(unsupported("message.role"));
                }
                out.messages.push(Message {
                    role: role(r)?,
                    parts: decode_parts(protocol, required(message, "parts", "message.parts")?)?,
                });
            }
            if let Some(config) = body.get("generationConfig") {
                fields(
                    config,
                    &["temperature", "topP", "maxOutputTokens", "stopSequences"],
                    "request.generationConfig",
                )?;
                out.temperature = config.get("temperature").cloned();
                out.top_p = config.get("topP").cloned();
                out.max_output = config.get("maxOutputTokens").cloned();
                out.stop = config.get("stopSequences").cloned();
            }
        }
    }
    if protocol != Protocol::Gemini {
        out.temperature = body.get("temperature").cloned();
        out.top_p = body.get("top_p").cloned();
        out.stream = body.get("stream").cloned();
    }
    validate_settings(&out)?;
    out.tools = decode_tools(protocol, body.get("tools"))?;
    out.tool_choice = decode_choice(protocol, body)?;
    Ok(out)
}

fn validate_settings(request: &Request) -> Result<()> {
    for value in [&request.temperature, &request.top_p].into_iter().flatten() {
        if !value.is_null() && value.as_f64().is_none() {
            return Err(fail(ErrorKind::InvalidInput, "request.sampling"));
        }
    }
    if let Some(limit) = request.max_output.as_ref().filter(|v| !v.is_null()) {
        if !limit.as_u64().is_some_and(|n| n > 0) {
            return Err(fail(ErrorKind::InvalidInput, "request.output_limit"));
        }
    }
    for value in [&request.parallel, &request.stream].into_iter().flatten() {
        if !value.is_null() && !value.is_boolean() {
            return Err(fail(ErrorKind::InvalidInput, "request.boolean"));
        }
    }
    if let Some(effort) = request.effort.as_ref().filter(|v| !v.is_null()) {
        string(effort, "request.reasoning_effort")?;
    }
    if let Some(stop) = request.stop.as_ref().filter(|v| !v.is_null()) {
        if !stop.is_string() {
            for item in array(stop, "request.stop")? {
                string(item, "request.stop")?;
            }
        }
    }
    Ok(())
}

pub(super) fn decode_parts(protocol: Protocol, content: &Value) -> Result<Vec<Part>> {
    if content.is_null() {
        return Ok(Vec::new());
    }
    if let Some(text) = content.as_str() {
        return Ok(vec![Part::Text(text.to_owned())]);
    }
    let mut parts = Vec::new();
    for part in array(content, "message.content")? {
        if protocol == Protocol::Gemini {
            fields(
                part,
                &[
                    "text",
                    "thought",
                    "thoughtSignature",
                    "inlineData",
                    "fileData",
                    "functionCall",
                    "functionResponse",
                ],
                "part",
            )?;
            let data_fields = [
                "text",
                "inlineData",
                "fileData",
                "functionCall",
                "functionResponse",
            ];
            if data_fields
                .iter()
                .filter(|key| part.get(**key).is_some())
                .count()
                != 1
            {
                return Err(fail(ErrorKind::InvalidInput, "part.union"));
            }
            if part.get("thoughtSignature").is_some() {
                return Err(unsupported("part.thoughtSignature"));
            }
            let thought = part
                .get("thought")
                .map(|v| {
                    v.as_bool()
                        .ok_or_else(|| fail(ErrorKind::InvalidInput, "part.thought"))
                })
                .transpose()?
                .unwrap_or(false);
            if thought && part.get("text").is_none() {
                return Err(unsupported("part.thought"));
            }
            if let Some(value) = part.get("text") {
                let text = string(value, "part.text")?.to_owned();
                parts.push(if thought {
                    Part::Reasoning {
                        text,
                        signature: None,
                    }
                } else {
                    Part::Text(text)
                });
            } else if let Some(blob) = part.get("inlineData") {
                fields(blob, &["mimeType", "data"], "part.inlineData")?;
                let mime = text(blob, "mimeType", "part.mimeType")?.to_owned();
                if !mime.starts_with("image/") {
                    return Err(unsupported("part.media_type"));
                }
                parts.push(Part::InlineImage {
                    mime,
                    data: text(blob, "data", "part.data")?.to_owned(),
                });
            } else if part.get("fileData").is_some() {
                // Gemini file URIs and OpenAI image URLs have different access
                // semantics. A caller must upload/resolve media explicitly.
                return Err(unsupported("part.fileData"));
            } else if let Some(call) = part.get("functionCall") {
                fields(call, &["id", "name", "args"], "part.functionCall")?;
                let args = call.get("args").cloned().unwrap_or_else(|| json!({}));
                object(&args, "tool.arguments")?;
                parts.push(Part::Call {
                    id: optional_string(call, "id", "tool_call.id")?,
                    name: text(call, "name", "tool_call.name")?.to_owned(),
                    arguments: args.to_string(),
                });
            } else if let Some(result) = part.get("functionResponse") {
                fields(result, &["id", "name", "response"], "part.functionResponse")?;
                let output = required(result, "response", "tool_result.response")?.clone();
                object(&output, "tool_result.response")?;
                parts.push(Part::Result {
                    id: optional_string(result, "id", "tool_result.id")?,
                    name: Some(text(result, "name", "tool_result.name")?.to_owned()),
                    output,
                    is_error: false,
                });
            }
            continue;
        }
        let kind = text(part, "type", "part.type")?;
        match kind {
            "text" | "input_text" | "output_text" => {
                if !matches!(
                    (protocol, kind),
                    (Protocol::OpenAiChat | Protocol::AnthropicMessages, "text")
                        | (Protocol::OpenAiResponses, "input_text" | "output_text")
                ) {
                    return Err(unsupported("part.type"));
                }
                fields(part, &["type", "text", "annotations"], "part.text")?;
                if part
                    .get("annotations")
                    .is_some_and(|v| v.as_array().is_none_or(|a| !a.is_empty()))
                {
                    return Err(unsupported("part.annotations"));
                }
                parts.push(Part::Text(text(part, "text", "part.text")?.to_owned()));
            }
            "image_url" if protocol == Protocol::OpenAiChat => {
                fields(part, &["type", "image_url"], "part.image")?;
                let image = required(part, "image_url", "part.image_url")?;
                fields(image, &["url", "detail"], "part.image_url")?;
                parts.push(image_url(
                    text(image, "url", "part.url")?,
                    optional_string(image, "detail", "part.detail")?,
                )?);
            }
            "input_image" if protocol == Protocol::OpenAiResponses => {
                fields(part, &["type", "image_url", "detail"], "part.image")?;
                parts.push(image_url(
                    text(part, "image_url", "part.image_url")?,
                    optional_string(part, "detail", "part.detail")?,
                )?);
            }
            "image" if protocol == Protocol::AnthropicMessages => {
                fields(part, &["type", "source"], "part.image")?;
                let source = required(part, "source", "part.image.source")?;
                match text(source, "type", "part.image.source.type")? {
                    "base64" => {
                        fields(source, &["type", "media_type", "data"], "part.image.source")?;
                        parts.push(Part::InlineImage {
                            mime: text(source, "media_type", "part.media_type")?.to_owned(),
                            data: text(source, "data", "part.data")?.to_owned(),
                        });
                    }
                    "url" => {
                        fields(source, &["type", "url"], "part.image.source")?;
                        parts.push(Part::Image {
                            url: text(source, "url", "part.url")?.to_owned(),
                            detail: None,
                        });
                    }
                    _ => return Err(unsupported("part.image.source.type")),
                }
            }
            "tool_use" if protocol == Protocol::AnthropicMessages => {
                fields(part, &["type", "id", "name", "input"], "part.tool_use")?;
                let input = required(part, "input", "tool.arguments")?;
                object(input, "tool.arguments")?;
                parts.push(Part::Call {
                    id: Some(text(part, "id", "tool_call.id")?.to_owned()),
                    name: text(part, "name", "tool_call.name")?.to_owned(),
                    arguments: input.to_string(),
                });
            }
            "tool_result" if protocol == Protocol::AnthropicMessages => {
                fields(
                    part,
                    &["type", "tool_use_id", "content", "is_error"],
                    "part.tool_result",
                )?;
                let output = part.get("content").cloned().unwrap_or_else(|| json!(""));
                if !output.is_string() {
                    return Err(unsupported("tool_result.content"));
                }
                let is_error = part
                    .get("is_error")
                    .map(|v| {
                        v.as_bool()
                            .ok_or_else(|| fail(ErrorKind::InvalidInput, "tool_result.is_error"))
                    })
                    .transpose()?
                    .unwrap_or(false);
                parts.push(Part::Result {
                    id: Some(text(part, "tool_use_id", "tool_result.id")?.to_owned()),
                    name: None,
                    output,
                    is_error,
                });
            }
            "thinking" if protocol == Protocol::AnthropicMessages => {
                fields(part, &["type", "thinking", "signature"], "part.thinking")?;
                parts.push(Part::Reasoning {
                    text: text(part, "thinking", "part.thinking")?.to_owned(),
                    signature: optional_string(part, "signature", "part.signature")?,
                });
            }
            "refusal" if protocol == Protocol::OpenAiResponses => {
                fields(part, &["type", "refusal"], "part.refusal")?;
                parts.push(Part::Refusal(
                    text(part, "refusal", "part.refusal")?.to_owned(),
                ));
            }
            _ => return Err(unsupported("part.type")),
        }
    }
    Ok(parts)
}

fn image_url(url: &str, detail: Option<String>) -> Result<Part> {
    if let Some(data) = url.strip_prefix("data:") {
        if detail.as_deref().is_some_and(|s| s != "auto") {
            return Err(unsupported("part.image.detail"));
        }
        let (mime, data) = data
            .split_once(";base64,")
            .ok_or_else(|| unsupported("part.image.data_url"))?;
        if !mime.starts_with("image/") {
            return Err(unsupported("part.media_type"));
        }
        Ok(Part::InlineImage {
            mime: mime.to_owned(),
            data: data.to_owned(),
        })
    } else {
        Ok(Part::Image {
            url: url.to_owned(),
            detail,
        })
    }
}

pub(super) fn encode_parts(protocol: Protocol, role: Role, parts: &[Part]) -> Result<Vec<Value>> {
    let mut out = Vec::new();
    for part in parts {
        let value = match (protocol, part) {
            (Protocol::OpenAiChat, Part::Text(text)) => json!({"type":"text","text":text}),
            (Protocol::OpenAiResponses, Part::Text(text)) => {
                json!({"type":if role == Role::Assistant {"output_text"} else {"input_text"},"text":text})
            }
            (Protocol::AnthropicMessages, Part::Text(text)) => json!({"type":"text","text":text}),
            (Protocol::Gemini, Part::Text(text)) => json!({"text":text}),
            (Protocol::OpenAiChat, Part::Image { url, detail }) => {
                let mut value = json!({"type":"image_url","image_url":{"url":url}});
                if let Some(detail) = detail {
                    value["image_url"]["detail"] = json!(detail);
                }
                value
            }
            (Protocol::OpenAiResponses, Part::Image { url, detail }) => {
                let mut value = json!({"type":"input_image","image_url":url});
                if let Some(detail) = detail {
                    value["detail"] = json!(detail);
                }
                value
            }
            (Protocol::AnthropicMessages, Part::Image { url, detail }) => {
                if detail.as_deref().is_some_and(|s| s != "auto") {
                    return Err(unsupported("part.image.detail"));
                }
                json!({"type":"image","source":{"type":"url","url":url}})
            }
            (Protocol::Gemini, Part::Image { .. }) => {
                return Err(unsupported("part.image.remote_url"));
            }
            (Protocol::OpenAiChat, Part::InlineImage { mime, data }) => {
                json!({"type":"image_url","image_url":{"url":format!("data:{mime};base64,{data}")}})
            }
            (Protocol::OpenAiResponses, Part::InlineImage { mime, data }) => {
                json!({"type":"input_image","image_url":format!("data:{mime};base64,{data}")})
            }
            (Protocol::AnthropicMessages, Part::InlineImage { mime, data }) => {
                json!({"type":"image","source":{"type":"base64","media_type":mime,"data":data}})
            }
            (Protocol::Gemini, Part::InlineImage { mime, data }) => {
                json!({"inlineData":{"mimeType":mime,"data":data}})
            }
            (
                Protocol::AnthropicMessages,
                Part::Call {
                    id,
                    name,
                    arguments,
                },
            ) => {
                json!({"type":"tool_use","id":id.as_deref().ok_or_else(|| unsupported("tool_call.missing_id"))?,"name":name,"input":argument_object(arguments)?})
            }
            (
                Protocol::Gemini,
                Part::Call {
                    id,
                    name,
                    arguments,
                },
            ) => {
                let mut value =
                    json!({"functionCall":{"name":name,"args":argument_object(arguments)?}});
                if let Some(id) = id {
                    value["functionCall"]["id"] = json!(id);
                }
                value
            }
            (
                Protocol::AnthropicMessages,
                Part::Result {
                    id,
                    output,
                    is_error,
                    ..
                },
            ) => {
                json!({"type":"tool_result","tool_use_id":id.as_deref().ok_or_else(|| unsupported("tool_result.missing_id"))?,"content":tool_output_text(output)?,"is_error":is_error})
            }
            (
                Protocol::Gemini,
                Part::Result {
                    id,
                    name,
                    output,
                    is_error,
                },
            ) => {
                if *is_error {
                    return Err(unsupported("tool_result.is_error"));
                }
                let mut value = json!({"functionResponse":{"name":name.as_deref().ok_or_else(|| unsupported("tool_result.missing_name"))?,"response":if output.is_object() {output.clone()} else {json!({"output":output})}}});
                if let Some(id) = id {
                    value["functionResponse"]["id"] = json!(id);
                }
                value
            }
            (
                Protocol::Gemini,
                Part::Reasoning {
                    text,
                    signature: None,
                },
            ) => json!({"text":text,"thought":true}),
            (Protocol::OpenAiResponses, Part::Refusal(text)) => {
                json!({"type":"refusal","refusal":text})
            }
            (_, Part::Reasoning { .. }) => return Err(unsupported("part.reasoning")),
            (_, Part::Refusal(_)) => return Err(unsupported("part.refusal")),
            _ => return Err(unsupported("part.type")),
        };
        out.push(value);
    }
    Ok(out)
}

fn decode_tools(protocol: Protocol, tools: Option<&Value>) -> Result<Vec<Tool>> {
    let mut out = Vec::new();
    let Some(tools) = tools.filter(|v| !v.is_null()) else {
        return Ok(out);
    };
    for tool in array(tools, "request.tools")? {
        let functions: Vec<&Value> = match protocol {
            Protocol::OpenAiChat => {
                fields(tool, &["type", "function"], "tool")?;
                if text(tool, "type", "tool.type")? != "function" {
                    return Err(unsupported("tool.type"));
                }
                vec![required(tool, "function", "tool.function")?]
            }
            Protocol::Gemini => {
                fields(tool, &["functionDeclarations"], "tool")?;
                array(
                    required(tool, "functionDeclarations", "tool.functionDeclarations")?,
                    "tool.functionDeclarations",
                )?
                .iter()
                .collect()
            }
            _ => vec![tool],
        };
        for function in functions {
            let schema_key = match protocol {
                Protocol::AnthropicMessages => "input_schema",
                Protocol::Gemini => "parametersJsonSchema",
                _ => "parameters",
            };
            let allowed: &[&str] = match protocol {
                Protocol::OpenAiChat => &["name", "description", "parameters", "strict"],
                Protocol::OpenAiResponses => {
                    &["type", "name", "description", "parameters", "strict"]
                }
                Protocol::AnthropicMessages => &["name", "description", "input_schema"],
                Protocol::Gemini => &["name", "description", "parametersJsonSchema"],
            };
            fields(function, allowed, "tool.function")?;
            if protocol == Protocol::OpenAiResponses
                && text(function, "type", "tool.type")? != "function"
            {
                return Err(unsupported("tool.type"));
            }
            let schema = function
                .get(schema_key)
                .cloned()
                .ok_or_else(|| unsupported("tool.implicit_schema"))?;
            object(&schema, "tool.schema")?;
            out.push(Tool {
                name: text(function, "name", "tool.name")?.to_owned(),
                description: optional_string(function, "description", "tool.description")?,
                schema,
                strict: {
                    if protocol == Protocol::OpenAiResponses
                        && function.get("strict").is_none_or(Value::is_null)
                    {
                        return Err(unsupported("tool.responses_implicit_strict"));
                    }
                    function
                        .get("strict")
                        .filter(|x| !x.is_null())
                        .map(|x| {
                            x.as_bool()
                                .ok_or_else(|| fail(ErrorKind::InvalidInput, "tool.strict"))
                        })
                        .transpose()?
                },
            });
        }
    }
    Ok(out)
}

fn decode_choice(protocol: Protocol, body: &Value) -> Result<Option<Choice>> {
    let choice = if protocol == Protocol::Gemini {
        let Some(config) = body.get("toolConfig") else {
            return Ok(None);
        };
        fields(config, &["functionCallingConfig"], "request.toolConfig")?;
        let config = required(
            config,
            "functionCallingConfig",
            "request.functionCallingConfig",
        )?;
        fields(
            config,
            &["mode", "allowedFunctionNames"],
            "request.functionCallingConfig",
        )?;
        let mode = text(config, "mode", "request.tool_choice")?;
        if let Some(names) = config.get("allowedFunctionNames") {
            let names = array(names, "request.tool_choice.names")?;
            if mode != "ANY" || names.len() != 1 {
                return Err(unsupported("request.tool_choice.names"));
            }
            return Ok(Some(Choice::Named(
                string(&names[0], "request.tool_choice.name")?.to_owned(),
            )));
        }
        return Ok(Some(match mode {
            "AUTO" => Choice::Auto,
            "NONE" => Choice::None,
            "ANY" => Choice::Required,
            _ => return Err(unsupported("request.tool_choice.mode")),
        }));
    } else {
        let Some(choice) = body.get("tool_choice").filter(|v| !v.is_null()) else {
            return Ok(None);
        };
        choice
    };
    if let Some(value) = choice.as_str() {
        return Ok(Some(match value {
            "auto" => Choice::Auto,
            "none" => Choice::None,
            "required" => Choice::Required,
            _ => return Err(unsupported("request.tool_choice")),
        }));
    }
    if protocol == Protocol::AnthropicMessages {
        fields(choice, &["type", "name"], "request.tool_choice")?;
        if choice.get("type") != Some(&json!("tool")) && choice.get("name").is_some() {
            return Err(unsupported("request.tool_choice.name"));
        }
        Ok(Some(
            match text(choice, "type", "request.tool_choice.type")? {
                "auto" => Choice::Auto,
                "none" => Choice::None,
                "any" => Choice::Required,
                "tool" => {
                    Choice::Named(text(choice, "name", "request.tool_choice.name")?.to_owned())
                }
                _ => return Err(unsupported("request.tool_choice")),
            },
        ))
    } else {
        fields(
            choice,
            if protocol == Protocol::OpenAiChat {
                &["type", "function"]
            } else {
                &["type", "name"]
            },
            "request.tool_choice",
        )?;
        if text(choice, "type", "request.tool_choice.type")? != "function" {
            return Err(unsupported("request.tool_choice"));
        }
        let name = if protocol == Protocol::OpenAiChat {
            let function = required(choice, "function", "request.tool_choice.function")?;
            fields(function, &["name"], "request.tool_choice.function")?;
            text(function, "name", "request.tool_choice.name")?
        } else {
            text(choice, "name", "request.tool_choice.name")?
        };
        Ok(Some(Choice::Named(name.to_owned())))
    }
}

fn encode(protocol: Protocol, mut request: Request, model: &str) -> Result<Value> {
    // Resolve names from call IDs only; never guess a function name from output.
    let calls: Vec<(String, String)> = request
        .messages
        .iter()
        .flat_map(|m| &m.parts)
        .filter_map(|p| {
            if let Part::Call {
                id: Some(id), name, ..
            } = p
            {
                Some((id.clone(), name.clone()))
            } else {
                None
            }
        })
        .collect();
    for message in &mut request.messages {
        for part in &mut message.parts {
            if let Part::Result {
                id: Some(id), name, ..
            } = part
            {
                if name.is_none() {
                    let matching: Vec<_> =
                        calls.iter().filter(|(call_id, _)| call_id == id).collect();
                    if matching.len() == 1 {
                        *name = Some(matching[0].1.clone());
                    }
                }
            }
        }
    }
    let mut output = if protocol == Protocol::Gemini {
        json!({"contents":[]})
    } else {
        json!({"model":model})
    };
    let mut messages = Vec::new();
    let mut system = Vec::new();
    let mut seen_conversation = false;
    for message in &request.messages {
        if matches!(protocol, Protocol::OpenAiChat | Protocol::OpenAiResponses)
            && message
                .parts
                .iter()
                .any(|p| matches!(p, Part::Result { .. }))
            && message
                .parts
                .iter()
                .any(|p| !matches!(p, Part::Result { .. }))
        {
            return Err(unsupported("message.mixed_tool_results_and_content"));
        }
        if matches!(protocol, Protocol::AnthropicMessages | Protocol::Gemini) {
            if message.role == Role::Developer {
                return Err(unsupported("message.developer_role"));
            }
            if message.role == Role::System {
                if seen_conversation || message.parts.iter().any(|p| !matches!(p, Part::Text(_))) {
                    return Err(unsupported("message.system_position_or_content"));
                }
                system.extend(encode_parts(protocol, message.role, &message.parts)?);
                continue;
            }
            seen_conversation = true;
        }
        match protocol {
            Protocol::OpenAiChat => {
                let mut content = Vec::new();
                let mut calls = Vec::new();
                let mut refusal = None;
                for part in &message.parts {
                    match part {
                        Part::Call {
                            id,
                            name,
                            arguments,
                        } => {
                            if message.role != Role::Assistant {
                                return Err(unsupported("tool_call.role"));
                            }
                            calls.push(json!({"id":id.as_deref().ok_or_else(|| unsupported("tool_call.missing_id"))?,"type":"function","function":{"name":name,"arguments":arguments}}));
                        }
                        Part::Result {
                            id,
                            output,
                            is_error,
                            ..
                        } => {
                            if *is_error {
                                return Err(unsupported("tool_result.is_error"));
                            }
                            messages.push(json!({"role":"tool","tool_call_id":id.as_deref().ok_or_else(|| unsupported("tool_result.missing_id"))?,"content":tool_output_text(output)?}));
                        }
                        Part::Refusal(text) => {
                            if refusal.is_some() {
                                return Err(unsupported("message.multiple_refusals"));
                            }
                            refusal = Some(text);
                        }
                        _ => content.extend(encode_parts(
                            protocol,
                            message.role,
                            std::slice::from_ref(part),
                        )?),
                    }
                }
                if !content.is_empty()
                    || !calls.is_empty()
                    || refusal.is_some()
                    || message.parts.is_empty()
                {
                    let mut value = json!({"role":role_name(message.role),"content":if content.is_empty() {Value::Null} else {json!(content)}});
                    if !calls.is_empty() {
                        value["tool_calls"] = json!(calls);
                    }
                    if let Some(refusal) = refusal {
                        value["refusal"] = json!(refusal);
                    }
                    messages.push(value);
                }
            }
            Protocol::OpenAiResponses => {
                let mut content = Vec::new();
                for part in &message.parts {
                    match part {
                        Part::Call {
                            id,
                            name,
                            arguments,
                        } => {
                            flush_response_message(&mut messages, message.role, &mut content);
                            messages.push(json!({"type":"function_call","call_id":id.as_deref().ok_or_else(|| unsupported("tool_call.missing_id"))?,"name":name,"arguments":arguments}));
                        }
                        Part::Result {
                            id,
                            output,
                            is_error,
                            ..
                        } => {
                            if *is_error {
                                return Err(unsupported("tool_result.is_error"));
                            }
                            flush_response_message(&mut messages, message.role, &mut content);
                            messages.push(json!({"type":"function_call_output","call_id":id.as_deref().ok_or_else(|| unsupported("tool_result.missing_id"))?,"output":tool_output_text(output)?}));
                        }
                        _ => content.extend(encode_parts(
                            protocol,
                            message.role,
                            std::slice::from_ref(part),
                        )?),
                    }
                }
                flush_response_message(&mut messages, message.role, &mut content);
            }
            Protocol::AnthropicMessages => {
                messages.push(json!({"role":if message.role == Role::Tool {"user"} else {role_name(message.role)},"content":encode_parts(protocol,message.role,&message.parts)?}));
            }
            Protocol::Gemini => {
                messages.push(json!({"role":if message.role == Role::Assistant {"model"} else {"user"},"parts":encode_parts(protocol,message.role,&message.parts)?}));
            }
        }
    }
    output[match protocol {
        Protocol::OpenAiResponses => "input",
        Protocol::Gemini => "contents",
        _ => "messages",
    }] = json!(messages);
    if !system.is_empty() {
        if protocol == Protocol::Gemini {
            output["systemInstruction"] = json!({"parts":system});
        } else {
            output["system"] = json!(system);
        }
    }
    if protocol == Protocol::Gemini {
        // Streaming is selected by the URL method, not a JSON body field.
        let mut config = json!({});
        put_optional(&mut config, "temperature", request.temperature);
        put_optional(&mut config, "topP", request.top_p);
        put_optional(&mut config, "maxOutputTokens", request.max_output);
        put_optional(&mut config, "stopSequences", stop_array(request.stop)?);
        if config != json!({}) {
            output["generationConfig"] = config;
        }
        if request.effort.is_some() || request.parallel.is_some() {
            return Err(unsupported("request.reasoning_or_parallel"));
        }
    } else {
        put_optional(&mut output, "stream", request.stream);
        put_optional(&mut output, "temperature", request.temperature);
        put_optional(&mut output, "top_p", request.top_p);
        put_optional(
            &mut output,
            match protocol {
                Protocol::OpenAiChat => "max_completion_tokens",
                Protocol::OpenAiResponses => "max_output_tokens",
                _ => "max_tokens",
            },
            request.max_output,
        );
        if protocol == Protocol::AnthropicMessages
            && output.get("max_tokens").is_none_or(Value::is_null)
        {
            return Err(unsupported("request.required_max_tokens"));
        }
        match protocol {
            Protocol::OpenAiChat => {
                put_optional(&mut output, "stop", request.stop);
                put_optional(&mut output, "reasoning_effort", request.effort);
                put_optional(&mut output, "parallel_tool_calls", request.parallel);
                if output.get("stream") == Some(&Value::Bool(true)) {
                    output["stream_options"] = json!({"include_usage":true});
                }
            }
            Protocol::OpenAiResponses => {
                if request
                    .stop
                    .as_ref()
                    .is_some_and(|v| !v.is_null() && v != &json!([]))
                {
                    return Err(unsupported("request.stop"));
                }
                if let Some(effort) = request.effort {
                    output["reasoning"] = json!({"effort":effort});
                }
                put_optional(&mut output, "parallel_tool_calls", request.parallel);
            }
            Protocol::AnthropicMessages => {
                put_optional(&mut output, "stop_sequences", stop_array(request.stop)?);
                if request.effort.is_some() || request.parallel.is_some() {
                    return Err(unsupported("request.reasoning_or_parallel"));
                }
            }
            Protocol::Gemini => unreachable!("handled above"),
        }
    }
    if !request.tools.is_empty() {
        let mut tools = Vec::new();
        for tool in &request.tools {
            if tool.strict == Some(true)
                && matches!(protocol, Protocol::Gemini | Protocol::AnthropicMessages)
            {
                return Err(unsupported("tool.strict"));
            }
            let mut value = json!({"name":tool.name});
            if let Some(description) = &tool.description {
                value["description"] = json!(description);
            }
            value[match protocol {
                Protocol::AnthropicMessages => "input_schema",
                Protocol::Gemini => "parametersJsonSchema",
                _ => "parameters",
            }] = tool.schema.clone();
            if matches!(protocol, Protocol::OpenAiChat | Protocol::OpenAiResponses) {
                // Responses defaults to strict normalization, whereas ordinary
                // tools in Chat/Messages/Gemini do not request strict mode.
                value["strict"] = json!(tool.strict.unwrap_or(false));
            }
            tools.push(match protocol {
                Protocol::OpenAiChat => json!({"type":"function","function":value}),
                Protocol::OpenAiResponses => {
                    value["type"] = json!("function");
                    value
                }
                _ => value,
            });
        }
        output["tools"] = if protocol == Protocol::Gemini {
            json!([{"functionDeclarations":tools}])
        } else {
            json!(tools)
        };
    }
    if let Some(choice) = request.tool_choice {
        if protocol == Protocol::Gemini {
            output["toolConfig"] = json!({"functionCallingConfig": match choice {
                Choice::Auto => json!({"mode":"AUTO"}), Choice::None => json!({"mode":"NONE"}),
                Choice::Required => json!({"mode":"ANY"}), Choice::Named(name) => json!({"mode":"ANY","allowedFunctionNames":[name]}),
            }});
        } else {
            output["tool_choice"] = match (protocol, choice) {
                (Protocol::AnthropicMessages, Choice::Auto) => json!({"type":"auto"}),
                (Protocol::AnthropicMessages, Choice::None) => json!({"type":"none"}),
                (Protocol::AnthropicMessages, Choice::Required) => json!({"type":"any"}),
                (Protocol::AnthropicMessages, Choice::Named(name)) => {
                    json!({"type":"tool","name":name})
                }
                (_, Choice::Auto) => json!("auto"),
                (_, Choice::None) => json!("none"),
                (_, Choice::Required) => json!("required"),
                (Protocol::OpenAiChat, Choice::Named(name)) => {
                    json!({"type":"function","function":{"name":name}})
                }
                (_, Choice::Named(name)) => json!({"type":"function","name":name}),
            };
        }
    }
    Ok(output)
}

fn flush_response_message(out: &mut Vec<Value>, role: Role, content: &mut Vec<Value>) {
    if !content.is_empty() {
        out.push(
            json!({"type":"message","role":role_name(role),"content":std::mem::take(content)}),
        );
    }
}

fn stop_array(value: Option<Value>) -> Result<Option<Value>> {
    match value {
        Some(Value::String(text)) => Ok(Some(json!([text]))),
        Some(Value::Null) | None => Ok(None),
        Some(value) => {
            array(&value, "request.stop")?;
            Ok(Some(value))
        }
    }
}

fn tool_output_text(output: &Value) -> Result<String> {
    if let Some(text) = output.as_str() {
        return Ok(text.to_owned());
    }
    // Only the envelope produced above has a lossless text representation.
    if let Some(object) = output.as_object() {
        if object.len() == 1 {
            if let Some(text) = object.get("output").and_then(Value::as_str) {
                return Ok(text.to_owned());
            }
        }
    }
    Err(unsupported("tool_result.structured_output"))
}
