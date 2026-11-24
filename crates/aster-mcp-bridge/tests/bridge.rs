//! The built bridge between a pipe pair and a real server, the way an MCP
//! client runs it.
#![cfg(unix)]

use std::io::{BufRead, BufReader, Read, Write};
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use aster_mcp::{McpConfig, McpServer, NoEvents};
use aster_tools::{
    Approval, ApprovalRequest, Approver, Audit, DocumentInfo, Host, Invoker, Policy, ToolSpec,
};
use serde_json::{Value, json};

const BRIDGE: &str = env!("CARGO_BIN_EXE_aster-mcp");

struct NoDocuments;

impl Host for NoDocuments {
    fn documents(&self) -> Vec<DocumentInfo> {
        Vec::new()
    }

    fn run(&self, _: &ToolSpec, _: Option<&DocumentInfo>, _: &Value) -> Result<String, String> {
        Err("nothing to run".to_owned())
    }
}

struct Refuse;

impl Approver for Refuse {
    fn ask(&self, _: ApprovalRequest, _: Duration) -> Approval {
        Approval::Deny
    }
}

#[test]
fn the_bridge_carries_a_session_both_ways() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("mcp.sock");
    let invoker = Invoker::new(
        Arc::new(Mutex::new(Policy::default())),
        Arc::new(NoDocuments),
        Arc::new(Refuse),
        Arc::new(Audit::in_memory()),
    );
    let _server = McpServer::start(
        McpConfig {
            socket_path: Some(path.clone()),
        },
        Arc::new(invoker),
        Arc::new(NoEvents),
    )
    .unwrap();

    let mut bridge = Command::new(BRIDGE)
        .env("ASTER_MCP_SOCKET", &path)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let mut stdin = bridge.stdin.take().unwrap();
    let mut stdout = BufReader::new(bridge.stdout.take().unwrap());
    let mut next = || {
        let mut line = String::new();
        stdout.read_line(&mut line).unwrap();
        serde_json::from_str::<Value>(&line).unwrap()
    };

    let initialize = json!({"jsonrpc": "2.0", "id": 1, "method": "initialize",
        "params": {"protocolVersion": "2025-06-18", "clientInfo": {"name": "bridge-test"}}});
    writeln!(stdin, "{initialize}").unwrap();
    let answer = next();
    assert_eq!(answer["id"], 1);
    assert_eq!(answer["result"]["serverInfo"]["name"], "aster-office");

    // Several messages in one write come back one by one.
    let more = format!(
        "{}\n{}\n{}\n",
        json!({"jsonrpc": "2.0", "method": "notifications/initialized"}),
        json!({"jsonrpc": "2.0", "id": 2, "method": "tools/list"}),
        json!({"jsonrpc": "2.0", "id": 3, "method": "tools/call",
               "params": {"name": "list_documents", "arguments": {}}}),
    );
    stdin.write_all(more.as_bytes()).unwrap();
    let listed = next();
    assert_eq!(listed["id"], 2);
    assert_eq!(
        listed["result"]["tools"].as_array().unwrap().len(),
        aster_tools::tools().len()
    );
    let called = next();
    assert_eq!(called["id"], 3);
    assert_eq!(called["result"]["isError"], false);

    // Closing stdin ends the session, and the bridge exits cleanly.
    drop(stdin);
    let status = bridge.wait().unwrap();
    assert_eq!(status.code(), Some(0));
    let mut rest = String::new();
    stdout.read_to_string(&mut rest).unwrap();
    assert!(rest.is_empty(), "{rest}");
}

#[test]
fn nothing_listening_exits_with_3() {
    let dir = tempfile::tempdir().unwrap();
    let output = Command::new(BRIDGE)
        .env("ASTER_MCP_SOCKET", dir.path().join("mcp.sock"))
        .stdin(Stdio::null())
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(3));
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert_eq!(stderr.lines().count(), 1, "{stderr}");
    assert!(stderr.contains("Aster Office is not running or MCP is off"));
    assert!(output.stdout.is_empty());
}

#[test]
fn a_socket_path_too_long_is_another_error() {
    let output = Command::new(BRIDGE)
        .env("ASTER_MCP_SOCKET", format!("/tmp/{}", "x".repeat(200)))
        .stdin(Stdio::null())
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&output.stderr).contains("at most"));
}
