//! Protocol Buffers' wire format, read without a schema: fields by number.

/// A field's value as the wire has it.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Wire<'a> {
    Varint(u64),
    Bytes(&'a [u8]),
    Fixed64(u64),
    Fixed32(u32),
}

/// A message's fields in order; ends at the first that is damaged.
pub struct Fields<'a> {
    b: &'a [u8],
}

pub fn fields(b: &[u8]) -> Fields<'_> {
    Fields { b }
}

pub fn varint(b: &mut &[u8]) -> Option<u64> {
    let mut v = 0u64;
    for i in 0..10 {
        let c = *b.get(i)?;
        v |= ((c & 0x7f) as u64) << (7 * i);
        if c & 0x80 == 0 {
            *b = &b[i + 1..];
            return Some(v);
        }
    }
    None
}

fn take<'a>(b: &mut &'a [u8], n: usize) -> Option<&'a [u8]> {
    let (head, rest) = b.split_at_checked(n)?;
    *b = rest;
    Some(head)
}

impl<'a> Iterator for Fields<'a> {
    type Item = (u32, Wire<'a>);

    fn next(&mut self) -> Option<Self::Item> {
        let b = &mut self.b;
        let key = varint(b)?;
        let value = match key & 7 {
            0 => Wire::Varint(varint(b)?),
            1 => Wire::Fixed64(u64::from_le_bytes(take(b, 8)?.try_into().unwrap())),
            2 => {
                let n = usize::try_from(varint(b)?).ok()?;
                Wire::Bytes(take(b, n)?)
            }
            5 => Wire::Fixed32(u32::from_le_bytes(take(b, 4)?.try_into().unwrap())),
            _ => {
                self.b = &[];
                return None;
            }
        };
        Some((u32::try_from(key >> 3).ok()?, value))
    }
}

/// Every length-delimited value of field `n`: strings, bytes and nested messages.
pub fn all(b: &[u8], n: u32) -> impl Iterator<Item = &[u8]> {
    fields(b).filter_map(move |(f, w)| match w {
        Wire::Bytes(v) if f == n => Some(v),
        _ => None,
    })
}

pub fn bytes(b: &[u8], n: u32) -> Option<&[u8]> {
    all(b, n).next()
}

pub fn number(b: &[u8], n: u32) -> Option<u64> {
    fields(b).find_map(|(f, w)| match w {
        Wire::Varint(v) if f == n => Some(v),
        _ => None,
    })
}

pub fn text(b: &[u8], n: u32) -> Option<String> {
    bytes(b, n).map(|s| String::from_utf8_lossy(s).into_owned())
}

/// The object a reference field names: a message whose field 1 is its identifier.
pub fn reference(b: &[u8], n: u32) -> Option<u64> {
    number(bytes(b, n)?, 1)
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// A message, as a test writes it.
    #[derive(Default, Clone)]
    pub struct Message(pub Vec<u8>);

    fn put_varint(out: &mut Vec<u8>, mut v: u64) {
        while v > 0x7f {
            out.push(v as u8 | 0x80);
            v >>= 7;
        }
        out.push(v as u8);
    }

    impl Message {
        pub fn number(mut self, field: u32, v: u64) -> Self {
            put_varint(&mut self.0, (field as u64) << 3);
            put_varint(&mut self.0, v);
            self
        }

        pub fn bytes(mut self, field: u32, v: &[u8]) -> Self {
            put_varint(&mut self.0, (field as u64) << 3 | 2);
            put_varint(&mut self.0, v.len() as u64);
            self.0.extend_from_slice(v);
            self
        }

        pub fn reference(self, field: u32, id: u64) -> Self {
            self.bytes(field, &Message::default().number(1, id).0)
        }
    }

    #[test]
    fn fields_by_number() {
        let m = Message::default().number(1, 300).bytes(2, b"hi").reference(3, 77).bytes(2, b"yo").0;
        let m = [&m[..], &[4 << 3 | 1, 1, 0, 0, 0, 0, 0, 0, 0, 5 << 3 | 5, 2, 0, 0, 0]].concat();
        let got: Vec<_> = fields(&m).collect();
        assert_eq!(got.len(), 6);
        assert_eq!((got[0], got[4], got[5]), ((1, Wire::Varint(300)), (4, Wire::Fixed64(1)), (5, Wire::Fixed32(2))));
        assert_eq!(number(&m, 1), Some(300));
        assert_eq!(number(&m, 2), None);
        assert_eq!(all(&m, 2).collect::<Vec<_>>(), [b"hi", b"yo"]);
        assert_eq!(text(&m, 2).as_deref(), Some("hi"));
        assert_eq!(reference(&m, 3), Some(77));
        assert_eq!(reference(&m, 2), None);
        assert_eq!(bytes(&m, 9), None);
    }

    #[test]
    fn damage_ends_the_fields() {
        // A length past the end, a group (wire type 3), a varint that never ends, a
        // fixed value cut short: what came before is kept.
        for tail in [&[2 << 3 | 2, 9, 1][..], &[2 << 3 | 3], &[0x80; 11], &[2 << 3 | 1, 1, 2], &[2 << 3 | 5, 1]] {
            let m = [&Message::default().number(1, 5).0[..], tail].concat();
            assert_eq!(fields(&m).collect::<Vec<_>>(), [(1, Wire::Varint(5))], "{tail:?}");
        }
        let mut b: &[u8] = &[0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0x01, 7];
        assert_eq!(varint(&mut b), Some(u64::MAX));
        assert_eq!(b, [7]);
    }
}
