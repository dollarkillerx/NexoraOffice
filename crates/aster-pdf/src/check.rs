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
use hayro::hayro_syntax::object::{Array, Dict, MaybeRef, Name, Object, ObjectIdentifier, Stream};

use std::collections::HashSet;

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

/// Rounds of looking at the whole file before it is refused as unsteady. A
/// repair of the cross-reference table happens once, so two are enough; the
/// rest is margin.
const ROUNDS: usize = 5;
/// How deep the walk goes into the dictionaries and arrays of one object.
const MAX_DEPTH: usize = 64;

/// Check every stream of `pdf`, and the number of its objects and pages.
///
/// The parser reads lazily, and a bad cross-reference entry makes it rebuild
/// its whole object table by scanning the file, in the middle of whatever is
/// being read. Objects that only the rebuilt table has would never be seen
/// by a single walk. So the walk is repeated, over the parser's list and
/// over every `N G obj` header found in the raw bytes, until a round finds
/// nothing new.
pub fn check_document(pdf: &Pdf, limits: &Limits) -> Result<(), Refusal> {
    check_counts(pdf, limits)?;
    let mut walk = Walk {
        limits,
        seen: HashSet::new(),
        nodes: 0,
        budget: limits
            .max_objects
            .saturating_mul(16)
            .saturating_add(1 << 20),
    };
    let mut steady = false;
    for _ in 0..ROUNDS {
        let mut fresh = false;
        for object in pdf.objects() {
            fresh |= walk.visit(&object)?;
        }
        for id in header_ids(pdf.data().as_ref(), limits)? {
            if let Some(object) = pdf.xref().get::<Object<'_>>(id) {
                fresh |= walk.visit(&object)?;
            }
        }
        // A repair may have made the table larger.
        check_counts(pdf, limits)?;
        if !fresh {
            steady = true;
            break;
        }
    }
    if !steady {
        return Err(Refusal::Unchecked);
    }
    for page in pdf.pages().iter() {
        page_contents(pdf, page.raw(), &mut walk)?;
    }
    Ok(())
}

fn check_counts(pdf: &Pdf, limits: &Limits) -> Result<(), Refusal> {
    if pdf.len() > limits.max_objects {
        return Err(crate::Exceeded::Objects.into());
    }
    if pdf.pages().len() > limits.max_pages {
        return Err(crate::Exceeded::Pages.into());
    }
    Ok(())
}

/// What has been looked at, and how much work is left.
struct Walk<'l> {
    limits: &'l Limits,
    /// Position in the file data of the objects already checked. An object
    /// that a repair moved is a new one.
    seen: HashSet<(usize, usize)>,
    nodes: usize,
    budget: usize,
}

impl Walk<'_> {
    /// Check a top-level object, with everything inside it, unless it was
    /// checked before. Whether it was new.
    fn visit(&mut self, object: &Object<'_>) -> Result<bool, Refusal> {
        let data = match object {
            Object::Stream(stream) => stream.dict().data(),
            Object::Dict(dict) => dict.data(),
            Object::Array(array) => array.data(),
            _ => return Ok(false),
        };
        if !self.seen.insert((data.as_ptr() as usize, data.len())) {
            return Ok(false);
        }
        self.inspect(object, 0)?;
        Ok(true)
    }

    /// Count one more thing looked at; the work is bounded.
    fn spend(&mut self, depth: usize) -> Result<(), Refusal> {
        self.nodes += 1;
        if self.nodes > self.budget || depth > MAX_DEPTH {
            return Err(Refusal::Unchecked);
        }
        Ok(())
    }

    /// Check `object` if it is a stream, and look for streams written
    /// directly inside it: the parser reads those from dictionaries and
    /// arrays like any other value (`/Contents << /Length 5 >> stream ...`).
    fn inspect(&mut self, object: &Object<'_>, depth: usize) -> Result<(), Refusal> {
        self.spend(depth)?;
        match object {
            Object::Stream(stream) => {
                check_stream(stream, self.limits)?;
                self.dict(stream.dict(), depth)
            }
            Object::Dict(dict) => self.dict(dict, depth),
            Object::Array(array) => {
                for item in array.raw_iter() {
                    self.spend(depth + 1)?;
                    if let MaybeRef::NotRef(inner) = item {
                        self.inspect(&inner, depth + 1)?;
                    }
                }
                Ok(())
            }
            _ => Ok(()),
        }
    }

    fn dict(&mut self, dict: &Dict<'_>, depth: usize) -> Result<(), Refusal> {
        for (_, value) in dict.entries() {
            self.spend(depth + 1)?;
            if let MaybeRef::NotRef(inner) = value {
                self.inspect(&inner, depth + 1)?;
            }
        }
        Ok(())
    }
}

/// Every `N G obj` in the raw bytes: the objects a repair of the table
/// would find. Each is looked up in the parser's current table.
pub(crate) fn header_ids(data: &[u8], limits: &Limits) -> Result<Vec<ObjectIdentifier>, Refusal> {
    let blank = |b: u8| matches!(b, 0 | 9 | 10 | 12 | 13 | b' ');
    // Up to ten digits, walking back from `back`.
    let number = |back: &mut usize| {
        let end = *back;
        while *back > 0 && data[*back - 1].is_ascii_digit() && end - *back < 10 {
            *back -= 1;
        }
        std::str::from_utf8(&data[*back..end])
            .ok()
            .and_then(|s| s.parse::<u32>().ok())
    };
    let blanks = |back: &mut usize| {
        while *back > 0 && blank(data[*back - 1]) {
            *back -= 1;
        }
    };
    let mut found = HashSet::new();
    let mut ids = Vec::new();
    let mut at = 0;
    while let Some(offset) = data[at..].iter().position(|&b| b == b'o') {
        let tag = at + offset;
        at = tag + 1;
        if !data[tag..].starts_with(b"obj") {
            continue;
        }
        // Walk back over the generation, blanks, the number.
        let mut back = tag;
        blanks(&mut back);
        let Some(generation) = number(&mut back).and_then(|g| i32::try_from(g).ok()) else {
            continue;
        };
        let before = back;
        blanks(&mut back);
        if back == before {
            continue;
        }
        let Some(object) = number(&mut back) else {
            continue;
        };
        let id = ObjectIdentifier::new(object, generation);
        if found.insert(id) {
            if found.len() > limits.max_objects {
                return Err(crate::Exceeded::Objects.into());
            }
            ids.push(id);
        }
    }
    Ok(ids)
}

/// A page's content streams put together as the parser does when /Contents
/// is an array (one blank after each; a stream that does not decode adds
/// nothing), and the inline pictures in the whole. They can start in one
/// stream and carry their data in the next.
fn page_contents(pdf: &Pdf, page: &Dict<'_>, walk: &mut Walk<'_>) -> Result<(), Refusal> {
    // One stream was looked at on its own already.
    if page.get::<Stream<'_>>(b"Contents").is_some() {
        return Ok(());
    }
    let Some(array) = page.get::<Array<'_>>(b"Contents") else {
        return Ok(());
    };
    let limits = walk.limits;
    let mut whole: Vec<u8> = Vec::new();
    for item in array.raw_iter() {
        walk.spend(1)?;
        let object = match item {
            MaybeRef::Ref(reference) => pdf.xref().get::<Object<'_>>(reference.into()),
            MaybeRef::NotRef(object) => Some(object),
        };
        let Some(Object::Stream(stream)) = object else {
            continue;
        };
        if let Some(data) = decode_content(&stream, limits)? {
            // The parser holds the whole as well.
            if whole.len().saturating_add(data.len()).saturating_add(1) > limits.max_stream_bytes {
                return Err(crate::Exceeded::Stream.into());
            }
            whole.extend_from_slice(&data);
            whole.push(b' ');
        }
    }
    inline_pictures(&whole, limits)
}

/// The filters the parser applies to `stream`, with their parameters, in
/// the parser's own way (`hayro_syntax::object::stream::filters_and_params`),
/// branch for branch: a single name in `/F`, else in `/Filter`, that it
/// knows, with `/DP` before `/DecodeParms` as a dictionary; otherwise an
/// array in `/F`, else in `/Filter`, with the parameters taken in step
/// from an array in `/DP`, else `/DecodeParms`, and names it does not know
/// dropped.
pub(crate) fn filters_and_params<'a>(stream: &Stream<'a>) -> Vec<(Filter, Dict<'a>)> {
    let dict = stream.dict();
    if let Some(filter) = dict
        .get::<Name<'_>>(b"F")
        .or_else(|| dict.get::<Name<'_>>(b"Filter"))
        .and_then(|name| filter_from_name(&name))
    {
        let params = dict
            .get::<Dict<'_>>(b"DP")
            .or_else(|| dict.get::<Dict<'_>>(b"DecodeParms"))
            .unwrap_or_default();
        return vec![(filter, params)];
    }
    let Some(names) = dict
        .get::<Array<'_>>(b"F")
        .or_else(|| dict.get::<Array<'_>>(b"Filter"))
    else {
        return Vec::new();
    };
    let mut params = dict
        .get::<Array<'_>>(b"DP")
        .or_else(|| dict.get::<Array<'_>>(b"DecodeParms"))
        .map(|a| a.iter::<Object<'_>>());
    let mut out = Vec::new();
    for name in names.iter::<Name<'_>>() {
        let param = params
            .as_mut()
            .and_then(|p| p.next())
            .and_then(|p| p.into_dict())
            .unwrap_or_default();
        if let Some(filter) = filter_from_name(&name) {
            out.push((filter, param));
        }
    }
    out
}

/// The names the parser's `Filter::from_name` knows.
fn filter_from_name(name: &[u8]) -> Option<Filter> {
    Some(match name {
        b"ASCIIHexDecode" | b"AHx" => Filter::AsciiHexDecode,
        b"ASCII85Decode" | b"A85" => Filter::Ascii85Decode,
        b"LZWDecode" | b"LZW" => Filter::LzwDecode,
        b"FlateDecode" | b"Fl" => Filter::FlateDecode,
        b"RunLengthDecode" | b"RL" => Filter::RunLengthDecode,
        b"CCITTFaxDecode" | b"CCF" => Filter::CcittFaxDecode,
        b"JBIG2Decode" => Filter::Jbig2Decode,
        b"DCTDecode" | b"DCT" => Filter::DctDecode,
        b"JPXDecode" => Filter::JpxDecode,
        b"Crypt" => Filter::Crypt,
        _ => return None,
    })
}

/// Check `stream` within the limits: its picture size, its data through its
/// filters, the inline pictures in what comes out.
pub fn check_stream(stream: &Stream<'_>, limits: &Limits) -> Result<(), Refusal> {
    picture_size(stream.dict(), limits)?;
    match decode_content(stream, limits)? {
        Some(content) => inline_pictures(&content, limits),
        None => Ok(()),
    }
}

/// What the parser's `decoded_data` gives for `stream`, as far as it gives
/// anything: `None` for a stream it does not decode (a file specification
/// in /F) or whose last filter is a picture codec.
fn decode_content(stream: &Stream<'_>, limits: &Limits) -> Result<Option<Vec<u8>>, Refusal> {
    if matches!(
        stream.dict().get::<Object<'_>>(b"F"),
        Some(Object::String(_) | Object::Dict(_))
    ) {
        return Ok(None);
    }
    decode(stream, limits, false)
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
            Filter::Jbig2Decode if !inline => inflate::jbig2_size(data).map(|(w, h)| (w, h, 1)),
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
                    Ok(columns) if height > 0 => Some((columns, height, 1)),
                    _ => None,
                }
            }
        };
        // A picture codec: its output is pixels, nothing may follow it.
        let (width, height, components) = size.ok_or(Refusal::Unchecked)?;
        pixels(width, height, limits)?;
        samples(width, height, components, limits)?;
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

/// Components count too: a picture of more samples than four per allowed
/// pixel is refused.
fn samples(width: u32, height: u32, components: u32, limits: &Limits) -> Result<(), Refusal> {
    let samples = u128::from(width) * u128::from(height) * u128::from(components);
    if samples > u128::from(limits.max_image_pixels) * 4 {
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
