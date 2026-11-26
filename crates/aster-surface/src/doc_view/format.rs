//! Formatting commands and the state the toolbar shows.

use aster_writer::edit::{self, ListKind, Selection};
use aster_writer::{
    Align, Document, LineSpacing, NumberFormat, Paragraph, Pos, RunLook, RunPatch, Underline,
    UnderlineStyle, VertAlign,
};

use super::DocView;
use crate::text_surface::Rgba8;

/// Whether something holds for all, none or part of the text.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Tri {
    #[default]
    Off,
    On,
    Mixed,
}

impl Tri {
    fn of(values: impl IntoIterator<Item = bool>) -> Tri {
        let mut seen = (false, false);
        for v in values {
            if v {
                seen.0 = true;
            } else {
                seen.1 = true;
            }
        }
        match seen {
            (true, false) => Tri::On,
            (true, true) => Tri::Mixed,
            _ => Tri::Off,
        }
    }
}

/// What the toolbar shows at the caret or over the selection.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct FormatState {
    pub bold: Tri,
    pub italic: Tri,
    pub underline: Tri,
    pub strike: Tri,
    pub superscript: Tri,
    pub subscript: Tri,
    /// The typeface; `Some("")` for the document's own, `None` if mixed.
    pub font: Option<String>,
    /// Points; `None` if mixed.
    pub size: Option<f32>,
    pub color: Option<Option<Rgba8>>,
    pub highlight: Option<Option<Rgba8>>,
    /// The id of the paragraph style (`Some("")` for none); `None` if mixed.
    pub style: Option<String>,
    pub align: Option<Align>,
    pub bullets: Tri,
    pub numbering: Tri,
}

/// A change the toolbar and menus ask for.
#[derive(Clone, Debug, PartialEq)]
pub enum DocCommand {
    Undo,
    Redo,
    Bold,
    Italic,
    Underline,
    Strikethrough,
    Superscript,
    Subscript,
    /// `None` for the document's own typeface.
    Font(Option<String>),
    Size(f32),
    /// One step larger (positive) or smaller.
    SizeStep(i32),
    /// `None` for automatic.
    Color(Option<Rgba8>),
    Highlight(Option<Rgba8>),
    Align(Align),
    /// Steps of half an inch; negative to take out.
    Indent(i32),
    LineSpacing(LineSpacing),
    Bullets,
    Numbering,
    Style(String),
}

/// Sizes the size steps go through, in points.
const SIZES: [f32; 17] = [
    8.0, 9.0, 10.0, 10.5, 11.0, 12.0, 14.0, 16.0, 18.0, 20.0, 22.0, 24.0, 26.0, 28.0, 36.0, 48.0,
    72.0,
];

/// Typefaces that are for East Asian text: choosing one sets that face too.
const EAST_ASIAN: [&str; 12] = [
    "SimSun", "SimHei", "KaiTi", "FangSong", "Microsoft YaHei", "宋体", "黑体", "楷体", "仿宋",
    "MS Mincho", "MS Gothic", "Yu Mincho",
];

fn kind_of(document: &Document, paragraph: &Paragraph) -> Option<ListKind> {
    let list = paragraph.props.list.as_ref()?;
    let level = document.numbering.get(list.list)?.levels.first()?;
    match level.format {
        NumberFormat::Bullet => Some(ListKind::Bullet),
        NumberFormat::None => None,
        _ => Some(ListKind::Numbered),
    }
}

impl DocView {
    /// The ends of what commands act on: the selection, or the caret.
    fn range(&self) -> Option<(Pos, Pos)> {
        let sel = self.sel.as_ref()?;
        Some(if sel.anchor <= sel.focus {
            (sel.anchor.clone(), sel.focus.clone())
        } else {
            (sel.focus.clone(), sel.anchor.clone())
        })
    }

    /// The looks of the text in the range; at a caret the look text typed
    /// there gets, with what is pending.
    fn looks(&self) -> Vec<RunLook> {
        let Some((from, to)) = self.range() else {
            return Vec::new();
        };
        if from == to {
            let mut look = self
                .document
                .paragraph(&from.path)
                .map(|p| p.look_at(from.offset).clone())
                .unwrap_or_default();
            if let Some(pending) = &self.pending {
                pending.apply(&mut look);
            }
            return vec![look];
        }
        let mut out = Vec::new();
        for (path, paragraph) in self.document.paragraphs() {
            if path < from.path || path > to.path {
                continue;
            }
            let a = if path == from.path { from.offset } else { 0 };
            let b = if path == to.path {
                to.offset
            } else {
                usize::MAX
            };
            let mut start = 0;
            let mut any = false;
            for run in &paragraph.runs {
                let end = start + run.text.len();
                if !run.text.is_empty() && end > a && start < b {
                    out.push(run.look.clone());
                    any = true;
                }
                start = end;
            }
            if !any {
                out.push(paragraph.mark.clone());
            }
        }
        out
    }

    fn paragraphs_in_range(&self) -> Vec<&Paragraph> {
        let Some((from, to)) = self.range() else {
            return Vec::new();
        };
        self.document
            .paragraphs()
            .into_iter()
            .filter(|(path, _)| *path >= from.path && *path <= to.path)
            .map(|(_, p)| &**p)
            .collect()
    }

    /// What the toolbar shows for the caret or the selection.
    pub fn format_state(&self) -> FormatState {
        let looks = self.looks();
        let same = |f: &dyn Fn(&RunLook) -> String| -> Option<String> {
            let first = looks.first().map(f)?;
            looks.iter().all(|l| f(l) == first).then_some(first)
        };
        let paragraphs = self.paragraphs_in_range();
        let first = paragraphs.first();
        let all_same = |f: &dyn Fn(&Paragraph) -> String| -> Option<String> {
            let one = first.map(|p| f(p))?;
            paragraphs.iter().all(|p| f(p) == one).then_some(one)
        };
        let color = |l: &RunLook| format!("{:?}", l.color);
        let highlight = |l: &RunLook| format!("{:?}", l.highlight);
        FormatState {
            bold: Tri::of(looks.iter().map(|l| l.bold)),
            italic: Tri::of(looks.iter().map(|l| l.italic)),
            underline: Tri::of(looks.iter().map(|l| l.underline.is_some())),
            strike: Tri::of(looks.iter().map(|l| l.strike || l.double_strike)),
            superscript: Tri::of(looks.iter().map(|l| l.position == VertAlign::Superscript)),
            subscript: Tri::of(looks.iter().map(|l| l.position == VertAlign::Subscript)),
            font: same(&|l| l.font.as_deref().unwrap_or("").to_owned()),
            size: same(&|l| l.size.to_bits().to_string())
                .and_then(|_| looks.first().map(|l| l.size)),
            color: same(&color).and_then(|_| looks.first().map(|l| l.color)),
            highlight: same(&highlight).and_then(|_| looks.first().map(|l| l.highlight)),
            style: all_same(&|p| p.props.style.as_deref().unwrap_or("").to_owned()),
            align: all_same(&|p| format!("{:?}", p.props.align))
                .and_then(|_| first.map(|p| p.props.align)),
            bullets: Tri::of(
                paragraphs
                    .iter()
                    .map(|p| kind_of(&self.document, p) == Some(ListKind::Bullet)),
            ),
            numbering: Tri::of(
                paragraphs
                    .iter()
                    .map(|p| kind_of(&self.document, p) == Some(ListKind::Numbered)),
            ),
        }
    }

    /// The paragraph styles to offer: id and name, best first.
    pub fn paragraph_styles(&self) -> Vec<(String, String)> {
        self.document
            .styles
            .paragraph_styles()
            .into_iter()
            .map(|s| (s.id.to_string(), s.name.clone()))
            .collect()
    }

    /// The name of the style with this id, as the file gives it.
    pub fn style_name(&self, id: &str) -> Option<String> {
        self.document.styles.get(id).map(|s| s.name.clone())
    }

    /// The id of the style paragraphs have that name none.
    pub fn default_style_id(&self) -> String {
        self.document
            .styles
            .default_style
            .as_deref()
            .unwrap_or("")
            .to_owned()
    }

    /// Carry out a command. Returns whether anything changed.
    pub fn command(&mut self, command: DocCommand) -> bool {
        if self.compose.is_some() {
            return false;
        }
        if !matches!(command, DocCommand::Undo | DocCommand::Redo) {
            self.ensure_paragraph();
        }
        let state = self.format_state();
        match command {
            DocCommand::Undo => self.undo(),
            DocCommand::Redo => self.redo(),
            DocCommand::Bold => self.patch_run(RunPatch {
                bold: Some(state.bold != Tri::On),
                ..RunPatch::default()
            }),
            DocCommand::Italic => self.patch_run(RunPatch {
                italic: Some(state.italic != Tri::On),
                ..RunPatch::default()
            }),
            DocCommand::Underline => self.patch_run(RunPatch {
                underline: Some((state.underline != Tri::On).then_some(Underline {
                    style: UnderlineStyle::Single,
                    color: None,
                })),
                ..RunPatch::default()
            }),
            DocCommand::Strikethrough => self.patch_run(RunPatch {
                strike: Some(state.strike != Tri::On),
                ..RunPatch::default()
            }),
            DocCommand::Superscript => self.patch_run(RunPatch {
                position: Some(if state.superscript == Tri::On {
                    VertAlign::Baseline
                } else {
                    VertAlign::Superscript
                }),
                ..RunPatch::default()
            }),
            DocCommand::Subscript => self.patch_run(RunPatch {
                position: Some(if state.subscript == Tri::On {
                    VertAlign::Baseline
                } else {
                    VertAlign::Subscript
                }),
                ..RunPatch::default()
            }),
            DocCommand::Font(name) => {
                let east = name
                    .as_deref()
                    .filter(|n| EAST_ASIAN.contains(n))
                    .map(|n| Some(n.into()));
                self.patch_run(RunPatch {
                    font: Some(name.map(|n| n.as_str().into())),
                    east_asian_font: east,
                    ..RunPatch::default()
                })
            }
            DocCommand::Size(size) => self.patch_run(RunPatch {
                size: Some(size.clamp(1.0, 1638.0)),
                ..RunPatch::default()
            }),
            DocCommand::SizeStep(steps) => {
                let current = state
                    .size
                    .or_else(|| self.looks().first().map(|l| l.size))
                    .unwrap_or(10.0);
                let mut size = current;
                for _ in 0..steps.unsigned_abs() {
                    size = if steps > 0 {
                        SIZES
                            .iter()
                            .copied()
                            .find(|s| *s > size + 0.01)
                            .unwrap_or(size + 10.0)
                    } else {
                        SIZES
                            .iter()
                            .rev()
                            .copied()
                            .find(|s| *s < size - 0.01)
                            .unwrap_or((size - 1.0).max(1.0))
                    };
                }
                self.patch_run(RunPatch {
                    size: Some(size),
                    ..RunPatch::default()
                })
            }
            DocCommand::Color(color) => self.patch_run(RunPatch {
                color: Some(color),
                ..RunPatch::default()
            }),
            DocCommand::Highlight(color) => self.patch_run(RunPatch {
                highlight: Some(color),
                ..RunPatch::default()
            }),
            DocCommand::Align(align) => self.paragraph_edit(move |d, f, t| {
                edit::set_align(d, f, t, align);
            }),
            DocCommand::Indent(steps) => self.paragraph_edit(move |d, f, t| {
                edit::change_indent(d, f, t, steps);
            }),
            DocCommand::LineSpacing(line) => self.paragraph_edit(move |d, f, t| {
                edit::set_line_spacing(d, f, t, line);
            }),
            DocCommand::Bullets => {
                let kind = (state.bullets != Tri::On).then_some(ListKind::Bullet);
                self.paragraph_edit(move |d, f, t| edit::set_list(d, f, t, kind))
            }
            DocCommand::Numbering => {
                let kind = (state.numbering != Tri::On).then_some(ListKind::Numbered);
                self.paragraph_edit(move |d, f, t| edit::set_list(d, f, t, kind))
            }
            DocCommand::Style(id) => self.paragraph_edit(move |d, f, t| {
                edit::set_style(d, f, t, &id);
            }),
        }
    }

    /// A change to paragraphs, over the selection or the caret's paragraph.
    fn paragraph_edit(&mut self, change: impl FnOnce(&mut Document, &Pos, &Pos)) -> bool {
        self.apply(None, true, |document, selection| {
            let (from, to) = selection.ordered();
            change(document, &from, &to);
            Some(Selection::new(selection.anchor.clone(), selection.focus.clone()))
        })
    }

    /// A change of look: over the selection at once; at a caret it waits for
    /// the next typed text (an empty paragraph takes it on its mark).
    fn patch_run(&mut self, patch: RunPatch) -> bool {
        let Some((from, to)) = self.range() else {
            return false;
        };
        let empty = self
            .document
            .paragraph(&from.path)
            .is_some_and(Paragraph::is_empty);
        if from == to && !empty {
            let merged = merge(self.pending.take(), patch);
            self.pending = Some(merged);
            self.touch();
            return true;
        }
        self.apply(None, true, |document, selection| {
            let (from, to) = selection.ordered();
            edit::apply_run_patch(document, &from, &to, &patch);
            Some(Selection::new(selection.anchor.clone(), selection.focus.clone()))
        })
    }

    /// Whether there is a look waiting for the next typed text.
    pub fn has_pending_look(&self) -> bool {
        self.pending.is_some()
    }

    /// A paragraph's line spacing at the caret, for the toolbar.
    pub fn line_spacing(&self) -> Option<LineSpacing> {
        self.paragraphs_in_range().first().map(|p| p.props.line)
    }
}

/// `new` on top of `old`: what `new` sets wins.
fn merge(old: Option<RunPatch>, new: RunPatch) -> RunPatch {
    let mut out = old.unwrap_or_default();
    macro_rules! take {
        ($($f:ident),+) => { $( if new.$f.is_some() { out.$f = new.$f.clone(); } )+ };
    }
    take!(
        font,
        east_asian_font,
        size,
        bold,
        italic,
        underline,
        strike,
        color,
        highlight,
        position
    );
    out
}
