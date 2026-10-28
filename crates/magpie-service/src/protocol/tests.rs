//! All fixtures are synthetic JSON/bytes. No files, accounts, sockets, or clocks.

use super::*;
use serde_json::{Value, json};

const PROTOCOLS: [Protocol; 4] = [
    Protocol::OpenAiChat,
    Protocol::OpenAiResponses,
    Protocol::AnthropicMessages,
    Protocol::Gemini,
];
const ARGUMENTS: &str = r#"{"city":"中","n":9007199254740993}"#;
const TEXT: &str = "\u{4e2d}\u{1f642}";

fn context() -> ResponseContext {
    ResponseContext {
        id: "fixture_response".to_owned(),
        model: "fixture/model".to_owned(),
        created: 1_700_000_000,
    }
}

fn request_fixture(protocol: Protocol) -> Value {
    let schema = json!({"type":"object","properties":{"city":{"type":"string"},"n":{"type":"integer"}},"required":["city","n"]});
    match protocol {
        Protocol::OpenAiChat => json!({
            "model":"fixture","messages":[
                {"role":"system","content":"system fixture"},
                {"role":"user","content":TEXT},
                {"role":"assistant","content":null,"tool_calls":[{"id":"call_fixture","type":"function","function":{"name":"weather","arguments":ARGUMENTS}}]},
                {"role":"tool","tool_call_id":"call_fixture","content":"tool fixture"}
            ],
            "tools":[{"type":"function","function":{"name":"weather","parameters":schema,"strict":false}}],
            "tool_choice":"auto","max_completion_tokens":128,"temperature":0.25
        }),
        Protocol::OpenAiResponses => json!({
            "model":"fixture","input":[
                {"role":"system","content":"system fixture"},
                {"role":"user","content":TEXT},
                {"type":"function_call","call_id":"call_fixture","name":"weather","arguments":ARGUMENTS},
                {"type":"function_call_output","call_id":"call_fixture","output":"tool fixture"}
            ],
            "tools":[{"type":"function","name":"weather","parameters":schema,"strict":false}],
            "tool_choice":"auto","max_output_tokens":128,"temperature":0.25
        }),
        Protocol::AnthropicMessages => json!({
            "model":"fixture","system":"system fixture","messages":[
                {"role":"user","content":TEXT},
                {"role":"assistant","content":[{"type":"tool_use","id":"call_fixture","name":"weather","input":serde_json::from_str::<Value>(ARGUMENTS).expect("synthetic arguments are valid")}]},
                {"role":"user","content":[{"type":"tool_result","tool_use_id":"call_fixture","content":"tool fixture"}]}
            ],
            "tools":[{"name":"weather","input_schema":schema}],"tool_choice":{"type":"auto"},"max_tokens":128,"temperature":0.25
        }),
        Protocol::Gemini => json!({
            "systemInstruction":{"parts":[{"text":"system fixture"}]},
            "contents":[
                {"role":"user","parts":[{"text":TEXT}]},
                {"role":"model","parts":[{"functionCall":{"id":"call_fixture","name":"weather","args":serde_json::from_str::<Value>(ARGUMENTS).expect("synthetic arguments are valid")}}]},
                {"role":"user","parts":[{"functionResponse":{"id":"call_fixture","name":"weather","response":{"output":"tool fixture"}}}]}
            ],
            "tools":[{"functionDeclarations":[{"name":"weather","parametersJsonSchema":schema}]}],
            "toolConfig":{"functionCallingConfig":{"mode":"AUTO"}},
            "generationConfig":{"maxOutputTokens":128,"temperature":0.25}
        }),
    }
}

fn response_fixture(protocol: Protocol) -> Value {
    match protocol {
        Protocol::OpenAiChat => json!({
            "id":"fixture","object":"chat.completion","created":1700000000,"model":"fixture",
            "choices":[{"index":0,"message":{"role":"assistant","content":TEXT,"tool_calls":[{"id":"call_fixture","type":"function","function":{"name":"weather","arguments":ARGUMENTS}}]},"finish_reason":"tool_calls"}],
            "usage":{"prompt_tokens":7,"completion_tokens":3,"total_tokens":10}
        }),
        Protocol::OpenAiResponses => response_terminal(),
        Protocol::AnthropicMessages => json!({
            "id":"fixture","type":"message","role":"assistant","model":"fixture",
            "content":[{"type":"text","text":TEXT},{"type":"tool_use","id":"call_fixture","name":"weather","input":serde_json::from_str::<Value>(ARGUMENTS).expect("synthetic arguments are valid")}],
            "stop_reason":"tool_use","stop_sequence":null,"usage":{"input_tokens":7,"output_tokens":3}
        }),
        Protocol::Gemini => json!({
            "responseId":"fixture","modelVersion":"fixture",
            "candidates":[{"index":0,"content":{"role":"model","parts":[{"text":TEXT},{"functionCall":{"id":"call_fixture","name":"weather","args":serde_json::from_str::<Value>(ARGUMENTS).expect("synthetic arguments are valid")}}]},"finishReason":"STOP"}],
            "usageMetadata":{"promptTokenCount":7,"candidatesTokenCount":3,"totalTokenCount":10}
        }),
    }
}

fn message_item() -> Value {
    json!({"id":"item_text","type":"message","status":"completed","role":"assistant","content":[{"type":"output_text","text":TEXT,"annotations":[]}]})
}

fn call_item() -> Value {
    json!({"id":"item_call","type":"function_call","status":"completed","call_id":"call_fixture","name":"weather","arguments":ARGUMENTS})
}

fn response_terminal() -> Value {
    json!({"id":"fixture","object":"response","created_at":1700000000,"model":"fixture","status":"completed","output":[message_item(),call_item()],"error":null,"incomplete_details":null,"usage":{"input_tokens":7,"output_tokens":3,"total_tokens":10}})
}

fn arguments_in_request(protocol: Protocol, value: &Value) -> Value {
    match protocol {
        Protocol::OpenAiChat => {
            let message = value["messages"]
                .as_array()
                .expect("messages array")
                .iter()
                .find(|m| m.get("tool_calls").is_some())
                .expect("tool call");
            serde_json::from_str(
                message["tool_calls"][0]["function"]["arguments"]
                    .as_str()
                    .expect("arguments string"),
            )
            .expect("arguments object")
        }
        Protocol::OpenAiResponses => {
            let item = value["input"]
                .as_array()
                .expect("input array")
                .iter()
                .find(|i| i["type"] == "function_call")
                .expect("call item");
            serde_json::from_str(item["arguments"].as_str().expect("arguments string"))
                .expect("arguments object")
        }
        Protocol::AnthropicMessages => {
            let message = value["messages"]
                .as_array()
                .expect("messages array")
                .iter()
                .find(|m| m["role"] == "assistant")
                .expect("assistant message");
            message["content"][0]["input"].clone()
        }
        Protocol::Gemini => value["contents"][1]["parts"][0]["functionCall"]["args"].clone(),
    }
}

#[test]
fn requests_cover_all_twelve_directed_pairs_with_tools_and_large_integers() {
    let expected: Value = serde_json::from_str(ARGUMENTS).expect("synthetic arguments");
    for source in PROTOCOLS {
        for target in PROTOCOLS {
            if source == target {
                continue;
            }
            let got = convert_request(source, target, &request_fixture(source), "upstream_fixture")
                .unwrap_or_else(|e| panic!("{source:?}->{target:?}: {e}"));
            assert_eq!(
                arguments_in_request(target, &got),
                expected,
                "{source:?}->{target:?}"
            );
            assert_eq!(
                arguments_in_request(target, &got)["n"].to_string(),
                "9007199254740993",
                "JSON integer must not pass through f64"
            );
            if target == Protocol::Gemini {
                assert!(got.get("model").is_none());
            } else {
                assert_eq!(got["model"], "upstream_fixture");
            }
            let round_trip = convert_request(target, source, &got, "return_fixture")
                .unwrap_or_else(|e| panic!("{source:?}->{target:?}->{source:?}: {e}"));
            assert_eq!(arguments_in_request(source, &round_trip), expected);
            if source == Protocol::AnthropicMessages && target == Protocol::Gemini {
                assert_eq!(
                    round_trip["messages"][2]["content"][0]["content"],
                    "tool fixture"
                );
            }
        }
    }
}

#[test]
fn openai_pairs_preserve_argument_string_bytes() {
    let arguments = " { \"n\" : 9007199254740993, \"s\" : \"\\u4e2d\" } ";
    let mut request = request_fixture(Protocol::OpenAiChat);
    request["messages"][2]["tool_calls"][0]["function"]["arguments"] = json!(arguments);
    let got = convert_request(
        Protocol::OpenAiChat,
        Protocol::OpenAiResponses,
        &request,
        "fixture",
    )
    .expect("supported pair");
    assert_eq!(got["input"][2]["arguments"], arguments);
    let returned = convert_request(
        Protocol::OpenAiResponses,
        Protocol::OpenAiChat,
        &got,
        "fixture",
    )
    .expect("supported reverse");
    assert_eq!(
        returned["messages"][2]["tool_calls"][0]["function"]["arguments"],
        arguments
    );
}

#[test]
fn developer_role_is_preserved_between_openai_endpoints_and_rejected_elsewhere() {
    let request = json!({"model":"fixture","messages":[{"role":"developer","content":"do not merge my role"},{"role":"user","content":"fixture"}],"max_completion_tokens":10});
    let got = convert_request(
        Protocol::OpenAiChat,
        Protocol::OpenAiResponses,
        &request,
        "fixture",
    )
    .expect("developer has a native equivalent");
    assert_eq!(got["input"][0]["role"], "developer");
    for target in [Protocol::AnthropicMessages, Protocol::Gemini] {
        assert_eq!(
            convert_request(Protocol::OpenAiChat, target, &request, "fixture")
                .expect_err("developer cannot be silently merged")
                .location,
            "message.developer_role"
        );
    }
}

#[test]
fn requests_reject_unknown_fields_at_root_and_inside_parts() {
    let mut root = request_fixture(Protocol::OpenAiChat);
    root["fixture_unknown"] = json!({"private":"do not reflect"});
    let error = convert_request(Protocol::OpenAiChat, Protocol::Gemini, &root, "fixture")
        .expect_err("unknown root field");
    assert_eq!(error.kind, ErrorKind::UnsupportedField);
    assert!(!error.to_string().contains("private"));
    let mut nested = request_fixture(Protocol::OpenAiChat);
    nested["messages"][1]["content"] =
        json!([{"type":"text","text":"fixture","cache_control":{"type":"ephemeral"}}]);
    assert_eq!(
        convert_request(Protocol::OpenAiChat, Protocol::Gemini, &nested, "fixture")
            .expect_err("unknown part field")
            .kind,
        ErrorKind::UnsupportedField
    );
}

#[test]
fn native_requests_preserve_extensions_and_replace_only_model() {
    let body = json!({"model":"old","fixture_extension":{"opaque":[1,2,3]}});
    let got = convert_request(Protocol::OpenAiChat, Protocol::OpenAiChat, &body, "new")
        .expect("native passthrough");
    assert_eq!(
        got,
        json!({"model":"new","fixture_extension":{"opaque":[1,2,3]}})
    );
}

#[test]
fn inline_image_request_round_trips_across_every_pair() {
    let fixtures = [
        json!({"model":"fixture","messages":[{"role":"user","content":[{"type":"image_url","image_url":{"url":"data:image/png;base64,AQID"}}]}],"max_completion_tokens":10}),
        json!({"model":"fixture","input":[{"role":"user","content":[{"type":"input_image","image_url":"data:image/png;base64,AQID"}]}],"max_output_tokens":10}),
        json!({"model":"fixture","messages":[{"role":"user","content":[{"type":"image","source":{"type":"base64","media_type":"image/png","data":"AQID"}}]}],"max_tokens":10}),
        json!({"contents":[{"role":"user","parts":[{"inlineData":{"mimeType":"image/png","data":"AQID"}}]}],"generationConfig":{"maxOutputTokens":10}}),
    ];
    for (index, source) in PROTOCOLS.into_iter().enumerate() {
        for target in PROTOCOLS {
            let got = convert_request(source, target, &fixtures[index], "fixture")
                .expect("image format mapping");
            assert!(got.to_string().contains("AQID"));
            let returned =
                convert_request(target, source, &got, "fixture").expect("reverse image mapping");
            assert!(returned.to_string().contains("image/png"));
        }
    }
}

#[test]
fn signature_reasoning_and_external_media_never_become_plain_text() {
    let mut fixture = request_fixture(Protocol::Gemini);
    fixture["contents"][0]["parts"] =
        json!([{"text":"fixture thought","thought":true,"thoughtSignature":"fixture signature"}]);
    assert_eq!(
        convert_request(
            Protocol::Gemini,
            Protocol::OpenAiResponses,
            &fixture,
            "fixture"
        )
        .expect_err("signature has no transferable encoding")
        .kind,
        ErrorKind::UnsupportedSemantics
    );
    fixture["contents"][0]["parts"] =
        json!([{"inlineData":{"mimeType":"audio/wav","data":"AQID"}}]);
    assert_eq!(
        convert_request(
            Protocol::Gemini,
            Protocol::OpenAiResponses,
            &fixture,
            "fixture"
        )
        .expect_err("audio has no declared mapping")
        .kind,
        ErrorKind::UnsupportedSemantics
    );
}

#[test]
fn missing_anthropic_output_limit_and_responses_stop_are_explicit_errors() {
    let no_limit = json!({"model":"fixture","messages":[{"role":"user","content":"fixture"}]});
    assert_eq!(
        convert_request(
            Protocol::OpenAiChat,
            Protocol::AnthropicMessages,
            &no_limit,
            "fixture"
        )
        .expect_err("no invented output cap")
        .location,
        "request.required_max_tokens"
    );
    let mut stop = request_fixture(Protocol::OpenAiChat);
    stop["stop"] = json!(["STOP"]);
    assert_eq!(
        convert_request(
            Protocol::OpenAiChat,
            Protocol::OpenAiResponses,
            &stop,
            "fixture"
        )
        .expect_err("stop not silently removed")
        .location,
        "request.stop"
    );
}

#[test]
fn responses_cover_all_twelve_directed_pairs_and_one_usage_observation() {
    for source in PROTOCOLS {
        for target in PROTOCOLS {
            if source == target {
                continue;
            }
            let got = convert_response(source, target, &response_fixture(source), &context())
                .unwrap_or_else(|e| panic!("{source:?}->{target:?}: {e}"));
            assert_eq!(got.outcome, Outcome::Finished(FinishReason::ToolCalls));
            let usage = got.usage.as_ref().expect("fixture has usage");
            assert_eq!(
                (usage.input_tokens, usage.output_tokens, usage.total_tokens),
                (Some(7), Some(3), Some(10))
            );
            assert!(got.body.to_string().contains("9007199254740993"));
            let reversed = convert_response(target, source, &got.body, &context())
                .expect("reverse response mapping");
            assert_eq!(reversed.usage, got.usage);
        }
    }
}

#[test]
fn cache_and_thought_usage_include_provider_specific_token_semantics() {
    let anthropic = super::model::Usage::decode(Protocol::AnthropicMessages, &json!({"input_tokens":4,"cache_read_input_tokens":2,"cache_creation_input_tokens":1,"output_tokens":3})).expect("cache usage");
    assert_eq!(
        (anthropic.input_tokens, anthropic.total_tokens),
        (Some(7), Some(10))
    );
    assert_eq!(
        anthropic
            .encode(Protocol::OpenAiChat)
            .expect_err("cache creation has no Chat slot")
            .location,
        "usage.cache_creation_input_tokens"
    );
    let gemini = super::model::Usage::decode(Protocol::Gemini, &json!({"promptTokenCount":7,"candidatesTokenCount":3,"thoughtsTokenCount":2,"totalTokenCount":12})).expect("thought usage");
    assert_eq!(gemini.output_tokens, Some(5));
    let chat = gemini
        .encode(Protocol::OpenAiChat)
        .expect("reasoning counter mapping");
    assert_eq!(chat["completion_tokens"], 5);
    assert_eq!(chat["completion_tokens_details"]["reasoning_tokens"], 2);
}

#[test]
fn absent_usage_is_unknown_and_never_zero() {
    let mut source = response_fixture(Protocol::OpenAiChat);
    source
        .as_object_mut()
        .expect("fixture object")
        .remove("usage");
    let got = convert_response(Protocol::OpenAiChat, Protocol::Gemini, &source, &context())
        .expect("Gemini can omit usage");
    assert!(got.usage.is_none());
    assert!(got.body.get("usageMetadata").is_none());
    assert_eq!(
        convert_response(
            Protocol::OpenAiChat,
            Protocol::AnthropicMessages,
            &source,
            &context()
        )
        .expect_err("Messages requires counts")
        .location,
        "usage.required_anthropic_counts"
    );
}

#[test]
fn safe_errors_keep_categories_without_reflecting_upstream_text() {
    for target in PROTOCOLS {
        let input = json!({"error":{"type":"rate_limit_error","message":"fixture-secret prompt and response"}});
        let got = convert_response(Protocol::AnthropicMessages, target, &input, &context())
            .expect("safe error mapping");
        assert_eq!(got.outcome, Outcome::Failed(ErrorCategory::RateLimit));
        assert!(!got.body.to_string().contains("fixture-secret"));
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
        json!({"id":"fixture","object":"chat.completion.chunk","created":1700000000,"model":"fixture","choices":[{"index":0,"delta":delta,"finish_reason":finish}]}),
    )
}

fn stream_fixture(protocol: Protocol) -> Vec<u8> {
    let mut bytes = Vec::new();
    match protocol {
        Protocol::OpenAiChat => {
            bytes.extend(chat_event(
                json!({"role":"assistant","content":""}),
                Value::Null,
            ));
            bytes.extend(chat_event(json!({"content":TEXT}), Value::Null));
            bytes.extend(chat_event(json!({"tool_calls":[{"index":0,"id":"call_fixture","type":"function","function":{"name":"weather","arguments":"{\"city\":\""}}]}), Value::Null));
            bytes.extend(chat_event(
                json!({"tool_calls":[{"index":0,"function":{"arguments":"中"}}]}),
                Value::Null,
            ));
            bytes.extend(chat_event(json!({"tool_calls":[{"index":0,"function":{"arguments":"\",\"n\":9007199254740993}"}}]}), Value::Null));
            bytes.extend(chat_event(json!({}), json!("tool_calls")));
            // A repeated, identical total is a snapshot, not another charge.
            for _ in 0..2 {
                bytes.extend(event("", json!({"id":"fixture","object":"chat.completion.chunk","created":1700000000,"model":"fixture","choices":[],"usage":{"prompt_tokens":7,"completion_tokens":3,"total_tokens":10}})));
            }
            bytes.extend_from_slice(b"data: [DONE]\n\n");
        }
        Protocol::AnthropicMessages => {
            let events = [
                (
                    "message_start",
                    json!({"type":"message_start","message":{"id":"fixture","type":"message","role":"assistant","model":"fixture","content":[],"stop_reason":null,"stop_sequence":null,"usage":{"input_tokens":7,"output_tokens":0}}}),
                ),
                (
                    "content_block_start",
                    json!({"type":"content_block_start","index":0,"content_block":{"type":"text","text":""}}),
                ),
                (
                    "content_block_delta",
                    json!({"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":TEXT}}),
                ),
                (
                    "content_block_stop",
                    json!({"type":"content_block_stop","index":0}),
                ),
                (
                    "content_block_start",
                    json!({"type":"content_block_start","index":1,"content_block":{"type":"tool_use","id":"call_fixture","name":"weather","input":{}}}),
                ),
                (
                    "content_block_delta",
                    json!({"type":"content_block_delta","index":1,"delta":{"type":"input_json_delta","partial_json":"{\"city\":\""}}),
                ),
                (
                    "content_block_delta",
                    json!({"type":"content_block_delta","index":1,"delta":{"type":"input_json_delta","partial_json":"中"}}),
                ),
                (
                    "content_block_delta",
                    json!({"type":"content_block_delta","index":1,"delta":{"type":"input_json_delta","partial_json":"\",\"n\":9007199254740993}"}}),
                ),
                (
                    "content_block_stop",
                    json!({"type":"content_block_stop","index":1}),
                ),
                (
                    "message_delta",
                    json!({"type":"message_delta","delta":{"stop_reason":"tool_use","stop_sequence":null},"usage":{"output_tokens":3}}),
                ),
                ("message_stop", json!({"type":"message_stop"})),
            ];
            for (name, value) in events {
                bytes.extend(event(name, value));
            }
        }
        Protocol::Gemini => {
            bytes.extend(event("", json!({"responseId":"fixture","modelVersion":"fixture","candidates":[{"index":0,"content":{"role":"model","parts":[{"text":TEXT}]}}]})));
            bytes.extend(event("", json!({"responseId":"fixture","modelVersion":"fixture","candidates":[{"index":0,"content":{"role":"model","parts":[{"functionCall":{"id":"call_fixture","name":"weather","args":serde_json::from_str::<Value>(ARGUMENTS).expect("synthetic arguments")}}]},"finishReason":"STOP"}],"usageMetadata":{"promptTokenCount":7,"candidatesTokenCount":3,"totalTokenCount":10}})));
        }
        Protocol::OpenAiResponses => {
            let shell = json!({"id":"fixture","object":"response","created_at":1700000000,"model":"fixture","status":"in_progress","output":[],"error":null,"incomplete_details":null});
            let events = [
                ("response.created", json!({"response":shell})),
                ("response.in_progress", json!({"response":shell})),
                (
                    "response.output_item.added",
                    json!({"output_index":0,"item":{"id":"item_text","type":"message","role":"assistant","status":"in_progress","content":[]}}),
                ),
                (
                    "response.content_part.added",
                    json!({"item_id":"item_text","output_index":0,"content_index":0,"part":{"type":"output_text","text":"","annotations":[]}}),
                ),
                (
                    "response.output_text.delta",
                    json!({"item_id":"item_text","output_index":0,"content_index":0,"delta":TEXT}),
                ),
                (
                    "response.output_text.done",
                    json!({"item_id":"item_text","output_index":0,"content_index":0,"text":TEXT}),
                ),
                (
                    "response.content_part.done",
                    json!({"item_id":"item_text","output_index":0,"content_index":0,"part":{"type":"output_text","text":TEXT,"annotations":[]}}),
                ),
                (
                    "response.output_item.done",
                    json!({"output_index":0,"item":message_item()}),
                ),
                (
                    "response.output_item.added",
                    json!({"output_index":1,"item":{"id":"item_call","type":"function_call","call_id":"call_fixture","name":"weather","arguments":"","status":"in_progress"}}),
                ),
                (
                    "response.function_call_arguments.delta",
                    json!({"item_id":"item_call","output_index":1,"delta":"{\"city\":\""}),
                ),
                (
                    "response.function_call_arguments.delta",
                    json!({"item_id":"item_call","output_index":1,"delta":"中"}),
                ),
                (
                    "response.function_call_arguments.delta",
                    json!({"item_id":"item_call","output_index":1,"delta":"\",\"n\":9007199254740993}"}),
                ),
                (
                    "response.function_call_arguments.done",
                    json!({"item_id":"item_call","output_index":1,"arguments":ARGUMENTS}),
                ),
                (
                    "response.output_item.done",
                    json!({"output_index":1,"item":call_item()}),
                ),
                (
                    "response.completed",
                    json!({"response":response_terminal()}),
                ),
            ];
            for (sequence, (name, mut value)) in events.into_iter().enumerate() {
                value["type"] = json!(name);
                value["sequence_number"] = json!(sequence);
                bytes.extend(event(name, value));
            }
        }
    }
    bytes
}

fn decode_frames(frames: &[Vec<u8>]) -> Vec<Value> {
    frames
        .iter()
        .filter_map(|frame| {
            let text = std::str::from_utf8(frame).expect("output UTF-8");
            let data = text.lines().find_map(|line| line.strip_prefix("data: "))?;
            if data == "[DONE]" {
                return None;
            }
            Some(serde_json::from_str(data).expect("output JSON"))
        })
        .collect()
}

fn run_stream(
    source: Protocol,
    target: Protocol,
    input: &[u8],
    chunk: usize,
) -> (Vec<Vec<u8>>, StreamEnd) {
    let mut converter = SseConverter::new(source, target, context(), Limits::default())
        .expect("cross-protocol converter");
    let mut frames = Vec::new();
    let mut final_end = None;
    for part in input.chunks(chunk) {
        let batch = converter
            .push(part)
            .unwrap_or_else(|e| panic!("{source:?}->{target:?}: {e}"));
        frames.extend(batch.frames);
        if let Some(end) = batch.end {
            assert!(final_end.is_none(), "one final observation");
            final_end = Some(end);
        }
    }
    if source == Protocol::Gemini {
        assert!(
            final_end.is_none(),
            "Gemini terminal usage can follow the finished candidate"
        );
        let batch = converter
            .finish()
            .expect("EOF after Gemini semantic finish");
        frames.extend(batch.frames);
        final_end = batch.end;
    }
    assert_eq!(
        converter
            .cancel()
            .expect_err("terminal is not emitted twice")
            .kind,
        ErrorKind::Closed
    );
    (frames, final_end.expect("exactly one final observation"))
}

#[test]
fn sse_covers_twelve_pairs_and_every_utf8_byte_boundary() {
    for source in PROTOCOLS {
        let input = stream_fixture(source);
        for target in PROTOCOLS {
            if source == target {
                continue;
            }
            let (frames, end) = run_stream(source, target, &input, 1);
            assert_eq!(
                end.outcome,
                Outcome::Finished(FinishReason::ToolCalls),
                "{source:?}->{target:?}"
            );
            let usage = end.usage.expect("one final fixture usage");
            assert_eq!(
                (usage.input_tokens, usage.output_tokens, usage.total_tokens),
                (Some(7), Some(3), Some(10))
            );
            let values = decode_frames(&frames);
            let final_usage_events = values
                .iter()
                .filter(|v| match target {
                    Protocol::OpenAiChat => v.get("usage").is_some_and(|u| !u.is_null()),
                    Protocol::OpenAiResponses => matches!(
                        v["type"].as_str(),
                        Some("response.completed" | "response.incomplete")
                    ),
                    Protocol::AnthropicMessages => {
                        v["type"] == "message_delta" && v.get("usage").is_some()
                    }
                    Protocol::Gemini => v.get("usageMetadata").is_some(),
                })
                .count();
            assert_eq!(final_usage_events, 1, "{source:?}->{target:?}");
            let tool_text: String = values
                .iter()
                .filter_map(|value| match target {
                    Protocol::OpenAiChat => {
                        value["choices"][0]["delta"]["tool_calls"][0]["function"]["arguments"]
                            .as_str()
                    }
                    Protocol::OpenAiResponses => {
                        if value["type"] == "response.function_call_arguments.delta" {
                            value["delta"].as_str()
                        } else {
                            None
                        }
                    }
                    Protocol::AnthropicMessages => value["delta"]["partial_json"].as_str(),
                    Protocol::Gemini => None,
                })
                .collect();
            if target == Protocol::Gemini {
                let call = values
                    .iter()
                    .find_map(|v| v["candidates"][0]["content"]["parts"][0].get("functionCall"))
                    .expect("one completed tool call");
                assert_eq!(call["args"]["n"].to_string(), "9007199254740993");
                assert_eq!(call["args"]["city"], "中");
            } else {
                assert_eq!(tool_text, ARGUMENTS, "{source:?}->{target:?}");
            }
        }
    }
}

#[test]
fn sse_is_chunk_invariant() {
    let input = stream_fixture(Protocol::OpenAiChat);
    let expected = run_stream(Protocol::OpenAiChat, Protocol::OpenAiResponses, &input, 1);
    for size in [2, 3, 7, 31, 4096] {
        assert_eq!(
            run_stream(
                Protocol::OpenAiChat,
                Protocol::OpenAiResponses,
                &input,
                size
            ),
            expected
        );
    }
}

#[test]
fn sse_cancel_releases_state_and_cannot_report_success_or_usage() {
    let mut converter = SseConverter::new(
        Protocol::OpenAiChat,
        Protocol::Gemini,
        context(),
        Limits::default(),
    )
    .expect("converter");
    converter
        .push(&chat_event(
            json!({"content":"partial fixture"}),
            Value::Null,
        ))
        .expect("partial output");
    let end = converter.cancel().expect("cancel once");
    assert_eq!(
        end,
        StreamEnd {
            outcome: Outcome::Cancelled,
            usage: None
        }
    );
    assert_eq!(
        converter
            .finish()
            .expect_err("closed after cancellation")
            .kind,
        ErrorKind::Closed
    );
    assert_eq!(
        converter
            .push(b"data: [DONE]\n\n")
            .expect_err("no resurrection")
            .kind,
        ErrorKind::Closed
    );
}

#[test]
fn sse_eof_without_semantic_finish_is_incomplete() {
    for source in [Protocol::OpenAiChat, Protocol::Gemini] {
        let mut converter = SseConverter::new(
            source,
            Protocol::OpenAiResponses,
            context(),
            Limits::default(),
        )
        .expect("converter");
        let bytes = if source == Protocol::OpenAiChat {
            chat_event(json!({"content":"fixture"}), Value::Null)
        } else {
            event(
                "",
                json!({"candidates":[{"content":{"role":"model","parts":[{"text":"fixture"}]}}]}),
            )
        };
        converter.push(&bytes).expect("partial stream");
        assert_eq!(
            converter.finish().expect_err("EOF is not success").kind,
            ErrorKind::IncompleteStream
        );
    }
}

#[test]
fn sse_rejects_partial_event_invalid_utf8_and_oversized_input() {
    let make = || {
        SseConverter::new(
            Protocol::OpenAiChat,
            Protocol::Gemini,
            context(),
            Limits {
                max_chunk_bytes: 32,
                max_event_bytes: 64,
                ..Limits::default()
            },
        )
        .expect("converter")
    };
    let mut partial = make();
    partial.push(b"data: {").expect("partial event retained");
    assert_eq!(
        partial.finish().expect_err("truncated frame").kind,
        ErrorKind::IncompleteStream
    );
    let mut utf8 = make();
    assert_eq!(
        utf8.push(b"data: \xff\n\n")
            .expect_err("invalid UTF-8")
            .kind,
        ErrorKind::InvalidUtf8
    );
    let mut chunk = make();
    assert_eq!(
        chunk.push(&[b'x'; 33]).expect_err("chunk cap").kind,
        ErrorKind::LimitExceeded
    );
    let mut event = make();
    event.push(&[b'x'; 32]).expect("first segment");
    event.push(&[b'x'; 32]).expect("second segment");
    assert_eq!(
        event.push(b"x").expect_err("event cap across chunks").kind,
        ErrorKind::LimitExceeded
    );
}

#[test]
fn sse_rejects_incomplete_tool_json_and_unknown_events() {
    let mut converter = SseConverter::new(
        Protocol::OpenAiChat,
        Protocol::Gemini,
        context(),
        Limits::default(),
    )
    .expect("converter");
    let mut input = chat_event(
        json!({"tool_calls":[{"index":0,"id":"fixture","type":"function","function":{"name":"tool","arguments":"{\"broken\":"}}]}),
        Value::Null,
    );
    input.extend(chat_event(json!({}), json!("tool_calls")));
    input.extend_from_slice(b"data: [DONE]\n\n");
    assert_eq!(
        converter
            .push(&input)
            .expect_err("partial JSON cannot become a completed Gemini object")
            .kind,
        ErrorKind::InvalidJson
    );
    let mut unknown = SseConverter::new(
        Protocol::AnthropicMessages,
        Protocol::Gemini,
        context(),
        Limits::default(),
    )
    .expect("converter");
    assert_eq!(
        unknown
            .push(&event(
                "future_event",
                json!({"type":"future_event","fixture":1})
            ))
            .expect_err("unknown semantic event")
            .kind,
        ErrorKind::UnsupportedSemantics
    );
}

#[test]
fn sse_error_is_terminal_redacted_and_has_no_success_sentinel() {
    for target in [
        Protocol::OpenAiChat,
        Protocol::OpenAiResponses,
        Protocol::Gemini,
    ] {
        let mut converter = SseConverter::new(
            Protocol::AnthropicMessages,
            target,
            context(),
            Limits::default(),
        )
        .expect("converter");
        let got = converter.push(&event("error", json!({"type":"error","error":{"type":"overloaded_error","message":"fixture-secret prompt"}}))).expect("error conversion");
        assert_eq!(
            got.end.expect("terminal error").outcome,
            Outcome::Failed(ErrorCategory::UpstreamServer)
        );
        let bytes = String::from_utf8(got.frames.concat()).expect("UTF-8");
        assert!(!bytes.contains("fixture-secret"));
        assert!(!bytes.contains("[DONE]"));
        assert!(!bytes.contains("\"status\":\"completed\""));
    }
}

#[test]
fn nonresponses_text_stream_does_not_accumulate_full_output() {
    let limits = Limits {
        max_output_bytes: 32,
        ..Limits::default()
    };
    let mut converter =
        SseConverter::new(Protocol::OpenAiChat, Protocol::Gemini, context(), limits)
            .expect("bounded converter");
    for _ in 0..1000 {
        converter
            .push(&chat_event(json!({"content":"1234567890"}), Value::Null))
            .expect("long text is emitted incrementally");
    }
    let mut terminal = chat_event(json!({}), json!("stop"));
    terminal.extend_from_slice(b"data: [DONE]\n\n");
    assert!(converter.push(&terminal).expect("complete").end.is_some());
}

#[test]
fn responses_output_and_tool_argument_bounds_are_enforced() {
    let limits = Limits {
        max_output_bytes: 40,
        ..Limits::default()
    };
    let mut converter = SseConverter::new(
        Protocol::OpenAiChat,
        Protocol::OpenAiResponses,
        context(),
        limits,
    )
    .expect("converter");
    assert_eq!(
        converter
            .push(&chat_event(json!({"content":"x".repeat(41)}), Value::Null))
            .expect_err("bounded terminal snapshot")
            .kind,
        ErrorKind::LimitExceeded
    );
    let limits = Limits {
        max_tool_argument_bytes: 3,
        ..Limits::default()
    };
    let mut converter =
        SseConverter::new(Protocol::OpenAiChat, Protocol::Gemini, context(), limits)
            .expect("converter");
    assert_eq!(converter.push(&chat_event(json!({"tool_calls":[{"index":0,"id":"fixture","type":"function","function":{"name":"tool","arguments":"{\"x\":"}}]}), Value::Null)).expect_err("tool argument cap").kind, ErrorKind::LimitExceeded);
}

#[test]
fn conflicting_usage_is_rejected_and_never_counted_twice() {
    let mut converter = SseConverter::new(
        Protocol::OpenAiChat,
        Protocol::Gemini,
        context(),
        Limits::default(),
    )
    .expect("converter");
    for output in [3, 4] {
        let frame = event(
            "",
            json!({"choices":[],"usage":{"prompt_tokens":7,"completion_tokens":output,"total_tokens":7+output}}),
        );
        let result = converter.push(&frame);
        if output == 3 {
            assert!(result.expect("first snapshot").end.is_none());
        } else {
            assert_eq!(
                result.expect_err("conflicting final totals").kind,
                ErrorKind::InvalidSequence
            );
        }
    }
}

#[test]
fn safe_debug_does_not_expose_buffered_content() {
    let mut converter = SseConverter::new(
        Protocol::OpenAiChat,
        Protocol::OpenAiResponses,
        context(),
        Limits::default(),
    )
    .expect("converter");
    converter
        .push(&chat_event(
            json!({"content":"private-fixture-123"}),
            Value::Null,
        ))
        .expect("partial output");
    assert!(!format!("{converter:?}").contains("private-fixture-123"));
}

#[test]
fn sse_accepts_crlf_bom_comments_and_multiline_json_data() {
    let mut bytes = b"\xef\xbb\xbf: fixture keepalive\r\n\r\nevent: message\r\ndata: {\"choices\":[{\"index\":0,\r\ndata: \"delta\":{\"content\":\"fixture\"},\"finish_reason\":null}]}\r\n\r\n".to_vec();
    bytes.extend(chat_event(json!({}), json!("stop")));
    bytes.extend_from_slice(b"data: [DONE]\r\n\r\n");
    let mut converter = SseConverter::new(
        Protocol::OpenAiChat,
        Protocol::Gemini,
        context(),
        Limits::default(),
    )
    .expect("converter");
    let result = converter.push(&bytes).expect("valid SSE framing");
    assert_eq!(
        result.end.expect("terminal observation").outcome,
        Outcome::Finished(FinishReason::Stop)
    );
    let values = decode_frames(&result.frames);
    assert_eq!(
        values[0]["candidates"][0]["content"]["parts"][0]["text"],
        "fixture"
    );
}

#[test]
fn official_chat_null_tool_metadata_does_not_reject_argument_deltas() {
    let mut converter = SseConverter::new(
        Protocol::OpenAiChat,
        Protocol::Gemini,
        context(),
        Limits::default(),
    )
    .expect("converter");
    converter.push(&chat_event(json!({"tool_calls":[{"index":0,"id":"call_fixture","type":"function","function":{"name":"weather","arguments":""}}]}), Value::Null)).expect("initial metadata");
    converter.push(&chat_event(json!({"tool_calls":[{"index":0,"id":null,"type":null,"function":{"name":null,"arguments":"{\"x\":"}}]}), Value::Null)).expect("null metadata means absent");
    converter.push(&chat_event(json!({"tool_calls":[{"index":0,"id":null,"type":null,"function":{"name":null,"arguments":"1}"}}]}), Value::Null)).expect("argument continuation");
    let mut terminal = chat_event(json!({}), json!("tool_calls"));
    terminal.extend_from_slice(b"data: [DONE]\n\n");
    assert!(converter.push(&terminal).expect("terminal").end.is_some());
}

#[test]
fn gemini_usage_only_tail_is_finalized_at_eof_once() {
    let mut converter = SseConverter::new(
        Protocol::Gemini,
        Protocol::AnthropicMessages,
        context(),
        Limits::default(),
    )
    .expect("converter");
    assert!(converter.push(&event("", json!({"candidates":[{"index":0,"content":{"role":"model","parts":[{"text":"fixture"}]},"finishReason":"STOP"}]}))).expect("semantic finish").end.is_none());
    assert!(converter.push(&event("", json!({"usageMetadata":{"promptTokenCount":7,"candidatesTokenCount":3,"totalTokenCount":10}}))).expect("usage after finish").end.is_none());
    let end = converter
        .finish()
        .expect("EOF after semantic finish")
        .end
        .expect("one final observation");
    assert_eq!(end.usage.expect("tail usage").total_tokens, Some(10));
    assert_eq!(
        converter.finish().expect_err("no repeated accounting").kind,
        ErrorKind::Closed
    );
}

#[test]
fn conversion_failure_can_emit_one_safe_sse_abort() {
    let mut converter = SseConverter::new(
        Protocol::OpenAiChat,
        Protocol::OpenAiResponses,
        context(),
        Limits::default(),
    )
    .expect("converter");
    converter
        .push(&chat_event(
            json!({"content":"fixture partial"}),
            Value::Null,
        ))
        .expect("initial frame");
    assert_eq!(
        converter
            .push(b"data: invalid\n\n")
            .expect_err("invalid JSON closes parsing")
            .kind,
        ErrorKind::InvalidJson
    );
    let error = GatewayError {
        category: ErrorCategory::Compatibility,
        status: 502,
    };
    let batch = converter
        .abort(error.clone())
        .expect("safe downstream failure");
    assert_eq!(
        batch.end.expect("failed once").outcome,
        Outcome::Failed(ErrorCategory::Compatibility)
    );
    assert_eq!(decode_frames(&batch.frames)[0]["type"], "response.failed");
    assert_eq!(
        converter.abort(error).expect_err("one failure record").kind,
        ErrorKind::Closed
    );
}

#[test]
fn duplicate_argument_keys_cannot_be_erased_by_object_projection() {
    let mut request = request_fixture(Protocol::OpenAiChat);
    request["messages"][2]["tool_calls"][0]["function"]["arguments"] =
        json!(r#"{"x":1,"\u0078":2}"#);
    let error = convert_request(Protocol::OpenAiChat, Protocol::Gemini, &request, "fixture")
        .expect_err("escaped duplicate key");
    assert_eq!(error.location, "tool.arguments.duplicate_key");
    request["messages"][2]["tool_calls"][0]["function"]["arguments"] =
        json!(r#"{"x":1,"nested":{"x":2}}"#);
    assert!(
        convert_request(Protocol::OpenAiChat, Protocol::Gemini, &request, "fixture").is_ok(),
        "different object scopes may reuse a key"
    );
}

#[test]
fn implicit_responses_strict_normalization_is_not_guessed() {
    let mut fixture = request_fixture(Protocol::OpenAiResponses);
    fixture["tools"][0]
        .as_object_mut()
        .expect("tool")
        .remove("strict");
    assert_eq!(
        convert_request(
            Protocol::OpenAiResponses,
            Protocol::OpenAiChat,
            &fixture,
            "fixture"
        )
        .expect_err("automatic normalization is server behavior")
        .location,
        "tool.responses_implicit_strict"
    );
}

#[test]
fn length_finish_remains_incomplete_in_responses_format() {
    let mut fixture = response_fixture(Protocol::OpenAiChat);
    fixture["choices"][0]["finish_reason"] = json!("length");
    let converted = convert_response(
        Protocol::OpenAiChat,
        Protocol::OpenAiResponses,
        &fixture,
        &context(),
    )
    .expect("length mapping");
    assert_eq!(converted.outcome, Outcome::Finished(FinishReason::Length));
    assert_eq!(converted.body["status"], "incomplete");
    assert_eq!(
        converted.body["incomplete_details"]["reason"],
        "max_output_tokens"
    );
}
