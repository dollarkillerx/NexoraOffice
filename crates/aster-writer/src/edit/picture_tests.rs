//! Tests of how pictures sit and move.

use super::*;
use crate::edit::table_tests::{doc_of, pos};
use crate::{OBJECT, Path};

fn png() -> Arc<[u8]> {
    let mut bytes = b"\x89PNG\r\n\x1a\n".to_vec();
    bytes.extend([0; 20]);
    Arc::from(bytes)
}

fn with_picture() -> Document {
    let mut doc = doc_of(&["abcd", "second"]);
    insert_picture(&mut doc, &pos(0, 2), png(), (1_000_000, 500_000), None).unwrap();
    doc
}

fn drawing(doc: &Document, block: usize, offset: usize) -> Drawing {
    drawing_at(doc, &pos(block, offset))
        .expect("a drawing")
        .clone()
}

#[test]
fn a_picture_leaves_the_line_where_it_was() {
    let mut doc = with_picture();
    assert_eq!(picture_layout(&drawing(&doc, 0, 2)), PictureLayout::Inline);
    assert!(set_picture_layout(
        &mut doc,
        &pos(0, 2),
        PictureLayout::Square
    ));
    let d = drawing(&doc, 0, 2);
    assert_eq!(picture_layout(&d), PictureLayout::Square);
    let float = d.float.unwrap();
    // At the character, at the top of the line.
    assert_eq!(float.x.from, FloatFrom::Text);
    assert_eq!(float.y.from, FloatFrom::Text);
    assert_eq!(float.x.at, FloatAt::Offset(0));
    assert_eq!(float.y.at, FloatAt::Offset(0));
    assert_eq!(float.distance, [114_300, 0, 114_300, 0]);
    // The same place in the same paragraph; nothing else changed.
    assert_eq!(
        doc.paragraph(&Path::body(0)).unwrap().text(),
        format!("ab{OBJECT}cd")
    );
    assert_eq!(d.size, (1_000_000, 500_000));
}

#[test]
fn every_layout_can_be_reached_from_every_other() {
    let layouts = [
        PictureLayout::Inline,
        PictureLayout::Square,
        PictureLayout::Tight,
        PictureLayout::TopAndBottom,
        PictureLayout::BehindText,
        PictureLayout::InFrontOfText,
    ];
    for from in layouts {
        for to in layouts {
            let mut doc = with_picture();
            set_picture_layout(&mut doc, &pos(0, 2), from);
            let changed = set_picture_layout(&mut doc, &pos(0, 2), to);
            assert_eq!(changed, from != to, "{from:?} to {to:?}");
            assert_eq!(picture_layout(&drawing(&doc, 0, 2)), to);
        }
    }
}

#[test]
fn a_floating_picture_keeps_its_place_when_the_wrap_changes() {
    let mut doc = with_picture();
    set_picture_layout(&mut doc, &pos(0, 2), PictureLayout::Square);
    move_floating_drawing(&mut doc, &pos(0, 2), (500, 700), (0, 0));
    let before = drawing(&doc, 0, 2).float.unwrap();
    set_picture_layout(&mut doc, &pos(0, 2), PictureLayout::BehindText);
    let after = drawing(&doc, 0, 2).float.unwrap();
    assert_eq!((after.x, after.y), (before.x, before.y));
    assert!(after.behind);
    assert_eq!(after.wrap, Wrap::None);
    assert_eq!(after.distance, before.distance);
    set_picture_layout(&mut doc, &pos(0, 2), PictureLayout::InFrontOfText);
    assert!(!drawing(&doc, 0, 2).float.unwrap().behind);
    // Back in the line, the place is gone.
    set_picture_layout(&mut doc, &pos(0, 2), PictureLayout::Inline);
    assert_eq!(drawing(&doc, 0, 2).float, None);
}

#[test]
fn moving_a_floating_picture_turns_alignments_into_offsets() {
    let mut doc = with_picture();
    set_picture_layout(&mut doc, &pos(0, 2), PictureLayout::InFrontOfText);
    assert!(move_floating_drawing(
        &mut doc,
        &pos(0, 2),
        (100, -50),
        (0, 0)
    ));
    let float = drawing(&doc, 0, 2).float.unwrap();
    assert_eq!(
        (float.x.at, float.y.at),
        (FloatAt::Offset(100), FloatAt::Offset(-50))
    );
    assert!(move_floating_drawing(&mut doc, &pos(0, 2), (1, 1), (0, 0)));
    // Centred: half of what is left of the reference after the picture.
    with_drawing(&mut doc, &pos(0, 2), |d| {
        let f = d.float.as_mut().unwrap();
        f.x.at = FloatAt::Center;
        f.y.at = FloatAt::End;
        true
    });
    assert!(move_floating_drawing(
        &mut doc,
        &pos(0, 2),
        (10, 20),
        (5_000_000, 3_000_000)
    ));
    let float = drawing(&doc, 0, 2).float.unwrap();
    assert_eq!(
        float.x.at,
        FloatAt::Offset((5_000_000 - 1_000_000) / 2 + 10)
    );
    assert_eq!(float.y.at, FloatAt::Offset(3_000_000 - 500_000 + 20));
    // A left alignment is offset 0 plus the move.
    with_drawing(&mut doc, &pos(0, 2), |d| {
        d.float.as_mut().unwrap().x.at = FloatAt::Start;
        true
    });
    move_floating_drawing(&mut doc, &pos(0, 2), (7, 0), (0, 0));
    assert_eq!(drawing(&doc, 0, 2).float.unwrap().x.at, FloatAt::Offset(7));
}

#[test]
fn offsets_are_held_and_the_line_has_nothing_to_move() {
    let mut doc = with_picture();
    assert!(!move_floating_drawing(&mut doc, &pos(0, 2), (1, 1), (0, 0)));
    set_picture_layout(&mut doc, &pos(0, 2), PictureLayout::Square);
    move_floating_drawing(&mut doc, &pos(0, 2), (i64::MAX / 4, 0), (0, 0));
    assert_eq!(
        drawing(&doc, 0, 2).float.unwrap().x.at,
        FloatAt::Offset(200_000_000)
    );
    assert!(!move_floating_drawing(&mut doc, &pos(0, 0), (1, 1), (0, 0)));
}

#[test]
fn resizing_works_on_a_floating_picture_and_leaves_its_place() {
    let mut doc = with_picture();
    set_picture_layout(&mut doc, &pos(0, 2), PictureLayout::TopAndBottom);
    move_floating_drawing(&mut doc, &pos(0, 2), (30, 40), (0, 0));
    let float = drawing(&doc, 0, 2).float;
    resize_drawing(&mut doc, &pos(0, 2), (2_000_000, 900_000));
    let d = drawing(&doc, 0, 2);
    assert_eq!(d.size, (2_000_000, 900_000));
    assert_eq!(d.float, float);
}

#[test]
fn a_picture_moves_to_another_place_in_the_text() {
    let mut doc = with_picture();
    let width = OBJECT.len();
    // Later in the same paragraph.
    let now = move_drawing(&mut doc, &pos(0, 2), &pos(0, 2 + width + 1)).unwrap();
    assert_eq!(now, pos(0, 3));
    assert_eq!(
        doc.paragraph(&Path::body(0)).unwrap().text(),
        format!("abc{OBJECT}d")
    );
    // Earlier.
    let now = move_drawing(&mut doc, &pos(0, 3), &pos(0, 0)).unwrap();
    assert_eq!(now, pos(0, 0));
    assert_eq!(
        doc.paragraph(&Path::body(0)).unwrap().text(),
        format!("{OBJECT}abcd")
    );
    // Into another paragraph.
    let now = move_drawing(&mut doc, &pos(0, 0), &pos(1, 3)).unwrap();
    assert_eq!(now, pos(1, 3));
    assert_eq!(doc.paragraph(&Path::body(0)).unwrap().text(), "abcd");
    assert_eq!(
        doc.paragraph(&Path::body(1)).unwrap().text(),
        format!("sec{OBJECT}ond")
    );
    assert_eq!(drawing(&doc, 1, 3).size, (1_000_000, 500_000));
    // Onto itself, or where there is none: nothing.
    assert_eq!(
        move_drawing(&mut doc, &pos(1, 3), &pos(1, 3)),
        Some(pos(1, 3))
    );
    assert_eq!(
        move_drawing(&mut doc, &pos(1, 3), &pos(1, 3 + width)),
        Some(pos(1, 3))
    );
    assert_eq!(move_drawing(&mut doc, &pos(1, 0), &pos(0, 0)), None);
    assert_eq!(move_drawing(&mut doc, &pos(1, 3), &pos(9, 0)), None);
}

#[test]
fn a_floating_picture_moves_its_anchor_too() {
    let mut doc = with_picture();
    set_picture_layout(&mut doc, &pos(0, 2), PictureLayout::Square);
    let float = drawing(&doc, 0, 2).float;
    move_drawing(&mut doc, &pos(0, 2), &pos(1, 6)).unwrap();
    assert_eq!(drawing(&doc, 1, 6).float, float);
}

#[test]
fn a_picture_dropped_inside_text_splits_the_run_and_the_text_joins_again() {
    let mut doc = with_picture();
    move_drawing(&mut doc, &pos(0, 2), &pos(1, 2)).unwrap();
    move_drawing(&mut doc, &pos(1, 2), &pos(0, 1)).unwrap();
    let p = doc.paragraph(&Path::body(1)).unwrap();
    // "second" is one run again.
    assert_eq!(p.runs.len(), 1);
    assert_eq!(p.text(), "second");
}
