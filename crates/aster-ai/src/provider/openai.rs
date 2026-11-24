//! OpenAI-compatible Chat Completions (`POST {base}/chat/completions`),
//! streamed: OpenAI, DeepSeek and local servers such as Ollama.
//!
//! Each chunk is a `data:` line with a `choices[0].delta`; text comes as
//! `content` pieces and tool calls as `tool_calls` pieces that carry an
//! `index`, an id and name in the first piece and the arguments as JSON
//! text spread over the rest. The stream ends with `data: [DONE]`.
//!
//! Compatible servers differ in the details, so the decoder is lenient:
//! a missing `[DONE]` is fine after a `finish_reason`, a tool call without
//! an id gets one, and arguments sent as an object instead of text are
//! taken as they are.

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use serde_json::{Value, json};

use super::{
    Decoder, ModelInfo, ModelResponse, Provider, ProviderConfig, Request, StopReason, add_text,
    check_tool_count, check_tool_input, check_tool_name, cut_off, decode, fetch_json, models_from,
    parse_input, parse_json,
};
use crate::conversation::{ContentBlock, Message, Role};
use crate::error::{AiError, shorten};
use crate::http::{self, HttpRequest, Method};
use crate::limits::MAX_TOOL_NAME_BYTES;
use crate::sse;

pub(crate) struct OpenAi {
    config: ProviderConfig,
    api_key: Option<String>,
    agent: ureq::Agent,
}

impl OpenAi {
    pub(crate) fn new(config: ProviderConfig, api_key: Option<String>) -> Self {
        Self {
            config,
            api_key,
            agent: http::agent(),
        }
    }

    fn headers(&self) -> Vec<(&'static str, String)> {
        let mut headers = Vec::new();
        if let Some(key) = &self.api_key {
            headers.push(("authorization", format!("Bearer {key}")));
        }
        headers
    }
}

impl Provider for OpenAi {
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
        let mut headers = self.headers();
        headers.push(("content-type", "application/json".into()));
        headers.push(("accept", "text/event-stream".into()));
        let http_request = HttpRequest {
            method: Method::Post,
            url: format!("{}/chat/completions", self.config.base()),
            headers,
            body: Some(body(&self.config, request).to_string()),
        };
        self.config
            .check_request(&http_request.url, self.api_key.is_some())?;
        let response = http::send(&self.agent, http_request);
        decode(response, Stream::default(), on_text, cancel)
    }

    fn list_models(&self, cancel: &AtomicBool) -> Result<Vec<ModelInfo>, AiError> {
        let request = HttpRequest {
            method: Method::Get,
            url: format!("{}/models", self.config.base()),
            headers: self.headers(),
            body: None,
        };
        self.config
            .check_request(&request.url, self.api_key.is_some())?;
        let list = fetch_json(&self.agent, request, cancel)?;
        models_from(&list, None)
    }
}

pub(crate) fn body(config: &ProviderConfig, request: &Request<'_>) -> Value {
    let mut wire = vec![json!({ "role": "system", "content": request.system })];
    wire.extend(messages(request.messages));
    let mut body = json!({
        "model": config.model,
        "stream": true,
        "messages": wire,
    });
    if !request.tools.is_empty() {
        let tools: Vec<Value> = request
            .tools
            .iter()
            .map(|tool| {
                json!({
                    "type": "function",
                    "function": {
                        "name": tool.name,
                        "description": tool.description,
                        "parameters": tool.schema,
                    }
                })
            })
            .collect();
        body["tools"] = Value::Array(tools);
    }
    body
}

/// The conversation in Chat Completions form: tool results become `tool`
/// messages (before the user's text, right after the call they answer),
/// text blocks are joined, and opaque blocks are left out.
pub(crate) fn messages(messages: &[Message]) -> Vec<Value> {
    let mut wire = Vec::new();
    for message in messages {
        match message.role {
            Role::User => {
                let mut texts = Vec::new();
                for block in &message.content {
                    match block {
                        ContentBlock::ToolResult {
                            tool_use_id,
                            text,
                            is_error,
                        } => {
                            // The format has no error flag; say it in the text.
                            let content = if *is_error {
                                format!("Error: {text}")
                            } else {
                                text.clone()
                            };
                            wire.push(json!({
                                "role": "tool",
                                "tool_call_id": tool_use_id,
                                "content": content,
                            }));
                        }
                        ContentBlock::Text { text } if !text.is_empty() => {
                            texts.push(text.as_str())
                        }
                        _ => {}
                    }
                }
                if !texts.is_empty() {
                    wire.push(json!({ "role": "user", "content": texts.join("\n\n") }));
                }
            }
            Role::Assistant => {
                let text = message.text();
                let calls: Vec<Value> = message
                    .content
                    .iter()
                    .filter_map(|block| match block {
                        ContentBlock::ToolUse {
                            id,
                            name,
                            input,
                            raw_input,
                        } => Some(json!({
                            "id": id,
                            "type": "function",
                            "function": {
                                "name": name,
                                // Send back what the model wrote, even when
                                // it was not valid JSON.
                                "arguments": raw_input.clone().unwrap_or_else(|| input.to_string()),
                            }
                        })),
                        _ => None,
                    })
                    .collect();
                if text.is_empty() && calls.is_empty() {
                    continue;
                }
                let mut assistant = json!({
                    "role": "assistant",
                    "content": if text.is_empty() { Value::Null } else { Value::String(text) },
                });
                if !calls.is_empty() {
                    assistant["tool_calls"] = Value::Array(calls);
                }
                wire.push(assistant);
            }
        }
    }
    wire
}

#[derive(Default)]
struct Call {
    id: String,
    name: String,
    arguments: String,
}

/// The stream decoder.
#[derive(Default)]
pub(crate) struct Stream {
    text: String,
    /// Characters of `text`, against [`crate::limits::MAX_RESPONSE_CHARS`].
    chars: usize,
    calls: BTreeMap<u64, Call>,
    finish: Option<String>,
    done: bool,
}

/// Ids for tool calls a server sent without one; unique for the process.
static NEXT_CALL: AtomicU64 = AtomicU64::new(1);

impl Decoder for Stream {
    fn event(&mut self, event: &sse::Event, on_text: &mut dyn FnMut(&str)) -> Result<(), AiError> {
        if event.data.trim() == "[DONE]" {
            self.done = true;
            return Ok(());
        }
        let data = parse_json(&event.data)?;
        if let Some(error) = data.get("error") {
            return Err(stream_error(error));
        }
        // A chunk without choices carries only usage.
        let Some(choice) = data.pointer("/choices/0") else {
            return Ok(());
        };
        let delta = &choice["delta"];
        if let Some(text) = delta.get("content").and_then(Value::as_str)
            && !text.is_empty()
        {
            add_text(&mut self.chars, text)?;
            self.text.push_str(text);
            on_text(text);
        }
        if let Some(calls) = delta.get("tool_calls").and_then(Value::as_array) {
            for (position, piece) in calls.iter().enumerate() {
                let index = piece
                    .get("index")
                    .and_then(Value::as_u64)
                    .unwrap_or(position as u64);
                // Each new index is a new call; the server picks the
                // indices, so the count is what is bounded.
                if !self.calls.contains_key(&index) {
                    check_tool_count(self.calls.len() + 1)?;
                }
                let call = self.calls.entry(index).or_default();
                if let Some(id) = piece.get("id").and_then(Value::as_str)
                    && !id.is_empty()
                {
                    call.id = id.to_string();
                }
                if let Some(name) = piece.pointer("/function/name").and_then(Value::as_str)
                    && call.name != name
                {
                    // Some servers send the name in pieces, so it is bounded
                    // as it grows; whether it is a valid name is checked
                    // once it is complete.
                    if call.name.len() + name.len() > MAX_TOOL_NAME_BYTES {
                        return Err(AiError::BadResponse(format!(
                            "the model asked for a tool with a name longer than {MAX_TOOL_NAME_BYTES} bytes"
                        )));
                    }
                    call.name.push_str(name);
                }
                match piece.pointer("/function/arguments") {
                    Some(Value::String(more)) => {
                        check_tool_input(call.arguments.len() + more.len())?;
                        call.arguments.push_str(more);
                    }
                    Some(object @ Value::Object(_)) => {
                        let text = object.to_string();
                        check_tool_input(text.len())?;
                        call.arguments = text;
                    }
                    _ => {}
                }
            }
        }
        if let Some(reason) = choice.get("finish_reason").and_then(Value::as_str) {
            self.finish = Some(reason.to_string());
        }
        Ok(())
    }

    fn finish(self) -> Result<ModelResponse, AiError> {
        let Some(finish) = self.finish else {
            return Err(cut_off());
        };
        let mut content = Vec::new();
        if !self.text.is_empty() {
            content.push(ContentBlock::Text { text: self.text });
        }
        for call in self.calls.values() {
            check_tool_name(&call.name)?;
        }
        for call in self.calls.into_values() {
            let id = if call.id.is_empty() {
                format!("call_{}", NEXT_CALL.fetch_add(1, Ordering::Relaxed))
            } else {
                call.id
            };
            let (input, raw_input) = parse_input(&call.arguments);
            content.push(ContentBlock::ToolUse {
                id,
                name: call.name,
                input,
                raw_input,
            });
        }
        let stop = match finish.as_str() {
            "stop" => StopReason::EndTurn,
            "tool_calls" | "function_call" => StopReason::ToolUse,
            "length" => StopReason::MaxTokens,
            "content_filter" => StopReason::Refusal,
            other => StopReason::Other(other.to_string()),
        };
        Ok(ModelResponse { content, stop })
    }
}

/// An error object in the middle of a stream.
fn stream_error(error: &Value) -> AiError {
    let message = shorten(
        error
            .get("message")
            .and_then(Value::as_str)
            .or_else(|| error.as_str())
            .unwrap_or(""),
    );
    let code = [error.get("code"), error.get("type")]
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .collect::<Vec<_>>()
        .join(" ");
    if code.contains("rate_limit") || code.contains("insufficient_quota") {
        AiError::RateLimited {
            status: 429,
            message,
            retry_after: None,
        }
    } else if code.contains("invalid_api_key") || code.contains("authentication") {
        AiError::Auth {
            status: 401,
            message,
        }
    } else if code.contains("server_error") {
        AiError::Server {
            status: 500,
            message,
        }
    } else {
        AiError::BadResponse(message)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn data(value: Value) -> String {
        format!("data: {value}\n\n")
    }

    fn tool_stream() -> String {
        [
            data(json!({"choices":[{"index":0,"delta":{"role":"assistant","content":""}}]})),
            data(json!({"choices":[{"index":0,"delta":{"content":"Let me "}}]})),
            data(json!({"choices":[{"index":0,"delta":{"content":"check."}}]})),
            data(json!({"choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"id":"call_a","type":"function","function":{"name":"read_document","arguments":""}}]}}]})),
            data(json!({"choices":[{"index":0,"delta":{"tool_calls":[{"index":1,"id":"call_b","type":"function","function":{"name":"get_outline","arguments":"{\"document\":"}}]}}]})),
            data(json!({"choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"function":{"arguments":"{\"document\": "}}]}}]})),
            data(json!({"choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"function":{"arguments":"\"1\"}"}}]}}]})),
            data(json!({"choices":[{"index":0,"delta":{"tool_calls":[{"index":1,"function":{"arguments":"\"2\"}"}}]}}]})),
            data(json!({"choices":[{"index":0,"delta":{},"finish_reason":"tool_calls"}]})),
            data(json!({"choices":[],"usage":{"total_tokens":9}})),
            "data: [DONE]\n\n".to_string(),
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
    fn tool_calls_are_assembled_by_index_however_the_bytes_are_split() {
        let sse = tool_stream();
        let bytes = sse.as_bytes();
        let (whole, text) = run(&[bytes]);
        let whole = whole.unwrap();
        assert_eq!(text, "Let me check.");
        assert_eq!(whole.stop, StopReason::ToolUse);
        assert_eq!(
            whole.content,
            vec![
                ContentBlock::text("Let me check."),
                ContentBlock::ToolUse {
                    id: "call_a".into(),
                    name: "read_document".into(),
                    input: json!({"document": "1"}),
                    raw_input: None,
                },
                ContentBlock::ToolUse {
                    id: "call_b".into(),
                    name: "get_outline".into(),
                    input: json!({"document": "2"}),
                    raw_input: None,
                },
            ]
        );
        for cut in (0..bytes.len()).step_by(5) {
            let (split, _) = run(&[&bytes[..cut], &bytes[cut..]]);
            assert_eq!(split.unwrap(), whole, "cut at {cut}");
        }
    }

    #[test]
    fn a_stream_without_a_finish_reason_is_cut_off() {
        let sse = data(json!({"choices":[{"index":0,"delta":{"content":"Hel"}}]}));
        let (result, text) = run(&[sse.as_bytes()]);
        assert_eq!(text, "Hel");
        assert_eq!(result.unwrap_err().kind(), "network");
    }

    #[test]
    fn lenient_servers_are_understood() {
        // No ids, arguments as an object, no [DONE].
        let sse = [
            data(json!({"choices":[{"delta":{"tool_calls":[{"function":{"name":"list_documents","arguments":{}}}]}}]})),
            data(json!({"choices":[{"delta":{},"finish_reason":"length"}]})),
        ]
        .concat();
        let (result, _) = run(&[sse.as_bytes()]);
        let response = result.unwrap();
        assert_eq!(response.stop, StopReason::MaxTokens);
        let ContentBlock::ToolUse { id, input, .. } = &response.content[0] else {
            panic!("{:?}", response.content)
        };
        assert!(id.starts_with("call_"));
        assert_eq!(input, &json!({}));
    }

    #[test]
    fn the_conversation_becomes_chat_messages() {
        let history = vec![
            Message::user(vec![
                ContentBlock::text("<context>\nx\n</context>"),
                ContentBlock::text("Hi"),
            ]),
            Message::assistant(vec![
                ContentBlock::Opaque {
                    provider: "anthropic".into(),
                    block: json!({}),
                },
                ContentBlock::ToolUse {
                    id: "c1".into(),
                    name: "read_document".into(),
                    input: json!({}),
                    raw_input: Some("{bad".into()),
                },
            ]),
            Message::user(vec![ContentBlock::ToolResult {
                tool_use_id: "c1".into(),
                text: "INVALID_JSON: x".into(),
                is_error: true,
            }]),
        ];
        assert_eq!(
            Value::Array(messages(&history)),
            json!([
                {"role": "user", "content": "<context>\nx\n</context>\n\nHi"},
                {"role": "assistant", "content": null, "tool_calls": [
                    {"id": "c1", "type": "function", "function": {"name": "read_document", "arguments": "{bad"}}
                ]},
                {"role": "tool", "tool_call_id": "c1", "content": "Error: INVALID_JSON: x"}
            ])
        );
    }

    fn finished() -> String {
        data(json!({"choices":[{"index":0,"delta":{},"finish_reason":"tool_calls"}]}))
    }

    fn call(index: usize, name: &str, arguments: &str) -> String {
        data(
            json!({"choices":[{"index":0,"delta":{"tool_calls":[{"index":index,"id":format!("call_{index}"),"type":"function","function":{"name":name,"arguments":arguments}}]}}]}),
        )
    }

    #[test]
    fn too_much_text_ends_the_reply() {
        use crate::limits::MAX_RESPONSE_CHARS;
        let piece = "é".repeat(200_000);
        let mut sse = String::new();
        for _ in 0..(MAX_RESPONSE_CHARS / 200_000) {
            sse.push_str(&data(
                json!({"choices":[{"index":0,"delta":{"content":piece}}]}),
            ));
        }
        let (result, text) = run(&[(sse.clone() + &finished()).as_bytes()]);
        assert!(result.is_ok());
        assert_eq!(text.chars().count(), MAX_RESPONSE_CHARS);
        sse.push_str(&data(
            json!({"choices":[{"index":0,"delta":{"content":"!"}}]}),
        ));
        let (result, text) = run(&[sse.as_bytes()]);
        assert_eq!(result.unwrap_err().kind(), "too_large");
        assert!(!text.ends_with('!'));
    }

    #[test]
    fn tool_calls_are_bounded() {
        use crate::limits::{MAX_TOOL_CALLS, MAX_TOOL_INPUT_BYTES};
        let calls = |count: usize| {
            let mut sse = String::new();
            for index in 0..count {
                sse.push_str(&call(index, "list_documents", "{}"));
            }
            sse + &finished()
        };
        let (result, _) = run(&[calls(MAX_TOOL_CALLS).as_bytes()]);
        assert_eq!(result.unwrap().content.len(), MAX_TOOL_CALLS);
        let (result, _) = run(&[calls(MAX_TOOL_CALLS + 1).as_bytes()]);
        assert_eq!(result.unwrap_err().kind(), "too_large");
        // Far-apart indices are still separate calls.
        let mut sparse = String::new();
        for index in 0..=MAX_TOOL_CALLS {
            sparse.push_str(&call(index * 1_000_000, "list_documents", "{}"));
        }
        let (result, _) = run(&[sparse.as_bytes()]);
        assert_eq!(result.unwrap_err().kind(), "too_large");

        for name in ["read document", "a\nb", "", &"n".repeat(129)] {
            let sse = call(0, name, "{}") + &finished();
            let (result, _) = run(&[sse.as_bytes()]);
            assert_eq!(result.unwrap_err().kind(), "bad_response", "{name:?}");
        }
        // A name sent in pieces that add up past the limit.
        let mut sse = String::new();
        for piece in 0..3 {
            sse.push_str(&call(0, &format!("{piece}{}", "n".repeat(60)), ""));
        }
        let (result, _) = run(&[(sse + &finished()).as_bytes()]);
        assert_eq!(result.unwrap_err().kind(), "bad_response");

        // Arguments that grow past the limit, a piece at a time.
        let piece = "a".repeat(256 * 1024);
        let mut sse = call(0, "insert_text", "{\"text\":\"");
        for _ in 0..(MAX_TOOL_INPUT_BYTES / piece.len()) {
            sse.push_str(&call(0, "insert_text", &piece));
        }
        let (result, _) = run(&[(sse + &finished()).as_bytes()]);
        assert_eq!(result.unwrap_err().kind(), "too_large");
    }

    #[test]
    fn an_event_past_the_line_limit_ends_the_stream() {
        use crate::limits::MAX_EVENT_BYTES;
        let sse =
            data(json!({"choices":[{"index":0,"delta":{"content":"x".repeat(MAX_EVENT_BYTES)}}]}));
        let (result, text) = run(&[sse.as_bytes()]);
        assert_eq!(result.unwrap_err().kind(), "too_large");
        assert!(text.is_empty());
    }
}
