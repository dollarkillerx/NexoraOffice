//! A rich, valid .docx written the way Word writes one, for the DOCX
//! reader's tests and for looking at in other programs
//! (`cargo run -p aster-ooxml --example make_docx_sample -- out.docx`).
//!
//! Three sections: A4 upright with a line grid, headers and footers; a
//! continuous two-column one; and a landscape one that ends the document.

#![allow(dead_code)]

use std::io::{Cursor, Write};

use zip::write::SimpleFileOptions;
use zip::{CompressionMethod, ZipWriter};

const W: &str = "http://schemas.openxmlformats.org/wordprocessingml/2006/main";
const R: &str = "http://schemas.openxmlformats.org/officeDocument/2006/relationships";
const REL: &str = "http://schemas.openxmlformats.org/officeDocument/2006/relationships";

/// The namespaces a Word document declares on its root.
fn namespaces() -> String {
    format!(
        r#"xmlns:w="{W}" xmlns:r="{R}" xmlns:wp="http://schemas.openxmlformats.org/drawingml/2006/wordprocessingDrawing" xmlns:a="http://schemas.openxmlformats.org/drawingml/2006/main" xmlns:pic="http://schemas.openxmlformats.org/drawingml/2006/picture" xmlns:mc="http://schemas.openxmlformats.org/markup-compatibility/2006" xmlns:wps="http://schemas.microsoft.com/office/word/2010/wordprocessingShape" xmlns:v="urn:schemas-microsoft-com:vml" xmlns:o="urn:schemas-microsoft-com:office:office" xmlns:w10="urn:schemas-microsoft-com:office:word" xmlns:w14="http://schemas.microsoft.com/office/word/2010/wordml" xmlns:wp14="http://schemas.microsoft.com/office/word/2010/wordprocessingDrawing" mc:Ignorable="w14 wp14""#
    )
}

const XML_DECL: &str = "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\n";

fn crc32(data: &[u8]) -> u32 {
    let mut crc = !0u32;
    for byte in data {
        crc ^= u32::from(*byte);
        for _ in 0..8 {
            crc = if crc & 1 != 0 {
                (crc >> 1) ^ 0xEDB8_8320
            } else {
                crc >> 1
            };
        }
    }
    !crc
}

fn chunk(kind: &[u8; 4], data: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    out.extend((data.len() as u32).to_be_bytes());
    out.extend(kind);
    out.extend(data);
    let mut checked = kind.to_vec();
    checked.extend(data);
    out.extend(crc32(&checked).to_be_bytes());
    out
}

/// A small RGB PNG (a gradient), written with stored deflate blocks.
pub fn png(width: u32, height: u32) -> Vec<u8> {
    let mut raw = Vec::new();
    for y in 0..height {
        raw.push(0);
        for x in 0..width {
            raw.extend([
                (x * 255 / width.max(1)) as u8,
                (y * 255 / height.max(1)) as u8,
                160,
            ]);
        }
    }
    let mut z = vec![0x78, 0x01];
    let blocks: Vec<&[u8]> = raw.chunks(65_535).collect();
    for (i, block) in blocks.iter().enumerate() {
        z.push(u8::from(i + 1 == blocks.len()));
        z.extend((block.len() as u16).to_le_bytes());
        z.extend((!(block.len() as u16)).to_le_bytes());
        z.extend(*block);
    }
    let (mut a, mut b) = (1u32, 0u32);
    for byte in &raw {
        a = (a + u32::from(*byte)) % 65_521;
        b = (b + a) % 65_521;
    }
    z.extend(((b << 16) | a).to_be_bytes());
    let mut header = Vec::new();
    header.extend(width.to_be_bytes());
    header.extend(height.to_be_bytes());
    header.extend([8, 2, 0, 0, 0]);
    let mut out = vec![0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A];
    out.extend(chunk(b"IHDR", &header));
    out.extend(chunk(b"IDAT", &z));
    out.extend(chunk(b"IEND", &[]));
    out
}

fn content_types() -> String {
    let ct = "application/vnd.openxmlformats-officedocument.wordprocessingml";
    format!(
        r#"{XML_DECL}<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/><Default Extension="xml" ContentType="application/xml"/><Default Extension="png" ContentType="image/png"/><Override PartName="/word/document.xml" ContentType="{ct}.document.main+xml"/><Override PartName="/word/styles.xml" ContentType="{ct}.styles+xml"/><Override PartName="/word/numbering.xml" ContentType="{ct}.numbering+xml"/><Override PartName="/word/settings.xml" ContentType="{ct}.settings+xml"/><Override PartName="/word/footnotes.xml" ContentType="{ct}.footnotes+xml"/><Override PartName="/word/endnotes.xml" ContentType="{ct}.endnotes+xml"/><Override PartName="/word/comments.xml" ContentType="{ct}.comments+xml"/><Override PartName="/word/header1.xml" ContentType="{ct}.header+xml"/><Override PartName="/word/header2.xml" ContentType="{ct}.header+xml"/><Override PartName="/word/footer1.xml" ContentType="{ct}.footer+xml"/><Override PartName="/word/theme/theme1.xml" ContentType="application/vnd.openxmlformats-officedocument.theme+xml"/><Override PartName="/docProps/core.xml" ContentType="application/vnd.openxmlformats-package.core-properties+xml"/></Types>"#
    )
}

fn root_rels() -> String {
    format!(
        r#"{XML_DECL}<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="{REL}/officeDocument" Target="word/document.xml"/><Relationship Id="rId2" Type="http://schemas.openxmlformats.org/package/2006/relationships/metadata/core-properties" Target="docProps/core.xml"/></Relationships>"#
    )
}

fn document_rels() -> String {
    let rel = |id: &str, kind: &str, target: &str| {
        format!(r#"<Relationship Id="{id}" Type="{REL}/{kind}" Target="{target}"/>"#)
    };
    format!(
        r#"{XML_DECL}<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">{}{}{}{}{}{}{}{}{}{}{}<Relationship Id="rId20" Type="{REL}/hyperlink" Target="https://example.org/aster?a=1&amp;b=2" TargetMode="External"/></Relationships>"#,
        rel("rId1", "styles", "styles.xml"),
        rel("rId2", "numbering", "numbering.xml"),
        rel("rId3", "settings", "settings.xml"),
        rel("rId4", "theme", "theme/theme1.xml"),
        rel("rId5", "footnotes", "footnotes.xml"),
        rel("rId6", "endnotes", "endnotes.xml"),
        rel("rId7", "comments", "comments.xml"),
        rel("rId8", "header", "header1.xml"),
        rel("rId9", "footer", "footer1.xml"),
        rel("rId10", "header", "header2.xml"),
        rel("rId11", "image", "media/image1.png"),
    )
}

fn theme() -> String {
    format!(
        r#"{XML_DECL}<a:theme xmlns:a="http://schemas.openxmlformats.org/drawingml/2006/main" name="Office Theme"><a:themeElements><a:clrScheme name="Office"><a:dk1><a:sysClr val="windowText" lastClr="000000"/></a:dk1><a:lt1><a:sysClr val="window" lastClr="FFFFFF"/></a:lt1><a:dk2><a:srgbClr val="44546A"/></a:dk2><a:lt2><a:srgbClr val="E7E6E6"/></a:lt2><a:accent1><a:srgbClr val="4472C4"/></a:accent1><a:accent2><a:srgbClr val="ED7D31"/></a:accent2><a:accent3><a:srgbClr val="A5A5A5"/></a:accent3><a:accent4><a:srgbClr val="FFC000"/></a:accent4><a:accent5><a:srgbClr val="5B9BD5"/></a:accent5><a:accent6><a:srgbClr val="70AD47"/></a:accent6><a:hlink><a:srgbClr val="0563C1"/></a:hlink><a:folHlink><a:srgbClr val="954F72"/></a:folHlink></a:clrScheme><a:fontScheme name="Office"><a:majorFont><a:latin typeface="Calibri Light"/><a:ea typeface=""/><a:cs typeface=""/><a:font script="Jpan" typeface="游ゴシック Light"/><a:font script="Hans" typeface="黑体"/><a:font script="Hant" typeface="新細明體"/><a:font script="Hang" typeface="맑은 고딕"/></a:majorFont><a:minorFont><a:latin typeface="Calibri"/><a:ea typeface=""/><a:cs typeface=""/><a:font script="Jpan" typeface="游明朝"/><a:font script="Hans" typeface="宋体"/><a:font script="Hant" typeface="新細明體"/><a:font script="Hang" typeface="맑은 고딕"/></a:minorFont></a:fontScheme><a:fmtScheme name="Office"><a:fillStyleLst><a:solidFill><a:schemeClr val="phClr"/></a:solidFill><a:solidFill><a:schemeClr val="phClr"/></a:solidFill><a:solidFill><a:schemeClr val="phClr"/></a:solidFill></a:fillStyleLst><a:lnStyleLst><a:ln w="6350"><a:solidFill><a:schemeClr val="phClr"/></a:solidFill></a:ln><a:ln w="12700"><a:solidFill><a:schemeClr val="phClr"/></a:solidFill></a:ln><a:ln w="19050"><a:solidFill><a:schemeClr val="phClr"/></a:solidFill></a:ln></a:lnStyleLst><a:effectStyleLst><a:effectStyle><a:effectLst/></a:effectStyle><a:effectStyle><a:effectLst/></a:effectStyle><a:effectStyle><a:effectLst/></a:effectStyle></a:effectStyleLst><a:bgFillStyleLst><a:solidFill><a:schemeClr val="phClr"/></a:solidFill><a:solidFill><a:schemeClr val="phClr"/></a:solidFill><a:solidFill><a:schemeClr val="phClr"/></a:solidFill></a:bgFillStyleLst></a:fmtScheme></a:themeElements></a:theme>"#
    )
}

fn settings() -> String {
    format!(
        r#"{XML_DECL}<w:settings xmlns:w="{W}"><w:zoom w:percent="100"/><w:defaultTabStop w:val="420"/><w:characterSpacingControl w:val="compressPunctuation"/><w:themeFontLang w:val="en-US" w:eastAsia="zh-CN"/><w:compat><w:compatSetting w:name="compatibilityMode" w:uri="http://schemas.microsoft.com/office/word" w:val="15"/></w:compat></w:settings>"#
    )
}

fn styles() -> String {
    format!(
        r#"{XML_DECL}<w:styles xmlns:w="{W}"><w:docDefaults><w:rPrDefault><w:rPr><w:rFonts w:asciiTheme="minorHAnsi" w:eastAsiaTheme="minorEastAsia" w:hAnsiTheme="minorHAnsi" w:cstheme="minorBidi"/><w:kern w:val="2"/><w:sz w:val="21"/><w:szCs w:val="21"/><w:lang w:val="en-US" w:eastAsia="zh-CN" w:bidi="ar-SA"/></w:rPr></w:rPrDefault><w:pPrDefault><w:pPr><w:spacing w:after="120" w:line="276" w:lineRule="auto"/></w:pPr></w:pPrDefault></w:docDefaults>
<w:style w:type="paragraph" w:default="1" w:styleId="Normal"><w:name w:val="Normal"/><w:qFormat/><w:pPr><w:widowControl w:val="0"/><w:jc w:val="both"/></w:pPr></w:style>
<w:style w:type="character" w:default="1" w:styleId="DefaultParagraphFont"><w:name w:val="Default Paragraph Font"/><w:uiPriority w:val="1"/><w:semiHidden/><w:unhideWhenUsed/></w:style>
<w:style w:type="table" w:default="1" w:styleId="TableNormal"><w:name w:val="Normal Table"/><w:uiPriority w:val="99"/><w:semiHidden/><w:unhideWhenUsed/><w:tblPr><w:tblInd w:w="0" w:type="dxa"/><w:tblCellMar><w:top w:w="0" w:type="dxa"/><w:left w:w="108" w:type="dxa"/><w:bottom w:w="0" w:type="dxa"/><w:right w:w="108" w:type="dxa"/></w:tblCellMar></w:tblPr></w:style>
<w:style w:type="numbering" w:default="1" w:styleId="NoList"><w:name w:val="No List"/><w:uiPriority w:val="99"/><w:semiHidden/><w:unhideWhenUsed/></w:style>
<w:style w:type="paragraph" w:styleId="Title"><w:name w:val="Title"/><w:basedOn w:val="Normal"/><w:next w:val="Normal"/><w:uiPriority w:val="10"/><w:qFormat/><w:pPr><w:jc w:val="center"/><w:spacing w:before="240" w:after="240"/></w:pPr><w:rPr><w:rFonts w:asciiTheme="majorHAnsi" w:eastAsiaTheme="majorEastAsia" w:hAnsiTheme="majorHAnsi"/><w:b/><w:sz w:val="44"/></w:rPr></w:style>
<w:style w:type="paragraph" w:styleId="Heading1"><w:name w:val="heading 1"/><w:basedOn w:val="Normal"/><w:next w:val="Normal"/><w:uiPriority w:val="9"/><w:qFormat/><w:pPr><w:keepNext/><w:keepLines/><w:spacing w:before="340" w:after="330" w:line="578" w:lineRule="auto"/><w:outlineLvl w:val="0"/></w:pPr><w:rPr><w:b/><w:bCs/><w:color w:val="2F5496" w:themeColor="accent1" w:themeShade="BF"/><w:kern w:val="44"/><w:sz w:val="44"/><w:szCs w:val="44"/></w:rPr></w:style>
<w:style w:type="paragraph" w:styleId="Heading2"><w:name w:val="heading 2"/><w:basedOn w:val="Normal"/><w:next w:val="Normal"/><w:uiPriority w:val="9"/><w:unhideWhenUsed/><w:qFormat/><w:pPr><w:keepNext/><w:keepLines/><w:spacing w:before="260" w:after="260" w:line="416" w:lineRule="auto"/><w:outlineLvl w:val="1"/></w:pPr><w:rPr><w:rFonts w:asciiTheme="majorHAnsi" w:eastAsiaTheme="majorEastAsia" w:hAnsiTheme="majorHAnsi"/><w:b/><w:bCs/><w:sz w:val="32"/><w:szCs w:val="32"/></w:rPr></w:style>
<w:style w:type="paragraph" w:styleId="Heading3"><w:name w:val="heading 3"/><w:basedOn w:val="Normal"/><w:next w:val="Normal"/><w:uiPriority w:val="9"/><w:unhideWhenUsed/><w:qFormat/><w:pPr><w:keepNext/><w:keepLines/><w:spacing w:before="260" w:after="260" w:line="416" w:lineRule="auto"/><w:outlineLvl w:val="2"/></w:pPr><w:rPr><w:b/><w:bCs/><w:sz w:val="30"/><w:szCs w:val="30"/></w:rPr></w:style>
<w:style w:type="paragraph" w:styleId="ListParagraph"><w:name w:val="List Paragraph"/><w:basedOn w:val="Normal"/><w:uiPriority w:val="34"/><w:qFormat/><w:pPr><w:ind w:firstLineChars="200" w:firstLine="420"/></w:pPr></w:style>
<w:style w:type="paragraph" w:styleId="Header"><w:name w:val="header"/><w:basedOn w:val="Normal"/><w:uiPriority w:val="99"/><w:unhideWhenUsed/><w:pPr><w:pBdr><w:bottom w:val="single" w:sz="6" w:space="1" w:color="auto"/></w:pBdr><w:tabs><w:tab w:val="center" w:pos="4153"/><w:tab w:val="right" w:pos="8306"/></w:tabs><w:snapToGrid w:val="0"/><w:jc w:val="center"/></w:pPr><w:rPr><w:sz w:val="18"/><w:szCs w:val="18"/></w:rPr></w:style>
<w:style w:type="paragraph" w:styleId="Footer"><w:name w:val="footer"/><w:basedOn w:val="Normal"/><w:uiPriority w:val="99"/><w:unhideWhenUsed/><w:pPr><w:tabs><w:tab w:val="center" w:pos="4153"/><w:tab w:val="right" w:pos="8306"/></w:tabs><w:snapToGrid w:val="0"/><w:jc w:val="left"/></w:pPr><w:rPr><w:sz w:val="18"/><w:szCs w:val="18"/></w:rPr></w:style>
<w:style w:type="paragraph" w:styleId="FootnoteText"><w:name w:val="footnote text"/><w:basedOn w:val="Normal"/><w:uiPriority w:val="99"/><w:semiHidden/><w:unhideWhenUsed/><w:pPr><w:snapToGrid w:val="0"/><w:jc w:val="left"/></w:pPr><w:rPr><w:sz w:val="18"/><w:szCs w:val="18"/></w:rPr></w:style>
<w:style w:type="character" w:styleId="FootnoteReference"><w:name w:val="footnote reference"/><w:basedOn w:val="DefaultParagraphFont"/><w:uiPriority w:val="99"/><w:semiHidden/><w:unhideWhenUsed/><w:rPr><w:vertAlign w:val="superscript"/></w:rPr></w:style>
<w:style w:type="character" w:styleId="Hyperlink"><w:name w:val="Hyperlink"/><w:basedOn w:val="DefaultParagraphFont"/><w:uiPriority w:val="99"/><w:unhideWhenUsed/><w:rPr><w:color w:val="0563C1" w:themeColor="hyperlink"/><w:u w:val="single"/></w:rPr></w:style>
<w:style w:type="character" w:styleId="CommentReference"><w:name w:val="annotation reference"/><w:basedOn w:val="DefaultParagraphFont"/><w:uiPriority w:val="99"/><w:semiHidden/><w:unhideWhenUsed/><w:rPr><w:sz w:val="21"/><w:szCs w:val="21"/></w:rPr></w:style>
<w:style w:type="table" w:styleId="TableGrid"><w:name w:val="Table Grid"/><w:basedOn w:val="TableNormal"/><w:uiPriority w:val="39"/><w:pPr><w:spacing w:after="0" w:line="240" w:lineRule="auto"/></w:pPr><w:tblPr><w:tblBorders><w:top w:val="single" w:sz="4" w:space="0" w:color="auto"/><w:left w:val="single" w:sz="4" w:space="0" w:color="auto"/><w:bottom w:val="single" w:sz="4" w:space="0" w:color="auto"/><w:right w:val="single" w:sz="4" w:space="0" w:color="auto"/><w:insideH w:val="single" w:sz="4" w:space="0" w:color="auto"/><w:insideV w:val="single" w:sz="4" w:space="0" w:color="auto"/></w:tblBorders></w:tblPr></w:style>
<w:style w:type="table" w:styleId="SampleTable"><w:name w:val="Sample Table"/><w:basedOn w:val="TableGrid"/><w:uiPriority w:val="40"/><w:tblPr><w:tblStyleRowBandSize w:val="1"/></w:tblPr><w:tblStylePr w:type="firstRow"><w:rPr><w:b/><w:color w:val="FFFFFF"/></w:rPr><w:tcPr><w:shd w:val="clear" w:color="auto" w:fill="4472C4"/></w:tcPr></w:tblStylePr><w:tblStylePr w:type="band1Horz"><w:tcPr><w:shd w:val="clear" w:color="auto" w:fill="DEEAF6"/></w:tcPr></w:tblStylePr></w:style>
</w:styles>"#
    )
}

fn numbering() -> String {
    format!(
        r#"{XML_DECL}<w:numbering xmlns:w="{W}"><w:abstractNum w:abstractNumId="0"><w:multiLevelType w:val="multilevel"/><w:lvl w:ilvl="0"><w:start w:val="1"/><w:numFmt w:val="decimal"/><w:lvlText w:val="%1."/><w:lvlJc w:val="left"/><w:pPr><w:ind w:left="425" w:hanging="425"/></w:pPr></w:lvl><w:lvl w:ilvl="1"><w:start w:val="1"/><w:numFmt w:val="decimal"/><w:lvlText w:val="%1.%2"/><w:lvlJc w:val="left"/><w:pPr><w:ind w:left="992" w:hanging="567"/></w:pPr><w:rPr><w:b/></w:rPr></w:lvl><w:lvl w:ilvl="2"><w:start w:val="1"/><w:numFmt w:val="lowerLetter"/><w:lvlText w:val="(%3)"/><w:lvlJc w:val="left"/><w:pPr><w:ind w:left="1418" w:hanging="567"/></w:pPr></w:lvl></w:abstractNum>
<w:abstractNum w:abstractNumId="1"><w:multiLevelType w:val="hybridMultilevel"/><w:lvl w:ilvl="0"><w:start w:val="1"/><w:numFmt w:val="bullet"/><w:lvlText w:val="&#xF0B7;"/><w:lvlJc w:val="left"/><w:pPr><w:ind w:left="840" w:hanging="420"/></w:pPr><w:rPr><w:rFonts w:ascii="Symbol" w:hAnsi="Symbol" w:hint="default"/></w:rPr></w:lvl><w:lvl w:ilvl="1"><w:start w:val="1"/><w:numFmt w:val="bullet"/><w:lvlText w:val="o"/><w:lvlJc w:val="left"/><w:pPr><w:ind w:left="1260" w:hanging="420"/></w:pPr><w:rPr><w:rFonts w:ascii="Courier New" w:hAnsi="Courier New" w:cs="Courier New" w:hint="default"/></w:rPr></w:lvl><w:lvl w:ilvl="2"><w:start w:val="1"/><w:numFmt w:val="bullet"/><w:lvlText w:val="&#xF0A7;"/><w:lvlJc w:val="left"/><w:pPr><w:ind w:left="1680" w:hanging="420"/></w:pPr><w:rPr><w:rFonts w:ascii="Wingdings" w:hAnsi="Wingdings" w:hint="default"/></w:rPr></w:lvl></w:abstractNum>
<w:num w:numId="1"><w:abstractNumId w:val="0"/></w:num><w:num w:numId="2"><w:abstractNumId w:val="1"/></w:num><w:num w:numId="3"><w:abstractNumId w:val="0"/><w:lvlOverride w:ilvl="0"><w:startOverride w:val="5"/></w:lvlOverride></w:num></w:numbering>"#
    )
}

fn footnotes() -> String {
    format!(
        r#"{XML_DECL}<w:footnotes xmlns:w="{W}"><w:footnote w:type="separator" w:id="-1"><w:p><w:pPr><w:spacing w:after="0" w:line="240" w:lineRule="auto"/></w:pPr><w:r><w:separator/></w:r></w:p></w:footnote><w:footnote w:type="continuationSeparator" w:id="0"><w:p><w:pPr><w:spacing w:after="0" w:line="240" w:lineRule="auto"/></w:pPr><w:r><w:continuationSeparator/></w:r></w:p></w:footnote><w:footnote w:id="2"><w:p><w:pPr><w:pStyle w:val="FootnoteText"/></w:pPr><w:r><w:rPr><w:rStyle w:val="FootnoteReference"/></w:rPr><w:footnoteRef/></w:r><w:r><w:t xml:space="preserve"> The footnote text, 脚注。</w:t></w:r></w:p></w:footnote></w:footnotes>"#
    )
}

fn endnotes() -> String {
    format!(
        r#"{XML_DECL}<w:endnotes xmlns:w="{W}"><w:endnote w:type="separator" w:id="-1"><w:p><w:r><w:separator/></w:r></w:p></w:endnote><w:endnote w:type="continuationSeparator" w:id="0"><w:p><w:r><w:continuationSeparator/></w:r></w:p></w:endnote><w:endnote w:id="1"><w:p><w:r><w:endnoteRef/></w:r><w:r><w:t xml:space="preserve"> An endnote.</w:t></w:r></w:p></w:endnote></w:endnotes>"#
    )
}

fn comments() -> String {
    format!(
        r#"{XML_DECL}<w:comments xmlns:w="{W}"><w:comment w:id="0" w:author="Reviewer" w:date="2026-01-01T00:00:00Z" w:initials="R"><w:p><w:r><w:t>Check this.</w:t></w:r></w:p></w:comment></w:comments>"#
    )
}

/// A complex field: begin, code, separate, result, end, each in its own run.
fn field(code: &str, result: &str) -> String {
    format!(
        r#"<w:r><w:fldChar w:fldCharType="begin"/></w:r><w:r><w:instrText xml:space="preserve"> {code} </w:instrText></w:r><w:r><w:fldChar w:fldCharType="separate"/></w:r><w:r><w:rPr><w:noProof/></w:rPr><w:t>{result}</w:t></w:r><w:r><w:fldChar w:fldCharType="end"/></w:r>"#
    )
}

fn header1() -> String {
    format!(
        r#"{XML_DECL}<w:hdr {}><w:p><w:pPr><w:pStyle w:val="Header"/></w:pPr><w:r><w:t>Aster Office</w:t></w:r><w:r><w:tab/></w:r><w:r><w:t xml:space="preserve">样本 </w:t></w:r><w:r><w:tab/><w:t xml:space="preserve">Page </w:t></w:r>{}<w:r><w:t xml:space="preserve"> of </w:t></w:r>{}</w:p></w:hdr>"#,
        namespaces(),
        field("PAGE", "1"),
        field("NUMPAGES", "3"),
    )
}

fn header2() -> String {
    format!(
        r#"{XML_DECL}<w:hdr {}><w:p><w:pPr><w:pStyle w:val="Header"/></w:pPr><w:r><w:t>First page header</w:t></w:r></w:p></w:hdr>"#,
        namespaces()
    )
}

fn footer1() -> String {
    format!(
        r#"{XML_DECL}<w:ftr {}><w:p><w:pPr><w:pStyle w:val="Footer"/><w:jc w:val="center"/></w:pPr><w:r><w:t xml:space="preserve">- </w:t></w:r>{}<w:r><w:t xml:space="preserve"> / </w:t></w:r><w:fldSimple w:instr=" NUMPAGES "><w:r><w:rPr><w:noProof/></w:rPr><w:t>3</w:t></w:r></w:fldSimple><w:r><w:t xml:space="preserve"> -</w:t></w:r></w:p></w:ftr>"#,
        namespaces(),
        field("PAGE", "1"),
    )
}

/// A paragraph of one plain run.
fn para(text: &str) -> String {
    format!(r#"<w:p><w:r><w:t xml:space="preserve">{text}</w:t></w:r></w:p>"#)
}

fn styled(style: &str, text: &str) -> String {
    format!(r#"<w:p><w:pPr><w:pStyle w:val="{style}"/></w:pPr><w:r><w:t>{text}</w:t></w:r></w:p>"#)
}

fn item(num: u32, level: u32, text: &str) -> String {
    format!(
        r#"<w:p><w:pPr><w:pStyle w:val="ListParagraph"/><w:numPr><w:ilvl w:val="{level}"/><w:numId w:val="{num}"/></w:numPr><w:ind w:firstLineChars="0" w:firstLine="0"/></w:pPr><w:r><w:t>{text}</w:t></w:r></w:p>"#
    )
}

fn picture_inline() -> String {
    r#"<w:r><w:drawing><wp:inline distT="0" distB="0" distL="0" distR="0"><wp:extent cx="1143000" cy="762000"/><wp:effectExtent l="0" t="0" r="0" b="0"/><wp:docPr id="1" name="Picture 1" descr="A gradient"/><wp:cNvGraphicFramePr><a:graphicFrameLocks noChangeAspect="1"/></wp:cNvGraphicFramePr><a:graphic><a:graphicData uri="http://schemas.openxmlformats.org/drawingml/2006/picture"><pic:pic><pic:nvPicPr><pic:cNvPr id="1" name="image1.png"/><pic:cNvPicPr/></pic:nvPicPr><pic:blipFill><a:blip r:embed="rId11"/><a:srcRect l="10000" t="5000"/><a:stretch><a:fillRect/></a:stretch></pic:blipFill><pic:spPr><a:xfrm><a:off x="0" y="0"/><a:ext cx="1143000" cy="762000"/></a:xfrm><a:prstGeom prst="rect"><a:avLst/></a:prstGeom></pic:spPr></pic:pic></a:graphicData></a:graphic></wp:inline></w:drawing></w:r>"#.to_owned()
}

fn picture_floating() -> String {
    r#"<w:r><w:drawing><wp:anchor distT="0" distB="0" distL="114300" distR="114300" simplePos="0" relativeHeight="251658240" behindDoc="0" locked="0" layoutInCell="1" allowOverlap="1"><wp:simplePos x="0" y="0"/><wp:positionH relativeFrom="margin"><wp:align>right</wp:align></wp:positionH><wp:positionV relativeFrom="paragraph"><wp:posOffset>91440</wp:posOffset></wp:positionV><wp:extent cx="914400" cy="609600"/><wp:effectExtent l="0" t="0" r="0" b="0"/><wp:wrapSquare wrapText="bothSides"/><wp:docPr id="2" name="Picture 2" descr="Floating"/><wp:cNvGraphicFramePr/><a:graphic><a:graphicData uri="http://schemas.openxmlformats.org/drawingml/2006/picture"><pic:pic><pic:nvPicPr><pic:cNvPr id="2" name="image1.png"/><pic:cNvPicPr/></pic:nvPicPr><pic:blipFill><a:blip r:embed="rId11"/><a:stretch><a:fillRect/></a:stretch></pic:blipFill><pic:spPr><a:xfrm><a:off x="0" y="0"/><a:ext cx="914400" cy="609600"/></a:xfrm><a:prstGeom prst="rect"><a:avLst/></a:prstGeom></pic:spPr></pic:pic></a:graphicData></a:graphic></wp:anchor></w:drawing></w:r>"#.to_owned()
}

fn text_box() -> String {
    let inner = r#"<w:txbxContent><w:p><w:pPr><w:jc w:val="left"/></w:pPr><w:r><w:t>Text box 文本框</w:t></w:r></w:p><w:p><w:r><w:rPr><w:b/></w:rPr><w:t>Second paragraph</w:t></w:r></w:p></w:txbxContent>"#;
    format!(
        r##"<w:r><mc:AlternateContent><mc:Choice Requires="wps"><w:drawing><wp:anchor distT="0" distB="0" distL="114300" distR="114300" simplePos="0" relativeHeight="251659264" behindDoc="0" locked="0" layoutInCell="1" allowOverlap="1"><wp:simplePos x="0" y="0"/><wp:positionH relativeFrom="column"><wp:posOffset>1200000</wp:posOffset></wp:positionH><wp:positionV relativeFrom="paragraph"><wp:posOffset>50000</wp:posOffset></wp:positionV><wp:extent cx="2400000" cy="900000"/><wp:effectExtent l="0" t="0" r="0" b="0"/><wp:wrapTopAndBottom/><wp:docPr id="3" name="Text Box 3"/><wp:cNvGraphicFramePr/><a:graphic><a:graphicData uri="http://schemas.microsoft.com/office/word/2010/wordprocessingShape"><wps:wsp><wps:cNvSpPr txBox="1"/><wps:spPr><a:xfrm><a:off x="0" y="0"/><a:ext cx="2400000" cy="900000"/></a:xfrm><a:prstGeom prst="rect"><a:avLst/></a:prstGeom><a:solidFill><a:srgbClr val="FFF2CC"/></a:solidFill><a:ln w="12700"><a:solidFill><a:srgbClr val="C00000"/></a:solidFill></a:ln></wps:spPr><wps:txbx>{inner}</wps:txbx><wps:bodyPr rot="0" vert="horz" wrap="square" lIns="91440" tIns="45720" rIns="91440" bIns="45720" anchor="t" anchorCtr="0"><a:noAutofit/></wps:bodyPr></wps:wsp></a:graphicData></a:graphic></wp:anchor></w:drawing></mc:Choice><mc:Fallback><w:pict><v:shape id="TextBox3" o:spid="_x0000_s1027" type="#_x0000_t202" style="position:absolute;margin-left:94.5pt;margin-top:3.9pt;width:189pt;height:70.9pt;z-index:251659264;mso-position-horizontal-relative:text;mso-position-vertical-relative:text" fillcolor="#fff2cc" strokecolor="#c00000"><v:textbox><w:txbxContent><w:p><w:r><w:t>Text box 文本框</w:t></w:r></w:p></w:txbxContent></v:textbox><w10:wrap type="topAndBottom"/></v:shape></w:pict></mc:Fallback></mc:AlternateContent></w:r>"##
    )
}

fn section_one() -> String {
    r#"<w:p><w:pPr><w:sectPr><w:headerReference w:type="default" r:id="rId8"/><w:headerReference w:type="first" r:id="rId10"/><w:footerReference w:type="default" r:id="rId9"/><w:pgSz w:w="11906" w:h="16838"/><w:pgMar w:top="1440" w:right="1800" w:bottom="1440" w:left="1800" w:header="851" w:footer="992" w:gutter="0"/><w:cols w:space="425"/><w:titlePg/><w:docGrid w:type="lines" w:linePitch="312"/></w:sectPr></w:pPr></w:p>"#.to_owned()
}

fn section_two() -> String {
    r#"<w:p><w:pPr><w:sectPr><w:type w:val="continuous"/><w:pgSz w:w="11906" w:h="16838"/><w:pgMar w:top="1440" w:right="1800" w:bottom="1440" w:left="1800" w:header="851" w:footer="992" w:gutter="0"/><w:cols w:num="2" w:space="425" w:sep="1"/><w:docGrid w:type="lines" w:linePitch="312"/></w:sectPr></w:pPr></w:p>"#.to_owned()
}

fn formatting_paragraph() -> String {
    let run = |props: &str, text: &str| {
        format!(r#"<w:r><w:rPr>{props}</w:rPr><w:t xml:space="preserve">{text}</w:t></w:r>"#)
    };
    let mut out = String::from(
        r#"<w:p><w:pPr><w:keepNext/><w:spacing w:before="120" w:after="60"/></w:pPr>"#,
    );
    out += &run("<w:b/>", "Bold ");
    out += &run("<w:i/>", "italic ");
    out += &run(r#"<w:u w:val="single"/>"#, "underline ");
    out += &run(r#"<w:u w:val="double" w:color="FF0000"/>"#, "double ");
    out += &run(r#"<w:u w:val="wave"/>"#, "wavy ");
    out += &run("<w:strike/>", "strike ");
    out += &run(r#"<w:color w:val="C00000"/>"#, "red ");
    out += &run(r#"<w:highlight w:val="yellow"/>"#, "highlight ");
    out += &run(r#"<w:vertAlign w:val="superscript"/>"#, "sup ");
    out += &run(r#"<w:vertAlign w:val="subscript"/>"#, "sub ");
    out += &run(r#"<w:sz w:val="36"/><w:szCs w:val="36"/>"#, "18pt ");
    out += &run(
        r#"<w:rFonts w:ascii="Georgia" w:hAnsi="Georgia" w:eastAsia="MS Mincho" w:hint="eastAsia"/>"#,
        "日本語のフォント ",
    );
    out += &run(r#"<w:caps/><w:spacing w:val="20"/>"#, "caps ");
    out += &run(r#"<w:w w:val="150"/><w:position w:val="6"/>"#, "wide");
    out += "</w:p>";
    out
}

fn table() -> String {
    let nested = r#"<w:tbl><w:tblPr><w:tblStyle w:val="TableGrid"/><w:tblW w:w="1600" w:type="dxa"/><w:tblLayout w:type="fixed"/><w:tblLook w:val="04A0"/></w:tblPr><w:tblGrid><w:gridCol w:w="800"/><w:gridCol w:w="800"/></w:tblGrid><w:tr><w:tc><w:tcPr><w:tcW w:w="800" w:type="dxa"/></w:tcPr><w:p><w:r><w:t>n1</w:t></w:r></w:p></w:tc><w:tc><w:tcPr><w:tcW w:w="800" w:type="dxa"/></w:tcPr><w:p><w:r><w:t>n2</w:t></w:r></w:p></w:tc></w:tr></w:tbl>"#;
    let cell = |w: u32, props: &str, body: &str| {
        format!(r#"<w:tc><w:tcPr><w:tcW w:w="{w}" w:type="dxa"/>{props}</w:tcPr>{body}</w:tc>"#)
    };
    let text = |t: &str| format!(r#"<w:p><w:r><w:t>{t}</w:t></w:r></w:p>"#);
    let mut out = String::from(
        r#"<w:tbl><w:tblPr><w:tblStyle w:val="SampleTable"/><w:tblW w:w="9000" w:type="dxa"/><w:jc w:val="center"/><w:tblInd w:w="0" w:type="dxa"/><w:tblBorders><w:top w:val="single" w:sz="12" w:space="0" w:color="1F3864"/><w:insideH w:val="dashed" w:sz="4" w:space="0" w:color="808080"/></w:tblBorders><w:tblLayout w:type="fixed"/><w:tblLook w:val="04A0" w:firstRow="1" w:lastRow="0" w:firstColumn="1" w:lastColumn="0" w:noHBand="0" w:noVBand="1"/></w:tblPr><w:tblGrid><w:gridCol w:w="2000"/><w:gridCol w:w="2000"/><w:gridCol w:w="2400"/><w:gridCol w:w="2600"/></w:tblGrid>"#,
    );
    out += r#"<w:tr><w:trPr><w:cantSplit/><w:trHeight w:val="500" w:hRule="atLeast"/><w:tblHeader/></w:trPr>"#;
    out += &cell(2000, "", &text("姓名"));
    out += &cell(2000, "", &text("部門"));
    out += &cell(2400, "", &text("Score"));
    out += &cell(2600, "", &text("備考"));
    out += "</w:tr>";
    out += "<w:tr>";
    out += &cell(
        4000,
        r#"<w:gridSpan w:val="2"/><w:tcBorders><w:bottom w:val="double" w:sz="6" w:space="0" w:color="FF0000"/></w:tcBorders>"#,
        &text("Merged across"),
    );
    out += &cell(2400, r#"<w:vMerge w:val="restart"/>"#, &text("Merged down"));
    out += &cell(2600, "", &text("x"));
    out += "</w:tr>";
    out += "<w:tr><w:trPr><w:trHeight w:val=\"700\" w:hRule=\"exact\"/></w:trPr>";
    out += &cell(2000, "", &text("a"));
    out += &cell(
        2000,
        r#"<w:tcBorders><w:left w:val="nil"/></w:tcBorders><w:shd w:val="clear" w:color="auto" w:fill="FFC000"/><w:vAlign w:val="center"/>"#,
        &text("b"),
    );
    out += &cell(2400, r#"<w:vMerge/>"#, "<w:p/>");
    out += &cell(2600, "", &format!("{nested}<w:p/>"));
    out += "</w:tr>";
    out += "<w:tr>";
    out += &cell(2000, "", &text("c"));
    out += &cell(2000, "", &text("d"));
    out += &cell(2400, "", &text("e"));
    out += &cell(2600, "", &text("f"));
    out += "</w:tr></w:tbl>";
    out
}

fn body() -> String {
    let mut b = String::new();
    // Section one.
    b += &styled("Title", "Aster Office 样本文档");
    b += r#"<w:p><w:pPr><w:pStyle w:val="Heading1"/><w:spacing w:beforeLines="50" w:before="156"/></w:pPr><w:bookmarkStart w:id="0" w:name="intro"/><w:r><w:t>第一章 Introduction</w:t></w:r><w:bookmarkEnd w:id="0"/></w:p>"#;
    b += &styled("Heading2", "第二级标题 Heading two");
    b += &styled("Heading3", "見出し三 Heading three");
    b += r#"<w:p><w:pPr><w:ind w:firstLineChars="200" w:firstLine="420"/></w:pPr><w:r><w:rPr><w:rFonts w:hint="eastAsia"/></w:rPr><w:t>中文段落，首行缩进两个字符。</w:t></w:r><w:r><w:rPr><w:rFonts w:hint="eastAsia"/><w:lang w:eastAsia="ja-JP"/></w:rPr><w:t>日本語の文章もあります。</w:t></w:r><w:r><w:t xml:space="preserve"> The quick brown fox jumps over the lazy dog.</w:t></w:r></w:p>"#;
    b += &formatting_paragraph();
    b += &item(1, 0, "First item");
    b += &item(1, 1, "Nested item");
    b += &item(1, 2, "Third level");
    b += &item(1, 1, "Nested again");
    b += &item(1, 0, "Second item");
    b += &item(3, 0, "A list with a restart");
    b += &item(2, 0, "Bullet one");
    b += &item(2, 1, "Bullet two");
    b += &item(2, 2, "Bullet three");
    b += r#"<w:p><w:pPr><w:tabs><w:tab w:val="left" w:leader="dot" w:pos="2000"/><w:tab w:val="right" w:leader="dot" w:pos="6000"/></w:tabs><w:jc w:val="left"/></w:pPr><w:r><w:t>Chapter</w:t></w:r><w:r><w:tab/></w:r><w:r><w:t>12</w:t></w:r><w:r><w:tab/><w:t>34</w:t></w:r></w:p>"#;
    b += r#"<w:p><w:r><w:t xml:space="preserve">Ruby: </w:t></w:r><w:r><w:ruby><w:rubyPr><w:rubyAlign w:val="distributeSpace"/><w:hps w:val="10"/><w:hpsRaise w:val="20"/><w:hpsBaseText w:val="21"/><w:lid w:val="ja-JP"/></w:rubyPr><w:rt><w:r><w:rPr><w:sz w:val="10"/></w:rPr><w:t>かんじ</w:t></w:r></w:rt><w:rubyBase><w:r><w:t>漢字</w:t></w:r></w:rubyBase></w:ruby></w:r><w:r><w:t xml:space="preserve"> and a </w:t></w:r><w:hyperlink r:id="rId20" w:history="1"><w:r><w:rPr><w:rStyle w:val="Hyperlink"/></w:rPr><w:t>link</w:t></w:r></w:hyperlink><w:r><w:t xml:space="preserve">, a </w:t></w:r><w:hyperlink w:anchor="intro"><w:r><w:rPr><w:rStyle w:val="Hyperlink"/></w:rPr><w:t>jump</w:t></w:r></w:hyperlink><w:r><w:t>.</w:t></w:r></w:p>"#;
    b += r#"<w:p><w:r><w:t>Note</w:t></w:r><w:r><w:rPr><w:rStyle w:val="FootnoteReference"/></w:rPr><w:footnoteReference w:id="2"/></w:r><w:r><w:t xml:space="preserve"> and endnote</w:t></w:r><w:r><w:endnoteReference w:id="1"/></w:r><w:r><w:t xml:space="preserve">. </w:t></w:r><w:commentRangeStart w:id="0"/><w:r><w:t>commented</w:t></w:r><w:commentRangeEnd w:id="0"/><w:r><w:rPr><w:rStyle w:val="CommentReference"/></w:rPr><w:commentReference w:id="0"/></w:r><w:r><w:t xml:space="preserve"> </w:t></w:r><w:ins w:id="1" w:author="Reviewer" w:date="2026-01-01T00:00:00Z"><w:r><w:t>inserted</w:t></w:r></w:ins><w:r><w:t xml:space="preserve"> </w:t></w:r><w:del w:id="2" w:author="Reviewer" w:date="2026-01-01T00:00:00Z"><w:r><w:delText>deleted</w:delText></w:r></w:del><w:proofErr w:type="spellStart"/><w:r><w:t xml:space="preserve"> splt</w:t></w:r><w:proofErr w:type="spellEnd"/></w:p>"#;
    b += &format!(
        r#"<w:sdt><w:sdtPr><w:alias w:val="Control"/><w:id w:val="42"/></w:sdtPr><w:sdtContent>{}</w:sdtContent></w:sdt>"#,
        para("Inside a content control.")
    );
    b += r#"<w:p><w:r><w:t>Before the break</w:t></w:r><w:r><w:br w:type="page"/></w:r><w:r><w:t>After the break</w:t></w:r></w:p>"#;
    b += &section_one();

    // Section two: two columns.
    b += &format!(
        "<w:p><w:r><w:t xml:space=\"preserve\">Two columns. </w:t></w:r>{}{}<w:r><w:t xml:space=\"preserve\"> Text goes around the picture and the box. 文字围绕图片和文本框排列。</w:t></w:r></w:p>",
        picture_floating(),
        text_box()
    );
    b += &para(
        "The second paragraph of the two-column section. It has enough words to run over a line or two when it is laid out.",
    );
    b += &section_two();

    // Section three: landscape.
    b += &format!(
        "<w:p><w:r><w:t xml:space=\"preserve\">Inline picture: </w:t></w:r>{}</w:p>",
        picture_inline()
    );
    b += &table();
    b += &para("After the table.");
    b += r#"<w:sectPr><w:pgSz w:w="16838" w:h="11906" w:orient="landscape"/><w:pgMar w:top="1800" w:right="1440" w:bottom="1800" w:left="1440" w:header="851" w:footer="992" w:gutter="0"/><w:cols w:space="425"/></w:sectPr>"#;
    b
}

fn document() -> String {
    format!(
        "{XML_DECL}<w:document {}><w:body>{}</w:body></w:document>",
        namespaces(),
        body()
    )
}

fn core() -> String {
    format!(
        r#"{XML_DECL}<cp:coreProperties xmlns:cp="http://schemas.openxmlformats.org/package/2006/metadata/core-properties" xmlns:dc="http://purl.org/dc/elements/1.1/" xmlns:dcterms="http://purl.org/dc/terms/" xmlns:xsi="http://www.w3.org/2001/XMLSchema-instance"><dc:title>Aster Office sample</dc:title></cp:coreProperties>"#
    )
}

/// The parts of the sample, by package name.
pub fn parts() -> Vec<(String, Vec<u8>)> {
    let text = |name: &str, xml: String| (name.to_owned(), xml.into_bytes());
    vec![
        text("[Content_Types].xml", content_types()),
        text("_rels/.rels", root_rels()),
        text("docProps/core.xml", core()),
        text("word/document.xml", document()),
        text("word/_rels/document.xml.rels", document_rels()),
        text("word/styles.xml", styles()),
        text("word/numbering.xml", numbering()),
        text("word/settings.xml", settings()),
        text("word/theme/theme1.xml", theme()),
        text("word/footnotes.xml", footnotes()),
        text("word/endnotes.xml", endnotes()),
        text("word/comments.xml", comments()),
        text("word/header1.xml", header1()),
        text("word/header2.xml", header2()),
        text("word/footer1.xml", footer1()),
        ("word/media/image1.png".to_owned(), png(24, 16)),
    ]
}

/// A zip file of `parts`, deflated.
pub fn zip(parts: &[(String, Vec<u8>)]) -> Vec<u8> {
    let mut writer = ZipWriter::new(Cursor::new(Vec::new()));
    let options = SimpleFileOptions::default().compression_method(CompressionMethod::Deflated);
    for (name, data) in parts {
        writer
            .start_file(name.as_str(), options)
            .expect("start part");
        writer.write_all(data).expect("write part");
    }
    writer.finish().expect("finish zip").into_inner()
}

/// The sample as a .docx file.
pub fn sample() -> Vec<u8> {
    zip(&parts())
}
