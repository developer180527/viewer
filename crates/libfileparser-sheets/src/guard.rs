//! What's checked before calamine sees a file. calamine lays a sheet out densely, rows
//! times columns, so a few bytes naming cells A1 and XFD1048576 would ask for hundreds
//! of gigabytes, and a failed allocation ends the process: no error, nothing to catch.
//! XLSX and XLSB are read cell by cell instead (workbook.rs), so only their unzipped
//! size is checked here; XLS, which calamine has no such path for, is measured.

use std::io::{Cursor, Read};

use libfileparser_core::{Limit, Limits, ParseError, Result};

use crate::workbook::Shared;

/// Every entry of a zip-based workbook, unzipped, against the unpack limit.
pub fn zip_size(bytes: &Shared, limits: &Limits) -> Result<()> {
    let packed = bytes.as_ref().len() as u64;
    let Ok(mut zip) = zip::ZipArchive::new(Cursor::new(bytes.clone())) else { return Ok(()) };
    let mut total = 0u64;
    for i in 0..zip.len() {
        if let Ok(f) = zip.by_index_raw(i) {
            total = total.saturating_add(f.size());
        }
    }
    limits.check_unpack(packed, total)
}

/// How many cells calamine may lay out for one workbook: its cell is 32 bytes.
fn budget(limits: &Limits) -> u64 {
    limits.allocation / 32
}

fn check(total: u64, limits: &Limits) -> Result<()> {
    let max = budget(limits);
    if total > max {
        return Err(ParseError::too_large(Limit::Allocation, total.saturating_mul(32), max.saturating_mul(32)));
    }
    Ok(())
}

/// XLS: walks the BIFF records for each sheet's extent. calamine lays out every sheet
/// when it opens the file.
pub fn xls_extent(bytes: &Shared, limits: &Limits) -> Result<()> {
    let Ok(mut file) = cfb::CompoundFile::open(Cursor::new(bytes.clone())) else { return Ok(()) };
    let mut stream = Vec::new();
    let mut read = false;
    for name in ["/Workbook", "/Book"] {
        if let Ok(mut s) = file.open_stream(name) {
            if s.len() > limits.allocation {
                return Err(ParseError::too_large(Limit::Allocation, s.len(), limits.allocation));
            }
            read = s.read_to_end(&mut stream).is_ok();
            break;
        }
    }
    if !read {
        return Ok(());
    }
    // Rows and columns seen in the sheet being walked, and the total so far.
    let (mut rows, mut cols, mut seen, mut total) = ((u16::MAX, 0u16), (u16::MAX, 0u16), false, 0u64);
    let mut at = 0usize;
    while let Some(head) = stream.get(at..at + 4) {
        let (id, len) = (u16::from_le_bytes([head[0], head[1]]), u16::from_le_bytes([head[2], head[3]]) as usize);
        let Some(body) = stream.get(at + 4..at + 4 + len) else { break };
        let u16_at = |o: usize| body.get(o..o + 2).map(|b| u16::from_le_bytes([b[0], b[1]]));
        match id {
            // BOF and EOF of a substream.
            0x0809 | 0x000a if seen => {
                total = total.saturating_add(((rows.1 - rows.0) as u64 + 1) * ((cols.1 - cols.0) as u64 + 1));
                check(total, limits)?;
                (rows, cols, seen) = ((u16::MAX, 0), (u16::MAX, 0), false);
            }
            // Cells: row, column first; a run of them ends with its last column.
            0x0006 | 0x00bd | 0x00be | 0x00fd | 0x0201 | 0x0203 | 0x0204 | 0x0205 | 0x027e => {
                if let (Some(r), Some(c)) = (u16_at(0), u16_at(2)) {
                    let last = if matches!(id, 0x00bd | 0x00be) { len.checked_sub(2).and_then(u16_at).unwrap_or(c) } else { c };
                    rows = (rows.0.min(r), rows.1.max(r));
                    cols = (cols.0.min(c), cols.1.max(last.max(c)));
                    seen = true;
                }
            }
            _ => {}
        }
        at += 4 + len;
    }
    Ok(())
}
