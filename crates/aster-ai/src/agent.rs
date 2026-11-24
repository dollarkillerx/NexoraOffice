//! The agent loop: one user turn, which may take several model requests.
//! The model answers; when it stops to use tools, the calls are run (at the
//! same time when there are several), their results go back in one user
//! message, and the model is asked again, until it finishes, is cut off,
//! declines, runs out of steps, or the user stops it.
//!
//! The conversation is kept valid for the next request whatever happens:
//! every tool call gets a result, run or not. Calls in a reply that did not
//! end in a request for tools (cut off at the output limit, declined) are
//! never run; they get an error result saying so. Calls left without a
//! result by a stopped turn get one at the start of the next turn.
//!
//! Tools run only through the caller's [`ToolExecutor`]. In the application
//! that is an [`InvokerExecutor`], which goes through the policy, the
//! approvals and the audit log of `aster-tools`.

use std::collections::HashSet;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::thread;

use aster_tools::{Invoker, Origin, ToolResult};
use serde_json::Value;

use crate::conversation::{ContentBlock, Message, Role};
use crate::error::AiError;
use crate::prompt::SYSTEM_PROMPT;
use crate::provider::{Provider, ProviderConfig, Request, StopReason, ToolDef};
use crate::redact::redact;

/// How many model requests one user turn may take.
pub const DEFAULT_MAX_STEPS: usize = 25;

/// The result text for calls a stopped turn left without one.
pub const CANCELLED_RESULT: &str = "Not run: cancelled by the user.";

/// The most of an invalid tool input quoted back to the model.
const MAX_QUOTED_INPUT: usize = 2000;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AgentOptions {
    /// Model requests per user turn; at least 1.
    pub max_steps: usize,
    /// Whether to mask likely secrets in the user's message and in tool
    /// results before they go to the provider.
    pub redact: bool,
}

impl Default for AgentOptions {
    fn default() -> Self {
        Self {
            max_steps: DEFAULT_MAX_STEPS,
            redact: true,
        }
    }
}

/// Runs the model's tool calls. Called from several threads at once when a
/// reply has several calls, and it may block (an approval can take
/// minutes).
pub trait ToolExecutor: Sync {
    fn execute(&self, name: &str, input: &Value) -> ToolResult;
}

impl<F> ToolExecutor for F
where
    F: Fn(&str, &Value) -> ToolResult + Sync,
{
    fn execute(&self, name: &str, input: &Value) -> ToolResult {
        self(name, input)
    }
}

/// The executor the application uses: every call goes through the
/// [`Invoker`], as `ai:<provider>/<model>` in the conversation's session.
pub struct InvokerExecutor<'a> {
    invoker: &'a Invoker,
    origin: Origin,
}

impl<'a> InvokerExecutor<'a> {
    pub fn new(invoker: &'a Invoker, provider: &ProviderConfig, conversation_id: &str) -> Self {
        Self {
            invoker,
            origin: Origin::new(provider.caller(), conversation_id),
        }
    }
}

impl ToolExecutor for InvokerExecutor<'_> {
    fn execute(&self, name: &str, input: &Value) -> ToolResult {
        self.invoker.call(&self.origin, name, input)
    }
}

/// What a turn reports while it runs, in order. Every turn ends with
/// exactly one [`AgentEvent::Done`] or [`AgentEvent::Error`].
#[derive(Clone, Debug, PartialEq)]
pub enum AgentEvent {
    /// Secrets were masked in the user's message before it was sent.
    Redacted {
        count: usize,
    },
    /// More of the assistant's text.
    TextDelta(String),
    /// A tool call is about to run (or to be refused, for invalid input).
    ToolCallStarted {
        id: String,
        name: String,
        input: Value,
    },
    /// A tool call ended. `result` is what the model gets, after masking;
    /// `redacted` counts what was masked.
    ToolCallFinished {
        id: String,
        name: String,
        result: ToolResult,
        redacted: usize,
    },
    /// One model request is complete (step 1, 2, …).
    StepFinished {
        step: usize,
        stop: StopReason,
    },
    Done {
        end: TurnEnd,
    },
    /// The turn failed, or was stopped ([`AiError::Cancelled`]).
    Error(AiError),
}

/// How a turn that did not fail ended.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TurnEnd {
    /// The model finished its answer.
    Finished,
    /// The answer was cut off at the output limit.
    MaxTokens,
    /// The model or the provider's safety filter declined.
    Refusal,
    /// The turn used all its steps while the model still wanted tools.
    StepLimit,
    /// Any other stop reason, by the provider's name for it.
    Other(String),
}

/// Run one user turn. The caller has already pushed the user's message
/// (see [`crate::user_message`]); the assistant's messages and the tool
/// results are appended to `conversation` as the turn goes.
///
/// Blocks until the turn ends; run it on a worker thread. `sink` gets the
/// [`AgentEvent`]s on this thread. Setting `cancel` stops the turn as soon
/// as the stream or the running tool calls allow: text streamed by an
/// unfinished reply is not kept in the conversation.
pub fn run(
    provider: &dyn Provider,
    conversation: &mut Vec<Message>,
    tools: &[ToolDef],
    executor: &dyn ToolExecutor,
    options: &AgentOptions,
    sink: &mut dyn FnMut(AgentEvent),
    cancel: &AtomicBool,
) -> Result<TurnEnd, AiError> {
    let result = turn(
        provider,
        conversation,
        tools,
        executor,
        options,
        sink,
        cancel,
    );
    match &result {
        Ok(end) => sink(AgentEvent::Done { end: end.clone() }),
        Err(error) => sink(AgentEvent::Error(error.clone())),
    }
    result
}

fn turn(
    provider: &dyn Provider,
    conversation: &mut Vec<Message>,
    tools: &[ToolDef],
    executor: &dyn ToolExecutor,
    options: &AgentOptions,
    sink: &mut dyn FnMut(AgentEvent),
    cancel: &AtomicBool,
) -> Result<TurnEnd, AiError> {
    answer_dangling_calls(conversation);
    if conversation
        .last()
        .is_none_or(|last| last.role != Role::User)
    {
        return Err(AiError::NotConfigured(
            "the conversation must end with the user's message".into(),
        ));
    }
    if options.redact {
        let count = redact_message(conversation.last_mut().expect("checked above"));
        if count > 0 {
            sink(AgentEvent::Redacted { count });
        }
    }
    let max_steps = options.max_steps.max(1);
    for step in 1..=max_steps {
        if cancel.load(Ordering::Relaxed) {
            return Err(AiError::Cancelled);
        }
        let request = Request {
            system: SYSTEM_PROMPT,
            tools,
            messages: conversation,
        };
        let response = provider.stream(
            &request,
            &mut |text| sink(AgentEvent::TextDelta(text.to_string())),
            cancel,
        )?;
        let calls: Vec<Call> = response
            .content
            .iter()
            .filter_map(|block| match block {
                ContentBlock::ToolUse {
                    id,
                    name,
                    input,
                    raw_input,
                } => Some(Call {
                    id: id.clone(),
                    name: name.clone(),
                    input: input.clone(),
                    raw_input: raw_input.clone(),
                }),
                _ => None,
            })
            .collect();
        if !response.content.is_empty() {
            conversation.push(Message::assistant(response.content));
        }
        sink(AgentEvent::StepFinished {
            step,
            stop: response.stop.clone(),
        });
        if response.stop != StopReason::ToolUse || calls.is_empty() {
            if !calls.is_empty() {
                let why = match &response.stop {
                    StopReason::MaxTokens => "Not run: the reply was cut off at the output limit before the call was complete.".to_string(),
                    other => format!("Not run: the reply ended ({}) instead of asking for the call.", stop_name(other)),
                };
                let results = calls
                    .iter()
                    .map(|call| ContentBlock::ToolResult {
                        tool_use_id: call.id.clone(),
                        text: why.clone(),
                        is_error: true,
                    })
                    .collect();
                conversation.push(Message::user(results));
            }
            return Ok(match response.stop {
                StopReason::EndTurn | StopReason::ToolUse => TurnEnd::Finished,
                StopReason::MaxTokens => TurnEnd::MaxTokens,
                StopReason::Refusal => TurnEnd::Refusal,
                StopReason::Other(reason) => TurnEnd::Other(reason),
            });
        }
        let results = execute(&calls, executor, options.redact, sink);
        conversation.push(Message::user(results));
        if cancel.load(Ordering::Relaxed) {
            return Err(AiError::Cancelled);
        }
    }
    Ok(TurnEnd::StepLimit)
}

fn stop_name(stop: &StopReason) -> &str {
    match stop {
        StopReason::EndTurn => "end of turn",
        StopReason::ToolUse => "tool use",
        StopReason::MaxTokens => "output limit",
        StopReason::Refusal => "declined",
        StopReason::Other(reason) => reason,
    }
}

struct Call {
    id: String,
    name: String,
    input: Value,
    raw_input: Option<String>,
}

/// Run the calls, at the same time, and return their results in the calls'
/// order. Events are sent from this thread as each call finishes.
fn execute(
    calls: &[Call],
    executor: &dyn ToolExecutor,
    redaction: bool,
    sink: &mut dyn FnMut(AgentEvent),
) -> Vec<ContentBlock> {
    let mut results: Vec<Option<ToolResult>> = vec![None; calls.len()];
    let mut finished = |index: usize, mut result: ToolResult, sink: &mut dyn FnMut(AgentEvent)| {
        let mut redacted = 0;
        if redaction {
            let masked = redact(&result.text);
            result.text = masked.text;
            redacted = masked.count;
        }
        sink(AgentEvent::ToolCallFinished {
            id: calls[index].id.clone(),
            name: calls[index].name.clone(),
            result: result.clone(),
            redacted,
        });
        results[index] = Some(result);
    };
    for call in calls {
        sink(AgentEvent::ToolCallStarted {
            id: call.id.clone(),
            name: call.name.clone(),
            input: call.input.clone(),
        });
    }
    let mut valid = Vec::new();
    for (index, call) in calls.iter().enumerate() {
        match &call.raw_input {
            Some(raw) => finished(index, invalid_input(raw), sink),
            None => valid.push(index),
        }
    }
    thread::scope(|scope| {
        let (tx, rx) = mpsc::channel();
        for &index in &valid {
            let tx = tx.clone();
            let call = &calls[index];
            scope.spawn(move || {
                let result = catch_unwind(AssertUnwindSafe(|| {
                    executor.execute(&call.name, &call.input)
                }))
                .unwrap_or_else(|_| ToolResult {
                    text: "The tool failed unexpectedly.".into(),
                    is_error: true,
                });
                let _ = tx.send((index, result));
            });
        }
        drop(tx);
        for (index, result) in rx {
            finished(index, result, sink);
        }
    });
    calls
        .iter()
        .zip(results)
        .map(|(call, result)| {
            let result = result.unwrap_or_else(|| ToolResult {
                text: "The tool gave no result.".into(),
                is_error: true,
            });
            ContentBlock::ToolResult {
                tool_use_id: call.id.clone(),
                text: result.text,
                is_error: result.is_error,
            }
        })
        .collect()
}

/// The result for a call whose input was not a JSON object: the model is
/// told, with what it sent, so it can try again.
fn invalid_input(raw: &str) -> ToolResult {
    let mut quoted: String = raw.chars().take(MAX_QUOTED_INPUT).collect();
    if raw.chars().count() > MAX_QUOTED_INPUT {
        quoted.push('…');
    }
    ToolResult {
        text: format!(
            "INVALID_JSON: the tool input was not a valid JSON object, so the tool was not run. Send the call again with a JSON object as its input. Input received: {quoted}"
        ),
        is_error: true,
    }
}

/// Give every tool call that has no result one saying it was cancelled, so
/// the history is valid again after a stopped turn. The results go first
/// into the user message that follows the call, or into a new one.
fn answer_dangling_calls(conversation: &mut Vec<Message>) {
    let mut index = 0;
    while index < conversation.len() {
        if conversation[index].role == Role::Assistant {
            let answered: HashSet<String> = conversation
                .get(index + 1)
                .filter(|next| next.role == Role::User)
                .map(|next| {
                    next.content
                        .iter()
                        .filter_map(|block| match block {
                            ContentBlock::ToolResult { tool_use_id, .. } => {
                                Some(tool_use_id.clone())
                            }
                            _ => None,
                        })
                        .collect()
                })
                .unwrap_or_default();
            let missing: Vec<ContentBlock> = conversation[index]
                .tool_uses()
                .filter_map(|block| match block {
                    ContentBlock::ToolUse { id, .. } if !answered.contains(id) => {
                        Some(ContentBlock::ToolResult {
                            tool_use_id: id.clone(),
                            text: CANCELLED_RESULT.into(),
                            is_error: true,
                        })
                    }
                    _ => None,
                })
                .collect();
            if !missing.is_empty() {
                match conversation.get_mut(index + 1) {
                    Some(next) if next.role == Role::User => {
                        next.content.splice(0..0, missing);
                    }
                    _ => conversation.insert(index + 1, Message::user(missing)),
                }
            }
        }
        index += 1;
    }
}

/// Mask secrets in a message's text blocks; returns how many.
fn redact_message(message: &mut Message) -> usize {
    let mut count = 0;
    for block in &mut message.content {
        if let ContentBlock::Text { text } = block {
            let masked = redact(text);
            if masked.count > 0 {
                *text = masked.text;
                count += masked.count;
            }
        }
    }
    count
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn tool_use(id: &str) -> ContentBlock {
        ContentBlock::ToolUse {
            id: id.into(),
            name: "list_documents".into(),
            input: json!({}),
            raw_input: None,
        }
    }

    #[test]
    fn dangling_calls_get_cancelled_results_before_the_new_message() {
        let mut conversation = vec![
            Message::user(vec![ContentBlock::text("Go")]),
            Message::assistant(vec![tool_use("a"), tool_use("b")]),
            Message::user(vec![ContentBlock::text("Never mind")]),
        ];
        answer_dangling_calls(&mut conversation);
        assert_eq!(conversation.len(), 3);
        let content = &conversation[2].content;
        assert_eq!(content.len(), 3);
        assert!(
            matches!(&content[0], ContentBlock::ToolResult { tool_use_id, text, is_error: true } if tool_use_id == "a" && text == CANCELLED_RESULT)
        );
        assert!(
            matches!(&content[1], ContentBlock::ToolResult { tool_use_id, .. } if tool_use_id == "b")
        );
        assert_eq!(content[2], ContentBlock::text("Never mind"));

        // An answered call is left alone, and a trailing call gets a new
        // message.
        let mut conversation = vec![
            Message::assistant(vec![tool_use("a")]),
            Message::user(vec![ContentBlock::ToolResult {
                tool_use_id: "a".into(),
                text: "ok".into(),
                is_error: false,
            }]),
            Message::assistant(vec![tool_use("b")]),
        ];
        answer_dangling_calls(&mut conversation);
        assert_eq!(conversation.len(), 4);
        assert_eq!(conversation[1].content.len(), 1);
        assert_eq!(conversation[3].role, Role::User);
    }

    #[test]
    fn invalid_input_is_quoted_back() {
        let result = invalid_input("{\"document\": ");
        assert!(result.is_error);
        assert!(result.text.starts_with("INVALID_JSON: "));
        assert!(result.text.ends_with("{\"document\": "));
    }
}
