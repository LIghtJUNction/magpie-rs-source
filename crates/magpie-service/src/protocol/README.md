# Pure protocol converter handoff

Scope: `protocol.rs` and this directory only. Baseline:
`dd5eed87b6d14213aeb604b50dd23d6d25a03f66`. Prepared 2026-10-29.
This is an implementation candidate with synthetic fixtures, **not** full
REQ-031/032/033 acceptance or provider compatibility certification.

## Fixed public API

```rust
pub enum Protocol {
    OpenAiChat,
    OpenAiResponses,
    AnthropicMessages,
    Gemini,
}

pub fn convert_request(
    source: Protocol,
    target: Protocol,
    body: &serde_json::Value,
    upstream_model: &str,
) -> Result<serde_json::Value, ConversionError>;

pub fn convert_response(
    source: Protocol,
    target: Protocol,
    body: &serde_json::Value,
    context: &ResponseContext,
) -> Result<ConvertedResponse, ConversionError>;

pub struct ResponseContext {
    pub id: String,
    pub model: String,
    pub created: u64,
}

pub struct ConvertedResponse {
    pub body: serde_json::Value,
    pub usage: Option<Usage>,
    pub outcome: Outcome,
}

impl SseConverter {
    pub fn new(
        source: Protocol,
        target: Protocol,
        context: ResponseContext,
        limits: Limits,
    ) -> Result<Self, ConversionError>;

    pub fn push(&mut self, bytes: &[u8]) -> Result<StreamBatch, ConversionError>;
    pub fn finish(&mut self) -> Result<StreamBatch, ConversionError>;
    pub fn cancel(&mut self) -> Result<StreamEnd, ConversionError>;
    pub fn abort(&mut self, error: GatewayError) -> Result<StreamBatch, ConversionError>;
}

pub struct StreamBatch {
    pub frames: Vec<Vec<u8>>,
    pub end: Option<StreamEnd>,
}

pub struct StreamEnd {
    pub outcome: Outcome,
    pub usage: Option<Usage>,
}

pub fn error_body(protocol: Protocol, error: &GatewayError) -> serde_json::Value;
```

`Outcome` distinguishes `Finished(FinishReason)`, `Failed(ErrorCategory)`, and
`Cancelled`. Finish reasons are `Stop`, `StopSequence(String)`, `Length`,
`ToolCalls`, and `ContentFilter`. Stop sequences are not reclassified as an
unspecified stop when their identity cannot be represented.

`ConversionError { kind, location }` contains only a stable category and a static
schema location. `ErrorKind` includes `InvalidInput`, `UnsupportedField`,
`UnsupportedSemantics`, `LimitExceeded`, `InvalidUtf8`, `InvalidJson`,
`InvalidSequence`, `IncompleteStream`, and `Closed`. It implements
`std::error::Error`. Unknown property **values and names** are not reflected in
the error text.

`GatewayError { category, status }` is supplied by the HTTP boundary. Categories
include compatibility, authentication, permission, model not found, quota,
rate limit, upstream client/server errors, network, timeout, and internal error.
Error encoders use safe static messages and preserve these categories without
copying an upstream prompt, token, or response body.

## Main-thread integration fragment

The owner of `lib.rs` adds:

```rust
pub mod protocol;
```

Provider persistence keeps its own protocol labels. The integration maps those
labels to this enum; this module imports neither provider state nor credentials.
The entry route, not the provider setting, determines the client protocol.

```rust
let mut upstream_body = protocol::convert_request(
    client_protocol,
    provider_protocol,
    &client_body,
    upstream_model,
)?;

// The HTTP boundary already knows whether the client used a streaming route.
// Gemini expresses streaming and the model in the URL, not in its JSON body.
if provider_protocol != protocol::Protocol::Gemini {
    upstream_body["stream"] = serde_json::json!(is_stream);
}
if is_stream && provider_protocol == protocol::Protocol::OpenAiChat {
    upstream_body["stream_options"] = serde_json::json!({"include_usage": true});
}
```

Send exactly one upstream request after conversion succeeds. Use the selected
provider's existing HTTP/authentication code, allowlist, timeout, concurrency
limit, and credential reference. The converter never fetches, retries, routes,
authenticates, records prompts, or starts a display/runtime.

For non-streaming replies call
`convert_response(provider_protocol, client_protocol, &reply, &context)`.
`ResponseContext` is the gateway's explicitly chosen output identity, downstream
model label, and Unix seconds timestamp. It is not inferred from content or an
account. Cross-protocol output items use stable IDs derived from that identity.
The original wire IDs are not a portable conversation store:
`previous_response_id` and item references are rejected on translated requests.

For different streaming protocols:

1. Create one `SseConverter`.
2. Slice network chunks to `limits.max_chunk_bytes`, then call `push`.
3. Drain/await each returned frame before polling upstream again. Do not collect
   all returned batches or all upstream bytes into a second stream buffer.
4. Record a final request observation only for the single `batch.end`. Partial
   provider usage snapshots are merged, not added. Attempt billing/deduplication
   remains keyed by request/attempt IDs at the service layer.
5. On transport EOF, call `finish` only if no terminal batch was returned.
6. On downstream cancellation, stop polling/close the upstream body and call
   `cancel` if still active. `cancel` frees local state but cannot close a socket
   it does not own.
7. On conversion/EOF error, stop the upstream body. Before headers, return a
   classified protocol error with the appropriate HTTP status. After headers,
   `abort(GatewayError { category: ErrorCategory::Compatibility, status: 502 })`
   emits the client's safe SSE error and one failed terminal record. `abort`
   also works after a failed `push`/`finish`; no extra adapter is necessary.

Do not call `finish` or feed further semantic events after receiving `end`.
Native streams use the existing passthrough path; `SseConverter::new` rejects
equal protocols. Native non-streaming bodies retain provider extensions.
Native request conversion replaces only the model outside Gemini. The native
non-stream response helper still validates the usage/finish observation; native
forwarding may bypass this helper if it needs opaque extension support.

The HTTP layer preserves the upstream HTTP classification/status separately;
JSON conversion cannot reconstruct an HTTP status reliably. Suggested mapping:
unsupported client translation -> 422, malformed client JSON -> 400, caller
request limit -> 413, invalid/truncated upstream stream -> 502. Do not change the
HTTP status after sending headers. Do not log `ConvertedResponse`, raw JSON,
frames, or tool arguments; its `Debug` output is not a diagnostic whitelist.
`SseConverter` has a redacted custom `Debug`.

## Implemented paths and explicit boundaries

The fixtures describe all 12 directed cross-protocol pairs for requests,
non-streaming responses, and SSE text/tool flows. They include system/user/
assistant context, function declarations/calls/results with IDs, inline images,
temperature/output caps, supported tool choices, byte-exact OpenAI argument
strings, and exact JSON integer values when projecting arguments into objects.
These fixtures have **not been run by this worker**.

- `system` and `developer` remain distinct. OpenAI preserves developer roles;
  Messages/Gemini translations reject them. System instructions targeting
  Messages/Gemini must precede conversation messages and contain only text.
- OpenAI Chat <-> Responses maps reasoning effort. Plain Gemini thought text
  and unsigned reasoning summaries have an explicit Responses/Gemini output
  representation. No unofficial Chat `reasoning_content` field is invented.
- Signed Anthropic thinking, Gemini `thoughtSignature`, encrypted Responses
  reasoning, redacted blocks, thinking budget/effort dialects across vendors,
  and server-managed history are rejected until a lossless contract exists.
- Common function JSON schemas are carried unchanged. Gemini currently accepts
  the `parametersJsonSchema` dialect; its protobuf/OpenAPI `parameters` dialect
  is pending. An explicit schema is required. A Responses tool with omitted or
  null `strict` is rejected because its server-side automatic strict
  normalization cannot be reproduced by guessing. Chat/Messages/Gemini
  non-strict tools become `strict: false` when targeting Responses. Explicit
  strict enforcement cannot be weakened for a Messages/Gemini target.
- Tool results require an ID when the target requires one. A Gemini function
  response also needs a name, resolved only from an unambiguous call ID in the
  supplied history. Missing identifiers are rejected, never fabricated.
  String results use Gemini's `{"output": string}` response envelope.
  Other structured results, error-result flags without a native target
  equivalent, and mixed text/tool-result blocks are explicitly rejected.
- An OpenAI JSON argument string remains byte-identical between OpenAI
  endpoints. Messages/Gemini require objects, so JSON spelling/whitespace cannot
  be retained there; values remain JSON values using the existing
  `serde_json/arbitrary_precision` dependency. Duplicate argument-object keys,
  including equivalent escaped spellings, are rejected before map projection.
- Inline base64 images map among all four request formats. Remote OpenAI/
  Anthropic image URLs map where there is an equivalent URL image source.
  Gemini file URIs, remote-image uploading, unsupported image-detail mappings,
  audio/video/PDF media, generated media streams, and speech multipart routing
  remain pending. No media is fetched or decoded by this module.
- Responses has no declared stop-sequence request equivalent; nonempty stops
  targeting it are rejected. Messages requires the caller's explicit output
  cap. Sampling/configuration fields with no mapping are rejected.
- Multiple choices/candidates, provider service-tier/fingerprint metadata,
  logprobs, citations/grounding/safety-rating payloads, unknown finish reasons,
  built-in/custom/server tools, unknown request fields, and unknown semantic SSE
  events are rejected when translating. This is intentionally not a declaration
  that all official API fields are supported.
- Interleaved Chat tools that revisit an already-closed Anthropic target block
  are rejected. Parallel-tool scheduling and replay of already emitted block
  content are pending. Text/tool interleaving that the target format cannot
  represent also fails explicitly.

## Streaming completion, usage, and bounds

Chat completion requires a finish reason and `[DONE]`; Messages requires its
block lifecycle, stop reason, and `message_stop`; Responses validates output
item/content lifecycles, increasing sequence numbers, done snapshots, final
output consistency, and `response.completed`/`response.incomplete`/failure.
Gemini requires `finishReason`; usage-only chunks may follow it, so final
emission waits for transport EOF. Truncated or cancelled streams cannot become
successful completions.

The normalized `Usage` stores optional input/output/total, cached input,
cache-creation input, and reasoning counts. Unknown remains `None`. Anthropic's
separate cache read/create tokens are included in normalized input; Gemini's
separate thought tokens are included in normalized output. Source totals are
kept when present. Unsupported nonzero breakdowns are rejected, not erased.
When absent, a total is derived only if both input and output are known and
their sum does not overflow. Initial Anthropic stream-start zero counters are
provisional wire placeholders, never a final accounting observation; an
Anthropic target refuses successful termination without actual input/output
counts. Failure/cancellation do not promote partial usage to final billing.

Default limits are 64 KiB per input chunk, 4 MiB per SSE event, 4 MiB of retained
normalized output/metadata, 256 KiB per tool argument string, and 128 blocks.
They are configurable safety bounds, **not measured product performance
budgets**. Requests/non-streaming bodies have already been parsed into `Value`;
their HTTP byte/time limits remain the caller's responsibility.

Ordinary non-Responses text streams do not accumulate the full output. Tool
argument objects are buffered to a finite bound for validation and Gemini's
object emission. A Responses source/target retains normalized output to compare
done snapshots or generate the required final full response. It never buffers
the entire raw SSE transcript. Parsing/serializing a terminal response may
temporarily hold bounded additional representations; no RSS/latency claim is
made without measurement.

All conversion errors close the state and free private buffers. `abort` can then
emit the safe downstream failure once. Cancellation/terminal handling also frees
retained buffers. Network cancellation latency and released sockets/permits
remain integration acceptance, not a consequence proven by these pure tests.

## Official primary sources

Reviewed 2026-10-29; this is not an API-version support guarantee.

- [OpenAI Chat create](https://developers.openai.com/api/reference/resources/chat/subresources/completions/methods/create)
  and [Chat streaming events](https://developers.openai.com/api/reference/resources/chat/subresources/completions/streaming-events):
  role/content/tool shapes, finish reasons, and final usage before `[DONE]`.
- [OpenAI Responses streaming events](https://developers.openai.com/api/reference/resources/responses/streaming-events):
  output item/content/argument deltas, sequence numbers, and terminal snapshots.
- [OpenAI function calling](https://developers.openai.com/api/docs/guides/function-calling)
  and [Responses migration](https://developers.openai.com/api/docs/guides/migrate-to-responses):
  call IDs, schema/strict behavior, and differences between the two OpenAI APIs.
- [Anthropic streaming](https://platform.claude.com/docs/en/build-with-claude/streaming):
  message/block lifecycle, cumulative usage, partial JSON, signature deltas, and
  in-stream errors.
- [Gemini GenerateContent reference](https://ai.google.dev/api/generate-content):
  content/part unions, function call/response IDs, finish reasons, schemas, and
  token breakdowns.
- [Gemini thought signatures](https://ai.google.dev/gemini-api/docs/generate-content/thought-signatures):
  opaque metadata that must not be replaced with ordinary text.

## Acceptance handoff

The main thread owns Cargo execution, root recipes/COVERAGE/README, commits,
publication, and gitlink changes. This worker did not modify those paths,
`forward.rs`, `lib.rs`, Cargo manifests, core, or UI.

Fixture source is `tests.rs`. It covers pairwise conversions, large integers,
image payloads, unsupported roles/fields/signatures, UTF-8 byte boundaries,
partial tool JSON, event/output/tool bounds, cancellation, missing EOF/terminal
conditions, one final usage, safe error output, and incremental long text.

After module registration, the main thread can run its existing serialized,
external-target Cargo validation, for example:

```sh
CARGO_TARGET_DIR=/tmp/magpie-rs-build \
  cargo test --locked -p magpie-service protocol::
CARGO_TARGET_DIR=/tmp/magpie-rs-build \
  cargo clippy --locked -p magpie-service --all-targets -- -D warnings
```

Run the global space-first workflow before a build that may generate >100 MiB.
Do not infer passing tests from their presence.

Suggested root coverage entry: REQ-032/033 have a pure conversion candidate and
synthetic fixtures, pending serial compilation/test execution and gateway
integration. Full REQ-031/032/033, networking/backpressure/cancellation,
retry/attempt deduplication, plugins/tools lifecycle in spec06, provider accounts,
OS/device execution, original-product comparison, and external compatibility
remain incomplete/unverified. No REQ is changed to fully passing by this handoff.
