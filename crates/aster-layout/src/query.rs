//! The result of laying a document out, and the questions an editor asks of
//! it: which place in the text is under a point, where a place is, what a
//! selection covers, where the arrow keys go.
//!
//! Only the body's text is answered for (table cells included): headers,
//! footers, notes and text boxes are drawn but cannot be pointed at.

use std::collections::HashMap;

use aster_writer::{Path, Pos};

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

/// Where the caret goes: a bar `height` tall from `top`, at `x`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Caret {
    pub page: usize,
    pub x: f32,
    pub top: f32,
    pub height: f32,
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
        }
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
        for n in (2..pos.path.0.len()).rev() {
            if let Some(start) = self.redirects.get(&Path(pos.path.0[..n].to_vec())) {
                return std::borrow::Cow::Owned(Pos::new(start.clone(), 0));
            }
        }
        std::borrow::Cow::Borrowed(pos)
    }

    fn locate(&self, pos: &Pos) -> Option<(usize, usize)> {
        let pos = &*self.resolve(pos);
        let indexes = self.by_path.get(&pos.path)?;
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

    /// Where the caret is at `pos`; `None` for a path that is not in the
    /// body or has not been placed.
    pub fn caret(&self, pos: &Pos) -> Option<Caret> {
        let pos = &*self.resolve(pos);
        let (index, line) = self.locate(pos)?;
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
    pub fn hit(&self, page: usize, x: f32, y: f32) -> Option<Pos> {
        let mut best: Option<(f32, f32, usize)> = None;
        for (index, frag) in self.frags.iter().enumerate() {
            if frag.page != page {
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
        let (from, to) = if a <= b { (a, b) } else { (b, a) };
        let mut out = Vec::new();
        if from == to {
            return out;
        }
        for frag in &self.frags {
            if frag.repeat || frag.path < from.path || frag.path > to.path {
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

    /// The first place on the line `pos` is on.
    pub fn line_start(&self, pos: &Pos) -> Option<Pos> {
        let (index, line) = self.locate(pos)?;
        let frag = &self.frags[index];
        let off = frag.shaped.paragraph.line_start_offset(line);
        Some(Pos::new(frag.path.clone(), frag.shaped.to_orig(off)))
    }

    /// The last place on the line `pos` is on (before the space a wrapped
    /// line gave up).
    pub fn line_end(&self, pos: &Pos) -> Option<Pos> {
        let (index, line) = self.locate(pos)?;
        let frag = &self.frags[index];
        let off = frag.shaped.paragraph.line_end_offset(line);
        Some(Pos::new(frag.path.clone(), frag.shaped.to_orig(off)))
    }

    /// The place on the line above, as near `goal_x` (a page x) as it gets.
    pub fn above(&self, pos: &Pos, goal_x: f32) -> Option<Pos> {
        self.step(pos, goal_x, true)
    }

    /// The place on the line below.
    pub fn below(&self, pos: &Pos, goal_x: f32) -> Option<Pos> {
        self.step(pos, goal_x, false)
    }

    fn step(&self, pos: &Pos, goal_x: f32, up: bool) -> Option<Pos> {
        let (index, line) = self.locate(pos)?;
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
        let mut steps = frag.path.0.clone();
        while steps.len() >= 4 {
            steps.truncate(steps.len() - 3);
            let mut best: Option<(f32, usize, usize)> = None;
            for (i, g) in self.frags.iter().enumerate() {
                if i == index
                    || g.repeat
                    || g.page != frag.page
                    || !g.path.0.starts_with(&steps)
                    || g.path.0.len() == steps.len()
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
            let mut steps = frag.path.0.clone();
            while steps.len() >= 4 {
                steps.truncate(steps.len() - 3);
                if g.path.0.starts_with(&steps) {
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
            if g.repeat || inside(g) {
                continue;
            }
            // Of a paragraph split over pages, the part that touches this one.
            let n = if up { g.lines.end - 1 } else { g.lines.start };
            return Some((i, n));
        }
        None
    }
}
