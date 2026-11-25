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

use std::collections::{HashMap, HashSet};
use std::hash::{BuildHasher, Hash, Hasher, RandomState};

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

/// Decoded bytes all streams of one file may produce together, inline
/// pictures and page concatenations included. Every stream is decoded once
/// (see [`Walk::ensure`]), so ordinary files spend their decoded size once:
/// a 500-page book of flate pictures, 8 MB each decoded, is 4 GB and still
/// passes; a few hundred megabytes of text and fonts are nothing. Our
/// decoders run at several hundred megabytes a second at the least, so a
/// file that spends all of it is refused after seconds, not after the
/// child's whole open time.
pub(crate) const DECODE_BUDGET: u64 = 4 << 30;

/// Check every stream of `pdf`, and the number of its objects and pages.
///
/// The parser reads lazily, and a bad cross-reference entry makes it rebuild
/// its whole object table by scanning the file, in the middle of whatever is
/// being read. Objects that only the rebuilt table has would never be seen
/// by a single walk. So the walk is repeated, over the parser's list and
/// over every `N G obj` header found in the raw bytes, until a round finds
/// nothing new.
pub fn check_document(pdf: &Pdf, limits: &Limits) -> Result<(), Refusal> {
    check_document_with(pdf, limits, DECODE_BUDGET).map(|_| ())
}

pub(crate) fn check_document_with(
    pdf: &Pdf,
    limits: &Limits,
    decode_budget: u64,
) -> Result<usize, Refusal> {
    check_counts(pdf, limits)?;
    let mut walk = Walk {
        limits,
        seen: HashSet::new(),
        containers: HashSet::new(),
        memo: HashMap::new(),
        concatenations: HashSet::new(),
        hasher: RandomState::new(),
        cached: 0,
        decodes: 0,
        nodes: 0,
        budget: limits
            .max_objects
            .saturating_mul(16)
            .saturating_add(1 << 20),
        scanned: 0,
        scan_budget: (pdf.data().as_ref().len() as u64)
            .saturating_mul(4)
            .saturating_add(64 << 20),
        decoded: Budget(decode_budget),
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
    // How many streams were decoded: one each, whatever refers to them.
    Ok(walk.decodes)
}

/// What is left of the decoded bytes a file may produce.
pub(crate) struct Budget(u64);

impl Budget {
    /// The ceiling for one decoded stream: the limit, or what is left.
    fn ceiling(&self, limit: usize) -> usize {
        limit.min(usize::try_from(self.0).unwrap_or(usize::MAX))
    }

    fn spend(&mut self, bytes: usize) -> Result<(), Refusal> {
        match self.0.checked_sub(bytes as u64) {
            Some(left) => {
                self.0 = left;
                Ok(())
            }
            None => {
                self.0 = 0;
                Err(crate::Exceeded::Stream.into())
            }
        }
    }
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

/// Where a stream's dictionary lies in the file: one stream, wherever it is
/// written (an object, or inside another one), has one place.
type Key = (usize, usize);

/// What the check keeps of a stream it decoded.
#[derive(Clone, Copy)]
struct Info {
    /// Length of what the parser would hand out; `None` when it hands out
    /// nothing (a picture codec, a file specification).
    len: Option<usize>,
    /// The bytes `BI` appear in it: only then can it hold inline pictures.
    inline: bool,
}

/// Decoded content kept for page concatenations: small streams only, and
/// not much of them in all.
const CACHE_ONE: usize = 1 << 20;
const CACHE_ALL: usize = 64 << 20;

/// What has been looked at, and how much work is left.
struct Walk<'l> {
    limits: &'l Limits,
    /// Position in the file data of the objects already checked. An object
    /// that a repair moved is a new one.
    seen: HashSet<Key>,
    /// Dictionaries and arrays below those, looked into already.
    containers: HashSet<Key>,
    /// Streams decoded: each is decoded once, whatever refers to it and
    /// however many times.
    memo: HashMap<Key, (Info, Option<Vec<u8>>)>,
    /// Page concatenations checked, by the list of streams in them.
    concatenations: HashSet<u64>,
    hasher: RandomState,
    /// Bytes of decoded content kept in `memo`.
    cached: usize,
    /// How many streams were decoded.
    decodes: usize,
    nodes: usize,
    budget: usize,
    /// Bytes of containers searched for a `stream` keyword, and the most
    /// that may be.
    scanned: u64,
    scan_budget: u64,
    decoded: Budget,
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

    /// Whether a container written in `data` is to be looked into: it is
    /// not, when it was before, or when it has no `stream` keyword, which
    /// every stream the parser reads must have. Each container is searched
    /// once, and the bytes searched are bounded; the parser's own reading
    /// of what lies inside (which costs its size again for every level)
    /// is left out when nothing can be there.
    fn worth_entering(&mut self, data: &[u8]) -> Result<bool, Refusal> {
        if !self.containers.insert((data.as_ptr() as usize, data.len())) {
            return Ok(false);
        }
        self.scanned = self.scanned.saturating_add(data.len() as u64);
        if self.scanned > self.scan_budget {
            return Err(Refusal::Unchecked);
        }
        Ok(find(data, b"stream"))
    }

    /// Check `object` if it is a stream, and look for streams written
    /// directly inside it: the parser reads those from dictionaries and
    /// arrays like any other value (`/Contents << /Length 5 >> stream ...`).
    fn inspect(&mut self, object: &Object<'_>, depth: usize) -> Result<(), Refusal> {
        self.spend(depth)?;
        match object {
            Object::Stream(stream) => {
                self.ensure(stream)?;
                self.dict(stream.dict(), depth)
            }
            Object::Dict(dict) => self.dict(dict, depth),
            Object::Array(array) => {
                if !self.worth_entering(array.data())? {
                    return Ok(());
                }
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
        if !self.worth_entering(dict.data())? {
            return Ok(());
        }
        for (_, value) in dict.entries() {
            self.spend(depth + 1)?;
            if let MaybeRef::NotRef(inner) = value {
                self.inspect(&inner, depth + 1)?;
            }
        }
        Ok(())
    }

    /// Check `stream`: its picture size, its data through its filters, the
    /// inline pictures in what comes out; once. Later asks get the answer
    /// kept.
    fn ensure(&mut self, stream: &Stream<'_>) -> Result<Info, Refusal> {
        let data = stream.dict().data();
        let key = (data.as_ptr() as usize, data.len());
        if let Some((info, _)) = self.memo.get(&key) {
            return Ok(*info);
        }
        picture_size(stream.dict(), self.limits)?;
        self.decodes += 1;
        let content = decode_content(stream, self.limits, &mut self.decoded)?;
        let mut info = Info {
            len: None,
            inline: false,
        };
        let mut kept = None;
        if let Some(content) = content {
            info.len = Some(content.len());
            info.inline = find(&content, b"BI");
            if info.inline {
                // Parsing costs about what decoding did.
                self.decoded.spend(content.len())?;
                inline_pictures(&content, self.limits, &mut self.decoded)?;
            }
            if content.len() <= CACHE_ONE && self.cached + content.len() <= CACHE_ALL {
                self.cached += content.len();
                kept = Some(content);
            }
        }
        self.memo.insert(key, (info, kept));
        Ok(info)
    }

    /// The content of a stream `ensure` has seen: kept, or decoded again
    /// (and paid for again).
    fn content(&mut self, stream: &Stream<'_>) -> Result<Vec<u8>, Refusal> {
        let data = stream.dict().data();
        let key = (data.as_ptr() as usize, data.len());
        if let Some((_, Some(kept))) = self.memo.get(&key) {
            return Ok(kept.clone());
        }
        Ok(decode_content(stream, self.limits, &mut self.decoded)?.unwrap_or_default())
    }
}

/// Whether `needle` is in `data`.
fn find(data: &[u8], needle: &[u8]) -> bool {
    let first = needle[0];
    let mut at = 0;
    while let Some(offset) = data[at..].iter().position(|&b| b == first) {
        let start = at + offset;
        if data[start..].starts_with(needle) {
            return true;
        }
        at = start + 1;
    }
    false
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
///
/// Every stream was decoded once already; here only lengths are added up,
/// and the whole is built (from content kept, or decoded again at the cost
/// of the budget) only when it fits and some stream can hold a picture.
fn page_contents(pdf: &Pdf, page: &Dict<'_>, walk: &mut Walk<'_>) -> Result<(), Refusal> {
    // One stream was looked at on its own already.
    if page.get::<Stream<'_>>(b"Contents").is_some() {
        return Ok(());
    }
    let Some(array) = page.get::<Array<'_>>(b"Contents") else {
        return Ok(());
    };
    let limits = walk.limits;
    let mut parts = Vec::new();
    let mut total = 0usize;
    let mut inline = false;
    for item in array.raw_iter() {
        walk.spend(1)?;
        let object = match item {
            MaybeRef::Ref(reference) => pdf.xref().get::<Object<'_>>(reference.into()),
            MaybeRef::NotRef(object) => Some(object),
        };
        let Some(Object::Stream(stream)) = object else {
            continue;
        };
        let info = walk.ensure(&stream)?;
        if let Some(len) = info.len {
            // The parser holds the whole as well.
            total = total.saturating_add(len).saturating_add(1);
            if total > limits.max_stream_bytes {
                return Err(crate::Exceeded::Stream.into());
            }
            inline |= info.inline;
            parts.push(stream);
        }
    }
    if parts.len() < 2 || !inline {
        return Ok(());
    }
    // The same list on another page is the same whole.
    let mut hasher = walk.hasher.build_hasher();
    for part in &parts {
        let data = part.dict().data();
        (data.as_ptr() as usize, data.len()).hash(&mut hasher);
    }
    if !walk.concatenations.insert(hasher.finish()) {
        return Ok(());
    }
    walk.decoded.spend(total)?;
    let mut whole: Vec<u8> = Vec::with_capacity(total);
    for part in &parts {
        whole.extend_from_slice(&walk.content(part)?);
        whole.push(b' ');
    }
    inline_pictures(&whole, limits, &mut walk.decoded)
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

/// What the parser's `decoded_data` gives for `stream`, as far as it gives
/// anything: `None` for a stream it does not decode (a file specification
/// in /F) or whose last filter is a picture codec.
fn decode_content(
    stream: &Stream<'_>,
    limits: &Limits,
    budget: &mut Budget,
) -> Result<Option<Vec<u8>>, Refusal> {
    if matches!(
        stream.dict().get::<Object<'_>>(b"F"),
        Some(Object::String(_) | Object::Dict(_))
    ) {
        return Ok(None);
    }
    decode(stream, limits, budget, false)
}

/// Apply `stream`'s filters with our decoders. `Some(data)` is what the
/// parser would hand to whoever reads the stream; `None` when the last
/// filter is a picture codec (its size checked).
fn decode(
    stream: &Stream<'_>,
    limits: &Limits,
    budget: &mut Budget,
    inline: bool,
) -> Result<Option<Vec<u8>>, Refusal> {
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
        let limit = budget.ceiling(limits.max_stream_bytes);
        let size = match filter {
            // The parser skips a leading Crypt (the data is decrypted
            // already) and fails on any other.
            Filter::Crypt if index == 0 && !inline => continue,
            Filter::Crypt => return Err(Refusal::Unchecked),
            Filter::FlateDecode => {
                let out = predicted(inflate::flate(data, limit), params, limit)?;
                budget.spend(out.len())?;
                owned = Some(out);
                continue;
            }
            Filter::LzwDecode => {
                let early = params.get::<u8>(b"EarlyChange").is_none_or(|v| v != 0);
                let out = predicted(inflate::lzw(data, early, limit), params, limit)?;
                budget.spend(out.len())?;
                owned = Some(out);
                continue;
            }
            Filter::RunLengthDecode => {
                let out = decoded(inflate::run_length(data, limit))?;
                budget.spend(out.len())?;
                owned = Some(out);
                continue;
            }
            Filter::AsciiHexDecode => {
                let out = decoded(inflate::ascii_hex(data))?;
                budget.spend(out.len())?;
                owned = Some(out);
                continue;
            }
            Filter::Ascii85Decode => {
                let out = decoded(inflate::ascii_85(data, limit))?;
                budget.spend(out.len())?;
                owned = Some(out);
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
    let out = owned.unwrap_or_else(|| raw.into_owned());
    if filters.is_empty() {
        budget.spend(out.len())?;
    }
    Ok(Some(out))
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
fn inline_pictures(content: &[u8], limits: &Limits, budget: &mut Budget) -> Result<(), Refusal> {
    let mut iter = TypedIter::new(content);
    while let Some(instruction) = iter.next() {
        if let TypedInstruction::InlineImage(image) = instruction {
            let stream = image.0;
            picture_size(stream.dict(), limits)?;
            decode(stream, limits, budget, true)?;
        }
    }
    Ok(())
}
