//! DIF (`.dif`, Data Interchange Format): a header of named sections, then values one
//! by one, each as a `type,number` line and a text line.

/// What a cell holds.
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub enum Value {
    Number(f64),
    Text(String),
    Bool(bool),
    /// DIF's `NA` and `ERROR`.
    Error(String),
}

#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub struct Cell {
    pub row: u32,
    pub col: u32,
    pub value: Value,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum Error {
    NotDif,
    TooManyCells(u64),
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::NotDif => write!(f, "not a DIF file"),
            Error::TooManyCells(max) => write!(f, "more than {max} cells"),
        }
    }
}

impl std::error::Error for Error {}

/// Whether these are a DIF file's first bytes.
pub fn detect(header: &[u8]) -> bool {
    header.starts_with(b"TABLE\r\n0,1") || header.starts_with(b"TABLE\n0,1")
}

fn unquote(s: &str) -> String {
    let s = s.trim();
    s.strip_prefix('"').map_or(s, |x| x.strip_suffix('"').unwrap_or(x)).replace("\"\"", "\"")
}

/// Reads a file's cells, at most `max_cells`.
pub fn read(bytes: &[u8], max_cells: u64) -> Result<Vec<Cell>, Error> {
    if !detect(bytes) {
        return Err(Error::NotDif);
    }
    let text = String::from_utf8_lossy(bytes);
    let mut lines = text.lines();
    // The header ends with DATA, its two lines, and then the values.
    while let Some(l) = lines.next() {
        if l.trim() == "DATA" {
            lines.next();
            lines.next();
            break;
        }
    }
    let (mut out, mut row, mut col) = (Vec::new(), None::<u32>, 0u32);
    while let (Some(head), Some(body)) = (lines.next(), lines.next()) {
        let (kind, number) = head.split_once(',').unwrap_or((head, ""));
        let body = body.trim();
        let value = match (kind.trim(), body) {
            ("-1", "BOT") => {
                row = Some(row.map_or(0, |r| r.saturating_add(1)));
                col = 0;
                continue;
            }
            ("-1", _) => break, // EOD
            ("0", "V") => number.trim().parse().ok().map(Value::Number),
            ("0", "TRUE") => Some(Value::Bool(true)),
            ("0", "FALSE") => Some(Value::Bool(false)),
            ("0", "NA" | "ERROR") => Some(Value::Error(body.to_string())),
            ("1", b) => Some(Value::Text(unquote(b))).filter(|v| *v != Value::Text(String::new())),
            _ => None,
        };
        if let (Some(value), Some(row)) = (value, row) {
            if out.len() as u64 >= max_cells {
                return Err(Error::TooManyCells(max_cells));
            }
            out.push(Cell { row, col, value });
        }
        col = col.saturating_add(1);
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cells() {
        let f = "TABLE\n0,1\n\"X\"\nVECTORS\n0,2\n\"\"\nTUPLES\n0,2\n\"\"\nDATA\n0,0\n\"\"\n-1,0\nBOT\n1,0\n\"a\"\"b\"\n0,1.5\nV\n-1,0\nBOT\n0,0\nNA\n1,0\n\"\"\n-1,0\nEOD\n";
        let c = read(f.as_bytes(), 9).unwrap();
        assert_eq!(c, vec![
            Cell { row: 0, col: 0, value: Value::Text("a\"b".into()) },
            Cell { row: 0, col: 1, value: Value::Number(1.5) },
            Cell { row: 1, col: 0, value: Value::Error("NA".into()) },
        ]);
        assert_eq!(read(b"x", 9), Err(Error::NotDif));
    }
}
