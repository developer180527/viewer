//! What's checked before calamine sees a file. calamine lays a sheet out densely, rows
//! times columns, so a few bytes naming cells A1 and XFD1048576 would ask for hundreds
//! of gigabytes, and a failed allocation ends the process: no error, nothing to catch.
//! XLSX and XLSB are read cell by cell instead (workbook.rs), so only their unzipped
//! size is checked here; XLS, which calamine has no such path for, is measured.

use std::io::Cursor;

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

/// Bytes calamine holds per cell it lays out or reserves: a value is 32, a value with
/// its position 40.
const CELL: u64 = 40;

/// How many cells calamine may lay out for one workbook.
fn budget(limits: &Limits) -> u64 {
    limits.allocation / CELL
}

fn check(total: u64, limits: &Limits) -> Result<()> {
    let max = budget(limits);
    if total > max {
        return Err(ParseError::too_large(Limit::Allocation, total.saturating_mul(CELL), max.saturating_mul(CELL)));
    }
    Ok(())
}

/// The cells a Dimensions record (MS-XLS 2.4.90) makes calamine reserve, read as
/// calamine reads it; `None` when its last row comes before its first, which calamine
/// subtracts without checking (a panic, or in a release build a reservation of
/// billions).
fn dimensions(r: &[u8]) -> Option<u64> {
    let u16_at = |o: usize| u16::from_le_bytes([r[o], r[o + 1]]) as u32;
    let u32_at = |o: usize| u32::from_le_bytes([r[o], r[o + 1], r[o + 2], r[o + 3]]);
    let (rf, rl, mut cf, cl) = match r.len() {
        10 => (u16_at(0), u16_at(2), u16_at(4), u16_at(6)),
        14 | 16 => (u32_at(0), u32_at(4), u16_at(8), u16_at(10)),
        _ => return Some(0),
    };
    if 0xff < cf || cl < cf {
        cf = 0;
    }
    if rl == 0 || cl == 0 {
        return Some(1);
    }
    let rows = (rl - 1).checked_sub(rf)? as u64 + 1;
    let cols = (cl - 1).checked_sub(cf)? as u64 + 1;
    Some(rows * cols)
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

/// The workbook stream ("Workbook", or "Book" before Excel 97) of a compound file, read
/// as calamine reads it: directory entries scanned in order by name, without the
/// red-black tree's ordering checked, and a stream under 4096 bytes taken from the mini
/// stream. `None` when calamine would find no such stream or fail to read it. Run after
/// [`chains_end`], so every chain followed here ends; each is bounded all the same.
fn workbook_stream(b: &[u8], limits: &Limits) -> Result<Option<Vec<u8>>> {
    const RESERVED: u32 = 0xFFFF_FFFA;
    const DIFSECT: u32 = 0xFFFF_FFFC;
    const END: u32 = 0xFFFF_FFFE;
    let (Some(head), Some(shift)) = (b.get(..512), b.get(30..32)) else { return Ok(None) };
    let size = match u16::from_le_bytes([shift[0], shift[1]]) {
        9 => 512,
        12 => 4096,
        _ => return Ok(None),
    };
    let u32s = |s: &[u8]| s.as_chunks::<4>().0.iter().map(|c| u32::from_le_bytes(*c)).collect::<Vec<_>>();
    let u32_at = |at: usize| u32::from_le_bytes([head[at], head[at + 1], head[at + 2], head[at + 3]]);
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
        let Some(s) = sector(id).filter(|_| steps <= maxid + 1) else { return Ok(None) };
        difat.extend(u32s(s));
        let Some(next) = difat.pop() else { return Ok(None) };
        id = next;
    }
    let mut fat = Vec::new();
    for id in difat.into_iter().filter(|id| *id < DIFSECT) {
        let Some(s) = sector(id) else { return Ok(None) };
        fat.extend(u32s(s));
    }
    // A chain of `unit`-sized pieces, up to `len` bytes; `None` where calamine would fail.
    fn chain<'a>(mut id: u32, next: &[u32], piece: impl Fn(u32) -> Option<&'a [u8]>, len: usize) -> Option<Vec<u8>> {
        let mut out = Vec::new();
        let mut steps = 0usize;
        while id != END {
            steps += 1;
            if steps > next.len() + 1 {
                return None;
            }
            let s = piece(id)?;
            out.extend_from_slice(&s[..s.len().min(len - out.len())]);
            if out.len() >= len || s.is_empty() {
                break;
            }
            id = *next.get(id as usize)?;
        }
        Some(out)
    }
    let Some(dirs) = chain(u32_at(48), &fat, sector, usize::MAX) else { return Ok(None) };
    let entry = |d: &[u8]| -> (String, u32, usize) {
        let units: Vec<u16> = d[..64].as_chunks::<2>().0.iter().map(|c| u16::from_le_bytes(*c)).collect();
        let name: String = char::decode_utf16(units).map(|c| c.unwrap_or('\u{fffd}')).take_while(|c| *c != '\0').collect();
        let start = u32::from_le_bytes([d[116], d[117], d[118], d[119]]);
        let len = if size == 512 { u32::from_le_bytes([d[120], d[121], d[122], d[123]]) as u64 } else { u64::from_le_bytes(d[120..128].try_into().unwrap()) };
        (name, start, usize::try_from(len).unwrap_or(usize::MAX))
    };
    let dirs: Vec<(String, u32, usize)> = dirs.as_chunks::<128>().0.iter().map(|d| entry(d)).collect();
    let Some(root) = dirs.first() else { return Ok(None) };
    let found = ["Workbook", "Book"].iter().find_map(|want| dirs.iter().find(|d| d.0.eq_ignore_ascii_case(want)));
    let Some((_, start, len)) = found else { return Ok(None) };
    if *len as u64 > limits.allocation {
        return Err(ParseError::too_large(Limit::Allocation, *len as u64, limits.allocation));
    }
    if *len >= 4096 {
        return Ok(chain(*start, &fat, sector, *len));
    }
    // A small stream lives in the mini stream: 64-byte pieces, chained by the mini table.
    let mini_len = (u32_at(64) as usize).saturating_mul(size);
    let (Some(mini_fat), Some(mini)) = (chain(u32_at(60), &fat, sector, mini_len), chain(root.1, &fat, sector, root.2)) else { return Ok(None) };
    let mini_fat = u32s(&mini_fat);
    Ok(chain(*start, &mini_fat, |id| mini.get(id as usize * 64..(id as usize * 64 + 64).min(mini.len())), *len))
}

/// XLS: walks the BIFF records for each sheet's extent. calamine lays out every sheet
/// when it opens the file.
pub fn xls_extent(bytes: &Shared, limits: &Limits) -> Result<()> {
    if !chains_end(bytes.as_ref()) {
        return Err(ParseError::corrupt("a chain of sectors in the workbook loops back on itself"));
    }
    // The stream calamine will read, found as calamine finds it: a stricter reader would
    // refuse files calamine opens. One that can't be found can't be measured, so it isn't
    // handed over.
    let stream = match workbook_stream(bytes.as_ref(), limits)? {
        Some(s) => s,
        None => return Err(ParseError::corrupt("the workbook's compound file is damaged")),
    };
    // Rows and columns seen in the sheet being walked, and the total so far.
    let (mut rows, mut cols, mut seen, mut total) = ((u16::MAX, 0u16), (u16::MAX, 0u16), false, 0u64);
    let mut at = 0usize;
    while let Some(head) = stream.get(at..at + 4) {
        let (id, len) = (u16::from_le_bytes([head[0], head[1]]), u16::from_le_bytes([head[2], head[3]]) as usize);
        let Some(body) = stream.get(at + 4..at + 4 + len) else { break };
        let u16_at = |o: usize| body.get(o..o + 2).map(|b| u16::from_le_bytes([b[0], b[1]]));
        match id {
            // FilePass: what follows is encrypted, so it can't be measured, and calamine
            // can't decrypt it.
            0x002f => return Err(crate::locked()),
            // Dimensions: calamine reserves what it claims, cells or no cells.
            0x0200 => match dimensions(body) {
                Some(n) => check(total.saturating_add(n), limits)?,
                None => return Err(ParseError::corrupt("a sheet's dimensions end before they start")),
            },
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

    /// A version 3 compound file of 512-byte sectors holding `workbook` (at least 4096
    /// bytes, so outside the mini stream) as "Workbook", with a sibling whose name sorts
    /// after it on its left: out of the order MS-CFB asks for, as some writers leave it.
    fn compound(workbook: &[u8]) -> Vec<u8> {
        const END: u32 = 0xFFFF_FFFE;
        const FREE: u32 = 0xFFFF_FFFF;
        assert!(workbook.len() >= 4096);
        let sectors = workbook.len().div_ceil(512) as u32;
        let mut head = vec![0u8; 512];
        head[..8].copy_from_slice(b"\xD0\xCF\x11\xE0\xA1\xB1\x1A\xE1");
        let mut put = |at: usize, v: &[u8]| head[at..at + v.len()].copy_from_slice(v);
        put(24, &[0x3e, 0, 3, 0, 0xfe, 0xff, 9, 0, 6, 0]);
        put(44, &1u32.to_le_bytes());
        put(48, &1u32.to_le_bytes());
        put(56, &4096u32.to_le_bytes());
        put(60, &END.to_le_bytes());
        put(68, &END.to_le_bytes());
        put(76, &0u32.to_le_bytes());
        for i in 1..109 {
            put(76 + 4 * i, &FREE.to_le_bytes());
        }
        // Sector 0 the table, 1 the directory, 2 on the workbook.
        let mut fat = vec![FREE; 128];
        fat[0] = 0xFFFF_FFFD;
        fat[1] = END;
        for i in 0..sectors {
            fat[2 + i as usize] = if i + 1 == sectors { END } else { 3 + i };
        }
        let entry = |name: &str, kind: u8, left: u32, child: u32, start: u32, len: u32| {
            let mut e = vec![0u8; 128];
            let units: Vec<u8> = name.encode_utf16().chain([0]).flat_map(u16::to_le_bytes).collect();
            e[..units.len()].copy_from_slice(&units);
            e[64..66].copy_from_slice(&(units.len() as u16).to_le_bytes());
            e[66] = kind;
            e[68..72].copy_from_slice(&left.to_le_bytes());
            e[72..76].copy_from_slice(&FREE.to_le_bytes());
            e[76..80].copy_from_slice(&child.to_le_bytes());
            e[116..120].copy_from_slice(&start.to_le_bytes());
            e[120..124].copy_from_slice(&len.to_le_bytes());
            e
        };
        let dir = [entry("Root Entry", 5, FREE, 1, END, 0), entry("Workbook", 2, 2, FREE, 2, workbook.len() as u32), entry("ZZZZZZZZZZZZ", 2, FREE, FREE, END, 0), vec![0; 128]].concat();
        let mut out = head;
        out.extend(fat.iter().flat_map(|v| v.to_le_bytes()));
        out.extend(dir);
        out.extend(workbook);
        out.resize(512 * (3 + sectors as usize), 0);
        out
    }

    #[test]
    fn the_workbook_is_found_whatever_order_its_directory_is_in() {
        let stream: Vec<u8> = (0..5000u32).map(|i| i as u8).collect();
        assert_eq!(workbook_stream(&compound(&stream), &Limits::desktop()).unwrap(), Some(stream));
        // A file with no workbook stream is refused rather than handed to calamine.
        let mut other = compound(&[0; 4096]);
        let at = 512 * 2 + 128;
        other[at..at + 2].copy_from_slice(&[b'X', 0]);
        assert_eq!(workbook_stream(&other, &Limits::desktop()).unwrap(), None);
    }

    #[test]
    fn an_encrypted_workbook_is_said_to_be_locked() {
        // BOF, then FilePass: everything after it is ciphertext.
        let mut stream = [&[0x09, 0x08, 16, 0][..], &[0; 16], &[0x2f, 0, 6, 0], &[0; 6]].concat();
        stream.resize(4096, 0xA5);
        let e = xls_extent(&Shared(std::sync::Arc::new(compound(&stream))), &Limits::desktop()).unwrap_err();
        assert!(matches!(e, ParseError::Unsupported { .. }) && e.to_string().contains("password"), "{e}");
    }

    #[test]
    fn dimensions_are_read_as_calamine_reads_them() {
        let biff8 = |rf: u32, rl: u32, cf: u16, cl: u16| [&rf.to_le_bytes()[..], &rl.to_le_bytes(), &cf.to_le_bytes(), &cl.to_le_bytes(), &[0, 0]].concat();
        assert_eq!(dimensions(&biff8(0, 10, 0, 4)), Some(40));
        // A million rows by 16,384 columns: what a small file can claim.
        assert_eq!(dimensions(&biff8(0, 1 << 20, 0, 1 << 14)), Some(1 << 34));
        // Columns past 255 start at the first, as calamine has them.
        assert_eq!(dimensions(&biff8(0, 1, 300, 302)), Some(302));
        assert_eq!(dimensions(&biff8(5, 2, 0, 1)), None);
        assert_eq!(dimensions(&biff8(0, 0, 0, 0)), Some(1));
        assert_eq!(dimensions(&[0; 3]), Some(0));
    }
}
