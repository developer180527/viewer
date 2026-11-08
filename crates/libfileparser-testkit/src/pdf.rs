//! Small PDFs written by hand, for PDF readers' tests (`libfileparser-docs`, MuPDF,
//! PDFKit, or an app's own).

/// A PDF with one page per `(width, height, rotate)`, each with its top-left
/// quarter (as the page is defined, before `/Rotate`) filled red.
pub fn pdf(pages: &[(f32, f32, u32)]) -> Vec<u8> {
    let n = pages.len();
    // Objects: 1 catalog, 2 page tree, then a page and its contents per page.
    let mut objects = vec![
        "<< /Type /Catalog /Pages 2 0 R >>".to_string(),
        format!("<< /Type /Pages /Count {n} /Kids [{}] >>", (0..n).map(|i| format!("{} 0 R", 3 + i * 2)).collect::<Vec<_>>().join(" ")),
    ];
    for (i, &(w, h, rot)) in pages.iter().enumerate() {
        let content = format!("1 0 0 rg 0 {} {} {} re f", h / 2.0, w / 2.0, h / 2.0);
        objects.push(format!("<< /Type /Page /Parent 2 0 R /MediaBox [0 0 {w} {h}] /Rotate {rot} /Contents {} 0 R >>", 4 + i * 2));
        objects.push(format!("<< /Length {} >>\nstream\n{content}\nendstream", content.len()));
    }
    let mut out = b"%PDF-1.4\n".to_vec();
    let mut offsets = Vec::new();
    for (i, o) in objects.iter().enumerate() {
        offsets.push(out.len());
        out.extend(format!("{} 0 obj\n{o}\nendobj\n", i + 1).bytes());
    }
    let xref = out.len();
    out.extend(format!("xref\n0 {}\n0000000000 65535 f \n", objects.len() + 1).bytes());
    for o in offsets {
        out.extend(format!("{o:010} 00000 n \n").bytes());
    }
    out.extend(format!("trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n", objects.len() + 1).bytes());
    out
}

/// The pixel at (x, y) of an RGBA image `width` wide.
pub fn pixel(rgba: &[u8], width: u32, x: u32, y: u32) -> [u8; 4] {
    let i = (y * width + x) as usize * 4;
    rgba[i..i + 4].try_into().unwrap()
}
