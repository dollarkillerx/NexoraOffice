//! A text view for documents of any size.
//!
//! [`crate::TextSurface`] lays out its whole text as one Parley layout, which
//! is fine for a page and hopeless for 100 MB: every key press would lay out
//! the whole file again, and the layout alone would take gigabytes. This view
//! keeps the text in a rope and lays out only what is on screen, line by
//! line, the way the G2 gate cached paragraphs:
//!
//! - Every line has a height: measured once it has been laid out, estimated
//!   from its length until then. Heights are summed in groups of lines, so
//!   finding the line at a given height, or the height of a given line, costs
//!   a walk over the groups and one group, not over the file.
//! - The scroll position is held as a line and an offset into it. When a line
//!   above the view turns out taller than its estimate, the view does not
//!   jump.
//! - A line longer than [`SEGMENT`] bytes is laid out in segments of that
//!   size, so a file that is one 100 MB line still lays out a few kilobytes at
//!   a time. The price: a visual line may break early where a segment ends.
//! - Laid-out lines are cached and dropped again once they leave the view.
//!
//! Like `TextSurface`, nothing here knows about Slint or winit, and the host
//! keeps the document model: it repeats each edit with
//! [`TextView::replace_range`] and asks the view where the caret goes when it
//! moves.

use std::collections::HashMap;
use std::ops::Range;

use parley::layout::PositionedLayoutItem;
use parley::{
    Affinity, Cursor, FontContext, GenericFamily, Layout, LayoutContext, Selection, StyleProperty,
};
use ropey::Rope;
use vello_cpu::kurbo::{Affine, Rect, RoundedRect, Shape};
use vello_cpu::peniko::Fill;
use vello_cpu::{Glyph, Pixmap, RenderContext, Resources};

use crate::glyphs::{self, GlyphPaint};
use crate::text_surface::{
    Action, Brush, CARET_SCROLL_MARGIN, CARET_WIDTH, CODE_INSET, DEFAULT_LANGUAGE, Decoration,
    PageLayout, Palette, color, configure_han_fallback, draw_decorated_run, for_each_cluster,
    line_background_in, style_at,
};

/// Longest piece of a line laid out at once, in bytes.
pub const SEGMENT: usize = 8 * 1024;
/// Lines per group of summed heights.
const GROUP: usize = 256;
/// Laid-out segments kept beyond those on screen.
const CACHE_SLACK: usize = 64;
/// Line height as a multiple of the font size, as in `TextSurface`.
const LINE_HEIGHT: f32 = 1.6;

/// Heights of all lines, measured or estimated, summed by group.
struct Lines {
    /// Bytes in each line, without its `\n`.
    lens: Vec<u32>,
    /// Measured height of each line; NaN until laid out.
    heights: Vec<f32>,
    /// Sum of the heights of each group of [`GROUP`] lines.
    sums: Vec<f64>,
    /// Sum of all groups before each group.
    prefix: Vec<f64>,
    /// Groups whose sums are out of date from this one on: after lines were
    /// added or removed, every later group holds other lines.
    stale_from: usize,
    /// Single groups whose sums are out of date: a line in them changed
    /// height.
    dirty: Vec<usize>,
    /// Height of one visual line, physical pixels.
    line_height: f32,
    /// About how many bytes fill one visual line.
    bytes_per_line: f32,
}

/// Lengths of the lines of a rope, without their line breaks. One pass over
/// the text; for a 100 MB file this is the slow part of opening a view, so a
/// host may compute it on another thread and pass it to
/// [`TextView::with_line_lengths`].
pub fn line_lengths(text: &Rope) -> Vec<u32> {
    let mut lens = Vec::with_capacity(text.len_lines());
    let mut current: usize = 0;
    for chunk in text.chunks() {
        let bytes = chunk.as_bytes();
        let mut from = 0;
        while let Some(at) = bytes[from..].iter().position(|b| *b == b'\n') {
            current += at;
            lens.push(current as u32);
            current = 0;
            from += at + 1;
        }
        current += bytes.len() - from;
    }
    lens.push(current as u32);
    lens
}

impl Lines {
    fn new(lens: Vec<u32>) -> Self {
        let count = lens.len();
        let mut lines = Self {
            heights: vec![f32::NAN; count],
            lens,
            sums: Vec::new(),
            prefix: Vec::new(),
            stale_from: 0,
            dirty: Vec::new(),
            line_height: 1.0,
            bytes_per_line: 80.0,
        };
        lines.refresh();
        lines
    }

    fn count(&self) -> usize {
        self.lens.len()
    }

    fn estimate(&self, bytes: usize) -> f32 {
        let lines = (bytes as f32 / self.bytes_per_line).ceil().max(1.0);
        lines * self.line_height
    }

    /// The height of a line: measured, or estimated from its length. Long
    /// lines add up their segments.
    fn height(&self, line: usize, long: &HashMap<usize, Vec<f32>>) -> f32 {
        let measured = self.heights[line];
        if !measured.is_nan() {
            return measured;
        }
        if let Some(segments) = long.get(&line) {
            let len = self.lens[line] as usize;
            return segments
                .iter()
                .enumerate()
                .map(|(index, h)| {
                    if h.is_nan() {
                        let bytes = SEGMENT.min(len - (index * SEGMENT).min(len));
                        self.estimate(bytes)
                    } else {
                        *h
                    }
                })
                .sum();
        }
        self.estimate(self.lens[line] as usize)
    }

    /// A line changed height.
    fn mark(&mut self, line: usize) {
        self.dirty.push(line / GROUP);
    }

    /// Lines were added or removed at `line`.
    fn restructure(&mut self, line: usize) {
        self.stale_from = self.stale_from.min(line / GROUP);
    }

    fn group_sum(&self, group: usize, long: &HashMap<usize, Vec<f32>>) -> f64 {
        let lines = group * GROUP..((group + 1) * GROUP).min(self.count());
        lines.map(|line| f64::from(self.height(line, long))).sum()
    }

    /// Bring the group sums up to date.
    fn refresh_with(&mut self, long: &HashMap<usize, Vec<f32>>) {
        let groups = self.count().div_ceil(GROUP);
        if self.stale_from >= groups && self.sums.len() == groups && self.dirty.is_empty() {
            return;
        }
        let from = self.stale_from.min(self.sums.len()).min(groups);
        self.sums.truncate(from);
        for group in from..groups {
            let sum = self.group_sum(group, long);
            self.sums.push(sum);
        }
        for group in std::mem::take(&mut self.dirty) {
            if group < from {
                self.sums[group] = self.group_sum(group, long);
            }
        }
        self.prefix.clear();
        let mut total = 0.0;
        for sum in &self.sums {
            self.prefix.push(total);
            total += sum;
        }
        self.prefix.push(total);
        self.stale_from = usize::MAX;
    }

    fn refresh(&mut self) {
        self.refresh_with(&HashMap::new());
    }

    fn total(&self) -> f64 {
        self.prefix.last().copied().unwrap_or(0.0)
    }

    /// Height above the top of `line`. Sums must be fresh.
    fn y_of(&self, line: usize, long: &HashMap<usize, Vec<f32>>) -> f64 {
        let group = line / GROUP;
        let mut y = self
            .prefix
            .get(group)
            .copied()
            .unwrap_or_else(|| self.total());
        for earlier in group * GROUP..line {
            y += f64::from(self.height(earlier, long));
        }
        y
    }

    /// The line at height `y` and how far into it `y` is. Sums must be fresh.
    fn at(&self, y: f64, long: &HashMap<usize, Vec<f32>>) -> (usize, f64) {
        if y <= 0.0 {
            return (0, y);
        }
        let group = self
            .prefix
            .partition_point(|top| *top <= y)
            .saturating_sub(1);
        let group = group.min(self.sums.len().saturating_sub(1));
        let mut top = self.prefix[group];
        let end = ((group + 1) * GROUP).min(self.count());
        for line in group * GROUP..end {
            let height = f64::from(self.height(line, long));
            if y < top + height || line + 1 == self.count() {
                return (line, y - top);
            }
            top += height;
        }
        let last = self.count() - 1;
        (last, y - top)
    }
}

/// A piece of a line that is laid out on its own.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
struct Key {
    line: usize,
    segment: usize,
}

struct Cached {
    layout: Layout<Brush>,
    /// Frame in which it was last drawn or used.
    used: u64,
}

/// Composition text from the input method, shown at `at` but not part of the
/// text.
#[derive(Clone, Debug)]
struct Compose {
    at: usize,
    text: String,
    cursor: Option<(usize, usize)>,
}

/// Page and text placement in physical pixels.
#[derive(Clone, Copy, Debug)]
struct Columns {
    page_x: f64,
    page_width: f64,
    padding: f64,
    margin: f64,
}

pub struct TextView {
    text: Rope,
    lines: Lines,
    /// Segment heights of lines longer than [`SEGMENT`]; NaN until laid out.
    long: HashMap<usize, Vec<f32>>,
    cache: HashMap<Key, Cached>,
    frame: u64,
    font_cx: FontContext,
    layout_cx: LayoutContext<Brush>,
    resources: Resources,
    ctx: RenderContext,
    pixmap: Pixmap,
    width: u16,
    height: u16,
    scale: f32,
    font_size: f32,
    /// The line at the top of the view and how far down it the view starts,
    /// physical pixels. Negative above the first line, in the page padding.
    top: (usize, f64),
    anchor: usize,
    focus: usize,
    /// The caret sits at the end of a wrapped visual line rather than the
    /// start of the next one.
    upstream: bool,
    /// Where vertical movement aims, physical pixels from the text's left.
    preferred_x: Option<f32>,
    compose: Option<Compose>,
    caret_on: bool,
    palette: Palette,
    page: PageLayout,
    decorations: Vec<Decoration>,
    glyph_buffer: Vec<Glyph>,
}

impl TextView {
    /// A view of `text` with the caret at the start.
    pub fn new(text: Rope) -> Self {
        let lens = line_lengths(&text);
        Self::with_line_lengths(text, lens)
    }

    /// A view of `text` whose line lengths were computed with
    /// [`line_lengths`], perhaps on another thread.
    pub fn with_line_lengths(text: Rope, lens: Vec<u32>) -> Self {
        let mut font_cx = FontContext::default();
        configure_han_fallback(&mut font_cx);
        let mut view = Self {
            text,
            lines: Lines::new(lens),
            long: HashMap::new(),
            cache: HashMap::new(),
            frame: 0,
            font_cx,
            layout_cx: LayoutContext::default(),
            resources: Resources::new(),
            ctx: RenderContext::new(1, 1),
            pixmap: Pixmap::new(1, 1),
            width: 0,
            height: 0,
            scale: 1.0,
            font_size: 18.0,
            top: (0, 0.0),
            anchor: 0,
            focus: 0,
            upstream: false,
            preferred_x: None,
            compose: None,
            caret_on: true,
            palette: Palette::default(),
            page: PageLayout::default(),
            decorations: Vec::new(),
            glyph_buffer: Vec::new(),
        };
        view.remeasure();
        view
    }

    // --- Geometry -----------------------------------------------------------

    fn columns(&self) -> Columns {
        let s = f64::from(self.scale);
        let width = f64::from(self.width);
        let margin = self.page.margin * s;
        let available = (width - 2.0 * margin).max(1.0);
        let page_width = match self.page.max_width {
            Some(max) => (max * s).min(available),
            None => available,
        };
        let page_x = ((width - page_width) / 2.0).floor();
        let padding = (self.page.padding * s).min(page_width / 8.0);
        Columns {
            page_x,
            page_width,
            padding,
            margin,
        }
    }

    fn text_width(&self) -> f32 {
        let c = self.columns();
        (c.page_width - 2.0 * c.padding).max(1.0) as f32
    }

    /// Space above the first line: margin and padding.
    fn top_pad(&self) -> f64 {
        let c = self.columns();
        c.margin + c.padding
    }

    /// Drop every measurement and layout: after a change of size, scale or
    /// font. The scroll position stays on the same line.
    fn remeasure(&mut self) {
        self.cache.clear();
        self.long.clear();
        self.lines.heights.fill(f32::NAN);
        self.lines.line_height = self.font_size * LINE_HEIGHT * self.scale;
        // A CJK character is an em wide and three bytes; a Latin letter about
        // half an em and one byte. Two bytes per half-em is a fair middle.
        let em = (self.font_size * self.scale).max(1.0);
        self.lines.bytes_per_line = (self.text_width() / em * 2.5).max(8.0);
        self.lines.stale_from = 0;
        self.lines.refresh_with(&self.long);
    }

    fn refresh(&mut self) {
        self.lines.refresh_with(&self.long);
    }

    /// Page-coordinate scroll offset: the top of the view, physical pixels
    /// from the top of the page.
    fn scroll(&mut self) -> f64 {
        self.refresh();
        let (line, offset) = self.top;
        self.top_pad() + self.lines.y_of(line, &self.long) + offset
    }

    fn max_scroll(&mut self) -> f64 {
        self.refresh();
        let c = self.columns();
        let page = self.lines.total() + 2.0 * c.padding;
        let page = match self.page.min_aspect {
            Some(aspect) => page.max(c.page_width * aspect),
            None => page.max(f64::from(self.height) - 2.0 * c.margin),
        };
        (page + 2.0 * c.margin - f64::from(self.height)).max(0.0)
    }

    fn set_scroll(&mut self, scroll: f64) {
        let scroll = scroll.clamp(0.0, self.max_scroll());
        let y = scroll - self.top_pad();
        self.refresh();
        self.top = if y < 0.0 {
            (0, y)
        } else {
            self.lines.at(y, &self.long)
        };
    }

    // --- Segments and layout ---------------------------------------------

    fn line_start(&self, line: usize) -> usize {
        self.text.line_to_byte(line)
    }

    fn segment_count(&self, line: usize) -> usize {
        (self.lines.lens[line] as usize).div_ceil(SEGMENT).max(1)
    }

    /// Byte range of a segment, relative to the start of its line.
    fn segment_range(&self, line: usize, segment: usize) -> Range<usize> {
        let len = self.lines.lens[line] as usize;
        if len <= SEGMENT {
            return 0..len;
        }
        let start_of_line = self.line_start(line);
        let boundary = |index: usize| {
            if index == 0 {
                0
            } else if index * SEGMENT >= len {
                len
            } else {
                let at = start_of_line + index * SEGMENT;
                self.text.char_to_byte(self.text.byte_to_char(at)) - start_of_line
            }
        };
        boundary(segment)..boundary(segment + 1)
    }

    /// The segment of `line` holding the byte `offset` into the line. An
    /// offset on a boundary belongs to the segment it starts, except at the
    /// end of the line.
    fn segment_of(&self, line: usize, offset: usize) -> usize {
        let count = self.segment_count(line);
        if count == 1 {
            return 0;
        }
        let mut segment = (offset / SEGMENT).min(count - 1);
        while segment > 0 && self.segment_range(line, segment).start > offset {
            segment -= 1;
        }
        while segment + 1 < count && self.segment_range(line, segment + 1).start <= offset {
            segment += 1;
        }
        segment
    }

    /// Where a committed-text offset is: line, segment, offset in segment.
    fn locate(&self, offset: usize) -> (Key, usize) {
        let offset = offset.min(self.text.len_bytes());
        let line = self.text.byte_to_line(offset);
        let in_line = offset - self.line_start(line);
        let segment = self.segment_of(line, in_line);
        let local = in_line - self.segment_range(line, segment).start;
        (Key { line, segment }, local)
    }

    fn absolute(&self, key: Key, local: usize) -> usize {
        self.line_start(key.line) + self.segment_range(key.line, key.segment).start + local
    }

    /// The composition, if it is in this segment: its start and length in the
    /// laid-out text.
    fn compose_in(&self, key: Key) -> Option<(usize, &Compose)> {
        let compose = self.compose.as_ref()?;
        let (at_key, local) = self.locate(compose.at);
        (at_key == key).then_some((local, compose))
    }

    /// Map a committed offset in this segment to the laid-out text.
    fn raw(&self, key: Key, local: usize) -> usize {
        match self.compose_in(key) {
            Some((at, compose)) if local >= at => local + compose.text.len(),
            _ => local,
        }
    }

    /// Lay out a segment, or take it from the cache, and record its height.
    fn layout(&mut self, key: Key) -> &Layout<Brush> {
        self.frame_use(key);
        if !self.cache.contains_key(&key) {
            let range = self.segment_range(key.line, key.segment);
            let start = self.line_start(key.line);
            let mut text = self
                .text
                .byte_slice(start + range.start..start + range.end)
                .to_string();
            let mut preedit = None;
            if let Some((at, compose)) = self.compose_in(key) {
                text.insert_str(at, &compose.text);
                preedit = Some(at..at + compose.text.len());
            }
            let width = self.text_width();
            let mut builder =
                self.layout_cx
                    .ranged_builder(&mut self.font_cx, &text, self.scale, true);
            builder.push_default(StyleProperty::from(GenericFamily::SystemUi));
            builder.push_default(StyleProperty::FontSize(self.font_size));
            builder.push_default(StyleProperty::LineHeight(
                parley::LineHeight::FontSizeRelative(LINE_HEIGHT),
            ));
            builder.push_default(StyleProperty::Brush(Brush(color(self.palette.ink))));
            builder.push_default(StyleProperty::Locale(DEFAULT_LANGUAGE.parse().ok()));
            if let Some(range) = preedit {
                builder.push(StyleProperty::Underline(true), range);
            }
            let mut layout = builder.build(&text);
            layout.break_all_lines(Some(width));
            let height = layout.height().max(self.lines.line_height);
            self.record_height(key, height);
            self.cache.insert(
                key,
                Cached {
                    layout,
                    used: self.frame,
                },
            );
        }
        &self.cache[&key].layout
    }

    fn frame_use(&mut self, key: Key) {
        if let Some(cached) = self.cache.get_mut(&key) {
            cached.used = self.frame;
        }
    }

    fn record_height(&mut self, key: Key, height: f32) {
        let count = self.segment_count(key.line);
        if count == 1 {
            if self.lines.heights[key.line] != height {
                self.lines.heights[key.line] = height;
                self.lines.mark(key.line);
            }
        } else {
            let segments = self
                .long
                .entry(key.line)
                .or_insert_with(|| vec![f32::NAN; count]);
            segments.resize(count, f32::NAN);
            if segments[key.segment] != height {
                segments[key.segment] = height;
                self.lines.mark(key.line);
            }
        }
    }

    /// Height of a segment: measured, or estimated.
    fn segment_height(&self, key: Key) -> f32 {
        if self.segment_count(key.line) == 1 {
            return self.lines.height(key.line, &self.long);
        }
        if let Some(h) = self.long.get(&key.line).and_then(|s| s.get(key.segment))
            && !h.is_nan()
        {
            return *h;
        }
        self.segment_estimate(key)
    }

    /// Estimated height of a segment of a long line. From the nominal
    /// segment size, not its exact bounds: those cost a rope lookup each,
    /// and a 100 MB line has thousands of segments.
    fn segment_estimate(&self, key: Key) -> f32 {
        let len = self.lines.lens[key.line] as usize;
        let start = (key.segment * SEGMENT).min(len);
        self.lines.estimate(SEGMENT.min(len - start))
    }

    /// Height above the top of a segment, in text coordinates.
    fn y_of(&mut self, key: Key) -> f64 {
        self.refresh();
        let mut y = self.lines.y_of(key.line, &self.long);
        if key.segment == 0 {
            return y;
        }
        let measured = self.long.get(&key.line);
        for segment in 0..key.segment {
            let h = measured
                .and_then(|heights| heights.get(segment))
                .copied()
                .filter(|h| !h.is_nan())
                .unwrap_or_else(|| {
                    self.segment_estimate(Key {
                        line: key.line,
                        segment,
                    })
                });
            y += f64::from(h);
        }
        y
    }

    /// The segment at height `y` in text coordinates, and `y` within it.
    fn segment_at(&mut self, y: f64) -> (Key, f64) {
        self.refresh();
        let (line, mut offset) = self.lines.at(y, &self.long);
        let count = self.segment_count(line);
        let mut segment = 0;
        while segment + 1 < count {
            let h = f64::from(self.segment_height(Key { line, segment }));
            if offset < h {
                break;
            }
            offset -= h;
            segment += 1;
        }
        (Key { line, segment }, offset)
    }

    fn next_key(&self, key: Key) -> Option<Key> {
        if key.segment + 1 < self.segment_count(key.line) {
            Some(Key {
                line: key.line,
                segment: key.segment + 1,
            })
        } else if key.line + 1 < self.lines.count() {
            Some(Key {
                line: key.line + 1,
                segment: 0,
            })
        } else {
            None
        }
    }

    fn previous_key(&self, key: Key) -> Option<Key> {
        if key.segment > 0 {
            Some(Key {
                line: key.line,
                segment: key.segment - 1,
            })
        } else if key.line > 0 {
            let line = key.line - 1;
            Some(Key {
                line,
                segment: self.segment_count(line) - 1,
            })
        } else {
            None
        }
    }

    // --- The host's controls ---------------------------------------------

    /// Resize the backing buffer. Sizes are physical pixels.
    pub fn resize(&mut self, width: u16, height: u16, scale: f32) {
        if (width, height, scale) == (self.width, self.height, self.scale) {
            return;
        }
        let relayout = width != self.width || scale != self.scale;
        self.width = width;
        self.height = height;
        self.scale = scale;
        self.ctx = RenderContext::new(width.max(1), height.max(1));
        self.pixmap = Pixmap::new(width.max(1), height.max(1));
        if relayout {
            let offset = self.top.1 / f64::from(self.lines.line_height.max(1.0));
            self.remeasure();
            self.top.1 = offset * f64::from(self.lines.line_height);
        }
    }

    pub fn size(&self) -> (u16, u16, f32) {
        (self.width, self.height, self.scale)
    }

    pub fn set_palette(&mut self, palette: Palette) {
        self.palette = palette;
    }

    pub fn set_page_layout(&mut self, page: PageLayout) {
        if page != self.page {
            self.page = page;
            self.remeasure();
        }
    }

    /// Body text size in logical pixels.
    pub fn set_font_size(&mut self, size: f32) {
        if size != self.font_size {
            self.font_size = size;
            self.remeasure();
        }
    }

    pub fn set_caret_on(&mut self, on: bool) {
        self.caret_on = on;
    }

    pub fn set_decorations(&mut self, decorations: Vec<Decoration>) {
        self.decorations = decorations;
    }

    /// The whole committed text. Copies it: for tests and small documents.
    pub fn text(&self) -> String {
        self.text.to_string()
    }

    pub fn len(&self) -> usize {
        self.text.len_bytes()
    }

    pub fn is_empty(&self) -> bool {
        self.text.len_bytes() == 0
    }

    pub fn selection(&self) -> (usize, usize) {
        (self.anchor, self.focus)
    }

    fn floor(&self, at: usize) -> usize {
        let at = at.min(self.text.len_bytes());
        self.text.char_to_byte(self.text.byte_to_char(at))
    }

    pub fn set_selection(&mut self, anchor: usize, focus: usize) {
        if self.compose.is_some() {
            return;
        }
        self.anchor = self.floor(anchor);
        self.focus = self.floor(focus);
        self.upstream = false;
        self.preferred_x = None;
        self.caret_on = true;
    }

    /// Repeat an edit the host's document has made: the committed bytes in
    /// `range` become `inserted`. Ends any composition. The caret goes after
    /// the new text.
    pub fn replace_range(&mut self, range: Range<usize>, inserted: &str) {
        let mut text = self.text.clone();
        let start = text.byte_to_char(range.start);
        let end = text.byte_to_char(range.end);
        if end > start {
            text.remove(start..end);
        }
        if !inserted.is_empty() {
            text.insert(start, inserted);
        }
        self.apply_change(range, inserted.len(), text);
    }

    /// Take an edit the host has already made to its own copy of the text:
    /// `text` is the whole text after the committed bytes in `range` became
    /// `inserted` bytes. The view then shares the host's rope instead of
    /// keeping a second copy. Ends any composition; the caret goes after
    /// the new text.
    pub fn apply_change(&mut self, range: Range<usize>, inserted: usize, text: Rope) {
        self.end_composition();
        let first = self.text.byte_to_line(range.start);
        let last = self.text.byte_to_line(range.end);
        let start_in_line = range.start - self.line_start(first);
        let first_segment = self.segment_of(first, start_in_line);
        self.text = text;

        // Lines `first..=last` became `first..=new_last`.
        let new_last = self.text.byte_to_line(range.start + inserted);
        let new_lens = self.lengths(first..=new_last);
        let added = new_last - first + 1;
        let removed = last - first + 1;
        let same_lines = added == removed;
        self.lines.lens.splice(first..=last, new_lens);
        self.lines
            .heights
            .splice(first..=last, std::iter::repeat_n(f32::NAN, added));
        if same_lines {
            for line in first..=new_last {
                self.lines.mark(line);
            }
        } else {
            // Every group after this one has other lines in it now.
            self.lines.restructure(first);
        }

        // Layouts and segment heights: lines after the edit move, the edited
        // ones are laid out again. Within one long line only the segments
        // from the edit on change.
        let shift = |line: usize| line + added - removed;
        let one_line = first == last && added == 1;
        let old_cache = std::mem::take(&mut self.cache);
        for (key, cached) in old_cache {
            if key.line < first || (one_line && key.line == first && key.segment < first_segment) {
                self.cache.insert(key, cached);
            } else if key.line > last {
                self.cache.insert(
                    Key {
                        line: shift(key.line),
                        segment: key.segment,
                    },
                    cached,
                );
            }
        }
        let old_long = std::mem::take(&mut self.long);
        for (line, mut segments) in old_long {
            if line < first {
                self.long.insert(line, segments);
            } else if line > last {
                self.long.insert(shift(line), segments);
            } else if one_line && line == first {
                segments.truncate(first_segment);
                if self.segment_count(first) > 1 {
                    segments.resize(self.segment_count(first), f32::NAN);
                    self.long.insert(line, segments);
                }
            }
        }

        // The view stays on the same text.
        if self.top.0 > last {
            self.top.0 = shift(self.top.0);
        } else if self.top.0 > new_last {
            self.top = (new_last, 0.0);
        }
        let caret = self.floor(range.start + inserted);
        self.anchor = caret;
        self.focus = caret;
        self.upstream = false;
        self.preferred_x = None;
        self.caret_on = true;
    }

    /// Lengths of a run of lines, in one pass over their text.
    fn lengths(&self, lines: std::ops::RangeInclusive<usize>) -> Vec<u32> {
        let (first, last) = (*lines.start(), *lines.end());
        let start = self.text.line_to_byte(first);
        let end = if last + 1 < self.text.len_lines() {
            self.text.line_to_byte(last + 1) - 1
        } else {
            self.text.len_bytes()
        };
        let mut lens = Vec::with_capacity(last - first + 1);
        let mut current = 0usize;
        for chunk in self.text.byte_slice(start..end).chunks() {
            let bytes = chunk.as_bytes();
            let mut from = 0;
            while let Some(at) = bytes[from..].iter().position(|b| *b == b'\n') {
                lens.push((current + at) as u32);
                current = 0;
                from += at + 1;
            }
            current += bytes.len() - from;
        }
        lens.push(current as u32);
        lens
    }

    pub fn is_composing(&self) -> bool {
        self.compose.is_some()
    }

    /// Show composition text at the caret; empty text ends the composition.
    pub fn compose(&mut self, text: &str, cursor: Option<(usize, usize)>) {
        if text.is_empty() {
            self.end_composition();
            return;
        }
        let at = self.compose.as_ref().map_or(self.focus, |c| c.at);
        let (key, _) = self.locate(at);
        self.compose = Some(Compose {
            at,
            text: text.to_owned(),
            cursor,
        });
        self.cache.remove(&key);
        self.caret_on = true;
        self.reveal_caret();
    }

    pub fn end_composition(&mut self) {
        if let Some(compose) = self.compose.take() {
            let (key, _) = self.locate(compose.at);
            self.cache.remove(&key);
        }
        self.caret_on = true;
    }

    // --- Scrolling --------------------------------------------------------

    /// Scroll by physical pixels; positive moves down. Returns whether the
    /// view moved.
    pub fn scroll_by(&mut self, delta: f64) -> bool {
        let before = self.scroll();
        self.set_scroll(before + delta);
        (self.scroll() - before).abs() > 0.01
    }

    /// Scroll offset and its upper bound, physical pixels. Heights not laid
    /// out yet are estimates, so these move a little as the view scrolls.
    pub fn scroll_range(&mut self) -> (f64, f64) {
        let max = self.max_scroll();
        (self.scroll().min(max), max)
    }

    /// The committed text on the lines inside the view.
    pub fn visible_range(&mut self) -> Range<usize> {
        self.refresh();
        let (first, offset) = self.top;
        let mut y = -offset;
        let mut line = first;
        while line + 1 < self.lines.count() {
            y += f64::from(self.lines.height(line, &self.long));
            if y >= f64::from(self.height) {
                break;
            }
            line += 1;
        }
        let start = self.line_start(first);
        let end = self.line_start(line) + self.lines.lens[line] as usize;
        start..end
    }

    /// The part of the text at the top edge of the view: a byte offset with a
    /// fraction for how far down its line the edge is.
    pub fn top_position(&mut self) -> f64 {
        self.refresh();
        let (line, offset) = self.top;
        let height = f64::from(self.lines.height(line, &self.long)).max(1.0);
        let fraction = (offset / height).clamp(0.0, 1.0);
        self.line_start(line) as f64 + fraction * f64::from(self.lines.lens[line])
    }

    /// Scroll so that a position from [`Self::top_position`] is at the top.
    pub fn scroll_to_position(&mut self, position: f64) {
        self.refresh();
        let at = (position.max(0.0) as usize).min(self.text.len_bytes());
        let line = self.text.byte_to_line(at);
        let len = f64::from(self.lines.lens[line]);
        let fraction = if len > 0.0 {
            ((position - self.line_start(line) as f64) / len).clamp(0.0, 1.0)
        } else {
            0.0
        };
        let height = f64::from(self.lines.height(line, &self.long));
        self.top = (line, fraction * height);
        let scroll = self.scroll();
        self.set_scroll(scroll);
    }

    /// Caret rectangle in text coordinates: `(x0, y0, x1, y1)`.
    fn caret_rect(&mut self) -> (f64, f64, f64, f64) {
        let (key, local, caret) = match &self.compose {
            Some(compose) => {
                let (key, local) = self.locate(compose.at);
                let offset = compose.cursor.map_or(compose.text.len(), |c| c.0);
                (key, local, local + offset)
            }
            None => {
                let (key, local) = self.locate(self.focus);
                let raw = self.raw(key, local);
                (key, local, raw)
            }
        };
        let _ = local;
        let y = self.y_of(key);
        let affinity = if self.upstream {
            Affinity::Upstream
        } else {
            Affinity::Downstream
        };
        let width = CARET_WIDTH * self.scale;
        let layout = self.layout(key);
        let rect = Cursor::from_byte_index(layout, caret, affinity).geometry(layout, width);
        (rect.x0, y + rect.y0, rect.x1, y + rect.y1)
    }

    /// Scroll just far enough to bring the caret into view.
    pub fn reveal_caret(&mut self) {
        if self.height == 0 {
            return;
        }
        let (_, top, _, bottom) = self.caret_rect();
        let pad = self.top_pad();
        let scroll = self.scroll();
        let height = f64::from(self.height);
        let keep = (CARET_SCROLL_MARGIN * f64::from(self.scale)).min(height / 4.0);
        let (top, bottom) = (top + pad - scroll, bottom + pad - scroll);
        if top < keep {
            self.set_scroll(scroll - (keep - top));
        } else if bottom > height - keep {
            self.set_scroll(scroll + bottom - (height - keep));
        }
    }

    /// Where the candidate window goes: `(x, y, width, height)` in physical
    /// pixels relative to the view.
    pub fn ime_cursor_area(&mut self) -> (f64, f64, f64, f64) {
        let c = self.columns();
        let scroll = self.scroll();
        let pad = self.top_pad();
        let (x0, y0, x1, y1) = match self.compose.clone() {
            Some(compose) => {
                let (key, local) = self.locate(compose.at);
                let y = self.y_of(key);
                let layout = self.layout(key);
                let mut area: Option<(f64, f64, f64, f64)> = None;
                let selection = Selection::new(
                    Cursor::from_byte_index(layout, local, Affinity::Downstream),
                    Cursor::from_byte_index(layout, local + compose.text.len(), Affinity::Upstream),
                );
                selection.geometry_with(layout, |rect, _| {
                    area = Some(match area {
                        None => (rect.x0, rect.y0, rect.x1, rect.y1),
                        Some(a) => (
                            a.0.min(rect.x0),
                            a.1.min(rect.y0),
                            a.2.max(rect.x1),
                            a.3.max(rect.y1),
                        ),
                    });
                });
                let a = area.unwrap_or((0.0, 0.0, 1.0, f64::from(self.lines.line_height)));
                (a.0, y + a.1, a.2, y + a.3)
            }
            None => self.caret_rect(),
        };
        (
            x0 + c.page_x + c.padding,
            y0 + pad - scroll,
            (x1 - x0).max(1.0),
            (y1 - y0).max(1.0),
        )
    }

    // --- Movement -----------------------------------------------------------

    /// Carry out a caret movement. Text-changing actions are ignored: the
    /// host's document makes those.
    pub fn apply(&mut self, action: Action) {
        if self.compose.is_some() {
            return;
        }
        let (select, target) = match action {
            Action::Left { select, word } => (select, self.horizontal(false, word, select)),
            Action::Right { select, word } => (select, self.horizontal(true, word, select)),
            Action::Up { select } => (select, self.vertical(false)),
            Action::Down { select } => (select, self.vertical(true)),
            Action::LineStart { select } => (select, self.line_edge(false)),
            Action::LineEnd { select } => (select, self.line_edge(true)),
            Action::TextStart { select } => (select, (0, false, false)),
            Action::TextEnd { select } => (select, (self.text.len_bytes(), false, false)),
            Action::SelectAll => {
                self.anchor = 0;
                self.focus = self.text.len_bytes();
                self.upstream = false;
                self.caret_on = true;
                return;
            }
            Action::Insert(_)
            | Action::Newline
            | Action::PlainNewline
            | Action::Indent
            | Action::Outdent
            | Action::Backspace
            | Action::Delete => return,
        };
        let (focus, upstream, keep_x) = target;
        self.focus = focus;
        if !select {
            self.anchor = focus;
        }
        self.upstream = upstream;
        if !keep_x {
            self.preferred_x = None;
        }
        self.caret_on = true;
        self.reveal_caret();
    }

    /// Left or right by a character or a word. Returns the new focus.
    fn horizontal(&mut self, forward: bool, word: bool, select: bool) -> (usize, bool, bool) {
        // Without Shift, a selection collapses to its edge.
        if !select && self.anchor != self.focus && !word {
            let edge = if forward {
                self.anchor.max(self.focus)
            } else {
                self.anchor.min(self.focus)
            };
            return (edge, false, false);
        }
        let len = self.text.len_bytes();
        let (key, local) = self.locate(self.focus);
        let range = self.segment_range(key.line, key.segment);
        let layout = self.layout(key);
        let cursor = Cursor::from_byte_index(layout, local, Affinity::Downstream);
        let step = |cursor: Cursor| match (forward, word) {
            (true, false) => cursor.next_visual(layout),
            (false, false) => cursor.previous_visual(layout),
            (true, true) => cursor.next_visual_word(layout),
            (false, true) => cursor.previous_visual_word(layout),
        };
        let mut moved = step(cursor);
        // At a soft wrap the first step only changes which line the caret
        // is drawn on.
        if moved.index() == local && local > 0 && local < range.len() {
            moved = step(moved);
        }
        let index = moved.index().min(range.len());
        if forward && index > local {
            return (self.absolute(key, index), false, false);
        }
        if !forward && index < local {
            return (self.absolute(key, index), false, false);
        }
        // At the edge of the segment: step over into the next or previous
        // one; at the end of a line that steps over the line break.
        let at = self.focus;
        if forward {
            if at >= len {
                return (len, false, false);
            }
            // Past the end of the segment: the line break, or the first
            // character of the next segment.
            (self.floor_up(at + 1), false, false)
        } else {
            if at == 0 {
                return (0, false, false);
            }
            (self.floor(at - 1), false, false)
        }
    }

    /// The next character boundary at or after `at`.
    fn floor_up(&self, at: usize) -> usize {
        let at = at.min(self.text.len_bytes());
        let floor = self.floor(at);
        if floor == at {
            at
        } else {
            self.text.char_to_byte(self.text.byte_to_char(at) + 1)
        }
    }

    /// Visual line index inside a layout that holds a raw offset.
    fn visual_line(layout: &Layout<Brush>, raw: usize, upstream: bool) -> usize {
        let mut found = 0;
        for (index, line) in layout.lines().enumerate() {
            let range = line.text_range();
            if raw >= range.start && (raw < range.end || (upstream && raw == range.end)) {
                return index;
            }
            if raw >= range.start {
                found = index;
            }
        }
        found
    }

    /// Up or down by a visual line, keeping the horizontal position.
    fn vertical(&mut self, down: bool) -> (usize, bool, bool) {
        let (key, local) = self.locate(self.focus);
        let upstream = self.upstream;
        let width = CARET_WIDTH * self.scale;
        let preferred = self.preferred_x;
        let layout = self.layout(key);
        let affinity = if upstream {
            Affinity::Upstream
        } else {
            Affinity::Downstream
        };
        let caret = Cursor::from_byte_index(layout, local, affinity).geometry(layout, width);
        let x = preferred.unwrap_or(caret.x0 as f32);
        let line_index = Self::visual_line(layout, local, upstream);
        let lines = layout.len();
        self.preferred_x = Some(x);
        let (target, target_line) = if down && line_index + 1 < lines {
            (key, Some(line_index + 1))
        } else if !down && line_index > 0 {
            (key, Some(line_index - 1))
        } else if down {
            match self.next_key(key) {
                Some(next) => (next, Some(0)),
                None => return (self.text.len_bytes(), false, true),
            }
        } else {
            match self.previous_key(key) {
                Some(previous) => (previous, None),
                None => return (0, false, true),
            }
        };
        let layout = self.layout(target);
        let index = target_line.unwrap_or(layout.len().saturating_sub(1));
        let y = layout.get(index).map_or(0.0, |line| {
            let m = line.metrics();
            (m.block_min_coord + m.block_max_coord) / 2.0
        });
        let cursor = Cursor::from_point(layout, x, y);
        let raw = cursor.index();
        let range = self.segment_range(target.line, target.segment);
        let local = self.committed_local(target, raw).min(range.len());
        let upstream = cursor.affinity() == Affinity::Upstream;
        (self.absolute(target, local), upstream, true)
    }

    /// A raw offset of a segment's layout as a committed offset in it.
    fn committed_local(&self, key: Key, raw: usize) -> usize {
        match self.compose_in(key) {
            Some((at, compose)) if raw >= at + compose.text.len() => raw - compose.text.len(),
            Some((at, _)) if raw > at => at,
            _ => raw,
        }
    }

    /// Start or end of the visual line the caret is on.
    fn line_edge(&mut self, end: bool) -> (usize, bool, bool) {
        let (key, local) = self.locate(self.focus);
        let upstream = self.upstream;
        let range = self.segment_range(key.line, key.segment);
        let layout = self.layout(key);
        let index = Self::visual_line(layout, local, upstream);
        let Some(line) = layout.get(index) else {
            return (self.focus, false, false);
        };
        let text_range = line.text_range();
        if !end {
            return (self.absolute(key, text_range.start), false, false);
        }
        // A wrapped line ends before the next one starts: put the caret at
        // the end of this one. The last line ends at the end of the segment.
        let last = index + 1 == layout.len();
        if last {
            (self.absolute(key, range.len()), false, false)
        } else {
            let mut at = text_range.end;
            // Trailing white space hangs past the edge; stop before it.
            let text = self.text.byte_slice(
                self.absolute(key, text_range.start)..self.absolute(key, text_range.end),
            );
            let trailing = text.to_string();
            let trimmed = trailing.trim_end().len();
            if trimmed < trailing.len() {
                at = text_range.start + trimmed;
                return (self.absolute(key, at), false, false);
            }
            (self.absolute(key, at), true, false)
        }
    }

    // --- Pointer ------------------------------------------------------------

    /// The committed offset at a point in physical pixels relative to the view.
    fn hit(&mut self, x: f64, y: f64) -> (usize, bool) {
        let c = self.columns();
        let scroll = self.scroll();
        let text_y = y + scroll - self.top_pad();
        let text_x = (x - c.page_x - c.padding) as f32;
        if text_y < 0.0 {
            return (0, false);
        }
        let (key, within) = self.segment_at(text_y);
        let layout = self.layout(key);
        let cursor = Cursor::from_point(layout, text_x, within as f32);
        let upstream = cursor.affinity() == Affinity::Upstream;
        let raw = cursor.index();
        let range = self.segment_range(key.line, key.segment);
        let local = self.committed_local(key, raw).min(range.len());
        (self.absolute(key, local), upstream)
    }

    pub fn pointer_down(&mut self, x: f64, y: f64, extend: bool) {
        if self.compose.is_some() {
            return;
        }
        let (at, upstream) = self.hit(x, y);
        self.focus = at;
        if !extend {
            self.anchor = at;
        }
        self.upstream = upstream;
        self.preferred_x = None;
        self.caret_on = true;
    }

    pub fn pointer_drag(&mut self, x: f64, y: f64) {
        if self.compose.is_some() {
            return;
        }
        let (at, upstream) = self.hit(x, y);
        self.focus = at;
        self.upstream = upstream;
        self.caret_on = true;
    }

    // --- Drawing ------------------------------------------------------------

    /// Draw what is in view and return the pixels, premultiplied RGBA8.
    pub fn render(&mut self) -> &Pixmap {
        self.frame += 1;
        let c = self.columns();
        let (w, h) = (f64::from(self.width), f64::from(self.height));

        // Lay out the segments in view first: that measures them, and the
        // positions depend on the measurements.
        let mut keys: Vec<Key> = Vec::new();
        {
            let scroll = self.scroll();
            let top_y = self.top_pad() - scroll;
            let mut key = Key {
                line: self.top.0,
                segment: 0,
            };
            let mut y = top_y + self.y_of(key);
            loop {
                let estimate = f64::from(self.segment_height(key));
                if y + estimate >= 0.0 {
                    self.layout(key);
                    keys.push(key);
                }
                y += f64::from(self.segment_height(key));
                if y > h {
                    break;
                }
                match self.next_key(key) {
                    Some(next) => key = next,
                    None => break,
                }
            }
        }
        let scroll = self.scroll();
        let max = self.max_scroll();
        let top_y = self.top_pad() - scroll;
        let pieces: Vec<Piece> = {
            let mut y = keys.first().map_or(0.0, |key| top_y + self.y_of(*key));
            let mut pieces = Vec::with_capacity(keys.len());
            for key in &keys {
                let range = self.segment_range(key.line, key.segment);
                let base = self.line_start(key.line) + range.start;
                let compose = self
                    .compose_in(*key)
                    .map(|(at, compose)| (at, compose.text.len(), compose.cursor));
                let (focus_key, focus_local) = self.locate(self.focus);
                pieces.push(Piece {
                    key: *key,
                    y,
                    base,
                    len: range.len(),
                    compose,
                    caret: (focus_key == *key).then_some(focus_local),
                });
                y += f64::from(self.segment_height(*key));
            }
            pieces
        };
        // Drop layouts that have not been used for a while.
        if self.cache.len() > pieces.len() + CACHE_SLACK {
            let frame = self.frame;
            self.drop_layouts_older(frame);
        }

        let Self {
            ctx,
            pixmap,
            resources,
            palette,
            cache,
            decorations,
            glyph_buffer,
            scale,
            caret_on,
            anchor,
            focus,
            upstream,
            ..
        } = self;
        let scale = *scale;
        ctx.reset();
        ctx.set_fill_rule(Fill::NonZero);
        ctx.set_transform(Affine::IDENTITY);
        ctx.set_paint(color(palette.desk));
        ctx.fill_rect(&Rect::new(0.0, 0.0, w, h));
        let page_height = max + h - 2.0 * c.margin;
        let page_rect = Rect::new(
            c.page_x,
            c.margin - scroll,
            c.page_x + c.page_width,
            c.margin - scroll + page_height,
        );
        if palette.page_edge[3] != 0 {
            let edge = f64::from(scale).round().max(1.0);
            ctx.set_paint(color(palette.page_edge));
            ctx.fill_rect(&page_rect.inflate(edge, edge));
        }
        ctx.set_paint(color(palette.page));
        ctx.fill_rect(&page_rect);

        let selection = (*anchor).min(*focus)..(*anchor).max(*focus);
        let text_x = c.page_x + c.padding;
        let text_width = c.page_width - 2.0 * c.padding;
        let inset = (f64::from(CODE_INSET) * f64::from(scale)).min(c.padding);
        let ink = color(palette.ink);

        for piece in &pieces {
            let Some(cached) = cache.get(&piece.key) else {
                continue;
            };
            let layout = &cached.layout;
            ctx.set_transform(Affine::translate((text_x, piece.y)));
            let committed = |raw: usize| piece.committed(raw);

            // Backgrounds: whole lines of code blocks, then inline code.
            if !decorations.is_empty() {
                for line in layout.lines() {
                    let metrics = line.metrics();
                    let (top, bottom) = (
                        f64::from(metrics.block_min_coord),
                        f64::from(metrics.block_max_coord),
                    );
                    let range = line.text_range();
                    let start = committed(range.start).unwrap_or(piece.base);
                    let end = committed(range.end).unwrap_or(start);
                    if let Some(background) =
                        line_background_in(decorations, start..end.max(start + 1))
                    {
                        ctx.set_paint(color(background));
                        ctx.fill_rect(&Rect::new(-inset, top, text_width + inset, bottom));
                    }
                    let pad = (bottom - top) * 0.12;
                    for_each_cluster(&line, |cluster, x0, x1| {
                        let Some(offset) = committed(cluster.text_range().start) else {
                            return;
                        };
                        if let Some(background) = style_at(decorations, offset).background {
                            let radius = 3.0 * f64::from(scale);
                            let rect = RoundedRect::new(x0, top + pad, x1, bottom - pad, radius);
                            ctx.set_paint(color(background));
                            ctx.fill_path(&rect.to_path(0.1));
                        }
                    });
                }
            }

            // Selection.
            let end = piece.base + piece.len;
            if selection.start < selection.end
                && selection.start <= end
                && selection.end > piece.base
            {
                let from = piece.raw(selection.start.max(piece.base) - piece.base);
                let to = piece.raw(selection.end.min(end) - piece.base);
                ctx.set_paint(color(palette.selection));
                if from < to {
                    Selection::new(
                        Cursor::from_byte_index(layout, from, Affinity::Downstream),
                        Cursor::from_byte_index(layout, to, Affinity::Upstream),
                    )
                    .geometry_with(layout, |rect, _| {
                        ctx.fill_rect(&Rect::new(rect.x0, rect.y0, rect.x1, rect.y1));
                    });
                }
                // The selected line break: a sliver at the end of the line.
                if selection.end > end
                    && let Some(last) = layout.lines().last()
                {
                    let m = last.metrics();
                    let x = f64::from(m.advance + m.offset);
                    let w = f64::from(scale) * 6.0;
                    ctx.fill_rect(&Rect::new(
                        x,
                        f64::from(m.block_min_coord),
                        x + w,
                        f64::from(m.block_max_coord),
                    ));
                }
            }

            // Glyphs.
            for line in layout.lines() {
                let mut previous_run: Option<Range<usize>> = None;
                for item in line.items() {
                    let PositionedLayoutItem::GlyphRun(glyph_run) = item else {
                        continue;
                    };
                    let style = glyph_run.style();
                    let run = glyph_run.run();
                    if let Some(underline) = &style.underline {
                        let metrics = run.font_metrics();
                        let offset = underline.offset.unwrap_or(metrics.underline_offset);
                        let size = underline.size.unwrap_or(metrics.underline_size);
                        let x0 = f64::from(glyph_run.offset());
                        let x1 = x0 + f64::from(glyph_run.advance());
                        glyphs::line(
                            ctx,
                            ink,
                            (x0, x1),
                            glyph_run.baseline(),
                            offset,
                            size,
                            scale,
                        );
                    }
                    if decorations.is_empty() {
                        glyph_buffer.clear();
                        glyph_buffer.extend(glyph_run.positioned_glyphs().map(|glyph| Glyph {
                            id: glyph.id,
                            x: glyph.x,
                            y: glyph.y,
                        }));
                        let paint = GlyphPaint {
                            color: ink,
                            embolden: false,
                            skew: None,
                        };
                        glyphs::draw(ctx, resources, run, glyph_buffer, paint);
                        continue;
                    }
                    if previous_run.as_ref() == Some(&run.text_range()) {
                        continue;
                    }
                    previous_run = Some(run.text_range());
                    draw_decorated_run(
                        ctx,
                        resources,
                        run,
                        glyph_run.offset(),
                        glyph_run.baseline(),
                        ink,
                        &committed,
                        decorations,
                        scale,
                        glyph_buffer,
                    );
                }
            }

            // Caret.
            let caret = match piece.compose {
                Some((at, _, Some((cursor, _)))) => Some(at + cursor),
                Some((_, _, None)) => None,
                None => piece.caret.map(|local| piece.raw(local)),
            };
            if *caret_on && let Some(caret) = caret {
                let affinity = if *upstream && piece.compose.is_none() {
                    Affinity::Upstream
                } else {
                    Affinity::Downstream
                };
                let rect = Cursor::from_byte_index(layout, caret, affinity)
                    .geometry(layout, CARET_WIDTH * scale);
                ctx.set_paint(color(palette.caret));
                ctx.fill_rect(&Rect::new(rect.x0, rect.y0, rect.x1, rect.y1));
            }
        }

        ctx.flush();
        ctx.render(&mut *pixmap, resources);
        pixmap
    }

    fn drop_layouts_older(&mut self, frame: u64) {
        self.cache.retain(|_, cached| cached.used + 2 >= frame);
    }

    /// Render and encode as PNG, for tests.
    pub fn encode_png(&mut self) -> Result<Vec<u8>, String> {
        let (width, height) = (self.width.max(1), self.height.max(1));
        let data = self.render().data().to_vec();
        Pixmap::from_parts(data, width, height)
            .into_png()
            .map_err(|err| err.to_string())
    }
}

/// A composition inside a segment: where it starts in the laid-out text,
/// how long it is, and the input method's cursor in it.
type PieceCompose = (usize, usize, Option<(usize, usize)>);

/// A segment in view, with what drawing it needs to know.
struct Piece {
    key: Key,
    /// Top, physical pixels from the top of the view.
    y: f64,
    /// Committed offset of the segment's first byte.
    base: usize,
    /// Committed length of the segment.
    len: usize,
    /// Composition in this segment: where, how long, its cursor.
    compose: Option<PieceCompose>,
    /// The caret, if it is in this segment: committed offset in it.
    caret: Option<usize>,
}

impl Piece {
    fn committed(&self, raw: usize) -> Option<usize> {
        match self.compose {
            Some((at, len, _)) if raw >= at + len => Some(self.base + raw - len),
            Some((at, _, _)) if raw > at => None,
            _ => Some(self.base + raw),
        }
    }

    fn raw(&self, local: usize) -> usize {
        match self.compose {
            Some((at, len, _)) if local >= at => local + len,
            _ => local,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::text_surface::TextStyle;

    fn view(text: &str) -> TextView {
        let mut v = TextView::new(Rope::from_str(text));
        v.set_font_size(15.0);
        v.resize(1200, 800, 2.0);
        v
    }

    fn ink_pixels(v: &mut TextView) -> usize {
        let ink = Palette::default().ink;
        let data = v.render().data_as_u8_slice();
        let (pixels, _) = data.as_chunks::<4>();
        pixels.iter().filter(|px| **px == ink).count()
    }

    fn apply(v: &mut TextView, action: Action) -> usize {
        v.apply(action);
        v.selection().1
    }

    const RIGHT: Action = Action::Right {
        select: false,
        word: false,
    };
    const LEFT: Action = Action::Left {
        select: false,
        word: false,
    };

    #[test]
    fn text_is_drawn_and_empty_text_is_fine() {
        let mut v = view("HHHH 中文");
        assert!(ink_pixels(&mut v) > 50);
        let mut empty = view("");
        empty.render();
        assert_eq!(empty.visible_range(), 0..0);
        let (_, _, w, h) = empty.ime_cursor_area();
        assert!(w >= 1.0 && h > 10.0);
    }

    #[test]
    fn edits_keep_the_line_records_exact() {
        let mut mirror = String::from("第一行\nsecond line\n\n第四行 with text");
        let mut v = view(&mirror);
        let pieces = ["a", "中", "\n", "\n\n", "", "長い文章", " "];
        let mut seed: u64 = 11;
        let mut next = |bound: usize| {
            seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1);
            ((seed >> 33) as usize) % bound.max(1)
        };
        for step in 0..1500 {
            let chars = mirror.chars().count();
            let a = next(chars + 1);
            let b = (a + next(5)).min(chars);
            let at = |c: usize| {
                mirror
                    .char_indices()
                    .nth(c)
                    .map_or(mirror.len(), |(i, _)| i)
            };
            let (start, end) = (at(a), at(b));
            let inserted = pieces[next(pieces.len())];
            mirror.replace_range(start..end, inserted);
            v.replace_range(start..end, inserted);
            assert_eq!(v.text(), mirror, "step {step}");
            assert_eq!(v.lines.lens, line_lengths(&v.text), "step {step}");
            assert_eq!(
                v.selection(),
                (start + inserted.len(), start + inserted.len())
            );
            if step % 50 == 0 {
                v.render();
                v.lines.refresh_with(&v.long);
                let sum: f64 = (0..v.lines.count())
                    .map(|line| f64::from(v.lines.height(line, &v.long)))
                    .sum();
                assert!((v.lines.total() - sum).abs() < 0.5, "step {step}");
            }
        }
    }

    #[test]
    fn the_caret_moves_across_lines_and_characters() {
        let text = "ab\n中文\n\nxyz";
        let mut v = view(text);
        v.set_selection(0, 0);
        assert_eq!(apply(&mut v, RIGHT), 1);
        assert_eq!(apply(&mut v, RIGHT), 2);
        // Over the line break.
        assert_eq!(apply(&mut v, RIGHT), 3);
        assert_eq!(apply(&mut v, RIGHT), 6);
        assert_eq!(apply(&mut v, LEFT), 3);
        assert_eq!(apply(&mut v, LEFT), 2);
        // Down keeps the column as well as it can.
        v.set_selection(1, 1);
        let down = apply(&mut v, Action::Down { select: false });
        assert!((3..=9).contains(&down), "{down}");
        let down = apply(&mut v, Action::Down { select: false });
        assert_eq!(down, 10, "the empty line");
        let down = apply(&mut v, Action::Down { select: false });
        assert!((11..=14).contains(&down));
        assert_eq!(apply(&mut v, Action::Down { select: false }), text.len());
        assert_eq!(apply(&mut v, Action::Up { select: false }), 10);
        // Line start and end.
        v.set_selection(4, 4);
        assert_eq!(apply(&mut v, Action::LineEnd { select: false }), 9);
        assert_eq!(apply(&mut v, Action::LineStart { select: false }), 3);
        // Selecting keeps the anchor.
        v.apply(Action::Right {
            select: true,
            word: false,
        });
        assert_eq!(v.selection(), (3, 6));
        v.apply(Action::SelectAll);
        assert_eq!(v.selection(), (0, text.len()));
    }

    #[test]
    fn a_click_puts_the_caret_under_the_pointer() {
        let mut v = view("first line\nsecond line\nthird line");
        v.render();
        v.set_selection(0, 0);
        let (x0, y0, _, h) = v.ime_cursor_area();
        // A click one line down, at the start, lands on the second line.
        v.pointer_down(x0, y0 + h * 1.5, false);
        assert_eq!(v.selection(), (11, 11));
        v.pointer_drag(x0 + 400.0, y0 + h * 2.5);
        let (anchor, focus) = v.selection();
        assert_eq!(anchor, 11);
        assert!(focus > 22, "{focus}");
    }

    #[test]
    fn composition_is_shown_but_not_part_of_the_text() {
        let mut v = view("ab");
        v.set_selection(2, 2);
        let (_, _, caret_width, _) = v.ime_cursor_area();
        v.compose("nihao", Some((5, 5)));
        assert!(v.is_composing());
        assert_eq!(v.text(), "ab");
        let (_, _, width, _) = v.ime_cursor_area();
        assert!(width > caret_width);
        v.render();
        // Keys and clicks wait for the composition to end.
        v.apply(LEFT);
        assert_eq!(v.selection(), (2, 2));
        v.end_composition();
        assert!(!v.is_composing());
        v.replace_range(2..2, "你好");
        assert_eq!(v.text(), "ab你好");
    }

    #[test]
    fn scrolling_through_a_large_text_keeps_few_layouts() {
        let text: String = (0..20_000)
            .map(|i| format!("第 {i} 行，some text 混在一起。\n"))
            .collect();
        let mut v = view(&text);
        v.render();
        let (_, max) = v.scroll_range();
        assert!(max > 100_000.0);
        let mut last = v.top_position();
        for _ in 0..200 {
            assert!(v.scroll_by(1200.0));
            v.render();
            let top = v.top_position();
            assert!(top > last);
            last = top;
            assert!(v.cache.len() < 200, "{} layouts kept", v.cache.len());
        }
        let line = text.find("第 15000 行").unwrap();
        v.scroll_to_position(line as f64);
        assert!((v.top_position() - line as f64).abs() < 2.0);
        let visible = v.visible_range();
        assert!(visible.contains(&line));
        // The caret at the very end, revealed.
        v.set_selection(text.len(), text.len());
        v.reveal_caret();
        v.render();
        let (_, y, _, h) = v.ime_cursor_area();
        assert!(y >= 0.0 && y + h <= 800.0, "caret at {y}");
    }

    #[test]
    fn a_line_longer_than_a_segment_is_laid_out_in_pieces() {
        let text = "長い行の文章 long line text ".repeat(4000);
        assert!(text.len() > 10 * SEGMENT);
        let mut v = view(&text);
        v.render();
        assert!(v.cache.len() <= 3, "{} segments laid out", v.cache.len());
        // Typing in the middle keeps the text right and lays out little.
        let middle = {
            let mut at = text.len() / 2;
            while !text.is_char_boundary(at) {
                at -= 1;
            }
            at
        };
        v.set_selection(middle, middle);
        v.reveal_caret();
        v.render();
        v.replace_range(middle..middle, "字");
        v.render();
        assert!(v.cache.len() <= 6);
        let mut expected = text.clone();
        expected.insert(middle, '字');
        assert_eq!(v.text(), expected);
        // Moving across a segment boundary.
        let boundary = v.segment_range(0, 1).start;
        v.set_selection(boundary, boundary);
        let before = apply(&mut v, LEFT);
        assert!(before < boundary);
        assert_eq!(apply(&mut v, RIGHT), boundary);
        // The end of the text can be reached and shown.
        v.set_selection(v.len(), v.len());
        v.reveal_caret();
        v.render();
    }

    #[test]
    fn decorations_recolour_without_moving_text() {
        let mut v = view("HHHHHHHH");
        v.set_caret_on(false);
        let plain = ink_pixels(&mut v);
        let (x, y, _, _) = v.ime_cursor_area();
        let grey = [0x80, 0x80, 0x80, 0xff];
        v.set_decorations(vec![Decoration {
            range: 0..4,
            style: TextStyle {
                color: Some(grey),
                bold: true,
                ..TextStyle::default()
            },
        }]);
        let data = v.render().data_as_u8_slice().to_vec();
        let (pixels, _) = data.as_chunks::<4>();
        assert!(pixels.iter().filter(|px| **px == grey).count() > 20);
        assert!(ink_pixels(&mut v) < plain);
        assert_eq!(v.ime_cursor_area().0, x);
        assert_eq!(v.ime_cursor_area().1, y);
    }

    #[test]
    fn resizing_keeps_the_same_text_at_the_top() {
        let text: String = (0..3000)
            .map(|i| format!("line {i} with some words in it\n"))
            .collect();
        let mut v = view(&text);
        let line = text.find("line 2000 ").unwrap();
        v.scroll_to_position(line as f64);
        v.render();
        v.resize(700, 800, 2.0);
        v.render();
        let top = v.top_position();
        assert!((top - line as f64).abs() < 40.0, "{top} vs {line}");
    }
}
