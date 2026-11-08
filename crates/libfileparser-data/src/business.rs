//! Business messages. EDI (ANSI X12 and UN/EDIFACT) and HL7 v2 as a table of segments
//! over a tree of interchanges, messages and segments; OFX (1.x SGML and 2.x XML) and
//! QIF as tables of transactions; FIX as a table of messages; SWIFT MT as a table of
//! fields; XBRL as a table of facts.

use std::ops::Range;
use std::sync::{Arc, Mutex};

use libfileparser_core::{Cell, Column, Content, Document, FileSource, Limit, OwnedTable, ParseContext, ParseError, Result, Role, ValueKind};

use crate::lines::{self, Line, LineTable, RowLines, columns, num, text};
use crate::nodes::{self, NodeTable, child, format, local, named, value};
use crate::tree::{Builder, DataTree, LineCounter};

use ValueKind::{Number, Text};

/// Element columns at most in a segment table.
const MAX_ELEMENTS: usize = 32;

// ---------------------------------------------------------------- segments

/// One segment: its id, its elements, where it is.
struct Segment {
    id: String,
    elements: Vec<String>,
    bytes: Range<usize>,
}

/// A table of segments (`Segment`, then one column per element position) and a tree
/// grouping them by `opens`/`closes` (ISA…IEA, GS…GE, ST…SE).
fn segment_document(id: &'static str, name: &str, segs: Vec<Segment>, groups: &[(&str, &str)], src: &FileSource, language: &str, cx: &ParseContext) -> Result<(Document, Vec<usize>)> {
    cx.limits.check(Limit::Rows, segs.len() as u64)?;
    let bytes = src.bytes();
    let mut lines = LineCounter::new(bytes);
    let width = segs.iter().take(10_000).map(|s| s.elements.len()).max().unwrap_or(0).min(MAX_ELEMENTS);
    // The tree: group segments open a node, their closing segments end it.
    let mut b = Builder::new(cx);
    let mut stack: Vec<(u32, &str)> = Vec::new();
    let mut opened = Vec::new();
    let mut row_lines = Vec::with_capacity(segs.len());
    for (n, s) in segs.iter().enumerate() {
        let (a, z) = (lines.line(s.bytes.start), lines.line(s.bytes.end.saturating_sub(1).max(s.bytes.start)));
        row_lines.push(a as u64..z as u64 + 1);
        if let Some(&(_, close)) = groups.iter().find(|(open, _)| *open == s.id) {
            let parent = stack.last().map(|x| x.0);
            let id = b.add(parent, stack.len() + 1, s.id.clone(), s.elements.iter().take(3).cloned().collect::<Vec<_>>().join(" "), ValueKind::Group, a..z + 1)?;
            stack.push((id, close));
            opened.push(n);
            continue;
        }
        let parent = stack.last().map(|x| x.0);
        let leaf = b.add(parent, stack.len() + 1, s.id.clone(), s.elements.join(" | "), Text, a..z + 1)?;
        let _ = leaf;
        if let Some(at) = stack.iter().rposition(|x| x.1 == s.id) {
            for (id, _) in stack.drain(at..) {
                b.end(id, z);
            }
        }
    }
    let mut cols = vec![Column::new("Segment").with_kind(Text)];
    cols.extend((1..=width).map(|i| Column::new(format!("{i:02}")).with_kind(Text)));
    let rows = segs
        .iter()
        .map(|s| {
            let mut row = vec![Cell::Text(s.id.clone())];
            row.extend((0..width).map(|i| s.elements.get(i).map_or(Cell::Null, |e| text(e))));
            row
        })
        .collect();
    let tree = b.finish()?;
    let doc = Document::new(format(id, name))
        .with_facet(Role::Primary, "Segments", Content::Table(Arc::new(OwnedTable::new(cols, rows))))
        .with_facet(Role::Structure, "Tree", Content::Tree(Arc::new(tree)))
        .with_meta("Segments", segs.len())
        .with_links(RowLines(row_lines))
        .with_file_facets(src, Some(language));
    Ok((doc, opened))
}

/// Splits on `sep` except where `release` escapes it.
fn split_released(s: &str, sep: char, release: Option<char>) -> Vec<String> {
    let mut out = vec![String::new()];
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        if Some(c) == release {
            if let Some(n) = chars.next() {
                out.last_mut().expect("a part").push(n);
            }
        } else if c == sep {
            out.push(String::new());
        } else {
            out.last_mut().expect("a part").push(c);
        }
    }
    out
}

// ---------------------------------------------------------------- X12

pub(crate) fn x12(src: &FileSource, cx: &ParseContext) -> Result<Document> {
    let b = src.bytes();
    let text_all = String::from_utf8_lossy(b);
    let start = text_all.find("ISA").ok_or_else(|| ParseError::corrupt("X12: no ISA segment"))?;
    let isa = &text_all.as_bytes()[start..];
    if isa.len() < 106 {
        return Err(ParseError::corrupt("X12: ISA segment cut short"));
    }
    // ISA is fixed-width: its 4th byte separates elements, its 106th ends segments.
    let (elem, term) = (isa[3] as char, isa[105] as char);
    let mut segs = Vec::new();
    let mut at = start;
    for raw in text_all[start..].split(term) {
        let len = raw.len() + term.len_utf8();
        let trimmed = raw.trim_matches(['\r', '\n', ' ']);
        if !trimmed.is_empty() {
            let lead = raw.len() - raw.trim_start_matches(['\r', '\n', ' ']).len();
            let mut parts = trimmed.split(elem);
            let id = parts.next().unwrap_or("").to_string();
            segs.push(Segment { id, elements: parts.map(str::to_owned).collect(), bytes: at + lead..at + lead + trimmed.len() });
        }
        at += len;
    }
    let sets: Vec<String> = segs.iter().filter(|s| s.id == "ST").filter_map(|s| s.elements.first().cloned()).collect();
    let isa_fields = segs.first().map(|s| s.elements.clone()).unwrap_or_default();
    let (mut doc, _) = segment_document("x12", "ANSI X12", segs, &[("ISA", "IEA"), ("GS", "GE"), ("ST", "SE")], src, "edi", cx)?;
    if let (Some(from), Some(to)) = (isa_fields.get(5), isa_fields.get(7)) {
        doc = doc.with_meta("Sender", from.trim()).with_meta("Receiver", to.trim());
    }
    let mut kinds = sets.clone();
    kinds.dedup();
    if !kinds.is_empty() {
        doc = doc.with_meta("Transaction sets", kinds.join(", ")).with_meta("Transactions", sets.len());
    }
    Ok(doc)
}

// ---------------------------------------------------------------- EDIFACT

pub(crate) fn edifact(src: &FileSource, cx: &ParseContext) -> Result<Document> {
    let text_all = String::from_utf8_lossy(src.bytes());
    let t = text_all.trim_start_matches('\u{feff}');
    let offset = text_all.len() - t.len();
    // `UNA:+.? '`: component, element, decimal, release, reserved, terminator.
    let (comp, elem, release, term, body_at) = match t.strip_prefix("UNA") {
        Some(una) if una.chars().count() >= 6 => {
            let c: Vec<char> = una.chars().take(6).collect();
            let len: usize = c.iter().map(|ch| ch.len_utf8()).sum();
            (c[0], c[1], Some(c[3]).filter(|r| *r != ' '), c[5], 3 + len)
        }
        _ => (':', '+', Some('?'), '\'', 0),
    };
    let _ = comp;
    let mut segs = Vec::new();
    let body = &t[body_at..];
    let mut at = offset + body_at;
    let mut cur = String::new();
    let mut seg_start = at;
    let mut chars = body.char_indices().peekable();
    let push = |cur: &mut String, from: usize, to: usize, segs: &mut Vec<Segment>| {
        let s = cur.trim_matches(['\r', '\n', ' ']).to_string();
        cur.clear();
        if s.is_empty() {
            return;
        }
        let mut parts = split_released(&s, elem, release).into_iter();
        let id = parts.next().unwrap_or_default();
        segs.push(Segment { id, elements: parts.collect(), bytes: from..to });
    };
    while let Some((i, c)) = chars.next() {
        if Some(c) == release {
            cur.push(c);
            if let Some((_, n)) = chars.next() {
                cur.push(n);
            }
            continue;
        }
        if c == term {
            let end = offset + body_at + i;
            push(&mut cur, seg_start, end, &mut segs);
            seg_start = end + 1;
            // Line breaks between segments.
            while chars.peek().is_some_and(|(_, c)| matches!(c, '\r' | '\n')) {
                let (j, _) = chars.next().expect("peeked");
                seg_start = offset + body_at + j + 1;
            }
            continue;
        }
        cur.push(c);
        at = offset + body_at + i;
    }
    push(&mut cur, seg_start, at + 1, &mut segs);
    if segs.is_empty() {
        return Err(ParseError::corrupt("EDIFACT: no segments"));
    }
    let types: Vec<String> = segs.iter().filter(|s| s.id == "UNH").filter_map(|s| s.elements.get(1)).map(|t| t.split(':').next().unwrap_or(t).to_string()).collect();
    let (mut doc, _) = segment_document("edifact", "UN/EDIFACT", segs, &[("UNB", "UNZ"), ("UNG", "UNE"), ("UNH", "UNT")], src, "edi", cx)?;
    if !types.is_empty() {
        let mut kinds = types.clone();
        kinds.dedup();
        doc = doc.with_meta("Message types", kinds.join(", ")).with_meta("Messages", types.len());
    }
    Ok(doc)
}

// ---------------------------------------------------------------- HL7

pub(crate) fn hl7(src: &FileSource, cx: &ParseContext) -> Result<Document> {
    let t = String::from_utf8_lossy(src.bytes());
    let msh = t.find("MSH").ok_or_else(|| ParseError::corrupt("HL7: no MSH segment"))?;
    let field = t[msh + 3..].chars().next().ok_or_else(|| ParseError::corrupt("HL7: MSH cut short"))?;
    let mut segs = Vec::new();
    let mut at = 0;
    // Segments end with a carriage return (or a line break, as files are often saved).
    for raw in t.split_inclusive(['\r', '\n']) {
        let line = raw.trim_end_matches(['\r', '\n']);
        if line.len() >= 3 {
            let mut parts: Vec<String> = line.split(field).map(str::to_owned).collect();
            let id = parts.remove(0);
            // MSH-1 is the field separator itself.
            if id == "MSH" {
                parts.insert(0, field.to_string());
            }
            segs.push(Segment { id, elements: parts, bytes: at..at + line.len() });
        }
        at += raw.len();
    }
    let first_msh = segs.iter().find(|s| s.id == "MSH").map(|s| s.elements.clone()).unwrap_or_default();
    let messages = segs.iter().filter(|s| s.id == "MSH").count();
    // A message runs from its MSH to the next.
    let (mut doc, _) = segment_document("hl7", "HL7 v2", segs, &[("MSH", "MSH")], src, "hl7", cx)?;
    let get = |i: usize| first_msh.get(i - 1).filter(|v| !v.is_empty());
    if let Some(t) = get(9) {
        doc = doc.with_meta("Message type", t.replace('^', " "));
    }
    if let Some(v) = get(12) {
        doc = doc.with_meta("Version", v);
    }
    if let Some(a) = get(3) {
        doc = doc.with_meta("Sending application", a.replace('^', " "));
    }
    Ok(doc.with_meta("Messages", messages))
}

// ---------------------------------------------------------------- OFX

/// OFX dates, `20241108120000.000[-5:EST]`, as `2024-11-08 12:00:00`.
fn ofx_date(v: &str) -> Cell {
    let d: String = v.chars().take_while(char::is_ascii_digit).collect();
    match d.len() {
        8.. if d.len() >= 14 => Cell::Date(format!("{}-{}-{} {}:{}:{}", &d[..4], &d[4..6], &d[6..8], &d[8..10], &d[10..12], &d[12..14])),
        8.. => Cell::Date(format!("{}-{}-{}", &d[..4], &d[4..6], &d[6..8])),
        _ => text(v),
    }
}

pub(crate) fn ofx(src: &FileSource, cx: &ParseContext) -> Result<Document> {
    let b = src.bytes();
    // 2.x is XML; 1.x is SGML with unclosed leaf tags.
    let xml = memchr::memmem::find(&b[..b.len().min(1024)], b"<?xml").is_some();
    // Some OFX 2 files leave their leaf tags unclosed, as 1.x does: read those leniently.
    let tree = if xml { crate::xml::parse(b, cx).or_else(|_| crate::markup::parse(b, cx, true))? } else { crate::markup::parse(b, cx, true)? };
    let root = tree.roots().iter().copied().find(|&r| tree.label(r).eq_ignore_ascii_case("OFX")).ok_or_else(|| ParseError::corrupt("OFX: no <OFX>"))?;
    let trns = named(&tree, root, "STMTTRN");
    let currency = named(&tree, root, "CURDEF").first().map(|&c| tree.value(c).to_string());
    let balance = named(&tree, root, "LEDGERBAL").first().and_then(|&l| child(&tree, l, "BALAMT")).map(|a| tree.value(a).to_string());
    let tree = Arc::new(tree);
    let get = |t: &DataTree, row: u32, name: &str| child(t, row, name).map(|c| t.value(c).trim().to_string()).filter(|v| !v.is_empty());
    let table = NodeTable::new(tree, trns.into())
        .column("Date", ValueKind::Date, move |t, row| get(t, row, "DTPOSTED").map_or(Cell::Null, |d| ofx_date(&d)))
        .column("Amount", Number, move |t, row| get(t, row, "TRNAMT").map_or(Cell::Null, |a| num(&a.replace(',', "."))))
        .column("Name", Text, move |t, row| get(t, row, "NAME").or_else(|| child(t, row, "PAYEE").and_then(|p| get(t, p, "NAME"))).map_or(Cell::Null, |n| text(&n)))
        .column("Memo", Text, move |t, row| get(t, row, "MEMO").map_or(Cell::Null, |m| text(&m)))
        .column("Type", Text, move |t, row| get(t, row, "TRNTYPE").map_or(Cell::Null, |m| text(&m)))
        .column("Check", Text, move |t, row| get(t, row, "CHECKNUM").map_or(Cell::Null, |m| text(&m)))
        .column("ID", Text, move |t, row| get(t, row, "FITID").map_or(Cell::Null, |m| text(&m)))
        .column("Account", Text, move |t, row| {
            // The statement it is in names the account.
            let mut at = t.parent(row);
            while let Some(p) = at {
                if let Some(acct) = ["BANKACCTFROM", "CCACCTFROM", "INVACCTFROM"].iter().find_map(|k| child(t, p, k)) {
                    return get(t, acct, "ACCTID").map_or(Cell::Null, |a| text(&a));
                }
                at = t.parent(p);
            }
            Cell::Null
        });
    let mut doc = nodes::document(format("ofx", if xml { "OFX 2" } else { "OFX 1" }), "Transactions", table, src, Some(if xml { "xml" } else { "sgml" }), true);
    if let Some(c) = currency {
        doc = doc.with_meta("Currency", c);
    }
    if let Some(b) = balance {
        doc = doc.with_meta("Balance", b);
    }
    Ok(doc)
}

// ---------------------------------------------------------------- QIF

pub(crate) fn qif(src: &FileSource, cx: &ParseContext) -> Result<Document> {
    // Records end with `^`; `!Type:Bank` names the account type of those that follow.
    let kinds = Arc::new(Mutex::new(Vec::<String>::new()));
    let noting = kinds.clone();
    let mut section = String::new();
    let mut expecting = true;
    let index = lines::index(src, "qif", cx, move |_, line| {
        let l = line.trim();
        if let Some(t) = l.strip_prefix("!Type:").or_else(|| l.strip_prefix("!type:")) {
            section = t.trim().to_string();
            expecting = true;
            return Line::Within;
        }
        if l.starts_with('!') || l.is_empty() {
            return Line::Within;
        }
        if l == "^" {
            expecting = true;
            return Line::Within;
        }
        if expecting {
            expecting = false;
            noting.lock().unwrap_or_else(|e| e.into_inner()).push(section.clone());
            Line::Start
        } else {
            Line::Within
        }
    })?;
    let kinds = Arc::new(std::mem::take(&mut *kinds.lock().unwrap_or_else(|e| e.into_inner())));
    let cols = [("Date", ValueKind::Date), ("Amount", Number), ("Payee", Text), ("Memo", Text), ("Category", Text), ("Number", Text), ("Cleared", Text), ("Splits", Number), ("Account type", Text)];
    let table = LineTable::new(index, columns(&cols), move |row, lines| {
        let mut out = vec![Cell::Null; 9];
        let mut splits = 0;
        for l in lines.iter().map(|l| l.trim_end()) {
            if l == "^" {
                break;
            }
            let mut chars = l.chars();
            let code = chars.next().map(String::from).unwrap_or_default();
            let v = chars.as_str();
            match code.as_str() {
                "D" => out[0] = Cell::Date(v.trim().to_string()),
                "T" | "U" => out[1] = num(&v.replace(',', "")),
                "P" => out[2] = text(v),
                "M" => out[3] = text(v),
                "L" => out[4] = text(v),
                "N" => out[5] = text(v),
                "C" => out[6] = text(v),
                "S" => splits += 1,
                _ => {}
            }
        }
        out[7] = if splits > 0 { Cell::Int(splits) } else { Cell::Null };
        out[8] = kinds.get(row).map_or(Cell::Null, |k| text(k));
        out
    });
    Ok(lines::document("qif", "Quicken Interchange Format", "Transactions", table, src, "qif"))
}

// ---------------------------------------------------------------- FIX

/// Names of the FIX tags most messages carry.
fn fix_tag(tag: &str) -> Option<&'static str> {
    Some(match tag {
        "8" => "BeginString",
        "9" => "BodyLength",
        "10" => "CheckSum",
        "11" => "ClOrdID",
        "14" => "CumQty",
        "17" => "ExecID",
        "31" => "LastPx",
        "32" => "LastQty",
        "34" => "MsgSeqNum",
        "35" => "MsgType",
        "37" => "OrderID",
        "38" => "OrderQty",
        "39" => "OrdStatus",
        "40" => "OrdType",
        "44" => "Price",
        "49" => "SenderCompID",
        "52" => "SendingTime",
        "54" => "Side",
        "55" => "Symbol",
        "56" => "TargetCompID",
        "58" => "Text",
        "59" => "TimeInForce",
        "60" => "TransactTime",
        "98" => "EncryptMethod",
        "108" => "HeartBtInt",
        "150" => "ExecType",
        "151" => "LeavesQty",
        _ => return None,
    })
}

fn fix_msg_type(t: &str) -> String {
    let name = match t {
        "0" => "Heartbeat",
        "1" => "TestRequest",
        "2" => "ResendRequest",
        "3" => "Reject",
        "4" => "SequenceReset",
        "5" => "Logout",
        "8" => "ExecutionReport",
        "9" => "OrderCancelReject",
        "A" => "Logon",
        "D" => "NewOrderSingle",
        "F" => "OrderCancelRequest",
        "G" => "OrderCancelReplaceRequest",
        "V" => "MarketDataRequest",
        "W" => "MarketDataSnapshot",
        "X" => "MarketDataIncrementalRefresh",
        "j" => "BusinessMessageReject",
        _ => return t.to_string(),
    };
    format!("{name} ({t})")
}

pub(crate) fn fix(src: &FileSource, cx: &ParseContext) -> Result<Document> {
    let t = String::from_utf8_lossy(src.bytes());
    // Fields end with SOH, or `|` as logs print it.
    let sep = if t.contains('\u{1}') { '\u{1}' } else { '|' };
    let mut b = Builder::new(cx);
    let mut lines = LineCounter::new(t.as_bytes());
    let mut msgs: Vec<(u32, Range<u64>)> = Vec::new();
    let mut at = 0;
    while let Some(p) = t[at..].find("8=FIX") {
        let start = at + p;
        // To the checksum field and its separator.
        let end = t[start..].find(&format!("{sep}10=")).map(|q| start + q + 1).and_then(|c| t[c..].find(sep).map(|e| c + e + 1).or(Some(t.len()))).unwrap_or(t.len());
        let body = &t[start..end];
        let (a, z) = (lines.line(start), lines.line(end.saturating_sub(1)));
        let fields: Vec<(&str, &str)> = body.split(sep).filter_map(|f| f.split_once('=')).collect();
        let msg_type = fields.iter().find(|(k, _)| *k == "35").map_or(String::new(), |(_, v)| fix_msg_type(v));
        let id = b.add(None, 1, msg_type, format!("{} fields", fields.len()), ValueKind::Group, a..z + 1)?;
        for (k, v) in &fields {
            let label = fix_tag(k).map_or_else(|| k.to_string(), |n| format!("{n} ({k})"));
            b.add(Some(id), 2, label, *v, Text, a..z + 1)?;
        }
        msgs.push((id, a as u64..z as u64 + 1));
        cx.limits.check(Limit::Rows, msgs.len() as u64)?;
        at = end.max(start + 5);
    }
    if msgs.is_empty() {
        return Err(ParseError::corrupt("FIX: no 8=FIX message"));
    }
    let tree = Arc::new(b.finish()?);
    let tag = |t: &DataTree, row: u32, tag: &str| t.child_ids(row).iter().copied().find(|&c| t.label(c).ends_with(&format!("({tag})")) || t.label(c) == tag).map(|c| t.value(c).to_string());
    let rows: Vec<u32> = msgs.iter().map(|m| m.0).collect();
    let table = NodeTable::new(tree, rows.into())
        .column("Type", Text, |t, row| Cell::Text(t.label(row).to_string()))
        .column("Sequence", Number, move |t, row| tag(t, row, "34").map_or(Cell::Null, |v| num(&v)))
        .column("Sent", ValueKind::Date, move |t, row| tag(t, row, "52").map_or(Cell::Null, Cell::Date))
        .column("Sender", Text, move |t, row| tag(t, row, "49").map_or(Cell::Null, |v| text(&v)))
        .column("Target", Text, move |t, row| tag(t, row, "56").map_or(Cell::Null, |v| text(&v)))
        .column("Symbol", Text, move |t, row| tag(t, row, "55").map_or(Cell::Null, |v| text(&v)))
        .column("Side", Text, move |t, row| {
            tag(t, row, "54").map_or(Cell::Null, |v| Cell::Text(match v.as_str() {
                "1" => "Buy".into(),
                "2" => "Sell".into(),
                "5" => "Sell short".into(),
                o => o.into(),
            }))
        })
        .column("Quantity", Number, move |t, row| tag(t, row, "38").map_or(Cell::Null, |v| num(&v)))
        .column("Price", Number, move |t, row| tag(t, row, "44").map_or(Cell::Null, |v| num(&v)));
    let n = msgs.len();
    Ok(nodes::document(format("fix", "FIX messages"), "Messages", table, src, Some("fix"), true).with_meta("Messages", n))
}

// ---------------------------------------------------------------- SWIFT MT

fn mt_field(tag: &str) -> &'static str {
    match tag {
        "20" => "Transaction reference",
        "21" => "Related reference",
        "23B" => "Bank operation code",
        "25" | "25A" => "Account",
        "28C" => "Statement number",
        "32A" => "Value date, currency, amount",
        "33B" => "Currency, instructed amount",
        "50A" | "50F" | "50K" => "Ordering customer",
        "52A" | "52D" => "Ordering institution",
        "53A" | "53B" => "Sender's correspondent",
        "56A" => "Intermediary",
        "57A" | "57D" => "Account with institution",
        "59" | "59A" | "59F" => "Beneficiary",
        "60F" | "60M" => "Opening balance",
        "61" => "Statement line",
        "62F" | "62M" => "Closing balance",
        "64" => "Closing available balance",
        "70" => "Remittance information",
        "71A" => "Details of charges",
        "72" => "Sender to receiver information",
        "86" => "Information to account owner",
        _ => "",
    }
}

/// The `:tag:value` fields of `t[range]`, a value running over the lines to the next
/// tag, into `rows` as message `message`. Without the SWIFT envelope (an MT940
/// statement as banks export them), each `:20:` starts the next message.
#[allow(clippy::too_many_arguments)]
fn mt_fields(t: &str, range: Range<usize>, mut message: i64, bare: bool, lines: &mut LineCounter, rows: &mut Vec<Vec<Cell>>, row_lines: &mut Vec<Range<u64>>) -> i64 {
    let mut offset = range.start;
    let mut current: Option<(String, String, usize, i64)> = None;
    let mut flush = |cur: &mut Option<(String, String, usize, i64)>, end: usize, rows: &mut Vec<Vec<Cell>>, row_lines: &mut Vec<Range<u64>>| {
        if let Some((tag, value, from, m)) = cur.take() {
            let (a, z) = (lines.line(from), lines.line(end.saturating_sub(1).max(from)));
            let name = mt_field(&tag);
            rows.push(vec![Cell::Int(m), Cell::Text(tag), if name.is_empty() { Cell::Null } else { Cell::Text(name.into()) }, Cell::Text(value.trim_end().to_string())]);
            row_lines.push(a as u64..z as u64 + 1);
        }
    };
    for line in t[range.clone()].split_inclusive('\n') {
        let l = line.trim_end_matches(['\r', '\n']);
        if let Some(rest) = l.strip_prefix(':')
            && let Some((tag, value)) = rest.split_once(':')
            && (2..=4).contains(&tag.len())
            && tag.starts_with(|c: char| c.is_ascii_digit())
        {
            flush(&mut current, offset, rows, row_lines);
            if bare && tag == "20" {
                message += 1;
            }
            current = Some((tag.to_string(), value.to_string(), offset, message.max(1)));
        } else if l.trim() == "-" {
            // The end of a statement.
            flush(&mut current, offset, rows, row_lines);
        } else if let Some((_, v, _, _)) = current.as_mut()
            && !l.is_empty()
        {
            v.push('\n');
            v.push_str(l);
        }
        offset += line.len();
    }
    flush(&mut current, range.end, rows, row_lines);
    message
}

/// Whether text is an MT message without its envelope: a `:tag:` field among its
/// first lines, as statements exported by banks start (with `:20:`).
pub(crate) fn looks_like_bare_mt(header: &[u8]) -> bool {
    let text = String::from_utf8_lossy(header);
    // A field line: `:20:`, `:60F:`, `:86:`.
    let field = |l: &str| {
        let Some(rest) = l.strip_prefix(':') else { return false };
        let tag = rest.split(':').next().unwrap_or("");
        rest.len() > tag.len() && (2..=4).contains(&tag.len()) && tag[..2].bytes().all(|c| c.is_ascii_digit()) && tag[2..].bytes().all(|c| c.is_ascii_uppercase())
    };
    let mut lines = text.lines().map(str::trim).filter(|l| !l.is_empty()).take(6);
    lines.any(field)
}

pub(crate) fn swift(src: &FileSource, cx: &ParseContext) -> Result<Document> {
    let t = String::from_utf8_lossy(src.bytes());
    let mut rows = Vec::new();
    let mut row_lines = Vec::new();
    let mut lines = LineCounter::new(t.as_bytes());
    let mut types = Vec::new();
    let mut message = 0i64;
    let mut at = 0;
    while let Some(p) = t[at..].find("{1:") {
        message += 1;
        let start = at + p;
        let next = t[start + 3..].find("{1:").map_or(t.len(), |q| start + 3 + q);
        let msg = &t[start..next];
        // Block 2: `I103BANKDEFFXXXXN` (input) or `O103…` (output).
        if let Some(b2) = msg.find("{2:") {
            let kind = msg[b2 + 3..].chars().skip(1).take(3).collect::<String>();
            types.push(format!("MT{kind}"));
        }
        // Block 4: `:tag:value` lines to `-}`.
        if let Some(b4) = msg.find("{4:") {
            let body_start = start + b4 + 3;
            let body_end = msg[b4..].find("\n-}").or_else(|| msg[b4..].find("-}")).map_or(next, |e| start + b4 + e);
            mt_fields(&t, body_start..body_end, message, false, &mut lines, &mut rows, &mut row_lines);
        }
        cx.limits.check(Limit::Rows, rows.len() as u64)?;
        at = next;
    }
    let bare = message == 0;
    if bare {
        // No envelope: the fields alone, statement after statement.
        if !looks_like_bare_mt(src.header()) {
            return Err(ParseError::corrupt("SWIFT: no {1: block or :20: field"));
        }
        message = mt_fields(&t, 0..t.len(), 0, true, &mut lines, &mut rows, &mut row_lines).max(1);
        cx.limits.check(Limit::Rows, rows.len() as u64)?;
        types.push("MT940".into());
    }
    let n = rows.len();
    let table = OwnedTable::new(columns(&[("Message", Number), ("Tag", Text), ("Field", Text), ("Value", Text)]), rows);
    let mut kinds = types.clone();
    kinds.dedup();
    Ok(Document::new(format("swift", if bare { "SWIFT MT940 statement" } else { "SWIFT MT" }))
        .with_facet(Role::Primary, "Fields", Content::Table(Arc::new(table)))
        .with_meta("Messages", message)
        .with_meta("Message types", kinds.join(", "))
        .with_meta("Fields", n)
        .with_links(RowLines(row_lines))
        .with_file_facets(src, Some("swift")))
}

// ---------------------------------------------------------------- XBRL

pub(crate) fn xbrl(src: &FileSource, cx: &ParseContext) -> Result<Document> {
    let tree = crate::xml::parse(src.bytes(), cx)?;
    let root = tree.roots().first().copied().ok_or_else(|| ParseError::corrupt("XBRL: empty"))?;
    // Facts: elements with a context.
    let facts: Vec<u32> = tree.all().into_iter().filter(|&n| tree.child(n, "@contextRef").is_some()).collect();
    // Contexts' periods, and units' measures, by id.
    let periods: Arc<Vec<(String, String)>> = Arc::new(
        named(&tree, root, "context")
            .into_iter()
            .filter_map(|c| {
                let id = tree.child(c, "@id").map(|i| tree.value(i).to_string())?;
                let period = child(&tree, c, "period")?;
                let get = |n: &str| child(&tree, period, n).map(|v| tree.value(v).to_string());
                let p = match (get("instant"), get("startDate"), get("endDate")) {
                    (Some(i), ..) => i,
                    (None, Some(s), Some(e)) => format!("{s} to {e}"),
                    _ => "forever".into(),
                };
                Some((id, p))
            })
            .collect(),
    );
    let units: Arc<Vec<(String, String)>> = Arc::new(
        named(&tree, root, "unit")
            .into_iter()
            .filter_map(|u| {
                let id = tree.child(u, "@id").map(|i| tree.value(i).to_string())?;
                let measures: Vec<String> = named(&tree, u, "measure").into_iter().map(|m| tree.value(m).to_string()).collect();
                Some((id, measures.join(" / ")))
            })
            .collect(),
    );
    let tree = Arc::new(tree);
    let lookup = |list: &[(String, String)], id: &str| list.iter().find(|(k, _)| k == id).map(|(_, v)| v.clone());
    let table = NodeTable::new(tree, facts.into())
        .column("Concept", Text, |t, row| Cell::Text(t.label(row).to_string()))
        .column("Value", ValueKind::Mixed, |t, row| {
            let v = t.value(row).trim();
            match num(v) {
                Cell::Null => text(v),
                n => n,
            }
        })
        .column("Period", Text, move |t, row| t.child(row, "@contextRef").and_then(|c| lookup(&periods, t.value(c))).map_or(Cell::Null, Cell::Text))
        .column("Unit", Text, move |t, row| t.child(row, "@unitRef").map(|u| lookup(&units, t.value(u)).unwrap_or_else(|| t.value(u).to_string())).map_or(Cell::Null, Cell::Text))
        .column("Decimals", Text, |t, row| value(t, t.child(row, "@decimals")))
        .column("Context", Text, |t, row| value(t, t.child(row, "@contextRef")));
    let _ = local;
    Ok(nodes::document(format("xbrl", "XBRL"), "Facts", table, src, Some("xml"), true))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn released_separators_and_dates() {
        assert_eq!(split_released("a+b?+c+d", '+', Some('?')), ["a", "b+c", "d"]);
        assert_eq!(ofx_date("20241108120000.000[-5:EST]"), Cell::Date("2024-11-08 12:00:00".into()));
        assert_eq!(ofx_date("20241108"), Cell::Date("2024-11-08".into()));
        assert_eq!(fix_msg_type("D"), "NewOrderSingle (D)");
    }
}
