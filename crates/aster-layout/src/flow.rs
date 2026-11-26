//! Pagination: sections, columns, pages and what goes where.
//!
//! Blocks are placed one after the other into the current column of the
//! current page. A paragraph is placed in fragments of whole lines; a table
//! row by row; when the column is full the cursor moves to the next column,
//! and after the last to a new page.

use std::sync::Arc;

use aster_writer::numbering::format_number;
use aster_writer::{
    Block, Break, Document, Inline, Marginals, Paragraph, Path, Pos, Section, SectionStart, Table,
    Wrap,
};

use crate::cx::{
    Cx, EmitCtx, Frag, Layers, Layouter, Out, ParaBox, ParaEntry, Paths, Stack, gap, pads,
};
use crate::display::{Item, Page, Rect, RuleStyle};
use crate::query::{Layout, OutlineEntry};
use crate::shape::{NoteNumbers, PageValues, emu_pt, outline_text, pt};
use crate::table::{Slice, TableLayout};

const EPS: f32 = 0.01;
/// Room between the body and the footnote separator, points.
const NOTE_GAP: f32 = 6.0;
/// Most pages a document is laid out into; the rest is dropped.
const MAX_PAGES: usize = 20_000;

struct Region {
    top: f32,
    cols: Vec<(f32, f32)>,
    separator: bool,
    used_bottom: f32,
}

struct PageState {
    width: f32,
    height: f32,
    number: u32,
    section: usize,
    sec: Arc<Section>,
    layers: Layers,
    regions: Vec<Region>,
    margin: Rect,
    body_top: f32,
    body_bottom: f32,
    header_height: f32,
    footer_height: f32,
    header: Option<usize>,
    footer: Option<usize>,
    notes: Vec<(i32, Stack)>,
    note_height: f32,
    obstructions: Vec<Rect>,
    used: bool,
    page_in_section: usize,
}

/// The shape of a section's pages.
struct Geom {
    width: f32,
    height: f32,
    margin: Rect,
    cols: Vec<(f32, f32)>,
    separator: bool,
    /// What paragraphs are shaped for: the narrowest column.
    shape_width: f32,
}

fn geometry(sec: &Section) -> Geom {
    let width = pt(sec.page.0).max(1.0);
    let height = pt(sec.page.1).max(1.0);
    let m = &sec.margins;
    let left = pt(m.left + m.gutter).max(0.0).min(width - 1.0);
    let right = (width - pt(m.right).max(0.0)).max(left + 1.0);
    let top = pt(m.top).max(0.0).min(height - 1.0);
    let bottom = (height - pt(m.bottom).max(0.0)).max(top + 1.0);
    let margin = Rect::new(left, top, right, bottom);
    let text = right - left;
    let n = (sec.columns.count.max(1) as usize).min(64);
    let mut cols = Vec::new();
    if sec.columns.widths.is_empty() || n == 1 {
        let space = pt(sec.columns.space).max(0.0);
        let each = ((text - space * (n - 1) as f32) / n as f32).max(1.0);
        for i in 0..n {
            cols.push((left + i as f32 * (each + space), each));
        }
    } else {
        let mut x = left;
        for (w, space) in sec.columns.widths.iter().take(n) {
            let w = pt(*w).max(1.0);
            cols.push((x, w));
            x += w + pt(*space).max(0.0);
        }
    }
    let shape_width = cols
        .iter()
        .map(|c| c.1)
        .fold(f32::INFINITY, f32::min)
        .max(1.0);
    Geom {
        width,
        height,
        margin,
        cols,
        separator: sec.columns.separator && n > 1,
        shape_width,
    }
}

struct Flow<'a, 'b> {
    cx: &'b mut Cx<'a>,
    doc: &'a Document,
    pages: Vec<PageState>,
    frags: Vec<Frag>,
    outline: Vec<OutlineEntry>,
    section: usize,
    sec: Arc<Section>,
    geom: Geom,
    col: usize,
    y: f32,
    col_empty: bool,
    overflowed: bool,
    next_number: u32,
    pending_start: Option<u32>,
    prev: Option<Arc<Paragraph>>,
    sections_pages: Vec<usize>,
}

fn collect_notes(doc: &Document) -> NoteNumbers {
    let mut numbers = NoteNumbers::default();
    fn scan(blocks: &[Block], numbers: &mut NoteNumbers) {
        for block in blocks {
            match block {
                Block::Paragraph(p) => {
                    for run in &p.runs {
                        match run.kind {
                            Inline::Footnote(id) => {
                                let next = numbers.foot.len() as u32 + 1;
                                numbers.foot.entry(id).or_insert(next);
                            }
                            Inline::Endnote(id) => {
                                let next = numbers.end.len() as u32 + 1;
                                numbers.end.entry(id).or_insert(next);
                            }
                            _ => {}
                        }
                    }
                }
                Block::Table(t) => {
                    for row in &t.rows {
                        for cell in &row.cells {
                            scan(&cell.blocks, numbers);
                        }
                    }
                }
            }
        }
    }
    scan(&doc.body.blocks, &mut numbers);
    numbers
}

impl Layouter {
    /// Lay a document out into pages.
    pub fn layout(&mut self, document: &Document) -> Layout {
        self.begin();
        let notes = collect_notes(document);
        let sections = document.sections();
        let first: Arc<Section> = sections
            .first()
            .map(|(_, s)| (*s).clone())
            .unwrap_or_default();
        let geom = geometry(&first);
        let first_section = first.clone();
        let mut cx = Cx {
            lay: self,
            doc: document,
            notes,
            grid: None,
            max_object: 1.0,
            page_values: None,
        };
        let mut flow = Flow {
            cx: &mut cx,
            doc: document,
            pages: Vec::new(),
            frags: Vec::new(),
            outline: Vec::new(),
            section: 0,
            sec: first_section,
            geom,
            col: 0,
            y: 0.0,
            col_empty: true,
            overflowed: false,
            next_number: 1,
            pending_start: None,
            prev: None,
            sections_pages: Vec::new(),
        };
        flow.run();
        let layout = flow.finish();
        self.finish();
        layout
    }
}

impl Flow<'_, '_> {
    fn run(&mut self) {
        let sections = self.doc.sections();
        let blocks = &self.doc.body.blocks;
        let mut start = 0usize;
        for (index, (last, sec)) in sections.iter().enumerate() {
            let end = (*last + 1).min(blocks.len()).max(start);
            self.begin_section(index, sec);
            for i in start..end {
                if self.pages.len() >= MAX_PAGES {
                    return;
                }
                self.place_block(blocks, i);
            }
            start = end;
        }
        self.place_endnotes();
    }

    fn page(&mut self) -> &mut PageState {
        self.pages
            .last_mut()
            .expect("a page exists once flow begins")
    }

    fn col_bottom(&self) -> f32 {
        let page = self.pages.last().expect("page");
        page.body_bottom - page.note_height
    }

    fn region(&self) -> &Region {
        self.pages
            .last()
            .and_then(|p| p.regions.last())
            .expect("region")
    }

    fn col_rect(&self) -> (f32, f32) {
        let region = self.region();
        region.cols[self.col.min(region.cols.len() - 1)]
    }

    fn begin_section(&mut self, index: usize, sec: &Arc<Section>) {
        let first = self.pages.is_empty();
        self.section = index;
        self.sec = sec.clone();
        self.geom = geometry(sec);
        self.cx.grid = sec.grid.map(|g| pt(g.line_pitch)).filter(|p| *p > 0.0);
        self.cx.max_object = (self.geom.margin.height() * 0.95).max(1.0);
        self.sections_pages.push(0);
        self.pending_start = sec.page_number_start;
        let same_size = self.pages.last().is_some_and(|p| {
            (p.width - self.geom.width).abs() < 0.5 && (p.height - self.geom.height).abs() < 0.5
        });
        match sec.start {
            _ if first => self.new_page(),
            SectionStart::Continuous if same_size => {
                self.pending_start = None;
                let top = {
                    let region = self.region();
                    region.used_bottom.max(self.y)
                };
                let cols = self.geom.cols.clone();
                let separator = self.geom.separator;
                let margin = self.geom.margin;
                let page = self.page();
                page.margin = margin;
                page.regions.push(Region {
                    top,
                    cols,
                    separator,
                    used_bottom: top,
                });
                self.col = 0;
                self.y = top;
                self.col_empty = true;
                self.prev = None;
            }
            SectionStart::NextColumn if self.geom.cols.len() > 1 && same_size => {
                self.new_column_in_new_region()
            }
            SectionStart::EvenPage | SectionStart::OddPage => {
                let upcoming = self.pending_start.unwrap_or(self.next_number);
                let want_even = sec.start == SectionStart::EvenPage;
                if (upcoming % 2 == 0) != want_even {
                    let restart = self.pending_start.take();
                    self.new_page();
                    self.pending_start = restart;
                }
                self.new_page();
            }
            _ => self.new_page(),
        }
        // The page a section starts on takes this section's header and footer.
    }

    fn new_column_in_new_region(&mut self) {
        let top = self.region().used_bottom.max(self.y);
        let cols = self.geom.cols.clone();
        let separator = self.geom.separator;
        let margin = self.geom.margin;
        let page = self.page();
        page.margin = margin;
        page.regions.push(Region {
            top,
            cols,
            separator,
            used_bottom: top,
        });
        self.col = 0;
        self.y = top;
        self.col_empty = true;
        self.prev = None;
    }

    fn marginal_stack(
        &mut self,
        which: Option<usize>,
        width: f32,
        values: &PageValues,
    ) -> Option<Stack> {
        let story = self.doc.marginals.get(which?)?;
        self.cx.page_values = Some(values.clone());
        let stack = self.cx.layout_stack(&story.blocks, &Paths::None, width, 0);
        self.cx.page_values = None;
        Some(stack)
    }

    fn pick(&self, set: &Marginals, number: u32, first: bool) -> Option<usize> {
        if first && self.sec.title_page {
            set.first
        } else if self.doc.settings.even_and_odd_headers && number % 2 == 0 {
            set.even
        } else {
            set.default
        }
    }

    fn page_values(
        &self,
        number: u32,
        count: usize,
        section_pages: usize,
        sec: &Section,
    ) -> PageValues {
        PageValues {
            page: format_number(number, &sec.page_number_format),
            count: count.to_string(),
            section_pages: section_pages.to_string(),
        }
    }

    fn new_page(&mut self) {
        let number = self.pending_start.take().unwrap_or(self.next_number);
        self.next_number = number.wrapping_add(1).max(1);
        let sec = self.sec.clone();
        let geom = &self.geom;
        let page_in_section = {
            let section = self.section;
            self.pages.iter().filter(|p| p.section == section).count()
        };
        let (width, height, margin) = (geom.width, geom.height, geom.margin);
        let text_w = margin.width();
        let first = page_in_section == 0;
        let header = self.pick(&sec.headers, number, first);
        let footer = self.pick(&sec.footers, number, first);
        let values = self.page_values(number, 1, 1, &sec);
        let header_stack = self.marginal_stack(header, text_w, &values);
        let footer_stack = self.marginal_stack(footer, text_w, &values);
        let header_height = header_stack.as_ref().map_or(0.0, |s| s.height);
        let footer_height = footer_stack.as_ref().map_or(0.0, |s| s.height);
        let m = &sec.margins;
        let mut body_top = margin.y0;
        if header_height > 0.0 {
            body_top = body_top.max(pt(m.header).max(0.0) + header_height);
        }
        let mut body_bottom = margin.y1;
        if footer_height > 0.0 {
            body_bottom = body_bottom.min(height - pt(m.footer).max(0.0) - footer_height);
        }
        // A page always has room for something.
        body_top = body_top.min(height * 0.8);
        body_bottom = body_bottom.max(body_top + 1.0);
        let cols = self.geom.cols.clone();
        let separator = self.geom.separator;
        self.pages.push(PageState {
            width,
            height,
            number,
            section: self.section,
            sec,
            layers: Layers::default(),
            regions: vec![Region {
                top: body_top,
                cols,
                separator,
                used_bottom: body_top,
            }],
            margin,
            body_top,
            body_bottom,
            header_height,
            footer_height,
            header,
            footer,
            notes: Vec::new(),
            note_height: 0.0,
            obstructions: Vec::new(),
            used: false,
            page_in_section,
        });
        self.y = body_top;
        self.col = 0;
        self.col_empty = true;
        self.prev = None;
        if let Some(count) = self.sections_pages.get_mut(self.section) {
            *count += 1;
        }
    }

    /// Move to the next column, or the next page after the last. `explicit`
    /// is for a break the document asked for: space before a paragraph is
    /// kept after one.
    fn advance(&mut self, explicit: bool) {
        let columns = self.region().cols.len();
        if self.col + 1 < columns {
            self.col += 1;
            self.y = self.region().top;
            self.col_empty = true;
            self.prev = None;
        } else {
            self.new_page();
        }
        self.overflowed = !explicit;
    }

    fn advance_page(&mut self) {
        let columns = self.region().cols.len();
        self.col = columns - 1;
        self.advance(true);
    }

    fn ec(&self, record: bool) -> EmitCtx {
        let page = self.pages.last().expect("page");
        let (x, w) = self.col_rect();
        EmitCtx {
            page: self.pages.len() - 1,
            page_rect: Rect::new(0.0, 0.0, page.width, page.height),
            margin: page.margin,
            column: Rect::new(x, page.body_top, x + w, page.body_bottom),
            bounds: None,
            repeat: false,
            record,
            depth: 0,
        }
    }

    fn place_block(&mut self, blocks: &[Block], i: usize) {
        match &blocks[i] {
            Block::Paragraph(p) => {
                let path = Some(Path::body(i));
                self.place_with_chain(blocks, i, path, p);
            }
            Block::Table(t) => self.place_table(Path::body(i), t),
        }
    }

    /// The height a keep-with-next chain starting at `i` needs: what must
    /// stay together on one page.
    fn chain_need(&mut self, blocks: &[Block], i: usize) -> f32 {
        let mut need = 0.0;
        let width = self.geom.shape_width;
        let mut prev: Option<Arc<Paragraph>> = None;
        for block in blocks.iter().skip(i).take(12) {
            let Block::Paragraph(p) = block else {
                break;
            };
            let shaped = self.cx.shaped(p, Cx::text_width(p, width));
            let lines = shaped.lines();
            let space = prev
                .as_deref()
                .map_or(pt(p.props.space_before), |q| gap(q, p));
            let all = shaped.paragraph.height();
            let unit = if p.props.keep_lines || lines.len() <= 4 {
                all
            } else {
                let tail = if p.props.keep_next { 2 } else { 2 };
                let head: f32 = lines.iter().take(tail).map(|l| l.height).sum();
                if p.props.keep_next { head } else { head }
            };
            need += space + unit;
            if !p.props.keep_next {
                break;
            }
            prev = Some(p.clone());
        }
        need
    }

    fn place_with_chain(
        &mut self,
        blocks: &[Block],
        i: usize,
        path: Option<Path>,
        para: &Arc<Paragraph>,
    ) {
        if para.props.keep_next && !self.col_empty {
            let need = self.chain_need(blocks, i);
            if self.y + need > self.col_bottom() + EPS
                && need <= self.col_bottom() - self.region().top
            {
                self.advance(false);
            }
        }
        self.place_paragraph(path, para, blocks.get(i + 1));
    }

    fn obstruction(&self, top: f32, bottom: f32) -> Option<f32> {
        let page = self.pages.last()?;
        let (x, w) = self.col_rect();
        page.obstructions
            .iter()
            .filter(|o| {
                o.x0 < x + w
                    && o.x1 > x
                    && o.y0 < bottom - EPS
                    && o.y1 > top + EPS
                    && o.y1 > top + 0.5
            })
            .map(|o| o.y1)
            .fold(None, |best: Option<f32>, y| {
                Some(best.map_or(y, |b| b.max(y)))
            })
    }

    fn place_paragraph(&mut self, path: Option<Path>, para: &Arc<Paragraph>, next: Option<&Block>) {
        let props = &para.props;
        if props.page_break_before && self.pages.last().is_some_and(|p| p.used) {
            self.advance_page();
        }
        let shaped = self
            .cx
            .shaped(para, Cx::text_width(para, self.geom.shape_width));
        let nlines = shaped.lines().len();
        let (pad_top, pad_bottom) = pads(para);
        let next_para = next.and_then(Block::as_paragraph);
        let has_box = props.borders.iter().any(Option::is_some) || props.shading.is_some();
        let same = |a: &Paragraph, b: &Paragraph| {
            a.props.borders == b.props.borders
                && a.props.shading == b.props.shading
                && a.props.indent_left == b.props.indent_left
                && a.props.indent_right == b.props.indent_right
        };
        let next_joined = has_box && next_para.is_some_and(|n| same(para, n));
        let break_lines: Vec<usize> = shaped
            .breaks
            .iter()
            .map(|(offset, _)| shaped.paragraph.line_of(*offset))
            .collect();
        // Where floating drawings go, worked out once where the paragraph
        // starts; text keeps clear of them.
        let mut fixed: Vec<(usize, Rect)> = Vec::new();
        let mut fixed_on: Option<(usize, usize)> = None;
        let mut line = 0;
        let mut first_fragment = true;
        let mut guard = 0;
        loop {
            guard += 1;
            if guard > nlines * 4 + 64 {
                break;
            }
            let (col_x, col_w) = self.col_rect();
            let joined = has_box
                && self.prev.as_deref().is_some_and(|p| {
                    (p.props.borders.iter().any(Option::is_some) || p.props.shading.is_some())
                        && same(p, para)
                });
            let base_space = match &self.prev {
                Some(prev) => gap(prev, para),
                None => pt(props.space_before),
            };
            let space = if line > 0 || (self.col_empty && self.overflowed && self.prev.is_none()) {
                if line > 0 { 0.0 } else { 0.0 }
            } else {
                base_space
            };
            let top_pad = if line == 0 && !joined { pad_top } else { 0.0 };
            let lines = shaped.lines();
            let bottom_edge = self.col_bottom();
            let y0 = self.y + space + top_pad;
            if line == 0 && !shaped.floats.is_empty() {
                let page_index = self.pages.len() - 1;
                if fixed_on.map(|(p, _)| p) != Some(page_index) {
                    if let Some((p, len)) = fixed_on
                        && let Some(page) = self.pages.get_mut(p)
                    {
                        page.obstructions.truncate(len);
                    }
                    fixed_on = Some((page_index, self.pages[page_index].obstructions.len()));
                    fixed.clear();
                    let ec = self.ec(false);
                    let origin_y = y0 - shaped.lines()[0].top;
                    let origin_x = col_x + pt(props.indent_left);
                    for (offset, drawing) in &shaped.floats {
                        let Some(float) = drawing.float else {
                            continue;
                        };
                        let (li, caret_x) = shaped.paragraph.caret(*offset);
                        let b = &shaped.lines()[li];
                        let rect = self.cx.float_rect(
                            drawing,
                            &float,
                            &ec,
                            (
                                origin_x + caret_x,
                                origin_y + b.top,
                                origin_y + b.top + b.height,
                            ),
                            origin_y,
                            origin_y + shaped.paragraph.height(),
                        );
                        fixed.push((*offset, rect));
                        if float.wrap != Wrap::None {
                            let [dl, dt, dr, db] = float.distance.map(emu_pt);
                            self.pages[page_index].obstructions.push(Rect::new(
                                rect.x0 - dl,
                                rect.y0 - dt,
                                rect.x1 + dr,
                                rect.y1 + db,
                            ));
                        }
                    }
                }
            }
            let mut used = 0.0f32;
            let mut k = 0usize;
            let mut pending: Vec<(usize, i32, Stack)> = Vec::new();
            let mut pending_height = 0.0f32;
            let mut bump: Option<f32> = None;
            let mut explicit = false;
            for j in line..nlines {
                let h = lines[j].height;
                let j_top = y0 + used;
                if let Some(b) = self.obstruction(j_top, j_top + h) {
                    bump = Some(b);
                    break;
                }
                let ids: Vec<i32> = pending.iter().map(|(_, id, _)| *id).collect();
                let new_notes = self.notes_of_line(&shaped, j, &ids);
                let mut extra: f32 = new_notes.iter().map(|(_, s)| s.height).sum();
                if extra > 0.0
                    && pending.is_empty()
                    && self.pages.last().is_some_and(|p| p.notes.is_empty())
                {
                    extra += NOTE_GAP;
                }
                let pad = if j + 1 == nlines && !next_joined {
                    pad_bottom
                } else {
                    0.0
                };
                if j_top + h + pad <= bottom_edge - pending_height - extra + EPS {
                    used += h;
                    k += 1;
                    pending_height += extra;
                    for (id, stack) in new_notes {
                        pending.push((j, id, stack));
                    }
                } else {
                    break;
                }
                if break_lines.contains(&j) {
                    explicit = true;
                    break;
                }
            }
            let fit = k;
            let remaining = nlines - line;
            if k < remaining && !explicit && bump.is_none() {
                if props.keep_lines && line == 0 && !self.col_empty {
                    k = 0;
                } else if props.widow_control {
                    if remaining - k < 2 {
                        k = remaining.saturating_sub(2);
                    }
                    if line == 0 && k < 2 && remaining >= 2 {
                        k = 0;
                    }
                }
                if k == 0 && self.col_empty && self.prev.is_none() {
                    k = fit.max(1).min(remaining);
                }
            }
            if k == 0 && bump.is_none() && fit == 0 && self.col_empty && self.prev.is_none() {
                k = 1;
            }
            // The notes of the lines that stay go on the page.
            {
                let page = self.pages.last_mut().expect("page");
                for (j, id, stack) in pending {
                    if j < line + k {
                        page.notes.push((id, stack));
                    }
                }
            }
            self.recompute_note_height();
            if k == 0 {
                if let Some(b) = bump {
                    // Text goes below the drawing in the way.
                    self.y = (b - space - top_pad).max(self.y + EPS);
                    let page_bottom = self.col_bottom();
                    if self.y > page_bottom + EPS {
                        self.advance(false);
                    }
                    continue;
                }
                self.advance(false);
                continue;
            }
            // Put lines `line..line + k` on the page.
            let origin_y = y0 - lines[line].top;
            let (x, _) = (col_x, col_w);
            let bx = has_box.then(|| ParaBox {
                borders: props.borders,
                between: props.border_between,
                shading: props.shading,
                join_prev: joined,
                join_next: next_joined,
                gap_prev: base_space,
                gap_next: next_para.map_or(0.0, |n| gap(para, n)),
            });
            let entry = ParaEntry {
                para: para.clone(),
                shaped: shaped.clone(),
                path: path.clone(),
                x: x + pt(props.indent_left),
                top: 0.0,
                bx,
                floats: if line == 0 { fixed.clone() } else { Vec::new() },
            };
            let ec = self.ec(path.is_some());
            let end_line = line + k;
            let mut layers = std::mem::take(&mut self.pages.last_mut().expect("page").layers);
            let clear = {
                let mut out = Out {
                    layers: &mut layers,
                    frags: &mut self.frags,
                };
                self.cx.emit_para(
                    &entry,
                    line..end_line,
                    x + pt(props.indent_left),
                    origin_y,
                    &ec,
                    &mut out,
                )
            };
            let used_height = used
                .min(lines[end_line - 1].top + lines[end_line - 1].height - lines[line].top)
                .max(0.0);
            let _ = used_height;
            let bottom = origin_y + lines[end_line - 1].top + lines[end_line - 1].height;
            let last = end_line == nlines;
            let end_y = bottom
                + if last && !next_joined {
                    pad_bottom
                } else {
                    0.0
                };
            {
                let page = self.pages.last_mut().expect("page");
                page.layers = layers;
                page.used = true;
                page.obstructions.extend(clear);
                let region = page.regions.last_mut().expect("region");
                region.used_bottom = region.used_bottom.max(end_y);
            }
            if first_fragment
                && let Some(level) = props.outline_level
                && let Some(p) = &path
            {
                let page_index = self.pages.len() - 1;
                self.outline.push(OutlineEntry {
                    level,
                    text: outline_text(para),
                    pos: Pos::new(p.clone(), 0),
                    page: page_index,
                });
            }
            first_fragment = false;
            self.y = end_y;
            self.col_empty = false;
            self.overflowed = false;
            self.prev = last.then(|| para.clone());
            line = end_line;
            if explicit {
                self.advance(true);
                if line >= nlines {
                    break;
                }
            } else if line < nlines {
                self.advance(false);
            } else {
                break;
            }
        }
        if nlines == 0 {
            self.prev = Some(para.clone());
        }
    }

    fn recompute_note_height(&mut self) {
        let page = self.pages.last_mut().expect("page");
        let sum: f32 = page.notes.iter().map(|(_, s)| s.height).sum();
        page.note_height = if page.notes.is_empty() {
            0.0
        } else {
            sum + NOTE_GAP
        };
    }

    /// The notes referenced in line `j` that are not on the page yet.
    fn notes_of_line(
        &mut self,
        shaped: &crate::shape::Shaped,
        j: usize,
        pending: &[i32],
    ) -> Vec<(i32, Stack)> {
        let mut out = Vec::new();
        if shaped.notes.is_empty() {
            return out;
        }
        for (offset, id, end) in &shaped.notes {
            if *end || shaped.paragraph.line_of(*offset) != j {
                continue;
            }
            let on_page = self
                .pages
                .last()
                .is_some_and(|p| p.notes.iter().any(|(n, _)| n == id));
            if on_page || pending.contains(id) || out.iter().any(|(n, _)| n == id) {
                continue;
            }
            let Some(note) = self.doc.footnotes.notes.iter().find(|n| n.id == *id) else {
                continue;
            };
            let width = self.pages.last().map_or(1.0, |p| p.margin.width());
            let stack = self.cx.layout_stack(&note.blocks, &Paths::None, width, 0);
            out.push((*id, stack));
        }
        out
    }

    fn place_table(&mut self, path: Path, table: &Arc<Table>) {
        let (col_x, col_w) = self.col_rect();
        if let Some(prev) = self.prev.take()
            && !self.col_empty
        {
            self.y += pt(prev.props.space_after.max(0));
        }
        let layout = self.cx.layout_table(table, Some(&path), col_w, 0);
        let ox = col_x + layout.x;
        let nrows = layout.rows.len();
        let header_rows = layout.header_rows().min(nrows.saturating_sub(1));
        let mut batch: Vec<Slice> = Vec::new();
        let mut batch_y = self.y;
        let mut r = 0;
        let mut c0 = 0.0f32;
        let mut guard = 0usize;
        while r < nrows {
            guard += 1;
            if guard > nrows * 8 + 10_000_000 {
                break;
            }
            let row = &layout.rows[r];
            let remaining = row.height - c0;
            let avail = self.col_bottom() - self.y;
            if remaining <= avail + EPS {
                batch.push(Slice {
                    row: r,
                    c0,
                    c1: row.height,
                    repeat: false,
                });
                self.y += remaining;
                self.col_empty = false;
                self.overflowed = false;
                r += 1;
                c0 = 0.0;
                continue;
            }
            let at_top = self.col_empty && batch.iter().all(|s| s.repeat);
            let capacity = self.col_bottom() - self.region().top;
            if c0 == 0.0 && !at_top && row.height <= capacity {
                self.flush_rows(&layout, &mut batch, ox, batch_y);
                self.advance(false);
                batch_y = self.y;
                self.repeat_headers(&layout, header_rows, r, &mut batch);
                continue;
            }
            let target = c0 + avail.max(0.0);
            let c1 = layout.cut(r, c0, target);
            if !at_top && (avail <= EPS || c1 <= c0 + EPS) {
                self.flush_rows(&layout, &mut batch, ox, batch_y);
                self.advance(false);
                batch_y = self.y;
                self.repeat_headers(&layout, header_rows, r, &mut batch);
                continue;
            }
            let c1 = c1.max(c0 + EPS.min(row.height - c0)).min(row.height);
            batch.push(Slice {
                row: r,
                c0,
                c1,
                repeat: false,
            });
            self.y += c1 - c0;
            self.col_empty = false;
            self.overflowed = false;
            if c1 >= row.height - EPS {
                r += 1;
                c0 = 0.0;
            } else {
                c0 = c1;
                self.flush_rows(&layout, &mut batch, ox, batch_y);
                self.advance(false);
                batch_y = self.y;
                self.repeat_headers(&layout, header_rows, r, &mut batch);
            }
        }
        self.flush_rows(&layout, &mut batch, ox, batch_y);
        self.prev = None;
    }

    /// At the top of a page a table continues on: its header rows again.
    fn repeat_headers(
        &mut self,
        layout: &TableLayout,
        header_rows: usize,
        next_row: usize,
        batch: &mut Vec<Slice>,
    ) {
        if next_row < header_rows || header_rows == 0 {
            return;
        }
        for h in 0..header_rows {
            batch.push(Slice {
                row: h,
                c0: 0.0,
                c1: layout.rows[h].height,
                repeat: true,
            });
            self.y += layout.rows[h].height;
        }
    }

    fn flush_rows(&mut self, layout: &TableLayout, batch: &mut Vec<Slice>, ox: f32, y: f32) {
        if batch.is_empty() {
            return;
        }
        let ec = self.ec(true);
        let mut layers = std::mem::take(&mut self.pages.last_mut().expect("page").layers);
        let height = {
            let mut out = Out {
                layers: &mut layers,
                frags: &mut self.frags,
            };
            self.cx.emit_row_slices(layout, batch, ox, y, &ec, &mut out)
        };
        let page = self.pages.last_mut().expect("page");
        page.layers = layers;
        page.used = true;
        let region = page.regions.last_mut().expect("region");
        region.used_bottom = region.used_bottom.max(y + height);
        batch.clear();
    }

    fn place_endnotes(&mut self) {
        if self.cx.notes.end.is_empty() || self.pages.is_empty() {
            return;
        }
        let mut order: Vec<(u32, i32)> =
            self.cx.notes.end.iter().map(|(id, n)| (*n, *id)).collect();
        order.sort();
        // A short rule, then the notes.
        let (col_x, col_w) = self.col_rect();
        if self.y + 12.0 > self.col_bottom() {
            self.advance(false);
        }
        let y = self.y + 6.0;
        let width = (col_w / 3.0).min(144.0);
        let page = self.page();
        page.layers.rules.push(Item::Rule {
            from: (col_x, y),
            to: (col_x + width, y),
            width: 0.5,
            color: [0, 0, 0, 255],
            style: RuleStyle::Single,
        });
        self.y = y + 6.0;
        self.prev = None;
        for (_, id) in order {
            let Some(note) = self.doc.endnotes.notes.iter().find(|n| n.id == id) else {
                continue;
            };
            for (i, block) in note.blocks.iter().enumerate() {
                match block {
                    Block::Paragraph(p) => self.place_with_chain(&note.blocks, i, None, p),
                    Block::Table(_) => {}
                }
            }
        }
    }

    /// Headers, footers, footnotes and column separators, now that the
    /// numbers of pages are known; then the pages.
    fn finish(mut self) -> Layout {
        if self.pages.is_empty() {
            self.new_page();
        }
        let count = self.pages.len();
        let section_pages = self.sections_pages.clone();
        let sections = self.doc.sections();
        let mut pages = Vec::with_capacity(count);
        let states = std::mem::take(&mut self.pages);
        for (index, mut state) in states.into_iter().enumerate() {
            let sec = state.sec.clone();
            let label = format_number(state.number, &sec.page_number_format);
            let values = self.page_values(
                state.number,
                count,
                section_pages.get(state.section).copied().unwrap_or(1),
                &sec,
            );
            let width = state.margin.width();
            let page_rect = Rect::new(0.0, 0.0, state.width, state.height);
            let ec = EmitCtx {
                page: index,
                page_rect,
                margin: state.margin,
                column: state.margin,
                bounds: None,
                repeat: false,
                record: false,
                depth: 0,
            };
            self.cx.grid = sections
                .get(state.section)
                .and_then(|(_, s)| s.grid)
                .map(|g| pt(g.line_pitch))
                .filter(|p| *p > 0.0);
            let header = self.marginal_stack(state.header, width, &values);
            let footer = self.marginal_stack(state.footer, width, &values);
            let mut layers = std::mem::take(&mut state.layers);
            let mut scratch = Vec::new();
            {
                let mut out = Out {
                    layers: &mut layers,
                    frags: &mut scratch,
                };
                if let Some(stack) = &header {
                    let y = pt(sec.margins.header).max(0.0);
                    self.cx.emit_stack(
                        stack,
                        state.margin.x0,
                        y,
                        f32::NEG_INFINITY,
                        f32::INFINITY,
                        &ec,
                        &mut out,
                    );
                }
                if let Some(stack) = &footer {
                    let y = state.height - pt(sec.margins.footer).max(0.0) - stack.height;
                    self.cx.emit_stack(
                        stack,
                        state.margin.x0,
                        y,
                        f32::NEG_INFINITY,
                        f32::INFINITY,
                        &ec,
                        &mut out,
                    );
                }
                if !state.notes.is_empty() {
                    let total = state.note_height;
                    let mut y = state.body_bottom - total + NOTE_GAP;
                    let rule_y = y - NOTE_GAP / 2.0;
                    out.layers.rules.push(Item::Rule {
                        from: (state.margin.x0, rule_y),
                        to: (state.margin.x0 + (width / 3.0).min(144.0), rule_y),
                        width: 0.5,
                        color: [0, 0, 0, 255],
                        style: RuleStyle::Single,
                    });
                    for (_, stack) in &state.notes {
                        self.cx.emit_stack(
                            stack,
                            state.margin.x0,
                            y,
                            f32::NEG_INFINITY,
                            f32::INFINITY,
                            &ec,
                            &mut out,
                        );
                        y += stack.height;
                    }
                }
                for region in &state.regions {
                    if region.separator && region.cols.len() > 1 {
                        for pair in region.cols.windows(2) {
                            let x = (pair[0].0 + pair[0].1 + pair[1].0) / 2.0;
                            out.layers.rules.push(Item::Rule {
                                from: (x, region.top),
                                to: (x, region.used_bottom.max(region.top + 1.0)),
                                width: 0.5,
                                color: [0, 0, 0, 255],
                                style: RuleStyle::Single,
                            });
                        }
                    }
                }
            }
            let _ = (
                state.header_height,
                state.footer_height,
                state.page_in_section,
                emu_pt(0),
            );
            pages.push(Page {
                width: state.width,
                height: state.height,
                number: state.number,
                label,
                section: state.section,
                items: layers.into_items(),
            });
        }
        Layout::new(pages, self.frags, self.outline)
    }
}

#[allow(dead_code)]
fn _unused(_: Break) {}
