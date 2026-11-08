//! The zip package and XML helpers the reader needs.

use std::collections::HashMap;
use std::io::{Cursor, Read};

use quick_xml::events::BytesStart;

use crate::{Error, Limits};

pub(crate) struct Package<'a> {
    bytes: &'a [u8],
    names: HashMap<String, usize>,
    limits: Limits,
}

impl<'a> Package<'a> {
    pub fn open(bytes: &'a [u8], limits: &Limits) -> Result<Self, Error> {
        let z = zip::ZipArchive::new(Cursor::new(bytes)).map_err(|e| Error::Zip(e.to_string()))?;
        if z.len() > limits.entries {
            return Err(Error::TooLarge { what: "entries", value: z.len() as u64, max: limits.entries as u64 });
        }
        let names = (0..z.len()).filter_map(|i| z.name_for_index(i).and_then(|n| n.ok()).map(|n| (n.to_string(), i))).collect();
        Ok(Self { bytes, names, limits: limits.clone() })
    }

    pub fn has(&self, name: &str) -> bool {
        self.names.contains_key(name.trim_start_matches('/'))
    }

    /// Part `name` as text; `None` when there's no such part.
    pub fn text(&self, name: &str) -> Result<Option<String>, Error> {
        let name = name.trim_start_matches('/');
        let Some(&i) = self.names.get(name) else { return Ok(None) };
        let mut z = zip::ZipArchive::new(Cursor::new(self.bytes)).map_err(|e| Error::Zip(e.to_string()))?;
        let f = z.by_index(i).map_err(|e| Error::Zip(format!("{name}: {e}")))?;
        let (packed, size) = (f.compressed_size(), f.size());
        let cap = self.limits.part_bytes.min(packed.saturating_mul(self.limits.unpack_ratio).max(self.limits.unpack_always));
        if size > cap {
            return Err(Error::TooLarge { what: "part", value: size, max: cap });
        }
        let mut data = Vec::with_capacity(size as usize);
        f.take(size).read_to_end(&mut data).map_err(|e| Error::Zip(format!("{name}: {e}")))?;
        Ok(Some(String::from_utf8_lossy(&data).into_owned()))
    }
}

/// `path` relative to the folder of part `base`, `..` and `.` resolved; URL escapes
/// undone, a fragment dropped.
pub(crate) fn resolve(base: &str, path: &str) -> String {
    let path = path.split('#').next().unwrap_or("");
    let path = percent_decode(path);
    let mut parts: Vec<&str> = if path.starts_with('/') { Vec::new() } else { base.rsplit_once('/').map_or(Vec::new(), |(d, _)| d.split('/').collect()) };
    for p in path.split('/') {
        match p {
            "" | "." => {}
            ".." => {
                parts.pop();
            }
            p => parts.push(p),
        }
    }
    parts.join("/")
}

fn percent_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%'
            && let Some(v) = b.get(i + 1..i + 3).and_then(|h| std::str::from_utf8(h).ok()).and_then(|h| u8::from_str_radix(h, 16).ok())
        {
            out.push(v);
            i += 3;
        } else {
            out.push(b[i]);
            i += 1;
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// An element's name without its namespace prefix.
pub(crate) fn local(e: &BytesStart) -> String {
    String::from_utf8_lossy(e.name().local_name().as_ref()).into_owned()
}

/// Attribute `name` (matched without prefix, or with exactly `prefixed`), unescaped.
pub(crate) fn attr(e: &BytesStart, name: &str) -> Option<String> {
    e.attributes().flatten().find_map(|a| {
        let full = String::from_utf8_lossy(a.key.as_ref()).into_owned();
        let local = String::from_utf8_lossy(a.key.local_name().as_ref()).into_owned();
        (local == name || full == name).then(|| unescape(&String::from_utf8_lossy(&a.value)))
    })
}

/// XML's entities and character references.
pub(crate) fn unescape(s: &str) -> String {
    if !s.contains('&') {
        return s.to_string();
    }
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(at) = rest.find('&') {
        out.push_str(&rest[..at]);
        rest = &rest[at..];
        let Some(end) = rest.bytes().take(12).position(|c| c == b';') else {
            out.push('&');
            rest = &rest[1..];
            continue;
        };
        let name = &rest[1..end];
        let c = if let Some(num) = name.strip_prefix("#x").or_else(|| name.strip_prefix("#X")) {
            u32::from_str_radix(num, 16).ok().and_then(char::from_u32)
        } else if let Some(num) = name.strip_prefix('#') {
            num.parse().ok().and_then(char::from_u32)
        } else {
            match name {
                "amp" => Some('&'),
                "lt" => Some('<'),
                "gt" => Some('>'),
                "quot" => Some('"'),
                "apos" => Some('\''),
                _ => None,
            }
        };
        match c {
            Some(c) => {
                out.push(c);
                rest = &rest[end + 1..];
            }
            None => {
                out.push('&');
                rest = &rest[1..];
            }
        }
    }
    out.push_str(rest);
    out
}
