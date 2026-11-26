//! Run and paragraph formatting, styles and lists of the DOCX reader.

mod common;
#[path = "common/docx_doc.rs"]
mod docx_doc;

use std::sync::Arc;

use aster_writer::*;
use docx_doc::*;

fn run(body_p: &str) -> Document {
    read(&format!("<w:p>{body_p}</w:p>"))
}

fn look_of(document: &Document) -> &RunLook {
    &p(document, 0).runs[0].look
}

// ---------------------------------------------------------------------------
// Run formatting
// ---------------------------------------------------------------------------

#[test]
fn text_is_ten_points_without_document_defaults() {
    let document = run(r#"<w:r><w:t>x</w:t></w:r>"#);
    assert_eq!(look_of(&document).size, 10.0);
    assert_eq!(p(&document, 0).base.size, 10.0);
    assert_eq!(p(&document, 0).mark.size, 10.0);
}

#[test]
fn layers_add_up_from_defaults_through_styles_to_direct_formatting() {
    let styles = r#"<w:docDefaults><w:rPrDefault><w:rPr><w:rFonts w:ascii="Arial" w:hAnsi="Arial" w:eastAsia="SimSun"/><w:sz w:val="24"/><w:color w:val="111111"/></w:rPr></w:rPrDefault></w:docDefaults>
<w:style w:type="paragraph" w:default="1" w:styleId="Normal"><w:name w:val="Normal"/><w:rPr><w:i/></w:rPr></w:style>
<w:style w:type="paragraph" w:styleId="Base"><w:name w:val="Base"/><w:basedOn w:val="Normal"/><w:rPr><w:b/><w:sz w:val="28"/></w:rPr></w:style>
<w:style w:type="paragraph" w:styleId="Child"><w:name w:val="Child"/><w:basedOn w:val="Base"/><w:rPr><w:color w:val="222222"/><w:i w:val="0"/></w:rPr></w:style>
<w:style w:type="character" w:styleId="Loud"><w:name w:val="Loud"/><w:rPr><w:sz w:val="40"/><w:u w:val="single"/></w:rPr></w:style>"#;
    let document = doc(
        r#"<w:p><w:pPr><w:pStyle w:val="Child"/><w:rPr><w:strike/></w:rPr></w:pPr>
<w:r><w:t>plain</w:t></w:r>
<w:r><w:rPr><w:rStyle w:val="Loud"/></w:rPr><w:t>styled</w:t></w:r>
<w:r><w:rPr><w:rStyle w:val="Loud"/><w:sz w:val="16"/><w:b w:val="0"/></w:rPr><w:t>direct</w:t></w:r></w:p>"#,
    )
    .styles(styles)
    .read();
    let paragraph = p(&document, 0);
    let base = &paragraph.base;
    assert_eq!(base.font.as_deref(), Some("Arial"));
    assert_eq!(base.east_asian_font.as_deref(), Some("SimSun"));
    assert_eq!(base.size, 14.0);
    assert!(base.bold);
    assert!(!base.italic);
    assert_eq!(base.color, Some(rgb(0x222222)));
    // The mark is the base plus the paragraph's own run properties.
    assert!(paragraph.mark.strike && !base.strike);
    assert_eq!(paragraph.mark.size, 14.0);
    // Runs: the base, then the character style, then direct formatting.
    assert_eq!(paragraph.runs[0].look, *base);
    let styled = &paragraph.runs[1];
    assert_eq!(styled.style.as_deref(), Some("Loud"));
    assert_eq!(styled.look.size, 20.0);
    assert_eq!(styled.look.underline.unwrap().style, UnderlineStyle::Single);
    assert!(styled.look.bold);
    let direct = &paragraph.runs[2].look;
    assert_eq!(direct.size, 8.0);
    assert!(!direct.bold);
    assert!(direct.underline.is_some());
}

#[test]
fn a_table_style_sits_between_the_paragraph_style_and_the_character_style() {
    let styles = r#"<w:style w:type="paragraph" w:default="1" w:styleId="Normal"><w:name w:val="Normal"/><w:rPr><w:sz w:val="20"/><w:color w:val="000001"/></w:rPr></w:style>
<w:style w:type="character" w:styleId="Em"><w:name w:val="Em"/><w:rPr><w:color w:val="AA0000"/></w:rPr></w:style>
<w:style w:type="table" w:styleId="Grid"><w:name w:val="Grid"/><w:rPr><w:color w:val="00AA00"/><w:i/></w:rPr><w:pPr><w:jc w:val="center"/></w:pPr></w:style>"#;
    let table = |cell: &str| {
        format!(
            r#"<w:tbl><w:tblPr><w:tblStyle w:val="Grid"/></w:tblPr><w:tblGrid><w:gridCol w:w="1000"/></w:tblGrid><w:tr><w:tc><w:p>{cell}</w:p></w:tc></w:tr></w:tbl>"#
        )
    };
    let document = doc(&format!(
        "{}<w:p><w:r><w:t>outside</w:t></w:r></w:p>",
        table(r#"<w:r><w:t>in</w:t></w:r><w:r><w:rPr><w:rStyle w:val="Em"/></w:rPr><w:t>em</w:t></w:r>"#)
    ))
    .styles(styles)
    .read();
    let cell = &t(&document, 0).rows[0].cells[0];
    let paragraph = cell.blocks[0].as_paragraph().unwrap();
    assert_eq!(paragraph.base.color, Some(rgb(0x00AA00)));
    assert!(paragraph.base.italic);
    assert_eq!(paragraph.base.size, 10.0);
    assert_eq!(paragraph.runs[1].look.color, Some(rgb(0xAA0000)));
    assert_eq!(paragraph.props.align, Align::Center);
    let outside = p(&document, 1);
    assert_eq!(outside.base.color, Some(rgb(1)));
    assert_eq!(outside.props.align, Align::Left);
}

#[test]
fn conditional_table_formats_follow_the_table_look() {
    let styles = r#"<w:style w:type="table" w:styleId="Banded"><w:name w:val="Banded"/><w:tblStylePr w:type="firstRow"><w:rPr><w:b/></w:rPr><w:tcPr><w:shd w:val="clear" w:color="auto" w:fill="000080"/></w:tcPr></w:tblStylePr><w:tblStylePr w:type="lastRow"><w:rPr><w:i/></w:rPr></w:tblStylePr><w:tblStylePr w:type="firstCol"><w:rPr><w:u w:val="single"/></w:rPr></w:tblStylePr><w:tblStylePr w:type="band1Horz"><w:tcPr><w:shd w:val="clear" w:color="auto" w:fill="EEEEEE"/></w:tcPr></w:tblStylePr><w:tblStylePr w:type="band2Horz"><w:tcPr><w:shd w:val="clear" w:color="auto" w:fill="DDDDDD"/></w:tcPr></w:tblStylePr></w:style>"#;
    let row = |n: u32| {
        format!(
            r#"<w:tr><w:tc><w:p><w:r><w:t>{n}a</w:t></w:r></w:p></w:tc><w:tc><w:p><w:r><w:t>{n}b</w:t></w:r></w:p></w:tc></w:tr>"#
        )
    };
    let table = |look: &str| {
        format!(
            r#"<w:tbl><w:tblPr><w:tblStyle w:val="Banded"/>{look}</w:tblPr><w:tblGrid><w:gridCol w:w="1000"/><w:gridCol w:w="1000"/></w:tblGrid>{}{}{}{}</w:tbl>"#,
            row(0),
            row(1),
            row(2),
            row(3)
        )
    };
    let document = doc(&format!(
        "{}{}",
        table(r#"<w:tblLook w:val="04A0" w:firstRow="1" w:lastRow="0" w:firstColumn="1" w:lastColumn="0" w:noHBand="0" w:noVBand="1"/>"#),
        table(r#"<w:tblLook w:firstRow="0" w:lastRow="1" w:firstColumn="0" w:lastColumn="0" w:noHBand="1" w:noVBand="1"/>"#)
    ))
    .styles(styles)
    .read();
    let first = t(&document, 0);
    let shade = |table: &Table, r: usize, c: usize| table.rows[r].cells[c].shading;
    let para = |table: &Table, r: usize, c: usize| -> Paragraph {
        table.rows[r].cells[c].blocks[0]
            .as_paragraph()
            .unwrap()
            .clone()
    };
    assert_eq!(shade(first, 0, 0), Some(rgb(0x000080)));
    assert!(para(first, 0, 1).base.bold);
    // Banding starts after the header row.
    assert_eq!(shade(first, 1, 0), Some(rgb(0xEEEEEE)));
    assert_eq!(shade(first, 2, 1), Some(rgb(0xDDDDDD)));
    assert_eq!(shade(first, 3, 1), Some(rgb(0xEEEEEE)));
    assert!(para(first, 2, 0).base.underline.is_some());
    assert!(para(first, 2, 1).base.underline.is_none());
    assert!(!para(first, 3, 0).base.italic);
    let second = t(&document, 1);
    assert_eq!(shade(second, 0, 0), None);
    assert!(!para(second, 0, 0).base.bold);
    assert!(para(second, 3, 1).base.italic);
    assert!(para(second, 3, 0).base.underline.is_none());
    assert_eq!(shade(second, 1, 0), None);
}

#[test]
fn on_off_elements_are_on_unless_they_say_off() {
    let document = run(
        r#"<w:r><w:rPr><w:b/><w:i w:val="0"/><w:strike w:val="false"/><w:dstrike w:val="off"/><w:caps w:val="true"/><w:smallCaps w:val="1"/><w:vanish/></w:rPr><w:t>x</w:t></w:r>"#,
    );
    let look = look_of(&document);
    assert!(look.bold && look.caps && look.small_caps && look.hidden);
    assert!(!look.italic && !look.strike && !look.double_strike);
}

#[test]
fn run_elements_set_their_properties() {
    let document = run(
        r#"<w:r><w:rPr><w:u w:val="wave" w:color="00FF00"/><w:dstrike/><w:color w:val="auto"/><w:highlight w:val="darkCyan"/><w:shd w:val="clear" w:color="auto" w:fill="ABCDEF"/><w:vertAlign w:val="superscript"/><w:spacing w:val="-10"/><w:w w:val="80"/><w:position w:val="-6"/><w:sz w:val="23"/></w:rPr><w:t>x</w:t></w:r>"#,
    );
    let look = look_of(&document);
    let u = look.underline.unwrap();
    assert_eq!(
        (u.style, u.color),
        (UnderlineStyle::Wave, Some(rgb(0x00FF00)))
    );
    assert!(look.double_strike);
    assert_eq!(look.color, None);
    assert_eq!(look.highlight, Some(rgb(0x008080)));
    assert_eq!(look.shading, Some(rgb(0xABCDEF)));
    assert_eq!(look.position, VertAlign::Superscript);
    assert_eq!(look.spacing, -10);
    assert_eq!(look.scale, 0.8);
    assert_eq!(look.raise, -3.0);
    assert_eq!(look.size, 11.5);
}

#[test]
fn underline_names_map_to_kinds_and_none_turns_it_off() {
    let kinds: Vec<UnderlineStyle> = [
        "single",
        "words",
        "double",
        "thick",
        "dotted",
        "dash",
        "dotDash",
        "wave",
        "wavyHeavy",
    ]
    .iter()
    .map(|name| {
        let d = run(&format!(
            r#"<w:r><w:rPr><w:u w:val="{name}"/></w:rPr><w:t>x</w:t></w:r>"#
        ));
        look_of(&d).underline.unwrap().style
    })
    .collect();
    use UnderlineStyle::*;
    assert_eq!(
        kinds,
        [
            Single, Words, Double, Thick, Dotted, Dashed, Dashed, Wave, Wave
        ]
    );
    let styles = r#"<w:style w:type="paragraph" w:default="1" w:styleId="Normal"><w:name w:val="Normal"/><w:rPr><w:u w:val="single"/></w:rPr></w:style>"#;
    let off = doc(r#"<w:p><w:r><w:rPr><w:u w:val="none"/></w:rPr><w:t>x</w:t></w:r></w:p>"#)
        .styles(styles)
        .read();
    assert_eq!(look_of(&off).underline, None);
}

#[test]
fn colours_may_come_from_the_theme_with_tint_and_shade() {
    let theme = r#"<a:theme xmlns:a="http://schemas.openxmlformats.org/drawingml/2006/main"><a:themeElements><a:clrScheme name="x"><a:dk1><a:srgbClr val="101010"/></a:dk1><a:lt1><a:srgbClr val="F0F0F0"/></a:lt1><a:dk2><a:srgbClr val="204060"/></a:dk2><a:lt2><a:srgbClr val="E0E0E0"/></a:lt2><a:accent1><a:srgbClr val="FF0000"/></a:accent1><a:hlink><a:srgbClr val="0000FF"/></a:hlink></a:clrScheme></a:themeElements></a:theme>"#;
    let color = |attrs: &str| {
        let d = doc(&format!(
            r#"<w:p><w:r><w:rPr><w:color {attrs}/></w:rPr><w:t>x</w:t></w:r></w:p>"#
        ))
        .theme(theme)
        .read();
        look_of(&d).color
    };
    assert_eq!(
        color(r#"w:val="123456" w:themeColor="accent1""#),
        Some(rgb(0xFF0000))
    );
    assert_eq!(
        color(r#"w:val="auto" w:themeColor="text1""#),
        Some(rgb(0x101010))
    );
    assert_eq!(color(r#"w:themeColor="background1""#), Some(rgb(0xF0F0F0)));
    assert_eq!(color(r#"w:themeColor="text2""#), Some(rgb(0x204060)));
    assert_eq!(color(r#"w:themeColor="hyperlink""#), Some(rgb(0x0000FF)));
    // Half way to white, and half way to black.
    assert_eq!(
        color(r#"w:themeColor="accent1" w:themeTint="80""#),
        Some([255, 127, 127, 255])
    );
    assert_eq!(
        color(r#"w:themeColor="accent1" w:themeShade="80""#),
        Some([128, 0, 0, 255])
    );
}

const THEME_FONTS: &str = r#"<a:theme xmlns:a="http://schemas.openxmlformats.org/drawingml/2006/main"><a:themeElements><a:fontScheme name="x"><a:majorFont><a:latin typeface="MajorLatin"/><a:ea typeface="MajorEa"/><a:font script="Jpan" typeface="MajorJpan"/></a:majorFont><a:minorFont><a:latin typeface="MinorLatin"/><a:ea typeface=""/><a:font script="Jpan" typeface="MinorJpan"/><a:font script="Hans" typeface="MinorHans"/><a:font script="Hant" typeface="MinorHant"/><a:font script="Hang" typeface="MinorHang"/></a:minorFont></a:fontScheme></a:themeElements></a:theme>"#;

fn fonts(settings: &str, rfonts: &str) -> (Option<String>, Option<String>) {
    let d = doc(&format!(
        r#"<w:p><w:r><w:rPr><w:rFonts {rfonts}/></w:rPr><w:t>x</w:t></w:r></w:p>"#
    ))
    .theme(THEME_FONTS)
    .settings(settings)
    .read();
    let look = look_of(&d);
    (
        look.font.as_deref().map(str::to_owned),
        look.east_asian_font.as_deref().map(str::to_owned),
    )
}

#[test]
fn theme_fonts_are_looked_up_with_the_script_of_the_theme_font_language() {
    let rfonts = r#"w:asciiTheme="minorHAnsi" w:eastAsiaTheme="minorEastAsia""#;
    let lang = |ea: &str| format!(r#"<w:themeFontLang w:val="en-US" w:eastAsia="{ea}"/>"#);
    let found = |ea: &str| fonts(&lang(ea), rfonts);
    assert_eq!(
        found("ja-JP"),
        (Some("MinorLatin".into()), Some("MinorJpan".into()))
    );
    assert_eq!(found("zh-CN").1.as_deref(), Some("MinorHans"));
    assert_eq!(found("zh-TW").1.as_deref(), Some("MinorHant"));
    assert_eq!(found("ko-KR").1.as_deref(), Some("MinorHang"));
    // A theme face that is not empty is used as it is.
    assert_eq!(
        fonts(
            &lang("ja-JP"),
            r#"w:asciiTheme="majorHAnsi" w:eastAsiaTheme="majorEastAsia""#
        ),
        (Some("MajorLatin".into()), Some("MajorEa".into()))
    );
    // The theme wins over a name written beside it.
    assert_eq!(
        fonts(
            "",
            r#"w:ascii="Plain" w:asciiTheme="minorHAnsi" w:eastAsia="PlainEa""#
        ),
        (Some("MinorLatin".into()), Some("PlainEa".into()))
    );
    assert_eq!(
        fonts("", r#"w:ascii="Arial" w:hAnsi="Arial" w:eastAsia="SimSun""#),
        (Some("Arial".into()), Some("SimSun".into()))
    );
}

#[test]
fn equal_font_names_share_their_storage() {
    let document = run(
        r#"<w:r><w:rPr><w:rFonts w:ascii="Arial"/></w:rPr><w:t>a</w:t></w:r><w:r><w:rPr><w:rFonts w:ascii="Arial"/></w:rPr><w:t>b</w:t></w:r><w:r><w:rPr><w:rStyle w:val="Same"/></w:rPr><w:t>c</w:t></w:r>"#,
    );
    let runs = &p(&document, 0).runs;
    assert!(Arc::ptr_eq(
        runs[0].look.font.as_ref().unwrap(),
        runs[1].look.font.as_ref().unwrap()
    ));
    assert!(
        Arc::ptr_eq(runs[2].style.as_ref().unwrap(), &Arc::from("Same"))
            || runs[2].style.as_deref() == Some("Same")
    );
}

#[test]
fn languages_come_from_the_default_run_properties() {
    let d = doc("<w:p/>")
        .styles(r#"<w:docDefaults><w:rPrDefault><w:rPr><w:lang w:val="fr-FR" w:eastAsia="ja-JP"/></w:rPr></w:rPrDefault></w:docDefaults>"#)
        .read();
    assert_eq!(d.settings.language.as_deref(), Some("fr-FR"));
    assert_eq!(d.settings.east_asian_language.as_deref(), Some("ja-JP"));
}

#[test]
fn settings_are_read() {
    let d = doc("<w:p/>")
        .settings(r#"<w:defaultTabStop w:val="560"/><w:evenAndOddHeaders/><w:characterSpacingControl w:val="compressPunctuation"/><w:themeFontLang w:val="en-GB"/>"#)
        .read();
    assert_eq!(d.settings.default_tab, 560);
    assert!(d.settings.even_and_odd_headers);
    assert!(d.settings.compress_punctuation);
    assert_eq!(d.settings.language.as_deref(), Some("en-GB"));
    let d = doc("<w:p/>")
        .settings(r#"<w:characterSpacingControl w:val="doNotCompress"/>"#)
        .read();
    assert!(!d.settings.compress_punctuation);
    assert_eq!(d.settings.default_tab, 720);
}

// ---------------------------------------------------------------------------
// Paragraph formatting
// ---------------------------------------------------------------------------

fn props(ppr: &str) -> ParaProps {
    let d = read(&format!(
        r#"<w:p><w:pPr>{ppr}</w:pPr><w:r><w:t>x</w:t></w:r></w:p>"#
    ));
    p(&d, 0).props.clone()
}

#[test]
fn alignment_values() {
    use Align::*;
    let align = |v: &str| props(&format!(r#"<w:jc w:val="{v}"/>"#)).align;
    assert_eq!(
        [
            "left",
            "start",
            "center",
            "right",
            "end",
            "both",
            "distribute"
        ]
        .map(align),
        [Left, Left, Center, Right, Right, Justify, Distribute]
    );
}

#[test]
fn indents_in_twips_and_characters() {
    let a = props(r#"<w:ind w:left="720" w:right="360" w:firstLine="200"/>"#);
    assert_eq!(
        (a.indent_left, a.indent_right, a.first_line),
        (720, 360, 200)
    );
    let b = props(r#"<w:ind w:start="100" w:end="50" w:hanging="300"/>"#);
    assert_eq!(
        (b.indent_left, b.indent_right, b.first_line),
        (100, 50, -300)
    );
    // Ten and a half points: a character is 210 twips.
    let styles = r#"<w:docDefaults><w:rPrDefault><w:rPr><w:sz w:val="21"/></w:rPr></w:rPrDefault></w:docDefaults>"#;
    let d = doc(r#"<w:p><w:pPr><w:ind w:firstLineChars="200" w:firstLine="999" w:leftChars="50" w:rightChars="0" w:right="40"/></w:pPr></w:p>"#)
        .styles(styles)
        .read();
    let c = &p(&d, 0).props;
    assert_eq!(c.first_line, 420);
    assert_eq!(c.indent_left, 105);
    // Zero characters do not win over twips.
    assert_eq!(c.indent_right, 40);
    let hanging = props(r#"<w:ind w:hangingChars="100"/>"#);
    assert_eq!(hanging.first_line, -200);
}

#[test]
fn spacing_in_twips_lines_and_automatic() {
    let a = props(r#"<w:spacing w:before="100" w:after="200" w:line="360" w:lineRule="auto"/>"#);
    assert_eq!((a.space_before, a.space_after), (100, 200));
    assert_eq!(a.line, LineSpacing::Multiple(1.5));
    let b = props(r#"<w:spacing w:line="300" w:lineRule="atLeast"/>"#);
    assert_eq!(b.line, LineSpacing::AtLeast(300));
    let c = props(r#"<w:spacing w:line="280" w:lineRule="exact"/>"#);
    assert_eq!(c.line, LineSpacing::Exactly(280));
    let d = props(r#"<w:spacing w:line="480"/>"#);
    assert_eq!(d.line, LineSpacing::Multiple(2.0));
    // Lines are 240 twips without a grid.
    let e = props(r#"<w:spacing w:beforeLines="50" w:afterLines="100"/>"#);
    assert_eq!((e.space_before, e.space_after), (120, 240));
    let f = props(r#"<w:spacing w:before="1" w:beforeAutospacing="1" w:afterAutospacing="1"/>"#);
    assert_eq!((f.space_before, f.space_after), (280, 280));
}

#[test]
fn a_line_is_the_pitch_of_the_sections_grid() {
    let section =
        |grid: &str| format!(r#"<w:sectPr><w:pgSz w:w="11906" w:h="16838"/>{grid}</w:sectPr>"#);
    let para = r#"<w:p><w:pPr><w:spacing w:beforeLines="100" w:afterLines="50"/></w:pPr></w:p>"#;
    let d = read(&format!(
        r#"{para}<w:p><w:pPr>{}</w:pPr></w:p>{para}{}"#,
        section(r#"<w:docGrid w:type="lines" w:linePitch="312"/>"#),
        section("")
    ));
    // The first section has a grid, the second has none.
    assert_eq!(
        (p(&d, 0).props.space_before, p(&d, 0).props.space_after),
        (312, 156)
    );
    assert_eq!(
        (p(&d, 2).props.space_before, p(&d, 2).props.space_after),
        (240, 120)
    );
}

#[test]
fn flags_and_outline_levels() {
    let a = props(
        r#"<w:keepNext/><w:keepLines/><w:pageBreakBefore/><w:contextualSpacing/><w:widowControl w:val="0"/><w:snapToGrid w:val="0"/><w:outlineLvl w:val="1"/>"#,
    );
    assert!(a.keep_next && a.keep_lines && a.page_break_before && a.contextual_spacing);
    assert!(!a.widow_control && !a.snap_to_grid);
    assert_eq!(a.outline_level, Some(1));
    let b = props("");
    assert!(b.widow_control && b.snap_to_grid && b.outline_level.is_none());
    assert_eq!(props(r#"<w:outlineLvl w:val="9"/>"#).outline_level, None);
}

#[test]
fn borders_between_and_shading() {
    let a = props(
        r#"<w:pBdr><w:top w:val="single" w:sz="8" w:space="2" w:color="FF0000"/><w:left w:val="double" w:sz="4"/><w:bottom w:val="nil"/><w:right w:val="dashed" w:sz="12"/><w:between w:val="dotted" w:sz="4" w:color="auto"/></w:pBdr><w:shd w:val="clear" w:color="auto" w:fill="FFFF00"/>"#,
    );
    let [left, top, right, bottom] = a.borders;
    assert_eq!(left.unwrap().style, BorderStyle::Double);
    let top = top.unwrap();
    assert_eq!(
        (top.style, top.width, top.space, top.color),
        (BorderStyle::Single, 1.0, 2.0, Some(rgb(0xFF0000)))
    );
    assert_eq!(right.unwrap().width, 1.5);
    assert_eq!(bottom, None);
    assert_eq!(a.border_between.unwrap().style, BorderStyle::Dotted);
    assert_eq!(a.shading, Some(rgb(0xFFFF00)));
}

#[test]
fn tab_stops_are_sorted_and_cleared_stops_are_gone() {
    let styles = r#"<w:style w:type="paragraph" w:default="1" w:styleId="Normal"><w:name w:val="Normal"/><w:pPr><w:tabs><w:tab w:val="left" w:pos="720"/><w:tab w:val="right" w:leader="dot" w:pos="9000"/></w:tabs></w:pPr></w:style>"#;
    let d = doc(r#"<w:p><w:pPr><w:tabs><w:tab w:val="clear" w:pos="720"/><w:tab w:val="decimal" w:leader="hyphen" w:pos="4000"/><w:tab w:val="center" w:leader="underscore" w:pos="2000"/><w:tab w:val="bar" w:pos="500"/><w:tab w:val="left" w:leader="middleDot" w:pos="6000"/></w:tabs></w:pPr></w:p>"#)
        .styles(styles)
        .read();
    let tabs = &p(&d, 0).props.tabs;
    let summary: Vec<(i32, TabAlign, TabLeader)> = tabs
        .iter()
        .map(|t| (t.position, t.align, t.leader))
        .collect();
    assert_eq!(
        summary,
        [
            (500, TabAlign::Bar, TabLeader::None),
            (2000, TabAlign::Center, TabLeader::Underscore),
            (4000, TabAlign::Decimal, TabLeader::Hyphens),
            (6000, TabAlign::Left, TabLeader::MiddleDots),
            (9000, TabAlign::Right, TabLeader::Dots),
        ]
    );
}

#[test]
fn a_paragraph_style_gives_its_properties_below_direct_ones() {
    let styles = r#"<w:docDefaults><w:pPrDefault><w:pPr><w:spacing w:after="100" w:line="240" w:lineRule="auto"/></w:pPr></w:pPrDefault></w:docDefaults>
<w:style w:type="paragraph" w:default="1" w:styleId="Normal"><w:name w:val="Normal"/><w:pPr><w:jc w:val="both"/></w:pPr></w:style>
<w:style w:type="paragraph" w:styleId="H"><w:name w:val="H"/><w:basedOn w:val="Normal"/><w:pPr><w:keepNext/><w:spacing w:before="300"/><w:jc w:val="center"/><w:outlineLvl w:val="0"/></w:pPr></w:style>"#;
    let d = doc(r#"<w:p><w:pPr><w:pStyle w:val="H"/><w:spacing w:after="0"/></w:pPr></w:p><w:p/><w:p><w:pPr><w:pStyle w:val="Missing"/></w:pPr></w:p>"#)
        .styles(styles)
        .read();
    let heading = &p(&d, 0).props;
    assert_eq!(heading.style.as_deref(), Some("H"));
    assert_eq!(heading.align, Align::Center);
    assert_eq!((heading.space_before, heading.space_after), (300, 0));
    assert!(heading.keep_next);
    assert_eq!(heading.outline_level, Some(0));
    let plain = &p(&d, 1).props;
    assert_eq!(plain.style, None);
    assert_eq!(plain.align, Align::Justify);
    assert_eq!(plain.space_after, 100);
    // An unknown style is read as the default style.
    assert_eq!(p(&d, 2).props.align, Align::Justify);
}

// ---------------------------------------------------------------------------
// The style sheet
// ---------------------------------------------------------------------------

const SHEET: &str = r#"<w:docDefaults><w:rPrDefault><w:rPr><w:sz w:val="22"/></w:rPr></w:rPrDefault><w:pPrDefault><w:pPr><w:spacing w:after="160"/></w:pPr></w:pPrDefault></w:docDefaults>
<w:style w:type="paragraph" w:default="1" w:styleId="Normal"><w:name w:val="Normal"/><w:qFormat/></w:style>
<w:style w:type="paragraph" w:styleId="H1"><w:name w:val="heading 1"/><w:basedOn w:val="Normal"/><w:next w:val="Normal"/><w:uiPriority w:val="9"/><w:qFormat/><w:pPr><w:keepNext/><w:spacing w:before="240"/></w:pPr><w:rPr><w:b/><w:sz w:val="32"/></w:rPr></w:style>
<w:style w:type="paragraph" w:styleId="H2"><w:name w:val="heading 2"/><w:basedOn w:val="H1"/><w:rPr><w:sz w:val="26"/></w:rPr></w:style>
<w:style w:type="paragraph" w:styleId="Old"><w:name w:val="old"/><w:semiHidden/></w:style>
<w:style w:type="paragraph" w:styleId="Gone"><w:name w:val="gone"/><w:hidden/></w:style>
<w:style w:type="character" w:styleId="Strong"><w:name w:val="Strong"/><w:rPr><w:b/></w:rPr></w:style>
<w:style w:type="table" w:styleId="T"><w:name w:val="T"/></w:style>
<w:style w:type="numbering" w:styleId="NL"><w:name w:val="NL"/></w:style>
<w:style w:type="paragraph" w:styleId="A"><w:name w:val="A"/><w:basedOn w:val="B"/><w:rPr><w:i/></w:rPr></w:style>
<w:style w:type="paragraph" w:styleId="B"><w:name w:val="B"/><w:basedOn w:val="A"/><w:rPr><w:u w:val="single"/></w:rPr></w:style>
<w:style w:type="paragraph" w:styleId="Self"><w:name w:val="Self"/><w:basedOn w:val="Self"/><w:rPr><w:strike/></w:rPr></w:style>"#;

#[test]
fn every_style_is_resolved_through_its_chain() {
    let d = doc("<w:p/>").styles(SHEET).read();
    let sheet = &d.styles;
    assert_eq!(sheet.default_style.as_deref(), Some("Normal"));
    assert_eq!(sheet.default_run.size, 11.0);
    assert_eq!(sheet.default_para.space_after, 160);
    let h2 = sheet.get("H2").unwrap();
    assert_eq!(h2.kind, StyleKind::Paragraph);
    assert_eq!(h2.based_on.as_deref(), Some("H1"));
    assert_eq!(h2.run.size, 13.0);
    assert!(h2.run.bold);
    assert!(h2.para.keep_next);
    assert_eq!((h2.para.space_before, h2.para.space_after), (240, 160));
    let h1 = sheet.get("H1").unwrap();
    assert_eq!(h1.next.as_deref(), Some("Normal"));
    assert_eq!(h1.name, "heading 1");
    assert!(h1.quick && h1.priority == 9 && !h1.hidden);
    // Unmentioned: not quick, priority 99.
    let old = sheet.get("Old").unwrap();
    assert!(old.hidden && !old.quick && old.priority == 99);
    assert!(sheet.get("Gone").unwrap().hidden);
    let strong = sheet.get("Strong").unwrap();
    assert_eq!(strong.kind, StyleKind::Character);
    // A character style: the document defaults with what it sets.
    assert!(strong.run.bold);
    assert_eq!(strong.run.size, 11.0);
    assert_eq!(sheet.get("T").unwrap().kind, StyleKind::Table);
    assert_eq!(sheet.get("NL").unwrap().kind, StyleKind::Numbering);
    let names: Vec<&str> = sheet.paragraph_styles().iter().map(|s| &*s.id).collect();
    assert!(names.contains(&"H1") && !names.contains(&"Old") && !names.contains(&"Gone"));
}

#[test]
fn styles_that_lead_back_to_themselves_do_not_loop() {
    let d = doc(r#"<w:p><w:pPr><w:pStyle w:val="A"/></w:pPr><w:r><w:t>x</w:t></w:r></w:p><w:p><w:pPr><w:pStyle w:val="Self"/></w:pPr></w:p>"#)
        .styles(SHEET)
        .read();
    let a = &p(&d, 0).base;
    assert!(a.italic && a.underline.is_some());
    assert!(p(&d, 1).base.strike);
    assert!(d.styles.get("B").unwrap().run.underline.is_some());
}

#[test]
fn a_style_part_without_a_default_style_leaves_the_defaults() {
    let d = doc("<w:p/>")
        .styles(r#"<w:style w:type="paragraph" w:styleId="X"><w:name w:val="X"/></w:style>"#)
        .read();
    assert_eq!(d.styles.default_style, None);
    assert_eq!(d.styles.paragraph_look(Some("X")).1.size, 10.0);
}

// ---------------------------------------------------------------------------
// Lists
// ---------------------------------------------------------------------------

fn lvl(ilvl: u32, fmt: &str, text: &str, extra: &str) -> String {
    format!(
        r#"<w:lvl w:ilvl="{ilvl}"><w:start w:val="1"/><w:numFmt w:val="{fmt}"/><w:lvlText w:val="{text}"/><w:lvlJc w:val="left"/>{extra}</w:lvl>"#
    )
}

fn item(num: u32, level: u32) -> String {
    format!(
        r#"<w:p><w:pPr><w:numPr><w:ilvl w:val="{level}"/><w:numId w:val="{num}"/></w:numPr></w:pPr><w:r><w:t>i</w:t></w:r></w:p>"#
    )
}

fn labels(d: &Document) -> Vec<Option<String>> {
    d.paragraphs()
        .iter()
        .map(|(_, p)| p.label.as_ref().map(|l| l.text.clone()))
        .collect()
}

#[test]
fn levels_are_read_with_their_format_text_suffix_and_indents() {
    let numbering = format!(
        r#"<w:abstractNum w:abstractNumId="7">{}{}{}</w:abstractNum><w:num w:numId="3"><w:abstractNumId w:val="7"/></w:num>"#,
        lvl(0, "upperRoman", "%1)", r#"<w:suff w:val="space"/><w:pPr><w:ind w:left="600" w:hanging="300"/></w:pPr><w:rPr><w:b/><w:color w:val="FF0000"/></w:rPr>"#)
            .replace(r#"<w:lvlJc w:val="left"/><w:suff"#, r#"<w:suff"#),
        lvl(1, "chineseCounting", "第%2章", r#"<w:isLgl/>"#),
        lvl(2, "frobnicate", "%3", ""),
    );
    let d = doc(&format!("{}{}{}", item(3, 0), item(3, 1), item(3, 2)))
        .numbering(&numbering)
        .read();
    let list = d.numbering.get(3).expect("list 3");
    assert_eq!(list.definition, 7);
    assert_eq!(list.levels.len(), 3);
    let first = &list.levels[0];
    assert_eq!(first.format, NumberFormat::UpperRoman);
    assert_eq!(first.text, "%1)");
    assert_eq!(first.suffix, Suffix::Space);
    assert_eq!((first.indent_left, first.first_line), (600, -300));
    assert_eq!(first.look.bold, Some(true));
    assert_eq!(first.look.color, Some(Some(rgb(0xFF0000))));
    // Legal numbering shows its numbers in decimal.
    assert!(list.levels[1].legal);
    assert_eq!(
        list.levels[2].format,
        NumberFormat::Other("frobnicate".to_owned())
    );
    // The label's look is the paragraph mark's with the level's patch.
    let label = p(&d, 0).label.clone().unwrap();
    assert_eq!(label.text, "I)");
    assert!(label.look.bold && label.look.color == Some(rgb(0xFF0000)));
    assert_eq!(label.suffix, Suffix::Space);
    assert_eq!(
        labels(&d),
        [Some("I)".into()), Some("第1章".into()), Some("1".into())]
    );
    // The level's indents are in the paragraph's properties.
    assert_eq!(
        (p(&d, 0).props.indent_left, p(&d, 0).props.first_line),
        (600, -300)
    );
}

#[test]
fn every_number_format_name_is_mapped() {
    let names = [
        ("decimal", NumberFormat::Decimal),
        ("decimalZero", NumberFormat::DecimalZero),
        ("lowerLetter", NumberFormat::LowerLetter),
        ("upperLetter", NumberFormat::UpperLetter),
        ("lowerRoman", NumberFormat::LowerRoman),
        ("upperRoman", NumberFormat::UpperRoman),
        ("bullet", NumberFormat::Bullet),
        ("none", NumberFormat::None),
        ("decimalEnclosedCircle", NumberFormat::DecimalEnclosedCircle),
        ("decimalFullWidth", NumberFormat::DecimalFullWidth),
        ("chineseCounting", NumberFormat::ChineseCounting),
        ("japaneseCounting", NumberFormat::ChineseCounting),
        ("chineseCountingThousand", NumberFormat::ChineseCounting),
        ("taiwaneseCounting", NumberFormat::ChineseCounting),
        ("chineseLegalSimplified", NumberFormat::ChineseLegal),
        ("ideographDigital", NumberFormat::IdeographDigital),
        ("japaneseDigitalTenThousand", NumberFormat::IdeographDigital),
        ("ideographTraditional", NumberFormat::IdeographTraditional),
        ("ideographZodiac", NumberFormat::IdeographZodiac),
        ("aiueoFullWidth", NumberFormat::Aiueo),
        ("aiueo", NumberFormat::Aiueo),
        ("irohaFullWidth", NumberFormat::Iroha),
        ("iroha", NumberFormat::Iroha),
        ("ordinal", NumberFormat::Ordinal),
    ];
    let levels: String = names
        .iter()
        .enumerate()
        .map(|(i, (n, _))| lvl(i as u32 % 9, n, "%1", ""))
        .collect();
    // Nine levels at most: one definition per three names.
    let mut defs = String::new();
    let mut nums = String::new();
    for (k, chunk) in names.chunks(8).enumerate() {
        let lv: String = chunk
            .iter()
            .enumerate()
            .map(|(i, (n, _))| lvl(i as u32, n, "%1", ""))
            .collect();
        defs += &format!(r#"<w:abstractNum w:abstractNumId="{k}">{lv}</w:abstractNum>"#);
        nums += &format!(
            r#"<w:num w:numId="{}"><w:abstractNumId w:val="{k}"/></w:num>"#,
            k + 1
        );
    }
    let _ = levels;
    let d = doc("<w:p/>").numbering(&format!("{defs}{nums}")).read();
    for (k, chunk) in names.chunks(8).enumerate() {
        let list = d.numbering.get(k as u32 + 1).unwrap();
        for (i, (_, expected)) in chunk.iter().enumerate() {
            assert_eq!(&list.levels[i].format, expected, "{k}/{i}");
        }
    }
}

#[test]
fn start_and_overrides() {
    let numbering = format!(
        r#"<w:abstractNum w:abstractNumId="0">{}{}</w:abstractNum>
<w:num w:numId="1"><w:abstractNumId w:val="0"/></w:num>
<w:num w:numId="2"><w:abstractNumId w:val="0"/><w:lvlOverride w:ilvl="0"><w:startOverride w:val="9"/></w:lvlOverride><w:lvlOverride w:ilvl="1"><w:lvl w:ilvl="1"><w:start w:val="1"/><w:numFmt w:val="upperLetter"/><w:lvlText w:val="[%2]"/></w:lvl></w:lvlOverride></w:num>"#,
        lvl(0, "decimal", "%1.", "").replace(r#"<w:start w:val="1"/>"#, r#"<w:start w:val="3"/>"#),
        lvl(1, "decimal", "%1.%2", "")
    );
    let d = doc(&format!(
        "{}{}{}{}{}",
        item(1, 0),
        item(1, 0),
        item(2, 0),
        item(2, 1),
        item(2, 1)
    ))
    .numbering(&numbering)
    .read();
    assert_eq!(d.numbering.get(2).unwrap().levels[0].restart, Some(9));
    assert_eq!(d.numbering.get(1).unwrap().levels[0].restart, None);
    assert_eq!(
        labels(&d),
        ["3.", "4.", "9.", "[A]", "[B]"].map(|s| Some(s.to_owned()))
    );
}

#[test]
fn bullets_in_symbol_fonts_become_unicode_without_the_font() {
    let numbering = format!(
        r#"<w:abstractNum w:abstractNumId="0">{}{}{}</w:abstractNum><w:num w:numId="1"><w:abstractNumId w:val="0"/></w:num>"#,
        lvl(
            0,
            "bullet",
            "&#xF0B7;",
            r#"<w:rPr><w:rFonts w:ascii="Symbol" w:hAnsi="Symbol" w:hint="default"/></w:rPr>"#
        ),
        lvl(
            1,
            "bullet",
            "&#xF0A7;",
            r#"<w:rPr><w:rFonts w:ascii="Wingdings" w:hAnsi="Wingdings"/><w:color w:val="00AA00"/></w:rPr>"#
        ),
        lvl(
            2,
            "bullet",
            "o",
            r#"<w:rPr><w:rFonts w:ascii="Courier New" w:hAnsi="Courier New"/></w:rPr>"#
        ),
    );
    let d = doc(&format!("{}{}{}", item(1, 0), item(1, 1), item(1, 2)))
        .numbering(&numbering)
        .read();
    let list = d.numbering.get(1).unwrap();
    assert_eq!(list.levels[0].text, "•");
    assert_eq!(list.levels[0].look.font, None);
    assert_eq!(list.levels[1].text, "▪");
    assert_eq!(list.levels[1].look.font, None);
    assert_eq!(list.levels[1].look.color, Some(Some(rgb(0x00AA00))));
    assert_eq!(list.levels[2].text, "o");
    assert_eq!(
        list.levels[2].look.font,
        Some(Some(Arc::from("Courier New")))
    );
    assert_eq!(
        labels(&d),
        [Some("•".into()), Some("▪".into()), Some("o".into())]
    );
    assert_eq!(
        p(&d, 0).label.as_ref().unwrap().look.font,
        p(&d, 0).mark.font
    );
}

#[test]
fn a_list_may_be_defined_by_a_list_style() {
    let numbering = format!(
        r#"<w:abstractNum w:abstractNumId="0"><w:styleLink w:val="MyList"/>{}</w:abstractNum>
<w:abstractNum w:abstractNumId="1"><w:numStyleLink w:val="MyList"/></w:abstractNum>
<w:num w:numId="1"><w:abstractNumId w:val="0"/></w:num>
<w:num w:numId="2"><w:abstractNumId w:val="1"/></w:num>"#,
        lvl(0, "lowerLetter", "%1)", "")
    );
    let styles = r#"<w:style w:type="numbering" w:styleId="MyList"><w:name w:val="MyList"/><w:pPr><w:numPr><w:numId w:val="1"/></w:numPr></w:pPr></w:style>"#;
    let d = doc(&format!("{}{}", item(2, 0), item(2, 0)))
        .numbering(&numbering)
        .styles(styles)
        .read();
    assert_eq!(
        d.numbering.get(2).unwrap().levels[0].format,
        NumberFormat::LowerLetter
    );
    assert_eq!(labels(&d), [Some("a)".into()), Some("b)".into())]);
}

#[test]
fn links_between_list_definitions_may_form_a_loop() {
    let numbering = r#"<w:abstractNum w:abstractNumId="0"><w:numStyleLink w:val="L1"/></w:abstractNum>
<w:abstractNum w:abstractNumId="1"><w:numStyleLink w:val="L0"/></w:abstractNum>
<w:num w:numId="1"><w:abstractNumId w:val="0"/></w:num><w:num w:numId="2"><w:abstractNumId w:val="1"/></w:num>"#;
    let styles = r#"<w:style w:type="numbering" w:styleId="L0"><w:name w:val="L0"/><w:pPr><w:numPr><w:numId w:val="1"/></w:numPr></w:pPr></w:style><w:style w:type="numbering" w:styleId="L1"><w:name w:val="L1"/><w:pPr><w:numPr><w:numId w:val="2"/></w:numPr></w:pPr></w:style>"#;
    let d = doc(&item(1, 0)).numbering(numbering).styles(styles).read();
    assert_eq!(d.plain_text(), "i");
}

#[test]
fn num_id_zero_removes_a_list_and_a_style_may_bring_one() {
    let numbering = format!(
        r#"<w:abstractNum w:abstractNumId="0">{}</w:abstractNum><w:num w:numId="1"><w:abstractNumId w:val="0"/></w:num>"#,
        lvl(
            0,
            "decimal",
            "%1.",
            r#"<w:pPr><w:ind w:left="700" w:hanging="350"/></w:pPr>"#
        )
    );
    let styles = r#"<w:style w:type="paragraph" w:styleId="Num"><w:name w:val="Num"/><w:pPr><w:numPr><w:numId w:val="1"/></w:numPr></w:pPr></w:style>
<w:style w:type="paragraph" w:styleId="NumInd"><w:name w:val="NumInd"/><w:pPr><w:numPr><w:numId w:val="1"/></w:numPr><w:ind w:left="1000" w:firstLine="0"/></w:pPr></w:style>"#;
    let d = doc(
        r#"<w:p><w:pPr><w:pStyle w:val="Num"/></w:pPr></w:p>
<w:p><w:pPr><w:pStyle w:val="Num"/><w:numPr><w:numId w:val="0"/></w:numPr></w:pPr></w:p>
<w:p><w:pPr><w:pStyle w:val="NumInd"/></w:pPr></w:p>
<w:p><w:pPr><w:pStyle w:val="NumInd"/><w:numPr><w:ilvl w:val="0"/><w:numId w:val="1"/></w:numPr></w:pPr></w:p>
<w:p><w:pPr><w:pStyle w:val="Num"/><w:ind w:left="50"/></w:pPr></w:p>"#,
    )
    .numbering(&numbering)
    .styles(styles)
    .read();
    assert_eq!(
        labels(&d),
        [
            Some("1.".into()),
            None,
            Some("2.".into()),
            Some("3.".into()),
            Some("4.".into())
        ]
    );
    let ind = |i: usize| (p(&d, i).props.indent_left, p(&d, i).props.first_line);
    // The list's indents where the paragraph sets none.
    assert_eq!(ind(0), (700, -350));
    assert_eq!(p(&d, 1).props.list, None);
    // A list from the style: the style's own indent wins over the level's.
    assert_eq!(ind(2), (1000, 0));
    // A list set on the paragraph itself: the level's indents win over the style's.
    assert_eq!(ind(3), (700, -350));
    // Direct indents win over everything.
    assert_eq!(p(&d, 4).props.indent_left, 50);
    assert_eq!(p(&d, 4).props.first_line, -350);
}

#[test]
fn a_paragraph_on_a_list_that_does_not_exist_is_not_on_a_list() {
    let d = doc(&item(9, 0)).read();
    assert_eq!(p(&d, 0).props.list, None);
    assert_eq!(labels(&d), [None]);
}
