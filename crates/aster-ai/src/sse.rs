//! Server-sent events, as both providers stream them: lines of `event:`
//! and `data:` fields, an empty line ending each event. Bytes arrive in
//! whatever pieces the network makes, so a line, or a character of it, can
//! be split across chunks; the parser keeps the unfinished line until the
//! rest comes.

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
    pub(crate) fn feed(&mut self, bytes: &[u8]) -> Vec<Event> {
        let mut events = Vec::new();
        for &byte in bytes {
            if byte == b'\n' {
                let mut line = std::mem::take(&mut self.line);
                if line.last() == Some(&b'\r') {
                    line.pop();
                }
                if let Some(event) = self.line_done(&String::from_utf8_lossy(&line)) {
                    events.push(event);
                }
            } else {
                self.line.push(byte);
            }
        }
        events
    }

    /// The event still open when the stream ended without its empty line.
    pub(crate) fn finish(&mut self) -> Option<Event> {
        if !self.line.is_empty() {
            let line = std::mem::take(&mut self.line);
            let _ = self.line_done(&String::from_utf8_lossy(&line));
        }
        self.dispatch()
    }

    fn line_done(&mut self, line: &str) -> Option<Event> {
        if line.is_empty() {
            return self.dispatch();
        }
        if line.starts_with(':') {
            // A comment, used as a keep-alive.
            return None;
        }
        let (field, value) = match line.split_once(':') {
            Some((field, value)) => (field, value.strip_prefix(' ').unwrap_or(value)),
            None => (line, ""),
        };
        match field {
            "event" => self.name = value.to_string(),
            "data" => {
                if self.has_data {
                    self.data.push('\n');
                }
                self.data.push_str(value);
                self.has_data = true;
            }
            _ => {}
        }
        None
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

#[cfg(test)]
mod tests {
    use super::*;

    fn all(chunks: &[&[u8]]) -> Vec<Event> {
        let mut parser = Parser::default();
        let mut events = Vec::new();
        for chunk in chunks {
            events.extend(parser.feed(chunk));
        }
        events.extend(parser.finish());
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
}
