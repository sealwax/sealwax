//! Building small PDFs for the tests.

#![allow(dead_code)]

/// A minimal PDF whose objects are given as `(number, body)`, object 1 being the catalog.
/// The xref table is computed, so offsets are exact unless `break_xref` is set.
pub fn pdf(objects: &[(u32, &str)], break_xref: bool) -> Vec<u8> {
    pdf_with_trailer(objects, break_xref, "")
}

/// As [`pdf`], with `trailer` appended to the trailer dictionary.
pub fn pdf_with_trailer(objects: &[(u32, &str)], break_xref: bool, trailer: &str) -> Vec<u8> {
    let objects: Vec<(u32, Vec<u8>)> = objects
        .iter()
        .map(|(number, body)| (*number, body.as_bytes().to_vec()))
        .collect();
    pdf_bytes(&objects, break_xref, trailer)
}

/// As [`pdf_with_trailer`], with bodies of any bytes (binary stream data).
pub fn pdf_bytes(objects: &[(u32, Vec<u8>)], break_xref: bool, trailer: &str) -> Vec<u8> {
    let mut out = b"%PDF-1.7\n%\xE2\xE3\xCF\xD3\n".to_vec();
    let mut offsets = Vec::new();
    for (number, body) in objects {
        offsets.push((*number, out.len()));
        out.extend_from_slice(format!("{number} 0 obj\n").as_bytes());
        out.extend_from_slice(body);
        out.extend_from_slice(b"\nendobj\n");
    }
    let size = objects.iter().map(|(n, _)| n).max().copied().unwrap_or(0) + 1;
    let xref_at = out.len();
    out.extend_from_slice(format!("xref\n0 {size}\n0000000000 65535 f \n").as_bytes());
    for number in 1..size {
        let offset = offsets
            .iter()
            .find(|(n, _)| *n == number)
            .map(|(_, o)| if break_xref { o + 7 } else { *o })
            .unwrap_or(0);
        out.extend_from_slice(format!("{offset:010} 00000 n \n").as_bytes());
    }
    out.extend_from_slice(
        format!("trailer\n<< /Size {size} /Root 1 0 R {trailer} >>\nstartxref\n{xref_at}\n%%EOF\n")
            .as_bytes(),
    );
    out
}
