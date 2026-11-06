//! Why a file couldn't be read, as something callers can match on.

use std::fmt;

use crate::{Limit, Unreadable};

pub type Result<T> = std::result::Result<T, ParseError>;

/// Why opening a file, or making one of its facets, failed.
///
/// The first two let the front door try another reader; the others are final and
/// worth telling the user as they are (a file that is too large is not shown as hex
/// instead).
#[derive(Debug)]
#[non_exhaustive]
pub enum ParseError {
    /// The file isn't in this reader's format.
    NotThisFormat,
    /// The file is in this format but damaged or not understood.
    Corrupt(String),
    /// The file claims more than the limits allow.
    TooLarge(LimitExceeded),
    /// A file this one needs couldn't be read.
    Unreadable { reference: String, reason: Unreadable },
    /// The document was closed before this finished.
    Cancelled,
    Io(std::io::Error),
}

impl ParseError {
    pub fn corrupt(why: impl fmt::Display) -> Self {
        ParseError::Corrupt(why.to_string())
    }

    /// A limit gone past: `value` where at most `max` is allowed.
    pub fn too_large(limit: Limit, value: u64, max: u64) -> Self {
        ParseError::TooLarge(LimitExceeded { limit, value, max })
    }

    /// Whether another reader may still read the file.
    pub fn try_next_reader(&self) -> bool {
        matches!(self, ParseError::NotThisFormat | ParseError::Corrupt(_))
    }
}

impl fmt::Display for ParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ParseError::NotThisFormat => write!(f, "not in this format"),
            ParseError::Corrupt(why) => write!(f, "damaged or not understood: {why}"),
            ParseError::TooLarge(e) => write!(f, "{e}"),
            ParseError::Unreadable { reference, reason } => write!(f, "{reference}: {reason}"),
            ParseError::Cancelled => write!(f, "cancelled"),
            ParseError::Io(e) => write!(f, "{e}"),
        }
    }
}

impl std::error::Error for ParseError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            ParseError::Io(e) => Some(e),
            _ => None,
        }
    }
}

impl From<std::io::Error> for ParseError {
    fn from(e: std::io::Error) -> Self {
        ParseError::Io(e)
    }
}

/// A limit a file went past: which, how much it claimed, and the most allowed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub struct LimitExceeded {
    pub limit: Limit,
    pub value: u64,
    pub max: u64,
}

impl fmt::Display for LimitExceeded {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "too large: {} is {}, more than the {} allowed", self.limit, self.value, self.max)
    }
}
