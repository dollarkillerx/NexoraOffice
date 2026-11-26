//! The result of laying a document out, and the questions an editor asks of
//! it: which place in the text is under a point, where a place is, what a
//! selection covers, where the arrow keys go.
//!
//! The body's text is answered for (table cells included), and so is the
//! text of headers, footers, footnotes and endnotes: their paragraphs have
//! paths that name their story ([`aster_writer::StoryId`]). The plain
//! queries ([`Layout::hit`], [`Layout::caret`], ...) take a place of the
//! body or a note. A header or footer is shown on many pages, so for the
//! places in one the `_on` variants take the page; [`Layout::hit_in`] and
//! [`Layout::story_at`] say which story a point of a page is in. Text boxes
//! are drawn but cannot be pointed at.

use std::collections::HashMap;

use aster_writer::{Document, MarginalSlot, Path, Pos, StoryId};

use crate::cx::Frag;
use crate::display::{Page, Rect};

/// A heading: a paragraph with an outline level.
#[derive(Clone, Debug, PartialEq)]
pub struct OutlineEntry {
    /// 0 for the top level.
    pub level: u8,
    pub text: String,
    pub pos: Pos,
    /// Index of the page it starts on.
    pub page: usize,
}

/// Where text with a hyperlink is on a page.
#[derive(Clone, Debug, PartialEq)]
pub struct LinkRect {
    pub page: usize,
    /// One line of the linked text.
    pub rect: Rect,
    /// A URL, or `#name` for a bookmark of the document.
    pub target: String,
}

/// Where the caret goes: a bar `height` tall from `top`, at `x`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Caret {
    pub page: usize,
    pub x: f32,
    pub top: f32,
    pub height: f32,
}

/// What a page shows around its body.
#[derive(Clone, Debug, PartialEq)]
pub struct PageInfo {
    /// Index into `Document::sections()`.
    pub section: usize,
    /// The header and the footer story the page shows, as indexes into
    /// `Document::marginals`; `None` where the section has none.
    pub header: Option<usize>,
    pub footer: Option<usize>,
    /// Which header and footer of its section the page takes: the
    /// slot to make one in when there is none.
    pub slot: MarginalSlot,
    /// The strip at the top of the page above the body, and the strip at
    /// the bottom below it (where the notes end). Whether or not there is a
    /// header or footer in them.
    pub header_area: Rect,
    pub footer_area: Rect,
    /// The box the body's text is set in: the page less its margins.
    pub margin: Rect,
}

/// The part of a table's rows that is on one page, and where its grid lines
/// are (page points). A table split over pages, or one in a header, which
/// is on every page, has one for each page.
#[derive(Clone, Debug, PartialEq)]
pub struct TableGrid {
    /// The table's block.
    pub table: Path,
    pub page: usize,
    /// The vertical grid lines, left to right: one more than the table has
    /// grid columns.
    pub columns: Vec<f32>,
    /// The rows here, top to bottom.
    pub rows: Vec<GridRow>,
}

/// A row, or the part of it that is on a page.
#[derive(Clone, Debug, PartialEq)]
pub struct GridRow {
    /// Index into the table's rows.
    pub row: usize,
    pub top: f32,
    pub bottom: f32,
    /// How high the whole row is.
    pub height: f32,
    /// This is where the row starts / ends (it is not a piece of a row that
    /// goes on over the page).
    pub starts: bool,
    pub finishes: bool,
    /// A copy of a header row at the top of a later page.
    pub repeat: bool,
    /// For each grid line (the same number as `columns`): whether a cell of
    /// this row ends there; a line a merged cell hides is not one.
    pub edges: Vec<bool>,
    /// For each grid column: whether the cell over it ends with this row
    /// (not for one merged with the row below, and not where there is no
    /// cell).
    pub ends: Vec<bool>,
}

/// A line of a table that can be dragged.
#[derive(Clone, Debug, PartialEq)]
pub enum TableBorder {
    /// The vertical line `border` (0 is the table's left edge, as many as
    /// there are grid columns the right one) at page x `x`, which runs from
    /// `y0` to `y1` on the page.
    Column {
        table: Path,
        border: usize,
        page: usize,
        x: f32,
        y0: f32,
        y1: f32,
    },
    /// The bottom of row `row`, at page y `y`, across `x0..x1`; `height` is
    /// the row's height now.
    Row {
        table: Path,
        row: usize,
        page: usize,
        y: f32,
        x0: f32,
        x1: f32,
        height: f32,
    },
}

/// A laid-out document.
pub struct Layout {
    /// At least one.
    pub pages: Vec<Page>,
    pub(crate) frags: Vec<Frag>,
    by_path: HashMap<Path, Vec<usize>>,
    /// Covered cells of vertical merges: the cell, and the start of the merge.
    redirects: HashMap<Path, Path>,
    outline: Vec<OutlineEntry>,
    infos: Vec<PageInfo>,
    grids: Vec<TableGrid>,
}

impl Layout {
    pub(crate) fn new(
        pages: Vec<Page>,
        frags: Vec<Frag>,
        outline: Vec<OutlineEntry>,
        redirects: Vec<(Path, Path)>,
    ) -> Self {
        let mut by_path: HashMap<Path, Vec<usize>> = HashMap::new();
        for (index, frag) in frags.iter().enumerate() {
            if !frag.repeat {
                by_path.entry(frag.path.clone()).or_default().push(index);
            }
        }
        Self {
            pages,
            frags,
            by_path,
            redirects: redirects.into_iter().collect(),
            outline,
            infos: Vec::new(),
            grids: Vec::new(),
        }
    }

    pub(crate) fn with_grids(mut self, grids: Vec<TableGrid>) -> Self {
        self.grids = grids;
        self
    }

    pub(crate) fn with_page_info(mut self, infos: Vec<PageInfo>) -> Self {
        self.infos = infos;
        self
    }

    /// The grid lines of every table with rows on `page`, outermost first
    /// for tables inside one another.
    pub fn table_grids(&self, page: usize) -> impl Iterator<Item = &TableGrid> {
        self.grids.iter().filter(move |grid| grid.page == page)
    }

    /// The line of a table that is within `reach` points of a point of
    /// `page`, the nearest (the innermost table for a tie). A column line is
    /// only where a cell of the row ends, and a row's bottom only where a
    /// cell ends with the row and the row ends on this page, so that a line
    /// a merged cell hides is not found. `marginal` says whether the tables
    /// of headers and footers are asked about or those of the body and
    /// the notes.
    pub fn table_border_at(
        &self,
        page: usize,
        x: f32,
        y: f32,
        reach: f32,
        marginal: bool,
    ) -> Option<TableBorder> {
        let mut best: Option<(f32, usize, TableBorder)> = None;
        let mut consider = |distance: f32, depth: usize, border: TableBorder| {
            let better = best.as_ref().is_none_or(|(d, deep, _)| {
                distance < *d - 0.001 || (distance < *d + 0.001 && depth > *deep)
            });
            if better {
                best = Some((distance, depth, border));
            }
        };
        for grid in self.table_grids(page) {
            if matches!(grid.table.story, StoryId::Marginal(_)) != marginal {
                continue;
            }
            let (Some(first), Some(last)) = (grid.rows.first(), grid.rows.last()) else {
                continue;
            };
            let depth = grid.table.steps.len();
            let (left, right) = (
                grid.columns.first().copied().unwrap_or(0.0),
                grid.columns.last().copied().unwrap_or(0.0),
            );
            if y >= first.top - reach && y <= last.bottom + reach {
                for (border, line) in grid.columns.iter().enumerate() {
                    let distance = (x - line).abs();
                    let drawn = grid
                        .rows
                        .iter()
                        .any(|r| y >= r.top - 0.01 && y <= r.bottom + 0.01 && r.edges[border]);
                    if distance <= reach && drawn {
                        consider(
                            distance,
                            depth,
                            TableBorder::Column {
                                table: grid.table.clone(),
                                border,
                                page,
                                x: *line,
                                y0: first.top,
                                y1: last.bottom,
                            },
                        );
                    }
                }
            }
            if x >= left && x <= right {
                for row in grid.rows.iter().filter(|r| r.finishes) {
                    let distance = (y - row.bottom).abs();
                    let over_end = grid.columns.windows(2).enumerate().any(|(c, pair)| {
                        x >= pair[0] && x <= pair[1] && row.ends.get(c).copied().unwrap_or(false)
                    });
                    if distance <= reach && over_end {
                        consider(
                            distance,
                            depth,
                            TableBorder::Row {
                                table: grid.table.clone(),
                                row: row.row,
                                page,
                                y: row.bottom,
                                x0: left,
                                x1: right,
                                height: row.height,
                            },
                        );
                    }
                }
            }
        }
        best.map(|(_, _, border)| border)
    }

    /// The area rows `rows.0..=rows.1` and grid columns `columns.0..columns.1`
    /// of the table at `table` take, one rectangle per page it is on (whole
    /// cells, borders and all). For a table of a header or footer, `page`
    /// limits it to the page it is shown on.
    pub fn cell_rects(
        &self,
        page: Option<usize>,
        table: &Path,
        rows: (usize, usize),
        columns: (usize, usize),
    ) -> Vec<(usize, Rect)> {
        let single = matches!(table.story, StoryId::Marginal(_));
        let mut out = Vec::new();
        for grid in &self.grids {
            if grid.table != *table || (single && page.is_some_and(|p| p != grid.page)) {
                continue;
            }
            let (Some(x0), Some(x1)) = (grid.columns.get(columns.0), grid.columns.get(columns.1))
            else {
                continue;
            };
            let mut wanted = grid
                .rows
                .iter()
                .filter(|r| r.row >= rows.0 && r.row <= rows.1);
            let Some(first) = wanted.next() else {
                continue;
            };
            let bottom = wanted.last().map_or(first.bottom, |r| r.bottom);
            out.push((grid.page, Rect::new(*x0, first.top, *x1, bottom)));
        }
        out
    }

    /// What `page` shows around its body.
    pub fn page_info(&self, page: usize) -> Option<&PageInfo> {
        self.infos.get(page)
    }

    /// The story whose area a point of `page` is in: the header or footer
    /// the page shows when the point is in the strip above or below the
    /// body, a note when it is over one, else the body. `None` in the
    /// strip of a header or footer the page does not have (a double-click
    /// there still means to make one: see [`Layout::page_info`]).
    pub fn story_at(&self, page: usize, x: f32, y: f32) -> Option<StoryId> {
        if let Some(info) = self.infos.get(page) {
            if y < info.header_area.y1 {
                return info.header.map(StoryId::Marginal);
            }
            if y > info.footer_area.y0 {
                return info.footer.map(StoryId::Marginal);
            }
        }
        let note = self.frags.iter().find(|frag| {
            frag.page == page
                && matches!(frag.path.story, StoryId::Footnote(_) | StoryId::Endnote(_))
                && frag.bounds.contains(x, y)
        });
        Some(note.map_or(StoryId::Body, |frag| frag.path.story))
    }

    /// Every paragraph with an outline level, in reading order.
    pub fn outline(&self) -> &[OutlineEntry] {
        &self.outline
    }

    /// The fragment of `pos`'s paragraph that has its place, and the line.
    /// The place a position stands for: itself, or for a cell a vertical
    /// merge covers, the start of the merge.
    fn resolve<'a>(&self, pos: &'a Pos) -> std::borrow::Cow<'a, Pos> {
        if self.by_path.contains_key(&pos.path) || self.redirects.is_empty() {
            return std::borrow::Cow::Borrowed(pos);
        }
        // The cell is the path up to some `[row, cell]`, then anything.
        for n in (2..pos.path.steps.len()).rev() {
            if let Some(start) = self
                .redirects
                .get(&pos.path.with_steps(pos.path.steps[..n].to_vec()))
            {
                return std::borrow::Cow::Owned(Pos::new(start.clone(), 0));
            }
        }
        std::borrow::Cow::Borrowed(pos)
    }

    fn locate(&self, pos: &Pos) -> Option<(usize, usize)> {
        self.locate_on(None, pos)
    }

    /// The fragment and line of a place. A header or footer is on many
    /// pages: with `page` it is the one on that page.
    fn locate_on(&self, page: Option<usize>, pos: &Pos) -> Option<(usize, usize)> {
        let pos = &*self.resolve(pos);
        let all = self.by_path.get(&pos.path)?;
        let on_page: Vec<usize>;
        let indexes: &Vec<usize> = match page {
            Some(page) if matches!(pos.path.story, StoryId::Marginal(_)) => {
                on_page = all
                    .iter()
                    .copied()
                    .filter(|i| self.frags[*i].page == page)
                    .collect();
                &on_page
            }
            _ => all,
        };
        let first = &self.frags[*indexes.first()?];
        let off = first.shaped.to_shaped(pos.offset);
        let line = first.shaped.paragraph.line_of(off);
        let found = indexes
            .iter()
            .copied()
            .find(|i| self.frags[*i].lines.contains(&line))
            .unwrap_or(indexes[0]);
        let frag = &self.frags[found];
        let line = line.clamp(frag.lines.start, frag.lines.end.saturating_sub(1));
        Some((found, line))
    }

    /// The page a place is on.
    pub fn page_of(&self, pos: &Pos) -> Option<usize> {
        self.locate(pos).map(|(frag, _)| self.frags[frag].page)
    }

    /// Every page a place can be seen on: the pages that show the story
    /// for a header or footer, the one page of the line for anything else.
    pub fn pages_of(&self, pos: &Pos) -> Vec<usize> {
        if !matches!(pos.path.story, StoryId::Marginal(_)) {
            return self.page_of(pos).into_iter().collect();
        }
        let Some(indexes) = self.by_path.get(&self.resolve(pos).path) else {
            return Vec::new();
        };
        let mut pages: Vec<usize> = indexes.iter().map(|i| self.frags[*i].page).collect();
        pages.dedup();
        pages
    }

    /// Where the caret is at `pos`; `None` for a path that is not in the
    /// body or has not been placed.
    pub fn caret(&self, pos: &Pos) -> Option<Caret> {
        self.caret_on(None, pos)
    }

    /// The same for a place of a header or footer as shown on `page`.
    pub fn caret_on(&self, page: Option<usize>, pos: &Pos) -> Option<Caret> {
        let pos = &*self.resolve(pos);
        let (index, line) = self.locate_on(page, pos)?;
        let frag = &self.frags[index];
        let paragraph = &frag.shaped.paragraph;
        let off = frag.shaped.to_shaped(pos.offset);
        let (caret_line, x) = paragraph.caret(off);
        // A place at the break between two fragments is on the later one.
        let line = if frag.lines.contains(&caret_line) {
            caret_line
        } else {
            line
        };
        let (x, line_box) = if line == caret_line {
            (x, &paragraph.lines()[line])
        } else {
            let l = &paragraph.lines()[line];
            (l.left, l)
        };
        Some(Caret {
            page: frag.page,
            x: frag.x + x,
            top: frag.y + line_box.top,
            height: line_box.height,
        })
    }

    /// The place nearest to a point of a page. A click beside or below a
    /// line goes to the nearest line and its nearest end.
    ///
    /// Only the body answers: use [`Layout::hit_in`] for the other stories.
    pub fn hit(&self, page: usize, x: f32, y: f32) -> Option<Pos> {
        self.hit_in(page, StoryId::Body, x, y)
    }

    /// The same among the paragraphs of one story as shown on `page`.
    pub fn hit_in(&self, page: usize, story: StoryId, x: f32, y: f32) -> Option<Pos> {
        let mut best: Option<(f32, f32, usize)> = None;
        for (index, frag) in self.frags.iter().enumerate() {
            if frag.page != page || frag.path.story != story {
                continue;
            }
            let b = &frag.bounds;
            let dy = (b.y0 - y).max(y - b.y1).max(0.0);
            let dx = (b.x0 - x).max(x - b.x1).max(0.0);
            let better = match best {
                None => true,
                Some((by, bx, _)) => dy < by - 0.001 || (dy < by + 0.001 && dx <= bx),
            };
            if better {
                best = Some((dy, dx, index));
            }
        }
        let (_, _, index) = best?;
        Some(self.pos_in(index, x, y))
    }

    fn pos_in(&self, index: usize, x: f32, y: f32) -> Pos {
        let frag = &self.frags[index];
        let lines = frag.shaped.paragraph.lines();
        let first = &lines[frag.lines.start];
        let last = &lines[frag.lines.end - 1];
        let local_y = (y - frag.y).clamp(first.top, (last.top + last.height - 0.01).max(first.top));
        let off = frag.shaped.paragraph.hit(x - frag.x, local_y);
        Pos::new(frag.path.clone(), frag.shaped.to_orig(off))
    }

    /// The rectangles a selection from `a` to `b` covers, per page.
    pub fn selection(&self, a: &Pos, b: &Pos) -> Vec<(usize, Rect)> {
        self.selection_on(None, a, b)
    }

    /// The same for a selection in a header or footer as shown on `page`.
    /// A selection lives in one story: none for two places in different
    /// ones.
    pub fn selection_on(&self, page: Option<usize>, a: &Pos, b: &Pos) -> Vec<(usize, Rect)> {
        let (from, to) = if a <= b { (a, b) } else { (b, a) };
        let mut out = Vec::new();
        if from == to || from.path.story != to.path.story {
            return out;
        }
        let single = matches!(from.path.story, StoryId::Marginal(_));
        for frag in &self.frags {
            if frag.repeat
                || frag.path < from.path
                || frag.path > to.path
                || (single && page.is_some_and(|p| p != frag.page))
            {
                continue;
            }
            let shaped = &frag.shaped;
            let start = if frag.path == from.path {
                shaped.to_shaped(from.offset)
            } else {
                0
            };
            let (end, through) = if frag.path == to.path {
                (shaped.to_shaped(to.offset), false)
            } else {
                (usize::MAX, true)
            };
            let paragraph = &shaped.paragraph;
            for r in paragraph.rects_in(start..end, frag.lines.clone()) {
                out.push((
                    frag.page,
                    Rect::new(frag.x + r[0], frag.y + r[1], frag.x + r[2], frag.y + r[3]),
                ));
            }
            // The paragraph mark of a paragraph the selection goes past.
            if through && frag.lines.end >= paragraph.lines().len() {
                let line = &paragraph.lines()[frag.lines.end - 1];
                let (_, x) = paragraph.caret(paragraph.text().len());
                let x = frag.x + x;
                out.push((
                    frag.page,
                    Rect::new(
                        x,
                        frag.y + line.top,
                        x + 4.0,
                        frag.y + line.top + line.height,
                    ),
                ));
            }
        }
        out
    }

    /// The rectangles of linked text, one per line of it, in reading order.
    /// `document` must be the one this was laid out from. Like the other
    /// queries, only the body's text is covered: links in headers, footers,
    /// notes and text boxes are not reported.
    pub fn links(&self, document: &Document) -> Vec<LinkRect> {
        let mut out = Vec::new();
        for frag in &self.frags {
            if frag.repeat {
                continue;
            }
            let Some(paragraph) = document.paragraph(&frag.path) else {
                continue;
            };
            let shaped = &frag.shaped;
            let mut start = 0;
            for run in &paragraph.runs {
                let end = start + run.text.len();
                if let Some(link) = &run.link
                    && end > start
                    && !link.target.is_empty()
                {
                    let range = shaped.to_shaped(start)..shaped.to_shaped(end);
                    for r in shaped.paragraph.rects_in(range, frag.lines.clone()) {
                        let rect =
                            Rect::new(frag.x + r[0], frag.y + r[1], frag.x + r[2], frag.y + r[3]);
                        // The runs of one link are separate runs; one
                        // rectangle serves for the pieces on a line.
                        match out.last_mut() {
                            Some(LinkRect {
                                page,
                                rect: last,
                                target,
                            }) if *page == frag.page
                                && *target == link.target
                                && (last.y0 - rect.y0).abs() < 0.01
                                && (last.y1 - rect.y1).abs() < 0.01
                                && (rect.x0 - last.x1).abs() < 0.5 =>
                            {
                                last.x1 = last.x1.max(rect.x1);
                            }
                            _ => out.push(LinkRect {
                                page: frag.page,
                                rect,
                                target: link.target.clone(),
                            }),
                        }
                    }
                }
                start = end;
            }
        }
        out
    }

    /// The first place on the line `pos` is on.
    pub fn line_start(&self, pos: &Pos) -> Option<Pos> {
        self.line_start_on(None, pos)
    }

    pub fn line_start_on(&self, page: Option<usize>, pos: &Pos) -> Option<Pos> {
        let (index, line) = self.locate_on(page, pos)?;
        let frag = &self.frags[index];
        let off = frag.shaped.paragraph.line_start_offset(line);
        Some(Pos::new(frag.path.clone(), frag.shaped.to_orig(off)))
    }

    /// The last place on the line `pos` is on (before the space a wrapped
    /// line gave up).
    pub fn line_end(&self, pos: &Pos) -> Option<Pos> {
        self.line_end_on(None, pos)
    }

    pub fn line_end_on(&self, page: Option<usize>, pos: &Pos) -> Option<Pos> {
        let (index, line) = self.locate_on(page, pos)?;
        let frag = &self.frags[index];
        let off = frag.shaped.paragraph.line_end_offset(line);
        Some(Pos::new(frag.path.clone(), frag.shaped.to_orig(off)))
    }

    /// The place on the line above, as near `goal_x` (a page x) as it gets.
    pub fn above(&self, pos: &Pos, goal_x: f32) -> Option<Pos> {
        self.step(None, pos, goal_x, true)
    }

    /// The place on the line below.
    pub fn below(&self, pos: &Pos, goal_x: f32) -> Option<Pos> {
        self.step(None, pos, goal_x, false)
    }

    /// The same for places of a header or footer as shown on `page`. They
    /// stay in the story: the first line has nothing above it.
    pub fn above_on(&self, page: Option<usize>, pos: &Pos, goal_x: f32) -> Option<Pos> {
        self.step(page, pos, goal_x, true)
    }

    pub fn below_on(&self, page: Option<usize>, pos: &Pos, goal_x: f32) -> Option<Pos> {
        self.step(page, pos, goal_x, false)
    }

    fn step(&self, page: Option<usize>, pos: &Pos, goal_x: f32, up: bool) -> Option<Pos> {
        let (index, line) = self.locate_on(page, pos)?;
        let frag = &self.frags[index];
        let target = if up && line > frag.lines.start {
            Some((index, line - 1))
        } else if !up && line + 1 < frag.lines.end {
            Some((index, line + 1))
        } else {
            self.other_paragraph(index, line, goal_x, up)
        };
        let (target, line) = target?;
        let frag = &self.frags[target];
        let l = &frag.shaped.paragraph.lines()[line];
        let y = frag.y + l.top + l.height / 2.0;
        Some(self.pos_in(target, goal_x, y))
    }

    /// The line above or below the one at `line` of fragment `index`, in
    /// another fragment: in the same table by geometry first, then in
    /// reading order.
    fn other_paragraph(
        &self,
        index: usize,
        line: usize,
        goal_x: f32,
        up: bool,
    ) -> Option<(usize, usize)> {
        let frag = &self.frags[index];
        let l = &frag.shaped.paragraph.lines()[line];
        let (top, bottom) = (frag.y + l.top, frag.y + l.top + l.height);
        let mut steps = frag.path.steps.clone();
        while steps.len() >= 4 {
            steps.truncate(steps.len() - 3);
            let mut best: Option<(f32, usize, usize)> = None;
            for (i, g) in self.frags.iter().enumerate() {
                if i == index
                    || g.repeat
                    || g.page != frag.page
                    || g.path.story != frag.path.story
                    || !g.path.steps.starts_with(&steps)
                    || g.path.steps.len() == steps.len()
                    || goal_x < g.bounds.x0
                    || goal_x > g.bounds.x1
                {
                    continue;
                }
                let lines = g.shaped.paragraph.lines();
                for n in g.lines.clone() {
                    let (t, b) = (g.y + lines[n].top, g.y + lines[n].top + lines[n].height);
                    let distance = if up { top - b } else { t - bottom };
                    if distance >= -0.5 && best.is_none_or(|(d, _, _)| distance < d) {
                        best = Some((distance, i, n));
                    }
                }
            }
            if let Some((_, i, n)) = best {
                return Some((i, n));
            }
        }
        // Reading order, leaving the tables the place is in.
        let inside = |g: &Frag| {
            let mut steps = frag.path.steps.clone();
            while steps.len() >= 4 {
                steps.truncate(steps.len() - 3);
                if g.path.steps.starts_with(&steps) {
                    return true;
                }
            }
            false
        };
        let candidates: Box<dyn Iterator<Item = usize>> = if up {
            Box::new((0..index).rev())
        } else {
            Box::new(index + 1..self.frags.len())
        };
        for i in candidates {
            let g = &self.frags[i];
            // A story is a world of its own; a header is a different one on
            // each page.
            if g.repeat
                || g.path.story != frag.path.story
                || (matches!(frag.path.story, StoryId::Marginal(_)) && g.page != frag.page)
                || inside(g)
            {
                continue;
            }
            // Of a paragraph split over pages, the part that touches this one.
            let n = if up { g.lines.end - 1 } else { g.lines.start };
            return Some((i, n));
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use aster_writer::{
        Block, Document, Link, Margins, Paragraph, Path, Pos, Run, RunLook, Section, Story,
    };

    use crate::Layouter;

    fn look() -> RunLook {
        RunLook {
            size: 10.0,
            ..RunLook::default()
        }
    }

    fn linked(text: &str, target: &str) -> Run {
        Run {
            link: Some(Arc::new(Link {
                target: target.into(),
                tooltip: None,
            })),
            ..Run::text(text, look())
        }
    }

    fn document(runs: Vec<Run>) -> Document {
        let paragraph = Paragraph {
            runs,
            base: look(),
            mark: look(),
            ..Paragraph::default()
        };
        let section = Section {
            page: (6_000, 4_000),
            margins: Margins {
                top: 400,
                right: 400,
                bottom: 400,
                left: 400,
                header: 200,
                footer: 200,
                gutter: 0,
            },
            ..Section::default()
        };
        Document {
            body: Story {
                blocks: vec![Block::paragraph(paragraph)],
                part: None,
            },
            last_section: Arc::new(section),
            ..Document::default()
        }
    }

    #[test]
    fn linked_text_is_found_with_its_target() {
        let document = document(vec![
            Run::text("before ", look()),
            linked("one", "https://example.org/"),
            linked(" two", "https://example.org/"),
            Run::text(" between ", look()),
            linked("inside", "#chapter"),
        ]);
        let layout = Layouter::new().layout(&document);
        let links = layout.links(&document);
        // The two runs of the first link are one rectangle.
        assert_eq!(links.len(), 2, "{links:?}");
        assert_eq!(links[0].target, "https://example.org/");
        assert_eq!(links[1].target, "#chapter");
        let caret = |offset| layout.caret(&Pos::new(Path::body(0), offset)).unwrap();
        // "before " is 7 bytes, the first link is "one two".
        let (a, b) = (caret(7), caret(14));
        let first = &links[0];
        assert_eq!(first.page, 0);
        assert!((first.rect.x0 - a.x).abs() < 0.5, "{first:?} {a:?}");
        assert!((first.rect.x1 - b.x).abs() < 0.5, "{first:?} {b:?}");
        assert!((first.rect.y0 - a.top).abs() < 0.5);
        assert!(links[1].rect.x0 > first.rect.x1);
    }

    #[test]
    fn linked_text_that_wraps_has_a_rectangle_per_line() {
        let long = "word ".repeat(40);
        let document = document(vec![linked(&long, "https://example.org/")]);
        let layout = Layouter::new().layout(&document);
        let links = layout.links(&document);
        assert!(links.len() >= 3, "{links:?}");
        assert!(links.windows(2).all(|w| w[1].rect.y0 > w[0].rect.y0));
    }

    #[test]
    fn a_document_without_links_has_none() {
        let document = document(vec![Run::text("plain", look())]);
        let layout = Layouter::new().layout(&document);
        assert!(layout.links(&document).is_empty());
    }
}
