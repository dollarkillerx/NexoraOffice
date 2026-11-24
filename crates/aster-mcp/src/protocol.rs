//! The MCP side of one connection, without any input or output: a line of
//! JSON-RPC goes in, and what to do about it comes out. Tool calls are not
//! made here; they are handed back to the connection, which runs each one
//! on a thread of its own so that a call waiting for the user does not hold
//! up the next message.
//!
//! Choices made here:
//! - Batches (a JSON array) are answered with one "invalid request" error:
//!   MCP 2025-06-18 dropped them, and no client we serve sends them.
//! - Everything except `initialize` and `ping` is refused with -32002 until
//!   `initialize` has been answered. The `notifications/initialized` that
//!   follows is accepted but not waited for.
//! - Messages without a method (responses) are ignored: the server never
//!   sends requests of its own.

use std::sync::OnceLock;

use serde_json::{Map, Value, json};

/// The protocol versions we speak, newest first. What we use of MCP (tools
/// only, no server-to-client requests) reads the same in all of them.
pub const PROTOCOL_VERSIONS: &[&str] = &["2025-11-25", "2025-06-18", "2025-03-26", "2024-11-05"];

/// The client name used when `initialize` names none we can show.
pub const DEFAULT_CLIENT: &str = "local";
/// Longest client name kept, in characters.
pub const MAX_CLIENT_NAME: usize = 64;

pub const PARSE_ERROR: i64 = -32700;
pub const INVALID_REQUEST: i64 = -32600;
pub const METHOD_NOT_FOUND: i64 = -32601;
pub const INVALID_PARAMS: i64 = -32602;
/// A request other than `initialize` or `ping` before `initialize`.
pub const NOT_INITIALIZED: i64 = -32002;
/// The connection already has as many calls running as it may.
pub const TOO_BUSY: i64 = -32000;

const INSTRUCTIONS: &str = "Aster Office is an office suite. These tools read and change the documents open in it. Call list_documents first: it lists the documents you may see, with their ids and your access to each. The user decides what you may see, and is asked before changes or saves; a refusal comes back as a tool error.";

/// What the connection should do with one message.
#[derive(Debug, PartialEq)]
pub enum Outcome {
    /// Nothing: a notification, or a response.
    Nothing,
    /// Send this message.
    Reply(Value),
    /// `initialize` succeeded: send the reply, and the client now has a name.
    Initialized {
        reply: Value,
        client: String,
        protocol_version: String,
    },
    /// Run a tool and answer request `id` with the result.
    Call {
        id: Value,
        client: String,
        name: String,
        arguments: Value,
    },
    /// The client gave up on request `id`.
    Cancel(Value),
}

/// The protocol state of one connection.
#[derive(Debug, Default)]
pub struct Session {
    client: Option<String>,
}

impl Session {
    pub fn new() -> Self {
        Self::default()
    }

    /// The client's cleaned name, once `initialize` has been answered.
    pub fn client(&self) -> Option<&str> {
        self.client.as_deref()
    }

    /// Handle one line (without its line break).
    pub fn handle(&mut self, line: &[u8]) -> Outcome {
        let message: Value = match serde_json::from_slice(line) {
            Ok(message) => message,
            Err(error) => {
                return Outcome::Reply(error_reply(
                    Value::Null,
                    PARSE_ERROR,
                    &format!("parse error: {error}"),
                ));
            }
        };
        if message.is_array() {
            return Outcome::Reply(error_reply(
                Value::Null,
                INVALID_REQUEST,
                "batches are not supported; send one message per line",
            ));
        }
        let Some(object) = message.as_object() else {
            return invalid(Value::Null, "a message must be a JSON object");
        };
        // A request's id must be a string or a number; anything else cannot
        // be answered by that id.
        let id = match object.get("id") {
            None => None,
            Some(id @ (Value::String(_) | Value::Number(_))) => Some(id.clone()),
            Some(_) => return invalid(Value::Null, "`id` must be a string or a number"),
        };
        if object.get("jsonrpc").and_then(Value::as_str) != Some("2.0") {
            return invalid(id.unwrap_or(Value::Null), "`jsonrpc` must be \"2.0\"");
        }
        let Some(method) = object.get("method") else {
            if object.contains_key("result") || object.contains_key("error") {
                return Outcome::Nothing;
            }
            return invalid(id.unwrap_or(Value::Null), "`method` is missing");
        };
        let Some(method) = method.as_str() else {
            return invalid(id.unwrap_or(Value::Null), "`method` must be a string");
        };
        let params = object.get("params").cloned().unwrap_or(Value::Null);
        if !(params.is_null() || params.is_object()) {
            return match id {
                Some(id) => Outcome::Reply(error_reply(
                    id,
                    INVALID_PARAMS,
                    "`params` must be an object",
                )),
                None => Outcome::Nothing,
            };
        }
        match id {
            None => self.notification(method, &params),
            Some(id) => self.request(id, method, params),
        }
    }

    fn notification(&mut self, method: &str, params: &Value) -> Outcome {
        match method {
            "notifications/cancelled" => match params.get("requestId") {
                Some(id @ (Value::String(_) | Value::Number(_))) => Outcome::Cancel(id.clone()),
                _ => Outcome::Nothing,
            },
            // `notifications/initialized` and anything we do not know: a
            // notification is never answered, not even with an error.
            _ => Outcome::Nothing,
        }
    }

    fn request(&mut self, id: Value, method: &str, params: Value) -> Outcome {
        match method {
            "ping" => return Outcome::Reply(reply(id, json!({}))),
            "initialize" => return self.initialize(id, &params),
            _ => {}
        }
        let Some(client) = self.client.clone() else {
            return Outcome::Reply(error_reply(
                id,
                NOT_INITIALIZED,
                "the session is not initialized; send `initialize` first",
            ));
        };
        match method {
            "tools/list" => Outcome::Reply(reply(id, tool_list().clone())),
            "tools/call" => {
                let Some(name) = params.get("name").and_then(Value::as_str) else {
                    return Outcome::Reply(error_reply(
                        id,
                        INVALID_PARAMS,
                        "`params.name` must be the name of a tool",
                    ));
                };
                // A missing `arguments` is an empty object; anything that is
                // not an object goes on to the tool layer, which answers
                // with a tool error the model can read.
                let arguments = match params.get("arguments") {
                    None | Some(Value::Null) => Value::Object(Map::new()),
                    Some(arguments) => arguments.clone(),
                };
                Outcome::Call {
                    id,
                    client,
                    name: name.to_owned(),
                    arguments,
                }
            }
            _ => Outcome::Reply(error_reply(
                id,
                METHOD_NOT_FOUND,
                &format!("method `{method}` is not supported"),
            )),
        }
    }

    fn initialize(&mut self, id: Value, params: &Value) -> Outcome {
        if self.client.is_some() {
            return invalid(id, "the session is already initialized");
        }
        if !params.is_object() {
            return Outcome::Reply(error_reply(
                id,
                INVALID_PARAMS,
                "`initialize` needs params with a protocolVersion",
            ));
        }
        let asked = params.get("protocolVersion").and_then(Value::as_str);
        let version = PROTOCOL_VERSIONS
            .iter()
            .find(|v| Some(**v) == asked)
            .unwrap_or(&PROTOCOL_VERSIONS[0]);
        let client = clean_client_name(
            params
                .get("clientInfo")
                .and_then(|info| info.get("name"))
                .and_then(Value::as_str),
        );
        self.client = Some(client.clone());
        let result = json!({
            "protocolVersion": version,
            "capabilities": { "tools": { "listChanged": false } },
            "serverInfo": {
                "name": "aster-office",
                "title": "Aster Office",
                "version": env!("CARGO_PKG_VERSION"),
            },
            "instructions": INSTRUCTIONS,
        });
        Outcome::Initialized {
            reply: reply(id, result),
            client,
            protocol_version: (*version).to_owned(),
        }
    }
}

/// The client's name as the interface and the audit log show it: letters,
/// digits, spaces and `. _ - / @ + :`, at most 64 characters. Anything else
/// is dropped, so a name cannot carry control characters, bidirectional
/// overrides or markup into the interface.
pub fn clean_client_name(name: Option<&str>) -> String {
    let cleaned: String = name
        .unwrap_or_default()
        .chars()
        .filter(|c| c.is_alphanumeric() || " ._-/@+:".contains(*c))
        .take(MAX_CLIENT_NAME)
        .collect();
    let cleaned = cleaned.trim();
    if cleaned.is_empty() {
        DEFAULT_CLIENT.to_owned()
    } else {
        cleaned.to_owned()
    }
}

/// The `tools/list` result, built once: the catalogue never changes while
/// the program runs, so every client sees the same bytes.
pub fn tool_list() -> &'static Value {
    static LIST: OnceLock<Value> = OnceLock::new();
    LIST.get_or_init(|| {
        let tools: Vec<Value> = aster_tools::tools()
            .iter()
            .map(|tool| {
                json!({
                    "name": tool.name,
                    "title": tool.title,
                    "description": tool.description,
                    "inputSchema": tool.schema(),
                    "annotations": {
                        "title": tool.title,
                        "readOnlyHint": tool.annotations.read_only,
                        "destructiveHint": tool.annotations.destructive,
                        "idempotentHint": tool.annotations.idempotent,
                        "openWorldHint": tool.annotations.open_world,
                    },
                })
            })
            .collect();
        json!({ "tools": tools })
    })
}

/// The answer to a `tools/call`.
pub fn call_result(result: &aster_tools::ToolResult) -> Value {
    json!({
        "content": [{ "type": "text", "text": result.text }],
        "isError": result.is_error,
    })
}

pub fn reply(id: Value, result: Value) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "result": result })
}

pub fn error_reply(id: Value, code: i64, message: &str) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "error": { "code": code, "message": message } })
}

fn invalid(id: Value, message: &str) -> Outcome {
    Outcome::Reply(error_reply(id, INVALID_REQUEST, message))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn handle(session: &mut Session, message: Value) -> Outcome {
        session.handle(message.to_string().as_bytes())
    }

    fn initialized() -> Session {
        let mut session = Session::new();
        handle(
            &mut session,
            json!({"jsonrpc": "2.0", "id": 0, "method": "initialize",
                   "params": {"protocolVersion": "2025-06-18", "clientInfo": {"name": "t"}}}),
        );
        session
    }

    fn error_code(outcome: Outcome) -> i64 {
        match outcome {
            Outcome::Reply(value) => value["error"]["code"].as_i64().unwrap(),
            other => panic!("not an error reply: {other:?}"),
        }
    }

    #[test]
    fn versions_are_negotiated() {
        for (asked, answered) in [
            ("2025-03-26", "2025-03-26"),
            ("2024-11-05", "2024-11-05"),
            ("1999-01-01", PROTOCOL_VERSIONS[0]),
        ] {
            let mut session = Session::new();
            let outcome = handle(
                &mut session,
                json!({"jsonrpc": "2.0", "id": "a", "method": "initialize",
                       "params": {"protocolVersion": asked}}),
            );
            let Outcome::Initialized {
                reply,
                client,
                protocol_version,
            } = outcome
            else {
                panic!("not initialized: {outcome:?}");
            };
            assert_eq!(reply["id"], "a");
            assert_eq!(reply["result"]["protocolVersion"], answered);
            assert_eq!(protocol_version, answered);
            assert_eq!(client, "local");
            assert_eq!(session.client(), Some("local"));
        }
    }

    #[test]
    fn malformed_messages_get_the_right_errors() {
        let mut session = initialized();
        assert_eq!(error_code(session.handle(b"{nope")), PARSE_ERROR);
        assert_eq!(error_code(session.handle(b"[]")), INVALID_REQUEST);
        assert_eq!(error_code(session.handle(b"3")), INVALID_REQUEST);
        assert_eq!(
            error_code(handle(&mut session, json!({"id": 1, "method": "ping"}))),
            INVALID_REQUEST
        );
        assert_eq!(
            error_code(handle(
                &mut session,
                json!({"jsonrpc": "2.0", "id": [1], "method": "ping"})
            )),
            INVALID_REQUEST
        );
        assert_eq!(
            error_code(handle(
                &mut session,
                json!({"jsonrpc": "2.0", "id": 1, "method": "tools/call", "params": []})
            )),
            INVALID_PARAMS
        );
        assert_eq!(
            error_code(handle(
                &mut session,
                json!({"jsonrpc": "2.0", "id": 1, "method": "tools/call", "params": {}})
            )),
            INVALID_PARAMS
        );
        assert_eq!(
            error_code(handle(
                &mut session,
                json!({"jsonrpc": "2.0", "id": 1, "method": "resources/list"})
            )),
            METHOD_NOT_FOUND
        );
        // Notifications and responses are never answered.
        assert_eq!(
            handle(&mut session, json!({"jsonrpc": "2.0", "method": "nope"})),
            Outcome::Nothing
        );
        assert_eq!(
            handle(
                &mut session,
                json!({"jsonrpc": "2.0", "id": 5, "result": {}})
            ),
            Outcome::Nothing
        );
        // Initializing twice is refused.
        assert_eq!(
            error_code(handle(
                &mut session,
                json!({"jsonrpc": "2.0", "id": 2, "method": "initialize", "params": {}})
            )),
            INVALID_REQUEST
        );
    }

    #[test]
    fn calls_and_cancellations_are_handed_back() {
        let mut session = initialized();
        assert_eq!(
            handle(
                &mut session,
                json!({"jsonrpc": "2.0", "id": 9, "method": "tools/call",
                       "params": {"name": "list_documents"}})
            ),
            Outcome::Call {
                id: json!(9),
                client: "t".into(),
                name: "list_documents".into(),
                arguments: json!({}),
            }
        );
        assert_eq!(
            handle(
                &mut session,
                json!({"jsonrpc": "2.0", "method": "notifications/cancelled",
                       "params": {"requestId": 9, "reason": "user"}})
            ),
            Outcome::Cancel(json!(9))
        );
    }

    #[test]
    fn client_names_are_cleaned() {
        assert_eq!(clean_client_name(Some("claude-code")), "claude-code");
        assert_eq!(
            clean_client_name(Some("Evil\u{202e}<b>name</b>\n")),
            "Evilbname/b"
        );
        assert_eq!(clean_client_name(Some("クロード 1.0")), "クロード 1.0");
        assert_eq!(clean_client_name(Some("  \u{0}  ")), "local");
        assert_eq!(clean_client_name(None), "local");
        assert_eq!(clean_client_name(Some(&"x".repeat(100))).len(), 64);
    }

    #[test]
    fn the_tool_list_follows_the_catalogue() {
        let list = tool_list()["tools"].as_array().unwrap();
        assert_eq!(list.len(), aster_tools::tools().len());
        for (entry, tool) in list.iter().zip(aster_tools::tools()) {
            assert_eq!(entry["name"], tool.name);
            assert_eq!(entry["inputSchema"], tool.schema());
            assert_eq!(
                entry["annotations"]["destructiveHint"],
                tool.annotations.destructive
            );
        }
    }
}
