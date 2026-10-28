use super::{model::*, request, response};
use serde_json::{Value, json};
use std::collections::BTreeMap;

type Key = (u64, u64);

/// Output ready to send under downstream backpressure, plus one terminal record.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct StreamBatch {
    pub frames: Vec<Vec<u8>>,
    pub end: Option<StreamEnd>,
}

/// The only accounting observation emitted by a successfully closed converter.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StreamEnd {
    pub outcome: Outcome,
    pub usage: Option<Usage>,
}

#[derive(Clone, Debug, PartialEq)]
enum Kind {
    Text,
    Refusal,
    Reasoning,
    Tool { id: String, name: String },
}

struct Block {
    kind: Kind,
    wire_index: usize,
    value: String,
    stopped: bool,
}

#[derive(Clone, Copy, PartialEq)]
enum ItemKind {
    Message,
    Tool,
    Reasoning,
}

struct Item {
    id: String,
    kind: ItemKind,
    done: bool,
}

#[derive(Default)]
struct Framing {
    line: Vec<u8>,
    data: Vec<u8>,
    event: String,
    after_cr: bool,
    first_line: bool,
}

/// Incremental SSE conversion with finite buffers and explicit EOF semantics.
///
/// Calls neither a runtime nor a network API. Consume each returned batch before
/// reading more upstream bytes. On conversion failure the state is closed and
/// all private buffers are released; the caller must close the upstream socket.
/// Native streams belong on the caller's existing passthrough path.
pub struct SseConverter {
    source: Protocol,
    target: Protocol,
    context: ResponseContext,
    limits: Limits,
    framing: Framing,
    blocks: BTreeMap<Key, Block>,
    items: BTreeMap<u64, Item>,
    pending_tools: BTreeMap<u64, (String, String)>,
    raw_usage: Value,
    usage: Option<Usage>,
    finish_reason: Option<FinishReason>,
    retained_bytes: usize,
    sequence: u64,
    source_sequence: Option<u64>,
    started: bool,
    source_started: bool,
    closed: bool,
    end_emitted: bool,
    // Anthropic blocks cannot be reopened after their stop event. Parallel
    // Chat tool deltas which revisit a closed block are explicitly rejected.
    active_anthropic: Option<Key>,
}

impl std::fmt::Debug for SseConverter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SseConverter")
            .field("source", &self.source)
            .field("target", &self.target)
            .field("limits", &self.limits)
            .field("blocks", &self.blocks.len())
            .field("retained_bytes", &self.retained_bytes)
            .field("closed", &self.closed)
            .finish()
    }
}

impl SseConverter {
    /// Creates one cross-protocol converter with caller-provided response identity.
    ///
    /// # Errors
    /// Rejects zero bounds, excessive block counts, empty identity, or native use.
    pub fn new(
        source: Protocol,
        target: Protocol,
        context: ResponseContext,
        limits: Limits,
    ) -> Result<Self> {
        if source == target {
            return Err(unsupported("stream.native_passthrough"));
        }
        if context.id.is_empty()
            || context.model.is_empty()
            || context.id.len().saturating_add(context.model.len()) > limits.max_output_bytes
        {
            return Err(fail(ErrorKind::InvalidInput, "stream.context"));
        }
        if limits.max_chunk_bytes == 0
            || limits.max_event_bytes == 0
            || limits.max_output_bytes == 0
            || limits.max_tool_argument_bytes == 0
            || limits.max_blocks == 0
            || limits.max_blocks > 65_536
        {
            return Err(fail(ErrorKind::InvalidInput, "stream.limits"));
        }
        Ok(Self {
            source,
            target,
            context,
            limits,
            framing: Framing {
                first_line: true,
                ..Framing::default()
            },
            blocks: BTreeMap::new(),
            items: BTreeMap::new(),
            pending_tools: BTreeMap::new(),
            raw_usage: json!({}),
            usage: None,
            finish_reason: None,
            retained_bytes: 0,
            sequence: 0,
            source_sequence: None,
            started: false,
            source_started: false,
            closed: false,
            end_emitted: false,
            active_anthropic: None,
        })
    }

    /// Consumes a bounded byte chunk, including partial UTF-8 and SSE lines.
    ///
    /// # Errors
    /// An invalid/oversized event closes the converter. Do not retry the bytes.
    pub fn push(&mut self, bytes: &[u8]) -> Result<StreamBatch> {
        if self.closed {
            return Err(fail(ErrorKind::Closed, "stream"));
        }
        let result = self.push_inner(bytes);
        if result.is_err() {
            self.release();
            self.closed = true;
            self.end_emitted = false;
        }
        result
    }

    fn push_inner(&mut self, bytes: &[u8]) -> Result<StreamBatch> {
        if bytes.len() > self.limits.max_chunk_bytes {
            return Err(fail(ErrorKind::LimitExceeded, "stream.chunk"));
        }
        let mut batch = StreamBatch::default();
        for &byte in bytes {
            if self.framing.after_cr {
                self.framing.after_cr = false;
                if byte == b'\n' {
                    continue;
                }
            }
            if byte == b'\r' || byte == b'\n' {
                self.line(&mut batch)?;
                self.framing.after_cr = byte == b'\r';
            } else {
                if self
                    .framing
                    .line
                    .len()
                    .saturating_add(self.framing.data.len())
                    .saturating_add(self.framing.event.len())
                    >= self.limits.max_event_bytes
                {
                    return Err(fail(ErrorKind::LimitExceeded, "stream.event"));
                }
                self.framing.line.push(byte);
            }
        }
        if self.closed {
            if !self.framing.line.is_empty()
                || !self.framing.data.is_empty()
                || !self.framing.event.is_empty()
            {
                return Err(fail(ErrorKind::InvalidSequence, "stream.after_terminal"));
            }
            self.release();
        }
        Ok(batch)
    }

    fn line(&mut self, batch: &mut StreamBatch) -> Result<()> {
        let bytes = std::mem::take(&mut self.framing.line);
        let line =
            std::str::from_utf8(&bytes).map_err(|_| fail(ErrorKind::InvalidUtf8, "stream.line"))?;
        let line = if self.framing.first_line {
            self.framing.first_line = false;
            line.strip_prefix('\u{feff}').unwrap_or(line)
        } else {
            line
        };
        if line.is_empty() {
            if !self.framing.data.is_empty() {
                if self.closed {
                    return Err(fail(ErrorKind::InvalidSequence, "stream.after_terminal"));
                }
                let mut data = std::mem::take(&mut self.framing.data);
                data.pop(); // SSE joins data lines and removes the last newline.
                let event = std::mem::take(&mut self.framing.event);
                self.event(&event, &data, batch)?;
            } else {
                self.framing.event.clear();
            }
            return Ok(());
        }
        if line.starts_with(':') {
            return Ok(());
        }
        if self.closed {
            return Err(fail(ErrorKind::InvalidSequence, "stream.after_terminal"));
        }
        let (name, value) = line.split_once(':').unwrap_or((line, ""));
        let value = value.strip_prefix(' ').unwrap_or(value);
        match name {
            "data" => {
                if self
                    .framing
                    .data
                    .len()
                    .saturating_add(value.len())
                    .saturating_add(self.framing.event.len())
                    .saturating_add(1)
                    > self.limits.max_event_bytes
                {
                    return Err(fail(ErrorKind::LimitExceeded, "stream.event"));
                }
                self.framing.data.extend_from_slice(value.as_bytes());
                self.framing.data.push(b'\n');
            }
            "event" => self.framing.event = value.to_owned(),
            // SSE framing metadata does not modify generated model content.
            "id" | "retry" => {}
            _ => return Err(unsupported("stream.sse_field")),
        }
        Ok(())
    }

    /// Marks transport EOF; Gemini success requires a prior semantic finish.
    ///
    /// # Errors
    /// A missing terminator, partial event, or unfinished block is incomplete.
    pub fn finish(&mut self) -> Result<StreamBatch> {
        if self.closed {
            return Err(fail(ErrorKind::Closed, "stream"));
        }
        let result = if !self.framing.line.is_empty()
            || !self.framing.data.is_empty()
            || !self.framing.event.is_empty()
        {
            Err(fail(ErrorKind::IncompleteStream, "stream.partial_event"))
        } else if self.source == Protocol::Gemini && self.finish_reason.is_some() {
            let mut batch = StreamBatch::default();
            self.complete(&mut batch).map(|()| batch)
        } else {
            Err(fail(ErrorKind::IncompleteStream, "stream.missing_terminal"))
        };
        self.closed = true;
        self.release();
        result
    }

    /// Cancels once and immediately releases all private buffers.
    ///
    /// In-flight usage snapshots are not reported as final billing observations.
    ///
    /// # Errors
    /// A previously terminated, failed, or cancelled converter is closed.
    pub fn cancel(&mut self) -> Result<StreamEnd> {
        if self.closed {
            return Err(fail(ErrorKind::Closed, "stream"));
        }
        self.closed = true;
        self.end_emitted = true;
        self.release();
        Ok(StreamEnd {
            outcome: Outcome::Cancelled,
            usage: None,
        })
    }

    /// Emits one safe protocol error, including after a conversion/EOF failure.
    ///
    /// The caller must stop upstream I/O. This is useful after headers have
    /// already been sent; it does not attempt to change the HTTP status.
    ///
    /// # Errors
    /// Rejects a converter whose final record was already delivered or cancelled.
    pub fn abort(&mut self, error: GatewayError) -> Result<StreamBatch> {
        if self.end_emitted {
            return Err(fail(ErrorKind::Closed, "stream"));
        }
        let mut batch = StreamBatch::default();
        let result = self.fail_upstream(error, &mut batch);
        self.closed = true;
        self.release();
        result.map(|()| batch)
    }

    fn release(&mut self) {
        self.blocks = BTreeMap::new();
        self.items = BTreeMap::new();
        self.pending_tools = BTreeMap::new();
        self.framing = Framing::default();
        self.raw_usage = Value::Null;
        self.usage = None;
        self.retained_bytes = 0;
        self.active_anthropic = None;
    }

    fn event(&mut self, name: &str, data: &[u8], batch: &mut StreamBatch) -> Result<()> {
        if self.source == Protocol::OpenAiChat && data == b"[DONE]" {
            if !name.is_empty() && name != "message" {
                return Err(unsupported("stream.event_name"));
            }
            return self.complete(batch);
        }
        let value = parse_json(data, "stream.event.json")?;
        object(&value, "stream.event")?;
        if self.source == Protocol::OpenAiResponses {
            let sequence = required(&value, "sequence_number", "stream.sequence_number")?
                .as_u64()
                .ok_or_else(|| fail(ErrorKind::InvalidInput, "stream.sequence_number"))?;
            if self
                .source_sequence
                .is_some_and(|previous| sequence <= previous)
            {
                return Err(fail(ErrorKind::InvalidSequence, "stream.sequence_number"));
            }
            self.source_sequence = Some(sequence);
        }
        if value.get("error").is_some_and(|v| !v.is_null())
            && value.get("type").is_none_or(|v| v == "error")
        {
            return self.fail_upstream(response::upstream_error(&value), batch);
        }
        match self.source {
            Protocol::OpenAiChat => {
                if !name.is_empty() && name != "message" {
                    return Err(unsupported("stream.event_name"));
                }
                self.chat(&value, batch)
            }
            Protocol::AnthropicMessages | Protocol::OpenAiResponses => {
                let kind = text(&value, "type", "stream.event.type")?;
                if !name.is_empty() && name != kind {
                    return Err(fail(ErrorKind::InvalidSequence, "stream.event_name"));
                }
                if self.source == Protocol::AnthropicMessages {
                    self.anthropic(kind, &value, batch)
                } else {
                    self.responses(kind, &value, batch)
                }
            }
            Protocol::Gemini => {
                if !name.is_empty() && name != "message" {
                    return Err(unsupported("stream.event_name"));
                }
                self.gemini(&value, batch)
            }
        }
    }

    fn header(&mut self, frames: &mut Vec<Vec<u8>>) -> Result<()> {
        if self.started {
            return Ok(());
        }
        self.started = true;
        match self.target {
            Protocol::OpenAiChat => {
                self.chat_frame(json!({"role":"assistant","content":""}), None, frames)
            }
            Protocol::AnthropicMessages => {
                // These are provisional stream-start counters. No accounting
                // observation is emitted until the final cumulative usage.
                self.frame("message_start", json!({"type":"message_start","message":{"id":self.context.id,"type":"message","role":"assistant","model":self.context.model,"content":[],"stop_reason":null,"stop_sequence":null,"usage":{"input_tokens":0,"output_tokens":0}}}), frames)
            }
            Protocol::OpenAiResponses => {
                let shell = json!({"id":self.context.id,"object":"response","created_at":self.context.created,"model":self.context.model,"status":"in_progress","output":[],"error":null,"incomplete_details":null});
                self.response_frame("response.created", json!({"response":shell}), frames)?;
                self.response_frame("response.in_progress", json!({"response":shell}), frames)
            }
            Protocol::Gemini => Ok(()),
        }
    }

    fn frame(&self, name: &str, value: Value, frames: &mut Vec<Vec<u8>>) -> Result<()> {
        let data = value.to_string();
        if data.len() > self.limits.max_event_bytes {
            return Err(fail(ErrorKind::LimitExceeded, "stream.output_event"));
        }
        frames.push(
            if name.is_empty() {
                format!("data: {data}\n\n")
            } else {
                format!("event: {name}\ndata: {data}\n\n")
            }
            .into_bytes(),
        );
        Ok(())
    }

    fn response_frame(
        &mut self,
        name: &str,
        mut value: Value,
        frames: &mut Vec<Vec<u8>>,
    ) -> Result<()> {
        value["type"] = json!(name);
        value["sequence_number"] = json!(self.sequence);
        self.sequence = self
            .sequence
            .checked_add(1)
            .ok_or_else(|| fail(ErrorKind::LimitExceeded, "stream.sequence"))?;
        self.frame(name, value, frames)
    }

    fn chat_frame(
        &self,
        delta: Value,
        finish: Option<&str>,
        frames: &mut Vec<Vec<u8>>,
    ) -> Result<()> {
        self.frame("", json!({"id":self.context.id,"object":"chat.completion.chunk","created":self.context.created,"model":self.context.model,"choices":[{"index":0,"delta":delta,"finish_reason":finish}]}), frames)
    }

    fn retains_text(&self) -> bool {
        self.source == Protocol::OpenAiResponses || self.target == Protocol::OpenAiResponses
    }

    fn reserve(&mut self, bytes: usize) -> Result<()> {
        self.retained_bytes = self
            .retained_bytes
            .checked_add(bytes)
            .ok_or_else(|| fail(ErrorKind::LimitExceeded, "stream.output"))?;
        if self.retained_bytes > self.limits.max_output_bytes {
            return Err(fail(ErrorKind::LimitExceeded, "stream.output"));
        }
        Ok(())
    }

    fn start_block(&mut self, key: Key, kind: Kind, batch: &mut StreamBatch) -> Result<()> {
        if self.blocks.contains_key(&key) {
            return Err(fail(ErrorKind::InvalidSequence, "stream.duplicate_block"));
        }
        if self.blocks.len() >= self.limits.max_blocks {
            return Err(fail(ErrorKind::LimitExceeded, "stream.blocks"));
        }
        if self.target == Protocol::OpenAiChat && kind == Kind::Reasoning {
            return Err(unsupported("stream.reasoning.chat"));
        }
        if self.target == Protocol::AnthropicMessages
            && matches!(kind, Kind::Reasoning | Kind::Refusal)
        {
            return Err(unsupported("stream.reasoning_or_refusal.anthropic"));
        }
        if self.target == Protocol::Gemini && kind == Kind::Refusal {
            return Err(unsupported("stream.refusal.gemini"));
        }
        if self.target == Protocol::AnthropicMessages {
            if let Some(active) = self.active_anthropic {
                if !self.blocks.get(&active).is_some_and(|b| b.stopped) {
                    self.stop_block(active, batch)?;
                }
            }
            self.active_anthropic = Some(key);
        }
        self.header(&mut batch.frames)?;
        let index = self.blocks.len();
        if let Kind::Tool { id, name } = &kind {
            if id.is_empty() || name.is_empty() {
                return Err(unsupported("stream.tool.missing_identity"));
            }
            self.reserve(id.len().saturating_add(name.len()))?;
        }
        self.blocks.insert(
            key,
            Block {
                kind: kind.clone(),
                wire_index: index,
                value: String::new(),
                stopped: false,
            },
        );
        let item_id = self.item_id(index);
        match self.target {
            Protocol::AnthropicMessages => {
                let content = match kind {
                    Kind::Text => json!({"type":"text","text":""}),
                    Kind::Tool { id, name } => {
                        json!({"type":"tool_use","id":id,"name":name,"input":{}})
                    }
                    _ => return Err(unsupported("stream.content.anthropic")),
                };
                self.frame(
                    "content_block_start",
                    json!({"type":"content_block_start","index":index,"content_block":content}),
                    &mut batch.frames,
                )?;
            }
            Protocol::OpenAiChat => {
                if let Kind::Tool { id, name } = kind {
                    let tool_index = self.tool_index(key)?;
                    self.chat_frame(json!({"tool_calls":[{"index":tool_index,"id":id,"type":"function","function":{"name":name,"arguments":""}}]}), None, &mut batch.frames)?;
                }
            }
            Protocol::OpenAiResponses => {
                let item = match &kind {
                    Kind::Text | Kind::Refusal => {
                        json!({"id":item_id,"type":"message","role":"assistant","status":"in_progress","content":[]})
                    }
                    Kind::Reasoning => json!({"id":item_id,"type":"reasoning","summary":[]}),
                    Kind::Tool { id, name } => {
                        json!({"id":item_id,"type":"function_call","call_id":id,"name":name,"arguments":"","status":"in_progress"})
                    }
                };
                self.response_frame(
                    "response.output_item.added",
                    json!({"output_index":index,"item":item}),
                    &mut batch.frames,
                )?;
                match kind {
                    Kind::Text | Kind::Refusal => {
                        let part = if kind == Kind::Text { json!({"type":"output_text","text":"","annotations":[]}) } else { json!({"type":"refusal","refusal":""}) };
                        self.response_frame("response.content_part.added", json!({"item_id":item_id,"output_index":index,"content_index":0,"part":part}), &mut batch.frames)?;
                    }
                    Kind::Reasoning => self.response_frame("response.reasoning_summary_part.added", json!({"item_id":item_id,"output_index":index,"summary_index":0,"part":{"type":"summary_text","text":""}}), &mut batch.frames)?,
                    Kind::Tool { .. } => {}
                }
            }
            Protocol::Gemini => {}
        }
        Ok(())
    }

    fn item_id(&self, index: usize) -> String {
        format!("{}_item_{index}", self.context.id)
    }

    fn tool_index(&self, key: Key) -> Result<usize> {
        let index = self
            .blocks
            .get(&key)
            .ok_or_else(|| fail(ErrorKind::InvalidSequence, "stream.block"))?
            .wire_index;
        Ok(self
            .blocks
            .values()
            .filter(|b| b.wire_index < index && matches!(b.kind, Kind::Tool { .. }))
            .count())
    }

    fn append(&mut self, key: Key, delta: &str, batch: &mut StreamBatch) -> Result<()> {
        let block = self
            .blocks
            .get(&key)
            .ok_or_else(|| fail(ErrorKind::InvalidSequence, "stream.delta_without_start"))?;
        if block.stopped {
            return Err(fail(ErrorKind::InvalidSequence, "stream.delta_after_stop"));
        }
        let kind = block.kind.clone();
        let index = block.wire_index;
        let tool = matches!(kind, Kind::Tool { .. });
        if tool
            && block.value.len().saturating_add(delta.len()) > self.limits.max_tool_argument_bytes
        {
            return Err(fail(ErrorKind::LimitExceeded, "stream.tool.arguments"));
        }
        if tool || self.retains_text() {
            self.reserve(delta.len())?;
            self.blocks
                .get_mut(&key)
                .ok_or_else(|| fail(ErrorKind::InvalidSequence, "stream.block"))?
                .value
                .push_str(delta);
        }
        let item_id = self.item_id(index);
        match self.target {
            Protocol::OpenAiChat => {
                let delta = match kind {
                    Kind::Text => json!({"content":delta}),
                    Kind::Refusal => json!({"refusal":delta}),
                    Kind::Tool { .. } => {
                        json!({"tool_calls":[{"index":self.tool_index(key)?,"function":{"arguments":delta}}]})
                    }
                    Kind::Reasoning => return Err(unsupported("stream.reasoning.chat")),
                };
                self.chat_frame(delta, None, &mut batch.frames)?;
            }
            Protocol::AnthropicMessages => {
                let delta = if tool {
                    json!({"type":"input_json_delta","partial_json":delta})
                } else {
                    json!({"type":"text_delta","text":delta})
                };
                self.frame(
                    "content_block_delta",
                    json!({"type":"content_block_delta","index":index,"delta":delta}),
                    &mut batch.frames,
                )?;
            }
            Protocol::OpenAiResponses => {
                let (event, mut value) = match kind {
                    Kind::Text => (
                        "response.output_text.delta",
                        json!({"content_index":0,"delta":delta}),
                    ),
                    Kind::Refusal => (
                        "response.refusal.delta",
                        json!({"content_index":0,"delta":delta}),
                    ),
                    Kind::Reasoning => (
                        "response.reasoning_summary_text.delta",
                        json!({"summary_index":0,"delta":delta}),
                    ),
                    Kind::Tool { .. } => (
                        "response.function_call_arguments.delta",
                        json!({"delta":delta}),
                    ),
                };
                value["item_id"] = json!(item_id);
                value["output_index"] = json!(index);
                self.response_frame(event, value, &mut batch.frames)?;
            }
            Protocol::Gemini if !tool => {
                let part = if kind == Kind::Reasoning {
                    json!({"text":delta,"thought":true})
                } else {
                    json!({"text":delta})
                };
                self.gemini_parts(vec![part], &mut batch.frames)?;
            }
            Protocol::Gemini => {}
        }
        Ok(())
    }

    fn stop_block(&mut self, key: Key, batch: &mut StreamBatch) -> Result<()> {
        let block = self
            .blocks
            .get(&key)
            .ok_or_else(|| fail(ErrorKind::InvalidSequence, "stream.stop_without_start"))?;
        if block.stopped {
            return Err(fail(ErrorKind::InvalidSequence, "stream.duplicate_stop"));
        }
        let kind = block.kind.clone();
        let index = block.wire_index;
        if matches!(kind, Kind::Tool { .. }) {
            argument_object(
                &self
                    .blocks
                    .get(&key)
                    .ok_or_else(|| fail(ErrorKind::InvalidSequence, "stream.block"))?
                    .value,
            )?;
        }
        let block = self
            .blocks
            .get(&key)
            .ok_or_else(|| fail(ErrorKind::InvalidSequence, "stream.block"))?;
        match self.target {
            Protocol::AnthropicMessages => self.frame(
                "content_block_stop",
                json!({"type":"content_block_stop","index":index}),
                &mut batch.frames,
            )?,
            Protocol::OpenAiResponses => {
                let value = block.value.clone();
                let item_id = self.item_id(index);
                let item = self.output_item(key)?;
                match kind {
                    Kind::Text | Kind::Refusal => {
                        let event = if kind == Kind::Text {
                            "response.output_text.done"
                        } else {
                            "response.refusal.done"
                        };
                        let mut done =
                            json!({"item_id":item_id,"output_index":index,"content_index":0});
                        done[if kind == Kind::Text {
                            "text"
                        } else {
                            "refusal"
                        }] = json!(value);
                        self.response_frame(event, done, &mut batch.frames)?;
                        self.response_frame("response.content_part.done", json!({"item_id":item_id,"output_index":index,"content_index":0,"part":item["content"][0]}), &mut batch.frames)?;
                    }
                    Kind::Reasoning => {
                        self.response_frame("response.reasoning_summary_text.done", json!({"item_id":item_id,"output_index":index,"summary_index":0,"text":value}), &mut batch.frames)?;
                        self.response_frame("response.reasoning_summary_part.done", json!({"item_id":item_id,"output_index":index,"summary_index":0,"part":{"type":"summary_text","text":value}}), &mut batch.frames)?;
                    }
                    Kind::Tool { .. } => self.response_frame(
                        "response.function_call_arguments.done",
                        json!({"item_id":item_id,"output_index":index,"arguments":value}),
                        &mut batch.frames,
                    )?,
                }
                self.response_frame(
                    "response.output_item.done",
                    json!({"output_index":index,"item":item}),
                    &mut batch.frames,
                )?;
            }
            Protocol::Gemini => {
                if let Kind::Tool { id, name } = kind {
                    self.gemini_parts(vec![json!({"functionCall":{"id":id,"name":name,"args":argument_object(&block.value)?}})], &mut batch.frames)?;
                }
            }
            Protocol::OpenAiChat => {}
        }
        self.blocks
            .get_mut(&key)
            .ok_or_else(|| fail(ErrorKind::InvalidSequence, "stream.block"))?
            .stopped = true;
        Ok(())
    }

    fn output_item(&self, key: Key) -> Result<Value> {
        let block = self
            .blocks
            .get(&key)
            .ok_or_else(|| fail(ErrorKind::InvalidSequence, "stream.block"))?;
        let id = self.item_id(block.wire_index);
        Ok(match &block.kind {
            Kind::Text => {
                json!({"type":"message","id":id,"role":"assistant","status":"completed","content":[{"type":"output_text","text":block.value,"annotations":[]}]})
            }
            Kind::Refusal => {
                json!({"type":"message","id":id,"role":"assistant","status":"completed","content":[{"type":"refusal","refusal":block.value}]})
            }
            Kind::Reasoning => {
                json!({"type":"reasoning","id":id,"summary":[{"type":"summary_text","text":block.value}]})
            }
            Kind::Tool { id: call_id, name } => {
                json!({"type":"function_call","id":id,"call_id":call_id,"name":name,"arguments":block.value,"status":"completed"})
            }
        })
    }

    fn gemini_parts(&self, parts: Vec<Value>, frames: &mut Vec<Vec<u8>>) -> Result<()> {
        self.frame("", json!({"responseId":self.context.id,"modelVersion":self.context.model,"candidates":[{"index":0,"content":{"role":"model","parts":parts}}]}), frames)
    }

    fn record_usage(&mut self, patch: &Value) -> Result<()> {
        object(patch, "stream.usage")?;
        // Anthropic message_delta usage is cumulative and may omit initial
        // input/cache counters. Patch counters, never add repeated snapshots.
        if matches!(self.source, Protocol::AnthropicMessages | Protocol::Gemini) {
            let map = self
                .raw_usage
                .as_object_mut()
                .ok_or_else(|| fail(ErrorKind::InvalidSequence, "stream.usage"))?;
            for (key, value) in object(patch, "stream.usage")? {
                if let (Some(old), Some(new)) =
                    (map.get(key).and_then(Value::as_u64), value.as_u64())
                {
                    if new < old {
                        return Err(fail(ErrorKind::InvalidSequence, "stream.usage_regression"));
                    }
                }
                map.insert(key.clone(), value.clone());
            }
        } else {
            if self.usage.is_some() && self.raw_usage != *patch {
                return Err(fail(ErrorKind::InvalidSequence, "stream.conflicting_usage"));
            }
            self.raw_usage = patch.clone();
        }
        self.usage = Some(Usage::decode(self.source, &self.raw_usage)?);
        Ok(())
    }

    fn set_finish(&mut self, reason: FinishReason) -> Result<()> {
        if self.finish_reason.is_some() {
            return Err(fail(ErrorKind::InvalidSequence, "stream.duplicate_finish"));
        }
        // Reject a terminal reason that the target cannot represent before
        // announcing success or publishing a final accounting record.
        response::finish_value(self.target, &reason)?;
        self.finish_reason = Some(reason);
        Ok(())
    }

    fn complete(&mut self, batch: &mut StreamBatch) -> Result<()> {
        if !self.source_started || !self.pending_tools.is_empty() {
            return Err(fail(
                ErrorKind::IncompleteStream,
                "stream.missing_start_or_tool_identity",
            ));
        }
        let reason = self
            .finish_reason
            .clone()
            .ok_or_else(|| fail(ErrorKind::IncompleteStream, "stream.missing_finish"))?;
        if self.source == Protocol::AnthropicMessages && self.blocks.values().any(|b| !b.stopped) {
            return Err(fail(
                ErrorKind::IncompleteStream,
                "stream.open_content_block",
            ));
        }
        if self.source == Protocol::OpenAiResponses
            && (self.items.values().any(|i| !i.done) || self.blocks.values().any(|b| !b.stopped))
        {
            return Err(fail(ErrorKind::IncompleteStream, "stream.open_output_item"));
        }
        if self.target == Protocol::AnthropicMessages
            && self
                .usage
                .as_ref()
                .is_none_or(|u| u.input_tokens.is_none() || u.output_tokens.is_none())
        {
            return Err(unsupported("usage.required_anthropic_counts"));
        }
        let usage = self
            .usage
            .as_ref()
            .map(|u| u.encode(self.target))
            .transpose()?;
        self.header(&mut batch.frames)?;
        let open: Vec<_> = self
            .blocks
            .iter()
            .filter_map(|(key, b)| (!b.stopped).then_some(*key))
            .collect();
        for key in open {
            self.stop_block(key, batch)?;
        }
        let finish = response::finish_value(self.target, &reason)?;
        match self.target {
            Protocol::OpenAiChat => {
                self.chat_frame(json!({}), Some(finish), &mut batch.frames)?;
                if let Some(usage) = usage {
                    self.frame("", json!({"id":self.context.id,"object":"chat.completion.chunk","created":self.context.created,"model":self.context.model,"choices":[],"usage":usage}), &mut batch.frames)?;
                }
                batch.frames.push(b"data: [DONE]\n\n".to_vec());
            }
            Protocol::AnthropicMessages => {
                let mut value = json!({"type":"message_delta","delta":{"stop_reason":finish,"stop_sequence":match &reason {FinishReason::StopSequence(sequence) => json!(sequence),_ => Value::Null}}});
                if let Some(usage) = usage {
                    value["usage"] = usage;
                }
                self.frame("message_delta", value, &mut batch.frames)?;
                self.frame(
                    "message_stop",
                    json!({"type":"message_stop"}),
                    &mut batch.frames,
                )?;
            }
            Protocol::OpenAiResponses => {
                let mut ordered: Vec<_> = self.blocks.iter().collect();
                ordered.sort_by_key(|(_, b)| b.wire_index);
                let output: Vec<Value> = ordered
                    .into_iter()
                    .map(|(key, _)| self.output_item(*key))
                    .collect::<Result<_>>()?;
                let mut value = json!({"id":self.context.id,"object":"response","created_at":self.context.created,"model":self.context.model,"status":finish,"output":output,"error":null,"incomplete_details":null});
                if let Some(usage) = usage {
                    value["usage"] = usage;
                }
                if matches!(reason, FinishReason::Length | FinishReason::ContentFilter) {
                    value["incomplete_details"] = json!({"reason":if reason == FinishReason::Length {"max_output_tokens"} else {"content_filter"}});
                }
                self.response_frame(
                    if finish == "completed" {
                        "response.completed"
                    } else {
                        "response.incomplete"
                    },
                    json!({"response":value}),
                    &mut batch.frames,
                )?;
            }
            Protocol::Gemini => {
                let mut value = json!({"responseId":self.context.id,"modelVersion":self.context.model,"candidates":[{"index":0,"content":{"role":"model","parts":[]},"finishReason":finish}]});
                if let Some(usage) = usage {
                    value["usageMetadata"] = usage;
                }
                self.frame("", value, &mut batch.frames)?;
            }
        }
        batch.end = Some(StreamEnd {
            outcome: Outcome::Finished(reason),
            usage: self.usage.take(),
        });
        self.closed = true;
        self.end_emitted = true;
        Ok(())
    }

    fn fail_upstream(&mut self, error: GatewayError, batch: &mut StreamBatch) -> Result<()> {
        let body = response::error_body(self.target, &error);
        match self.target {
            Protocol::OpenAiResponses => {
                let value = json!({"id":self.context.id,"object":"response","created_at":self.context.created,"model":self.context.model,"status":"failed","output":[],"error":body["error"]});
                self.response_frame(
                    "response.failed",
                    json!({"response":value}),
                    &mut batch.frames,
                )?;
            }
            Protocol::AnthropicMessages => self.frame("error", body, &mut batch.frames)?,
            _ => self.frame("", body, &mut batch.frames)?,
        }
        batch.end = Some(StreamEnd {
            outcome: Outcome::Failed(error.category),
            usage: None,
        });
        self.closed = true;
        self.end_emitted = true;
        Ok(())
    }

    fn chat(&mut self, value: &Value, batch: &mut StreamBatch) -> Result<()> {
        fields(
            value,
            &["id", "object", "created", "model", "choices", "usage"],
            "stream.chat",
        )?;
        self.source_started = true;
        if let Some(usage) = value.get("usage").filter(|v| !v.is_null()) {
            self.record_usage(usage)?;
        }
        let choices = array(
            required(value, "choices", "stream.choices")?,
            "stream.choices",
        )?;
        if choices.is_empty() {
            if value.get("usage").is_none_or(Value::is_null) {
                return Err(fail(ErrorKind::InvalidInput, "stream.empty_choices"));
            }
            return Ok(());
        }
        if choices.len() != 1 {
            return Err(unsupported("stream.multiple_choices"));
        }
        let choice = &choices[0];
        fields(
            choice,
            &["index", "delta", "finish_reason", "logprobs"],
            "stream.choice",
        )?;
        response::index_zero(choice)?;
        if choice.get("logprobs").is_some_and(|v| !v.is_null()) {
            return Err(unsupported("stream.logprobs"));
        }
        let delta = required(choice, "delta", "stream.delta")?;
        fields(
            delta,
            &["role", "content", "tool_calls", "refusal"],
            "stream.delta",
        )?;
        if delta.get("role").is_some_and(|v| v != "assistant") {
            return Err(unsupported("stream.role"));
        }
        if self.finish_reason.is_some()
            && object(delta, "stream.delta")?
                .values()
                .any(|v| !v.is_null())
        {
            return Err(fail(
                ErrorKind::InvalidSequence,
                "stream.delta_after_finish",
            ));
        }
        for (field, key, kind) in [
            ("content", (0, 0), Kind::Text),
            ("refusal", (0, 1), Kind::Refusal),
        ] {
            if let Some(delta) = optional_string(delta, field, "stream.text")? {
                if !delta.is_empty() {
                    if !self.blocks.contains_key(&key) {
                        self.start_block(key, kind, batch)?;
                    }
                    self.append(key, &delta, batch)?;
                }
            }
        }
        if let Some(calls) = delta.get("tool_calls").filter(|v| !v.is_null()) {
            for call in array(calls, "stream.tool_calls")? {
                fields(
                    call,
                    &["index", "id", "type", "function"],
                    "stream.tool_call",
                )?;
                let index = required(call, "index", "stream.tool_call.index")?
                    .as_u64()
                    .ok_or_else(|| fail(ErrorKind::InvalidInput, "stream.tool_call.index"))?;
                if index >= self.limits.max_blocks as u64 {
                    return Err(fail(ErrorKind::LimitExceeded, "stream.tool_index"));
                }
                let key = (index + 1, 0);
                if call
                    .get("type")
                    .is_some_and(|v| !v.is_null() && v != "function")
                {
                    return Err(unsupported("stream.tool_call.type"));
                }
                let empty_function = json!({});
                let function = call
                    .get("function")
                    .filter(|v| !v.is_null())
                    .unwrap_or(&empty_function);
                fields(
                    function,
                    &["name", "arguments"],
                    "stream.tool_call.function",
                )?;
                if !self.blocks.contains_key(&key) {
                    if self.pending_tools.len() >= self.limits.max_blocks {
                        return Err(fail(ErrorKind::LimitExceeded, "stream.pending_tools"));
                    }
                    let id =
                        optional_string(call, "id", "stream.tool_call.id")?.unwrap_or_default();
                    let name = optional_string(function, "name", "stream.tool_call.name")?
                        .unwrap_or_default();
                    self.reserve(id.len().saturating_add(name.len()))?;
                    let entry = self.pending_tools.entry(index).or_default();
                    entry.0.push_str(&id);
                    entry.1.push_str(&name);
                    if let Some(arguments) =
                        optional_string(function, "arguments", "stream.tool.arguments")?
                    {
                        if !arguments.is_empty() {
                            let (id, name) =
                                self.pending_tools.remove(&index).ok_or_else(|| {
                                    fail(ErrorKind::InvalidSequence, "stream.tool.identity")
                                })?;
                            self.retained_bytes -= id.len().saturating_add(name.len());
                            self.start_block(key, Kind::Tool { id, name }, batch)?;
                            self.append(key, &arguments, batch)?;
                        }
                    }
                } else {
                    if call.get("id").is_some_and(|v| !v.is_null())
                        || function.get("name").is_some_and(|v| !v.is_null())
                    {
                        return Err(fail(
                            ErrorKind::InvalidSequence,
                            "stream.tool.late_identity",
                        ));
                    }
                    if let Some(arguments) =
                        optional_string(function, "arguments", "stream.tool.arguments")?
                    {
                        self.append(key, &arguments, batch)?;
                    }
                }
            }
        }
        if let Some(finish) = optional_string(choice, "finish_reason", "stream.finish_reason")? {
            let pending = std::mem::take(&mut self.pending_tools);
            for (index, (id, name)) in pending {
                self.retained_bytes -= id.len().saturating_add(name.len());
                self.start_block((index + 1, 0), Kind::Tool { id, name }, batch)?;
            }
            self.set_finish(response::parse_finish(self.source, &finish, None)?)?;
        }
        Ok(())
    }

    fn anthropic(&mut self, kind: &str, value: &Value, batch: &mut StreamBatch) -> Result<()> {
        match kind {
            "ping" => {
                fields(value, &["type"], "stream.ping")?;
            }
            "message_start" => {
                fields(value, &["type", "message"], "stream.message_start")?;
                if self.source_started {
                    return Err(fail(ErrorKind::InvalidSequence, "stream.duplicate_start"));
                }
                let message = required(value, "message", "stream.message")?;
                fields(
                    message,
                    &[
                        "id",
                        "type",
                        "role",
                        "content",
                        "model",
                        "stop_reason",
                        "stop_sequence",
                        "usage",
                    ],
                    "stream.message",
                )?;
                if text(message, "role", "stream.role")? != "assistant"
                    || !array(
                        required(message, "content", "stream.content")?,
                        "stream.content",
                    )?
                    .is_empty()
                    || message.get("stop_reason").is_some_and(|v| !v.is_null())
                {
                    return Err(fail(ErrorKind::InvalidSequence, "stream.message_start"));
                }
                if let Some(usage) = message.get("usage") {
                    self.record_usage(usage)?;
                }
                self.source_started = true;
                self.header(&mut batch.frames)?;
            }
            "content_block_start" => {
                fields(
                    value,
                    &["type", "index", "content_block"],
                    "stream.block_start",
                )?;
                self.require_content_phase()?;
                let key = (source_index(value, "index")?, 0);
                if key.0 != self.blocks.len() as u64 {
                    return Err(fail(ErrorKind::InvalidSequence, "stream.block_index"));
                }
                let block = required(value, "content_block", "stream.content_block")?;
                let (kind, initial) = match text(block, "type", "stream.block.type")? {
                    "text" => {
                        fields(block, &["type", "text"], "stream.block.text")?;
                        (
                            Kind::Text,
                            text(block, "text", "stream.block.text")?.to_owned(),
                        )
                    }
                    "thinking" => {
                        fields(
                            block,
                            &["type", "thinking", "signature"],
                            "stream.block.thinking",
                        )?;
                        if block
                            .get("signature")
                            .and_then(Value::as_str)
                            .is_some_and(|s| !s.is_empty())
                        {
                            return Err(unsupported("stream.reasoning.signature"));
                        }
                        (
                            Kind::Reasoning,
                            text(block, "thinking", "stream.block.thinking")?.to_owned(),
                        )
                    }
                    "tool_use" => {
                        fields(
                            block,
                            &["type", "id", "name", "input"],
                            "stream.block.tool_use",
                        )?;
                        let input = required(block, "input", "stream.tool.input")?;
                        let input = object(input, "stream.tool.input")?;
                        (
                            Kind::Tool {
                                id: text(block, "id", "stream.tool.id")?.to_owned(),
                                name: text(block, "name", "stream.tool.name")?.to_owned(),
                            },
                            if input.is_empty() {
                                String::new()
                            } else {
                                Value::Object(input.clone()).to_string()
                            },
                        )
                    }
                    _ => return Err(unsupported("stream.block.type")),
                };
                self.start_block(key, kind, batch)?;
                if !initial.is_empty() {
                    self.append(key, &initial, batch)?;
                }
            }
            "content_block_delta" => {
                fields(value, &["type", "index", "delta"], "stream.block_delta")?;
                self.require_content_phase()?;
                let key = (source_index(value, "index")?, 0);
                let delta = required(value, "delta", "stream.delta")?;
                let (expected, field) = match text(delta, "type", "stream.delta.type")? {
                    "text_delta" => (Kind::Text, "text"),
                    "thinking_delta" => (Kind::Reasoning, "thinking"),
                    "input_json_delta" => {
                        let block = self
                            .blocks
                            .get(&key)
                            .ok_or_else(|| fail(ErrorKind::InvalidSequence, "stream.block"))?;
                        if !matches!(block.kind, Kind::Tool { .. }) {
                            return Err(fail(ErrorKind::InvalidSequence, "stream.delta_kind"));
                        }
                        (block.kind.clone(), "partial_json")
                    }
                    "signature_delta" => return Err(unsupported("stream.reasoning.signature")),
                    _ => return Err(unsupported("stream.delta.type")),
                };
                fields(delta, &["type", field], "stream.block_delta")?;
                self.require_kind(key, &expected)?;
                self.append(key, text(delta, field, "stream.delta.text")?, batch)?;
            }
            "content_block_stop" => {
                fields(value, &["type", "index"], "stream.block_stop")?;
                self.require_content_phase()?;
                let key = (source_index(value, "index")?, 0);
                if self
                    .blocks
                    .get(&key)
                    .is_some_and(|b| matches!(b.kind, Kind::Tool { .. }) && b.value.is_empty())
                {
                    // Anthropic's initial empty input object is real input even
                    // when no input_json_delta was sent.
                    self.append(key, "{}", batch)?;
                }
                self.stop_block(key, batch)?;
            }
            "message_delta" => {
                fields(value, &["type", "delta", "usage"], "stream.message_delta")?;
                if !self.source_started {
                    return Err(fail(ErrorKind::InvalidSequence, "stream.missing_start"));
                }
                if self.blocks.values().any(|b| !b.stopped) {
                    return Err(fail(
                        ErrorKind::InvalidSequence,
                        "stream.open_block_at_finish",
                    ));
                }
                let delta = required(value, "delta", "stream.delta")?;
                fields(
                    delta,
                    &["stop_reason", "stop_sequence"],
                    "stream.message_delta",
                )?;
                if let Some(usage) = value.get("usage") {
                    self.record_usage(usage)?;
                }
                if let Some(finish) = optional_string(delta, "stop_reason", "stream.stop_reason")? {
                    self.set_finish(response::parse_finish(
                        self.source,
                        &finish,
                        delta.get("stop_sequence"),
                    )?)?;
                }
            }
            "message_stop" => {
                fields(value, &["type"], "stream.message_stop")?;
                self.complete(batch)?;
            }
            _ => return Err(unsupported("stream.event.type")),
        }
        Ok(())
    }

    fn require_content_phase(&self) -> Result<()> {
        if !self.source_started || self.finish_reason.is_some() {
            return Err(fail(ErrorKind::InvalidSequence, "stream.content_phase"));
        }
        Ok(())
    }

    fn require_kind(&self, key: Key, kind: &Kind) -> Result<()> {
        if self.blocks.get(&key).is_none_or(|b| &b.kind != kind) {
            return Err(fail(ErrorKind::InvalidSequence, "stream.delta_kind"));
        }
        Ok(())
    }

    fn gemini(&mut self, value: &Value, batch: &mut StreamBatch) -> Result<()> {
        fields(
            value,
            &["candidates", "usageMetadata", "modelVersion", "responseId"],
            "stream.gemini",
        )?;
        self.source_started = true;
        if let Some(usage) = value.get("usageMetadata") {
            self.record_usage(usage)?;
        }
        let Some(candidates) = value.get("candidates") else {
            return Ok(());
        };
        if array(candidates, "stream.candidates")?.is_empty() {
            return Ok(());
        }
        let candidate = response::one(candidates, "stream.candidates")?;
        fields(
            candidate,
            &["index", "content", "finishReason"],
            "stream.candidate",
        )?;
        response::index_zero(candidate)?;
        if let Some(content) = candidate.get("content") {
            fields(content, &["role", "parts"], "stream.content")?;
            if content.get("role").is_some_and(|v| v != "model") {
                return Err(unsupported("stream.role"));
            }
            let parts =
                request::decode_parts(self.source, required(content, "parts", "stream.parts")?)?;
            if self.finish_reason.is_some() && !parts.is_empty() {
                return Err(fail(
                    ErrorKind::InvalidSequence,
                    "stream.delta_after_finish",
                ));
            }
            for part in parts {
                match part {
                    Part::Text(delta) => {
                        let key = (0, 0);
                        if !self.blocks.contains_key(&key) {
                            self.start_block(key, Kind::Text, batch)?;
                        }
                        self.append(key, &delta, batch)?;
                    }
                    Part::Reasoning {
                        text,
                        signature: None,
                    } => {
                        let key = (0, 1);
                        if !self.blocks.contains_key(&key) {
                            self.start_block(key, Kind::Reasoning, batch)?;
                        }
                        self.append(key, &text, batch)?;
                    }
                    Part::Call {
                        id,
                        name,
                        arguments,
                    } => {
                        let id = id.ok_or_else(|| unsupported("stream.tool.missing_id"))?;
                        let key = (self.blocks.len() as u64 + 1, 0);
                        self.start_block(key, Kind::Tool { id, name }, batch)?;
                        self.append(key, &arguments, batch)?;
                        self.stop_block(key, batch)?;
                    }
                    _ => return Err(unsupported("stream.gemini.part")),
                }
            }
        }
        if let Some(reason) = optional_string(candidate, "finishReason", "stream.finishReason")? {
            let mut reason = response::parse_finish(self.source, &reason, None)?;
            if reason == FinishReason::Stop
                && self
                    .blocks
                    .values()
                    .any(|b| matches!(b.kind, Kind::Tool { .. }))
            {
                reason = FinishReason::ToolCalls;
            }
            self.set_finish(reason)?;
        }
        Ok(())
    }

    fn responses(&mut self, kind: &str, value: &Value, batch: &mut StreamBatch) -> Result<()> {
        match kind {
            "response.created" | "response.in_progress" => {
                fields(
                    value,
                    &["type", "sequence_number", "response"],
                    "stream.response_start",
                )?;
                if kind == "response.created" && self.source_started {
                    return Err(fail(ErrorKind::InvalidSequence, "stream.duplicate_start"));
                }
                if kind == "response.in_progress" && !self.source_started {
                    return Err(fail(ErrorKind::InvalidSequence, "stream.missing_created"));
                }
                let shell = required(value, "response", "stream.response")?;
                fields(
                    shell,
                    &[
                        "id",
                        "object",
                        "created_at",
                        "status",
                        "model",
                        "output",
                        "usage",
                        "error",
                        "incomplete_details",
                    ],
                    "stream.response",
                )?;
                if shell
                    .get("output")
                    .is_some_and(|v| v.as_array().is_none_or(|a| !a.is_empty()))
                    || shell.get("status") != Some(&json!("in_progress"))
                {
                    return Err(fail(ErrorKind::InvalidSequence, "stream.response_start"));
                }
                self.source_started = true;
                self.header(&mut batch.frames)?;
            }
            "response.output_item.added" => {
                fields(
                    value,
                    &["type", "sequence_number", "output_index", "item"],
                    "stream.output_item.added",
                )?;
                self.require_content_phase()?;
                let index = source_index(value, "output_index")?;
                if self.items.len() >= self.limits.max_blocks {
                    return Err(fail(ErrorKind::LimitExceeded, "stream.items"));
                }
                if index != self.items.len() as u64 {
                    return Err(fail(ErrorKind::InvalidSequence, "stream.output_index"));
                }
                if self.items.contains_key(&index) {
                    return Err(fail(ErrorKind::InvalidSequence, "stream.duplicate_item"));
                }
                let item = required(value, "item", "stream.item")?;
                let id = text(item, "id", "stream.item.id")?.to_owned();
                self.reserve(id.len())?;
                let item_kind = match text(item, "type", "stream.item.type")? {
                    "message" => {
                        fields(
                            item,
                            &["id", "type", "role", "status", "content"],
                            "stream.item.message",
                        )?;
                        if text(item, "role", "stream.role")? != "assistant"
                            || !array(
                                required(item, "content", "stream.content")?,
                                "stream.content",
                            )?
                            .is_empty()
                        {
                            return Err(fail(ErrorKind::InvalidSequence, "stream.message.added"));
                        }
                        ItemKind::Message
                    }
                    "reasoning" => {
                        fields(
                            item,
                            &["id", "type", "summary", "status", "encrypted_content"],
                            "stream.item.reasoning",
                        )?;
                        if item.get("encrypted_content").is_some_and(|v| !v.is_null())
                            || item
                                .get("summary")
                                .is_some_and(|v| v.as_array().is_none_or(|a| !a.is_empty()))
                        {
                            return Err(unsupported("stream.reasoning.encrypted_or_initial"));
                        }
                        ItemKind::Reasoning
                    }
                    "function_call" => {
                        fields(
                            item,
                            &["id", "type", "call_id", "name", "arguments", "status"],
                            "stream.item.function_call",
                        )?;
                        let kind = Kind::Tool {
                            id: text(item, "call_id", "stream.tool.call_id")?.to_owned(),
                            name: text(item, "name", "stream.tool.name")?.to_owned(),
                        };
                        self.start_block((index, 0), kind, batch)?;
                        if let Some(arguments) =
                            optional_string(item, "arguments", "stream.tool.arguments")?
                        {
                            if !arguments.is_empty() {
                                self.append((index, 0), &arguments, batch)?;
                            }
                        }
                        ItemKind::Tool
                    }
                    _ => return Err(unsupported("stream.item.type")),
                };
                self.items.insert(
                    index,
                    Item {
                        id,
                        kind: item_kind,
                        done: false,
                    },
                );
            }
            "response.content_part.added" | "response.reasoning_summary_part.added" => {
                let reasoning = kind == "response.reasoning_summary_part.added";
                fields(
                    value,
                    &[
                        "type",
                        "sequence_number",
                        "item_id",
                        "output_index",
                        "content_index",
                        "summary_index",
                        "part",
                    ],
                    "stream.part.added",
                )?;
                self.require_content_phase()?;
                let key = self.response_key(value, reasoning)?;
                if key.1 != self.blocks.keys().filter(|k| k.0 == key.0).count() as u64 {
                    return Err(fail(ErrorKind::InvalidSequence, "stream.content_index"));
                }
                let item = self
                    .items
                    .get(&key.0)
                    .ok_or_else(|| fail(ErrorKind::InvalidSequence, "stream.part_without_item"))?;
                if item.kind
                    != if reasoning {
                        ItemKind::Reasoning
                    } else {
                        ItemKind::Message
                    }
                {
                    return Err(fail(ErrorKind::InvalidSequence, "stream.part_kind"));
                }
                let part = required(value, "part", "stream.part")?;
                let (kind, initial) = if reasoning {
                    fields(part, &["type", "text"], "stream.summary")?;
                    if text(part, "type", "stream.summary.type")? != "summary_text" {
                        return Err(unsupported("stream.summary.type"));
                    }
                    (Kind::Reasoning, text(part, "text", "stream.summary.text")?)
                } else {
                    match text(part, "type", "stream.part.type")? {
                        "output_text" => {
                            fields(part, &["type", "text", "annotations"], "stream.part.text")?;
                            if part
                                .get("annotations")
                                .is_some_and(|v| v.as_array().is_none_or(|a| !a.is_empty()))
                            {
                                return Err(unsupported("stream.annotations"));
                            }
                            (Kind::Text, text(part, "text", "stream.part.text")?)
                        }
                        "refusal" => {
                            fields(part, &["type", "refusal"], "stream.part.refusal")?;
                            (Kind::Refusal, text(part, "refusal", "stream.refusal")?)
                        }
                        _ => return Err(unsupported("stream.part.type")),
                    }
                };
                self.start_block(key, kind, batch)?;
                if !initial.is_empty() {
                    self.append(key, initial, batch)?;
                }
            }
            "response.output_text.delta"
            | "response.refusal.delta"
            | "response.reasoning_summary_text.delta"
            | "response.function_call_arguments.delta" => {
                fields(
                    value,
                    &[
                        "type",
                        "sequence_number",
                        "item_id",
                        "output_index",
                        "content_index",
                        "summary_index",
                        "delta",
                    ],
                    "stream.response_delta",
                )?;
                self.require_content_phase()?;
                let key = self.response_key(value, kind.contains("reasoning_summary"))?;
                let block = self.blocks.get(&key).ok_or_else(|| {
                    fail(ErrorKind::InvalidSequence, "stream.delta_without_start")
                })?;
                let valid = match kind {
                    "response.output_text.delta" => block.kind == Kind::Text,
                    "response.refusal.delta" => block.kind == Kind::Refusal,
                    "response.reasoning_summary_text.delta" => block.kind == Kind::Reasoning,
                    _ => matches!(block.kind, Kind::Tool { .. }),
                };
                if !valid {
                    return Err(fail(ErrorKind::InvalidSequence, "stream.delta_kind"));
                }
                self.append(key, text(value, "delta", "stream.delta")?, batch)?;
            }
            "response.output_text.done"
            | "response.refusal.done"
            | "response.reasoning_summary_text.done"
            | "response.function_call_arguments.done" => {
                fields(
                    value,
                    &[
                        "type",
                        "sequence_number",
                        "item_id",
                        "output_index",
                        "content_index",
                        "summary_index",
                        "text",
                        "refusal",
                        "arguments",
                    ],
                    "stream.response_done",
                )?;
                let key = self.response_key(value, kind.contains("reasoning_summary"))?;
                let block = self
                    .blocks
                    .get(&key)
                    .ok_or_else(|| fail(ErrorKind::InvalidSequence, "stream.done_without_start"))?;
                let field = if kind == "response.refusal.done" {
                    "refusal"
                } else if kind == "response.function_call_arguments.done" {
                    "arguments"
                } else {
                    "text"
                };
                if text(value, field, "stream.done_value")? != block.value {
                    return Err(fail(ErrorKind::InvalidSequence, "stream.inconsistent_done"));
                }
                if kind == "response.function_call_arguments.done" {
                    if !matches!(block.kind, Kind::Tool { .. }) {
                        return Err(fail(ErrorKind::InvalidSequence, "stream.delta_kind"));
                    }
                    self.stop_block(key, batch)?;
                }
            }
            "response.content_part.done" | "response.reasoning_summary_part.done" => {
                let reasoning = kind == "response.reasoning_summary_part.done";
                fields(
                    value,
                    &[
                        "type",
                        "sequence_number",
                        "item_id",
                        "output_index",
                        "content_index",
                        "summary_index",
                        "part",
                    ],
                    "stream.part.done",
                )?;
                let key = self.response_key(value, reasoning)?;
                let part = required(value, "part", "stream.part")?;
                let block = self
                    .blocks
                    .get(&key)
                    .ok_or_else(|| fail(ErrorKind::InvalidSequence, "stream.done_without_start"))?;
                let expected = match block.kind {
                    Kind::Text => json!({"type":"output_text","text":block.value,"annotations":[]}),
                    Kind::Refusal => json!({"type":"refusal","refusal":block.value}),
                    Kind::Reasoning => json!({"type":"summary_text","text":block.value}),
                    _ => return Err(fail(ErrorKind::InvalidSequence, "stream.part_kind")),
                };
                if *part != expected {
                    return Err(fail(
                        ErrorKind::InvalidSequence,
                        "stream.inconsistent_part_done",
                    ));
                }
                self.stop_block(key, batch)?;
            }
            "response.output_item.done" => {
                fields(
                    value,
                    &["type", "sequence_number", "output_index", "item"],
                    "stream.item.done",
                )?;
                let index = source_index(value, "output_index")?;
                let item = self.items.get(&index).ok_or_else(|| {
                    fail(ErrorKind::InvalidSequence, "stream.item_done_without_start")
                })?;
                if item.done {
                    return Err(fail(
                        ErrorKind::InvalidSequence,
                        "stream.duplicate_item_done",
                    ));
                }
                let wire = required(value, "item", "stream.item")?;
                if text(wire, "id", "stream.item.id")? != item.id {
                    return Err(fail(ErrorKind::InvalidSequence, "stream.item_id"));
                }
                let expected = response::decode_response_output(&json!([wire]))?;
                let observed = self.observed_parts(Some(index))?;
                if expected != observed {
                    return Err(fail(
                        ErrorKind::InvalidSequence,
                        "stream.inconsistent_item_done",
                    ));
                }
                let keys: Vec<_> = self
                    .blocks
                    .iter()
                    .filter_map(|(key, b)| (key.0 == index && !b.stopped).then_some(*key))
                    .collect();
                for key in keys {
                    self.stop_block(key, batch)?;
                }
                self.items
                    .get_mut(&index)
                    .ok_or_else(|| fail(ErrorKind::InvalidSequence, "stream.item"))?
                    .done = true;
            }
            "response.completed" | "response.incomplete" => {
                fields(
                    value,
                    &["type", "sequence_number", "response"],
                    "stream.response_terminal",
                )?;
                let body = required(value, "response", "stream.response")?;
                let decoded = response::decode(self.source, body)?;
                if decoded.parts != self.observed_parts(None)? {
                    return Err(fail(
                        ErrorKind::InvalidSequence,
                        "stream.inconsistent_terminal",
                    ));
                }
                let Outcome::Finished(reason) = decoded.outcome else {
                    return Err(fail(ErrorKind::InvalidSequence, "stream.terminal_status"));
                };
                let status = text(body, "status", "stream.response.status")?;
                if (kind == "response.completed" && status != "completed")
                    || (kind == "response.incomplete" && status != "incomplete")
                {
                    return Err(fail(ErrorKind::InvalidSequence, "stream.terminal_status"));
                }
                if let Some(usage) = body.get("usage").filter(|v| !v.is_null()) {
                    self.record_usage(usage)?;
                }
                self.set_finish(reason)?;
                self.complete(batch)?;
            }
            "response.failed" => {
                fields(
                    value,
                    &["type", "sequence_number", "response"],
                    "stream.response.failed",
                )?;
                self.fail_upstream(
                    response::upstream_error(required(value, "response", "stream.response")?),
                    batch,
                )?;
            }
            "error" => {
                self.fail_upstream(response::upstream_error(value), batch)?;
            }
            _ => return Err(unsupported("stream.event.type")),
        }
        Ok(())
    }

    fn response_key(&self, value: &Value, reasoning: bool) -> Result<Key> {
        let index = source_index(value, "output_index")?;
        let item = self
            .items
            .get(&index)
            .ok_or_else(|| fail(ErrorKind::InvalidSequence, "stream.item_reference"))?;
        if item.done || text(value, "item_id", "stream.item_id")? != item.id {
            return Err(fail(ErrorKind::InvalidSequence, "stream.item_reference"));
        }
        let content = if item.kind == ItemKind::Tool {
            0
        } else {
            source_index(
                value,
                if reasoning {
                    "summary_index"
                } else {
                    "content_index"
                },
            )?
        };
        Ok((index, content))
    }

    fn observed_parts(&self, item: Option<u64>) -> Result<Vec<Part>> {
        let mut out = Vec::new();
        for (key, block) in &self.blocks {
            if item.is_some_and(|index| key.0 != index) {
                continue;
            }
            out.push(match &block.kind {
                Kind::Text => Part::Text(block.value.clone()),
                Kind::Refusal => Part::Refusal(block.value.clone()),
                Kind::Reasoning => Part::Reasoning {
                    text: block.value.clone(),
                    signature: None,
                },
                Kind::Tool { id, name } => Part::Call {
                    id: Some(id.clone()),
                    name: name.clone(),
                    arguments: block.value.clone(),
                },
            });
        }
        Ok(out)
    }
}

fn source_index(value: &Value, field: &str) -> Result<u64> {
    required(value, field, "stream.index")?
        .as_u64()
        .ok_or_else(|| fail(ErrorKind::InvalidInput, "stream.index"))
}
