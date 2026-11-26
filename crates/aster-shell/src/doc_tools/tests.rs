use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use aster_tools::{
    Approval, ApprovalRequest, Approver, Audit, DocumentInfo, Host, Invoker, Level, Origin, Policy,
    ToolSpec,
};

use super::*;
use crate::i18n::Language;
use crate::theme::{ThemeMode, Tokens};

use crate::writer_tab::docx_sample;

fn blank() -> DocTab {
    let mut tab = DocTab::blank(&Tokens::of(ThemeMode::Light)).expect("the blank document");
    tab.view.resize(900, 700, 1.0);
    tab
}

fn call(tab: &mut DocTab, tool: &str, args: Value) -> Result<Value, String> {
    run(tab, tool, &args).map(|(text, _)| serde_json::from_str(&text).expect("a JSON answer"))
}

fn ok(tab: &mut DocTab, tool: &str, args: Value) -> Value {
    match call(tab, tool, args.clone()) {
        Ok(value) => value,
        Err(error) => panic!("{tool} {args} failed: {error}"),
    }
}

fn fails(tab: &mut DocTab, tool: &str, args: Value) -> String {
    match call(tab, tool, args.clone()) {
        Ok(value) => panic!("{tool} {args} should fail, gave {value}"),
        Err(error) => error,
    }
}

/// Six blocks: a heading, a paragraph, two bullets, a table, a closing
/// paragraph.
fn sample() -> DocTab {
    let mut tab = blank();
    ok(
        &mut tab,
        "replace_paragraphs",
        json!({
            "block": 1,
            "text": "Report\nThe budget is ready for review.\nTea\nCoffee\nClosing remarks"
        }),
    );
    ok(
        &mut tab,
        "format_text",
        json!({ "block": 1, "style": "heading 1" }),
    );
    ok(
        &mut tab,
        "format_text",
        json!({ "block": 3, "end_block": 4, "list": "bullet" }),
    );
    ok(
        &mut tab,
        "edit_table",
        json!({
            "action": "insert",
            "block": 4,
            "cells": [["Item", "Cost"], ["Tea", "3"], ["Cake", "4"]]
        }),
    );
    tab
}

fn texts(tab: &DocTab) -> Vec<String> {
    tab.view
        .document()
        .body
        .blocks
        .iter()
        .map(|block| match block {
            Block::Paragraph(p) => p.text(),
            Block::Table(_) => "<table>".to_owned(),
        })
        .collect()
}

fn paragraph(tab: &DocTab, block: usize) -> Paragraph {
    tab.view.document().body.blocks[block - 1]
        .as_paragraph()
        .expect("a paragraph")
        .clone()
}

/// Run `tool`, check that it is one step of undo that gives back exactly the
/// earlier document, and do it again.
fn is_one_undo_step(tab: &mut DocTab, tool: &str, args: Value) -> Value {
    let before = tab.view.document().clone();
    let depth = tab.view.can_undo();
    let revision = tab.view.revision();
    let answer = ok(tab, tool, args);
    assert_eq!(answer["changed"], true, "{tool} changed nothing");
    assert!(tab.view.revision() > revision);
    assert!(tab.is_modified());
    assert!(*tab.view.document() != before);
    assert!(tab.view.undo(), "{tool} left nothing to undo");
    assert!(
        *tab.view.document() == before,
        "{tool}: one undo does not give back the earlier document"
    );
    // Whatever was undone before this call is still there to undo: the
    // call was exactly one step.
    assert_eq!(tab.view.can_undo(), depth);
    assert!(tab.view.redo());
    answer
}

#[test]
fn reading_gives_block_numbers_styles_lists_and_table_cells() {
    let mut tab = sample();
    let read = ok(&mut tab, "read_word", json!({}));
    assert_eq!(read["total_blocks"], 6);
    assert_eq!(read["first"], 1);
    assert_eq!(read["last"], 6);
    let blocks = read["blocks"].as_array().unwrap();
    assert_eq!(blocks.len(), 6);
    assert_eq!(blocks[0]["block"], 1);
    assert_eq!(blocks[0]["type"], "paragraph");
    assert_eq!(blocks[0]["style"], "heading 1");
    assert_eq!(blocks[0]["text"], "Report");
    assert_eq!(blocks[1]["style"], "Normal");
    assert_eq!(blocks[2]["list"], "bullet");
    assert_eq!(blocks[3]["list"], "bullet");
    assert_eq!(blocks[4]["type"], "table");
    assert_eq!(blocks[4]["block"], 5);
    assert_eq!(blocks[4]["rows"], 3);
    assert_eq!(blocks[4]["columns"], 2);
    assert_eq!(
        blocks[4]["cells"],
        json!([["Item", "Cost"], ["Tea", "3"], ["Cake", "4"]])
    );
    assert_eq!(blocks[5]["text"], "Closing remarks");
    // The styles to name in the editing tools.
    let styles = read["styles"].as_array().unwrap();
    assert!(styles.contains(&json!("heading 1")));
    // Parts: blocks 3 and 4, then the rest.
    let part = ok(
        &mut tab,
        "read_word",
        json!({ "start_block": 3, "max_blocks": 2 }),
    );
    assert_eq!(part["first"], 3);
    assert_eq!(part["last"], 4);
    assert!(part["more"].as_str().unwrap().contains("start_block 5"));
    assert_eq!(part["blocks"][0]["text"], "Tea");
    // Rows of a table in parts.
    let rows = ok(
        &mut tab,
        "read_word",
        json!({ "start_block": 5, "max_blocks": 1, "start_row": 2, "max_rows": 1 }),
    );
    assert_eq!(rows["blocks"][0]["first_row"], 2);
    assert_eq!(rows["blocks"][0]["cells"], json!([["Tea", "3"]]));
    assert!(rows["blocks"][0]["more_rows"].is_string());
    // Out of range.
    let error = fails(&mut tab, "read_word", json!({ "start_block": 9 }));
    assert!(error.contains("blocks 1 to 6"), "{error}");
}

#[test]
fn a_long_document_is_read_in_parts_under_a_budget() {
    let mut tab = blank();
    let lines: Vec<String> = (1..=400)
        .map(|i| format!("Line {i}: {}", "word ".repeat(30)))
        .collect();
    ok(
        &mut tab,
        "replace_paragraphs",
        json!({ "block": 1, "text": lines.join("\n") }),
    );
    let read = ok(&mut tab, "read_word", json!({ "max_blocks": 300 }));
    let shown = read["blocks"].as_array().unwrap().len();
    assert!(shown > 20 && shown < 300, "{shown}");
    let more = read["more"].as_str().unwrap();
    assert!(
        more.contains(&format!("start_block {}", shown + 1)),
        "{more}"
    );
    // Reading on from there gives the next block.
    let next = ok(
        &mut tab,
        "read_word",
        json!({ "start_block": shown + 1, "max_blocks": 1 }),
    );
    assert_eq!(next["blocks"][0]["block"], shown + 1);
}

#[test]
fn the_outline_names_the_block_of_each_heading() {
    let mut tab = sample();
    let outline = ok(&mut tab, "get_outline", json!({}));
    assert_eq!(outline["headings"][0]["text"], "Report");
    assert_eq!(outline["headings"][0]["block"], 1);
    // The line tools still work on a Word document.
    let lines = run(&mut tab, "read_document", &json!({})).unwrap().0;
    assert!(lines.contains("Closing remarks"));
}

#[test]
fn replacing_paragraphs_keeps_the_formatting_and_is_one_undo_step() {
    let mut tab = sample();
    let answer = is_one_undo_step(
        &mut tab,
        "replace_paragraphs",
        json!({ "block": 2, "text": "The budget is approved.\nNext steps follow." }),
    );
    assert_eq!(answer["replaced_blocks"], 1);
    assert_eq!(answer["first_block"], 2);
    assert_eq!(answer["last_block"], 3);
    assert_eq!(answer["total_blocks"], 7);
    assert_eq!(texts(&tab)[1], "The budget is approved.");
    assert_eq!(texts(&tab)[2], "Next steps follow.");
    // The heading keeps its style when its words change.
    ok(
        &mut tab,
        "replace_paragraphs",
        json!({ "block": 1, "text": "Annual report" }),
    );
    assert_eq!(
        style_name(tab.view.document(), &paragraph(&tab, 1)),
        "heading 1"
    );
    // A range of paragraphs becomes one.
    ok(
        &mut tab,
        "replace_paragraphs",
        json!({ "block": 3, "end_block": 4, "text": "Merged" }),
    );
    assert_eq!(texts(&tab)[2], "Merged");
    assert_eq!(texts(&tab)[3], "Coffee");
}

#[test]
fn replacing_paragraphs_gives_clear_errors() {
    let mut tab = sample();
    let before = tab.view.document().clone();
    let error = fails(
        &mut tab,
        "replace_paragraphs",
        json!({ "block": 7, "text": "x" }),
    );
    assert!(
        error.contains("block 7") && error.contains("blocks 1 to 6"),
        "{error}"
    );
    let error = fails(
        &mut tab,
        "replace_paragraphs",
        json!({ "block": 2, "end_block": 9, "text": "x" }),
    );
    assert!(error.contains("block 9"), "{error}");
    let error = fails(
        &mut tab,
        "replace_paragraphs",
        json!({ "block": 4, "end_block": 2, "text": "x" }),
    );
    assert!(error.contains("document order"), "{error}");
    let error = fails(
        &mut tab,
        "replace_paragraphs",
        json!({ "block": 0, "text": "x" }),
    );
    assert!(error.contains("from 1"), "{error}");
    // A table is not a paragraph.
    let error = fails(
        &mut tab,
        "replace_paragraphs",
        json!({ "block": 5, "text": "x" }),
    );
    assert!(
        error.contains("table") && error.contains("edit_table"),
        "{error}"
    );
    let error = fails(
        &mut tab,
        "replace_paragraphs",
        json!({ "block": 4, "end_block": 6, "text": "x" }),
    );
    assert!(error.contains("block 5 is a table"), "{error}");
    // Text limits.
    let long = "x".repeat(MAX_TEXT + 1);
    let error = fails(
        &mut tab,
        "replace_paragraphs",
        json!({ "block": 2, "text": long }),
    );
    assert!(error.contains("too long"), "{error}");
    let many = "a\n".repeat(MAX_LINES + 1);
    let error = fails(
        &mut tab,
        "replace_paragraphs",
        json!({ "block": 2, "text": many }),
    );
    assert!(error.contains("paragraphs"), "{error}");
    let error = fails(
        &mut tab,
        "replace_paragraphs",
        json!({ "block": 2, "text": "a\u{FFFC}b" }),
    );
    assert!(error.contains("U+FFFC"), "{error}");
    // Nothing changed, and nothing to undo: each failure is no step.
    assert!(*tab.view.document() == before);
    assert!(!tab.is_modified() || tab.view.can_undo());
}

#[test]
fn paragraphs_go_in_before_and_after_a_block_with_a_style() {
    let mut tab = sample();
    let answer = is_one_undo_step(
        &mut tab,
        "insert_paragraphs",
        json!({ "block": 2, "text": "First point\nSecond point" }),
    );
    assert_eq!(answer["first_block"], 3);
    assert_eq!(answer["last_block"], 4);
    assert_eq!(texts(&tab)[2], "First point");
    assert_eq!(texts(&tab)[3], "Second point");
    // Before the first block, with a style.
    ok(
        &mut tab,
        "insert_paragraphs",
        json!({ "block": 1, "position": "before", "text": "Cover", "style": "Title" }),
    );
    assert_eq!(texts(&tab)[0], "Cover");
    assert_eq!(
        style_name(tab.view.document(), &paragraph(&tab, 1)),
        "Title"
    );
    assert_eq!(texts(&tab)[1], "Report");
    assert_eq!(
        style_name(tab.view.document(), &paragraph(&tab, 2)),
        "heading 1"
    );
    // After a heading the new paragraph is body text.
    ok(
        &mut tab,
        "insert_paragraphs",
        json!({ "block": 2, "text": "Under the heading" }),
    );
    assert_eq!(texts(&tab)[2], "Under the heading");
    assert_eq!(
        style_name(tab.view.document(), &paragraph(&tab, 3)),
        "Normal"
    );
    // Next to a table: after it, and before it.
    let table = texts(&tab).iter().position(|t| t == "<table>").unwrap() + 1;
    ok(
        &mut tab,
        "insert_paragraphs",
        json!({ "block": table, "text": "After the table" }),
    );
    assert_eq!(texts(&tab)[table], "After the table");
    ok(
        &mut tab,
        "insert_paragraphs",
        json!({ "block": table, "position": "before", "text": "Before the table" }),
    );
    assert_eq!(texts(&tab)[table - 1], "Before the table");
    assert_eq!(texts(&tab)[table], "<table>");
    // An empty text is an empty paragraph; the list continues.
    let list = texts(&tab).iter().position(|t| t == "Coffee").unwrap() + 1;
    ok(
        &mut tab,
        "insert_paragraphs",
        json!({ "block": list, "text": "Juice" }),
    );
    let read = ok(
        &mut tab,
        "read_word",
        json!({ "start_block": list + 1, "max_blocks": 1 }),
    );
    assert_eq!(read["blocks"][0]["text"], "Juice");
    assert_eq!(read["blocks"][0]["list"], "bullet");
    // Errors.
    let error = fails(
        &mut tab,
        "insert_paragraphs",
        json!({ "block": 99, "text": "x" }),
    );
    assert!(error.contains("outside the document"), "{error}");
    let error = fails(
        &mut tab,
        "insert_paragraphs",
        json!({ "block": 1, "text": "x", "style": "no such style" }),
    );
    assert!(
        error.contains("no paragraph style") && error.contains("heading 1"),
        "{error}"
    );
    let error = fails(
        &mut tab,
        "insert_paragraphs",
        json!({ "block": 1, "text": "x", "position": "middle" }),
    );
    assert!(error.contains("before"), "{error}");
}

#[test]
fn blocks_are_deleted_singly_in_ranges_and_as_tables() {
    let mut tab = sample();
    let answer = is_one_undo_step(&mut tab, "delete_blocks", json!({ "block": 2 }));
    assert_eq!(answer["deleted_blocks"], 1);
    assert_eq!(answer["total_blocks"], 5);
    assert_eq!(texts(&tab)[1], "Tea");
    // The table with a paragraph next to it.
    is_one_undo_step(&mut tab, "delete_blocks", json!({ "block": 4 }));
    assert!(!texts(&tab).contains(&"<table>".to_owned()));
    // A range.
    is_one_undo_step(
        &mut tab,
        "delete_blocks",
        json!({ "block": 2, "end_block": 3 }),
    );
    assert_eq!(texts(&tab), ["Report", "Closing remarks"]);
    // The last paragraph of a document is emptied, never removed.
    ok(
        &mut tab,
        "delete_blocks",
        json!({ "block": 1, "end_block": 2 }),
    );
    assert_eq!(texts(&tab), [""]);
    // The first block can go too.
    let mut tab = sample();
    ok(&mut tab, "delete_blocks", json!({ "block": 1 }));
    assert_eq!(texts(&tab)[0], "The budget is ready for review.");
    assert_eq!(
        style_name(tab.view.document(), &paragraph(&tab, 1)),
        "Normal"
    );
    let error = fails(&mut tab, "delete_blocks", json!({ "block": 8 }));
    assert!(error.contains("blocks 1 to 5"), "{error}");
}

#[test]
fn a_table_is_never_last_or_next_to_another_after_a_delete() {
    let mut tab = sample();
    ok(&mut tab, "delete_blocks", json!({ "block": 6 }));
    let blocks = &tab.view.document().body.blocks;
    assert!(blocks.last().unwrap().as_paragraph().is_some());
    // Two tables with a paragraph between them: deleting it keeps them apart.
    ok(
        &mut tab,
        "edit_table",
        json!({ "action": "insert", "block": 6, "cells": [["x"]] }),
    );
    let at = texts(&tab).iter().position(|t| t == "<table>").unwrap();
    let blocks = &tab.view.document().body.blocks;
    assert!(blocks[at].as_table().is_some());
    ok(&mut tab, "delete_blocks", json!({ "block": at + 2 }));
    let blocks = &tab.view.document().body.blocks;
    for pair in blocks.windows(2) {
        assert!(
            pair[0].as_table().is_none() || pair[1].as_table().is_none(),
            "two tables touch"
        );
    }
    assert!(blocks.last().unwrap().as_paragraph().is_some());
}

#[test]
fn find_and_replace_counts_limits_and_keeps_the_look() {
    let mut tab = blank();
    ok(
        &mut tab,
        "replace_paragraphs",
        json!({ "block": 1, "text": "Tea and tea and TEA\nNo match here\nTea again" }),
    );
    ok(
        &mut tab,
        "format_text",
        json!({ "block": 1, "phrase": "and", "bold": true }),
    );
    let answer = is_one_undo_step(
        &mut tab,
        "find_and_replace",
        json!({ "find": "tea", "replace": "coffee", "case_sensitive": false }),
    );
    assert_eq!(answer["replaced"], 4);
    assert_eq!(
        texts(&tab),
        [
            "coffee and coffee and coffee",
            "No match here",
            "coffee again"
        ]
    );
    // Case matters by default; a limit counts from the start.
    let mut tab = blank();
    ok(
        &mut tab,
        "replace_paragraphs",
        json!({ "block": 1, "text": "Tea tea Tea\nTea" }),
    );
    assert_eq!(
        ok(
            &mut tab,
            "find_and_replace",
            json!({ "find": "Tea", "replace": "Chai", "limit": 2 })
        )["replaced"],
        2
    );
    assert_eq!(texts(&tab), ["Chai tea Chai", "Tea"]);
    assert_eq!(
        ok(
            &mut tab,
            "find_and_replace",
            json!({ "find": "Tea", "replace": "x", "all": false })
        )["replaced"],
        1
    );
    assert_eq!(texts(&tab)[1], "x");
    // Nothing to replace is not an error and not a step.
    let steps = tab.view.can_undo();
    let revision = tab.view.revision();
    let answer = ok(
        &mut tab,
        "find_and_replace",
        json!({ "find": "zzz", "replace": "x" }),
    );
    assert_eq!(answer["replaced"], 0);
    assert_eq!(answer["changed"], false);
    assert_eq!(tab.view.revision(), revision);
    assert_eq!(tab.view.can_undo(), steps);
    // A replacement takes the look of what it replaces.
    let mut tab = blank();
    ok(
        &mut tab,
        "replace_paragraphs",
        json!({ "block": 1, "text": "plain bold plain" }),
    );
    ok(
        &mut tab,
        "format_text",
        json!({ "block": 1, "phrase": "bold", "bold": true, "color": "#C00000" }),
    );
    ok(
        &mut tab,
        "find_and_replace",
        json!({ "find": "bold", "replace": "heavy" }),
    );
    let p = paragraph(&tab, 1);
    let heavy = p.runs.iter().find(|r| r.text.contains("heavy")).unwrap();
    assert!(heavy.look.bold);
    assert_eq!(heavy.look.color, Some([0xC0, 0, 0, 0xFF]));
    assert!(!p.runs[0].look.bold);
}

#[test]
fn find_and_replace_reaches_table_cells_and_refuses_what_it_cannot_do() {
    let mut tab = sample();
    let answer = ok(
        &mut tab,
        "find_and_replace",
        json!({ "find": "Tea", "replace": "Green tea" }),
    );
    // Block 3 and the table's cell.
    assert_eq!(answer["replaced"], 2);
    let read = ok(
        &mut tab,
        "read_word",
        json!({ "start_block": 5, "max_blocks": 1 }),
    );
    assert_eq!(read["blocks"][0]["cells"][1][0], "Green tea");
    let error = fails(
        &mut tab,
        "find_and_replace",
        json!({ "find": "T.a", "replace": "x", "regex": true }),
    );
    assert!(error.contains("regular expressions"), "{error}");
    let error = fails(
        &mut tab,
        "find_and_replace",
        json!({ "find": "a", "replace": "x\ny" }),
    );
    assert!(error.contains("line breaks"), "{error}");
    let error = fails(
        &mut tab,
        "find_and_replace",
        json!({ "find": "", "replace": "x" }),
    );
    assert!(error.contains("empty"), "{error}");
    let error = fails(
        &mut tab,
        "find_and_replace",
        json!({ "find": "x".repeat(MAX_PHRASE + 1), "replace": "y" }),
    );
    assert!(error.contains("too long"), "{error}");
}

#[test]
fn pictures_and_notes_are_never_removed() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("sample.docx");
    std::fs::write(&path, docx_sample::sample()).unwrap();
    let crate::office::Loaded::Document(view, source) = crate::office::load(&path).unwrap() else {
        panic!("a document");
    };
    let mut tab = DocTab::new(*view, source, Some(path), &Tokens::of(ThemeMode::Light));
    tab.view.resize(900, 700, 1.0);
    let objects: Vec<usize> = tab
        .view
        .document()
        .body
        .blocks
        .iter()
        .enumerate()
        .filter(|(_, block)| objects_in(std::slice::from_ref(block)) > 0)
        .map(|(i, _)| i + 1)
        .collect();
    assert!(!objects.is_empty(), "the sample has a picture or a note");
    let first = objects[0];
    let before = tab.view.document().clone();
    let read = ok(
        &mut tab,
        "read_word",
        json!({ "start_block": first, "max_blocks": 1 }),
    );
    let block = &read["blocks"][0];
    assert!(block["objects"].as_u64().unwrap_or(0) > 0 || block["type"] == "table");
    for (tool, args) in [
        (
            "replace_paragraphs",
            json!({ "block": first, "text": "gone" }),
        ),
        ("delete_blocks", json!({ "block": first })),
    ] {
        let error = fails(&mut tab, tool, args);
        assert!(
            error.contains("picture") || error.contains("table"),
            "{tool}: {error}"
        );
    }
    assert!(*tab.view.document() == before);
    assert!(!tab.is_modified());
}

#[test]
fn text_is_formatted_by_paragraph_and_by_phrase() {
    let mut tab = sample();
    let answer = is_one_undo_step(
        &mut tab,
        "format_text",
        json!({
            "block": 2, "phrase": "ready", "bold": true, "italic": true, "underline": true,
            "strikethrough": true, "color": "red", "highlight": "#FFFF00", "size": 14, "font": "Arial"
        }),
    );
    assert_eq!(answer["phrases"], 1);
    let p = paragraph(&tab, 2);
    let ready = p
        .runs
        .iter()
        .find(|r| r.text == "ready")
        .expect("a run of its own");
    assert!(ready.look.bold && ready.look.italic && ready.look.strike);
    assert!(ready.look.underline.is_some());
    assert_eq!(ready.look.color, Some([0xFF, 0, 0, 0xFF]));
    assert_eq!(ready.look.highlight, Some([0xFF, 0xFF, 0, 0xFF]));
    assert_eq!(ready.look.size, 14.0);
    assert_eq!(ready.look.font.as_deref(), Some("Arial"));
    assert_eq!(p.text(), "The budget is ready for review.");
    assert!(!p.runs[0].look.bold);
    // Off again; every occurrence of a phrase in a range of paragraphs.
    ok(
        &mut tab,
        "format_text",
        json!({ "block": 2, "phrase": "ready", "bold": false, "color": "none" }),
    );
    let ready = paragraph(&tab, 2);
    let ready = ready.runs.iter().find(|r| r.text == "ready").unwrap();
    assert!(!ready.look.bold && ready.look.color.is_none());
    // Whole paragraphs: look, style, alignment, list.
    is_one_undo_step(&mut tab, "format_text", json!({ "block": 2, "bold": true }));
    assert!(paragraph(&tab, 2).runs.iter().all(|r| r.look.bold));
    let answer = is_one_undo_step(
        &mut tab,
        "format_text",
        json!({ "block": 2, "end_block": 2, "style": "Heading 2", "align": "center" }),
    );
    assert_eq!(answer["paragraphs"], 1);
    assert_eq!(
        style_name(tab.view.document(), &paragraph(&tab, 2)),
        "heading 2"
    );
    assert_eq!(paragraph(&tab, 2).props.align, Align::Center);
    is_one_undo_step(
        &mut tab,
        "format_text",
        json!({ "block": 2, "list": "numbered" }),
    );
    let read = ok(
        &mut tab,
        "read_word",
        json!({ "start_block": 2, "max_blocks": 1 }),
    );
    assert_eq!(read["blocks"][0]["list"], "numbered");
    assert_eq!(read["blocks"][0]["align"], "center");
    is_one_undo_step(
        &mut tab,
        "format_text",
        json!({ "block": 3, "end_block": 4, "list": "none" }),
    );
    // A cell of the table.
    is_one_undo_step(
        &mut tab,
        "format_text",
        json!({ "block": 5, "row": 1, "cell": 1, "bold": true }),
    );
    let table = tab.view.document().body.blocks[4].as_table().unwrap();
    let head = table.rows[0].cells[0].blocks[0].as_paragraph().unwrap();
    assert!(head.runs.iter().all(|r| r.look.bold));
    let other = table.rows[1].cells[0].blocks[0].as_paragraph().unwrap();
    assert!(other.runs.iter().all(|r| !r.look.bold));
}

#[test]
fn a_phrase_gets_a_link_that_can_be_taken_off() {
    let mut tab = sample();
    let answer = is_one_undo_step(
        &mut tab,
        "format_text",
        json!({ "block": 2, "phrase": "budget", "link": "https://example.com/budget" }),
    );
    assert_eq!(answer["phrases"], 1);
    let read = ok(
        &mut tab,
        "read_word",
        json!({ "start_block": 2, "max_blocks": 1 }),
    );
    assert_eq!(
        read["blocks"][0]["links"],
        json!([{ "text": "budget", "target": "https://example.com/budget" }])
    );
    ok(
        &mut tab,
        "format_text",
        json!({ "block": 2, "phrase": "budget", "link": "" }),
    );
    let read = ok(
        &mut tab,
        "read_word",
        json!({ "start_block": 2, "max_blocks": 1 }),
    );
    assert!(read["blocks"][0].get("links").is_none());
    for good in ["mailto:a@example.com", "#intro", "http://example.com"] {
        ok(
            &mut tab,
            "format_text",
            json!({ "block": 2, "phrase": "ready", "link": good }),
        );
    }
    for bad in [
        "file:///etc/passwd",
        "javascript:alert(1)",
        "ftp://example.com",
        "C:\\secret.docx",
        "https://exa mple.com",
        "#",
    ] {
        let error = fails(
            &mut tab,
            "format_text",
            json!({ "block": 2, "phrase": "ready", "link": bad }),
        );
        assert!(error.contains("link"), "{bad}: {error}");
    }
}

#[test]
fn formatting_errors_say_what_to_change() {
    let mut tab = sample();
    let before = tab.view.document().clone();
    let error = fails(&mut tab, "format_text", json!({ "block": 2 }));
    assert!(error.contains("nothing to change"), "{error}");
    let error = fails(
        &mut tab,
        "format_text",
        json!({ "block": 2, "phrase": "nowhere", "bold": true }),
    );
    assert!(
        error.contains("nowhere") && error.contains("block 2"),
        "{error}"
    );
    let error = fails(
        &mut tab,
        "format_text",
        json!({ "block": 2, "phrase": "ready", "align": "center" }),
    );
    assert!(error.contains("whole paragraphs"), "{error}");
    let error = fails(
        &mut tab,
        "format_text",
        json!({ "block": 2, "color": "greenish" }),
    );
    assert!(error.contains("not a colour"), "{error}");
    let error = fails(&mut tab, "format_text", json!({ "block": 2, "size": 0 }));
    assert!(error.contains("size"), "{error}");
    let error = fails(
        &mut tab,
        "format_text",
        json!({ "block": 2, "align": "middle" }),
    );
    assert!(error.contains("left, center"), "{error}");
    let error = fails(
        &mut tab,
        "format_text",
        json!({ "block": 2, "style": "Bogus" }),
    );
    assert!(error.contains("no paragraph style named"), "{error}");
    let error = fails(&mut tab, "format_text", json!({ "block": 5, "bold": true }));
    assert!(error.contains("row"), "{error}");
    let error = fails(
        &mut tab,
        "format_text",
        json!({ "block": 5, "row": 9, "bold": true }),
    );
    assert!(error.contains("rows 1 to 3"), "{error}");
    let error = fails(
        &mut tab,
        "format_text",
        json!({ "block": 5, "row": 1, "cell": 7, "bold": true }),
    );
    assert!(error.contains("cells 1 to 2"), "{error}");
    let error = fails(
        &mut tab,
        "format_text",
        json!({ "block": 2, "row": 1, "bold": true }),
    );
    assert!(error.contains("not a table"), "{error}");
    // Already as asked: the call works and changes nothing.
    ok(
        &mut tab,
        "format_text",
        json!({ "block": 2, "bold": false }),
    );
    assert!(*tab.view.document() == before);
}

#[test]
fn tables_are_made_and_their_cells_set() {
    let mut tab = blank();
    ok(
        &mut tab,
        "replace_paragraphs",
        json!({ "block": 1, "text": "Before\nAfter" }),
    );
    let answer = is_one_undo_step(
        &mut tab,
        "edit_table",
        json!({
            "action": "insert", "block": 1,
            "cells": [["Name", "Qty"], ["Tea", 2], ["Cake", null], ["Done", true]]
        }),
    );
    assert_eq!(answer["block"], 2);
    assert_eq!(answer["rows"], 4);
    assert_eq!(answer["columns"], 2);
    assert_eq!(texts(&tab), ["Before", "<table>", "After"]);
    let read = ok(
        &mut tab,
        "read_word",
        json!({ "start_block": 2, "max_blocks": 1 }),
    );
    assert_eq!(
        read["blocks"][0]["cells"],
        json!([
            ["Name", "Qty"],
            ["Tea", "2"],
            ["Cake", ""],
            ["Done", "true"]
        ])
    );
    edit::check_table(tab.view.document().body.blocks[1].as_table().unwrap()).unwrap();
    // A cell's text, with a second paragraph.
    is_one_undo_step(
        &mut tab,
        "edit_table",
        json!({ "action": "set_cell", "block": 2, "row": 3, "cell": 2, "text": "Eight\nnine" }),
    );
    let read = ok(
        &mut tab,
        "read_word",
        json!({ "start_block": 2, "max_blocks": 1 }),
    );
    assert_eq!(read["blocks"][0]["cells"][2][1], "Eight\nnine");
    ok(
        &mut tab,
        "edit_table",
        json!({ "action": "set_cell", "block": 2, "row": 3, "cell": 2, "text": "Ten" }),
    );
    let read = ok(
        &mut tab,
        "read_word",
        json!({ "start_block": 2, "max_blocks": 1 }),
    );
    assert_eq!(read["blocks"][0]["cells"][2][1], "Ten");
    // Before a paragraph, before and after a table.
    ok(
        &mut tab,
        "edit_table",
        json!({ "action": "insert", "block": 3, "position": "before", "cells": [["a", "b", "c"]] }),
    );
    assert_eq!(texts(&tab), ["Before", "<table>", "", "<table>", "After"]);
    // Two tables with nothing between would run together: a paragraph is
    // kept between them.
    let blocks = &tab.view.document().body.blocks;
    assert!(blocks[1].as_table().is_some());
    ok(
        &mut tab,
        "edit_table",
        json!({ "action": "insert", "block": 1, "position": "after", "cells": [["x"]] }),
    );
    for pair in tab.view.document().body.blocks.windows(2) {
        assert!(pair[0].as_table().is_none() || pair[1].as_table().is_none());
    }
    for block in &tab.view.document().body.blocks {
        if let Some(table) = block.as_table() {
            edit::check_table(table).unwrap();
        }
    }
}

#[test]
fn table_rows_and_columns_come_and_go() {
    let mut tab = sample();
    let answer = is_one_undo_step(
        &mut tab,
        "edit_table",
        json!({ "action": "insert_row", "block": 5, "row": 2, "position": "after", "count": 2 }),
    );
    assert_eq!(answer["rows"], 5);
    let read = ok(
        &mut tab,
        "read_word",
        json!({ "start_block": 5, "max_blocks": 1 }),
    );
    assert_eq!(
        read["blocks"][0]["cells"],
        json!([
            ["Item", "Cost"],
            ["Tea", "3"],
            ["", ""],
            ["", ""],
            ["Cake", "4"]
        ])
    );
    ok(
        &mut tab,
        "edit_table",
        json!({ "action": "delete_row", "block": 5, "row": 3 }),
    );
    ok(
        &mut tab,
        "edit_table",
        json!({ "action": "delete_row", "block": 5, "row": 3 }),
    );
    let answer = is_one_undo_step(
        &mut tab,
        "edit_table",
        json!({ "action": "insert_column", "block": 5, "cell": 1, "position": "before" }),
    );
    assert_eq!(answer["columns"], 3);
    let read = ok(
        &mut tab,
        "read_word",
        json!({ "start_block": 5, "max_blocks": 1 }),
    );
    assert_eq!(read["blocks"][0]["cells"][0], json!(["", "Item", "Cost"]));
    let answer = is_one_undo_step(
        &mut tab,
        "edit_table",
        json!({ "action": "delete_column", "block": 5, "cell": 1 }),
    );
    assert_eq!(answer["columns"], 2);
    let read = ok(
        &mut tab,
        "read_word",
        json!({ "start_block": 5, "max_blocks": 1 }),
    );
    assert_eq!(read["blocks"][0]["cells"][0], json!(["Item", "Cost"]));
    edit::check_table(tab.view.document().body.blocks[4].as_table().unwrap()).unwrap();
}

#[test]
fn table_errors_say_what_is_valid() {
    let mut tab = sample();
    let before = tab.view.document().clone();
    let cases = [
        (
            json!({ "action": "set_cell", "block": 2, "row": 1, "cell": 1, "text": "x" }),
            "not a table",
        ),
        (
            json!({ "action": "set_cell", "block": 9, "row": 1, "cell": 1, "text": "x" }),
            "blocks 1 to 6",
        ),
        (
            json!({ "action": "set_cell", "block": 5, "row": 4, "cell": 1, "text": "x" }),
            "rows 1 to 3",
        ),
        (
            json!({ "action": "set_cell", "block": 5, "row": 1, "cell": 3, "text": "x" }),
            "cells 1 to 2",
        ),
        (
            json!({ "action": "set_cell", "block": 5, "row": 1, "text": "x" }),
            "`cell` is needed",
        ),
        (
            json!({ "action": "insert_row", "block": 5 }),
            "`row` is needed",
        ),
        (
            json!({ "action": "insert_column", "block": 5, "row": 1 }),
            "`cell` is needed",
        ),
        (
            json!({ "action": "insert_row", "block": 5, "row": 1, "count": 99 }),
            "at most 50",
        ),
        (
            json!({ "action": "insert", "block": 1, "cells": [] }),
            "no rows",
        ),
        (json!({ "action": "insert", "block": 1 }), "array of rows"),
        (
            json!({ "action": "insert", "block": 1, "cells": [[{}]] }),
            "string, number or boolean",
        ),
        (
            json!({ "action": "frobnicate", "block": 1 }),
            "`action` must be",
        ),
    ];
    for (args, wanted) in cases {
        let error = fails(&mut tab, "edit_table", args.clone());
        assert!(error.contains(wanted), "{args}: {error}");
    }
    // Too big.
    let wide: Vec<Value> = vec![json!("x"); MAX_TABLE_COLUMNS + 1];
    let error = fails(
        &mut tab,
        "edit_table",
        json!({ "action": "insert", "block": 1, "cells": [wide] }),
    );
    assert!(error.contains("columns"), "{error}");
    let tall: Vec<Value> = (0..MAX_TABLE_ROWS + 1).map(|_| json!(["x"])).collect();
    let error = fails(
        &mut tab,
        "edit_table",
        json!({ "action": "insert", "block": 1, "cells": tall }),
    );
    assert!(error.contains("rows"), "{error}");
    let error = fails(
        &mut tab,
        "edit_table",
        json!({ "action": "set_cell", "block": 5, "row": 1, "cell": 1, "text": "x".repeat(MAX_TEXT + 1) }),
    );
    assert!(error.contains("too long"), "{error}");
    // The last row and the last column cannot go: the table would.
    let mut one = blank();
    ok(
        &mut one,
        "edit_table",
        json!({ "action": "insert", "block": 1, "cells": [["a"]] }),
    );
    for action in ["delete_row", "delete_column"] {
        let error = fails(
            &mut one,
            "edit_table",
            json!({ "action": action, "block": 1, "row": 1, "cell": 1 }),
        );
        assert!(error.contains("delete_blocks"), "{error}");
    }
    assert!(*tab.view.document() == before);
}

#[test]
fn a_write_is_refused_while_an_input_method_composes() {
    use aster_surface::ImeEvent;
    let mut tab = sample();
    assert!(tab.view.ime(ImeEvent::Preedit {
        text: "ni".into(),
        cursor: None
    }));
    assert!(tab.view.is_composing());
    let before = tab.view.document().clone();
    let revision = tab.view.revision();
    for (tool, args) in [
        ("replace_paragraphs", json!({ "block": 2, "text": "x" })),
        ("insert_paragraphs", json!({ "block": 2, "text": "x" })),
        ("delete_blocks", json!({ "block": 2 })),
        ("find_and_replace", json!({ "find": "Tea", "replace": "x" })),
        ("format_text", json!({ "block": 2, "bold": true })),
        (
            "edit_table",
            json!({ "action": "delete_row", "block": 5, "row": 1 }),
        ),
    ] {
        assert_eq!(fails(&mut tab, tool, args), COMPOSING, "{tool}");
    }
    assert!(*tab.view.document() == before);
    assert_eq!(tab.view.revision(), revision);
    // Reading is fine, and the composition is still going.
    ok(&mut tab, "read_word", json!({}));
    assert!(tab.view.is_composing());
    tab.view.cancel_composition();
    ok(&mut tab, "format_text", json!({ "block": 2, "bold": true }));
}

#[test]
fn the_users_selection_survives_formatting_and_the_edit_goes_after_the_users_undo() {
    let mut tab = sample();
    tab.view.select_all();
    let selected = tab.view.selected_text();
    ok(
        &mut tab,
        "format_text",
        json!({ "block": 2, "italic": true }),
    );
    assert_eq!(tab.view.selected_text(), selected);
    // The user's own typing before and after stays separate steps.
    tab.view.go_to(&Pos::new(Path::body(5), 0));
    tab.view.type_text("A");
    ok(
        &mut tab,
        "replace_paragraphs",
        json!({ "block": 2, "text": "Changed" }),
    );
    tab.view.type_text("B");
    let typed = tab.view.document().plain_text();
    assert!(typed.contains("AClosing") || typed.contains("A"), "{typed}");
    assert!(tab.view.undo());
    assert!(tab.view.undo());
    assert_eq!(texts(&tab)[1], "The budget is ready for review.");
}

#[test]
fn saved_changes_are_in_the_file() {
    let mut tab = sample();
    ok(
        &mut tab,
        "replace_paragraphs",
        json!({ "block": 2, "text": "Approved by the board." }),
    );
    ok(
        &mut tab,
        "insert_paragraphs",
        json!({ "block": 6, "text": "Appendix\nNotes", "style": "heading 2" }),
    );
    ok(
        &mut tab,
        "find_and_replace",
        json!({ "find": "Tea", "replace": "Green tea" }),
    );
    ok(
        &mut tab,
        "format_text",
        json!({ "block": 2, "phrase": "board", "bold": true, "link": "https://example.com/board" }),
    );
    ok(
        &mut tab,
        "edit_table",
        json!({ "action": "set_cell", "block": 5, "row": 3, "cell": 2, "text": "5" }),
    );
    ok(
        &mut tab,
        "edit_table",
        json!({ "action": "insert_row", "block": 5, "row": 1, "position": "after" }),
    );
    ok(&mut tab, "delete_blocks", json!({ "block": 6 }));
    let job = tab.begin_save().expect("a save starts");
    let bytes = job.bytes().expect("the file is made");
    let read = aster_ooxml::docx::read(bytes).expect("the file reads");
    assert_eq!(
        read.plain_text().replace(OBJECT, ""),
        tab.view.document().plain_text().replace(OBJECT, "")
    );
    let blocks = &read.body.blocks;
    assert_eq!(
        blocks[1].as_paragraph().unwrap().text(),
        "Approved by the board."
    );
    assert!(blocks[1].as_paragraph().unwrap().runs.iter().any(|run| {
        run.look.bold
            && run.text == "board"
            && run
                .link
                .as_ref()
                .is_some_and(|l| l.target == "https://example.com/board")
    }));
    assert_eq!(
        style_name(&read, blocks[0].as_paragraph().unwrap()),
        "heading 1"
    );
    let table = blocks[4].as_table().expect("the table");
    assert_eq!(table.rows.len(), 4);
    assert_eq!(cell_text(&table.rows[3].cells[1].blocks), "5");
    assert_eq!(cell_text(&table.rows[2].cells[0].blocks), "Green tea");
    assert_eq!(cell_text(&table.rows[3].cells[0].blocks), "Cake");
    assert_eq!(cell_text(&table.rows[1].cells[0].blocks), "");
    let bullets: Vec<&Block> = blocks
        .iter()
        .filter(|b| {
            b.as_paragraph()
                .is_some_and(|p| list_kind(&read, p) == Some("bullet"))
        })
        .collect();
    assert_eq!(bullets.len(), 2);
    assert_eq!(bullets[0].as_paragraph().unwrap().text(), "Green tea");
    assert!(read.plain_text().contains("Appendix"));
    assert!(!read.plain_text().contains("Closing remarks"));
}

#[test]
fn every_tool_in_every_language_has_a_card() {
    let tab = sample();
    let revision = tab.view.revision();
    let calls = [
        (
            "replace_paragraphs",
            json!({ "block": 2, "text": "New text" }),
        ),
        (
            "replace_paragraphs",
            json!({ "block": 2, "end_block": 3, "text": "New text" }),
        ),
        ("insert_paragraphs", json!({ "block": 2, "text": "More" })),
        (
            "insert_paragraphs",
            json!({ "block": 2, "position": "before", "text": "More" }),
        ),
        ("delete_blocks", json!({ "block": 2, "end_block": 3 })),
        (
            "find_and_replace",
            json!({ "find": "Tea", "replace": "Chai" }),
        ),
        (
            "find_and_replace",
            json!({ "find": "Tea", "replace": "Chai", "all": false }),
        ),
        (
            "find_and_replace",
            json!({ "find": "Tea", "replace": "Chai", "limit": 1 }),
        ),
        (
            "format_text",
            json!({ "block": 2, "phrase": "budget", "bold": true, "size": 12, "color": "red", "link": "https://example.com" }),
        ),
        (
            "format_text",
            json!({ "block": 2, "style": "heading 2", "align": "center", "list": "none" }),
        ),
        (
            "edit_table",
            json!({ "action": "insert", "block": 2, "cells": [["a", "b"]] }),
        ),
        (
            "edit_table",
            json!({ "action": "set_cell", "block": 5, "row": 2, "cell": 1, "text": "Z" }),
        ),
        (
            "edit_table",
            json!({ "action": "insert_row", "block": 5, "row": 2, "position": "before", "count": 3 }),
        ),
        (
            "edit_table",
            json!({ "action": "delete_row", "block": 5, "row": 2 }),
        ),
        (
            "edit_table",
            json!({ "action": "insert_column", "block": 5, "row": 1, "cell": 1 }),
        ),
        (
            "edit_table",
            json!({ "action": "delete_column", "block": 5, "row": 1, "cell": 1 }),
        ),
    ];
    for language in Language::ALL {
        let strings = language.strings();
        for (tool, args) in &calls {
            let (facts, removed, written) = card(&tab, tool, args, strings);
            assert!(!facts.is_empty(), "{language:?} {tool} {args}");
            assert!(!facts.contains('{'), "{language:?} {tool}: {facts}");
            assert!(!removed.contains('{') && !written.contains("{block}"));
        }
    }
    // What would be replaced and written is shown.
    let strings = Language::En.strings();
    let (facts, removed, written) = card(&tab, "replace_paragraphs", &calls[0].1, strings);
    assert_eq!(facts, "Block 2 of 6");
    assert_eq!(removed, "The budget is ready for review.");
    assert_eq!(written, "New text");
    let (facts, removed, _) = card(&tab, "delete_blocks", &calls[4].1, strings);
    assert_eq!(facts, "Blocks 2–3 of 6");
    assert_eq!(removed, "The budget is ready for review.\nTea");
    let (facts, _, _) = card(&tab, "find_and_replace", &calls[5].1, strings);
    assert!(facts.contains("2"), "{facts}");
    let (_, removed, written) = card(&tab, "edit_table", &calls[11].1, strings);
    assert_eq!((removed.as_str(), written.as_str()), ("Tea", "Z"));
    let (_, _, written) = card(&tab, "format_text", &calls[8].1, strings);
    assert!(
        written.contains("Bold: ✓") || written.contains("✓"),
        "{written}"
    );
    // A bad address gives an empty card, not a panic.
    let (facts, removed, _) = card(
        &tab,
        "replace_paragraphs",
        &json!({ "block": 99, "text": "x" }),
        strings,
    );
    assert!(facts.is_empty() && removed.is_empty());
    let (facts, ..) = card(&tab, "delete_blocks", &json!({ "block": 0 }), strings);
    assert!(facts.is_empty());
    // A card changes nothing.
    assert_eq!(tab.view.revision(), revision);
}

// --- Through the invoker -----------------------------------------------------

type Job = Box<dyn FnOnce(&mut DocTab) + Send>;

/// A document tab on a thread of its own, as the shell's interface thread
/// holds it, and a host that does what the shell's host does with it.
struct TabHost {
    jobs: Mutex<mpsc::Sender<Job>>,
    info: DocumentInfo,
}

impl TabHost {
    fn start(make: impl FnOnce() -> DocTab + Send + 'static) -> Arc<Self> {
        let (jobs, inbox) = mpsc::channel::<Job>();
        std::thread::spawn(move || {
            let mut tab = make();
            while let Ok(job) = inbox.recv() {
                job(&mut tab);
            }
        });
        Arc::new(Self {
            jobs: Mutex::new(jobs),
            info: DocumentInfo {
                id: "7".into(),
                title: "Notes".into(),
                path: None,
                kind: "word".into(),
                modified: false,
            },
        })
    }

    fn with<R: Send + 'static>(&self, job: impl FnOnce(&mut DocTab) -> R + Send + 'static) -> R {
        let (sender, answer) = mpsc::channel();
        self.jobs
            .lock()
            .unwrap()
            .send(Box::new(move |tab| {
                let _ = sender.send(job(tab));
            }))
            .unwrap();
        answer.recv_timeout(Duration::from_secs(20)).unwrap()
    }
}

impl Host for TabHost {
    fn documents(&self) -> Vec<DocumentInfo> {
        vec![self.info.clone()]
    }

    fn fingerprint(&self, _: &ToolSpec, _: Option<&DocumentInfo>, _: &Value) -> Option<String> {
        Some(self.with(|tab| format!("r{}", tab.view.revision())))
    }

    fn run(
        &self,
        tool: &ToolSpec,
        _: Option<&DocumentInfo>,
        args: &Value,
    ) -> Result<String, String> {
        self.run_checked(tool, None, args, None)
    }

    fn run_checked(
        &self,
        tool: &ToolSpec,
        _: Option<&DocumentInfo>,
        args: &Value,
        expect: Option<&str>,
    ) -> Result<String, String> {
        let (name, args, expect) = (tool.name, args.clone(), expect.map(str::to_owned));
        self.with(move |tab| {
            if let Some(expect) = expect
                && format!("r{}", tab.view.revision()) != expect
            {
                return Err(aster_tools::CHANGED_WHILE_ASKING.to_owned());
            }
            run(tab, name, &args).map(|(text, _)| text)
        })
    }
}

/// Answers questions as told, and can change the document while it
/// "asks" (the user typing while a card is up).
struct Answers {
    answer: Mutex<Approval>,
    asked: Mutex<Vec<ApprovalRequest>>,
    meanwhile: Mutex<Option<Box<dyn FnOnce() + Send>>>,
}

impl Approver for Answers {
    fn ask(&self, request: ApprovalRequest, _: Duration) -> Approval {
        self.asked.lock().unwrap().push(request);
        if let Some(job) = self.meanwhile.lock().unwrap().take() {
            job();
        }
        *self.answer.lock().unwrap()
    }
}

struct Setup {
    host: Arc<TabHost>,
    answers: Arc<Answers>,
    invoker: Invoker,
    audit: Arc<Audit>,
}

fn setup(level: Level) -> Setup {
    let host = TabHost::start(sample);
    let answers = Arc::new(Answers {
        answer: Mutex::new(Approval::Allow),
        asked: Mutex::default(),
        meanwhile: Mutex::default(),
    });
    let policy = Policy {
        untitled: level,
        ..Policy::default()
    };
    let audit = Arc::new(Audit::in_memory());
    let invoker = Invoker::new(
        Arc::new(Mutex::new(policy)),
        host.clone(),
        answers.clone(),
        audit.clone(),
    )
    .with_timeout(Duration::from_secs(5));
    Setup {
        host,
        answers,
        invoker,
        audit,
    }
}

fn origin() -> Origin {
    Origin::new("ai:test/model", "chat-1")
}

fn text_of(setup: &Setup) -> Vec<String> {
    setup.host.with(|tab| texts(tab))
}

#[test]
fn a_change_is_asked_about_once_and_made_when_allowed() {
    let setup = setup(Level::Ask);
    let args = json!({ "document": "7", "block": 2, "text": "Approved." });
    let result = setup.invoker.call(&origin(), "replace_paragraphs", &args);
    assert!(!result.is_error, "{}", result.text);
    assert_eq!(text_of(&setup)[1], "Approved.");
    let asked = setup.answers.asked.lock().unwrap();
    assert_eq!(asked.len(), 1);
    assert_eq!(asked[0].tool.name, "replace_paragraphs");
    assert_eq!(asked[0].args, args);
    drop(asked);
    // The card the user would see names what changes.
    let host = setup.host.clone();
    let card_facts = host.with(|tab| {
        card(
            tab,
            "replace_paragraphs",
            &json!({ "block": 2, "text": "x" }),
            Language::En.strings(),
        )
        .0
    });
    assert_eq!(card_facts, "Block 2 of 6");
    // Reads are not asked about.
    let result = setup
        .invoker
        .call(&origin(), "read_word", &json!({ "document": "7" }));
    assert!(!result.is_error);
    assert_eq!(setup.answers.asked.lock().unwrap().len(), 1);
    assert!(result.text.contains("Approved."));
    // The user undoes the whole call at once.
    let undone = setup.host.with(|tab| tab.view.undo());
    assert!(undone);
    assert_eq!(text_of(&setup)[1], "The budget is ready for review.");
}

#[test]
fn a_refused_change_is_not_made_and_read_only_cannot_ask() {
    let setup = setup(Level::Ask);
    *setup.answers.answer.lock().unwrap() = Approval::Deny;
    let result = setup.invoker.call(
        &origin(),
        "delete_blocks",
        &json!({ "document": "7", "block": 1 }),
    );
    assert!(
        result.is_error && result.text.contains("declined"),
        "{}",
        result.text
    );
    assert_eq!(text_of(&setup)[0], "Report");
    let read_only = super::tests::setup(Level::ReadOnly);
    let result = read_only.invoker.call(
        &origin(),
        "format_text",
        &json!({ "document": "7", "block": 1, "bold": true }),
    );
    assert!(
        result.is_error && result.text.contains("read-only"),
        "{}",
        result.text
    );
    assert!(read_only.answers.asked.lock().unwrap().is_empty());
    // Trusted: changes are made without asking.
    let trusted = super::tests::setup(Level::Trusted);
    let result = trusted.invoker.call(
        &origin(),
        "format_text",
        &json!({ "document": "7", "block": 1, "bold": true }),
    );
    assert!(!result.is_error, "{}", result.text);
    assert!(trusted.answers.asked.lock().unwrap().is_empty());
}

#[test]
fn an_approval_is_void_once_the_document_changed_while_asking() {
    for tool_args in [
        (
            "replace_paragraphs",
            json!({ "document": "7", "block": 2, "text": "Late" }),
        ),
        (
            "insert_paragraphs",
            json!({ "document": "7", "block": 2, "text": "Late" }),
        ),
        ("delete_blocks", json!({ "document": "7", "block": 2 })),
        (
            "find_and_replace",
            json!({ "document": "7", "find": "Tea", "replace": "Late" }),
        ),
        (
            "format_text",
            json!({ "document": "7", "block": 2, "bold": true }),
        ),
        (
            "edit_table",
            json!({ "document": "7", "action": "delete_row", "block": 5, "row": 1 }),
        ),
    ] {
        let setup = setup(Level::Ask);
        // The user types while the card is up.
        let host = setup.host.clone();
        *setup.answers.meanwhile.lock().unwrap() = Some(Box::new(move || {
            host.with(|tab| {
                tab.view.go_to(&Pos::new(Path::body(5), 0));
                tab.view.type_text("typed ");
            });
        }));
        let before = setup.host.with(|tab| tab.view.document().clone());
        let _ = before;
        let result = setup.invoker.call(&origin(), tool_args.0, &tool_args.1);
        assert!(result.is_error, "{}", tool_args.0);
        assert_eq!(
            result.text,
            aster_tools::CHANGED_WHILE_ASKING,
            "{}",
            tool_args.0
        );
        let typed = text_of(&setup);
        assert!(typed[5].starts_with("typed "), "{typed:?}");
        assert_eq!(
            typed[1], "The budget is ready for review.",
            "{}",
            tool_args.0
        );
        assert_eq!(setup.audit.recent().len(), 1);
    }
}

#[test]
fn arguments_are_checked_before_anything_is_asked() {
    let setup = setup(Level::Ask);
    for (tool, args) in [
        (
            "replace_paragraphs",
            json!({ "document": "7", "block": "2", "text": "x" }),
        ),
        (
            "edit_table",
            json!({ "document": "7", "action": "explode", "block": 5 }),
        ),
        (
            "format_text",
            json!({ "document": "7", "block": 2, "align": "middle" }),
        ),
        ("insert_paragraphs", json!({ "document": "7", "block": 2 })),
        (
            "format_text",
            json!({ "document": "7", "block": 2, "fill": "red" }),
        ),
    ] {
        let result = setup.invoker.call(&origin(), tool, &args);
        assert!(
            result.is_error && result.text.starts_with("invalid arguments"),
            "{tool}: {}",
            result.text
        );
    }
    assert!(setup.answers.asked.lock().unwrap().is_empty());
}

#[test]
fn a_sequence_of_edits_reads_back_and_undoes_step_by_step() {
    let setup = setup(Level::Trusted);
    let go = |tool: &str, args: Value| {
        let mut args = args;
        args["document"] = json!("7");
        let result = setup.invoker.call(&origin(), tool, &args);
        assert!(!result.is_error, "{tool}: {}", result.text);
        result.text
    };
    let start = setup.host.with(|tab| tab.view.document().clone());
    go(
        "insert_paragraphs",
        json!({ "block": 6, "text": "Appendix", "style": "heading 1" }),
    );
    go(
        "find_and_replace",
        json!({ "find": "budget", "replace": "plan" }),
    );
    go(
        "format_text",
        json!({ "block": 2, "phrase": "plan", "bold": true }),
    );
    go(
        "edit_table",
        json!({ "action": "insert_row", "block": 5, "row": 3 }),
    );
    go(
        "replace_paragraphs",
        json!({ "block": 6, "text": "Wrap-up" }),
    );
    let read: Value = serde_json::from_str(&go("read_word", json!({}))).unwrap();
    assert_eq!(read["total_blocks"], 7);
    assert_eq!(read["blocks"][1]["text"], "The plan is ready for review.");
    assert_eq!(read["blocks"][5]["text"], "Wrap-up");
    assert_eq!(read["blocks"][6]["style"], "heading 1");
    assert_eq!(read["blocks"][4]["rows"], 4);
    // Five calls, five steps of undo, and then the document as it was.
    for _ in 0..5 {
        assert!(setup.host.with(|tab| tab.view.undo()));
    }
    let same = setup.host.with(move |tab| *tab.view.document() == start);
    assert!(
        same,
        "undoing every call gives back the document that was opened"
    );
    // Every call is in the audit log, with what it was.
    assert!(setup.audit.recent().len() >= 6);
}
