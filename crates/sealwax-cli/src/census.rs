//! `sealwax census`: survey a corpus, one JSON line per document; `census-report`: aggregate them.

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::io::BufRead;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::{Path, PathBuf};

use sealwax::{Census, Document, Error, OpenOptions};
use serde::{Deserialize, Serialize};

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "lowercase", tag = "status")]
pub enum Outcome {
    Ok { census: Box<Census> },
    Password,
    Malformed,
    Error { error: String },
    Panic,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct Row {
    pub file: String,
    pub bytes: u64,
    #[serde(flatten)]
    pub outcome: Outcome,
}

fn survey(file: &Path) -> Outcome {
    let result = catch_unwind(AssertUnwindSafe(|| {
        Document::open_path(file, &OpenOptions::default()).map(|doc| {
            let mut census = doc.census();
            census.peaks = doc.measure_pages().ok();
            census
        })
    }));
    match result {
        Ok(Ok(census)) => Outcome::Ok {
            census: Box::new(census),
        },
        Ok(Err(Error::PasswordRequired)) => Outcome::Password,
        Ok(Err(Error::Malformed)) => Outcome::Malformed,
        Ok(Err(error)) => Outcome::Error {
            error: error.to_string(),
        },
        Err(_) => Outcome::Panic,
    }
}

/// Survey every PDF under `paths` with `jobs` threads, writing JSON lines to `out`.
pub fn run(paths: &[PathBuf], out: &Path, jobs: usize) -> std::io::Result<usize> {
    crate::corpus::each_to_jsonl(paths, out, jobs, |file| Row {
        file: file.display().to_string(),
        bytes: std::fs::metadata(file).map(|m| m.len()).unwrap_or(0),
        outcome: survey(file),
    })
}

fn pct(part: usize, whole: usize) -> String {
    format!(
        "{part} ({:.1} %)",
        100.0 * part as f64 / whole.max(1) as f64
    )
}

fn top<K: Ord + Clone + std::fmt::Display>(map: &BTreeMap<K, usize>, n: usize) -> Vec<(K, usize)> {
    let mut items: Vec<(K, usize)> = map.iter().map(|(k, v)| (k.clone(), *v)).collect();
    items.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    items.truncate(n);
    items
}

/// Markdown report of a census run.
pub fn report(results: &Path) -> std::io::Result<String> {
    let rows: Vec<Row> = std::io::BufReader::new(std::fs::File::open(results)?)
        .lines()
        .map_while(Result::ok)
        .filter_map(|l| serde_json::from_str(&l).ok())
        .collect();
    let docs: Vec<&Census> = rows
        .iter()
        .filter_map(|r| match &r.outcome {
            Outcome::Ok { census } => Some(census.as_ref()),
            _ => None,
        })
        .collect();
    let n = docs.len();
    let mut out = String::new();
    let count = |f: fn(&Outcome) -> bool| rows.iter().filter(|r| f(&r.outcome)).count();
    let _ = writeln!(out, "# Corpus census\n");
    let _ = writeln!(
        out,
        "| Documents | Opened | Password required | Malformed | Other error | Panic |\n|---|---|---|---|---|---|"
    );
    let _ = writeln!(
        out,
        "| {} | {} | {} | {} | {} | {} |\n",
        rows.len(),
        pct(n, rows.len()),
        count(|o| matches!(o, Outcome::Password)),
        count(|o| matches!(o, Outcome::Malformed)),
        count(|o| matches!(o, Outcome::Error { .. })),
        count(|o| matches!(o, Outcome::Panic)),
    );
    let pages: usize = docs.iter().map(|d| d.pages).sum();
    let _ = writeln!(out, "Pages: {pages}.\n");

    // Point 1: tagging, PDF/A.
    let _ = writeln!(out, "## Tagging and PDF/A (complex point 1)\n");
    let _ = writeln!(
        out,
        "| Tagged (`Marked` + `StructTreeRoot`) | `/Lang` | Claims PDF/A |\n|---|---|---|"
    );
    let _ = writeln!(
        out,
        "| {} | {} | {} |\n",
        pct(
            docs.iter()
                .filter(|d| d.tagging.marked && d.tagging.struct_tree)
                .count(),
            n
        ),
        pct(docs.iter().filter(|d| d.tagging.lang).count(), n),
        pct(docs.iter().filter(|d| d.pdfa_claim.is_some()).count(), n),
    );
    let mut claims: BTreeMap<String, usize> = BTreeMap::new();
    for d in &docs {
        if let Some(c) = &d.pdfa_claim {
            *claims.entry(c.clone()).or_default() += 1;
        }
    }
    if !claims.is_empty() {
        let _ = writeln!(
            out,
            "PDF/A claims: {}\n",
            top(&claims, 10)
                .iter()
                .map(|(k, v)| format!("{k}: {v}"))
                .collect::<Vec<_>>()
                .join(", ")
        );
    }

    // Point 2: fonts.
    let fonts: Vec<_> = docs.iter().flat_map(|d| d.fonts.iter()).collect();
    let f = fonts.len();
    let _ = writeln!(out, "## Fonts (complex point 2)\n");
    let _ = writeln!(
        out,
        "| Distinct fonts (per document) | Not embedded | of which standard 14 | Embedded without `ToUnicode` | Not embedded without `ToUnicode` | Symbolic | Type 3 |\n|---|---|---|---|---|---|---|"
    );
    let _ = writeln!(
        out,
        "| {f} | {} | {} | {} | {} | {} | {} |\n",
        pct(fonts.iter().filter(|x| !x.embedded).count(), f),
        pct(fonts.iter().filter(|x| x.standard_14).count(), f),
        pct(
            fonts.iter().filter(|x| x.embedded && !x.to_unicode).count(),
            f
        ),
        pct(
            fonts
                .iter()
                .filter(|x| !x.embedded && !x.to_unicode)
                .count(),
            f
        ),
        pct(fonts.iter().filter(|x| x.symbolic).count(), f),
        pct(fonts.iter().filter(|x| x.subtype == "Type3").count(), f),
    );
    let _ = writeln!(
        out,
        "Documents with at least one font not embedded: {}\n",
        pct(
            docs.iter()
                .filter(|d| d.fonts.iter().any(|x| !x.embedded))
                .count(),
            n
        )
    );
    let mut missing: BTreeMap<String, usize> = BTreeMap::new();
    let mut subtypes: BTreeMap<String, usize> = BTreeMap::new();
    for font in &fonts {
        *subtypes.entry(font.subtype.clone()).or_default() += 1;
        if !font.embedded {
            *missing.entry(font.base_font.clone()).or_default() += 1;
        }
    }
    let _ = writeln!(
        out,
        "By type: {}\n",
        top(&subtypes, 10)
            .iter()
            .map(|(k, v)| format!("{k}: {v}"))
            .collect::<Vec<_>>()
            .join(", ")
    );
    if !missing.is_empty() {
        let _ = writeln!(
            out,
            "Most frequent fonts not embedded:\n\n| Font | Documents |\n|---|---|"
        );
        for (font, c) in top(&missing, 15) {
            let _ = writeln!(out, "| `{font}` | {c} |");
        }
        let _ = writeln!(out);
    }

    // Point 4: forms and annotations.
    let _ = writeln!(out, "## Forms and annotations (complex point 4)\n");
    let _ = writeln!(
        out,
        "| AcroForm | `NeedAppearances` | XFA | Signed signature fields | Empty signature fields |\n|---|---|---|---|---|"
    );
    let _ = writeln!(
        out,
        "| {} | {} | {} | {} | {} |\n",
        pct(docs.iter().filter(|d| d.form.acroform).count(), n),
        pct(docs.iter().filter(|d| d.form.need_appearances).count(), n),
        pct(docs.iter().filter(|d| d.form.xfa).count(), n),
        docs.iter()
            .map(|d| d.form.signed_signature_fields)
            .sum::<usize>(),
        docs.iter()
            .map(|d| d.form.empty_signature_fields)
            .sum::<usize>(),
    );
    let mut annots: BTreeMap<String, (usize, usize)> = BTreeMap::new();
    for d in &docs {
        for (k, (total, no_ap)) in &d.annotations {
            let e = annots.entry(k.clone()).or_default();
            e.0 += total;
            e.1 += no_ap;
        }
    }
    if !annots.is_empty() {
        let _ = writeln!(
            out,
            "| Annotation | Count | Without appearance |\n|---|---|---|"
        );
        let mut items: Vec<_> = annots.into_iter().collect();
        items.sort_by_key(|item| std::cmp::Reverse(item.1.0));
        for (k, (total, no_ap)) in items {
            let _ = writeln!(out, "| {k} | {total} | {} |", pct(no_ap, total));
        }
        let _ = writeln!(out);
    }

    // Point 3 and 6: structure.
    let _ = writeln!(out, "## Structure, encryption, images\n");
    let _ = writeln!(
        out,
        "| Encrypted (opened without password) | Several revisions | Shared Form XObjects (documents) | Pages without MediaBox | Pages with UserUnit ≠ 1 |\n|---|---|---|---|---|"
    );
    let _ = writeln!(
        out,
        "| {} | {} | {} | {} | {} |\n",
        pct(docs.iter().filter(|d| d.encrypted).count(), n),
        pct(docs.iter().filter(|d| d.revisions > 1).count(), n),
        pct(
            docs.iter().filter(|d| d.shared_form_xobjects > 0).count(),
            n
        ),
        docs.iter()
            .map(|d| d.page_facts.missing_media_box)
            .sum::<usize>(),
        docs.iter()
            .map(|d| d.page_facts.user_unit_not_1)
            .sum::<usize>(),
    );
    let mut images: BTreeMap<String, usize> = BTreeMap::new();
    let mut sizes: BTreeMap<String, usize> = BTreeMap::new();
    let mut rotations: BTreeMap<String, usize> = BTreeMap::new();
    let mut producers: BTreeMap<String, usize> = BTreeMap::new();
    for d in &docs {
        for (k, v) in &d.images {
            *images.entry(k.clone()).or_default() += v;
        }
        for (k, v) in &d.page_facts.sizes {
            *sizes.entry(k.clone()).or_default() += v;
        }
        for (k, v) in &d.page_facts.rotations {
            *rotations.entry(k.clone()).or_default() += v;
        }
        let producer = d.producer.clone().unwrap_or_else(|| "(none)".into());
        *producers.entry(producer).or_default() += 1;
    }
    let join = |items: Vec<(String, usize)>| {
        items
            .iter()
            .map(|(k, v)| format!("{k}: {v}"))
            .collect::<Vec<_>>()
            .join(", ")
    };
    let _ = writeln!(out, "Images by filter: {}\n", join(top(&images, 10)));
    let _ = writeln!(out, "Page sizes: {}\n", join(top(&sizes, 5)));
    let _ = writeln!(
        out,
        "Page rotations: {}\n",
        join(
            top(&rotations, 4)
                .into_iter()
                .map(|(k, v)| (format!("{k}°"), v))
                .collect()
        )
    );
    let _ = writeln!(out, "## Producers\n\n| Producer | Documents |\n|---|---|");
    for (producer, c) in top(&producers, 15) {
        let _ = writeln!(out, "| {} | {c} |", producer.replace('|', "/"));
    }
    Ok(out)
}
