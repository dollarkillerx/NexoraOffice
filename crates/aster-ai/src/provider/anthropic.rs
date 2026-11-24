//! Anthropic's Messages API (`POST {base}/v1/messages`), streamed.
//!
//! The reply's blocks arrive as `content_block_start`, a run of
//! `content_block_delta`s and `content_block_stop`, keyed by index; tool
//! inputs come as fragments of JSON text (`input_json_delta`) that are only
//! parsed once the block is complete. Thinking blocks are kept as opaque
//! blocks and sent back unchanged, which the API needs for the model to
//! keep its reasoning across tool calls.
//!
//! Against the API itself (the default base URL) a request also turns on
//! three things a proxy or a compatible server might reject, so they are
//! left off for any other base URL: prompt caching, eager tool input
//! streaming (inputs arrive as they are written, unvalidated; this crate
//! parses them strictly anyway), and the server-side fallback that retries
//! a declined request on another model.

use std::collections::BTreeMap;
use std::sync::atomic::AtomicBool;

use serde_json::{Value, json};

use super::{
    Decoder, ModelInfo, ModelResponse, Provider, ProviderConfig, Request, StopReason, add_text,
    check_tool_count, check_tool_input, check_tool_name, cut_off, decode, fetch_json, models_from,
    parse_input, parse_json,
};
use crate::conversation::{ContentBlock, Message, Role};
use crate::error::{AiError, shorten};
use crate::http::{self, HttpRequest, Method};
use crate::sse;

pub(crate) const DEFAULT_BASE_URL: &str = "https://api.anthropic.com";
pub(crate) const DEFAULT_MODEL: &str = "claude-opus-5-5";
const API_VERSION: &str = "2023-06-01";
/// Room for long answers and long tool inputs (a rewritten section); the
/// stream has no total timeout, so a high limit costs nothing.
const MAX_TOKENS: u32 = 64_000;
const FALLBACK_BETA: &str = "server-side-fallback-2026-07-01";
/// The models the server-side fallback is offered for.
const FALLBACK_MODELS: &[&str] = &[
    "claude-fable-5-2",
    "claude-fable-5-1",
    "claude-opus-5-5",
    "claude-opus-5",
    "claude-sonnet-5-5",
];
pub(crate) const PROVIDER: &str = "anthropic";

pub(crate) struct Anthropic {
    config: ProviderConfig,
    api_key: Option<String>,
    agent: ureq::Agent,
}

impl Anthropic {
    pub(crate) fn new(config: ProviderConfig, api_key: Option<String>) -> Self {
        Self {
            config,
            api_key,
            agent: http::agent(),
        }
    }

    fn headers(&self) -> Vec<(&'static str, String)> {
        let mut headers = vec![("anthropic-version", API_VERSION.to_string())];
        if let Some(key) = &self.api_key {
            headers.push(("x-api-key", key.clone()));
        }
        headers
    }
}

impl Provider for Anthropic {
    fn config(&self) -> &ProviderConfig {
        &self.config
    }

    fn stream(
        &self,
        request: &Request<'_>,
        on_text: &mut dyn FnMut(&str),
        cancel: &AtomicBool,
    ) -> Result<ModelResponse, AiError> {
        if self.config.model.trim().is_empty() {
            return Err(AiError::NotConfigured("no model is chosen".into()));
        }
        let official = self.config.base() == DEFAULT_BASE_URL;
        let body = body(&self.config, request, official);
        let mut headers = self.headers();
        headers.push(("content-type", "application/json".into()));
        headers.push(("accept", "text/event-stream".into()));
        if body.get("fallbacks").is_some() {
            headers.push(("anthropic-beta", FALLBACK_BETA.into()));
        }
        let http_request = HttpRequest {
            method: Method::Post,
            url: format!("{}/v1/messages", self.config.base()),
            headers,
            body: Some(body.to_string()),
        };
        self.config
            .check_request(&http_request.url, self.api_key.is_some())?;
        let response = http::send(&self.agent, http_request);
        decode(response, Stream::default(), on_text, cancel)
    }

    fn list_models(&self, cancel: &AtomicBool) -> Result<Vec<ModelInfo>, AiError> {
        let request = HttpRequest {
            method: Method::Get,
            url: format!("{}/v1/models?limit=1000", self.config.base()),
            headers: self.headers(),
            body: None,
        };
        self.config
            .check_request(&request.url, self.api_key.is_some())?;
        let list = fetch_json(&self.agent, request, cancel)?;
        models_from(&list, Some("display_name"))
    }
}

/// The request body. `official` turns on what only the API itself takes.
pub(crate) fn body(config: &ProviderConfig, request: &Request<'_>, official: bool) -> Value {
    let tools: Vec<Value> = request
        .tools
        .iter()
        .map(|tool| {
            let mut wire = json!({
                "name": tool.name,
                "description": tool.description,
                "input_schema": tool.schema,
            });
            if official {
                wire["eager_input_streaming"] = Value::Bool(true);
            }
            wire
        })
        .collect();
    let mut body = json!({
        "model": config.model,
        "max_tokens": MAX_TOKENS,
        "stream": true,
        "system": request.system,
        "messages": messages(request.messages),
    });
    if !tools.is_empty() {
        body["tools"] = Value::Array(tools);
    }
    if official {
        // Cache the prefix (tools, system prompt, history) automatically.
        body["cache_control"] = json!({ "type": "ephemeral" });
        if FALLBACK_MODELS.contains(&config.model.as_str()) {
            body["fallbacks"] = json!("default");
        }
    }
    body
}

/// The conversation in the API's form. Messages of the same role in a row
/// are merged, with tool results first in a user message, as the API
/// requires; empty blocks and blocks of other providers are left out.
pub(crate) fn messages(messages: &[Message]) -> Vec<Value> {
    let mut merged: Vec<(Role, Vec<Value>)> = Vec::new();
    for message in messages {
        let blocks: Vec<Value> = message.content.iter().filter_map(block).collect();
        if blocks.is_empty() {
            continue;
        }
        match merged.last_mut() {
            Some((role, existing)) if *role == message.role => existing.extend(blocks),
            _ => merged.push((message.role, blocks)),
        }
    }
    merged
        .into_iter()
        .map(|(role, mut blocks)| {
            if role == Role::User {
                blocks.sort_by_key(|block| block["type"] != "tool_result");
            }
            let role = match role {
                Role::User => "user",
                Role::Assistant => "assistant",
            };
            json!({ "role": role, "content": blocks })
        })
        .collect()
}

fn block(block: &ContentBlock) -> Option<Value> {
    match block {
        ContentBlock::Text { text } if text.is_empty() => None,
        ContentBlock::Text { text } => Some(json!({ "type": "text", "text": text })),
        ContentBlock::ToolUse {
            id, name, input, ..
        } => Some(json!({ "type": "tool_use", "id": id, "name": name, "input": input })),
        ContentBlock::ToolResult {
            tool_use_id,
            text,
            is_error,
        } => {
            let mut result = json!({
                "type": "tool_result",
                "tool_use_id": tool_use_id,
                "content": if text.is_empty() { "(no output)" } else { text.as_str() },
            });
            if *is_error {
                result["is_error"] = Value::Bool(true);
            }
            Some(result)
        }
        ContentBlock::Opaque { provider, block } if provider == PROVIDER => Some(block.clone()),
        ContentBlock::Opaque { .. } => None,
    }
}

enum Partial {
    Text(String),
    Tool {
        id: String,
        name: String,
        json: String,
    },
    /// A thinking, redacted thinking or fallback block, or any type this
    /// crate does not know, kept as the API sent it.
    Opaque(Value),
}

/// The stream decoder.
#[derive(Default)]
pub(crate) struct Stream {
    blocks: BTreeMap<u64, Partial>,
    stop: Option<StopReason>,
    done: bool,
    /// Characters kept so far, of text, thinking and other blocks, against
    /// [`crate::limits::MAX_RESPONSE_CHARS`].
    chars: usize,
    /// Tool calls started so far.
    tool_calls: usize,
}

impl Decoder for Stream {
    fn event(&mut self, event: &sse::Event, on_text: &mut dyn FnMut(&str)) -> Result<(), AiError> {
        let data = parse_json(&event.data)?;
        let kind = data
            .get("type")
            .and_then(Value::as_str)
            .unwrap_or(&event.name);
        let index = data.get("index").and_then(Value::as_u64);
        match (kind, index) {
            ("content_block_start", Some(index)) => {
                let block = &data["content_block"];
                let partial = match block.get("type").and_then(Value::as_str) {
                    Some("text") => {
                        let text = block.get("text").and_then(Value::as_str).unwrap_or("");
                        add_text(&mut self.chars, text)?;
                        if !text.is_empty() {
                            on_text(text);
                        }
                        Partial::Text(text.to_string())
                    }
                    Some("tool_use") => {
                        let name = string(block, "name");
                        check_tool_name(&name)?;
                        self.tool_calls += 1;
                        check_tool_count(self.tool_calls)?;
                        Partial::Tool {
                            id: string(block, "id"),
                            name,
                            json: String::new(),
                        }
                    }
                    _ => {
                        // Thinking and unknown blocks are kept and sent
                        // back, so they count against the reply's size too.
                        add_text(&mut self.chars, &event.data)?;
                        Partial::Opaque(block.clone())
                    }
                };
                self.blocks.insert(index, partial);
            }
            ("content_block_delta", Some(index)) => {
                let delta = &data["delta"];
                let delta_type = delta.get("type").and_then(Value::as_str).unwrap_or("");
                match (self.blocks.get_mut(&index), delta_type) {
                    (Some(Partial::Text(text)), "text_delta") => {
                        let more = delta.get("text").and_then(Value::as_str).unwrap_or("");
                        add_text(&mut self.chars, more)?;
                        text.push_str(more);
                        if !more.is_empty() {
                            on_text(more);
                        }
                    }
                    (Some(Partial::Tool { json, .. }), "input_json_delta") => {
                        let more = delta
                            .get("partial_json")
                            .and_then(Value::as_str)
                            .unwrap_or("");
                        check_tool_input(json.len() + more.len())?;
                        json.push_str(more);
                    }
                    (Some(Partial::Opaque(block)), "thinking_delta") => {
                        let more = delta.get("thinking").and_then(Value::as_str).unwrap_or("");
                        add_text(&mut self.chars, more)?;
                        // Appended in place: copying the whole thinking text
                        // for every small delta would take time growing
                        // with the square of its length.
                        match block.get_mut("thinking") {
                            Some(Value::String(thinking)) => thinking.push_str(more),
                            _ => block["thinking"] = Value::String(more.to_string()),
                        }
                    }
                    (Some(Partial::Opaque(block)), "signature_delta") => {
                        block["signature"] = delta.get("signature").cloned().unwrap_or_default();
                    }
                    _ => {}
                }
            }
            ("message_delta", _) => {
                if let Some(reason) = data.pointer("/delta/stop_reason").and_then(Value::as_str) {
                    self.stop = Some(stop_reason(reason));
                }
            }
            ("message_stop", _) => self.done = true,
            ("error", _) => return Err(stream_error(&data["error"])),
            _ => {}
        }
        Ok(())
    }

    fn finish(self) -> Result<ModelResponse, AiError> {
        if !self.done {
            return Err(cut_off());
        }
        let mut content: Vec<ContentBlock> = self
            .blocks
            .into_values()
            .filter_map(|partial| match partial {
                Partial::Text(text) if text.is_empty() => None,
                Partial::Text(text) => Some(ContentBlock::Text { text }),
                Partial::Tool { id, name, json } => {
                    let (input, raw_input) = parse_input(&json);
                    Some(ContentBlock::ToolUse {
                        id,
                        name,
                        input,
                        raw_input,
                    })
                }
                Partial::Opaque(block) => Some(ContentBlock::Opaque {
                    provider: PROVIDER.into(),
                    block,
                }),
            })
            .collect();
        drop_declined_attempt(&mut content);
        Ok(ModelResponse {
            content,
            stop: self
                .stop
                .unwrap_or_else(|| StopReason::Other("unknown".into())),
        })
    }
}

/// After a server-side fallback, the blocks before the last `fallback`
/// block belong to the declined attempt. Its text stays (it was shown and
/// the fallback model continued from it), but its thinking, tool calls and
/// other internal blocks must not be run or sent back.
fn drop_declined_attempt(content: &mut Vec<ContentBlock>) {
    let is_fallback = |block: &ContentBlock| matches!(block, ContentBlock::Opaque { block, .. } if block["type"] == "fallback");
    let Some(last) = content.iter().rposition(is_fallback) else {
        return;
    };
    let mut index = 0;
    content.retain(|block| {
        let keep =
            index >= last || matches!(block, ContentBlock::Text { .. }) || is_fallback(block);
        index += 1;
        keep
    });
}

fn string(value: &Value, field: &str) -> String {
    value
        .get(field)
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string()
}

fn stop_reason(reason: &str) -> StopReason {
    match reason {
        "end_turn" | "stop_sequence" => StopReason::EndTurn,
        "tool_use" => StopReason::ToolUse,
        "max_tokens" => StopReason::MaxTokens,
        "refusal" => StopReason::Refusal,
        other => StopReason::Other(other.to_string()),
    }
}

/// An `error` event in the middle of a stream (the HTTP status was 200).
fn stream_error(error: &Value) -> AiError {
    let message = shorten(error.get("message").and_then(Value::as_str).unwrap_or(""));
    match error.get("type").and_then(Value::as_str).unwrap_or("") {
        "authentication_error" => AiError::Auth {
            status: 401,
            message,
        },
        "permission_error" => AiError::Auth {
            status: 403,
            message,
        },
        "rate_limit_error" => AiError::RateLimited {
            status: 429,
            message,
            retry_after: None,
        },
        "overloaded_error" => AiError::Server {
            status: 529,
            message,
        },
        "api_error" => AiError::Server {
            status: 500,
            message,
        },
        "invalid_request_error" => AiError::InvalidRequest {
            status: 400,
            message,
        },
        _ => AiError::BadResponse(message),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::provider::{Preset, ToolDef};

    fn event(name: &str, data: Value) -> String {
        format!("event: {name}\ndata: {data}\n\n")
    }

    /// A stream with text, a thinking block and a tool call whose input
    /// comes in pieces.
    fn tool_stream() -> String {
        [
            event("message_start", json!({"type":"message_start","message":{"id":"msg_1","content":[]}})),
            event("content_block_start", json!({"type":"content_block_start","index":0,"content_block":{"type":"thinking","thinking":"","signature":""}})),
            event("content_block_delta", json!({"type":"content_block_delta","index":0,"delta":{"type":"thinking_delta","thinking":"Let me"}})),
            event("content_block_delta", json!({"type":"content_block_delta","index":0,"delta":{"type":"signature_delta","signature":"sig=="}})),
            event("content_block_stop", json!({"type":"content_block_stop","index":0})),
            event("content_block_start", json!({"type":"content_block_start","index":1,"content_block":{"type":"text","text":""}})),
            event("content_block_delta", json!({"type":"content_block_delta","index":1,"delta":{"type":"text_delta","text":"読んで"}})),
            event("content_block_delta", json!({"type":"content_block_delta","index":1,"delta":{"type":"text_delta","text":"みます。"}})),
            event("content_block_stop", json!({"type":"content_block_stop","index":1})),
            "event: ping\ndata: {\"type\": \"ping\"}\n\n".to_string(),
            event("content_block_start", json!({"type":"content_block_start","index":2,"content_block":{"type":"tool_use","id":"toolu_1","name":"read_document","input":{}}})),
            event("content_block_delta", json!({"type":"content_block_delta","index":2,"delta":{"type":"input_json_delta","partial_json":"{\"docu"}})),
            event("content_block_delta", json!({"type":"content_block_delta","index":2,"delta":{"type":"input_json_delta","partial_json":"ment\": \"3\"}"}})),
            event("content_block_stop", json!({"type":"content_block_stop","index":2})),
            event("message_delta", json!({"type":"message_delta","delta":{"stop_reason":"tool_use"},"usage":{"output_tokens":20}})),
            event("message_stop", json!({"type":"message_stop"})),
        ]
        .concat()
    }

    fn run(chunks: &[&[u8]]) -> (Result<ModelResponse, AiError>, String) {
        let mut parser = sse::Parser::default();
        let mut stream = Stream::default();
        let mut text = String::new();
        let mut on_text = |t: &str| text.push_str(t);
        for chunk in chunks {
            let events = match parser.feed(chunk) {
                Ok(events) => events,
                Err(error) => return (Err(error), text),
            };
            for event in events {
                if let Err(error) = stream.event(&event, &mut on_text) {
                    return (Err(error), text);
                }
            }
        }
        (stream.finish(), text)
    }

    #[test]
    fn a_tool_call_is_assembled_from_its_pieces_however_the_bytes_are_split() {
        let sse = tool_stream();
        let bytes = sse.as_bytes();
        let (whole, text) = run(&[bytes]);
        let whole = whole.unwrap();
        assert_eq!(text, "読んでみます。");
        assert_eq!(whole.stop, StopReason::ToolUse);
        assert_eq!(
            whole.content,
            vec![
                ContentBlock::Opaque {
                    provider: "anthropic".into(),
                    block: json!({"type":"thinking","thinking":"Let me","signature":"sig=="}),
                },
                ContentBlock::text("読んでみます。"),
                ContentBlock::ToolUse {
                    id: "toolu_1".into(),
                    name: "read_document".into(),
                    input: json!({"document": "3"}),
                    raw_input: None,
                },
            ]
        );
        for cut in (0..bytes.len()).step_by(7) {
            let (split, split_text) = run(&[&bytes[..cut], &bytes[cut..]]);
            assert_eq!(split.unwrap(), whole, "cut at {cut}");
            assert_eq!(split_text, text);
        }
    }

    #[test]
    fn a_stream_without_message_stop_is_cut_off() {
        let sse = tool_stream();
        let cut = sse.find("event: message_stop").unwrap();
        let (result, _) = run(&[&sse.as_bytes()[..cut]]);
        assert_eq!(result.unwrap_err().kind(), "network");
    }

    #[test]
    fn error_events_become_errors() {
        let sse = event(
            "error",
            json!({"type":"error","error":{"type":"overloaded_error","message":"Overloaded"}}),
        );
        let (result, _) = run(&[sse.as_bytes()]);
        assert_eq!(
            result.unwrap_err(),
            AiError::Server {
                status: 529,
                message: "Overloaded".into()
            }
        );
    }

    #[test]
    fn the_declined_attempt_before_a_fallback_is_dropped() {
        let sse = [
            event("content_block_start", json!({"type":"content_block_start","index":0,"content_block":{"type":"thinking","thinking":"","signature":"a"}})),
            event("content_block_start", json!({"type":"content_block_start","index":1,"content_block":{"type":"text","text":"Part"}})),
            event("content_block_start", json!({"type":"content_block_start","index":2,"content_block":{"type":"tool_use","id":"t0","name":"x","input":{}}})),
            event("content_block_start", json!({"type":"content_block_start","index":3,"content_block":{"type":"fallback","model":"claude-opus-4-8"}})),
            event("content_block_start", json!({"type":"content_block_start","index":4,"content_block":{"type":"text","text":"ial"}})),
            event("message_delta", json!({"type":"message_delta","delta":{"stop_reason":"end_turn"}})),
            event("message_stop", json!({"type":"message_stop"})),
        ]
        .concat();
        let (result, _) = run(&[sse.as_bytes()]);
        let content = result.unwrap().content;
        assert_eq!(content.len(), 3);
        assert_eq!(content[0], ContentBlock::text("Part"));
        assert!(
            matches!(&content[1], ContentBlock::Opaque { block, .. } if block["type"] == "fallback")
        );
        assert_eq!(content[2], ContentBlock::text("ial"));
    }

    #[test]
    fn messages_merge_and_put_tool_results_first() {
        let history = vec![
            Message::user(vec![ContentBlock::text("Hi")]),
            Message::assistant(vec![
                ContentBlock::Opaque {
                    provider: "openai".into(),
                    block: json!({}),
                },
                ContentBlock::ToolUse {
                    id: "t1".into(),
                    name: "list_documents".into(),
                    input: json!({}),
                    raw_input: None,
                },
            ]),
            Message::user(vec![ContentBlock::ToolResult {
                tool_use_id: "t1".into(),
                text: String::new(),
                is_error: true,
            }]),
            Message::user(vec![ContentBlock::text("")]),
            Message::user(vec![ContentBlock::text("And now?")]),
        ];
        let wire = messages(&history);
        assert_eq!(
            Value::Array(wire),
            json!([
                {"role": "user", "content": [{"type": "text", "text": "Hi"}]},
                {"role": "assistant", "content": [{"type": "tool_use", "id": "t1", "name": "list_documents", "input": {}}]},
                {"role": "user", "content": [
                    {"type": "tool_result", "tool_use_id": "t1", "content": "(no output)", "is_error": true},
                    {"type": "text", "text": "And now?"}
                ]}
            ])
        );
    }

    #[test]
    fn api_only_options_go_to_the_api_only() {
        let config = ProviderConfig::from_preset("a", Preset::Anthropic);
        let tools = vec![ToolDef {
            name: "t".into(),
            description: "d".into(),
            schema: json!({"type": "object"}),
        }];
        let request = Request {
            system: "S",
            tools: &tools,
            messages: &[],
        };
        let official = body(&config, &request, true);
        assert_eq!(official["model"], "claude-opus-5-5");
        assert_eq!(official["stream"], true);
        assert_eq!(official["system"], "S");
        assert_eq!(
            official["tools"][0]["input_schema"],
            json!({"type": "object"})
        );
        assert_eq!(official["tools"][0]["eager_input_streaming"], true);
        assert_eq!(official["cache_control"]["type"], "ephemeral");
        assert_eq!(official["fallbacks"], "default");
        let proxy = body(&config, &request, false);
        assert!(proxy["tools"][0].get("eager_input_streaming").is_none());
        assert!(proxy.get("cache_control").is_none());
        assert!(proxy.get("fallbacks").is_none());
    }

    fn start(index: usize, block: Value) -> String {
        event(
            "content_block_start",
            json!({"type":"content_block_start","index":index,"content_block":block}),
        )
    }

    fn delta(index: usize, delta: Value) -> String {
        event(
            "content_block_delta",
            json!({"type":"content_block_delta","index":index,"delta":delta}),
        )
    }

    #[test]
    fn too_much_text_ends_the_reply_before_it_is_shown() {
        use crate::limits::MAX_RESPONSE_CHARS;
        let piece = "あ".repeat(100_000);
        let mut sse = start(0, json!({"type":"text","text":""}));
        for _ in 0..(MAX_RESPONSE_CHARS / 100_000) {
            sse.push_str(&delta(0, json!({"type":"text_delta","text":piece})));
        }
        // Exactly at the limit is still fine...
        let (result, text) = run(&[sse.as_bytes()]);
        assert_eq!(
            result.unwrap_err().kind(),
            "network",
            "cut off, not too large"
        );
        assert_eq!(text.chars().count(), MAX_RESPONSE_CHARS);
        // ...one character more is not, and it never reaches the caller.
        sse.push_str(&delta(0, json!({"type":"text_delta","text":"!"})));
        let (result, text) = run(&[sse.as_bytes()]);
        assert_eq!(result.unwrap_err().kind(), "too_large");
        assert!(!text.ends_with('!'));
    }

    #[test]
    fn thinking_counts_against_the_text_limit() {
        use crate::limits::MAX_RESPONSE_CHARS;
        let piece = "t".repeat(500_000);
        let mut sse = start(0, json!({"type":"thinking","thinking":"","signature":""}));
        for _ in 0..(MAX_RESPONSE_CHARS / 500_000 + 1) {
            sse.push_str(&delta(0, json!({"type":"thinking_delta","thinking":piece})));
        }
        let (result, _) = run(&[sse.as_bytes()]);
        assert_eq!(result.unwrap_err().kind(), "too_large");
    }

    #[test]
    fn tool_calls_are_bounded() {
        use crate::limits::{MAX_TOOL_CALLS, MAX_TOOL_INPUT_BYTES};
        let finished = [
            event(
                "message_delta",
                json!({"type":"message_delta","delta":{"stop_reason":"tool_use"}}),
            ),
            event("message_stop", json!({"type":"message_stop"})),
        ]
        .concat();
        let calls = |count: usize| {
            let mut sse = String::new();
            for index in 0..count {
                sse.push_str(&start(index, json!({"type":"tool_use","id":format!("t{index}"),"name":"list_documents","input":{}})));
            }
            sse + &finished
        };
        let (result, _) = run(&[calls(MAX_TOOL_CALLS).as_bytes()]);
        assert_eq!(result.unwrap().content.len(), MAX_TOOL_CALLS);
        let (result, _) = run(&[calls(MAX_TOOL_CALLS + 1).as_bytes()]);
        assert_eq!(result.unwrap_err().kind(), "too_large");

        // A name with characters outside A-Z a-z 0-9 _ -, or too long.
        for name in ["read document", "x\u{202e}y", &"n".repeat(129)] {
            let sse = start(
                0,
                json!({"type":"tool_use","id":"t","name":name,"input":{}}),
            ) + &finished;
            let (result, _) = run(&[sse.as_bytes()]);
            assert_eq!(result.unwrap_err().kind(), "bad_response", "{name:?}");
        }

        // An input that grows past the limit, a piece at a time.
        let piece = "a".repeat(256 * 1024);
        let mut sse = start(
            0,
            json!({"type":"tool_use","id":"t","name":"insert_text","input":{}}),
        );
        sse.push_str(&delta(
            0,
            json!({"type":"input_json_delta","partial_json":"{\"text\":\""}),
        ));
        for _ in 0..(MAX_TOOL_INPUT_BYTES / piece.len()) {
            sse.push_str(&delta(
                0,
                json!({"type":"input_json_delta","partial_json":piece}),
            ));
        }
        sse.push_str(&finished);
        let (result, _) = run(&[sse.as_bytes()]);
        assert_eq!(result.unwrap_err().kind(), "too_large");
    }

    #[test]
    fn thinking_is_appended_in_place() {
        let mut sse = start(0, json!({"type":"thinking","thinking":"a","signature":""}));
        for part in ["b", "c"] {
            sse.push_str(&delta(0, json!({"type":"thinking_delta","thinking":part})));
        }
        // A block that started without the field still gets it.
        sse.push_str(&start(1, json!({"type":"thinking","signature":""})));
        sse.push_str(&delta(1, json!({"type":"thinking_delta","thinking":"d"})));
        sse.push_str(&event(
            "message_delta",
            json!({"type":"message_delta","delta":{"stop_reason":"end_turn"}}),
        ));
        sse.push_str(&event("message_stop", json!({"type":"message_stop"})));
        let (result, _) = run(&[sse.as_bytes()]);
        let content = result.unwrap().content;
        assert!(
            matches!(&content[0], ContentBlock::Opaque { block, .. } if block["thinking"] == "abc")
        );
        assert!(
            matches!(&content[1], ContentBlock::Opaque { block, .. } if block["thinking"] == "d")
        );
    }
}
