//! Tables, sections, headers and footers, notes and drawings of the DOCX
//! reader.

mod common;
#[path = "common/docx_doc.rs"]
mod docx_doc;

use aster_writer::*;
use docx_doc::*;

// ---------------------------------------------------------------------------
// Tables
// ---------------------------------------------------------------------------

fn cell(w: u32, props: &str, text: &str) -> String {
    format!(
        r#"<w:tc><w:tcPr><w:tcW w:w="{w}" w:type="dxa"/>{props}</w:tcPr><w:p><w:r><w:t>{text}</w:t></w:r></w:p></w:tc>"#
    )
}

fn table(props: &str, grid: &[u32], rows: &[String]) -> String {
    let grid: String = grid
        .iter()
        .map(|w| format!(r#"<w:gridCol w:w="{w}"/>"#))
        .collect();
    let grid = if grid.is_empty() {
        String::new()
    } else {
        format!("<w:tblGrid>{grid}</w:tblGrid>")
    };
    format!(
        "<w:tbl><w:tblPr>{props}</w:tblPr>{grid}{}</w:tbl>",
        rows.concat()
    )
}

fn row(props: &str, cells: &[String]) -> String {
    format!("<w:tr><w:trPr>{props}</w:trPr>{}</w:tr>", cells.concat())
}

fn side(name: &str, val: &str, sz: u32, color: &str) -> String {
    format!(r#"<w:{name} w:val="{val}" w:sz="{sz}" w:space="0" w:color="{color}"/>"#)
}

#[test]
fn the_grid_is_the_files_when_it_fits() {
    let d = read(&table(
        r#"<w:jc w:val="right"/><w:tblInd w:w="200" w:type="dxa"/><w:tblLayout w:type="fixed"/>"#,
        &[1000, 2000],
        &[row("", &[cell(1000, "", "a"), cell(2000, "", "b")])],
    ));
    let table = t(&d, 0);
    assert_eq!(table.columns, [1000, 2000]);
    assert_eq!(table.props.align, Align::Right);
    assert_eq!(table.props.indent, 200);
    assert!(table.props.fixed_layout);
    assert_eq!(table.rows[0].cells.len(), 2);
    assert_eq!(table.rows[0].cells[1].span, 1);
}

#[test]
fn a_missing_or_short_grid_is_worked_out_from_the_cell_widths() {
    let rows = [
        row(
            "",
            &[
                cell(1500, "", "a"),
                cell(2500, r#"<w:gridSpan w:val="2"/>"#, "b"),
            ],
        ),
        row(
            "",
            &[
                cell(1500, "", "c"),
                cell(1000, "", "d"),
                cell(1500, "", "e"),
            ],
        ),
    ];
    let none = read(&table("", &[], &rows));
    assert_eq!(t(&none, 0).columns, [1500, 1000, 1500]);
    let short = read(&table("", &[1500], &rows));
    assert_eq!(t(&short, 0).columns, [1500, 1000, 1500]);
    // Cells without widths fall back to a default.
    let blank = read(&table(
        "",
        &[],
        &[row(
            "",
            &[
                "<w:tc><w:p/></w:tc>".to_owned(),
                "<w:tc><w:p/></w:tc>".to_owned(),
            ],
        )],
    ));
    assert_eq!(t(&blank, 0).columns.len(), 2);
    // A grid that is wider than the rows is kept.
    let wide = read(&table(
        "",
        &[1000, 1000, 1000],
        &[row("", &[cell(1000, "", "a")])],
    ));
    assert_eq!(t(&wide, 0).columns.len(), 3);
}

#[test]
fn absurd_spans_and_grid_offsets_are_clamped() {
    let d = read(&table(
        "",
        &[1000],
        &[row(
            r#"<w:gridBefore w:val="4000000000"/><w:gridAfter w:val="99999999999"/>"#,
            &[cell(1000, r#"<w:gridSpan w:val="2000000000"/>"#, "a")],
        )],
    ));
    let table = t(&d, 0);
    assert!(table.rows[0].cells[0].span <= 256);
    assert!(table.rows[0].grid_before <= 4096 && table.rows[0].grid_after <= 4096);
    assert!(table.columns.len() <= 4096 + 256 + 4096);
}

#[test]
fn row_properties() {
    let d = read(&table(
        "",
        &[1000],
        &[
            row(
                r#"<w:cantSplit/><w:tblHeader/><w:trHeight w:val="500" w:hRule="atLeast"/>"#,
                &[cell(1000, "", "a")],
            ),
            row(
                r#"<w:trHeight w:val="300" w:hRule="exact"/>"#,
                &[cell(1000, "", "b")],
            ),
            row(
                r#"<w:trHeight w:val="400"/><w:gridBefore w:val="1"/><w:gridAfter w:val="2"/>"#,
                &[cell(1000, "", "c")],
            ),
            row(r#"<w:tblHeader w:val="0"/>"#, &[cell(1000, "", "d")]),
        ],
    ));
    let rows = &t(&d, 0).rows;
    assert!(rows[0].header && rows[0].cant_split);
    assert_eq!(rows[0].height, RowHeight::AtLeast(500));
    assert_eq!(rows[1].height, RowHeight::Exactly(300));
    assert_eq!(rows[2].height, RowHeight::AtLeast(400));
    assert_eq!((rows[2].grid_before, rows[2].grid_after), (1, 2));
    assert!(!rows[3].header && rows[3].height == RowHeight::Auto);
    assert!(rows[0].source.is_some());
}

#[test]
fn cells_merge_across_and_down() {
    let d = read(&table(
        "",
        &[1000, 1000, 1000],
        &[
            row(
                "",
                &[
                    cell(
                        2000,
                        r#"<w:gridSpan w:val="2"/><w:vMerge w:val="restart"/>"#,
                        "a",
                    ),
                    cell(1000, "", "b"),
                ],
            ),
            row(
                "",
                &[
                    cell(2000, r#"<w:gridSpan w:val="2"/><w:vMerge/>"#, ""),
                    cell(1000, "", "c"),
                ],
            ),
        ],
    ));
    let rows = &t(&d, 0).rows;
    assert_eq!(
        (rows[0].cells[0].span, rows[0].cells[0].merge),
        (2, VMerge::Start)
    );
    assert_eq!(
        (rows[1].cells[0].span, rows[1].cells[0].merge),
        (2, VMerge::Continue)
    );
    assert_eq!(rows[0].cells[1].merge, VMerge::None);
}

#[test]
fn borders_layer_from_the_style_through_the_table_to_the_cell() {
    let styles = format!(
        r#"<w:style w:type="table" w:styleId="S"><w:name w:val="S"/><w:tblPr><w:tblBorders>{}{}{}{}{}{}</w:tblBorders></w:tblPr></w:style>"#,
        side("top", "single", 4, "111111"),
        side("left", "single", 4, "111111"),
        side("bottom", "single", 4, "111111"),
        side("right", "single", 4, "111111"),
        side("insideH", "dashed", 4, "222222"),
        side("insideV", "dotted", 4, "333333"),
    );
    let table_borders = format!(
        "<w:tblBorders>{}{}</w:tblBorders>",
        side("top", "double", 12, "AA0000"),
        side("insideH", "nil", 0, "auto")
    );
    let d = doc(&table(
        &format!(r#"<w:tblStyle w:val="S"/>{table_borders}"#),
        &[1000, 1000],
        &[
            row(
                "",
                &[
                    cell(1000, "", "a"),
                    cell(
                        1000,
                        &format!(
                            "<w:tcBorders>{}</w:tcBorders>",
                            side("right", "single", 24, "00AA00")
                        ),
                        "b",
                    ),
                ],
            ),
            row(
                "",
                &[
                    cell(
                        1000,
                        &format!(
                            "<w:tcBorders>{}</w:tcBorders>",
                            side("left", "nil", 0, "auto")
                        ),
                        "c",
                    ),
                    cell(1000, "", "d"),
                ],
            ),
        ],
    ))
    .styles(&styles)
    .read();
    let rows = &t(&d, 0).rows;
    let b = |r: usize, c: usize, s: usize| rows[r].cells[c].borders[s];
    // Left, top, right, bottom.
    assert_eq!(b(0, 0, 0).unwrap().color, Some([0x11, 0x11, 0x11, 255]));
    // The table's top border replaces the style's on the top row.
    let top = b(0, 0, 1).unwrap();
    assert_eq!(
        (top.style, top.width, top.color),
        (BorderStyle::Double, 1.5, Some([0xAA, 0, 0, 255]))
    );
    // Between cells: the style's vertical lines; the table says no
    // horizontal ones.
    assert_eq!(b(0, 0, 2).unwrap().style, BorderStyle::Dotted);
    assert_eq!(b(0, 0, 3), None);
    assert_eq!(b(1, 0, 1), None);
    assert_eq!(b(1, 1, 0).unwrap().style, BorderStyle::Dotted);
    // The bottom row's lower edge is the style's outer border.
    assert_eq!(b(1, 1, 3).unwrap().style, BorderStyle::Single);
    // The cell's own say wins, and nil removes.
    let own = b(0, 1, 2).unwrap();
    assert_eq!((own.width, own.color), (3.0, Some([0, 0xAA, 0, 255])));
    assert_eq!(b(1, 0, 0), None);
    // The right edge of the table is the style's outer border.
    assert_eq!(b(1, 1, 2).unwrap().style, BorderStyle::Single);
}

#[test]
fn merged_cells_take_the_bottom_edge_from_where_the_merge_ends() {
    let borders = format!(
        "<w:tblBorders>{}{}</w:tblBorders>",
        side("bottom", "single", 16, "000000"),
        side("insideH", "dashed", 4, "808080")
    );
    let d = read(&table(
        &borders,
        &[1000, 1000],
        &[
            row(
                "",
                &[
                    cell(1000, r#"<w:vMerge w:val="restart"/>"#, "a"),
                    cell(1000, "", "b"),
                ],
            ),
            row("", &[cell(1000, "<w:vMerge/>", ""), cell(1000, "", "c")]),
        ],
    ));
    let rows = &t(&d, 0).rows;
    assert_eq!(
        rows[0].cells[0].borders[3].unwrap().style,
        BorderStyle::Single
    );
    assert_eq!(
        rows[0].cells[1].borders[3].unwrap().style,
        BorderStyle::Dashed
    );
}

#[test]
fn shading_and_margins_layer_like_borders() {
    let styles = r#"<w:style w:type="table" w:default="1" w:styleId="TableNormal"><w:name w:val="Normal Table"/><w:tblPr><w:tblCellMar><w:top w:w="10" w:type="dxa"/><w:left w:w="108" w:type="dxa"/><w:bottom w:w="20" w:type="dxa"/><w:right w:w="108" w:type="dxa"/></w:tblCellMar></w:tblPr><w:tcPr><w:shd w:val="clear" w:color="auto" w:fill="EEEEEE"/></w:tcPr></w:style>"#;
    let d = doc(&table(
        r#"<w:tblCellMar><w:left w:w="200" w:type="dxa"/></w:tblCellMar>"#,
        &[1000, 1000],
        &[row("", &[
            cell(1000, "", "a"),
            cell(1000, r#"<w:tcMar><w:top w:w="55" w:type="dxa"/></w:tcMar><w:shd w:val="clear" w:color="auto" w:fill="123456"/>"#, "b"),
        ])],
    ))
    .styles(styles)
    .read();
    let table = t(&d, 0);
    // The table style's default style applies to a table that names none.
    assert_eq!(table.props.cell_margins, [200, 10, 108, 20]);
    let [a, b] = [&table.rows[0].cells[0], &table.rows[0].cells[1]];
    assert_eq!(a.margins, [200, 10, 108, 20]);
    assert_eq!(a.shading, Some([0xEE, 0xEE, 0xEE, 255]));
    assert_eq!(b.margins, [200, 55, 108, 20]);
    assert_eq!(b.shading, Some([0x12, 0x34, 0x56, 255]));
}

#[test]
fn a_cell_has_at_least_one_paragraph_and_may_hold_a_table() {
    let inner = table("", &[500], &[row("", &[cell(500, "", "in")])]);
    let d = read(&table(
        "",
        &[2000],
        &[row(
            "",
            &[
                "<w:tc><w:tcPr><w:tcW w:w=\"2000\" w:type=\"dxa\"/></w:tcPr></w:tc>".to_owned(),
                format!("<w:tc>{inner}<w:p><w:r><w:t>after</w:t></w:r></w:p></w:tc>"),
            ],
        )],
    ));
    let cells = &t(&d, 0).rows[0].cells;
    assert_eq!(cells[0].blocks.len(), 1);
    assert!(cells[0].blocks[0].as_paragraph().is_some());
    assert!(cells[1].blocks[0].as_table().is_some());
    assert_eq!(d.plain_text(), "\nin\nafter");
    // Paths lead into nested tables.
    let nested = Path::body(0).cell_block(0, 1, 0).cell_block(0, 0, 0);
    assert_eq!(d.paragraph(&nested).unwrap().text(), "in");
}

#[test]
fn rows_and_cells_in_content_controls_are_found() {
    let d = read(&format!(
        "<w:tbl><w:tblGrid><w:gridCol w:w=\"1000\"/></w:tblGrid><w:sdt><w:sdtContent><w:tr><w:sdt><w:sdtContent>{}</w:sdtContent></w:sdt></w:tr></w:sdtContent></w:sdt></w:tbl>",
        cell(1000, "", "deep")
    ));
    assert_eq!(d.plain_text(), "deep");
}

#[test]
fn vertical_alignment_of_cells() {
    let d = read(&table(
        "",
        &[1000, 1000, 1000],
        &[row(
            "",
            &[
                cell(1000, r#"<w:vAlign w:val="center"/>"#, "a"),
                cell(1000, r#"<w:vAlign w:val="bottom"/>"#, "b"),
                cell(1000, "", "c"),
            ],
        )],
    ));
    let cells = &t(&d, 0).rows[0].cells;
    assert_eq!(
        [cells[0].align, cells[1].align, cells[2].align],
        [CellAlign::Center, CellAlign::Bottom, CellAlign::Top]
    );
}

// ---------------------------------------------------------------------------
// Sections
// ---------------------------------------------------------------------------

fn last_section(sect: &str) -> std::sync::Arc<Section> {
    read(&format!("<w:p/><w:sectPr>{sect}</w:sectPr>")).last_section
}

#[test]
fn page_size_margins_and_their_defaults() {
    let s = last_section("");
    assert_eq!(s.page, (12_240, 15_840));
    assert_eq!(s.margins, Margins::default());
    let s = last_section(
        r#"<w:pgSz w:w="11906" w:h="16838" w:orient="portrait"/><w:pgMar w:top="-1000" w:right="1100" w:bottom="-900" w:left="1200" w:header="500" w:footer="600" w:gutter="100"/>"#,
    );
    assert_eq!(s.page, (11_906, 16_838));
    assert_eq!(
        s.margins,
        Margins {
            top: 1000,
            right: 1100,
            bottom: 900,
            left: 1200,
            header: 500,
            footer: 600,
            gutter: 100
        }
    );
    // A page size that makes no sense is the default.
    assert_eq!(
        last_section(r#"<w:pgSz w:w="0" w:h="-4"/>"#).page,
        (12_240, 15_840)
    );
    assert!(s.source.is_some());
}

#[test]
fn columns_equal_and_unequal() {
    let s = last_section(r#"<w:cols w:num="3" w:space="360" w:sep="1"/>"#);
    assert_eq!(
        (s.columns.count, s.columns.space, s.columns.separator),
        (3, 360, true)
    );
    assert!(s.columns.widths.is_empty());
    let s = last_section(
        r#"<w:cols w:num="2" w:equalWidth="0"><w:col w:w="3000" w:space="200"/><w:col w:w="5000"/></w:cols>"#,
    );
    assert_eq!(s.columns.count, 2);
    assert_eq!(s.columns.widths, [(3000, 200), (5000, 0)]);
    assert_eq!(last_section(r#"<w:cols w:num="0"/>"#).columns.count, 1);
    assert_eq!(
        last_section(r#"<w:cols w:num="4000000"/>"#).columns.count,
        45
    );
}

#[test]
fn section_type_page_numbers_grid_alignment_and_direction() {
    use SectionStart::*;
    let start = |v: &str| last_section(&format!(r#"<w:type w:val="{v}"/>"#)).start;
    assert_eq!(
        [
            "continuous",
            "nextPage",
            "evenPage",
            "oddPage",
            "nextColumn"
        ]
        .map(start),
        [Continuous, NextPage, EvenPage, OddPage, NextColumn]
    );
    assert_eq!(last_section("").start, NextPage);
    let s = last_section(
        r#"<w:titlePg/><w:pgNumType w:start="5" w:fmt="upperRoman"/><w:vAlign w:val="center"/><w:textDirection w:val="tbRl"/>"#,
    );
    assert!(s.title_page && s.vertical_text);
    assert_eq!(s.page_number_start, Some(5));
    assert_eq!(s.page_number_format, NumberFormat::UpperRoman);
    assert_eq!(s.vertical_align, CellAlign::Center);
    assert!(!last_section(r#"<w:textDirection w:val="lrTb"/>"#).vertical_text);
    let grid =
        |ty: &str| last_section(&format!(r#"<w:docGrid w:type="{ty}" w:linePitch="360"/>"#)).grid;
    assert_eq!(
        grid("lines"),
        Some(DocGrid {
            kind: GridKind::Lines,
            line_pitch: 360
        })
    );
    assert_eq!(grid("linesAndChars").unwrap().kind, GridKind::LinesAndChars);
    assert_eq!(grid("snapToChars").unwrap().kind, GridKind::SnapToChars);
    assert_eq!(grid("default"), None);
    assert_eq!(last_section("").grid, None);
}

#[test]
fn paragraphs_carry_the_sections_they_end() {
    let d = read(
        r#"<w:p><w:r><w:t>one</w:t></w:r></w:p><w:p><w:pPr><w:sectPr><w:pgSz w:w="10000" w:h="12000"/></w:sectPr></w:pPr><w:r><w:t>two</w:t></w:r></w:p><w:p><w:r><w:t>three</w:t></w:r></w:p><w:sectPr><w:pgSz w:w="20000" w:h="10000"/></w:sectPr>"#,
    );
    let sections = d.sections();
    assert_eq!(sections.len(), 2);
    assert_eq!(sections[0].0, 1);
    assert_eq!(sections[0].1.page, (10_000, 12_000));
    assert_eq!(d.last_section.page, (20_000, 10_000));
    assert!(p(&d, 1).is_as_read());
}

#[test]
fn headers_and_footers_are_stories_and_sections_inherit_them() {
    let d = doc(
        r#"<w:p><w:pPr><w:sectPr><w:headerReference w:type="default" r:id="h1"/><w:headerReference w:type="first" r:id="h2"/><w:footerReference w:type="default" r:id="f1"/><w:titlePg/></w:sectPr></w:pPr></w:p>
<w:p><w:pPr><w:sectPr><w:headerReference w:type="default" r:id="h3"/></w:sectPr></w:pPr></w:p>
<w:p><w:r><w:t>last</w:t></w:r></w:p><w:sectPr><w:footerReference w:type="even" r:id="f2"/></w:sectPr>"#,
    )
    .marginal("h1", "header", "header1.xml", r#"<w:p><w:r><w:t>H1</w:t></w:r></w:p>"#)
    .marginal("h2", "header", "header2.xml", r#"<w:p><w:r><w:t>H2</w:t></w:r></w:p>"#)
    .marginal("h3", "header", "header3.xml", r#"<w:p><w:r><w:t>H3</w:t></w:r></w:p>"#)
    .marginal("f1", "footer", "footer1.xml", r#"<w:p><w:r><w:t>F1</w:t></w:r></w:p><w:p><w:r><w:t>F1b</w:t></w:r></w:p>"#)
    .marginal("f2", "footer", "footer2.xml", r#"<w:p><w:r><w:t>F2</w:t></w:r></w:p>"#)
    .read();
    assert_eq!(d.marginals.len(), 5);
    let text = |i: usize| -> String {
        d.marginals[i]
            .blocks
            .iter()
            .filter_map(Block::as_paragraph)
            .map(Paragraph::text)
            .collect::<Vec<_>>()
            .join("|")
    };
    let sections = d.sections();
    let (s0, s1, s2) = (sections[0].1, sections[1].1, &d.last_section);
    assert_eq!(text(s0.headers.default.unwrap()), "H1");
    assert_eq!(text(s0.headers.first.unwrap()), "H2");
    assert_eq!(text(s0.footers.default.unwrap()), "F1|F1b");
    assert!(s0.title_page && s0.headers.even.is_none());
    // The second section names only a new default header and keeps the rest.
    assert_eq!(text(s1.headers.default.unwrap()), "H3");
    assert_eq!(s1.headers.first, s0.headers.first);
    assert_eq!(s1.footers, s0.footers);
    assert!(!s1.title_page);
    // The last adds an even footer.
    assert_eq!(s2.headers, s1.headers);
    assert_eq!(text(s2.footers.even.unwrap()), "F2");
    assert_eq!(s2.footers.default, s0.footers.default);
    assert_eq!(d.marginals[0].part.as_deref(), Some("word/header1.xml"));
    // The same part is one story, however many sections name it.
    let again = doc(
        r#"<w:p><w:pPr><w:sectPr><w:headerReference w:type="default" r:id="h1"/></w:sectPr></w:pPr></w:p><w:sectPr><w:headerReference w:type="default" r:id="h1"/><w:footerReference w:type="default" r:id="gone"/></w:sectPr>"#,
    )
    .marginal("h1", "header", "header1.xml", "<w:p/>")
    .rel("gone", "footer", "missing.xml")
    .read();
    assert_eq!(again.marginals.len(), 1);
    assert_eq!(again.last_section.footers.default, None);
}

// ---------------------------------------------------------------------------
// Notes
// ---------------------------------------------------------------------------

#[test]
fn notes_skip_their_separators_and_refer_to_themselves() {
    let d = doc(
        r#"<w:p><w:r><w:footnoteReference w:id="3"/></w:r><w:r><w:endnoteReference w:id="1"/></w:r></w:p>"#,
    )
    .footnotes(
        r#"<w:footnote w:type="separator" w:id="-1"><w:p><w:r><w:separator/></w:r></w:p></w:footnote><w:footnote w:type="continuationSeparator" w:id="0"><w:p><w:r><w:continuationSeparator/></w:r></w:p></w:footnote><w:footnote w:id="3"><w:p><w:r><w:footnoteRef/></w:r><w:r><w:t xml:space="preserve"> three</w:t></w:r></w:p></w:footnote>"#,
    )
    .endnotes(
        r#"<w:endnote w:type="separator" w:id="-1"><w:p><w:r><w:separator/></w:r></w:p></w:endnote><w:endnote w:id="1"><w:p><w:r><w:endnoteRef/></w:r><w:r><w:t>one</w:t></w:r></w:p></w:endnote>"#,
    )
    .read();
    assert_eq!(p(&d, 0).runs[0].kind, Inline::Footnote(3));
    assert_eq!(p(&d, 0).runs[1].kind, Inline::Endnote(1));
    assert_eq!(d.footnotes.notes.len(), 1);
    assert_eq!(d.footnotes.notes[0].id, 3);
    assert_eq!(d.footnotes.part.as_deref(), Some("word/footnotes.xml"));
    let own = d.footnotes.notes[0].blocks[0].as_paragraph().unwrap();
    assert_eq!(own.runs[0].kind, Inline::Footnote(3));
    assert_eq!(own.runs[1].text, " three");
    assert_eq!(
        d.endnotes.notes[0].blocks[0].as_paragraph().unwrap().runs[0].kind,
        Inline::Endnote(1)
    );
}

// ---------------------------------------------------------------------------
// Drawings
// ---------------------------------------------------------------------------

fn png() -> Vec<u8> {
    vec![0x89, b'P', b'N', b'G', 1, 2, 3]
}

fn with_image(body: &str) -> Document {
    let mut d = doc(body).rel("rIdImg", "image", "media/pic.png").rel(
        "rIdImg2",
        "image",
        "media/other.png",
    );
    d.binary.push(("word/media/pic.png".to_owned(), png()));
    d.binary
        .push(("word/media/other.png".to_owned(), vec![9, 9]));
    d.read()
}

fn drawing(d: &Document) -> std::sync::Arc<Drawing> {
    match &p(d, 0).runs[0].kind {
        Inline::Drawing(d) => d.clone(),
        other => panic!("expected a drawing, found {other:?}"),
    }
}

fn picture_xml(host: &str, embed: &str, src: &str) -> String {
    format!(
        r#"<w:p><w:r><w:drawing>{host}<a:graphic><a:graphicData uri="http://schemas.openxmlformats.org/drawingml/2006/picture"><pic:pic><pic:blipFill><a:blip {embed}/>{src}</pic:blipFill></pic:pic></a:graphicData></a:graphic>{}</w:drawing></w:r></w:p>"#,
        if host.contains("<wp:anchor") {
            "</wp:anchor>"
        } else {
            "</wp:inline>"
        }
    )
}

#[test]
fn an_inline_picture_has_its_bytes_size_alt_and_crop() {
    let xml = picture_xml(
        r#"<wp:inline distT="0"><wp:extent cx="914400" cy="457200"/><wp:docPr id="1" name="n" descr="A cat"/>"#,
        r#"r:embed="rIdImg""#,
        r#"<a:srcRect l="10000" t="20000" r="5000" b="0"/>"#,
    );
    let d = with_image(&xml);
    let drawing = drawing(&d);
    assert_eq!(drawing.size, (914_400, 457_200));
    assert_eq!(drawing.alt.as_deref(), Some("A cat"));
    assert_eq!(drawing.float, None);
    let DrawingContent::Picture(picture) = &drawing.content else {
        panic!("a picture")
    };
    assert_eq!(&*picture.image, png().as_slice());
    assert_eq!(picture.crop, [0.1, 0.2, 0.05, 0.0]);
}

#[test]
fn pictures_of_the_same_part_share_their_bytes_and_broken_ones_are_placeholders() {
    let one = |embed: &str| picture_xml(r#"<wp:inline><wp:extent cx="1" cy="1"/>"#, embed, "");
    let d = with_image(&format!(
        "{}{}{}{}",
        one(r#"r:embed="rIdImg""#),
        one(r#"r:embed="rIdImg""#),
        one(r#"r:link="rIdImg2""#),
        one(r#"r:embed="rIdNone""#)
    ));
    let image = |i: usize| match &drawing_at(&d, i).content {
        DrawingContent::Picture(p) => Some(p.image.clone()),
        _ => None,
    };
    assert!(std::sync::Arc::ptr_eq(
        &image(0).unwrap(),
        &image(1).unwrap()
    ));
    assert_eq!(
        drawing_at(&d, 2).content,
        DrawingContent::Other("picture".to_owned())
    );
    assert_eq!(
        drawing_at(&d, 3).content,
        DrawingContent::Other("picture".to_owned())
    );
}

fn drawing_at(d: &Document, i: usize) -> std::sync::Arc<Drawing> {
    match &p(d, i).runs[0].kind {
        Inline::Drawing(d) => d.clone(),
        other => panic!("expected a drawing, found {other:?}"),
    }
}

fn anchor(h: &str, v: &str, wrap: &str, attrs: &str) -> Document {
    let host = format!(
        r#"<wp:anchor {attrs}><wp:simplePos x="0" y="0"/>{h}{v}<wp:extent cx="500" cy="600"/>{wrap}<wp:docPr id="1" name="x"/>"#
    );
    let xml = picture_xml(&host, r#"r:embed="rIdImg""#, "");
    with_image(&xml)
}

#[test]
fn anchors_have_positions_wraps_and_distances() {
    let d = anchor(
        r#"<wp:positionH relativeFrom="margin"><wp:align>center</wp:align></wp:positionH>"#,
        r#"<wp:positionV relativeFrom="paragraph"><wp:posOffset>-12345</wp:posOffset></wp:positionV>"#,
        r#"<wp:wrapSquare wrapText="bothSides"/>"#,
        r#"distT="1" distB="2" distL="3" distR="4" behindDoc="1""#,
    );
    let float = drawing(&d).float.unwrap();
    assert_eq!(
        float.x,
        FloatAxis {
            from: FloatFrom::Margin,
            at: FloatAt::Center
        }
    );
    assert_eq!(
        float.y,
        FloatAxis {
            from: FloatFrom::Paragraph,
            at: FloatAt::Offset(-12_345)
        }
    );
    assert_eq!(float.wrap, Wrap::Square);
    assert!(float.behind);
    // Left, top, right, bottom.
    assert_eq!(float.distance, [3, 1, 4, 2]);
}

#[test]
fn relative_positions_and_alignments_are_mapped() {
    use FloatFrom::*;
    let from = |axis: &str, name: &str| {
        let (h, v) = if axis == "H" {
            (
                format!(
                    r#"<wp:positionH relativeFrom="{name}"><wp:posOffset>7</wp:posOffset></wp:positionH>"#
                ),
                String::new(),
            )
        } else {
            (
                String::new(),
                format!(
                    r#"<wp:positionV relativeFrom="{name}"><wp:posOffset>7</wp:posOffset></wp:positionV>"#
                ),
            )
        };
        let d = anchor(&h, &v, "<wp:wrapNone/>", "");
        let f = drawing(&d).float.unwrap();
        if axis == "H" { f.x.from } else { f.y.from }
    };
    let h: Vec<_> = [
        "page",
        "margin",
        "column",
        "character",
        "leftMargin",
        "rightMargin",
        "insideMargin",
        "outsideMargin",
    ]
    .iter()
    .map(|n| from("H", n))
    .collect();
    assert_eq!(
        h,
        [Page, Margin, Column, Text, Page, Margin, Margin, Margin]
    );
    let v: Vec<_> = [
        "page",
        "margin",
        "paragraph",
        "line",
        "topMargin",
        "bottomMargin",
        "insideMargin",
    ]
    .iter()
    .map(|n| from("V", n))
    .collect();
    assert_eq!(v, [Page, Margin, Paragraph, Text, Page, Margin, Margin]);
    let at = |word: &str| {
        let d = anchor(
            &format!(
                r#"<wp:positionH relativeFrom="page"><wp:align>{word}</wp:align></wp:positionH>"#
            ),
            "",
            "<wp:wrapNone/>",
            "",
        );
        drawing(&d).float.unwrap().x.at
    };
    assert_eq!(
        ["left", "center", "right", "inside", "outside"].map(at),
        [
            FloatAt::Start,
            FloatAt::Center,
            FloatAt::End,
            FloatAt::Start,
            FloatAt::End
        ]
    );
}

#[test]
fn every_wrap_kind_is_mapped() {
    let wrap = |xml: &str| drawing(&anchor("", "", xml, "")).float.unwrap().wrap;
    assert_eq!(
        [
            wrap("<wp:wrapNone/>"),
            wrap("<wp:wrapSquare/>"),
            wrap("<wp:wrapTight/>"),
            wrap("<wp:wrapThrough/>"),
            wrap("<wp:wrapTopAndBottom/>"),
        ],
        [
            Wrap::None,
            Wrap::Square,
            Wrap::Tight,
            Wrap::Tight,
            Wrap::TopAndBottom
        ]
    );
}

fn shape_xml(inner: &str) -> String {
    format!(
        r#"<w:p><w:r><w:drawing><wp:inline><wp:extent cx="2000" cy="1000"/><a:graphic><a:graphicData uri="x">{inner}</a:graphicData></a:graphic></wp:inline></w:drawing></w:r></w:p>"#
    )
}

#[test]
fn text_boxes_have_blocks_fill_line_and_insets() {
    let d = read(&shape_xml(
        r#"<wps:wsp><wps:spPr><a:solidFill><a:srgbClr val="00FF00"/></a:solidFill><a:ln w="25400"><a:solidFill><a:schemeClr val="accent2"/></a:solidFill></a:ln></wps:spPr><wps:txbx><w:txbxContent><w:p><w:r><w:t>inside</w:t></w:r></w:p><w:tbl><w:tblGrid><w:gridCol w:w="500"/></w:tblGrid><w:tr><w:tc><w:p><w:r><w:t>cell</w:t></w:r></w:p></w:tc></w:tr></w:tbl></w:txbxContent></wps:txbx><wps:bodyPr lIns="10" tIns="20" rIns="30" bIns="40"/></wps:wsp>"#,
    ));
    let DrawingContent::TextBox(boxed) = &drawing(&d).content else {
        panic!("a text box")
    };
    assert_eq!(boxed.blocks.len(), 2);
    assert_eq!(boxed.blocks[0].as_paragraph().unwrap().text(), "inside");
    assert!(boxed.blocks[1].as_table().is_some());
    assert_eq!(boxed.fill, Some([0, 255, 0, 255]));
    let line = boxed.line.unwrap();
    assert_eq!(line.width, 2.0);
    assert_eq!(line.color, Some([0xED, 0x7D, 0x31, 255]));
    assert_eq!(boxed.insets, [10, 20, 30, 40]);
    // Its paragraphs are not in the document's own order.
    assert_eq!(d.plain_text(), "\u{FFFC}");
}

#[test]
fn text_boxes_without_fill_or_line_have_none() {
    let d = read(&shape_xml(
        r#"<wps:wsp><wps:spPr><a:noFill/><a:ln><a:noFill/></a:ln></wps:spPr><wps:txbx><w:txbxContent><w:p/></w:txbxContent></wps:txbx></wps:wsp>"#,
    ));
    let DrawingContent::TextBox(boxed) = &drawing(&d).content else {
        panic!("a text box")
    };
    assert_eq!((boxed.fill, boxed.line), (None, None));
    assert_eq!(boxed.insets, [91_440, 45_720, 91_440, 45_720]);
}

#[test]
fn other_graphics_are_named_placeholders() {
    let content = |inner: &str| drawing(&read(&shape_xml(inner))).content.clone();
    let other = |s: &str| DrawingContent::Other(s.to_owned());
    assert_eq!(
        content(
            r#"<c:chart xmlns:c="http://schemas.openxmlformats.org/drawingml/2006/chart" r:id="rId9"/>"#
        ),
        other("chart")
    );
    assert_eq!(
        content(r#"<dgm:relIds xmlns:dgm="d" r:dm="a" r:lo="b"/>"#),
        other("diagram")
    );
    assert_eq!(content(r#"<wps:wsp><wps:spPr/></wps:wsp>"#), other("shape"));
    assert_eq!(
        content(r#"<wpg:wgp xmlns:wpg="g"><wps:wsp/></wpg:wgp>"#),
        other("group")
    );
}

#[test]
fn a_text_box_in_alternate_content_is_read_from_the_choice() {
    let d = read(
        r#"<w:p><w:r><mc:AlternateContent><mc:Choice Requires="wps"><w:drawing><wp:anchor><wp:positionH relativeFrom="column"><wp:posOffset>1</wp:posOffset></wp:positionH><wp:positionV relativeFrom="paragraph"><wp:posOffset>2</wp:posOffset></wp:positionV><wp:extent cx="300" cy="400"/><wp:wrapSquare/><a:graphic><a:graphicData uri="x"><wps:wsp><wps:txbx><w:txbxContent><w:p><w:r><w:t>modern</w:t></w:r></w:p></w:txbxContent></wps:txbx></wps:wsp></a:graphicData></a:graphic></wp:anchor></w:drawing></mc:Choice><mc:Fallback><w:pict><v:shape style="width:10pt;height:10pt"><v:textbox><w:txbxContent><w:p><w:r><w:t>legacy</w:t></w:r></w:p></w:txbxContent></v:textbox></v:shape></w:pict></mc:Fallback></mc:AlternateContent></w:r></w:p>"#,
    );
    let DrawingContent::TextBox(boxed) = &drawing(&d).content else {
        panic!("a text box")
    };
    assert_eq!(boxed.blocks[0].as_paragraph().unwrap().text(), "modern");
    assert_eq!(drawing(&d).size, (300, 400));
}

#[test]
fn vml_objects_and_text_boxes() {
    let d = read(
        r##"<w:p><w:r><w:pict><v:shape id="s" style="position:absolute;margin-left:10pt;margin-top:20pt;width:100pt;height:50pt;z-index:-1;mso-position-horizontal-relative:page;mso-position-vertical-relative:margin" fillcolor="#ffff00" stroked="f"><v:textbox><w:txbxContent><w:p><w:r><w:t>vml text</w:t></w:r></w:p></w:txbxContent></v:textbox><w10:wrap type="square"/></v:shape></w:pict></w:r><w:r><w:object w:dxaOrig="1440" w:dyaOrig="720"><v:shape style="width:72pt;height:36pt"><v:imagedata r:id="rIdNone"/></v:shape><o:OLEObject Type="Embed"/></w:object></w:r><w:r><w:pict><v:rect style="width:1in;height:0.5in"/></w:pict></w:r></w:p>"##,
    );
    let first = drawing_at(&d, 0);
    assert_eq!(first.size, (1_270_000, 635_000));
    let DrawingContent::TextBox(boxed) = &first.content else {
        panic!("a text box")
    };
    assert_eq!(boxed.blocks[0].as_paragraph().unwrap().text(), "vml text");
    assert_eq!(boxed.fill, Some([255, 255, 0, 255]));
    assert_eq!(boxed.line, None);
    let float = first.float.unwrap();
    assert_eq!(
        float.x,
        FloatAxis {
            from: FloatFrom::Page,
            at: FloatAt::Offset(127_000)
        }
    );
    assert_eq!(
        float.y,
        FloatAxis {
            from: FloatFrom::Margin,
            at: FloatAt::Offset(254_000)
        }
    );
    assert_eq!(float.wrap, Wrap::Square);
    assert!(float.behind);
    let object = match &p(&d, 0).runs[1].kind {
        Inline::Drawing(d) => d.clone(),
        _ => panic!("an object"),
    };
    assert_eq!(object.content, DrawingContent::Other("object".to_owned()));
    assert_eq!(object.size, (914_400, 457_200));
    let rect = match &p(&d, 0).runs[2].kind {
        Inline::Drawing(d) => d.clone(),
        _ => panic!("a shape"),
    };
    assert_eq!(rect.content, DrawingContent::Other("object".to_owned()));
    assert_eq!(rect.size, (914_400, 457_200));
    assert_eq!(rect.float, None);
}

#[test]
fn a_vml_picture_with_image_data_is_a_picture() {
    let mut d = doc(r#"<w:p><w:r><w:pict><v:shape style="width:10pt;height:5pt"><v:imagedata r:id="rIdImg"/></v:shape></w:pict></w:r></w:p>"#)
        .rel("rIdImg", "image", "media/pic.png");
    d.binary.push(("word/media/pic.png".to_owned(), png()));
    let d = d.read();
    assert!(matches!(drawing(&d).content, DrawingContent::Picture(_)));
}
