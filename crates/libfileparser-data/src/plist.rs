//! Apple property lists, XML and binary.

use libfileparser_core::{ParseContext, ParseError, Result};
use plist::Value;

use crate::tree::{Builder, DataTree};

pub(crate) fn parse(bytes: &[u8], cx: &ParseContext) -> Result<DataTree> {
    let value = Value::from_reader(std::io::Cursor::new(bytes))
        .map_err(|e| ParseError::corrupt(format!("property list: {e}")))?;
    let mut b = Builder::new(cx);
    match &value {
        Value::Dictionary(d) => {
            for (k, v) in d {
                add(&mut b, None, 1, k.clone(), v)?;
            }
        }
        Value::Array(items) => {
            for (i, v) in items.iter().enumerate() {
                add(&mut b, None, 1, i.to_string(), v)?;
            }
        }
        other => {
            add(&mut b, None, 1, "value".into(), other)?;
        }
    }
    b.finish()
}

fn add(b: &mut Builder, parent: Option<u32>, depth: usize, label: String, v: &Value) -> Result<()> {
    let shown = match v {
        Value::Dictionary(d) => format!("{{{}}}", d.len()),
        Value::Array(a) => format!("[{}]", a.len()),
        Value::String(s) => format!("{s:?}"),
        Value::Boolean(v) => v.to_string(),
        Value::Integer(i) => i.to_string(),
        Value::Real(r) => r.to_string(),
        Value::Date(d) => d.to_xml_format(),
        Value::Data(d) => format!("{} bytes", d.len()),
        Value::Uid(u) => format!("UID {}", u.get()),
        _ => String::new(),
    };
    // Binary plists have no lines.
    let id = b.add(parent, depth, label, shown, 0..0)?;
    match v {
        Value::Dictionary(d) => {
            for (k, v) in d {
                add(b, Some(id), depth + 1, k.clone(), v)?;
            }
        }
        Value::Array(items) => {
            for (i, v) in items.iter().enumerate() {
                add(b, Some(id), depth + 1, i.to_string(), v)?;
            }
        }
        _ => {}
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use libfileparser_core::TreeSource;

    #[test]
    fn xml_plist() {
        let cx = ParseContext::new();
        let xml = br#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict><key>Name</key><string>App</string><key>Tags</key><array><integer>1</integer></array></dict></plist>"#;
        let t = parse(xml, &cx).unwrap();
        let top: Vec<_> = t
            .children(None, 0..10)
            .unwrap()
            .into_iter()
            .map(|n| (n.label, n.value))
            .collect();
        assert_eq!(
            top,
            [("Name", "\"App\""), ("Tags", "[1]")].map(|(a, b)| (a.to_string(), b.to_string()))
        );
    }
}
