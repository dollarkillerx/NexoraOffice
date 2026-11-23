//! Row heights or column widths: a default size and the runs that differ.
//!
//! Positions are found by binary search over the runs, so a sheet with a
//! custom height on every one of a million rows still answers "which row is at
//! this height" in microseconds.

/// Sizes along one axis, in logical pixels. A size of 0 hides the line.
#[derive(Clone, Debug, PartialEq)]
pub struct Axis {
    default: f64,
    count: u32,
    /// Sorted, not overlapping, never empty.
    spans: Vec<Span>,
    /// Offset of each span's start.
    starts: Vec<f64>,
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct Span {
    start: u32,
    /// Exclusive.
    end: u32,
    size: f64,
}

impl Axis {
    /// `count` lines of `default` size.
    pub fn new(default: f64, count: u32) -> Self {
        Self {
            default: default.max(0.0),
            count,
            spans: Vec::new(),
            starts: Vec::new(),
        }
    }

    /// Give lines `start..end` their own size. Later calls win where they
    /// overlap earlier ones.
    pub fn set(&mut self, start: u32, end: u32, size: f64) {
        let end = end.min(self.count);
        if start >= end {
            return;
        }
        let size = if size.is_finite() { size.max(0.0) } else { 0.0 };
        let mut spans = Vec::with_capacity(self.spans.len() + 2);
        let mut inserted = false;
        for span in self.spans.drain(..) {
            if span.end <= start {
                spans.push(span);
                continue;
            }
            if span.start < start {
                spans.push(Span { end: start, ..span });
            }
            if !inserted {
                spans.push(Span { start, end, size });
                inserted = true;
            }
            if span.end > end {
                spans.push(Span {
                    start: span.start.max(end),
                    ..span
                });
            }
        }
        if !inserted {
            spans.push(Span { start, end, size });
        }
        self.spans = spans;
        self.reindex();
    }

    /// Like calling [`Axis::set`] for each, faster when there are many:
    /// `(start, end, size)` sorted by `start` and not overlapping.
    pub fn set_sorted(&mut self, runs: impl IntoIterator<Item = (u32, u32, f64)>) {
        let mut previous_end = 0;
        let mut ok = self.spans.is_empty();
        let mut spans = Vec::new();
        for (start, end, size) in runs {
            let end = end.min(self.count);
            if start >= end {
                continue;
            }
            ok &= start >= previous_end;
            previous_end = end;
            let size = if size.is_finite() { size.max(0.0) } else { 0.0 };
            spans.push(Span { start, end, size });
        }
        if ok {
            self.spans = spans;
            self.reindex();
        } else {
            for span in spans {
                self.set(span.start, span.end, span.size);
            }
        }
    }

    fn reindex(&mut self) {
        self.starts.clear();
        let mut offset = 0.0;
        let mut position = 0;
        for span in &self.spans {
            offset += f64::from(span.start - position) * self.default;
            self.starts.push(offset);
            offset += f64::from(span.end - span.start) * span.size;
            position = span.end;
        }
    }

    pub fn count(&self) -> u32 {
        self.count
    }

    pub fn default_size(&self) -> f64 {
        self.default
    }

    pub fn size(&self, index: u32) -> f64 {
        if index >= self.count {
            return 0.0;
        }
        let at = self.spans.partition_point(|span| span.end <= index);
        match self.spans.get(at) {
            Some(span) if span.start <= index => span.size,
            _ => self.default,
        }
    }

    /// Distance from the start to line `index`; `index == count` gives the
    /// whole length.
    pub fn offset(&self, index: u32) -> f64 {
        let index = index.min(self.count);
        let at = self.spans.partition_point(|span| span.end <= index);
        match self.spans.get(at) {
            Some(span) if span.start <= index => {
                self.starts[at] + f64::from(index - span.start) * span.size
            }
            _ => {
                // After span `at - 1`, before span `at`.
                let (base, from) = match at.checked_sub(1) {
                    Some(prev) => {
                        let span = self.spans[prev];
                        (
                            self.starts[prev] + f64::from(span.end - span.start) * span.size,
                            span.end,
                        )
                    }
                    None => (0.0, 0),
                };
                base + f64::from(index - from) * self.default
            }
        }
    }

    pub fn length(&self) -> f64 {
        self.offset(self.count)
    }

    /// The line at distance `position`, skipping hidden lines. Past the end
    /// gives the last line.
    pub fn index_at(&self, position: f64) -> u32 {
        if self.count == 0 {
            return 0;
        }
        if position <= 0.0 {
            return self.first_visible(0);
        }
        // Binary search over lines: offsets grow monotonically.
        let (mut lo, mut hi) = (0u32, self.count);
        while lo + 1 < hi {
            let mid = lo + (hi - lo) / 2;
            if self.offset(mid) <= position {
                lo = mid;
            } else {
                hi = mid;
            }
        }
        // `lo` is the last line starting at or before `position`; if it is
        // hidden, the line that covers the position is the next visible one.
        if self.size(lo) == 0.0 {
            self.first_visible(lo)
        } else {
            lo
        }
    }

    fn first_visible(&self, from: u32) -> u32 {
        let mut index = from;
        while index + 1 < self.count && self.size(index) == 0.0 {
            index += 1;
        }
        index
    }

    /// The next line after `index` that is not hidden, if any.
    pub fn next_visible(&self, index: u32) -> Option<u32> {
        let mut next = index.checked_add(1)?;
        while next < self.count {
            if self.size(next) > 0.0 {
                return Some(next);
            }
            next += 1;
            // Hidden runs are spans: jump over them in one step.
            let at = self.spans.partition_point(|span| span.end <= next);
            if let Some(span) = self.spans.get(at)
                && span.start <= next
                && span.size == 0.0
            {
                next = span.end;
            }
        }
        None
    }

    /// The line before `index` that is not hidden, if any.
    pub fn previous_visible(&self, index: u32) -> Option<u32> {
        let mut previous = index.min(self.count);
        while previous > 0 {
            previous -= 1;
            if self.size(previous) > 0.0 {
                return Some(previous);
            }
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn brute_offset(sizes: &[f64], index: u32) -> f64 {
        sizes[..index as usize].iter().sum()
    }

    #[test]
    fn defaults_only() {
        let axis = Axis::new(20.0, 1_048_576);
        assert_eq!(axis.offset(0), 0.0);
        assert_eq!(axis.offset(10), 200.0);
        assert_eq!(axis.length(), 20.0 * 1_048_576.0);
        assert_eq!(axis.index_at(0.0), 0);
        assert_eq!(axis.index_at(19.9), 0);
        assert_eq!(axis.index_at(20.0), 1);
        assert_eq!(axis.index_at(1e12), 1_048_575);
    }

    #[test]
    fn runs_override_and_split_each_other() {
        let mut axis = Axis::new(10.0, 100);
        axis.set(5, 10, 30.0);
        axis.set(8, 12, 0.0);
        axis.set(0, 1, 15.0);
        let mut sizes = vec![10.0; 100];
        sizes[5..10].fill(30.0);
        sizes[8..12].fill(0.0);
        sizes[0] = 15.0;
        for index in 0..=100 {
            assert_eq!(axis.offset(index), brute_offset(&sizes, index), "{index}");
        }
        for (index, size) in sizes.iter().enumerate() {
            assert_eq!(axis.size(index as u32), *size);
        }
        // Hidden lines are never the answer.
        assert_eq!(axis.index_at(axis.offset(8)), 12);
        assert_eq!(axis.next_visible(7), Some(12));
        assert_eq!(axis.previous_visible(12), Some(7));
        assert_eq!(axis.next_visible(99), None);
        assert_eq!(axis.previous_visible(0), None);
    }

    #[test]
    fn a_million_custom_rows_are_found_quickly() {
        let mut axis = Axis::new(20.0, 1_048_576);
        axis.set_sorted((0..1_000_000u32).map(|row| (row, row + 1, 15.0 + f64::from(row % 7))));
        let start = std::time::Instant::now();
        for probe in 0..10_000u32 {
            let row = probe * 100;
            let at = axis.index_at(axis.offset(row) + 1.0);
            assert_eq!(at, row);
        }
        assert!(start.elapsed().as_millis() < 2_000);
    }

    #[test]
    fn unsorted_runs_fall_back_to_one_at_a_time() {
        let mut axis = Axis::new(10.0, 10);
        axis.set_sorted([(5, 6, 1.0), (2, 3, 2.0), (2, 6, 3.0)]);
        assert_eq!(axis.size(2), 3.0);
        assert_eq!(axis.size(5), 3.0);
        assert_eq!(axis.size(6), 10.0);
    }
}
