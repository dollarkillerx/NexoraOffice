//! Conditional formats on screen: fills, fonts, bars, icons, and what they
//! do when cells change.

use std::sync::Arc;

use aster_sheets::cond::{
    Cfvo, CfvoKind, Compare, CondFormat, Dxf, ExprEngine, IconSet, Kind, Rule,
};
use aster_sheets::structure::Lines;
use aster_sheets::{Cell, CellRange, CellRef, CellStyle, SheetBuilder, SheetKind, Value, Workbook};

use super::*;

const RED: Rgba = [0xff, 0x00, 0x00, 0xff];
const GREEN: Rgba = [0x00, 0xb0, 0x50, 0xff];
const WHITE: [u8; 4] = [0xff; 4];

fn dxf(fill: Rgba) -> Option<Arc<Dxf>> {
    Some(Arc::new(Dxf {
        fill: Some(fill),
        ..Dxf::default()
    }))
}

fn rule(kind: Kind, dxf: Option<Arc<Dxf>>) -> Rule {
    Rule {
        priority: 1,
        stop_if_true: false,
        kind,
        dxf,
    }
}

fn cfvo(kind: CfvoKind, value: &str) -> Cfvo {
    Cfvo {
        kind,
        value: value.to_owned(),
        gte: true,
    }
}

/// Numbers 1..=n down column A, with the rules on all of them.
fn book(n: u32, rules: Vec<Rule>) -> Workbook {
    let mut sheet = SheetBuilder::new("S", SheetKind::Worksheet);
    for row in 0..n {
        sheet.push(row, Cell::new(0, Value::Number(f64::from(row + 1))));
    }
    let mut sheet = sheet.finish();
    sheet.cond_formats.push(CondFormat {
        ranges: vec![CellRange::new(CellRef::new(0, 0), CellRef::new(n - 1, 0))],
        rules,
    });
    Workbook {
        sheets: vec![sheet],
        styles: vec![CellStyle::default()],
        ..Workbook::default()
    }
}

fn open(book: Workbook) -> SheetView {
    let mut view = SheetView::new(Arc::new(book));
    view.set_expr_engine(Box::new(aster_calc::ExprProbe::new()));
    view.resize(600, 400, 1.0);
    view
}

fn pixel(view: &mut SheetView, x: f64, y: f64) -> [u8; 4] {
    let width = usize::from(view.size().0);
    let data = view.render().data_as_u8_slice();
    let at = (y as usize * width + x as usize) * 4;
    [data[at], data[at + 1], data[at + 2], data[at + 3]]
}

/// A point inside a cell, away from its right-aligned number.
fn inside(view: &SheetView, at: &str) -> (f64, f64) {
    let (x0, y0, _, _) = view.cell_box(CellRef::parse(at).unwrap()).unwrap();
    (x0 + 3.0, y0 + 3.0)
}

fn probe(view: &mut SheetView, at: &str) -> [u8; 4] {
    let (x, y) = inside(view, at);
    pixel(view, x, y)
}

#[test]
fn a_rule_fills_the_cells_it_matches() {
    let greater = Kind::CellIs {
        op: Compare::Greater,
        first: Arc::from("2"),
        second: None,
    };
    let mut view = open(book(5, vec![rule(greater, dxf(RED))]));
    assert_eq!(probe(&mut view, "A2"), WHITE);
    assert_eq!(probe(&mut view, "A3"), RED);
    assert_eq!(probe(&mut view, "A5"), RED);
    // Cells outside the rule's range are untouched.
    assert_eq!(probe(&mut view, "A6"), WHITE);
}

#[test]
fn a_changed_cell_changes_what_the_rules_do() {
    let greater = Kind::CellIs {
        op: Compare::Greater,
        first: Arc::from("2"),
        second: None,
    };
    let mut view = open(book(5, vec![rule(greater, dxf(RED))]));
    assert_eq!(probe(&mut view, "A1"), WHITE);
    let at = CellRef::parse("A1").unwrap();
    view.workbook_mut()
        .set_cells(0, vec![(at, Some(Cell::new(0, Value::Number(50.0))))]);
    assert!(view.needs_render());
    assert_eq!(probe(&mut view, "A1"), RED);
}

#[test]
fn formulas_are_evaluated_for_each_cell_relative_to_the_rules_corner() {
    let banding = Kind::Expression(Arc::from("MOD(ROW(),2)=0"));
    let after = Kind::Expression(Arc::from("A1>3"));
    let mut view = open(book(
        6,
        vec![rule(banding, dxf(RED)), {
            let mut r = rule(after, dxf(GREEN));
            r.priority = 2;
            r
        }],
    ));
    assert_eq!(probe(&mut view, "A1"), WHITE);
    assert_eq!(probe(&mut view, "A2"), RED);
    // Row 4 is both: the first rule wins; row 5 is only the second.
    assert_eq!(probe(&mut view, "A4"), RED);
    assert_eq!(probe(&mut view, "A5"), GREEN);
}

#[test]
fn a_font_colour_and_bold_change_the_text() {
    let greater = Kind::CellIs {
        op: Compare::Greater,
        first: Arc::from("2"),
        second: None,
    };
    let styled = Some(Arc::new(Dxf {
        font_color: Some([0, 0, 0xff, 0xff]),
        bold: Some(true),
        ..Dxf::default()
    }));
    let mut plain = open(book(5, vec![]));
    let mut marked = open(book(5, vec![rule(greater, styled)]));
    let count = |view: &mut SheetView| {
        let (x0, y0, x1, y1) = view.cell_box(CellRef::parse("A4").unwrap()).unwrap();
        let width = usize::from(view.size().0);
        let data = view.render().data_as_u8_slice().to_vec();
        let mut blue = 0usize;
        for y in y0 as usize..y1 as usize {
            for x in x0 as usize..x1 as usize {
                let p = &data[(y * width + x) * 4..][..4];
                if p[2] > 0xc0 && p[0] < 0x60 && p[1] < 0x60 {
                    blue += 1;
                }
            }
        }
        blue
    };
    assert_eq!(count(&mut plain), 0);
    assert!(count(&mut marked) > 5);
}

#[test]
fn a_number_format_of_the_rule_changes_what_is_written() {
    let always = Kind::Expression(Arc::from("TRUE"));
    let format = Some(Arc::new(Dxf {
        format: Some(Arc::new(aster_sheets::Format::parse("0.00"))),
        ..Dxf::default()
    }));
    let mut view = open(book(2, vec![rule(always, format)]));
    view.render();
    let laid = view
        .laid(CellRef::new(0, 0), &Cell::new(0, Value::Number(1.0)), 64.0)
        .unwrap();
    // "1.00" is wider than "1".
    let plain = open(book(2, vec![]));
    let mut plain = plain;
    plain.render();
    let narrow = plain
        .laid(CellRef::new(0, 0), &Cell::new(0, Value::Number(1.0)), 64.0)
        .unwrap();
    assert!(laid.paragraph.width() > narrow.paragraph.width() + 4.0);
}

#[test]
fn a_colour_scale_blends_from_one_end_to_the_other() {
    let scale = Kind::ColorScale(vec![
        (cfvo(CfvoKind::Min, ""), [0xff, 0xff, 0xff, 0xff]),
        (cfvo(CfvoKind::Max, ""), [0x00, 0x00, 0xff, 0xff]),
    ]);
    let mut view = open(book(5, vec![rule(scale, None)]));
    assert_eq!(probe(&mut view, "A1"), WHITE);
    assert_eq!(probe(&mut view, "A5"), [0, 0, 0xff, 0xff]);
    let middle = probe(&mut view, "A3");
    assert_eq!(middle, [0x80, 0x80, 0xff, 0xff]);
}

#[test]
fn a_data_bar_is_as_long_as_the_value_says() {
    let bar = Kind::DataBar {
        min: cfvo(CfvoKind::Num, "0"),
        max: cfvo(CfvoKind::Num, "10"),
        color: [0, 0x70, 0xc0, 0xff],
        min_length: 0,
        max_length: 100,
        show_value: false,
    };
    let mut sheet = SheetBuilder::new("S", SheetKind::Worksheet);
    sheet.push(0, Cell::new(0, Value::Number(2.0)));
    sheet.push(1, Cell::new(0, Value::Number(8.0)));
    let mut sheet = sheet.finish();
    sheet.cond_formats.push(CondFormat {
        ranges: vec![CellRange::parse("A1:A2").unwrap()],
        rules: vec![rule(bar, None)],
    });
    let mut view = open(Workbook {
        sheets: vec![sheet],
        styles: vec![CellStyle::default()],
        ..Workbook::default()
    });
    let width = usize::from(view.size().0);
    let data = view.render().data_as_u8_slice().to_vec();
    let bar_pixels = |view: &SheetView, at: &str| {
        let (x0, y0, x1, y1) = view.cell_box(CellRef::parse(at).unwrap()).unwrap();
        let y = ((y0 + y1) / 2.0) as usize;
        (x0 as usize..x1 as usize)
            .filter(|x| {
                let p = &data[(y * width + x) * 4..][..4];
                u16::from(p[2]) > u16::from(p[0]) + 60
            })
            .count()
    };
    let (short, long) = (bar_pixels(&view, "A1"), bar_pixels(&view, "A2"));
    assert!(short > 5, "{short}");
    assert!(long > short * 3, "{short} {long}");
    // The numbers are not written (showValue off): only the bar's colour
    // and the background are in the row.
    let (x0, y0, x1, y1) = view.cell_box(CellRef::parse("A2").unwrap()).unwrap();
    let y = ((y0 + y1) / 2.0) as usize;
    let dark = (x0 as usize..x1 as usize)
        .filter(|x| {
            let p = &data[(y * width + x) * 4..][..4];
            p[0] < 0x40 && p[1] < 0x40 && p[2] < 0x40
        })
        .count();
    assert_eq!(dark, 0);
}

#[test]
fn an_icon_is_drawn_at_the_left_of_the_cell() {
    let step = |kind, value: &str| cfvo(kind, value);
    let icons = Kind::IconSet {
        set: IconSet::parse("3TrafficLights1").unwrap(),
        steps: vec![
            step(CfvoKind::Percent, "0"),
            step(CfvoKind::Percent, "33"),
            step(CfvoKind::Percent, "67"),
        ],
        reverse: false,
        show_value: true,
    };
    let mut view = open(book(3, vec![rule(icons, None)]));
    let width = usize::from(view.size().0);
    let data = view.render().data_as_u8_slice().to_vec();
    let coloured = |at: &str| {
        let (x0, y0, x1, y1) = view.cell_box(CellRef::parse(at).unwrap()).unwrap();
        let mut found = [0usize; 3];
        for y in y0 as usize..y1 as usize {
            for x in x0 as usize..(x0 + 20.0).min(x1) as usize {
                let p = &data[(y * width + x) * 4..][..4];
                if p[0] > 0xc0 && p[1] < 0x60 {
                    found[0] += 1;
                } else if p[0] > 0xe0 && p[1] > 0xa0 && p[2] < 0x40 {
                    found[1] += 1;
                } else if p[1] > 0x90 && p[0] < 0x60 {
                    found[2] += 1;
                }
            }
        }
        found
    };
    let (low, middle, high) = (coloured("A1"), coloured("A2"), coloured("A3"));
    assert!(low[0] > 20 && low[1] == 0 && low[2] == 0, "{low:?}");
    assert!(middle[1] > 20 && middle[0] == 0, "{middle:?}");
    assert!(high[2] > 20 && high[0] == 0, "{high:?}");
}

#[test]
fn rules_follow_inserted_rows() {
    let greater = Kind::CellIs {
        op: Compare::Greater,
        first: Arc::from("3"),
        second: None,
    };
    let mut view = open(book(5, vec![rule(greater, dxf(RED))]));
    assert_eq!(probe(&mut view, "A4"), RED);
    view.workbook_mut().restructure(
        0,
        &Lines {
            columns: false,
            at: 0,
            count: 2,
            insert: true,
        },
    );
    // The two new rows are outside the rule; its cells moved down.
    assert_eq!(probe(&mut view, "A1"), WHITE);
    assert_eq!(probe(&mut view, "A2"), WHITE);
    assert_eq!(probe(&mut view, "A3"), WHITE);
    assert_eq!(probe(&mut view, "A6"), RED);
    assert_eq!(probe(&mut view, "A7"), RED);
}

#[test]
fn a_big_sheet_stays_quick_and_the_work_is_kept_between_frames() {
    let top = Kind::Top {
        rank: 10,
        percent: false,
        bottom: false,
    };
    let scale = Kind::ColorScale(vec![
        (cfvo(CfvoKind::Min, ""), [0xff, 0xff, 0xff, 0xff]),
        (cfvo(CfvoKind::Max, ""), [0, 0, 0xff, 0xff]),
    ]);
    let rows = 200_000;
    let mut view = open(book(
        rows,
        vec![rule(top, dxf(RED)), {
            let mut r = rule(scale, None);
            r.priority = 2;
            r
        }],
    ));
    let started = std::time::Instant::now();
    view.render();
    let first = started.elapsed();
    // Scrolling reuses the statistics.
    let started = std::time::Instant::now();
    for _ in 0..20 {
        view.scroll_by(0.0, 400.0);
        view.render();
    }
    let scrolled = started.elapsed();
    assert!(first.as_secs_f64() < 5.0, "{first:?}");
    assert!(scrolled.as_secs_f64() < 5.0, "{scrolled:?}");
}

/// An engine that is asked for nothing: rules that need one do not apply,
/// and nothing fails.
#[test]
fn without_an_engine_formula_rules_do_nothing() {
    struct Never;
    impl ExprEngine for Never {
        fn evaluate(&mut self, _: &Workbook, _: usize, _: &str, _: CellRef) -> Option<Value> {
            None
        }
    }
    let mut view = SheetView::new(Arc::new(book(
        3,
        vec![rule(Kind::Expression(Arc::from("TRUE")), dxf(RED))],
    )));
    view.resize(600, 400, 1.0);
    assert_eq!(probe(&mut view, "A1"), WHITE);
    view.set_expr_engine(Box::new(Never));
    assert_eq!(probe(&mut view, "A1"), WHITE);
}
