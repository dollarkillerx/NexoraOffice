//! Bold faces the system does not say are bold.
//!
//! Some fonts ship a real bold face in a family but describe it as an
//! ordinary one: in Kaiti SC's collection the face called "粗体" has a weight
//! class of 400 like the regular, so a request for bold matches the regular
//! and the text is thickened by hand. [`Shaper::prefer_real_bold`] finds
//! such a face by its name, copies it out of the collection as a font file
//! of its own and registers that as the family's bold.
//!
//! [`Shaper::prefer_real_bold`]: crate::Shaper::prefer_real_bold

fn be16(data: &[u8], at: usize) -> Option<usize> {
    data.get(at..at + 2)
        .map(|b| usize::from(u16::from_be_bytes([b[0], b[1]])))
}

fn be32(data: &[u8], at: usize) -> Option<usize> {
    data.get(at..at + 4)
        .map(|b| u32::from_be_bytes([b[0], b[1], b[2], b[3]]) as usize)
}

/// Where the table directory of face `index` starts.
fn directory(data: &[u8], index: u32) -> Option<usize> {
    if data.get(0..4) == Some(b"ttcf") {
        be32(data, 12 + 4 * index as usize)
    } else {
        Some(0)
    }
}

/// `(tag, offset, length)` of every table of a face.
fn tables(data: &[u8], base: usize) -> Option<Vec<([u8; 4], usize, usize)>> {
    let count = be16(data, base + 4)?;
    let mut out = Vec::with_capacity(count);
    for i in 0..count {
        let record = base + 12 + 16 * i;
        let tag: [u8; 4] = data.get(record..record + 4)?.try_into().ok()?;
        let offset = be32(data, record + 8)?;
        let length = be32(data, record + 12)?;
        data.get(offset..offset.checked_add(length)?)?;
        out.push((tag, offset, length));
    }
    Some(out)
}

/// The full name and subfamily names of a face (name ids 2, 4 and 17).
fn names(data: &[u8], index: u32) -> Vec<String> {
    let mut out = Vec::new();
    let Some(base) = directory(data, index) else {
        return out;
    };
    let Some(all) = tables(data, base) else {
        return out;
    };
    let Some(&(_, start, length)) = all.iter().find(|t| &t.0 == b"name") else {
        return out;
    };
    let table = &data[start..start + length];
    let (Some(count), Some(strings)) = (be16(table, 2), be16(table, 4)) else {
        return out;
    };
    for i in 0..count {
        let record = 6 + 12 * i;
        let (Some(platform), Some(id), Some(len), Some(offset)) = (
            be16(table, record),
            be16(table, record + 6),
            be16(table, record + 8),
            be16(table, record + 10),
        ) else {
            break;
        };
        if !matches!(id, 2 | 4 | 17) {
            continue;
        }
        let Some(raw) = table.get(strings + offset..strings + offset + len) else {
            continue;
        };
        let text = match platform {
            0 | 3 => {
                let (pairs, _) = raw.as_chunks::<2>();
                let units: Vec<u16> = pairs.iter().map(|b| u16::from_be_bytes(*b)).collect();
                String::from_utf16_lossy(&units)
            }
            _ => raw.iter().map(|&b| b as char).collect(),
        };
        out.push(text);
    }
    out
}

/// Whether a face is, by its name, a plain bold ("Bold", "粗体", "粗體"; not
/// semibold or the heavier weights).
pub(crate) fn is_named_bold(data: &[u8], index: u32) -> bool {
    names(data, index).iter().any(|name| {
        let lower = name.to_lowercase();
        let latin = lower
            .split(|c: char| !c.is_alphanumeric())
            .any(|word| word == "bold");
        latin || name.contains("粗体") || name.contains("粗體")
    })
}

/// A font file holding only face `index` of `data` (which may be a
/// collection).
pub(crate) fn single_face(data: &[u8], index: u32) -> Option<Vec<u8>> {
    let base = directory(data, index)?;
    let all = tables(data, base)?;
    let version = data.get(base..base + 4)?;
    let count = all.len();
    let mut out = Vec::new();
    out.extend_from_slice(version);
    out.extend_from_slice(&(count as u16).to_be_bytes());
    // The search fields of the directory; readers do not depend on them.
    let mut power = 1usize;
    let mut log = 0u16;
    while power * 2 <= count {
        power *= 2;
        log += 1;
    }
    out.extend_from_slice(&((power * 16) as u16).to_be_bytes());
    out.extend_from_slice(&log.to_be_bytes());
    out.extend_from_slice(&((count * 16 - power * 16) as u16).to_be_bytes());
    let offset = 12 + 16 * count;
    let mut body: Vec<u8> = Vec::new();
    for (tag, from, length) in &all {
        out.extend_from_slice(tag);
        // The checksum is not looked at.
        out.extend_from_slice(&0u32.to_be_bytes());
        out.extend_from_slice(&((offset + body.len()) as u32).to_be_bytes());
        out.extend_from_slice(&(*length as u32).to_be_bytes());
        body.extend_from_slice(&data[*from..*from + *length]);
        while !body.len().is_multiple_of(4) {
            body.push(0);
        }
    }
    out.extend_from_slice(&body);
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A collection of two faces made up of a name table and one more.
    fn collection() -> Vec<u8> {
        let name = |text: &str| {
            let units: Vec<u8> = text.encode_utf16().flat_map(u16::to_be_bytes).collect();
            let mut t = vec![0, 0, 0, 1, 0, 18];
            for v in [3u16, 1, 0x409, 4, units.len() as u16, 0] {
                t.extend(v.to_be_bytes());
            }
            t.extend(units);
            t
        };
        let tables = [name("Test Regular"), name("Test 粗体")];
        // Header, two directories of one table each, then the tables.
        let mut out = Vec::new();
        out.extend(b"ttcf");
        out.extend([0, 1, 0, 0, 0, 0, 0, 2]);
        let first = 20u32;
        let second = first + 12 + 16;
        out.extend(first.to_be_bytes());
        out.extend(second.to_be_bytes());
        let data_start = second + 12 + 16;
        let mut at = data_start;
        let mut bodies = Vec::new();
        for (i, table) in tables.iter().enumerate() {
            let _ = i;
            out.extend([0, 1, 0, 0, 0, 1, 0, 0, 0, 0, 0, 0]);
            out.extend(b"name");
            out.extend(0u32.to_be_bytes());
            out.extend(at.to_be_bytes());
            out.extend((table.len() as u32).to_be_bytes());
            bodies.push(table.clone());
            at += table.len() as u32;
        }
        for body in bodies {
            out.extend(body);
        }
        out
    }

    #[test]
    fn a_bold_face_is_known_by_its_name() {
        let data = collection();
        assert!(!is_named_bold(&data, 0));
        assert!(is_named_bold(&data, 1));
        assert_eq!(names(&data, 1), vec!["Test 粗体".to_owned()]);
    }

    #[test]
    fn one_face_can_be_copied_out_of_a_collection() {
        let data = collection();
        let single = single_face(&data, 1).unwrap();
        assert_eq!(&single[0..4], &[0, 1, 0, 0]);
        assert!(is_named_bold(&single, 0));
        assert_eq!(names(&single, 0).len(), 1);
        // Not a face, not a panic.
        assert!(single_face(&data, 5).is_none());
        assert!(single_face(&[1, 2, 3], 0).is_none());
        assert!(names(&[1, 2, 3], 0).is_empty());
    }
}
