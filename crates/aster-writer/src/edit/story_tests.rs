//! Editing in headers, footers and notes, making them, and what deleting a
//! reference does.

use std::sync::Arc;

use super::*;
use crate::{
    Cell, MarginalKind, MarginalSlot, Note, OBJECT, PageField, Row, Section, Story, Style,
    StyleKind, StyleSheet, Table, VertAlign,
};

fn look() -> RunLook {
    RunLook::default()
}

fn text_para(text: &str) -> Paragraph {
    Paragraph {
        runs: vec![Run::text(text, look())],
        ..Paragraph::default()
    }
}

fn blocks(texts: &[&str]) -> Vec<Block> {
    texts
        .iter()
        .map(|t| Block::paragraph(text_para(t)))
        .collect()
}

/// A body of two paragraphs, a header "head" (marginal 0) and a footnote 1
/// with "note", referred to from the first paragraph.
fn sample() -> Document {
    let mut body = text_para("Body one");
    body.runs.push(Run::object(Inline::Footnote(1), look()));
    Document {
        body: Story {
            blocks: vec![Block::paragraph(body), Block::paragraph(text_para("two"))],
            part: None,
        },
        marginals: vec![Arc::new(Story {
            blocks: blocks(&["head", "second"]),
            part: None,
        })],
        footnotes: crate::Notes {
            notes: vec![Note {
                id: 1,
                blocks: blocks(&["note", "more"]),
            }],
            part: None,
        },
        ..Document::default()
    }
}

fn header(block: usize, offset: usize) -> Pos {
    Pos::new(Path::in_story(StoryId::Marginal(0), block), offset)
}

fn note(block: usize, offset: usize) -> Pos {
    Pos::new(Path::in_story(StoryId::Footnote(1), block), offset)
}

fn body(block: usize, offset: usize) -> Pos {
    Pos::new(Path::body(block), offset)
}

fn story_texts(doc: &Document, story: StoryId) -> Vec<String> {
    doc.paragraphs_in(story)
        .iter()
        .map(|(_, p)| p.text())
        .collect()
}

#[test]
fn a_path_names_its_story_and_the_body_is_the_default() {
    assert_eq!(Path::body(3).story, StoryId::Body);
    assert_eq!(Path::from(vec![1, 2]).story, StoryId::Body);
    assert_eq!(Path::default().story, StoryId::Body);
    let doc = sample();
    assert_eq!(doc.paragraph(&header(1, 0).path).unwrap().text(), "second");
    assert_eq!(doc.paragraph(&note(0, 0).path).unwrap().text(), "note");
    // A story that does not exist leads nowhere.
    let ghost = Path::in_story(StoryId::Footnote(9), 0);
    assert!(doc.paragraph(&ghost).is_none());
    assert!(
        doc.paragraph(&Path::in_story(StoryId::Marginal(4), 0))
            .is_none()
    );
}

#[test]
fn typing_and_deleting_work_in_a_header_and_a_note() {
    for (story, at) in [
        (StoryId::Marginal(0), header(0, 4)),
        (StoryId::Footnote(1), note(0, 4)),
    ] {
        let mut doc = sample();
        let before = doc.clone();
        let caret = insert_text(&mut doc, &at, "er", None);
        assert_eq!(caret.path.story, story);
        assert_eq!(story_texts(&doc, story)[0].len(), 6);
        // The body is the same blocks.
        assert!(doc.body.blocks[0].is(&before.body.blocks[0]));
        // Enter, then Backspace joins again.
        let split = split_paragraph(&mut doc, &Pos::new(at.path.clone(), 2));
        assert_eq!(split.path.story, story);
        assert_eq!(story_texts(&doc, story).len(), 3);
        let joined = delete_backward(&mut doc, &split);
        assert_eq!(joined.path.story, story);
        assert_eq!(story_texts(&doc, story).len(), 2);
        // A range across the two paragraphs.
        let start = Pos::new(at.path.with_steps(vec![0]), 1);
        let end = Pos::new(at.path.with_steps(vec![1]), 2);
        delete(&mut doc, &start, &end);
        assert_eq!(story_texts(&doc, story).len(), 1);
        // Forward delete at the end of the last paragraph does nothing.
        let last = doc.story_end(story).unwrap();
        delete_forward(&mut doc, &last);
        assert_eq!(story_texts(&doc, story).len(), 1);
        assert_eq!(texts_of_body(&doc), texts_of_body(&before));
    }
}

fn texts_of_body(doc: &Document) -> Vec<String> {
    story_texts(doc, StoryId::Body)
}

#[test]
fn formatting_alignment_and_lists_work_in_a_header_and_a_note() {
    for (story, a, b) in [
        (StoryId::Marginal(0), header(0, 0), header(1, 3)),
        (StoryId::Footnote(1), note(0, 0), note(1, 3)),
    ] {
        let mut doc = sample();
        apply_run_patch(
            &mut doc,
            &a,
            &b,
            &RunPatch {
                bold: Some(true),
                ..RunPatch::default()
            },
        );
        for (_, p) in doc.paragraphs_in(story) {
            assert!(p.runs.iter().any(|r| r.look.bold));
        }
        set_align(&mut doc, &a, &b, Align::Center);
        assert!(
            doc.paragraphs_in(story)
                .iter()
                .all(|(_, p)| p.props.align == Align::Center)
        );
        set_list(&mut doc, &a, &b, Some(ListKind::Numbered));
        let labels: Vec<String> = doc
            .paragraphs_in(story)
            .iter()
            .map(|(_, p)| p.label.as_ref().unwrap().text.clone())
            .collect();
        assert_eq!(labels, ["1.", "2."]);
        // The body has no list.
        assert!(doc.paragraphs().iter().all(|(_, p)| p.label.is_none()));
        // A list in a header counts on its own: the body starts at 1 too.
        set_list(&mut doc, &body(0, 0), &body(1, 0), Some(ListKind::Numbered));
        assert_eq!(doc.paragraphs()[0].1.label.as_ref().unwrap().text, "1.");
        assert_eq!(
            doc.paragraphs_in(story)[0].1.label.as_ref().unwrap().text,
            "1."
        );
    }
}

#[test]
fn a_table_and_a_link_can_go_in_a_header_and_a_note() {
    for (story, at) in [
        (StoryId::Marginal(0), header(0, 2)),
        (StoryId::Footnote(1), note(0, 2)),
    ] {
        let mut doc = sample();
        let caret = insert_table(&mut doc, &at, 2, 2, 4000);
        assert_eq!(caret.path.story, story);
        assert!(
            doc.story_blocks(story)
                .unwrap()
                .iter()
                .any(|b| b.as_table().is_some())
        );
        // Text typed into a cell of it stays in the story.
        let typed = insert_text(&mut doc, &caret, "cell", None);
        assert_eq!(typed.path.story, story);
        assert_eq!(doc.paragraph(&typed.path).unwrap().text(), "cell");
        // A link on the first paragraph's text.
        let first = doc
            .paragraphs_in(story)
            .into_iter()
            .find(|(_, p)| p.text().starts_with("he") || p.text().starts_with("no"))
            .map(|(path, _)| path)
            .unwrap();
        set_link(
            &mut doc,
            &Pos::new(first.clone(), 0),
            &Pos::new(first.clone(), 2),
            Some(crate::Link {
                target: "https://example.org/".into(),
                tooltip: None,
            }),
        );
        assert!(
            doc.paragraph(&first)
                .unwrap()
                .runs
                .iter()
                .any(|r| r.link.is_some())
        );
        delete_table(&mut doc, &caret);
        assert!(
            doc.story_blocks(story)
                .unwrap()
                .iter()
                .all(|b| b.as_table().is_none())
        );
    }
}

#[test]
fn a_range_in_two_stories_does_nothing() {
    let mut doc = sample();
    let before = doc.clone();
    assert_eq!(delete(&mut doc, &body(0, 1), &header(0, 2)), body(0, 1));
    apply_run_patch(
        &mut doc,
        &body(0, 0),
        &note(0, 2),
        &RunPatch {
            bold: Some(true),
            ..RunPatch::default()
        },
    );
    set_align(&mut doc, &header(0, 0), &note(0, 1), Align::Right);
    set_list(&mut doc, &body(0, 0), &header(1, 1), Some(ListKind::Bullet));
    assert!(fragment(&doc, &body(0, 0), &note(0, 2)).is_empty());
    assert!(same_content(&doc, &before));
}

#[test]
fn page_breaks_and_note_references_are_refused_outside_the_body() {
    let mut doc = sample();
    let before = doc.clone();
    assert_eq!(
        insert_break(&mut doc, &header(0, 1), Break::Page),
        header(0, 1)
    );
    assert_eq!(
        insert_break(&mut doc, &note(0, 1), Break::Column),
        note(0, 1)
    );
    assert!(insert_footnote(&mut doc, &header(0, 1)).is_none());
    assert!(insert_footnote(&mut doc, &note(0, 1)).is_none());
    assert!(same_content(&doc, &before));
    // A line break is fine.
    insert_break(&mut doc, &header(0, 1), Break::Line);
    assert_eq!(story_texts(&doc, StoryId::Marginal(0))[0], "h\nead");
    // Pasting leaves out references and page breaks, keeps the text.
    let pasted = vec![Block::paragraph(Paragraph {
        runs: vec![
            Run::text("a", look()),
            Run::object(Inline::Footnote(1), look()),
            Run::object(Inline::Break(Break::Page), look()),
            Run::text("b", look()),
        ],
        ..Paragraph::default()
    })];
    let end = insert_fragment(&mut doc, &header(1, 0), &pasted);
    assert_eq!(doc.paragraph(&end.path).unwrap().text(), "absecond");
    assert_eq!(end.offset, 2);
    // In the body they stay.
    insert_fragment(&mut doc, &body(1, 0), &pasted);
    assert!(doc.paragraphs()[1].1.text().contains(OBJECT));
}

#[test]
fn a_footnote_has_a_reference_and_a_note_with_one_empty_paragraph() {
    let mut doc = sample();
    let selection = insert_footnote(&mut doc, &body(1, 1)).unwrap();
    assert!(selection.is_caret());
    let id = 2;
    assert_eq!(
        selection.focus.path,
        Path::in_story(StoryId::Footnote(id), 0)
    );
    // The reference sits in the paragraph, between "t" and "wo".
    let p = doc.paragraph(&body(1, 0).path).unwrap();
    assert_eq!(p.runs.len(), 3);
    assert_eq!(p.runs[1].kind, Inline::Footnote(id));
    assert_eq!(p.runs[1].look.position, VertAlign::Superscript);
    // The note: its own mark and a space; the caret is after them.
    let n = doc.paragraph(&selection.focus.path).unwrap();
    assert_eq!(n.runs[0].kind, Inline::Footnote(id));
    assert_eq!(n.text(), format!("{OBJECT} "));
    assert_eq!(selection.focus.offset, n.len());
    assert!(n.runs[1].look.size <= 10.0);
    // Direct formatting: the paragraph's base look is the style sheet's.
    assert_eq!(n.base, doc.styles.default_run);
    // Typing goes into the note.
    let caret = insert_text(&mut doc, &selection.focus, "Source", None);
    assert_eq!(caret.path.story, StoryId::Footnote(id));
    assert_eq!(doc.footnotes.notes.len(), 2);
    assert_eq!(doc.footnotes.notes[1].id, id);
    // A second footnote gets the next id.
    let again = insert_footnote(&mut doc, &body(0, 0)).unwrap();
    assert_eq!(again.focus.path.story, StoryId::Footnote(3));
}

fn with_note_styles(mut doc: Document) -> Document {
    let mut sheet = StyleSheet::default();
    let text_look = RunLook {
        size: 9.0,
        ..look()
    };
    sheet.styles.push(Style {
        id: Arc::from("FootnoteText"),
        name: "footnote text".into(),
        kind: StyleKind::Paragraph,
        based_on: None,
        next: None,
        para: ParaProps {
            style: None,
            space_after: 0,
            ..ParaProps::default()
        },
        run: text_look,
        hidden: false,
        quick: false,
        priority: 99,
    });
    sheet.styles.push(Style {
        id: Arc::from("FootnoteReference"),
        name: "footnote reference".into(),
        kind: StyleKind::Character,
        based_on: None,
        next: None,
        para: ParaProps::default(),
        run: RunLook {
            position: VertAlign::Superscript,
            ..look()
        },
        hidden: false,
        quick: false,
        priority: 99,
    });
    doc.styles = Arc::new(sheet);
    doc
}

#[test]
fn a_footnote_uses_the_documents_note_styles_when_it_has_them() {
    let mut doc = with_note_styles(sample());
    let selection = insert_footnote(&mut doc, &body(0, 2)).unwrap();
    let p = doc.paragraph(&body(0, 0).path).unwrap();
    let reference = p
        .runs
        .iter()
        .find(|r| r.kind == Inline::Footnote(2))
        .unwrap();
    assert_eq!(reference.style.as_deref(), Some("FootnoteReference"));
    assert_eq!(reference.look.position, VertAlign::Superscript);
    let n = doc.paragraph(&selection.focus.path).unwrap();
    assert_eq!(n.props.style.as_deref(), Some("FootnoteText"));
    assert_eq!(n.base.size, 9.0);
    assert_eq!(n.runs[0].style.as_deref(), Some("FootnoteReference"));
}

#[test]
fn deleting_a_reference_takes_its_note_and_undo_brings_it_back() {
    let mut doc = sample();
    let mut history = History::default();
    insert_footnote(&mut doc, &body(1, 2)).unwrap();
    assert_eq!(doc.footnotes.notes.len(), 2);

    // Backspace after the new reference removes it and its note.
    let before = doc.clone();
    let after_reference = body(1, 2 + OBJECT.len());
    let caret = delete_backward(&mut doc, &after_reference);
    assert_eq!(caret, body(1, 2));
    assert_eq!(doc.footnotes.notes.len(), 1);
    assert_eq!(doc.footnotes.notes[0].id, 1);
    history.record(
        &before,
        &Selection::caret(after_reference.clone()),
        &Selection::caret(caret),
        None,
    );
    let (restored, restored_selection) = history.undo(&doc).unwrap();
    assert!(same_content(&restored, &before));
    assert_eq!(restored.footnotes.notes.len(), 2);
    assert_eq!(restored_selection.focus, after_reference);

    // A range that covers the reference of note 1 and more.
    let mut doc = sample();
    delete(&mut doc, &body(0, 0), &body(1, 1));
    assert!(doc.footnotes.notes.is_empty());
    assert_eq!(doc.paragraphs().len(), 1);

    // Deleting text that does not cover a reference leaves the notes.
    let mut doc = sample();
    delete(&mut doc, &body(0, 0), &body(0, 3));
    assert_eq!(doc.footnotes.notes.len(), 1);

    // Two references to one note: the note stays while one is left.
    let mut doc = sample();
    doc.body.blocks[1] = Block::paragraph(Paragraph {
        runs: vec![Run::object(Inline::Footnote(1), look())],
        ..Paragraph::default()
    });
    delete(&mut doc, &body(1, 0), &body(1, OBJECT.len()));
    assert_eq!(doc.footnotes.notes.len(), 1);
    delete(&mut doc, &body(0, 0), &body(0, 20));
    assert!(doc.footnotes.notes.is_empty());

    // Deleting the note's own mark inside the note does not touch notes.
    let mut doc = sample();
    doc.footnotes.notes[0].blocks[0] = Block::paragraph(Paragraph {
        runs: vec![
            Run::object(Inline::Footnote(1), look()),
            Run::text(" x", look()),
        ],
        ..Paragraph::default()
    });
    delete(&mut doc, &note(0, 0), &note(0, OBJECT.len()));
    assert_eq!(doc.footnotes.notes.len(), 1);
}

fn two_sections() -> Document {
    let first = Section::default();
    let mut doc = Document {
        body: Story {
            blocks: vec![
                Block::paragraph(Paragraph {
                    runs: vec![Run::text("one", look())],
                    section: Some(Arc::new(first)),
                    ..Paragraph::default()
                }),
                Block::paragraph(text_para("two")),
            ],
            part: None,
        },
        ..Document::default()
    };
    doc.last_section = Arc::new(Section::default());
    doc
}

#[test]
fn a_header_is_made_on_the_first_edit_and_looking_changes_nothing() {
    let mut doc = two_sections();
    let before = doc.clone();
    for kind in [MarginalKind::Header, MarginalKind::Footer] {
        assert!(existing_marginal(&doc, 0, kind, MarginalSlot::Default).is_none());
    }
    assert!(same_content(&doc, &before));
    assert!(doc.marginals.is_empty());

    let story = ensure_marginal(&mut doc, 0, MarginalKind::Header, MarginalSlot::Default).unwrap();
    assert_eq!(story, StoryId::Marginal(0));
    assert_eq!(doc.marginals.len(), 1);
    // Empty, one paragraph.
    assert_eq!(story_texts(&doc, story), [""]);
    // Both sections show it: the second had none of its own.
    let sections = doc.sections();
    assert_eq!(sections[0].1.headers.default, Some(0));
    assert_eq!(sections[1].1.headers.default, Some(0));
    // The footer and the other slots are untouched.
    assert_eq!(sections[0].1.footers.default, None);
    assert_eq!(sections[0].1.headers.first, None);

    // Asking again gives the same story, and editing it from the second
    // section edits the story it inherits.
    assert_eq!(
        ensure_marginal(&mut doc, 1, MarginalKind::Header, MarginalSlot::Default),
        Some(story)
    );
    assert_eq!(doc.marginals.len(), 1);
    let caret = insert_text(
        &mut doc,
        &Pos::new(Path::in_story(story, 0), 0),
        "Title",
        None,
    );
    assert_eq!(caret.offset, 5);
    assert_eq!(story_texts(&doc, story), ["Title"]);
}

#[test]
fn a_header_made_in_a_later_section_does_not_reach_back() {
    let mut doc = two_sections();
    let story = ensure_marginal(&mut doc, 1, MarginalKind::Footer, MarginalSlot::First).unwrap();
    let sections = doc.sections();
    assert_eq!(sections[0].1.footers.first, None);
    assert_eq!(sections[1].1.footers.first, Some(0));
    assert_eq!(story, StoryId::Marginal(0));
    // A section that does not exist has none.
    assert!(ensure_marginal(&mut doc, 7, MarginalKind::Header, MarginalSlot::Default).is_none());

    // A later section with a header of its own keeps it.
    let mut doc = two_sections();
    ensure_marginal(&mut doc, 1, MarginalKind::Header, MarginalSlot::Default).unwrap();
    ensure_marginal(&mut doc, 0, MarginalKind::Header, MarginalSlot::Default).unwrap();
    let sections = doc.sections();
    assert_eq!(sections[0].1.headers.default, Some(1));
    assert_eq!(sections[1].1.headers.default, Some(0));
}

#[test]
fn a_new_header_takes_the_header_style_when_there_is_one() {
    let mut doc = two_sections();
    let mut sheet = StyleSheet::default();
    sheet.styles.push(Style {
        id: Arc::from("Header"),
        name: "header".into(),
        kind: StyleKind::Paragraph,
        based_on: None,
        next: None,
        para: ParaProps::default(),
        run: RunLook {
            size: 9.0,
            ..look()
        },
        hidden: false,
        quick: false,
        priority: 99,
    });
    doc.styles = Arc::new(sheet);
    let story = ensure_marginal(&mut doc, 0, MarginalKind::Header, MarginalSlot::Default).unwrap();
    let p = doc.paragraph(&Path::in_story(story, 0)).unwrap();
    assert_eq!(p.props.style.as_deref(), Some("Header"));
    assert_eq!(p.base.size, 9.0);
    // A footer, where the sheet has no Footer style, has the defaults.
    let story = ensure_marginal(&mut doc, 0, MarginalKind::Footer, MarginalSlot::Default).unwrap();
    assert_eq!(
        doc.paragraph(&Path::in_story(story, 0))
            .unwrap()
            .props
            .style,
        None
    );
}

#[test]
fn a_page_number_goes_in_at_the_caret() {
    let mut doc = sample();
    let caret = insert_page_number(&mut doc, &header(0, 2), PageField::Page);
    assert_eq!(caret.path.story, StoryId::Marginal(0));
    let p = doc.paragraph(&caret.path).unwrap();
    assert_eq!(p.runs.len(), 3);
    assert_eq!(p.runs[1].kind, Inline::PageNumber(PageField::Page));
    assert_eq!(p.text(), "he1ad");
    assert_eq!(caret.offset, 3);
    // The text typed after goes behind it.
    let caret = insert_text(&mut doc, &caret, "/", None);
    assert_eq!(doc.paragraph(&caret.path).unwrap().text(), "he1/ad");
    // A page count in the body is allowed too.
    insert_page_number(&mut doc, &body(1, 0), PageField::PageCount);
    // A path that leads nowhere changes nothing.
    let ghost = Pos::new(Path::in_story(StoryId::Footnote(8), 0), 0);
    assert_eq!(insert_page_number(&mut doc, &ghost, PageField::Page), ghost);
}

#[test]
fn editing_a_header_copies_only_what_it_changes() {
    let mut doc = sample();
    let before = doc.clone();
    insert_text(&mut doc, &header(1, 0), "x", None);
    assert!(!Arc::ptr_eq(&doc.marginals[0], &before.marginals[0]));
    assert!(doc.marginals[0].blocks[0].is(&before.marginals[0].blocks[0]));
    assert!(!doc.marginals[0].blocks[1].is(&before.marginals[0].blocks[1]));
    assert!(!same_content(&doc, &before));
    // A change to a note is a change.
    let mut doc = sample();
    insert_text(&mut doc, &note(1, 0), "x", None);
    assert!(!same_content(&doc, &before));
    // A path that leads nowhere does not unshare a header.
    let mut same = before.clone();
    assert!(
        same.paragraph_mut(&Path::in_story(StoryId::Marginal(0), 9))
            .is_none()
    );
    assert!(Arc::ptr_eq(&same.marginals[0], &before.marginals[0]));
}

#[test]
fn a_cell_in_a_header_has_its_siblings_in_the_header() {
    let mut doc = sample();
    let cell_blocks = blocks(&["a", "b"]);
    let table = Table {
        rows: vec![Row {
            cells: vec![Cell {
                blocks: cell_blocks,
                ..Cell::default()
            }],
            ..Row::default()
        }],
        ..Table::default()
    };
    Arc::make_mut(&mut doc.marginals[0])
        .blocks
        .insert(1, Block::table(table));
    let inner = Path::in_story(StoryId::Marginal(0), 1).cell_block(0, 0, 1);
    assert_eq!(doc.paragraph(&inner).unwrap().text(), "b");
    assert_eq!(doc.siblings(&inner).unwrap().len(), 2);
    doc.siblings_mut(&inner).unwrap().pop();
    assert_eq!(doc.siblings(&inner).unwrap().len(), 1);
    let top = Path::in_story(StoryId::Marginal(0), 0);
    assert_eq!(doc.siblings(&top).unwrap().len(), 3);
}
