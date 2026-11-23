use std::fmt;

/// Why a file could not be read. [`Error::kind`] is what the shell shows a
/// message for; the details are for logs and tests.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Error {
    /// Not a zip file at all.
    NotZip(String),
    /// A compound file: an Office 97–2003 document (`.xls`, `.ppt`) or an
    /// Office 2007+ document protected with a password. Both look the same
    /// from the outside.
    LegacyOrEncrypted,
    /// A zip file, but not an Office document, or one of another kind than
    /// asked for (a presentation opened as a workbook).
    WrongKind {
        expected: &'static str,
        found: String,
    },
    /// Over one of the safety limits: too many parts, a part or the whole
    /// too large, or compressed suspiciously well.
    TooLarge(String),
    /// A part the document needs is missing or unreadable.
    Broken(String),
}

/// The kinds of [`Error`], for choosing a message.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ErrorKind {
    NotZip,
    LegacyOrEncrypted,
    WrongKind,
    TooLarge,
    Broken,
}

impl Error {
    pub fn kind(&self) -> ErrorKind {
        match self {
            Error::NotZip(_) => ErrorKind::NotZip,
            Error::LegacyOrEncrypted => ErrorKind::LegacyOrEncrypted,
            Error::WrongKind { .. } => ErrorKind::WrongKind,
            Error::TooLarge(_) => ErrorKind::TooLarge,
            Error::Broken(_) => ErrorKind::Broken,
        }
    }

    pub(crate) fn broken(part: &str, what: impl fmt::Display) -> Self {
        Error::Broken(format!("{part}: {what}"))
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::NotZip(detail) => write!(f, "not a zip file: {detail}"),
            Error::LegacyOrEncrypted => {
                write!(
                    f,
                    "an Office 97-2003 file, or one protected with a password"
                )
            }
            Error::WrongKind { expected, found } => {
                write!(f, "expected {expected}, found {found}")
            }
            Error::TooLarge(detail) => write!(f, "over a safety limit: {detail}"),
            Error::Broken(detail) => write!(f, "damaged: {detail}"),
        }
    }
}

impl std::error::Error for Error {}

pub type Result<T> = std::result::Result<T, Error>;
