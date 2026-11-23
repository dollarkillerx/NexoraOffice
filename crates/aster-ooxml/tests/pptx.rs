mod common;

use aster_ooxml::{ErrorKind, FormatCodec, Pptx};
use aster_slides::{Align, Anchor, Frame, Geometry, Role};
use common::{CONTENT_TYPES_HEAD, package, rels, root_rels};

const NS: &str = r#"xmlns:a="http://schemas.openxmlformats.org/drawingml/2006/main" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships" xmlns:p="http://schemas.openxmlformats.org/presentationml/2006/main""#;

fn content_types() -> String {
    format!(
        r#"{CONTENT_TYPES_HEAD}<Override PartName="/ppt/presentation.xml" ContentType="application/vnd.openxmlformats-officedocument.presentationml.presentation.main+xml"/></Types>"#
    )
}

fn master() -> String {
    format!(
        r#"<p:sldMaster {NS}><p:cSld><p:spTree>
  <p:nvGrpSpPr><p:cNvPr id="1" name=""/><p:cNvGrpSpPr/><p:nvPr/></p:nvGrpSpPr><p:grpSpPr/>
  <p:sp><p:nvSpPr><p:cNvPr id="2" name="Title"/><p:cNvSpPr/><p:nvPr><p:ph type="title"/></p:nvPr></p:nvSpPr>
    <p:spPr><a:xfrm><a:off x="100" y="100"/><a:ext cx="9000" cy="1000"/></a:xfrm></p:spPr>
    <p:txBody><a:bodyPr anchor="ctr"/><a:lstStyle/><a:p><a:r><a:t>Master title</a:t></a:r></a:p></p:txBody></p:sp>
  <p:sp><p:nvSpPr><p:cNvPr id="3" name="Body"/><p:cNvSpPr/><p:nvPr><p:ph type="body" idx="1"/></p:nvPr></p:nvSpPr>
    <p:spPr><a:xfrm><a:off x="100" y="1200"/><a:ext cx="9000" cy="5000"/></a:xfrm></p:spPr>
    <p:txBody><a:bodyPr lIns="0"/><a:lstStyle/><a:p><a:r><a:t>Body</a:t></a:r></a:p></p:txBody></p:sp>
  <p:sp><p:nvSpPr><p:cNvPr id="4" name="Logo"/><p:cNvSpPr/><p:nvPr/></p:nvSpPr>
    <p:spPr><a:xfrm><a:off x="8000" y="6000"/><a:ext cx="500" cy="300"/></a:xfrm><a:prstGeom prst="rect"/></p:spPr>
    <p:style><a:lnRef idx="1"/><a:fillRef idx="1"/></p:style></p:sp>
</p:spTree></p:cSld>
<p:txStyles>
  <p:titleStyle><a:lvl1pPr algn="ctr"><a:defRPr sz="4400" b="1"/></a:lvl1pPr></p:titleStyle>
  <p:bodyStyle>
    <a:lvl1pPr marL="228600" indent="-228600"><a:buChar char="•"/><a:defRPr sz="2800"/></a:lvl1pPr>
    <a:lvl2pPr marL="685800"><a:buChar char="–"/><a:defRPr sz="2400"/></a:lvl2pPr>
  </p:bodyStyle>
  <p:otherStyle><a:lvl1pPr><a:defRPr sz="1800"/></a:lvl1pPr></p:otherStyle>
</p:txStyles></p:sldMaster>"#
    )
}

fn layout() -> String {
    format!(
        r#"<p:sldLayout {NS}><p:cSld><p:spTree>
  <p:sp><p:nvSpPr><p:cNvPr id="2" name="Title"/><p:cNvSpPr/><p:nvPr><p:ph type="title"/></p:nvPr></p:nvSpPr>
    <p:spPr><a:xfrm><a:off x="200" y="300"/><a:ext cx="8000" cy="900"/></a:xfrm></p:spPr></p:sp>
  <p:sp><p:nvSpPr><p:cNvPr id="3" name="Content"/><p:cNvSpPr/><p:nvPr><p:ph idx="1"/></p:nvPr></p:nvSpPr>
    <p:spPr/><p:txBody><a:bodyPr/><a:lstStyle><a:lvl2pPr><a:defRPr sz="2000"/></a:lvl2pPr></a:lstStyle><a:p/></p:txBody></p:sp>
  <p:cxnSp><p:nvCxnSpPr><p:cNvPr id="4" name="Rule"/><p:cNvCxnSpPr/><p:nvPr/></p:nvCxnSpPr>
    <p:spPr><a:xfrm><a:off x="0" y="1250"/><a:ext cx="12000" cy="0"/></a:xfrm><a:prstGeom prst="line"/><a:ln><a:solidFill/></a:ln></p:spPr></p:cxnSp>
</p:spTree></p:cSld></p:sldLayout>"#
    )
}

fn slide_one() -> String {
    format!(
        r#"<p:sld {NS}><p:cSld><p:spTree>
  <p:nvGrpSpPr><p:cNvPr id="1" name=""/><p:cNvGrpSpPr/><p:nvPr/></p:nvGrpSpPr><p:grpSpPr/>
  <p:sp><p:nvSpPr><p:cNvPr id="2" name="Title 1"/><p:cNvSpPr/><p:nvPr><p:ph type="title"/></p:nvPr></p:nvSpPr><p:spPr/>
    <p:txBody><a:bodyPr/><a:lstStyle/><a:p><a:r><a:rPr lang="zh-CN"/><a:t>第三季度报告</a:t></a:r></a:p></p:txBody></p:sp>
  <p:sp><p:nvSpPr><p:cNvPr id="3" name="Content 2"/><p:cNvSpPr/><p:nvPr><p:ph idx="1"/></p:nvPr></p:nvSpPr><p:spPr/>
    <p:txBody><a:bodyPr><a:normAutofit fontScale="85000"/></a:bodyPr><a:lstStyle/>
      <a:p><a:r><a:t>売上は前年比 120%</a:t></a:r></a:p>
      <a:p><a:pPr lvl="1"/><a:r><a:t>東日本</a:t></a:r><a:br/><a:r><a:rPr i="1"/><a:t>西日本</a:t></a:r></a:p>
      <a:p><a:pPr><a:buNone/></a:pPr><a:r><a:rPr sz="1200" u="sng"/><a:t>注：概算</a:t></a:r></a:p>
      <a:p><a:endParaRPr sz="900"/></a:p>
      <a:p><a:pPr algn="r"><a:buAutoNum type="arabicPeriod"/></a:pPr><a:r><a:t>一</a:t></a:r></a:p>
      <a:p><a:pPr><a:buAutoNum type="arabicPeriod"/></a:pPr><a:r><a:t>二</a:t></a:r></a:p>
    </p:txBody></p:sp>
  <p:sp><p:nvSpPr><p:cNvPr id="4" name="TextBox"/><p:cNvSpPr txBox="1"/><p:nvPr/></p:nvSpPr>
    <p:spPr><a:xfrm rot="5400000" flipH="1"><a:off x="10" y="20"/><a:ext cx="300" cy="400"/></a:xfrm><a:prstGeom prst="rect"/></p:spPr>
    <p:txBody><a:bodyPr wrap="none" lIns="1" tIns="2" rIns="3" bIns="4" anchor="b"/><a:p><a:r><a:rPr sz="2000" b="1"/><a:t>A &amp; B</a:t></a:r><a:fld type="slidenum"><a:t>1</a:t></a:fld></a:p></p:txBody></p:sp>
  <p:grpSp><p:nvGrpSpPr><p:cNvPr id="5" name="Group"/><p:cNvGrpSpPr/><p:nvPr/></p:nvGrpSpPr>
    <p:grpSpPr><a:xfrm><a:off x="1000" y="2000"/><a:ext cx="2000" cy="1000"/><a:chOff x="0" y="0"/><a:chExt cx="1000" cy="1000"/></a:xfrm></p:grpSpPr>
    <p:sp><p:nvSpPr><p:cNvPr id="6" name="Oval"/><p:cNvSpPr/><p:nvPr/></p:nvSpPr>
      <p:spPr><a:xfrm><a:off x="500" y="500"/><a:ext cx="100" cy="200"/></a:xfrm><a:prstGeom prst="ellipse"/></p:spPr>
      <p:style><a:lnRef idx="2"/></p:style></p:sp>
    <p:grpSp><p:nvGrpSpPr><p:cNvPr id="7" name="Inner"/><p:cNvGrpSpPr/><p:nvPr/></p:nvGrpSpPr>
      <p:grpSpPr><a:xfrm><a:off x="0" y="0"/><a:ext cx="100" cy="100"/><a:chOff x="0" y="0"/><a:chExt cx="200" cy="200"/></a:xfrm></p:grpSpPr>
      <p:sp><p:nvSpPr><p:cNvPr id="8" name="Star"/><p:cNvSpPr/><p:nvPr/></p:nvSpPr>
        <p:spPr><a:xfrm><a:off x="100" y="100"/><a:ext cx="100" cy="100"/></a:xfrm><a:prstGeom prst="star5"/><a:ln><a:noFill/></a:ln></p:spPr>
        <p:style><a:lnRef idx="2"/></p:style></p:sp>
    </p:grpSp>
  </p:grpSp>
  <p:pic><p:nvPicPr><p:cNvPr id="9" name="Picture"/><p:cNvPicPr/><p:nvPr/></p:nvPicPr><p:blipFill/>
    <p:spPr><a:xfrm><a:off x="5000" y="5000"/><a:ext cx="100" cy="100"/></a:xfrm><a:prstGeom prst="rect"/></p:spPr></p:pic>
  <p:graphicFrame><p:nvGraphicFramePr><p:cNvPr id="10" name="Table"/><p:cNvGraphicFramePr/><p:nvPr/></p:nvGraphicFramePr>
    <p:xfrm><a:off x="6000" y="6000"/><a:ext cx="300" cy="300"/></p:xfrm>
    <a:graphic><a:graphicData uri="http://schemas.openxmlformats.org/drawingml/2006/table"><a:tbl/></a:graphicData></a:graphic></p:graphicFrame>
  <mc:AlternateContent xmlns:mc="http://schemas.openxmlformats.org/markup-compatibility/2006">
    <mc:Choice Requires="p14"><p:sp><p:nvSpPr><p:cNvPr id="11" name="New"/><p:cNvSpPr/><p:nvPr/></p:nvSpPr>
      <p:spPr><a:xfrm><a:off x="7" y="7"/><a:ext cx="7" cy="7"/></a:xfrm><a:prstGeom prst="roundRect"/></p:spPr></p:sp></mc:Choice>
    <mc:Fallback><p:sp><p:nvSpPr><p:cNvPr id="12" name="Old"/><p:cNvSpPr/><p:nvPr/></p:nvSpPr>
      <p:spPr><a:xfrm><a:off x="8" y="8"/><a:ext cx="8" cy="8"/></a:xfrm></p:spPr></p:sp></mc:Fallback>
  </mc:AlternateContent>
</p:spTree></p:cSld></p:sld>"#
    )
}

fn slide_two() -> String {
    format!(
        r#"<p:sld {NS} show="0" showMasterSp="0"><p:cSld><p:spTree>
  <p:sp><p:nvSpPr><p:cNvPr id="2" name="Title 1"/><p:cNvSpPr/><p:nvPr><p:ph type="ctrTitle"/></p:nvPr></p:nvSpPr><p:spPr/>
    <p:txBody><a:bodyPr/><a:p><a:endParaRPr/></a:p></p:txBody></p:sp>
  <p:sp><p:nvSpPr><p:cNvPr id="3" name="Free"/><p:cNvSpPr/><p:nvPr/></p:nvSpPr><p:spPr/>
    <p:txBody><a:p><a:r><a:t>no frame anywhere</a:t></a:r></a:p></p:txBody></p:sp>
</p:spTree></p:cSld></p:sld>"#
    )
}

fn sample() -> Vec<u8> {
    let presentation = format!(
        r#"<p:presentation {NS}>
  <p:sldMasterIdLst><p:sldMasterId id="2147483648" r:id="rId1"/></p:sldMasterIdLst>
  <p:sldIdLst><p:sldId id="256" r:id="rId3"/><p:sldId id="257" r:id="rId2"/></p:sldIdLst>
  <p:sldSz cx="12192000" cy="6858000"/><p:notesSz cx="6858000" cy="9144000"/>
  <p:defaultTextStyle><a:lvl1pPr><a:defRPr sz="1600"/></a:lvl1pPr></p:defaultTextStyle>
</p:presentation>"#
    );
    // Slide order comes from the list, not from the file names.
    let presentation_rels = rels(&[
        ("rId1", "slideMaster", "slideMasters/slideMaster1.xml"),
        ("rId2", "slide", "slides/slide2.xml"),
        ("rId3", "slide", "slides/slide1.xml"),
    ]);
    let to_layout = rels(&[("rId1", "slideLayout", "../slideLayouts/slideLayout1.xml")]);
    let to_master = rels(&[("rId1", "slideMaster", "../slideMasters/slideMaster1.xml")]);
    package(&[
        ("[Content_Types].xml", content_types().as_bytes()),
        ("_rels/.rels", root_rels("ppt/presentation.xml").as_bytes()),
        ("ppt/presentation.xml", presentation.as_bytes()),
        (
            "ppt/_rels/presentation.xml.rels",
            presentation_rels.as_bytes(),
        ),
        ("ppt/slides/slide1.xml", slide_one().as_bytes()),
        ("ppt/slides/_rels/slide1.xml.rels", to_layout.as_bytes()),
        ("ppt/slides/slide2.xml", slide_two().as_bytes()),
        ("ppt/slides/_rels/slide2.xml.rels", to_layout.as_bytes()),
        ("ppt/slideLayouts/slideLayout1.xml", layout().as_bytes()),
        (
            "ppt/slideLayouts/_rels/slideLayout1.xml.rels",
            to_master.as_bytes(),
        ),
        ("ppt/slideMasters/slideMaster1.xml", master().as_bytes()),
    ])
}

#[test]
fn slides_inherit_from_layout_and_master() {
    let presentation = Pptx.read(sample()).unwrap();
    assert_eq!(presentation.size, (12_192_000, 6_858_000));
    assert_eq!(presentation.slides.len(), 2);
    let slide = &presentation.slides[0];
    assert!(!slide.hidden);
    assert_eq!(slide.title().as_deref(), Some("第三季度报告"));

    // Master and layout shapes that are not placeholders come first.
    let inherited: Vec<_> = slide.shapes.iter().filter(|s| s.inherited).collect();
    assert_eq!(inherited.len(), 2);
    assert_eq!(inherited[0].frame, Frame::new(8000, 6000, 500, 300));
    assert!(inherited[0].outlined, "a styled master shape has a line");
    assert_eq!(inherited[1].geometry, Geometry::Line);
    assert!(inherited[1].outlined);

    let own: Vec<_> = slide.shapes.iter().filter(|s| !s.inherited).collect();
    // Title: frame from the layout, size and alignment from the master's
    // title style, anchor from the master placeholder.
    let title = own[0];
    assert_eq!(title.role, Role::Title);
    assert_eq!(title.frame, Frame::new(200, 300, 8000, 900));
    assert!(!title.outlined);
    let text = title.text.as_ref().unwrap();
    assert_eq!(text.anchor, Anchor::Middle);
    let paragraph = &text.paragraphs[0];
    assert_eq!(paragraph.align, Align::Center);
    assert_eq!(
        (paragraph.runs[0].size, paragraph.runs[0].bold),
        (44.0, true)
    );
    assert_eq!(paragraph.bullet, None);

    // Body: matched by index on the layout, frame from the master.
    let body = own[1];
    assert_eq!(body.role, Role::Body);
    assert_eq!(body.frame, Frame::new(100, 1200, 9000, 5000));
    let text = body.text.as_ref().unwrap();
    assert_eq!(text.insets[0], 0, "left inset from the master placeholder");
    assert!((text.font_scale - 0.85).abs() < 1e-6);
    let p = &text.paragraphs;
    assert_eq!(p[0].bullet.as_deref(), Some("•"));
    assert_eq!(p[0].runs[0].size, 28.0);
    assert_eq!((p[0].margin, p[0].indent), (228_600, -228_600));
    // Level 2: size from the layout's list style, bullet from the master.
    assert_eq!(p[1].level, 1);
    assert_eq!(p[1].bullet.as_deref(), Some("–"));
    assert_eq!(p[1].text(), "東日本\n西日本");
    assert!(p[1].runs.iter().all(|r| r.size == 20.0));
    assert!(p[1].runs.last().unwrap().italic);
    assert_eq!(p[2].bullet, None);
    assert!(p[2].runs[0].underline);
    assert_eq!(p[2].runs[0].size, 12.0);
    // An empty paragraph keeps its line height and gets no bullet.
    assert!(p[3].runs.is_empty());
    assert_eq!((p[3].end_size, p[3].bullet.as_deref()), (9.0, None));
    assert_eq!(p[4].bullet.as_deref(), Some("1."));
    assert_eq!(p[4].align, Align::Right);
    assert_eq!(p[5].bullet.as_deref(), Some("2."));

    // A text box: its own frame, transform and insets, no outline.
    let text_box = own[2];
    assert_eq!(text_box.role, Role::Shape);
    assert_eq!(text_box.frame, Frame::new(10, 20, 300, 400));
    assert_eq!((text_box.rotation, text_box.flip_h), (90.0, true));
    assert!(!text_box.outlined);
    let body = text_box.text.as_ref().unwrap();
    assert_eq!(body.insets, [1, 2, 3, 4]);
    assert_eq!((body.anchor, body.wrap), (Anchor::Bottom, false));
    assert_eq!(body.paragraphs[0].text(), "A & B1");
    // Not a placeholder: the master's "other" style.
    assert_eq!(body.paragraphs[0].runs[1].size, 18.0);

    // Groups: child coordinates scaled and moved, nested groups composed.
    let oval = own[3];
    assert_eq!(oval.geometry, Geometry::Ellipse);
    assert_eq!(oval.frame, Frame::new(2000, 2500, 200, 200));
    assert!(oval.outlined);
    let star = own[4];
    assert_eq!(star.geometry, Geometry::Other("star5".to_owned()));
    assert_eq!(star.frame, Frame::new(1100, 2050, 100, 50));
    assert!(!star.outlined, "an explicit no-line wins over the style");

    assert_eq!(own[5].role, Role::Picture);
    assert_eq!(own[6].role, Role::Table);
    assert_eq!(own[6].frame, Frame::new(6000, 6000, 300, 300));
    // Alternate content: the choice, not the fallback.
    assert_eq!(own[7].geometry, Geometry::RoundRect);
    assert_eq!(own.len(), 8);
}

#[test]
fn a_hidden_slide_without_master_shapes() {
    let presentation = Pptx.read(sample()).unwrap();
    let slide = &presentation.slides[1];
    assert!(slide.hidden);
    assert!(slide.shapes.iter().all(|s| !s.inherited));
    // The empty title has no text to show; the shape with no frame anywhere
    // is left out.
    assert_eq!(slide.shapes.len(), 1);
    assert_eq!(slide.shapes[0].text, None);
    assert_eq!(slide.title(), None);
}

#[test]
fn a_presentation_without_slides() {
    let presentation =
        format!(r#"<p:presentation {NS}><p:sldSz cx="9144000" cy="6858000"/></p:presentation>"#);
    let bytes = package(&[
        ("[Content_Types].xml", content_types().as_bytes()),
        ("_rels/.rels", root_rels("ppt/presentation.xml").as_bytes()),
        ("ppt/presentation.xml", presentation.as_bytes()),
    ]);
    let presentation = Pptx.read(bytes).unwrap();
    assert_eq!(presentation.size, (9_144_000, 6_858_000));
    assert!(presentation.slides.is_empty());
}

#[test]
fn many_slides_and_missing_parts() {
    let mut list = String::new();
    let mut entries = Vec::new();
    let mut parts: Vec<(String, Vec<u8>)> = Vec::new();
    for n in 1..=300 {
        list.push_str(&format!(r#"<p:sldId id="{}" r:id="rId{n}"/>"#, 255 + n));
        entries.push((format!("rId{n}"), format!("slides/slide{n}.xml")));
        // Every tenth slide's part is missing; the rest have no layout.
        if n % 10 != 0 {
            parts.push((
                format!("ppt/slides/slide{n}.xml"),
                format!(
                    r#"<p:sld {NS}><p:cSld><p:spTree><p:sp><p:nvSpPr><p:cNvPr id="2" name="t"/><p:cNvSpPr txBox="1"/><p:nvPr/></p:nvSpPr><p:spPr><a:xfrm><a:off x="0" y="0"/><a:ext cx="10" cy="10"/></a:xfrm></p:spPr><p:txBody><a:p><a:r><a:t>第{n}页</a:t></a:r></a:p></p:txBody></p:sp></p:spTree></p:cSld></p:sld>"#
                )
                .into_bytes(),
            ));
        }
    }
    let presentation =
        format!(r#"<p:presentation {NS}><p:sldIdLst>{list}</p:sldIdLst></p:presentation>"#);
    let entry_refs: Vec<(&str, &str, &str)> = entries
        .iter()
        .map(|(id, target)| (id.as_str(), "slide", target.as_str()))
        .collect();
    let presentation_rels = rels(&entry_refs);
    let content_types = content_types();
    let root = root_rels("ppt/presentation.xml");
    let mut all: Vec<(&str, &[u8])> = vec![
        ("[Content_Types].xml", content_types.as_bytes()),
        ("_rels/.rels", root.as_bytes()),
        ("ppt/presentation.xml", presentation.as_bytes()),
        (
            "ppt/_rels/presentation.xml.rels",
            presentation_rels.as_bytes(),
        ),
    ];
    all.extend(
        parts
            .iter()
            .map(|(name, data)| (name.as_str(), data.as_slice())),
    );
    let presentation = Pptx.read(package(&all)).unwrap();
    assert_eq!(presentation.size, aster_slides::DEFAULT_SIZE);
    assert_eq!(presentation.slides.len(), 270);
    assert_eq!(presentation.slides[9].title().as_deref(), Some("第11页"));
}

#[test]
fn a_workbook_is_not_a_presentation() {
    let bytes = package(&[
        (
            "[Content_Types].xml",
            format!(
                r#"{CONTENT_TYPES_HEAD}<Override PartName="/xl/workbook.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.sheet.main+xml"/></Types>"#
            )
            .as_bytes(),
        ),
        ("_rels/.rels", root_rels("xl/workbook.xml").as_bytes()),
        ("xl/workbook.xml", b"<workbook/>"),
    ]);
    assert_eq!(Pptx.read(bytes).unwrap_err().kind(), ErrorKind::WrongKind);
}
