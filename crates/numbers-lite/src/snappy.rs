//! Snappy, as an IWA file uses it: chunks of `0x00`, a three-byte length, then one raw
//! Snappy block (no stream header, no checksums).

/// Why a block wasn't decompressed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Stop {
    /// It isn't valid Snappy.
    Damaged,
    /// It would pass the cap.
    Full,
}

/// One raw Snappy block, decompressed onto `out`, which may grow to `cap` bytes at most.
pub fn block(b: &[u8], out: &mut Vec<u8>, cap: usize) -> Result<(), Stop> {
    let (len, mut i) = varint(b).ok_or(Stop::Damaged)?;
    let start = out.len();
    let len = usize::try_from(len).map_err(|_| Stop::Full)?;
    let end = start.checked_add(len).filter(|e| *e <= cap).ok_or(Stop::Full)?;
    // What a block holds can't be more than 255 times its size (a run of copies);
    // reserving by that, not by its claim, keeps a few bytes from asking for the cap.
    out.reserve(len.min(b.len().saturating_mul(255)));
    while i < b.len() {
        element(b, &mut i, out, start, end).ok_or(Stop::Damaged)?;
    }
    if out.len() == end { Ok(()) } else { Err(Stop::Damaged) }
}

/// One element of a block, at `*i`: a literal or a copy, written onto `out`, which this
/// block began at `start` and must end at `end`. `None` when it is damaged.
fn element(b: &[u8], i: &mut usize, out: &mut Vec<u8>, start: usize, end: usize) -> Option<()> {
    let tag = *b.get(*i)?;
    *i += 1;
    // A literal: its length in the tag, or in the one to four bytes after it.
    if tag & 3 == 0 {
        let mut n = (tag >> 2) as usize;
        if n >= 60 {
            let raw = b.get(*i..*i + (n - 59))?;
            *i += raw.len();
            n = raw.iter().rev().fold(0usize, |a, c| a * 256 + *c as usize);
        }
        let lit = b.get(*i..i.checked_add(n + 1)?)?;
        *i += lit.len();
        (out.len() + lit.len() <= end).then(|| out.extend_from_slice(lit))?;
        return Some(());
    }
    // A copy of `n` bytes from `offset` back in what this block has written.
    let (n, offset) = match tag & 3 {
        1 => (((tag >> 2) & 7) as usize + 4, (tag >> 5) as usize * 256 + *b.get(*i)? as usize),
        2 => ((tag >> 2) as usize + 1, u16::from_le_bytes(b.get(*i..*i + 2)?.try_into().unwrap()) as usize),
        _ => ((tag >> 2) as usize + 1, u32::from_le_bytes(b.get(*i..*i + 4)?.try_into().unwrap()) as usize),
    };
    *i += [0, 1, 2, 4][(tag & 3) as usize];
    if offset == 0 || offset > out.len() - start || out.len() + n > end {
        return None;
    }
    // A copy may overlap what it writes (a run): byte by byte.
    let from = out.len() - offset;
    for k in 0..n {
        out.push(out[from + k]);
    }
    Some(())
}

fn varint(b: &[u8]) -> Option<(u64, usize)> {
    let mut v = 0u64;
    for (i, c) in b.iter().enumerate().take(10) {
        v |= ((c & 0x7f) as u64) << (7 * i);
        if c & 0x80 == 0 {
            return Some((v, i + 1));
        }
    }
    None
}

/// A whole IWA file: its chunks, decompressed one after another, `cap` bytes at most.
pub fn iwa(b: &[u8], cap: usize) -> Result<Vec<u8>, Stop> {
    let mut out = Vec::new();
    let mut i = 0;
    while i < b.len() {
        let head = b.get(i..i + 4).filter(|h| h[0] == 0).ok_or(Stop::Damaged)?;
        let n = u32::from_le_bytes([head[1], head[2], head[3], 0]) as usize;
        block(b.get(i + 4..i + 4 + n).ok_or(Stop::Damaged)?, &mut out, cap)?;
        i += 4 + n;
    }
    Ok(out)
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// A block of literals only: valid Snappy, as a test writes it.
    pub fn literal(data: &[u8]) -> Vec<u8> {
        let mut out = Vec::new();
        let mut n = data.len();
        loop {
            out.push((n & 0x7f) as u8 | if n > 0x7f { 0x80 } else { 0 });
            n >>= 7;
            if n == 0 {
                break;
            }
        }
        for piece in data.chunks(60) {
            out.push(((piece.len() - 1) as u8) << 2);
            out.extend_from_slice(piece);
        }
        out
    }

    /// An IWA file of one chunk.
    pub fn chunk(data: &[u8]) -> Vec<u8> {
        let block = literal(data);
        [&[0][..], &(block.len() as u32).to_le_bytes()[..3], &block].concat()
    }

    #[test]
    fn literals_and_copies() {
        let mut out = Vec::new();
        // "abcd", then a copy of 6 from 4 back (a run), then a long literal.
        let long = [b'z'; 70];
        let b = [&[80][..], &[3 << 2], b"abcd", &[(6 - 4) << 2 | 1, 4], &[60 << 2, 69], &long].concat();
        block(&b, &mut out, 1000).unwrap();
        assert_eq!(out, [&b"abcdabcdab"[..], &long].concat());
        // Two-byte and four-byte offsets.
        let mut out = b"0123456789".to_vec();
        block(&[5, 0, b'x', (2 - 1) << 2 | 2, 1, 0, (2 - 1) << 2 | 3, 3, 0, 0, 0], &mut out, 100).unwrap();
        assert_eq!(&out[10..], b"xxxxx");
        assert_eq!(literal(&[7; 200]).len(), 2 + 200 + 4);
        let mut out = Vec::new();
        block(&literal(&[7; 200]), &mut out, 200).unwrap();
        assert_eq!(out, [7; 200]);
    }

    #[test]
    fn long_literals_and_far_copies() {
        // 300 bytes, each its place: a literal whose length takes two bytes, then
        // copies from 300, 299 and 298 back, by offsets of one, two and four bytes.
        let data: Vec<u8> = (0..300u32).map(|i| (i * 7 % 251) as u8).collect();
        let b = [&[0xb7, 0x02][..], &[61 << 2, 0x2b, 0x01], &data, &[(5 - 4) << 2 | 1 | 1 << 5, 44], &[(4 - 1) << 2 | 2, 0x2b, 0x01], &[(2 - 1) << 2 | 3, 0x2a, 0x01, 0, 0]].concat();
        let mut out = Vec::new();
        block(&b, &mut out, 400).unwrap();
        assert_eq!(out.len(), 311);
        assert_eq!(&out[..300], &data[..]);
        assert_eq!(&out[300..305], &data[..5]);
        assert_eq!(&out[305..309], &data[6..10]);
        assert_eq!(&out[309..], &data[11..13]);
        // A literal's length in three bytes.
        let big = vec![3u8; 70_000];
        let b = [&[0xf0, 0xa2, 0x04][..], &[62 << 2, 0x6f, 0x11, 0x01], &big].concat();
        let mut out = Vec::new();
        block(&b, &mut out, 70_000).unwrap();
        assert_eq!(out, big);
    }

    #[test]
    fn damage_is_refused() {
        let bad: [&[u8]; 8] = [
            &[],                      // no length
            &[0x80; 12],              // a length that never ends
            &[4, 3 << 2, b'a'],       // a literal cut short
            &[4, 0, b'a'],            // shorter than it says
            &[1, 1 << 2, b'a', b'b'], // longer than it says
            &[4, 1, 0],               // a copy from nothing
            &[4, 0, b'a', 1, 5],      // a copy from before the start
            &[4, 61 << 2, 0xff],      // a literal's length cut short
        ];
        for b in bad {
            assert_eq!(block(b, &mut Vec::new(), 100), Err(Stop::Damaged), "{b:?}");
        }
        // A copy reaching into what an earlier block wrote, or cut short.
        assert_eq!(block(&[2, 1 << 2 | 2, 1, 0], &mut vec![9], 100), Err(Stop::Damaged));
        assert_eq!(block(&[3, 0, b'a', 1 << 2 | 2, 1], &mut Vec::new(), 100), Err(Stop::Damaged));
        assert_eq!(block(&[3, 0, b'a', 1 << 2 | 3, 1, 0, 0], &mut Vec::new(), 100), Err(Stop::Damaged));
        for b in [&[1, 0, 0, 0][..], &[0, 9, 0, 0, 1], &[0, 0]] {
            assert_eq!(iwa(b, 10), Err(Stop::Damaged), "{b:?}");
        }
        assert_eq!(iwa(&[], 0).unwrap(), b"");
    }

    #[test]
    fn the_cap_is_told_from_damage() {
        assert!(block(&literal(&[1; 50]), &mut Vec::new(), 50).is_ok());
        assert_eq!(block(&literal(&[1; 50]), &mut Vec::new(), 49), Err(Stop::Full));
        // What's already there counts.
        assert_eq!(block(&literal(&[1; 50]), &mut vec![0; 10], 59), Err(Stop::Full));
        assert!(block(&literal(&[1; 50]), &mut vec![0; 10], 60).is_ok());
        assert_eq!(iwa(&[chunk(b"ab"), chunk(b"c")].concat(), 3).unwrap(), b"abc");
        assert_eq!(iwa(&[chunk(b"ab"), chunk(b"c")].concat(), 2), Err(Stop::Full));
        // A length no machine holds.
        assert_eq!(block(&[0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0x01], &mut Vec::new(), 1 << 40), Err(Stop::Full));
    }
}
