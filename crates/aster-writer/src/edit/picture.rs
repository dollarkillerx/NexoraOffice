//! Pictures: putting an image in the text and changing the size of a
//! drawing.
//!
//! Nothing here decodes an image. The caller reads the pixel size (to work
//! out the size on the page) and hands the file's bytes over; the format is
//! known from the first bytes, because saving writes the bytes into the
//! document as they are and a format Word cannot open must not get there.

use std::sync::Arc;

use super::{insert_index, snap};
use crate::{Document, Drawing, DrawingContent, Inline, Picture, Pos, Run};

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

/// Give the drawing that starts at `at` (the position just before its
/// object character) the size `size`, width and height in EMU. Nothing
/// happens where there is no drawing at `at`, or the size is not positive
/// (it is cut to about 220 metres). Only the size changes: the crop of a
/// picture and the place of a floating one stay as they are.
pub fn resize_drawing(doc: &mut Document, at: &Pos, size: (i64, i64)) {
    let Some(size) = clamped(size) else {
        return;
    };
    let Some(p) = doc.paragraph(&at.path) else {
        return;
    };
    let mut start = 0;
    let mut found = None;
    for (i, run) in p.runs.iter().enumerate() {
        if start == at.offset
            && let Inline::Drawing(drawing) = &run.kind
        {
            found = (drawing.size != size).then_some(i);
            break;
        }
        start += run.text.len();
    }
    let Some(i) = found else {
        return;
    };
    if let Some(p) = doc.paragraph_mut(&at.path)
        && let Inline::Drawing(drawing) = &mut p.runs[i].kind
    {
        Arc::make_mut(drawing).size = size;
    }
}
