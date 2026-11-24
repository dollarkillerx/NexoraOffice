//! The audit log: one record for every tool call, whoever made it and
//! however it ended. Records go to a JSON Lines file (one object per line,
//! only ever appended) and the latest few hundred stay in memory for the
//! interface. Text arguments are recorded by their length only.

use std::collections::VecDeque;
use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::Path;
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::{Map, Value, json};

/// How many records the interface can look back on.
const KEPT: usize = 500;

/// Arguments whose values are recorded as a length only: they hold the
/// user's text.
const TEXT_ARGUMENTS: &[&str] = &["text", "find", "replace", "query", "title"];

#[derive(Clone, Debug, PartialEq)]
pub struct AuditRecord {
    /// Milliseconds since the Unix epoch.
    pub time: u64,
    /// `mcp:<client>` or `ai:<provider>/<model>`.
    pub caller: String,
    pub tool: String,
    /// The document's title, if the call named one.
    pub document: Option<String>,
    /// `allow`, `ask`, `deny`, `hidden` or `invalid`.
    pub verdict: &'static str,
    /// The user's answer, for calls that were asked about.
    pub approval: Option<&'static str>,
    pub ok: bool,
    pub error: Option<String>,
    /// The arguments, text ones as their length.
    pub arguments: Value,
}

impl AuditRecord {
    fn to_json(&self) -> Value {
        json!({
            "time": self.time,
            "caller": self.caller,
            "tool": self.tool,
            "document": self.document,
            "verdict": self.verdict,
            "approval": self.approval,
            "ok": self.ok,
            "error": self.error,
            "arguments": self.arguments,
        })
    }
}

/// The arguments as they are recorded: the user's text replaced by its
/// length in characters.
pub fn summarise(args: &Value) -> Value {
    let Some(object) = args.as_object() else {
        return Value::Null;
    };
    let mut out = Map::new();
    for (name, value) in object {
        let value = match value.as_str() {
            Some(text) if TEXT_ARGUMENTS.contains(&name.as_str()) => {
                json!({ "chars": text.chars().count() })
            }
            _ => value.clone(),
        };
        out.insert(name.clone(), value);
    }
    Value::Object(out)
}

pub fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as u64)
}

#[derive(Debug, Default)]
pub struct Audit {
    file: Option<Mutex<File>>,
    recent: Mutex<VecDeque<AuditRecord>>,
}

impl Audit {
    /// An audit log that keeps records in memory only.
    pub fn in_memory() -> Self {
        Self::default()
    }

    /// An audit log appending to `path`, created if missing.
    pub fn open(path: &Path) -> std::io::Result<Self> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let mut options = OpenOptions::new();
        options.create(true).append(true);
        // Only the user may read what their assistant and clients did.
        #[cfg(unix)]
        std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
        let file = options.open(path)?;
        #[cfg(unix)]
        std::fs::set_permissions(path, std::os::unix::fs::PermissionsExt::from_mode(0o600))?;
        Ok(Self {
            file: Some(Mutex::new(file)),
            recent: Mutex::new(VecDeque::new()),
        })
    }

    pub fn record(&self, record: AuditRecord) {
        if let Some(file) = &self.file
            && let Ok(mut file) = file.lock()
        {
            // A full disk must not stop the tool from answering.
            let _ = writeln!(file, "{}", record.to_json());
        }
        if let Ok(mut recent) = self.recent.lock() {
            if recent.len() == KEPT {
                recent.pop_front();
            }
            recent.push_back(record);
        }
    }

    /// The latest records, oldest first.
    pub fn recent(&self) -> Vec<AuditRecord> {
        self.recent
            .lock()
            .map(|recent| recent.iter().cloned().collect())
            .unwrap_or_default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn record(tool: &str) -> AuditRecord {
        AuditRecord {
            time: 1,
            caller: "ai:test/model".to_owned(),
            tool: tool.to_owned(),
            document: Some("笔记.md".to_owned()),
            verdict: "allow",
            approval: None,
            ok: true,
            error: None,
            arguments: summarise(&json!({"document": "1", "text": "你好世界"})),
        }
    }

    #[test]
    fn text_arguments_are_recorded_by_length() {
        let summary = summarise(&json!({"document": "1", "text": "你好世界", "line": 3}));
        assert_eq!(
            summary,
            json!({"document": "1", "text": {"chars": 4}, "line": 3})
        );
    }

    #[test]
    fn records_are_appended_to_the_file_and_kept_in_memory() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("audit/tools.jsonl");
        let audit = Audit::open(&path).unwrap();
        audit.record(record("read_document"));
        audit.record(record("insert_text"));
        let text = std::fs::read_to_string(&path).unwrap();
        let lines: Vec<Value> = text
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect();
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[1]["tool"], "insert_text");
        assert_eq!(lines[1]["arguments"]["text"]["chars"], 4);
        assert!(!text.contains("你好世界"));
        assert_eq!(audit.recent().len(), 2);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&path).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o600);
        }
    }
}
