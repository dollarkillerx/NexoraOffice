//! The document tools of `aster-tools`, carried out on an open Markdown
//! document. The policy, approvals and audit have already happened by the
//! time these run (see `aster_tools::Invoker`); this is only the doing.
//!
//! Everything here runs on the interface thread, on the editor itself, so
//! a change the assistant makes is an ordinary edit: one undo step, in the
//! recovery record, highlighted and previewed like the user's own typing.
//!
//! Lines are numbered from 1, as models and people count them. Columns are
//! counted in characters.

use std::ops::Range;

use aster_markdown::search::{Finder, Query};
use aster_markdown::structure;
use aster_markdown::{Rope, Selection};
use serde_json::{Value, json};

use crate::content::MarkdownEditor;

/// How many lines `read_document` gives when not told.
const DEFAULT_LINES: usize = 400;
/// The most lines one `read_document` call gives.
const MAX_LINES: usize = 2000;
/// The most lines `search_document` lists.
const MAX_FOUND_LINES: usize = 200;
/// A found line longer than this is shortened around the match.
const FOUND_LINE_CHARS: usize = 300;

/// Run a document tool on `editor`. The arguments have been checked against
/// the tool's schema.
pub fn run(editor: &mut MarkdownEditor, tool: &str, args: &Value) -> Result<String, String> {
    match tool {
        "read_document" => Ok(read(editor.document.text(), args)),
        "get_selection" => Ok(selection(editor)),
        "search_document" => search(editor.document.text(), args),
        "get_outline" => Ok(outline(editor.document.text())),
        "replace_selection" => {
            let range = editor.document.selection().range();
            let text = string(args, "text");
            change(editor, range, text, "replaced the selection")
        }
        "insert_text" => {
            let at = insert_position(editor, args)?;
            change(editor, at..at, string(args, "text"), "inserted")
        }
        "replace_lines" => {
            let range = line_range(editor.document.text(), args)?;
            change(editor, range, string(args, "text"), "replaced the lines")
        }
        "find_and_replace" => find_and_replace(editor, args),
        other => Err(format!("`{other}` does not work on this kind of document")),
    }
}

/// The tools that only read, on a Word document: its text is a line per
/// paragraph, `selected` what is selected, `headings` its outline. The tools
/// that change a document refuse: the assistant cannot edit Word documents
/// yet.
pub fn run_on_word(
    text: &str,
    selected: &str,
    headings: Vec<Value>,
    tool: &str,
    args: &Value,
) -> Result<String, String> {
    let rope = Rope::from_str(text);
    match tool {
        "read_document" => Ok(read(&rope, args)),
        "search_document" => search(&rope, args),
        "get_selection" => Ok(json!({ "text": selected.chars().take(20_000).collect::<String>() })
            .to_string()),
        "get_outline" => Ok(json!({ "headings": headings }).to_string()),
        other => Err(format!(
            "`{other}` does not work on a Word document: the assistant can read Word documents but not change them yet"
        )),
    }
}

fn string<'a>(args: &'a Value, name: &str) -> &'a str {
    args[name].as_str().unwrap_or_default()
}

fn number(args: &Value, name: &str) -> Option<usize> {
    args[name].as_u64().map(|n| n as usize)
}

/// The number of lines, as an editor shows them: a final line break starts
/// an empty last line.
fn line_count(text: &Rope) -> usize {
    text.len_lines()
}

/// A line's text without its line break.
fn line_text(text: &Rope, line: usize) -> String {
    let start = text.line_to_byte(line);
    let end = if line + 1 < text.len_lines() {
        text.line_to_byte(line + 1) - 1
    } else {
        text.len_bytes()
    };
    text.byte_slice(start..end).to_string()
}

/// 1-based line and column (in characters) of a byte offset.
fn position(text: &Rope, offset: usize) -> (usize, usize) {
    let line = text.byte_to_line(offset);
    let start = text.line_to_byte(line);
    let column = text.byte_slice(start..offset).chars().count();
    (line + 1, column + 1)
}

fn read(text: &Rope, args: &Value) -> String {
    let total = line_count(text);
    let first = number(args, "start_line")
        .unwrap_or(1)
        .clamp(1, total.max(1));
    let count = number(args, "max_lines")
        .unwrap_or(DEFAULT_LINES)
        .clamp(1, MAX_LINES);
    let last = (first + count - 1).min(total);
    let mut out = format!("[lines {first}-{last} of {total}]\n");
    for line in first..=last {
        out.push_str(&format!("{line}: {}\n", line_text(text, line - 1)));
    }
    if last < total {
        out.push_str(&format!(
            "[{} more lines; read on with start_line {}]\n",
            total - last,
            last + 1
        ));
    }
    out
}

fn selection(editor: &MarkdownEditor) -> String {
    let text = editor.document.text();
    let range = editor.document.selection().range();
    let selected: String = text
        .byte_slice(range.clone())
        .chars()
        .take(20_000)
        .collect();
    let (start_line, start_column) = position(text, range.start);
    let (end_line, end_column) = position(text, range.end);
    json!({
        "text": selected,
        "start": { "line": start_line, "column": start_column },
        "end": { "line": end_line, "column": end_column },
    })
    .to_string()
}

fn finder(args: &Value, pattern: &str, case_default: bool) -> Result<Finder, String> {
    let query = Query {
        pattern: pattern.to_owned(),
        case_sensitive: args["case_sensitive"].as_bool().unwrap_or(case_default),
        whole_word: false,
        regex: args["regex"].as_bool().unwrap_or(false),
    };
    Finder::new(&query)
        .map_err(|error| format!("the regular expression does not compile: {error}"))?
        .ok_or_else(|| "nothing to look for".to_owned())
}

fn search(text: &Rope, args: &Value) -> Result<String, String> {
    let finder = finder(args, string(args, "query"), false)?;
    let whole = text.to_string();
    let found = finder
        .find_all(&whole, &std::sync::atomic::AtomicBool::new(false), |_| {})
        .unwrap_or_default();
    let mut lines: Vec<usize> = found
        .matches
        .iter()
        .map(|m| text.byte_to_line(m.start))
        .collect();
    lines.dedup();
    let mut out = format!(
        "[{} matches on {} lines]\n",
        found.matches.len(),
        lines.len()
    );
    for line in lines.iter().take(MAX_FOUND_LINES) {
        let shown: String = line_text(text, *line)
            .chars()
            .take(FOUND_LINE_CHARS)
            .collect();
        out.push_str(&format!("{}: {shown}\n", line + 1));
    }
    if lines.len() > MAX_FOUND_LINES {
        out.push_str(&format!(
            "[{} more lines not listed]\n",
            lines.len() - MAX_FOUND_LINES
        ));
    }
    Ok(out)
}

fn outline(text: &Rope) -> String {
    let headings: Vec<Value> = structure::headings(&text.to_string())
        .into_iter()
        .map(|h| json!({ "level": h.level, "text": h.text, "line": h.line }))
        .collect();
    json!({ "headings": headings }).to_string()
}

fn insert_position(editor: &MarkdownEditor, args: &Value) -> Result<usize, String> {
    let text = editor.document.text();
    match args["position"].as_str().unwrap_or("cursor") {
        "start" => Ok(0),
        "end" => Ok(text.len_bytes()),
        "line" => {
            let line = number(args, "line").ok_or("position \"line\" needs `line`")?;
            let total = line_count(text);
            if line == 0 || line > total + 1 {
                return Err(format!(
                    "line {line} is outside the document (1 to {total})"
                ));
            }
            Ok(if line > total {
                text.len_bytes()
            } else {
                text.line_to_byte(line - 1)
            })
        }
        _ => Ok(editor.document.selection().focus),
    }
}

/// The bytes of lines `start_line` to `end_line`, without the last line's
/// break.
fn line_range(text: &Rope, args: &Value) -> Result<Range<usize>, String> {
    let total = line_count(text);
    let (Some(first), Some(last)) = (number(args, "start_line"), number(args, "end_line")) else {
        return Err("start_line and end_line are needed".to_owned());
    };
    if first == 0 || last < first || last > total {
        return Err(format!(
            "lines {first} to {last} are not in the document (1 to {total})"
        ));
    }
    let start = text.line_to_byte(first - 1);
    let end = if last < total {
        text.line_to_byte(last) - 1
    } else {
        text.len_bytes()
    };
    Ok(start..end)
}

fn change(
    editor: &mut MarkdownEditor,
    range: Range<usize>,
    text: &str,
    what: &str,
) -> Result<String, String> {
    let caret = range.start + text.len();
    editor
        .apply_external(range.clone(), text, Selection::caret(caret))
        .map_err(str::to_owned)?;
    let (line, _) = position(editor.document.text(), range.start);
    Ok(json!({ "ok": true, "did": what, "line": line, "chars": text.chars().count() }).to_string())
}

fn find_and_replace(editor: &mut MarkdownEditor, args: &Value) -> Result<String, String> {
    let finder = finder(args, string(args, "find"), true)?;
    let whole = editor.document.text().to_string();
    let never = std::sync::atomic::AtomicBool::new(false);
    let found = finder.find_all(&whole, &never, |_| {}).unwrap_or_default();
    let count = if args["all"].as_bool().unwrap_or(true) {
        found.matches.len()
    } else {
        found.matches.len().min(1)
    };
    let Some((range, replaced)) = finder.replace_all(
        &whole,
        &found.matches[..count],
        string(args, "replace"),
        &never,
    ) else {
        return Ok(json!({ "replaced": 0 }).to_string());
    };
    drop(whole);
    let caret = range.start + replaced.len();
    editor
        .apply_external(range, &replaced, Selection::caret(caret))
        .map_err(str::to_owned)?;
    Ok(json!({ "replaced": count }).to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::theme::{self, ThemeMode};
    use aster_markdown::TextFormat;

    fn editor(text: &str) -> MarkdownEditor {
        let tokens = theme::Tokens::of(ThemeMode::Light);
        let mut editor = MarkdownEditor::new(text, TextFormat::default(), None, None, &tokens);
        editor.surface.resize(1200, 600, 2.0);
        editor
    }

    fn text_of(editor: &MarkdownEditor) -> String {
        editor.document.text().to_string()
    }

    const NOTE: &str = "# 会议纪要\n\n## 议题\n\n- 预算\n- 人员\n\n结论：通过。";

    #[test]
    fn reading_gives_numbered_lines_in_parts() {
        let mut e = editor(NOTE);
        let all = run(&mut e, "read_document", &json!({"document": "1"})).unwrap();
        assert!(all.starts_with("[lines 1-8 of 8]\n1: # 会议纪要\n2: \n3: ## 议题"));
        let part = run(
            &mut e,
            "read_document",
            &json!({"document": "1", "start_line": 5, "max_lines": 2}),
        )
        .unwrap();
        assert_eq!(
            part,
            "[lines 5-6 of 8]\n5: - 预算\n6: - 人员\n[2 more lines; read on with start_line 7]\n"
        );
    }

    #[test]
    fn selection_outline_and_search() {
        let mut e = editor(NOTE);
        let start = NOTE.find("预算").unwrap();
        e.document.set_selection(Selection {
            anchor: start,
            focus: start + "预算".len(),
        });
        let selected: Value =
            serde_json::from_str(&run(&mut e, "get_selection", &json!({})).unwrap()).unwrap();
        assert_eq!(selected["text"], "预算");
        assert_eq!(selected["start"], json!({"line": 5, "column": 3}));
        let outline: Value =
            serde_json::from_str(&run(&mut e, "get_outline", &json!({})).unwrap()).unwrap();
        assert_eq!(
            outline["headings"][1],
            json!({"level": 2, "text": "议题", "line": 3})
        );
        let found = run(&mut e, "search_document", &json!({"query": "议"})).unwrap();
        assert_eq!(found, "[2 matches on 2 lines]\n1: # 会议纪要\n3: ## 议题\n");
        let bad = run(
            &mut e,
            "search_document",
            &json!({"query": "(", "regex": true}),
        );
        assert!(bad.unwrap_err().contains("does not compile"));
    }

    #[test]
    fn edits_are_one_undo_step_each() {
        let mut e = editor(NOTE);
        run(
            &mut e,
            "insert_text",
            &json!({"text": "- 场地\n", "position": "line", "line": 7}),
        )
        .unwrap();
        assert!(text_of(&e).contains("- 人员\n- 场地\n\n结论"));
        run(
            &mut e,
            "replace_lines",
            &json!({"start_line": 1, "end_line": 1, "text": "# 周会纪要"}),
        )
        .unwrap();
        assert!(text_of(&e).starts_with("# 周会纪要\n"));
        run(
            &mut e,
            "insert_text",
            &json!({"text": "\n附件：无", "position": "end"}),
        )
        .unwrap();
        assert!(text_of(&e).ends_with("结论：通过。\n附件：无"));
        e.undo();
        e.undo();
        e.undo();
        assert_eq!(text_of(&e), NOTE);
        // The selection is replaced where the user left it.
        let start = NOTE.find("通过").unwrap();
        e.document.set_selection(Selection {
            anchor: start,
            focus: start + "通过".len(),
        });
        run(&mut e, "replace_selection", &json!({"text": "暂缓"})).unwrap();
        assert!(text_of(&e).ends_with("结论：暂缓。"));
        assert!(
            run(
                &mut e,
                "replace_lines",
                &json!({"start_line": 3, "end_line": 99, "text": ""})
            )
            .is_err()
        );
    }

    #[test]
    fn find_and_replace_with_groups_counts_and_undoes_at_once() {
        let mut e = editor("2026-11-23 与 2025-01-02，2024-12-31");
        let result = run(
            &mut e,
            "find_and_replace",
            &json!({"find": r"(\d{4})-(\d\d)-(\d\d)", "replace": "$1年$2月$3日", "regex": true}),
        )
        .unwrap();
        assert_eq!(result, r#"{"replaced":3}"#);
        assert_eq!(
            text_of(&e),
            "2026年11月23日 与 2025年01月02日，2024年12月31日"
        );
        e.undo();
        assert_eq!(text_of(&e), "2026-11-23 与 2025-01-02，2024-12-31");
        let once = run(
            &mut e,
            "find_and_replace",
            &json!({"find": "20", "replace": "X", "all": false}),
        );
        assert_eq!(once.unwrap(), r#"{"replaced":1}"#);
        assert!(text_of(&e).starts_with("X26-11-23"));
        let none = run(
            &mut e,
            "find_and_replace",
            &json!({"find": "不存在", "replace": "x"}),
        );
        assert_eq!(none.unwrap(), r#"{"replaced":0}"#);
    }
}
