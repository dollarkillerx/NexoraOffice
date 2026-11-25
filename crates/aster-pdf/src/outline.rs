//! A document's outline (bookmarks), with destinations resolved to pages.
//!
//! The outline is a tree of dictionaries linked by `/First` and `/Next`. It is
//! walked without recursion, in document order, and every link is checked
//! against the items already seen, so a file whose links run in a circle ends
//! the walk instead of looping. Only destinations inside this document are
//! kept: an item's action is followed when it is `GoTo`; every other action
//! (a URI, a launched file, a script, another file) gives an item without a
//! target. A title is only ever shown as text.

use std::collections::{HashMap, HashSet};
use std::panic::{AssertUnwindSafe, catch_unwind};

use hayro::hayro_interpret::TransformExt;
use hayro::hayro_interpret::hayro_syntax::object::{
    Array, Dict, MaybeRef, Name, Object, ObjectIdentifier, String as PdfString,
};

use crate::{Document, OutlineItem, Target};

/// Work a document may cost in name-tree lookups, over all its items.
const NAME_TREE_BUDGET: usize = 2_000_000;
/// What visiting one name-tree node costs against that budget.
const NODE_COST: usize = 16;

impl Document {
    /// The outline, flattened in document order. Empty when there is none.
    pub fn outline(&self) -> Vec<OutlineItem> {
        catch_unwind(AssertUnwindSafe(|| self.read_outline())).unwrap_or_default()
    }

    fn read_outline(&self) -> Vec<OutlineItem> {
        let xref = self.pdf.xref();
        let Some(catalog) = xref.get::<Dict<'_>>(xref.root_id()) else {
            return Vec::new();
        };
        let Some(root) = catalog.get::<Dict<'_>>(b"Outlines") else {
            return Vec::new();
        };
        let pages = self.pdf.pages();
        let page_index: HashMap<ObjectIdentifier, usize> = pages
            .iter()
            .enumerate()
            .filter_map(|(index, page)| Some((page.raw().obj_id()?, index)))
            .collect();
        let mut resolver = Resolver {
            document: self,
            catalog,
            page_index,
            budget: NAME_TREE_BUDGET,
        };

        let max_items = self.limits.max_outline_items;
        let max_depth = self.limits.max_outline_depth;
        // Items looked at, skipped ones too, so that a long chain of items
        // too deep to show cannot go on without end.
        let max_visited = max_items.saturating_mul(4).saturating_add(64);
        let mut items = Vec::new();
        let mut visited: HashSet<ObjectIdentifier> = HashSet::new();
        let mut looked_at = 0_usize;
        // The next sibling of each ancestor, and its depth.
        let mut pending: Vec<(Option<Dict<'_>>, usize)> = Vec::new();
        let mut cursor = root.get::<Dict<'_>>(b"First");
        let mut depth = 0_usize;
        while items.len() < max_items && looked_at < max_visited {
            let Some(item) = cursor.take() else {
                let Some((next, parent_depth)) = pending.pop() else {
                    break;
                };
                cursor = next;
                depth = parent_depth;
                continue;
            };
            looked_at += 1;
            if let Some(id) = item.obj_id()
                && !visited.insert(id)
            {
                // A link back to an item seen: this chain ends here.
                continue;
            }
            let next = item.get::<Dict<'_>>(b"Next");
            if depth >= max_depth {
                cursor = next;
                continue;
            }
            items.push(OutlineItem {
                depth: depth as u16,
                title: self.title(&item),
                target: resolver.target(&item),
                open: item.get::<i32>(b"Count").is_some_and(|count| count > 0),
            });
            if let Some(first) = item.get::<Dict<'_>>(b"First") {
                pending.push((next, depth));
                cursor = Some(first);
                depth += 1;
            } else {
                cursor = next;
            }
        }
        items
    }

    fn title(&self, item: &Dict<'_>) -> String {
        item.get::<PdfString<'_>>(b"Title")
            .map(|title| decode_text(title.as_bytes(), self.limits.max_title_chars))
            .unwrap_or_default()
    }
}

/// Turns destinations into targets.
struct Resolver<'a, 'd> {
    document: &'d Document,
    catalog: Dict<'a>,
    page_index: HashMap<ObjectIdentifier, usize>,
    /// Name-tree work left.
    budget: usize,
}

impl<'a> Resolver<'a, '_> {
    fn target(&mut self, item: &Dict<'a>) -> Option<Target> {
        if let Some(destination) = item.get::<Object<'a>>(b"Dest") {
            return self.destination(destination);
        }
        let action = item.get::<Dict<'a>>(b"A")?;
        if action.get::<Name<'_>>(b"S")?.as_ref() != b"GoTo" {
            return None;
        }
        self.destination(action.get::<Object<'a>>(b"D")?)
    }

    /// An explicit destination, or a name to look up.
    fn destination(&mut self, object: Object<'a>) -> Option<Target> {
        match object {
            Object::Array(array) => self.explicit(&array),
            Object::Name(name) => {
                let value = self.named(name.as_ref())?;
                self.named_value(value)
            }
            Object::String(name) => {
                let value = self.named(name.as_bytes())?;
                self.named_value(value)
            }
            Object::Dict(dict) => self.explicit(&dict.get::<Array<'a>>(b"D")?),
            _ => None,
        }
    }

    /// What a name stands for: an array, or a dictionary with `/D`.
    fn named_value(&mut self, value: Object<'a>) -> Option<Target> {
        match value {
            Object::Array(array) => self.explicit(&array),
            Object::Dict(dict) => self.explicit(&dict.get::<Array<'a>>(b"D")?),
            _ => None,
        }
    }

    fn explicit(&mut self, array: &Array<'a>) -> Option<Target> {
        let mut raw = array.raw_iter().take(8);
        let page = match raw.next()? {
            MaybeRef::Ref(reference) => *self.page_index.get(&reference.into())?,
            MaybeRef::NotRef(Object::Number(number)) => {
                let index = usize::try_from(number.as_i64()).ok()?;
                (index < self.document.info.pages.len()).then_some(index)?
            }
            MaybeRef::NotRef(_) => return None,
        };
        let rest: Vec<MaybeRef<Object<'a>>> = raw.collect();
        let number = |index: usize| -> Option<f64> {
            match rest.get(index)? {
                MaybeRef::NotRef(Object::Number(number)) => Some(number.as_f64()),
                MaybeRef::Ref(reference) => self
                    .document
                    .pdf
                    .xref()
                    .get::<Object<'_>>((*reference).into())
                    .and_then(|o| o.into_number())
                    .map(|number| number.as_f64()),
                _ => None,
            }
        };
        let kind: &[u8] = match rest.first() {
            Some(MaybeRef::NotRef(Object::Name(name))) => name.as_ref(),
            _ => b"",
        };
        let (left, top) = match kind {
            // [page /XYZ left top zoom]
            b"XYZ" => (number(1), number(2)),
            // [page /FitH top] and [page /FitBH top]
            b"FitH" | b"FitBH" => (None, number(1)),
            // [page /FitR left bottom right top]
            b"FitR" => (number(1), number(4)),
            _ => (None, None),
        };
        let top = top.and_then(|top| self.shown_top(page, left, top));
        Some(Target { page, top })
    }

    /// The distance from the top of the page as shown to a point of the
    /// page's user space: through the same transform the page is drawn with,
    /// so a rotated page counts from its shown top. Without `left` the
    /// page's left edge is used.
    fn shown_top(&self, page: usize, left: Option<f64>, top: f64) -> Option<f32> {
        let page = self.document.pdf.pages().get(page)?;
        let transform = page.initial_transform(true).to_kurbo();
        let x = left.unwrap_or(page.intersected_crop_box().x0);
        if !(x.is_finite() && top.is_finite()) {
            return None;
        }
        let shown = transform * hayro::hayro_interpret::kurbo::Point::new(x, top);
        let (_, height) = page.render_dimensions();
        shown
            .y
            .is_finite()
            .then(|| shown.y.clamp(0.0, f64::from(height)) as f32)
    }

    /// A named destination: the catalog's `/Dests` dictionary, then its
    /// `/Names /Dests` name tree.
    fn named(&mut self, name: &[u8]) -> Option<Object<'a>> {
        if let Some(dests) = self.catalog.get::<Dict<'a>>(b"Dests")
            && let Some(value) = dests.get::<Object<'a>>(name)
        {
            return Some(value);
        }
        let root = self
            .catalog
            .get::<Dict<'a>>(b"Names")?
            .get::<Dict<'a>>(b"Dests")?;
        self.name_tree(root, name)
    }

    fn name_tree(&mut self, root: Dict<'a>, key: &[u8]) -> Option<Object<'a>> {
        let mut stack = vec![root];
        let mut seen: HashSet<ObjectIdentifier> = HashSet::new();
        while let Some(node) = stack.pop() {
            if self.budget < NODE_COST {
                return None;
            }
            self.budget -= NODE_COST;
            if let Some(id) = node.obj_id()
                && !seen.insert(id)
            {
                continue;
            }
            if let Some(names) = node.get::<Array<'a>>(b"Names") {
                let mut pairs = names.flex_iter();
                while self.budget > 0 {
                    self.budget -= 1;
                    let Some(candidate) = pairs.next::<PdfString<'a>>() else {
                        break;
                    };
                    let value = pairs.next::<Object<'a>>();
                    if candidate.as_bytes() == key {
                        return value;
                    }
                }
            } else if let Some(kids) = node.get::<Array<'a>>(b"Kids") {
                let mut children = Vec::new();
                for kid in kids.iter::<Dict<'a>>() {
                    if self.budget == 0 {
                        return None;
                    }
                    self.budget -= 1;
                    if let Some(limits) = kid.get::<Array<'a>>(b"Limits") {
                        let mut ends = limits.iter::<PdfString<'a>>();
                        if let (Some(low), Some(high)) = (ends.next(), ends.next())
                            && (key < low.as_bytes() || key > high.as_bytes())
                        {
                            continue;
                        }
                    }
                    children.push(kid);
                }
                // First kid first.
                stack.extend(children.into_iter().rev());
            }
        }
        None
    }
}

/// A PDF text string as text: UTF-16 with a byte order mark, UTF-8 with its
/// mark, or PDFDocEncoding. Control characters are dropped and at most `max`
/// characters kept.
fn decode_text(bytes: &[u8], max: usize) -> String {
    let keep = |c: &char| !c.is_control();
    if let Some(rest) = bytes.strip_prefix(&[0xFE, 0xFF]) {
        let units = rest
            .as_chunks::<2>()
            .0
            .iter()
            .map(|pair| u16::from_be_bytes(*pair));
        char::decode_utf16(units)
            .map(|r| r.unwrap_or('\u{FFFD}'))
            .filter(keep)
            .take(max)
            .collect()
    } else if let Some(rest) = bytes.strip_prefix(&[0xFF, 0xFE]) {
        let units = rest
            .as_chunks::<2>()
            .0
            .iter()
            .map(|pair| u16::from_le_bytes(*pair));
        char::decode_utf16(units)
            .map(|r| r.unwrap_or('\u{FFFD}'))
            .filter(keep)
            .take(max)
            .collect()
    } else if let Some(rest) = bytes.strip_prefix(&[0xEF, 0xBB, 0xBF]) {
        String::from_utf8_lossy(rest)
            .chars()
            .filter(keep)
            .take(max)
            .collect()
    } else {
        bytes
            .iter()
            .filter_map(|byte| pdf_doc_char(*byte))
            .filter(keep)
            .take(max)
            .collect()
    }
}

/// PDFDocEncoding: Latin-1 except for these.
fn pdf_doc_char(byte: u8) -> Option<char> {
    const LOW: [char; 8] = [
        '\u{2D8}', '\u{2C7}', '\u{2C6}', '\u{2D9}', '\u{2DD}', '\u{2DB}', '\u{2DA}', '\u{2DC}',
    ];
    const HIGH: [char; 32] = [
        '\u{2022}', '\u{2020}', '\u{2021}', '\u{2026}', '\u{2014}', '\u{2013}', '\u{192}',
        '\u{2044}', '\u{2039}', '\u{203A}', '\u{2212}', '\u{2030}', '\u{201E}', '\u{201C}',
        '\u{201D}', '\u{2018}', '\u{2019}', '\u{201A}', '\u{2122}', '\u{FB01}', '\u{FB02}',
        '\u{141}', '\u{152}', '\u{160}', '\u{178}', '\u{17D}', '\u{131}', '\u{142}', '\u{153}',
        '\u{161}', '\u{17E}', '\u{0}',
    ];
    match byte {
        0x18..=0x1F => Some(LOW[usize::from(byte - 0x18)]),
        0x80..=0x9F => Some(HIGH[usize::from(byte - 0x80)]),
        0xA0 => Some('\u{20AC}'),
        _ => Some(char::from(byte)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Limits;
    use crate::builder::Builder;
    use crate::text::testing::with_document;

    fn hex(bytes: &[u8]) -> String {
        let digits: String = bytes.iter().map(|b| format!("{b:02X}")).collect();
        format!("<{digits}>")
    }

    fn utf16(text: &str) -> String {
        let mut bytes = vec![0xFE, 0xFF];
        for unit in text.encode_utf16() {
            bytes.extend_from_slice(&unit.to_be_bytes());
        }
        hex(&bytes)
    }

    /// A document of two pages (200 x 100; the first has `page_extra`).
    /// `build` gets the builder and the objects of both pages and returns
    /// what to add to the catalog.
    fn outline_pdf(
        page_extra: &str,
        build: impl FnOnce(&mut Builder, usize, usize) -> String,
    ) -> Vec<u8> {
        let mut b = Builder::default();
        let catalog = b.reserve();
        let pages = b.reserve();
        let mut kids = Vec::new();
        for extra in [page_extra, ""] {
            let content = b.stream("", b"0 g 0 0 10 10 re f");
            kids.push(b.add(format!(
                "<< /Type /Page /Parent {pages} 0 R /MediaBox [0 0 200 100] {extra} \
                 /Contents {content} 0 R >>"
            )));
        }
        let more = build(&mut b, kids[0], kids[1]);
        b.set(
            catalog,
            format!("<< /Type /Catalog /Pages {pages} 0 R {more} >>"),
        );
        b.set(
            pages,
            format!(
                "<< /Type /Pages /Kids [{} 0 R {} 0 R] /Count 2 >>",
                kids[0], kids[1]
            ),
        );
        b.finish(catalog)
    }

    /// Items given as (title string, extra entries), as siblings of one
    /// level under an outline root. Returns the catalog entry.
    fn flat(b: &mut Builder, items: &[(String, String)]) -> String {
        let root = b.reserve();
        let numbers: Vec<usize> = items.iter().map(|_| b.reserve()).collect();
        for (index, (title, extra)) in items.iter().enumerate() {
            let next = numbers
                .get(index + 1)
                .map(|n| format!("/Next {n} 0 R"))
                .unwrap_or_default();
            let prev = index
                .checked_sub(1)
                .map(|i| format!("/Prev {} 0 R", numbers[i]))
                .unwrap_or_default();
            b.set(
                numbers[index],
                format!("<< /Title {title} /Parent {root} 0 R {next} {prev} {extra} >>"),
            );
        }
        b.set(
            root,
            format!(
                "<< /Type /Outlines /First {} 0 R /Last {} 0 R /Count {} >>",
                numbers[0],
                numbers[numbers.len() - 1],
                items.len()
            ),
        );
        format!("/Outlines {root} 0 R")
    }

    fn outline_of(bytes: Vec<u8>) -> Vec<OutlineItem> {
        with_document(bytes, Limits::default(), |d| d.outline())
    }

    #[test]
    fn no_outline_is_empty() {
        let bytes = outline_pdf("", |_, _, _| String::new());
        assert!(outline_of(bytes).is_empty());
    }

    #[test]
    fn nested_items_with_depth_and_open() {
        let bytes = outline_pdf("", |b, p1, p2| {
            let root = b.reserve();
            let [a, a1, a2, a2x, c] = [(); 5].map(|_| b.reserve());
            b.set(
                a,
                format!(
                    "<< /Title (A) /Parent {root} 0 R /First {a1} 0 R /Last {a2} 0 R /Count 3 \
                     /Next {c} 0 R /Dest [{p1} 0 R /XYZ 10 90 0] >>"
                ),
            );
            b.set(
                a1,
                format!("<< /Title (A1) /Parent {a} 0 R /Next {a2} 0 R /Dest [{p2} 0 R /Fit] >>"),
            );
            b.set(
                a2,
                format!(
                    "<< /Title (A2) /Parent {a} 0 R /Prev {a1} 0 R /First {a2x} 0 R /Count -1 >>"
                ),
            );
            b.set(a2x, format!("<< /Title (A2x) /Parent {a2} 0 R >>"));
            b.set(
                c,
                format!("<< /Title (C) /Parent {root} 0 R /Prev {a} 0 R /Count 0 >>"),
            );
            b.set(
                root,
                format!("<< /Type /Outlines /First {a} 0 R /Last {c} 0 R /Count 5 >>"),
            );
            format!("/Outlines {root} 0 R")
        });
        let items = outline_of(bytes);
        let shape: Vec<(u16, &str, bool)> = items
            .iter()
            .map(|i| (i.depth, i.title.as_str(), i.open))
            .collect();
        assert_eq!(
            shape,
            vec![
                (0, "A", true),
                (1, "A1", false),
                (1, "A2", false),
                (2, "A2x", false),
                (0, "C", false),
            ]
        );
        // Page 1 is 200 x 100: y 90 from the bottom is 10 from the top.
        assert_eq!(
            items[0].target,
            Some(Target {
                page: 0,
                top: Some(10.0)
            })
        );
        assert_eq!(items[1].target, Some(Target { page: 1, top: None }));
        assert_eq!(items[2].target, None);
    }

    #[test]
    fn explicit_destination_kinds() {
        let bytes = outline_pdf("", |b, p1, p2| {
            flat(
                b,
                &[
                    (
                        "(xyz)".into(),
                        format!("/Dest [{p1} 0 R /XYZ null 70 null]"),
                    ),
                    ("(fith)".into(), format!("/Dest [{p2} 0 R /FitH 25]")),
                    ("(fitv)".into(), format!("/Dest [{p2} 0 R /FitV 5]")),
                    (
                        "(fitr)".into(),
                        format!("/Dest [{p1} 0 R /FitR 10 20 100 60]"),
                    ),
                    ("(fitbh)".into(), format!("/Dest [{p1} 0 R /FitBH 100]")),
                    ("(index)".into(), "/Dest [1 /XYZ 0 50 0]".into()),
                    ("(badindex)".into(), "/Dest [9 /Fit]".into()),
                    ("(nopage)".into(), "/Dest [999 0 R /Fit]".into()),
                    ("(none)".into(), "".into()),
                ],
            )
        });
        let tops: Vec<Option<Target>> = outline_of(bytes).into_iter().map(|i| i.target).collect();
        let t = |page, top| Some(Target { page, top });
        assert_eq!(
            tops,
            vec![
                t(0, Some(30.0)),
                t(1, Some(75.0)),
                t(1, None),
                t(0, Some(40.0)),
                t(0, Some(0.0)),
                t(1, Some(50.0)),
                None,
                None,
                None,
            ]
        );
    }

    #[test]
    fn named_destinations_in_dests_and_name_tree() {
        let bytes = outline_pdf("", |b, p1, p2| {
            let items = flat(
                b,
                &[
                    ("(one)".into(), "/Dest /first".into()),
                    ("(two)".into(), "/Dest (second)".into()),
                    ("(three)".into(), "/A << /S /GoTo /D (third) >>".into()),
                    ("(four)".into(), "/Dest /missing".into()),
                    ("(five)".into(), "/Dest (zeta)".into()),
                ],
            );
            let leaf1 = b.add(format!(
                "<< /Limits [(second) (second)] /Names [(second) [{p2} 0 R /XYZ 0 80 0]] >>"
            ));
            let leaf2 = b.add(format!(
                "<< /Limits [(third) (zeta)] /Names [(third) << /D [{p2} 0 R /FitH 10] >> \
                 (zeta) [{p1} 0 R /Fit]] >>"
            ));
            let tree = b.add(format!("<< /Kids [{leaf1} 0 R {leaf2} 0 R] >>"));
            format!(
                "{items} /Dests << /first [{p1} 0 R /FitH 40] >> /Names << /Dests {tree} 0 R >>"
            )
        });
        let items = outline_of(bytes);
        let targets: Vec<Option<Target>> = items.iter().map(|i| i.target).collect();
        let t = |page, top| Some(Target { page, top });
        assert_eq!(
            targets,
            vec![
                t(0, Some(60.0)),
                t(1, Some(20.0)),
                t(1, Some(90.0)),
                None,
                t(0, None),
            ]
        );
    }

    #[test]
    fn only_goto_actions_give_a_target() {
        let bytes = outline_pdf("", |b, p1, _| {
            flat(
                b,
                &[
                    (
                        "(goto)".into(),
                        format!("/A << /S /GoTo /D [{p1} 0 R /Fit] >>"),
                    ),
                    (
                        "(uri)".into(),
                        "/A << /S /URI /URI (https://example.com/) >>".into(),
                    ),
                    ("(launch)".into(), "/A << /S /Launch /F (/bin/sh) >>".into()),
                    (
                        "(js)".into(),
                        "/A << /S /JavaScript /JS (app.alert\\(1\\)) >>".into(),
                    ),
                    (
                        "(remote)".into(),
                        format!("/A << /S /GoToR /F (other.pdf) /D [{p1} 0 R /Fit] >>"),
                    ),
                    ("(named)".into(), "/A << /S /Named /N /NextPage >>".into()),
                    (
                        "(embedded)".into(),
                        format!("/A << /S /GoToE /D [{p1} 0 R /Fit] >>"),
                    ),
                ],
            )
        });
        let items = outline_of(bytes);
        assert_eq!(items.len(), 7);
        assert_eq!(items[0].target, Some(Target { page: 0, top: None }));
        for item in &items[1..] {
            assert_eq!(item.target, None, "{}", item.title);
        }
        assert_eq!(items[2].title, "launch");
    }

    #[test]
    fn rotated_page_counts_from_the_shown_top() {
        let bytes = outline_pdf("/Rotate 90", |b, p1, _| {
            flat(
                b,
                &[
                    ("(a)".into(), format!("/Dest [{p1} 0 R /XYZ 20 30 0]")),
                    ("(b)".into(), format!("/Dest [{p1} 0 R /XYZ 20 30 0]")),
                ],
            )
        });
        let items = outline_of(bytes);
        // Turned a quarter clockwise, the shown top runs along the page's
        // left side: the point (20, 30) is 20 from the top.
        assert_eq!(items[0].target.unwrap().top, Some(20.0));
    }

    #[test]
    fn a_next_cycle_ends() {
        let bytes = outline_pdf("", |b, _, _| {
            let root = b.reserve();
            let [a, c] = [(); 2].map(|_| b.reserve());
            b.set(
                a,
                format!("<< /Title (A) /Parent {root} 0 R /Next {c} 0 R >>"),
            );
            b.set(
                c,
                format!("<< /Title (B) /Parent {root} 0 R /Next {a} 0 R >>"),
            );
            b.set(
                root,
                format!("<< /Type /Outlines /First {a} 0 R /Count 2 >>"),
            );
            format!("/Outlines {root} 0 R")
        });
        let titles: Vec<String> = outline_of(bytes).into_iter().map(|i| i.title).collect();
        assert_eq!(titles, ["A", "B"]);
    }

    #[test]
    fn a_first_cycle_ends() {
        let bytes = outline_pdf("", |b, _, _| {
            let root = b.reserve();
            let [a, c] = [(); 2].map(|_| b.reserve());
            // A's child is B, whose child is A again; B's sibling is also A.
            b.set(a, format!("<< /Title (A) /First {c} 0 R /Count 1 >>"));
            b.set(c, format!("<< /Title (B) /First {a} 0 R /Next {a} 0 R >>"));
            b.set(
                root,
                format!("<< /Type /Outlines /First {a} 0 R /Count 2 >>"),
            );
            format!("/Outlines {root} 0 R")
        });
        let items: Vec<(u16, String)> = outline_of(bytes)
            .into_iter()
            .map(|i| (i.depth, i.title))
            .collect();
        assert_eq!(items, [(0, "A".to_string()), (1, "B".to_string())]);
    }

    #[test]
    fn items_deeper_than_the_cap_are_skipped_and_the_rest_kept() {
        let bytes = outline_pdf("", |b, _, _| {
            let root = b.reserve();
            // A chain of 40 levels, then a second top-level item.
            let chain: Vec<usize> = (0..40).map(|_| b.reserve()).collect();
            let last = b.reserve();
            for (level, number) in chain.iter().enumerate() {
                let child = chain
                    .get(level + 1)
                    .map(|c| format!("/First {c} 0 R"))
                    .unwrap_or_default();
                let next = if level == 0 {
                    format!("/Next {last} 0 R")
                } else {
                    String::new()
                };
                b.set(*number, format!("<< /Title (L{level}) {child} {next} >>"));
            }
            b.set(last, "<< /Title (After) >>");
            b.set(
                root,
                format!("<< /Type /Outlines /First {} 0 R >>", chain[0]),
            );
            format!("/Outlines {root} 0 R")
        });
        let items = outline_of(bytes);
        let deepest = items.iter().map(|i| i.depth).max().unwrap();
        assert_eq!(deepest, 31);
        assert_eq!(items.len(), 33);
        let last = items.last().unwrap();
        assert_eq!((last.depth, last.title.as_str()), (0, "After"));
    }

    #[test]
    fn more_items_than_the_cap_stop() {
        let bytes = outline_pdf("", |b, _, _| {
            let items: Vec<(String, String)> = (0..50)
                .map(|i| (format!("(item {i})"), String::new()))
                .collect();
            flat(b, &items)
        });
        let limits = Limits {
            max_outline_items: 7,
            ..Limits::default()
        };
        let items = with_document(bytes, limits, |d| d.outline());
        assert_eq!(items.len(), 7);
        assert_eq!(items[6].title, "item 6");
    }

    #[test]
    fn a_name_tree_whose_kids_loop_ends() {
        let bytes = outline_pdf("", |b, _, _| {
            let items = flat(b, &[("(x)".into(), "/Dest (nowhere)".into())]);
            let tree = b.reserve();
            let other = b.reserve();
            b.set(tree, format!("<< /Kids [{other} 0 R {tree} 0 R] >>"));
            b.set(other, format!("<< /Kids [{tree} 0 R {other} 0 R] >>"));
            format!("{items} /Names << /Dests {tree} 0 R >>")
        });
        let items = outline_of(bytes);
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].target, None);
    }

    #[test]
    fn a_long_title_is_cut() {
        let long = "x".repeat(100_000);
        let bytes = outline_pdf("", |b, _, _| {
            flat(b, &[(format!("({long})"), String::new())])
        });
        let items = outline_of(bytes);
        assert_eq!(items[0].title.chars().count(), 512);
    }

    #[test]
    fn titles_are_decoded_and_cleaned() {
        let bytes = outline_pdf("", |b, _, _| {
            flat(
                b,
                &[
                    (
                        utf16("A\u{7}B\u{0}\n\u{65E5}\u{672C}\u{1F600}"),
                        String::new(),
                    ),
                    (hex(b"caf\xE9 \x80 \x01\x7f"), String::new()),
                    (hex(b"\xEF\xBB\xBFna\xC3\xAFve\r\n"), String::new()),
                    (hex(b"\xFF\xFEh\x00i\x00"), String::new()),
                    (hex(&[0xFE, 0xFF, 0xD8, 0x00, 0x00, 0x41]), String::new()),
                ],
            )
        });
        let titles: Vec<String> = outline_of(bytes).into_iter().map(|i| i.title).collect();
        assert_eq!(titles[0], "AB\u{65E5}\u{672C}\u{1F600}");
        assert_eq!(titles[1], "caf\u{E9} \u{2022} ");
        assert_eq!(titles[2], "na\u{EF}ve");
        assert_eq!(titles[3], "hi");
        // A lone surrogate becomes U+FFFD, not a failure.
        assert_eq!(titles[4], "\u{FFFD}A");
    }

    #[test]
    fn decoding_text_strings() {
        assert_eq!(decode_text(b"abc", 2), "ab");
        assert_eq!(decode_text(&[0x18, 0x9F, 0xA0], 10), "\u{2D8}\u{20AC}");
        assert_eq!(decode_text(&[0xFE, 0xFF, 0x00, 0x41, 0x00], 10), "A");
    }
}
