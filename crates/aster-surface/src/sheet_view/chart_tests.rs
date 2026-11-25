//! Charts on screen: what each kind draws, that they follow their cells,
//! and that hostile data stays bounded.

use std::sync::Arc;

use aster_sheets::chart::{
    AxisKind, AxisSide, Chart, ChartAxis, Grouping, Labels, LegendPos, Marker, MarkerSymbol, Plot,
    PlotKind, Series, cell_source,
};
use aster_sheets::{
    Anchor, Cell, CellRef, CellStyle, Picture, Placement, SheetBuilder, SheetKind, Value, Workbook,
};

use super::*;

const RED: [u8; 4] = [0xe0, 0x20, 0x20, 0xff];
const BLUE: [u8; 4] = [0x20, 0x40, 0xe0, 0xff];
const GREEN: [u8; 4] = [0x20, 0xb0, 0x40, 0xff];

fn axes() -> Vec<ChartAxis> {
    let axis = |id, kind, side, gridlines| ChartAxis {
        id,
        kind,
        side,
        deleted: false,
        reversed: false,
        min: None,
        max: None,
        major_unit: None,
        log_base: None,
        gridlines,
        title: None,
        format: None,
        labels: true,
    };
    vec![
        axis(1, AxisKind::Category, AxisSide::Bottom, false),
        axis(2, AxisKind::Value, AxisSide::Left, true),
    ]
}

fn series(values: &str, color: [u8; 4]) -> Series {
    Series {
        name: Some(cell_source("S!$B$1")),
        categories: Some(cell_source("S!$A$2:$A$7")),
        values: Some(cell_source(values)),
        fill: Some(color),
        line: Some(color),
        ..Series::default()
    }
}

fn chart(kind: PlotKind, series: Vec<Series>) -> Chart {
    Chart {
        title: Some("売上の推移".to_owned()),
        plots: vec![Plot {
            kind,
            series,
            axes: vec![1, 2],
        }],
        axes: axes(),
        legend: Some(LegendPos::Bottom),
        visible_only: true,
        ..Chart::default()
    }
}

fn bar(grouping: Grouping, horizontal: bool) -> PlotKind {
    PlotKind::Bar {
        horizontal,
        grouping,
        gap: 80.0,
        overlap: if grouping == Grouping::Clustered {
            0.0
        } else {
            100.0
        },
    }
}

/// Data in A1:C7 and a chart over E2:N20.
fn book(chart: Chart) -> Workbook {
    let mut sheet = SheetBuilder::new("S", SheetKind::Worksheet);
    sheet.push(0, Cell::new(1, Value::Text("甲".into())));
    sheet.push(0, Cell::new(2, Value::Text("乙".into())));
    for (row, name) in ["1月", "2月", "3月", "4月", "5月", "6月"]
        .iter()
        .enumerate()
    {
        let row = row as u32 + 1;
        sheet.push(row, Cell::new(0, Value::Text((*name).into())));
        sheet.push(row, Cell::new(1, Value::Number(f64::from(row * 10))));
        sheet.push(row, Cell::new(2, Value::Number(f64::from(70 - row * 8))));
    }
    sheet.pictures.push(Picture {
        placement: Placement::TwoCell {
            from: Anchor {
                cell: CellRef::new(1, 4),
                dx: 0.0,
                dy: 0.0,
            },
            to: Anchor {
                cell: CellRef::new(19, 13),
                dx: 0.0,
                dy: 0.0,
            },
        },
        image: None,
        name: "グラフ 1".into(),
        chart: Some(Arc::new(chart)),
    });
    Workbook {
        sheets: vec![sheet.finish()],
        styles: vec![CellStyle::default()],
        ..Workbook::default()
    }
}

fn open(book: Workbook) -> SheetView {
    let mut view = SheetView::new(Arc::new(book));
    view.resize(900, 600, 1.0);
    view
}

/// The window's pixels and the chart's rectangle in it.
fn shot(view: &mut SheetView) -> (Vec<u8>, (usize, usize, usize, usize), usize) {
    let width = usize::from(view.size().0);
    let data = view.render().data_as_u8_slice().to_vec();
    let (x0, y0, _, _) = view.cell_box(CellRef::new(1, 4)).unwrap();
    let (_, _, x1, y1) = view.cell_box(CellRef::new(18, 13)).unwrap();
    if let Ok(dir) = std::env::var("ASTER_DUMP_DIR") {
        static COUNT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        let png = view.encode_png().unwrap();
        let name = format!(
            "{}/chart-{}-{}.png",
            dir,
            std::thread::current()
                .name()
                .unwrap_or("t")
                .rsplit("::")
                .next()
                .unwrap(),
            COUNT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        );
        std::fs::write(name, png).unwrap();
    }
    (
        data,
        (x0 as usize, y0 as usize, x1 as usize, y1 as usize),
        width,
    )
}

/// How many pixels in the chart's rectangle are near `color`.
fn near(shot: &(Vec<u8>, (usize, usize, usize, usize), usize), color: [u8; 4]) -> usize {
    let (data, (x0, y0, x1, y1), width) = shot;
    let mut count = 0;
    for y in *y0..*y1 {
        for x in *x0..*x1 {
            let p = &data[(y * width + x) * 4..][..4];
            if (0..3).all(|i| p[i].abs_diff(color[i]) < 24) {
                count += 1;
            }
        }
    }
    count
}

fn non_background(shot: &(Vec<u8>, (usize, usize, usize, usize), usize)) -> usize {
    let (data, (x0, y0, x1, y1), width) = shot;
    let mut count = 0;
    for y in *y0 + 2..*y1 - 2 {
        for x in *x0 + 2..*x1 - 2 {
            if data[(y * width + x) * 4..][..3] != [0xff, 0xff, 0xff] {
                count += 1;
            }
        }
    }
    count
}

/// Grey and dark pixels: text and axis lines, not the series' colours.
fn ink(shot: &(Vec<u8>, (usize, usize, usize, usize), usize)) -> usize {
    let (data, (x0, y0, x1, y1), width) = shot;
    let mut count = 0;
    for y in *y0 + 2..*y1 - 2 {
        for x in *x0 + 2..*x1 - 2 {
            let p = &data[(y * width + x) * 4..][..3];
            if p[0].abs_diff(p[1]) < 14 && p[1].abs_diff(p[2]) < 14 && p[0] < 0xd8 {
                count += 1;
            }
        }
    }
    count
}

#[test]
fn a_clustered_column_chart_draws_bars_in_the_series_colours() {
    let c = chart(
        bar(Grouping::Clustered, false),
        vec![series("S!$B$2:$B$7", RED), series("S!$C$2:$C$7", BLUE)],
    );
    let mut view = open(book(c));
    let drawn = shot(&mut view);
    let (red, blue) = (near(&drawn, RED), near(&drawn, BLUE));
    assert!(red > 800 && blue > 800, "{red} {blue}");
    // Bars are solid blocks: far more than any line or text of that colour.
    assert!(non_background(&drawn) > red + blue);
}

#[test]
fn a_chart_follows_its_cells() {
    let c = chart(
        bar(Grouping::Clustered, false),
        vec![series("S!$B$2:$B$7", RED)],
    );
    let mut view = open(book(c));
    let before = near(&shot(&mut view), RED);
    // Triple one value: the bar and the axis it sets are different.
    let at = CellRef::new(1, 1);
    view.workbook_mut()
        .set_cells(0, vec![(at, Some(Cell::new(1, Value::Number(500.0))))]);
    let after = near(&shot(&mut view), RED);
    assert_ne!(before, after);
    assert!(
        after < before,
        "the other bars are shorter now: {before} {after}"
    );
}

#[test]
fn stacked_and_percent_stacked_bars_fill_the_plot_to_the_top() {
    let two = |kind| {
        chart(
            kind,
            vec![series("S!$B$2:$B$7", RED), series("S!$C$2:$C$7", BLUE)],
        )
    };
    let mut stacked = open(book(two(bar(Grouping::Stacked, false))));
    let mut percent = open(book(two(bar(Grouping::Percent, false))));
    let (s, p) = (shot(&mut stacked), shot(&mut percent));
    assert!(near(&s, RED) > 500 && near(&s, BLUE) > 500);
    // In a percent stack every bar is full height: more colour than the
    // plain stack, whose bars vary.
    assert!(near(&p, RED) + near(&p, BLUE) > near(&s, RED) + near(&s, BLUE));
}

#[test]
fn horizontal_bars_lie_down() {
    let c = chart(
        bar(Grouping::Clustered, true),
        vec![series("S!$B$2:$B$7", GREEN)],
    );
    let mut view = open(book(c));
    let drawn = shot(&mut view);
    assert!(near(&drawn, GREEN) > 800);
    // The bars of a horizontal chart start at the same x: the column of
    // pixels just right of the axis has green in many rows.
    let (data, (x0, y0, x1, y1), width) = &drawn;
    let mut best = 0;
    for x in *x0..*x1 {
        let rows = (*y0..*y1)
            .filter(|y| {
                let p = &data[(y * width + x) * 4..][..3];
                (0..3).all(|i| p[i].abs_diff(GREEN[i]) < 24)
            })
            .count();
        best = best.max(rows);
    }
    assert!(best > 100, "{best}");
}

#[test]
fn lines_markers_areas_scatter_and_pie_draw() {
    let line = |markers: bool| {
        let mut s = series("S!$B$2:$B$7", RED);
        s.marker = Marker {
            symbol: if markers {
                MarkerSymbol::Circle
            } else {
                MarkerSymbol::None
            },
            size: Some(9.0),
            fill: None,
        };
        chart(
            PlotKind::Line {
                grouping: Grouping::Clustered,
            },
            vec![s],
        )
    };
    let with = near(&shot(&mut open(book(line(true)))), RED);
    let without = near(&shot(&mut open(book(line(false)))), RED);
    assert!(without > 100 && with > without + 30, "{with} {without}");

    let area = chart(
        PlotKind::Area {
            grouping: Grouping::Clustered,
        },
        vec![series("S!$B$2:$B$7", RED)],
    );
    assert!(near(&shot(&mut open(book(area))), RED) > 2_000);

    let mut points = series("S!$C$2:$C$7", BLUE);
    points.categories = Some(cell_source("S!$B$2:$B$7"));
    points.no_line = true;
    points.marker.symbol = MarkerSymbol::Square;
    let mut scatter = chart(PlotKind::Scatter, vec![points]);
    scatter.plots[0].axes = vec![1, 2];
    scatter.axes[0].kind = AxisKind::Value;
    let drawn = shot(&mut open(book(scatter)));
    let squares = near(&drawn, BLUE);
    assert!(squares > 6 * 20 && squares < 6 * 150, "{squares}");

    let mut pie = series("S!$B$2:$B$7", RED);
    pie.point_fills = vec![(0, RED), (1, BLUE), (2, GREEN)];
    let pie = chart(
        PlotKind::Pie {
            hole: None,
            first_angle: 0.0,
        },
        vec![pie],
    );
    let drawn = shot(&mut open(book(pie)));
    let (r, b, g) = (near(&drawn, RED), near(&drawn, BLUE), near(&drawn, GREEN));
    // Slices are in proportion to 10, 20, 30.
    assert!(g > b && b > r && r > 300, "{r} {b} {g}");

    let mut ring = series("S!$B$2:$B$7", RED);
    ring.point_fills = vec![(0, RED), (1, BLUE), (2, GREEN)];
    let ring = chart(
        PlotKind::Pie {
            hole: Some(0.6),
            first_angle: 0.0,
        },
        vec![ring],
    );
    let doughnut = shot(&mut open(book(ring)));
    assert!(near(&doughnut, GREEN) < g, "the hole takes pixels away");
}

#[test]
fn titles_axis_titles_legend_labels_and_the_note_are_text() {
    let plain = chart(
        bar(Grouping::Clustered, false),
        vec![series("S!$B$2:$B$7", RED)],
    );
    let mut bare = plain.clone();
    bare.title = None;
    bare.legend = None;
    for axis in &mut bare.axes {
        axis.labels = false;
        axis.gridlines = false;
    }
    let full = ink(&shot(&mut open(book(plain.clone()))));
    let stripped = ink(&shot(&mut open(book(bare))));
    assert!(full > stripped + 200, "{full} {stripped}");
    // Axis titles and data labels add text.
    let mut titled = plain.clone();
    titled.axes[1].title = Some("万円".to_owned());
    titled.axes[0].title = Some("月".to_owned());
    titled.plots[0].series[0].labels = Labels {
        value: true,
        ..Labels::default()
    };
    assert!(ink(&shot(&mut open(book(titled)))) > full + 100);
    // What is not drawn is said.
    let mut noted = plain;
    noted.not_shown.push("trend line".to_owned());
    assert!(ink(&shot(&mut open(book(noted)))) > full + 50);
}

#[test]
fn a_chart_with_nothing_to_plot_or_nothing_valid_still_draws_a_frame() {
    for c in [
        Chart::default(),
        Chart {
            title: Some("空".into()),
            not_shown: vec!["radar chart".into()],
            ..Chart::default()
        },
        chart(bar(Grouping::Clustered, false), vec![Series::default()]),
        chart(
            bar(Grouping::Clustered, false),
            vec![series("Nowhere!$A$1:$A$5", RED)],
        ),
        chart(
            bar(Grouping::Clustered, false),
            vec![series("not a reference", RED)],
        ),
        chart(
            PlotKind::Pie {
                hole: Some(0.9),
                first_angle: 400.0,
            },
            vec![series("S!$B$2:$B$7", RED), series("S!$C$2:$C$7", BLUE)],
        ),
    ] {
        let mut view = open(book(c));
        let drawn = shot(&mut view);
        assert!(non_background(&drawn) < 40_000);
    }
}

#[test]
fn hostile_charts_are_bounded_and_quick() {
    // 64 series of 10,000 points in a line, and 200,000 in a bar chart.
    let big_sheet = || {
        let mut sheet = SheetBuilder::new("S", SheetKind::Worksheet);
        for row in 0..200_000u32 {
            sheet.push(row, Cell::new(0, Value::Number(f64::from(row % 977))));
        }
        sheet.finish()
    };
    let many = |n: usize, kind: PlotKind, range: &str| Chart {
        plots: vec![Plot {
            kind,
            series: (0..n)
                .map(|i| Series {
                    values: Some(cell_source(range)),
                    marker: Marker {
                        symbol: MarkerSymbol::Circle,
                        ..Marker::default()
                    },
                    ..Series {
                        fill: Some([i as u8, 100, 200, 255]),
                        ..Series::default()
                    }
                })
                .collect(),
            axes: vec![1, 2],
        }],
        axes: axes(),
        legend: Some(LegendPos::Right),
        visible_only: true,
        ..Chart::default()
    };
    let charts = [
        many(
            100,
            PlotKind::Line {
                grouping: Grouping::Clustered,
            },
            "S!$A$1:$A$10000",
        ),
        many(
            64,
            PlotKind::Line {
                grouping: Grouping::Stacked,
            },
            "S!$A$1:$A$200000",
        ),
        many(5, bar(Grouping::Clustered, false), "S!$A$1:$A$200000"),
        many(
            3,
            PlotKind::Area {
                grouping: Grouping::Percent,
            },
            "S!$A$1:$A$200000",
        ),
        many(2, PlotKind::Scatter, "S!$A$1:$A$200000"),
        many(
            1,
            PlotKind::Pie {
                hole: None,
                first_angle: 0.0,
            },
            "S!$A$1:$A$200000",
        ),
        many(1, bar(Grouping::Clustered, true), "S!$A$1:$A$1048576"),
    ];
    for c in charts {
        let mut sheet = big_sheet();
        sheet.pictures.push(Picture {
            placement: Placement::OneCell {
                from: Anchor::default(),
                width: 700.0,
                height: 450.0,
            },
            image: None,
            name: "c".into(),
            chart: Some(Arc::new(c)),
        });
        let mut view = SheetView::new(Arc::new(Workbook {
            sheets: vec![sheet],
            styles: vec![CellStyle::default()],
            ..Workbook::default()
        }));
        view.resize(900, 600, 1.0);
        let started = std::time::Instant::now();
        view.render();
        let took = started.elapsed();
        assert!(took.as_secs_f64() < 20.0, "{took:?}");
        // Scrolling draws the same pixels again, not the chart again.
        let started = std::time::Instant::now();
        for _ in 0..10 {
            view.scroll_by(0.0, 3.0);
            view.render();
        }
        let again = started.elapsed();
        assert!(again.as_secs_f64() < 5.0, "{again:?}");
    }
}
