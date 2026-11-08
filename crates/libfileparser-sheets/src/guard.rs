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

/// Whether every sector chain of a compound file ends. calamine follows a chain until
/// it ends or has the length asked for, which for the directory is unbounded: a chain
/// that loops back on itself spins there, growing a buffer, until memory runs out.
/// A file allocation table (FAT) with a loop in it is damaged, so the whole table is
/// checked, and then the mini table it leads to. Files calamine refuses for other
/// reasons pass, to be refused there.
fn chains_end(b: &[u8]) -> bool {
    const RESERVED: u32 = 0xFFFF_FFFA;
    const DIFSECT: u32 = 0xFFFF_FFFC;
    let (Some(head), Some(shift)) = (b.get(..512), b.get(30..32)) else { return true };
    let size = match u16::from_le_bytes([shift[0], shift[1]]) {
        9 => 512,
        12 => 4096,
        _ => return true,
    };
    let u32s = |s: &[u8]| s.as_chunks::<4>().0.iter().map(|c| u32::from_le_bytes(*c)).collect::<Vec<_>>();
    let u32_at = |at: usize| u32::from_le_bytes([head[at], head[at + 1], head[at + 2], head[at + 3]]);
    // As calamine reads them: sector `id` follows the header, which is one sector long.
    let maxid = b.len() / size;
    let sector = |id: u32| -> Option<&[u8]> {
        let start = (id as usize).checked_add(1)?.checked_mul(size)?;
        (id as usize <= maxid).then(|| b.get(start..(start + size).min(b.len()))).flatten()
    };
    let mut difat = u32s(&head[76..512]);
    let mut id = u32_at(68);
    let mut steps = 0usize;
    while id < RESERVED {
        steps += 1;
        let Some(s) = sector(id).filter(|_| steps <= maxid + 1) else { return true };
        difat.extend(u32s(s));
        let Some(next) = difat.pop() else { return true };
        id = next;
    }
    let mut fat = Vec::new();
    for id in difat.into_iter().filter(|id| *id < DIFSECT) {
        let Some(s) = sector(id) else { return true };
        fat.extend(u32s(s));
    }
    if !acyclic(&fat) {
        return false;
    }
    // The mini table's own chain, now known to end, then the table.
    let (mut mini, mut id, want) = (Vec::new(), u32_at(60), (u32_at(64) as usize).saturating_mul(size));
    while (id as usize) < fat.len() && mini.len() < want {
        let Some(s) = sector(id) else { break };
        mini.extend_from_slice(s);
        id = fat[id as usize];
    }
    mini.truncate(want);
    acyclic(&u32s(&mini))
}

/// Whether following `next[i]` from any `i` always leaves the table (an end of chain,
/// a free sector, or a sector past it) rather than coming back round.
fn acyclic(next: &[u32]) -> bool {
    // 0: not seen; 1: on the path being walked; 2: known to leave.
    let mut state = vec![0u8; next.len()];
    let mut path = Vec::new();
    for start in 0..next.len() {
        let mut at = start;
        while state[at] == 0 {
            state[at] = 1;
            path.push(at);
            match next[at] as usize {
                n if n < next.len() => at = n,
                _ => break,
            }
        }
        if state[at] == 1 && path.last().is_some_and(|&last| (next[last] as usize) < next.len()) {
            return false;
        }
        for p in path.drain(..) {
            state[p] = 2;
        }
    }
    true
}

/// XLS: walks the BIFF records for each sheet's extent. calamine lays out every sheet
/// when it opens the file.
pub fn xls_extent(bytes: &Shared, limits: &Limits) -> Result<()> {
    if !chains_end(bytes.as_ref()) {
        return Err(ParseError::corrupt("a chain of sectors in the workbook loops back on itself"));
    }
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
    // A stream cut short of its last EOF: that sheet is laid out all the same.
    if seen {
        total = total.saturating_add(((rows.1 - rows.0) as u64 + 1) * ((cols.1 - cols.0) as u64 + 1));
        check(total, limits)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_table_with_a_loop_in_it_is_found() {
        const END: u32 = 0xFFFF_FFFE;
        assert!(acyclic(&[1, 2, END, END, 0xFFFF_FFFF]));
        assert!(acyclic(&[9, 0]));
        assert!(!acyclic(&[1, 2, 0]));
        assert!(!acyclic(&[END, 1]));
        assert!(!acyclic(&[2, END, 3, 2]));
        assert!(acyclic(&[]));
    }
}
