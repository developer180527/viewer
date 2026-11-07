//! Typed values: what a table cell or a tree node holds, so a view can tell `"1"` from
//! `1` and `null` from empty, align numbers, and sort.

use std::fmt;
use std::sync::Arc;

/// What kind of value a cell, a column or a tree node holds.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum ValueKind {
    /// Not known, or not said.
    #[default]
    Unknown,
    /// No value: JSON `null`, SQL `NULL`, a missing key.
    Null,
    Bool,
    /// Any number: integer, float, or one kept as written.
    Number,
    Text,
    Bytes,
    /// A date, a time, or both.
    Date,
    /// A value that couldn't be read: a bad line, a spreadsheet's `#DIV/0!`.
    Error,
    /// Keyed members: a JSON object, a YAML mapping, a TOML table, a dictionary.
    Map,
    /// Ordered items: an array, a sequence.
    List,
    /// A node of a format's own structure: an XML element, a USD prim, an FBX record.
    Group,
    /// A column whose cells are of several kinds.
    Mixed,
}

impl ValueKind {
    /// Whether values of this kind read best right-aligned.
    pub fn is_numeric(self) -> bool {
        matches!(self, ValueKind::Number)
    }
}

/// One value of a table (or a tree node's whole value, [`crate::TreeSource::value`]).
#[derive(Clone, Debug, PartialEq)]
#[non_exhaustive]
pub enum Cell {
    /// No value. Not the same as empty text.
    Null,
    Bool(bool),
    Int(i64),
    Float(f64),
    /// A number kept as the file wrote it: too large for `i64`, or written so that
    /// converting would change it (`1.50`, `0x1F`, `1e400`).
    Number(String),
    Text(String),
    Bytes(Arc<[u8]>),
    /// A date or time, as the file wrote it (ISO 8601 where the format has one).
    Date(String),
    /// A value that couldn't be read, and why.
    Error(String),
}

impl Cell {
    /// A number written as `text`: an [`Cell::Int`] when it is a plain integer that
    /// fits, else kept as written.
    pub fn number(text: &str) -> Cell {
        match text.parse::<i64>() {
            Ok(i) if !text.starts_with('+') && !(text.len() > 1 && text.trim_start_matches('-').starts_with('0')) => Cell::Int(i),
            _ => Cell::Number(text.to_string()),
        }
    }

    pub fn kind(&self) -> ValueKind {
        match self {
            Cell::Null => ValueKind::Null,
            Cell::Bool(_) => ValueKind::Bool,
            Cell::Int(_) | Cell::Float(_) | Cell::Number(_) => ValueKind::Number,
            Cell::Text(_) => ValueKind::Text,
            Cell::Bytes(_) => ValueKind::Bytes,
            Cell::Date(_) => ValueKind::Date,
            Cell::Error(_) => ValueKind::Error,
        }
    }

    pub fn is_null(&self) -> bool {
        matches!(self, Cell::Null)
    }

    /// The text, for a text cell.
    pub fn as_str(&self) -> Option<&str> {
        match self {
            Cell::Text(s) => Some(s),
            _ => None,
        }
    }

    /// The value as a number, where it is one: for sorting and charts.
    pub fn as_f64(&self) -> Option<f64> {
        match self {
            Cell::Int(i) => Some(*i as f64),
            Cell::Float(f) => Some(*f),
            Cell::Number(s) => match s.strip_prefix("0x").or_else(|| s.strip_prefix("0X")) {
                Some(hex) => i64::from_str_radix(hex, 16).ok().map(|i| i as f64),
                None => s.parse().ok(),
            },
            Cell::Bool(b) => Some(f64::from(u8::from(*b))),
            _ => None,
        }
    }
}

/// How a cell reads as plain text: empty for null, the number or text as it is, a
/// size for bytes. Views may show more (quotes, a null marker).
impl fmt::Display for Cell {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Cell::Null => Ok(()),
            Cell::Bool(b) => write!(f, "{b}"),
            Cell::Int(i) => write!(f, "{i}"),
            Cell::Float(x) => write!(f, "{x}"),
            Cell::Number(s) | Cell::Text(s) | Cell::Date(s) => f.write_str(s),
            Cell::Bytes(b) => write!(f, "{} bytes", b.len()),
            Cell::Error(why) => write!(f, "#error: {why}"),
        }
    }
}

impl From<String> for Cell {
    fn from(s: String) -> Self {
        Cell::Text(s)
    }
}

impl From<&str> for Cell {
    fn from(s: &str) -> Self {
        Cell::Text(s.to_string())
    }
}

/// A table's column: its name and the kind of value it holds.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub struct Column {
    pub name: String,
    /// [`ValueKind::Unknown`] when not known up front; [`ValueKind::Mixed`] when its
    /// cells are of several kinds.
    pub kind: ValueKind,
}

impl Column {
    pub fn new(name: impl Into<String>) -> Self {
        Self { name: name.into(), kind: ValueKind::Unknown }
    }

    pub fn with_kind(mut self, kind: ValueKind) -> Self {
        self.kind = kind;
        self
    }
}

impl From<&str> for Column {
    fn from(name: &str) -> Self {
        Column::new(name)
    }
}

impl From<String> for Column {
    fn from(name: String) -> Self {
        Column::new(name)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn numbers_keep_what_converting_would_change() {
        assert_eq!(Cell::number("42"), Cell::Int(42));
        assert_eq!(Cell::number("-7"), Cell::Int(-7));
        assert_eq!(Cell::number("0"), Cell::Int(0));
        for kept in ["1.50", "007", "+3", "0x1F", "99999999999999999999"] {
            assert_eq!(Cell::number(kept), Cell::Number(kept.into()), "{kept}");
        }
        assert_eq!(Cell::number("0x1F").as_f64(), Some(31.0));
        assert_eq!(Cell::Null.to_string(), "");
        assert_ne!(Cell::Null, Cell::Text(String::new()));
    }
}
