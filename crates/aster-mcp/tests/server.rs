//! A real server on a socket in a temporary folder, with a fake application
//! behind the invoker, driven the way an MCP client would.
#![cfg(unix)]

use std::io::{BufRead, BufReader, Write};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use aster_mcp::protocol::{
    INVALID_REQUEST, METHOD_NOT_FOUND, NOT_INITIALIZED, PARSE_ERROR, PROTOCOL_VERSIONS, TOO_BUSY,
};
use aster_mcp::{
    EndpointError, MAX_CALLS_PER_CONNECTION, MAX_LINE_BYTES, McpClient, McpConfig, McpEvents,
    McpServer, StartError,
};
use aster_tools::{
    Approval, ApprovalRequest, Approver, Audit, DocumentInfo, Host, Invoker, Level, Policy,
    ToolResult, ToolSpec,
};
use serde_json::{Value, json};

struct FakeHost;

impl Host for FakeHost {
    fn documents(&self) -> Vec<DocumentInfo> {
        vec![
            DocumentInfo {
                id: "1".into(),
                title: "notes.md".into(),
                path: Some("/notes/notes.md".into()),
                kind: "markdown".into(),
                modified: false,
            },
            DocumentInfo {
                id: "2".into(),
                title: "secret.md".into(),
                path: Some("/private/secret.md".into()),
                kind: "markdown".into(),
                modified: false,
            },
        ]
    }

    fn run(&self, tool: &ToolSpec, _: Option<&DocumentInfo>, _: &Value) -> Result<String, String> {
        match tool.name {
            "read_document" => Ok("1: hello".to_owned()),
            _ => Ok("done".to_owned()),
        }
    }
}

/// Tells the test when the user is asked, and answers when the test says.
struct GateApprover {
    entered: Mutex<Sender<u64>>,
    release: Mutex<Receiver<Approval>>,
}

impl Approver for GateApprover {
    fn ask(&self, request: ApprovalRequest, timeout: Duration) -> Approval {
        let _ = self.entered.lock().unwrap().send(request.id);
        self.release
            .lock()
            .unwrap()
            .recv_timeout(timeout)
            .unwrap_or(Approval::TimedOut)
    }
}

#[derive(Default)]
struct Recorder {
    events: Mutex<Vec<String>>,
    /// Connection ids are unique in the process, and tests run side by side:
    /// events name connections by the order this server saw them, from 1.
    ids: Mutex<Vec<u64>>,
}

impl Recorder {
    fn local(&self, connection: u64) -> usize {
        let mut ids = self.ids.lock().unwrap();
        match ids.iter().position(|id| *id == connection) {
            Some(index) => index + 1,
            None => {
                ids.push(connection);
                ids.len()
            }
        }
    }

    fn events(&self) -> Vec<String> {
        self.events.lock().unwrap().clone()
    }

    /// Wait for an event, since they come from the server's threads.
    fn wait_for(&self, event: &str) {
        let deadline = Instant::now() + Duration::from_secs(5);
        while !self.events().iter().any(|e| e == event) {
            assert!(
                Instant::now() < deadline,
                "no `{event}` in {:?}",
                self.events()
            );
            std::thread::sleep(Duration::from_millis(5));
        }
    }
}

impl McpEvents for Recorder {
    fn client_connected(&self, client: &McpClient) {
        self.events.lock().unwrap().push(format!(
            "connected {} {}",
            self.local(client.connection),
            client.name
        ));
    }

    fn client_disconnected(&self, connection: u64) {
        self.events
            .lock()
            .unwrap()
            .push(format!("disconnected {}", self.local(connection)));
    }

    fn tool_called(&self, connection: u64, tool: &str, result: &ToolResult) {
        self.events.lock().unwrap().push(format!(
            "called {} {tool} {}",
            self.local(connection),
            result.is_error
        ));
    }
}

struct Fixture {
    dir: tempfile::TempDir,
    path: PathBuf,
    server: McpServer,
    invoker: Arc<Invoker>,
    entered: Receiver<u64>,
    release: Sender<Approval>,
    events: Arc<Recorder>,
}

fn invoker() -> (Arc<Invoker>, Receiver<u64>, Sender<Approval>) {
    let (entered_tx, entered) = mpsc::channel();
    let (release, release_rx) = mpsc::channel();
    let approver = Arc::new(GateApprover {
        entered: Mutex::new(entered_tx),
        release: Mutex::new(release_rx),
    });
    let mut policy = Policy::default();
    policy.set_folder("/notes".into(), Level::Ask);
    let invoker = Invoker::new(
        Arc::new(Mutex::new(policy)),
        Arc::new(FakeHost),
        approver,
        Arc::new(Audit::in_memory()),
    )
    .with_timeout(Duration::from_secs(30));
    (Arc::new(invoker), entered, release)
}

fn fixture() -> Fixture {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("run/mcp.sock");
    let (invoker, entered, release) = invoker();
    let events = Arc::new(Recorder::default());
    let server = McpServer::start(
        McpConfig {
            socket_path: Some(path.clone()),
        },
        invoker.clone(),
        events.clone(),
    )
    .unwrap();
    Fixture {
        dir,
        path,
        server,
        invoker,
        entered,
        release,
        events,
    }
}

struct Client {
    reader: BufReader<UnixStream>,
    writer: UnixStream,
}

impl Client {
    fn connect(path: &Path) -> Client {
        let stream = UnixStream::connect(path).unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(10)))
            .unwrap();
        Client {
            writer: stream.try_clone().unwrap(),
            reader: BufReader::new(stream),
        }
    }

    fn send_raw(&mut self, bytes: &[u8]) {
        self.writer.write_all(bytes).unwrap();
    }

    fn send(&mut self, message: Value) {
        self.send_raw(format!("{message}\n").as_bytes());
    }

    /// The next message, or `None` at the end of the connection.
    fn try_recv(&mut self) -> Option<Value> {
        let mut line = String::new();
        let read = self.reader.read_line(&mut line).unwrap_or(0);
        (read > 0).then(|| serde_json::from_str(&line).unwrap())
    }

    fn recv(&mut self) -> Value {
        self.try_recv().expect("the server closed the connection")
    }

    fn request(&mut self, id: u64, method: &str, params: Value) -> Value {
        self.send(json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params}));
        let answer = self.recv();
        assert_eq!(answer["id"], id, "{answer}");
        answer
    }

    fn initialize(&mut self, name: &str) -> Value {
        let answer = self.request(
            0,
            "initialize",
            json!({"protocolVersion": "2025-06-18", "capabilities": {},
                   "clientInfo": {"name": name, "version": "1"}}),
        );
        self.send(json!({"jsonrpc": "2.0", "method": "notifications/initialized"}));
        answer
    }

    fn call(&mut self, id: u64, tool: &str, arguments: Value) -> Value {
        self.request(
            id,
            "tools/call",
            json!({"name": tool, "arguments": arguments}),
        )["result"]
            .clone()
    }
}

#[test]
fn the_handshake_negotiates_the_version() {
    let fixture = fixture();
    let mut client = Client::connect(&fixture.path);
    // Ping works before initialize.
    assert_eq!(client.request(1, "ping", json!({}))["result"], json!({}));
    let answer = client.request(
        2,
        "initialize",
        json!({"protocolVersion": "2025-03-26", "clientInfo": {"name": "claude-code"}}),
    );
    let result = &answer["result"];
    assert_eq!(result["protocolVersion"], "2025-03-26");
    assert_eq!(
        result["capabilities"],
        json!({"tools": {"listChanged": false}})
    );
    assert_eq!(result["serverInfo"]["name"], "aster-office");
    assert_eq!(result["serverInfo"]["title"], "Aster Office");
    assert_eq!(result["serverInfo"]["version"], env!("CARGO_PKG_VERSION"));
    assert!(
        result["instructions"]
            .as_str()
            .unwrap()
            .contains("list_documents")
    );
    fixture.events.wait_for("connected 1 claude-code");

    // A version we do not know gets our latest.
    let mut other = Client::connect(&fixture.path);
    let answer = other.request(
        1,
        "initialize",
        json!({"protocolVersion": "2099-01-01", "clientInfo": {"name": "next"}}),
    );
    assert_eq!(answer["result"]["protocolVersion"], PROTOCOL_VERSIONS[0]);
}

#[test]
fn the_tool_list_is_the_catalogue() {
    let fixture = fixture();
    let mut client = Client::connect(&fixture.path);
    client.initialize("t");
    let answer = client.request(1, "tools/list", json!({}));
    let tools = answer["result"]["tools"].as_array().unwrap();
    assert_eq!(tools.len(), aster_tools::tools().len());
    for (entry, tool) in tools.iter().zip(aster_tools::tools()) {
        assert_eq!(entry["name"], tool.name);
        assert_eq!(entry["title"], tool.title);
        assert_eq!(entry["description"], tool.description);
        assert_eq!(entry["inputSchema"], tool.schema());
        let hints = &entry["annotations"];
        assert_eq!(hints["readOnlyHint"], tool.annotations.read_only);
        assert_eq!(hints["destructiveHint"], tool.annotations.destructive);
        assert_eq!(hints["idempotentHint"], tool.annotations.idempotent);
        assert_eq!(hints["openWorldHint"], tool.annotations.open_world);
    }
    assert!(answer["result"].get("nextCursor").is_none());
}

#[test]
fn tool_calls_succeed_or_fail_as_tool_results() {
    let fixture = fixture();
    let mut client = Client::connect(&fixture.path);
    client.initialize("t");
    let listed = client.call(1, "list_documents", json!({}));
    assert_eq!(listed["isError"], false);
    let text = listed["content"][0]["text"].as_str().unwrap();
    assert!(text.contains("notes.md") && !text.contains("secret"));
    let read = client.call(2, "read_document", json!({"document": "1"}));
    assert_eq!(
        read,
        json!({"content": [{"type": "text", "text": "1: hello"}], "isError": false})
    );
    // A document outside the policy does not exist for the client.
    let hidden = client.call(3, "read_document", json!({"document": "2"}));
    assert_eq!(hidden["isError"], true);
    assert!(
        hidden["content"][0]["text"]
            .as_str()
            .unwrap()
            .contains("no open document `2`")
    );
    // So is an unknown tool: a result the model can read, not a protocol
    // error.
    let unknown = client.call(4, "rm_rf", json!({}));
    assert_eq!(unknown["isError"], true);
    assert!(
        unknown["content"][0]["text"]
            .as_str()
            .unwrap()
            .contains("unknown tool")
    );
    fixture.events.wait_for("called 1 read_document false");
    fixture.events.wait_for("called 1 read_document true");
    // The calls went through the invoker's audit log, as `mcp:<client>`.
    let records = fixture.invoker.audit().recent();
    assert_eq!(records.len(), 4);
    assert!(records.iter().all(|r| r.caller == "mcp:t"));
}

#[test]
fn bad_messages_get_json_rpc_errors() {
    let fixture = fixture();
    let mut client = Client::connect(&fixture.path);
    // Before initialize.
    let early = client.request(1, "tools/list", json!({}));
    assert_eq!(early["error"]["code"], NOT_INITIALIZED);
    client.initialize("t");
    client.send_raw(b"{\"jsonrpc\": \"2.0\", \"id\": 2, \"method\"\n");
    let parse = client.recv();
    assert_eq!(parse["error"]["code"], PARSE_ERROR);
    assert_eq!(parse["id"], Value::Null);
    client.send_raw(b"[{\"jsonrpc\": \"2.0\", \"id\": 3, \"method\": \"ping\"}]\n");
    assert_eq!(client.recv()["error"]["code"], INVALID_REQUEST);
    let unknown = client.request(4, "resources/list", json!({}));
    assert_eq!(unknown["error"]["code"], METHOD_NOT_FOUND);
    // Notifications, known or not, and blank lines get no answer: the next
    // message is the ping's.
    client.send(json!({"jsonrpc": "2.0", "method": "notifications/whatever"}));
    client.send_raw(b"\r\n\n");
    assert_eq!(client.request(5, "ping", json!({}))["result"], json!({}));
}

#[test]
fn a_server_started_again_gives_new_connection_ids() {
    let mut first = fixture();
    let mut client = Client::connect(&first.path);
    client.initialize("one");
    first.events.wait_for("connected 1 one");
    let old = first.server.clients()[0].connection;
    first.server.stop();
    drop(client);
    let path = first.path.clone();
    let (invoker, _entered, _release) = invoker();
    let events = Arc::new(Recorder::default());
    let server = McpServer::start(
        McpConfig {
            socket_path: Some(path.clone()),
        },
        invoker,
        events.clone(),
    )
    .unwrap();
    let mut again = Client::connect(&path);
    again.initialize("two");
    events.wait_for("connected 1 two");
    assert!(server.clients()[0].connection > old);
}

#[test]
fn client_names_are_cleaned_and_listed() {
    let fixture = fixture();
    let mut client = Client::connect(&fixture.path);
    client.initialize("Bad\u{202e}<script>Name\u{7}");
    fixture.events.wait_for("connected 1 BadscriptName");
    assert_eq!(fixture.server.clients()[0].name, "BadscriptName");
    let mut anonymous = Client::connect(&fixture.path);
    anonymous.request(1, "initialize", json!({"protocolVersion": "2025-06-18"}));
    fixture.events.wait_for("connected 2 local");
}

#[test]
fn two_connections_work_at_once_and_leaving_is_reported() {
    let fixture = fixture();
    let mut first = Client::connect(&fixture.path);
    let mut second = Client::connect(&fixture.path);
    first.initialize("first");
    second.initialize("second");
    let names: Vec<String> = fixture
        .server
        .clients()
        .into_iter()
        .map(|c| c.name)
        .collect();
    assert_eq!(names, ["first", "second"]);
    first.send(json!({"jsonrpc": "2.0", "id": 7, "method": "ping"}));
    second.send(json!({"jsonrpc": "2.0", "id": 8, "method": "ping"}));
    assert_eq!(second.recv()["id"], 8);
    assert_eq!(first.recv()["id"], 7);
    drop(first);
    fixture.events.wait_for("disconnected 1");
    assert_eq!(fixture.server.clients().len(), 1);
}

#[test]
fn a_call_waiting_for_the_user_does_not_block_the_connection() {
    let fixture = fixture();
    let mut client = Client::connect(&fixture.path);
    client.initialize("t");
    client.send(json!({"jsonrpc": "2.0", "id": 1, "method": "tools/call",
                       "params": {"name": "insert_text", "arguments": {"document": "1", "text": "x"}}}));
    fixture
        .entered
        .recv_timeout(Duration::from_secs(5))
        .unwrap();
    // The user has not answered; the connection still does.
    assert_eq!(client.request(2, "ping", json!({}))["result"], json!({}));
    assert_eq!(
        client.call(3, "read_document", json!({"document": "1"}))["isError"],
        false
    );
    fixture.release.send(Approval::Allow).unwrap();
    let answer = client.recv();
    assert_eq!(answer["id"], 1);
    assert_eq!(answer["result"]["isError"], false);
}

#[test]
fn a_cancelled_call_gets_no_reply() {
    let fixture = fixture();
    let mut client = Client::connect(&fixture.path);
    client.initialize("t");
    client.send(json!({"jsonrpc": "2.0", "id": 1, "method": "tools/call",
                       "params": {"name": "insert_text", "arguments": {"document": "1", "text": "x"}}}));
    fixture
        .entered
        .recv_timeout(Duration::from_secs(5))
        .unwrap();
    client.send(
        json!({"jsonrpc": "2.0", "method": "notifications/cancelled",
                       "params": {"requestId": 1}}),
    );
    // Make sure the cancellation has been read before the call ends.
    assert_eq!(client.request(2, "ping", json!({}))["id"], 2);
    fixture.release.send(Approval::Allow).unwrap();
    fixture.events.wait_for("called 1 insert_text false");
    assert_eq!(client.request(3, "ping", json!({}))["id"], 3);
}

#[test]
fn calls_per_connection_are_limited() {
    let fixture = fixture();
    let mut client = Client::connect(&fixture.path);
    client.initialize("t");
    let arguments = json!({"document": "1", "text": "x"});
    for id in 1..=MAX_CALLS_PER_CONNECTION as u64 {
        client.send(json!({"jsonrpc": "2.0", "id": id, "method": "tools/call",
                           "params": {"name": "insert_text", "arguments": arguments}}));
    }
    fixture
        .entered
        .recv_timeout(Duration::from_secs(5))
        .unwrap();
    let refused = client.request(
        100,
        "tools/call",
        json!({"name": "insert_text", "arguments": arguments}),
    );
    assert_eq!(refused["error"]["code"], TOO_BUSY);
}

#[test]
fn an_overlong_message_closes_the_connection() {
    let fixture = fixture();
    let mut client = Client::connect(&fixture.path);
    client.initialize("t");
    let long = vec![b'a'; MAX_LINE_BYTES + 1024];
    // The server stops reading partway, so the end of this may not go out.
    let _ = client.writer.write_all(&long);
    let answer = client.recv();
    assert_eq!(answer["error"]["code"], INVALID_REQUEST);
    assert!(client.try_recv().is_none());
}

#[test]
fn stopping_is_quick_even_with_a_call_waiting() {
    let mut fixture = fixture();
    let mut client = Client::connect(&fixture.path);
    client.initialize("t");
    client.send(json!({"jsonrpc": "2.0", "id": 1, "method": "tools/call",
                       "params": {"name": "insert_text", "arguments": {"document": "1", "text": "x"}}}));
    fixture
        .entered
        .recv_timeout(Duration::from_secs(5))
        .unwrap();
    let started = Instant::now();
    fixture.server.stop();
    assert!(
        started.elapsed() < Duration::from_secs(3),
        "{:?}",
        started.elapsed()
    );
    assert!(!fixture.path.exists());
    assert!(client.try_recv().is_none());
    assert!(UnixStream::connect(&fixture.path).is_err());
    fixture.events.wait_for("disconnected 1");
    // Stopping again, and dropping, are harmless.
    fixture.server.stop();
}

#[test]
fn a_stale_socket_is_replaced_but_a_live_one_is_not() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("mcp.sock");
    // A crash leaves the file behind with nobody listening.
    drop(UnixListener::bind(&path).unwrap());
    assert!(path.exists());
    let config = McpConfig {
        socket_path: Some(path.clone()),
    };
    let (invoker, _entered, _release) = invoker();
    let events = Arc::new(Recorder::default());
    let server = McpServer::start(config.clone(), invoker.clone(), events.clone()).unwrap();
    let mut client = Client::connect(&path);
    assert_eq!(client.request(1, "ping", json!({}))["result"], json!({}));
    // A second instance must not take the socket of a running one.
    let second = McpServer::start(config.clone(), invoker.clone(), events.clone());
    assert!(matches!(second, Err(StartError::AlreadyRunning(_))));
    assert_eq!(client.request(2, "ping", json!({}))["result"], json!({}));
    drop(server);
    assert!(!path.exists());
    // Something that is not a socket is left alone.
    std::fs::write(&path, "keep me").unwrap();
    let occupied = McpServer::start(config, invoker, events);
    assert!(matches!(occupied, Err(StartError::Occupied(_))));
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "keep me");
}

#[test]
fn the_socket_and_its_folder_are_private() {
    let fixture = fixture();
    let socket = std::fs::metadata(&fixture.path)
        .unwrap()
        .permissions()
        .mode();
    assert_eq!(socket & 0o777, 0o600);
    let folder = std::fs::metadata(fixture.path.parent().unwrap())
        .unwrap()
        .permissions()
        .mode();
    assert_eq!(folder & 0o777, 0o700);
    assert!(fixture.dir.path().exists());
}

#[test]
fn bad_socket_paths_are_refused() {
    let dir = tempfile::tempdir().unwrap();
    let (invoker, _entered, _release) = invoker();
    let long = dir.path().join("x".repeat(120));
    let result = McpServer::start(
        McpConfig {
            socket_path: Some(long),
        },
        invoker.clone(),
        Arc::new(aster_mcp::NoEvents),
    );
    assert!(matches!(
        result,
        Err(StartError::Endpoint(EndpointError::TooLong { .. }))
    ));
    // A folder that is a link is not trusted.
    let real = dir.path().join("real");
    std::fs::create_dir(&real).unwrap();
    let link = dir.path().join("link");
    std::os::unix::fs::symlink(&real, &link).unwrap();
    let result = McpServer::start(
        McpConfig {
            socket_path: Some(link.join("mcp.sock")),
        },
        invoker,
        Arc::new(aster_mcp::NoEvents),
    );
    let Err(StartError::Directory { problem, .. }) = result else {
        panic!("a linked folder was accepted");
    };
    assert!(problem.contains("symbolic link"));
}
