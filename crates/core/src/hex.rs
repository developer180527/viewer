use crate::{Confidence, Content, Document, FileSource, FormatLoader, Result};

/// Shows raw bytes. Accepts every file, so nothing is ever "unsupported".
pub struct HexLoader;

impl FormatLoader for HexLoader {
    fn id(&self) -> &'static str {
        "hex"
    }

    fn name(&self) -> &'static str {
        "Hex"
    }

    fn probe(&self, _: &[u8], _: Option<&str>, _: &str) -> Confidence {
        Confidence::FALLBACK
    }

    fn open(&self, src: &FileSource) -> Result<Document> {
        let detected = infer::get(src.header())
            .map(|t| format!("{} ({})", t.mime_type(), t.extension()))
            .unwrap_or_else(|| "unknown".into());
        Ok(Document::new("Binary data", Content::Hex(src.clone()))
            .with_meta("Detected type", detected)
            .with_meta("Entropy (first 64 KiB)", format!("{:.2} bits/byte", entropy(src.bytes()))))
    }
}

fn entropy(bytes: &[u8]) -> f64 {
    let sample = &bytes[..bytes.len().min(64 * 1024)];
    if sample.is_empty() {
        return 0.0;
    }
    let mut counts = [0u32; 256];
    for &b in sample {
        counts[b as usize] += 1;
    }
    let n = sample.len() as f64;
    counts
        .iter()
        .filter(|&&c| c > 0)
        .map(|&c| {
            let p = c as f64 / n;
            -p * p.log2()
        })
        .sum()
}
