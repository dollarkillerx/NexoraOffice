use super::*;
use crate::{
    Cell, Drawing, DrawingContent, Label, ParaProps, ParaSource, Row, RunSource, Section, Story,
    Style, StyleKind, StyleSheet, Table,
};

fn look() -> RunLook {
    RunLook::default()
}

fn bold() -> RunLook {
    RunLook {
        bold: true,
        ..look()
    }
}

fn marker() -> Run {
    Run::object(Inline::Marker, look())
}

fn text_para(text: &str) -> Paragraph {
    Paragraph {
        runs: vec![Run::text(text, look())],
        ..Paragraph::default()
    }
}

/// The paragraph as if it had been read: every run and the paragraph have a
/// source describing them as they are.
fn read(mut p: Paragraph) -> Paragraph {
    let mut at = 100;
    let mut sources = Vec::new();
    for run in &mut p.runs {
        let source = Arc::new(RunSource {
            range: at..at + 10,
            parent: None,
            text: run.text.clone(),
            look: run.look.clone(),
            style: run.style.clone(),
            link: run.link.clone(),
            annotation: run.annotation.clone(),
        });
        at += 10;
        run.source = Some(source.clone());
        sources.push(source);
    }
    p.source = Some(Arc::new(ParaSource {
        range: 0..at + 10,
        runs: sources,
        props: p.props.clone(),
        mark: p.mark.clone(),
        section: p.section.clone(),
        changes: p.changes.clone(),
    }));
    p
}

fn doc_of(paragraphs: Vec<Paragraph>) -> Document {
    Document {
        body: Story {
            blocks: paragraphs.into_iter().map(Block::paragraph).collect(),
            part: None,
        },
        ..Document::default()
    }
}

fn pos(block: usize, offset: usize) -> Pos {
    Pos::new(Path::body(block), offset)
}

fn texts(doc: &Document) -> Vec<String> {
    doc.paragraphs().iter().map(|(_, p)| p.text()).collect()
}

fn p(doc: &Document, block: usize) -> &Paragraph {
    doc.paragraph(&Path::body(block)).unwrap()
}

fn cell(texts: &[&str]) -> Cell {
    Cell {
        blocks: texts
            .iter()
            .map(|t| Block::paragraph(text_para(t)))
            .collect(),
        ..Cell::default()
    }
}

type Texts<'a> = &'a [&'a str];

fn table(rows: &[&[Texts]]) -> Block {
    let rows = rows
        .iter()
        .map(|cells| Row {
            cells: cells.iter().map(|c| cell(c)).collect(),
            ..Row::default()
        })
        .collect();
    Block::table(Table {
        columns: vec![1000; 2],
        rows,
        ..Table::default()
    })
}

fn cell_pos(table: usize, row: usize, cell: usize, block: usize, offset: usize) -> Pos {
    Pos::new(Path::body(table).cell_block(row, cell, block), offset)
}

fn styles() -> Arc<StyleSheet> {
    let normal = Style {
        id: Arc::from("Normal"),
        name: "Normal".to_owned(),
        kind: StyleKind::Paragraph,
        based_on: None,
        next: None,
        para: ParaProps {
            style: Some(Arc::from("Normal")),
            ..ParaProps::default()
        },
        run: look(),
        hidden: false,
        quick: true,
        priority: 0,
    };
    let heading = Style {
        id: Arc::from("Heading1"),
        name: "heading 1".to_owned(),
        next: Some(Arc::from("Normal")),
        para: ParaProps {
            style: Some(Arc::from("Heading1")),
            space_before: 240,
            keep_next: true,
            ..ParaProps::default()
        },
        run: RunLook {
            size: 16.0,
            bold: true,
            ..look()
        },
        ..normal.clone()
    };
    Arc::new(StyleSheet {
        styles: vec![normal, heading],
        default_para: ParaProps::default(),
        default_run: look(),
        default_style: Some(Arc::from("Normal")),
    })
}

// ---------------------------------------------------------------------------
// Typing
// ---------------------------------------------------------------------------

#[test]
fn typing_extends_the_run_it_is_typed_into() {
    let mut doc = doc_of(vec![read(text_para("hello"))]);
    let at = insert_text(&mut doc, &pos(0, 5), "!", None);
    assert_eq!(at, pos(0, 6));
    assert_eq!(p(&doc, 0).runs.len(), 1);
    assert!(p(&doc, 0).runs[0].source.is_some());
    assert_eq!(p(&doc, 0).text(), "hello!");
    // Typed into the middle and at the start with the same look.
    insert_text(&mut doc, &pos(0, 0), ">", None);
    insert_text(&mut doc, &pos(0, 3), "-", Some(&look()));
    assert_eq!(p(&doc, 0).text(), ">he-llo!");
    assert_eq!(p(&doc, 0).runs.len(), 1);
}

#[test]
fn a_look_that_differs_makes_a_run_of_its_own() {
    let mut doc = doc_of(vec![read(text_para("abcd"))]);
    let at = insert_text(&mut doc, &pos(0, 2), "XY", Some(&bold()));
    assert_eq!(at, pos(0, 4));
    let runs = &p(&doc, 0).runs;
    assert_eq!(
        runs.iter().map(|r| r.text.as_str()).collect::<Vec<_>>(),
        ["ab", "XY", "cd"]
    );
    assert!(runs[1].look.bold && runs[1].source.is_none());
    assert!(runs[0].source.is_some() && runs[2].source.is_some());
    assert!(!p(&doc, 0).is_as_read());
}

#[test]
fn typing_a_character_and_deleting_it_gives_the_paragraph_back() {
    for look_arg in [None, Some(bold())] {
        let mut doc = doc_of(vec![read(text_para("abcd"))]);
        let before = doc.clone();
        for offset in [0, 2, 4] {
            let at = insert_text(&mut doc, &pos(0, offset), "x", look_arg.as_ref());
            let back = delete_backward(&mut doc, &at);
            assert_eq!(back, pos(0, offset));
            assert!(
                p(&doc, 0).is_as_read(),
                "offset {offset}, look {look_arg:?}"
            );
            assert!(same_content(&doc, &before));
        }
    }
}

#[test]
fn multi_byte_text_works_at_every_char_boundary() {
    let sample = "中文日本語😀éa";
    for (offset, _) in sample.char_indices().chain([(sample.len(), ' ')]) {
        let mut doc = doc_of(vec![read(text_para(sample))]);
        let at = insert_text(&mut doc, &pos(0, offset), "あ", None);
        assert_eq!(at.offset, offset + 3);
        let mut expected = sample.to_owned();
        expected.insert(offset, 'あ');
        assert_eq!(p(&doc, 0).text(), expected);
        // Backspace takes the whole char before the caret.
        let back = delete_backward(&mut doc, &at);
        assert_eq!(back.offset, offset);
        assert_eq!(p(&doc, 0).text(), sample);
        // Delete takes the whole char after it.
        if offset < sample.len() {
            delete_forward(&mut doc, &pos(0, offset));
            let mut rest = sample.to_owned();
            rest.remove(offset);
            assert_eq!(p(&doc, 0).text(), rest);
        }
    }
}

#[test]
fn offsets_inside_a_char_are_moved_to_a_boundary() {
    let mut doc = doc_of(vec![text_para("é中")]);
    // Byte 1 is inside é, 4 inside 中.
    insert_text(&mut doc, &pos(0, 1), "x", None);
    assert_eq!(p(&doc, 0).text(), "xé中");
    insert_text(&mut doc, &pos(0, 5), "y", None);
    assert_eq!(p(&doc, 0).text(), "xéy中");
    delete(&mut doc, &pos(0, 1), &pos(0, 2));
    assert_eq!(p(&doc, 0).text(), "xy中");
}

#[test]
fn newlines_tabs_and_control_characters() {
    let mut doc = doc_of(vec![text_para("ab")]);
    let at = insert_text(&mut doc, &pos(0, 1), "1\n2\t3\u{7}\r\n4\u{FFFC}", None);
    assert_eq!(texts(&doc), ["a1", "2\t3", "4b"]);
    assert_eq!(at, pos(2, 1));
    let runs = &p(&doc, 1).runs;
    assert!(runs.iter().any(|r| r.kind == Inline::Tab));
}

#[test]
fn bad_positions_do_nothing() {
    let mut doc = doc_of(vec![text_para("ab")]);
    let before = doc.clone();
    let bad = Pos::new(Path::from(vec![5, 1, 1, 1]), 3);
    assert_eq!(insert_text(&mut doc, &bad, "x", None), bad);
    split_paragraph(&mut doc, &bad);
    delete(&mut doc, &bad, &pos(0, 1));
    delete_backward(&mut doc, &bad);
    delete_forward(&mut doc, &bad);
    insert_break(&mut doc, &bad, Break::Line);
    apply_run_patch(&mut doc, &bad, &pos(0, 1), &make_bold());
    set_align(&mut doc, &bad, &bad, Align::Center);
    set_style(&mut doc, &bad, &bad, "Heading1");
    set_list(&mut doc, &bad, &bad, Some(ListKind::Bullet));
    assert_eq!(plain_text(&doc, &bad, &bad), "");
    assert!(fragment(&doc, &bad, &bad).is_empty());
    assert!(same_content(&doc, &before));
    assert!(doc.body.blocks[0].is(&before.body.blocks[0]));
    // A path with a bad step in a table, and a block that is a table.
    doc.body.blocks.push(table(&[&[&["a"], &["b"]]]));
    for path in [vec![1], vec![1, 0, 5, 0], vec![1, 9, 0, 0], vec![]] {
        let bad = Pos::new(Path::from(path), 0);
        insert_text(&mut doc, &bad, "x", None);
        delete(&mut doc, &bad, &bad);
        split_paragraph(&mut doc, &bad);
    }
    assert_eq!(doc.plain_text(), "ab\na\nb");
    // An offset past the end is the end.
    let at = insert_text(&mut doc, &pos(0, 99), "z", None);
    assert_eq!(at, pos(0, 3));
    assert_eq!(p(&doc, 0).text(), "abz");
}

#[test]
fn a_break_is_one_object() {
    let mut doc = doc_of(vec![text_para("ab")]);
    let at = insert_break(&mut doc, &pos(0, 1), Break::Page);
    assert_eq!(at, pos(0, 1 + 3));
    insert_break(&mut doc, &at, Break::Line);
    assert_eq!(p(&doc, 0).text(), format!("a{}\nb", crate::OBJECT));
    // Offsets inside the page break snap to its start.
    insert_text(&mut doc, &pos(0, 2), "x", None);
    assert_eq!(p(&doc, 0).text(), format!("ax{}\nb", crate::OBJECT));
}

// ---------------------------------------------------------------------------
// Splitting
// ---------------------------------------------------------------------------

#[test]
fn enter_splits_the_paragraph_and_keeps_the_rest_shared() {
    let mut doc = doc_of(vec![
        text_para("first"),
        read(Paragraph {
            runs: vec![Run::text("abc", look()), Run::text("def", bold())],
            props: ParaProps {
                align: Align::Center,
                ..ParaProps::default()
            },
            ..Paragraph::default()
        }),
        text_para("last"),
    ]);
    let before = doc.clone();
    let at = split_paragraph(&mut doc, &pos(1, 4));
    assert_eq!(at, pos(2, 0));
    assert_eq!(texts(&doc), ["first", "abcd", "ef", "last"]);
    assert_eq!(p(&doc, 2).props.align, Align::Center);
    assert!(p(&doc, 1).runs[1].source.is_some());
    assert!(p(&doc, 2).runs[0].source.is_some());
    assert!(doc.body.blocks[0].is(&before.body.blocks[0]));
    assert!(doc.body.blocks[3].is(&before.body.blocks[2]));
}

#[test]
fn the_section_goes_with_the_end_of_the_paragraph() {
    let section = Arc::new(Section::default());
    let mut doc = doc_of(vec![Paragraph {
        section: Some(section.clone()),
        ..text_para("abc")
    }]);
    split_paragraph(&mut doc, &pos(0, 1));
    assert!(p(&doc, 0).section.is_none());
    assert!(Arc::ptr_eq(p(&doc, 1).section.as_ref().unwrap(), &section));
}

fn shape(p: &Paragraph) -> String {
    p.runs
        .iter()
        .map(|r| if r.kind == Inline::Marker { 'M' } else { 't' })
        .collect()
}

#[test]
fn markers_stay_on_their_side_of_a_split() {
    let make = || {
        doc_of(vec![Paragraph {
            runs: vec![
                marker(),
                Run::text("ab", look()),
                marker(),
                Run::text("cd", look()),
                marker(),
            ],
            ..Paragraph::default()
        }])
    };
    let mut doc = make();
    split_paragraph(&mut doc, &pos(0, 0));
    assert_eq!(
        (shape(p(&doc, 0)), shape(p(&doc, 1))),
        (String::new(), "MtMtM".into())
    );
    let mut doc = make();
    split_paragraph(&mut doc, &pos(0, 4));
    assert_eq!(
        (shape(p(&doc, 0)), shape(p(&doc, 1))),
        ("MtMtM".into(), String::new())
    );
    let mut doc = make();
    split_paragraph(&mut doc, &pos(0, 2));
    assert_eq!(
        (shape(p(&doc, 0)), shape(p(&doc, 1))),
        ("Mt".into(), "MtM".into())
    );
}

#[test]
fn enter_at_the_end_of_a_heading_gives_the_next_style() {
    let mut doc = doc_of(vec![]);
    doc.styles = styles();
    let heading = doc.styles.get("Heading1").unwrap().clone();
    doc.body.blocks.push(Block::paragraph(Paragraph {
        runs: vec![Run::text("Title", heading.run.clone())],
        props: heading.para.clone(),
        base: heading.run.clone(),
        mark: heading.run.clone(),
        ..Paragraph::default()
    }));
    let mut middle = doc.clone();
    split_paragraph(&mut doc, &pos(0, 5));
    let next = p(&doc, 1);
    assert_eq!(next.props.style.as_deref(), Some("Normal"));
    assert_eq!(next.base, look());
    assert_eq!(next.props.space_before, 0);
    // In the middle both halves stay headings.
    split_paragraph(&mut middle, &pos(0, 2));
    assert_eq!(p(&middle, 1).props.style.as_deref(), Some("Heading1"));
}

#[test]
fn enter_in_an_empty_list_item_leaves_the_list() {
    let mut doc = doc_of(vec![text_para("one"), text_para("")]);
    set_list(&mut doc, &pos(0, 0), &pos(1, 0), Some(ListKind::Numbered));
    assert_eq!(p(&doc, 1).label.as_ref().unwrap().text, "2.");
    let at = split_paragraph(&mut doc, &pos(1, 0));
    assert_eq!(at, pos(1, 0));
    assert_eq!(doc.body.blocks.len(), 2);
    assert!(p(&doc, 1).props.list.is_none() && p(&doc, 1).label.is_none());
    assert_eq!(p(&doc, 1).props.indent_left, 0);
    // Enter in a non-empty item continues the list.
    split_paragraph(&mut doc, &pos(0, 3));
    assert_eq!(p(&doc, 1).label.as_ref().unwrap().text, "2.");
}

// ---------------------------------------------------------------------------
// Deleting
// ---------------------------------------------------------------------------

fn drawing() -> Run {
    Run::object(
        Inline::Drawing(Arc::new(Drawing {
            size: (1, 1),
            content: DrawingContent::Other("shape".into()),
            float: None,
            alt: None,
        })),
        look(),
    )
}

#[test]
fn deleting_within_a_paragraph_keeps_markers_and_removes_objects_whole() {
    let mut doc = doc_of(vec![Paragraph {
        runs: vec![
            Run::text("ab", look()),
            marker(),
            Run::text("cd", look()),
            drawing(),
            Run::text("ef", look()),
        ],
        ..Paragraph::default()
    }]);
    // From inside "ab" to the middle of the drawing's char: the drawing goes.
    let at = delete(&mut doc, &pos(0, 1), &pos(0, 5));
    assert_eq!(at, pos(0, 1));
    assert_eq!(p(&doc, 0).text(), "aef");
    assert_eq!(
        p(&doc, 0)
            .runs
            .iter()
            .filter(|r| r.kind == Inline::Marker)
            .count(),
        1
    );
    // A reference alone is removed when the range covers it.
    let mut doc = doc_of(vec![Paragraph {
        runs: vec![
            Run::text("a", look()),
            Run::object(Inline::Footnote(1), look()),
            Run::text("b", look()),
        ],
        ..Paragraph::default()
    }]);
    delete(&mut doc, &pos(0, 1), &pos(0, 4));
    assert_eq!(p(&doc, 0).text(), "ab");
    // Part of a page number goes whole.
    let mut number = Run::object(Inline::PageNumber(crate::PageField::Page), look());
    number.text = "12".to_owned();
    let mut doc = doc_of(vec![Paragraph {
        runs: vec![Run::text("x", look()), number, Run::text("y", look())],
        ..Paragraph::default()
    }]);
    delete(&mut doc, &pos(0, 2), &pos(0, 3));
    assert_eq!(p(&doc, 0).text(), "xy");
}

#[test]
fn joining_paragraphs_keeps_markers_properties_and_the_second_section() {
    let section = Arc::new(Section::default());
    let first = Paragraph {
        runs: vec![Run::text("one", look()), marker()],
        props: ParaProps {
            align: Align::Right,
            ..ParaProps::default()
        },
        section: Some(Arc::new(Section {
            page: (1, 1),
            ..Section::default()
        })),
        ..Paragraph::default()
    };
    let middle = Paragraph {
        runs: vec![marker(), Run::text("mid", look())],
        section: Some(Arc::new(Section {
            page: (2, 2),
            ..Section::default()
        })),
        ..Paragraph::default()
    };
    let last = Paragraph {
        runs: vec![Run::text("xtwo", look()), marker()],
        section: Some(section.clone()),
        ..Paragraph::default()
    };
    let mut doc = doc_of(vec![
        text_para("before"),
        first,
        middle,
        last,
        text_para("after"),
    ]);
    let untouched = doc.clone();
    let at = delete(&mut doc, &pos(1, 2), &pos(3, 1));
    assert_eq!(at, pos(1, 2));
    assert_eq!(texts(&doc), ["before", "ontwo", "after"]);
    let joined = p(&doc, 1);
    assert_eq!(joined.props.align, Align::Right);
    assert!(Arc::ptr_eq(joined.section.as_ref().unwrap(), &section));
    // Markers: the first's, the middle one's, the last's, in order.
    let order: Vec<bool> = joined
        .runs
        .iter()
        .map(|r| r.kind == Inline::Marker)
        .collect();
    assert_eq!(order.iter().filter(|m| **m).count(), 3);
    assert_eq!(order.first(), Some(&false));
    assert_eq!(order.last(), Some(&true));
    assert!(doc.body.blocks[0].is(&untouched.body.blocks[0]));
    assert!(doc.body.blocks[2].is(&untouched.body.blocks[4]));
}

#[test]
fn backspace_and_delete_join_paragraphs_and_stop_at_tables() {
    let mut doc = doc_of(vec![text_para("ab"), text_para("cd")]);
    doc.body.blocks.push(table(&[&[&["t1"], &["t2", "t3"]]]));
    doc.body.blocks.push(Block::paragraph(text_para("after")));
    // Backspace at the start of "cd" joins.
    let at = delete_backward(&mut doc, &pos(1, 0));
    assert_eq!(at, pos(0, 2));
    assert_eq!(p(&doc, 0).text(), "abcd");
    // Backspace at the start of the paragraph after a table: nothing.
    let before = doc.clone();
    delete_backward(&mut doc, &pos(2, 0));
    assert!(same_content(&doc, &before));
    // Delete at the end of the paragraph before the table: nothing.
    delete_forward(&mut doc, &pos(0, 4));
    assert!(same_content(&doc, &before));
    // Cell boundaries: nothing at the start of a cell's first paragraph or
    // the end of its last.
    delete_backward(&mut doc, &cell_pos(1, 0, 1, 0, 0));
    delete_forward(&mut doc, &cell_pos(1, 0, 0, 0, 2));
    assert!(same_content(&doc, &before));
    // Inside a cell paragraphs join.
    delete_backward(&mut doc, &cell_pos(1, 0, 1, 1, 0));
    assert_eq!(
        doc.paragraph(&Path::body(1).cell_block(0, 1, 0))
            .unwrap()
            .text(),
        "t2t3"
    );
    // Delete at the end of the last paragraph does nothing either.
    let last = Pos::new(Path::body(2), 5);
    let before = doc.clone();
    delete_forward(&mut doc, &last);
    assert!(same_content(&doc, &before));
}

#[test]
fn backspace_at_the_start_of_a_list_item_leaves_the_list_first() {
    let mut doc = doc_of(vec![text_para("one"), text_para("two")]);
    set_list(&mut doc, &pos(0, 0), &pos(1, 0), Some(ListKind::Bullet));
    assert_eq!(p(&doc, 1).label.as_ref().unwrap().text, "•");
    delete_backward(&mut doc, &pos(1, 0));
    assert_eq!(doc.body.blocks.len(), 2);
    assert!(p(&doc, 1).props.list.is_none());
    delete_backward(&mut doc, &pos(1, 0));
    assert_eq!(texts(&doc), ["onetwo"]);
}

#[test]
fn a_range_inside_one_cell_joins_like_the_body() {
    let mut doc = doc_of(vec![text_para("x")]);
    doc.body
        .blocks
        .push(table(&[&[&["aa", "bb", "cc"], &["d"]]]));
    let at = delete(&mut doc, &cell_pos(1, 0, 0, 0, 1), &cell_pos(1, 0, 0, 2, 1));
    assert_eq!(at, cell_pos(1, 0, 0, 0, 1));
    let cell = &doc.block(&Path::body(1)).unwrap().as_table().unwrap().rows[0].cells[0];
    assert_eq!(cell.blocks.len(), 1);
    assert_eq!(cell.blocks[0].as_paragraph().unwrap().text(), "ac");
}

#[test]
fn a_range_over_several_cells_only_empties_text() {
    let mut doc = doc_of(vec![text_para("before")]);
    doc.body
        .blocks
        .push(table(&[&[&["a1"], &["b1", "b2"]], &[&["c1"], &["d1"]]]));
    doc.body.blocks.push(Block::paragraph(text_para("after")));
    delete(&mut doc, &cell_pos(1, 0, 0, 0, 1), &cell_pos(1, 1, 1, 0, 1));
    let t = doc.block(&Path::body(1)).unwrap().as_table().unwrap();
    assert_eq!(t.rows.len(), 2);
    let got: Vec<Vec<String>> = t
        .rows
        .iter()
        .flat_map(|r| &r.cells)
        .map(|c| {
            c.blocks
                .iter()
                .map(|b| b.as_paragraph().unwrap().text())
                .collect()
        })
        .collect();
    assert_eq!(got, vec![vec!["a"], vec!["", ""], vec![""], vec!["1"]]);
    assert_eq!(texts(&doc)[0], "before");
    assert_eq!(*texts(&doc).last().unwrap(), "after");
}

#[test]
fn a_range_from_outside_a_table_into_it_never_joins() {
    let mut doc = doc_of(vec![text_para("one"), text_para("two")]);
    doc.body.blocks.push(table(&[&[&["a1", "a2"], &["b1"]]]));
    doc.body.blocks.push(Block::paragraph(text_para("tail")));
    delete(&mut doc, &pos(0, 1), &cell_pos(2, 0, 0, 1, 1));
    // "two" (between) is removed, the table stays, nothing is joined.
    assert_eq!(texts(&doc), ["o", "", "2", "b1", "tail"]);
    assert!(doc.body.blocks[1].as_table().is_some());
}

#[test]
fn a_table_inside_the_range_is_removed() {
    let mut doc = doc_of(vec![text_para("one")]);
    let mut marked = table(&[&[&["gone"], &["x"]]]);
    if let Block::Table(t) = &mut marked
        && let Block::Paragraph(p) = &mut Arc::make_mut(t).rows[0].cells[0].blocks[0]
    {
        Arc::make_mut(p).runs.push(marker());
    }
    doc.body.blocks.push(marked);
    doc.body.blocks.push(Block::paragraph(text_para("two")));
    let at = delete(&mut doc, &pos(0, 1), &pos(2, 1));
    assert_eq!(at, pos(0, 1));
    assert_eq!(doc.body.blocks.len(), 1);
    assert_eq!(p(&doc, 0).text(), "owo");
    // The marker of the removed table is kept.
    assert_eq!(
        p(&doc, 0)
            .runs
            .iter()
            .filter(|r| r.kind == Inline::Marker)
            .count(),
        1
    );
}

#[test]
fn a_nested_table_inside_the_range_is_removed_and_the_cell_keeps_a_paragraph() {
    let inner = table(&[&[&["n1"], &["n2"]]]);
    let mut outer_cell = cell(&["head"]);
    outer_cell.blocks.push(inner);
    let mut other = cell(&["next"]);
    other.blocks.push(Block::paragraph(text_para("more")));
    let mut doc = doc_of(vec![]);
    doc.body.blocks.push(Block::table(Table {
        columns: vec![1000, 1000],
        rows: vec![Row {
            cells: vec![outer_cell, other],
            ..Row::default()
        }],
        ..Table::default()
    }));
    delete(&mut doc, &cell_pos(0, 0, 0, 0, 1), &cell_pos(0, 0, 1, 0, 2));
    let t = doc.block(&Path::body(0)).unwrap().as_table().unwrap();
    let first = &t.rows[0].cells[0];
    assert!(first.blocks.iter().all(|b| b.as_paragraph().is_some()));
    assert_eq!(first.blocks[0].as_paragraph().unwrap().text(), "h");
    assert_eq!(
        t.rows[0].cells[1].blocks[0].as_paragraph().unwrap().text(),
        "xt"
    );
}

#[test]
fn a_backwards_range_works_too() {
    let mut doc = doc_of(vec![text_para("abc"), text_para("def")]);
    let at = delete(&mut doc, &pos(1, 2), &pos(0, 1));
    assert_eq!(at, pos(0, 1));
    assert_eq!(texts(&doc), ["af"]);
}

// ---------------------------------------------------------------------------
// Formatting
// ---------------------------------------------------------------------------

fn make_bold() -> RunPatch {
    RunPatch {
        bold: Some(true),
        ..RunPatch::default()
    }
}

#[test]
fn a_patch_splits_runs_at_the_ends_and_merges_what_ends_up_the_same() {
    let mut doc = doc_of(vec![read(text_para("abcdef"))]);
    apply_run_patch(&mut doc, &pos(0, 2), &pos(0, 4), &make_bold());
    let runs = &p(&doc, 0).runs;
    assert_eq!(
        runs.iter()
            .map(|r| (r.text.as_str(), r.look.bold))
            .collect::<Vec<_>>(),
        [("ab", false), ("cd", true), ("ef", false)]
    );
    // Undo it by hand: the pieces become one run again, as read.
    apply_run_patch(
        &mut doc,
        &pos(0, 2),
        &pos(0, 4),
        &RunPatch {
            bold: Some(false),
            ..RunPatch::default()
        },
    );
    assert_eq!(p(&doc, 0).runs.len(), 1);
    assert!(p(&doc, 0).is_as_read());
    // Made-here neighbours merge.
    let mut doc = doc_of(vec![text_para("abc")]);
    apply_run_patch(&mut doc, &pos(0, 0), &pos(0, 1), &make_bold());
    apply_run_patch(&mut doc, &pos(0, 1), &pos(0, 3), &make_bold());
    assert_eq!(p(&doc, 0).runs.len(), 1);
    assert!(p(&doc, 0).runs[0].look.bold);
}

#[test]
fn a_patch_reaches_objects_not_markers_and_whole_paragraphs_marks() {
    let mut doc = doc_of(vec![
        Paragraph {
            runs: vec![
                Run::text("a", look()),
                Run::object(Inline::Footnote(1), look()),
                marker(),
            ],
            ..Paragraph::default()
        },
        text_para("mid"),
        text_para(""),
        text_para("end"),
    ]);
    let untouched = doc.clone();
    apply_run_patch(&mut doc, &pos(0, 0), &pos(3, 1), &make_bold());
    assert!(p(&doc, 0).runs[1].look.bold);
    assert!(!p(&doc, 0).runs[2].look.bold);
    assert!(p(&doc, 0).mark.bold && p(&doc, 1).mark.bold && p(&doc, 2).mark.bold);
    assert!(!p(&doc, 3).mark.bold && p(&doc, 3).runs[0].look.bold);
    // A patch that changes nothing leaves blocks shared.
    let mut again = doc.clone();
    apply_run_patch(&mut again, &pos(0, 0), &pos(3, 1), &make_bold());
    assert!(again.body.blocks[1].is(&doc.body.blocks[1]));
    assert!(!doc.body.blocks[1].is(&untouched.body.blocks[1]));
    // A caret in an empty paragraph sets the look of what is typed there.
    let mut empty = doc_of(vec![text_para("")]);
    apply_run_patch(&mut empty, &pos(0, 0), &pos(0, 0), &make_bold());
    let at = insert_text(&mut empty, &pos(0, 0), "x", None);
    assert_eq!(at, pos(0, 1));
    assert!(
        p(&empty, 0)
            .runs
            .iter()
            .any(|r| r.text == "x" && r.look.bold)
    );
}

#[test]
fn paragraph_formatting_changes_only_the_paragraphs_touched() {
    let mut doc = doc_of(vec![text_para("a"), text_para("b"), text_para("c")]);
    let before = doc.clone();
    set_align(&mut doc, &pos(1, 0), &pos(1, 1), Align::Center);
    assert_eq!(p(&doc, 1).props.align, Align::Center);
    assert!(doc.body.blocks[0].is(&before.body.blocks[0]));
    assert!(doc.body.blocks[2].is(&before.body.blocks[2]));
    change_indent(&mut doc, &pos(0, 0), &pos(2, 0), 2);
    assert_eq!(p(&doc, 2).props.indent_left, 1440);
    change_indent(&mut doc, &pos(0, 0), &pos(2, 0), -5);
    assert_eq!(p(&doc, 0).props.indent_left, 0);
    set_line_spacing(&mut doc, &pos(0, 0), &pos(0, 0), LineSpacing::Multiple(1.5));
    set_space_before(&mut doc, &pos(0, 0), &pos(0, 0), 120);
    set_space_after(&mut doc, &pos(0, 0), &pos(0, 0), 60);
    let props = &p(&doc, 0).props;
    assert_eq!(
        (props.line, props.space_before, props.space_after),
        (LineSpacing::Multiple(1.5), 120, 60)
    );
    // Nothing to change: still shared.
    let mut same = doc.clone();
    set_align(&mut same, &pos(1, 0), &pos(1, 0), Align::Center);
    assert!(same.body.blocks[1].is(&doc.body.blocks[1]));
}

#[test]
fn a_style_brings_its_properties_and_what_was_inherited_follows() {
    let mut doc = doc_of(vec![Paragraph {
        runs: vec![
            Run::text("plain", look()),
            Run::text(
                "set",
                RunLook {
                    size: 20.0,
                    italic: true,
                    ..look()
                },
            ),
        ],
        ..Paragraph::default()
    }]);
    doc.styles = styles();
    set_style(&mut doc, &pos(0, 0), &pos(0, 0), "Heading1");
    let para = p(&doc, 0);
    assert_eq!(para.props.style.as_deref(), Some("Heading1"));
    assert_eq!(para.props.space_before, 240);
    assert_eq!(para.base.size, 16.0);
    // "plain" had the base look: it follows. "set" set size and italic
    // itself; the size stays, bold (inherited) follows.
    assert_eq!(para.runs[0].look, para.base);
    assert_eq!(para.runs[1].look.size, 20.0);
    assert!(para.runs[1].look.italic && para.runs[1].look.bold);
    // Back to Normal.
    set_style(&mut doc, &pos(0, 0), &pos(0, 0), "Normal");
    assert_eq!(p(&doc, 0).runs[0].look, look());
    assert_eq!(p(&doc, 0).props.space_before, 0);
    // An unknown style does nothing.
    let before = doc.clone();
    set_style(&mut doc, &pos(0, 0), &pos(0, 0), "Nope");
    assert!(doc.body.blocks[0].is(&before.body.blocks[0]));
}

// ---------------------------------------------------------------------------
// Lists
// ---------------------------------------------------------------------------

fn labels(doc: &Document) -> Vec<Option<String>> {
    doc.paragraphs()
        .iter()
        .map(|(_, p)| p.label.as_ref().map(|l: &Label| l.text.clone()))
        .collect()
}

fn some(items: &[&str]) -> Vec<Option<String>> {
    items
        .iter()
        .map(|s| (!s.is_empty()).then(|| (*s).to_owned()))
        .collect()
}

#[test]
fn lists_number_after_splits_and_joins() {
    let mut doc = doc_of(vec![
        text_para("a"),
        text_para("b"),
        text_para("c"),
        text_para("d"),
    ]);
    set_list(&mut doc, &pos(0, 0), &pos(2, 0), Some(ListKind::Numbered));
    assert_eq!(labels(&doc), some(&["1.", "2.", "3.", ""]));
    assert_eq!(p(&doc, 0).props.indent_left, 720);
    assert_eq!(p(&doc, 0).props.first_line, -360);
    split_paragraph(&mut doc, &pos(1, 1));
    assert_eq!(labels(&doc), some(&["1.", "2.", "3.", "4.", ""]));
    delete(&mut doc, &pos(0, 1), &pos(1, 0));
    assert_eq!(labels(&doc), some(&["1.", "2.", "3.", ""]));
    // The paragraph after the list continues it.
    set_list(&mut doc, &pos(3, 0), &pos(3, 0), Some(ListKind::Numbered));
    assert_eq!(p(&doc, 3).label.as_ref().unwrap().text, "4.");
    assert_eq!(doc.numbering.lists.len(), 1);
}

#[test]
fn list_kinds_levels_and_removal() {
    let mut doc = doc_of(vec![text_para("a"), text_para("b"), text_para("c")]);
    set_list(&mut doc, &pos(0, 0), &pos(2, 0), Some(ListKind::Bullet));
    assert_eq!(labels(&doc), some(&["•", "•", "•"]));
    change_level(&mut doc, &pos(1, 0), &pos(1, 0), 1);
    change_level(&mut doc, &pos(2, 0), &pos(2, 0), 2);
    assert_eq!(p(&doc, 1).label.as_ref().unwrap().text, "◦");
    assert_eq!(p(&doc, 2).label.as_ref().unwrap().text, "▪");
    assert_eq!(p(&doc, 2).props.indent_left, 1440);
    change_level(&mut doc, &pos(2, 0), &pos(2, 0), -9);
    assert_eq!(p(&doc, 2).props.list.unwrap().level, 0);
    // A numbered list is another list.
    set_list(&mut doc, &pos(2, 0), &pos(2, 0), Some(ListKind::Numbered));
    assert_eq!(doc.numbering.lists.len(), 2);
    assert_eq!(p(&doc, 2).label.as_ref().unwrap().text, "1.");
    // Another bullet list later reuses the first bullet list.
    let mut more = doc_of(vec![text_para("x")]);
    more.numbering = doc.numbering.clone();
    set_list(&mut more, &pos(0, 0), &pos(0, 0), Some(ListKind::Bullet));
    assert_eq!(more.numbering.lists.len(), 2);
    set_list(&mut doc, &pos(0, 0), &pos(2, 0), None);
    assert!(labels(&doc).iter().all(Option::is_none));
    assert!(
        doc.paragraphs()
            .iter()
            .all(|(_, p)| p.props.indent_left == 0)
    );
    // Items outside a list ignore level changes.
    let before = doc.clone();
    change_level(&mut doc, &pos(0, 0), &pos(2, 0), 1);
    assert!(doc.body.blocks[0].is(&before.body.blocks[0]));
}

// ---------------------------------------------------------------------------
// Reading ranges, copy and paste
// ---------------------------------------------------------------------------

#[test]
fn plain_text_and_fragments_round_trip() {
    let mut doc = doc_of(vec![
        text_para("Hello"),
        Paragraph {
            runs: vec![Run::text("big", bold()), Run::text(" world", look())],
            props: ParaProps {
                align: Align::Right,
                ..ParaProps::default()
            },
            section: Some(Arc::new(Section::default())),
            ..Paragraph::default()
        },
        text_para("end"),
    ]);
    assert_eq!(
        plain_text(&doc, &pos(0, 2), &pos(2, 1)),
        "llo\nbig world\ne"
    );
    assert_eq!(
        plain_text(&doc, &pos(2, 1), &pos(0, 2)),
        "llo\nbig world\ne"
    );
    let part = fragment(&doc, &pos(0, 3), &pos(1, 5));
    assert_eq!(part.len(), 2);
    let copy = part[1].as_paragraph().unwrap();
    assert!(copy.source.is_none() && copy.section.is_none());
    assert_eq!(copy.text(), "big w");
    // Paste into the first paragraph: it splits, the last piece joins the
    // second half.
    let at = insert_fragment(&mut doc, &pos(0, 1), &part);
    assert_eq!(texts(&doc)[..2], ["Hlo", "big wello"]);
    assert_eq!(at, pos(1, 5));
    assert_eq!(p(&doc, 1).props.align, Align::Left);
    assert!(p(&doc, 1).runs[0].look.bold);
    // One paragraph goes into the text.
    let word = fragment(&doc, &pos(1, 0), &pos(1, 3));
    let at = insert_fragment(&mut doc, &pos(0, 3), &word);
    assert_eq!(at, pos(0, 6));
    assert_eq!(p(&doc, 0).text(), "Hlobig");
    assert!(p(&doc, 0).runs.last().unwrap().look.bold);
    // A fragment from nowhere inserts nothing.
    assert_eq!(insert_fragment(&mut doc, &pos(0, 0), &[]), pos(0, 0));
}

#[test]
fn paragraph_range_is_the_whole_paragraph() {
    let doc = doc_of(vec![text_para("héllo")]);
    assert_eq!(
        paragraph_range(&doc, &pos(0, 2)),
        Some((pos(0, 0), pos(0, "héllo".len())))
    );
    assert_eq!(paragraph_range(&doc, &pos(4, 0)), None);
}

// ---------------------------------------------------------------------------
// Comparing and history
// ---------------------------------------------------------------------------

#[test]
fn same_content_ignores_sources_and_how_text_is_split_up() {
    let a = doc_of(vec![read(text_para("abcd"))]);
    let mut b = doc_of(vec![Paragraph {
        runs: vec![
            Run::text("ab", look()),
            marker(),
            marker(),
            Run::text("cd", look()),
        ],
        ..Paragraph::default()
    }]);
    assert!(!same_content(&a, &b));
    b.body.blocks[0] = Block::paragraph(Paragraph {
        runs: vec![Run::text("ab", look()), Run::text("cd", look())],
        ..Paragraph::default()
    });
    assert!(same_content(&a, &b));
    let mut c = a.clone();
    apply_run_patch(&mut c, &pos(0, 0), &pos(0, 1), &make_bold());
    assert!(!same_content(&a, &c));
}

#[test]
fn undo_and_redo_restore_documents_and_selections() {
    let mut history = History::new(3);
    let mut doc = doc_of(vec![text_para("")]);
    let mut selection = Selection::caret(pos(0, 0));
    for c in ["a", "b", "c"] {
        let before = doc.clone();
        let sel_before = selection.clone();
        let at = insert_text(&mut doc, &selection.focus, c, None);
        selection = Selection::caret(at);
        history.record(&before, &sel_before, &selection, Some(StepKind::Typing));
    }
    // Three characters of typing are one step.
    assert_eq!(history.depth(), 1);
    let before = doc.clone();
    let sel_before = selection.clone();
    split_paragraph(&mut doc, &selection.focus);
    selection = Selection::caret(pos(1, 0));
    history.record(&before, &sel_before, &selection, None);
    assert_eq!(history.depth(), 2);

    let (restored, sel) = history.undo(&doc).unwrap();
    assert_eq!(texts(&restored), ["abc"]);
    assert_eq!(sel, Selection::caret(pos(0, 3)));
    let (back, sel2) = history.redo(&restored).unwrap();
    assert!(same_content(&back, &doc));
    assert_eq!(sel2, Selection::caret(pos(1, 0)));
    // Undo twice: the empty document and the caret where it was.
    let (one, _) = history.undo(&back).unwrap();
    let (zero, sel0) = history.undo(&one).unwrap();
    assert_eq!(texts(&zero), [""]);
    assert_eq!(sel0, Selection::caret(pos(0, 0)));
    assert!(!history.can_undo() && history.can_redo());
    assert!(history.undo(&zero).is_none());
    // A new edit drops redo.
    history.record(&zero, &sel0, &sel0, None);
    assert!(!history.can_redo());
}

#[test]
fn history_keeps_to_its_limit_and_merges_single_deletions() {
    let mut history = History::new(2);
    let doc = doc_of(vec![text_para("x")]);
    for n in 0..5 {
        let sel = Selection::caret(pos(0, n));
        history.record(&doc, &sel, &sel, None);
    }
    assert_eq!(history.depth(), 2);
    let mut history = History::new(10);
    for n in (1..4).rev() {
        let before = Selection::caret(pos(0, n));
        let after = Selection::caret(pos(0, n - 1));
        history.record(&doc, &before, &after, Some(StepKind::DeleteBackward));
    }
    assert_eq!(history.depth(), 1);
    // A step that does not begin where the last ended starts another.
    let before = Selection::caret(pos(0, 9));
    history.record(&doc, &before, &before, Some(StepKind::DeleteBackward));
    assert_eq!(history.depth(), 2);
    history.seal();
    history.record(&doc, &before, &before, Some(StepKind::DeleteBackward));
    assert_eq!(history.depth(), 3);
}

#[test]
fn selections_order_themselves() {
    let sel = Selection::new(pos(2, 1), pos(0, 4));
    assert_eq!(sel.ordered(), (pos(0, 4), pos(2, 1)));
    assert!(!sel.is_caret());
    assert!(Selection::caret(pos(1, 1)).is_caret());
}
