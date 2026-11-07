//! Scientific records. Sequences (FASTA, FASTQ, GenBank), annotations and variants
//! (GFF3, GTF, BED, VCF), alignments (SAM), structures (PDB atoms, mmCIF/CIF's
//! categories and loops), machine-learning data (ARFF) and problems (DIMACS CNF and
//! graphs), each a table of its records. Line formats are read by range, so a
//! genome-sized file opens at the cost of its record index.

use std::sync::{Arc, Mutex};

use libfileparser_core::{Cell, Column, Content, Document, FacetInfo, FileSource, Limit, Model, OwnedTable, ParseContext, ParseError, Result, Role, ValueKind};

use crate::lines::{self, Line, LineTable, columns, num, text};
use crate::nodes::format;
use crate::tree::Builder;

use ValueKind::{Number, Text};

/// Characters of a sequence shown in its row.
const PREVIEW: usize = 80;

fn preview(seq: &str) -> Cell {
    if seq.chars().count() > PREVIEW { Cell::Text(format!("{}…", seq.chars().take(PREVIEW).collect::<String>())) } else { text(seq) }
}

/// Fields split on tabs.
fn tabs(line: &str) -> Vec<&str> {
    line.trim_end_matches('\r').split('\t').collect()
}

// ---------------------------------------------------------------- FASTA

pub(crate) fn fasta(src: &FileSource, cx: &ParseContext) -> Result<Document> {
    // Lengths are counted while indexing: a record is read only as far as its preview.
    let lengths = Arc::new(Mutex::new(Vec::<u64>::new()));
    let counting = lengths.clone();
    let index = lines::index(src, "fasta", cx, move |_, line| {
        let mut l = counting.lock().unwrap_or_else(|e| e.into_inner());
        if line.starts_with('>') {
            l.push(0);
            Line::Start
        } else {
            if !line.starts_with(';')
                && let Some(last) = l.last_mut()
            {
                *last += line.trim_end().len() as u64;
            }
            Line::Within
        }
    })?;
    if index.starts.is_empty() {
        return Err(ParseError::corrupt("FASTA: no `>` header"));
    }
    let lengths = Arc::new(std::mem::take(&mut *lengths.lock().unwrap_or_else(|e| e.into_inner())));
    let total: u64 = lengths.iter().sum();
    let table = LineTable::new(index, columns(&[("ID", Text), ("Description", Text), ("Length", Number), ("Sequence", Text)]), move |row, lines| {
        let header = lines.first().map_or("", |h| h.trim_start_matches('>').trim_end());
        let (id, desc) = header.split_once(char::is_whitespace).unwrap_or((header, ""));
        let seq: String = lines.iter().skip(1).filter(|l| !l.starts_with(';')).map(|l| l.trim_end()).collect();
        vec![text(id), text(desc.trim()), Cell::Int(lengths.get(row).copied().unwrap_or(0) as i64), preview(&seq)]
    })
    .max_lines(3);
    Ok(lines::document("fasta", "FASTA", "Sequences", table, src, "fasta").with_meta("Residues", total))
}

// ---------------------------------------------------------------- FASTQ

pub(crate) fn fastq(src: &FileSource, cx: &ParseContext) -> Result<Document> {
    // Four lines a record, from the first `@`.
    let mut first: Option<u64> = None;
    let index = lines::index(src, "fastq", cx, |n, line| {
        if first.is_none() && line.starts_with('@') {
            first = Some(n);
        }
        match first {
            Some(f) if (n - f) % 4 == 0 && !line.trim().is_empty() => Line::Start,
            _ => Line::Within,
        }
    })?;
    if index.starts.is_empty() {
        return Err(ParseError::corrupt("FASTQ: no `@` record"));
    }
    let table = LineTable::new(index, columns(&[("ID", Text), ("Description", Text), ("Length", Number), ("Mean quality", Number), ("Sequence", Text)]), |_, lines| {
        let header = lines.first().map_or("", |h| h.trim_start_matches('@').trim_end());
        let (id, desc) = header.split_once(char::is_whitespace).unwrap_or((header, ""));
        let seq = lines.get(1).map_or("", |s| s.trim_end());
        let qual = lines.get(3).map_or("", |q| q.trim_end());
        // Phred+33.
        let mean = if qual.is_empty() { Cell::Null } else { Cell::Float((qual.bytes().map(|b| b.saturating_sub(33) as f64).sum::<f64>() / qual.len() as f64 * 10.0).round() / 10.0) };
        vec![text(id), text(desc.trim()), Cell::Int(seq.len() as i64), mean, preview(seq)]
    });
    Ok(lines::document("fastq", "FASTQ", "Reads", table, src, "fastq"))
}

// ---------------------------------------------------------------- GenBank

pub(crate) fn genbank(src: &FileSource, cx: &ParseContext) -> Result<Document> {
    // Features are counted while indexing; a record is read as far as its header.
    let features = Arc::new(Mutex::new(Vec::<u64>::new()));
    let counting = features.clone();
    let mut in_features = false;
    let index = lines::index(src, "genbank", cx, move |_, line| {
        let mut f = counting.lock().unwrap_or_else(|e| e.into_inner());
        if line.starts_with("LOCUS") {
            f.push(0);
            in_features = false;
            return Line::Start;
        }
        if line.starts_with("FEATURES") {
            in_features = true;
        } else if line.starts_with("ORIGIN") || line.starts_with("//") {
            in_features = false;
        } else if in_features
            && line.len() > 5
            && line.starts_with("     ")
            && !line.as_bytes()[5].is_ascii_whitespace()
            && let Some(last) = f.last_mut()
        {
            *last += 1;
        }
        Line::Within
    })?;
    if index.starts.is_empty() {
        return Err(ParseError::corrupt("GenBank: no LOCUS"));
    }
    let features = Arc::new(std::mem::take(&mut *features.lock().unwrap_or_else(|e| e.into_inner())));
    let table = LineTable::new(
        index,
        columns(&[("Locus", Text), ("Length (bp)", Number), ("Molecule", Text), ("Topology", Text), ("Date", ValueKind::Date), ("Definition", Text), ("Accession", Text), ("Organism", Text), ("Features", Number)]),
        move |row, lines| {
            // `LOCUS  NC_001416  48502 bp  DNA  linear  PHG 28-APR-2023`
            let locus: Vec<&str> = lines.first().map(|l| l.split_whitespace().collect()).unwrap_or_default();
            let bp = locus.iter().position(|w| *w == "bp" || *w == "aa").and_then(|i| locus.get(i - 1)).copied().unwrap_or("");
            let after = locus.iter().position(|w| *w == "bp" || *w == "aa").map_or(&[][..], |i| &locus[i + 1..]);
            let topology = after.iter().find(|w| matches!(**w, "linear" | "circular")).copied().unwrap_or("");
            // A keyword's value, with its continuation lines.
            let field = |key: &str| -> String {
                let mut out = String::new();
                let mut on = false;
                for l in lines.iter() {
                    if let Some(rest) = l.strip_prefix(key) {
                        on = true;
                        out.push_str(rest.trim());
                    } else if on && l.starts_with("            ") {
                        out.push(' ');
                        out.push_str(l.trim());
                    } else if on {
                        break;
                    }
                }
                out
            };
            let organism = lines.iter().find(|l| l.trim_start().starts_with("ORGANISM")).map(|l| l.trim_start()["ORGANISM".len()..].trim().to_string()).unwrap_or_default();
            vec![
                text(locus.get(1).copied().unwrap_or("")),
                num(bp),
                text(after.first().copied().unwrap_or("")),
                text(topology),
                locus.last().filter(|d| d.contains('-')).map_or(Cell::Null, |d| Cell::Date(d.to_string())),
                text(&field("DEFINITION")),
                text(&field("ACCESSION")),
                text(&organism),
                Cell::Int(features.get(row).copied().unwrap_or(0) as i64),
            ]
        },
    )
    .max_lines(80);
    Ok(lines::document("genbank", "GenBank", "Records", table, src, "genbank"))
}

// ---------------------------------------------------------------- GFF3, GTF

/// GFF3 attributes (`ID=g1;Name=abc`, URL-escaped) or GTF's (`gene_id "g1";`).
fn attributes(s: &str) -> Vec<(String, String)> {
    s.split(';')
        .map(str::trim)
        .filter(|a| !a.is_empty())
        .filter_map(|a| match a.split_once('=') {
            Some((k, v)) => Some((k.trim().to_string(), unescape_url(v.trim()))),
            None => a.split_once(' ').map(|(k, v)| (k.trim().to_string(), v.trim().trim_matches('"').to_string())),
        })
        .collect()
}

fn unescape_url(s: &str) -> String {
    let mut out = Vec::new();
    let b = s.as_bytes();
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%'
            && let Some(v) = s.get(i + 1..i + 3).and_then(|h| u8::from_str_radix(h, 16).ok())
        {
            out.push(v);
            i += 3;
            continue;
        }
        out.push(b[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

pub(crate) fn gff(src: &FileSource, cx: &ParseContext) -> Result<Document> {
    let gtf = src.ext().as_deref() == Some("gtf");
    // `##FASTA` ends the features: what follows is sequence.
    let index = lines::index(src, "gff", cx, |_, line| {
        if line.starts_with("##FASTA") || line.starts_with('>') {
            Line::Stop
        } else if line.starts_with('#') || line.trim().is_empty() {
            Line::Within
        } else {
            Line::Start
        }
    })?;
    let named: [&str; 3] = if gtf { ["gene_id", "transcript_id", "gene_name"] } else { ["ID", "Name", "Parent"] };
    let mut cols = vec![("Sequence", Text), ("Source", Text), ("Type", Text), ("Start", Number), ("End", Number), ("Score", Number), ("Strand", Text), ("Phase", Number)];
    cols.extend(named.iter().map(|n| (*n, Text)));
    cols.push(("Attributes", Text));
    let table = LineTable::new(index, columns(&cols), move |_, lines| {
        let f = tabs(lines.first().map_or("", String::as_str));
        let get = |i: usize| f.get(i).copied().filter(|v| *v != ".").unwrap_or("");
        let attrs = attributes(get(8));
        let mut row = vec![text(get(0)), text(get(1)), text(get(2)), num(get(3)), num(get(4)), num(get(5)), text(get(6)), num(get(7))];
        row.extend(named.iter().map(|n| attrs.iter().find(|(k, _)| k == n).map_or(Cell::Null, |(_, v)| text(v))));
        row.push(text(get(8)));
        row
    });
    Ok(lines::document("gff", if gtf { "GTF" } else { "GFF3" }, "Features", table, src, "gff"))
}

// ---------------------------------------------------------------- BED

pub(crate) fn bed(src: &FileSource, cx: &ParseContext) -> Result<Document> {
    let mut width = 0;
    let index = lines::index(src, "bed", cx, |_, line| {
        let l = line.trim();
        if l.is_empty() || l.starts_with('#') || l.starts_with("track") || l.starts_with("browser") {
            return Line::Within;
        }
        width = width.max(l.split(['\t', ' ']).filter(|f| !f.is_empty()).count().min(12));
        Line::Start
    })?;
    const ALL: [(&str, ValueKind); 12] = [
        ("Chromosome", Text),
        ("Start", Number),
        ("End", Number),
        ("Name", Text),
        ("Score", Number),
        ("Strand", Text),
        ("Thick start", Number),
        ("Thick end", Number),
        ("Color", Text),
        ("Blocks", Number),
        ("Block sizes", Text),
        ("Block starts", Text),
    ];
    let width = width.max(3);
    let table = LineTable::new(index, columns(&ALL[..width]), move |_, lines| {
        let line = lines.first().map_or("", |l| l.trim());
        let f: Vec<&str> = if line.contains('\t') { line.split('\t').collect() } else { line.split_whitespace().collect() };
        (0..width).map(|i| match (f.get(i).copied(), ALL[i].1) {
            (Some(v), Number) => num(v),
            (Some(v), _) => text(v),
            (None, _) => Cell::Null,
        }).collect()
    });
    Ok(lines::document("bed", "BED", "Regions", table, src, "bed"))
}

// ---------------------------------------------------------------- VCF

pub(crate) fn vcf(src: &FileSource, cx: &ParseContext) -> Result<Document> {
    let mut header: Vec<String> = Vec::new();
    let mut meta: Vec<(String, String)> = Vec::new();
    let index = lines::index(src, "vcf", cx, |_, line| {
        if let Some(m) = line.strip_prefix("##") {
            if let Some((k, v)) = m.split_once('=')
                && matches!(k, "fileformat" | "reference" | "source" | "fileDate")
            {
                meta.push((k.to_string(), v.to_string()));
            }
            Line::Within
        } else if let Some(h) = line.strip_prefix('#') {
            header = tabs(h).into_iter().map(str::to_owned).collect();
            Line::Within
        } else if line.trim().is_empty() {
            Line::Within
        } else {
            Line::Start
        }
    })?;
    if header.is_empty() {
        return Err(ParseError::corrupt("VCF: no #CHROM header"));
    }
    let samples = header.len().saturating_sub(9);
    let kinds: Vec<ValueKind> = header.iter().map(|h| if matches!(h.as_str(), "POS" | "QUAL") { Number } else { Text }).collect();
    let cols: Vec<Column> = header.iter().zip(&kinds).map(|(h, &k)| Column::new(h.as_str()).with_kind(k)).collect();
    let table = LineTable::new(index, cols, move |_, lines| {
        let f = tabs(lines.first().map_or("", String::as_str));
        kinds.iter().enumerate().map(|(i, k)| match (f.get(i).copied().filter(|v| *v != "."), k) {
            (Some(v), Number) => num(v),
            (Some(v), _) => text(v),
            (None, _) => Cell::Null,
        }).collect()
    });
    let mut doc = lines::document("vcf", "Variant Call Format", "Variants", table, src, "vcf").with_meta("Samples", samples);
    for (k, v) in meta {
        doc = doc.with_meta(k, v);
    }
    Ok(doc)
}

// ---------------------------------------------------------------- SAM

pub(crate) fn sam(src: &FileSource, cx: &ParseContext) -> Result<Document> {
    let (mut refs, mut groups) = (0usize, 0usize);
    let index = lines::index(src, "sam", cx, |_, line| {
        if line.starts_with("@SQ") {
            refs += 1;
        } else if line.starts_with("@RG") {
            groups += 1;
        }
        if line.starts_with('@') || line.trim().is_empty() { Line::Within } else { Line::Start }
    })?;
    let cols: [(&str, ValueKind); 12] = [("Read", Text), ("Flag", Number), ("Reference", Text), ("Position", Number), ("Mapping quality", Number), ("CIGAR", Text), ("Mate reference", Text), ("Mate position", Number), ("Template length", Number), ("Sequence", Text), ("Quality", Text), ("Tags", Text)];
    let table = LineTable::new(index, columns(&cols), move |_, lines| {
        let f = tabs(lines.first().map_or("", String::as_str));
        let mut row: Vec<Cell> = (0..11).map(|i| match (f.get(i).copied().filter(|v| *v != "*"), cols[i].1) {
            (Some(v), Number) => num(v),
            (Some(v), _) if i >= 9 => preview(v),
            (Some(v), _) => text(v),
            (None, _) => Cell::Null,
        }).collect();
        row.push(if f.len() > 11 { Cell::Text(f[11..].join(" ")) } else { Cell::Null });
        row
    });
    Ok(lines::document("sam", "SAM", "Alignments", table, src, "sam").with_meta("References", refs).with_meta("Read groups", groups))
}

// ---------------------------------------------------------------- PDB

pub(crate) fn pdb(src: &FileSource, cx: &ParseContext) -> Result<Document> {
    let (mut header, mut title, mut models) = (String::new(), String::new(), 0usize);
    let index = lines::index(src, "pdb", cx, |_, line| {
        if let Some(h) = line.strip_prefix("HEADER") {
            header = h.trim().to_string();
        } else if let Some(t) = line.strip_prefix("TITLE") {
            // Continued titles carry a number in 9-10.
            let t = t.get(4..).unwrap_or(t).trim();
            if !title.is_empty() {
                title.push(' ');
            }
            title.push_str(t);
        } else if line.starts_with("MODEL") {
            models += 1;
        }
        if line.starts_with("ATOM  ") || line.starts_with("HETATM") { Line::Start } else { Line::Within }
    })?;
    if index.starts.is_empty() {
        return Err(ParseError::corrupt("PDB: no ATOM or HETATM records"));
    }
    let cols = [
        ("Record", Text),
        ("Serial", Number),
        ("Atom", Text),
        ("Residue", Text),
        ("Chain", Text),
        ("Residue number", Number),
        ("X", Number),
        ("Y", Number),
        ("Z", Number),
        ("Occupancy", Number),
        ("B-factor", Number),
        ("Element", Text),
    ];
    let table = LineTable::new(index, columns(&cols), |_, lines| {
        let l = lines.first().map_or("", String::as_str);
        // Fixed columns, from 1 as the format numbers them.
        let col = |a: usize, b: usize| l.get(a - 1..b.min(l.len())).unwrap_or("").trim();
        vec![
            text(col(1, 6)),
            num(col(7, 11)),
            text(col(13, 16)),
            text(col(18, 20)),
            text(col(22, 22)),
            num(col(23, 26)),
            num(col(31, 38)),
            num(col(39, 46)),
            num(col(47, 54)),
            num(col(55, 60)),
            num(col(61, 66)),
            text(col(77, 78)),
        ]
    });
    let mut doc = lines::document("pdb", "Protein Data Bank", "Atoms", table, src, "pdb").with_meta("Models", models.max(1));
    if !header.is_empty() {
        doc = doc.with_meta("Header", header);
    }
    if !title.is_empty() {
        doc = doc.with_meta("Title", title);
    }
    Ok(doc)
}

// ---------------------------------------------------------------- CIF

/// CIF's values: bare words, `'…'` and `"…"` (closed by a quote before white space),
/// and `;`-fields running to a line that starts with `;`.
fn cif_tokens(text: &str) -> Vec<(String, u32, bool)> {
    let mut out = Vec::new();
    let mut lines = text.lines().enumerate().peekable();
    while let Some((n, line)) = lines.next() {
        if let Some(first) = line.strip_prefix(';') {
            let mut v = first.to_string();
            for (_, l) in lines.by_ref() {
                if l.starts_with(';') {
                    break;
                }
                v.push('\n');
                v.push_str(l);
            }
            out.push((v.trim().to_string(), n as u32, true));
            continue;
        }
        let b = line.as_bytes();
        let mut i = 0;
        while i < b.len() {
            match b[i] {
                c if c.is_ascii_whitespace() => i += 1,
                b'#' => break,
                q @ (b'\'' | b'"') => {
                    let mut j = i + 1;
                    while j < b.len() && !(b[j] == q && b.get(j + 1).is_none_or(|c| c.is_ascii_whitespace())) {
                        j += 1;
                    }
                    out.push((line[i + 1..j.min(b.len())].to_string(), n as u32, true));
                    i = j + 1;
                }
                _ => {
                    let end = line[i..].find(char::is_whitespace).map_or(b.len(), |p| i + p);
                    out.push((line[i..end].to_string(), n as u32, false));
                    i = end;
                }
            }
        }
    }
    out
}

/// A CIF value as a cell: `?` and `.` are unknown and not applicable.
fn cif_cell(v: &str, quoted: bool) -> Cell {
    if !quoted && (v == "?" || v == ".") {
        return Cell::Null;
    }
    if quoted {
        return Cell::Text(v.to_string());
    }
    // `1.234(5)`: a value and its uncertainty.
    match num(v.split('(').next().unwrap_or(v)) {
        Cell::Null => Cell::Text(v.to_string()),
        n => n,
    }
}

pub(crate) fn cif(src: &FileSource, cx: &ParseContext) -> Result<Document> {
    let text = String::from_utf8_lossy(src.bytes());
    let toks = cif_tokens(&text);
    let mut b = Builder::new(cx);
    // Loops become tables; single items become tree nodes under their block.
    let mut loops: Vec<(String, Vec<String>, Vec<Vec<Cell>>)> = Vec::new();
    let mut block: Option<u32> = None;
    let mut i = 0;
    while i < toks.len() {
        let (t, line, quoted) = &toks[i];
        if !quoted && t.starts_with("data_") {
            block = Some(b.add(None, 1, t[5..].to_string(), "data block", ValueKind::Group, *line..*line + 1)?);
            i += 1;
        } else if !quoted && t.eq_ignore_ascii_case("loop_") {
            i += 1;
            let mut names = Vec::new();
            while i < toks.len() && !toks[i].2 && toks[i].0.starts_with('_') {
                names.push(toks[i].0.clone());
                i += 1;
            }
            let mut values = Vec::new();
            while i < toks.len() && (toks[i].2 || !(toks[i].0.starts_with('_') || toks[i].0.eq_ignore_ascii_case("loop_") || toks[i].0.starts_with("data_"))) {
                values.push(cif_cell(&toks[i].0, toks[i].2));
                i += 1;
            }
            if names.is_empty() {
                continue;
            }
            let category = names[0].split('.').next().unwrap_or(&names[0]).trim_start_matches('_').to_string();
            let rows: Vec<Vec<Cell>> = values.chunks(names.len()).map(|c| c.to_vec()).collect();
            cx.limits.check(Limit::Rows, rows.len() as u64)?;
            if let Some(blk) = block {
                b.add(Some(blk), 2, category.clone(), format!("loop of {} rows", rows.len()), ValueKind::Group, *line..*line + 1)?;
            }
            let short: Vec<String> = names.iter().map(|n| n.split('.').nth(1).unwrap_or(n).trim_start_matches('_').to_string()).collect();
            loops.push((category, short, rows));
        } else if !quoted && t.starts_with('_') {
            let value = toks.get(i + 1).map(|v| (v.0.clone(), v.2)).unwrap_or_default();
            let kind = cif_cell(&value.0, value.1).kind();
            b.add(block, if block.is_some() { 2 } else { 1 }, t.trim_start_matches('_').to_string(), value.0, if kind == ValueKind::Null { Text } else { kind }, *line..*line + 1)?;
            i += 2;
        } else {
            i += 1;
        }
    }
    let tree = b.finish()?;
    // The atoms first when there are any, else the biggest loop.
    loops.sort_by_key(|l| (l.0 != "atom_site", std::cmp::Reverse(l.2.len())));
    let mut doc = Document::new(format("cif", "Crystallographic Information File"));
    let names: Vec<String> = loops.iter().map(|l| l.0.clone()).collect();
    for (n, (category, names, rows)) in loops.into_iter().take(32).enumerate() {
        let cols: Vec<Column> = names
            .iter()
            .enumerate()
            .map(|(i, name)| {
                let mut kinds = rows.iter().take(1000).map(|r| r.get(i).map_or(ValueKind::Null, Cell::kind)).filter(|k| *k != ValueKind::Null);
                let first = kinds.next();
                Column::new(name.as_str()).with_kind(match first {
                    Some(k) if kinds.all(|x| x == k) => k,
                    Some(_) => ValueKind::Mixed,
                    None => ValueKind::Unknown,
                })
            })
            .collect();
        let rows: Vec<Vec<Cell>> = rows.into_iter().map(|mut r| {
            r.resize(names.len(), Cell::Null);
            r
        }).collect();
        let table = Content::Table(Arc::new(OwnedTable::new(cols, rows)));
        let role = if n == 0 { Role::Primary } else { Role::Inspector };
        let info = FacetInfo::new(role, category, Model::Table);
        doc = doc.with_lazy_facet(info, move |_| Ok(table.clone()));
    }
    // Without loops, the items are what the file holds.
    let role = if names.is_empty() { Role::Primary } else { Role::Structure };
    Ok(doc.with_facet(role, "Items", Content::Tree(Arc::new(tree))).with_meta("Loops", names.len()).with_file_facets(src, Some("cif")))
}

// ---------------------------------------------------------------- ARFF

pub(crate) fn arff(src: &FileSource, cx: &ParseContext) -> Result<Document> {
    let mut attrs: Vec<(String, ValueKind)> = Vec::new();
    let mut relation = String::new();
    let mut data = false;
    let index = lines::index(src, "arff", cx, |_, line| {
        let l = line.trim();
        if l.is_empty() || l.starts_with('%') {
            return Line::Within;
        }
        if data {
            return Line::Start;
        }
        let lower = l.to_ascii_lowercase();
        if lower.starts_with("@relation") {
            relation = l[9..].trim().trim_matches(['\'', '"']).to_string();
        } else if lower.starts_with("@attribute") {
            let rest = l[10..].trim();
            // The name may be quoted.
            let (name, ty) = match rest.chars().next() {
                Some(q @ ('\'' | '"')) => {
                    let end = rest[1..].find(q).map_or(rest.len(), |p| p + 1);
                    (rest[1..end].to_string(), rest.get(end + 1..).unwrap_or("").trim())
                }
                _ => rest.split_once(char::is_whitespace).map_or((rest.to_string(), ""), |(n, t)| (n.to_string(), t.trim())),
            };
            let t = ty.to_ascii_lowercase();
            let kind = if matches!(t.as_str(), "numeric" | "real" | "integer") { Number } else if t.starts_with("date") { ValueKind::Date } else { Text };
            attrs.push((name, kind));
        } else if lower.starts_with("@data") {
            data = true;
        }
        Line::Within
    })?;
    if attrs.is_empty() {
        return Err(ParseError::corrupt("ARFF: no @attribute"));
    }
    let kinds: Vec<ValueKind> = attrs.iter().map(|a| a.1).collect();
    let table = LineTable::new(index, attrs.iter().map(|(n, k)| Column::new(n.as_str()).with_kind(*k)).collect(), move |_, lines| {
        let line = lines.first().map_or("", |l| l.trim());
        let mut row = vec![Cell::Null; kinds.len()];
        let cell = |v: &str, k: ValueKind| {
            let v = v.trim();
            let v = v.strip_prefix(['\'', '"']).and_then(|x| x.strip_suffix(['\'', '"'])).unwrap_or(v);
            match k {
                _ if v == "?" => Cell::Null,
                Number => num(v),
                ValueKind::Date => Cell::Date(v.to_string()),
                _ => text(v),
            }
        };
        if let Some(sparse) = line.strip_prefix('{').and_then(|l| l.strip_suffix('}')) {
            // Sparse: `{0 5, 3 yes}`; omitted values are 0.
            for (i, k) in kinds.iter().enumerate() {
                if *k == Number {
                    row[i] = Cell::Int(0);
                }
            }
            for pair in sparse.split(',') {
                if let Some((i, v)) = pair.trim().split_once(' ')
                    && let Ok(i) = i.parse::<usize>()
                    && i < kinds.len()
                {
                    row[i] = cell(v, kinds[i]);
                }
            }
        } else {
            for (i, v) in lines::csv_fields(line).into_iter().enumerate().take(kinds.len()) {
                row[i] = cell(&v, kinds[i]);
            }
        }
        row
    });
    let mut doc = lines::document("arff", "ARFF", "Instances", table, src, "arff");
    if !relation.is_empty() {
        doc = doc.with_meta("Relation", relation);
    }
    Ok(doc)
}

// ---------------------------------------------------------------- DIMACS

pub(crate) fn dimacs(src: &FileSource, cx: &ParseContext) -> Result<Document> {
    let mut problem: Option<Vec<String>> = None;
    let index = lines::index(src, "dimacs", cx, |_, line| {
        let l = line.trim();
        if let Some(p) = l.strip_prefix("p ") {
            problem = Some(p.split_whitespace().map(str::to_owned).collect());
            return Line::Within;
        }
        if l.is_empty() || l.starts_with('c') || l.starts_with('%') {
            return Line::Within;
        }
        Line::Start
    })?;
    let problem = problem.ok_or_else(|| ParseError::corrupt("DIMACS: no `p` line"))?;
    let kind = problem.first().cloned().unwrap_or_default();
    let (name, facet, table) = match kind.as_str() {
        "cnf" | "wcnf" => (
            "DIMACS CNF",
            "Clauses",
            LineTable::new(index, columns(&[("Literals", Text), ("Size", Number)]), |_, lines| {
                let lits: Vec<&str> = lines.iter().flat_map(|l| l.split_whitespace()).take_while(|l| *l != "0").collect();
                vec![Cell::Text(lits.join(" ")), Cell::Int(lits.len() as i64)]
            }),
        ),
        _ => (
            "DIMACS graph",
            "Edges",
            // `e u v`, `a u v w` (shortest paths, flows), `n id value`.
            LineTable::new(index, columns(&[("Kind", Text), ("From", Number), ("To", Number), ("Value", Number)]), |_, lines| {
                let f: Vec<&str> = lines.first().map(|l| l.split_whitespace().collect()).unwrap_or_default();
                let get = |i: usize| f.get(i).copied().map_or(Cell::Null, num);
                match f.first().copied() {
                    Some("n") => vec![text("node"), get(1), Cell::Null, get(2)],
                    Some(k) => vec![text(if k == "e" { "edge" } else { "arc" }), get(1), get(2), get(3)],
                    None => vec![Cell::Null; 4],
                }
            }),
        ),
    };
    Ok(lines::document("dimacs", name, facet, table, src, "dimacs").with_meta("Problem", problem.join(" ")))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cif_values_and_attributes() {
        let t = cif_tokens("data_x\n_cell.length_a 10.5(2)\n_title 'it''s a \"test\"'\n;\nmulti\nline\n;\n");
        assert_eq!(t[0].0, "data_x");
        assert_eq!(cif_cell(&t[2].0, t[2].2), Cell::Number("10.5".into()));
        assert_eq!(t[4].0, "it''s a \"test\"");
        assert_eq!((t[5].0.as_str(), t[5].2), ("multi\nline", true));
        assert_eq!(attributes("ID=gene1;Name=ab%3Bc"), [("ID".to_string(), "gene1".to_string()), ("Name".to_string(), "ab;c".to_string())]);
        assert_eq!(attributes("gene_id \"g1\"; transcript_id \"t1\";")[1], ("transcript_id".to_string(), "t1".to_string()));
    }
}
