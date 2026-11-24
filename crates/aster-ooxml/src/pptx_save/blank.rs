//! A new, empty presentation: 16:9, one title slide, a master with five
//! layouts (title slide, title and content, section header, title only,
//! blank) and an Office-like theme. Also the notes master and theme a
//! presentation gets when notes are first written into one that has none.

const NS: &str = r#"xmlns:a="http://schemas.openxmlformats.org/drawingml/2006/main" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships" xmlns:p="http://schemas.openxmlformats.org/presentationml/2006/main""#;
const DECL: &str = "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\n";
const PKG_REL: &str = "http://schemas.openxmlformats.org/package/2006/relationships";
const REL: &str = "http://schemas.openxmlformats.org/officeDocument/2006/relationships";
const CT: &str = "application/vnd.openxmlformats-officedocument";

const TREE_HEADER: &str = r#"<p:nvGrpSpPr><p:cNvPr id="1" name=""/><p:cNvGrpSpPr/><p:nvPr/></p:nvGrpSpPr><p:grpSpPr><a:xfrm><a:off x="0" y="0"/><a:ext cx="0" cy="0"/><a:chOff x="0" y="0"/><a:chExt cx="0" cy="0"/></a:xfrm></p:grpSpPr>"#;

/// The layouts' names, in the order the blank presentation lists them.
pub const LAYOUTS: usize = 5;

/// The theme: Office's colours and fonts, with fonts for Japanese,
/// Chinese and Korean text.
pub fn theme(name: &str) -> String {
    let fonts = |latin: &str, light: bool| {
        let l = if light { " Light" } else { "" };
        format!(
            r#"<a:latin typeface="{latin}" panose="020F0302020204030204"/><a:ea typeface=""/><a:cs typeface=""/><a:font script="Jpan" typeface="游ゴシック{l}"/><a:font script="Hang" typeface="맑은 고딕"/><a:font script="Hans" typeface="等线{l}"/><a:font script="Hant" typeface="新細明體"/>"#
        )
    };
    let solid = |extra: &str| {
        format!(r#"<a:solidFill><a:schemeClr val="phClr">{extra}</a:schemeClr></a:solidFill>"#)
    };
    let line = |w: u32| {
        format!(
            r#"<a:ln w="{w}" cap="flat" cmpd="sng" algn="ctr"><a:solidFill><a:schemeClr val="phClr"/></a:solidFill><a:prstDash val="solid"/><a:miter lim="800000"/></a:ln>"#
        )
    };
    format!(
        r#"{DECL}<a:theme xmlns:a="http://schemas.openxmlformats.org/drawingml/2006/main" name="{name}"><a:themeElements><a:clrScheme name="Office"><a:dk1><a:sysClr val="windowText" lastClr="000000"/></a:dk1><a:lt1><a:sysClr val="window" lastClr="FFFFFF"/></a:lt1><a:dk2><a:srgbClr val="44546A"/></a:dk2><a:lt2><a:srgbClr val="E7E6E6"/></a:lt2><a:accent1><a:srgbClr val="4472C4"/></a:accent1><a:accent2><a:srgbClr val="ED7D31"/></a:accent2><a:accent3><a:srgbClr val="A5A5A5"/></a:accent3><a:accent4><a:srgbClr val="FFC000"/></a:accent4><a:accent5><a:srgbClr val="5B9BD5"/></a:accent5><a:accent6><a:srgbClr val="70AD47"/></a:accent6><a:hlink><a:srgbClr val="0563C1"/></a:hlink><a:folHlink><a:srgbClr val="954F72"/></a:folHlink></a:clrScheme><a:fontScheme name="Office"><a:majorFont>{}</a:majorFont><a:minorFont>{}</a:minorFont></a:fontScheme><a:fmtScheme name="Office"><a:fillStyleLst>{}{}{}</a:fillStyleLst><a:lnStyleLst>{}{}{}</a:lnStyleLst><a:effectStyleLst><a:effectStyle><a:effectLst/></a:effectStyle><a:effectStyle><a:effectLst/></a:effectStyle><a:effectStyle><a:effectLst/></a:effectStyle></a:effectStyleLst><a:bgFillStyleLst>{}{}{}</a:bgFillStyleLst></a:fmtScheme></a:themeElements><a:objectDefaults/><a:extraClrSchemeLst/></a:theme>"#,
        fonts("Calibri Light", true),
        fonts("Calibri", false),
        solid(""),
        solid(r#"<a:tint val="50000"/><a:satMod val="300000"/>"#),
        solid(r#"<a:shade val="85000"/>"#),
        line(6350),
        line(12700),
        line(19050),
        solid(""),
        solid(r#"<a:tint val="95000"/><a:satMod val="170000"/>"#),
        solid(r#"<a:shade val="85000"/>"#),
    )
}

/// One level of a text style: margin, size, bullet.
fn level(n: usize, margin: i64, indent: i64, size: u32, bullet: bool, font: &str) -> String {
    let bullet = if bullet {
        r#"<a:buFont typeface="Arial" panose="020B0604020202020204"/><a:buChar char="•"/>"#
    } else {
        "<a:buNone/>"
    };
    let space = if n == 1 { "1000" } else { "500" };
    format!(
        r#"<a:lvl{n}pPr marL="{margin}" indent="{indent}" algn="l" defTabSz="914400" rtl="0" eaLnBrk="1" latinLnBrk="0" hangingPunct="1"><a:lnSpc><a:spcPct val="90000"/></a:lnSpc><a:spcBef><a:spcPts val="{space}"/></a:spcBef>{bullet}<a:defRPr sz="{size}" kern="1200"><a:solidFill><a:schemeClr val="tx1"/></a:solidFill><a:latin typeface="+{font}-lt"/><a:ea typeface="+{font}-ea"/><a:cs typeface="+{font}-cs"/></a:defRPr></a:lvl{n}pPr>"#
    )
}

fn plain_levels(tag: &str) -> String {
    let levels: String = (1..=9)
        .map(|n| {
            format!(
                r#"<a:lvl{n}pPr marL="{}" algn="l" defTabSz="914400" rtl="0" eaLnBrk="1" latinLnBrk="0" hangingPunct="1"><a:defRPr sz="1800" kern="1200"><a:solidFill><a:schemeClr val="tx1"/></a:solidFill><a:latin typeface="+mn-lt"/><a:ea typeface="+mn-ea"/><a:cs typeface="+mn-cs"/></a:defRPr></a:lvl{n}pPr>"#,
                (n - 1) * 457_200
            )
        })
        .collect();
    format!(r#"<{tag}><a:defPPr><a:defRPr lang="en-US"/></a:defPPr>{levels}</{tag}>"#)
}

#[allow(clippy::too_many_arguments)]
/// A placeholder: kind, index, its frame (`None` to inherit) and what is in
/// its list style and body properties.
fn placeholder(
    id: u32,
    name: &str,
    kind: &str,
    idx: Option<u32>,
    frame: Option<(i64, i64, i64, i64)>,
    body: &str,
    list: &str,
    text: &str,
) -> String {
    let kind = if kind.is_empty() {
        String::new()
    } else {
        format!(r#" type="{kind}""#)
    };
    let idx = idx.map_or(String::new(), |i| format!(r#" idx="{i}""#));
    let frame = frame.map_or("<p:spPr/>".to_owned(), |(x, y, cx, cy)| {
        format!(
            r#"<p:spPr><a:xfrm><a:off x="{x}" y="{y}"/><a:ext cx="{cx}" cy="{cy}"/></a:xfrm></p:spPr>"#
        )
    });
    let text = if text.is_empty() {
        r#"<a:p><a:endParaRPr lang="en-US"/></a:p>"#.to_owned()
    } else {
        format!(
            r#"<a:p><a:r><a:rPr lang="en-US"/><a:t>{text}</a:t></a:r><a:endParaRPr lang="en-US"/></a:p>"#
        )
    };
    format!(
        r#"<p:sp><p:nvSpPr><p:cNvPr id="{id}" name="{name}"/><p:cNvSpPr><a:spLocks noGrp="1"/></p:cNvSpPr><p:nvPr><p:ph{kind}{idx}/></p:nvPr></p:nvSpPr>{frame}<p:txBody>{body}<a:lstStyle>{list}</a:lstStyle>{text}</p:txBody></p:sp>"#
    )
}

fn master() -> String {
    let title = placeholder(
        2,
        "Title Placeholder 1",
        "title",
        None,
        Some((838_200, 365_125, 10_515_600, 1_325_563)),
        r#"<a:bodyPr vert="horz" lIns="91440" tIns="45720" rIns="91440" bIns="45720" rtlCol="0" anchor="ctr"><a:normAutofit/></a:bodyPr>"#,
        "",
        "Title",
    );
    let body = placeholder(
        3,
        "Text Placeholder 2",
        "body",
        Some(1),
        Some((838_200, 1_825_625, 10_515_600, 4_351_338)),
        r#"<a:bodyPr vert="horz" lIns="91440" tIns="45720" rIns="91440" bIns="45720" rtlCol="0"><a:normAutofit/></a:bodyPr>"#,
        "",
        "Text",
    );
    let small = |id: u32, name: &str, kind: &str, idx: u32, x: i64, w: i64, align: &str| {
        placeholder(
            id,
            name,
            kind,
            Some(idx),
            Some((x, 6_356_350, w, 365_125)),
            r#"<a:bodyPr vert="horz" lIns="91440" tIns="45720" rIns="91440" bIns="45720" rtlCol="0" anchor="ctr"/>"#,
            &format!(
                r#"<a:lvl1pPr algn="{align}"><a:defRPr sz="1200"><a:solidFill><a:schemeClr val="tx1"><a:tint val="75000"/></a:schemeClr></a:solidFill></a:defRPr></a:lvl1pPr>"#
            ),
            "",
        )
    };
    let body_style: String = [
        (1, 228_600, 2800),
        (2, 685_800, 2400),
        (3, 1_143_000, 2000),
        (4, 1_600_200, 1800),
        (5, 2_057_400, 1800),
        (6, 2_514_600, 1800),
        (7, 2_971_800, 1800),
        (8, 3_429_000, 1800),
        (9, 3_886_200, 1800),
    ]
    .iter()
    .map(|&(n, margin, size)| level(n, margin, -228_600, size, true, "mn"))
    .collect();
    let title_style = r#"<a:lvl1pPr algn="l" defTabSz="914400" rtl="0" eaLnBrk="1" latinLnBrk="0" hangingPunct="1"><a:lnSpc><a:spcPct val="90000"/></a:lnSpc><a:spcBef><a:spcPct val="0"/></a:spcBef><a:buNone/><a:defRPr sz="4400" kern="1200"><a:solidFill><a:schemeClr val="tx1"/></a:solidFill><a:latin typeface="+mj-lt"/><a:ea typeface="+mj-ea"/><a:cs typeface="+mj-cs"/></a:defRPr></a:lvl1pPr>"#;
    let layouts: String = (1..=LAYOUTS)
        .map(|n| {
            format!(
                r#"<p:sldLayoutId id="{}" r:id="rId{n}"/>"#,
                2_147_483_648u64 + n as u64
            )
        })
        .collect();
    format!(
        r#"{DECL}<p:sldMaster {NS}><p:cSld><p:bg><p:bgRef idx="1001"><a:schemeClr val="bg1"/></p:bgRef></p:bg><p:spTree>{TREE_HEADER}{title}{body}{}{}{}</p:spTree></p:cSld><p:clrMap bg1="lt1" tx1="dk1" bg2="lt2" tx2="dk2" accent1="accent1" accent2="accent2" accent3="accent3" accent4="accent4" accent5="accent5" accent6="accent6" hlink="hlink" folHlink="folHlink"/><p:sldLayoutIdLst>{layouts}</p:sldLayoutIdLst><p:txStyles><p:titleStyle>{title_style}</p:titleStyle><p:bodyStyle>{body_style}</p:bodyStyle>{}</p:txStyles></p:sldMaster>"#,
        small(4, "Date Placeholder 3", "dt", 2, 838_200, 2_743_200, "l"),
        small(
            5,
            "Footer Placeholder 4",
            "ftr",
            3,
            4_038_600,
            4_114_800,
            "ctr"
        ),
        small(
            6,
            "Slide Number Placeholder 5",
            "sldNum",
            4,
            8_610_600,
            2_743_200,
            "r"
        ),
        plain_levels("p:otherStyle"),
    )
}

/// The footer placeholders every layout repeats.
fn footers(first_id: u32) -> String {
    let empty = "<a:bodyPr/>";
    [
        ("Date Placeholder", "dt", 10),
        ("Footer Placeholder", "ftr", 11),
        ("Slide Number Placeholder", "sldNum", 12),
    ]
    .iter()
    .enumerate()
    .map(|(i, (name, kind, idx))| {
        let id = first_id + i as u32;
        placeholder(
            id,
            &format!("{name} {}", id - 1),
            kind,
            Some(*idx),
            None,
            empty,
            "",
            "",
        )
    })
    .collect()
}

fn layout(kind: &str, name: &str, shapes: &str) -> String {
    format!(
        r#"{DECL}<p:sldLayout {NS} type="{kind}" preserve="1"><p:cSld name="{}"><p:spTree>{TREE_HEADER}{shapes}</p:spTree></p:cSld><p:clrMapOvr><a:masterClrMapping/></p:clrMapOvr></p:sldLayout>"#,
        crate::xml_edit::escape_attribute(name)
    )
}

fn layouts(names: &[&str; LAYOUTS]) -> Vec<String> {
    let centred = |size: u32, bullets: bool| {
        format!(
            r#"<a:lvl1pPr marL="0" indent="0" algn="ctr">{}<a:defRPr sz="{size}"/></a:lvl1pPr>"#,
            if bullets { "" } else { "<a:buNone/>" }
        )
    };
    let title_slide = format!(
        "{}{}{}",
        placeholder(
            2,
            "Title 1",
            "ctrTitle",
            None,
            Some((1_524_000, 1_122_363, 9_144_000, 2_387_600)),
            r#"<a:bodyPr anchor="b"/>"#,
            r#"<a:lvl1pPr algn="ctr"><a:defRPr sz="6000"/></a:lvl1pPr>"#,
            "",
        ),
        placeholder(
            3,
            "Subtitle 2",
            "subTitle",
            Some(1),
            Some((1_524_000, 3_602_038, 9_144_000, 1_655_762)),
            "<a:bodyPr/>",
            &centred(2400, false),
            "",
        ),
        footers(4)
    );
    let content = format!(
        "{}{}{}",
        placeholder(2, "Title 1", "title", None, None, "<a:bodyPr/>", "", ""),
        placeholder(
            3,
            "Content Placeholder 2",
            "",
            Some(1),
            None,
            "<a:bodyPr/>",
            "",
            ""
        ),
        footers(4)
    );
    let section = format!(
        "{}{}{}",
        placeholder(
            2,
            "Title 1",
            "title",
            None,
            Some((831_850, 1_709_738, 10_515_600, 2_852_737)),
            r#"<a:bodyPr anchor="b"/>"#,
            r#"<a:lvl1pPr><a:defRPr sz="6000"/></a:lvl1pPr>"#,
            "",
        ),
        placeholder(
            3,
            "Text Placeholder 2",
            "body",
            Some(1),
            Some((831_850, 4_589_463, 10_515_600, 1_500_187)),
            "<a:bodyPr/>",
            r#"<a:lvl1pPr marL="0" indent="0"><a:buNone/><a:defRPr sz="2400"><a:solidFill><a:schemeClr val="tx1"><a:tint val="75000"/></a:schemeClr></a:solidFill></a:defRPr></a:lvl1pPr>"#,
            "",
        ),
        footers(4)
    );
    let title_only = format!(
        "{}{}",
        placeholder(2, "Title 1", "title", None, None, "<a:bodyPr/>", "", ""),
        footers(3)
    );
    let blank = footers(2);
    vec![
        layout("title", names[0], &title_slide),
        layout("obj", names[1], &content),
        layout("secHead", names[2], &section),
        layout("titleOnly", names[3], &title_only),
        layout("blank", names[4], &blank),
    ]
}

fn first_slide() -> String {
    format!(
        r#"{DECL}<p:sld {NS}><p:cSld><p:spTree>{TREE_HEADER}{}{}</p:spTree></p:cSld><p:clrMapOvr><a:masterClrMapping/></p:clrMapOvr></p:sld>"#,
        placeholder(2, "Title 1", "ctrTitle", None, None, "<a:bodyPr/>", "", ""),
        placeholder(
            3,
            "Subtitle 2",
            "subTitle",
            Some(1),
            None,
            "<a:bodyPr/>",
            "",
            ""
        ),
    )
}

/// A notes master: the slide image above, the notes below.
pub const NOTES_MASTER: &str = concat!(
    "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\n",
    r#"<p:notesMaster xmlns:a="http://schemas.openxmlformats.org/drawingml/2006/main" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships" xmlns:p="http://schemas.openxmlformats.org/presentationml/2006/main"><p:cSld><p:bg><p:bgRef idx="1001"><a:schemeClr val="bg1"/></p:bgRef></p:bg><p:spTree><p:nvGrpSpPr><p:cNvPr id="1" name=""/><p:cNvGrpSpPr/><p:nvPr/></p:nvGrpSpPr><p:grpSpPr><a:xfrm><a:off x="0" y="0"/><a:ext cx="0" cy="0"/><a:chOff x="0" y="0"/><a:chExt cx="0" cy="0"/></a:xfrm></p:grpSpPr><p:sp><p:nvSpPr><p:cNvPr id="2" name="Slide Image Placeholder 1"/><p:cNvSpPr><a:spLocks noGrp="1" noRot="1" noChangeAspect="1"/></p:cNvSpPr><p:nvPr><p:ph type="sldImg" idx="2"/></p:nvPr></p:nvSpPr><p:spPr><a:xfrm><a:off x="685800" y="1143000"/><a:ext cx="5486400" cy="3086100"/></a:xfrm><a:prstGeom prst="rect"><a:avLst/></a:prstGeom><a:noFill/><a:ln w="12700"><a:solidFill><a:prstClr val="black"/></a:solidFill></a:ln></p:spPr></p:sp><p:sp><p:nvSpPr><p:cNvPr id="3" name="Notes Placeholder 2"/><p:cNvSpPr><a:spLocks noGrp="1"/></p:cNvSpPr><p:nvPr><p:ph type="body" sz="quarter" idx="3"/></p:nvPr></p:nvSpPr><p:spPr><a:xfrm><a:off x="685800" y="4400550"/><a:ext cx="5486400" cy="3600450"/></a:xfrm><a:prstGeom prst="rect"><a:avLst/></a:prstGeom></p:spPr><p:txBody><a:bodyPr vert="horz" lIns="91440" tIns="45720" rIns="91440" bIns="45720" rtlCol="0"/><a:lstStyle/><a:p><a:endParaRPr lang="en-US"/></a:p></p:txBody></p:sp></p:spTree></p:cSld><p:clrMap bg1="lt1" tx1="dk1" bg2="lt2" tx2="dk2" accent1="accent1" accent2="accent2" accent3="accent3" accent4="accent4" accent5="accent5" accent6="accent6" hlink="hlink" folHlink="folHlink"/><p:notesStyle><a:lvl1pPr marL="0" algn="l" defTabSz="914400" rtl="0" eaLnBrk="1" latinLnBrk="0" hangingPunct="1"><a:defRPr sz="1200" kern="1200"><a:solidFill><a:schemeClr val="tx1"/></a:solidFill><a:latin typeface="+mn-lt"/><a:ea typeface="+mn-ea"/><a:cs typeface="+mn-cs"/></a:defRPr></a:lvl1pPr></p:notesStyle></p:notesMaster>"#
);

/// A new presentation as a package. `names` are the layouts' names in the
/// interface language: Title Slide, Title and Content, Section Header,
/// Title Only, Blank.
pub fn presentation(names: &[&str; LAYOUTS]) -> Vec<u8> {
    let pml = format!("{CT}.presentationml");
    let layout_overrides: String = (1..=LAYOUTS)
        .map(|n| {
            format!(
                r#"<Override PartName="/ppt/slideLayouts/slideLayout{n}.xml" ContentType="{pml}.slideLayout+xml"/>"#
            )
        })
        .collect();
    let types = format!(
        r#"{DECL}<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/><Default Extension="xml" ContentType="application/xml"/><Default Extension="png" ContentType="image/png"/><Default Extension="jpeg" ContentType="image/jpeg"/><Override PartName="/ppt/presentation.xml" ContentType="{pml}.presentation.main+xml"/><Override PartName="/ppt/slideMasters/slideMaster1.xml" ContentType="{pml}.slideMaster+xml"/>{layout_overrides}<Override PartName="/ppt/slides/slide1.xml" ContentType="{pml}.slide+xml"/><Override PartName="/ppt/theme/theme1.xml" ContentType="{CT}.theme+xml"/><Override PartName="/ppt/presProps.xml" ContentType="{pml}.presProps+xml"/><Override PartName="/ppt/viewProps.xml" ContentType="{pml}.viewProps+xml"/><Override PartName="/ppt/tableStyles.xml" ContentType="{pml}.tableStyles+xml"/></Types>"#
    );
    let root_rels = format!(
        r#"{DECL}<Relationships xmlns="{PKG_REL}"><Relationship Id="rId1" Type="{REL}/officeDocument" Target="ppt/presentation.xml"/></Relationships>"#
    );
    let presentation = format!(
        r#"{DECL}<p:presentation {NS} saveSubsetFonts="1"><p:sldMasterIdLst><p:sldMasterId id="2147483648" r:id="rId1"/></p:sldMasterIdLst><p:sldIdLst><p:sldId id="256" r:id="rId2"/></p:sldIdLst><p:sldSz cx="12192000" cy="6858000"/><p:notesSz cx="6858000" cy="9144000"/>{}</p:presentation>"#,
        plain_levels("p:defaultTextStyle")
    );
    let presentation_rels = format!(
        r#"{DECL}<Relationships xmlns="{PKG_REL}"><Relationship Id="rId1" Type="{REL}/slideMaster" Target="slideMasters/slideMaster1.xml"/><Relationship Id="rId2" Type="{REL}/slide" Target="slides/slide1.xml"/><Relationship Id="rId3" Type="{REL}/presProps" Target="presProps.xml"/><Relationship Id="rId4" Type="{REL}/viewProps" Target="viewProps.xml"/><Relationship Id="rId5" Type="{REL}/theme" Target="theme/theme1.xml"/><Relationship Id="rId6" Type="{REL}/tableStyles" Target="tableStyles.xml"/></Relationships>"#
    );
    let master_rels = {
        let layouts: String = (1..=LAYOUTS)
            .map(|n| {
                format!(
                    r#"<Relationship Id="rId{n}" Type="{REL}/slideLayout" Target="../slideLayouts/slideLayout{n}.xml"/>"#
                )
            })
            .collect();
        format!(
            r#"{DECL}<Relationships xmlns="{PKG_REL}">{layouts}<Relationship Id="rId{}" Type="{REL}/theme" Target="../theme/theme1.xml"/></Relationships>"#,
            LAYOUTS + 1
        )
    };
    let layout_rels = format!(
        r#"{DECL}<Relationships xmlns="{PKG_REL}"><Relationship Id="rId1" Type="{REL}/slideMaster" Target="../slideMasters/slideMaster1.xml"/></Relationships>"#
    );
    let slide_rels = format!(
        r#"{DECL}<Relationships xmlns="{PKG_REL}"><Relationship Id="rId1" Type="{REL}/slideLayout" Target="../slideLayouts/slideLayout1.xml"/></Relationships>"#
    );
    let pres_props = format!(r#"{DECL}<p:presentationPr {NS}/>"#);
    let view_props = format!(
        r#"{DECL}<p:viewPr {NS}><p:normalViewPr><p:restoredLeft sz="15620"/><p:restoredTop sz="94660"/></p:normalViewPr><p:gridSpacing cx="76200" cy="76200"/></p:viewPr>"#
    );
    let table_styles = format!(
        r#"{DECL}<a:tblStyleLst xmlns:a="http://schemas.openxmlformats.org/drawingml/2006/main" def="{{5C22544A-7EE6-4342-B048-85BDC9FD1C3A}}"/>"#
    );
    let mut parts: Vec<(String, String)> = vec![
        ("[Content_Types].xml".into(), types),
        ("_rels/.rels".into(), root_rels),
        ("ppt/presentation.xml".into(), presentation),
        ("ppt/_rels/presentation.xml.rels".into(), presentation_rels),
        ("ppt/slideMasters/slideMaster1.xml".into(), master()),
        (
            "ppt/slideMasters/_rels/slideMaster1.xml.rels".into(),
            master_rels,
        ),
    ];
    for (n, xml) in layouts(names).into_iter().enumerate() {
        parts.push((format!("ppt/slideLayouts/slideLayout{}.xml", n + 1), xml));
        parts.push((
            format!("ppt/slideLayouts/_rels/slideLayout{}.xml.rels", n + 1),
            layout_rels.clone(),
        ));
    }
    parts.extend([
        ("ppt/slides/slide1.xml".into(), first_slide()),
        ("ppt/slides/_rels/slide1.xml.rels".into(), slide_rels),
        ("ppt/theme/theme1.xml".into(), theme("Office Theme")),
        ("ppt/presProps.xml".into(), pres_props),
        ("ppt/viewProps.xml".into(), view_props),
        ("ppt/tableStyles.xml".into(), table_styles),
    ]);
    let mut writer = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
    let options = zip::write::SimpleFileOptions::default()
        .compression_method(zip::CompressionMethod::Deflated);
    for (name, text) in parts {
        // Writing to memory does not fail.
        let _ = writer.start_file(name, options);
        let _ = std::io::Write::write_all(&mut writer, text.as_bytes());
    }
    writer
        .finish()
        .map(|cursor| cursor.into_inner())
        .unwrap_or_default()
}
