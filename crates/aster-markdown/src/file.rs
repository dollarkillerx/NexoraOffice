//! Reading and writing the file a document is kept in.
//!
//! Files are UTF-8. Anything else is refused with a reason rather than guessed
//! at: a wrong guess would rewrite the user's bytes on the next save.
//!
//! In memory a line ends with `\n`. The style the file used is remembered and
//! put back on save, as is a byte-order mark. A file whose lines all end the
//! same way therefore round-trips byte for byte; one that mixes styles is
//! written back in its most common style, and [`TextFormat::mixed_line_endings`]
//! says so, so the application can tell the user before that happens.

use std::fmt;

const BOM: &[u8] = b"\xEF\xBB\xBF";

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

/// What must be put back when the text is written out again.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct TextFormat {
    pub line_ending: LineEnding,
    /// The file started with a UTF-8 byte-order mark.
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
    /// The file starts with a UTF-16 byte-order mark.
    Utf16,
    /// The bytes are not valid UTF-8. `offset` is where the first invalid
    /// sequence starts.
    NotUtf8 { offset: usize },
}

impl fmt::Display for DecodeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            DecodeError::Utf16 => f.write_str("the file is UTF-16, not UTF-8"),
            DecodeError::NotUtf8 { offset } => {
                write!(
                    f,
                    "the file is not valid UTF-8 (first bad byte at {offset})"
                )
            }
        }
    }
}

impl std::error::Error for DecodeError {}

/// Turn the bytes of a file into text.
pub fn decode(bytes: &[u8]) -> Result<Decoded, DecodeError> {
    if bytes.starts_with(b"\xFF\xFE") || bytes.starts_with(b"\xFE\xFF") {
        return Err(DecodeError::Utf16);
    }
    let bom = bytes.starts_with(BOM);
    let body = if bom { &bytes[BOM.len()..] } else { bytes };
    let raw = std::str::from_utf8(body).map_err(|err| DecodeError::NotUtf8 {
        offset: err.valid_up_to() + if bom { BOM.len() } else { 0 },
    })?;

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

    Ok(Decoded {
        text,
        format: TextFormat {
            line_ending,
            bom,
            mixed_line_endings: styles > 1,
        },
    })
}

/// Turn text back into the bytes of a file. `text` uses `\n`.
pub fn encode(text: &str, format: &TextFormat) -> Vec<u8> {
    encode_chunks(std::iter::once(text), text.len(), format)
}

/// [`encode`] for a rope: the same bytes, without first copying the text
/// into one string.
pub fn encode_rope(text: &ropey::Rope, format: &TextFormat) -> Vec<u8> {
    encode_chunks(text.chunks(), text.len_bytes(), format)
}

fn encode_chunks<'a>(
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

#[cfg(test)]
mod tests {
    use super::*;

    fn round_trip(bytes: &[u8]) -> Vec<u8> {
        let decoded = decode(bytes).expect("valid UTF-8");
        encode(&decoded.text, &decoded.format)
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
            assert!(!decode(sample).unwrap().format.mixed_line_endings);
        }
    }

    #[test]
    fn mixed_line_endings_are_flagged_and_unified_to_the_commonest() {
        let decoded = decode(b"a\r\nb\r\nc\nd").unwrap();
        assert!(decoded.format.mixed_line_endings);
        assert_eq!(decoded.format.line_ending, LineEnding::CrLf);
        assert_eq!(decoded.text, "a\nb\nc\nd");
        assert_eq!(encode(&decoded.text, &decoded.format), b"a\r\nb\r\nc\r\nd");

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
        assert_eq!(encode("hello!", &decoded.format), b"\xEF\xBB\xBFhello!");
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
        assert_eq!(decode(b"\xFF\xFEh\x00i\x00"), Err(DecodeError::Utf16));
        assert_eq!(decode(b"\xFE\xFF\x00h\x00i"), Err(DecodeError::Utf16));
        assert!(DecodeError::Utf16.to_string().contains("UTF-16"));
    }

    #[test]
    fn new_lines_typed_into_a_crlf_document_are_written_as_crlf() {
        let decoded = decode(b"a\r\n").unwrap();
        let edited = format!("{}b\nc\n", decoded.text);
        assert_eq!(encode(&edited, &decoded.format), b"a\r\nb\r\nc\r\n");
    }

    #[test]
    fn labels_and_strings_match() {
        for ending in [LineEnding::Lf, LineEnding::CrLf, LineEnding::Cr] {
            assert!(!ending.label().is_empty());
            assert!(!ending.as_str().is_empty());
        }
        assert_eq!(LineEnding::default(), LineEnding::Lf);
    }
}
