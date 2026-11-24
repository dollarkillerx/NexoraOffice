//! The call pipeline both callers share: look up, check, find the target,
//! judge, ask, run, cut down, audit.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::{Value, json};

use crate::audit::{Audit, AuditRecord, now_ms, summarise};
use crate::catalogue::{Capability, ToolSpec, find};
use crate::policy::{Level, Policy, Reason, Verdict, judge};
use crate::schema;

/// How long an approval waits for the user before it counts as refused.
pub const APPROVAL_TIMEOUT: Duration = Duration::from_secs(120);
/// Results longer than this many characters are cut.
pub const MAX_RESULT_CHARS: usize = 60_000;

/// An open document, as the application describes it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DocumentInfo {
    /// Stable while the document is open.
    pub id: String,
    pub title: String,
    pub path: Option<PathBuf>,
    /// `markdown`, `spreadsheet`, `presentation`, …
    pub kind: String,
    pub modified: bool,
}

/// Who is calling: `mcp:<client>` or `ai:<provider>/<model>`, and the
/// connection or conversation, which "allow for this session" is kept for.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Origin {
    pub caller: String,
    pub session: String,
}

impl Origin {
    pub fn new(caller: impl Into<String>, session: impl Into<String>) -> Self {
        Self {
            caller: caller.into(),
            session: session.into(),
        }
    }

    /// Whether the call comes from an external MCP client.
    pub fn is_mcp(&self) -> bool {
        self.caller.starts_with("mcp:")
    }
}

/// The application side: what is open, and running a tool once every check
/// has passed. Called from the caller's thread.
pub trait Host: Send + Sync {
    fn documents(&self) -> Vec<DocumentInfo>;
    /// Run `tool`. `document` is the document named by the `document`
    /// argument; `path` arguments have been made absolute and canonical. The
    /// result is text for the caller; an `Err` is a tool error the caller
    /// sees, not a failure of the call. A created or opened document is
    /// reported with a `document` field in the JSON text.
    fn run(
        &self,
        tool: &ToolSpec,
        document: Option<&DocumentInfo>,
        args: &Value,
    ) -> Result<String, String>;
}

/// A question for the user about one call.
#[derive(Clone, Debug)]
pub struct ApprovalRequest {
    /// Distinct for every request.
    pub id: u64,
    pub origin: Origin,
    pub tool: &'static ToolSpec,
    pub document: Option<DocumentInfo>,
    pub reason: Reason,
    pub args: Value,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Approval {
    Allow,
    /// Allow, and the same tool on the same document for the rest of this
    /// conversation or connection.
    AllowSession,
    Deny,
    TimedOut,
}

impl Approval {
    fn key(self) -> &'static str {
        match self {
            Approval::Allow => "allow",
            Approval::AllowSession => "allow_session",
            Approval::Deny => "deny",
            Approval::TimedOut => "timed_out",
        }
    }
}

/// Asks the user. Blocks the calling thread until an answer or `timeout`.
pub trait Approver: Send + Sync {
    fn ask(&self, request: ApprovalRequest, timeout: Duration) -> Approval;
}

/// What the caller gets back. A failed call is a result too (`is_error`),
/// so the model can read why and try something else.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ToolResult {
    pub text: String,
    pub is_error: bool,
}

impl ToolResult {
    fn ok(text: String) -> Self {
        Self {
            text: cut(text),
            is_error: false,
        }
    }

    fn error(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            is_error: true,
        }
    }
}

/// An error a caller can see; also how a [`ToolResult`] error reads.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ToolError(pub String);

fn cut(text: String) -> String {
    let count = text.chars().count();
    if count <= MAX_RESULT_CHARS {
        return text;
    }
    let end = text
        .char_indices()
        .nth(MAX_RESULT_CHARS)
        .map_or(text.len(), |(at, _)| at);
    format!(
        "{}\n[cut: {} more characters; ask for less, for example with start_line and max_lines]",
        &text[..end],
        count - MAX_RESULT_CHARS
    )
}

pub struct Invoker {
    policy: Arc<Mutex<Policy>>,
    host: Arc<dyn Host>,
    approver: Arc<dyn Approver>,
    audit: Arc<Audit>,
    /// "Allow for this session": session, tool, document id.
    allowed: Mutex<HashSet<(String, &'static str, String)>>,
    next_request: AtomicU64,
    timeout: Duration,
}

/// What a call targets once its arguments are understood.
enum Target {
    None,
    Document(DocumentInfo),
    Path(PathBuf),
}

impl Invoker {
    pub fn new(
        policy: Arc<Mutex<Policy>>,
        host: Arc<dyn Host>,
        approver: Arc<dyn Approver>,
        audit: Arc<Audit>,
    ) -> Self {
        Self {
            policy,
            host,
            approver,
            audit,
            allowed: Mutex::new(HashSet::new()),
            next_request: AtomicU64::new(1),
            timeout: APPROVAL_TIMEOUT,
        }
    }

    /// Wait this long for approvals instead of two minutes; for tests.
    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }

    pub fn policy(&self) -> &Arc<Mutex<Policy>> {
        &self.policy
    }

    pub fn audit(&self) -> &Arc<Audit> {
        &self.audit
    }

    /// Forget the "allow for this session" answers of a conversation or
    /// connection that has ended.
    pub fn end_session(&self, session: &str) {
        if let Ok(mut allowed) = self.allowed.lock() {
            allowed.retain(|(s, _, _)| s != session);
        }
    }

    fn policy_now(&self) -> Policy {
        self.policy.lock().map(|p| p.clone()).unwrap_or_default()
    }

    /// Make a tool call. Exactly one audit record is written.
    pub fn call(&self, origin: &Origin, name: &str, args: &Value) -> ToolResult {
        let mut record = AuditRecord {
            time: now_ms(),
            caller: origin.caller.clone(),
            tool: name.to_owned(),
            document: None,
            verdict: "invalid",
            approval: None,
            ok: false,
            error: None,
            arguments: summarise(args),
        };
        let result = self.call_inner(origin, name, args, &mut record);
        record.ok = !result.is_error;
        // The pipeline's own messages are fixed text; a tool's may repeat
        // what the user looked for, so it is not copied (see `call_inner`).
        if result.is_error && record.error.is_none() {
            record.error = Some(result.text.chars().take(300).collect());
        }
        self.audit.record(record);
        result
    }

    fn call_inner(
        &self,
        origin: &Origin,
        name: &str,
        args: &Value,
        record: &mut AuditRecord,
    ) -> ToolResult {
        let Some(tool) = find(name) else {
            return ToolResult::error(format!("unknown tool `{name}`"));
        };
        if let Err(problem) = schema::check(&tool.schema(), args) {
            return ToolResult::error(format!("invalid arguments: {problem}"));
        }
        let policy = self.policy_now();

        if tool.name == "list_documents" {
            record.verdict = "allow";
            return ToolResult::ok(self.list_documents(&policy, &origin.session));
        }

        // The target and its level.
        let (target, level) = if tool.on_document {
            let id = args["document"].as_str().unwrap_or_default();
            let found = self.host.documents().into_iter().find(|d| d.id == id);
            let Some(document) = found else {
                record.verdict = "hidden";
                return ToolResult::error(unknown_document(id));
            };
            let level = policy.level_for(&origin.session, &document.id, document.path.as_deref());
            record.document = Some(document.title.clone());
            (Target::Document(document), level)
        } else if let Some(path) = args.get("path").and_then(Value::as_str) {
            let path = match canonical(path) {
                Ok(path) => path,
                Err(error) => {
                    record.verdict = "hidden";
                    return ToolResult::error(error);
                }
            };
            let level = policy.level_of_path(&path);
            (Target::Path(path), level)
        } else {
            // Creating a document follows the level of new documents.
            (Target::None, policy.create_level())
        };

        let mut verdict = judge(level, tool.capability);
        if verdict == Verdict::Hidden {
            record.verdict = "hidden";
            return ToolResult::error(match &target {
                Target::Document(document) => unknown_document(&document.id),
                Target::Path(path) => {
                    format!("no folder or file `{}` that you may see", path.display())
                }
                Target::None => "not allowed".to_owned(),
            });
        }
        let document = match &target {
            Target::Document(document) => Some(document.clone()),
            _ => None,
        };
        let document_key = document.as_ref().map(|d| d.id.clone()).unwrap_or_default();
        if let Verdict::Ask(_) = verdict
            && self.allowed.lock().is_ok_and(|allowed| {
                allowed.contains(&(origin.session.clone(), tool.name, document_key.clone()))
            })
        {
            verdict = Verdict::Allow;
        }
        match verdict {
            Verdict::Allow => record.verdict = "allow",
            Verdict::Deny(reason) => {
                record.verdict = "deny";
                return ToolResult::error(refusal(reason));
            }
            Verdict::Hidden => unreachable!(),
            Verdict::Ask(reason) => {
                record.verdict = "ask";
                let request = ApprovalRequest {
                    id: self.next_request.fetch_add(1, Ordering::Relaxed),
                    origin: origin.clone(),
                    tool,
                    document: document.clone(),
                    reason,
                    args: args.clone(),
                };
                let answer = self.approver.ask(request, self.timeout);
                record.approval = Some(answer.key());
                match answer {
                    Approval::Allow => {}
                    Approval::AllowSession => {
                        if let Ok(mut allowed) = self.allowed.lock() {
                            allowed.insert((origin.session.clone(), tool.name, document_key));
                        }
                    }
                    Approval::Deny => {
                        return ToolResult::error("the user declined this action");
                    }
                    Approval::TimedOut => {
                        return ToolResult::error(
                            "the user did not answer in time; the action was not taken",
                        );
                    }
                }
            }
        }

        let mut args = args.clone();
        if let Target::Path(path) = &target {
            args["path"] = Value::String(path.display().to_string());
        }
        match self.host.run(tool, document.as_ref(), &args) {
            Ok(text) => {
                // A document the caller made itself it may go on editing;
                // only that caller, and only until the document is saved.
                if tool.capability == Capability::Create
                    && let Ok(value) = serde_json::from_str::<Value>(&text)
                    && let Some(id) = value["document"].as_str()
                    && let Ok(mut policy) = self.policy.lock()
                {
                    policy.grant_creator(&origin.session, id, Level::Trusted);
                }
                ToolResult::ok(text)
            }
            Err(error) => {
                record.error = Some("the tool reported an error".to_owned());
                ToolResult::error(error)
            }
        }
    }

    fn list_documents(&self, policy: &Policy, session: &str) -> String {
        let documents: Vec<Value> = self
            .host
            .documents()
            .into_iter()
            .filter_map(|document| {
                let level = policy.level_for(session, &document.id, document.path.as_deref());
                (level != Level::Off).then(|| {
                    json!({
                        "id": document.id,
                        "title": document.title,
                        "path": document.path.as_ref().map(|p| p.display().to_string()),
                        "kind": document.kind,
                        "modified": document.modified,
                        "access": level.key(),
                    })
                })
            })
            .collect();
        json!({ "documents": documents }).to_string()
    }
}

fn unknown_document(id: &str) -> String {
    format!("no open document `{id}` that you may see; call list_documents")
}

fn refusal(reason: Reason) -> String {
    match reason {
        Reason::ReadOnly => "you have read-only access to this; changes are not allowed".to_owned(),
        other => format!("not allowed: {}", other.key()),
    }
}

/// An absolute path with `..` and links resolved, so that a folder rule
/// cannot be stepped out of.
fn canonical(path: &str) -> Result<PathBuf, String> {
    let path = Path::new(path);
    if !path.is_absolute() {
        return Err(format!("`{}` is not an absolute path", path.display()));
    }
    path.canonicalize()
        .map_err(|_| format!("no folder or file `{}` that you may see", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    struct FakeHost {
        documents: Vec<DocumentInfo>,
        runs: Mutex<Vec<(String, Value)>>,
    }

    impl Host for FakeHost {
        fn documents(&self) -> Vec<DocumentInfo> {
            self.documents.clone()
        }

        fn run(
            &self,
            tool: &ToolSpec,
            _: Option<&DocumentInfo>,
            args: &Value,
        ) -> Result<String, String> {
            self.runs
                .lock()
                .unwrap()
                .push((tool.name.to_owned(), args.clone()));
            match tool.name {
                "create_document" => Ok(json!({"document": "9"}).to_string()),
                "read_document" => Ok("1: 你好".to_owned()),
                "save_document" => Err("the disk is full".to_owned()),
                _ => Ok("done".to_owned()),
            }
        }
    }

    struct FakeApprover {
        answer: Approval,
        asked: Mutex<Vec<ApprovalRequest>>,
    }

    impl Approver for FakeApprover {
        fn ask(&self, request: ApprovalRequest, _: Duration) -> Approval {
            self.asked.lock().unwrap().push(request);
            self.answer
        }
    }

    fn setup(answer: Approval) -> (Invoker, Arc<FakeHost>, Arc<FakeApprover>) {
        let host = Arc::new(FakeHost {
            documents: vec![
                DocumentInfo {
                    id: "1".into(),
                    title: "笔记.md".into(),
                    path: Some("/notes/笔记.md".into()),
                    kind: "markdown".into(),
                    modified: false,
                },
                DocumentInfo {
                    id: "2".into(),
                    title: "私密.md".into(),
                    path: Some("/private/私密.md".into()),
                    kind: "markdown".into(),
                    modified: true,
                },
            ],
            runs: Mutex::new(Vec::new()),
        });
        let approver = Arc::new(FakeApprover {
            answer,
            asked: Mutex::new(Vec::new()),
        });
        let mut policy = Policy::default();
        policy.set_folder("/notes".into(), Level::Ask);
        let invoker = Invoker::new(
            Arc::new(Mutex::new(policy)),
            host.clone(),
            approver.clone(),
            Arc::new(Audit::in_memory()),
        );
        (invoker, host, approver)
    }

    fn ai() -> Origin {
        Origin::new("ai:test/model", "conversation-1")
    }

    #[test]
    fn documents_without_access_do_not_exist() {
        let (invoker, host, _) = setup(Approval::Allow);
        let listed = invoker.call(&ai(), "list_documents", &json!({}));
        assert!(!listed.is_error);
        assert!(listed.text.contains("笔记.md"));
        assert!(!listed.text.contains("私密"));
        assert!(listed.text.contains("\"access\":\"ask\""));
        let hidden = invoker.call(&ai(), "read_document", &json!({"document": "2"}));
        assert!(hidden.is_error);
        assert!(hidden.text.contains("no open document `2`"));
        // The same answer as for a document that is not open at all.
        let missing = invoker.call(&ai(), "read_document", &json!({"document": "5"}));
        assert!(missing.text.contains("no open document `5`"));
        assert!(host.runs.lock().unwrap().is_empty());
        let records = invoker.audit().recent();
        assert_eq!(records.len(), 3);
        assert_eq!(records[1].verdict, "hidden");
    }

    #[test]
    fn reading_runs_and_changes_are_asked_about() {
        let (invoker, host, approver) = setup(Approval::Allow);
        let read = invoker.call(&ai(), "read_document", &json!({"document": "1"}));
        assert_eq!(read.text, "1: 你好");
        assert!(approver.asked.lock().unwrap().is_empty());
        let insert = invoker.call(
            &ai(),
            "insert_text",
            &json!({"document": "1", "text": "新"}),
        );
        assert!(!insert.is_error);
        let asked = approver.asked.lock().unwrap();
        assert_eq!(asked.len(), 1);
        assert_eq!(asked[0].reason, Reason::Edit);
        assert_eq!(asked[0].document.as_ref().unwrap().title, "笔记.md");
        assert_eq!(host.runs.lock().unwrap().len(), 2);
        let records = invoker.audit().recent();
        assert_eq!(records[1].approval, Some("allow"));
        assert_eq!(records[1].arguments["text"]["chars"], 1);
    }

    #[test]
    fn a_refusal_or_no_answer_stops_the_call() {
        for answer in [Approval::Deny, Approval::TimedOut] {
            let (invoker, host, _) = setup(answer);
            let result = invoker.call(
                &ai(),
                "replace_selection",
                &json!({"document": "1", "text": "x"}),
            );
            assert!(result.is_error);
            assert!(host.runs.lock().unwrap().is_empty());
        }
    }

    #[test]
    fn allowing_for_the_session_stops_asking_in_that_session_only() {
        let (invoker, _, approver) = setup(Approval::AllowSession);
        let args = json!({"document": "1", "text": "x"});
        invoker.call(&ai(), "insert_text", &args);
        invoker.call(&ai(), "insert_text", &args);
        assert_eq!(approver.asked.lock().unwrap().len(), 1);
        // Another tool, another session: asked again.
        invoker.call(&ai(), "replace_selection", &args);
        invoker.call(
            &Origin::new("mcp:client", "connection-1"),
            "insert_text",
            &args,
        );
        assert_eq!(approver.asked.lock().unwrap().len(), 3);
        invoker.end_session("conversation-1");
        invoker.call(&ai(), "insert_text", &args);
        assert_eq!(approver.asked.lock().unwrap().len(), 4);
    }

    #[test]
    fn read_only_refuses_changes_and_trusted_asks_before_saving() {
        let (invoker, host, approver) = setup(Approval::Allow);
        invoker
            .policy()
            .lock()
            .unwrap()
            .set_folder("/notes".into(), Level::ReadOnly);
        let refused = invoker.call(&ai(), "insert_text", &json!({"document": "1", "text": "x"}));
        assert!(refused.is_error);
        assert!(refused.text.contains("read-only"));
        invoker
            .policy()
            .lock()
            .unwrap()
            .set_folder("/notes".into(), Level::Trusted);
        assert!(
            !invoker
                .call(&ai(), "insert_text", &json!({"document": "1", "text": "x"}))
                .is_error
        );
        assert!(approver.asked.lock().unwrap().is_empty());
        // Saving asks; the host's failure comes back as a tool error.
        let saved = invoker.call(&ai(), "save_document", &json!({"document": "1"}));
        assert_eq!(approver.asked.lock().unwrap()[0].reason, Reason::Save);
        assert_eq!(saved, ToolResult::error("the disk is full"));
        assert_eq!(host.runs.lock().unwrap().len(), 2);
    }

    #[test]
    fn bad_calls_are_errors_with_a_record() {
        let (invoker, _, _) = setup(Approval::Allow);
        assert!(
            invoker
                .call(&ai(), "rm_rf", &json!({}))
                .text
                .contains("unknown tool")
        );
        assert!(
            invoker
                .call(&ai(), "read_document", &json!({}))
                .text
                .contains("missing argument")
        );
        let records = invoker.audit().recent();
        assert_eq!(records.len(), 2);
        assert!(records.iter().all(|r| r.verdict == "invalid" && !r.ok));
    }

    #[test]
    fn creating_asks_and_the_new_document_is_granted() {
        let (invoker, _, approver) = setup(Approval::Allow);
        let created = invoker.call(&ai(), "create_document", &json!({"text": "# 新"}));
        assert!(!created.is_error);
        assert_eq!(approver.asked.lock().unwrap()[0].reason, Reason::Create);
        let policy = invoker.policy().lock().unwrap();
        assert_eq!(
            policy.level_for("conversation-1", "9", None),
            Level::Trusted
        );
        assert_eq!(policy.level_for("connection-2", "9", None), Level::Off);
    }

    #[test]
    fn paths_are_checked_after_resolving_them() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        std::fs::create_dir(root.join("open")).unwrap();
        std::fs::create_dir(root.join("closed")).unwrap();
        let (invoker, host, _) = setup(Approval::Allow);
        invoker
            .policy()
            .lock()
            .unwrap()
            .set_folder(root.join("open"), Level::ReadOnly);
        let inside = root.join("open");
        let listed = invoker.call(
            &ai(),
            "list_folder",
            &json!({"path": inside.display().to_string()}),
        );
        assert!(!listed.is_error);
        // `..` does not get out of the folder.
        let escape = format!("{}/../closed", inside.display());
        let refused = invoker.call(&ai(), "list_folder", &json!({"path": escape}));
        assert!(refused.is_error);
        assert!(
            invoker
                .call(&ai(), "list_folder", &json!({"path": "relative"}))
                .is_error
        );
        assert_eq!(host.runs.lock().unwrap().len(), 1);
    }

    #[test]
    fn long_results_are_cut() {
        let long = "字".repeat(MAX_RESULT_CHARS + 10);
        let cut = cut(long);
        assert!(cut.ends_with("max_lines]"));
        assert!(cut.contains("10 more characters"));
    }

    #[test]
    fn a_tools_own_error_text_stays_out_of_the_audit_log() {
        // The tool's message can repeat what was searched for; the caller
        // gets it, the log does not.
        let (invoker, _, _) = setup(Approval::Allow);
        invoker
            .policy()
            .lock()
            .unwrap()
            .set_folder("/notes".into(), Level::FullAccess);
        let saved = invoker.call(&ai(), "save_document", &json!({"document": "1"}));
        assert_eq!(saved.text, "the disk is full");
        let record = invoker.audit().recent().pop().unwrap();
        assert_eq!(record.error.as_deref(), Some("the tool reported an error"));
        // Creating while new documents are read-only is refused, not asked.
        invoker.policy().lock().unwrap().untitled = Level::ReadOnly;
        let refused = invoker.call(
            &ai(),
            "create_document",
            &json!({"text": "x", "title": "秘密标题"}),
        );
        assert!(refused.is_error);
        let record = invoker.audit().recent().pop().unwrap();
        assert_eq!(record.verdict, "deny");
        assert_eq!(record.arguments["title"]["chars"], 4);
    }
}
