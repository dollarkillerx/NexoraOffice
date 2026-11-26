//! Writing tables back: which of the original bytes stay and what is
//! written from the model.
//!
//! A table that is where it was keeps its bytes as far as it is as read.
//! When rows or columns change, the table's start is kept (its `tblPr` with
//! the width and indent following the grid), the grid is written again and
//! every row that changed is written from the model **over its original
//! bytes**: the row keeps its start tag and the `trPr` children the model
//! has no field for, a cell keeps its start tag and the `tcPr` children the
//! model has no field for (`cnfStyle`, `noWrap`, `textDirection`,
//! `tcFitText`, `hideMark` and the like), and the content of a cell that is
//! as read is its own bytes. Only what the model holds (width, span, merge,
//! borders, shading, margins, vertical alignment; for a row the grid
//! columns it skips, its height, whether it splits or repeats) is written
//! again, and only where it differs from what was read.

use super::*;
use crate::xml_edit::edit_children;

impl<'a> Saver<'a> {
    // -- tables -----------------------------------------------------------

    /// A table that is where it was. What is kept of the original bytes
    /// depends on what changed:
    ///
    /// - only the content of cells: the bytes of the table, with the content
    ///   of the cells that changed written again;
    /// - rows added or removed, columns as they were: the table's start
    ///   (`tblPr`, `tblGrid`) and every row that is as read are copied, the
    ///   rows made here are written from the model;
    /// - columns added or removed: the table's `tblPr` is kept (its width
    ///   follows the grid where it was the grid's sum), the grid and every
    ///   row are written from the model, with each cell's content as for
    ///   any block (paragraphs that are as read are copied).
    pub(super) fn table_in_place(&mut self, model: &Table, range: &Range<usize>) -> String {
        let Some(ot) = self.tables.get(&range_key(range)).copied() else {
            return self.table_from_model(model);
        };
        if model.props != ot.props
            && (aster_writer::TableProps {
                indent: ot.props.indent,
                ..model.props.clone()
            }) != ot.props
        {
            return self.table_from_model(model);
        }
        if same_structure(model, ot) {
            let mut replacements: Vec<(Range<usize>, String)> = Vec::new();
            for (row, orow) in model.rows.iter().zip(&ot.rows) {
                match self.cell_replacements(row, orow) {
                    Some(found) => replacements.extend(found),
                    None => return self.table_from_model(model),
                }
            }
            return self.apply(range, replacements);
        }
        self.table_rewritten(model, ot, range)
    }

    /// The original bytes of `range` with the replacements put in.
    fn apply(&self, range: &Range<usize>, replacements: Vec<(Range<usize>, String)>) -> String {
        let mut out = String::new();
        let mut at = range.start;
        for (region, content) in replacements {
            out.push_str(&self.text[at..region.start]);
            out.push_str(&content);
            at = region.end;
        }
        out.push_str(&self.text[at..range.end]);
        out
    }

    /// The content of the cells of `row` that changed, written again, with
    /// the part of the original bytes it replaces. `None` if the bytes do
    /// not fit the model.
    fn cell_replacements(
        &mut self,
        row: &aster_writer::Row,
        orow: &aster_writer::Row,
    ) -> Option<Vec<(Range<usize>, String)>> {
        let mut out = Vec::new();
        for (cell, ocell) in row.cells.iter().zip(&orow.cells) {
            if cell_unchanged(cell, ocell) {
                continue;
            }
            let crange = ocell.source.as_ref()?;
            let element = self.slice(crange)?;
            let (open, close) = inner(element);
            let region = crange.start + open..crange.start + close;
            self.in_cell += 1;
            let content = self.container(region.clone(), &ocell.blocks, &cell.blocks);
            self.in_cell -= 1;
            out.push((region, content));
        }
        Some(out)
    }

    /// A table whose rows or columns changed, over the original's bytes.
    fn table_rewritten(&mut self, model: &Table, ot: &Table, range: &Range<usize>) -> String {
        let Some(element) = self.slice(range) else {
            return self.table_from_model(model);
        };
        let kids = children(element);
        let Some(grid) = kids.iter().find(|c| c.name == "tblGrid") else {
            return self.table_from_model(model);
        };
        let (_, close) = inner(element);
        if grid.end > close {
            return self.table_from_model(model);
        }
        let region = range.start + grid.end..range.start + close;
        let columns_changed = model.columns != ot.columns;

        // The start: kept, with the table's width following the grid.
        let mut out = String::new();
        match kids.iter().find(|c| c.name == "tblPr") {
            Some(pr) if pr.end <= grid.start => {
                out.push_str(&element[..pr.start]);
                out.push_str(&self.patch_table_pr(
                    &element[pr.start..pr.end],
                    (ot.columns.iter().sum(), model.columns.iter().sum()),
                    (ot.props.indent, model.props.indent),
                ));
                out.push_str(&element[pr.end..grid.start]);
            }
            _ => out.push_str(&element[..grid.start]),
        }
        if columns_changed {
            out.push_str(&self.grid_xml(&model.columns));
        } else {
            out.push_str(&element[grid.start..grid.end]);
        }

        // The rows of the original, in order and inside the region.
        let mut ranges: Vec<(Range<usize>, usize)> = Vec::new();
        for (i, row) in ot.rows.iter().enumerate() {
            if let Some(r) = &row.source
                && r.start >= ranges.last().map_or(region.start, |(r, _)| r.end)
                && r.end <= region.end
                && r.start < r.end
            {
                ranges.push((r.clone(), i));
            }
        }
        let index: HashMap<(usize, usize), usize> = ranges
            .iter()
            .enumerate()
            .map(|(i, (r, _))| (range_key(r), i))
            .collect();
        let n = ranges.len();
        let gap = |g: usize| -> Range<usize> {
            let start = if g == 0 {
                region.start
            } else {
                ranges[g - 1].0.end
            };
            let end = if g == n {
                region.end
            } else {
                ranges[g].0.start
            };
            start..end
        };
        out.push_str(&self.text[gap(0)]);
        let mut next_gap = 1;
        let mut last: isize = -1;
        for row in &model.rows {
            let matched = row
                .source
                .as_ref()
                .and_then(|r| index.get(&range_key(r)).copied())
                .filter(|&i| i as isize > last);
            let Some(i) = matched else {
                out.push_str(&self.row_xml(&model.columns, row, columns_changed));
                continue;
            };
            for g in next_gap..=i {
                out.push_str(&self.text[gap(g)]);
            }
            next_gap = next_gap.max(i + 1);
            last = i as isize;
            let (rrange, oi) = (&ranges[i].0, ranges[i].1);
            let orow = &ot.rows[oi];
            let text = if !columns_changed && same_row(row, orow) {
                match self.cell_replacements(row, orow) {
                    Some(found) => self.apply(rrange, found),
                    None => self.row_xml(&model.columns, row, columns_changed),
                }
            } else {
                self.row_xml(&model.columns, row, columns_changed)
            };
            out.push_str(&text);
        }
        for g in next_gap..=n {
            out.push_str(&self.text[gap(g)]);
        }
        out.push_str(&element[close..]);
        out
    }

    /// `tblPr` following the grid: a width in twips follows the sum of the
    /// columns (it was the sum of the old ones, or it moves by the same
    /// amount), and the indent is the new one. A width in another unit
    /// (percent, automatic) is left.
    fn patch_table_pr(&self, pr: &str, widths: (i32, i32), indents: (i32, i32)) -> String {
        let mut pr = pr.to_owned();
        let (old, new) = widths;
        if old != new
            && let Some(width) = children(&pr).into_iter().find(|c| c.name == "tblW")
        {
            let element = &pr[width.start..width.end];
            let is_dxa =
                attribute(element, &format!("{}type", self.w)).is_none_or(|kind| kind == "dxa");
            let value = format!("{}w", self.w);
            let then = attribute(element, &value).and_then(|v| v.trim().parse::<i32>().ok());
            if is_dxa && let Some(then) = then {
                let now = if then == old {
                    new
                } else {
                    (then + (new - old)).max(0)
                };
                pr = format!(
                    "{}{}{}",
                    &pr[..width.start],
                    set_attribute(element, &value, &now.to_string()),
                    &pr[width.end..]
                );
            }
        }
        if indents.0 != indents.1 {
            let w = &self.w;
            let element = format!("<{w}tblInd {w}w=\"{}\" {w}type=\"dxa\"/>", indents.1);
            pr = edit_children(
                &pr,
                &["tblInd"],
                &element,
                &[
                    "tblBorders",
                    "shd",
                    "tblLayout",
                    "tblCellMar",
                    "tblLook",
                    "tblCaption",
                    "tblDescription",
                    "tblPrChange",
                ],
            );
        }
        pr
    }

    fn grid_xml(&self, columns: &[i32]) -> String {
        let w = &self.w;
        let mut out = format!("<{w}tblGrid>");
        for column in columns {
            out.push_str(&format!("<{w}gridCol {w}w=\"{column}\"/>"));
        }
        out.push_str(&format!("</{w}tblGrid>"));
        out
    }

    /// A table written from the model: grid, rows, cells, spans, merges,
    /// borders and shading.
    pub(super) fn table_from_model(&mut self, table: &Table) -> String {
        let w = self.w.clone();
        let mut out = format!("<{w}tbl><{w}tblPr>");
        if let Some(style) = &table.props.style {
            out.push_str(&format!(
                "<{w}tblStyle {w}val=\"{}\"/>",
                escape_attribute(style)
            ));
        }
        let total: i32 = table.columns.iter().sum();
        out.push_str(&format!("<{w}tblW {w}w=\"{total}\" {w}type=\"dxa\"/>"));
        match table.props.align {
            Align::Center => out.push_str(&format!("<{w}jc {w}val=\"center\"/>")),
            Align::Right => out.push_str(&format!("<{w}jc {w}val=\"right\"/>")),
            _ => {}
        }
        out.push_str(&format!(
            "<{w}tblInd {w}w=\"{}\" {w}type=\"dxa\"/>",
            table.props.indent
        ));
        // Borders that every cell has alike are the table's too.
        if let Some(sides) = common_borders(table) {
            let mut borders = borders_element(
                &w,
                "tblBorders",
                &sides,
                None,
                ["left", "top", "right", "bottom"],
            );
            let inside = format!(
                "{}{}</{w}tblBorders>",
                border_element(&w, "insideH", sides[1].as_ref()),
                border_element(&w, "insideV", sides[0].as_ref())
            );
            borders = borders.replace(&format!("</{w}tblBorders>"), &inside);
            out.push_str(&borders);
        }
        if table.props.fixed_layout {
            out.push_str(&format!("<{w}tblLayout {w}type=\"fixed\"/>"));
        }
        let [left, top, right, bottom] = table.props.cell_margins;
        out.push_str(&format!(
            "<{w}tblCellMar><{w}top {w}w=\"{top}\" {w}type=\"dxa\"/><{w}left {w}w=\"{left}\" {w}type=\"dxa\"/><{w}bottom {w}w=\"{bottom}\" {w}type=\"dxa\"/><{w}right {w}w=\"{right}\" {w}type=\"dxa\"/></{w}tblCellMar>"
        ));
        // What Word asks of a table style by default.
        out.push_str(&format!(
            "<{w}tblLook {w}val=\"04A0\" {w}firstRow=\"1\" {w}lastRow=\"0\" {w}firstColumn=\"1\" {w}lastColumn=\"0\" {w}noHBand=\"0\" {w}noVBand=\"1\"/>"
        ));
        out.push_str(&format!("</{w}tblPr>"));
        out.push_str(&self.grid_xml(&table.columns));
        for row in &table.rows {
            out.push_str(&self.row_xml(&table.columns, row, true));
        }
        out.push_str(&format!("</{w}tbl>"));
        out
    }

    /// What the model holds of a row's properties, as `trPr` children: the
    /// grid columns it skips (with their widths), whether it splits, its
    /// height and whether it repeats.
    fn row_pr_items(&self, row: &Row, columns: &[i32]) -> String {
        let w = &self.w;
        let mut pr = String::new();
        let before = row.grid_before as usize;
        let after = row.grid_after as usize;
        if before > 0 {
            pr.push_str(&format!("<{w}gridBefore {w}val=\"{before}\"/>"));
            let width: i32 = columns.iter().take(before).sum();
            pr.push_str(&format!("<{w}wBefore {w}w=\"{width}\" {w}type=\"dxa\"/>"));
        }
        if after > 0 {
            pr.push_str(&format!("<{w}gridAfter {w}val=\"{after}\"/>"));
            let skip = columns.len().saturating_sub(after);
            let width: i32 = columns.iter().skip(skip).sum();
            pr.push_str(&format!("<{w}wAfter {w}w=\"{width}\" {w}type=\"dxa\"/>"));
        }
        if row.cant_split {
            pr.push_str(&format!("<{w}cantSplit/>"));
        }
        match row.height {
            RowHeight::Auto => {}
            RowHeight::AtLeast(h) => pr.push_str(&format!(
                "<{w}trHeight {w}val=\"{h}\" {w}hRule=\"atLeast\"/>"
            )),
            RowHeight::Exactly(h) => {
                pr.push_str(&format!("<{w}trHeight {w}val=\"{h}\" {w}hRule=\"exact\"/>"))
            }
        }
        if row.header {
            pr.push_str(&format!("<{w}tblHeader/>"));
        }
        pr
    }

    /// The `trPr` of a row made here.
    fn row_pr(&self, row: &Row, columns: &[i32]) -> String {
        let items = self.row_pr_items(row, columns);
        if items.is_empty() {
            items
        } else {
            let w = &self.w;
            format!("<{w}trPr>{items}</{w}trPr>")
        }
    }

    /// The start tag of a row with its `trPr` and whatever else comes
    /// before its cells, and its end tag. For a row read from the file: its
    /// own start tag, `tblPrEx` and `trPr`, the `trPr` unchanged where the
    /// model's properties are, and otherwise with the children the model
    /// holds written again and the others kept.
    fn row_frame(
        &self,
        row: &Row,
        orow: Option<&Row>,
        columns: &[i32],
        columns_changed: bool,
    ) -> (String, String) {
        let w = &self.w;
        let fresh = || {
            (
                format!("<{w}tr>{}", self.row_pr(row, columns)),
                format!("</{w}tr>"),
            )
        };
        let Some(orow) = orow else {
            return fresh();
        };
        let Some(element) = orow.source.as_ref().and_then(|r| self.slice(r)) else {
            return fresh();
        };
        if !is_element(element, "tr") {
            return fresh();
        }
        let (open, close) = inner(element);
        if element[..open].ends_with("/>") {
            return fresh();
        }
        let kids = children(element);
        // Everything before the first child that is not a row property.
        let first = kids
            .iter()
            .find(|c| c.name != "trPr" && c.name != "tblPrEx")
            .map_or(close, |c| c.start);
        let end = format!("</{}>", qualified_name(element));
        let pr = kids.iter().find(|c| c.name == "trPr" && c.end <= first);
        let skips = row.grid_before > 0 || row.grid_after > 0;
        if row_props_equal(row, orow) && !(columns_changed && skips) {
            return (element[..first].to_owned(), end);
        }
        let items = self.row_pr_items(row, columns);
        match pr {
            Some(pr) => {
                let old = &element[pr.start..pr.end];
                let (popen, _) = inner(old);
                let kept: String = children(old)
                    .iter()
                    .filter(|c| {
                        !matches!(
                            c.name.as_str(),
                            "gridBefore"
                                | "gridAfter"
                                | "wBefore"
                                | "wAfter"
                                | "cantSplit"
                                | "trHeight"
                                | "tblHeader"
                        )
                    })
                    .map(|c| &old[c.start..c.end])
                    .collect();
                let name = qualified_name(old);
                let new = if items.is_empty() && kept.is_empty() {
                    String::new()
                } else if old[..popen].ends_with("/>") {
                    format!("<{name}>{items}{kept}</{name}>")
                } else {
                    format!("{}{items}{kept}</{name}>", &old[..popen])
                };
                (
                    format!("{}{new}{}", &element[..pr.start], &element[pr.end..first]),
                    end,
                )
            }
            None => {
                let new = if items.is_empty() {
                    String::new()
                } else {
                    format!("<{w}trPr>{items}</{w}trPr>")
                };
                (format!("{}{new}", &element[..first]), end)
            }
        }
    }

    /// A row from the model: its cells with their widths from `columns`,
    /// spans, merges, borders, shading, margins and alignment, and their
    /// content as for any block. A row that was read keeps its start tag
    /// and the properties the model does not hold, and its cells theirs
    /// (see the module documentation).
    fn row_xml(&mut self, columns: &[i32], row: &Row, columns_changed: bool) -> String {
        let orow = row
            .source
            .as_ref()
            .and_then(|r| self.rows.get(&range_key(r)).copied())
            .filter(|o| o.source == row.source);
        let (start, end) = self.row_frame(row, orow, columns, columns_changed);
        let mut out = start;
        let mut column = row.grid_before as usize;
        for cell in &row.cells {
            let span = cell.span.max(1) as usize;
            let width: i32 = columns.iter().skip(column).take(span).sum();
            column += span;
            out.push_str(&self.cell_xml(cell, width, columns_changed));
        }
        out.push_str(&end);
        out
    }

    /// A cell from the model, `width` twips wide.
    fn cell_xml(&mut self, cell: &aster_writer::Cell, width: i32, columns_changed: bool) -> String {
        let w = self.w.clone();
        let ocell = cell
            .source
            .as_ref()
            .and_then(|r| self.cells.get(&range_key(r)).copied())
            .filter(|o| o.source == cell.source);
        let element = ocell
            .and_then(|o| o.source.as_ref())
            .and_then(|r| self.slice(r))
            .filter(|e| is_element(e, "tc") && !e[..inner(e).0].ends_with("/>"));
        let (Some(ocell), Some(element)) = (ocell, element) else {
            let mut out = format!("<{w}tc>");
            out.push_str(&self.tc_pr(cell, None, width, columns_changed));
            self.in_cell += 1;
            for block in &cell.blocks {
                out.push_str(&self.block_elsewhere(block));
            }
            self.in_cell -= 1;
            if cell.blocks.last().is_none_or(|b| b.as_table().is_some()) {
                out.push_str(&format!("<{w}p/>"));
            }
            out.push_str(&format!("</{w}tc>"));
            return out;
        };
        // The cell as it was: its start tag, its properties patched, and
        // its content (the bytes after the properties) as it was or
        // written again.
        let range = ocell.source.clone().expect("a cell with a source");
        let (open, close) = inner(element);
        let kids = children(element);
        let pr = kids
            .iter()
            .find(|c| c.name == "tcPr" && element[open..c.start].trim().is_empty());
        let content_from = pr.map_or(open, |pr| pr.end);
        let old_pr = pr.map(|pr| &element[pr.start..pr.end]);
        let new_pr = self.tc_pr(cell, Some((ocell, old_pr)), width, columns_changed);
        let content = if cell_unchanged(cell, ocell) {
            element[content_from..close].to_owned()
        } else {
            let region = range.start + content_from..range.start + close;
            self.in_cell += 1;
            let content = self.container(region, &ocell.blocks, &cell.blocks);
            self.in_cell -= 1;
            if cell.blocks.last().is_none_or(|b| b.as_table().is_some()) {
                format!("{content}<{w}p/>")
            } else {
                content
            }
        };
        format!(
            "{}{new_pr}{content}</{}>",
            &element[..open],
            qualified_name(element)
        )
    }

    /// The `tcPr` of a cell. From the model alone for a cell made here.
    /// For a cell read from the file, `orig` is the cell as read and its
    /// `tcPr` (if it had one): the children the model has no field for stay,
    /// and so do the modeled ones that are as they were, byte for byte; the
    /// ones that differ are written again (or taken out where the model has
    /// nothing).
    fn tc_pr(
        &self,
        cell: &aster_writer::Cell,
        orig: Option<(&aster_writer::Cell, Option<&str>)>,
        width: i32,
        columns_changed: bool,
    ) -> String {
        let w = &self.w;
        let span = cell.span.max(1);
        let tcw = format!("<{w}tcW {w}w=\"{width}\" {w}type=\"dxa\"/>");
        let grid_span = if span > 1 {
            format!("<{w}gridSpan {w}val=\"{span}\"/>")
        } else {
            String::new()
        };
        let v_merge = match cell.merge {
            VMerge::None => String::new(),
            VMerge::Start => format!("<{w}vMerge {w}val=\"restart\"/>"),
            VMerge::Continue => format!("<{w}vMerge/>"),
        };
        let borders = if cell.borders.iter().any(Option::is_some) {
            borders_element(
                w,
                "tcBorders",
                &cell.borders,
                None,
                ["left", "top", "right", "bottom"],
            )
        } else {
            String::new()
        };
        let shading = cell.shading.map_or(String::new(), |fill| {
            format!(
                "<{w}shd {w}val=\"clear\" {w}color=\"auto\" {w}fill=\"{}\"/>",
                hex(fill)
            )
        });
        let [l, t, r, b] = cell.margins;
        let margins = format!(
            "<{w}tcMar><{w}top {w}w=\"{t}\" {w}type=\"dxa\"/><{w}left {w}w=\"{l}\" {w}type=\"dxa\"/><{w}bottom {w}w=\"{b}\" {w}type=\"dxa\"/><{w}right {w}w=\"{r}\" {w}type=\"dxa\"/></{w}tcMar>"
        );
        let v_align = match cell.align {
            CellAlign::Top => String::new(),
            CellAlign::Center => format!("<{w}vAlign {w}val=\"center\"/>"),
            CellAlign::Bottom => format!("<{w}vAlign {w}val=\"bottom\"/>"),
        };
        let Some((ocell, old)) = orig else {
            return format!(
                "<{w}tcPr>{tcw}{grid_span}{v_merge}{borders}{shading}{margins}{v_align}</{w}tcPr>"
            );
        };

        let mut pr = old.map_or_else(|| format!("<{w}tcPr></{w}tcPr>"), str::to_owned);
        // The width: as it was where it is the width now; written where the
        // file had none only if the grid changed.
        let kept_width = children(&pr)
            .iter()
            .find(|c| c.name == "tcW")
            .map(|c| &pr[c.start..c.end])
            .map(|e| {
                attribute(e, &format!("{w}type")).is_none_or(|t| t == "dxa")
                    && attribute(e, &format!("{w}w")).as_deref() == Some(width.to_string().as_str())
            });
        match kept_width {
            Some(true) => {}
            Some(false) => pr = put_tc(&pr, "tcW", &tcw),
            None if columns_changed => pr = put_tc(&pr, "tcW", &tcw),
            None => {}
        }
        if span != ocell.span.max(1) {
            pr = put_tc(&pr, "gridSpan", &grid_span);
        }
        if cell.merge != ocell.merge {
            pr = put_tc(&pr, "vMerge", &v_merge);
        }
        if cell.borders != ocell.borders {
            pr = put_tc(&pr, "tcBorders", &borders);
        }
        if cell.shading != ocell.shading {
            pr = put_tc(&pr, "shd", &shading);
        }
        if cell.margins != ocell.margins {
            pr = put_tc(&pr, "tcMar", &margins);
        }
        if cell.align != ocell.align {
            pr = put_tc(&pr, "vAlign", &v_align);
        }
        if old.is_none() && children(&pr).is_empty() {
            return String::new();
        }
        pr
    }
}

/// The order of `tcPr`'s children in the schema.
const TCPR_ORDER: [&str; 17] = [
    "cnfStyle",
    "tcW",
    "gridSpan",
    "hMerge",
    "vMerge",
    "tcBorders",
    "shd",
    "noWrap",
    "tcMar",
    "textDirection",
    "tcFitText",
    "vAlign",
    "hideMark",
    "headers",
    "cellIns",
    "cellDel",
    "cellMerge",
];

/// `element` (empty to take the child out) as the child `name` of the
/// `tcPr` `pr`, at the place the schema gives it; the other children are
/// untouched.
fn put_tc(pr: &str, name: &str, element: &str) -> String {
    let at = TCPR_ORDER.iter().position(|n| *n == name).unwrap_or(0);
    let mut after: Vec<&str> = TCPR_ORDER[at + 1..].to_vec();
    after.push("tcPrChange");
    edit_children(pr, &[name], element, &after)
}
