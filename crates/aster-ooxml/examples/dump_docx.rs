//! Print an outline of what the DOCX reader makes of a file:
//!
//! `cargo run -p aster-ooxml --example dump_docx -- file.docx`
//!
//! Sections, then every block with its path: paragraph properties that
//! differ from the default, the list label, the runs (kind, shortened
//! text, look), tables with grid, rows and cells, then headers and footers,
//! notes, and counts of styles and lists.

use aster_ooxml::docx;
use aster_writer::{
    Align, Block, Border, Document, Inline, LineSpacing, ParaProps, Paragraph, Path, Rgba, Run,
    RunLook, Section, Story, Table, UnderlineStyle, VMerge, VertAlign,
};

fn main() {
    let Some(file) = std::env::args().nth(1) else {
        eprintln!("usage: dump_docx <file.docx>");
        std::process::exit(2);
    };
    let bytes = match std::fs::read(&file) {
        Ok(bytes) => bytes,
        Err(err) => {
            eprintln!("cannot read {file}: {err}");
            std::process::exit(1);
        }
    };
    let started = std::time::Instant::now();
    let document = match docx::read(bytes) {
        Ok(document) => document,
        Err(err) => {
            eprintln!("cannot read {file}: {err}");
            std::process::exit(1);
        }
    };
    let elapsed = started.elapsed();
    dump(&document);
    println!("\nread in {:.1} ms", elapsed.as_secs_f64() * 1000.0);
}

fn dump(document: &Document) {
    let styles = &document.styles;
    let count = |kind| styles.styles.iter().filter(|s| s.kind == kind).count();
    println!(
        "styles: {} (paragraph {}, character {}, table {}, numbering {}), default {:?}",
        styles.styles.len(),
        count(aster_writer::StyleKind::Paragraph),
        count(aster_writer::StyleKind::Character),
        count(aster_writer::StyleKind::Table),
        count(aster_writer::StyleKind::Numbering),
        styles.default_style.as_deref().unwrap_or("-"),
    );
    println!(
        "lists: {}, header/footer stories: {}, footnotes: {}, endnotes: {}",
        document.numbering.lists.len(),
        document.marginals.len(),
        document.footnotes.notes.len(),
        document.endnotes.notes.len(),
    );
    let s = &document.settings;
    println!(
        "settings: default tab {}, even/odd headers {}, language {:?}, east asian {:?}, compress punctuation {}",
        s.default_tab,
        s.even_and_odd_headers,
        s.language,
        s.east_asian_language,
        s.compress_punctuation
    );
    println!(
        "default text: {} / default paragraph: {}",
        look(&styles.default_run),
        para_props(&styles.default_para)
    );

    println!();
    for (n, (last, section)) in document.sections().iter().enumerate() {
        println!(
            "section {n} (ends at body block {last}): {}",
            section_line(section)
        );
    }

    println!("\nbody ({}):", document.body.part.as_deref().unwrap_or("-"));
    blocks(&document.body.blocks, &Path::default(), 1);

    for (i, story) in document.marginals.iter().enumerate() {
        println!("\nheader/footer {i}: {}", story_name(story));
        blocks(&story.blocks, &Path::default(), 1);
    }
    for (title, notes) in [
        ("footnotes", &document.footnotes),
        ("endnotes", &document.endnotes),
    ] {
        for note in &notes.notes {
            println!(
                "\n{title}, note {} ({}):",
                note.id,
                notes.part.as_deref().unwrap_or("-")
            );
            blocks(&note.blocks, &Path::default(), 1);
        }
    }
}

fn story_name(story: &Story) -> String {
    format!(
        "{}, {} blocks",
        story.part.as_deref().unwrap_or("-"),
        story.blocks.len()
    )
}

fn section_line(s: &Section) -> String {
    let m = &s.margins;
    let mut out = format!(
        "page {}x{}, margins t{} r{} b{} l{} header {} footer {} gutter {}, columns {} space {}{}{}, start {:?}",
        s.page.0,
        s.page.1,
        m.top,
        m.right,
        m.bottom,
        m.left,
        m.header,
        m.footer,
        m.gutter,
        s.columns.count,
        s.columns.space,
        if s.columns.widths.is_empty() {
            String::new()
        } else {
            format!(" widths {:?}", s.columns.widths)
        },
        if s.columns.separator {
            " separator"
        } else {
            ""
        },
        s.start,
    );
    out += &format!(
        ", headers {}, footers {}",
        marginals(&s.headers),
        marginals(&s.footers)
    );
    if s.title_page {
        out += ", title page";
    }
    if let Some(start) = s.page_number_start {
        out += &format!(", page numbers from {start}");
    }
    if s.page_number_format != aster_writer::NumberFormat::Decimal {
        out += &format!(", page number format {:?}", s.page_number_format);
    }
    if let Some(grid) = s.grid {
        out += &format!(", grid {:?} pitch {}", grid.kind, grid.line_pitch);
    }
    if s.vertical_align != aster_writer::CellAlign::Top {
        out += &format!(", vertical align {:?}", s.vertical_align);
    }
    if s.vertical_text {
        out += ", vertical text";
    }
    out += &format!(", source {:?}", s.source);
    out
}

fn marginals(m: &aster_writer::Marginals) -> String {
    let at = |v: Option<usize>| v.map_or("-".to_owned(), |v| v.to_string());
    format!(
        "default {} first {} even {}",
        at(m.default),
        at(m.first),
        at(m.even)
    )
}

fn blocks(list: &[Block], prefix: &Path, depth: usize) {
    for (i, block) in list.iter().enumerate() {
        let mut steps = prefix.0.clone();
        steps.push(i as u32);
        let path = Path(steps);
        match block {
            Block::Paragraph(p) => paragraph(p, &path, depth),
            Block::Table(t) => table(t, &path, depth),
        }
    }
}

fn indent(depth: usize) -> String {
    "  ".repeat(depth)
}

fn short(text: &str) -> String {
    let flat: String = text
        .chars()
        .map(|c| match c {
            '\n' => '⏎',
            '\t' => '→',
            '\u{FFFC}' => '▯',
            c => c,
        })
        .collect();
    if flat.chars().count() > 40 {
        let head: String = flat.chars().take(37).collect();
        format!("{head}...")
    } else {
        flat
    }
}

fn color(c: Rgba) -> String {
    format!("#{:02X}{:02X}{:02X}", c[0], c[1], c[2])
}

fn border(b: &Option<Border>) -> String {
    match b {
        None => "-".to_owned(),
        Some(b) => format!(
            "{:?}{}pt{}",
            b.style,
            b.width,
            b.color.map_or(String::new(), color)
        ),
    }
}

fn paragraph(p: &Paragraph, path: &Path, depth: usize) {
    let steps: Vec<String> = path.0.iter().map(u32::to_string).collect();
    let mut head = format!("{}[{}] P", indent(depth), steps.join(","));
    let props = para_props(&p.props);
    if !props.is_empty() {
        head += &format!(" {props}");
    }
    if let Some(label) = &p.label {
        head += &format!(" label {:?} ({:?})", label.text, label.suffix);
    }
    if let Some(section) = &p.section {
        head += &format!(" ENDS SECTION page {}x{}", section.page.0, section.page.1);
    }
    println!("{head}");
    let source = p.source.as_ref().map_or("no source".to_owned(), |s| {
        format!(
            "{:?}{}",
            s.range,
            if p.is_as_read() { "" } else { " CHANGED" }
        )
    });
    println!(
        "{}  base: {} | mark: {} | {source}",
        indent(depth),
        look(&p.base),
        look_diff(&p.mark, &p.base),
    );
    for run in &p.runs {
        run_line(run, &p.base, depth + 1);
    }
}

fn run_line(run: &Run, base: &RunLook, depth: usize) {
    let kind = match &run.kind {
        Inline::Text => "text".to_owned(),
        Inline::Tab => "tab".to_owned(),
        Inline::Break(b) => format!("break {b:?}"),
        Inline::Drawing(d) => {
            let content = match &d.content {
                aster_writer::DrawingContent::Picture(p) => {
                    format!("picture {} bytes crop {:?}", p.image.len(), p.crop)
                }
                aster_writer::DrawingContent::TextBox(t) => format!(
                    "text box ({} blocks, fill {:?}, line {}, insets {:?})",
                    t.blocks.len(),
                    t.fill.map(color),
                    border(&t.line),
                    t.insets
                ),
                aster_writer::DrawingContent::Other(what) => format!("other {what}"),
            };
            let float = d.float.map_or("inline".to_owned(), |f| {
                format!(
                    "float x {:?}/{:?} y {:?}/{:?} wrap {:?}{} dist {:?}",
                    f.x.from,
                    f.x.at,
                    f.y.from,
                    f.y.at,
                    f.wrap,
                    if f.behind { " behind" } else { "" },
                    f.distance
                )
            });
            format!(
                "drawing {}x{} emu, {float}, {content}{}",
                d.size.0,
                d.size.1,
                d.alt
                    .as_ref()
                    .map_or(String::new(), |a| format!(", alt {a:?}"))
            )
        }
        Inline::Footnote(id) => format!("footnote {id}"),
        Inline::Endnote(id) => format!("endnote {id}"),
        Inline::PageNumber(f) => format!("page field {f:?}"),
        Inline::Ruby(r) => format!("ruby {:?} {}pt", r.text, r.size),
        Inline::Marker => "marker".to_owned(),
    };
    let mut line = format!("{}- {kind}", indent(depth));
    if !run.text.is_empty() && !matches!(run.kind, Inline::Tab | Inline::Drawing(_)) {
        line += &format!(" {:?}", short(&run.text));
    }
    if let Some(style) = &run.style {
        line += &format!(" style {style}");
    }
    if let Some(link) = &run.link {
        line += &format!(" link {}", link.target);
    }
    if !matches!(run.kind, Inline::Marker) {
        let diff = look_diff(&run.look, base);
        if !diff.is_empty() {
            line += &format!(" [{diff}]");
        }
    }
    if let Some(source) = &run.source {
        line += &format!(" @{:?}", source.range);
        if let Some(parent) = &source.parent {
            line += &format!(" in {parent:?}");
        }
    }
    println!("{line}");
    if let Inline::Drawing(d) = &run.kind
        && let aster_writer::DrawingContent::TextBox(t) = &d.content
    {
        blocks(&t.blocks, &Path::default(), depth + 2);
    }
}

fn para_props(p: &ParaProps) -> String {
    let d = ParaProps::default();
    let mut out = Vec::new();
    if let Some(style) = &p.style {
        out.push(format!("style={style}"));
    }
    if p.align != Align::Left {
        out.push(format!("align={:?}", p.align));
    }
    if p.indent_left != d.indent_left {
        out.push(format!("left={}", p.indent_left));
    }
    if p.indent_right != d.indent_right {
        out.push(format!("right={}", p.indent_right));
    }
    if p.first_line != d.first_line {
        out.push(format!("first={}", p.first_line));
    }
    if p.space_before != 0 {
        out.push(format!("before={}", p.space_before));
    }
    if p.space_after != 0 {
        out.push(format!("after={}", p.space_after));
    }
    if p.line != LineSpacing::default() {
        out.push(format!("line={:?}", p.line));
    }
    for (on, name) in [
        (p.contextual_spacing, "contextual"),
        (p.keep_next, "keepNext"),
        (p.keep_lines, "keepLines"),
        (p.page_break_before, "pageBreakBefore"),
        (!p.widow_control, "noWidow"),
        (!p.snap_to_grid, "noSnap"),
    ] {
        if on {
            out.push(name.to_owned());
        }
    }
    if let Some(level) = p.outline_level {
        out.push(format!("outline={level}"));
    }
    if let Some(list) = p.list {
        out.push(format!("list={}/{}", list.list, list.level));
    }
    if !p.tabs.is_empty() {
        let tabs: Vec<String> = p
            .tabs
            .iter()
            .map(|t| format!("{}{:?}{:?}", t.position, t.align, t.leader))
            .collect();
        out.push(format!("tabs=[{}]", tabs.join(" ")));
    }
    if p.borders.iter().any(Option::is_some) {
        let sides: Vec<String> = p.borders.iter().map(border).collect();
        out.push(format!("borders(L,T,R,B)=[{}]", sides.join(" ")));
    }
    if p.border_between.is_some() {
        out.push(format!("between={}", border(&p.border_between)));
    }
    if let Some(shading) = p.shading {
        out.push(format!("shading={}", color(shading)));
    }
    out.join(" ")
}

fn look(l: &RunLook) -> String {
    look_diff(l, &RunLook::default())
}

/// What `l` has that `base` does not (everything but the size and the
/// fonts when `base` is the default look).
fn look_diff(l: &RunLook, base: &RunLook) -> String {
    let mut out = Vec::new();
    if l.font != base.font || l.east_asian_font != base.east_asian_font {
        out.push(format!(
            "font={}/{}",
            l.font.as_deref().unwrap_or("-"),
            l.east_asian_font.as_deref().unwrap_or("-")
        ));
    }
    if l.size != base.size {
        out.push(format!("{}pt", l.size));
    }
    if l.bold != base.bold {
        out.push(if l.bold { "b" } else { "!b" }.to_owned());
    }
    if l.italic != base.italic {
        out.push(if l.italic { "i" } else { "!i" }.to_owned());
    }
    if l.underline != base.underline {
        out.push(match l.underline {
            None => "!u".to_owned(),
            Some(u) => format!(
                "u:{}{}",
                match u.style {
                    UnderlineStyle::Single => "single",
                    UnderlineStyle::Double => "double",
                    UnderlineStyle::Thick => "thick",
                    UnderlineStyle::Dotted => "dotted",
                    UnderlineStyle::Dashed => "dashed",
                    UnderlineStyle::Wave => "wave",
                    UnderlineStyle::Words => "words",
                },
                u.color.map_or(String::new(), |c| format!(" {}", color(c)))
            ),
        });
    }
    if l.strike != base.strike {
        out.push(if l.strike { "strike" } else { "!strike" }.to_owned());
    }
    if l.double_strike != base.double_strike {
        out.push(
            if l.double_strike {
                "dstrike"
            } else {
                "!dstrike"
            }
            .to_owned(),
        );
    }
    if l.color != base.color {
        out.push(format!(
            "color={}",
            l.color.map_or("auto".to_owned(), color)
        ));
    }
    if l.highlight != base.highlight {
        out.push(format!(
            "highlight={}",
            l.highlight.map_or("none".to_owned(), color)
        ));
    }
    if l.shading != base.shading {
        out.push(format!(
            "shading={}",
            l.shading.map_or("none".to_owned(), color)
        ));
    }
    if l.position != base.position {
        out.push(
            match l.position {
                VertAlign::Baseline => "baseline",
                VertAlign::Superscript => "sup",
                VertAlign::Subscript => "sub",
            }
            .to_owned(),
        );
    }
    for (a, b, name) in [
        (l.caps, base.caps, "caps"),
        (l.small_caps, base.small_caps, "smallcaps"),
        (l.hidden, base.hidden, "hidden"),
    ] {
        if a != b {
            out.push(if a {
                name.to_owned()
            } else {
                format!("!{name}")
            });
        }
    }
    if l.spacing != base.spacing {
        out.push(format!("spacing={}", l.spacing));
    }
    if l.scale != base.scale {
        out.push(format!("scale={}", l.scale));
    }
    if l.raise != base.raise {
        out.push(format!("raise={}", l.raise));
    }
    if out.is_empty() && base == &RunLook::default() {
        return "default".to_owned();
    }
    out.join(" ")
}

fn table(t: &Table, path: &Path, depth: usize) {
    let steps: Vec<String> = path.0.iter().map(u32::to_string).collect();
    let pad = indent(depth);
    println!(
        "{pad}[{}] TABLE grid {:?} style {} align {:?} indent {} margins {:?}{} source {:?}",
        steps.join(","),
        t.columns,
        t.props.style.as_deref().unwrap_or("-"),
        t.props.align,
        t.props.indent,
        t.props.cell_margins,
        if t.props.fixed_layout { " fixed" } else { "" },
        t.source,
    );
    for (r, row) in t.rows.iter().enumerate() {
        let mut flags = String::new();
        if row.header {
            flags += " header";
        }
        if row.cant_split {
            flags += " cantSplit";
        }
        if row.grid_before > 0 || row.grid_after > 0 {
            flags += &format!(
                " gridBefore {} gridAfter {}",
                row.grid_before, row.grid_after
            );
        }
        println!(
            "{pad}  row {r}: height {:?}{flags} source {:?}",
            row.height, row.source
        );
        for (c, cell) in row.cells.iter().enumerate() {
            let sides: Vec<String> = cell.borders.iter().map(border).collect();
            let merge = match cell.merge {
                VMerge::None => "",
                VMerge::Start => " vmerge-start",
                VMerge::Continue => " vmerge-continue",
            };
            println!(
                "{pad}    cell {c}: span {}{merge} borders(L,T,R,B)=[{}] shading {} margins {:?} align {:?} source {:?}",
                cell.span,
                sides.join(" "),
                cell.shading.map_or("-".to_owned(), color),
                cell.margins,
                cell.align,
                cell.source,
            );
            let mut inner = path.0.clone();
            inner.extend([r as u32, c as u32]);
            blocks(&cell.blocks, &Path(inner), depth + 3);
        }
    }
}
