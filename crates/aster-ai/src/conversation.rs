//! The conversation, the same for every provider: messages from the user
//! and from the assistant, each a list of content blocks. Each provider
//! turns it into its own wire format when it sends a request.
//!
//! The per-turn context (which document is open, what is selected) goes
//! into the user's message rather than the system prompt, so that the
//! system prompt and everything before the new message stay the same from
//! turn to turn and the providers can reuse their cached prefix.

use serde_json::Value;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Role {
    User,
    Assistant,
}

#[derive(Clone, Debug, PartialEq)]
pub enum ContentBlock {
    Text {
        text: String,
    },
    /// A tool call the model asked for.
    ToolUse {
        id: String,
        name: String,
        /// The arguments, always a JSON object. When the model's input was
        /// not a valid JSON object this is empty and `raw_input` holds what
        /// it sent; such a call is not run.
        input: Value,
        raw_input: Option<String>,
    },
    /// The answer to a [`ContentBlock::ToolUse`], in a user message.
    ToolResult {
        tool_use_id: String,
        text: String,
        is_error: bool,
    },
    /// A block only one provider understands, kept so it can be sent back
    /// unchanged: Anthropic's thinking blocks must be, for the model to
    /// keep its reasoning across tool calls. Other providers leave it out.
    Opaque {
        provider: String,
        block: Value,
    },
}

impl ContentBlock {
    pub fn text(text: impl Into<String>) -> Self {
        ContentBlock::Text { text: text.into() }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Message {
    pub role: Role,
    pub content: Vec<ContentBlock>,
}

impl Message {
    pub fn user(content: Vec<ContentBlock>) -> Self {
        Self {
            role: Role::User,
            content,
        }
    }

    pub fn assistant(content: Vec<ContentBlock>) -> Self {
        Self {
            role: Role::Assistant,
            content,
        }
    }

    /// The message's text blocks, joined; what the interface shows.
    pub fn text(&self) -> String {
        let mut text = String::new();
        for block in &self.content {
            if let ContentBlock::Text { text: part } = block {
                text.push_str(part);
            }
        }
        text
    }

    /// The tool calls in the message, in order.
    pub fn tool_uses(&self) -> impl Iterator<Item = &ContentBlock> {
        self.content
            .iter()
            .filter(|block| matches!(block, ContentBlock::ToolUse { .. }))
    }
}

/// What the user is looking at when they send a message. Every field is
/// optional; the shell fills in what it knows.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TurnContext {
    pub document_id: Option<String>,
    pub document_title: Option<String>,
    /// The selected text, if any.
    pub selection: Option<String>,
}

/// The most of a selection put in the context, in characters; the model can
/// read the rest with `get_selection`.
const MAX_SELECTION_CHARS: usize = 4000;

impl TurnContext {
    fn is_empty(&self) -> bool {
        self.document_id.is_none()
            && self.document_title.is_none()
            && self.selection.as_deref().is_none_or(str::is_empty)
    }

    /// The `<context>…</context>` block, or `None` when there is nothing to
    /// say.
    pub fn block(&self) -> Option<String> {
        if self.is_empty() {
            return None;
        }
        let mut out = String::from("<context>\n");
        if let Some(id) = &self.document_id {
            out.push_str(&format!("Current document id: {}\n", one_line(id)));
        }
        if let Some(title) = &self.document_title {
            out.push_str(&format!("Current document title: {}\n", one_line(title)));
        }
        if let Some(selection) = self.selection.as_deref().filter(|s| !s.is_empty()) {
            let count = selection.chars().count();
            let shown: String = selection.chars().take(MAX_SELECTION_CHARS).collect();
            if count > MAX_SELECTION_CHARS {
                out.push_str(&format!(
                    "Selected text (the first {MAX_SELECTION_CHARS} of {count} characters; get_selection reads all of it):\n"
                ));
            } else {
                out.push_str("Selected text:\n");
            }
            // The selection is the user's document text; it must not be
            // able to end the block early.
            out.push_str(&shown.replace("</context>", "</ context>"));
            out.push('\n');
        }
        out.push_str("</context>");
        Some(out)
    }
}

fn one_line(text: &str) -> String {
    text.replace(['\n', '\r'], " ")
}

/// The user message for what the user typed: the context block first, when
/// there is one, as its own text block, then the typed text. The interface
/// shows only the last block.
pub fn user_message(text: &str, context: &TurnContext) -> Message {
    let mut content = Vec::new();
    if let Some(block) = context.block() {
        content.push(ContentBlock::text(block));
    }
    content.push(ContentBlock::text(text));
    Message::user(content)
}

/// The longest title, in characters.
const MAX_TITLE_CHARS: usize = 40;

/// A short title for a conversation from the user's first message: its
/// first non-empty line, cut at a word boundary where it can be, at most 40
/// characters. No model call.
pub fn title(first_message: &str) -> String {
    let line = first_message
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .unwrap_or("");
    let line = line.split_whitespace().collect::<Vec<_>>().join(" ");
    if line.chars().count() <= MAX_TITLE_CHARS {
        return line;
    }
    let cut: String = line.chars().take(MAX_TITLE_CHARS - 1).collect();
    // Cut at the last space when it keeps most of the text; CJK text has
    // no spaces and is cut anywhere.
    let cut = match cut.rfind(' ') {
        Some(space) if cut[..space].chars().count() >= MAX_TITLE_CHARS / 2 => &cut[..space],
        _ => cut.as_str(),
    };
    format!("{}…", cut.trim_end())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_context_goes_first_in_its_own_block() {
        let context = TurnContext {
            document_id: Some("3".into()),
            document_title: Some("Plan.md".into()),
            selection: Some("a </context> b".into()),
        };
        let message = user_message("Fix the typo", &context);
        assert_eq!(message.content.len(), 2);
        let ContentBlock::Text { text } = &message.content[0] else {
            panic!()
        };
        assert!(text.starts_with("<context>\nCurrent document id: 3\n"));
        assert!(text.contains("Current document title: Plan.md"));
        assert!(text.ends_with("a </ context> b\n</context>"));
        assert_eq!(message.content[1], ContentBlock::text("Fix the typo"));

        let bare = user_message("Hi", &TurnContext::default());
        assert_eq!(bare.content, vec![ContentBlock::text("Hi")]);
    }

    #[test]
    fn titles_are_short_first_lines() {
        assert_eq!(title("\n  Summarise   this\nsecond line"), "Summarise this");
        let long = "Please rewrite the introduction of the quarterly report so it reads better";
        let short = title(long);
        assert!(short.chars().count() <= MAX_TITLE_CHARS);
        assert_eq!(short, "Please rewrite the introduction of the…");
        let japanese = "この文書の誤字を直して、表現も少し柔らかくしてください。それから見出しも整えてください。";
        let short = title(japanese);
        assert_eq!(short.chars().count(), MAX_TITLE_CHARS);
        assert!(short.ends_with('…'));
        assert_eq!(title(""), "");
    }
}
