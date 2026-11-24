//! A pass over a PDF's bytes before the parser sees them.
//!
//! The parser has no ceiling on how far a stream may inflate or how large a
//! picture may be, so a file of a few kilobytes can make it ask for gigabytes.
//! This pass finds every `stream … endstream` in the file, decodes its filters
//! itself with a ceiling, reads the size of pictures from their headers, and
//! looks into decoded content for inline pictures. A file that breaks a limit
//! is refused before the parser runs.
//!
//! It works on the bytes as they are, so in encrypted files it cannot read the
//! streams (they fail to inflate and are passed over); [`crate::Document`]
//! checks those again through the parser once it has the key.

use std::collections::HashMap;

use crate::Limits;
use crate::inflate::{self, Decoded};

/// Which limit a file broke.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Exceeded {
    /// The file itself is larger than `max_file_bytes`.
    File,
    /// More objects than `max_objects`.
    Objects,
    /// More pages than `max_pages`.
    Pages,
    /// A stream inflates to more than `max_stream_bytes`.
    Stream,
    /// A picture has more pixels than `max_image_pixels`.
    Image,
    /// An encrypted object stream too large to check before decrypting.
    Encrypted,
}

/// What the pass learnt about a file.
#[derive(Debug, Default)]
pub struct Scan {
    /// The trailer names an encryption dictionary.
    pub encrypted: bool,
    /// `N G obj` headers seen.
    pub objects: usize,
    /// Streams found, with what the parser would need to look them up again.
    pub streams: Vec<StreamInfo>,
}

/// A stream found in the file.
#[derive(Debug, Clone)]
pub struct StreamInfo {
    /// Object number and generation, when an `obj` header came before it.
    pub id: Option<(u32, u16)>,
    /// `/Type /ObjStm` or `/Type /XRef`: the parser reads these while opening.
    pub structural: bool,
    /// Whether the bytes as stored could be decoded (not encrypted, not
    /// damaged).
    pub checked: bool,
    /// Its dictionary, as far as the scan reads one.
    pub dict: Dict,
}

/// Scan `bytes`. Returns which limit is broken, or what was found.
pub fn scan(bytes: &[u8], limits: &Limits) -> Result<Scan, Exceeded> {
    if bytes.len() as u64 > limits.max_file_bytes {
        return Err(Exceeded::File);
    }
    let mut found = Scan {
        encrypted: has_encrypt_key(bytes),
        ..Scan::default()
    };
    let integers = small_integers(bytes);
    let mut at = 0;
    let mut object: Option<(usize, (u32, u16))> = None;
    while at < bytes.len() {
        let Some(offset) = memchr2(b'o', b's', &bytes[at..]) else {
            break;
        };
        let i = at + offset;
        at = i + 1;
        if bytes[i] == b'o' {
            if let Some(id) = object_header(bytes, i) {
                found.objects += 1;
                if found.objects > limits.max_objects {
                    return Err(Exceeded::Objects);
                }
                object = Some((i + 3, id));
            }
            continue;
        }
        let Some(data_start) = stream_keyword(bytes, i) else {
            continue;
        };
        let data_end = find(bytes, b"endstream", data_start).unwrap_or(bytes.len());
        let dict_start = match object {
            Some((start, _)) if start <= i && i - start <= 1 << 20 => start,
            _ => i.saturating_sub(4096),
        };
        let id = object
            .filter(|(start, _)| *start == dict_start)
            .map(|(_, id)| id);
        let dict = Dict::parse(&bytes[dict_start..i]);
        let data = trim_eol(&bytes[data_start..data_end]);
        let structural = matches!(dict.name(b"Type"), Some(b"ObjStm" | b"XRef"));
        let checked = check_stream(&dict, data, &integers, limits)?;
        if !checked && found.encrypted && dict.name(b"Type") == Some(b"ObjStm") {
            // The parser decrypts and inflates object streams while opening,
            // before anything can be checked. Deflate turns one byte into at
            // most 1032; refuse what could pass the ceiling that way.
            let mut worst = data.len() as u64;
            let mut known = true;
            for filter in dict.filters() {
                match filter.as_slice() {
                    b"FlateDecode" | b"Fl" => worst = worst.saturating_mul(1032),
                    _ => known = false,
                }
            }
            if !known || worst > limits.max_stream_bytes as u64 {
                return Err(Exceeded::Encrypted);
            }
        }
        found.streams.push(StreamInfo {
            id,
            structural,
            checked,
            dict,
        });
        object = None;
        at = data_end.max(data_start);
    }
    Ok(found)
}

/// Decode `data` with the filters `dict` names, within the limits, and look at
/// what comes out. `Ok(false)` when it could not be decoded as stored.
pub fn check_stream(
    dict: &Dict,
    data: &[u8],
    integers: &HashMap<(u32, u16), i64>,
    limits: &Limits,
) -> Result<bool, Exceeded> {
    let filters = dict.filters();
    let parms = dict.decode_parms();
    let mut owned: Option<Vec<u8>> = None;
    for (index, filter) in filters.iter().enumerate() {
        let view = owned.as_deref().unwrap_or(data);
        let limit = limits.max_stream_bytes;
        let parm = parms.get(index).cloned().unwrap_or_default();
        let outcome = match filter.as_slice() {
            b"FlateDecode" | b"Fl" => inflate::flate(view, limit),
            b"LZWDecode" | b"LZW" => {
                let early = parm
                    .integer(b"EarlyChange", integers)
                    .is_none_or(|v| v != 0);
                inflate::lzw(view, early, limit)
            }
            b"RunLengthDecode" | b"RL" => inflate::run_length(view, limit),
            b"ASCIIHexDecode" | b"AHx" => inflate::ascii_hex(view),
            b"ASCII85Decode" | b"A85" => inflate::ascii_85(view, limit),
            b"DCTDecode" | b"DCT" => {
                return picture(inflate::jpeg_size(view), dict, integers, limits);
            }
            b"JPXDecode" => return picture(inflate::jpx_size(view), dict, integers, limits),
            b"JBIG2Decode" => return picture(inflate::jbig2_size(view), dict, integers, limits),
            b"CCITTFaxDecode" | b"CCF" => {
                let columns = parm.integer(b"Columns", integers).unwrap_or(1728);
                let rows = parm
                    .integer(b"Rows", integers)
                    .or_else(|| dict.integer(b"Height", integers))
                    .unwrap_or(0);
                let size = (u32::try_from(columns).ok(), u32::try_from(rows).ok());
                let size = match size {
                    (Some(c), Some(r)) => Some((c, r)),
                    _ => return Err(Exceeded::Image),
                };
                return picture(size, dict, integers, limits);
            }
            // Crypt with the identity filter, or a name we do not know: the
            // parser will not decode further either.
            _ => break,
        };
        match outcome {
            Decoded::Data(out) => owned = Some(out),
            Decoded::TooLarge => return Err(Exceeded::Stream),
            Decoded::Invalid => return Ok(false),
        }
    }
    picture(None, dict, integers, limits)?;
    inline_pictures(owned.as_deref().unwrap_or(data), limits)?;
    Ok(true)
}

/// A picture's size: from its header when there is one, else from the
/// dictionary. Both must stay under the pixel limit.
fn picture(
    header: Option<(u32, u32)>,
    dict: &Dict,
    integers: &HashMap<(u32, u16), i64>,
    limits: &Limits,
) -> Result<bool, Exceeded> {
    let too_many = |w: i64, h: i64| {
        w < 0 || h < 0 || (w as u64).saturating_mul(h as u64) > limits.max_image_pixels
    };
    if let Some((w, h)) = header
        && too_many(i64::from(w), i64::from(h))
    {
        return Err(Exceeded::Image);
    }
    let is_image = dict.name(b"Subtype") == Some(b"Image");
    if is_image {
        let w = dict.integer(b"Width", integers).unwrap_or(0);
        let h = dict.integer(b"Height", integers).unwrap_or(0);
        if too_many(w, h) {
            return Err(Exceeded::Image);
        }
    }
    Ok(true)
}

/// Inline pictures (`BI … ID … EI`) in decoded content: their size, and
/// their data inflated within the limit.
fn inline_pictures(content: &[u8], limits: &Limits) -> Result<(), Exceeded> {
    let mut at = 0;
    while let Some(offset) = find(content, b"BI", at) {
        let i = offset;
        at = i + 2;
        let before_ok = i == 0 || is_white(content[i - 1]);
        let after_ok = content.get(i + 2).is_none_or(|&b| is_white(b) || b == b'/');
        if !before_ok || !after_ok {
            continue;
        }
        let Some(id) = find(content, b"ID", at) else {
            break;
        };
        if id - at > 4096 {
            continue;
        }
        let dict = Dict::parse_entries(&content[at..id]);
        let empty = HashMap::new();
        let w = dict
            .integer(b"W", &empty)
            .or_else(|| dict.integer(b"Width", &empty))
            .unwrap_or(0);
        let h = dict
            .integer(b"H", &empty)
            .or_else(|| dict.integer(b"Height", &empty))
            .unwrap_or(0);
        if w < 0 || h < 0 || (w as u64).saturating_mul(h as u64) > limits.max_image_pixels {
            return Err(Exceeded::Image);
        }
        let data = &content[(id + 3).min(content.len())..];
        let mut filters = dict.filters();
        if filters.is_empty() {
            filters = dict.names_of(b"F");
        }
        if let Some(first) = filters.first()
            && matches!(first.as_slice(), b"Fl" | b"FlateDecode")
            && inflate::flate(data, limits.max_stream_bytes) == Decoded::TooLarge
        {
            return Err(Exceeded::Stream);
        }
        at = id + 2;
    }
    Ok(())
}

/// Objects whose whole body is one integer (`12 0 obj 640 endobj`), so that
/// `/Width 12 0 R` can be checked too.
pub fn small_integers(bytes: &[u8]) -> HashMap<(u32, u16), i64> {
    let mut map = HashMap::new();
    let mut at = 0;
    while let Some(i) = find(bytes, b"obj", at) {
        at = i + 3;
        let Some(id) = object_header(bytes, i) else {
            continue;
        };
        let rest = &bytes[at..bytes.len().min(at + 64)];
        let rest = trim_start(rest);
        let digits = rest
            .iter()
            .take_while(|b| b.is_ascii_digit() || **b == b'-' || **b == b'+')
            .count();
        if digits == 0 {
            continue;
        }
        let after = trim_start(&rest[digits..]);
        if after.starts_with(b"endobj")
            && let Some(value) = std::str::from_utf8(&rest[..digits])
                .ok()
                .and_then(|s| s.parse().ok())
        {
            map.insert(id, value);
        }
    }
    map
}

fn has_encrypt_key(bytes: &[u8]) -> bool {
    let mut at = 0;
    while let Some(i) = find(bytes, b"/Encrypt", at) {
        at = i + 8;
        if bytes
            .get(at)
            .is_none_or(|&b| is_white(b) || is_delimiter(b))
        {
            return true;
        }
    }
    false
}

/// `N G obj` ending at `i` (where `obj` starts): the object's number and
/// generation.
fn object_header(bytes: &[u8], i: usize) -> Option<(u32, u16)> {
    if !bytes[i..].starts_with(b"obj") {
        return None;
    }
    if bytes
        .get(i + 3)
        .is_some_and(|&b| !is_white(b) && !is_delimiter(b))
    {
        return None;
    }
    let mut j = i;
    let skip_white = |j: &mut usize| {
        let start = *j;
        while *j > 0 && is_white(bytes[*j - 1]) {
            *j -= 1;
        }
        *j < start
    };
    let digits = |j: &mut usize| {
        let end = *j;
        while *j > 0 && bytes[*j - 1].is_ascii_digit() && end - *j < 10 {
            *j -= 1;
        }
        (*j < end).then(|| {
            std::str::from_utf8(&bytes[*j..end])
                .ok()?
                .parse::<u64>()
                .ok()
        })?
    };
    if !skip_white(&mut j) {
        return None;
    }
    let generation = digits(&mut j)?;
    if !skip_white(&mut j) {
        return None;
    }
    let number = digits(&mut j)?;
    Some((u32::try_from(number).ok()?, u16::try_from(generation).ok()?))
}

/// The keyword `stream` at `i`, followed by its end of line: where the data
/// starts.
fn stream_keyword(bytes: &[u8], i: usize) -> Option<usize> {
    if !bytes[i..].starts_with(b"stream") {
        return None;
    }
    if i > 0 && !(is_white(bytes[i - 1]) || bytes[i - 1] == b'>') {
        return None;
    }
    let after = i + 6;
    match (bytes.get(after), bytes.get(after + 1)) {
        (Some(b'\r'), Some(b'\n')) => Some(after + 2),
        (Some(b'\n' | b'\r'), _) => Some(after + 1),
        (None, _) => Some(after),
        _ => None,
    }
}

fn trim_eol(data: &[u8]) -> &[u8] {
    let mut end = data.len();
    if end > 0 && data[end - 1] == b'\n' {
        end -= 1;
    }
    if end > 0 && data[end - 1] == b'\r' {
        end -= 1;
    }
    &data[..end]
}

fn trim_start(data: &[u8]) -> &[u8] {
    let n = data.iter().take_while(|b| is_white(**b)).count();
    &data[n..]
}

fn is_white(b: u8) -> bool {
    matches!(b, b' ' | b'\n' | b'\r' | b'\t' | b'\x0c' | b'\0')
}

fn is_delimiter(b: u8) -> bool {
    matches!(
        b,
        b'(' | b')' | b'<' | b'>' | b'[' | b']' | b'{' | b'}' | b'/' | b'%'
    )
}

fn memchr2(a: u8, b: u8, hay: &[u8]) -> Option<usize> {
    hay.iter().position(|&x| x == a || x == b)
}

pub(crate) fn find(hay: &[u8], needle: &[u8], from: usize) -> Option<usize> {
    let first = needle[0];
    let mut at = from;
    while at + needle.len() <= hay.len() {
        let offset = hay[at..hay.len() - needle.len() + 1]
            .iter()
            .position(|&b| b == first)?;
        let i = at + offset;
        if &hay[i..i + needle.len()] == needle {
            return Some(i);
        }
        at = i + 1;
    }
    None
}

/// A value in a dictionary, as far as this pass needs to understand one.
#[derive(Debug, Clone, PartialEq)]
enum Value {
    Name(Vec<u8>),
    Integer(i64),
    Reference(u32, u16),
    Names(Vec<Vec<u8>>),
    Dicts(Vec<Dict>),
    Dict(Dict),
    Other,
}

/// The entries of one dictionary, nested dictionaries included one level
/// down (for `/DecodeParms`).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Dict {
    entries: Vec<(Vec<u8>, Value)>,
}

#[derive(Debug, Clone, PartialEq)]
enum Token {
    Name(Vec<u8>),
    Number(Vec<u8>),
    Word(Vec<u8>),
    OpenDict,
    CloseDict,
    OpenArray,
    CloseArray,
    Other,
}

fn tokens(bytes: &[u8]) -> Vec<Token> {
    let mut out = Vec::new();
    let mut i = 0;
    while i < bytes.len() && out.len() < 100_000 {
        let b = bytes[i];
        match b {
            _ if is_white(b) => i += 1,
            b'%' => {
                while i < bytes.len() && !matches!(bytes[i], b'\n' | b'\r') {
                    i += 1;
                }
            }
            b'/' => {
                let start = i + 1;
                i = start;
                while i < bytes.len() && !is_white(bytes[i]) && !is_delimiter(bytes[i]) {
                    i += 1;
                }
                out.push(Token::Name(bytes[start..i].to_vec()));
            }
            b'<' if bytes.get(i + 1) == Some(&b'<') => {
                out.push(Token::OpenDict);
                i += 2;
            }
            b'>' if bytes.get(i + 1) == Some(&b'>') => {
                out.push(Token::CloseDict);
                i += 2;
            }
            b'<' => {
                while i < bytes.len() && bytes[i] != b'>' {
                    i += 1;
                }
                i += 1;
                out.push(Token::Other);
            }
            b'(' => {
                let mut depth = 0usize;
                while i < bytes.len() {
                    match bytes[i] {
                        b'\\' => i += 1,
                        b'(' => depth += 1,
                        b')' => {
                            depth -= 1;
                            if depth == 0 {
                                i += 1;
                                break;
                            }
                        }
                        _ => {}
                    }
                    i += 1;
                }
                out.push(Token::Other);
            }
            b'[' => {
                out.push(Token::OpenArray);
                i += 1;
            }
            b']' => {
                out.push(Token::CloseArray);
                i += 1;
            }
            _ => {
                let start = i;
                while i < bytes.len() && !is_white(bytes[i]) && !is_delimiter(bytes[i]) {
                    i += 1;
                }
                if i == start {
                    i += 1;
                    out.push(Token::Other);
                    continue;
                }
                let word = bytes[start..i].to_vec();
                if word
                    .iter()
                    .all(|c| c.is_ascii_digit() || matches!(c, b'-' | b'+' | b'.'))
                {
                    out.push(Token::Number(word));
                } else {
                    out.push(Token::Word(word));
                }
            }
        }
    }
    out
}

impl Dict {
    /// The first dictionary in `bytes` (everything from its `<<`).
    pub fn parse(bytes: &[u8]) -> Dict {
        let tokens = tokens(bytes);
        let Some(start) = tokens.iter().position(|t| *t == Token::OpenDict) else {
            return Dict::default();
        };
        let mut at = start + 1;
        Dict::read(&tokens, &mut at, 0)
    }

    /// Entries without the surrounding `<< >>`, as inline pictures write them.
    pub fn parse_entries(bytes: &[u8]) -> Dict {
        let tokens = tokens(bytes);
        let mut at = 0;
        Dict::read(&tokens, &mut at, 0)
    }

    fn read(tokens: &[Token], at: &mut usize, depth: usize) -> Dict {
        let mut dict = Dict::default();
        while *at < tokens.len() {
            let key = match &tokens[*at] {
                Token::CloseDict => {
                    *at += 1;
                    return dict;
                }
                Token::Name(name) => name.clone(),
                _ => {
                    *at += 1;
                    continue;
                }
            };
            *at += 1;
            let value = Dict::value(tokens, at, depth);
            dict.entries.push((key, value));
        }
        dict
    }

    fn value(tokens: &[Token], at: &mut usize, depth: usize) -> Value {
        let Some(token) = tokens.get(*at) else {
            return Value::Other;
        };
        *at += 1;
        match token {
            Token::Name(name) => Value::Name(name.clone()),
            Token::Number(number) => {
                let integer = std::str::from_utf8(number)
                    .ok()
                    .and_then(|s| s.parse::<i64>().ok());
                if let (Some(Token::Number(generation)), Some(Token::Word(r))) =
                    (tokens.get(*at), tokens.get(*at + 1))
                    && r == b"R"
                {
                    *at += 2;
                    let generation = std::str::from_utf8(generation)
                        .ok()
                        .and_then(|s| s.parse().ok());
                    return match (integer.and_then(|n| u32::try_from(n).ok()), generation) {
                        (Some(n), Some(g)) => Value::Reference(n, g),
                        _ => Value::Other,
                    };
                }
                integer.map_or(Value::Other, Value::Integer)
            }
            Token::OpenDict if depth < 4 => Value::Dict(Dict::read(tokens, at, depth + 1)),
            Token::OpenDict => {
                Dict::skip(tokens, at, Token::OpenDict, Token::CloseDict);
                Value::Other
            }
            Token::OpenArray => {
                let mut names = Vec::new();
                let mut dicts = Vec::new();
                while let Some(token) = tokens.get(*at) {
                    match token {
                        Token::CloseArray => {
                            *at += 1;
                            break;
                        }
                        Token::Name(name) => {
                            names.push(name.clone());
                            *at += 1;
                        }
                        Token::OpenDict if depth < 4 => {
                            *at += 1;
                            dicts.push(Dict::read(tokens, at, depth + 1));
                        }
                        Token::Word(w) if w == b"null" => {
                            dicts.push(Dict::default());
                            *at += 1;
                        }
                        Token::OpenArray => {
                            *at += 1;
                            Dict::skip(tokens, at, Token::OpenArray, Token::CloseArray);
                        }
                        Token::OpenDict => {
                            *at += 1;
                            Dict::skip(tokens, at, Token::OpenDict, Token::CloseDict);
                        }
                        _ => *at += 1,
                    }
                }
                if dicts.is_empty() {
                    Value::Names(names)
                } else {
                    Value::Dicts(dicts)
                }
            }
            _ => Value::Other,
        }
    }

    fn skip(tokens: &[Token], at: &mut usize, open: Token, close: Token) {
        let mut depth = 1usize;
        while let Some(token) = tokens.get(*at) {
            *at += 1;
            if *token == open {
                depth += 1;
            } else if *token == close {
                depth -= 1;
                if depth == 0 {
                    return;
                }
            }
        }
    }

    fn get(&self, key: &[u8]) -> Option<&Value> {
        self.entries.iter().find(|(k, _)| k == key).map(|(_, v)| v)
    }

    /// The value of `key` when it is a name.
    pub fn name(&self, key: &[u8]) -> Option<&[u8]> {
        match self.get(key)? {
            Value::Name(name) => Some(name),
            _ => None,
        }
    }

    /// The value of `key` when it is an integer, directly or through a
    /// reference to an integer object.
    pub fn integer(&self, key: &[u8], integers: &HashMap<(u32, u16), i64>) -> Option<i64> {
        match self.get(key)? {
            Value::Integer(n) => Some(*n),
            Value::Reference(n, g) => integers.get(&(*n, *g)).copied(),
            _ => None,
        }
    }

    fn names_of(&self, key: &[u8]) -> Vec<Vec<u8>> {
        match self.get(key) {
            Some(Value::Name(name)) => vec![name.clone()],
            Some(Value::Names(names)) => names.clone(),
            _ => Vec::new(),
        }
    }

    /// `/Filter`, as a list.
    pub fn filters(&self) -> Vec<Vec<u8>> {
        self.names_of(b"Filter")
    }

    /// `/DecodeParms` (or `/DP`), one per filter.
    fn decode_parms(&self) -> Vec<Dict> {
        match self.get(b"DecodeParms").or_else(|| self.get(b"DP")) {
            Some(Value::Dict(dict)) => vec![dict.clone()],
            Some(Value::Dicts(dicts)) => dicts.clone(),
            _ => Vec::new(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dictionaries_are_read() {
        let dict = Dict::parse(
            b"<< /Type /XObject /Subtype /Image /Width 640 /Height 12 0 R \
              /Filter [/ASCIIHexDecode /FlateDecode] /DecodeParms [null << /Predictor 12 >>] \
              /Name (a (nested) string >> here) >>",
        );
        let mut integers = HashMap::new();
        integers.insert((12, 0), 480);
        assert_eq!(dict.name(b"Subtype"), Some(&b"Image"[..]));
        assert_eq!(dict.integer(b"Width", &integers), Some(640));
        assert_eq!(dict.integer(b"Height", &integers), Some(480));
        assert_eq!(
            dict.filters(),
            vec![b"ASCIIHexDecode".to_vec(), b"FlateDecode".to_vec()]
        );
        assert_eq!(dict.decode_parms().len(), 2);
    }

    #[test]
    fn object_headers_and_integer_objects() {
        let bytes = b"1 0 obj << >> endobj\n12 0 obj\n480\nendobj\nxobj 3 obj";
        assert_eq!(object_header(bytes, 4), Some((1, 0)));
        assert_eq!(small_integers(bytes).get(&(12, 0)), Some(&480));
    }
}
