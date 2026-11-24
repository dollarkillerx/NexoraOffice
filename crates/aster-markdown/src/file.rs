//! Reading and writing the file a document is kept in.
//!
//! A file is read as UTF-8 unless it starts with a UTF-16 byte-order mark.
//! Anything else is refused with a reason rather than guessed at: a wrong
//! guess would rewrite the user's bytes on the next save. The application can
//! then offer the legacy encodings [`guess_encodings`] finds plausible and
//! read the file with [`decode_as`] in the one the user picks. Decoding is
//! strict in every encoding, so a wrong pick fails instead of turning bytes
//! into U+FFFD, and saving fails on a character the encoding cannot hold
//! instead of writing a substitute.
//!
//! In memory a line ends with `\n`. The style the file used is remembered and
//! put back on save, as are the encoding and a byte-order mark. A file whose
//! lines all end the same way therefore round-trips byte for byte; one that
//! mixes styles is written back in its most common style, and
//! [`TextFormat::mixed_line_endings`] says so, so the application can tell the
//! user before that happens.

use std::fmt;

use encoding_rs::{DecoderResult, EncoderResult};

const BOM: &[u8] = b"\xEF\xBB\xBF";
const BOM_UTF16LE: &[u8] = b"\xFF\xFE";
const BOM_UTF16BE: &[u8] = b"\xFE\xFF";

/// How much of a file [`guess_encodings`] looks at.
const GUESS_LIMIT: usize = 64 * 1024;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum LineEnding {
    /// `\n`: Unix, macOS.
    #[default]
    Lf,
    /// `\r\n`: Windows.
    CrLf,
    /// `\r`: classic Mac OS.
    Cr,
}

impl LineEnding {
    pub fn as_str(self) -> &'static str {
        match self {
            LineEnding::Lf => "\n",
            LineEnding::CrLf => "\r\n",
            LineEnding::Cr => "\r",
        }
    }

    /// The name editors show for it.
    pub fn label(self) -> &'static str {
        match self {
            LineEnding::Lf => "LF",
            LineEnding::CrLf => "CRLF",
            LineEnding::Cr => "CR",
        }
    }
}

/// The character encoding of a file.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum Encoding {
    #[default]
    Utf8,
    Utf16Le,
    Utf16Be,
    /// Simplified Chinese. Also reads GBK and GB2312, which it contains.
    Gb18030,
    /// Traditional Chinese.
    Big5,
    /// Japanese, as Windows writes it.
    ShiftJis,
    /// Japanese, as Unix systems used to write it.
    EucJp,
    /// Korean. Also reads the Windows extension of it (code page 949).
    EucKr,
    /// Western European, a superset of ISO 8859-1.
    Windows1252,
}

impl Encoding {
    /// Every encoding, in the order a menu lists them.
    pub const ALL: &'static [Encoding] = &[
        Encoding::Utf8,
        Encoding::Utf16Le,
        Encoding::Utf16Be,
        Encoding::Gb18030,
        Encoding::Big5,
        Encoding::ShiftJis,
        Encoding::EucJp,
        Encoding::EucKr,
        Encoding::Windows1252,
    ];

    /// The name editors show for it.
    pub fn label(self) -> &'static str {
        match self {
            Encoding::Utf8 => "UTF-8",
            Encoding::Utf16Le => "UTF-16 LE",
            Encoding::Utf16Be => "UTF-16 BE",
            Encoding::Gb18030 => "GB18030",
            Encoding::Big5 => "Big5",
            Encoding::ShiftJis => "Shift_JIS",
            Encoding::EucJp => "EUC-JP",
            Encoding::EucKr => "EUC-KR",
            Encoding::Windows1252 => "Windows-1252",
        }
    }

    /// A name for storing the choice. It never changes.
    pub fn key(self) -> &'static str {
        match self {
            Encoding::Utf8 => "utf-8",
            Encoding::Utf16Le => "utf-16le",
            Encoding::Utf16Be => "utf-16be",
            Encoding::Gb18030 => "gb18030",
            Encoding::Big5 => "big5",
            Encoding::ShiftJis => "shift_jis",
            Encoding::EucJp => "euc-jp",
            Encoding::EucKr => "euc-kr",
            Encoding::Windows1252 => "windows-1252",
        }
    }

    /// The encoding [`Encoding::key`] returned `key` for.
    pub fn from_key(key: &str) -> Option<Encoding> {
        Encoding::ALL
            .iter()
            .copied()
            .find(|encoding| encoding.key() == key)
    }

    fn to_encoding_rs(self) -> &'static encoding_rs::Encoding {
        match self {
            Encoding::Utf8 => encoding_rs::UTF_8,
            Encoding::Utf16Le => encoding_rs::UTF_16LE,
            Encoding::Utf16Be => encoding_rs::UTF_16BE,
            Encoding::Gb18030 => encoding_rs::GB18030,
            Encoding::Big5 => encoding_rs::BIG5,
            Encoding::ShiftJis => encoding_rs::SHIFT_JIS,
            Encoding::EucJp => encoding_rs::EUC_JP,
            Encoding::EucKr => encoding_rs::EUC_KR,
            Encoding::Windows1252 => encoding_rs::WINDOWS_1252,
        }
    }

    fn is_japanese(self) -> bool {
        matches!(self, Encoding::ShiftJis | Encoding::EucJp)
    }
}

/// What must be put back when the text is written out again.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct TextFormat {
    pub line_ending: LineEnding,
    pub encoding: Encoding,
    /// The file started with a byte-order mark: the UTF-8 one, or for UTF-16
    /// the UTF-16 one in its byte order. The other encodings have none, and
    /// for them this is ignored.
    pub bom: bool,
    /// The file used more than one line-ending style. Saving unifies them to
    /// `line_ending`.
    pub mixed_line_endings: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Decoded {
    /// The text with every line ending turned into `\n` and no byte-order mark.
    pub text: String,
    pub format: TextFormat,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DecodeError {
    /// The bytes are not valid UTF-8. `offset` is where the first invalid
    /// sequence starts.
    NotUtf8 { offset: usize },
    /// The bytes are not valid in `encoding`, which is not UTF-8. `offset` is
    /// where the first invalid sequence starts.
    Malformed { encoding: Encoding, offset: usize },
}

impl fmt::Display for DecodeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            DecodeError::NotUtf8 { offset } => {
                write!(
                    f,
                    "the file is not valid UTF-8 (first bad byte at {offset})"
                )
            }
            DecodeError::Malformed { encoding, offset } => {
                write!(
                    f,
                    "the file is not valid {} (first bad byte at {offset})",
                    encoding.label()
                )
            }
        }
    }
}

impl std::error::Error for DecodeError {}

/// A character the file's encoding has no bytes for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct EncodeError {
    pub ch: char,
    /// Where the character starts in the text, which uses `\n`.
    pub offset: usize,
}

impl fmt::Display for EncodeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "the character {:?} (U+{:04X}) at {} cannot be written in this encoding",
            self.ch, self.ch as u32, self.offset
        )
    }
}

impl std::error::Error for EncodeError {}

/// Turn the bytes of a file into text. The file is UTF-16 if it starts with a
/// UTF-16 byte-order mark and UTF-8 otherwise.
pub fn decode(bytes: &[u8]) -> Result<Decoded, DecodeError> {
    let encoding = if bytes.starts_with(BOM_UTF16LE) {
        Encoding::Utf16Le
    } else if bytes.starts_with(BOM_UTF16BE) {
        Encoding::Utf16Be
    } else {
        Encoding::Utf8
    };
    decode_as(bytes, encoding)
}

/// Turn the bytes of a file into text in the given encoding. A byte-order
/// mark is taken out for UTF-8 and UTF-16 and remembered. Bytes the encoding
/// does not allow are an error, never a replacement character.
pub fn decode_as(bytes: &[u8], encoding: Encoding) -> Result<Decoded, DecodeError> {
    let malformed = |offset| DecodeError::Malformed { encoding, offset };
    match encoding {
        Encoding::Utf8 => {
            let bom = bytes.starts_with(BOM);
            let body = if bom { &bytes[BOM.len()..] } else { bytes };
            let raw = std::str::from_utf8(body).map_err(|err| DecodeError::NotUtf8 {
                offset: err.valid_up_to() + if bom { BOM.len() } else { 0 },
            })?;
            Ok(split_lines(raw, encoding, bom))
        }
        Encoding::Utf16Le | Encoding::Utf16Be => {
            let big_endian = encoding == Encoding::Utf16Be;
            let mark = if big_endian { BOM_UTF16BE } else { BOM_UTF16LE };
            let bom = bytes.starts_with(mark);
            let start = if bom { mark.len() } else { 0 };
            let raw = decode_utf16(&bytes[start..], big_endian)
                .map_err(|offset| malformed(start + offset))?;
            Ok(split_lines(&raw, encoding, bom))
        }
        _ => {
            let raw = decode_strict(encoding, bytes, true).map_err(malformed)?;
            Ok(split_lines(&raw, encoding, false))
        }
    }
}

/// UTF-16 without a byte-order mark. The error is the offset of the first
/// unit that is not part of a valid character.
fn decode_utf16(bytes: &[u8], big_endian: bool) -> Result<String, usize> {
    let (pairs, odd) = bytes.as_chunks::<2>();
    let units = pairs.iter().map(|pair| {
        if big_endian {
            u16::from_be_bytes(*pair)
        } else {
            u16::from_le_bytes(*pair)
        }
    });
    let mut text = String::with_capacity(bytes.len());
    let mut at = 0;
    for unit in char::decode_utf16(units) {
        let ch = unit.map_err(|_| at)?;
        at += 2 * ch.len_utf16();
        text.push(ch);
    }
    if !odd.is_empty() {
        // A byte left over: half a unit.
        return Err(bytes.len() - 1);
    }
    Ok(text)
}

/// A legacy encoding, with no replacement characters. The error is the
/// offset of the first malformed sequence. With `last` false the bytes may
/// stop in the middle of a character, as a sample cut from a file does.
fn decode_strict(encoding: Encoding, bytes: &[u8], last: bool) -> Result<String, usize> {
    let mut decoder = encoding.to_encoding_rs().new_decoder_without_bom_handling();
    let capacity = decoder
        .max_utf8_buffer_length_without_replacement(bytes.len())
        .unwrap_or(bytes.len());
    let mut text = String::with_capacity(capacity);
    let mut read = 0;
    loop {
        let (result, n) =
            decoder.decode_to_string_without_replacement(&bytes[read..], &mut text, last);
        read += n;
        match result {
            DecoderResult::InputEmpty => return Ok(text),
            DecoderResult::OutputFull => text.reserve(bytes.len() - read + 16),
            DecoderResult::Malformed(bad, after) => {
                return Err(read.saturating_sub(usize::from(bad) + usize::from(after)));
            }
        }
    }
}

fn split_lines(raw: &str, encoding: Encoding, bom: bool) -> Decoded {
    let (mut lf, mut crlf, mut cr) = (0usize, 0usize, 0usize);
    let mut text = String::with_capacity(raw.len());
    let mut rest = raw;
    while let Some(at) = rest.find(['\r', '\n']) {
        text.push_str(&rest[..at]);
        text.push('\n');
        let tail = &rest[at..];
        rest = if let Some(after) = tail.strip_prefix("\r\n") {
            crlf += 1;
            after
        } else if let Some(after) = tail.strip_prefix('\r') {
            cr += 1;
            after
        } else {
            lf += 1;
            &tail[1..]
        };
    }
    text.push_str(rest);

    // The most common style wins; a tie goes to the earlier one in this list,
    // and a file with no line breaks at all is LF.
    let line_ending = [
        (lf, LineEnding::Lf),
        (crlf, LineEnding::CrLf),
        (cr, LineEnding::Cr),
    ]
    .into_iter()
    .rev()
    .max_by_key(|(count, _)| *count)
    .map_or(LineEnding::Lf, |(_, ending)| ending);
    let styles = [lf, crlf, cr]
        .into_iter()
        .filter(|count| *count > 0)
        .count();

    Decoded {
        text,
        format: TextFormat {
            line_ending,
            encoding,
            bom,
            mixed_line_endings: styles > 1,
        },
    }
}

/// The legacy encodings, not UTF-8 or UTF-16, that read the bytes without an
/// error, the most plausible first. Only the first 64 KB are looked at, so
/// [`decode_as`] can still fail further on.
///
/// Each candidate gets a score, the sum over the characters it reads:
///
/// - Control characters other than tab, CR and LF, and private-use
///   characters: -10. Bytes read in the wrong encoding often land there.
/// - Half-width katakana: -3. Real text rarely has them, but Chinese bytes
///   read as Shift_JIS turn into long runs of them.
/// - Hiragana and katakana: +2 in a Japanese encoding, -1 in the others.
/// - Hangul syllables: +3 in EUC-KR when they come from the main KS X 1001
///   table, 0 from the Windows extension (rare syllables), -1 elsewhere.
/// - CJK ideographs: +2 for the encoding's first, most common level (GB2312
///   level 1, Big5 A440-C67E, JIS level 1), +1 for its second level, 0 for
///   the rest. In EUC-KR -2: Korean text seldom has hanja, while Chinese
///   bytes read as EUC-KR are full of them.
/// - Rarer ideographs (extension A, compatibility, beyond the BMP): -2.
/// - CJK and full-width punctuation: +1.
/// - Anything else, ASCII included: 0.
///
/// The CJK encodings are sorted by score, a tie keeping the order of
/// [`Encoding::ALL`], and Windows-1252, which reads any bytes, comes last.
///
/// The score only tells encodings apart where their byte layouts differ.
/// GB2312 and EUC-KR share theirs, so Korean text with only common hangul
/// reads as common Chinese characters in GB18030 too; the higher score of
/// hangul is what puts EUC-KR first there. Kanji-only Japanese text in EUC-JP
/// reads as Chinese in GB18030 just as well, and a tie goes to GB18030;
/// Korean written only in hanja comes out as Chinese too. EUC-JP and EUC-KR
/// are still in the list for the user to pick.
pub fn guess_encodings(bytes: &[u8]) -> Vec<Encoding> {
    let last = bytes.len() <= GUESS_LIMIT;
    let sample = &bytes[..bytes.len().min(GUESS_LIMIT)];
    let mut scored: Vec<(i64, Encoding)> = [
        Encoding::Gb18030,
        Encoding::Big5,
        Encoding::ShiftJis,
        Encoding::EucJp,
        Encoding::EucKr,
    ]
    .into_iter()
    .filter(|encoding| decode_strict(*encoding, sample, last).is_ok())
    .map(|encoding| (score(encoding, sample), encoding))
    .collect();
    scored.sort_by_key(|(score, _)| std::cmp::Reverse(*score));
    let mut guesses: Vec<Encoding> = scored.into_iter().map(|(_, encoding)| encoding).collect();
    if decode_strict(Encoding::Windows1252, sample, last).is_ok() {
        guesses.push(Encoding::Windows1252);
    }
    guesses
}

/// The score [`guess_encodings`] describes. The bytes are known to decode,
/// so they can be walked one character's sequence at a time.
fn score(encoding: Encoding, bytes: &[u8]) -> i64 {
    let mut decoder = encoding.to_encoding_rs().new_decoder_without_bom_handling();
    let mut out = [0u8; 16];
    let mut total = 0i64;
    let mut at = 0;
    while at < bytes.len() {
        let len = sequence_len(encoding, &bytes[at..]);
        let Some(seq) = bytes.get(at..at + len) else {
            // A character cut off by the end of the sample.
            break;
        };
        at += len;
        if seq[0] < 0x80 {
            total += score_char(encoding, seq, seq[0] as char);
            continue;
        }
        let (result, _, written) = decoder.decode_to_utf8_without_replacement(seq, &mut out, false);
        let ch = std::str::from_utf8(&out[..written])
            .ok()
            .and_then(|text| text.chars().next());
        match (result, ch) {
            (DecoderResult::InputEmpty, Some(ch)) => total += score_char(encoding, seq, ch),
            // The walk lost step with the decoder; start it afresh.
            _ => {
                total -= 10;
                decoder = encoding.to_encoding_rs().new_decoder_without_bom_handling();
            }
        }
    }
    total
}

/// How many bytes the character starting at `bytes[0]` takes, for bytes that
/// are valid in `encoding`.
fn sequence_len(encoding: Encoding, bytes: &[u8]) -> usize {
    let lead = bytes[0];
    if lead < 0x80 {
        return 1;
    }
    match encoding {
        Encoding::Gb18030 => match bytes.get(1) {
            _ if lead == 0x80 => 1,
            Some(0x30..=0x39) => 4,
            _ => 2,
        },
        Encoding::ShiftJis if lead == 0x80 || (0xA1..=0xDF).contains(&lead) => 1,
        Encoding::EucJp if lead == 0x8F => 3,
        Encoding::Windows1252 => 1,
        _ => 2,
    }
}

fn score_char(encoding: Encoding, seq: &[u8], ch: char) -> i64 {
    let lead = seq[0];
    let trail = seq.get(1).copied().unwrap_or(0);
    let two_byte = seq.len() == 2;
    match ch {
        '\t' | '\n' | '\r' | ' '..='~' => 0,
        '\0'..='\u{1F}' | '\u{7F}'..='\u{9F}' | '\u{E000}'..='\u{F8FF}' => -10,
        '\u{FF61}'..='\u{FF9F}' => -3,
        '\u{3040}'..='\u{30FF}' => {
            if encoding.is_japanese() {
                2
            } else {
                -1
            }
        }
        '\u{AC00}'..='\u{D7A3}' => match encoding {
            Encoding::EucKr if (0xB0..=0xC8).contains(&lead) && trail >= 0xA1 => 3,
            Encoding::EucKr => 0,
            _ => -1,
        },
        '\u{4E00}'..='\u{9FFF}' => match encoding {
            Encoding::Gb18030 if two_byte && trail >= 0xA1 => match lead {
                0xB0..=0xD7 => 2,
                0xD8..=0xF7 => 1,
                _ => 0,
            },
            Encoding::Big5 => match lead {
                0xA4..=0xC5 => 2,
                0xC6 if trail <= 0x7E => 2,
                0xC9..=0xF9 => 1,
                _ => 0,
            },
            Encoding::ShiftJis => match lead {
                0x88..=0x97 => 2,
                0x98 if trail <= 0x72 => 2,
                0x98..=0x9F | 0xE0..=0xEA => 1,
                _ => 0,
            },
            Encoding::EucJp if two_byte => match lead {
                0xB0..=0xCF => 2,
                0xD0..=0xF4 => 1,
                _ => 0,
            },
            Encoding::EucKr => -2,
            _ => 0,
        },
        '\u{3400}'..='\u{4DBF}' | '\u{F900}'..='\u{FAFF}' | '\u{10000}'.. => -2,
        '\u{3000}'..='\u{303F}' | '\u{FF01}'..='\u{FF60}' => 1,
        _ => 0,
    }
}

/// Turn text back into the bytes of a file. `text` uses `\n`. Fails on the
/// first character the encoding cannot hold; UTF-8 and UTF-16 hold them all.
pub fn encode(text: &str, format: &TextFormat) -> Result<Vec<u8>, EncodeError> {
    encode_chunks(std::iter::once(text), text.len(), format)
}

/// [`encode`] for a rope: the same bytes, without first copying the text
/// into one string.
pub fn encode_rope(text: &ropey::Rope, format: &TextFormat) -> Result<Vec<u8>, EncodeError> {
    encode_chunks(text.chunks(), text.len_bytes(), format)
}

fn encode_chunks<'a>(
    chunks: impl Iterator<Item = &'a str> + Clone,
    len: usize,
    format: &TextFormat,
) -> Result<Vec<u8>, EncodeError> {
    match format.encoding {
        Encoding::Utf8 => Ok(encode_utf8(chunks, len, format)),
        Encoding::Utf16Le | Encoding::Utf16Be => Ok(encode_utf16(chunks, len, format)),
        _ => encode_legacy(chunks, len, format),
    }
}

fn encode_utf8<'a>(
    chunks: impl Iterator<Item = &'a str> + Clone,
    len: usize,
    format: &TextFormat,
) -> Vec<u8> {
    let ending = format.line_ending.as_str();
    let breaks = if format.line_ending == LineEnding::Lf {
        0
    } else {
        chunks
            .clone()
            .map(|chunk| chunk.bytes().filter(|byte| *byte == b'\n').count())
            .sum()
    };
    let mut bytes = Vec::with_capacity(
        len + breaks * (ending.len() - 1) + if format.bom { BOM.len() } else { 0 },
    );
    if format.bom {
        bytes.extend_from_slice(BOM);
    }
    for chunk in chunks {
        if format.line_ending == LineEnding::Lf {
            bytes.extend_from_slice(chunk.as_bytes());
            continue;
        }
        let mut lines = chunk.split('\n');
        if let Some(first) = lines.next() {
            bytes.extend_from_slice(first.as_bytes());
        }
        for line in lines {
            bytes.extend_from_slice(ending.as_bytes());
            bytes.extend_from_slice(line.as_bytes());
        }
    }
    bytes
}

fn encode_utf16<'a>(
    chunks: impl Iterator<Item = &'a str>,
    len: usize,
    format: &TextFormat,
) -> Vec<u8> {
    let big_endian = format.encoding == Encoding::Utf16Be;
    let mut bytes = Vec::with_capacity(2 * len + 2);
    let mut push = |unit: u16| {
        bytes.extend_from_slice(&if big_endian {
            unit.to_be_bytes()
        } else {
            unit.to_le_bytes()
        });
    };
    if format.bom {
        push(0xFEFF);
    }
    let mut units = [0u16; 2];
    for chunk in chunks {
        for ch in chunk.chars() {
            if ch == '\n' {
                format
                    .line_ending
                    .as_str()
                    .bytes()
                    .for_each(|byte| push(u16::from(byte)));
            } else {
                ch.encode_utf16(&mut units)
                    .iter()
                    .for_each(|unit| push(*unit));
            }
        }
    }
    bytes
}

/// The legacy encodings are all ASCII-compatible, so a line ending is written
/// as its ASCII bytes between the encoded lines.
fn encode_legacy<'a>(
    chunks: impl Iterator<Item = &'a str>,
    len: usize,
    format: &TextFormat,
) -> Result<Vec<u8>, EncodeError> {
    let ending = format.line_ending.as_str().as_bytes();
    let mut encoder = format.encoding.to_encoding_rs().new_encoder();
    let mut bytes = Vec::with_capacity(len);
    let mut offset = 0;
    for chunk in chunks {
        let mut lines = chunk.split('\n');
        if let Some(first) = lines.next() {
            encode_piece(&mut encoder, first, offset, false, &mut bytes)?;
            offset += first.len();
        }
        for line in lines {
            bytes.extend_from_slice(ending);
            offset += 1;
            encode_piece(&mut encoder, line, offset, false, &mut bytes)?;
            offset += line.len();
        }
    }
    encode_piece(&mut encoder, "", offset, true, &mut bytes)?;
    Ok(bytes)
}

/// Encode `piece`, which starts at `offset` in the text. `last` ends the
/// stream.
fn encode_piece(
    encoder: &mut encoding_rs::Encoder,
    piece: &str,
    offset: usize,
    last: bool,
    bytes: &mut Vec<u8>,
) -> Result<(), EncodeError> {
    let mut read = 0;
    loop {
        let rest = &piece[read..];
        let room = encoder
            .max_buffer_length_from_utf8_without_replacement(rest.len())
            .unwrap_or(rest.len());
        bytes.reserve(room);
        let (result, n) = encoder.encode_from_utf8_to_vec_without_replacement(rest, bytes, last);
        read += n;
        match result {
            EncoderResult::InputEmpty => return Ok(()),
            EncoderResult::OutputFull => {}
            EncoderResult::Unmappable(ch) => {
                return Err(EncodeError {
                    ch,
                    offset: offset + read - ch.len_utf8(),
                });
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn round_trip(bytes: &[u8]) -> Vec<u8> {
        let decoded = decode(bytes).expect("valid UTF-8");
        encode(&decoded.text, &decoded.format).unwrap()
    }

    /// Bytes of `text` in `encoding`, made by encoding_rs itself so the tests
    /// do not depend on this module's encoder.
    fn legacy(text: &str, encoding: Encoding) -> Vec<u8> {
        let (bytes, _, unmappable) = encoding.to_encoding_rs().encode(text);
        assert!(!unmappable, "{text:?} in {}", encoding.label());
        bytes.into_owned()
    }

    #[test]
    fn text_in_memory_always_uses_lf() {
        for bytes in [&b"a\nb\n"[..], b"a\r\nb\r\n", b"a\rb\r"] {
            assert_eq!(decode(bytes).unwrap().text, "a\nb\n");
        }
    }

    #[test]
    fn the_line_ending_style_is_detected() {
        let style = |bytes: &[u8]| decode(bytes).unwrap().format.line_ending;
        assert_eq!(style(b"a\nb\n"), LineEnding::Lf);
        assert_eq!(style(b"a\r\nb\r\n"), LineEnding::CrLf);
        assert_eq!(style(b"a\rb\r"), LineEnding::Cr);
        assert_eq!(style(b"no line break"), LineEnding::Lf);
        assert_eq!(style(b""), LineEnding::Lf);
    }

    #[test]
    fn uniform_files_round_trip_byte_for_byte() {
        let samples: [&[u8]; 9] = [
            b"",
            b"one line, no break at the end",
            b"# Title\n\nBody.\n",
            b"# Title\r\n\r\nBody.\r\n",
            b"old mac\rsecond\r",
            b"\xEF\xBB\xBFwith a byte-order mark\r\n",
            "中文和日本語\r\n第二行\r\n".as_bytes(),
            b"\n\n\n",
            b"trailing spaces  \n\ttab\n",
        ];
        for sample in samples {
            assert_eq!(
                round_trip(sample),
                sample,
                "{:?}",
                String::from_utf8_lossy(sample)
            );
            let format = decode(sample).unwrap().format;
            assert!(!format.mixed_line_endings);
            assert_eq!(format.encoding, Encoding::Utf8);
        }
    }

    #[test]
    fn mixed_line_endings_are_flagged_and_unified_to_the_commonest() {
        let decoded = decode(b"a\r\nb\r\nc\nd").unwrap();
        assert!(decoded.format.mixed_line_endings);
        assert_eq!(decoded.format.line_ending, LineEnding::CrLf);
        assert_eq!(decoded.text, "a\nb\nc\nd");
        assert_eq!(
            encode(&decoded.text, &decoded.format).unwrap(),
            b"a\r\nb\r\nc\r\nd"
        );

        // A tie goes to LF.
        let tie = decode(b"a\nb\r\nc").unwrap();
        assert!(tie.format.mixed_line_endings);
        assert_eq!(tie.format.line_ending, LineEnding::Lf);
    }

    #[test]
    fn a_byte_order_mark_is_kept_out_of_the_text_and_put_back() {
        let decoded = decode(b"\xEF\xBB\xBFhello").unwrap();
        assert!(decoded.format.bom);
        assert_eq!(decoded.text, "hello");
        assert_eq!(
            encode("hello!", &decoded.format).unwrap(),
            b"\xEF\xBB\xBFhello!"
        );
    }

    #[test]
    fn files_that_are_not_utf8_are_refused() {
        // "中文" in GBK.
        assert_eq!(
            decode(b"ok \xD6\xD0\xCE\xC4"),
            Err(DecodeError::NotUtf8 { offset: 3 })
        );
        // Latin-1 e-acute.
        assert_eq!(decode(b"caf\xE9"), Err(DecodeError::NotUtf8 { offset: 3 }));
        // The offset counts the byte-order mark.
        assert_eq!(
            decode(b"\xEF\xBB\xBFa\xFF"),
            Err(DecodeError::NotUtf8 { offset: 4 })
        );
        assert!(
            DecodeError::NotUtf8 { offset: 3 }
                .to_string()
                .contains("UTF-8")
        );
    }

    #[test]
    fn new_lines_typed_into_a_crlf_document_are_written_as_crlf() {
        let decoded = decode(b"a\r\n").unwrap();
        let edited = format!("{}b\nc\n", decoded.text);
        assert_eq!(
            encode(&edited, &decoded.format).unwrap(),
            b"a\r\nb\r\nc\r\n"
        );
    }

    #[test]
    fn labels_and_strings_match() {
        for ending in [LineEnding::Lf, LineEnding::CrLf, LineEnding::Cr] {
            assert!(!ending.label().is_empty());
            assert!(!ending.as_str().is_empty());
        }
        assert_eq!(LineEnding::default(), LineEnding::Lf);
    }

    #[test]
    fn encodings_have_labels_and_stable_keys() {
        assert_eq!(Encoding::default(), Encoding::Utf8);
        assert_eq!(TextFormat::default().encoding, Encoding::Utf8);
        assert_eq!(Encoding::ALL.len(), 9);
        for encoding in Encoding::ALL {
            assert_eq!(Encoding::from_key(encoding.key()), Some(*encoding));
            assert!(!encoding.label().is_empty());
        }
        let keys: Vec<&str> = Encoding::ALL.iter().map(|e| e.key()).collect();
        assert_eq!(
            keys,
            [
                "utf-8",
                "utf-16le",
                "utf-16be",
                "gb18030",
                "big5",
                "shift_jis",
                "euc-jp",
                "euc-kr",
                "windows-1252"
            ]
        );
        assert_eq!(Encoding::ShiftJis.label(), "Shift_JIS");
        assert_eq!(Encoding::Utf16Le.label(), "UTF-16 LE");
        assert_eq!(Encoding::from_key("latin-1"), None);
    }

    #[test]
    fn utf16_with_a_byte_order_mark_is_read_and_written_back() {
        let le = b"\xFF\xFEh\x00i\x00\r\x00\n\x00=\xD8\x00\xDE";
        let decoded = decode(le).unwrap();
        assert_eq!(decoded.text, "hi\n\u{1F600}");
        assert_eq!(decoded.format.encoding, Encoding::Utf16Le);
        assert_eq!(decoded.format.line_ending, LineEnding::CrLf);
        assert!(decoded.format.bom);
        assert_eq!(encode(&decoded.text, &decoded.format).unwrap(), le);

        let be = b"\xFE\xFF\x00h\x00i\x00\n\x4E\x2D";
        let decoded = decode(be).unwrap();
        assert_eq!(decoded.text, "hi\n中");
        assert_eq!(decoded.format.encoding, Encoding::Utf16Be);
        assert_eq!(encode(&decoded.text, &decoded.format).unwrap(), be);

        // Without a byte-order mark only when asked for, and none is added.
        let bare = b"a\x00\n\x00";
        let decoded = decode_as(bare, Encoding::Utf16Le).unwrap();
        assert_eq!(decoded.text, "a\n");
        assert!(!decoded.format.bom);
        assert_eq!(encode(&decoded.text, &decoded.format).unwrap(), bare);
    }

    #[test]
    fn malformed_utf16_is_refused_with_the_offset() {
        let malformed = |offset| DecodeError::Malformed {
            encoding: Encoding::Utf16Le,
            offset,
        };
        // A high surrogate followed by an ordinary character.
        assert_eq!(decode(b"\xFF\xFEa\x00=\xD8b\x00"), Err(malformed(4)));
        // A lone low surrogate.
        assert_eq!(decode(b"\xFF\xFE\x00\xDE"), Err(malformed(2)));
        // A high surrogate at the very end.
        assert_eq!(decode(b"\xFF\xFEa\x00=\xD8"), Err(malformed(4)));
        // Half a unit at the end.
        assert_eq!(decode(b"\xFF\xFEa\x00b"), Err(malformed(4)));
        assert_eq!(
            decode(b"\xFE\xFF\xDC\x00"),
            Err(DecodeError::Malformed {
                encoding: Encoding::Utf16Be,
                offset: 2
            })
        );
        assert!(malformed(4).to_string().contains("UTF-16 LE"));
    }

    #[test]
    fn legacy_files_round_trip_byte_for_byte() {
        let samples = [
            (
                Encoding::Gb18030,
                "# 标题\r\n\r\n简体中文，还有“引号”和€。\r\n",
            ),
            (Encoding::Gb18030, "生僻字𠀀也能写回去\n"),
            (Encoding::Big5, "# 標題\r\n\r\n繁體中文，臺灣。\r\n"),
            (
                Encoding::ShiftJis,
                "# 見出し\r\n\r\nひらがなとカタカナ、ｶﾀｶﾅ。\r\n",
            ),
            (Encoding::EucJp, "# 見出し\n\n日本語の文章です。\n"),
            (Encoding::EucKr, "# 제목\r\n\r\n한국어 문장입니다. 漢字\r\n"),
            (Encoding::Windows1252, "Café, naïve – “quotes” €\r\n"),
        ];
        for (encoding, text) in samples {
            let bytes = legacy(text, encoding);
            assert!(std::str::from_utf8(&bytes).is_err() || bytes.is_ascii());
            let decoded = decode_as(&bytes, encoding).unwrap();
            assert_eq!(decoded.text, text.replace("\r\n", "\n"));
            assert_eq!(decoded.format.encoding, encoding);
            assert!(!decoded.format.bom);
            assert_eq!(
                encode(&decoded.text, &decoded.format).unwrap(),
                bytes,
                "{}",
                encoding.label()
            );
            let rope = ropey::Rope::from_str(&decoded.text);
            assert_eq!(encode_rope(&rope, &decoded.format).unwrap(), bytes);
        }
    }

    #[test]
    fn known_legacy_bytes_decode_to_the_right_text() {
        let cases: [(Encoding, &[u8], &str); 5] = [
            (Encoding::Gb18030, b"\xD6\xD0\xCE\xC4", "中文"),
            (Encoding::Big5, b"\xA4\xA4\xA4\xE5", "中文"),
            (Encoding::ShiftJis, b"\x93\xFA\x96\x7B", "日本"),
            (Encoding::EucJp, b"\xC6\xFC\xCB\xDC", "日本"),
            (Encoding::EucKr, b"\xC7\xD1\xB1\xB9", "한국"),
        ];
        for (encoding, bytes, text) in cases {
            assert_eq!(decode_as(bytes, encoding).unwrap().text, text);
            let format = TextFormat {
                encoding,
                ..TextFormat::default()
            };
            assert_eq!(encode(text, &format).unwrap(), bytes);
        }
        // The legacy encodings have no byte-order mark to write.
        let format = TextFormat {
            encoding: Encoding::Gb18030,
            bom: true,
            ..TextFormat::default()
        };
        assert_eq!(encode("中文", &format).unwrap(), b"\xD6\xD0\xCE\xC4");
    }

    #[test]
    fn malformed_legacy_bytes_are_refused_with_the_offset() {
        let error = |encoding, offset| Err(DecodeError::Malformed { encoding, offset });
        // A lead byte followed by ASCII.
        assert_eq!(
            decode_as(b"ab\xD6 x", Encoding::Gb18030),
            error(Encoding::Gb18030, 2)
        );
        // A lead byte cut off by the end of the file.
        assert_eq!(
            decode_as(b"\xD6\xD0\xCE", Encoding::Gb18030),
            error(Encoding::Gb18030, 2)
        );
        assert_eq!(
            decode_as(b"ok\x80", Encoding::Big5),
            error(Encoding::Big5, 2)
        );
        // 0xA0 is no character in Shift_JIS.
        assert_eq!(
            decode_as(b"\x93\xFA\xA0", Encoding::ShiftJis),
            error(Encoding::ShiftJis, 2)
        );
        assert_eq!(
            decode_as(b"x\xC6\x20", Encoding::EucJp),
            error(Encoding::EucJp, 1)
        );
        assert_eq!(
            decode_as(b"\xC7\xD1\xFF", Encoding::EucKr),
            error(Encoding::EucKr, 2)
        );
        // UTF-8 asked for explicitly is refused as before.
        assert_eq!(
            decode_as(b"caf\xE9", Encoding::Utf8),
            Err(DecodeError::NotUtf8 { offset: 3 })
        );
        // Windows-1252 has a character for every byte.
        assert!(decode_as(b"\x81\x8D\x8F\x90\x9D\xFF", Encoding::Windows1252).is_ok());
        assert!(
            DecodeError::Malformed {
                encoding: Encoding::Big5,
                offset: 2
            }
            .to_string()
            .contains("Big5")
        );
    }

    #[test]
    fn a_character_the_encoding_cannot_hold_fails_the_save() {
        let format = |encoding| TextFormat {
            encoding,
            line_ending: LineEnding::CrLf,
            ..TextFormat::default()
        };
        // The offset is in the `\n` text, not in the bytes written.
        assert_eq!(
            encode("日本\n語😀", &format(Encoding::ShiftJis)),
            Err(EncodeError {
                ch: '😀',
                offset: 10
            })
        );
        assert_eq!(
            encode("한국어\n们", &format(Encoding::EucKr)),
            Err(EncodeError {
                ch: '们',
                offset: 10
            })
        );
        assert_eq!(
            encode("price: €5 and 中", &format(Encoding::Windows1252)),
            Err(EncodeError {
                ch: '中',
                offset: 16
            })
        );
        assert_eq!(
            encode("简体", &format(Encoding::Big5)),
            Err(EncodeError {
                ch: '简',
                offset: 0
            })
        );
        // A rope reports the same offset across its chunks.
        let long = format!("{}\n😀", "日本語".repeat(2000));
        let rope = ropey::Rope::from_str(&long);
        assert!(rope.chunks().count() > 1);
        assert_eq!(
            encode_rope(&rope, &format(Encoding::EucJp)),
            Err(EncodeError {
                ch: '😀',
                offset: long.len() - '😀'.len_utf8()
            })
        );
        // UTF-8 and UTF-16 hold everything.
        for encoding in [Encoding::Utf8, Encoding::Utf16Le, Encoding::Utf16Be] {
            assert!(encode("😀 中 € 한", &format(encoding)).is_ok());
        }
        let message = EncodeError {
            ch: '😀',
            offset: 3,
        }
        .to_string();
        assert!(message.contains("U+1F600"), "{message}");
    }

    const CHINESE_SIMPLIFIED: &str = "这是一个用简体中文写的文件，我们用它来测试编码的识别。\
        在这里写一些常用的汉字：人民、国家、经济、发展、社会、时间、问题、工作。\n";
    const CHINESE_TRADITIONAL: &str = "這是一個用繁體中文寫的檔案，我們用它來測試編碼的識別。\
        在這裡寫一些常用的漢字：人民、國家、經濟、發展、社會、時間、問題、工作。\n";
    const JAPANESE: &str = "これは日本語で書かれたファイルです。文字コードの判定を試します。\
        今日は天気が良いので、公園まで散歩に行きました。\n";
    const KOREAN: &str = "이것은 한국어로 쓴 파일입니다. 문자 인코딩 감지를 시험합니다. \
        오늘은 날씨가 좋아서 공원에 산책하러 갔습니다.\n";

    #[test]
    fn the_right_encoding_is_guessed_first() {
        let cases = [
            (CHINESE_SIMPLIFIED, Encoding::Gb18030),
            (CHINESE_TRADITIONAL, Encoding::Big5),
            (JAPANESE, Encoding::ShiftJis),
            (JAPANESE, Encoding::EucJp),
            (KOREAN, Encoding::EucKr),
        ];
        for (text, encoding) in cases {
            let bytes = legacy(text, encoding);
            let guesses = guess_encodings(&bytes);
            assert_eq!(guesses.first(), Some(&encoding), "{guesses:?}");
            assert_eq!(guesses.last(), Some(&Encoding::Windows1252));
            assert!(!guesses.contains(&Encoding::Utf8));
            // Every guess reads the file.
            for guess in &guesses {
                assert!(decode_as(&bytes, *guess).is_ok(), "{guess:?}");
            }
        }
    }

    #[test]
    fn kanji_without_kana_is_ambiguous() {
        // EUC-JP and GB2312 share their byte layout, and without kana nothing
        // tells them apart, so the tie goes to GB18030.
        let bytes = legacy("日本国憲法", Encoding::EucJp);
        let guesses = guess_encodings(&bytes);
        assert_eq!(guesses.first(), Some(&Encoding::Gb18030));
        assert!(guesses.contains(&Encoding::EucJp));
    }

    #[test]
    fn western_text_guesses_windows_1252_only() {
        let bytes = legacy("Café au lait, naïve façade.\n", Encoding::Windows1252);
        assert_eq!(guess_encodings(&bytes), [Encoding::Windows1252]);
    }

    #[test]
    fn only_the_start_of_a_large_file_is_looked_at() {
        let mut bytes = legacy(&CHINESE_SIMPLIFIED.repeat(1500), Encoding::Gb18030);
        assert!(bytes.len() > GUESS_LIMIT);
        // Bytes past the limit that GB18030 refuses do not count.
        bytes.extend_from_slice(b"\xFF\xFF");
        assert_eq!(guess_encodings(&bytes).first(), Some(&Encoding::Gb18030));
        assert!(decode_as(&bytes, Encoding::Gb18030).is_err());
    }
}
