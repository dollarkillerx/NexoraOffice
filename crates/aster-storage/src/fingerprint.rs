//! Telling whether a file changed behind the application's back.

use std::fs;
use std::io;
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

/// FNV-1a, 64 bit. Not a cryptographic hash: it detects accidents, not
/// tampering.
pub fn checksum(bytes: &[u8]) -> u64 {
    bytes.iter().fold(0xcbf2_9ce4_8422_2325, |hash, byte| {
        (hash ^ u64::from(*byte)).wrapping_mul(0x0000_0100_0000_01b3)
    })
}

/// What a file held when the application last read or wrote it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Fingerprint {
    pub len: u64,
    pub checksum: u64,
    /// Modification time in nanoseconds since 1970, if the system reports one.
    /// Shown to the user; not used to decide whether the file changed, because
    /// copying or restoring a file can change it without changing the content.
    pub modified_nanos: Option<u128>,
}

fn modified_nanos(path: &Path) -> Option<u128> {
    let modified = fs::metadata(path).ok()?.modified().ok()?;
    Some(modified.duration_since(UNIX_EPOCH).ok()?.as_nanos())
}

impl Fingerprint {
    /// The fingerprint of `bytes`, which were just read from or written to
    /// `path`.
    pub fn of(path: &Path, bytes: &[u8]) -> Self {
        Self {
            len: bytes.len() as u64,
            checksum: checksum(bytes),
            modified_nanos: modified_nanos(path),
        }
    }

    /// Read `path` and take its fingerprint.
    pub fn read(path: &Path) -> io::Result<Self> {
        let bytes = fs::read(path)?;
        Ok(Self::of(path, &bytes))
    }

    /// Whether `other` describes the same content.
    pub fn same_content(&self, other: &Fingerprint) -> bool {
        self.len == other.len && self.checksum == other.checksum
    }

    pub fn modified(&self) -> Option<SystemTime> {
        let nanos = u64::try_from(self.modified_nanos?).ok()?;
        UNIX_EPOCH.checked_add(std::time::Duration::from_nanos(nanos))
    }

    /// A short text form, for storing next to a recovery journal.
    pub fn encode(&self) -> String {
        let modified = self
            .modified_nanos
            .map_or_else(String::new, |nanos| nanos.to_string());
        format!("{} {:016x} {modified}", self.len, self.checksum)
    }

    pub fn decode(text: &str) -> Option<Self> {
        let mut parts = text.split(' ');
        let len = parts.next()?.parse().ok()?;
        let checksum = u64::from_str_radix(parts.next()?, 16).ok()?;
        let modified_nanos = match parts.next()? {
            "" => None,
            nanos => Some(nanos.parse().ok()?),
        };
        parts.next().is_none().then_some(Self {
            len,
            checksum,
            modified_nanos,
        })
    }
}

/// How a file compares with the fingerprint taken earlier.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FileState {
    Unchanged,
    /// The content differs.
    Changed,
    /// There is no file at that path any more.
    Missing,
    /// It could not be read; the reason is attached.
    Unreadable(String),
}

/// Compare the file at `path` with what it held earlier.
pub fn compare(path: &Path, earlier: &Fingerprint) -> FileState {
    match Fingerprint::read(path) {
        Ok(now) if now.same_content(earlier) => FileState::Unchanged,
        Ok(_) => FileState::Changed,
        Err(err) if err.kind() == io::ErrorKind::NotFound => FileState::Missing,
        Err(err) => FileState::Unreadable(err.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn checksum_is_fnv1a() {
        // Published FNV-1a 64 test vectors.
        assert_eq!(checksum(b""), 0xcbf2_9ce4_8422_2325);
        assert_eq!(checksum(b"a"), 0xaf63_dc4c_8601_ec8c);
        assert_eq!(checksum(b"foobar"), 0x8594_4171_f739_67e8);
    }

    #[test]
    fn fingerprints_round_trip_through_text() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a.md");
        fs::write(&path, "中文").unwrap();
        let print = Fingerprint::read(&path).unwrap();
        assert_eq!(print.len, 6);
        assert!(print.modified().is_some());
        assert_eq!(Fingerprint::decode(&print.encode()), Some(print));

        let no_time = Fingerprint {
            modified_nanos: None,
            ..print
        };
        assert_eq!(Fingerprint::decode(&no_time.encode()), Some(no_time));
        for bad in ["", "1", "1 zz 3", "1 00 3 4", "x 00 "] {
            assert_eq!(Fingerprint::decode(bad), None, "{bad:?}");
        }
    }

    #[test]
    fn changes_are_judged_by_content() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a.md");
        fs::write(&path, "one").unwrap();
        let earlier = Fingerprint::read(&path).unwrap();
        assert_eq!(compare(&path, &earlier), FileState::Unchanged);

        // Rewriting the same bytes is not a change.
        fs::write(&path, "one").unwrap();
        assert_eq!(compare(&path, &earlier), FileState::Unchanged);

        fs::write(&path, "two").unwrap();
        assert_eq!(compare(&path, &earlier), FileState::Changed);

        fs::remove_file(&path).unwrap();
        assert_eq!(compare(&path, &earlier), FileState::Missing);

        // A folder where the file was cannot be read as one.
        fs::create_dir(&path).unwrap();
        assert!(matches!(compare(&path, &earlier), FileState::Unreadable(_)));
    }
}
