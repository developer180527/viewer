//! SYLK (`.slk`): a spreadsheet as lines of semicolon-separated records.
//!
//! Each `C` record is a cell: `C;Y3;X2;K"text";ER1C1+1`, its row and column carried over
//! from the record before when left out. Everything else (formats, widths) is ignored.

/// What a cell holds.
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub enum Value {
    Number(f64),
    Text(String),
    Bool(bool),
    /// `#N/A`, `#DIV/0!`…
    Error(String),
}

#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub struct Cell {
    pub row: u32,
    pub col: u32,
    pub value: Option<Value>,
    pub formula: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum Error {
    NotSylk,
    TooManyCells(u64),
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::NotSylk => write!(f, "not a SYLK file"),
            Error::TooManyCells(max) => write!(f, "more than {max} cells"),
        }
    }
}

impl std::error::Error for Error {}

/// Whether these are a SYLK file's first bytes.
pub fn detect(header: &[u8]) -> bool {
    header.starts_with(b"ID;")
}

fn value(k: &str) -> Value {
    if let Some(s) = k.strip_prefix('"') {
        return Value::Text(s.strip_suffix('"').unwrap_or(s).to_string());
    }
    match k {
        "TRUE" => Value::Bool(true),
        "FALSE" => Value::Bool(false),
        _ if k.starts_with('#') => Value::Error(k.to_string()),
        _ => k.parse().map_or_else(|_| Value::Text(k.to_string()), Value::Number),
    }
}

/// The fields of a record: `;;` is a semicolon, and doesn't end a field.
fn fields(line: &str) -> Vec<String> {
    let (mut out, mut now, mut chars) = (Vec::new(), String::new(), line.chars().peekable());
    while let Some(c) = chars.next() {
        if c != ';' {
            now.push(c);
        } else if chars.peek() == Some(&';') {
            chars.next();
            now.push(';');
        } else {
            out.push(std::mem::take(&mut now));
        }
    }
    out.push(now);
    out
}

/// Reads a file's cells, at most `max_cells`.
pub fn read(bytes: &[u8], max_cells: u64) -> Result<Vec<Cell>, Error> {
    if !detect(bytes) {
        return Err(Error::NotSylk);
    }
    let text = String::from_utf8_lossy(bytes);
    let (mut out, mut row, mut col) = (Vec::new(), 1u32, 1u32);
    for line in text.lines() {
        let fields = fields(line);
        match fields[0].as_str() {
            "E" => break,
            "F" => {
                for f in &fields[1..] {
                    if let Some(v) = f.strip_prefix('Y').and_then(|v| v.parse().ok()) {
                        row = v;
                    } else if let Some(v) = f.strip_prefix('X').and_then(|v| v.parse().ok()) {
                        col = v;
                    }
                }
            }
            "C" => {
                let (mut v, mut formula) = (None, None);
                for f in &fields[1..] {
                    let (key, rest) = f.split_at(f.chars().next().map_or(0, char::len_utf8));
                    match key {
                        "Y" => row = rest.parse().unwrap_or(row),
                        "X" => col = rest.parse().unwrap_or(col),
                        "K" => v = Some(value(rest)),
                        "E" => formula = Some(rest.to_string()),
                        _ => {}
                    }
                }
                if v.is_none() && formula.is_none() {
                    continue;
                }
                if out.len() as u64 >= max_cells {
                    return Err(Error::TooManyCells(max_cells));
                }
                // 1-based in the file; a nonsense 0 goes in the first row or column.
                out.push(Cell { row: row.saturating_sub(1), col: col.saturating_sub(1), value: v, formula });
            }
            _ => {}
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cells() {
        let c = read(b"ID;PWXL\r\nC;Y2;X3;K\"a;;b\"\r\nC;X4;K1.5;ER2C3*2\r\nF;Y5;X1\r\nC;KTRUE\r\nC;Y1;X1;K#N/A\r\nE\r\nC;K9\r\n", 10).unwrap();
        assert_eq!(c.len(), 4);
        assert_eq!((c[0].row, c[0].col, c[0].value.clone()), (1, 2, Some(Value::Text("a;b".into()))));
        assert_eq!((c[1].row, c[1].col, c[1].formula.as_deref()), (1, 3, Some("R2C3*2")));
        assert_eq!((c[2].row, c[2].col, c[2].value.clone()), (4, 0, Some(Value::Bool(true))));
        assert_eq!(c[3].value, Some(Value::Error("#N/A".into())));
        assert_eq!(read(b"nope", 1), Err(Error::NotSylk));
        assert_eq!(read(b"ID;\nC;K1\nC;K2\n", 1), Err(Error::TooManyCells(1)));
    }
}
