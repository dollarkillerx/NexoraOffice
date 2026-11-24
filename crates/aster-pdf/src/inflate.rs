//! Decoding PDF stream filters with a ceiling on the output, and reading the
//! size of compressed pictures from their headers.
//!
//! The parser decodes streams without any limit, so a few kilobytes of
//! deflate can ask for gigabytes. Everything here stops as soon as the output
//! would pass `limit` and says so, without holding more than `limit` bytes.

use flate2::{Decompress, FlushDecompress, Status};

/// What became of a decoding attempt.
#[derive(Debug, PartialEq, Eq)]
pub enum Decoded {
    /// The whole output, at most the limit.
    Data(Vec<u8>),
    /// The output would be larger than the limit.
    TooLarge,
    /// The input is not valid for the filter (or is still encrypted). What
    /// came out before the error was within the limit.
    Invalid,
}

/// Inflate zlib data, or raw deflate when there is no zlib header, as the
/// parser does.
pub fn flate(input: &[u8], limit: usize) -> Decoded {
    match inflate(input, limit, true) {
        Decoded::Invalid => inflate(input, limit, false),
        other => other,
    }
}

fn inflate(input: &[u8], limit: usize, zlib: bool) -> Decoded {
    let mut decompress = Decompress::new(zlib);
    let mut out: Vec<u8> =
        Vec::with_capacity(input.len().saturating_mul(4).min(limit).min(1 << 20));
    loop {
        if out.len() == out.capacity() {
            if out.len() >= limit {
                // Full to the limit: find out whether anything is left.
                let mut probe = [0u8; 1];
                let consumed = decompress.total_in() as usize;
                return match decompress.decompress(
                    &input[consumed.min(input.len())..],
                    &mut probe,
                    FlushDecompress::None,
                ) {
                    Ok(_) if decompress.total_out() as usize > out.len() => Decoded::TooLarge,
                    _ => Decoded::Data(out),
                };
            }
            let more = out.capacity().max(4096).min(limit - out.len());
            out.reserve_exact(more);
        }
        let consumed = decompress.total_in() as usize;
        let before = decompress.total_out();
        let status = decompress.decompress_vec(
            &input[consumed.min(input.len())..],
            &mut out,
            FlushDecompress::None,
        );
        match status {
            Ok(Status::StreamEnd) => return Decoded::Data(out),
            Ok(_) => {
                let progressed =
                    decompress.total_out() != before || decompress.total_in() as usize != consumed;
                if !progressed && out.len() < out.capacity() {
                    // Input ran out before the end of the stream: keep what
                    // there is, as the parser would.
                    return Decoded::Data(out);
                }
            }
            Err(_) => {
                return if out.is_empty() {
                    Decoded::Invalid
                } else {
                    Decoded::Data(out)
                };
            }
        }
    }
}

/// LZW as PDF uses it (8-bit, MSB first codes of 9 to 12 bits).
pub fn lzw(input: &[u8], early_change: bool, limit: usize) -> Decoded {
    const CLEAR: usize = 256;
    const END: usize = 257;
    // Each entry: (previous code, last byte, length).
    let mut table: Vec<(u16, u8, u32)> = Vec::with_capacity(4096);
    let reset = |table: &mut Vec<(u16, u8, u32)>| {
        table.clear();
        for byte in 0..=255u8 {
            table.push((u16::MAX, byte, 1));
        }
        table.push((u16::MAX, 0, 0));
        table.push((u16::MAX, 0, 0));
    };
    reset(&mut table);
    let mut out = Vec::new();
    let mut width = 9u32;
    let mut bits = 0u32;
    let mut held = 0u32;
    let mut previous: Option<usize> = None;
    let mut scratch = Vec::new();
    let mut bytes = input.iter();
    loop {
        while held < width {
            match bytes.next() {
                Some(&byte) => {
                    bits = (bits << 8) | u32::from(byte);
                    held += 8;
                }
                None => return Decoded::Data(out),
            }
        }
        held -= width;
        let code = ((bits >> held) & ((1 << width) - 1)) as usize;
        bits &= (1 << held) - 1;
        if code == CLEAR {
            reset(&mut table);
            width = 9;
            previous = None;
            continue;
        }
        if code == END {
            return Decoded::Data(out);
        }
        let first = |table: &Vec<(u16, u8, u32)>, mut code: usize| loop {
            let (prev, byte, _) = table[code];
            if prev == u16::MAX {
                return byte;
            }
            code = prev as usize;
        };
        let entry = if code < table.len() {
            if table[code].2 == 0 {
                return Decoded::Invalid;
            }
            if let Some(prev) = previous
                && table.len() < 4096
            {
                let byte = first(&table, code);
                table.push((prev as u16, byte, table[prev].2 + 1));
            }
            code
        } else if code == table.len() {
            let Some(prev) = previous else {
                return Decoded::Invalid;
            };
            let byte = first(&table, prev);
            table.push((prev as u16, byte, table[prev].2 + 1));
            code
        } else {
            return Decoded::Invalid;
        };
        let length = table[entry].2 as usize;
        if out.len() + length > limit {
            return Decoded::TooLarge;
        }
        scratch.clear();
        let mut at = entry;
        loop {
            let (prev, byte, _) = table[at];
            scratch.push(byte);
            if prev == u16::MAX {
                break;
            }
            at = prev as usize;
        }
        out.extend(scratch.iter().rev());
        previous = Some(entry);
        let next = table.len() + usize::from(early_change);
        width = match next {
            ..512 => 9,
            512..1024 => 10,
            1024..2048 => 11,
            _ => 12,
        };
    }
}

/// RunLengthDecode.
pub fn run_length(input: &[u8], limit: usize) -> Decoded {
    let mut out = Vec::new();
    let mut at = 0;
    while at < input.len() {
        let length = input[at];
        at += 1;
        match length {
            128 => break,
            0..128 => {
                let count = usize::from(length) + 1;
                let end = (at + count).min(input.len());
                if out.len() + (end - at) > limit {
                    return Decoded::TooLarge;
                }
                out.extend_from_slice(&input[at..end]);
                at = end;
            }
            _ => {
                let count = 257 - usize::from(length);
                let Some(&byte) = input.get(at) else { break };
                at += 1;
                if out.len() + count > limit {
                    return Decoded::TooLarge;
                }
                out.extend(std::iter::repeat_n(byte, count));
            }
        }
    }
    Decoded::Data(out)
}

/// ASCIIHexDecode. The output is never larger than the input.
pub fn ascii_hex(input: &[u8]) -> Decoded {
    let mut out = Vec::with_capacity(input.len() / 2);
    let mut high: Option<u8> = None;
    for &byte in input {
        let digit = match byte {
            b'0'..=b'9' => byte - b'0',
            b'a'..=b'f' => byte - b'a' + 10,
            b'A'..=b'F' => byte - b'A' + 10,
            b'>' => break,
            _ if byte.is_ascii_whitespace() || byte == 0 => continue,
            _ => return Decoded::Invalid,
        };
        match high.take() {
            Some(h) => out.push(h << 4 | digit),
            None => high = Some(digit),
        }
    }
    if let Some(h) = high {
        out.push(h << 4);
    }
    Decoded::Data(out)
}

/// ASCII85Decode. Five characters make four bytes; `z` makes four zeros from
/// one character, so the output is at most four times the input.
pub fn ascii_85(input: &[u8], limit: usize) -> Decoded {
    let mut out = Vec::with_capacity(input.len());
    let mut group = [0u32; 5];
    let mut count = 0;
    let mut data = input;
    if data.starts_with(b"<~") {
        data = &data[2..];
    }
    for &byte in data {
        match byte {
            b'~' => break,
            b'z' if count == 0 => out.extend_from_slice(&[0; 4]),
            b'!'..=b'u' => {
                group[count] = u32::from(byte - b'!');
                count += 1;
                if count == 5 {
                    let value = group.iter().fold(0u64, |v, &d| v * 85 + u64::from(d));
                    let Ok(value) = u32::try_from(value) else {
                        return Decoded::Invalid;
                    };
                    out.extend_from_slice(&value.to_be_bytes());
                    count = 0;
                }
            }
            _ if byte.is_ascii_whitespace() || byte == 0 => {}
            _ => return Decoded::Invalid,
        }
        if out.len() > limit {
            return Decoded::TooLarge;
        }
    }
    if count > 1 {
        for slot in group.iter_mut().skip(count) {
            *slot = 84;
        }
        let value = group.iter().fold(0u64, |v, &d| v * 85 + u64::from(d));
        let bytes = (value as u32).to_be_bytes();
        out.extend_from_slice(&bytes[..count - 1]);
    }
    Decoded::Data(out)
}

/// Width and height from a JPEG's frame header.
pub fn jpeg_size(data: &[u8]) -> Option<(u32, u32)> {
    let mut at = 2;
    if !data.starts_with(&[0xFF, 0xD8]) {
        // Some writers put junk before the start marker.
        at = data.windows(2).position(|w| w == [0xFF, 0xD8])? + 2;
    }
    while at + 4 <= data.len() {
        if data[at] != 0xFF {
            at += 1;
            continue;
        }
        let marker = data[at + 1];
        if marker == 0xFF {
            at += 1;
            continue;
        }
        if matches!(marker, 0xD8 | 0x01 | 0xD0..=0xD7) {
            at += 2;
            continue;
        }
        let length = usize::from(u16::from_be_bytes([data[at + 2], data[at + 3]]));
        let start_of_frame = matches!(marker, 0xC0..=0xCF) && !matches!(marker, 0xC4 | 0xC8 | 0xCC);
        if start_of_frame {
            let header = data.get(at + 5..at + 9)?;
            let height = u32::from(u16::from_be_bytes([header[0], header[1]]));
            let width = u32::from(u16::from_be_bytes([header[2], header[3]]));
            return Some((width, height));
        }
        at += 2 + length;
    }
    None
}

/// Width and height from a JPEG 2000 codestream's SIZ segment (inside a JP2
/// box structure or bare).
pub fn jpx_size(data: &[u8]) -> Option<(u32, u32)> {
    let at = data.windows(2).position(|w| w == [0xFF, 0x51])?;
    // After the marker: Lsiz (2), Rsiz (2), Xsiz, Ysiz, XOsiz, YOsiz (4 each).
    let siz = data.get(at + 6..at + 22)?;
    let word = |i: usize| u32::from_be_bytes([siz[i], siz[i + 1], siz[i + 2], siz[i + 3]]);
    Some((
        word(0).saturating_sub(word(8)),
        word(4).saturating_sub(word(12)),
    ))
}

/// Width and height of a JBIG2 page, from its page information segment
/// (embedded streams have no file header).
pub fn jbig2_size(data: &[u8]) -> Option<(u32, u32)> {
    // Segment header: number (4), flags (1), referred-to count and numbers,
    // page association (1 or 4), data length (4). Page information is type 48
    // and starts with width (4) and height (4).
    let mut at = 0;
    while at + 11 <= data.len() {
        let flags = data[at + 4];
        let kind = flags & 0x3F;
        let page_size_4 = flags & 0x40 != 0;
        let mut p = at + 5;
        let referred = data[p];
        let count = usize::from(referred >> 5);
        if count == 7 {
            return None;
        }
        p += 1;
        let number = u32::from_be_bytes(data[at..at + 4].try_into().ok()?);
        let width = if number <= 256 {
            1
        } else if number <= 65536 {
            2
        } else {
            4
        };
        p += count * width;
        p += if page_size_4 { 4 } else { 1 };
        let length = u32::from_be_bytes(data.get(p..p + 4)?.try_into().ok()?) as usize;
        p += 4;
        if kind == 48 {
            let w = u32::from_be_bytes(data.get(p..p + 4)?.try_into().ok()?);
            let h = u32::from_be_bytes(data.get(p + 4..p + 8)?.try_into().ok()?);
            return Some((w, h));
        }
        if length == 0xFFFF_FFFF {
            // "Unknown length", only allowed for the last region: no page
            // information came before it.
            return None;
        }
        at = p.checked_add(length)?;
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use flate2::{Compression, write::ZlibEncoder};
    use std::io::Write;

    fn deflate(data: &[u8]) -> Vec<u8> {
        let mut encoder = ZlibEncoder::new(Vec::new(), Compression::best());
        encoder.write_all(data).unwrap();
        encoder.finish().unwrap()
    }

    #[test]
    fn flate_within_and_beyond_the_limit() {
        let data = vec![7u8; 100_000];
        let packed = deflate(&data);
        assert_eq!(flate(&packed, 100_000), Decoded::Data(data.clone()));
        assert_eq!(flate(&packed, 99_999), Decoded::TooLarge);
        assert_eq!(flate(b"not deflate at all", 1000), Decoded::Invalid);
    }

    #[test]
    fn flate_bomb_stops_at_the_limit() {
        // 64 MB of zeros packs into about 64 KB.
        let mut encoder = ZlibEncoder::new(Vec::new(), Compression::best());
        let chunk = vec![0u8; 1 << 20];
        for _ in 0..64 {
            encoder.write_all(&chunk).unwrap();
        }
        let packed = encoder.finish().unwrap();
        assert!(packed.len() < 200_000);
        assert_eq!(flate(&packed, 1 << 20), Decoded::TooLarge);
    }

    #[test]
    fn truncated_flate_keeps_what_there_is() {
        let data: Vec<u8> = (0..50_000u32).map(|i| (i * 7 % 251) as u8).collect();
        let packed = deflate(&data);
        let Decoded::Data(out) = flate(&packed[..packed.len() / 2], 1 << 20) else {
            panic!("expected data");
        };
        assert!(!out.is_empty() && data.starts_with(&out));
    }

    #[test]
    fn lzw_decodes_the_reference_example() {
        // The example from the PDF reference, section 7.4.4.2.
        let input = [0x80, 0x0B, 0x60, 0x50, 0x22, 0x0C, 0x0C, 0x85, 0x01];
        assert_eq!(
            lzw(&input, true, 100),
            Decoded::Data(vec![45, 45, 45, 45, 45, 65, 45, 45, 45, 66])
        );
        assert_eq!(lzw(&input, true, 5), Decoded::TooLarge);
    }

    #[test]
    fn run_length_and_ascii_filters() {
        assert_eq!(
            run_length(&[2, b'a', b'b', b'c', 254, b'x', 128], 100),
            Decoded::Data(b"abcxxx".to_vec())
        );
        assert_eq!(run_length(&[129; 64], 1000), Decoded::TooLarge);
        assert_eq!(
            ascii_hex(b"48 65 6c6C 6f>"),
            Decoded::Data(b"Hello".to_vec())
        );
        assert_eq!(
            ascii_85(b"<~87cURD]i,\"Ebo80~>", 100),
            Decoded::Data(b"Hello World!".to_vec())
        );
        assert_eq!(ascii_85(b"zzzz", 8), Decoded::TooLarge);
    }

    #[test]
    fn picture_headers() {
        let mut jpeg = vec![0xFF, 0xD8, 0xFF, 0xE0, 0, 4, 0, 0, 0xFF, 0xC0, 0, 11, 8];
        jpeg.extend_from_slice(&[0x01, 0x00, 0x02, 0x00]); // 256 high, 512 wide
        assert_eq!(jpeg_size(&jpeg), Some((512, 256)));
        let mut j2k = vec![0xFF, 0x4F, 0xFF, 0x51, 0, 41, 0, 0];
        j2k.extend_from_slice(&40000u32.to_be_bytes());
        j2k.extend_from_slice(&30000u32.to_be_bytes());
        j2k.extend_from_slice(&[0; 8]);
        assert_eq!(jpx_size(&j2k), Some((40000, 30000)));
        // A page information segment: number 1, type 48, no references,
        // page 1, length 19, then 60000 x 50000.
        let mut jb2 = vec![0, 0, 0, 1, 48, 0, 1, 0, 0, 0, 19];
        jb2.extend_from_slice(&60000u32.to_be_bytes());
        jb2.extend_from_slice(&50000u32.to_be_bytes());
        jb2.extend_from_slice(&[0; 11]);
        assert_eq!(jbig2_size(&jb2), Some((60000, 50000)));
    }
}
