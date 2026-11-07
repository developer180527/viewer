//! Why a file couldn't be read, as something callers can match on.

use std::fmt;

use crate::{Limit, Unreadable};

pub type Result<T> = std::result::Result<T, ParseError>;

/// Why opening a file, or making one of its facets, failed.
///
/// The first two let the front door try another reader; the others are final and
/// worth telling the user as they are (a file that is too large is not shown as hex
/// instead). `InvalidRequest` is the caller's mistake, never the file's.
#[derive(Debug)]
#[non_exhaustive]
pub enum ParseError {
    /// The file isn't in this reader's format.
    NotThisFormat,
    /// The file is in this format but damaged or not understood: why, and where when
    /// the reader knows ("line 3, column 14").
    #[non_exhaustive]
    Corrupt { why: String, at: Option<Position> },
    /// The file claims more than the limits allow.
    TooLarge(LimitExceeded),
    /// A file this one needs couldn't be read.
    Unreadable { reference: String, reason: Unreadable },
    /// The document was closed before this finished.
    Cancelled,
    Io(std::io::Error),
    /// The format is known, but can't be read in this build or on this platform: the
    /// reason says so plainly ("EPUB isn't available on iPhone and iPad").
    Unsupported(String),
    /// The caller asked for something that doesn't exist: a facet, node or entry
    /// that wasn't handed out. A bug in the calling code, not a damaged file.
    InvalidRequest(String),
}

impl ParseError {
    pub fn corrupt(why: impl fmt::Display) -> Self {
        ParseError::Corrupt { why: why.to_string(), at: None }
    }

    /// Damage at a known place in the file.
    pub fn corrupt_at(why: impl fmt::Display, at: Position) -> Self {
        ParseError::Corrupt { why: why.to_string(), at: Some(at) }
    }

    /// Where in the file the error is, when it says.
    pub fn position(&self) -> Option<Position> {
        match self {
            ParseError::Corrupt { at, .. } => *at,
            _ => None,
        }
    }

    pub fn unsupported(why: impl fmt::Display) -> Self {
        ParseError::Unsupported(why.to_string())
    }

    pub fn invalid(why: impl fmt::Display) -> Self {
        ParseError::InvalidRequest(why.to_string())
    }

    /// A limit gone past: `value` where at most `max` is allowed.
    pub fn too_large(limit: Limit, value: u64, max: u64) -> Self {
        ParseError::TooLarge(LimitExceeded { limit, value, max })
    }

    /// Whether another reader may still read the file.
    pub fn try_next_reader(&self) -> bool {
        matches!(self, ParseError::NotThisFormat | ParseError::Corrupt { .. })
    }
}

impl fmt::Display for ParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ParseError::NotThisFormat => write!(f, "not in this format"),
            ParseError::Corrupt { why, at: Some(at) } => write!(f, "damaged or not understood: {at}: {why}"),
            ParseError::Corrupt { why, at: None } => write!(f, "damaged or not understood: {why}"),
            ParseError::TooLarge(e) => write!(f, "{e}"),
            ParseError::Unreadable { reference, reason } => write!(f, "{reference}: {reason}"),
            ParseError::Cancelled => write!(f, "cancelled"),
            ParseError::Io(e) => write!(f, "{e}"),
            ParseError::Unsupported(why) => write!(f, "{why}"),
            ParseError::InvalidRequest(why) => write!(f, "invalid request: {why}"),
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

/// A place in a file: a line, and a column on it, both from 1 as editors show them;
/// or a byte offset, for binary files.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub struct Position {
    /// From 1.
    pub line: Option<u64>,
    /// From 1, in characters.
    pub column: Option<u64>,
    /// From 0.
    pub byte: Option<u64>,
}

impl Position {
    /// Line `line` (from 1).
    pub fn line(line: u64) -> Self {
        Self { line: Some(line), column: None, byte: None }
    }

    /// Line `line`, column `column` (both from 1).
    pub fn line_column(line: u64, column: u64) -> Self {
        Self { line: Some(line), column: Some(column), byte: None }
    }

    /// Byte `offset` (from 0).
    pub fn byte(offset: u64) -> Self {
        Self { line: None, column: None, byte: Some(offset) }
    }

    /// The line and column of byte `offset` in `text`.
    pub fn in_text(text: &[u8], offset: usize) -> Self {
        let before = &text[..offset.min(text.len())];
        let line_start = before.iter().rposition(|&c| c == b'\n').map_or(0, |p| p + 1);
        let column = String::from_utf8_lossy(&before[line_start..]).chars().count() as u64 + 1;
        let line = before.iter().filter(|&&c| c == b'\n').count() as u64 + 1;
        Self { line: Some(line), column: Some(column), byte: Some(offset as u64) }
    }
}

impl fmt::Display for Position {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match (self.line, self.column, self.byte) {
            (Some(l), Some(c), _) => write!(f, "line {l}, column {c}"),
            (Some(l), None, _) => write!(f, "line {l}"),
            (None, _, Some(b)) => write!(f, "byte {b}"),
            (None, _, None) => f.write_str("somewhere"),
        }
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
