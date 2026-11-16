//! `sealwax text-census`: what the glyphs of every PDF of a corpus map to in Unicode, against
//! the two PDF/A rules that most often keep a conversion from level a (ISO 19005-3, 6.2.11.7.2
//! and 6.2.11.7.3). One JSON line per document; `sealwax text-census-report` sums them up.

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::io::BufRead;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::{Path, PathBuf};
use std::time::Instant;

use sealwax::{Document, Error, OpenOptions};
use sealwax_text::{TextCensus, text_census};
use serde::{Deserialize, Serialize};

#[derive(Debug, Default, Serialize, Deserialize)]
struct Row {
    sha256: String,
    bytes: usize,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    error: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    text: Option<TextCensus>,
    seconds: f64,
}

fn one(file: &Path) -> Row {
    let mut row = Row::default();
    let data = match std::fs::read(file) {
        Ok(data) => data,
        Err(error) => {
            row.error = Some(error.to_string());
            return row;
        }
    };
    row.sha256 = crate::corpus::sha256(&data);
    row.bytes = data.len();
    let started = Instant::now();
    let measured = catch_unwind(AssertUnwindSafe(|| {
        // The census interprets every page, as a conversion would.
        Document::open(data, &OpenOptions::default()).and_then(|doc| text_census(&doc))
    }));
    row.seconds = started.elapsed().as_secs_f64();
    match measured {
        Ok(Ok(text)) => row.text = Some(text),
        Ok(Err(Error::PasswordRequired)) => row.error = Some("password".into()),
        Ok(Err(Error::Malformed)) => row.error = Some("malformed".into()),
        Ok(Err(error)) => row.error = Some(error.to_string()),
        Err(_) => row.error = Some("panic".into()),
    }
    row
}

/// Measure every PDF under `paths` with `jobs` threads, one JSON line each in `out`.
pub fn run(paths: &[PathBuf], out: &Path, jobs: usize) -> std::io::Result<usize> {
    crate::corpus::each_to_jsonl(paths, out, jobs, one)
}

/// Markdown summary of a run.
pub fn report(results: &Path) -> std::io::Result<String> {
    let mut rows: BTreeMap<String, Row> = BTreeMap::new();
    for line in std::io::BufReader::new(std::fs::File::open(results)?)
        .lines()
        .map_while(Result::ok)
    {
        if let Ok(row) = serde_json::from_str::<Row>(&line) {
            rows.insert(row.sha256.clone(), row);
        }
    }
    let docs: Vec<(&String, &TextCensus)> = rows
        .iter()
        .filter_map(|(sha, row)| row.text.as_ref().map(|text| (sha, text)))
        .collect();
    let n = docs.len();
    let with_text = docs.iter().filter(|(_, t)| t.glyphs > 0).count();
    let count = |f: &dyn Fn(&TextCensus) -> bool| docs.iter().filter(|(_, t)| f(t)).count();
    let mapping = count(&TextCensus::fails_unicode_mapping);
    let private_use = count(&TextCensus::fails_private_use);
    let either = count(&|t| t.fails_unicode_mapping() || t.fails_private_use());
    let ocr = count(&|t| t.pages_only_invisible > 0);
    let partial = count(&|t| t.unsupported_fonts > 0 || t.failed_pages > 0);
    let glyphs: u64 = docs.iter().map(|(_, t)| t.glyphs).sum();
    let without: u64 = docs
        .iter()
        .map(|(_, t)| t.without_unicode + t.invalid_unicode)
        .sum();
    let pua: u64 = docs
        .iter()
        .map(|(_, t)| t.private_use - t.private_use_with_actual_text)
        .sum();

    let mut out = String::new();
    let _ = writeln!(out, "# Text census: glyphs and Unicode\n");
    let _ = writeln!(
        out,
        "{} documents, {n} read ({} with text), {} not read.\n",
        rows.len(),
        with_text,
        rows.len() - n
    );
    let _ = writeln!(out, "| | Documents | Glyphs |\n|---|---|---|");
    let _ = writeln!(
        out,
        "| A glyph without a valid Unicode value (6.2.11.7.2) | {mapping} | {without} of {glyphs} |"
    );
    let _ = writeln!(
        out,
        "| A Private Use Area glyph without `ActualText` (6.2.11.7.3) | {private_use} | {pua} |"
    );
    let _ = writeln!(
        out,
        "| **Either: not PDF/A level a or u as it stands** | **{either}** | |"
    );
    let in_annotations =
        count(&|t| t.annotation_without_unicode + t.annotation_private_use_uncovered > 0);
    let only_annotations = count(&|t| {
        let problems =
            t.without_unicode + t.invalid_unicode + t.private_use - t.private_use_with_actual_text;
        problems > 0
            && problems == t.annotation_without_unicode + t.annotation_private_use_uncovered
    });
    let _ = writeln!(
        out,
        "| … in annotation appearances (fields, stamps, free text) | {in_annotations} | |"
    );
    let _ = writeln!(
        out,
        "| … in annotation appearances only, the page content being fine | {only_annotations} | |"
    );
    let _ = writeln!(
        out,
        "| A page with invisible text only (OCR layer) | {ocr} | |"
    );
    let _ = writeln!(
        out,
        "| Partly read (font hayro cannot load, page that failed) | {partial} | |\n"
    );

    // Fonts behind the failures, by number of documents.
    let mut fonts: BTreeMap<String, (usize, u64, u64)> = BTreeMap::new();
    for (_, text) in &docs {
        for font in &text.fonts {
            let missing = font.without_unicode + font.invalid_unicode;
            let pua = font.private_use - font.private_use_with_actual_text;
            if missing + pua == 0 {
                continue;
            }
            let entry = fonts.entry(strip_subset(&font.name)).or_default();
            entry.0 += 1;
            entry.1 += missing;
            entry.2 += pua;
        }
    }
    let mut fonts: Vec<_> = fonts.into_iter().collect();
    fonts.sort_by(|a, b| b.1.0.cmp(&a.1.0).then(a.0.cmp(&b.0)));
    if !fonts.is_empty() {
        let _ = writeln!(out, "## Fonts behind the failures\n");
        let _ = writeln!(
            out,
            "| Font | Documents | Glyphs without Unicode | Private Use glyphs without `ActualText` |\n|---|---|---|---|"
        );
        for (name, (documents, missing, pua)) in fonts.iter().take(25) {
            let _ = writeln!(out, "| `{name}` | {documents} | {missing} | {pua} |");
        }
        let _ = writeln!(out);
    }

    let failing: Vec<&String> = docs
        .iter()
        .filter(|(_, t)| t.fails_unicode_mapping() || t.fails_private_use())
        .map(|(sha, _)| *sha)
        .collect();
    if !failing.is_empty() {
        let _ = writeln!(out, "Documents failing (SHA-256 prefix):");
        for sha in failing.iter().take(40) {
            let _ = write!(out, " `{}`", &sha[..sha.len().min(16)]);
        }
        let _ = writeln!(out);
    }
    Ok(out)
}

/// `ABCDEF+Calibri` is `Calibri`.
fn strip_subset(name: &str) -> String {
    match name.split_once('+') {
        Some((prefix, rest))
            if prefix.len() == 6 && prefix.bytes().all(|b| b.is_ascii_uppercase()) =>
        {
            rest.to_owned()
        }
        _ => name.to_owned(),
    }
}
