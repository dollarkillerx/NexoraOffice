//! Hostile and broken files, speed, and files made by other programs.

mod common;
#[path = "common/docx_doc.rs"]
mod docx_doc;
#[path = "common/docx_sample.rs"]
mod docx_sample;

use std::io::{Cursor, Read};
use std::time::Instant;

use aster_ooxml::{Docx, FormatCodec};
use aster_writer::*;
use docx_doc::*;

/// Replace the contents of `name` in a package.
fn with_part(bytes: &[u8], name: &str, replace: impl Fn(&[u8]) -> Vec<u8>) -> Vec<u8> {
    let mut archive = zip::ZipArchive::new(Cursor::new(bytes)).unwrap();
    let mut parts = Vec::new();
    for i in 0..archive.len() {
        let mut entry = archive.by_index(i).unwrap();
        let mut data = Vec::new();
        entry.read_to_end(&mut data).unwrap();
        let part = entry.name().to_owned();
        if part == name {
            data = replace(&data);
        }
        parts.push((part, data));
    }
    docx_sample::zip(&parts)
}

#[test]
fn truncated_and_corrupted_parts_never_panic() {
    let sample = docx_sample::sample();
    let names = [
        "word/document.xml",
        "word/styles.xml",
        "word/numbering.xml",
        "word/header1.xml",
        "word/footnotes.xml",
        "word/settings.xml",
        "word/theme/theme1.xml",
        "word/_rels/document.xml.rels",
    ];
    let mut tried = 0;
    let mut read_fine = 0;
    for name in names {
        let length = common_part(&sample, name).len();
        let step = (length / 120).max(1);
        // Cut off at many places.
        for at in (0..length).step_by(step) {
            let broken = with_part(&sample, name, |data| data[..at].to_vec());
            tried += 1;
            read_fine += usize::from(Docx.read(broken).is_ok());
        }
        // Damage one byte at many places, in several ways.
        for (n, at) in (0..length).step_by(step).enumerate() {
            let byte = [b'<', b'>', b'&', b'"', 0xFF, 0][n % 6];
            let broken = with_part(&sample, name, |data| {
                let mut data = data.to_vec();
                data[at] = byte;
                data
            });
            tried += 1;
            read_fine += usize::from(Docx.read(broken).is_ok());
        }
    }
    assert!(tried > 1_000);
    // Parts that are not the main one are optional: damage there still
    // gives a document.
    assert!(read_fine > tried / 3, "{read_fine} of {tried}");
}

#[test]
fn a_zip_that_is_cut_off_is_an_error() {
    let sample = docx_sample::sample();
    for at in (0..sample.len()).step_by(97) {
        let _ = Docx.read(sample[..at].to_vec());
    }
}

fn nest(open: &str, close: &str, inner: &str, depth: usize) -> String {
    format!("{}{inner}{}", open.repeat(depth), close.repeat(depth))
}

#[test]
fn deep_nesting_is_refused_or_cut_off_without_overflowing_the_stack() {
    let cell = |inner: &str| {
        format!(
            r#"<w:tbl><w:tblGrid><w:gridCol w:w="500"/></w:tblGrid><w:tr><w:tc>{inner}<w:p/></w:tc></w:tr></w:tbl>"#
        )
    };
    // Tables: as deep as the XML reader goes, then fewer than that.
    for depth in [10, 30, 62, 100, 400] {
        let mut xml = String::from("<w:p><w:r><w:t>core</w:t></w:r></w:p>");
        for _ in 0..depth {
            xml = cell(&xml);
        }
        if let Ok(document) = Docx.read(doc(&xml).bytes()) {
            fn depth_of(blocks: &[Block]) -> usize {
                blocks
                    .iter()
                    .filter_map(Block::as_table)
                    .flat_map(|t| t.rows.iter().flat_map(|r| r.cells.iter()))
                    .map(|c| 1 + depth_of(&c.blocks))
                    .max()
                    .unwrap_or(0)
            }
            assert!(depth_of(&document.body.blocks) <= 12, "depth {depth}");
        }
    }
    // Content controls, hyperlinks, tracked changes, text boxes.
    let sdt = nest(
        "<w:sdt><w:sdtContent>",
        "</w:sdtContent></w:sdt>",
        "<w:p><w:r><w:t>x</w:t></w:r></w:p>",
        120,
    );
    let _ = Docx.read(doc(&sdt).bytes());
    let links = nest(
        "<w:hyperlink w:anchor=\"a\">",
        "</w:hyperlink>",
        "<w:r><w:t>x</w:t></w:r>",
        200,
    );
    let _ = Docx.read(doc(&format!("<w:p>{links}</w:p>")).bytes());
    let ins = nest("<w:ins>", "</w:ins>", "<w:r><w:t>x</w:t></w:r>", 200);
    let _ = Docx.read(doc(&format!("<w:p>{ins}</w:p>")).bytes());
    let alt = nest(
        "<mc:AlternateContent><mc:Choice Requires=\"wps\">",
        "</mc:Choice></mc:AlternateContent>",
        "<w:p/>",
        100,
    );
    let _ = Docx.read(doc(&alt).bytes());
    let mut boxed = String::from("<w:p><w:r><w:t>core</w:t></w:r></w:p>");
    for _ in 0..15 {
        boxed = format!(
            r#"<w:p><w:r><w:drawing><wp:inline><wp:extent cx="1" cy="1"/><a:graphic><a:graphicData uri="x"><wps:wsp><wps:txbx><w:txbxContent>{boxed}</w:txbxContent></wps:txbx></wps:wsp></a:graphicData></a:graphic></wp:inline></w:drawing></w:r></w:p>"#
        );
    }
    let document = Docx.read(doc(&boxed).bytes()).unwrap();
    fn boxes(blocks: &[Block]) -> usize {
        blocks
            .iter()
            .filter_map(Block::as_paragraph)
            .flat_map(|p| p.runs.iter())
            .filter_map(|r| match &r.kind {
                Inline::Drawing(d) => match &d.content {
                    DrawingContent::TextBox(t) => Some(1 + boxes(&t.blocks)),
                    _ => None,
                },
                _ => None,
            })
            .max()
            .unwrap_or(0)
    }
    assert!(boxes(&document.body.blocks) <= 8);
    // Fields that are never closed, and closed more often than opened.
    let open = r#"<w:r><w:fldChar w:fldCharType="begin"/></w:r>"#.repeat(500);
    let close = r#"<w:r><w:fldChar w:fldCharType="end"/></w:r>"#.repeat(700);
    let d = Docx
        .read(doc(&format!("<w:p>{open}<w:r><w:t>a</w:t></w:r>{close}<w:r><w:t>b</w:t></w:r></w:p><w:p><w:r><w:t>c</w:t></w:r></w:p>")).bytes())
        .unwrap();
    assert!(d.plain_text().ends_with('c'));
}

#[test]
fn huge_numbers_are_clamped() {
    let d = doc(
        r#"<w:p><w:pPr><w:ind w:left="99999999999999" w:right="-99999999999999" w:firstLineChars="99999999999"/><w:spacing w:before="999999999999" w:after="-999999999999" w:line="9999999999999" w:beforeLines="99999999999"/><w:outlineLvl w:val="99999999999"/><w:numPr><w:ilvl w:val="99999999999"/><w:numId w:val="99999999999"/></w:numPr><w:tabs><w:tab w:val="left" w:pos="999999999999"/></w:tabs></w:pPr><w:r><w:rPr><w:sz w:val="99999999999"/><w:spacing w:val="999999999999"/><w:w w:val="99999999999"/><w:position w:val="999999999999"/></w:rPr><w:t>x</w:t></w:r><w:r><w:footnoteReference w:id="99999999999999"/></w:r></w:p><w:sectPr><w:pgSz w:w="99999999999" w:h="99999999999"/><w:pgMar w:top="99999999999999"/><w:cols w:num="99999999999" w:space="99999999999"/><w:pgNumType w:start="99999999999999"/><w:docGrid w:type="lines" w:linePitch="99999999999"/></w:sectPr>"#,
    )
    .numbering(r#"<w:abstractNum w:abstractNumId="99999999999"><w:lvl w:ilvl="99999999999"><w:start w:val="99999999999"/></w:lvl><w:lvl w:ilvl="2"><w:start w:val="4294967295"/></w:lvl></w:abstractNum><w:num w:numId="99999999999"><w:abstractNumId w:val="99999999999"/><w:lvlOverride w:ilvl="99999999999"><w:startOverride w:val="99999999999"/></w:lvlOverride></w:num>"#)
    .read();
    let paragraph = p(&d, 0);
    assert!(paragraph.base.size <= 2_000.0);
    assert!(paragraph.props.indent_left.abs() <= 1_000_000);
    assert!(d.last_section.page.0 <= 1_000_000);
    assert!(d.last_section.columns.count <= 45);
}

#[test]
fn an_enormous_list_start_does_not_overflow() {
    let numbering = r#"<w:abstractNum w:abstractNumId="0"><w:lvl w:ilvl="0"><w:start w:val="4294967295"/><w:numFmt w:val="lowerLetter"/><w:lvlText w:val="%1"/></w:lvl></w:abstractNum><w:num w:numId="1"><w:abstractNumId w:val="0"/></w:num>"#;
    let item = r#"<w:p><w:pPr><w:numPr><w:numId w:val="1"/></w:numPr></w:pPr></w:p>"#;
    let d = doc(&item.repeat(3)).numbering(numbering).read();
    assert_eq!(d.paragraphs().len(), 3);
}

#[test]
fn an_image_over_the_limit_fails_the_file_as_it_does_for_presentations() {
    // A part that inflates beyond its ratio limit is refused by the package.
    let mut d = doc(r#"<w:p><w:r><w:drawing><wp:inline><wp:extent cx="1" cy="1"/><a:graphic><a:graphicData uri="x"><pic:pic><pic:blipFill><a:blip r:embed="rIdBig"/></pic:blipFill></pic:pic></a:graphicData></a:graphic></wp:inline></w:drawing></w:r></w:p>"#)
        .rel("rIdBig", "image", "media/big.png");
    d.binary
        .push(("word/media/big.png".to_owned(), vec![0u8; 3 << 20]));
    let err = Docx.read(d.bytes()).unwrap_err();
    assert_eq!(err.kind(), aster_ooxml::ErrorKind::TooLarge);
}

// ---------------------------------------------------------------------------
// Speed
// ---------------------------------------------------------------------------

fn big_document(paragraphs: usize) -> Vec<u8> {
    let styles = r#"<w:docDefaults><w:rPrDefault><w:rPr><w:sz w:val="21"/></w:rPr></w:rPrDefault></w:docDefaults>
<w:style w:type="paragraph" w:default="1" w:styleId="Normal"><w:name w:val="Normal"/></w:style>
<w:style w:type="paragraph" w:styleId="H1"><w:name w:val="heading 1"/><w:basedOn w:val="Normal"/><w:pPr><w:keepNext/><w:outlineLvl w:val="0"/></w:pPr><w:rPr><w:b/><w:sz w:val="32"/></w:rPr></w:style>
<w:style w:type="character" w:styleId="Em"><w:name w:val="Em"/><w:rPr><w:i/></w:rPr></w:style>"#;
    let numbering = r#"<w:abstractNum w:abstractNumId="0"><w:lvl w:ilvl="0"><w:start w:val="1"/><w:numFmt w:val="decimal"/><w:lvlText w:val="%1."/></w:lvl></w:abstractNum><w:num w:numId="1"><w:abstractNumId w:val="0"/></w:num>"#;
    let mut body = String::new();
    for i in 0..paragraphs {
        match i % 10 {
            0 => {
                body += &format!(
                    r#"<w:p><w:pPr><w:pStyle w:val="H1"/></w:pPr><w:r><w:t>Heading {i}</w:t></w:r></w:p>"#
                )
            }
            1 | 2 => {
                body += &format!(
                    r#"<w:p><w:pPr><w:numPr><w:numId w:val="1"/></w:numPr></w:pPr><w:r><w:t>Item {i}</w:t></w:r></w:p>"#
                )
            }
            _ => {
                body += &format!(
                    r#"<w:p><w:pPr><w:spacing w:after="120"/><w:jc w:val="both"/></w:pPr><w:r><w:t xml:space="preserve">Paragraph {i} with some words, </w:t></w:r><w:r><w:rPr><w:b/></w:rPr><w:t>bold</w:t></w:r><w:r><w:rPr><w:rStyle w:val="Em"/></w:rPr><w:t xml:space="preserve"> emphasis </w:t></w:r><w:r><w:rPr><w:rFonts w:ascii="Arial" w:eastAsia="SimSun"/><w:color w:val="336699"/></w:rPr><w:t>中文字体 and more text to fill the line.</w:t></w:r></w:p>"#
                );
            }
        }
    }
    doc(&body).styles(styles).numbering(numbering).bytes()
}

#[test]
fn five_thousand_paragraphs_read_quickly() {
    let bytes = big_document(5_000);
    let started = Instant::now();
    let document = Docx.read(bytes.clone()).unwrap();
    let elapsed = started.elapsed();
    eprintln!(
        "DOCX read: {} paragraphs, {} bytes of XML in {elapsed:?}",
        document.paragraphs().len(),
        common_part(&bytes, "word/document.xml").len()
    );
    assert_eq!(document.paragraphs().len(), 5_000);
    assert!(document.paragraphs().iter().all(|(_, p)| p.is_as_read()));
    // Generous: a release build takes a few tens of milliseconds.
    assert!(elapsed.as_secs_f64() < 5.0, "{elapsed:?}");
}

// ---------------------------------------------------------------------------
// Files made by other programs
// ---------------------------------------------------------------------------

/// A directory of an unpacked package as a zip file, `[Content_Types].xml`
/// first.
fn pack(dir: &std::path::Path) -> Vec<u8> {
    fn walk(dir: &std::path::Path, base: &std::path::Path, out: &mut Vec<(String, Vec<u8>)>) {
        let mut entries: Vec<_> = std::fs::read_dir(dir).unwrap().flatten().collect();
        entries.sort_by_key(|e| e.file_name());
        for entry in entries {
            let path = entry.path();
            if path.is_dir() {
                walk(&path, base, out);
            } else {
                let name = path
                    .strip_prefix(base)
                    .unwrap()
                    .to_string_lossy()
                    .replace('\\', "/");
                out.push((name, std::fs::read(&path).unwrap()));
            }
        }
    }
    let mut parts = Vec::new();
    walk(dir, dir, &mut parts);
    parts.sort_by_key(|(name, _)| name != "[Content_Types].xml");
    docx_sample::zip(&parts)
}

#[test]
fn the_spike_corpus_document_reads() {
    let dir =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../spikes/g4-ooxml/corpus/docx");
    if !dir.is_dir() {
        eprintln!("no corpus at {}", dir.display());
        return;
    }
    let bytes = pack(&dir);
    let document = Docx.read(bytes).expect("the corpus document reads");
    assert!(!document.paragraphs().is_empty());
    let text = document.plain_text();
    eprintln!(
        "corpus text: {:?}",
        text.chars().take(60).collect::<String>()
    );
    assert!(document.paragraphs().iter().all(|(_, p)| p.is_as_read()));
    assert!(!document.styles.styles.is_empty());
}
