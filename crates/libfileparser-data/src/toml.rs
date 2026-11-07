//! TOML 1.1, keys in the order written, each value on the lines it was written on.

use libfileparser_core::{ParseContext, ParseError, Result};
use toml::de::{DeTable, DeValue};

use crate::tree::{Builder, DataTree, LineCounter};

pub(crate) fn parse(text: &str, cx: &ParseContext) -> Result<DataTree> {
    let table = DeTable::parse(text)
        .map_err(|e| ParseError::corrupt(format!("TOML: {}", e.to_string().trim_end())))?;
    let mut b = Builder::new(cx);
    let mut lines = LineCounter::new(text.as_bytes());
    members(&mut b, &mut lines, None, 1, table.get_ref())?;
    b.finish()
}

fn members(
    b: &mut Builder,
    lines: &mut LineCounter,
    parent: Option<u32>,
    depth: usize,
    table: &DeTable,
) -> Result<()> {
    for (key, value) in table.iter() {
        let span = key.span();
        let vspan = value.span();
        let (start, end) = (span.start.min(vspan.start), span.end.max(vspan.end));
        add(
            b,
            lines,
            parent,
            depth,
            key.get_ref().to_string(),
            value.get_ref(),
            start..end,
        )?;
    }
    Ok(())
}

fn add(
    b: &mut Builder,
    lines: &mut LineCounter,
    parent: Option<u32>,
    depth: usize,
    label: String,
    value: &DeValue,
    span: std::ops::Range<usize>,
) -> Result<u32> {
    let first = lines.line(span.start);
    let last = lines.line(span.end.saturating_sub(1).max(span.start));
    let shown = match value {
        DeValue::String(s) => format!("{s:?}"),
        DeValue::Integer(i) => i.to_string(),
        DeValue::Float(f) => f.to_string(),
        DeValue::Boolean(v) => v.to_string(),
        DeValue::Datetime(d) => d.to_string(),
        DeValue::Array(a) => format!("[{}]", a.len()),
        DeValue::Table(t) => format!("{{{}}}", t.len()),
    };
    let id = b.add(parent, depth, label, shown, first..last + 1)?;
    match value {
        DeValue::Array(items) => {
            for (i, item) in items.iter().enumerate() {
                add(
                    b,
                    lines,
                    Some(id),
                    depth + 1,
                    i.to_string(),
                    item.get_ref(),
                    item.span(),
                )?;
            }
        }
        DeValue::Table(t) => members(b, lines, Some(id), depth + 1, t)?,
        _ => {}
    }
    Ok(id)
}

#[cfg(test)]
mod tests {
    use super::*;
    use libfileparser_core::TreeSource;

    #[test]
    fn tables_in_order_with_lines() {
        let cx = ParseContext::new();
        let t = parse(
            "title = \"x\"\n[pkg]\nname = 'a'\nlist = [1, 0x10]\n[[bin]]\npath = \"b\"\n",
            &cx,
        )
        .unwrap();
        let top: Vec<_> = t
            .children(None, 0..10)
            .unwrap()
            .into_iter()
            .map(|n| (n.label, n.value))
            .collect();
        assert_eq!(
            top,
            [("title", "\"x\""), ("pkg", "{2}"), ("bin", "[1]")]
                .map(|(a, b)| (a.to_string(), b.to_string()))
        );
        let list = t.find("/pkg/list/1").unwrap().unwrap();
        assert_eq!(t.lines(list.0 as u32), 3..4);
        assert!(parse("a = \n", &cx).is_err());
    }
}
