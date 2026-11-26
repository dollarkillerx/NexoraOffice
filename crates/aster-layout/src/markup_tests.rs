//! Tracked changes and comments on the page: coloured text, change bars,
//! the marks of comments, and the text as it was with markup off.

use std::sync::Arc;

use aster_writer::*;

use crate::{Item, Layout, Layouter};

fn look() -> RunLook {
    RunLook {
        size: 10.0,
        ..RunLook::default()
    }
}

fn change(id: i64, author: &str) -> Change {
    Change {
        id,
        author: Arc::from(author),
        date: None,
        moved: false,
    }
}

fn inserted(text: &str, author: &str) -> Run {
    let mut run = Run::text(text, look());
    run.annotation = Some(Annotation::Inserted(Arc::new(change(1, author))));
    run
}

fn deleted(text: &str, author: &str) -> Run {
    let mut run = Run::object(Inline::Marker, look());
    run.annotation = Some(Annotation::Deleted(Arc::new(Deleted {
        change: change(2, author),
        runs: vec![Run::text(text, look())],
        base: look(),
    })));
    run
}

fn mark(annotation: Annotation) -> Run {
    let mut run = Run::object(Inline::Marker, look());
    run.annotation = Some(annotation);
    run
}

fn paragraph(runs: Vec<Run>) -> Paragraph {
    Paragraph {
        runs,
        base: look(),
        mark: look(),
        ..Paragraph::default()
    }
}

fn doc(paragraphs: Vec<Paragraph>) -> Document {
    Document {
        body: Story {
            blocks: paragraphs.into_iter().map(Block::paragraph).collect(),
            part: None,
        },
        last_section: Arc::new(Section {
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
        }),
        ..Document::default()
    }
}

fn sample() -> Document {
    doc(vec![paragraph(vec![
        Run::text("one ", look()),
        inserted("two ", "Ann"),
        deleted("gone ", "Bob"),
        Run::text("three", look()),
    ])])
}

fn lay(document: &Document, markup: bool) -> Layout {
    let mut layouter = Layouter::new();
    layouter.set_markup(markup);
    layouter.layout(document)
}

/// The text the first page's first text item shows.
fn shown(layout: &Layout) -> String {
    layout.pages[0]
        .items
        .iter()
        .find_map(|item| match item {
            Item::Text(text) => Some(text.paragraph.text().to_owned()),
            _ => None,
        })
        .unwrap_or_default()
}

fn rules(layout: &Layout) -> usize {
    layout.pages[0]
        .items
        .iter()
        .filter(|item| matches!(item, Item::Rule { .. }))
        .count()
}

#[test]
fn deleted_text_is_shown_in_the_text_with_markup_on_and_not_with_it_off() {
    let document = sample();
    assert_eq!(shown(&lay(&document, true)), "one two gone three");
    assert_eq!(shown(&lay(&document, false)), "one two three");
}

#[test]
fn positions_in_the_text_ignore_the_deleted_text() {
    let document = sample();
    let on = lay(&document, true);
    let off = lay(&document, false);
    let at = |offset| Pos::new(Path::body(0), offset);
    // Before the deleted text's place and after it: the same offsets mean the
    // same characters, shown or not.
    for offset in [0, 4, 8, 9, 13] {
        assert!(on.caret(&at(offset)).is_some());
        assert!(off.caret(&at(offset)).is_some());
    }
    let after = on.caret(&at(9)).unwrap();
    let before = on.caret(&at(8)).unwrap();
    // "t" of "three" is further right than the end of "two " by the width of
    // the deleted text.
    assert!(after.x > before.x + 10.0, "{after:?} {before:?}");
    // A click on "three" finds a place in it.
    let hit = on
        .hit(0, after.x + 2.0, after.top + after.height / 2.0)
        .unwrap();
    assert_eq!(hit.offset, 9);
}

#[test]
fn changes_have_the_colour_of_their_author() {
    let document = sample();
    let layout = lay(&document, true);
    assert_eq!(layout.authors.len(), 2);
    let ann = layout.author_color("Ann");
    let bob = layout.author_color("Bob");
    assert_ne!(ann, bob);
    let item = layout.pages[0]
        .items
        .iter()
        .find_map(|item| match item {
            Item::Text(text) => Some(text.paragraph.clone()),
            _ => None,
        })
        .unwrap();
    // The inserted text is underlined and the deleted struck through, each in
    // its author's colour; a strike-through is higher than an underline.
    let lines = item.decorations_in(0..item.lines().len(), 0.0);
    let underline = lines.iter().find(|d| d.color == ann).expect("an underline");
    let strike = lines
        .iter()
        .find(|d| d.color == bob)
        .expect("a strike-through");
    assert!(strike.y < underline.y, "{lines:?}");
}

#[test]
fn a_line_with_a_change_has_a_bar_in_the_margin() {
    let document = doc(vec![
        paragraph(vec![Run::text("plain", look())]),
        paragraph(vec![inserted("added", "Ann")]),
        paragraph(vec![Run::text("plain", look())]),
    ]);
    let on = lay(&document, true);
    let bars: Vec<_> = on.pages[0]
        .items
        .iter()
        .filter_map(|item| match item {
            Item::Rule {
                from, to, color, ..
            } => Some((*from, *to, *color)),
            _ => None,
        })
        .collect();
    assert_eq!(bars.len(), 1, "{bars:?}");
    let (from, to, color) = bars[0];
    // In the left margin (the page margin is 20 points), as tall as the line.
    assert!(from.0 < 20.0 && from.0 > 0.0);
    assert_eq!(from.0, to.0);
    assert!(to.1 > from.1);
    assert_eq!(color, on.author_color("Ann"));
    assert_eq!(rules(&lay(&document, false)), 0);
}

#[test]
fn a_deletion_and_a_paragraph_change_get_bars_too() {
    let mut marked = paragraph(vec![Run::text("mark", look())]);
    marked.changes.inserted_mark = Some(Arc::new(change(9, "Cy")));
    let mut formatted = paragraph(vec![Run::text("fmt", look())]);
    formatted.changes.formatting = true;
    let document = doc(vec![
        paragraph(vec![deleted("gone", "Bob")]),
        marked,
        formatted,
        paragraph(vec![Run::text("plain", look())]),
    ]);
    assert_eq!(rules(&lay(&document, true)), 3);
    assert_eq!(rules(&lay(&document, false)), 0);
}

#[test]
fn commented_text_has_a_light_background_and_a_mark_in_the_right_margin() {
    let mut document = doc(vec![paragraph(vec![
        Run::text("ab ", look()),
        mark(Annotation::CommentStart(5)),
        Run::text("cd", look()),
        mark(Annotation::CommentEnd(5)),
        mark(Annotation::CommentReference(5)),
        Run::text(" ef", look()),
    ])]);
    document.comments = vec![Arc::new(Comment {
        id: 5,
        author: "Dee".into(),
        initials: "D".into(),
        date: None,
        paragraphs: vec!["hi".into()],
    })];
    let on = lay(&document, true);
    let color = on.author_color("Dee");
    let fills: Vec<_> = on.pages[0]
        .items
        .iter()
        .filter_map(|item| match item {
            Item::Fill { rect, color } => Some((*rect, *color)),
            _ => None,
        })
        .collect();
    // The background, translucent, over "cd"; the mark beyond the right
    // margin (the page is 300 wide, the margin 20).
    let back = fills
        .iter()
        .find(|(r, c)| c[3] < 0xff && r.x1 < 280.0)
        .unwrap();
    assert_eq!(&back.1[..3], &color[..3]);
    let mark = fills.iter().find(|(r, _)| r.x0 > 280.0).unwrap();
    assert_eq!(mark.1, color);
    // With markup off there is nothing of either.
    let off = lay(&document, false);
    assert!(
        !off.pages[0]
            .items
            .iter()
            .any(|item| matches!(item, Item::Fill { .. }))
    );
}

#[test]
fn markup_off_lays_out_as_if_the_changes_were_accepted() {
    // A document whose deleted text would take a line or more.
    let long = "a long deleted sentence that goes on ".repeat(8);
    let with = doc(vec![paragraph(vec![
        Run::text("start ", look()),
        deleted(&long, "Bob"),
        Run::text("end", look()),
    ])]);
    let without = doc(vec![paragraph(vec![Run::text("start end", look())])]);
    let (a, b) = (lay(&with, false), lay(&without, false));
    let height = |layout: &Layout| {
        layout.pages[0]
            .items
            .iter()
            .find_map(|item| match item {
                Item::Text(t) => Some(t.paragraph.height()),
                _ => None,
            })
            .unwrap_or(0.0)
    };
    assert_eq!(height(&a), height(&b));
    assert!(height(&lay(&with, true)) > height(&a));
}

#[test]
fn the_cache_follows_the_switch() {
    let document = sample();
    let mut layouter = Layouter::new();
    let on = layouter.layout(&document);
    let off = {
        layouter.set_markup(false);
        layouter.layout(&document)
    };
    assert!(on.markup && !off.markup);
    let again = {
        layouter.set_markup(true);
        layouter.layout(&document)
    };
    assert_eq!(shown(&again), shown(&on));
    assert_ne!(shown(&off), shown(&on));
}
