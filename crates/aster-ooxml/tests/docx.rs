mod common;
#[path = "common/docx_doc.rs"]
mod docx_doc;
#[path = "common/docx_sample.rs"]
mod docx_sample;

use std::sync::Arc;

use aster_ooxml::{Docx, ErrorKind, FormatCodec};
use aster_writer::*;
use common::{CONTENT_TYPES_HEAD, package, root_rels};
use docx_doc::*;

// ---------------------------------------------------------------------------
// The package
// ---------------------------------------------------------------------------

#[test]
fn the_main_part_is_found_through_the_relationship() {
    let mut d = doc(r#"<w:p><w:r><w:t>hello</w:t></w:r></w:p>"#);
    d.main = "word/custom/body.xml".to_owned();
    let document = d.read();
    assert_eq!(document.plain_text(), "hello");
    assert_eq!(document.body.part.as_deref(), Some("word/custom/body.xml"));
}

#[test]
fn the_codec_names_its_extensions_and_refuses_other_kinds() {
    assert_eq!(Docx::EXTENSIONS, &["docx", "docm"]);
    let presentation = package(&[
        (
            "[Content_Types].xml",
            format!(
                r#"{CONTENT_TYPES_HEAD}<Override PartName="/ppt/presentation.xml" ContentType="application/vnd.openxmlformats-officedocument.presentationml.presentation.main+xml"/></Types>"#
            )
            .as_bytes(),
        ),
        ("_rels/.rels", root_rels("ppt/presentation.xml").as_bytes()),
        ("ppt/presentation.xml", b"<p:presentation/>"),
    ]);
    let err = Docx.read(presentation).unwrap_err();
    assert_eq!(err.kind(), ErrorKind::WrongKind);
    assert_eq!(Docx.read(b"junk".to_vec()).unwrap_err().kind(), ErrorKind::NotZip);
}

#[test]
fn a_package_without_optional_parts_still_reads() {
    let document = read(r#"<w:p><w:r><w:t>plain</w:t></w:r></w:p>"#);
    assert_eq!(document.plain_text(), "plain");
    let paragraph = p(&document, 0);
    // Word's own defaults: ten points, one Normal style, Letter pages.
    assert_eq!(paragraph.base.size, 10.0);
    assert_eq!(document.styles.default_style.as_deref(), Some("Normal"));
    assert_eq!(document.last_section.page, (12_240, 15_840));
    assert_eq!(document.settings.default_tab, 720);
    assert!(document.marginals.is_empty());
}

#[test]
fn a_part_that_cannot_be_parsed_is_left_to_the_defaults() {
    let d = doc(r#"<w:p><w:r><w:t>x</w:t></w:r></w:p>"#)
        .styles("<w:style")
        .numbering("<<<");
    assert_eq!(d.read().plain_text(), "x");
}

// ---------------------------------------------------------------------------
// Blocks and runs
// ---------------------------------------------------------------------------

#[test]
fn blocks_come_through_content_controls_and_the_like() {
    let document = read(
        r#"<w:sdt><w:sdtPr/><w:sdtContent><w:p><w:r><w:t>a</w:t></w:r></w:p><w:sdt><w:sdtContent><w:p><w:r><w:t>b</w:t></w:r></w:p></w:sdtContent></w:sdt></w:sdtContent></w:sdt>
<w:customXml w:element="x"><w:p><w:r><w:t>c</w:t></w:r></w:p></w:customXml>
<w:smartTag w:uri="u" w:element="e"><w:p><w:r><w:t>d</w:t></w:r></w:p></w:smartTag>
<w:ins w:id="1" w:author="x"><w:p><w:r><w:t>e</w:t></w:r></w:p></w:ins>
<w:moveTo w:id="2" w:author="x"><w:p><w:r><w:t>f</w:t></w:r></w:p></w:moveTo>
<w:altChunk r:id="rId9"/>
<w:p><w:r><w:t>g</w:t></w:r></w:p>"#,
    );
    assert_eq!(document.plain_text(), "a\nb\nc\nd\ne\nf\ng");
}

#[test]
fn alternate_content_picks_a_choice_we_understand_or_the_fallback() {
    let document = read(
        r#"<mc:AlternateContent><mc:Choice Requires="wps"><w:p><w:r><w:t>choice</w:t></w:r></w:p></mc:Choice><mc:Fallback><w:p><w:r><w:t>fallback</w:t></w:r></w:p></mc:Fallback></mc:AlternateContent>
<mc:AlternateContent><mc:Choice Requires="w99"><w:p><w:r><w:t>new</w:t></w:r></w:p></mc:Choice><mc:Fallback><w:p><w:r><w:t>old</w:t></w:r></w:p></mc:Fallback></mc:AlternateContent>"#,
    );
    assert_eq!(document.plain_text(), "choice\nold");
}

#[test]
fn run_children_become_text_tabs_breaks_and_hyphens() {
    let document = read(
        r#"<w:p><w:r><w:t>a</w:t></w:r><w:r><w:t xml:space="preserve"> b </w:t></w:r><w:r><w:t> c </w:t></w:r><w:r><w:tab/></w:r><w:r><w:ptab w:relativeTo="margin" w:alignment="right" w:leader="none"/></w:r><w:r><w:br/></w:r><w:r><w:br w:type="page"/></w:r><w:r><w:br w:type="column"/></w:r><w:r><w:cr/></w:r><w:r><w:noBreakHyphen/></w:r><w:r><w:softHyphen/></w:r><w:r><w:lastRenderedPageBreak/><w:t>z</w:t></w:r></w:p>"#,
    );
    let paragraph = p(&document, 0);
    assert_eq!(
        kinds(paragraph),
        ["text", "text", "text", "tab", "tab", "line", "page", "column", "line", "text", "text", "text"]
    );
    assert_eq!(
        run_texts(paragraph),
        ["a", " b ", "c", "\t", "\t", "\n", OBJECT, OBJECT, "\n", "\u{2011}", "\u{ad}", "z"]
    );
    assert_eq!(paragraph.text(), "a b c\t\t\n\u{FFFC}\u{FFFC}\n\u{2011}\u{ad}z");
}

#[test]
fn symbols_become_unicode_and_lose_the_symbol_font() {
    let document = read(
        r#"<w:p><w:r><w:sym w:font="Symbol" w:char="F0B7"/></w:r><w:r><w:sym w:font="Wingdings" w:char="F0FC"/></w:r><w:r><w:sym w:font="Webdings" w:char="F061"/></w:r></w:p>"#,
    );
    let paragraph = p(&document, 0);
    assert_eq!(run_texts(paragraph), ["•", "✓", "\u{F061}"]);
    assert_eq!(paragraph.runs[0].look.font, None);
    assert_eq!(paragraph.runs[1].look.font, None);
    // A symbol we do not know keeps its font, so it can still be drawn.
    assert_eq!(paragraph.runs[2].look.font.as_deref(), Some("Webdings"));
}

#[test]
fn a_run_of_one_thing_is_one_run_and_several_things_are_several_runs() {
    let d = doc(
        r#"<w:p><w:r><w:rPr><w:b/></w:rPr><w:t>one</w:t></w:r><w:r><w:rPr><w:i/></w:rPr><w:t>a</w:t><w:tab/><w:t>b</w:t></w:r><w:r><w:rPr><w:u w:val="single"/></w:rPr></w:r></w:p>"#,
    );
    let bytes = d.bytes();
    let document = d.read();
    let paragraph = p(&document, 0);
    assert_eq!(kinds(paragraph), ["text", "text", "tab", "text", "marker"]);
    // The part as the package returns it.
    let part = common_part(&bytes, "word/document.xml");
    let source = |i: usize| paragraph.runs[i].source.clone().unwrap();
    assert!(raw(&part, &source(0).range).starts_with("<w:r>"));
    assert!(raw(&part, &source(0).range).ends_with("</w:r>"));
    assert_eq!(source(0).parent, None);
    assert_eq!(raw(&part, &source(1).range), "<w:t>a</w:t>");
    assert_eq!(raw(&part, &source(2).range), "<w:tab/>");
    assert_eq!(raw(&part, &source(3).range), "<w:t>b</w:t>");
    for i in 1..=3 {
        let parent = source(i).parent.clone().unwrap();
        assert!(raw(&part, &parent).starts_with("<w:r>"));
        assert!(raw(&part, &parent).contains("<w:i/>"));
    }
    // An empty run is a marker for the whole `<w:r>`.
    assert!(raw(&part, &source(4).range).contains(r#"w:val="single""#));
    // The three share their look.
    assert!(paragraph.runs[1].look.italic && paragraph.runs[3].look.italic);
    assert!(paragraph.runs[1].same_look(&paragraph.runs[3]));
}


#[test]
fn hyperlinks_give_their_runs_a_link() {
    let document = doc(
        r#"<w:p><w:hyperlink r:id="rId77" w:tooltip="Go"><w:r><w:t>web</w:t></w:r></w:hyperlink><w:hyperlink w:anchor="top"><w:r><w:t>local</w:t></w:r></w:hyperlink><w:hyperlink r:id="rId77" w:anchor="part"><w:r><w:t>both</w:t></w:r></w:hyperlink><w:r><w:t>plain</w:t></w:r></w:p>"#,
    )
    .rel("rId77", "hyperlink", "https://example.org/a?x=1&amp;y=2")
    .read();
    let paragraph = p(&document, 0);
    let target = |i: usize| paragraph.runs[i].link.as_ref().map(|l| l.target.clone());
    assert_eq!(target(0).as_deref(), Some("https://example.org/a?x=1&y=2"));
    assert_eq!(
        paragraph.runs[0].link.as_ref().unwrap().tooltip.as_deref(),
        Some("Go")
    );
    assert_eq!(target(1).as_deref(), Some("#top"));
    assert_eq!(target(2).as_deref(), Some("https://example.org/a?x=1&y=2#part"));
    assert_eq!(target(3), None);
}

#[test]
fn page_fields_become_page_numbers() {
    let document = read(
        r#"<w:p><w:fldSimple w:instr=" PAGE \* MERGEFORMAT "><w:r><w:t>4</w:t></w:r><w:r><w:t>5</w:t></w:r></w:fldSimple><w:fldSimple w:instr="NUMPAGES"><w:r><w:t>9</w:t></w:r></w:fldSimple><w:fldSimple w:instr="SECTIONPAGES"><w:r><w:t>2</w:t></w:r></w:fldSimple><w:fldSimple w:instr="DATE"><w:r><w:t>today</w:t></w:r></w:fldSimple></w:p>"#,
    );
    let paragraph = p(&document, 0);
    assert_eq!(kinds(paragraph), ["pagenumber", "marker", "pagenumber", "pagenumber", "text"]);
    assert_eq!(paragraph.runs[0].kind, Inline::PageNumber(PageField::Page));
    assert_eq!(paragraph.runs[2].kind, Inline::PageNumber(PageField::PageCount));
    assert_eq!(paragraph.runs[3].kind, Inline::PageNumber(PageField::SectionPages));
    assert_eq!(paragraph.runs[0].text, "4");
    assert_eq!(paragraph.runs[4].text, "today");
}

fn field(code: &str, result: &str) -> String {
    format!(
        r#"<w:r><w:fldChar w:fldCharType="begin"/></w:r><w:r><w:instrText xml:space="preserve"> {code} </w:instrText></w:r><w:r><w:fldChar w:fldCharType="separate"/></w:r>{result}<w:r><w:fldChar w:fldCharType="end"/></w:r>"#
    )
}

#[test]
fn complex_fields_keep_their_code_as_markers_and_show_their_result() {
    let document = read(&format!(
        "<w:p>{}{}{}</w:p>",
        field("PAGE", "<w:r><w:t>3</w:t></w:r><w:r><w:t>x</w:t></w:r>"),
        field("AUTHOR", "<w:r><w:t>Ann</w:t></w:r>"),
        field("NUMPAGES", "<w:r><w:t>7</w:t></w:r>")
    ));
    let paragraph = p(&document, 0);
    // Begin, code and separate are contiguous and so are one marker, and so
    // are the end of one field and the start of the next.
    assert_eq!(
        kinds(paragraph),
        ["marker", "pagenumber", "marker", "text", "marker", "pagenumber", "marker"]
    );
    assert_eq!(paragraph.runs[1].kind, Inline::PageNumber(PageField::Page));
    assert_eq!(paragraph.text(), "3Ann7");
}

#[test]
fn nested_fields_and_fields_across_paragraphs_do_not_lose_their_place() {
    let nested = field(
        "IF ",
        &format!(
            "{}<w:r><w:t>after</w:t></w:r>",
            field("PAGE", "<w:r><w:t>8</w:t></w:r>")
        ),
    );
    let document = read(&format!(
        r#"<w:p>{nested}<w:r><w:t>out</w:t></w:r></w:p>
<w:p><w:r><w:fldChar w:fldCharType="begin"/></w:r><w:r><w:instrText> TOC </w:instrText></w:r><w:r><w:fldChar w:fldCharType="separate"/></w:r><w:r><w:t>entry one</w:t></w:r></w:p>
<w:p><w:r><w:t>entry two</w:t></w:r><w:r><w:fldChar w:fldCharType="end"/></w:r></w:p>
<w:p>{}</w:p>"#,
        field("PAGE", "<w:r><w:t>2</w:t></w:r>")
    ));
    let first = p(&document, 0);
    assert!(first.runs.iter().any(|r| r.kind == Inline::PageNumber(PageField::Page)));
    assert_eq!(first.text(), "8afterout");
    // The result of a table of contents is text, whichever paragraph it is in.
    assert_eq!(p(&document, 1).text(), "entry one");
    assert_eq!(p(&document, 2).text(), "entry two");
    // After the end of that field a page field is still a page number.
    assert!(p(&document, 3).runs.iter().any(|r| matches!(r.kind, Inline::PageNumber(_))));
}

#[test]
fn hyperlink_fields_link_their_result() {
    let document = read(&format!(
        "<w:p>{}</w:p>",
        field(r#"HYPERLINK "https://example.org/x" \o "tip""#, "<w:r><w:t>site</w:t></w:r>")
    ));
    let run = p(&document, 0)
        .runs
        .iter()
        .find(|r| r.text == "site")
        .unwrap()
        .clone();
    assert_eq!(run.link.unwrap().target, "https://example.org/x");
}

#[test]
fn things_kept_but_not_shown_are_zero_width_markers() {
    let d = doc(
        r#"<w:p><w:bookmarkStart w:id="0" w:name="b"/><w:bookmarkEnd w:id="0"/><w:r><w:t>a</w:t></w:r><w:commentRangeStart w:id="1"/><w:r><w:t>b</w:t></w:r><w:commentRangeEnd w:id="1"/><w:r><w:commentReference w:id="1"/></w:r><w:proofErr w:type="spellStart"/><w:permStart w:id="2"/><w:permEnd w:id="2"/><w:del w:id="3"><w:r><w:delText>gone</w:delText></w:r></w:del><w:moveFrom w:id="4"><w:r><w:t>moved</w:t></w:r></w:moveFrom><w:moveFromRangeStart w:id="5"/><w:moveFromRangeEnd w:id="5"/><w:moveToRangeStart w:id="6"/><w:moveToRangeEnd w:id="6"/><w:ins w:id="7"><w:r><w:t>new</w:t></w:r></w:ins><w:moveTo w:id="8"><w:r><w:t>here</w:t></w:r></w:moveTo><w:unknownThing><w:x/></w:unknownThing><w:r><w:t>end</w:t></w:r></w:p>"#,
    );
    let bytes = d.bytes();
    let document = d.read();
    let paragraph = p(&document, 0);
    assert_eq!(paragraph.text(), "abnewhereend");
    for run in paragraph.runs.iter().filter(|r| r.kind == Inline::Marker) {
        assert!(run.text.is_empty());
    }
    // Neighbouring markers are one marker, and a marker never takes room.
    assert_eq!(
        kinds(paragraph),
        ["marker", "text", "marker", "text", "marker", "text", "text", "marker", "text"]
    );
    let part = common_part(&bytes, "word/document.xml");
    let first = paragraph.runs[0].source.clone().unwrap();
    assert_eq!(
        raw(&part, &first.range),
        r#"<w:bookmarkStart w:id="0" w:name="b"/><w:bookmarkEnd w:id="0"/>"#
    );
    let deleted = paragraph.runs[4].source.clone().unwrap();
    assert!(raw(&part, &deleted.range).starts_with("<w:commentRangeEnd"));
    assert!(raw(&part, &deleted.range).contains("<w:del "));
    assert!(raw(&part, &deleted.range).contains("<w:moveFrom"));
    assert!(raw(&part, &deleted.range).ends_with(r#"<w:moveToRangeEnd w:id="6"/>"#));
}

#[test]
fn drawing_runs_hold_an_object_character() {
    let document = read(
        r#"<w:p><w:r><w:rPr><w:noProof/></w:rPr><w:drawing><wp:inline><wp:extent cx="100" cy="200"/><wp:docPr id="1" name="x" descr="d"/><a:graphic><a:graphicData uri="x"><a:unknown/></a:graphicData></a:graphic></wp:inline></w:drawing></w:r></w:p>"#,
    );
    let run = &p(&document, 0).runs[0];
    assert_eq!(run.text, OBJECT);
    let Inline::Drawing(d) = &run.kind else {
        panic!("a drawing")
    };
    assert_eq!(d.size, (100, 200));
    assert_eq!(d.alt.as_deref(), Some("d"));
    assert_eq!(d.content, DrawingContent::Other("shape".to_owned()));
}

#[test]
fn ruby_keeps_its_base_text_and_its_reading() {
    let document = read(
        r#"<w:p><w:r><w:ruby><w:rubyPr><w:hps w:val="12"/></w:rubyPr><w:rt><w:r><w:t>とうきょう</w:t></w:r></w:rt><w:rubyBase><w:r><w:t>東京</w:t></w:r></w:rubyBase></w:ruby></w:r></w:p>"#,
    );
    let run = &p(&document, 0).runs[0];
    assert_eq!(run.text, "東京");
    let Inline::Ruby(ruby) = &run.kind else {
        panic!("a ruby")
    };
    assert_eq!(ruby.text, "とうきょう");
    assert_eq!(ruby.size, 6.0);
}

#[test]
fn page_number_runs_made_of_pgnum_in_a_header() {
    let document = doc(r#"<w:p/><w:sectPr><w:headerReference w:type="default" r:id="rIdH"/></w:sectPr>"#)
        .marginal(
            "rIdH",
            "header",
            "header1.xml",
            r#"<w:p><w:r><w:pgNum/></w:r></w:p>"#,
        )
        .read();
    let story = &document.marginals[0];
    let paragraph = story.blocks[0].as_paragraph().unwrap();
    assert_eq!(paragraph.runs[0].kind, Inline::PageNumber(PageField::Page));
}

// ---------------------------------------------------------------------------
// Sources
// ---------------------------------------------------------------------------

fn check_paragraph_range(part: &[u8], paragraph: &Paragraph) {
    let source = paragraph.source.as_ref().expect("a source");
    let text = raw(part, &source.range);
    assert!(
        text.starts_with("<w:p>") || text.starts_with("<w:p ") || text.starts_with("<w:p/>"),
        "starts with {:?}",
        &text[..text.len().min(20)]
    );
    assert!(text.ends_with("</w:p>") || text.ends_with("/>"), "ends with {:?}", &text[text.len().saturating_sub(10)..]);
    assert_eq!(source.runs.len(), paragraph.runs.len());
    for (run, read) in paragraph.runs.iter().zip(&source.runs) {
        assert!(Arc::ptr_eq(run.source.as_ref().unwrap(), read));
        let range = raw(part, &read.range);
        assert!(range.starts_with('<') && range.ends_with('>'), "run range {range:?}");
        assert!(read.range.start >= source.range.start && read.range.end <= source.range.end);
        if let Some(parent) = &read.parent {
            assert!(raw(part, parent).starts_with("<w:r"));
            assert!(parent.start <= read.range.start && read.range.end <= parent.end);
        }
    }
    assert!(paragraph.is_as_read());
}

fn check_blocks(part: &[u8], blocks: &[Block], seen: &mut usize) {
    for block in blocks {
        match block {
            Block::Paragraph(paragraph) => {
                check_paragraph_range(part, paragraph);
                *seen += 1;
                for run in &paragraph.runs {
                    if let Inline::Drawing(d) = &run.kind {
                        if let DrawingContent::TextBox(boxed) = &d.content {
                            check_blocks(part, &boxed.blocks, seen);
                        }
                    }
                }
            }
            Block::Table(table) => {
                let range = table.source.clone().expect("table range");
                assert!(raw(part, &range).starts_with("<w:tbl>"));
                assert!(raw(part, &range).ends_with("</w:tbl>"));
                for row in &table.rows {
                    let range = row.source.clone().expect("row range");
                    assert!(raw(part, &range).starts_with("<w:tr"));
                    assert!(raw(part, &range).ends_with("</w:tr>"));
                    for cell in &row.cells {
                        let range = cell.source.clone().expect("cell range");
                        assert!(raw(part, &range).starts_with("<w:tc>"));
                        assert!(raw(part, &range).ends_with("</w:tc>"));
                        check_blocks(part, &cell.blocks, seen);
                    }
                }
            }
        }
    }
}

#[test]
fn every_range_of_the_sample_is_its_element() {
    let bytes = docx_sample::sample();
    let document = Docx.read(bytes.clone()).unwrap();
    let main = common_part(&bytes, "word/document.xml");
    let mut seen = 0;
    check_blocks(&main, &document.body.blocks, &mut seen);
    assert!(seen > 30);
    for (_, section) in document.sections() {
        let range = section.source.clone().unwrap();
        assert!(raw(&main, &range).starts_with("<w:sectPr"));
        assert!(raw(&main, &range).ends_with("</w:sectPr>"));
    }
    for story in &document.marginals {
        let part = common_part(&bytes, story.part.as_deref().unwrap());
        check_blocks(&part, &story.blocks, &mut seen);
    }
    for notes in [&document.footnotes, &document.endnotes] {
        let part = common_part(&bytes, notes.part.as_deref().unwrap());
        for note in &notes.notes {
            check_blocks(&part, &note.blocks, &mut seen);
        }
    }
    // Text boxes are in the main part and counted too.
    assert!(document
        .paragraphs()
        .iter()
        .any(|(_, p)| p.runs.iter().any(|r| matches!(&r.kind, Inline::Drawing(d) if matches!(d.content, DrawingContent::TextBox(_))))));
}

#[test]
fn a_paragraph_straight_from_the_file_is_as_read() {
    let document = Docx.read(docx_sample::sample()).unwrap();
    let all = document.paragraphs();
    assert!(all.len() > 30);
    for (path, paragraph) in all {
        assert!(paragraph.is_as_read(), "paragraph {path:?} is not as read");
    }
    // Changing anything makes it not so.
    let mut changed = document.clone();
    let path = Path::body(5);
    changed.paragraph_mut(&path).unwrap().runs[0].look.bold ^= true;
    assert!(!changed.paragraph(&path).unwrap().is_as_read());
}

#[test]
fn a_byte_order_mark_does_not_shift_the_ranges() {
    let mut d = doc(r#"<w:p><w:r><w:t>bom</w:t></w:r></w:p>"#);
    d.bom = true;
    let bytes = d.bytes();
    let document = d.read();
    let part = common_part(&bytes, "word/document.xml");
    assert!(part.starts_with(b"\xEF\xBB\xBF"));
    let source = p(&document, 0).source.clone().unwrap();
    assert_eq!(raw(&part, &source.range), "<w:p><w:r><w:t>bom</w:t></w:r></w:p>");
}

#[test]
fn an_empty_paragraph_element_has_a_range_too() {
    let d = doc(r#"<w:p/><w:p w:rsidR="00A1"/>"#);
    let bytes = d.bytes();
    let document = d.read();
    let part = common_part(&bytes, "word/document.xml");
    assert_eq!(raw(&part, &p(&document, 0).source.clone().unwrap().range), "<w:p/>");
    assert_eq!(
        raw(&part, &p(&document, 1).source.clone().unwrap().range),
        r#"<w:p w:rsidR="00A1"/>"#
    );
    assert!(p(&document, 0).runs.is_empty());
}

#[test]
fn alternate_content_inside_a_run_keeps_its_own_range() {
    let d = doc(
        r#"<w:p><w:r><mc:AlternateContent><mc:Choice Requires="wps"><w:drawing><wp:inline><wp:extent cx="1" cy="1"/></wp:inline></w:drawing></mc:Choice><mc:Fallback><w:pict/></mc:Fallback></mc:AlternateContent><w:t>after</w:t></w:r></w:p>"#,
    );
    let bytes = d.bytes();
    let document = d.read();
    let paragraph = p(&document, 0);
    assert_eq!(kinds(paragraph), ["drawing", "text"]);
    let part = common_part(&bytes, "word/document.xml");
    let range = &paragraph.runs[0].source.clone().unwrap().range;
    assert!(raw(&part, range).starts_with("<mc:AlternateContent>"));
    assert!(raw(&part, range).ends_with("</mc:AlternateContent>"));
}
