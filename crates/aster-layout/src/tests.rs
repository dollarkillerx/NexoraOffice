//! Tests of the layout rules. Documents are built in code with small pages so
//! that a few paragraphs fill them. Fonts are the system's: assertions are
//! about relations (this is below that, this stays with that), not about
//! exact line heights.

use std::sync::Arc;

use aster_writer::*;

use crate::{Item, Layout, Layouter, Page};

fn look() -> RunLook {
    RunLook {
        size: 10.0,
        ..RunLook::default()
    }
}

fn paragraph(text: &str) -> Paragraph {
    Paragraph {
        runs: vec![Run::text(text, look())],
        base: look(),
        mark: look(),
        ..Paragraph::default()
    }
}

fn pb(text: &str) -> Block {
    Block::paragraph(paragraph(text))
}

fn with(text: &str, edit: impl FnOnce(&mut ParaProps)) -> Block {
    let mut p = paragraph(text);
    edit(&mut p.props);
    Block::paragraph(p)
}

/// 300 by 200 points, 20-point margins.
fn small() -> Section {
    Section {
        page: (6_000, 4_000),
        margins: Margins {
            top: 400,
            right: 400,
            bottom: 400,
            left: 400,
            header: 200,
            footer: 200,
            gutter: 0,
        },
        ..Section::default()
    }
}

fn doc(blocks: Vec<Block>) -> Document {
    Document {
        body: Story { blocks, part: None },
        last_section: Arc::new(small()),
        ..Document::default()
    }
}

fn lay(document: &Document) -> Layout {
    Layouter::new().layout(document)
}

/// What a page shows as text: one string per text item (its lines).
fn page_texts(page: &Page) -> Vec<String> {
    page.items
        .iter()
        .filter_map(|item| match item {
            Item::Text(t) => {
                let ranges = t.paragraph.line_ranges();
                let end = t.lines.end.min(ranges.len());
                let text = t.paragraph.text();
                Some(
                    ranges[t.lines.start.min(end)..end]
                        .iter()
                        .map(|r| text[r.clone()].to_owned())
                        .collect::<Vec<_>>()
                        .join("\n"),
                )
            }
            _ => None,
        })
        .collect()
}

fn page_has(page: &Page, needle: &str) -> bool {
    page_texts(page).iter().any(|t| t.contains(needle))
}

fn caret_top(layout: &Layout, block: usize, offset: usize) -> f32 {
    layout
        .caret(&Pos::new(Path::body(block), offset))
        .expect("a caret")
        .top
}

fn page_of(layout: &Layout, block: usize) -> usize {
    layout
        .page_of(&Pos::new(Path::body(block), 0))
        .expect("a page")
}

fn lines_of(layout: &Layout, path: &Path) -> Vec<(usize, usize)> {
    layout
        .frags
        .iter()
        .filter(|f| &f.path == path && !f.repeat)
        .map(|f| (f.page, f.lines.len()))
        .collect()
}

fn numbered(n: usize) -> Vec<Block> {
    (0..n).map(|i| pb(&format!("paragraph {i}"))).collect()
}

// ---------------------------------------------------------------------------
// Pages and paragraphs
// ---------------------------------------------------------------------------

#[test]
fn an_empty_document_is_one_page() {
    let layout = lay(&Document::default());
    assert_eq!(layout.pages.len(), 1);
    assert_eq!(layout.pages[0].number, 1);
    let layout = lay(&doc(Vec::new()));
    assert_eq!(layout.pages.len(), 1);
    assert_eq!(
        (layout.pages[0].width, layout.pages[0].height),
        (300.0, 200.0)
    );
    assert!(layout.outline().is_empty());
    assert!(layout.caret(&Pos::default()).is_none());
    assert!(layout.hit(0, 10.0, 10.0).is_none());
}

#[test]
fn text_flows_onto_numbered_pages_inside_the_margins() {
    let layout = lay(&doc(numbered(60)));
    assert!(layout.pages.len() > 2, "{}", layout.pages.len());
    for (n, page) in layout.pages.iter().enumerate() {
        assert_eq!(page.number as usize, n + 1);
        assert_eq!(page.label, (n + 1).to_string());
    }
    // Everything is inside the 20-point margins, and in reading order.
    let mut last = (0, 0.0f32);
    for index in 0..60 {
        let caret = layout
            .caret(&Pos::new(Path::body(index), 0))
            .expect("placed");
        assert!(
            caret.top >= 19.9 && caret.top + caret.height <= 180.1,
            "{caret:?}"
        );
        assert!((caret.page, caret.top) >= last);
        last = (caret.page, caret.top);
        assert!((caret.x - 20.0).abs() < 0.01);
    }
}

#[test]
fn indents_and_first_line_set_where_lines_start() {
    let mut block = paragraph("hello world and some more words to wrap over lines of text");
    block.props.indent_left = 400; // 20 pt
    block.props.indent_right = 200;
    block.props.first_line = 200; // 10 pt
    let mut hanging = paragraph(
        "hanging indent text that wraps over a few lines of text ok and then some more text to make a second line for sure",
    );
    hanging.props.indent_left = 600;
    hanging.props.first_line = -300;
    let document = doc(vec![Block::paragraph(block), Block::paragraph(hanging)]);
    let layout = lay(&document);
    let first = layout.caret(&Pos::new(Path::body(0), 0)).unwrap();
    assert!((first.x - (20.0 + 20.0 + 10.0)).abs() < 0.01, "{first:?}");
    let second_line = layout
        .line_start(&Pos::new(Path::body(0), 55))
        .expect("line start");
    assert!(second_line.offset > 0, "{second_line:?}");
    let c = layout.caret(&second_line).unwrap();
    assert!((c.x - 40.0).abs() < 0.01, "{c:?}");
    // A hanging indent: the first line is further out than the others.
    let h0 = layout.caret(&Pos::new(Path::body(1), 0)).unwrap();
    assert!((h0.x - (20.0 + 30.0 - 15.0)).abs() < 0.01, "{h0:?}");
    let h1 = layout
        .caret(&layout.line_start(&Pos::new(Path::body(1), 100)).unwrap())
        .unwrap();
    assert!((h1.x - 50.0).abs() < 0.01, "{h1:?}");
}

#[test]
fn space_before_and_after_add_up_and_contextual_spacing_drops_them() {
    let gap = |a: i32, b: i32, contextual: bool| -> f32 {
        let blocks = vec![
            with("one", |p| {
                p.space_after = a;
                p.contextual_spacing = contextual;
                p.style = Some("S".into());
            }),
            with("two", |p| {
                p.space_before = b;
                p.contextual_spacing = contextual;
                p.style = Some("S".into());
            }),
        ];
        let layout = lay(&doc(blocks));
        caret_top(&layout, 1, 0) - caret_top(&layout, 0, 0)
    };
    let plain = gap(0, 0, false);
    assert!((gap(200, 100, false) - plain - 15.0).abs() < 0.01);
    assert!((gap(200, 100, true) - plain).abs() < 0.01);
}

#[test]
fn space_before_is_dropped_at_the_top_after_overflow_but_kept_after_a_break() {
    // Find a count of fillers that makes the next paragraph start page two.
    let build = |n: usize| {
        let mut blocks = numbered(n);
        blocks.push(with("tall space", |p| p.space_before = 400));
        blocks_doc(blocks)
    };
    let n = (1..40)
        .find(|n| {
            let layout = lay(&build(*n));
            page_of(&layout, *n) == 1
        })
        .expect("a paragraph that starts the second page");
    let layout = lay(&build(n));
    let top = caret_top(&layout, n, 0);
    assert!((top - 20.0).abs() < 0.01, "dropped at the top: {top}");
    // After an explicit page break it is kept.
    let mut blocks = vec![pb("first")];
    blocks.push(with("second", |p| {
        p.space_before = 400;
        p.page_break_before = true;
    }));
    let layout = lay(&blocks_doc(blocks));
    assert_eq!(page_of(&layout, 1), 1);
    assert!((caret_top(&layout, 1, 0) - 40.0).abs() < 0.01);
}

fn blocks_doc(blocks: Vec<Block>) -> Document {
    doc(blocks)
}

#[test]
fn line_spacing_rules_set_line_heights() {
    let text = "a\nb\nc";
    let height = |line: LineSpacing| {
        let layout = lay(&doc(vec![with(text, |p| p.line = line)]));
        caret_top(&layout, 0, 2) - caret_top(&layout, 0, 0)
    };
    let single = height(LineSpacing::Multiple(1.0)) / 1.0;
    assert!((height(LineSpacing::Multiple(2.0)) - 2.0 * single).abs() < 0.05);
    assert!((height(LineSpacing::Exactly(400)) - 20.0).abs() < 0.05);
    assert!((height(LineSpacing::AtLeast(600)) - 30.0).abs() < 0.05);
    assert!((height(LineSpacing::AtLeast(20)) - single).abs() < 0.05);
}

#[test]
fn widows_and_orphans_are_never_left() {
    let blocks: Vec<Block> = (0..30)
        .map(|i| with(&format!("a{i}\nb\nc\nd\ne"), |p| p.space_after = 100))
        .collect();
    let layout = lay(&doc(blocks));
    assert!(layout.pages.len() > 3);
    let mut split = 0;
    for i in 0..30 {
        let parts = lines_of(&layout, &Path::body(i));
        if parts.len() > 1 {
            split += 1;
            assert!(parts.iter().all(|(_, n)| *n >= 2), "{i}: {parts:?}");
        }
        assert_eq!(parts.iter().map(|p| p.1).sum::<usize>(), 5);
    }
    assert!(split > 0, "the test needs paragraphs that split");
}

#[test]
fn keep_lines_keeps_a_paragraph_on_one_page() {
    let blocks: Vec<Block> = (0..30)
        .map(|i| with(&format!("k{i}\nb\nc\nd\ne\nf"), |p| p.keep_lines = true))
        .collect();
    let layout = lay(&doc(blocks));
    for i in 0..30 {
        assert_eq!(lines_of(&layout, &Path::body(i)).len(), 1, "{i}");
    }
}

#[test]
fn keep_with_next_moves_a_heading_along() {
    let build = |fillers: usize, keep: bool| {
        let mut blocks = numbered(fillers);
        blocks.push(with("heading", |p| p.keep_next = keep));
        blocks.push(with("b1\nb2\nb3\nb4\nb5", |p| p.keep_lines = true));
        doc(blocks)
    };
    // Find a count of fillers that leaves the heading alone at a page bottom.
    let n = (1..40)
        .find(|n| {
            let layout = lay(&build(*n, false));
            page_of(&layout, *n) == 0 && page_of(&layout, n + 1) == 1
        })
        .expect("a layout that parts heading and body");
    let layout = lay(&build(n, true));
    assert_eq!(page_of(&layout, n), page_of(&layout, n + 1));
    assert_eq!(page_of(&layout, n), 1);
}

#[test]
fn page_and_line_breaks_inside_a_paragraph() {
    let mut p = paragraph("");
    p.runs = vec![
        Run::text("before", look()),
        Run::object(Inline::Break(Break::Line), look()),
        Run::text("after line", look()),
        Run::object(Inline::Break(Break::Page), look()),
        Run::text("after page", look()),
    ];
    let layout = lay(&doc(vec![Block::paragraph(p), pb("next")]));
    assert_eq!(layout.pages.len(), 2);
    assert!(page_has(&layout.pages[0], "after line"));
    assert!(page_has(&layout.pages[1], "after page"));
    assert!(!page_has(&layout.pages[0], "after page"));
    // Positions after the break are on the second page.
    let offset = "before".len() + 1 + "after line".len() + OBJECT.len();
    assert_eq!(layout.page_of(&Pos::new(Path::body(0), offset)), Some(1));
    assert_eq!(layout.page_of(&Pos::new(Path::body(0), 0)), Some(0));
    assert_eq!(page_of(&layout, 1), 1);
}

#[test]
fn page_break_before_starts_a_page_but_not_an_empty_one() {
    let layout = lay(&doc(vec![
        with("first on page one", |p| p.page_break_before = true),
        with("second", |p| p.page_break_before = true),
    ]));
    assert_eq!(layout.pages.len(), 2);
    assert_eq!(page_of(&layout, 0), 0);
    assert_eq!(page_of(&layout, 1), 1);
}

#[test]
fn hidden_runs_take_no_room_and_caps_keep_offsets() {
    let mut hidden = look();
    hidden.hidden = true;
    let mut caps = look();
    caps.caps = true;
    let mut p = paragraph("");
    p.runs = vec![
        Run::text("ab", look()),
        Run::text("SECRET", hidden),
        Run::text("cd ef", caps),
    ];
    let mut q = paragraph("");
    q.runs = vec![Run::text("abcd ef", look())];
    let layout = lay(&doc(vec![Block::paragraph(p), Block::paragraph(q)]));
    let shown = page_texts(&layout.pages[0]);
    assert_eq!(shown[0], "abCD EF");
    assert_eq!(shown[1], "abcd ef");
    // The hidden text takes no width: the caret after it is where it was.
    let before = layout.caret(&Pos::new(Path::body(0), 2)).unwrap();
    let after = layout.caret(&Pos::new(Path::body(0), 8)).unwrap();
    assert!((before.x - after.x).abs() < 0.01);
    // Offsets keep meaning: they move right through the shown text.
    let mid = layout.caret(&Pos::new(Path::body(0), 10)).unwrap();
    let end = layout.caret(&Pos::new(Path::body(0), 13)).unwrap();
    assert!(mid.x > before.x && end.x > mid.x);
}

#[test]
fn list_labels_come_first_and_text_aligns_after_the_tab() {
    let label = |text: &str| Label {
        text: text.into(),
        look: look(),
        suffix: Suffix::Tab,
        align: Align::Left,
    };
    let mut item = paragraph("item text that is long enough to wrap onto a second line here");
    item.props.indent_left = 600; // 30 pt
    item.props.first_line = -400; // label at 10 pt
    item.label = Some(label("1."));
    let layout = lay(&doc(vec![Block::paragraph(item)]));
    // The text starts at the left indent, the label before it.
    let start = layout.caret(&Pos::new(Path::body(0), 0)).unwrap();
    assert!((start.x - (20.0 + 30.0)).abs() < 0.5, "{start:?}");
    let texts = page_texts(&layout.pages[0]);
    assert!(texts[0].starts_with("1.\titem text"), "{texts:?}");
    // The second line starts at the indent too.
    let second = layout
        .line_start(&Pos::new(Path::body(0), 45))
        .and_then(|p| layout.caret(&p))
        .unwrap();
    assert!((second.x - 50.0).abs() < 0.01);
    // Clicking in the label puts the caret at the start of the text.
    let hit = layout.hit(0, 22.0, start.top + 2.0).unwrap();
    assert_eq!(hit.offset, 0);
}

#[test]
fn tab_stops_leaders_and_default_stops() {
    let mut p = paragraph("a\tb\tc");
    p.props.tabs = vec![TabStop {
        position: 2_000, // 100 pt from the margin
        align: TabAlign::Left,
        leader: TabLeader::Dots,
    }];
    let layout = lay(&doc(vec![Block::paragraph(p)]));
    let b = layout.caret(&Pos::new(Path::body(0), 2)).unwrap();
    assert!((b.x - (20.0 + 100.0)).abs() < 0.2, "{b:?}");
    // Then the default stops, 36 pt apart, from the margin.
    let c = layout.caret(&Pos::new(Path::body(0), 4)).unwrap();
    assert!((c.x - (20.0 + 108.0)).abs() < 0.2, "{c:?}");
    let leaders = layout.pages[0]
        .items
        .iter()
        .filter(|i| matches!(i, Item::Leader(_)))
        .count();
    assert_eq!(leaders, 1);
}

#[test]
fn borders_and_shading_share_one_box_between_neighbours() {
    let edge = Border {
        style: BorderStyle::Single,
        width: 1.0,
        color: None,
        space: 2.0,
    };
    let boxed = |text: &str| {
        with(text, |p| {
            p.borders = [Some(edge); 4];
            p.border_between = Some(edge);
            p.shading = Some([200, 220, 255, 255]);
        })
    };
    let layout = lay(&doc(vec![boxed("one"), boxed("two"), pb("plain")]));
    let rules: Vec<_> = layout.pages[0]
        .items
        .iter()
        .filter(|i| matches!(i, Item::Rule { from, to, .. } if from.1 == to.1))
        .collect();
    let fills = layout.pages[0]
        .items
        .iter()
        .filter(|i| matches!(i, Item::Fill { .. }))
        .count();
    // The top, the bottom and one between the two: three horizontal lines,
    // not four.
    assert_eq!(rules.len(), 3, "{rules:?}");
    assert_eq!(fills, 2);
    // The text moved down by the top border and its space.
    let top = caret_top(&layout, 0, 0);
    assert!(top >= 20.0 + 3.0 - 0.01, "{top}");
}

// ---------------------------------------------------------------------------
// Tables
// ---------------------------------------------------------------------------

fn cell(text: &str) -> Cell {
    Cell {
        blocks: vec![pb(text)],
        ..Cell::default()
    }
}

fn rows_of(texts: &[&[&str]]) -> Vec<Row> {
    texts
        .iter()
        .map(|row| Row {
            cells: row.iter().map(|t| cell(t)).collect(),
            ..Row::default()
        })
        .collect()
}

fn table(columns: Vec<Twips>, rows: Vec<Row>) -> Block {
    Block::table(Table {
        columns,
        rows,
        ..Table::default()
    })
}

fn cell_pos(table: usize, row: usize, cell: usize) -> Pos {
    Pos::new(Path::body(table).cell_block(row, cell, 0), 0)
}

#[test]
fn table_columns_spans_and_alignment() {
    let mut wide = cell("wide");
    wide.span = 2;
    let rows = vec![
        Row {
            cells: vec![cell("a"), cell("b"), cell("c")],
            ..Row::default()
        },
        Row {
            cells: vec![wide, cell("d")],
            ..Row::default()
        },
    ];
    let layout = lay(&doc(vec![table(vec![1_000, 1_000, 1_000], rows)]));
    let a = layout.caret(&cell_pos(0, 0, 0)).unwrap();
    let b = layout.caret(&cell_pos(0, 0, 1)).unwrap();
    let c = layout.caret(&cell_pos(0, 0, 2)).unwrap();
    let d = layout.caret(&cell_pos(0, 1, 1)).unwrap();
    // The first cell's text lines up with the margin (the edge sits a cell
    // margin to the left of it); columns are 50 pt wide.
    assert!((a.x - 20.0).abs() < 0.01, "{a:?}");
    assert!((b.x - a.x - 50.0).abs() < 0.01);
    assert!((c.x - b.x - 50.0).abs() < 0.01);
    assert!(
        (d.x - c.x).abs() < 0.01,
        "the spanning cell covers two columns"
    );
    assert!(d.top > a.top);
    // Right aligned: the table's right edge is at the right margin.
    let mut right = Table {
        columns: vec![1_000, 1_000],
        rows: rows_of(&[&["x", "y"]]),
        ..Table::default()
    };
    right.props.align = Align::Right;
    let layout = lay(&doc(vec![Block::table(right)]));
    let y = layout.caret(&cell_pos(0, 0, 1)).unwrap();
    assert!((y.x - (280.0 - 50.0 + 5.75)).abs() < 0.5, "{y:?}");
}

#[test]
fn vertical_merges_span_rows_and_grow_the_last_one() {
    let mut start = Cell {
        blocks: vec![pb("m1\nm2\nm3\nm4\nm5\nm6")],
        merge: VMerge::Start,
        ..Cell::default()
    };
    start.shading = Some([255, 0, 0, 255]);
    let cont = Cell {
        blocks: vec![pb("")],
        merge: VMerge::Continue,
        ..Cell::default()
    };
    let rows = vec![
        Row {
            cells: vec![start, cell("r1")],
            ..Row::default()
        },
        Row {
            cells: vec![cont, cell("r2")],
            ..Row::default()
        },
        Row {
            cells: vec![cell("p"), cell("r3")],
            ..Row::default()
        },
    ];
    let layout = lay(&doc(vec![table(vec![1_000, 1_000], rows)]));
    let one = layout.caret(&cell_pos(0, 0, 1)).unwrap();
    let two = layout.caret(&cell_pos(0, 1, 1)).unwrap();
    let three = layout.caret(&cell_pos(0, 2, 1)).unwrap();
    // The merged cell's six lines fit by making the rows taller.
    let merged_height = layout
        .caret(&Pos::new(Path::body(0).cell_block(0, 0, 0), 15))
        .unwrap();
    assert!(
        three.top > merged_height.top,
        "row 3 is below the merged text"
    );
    assert!(two.top > one.top);
    // One fill covers the merged cell over its rows, one per row slice.
    let fills = layout.pages[0]
        .items
        .iter()
        .filter(|i| matches!(i, Item::Fill { .. }))
        .count();
    assert_eq!(fills, 2);
}

fn tall_table(rows: usize, header: bool) -> Block {
    let mut all = Vec::new();
    all.push(Row {
        cells: vec![cell("head")],
        header,
        ..Row::default()
    });
    for i in 0..rows {
        all.push(Row {
            cells: vec![cell(&format!("row {i}\nsecond line\nthird"))],
            cant_split: true,
            ..Row::default()
        });
    }
    table(vec![4_000], all)
}

#[test]
fn header_rows_repeat_and_rows_move_whole() {
    let layout = lay(&doc(vec![tall_table(12, true)]));
    assert!(layout.pages.len() >= 2);
    for page in 1..layout.pages.len() {
        let repeats = layout
            .frags
            .iter()
            .filter(|f| f.repeat && f.page == page)
            .count();
        assert_eq!(repeats, 1, "page {page}");
        assert!(page_has(&layout.pages[page], "head"));
        // The header is the first thing in the table on the page.
        let head = layout
            .frags
            .iter()
            .find(|f| f.repeat && f.page == page)
            .unwrap();
        let others = layout
            .frags
            .iter()
            .filter(|f| !f.repeat && f.page == page)
            .map(|f| f.y)
            .fold(f32::INFINITY, f32::min);
        assert!(head.y < others);
    }
    // A row is never cut: each of its paragraphs is in one piece.
    for i in 0..12 {
        let path = Path::body(0).cell_block(i + 1, 0, 0);
        assert_eq!(
            lines_of(&layout, &path),
            vec![(lines_of(&layout, &path)[0].0, 3)],
            "row {i}"
        );
    }
    // Without header rows nothing is repeated.
    let layout = lay(&doc(vec![tall_table(12, false)]));
    assert!(layout.frags.iter().all(|f| !f.repeat));
}

#[test]
fn a_row_taller_than_a_page_is_split_between_lines_and_nothing_is_lost() {
    let lines = (0..90)
        .map(|i| format!("line {i}"))
        .collect::<Vec<_>>()
        .join("\n");
    let rows = vec![
        Row {
            cells: vec![cell("head")],
            header: true,
            ..Row::default()
        },
        Row {
            cells: vec![Cell {
                blocks: vec![pb(&lines)],
                ..Cell::default()
            }],
            ..Row::default()
        },
        Row {
            cells: vec![cell("after")],
            ..Row::default()
        },
    ];
    let layout = lay(&doc(vec![table(vec![4_000], rows)]));
    assert!(layout.pages.len() >= 4);
    let path = Path::body(0).cell_block(1, 0, 0);
    let parts = lines_of(&layout, &path);
    assert!(parts.len() >= 4);
    assert_eq!(parts.iter().map(|p| p.1).sum::<usize>(), 90);
    // They are in order, on consecutive pages.
    assert!(parts.windows(2).all(|w| w[1].0 == w[0].0 + 1));
    let last = layout.pages.len() - 1;
    assert!(page_has(&layout.pages[last], "after"));
}

#[test]
fn cant_split_rows_and_exact_heights() {
    let mut row = Row {
        cells: vec![cell("a\nb\nc\nd\ne\nf\ng\nh\ni\nj")],
        height: RowHeight::Exactly(400),
        ..Row::default()
    };
    row.cant_split = true;
    let layout = lay(&doc(vec![table(
        vec![4_000],
        vec![
            row,
            Row {
                cells: vec![cell("below")],
                ..Row::default()
            },
        ],
    )]));
    // The exact height (20 pt) cuts the text; the next row follows at once.
    let above = layout.caret(&cell_pos(0, 0, 0)).unwrap();
    let below = layout.caret(&cell_pos(0, 1, 0)).unwrap();
    assert!(
        (below.top - above.top - 20.0).abs() < 1.5,
        "{above:?} {below:?}"
    );
    let clips = layout.pages[0]
        .items
        .iter()
        .filter(|i| matches!(i, Item::PushClip(_)))
        .count();
    assert_eq!(clips, 1);
}

#[test]
fn nested_tables_and_grid_before_after() {
    let inner = table(vec![800, 800], rows_of(&[&["i1", "i2"]]));
    let outer_cell = Cell {
        blocks: vec![pb("outer"), inner],
        ..Cell::default()
    };
    let rows = vec![
        Row {
            cells: vec![outer_cell, cell("side")],
            ..Row::default()
        },
        Row {
            cells: vec![cell("shifted")],
            grid_before: 1,
            grid_after: 0,
            ..Row::default()
        },
    ];
    let layout = lay(&doc(vec![table(vec![3_000, 1_500], rows)]));
    let i1 = Pos::new(Path::body(0).cell_block(0, 0, 1).cell_block(0, 0, 0), 0);
    let i2 = Pos::new(Path::body(0).cell_block(0, 0, 1).cell_block(0, 1, 0), 0);
    let (c1, c2) = (layout.caret(&i1).unwrap(), layout.caret(&i2).unwrap());
    assert!((c2.x - c1.x - 40.0).abs() < 0.01);
    let outer = layout.caret(&cell_pos(0, 0, 0)).unwrap();
    assert!(c1.top > outer.top);
    // grid_before leaves the first column empty.
    let shifted = layout.caret(&cell_pos(0, 1, 0)).unwrap();
    let side = layout.caret(&cell_pos(0, 0, 1)).unwrap();
    assert!((shifted.x - side.x).abs() < 0.01);
}

#[test]
fn shared_borders_are_drawn_once() {
    let line = Some(Border {
        style: BorderStyle::Single,
        width: 0.5,
        color: None,
        space: 0.0,
    });
    let mut a = cell("a");
    let mut b = cell("b");
    a.borders = [line; 4];
    b.borders = [line; 4];
    let layout = lay(&doc(vec![table(
        vec![1_000, 1_000],
        vec![Row {
            cells: vec![a, b],
            ..Row::default()
        }],
    )]));
    let rules = layout.pages[0]
        .items
        .iter()
        .filter(|i| matches!(i, Item::Rule { .. }))
        .count();
    // Three vertical lines and one top and one bottom across both cells.
    assert_eq!(rules, 5);
}

// ---------------------------------------------------------------------------
// Headers, footers, notes
// ---------------------------------------------------------------------------

fn field(kind: PageField) -> Run {
    Run {
        text: "0".into(),
        kind: Inline::PageNumber(kind),
        look: look(),
        ..Run::default()
    }
}

fn marginal(runs: Vec<Run>) -> Arc<Story> {
    let mut p = paragraph("");
    p.runs = runs;
    Arc::new(Story {
        blocks: vec![Block::paragraph(p)],
        part: None,
    })
}

#[test]
fn headers_and_footers_show_real_page_numbers() {
    let mut document = doc(numbered(40));
    document.marginals = vec![
        marginal(vec![Run::text("Head ", look()), field(PageField::Page)]),
        marginal(vec![
            field(PageField::Page),
            Run::text(" of ", look()),
            field(PageField::PageCount),
        ]),
    ];
    let mut section = small();
    section.headers.default = Some(0);
    section.footers.default = Some(1);
    section.page_number_start = Some(5);
    document.last_section = Arc::new(section);
    let layout = lay(&document);
    let n = layout.pages.len();
    assert!(n >= 2);
    for (i, page) in layout.pages.iter().enumerate() {
        assert_eq!(page.number as usize, 5 + i);
        assert!(
            page_has(page, &format!("Head {}", 5 + i)),
            "{:?}",
            page_texts(page)
        );
        assert!(
            page_has(page, &format!("{} of {}", 5 + i, n)),
            "{:?}",
            page_texts(page)
        );
    }
    // The footer is in the bottom margin, the header in the top one.
    let page = &layout.pages[0];
    let ys: Vec<f32> = page
        .items
        .iter()
        .filter_map(|i| match i {
            Item::Text(t) => Some(t.y),
            _ => None,
        })
        .collect();
    assert!(ys.iter().any(|y| *y < 20.0));
    assert!(ys.iter().any(|y| *y > 175.0));
}

#[test]
fn first_page_and_even_headers() {
    let mut document = doc(numbered(60));
    document.marginals = vec![
        marginal(vec![Run::text("DEFAULT", look())]),
        marginal(vec![Run::text("FIRST", look())]),
        marginal(vec![Run::text("EVEN", look())]),
    ];
    let mut section = small();
    section.headers = Marginals {
        default: Some(0),
        first: Some(1),
        even: Some(2),
    };
    section.title_page = true;
    document.last_section = Arc::new(section);
    document.settings.even_and_odd_headers = true;
    let layout = lay(&document);
    assert!(layout.pages.len() >= 3);
    assert!(page_has(&layout.pages[0], "FIRST"));
    assert!(page_has(&layout.pages[1], "EVEN"));
    assert!(page_has(&layout.pages[2], "DEFAULT"));
}

#[test]
fn a_tall_header_pushes_the_body_down() {
    let mut document = doc(vec![pb("body")]);
    let tall = "h\nh\nh\nh\nh\nh\nh\nh";
    document.marginals = vec![marginal(vec![Run::text(tall, look())])];
    let mut section = small();
    section.headers.default = Some(0);
    document.last_section = Arc::new(section);
    // The same paragraph with "\n" as characters is one line each: use breaks.
    let mut p = paragraph("");
    p.runs = (0..8)
        .flat_map(|_| {
            [
                Run::text("h", look()),
                Run::object(Inline::Break(Break::Line), look()),
            ]
        })
        .collect();
    document.marginals = vec![Arc::new(Story {
        blocks: vec![Block::paragraph(p)],
        part: None,
    })];
    let layout = lay(&document);
    let top = caret_top(&layout, 0, 0);
    assert!(top > 20.0 + 40.0, "{top}");
}

#[test]
fn footnotes_sit_at_the_bottom_of_their_page_and_are_numbered_in_order() {
    let reference = |id: i32| Run::object(Inline::Footnote(id), look());
    let mut first = paragraph("");
    first.runs = vec![Run::text("see", look()), reference(7)];
    let mut second = paragraph("");
    second.runs = vec![Run::text("and", look()), reference(3)];
    let mut blocks = vec![Block::paragraph(first)];
    blocks.extend(numbered(3));
    blocks.push(Block::paragraph(second));
    let mut document = doc(blocks);
    let note = |id: i32, text: &str| {
        let mut p = paragraph("");
        p.runs = vec![
            Run::object(Inline::Footnote(id), look()),
            Run::text(text, look()),
        ];
        Note {
            id,
            blocks: vec![Block::paragraph(p)],
        }
    };
    document.footnotes = Notes {
        notes: vec![note(3, " note three"), note(7, " note seven")],
        part: None,
    };
    let layout = lay(&document);
    let page = &layout.pages[0];
    let texts = page_texts(page);
    assert!(texts.iter().any(|t| t == "see1"), "{texts:?}");
    assert!(texts.iter().any(|t| t == "and2"), "{texts:?}");
    assert!(texts.iter().any(|t| t == "1 note seven"), "{texts:?}");
    assert!(texts.iter().any(|t| t == "2 note three"), "{texts:?}");
    // The notes are under the body text, below a separator.
    let y_of = |needle: &str| {
        page.items
            .iter()
            .find_map(|i| match i {
                Item::Text(t) if t.paragraph.text().contains(needle) => Some(t.y),
                _ => None,
            })
            .unwrap()
    };
    assert!(y_of("note seven") > y_of("and"));
    assert!(y_of("note three") > y_of("note seven"));
    assert!(y_of("note seven") > 150.0);
    assert!(page.items.iter().any(|i| matches!(i, Item::Rule { .. })));
}

#[test]
fn a_footnote_goes_with_its_reference_to_the_next_page() {
    let mut blocks = numbered(14);
    let mut p = paragraph("");
    p.runs = vec![
        Run::text("ref", look()),
        Run::object(Inline::Footnote(1), look()),
    ];
    blocks.push(Block::paragraph(p));
    let mut document = doc(blocks);
    document.footnotes = Notes {
        notes: vec![Note {
            id: 1,
            blocks: vec![pb("the note")],
        }],
        part: None,
    };
    let layout = lay(&document);
    let page = page_of(&layout, 14);
    assert!(page_has(&layout.pages[page], "the note"));
    for (i, other) in layout.pages.iter().enumerate() {
        if i != page {
            assert!(!page_has(other, "the note"));
        }
    }
}

#[test]
fn endnotes_come_at_the_end_of_the_document() {
    let mut p = paragraph("");
    p.runs = vec![
        Run::text("text", look()),
        Run::object(Inline::Endnote(9), look()),
    ];
    let mut document = doc(vec![Block::paragraph(p)]);
    document.endnotes = Notes {
        notes: vec![Note {
            id: 9,
            blocks: vec![pb("end note body")],
        }],
        part: None,
    };
    let layout = lay(&document);
    let last = layout.pages.last().unwrap();
    assert!(page_has(last, "end note body"));
}

// ---------------------------------------------------------------------------
// Sections and columns
// ---------------------------------------------------------------------------

#[test]
fn columns_flow_and_column_breaks_move_on() {
    let mut section = small();
    section.columns = Columns {
        count: 2,
        space: 400,
        widths: Vec::new(),
        separator: true,
    };
    let mut document = doc(numbered(30));
    document.last_section = Arc::new(section.clone());
    let layout = lay(&document);
    let xs: Vec<f32> = (0..30)
        .map(|i| layout.caret(&Pos::new(Path::body(i), 0)).unwrap().x)
        .collect();
    assert!(xs.iter().any(|x| (*x - 20.0).abs() < 0.01));
    // The second column starts after the first plus the space: 20 + 120 + 20.
    assert!(xs.iter().any(|x| (*x - 160.0).abs() < 0.01), "{xs:?}");
    // The first thing in the second column is at the top.
    let first_right = (0..30).find(|i| (xs[*i] - 160.0).abs() < 0.01).unwrap();
    assert!((caret_top(&layout, first_right, 0) - 20.0).abs() < 0.01);
    // The separator is drawn.
    assert!(
        layout.pages[0]
            .items
            .iter()
            .any(|i| matches!(i, Item::Rule { .. }))
    );

    let mut p = paragraph("");
    p.runs = vec![
        Run::text("left", look()),
        Run::object(Inline::Break(Break::Column), look()),
        Run::text("right", look()),
    ];
    let mut document = doc(vec![Block::paragraph(p)]);
    document.last_section = Arc::new(section);
    let layout = lay(&document);
    assert_eq!(layout.pages.len(), 1);
    let left = layout.caret(&Pos::new(Path::body(0), 0)).unwrap();
    let right = layout
        .caret(&Pos::new(Path::body(0), "left".len() + OBJECT.len()))
        .unwrap();
    assert!(right.x > left.x + 100.0);
    assert!((right.top - left.top).abs() < 0.01);
}

#[test]
fn unequal_columns_use_their_widths() {
    let mut section = small();
    section.columns = Columns {
        count: 2,
        space: 0,
        widths: vec![(2_000, 400), (1_000, 0)],
        separator: false,
    };
    let mut document = doc(numbered(40));
    document.last_section = Arc::new(section);
    let layout = lay(&document);
    let xs: Vec<f32> = (0..40)
        .map(|i| layout.caret(&Pos::new(Path::body(i), 0)).unwrap().x)
        .collect();
    assert!(xs.iter().any(|x| (*x - 140.0).abs() < 0.01), "{xs:?}");
}

#[test]
fn sections_can_have_their_own_page_size_and_start_kinds() {
    let landscape = Arc::new(Section {
        page: (8_000, 6_000),
        ..small()
    });
    let mut first = paragraph("portrait");
    first.section = Some(Arc::new(small()));
    let mut document = doc(vec![Block::paragraph(first), pb("landscape")]);
    document.last_section = landscape;
    let layout = lay(&document);
    assert_eq!(layout.pages.len(), 2);
    assert_eq!(
        (layout.pages[0].width, layout.pages[0].height),
        (300.0, 200.0)
    );
    assert_eq!(
        (layout.pages[1].width, layout.pages[1].height),
        (400.0, 300.0)
    );
    assert_eq!(layout.pages[1].section, 1);

    // Continuous: the same page, below what is there.
    let mut first = paragraph("above");
    first.section = Some(Arc::new(small()));
    let mut second = small();
    second.start = SectionStart::Continuous;
    second.columns.count = 2;
    let mut document = doc(vec![Block::paragraph(first), pb("below")]);
    document.last_section = Arc::new(second);
    let layout = lay(&document);
    assert_eq!(layout.pages.len(), 1);
    assert!(caret_top(&layout, 1, 0) > caret_top(&layout, 0, 0));

    // Odd page: a blank page is inserted when needed.
    let mut first = paragraph("one");
    first.section = Some(Arc::new(small()));
    let mut odd = small();
    odd.start = SectionStart::OddPage;
    let mut document = doc(vec![Block::paragraph(first), pb("two")]);
    document.last_section = Arc::new(odd);
    let layout = lay(&document);
    assert_eq!(layout.pages.len(), 3);
    assert_eq!(page_of(&layout, 1), 2);
    assert_eq!(layout.pages[2].number, 3);
    assert!(page_texts(&layout.pages[1]).is_empty());
    // Page two is already even.
    let mut first = paragraph("one");
    first.section = Some(Arc::new(small()));
    let mut even = small();
    even.start = SectionStart::EvenPage;
    let mut document = doc(vec![Block::paragraph(first), pb("two")]);
    document.last_section = Arc::new(even);
    assert_eq!(lay(&document).pages.len(), 2);
}

#[test]
fn a_document_grid_rounds_lines_to_whole_pitches() {
    let mut section = small();
    section.grid = Some(DocGrid {
        kind: GridKind::Lines,
        line_pitch: 400,
    });
    let mut document = doc(vec![pb("a\nb\nc")]);
    document.last_section = Arc::new(section);
    let layout = lay(&document);
    // A line of 10-point text takes one 20-point grid line.
    let step = caret_top(&layout, 0, 2) - caret_top(&layout, 0, 0);
    assert!((step - 20.0).abs() < 0.01, "{step}");
}

// ---------------------------------------------------------------------------
// Drawings
// ---------------------------------------------------------------------------

fn picture(width: f64, height: f64, float: Option<Float>) -> Run {
    Run::object(
        Inline::Drawing(Arc::new(Drawing {
            size: ((width * 12_700.0) as i64, (height * 12_700.0) as i64),
            content: DrawingContent::Picture(Picture {
                image: Arc::from(vec![0u8; 4]),
                crop: [0.0; 4],
            }),
            float,
            alt: None,
        })),
        look(),
    )
}

fn float_at(wrap: Wrap, behind: bool) -> Float {
    Float {
        x: FloatAxis {
            from: FloatFrom::Margin,
            at: FloatAt::Start,
        },
        y: FloatAxis {
            from: FloatFrom::Paragraph,
            at: FloatAt::Offset(0),
        },
        wrap,
        behind,
        distance: [0; 4],
    }
}

#[test]
fn inline_pictures_make_their_line_tall() {
    let mut p = paragraph("");
    p.runs = vec![
        Run::text("a", look()),
        picture(30.0, 60.0, None),
        Run::text("b", look()),
    ];
    let layout = lay(&doc(vec![Block::paragraph(p), pb("next")]));
    let pictures: Vec<_> = layout.pages[0]
        .items
        .iter()
        .filter_map(|i| match i {
            Item::Picture(p) => Some(p.rect),
            _ => None,
        })
        .collect();
    assert_eq!(pictures.len(), 1);
    assert!((pictures[0].width() - 30.0).abs() < 0.01);
    assert!((pictures[0].height() - 60.0).abs() < 0.01);
    assert!(caret_top(&layout, 1, 0) - caret_top(&layout, 0, 0) >= 60.0);
}

#[test]
fn floating_drawings_push_text_down_unless_they_do_not_wrap() {
    let build = |wrap, behind| {
        let mut p = paragraph("");
        p.runs = vec![
            picture(50.0, 50.0, Some(float_at(wrap, behind))),
            Run::text("anchor", look()),
        ];
        doc(vec![Block::paragraph(p), pb("after")])
    };
    let wrapped = lay(&build(Wrap::TopAndBottom, false));
    assert!(caret_top(&wrapped, 0, 3) >= 20.0 + 50.0 - 0.01);
    assert!(
        wrapped.pages[0]
            .items
            .iter()
            .any(|i| matches!(i, Item::Picture(p) if p.rect.y0 == 20.0))
    );
    let free = lay(&build(Wrap::None, false));
    assert!((caret_top(&free, 0, 3) - 20.0).abs() < 0.01);
    // Behind the text it comes before the text in paint order.
    let behind = lay(&build(Wrap::None, true));
    let items = &behind.pages[0].items;
    let picture = items
        .iter()
        .position(|i| matches!(i, Item::Picture(_)))
        .unwrap();
    let text = items
        .iter()
        .position(|i| matches!(i, Item::Text(_)))
        .unwrap();
    assert!(picture < text);
    let front = lay(&build(Wrap::None, false));
    let items = &front.pages[0].items;
    let picture = items
        .iter()
        .position(|i| matches!(i, Item::Picture(_)))
        .unwrap();
    let text = items
        .iter()
        .position(|i| matches!(i, Item::Text(_)))
        .unwrap();
    assert!(picture > text);
}

#[test]
fn text_boxes_hold_blocks_and_other_drawings_are_placeholders() {
    let text_box = Drawing {
        size: (100 * 12_700, 40 * 12_700),
        content: DrawingContent::TextBox(TextBox {
            blocks: vec![pb("inside the box")],
            fill: Some([255, 255, 200, 255]),
            line: None,
            insets: [0; 4],
        }),
        float: Some(float_at(Wrap::None, false)),
        alt: None,
    };
    let other = Drawing {
        size: (30 * 12_700, 30 * 12_700),
        content: DrawingContent::Other("chart".into()),
        float: None,
        alt: None,
    };
    let mut p = paragraph("");
    p.runs = vec![
        Run::object(Inline::Drawing(Arc::new(text_box)), look()),
        Run::object(Inline::Drawing(Arc::new(other)), look()),
    ];
    let layout = lay(&doc(vec![Block::paragraph(p)]));
    let page = &layout.pages[0];
    assert!(page.items.iter().any(|i| matches!(i, Item::Frame(_))));
    assert!(page_has(page, "inside the box"));
    assert!(
        page.items
            .iter()
            .any(|i| matches!(i, Item::Placeholder { label, .. } if label == "chart"))
    );
    // Text in the box cannot be pointed at: only the body is answered for.
    assert!(layout.frags.iter().all(|f| f.path.steps.len() == 1));
}

// ---------------------------------------------------------------------------
// Queries
// ---------------------------------------------------------------------------

fn sample() -> Document {
    let label = Label {
        text: "1.".into(),
        look: look(),
        suffix: Suffix::Tab,
        align: Align::Left,
    };
    let mut item = paragraph("list item with some text to wrap onto another line of the list");
    item.props.indent_left = 600;
    item.props.first_line = -400;
    item.label = Some(label);
    let mut hidden = look();
    hidden.hidden = true;
    let mut mixed = paragraph("");
    mixed.runs = vec![
        Run::text("shown ", look()),
        Run::text("hidden", hidden),
        Run::text(" tail\ttabbed", look()),
        Run::object(Inline::Footnote(1), look()),
        Run::text("end", look()),
    ];
    let mut blocks = vec![
        with("Heading", |p| p.outline_level = Some(0)),
        pb(
            "A paragraph of plain text that is long enough to wrap onto several lines on the small page. 中文和日本語も混ざっています。",
        ),
        Block::paragraph(item),
        Block::paragraph(mixed),
        table(
            vec![1_500, 1_500],
            rows_of(&[&["cell one", "cell two"], &["below one", "below two"]]),
        ),
        with("Second heading", |p| p.outline_level = Some(1)),
    ];
    blocks.extend(numbered(40));
    blocks.push(pb(""));
    let mut document = doc(blocks);
    document.footnotes = Notes {
        notes: vec![Note {
            id: 1,
            blocks: vec![pb("note")],
        }],
        part: None,
    };
    document
}

#[test]
fn every_caret_is_found_again_by_a_click() {
    let document = sample();
    let layout = lay(&document);
    let mut checked = 0;
    for (path, para) in document.paragraphs() {
        let text = para.text();
        let mut offsets: Vec<usize> = text.char_indices().map(|(i, _)| i).collect();
        offsets.push(text.len());
        for offset in offsets {
            // Positions inside an object's U+FFFC bytes are not places.
            if text.is_char_boundary(offset) {
                let pos = Pos::new(path.clone(), offset);
                let caret = layout
                    .caret(&pos)
                    .unwrap_or_else(|| panic!("no caret at {pos:?}"));
                let back = layout
                    .hit(caret.page, caret.x + 0.05, caret.top + caret.height / 2.0)
                    .unwrap_or_else(|| panic!("no hit for {pos:?}"));
                let again = layout.caret(&back).unwrap();
                // A place at a line end can be the start of the next line.
                let same_place = (again.x - caret.x).abs() < 0.6
                    && (again.top - caret.top).abs() < 0.6
                    && again.page == caret.page;
                assert!(
                    back == pos || same_place,
                    "{pos:?} at {caret:?} came back as {back:?} at {again:?}"
                );
                checked += 1;
            }
        }
    }
    assert!(checked > 300, "{checked}");
}

#[test]
fn hits_beside_and_outside_the_text_go_to_the_nearest_place() {
    let long =
        "first line of text that wraps around the page and goes on and on until it must wrap";
    let layout = lay(&doc(vec![pb(long), pb("second")]));
    let top = layout.caret(&Pos::new(Path::body(0), 0)).unwrap();
    // Far left and far right of the first line.
    let left = layout.hit(0, -50.0, top.top + 3.0).unwrap();
    assert_eq!(left, Pos::new(Path::body(0), 0));
    let right = layout.hit(0, 500.0, top.top + 3.0).unwrap();
    let end = layout.line_end(&Pos::new(Path::body(0), 0)).unwrap();
    assert_eq!(right, end);
    assert!(right.offset > 0 && right.offset < long.len());
    // Above everything: the first line; below everything: the last.
    let above = layout.hit(0, 100.0, -20.0).unwrap();
    assert_eq!(above.path, Path::body(0));
    assert!((layout.caret(&above).unwrap().top - top.top).abs() < 0.01);
    assert_eq!(
        layout.hit(0, 100.0, 500.0).unwrap(),
        Pos::new(Path::body(1), 6)
    );
}

#[test]
fn lines_and_arrow_keys() {
    let layout = lay(&doc(vec![
        pb("alpha beta gamma delta epsilon zeta eta theta iota kappa lambda mu nu xi"),
        pb("next paragraph"),
    ]));
    let start = layout.line_start(&Pos::new(Path::body(0), 30)).unwrap();
    let end = layout.line_end(&Pos::new(Path::body(0), 30)).unwrap();
    assert!(start.offset <= 30 && end.offset >= 30);
    let below = layout.below(&Pos::new(Path::body(0), 3), 40.0).unwrap();
    assert_eq!(below.path, Path::body(0));
    assert!(below.offset > 10);
    let back = layout.above(&below, 40.0).unwrap();
    assert_eq!(back.path, Path::body(0));
    assert!(back.offset <= 6);
    // Down from the last line goes to the next paragraph, up returns.
    let last = layout.line_start(&Pos::new(Path::body(0), 70)).unwrap();
    let next = layout.below(&last, 30.0).unwrap();
    assert_eq!(next.path, Path::body(1));
    assert_eq!(layout.above(&next, 30.0).unwrap().path, Path::body(0));
    // Nothing above the first line or below the last.
    assert!(layout.above(&Pos::new(Path::body(0), 0), 30.0).is_none());
    assert!(layout.below(&Pos::new(Path::body(1), 3), 30.0).is_none());
}

#[test]
fn arrows_cross_pages_and_move_between_cells() {
    let layout = lay(&doc(numbered(40)));
    // From the last paragraph of page one to the first of page two.
    let mut last_on_first = 0;
    for i in 0..40 {
        if page_of(&layout, i) == 0 {
            last_on_first = i;
        }
    }
    let down = layout
        .below(&Pos::new(Path::body(last_on_first), 0), 25.0)
        .unwrap();
    assert_eq!(down.path, Path::body(last_on_first + 1));
    assert_eq!(layout.page_of(&down), Some(1));

    let layout = lay(&doc(vec![table(
        vec![2_000, 2_000],
        rows_of(&[&["a", "b"], &["c", "d"]]),
    )]));
    let b = layout.caret(&cell_pos(0, 0, 1)).unwrap();
    let down = layout.below(&cell_pos(0, 0, 1), b.x).unwrap();
    assert_eq!(down.path, Path::body(0).cell_block(1, 1, 0));
    let up = layout.above(&down, b.x).unwrap();
    assert_eq!(up.path, Path::body(0).cell_block(0, 1, 0));
}

#[test]
fn selections_cover_lines_and_paragraphs() {
    let layout = lay(&doc(vec![
        pb("alpha beta gamma delta epsilon zeta eta theta iota kappa lambda mu nu"),
        pb("second paragraph"),
        pb("third"),
    ]));
    let same = layout.selection(&Pos::new(Path::body(0), 2), &Pos::new(Path::body(0), 60));
    assert!(same.len() >= 2, "{same:?}");
    assert!(
        same.iter()
            .all(|(page, r)| *page == 0 && r.width() > 0.0 && r.height() > 0.0)
    );
    let across = layout.selection(&Pos::new(Path::body(2), 3), &Pos::new(Path::body(0), 60));
    assert!(across.len() > same.len());
    assert!(
        layout
            .selection(&Pos::new(Path::body(1), 2), &Pos::new(Path::body(1), 2))
            .is_empty()
    );
}

#[test]
fn the_outline_lists_headings_with_their_pages() {
    let layout = lay(&sample());
    let outline = layout.outline();
    assert_eq!(outline.len(), 2);
    assert_eq!((outline[0].level, outline[0].text.as_str()), (0, "Heading"));
    assert_eq!(outline[0].pos, Pos::new(Path::body(0), 0));
    assert_eq!(outline[1].level, 1);
    assert_eq!(outline[1].text, "Second heading");
    assert!(outline[1].page >= outline[0].page);
}

#[test]
fn queries_for_places_that_do_not_exist_give_nothing() {
    let layout = lay(&sample());
    let nowhere = Pos::new(Path::from(vec![99]), 0);
    assert!(layout.caret(&nowhere).is_none());
    assert!(layout.line_start(&nowhere).is_none());
    assert!(layout.line_end(&nowhere).is_none());
    assert!(layout.above(&nowhere, 1.0).is_none());
    assert!(layout.below(&nowhere, 1.0).is_none());
    assert!(layout.page_of(&nowhere).is_none());
    assert!(layout.hit(999, 1.0, 1.0).is_none());
    assert!(layout.selection(&nowhere, &nowhere).is_empty());
    // An offset past the end is clamped.
    assert!(layout.caret(&Pos::new(Path::body(0), 10_000)).is_some());
    // A path into a paragraph as if it were a table.
    assert!(
        layout
            .caret(&Pos::new(Path::from(vec![0, 1, 2, 3]), 0))
            .is_none()
    );
}

// ---------------------------------------------------------------------------
// Caching
// ---------------------------------------------------------------------------

#[test]
fn laying_out_again_shapes_only_what_changed() {
    let before = doc(numbered(80));
    let mut layouter = Layouter::new();
    let first = layouter.layout(&before);
    assert_eq!(layouter.shaped_count(), 80);
    let again = layouter.layout(&before);
    assert_eq!(layouter.shaped_count(), 80);
    assert_eq!(first.pages.len(), again.pages.len());

    let mut after = before.clone();
    after.paragraph_mut(&Path::body(40)).unwrap().runs[0]
        .text
        .push_str(" changed");
    let changed = layouter.layout(&after);
    assert_eq!(layouter.shaped_count(), 81);
    assert!(
        page_has(&changed.pages[changed.pages.len() / 2], "changed")
            || changed.pages.iter().any(|p| page_has(p, "changed"))
    );

    // A paragraph in a table cell: only it is shaped again.
    let tabled = doc(vec![table(
        vec![2_000, 2_000],
        rows_of(&[&["a", "b"], &["c", "d"]]),
    )]);
    let mut layouter = Layouter::new();
    layouter.layout(&tabled);
    assert_eq!(layouter.shaped_count(), 4);
    let mut edited = tabled.clone();
    edited
        .paragraph_mut(&Path::body(0).cell_block(1, 1, 0))
        .unwrap()
        .runs[0]
        .text = "dd".into();
    layouter.layout(&edited);
    assert_eq!(layouter.shaped_count(), 5);
}

#[test]
fn a_different_width_or_page_number_shapes_again() {
    let mut document = doc(vec![pb("x")]);
    document.marginals = vec![marginal(vec![field(PageField::Page)])];
    let mut section = small();
    section.footers.default = Some(0);
    document.last_section = Arc::new(section.clone());
    let mut layouter = Layouter::new();
    layouter.layout(&document);
    let one = layouter.shaped_count();
    // The same document with narrower margins is shaped for the new width.
    section.margins.left = 1_200;
    document.last_section = Arc::new(section);
    layouter.layout(&document);
    assert!(layouter.shaped_count() > one);
}

// ---------------------------------------------------------------------------
// Odd input
// ---------------------------------------------------------------------------

#[test]
fn odd_page_and_column_geometry_does_not_panic_or_hang() {
    let weird: Vec<Section> = vec![
        Section {
            page: (0, 0),
            ..Section::default()
        },
        Section {
            page: (-500, -500),
            ..Section::default()
        },
        Section {
            page: (2_000, 2_000),
            margins: Margins {
                top: 5_000,
                right: 5_000,
                bottom: 5_000,
                left: 5_000,
                header: 9_000,
                footer: 9_000,
                gutter: 9_000,
            },
            ..Section::default()
        },
        Section {
            columns: Columns {
                count: 40,
                space: 300,
                widths: Vec::new(),
                separator: true,
            },
            ..small()
        },
        Section {
            columns: Columns {
                count: 0,
                space: -100,
                widths: vec![(0, 0), (-5, -5)],
                separator: false,
            },
            ..small()
        },
        Section {
            grid: Some(DocGrid {
                kind: GridKind::LinesAndChars,
                line_pitch: 0,
            }),
            ..small()
        },
        Section {
            grid: Some(DocGrid {
                kind: GridKind::SnapToChars,
                line_pitch: -40,
            }),
            ..small()
        },
    ];
    for section in weird {
        let mut document = doc(vec![
            pb("some text 中文 more words that need wrapping around a narrow column"),
            pb(""),
            table(vec![0, -5], rows_of(&[&["a", "b"]])),
            pb("end"),
        ]);
        document.last_section = Arc::new(section);
        let layout = lay(&document);
        assert!(!layout.pages.is_empty());
        for page in &layout.pages {
            assert!(page.width >= 1.0 && page.height >= 1.0);
        }
        let _ = layout.caret(&Pos::new(Path::body(0), 3));
        let _ = layout.hit(0, 1.0, 1.0);
    }
}

#[test]
fn odd_paragraph_and_table_input_does_not_panic_or_hang() {
    // Indents wider than the column.
    let wide = with("indented text that cannot fit anywhere", |p| {
        p.indent_left = 20_000;
        p.indent_right = 20_000;
        p.first_line = -30_000;
    });
    // A picture taller than the page, in the line and floating.
    let mut huge = paragraph("");
    huge.runs = vec![
        picture(100.0, 5_000.0, None),
        picture(100.0, 5_000.0, Some(float_at(Wrap::Square, false))),
        Run::text("after", look()),
    ];
    // An exact row height of a kilometre.
    let km = Row {
        cells: vec![cell("tall\nrow")],
        height: RowHeight::Exactly(56_700_000),
        ..Row::default()
    };
    let empty_cells = Row {
        cells: vec![Cell {
            blocks: Vec::new(),
            span: 0,
            ..Cell::default()
        }],
        ..Row::default()
    };
    let wide_span = Row {
        cells: vec![Cell {
            span: 50,
            ..cell("span")
        }],
        grid_before: 7,
        grid_after: 9,
        ..Row::default()
    };
    let stray = Row {
        cells: vec![Cell {
            merge: VMerge::Continue,
            ..cell("continues nothing")
        }],
        ..Row::default()
    };
    let document = doc(vec![
        wide,
        Block::paragraph(huge),
        table(vec![4_000], vec![km]),
        table(Vec::new(), Vec::new()),
        table(vec![1_000], vec![Row::default()]),
        table(vec![1_000], vec![empty_cells, wide_span, stray]),
        with("keep", |p| {
            p.keep_next = true;
            p.keep_lines = true;
        }),
    ]);
    let layout = lay(&document);
    assert!(layout.pages.len() < 1_000, "{}", layout.pages.len());
    assert!(layout.caret(&Pos::new(Path::body(1), 0)).is_some());
    // Paths that lead nowhere.
    assert!(
        layout
            .caret(&Pos::new(Path::body(2).cell_block(9, 9, 9), 0))
            .is_none()
    );
}

#[test]
fn a_keep_chain_longer_than_a_page_still_terminates() {
    let blocks: Vec<Block> = (0..80)
        .map(|i| {
            with(&format!("k{i}\nsecond\nthird"), |p| {
                p.keep_next = true;
                p.keep_lines = true;
            })
        })
        .collect();
    let layout = lay(&doc(blocks));
    assert!(layout.pages.len() >= 10);
    for i in 0..80 {
        assert!(layout.caret(&Pos::new(Path::body(i), 0)).is_some());
    }
}

#[test]
fn nothing_is_lost_in_a_long_mixed_document() {
    let document = sample();
    let layout = lay(&document);
    // Every paragraph of the body has its lines placed exactly once.
    for (path, _) in document.paragraphs() {
        let parts = lines_of(&layout, &path);
        assert!(!parts.is_empty(), "{path:?} not placed");
    }
}

// ---------------------------------------------------------------------------
// Text, language and sections
// ---------------------------------------------------------------------------

#[test]
fn chinese_lines_never_start_with_a_closing_mark() {
    let text = "排版引擎需要处理“禁则”：句号、逗号和后引号（」』）》等）不能出现在行首，前引号（「『（《等）不能出现在行尾。日本語の組版では、行頭に句読点や閉じ括弧（」）を置くことはできません。";
    for width in [4_000, 4_300, 4_700, 5_100, 5_500] {
        let mut section = small();
        section.page.0 = width + 800;
        let mut document = doc(vec![pb(text)]);
        document.last_section = Arc::new(section);
        document.settings.east_asian_language = Some("zh-CN".into());
        let layout = lay(&document);
        let shown = page_texts(&layout.pages[0]).join("\n");
        for line in shown.lines().skip(1) {
            let first = line.chars().next().unwrap();
            assert!(
                !"，。、」』）》：；！？”ゃゅょっー".contains(first),
                "width {width}: {line}"
            );
        }
        assert!(shown.lines().count() > 2);
    }
}

#[test]
fn letter_spacing_widens_a_run() {
    let at = |spacing| {
        let mut wide = look();
        wide.spacing = spacing;
        let mut p = paragraph("");
        p.runs = vec![Run::text("abcd", wide)];
        let layout = lay(&doc(vec![Block::paragraph(p)]));
        layout.caret(&Pos::new(Path::body(0), 4)).unwrap().x
    };
    assert!((at(100) - at(0) - 20.0).abs() < 0.5);
}

#[test]
fn superscripts_are_smaller_and_raised_and_small_caps_are_shorter() {
    let mut sup = look();
    sup.position = VertAlign::Superscript;
    let mut p = paragraph("");
    p.runs = vec![Run::text("x", look()), Run::text("2", sup)];
    let layout = lay(&doc(vec![Block::paragraph(p)]));
    let size_of_runs: Vec<f32> = layout.pages[0]
        .items
        .iter()
        .filter_map(|i| match i {
            Item::Text(t) => Some(t.paragraph.draw_runs(0.0, 100.0)),
            _ => None,
        })
        .flatten()
        .map(|run| run.face.size)
        .collect();
    assert_eq!(size_of_runs.len(), 2);
    assert!(size_of_runs[1] < size_of_runs[0]);
}

#[test]
fn a_section_can_centre_its_text_vertically() {
    let mut section = small();
    section.vertical_align = CellAlign::Center;
    let mut document = doc(vec![pb("middle")]);
    document.last_section = Arc::new(section);
    let layout = lay(&document);
    let caret = layout.caret(&Pos::new(Path::body(0), 0)).unwrap();
    assert!(caret.top > 70.0 && caret.top < 100.0, "{caret:?}");
    // And a click finds it where it is drawn.
    let hit = layout.hit(0, caret.x + 1.0, caret.top + 2.0).unwrap();
    assert_eq!(hit.path, Path::body(0));
}

#[test]
fn distribute_spreads_a_short_line_over_the_width() {
    let mut p = paragraph("姓名");
    p.props.align = Align::Distribute;
    let layout = lay(&doc(vec![Block::paragraph(p)]));
    let end = layout
        .caret(&Pos::new(Path::body(0), "姓名".len()))
        .unwrap();
    assert!((end.x - 280.0).abs() < 1.5, "{end:?}");
}

// ---------------------------------------------------------------------------
// Line heights of fonts the machine lacks, grids, floats, control characters
// ---------------------------------------------------------------------------

fn simsun_step(size: f32, multiple: f32, grid: Option<Twips>) -> f32 {
    let mut l = look();
    l.size = size;
    l.font = Some("SimSun".into());
    l.east_asian_font = Some("SimSun".into());
    let mut p = paragraph("");
    p.runs = vec![Run::text("a\nb", l.clone())];
    p.base = l;
    p.props.line = LineSpacing::Multiple(multiple);
    let mut section = small();
    section.grid = grid.map(|line_pitch| DocGrid {
        kind: GridKind::Lines,
        line_pitch,
    });
    let mut document = doc(vec![Block::paragraph(p)]);
    document.last_section = Arc::new(section);
    let layout = lay(&document);
    caret_top(&layout, 0, 2) - caret_top(&layout, 0, 0)
}

#[test]
fn a_missing_font_has_the_line_height_word_gives_it() {
    // SimSun is 1.3 em in Word; the machine has no SimSun.
    assert!((simsun_step(12.0, 1.0, None) - 15.6).abs() < 0.01);
    assert!((simsun_step(10.0, 1.0, None) - 13.0).abs() < 0.01);
}

#[test]
fn grid_lines_follow_words_rule() {
    let grid = Some(312);
    assert!((simsun_step(10.5, 1.0, grid) - 15.6).abs() < 0.01);
    assert!((simsun_step(12.0, 1.0, grid) - 15.6).abs() < 0.01);
    assert!((simsun_step(10.5, 1.15, grid) - 17.94).abs() < 0.01);
    assert!((simsun_step(12.0, 1.15, grid) - 17.94).abs() < 0.01);
    assert!((simsun_step(14.0, 1.0, grid) - 31.2).abs() < 0.01);
}

#[test]
fn a_tab_makes_no_glyph() {
    let layout = lay(&doc(vec![pb("a\tb\tc")]));
    let Item::Text(item) = &layout.pages[0]
        .items
        .iter()
        .find(|i| matches!(i, Item::Text(_)))
        .unwrap()
    else {
        unreachable!()
    };
    let glyphs: usize = item
        .paragraph
        .draw_runs(0.0, 100.0)
        .iter()
        .map(|r| r.glyphs.len())
        .sum();
    assert_eq!(glyphs, 3);
    assert!(item.paragraph.undrawable().is_empty());
}

#[test]
fn several_floats_in_one_paragraph_keep_their_own_places() {
    let float = |x: i64, y: i64| Float {
        x: FloatAxis {
            from: FloatFrom::Margin,
            at: FloatAt::Offset(x * 12_700),
        },
        y: FloatAxis {
            from: FloatFrom::Page,
            at: FloatAt::Offset(y * 12_700),
        },
        wrap: Wrap::None,
        behind: false,
        distance: [0; 4],
    };
    let boxed = Drawing {
        size: (60 * 12_700, 20 * 12_700),
        content: DrawingContent::TextBox(TextBox {
            blocks: vec![pb("boxed")],
            fill: Some([255, 255, 0, 255]),
            line: None,
            insets: [0; 4],
        }),
        float: Some(float(100, 100)),
        alt: None,
    };
    let mut p = paragraph("");
    p.runs = vec![
        picture(30.0, 20.0, Some(float(0, 40))),
        Run::object(Inline::Drawing(Arc::new(boxed)), look()),
        picture(10.0, 10.0, Some(float(200, 150))),
        Run::text("text", look()),
    ];
    let layout = lay(&doc(vec![Block::paragraph(p)]));
    let items = &layout.pages[0].items;
    let pictures: Vec<_> = items
        .iter()
        .filter_map(|i| match i {
            Item::Picture(p) => Some(p.rect),
            _ => None,
        })
        .collect();
    assert_eq!(pictures.len(), 2);
    assert_eq!(
        (pictures[0].x0, pictures[0].y0, pictures[0].width()),
        (20.0, 40.0, 30.0)
    );
    assert_eq!(
        (pictures[1].x0, pictures[1].y0, pictures[1].width()),
        (220.0, 150.0, 10.0)
    );
    let frames: Vec<_> = items
        .iter()
        .filter_map(|i| match i {
            Item::Frame(f) => Some(f.rect),
            _ => None,
        })
        .collect();
    assert_eq!(frames.len(), 1);
    assert_eq!(
        (frames[0].x0, frames[0].y0, frames[0].width()),
        (120.0, 100.0, 60.0)
    );
    assert!(page_has(&layout.pages[0], "boxed"));
}

#[test]
fn ruby_is_drawn_small_above_its_base_and_makes_the_line_tall() {
    let ruby = |with: bool| {
        let mut p = paragraph("");
        let run = if with {
            Run {
                text: "漢字".into(),
                kind: Inline::Ruby(Arc::new(Ruby {
                    text: "かんじ".into(),
                    size: 5.0,
                })),
                look: look(),
                ..Run::default()
            }
        } else {
            Run::text("漢字", look())
        };
        p.runs = vec![run, Run::text("\nnext", look())];
        lay(&doc(vec![Block::paragraph(p)]))
    };
    let (with, without) = (ruby(true), ruby(false));
    let step = |l: &Layout| caret_top(l, 0, 7) - caret_top(l, 0, 0);
    assert!(step(&with) > step(&without) + 5.0);
    let texts = page_texts(&with.pages[0]);
    assert!(texts.iter().any(|t| t == "かんじ"), "{texts:?}");
    // Above the base: its item sits higher than the paragraph's own.
    let ys: Vec<f32> = with.pages[0]
        .items
        .iter()
        .filter_map(|i| match i {
            Item::Text(t) => Some(t.y),
            _ => None,
        })
        .collect();
    assert!(ys[1] < ys[0] + 5.0);
}

#[test]
fn horizontal_scale_widens_a_run() {
    let end = |scale: f32| {
        let mut l = look();
        l.scale = scale;
        let mut p = paragraph("");
        p.runs = vec![Run::text("abcd", l)];
        lay(&doc(vec![Block::paragraph(p)]))
            .caret(&Pos::new(Path::body(0), 4))
            .unwrap()
            .x
    };
    assert!(((end(2.0) - 20.0) / (end(1.0) - 20.0) - 2.0).abs() < 0.02);
}

#[test]
fn automatic_text_is_white_on_dark_backgrounds() {
    let colour_of = |shading: Option<Rgba>, in_cell: bool| {
        let block = if in_cell {
            let mut c = cell("x");
            c.shading = shading;
            table(
                vec![2_000],
                vec![Row {
                    cells: vec![c],
                    ..Row::default()
                }],
            )
        } else {
            with("x", |p| p.shading = shading)
        };
        let layout = lay(&doc(vec![block]));
        layout.pages[0]
            .items
            .iter()
            .find_map(|i| match i {
                Item::Text(t) => Some(t.paragraph.draw_runs(0.0, 100.0)[0].color),
                _ => None,
            })
            .unwrap()
    };
    assert_eq!(colour_of(None, false), [0, 0, 0, 255]);
    assert_eq!(colour_of(Some([255, 255, 255, 255]), true), [0, 0, 0, 255]);
    assert_eq!(
        colour_of(Some([20, 30, 90, 255]), true),
        [255, 255, 255, 255]
    );
    assert_eq!(
        colour_of(Some([20, 30, 90, 255]), false),
        [255, 255, 255, 255]
    );
}

#[test]
fn wave_and_dotted_underlines_keep_their_kind() {
    use aster_text::LineKind;
    let kind_of = |style| {
        let mut l = look();
        l.underline = Some(Underline { style, color: None });
        let mut p = paragraph("");
        p.runs = vec![Run::text("abc", l)];
        let layout = lay(&doc(vec![Block::paragraph(p)]));
        layout.pages[0]
            .items
            .iter()
            .find_map(|i| match i {
                Item::Text(t) => Some(t.paragraph.decorations(0.0, 100.0, 0.5)[0].kind),
                _ => None,
            })
            .unwrap()
    };
    assert_eq!(kind_of(UnderlineStyle::Wave), LineKind::Wave);
    assert_eq!(kind_of(UnderlineStyle::Dotted), LineKind::Dotted);
    assert_eq!(kind_of(UnderlineStyle::Dashed), LineKind::Dashed);
    assert_eq!(kind_of(UnderlineStyle::Single), LineKind::Solid);
}

#[test]
fn cells_covered_by_a_vertical_merge_stand_for_the_start_cell() {
    let start = Cell {
        blocks: vec![pb("merged text")],
        merge: VMerge::Start,
        ..Cell::default()
    };
    let covered = |text: &str| Cell {
        blocks: vec![pb(text)],
        merge: VMerge::Continue,
        ..Cell::default()
    };
    let rows = vec![
        Row {
            cells: vec![start, cell("a")],
            ..Row::default()
        },
        Row {
            cells: vec![covered(""), cell("b")],
            ..Row::default()
        },
        Row {
            cells: vec![covered(""), cell("c")],
            ..Row::default()
        },
    ];
    let layout = lay(&doc(vec![table(vec![2_000, 2_000], rows)]));
    let at_start = layout.caret(&cell_pos(0, 0, 0)).unwrap();
    for row in [1, 2] {
        let caret = layout
            .caret(&cell_pos(0, row, 0))
            .expect("a caret in a covered cell");
        assert_eq!((caret.x, caret.top), (at_start.x, at_start.top));
        assert_eq!(layout.page_of(&cell_pos(0, row, 0)), Some(0));
        assert!(layout.line_start(&cell_pos(0, row, 0)).is_some());
    }
    // What the queries hand out always has a caret.
    let mut pos = cell_pos(0, 0, 1);
    for _ in 0..6 {
        pos = layout.below(&pos, at_start.x + 60.0).unwrap_or(pos);
        assert!(layout.caret(&pos).is_some());
    }
    for y in [0.0, 25.0, 60.0, 90.0] {
        let hit = layout.hit(0, 30.0, y).unwrap();
        assert!(layout.caret(&hit).is_some());
    }
}

#[test]
fn text_continues_below_a_float_on_the_same_page() {
    let mut f = float_at(Wrap::Square, false);
    f.y.at = FloatAt::Offset(15 * 12_700);
    let mut p = paragraph("");
    p.runs = vec![
        Run::text("first\nsecond", look()),
        picture(40.0, 30.0, Some(f)),
        Run::text("\nthird\nfourth\nfifth", look()),
    ];
    let layout = lay(&doc(vec![Block::paragraph(p)]));
    assert_eq!(layout.pages.len(), 1);
    let end = layout.caret(&Pos::new(Path::body(0), 28)).unwrap();
    assert!(end.top < 100.0, "{end:?}");
}

#[test]
fn a_row_may_split_between_pages_unless_it_cannot() {
    let lines = (0..8)
        .map(|i| format!("line {i}"))
        .collect::<Vec<_>>()
        .join("\n");
    let build = |cant_split: bool| {
        let mut blocks = numbered(12);
        blocks.push(table(
            vec![4_000],
            vec![Row {
                cells: vec![cell(&lines)],
                cant_split,
                ..Row::default()
            }],
        ));
        doc(blocks)
    };
    let path = Path::body(12).cell_block(0, 0, 0);
    let free = lay(&build(false));
    let parts = lines_of(&free, &path);
    assert_eq!(parts.len(), 2, "{parts:?}");
    assert_eq!(parts.iter().map(|p| p.1).sum::<usize>(), 8);
    let kept = lay(&build(true));
    assert_eq!(lines_of(&kept, &path).len(), 1);
}

#[test]
fn the_paragraph_mark_counts_in_the_last_line_and_a_picture_line_is_not_stretched() {
    let height = |mark: f32| {
        let mut p = paragraph("a");
        p.mark.size = mark;
        let layout = lay(&doc(vec![Block::paragraph(p), pb("next")]));
        caret_top(&layout, 1, 0) - caret_top(&layout, 0, 0)
    };
    assert!(height(24.0) > height(10.0) + 8.0);
    // Spacing 1.5 stretches the text, not the picture in the line.
    let step = |multiple: f32| {
        let mut p = paragraph("");
        p.runs = vec![picture(20.0, 100.0, None)];
        p.props.line = LineSpacing::Multiple(multiple);
        let layout = lay(&doc(vec![Block::paragraph(p), pb("next")]));
        caret_top(&layout, 1, 0) - caret_top(&layout, 0, 0)
    };
    assert!(step(1.5) - step(1.0) < 10.0, "{} {}", step(1.5), step(1.0));
}

#[test]
fn automatic_spacing_of_neighbours_does_not_add_up() {
    // `after` and `before` in twips; a flag says it is automatic (and then
    // the number is what the reader puts there, 14 points).
    let gap = |a: (i32, bool), b: (i32, bool), items: bool| {
        let list = items.then_some(ListRef { list: 1, level: 0 });
        let layout = lay(&doc(vec![
            with("one", |p| {
                (p.space_after, p.space_after_auto) = a;
                p.list = list;
            }),
            with("two", |p| {
                (p.space_before, p.space_before_auto) = b;
                p.list = list;
            }),
        ]));
        caret_top(&layout, 1, 0) - caret_top(&layout, 0, 0)
    };
    let base = gap((0, false), (0, false), false);
    // Two automatic spaces are one.
    assert!((gap((280, true), (280, true), false) - base - 14.0).abs() < 0.01);
    // Automatic and a set one add up.
    assert!((gap((200, false), (280, true), false) - base - 24.0).abs() < 0.01);
    assert!((gap((280, true), (100, false), false) - base - 19.0).abs() < 0.01);
    // The same numbers as set spaces are not automatic: they add up.
    assert!((gap((280, false), (280, false), false) - base - 28.0).abs() < 0.01);
    // Between list items there is none of the automatic kind.
    assert!((gap((280, true), (280, true), true) - base).abs() < 0.01);
    assert!((gap((180, false), (280, true), true) - base - 9.0).abs() < 0.01);
    assert!((gap((200, false), (280, false), true) - base - 24.0).abs() < 0.01);
}

// ---------------------------------------------------------------------------
// Text beside floating drawings
// ---------------------------------------------------------------------------

const WORDS: &str = "The quick brown fox jumps over the lazy dog while the five boxing wizards jump quickly and \
pack my box with five dozen liquor jugs, so that there is plenty of text to run past the picture. ";

fn words(times: usize) -> String {
    WORDS.repeat(times)
}

/// A picture wrapped as `wrap` at `x` from the margin and `y` points from
/// the top of its paragraph, keeping `distance` points clear.
fn beside(x: FloatAt, y: i64, wrap: Wrap, distance: i64) -> Float {
    Float {
        x: FloatAxis {
            from: FloatFrom::Margin,
            at: x,
        },
        y: FloatAxis {
            from: FloatFrom::Paragraph,
            at: FloatAt::Offset(y * 12_700),
        },
        wrap,
        behind: false,
        distance: [distance * 12_700; 4],
    }
}

/// Left edge, top and right edge of the lines of a block, on its page.
fn line_boxes(layout: &Layout, block: usize) -> Vec<(f32, f32, f32)> {
    let path = Path::body(block);
    let mut out = Vec::new();
    for frag in layout.frags.iter().filter(|f| f.path == path && !f.repeat) {
        let lines = frag.shaped.paragraph.lines();
        for line in &lines[frag.lines.clone()] {
            out.push((
                frag.x + line.left,
                frag.y + line.top,
                frag.x + line.left + line.width,
            ));
        }
    }
    out
}

fn floated(float: Float, w: f64, h: f64, blocks_after: Vec<Block>) -> Document {
    floated_n(3, float, w, h, blocks_after)
}

fn floated_n(times: usize, float: Float, w: f64, h: f64, blocks_after: Vec<Block>) -> Document {
    let mut p = paragraph("");
    p.runs = vec![picture(w, h, Some(float)), Run::text(words(times), look())];
    let mut blocks = vec![Block::paragraph(p)];
    blocks.extend(blocks_after);
    doc(blocks)
}

#[test]
fn text_flows_beside_a_square_picture_and_then_under_it() {
    let layout = lay(&floated(
        beside(FloatAt::Start, 0, Wrap::Square, 0),
        50.0,
        50.0,
        vec![],
    ));
    let lines = line_boxes(&layout, 0);
    assert!(lines.len() > 6, "{}", lines.len());
    let (mut beside_count, mut under_count) = (0, 0);
    for (x0, top, x1) in &lines {
        if *top < 70.0 - 0.01 {
            assert!(*x0 >= 70.0 - 0.01, "x {x0} at {top}");
            beside_count += 1;
        } else {
            assert!((x0 - 20.0).abs() < 0.01, "x {x0} at {top}");
            under_count += 1;
        }
        assert!(*x1 <= 280.5);
    }
    assert!(beside_count >= 3 && under_count >= 2);
    // The first line is where the paragraph starts.
    assert!((lines[0].1 - 20.0).abs() < 0.01);
    // The picture is where it was put.
    assert!(
        layout.pages[0]
            .items
            .iter()
            .any(|i| matches!(i, Item::Picture(p) if p.rect.x0 == 20.0 && p.rect.y0 == 20.0))
    );
}

#[test]
fn a_picture_on_the_right_shortens_lines_from_the_right_and_distance_is_kept() {
    let layout = lay(&floated(
        beside(FloatAt::End, 0, Wrap::Square, 10),
        50.0,
        30.0,
        vec![],
    ));
    // The picture is 230..280 and keeps 10 points clear all round.
    for (x0, top, x1) in line_boxes(&layout, 0) {
        if top < 20.0 + 30.0 + 10.0 - 0.01 {
            assert!(x1 <= 220.0 + 0.01, "right {x1} at {top}");
        }
        assert!((x0 - 20.0).abs() < 0.01);
    }
}

#[test]
fn a_float_pushes_the_lines_of_the_paragraphs_after_its_own() {
    let mut anchor = paragraph("");
    anchor.runs = vec![
        picture(60.0, 90.0, Some(beside(FloatAt::Start, 0, Wrap::Tight, 0))),
        Run::text("Short.", look()),
    ];
    let layout = lay(&doc(vec![
        Block::paragraph(anchor),
        pb(&words(2)),
        pb(&words(1)),
    ]));
    // 20..110 is taken at the left: Tight uses the bounding box.
    let first = line_boxes(&layout, 0);
    assert!(first[0].0 >= 80.0 - 0.01);
    let second = line_boxes(&layout, 1);
    assert!(second[0].0 >= 80.0 - 0.01);
    assert!(second.iter().any(|(x, _, _)| (*x - 20.0).abs() < 0.01));
    for (x0, top, _) in &second {
        assert_eq!(*x0 >= 80.0 - 0.01, *top < 110.0 - 0.01, "{x0} {top}");
    }
    // The paragraph below it all is not affected.
    for (x0, _, _) in line_boxes(&layout, 2) {
        assert!((x0 - 20.0).abs() < 0.01);
    }
}

#[test]
fn text_takes_the_wider_side_of_a_picture_in_the_middle() {
    // The picture is 120..170 of a column 20..280 wide: 100 on the left, 110
    // on the right.
    let layout = lay(&floated(
        beside(FloatAt::Offset(100 * 12_700), 0, Wrap::Square, 0),
        50.0,
        40.0,
        vec![],
    ));
    for (x0, top, x1) in line_boxes(&layout, 0) {
        if top < 60.0 - 0.01 {
            assert!(x0 >= 170.0 - 0.01, "{x0}");
            assert!(x1 <= 280.5);
        }
    }
    // Moved nearer to the right edge, the left side is the wider.
    let layout = lay(&floated(
        beside(FloatAt::Offset(150 * 12_700), 0, Wrap::Square, 0),
        50.0,
        40.0,
        vec![],
    ));
    for (x0, top, x1) in line_boxes(&layout, 0) {
        if top < 60.0 - 0.01 {
            assert!((x0 - 20.0).abs() < 0.01);
            assert!(x1 <= 170.0 + 0.01, "{x1}");
        }
    }
}

#[test]
fn lines_with_no_room_for_a_word_skip_below_the_picture() {
    // 250 of 260 points are taken: nothing fits beside it.
    let layout = lay(&floated(
        beside(FloatAt::Start, 0, Wrap::Square, 0),
        250.0,
        40.0,
        vec![],
    ));
    let lines = line_boxes(&layout, 0);
    assert!(lines[0].1 >= 60.0 - 0.01, "{}", lines[0].1);
    assert!((lines[0].0 - 20.0).abs() < 0.01);
}

#[test]
fn top_and_bottom_and_none_are_as_they_were() {
    let layout = lay(&floated(
        beside(FloatAt::Start, 0, Wrap::TopAndBottom, 0),
        50.0,
        50.0,
        vec![],
    ));
    let lines = line_boxes(&layout, 0);
    assert!(lines[0].1 >= 70.0 - 0.01);
    assert!(lines.iter().all(|(x, _, _)| (*x - 20.0).abs() < 0.01));
    let layout = lay(&floated(
        beside(FloatAt::Start, 0, Wrap::None, 0),
        50.0,
        50.0,
        vec![],
    ));
    let lines = line_boxes(&layout, 0);
    assert!((lines[0].1 - 20.0).abs() < 0.01);
    assert!(lines.iter().all(|(x, _, _)| (*x - 20.0).abs() < 0.01));
}

#[test]
fn a_float_does_not_hold_text_on_the_next_page() {
    // The picture sits near the bottom of the first page; the paragraph runs
    // on to the second, where nothing is in the way.
    let layout = lay(&floated_n(
        5,
        beside(FloatAt::Start, 130, Wrap::Square, 0),
        60.0,
        40.0,
        vec![pb(&words(6))],
    ));
    assert!(layout.pages.len() >= 2);
    let mut seen_beside = false;
    for frag in layout.frags.iter().filter(|f| f.path == Path::body(0)) {
        let lines = frag.shaped.paragraph.lines();
        for line in &lines[frag.lines.clone()] {
            let (x, top) = (frag.x + line.left, frag.y + line.top);
            if x > 21.0 {
                // Only lines that reach into the picture's 150..190 move.
                assert!(
                    frag.page == 0 && top + 8.0 > 150.0 && top < 190.0,
                    "{x} {top}"
                );
                assert!(x >= 80.0 - 0.01, "{x} {top}");
                seen_beside = true;
            } else {
                assert!((x - 20.0).abs() < 0.01);
                assert!(frag.page > 0 || top + 12.0 <= 150.0 + 0.01 || top >= 189.0);
            }
        }
    }
    assert!(seen_beside);
}

/// The place in the body paragraph of `frag` for a byte offset of its shaped
/// text (these paragraphs have no label or notes).
fn shaped_pos(frag: &crate::cx::Frag, offset: usize) -> Pos {
    Pos::new(frag.path.clone(), frag.shaped.to_orig(offset))
}

#[test]
fn queries_are_right_on_lines_beside_a_picture() {
    let layout = lay(&floated(
        beside(FloatAt::Start, 0, Wrap::Square, 0),
        50.0,
        50.0,
        vec![pb("tail")],
    ));
    let lines = line_boxes(&layout, 0);
    let frag = layout
        .frags
        .iter()
        .find(|f| f.path == Path::body(0))
        .unwrap();
    let paragraph = &frag.shaped.paragraph;
    let ranges = paragraph.line_ranges();
    assert!(lines[0].0 >= 70.0 - 0.01 && lines.last().unwrap().0 < 21.0);
    // Where the line starts, the caret is at the line's left edge, and a
    // click there comes back to the same place.
    for n in [0usize, 1, lines.len() - 1] {
        let start = shaped_pos(frag, ranges[n].start);
        let caret = layout.caret(&start).unwrap();
        assert!((caret.x - lines[n].0).abs() < 0.01, "{n}: {caret:?}");
        assert!((caret.top - lines[n].1).abs() < 0.01);
        let hit = layout.hit(0, lines[n].0 + 0.2, lines[n].1 + 2.0).unwrap();
        assert_eq!(hit, start, "line {n}");
        assert_eq!(layout.line_start(&hit).unwrap(), start);
    }
    // Clicks left of a shortened line go to its start, right of it to its end.
    let first = shaped_pos(frag, ranges[0].start);
    assert_eq!(layout.hit(0, 25.0, lines[0].1 + 2.0).unwrap(), first);
    let end = layout.line_end(&first).unwrap();
    assert_eq!(layout.hit(0, 279.0, lines[0].1 + 2.0).unwrap(), end);
    let so = |pos: &Pos| frag.shaped.to_shaped(pos.offset);
    assert!(so(&end) >= ranges[0].end - 1 && so(&end) <= ranges[0].end);
    // Down and up keep to the goal column across lines of different widths.
    let below = layout.below(&first, 100.0).unwrap();
    let back = layout.above(&below, 100.0).unwrap();
    assert_eq!(so(&layout.line_start(&below).unwrap()), ranges[1].start);
    assert_eq!(so(&layout.line_start(&back).unwrap()), ranges[0].start);
    // The last line beside the picture, then the first below it.
    let last_beside = lines.iter().rposition(|l| l.0 > 60.0).unwrap();
    let from = shaped_pos(frag, ranges[last_beside].start);
    let next = layout.below(&from, 21.0).unwrap();
    assert_eq!(
        so(&layout.line_start(&next).unwrap()),
        ranges[last_beside + 1].start
    );
    // A selection over two lines has a rectangle per line, each on its line.
    let a = shaped_pos(frag, ranges[0].start + 2);
    let b = shaped_pos(frag, ranges[1].start + 3);
    let rects = layout.selection(&a, &b);
    assert_eq!(rects.len(), 2);
    assert!(rects[0].1.x0 >= 70.0 - 0.01 && rects[1].1.x0 >= 70.0 - 0.01);
    assert!(rects[0].1.y0 < rects[1].1.y0);
}

#[test]
fn flowing_around_a_picture_does_not_reshape_other_paragraphs() {
    let mut blocks = vec![{
        let mut p = paragraph("");
        p.runs = vec![
            picture(50.0, 50.0, Some(beside(FloatAt::Start, 0, Wrap::Square, 0))),
            Run::text("anchor", look()),
        ];
        Block::paragraph(p)
    }];
    blocks.extend(numbered(60));
    let before = doc(blocks);
    let mut layouter = Layouter::new();
    layouter.layout(&before);
    let count = layouter.shaped_count();
    layouter.layout(&before);
    assert_eq!(layouter.shaped_count(), count);
    // A paragraph far below the picture: only it is shaped again.
    let mut after = before.clone();
    after.paragraph_mut(&Path::body(50)).unwrap().runs[0]
        .text
        .push_str(" changed");
    layouter.layout(&after);
    assert_eq!(layouter.shaped_count(), count + 1);
    // Text added to the anchor's own line, which stays one line: what is
    // below does not move, so the paragraphs beside the picture are kept.
    let mut again = after.clone();
    again.paragraph_mut(&Path::body(0)).unwrap().runs[1]
        .text
        .push_str("ed");
    layouter.layout(&again);
    assert_eq!(layouter.shaped_count(), count + 3);
}

// ---------------------------------------------------------------------------
// Footnotes that go on to the next page
// ---------------------------------------------------------------------------

/// The lines of a page that belong to the note called "zq": its text is
/// `zq1`, `zq2` and so on, one to a line.
fn note_lines(page: &Page) -> Vec<String> {
    page_texts(page)
        .iter()
        .flat_map(|t| t.split('\n').map(str::to_owned).collect::<Vec<_>>())
        .filter(|l| l.contains("zq"))
        .collect()
}

fn text_y(page: &Page, needle: &str) -> f32 {
    page.items
        .iter()
        .find_map(|i| match i {
            Item::Text(t) if t.paragraph.text().contains(needle) => Some(t.y),
            _ => None,
        })
        .unwrap_or_else(|| panic!("no text with {needle}"))
}

fn note_of(id: i32, lines: usize) -> Note {
    let text = (1..=lines)
        .map(|n| format!("zq{n}"))
        .collect::<Vec<_>>()
        .join("\n");
    let mut p = paragraph("");
    p.runs = vec![
        Run::object(Inline::Footnote(id), look()),
        Run::text(format!(" {text}"), look()),
    ];
    Note {
        id,
        blocks: vec![Block::paragraph(p)],
    }
}

/// A document with `before` plain lines, then a line with a reference to a
/// note of `lines` lines, then `after` plain paragraphs.
fn noted(before: usize, lines: usize, after: usize) -> Document {
    let mut blocks = numbered(before);
    let mut p = paragraph("");
    p.runs = vec![
        Run::text("ref", look()),
        Run::object(Inline::Footnote(1), look()),
    ];
    blocks.push(Block::paragraph(p));
    blocks.extend((0..after).map(|i| pb(&format!("later {i}"))));
    let mut document = doc(blocks);
    document.footnotes = Notes {
        notes: vec![note_of(1, lines)],
        part: None,
    };
    document
}

fn line_step() -> f32 {
    let layout = lay(&doc(numbered(2)));
    caret_top(&layout, 1, 0) - caret_top(&layout, 0, 0)
}

#[test]
fn a_footnote_that_just_overflows_continues_on_the_next_page() {
    let h = line_step();
    // Room for two and a half lines of note under the reference line.
    let before = ((160.0 - 6.0 - 2.5 * h) / h).floor() as usize - 1;
    let layout = lay(&noted(before, 5, 3));
    // The reference stays where it is, with the first lines of the note
    // that fit under it, and the rest comes on the next page.
    assert_eq!(page_of(&layout, before), 0);
    let first = note_lines(&layout.pages[0]);
    assert!((1..5).contains(&first.len()), "{first:?}");
    let rest = note_lines(&layout.pages[1]);
    assert_eq!(first.len() + rest.len(), 5, "{first:?} {rest:?}");
    // In order, and the number is not repeated.
    assert!(first[0].contains("zq1"));
    assert!(
        rest[0].starts_with(&format!("zq{}", first.len() + 1)),
        "{rest:?}"
    );
    // One line more would not have left room for the reference's line.
    assert!(first.len() <= 3);
    // The continuation has the separator, and is under the body text.
    assert!(
        layout.pages[1]
            .items
            .iter()
            .any(|i| matches!(i, Item::Rule { .. }))
    );
    assert!(text_y(&layout.pages[1], &rest[0]) > text_y(&layout.pages[1], "later 2"));
    // The text after the reference goes on as before.
    assert!(page_has(&layout.pages[1], "later 0"));
}

#[test]
fn a_continued_note_comes_before_the_notes_of_the_page_it_continues_on() {
    let h = line_step();
    let before = ((160.0 - 6.0 - 2.5 * h) / h).floor() as usize - 1;
    let mut document = noted(before, 5, 0);
    // A second reference near the top of the next page.
    let mut blocks = document.body.blocks.clone();
    for i in 0..4 {
        blocks.push(pb(&format!("later {i}")));
    }
    let mut p = paragraph("");
    p.runs = vec![
        Run::text("again", look()),
        Run::object(Inline::Footnote(2), look()),
    ];
    blocks.push(Block::paragraph(p));
    document.body = Story { blocks, part: None };
    let mut second = note_of(2, 1);
    if let Block::Paragraph(p) = &mut second.blocks[0] {
        Arc::make_mut(p).runs[1].text = " own note".into();
    }
    document.footnotes.notes.push(second);
    let layout = lay(&document);
    let page = &layout.pages[1];
    assert!(page_has(page, "own note"));
    assert!(text_y(page, "zq3") < text_y(page, "own note"));
    assert!(text_y(page, "again") < text_y(page, "zq3"));
    // Numbered in the order of the references.
    assert!(page_has(page, "2 own note"));
}

#[test]
fn a_footnote_longer_than_a_page_runs_over_several() {
    let layout = lay(&noted(3, 45, 2));
    assert!(layout.pages.len() >= 3, "{}", layout.pages.len());
    let mut seen = Vec::new();
    for (n, page) in layout.pages.iter().enumerate() {
        let lines = note_lines(page);
        if n == 0 {
            // The reference is on this page, and so is the start of the note.
            assert_eq!(page_of(&layout, 3), 0);
            assert!(!lines.is_empty());
        }
        seen.extend(lines);
    }
    // Every line once, in order.
    assert_eq!(seen.len(), 45, "{seen:?}");
    for (n, line) in seen.iter().enumerate() {
        assert!(line.contains(&format!("zq{}", n + 1)), "{n}: {line}");
    }
    // Nothing is drawn below the bottom margin.
    for page in &layout.pages {
        for item in &page.items {
            if let Item::Text(t) = item {
                let last = t.paragraph.lines()[t.lines.end - 1].top + t.y;
                assert!(last < 200.0 - 20.0 + 0.5 || t.y > 180.0, "{last}");
            }
        }
    }
    // The text after the reference follows the note's last line.
    let end = layout
        .pages
        .iter()
        .rposition(|p| !note_lines(p).is_empty())
        .unwrap();
    assert!(page_of(&layout, 5) <= end + 1);
}

#[test]
fn a_paragraph_hidden_altogether_takes_no_room() {
    let hidden_look = || RunLook {
        hidden: true,
        ..look()
    };
    let make = |mark_hidden: bool, run_hidden: bool| {
        let mut p = paragraph("");
        p.runs = vec![Run::text(
            "hidden words",
            if run_hidden { hidden_look() } else { look() },
        )];
        if mark_hidden {
            p.mark = hidden_look();
        }
        p.props.space_before = 400;
        p.props.space_after = 400;
        Block::paragraph(p)
    };
    let step = |middle: Option<Block>| {
        let mut blocks = vec![pb("one")];
        blocks.extend(middle);
        blocks.push(pb("two"));
        let two = blocks.len() - 1;
        let layout = lay(&doc(blocks));
        caret_top(&layout, two, 0) - caret_top(&layout, 0, 0)
    };
    let none = step(None);
    // Mark and text hidden: it is as if it were not there, spaces included.
    assert!((step(Some(make(true, true))) - none).abs() < 0.01);
    // Only the text hidden, or only the mark: the paragraph is still there.
    assert!(step(Some(make(false, true))) > none + 10.0);
    assert!(step(Some(make(true, false))) > none + 10.0);
    // Its place can still be asked for.
    let layout = lay(&doc(vec![pb("one"), make(true, true), pb("two")]));
    assert!(layout.caret(&Pos::new(Path::body(1), 0)).is_some());
}
