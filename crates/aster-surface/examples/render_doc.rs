//! Lay out a sample Writer document built in code and write one PNG per page.
//!
//! `cargo run -p aster-surface --example render_doc -- out-dir [scale]`
//!
//! The document uses most of what the layout engine does: headings, text in
//! Chinese, Japanese and English, direct formatting, justified text with a
//! first-line indent, lists, tab leaders, borders and shading, tables with
//! merged cells and a repeated header, nested tables, pictures, a text box,
//! headers and footers with page numbers, a footnote, columns, a landscape
//! section on a document grid and page breaks.

use std::path::Path;
use std::sync::Arc;

use aster_layout::Layouter;
use aster_surface::HAN_FALLBACK_FAMILIES;
use aster_surface::doc_paint::DocPainter;
use aster_text::Shaper;
use aster_writer::*;

fn look(size: f32) -> RunLook {
    RunLook {
        font: Some("Calibri".into()),
        east_asian_font: Some("宋体".into()),
        size,
        ..RunLook::default()
    }
}

fn text(s: &str, look: &RunLook) -> Run {
    Run::text(s, look.clone())
}

fn props() -> ParaProps {
    ParaProps {
        space_after: 120,
        line: LineSpacing::Multiple(1.15),
        ..ParaProps::default()
    }
}

fn para(runs: Vec<Run>, props: ParaProps) -> Block {
    Block::paragraph(Paragraph {
        runs,
        props,
        base: look(10.5),
        mark: look(10.5),
        ..Paragraph::default()
    })
}

fn plain(s: &str) -> Block {
    para(vec![text(s, &look(10.5))], props())
}

fn heading(s: &str, level: u8) -> Block {
    let size = [20.0, 15.0, 12.5][level as usize];
    let mut l = look(size);
    l.bold = true;
    l.east_asian_font = Some("黑体".into());
    l.color = Some([0x1f, 0x3a, 0x5f, 255]);
    Block::paragraph(Paragraph {
        runs: vec![text(s, &l)],
        props: ParaProps {
            outline_level: Some(level),
            space_before: if level == 0 { 240 } else { 200 },
            space_after: 120,
            keep_next: true,
            ..ParaProps::default()
        },
        base: l.clone(),
        mark: l,
        ..Paragraph::default()
    })
}

fn border(width: f32, color: Rgba) -> Border {
    Border {
        style: BorderStyle::Single,
        width,
        color: Some(color),
        space: 4.0,
    }
}

fn png(width: u32, height: u32) -> Arc<[u8]> {
    let mut bytes = Vec::new();
    {
        let mut encoder = png::Encoder::new(&mut bytes, width, height);
        encoder.set_color(png::ColorType::Rgba);
        encoder.set_depth(png::BitDepth::Eight);
        let mut writer = encoder.write_header().expect("header");
        let mut data = Vec::new();
        for y in 0..height {
            for x in 0..width {
                let (dx, dy) = (
                    x as f32 - width as f32 / 2.0,
                    y as f32 - height as f32 / 2.0,
                );
                let inside = dx * dx + dy * dy < (height as f32 / 3.0).powi(2);
                data.extend_from_slice(&if inside {
                    [255, 200, 40, 255]
                } else {
                    [
                        (40 + x * 200 / width) as u8,
                        110,
                        (230 - y * 150 / height) as u8,
                        255,
                    ]
                });
            }
        }
        writer.write_image_data(&data).expect("data");
    }
    bytes.into()
}

fn picture(width_pt: f64, height_pt: f64, float: Option<Float>) -> Run {
    let emu = |pt: f64| (pt * 12_700.0) as i64;
    Run::object(
        Inline::Drawing(Arc::new(Drawing {
            size: (emu(width_pt), emu(height_pt)),
            content: DrawingContent::Picture(Picture {
                image: png(96, 64),
                crop: [0.0; 4],
            }),
            float,
            alt: None,
        })),
        look(10.5),
    )
}

fn label(text: &str, indent: Twips, hang: Twips, mut base: ParaProps, runs: Vec<Run>) -> Block {
    base.indent_left = indent;
    base.first_line = -hang;
    base.space_after = 40;
    Block::paragraph(Paragraph {
        runs,
        props: base,
        base: look(10.5),
        mark: look(10.5),
        label: Some(Label {
            text: text.into(),
            look: look(10.5),
            suffix: Suffix::Tab,
            align: Align::Left,
        }),
        ..Paragraph::default()
    })
}

fn cell(blocks: Vec<Block>, span: u32, shading: Option<Rgba>) -> Cell {
    let line = Some(border(0.5, [0x55, 0x55, 0x55, 255]));
    Cell {
        blocks,
        span,
        borders: [line; 4].map(|b| {
            b.map(|mut b| {
                b.space = 0.0;
                b
            })
        }),
        shading,
        margins: [100, 40, 100, 40],
        ..Cell::default()
    }
}

fn small(s: &str) -> Block {
    para(vec![text(s, &look(10.0))], ParaProps::default())
}

fn big_table() -> Block {
    let head = |s: &str| {
        let mut l = look(10.5);
        l.bold = true;
        l.color = Some([255, 255, 255, 255]);
        cell(
            vec![para(vec![text(s, &l)], ParaProps::default())],
            1,
            Some([0x2f, 0x54, 0x96, 255]),
        )
    };
    let mut rows = vec![Row {
        cells: vec![head("编号"), head("名称 Name"), head("数量"), head("備考")],
        header: true,
        ..Row::default()
    }];
    for i in 1..=34 {
        let shade = (i % 2 == 0).then_some([0xea, 0xef, 0xf7, 255]);
        let mut row = Row {
            cells: vec![
                cell(vec![small(&format!("{i:03}"))], 1, shade),
                cell(
                    vec![small(&format!("项目 {i}: Alpha beta gamma"))],
                    1,
                    shade,
                ),
                cell(vec![small(&format!("{}", i * 7 % 50))], 1, shade),
                cell(
                    vec![small(if i % 5 == 0 {
                        "メモ：確認済み。"
                    } else {
                        ""
                    })],
                    1,
                    shade,
                ),
            ],
            ..Row::default()
        };
        if i % 11 == 3 {
            row.height = RowHeight::AtLeast(700);
        }
        rows.push(row);
    }
    // A row with a vertically merged cell and a spanning cell.
    let merged = |merge| Cell {
        merge,
        ..cell(
            if merge == VMerge::Start {
                vec![small("合并的单元格 merged over two rows")]
            } else {
                vec![para(vec![], ParaProps::default())]
            },
            1,
            Some([0xfd, 0xe9, 0xd9, 255]),
        )
    };
    rows.push(Row {
        cells: vec![
            merged(VMerge::Start),
            cell(
                vec![small("跨两列 spans two columns")],
                2,
                Some([0xe2, 0xf0, 0xd9, 255]),
            ),
            cell(vec![small("x")], 1, None),
        ],
        ..Row::default()
    });
    rows.push(Row {
        cells: vec![
            merged(VMerge::Continue),
            cell(vec![small("b")], 1, None),
            cell(vec![small("c")], 1, None),
            cell(vec![small("d")], 1, None),
        ],
        ..Row::default()
    });
    Block::table(Table {
        columns: vec![1100, 3800, 1500, 2600],
        rows,
        props: TableProps::default(),
        source: None,
    })
}

fn nested_table() -> Block {
    let inner = Block::table(Table {
        columns: vec![1500, 1500],
        rows: vec![Row {
            cells: vec![
                cell(vec![small("nested a")], 1, Some([0xff, 0xf2, 0xcc, 255])),
                cell(vec![small("嵌套 b")], 1, None),
            ],
            ..Row::default()
        }],
        props: TableProps::default(),
        source: None,
    });
    let mut center = cell(vec![small("垂直居中 centred")], 1, None);
    center.align = CellAlign::Center;
    Block::table(Table {
        columns: vec![3400, 3400, 2200],
        rows: vec![Row {
            cells: vec![
                cell(vec![small("A nested table follows:"), inner], 1, None),
                center,
                cell(vec![small("height at least 1.5 cm")], 1, None),
            ],
            height: RowHeight::AtLeast(850),
            ..Row::default()
        }],
        props: TableProps::default(),
        source: None,
    })
}

fn section(mut f: impl FnMut(&mut Section)) -> Arc<Section> {
    let mut s = Section::default();
    f(&mut s);
    Arc::new(s)
}

fn end_of(section: Arc<Section>) -> Block {
    Block::paragraph(Paragraph {
        props: ParaProps::default(),
        base: look(10.5),
        mark: look(10.5),
        section: Some(section),
        ..Paragraph::default()
    })
}

fn build() -> Document {
    let l = look(10.5);
    let zh = "排版引擎需要处理“禁则”：句号、逗号和后引号（」』）》等）不能出现在行首，前引号（「『（《等）不能出现在行尾。两端对齐时，标点符号之间的空白应当被挤压，否则版面会显得松散；中英文之间还要留出四分之一个字宽的空隙，例如 Aster Office 与 Rust 1.95。";
    let ja = "日本語の組版では、行頭に句読点や閉じ括弧（」）、小書きの仮名（ゃ・ゅ・ょ・っ）、長音記号「ー」などを置くことはできません。これを「行頭禁則」と呼びます。コンピューターでチェックしましょう。";
    let en = "Word processors lay text out in lines, lines in paragraphs and paragraphs on pages. This paragraph is set in the Latin font with a fallback for East Asian text, and it is long enough to wrap over several lines so that alignment, spacing and hyphen-less breaking can be checked at a glance.";

    let mut body: Vec<Block> = vec![heading("Aster Office 排版演示 Layout Sample", 0)];
    body.push(heading("1. 正文 Body text", 1));
    // Justified, first line indented two characters.
    body.push(para(
        vec![text(zh, &l)],
        ParaProps {
            align: Align::Justify,
            first_line: 420,
            ..props()
        },
    ));
    body.push(para(
        vec![text(ja, &l)],
        ParaProps {
            align: Align::Justify,
            first_line: 420,
            ..props()
        },
    ));
    // Direct formatting.
    let mut bold = l.clone();
    bold.bold = true;
    let mut italic = l.clone();
    italic.italic = true;
    let mut under = l.clone();
    under.underline = Some(Underline {
        style: UnderlineStyle::Single,
        color: None,
    });
    let mut double = l.clone();
    double.underline = Some(Underline {
        style: UnderlineStyle::Double,
        color: Some([200, 0, 0, 255]),
    });
    let mut strike = l.clone();
    strike.strike = true;
    let mut red = l.clone();
    red.color = Some([0xc0, 0x20, 0x20, 255]);
    let mut marked = l.clone();
    marked.highlight = Some([255, 240, 90, 255]);
    let mut sup = l.clone();
    sup.position = VertAlign::Superscript;
    let mut sub = l.clone();
    sub.position = VertAlign::Subscript;
    let mut caps = l.clone();
    caps.small_caps = true;
    let mut spaced = l.clone();
    spaced.spacing = 40;
    let mut big = l.clone();
    big.size = 16.0;
    body.push(para(
        vec![
            text(en, &l),
            text(" Direct formatting: ", &l),
            text("bold", &bold),
            text(", ", &l),
            text("italic", &italic),
            text(", ", &l),
            text("underlined", &under),
            text(", ", &l),
            text("double and red", &double),
            text(", ", &l),
            text("struck", &strike),
            text(", ", &l),
            text("coloured", &red),
            text(", ", &l),
            text("highlighted", &marked),
            text(", x", &l),
            text("2", &sup),
            text(" and H", &l),
            text("2", &sub),
            text("O, ", &l),
            text("small caps", &caps),
            text(", ", &l),
            text("spaced out", &spaced),
            text(", and ", &l),
            text("larger", &big),
            text(" text; 中文也可以加粗。", &bold),
            Run::object(Inline::Footnote(1), {
                let mut f = l.clone();
                f.position = VertAlign::Superscript;
                f
            }),
        ],
        ParaProps {
            align: Align::Left,
            ..props()
        },
    ));

    body.push(heading("2. 列表 Lists", 1));
    let item = |label_text: &str, indent, hang, s: &str| {
        label(label_text, indent, hang, props(), vec![text(s, &l)])
    };
    body.push(item("1.", 420, 420, "第一项 First item of a numbered list"));
    body.push(item("1.1", 900, 480, "Second level, 第二级编号"));
    body.push(item("(a)", 1440, 540, "Third level that is long enough to wrap onto a second line so that the hanging indent shows."));
    body.push(item("(b)", 1440, 540, "Another third-level item"));
    body.push(item("2.", 420, 420, "第二项 Second item"));
    body.push(item("•", 420, 300, "Bullet list item 项目符号"));
    body.push(item("•", 420, 300, "Another bullet with wrapping text, repeated: the quick brown fox jumps over the lazy dog, and then jumps again."));

    body.push(heading("3. 制表位 Tabs and leaders", 1));
    let tabs = vec![TabStop {
        position: 9000,
        align: TabAlign::Right,
        leader: TabLeader::Dots,
    }];
    for (name, page) in [
        ("第一章 概述 Overview", "1"),
        ("第二章 排版规则 Rules", "12"),
        ("第三章 表格 Tables", "27"),
    ] {
        body.push(para(
            vec![text(&format!("{name}\t{page}"), &l)],
            ParaProps {
                tabs: tabs.clone(),
                space_after: 40,
                ..ParaProps::default()
            },
        ));
    }
    body.push(para(
        vec![text("Price\t12.50\t3.1\t", &l)],
        ParaProps {
            tabs: vec![
                TabStop {
                    position: 3000,
                    align: TabAlign::Decimal,
                    leader: TabLeader::Hyphens,
                },
                TabStop {
                    position: 5400,
                    align: TabAlign::Center,
                    leader: TabLeader::Underscore,
                },
            ],
            ..props()
        },
    ));

    body.push(heading("4. 边框与底纹 Borders and shading", 1));
    let boxed = ParaProps {
        borders: [Some(border(1.0, [0x2f, 0x54, 0x96, 255])); 4],
        border_between: Some(border(0.5, [0x2f, 0x54, 0x96, 255])),
        shading: Some([0xe8, 0xf0, 0xfb, 255]),
        indent_left: 200,
        indent_right: 200,
        space_after: 0,
        ..ParaProps::default()
    };
    body.push(para(vec![text("A paragraph with a border and shading: 带边框和底纹的段落，相邻的同样边框的段落共用一个框。", &l)], boxed.clone()));
    body.push(para(
        vec![text("The neighbour shares the box, 中间有分隔线。", &l)],
        ParaProps {
            space_after: 160,
            ..boxed
        },
    ));

    body.push(heading("5. 图片与文本框 Pictures and text boxes", 1));
    body.push(para(
        vec![
            text("An inline picture sits in the line: ", &l),
            picture(60.0, 40.0, None),
            text(" and text goes on after it. 行内图片。", &l),
        ],
        props(),
    ));
    let float = Float {
        x: FloatAxis {
            from: FloatFrom::Margin,
            at: FloatAt::End,
        },
        y: FloatAxis {
            from: FloatFrom::Paragraph,
            at: FloatAt::Offset(0),
        },
        wrap: Wrap::Square,
        behind: false,
        distance: [127_000; 4],
    };
    body.push(para(
        vec![
            picture(90.0, 60.0, Some(float)),
            text("This paragraph has a floating picture anchored at its start and placed at the right margin; the text runs beside it and goes back to the full width below it, the way Word wraps text around a square picture, and the next paragraph keeps clear of it too. 浮动图片在段落开头锚定，文字绕到图片的左边。", &l),
        ],
        props(),
    ));
    let text_box = Drawing {
        size: (200 * 12_700, 56 * 12_700),
        content: DrawingContent::TextBox(TextBox {
            blocks: vec![para(
                vec![text("A text box 文本框: laid out like a table cell.", &l)],
                ParaProps::default(),
            )],
            fill: Some([0xff, 0xf8, 0xdc, 255]),
            line: Some(border(1.0, [0xb0, 0x80, 0x20, 255])),
            insets: [91_440, 45_720, 91_440, 45_720],
        }),
        float: Some(Float {
            x: FloatAxis {
                from: FloatFrom::Column,
                at: FloatAt::Center,
            },
            y: FloatAxis {
                from: FloatFrom::Paragraph,
                at: FloatAt::Offset(0),
            },
            wrap: Wrap::TopAndBottom,
            behind: false,
            distance: [0, 63_500, 0, 63_500],
        }),
        alt: None,
    };
    body.push(para(
        vec![
            Run::object(Inline::Drawing(Arc::new(text_box)), l.clone()),
            text("The next drawing is a chart this version cannot draw:", &l),
            Run::object(
                Inline::Drawing(Arc::new(Drawing {
                    size: (80 * 12_700, 40 * 12_700),
                    content: DrawingContent::Other("chart".into()),
                    float: None,
                    alt: None,
                })),
                l.clone(),
            ),
        ],
        props(),
    ));
    // An explicit page break.
    body.push(para(
        vec![
            text("Text before an explicit page break.", &l),
            Run::object(Inline::Break(Break::Page), l.clone()),
            text(
                "Text after the page break starts a new page. 分页符之后。",
                &l,
            ),
        ],
        props(),
    ));
    body.push(heading("6. 表格 Tables", 1));
    body.push(plain("The table below has a header row that repeats on each page, merged cells, borders and shading."));
    body.push(big_table());
    body.push(plain(" "));
    body.push(nested_table());
    body.push(plain("After the tables."));
    body.push(end_of(section(|s| {
        s.headers = Marginals {
            default: Some(0),
            ..Marginals::default()
        };
        s.footers = Marginals {
            default: Some(1),
            ..Marginals::default()
        };
    })));

    // Two columns.
    let columns = |s: &mut Section| {
        s.columns = Columns {
            count: 2,
            space: 500,
            widths: Vec::new(),
            separator: true,
        };
        s.headers = Marginals {
            default: Some(0),
            ..Marginals::default()
        };
        s.footers = Marginals {
            default: Some(1),
            ..Marginals::default()
        };
    };
    body.push(heading("7. 分栏 Two columns", 1));
    for _ in 0..6 {
        body.push(para(
            vec![text(zh, &l), text(en, &l)],
            ParaProps {
                align: Align::Justify,
                ..props()
            },
        ));
    }
    body.push(end_of(section(columns)));

    // Landscape with a document grid.
    let mut grid_para = vec![heading("8. 文档网格 Landscape with a grid", 1)];
    for _ in 0..2 {
        grid_para.push(para(
            vec![text(zh, &look(10.5))],
            ParaProps {
                align: Align::Justify,
                first_line: 420,
                line: LineSpacing::Multiple(1.0),
                ..ParaProps::default()
            },
        ));
    }
    body.extend(grid_para);

    let last = Section {
        page: (16_838, 11_906),
        grid: Some(DocGrid {
            kind: GridKind::Lines,
            line_pitch: 440,
        }),
        headers: Marginals {
            default: Some(0),
            ..Marginals::default()
        },
        footers: Marginals {
            default: Some(1),
            ..Marginals::default()
        },
        ..Section::default()
    };

    let mut small_look = look(9.0);
    small_look.color = Some([0x55, 0x55, 0x55, 255]);
    let header = Story {
        blocks: vec![para(
            vec![
                text("Aster Office layout sample 排版示例", &small_look),
                Run::object(Inline::Tab, small_look.clone()),
            ],
            ParaProps {
                tabs: vec![TabStop {
                    position: 9020,
                    align: TabAlign::Right,
                    leader: TabLeader::None,
                }],
                borders: [None, None, None, Some(border(0.5, [0x88, 0x88, 0x88, 255]))],
                ..ParaProps::default()
            },
        )],
        part: None,
    };
    let footer_runs = vec![
        text("第 ", &small_look),
        Run {
            text: "1".into(),
            kind: Inline::PageNumber(PageField::Page),
            look: small_look.clone(),
            ..Run::default()
        },
        text(" 页，共 ", &small_look),
        Run {
            text: "1".into(),
            kind: Inline::PageNumber(PageField::PageCount),
            look: small_look.clone(),
            ..Run::default()
        },
        text(" 页  /  Page ", &small_look),
        Run {
            text: "1".into(),
            kind: Inline::PageNumber(PageField::Page),
            look: small_look.clone(),
            ..Run::default()
        },
        text(" of ", &small_look),
        Run {
            text: "1".into(),
            kind: Inline::PageNumber(PageField::PageCount),
            look: small_look.clone(),
            ..Run::default()
        },
    ];
    let footer = Story {
        blocks: vec![para(
            footer_runs,
            ParaProps {
                align: Align::Center,
                ..ParaProps::default()
            },
        )],
        part: None,
    };
    let note = Note {
        id: 1,
        blocks: vec![para(
            vec![
                Run::object(Inline::Footnote(1), {
                    let mut f = look(9.0);
                    f.position = VertAlign::Superscript;
                    f
                }),
                text(
                    " 这是一条脚注 This is a footnote, shown under a separator at the bottom of its page.",
                    &look(9.0),
                ),
            ],
            ParaProps::default(),
        )],
    };
    Document {
        body: Story {
            blocks: body,
            part: None,
        },
        last_section: Arc::new(last),
        marginals: vec![Arc::new(header), Arc::new(footer)],
        footnotes: Notes {
            notes: vec![note],
            part: None,
        },
        settings: Settings {
            east_asian_language: Some("zh-CN".into()),
            compress_punctuation: true,
            ..Settings::default()
        },
        ..Document::default()
    }
}

fn main() {
    let mut args = std::env::args().skip(1);
    let out = args.next().expect("an output folder");
    let scale: f64 = args.next().and_then(|s| s.parse().ok()).unwrap_or(1.5);
    std::fs::create_dir_all(&out).expect("output folder");
    let mut shaper = Shaper::new();
    shaper.set_han_fallbacks(&HAN_FALLBACK_FAMILIES);
    let mut layouter = Layouter::with_shaper(shaper);
    let document = build();
    let started = std::time::Instant::now();
    let layout = layouter.layout(&document);
    println!(
        "{} pages in {:.0} ms",
        layout.pages.len(),
        started.elapsed().as_secs_f64() * 1000.0
    );
    let mut painter = DocPainter::new();
    for (index, page) in layout.pages.iter().enumerate() {
        let png = painter.encode_png(page, scale).expect("png");
        let path = Path::new(&out).join(format!("page-{:02}.png", index + 1));
        std::fs::write(&path, png).expect("write");
        println!("{}", path.display());
    }
    for entry in layout.outline() {
        println!("{} {} (page {})", entry.level, entry.text, entry.page + 1);
    }
}
