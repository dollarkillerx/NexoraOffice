//! A stand-in document: one string, edited by two commands.
//!
//! What matters for this gate is not the model but the shape around it, which is
//! the shape the architecture gives every editor: commands are plain data, they
//! serialise to bytes, and replaying the same commands on the same starting state
//! always gives the same document.

/// SplitMix64. Small, fast, and identical on every platform.
pub struct Rng(u64);

impl Rng {
    pub fn new(seed: u64) -> Self {
        Self(seed)
    }

    pub fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9e37_79b9_7f4a_7c15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        z ^ (z >> 31)
    }

    /// A value in `0..bound`. `bound` must not be zero.
    pub fn below(&mut self, bound: u64) -> u64 {
        self.next() % bound
    }
}

/// FNV-1a, 64 bit.
pub fn checksum(bytes: &[u8]) -> u64 {
    bytes.iter().fold(0xcbf2_9ce4_8422_2325, |hash, byte| {
        (hash ^ u64::from(*byte)).wrapping_mul(0x0000_0100_0000_01b3)
    })
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Command {
    Insert { at: usize, text: String },
    Delete { at: usize, len: usize },
}

/// Version byte at the start of every encoded command.
const FORMAT: u8 = 1;
const TAG_INSERT: u8 = 1;
const TAG_DELETE: u8 = 2;

impl Command {
    /// Apply to `doc`. Fails, leaving `doc` untouched, if the command does not
    /// fit the document: that means the journal and the document disagree.
    pub fn apply(&self, doc: &mut String) -> Result<(), String> {
        match self {
            Command::Insert { at, text } => {
                if !doc.is_char_boundary(*at) {
                    return Err(format!("insert at {at} is not on a character boundary"));
                }
                doc.insert_str(*at, text);
            }
            Command::Delete { at, len } => {
                let end = at.checked_add(*len).filter(|end| *end <= doc.len());
                match end {
                    Some(end) if doc.is_char_boundary(*at) && doc.is_char_boundary(end) => {
                        doc.replace_range(*at..end, "");
                    }
                    _ => {
                        return Err(format!(
                            "delete {at}+{len} does not fit a {} byte document",
                            doc.len()
                        ));
                    }
                }
            }
        }
        Ok(())
    }

    pub fn encode(&self) -> Vec<u8> {
        let (tag, at, len, bytes): (u8, usize, usize, &[u8]) = match self {
            Command::Insert { at, text } => (TAG_INSERT, *at, text.len(), text.as_bytes()),
            Command::Delete { at, len } => (TAG_DELETE, *at, *len, &[]),
        };
        let mut out = Vec::with_capacity(18 + bytes.len());
        out.push(FORMAT);
        out.push(tag);
        out.extend_from_slice(&(at as u64).to_le_bytes());
        out.extend_from_slice(&(len as u64).to_le_bytes());
        out.extend_from_slice(bytes);
        out
    }

    pub fn decode(bytes: &[u8]) -> Result<Self, String> {
        let [format, tag, rest @ ..] = bytes else {
            return Err("command is shorter than its header".to_owned());
        };
        if *format != FORMAT {
            return Err(format!("unknown command format {format}"));
        }
        let (Some(at), Some(len)) = (rest.get(..8), rest.get(8..16)) else {
            return Err("command is shorter than its header".to_owned());
        };
        let at = u64::from_le_bytes(at.try_into().unwrap()) as usize;
        let len = u64::from_le_bytes(len.try_into().unwrap()) as usize;
        let payload = &rest[16..];
        match *tag {
            TAG_INSERT if payload.len() == len => String::from_utf8(payload.to_vec())
                .map(|text| Command::Insert { at, text })
                .map_err(|_| "inserted text is not UTF-8".to_owned()),
            TAG_DELETE if payload.is_empty() => Ok(Command::Delete { at, len }),
            TAG_INSERT | TAG_DELETE => Err("command length does not match its payload".to_owned()),
            other => Err(format!("unknown command tag {other}")),
        }
    }
}

const FRAGMENTS: [&str; 10] = [
    "字",
    "排版",
    "Aster ",
    "。",
    "\n",
    "日本語",
    "x",
    "テスト",
    " 1.95 ",
    "，",
];
/// The generator keeps the document around this size, in bytes.
const TARGET_LEN: usize = 3000;

fn boundary_at_or_before(doc: &str, mut at: usize) -> usize {
    while !doc.is_char_boundary(at) {
        at -= 1;
    }
    at
}

/// The command that revision `rev` consists of, given the document just before
/// it. A pure function of its arguments: the crash test relies on being able to
/// recompute any revision from scratch.
pub fn generate(seed: u64, rev: u64, doc: &str) -> Command {
    let mut rng = Rng::new(seed ^ rev.wrapping_mul(0xa24b_aed4_963e_e407));
    let at = boundary_at_or_before(doc, rng.below(doc.len() as u64 + 1) as usize);
    let delete = !doc.is_empty() && (doc.len() > TARGET_LEN || rng.below(4) == 0);
    if delete {
        let wanted = 1 + rng.below(24) as usize;
        let end = boundary_at_or_before(doc, (at + wanted).min(doc.len()));
        if end > at {
            return Command::Delete { at, len: end - at };
        }
    }
    let pieces = 1 + rng.below(3);
    let text = (0..pieces)
        .map(|_| FRAGMENTS[rng.below(FRAGMENTS.len() as u64) as usize])
        .collect();
    Command::Insert { at, text }
}

/// A document together with the revision it is at, as stored in checkpoints and
/// in the saved file. The trailing checksum covers everything before it, so a
/// torn or altered file is detected instead of being loaded.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Snapshot {
    pub rev: u64,
    pub text: String,
}

const MAGIC: &[u8; 6] = b"ASTR1\n";

impl Snapshot {
    pub fn encode(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(MAGIC.len() + 24 + self.text.len());
        out.extend_from_slice(MAGIC);
        out.extend_from_slice(&self.rev.to_le_bytes());
        out.extend_from_slice(&(self.text.len() as u64).to_le_bytes());
        out.extend_from_slice(self.text.as_bytes());
        let sum = checksum(&out);
        out.extend_from_slice(&sum.to_le_bytes());
        out
    }

    pub fn decode(bytes: &[u8]) -> Result<Self, String> {
        let header = MAGIC.len() + 16;
        if bytes.len() < header + 8 || &bytes[..MAGIC.len()] != MAGIC {
            return Err("not a snapshot file".to_owned());
        }
        let (body, sum) = bytes.split_at(bytes.len() - 8);
        if checksum(body) != u64::from_le_bytes(sum.try_into().unwrap()) {
            return Err("snapshot checksum does not match".to_owned());
        }
        let rev = u64::from_le_bytes(body[MAGIC.len()..MAGIC.len() + 8].try_into().unwrap());
        let len = u64::from_le_bytes(body[MAGIC.len() + 8..header].try_into().unwrap()) as usize;
        if body.len() - header != len {
            return Err("snapshot length does not match its content".to_owned());
        }
        let text =
            String::from_utf8(body[header..].to_vec()).map_err(|_| "snapshot text is not UTF-8")?;
        Ok(Self { rev, text })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn commands_round_trip_through_bytes() {
        for command in [
            Command::Insert {
                at: 0,
                text: "排版 Aster\n".to_owned(),
            },
            Command::Insert {
                at: 7,
                text: String::new(),
            },
            Command::Delete { at: 3, len: 9 },
        ] {
            assert_eq!(Command::decode(&command.encode()), Ok(command));
        }
    }

    #[test]
    fn damaged_commands_are_rejected() {
        let good = Command::Insert {
            at: 0,
            text: "字".to_owned(),
        }
        .encode();
        assert!(Command::decode(&good[..good.len() - 1]).is_err());
        assert!(Command::decode(&good[..5]).is_err());
        assert!(Command::decode(&[]).is_err());
        let mut wrong_format = good.clone();
        wrong_format[0] = 9;
        assert!(Command::decode(&wrong_format).is_err());
        let mut wrong_tag = good;
        wrong_tag[1] = 7;
        assert!(Command::decode(&wrong_tag).is_err());
    }

    #[test]
    fn commands_that_do_not_fit_leave_the_document_alone() {
        let mut doc = "排版".to_owned();
        assert!(
            Command::Insert {
                at: 1,
                text: "x".to_owned()
            }
            .apply(&mut doc)
            .is_err()
        );
        assert!(Command::Delete { at: 0, len: 2 }.apply(&mut doc).is_err());
        assert!(Command::Delete { at: 3, len: 30 }.apply(&mut doc).is_err());
        assert!(
            Command::Delete {
                at: 0,
                len: usize::MAX
            }
            .apply(&mut doc)
            .is_err()
        );
        assert_eq!(doc, "排版");
        assert!(Command::Delete { at: 0, len: 3 }.apply(&mut doc).is_ok());
        assert_eq!(doc, "版");
    }

    #[test]
    fn generation_is_a_pure_function_and_always_applies() {
        let run = || {
            let mut doc = String::new();
            for rev in 1..=5000 {
                let command = generate(42, rev, &doc);
                command.apply(&mut doc).expect("generated commands fit");
            }
            doc
        };
        let (a, b) = (run(), run());
        assert_eq!(a, b);
        assert!(
            a.len() > 1000 && a.len() < TARGET_LEN + 200,
            "length {}",
            a.len()
        );
    }

    #[test]
    fn snapshots_round_trip_and_detect_damage() {
        let snapshot = Snapshot {
            rev: 77,
            text: "排版引擎\nAster".to_owned(),
        };
        let bytes = snapshot.encode();
        assert_eq!(Snapshot::decode(&bytes), Ok(snapshot));

        // Any single flipped bit, and any truncation, must be noticed.
        for i in 0..bytes.len() {
            let mut damaged = bytes.clone();
            damaged[i] ^= 0x10;
            assert!(
                Snapshot::decode(&damaged).is_err(),
                "flip at {i} went unnoticed"
            );
            assert!(
                Snapshot::decode(&bytes[..i]).is_err(),
                "truncation at {i} went unnoticed"
            );
        }
    }
}
