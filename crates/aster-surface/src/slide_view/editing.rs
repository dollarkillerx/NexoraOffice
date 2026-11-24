//! Editing the slide on screen: selecting, moving and resizing shapes,
//! typing into their text with an input method, formatting, inserting
//! shapes and pictures, adding, copying, moving and deleting slides, and
//! undoing all of it.
//!
//! A click on a shape selects it; a drag moves it, a drag on one of its
//! handles resizes it (Shift keeps its proportions). A click that does not
//! move on a text box, a placeholder or a shape already selected puts the
//! caret where it was; a double click selects a word, a triple click a
//! paragraph. Every change is one step of the history; typed characters
//! join the step before them while the caret stays in the same text.

use std::sync::Arc;

use aster_slides::edit::{LINE_BREAK, TextPos};
use aster_slides::{
    Align as SlideAlign, Anchor, Fill, Frame, Geometry, List, Outline, Paragraph, Picture, Role,
    Shape, Slide, TextBody,
};
use vello_cpu::kurbo::{Affine, Point, Rect, Shape as _, Stroke, Vec2};

use super::{
    LaidPara, LaidShape, SlideView, color, lay_out_paragraphs, lay_out_text, shape_transform,
    simple,
};
use crate::text_surface::ImeEvent;
use aster_text::Align;

/// What is selected on the current slide.
#[derive(Clone, Debug, Default, PartialEq)]
pub enum Selection {
    #[default]
    None,
    /// A shape, by index into the slide's shapes.
    Shape(usize),
    /// The caret, or a range from `anchor` to `caret`, in a shape's text.
    Text {
        shape: usize,
        anchor: TextPos,
        caret: TextPos,
    },
}

impl Selection {
    pub fn shape(&self) -> Option<usize> {
        match self {
            Selection::None => None,
            Selection::Shape(shape) | Selection::Text { shape, .. } => Some(*shape),
        }
    }
}

/// The pointer's look over a point of the view.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Cursor {
    Default,
    Text,
    Move,
    /// Resizing: left-right, up-down, and the two diagonals.
    ResizeX,
    ResizeY,
    ResizeDown,
    ResizeUp,
}

/// Shapes the toolbar inserts.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NewShape {
    TextBox,
    Rect,
    RoundRect,
    Ellipse,
    Line,
}

/// A formatting command.
#[derive(Clone, Debug, PartialEq)]
pub enum TextFormat {
    Bold,
    Italic,
    Underline,
    Strike,
    Size(f32),
    /// One step up or down the usual sizes.
    SizeStep(bool),
    /// `None`: the colour the text inherits.
    Color(Option<[u8; 4]>),
    Align(SlideAlign),
    Bullets,
    Numbering,
    /// One outline level deeper or shallower.
    Indent(bool),
}

/// How the text the toolbar acts on looks.
#[derive(Clone, Debug, PartialEq)]
pub struct TextLook {
    pub bold: bool,
    pub italic: bool,
    pub underline: bool,
    pub strike: bool,
    pub size: f32,
    pub color: Option<[u8; 4]>,
    pub align: SlideAlign,
    pub bullets: bool,
    pub numbered: bool,
}

/// Font sizes the size buttons step through, as PowerPoint's.
const SIZES: [f32; 22] = [
    8.0, 9.0, 10.0, 10.5, 11.0, 12.0, 14.0, 16.0, 18.0, 20.0, 24.0, 28.0, 32.0, 36.0, 40.0, 44.0,
    48.0, 54.0, 60.0, 66.0, 72.0, 96.0,
];

/// Which handle of a selected shape.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Handle {
    TopLeft,
    Top,
    TopRight,
    Right,
    BottomRight,
    Bottom,
    BottomLeft,
    Left,
}

impl Handle {
    const ALL: [Handle; 8] = [
        Handle::TopLeft,
        Handle::Top,
        Handle::TopRight,
        Handle::Right,
        Handle::BottomRight,
        Handle::Bottom,
        Handle::BottomLeft,
        Handle::Left,
    ];

    /// Where on the frame, as shares of its width and height.
    fn at(self) -> (f64, f64) {
        match self {
            Handle::TopLeft => (0.0, 0.0),
            Handle::Top => (0.5, 0.0),
            Handle::TopRight => (1.0, 0.0),
            Handle::Right => (1.0, 0.5),
            Handle::BottomRight => (1.0, 1.0),
            Handle::Bottom => (0.5, 1.0),
            Handle::BottomLeft => (0.0, 1.0),
            Handle::Left => (0.0, 0.5),
        }
    }

    fn cursor(self) -> Cursor {
        match self {
            Handle::Top | Handle::Bottom => Cursor::ResizeY,
            Handle::Left | Handle::Right => Cursor::ResizeX,
            Handle::TopLeft | Handle::BottomRight => Cursor::ResizeDown,
            Handle::TopRight | Handle::BottomLeft => Cursor::ResizeUp,
        }
    }
}

/// A press of the pointer being followed.
#[derive(Clone, Debug)]
pub(crate) enum Drag {
    /// On a shape, not moved yet: a move if it moves, else a click.
    Pending {
        shape: usize,
        from: Point,
        was_selected: bool,
        text_at: Option<TextPos>,
    },
    Move {
        shape: usize,
        from: Point,
        frame: Frame,
        before: Box<Slide>,
    },
    Resize {
        shape: usize,
        handle: Handle,
        from: Point,
        frame: Frame,
        before: Box<Slide>,
    },
    /// Selecting text.
    Text { shape: usize },
    /// A thumbnail, perhaps being dragged to another place.
    Slide { from: usize, y: f64, moving: bool },
}

#[derive(Clone, Debug)]
enum Change {
    /// One slide, by index, before and after.
    Slide(usize, Box<Slide>, Box<Slide>),
    /// The whole list of slides.
    Deck(Vec<Slide>, Vec<Slide>),
}

#[derive(Clone, Debug)]
struct Step {
    change: Change,
    before: (usize, Selection),
    after: (usize, Selection),
    /// Typing into this slide and shape (`usize::MAX` for the notes):
    /// more typing there joins this step.
    typing: Option<(usize, usize)>,
}

/// Steps to undo and redo, and where the file was saved.
#[derive(Debug)]
pub(crate) struct History {
    undo: Vec<Step>,
    redo: Vec<Step>,
    /// The number of steps to undo the saved state had; `None` when it can
    /// no longer be reached.
    saved: Option<usize>,
}

impl Default for History {
    fn default() -> Self {
        Self {
            undo: Vec::new(),
            redo: Vec::new(),
            saved: Some(0),
        }
    }
}

/// How far a press may move and still be a click, logical pixels.
const CLICK_SLOP: f64 = 3.0;
/// Handle size and how near the pointer must be, logical pixels.
const HANDLE: f64 = 7.0;
/// Inches to EMU.
const INCH: f64 = 914_400.0;

impl SlideView {
    // --- State the shell asks about ---

    pub fn selection(&self) -> &Selection {
        &self.selection
    }

    pub fn is_editing_text(&self) -> bool {
        matches!(self.selection, Selection::Text { .. })
    }

    /// The thumbnails have the keyboard.
    pub fn strip_has_focus(&self) -> bool {
        self.strip_focus
    }

    pub fn can_undo(&self) -> bool {
        !self.history.undo.is_empty()
    }

    pub fn can_redo(&self) -> bool {
        !self.history.redo.is_empty()
    }

    /// Changed since read or last saved.
    pub fn is_modified(&self) -> bool {
        self.history.saved != Some(self.history.undo.len())
    }

    /// How many steps there are to undo: a save remembers it.
    pub fn depth(&self) -> usize {
        self.history.undo.len()
    }

    /// What was saved is the state `depth` steps in.
    pub fn mark_saved_at(&mut self, depth: usize) {
        self.history.saved = Some(depth);
    }

    /// The presentation is now as saved: undo steps of the file before stay,
    /// but the file is a new starting point.
    pub fn replace_presentation(&mut self, presentation: Arc<Presentation>) {
        self.presentation = presentation;
        self.forget(None);
    }

    // --- Undo ---

    pub fn undo(&mut self) -> bool {
        self.commit_preedit();
        let Some(step) = self.history.undo.pop() else {
            return false;
        };
        self.restore(&step.change, true);
        self.current = step.before.0.min(self.count().saturating_sub(1));
        self.selection = self.valid(step.before.1.clone());
        self.history.redo.push(step);
        self.after_history();
        true
    }

    pub fn redo(&mut self) -> bool {
        self.commit_preedit();
        let Some(step) = self.history.redo.pop() else {
            return false;
        };
        self.restore(&step.change, false);
        self.current = step.after.0.min(self.count().saturating_sub(1));
        self.selection = self.valid(step.after.1.clone());
        self.history.undo.push(step);
        self.after_history();
        true
    }

    fn after_history(&mut self) {
        self.drag = None;
        self.goal_x = None;
        self.forget(None);
        self.clamp_strip();
    }

    fn restore(&mut self, change: &Change, undo: bool) {
        let presentation = Arc::make_mut(&mut self.presentation);
        match change {
            Change::Slide(index, before, after) => {
                if let Some(slide) = presentation.slides.get_mut(*index) {
                    *slide = if undo {
                        (**before).clone()
                    } else {
                        (**after).clone()
                    };
                }
            }
            Change::Deck(before, after) => {
                presentation.slides = if undo { before.clone() } else { after.clone() };
            }
        }
    }

    /// A selection that still points at something on the current slide.
    fn valid(&self, selection: Selection) -> Selection {
        let Some(slide) = self.presentation.slides.get(self.current) else {
            return Selection::None;
        };
        match selection {
            Selection::Shape(i) if i < slide.shapes.len() => selection,
            Selection::Text {
                shape,
                anchor,
                caret,
            } => match slide.shapes.get(shape).and_then(|s| s.text.as_ref()) {
                Some(body) => Selection::Text {
                    shape,
                    anchor: body.clamp(anchor),
                    caret: body.clamp(caret),
                },
                None => Selection::None,
            },
            _ => Selection::None,
        }
    }

    fn push(&mut self, step: Step) {
        let history = &mut self.history;
        if history
            .saved
            .is_some_and(|saved| saved > history.undo.len())
        {
            history.saved = None;
        }
        history.redo.clear();
        history.undo.push(step);
    }

    /// Change the current slide in one step. `f` returns whether it changed
    /// anything. `typing` joins the step before when that was typing into
    /// the same shape.
    fn edit(
        &mut self,
        typing: Option<usize>,
        f: impl FnOnce(&mut Slide, &mut Selection, (i64, i64)) -> bool,
    ) -> bool {
        let index = self.current;
        let Some(before) = self.presentation.slides.get(index).cloned() else {
            return false;
        };
        let size = self.presentation.size;
        let mut selection = self.selection.clone();
        let mut slide = before.clone();
        if !f(&mut slide, &mut selection, size) {
            return false;
        }
        Arc::make_mut(&mut self.presentation).slides[index] = slide;
        let selection_before = std::mem::replace(&mut self.selection, selection);
        if let Some(shape) = self.selection.shape() {
            self.grow(index, shape);
        }
        let after = self.presentation.slides[index].clone();
        let typing = typing.map(|shape| (index, shape));
        let history = &mut self.history;
        let joins = typing.is_some()
            && history.redo.is_empty()
            && history.saved != Some(history.undo.len())
            && history.undo.last().is_some_and(|last| {
                last.typing == typing && matches!(last.change, Change::Slide(i, ..) if i == index)
            });
        if joins && let Some(last) = history.undo.last_mut() {
            if let Change::Slide(_, _, then) = &mut last.change {
                **then = after;
            }
            last.after = (index, self.selection.clone());
        } else {
            self.push(Step {
                change: Change::Slide(index, Box::new(before), Box::new(after)),
                before: (index, selection_before),
                after: (index, self.selection.clone()),
                typing,
            });
        }
        self.forget(Some(index));
        true
    }

    /// Change the list of slides in one step; `f` may move to another slide.
    fn edit_deck(&mut self, f: impl FnOnce(&mut Vec<Slide>, &mut usize) -> bool) -> bool {
        self.commit_preedit();
        let before = self.presentation.slides.clone();
        let mut slides = before.clone();
        let mut current = self.current;
        if !f(&mut slides, &mut current) {
            return false;
        }
        let from = (self.current, self.selection.clone());
        Arc::make_mut(&mut self.presentation).slides = slides.clone();
        self.current = current.min(slides.len().saturating_sub(1));
        self.selection = Selection::None;
        self.push(Step {
            change: Change::Deck(before, slides),
            before: from,
            after: (self.current, Selection::None),
            typing: None,
        });
        self.after_history();
        // Keep the current thumbnail in view.
        let current = self.current;
        self.current = usize::MAX;
        self.go_to(current);
        self.current = current;
        true
    }

    /// A text box that fits its text takes the height of it.
    fn grow(&mut self, index: usize, shape: usize) {
        let Some(found) = self.presentation.slides[index].shapes.get(shape) else {
            return;
        };
        let Some(body) = found.text.as_ref().filter(|b| b.grow) else {
            return;
        };
        // Laid out at one pixel a point.
        let per_emu = 1.0 / aster_slides::EMU_PER_POINT;
        let (_, _, used) = lay_out_paragraphs(
            &mut self.shaper,
            body,
            found.frame.width as f64 * per_emu,
            per_emu,
        );
        let height = (used / per_emu).round() as i64 + body.insets[1] + body.insets[3];
        if height != found.frame.height {
            let slide = &mut Arc::make_mut(&mut self.presentation).slides[index];
            slide.shapes[shape].frame.height = height.max(1);
        }
    }

    // --- Geometry ---

    /// The current slide's page and its pixels per EMU.
    fn page(&self) -> (Rect, f64) {
        let page = self.page_rect();
        (page, page.width() / self.presentation.size.0.max(1) as f64)
    }

    fn frame_rect(frame: &Frame, page: Rect, per: f64) -> Rect {
        Rect::new(
            page.x0 + frame.x as f64 * per,
            page.y0 + frame.y as f64 * per,
            page.x0 + (frame.x + frame.width) as f64 * per,
            page.y0 + (frame.y + frame.height) as f64 * per,
        )
    }

    /// A shape's rectangle on screen and its rotation and flips.
    fn placed(&self, shape: &Shape) -> (Rect, Affine) {
        let (page, per) = self.page();
        let rect = Self::frame_rect(&shape.frame, page, per);
        (rect, shape_transform(shape, rect))
    }

    /// The shape of the current slide under a point, topmost first. Shapes
    /// of the layout and master cannot be picked.
    fn shape_at(&self, point: Point) -> Option<usize> {
        let slide = self.presentation.slides.get(self.current)?;
        let s = self.s();
        slide
            .shapes
            .iter()
            .enumerate()
            .rev()
            .find_map(|(i, shape)| {
                if shape.inherited {
                    return None;
                }
                let (rect, transform) = self.placed(shape);
                let local = transform.inverse() * point;
                let hit = if shape.geometry == Geometry::Line {
                    let (a, b) = (Point::new(rect.x0, rect.y0), Point::new(rect.x1, rect.y1));
                    distance_to_segment(local, a, b) <= 5.0 * s
                } else {
                    rect.inflate(2.0 * s, 2.0 * s).contains(local)
                };
                hit.then_some(i)
            })
    }

    fn handles(&self, shape: &Shape) -> Vec<(Handle, Point)> {
        let (rect, transform) = self.placed(shape);
        let list: &[Handle] = if shape.geometry == Geometry::Line {
            &[Handle::TopLeft, Handle::BottomRight]
        } else {
            &Handle::ALL
        };
        list.iter()
            .map(|handle| {
                let (fx, fy) = handle.at();
                let local = Point::new(rect.x0 + rect.width() * fx, rect.y0 + rect.height() * fy);
                (*handle, transform * local)
            })
            .collect()
    }

    fn handle_at(&self, point: Point) -> Option<(usize, Handle)> {
        let shape = self.selection.shape()?;
        let found = self
            .presentation
            .slides
            .get(self.current)?
            .shapes
            .get(shape)?;
        // A table's columns do not follow its frame: it only moves.
        if found.role == Role::Table {
            return None;
        }
        let reach = HANDLE * self.s();
        self.handles(found)
            .into_iter()
            .find(|(_, at)| (*at - point).hypot() <= reach)
            .map(|(handle, _)| (shape, handle))
    }

    /// The layout of a shape's text as drawn, with the composition when
    /// it is being typed into.
    fn text_layout(&mut self, shape: usize) -> Option<LaidShape> {
        let (page, per) = self.page();
        let presentation = self.presentation.clone();
        let found = presentation.slides.get(self.current)?.shapes.get(shape)?;
        let body = found.text.as_ref()?;
        let shown = self.shown_body(shape, body);
        let width = found.frame.width as f64 * per;
        let height = found.frame.height as f64 * per;
        let _ = page;
        let (texts, paras) = lay_out_text(&mut self.shaper, &shown, width, height, per);
        Some(LaidShape {
            index: shape,
            texts,
            paras,
            label: None,
            cells: Vec::new(),
            visible: true,
        })
    }

    /// The body as shown: with the composition at the caret, underlined.
    fn shown_body(&self, shape: usize, body: &TextBody) -> TextBody {
        let mut shown = body.clone();
        shown.ensure_paragraph();
        if let (
            Some((text, _)),
            Selection::Text {
                shape: s, caret, ..
            },
        ) = (&self.preedit, &self.selection)
            && *s == shape
        {
            let end = shown.insert(*caret, &text.replace('\n', " "));
            shown.format(*caret, end, |run| run.underline = true);
        }
        shown
    }

    /// The caret position in the shown text: after the composition's own
    /// caret when composing.
    fn shown_caret(&self, caret: TextPos) -> TextPos {
        match &self.preedit {
            Some((text, cursor)) => TextPos::new(
                caret.para,
                caret.offset + cursor.map_or(text.len(), |(start, _)| start),
            ),
            None => caret,
        }
    }

    /// The text position under a point in a shape's text.
    fn text_at(&mut self, shape: usize, point: Point) -> Option<TextPos> {
        let laid = self.text_layout(shape)?;
        let found = self
            .presentation
            .slides
            .get(self.current)?
            .shapes
            .get(shape)?;
        let (rect, transform) = self.placed(found);
        let local = transform.inverse() * point;
        let (x, y) = (local.x - rect.x0, local.y - rect.y0);
        let body = found.text.as_ref()?;
        let index = laid
            .paras
            .iter()
            .position(|p| y < p.y + p.height)
            .unwrap_or(laid.paras.len().saturating_sub(1));
        let para = laid.paras.get(index)?;
        let offset = match &para.laid {
            Some(paragraph) => paragraph.offset_at((x - para.x) as f32, (y - para.y) as f32),
            None => 0,
        };
        Some(body.clamp(TextPos::new(index, offset)))
    }

    /// Where the caret is drawn: x, top and height in the shape's frame.
    fn caret_box(para: &LaidPara, offset: usize) -> (f64, f64, f64) {
        let Some(paragraph) = &para.laid else {
            let x = match para.align {
                SlideAlign::Center => para.x + para.width / 2.0,
                SlideAlign::Right => para.x + para.width,
                _ => para.x,
            };
            return (x, para.y, para.height);
        };
        let text = paragraph.text();
        let offset = offset.min(text.len());
        let next = text[offset..]
            .chars()
            .next()
            .map(|c| offset..offset + c.len_utf8());
        // Before a line break the caret ends the line it breaks.
        let next = next.filter(|r| &text[r.clone()] != "\n");
        let (x, y, h) = match next.and_then(|r| paragraph.rects(r).into_iter().next()) {
            Some([x0, y0, _, y1]) => (x0, y0, y1 - y0),
            None => match paragraph.rects(0..offset).last() {
                Some([_, y0, x1, y1]) if !text[..offset].ends_with('\n') => (*x1, *y0, y1 - y0),
                _ => {
                    // After a line break: the start of the next line.
                    let line = paragraph
                        .lines()
                        .iter()
                        .find(|l| {
                            l.clusters.is_empty()
                                || paragraph
                                    .line_ranges()
                                    .iter()
                                    .any(|r| r.start >= offset && r.start == offset)
                        })
                        .or_else(|| paragraph.lines().last());
                    match line {
                        Some(line) => (line.left, line.top, line.height),
                        None => (0.0, 0.0, para.height as f32),
                    }
                }
            },
        };
        (para.x + f64::from(x), para.y + f64::from(y), f64::from(h))
    }

    // --- The pointer ---

    /// A press of the main button, physical pixels. `clicks` counts a
    /// double or triple click. Returns whether anything changed.
    pub fn press(&mut self, x: f64, y: f64, shift: bool, clicks: u32) -> bool {
        if self.presenting {
            return self.show_step(true).is_some();
        }
        self.goal_x = None;
        if let Some(index) = self.thumb_at(x, y) {
            self.commit_preedit();
            // The keyboard goes to the thumbnails: nothing on the slide stays
            // selected.
            self.leave_text();
            self.selection = Selection::None;
            let moved = self.go_to(index);
            self.strip_focus = true;
            self.drag = Some(Drag::Slide {
                from: index,
                y,
                moving: false,
            });
            self.touch();
            return moved || true;
        }
        if x < self.strip_width() {
            return false;
        }
        let was_strip = std::mem::replace(&mut self.strip_focus, false);
        let point = Point::new(x, y);
        if let Some((shape, handle)) = self.handle_at(point) {
            let Some(found) = self.presentation.slides[self.current].shapes.get(shape) else {
                return false;
            };
            self.drag = Some(Drag::Resize {
                shape,
                handle,
                from: point,
                frame: found.frame,
                before: Box::new(self.presentation.slides[self.current].clone()),
            });
            return true;
        }
        // In the text being typed into: the caret.
        if let Selection::Text { shape, anchor, .. } = self.selection.clone()
            && self.inside_text(shape, point)
            && let Some(at) = self.text_at(shape, point)
        {
            self.commit_preedit();
            self.click_text(shape, at, shift.then_some(anchor), clicks);
            return true;
        }
        let Some(shape) = self.shape_at(point) else {
            let changed = self.selection != Selection::None || was_strip;
            self.leave_text();
            self.selection = Selection::None;
            self.drag = None;
            self.touch();
            return changed;
        };
        let takes_text = self.presentation.slides[self.current].shapes[shape].takes_text();
        if clicks >= 2 && takes_text {
            self.leave_text();
            let at = self.text_at(shape, point).unwrap_or_default();
            self.click_text(shape, at, None, clicks);
            return true;
        }
        let was_selected = self.selection.shape() == Some(shape);
        let text_at = if takes_text {
            self.text_at(shape, point)
        } else {
            None
        };
        self.leave_text();
        self.selection = Selection::Shape(shape);
        self.drag = Some(Drag::Pending {
            shape,
            from: point,
            was_selected,
            text_at,
        });
        self.touch();
        true
    }

    /// Whether a point is inside a shape's frame, away from its edge where
    /// a drag moves it.
    fn inside_text(&self, shape: usize, point: Point) -> bool {
        let Some(found) = self
            .presentation
            .slides
            .get(self.current)
            .and_then(|s| s.shapes.get(shape))
        else {
            return false;
        };
        let (rect, transform) = self.placed(found);
        let local = transform.inverse() * point;
        let edge = 4.0 * self.s();
        let inner = rect.inset(-edge);
        inner.width() > 0.0 && inner.height() > 0.0 && inner.contains(local)
    }

    fn click_text(&mut self, shape: usize, at: TextPos, extend: Option<TextPos>, clicks: u32) {
        let body = self.presentation.slides[self.current].shapes[shape]
            .text
            .clone()
            .unwrap_or_default();
        let (anchor, caret) = match clicks {
            2 => body.word_at(at),
            n if n >= 3 => (
                TextPos::new(at.para, 0),
                TextPos::new(
                    at.para,
                    body.paragraphs.get(at.para).map_or(0, Paragraph::len),
                ),
            ),
            _ => (extend.unwrap_or(at), at),
        };
        self.selection = Selection::Text {
            shape,
            anchor,
            caret,
        };
        self.drag = Some(Drag::Text { shape });
        self.touch();
    }

    /// The pointer moved with the button down.
    pub fn drag(&mut self, x: f64, y: f64, shift: bool) -> bool {
        let point = Point::new(x, y);
        let s = self.s();
        let Some(drag) = self.drag.clone() else {
            return false;
        };
        match drag {
            Drag::Pending { shape, from, .. } => {
                if (point - from).hypot() <= CLICK_SLOP * s {
                    return false;
                }
                let slide = self.presentation.slides[self.current].clone();
                let frame = slide.shapes[shape].frame;
                self.drag = Some(Drag::Move {
                    shape,
                    from,
                    frame,
                    before: Box::new(slide),
                });
                self.drag(x, y, shift)
            }
            Drag::Move {
                shape, from, frame, ..
            } => {
                let (_, per) = self.page();
                let mut delta = (point - from) / per;
                if shift {
                    // Along one axis only.
                    if delta.x.abs() > delta.y.abs() {
                        delta.y = 0.0;
                    } else {
                        delta.x = 0.0;
                    }
                }
                let moved = Frame {
                    x: frame.x + delta.x.round() as i64,
                    y: frame.y + delta.y.round() as i64,
                    ..frame
                };
                self.set_frame(shape, moved)
            }
            Drag::Resize {
                shape,
                handle,
                from,
                frame,
                ..
            } => {
                let (_, per) = self.page();
                let Some(found) = self.presentation.slides[self.current].shapes.get(shape) else {
                    return false;
                };
                // The pointer's movement in the shape's own (unrotated) axes.
                let (_, transform) = self.placed(found);
                let linear = Affine::new({
                    let c = transform.as_coeffs();
                    [c[0], c[1], c[2], c[3], 0.0, 0.0]
                });
                let delta = linear.inverse() * Point::new(point.x - from.x, point.y - from.y);
                let delta = Vec2::new(delta.x / per, delta.y / per);
                let line = found.geometry == Geometry::Line;
                let resized = resize(frame, handle, delta, shift, line);
                self.set_frame(shape, resized)
            }
            Drag::Text { shape } => {
                let Some(at) = self.text_at(shape, point) else {
                    return false;
                };
                if let Selection::Text { caret, .. } = &mut self.selection
                    && *caret != at
                {
                    *caret = at;
                    self.touch();
                    return true;
                }
                false
            }
            Drag::Slide { from, moving, .. } => {
                let start_y = match &self.drag {
                    Some(Drag::Slide { y: start, .. }) if !moving => *start,
                    _ => f64::NAN,
                };
                let moving = moving || (y - start_y).abs() > 4.0 * s;
                self.drag = Some(Drag::Slide {
                    from,
                    y: if moving { y } else { start_y },
                    moving,
                });
                self.touch();
                moving
            }
        }
    }

    /// Set a shape's frame while dragging, outside the history.
    fn set_frame(&mut self, shape: usize, frame: Frame) -> bool {
        let index = self.current;
        let slide = &mut Arc::make_mut(&mut self.presentation).slides[index];
        let Some(found) = slide.shapes.get_mut(shape) else {
            return false;
        };
        if found.frame == frame {
            return false;
        }
        found.frame = frame;
        self.forget(Some(index));
        true
    }

    /// The button went up.
    pub fn release(&mut self) -> bool {
        let Some(drag) = self.drag.take() else {
            return false;
        };
        match drag {
            Drag::Pending {
                shape,
                was_selected,
                text_at,
                ..
            } => {
                let Some(found) = self.presentation.slides[self.current].shapes.get(shape) else {
                    return false;
                };
                // A click on text puts the caret there: at once on text
                // boxes and placeholders, on other shapes once selected.
                let texty = found.takes_text()
                    && (was_selected
                        || found.placeholder.is_some()
                        || found.fill.is_none() && found.line.is_none());
                match text_at {
                    Some(at) if texty => {
                        self.selection = Selection::Text {
                            shape,
                            anchor: at,
                            caret: at,
                        };
                        self.touch();
                        true
                    }
                    _ => false,
                }
            }
            Drag::Move { before, .. } | Drag::Resize { before, .. } => {
                let index = self.current;
                let after = self.presentation.slides[index].clone();
                if after == *before {
                    return false;
                }
                let selection = self.selection.clone();
                self.push(Step {
                    change: Change::Slide(index, before, Box::new(after)),
                    before: (index, selection.clone()),
                    after: (index, selection),
                    typing: None,
                });
                true
            }
            Drag::Text { .. } => false,
            Drag::Slide { from, y, moving } => {
                self.touch();
                if moving {
                    let slot = self.drop_slot(y);
                    let to = if slot > from { slot - 1 } else { slot };
                    return self.move_slide(from, to);
                }
                true
            }
        }
    }

    /// Where a dragged slide would go, while one is dragged.
    pub(super) fn drop_indicator(&self) -> Option<usize> {
        match &self.drag {
            Some(Drag::Slide {
                y, moving: true, ..
            }) => Some(self.drop_slot(*y)),
            _ => None,
        }
    }

    /// The pointer's look over a point.
    pub fn cursor_at(&self, x: f64, y: f64) -> Cursor {
        if self.presenting || x < self.strip_width() {
            return Cursor::Default;
        }
        let point = Point::new(x, y);
        if let Some((_, handle)) = self.handle_at(point) {
            return handle.cursor();
        }
        if let Selection::Text { shape, .. } = self.selection
            && self.inside_text(shape, point)
        {
            return Cursor::Text;
        }
        match self.shape_at(point) {
            Some(shape) => {
                let found = &self.presentation.slides[self.current].shapes[shape];
                let texty = found.takes_text()
                    && (found.placeholder.is_some()
                        || found.fill.is_none() && found.line.is_none());
                if texty { Cursor::Text } else { Cursor::Move }
            }
            None => Cursor::Default,
        }
    }

    // --- Typing ---

    /// Type text: into the text with the caret, or in place of a selected
    /// shape's text. Returns whether anything changed.
    pub fn type_text(&mut self, text: &str) -> bool {
        if text.is_empty() {
            return false;
        }
        if !self.begin_typing() {
            return false;
        }
        let Selection::Text { shape, .. } = self.selection else {
            return false;
        };
        let text = text.to_owned();
        self.goal_x = None;
        self.edit(Some(shape), move |slide, selection, _| {
            let Selection::Text { anchor, caret, .. } = selection.clone() else {
                return false;
            };
            let Some(body) = slide.shapes.get_mut(shape).and_then(|s| s.text.as_mut()) else {
                return false;
            };
            let at = body.delete(anchor, caret);
            let at = body.insert(at, &text);
            *selection = Selection::Text {
                shape,
                anchor: at,
                caret: at,
            };
            true
        })
    }

    /// With a shape selected, typing replaces its text: select all of it.
    fn begin_typing(&mut self) -> bool {
        match self.selection {
            Selection::Text { .. } => true,
            Selection::Shape(shape) => {
                let index = self.current;
                let Some(found) = self.presentation.slides[index].shapes.get(shape) else {
                    return false;
                };
                if !found.takes_text() {
                    return false;
                }
                if found.text.is_none() {
                    // A shape without text gets a body as PowerPoint's shapes
                    // have: centred, in the shape's text colour.
                    let look = self.presentation.slides[index].look.clone();
                    let slide = &mut Arc::make_mut(&mut self.presentation).slides[index];
                    slide.shapes[shape].text = Some(shape_body(&look, false));
                }
                let body = self.presentation.slides[index].shapes[shape]
                    .text
                    .clone()
                    .unwrap_or_default();
                self.selection = Selection::Text {
                    shape,
                    anchor: TextPos::default(),
                    caret: body.end(),
                };
                true
            }
            Selection::None => false,
        }
    }

    /// An input method's event. Returns whether anything shown changed.
    pub fn ime(&mut self, event: ImeEvent) -> bool {
        match event {
            ImeEvent::Preedit { text, cursor } => {
                if text.is_empty() {
                    return self.preedit.take().is_some() && {
                        self.touch();
                        true
                    };
                }
                if self.preedit.is_none() {
                    if !self.begin_typing() {
                        return false;
                    }
                    // A selection goes when the composition starts.
                    if let Selection::Text {
                        shape,
                        anchor,
                        caret,
                    } = self.selection.clone()
                        && anchor != caret
                    {
                        self.edit(Some(shape), |slide, selection, _| {
                            let Some(body) =
                                slide.shapes.get_mut(shape).and_then(|s| s.text.as_mut())
                            else {
                                return false;
                            };
                            let at = body.delete(anchor, caret);
                            *selection = Selection::Text {
                                shape,
                                anchor: at,
                                caret: at,
                            };
                            true
                        });
                    }
                }
                if !self.is_editing_text() {
                    return false;
                }
                self.preedit = Some((text, cursor));
                self.touch();
                true
            }
            ImeEvent::Commit(text) => {
                self.preedit = None;
                self.touch();
                self.type_text(&text) || true
            }
            ImeEvent::Enabled => false,
            ImeEvent::Disabled => {
                let had = self.preedit.take().is_some();
                if had {
                    self.touch();
                }
                had
            }
        }
    }

    /// A composition left open is committed as it stands.
    fn commit_preedit(&mut self) {
        if let Some((text, _)) = self.preedit.take() {
            self.type_text(&text);
        }
    }

    /// Leave the text being typed into; the shape stays selected.
    pub(super) fn leave_text(&mut self) {
        self.commit_preedit();
        if let Selection::Text { shape, .. } = self.selection {
            self.selection = Selection::Shape(shape);
            self.touch();
        }
    }

    /// Backspace (`forward` false) or Delete: the selected text, the
    /// character before or after the caret, the selected shape, or with the
    /// thumbnails in focus the current slide.
    pub fn delete(&mut self, forward: bool) -> bool {
        if self.strip_focus {
            return self.delete_slide();
        }
        match self.selection.clone() {
            Selection::Text {
                shape,
                anchor,
                caret,
            } => {
                self.goal_x = None;
                self.edit(None, move |slide, selection, _| {
                    let Some(body) = slide.shapes.get_mut(shape).and_then(|s| s.text.as_mut())
                    else {
                        return false;
                    };
                    let (from, to) = if anchor != caret {
                        (anchor, caret)
                    } else if forward {
                        match body.after(caret) {
                            Some(next) => (caret, next),
                            None => return false,
                        }
                    } else {
                        match body.before(caret) {
                            Some(previous) => (previous, caret),
                            // Backspace at the start of a list item takes
                            // its bullet away first.
                            None => {
                                let first = body.paragraphs.first_mut();
                                return match first {
                                    Some(p) if p.list.is_some() => {
                                        p.list = None;
                                        body.renumber();
                                        true
                                    }
                                    _ => false,
                                };
                            }
                        }
                    };
                    let at = body.delete(from, to);
                    *selection = Selection::Text {
                        shape,
                        anchor: at,
                        caret: at,
                    };
                    true
                })
            }
            Selection::Shape(_) => self.delete_shape(),
            Selection::None => false,
        }
    }

    /// Enter: a new paragraph; with `line_break`, a new line in this one.
    pub fn enter(&mut self, line_break: bool) -> bool {
        let Selection::Text {
            shape,
            anchor,
            caret,
        } = self.selection.clone()
        else {
            return self.begin_typing() && {
                self.touch();
                true
            };
        };
        self.goal_x = None;
        self.edit(None, move |slide, selection, _| {
            let Some(body) = slide.shapes.get_mut(shape).and_then(|s| s.text.as_mut()) else {
                return false;
            };
            let at = body.delete(anchor, caret);
            let at = if line_break {
                body.insert(at, &LINE_BREAK.to_string())
            } else {
                body.split(at)
            };
            *selection = Selection::Text {
                shape,
                anchor: at,
                caret: at,
            };
            true
        })
    }

    /// Tab: at the start of a paragraph, or over several, one level deeper
    /// (shallower with Shift); elsewhere a tab character.
    pub fn tab(&mut self, shift: bool) -> bool {
        let Selection::Text { anchor, caret, .. } = self.selection else {
            return false;
        };
        if anchor.para != caret.para || caret.offset == 0 || shift {
            return self.format(TextFormat::Indent(!shift));
        }
        self.type_text("\t")
    }

    /// Esc: out of the text, then nothing selected.
    pub fn escape(&mut self) -> bool {
        self.strip_focus = false;
        match self.selection {
            Selection::Text { .. } => {
                self.preedit = None;
                self.leave_text();
                true
            }
            Selection::Shape(_) => {
                self.selection = Selection::None;
                self.touch();
                true
            }
            Selection::None => false,
        }
    }

    /// Select all of the text being typed into.
    pub fn select_all(&mut self) -> bool {
        let Selection::Text { shape, .. } = self.selection else {
            return false;
        };
        let Some(body) = self.body(shape) else {
            return false;
        };
        let end = body.end();
        self.selection = Selection::Text {
            shape,
            anchor: TextPos::default(),
            caret: end,
        };
        self.touch();
        true
    }

    fn body(&self, shape: usize) -> Option<&TextBody> {
        self.presentation
            .slides
            .get(self.current)?
            .shapes
            .get(shape)?
            .text
            .as_ref()
    }

    /// Move the caret; with `extend` the selection grows. `word` moves by
    /// words left and right.
    pub fn move_caret(&mut self, direction: CaretMove, extend: bool) -> bool {
        let Selection::Text {
            shape,
            anchor,
            caret,
        } = self.selection.clone()
        else {
            return false;
        };
        let Some(body) = self.body(shape).cloned() else {
            return false;
        };
        let (start, end) = (anchor.min(caret), anchor.max(caret));
        let mut vertical = false;
        let to = match direction {
            CaretMove::Left if !extend && anchor != caret => start,
            CaretMove::Right if !extend && anchor != caret => end,
            CaretMove::Left => body.before(caret).unwrap_or(caret),
            CaretMove::Right => body.after(caret).unwrap_or(caret),
            CaretMove::WordLeft => {
                // Back over spaces, then to the start of the word before.
                let mut at = caret;
                while let Some(previous) = body.before(at) {
                    if previous.para != at.para {
                        if at == caret {
                            at = previous;
                        }
                        break;
                    }
                    let gap = body.text_in(previous, at);
                    if !gap.chars().all(|c| c == ' ' || c == '\t') {
                        break;
                    }
                    at = previous;
                }
                match body.before(at) {
                    Some(previous) if previous.para == at.para => body.word_at(previous).0,
                    _ => at,
                }
            }
            CaretMove::WordRight => {
                let (_, word_end) = body.word_at(caret);
                let mut at = if word_end > caret {
                    word_end
                } else {
                    body.after(caret).unwrap_or(caret)
                };
                // And the spaces after it.
                while let Some(next) = body.after(at) {
                    if next.para != at.para
                        || !body
                            .text_in(at, next)
                            .chars()
                            .all(|c| c == ' ' || c == '\t')
                    {
                        break;
                    }
                    at = next;
                }
                at
            }
            CaretMove::Up | CaretMove::Down => {
                vertical = true;
                match self.vertical(shape, caret, direction == CaretMove::Down) {
                    Some(at) => at,
                    None if direction == CaretMove::Up => TextPos::default(),
                    None => body.end(),
                }
            }
            CaretMove::LineStart | CaretMove::LineEnd => self
                .line_edge(shape, caret, direction == CaretMove::LineEnd)
                .unwrap_or(caret),
            CaretMove::Start => TextPos::default(),
            CaretMove::End => body.end(),
        };
        if !vertical {
            self.goal_x = None;
        }
        let selection = Selection::Text {
            shape,
            anchor: if extend { anchor } else { to },
            caret: to,
        };
        if selection == self.selection {
            return false;
        }
        self.selection = selection;
        self.touch();
        true
    }

    /// The position a line up or down from `caret`, keeping to the same x.
    fn vertical(&mut self, shape: usize, caret: TextPos, down: bool) -> Option<TextPos> {
        let laid = self.text_layout(shape)?;
        let para = laid.paras.get(caret.para)?;
        let (x, y, h) = Self::caret_box(para, caret.offset);
        let goal = *self.goal_x.get_or_insert(x);
        let target_y = if down { y + h * 1.5 } else { y - h * 0.5 };
        let (index, target) = if down {
            if target_y < para.y + para.height {
                (caret.para, para)
            } else {
                let next = caret.para + 1;
                (next, laid.paras.get(next)?)
            }
        } else if target_y >= para.y {
            (caret.para, para)
        } else {
            let previous = caret.para.checked_sub(1)?;
            (previous, laid.paras.get(previous)?)
        };
        let local_y = if index == caret.para {
            target_y - target.y
        } else if down {
            1.0
        } else {
            target.height - 1.0
        };
        let offset = match &target.laid {
            Some(paragraph) => paragraph.offset_at((goal - target.x) as f32, local_y as f32),
            None => 0,
        };
        Some(TextPos::new(index, offset))
    }

    /// The start or end of the line the caret is on.
    fn line_edge(&mut self, shape: usize, caret: TextPos, end: bool) -> Option<TextPos> {
        let laid = self.text_layout(shape)?;
        let para = laid.paras.get(caret.para)?;
        let Some(paragraph) = &para.laid else {
            return Some(TextPos::new(caret.para, 0));
        };
        let ranges = paragraph.line_ranges();
        let range = ranges
            .iter()
            .find(|r| caret.offset >= r.start && caret.offset <= r.end)
            .or(ranges.last())?;
        Some(TextPos::new(
            caret.para,
            if end { range.end } else { range.start },
        ))
    }

    /// The selected text; with `cut` it is deleted.
    pub fn copy(&mut self, cut: bool) -> Option<String> {
        let Selection::Text {
            anchor,
            caret,
            shape,
        } = self.selection
        else {
            return None;
        };
        if anchor == caret {
            return None;
        }
        let text = self.body(shape)?.text_in(anchor, caret);
        if cut {
            self.delete(false);
        }
        Some(text)
    }

    pub fn paste(&mut self, text: &str) -> bool {
        let text = text.replace("\r\n", "\n").replace('\r', "\n");
        self.type_text(&text)
    }

    // --- Formatting ---

    /// What the toolbar acts on: the selected text, or all of a selected
    /// shape's.
    fn format_range(&self) -> Option<(usize, TextPos, TextPos)> {
        match self.selection {
            Selection::Text {
                shape,
                anchor,
                caret,
            } => Some((shape, anchor.min(caret), anchor.max(caret))),
            Selection::Shape(shape) => {
                let body = self.body(shape)?;
                Some((shape, TextPos::default(), body.end()))
            }
            Selection::None => None,
        }
    }

    /// How the text the toolbar acts on looks; `None` when there is none.
    pub fn look(&self) -> Option<TextLook> {
        let (shape, start, end) = self.format_range()?;
        let body = self.body(shape)?;
        let run = if start == end {
            body.run_at(start)?.clone()
        } else {
            // The first character of the range.
            let at = body.after(start).unwrap_or(start);
            body.run_at(at)?.clone()
        };
        let paragraph = body.paragraphs.get(start.para)?;
        Some(TextLook {
            bold: run.bold,
            italic: run.italic,
            underline: run.underline,
            strike: run.strike,
            size: run.size,
            color: run.color,
            align: paragraph.align,
            bullets: matches!(paragraph.list, Some(List::Char(_))),
            numbered: matches!(paragraph.list, Some(List::Number(..))),
        })
    }

    /// Apply a format to the text the toolbar acts on.
    pub fn format(&mut self, format: TextFormat) -> bool {
        self.commit_preedit();
        let Some((shape, start, end)) = self.format_range() else {
            return false;
        };
        let Some(look) = self.look() else {
            return false;
        };
        self.edit(None, move |slide, _, _| {
            let Some(body) = slide.shapes.get_mut(shape).and_then(|s| s.text.as_mut()) else {
                return false;
            };
            let before = body.clone();
            let levels = body.level_looks();
            match &format {
                TextFormat::Bold => body.format(start, end, |r| r.bold = !look.bold),
                TextFormat::Italic => body.format(start, end, |r| r.italic = !look.italic),
                TextFormat::Underline => body.format(start, end, |r| r.underline = !look.underline),
                TextFormat::Strike => body.format(start, end, |r| r.strike = !look.strike),
                TextFormat::Size(size) => {
                    let size = size.clamp(1.0, 4000.0);
                    body.format(start, end, |r| r.size = size)
                }
                TextFormat::SizeStep(up) => {
                    body.format(start, end, |r| r.size = size_step(r.size, *up))
                }
                TextFormat::Color(color) => {
                    let paragraphs = body.paragraphs.clone();
                    let mut para = start.para;
                    body.format(start, end, |r| {
                        let level = paragraphs
                            .get(para)
                            .map_or(0, |p| usize::from(p.level.min(8)));
                        r.color = color.or(levels[level].color);
                        let _ = &mut para;
                    })
                }
                TextFormat::Align(align) => {
                    for i in body.paragraphs_in(start, end) {
                        body.paragraphs[i].align = *align;
                    }
                }
                TextFormat::Bullets | TextFormat::Numbering => {
                    let numbers = format == TextFormat::Numbering;
                    let on = if numbers {
                        !look.numbered
                    } else {
                        !look.bullets
                    };
                    for i in body.paragraphs_in(start, end) {
                        let paragraph = &mut body.paragraphs[i];
                        let level_list = levels[usize::from(paragraph.level.min(8))].list.clone();
                        paragraph.list = match (on, numbers) {
                            (false, _) => None,
                            (true, true) => Some(List::Number("arabicPeriod".into(), 1)),
                            (true, false) => match level_list {
                                Some(list @ List::Char(_)) => Some(list),
                                _ => Some(List::Char("•".into())),
                            },
                        };
                        // A bulleted line hangs its bullet in a margin.
                        if on && paragraph.margin == 0 {
                            paragraph.margin = 342_900;
                            paragraph.indent = -342_900;
                        } else if !on
                            && paragraph.indent < 0
                            && levels[usize::from(paragraph.level.min(8))].list.is_none()
                        {
                            paragraph.margin = levels[usize::from(paragraph.level.min(8))].margin;
                            paragraph.indent = levels[usize::from(paragraph.level.min(8))].indent;
                        }
                    }
                    body.renumber();
                }
                TextFormat::Indent(deeper) => {
                    for i in body.paragraphs_in(start, end) {
                        let paragraph = &mut body.paragraphs[i];
                        let level = if *deeper {
                            (paragraph.level + 1).min(8)
                        } else {
                            paragraph.level.saturating_sub(1)
                        };
                        if level != paragraph.level {
                            paragraph.set_level(level, &levels);
                        }
                    }
                    body.renumber();
                }
            }
            *body != before
        })
    }

    // --- Shapes ---

    /// Insert a shape in the middle of the slide and select it; a text box
    /// gets the caret.
    pub fn insert_shape(&mut self, kind: NewShape) -> bool {
        self.commit_preedit();
        if self.count() == 0 {
            return false;
        }
        self.edit(None, |slide, selection, (w, h)| {
            let look = slide.look.clone();
            let (width, height) = match kind {
                NewShape::TextBox => (4.0 * INCH, 0.5 * INCH),
                NewShape::Line => (3.0 * INCH, 0.0),
                _ => (2.0 * INCH, 1.5 * INCH),
            };
            let (width, height) = (width as i64, height as i64);
            let frame = Frame::new((w - width) / 2, (h - height) / 2, width, height);
            let geometry = match kind {
                NewShape::TextBox | NewShape::Rect => Geometry::Rect,
                NewShape::RoundRect => Geometry::RoundRect,
                NewShape::Ellipse => Geometry::Ellipse,
                NewShape::Line => Geometry::Line,
            };
            let mut shape = Shape::new(frame, geometry, Role::Shape);
            match kind {
                NewShape::TextBox => {
                    let mut body = TextBody {
                        grow: true,
                        levels: Some(look.text.clone()),
                        ..TextBody::default()
                    };
                    body.ensure_paragraph();
                    shape.text = Some(body);
                }
                NewShape::Line => {
                    shape.line = Some(Outline {
                        color: look.accent,
                        width: 12_700,
                    });
                }
                _ => {
                    shape.fill = Some(Fill::Solid(look.accent));
                    shape.line = Some(Outline {
                        color: look.accent_line,
                        width: 12_700,
                    });
                    shape.text = Some(shape_body(&look, true));
                }
            }
            slide.shapes.push(shape);
            let index = slide.shapes.len() - 1;
            *selection = match kind {
                NewShape::TextBox => Selection::Text {
                    shape: index,
                    anchor: TextPos::default(),
                    caret: TextPos::default(),
                },
                _ => Selection::Shape(index),
            };
            true
        })
    }

    /// Insert a picture (PNG or JPEG bytes, `pixels` its size) in the middle
    /// of the slide, at most two thirds of it.
    pub fn insert_picture(&mut self, image: Arc<[u8]>, pixels: (u32, u32)) -> bool {
        self.commit_preedit();
        if self.count() == 0 {
            return false;
        }
        self.edit(None, |slide, selection, (w, h)| {
            // 96 pixels an inch.
            let (mut width, mut height) = (
                f64::from(pixels.0.max(1)) * 9_525.0,
                f64::from(pixels.1.max(1)) * 9_525.0,
            );
            let fit = (w as f64 * 2.0 / 3.0 / width)
                .min(h as f64 * 2.0 / 3.0 / height)
                .min(1.0);
            width *= fit;
            height *= fit;
            let (width, height) = (width.round() as i64, height.round() as i64);
            let frame = Frame::new((w - width) / 2, (h - height) / 2, width, height);
            let mut shape = Shape::new(frame, Geometry::Rect, Role::Picture);
            shape.picture = Some(Picture {
                image,
                crop: [0.0; 4],
            });
            slide.shapes.push(shape);
            *selection = Selection::Shape(slide.shapes.len() - 1);
            true
        })
    }

    /// Delete the selected shape.
    pub fn delete_shape(&mut self) -> bool {
        let Some(shape) = self.selection.shape() else {
            return false;
        };
        self.preedit = None;
        self.edit(None, |slide, selection, _| {
            if shape >= slide.shapes.len() || slide.shapes[shape].inherited {
                return false;
            }
            slide.shapes.remove(shape);
            *selection = Selection::None;
            true
        })
    }

    /// A copy of the selected shape, a little down and to the right.
    pub fn duplicate_shape(&mut self) -> bool {
        self.commit_preedit();
        let Some(shape) = self.selection.shape() else {
            return false;
        };
        self.edit(None, |slide, selection, _| {
            let Some(found) = slide.shapes.get(shape) else {
                return false;
            };
            let mut copy = found.clone();
            copy.frame.x += 228_600;
            copy.frame.y += 228_600;
            if let Some(source) = &mut copy.source {
                source.copy = true;
            }
            slide.shapes.push(copy);
            *selection = Selection::Shape(slide.shapes.len() - 1);
            true
        })
    }

    /// Bring the selected shape in front of all others, or behind them all
    /// (still in front of the layout's and master's).
    pub fn order_shape(&mut self, front: bool) -> bool {
        self.commit_preedit();
        let Some(shape) = self.selection.shape() else {
            return false;
        };
        self.edit(None, |slide, selection, _| {
            if shape >= slide.shapes.len() {
                return false;
            }
            let to = if front {
                slide.shapes.len() - 1
            } else {
                slide.shapes.iter().take_while(|s| s.inherited).count()
            };
            if to == shape {
                return false;
            }
            let moved = slide.shapes.remove(shape);
            slide.shapes.insert(to, moved);
            *selection = match selection.clone() {
                Selection::Text { anchor, caret, .. } => Selection::Text {
                    shape: to,
                    anchor,
                    caret,
                },
                _ => Selection::Shape(to),
            };
            true
        })
    }

    /// Move the selected shape by EMU, or with nothing selected change
    /// nothing.
    pub fn nudge(&mut self, dx: i64, dy: i64) -> bool {
        let Selection::Shape(shape) = self.selection else {
            return false;
        };
        self.edit(None, |slide, _, _| {
            let Some(found) = slide.shapes.get_mut(shape) else {
                return false;
            };
            found.frame.x += dx;
            found.frame.y += dy;
            true
        })
    }

    // --- Slides ---

    /// A new slide on layout `layout` after the current one.
    pub fn add_slide(&mut self, layout: usize) -> bool {
        let template = self
            .presentation
            .layouts
            .get(layout)
            .map(|l| l.slide.clone());
        let fallback = self
            .presentation
            .slides
            .get(self.current)
            .map(|s| s.look.clone())
            .unwrap_or_default();
        self.edit_deck(|slides, current| {
            let slide = template.unwrap_or_else(|| Slide {
                look: fallback,
                ..Slide::default()
            });
            let at = if slides.is_empty() { 0 } else { *current + 1 };
            slides.insert(at, slide);
            *current = at;
            true
        })
    }

    /// A copy of the current slide after it.
    pub fn duplicate_slide(&mut self) -> bool {
        self.edit_deck(|slides, current| {
            let Some(slide) = slides.get(*current) else {
                return false;
            };
            let mut copy = slide.clone();
            if let Some(source) = &copy.source {
                let mut source = (**source).clone();
                source.copy = true;
                copy.source = Some(Arc::new(source));
            }
            slides.insert(*current + 1, copy);
            *current += 1;
            true
        })
    }

    pub fn delete_slide(&mut self) -> bool {
        self.edit_deck(|slides, current| {
            if *current >= slides.len() {
                return false;
            }
            slides.remove(*current);
            *current = (*current).min(slides.len().saturating_sub(1));
            true
        })
    }

    /// Move slide `from` to be slide `to`.
    pub fn move_slide(&mut self, from: usize, to: usize) -> bool {
        if from == to {
            return false;
        }
        self.edit_deck(|slides, current| {
            if from >= slides.len() || to >= slides.len() {
                return false;
            }
            let slide = slides.remove(from);
            slides.insert(to, slide);
            *current = to;
            true
        })
    }

    /// Give the current slide these speaker notes; typing on joins one step.
    pub fn set_notes(&mut self, notes: &str) -> bool {
        let notes = (!notes.trim().is_empty()).then(|| notes.to_owned());
        if self.presentation.slides.get(self.current).map(|s| &s.notes) == Some(&notes) {
            return false;
        }
        self.edit(Some(usize::MAX), |slide, _, _| {
            slide.notes = notes;
            true
        })
    }

    // --- Slide show ---

    pub fn is_presenting(&self) -> bool {
        self.presenting
    }

    /// Start the show at the current slide.
    pub fn start_show(&mut self) {
        self.commit_preedit();
        self.presenting = true;
        self.drag = None;
        self.touch();
    }

    pub fn end_show(&mut self) {
        self.presenting = false;
        self.touch();
    }

    /// The next (or previous) slide not hidden. `None` past the end, which
    /// ends the show going forward.
    pub fn show_step(&mut self, forward: bool) -> Option<usize> {
        let slides = &self.presentation.slides;
        let found = if forward {
            (self.current + 1..slides.len()).find(|&i| !slides[i].hidden)
        } else {
            (0..self.current).rev().find(|&i| !slides[i].hidden)
        };
        match found {
            Some(index) => {
                self.go_to(index);
                Some(index)
            }
            None if forward => {
                self.end_show();
                None
            }
            None => None,
        }
    }

    // --- Drawing ---

    /// The shape being typed into, laid out with its composition.
    pub(super) fn edited_layout(&mut self, _page: Rect) -> Option<LaidShape> {
        let Selection::Text { shape, .. } = self.selection else {
            return None;
        };
        self.text_layout(shape)
    }

    /// Prompts in empty placeholders, the selection's frame and handles,
    /// selected text and the caret.
    pub(super) fn draw_editing(&mut self, page: Rect) {
        let s = self.s();
        let presentation = self.presentation.clone();
        let Some(slide) = presentation.slides.get(self.current) else {
            return;
        };
        let per = page.width() / presentation.size.0.max(1) as f64;
        let editing = match self.selection {
            Selection::Text { shape, .. } => Some(shape),
            _ => None,
        };
        // Empty placeholders: a dotted frame and their prompt.
        for (i, shape) in slide.shapes.iter().enumerate() {
            let Some(placeholder) = &shape.placeholder else {
                continue;
            };
            if shape.inherited || !shape.text.as_ref().is_some_and(TextBody::is_empty) {
                continue;
            }
            let rect = Self::frame_rect(&shape.frame, page, per);
            let transform = shape_transform(shape, rect);
            self.ctx.set_transform(transform);
            self.ctx.set_paint(color(self.style.secondary));
            self.ctx
                .set_stroke(Stroke::new(s.max(1.0)).with_dashes(0.0, [3.0 * s, 3.0 * s]));
            self.ctx.stroke_path(&rect.to_path(0.1));
            self.ctx.set_transform(Affine::IDENTITY);
            if editing == Some(i) {
                continue;
            }
            let prompt = match placeholder.kind.as_str() {
                "title" | "ctrTitle" => self.style.prompt_title.clone(),
                "subTitle" => self.style.prompt_subtitle.clone(),
                _ => self.style.prompt_text.clone(),
            };
            let body = shape.text.as_ref().expect("checked above");
            let paragraph = body.paragraphs.first();
            let size = paragraph.map_or(18.0, |p| p.end.size);
            let size = (f64::from(size) * aster_slides::EMU_PER_POINT * per / s) as f32;
            let align = match paragraph.map(|p| p.align) {
                Some(SlideAlign::Center) => Align::Center,
                Some(SlideAlign::Right) => Align::End,
                _ => Align::Start,
            };
            let mut laid = simple(
                &mut self.shaper,
                &prompt,
                size.max(1.0),
                [0x80, 0x80, 0x80, 0xff],
                self.scale,
                align,
            );
            let [left, top, right, bottom] = body.insets.map(|v| v as f64 * per);
            let room = (rect.width() - left - right).max(1.0);
            if f64::from(laid.width()) > room {
                laid = self.shaper.lay_out(&aster_text::Spec {
                    text: &prompt,
                    base: aster_text::Style {
                        family: aster_text::Family::SystemUi,
                        size: size.max(1.0),
                        color: [0x80, 0x80, 0x80, 0xff],
                        bold: false,
                        italic: false,
                        underline: false,
                        strike: false,
                        rise: 0.0,
                    },
                    spans: &[],
                    language: aster_text::segment::guess_language(
                        &prompt,
                        crate::text_surface::DEFAULT_LANGUAGE,
                    ),
                    width: Some(room as f32),
                    scale: self.scale,
                    line_height: 1.2,
                    align,
                    rules: aster_text::Rules::for_language("en"),
                });
            }
            let height = f64::from(laid.height());
            let y = match body.anchor {
                Anchor::Top => rect.y0 + top,
                Anchor::Middle => rect.y0 + top + (rect.height() - top - bottom - height) / 2.0,
                Anchor::Bottom => rect.y1 - bottom - height,
            };
            let x = match align {
                Align::Start => rect.x0 + left,
                Align::Center => rect.x0 + left + (room - f64::from(laid.width())) / 2.0,
                Align::End => rect.x1 - right - f64::from(laid.width()),
            };
            self.ctx
                .set_transform(transform * Affine::translate((x, y)));
            super::draw_text(
                &mut self.ctx,
                &mut self.resources,
                &laid,
                f32::MIN,
                f32::MAX,
                self.scale,
                &mut self.glyphs,
            );
            self.ctx.set_transform(Affine::IDENTITY);
        }
        let Some(shape_index) = self.selection.shape() else {
            return;
        };
        let Some(shape) = slide.shapes.get(shape_index) else {
            return;
        };
        let rect = Self::frame_rect(&shape.frame, page, per);
        let transform = shape_transform(shape, rect);
        let accent = self.style.accent;
        // Selected text and the caret.
        if let Selection::Text { anchor, caret, .. } = self.selection.clone()
            && let Some(laid) = self.text_layout(shape_index)
        {
            let (start, end) = (anchor.min(caret), anchor.max(caret));
            if start != end {
                let mut highlight = accent;
                highlight[3] = 0x50;
                self.ctx.set_paint(color(highlight));
                for (i, para) in laid.paras.iter().enumerate() {
                    if i < start.para || i > end.para {
                        continue;
                    }
                    let Some(paragraph) = &para.laid else {
                        continue;
                    };
                    let from = if i == start.para { start.offset } else { 0 };
                    let to = if i == end.para {
                        end.offset
                    } else {
                        paragraph.text().len() + 1
                    };
                    for [x0, y0, x1, y1] in paragraph.rects(from..to.min(paragraph.text().len())) {
                        self.ctx.set_transform(transform);
                        self.ctx.fill_rect(&Rect::new(
                            rect.x0 + para.x + f64::from(x0),
                            rect.y0 + para.y + f64::from(y0),
                            rect.x0 + para.x + f64::from(x1),
                            rect.y0 + para.y + f64::from(y1),
                        ));
                    }
                }
            }
            let shown = self.shown_caret(caret);
            if let Some(para) = laid.paras.get(shown.para) {
                let (x, y, h) = Self::caret_box(para, shown.offset);
                let bar = Rect::new(
                    rect.x0 + x,
                    rect.y0 + y,
                    rect.x0 + x + s.max(1.0),
                    rect.y0 + y + h,
                );
                if self.caret_on {
                    self.ctx.set_transform(transform);
                    self.ctx.set_paint(color(self.style.text_caret()));
                    self.ctx.fill_rect(&bar);
                }
                let top_left = transform * Point::new(bar.x0, bar.y0);
                self.ime_area = (top_left.x, top_left.y, bar.width(), bar.height());
            }
            self.ctx.set_transform(Affine::IDENTITY);
        }
        // The frame and its handles.
        self.ctx.set_transform(transform);
        self.ctx.set_paint(color(accent));
        let line = (1.5 * s).round().max(1.0);
        self.ctx.set_stroke(if self.is_editing_text() {
            Stroke::new(line).with_dashes(0.0, [4.0 * s, 2.0 * s])
        } else {
            Stroke::new(line)
        });
        if shape.geometry == Geometry::Line {
            self.ctx.stroke_path(
                &vello_cpu::kurbo::Line::new((rect.x0, rect.y0), (rect.x1, rect.y1)).to_path(0.1),
            );
        } else {
            self.ctx.stroke_rect(&rect);
        }
        self.ctx.set_transform(Affine::IDENTITY);
        if shape.role != Role::Table {
            let half = HANDLE * s / 2.0;
            for (_, at) in self.handles(shape) {
                let square = Rect::new(at.x - half, at.y - half, at.x + half, at.y + half);
                self.ctx.set_paint(color([0xff, 0xff, 0xff, 0xff]));
                self.ctx.fill_rect(&square);
                self.ctx.set_paint(color(accent));
                self.ctx.set_stroke(Stroke::new(s.max(1.0)));
                self.ctx.stroke_rect(&square);
            }
        }
    }
}

/// Caret movements.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CaretMove {
    Left,
    Right,
    WordLeft,
    WordRight,
    Up,
    Down,
    LineStart,
    LineEnd,
    Start,
    End,
}

/// The text body of a new autoshape: centred, in the shape's text colour.
fn shape_body(look: &aster_slides::SlideLook, with_paragraph: bool) -> TextBody {
    let mut levels = (*look.text).clone();
    for level in &mut levels {
        level.color = Some(look.on_accent);
        level.align = SlideAlign::Center;
        level.list = None;
        level.margin = 0;
        level.indent = 0;
    }
    let mut body = TextBody {
        anchor: Anchor::Middle,
        levels: Some(Arc::new(levels)),
        ..TextBody::default()
    };
    if with_paragraph {
        body.ensure_paragraph();
    }
    body
}

/// The next size up or down the usual list.
fn size_step(size: f32, up: bool) -> f32 {
    if up {
        SIZES.into_iter().find(|s| *s > size).unwrap_or(size + 12.0)
    } else {
        SIZES
            .into_iter()
            .rev()
            .find(|s| *s < size)
            .unwrap_or((size - 1.0).max(1.0))
    }
}

/// A frame resized by dragging `handle` by `delta` EMU (in the shape's own
/// axes). `keep` keeps its proportions; lines may be flat.
fn resize(frame: Frame, handle: Handle, delta: Vec2, keep: bool, line: bool) -> Frame {
    let (fx, fy) = handle.at();
    let (mut x0, mut y0) = (frame.x as f64, frame.y as f64);
    let (mut x1, mut y1) = (x0 + frame.width as f64, y0 + frame.height as f64);
    if fx == 0.0 {
        x0 += delta.x;
    } else if fx == 1.0 {
        x1 += delta.x;
    }
    if fy == 0.0 {
        y0 += delta.y;
    } else if fy == 1.0 {
        y1 += delta.y;
    }
    let least = if line { 0.0 } else { 12_700.0 };
    if x1 - x0 < least {
        if fx == 0.0 {
            x0 = x1 - least;
        } else {
            x1 = x0 + least;
        }
    }
    if y1 - y0 < least {
        if fy == 0.0 {
            y0 = y1 - least;
        } else {
            y1 = y0 + least;
        }
    }
    let corner = fx != 0.5 && fy != 0.5;
    if keep && corner && frame.width > 0 && frame.height > 0 {
        let ratio = frame.width as f64 / frame.height as f64;
        let (w, h) = (x1 - x0, y1 - y0);
        let (w, h) = if w / ratio > h {
            (w, w / ratio)
        } else {
            (h * ratio, h)
        };
        if fx == 0.0 {
            x0 = x1 - w;
        } else {
            x1 = x0 + w;
        }
        if fy == 0.0 {
            y0 = y1 - h;
        } else {
            y1 = y0 + h;
        }
    }
    Frame::new(
        x0.round() as i64,
        y0.round() as i64,
        (x1 - x0).round() as i64,
        (y1 - y0).round() as i64,
    )
}

fn distance_to_segment(p: Point, a: Point, b: Point) -> f64 {
    let ab = b - a;
    let length = ab.hypot2();
    if length == 0.0 {
        return (p - a).hypot();
    }
    let t = ((p - a).dot(ab) / length).clamp(0.0, 1.0);
    (p - (a + ab * t)).hypot()
}

impl super::SlideStyle {
    /// The caret's colour on a slide, which is drawn in its own colours.
    fn text_caret(&self) -> [u8; 4] {
        [0x10, 0x10, 0x10, 0xff]
    }
}

use aster_slides::Presentation;

#[cfg(test)]
mod tests {
    use super::*;

    const THUMB_GAP_FOR_TESTS: f64 = 14.0;

    const NAMES: [&str; 5] = [
        "Title Slide",
        "Title and Content",
        "Section Header",
        "Title Only",
        "Blank",
    ];

    fn view() -> SlideView {
        let bytes = aster_ooxml::pptx_save::blank::presentation(&NAMES);
        let presentation = aster_ooxml::pptx::read(bytes).unwrap();
        let mut view = SlideView::new(Arc::new(presentation));
        view.resize(1600, 1000, 1.0);
        view.render();
        view
    }

    /// Where a point of the slide (EMU) is on screen.
    fn at(view: &SlideView, x: i64, y: i64) -> (f64, f64) {
        let (page, per) = view.page();
        (page.x0 + x as f64 * per, page.y0 + y as f64 * per)
    }

    fn click(view: &mut SlideView, x: i64, y: i64) {
        let (x, y) = at(view, x, y);
        view.press(x, y, false, 1);
        view.release();
    }

    fn title(view: &SlideView) -> String {
        view.presentation().slides[view.current()].shapes[0]
            .text
            .as_ref()
            .unwrap()
            .plain_text()
    }

    #[test]
    fn a_click_puts_the_caret_in_a_placeholder_and_compositions_go_in() {
        let mut view = view();
        assert!(!view.is_modified());
        // The middle of the title placeholder.
        click(&mut view, 6_000_000, 2_300_000);
        assert!(view.is_editing_text());
        assert!(view.ime(ImeEvent::Preedit {
            text: "とうきょう".into(),
            cursor: Some((3, 3)),
        }));
        assert!(view.is_composing());
        // Not typed yet: still the file's text, and the caret is drawn.
        assert_eq!(title(&view), "");
        view.render();
        let (x, y, _, h) = view.ime_cursor_area();
        assert!(x > 0.0 && y > 0.0 && h > 0.0);
        view.ime(ImeEvent::Commit("東京".into()));
        assert_eq!(title(&view), "東京");
        assert!(view.type_text("タワー"));
        assert!(view.type_text("!"));
        assert_eq!(title(&view), "東京タワー!");
        assert!(view.is_modified());
        // Typing joins one step.
        assert!(view.undo());
        assert_eq!(title(&view), "");
        assert!(!view.is_modified());
        assert!(view.redo());
        assert_eq!(title(&view), "東京タワー!");
        // Enter, Backspace, a line break.
        view.enter(false);
        view.type_text("second");
        assert_eq!(title(&view), "東京タワー!\nsecond");
        view.move_caret(CaretMove::LineStart, false);
        view.delete(false);
        assert_eq!(title(&view), "東京タワー!second");
        view.enter(true);
        let body = view.presentation().slides[0].shapes[0]
            .text
            .clone()
            .unwrap();
        assert_eq!(body.paragraphs.len(), 1);
        assert_eq!(body.paragraphs[0].text(), "東京タワー!\nsecond");
        // Selecting and replacing.
        view.select_all();
        assert_eq!(view.copy(false).as_deref(), Some("東京タワー!\nsecond"));
        view.type_text("x");
        assert_eq!(title(&view), "x");
        assert!(view.escape());
        assert_eq!(view.selection(), &Selection::Shape(0));
    }

    #[test]
    fn shapes_move_resize_and_order_with_undo() {
        let mut view = view();
        assert!(view.insert_shape(NewShape::Rect));
        let index = view.selection().shape().unwrap();
        let frame = view.presentation().slides[0].shapes[index].frame;
        // Drag from its middle.
        let middle = (frame.x + frame.width / 2, frame.y + frame.height / 2);
        let (x, y) = at(&view, middle.0, middle.1);
        view.press(x, y, false, 1);
        view.drag(x + 100.0, y + 50.0, false);
        assert!(view.release());
        let moved = view.presentation().slides[0].shapes[index].frame;
        assert!(moved.x > frame.x && moved.y > frame.y);
        assert_eq!((moved.width, moved.height), (frame.width, frame.height));
        // The bottom right handle with Shift: twice as wide, still 4:3.
        let (x, y) = at(&view, moved.x + moved.width, moved.y + moved.height);
        view.press(x, y, true, 1);
        let (_, per) = view.page();
        view.drag(x + frame.width as f64 * per, y, true);
        view.release();
        let resized = view.presentation().slides[0].shapes[index].frame;
        assert!(
            (resized.width - 2 * frame.width).abs() < 20_000,
            "{resized:?}"
        );
        assert!(
            (resized.height * 4 - resized.width * 3).abs() < 40_000,
            "{resized:?}"
        );
        assert!(view.undo());
        assert_eq!(view.presentation().slides[0].shapes[index].frame, moved);
        assert!(view.undo());
        assert_eq!(view.presentation().slides[0].shapes[index].frame, frame);
        // To the back: behind the placeholders.
        view.selection = Selection::Shape(index);
        assert!(view.order_shape(false));
        assert_eq!(
            view.presentation().slides[0].shapes[0].geometry,
            Geometry::Rect
        );
        assert!(view.duplicate_shape());
        assert_eq!(view.presentation().slides[0].shapes.len(), 4);
        assert!(view.delete(false));
        assert_eq!(view.presentation().slides[0].shapes.len(), 3);
    }

    #[test]
    fn text_boxes_grow_and_formats_apply_to_the_selection() {
        let mut view = view();
        assert!(view.insert_shape(NewShape::TextBox));
        assert!(view.is_editing_text());
        let index = view.selection().shape().unwrap();
        let before = view.presentation().slides[0].shapes[index].frame.height;
        view.type_text("one");
        view.enter(false);
        view.type_text("two");
        let after = view.presentation().slides[0].shapes[index].frame.height;
        assert!(after > before, "{before} {after}");
        view.select_all();
        assert!(view.format(TextFormat::Bold));
        assert!(view.format(TextFormat::SizeStep(true)));
        assert!(view.format(TextFormat::Numbering));
        let body = view.presentation().slides[0].shapes[index]
            .text
            .clone()
            .unwrap();
        assert!(
            body.paragraphs
                .iter()
                .all(|p| p.runs[0].bold && p.runs[0].size == 20.0)
        );
        assert_eq!(body.paragraphs[1].bullet.as_deref(), Some("2."));
        let look = view.look().unwrap();
        assert!(look.bold && look.numbered);
        assert!(view.format(TextFormat::Indent(true)));
        let body = view.presentation().slides[0].shapes[index]
            .text
            .clone()
            .unwrap();
        assert_eq!(body.paragraphs[0].level, 1);
    }

    #[test]
    fn slides_are_added_copied_moved_and_deleted_with_undo() {
        let mut view = view();
        assert!(view.add_slide(1));
        assert_eq!((view.count(), view.current()), (2, 1));
        assert!(view.duplicate_slide());
        assert_eq!((view.count(), view.current()), (3, 2));
        assert!(view.presentation().slides[2].source.is_none());
        assert!(view.move_slide(2, 0));
        assert_eq!(view.current(), 0);
        assert!(view.delete_slide());
        assert_eq!(view.count(), 2);
        assert!(view.set_notes("メモ"));
        assert!(view.set_notes("メモです"));
        assert_eq!(
            view.presentation().slides[0].notes.as_deref(),
            Some("メモです")
        );
        assert!(view.undo());
        assert_eq!(view.presentation().slides[0].notes, None);
        for _ in 0..4 {
            assert!(view.undo());
        }
        assert_eq!(view.count(), 1);
        assert!(!view.is_modified());
        // A thumbnail dragged below the next one moves there.
        assert!(view.add_slide(1));
        let pitch = view.thumb_pitch();
        let y0 = THUMB_GAP_FOR_TESTS + pitch * 0.5;
        view.press(100.0, y0, false, 1);
        view.drag(100.0, y0 + pitch * 1.2, false);
        assert!(view.release());
        assert_eq!(view.current(), 1);
        assert_eq!(view.presentation().slides[1].layout, Some(0));
    }

    #[test]
    fn the_show_skips_hidden_slides_and_ends() {
        let mut view = view();
        view.add_slide(1);
        view.add_slide(1);
        Arc::make_mut(&mut view.presentation).slides[1].hidden = true;
        view.go_to(0);
        view.start_show();
        assert!(view.is_presenting());
        assert_eq!(view.show_step(true), Some(2));
        assert_eq!(view.show_step(false), Some(0));
        view.go_to(2);
        assert_eq!(view.show_step(true), None);
        assert!(!view.is_presenting());
    }

    #[test]
    fn resizing_keeps_proportions_and_a_least_size() {
        let frame = Frame::new(0, 0, 200_000, 100_000);
        let wider = resize(
            frame,
            Handle::Right,
            Vec2::new(100_000.0, 50.0),
            false,
            false,
        );
        assert_eq!(wider, Frame::new(0, 0, 300_000, 100_000));
        let kept = resize(
            frame,
            Handle::BottomRight,
            Vec2::new(200_000.0, 0.0),
            true,
            false,
        );
        assert_eq!(kept, Frame::new(0, 0, 400_000, 200_000));
        let from_left = resize(
            frame,
            Handle::TopLeft,
            Vec2::new(-200_000.0, 0.0),
            true,
            false,
        );
        assert_eq!(from_left, Frame::new(-200_000, -100_000, 400_000, 200_000));
        let crushed = resize(frame, Handle::Left, Vec2::new(500_000.0, 0.0), false, false);
        assert_eq!(crushed.width, 12_700);
        assert_eq!(size_step(18.0, true), 20.0);
        assert_eq!(size_step(8.0, false), 7.0);
    }
}
