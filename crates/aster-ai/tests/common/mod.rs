//! A mock HTTP server for the tests: it listens on 127.0.0.1, answers each
//! connection with the next canned reply (a streamed body sent in small
//! pieces, or a status with a body), and records the requests. No test
//! touches the network.

#![allow(dead_code)]

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use serde_json::{Value, json};

pub enum Reply {
    /// A 200 event stream, chunked, written piece by piece with a pause
    /// between pieces; the connection stays open for `hold` before the end.
    Stream {
        pieces: Vec<Vec<u8>>,
        pause: Duration,
        hold: Duration,
    },
    Status {
        status: u16,
        body: String,
    },
    /// A 302 to `location`.
    Redirect {
        location: String,
    },
}

impl Reply {
    pub fn stream(events: Vec<String>) -> Self {
        Reply::Stream {
            pieces: split(&events),
            pause: Duration::from_millis(1),
            hold: Duration::ZERO,
        }
    }

    pub fn json(body: Value) -> Self {
        Reply::Status {
            status: 200,
            body: body.to_string(),
        }
    }
}

/// Each event cut in three, so lines and characters arrive split.
fn split(events: &[String]) -> Vec<Vec<u8>> {
    let mut pieces = Vec::new();
    for event in events {
        let bytes = event.as_bytes();
        let third = (bytes.len() / 3).max(1);
        for piece in bytes.chunks(third) {
            pieces.push(piece.to_vec());
        }
    }
    pieces
}

#[derive(Clone, Debug)]
pub struct Recorded {
    pub method: String,
    pub path: String,
    pub headers: Vec<(String, String)>,
    pub body: String,
}

impl Recorded {
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(key, _)| key.eq_ignore_ascii_case(name))
            .map(|(_, value)| value.as_str())
    }

    pub fn json(&self) -> Value {
        serde_json::from_str(&self.body).expect("the request body is JSON")
    }
}

pub struct MockServer {
    pub base: String,
    recorded: Arc<Mutex<Vec<Recorded>>>,
}

impl MockServer {
    pub fn start(replies: Vec<Reply>) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let recorded = Arc::new(Mutex::new(Vec::new()));
        let log = recorded.clone();
        thread::spawn(move || {
            for reply in replies {
                let Ok((stream, _)) = listener.accept() else {
                    return;
                };
                if let Some(request) = read_request(&stream) {
                    log.lock().unwrap().push(request);
                }
                let _ = write_reply(stream, reply);
            }
        });
        Self { base, recorded }
    }

    pub fn requests(&self) -> Vec<Recorded> {
        self.recorded.lock().unwrap().clone()
    }
}

fn read_request(stream: &TcpStream) -> Option<Recorded> {
    let mut reader = BufReader::new(stream);
    let mut line = String::new();
    reader.read_line(&mut line).ok()?;
    let mut parts = line.split_whitespace();
    let method = parts.next()?.to_string();
    let path = parts.next()?.to_string();
    let mut headers = Vec::new();
    loop {
        let mut line = String::new();
        reader.read_line(&mut line).ok()?;
        let line = line.trim_end();
        if line.is_empty() {
            break;
        }
        let (name, value) = line.split_once(':')?;
        headers.push((name.trim().to_ascii_lowercase(), value.trim().to_string()));
    }
    let length: usize = headers
        .iter()
        .find(|(name, _)| name == "content-length")
        .and_then(|(_, value)| value.parse().ok())
        .unwrap_or(0);
    let mut body = vec![0; length];
    reader.read_exact(&mut body).ok()?;
    Some(Recorded {
        method,
        path,
        headers,
        body: String::from_utf8(body).ok()?,
    })
}

fn write_reply(mut stream: TcpStream, reply: Reply) -> std::io::Result<()> {
    match reply {
        Reply::Stream {
            pieces,
            pause,
            hold,
        } => {
            stream.write_all(
                b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n",
            )?;
            for piece in pieces {
                write!(stream, "{:x}\r\n", piece.len())?;
                stream.write_all(&piece)?;
                stream.write_all(b"\r\n")?;
                stream.flush()?;
                thread::sleep(pause);
            }
            thread::sleep(hold);
            stream.write_all(b"0\r\n\r\n")?;
        }
        Reply::Status { status, body } => {
            write!(
                stream,
                "HTTP/1.1 {status} Mock\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            )?;
        }
        Reply::Redirect { location } => {
            write!(
                stream,
                "HTTP/1.1 302 Found\r\nLocation: {location}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
            )?;
        }
    }
    stream.flush()
}

/// A block of a canned reply.
pub enum Block<'a> {
    Text(&'a str),
    /// id, name, the input as JSON text (possibly invalid).
    Tool(&'a str, &'a str, &'a str),
}

fn sse(name: &str, data: Value) -> String {
    format!("event: {name}\ndata: {data}\n\n")
}

/// Halves of a string, cut at a character boundary.
fn halves(text: &str) -> (&str, &str) {
    let mut middle = text.len() / 2;
    while !text.is_char_boundary(middle) {
        middle += 1;
    }
    text.split_at(middle)
}

/// An Anthropic Messages stream with these blocks and stop reason.
pub fn anthropic(blocks: &[Block], stop: &str) -> Vec<String> {
    let mut events = vec![sse(
        "message_start",
        json!({"type": "message_start", "message": {"id": "msg_1", "type": "message", "role": "assistant", "content": [], "stop_reason": null}}),
    )];
    for (index, block) in blocks.iter().enumerate() {
        match block {
            Block::Text(text) => {
                events.push(sse("content_block_start", json!({"type": "content_block_start", "index": index, "content_block": {"type": "text", "text": ""}})));
                let (a, b) = halves(text);
                for part in [a, b] {
                    events.push(sse("content_block_delta", json!({"type": "content_block_delta", "index": index, "delta": {"type": "text_delta", "text": part}})));
                }
            }
            Block::Tool(id, name, input) => {
                events.push(sse("content_block_start", json!({"type": "content_block_start", "index": index, "content_block": {"type": "tool_use", "id": id, "name": name, "input": {}}})));
                let (a, b) = halves(input);
                for part in [a, b] {
                    events.push(sse("content_block_delta", json!({"type": "content_block_delta", "index": index, "delta": {"type": "input_json_delta", "partial_json": part}})));
                }
            }
        }
        events.push(sse(
            "content_block_stop",
            json!({"type": "content_block_stop", "index": index}),
        ));
    }
    events.push(sse(
        "message_delta",
        json!({"type": "message_delta", "delta": {"stop_reason": stop}, "usage": {"output_tokens": 5}}),
    ));
    events.push(sse("message_stop", json!({"type": "message_stop"})));
    events
}

/// An OpenAI Chat Completions stream with these blocks and finish reason.
pub fn openai(blocks: &[Block], finish: &str) -> Vec<String> {
    let chunk = |delta: Value, finish: Value| {
        format!(
            "data: {}\n\n",
            json!({"id": "c1", "object": "chat.completion.chunk", "choices": [{"index": 0, "delta": delta, "finish_reason": finish}]})
        )
    };
    let mut events = vec![chunk(json!({"role": "assistant"}), Value::Null)];
    let mut call_index = 0;
    for block in blocks {
        match block {
            Block::Text(text) => {
                let (a, b) = halves(text);
                for part in [a, b] {
                    events.push(chunk(json!({"content": part}), Value::Null));
                }
            }
            Block::Tool(id, name, input) => {
                events.push(chunk(
                    json!({"tool_calls": [{"index": call_index, "id": id, "type": "function", "function": {"name": name, "arguments": ""}}]}),
                    Value::Null,
                ));
                let (a, b) = halves(input);
                for part in [a, b] {
                    events.push(chunk(
                        json!({"tool_calls": [{"index": call_index, "function": {"arguments": part}}]}),
                        Value::Null,
                    ));
                }
                call_index += 1;
            }
        }
    }
    events.push(chunk(json!({}), json!(finish)));
    events.push("data: [DONE]\n\n".to_string());
    events
}
