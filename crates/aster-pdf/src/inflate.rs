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
    /// The input is not valid for the filter.
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
            // Damaged data: the parser's own decoder may read past where
            // ours stops, so nothing can be said about its size.
            Err(_) => return Decoded::Invalid,
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

/// Width and height from a JPEG's frame headers: the largest of every
/// frame header before the first scan. `None` when there is none.
pub fn jpeg_size(data: &[u8]) -> Option<(u32, u32)> {
    let mut at = 2;
    if !data.starts_with(&[0xFF, 0xD8]) {
        // Some writers put junk before the start marker.
        at = data.windows(2).position(|w| w == [0xFF, 0xD8])? + 2;
    }
    let mut largest: Option<(u32, u32)> = None;
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
        if matches!(marker, 0xD8 | 0x01 | 0xD0..=0xD7 | 0x00) {
            at += 2;
            continue;
        }
        if matches!(marker, 0xDA | 0xD9) {
            break;
        }
        let length = usize::from(u16::from_be_bytes([data[at + 2], data[at + 3]]));
        let frame = matches!(marker, 0xC0..=0xCF) && !matches!(marker, 0xC4 | 0xC8 | 0xCC);
        if frame {
            let header = data.get(at + 5..at + 9)?;
            let height = u32::from(u16::from_be_bytes([header[0], header[1]]));
            let width = u32::from(u16::from_be_bytes([header[2], header[3]]));
            let pixels = |s: (u32, u32)| u64::from(s.0) * u64::from(s.1);
            if largest.is_none_or(|l| pixels((width, height)) > pixels(l)) {
                largest = Some((width, height));
            }
        }
        at += 2 + length.max(2);
    }
    largest
}

/// Width and height from a JPEG 2000 codestream's SIZ segment, which must
/// follow its start marker. A JP2 file is walked box by box to its `jp2c`
/// box; the image header (`ihdr`) counts too, the larger of the two wins.
pub fn jpx_size(data: &[u8]) -> Option<(u32, u32)> {
    if data.starts_with(&[0xFF, 0x4F, 0xFF, 0x51]) {
        return siz(data);
    }
    let mut header: Option<(u32, u32)> = None;
    let mut codestream: Option<(u32, u32)> = None;
    walk_boxes(data, 0, &mut header, &mut codestream)?;
    let codestream = codestream?;
    let pixels = |s: (u32, u32)| u64::from(s.0) * u64::from(s.1);
    Some(match header {
        Some(h) if pixels(h) > pixels(codestream) => h,
        _ => codestream,
    })
}

fn walk_boxes(
    data: &[u8],
    depth: usize,
    header: &mut Option<(u32, u32)>,
    codestream: &mut Option<(u32, u32)>,
) -> Option<()> {
    if depth > 4 {
        return None;
    }
    let mut at = 0usize;
    while at + 8 <= data.len() {
        let length = u64::from(u32::from_be_bytes(data[at..at + 4].try_into().ok()?));
        let kind = &data[at + 4..at + 8];
        let (body, end) = match length {
            0 => (at + 8, data.len()),
            1 => {
                let long = u64::from_be_bytes(data.get(at + 8..at + 16)?.try_into().ok()?);
                let end = usize::try_from(long).ok().and_then(|l| at.checked_add(l))?;
                (at + 16, end)
            }
            2..8 => return None,
            n => (at + 8, at.checked_add(usize::try_from(n).ok()?)?),
        };
        let end = end.min(data.len());
        let content = data.get(body..end)?;
        match kind {
            b"jp2h" => walk_boxes(content, depth + 1, header, codestream)?,
            b"ihdr" => {
                let h = u32::from_be_bytes(content.get(0..4)?.try_into().ok()?);
                let w = u32::from_be_bytes(content.get(4..8)?.try_into().ok()?);
                *header = Some((w, h));
            }
            b"jp2c" if codestream.is_none() => {
                if !content.starts_with(&[0xFF, 0x4F, 0xFF, 0x51]) {
                    return None;
                }
                *codestream = Some(siz(content)?);
            }
            _ => {}
        }
        if end <= at {
            return None;
        }
        at = end;
    }
    Some(())
}

/// The image size in the SIZ segment that follows the SOC marker at the
/// start of `data`.
fn siz(data: &[u8]) -> Option<(u32, u32)> {
    // SOC (2), SIZ marker (2), Lsiz (2), Rsiz (2), then Xsiz, Ysiz, XOsiz,
    // YOsiz (4 each).
    let siz = data.get(8..24)?;
    let word = |i: usize| u32::from_be_bytes([siz[i], siz[i + 1], siz[i + 2], siz[i + 3]]);
    Some((
        word(0).saturating_sub(word(8)),
        word(4).saturating_sub(word(12)),
    ))
}

/// Undo a PNG (10–15, a filter byte per row) or TIFF (2) predictor. The
/// output is never larger than the input. `None` for parameters we do not
/// undo.
pub fn unpredict(
    data: Vec<u8>,
    predictor: u8,
    colors: u8,
    bits: u8,
    columns: usize,
    limit: usize,
) -> Option<Vec<u8>> {
    if !matches!(bits, 1 | 2 | 4 | 8 | 16) || colors == 0 || columns == 0 {
        return None;
    }
    let bits_per_pixel = usize::from(colors) * usize::from(bits);
    let row = columns.checked_mul(bits_per_pixel)?.div_ceil(8);
    if row == 0 || row > limit {
        return None;
    }
    let pixel = bits_per_pixel.div_ceil(8).max(1);
    if predictor >= 10 {
        let rows = data.len() / (row + 1);
        let mut out = vec![0u8; rows * row];
        for r in 0..rows {
            let source = &data[r * (row + 1)..(r + 1) * (row + 1)];
            let kind = source[0];
            let (done, rest) = out.split_at_mut(r * row);
            let previous = (r > 0).then(|| &done[done.len() - row..]);
            let line = &mut rest[..row];
            for i in 0..row {
                let raw = source[1 + i];
                let left = if i >= pixel { line[i - pixel] } else { 0 };
                let up = previous.map_or(0, |p| p[i]);
                let up_left = match previous {
                    Some(p) if i >= pixel => p[i - pixel],
                    _ => 0,
                };
                line[i] = match kind {
                    0 => raw,
                    1 => raw.wrapping_add(left),
                    2 => raw.wrapping_add(up),
                    3 => raw.wrapping_add(((u16::from(left) + u16::from(up)) / 2) as u8),
                    4 => raw.wrapping_add(paeth(left, up, up_left)),
                    _ => return None,
                };
            }
        }
        return Some(out);
    }
    if predictor == 2 {
        // TIFF: each sample is the difference from the one to its left.
        // Only whole-byte samples are undone.
        if bits != 8 {
            return None;
        }
        let mut out = data;
        let rows = out.len() / row;
        out.truncate(rows * row);
        let step = usize::from(colors);
        for line in out.chunks_exact_mut(row) {
            for i in step..row {
                line[i] = line[i].wrapping_add(line[i - step]);
            }
        }
        return Some(out);
    }
    None
}

fn paeth(a: u8, b: u8, c: u8) -> u8 {
    let p = i16::from(a) + i16::from(b) - i16::from(c);
    let pa = (p - i16::from(a)).abs();
    let pb = (p - i16::from(b)).abs();
    let pc = (p - i16::from(c)).abs();
    if pa <= pb && pa <= pc {
        a
    } else if pb <= pc {
        b
    } else {
        c
    }
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
    fn predictors_are_undone() {
        // PNG "Up" over two rows of three bytes.
        let data = vec![2, 1, 2, 3, 2, 1, 1, 1];
        assert_eq!(
            unpredict(data, 12, 1, 8, 3, 1000),
            Some(vec![1, 2, 3, 2, 3, 4])
        );
        // TIFF over one row of four grey samples.
        assert_eq!(
            unpredict(vec![5, 1, 1, 1], 2, 1, 8, 4, 1000),
            Some(vec![5, 6, 7, 8])
        );
        assert_eq!(unpredict(vec![0; 8], 2, 1, 4, 4, 1000), None);
        assert_eq!(unpredict(vec![0; 8], 12, 1, 3, 4, 1000), None);
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
        // A decoy SIZ before the real codestream counts for nothing: the
        // codestream in the jp2c box is what is read.
        let mut decoy = vec![0xFF, 0x51, 0, 41, 0, 0];
        decoy.extend_from_slice(&10u32.to_be_bytes());
        decoy.extend_from_slice(&10u32.to_be_bytes());
        decoy.extend_from_slice(&[0; 8]);
        let mut jp2 = Vec::new();
        jp2.extend_from_slice(&((decoy.len() + 8) as u32).to_be_bytes());
        jp2.extend_from_slice(b"junk");
        jp2.extend_from_slice(&decoy);
        jp2.extend_from_slice(&((j2k.len() + 8) as u32).to_be_bytes());
        jp2.extend_from_slice(b"jp2c");
        jp2.extend_from_slice(&j2k);
        assert_eq!(jpx_size(&jp2), Some((40000, 30000)));
        // Data that is neither a codestream nor boxes is not read.
        assert_eq!(jpx_size(&decoy), None);
        // Two frame headers in a JPEG: the larger counts.
        let mut two = vec![
            0xFF, 0xD8, 0xFF, 0xC0, 0, 11, 8, 0, 10, 0, 10, 1, 1, 0x11, 0,
        ];
        two.extend_from_slice(&[0xFF, 0xC2, 0, 11, 8, 0xFF, 0xFF, 0xFF, 0xFF, 1, 1, 0x11, 0]);
        two.extend_from_slice(&[0xFF, 0xDA, 0, 2]);
        assert_eq!(jpeg_size(&two), Some((65535, 65535)));
        // A page information segment: number 1, type 48, no references,
        // page 1, length 19, then 60000 x 50000.
        let mut jb2 = vec![0, 0, 0, 1, 48, 0, 1, 0, 0, 0, 19];
        jb2.extend_from_slice(&60000u32.to_be_bytes());
        jb2.extend_from_slice(&50000u32.to_be_bytes());
        jb2.extend_from_slice(&[0; 11]);
        assert_eq!(jbig2_size(&jb2), Some((60000, 50000)));
    }
}
