//! Checks on an opened document, before any page is drawn.
//!
//! The parser decodes streams without a ceiling and trusts the sizes that
//! pictures declare. Here every stream in the file is looked at as the parser
//! itself sees it: its dictionary as parsed (`/F` before `/Filter`, names with
//! `#xx` escapes decoded, references followed), its data as the parser cut it
//! (by `/Length`) and decrypted. Its filters are then applied by our own
//! decoders, which stop at the ceiling. Inline pictures in decoded content are
//! found with the parser's own content reader.
//!
//! Nothing is guessed: a filter we cannot decode ourselves, data our decoder
//! rejects, a picture whose size cannot be read, all refuse the file.

use hayro::hayro_syntax::Filter;
use hayro::hayro_syntax::Pdf;
use hayro::hayro_syntax::content::TypedIter;
use hayro::hayro_syntax::content::ops::TypedInstruction;
use hayro::hayro_syntax::object::{Array, Dict, Name, Object, Stream};

use crate::Limits;
use crate::inflate::{self, Decoded};

/// Why a document is refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Refusal {
    /// A limit is broken.
    Exceeded(crate::Exceeded),
    /// Something cannot be checked: a filter we do not decode, data that
    /// does not decode, a picture without a readable size.
    Unchecked,
}

impl From<crate::Exceeded> for Refusal {
    fn from(exceeded: crate::Exceeded) -> Self {
        Refusal::Exceeded(exceeded)
    }
}

/// Check every stream of `pdf`, and the number of its objects and pages.
pub fn check_document(pdf: &Pdf, limits: &Limits) -> Result<(), Refusal> {
    if pdf.len() > limits.max_objects {
        return Err(crate::Exceeded::Objects.into());
    }
    if pdf.pages().len() > limits.max_pages {
        return Err(crate::Exceeded::Pages.into());
    }
    for object in pdf.objects() {
        if let Object::Stream(stream) = object {
            check_stream(&stream, limits)?;
        }
    }
    Ok(())
}

/// The filters the parser applies to `stream`, with their parameters, in
/// the parser's own way (`hayro_syntax::object::stream::filters_and_params`):
/// `/F` before `/Filter`, `/DP` before `/DecodeParms`, parameters taken in
/// step with every name of the array, names it does not know dropped.
fn filters_and_params<'a>(stream: &Stream<'a>) -> Vec<(Filter, Dict<'a>)> {
    let dict = stream.dict();
    let filters = stream.filters();
    let single = dict
        .get::<Name<'_>>(b"F")
        .or_else(|| dict.get::<Name<'_>>(b"Filter"));
    if single.is_some() && filters.len() == 1 {
        let params = dict
            .get::<Dict<'_>>(b"DP")
            .or_else(|| dict.get::<Dict<'_>>(b"DecodeParms"))
            .unwrap_or_default();
        return vec![(filters[0], params)];
    }
    let Some(names) = dict
        .get::<Array<'_>>(b"F")
        .or_else(|| dict.get::<Array<'_>>(b"Filter"))
    else {
        return filters.iter().map(|f| (*f, Dict::default())).collect();
    };
    let mut params = dict
        .get::<Array<'_>>(b"DP")
        .or_else(|| dict.get::<Array<'_>>(b"DecodeParms"))
        .map(|a| a.iter::<Object<'_>>());
    let mut known = filters.iter();
    let mut out = Vec::new();
    for name in names.iter::<Name<'_>>() {
        let param = params
            .as_mut()
            .and_then(|p| p.next())
            .and_then(|p| p.into_dict())
            .unwrap_or_default();
        if is_filter_name(&name)
            && let Some(filter) = known.next()
        {
            out.push((*filter, param));
        }
    }
    out
}

/// The names `Filter::from_name` knows.
fn is_filter_name(name: &[u8]) -> bool {
    matches!(
        name,
        b"ASCIIHexDecode"
            | b"AHx"
            | b"ASCII85Decode"
            | b"A85"
            | b"LZWDecode"
            | b"LZW"
            | b"FlateDecode"
            | b"Fl"
            | b"RunLengthDecode"
            | b"RL"
            | b"CCITTFaxDecode"
            | b"CCF"
            | b"JBIG2Decode"
            | b"DCTDecode"
            | b"DCT"
            | b"JPXDecode"
            | b"Crypt"
    )
}

/// Decode `stream` within the limits and look at what comes out.
pub fn check_stream(stream: &Stream<'_>, limits: &Limits) -> Result<(), Refusal> {
    let dict = stream.dict();
    picture_size(dict, limits)?;
    // A file specification in /F: the parser does not decode the stream.
    if matches!(
        dict.get::<Object<'_>>(b"F"),
        Some(Object::String(_) | Object::Dict(_))
    ) {
        return Ok(());
    }
    match decode(stream, limits, false)? {
        Some(content) => inline_pictures(&content, limits),
        None => Ok(()),
    }
}

/// Apply `stream`'s filters with our decoders. `Some(data)` is what the
/// parser would hand to whoever reads the stream; `None` when the last
/// filter is a picture codec (its size checked).
fn decode(stream: &Stream<'_>, limits: &Limits, inline: bool) -> Result<Option<Vec<u8>>, Refusal> {
    let dict = stream.dict();
    let filters = filters_and_params(stream);
    if filters.len() != stream.filters().len() {
        return Err(Refusal::Unchecked);
    }
    let raw = stream.raw_data();
    let mut owned: Option<Vec<u8>> = None;
    let last = filters.len().saturating_sub(1);
    for (index, (filter, params)) in filters.iter().enumerate() {
        let data = owned.as_deref().unwrap_or(&raw);
        let limit = limits.max_stream_bytes;
        let size = match filter {
            // The parser skips a leading Crypt (the data is decrypted
            // already) and fails on any other.
            Filter::Crypt if index == 0 && !inline => continue,
            Filter::Crypt => return Err(Refusal::Unchecked),
            Filter::FlateDecode => {
                owned = Some(predicted(inflate::flate(data, limit), params, limit)?);
                continue;
            }
            Filter::LzwDecode => {
                let early = params.get::<u8>(b"EarlyChange").is_none_or(|v| v != 0);
                owned = Some(predicted(inflate::lzw(data, early, limit), params, limit)?);
                continue;
            }
            Filter::RunLengthDecode => {
                owned = Some(decoded(inflate::run_length(data, limit))?);
                continue;
            }
            Filter::AsciiHexDecode => {
                owned = Some(decoded(inflate::ascii_hex(data))?);
                continue;
            }
            Filter::Ascii85Decode => {
                owned = Some(decoded(inflate::ascii_85(data, limit))?);
                continue;
            }
            Filter::DctDecode => inflate::jpeg_size(data),
            Filter::JpxDecode if !inline => inflate::jpx_size(data),
            Filter::Jbig2Decode if !inline => inflate::jbig2_size(data),
            Filter::JpxDecode | Filter::Jbig2Decode => return Err(Refusal::Unchecked),
            Filter::CcittFaxDecode => {
                // As the parser: the picture's height, else /Rows.
                let columns = params.get::<usize>(b"Columns").unwrap_or(1728);
                let height = dict
                    .get::<u32>(b"H")
                    .or_else(|| dict.get::<u32>(b"Height"))
                    .filter(|h| *h > 0)
                    .or_else(|| params.get::<u32>(b"Rows"))
                    .unwrap_or(0);
                match u32::try_from(columns) {
                    Ok(columns) if height > 0 => Some((columns, height)),
                    _ => None,
                }
            }
        };
        // A picture codec: its output is pixels, nothing may follow it.
        let (width, height) = size.ok_or(Refusal::Unchecked)?;
        pixels(width, height, limits)?;
        if index != last {
            return Err(Refusal::Unchecked);
        }
        return Ok(None);
    }
    Ok(Some(owned.unwrap_or_else(|| raw.into_owned())))
}

fn decoded(outcome: Decoded) -> Result<Vec<u8>, Refusal> {
    match outcome {
        Decoded::Data(data) => Ok(data),
        Decoded::TooLarge => Err(crate::Exceeded::Stream.into()),
        Decoded::Invalid => Err(Refusal::Unchecked),
    }
}

/// Undo a PNG or TIFF predictor, as the parser does after Flate and LZW.
fn predicted(outcome: Decoded, params: &Dict<'_>, limit: usize) -> Result<Vec<u8>, Refusal> {
    let data = decoded(outcome)?;
    let predictor = params.get::<u8>(b"Predictor").unwrap_or(1);
    if predictor <= 1 {
        return Ok(data);
    }
    let colors = params.get::<u8>(b"Colors").unwrap_or(1);
    let bits = params.get::<u8>(b"BitsPerComponent").unwrap_or(8);
    let columns = params.get::<usize>(b"Columns").unwrap_or(1);
    inflate::unpredict(data, predictor, colors, bits, columns, limit).ok_or(Refusal::Unchecked)
}

/// Width and height the way the parser reads them for pictures: `/W` or
/// `/Width`, `/H` or `/Height`, as unsigned integers.
fn picture_size(dict: &Dict<'_>, limits: &Limits) -> Result<(), Refusal> {
    let width = dict.get::<u32>(b"W").or_else(|| dict.get::<u32>(b"Width"));
    let height = dict.get::<u32>(b"H").or_else(|| dict.get::<u32>(b"Height"));
    let is_image = dict
        .get::<Name<'_>>(b"Subtype")
        .is_some_and(|n| &*n == b"Image");
    match (width, height) {
        (Some(w), Some(h)) => pixels(w, h, limits),
        _ if is_image => Err(Refusal::Unchecked),
        _ => Ok(()),
    }
}

fn pixels(width: u32, height: u32, limits: &Limits) -> Result<(), Refusal> {
    if u64::from(width) * u64::from(height) > limits.max_image_pixels {
        return Err(crate::Exceeded::Image.into());
    }
    Ok(())
}

/// Inline pictures in `content`, found by the parser's own content reader:
/// their size, and their data through their filters (the parser maps the
/// short names, `/F` and `/Fl`, itself).
fn inline_pictures(content: &[u8], limits: &Limits) -> Result<(), Refusal> {
    let mut iter = TypedIter::new(content);
    while let Some(instruction) = iter.next() {
        if let TypedInstruction::InlineImage(image) = instruction {
            let stream = image.0;
            picture_size(stream.dict(), limits)?;
            decode(stream, limits, true)?;
        }
    }
    Ok(())
}
