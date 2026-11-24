//! `aster-mcp`: the stdio bridge for MCP clients that start a server as a
//! child process. It connects to the socket of the running Aster Office
//! and copies bytes both ways, stdin to the socket and the socket to
//! stdout, without reading them: the application does all of MCP.
//!
//! The socket is the one [`aster_mcp::endpoint`] names, or the path in
//! `ASTER_MCP_SOCKET`. Exit codes: 0 when the application closes the
//! connection, 3 when there is nothing to connect to (Aster Office is not
//! running, or its MCP server is off), 1 for any other failure. It does
//! not start Aster Office itself.

use std::process::ExitCode;

/// Nothing is listening: Aster Office is not running or MCP is off.
#[cfg(unix)]
const NOT_RUNNING: u8 = 3;

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err((code, message)) => {
            eprintln!("aster-mcp: {message}");
            ExitCode::from(code)
        }
    }
}

#[cfg(unix)]
fn run() -> Result<(), (u8, String)> {
    use std::io::{self, Read, Write};
    use std::net::Shutdown;
    use std::path::PathBuf;

    use aster_mcp::endpoint;

    let path = match std::env::var_os(endpoint::SOCKET_ENV) {
        Some(path) if !path.is_empty() => PathBuf::from(path),
        _ => endpoint::default_socket_path().map_err(|error| (1, error.to_string()))?,
    };
    endpoint::check_length(&path).map_err(|error| (1, error.to_string()))?;
    let stream = endpoint::connect(&path).map_err(|error| {
        (
            NOT_RUNNING,
            format!(
                "Aster Office is not running or MCP is off (cannot connect to `{}`: {error})",
                path.display()
            ),
        )
    })?;
    let mut to_app = stream
        .try_clone()
        .map_err(|error| (1, format!("cannot use the socket: {error}")))?;

    // stdin to the socket. When stdin ends, closing the socket's writing
    // side tells the application the session is over, while replies still
    // on their way can be read below.
    std::thread::spawn(move || {
        let mut stdin = io::stdin().lock();
        let mut buffer = [0u8; 64 * 1024];
        loop {
            match stdin.read(&mut buffer) {
                Ok(0) => break,
                Ok(read) => {
                    if to_app.write_all(&buffer[..read]).is_err() {
                        break;
                    }
                }
                Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
                Err(_) => break,
            }
        }
        let _ = to_app.shutdown(Shutdown::Write);
    });

    // The socket to stdout, flushed after every read so that a message is
    // never left sitting in a buffer while the client waits for it.
    let mut from_app = stream;
    let mut stdout = io::stdout().lock();
    let mut buffer = [0u8; 64 * 1024];
    loop {
        let read = match from_app.read(&mut buffer) {
            Ok(0) => return Ok(()),
            Ok(read) => read,
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            Err(error) => return Err((1, format!("reading from Aster Office failed: {error}"))),
        };
        stdout
            .write_all(&buffer[..read])
            .and_then(|()| stdout.flush())
            .map_err(|error| (1, format!("writing to stdout failed: {error}")))?;
    }
}

#[cfg(not(unix))]
fn run() -> Result<(), (u8, String)> {
    Err((1, aster_mcp::EndpointError::Unsupported.to_string()))
}
