//! Document metadata as PDF/A requires it: an XMP packet in the catalog (ISO 19005-3, 6.6.2),
//! identifying the part and conformance level (6.6.4), with only properties of the predefined
//! schemas (6.6.2.3.1).
//!
//! The packet is written anew from the document information dictionary, which is left as it is
//! but for text written in UTF-8 without a byte order mark (or in UTF-16LE), rewritten in
//! UTF-16BE: the two then agree, for a validator that reads the dictionary by the letter of the
//! standard too. Properties of the input's own XMP packet are not carried over yet.

use std::fmt::Write as _;

use sealwax_core::{
    Dict, Document, Name, Object, Result, Stream, encode_text_string, is_standard_text_string,
};

use crate::edit::{self, Location};
use crate::{Level, Report};

/// The fields of the document information dictionary, as text.
#[derive(Debug, Default)]
struct Info {
    title: Option<String>,
    author: Option<String>,
    subject: Option<String>,
    keywords: Option<String>,
    creator: Option<String>,
    producer: Option<String>,
    created: Option<String>,
    modified: Option<String>,
}

fn read_info(doc: &Document) -> Result<Info> {
    let Some(id) = doc.info_id() else {
        return Ok(Info::default());
    };
    let Some(Object::Dict(dict)) = doc.object(id)? else {
        return Ok(Info::default());
    };
    let text = |key: &[u8]| -> Result<Option<String>> {
        Ok(match dict.get(key) {
            Some(value) => edit::resolved(doc, value)?
                .as_ref()
                .and_then(Object::as_string)
                .map(text_string)
                .filter(|text| !text.is_empty()),
            None => None,
        })
    };
    let date = |key: &[u8]| -> Result<Option<String>> {
        Ok(match dict.get(key) {
            Some(value) => edit::resolved(doc, value)?
                .as_ref()
                .and_then(Object::as_string)
                .and_then(xmp_date),
            None => None,
        })
    };
    Ok(Info {
        title: text(b"Title")?,
        author: text(b"Author")?,
        subject: text(b"Subject")?,
        keywords: text(b"Keywords")?,
        creator: text(b"Creator")?,
        producer: text(b"Producer")?,
        created: date(b"CreationDate")?,
        modified: date(b"ModDate")?,
    })
}

pub(crate) use sealwax_core::decode_text_string as text_string;

/// A PDF date (`D:YYYYMMDDHHmmSSOHH'mm'`, every part after the year optional) as an XMP date.
pub(crate) fn xmp_date(bytes: &[u8]) -> Option<String> {
    let text = std::str::from_utf8(bytes).ok()?.trim();
    let text = text.strip_prefix("D:").unwrap_or(text);
    let digits = |from: usize, len: usize| -> Option<u32> {
        let part = text.get(from..from + len)?;
        part.bytes()
            .all(|b| b.is_ascii_digit())
            .then(|| part.parse().ok())?
    };
    let year = digits(0, 4)?;
    let mut out = format!("{year:04}");
    let mut at = 4;
    let ranges = [(1, 12), (1, 31), (0, 23), (0, 59), (0, 59)];
    let mut parts = Vec::new();
    for (low, high) in ranges {
        match digits(at, 2) {
            Some(value) if (low..=high).contains(&value) => {
                parts.push(value);
                at += 2;
            }
            Some(_) => return None,
            None => break,
        }
    }
    if let Some(month) = parts.first() {
        let _ = write!(out, "-{month:02}");
    }
    if let Some(day) = parts.get(1) {
        let _ = write!(out, "-{day:02}");
    }
    if parts.len() < 4 {
        // A date, or a date and an hour without minutes, which XMP cannot say: the date alone.
        return Some(out);
    }
    let _ = write!(out, "T{:02}:{:02}", parts[2], parts[3]);
    if let Some(seconds) = parts.get(4) {
        let _ = write!(out, ":{seconds:02}");
    }
    let rest = &text[at..];
    match rest.as_bytes().first() {
        Some(b'Z') => out.push('Z'),
        Some(sign @ (b'+' | b'-')) => {
            let zone: Vec<u32> = rest[1..]
                .split(|c: char| !c.is_ascii_digit())
                .filter(|part| !part.is_empty())
                .take(2)
                .filter_map(|part| part.parse().ok())
                .collect();
            let hours = zone.first().copied().unwrap_or(0);
            let minutes = zone.get(1).copied().unwrap_or(0);
            if hours <= 23 && minutes <= 59 {
                let _ = write!(out, "{}{hours:02}:{minutes:02}", char::from(*sign));
            }
        }
        _ => {}
    }
    Some(out)
}

/// Text for XML: escaped, without the characters XML 1.0 does not allow.
fn xml(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '\t' | '\n' | '\r' => out.push(c),
            c if u32::from(c) < 0x20 || matches!(c, '\u{FFFE}' | '\u{FFFF}') => {}
            c => out.push(c),
        }
    }
    out
}

/// The XMP packet.
fn packet(info: &Info, level: Level) -> String {
    let mut out = String::new();
    out.push_str("<?xpacket begin=\"\u{FEFF}\" id=\"W5M0MpCehiHzreSzNTczkc9d\"?>\n");
    out.push_str("<x:xmpmeta xmlns:x=\"adobe:ns:meta/\">\n");
    out.push_str(" <rdf:RDF xmlns:rdf=\"http://www.w3.org/1999/02/22-rdf-syntax-ns#\">\n");
    let _ = write!(
        out,
        "  <rdf:Description rdf:about=\"\" xmlns:pdfaid=\"http://www.aiim.org/pdfa/ns/id/\">\n   \
         <pdfaid:part>3</pdfaid:part>\n   <pdfaid:conformance>{}</pdfaid:conformance>\n  \
         </rdf:Description>\n",
        level.conformance()
    );
    out.push_str(
        "  <rdf:Description rdf:about=\"\" xmlns:dc=\"http://purl.org/dc/elements/1.1/\">\n   \
         <dc:format>application/pdf</dc:format>\n",
    );
    if let Some(title) = &info.title {
        let _ = writeln!(
            out,
            "   <dc:title><rdf:Alt><rdf:li xml:lang=\"x-default\">{}</rdf:li></rdf:Alt></dc:title>",
            xml(title)
        );
    }
    if let Some(author) = &info.author {
        let _ = writeln!(
            out,
            "   <dc:creator><rdf:Seq><rdf:li>{}</rdf:li></rdf:Seq></dc:creator>",
            xml(author)
        );
    }
    if let Some(subject) = &info.subject {
        let _ = writeln!(
            out,
            "   <dc:description><rdf:Alt><rdf:li xml:lang=\"x-default\">{}</rdf:li></rdf:Alt></dc:description>",
            xml(subject)
        );
    }
    out.push_str("  </rdf:Description>\n");
    if info.producer.is_some() || info.keywords.is_some() {
        out.push_str(
            "  <rdf:Description rdf:about=\"\" xmlns:pdf=\"http://ns.adobe.com/pdf/1.3/\">\n",
        );
        if let Some(producer) = &info.producer {
            let _ = writeln!(out, "   <pdf:Producer>{}</pdf:Producer>", xml(producer));
        }
        if let Some(keywords) = &info.keywords {
            let _ = writeln!(out, "   <pdf:Keywords>{}</pdf:Keywords>", xml(keywords));
        }
        out.push_str("  </rdf:Description>\n");
    }
    if info.creator.is_some() || info.created.is_some() || info.modified.is_some() {
        out.push_str(
            "  <rdf:Description rdf:about=\"\" xmlns:xmp=\"http://ns.adobe.com/xap/1.0/\">\n",
        );
        if let Some(creator) = &info.creator {
            let _ = writeln!(
                out,
                "   <xmp:CreatorTool>{}</xmp:CreatorTool>",
                xml(creator)
            );
        }
        if let Some(created) = &info.created {
            let _ = writeln!(out, "   <xmp:CreateDate>{created}</xmp:CreateDate>");
        }
        if let Some(modified) = &info.modified {
            let _ = writeln!(out, "   <xmp:ModifyDate>{modified}</xmp:ModifyDate>");
        }
        out.push_str("  </rdf:Description>\n");
    }
    out.push_str(" </rdf:RDF>\n</x:xmpmeta>\n<?xpacket end=\"w\"?>");
    out
}

/// The text entries of the document information that the packet repeats.
const INFO_TEXT: [&[u8]; 6] = [
    b"Title",
    b"Author",
    b"Subject",
    b"Keywords",
    b"Creator",
    b"Producer",
];

/// Rewrite in UTF-16BE the document information's text that a reader following the standard to
/// the letter reads otherwise than [`text_string`] (UTF-8 without a byte order mark, UTF-16LE):
/// the packet holds the text as decoded, and a validator compares it with the dictionary.
fn reencode_info(doc: &mut Document, report: &mut Report) -> Result<()> {
    let Some(id) = doc.info_id() else {
        return Ok(());
    };
    let Some(Object::Dict(dict)) = doc.object(id)? else {
        return Ok(());
    };
    let mut rewritten = Vec::new();
    for key in INFO_TEXT {
        let Some(value) = dict.get(key) else {
            continue;
        };
        if let Some(Object::String(bytes)) = edit::resolved(doc, value)?
            && !is_standard_text_string(&bytes)
        {
            rewritten.push((key, encode_text_string(&text_string(&bytes))));
        }
    }
    if rewritten.is_empty() {
        return Ok(());
    }
    let count = rewritten.len();
    edit::edit_dict(doc, &Location::of(id), false, |info| {
        for (key, bytes) in rewritten {
            info.insert(key, Object::String(bytes));
        }
    })?;
    report.change("info_reencoded", count);
    Ok(())
}

/// Write the XMP packet into the catalog, replacing the one there was.
pub(crate) fn apply(doc: &mut Document, level: Level, report: &mut Report) -> Result<()> {
    reencode_info(doc, report)?;
    let info = read_info(doc)?;
    let mut dict = Dict::new();
    dict.insert("Type", Name::from("Metadata"));
    dict.insert("Subtype", Name::from("XML"));
    // Not compressed: PDF/A-1 required it, and indexers read the packet in the raw bytes.
    let stream = Stream {
        dict,
        data: packet(&info, level).into_bytes(),
    };
    let id = doc.add_object(Object::Stream(stream))?;
    let catalog = Location::of(doc.catalog_id());
    let mut replaced = false;
    edit::edit_dict(doc, &catalog, false, |catalog| {
        replaced = catalog.insert("Metadata", id).is_some();
    })?;
    report.change(
        if replaced {
            "metadata_replaced"
        } else {
            "metadata_added"
        },
        1,
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{xml, xmp_date};

    #[test]
    fn pdf_dates_become_xmp_dates() {
        let cases: [(&[u8], Option<&str>); 9] = [
            (
                b"D:20231005142233+02'00'",
                Some("2023-10-05T14:22:33+02:00"),
            ),
            (b"D:20231005142233Z", Some("2023-10-05T14:22:33Z")),
            (b"D:20231005142233-05'30", Some("2023-10-05T14:22:33-05:30")),
            (b"D:202310051422", Some("2023-10-05T14:22")),
            (b"D:2023100514", Some("2023-10-05")),
            (b"20231005", Some("2023-10-05")),
            (b"D:2023", Some("2023")),
            (b"D:20231305", None),
            (b"yesterday", None),
        ];
        for (pdf, xmp) in cases {
            assert_eq!(
                xmp_date(pdf).as_deref(),
                xmp,
                "{}",
                String::from_utf8_lossy(pdf)
            );
        }
    }

    #[test]
    fn xml_drops_what_xml_cannot_hold() {
        assert_eq!(xml("a\u{1}b<&>\tc"), "ab&lt;&amp;&gt;\tc");
    }
}
