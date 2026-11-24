//! The agent loop end to end, against a mock provider on 127.0.0.1: what is
//! sent, what is run, what the conversation and the events look like.

mod common;

use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::thread;
use std::time::{Duration, Instant};

use aster_ai::{
    AgentEvent, AgentOptions, AiError, ContentBlock, Message, Preset, Provider, ProviderConfig,
    SYSTEM_PROMPT, StopReason, ToolDef, TurnContext, TurnEnd, connect, run, tool_defs,
    user_message,
};
use aster_tools::ToolResult;
use common::{Block, MockServer, Reply, anthropic, openai};
use serde_json::{Value, json};

fn anthropic_provider(server: &MockServer) -> Box<dyn Provider> {
    let mut config = ProviderConfig::from_preset("anthropic", Preset::Anthropic);
    config.base_url = server.base.clone();
    connect(config, Some("test-key".into()))
}

fn openai_provider(server: &MockServer) -> Box<dyn Provider> {
    let mut config = ProviderConfig::from_preset("local", Preset::Custom);
    config.base_url = format!("{}/v1/", server.base);
    config.model = "test-model".into();
    connect(config, Some("local-key".into()))
}

fn tools() -> Vec<ToolDef> {
    tool_defs(aster_tools::tools())
}

fn ok(text: &str) -> ToolResult {
    ToolResult {
        text: text.into(),
        is_error: false,
    }
}

/// Everything one turn did.
struct Turn {
    result: Result<TurnEnd, AiError>,
    events: Vec<AgentEvent>,
    conversation: Vec<Message>,
}

impl Turn {
    fn text(&self) -> String {
        self.events
            .iter()
            .filter_map(|event| match event {
                AgentEvent::TextDelta(text) => Some(text.as_str()),
                _ => None,
            })
            .collect()
    }
}

fn run_turn(
    provider: &dyn Provider,
    mut conversation: Vec<Message>,
    executor: &(dyn Fn(&str, &Value) -> ToolResult + Sync),
    options: &AgentOptions,
) -> Turn {
    let mut events = Vec::new();
    let cancel = AtomicBool::new(false);
    let result = run(
        provider,
        &mut conversation,
        &tools(),
        &executor,
        options,
        &mut |event| events.push(event),
        &cancel,
    );
    Turn {
        result,
        events,
        conversation,
    }
}

fn hello() -> Vec<Message> {
    vec![user_message("こんにちは", &TurnContext::default())]
}

fn never(_: &str, _: &Value) -> ToolResult {
    panic!("no tool may run")
}

/// The tool results of the last user message of a request, as sent.
fn sent_results(request: &common::Recorded) -> Vec<Value> {
    let body = request.json();
    let messages = body["messages"].as_array().unwrap();
    let last = messages.last().unwrap();
    assert_eq!(last["role"], "user");
    last["content"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|block| block["type"] == "tool_result")
        .cloned()
        .collect()
}

#[test]
fn a_text_reply_streams_and_finishes() {
    let server = MockServer::start(vec![Reply::stream(anthropic(
        &[Block::Text("こんにちは！ご用件は？")],
        "end_turn",
    ))]);
    let provider = anthropic_provider(&server);
    let context = TurnContext {
        document_id: Some("7".into()),
        document_title: Some("Notes.md".into()),
        selection: None,
    };
    let conversation = vec![user_message("Hi", &context)];
    let turn = run_turn(&*provider, conversation, &never, &AgentOptions::default());

    assert_eq!(turn.result, Ok(TurnEnd::Finished));
    assert_eq!(turn.text(), "こんにちは！ご用件は？");
    assert!(matches!(
        turn.events.last(),
        Some(AgentEvent::Done {
            end: TurnEnd::Finished
        })
    ));
    assert!(turn.events.contains(&AgentEvent::StepFinished {
        step: 1,
        stop: StopReason::EndTurn
    }));
    assert_eq!(turn.conversation.len(), 2);
    assert_eq!(turn.conversation[1].text(), "こんにちは！ご用件は？");

    let requests = server.requests();
    assert_eq!(requests.len(), 1);
    let request = &requests[0];
    assert_eq!(request.method, "POST");
    assert_eq!(request.path, "/v1/messages");
    assert_eq!(request.header("x-api-key"), Some("test-key"));
    assert_eq!(request.header("anthropic-version"), Some("2023-06-01"));
    let body = request.json();
    assert_eq!(body["model"], "claude-opus-5-5");
    assert_eq!(body["stream"], true);
    assert_eq!(body["system"], SYSTEM_PROMPT);
    let names: Vec<&str> = body["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|tool| tool["name"].as_str().unwrap())
        .collect();
    let mut sorted = names.clone();
    sorted.sort();
    assert_eq!(names, sorted);
    assert!(body["tools"][0]["input_schema"].is_object());
    let content = &body["messages"][0]["content"];
    assert!(
        content[0]["text"]
            .as_str()
            .unwrap()
            .starts_with("<context>\nCurrent document id: 7\n")
    );
    assert_eq!(content[1]["text"], "Hi");
}

#[test]
fn a_tool_call_round_trip() {
    let server = MockServer::start(vec![
        Reply::stream(anthropic(
            &[
                Block::Text("Reading it."),
                Block::Tool("toolu_1", "read_document", r#"{"document": "1"}"#),
            ],
            "tool_use",
        )),
        Reply::stream(anthropic(&[Block::Text("It says hello.")], "end_turn")),
    ]);
    let provider = anthropic_provider(&server);
    let calls = Mutex::new(Vec::new());
    let executor = |name: &str, input: &Value| {
        calls
            .lock()
            .unwrap()
            .push((name.to_string(), input.clone()));
        ok("hello")
    };
    let turn = run_turn(&*provider, hello(), &executor, &AgentOptions::default());

    assert_eq!(turn.result, Ok(TurnEnd::Finished));
    assert_eq!(
        *calls.lock().unwrap(),
        vec![("read_document".to_string(), json!({"document": "1"}))]
    );
    assert_eq!(turn.text(), "Reading it.It says hello.");
    assert!(turn.events.contains(&AgentEvent::ToolCallStarted {
        id: "toolu_1".into(),
        name: "read_document".into(),
        input: json!({"document": "1"}),
    }));
    assert!(turn.events.contains(&AgentEvent::ToolCallFinished {
        id: "toolu_1".into(),
        name: "read_document".into(),
        result: ok("hello"),
        redacted: 0,
    }));
    // user, assistant (text + call), user (result), assistant (text).
    assert_eq!(turn.conversation.len(), 4);
    assert_eq!(
        turn.conversation[2].content,
        vec![ContentBlock::ToolResult {
            tool_use_id: "toolu_1".into(),
            text: "hello".into(),
            is_error: false,
        }]
    );

    let requests = server.requests();
    assert_eq!(requests.len(), 2);
    let second = requests[1].json();
    let messages = second["messages"].as_array().unwrap();
    assert_eq!(messages.len(), 3);
    assert_eq!(messages[1]["role"], "assistant");
    assert_eq!(messages[1]["content"][1]["type"], "tool_use");
    assert_eq!(messages[1]["content"][1]["input"], json!({"document": "1"}));
    assert_eq!(
        sent_results(&requests[1]),
        vec![json!({"type": "tool_result", "tool_use_id": "toolu_1", "content": "hello"})]
    );
    // The prefix stays byte for byte the same between requests.
    let first = requests[0].json();
    assert_eq!(first["tools"].to_string(), second["tools"].to_string());
    assert_eq!(first["system"], second["system"]);
    assert_eq!(
        first["messages"][0].to_string(),
        second["messages"][0].to_string()
    );
}

#[test]
fn parallel_calls_run_together_and_keep_their_order() {
    let server = MockServer::start(vec![
        Reply::stream(anthropic(
            &[
                Block::Tool("slow", "read_document", r#"{"document": "1"}"#),
                Block::Tool("fast", "get_outline", r#"{"document": "2"}"#),
            ],
            "tool_use",
        )),
        Reply::stream(anthropic(&[Block::Text("Both read.")], "end_turn")),
    ]);
    let provider = anthropic_provider(&server);
    let running = AtomicUsize::new(0);
    let most = AtomicUsize::new(0);
    let executor = |name: &str, _: &Value| {
        let now = running.fetch_add(1, Ordering::SeqCst) + 1;
        most.fetch_max(now, Ordering::SeqCst);
        thread::sleep(Duration::from_millis(if name == "read_document" {
            400
        } else {
            100
        }));
        running.fetch_sub(1, Ordering::SeqCst);
        ok(name)
    };
    let turn = run_turn(&*provider, hello(), &executor, &AgentOptions::default());

    assert_eq!(turn.result, Ok(TurnEnd::Finished));
    assert_eq!(most.load(Ordering::SeqCst), 2, "the calls overlapped");
    // The fast call finished first...
    let finished: Vec<&str> = turn
        .events
        .iter()
        .filter_map(|event| match event {
            AgentEvent::ToolCallFinished { id, .. } => Some(id.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(finished, ["fast", "slow"]);
    // ...but the results go back in one message, in the calls' order.
    let results = sent_results(&server.requests()[1]);
    let ids: Vec<&str> = results
        .iter()
        .map(|result| result["tool_use_id"].as_str().unwrap())
        .collect();
    assert_eq!(ids, ["slow", "fast"]);
    assert_eq!(results[0]["content"], "read_document");
    assert_eq!(results[1]["content"], "get_outline");
}

#[test]
fn invalid_json_input_is_not_run() {
    let server = MockServer::start(vec![
        Reply::stream(anthropic(
            &[Block::Tool("bad", "read_document", r#"{"document": "1"#)],
            "tool_use",
        )),
        Reply::stream(anthropic(&[Block::Text("Retrying later.")], "end_turn")),
    ]);
    let provider = anthropic_provider(&server);
    let turn = run_turn(&*provider, hello(), &never, &AgentOptions::default());

    assert_eq!(turn.result, Ok(TurnEnd::Finished));
    let results = sent_results(&server.requests()[1]);
    assert_eq!(results.len(), 1);
    assert_eq!(results[0]["is_error"], true);
    let text = results[0]["content"].as_str().unwrap();
    assert!(text.starts_with("INVALID_JSON: "), "{text}");
    assert!(text.ends_with(r#"{"document": "1"#), "{text}");
    // The call is sent back with an object as its input, as the API needs.
    let messages = server.requests()[1].json()["messages"].clone();
    assert_eq!(messages[1]["content"][0]["input"], json!({}));
}

#[test]
fn a_reply_cut_at_max_tokens_runs_nothing() {
    let server = MockServer::start(vec![Reply::stream(anthropic(
        &[
            Block::Text("I will replace"),
            Block::Tool("cut", "replace_lines", r#"{"document": "1", "te"#),
        ],
        "max_tokens",
    ))]);
    let provider = anthropic_provider(&server);
    let turn = run_turn(&*provider, hello(), &never, &AgentOptions::default());

    assert_eq!(turn.result, Ok(TurnEnd::MaxTokens));
    assert_eq!(server.requests().len(), 1);
    assert!(
        !turn
            .events
            .iter()
            .any(|event| matches!(event, AgentEvent::ToolCallStarted { .. }))
    );
    let last = turn.conversation.last().unwrap();
    assert_eq!(last.role, aster_ai::Role::User);
    assert!(matches!(
        &last.content[..],
        [ContentBlock::ToolResult { tool_use_id, is_error: true, text }]
            if tool_use_id == "cut" && text.contains("output limit")
    ));
}

#[test]
fn the_step_limit_ends_the_turn() {
    let call = |id: &'static str| {
        Reply::stream(anthropic(
            &[Block::Tool(id, "list_documents", "{}")],
            "tool_use",
        ))
    };
    let server = MockServer::start(vec![call("a"), call("b"), call("c")]);
    let provider = anthropic_provider(&server);
    let count = AtomicUsize::new(0);
    let executor = |_: &str, _: &Value| {
        count.fetch_add(1, Ordering::SeqCst);
        ok("[]")
    };
    let options = AgentOptions {
        max_steps: 2,
        redact: true,
    };
    let turn = run_turn(&*provider, hello(), &executor, &options);

    assert_eq!(turn.result, Ok(TurnEnd::StepLimit));
    assert_eq!(count.load(Ordering::SeqCst), 2);
    assert_eq!(server.requests().len(), 2);
    // The history ends with the last call's result, ready for the next turn.
    assert!(matches!(
        &turn.conversation.last().unwrap().content[..],
        [ContentBlock::ToolResult { tool_use_id, .. }] if tool_use_id == "b"
    ));
}

#[test]
fn cancelling_stops_a_stream_at_once() {
    let mut pieces: Vec<Vec<u8>> = anthropic(&[Block::Text("Thinking about it")], "end_turn")
        .into_iter()
        .map(String::into_bytes)
        .collect();
    // Send everything up to the first text, then go quiet.
    pieces.truncate(3);
    let server = MockServer::start(vec![Reply::Stream {
        pieces,
        pause: Duration::ZERO,
        hold: Duration::from_secs(20),
    }]);
    let provider = anthropic_provider(&server);
    let cancel = AtomicBool::new(false);
    let mut conversation = hello();
    let mut events = Vec::new();
    let started = Instant::now();
    let result = run(
        &*provider,
        &mut conversation,
        &tools(),
        &never,
        &AgentOptions::default(),
        &mut |event| {
            if matches!(event, AgentEvent::TextDelta(_)) {
                cancel.store(true, Ordering::SeqCst);
            }
            events.push(event);
        },
        &cancel,
    );
    assert_eq!(result, Err(AiError::Cancelled));
    assert!(started.elapsed() < Duration::from_secs(5));
    assert_eq!(events.last(), Some(&AgentEvent::Error(AiError::Cancelled)));
    // The unfinished reply is not kept.
    assert_eq!(conversation, hello());
}

#[test]
fn calls_left_by_a_stopped_turn_are_answered_first() {
    let server = MockServer::start(vec![Reply::stream(anthropic(
        &[Block::Text("OK.")],
        "end_turn",
    ))]);
    let provider = anthropic_provider(&server);
    let conversation = vec![
        user_message("Read it", &TurnContext::default()),
        Message::assistant(vec![ContentBlock::ToolUse {
            id: "left".into(),
            name: "read_document".into(),
            input: json!({"document": "1"}),
            raw_input: None,
        }]),
        user_message("Never mind", &TurnContext::default()),
    ];
    let turn = run_turn(&*provider, conversation, &never, &AgentOptions::default());

    assert_eq!(turn.result, Ok(TurnEnd::Finished));
    let body = server.requests()[0].json();
    let last = &body["messages"][2];
    assert_eq!(
        last["content"][0],
        json!({"type": "tool_result", "tool_use_id": "left", "content": "Not run: cancelled by the user.", "is_error": true})
    );
    assert_eq!(
        last["content"][1],
        json!({"type": "text", "text": "Never mind"})
    );
}

#[test]
fn a_refused_key_is_an_auth_error() {
    let server = MockServer::start(vec![Reply::Status {
        status: 401,
        body: json!({"type": "error", "error": {"type": "authentication_error", "message": "invalid x-api-key"}}).to_string(),
    }]);
    let provider = anthropic_provider(&server);
    let turn = run_turn(&*provider, hello(), &never, &AgentOptions::default());

    let expected = AiError::Auth {
        status: 401,
        message: "invalid x-api-key".into(),
    };
    assert_eq!(turn.result, Err(expected.clone()));
    assert_eq!(turn.events.last(), Some(&AgentEvent::Error(expected)));
    assert_eq!(turn.conversation, hello());
}

#[test]
fn secrets_are_masked_before_they_are_sent() {
    let server = MockServer::start(vec![
        Reply::stream(anthropic(
            &[Block::Tool("t", "read_document", r#"{"document": "1"}"#)],
            "tool_use",
        )),
        Reply::stream(anthropic(&[Block::Text("Done.")], "end_turn")),
    ]);
    let provider = anthropic_provider(&server);
    let conversation = vec![user_message(
        "My DB password=hunter2, please check the config",
        &TurnContext::default(),
    )];
    let executor = |_: &str, _: &Value| ok("token = ghp_0123456789abcdefghijABCDEFGHIJ012345");
    let turn = run_turn(
        &*provider,
        conversation,
        &executor,
        &AgentOptions::default(),
    );

    assert_eq!(turn.result, Ok(TurnEnd::Finished));
    assert!(turn.events.contains(&AgentEvent::Redacted { count: 1 }));
    let requests = server.requests();
    for request in &requests {
        assert!(!request.body.contains("hunter2"));
        assert!(!request.body.contains("ghp_0123"));
    }
    assert!(requests[0].body.contains("password=[REDACTED]"));
    assert!(turn.events.iter().any(|event| matches!(
        event,
        AgentEvent::ToolCallFinished { redacted: 1, result, .. } if result.text == "token = [REDACTED]"
    )));

    // With redaction off, the text goes as typed.
    let server = MockServer::start(vec![Reply::stream(anthropic(
        &[Block::Text("OK.")],
        "end_turn",
    ))]);
    let provider = anthropic_provider(&server);
    let conversation = vec![user_message("password=hunter2", &TurnContext::default())];
    let options = AgentOptions {
        redact: false,
        ..AgentOptions::default()
    };
    let turn = run_turn(&*provider, conversation, &never, &options);
    assert_eq!(turn.result, Ok(TurnEnd::Finished));
    assert!(server.requests()[0].body.contains("hunter2"));
}

#[test]
fn an_openai_compatible_round_trip() {
    let server = MockServer::start(vec![
        Reply::stream(openai(
            &[
                Block::Text("Let me look."),
                Block::Tool("call_1", "read_document", r#"{"document": "1"}"#),
                Block::Tool("call_2", "get_outline", r#"{"document": "1"}"#),
            ],
            "tool_calls",
        )),
        Reply::stream(openai(&[Block::Text("見出しは三つです。")], "stop")),
    ]);
    let provider = openai_provider(&server);
    let executor = |name: &str, _: &Value| ok(&format!("{name} result"));
    let turn = run_turn(&*provider, hello(), &executor, &AgentOptions::default());

    assert_eq!(turn.result, Ok(TurnEnd::Finished));
    assert_eq!(turn.text(), "Let me look.見出しは三つです。");
    let requests = server.requests();
    assert_eq!(requests.len(), 2);
    assert_eq!(requests[0].path, "/v1/chat/completions");
    assert_eq!(
        requests[0].header("authorization"),
        Some("Bearer local-key")
    );
    let first = requests[0].json();
    assert_eq!(first["model"], "test-model");
    assert_eq!(
        first["messages"][0],
        json!({"role": "system", "content": SYSTEM_PROMPT})
    );
    assert_eq!(first["tools"][0]["type"], "function");
    assert!(first["tools"][0]["function"]["parameters"].is_object());

    let second = requests[1].json();
    let messages = second["messages"].as_array().unwrap();
    assert_eq!(messages.len(), 5);
    assert_eq!(messages[2]["role"], "assistant");
    assert_eq!(messages[2]["content"], "Let me look.");
    assert_eq!(messages[2]["tool_calls"][1]["id"], "call_2");
    assert_eq!(
        messages[2]["tool_calls"][0]["function"]["arguments"],
        r#"{"document":"1"}"#
    );
    assert_eq!(
        messages[3],
        json!({"role": "tool", "tool_call_id": "call_1", "content": "read_document result"})
    );
    assert_eq!(
        messages[4],
        json!({"role": "tool", "tool_call_id": "call_2", "content": "get_outline result"})
    );
    assert_eq!(first["tools"].to_string(), second["tools"].to_string());
}

#[test]
fn an_openai_reply_cut_by_length_runs_nothing() {
    let server = MockServer::start(vec![Reply::stream(openai(
        &[Block::Tool(
            "call_1",
            "insert_text",
            r#"{"document": "1", "te"#,
        )],
        "length",
    ))]);
    let provider = openai_provider(&server);
    let turn = run_turn(&*provider, hello(), &never, &AgentOptions::default());
    assert_eq!(turn.result, Ok(TurnEnd::MaxTokens));
    assert!(matches!(
        &turn.conversation.last().unwrap().content[..],
        [ContentBlock::ToolResult { is_error: true, .. }]
    ));
}

#[test]
fn model_lists_come_from_both_formats() {
    let server = MockServer::start(vec![
        Reply::json(json!({"data": [
            {"id": "claude-sonnet-5-5", "display_name": "Claude Sonnet 5.5", "type": "model"},
            {"id": "claude-opus-5-5", "display_name": "Claude Opus 5.5", "type": "model"}
        ], "has_more": false})),
        Reply::json(json!({"object": "list", "data": [{"id": "qwen3:8b"}, {"id": "llama3.2"}]})),
        Reply::Status {
            status: 500,
            body: json!({"error": {"message": "boom"}}).to_string(),
        },
    ]);
    let cancel = AtomicBool::new(false);
    let models = anthropic_provider(&server).list_models(&cancel).unwrap();
    assert_eq!(models[0].id, "claude-opus-5-5");
    assert_eq!(models[0].name.as_deref(), Some("Claude Opus 5.5"));
    let models = openai_provider(&server).list_models(&cancel).unwrap();
    let ids: Vec<&str> = models.iter().map(|model| model.id.as_str()).collect();
    assert_eq!(ids, ["llama3.2", "qwen3:8b"]);
    assert_eq!(
        openai_provider(&server).list_models(&cancel),
        Err(AiError::Server {
            status: 500,
            message: "boom".into()
        })
    );
    let requests = server.requests();
    assert_eq!(requests[0].method, "GET");
    assert_eq!(requests[0].path, "/v1/models?limit=1000");
    assert_eq!(requests[0].header("x-api-key"), Some("test-key"));
    assert_eq!(requests[1].path, "/v1/models");
}

#[test]
fn a_provider_without_a_model_says_so() {
    let config = ProviderConfig::from_preset("deepseek", Preset::DeepSeek);
    let provider = connect(config, None);
    let turn = run_turn(&*provider, hello(), &never, &AgentOptions::default());
    assert_eq!(turn.result.unwrap_err().kind(), "not_configured");
}
