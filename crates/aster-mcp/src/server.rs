//! The server: a listening socket, its connections, and stopping it all.
//!
//! Threads, none of them the interface's:
//! - one accept thread, which wakes every 100 ms to see whether the server
//!   is stopping;
//! - one reader thread per connection, which reads messages one line at a
//!   time and answers everything except tool calls itself;
//! - one thread per tool call, because a call can wait up to two minutes
//!   for the user and the connection must go on answering `ping` and
//!   accepting other calls meanwhile. Writes to the socket are serialised
//!   by a mutex, so replies may come back in any order but never mixed.
//!
//! A connection's session (its "allow for this session" answers) ends when
//! its reader and all of its calls have finished. `stop` closes every
//! connection at once but does not wait for calls still waiting on the
//! user: they finish, or time out, on their own, and their replies go
//! nowhere.

use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use aster_tools::{Invoker, ToolResult};

use crate::endpoint::{self, EndpointError};

/// The longest message accepted, in bytes. A longer one ends the
/// connection: it cannot be a sensible tool call, and reading it whole
/// would let a client fill memory.
pub const MAX_LINE_BYTES: usize = 8 * 1024 * 1024;
/// Tool calls one connection may have running at once.
pub const MAX_CALLS_PER_CONNECTION: usize = 8;
/// Connections open at once. More are closed as soon as they are accepted.
pub const MAX_CONNECTIONS: usize = 16;

/// How the shell configures the server.
#[derive(Clone, Debug, Default)]
pub struct McpConfig {
    /// Listen here instead of the default path. For tests; the bridge only
    /// finds a socket elsewhere through `ASTER_MCP_SOCKET`.
    pub socket_path: Option<PathBuf>,
}

/// A connected client, as the settings page lists it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct McpClient {
    /// Distinct for every connection while the server runs.
    pub connection: u64,
    /// The client's own name from `initialize`, cleaned; `local` if none.
    pub name: String,
    pub protocol_version: String,
}

/// What the interface hears about. Called on the server's threads, never
/// the interface's: hand the news over to the event loop and return.
pub trait McpEvents: Send + Sync {
    /// A client has introduced itself with `initialize`. Connections that
    /// never do are not reported.
    fn client_connected(&self, client: &McpClient) {
        let _ = client;
    }

    /// A client that was reported as connected has gone.
    fn client_disconnected(&self, connection: u64) {
        let _ = connection;
    }

    /// A tool call has finished, whatever its result.
    fn tool_called(&self, connection: u64, tool: &str, result: &ToolResult) {
        let _ = (connection, tool, result);
    }
}

/// Events nobody listens to.
pub struct NoEvents;

impl McpEvents for NoEvents {}

/// Why the server did not start.
#[derive(Debug)]
pub enum StartError {
    /// No usable socket path, or none on this system.
    Endpoint(EndpointError),
    /// The socket's directory could not be made, or is not private.
    Directory { path: PathBuf, problem: String },
    /// Another Aster Office is already listening on the socket.
    AlreadyRunning(PathBuf),
    /// Something that is not a socket is in the way; it is left alone.
    Occupied(PathBuf),
    /// Binding or preparing the socket failed.
    Socket {
        path: PathBuf,
        error: std::io::Error,
    },
}

impl fmt::Display for StartError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            StartError::Endpoint(error) => error.fmt(f),
            StartError::Directory { path, problem } => {
                write!(
                    f,
                    "the folder `{}` cannot hold the MCP socket: {problem}",
                    path.display()
                )
            }
            StartError::AlreadyRunning(path) => write!(
                f,
                "another Aster Office is already serving MCP at `{}`",
                path.display()
            ),
            StartError::Occupied(path) => write!(
                f,
                "`{}` exists and is not a socket; it was left alone",
                path.display()
            ),
            StartError::Socket { path, error } => {
                write!(f, "cannot listen at `{}`: {error}", path.display())
            }
        }
    }
}

impl std::error::Error for StartError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            StartError::Endpoint(error) => Some(error),
            StartError::Socket { error, .. } => Some(error),
            _ => None,
        }
    }
}

impl From<EndpointError> for StartError {
    fn from(error: EndpointError) -> Self {
        StartError::Endpoint(error)
    }
}

/// A running MCP server. Stops when dropped.
pub struct McpServer {
    path: PathBuf,
    #[cfg(unix)]
    running: Option<unix::Running>,
}

impl McpServer {
    /// Listen on the configured or default socket. Returns once the socket
    /// accepts connections; everything else happens on the server's threads.
    pub fn start(
        config: McpConfig,
        invoker: Arc<Invoker>,
        events: Arc<dyn McpEvents>,
    ) -> Result<McpServer, StartError> {
        let path = match config.socket_path {
            Some(path) => path,
            None => endpoint::default_socket_path()?,
        };
        endpoint::check_length(&path)?;
        #[cfg(unix)]
        {
            let running = unix::start(&path, invoker, events)?;
            Ok(McpServer {
                path,
                running: Some(running),
            })
        }
        #[cfg(not(unix))]
        {
            let _ = (path, invoker, events);
            Err(StartError::Endpoint(EndpointError::Unsupported))
        }
    }

    /// Where the server listens.
    pub fn socket_path(&self) -> &Path {
        &self.path
    }

    /// The clients connected now that have introduced themselves, oldest
    /// first.
    pub fn clients(&self) -> Vec<McpClient> {
        #[cfg(unix)]
        if let Some(running) = &self.running {
            return running.clients();
        }
        Vec::new()
    }

    /// Stop accepting, close every connection and remove the socket file.
    /// Returns within about two seconds, even with calls waiting for the
    /// user (those are left to end on their own). Doing it twice is fine.
    pub fn stop(&mut self) {
        #[cfg(unix)]
        if let Some(running) = self.running.take() {
            running.stop(&self.path);
        }
    }
}

impl Drop for McpServer {
    fn drop(&mut self) {
        self.stop();
    }
}

#[cfg(unix)]
mod unix {
    use std::collections::HashMap;
    use std::io::{self, BufRead, BufReader, Write};
    use std::net::Shutdown;
    use std::os::fd::AsRawFd;
    use std::os::unix::fs::{FileTypeExt, MetadataExt, PermissionsExt};
    use std::os::unix::net::{UnixListener, UnixStream};
    use std::path::Path;
    use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
    use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
    use std::thread::{self, JoinHandle};
    use std::time::{Duration, Instant};

    use aster_tools::{Invoker, Origin};
    use serde_json::Value;

    use super::{
        MAX_CALLS_PER_CONNECTION, MAX_CONNECTIONS, MAX_LINE_BYTES, McpClient, McpEvents, StartError,
    };
    use crate::endpoint;
    use crate::peer;
    use crate::protocol::{
        INVALID_REQUEST, Outcome, Session, TOO_BUSY, call_result, error_reply, reply,
    };

    /// How often the accept thread looks at the stop flag.
    const ACCEPT_TICK_MS: libc::c_int = 100;
    /// How long `stop` waits for readers to notice their socket closed.
    const STOP_WAIT: Duration = Duration::from_secs(2);
    /// A client that stops reading must not hold the write lock for ever.
    const WRITE_TIMEOUT: Duration = Duration::from_secs(30);

    pub(super) struct Running {
        shared: Arc<Shared>,
        accept: Option<JoinHandle<()>>,
        /// The socket file's device and inode, so that `stop` removes our
        /// socket and not one another instance has put there since.
        identity: (u64, u64),
    }

    /// What the accept thread and the readers share.
    struct Shared {
        invoker: Arc<Invoker>,
        events: Arc<dyn McpEvents>,
        stopping: AtomicBool,
        next_id: AtomicU64,
        connections: Mutex<HashMap<u64, Live>>,
    }

    /// A connection as the server keeps track of it, to close it on stop.
    struct Live {
        stream: UnixStream,
        client: Option<McpClient>,
        reader: Option<JoinHandle<()>>,
    }

    /// Poisoning only means another thread panicked mid-update; the maps
    /// and streams behind these locks are still usable.
    fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
        mutex.lock().unwrap_or_else(PoisonError::into_inner)
    }

    pub(super) fn start(
        path: &Path,
        invoker: Arc<Invoker>,
        events: Arc<dyn McpEvents>,
    ) -> Result<Running, StartError> {
        let socket_error = |error| StartError::Socket {
            path: path.to_owned(),
            error,
        };
        let dir = path
            .parent()
            .filter(|dir| !dir.as_os_str().is_empty())
            .ok_or_else(|| StartError::Directory {
                path: path.to_owned(),
                problem: "the socket path has no folder".to_owned(),
            })?;
        endpoint::prepare_directory(dir).map_err(|problem| StartError::Directory {
            path: dir.to_owned(),
            problem,
        })?;

        // A socket left behind by a crash is removed; one that answers
        // belongs to a running instance and is not ours to take.
        match std::fs::symlink_metadata(path) {
            Ok(metadata) if metadata.file_type().is_socket() => {
                if UnixStream::connect(path).is_ok() {
                    return Err(StartError::AlreadyRunning(path.to_owned()));
                }
                std::fs::remove_file(path).map_err(socket_error)?;
            }
            Ok(_) => return Err(StartError::Occupied(path.to_owned())),
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(socket_error(error)),
        }

        let listener = UnixListener::bind(path).map_err(socket_error)?;
        // The directory is private already; the file's own mode is the
        // second lock. (Between bind and here it has the umask's mode, but
        // nobody else can reach into the directory.)
        let prepared = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
            .and_then(|()| std::fs::symlink_metadata(path))
            .and_then(|metadata| listener.set_nonblocking(true).map(|()| metadata));
        let metadata = match prepared {
            Ok(metadata) => metadata,
            Err(error) => {
                let _ = std::fs::remove_file(path);
                return Err(socket_error(error));
            }
        };

        let shared = Arc::new(Shared {
            invoker,
            events,
            stopping: AtomicBool::new(false),
            next_id: AtomicU64::new(1),
            connections: Mutex::new(HashMap::new()),
        });
        let accepting = Arc::clone(&shared);
        let accept = thread::Builder::new()
            .name("aster-mcp-accept".to_owned())
            .spawn(move || accept_loop(&listener, &accepting));
        let accept = match accept {
            Ok(accept) => accept,
            Err(error) => {
                let _ = std::fs::remove_file(path);
                return Err(socket_error(error));
            }
        };
        Ok(Running {
            shared,
            accept: Some(accept),
            identity: (metadata.dev(), metadata.ino()),
        })
    }

    impl Running {
        pub(super) fn clients(&self) -> Vec<McpClient> {
            let mut clients: Vec<McpClient> = lock(&self.shared.connections)
                .values()
                .filter_map(|live| live.client.clone())
                .collect();
            clients.sort_by_key(|client| client.connection);
            clients
        }

        pub(super) fn stop(mut self, path: &Path) {
            self.shared.stopping.store(true, Ordering::SeqCst);
            // The accept thread ends within one tick and drops the listener,
            // so nothing new can be accepted after this.
            if let Some(accept) = self.accept.take() {
                let _ = accept.join();
            }
            let live: Vec<Live> = lock(&self.shared.connections)
                .drain()
                .map(|(_, live)| live)
                .collect();
            // Shutting a socket down wakes its reader and fails pending
            // writes, whichever descriptor of it they use.
            for connection in &live {
                let _ = connection.stream.shutdown(Shutdown::Both);
            }
            let deadline = Instant::now() + STOP_WAIT;
            for reader in live.into_iter().filter_map(|live| live.reader) {
                while !reader.is_finished() && Instant::now() < deadline {
                    thread::sleep(Duration::from_millis(5));
                }
                if reader.is_finished() {
                    let _ = reader.join();
                }
            }
            if let Ok(metadata) = std::fs::symlink_metadata(path)
                && (metadata.dev(), metadata.ino()) == self.identity
            {
                let _ = std::fs::remove_file(path);
            }
        }
    }

    fn accept_loop(listener: &UnixListener, shared: &Arc<Shared>) {
        while !shared.stopping.load(Ordering::SeqCst) {
            if !readable(listener) {
                continue;
            }
            match listener.accept() {
                Ok((stream, _)) => admit(shared, stream),
                Err(error)
                    if matches!(
                        error.kind(),
                        io::ErrorKind::WouldBlock | io::ErrorKind::Interrupted
                    ) => {}
                // Out of descriptors and the like: wait instead of spinning.
                Err(_) => thread::sleep(Duration::from_millis(100)),
            }
        }
    }

    /// Wait up to one tick for a connection to accept.
    fn readable(listener: &UnixListener) -> bool {
        let mut poll = libc::pollfd {
            fd: listener.as_raw_fd(),
            events: libc::POLLIN,
            revents: 0,
        };
        // SAFETY: one valid pollfd, and the descriptor belongs to
        // `listener`, which outlives the call.
        unsafe { libc::poll(&raw mut poll, 1, ACCEPT_TICK_MS) > 0 }
    }

    fn admit(shared: &Arc<Shared>, stream: UnixStream) {
        // Only this user's own programs may connect. Dropping the stream
        // closes it.
        if peer::peer_uid(&stream) != Some(endpoint::own_uid()) {
            return;
        }
        // An accepted socket inherits the listener's non-blocking mode on
        // macOS but not on Linux; the reader wants it blocking everywhere.
        if stream.set_nonblocking(false).is_err() {
            return;
        }
        let _ = stream.set_write_timeout(Some(WRITE_TIMEOUT));
        let (Ok(writer), Ok(registered)) = (stream.try_clone(), stream.try_clone()) else {
            return;
        };
        let id = shared.next_id.fetch_add(1, Ordering::Relaxed);
        {
            let mut connections = lock(&shared.connections);
            if connections.len() >= MAX_CONNECTIONS {
                return;
            }
            connections.insert(
                id,
                Live {
                    stream: registered,
                    client: None,
                    reader: None,
                },
            );
        }
        let connection = Arc::new(Connection {
            id,
            session: format!("mcp-{id}"),
            writer: Mutex::new(writer),
            invoker: Arc::clone(&shared.invoker),
            events: Arc::clone(&shared.events),
            calls: Mutex::new(HashMap::new()),
        });
        let reading = Arc::clone(shared);
        let spawned = thread::Builder::new()
            .name(format!("aster-mcp-{id}"))
            .spawn(move || read_loop(stream, &connection, &reading));
        let mut connections = lock(&shared.connections);
        match spawned {
            // The reader may have finished and removed itself already.
            Ok(reader) => {
                if let Some(live) = connections.get_mut(&id) {
                    live.reader = Some(reader);
                }
            }
            Err(_) => {
                connections.remove(&id);
            }
        }
    }

    /// One connection, shared by its reader and its calls.
    struct Connection {
        id: u64,
        /// The invoker's session key: "allow for this session" lasts as long
        /// as this connection.
        session: String,
        writer: Mutex<UnixStream>,
        invoker: Arc<Invoker>,
        events: Arc<dyn McpEvents>,
        /// Calls running, by request id as JSON text; `true` once the client
        /// has cancelled one.
        calls: Mutex<HashMap<String, bool>>,
    }

    impl Drop for Connection {
        // The last of the reader and the calls is gone: the session is over.
        fn drop(&mut self) {
            self.invoker.end_session(&self.session);
        }
    }

    impl Connection {
        /// Write one message. On failure the socket is shut down, which
        /// also ends the reader.
        fn send(&self, message: &Value) -> bool {
            let mut bytes = message.to_string().into_bytes();
            bytes.push(b'\n');
            let mut writer = lock(&self.writer);
            let sent = writer.write_all(&bytes).and_then(|()| writer.flush());
            if sent.is_err() {
                let _ = writer.shutdown(Shutdown::Both);
            }
            sent.is_ok()
        }

        fn shut_down(&self) {
            let _ = lock(&self.writer).shutdown(Shutdown::Both);
        }

        /// The call goes on (the user may be looking at its question), but
        /// its reply is not sent, as MCP asks of a cancelled request.
        fn cancel(&self, id: &Value) {
            if let Some(cancelled) = lock(&self.calls).get_mut(&id.to_string()) {
                *cancelled = true;
            }
        }

        fn start_call(
            self: &Arc<Self>,
            id: Value,
            client: String,
            name: String,
            args: Value,
        ) -> bool {
            let key = id.to_string();
            {
                let mut calls = lock(&self.calls);
                if calls.contains_key(&key) {
                    drop(calls);
                    return self.send(&error_reply(
                        id,
                        INVALID_REQUEST,
                        "a request with this id is still running",
                    ));
                }
                if calls.len() >= MAX_CALLS_PER_CONNECTION {
                    drop(calls);
                    return self.send(&error_reply(
                        id,
                        TOO_BUSY,
                        "too many tool calls at once on this connection; wait for one to finish",
                    ));
                }
                calls.insert(key.clone(), false);
            }
            let connection = Arc::clone(self);
            let (call_key, call_id) = (key.clone(), id.clone());
            let spawned = thread::Builder::new()
                .name(format!("aster-mcp-{}-call", self.id))
                .spawn(move || connection.run_call(&call_key, call_id, &client, &name, &args));
            if spawned.is_err() {
                lock(&self.calls).remove(&key);
                return self.send(&error_reply(id, TOO_BUSY, "cannot start the call now"));
            }
            true
        }

        fn run_call(&self, key: &str, id: Value, client: &str, name: &str, args: &Value) {
            let origin = Origin::new(format!("mcp:{client}"), self.session.clone());
            let result = self.invoker.call(&origin, name, args);
            self.events.tool_called(self.id, name, &result);
            let cancelled = lock(&self.calls).remove(key).unwrap_or(false);
            if !cancelled {
                self.send(&reply(id, call_result(&result)));
            }
        }
    }

    fn read_loop(stream: UnixStream, connection: &Arc<Connection>, shared: &Shared) {
        let mut reader = BufReader::with_capacity(64 * 1024, stream);
        let mut session = Session::new();
        let mut line = Vec::new();
        let mut connected = false;
        loop {
            match read_line(&mut reader, &mut line, MAX_LINE_BYTES) {
                Ok(Line::Text) => {}
                // End of input ends the session. The socket stays open for
                // the replies of calls still running, in case the client
                // only closed its writing side.
                Ok(Line::End) | Err(_) => break,
                Ok(Line::TooLong) => {
                    connection.send(&error_reply(
                        Value::Null,
                        INVALID_REQUEST,
                        "message longer than 8 MiB; closing the connection",
                    ));
                    connection.shut_down();
                    break;
                }
            }
            let message = line.trim_ascii();
            if message.is_empty() {
                continue;
            }
            let sent = match session.handle(message) {
                Outcome::Nothing => true,
                Outcome::Reply(message) => connection.send(&message),
                Outcome::Cancel(id) => {
                    connection.cancel(&id);
                    true
                }
                Outcome::Initialized {
                    reply,
                    client,
                    protocol_version,
                } => {
                    let client = McpClient {
                        connection: connection.id,
                        name: client,
                        protocol_version,
                    };
                    if let Some(live) = lock(&shared.connections).get_mut(&connection.id) {
                        live.client = Some(client.clone());
                    }
                    let sent = connection.send(&reply);
                    shared.events.client_connected(&client);
                    connected = true;
                    sent
                }
                Outcome::Call {
                    id,
                    client,
                    name,
                    arguments,
                } => connection.start_call(id, client, name, arguments),
            };
            if !sent {
                break;
            }
        }
        // Dropping the registered copy lets the socket close once the
        // running calls (if any) are done with it.
        lock(&shared.connections).remove(&connection.id);
        if connected {
            shared.events.client_disconnected(connection.id);
        }
    }

    #[derive(Debug, PartialEq, Eq)]
    enum Line {
        Text,
        End,
        TooLong,
    }

    /// Read up to and including the next line break, but never more than
    /// `max` bytes before it.
    fn read_line(reader: &mut impl BufRead, line: &mut Vec<u8>, max: usize) -> io::Result<Line> {
        line.clear();
        loop {
            let available = match reader.fill_buf() {
                Ok(available) => available,
                Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
                Err(error) => return Err(error),
            };
            if available.is_empty() {
                // A last message without its line break still counts.
                return Ok(if line.is_empty() {
                    Line::End
                } else {
                    Line::Text
                });
            }
            let (used, complete) = match available.iter().position(|byte| *byte == b'\n') {
                Some(at) => (at + 1, true),
                None => (available.len(), false),
            };
            let content = line.len() + used - usize::from(complete);
            if content > max {
                return Ok(Line::TooLong);
            }
            line.extend_from_slice(&available[..used]);
            reader.consume(used);
            if complete {
                return Ok(Line::Text);
            }
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn lines_are_read_whole_and_limited() {
            let input = b"{\"a\":1}\n\n12345678\nlast".as_slice();
            let mut reader = BufReader::with_capacity(3, input);
            let mut line = Vec::new();
            assert_eq!(read_line(&mut reader, &mut line, 8).unwrap(), Line::Text);
            assert_eq!(line, b"{\"a\":1}\n");
            assert_eq!(read_line(&mut reader, &mut line, 8).unwrap(), Line::Text);
            assert_eq!(line, b"\n");
            assert_eq!(read_line(&mut reader, &mut line, 8).unwrap(), Line::Text);
            assert_eq!(line, b"12345678\n");
            assert_eq!(read_line(&mut reader, &mut line, 8).unwrap(), Line::Text);
            assert_eq!(line, b"last");
            assert_eq!(read_line(&mut reader, &mut line, 8).unwrap(), Line::End);
            let mut reader = BufReader::with_capacity(3, b"123456789\n".as_slice());
            assert_eq!(read_line(&mut reader, &mut line, 8).unwrap(), Line::TooLong);
        }
    }
}
