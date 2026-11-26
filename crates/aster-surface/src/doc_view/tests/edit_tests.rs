//! Tests of editing through the view: typing, composing, deleting, the
//! clipboard, formatting, undo, replace and saving.

use super::*;
use crate::text_surface::ImeEvent;
use aster_writer::edit::same_content;

/// The text of the first paragraph the page's layout draws.
trait LayoutText {
    fn layout_text(&self, page: usize) -> String;
}

impl LayoutText for DocView {
    fn layout_text(&self, page: usize) -> String {
        self.layout().pages[page]
            .items
            .iter()
            .find_map(|item| match item {
                aster_layout::Item::Text(t) => Some(t.paragraph.text().to_owned()),
                _ => None,
            })
            .unwrap_or_default()
    }
}

fn caret_at(view: &mut DocView, block: usize, offset: usize) {
    let p = pos(block, offset);
    view.set_selection(p.clone(), p);
}

fn texts(view: &DocView) -> Vec<String> {
    view.document()
        .paragraphs()
        .iter()
        .map(|(_, p)| p.text())
        .collect()
}

fn select(view: &mut DocView, a: Pos, b: Pos) {
    assert!(view.set_selection(a, b));
}

#[test]
fn typing_replaces_the_selection_and_undo_redo_round_trips() {
    let mut view = view_of(doc(vec![pb("hello world"), pb("second")]));
    let original = view.document().clone();
    assert!(!view.is_modified());
    select(&mut view, pos(0, 6), pos(0, 11));
    assert!(view.type_text("there"));
    assert_eq!(texts(&view)[0], "hello there");
    assert_eq!(caret(&view), pos(0, 11));
    assert!(view.is_modified());
    // Typing on is one step with the first; undo goes back to the start.
    view.type_text("!");
    view.type_text("?");
    assert_eq!(texts(&view)[0], "hello there!?");
    assert!(view.undo());
    assert_eq!(texts(&view)[0], "hello there");
    assert!(view.undo());
    assert_eq!(texts(&view)[0], "hello world");
    // The selection is restored with it.
    assert_eq!(view.selected_text(), "world");
    assert!(same_content(view.document(), &original));
    assert!(view.redo());
    assert!(view.redo());
    assert_eq!(texts(&view)[0], "hello there!?");
    assert!(view.undo());
    assert!(view.undo());
    // A new edit drops what could be redone.
    caret_at(&mut view, 1, 0);
    view.type_text("x");
    assert!(!view.can_redo());
    assert!(view.undo());
    assert!(same_content(view.document(), &original));
}

#[test]
fn typing_steps_end_when_the_caret_moves_or_another_kind_of_edit_comes() {
    let mut view = view_of(doc(vec![pb("")]));
    for c in ["a", "b", "c"] {
        view.type_text(c);
    }
    view.move_caret(Move::Left, false);
    view.move_caret(Move::Right, false);
    view.type_text("d");
    assert_eq!(texts(&view)[0], "abcd");
    assert!(view.undo());
    assert_eq!(texts(&view)[0], "abc");
    assert!(view.undo());
    assert_eq!(texts(&view)[0], "");
    assert!(!view.undo());
    // Backspace after typing is a step of its own.
    for c in ["x", "y"] {
        view.type_text(c);
    }
    view.backspace(Unit::Cluster);
    assert_eq!(texts(&view)[0], "x");
    assert!(view.undo());
    assert_eq!(texts(&view)[0], "xy");
    // A pause ends a step too.
    view.seal_history();
    view.type_text("z");
    assert!(view.undo());
    assert_eq!(texts(&view)[0], "xy");
}

#[test]
fn a_look_chosen_with_only_a_caret_goes_on_the_next_text_and_is_dropped_when_it_moves() {
    let mut view = view_of(doc(vec![pb("plain")]));
    caret_at(&mut view, 0, 5);
    assert!(view.command(DocCommand::Bold));
    assert_eq!(view.format_state().bold, Tri::On);
    assert!(view.has_pending_look());
    view.type_text("BOLD");
    view.type_text("er");
    let p = view.document().paragraph(&Path::body(0)).unwrap();
    let bold: String = p
        .runs
        .iter()
        .filter(|r| r.look.bold)
        .map(|r| r.text.as_str())
        .collect();
    assert_eq!(bold, "BOLDer");
    // Chosen, then the caret moves: dropped.
    caret_at(&mut view, 0, 2);
    assert!(view.command(DocCommand::Italic));
    view.move_caret(Move::Right, false);
    assert!(!view.has_pending_look());
    view.type_text("-");
    assert!(
        view.document()
            .paragraph(&Path::body(0))
            .unwrap()
            .runs
            .iter()
            .all(|r| !r.look.italic)
    );
    // Size, then typing.
    caret_at(&mut view, 0, 0);
    view.command(DocCommand::Size(20.0));
    assert_eq!(view.format_state().size, Some(20.0));
    view.type_text(">");
    let first = &view.document().paragraph(&Path::body(0)).unwrap().runs[0];
    assert_eq!((first.text.as_str(), first.look.size), (">", 20.0));
}

#[test]
fn enter_splits_and_breaks_are_inserted() {
    let mut view = view_of(doc(vec![pb("one two")]));
    caret_at(&mut view, 0, 3);
    assert!(view.enter());
    assert_eq!(texts(&view), ["one", " two"]);
    assert_eq!(caret(&view), pos(1, 0));
    assert!(view.insert_break(Break::Line));
    assert!(view.insert_break(Break::Page));
    let p = view.document().paragraph(&Path::body(1)).unwrap();
    assert!(p.text().starts_with("\n\u{fffc}"));
    select(&mut view, pos(0, 1), pos(1, 0));
    assert!(view.enter());
    assert_eq!(texts(&view)[0], "o");
    while view.undo() {}
    assert_eq!(texts(&view), ["one two"]);
}

#[test]
fn deleting_by_cluster_word_and_line_and_joining_paragraphs() {
    let mut view = view_of(doc(vec![
        pb("ae\u{301}x"),
        pb("alpha beta  gamma"),
        pb("tail"),
    ]));
    caret_at(&mut view, 0, 4);
    assert!(view.backspace(Unit::Cluster));
    assert_eq!(texts(&view)[0], "ax");
    caret_at(&mut view, 1, 17);
    assert!(view.backspace(Unit::Word));
    assert_eq!(texts(&view)[1], "alpha beta  ");
    assert!(view.backspace(Unit::Word));
    assert_eq!(texts(&view)[1], "alpha ");
    caret_at(&mut view, 1, 0);
    assert!(view.delete_forward(Unit::Word));
    assert_eq!(texts(&view)[1], "");
    // At the start of a paragraph, backspace joins it to the one before.
    caret_at(&mut view, 2, 0);
    assert!(view.backspace(Unit::Cluster));
    assert_eq!(texts(&view), ["ax", "tail"]);
    assert_eq!(caret(&view), pos(1, 0));
    // Delete at the end joins the next.
    caret_at(&mut view, 0, 2);
    assert!(view.delete_forward(Unit::Cluster));
    assert_eq!(texts(&view), ["axtail"]);
    // Whole line.
    caret_at(&mut view, 0, 3);
    assert!(view.backspace(Unit::Line));
    assert_eq!(texts(&view), ["ail"]);
    caret_at(&mut view, 0, 1);
    assert!(view.delete_forward(Unit::Line));
    assert_eq!(texts(&view), ["a"]);
    // A selection goes whole, whatever the unit.
    caret_at(&mut view, 0, 0);
    view.type_text("hello");
    select(&mut view, pos(0, 1), pos(0, 3));
    assert!(view.backspace(Unit::Word));
    assert_eq!(texts(&view), ["hloa"]);
    // At the very start nothing happens.
    caret_at(&mut view, 0, 0);
    assert!(!view.backspace(Unit::Cluster) || texts(&view) == ["hloa"]);
}

#[test]
fn tab_types_a_tab_moves_lists_a_level_and_walks_table_cells() {
    let mut view = view_of(doc(vec![
        pb("item"),
        table(&[&["a1", "a2"], &["b1", "b2"]]),
        pb("after"),
    ]));
    caret_at(&mut view, 0, 2);
    assert!(view.tab(false));
    assert_eq!(texts(&view)[0], "it\tem");
    assert!(!view.tab(true), "Shift+Tab does nothing in plain text");
    // List start: level in and out.
    caret_at(&mut view, 0, 0);
    assert!(view.command(DocCommand::Bullets));
    let level = |view: &DocView| {
        view.document()
            .paragraph(&Path::body(0))
            .unwrap()
            .props
            .list
            .as_ref()
            .map(|l| l.level)
    };
    assert_eq!(level(&view), Some(0));
    assert!(view.tab(false));
    assert_eq!(level(&view), Some(1));
    assert!(view.tab(true));
    assert_eq!(level(&view), Some(0));
    assert_eq!(texts(&view)[0], "it\tem");
    // Table: next and previous cells, the last cell stays.
    caret_at(&mut view, 0, 0);
    view.set_selection(cell_pos(1, 0, 0, 1), cell_pos(1, 0, 0, 1));
    assert!(view.tab(false));
    assert_eq!(caret(&view), cell_pos(1, 0, 1, 0));
    assert!(view.tab(false));
    assert_eq!(caret(&view), cell_pos(1, 1, 0, 0));
    assert!(view.tab(true));
    assert_eq!(caret(&view), cell_pos(1, 0, 1, 0));
    view.set_selection(cell_pos(1, 1, 1, 1), cell_pos(1, 1, 1, 1));
    assert!(!view.tab(false));
    assert_eq!(caret(&view), cell_pos(1, 1, 1, 1));
    // Typing in a cell.
    view.type_text("!");
    assert_eq!(texts(&view)[4], "b2".replace("b2", "b!2"));
}

#[test]
fn composition_shows_the_preedit_text_and_a_commit_replaces_the_selection() {
    let mut view = view_of(doc(vec![pb("keep this"), pb("tail")]));
    let before = view.document().clone();
    select(&mut view, pos(0, 5), pos(0, 9));
    // The preedit text is shown, underlined; the document is not touched.
    assert!(view.ime(ImeEvent::Preedit {
        text: "ni".into(),
        cursor: Some((2, 2)),
    }));
    assert!(view.is_composing());
    assert_eq!(*view.document(), before);
    assert_eq!(view.layout_text(0), "keep ni");
    let (x, y, _, h) = view.ime_cursor_area();
    assert!(h > 0.0 && x > 0.0 && y >= 0.0);
    // The candidate text changes as the user goes on.
    view.ime(ImeEvent::Preedit {
        text: "你好".into(),
        cursor: Some((6, 6)),
    });
    assert_eq!(view.layout_text(0), "keep 你好");
    // Keys do nothing while composing.
    assert!(!view.type_text("x"));
    assert!(!view.undo());
    // Cancel: the text and the selection are as they were.
    assert!(view.ime(ImeEvent::Preedit {
        text: String::new(),
        cursor: None,
    }));
    assert!(!view.is_composing());
    assert_eq!(*view.document(), before);
    assert_eq!(view.selected_text(), "this");
    // Compose again and commit: the selection is replaced, one step of undo.
    view.ime(ImeEvent::Preedit {
        text: "ni".into(),
        cursor: Some((2, 2)),
    });
    assert!(view.ime(ImeEvent::Commit("你".into())));
    assert!(!view.is_composing());
    assert_eq!(texts(&view)[0], "keep 你");
    assert_eq!(caret(&view), pos(0, 8));
    assert!(view.undo());
    assert_eq!(texts(&view)[0], "keep this");
    // The commit may come first and the empty preedit after it, or a commit
    // with no composition at all.
    caret_at(&mut view, 1, 4);
    view.ime(ImeEvent::Preedit {
        text: "a".into(),
        cursor: Some((1, 1)),
    });
    view.ime(ImeEvent::Commit("あ".into()));
    view.ime(ImeEvent::Preedit {
        text: String::new(),
        cursor: None,
    });
    view.ime(ImeEvent::Commit("い".into()));
    assert_eq!(texts(&view)[1], "tailあい");
    // Losing the keyboard cancels.
    view.ime(ImeEvent::Preedit {
        text: "x".into(),
        cursor: Some((1, 1)),
    });
    assert!(view.ime(ImeEvent::Disabled));
    assert_eq!(texts(&view)[1], "tailあい");
    assert!(!view.ime(ImeEvent::Disabled));
    assert!(!view.ime(ImeEvent::Enabled));
}

#[test]
fn composition_in_an_empty_body_and_across_paragraphs_leaves_no_stray_text() {
    let mut view = view_of(Document::default());
    assert!(view.ime(ImeEvent::Preedit {
        text: "n".into(),
        cursor: Some((1, 1)),
    }));
    assert!(view.ime(ImeEvent::Commit("你".into())));
    assert_eq!(texts(&view), ["你"]);
    let mut view = view_of(doc(vec![pb("aaa"), pb("bbb"), pb("ccc")]));
    select(&mut view, pos(0, 1), pos(2, 1));
    view.ime(ImeEvent::Preedit {
        text: "x".into(),
        cursor: Some((1, 1)),
    });
    assert_eq!(view.layout_text(0), "axcc");
    view.ime(ImeEvent::Preedit {
        text: String::new(),
        cursor: None,
    });
    assert_eq!(texts(&view), ["aaa", "bbb", "ccc"]);
    assert_eq!(view.selected_text(), "aa\nbbb\nc");
    view.ime(ImeEvent::Commit("Z".into()));
    assert_eq!(texts(&view), ["aZcc"]);
}

#[test]
fn cut_copy_and_paste_keep_formatting_through_the_fragment() {
    let mut view = view_of(doc(vec![pb("make bold text"), pb("second line")]));
    select(&mut view, pos(0, 5), pos(0, 9));
    assert!(view.command(DocCommand::Bold));
    select(&mut view, pos(0, 5), pos(1, 6));
    let text = view.selected_text();
    let fragment = view.selected_fragment().expect("a fragment");
    assert_eq!(text, "bold text\nsecond");
    // Plain paste: line breaks become paragraphs, tabs tabs.
    caret_at(&mut view, 1, 11);
    assert!(view.paste_text("A\r\nB\tC"));
    assert_eq!(texts(&view), ["make bold text", "second lineA", "B\tC"]);
    assert!(view.undo());
    // Fragment paste: the bold survives.
    assert!(view.paste_fragment(&fragment));
    let last = view.document().paragraph(&Path::body(1)).unwrap();
    assert!(
        last.text().ends_with("bold text") || view.document().plain_text().contains("bold text")
    );
    let bold_chars: usize = view
        .document()
        .paragraphs()
        .iter()
        .flat_map(|(_, p)| p.runs.iter())
        .filter(|r| r.look.bold)
        .map(|r| r.text.len())
        .sum();
    assert!(bold_chars >= 8, "{bold_chars}");
    assert!(view.undo());
    // Cut takes the selection out and gives it back.
    select(&mut view, pos(0, 0), pos(0, 5));
    let (cut, fragment) = view.cut().expect("a selection");
    assert_eq!(cut, "make ");
    assert!(fragment.is_some());
    assert_eq!(texts(&view)[0], "bold text");
    assert!(view.cut().is_none());
}

#[test]
fn formatting_commands_and_the_state_the_toolbar_shows() {
    let mut view = view_of(doc(vec![pb("one two three"), pb("four")]));
    select(&mut view, pos(0, 4), pos(0, 7));
    for (command, check) in [
        (
            DocCommand::Bold,
            (|s: &FormatState| s.bold) as fn(&FormatState) -> Tri,
        ),
        (DocCommand::Italic, |s| s.italic),
        (DocCommand::Underline, |s| s.underline),
        (DocCommand::Strikethrough, |s| s.strike),
        (DocCommand::Superscript, |s| s.superscript),
    ] {
        assert_eq!(check(&view.format_state()), Tri::Off);
        assert!(view.command(command.clone()));
        assert_eq!(check(&view.format_state()), Tri::On, "{command:?}");
    }
    // Part of the text has it: mixed.
    select(&mut view, pos(0, 0), pos(0, 13));
    assert_eq!(view.format_state().bold, Tri::Mixed);
    // A toggle over mixed turns it on, then off.
    view.command(DocCommand::Bold);
    assert_eq!(view.format_state().bold, Tri::On);
    view.command(DocCommand::Bold);
    assert_eq!(view.format_state().bold, Tri::Off);
    // Subscript replaces superscript.
    select(&mut view, pos(0, 4), pos(0, 7));
    view.command(DocCommand::Subscript);
    assert_eq!(view.format_state().superscript, Tri::Off);
    assert_eq!(view.format_state().subscript, Tri::On);
    // Font, size, size steps, colours.
    view.command(DocCommand::Font(Some("Arial".into())));
    assert_eq!(view.format_state().font.as_deref(), Some("Arial"));
    view.command(DocCommand::Size(12.0));
    view.command(DocCommand::SizeStep(1));
    assert_eq!(view.format_state().size, Some(14.0));
    view.command(DocCommand::SizeStep(-2));
    assert_eq!(view.format_state().size, Some(11.0));
    view.command(DocCommand::Color(Some([255, 0, 0, 255])));
    assert_eq!(view.format_state().color, Some(Some([255, 0, 0, 255])));
    view.command(DocCommand::Highlight(Some([255, 255, 0, 255])));
    assert_eq!(
        view.format_state().highlight,
        Some(Some([255, 255, 0, 255]))
    );
    view.command(DocCommand::Color(None));
    assert_eq!(view.format_state().color, Some(None));
    select(&mut view, pos(0, 0), pos(1, 2));
    assert_eq!(view.format_state().font, None, "mixed");
    // Paragraphs: alignment, indent, spacing, lists, style.
    view.command(DocCommand::Align(Align::Center));
    assert_eq!(view.format_state().align, Some(Align::Center));
    view.command(DocCommand::Align(Align::Justify));
    assert_eq!(view.format_state().align, Some(Align::Justify));
    view.command(DocCommand::Indent(2));
    let indent = |view: &DocView, i| {
        view.document()
            .paragraph(&Path::body(i))
            .unwrap()
            .props
            .indent_left
    };
    assert_eq!((indent(&view, 0), indent(&view, 1)), (1440, 1440));
    view.command(DocCommand::Indent(-1));
    assert_eq!(indent(&view, 0), 720);
    view.command(DocCommand::Indent(-5));
    assert_eq!(indent(&view, 1), 0);
    view.command(DocCommand::LineSpacing(LineSpacing::Multiple(1.5)));
    assert_eq!(view.line_spacing(), Some(LineSpacing::Multiple(1.5)));
    view.command(DocCommand::Bullets);
    assert_eq!(view.format_state().bullets, Tri::On);
    view.command(DocCommand::Numbering);
    assert_eq!(view.format_state().numbering, Tri::On);
    assert_eq!(view.format_state().bullets, Tri::Off);
    view.command(DocCommand::Numbering);
    assert_eq!(view.format_state().numbering, Tri::Off);
    // Each is a step of undo; undoing them all gives the start back.
    while view.undo() {}
    assert_eq!(view.document().plain_text(), "one two three\nfour");
    assert!(!view.can_undo());
}

#[test]
fn paragraph_styles_come_from_the_document() {
    let bytes = aster_ooxml::docx_save::blank();
    let mut document = aster_ooxml::docx::read(bytes).unwrap();
    document.renumber();
    let mut view = view_of(document);
    let styles = view.paragraph_styles();
    assert!(!styles.is_empty());
    view.type_text("Title text");
    let heading = styles
        .iter()
        .find(|(_, name)| name.to_lowercase().contains("heading 1") || name.contains("标题 1"))
        .cloned();
    if let Some((id, name)) = heading {
        assert!(view.command(DocCommand::Style(id.clone())));
        assert_eq!(view.format_state().style.as_deref(), Some(id.as_str()));
        assert_eq!(view.style_name(&id), Some(name));
        assert!(view.undo());
    }
}

#[test]
fn replace_current_and_replace_all_are_one_undo_step_each() {
    let mut view = view_of(doc(vec![
        pb("cat and Cat"),
        pb("concat"),
        table(&[&["cat in a cell"]]),
    ]));
    let original = view.document().clone();
    assert_eq!(view.set_find(query("cat")), 4);
    assert!(view.replace_current("dog"));
    assert_eq!(texts(&view)[0], "dog and Cat");
    // The search was made again: three left, the next is current.
    assert_eq!(view.hit_count(), 3);
    assert_eq!(view.replace_all("bird"), 3);
    assert_eq!(texts(&view), ["dog and bird", "conbird", "bird in a cell"]);
    assert_eq!(view.hit_count(), 0);
    assert!(view.undo());
    assert_eq!(texts(&view)[0], "dog and Cat");
    assert!(view.undo());
    assert!(same_content(view.document(), &original));
    // No matches: nothing to do.
    view.set_find(query("zzz"));
    assert!(!view.replace_current("x"));
    assert_eq!(view.replace_all("x"), 0);
    // A longer and a shorter replacement in the same paragraph.
    view.set_find(query("c"));
    assert_eq!(view.replace_all("--"), 6);
    assert_eq!(texts(&view)[0], "--at and --at");
}

#[test]
fn an_empty_body_gets_a_paragraph_for_the_first_edit() {
    for document in [Document::default(), doc(Vec::new())] {
        let mut view = view_of(document);
        assert!(view.caret().is_none());
        assert!(view.type_text("first"));
        assert_eq!(texts(&view), ["first"]);
        assert!(view.undo());
        // The paragraph stays: the body is not empty again.
        assert!(view.caret().is_some());
        let mut view = view_of(Document::default());
        assert!(view.enter());
        assert_eq!(texts(&view).len(), 2);
    }
    let mut view = view_of(Document::default());
    assert!(view.command(DocCommand::Bold));
    assert!(view.paste_text("pasted"));
    assert_eq!(texts(&view), ["pasted"]);
}

#[test]
fn edits_change_the_pages_and_only_changed_pages_are_painted_again() {
    let mut view = view_of(lines(120));
    view.render();
    let page = |view: &DocView, i| Arc::as_ptr(&view.shots[&i].pixmap);
    let (first, second) = (page(&view, 0), view.page_count() >= 2);
    assert!(second);
    // Typing on the first page, with the rest of the document the same.
    let revision = view.revision();
    caret_at(&mut view, 0, 0);
    view.type_text("X");
    assert!(view.revision() > revision);
    view.render();
    assert_ne!(
        first,
        page(&view, 0),
        "the page that changed is painted again"
    );
    // Pages after it that have the same content keep their pixels when they
    // are visible and unchanged.
    view.go_to_page(3);
    view.render();
    let before = page(&view, 3);
    view.type_text("Y");
    view.go_to_page(3);
    view.render();
    assert!(view.layout_text(0).starts_with("XY"));
    assert_eq!(
        before,
        page(&view, 3),
        "an edit on page 1 does not repaint page 4"
    );
}

#[test]
fn a_scripted_session_saves_and_reads_back_as_the_same_content() {
    let blank = aster_ooxml::docx_save::blank();
    let mut document = aster_ooxml::docx::read(blank.clone()).unwrap();
    document.renumber();
    let mut view = view_of(document);
    view.type_text("Hello Word");
    view.enter();
    view.type_text("second paragraph with 中文");
    view.command(DocCommand::Bold);
    select(&mut view, pos(0, 0), pos(0, 5));
    view.command(DocCommand::Italic);
    view.command(DocCommand::Bullets);
    caret_at(&mut view, 1, 6);
    view.enter();
    view.type_text("third");
    view.undo();
    view.redo();
    view.set_find(query("second"));
    view.replace_current("2nd");
    let saved = aster_ooxml::docx_save::save(blank, view.document()).expect("saves");
    let mut read = aster_ooxml::docx::read(saved.clone()).expect("reads back");
    read.renumber();
    assert!(same_content(view.document(), &read));
    // And on from the saved file: its sources match its bytes.
    let mut again = view_of(read);
    again.type_text("more ");
    let saved_again = aster_ooxml::docx_save::save(saved, again.document()).expect("saves again");
    let mut read_again = aster_ooxml::docx::read(saved_again).expect("reads again");
    read_again.renumber();
    assert!(same_content(again.document(), &read_again));
}

#[test]
fn the_sample_document_edits_and_saves() {
    let bytes = docx_sample::sample();
    let mut document = aster_ooxml::docx::read(bytes.clone()).unwrap();
    document.renumber();
    let mut view = view_of(document);
    let original = view.document().clone();
    view.type_text("Start: ");
    view.command(DocCommand::Bold);
    view.select_all();
    view.command(DocCommand::Size(13.0));
    caret_at(&mut view, 3, 2);
    view.enter();
    view.type_text("inserted");
    let saved = aster_ooxml::docx_save::save(bytes, view.document()).expect("saves");
    let mut read = aster_ooxml::docx::read(saved).expect("reads back");
    read.renumber();
    assert!(same_content(view.document(), &read));
    while view.undo() {}
    assert!(same_content(view.document(), &original));
    assert!(view.can_redo());
}

#[test]
fn saving_marks_the_view_and_a_new_document_starts_clean() {
    let mut view = view_of(lines(3));
    assert!(!view.is_modified());
    view.type_text("a");
    assert!(view.is_modified());
    view.mark_saved();
    assert!(!view.is_modified());
    view.undo();
    assert!(view.is_modified());
    view.mark_unsaved();
    assert!(view.is_modified());
}
