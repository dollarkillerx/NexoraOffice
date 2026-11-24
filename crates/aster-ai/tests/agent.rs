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
fn a_redirect_is_not_followed_with_the_key() {
    let elsewhere = MockServer::start(vec![Reply::stream(anthropic(
        &[Block::Text("should not be asked")],
        "end_turn",
    ))]);
    let server = MockServer::start(vec![Reply::Redirect {
        location: format!("{}/v1/messages", elsewhere.base),
    }]);
    let provider = anthropic_provider(&server);
    let turn = run_turn(&*provider, hello(), &never, &AgentOptions::default());

    assert!(turn.result.is_err(), "{:?}", turn.result);
    assert_eq!(server.requests().len(), 1);
    assert!(elsewhere.requests().is_empty());
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

#[test]
fn many_calls_run_a_few_at_a_time_and_keep_their_order() {
    use aster_ai::limits::MAX_PARALLEL_TOOL_CALLS;
    let count = MAX_PARALLEL_TOOL_CALLS * 2 + 3;
    let ids: Vec<String> = (0..count).map(|index| format!("call{index:02}")).collect();
    let inputs: Vec<String> = (0..count)
        .map(|index| format!(r#"{{"document": "{index}"}}"#))
        .collect();
    let blocks: Vec<Block> = (0..count)
        .map(|index| Block::Tool(&ids[index], "read_document", &inputs[index]))
        .collect();
    let server = MockServer::start(vec![
        Reply::stream(anthropic(&blocks, "tool_use")),
        Reply::stream(anthropic(&[Block::Text("All read.")], "end_turn")),
    ]);
    let provider = anthropic_provider(&server);
    let running = AtomicUsize::new(0);
    let most = AtomicUsize::new(0);
    let executor = |_: &str, input: &Value| {
        let now = running.fetch_add(1, Ordering::SeqCst) + 1;
        most.fetch_max(now, Ordering::SeqCst);
        // Later calls finish sooner, so finishing order differs from the
        // calls' order.
        let document: u64 = input["document"].as_str().unwrap().parse().unwrap();
        thread::sleep(Duration::from_millis(150 - document * 5));
        running.fetch_sub(1, Ordering::SeqCst);
        ok(&format!("doc {document}"))
    };
    let turn = run_turn(&*provider, hello(), &executor, &AgentOptions::default());

    assert_eq!(turn.result, Ok(TurnEnd::Finished));
    assert_eq!(
        most.load(Ordering::SeqCst),
        MAX_PARALLEL_TOOL_CALLS,
        "never more at once than the cap, and the cap was reached"
    );
    let finished = turn
        .events
        .iter()
        .filter(|event| matches!(event, AgentEvent::ToolCallFinished { .. }))
        .count();
    assert_eq!(finished, count);
    let results = sent_results(&server.requests()[1]);
    let sent: Vec<(&str, &str)> = results
        .iter()
        .map(|result| {
            (
                result["tool_use_id"].as_str().unwrap(),
                result["content"].as_str().unwrap(),
            )
        })
        .collect();
    let expected: Vec<(String, String)> = (0..count)
        .map(|index| (ids[index].clone(), format!("doc {index}")))
        .collect();
    let expected: Vec<(&str, &str)> = expected
        .iter()
        .map(|(id, text)| (id.as_str(), text.as_str()))
        .collect();
    assert_eq!(sent, expected);
}

#[test]
fn too_many_tool_calls_end_the_turn_with_nothing_run() {
    use aster_ai::limits::MAX_TOOL_CALLS;
    let ids: Vec<String> = (0..=MAX_TOOL_CALLS)
        .map(|index| format!("c{index}"))
        .collect();
    let blocks: Vec<Block> = ids
        .iter()
        .map(|id| Block::Tool(id, "list_documents", "{}"))
        .collect();
    for openai_format in [false, true] {
        let events = if openai_format {
            openai(&blocks, "tool_calls")
        } else {
            anthropic(&blocks, "tool_use")
        };
        let server = MockServer::start(vec![Reply::stream(events)]);
        let provider = if openai_format {
            openai_provider(&server)
        } else {
            anthropic_provider(&server)
        };
        let turn = run_turn(&*provider, hello(), &never, &AgentOptions::default());
        let error = turn.result.unwrap_err();
        assert_eq!(error.kind(), "too_large", "{error}");
        assert_eq!(turn.events.last(), Some(&AgentEvent::Error(error)));
        assert_eq!(turn.conversation, hello(), "nothing of the reply is kept");
    }
}

#[test]
fn an_invalid_tool_name_ends_the_turn() {
    for openai_format in [false, true] {
        let blocks = [Block::Tool("c", "read_document\u{1b}[31m", "{}")];
        let events = if openai_format {
            openai(&blocks, "tool_calls")
        } else {
            anthropic(&blocks, "tool_use")
        };
        let server = MockServer::start(vec![Reply::stream(events)]);
        let provider = if openai_format {
            openai_provider(&server)
        } else {
            anthropic_provider(&server)
        };
        let turn = run_turn(&*provider, hello(), &never, &AgentOptions::default());
        assert_eq!(turn.result.unwrap_err().kind(), "bad_response");
    }
}

#[test]
fn an_endless_line_ends_the_turn() {
    use aster_ai::limits::MAX_EVENT_BYTES;
    // A line with no end, sent in pieces well past the limit.
    let mut pieces = vec![b"event: content_block_delta\ndata: ".to_vec()];
    pieces.extend(std::iter::repeat_n(
        vec![b'x'; 256 * 1024],
        MAX_EVENT_BYTES / (256 * 1024) + 2,
    ));
    let server = MockServer::start(vec![Reply::Stream {
        pieces,
        pause: Duration::ZERO,
        hold: Duration::ZERO,
    }]);
    let provider = anthropic_provider(&server);
    let turn = run_turn(&*provider, hello(), &never, &AgentOptions::default());
    assert_eq!(turn.result.unwrap_err().kind(), "too_large");
}

fn custom(base_url: &str) -> ProviderConfig {
    let mut config = ProviderConfig::from_preset("custom", Preset::Custom);
    config.base_url = base_url.into();
    config.model = "test-model".into();
    config
}

#[test]
fn plain_http_is_refused_for_other_computers_before_anything_is_sent() {
    let cancel = AtomicBool::new(false);
    for base in [
        "http://192.168.1.5:8080/v1",
        "http://localhost.evil.com/v1",
        "http://127.0.0.1@evil.com/v1",
    ] {
        let provider = connect(custom(base), Some("secret".into()));
        let turn = run_turn(&*provider, hello(), &never, &AgentOptions::default());
        let error = turn.result.unwrap_err();
        assert_eq!(error.kind(), "not_configured", "{base}: {error}");
        assert!(error.to_string().contains("http://"), "{error}");
        let models = provider.list_models(&cancel).unwrap_err();
        assert_eq!(models.kind(), "not_configured", "{base}");
    }
    // An Anthropic provider is held to the same rule.
    let mut config = ProviderConfig::from_preset("a", Preset::Anthropic);
    config.base_url = "http://192.168.1.5".into();
    let turn = run_turn(
        &*connect(config, None),
        hello(),
        &never,
        &AgentOptions::default(),
    );
    assert_eq!(turn.result.unwrap_err().kind(), "not_configured");
}

#[test]
fn loopback_over_http_works_and_ipv6_loopback_is_allowed() {
    // The mock server is on 127.0.0.1, so the other tests cover that; here
    // `localhost` by name and the IPv6 form, each with and without a key.
    let server = MockServer::start(vec![
        Reply::stream(openai(&[Block::Text("one")], "stop")),
        Reply::json(json!({"data": [{"id": "m"}]})),
    ]);
    let port = server.base.rsplit(':').next().unwrap().to_string();
    let provider = connect(custom(&format!("http://localhost:{port}/v1")), None);
    let turn = run_turn(&*provider, hello(), &never, &AgentOptions::default());
    assert_eq!(turn.result, Ok(TurnEnd::Finished), "{:?}", turn.result);
    let cancel = AtomicBool::new(false);
    assert_eq!(provider.list_models(&cancel).unwrap()[0].id, "m");
    assert!(aster_ai::is_loopback_url(&format!(
        "http://[::1]:{port}/v1"
    )));
}

#[test]
fn allow_insecure_lets_plain_http_through() {
    // An address that is not loopback by its name but reaches the mock
    // server: the IPv4-mapped form of 127.0.0.1. Without the setting it is
    // refused; with it, the request goes.
    let server = MockServer::start(vec![Reply::json(json!({"data": [{"id": "m"}]}))]);
    let port = server.base.rsplit(':').next().unwrap().to_string();
    let base = format!("http://[::ffff:127.0.0.1]:{port}/v1");
    assert!(!aster_ai::is_loopback_url(&base));
    let cancel = AtomicBool::new(false);
    let refused = connect(custom(&base), None).list_models(&cancel);
    assert_eq!(refused.unwrap_err().kind(), "not_configured");
    let mut config = custom(&base);
    config.allow_insecure = true;
    let result = connect(config, None).list_models(&cancel);
    // The mapped address may not be reachable on every system; what matters
    // is that it was not refused by the check.
    if let Err(error) = &result {
        assert_ne!(error.kind(), "not_configured", "{error}");
    } else {
        assert_eq!(server.requests().len(), 1);
    }
}

#[test]
fn a_key_is_not_sent_to_another_host_than_it_was_saved_for() {
    let server = MockServer::start(vec![
        Reply::stream(openai(&[Block::Text("saved here")], "stop")),
        Reply::stream(openai(&[Block::Text("old key")], "stop")),
    ]);
    let base = format!("{}/v1", server.base);

    // Saved for another host: refused before anything is sent.
    let mut elsewhere = custom(&base);
    elsewhere.key_host = Some("api.example.com".into());
    assert!(!elsewhere.key_matches_host());
    let provider = connect(elsewhere.clone(), Some("secret".into()));
    let turn = run_turn(&*provider, hello(), &never, &AgentOptions::default());
    let error = turn.result.unwrap_err();
    assert_eq!(error.kind(), "not_configured");
    assert!(error.to_string().contains("save it again"), "{error}");
    let cancel = AtomicBool::new(false);
    assert_eq!(
        provider.list_models(&cancel).unwrap_err().kind(),
        "not_configured"
    );
    assert!(server.requests().is_empty(), "nothing reached the server");

    // Saved for this host: sent.
    let mut here = custom(&base);
    here.key_host = aster_ai::url_host(&base);
    assert_eq!(here.key_host.as_deref(), Some("127.0.0.1"));
    let turn = run_turn(
        &*connect(here, Some("secret".into())),
        hello(),
        &never,
        &AgentOptions::default(),
    );
    assert_eq!(turn.result, Ok(TurnEnd::Finished));

    // Saved before hosts were kept: sent, as before.
    let turn = run_turn(
        &*connect(custom(&base), Some("old".into())),
        hello(),
        &never,
        &AgentOptions::default(),
    );
    assert_eq!(turn.result, Ok(TurnEnd::Finished));
    let requests = server.requests();
    assert_eq!(requests.len(), 2);
    assert_eq!(requests[0].header("authorization"), Some("Bearer secret"));
    assert_eq!(requests[1].header("authorization"), Some("Bearer old"));

    // Without a key there is nothing to protect, so a stale host does not
    // block a keyless local server.
    let server = MockServer::start(vec![Reply::json(json!({"data": []}))]);
    let mut keyless = custom(&format!("{}/v1", server.base));
    keyless.key_host = Some("api.example.com".into());
    assert!(connect(keyless, None).list_models(&cancel).is_ok());
}
