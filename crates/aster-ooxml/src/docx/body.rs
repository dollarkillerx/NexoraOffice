//! Blocks, paragraphs and runs.

use std::ops::Range;
use std::sync::Arc;

use aster_writer::{
    Block, Break, Inline, Link, OBJECT, PageField, ParaSource, Paragraph, Ruby, Run, RunLook,
    RunSource,
};

use super::dom::{Item, Node, flatten};
use super::props::{RunProps, finalize_para, finalize_run, parse_ppr, parse_rpr};
use super::styles::combine_para;
use super::symbol;
use super::{Field, ParaBase, PartState, Reader, SimpleField};

/// Hyperlinks, tracked changes and content controls inside each other, and
/// fields inside fields, are followed this deep.
const MAX_NESTING: usize = 32;
const MAX_FIELDS: usize = 32;

/// What a run inherits while its paragraph's content is walked.
struct Cx<'b> {
    base: &'b RunLook,
    link: Option<Arc<Link>>,
}

/// One content child of a run, as a piece of the line.
struct Piece {
    kind: Inline,
    text: String,
    /// A typeface the run's own look does not have.
    font: Option<Arc<str>>,
}

impl Piece {
    fn new(kind: Inline) -> Self {
        let text = Run::object(kind.clone(), RunLook::default()).text;
        Self {
            kind,
            text,
            font: None,
        }
    }

    fn text(text: String) -> Self {
        Self {
            kind: Inline::Text,
            text,
            font: None,
        }
    }
}

fn clamp_id(id: i64) -> i32 {
    id.clamp(i64::from(i32::MIN), i64::from(i32::MAX)) as i32
}

fn page_field(instruction: &str) -> Option<PageField> {
    let word = instruction.split_whitespace().next()?;
    if word.eq_ignore_ascii_case("PAGE") {
        Some(PageField::Page)
    } else if word.eq_ignore_ascii_case("NUMPAGES") {
        Some(PageField::PageCount)
    } else if word.eq_ignore_ascii_case("SECTIONPAGES") {
        Some(PageField::SectionPages)
    } else {
        None
    }
}

/// The target of a `HYPERLINK` field instruction.
fn hyperlink_target(instruction: &str) -> Option<String> {
    let mut words = Vec::new();
    let mut chars = instruction.chars().peekable();
    while let Some(&c) = chars.peek() {
        if c.is_whitespace() {
            chars.next();
        } else if c == '"' {
            chars.next();
            let mut word = String::new();
            for c in chars.by_ref() {
                if c == '"' {
                    break;
                }
                word.push(c);
            }
            words.push((true, word));
        } else {
            let mut word = String::new();
            while let Some(&c) = chars.peek() {
                if c.is_whitespace() {
                    break;
                }
                word.push(c);
                chars.next();
            }
            words.push((false, word));
        }
    }
    let (first, rest) = words.split_first()?;
    if !first.1.eq_ignore_ascii_case("HYPERLINK") {
        return None;
    }
    let mut bookmark = false;
    let mut skip_next = false;
    for (quoted, word) in rest {
        if skip_next {
            skip_next = false;
            continue;
        }
        if !quoted && word.starts_with('\\') {
            match word.as_str() {
                "\\l" => bookmark = true,
                // Switches that take a value: tooltip, target frame.
                "\\o" | "\\t" | "\\m" => skip_next = true,
                _ => {}
            }
            continue;
        }
        return Some(if bookmark {
            format!("#{word}")
        } else {
            word.clone()
        });
    }
    None
}

/// The text of a `w:t`: without `xml:space="preserve"` the white space at
/// its ends does not count; line ends and tabs inside are spaces.
fn run_text(node: &Node<'_>) -> String {
    let preserve = node.attr("space").as_deref() == Some("preserve");
    let text = if preserve {
        node.text.as_str()
    } else {
        node.text.trim_matches([' ', '\t', '\n', '\r'])
    };
    if text.contains(['\n', '\r', '\t']) {
        text.replace(['\n', '\r', '\t'], " ")
    } else {
        text.to_owned()
    }
}

fn is_block_container(node: &Node<'_>) -> bool {
    matches!(
        node.local(),
        b"sdt" | b"customXml" | b"smartTag" | b"ins" | b"moveTo" | b"dir" | b"bdo"
    )
}

impl Reader<'_> {
    /// The blocks among `children`: paragraphs and tables, through content
    /// controls and the like.
    pub(super) fn blocks(&mut self, children: &[Node<'_>], part: &mut PartState) -> Vec<Block> {
        let mut items = Vec::new();
        flatten(children, &is_block_container, &mut items);
        let mut out = Vec::with_capacity(items.len());
        for item in &items {
            match item.node.local() {
                b"p" => {
                    let paragraph = self.paragraph(item.node, part);
                    out.push(Block::paragraph(paragraph));
                }
                b"tbl" => {
                    if let Some(table) = self.table(item.node, part) {
                        out.push(Block::table(table));
                    }
                }
                _ => {}
            }
        }
        out
    }

    /// What paragraphs of style `style` in the current table cell start
    /// from.
    fn para_base(&mut self, style: Option<&str>, part: &PartState) -> Arc<ParaBase> {
        let (index, chain) = match self.styles.paragraph_chain(style) {
            Some((index, chain)) => (index, Some(chain)),
            None => (usize::MAX, None),
        };
        let cell_key = part.cell.as_ref().map_or(0, |c| Arc::as_ptr(c) as usize);
        if let Some(found) = self.bases.get(&(index, cell_key)) {
            return found.clone();
        }
        let styles = &self.styles;
        let empty_run = RunProps::default();
        let chain_run = chain.map_or(&empty_run, |c| &c.run);
        let cell_run = part.cell.as_ref().map_or(&empty_run, |c| &c.run);
        let look = finalize_run(&[&styles.defaults_run, chain_run, cell_run]);
        let mut lower = styles.defaults_para.clone();
        if let Some(cell) = &part.cell {
            lower.merge(&cell.para);
        }
        let base = Arc::new(ParaBase {
            look,
            lower,
            chain: chain.map(|c| c.para.clone()).unwrap_or_default(),
        });
        self.bases.insert((index, cell_key), base.clone());
        base
    }

    /// The look of a run with direct properties `rpr` on `base`: the
    /// character style's, then its own.
    fn run_look(&self, base: &RunLook, rpr: Option<&RunProps>) -> (RunLook, Option<Arc<str>>) {
        let mut look = base.clone();
        let Some(rpr) = rpr else {
            return (look, None);
        };
        if let Some(style) = rpr
            .style
            .as_deref()
            .and_then(|s| self.styles.character_chain(s))
        {
            style.apply(&mut look);
        }
        rpr.apply(&mut look);
        (look, rpr.style.clone())
    }

    /// A paragraph with nothing in it, in the document's default style.
    pub(super) fn empty_paragraph(&mut self, part: &PartState) -> Paragraph {
        let base = self.para_base(None, part);
        let partial = combine_para(&base.lower, &base.chain, None, &|id, l| {
            self.lists.lookup(id, l)
        });
        Paragraph {
            props: finalize_para(&partial, base.look.size, 240),
            base: base.look.clone(),
            mark: base.look.clone(),
            ..Paragraph::default()
        }
    }

    pub(super) fn paragraph(&mut self, node: &Node<'_>, part: &mut PartState) -> Paragraph {
        let ppr = node.child("pPr").map(|n| parse_ppr(n, &self.env));
        let direct = ppr.as_ref().map(|p| p.props.clone()).unwrap_or_default();
        let base = self.para_base(direct.style.as_deref(), part);
        let size = base.look.size;
        let pitch = part
            .grids
            .get(part.section)
            .or(part.grids.last())
            .copied()
            .flatten()
            .unwrap_or(240);
        let partial = combine_para(&base.lower, &base.chain, Some(&direct), &|id, l| {
            self.lists.lookup(id, l)
        });
        let mut props = finalize_para(&partial, size, pitch);
        props.style = direct.style.clone();
        if let Some(list) = props.list
            && self.lists.lookup(list.list, list.level).is_none()
        {
            props.list = None;
        }
        let (mark, _) = self.run_look(&base.look, ppr.as_ref().and_then(|p| p.mark.as_ref()));

        let mut runs = Vec::new();
        let cx = Cx {
            base: &base.look,
            link: None,
        };
        self.content(&node.children, part, &cx, &mut runs, 0);
        let runs = merge_markers(runs);

        let section = match ppr.as_ref().and_then(|p| p.section) {
            Some(sect) if part.sections && part.depth == 0 => {
                let section = self.section(sect, part);
                part.section += 1;
                Some(section)
            }
            _ => None,
        };
        let source = ParaSource {
            range: node.range(),
            runs: runs.iter().filter_map(|r| r.source.clone()).collect(),
            props: props.clone(),
            mark: mark.clone(),
            section: section.clone(),
        };
        Paragraph {
            runs,
            props,
            base: base.look.clone(),
            mark,
            label: None,
            section,
            source: Some(Arc::new(source)),
        }
    }

    /// The runs of the content of a paragraph or of something inside it.
    fn content(
        &mut self,
        children: &[Node<'_>],
        part: &mut PartState,
        cx: &Cx<'_>,
        out: &mut Vec<Run>,
        depth: usize,
    ) {
        let mut items = Vec::new();
        flatten(children, &|_| false, &mut items);
        for item in &items {
            let node = item.node;
            match node.local() {
                b"pPr" => {}
                b"r" => self.run(node, item.range.clone(), part, cx, out),
                b"hyperlink" if depth < MAX_NESTING => {
                    let link = self.link_of(node, part);
                    let inner = Cx {
                        base: cx.base,
                        link: link.or_else(|| cx.link.clone()),
                    };
                    self.content(&node.children, part, &inner, out, depth + 1);
                }
                b"fldSimple" if depth < MAX_NESTING => {
                    let field = node.attr("instr").as_deref().and_then(page_field);
                    let outer = part.simple.take();
                    part.simple = field.map(|field| SimpleField { field, done: false });
                    self.content(&node.children, part, cx, out, depth + 1);
                    part.simple = outer;
                }
                b"ins" | b"moveTo" | b"smartTag" | b"customXml" | b"dir" | b"bdo"
                    if depth < MAX_NESTING =>
                {
                    self.content(&node.children, part, cx, out, depth + 1);
                }
                b"sdt" if depth < MAX_NESTING => {
                    if let Some(content) = node.child("sdtContent") {
                        self.content(&content.children, part, cx, out, depth + 1);
                    }
                }
                // Tracked deletions, bookmarks, comment ranges and what we
                // do not know are kept in place and show nothing.
                _ => self.marker(out, cx, item.range.clone(), None),
            }
        }
    }

    fn link_of(&self, node: &Node<'_>, part: &PartState) -> Option<Arc<Link>> {
        let tooltip = node.attr("tooltip").filter(|t| !t.is_empty());
        let external = node
            .attrs("id")
            .into_iter()
            .find_map(|id| part.rels.get(&id))
            .and_then(|rel| rel.external.clone().or_else(|| rel.target.clone()));
        let anchor = node.attr("anchor").filter(|a| !a.is_empty());
        let target = match (external, anchor) {
            (Some(url), Some(anchor)) => format!("{url}#{anchor}"),
            (Some(url), None) => url,
            (None, Some(anchor)) => format!("#{anchor}"),
            (None, None) => return None,
        };
        Some(Arc::new(Link { target, tooltip }))
    }

    fn marker(
        &self,
        out: &mut Vec<Run>,
        cx: &Cx<'_>,
        range: Range<usize>,
        parent: Option<Range<usize>>,
    ) {
        self.push_run(
            out,
            Piece::new(Inline::Marker),
            cx.base,
            None,
            cx.link.clone(),
            range,
            parent,
        );
    }

    #[allow(clippy::too_many_arguments)]
    fn push_run(
        &self,
        out: &mut Vec<Run>,
        piece: Piece,
        look: &RunLook,
        style: Option<Arc<str>>,
        link: Option<Arc<Link>>,
        range: Range<usize>,
        parent: Option<Range<usize>>,
    ) {
        let mut look = look.clone();
        if let Some(font) = piece.font {
            look.font = Some(font);
        }
        let source = Arc::new(RunSource {
            range,
            parent,
            text: piece.text.clone(),
            look: look.clone(),
            style: style.clone(),
            link: link.clone(),
        });
        out.push(Run {
            text: piece.text,
            kind: piece.kind,
            look,
            style,
            link,
            source: Some(source),
        });
    }

    fn run(
        &mut self,
        node: &Node<'_>,
        range: Range<usize>,
        part: &mut PartState,
        cx: &Cx<'_>,
        out: &mut Vec<Run>,
    ) {
        let rpr = node.child("rPr").map(|n| parse_rpr(n, &self.env));
        let (look, style) = self.run_look(cx.base, rpr.as_ref());
        let mut items: Vec<Item<'_, '_>> = Vec::new();
        flatten(&node.children, &|_| false, &mut items);
        let content: Vec<&Item<'_, '_>> = items
            .iter()
            .filter(|i| !matches!(i.node.local(), b"rPr" | b"lastRenderedPageBreak"))
            .collect();
        let link = cx.link.clone().or_else(|| {
            part.fields
                .iter()
                .rev()
                .find(|f| f.result && f.link.is_some())
                .and_then(|f| f.link.clone())
        });
        if content.is_empty() {
            self.push_run(
                out,
                Piece::new(Inline::Marker),
                &look,
                style,
                link,
                range,
                None,
            );
            return;
        }
        let single = content.len() == 1;
        for item in content {
            let (at, parent) = if single {
                (range.clone(), None)
            } else {
                (item.range.clone(), Some(range.clone()))
            };
            let piece = self.piece(item.node, part, &look);
            self.push_run(out, piece, &look, style.clone(), link.clone(), at, parent);
        }
    }

    /// The text of a `w:t`, or the page number if it is the result of a page
    /// field.
    fn text_piece(&self, text: String, part: &mut PartState) -> Piece {
        if let Some(simple) = &mut part.simple {
            return if simple.done {
                Piece::new(Inline::Marker)
            } else {
                simple.done = true;
                let mut piece = Piece::new(Inline::PageNumber(simple.field));
                piece.text = text;
                piece
            };
        }
        if let Some(field) = part
            .fields
            .iter_mut()
            .find(|f| f.result && f.page.is_some())
        {
            return if field.page_done {
                Piece::new(Inline::Marker)
            } else {
                field.page_done = true;
                let mut piece = Piece::new(Inline::PageNumber(field.page.expect("page field")));
                piece.text = text;
                piece
            };
        }
        Piece::text(text)
    }

    fn piece(&mut self, node: &Node<'_>, part: &mut PartState, look: &RunLook) -> Piece {
        match node.local() {
            b"t" => self.text_piece(run_text(node), part),
            b"tab" | b"ptab" => Piece::new(Inline::Tab),
            b"br" => Piece::new(Inline::Break(match node.attr("type").as_deref() {
                Some("page") => Break::Page,
                Some("column") => Break::Column,
                _ => Break::Line,
            })),
            b"cr" => Piece::new(Inline::Break(Break::Line)),
            b"noBreakHyphen" => Piece::text("\u{2011}".to_owned()),
            b"softHyphen" => Piece::text("\u{00AD}".to_owned()),
            b"sym" => {
                let font = node.attr("font").unwrap_or_default();
                let code = node
                    .attr("char")
                    .and_then(|c| u32::from_str_radix(c.trim(), 16).ok())
                    .and_then(char::from_u32);
                match code {
                    Some(c) => match symbol::to_unicode(&font, c) {
                        Some(mapped) => Piece::text(mapped.to_string()),
                        None => {
                            let mut piece = Piece::text(c.to_string());
                            piece.font = (!font.is_empty()).then(|| self.env.intern(&font));
                            piece
                        }
                    },
                    None => Piece::new(Inline::Marker),
                }
            }
            b"drawing" => {
                let drawing = self.drawing(node, part);
                let mut piece = Piece::new(Inline::Drawing(Arc::new(drawing)));
                piece.text = OBJECT.to_owned();
                piece
            }
            b"pict" | b"object" => {
                let drawing = self.vml(node, part);
                let mut piece = Piece::new(Inline::Drawing(Arc::new(drawing)));
                piece.text = OBJECT.to_owned();
                piece
            }
            b"footnoteReference" => match node.int("id") {
                Some(id) => Piece::new(Inline::Footnote(clamp_id(id))),
                None => Piece::new(Inline::Marker),
            },
            b"endnoteReference" => match node.int("id") {
                Some(id) => Piece::new(Inline::Endnote(clamp_id(id))),
                None => Piece::new(Inline::Marker),
            },
            b"footnoteRef" | b"endnoteRef" => match part.note {
                Some((true, id)) if node.is("endnoteRef") => Piece::new(Inline::Endnote(id)),
                Some((false, id)) if node.is("footnoteRef") => Piece::new(Inline::Footnote(id)),
                _ => Piece::new(Inline::Marker),
            },
            b"pgNum" => {
                let mut piece = Piece::new(Inline::PageNumber(PageField::Page));
                piece.text = "1".to_owned();
                piece
            }
            b"ruby" => {
                let mut base = String::new();
                if let Some(b) = node.child("rubyBase") {
                    b.collect_text(&mut base);
                }
                let mut reading = String::new();
                if let Some(rt) = node.child("rt") {
                    rt.collect_text(&mut reading);
                }
                let size = node
                    .child("rubyPr")
                    .and_then(|p| p.child("hps"))
                    .and_then(|h| h.int("val"))
                    .map_or(look.size / 2.0, |v| (v.clamp(2, 400) as f32) / 2.0);
                let mut piece = Piece::new(Inline::Ruby(Arc::new(Ruby {
                    text: reading,
                    size,
                })));
                piece.text = base;
                piece
            }
            b"fldChar" => {
                self.field_char(node, part);
                Piece::new(Inline::Marker)
            }
            b"instrText" | b"delInstrText" => {
                if let Some(field) = part.fields.last_mut()
                    && !field.result
                {
                    field.instruction.push_str(&node.text);
                }
                Piece::new(Inline::Marker)
            }
            _ => Piece::new(Inline::Marker),
        }
    }

    fn field_char(&mut self, node: &Node<'_>, part: &mut PartState) {
        match node.attr("fldCharType").as_deref() {
            Some("begin") => {
                if part.fields.len() < MAX_FIELDS {
                    part.fields.push(Field::default());
                } else {
                    part.field_overflow += 1;
                }
            }
            Some("separate") if part.field_overflow == 0 => {
                if let Some(field) = part.fields.last_mut() {
                    field.result = true;
                    field.page = page_field(&field.instruction);
                    field.link = hyperlink_target(&field.instruction).map(|target| {
                        Arc::new(Link {
                            target,
                            tooltip: None,
                        })
                    });
                }
            }
            Some("end") => {
                if part.field_overflow > 0 {
                    part.field_overflow -= 1;
                } else {
                    part.fields.pop();
                }
            }
            _ => {}
        }
    }
}

/// Adjacent markers that follow each other in the bytes become one.
fn merge_markers(runs: Vec<Run>) -> Vec<Run> {
    let mut out: Vec<Run> = Vec::with_capacity(runs.len());
    for run in runs {
        if let (Some(last), Some(source)) = (out.last_mut(), &run.source)
            && matches!(last.kind, Inline::Marker)
            && matches!(run.kind, Inline::Marker)
            && let Some(prev) = &last.source
            && prev.range.end == source.range.start
            && prev.parent == source.parent
            && last.look == run.look
            && last.style == run.style
            && last.link == run.link
        {
            let merged = Arc::new(RunSource {
                range: prev.range.start..source.range.end,
                ..(**prev).clone()
            });
            last.source = Some(merged);
            continue;
        }
        out.push(run);
    }
    out
}
