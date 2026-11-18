//! What the document says about itself: the information dictionary, the same in an XMP packet,
//! and in the catalog its language and how readers show its title.

use std::fmt::Write as _;

use sealwax_core::{Dict, Document, Object, encode_text_string};

use crate::Error;
use crate::model::Info;

/// The information dictionary and the XMP packet of `info`, its PDF/UA claim only if `whole`
/// (nothing was left out); in the catalog, the language, and that readers show the title
/// rather than the file name (PDF/UA, 7.1-10).
pub(crate) fn write(doc: &mut Document, info: &Info, whole: bool) -> Result<(), Error> {
    let mut dict = Dict::new();
    for (key, value) in [
        ("Title", &info.title),
        ("Author", &info.author),
        ("Subject", &info.subject),
        ("Keywords", &info.keywords),
    ] {
        if let Some(value) = value {
            dict.insert(key, Object::String(encode_text_string(value)));
        }
    }
    if !dict.is_empty() {
        let id = doc.add_object(Object::Dict(dict))?;
        doc.set_info_id(Some(id));
    }
    doc.set_xmp(packet(info, info.pdf_ua && whole).as_bytes())?;
    let catalog = doc.catalog_id();
    if let Some(Object::Dict(mut dict)) = doc.object(catalog)? {
        if let Some(language) = &info.language {
            dict.insert("Lang", Object::String(encode_text_string(language)));
        }
        let mut preferences = Dict::new();
        preferences.insert("DisplayDocTitle", Object::Bool(true));
        dict.insert("ViewerPreferences", preferences);
        doc.set_object(catalog, Object::Dict(dict));
    }
    Ok(())
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

/// The XMP packet: Dublin Core and PDF properties as the information dictionary gives them,
/// and the PDF/UA part if `pdf_ua`. No date, so that a composition always writes the same
/// document.
fn packet(info: &Info, pdf_ua: bool) -> String {
    let mut out = String::new();
    out.push_str("<?xpacket begin=\"\u{FEFF}\" id=\"W5M0MpCehiHzreSzNTczkc9d\"?>\n");
    out.push_str("<x:xmpmeta xmlns:x=\"adobe:ns:meta/\">\n");
    out.push_str(" <rdf:RDF xmlns:rdf=\"http://www.w3.org/1999/02/22-rdf-syntax-ns#\">\n");
    out.push_str(
        "  <rdf:Description rdf:about=\"\" xmlns:dc=\"http://purl.org/dc/elements/1.1/\" \
         xmlns:pdf=\"http://ns.adobe.com/pdf/1.3/\" \
         xmlns:pdfuaid=\"http://www.aiim.org/pdfua/ns/id/\">\n",
    );
    let item = |out: &mut String, property: &str, container: &str, value: &str, alt: bool| {
        let lang = if alt { " xml:lang=\"x-default\"" } else { "" };
        let _ = writeln!(
            out,
            "   <dc:{property}><rdf:{container}><rdf:li{lang}>{}</rdf:li></rdf:{container}></dc:{property}>",
            xml(value)
        );
    };
    if let Some(title) = &info.title {
        item(&mut out, "title", "Alt", title, true);
    }
    if let Some(author) = &info.author {
        item(&mut out, "creator", "Seq", author, false);
    }
    if let Some(subject) = &info.subject {
        item(&mut out, "description", "Alt", subject, true);
    }
    if let Some(language) = &info.language {
        item(&mut out, "language", "Bag", language, false);
    }
    if let Some(keywords) = &info.keywords {
        let _ = writeln!(out, "   <pdf:Keywords>{}</pdf:Keywords>", xml(keywords));
    }
    if pdf_ua {
        out.push_str("   <pdfuaid:part>1</pdfuaid:part>\n");
    }
    out.push_str("  </rdf:Description>\n </rdf:RDF>\n</x:xmpmeta>\n");
    out.push_str("<?xpacket end=\"w\"?>");
    out
}

#[cfg(test)]
mod tests {
    use super::{Info, packet, xml};

    #[test]
    fn xml_escapes_and_drops_what_xml_cannot_hold() {
        assert_eq!(xml("a<b>&c\u{1}"), "a&lt;b&gt;&amp;c");
    }

    #[test]
    fn the_packet_says_what_the_information_says() {
        let info = Info {
            title: Some("Report & annex".to_owned()),
            author: Some("Tests".to_owned()),
            language: Some("fr-CA".to_owned()),
            ..Info::default()
        };
        assert!(packet(&info, true).contains("<pdfuaid:part>1</pdfuaid:part>"));
        let packet = packet(&info, false);
        assert!(!packet.contains("<pdfuaid:part>"));
        assert!(packet.contains(
            "<dc:title><rdf:Alt><rdf:li xml:lang=\"x-default\">Report &amp; annex</rdf:li>"
        ));
        assert!(packet.contains("<dc:creator><rdf:Seq><rdf:li>Tests</rdf:li>"));
        assert!(packet.contains("<dc:language><rdf:Bag><rdf:li>fr-CA</rdf:li>"));
        assert!(!packet.contains("dc:description"));
    }
}
