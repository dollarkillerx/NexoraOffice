//! The line breaker, from the Phase 0 gate G3 (`spikes/g3-cjk`). It knows
//! nothing about fonts or shaping: it takes clusters that already carry an
//! advance and a "may break before" flag, and decides where lines end and
//! where every cluster sits.
//!
//! All arithmetic is in 26.6 fixed point (1/64 pixel, `Fx`). Line breaks are
//! decided by comparing sums of advances with the line width; doing that in
//! integers means the same input gives the same breaks on every platform, with
//! no dependence on floating-point rounding.
//!
//! Rules implemented, each behind an option:
//!
//! - **Adjacent punctuation compression.** Full-width punctuation keeps a blank
//!   half em on one side. When two such marks meet, the blank halves are removed
//!   so that at most half an em separates them.
//! - **Line-end compression.** A closing mark, comma or full stop at the end of a
//!   line gives up its blank trailing half if that makes the line fit.
//! - **Hanging punctuation.** A comma or full stop that still does not fit may
//!   sit outside the line.
//! - **CJK–Latin spacing.** A quarter em is inserted where Han or kana meets a
//!   Latin letter or digit.
//! - **Justification.** Slack is shared between the gaps where expansion is
//!   allowed: word spaces and the boundaries between CJK characters.
//!
//! Where a break is allowed at all is not decided here; it comes in on each
//! cluster (from UAX #14 in strict mode), which is what keeps closing marks and
//! small kana off the start of a line.

use std::ops::Range;

use crate::class::Class;

/// 26.6 fixed point: one unit is 1/64 of a pixel.
pub type Fx = i32;

#[derive(Clone, Debug)]
pub struct Cluster {
    /// Byte range in the paragraph text.
    pub text: Range<usize>,
    pub class: Class,
    pub advance: Fx,
    /// One em at this cluster's font size.
    pub em: Fx,
    /// A line may end just before this cluster.
    pub break_before: bool,
    /// This cluster is a hard line break; the line ends after it.
    pub newline: bool,
    /// This cluster is a tab: its advance comes from the tab stops.
    pub tab: bool,
    /// This cluster is the decimal point a decimal tab stop lines up on.
    pub point: bool,
}

impl Cluster {
    /// Punctuation rules only apply to marks that really are one em wide. A curly
    /// quote taken from a Latin font is proportional and must be left alone.
    fn is_full_width(&self) -> bool {
        self.advance * 10 >= self.em * 9
    }

    fn blank_trailing_half(&self) -> bool {
        self.class.has_blank_trailing_half() && self.is_full_width()
    }

    fn blank_leading_half(&self) -> bool {
        self.class == Class::Opening && self.is_full_width()
    }

    fn collapses_at_line_end(&self) -> bool {
        self.class == Class::Space || self.newline
    }
}

#[derive(Clone, Copy, Debug)]
pub struct Options {
    pub max_width: Fx,
    pub compress_adjacent: bool,
    pub compress_line_end: bool,
    pub hang: bool,
    pub autospace: bool,
    pub justify: bool,
    /// Justify the last line of the paragraph and lines ended by a line
    /// break too.
    pub distribute: bool,
}

/// How a tab stop lines text up.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum TabAlign {
    #[default]
    Left,
    Center,
    Right,
    /// The decimal point is at the stop.
    Decimal,
}

/// What fills the gap a tab makes.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Leader {
    #[default]
    None,
    Dots,
    Hyphens,
    Underscore,
    MiddleDots,
}

/// A tab stop, from the margin the tab positions are counted from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Stop {
    pub position: Fx,
    pub align: TabAlign,
    pub leader: Leader,
}

/// What differs between the lines of a paragraph: where the first starts, and
/// where tabs go.
#[derive(Clone, Copy, Debug, Default)]
pub struct Shape<'a> {
    /// The first line starts this far from the others (negative: hanging).
    pub first_indent: Fx,
    /// Distance from the margin the stops are counted from to the left edge
    /// of the lines after the first.
    pub margin_offset: Fx,
    /// Sorted by position.
    pub stops: &'a [Stop],
    /// Interval of the stops after the explicit ones; 0 for none, and a tab
    /// then keeps the width of its glyph.
    pub default_tab: Fx,
}

/// A tab's gap on a line, for drawing leaders.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TabGap {
    /// The tab's cluster.
    pub index: usize,
    pub leader: Leader,
}

impl Options {
    /// Every rule on: what a Chinese or Japanese document in a word processor
    /// gets by default.
    pub fn cjk(max_width: Fx) -> Self {
        Self {
            max_width,
            compress_adjacent: true,
            compress_line_end: true,
            hang: true,
            autospace: true,
            justify: true,
            distribute: false,
        }
    }

    /// No rules: plain greedy breaking at the allowed opportunities.
    pub fn plain(max_width: Fx) -> Self {
        Self {
            max_width,
            compress_adjacent: false,
            compress_line_end: false,
            hang: false,
            autospace: false,
            justify: false,
            distribute: false,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BreakKind {
    /// Broke at an allowed opportunity because the next one did not fit.
    Soft,
    /// Ended by a newline.
    Hard,
    /// Nothing fitted; broke inside a run with no opportunity.
    Emergency,
    /// End of the paragraph.
    End,
}

/// What was done to the last visible cluster to make the line fit.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EndTreatment {
    None,
    Compressed,
    Hung,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Placed {
    pub index: usize,
    /// Left edge of the cluster's box.
    pub x: Fx,
    /// Width of the box after compression.
    pub advance: Fx,
    /// Where to start drawing the glyphs. Left of `x` when the blank leading half
    /// of an opening mark was removed.
    pub glyph_x: Fx,
}

#[derive(Clone, Debug)]
pub struct Line {
    /// Cluster indices on this line, including trailing spaces and the newline.
    pub clusters: Range<usize>,
    pub placed: Vec<Placed>,
    /// Right edge of the content that must stay inside the line.
    pub width: Fx,
    pub kind: BreakKind,
    pub end: EndTreatment,
    pub justified: bool,
    pub tabs: Vec<TabGap>,
}

struct Breaker<'a> {
    clusters: &'a [Cluster],
    options: Options,
    shape: Shape<'a>,
    /// Where the line starts, counted from the margin the stops use.
    origin: Fx,
    /// There are tabs to resolve against stops.
    tabbed: bool,
}

impl Breaker<'_> {
    /// One past the last cluster that takes up room on the line `start..end`.
    fn visible_end(&self, start: usize, end: usize) -> usize {
        let mut last = end;
        while last > start && self.clusters[last - 1].collapses_at_line_end() {
            last -= 1;
        }
        last
    }

    /// Space inserted before cluster `i` on a line starting at `start`.
    fn space_before(&self, i: usize, start: usize) -> Fx {
        if !self.options.autospace || i == start {
            return 0;
        }
        let (previous, current) = (&self.clusters[i - 1], &self.clusters[i]);
        let meets = matches!(
            (previous.class, current.class),
            (Class::Ideographic, Class::Western) | (Class::Western, Class::Ideographic)
        );
        if meets {
            current.em.max(previous.em) / 4
        } else {
            0
        }
    }

    /// Blank leading half removed from cluster `i`.
    fn leading_trim(&self, i: usize, start: usize) -> Fx {
        let cluster = &self.clusters[i];
        if !self.options.compress_adjacent || i == start || !cluster.blank_leading_half() {
            return 0;
        }
        // An opening mark right after another opening mark.
        if self.clusters[i - 1].blank_leading_half() {
            cluster.em / 2
        } else {
            0
        }
    }

    /// Blank trailing half removed from cluster `i` because of what follows it on
    /// the same line. `last` is one past the last visible cluster.
    fn trailing_trim(&self, i: usize, last: usize) -> Fx {
        let cluster = &self.clusters[i];
        if !self.options.compress_adjacent || i + 1 >= last || !cluster.blank_trailing_half() {
            return 0;
        }
        let next = &self.clusters[i + 1];
        let follows = (next.class.has_blank_trailing_half() || next.class == Class::Opening)
            && next.is_full_width();
        if follows { cluster.em / 2 } else { 0 }
    }

    /// Width of clusters `start..last` with adjacency rules applied and nothing
    /// done to the end of the line.
    fn natural_width(&self, start: usize, last: usize) -> Fx {
        if self.has_tab(start, last) {
            let mut boxes: Vec<(Fx, Fx, Fx)> = (start..last)
                .map(|i| {
                    let lead = self.leading_trim(i, start);
                    (
                        self.space_before(i, start),
                        self.clusters[i].advance - lead - self.trailing_trim(i, last),
                        lead,
                    )
                })
                .collect();
            self.resolve_tabs(start, &mut boxes, None);
            return boxes
                .iter()
                .map(|(space, advance, _)| space + advance)
                .sum();
        }
        (start..last)
            .map(|i| {
                self.space_before(i, start) + self.clusters[i].advance
                    - self.leading_trim(i, start)
                    - self.trailing_trim(i, last)
            })
            .sum()
    }

    fn has_tab(&self, start: usize, last: usize) -> bool {
        self.tabbed && self.clusters[start..last].iter().any(|c| c.tab)
    }

    /// The first stop after `at`, counted from the margin.
    fn next_stop(&self, at: Fx) -> Option<(Fx, TabAlign, Leader)> {
        if let Some(stop) = self.shape.stops.iter().find(|stop| stop.position > at) {
            return Some((stop.position, stop.align, stop.leader));
        }
        let interval = self.shape.default_tab;
        (interval > 0).then(|| {
            (
                (at.div_euclid(interval) + 1) * interval,
                TabAlign::Left,
                Leader::None,
            )
        })
    }

    /// Give every tab in `boxes` (the boxes of the clusters from `start`) the
    /// width that takes the text after it to its stop.
    fn resolve_tabs(
        &self,
        start: usize,
        boxes: &mut [(Fx, Fx, Fx)],
        mut gaps: Option<&mut Vec<TabGap>>,
    ) {
        let mut x: Fx = 0;
        for k in 0..boxes.len() {
            let i = start + k;
            if self.clusters[i].tab {
                let at = x + boxes[k].0 + self.origin;
                if let Some((position, align, leader)) = self.next_stop(at) {
                    let (mut segment, mut before_point) = (0, None);
                    for (j, b) in boxes.iter().enumerate().skip(k + 1) {
                        if self.clusters[start + j].tab {
                            break;
                        }
                        if before_point.is_none() && self.clusters[start + j].point {
                            before_point = Some(segment + b.0);
                        }
                        segment += b.0 + b.1;
                    }
                    let need = match align {
                        TabAlign::Left => 0,
                        TabAlign::Center => segment / 2,
                        TabAlign::Right => segment,
                        TabAlign::Decimal => before_point.unwrap_or(segment),
                    };
                    boxes[k].1 = (position - need - at).max(0);
                    if let Some(gaps) = gaps.as_deref_mut() {
                        gaps.push(TabGap { index: i, leader });
                    }
                }
            }
            x += boxes[k].0 + boxes[k].1;
        }
    }

    /// The least invasive end-of-line treatment that makes `start..end` fit, and
    /// the width that then has to fit inside the line. `None` if nothing does.
    fn fit(&self, start: usize, end: usize) -> Option<(EndTreatment, Fx)> {
        let last = self.visible_end(start, end);
        let natural = self.natural_width(start, last);
        if natural <= self.options.max_width {
            return Some((EndTreatment::None, natural));
        }
        if last == start {
            return None;
        }
        let tail = &self.clusters[last - 1];
        if self.options.compress_line_end && tail.blank_trailing_half() {
            let compressed = natural - tail.em / 2;
            if compressed <= self.options.max_width {
                return Some((EndTreatment::Compressed, compressed));
            }
        }
        if self.options.hang && tail.class.can_hang() && tail.is_full_width() {
            let without = natural - tail.advance;
            if without <= self.options.max_width {
                return Some((EndTreatment::Hung, without));
            }
        }
        None
    }

    /// Whether the gap after cluster `i` may be stretched when justifying.
    fn stretches_after(&self, i: usize) -> bool {
        let (a, b) = (self.clusters[i].class, self.clusters[i + 1].class);
        if self.clusters[i].tab || self.clusters[i + 1].tab {
            return false;
        }
        if a == Class::Space {
            return true;
        }
        let cjk = |c: Class| c == Class::Ideographic || c.is_cjk_punctuation();
        // Never pull a mark away from the text it belongs to.
        let glued = a == Class::Opening || b.has_blank_trailing_half() || b == Class::MiddleDot;
        (cjk(a) || cjk(b)) && !glued && b != Class::Space
    }

    fn place(
        &self,
        start: usize,
        end: usize,
        kind: BreakKind,
        end_treatment: EndTreatment,
    ) -> Line {
        let last = self.visible_end(start, end);
        // The hung mark sits outside the line and takes no part in justification.
        let inside = if end_treatment == EndTreatment::Hung {
            last - 1
        } else {
            last
        };

        let mut boxes: Vec<(Fx, Fx, Fx)> = (start..end)
            .map(|i| {
                if i >= last {
                    return (0, 0, 0);
                }
                let lead = self.leading_trim(i, start);
                let mut advance = self.clusters[i].advance - lead - self.trailing_trim(i, last);
                if i == last - 1 && end_treatment == EndTreatment::Compressed {
                    advance -= self.clusters[i].em / 2;
                }
                (self.space_before(i, start), advance, lead)
            })
            .collect();
        let mut tabs = Vec::new();
        if self.has_tab(start, last) {
            self.resolve_tabs(start, &mut boxes[..last - start], Some(&mut tabs));
        }
        let content: Fx = boxes[..inside - start]
            .iter()
            .map(|(space, advance, _)| space + advance)
            .sum();

        let gaps: Vec<usize> = (start..inside.saturating_sub(1))
            .filter(|&i| self.stretches_after(i))
            .collect();
        let slack = self.options.max_width - content;
        let justified = self.options.justify
            && (kind == BreakKind::Soft || self.options.distribute)
            && slack > 0
            && !gaps.is_empty();
        if justified {
            let (each, extra) = (slack / gaps.len() as Fx, slack % gaps.len() as Fx);
            for (n, &i) in gaps.iter().enumerate() {
                // Stretching a gap is the same as widening the box before it.
                boxes[i - start].1 += each + Fx::from((n as Fx) < extra);
            }
        }

        let mut x = 0;
        let mut width = 0;
        let placed = (start..end)
            .zip(&boxes)
            .map(|(index, &(space, advance, lead))| {
                x += space;
                let placed = Placed {
                    index,
                    x,
                    advance,
                    glyph_x: x - lead,
                };
                x += advance;
                if index < inside {
                    width = x;
                }
                placed
            })
            .collect();
        Line {
            clusters: start..end,
            placed,
            width,
            kind,
            end: end_treatment,
            justified,
            tabs,
        }
    }
}

/// Break a paragraph into lines and position every cluster.
pub fn break_lines(clusters: &[Cluster], options: Options) -> Vec<Line> {
    break_lines_shaped(clusters, options, &Shape::default())
}

/// The same for a paragraph whose first line starts elsewhere and which has
/// tab stops. `options.max_width` is the width of the lines after the first.
pub fn break_lines_shaped(clusters: &[Cluster], options: Options, shape: &Shape<'_>) -> Vec<Line> {
    let count = clusters.len();
    let mut lines = Vec::new();
    let mut start = 0;
    let tabbed = has_tabs_to_resolve(clusters, shape);
    while start < count {
        let indent = if lines.is_empty() {
            shape.first_indent
        } else {
            0
        };
        let line = break_line(
            clusters,
            options,
            shape,
            tabbed,
            start,
            indent,
            options.max_width,
        );
        start = line.clusters.end;
        lines.push(line);
    }
    lines
}

/// Whether tabs in `clusters` have stops to be resolved against.
pub fn has_tabs_to_resolve(clusters: &[Cluster], shape: &Shape<'_>) -> bool {
    (!shape.stops.is_empty() || shape.default_tab > 0) && clusters.iter().any(|cluster| cluster.tab)
}

/// The one line that starts at cluster `start` and runs from `left` to
/// `right` (counted from the paragraph's left edge). `tabbed` is
/// [`has_tabs_to_resolve`]; `start` must be below `clusters.len()`.
pub fn break_line(
    clusters: &[Cluster],
    options: Options,
    shape: &Shape<'_>,
    tabbed: bool,
    start: usize,
    left: Fx,
    right: Fx,
) -> Line {
    let count = clusters.len();
    let breaker = Breaker {
        clusters,
        options: Options {
            max_width: right.saturating_sub(left),
            ..options
        },
        shape: *shape,
        origin: shape.margin_offset + left,
        tabbed,
    };
    // The furthest allowed end that fits, and how it fits.
    let mut best: Option<(usize, EndTreatment, BreakKind)> = None;
    let mut end = start + 1;
    loop {
        let hard = clusters[end - 1].newline;
        let finished = end == count;
        if hard || finished || clusters[end].break_before {
            match breaker.fit(start, end) {
                Some((treatment, _)) => {
                    let kind = if hard {
                        BreakKind::Hard
                    } else if finished {
                        BreakKind::End
                    } else {
                        BreakKind::Soft
                    };
                    best = Some((end, treatment, kind));
                    if hard || finished {
                        break;
                    }
                }
                // Too long: fall back to the last opportunity that fitted,
                // or to an emergency break if there was none.
                None => break,
            }
        }
        end += 1;
    }

    let (end, treatment, kind) = best.unwrap_or_else(|| {
        // No opportunity fits: break wherever the line is full.
        let mut cut = end.min(count) - 1;
        while cut > start + 1 && breaker.fit(start, cut).is_none() {
            cut -= 1;
        }
        let cut = cut.max(start + 1);
        let treatment = breaker
            .fit(start, cut)
            .map_or(EndTreatment::None, |(t, _)| t);
        (cut, treatment, BreakKind::Emergency)
    });
    breaker.place(start, end, kind, treatment)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::class::{classify, is_wide};

    const EM: Fx = 64 * 16;

    /// One cluster per character, one em for wide characters and half an em for
    /// the rest, with a break allowed everywhere except around punctuation that
    /// must stay attached.
    fn clusters(text: &str) -> Vec<Cluster> {
        let mut out: Vec<Cluster> = Vec::new();
        for (offset, c) in text.char_indices() {
            let class = classify(c);
            let glued_to_previous = class.has_blank_trailing_half();
            let previous_opens = out.last().is_some_and(|p| p.class == Class::Opening);
            out.push(Cluster {
                text: offset..offset + c.len_utf8(),
                class,
                advance: if c == '\n' {
                    0
                } else if is_wide(c) {
                    EM
                } else {
                    EM / 2
                },
                em: EM,
                break_before: offset > 0 && !glued_to_previous && !previous_opens,
                newline: c == '\n',
                tab: false,
                point: false,
            });
        }
        out
    }

    fn texts<'a>(text: &'a str, clusters: &[Cluster], lines: &[Line]) -> Vec<&'a str> {
        lines
            .iter()
            .map(|line| {
                let range = clusters[line.clusters.start].text.start
                    ..clusters[line.clusters.end - 1].text.end;
                text[range].trim_end_matches('\n')
            })
            .collect()
    }

    fn broken(text: &str, options: Options) -> (Vec<Cluster>, Vec<Line>) {
        let clusters = clusters(text);
        let lines = break_lines(&clusters, options);
        (clusters, lines)
    }

    #[test]
    fn plain_breaking_fills_each_line() {
        let text = "一二三四五六七八九十";
        let (c, lines) = broken(text, Options::plain(4 * EM));
        assert_eq!(texts(text, &c, &lines), ["一二三四", "五六七八", "九十"]);
        assert_eq!(lines[0].kind, BreakKind::Soft);
        assert_eq!(lines[2].kind, BreakKind::End);
    }

    #[test]
    fn a_full_stop_is_never_pushed_to_the_next_line() {
        let text = "一二三四。五六";
        // Without help the stop does not fit, so "四。" moves down together.
        let (c, lines) = broken(text, Options::plain(4 * EM));
        assert_eq!(texts(text, &c, &lines), ["一二三", "四。五六"]);
    }

    #[test]
    fn line_end_compression_makes_room_for_half_an_em() {
        let text = "一二三四。五六";
        let mut options = Options::plain(4 * EM + EM / 2);
        options.compress_line_end = true;
        let (c, lines) = broken(text, options);
        assert_eq!(texts(text, &c, &lines), ["一二三四。", "五六"]);
        assert_eq!(lines[0].end, EndTreatment::Compressed);
        assert_eq!(lines[0].width, 4 * EM + EM / 2);
    }

    #[test]
    fn a_comma_hangs_when_nothing_else_fits() {
        let text = "一二三四，五六";
        let mut options = Options::plain(4 * EM);
        options.hang = true;
        let (c, lines) = broken(text, options);
        assert_eq!(texts(text, &c, &lines), ["一二三四，", "五六"]);
        assert_eq!(lines[0].end, EndTreatment::Hung);
        // The text block ends at the margin; the comma starts exactly there.
        assert_eq!(lines[0].width, 4 * EM);
        assert_eq!(lines[0].placed[4].x, 4 * EM);
    }

    #[test]
    fn compression_is_preferred_over_hanging() {
        let text = "一二三四，五六";
        let mut options = Options::plain(4 * EM + EM / 2);
        options.compress_line_end = true;
        options.hang = true;
        let (_, lines) = broken(text, options);
        assert_eq!(lines[0].end, EndTreatment::Compressed);
    }

    #[test]
    fn adjacent_marks_share_half_an_em() {
        // 」 then 「: two ems of boxes become one and a half.
        let text = "一」「二";
        let mut options = Options::plain(100 * EM);
        options.compress_adjacent = true;
        let (_, lines) = broken(text, options);
        assert_eq!(lines[0].width, EM + EM / 2 + EM + EM);
        // The opening mark keeps its own blank half and is not shifted.
        assert_eq!(lines[0].placed[2].glyph_x, lines[0].placed[2].x);
    }

    #[test]
    fn stacked_marks_close_up_completely() {
        // 。 」 ） in a row: each of the first two loses its blank half.
        let text = "一。」）二";
        let mut options = Options::plain(100 * EM);
        options.compress_adjacent = true;
        let (_, lines) = broken(text, options);
        assert_eq!(lines[0].width, EM + EM / 2 + EM / 2 + EM + EM);
    }

    #[test]
    fn a_second_opening_mark_is_drawn_half_an_em_to_the_left() {
        let text = "「『一";
        let mut options = Options::plain(100 * EM);
        options.compress_adjacent = true;
        let (_, lines) = broken(text, options);
        let second = lines[0].placed[1];
        assert_eq!(second.advance, EM / 2);
        assert_eq!(second.glyph_x, second.x - EM / 2);
    }

    #[test]
    fn proportional_quotes_are_left_alone() {
        let mut c = clusters("a”，b");
        // The quote comes from a Latin font: a quarter em wide.
        c[1].advance = EM / 4;
        let mut options = Options::plain(100 * EM);
        options.compress_adjacent = true;
        let lines = break_lines(&c, options);
        assert_eq!(lines[0].placed[1].advance, EM / 4);
    }

    #[test]
    fn a_quarter_em_separates_han_from_latin() {
        let text = "中A中";
        let mut options = Options::plain(100 * EM);
        options.autospace = true;
        let (_, lines) = broken(text, options);
        assert_eq!(lines[0].width, EM + EM / 4 + EM / 2 + EM / 4 + EM);
        // Not at the start of a line, and not where a space already is.
        let (_, lines) = broken("A中 B", options);
        assert_eq!(lines[0].placed[0].x, 0);
        assert_eq!(lines[0].width, EM / 2 + EM / 4 + EM + EM / 2 + EM / 2);
    }

    #[test]
    fn justified_lines_end_exactly_at_the_margin() {
        let text = "一二三四五六七八九十一二三四五";
        let mut options = Options::plain(4 * EM + 37);
        options.justify = true;
        let (_, lines) = broken(text, options);
        for line in &lines[..lines.len() - 1] {
            assert!(line.justified);
            assert_eq!(line.width, options.max_width);
        }
        let last = lines.last().unwrap();
        assert!(
            !last.justified,
            "the last line of a paragraph is not stretched"
        );
    }

    #[test]
    fn justification_does_not_separate_a_mark_from_its_text() {
        let text = "一「二」三四五六七八";
        let mut options = Options::plain(5 * EM + 40);
        options.justify = true;
        let (_, lines) = broken(text, options);
        let placed = &lines[0].placed;
        // 「 stays against 二, and 」 stays against 二.
        assert_eq!(placed[2].x, placed[1].x + EM);
        assert_eq!(placed[3].x, placed[2].x + EM);
    }

    #[test]
    fn newlines_end_lines_and_take_no_room() {
        let text = "一二\n三四五\n";
        let (c, lines) = broken(text, Options::cjk(10 * EM));
        assert_eq!(texts(text, &c, &lines), ["一二", "三四五"]);
        assert!(
            lines
                .iter()
                .all(|line| line.kind == BreakKind::Hard && !line.justified)
        );
        assert_eq!(lines[0].width, 2 * EM);
    }

    #[test]
    fn trailing_spaces_do_not_count() {
        let text = "ab cd ef";
        let (c, lines) = broken(text, Options::plain(EM));
        assert_eq!(texts(text, &c, &lines), ["ab ", "cd ", "ef"]);
        assert_eq!(lines[0].width, EM);
    }

    #[test]
    fn an_unbreakable_run_is_cut_where_the_line_is_full() {
        let mut c = clusters("abcdefgh");
        for cluster in &mut c {
            cluster.break_before = false;
        }
        let lines = break_lines(&c, Options::plain(EM + EM / 2));
        assert_eq!(lines.len(), 3);
        assert!(
            lines[..2]
                .iter()
                .all(|line| line.kind == BreakKind::Emergency)
        );
        assert!(lines.iter().all(|line| line.width <= EM + EM / 2));
    }

    #[test]
    fn every_cluster_is_placed_exactly_once() {
        let text =
            "他说：“今天的会议改到下午三点。”\n（真的吗？）那我们还来得及吗？Rust 1.95 和 Slint。";
        for width in 3..30 {
            let (c, lines) = broken(text, Options::cjk(width * EM));
            let indices: Vec<usize> = lines
                .iter()
                .flat_map(|l| l.placed.iter().map(|p| p.index))
                .collect();
            assert_eq!(indices, (0..c.len()).collect::<Vec<_>>(), "width {width}");
        }
    }
}
