//! Pointing at text in headers, footers and notes.

use std::sync::Arc;

use aster_writer::*;

use crate::{Layout, Layouter};

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

fn numbered(n: usize) -> Vec<Block> {
    (0..n).map(|i| pb(&format!("line {i}"))).collect()
}

fn doc(blocks: Vec<Block>) -> Document {
    Document {
        body: Story { blocks, part: None },
        last_section: Arc::new(small()),
        ..Document::default()
    }
}

fn story(blocks: Vec<Block>) -> Arc<Story> {
    Arc::new(Story { blocks, part: None })
}

fn lay(document: &Document) -> Layout {
    Layouter::new().layout(document)
}

fn header_pos(block: usize, offset: usize) -> Pos {
    Pos::new(Path::in_story(StoryId::Marginal(0), block), offset)
}

/// A document of several pages with a two-paragraph header and a footer.
fn with_header_and_footer() -> Document {
    let mut document = doc(numbered(40));
    document.marginals = vec![
        story(vec![pb("Head one"), pb("Head two")]),
        story(vec![pb("Foot")]),
    ];
    let mut section = small();
    section.headers.default = Some(0);
    section.footers.default = Some(1);
    document.last_section = Arc::new(section);
    document
}

#[test]
fn a_header_can_be_pointed_at_on_every_page() {
    let document = with_header_and_footer();
    let layout = lay(&document);
    assert!(layout.pages.len() >= 3);
    let header = StoryId::Marginal(0);
    let mut carets = Vec::new();
    for page in 0..layout.pages.len() {
        let caret = layout.caret_on(Some(page), &header_pos(0, 3)).unwrap();
        assert_eq!(caret.page, page);
        assert!(caret.top < 30.0, "{caret:?}");
        carets.push(caret);
        // A click there finds the place again, on that page.
        let hit = layout
            .hit_in(page, header, caret.x + 0.2, caret.top + caret.height / 2.0)
            .unwrap();
        assert_eq!(hit, header_pos(0, 3));
        // The second paragraph is below the first.
        let second = layout.caret_on(Some(page), &header_pos(1, 0)).unwrap();
        assert!(second.top > caret.top);
        assert_eq!(second.page, page);
    }
    assert!(
        carets
            .windows(2)
            .all(|w| (w[0].top - w[1].top).abs() < 0.01)
    );
    // Without a page: the first one shows it.
    assert_eq!(layout.caret(&header_pos(0, 3)).unwrap().page, 0);
    assert_eq!(layout.pages_of(&header_pos(0, 0)).len(), layout.pages.len());
}

#[test]
fn clicking_the_header_area_finds_the_body_with_hit_but_the_header_with_hit_in() {
    let document = with_header_and_footer();
    let layout = lay(&document);
    let header = layout.caret_on(Some(1), &header_pos(0, 2)).unwrap();
    let (x, y) = (header.x, header.top + 1.0);
    // hit() is the body's: the first line of the page.
    let body = layout.hit(1, x, y).unwrap();
    assert!(body.path.story.is_body());
    let there = layout.hit_in(1, StoryId::Marginal(0), x, y).unwrap();
    assert_eq!(there.path.story, StoryId::Marginal(0));
    // Nothing to find in a story that is not on the page.
    assert!(layout.hit_in(1, StoryId::Marginal(5), x, y).is_none());
    assert!(layout.hit_in(1, StoryId::Footnote(1), x, y).is_none());
}

#[test]
fn story_at_tells_the_header_the_footer_the_note_and_the_body() {
    let mut document = with_header_and_footer();
    let mut reference = paragraph("see");
    reference
        .runs
        .push(Run::object(Inline::Footnote(1), look()));
    document.body.blocks.insert(0, Block::paragraph(reference));
    document.footnotes = Notes {
        notes: vec![Note {
            id: 1,
            blocks: vec![pb("the note")],
        }],
        part: None,
    };
    let layout = lay(&document);
    let info = layout.page_info(0).unwrap();
    assert_eq!(info.header, Some(0));
    assert_eq!(info.footer, Some(1));
    assert_eq!(info.slot, MarginalSlot::Default);
    assert_eq!(layout.story_at(0, 50.0, 5.0), Some(StoryId::Marginal(0)));
    assert_eq!(layout.story_at(0, 50.0, 195.0), Some(StoryId::Marginal(1)));
    let note = layout
        .caret(&Pos::new(Path::in_story(StoryId::Footnote(1), 0), 2))
        .unwrap();
    assert_eq!(note.page, 0);
    assert_eq!(
        layout.story_at(0, note.x + 1.0, note.top + 2.0),
        Some(StoryId::Footnote(1))
    );
    assert_eq!(layout.story_at(0, 50.0, 60.0), Some(StoryId::Body));
    assert!(info.header_area.contains(10.0, 5.0));
    assert!(!info.header_area.contains(10.0, 100.0));
    assert!(info.footer_area.contains(10.0, 195.0));
}

#[test]
fn a_page_without_a_header_has_the_strip_but_no_story() {
    let layout = lay(&doc(numbered(3)));
    let info = layout.page_info(0).unwrap();
    assert_eq!(info.header, None);
    assert!(info.header_area.height() > 0.0);
    assert_eq!(layout.story_at(0, 50.0, 5.0), None);
    assert_eq!(layout.story_at(0, 50.0, 195.0), None);
    assert_eq!(layout.story_at(0, 50.0, 60.0), Some(StoryId::Body));
}

#[test]
fn the_first_and_even_slots_are_told_apart() {
    let mut document = doc(numbered(30));
    document.marginals = vec![story(vec![pb("a")]), story(vec![pb("b")])];
    let mut section = small();
    section.title_page = true;
    section.headers.first = Some(0);
    section.headers.default = Some(1);
    document.last_section = Arc::new(section);
    let layout = lay(&document);
    assert_eq!(layout.page_info(0).unwrap().slot, MarginalSlot::First);
    assert_eq!(layout.page_info(0).unwrap().header, Some(0));
    assert_eq!(layout.page_info(1).unwrap().slot, MarginalSlot::Default);
    assert_eq!(layout.page_info(1).unwrap().header, Some(1));
    document.settings.even_and_odd_headers = true;
    let layout = lay(&document);
    assert_eq!(layout.page_info(1).unwrap().slot, MarginalSlot::Even);
    assert_eq!(layout.page_info(2).unwrap().slot, MarginalSlot::Default);
}

#[test]
fn lines_arrows_and_selections_stay_in_the_header() {
    let long = "word ".repeat(30);
    let mut document = doc(numbered(6));
    document.marginals = vec![story(vec![pb(&long), pb("tail")])];
    let mut section = small();
    section.headers.default = Some(0);
    document.last_section = Arc::new(section);
    let layout = lay(&document);
    let page = Some(0);
    let first = layout.line_start_on(page, &header_pos(0, 10)).unwrap();
    assert_eq!(first, header_pos(0, 0));
    let end = layout.line_end_on(page, &header_pos(0, 10)).unwrap();
    assert!(end.offset > 10 && end.offset < long.len());
    let caret = layout.caret_on(page, &header_pos(0, 10)).unwrap();
    let below = layout.below_on(page, &header_pos(0, 10), caret.x).unwrap();
    assert_eq!(below.path, header_pos(0, 0).path);
    assert!(below.offset > end.offset - 1);
    // Down from the last line of the first paragraph goes to the second.
    let last = layout
        .line_start_on(page, &header_pos(0, long.len()))
        .unwrap();
    let next = layout.below_on(page, &last, 0.0).unwrap();
    assert_eq!(next.path, header_pos(1, 0).path);
    // Up from the first line, and down from the last, go nowhere: the
    // body is another story.
    assert!(layout.above_on(page, &header_pos(0, 0), 5.0).is_none());
    assert!(layout.below_on(page, &header_pos(1, 2), 5.0).is_none());
    // A selection across the two paragraphs is rectangles in the header.
    let rects = layout.selection_on(page, &header_pos(0, 3), &header_pos(1, 2));
    assert!(rects.len() >= 3);
    assert!(rects.iter().all(|(p, r)| *p == 0 && r.y1 < 60.0));
    // None between two stories.
    let body = Pos::new(Path::body(0), 1);
    assert!(layout.selection(&header_pos(0, 3), &body).is_empty());
}

#[test]
fn a_footnote_can_be_pointed_at_and_continues_over_pages() {
    let mut blocks = numbered(2);
    let mut p = paragraph("ref");
    p.runs.push(Run::object(Inline::Footnote(1), look()));
    blocks.push(Block::paragraph(p));
    let mut document = doc(blocks);
    let long = (0..160)
        .map(|i| format!("sentence {i}"))
        .collect::<Vec<_>>()
        .join(" ");
    document.footnotes = Notes {
        notes: vec![Note {
            id: 1,
            blocks: vec![pb(&long)],
        }],
        part: None,
    };
    let layout = lay(&document);
    assert!(layout.pages.len() >= 2, "{}", layout.pages.len());
    let note = |offset| Pos::new(Path::in_story(StoryId::Footnote(1), 0), offset);
    let start = layout.caret(&note(1)).unwrap();
    let end = layout.caret(&note(long.len())).unwrap();
    assert!(end.page > start.page, "{start:?} {end:?}");
    // Each end is found by a click on its page, in the note's story.
    let hit = layout
        .hit_in(
            end.page,
            StoryId::Footnote(1),
            end.x - 0.5,
            end.top + end.height / 2.0,
        )
        .unwrap();
    assert_eq!(hit.path, note(0).path);
    assert!(hit.offset > long.len() - 12, "{}", hit.offset);
    // The selection has rectangles on both pages.
    let pages: Vec<usize> = layout
        .selection(&note(1), &note(long.len()))
        .iter()
        .map(|(p, _)| *p)
        .collect();
    assert!(pages.contains(&start.page) && pages.contains(&end.page));
    // Arrow keys go on over the page break.
    let mut at = note(1);
    let mut seen = vec![start.page];
    for _ in 0..40 {
        let Some(next) = layout.below(&at, 0.0) else {
            break;
        };
        seen.push(layout.page_of(&next).unwrap());
        at = next;
    }
    assert!(seen.contains(&end.page), "{seen:?}");
}

#[test]
fn a_table_in_a_header_has_paths_with_its_story() {
    let cell = |text: &str| Cell {
        blocks: vec![pb(text)],
        ..Cell::default()
    };
    let table = Table {
        columns: vec![2_000, 2_000],
        rows: vec![Row {
            cells: vec![cell("left"), cell("right")],
            ..Row::default()
        }],
        ..Table::default()
    };
    let mut document = doc(numbered(3));
    document.marginals = vec![story(vec![Block::table(table), pb("after")])];
    let mut section = small();
    section.headers.default = Some(0);
    document.last_section = Arc::new(section);
    let layout = lay(&document);
    let right = Pos::new(
        Path::in_story(StoryId::Marginal(0), 0).cell_block(0, 1, 0),
        2,
    );
    let caret = layout.caret_on(Some(0), &right).unwrap();
    let hit = layout
        .hit_in(0, StoryId::Marginal(0), caret.x + 0.2, caret.top + 2.0)
        .unwrap();
    assert_eq!(hit.path, right.path);
    // Stepping down leaves the table for the paragraph after it.
    let below = layout.below_on(Some(0), &right, caret.x).unwrap();
    assert_eq!(below.path, Path::in_story(StoryId::Marginal(0), 1));
}

#[test]
fn drawing_and_page_counts_are_what_they_were_before_stories_could_be_pointed_at() {
    // The recorded fragments of headers and notes add nothing to the pages.
    let document = with_header_and_footer();
    let layout = lay(&document);
    let again = lay(&document);
    assert_eq!(layout.pages.len(), again.pages.len());
    assert_eq!(layout.pages[1].items.len(), again.pages[1].items.len());
    // The body's queries are unchanged: a hit anywhere on a body line is
    // a body place.
    let caret = layout.caret(&Pos::new(Path::body(5), 2)).unwrap();
    let hit = layout
        .hit(caret.page, caret.x + 0.1, caret.top + 2.0)
        .unwrap();
    assert_eq!(hit.path, Path::body(5));
}
