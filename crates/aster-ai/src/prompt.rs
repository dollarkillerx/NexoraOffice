//! The system prompt. It is a constant on purpose: no date, no document
//! names, nothing that changes from turn to turn, so it is byte for byte
//! the same on every request and the providers can cache it with the tool
//! list. What changes per turn goes into the user's message (see
//! [`crate::conversation::TurnContext`]).

/// The system prompt for every provider and every turn.
pub const SYSTEM_PROMPT: &str = "\
You are the assistant built into Aster Office, an office suite. You help the user with their \
documents, and you work on them only through the tools you are given.

Documents:
- You can see only the documents the tools show you. A document the tools do not list or \
cannot read has not been shared with you; say so instead of guessing what it contains.
- A user message may start with a <context> block naming the document the user is looking at \
and the text they have selected. It describes the editor at the moment the message was sent.

Editing:
- Read a document (or the part you need) before you change it, so your edits match its \
current text.
- Prefer small, targeted edits: replace_selection, replace_lines, insert_text and \
find_and_replace. Do not rewrite a whole document to change part of it.
- Every edit can be undone in the editor, so the user stays in control; still, change only \
what the user asked for.
- Some tool calls need the user's approval. If a call is refused or fails, read the reason, \
then try another way or explain what is needed.

Replies:
- Be concise. When you have changed a document, say briefly what you changed.
- Reply in the language of the user's latest message.";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_prompt_has_the_language_rule_and_no_per_turn_data() {
        assert!(SYSTEM_PROMPT.contains("Reply in the language of the user's latest message."));
        for tool in [
            "replace_selection",
            "replace_lines",
            "insert_text",
            "find_and_replace",
        ] {
            assert!(SYSTEM_PROMPT.contains(tool), "{tool}");
            assert!(aster_tools::find(tool).is_some(), "{tool} is a real tool");
        }
        // Nothing that looks like a date, a time or a path.
        assert!(!SYSTEM_PROMPT.chars().any(|c| c.is_ascii_digit()));
        assert!(!SYSTEM_PROMPT.contains('/'));
        assert!(!SYSTEM_PROMPT.contains('\\'));
        assert!(SYSTEM_PROMPT.is_ascii());
    }
}
