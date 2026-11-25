//! Text on a PDF view: layers, selection, search highlights.

use aster_surface::pdf_view::{FindMark, PdfStyle, PdfView, SELECTION_MAX, TextLayer, Zoom};
use vello_cpu::Pixmap;

const TEXT: &str = "Hello world\nsecond line";

/// "Hello world\nsecond line": 10 points per char, line 1 at y 10..30, line 2
/// at y 40..60, the newline without a box.
fn layer() -> TextLayer {
    let mut boxes = Vec::new();
    for (i, c) in TEXT.chars().enumerate() {
        if c == '\n' {
            boxes.push(None);
        } else if i < 11 {
            let x = 10.0 + i as f32 * 10.0;
            boxes.push(Some([x, 10.0, x + 10.0, 30.0]));
        } else {
            let x = 10.0 + (i - 12) as f32 * 10.0;
            boxes.push(Some([x, 40.0, x + 10.0, 60.0]));
        }
    }
    TextLayer {
        text: TEXT.to_string(),
        boxes,
    }
}

/// Two small pages, both on screen at 100 %: page 0 at x 398.., y 24..324,
/// page 1 at y 340..640.
fn two_pages() -> PdfView {
    let mut view = PdfView::new(vec![(400.0, 300.0); 2]);
    view.resize(1000, 800, 1.0);
    view.set_zoom(Zoom::Percent(100.0));
    assert!(view.set_text(0, layer()));
    assert!(view.set_text(1, layer()));
    view
}

/// Screen position of a point of page `page` of `two_pages`.
fn at(page: usize, x: f64, y: f64) -> (f64, f64) {
    (398.0 + x, 24.0 + 316.0 * page as f64 + y)
}

fn char_at(page: usize, i: usize) -> (f64, f64) {
    let (x, y) = if i < 11 {
        (10.0 + i as f64 * 10.0 + 5.0, 20.0)
    } else {
        (10.0 + (i - 12) as f64 * 10.0 + 5.0, 50.0)
    };
    at(page, x, y)
}

fn px(p: &Pixmap, x: f64, y: f64) -> [u8; 3] {
    let c = p.sample(x as u16, y as u16);
    [c.r, c.g, c.b]
}

#[test]
fn set_text_validates_and_counts() {
    let mut view = PdfView::new(vec![(400.0, 300.0); 3]);
    view.resize(1000, 800, 1.0);
    assert!(!view.set_text(3, layer()));
    let mut bad = layer();
    bad.boxes.pop();
    assert!(!view.set_text(0, bad));
    assert_eq!(view.text_state(0), None);
    assert!(view.set_text(0, layer()));
    assert_eq!(view.text_state(0), Some(true));
    // A scanned page: a layer without characters.
    assert!(view.set_text(1, TextLayer::default()));
    assert_eq!(view.text_state(1), Some(false));
    // Multi-byte characters count as one each.
    let ok = TextLayer {
        text: "é日".into(),
        boxes: vec![Some([0.0, 0.0, 1.0, 1.0]); 2],
    };
    assert!(view.set_text(2, ok));
    assert_eq!(view.text_state(2), Some(true));
}

#[test]
fn layers_obey_the_budget_least_recently_used_first() {
    let mut view = PdfView::new(vec![(400.0, 300.0); 30]);
    view.resize(1000, 800, 1.0);
    view.set_zoom(Zoom::Percent(100.0));
    let big = |n: usize| TextLayer {
        text: "x".repeat(n),
        boxes: vec![Some([0.0, 0.0, 1.0, 1.0]); n],
    };
    // 100 chars: 100 * (4 + 20) bytes.
    view.set_text_budget(5_000);
    assert!(view.set_text(0, big(100)));
    for page in [20, 21, 22] {
        assert!(view.set_text(page, big(100)));
    }
    // The page on screen stays; the older far pages went.
    assert_eq!(view.text_state(0), Some(true));
    assert_eq!(view.text_state(20), None);
    assert_eq!(view.text_state(21), None);
    assert_eq!(view.text_state(22), Some(true));
    assert!(view.text_bytes() <= 5_000);
    // One layer over the whole budget is refused, and not asked for again.
    assert!(!view.set_text(1, big(1_000)));
    assert!(!view.wanted_text().contains(&1));
    view.drop_pixels();
    assert_eq!(view.text_state(0), None);
    assert_eq!(view.text_bytes(), 0);
}

#[test]
fn wanted_text_is_nearest_first_and_skips_layers() {
    let mut view = PdfView::new(vec![(400.0, 300.0); 10]);
    view.resize(1000, 800, 1.0);
    view.set_zoom(Zoom::Percent(100.0));
    assert_eq!(view.wanted_text(), vec![0, 1, 2, 3]);
    view.set_text(1, layer());
    assert_eq!(view.wanted_text(), vec![0, 2, 3]);
    view.go_to(6);
    let wanted = view.wanted_text();
    assert_eq!(wanted[0], view.current());
    assert!(wanted.iter().all(|p| (4..=9).contains(p)), "{wanted:?}");
    assert!(!wanted.contains(&1));
}

#[test]
fn click_word_and_line_selection() {
    let mut view = two_pages();
    // One click selects nothing yet.
    let (x, y) = char_at(0, 7);
    assert!(!view.press_text(x, y, 1));
    assert!(!view.has_selection());
    // Double click: the word "world".
    assert!(view.press_text(x, y, 2));
    assert_eq!(view.selected_text(), "world");
    // Triple click: the line.
    assert!(view.press_text(x, y, 3));
    assert_eq!(view.selected_text(), "Hello world");
    let (x, y) = char_at(0, 14);
    assert!(view.press_text(x, y, 3));
    assert_eq!(view.selected_text(), "second line");
    // Drag within one line by characters.
    let (x, y) = char_at(0, 0);
    view.press_text(x, y, 1);
    let (x, y) = char_at(0, 4);
    assert!(view.drag_text(x, y));
    assert_eq!(view.selected_text(), "Hello");
    // Dragging back onto the first character leaves nothing selected.
    let (x, y) = char_at(0, 0);
    assert!(view.drag_text(x, y));
    assert!(!view.has_selection());
    // Word drag extends by words.
    let (x, y) = char_at(0, 1);
    view.press_text(x, y, 2);
    let (x, y) = char_at(0, 7);
    assert!(view.drag_text(x, y));
    assert_eq!(view.selected_text(), "Hello world");
    view.clear_selection();
    assert!(!view.has_selection());
    assert_eq!(view.selected_text(), "");
}

#[test]
fn dragging_across_pages_runs_in_document_order() {
    let mut view = two_pages();
    let (x, y) = char_at(0, 6);
    view.press_text(x, y, 1);
    let (x, y) = char_at(1, 4);
    assert!(view.drag_text(x, y));
    assert_eq!(view.selected_text(), "world\nsecond line\nHello");
    // Backwards gives the same text.
    let (x, y) = char_at(1, 4);
    view.press_text(x, y, 1);
    let (x, y) = char_at(0, 6);
    assert!(view.drag_text(x, y));
    assert_eq!(view.selected_text(), "world\nsecond line\nHello");
    // A click on a page without characters under it clears the selection.
    let mut other = PdfView::new(vec![(400.0, 300.0)]);
    other.resize(1000, 800, 1.0);
    other.set_zoom(Zoom::Percent(100.0));
    other.set_text(0, TextLayer::default());
    assert!(!other.press_text(500.0, 100.0, 1));
}

#[test]
fn select_all_takes_the_current_page() {
    let mut view = two_pages();
    assert!(view.select_all());
    assert_eq!(view.selected_text(), TEXT);
    assert!(!view.select_all());
    // A page without characters has nothing to select.
    let mut empty = PdfView::new(vec![(400.0, 300.0)]);
    empty.resize(1000, 800, 1.0);
    assert!(!empty.select_all());
    empty.set_text(0, TextLayer::default());
    assert!(!empty.select_all());
}

#[test]
fn selected_text_drops_control_characters_and_is_capped() {
    let mut view = PdfView::new(vec![(400.0, 300.0)]);
    view.resize(1000, 800, 1.0);
    let text = "a\u{1}b\rc\td\ne\u{7f}";
    let n = text.chars().count();
    view.set_text(
        0,
        TextLayer {
            text: text.into(),
            boxes: vec![Some([0.0, 0.0, 1.0, 1.0]); n],
        },
    );
    assert!(view.select_all());
    assert_eq!(view.selected_text(), "abcd\ne");

    let n = SELECTION_MAX + 5_000;
    // Two-byte characters, so the cap falls on a character boundary.
    view.set_text(
        0,
        TextLayer {
            text: "é".repeat(n),
            boxes: vec![Some([0.0, 0.0, 1.0, 1.0]); n],
        },
    );
    assert!(view.select_all());
    let got = view.selected_text();
    assert!(got.len() <= SELECTION_MAX && got.len() >= SELECTION_MAX - 1);
    assert!(got.chars().all(|c| c == 'é'));
}

#[test]
fn hit_testing_picks_the_nearest_character() {
    let mut view = two_pages();
    // Far right of the first line: its last character, so a double click
    // takes the last word.
    let (x, y) = at(0, 350.0, 20.0);
    assert!(view.press_text(x, y, 2));
    assert_eq!(view.selected_text(), "world");
    // Left of the second line (inside the page, before its first box).
    let (x, y) = at(0, 2.0, 50.0);
    assert!(view.press_text(x, y, 2));
    assert_eq!(view.selected_text(), "second");
    // Between the lines, nearer the first.
    let (x, y) = at(0, 25.0, 33.0);
    assert!(view.press_text(x, y, 2));
    assert_eq!(view.selected_text(), "Hello");
    // Below everything: the second line is the nearest.
    let (x, y) = at(0, 25.0, 200.0);
    assert!(view.press_text(x, y, 3));
    assert_eq!(view.selected_text(), "second line");
    // On a character, the cursor is a text cursor; beside it, not.
    let (x, y) = char_at(0, 2);
    assert!(view.is_over_text(x, y));
    let (x, y) = at(0, 350.0, 20.0);
    assert!(!view.is_over_text(x, y));
}

#[test]
fn the_strip_is_not_text() {
    let mut view = two_pages();
    let (x, y) = char_at(0, 2);
    assert!(view.press_text(x, y, 2));
    assert!(view.has_selection());
    // x = 100 is on the thumbnail strip.
    assert!(!view.press_text(100.0, 50.0, 2));
    assert!(!view.is_over_text(100.0, 50.0));
    assert!(!view.drag_text(100.0, 50.0));
    assert!(view.has_selection());
    // A page without a layer gives nothing.
    let mut view = PdfView::new(vec![(400.0, 300.0); 2]);
    view.resize(1000, 800, 1.0);
    view.set_zoom(Zoom::Percent(100.0));
    assert!(!view.press_text(500.0, 100.0, 2));
    assert!(!view.is_over_text(500.0, 100.0));
}

#[test]
fn hits_and_selection_are_drawn_and_trigger_frames() {
    let mut view = two_pages();
    let white = [255, 255, 255];
    let _ = view.render();
    assert!(!view.needs_render());
    let first = at(0, 30.0, 20.0);
    let second = at(0, 30.0, 50.0);
    assert_eq!(px(view.render(), first.0, first.1), white);

    view.set_find_hits(vec![
        FindMark {
            page: 0,
            boxes: vec![[10.0, 10.0, 50.0, 30.0]],
        },
        FindMark {
            page: 0,
            boxes: vec![[10.0, 40.0, 50.0, 60.0]],
        },
    ]);
    assert!(view.needs_render());
    let p = view.render();
    let (a, b) = (px(p, first.0, first.1), px(p, second.0, second.1));
    assert_ne!(a, white);
    assert_eq!(a, b);
    // Light yellow: red and green high, blue lower.
    assert!(a[0] > 240 && a[1] > 220 && a[2] < 230, "{a:?}");

    view.set_current_hit(Some(1));
    assert!(view.needs_render());
    let p = view.render();
    assert_eq!(px(p, first.0, first.1), a);
    let c = px(p, second.0, second.1);
    assert_ne!(c, a);
    assert!(c[2] < a[2], "orange has less blue: {c:?} vs {a:?}");

    // Selection: a tint over the second line, in one piece.
    let (x, y) = char_at(0, 13);
    view.press_text(x, y, 3);
    assert!(view.needs_render());
    let p = view.render();
    let inside = at(0, 100.0, 50.0);
    let s = px(p, inside.0, inside.1);
    assert_ne!(s, white);
    let (g1, g2) = (at(0, 50.0, 55.0), at(0, 105.0, 55.0));
    assert_eq!(px(p, g1.0, g1.1), px(p, g2.0, g2.1));
    // Outside the selected characters the page is still white.
    let outside = at(0, 150.0, 50.0);
    assert_eq!(px(p, outside.0, outside.1), white);

    view.clear_find();
    assert!(view.needs_render());
    let p = view.render();
    assert_eq!(px(p, first.0, first.1), white);
    view.clear_selection();
    let p = view.render();
    assert_eq!(px(p, inside.0, inside.1), white);

    // Colours come from the style.
    let style = PdfStyle {
        hit: [0, 0, 255, 255],
        ..PdfStyle::default()
    };
    view.set_style(style);
    view.set_find_hits(vec![FindMark {
        page: 0,
        boxes: vec![[10.0, 10.0, 50.0, 30.0]],
    }]);
    assert_eq!(px(view.render(), first.0, first.1), [0, 0, 255]);
}

#[test]
fn hits_are_bounded() {
    let mut view = two_pages();
    let many: Vec<FindMark> = (0..12_000)
        .map(|_| FindMark {
            page: 0,
            boxes: vec![[0.0, 0.0, 1.0, 1.0]; 300],
        })
        .collect();
    view.set_find_hits(many);
    assert_eq!(view.hit_count(), 10_000);
    view.add_find_hits(vec![FindMark::default()]);
    assert_eq!(view.hit_count(), 10_000);
    view.set_current_hit(Some(20_000));
    assert_eq!(view.current_hit(), None);
    view.set_current_hit(Some(5));
    assert_eq!(view.current_hit(), Some(5));
    // Rendering with that many is fine.
    let _ = view.render();
}

/// Pixels on screen (right of the strip) that look like the default hit.
fn hit_pixels(p: &Pixmap) -> usize {
    let mut found = 0;
    for y in 0..p.height() {
        for x in 200..p.width() {
            let c = p.sample(x, y);
            if c.b < 200 && c.r > 240 && c.g > 220 {
                found += 1;
            }
        }
    }
    found
}

#[test]
fn go_to_hit_scrolls_to_a_far_page() {
    let mut view = PdfView::new(vec![(400.0, 300.0); 40]);
    view.resize(1000, 800, 1.0);
    view.set_zoom(Zoom::Percent(100.0));
    view.set_find_hits(vec![
        FindMark {
            page: 25,
            boxes: vec![[10.0, 100.0, 60.0, 120.0]],
        },
        FindMark {
            page: 99,
            boxes: vec![[10.0, 10.0, 60.0, 20.0]],
        },
        FindMark {
            page: 3,
            boxes: Vec::new(),
        },
    ]);
    assert_eq!(hit_pixels(view.render()), 0);
    assert!(view.go_to_hit(0));
    assert_eq!(view.current(), 25);
    // The hit shows: its 50 x 20 points are on screen.
    assert!(hit_pixels(view.render()) >= 900);
    // Already visible: nothing to do. No such hit, no boxes, or a page that
    // is not there: false.
    assert!(!view.go_to_hit(0));
    assert!(!view.go_to_hit(1));
    assert!(!view.go_to_hit(2));
    assert!(!view.go_to_hit(7));
}

#[test]
fn go_to_hit_scrolls_sideways_when_zoomed() {
    let mut view = PdfView::new(vec![(400.0, 300.0); 2]);
    view.resize(1000, 800, 1.0);
    view.set_zoom(Zoom::Percent(400.0));
    // A box at the far right of the page (1600 px wide at 400 %).
    view.set_find_hits(vec![FindMark {
        page: 0,
        boxes: vec![[380.0, 100.0, 395.0, 120.0]],
    }]);
    assert_eq!(hit_pixels(view.render()), 0);
    assert!(view.go_to_hit(0));
    assert!(hit_pixels(view.render()) > 1000);
}

#[test]
fn clamped_pages_map_boxes_through_the_factor() {
    // 28 800 points is twice the largest size, so layout is at half size and
    // the real page is twice as large as the one laid out.
    let mut view = PdfView::new(vec![(28_800.0, 28_800.0)]);
    view.resize(1000, 800, 1.0);
    view.set_zoom(Zoom::Percent(100.0));
    // The laid-out page is 14 400 px at 100 %: x from 220, y from 24; the
    // real point (1000, 500) is at (220 + 500, 24 + 250).
    let layer = TextLayer {
        text: "ab".into(),
        boxes: vec![
            Some([990.0, 490.0, 1010.0, 510.0]),
            Some([1010.0, 490.0, 1030.0, 510.0]),
        ],
    };
    assert!(view.set_text(0, layer));
    assert!(view.press_text(720.0, 274.0, 2));
    assert_eq!(view.selected_text(), "ab");
    assert!(view.is_over_text(720.0, 274.0));
    assert!(!view.is_over_text(720.0, 400.0));
    // Single click on the second box, drag back to the first.
    view.press_text(731.0, 274.0, 1);
    assert!(view.drag_text(715.0, 274.0));
    assert_eq!(view.selected_text(), "ab");
    // The highlight lands there too.
    view.clear_selection();
    view.set_find_hits(vec![FindMark {
        page: 0,
        boxes: vec![[990.0, 490.0, 1010.0, 510.0]],
    }]);
    let p = view.render();
    assert_ne!(px(p, 720.0, 274.0), [255, 255, 255]);
    assert_eq!(px(p, 720.0, 300.0), [255, 255, 255]);
    assert_eq!(px(p, 700.0, 274.0), [255, 255, 255]);
}

/// Glyphs that alternate between the top and the bottom half of the page, so
/// no two neighbours merge.
fn halves(n: usize) -> TextLayer {
    let boxes = (0..n)
        .map(|i| {
            Some(if i % 2 == 0 {
                [0.0, 0.0, 400.0, 140.0]
            } else {
                [0.0, 160.0, 400.0, 300.0]
            })
        })
        .collect();
    TextLayer {
        text: "a".repeat(n),
        boxes,
    }
}

#[test]
fn marks_cost_the_same_however_many_boxes_make_them() {
    let mut view = PdfView::new(vec![(400.0, 300.0); 2]);
    view.resize(2880, 1800, 2.0);
    view.set_zoom(Zoom::Percent(100.0));
    assert!(view.set_text(0, halves(100_000)));
    let _ = view.render();
    let white = [255, 255, 255];
    // A middle of the page's top half, under the page's top-left corner at
    // about (x0, 48) in a 2880-wide window.
    let probe = |view: &mut PdfView| {
        let t = std::time::Instant::now();
        let p = view.render().clone();
        (t.elapsed(), p)
    };
    assert!(view.select_all());
    let (_, p) = probe(&mut view);
    let (x, y) = (1440.0, 48.0 + 2.0 * 70.0);
    assert_ne!(px(&p, x, y), white, "the selection is drawn");
    // The gap between the halves is not covered.
    assert_eq!(px(&p, x, 48.0 + 2.0 * 150.0), white);
    // Frames after the first reuse the picture.
    let mut slowest = std::time::Duration::ZERO;
    for _ in 0..5 {
        view.scroll_by(1.0);
        slowest = slowest.max(probe(&mut view).0);
    }
    view.clear_selection();
    let marks: Vec<FindMark> = (0..10_000)
        .map(|i| FindMark {
            page: 0,
            boxes: vec![if i % 2 == 0 {
                [0.0, 0.0, 400.0, 140.0]
            } else {
                [0.0, 160.0, 400.0, 300.0]
            }],
        })
        .collect();
    view.set_find_hits(marks);
    view.set_current_hit(Some(0));
    for _ in 0..5 {
        view.scroll_by(1.0);
        slowest = slowest.max(probe(&mut view).0);
    }
    // Unoptimised test builds are slow; the old cost was whole seconds per
    // frame, so this still tells the two apart.
    assert!(
        slowest < std::time::Duration::from_millis(500),
        "frames took {slowest:?}"
    );
}

#[test]
fn marks_of_a_huge_page_stay_bounded() {
    // 14 400 x 14 400 points at 800 %: far over the mask's cell cap.
    let mut view = PdfView::new(vec![(14_400.0, 14_400.0)]);
    view.resize(800, 600, 1.0);
    view.set_zoom(Zoom::Percent(800.0));
    let boxes = vec![Some([0.0, 0.0, 14_400.0, 14_400.0]); 1000];
    assert!(view.set_text(
        0,
        TextLayer {
            text: "a".repeat(1000),
            boxes
        }
    ));
    assert!(view.select_all());
    let _ = view.render();
    let p = view.render();
    assert_ne!(px(p, 400.0, 300.0), [255, 255, 255]);
}
