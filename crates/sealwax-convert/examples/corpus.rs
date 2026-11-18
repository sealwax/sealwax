//! Converts every fixture of docxide-pdf's corpus twice, with docxide's own PDF export and laid
//! out by docxide then written by sealwax-compose, and compares the two documents page by page:
//! page counts, ink at 150 dpi (Jaccard of the pixels darker than luma 200, docxide's metric)
//! and the text read back.
//!
//! Usage: `corpus <fixtures-root> <out-dir> [name-filter]`; writes `cases.csv`, `pages.csv` and
//! `summary.md` to `<out-dir>`, and both PDFs of every page under 98 to `<out-dir>/pdfs`.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Instant;

use sealwax_convert::DocxOptions;
use sealwax_core::{Document, Limits, OpenOptions, SaveOptions};

const SCALE: f64 = 150.0 / 72.0;

struct PageResult {
    jaccard: f64,
    text_equal: bool,
    text_equal_without_spaces: bool,
}

#[derive(Default)]
struct CaseResult {
    group: String,
    name: String,
    native_pages: usize,
    sealwax_pages: usize,
    pages: Vec<PageResult>,
    native_ms: f64,
    sealwax_ms: f64,
    not_described: BTreeMap<String, usize>,
    skipped: BTreeMap<String, usize>,
    error: Option<String>,
}

fn cases(root: &Path, filter: Option<&str>) -> Vec<(String, String, PathBuf)> {
    let mut found = Vec::new();
    for group in ["cases", "scraped"] {
        let Ok(dirs) = std::fs::read_dir(root.join(group)) else {
            continue;
        };
        for dir in dirs.flatten() {
            let input = dir.path().join("input.docx");
            let name = dir.file_name().to_string_lossy().into_owned();
            if input.is_file() && filter.is_none_or(|f| name.contains(f)) {
                found.push((group.to_owned(), name, input));
            }
        }
    }
    found.sort();
    found
}

fn open(bytes: Vec<u8>) -> Document {
    Document::open(bytes, &OpenOptions::default().limits(Limits::unbounded())).unwrap()
}

/// The pixels darker than luma 200, row by row.
fn ink(doc: &Document, page: usize) -> Option<(u32, u32, Vec<bool>)> {
    let options = sealwax_raster::RenderOptions::new(SCALE);
    let raster = sealwax_raster::render(doc, page, &options, &|| false).ok()??;
    let ink = raster
        .pixels
        .chunks(4)
        .map(|p| {
            0.299 * f64::from(p[0]) + 0.587 * f64::from(p[1]) + 0.114 * f64::from(p[2]) < 200.0
        })
        .collect();
    Some((raster.width, raster.height, ink))
}

fn jaccard(a: &(u32, u32, Vec<bool>), b: &(u32, u32, Vec<bool>)) -> f64 {
    if (a.0, a.1) != (b.0, b.1) {
        return 0.0;
    }
    let (mut both, mut either) = (0usize, 0usize);
    for (x, y) in a.2.iter().zip(&b.2) {
        both += usize::from(*x && *y);
        either += usize::from(*x || *y);
    }
    if either == 0 {
        100.0
    } else {
        100.0 * both as f64 / either as f64
    }
}

fn text(doc: &Document, page: usize) -> String {
    sealwax_text::glyphs(doc, page, false)
        .map(|glyphs| glyphs.into_iter().filter_map(|g| g.text).collect())
        .unwrap_or_default()
}

fn run_case(group: &str, name: &str, input: &Path, out: &Path) -> CaseResult {
    let mut result = CaseResult {
        group: group.to_owned(),
        name: name.to_owned(),
        ..CaseResult::default()
    };
    let docx = std::fs::read(input).unwrap();
    let options = DocxOptions::default().font_cache(false);

    let start = Instant::now();
    let native = match docxide_pdf::convert_docx_bytes_to_pdf_bytes_with(&docx, &options) {
        Ok(bytes) => bytes,
        Err(e) => {
            result.error = Some(format!("native: {e}"));
            return result;
        }
    };
    result.native_ms = start.elapsed().as_secs_f64() * 1000.0;

    let start = Instant::now();
    let converted = match sealwax_convert::docx_to_pdf(&docx, &options, &Limits::unbounded()) {
        Ok(converted) => converted,
        Err(e) => {
            result.error = Some(format!("sealwax: {e}"));
            return result;
        }
    };
    let composed = converted
        .composed
        .document
        .save(&SaveOptions::default())
        .unwrap();
    result.sealwax_ms = start.elapsed().as_secs_f64() * 1000.0;
    result.not_described = converted
        .not_described
        .iter()
        .map(|(k, v)| ((*k).to_owned(), *v))
        .collect();
    result.skipped = converted
        .composed
        .skipped
        .iter()
        .map(|(k, v)| (k.to_owned(), v))
        .collect();

    let (native_doc, sealwax_doc) = (open(native.clone()), open(composed.clone()));
    result.native_pages = native_doc.page_count();
    result.sealwax_pages = sealwax_doc.page_count();
    let mut keep = false;
    for page in 0..result.native_pages.min(result.sealwax_pages) {
        let score = match (ink(&native_doc, page), ink(&sealwax_doc, page)) {
            (Some(a), Some(b)) => jaccard(&a, &b),
            _ => 0.0,
        };
        let (a, b) = (text(&native_doc, page), text(&sealwax_doc, page));
        let squeeze = |s: &str| s.chars().filter(|c| !c.is_whitespace()).collect::<String>();
        keep |= score < 98.0;
        result.pages.push(PageResult {
            jaccard: score,
            text_equal: a == b,
            text_equal_without_spaces: squeeze(&a) == squeeze(&b),
        });
    }
    if keep || result.native_pages != result.sealwax_pages {
        let dir = out.join("pdfs").join(group).join(name);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("native.pdf"), &native).unwrap();
        std::fs::write(dir.join("sealwax.pdf"), &composed).unwrap();
    }
    result
}

fn percent(part: usize, whole: usize) -> f64 {
    if whole == 0 {
        0.0
    } else {
        100.0 * part as f64 / whole as f64
    }
}

fn summary(results: &[CaseResult]) -> String {
    let mut out = String::from("# docxide → sealwax-compose against docxide's own PDF\n\n");
    for group in ["cases", "scraped", "all"] {
        let cases: Vec<&CaseResult> = results
            .iter()
            .filter(|r| group == "all" || r.group == group)
            .collect();
        let pages: Vec<&PageResult> = cases.iter().flat_map(|r| &r.pages).collect();
        let mut scores: Vec<f64> = pages.iter().map(|p| p.jaccard).collect();
        scores.sort_by(f64::total_cmp);
        let median = scores.get(scores.len() / 2).copied().unwrap_or(0.0);
        let mean = scores.iter().sum::<f64>() / scores.len().max(1) as f64;
        let at_least = |t: f64| percent(scores.iter().filter(|s| **s >= t).count(), scores.len());
        let errors = cases.iter().filter(|r| r.error.is_some()).count();
        let page_mismatch = cases
            .iter()
            .filter(|r| r.error.is_none() && r.native_pages != r.sealwax_pages)
            .count();
        let text = percent(pages.iter().filter(|p| p.text_equal).count(), pages.len());
        let text_ns = percent(
            pages.iter().filter(|p| p.text_equal_without_spaces).count(),
            pages.len(),
        );
        let native_ms: f64 = cases.iter().map(|r| r.native_ms).sum();
        let sealwax_ms: f64 = cases.iter().map(|r| r.sealwax_ms).sum();
        let _ = writeln!(out, "## {group}\n");
        let _ = writeln!(out, "| Measure | Value |\n|---|---|");
        let _ = writeln!(out, "| Documents | {} |", cases.len());
        let _ = writeln!(out, "| Conversion errors | {errors} |");
        let _ = writeln!(out, "| Page count differs | {page_mismatch} |");
        let _ = writeln!(out, "| Pages compared | {} |", pages.len());
        let _ = writeln!(out, "| Jaccard mean / median | {mean:.2} / {median:.2} |");
        let _ = writeln!(out, "| Pages ≥ 99.5 | {:.1}% |", at_least(99.5));
        let _ = writeln!(out, "| Pages ≥ 98 | {:.1}% |", at_least(98.0));
        let _ = writeln!(out, "| Pages ≥ 90 | {:.1}% |", at_least(90.0));
        let _ = writeln!(out, "| Text identical | {text:.1}% |");
        let _ = writeln!(out, "| Text identical, whitespace aside | {text_ns:.1}% |");
        let _ = writeln!(
            out,
            "| Time native / sealwax (layout + compose + save) | {:.0} ms / {:.0} ms |\n",
            native_ms, sealwax_ms
        );
        let mut not_described: BTreeMap<&str, (usize, usize)> = BTreeMap::new();
        for r in &cases {
            for (k, v) in r.not_described.iter().chain(&r.skipped) {
                let e = not_described.entry(k.as_str()).or_default();
                e.0 += 1;
                e.1 += v;
            }
        }
        if !not_described.is_empty() {
            let _ = writeln!(out, "| Left out | Documents | Occurrences |\n|---|---|---|");
            for (k, (docs, n)) in not_described {
                let _ = writeln!(out, "| {k} | {docs} | {n} |");
            }
            out.push('\n');
        }
    }
    let mut worst: Vec<(&CaseResult, usize, f64)> = results
        .iter()
        .flat_map(|r| {
            r.pages
                .iter()
                .enumerate()
                .map(move |(i, p)| (r, i, p.jaccard))
        })
        .collect();
    worst.sort_by(|a, b| a.2.total_cmp(&b.2));
    let _ = writeln!(
        out,
        "## Lowest pages\n\n| Case | Page | Jaccard | Left out |\n|---|---|---|---|"
    );
    for (r, page, score) in worst.into_iter().take(30) {
        let left: Vec<String> = r
            .not_described
            .iter()
            .chain(&r.skipped)
            .map(|(k, v)| format!("{k} ×{v}"))
            .collect();
        let _ = writeln!(
            out,
            "| {}/{} | {} | {score:.2} | {} |",
            r.group,
            r.name,
            page + 1,
            left.join(", ")
        );
    }
    let errors: Vec<&CaseResult> = results.iter().filter(|r| r.error.is_some()).collect();
    if !errors.is_empty() {
        let _ = writeln!(out, "\n## Errors\n");
        for r in errors {
            let _ = writeln!(
                out,
                "- {}/{}: {}",
                r.group,
                r.name,
                r.error.as_deref().unwrap_or("")
            );
        }
    }
    out
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let (root, out) = (PathBuf::from(&args[1]), PathBuf::from(&args[2]));
    let filter = args.get(3).map(String::as_str);
    std::fs::create_dir_all(&out).unwrap();
    let cases = cases(&root, filter);
    eprintln!("{} documents", cases.len());
    let next = AtomicUsize::new(0);
    let results = Mutex::new(Vec::new());
    let threads = std::thread::available_parallelism().map_or(4, |n| n.get().min(8));
    std::thread::scope(|scope| {
        for _ in 0..threads {
            scope.spawn(|| {
                loop {
                    let i = next.fetch_add(1, Ordering::Relaxed);
                    let Some((group, name, input)) = cases.get(i) else {
                        break;
                    };
                    let result = std::panic::catch_unwind(|| run_case(group, name, input, &out))
                        .unwrap_or_else(|_| CaseResult {
                            group: group.clone(),
                            name: name.clone(),
                            error: Some("panic".to_owned()),
                            ..CaseResult::default()
                        });
                    results.lock().unwrap().push(result);
                    if i.is_multiple_of(100) {
                        eprintln!("{i}");
                    }
                }
            });
        }
    });
    let mut results = results.into_inner().unwrap();
    results.sort_by(|a, b| (&a.group, &a.name).cmp(&(&b.group, &b.name)));

    let mut cases_csv = String::from(
        "group,case,native_pages,sealwax_pages,min_jaccard,native_ms,sealwax_ms,left_out,error\n",
    );
    let mut pages_csv =
        String::from("group,case,page,jaccard,text_equal,text_equal_without_spaces\n");
    for r in &results {
        let min = r
            .pages
            .iter()
            .map(|p| p.jaccard)
            .fold(f64::INFINITY, f64::min);
        let left: Vec<String> = r
            .not_described
            .iter()
            .chain(&r.skipped)
            .map(|(k, v)| format!("{k}={v}"))
            .collect();
        let _ = writeln!(
            cases_csv,
            "{},{},{},{},{:.2},{:.1},{:.1},\"{}\",\"{}\"",
            r.group,
            r.name,
            r.native_pages,
            r.sealwax_pages,
            if min.is_finite() { min } else { 0.0 },
            r.native_ms,
            r.sealwax_ms,
            left.join("; "),
            r.error.as_deref().unwrap_or("")
        );
        for (i, p) in r.pages.iter().enumerate() {
            let _ = writeln!(
                pages_csv,
                "{},{},{},{:.3},{},{}",
                r.group,
                r.name,
                i + 1,
                p.jaccard,
                p.text_equal,
                p.text_equal_without_spaces
            );
        }
    }
    std::fs::write(out.join("cases.csv"), cases_csv).unwrap();
    std::fs::write(out.join("pages.csv"), pages_csv).unwrap();
    let summary = summary(&results);
    std::fs::write(out.join("summary.md"), &summary).unwrap();
    println!("{summary}");
}
