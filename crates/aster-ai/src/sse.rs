//! Server-sent events, as both providers stream them: lines of `event:`
//! and `data:` fields, an empty line ending each event. Bytes arrive in
//! whatever pieces the network makes, so a line, or a character of it, can
//! be split across chunks; the parser keeps the unfinished line until the
//! rest comes.
//!
//! The unfinished line and the event's data never grow past
//! [`MAX_EVENT_BYTES`]: a server that sends more without ending them ends
//! the stream with [`AiError::TooLarge`] instead of filling the memory.

use crate::error::AiError;
use crate::limits::MAX_EVENT_BYTES;

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct Event {
    /// The `event:` field, empty when the server sent none (OpenAI).
    pub name: String,
    /// The `data:` lines, joined by newlines.
    pub data: String,
}

#[derive(Default)]
pub(crate) struct Parser {
    /// Bytes of a line whose end has not arrived yet.
    line: Vec<u8>,
    name: String,
    data: String,
    has_data: bool,
}

impl Parser {
    /// Feed the next chunk; returns the events it completed.
    pub(crate) fn feed(&mut self, bytes: &[u8]) -> Result<Vec<Event>, AiError> {
        let mut events = Vec::new();
        for &byte in bytes {
            if byte == b'\n' {
                let mut line = std::mem::take(&mut self.line);
                if line.last() == Some(&b'\r') {
                    line.pop();
                }
                if let Some(event) = self.line_done(&String::from_utf8_lossy(&line))? {
                    events.push(event);
                }
            } else {
                // Checked before the push, so the buffer never holds more
                // than the limit.
                if self.line.len() >= MAX_EVENT_BYTES {
                    return Err(too_long());
                }
                self.line.push(byte);
            }
        }
        Ok(events)
    }

    /// The event still open when the stream ended without its empty line.
    pub(crate) fn finish(&mut self) -> Result<Option<Event>, AiError> {
        if !self.line.is_empty() {
            let line = std::mem::take(&mut self.line);
            let _ = self.line_done(&String::from_utf8_lossy(&line))?;
        }
        Ok(self.dispatch())
    }

    fn line_done(&mut self, line: &str) -> Result<Option<Event>, AiError> {
        if line.is_empty() {
            return Ok(self.dispatch());
        }
        if line.starts_with(':') {
            // A comment, used as a keep-alive.
            return Ok(None);
        }
        let (field, value) = match line.split_once(':') {
            Some((field, value)) => (field, value.strip_prefix(' ').unwrap_or(value)),
            None => (line, ""),
        };
        match field {
            "event" => self.name = value.to_string(),
            "data" => {
                // Many short `data:` lines add up to one event's data, so
                // the line limit alone does not bound it.
                if self.data.len() + 1 + value.len() > MAX_EVENT_BYTES {
                    return Err(too_long());
                }
                if self.has_data {
                    self.data.push('\n');
                }
                self.data.push_str(value);
                self.has_data = true;
            }
            _ => {}
        }
        Ok(None)
    }

    fn dispatch(&mut self) -> Option<Event> {
        let name = std::mem::take(&mut self.name);
        let data = std::mem::take(&mut self.data);
        if !std::mem::take(&mut self.has_data) {
            return None;
        }
        Some(Event { name, data })
    }
}

fn too_long() -> AiError {
    AiError::TooLarge(format!(
        "an event of the stream is longer than {} MiB",
        MAX_EVENT_BYTES / (1024 * 1024)
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn all(chunks: &[&[u8]]) -> Vec<Event> {
        let mut parser = Parser::default();
        let mut events = Vec::new();
        for chunk in chunks {
            events.extend(parser.feed(chunk).unwrap());
        }
        events.extend(parser.finish().unwrap());
        events
    }

    #[test]
    fn events_are_split_at_empty_lines() {
        let events =
            all(&[b"event: a\ndata: {\"x\":1}\n\n: ping\n\ndata: one\r\ndata: two\r\n\r\n"]);
        assert_eq!(
            events,
            vec![
                Event {
                    name: "a".into(),
                    data: "{\"x\":1}".into()
                },
                Event {
                    name: String::new(),
                    data: "one\ntwo".into()
                },
            ]
        );
    }

    #[test]
    fn any_split_gives_the_same_events() {
        let text = "event: delta\ndata: {\"text\":\"日本語のテキスト\"}\n\ndata: [DONE]\n\n";
        let whole = all(&[text.as_bytes()]);
        let bytes = text.as_bytes();
        for cut in 0..bytes.len() {
            assert_eq!(all(&[&bytes[..cut], &bytes[cut..]]), whole, "cut at {cut}");
        }
        let singles: Vec<&[u8]> = bytes.chunks(1).collect();
        assert_eq!(all(&singles), whole);
    }

    #[test]
    fn a_last_event_without_its_empty_line_still_counts() {
        assert_eq!(all(&[b"data: end"]).len(), 1);
    }

    #[test]
    fn an_endless_line_is_refused_without_growing_past_the_limit() {
        let mut parser = Parser::default();
        let chunk = vec![b'x'; 64 * 1024];
        let mut result = Ok(Vec::new());
        for _ in 0..(MAX_EVENT_BYTES / chunk.len() + 1) {
            result = parser.feed(&chunk);
            assert!(parser.line.len() <= MAX_EVENT_BYTES);
            if result.is_err() {
                break;
            }
        }
        assert_eq!(result.unwrap_err().kind(), "too_large");

        // A line of exactly the limit is still fine.
        let mut parser = Parser::default();
        let mut line = b"data: ".to_vec();
        line.resize(MAX_EVENT_BYTES, b'y');
        line.extend_from_slice(b"\n\n");
        let events = parser.feed(&line).unwrap();
        assert_eq!(events[0].data.len(), MAX_EVENT_BYTES - 6);
    }

    #[test]
    fn many_data_lines_cannot_add_up_past_the_limit() {
        let mut parser = Parser::default();
        let line = format!("data: {}\n", "z".repeat(1000));
        let mut failed = None;
        for _ in 0..(MAX_EVENT_BYTES / 1000 + 2) {
            if let Err(error) = parser.feed(line.as_bytes()) {
                failed = Some(error);
                break;
            }
            assert!(parser.data.len() <= MAX_EVENT_BYTES);
        }
        assert_eq!(failed.unwrap().kind(), "too_large");
    }

    #[test]
    fn an_unended_last_line_past_the_limit_is_refused() {
        let mut parser = Parser::default();
        let mut line = b"data: ".to_vec();
        line.resize(MAX_EVENT_BYTES + 10, b'q');
        assert!(parser.feed(&line).is_err());
    }
}
