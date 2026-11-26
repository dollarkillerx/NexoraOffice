//! The MCP side of one connection, without any input or output: a line of
//! JSON-RPC goes in, and what to do about it comes out. Nothing that goes
//! through the invoker is done here (tool calls, listing and reading
//! resources); it is handed back to the connection as a [`Job`], which runs
//! each one on a thread of its own so that a call waiting for the user does
//! not hold up the next message. The functions that turn a job's tool
//! results into replies are here, so that they can be tested without a
//! socket.
//!
//! Resources and prompts:
//! - Resources are read through the same invoker as tools, so policy and
//!   audit apply: `resources/list` and `aster://documents` are a
//!   `list_documents` call, `aster://document/{id}` is a `read_document`
//!   call. A document the client may not see is not listed, and reading it
//!   gets "resource not found", the same as a document that is not open.
//! - Reading a document gives what one `read_document` call gives: the
//!   text with 1-based line numbers (which `replace_lines` needs), up to
//!   [`RESOURCE_LINES`] lines and the invoker's size limit, with the tool's
//!   own note on how many lines follow. One call, not one per part, so the
//!   parts cannot come from different versions of a document being edited;
//!   the rest is read with the `read_document` tool. It is labelled
//!   `text/plain`, since the line numbers are not Markdown; the listing
//!   gives a Markdown document's own type, `text/markdown`.
//! - Prompts are fixed English templates that tell the model which tools
//!   to use. They never contain document text, so they cannot get round
//!   the policy.
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

/// The protocol versions we speak, newest first. What we use of MCP (tools,
/// resources and prompts, no subscriptions, no server-to-client requests)
/// reads the same in all of them.
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
/// The code MCP gives a resource that does not exist. It is the same number
/// as [`NOT_INITIALIZED`], which only ever comes before `initialize`.
pub const RESOURCE_NOT_FOUND: i64 = -32002;
pub const INTERNAL_ERROR: i64 = -32603;

/// The document list as a resource.
pub const DOCUMENTS_URI: &str = "aster://documents";
/// Documents are `aster://document/{id}`, the id percent-encoded.
pub const DOCUMENT_URI_PREFIX: &str = "aster://document/";
pub const SHEET_URI_PREFIX: &str = "aster://sheet/";
/// How many lines reading a document resource asks `read_document` for.
pub const RESOURCE_LINES: u64 = 2000;
/// The longest `document_id` a prompt takes, in characters.
pub const MAX_PROMPT_ARGUMENT: usize = 256;

const INSTRUCTIONS: &str = "Aster Office is an office suite. These tools read and change the documents open in it. Call list_documents first: it lists the documents you may see, with their ids and your access to each. The user decides what you may see, and is asked before changes or saves; a refusal comes back as a tool error. The same documents are resources (aster://document/{id}), read with line numbers.";

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
    /// Do `job` through the invoker and answer request `id`.
    Call { id: Value, client: String, job: Job },
    /// The client gave up on request `id`.
    Cancel(Value),
}

/// Work that goes through the invoker, so it runs on a thread of its own.
#[derive(Debug, PartialEq)]
pub enum Job {
    /// `tools/call`.
    Tool { name: String, arguments: Value },
    /// `resources/list`.
    ListResources,
    /// `resources/read`.
    ReadResource(Resource),
}

/// A resource the server has.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Resource {
    /// `aster://documents`.
    Documents,
    /// `aster://document/{id}`, with the id decoded.
    Document(String),
    /// `aster://sheet/{id}`: a spreadsheet document, read as its cells.
    Sheet(String),
}

impl Resource {
    /// The resource a URI names, if it is one of ours.
    pub fn parse(uri: &str) -> Option<Resource> {
        if uri == DOCUMENTS_URI {
            return Some(Resource::Documents);
        }
        if let Some(rest) = uri.strip_prefix(SHEET_URI_PREFIX) {
            let id = percent_decode(rest)?;
            return (!id.is_empty()).then_some(Resource::Sheet(id));
        }
        let id = percent_decode(uri.strip_prefix(DOCUMENT_URI_PREFIX)?)?;
        (!id.is_empty()).then_some(Resource::Document(id))
    }

    pub fn uri(&self) -> String {
        match self {
            Resource::Documents => DOCUMENTS_URI.to_owned(),
            Resource::Document(id) => format!("{DOCUMENT_URI_PREFIX}{}", percent_encode(id)),
            Resource::Sheet(id) => format!("{SHEET_URI_PREFIX}{}", percent_encode(id)),
        }
    }
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
            "resources/list" => Outcome::Call {
                id,
                client,
                job: Job::ListResources,
            },
            "resources/templates/list" => Outcome::Reply(reply(id, resource_templates())),
            "resources/read" => {
                let Some(uri) = params.get("uri").and_then(Value::as_str) else {
                    return Outcome::Reply(error_reply(
                        id,
                        INVALID_PARAMS,
                        "`params.uri` must be the URI of a resource",
                    ));
                };
                // A URI that is not ours is not found without asking the
                // invoker: there is nothing to look up or record.
                match Resource::parse(uri) {
                    Some(resource) => Outcome::Call {
                        id,
                        client,
                        job: Job::ReadResource(resource),
                    },
                    None => Outcome::Reply(not_found(id, uri)),
                }
            }
            "prompts/list" => Outcome::Reply(reply(id, prompt_list())),
            "prompts/get" => Outcome::Reply(match get_prompt(&params) {
                Ok(result) => reply(id, result),
                Err(message) => error_reply(id, INVALID_PARAMS, &message),
            }),
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
                    job: Job::Tool {
                        name: name.to_owned(),
                        arguments,
                    },
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
            "capabilities": {
                "tools": { "listChanged": false },
                "resources": { "subscribe": false, "listChanged": false },
                "prompts": { "listChanged": false },
            },
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

/// The arguments of the `list_documents` call behind `resources/list` and
/// `aster://documents`.
pub fn list_arguments() -> Value {
    json!({})
}

/// The arguments of the `read_sheet` call behind a spreadsheet resource:
/// the used part of the sheet in front.
pub fn sheet_arguments(id: &str) -> Value {
    json!({ "document": id })
}

/// The arguments of the `read_document` call behind a document resource.
pub fn read_arguments(id: &str) -> Value {
    json!({ "document": id, "max_lines": RESOURCE_LINES })
}

/// The `resources/list` result from what `list_documents` gave.
pub fn resource_list(listed: &aster_tools::ToolResult) -> Result<Value, (i64, String)> {
    let documents = documents(listed)?;
    let mut resources = vec![json!({
        "uri": DOCUMENTS_URI,
        "name": "documents",
        "title": "Open documents",
        "description": "The open documents you may see, with their ids and your access to each, as list_documents gives them.",
        "mimeType": "application/json",
    })];
    for document in documents {
        let (Some(id), Some(title)) = (document["id"].as_str(), document["title"].as_str()) else {
            continue;
        };
        let kind = document["kind"].as_str().unwrap_or("document");
        let access = document["access"].as_str().unwrap_or_default();
        if kind == "spreadsheet" {
            resources.push(json!({
                "uri": Resource::Sheet(id.to_owned()).uri(),
                "name": title,
                "title": title,
                "description": format!("A spreadsheet; your access: {access}. Reading it gives the used part of the sheet in front as JSON, as read_sheet does."),
                "mimeType": "application/json",
            }));
            continue;
        }
        resources.push(json!({
            "uri": Resource::Document(id.to_owned()).uri(),
            "name": title,
            "title": title,
            "description": format!("A {kind} document; your access: {access}. Reading it gives its text with line numbers."),
            "mimeType": mime_type(kind),
        }));
    }
    Ok(json!({ "resources": resources }))
}

/// The `resources/read` result for `resource` from its tool call's result.
pub fn read_result(
    resource: &Resource,
    result: &aster_tools::ToolResult,
) -> Result<Value, (i64, String)> {
    let mime = match resource {
        Resource::Documents => {
            documents(result)?;
            "application/json"
        }
        Resource::Document(id) | Resource::Sheet(id) => {
            if result.is_error {
                // Not open, or not to be seen: the same answer, so a hidden
                // document does not exist for the client.
                if result.text == aster_tools::invoke::unknown_document(id) {
                    return Err((RESOURCE_NOT_FOUND, result.text.clone()));
                }
                return Err((INTERNAL_ERROR, result.text.clone()));
            }
            if matches!(resource, Resource::Sheet(_)) {
                "application/json"
            } else {
                "text/plain"
            }
        }
    };
    Ok(json!({
        "contents": [{ "uri": resource.uri(), "mimeType": mime, "text": result.text }],
    }))
}

/// The documents in a `list_documents` result.
fn documents(listed: &aster_tools::ToolResult) -> Result<Vec<Value>, (i64, String)> {
    if listed.is_error {
        return Err((INTERNAL_ERROR, listed.text.clone()));
    }
    let parsed: Value = serde_json::from_str(&listed.text)
        .map_err(|_| (INTERNAL_ERROR, "the document list is unreadable".to_owned()))?;
    match parsed.get("documents") {
        Some(Value::Array(documents)) => Ok(documents.clone()),
        _ => Err((INTERNAL_ERROR, "the document list is unreadable".to_owned())),
    }
}

fn mime_type(kind: &str) -> &'static str {
    match kind {
        "markdown" => "text/markdown",
        _ => "text/plain",
    }
}

/// The `resources/templates/list` result.
pub fn resource_templates() -> Value {
    json!({
        "resourceTemplates": [{
            "uriTemplate": "aster://document/{id}",
            "name": "document",
            "title": "Open document",
            "description": "An open document by its id from list_documents: its text with 1-based line numbers.",
            "mimeType": "text/plain",
        }, {
            "uriTemplate": "aster://sheet/{id}",
            "name": "sheet",
            "title": "Open spreadsheet",
            "description": "An open spreadsheet by its id from list_documents: the used part of the sheet in front, as JSON.",
            "mimeType": "application/json",
        }],
    })
}

pub fn not_found(id: Value, uri: &str) -> Value {
    let shown: String = uri.chars().take(200).collect();
    error_reply(
        id,
        RESOURCE_NOT_FOUND,
        &format!("no resource `{shown}` that you may see"),
    )
}

/// The prompts: name, title, description, whether `document_id` is needed.
const PROMPTS: &[(&str, &str, &str, bool)] = &[
    (
        "summarize_document",
        "Summarize a document",
        "Read an open document and sum up its main points, without changing it.",
        true,
    ),
    (
        "proofread_document",
        "Proofread a document",
        "Fix spelling, punctuation and grammar in an open document with minimal edits, each approved by the user.",
        false,
    ),
];

/// The `prompts/list` result.
pub fn prompt_list() -> Value {
    let prompts: Vec<Value> = PROMPTS
        .iter()
        .map(|(name, title, description, required)| {
            json!({
                "name": name,
                "title": title,
                "description": description,
                "arguments": [{
                    "name": "document_id",
                    "description": "The document's id, from list_documents.",
                    "required": required,
                }],
            })
        })
        .collect();
    json!({ "prompts": prompts })
}

/// The `prompts/get` result, or why the request is wrong.
pub fn get_prompt(params: &Value) -> Result<Value, String> {
    let name = params
        .get("name")
        .and_then(Value::as_str)
        .ok_or("`params.name` must be the name of a prompt")?;
    let Some((name, _, description, required)) = PROMPTS.iter().find(|p| p.0 == name) else {
        let shown: String = name.chars().take(64).collect();
        return Err(format!("no prompt `{shown}`"));
    };
    let empty = Map::new();
    let arguments = match params.get("arguments") {
        None | Some(Value::Null) => &empty,
        Some(Value::Object(arguments)) => arguments,
        Some(_) => return Err("`params.arguments` must be an object".to_owned()),
    };
    let document = match arguments.get("document_id") {
        None | Some(Value::Null) => None,
        Some(Value::String(id)) if id.trim().is_empty() => None,
        Some(Value::String(id)) => Some(id.trim()),
        Some(_) => return Err("`document_id` must be a string".to_owned()),
    };
    if let Some(id) = document
        && (id.chars().count() > MAX_PROMPT_ARGUMENT || id.chars().any(char::is_control))
    {
        return Err("`document_id` is not a document id".to_owned());
    }
    if *required && document.is_none() {
        return Err(format!("the prompt `{name}` needs `document_id`"));
    }
    let text = match *name {
        "summarize_document" => summarize_text(document.unwrap_or_default()),
        _ => proofread_text(document),
    };
    Ok(json!({
        "description": description,
        "messages": [{ "role": "user", "content": { "type": "text", "text": text } }],
    }))
}

/// How to read a whole document, for the prompts.
fn reading(id: &str) -> String {
    let argument = Value::String(id.to_owned());
    format!(
        "Read it with the read_document tool ({{\"document\": {argument}}}). Long documents come in parts: when the result says more lines follow, call read_document again with start_line until you have read all of it."
    )
}

fn summarize_text(id: &str) -> String {
    format!(
        "Summarize the document `{id}` that is open in Aster Office. {} Then sum up its main points in a few sentences, in the language the document is written in. Do not change the document: use no tool that edits or saves.",
        reading(id)
    )
}

fn proofread_text(id: Option<&str>) -> String {
    let (which, read) = match id {
        Some(id) => (format!("the document `{id}`"), reading(id)),
        None => (
            "a document".to_owned(),
            "Call list_documents to find it; if it is not clear which document is meant, ask. Read it with the read_document tool; long documents come in parts, so call it again with start_line until you have read all of it.".to_owned(),
        ),
    };
    format!(
        "Proofread {which} that is open in Aster Office. {read} Fix only spelling, punctuation and grammar mistakes; keep the wording, the style, the Markdown formatting and the meaning as they are. Make minimal edits: change each mistake with replace_lines on just the lines it is on (using the line numbers from read_document), or with find_and_replace (one occurrence, plain text) for a short phrase. Never rewrite a whole paragraph or the whole document. When a change adds or removes lines, the line numbers after it shift: read the document again before the next replace_lines. The user is asked to approve changes and may decline one; do not retry a declined change. Finish with a short list of what you changed."
    )
}

/// Percent-encode everything but unreserved characters, so any id makes a
/// valid URI.
fn percent_encode(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for byte in text.bytes() {
        if byte.is_ascii_alphanumeric() || b"-._~".contains(&byte) {
            out.push(char::from(byte));
        } else {
            out.push_str(&format!("%{byte:02X}"));
        }
    }
    out
}

/// The reverse of [`percent_encode`]; `None` for broken escapes or text
/// that is not UTF-8.
fn percent_decode(text: &str) -> Option<String> {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut at = 0;
    while at < bytes.len() {
        if bytes[at] == b'%' {
            let digits = bytes.get(at + 1..at + 3)?;
            if !digits.iter().all(u8::is_ascii_hexdigit) {
                return None;
            }
            let hex = std::str::from_utf8(digits).ok()?;
            out.push(u8::from_str_radix(hex, 16).ok()?);
            at += 3;
        } else {
            out.push(bytes[at]);
            at += 1;
        }
    }
    String::from_utf8(out).ok()
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

    #[test]
    fn spreadsheets_are_resources_of_their_own() {
        let listed = ok(
            &json!({"documents": [{"id": "4", "title": "Budget", "kind": "spreadsheet", "access": "ask"}]})
                .to_string(),
        );
        let list = resource_list(&listed).unwrap();
        assert_eq!(list["resources"][1]["uri"], "aster://sheet/4");
        assert_eq!(list["resources"][1]["mimeType"], "application/json");
        assert_eq!(
            Resource::parse("aster://sheet/4"),
            Some(Resource::Sheet("4".into()))
        );
        let read = read_result(&Resource::Sheet("4".into()), &ok("{\"rows\":[]}")).unwrap();
        assert_eq!(read["contents"][0]["mimeType"], "application/json");
        assert_eq!(
            resource_templates()["resourceTemplates"]
                .as_array()
                .unwrap()
                .len(),
            2
        );
    }

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
                json!({"jsonrpc": "2.0", "id": 1, "method": "sampling/createMessage"})
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
                job: Job::Tool {
                    name: "list_documents".into(),
                    arguments: json!({}),
                },
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
    fn resources_are_jobs_and_unknown_uris_are_not_found() {
        let mut session = initialized();
        let request = |method: &str, params: Value| json!({"jsonrpc": "2.0", "id": 3, "method": method, "params": params});
        assert_eq!(
            handle(&mut session, request("resources/list", json!({}))),
            Outcome::Call {
                id: json!(3),
                client: "t".into(),
                job: Job::ListResources,
            }
        );
        assert_eq!(
            handle(
                &mut session,
                request("resources/read", json!({"uri": "aster://document/a%20b"}))
            ),
            Outcome::Call {
                id: json!(3),
                client: "t".into(),
                job: Job::ReadResource(Resource::Document("a b".into())),
            }
        );
        for uri in [
            "aster://document/",
            "aster://document/%zz",
            "aster://document/%+1",
            "aster://document/%FF",
            "file:///etc/passwd",
            "aster://documents/1",
        ] {
            assert_eq!(
                error_code(handle(
                    &mut session,
                    request("resources/read", json!({"uri": uri}))
                )),
                RESOURCE_NOT_FOUND,
                "{uri}"
            );
        }
        assert_eq!(
            error_code(handle(
                &mut session,
                request("resources/read", json!({"uri": 5}))
            )),
            INVALID_PARAMS
        );
        let Outcome::Reply(templates) =
            handle(&mut session, request("resources/templates/list", json!({})))
        else {
            panic!("no templates");
        };
        assert_eq!(
            templates["result"]["resourceTemplates"][0]["uriTemplate"],
            "aster://document/{id}"
        );
        // Before initialize, nothing of this is served.
        let mut fresh = Session::new();
        assert_eq!(
            error_code(handle(&mut fresh, request("resources/list", json!({})))),
            NOT_INITIALIZED
        );
    }

    #[test]
    fn resource_uris_round_trip() {
        for id in ["1", "a b", "笔记/../x", "%", "50%"] {
            let resource = Resource::Document(id.into());
            let uri = resource.uri();
            assert!(uri.bytes().all(|b| b.is_ascii_graphic()), "{uri}");
            assert_eq!(Resource::parse(&uri), Some(resource));
        }
        assert_eq!(Resource::parse(DOCUMENTS_URI), Some(Resource::Documents));
        assert_eq!(Resource::Document("7".into()).uri(), "aster://document/7");
    }

    fn ok(text: &str) -> aster_tools::ToolResult {
        aster_tools::ToolResult {
            text: text.to_owned(),
            is_error: false,
        }
    }

    fn failed(text: &str) -> aster_tools::ToolResult {
        aster_tools::ToolResult {
            text: text.to_owned(),
            is_error: true,
        }
    }

    #[test]
    fn listings_and_reads_follow_the_tools() {
        let listed = ok(&json!({"documents": [
            {"id": "1", "title": "notes.md", "kind": "markdown", "access": "ask"},
            {"id": "x y", "title": "budget.xlsx", "kind": "spreadsheet", "access": "read_only"},
        ]})
        .to_string());
        let list = resource_list(&listed).unwrap();
        let resources = list["resources"].as_array().unwrap();
        assert_eq!(resources.len(), 3);
        assert_eq!(resources[0]["uri"], DOCUMENTS_URI);
        assert_eq!(resources[0]["mimeType"], "application/json");
        assert_eq!(resources[1]["uri"], "aster://document/1");
        assert_eq!(resources[1]["name"], "notes.md");
        assert_eq!(resources[1]["mimeType"], "text/markdown");
        assert_eq!(resources[2]["uri"], "aster://sheet/x%20y");
        assert_eq!(resources[2]["mimeType"], "application/json");
        assert!(list.get("nextCursor").is_none());
        // Nothing visible: only the list itself.
        let empty = resource_list(&ok(r#"{"documents": []}"#)).unwrap();
        assert_eq!(empty["resources"].as_array().unwrap().len(), 1);
        assert_eq!(resource_list(&failed("no")).unwrap_err().0, INTERNAL_ERROR);
        assert_eq!(resource_list(&ok("nope")).unwrap_err().0, INTERNAL_ERROR);

        let read = read_result(&Resource::Documents, &listed).unwrap();
        assert_eq!(read["contents"][0]["text"], listed.text);
        assert_eq!(read["contents"][0]["uri"], DOCUMENTS_URI);
        let document = Resource::Document("1".into());
        let read = read_result(
            &document,
            &ok("[lines 1-1 of 1]
1: hi
"),
        )
        .unwrap();
        assert_eq!(
            read,
            json!({"contents": [{"uri": "aster://document/1", "mimeType": "text/plain",
                                  "text": "[lines 1-1 of 1]
1: hi
"}]})
        );
        // Hidden or not open: not found. Any other tool error: an error.
        let hidden = failed(&aster_tools::invoke::unknown_document("1"));
        assert_eq!(
            read_result(&document, &hidden).unwrap_err().0,
            RESOURCE_NOT_FOUND
        );
        let unsupported = failed("not supported for spreadsheets yet");
        assert_eq!(
            read_result(&document, &unsupported).unwrap_err(),
            (INTERNAL_ERROR, unsupported.text.clone())
        );
        assert_eq!(
            read_arguments("1"),
            json!({"document": "1", "max_lines": 2000})
        );
    }

    #[test]
    fn prompts_name_the_tools_and_carry_no_document_text() {
        let list = prompt_list();
        let prompts = list["prompts"].as_array().unwrap();
        let names: Vec<&str> = prompts
            .iter()
            .map(|p| p["name"].as_str().unwrap())
            .collect();
        assert_eq!(names, ["summarize_document", "proofread_document"]);
        assert_eq!(prompts[0]["arguments"][0]["name"], "document_id");
        assert_eq!(prompts[0]["arguments"][0]["required"], true);
        assert_eq!(prompts[1]["arguments"][0]["required"], false);

        let summary = get_prompt(&json!({"name": "summarize_document",
                                          "arguments": {"document_id": "7"}}))
        .unwrap();
        let message = &summary["messages"][0];
        assert_eq!(message["role"], "user");
        let text = message["content"]["text"].as_str().unwrap();
        assert!(text.contains("read_document") && text.contains(r#"{"document": "7"}"#));
        assert!(text.contains("Do not change the document"));

        let proofread = get_prompt(&json!({"name": "proofread_document",
                                            "arguments": {"document_id": "7"}}))
        .unwrap();
        let text = proofread["messages"][0]["content"]["text"]
            .as_str()
            .unwrap();
        for tool in ["read_document", "replace_lines", "find_and_replace"] {
            assert!(text.contains(tool), "{tool}");
        }
        assert!(text.contains("minimal edits"));
        let any = get_prompt(&json!({"name": "proofread_document"})).unwrap();
        let text = any["messages"][0]["content"]["text"].as_str().unwrap();
        assert!(text.contains("list_documents"));

        for bad in [
            json!({"name": "summarize_document"}),
            json!({"name": "summarize_document", "arguments": {"document_id": " "}}),
            json!({"name": "summarize_document", "arguments": {"document_id": 7}}),
            json!({"name": "summarize_document", "arguments": {"document_id": "a\nb"}}),
            json!({"name": "summarize_document", "arguments": {"document_id": "x".repeat(300)}}),
            json!({"name": "summarize_document", "arguments": []}),
            json!({"name": "nope"}),
            json!({}),
        ] {
            assert!(get_prompt(&bad).is_err(), "{bad}");
        }
        // Through the session: a bad request is invalid params.
        let mut session = initialized();
        assert_eq!(
            error_code(handle(
                &mut session,
                json!({"jsonrpc": "2.0", "id": 1, "method": "prompts/get",
                       "params": {"name": "nope"}})
            )),
            INVALID_PARAMS
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

    /// The tools for Word documents are offered to MCP clients with the
    /// same hints as the spreadsheet ones: the reader read-only, the
    /// writers not.
    #[test]
    fn the_word_tools_are_in_the_list_with_their_hints() {
        let list = tool_list()["tools"].as_array().unwrap();
        let entry = |name: &str| {
            list.iter()
                .find(|entry| entry["name"] == name)
                .unwrap_or_else(|| panic!("{name} is listed"))
        };
        assert_eq!(entry("read_word")["annotations"]["readOnlyHint"], true);
        for name in [
            "replace_paragraphs",
            "insert_paragraphs",
            "delete_blocks",
            "find_and_replace",
            "format_text",
            "edit_table",
            "write_cells",
        ] {
            assert_eq!(entry(name)["annotations"]["readOnlyHint"], false, "{name}");
            assert_eq!(entry(name)["inputSchema"]["additionalProperties"], false);
        }
        // Deleting and replacing are marked destructive, inserting and
        // formatting are not.
        assert_eq!(
            entry("delete_blocks")["annotations"]["destructiveHint"],
            true
        );
        assert_eq!(
            entry("format_text")["annotations"]["destructiveHint"],
            false
        );
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
