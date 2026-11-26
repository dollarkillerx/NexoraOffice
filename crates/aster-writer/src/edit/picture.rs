//! Pictures: putting an image in the text and changing the size of a
//! drawing.
//!
//! Nothing here decodes an image. The caller reads the pixel size (to work
//! out the size on the page) and hands the file's bytes over; the format is
//! known from the first bytes, because saving writes the bytes into the
//! document as they are and a format Word cannot open must not get there.

use std::sync::Arc;

use super::{insert_index, merge_runs, snap, split_run};
use crate::{
    Document, Drawing, DrawingContent, Float, FloatAt, FloatAxis, FloatFrom, Inline, Paragraph,
    Picture, Pos, Run, Wrap,
};

/// The most a drawing is wide or high, in EMU (about 220 metres, as much as
/// the reader takes at its word).
const MAX_EMU: i64 = 200_000_000;

/// The image formats a Word document can hold as a picture and that are
/// written as they are.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ImageFormat {
    Png,
    Jpeg,
    Gif,
    Bmp,
    Tiff,
}

impl ImageFormat {
    /// The file name extension, lower case, without the dot.
    pub fn extension(self) -> &'static str {
        match self {
            ImageFormat::Png => "png",
            ImageFormat::Jpeg => "jpeg",
            ImageFormat::Gif => "gif",
            ImageFormat::Bmp => "bmp",
            ImageFormat::Tiff => "tiff",
        }
    }

    /// The media type.
    pub fn content_type(self) -> &'static str {
        match self {
            ImageFormat::Png => "image/png",
            ImageFormat::Jpeg => "image/jpeg",
            ImageFormat::Gif => "image/gif",
            ImageFormat::Bmp => "image/bmp",
            ImageFormat::Tiff => "image/tiff",
        }
    }
}

/// The format of an image file by its first bytes; `None` for anything that
/// is not PNG, JPEG, GIF, BMP or TIFF.
pub fn image_format(bytes: &[u8]) -> Option<ImageFormat> {
    if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
        Some(ImageFormat::Png)
    } else if bytes.starts_with(&[0xFF, 0xD8, 0xFF]) {
        Some(ImageFormat::Jpeg)
    } else if bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a") {
        Some(ImageFormat::Gif)
    } else if bytes.starts_with(b"BM") && bytes.len() >= 26 {
        Some(ImageFormat::Bmp)
    } else if bytes.starts_with(b"II*\0") || bytes.starts_with(b"MM\0*") {
        Some(ImageFormat::Tiff)
    } else {
        None
    }
}

/// Why a picture was not put in.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PictureError {
    /// Not PNG, JPEG, GIF, BMP or TIFF.
    UnknownFormat,
    /// A width or height of 0 or less.
    BadSize,
    /// The position leads to no paragraph.
    BadPosition,
}

impl std::fmt::Display for PictureError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            PictureError::UnknownFormat => "the image is not PNG, JPEG, GIF, BMP or TIFF",
            PictureError::BadSize => "the picture has no size",
            PictureError::BadPosition => "there is no paragraph at that place",
        })
    }
}

impl std::error::Error for PictureError {}

fn clamped(size: (i64, i64)) -> Option<(i64, i64)> {
    (size.0 > 0 && size.1 > 0).then(|| (size.0.min(MAX_EMU), size.1.min(MAX_EMU)))
}

/// Put a picture in the line at `at`: an inline drawing with the image's
/// bytes as given, `size` (width and height in EMU) on the page, and `alt`
/// as its description. Where the caret goes: after it (the picture is one
/// object character, [`crate::OBJECT`]). The picture is no link and no
/// character style; its look is the paragraph's base look at the text size
/// of the place it goes into, which gives the line its height.
///
/// Errors, with nothing changed: the bytes are not a PNG, JPEG, GIF, BMP or
/// TIFF file ([`image_format`]), the size is not positive, or the path
/// leads to no paragraph. A size beyond about 220 metres is cut to that.
pub fn insert_picture(
    doc: &mut Document,
    at: &Pos,
    image: Arc<[u8]>,
    size: (i64, i64),
    alt: Option<String>,
) -> Result<Pos, PictureError> {
    if image_format(&image).is_none() {
        return Err(PictureError::UnknownFormat);
    }
    let size = clamped(size).ok_or(PictureError::BadSize)?;
    let p = doc
        .paragraph_mut(&at.path)
        .ok_or(PictureError::BadPosition)?;
    let offset = snap(p, at.offset, false);
    let mut look = p.base.clone();
    look.size = p.look_at(offset).size;
    let drawing = Drawing {
        size,
        content: DrawingContent::Picture(Picture {
            image,
            crop: [0.0; 4],
        }),
        float: None,
        alt: alt.filter(|text| !text.is_empty()),
    };
    let run = Run::object(Inline::Drawing(Arc::new(drawing)), look);
    let width = run.text.len();
    let index = insert_index(p, offset);
    p.runs.insert(index, run);
    Ok(Pos::new(at.path.clone(), offset + width))
}

/// The index of the run of `p` that is a drawing starting at `offset`.
fn drawing_run(p: &Paragraph, offset: usize) -> Option<usize> {
    let mut start = 0;
    for (i, run) in p.runs.iter().enumerate() {
        if start == offset && matches!(run.kind, Inline::Drawing(_)) {
            return Some(i);
        }
        if start > offset {
            return None;
        }
        start += run.text.len();
    }
    None
}

/// The drawing that starts at `at` (the position just before its object
/// character).
pub fn drawing_at<'a>(doc: &'a Document, at: &Pos) -> Option<&'a Drawing> {
    let p = doc.paragraph(&at.path)?;
    match &p.runs[drawing_run(p, at.offset)?].kind {
        Inline::Drawing(drawing) => Some(drawing),
        _ => None,
    }
}

/// `f` on the drawing that starts at `at`; whether `f` said it changed
/// something. A drawing shared with other copies of the document is only
/// copied when it really changes.
fn with_drawing(doc: &mut Document, at: &Pos, f: impl FnOnce(&mut Drawing) -> bool) -> bool {
    let Some(p) = doc.paragraph_mut(&at.path) else {
        return false;
    };
    let Some(i) = drawing_run(p, at.offset) else {
        return false;
    };
    let Inline::Drawing(drawing) = &mut p.runs[i].kind else {
        return false;
    };
    let mut trial = (**drawing).clone();
    if !f(&mut trial) {
        return false;
    }
    *Arc::make_mut(drawing) = trial;
    true
}

/// Give the drawing that starts at `at` (the position just before its
/// object character) the size `size`, width and height in EMU. Nothing
/// happens where there is no drawing at `at`, or the size is not positive
/// (it is cut to about 220 metres). Only the size changes: the crop of a
/// picture and the place of a floating one stay as they are, so this works
/// the same for a floating drawing as for one in the line.
pub fn resize_drawing(doc: &mut Document, at: &Pos, size: (i64, i64)) {
    let Some(size) = clamped(size) else {
        return;
    };
    with_drawing(doc, at, |drawing| {
        let changed = drawing.size != size;
        drawing.size = size;
        changed
    });
}

/// How a drawing sits in relation to the text: the choices of Word's
/// Layout Options.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PictureLayout {
    /// In the line, like a big character.
    Inline,
    /// Floating; text runs around its frame.
    Square,
    /// Floating; text runs around its shape (laid out like square: the
    /// model has no outline to follow).
    Tight,
    /// Floating; text above and below only.
    TopAndBottom,
    /// Floating under the text, which runs over it.
    BehindText,
    /// Floating over the text, which runs under it.
    InFrontOfText,
}

/// The layout a drawing has. A floating drawing that wraps text is as its
/// wrap says (a `behind` flag it also has is kept but does not show in the
/// answer); one text does not wrap around is behind or in front by `behind`.
pub fn picture_layout(drawing: &Drawing) -> PictureLayout {
    match &drawing.float {
        None => PictureLayout::Inline,
        Some(float) => match float.wrap {
            Wrap::Square => PictureLayout::Square,
            Wrap::Tight => PictureLayout::Tight,
            Wrap::TopAndBottom => PictureLayout::TopAndBottom,
            Wrap::None if float.behind => PictureLayout::BehindText,
            Wrap::None => PictureLayout::InFrontOfText,
        },
    }
}

/// The room kept clear around a drawing that text wraps around when it
/// starts to float: Word's own, an eighth of an inch at the sides (left,
/// top, right, bottom, EMU).
const WRAP_DISTANCE: [i64; 4] = [114_300, 0, 114_300, 0];

/// The place a drawing that leaves the line gets: where it was, at the
/// character and in the line it was in, so that nothing moves until the
/// user moves it.
fn first_float() -> Float {
    Float {
        x: FloatAxis {
            from: FloatFrom::Text,
            at: FloatAt::Offset(0),
        },
        y: FloatAxis {
            from: FloatFrom::Text,
            at: FloatAt::Offset(0),
        },
        wrap: Wrap::Square,
        behind: false,
        distance: [0; 4],
    }
}

/// Change how the drawing that starts at `at` sits in relation to the text.
/// Returns whether anything changed (false where there is no drawing, or it
/// already has the layout).
///
/// **Leaving the line.** The drawing stays anchored in the same paragraph at
/// the same place in the text, and is put where it was: its left edge at the
/// character, its top at the top of the line ([`FloatFrom::Text`] both
/// ways, offset 0). The wrapping ones keep an eighth of an inch clear at the
/// sides. **Going on floating** (square to tight, behind to in front, ...)
/// keeps its position and the room around it; only the wrap and the side of
/// the text change. **Coming back into the line** drops the position (and
/// the room around).
pub fn set_picture_layout(doc: &mut Document, at: &Pos, layout: PictureLayout) -> bool {
    with_drawing(doc, at, |drawing| {
        if picture_layout(drawing) == layout {
            return false;
        }
        if layout == PictureLayout::Inline {
            drawing.float = None;
            return true;
        }
        let leaving = drawing.float.is_none();
        let mut float = drawing.float.unwrap_or_else(first_float);
        match layout {
            PictureLayout::Inline => {}
            PictureLayout::Square => (float.wrap, float.behind) = (Wrap::Square, false),
            PictureLayout::Tight => (float.wrap, float.behind) = (Wrap::Tight, false),
            PictureLayout::TopAndBottom => (float.wrap, float.behind) = (Wrap::TopAndBottom, false),
            PictureLayout::BehindText => (float.wrap, float.behind) = (Wrap::None, true),
            PictureLayout::InFrontOfText => (float.wrap, float.behind) = (Wrap::None, false),
        }
        if leaving
            && matches!(
                layout,
                PictureLayout::Square | PictureLayout::Tight | PictureLayout::TopAndBottom
            )
        {
            float.distance = WRAP_DISTANCE;
        }
        drawing.float = Some(float);
        true
    })
}

/// Move the floating drawing that starts at `at` by `delta` (right and down,
/// EMU, negative for left and up). A position that was an offset moves by
/// it; one that was given by alignment (left, centre, right, top, bottom)
/// becomes an offset from the same reference. For that the size of the
/// reference is needed: `spans` is the width of what the horizontal position
/// is measured from and the height of what the vertical one is measured from
/// (EMU; a page, a margin box, a column), and is only looked at for centre,
/// right and bottom. Offsets are held within about 220 metres. Returns
/// whether anything moved; false for a drawing in the line (use
/// [`move_drawing`] to move that to another place in the text).
pub fn move_floating_drawing(
    doc: &mut Document,
    at: &Pos,
    delta: (i64, i64),
    spans: (i64, i64),
) -> bool {
    with_drawing(doc, at, |drawing| {
        let size = drawing.size;
        let Some(float) = &mut drawing.float else {
            return false;
        };
        let moved = |axis: &mut FloatAxis, by: i64, span: i64, size: i64| {
            let base = match axis.at {
                FloatAt::Offset(offset) => offset,
                FloatAt::Start => 0,
                FloatAt::Center => (span - size) / 2,
                FloatAt::End => span - size,
            };
            let now = (base + by).clamp(-MAX_EMU, MAX_EMU);
            let changed = axis.at != FloatAt::Offset(now);
            axis.at = FloatAt::Offset(now);
            changed
        };
        let x = moved(&mut float.x, delta.0, spans.0, size.0);
        let y = moved(&mut float.y, delta.1, spans.1, size.1);
        x || y
    })
}

/// Move the drawing that starts at `from` (in the line or floating: its
/// anchor) to the place `to` in the text, which may be in another
/// paragraph; where it is now: the position just before it. `None` where
/// there is no drawing at `from` or `to` leads to no paragraph; `from` is
/// given back where `to` is at the drawing itself (nothing moves). The
/// drawing keeps everything it has, and so its bytes in the file when it was
/// read from one.
pub fn move_drawing(doc: &mut Document, from: &Pos, to: &Pos) -> Option<Pos> {
    let p = doc.paragraph(&from.path)?;
    let i = drawing_run(p, from.offset)?;
    let width = p.runs[i].text.len();
    let target = doc.paragraph(&to.path)?;
    let at = snap(target, to.offset, false);
    let same = to.path == from.path;
    if same && at >= from.offset && at <= from.offset + width {
        return Some(from.clone());
    }
    let p = doc.paragraph_mut(&from.path)?;
    let run = p.runs.remove(i);
    merge_runs(p);
    let at = if same && at > from.offset {
        at - width
    } else {
        at
    };
    let target = doc.paragraph_mut(&to.path)?;
    let index = split_run(target, at);
    target.runs.insert(index, run);
    merge_runs(target);
    Some(Pos::new(to.path.clone(), at))
}

#[cfg(test)]
#[path = "picture_tests.rs"]
mod tests;
