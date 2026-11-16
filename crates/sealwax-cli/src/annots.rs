//! `sealwax flatten` and `sealwax xfdf`: annotations flattened, and the XFDF of a signing page
//! applied, on every PDF of a corpus, for the judges of `benchmark/judges/`.
//!
//! Outputs are written to `<out-dir>/<sha256 of the input>.pdf`, outside the repository, with one
//! JSON line per document. With `--pdfa`, the output is then converted to PDF/A-3.

use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::{Path, PathBuf};
use std::time::Instant;

use sealwax::{Document, OpenOptions, SaveOptions};
use sealwax_annots::{Applied, Flattened};
use sealwax_pdfa::{Level, Options, Report};
use serde::Serialize;

use crate::pdfa::Conformance;

#[derive(Debug, Default, Serialize)]
struct Row {
    sha256: String,
    bytes: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    out_bytes: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    flattened: Option<Flattened>,
    #[serde(skip_serializing_if = "Option::is_none")]
    applied: Option<Applied>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pdfa: Option<Report>,
    seconds: f64,
}

/// What is done to each document.
#[derive(Debug, Clone, Copy)]
enum Task<'a> {
    Flatten,
    Xfdf(&'a Path),
}

fn one(file: &Path, out_dir: &Path, task: Task<'_>, pdfa: Option<&Options>) -> Row {
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
    let xfdf = match task {
        Task::Flatten => None,
        Task::Xfdf(dir) => {
            match std::fs::read_to_string(dir.join(format!("{}.xfdf", row.sha256))) {
                Ok(text) => Some(text),
                Err(_) => {
                    row.error = Some("no XFDF".into());
                    return row;
                }
            }
        }
    };
    let started = Instant::now();
    let done = catch_unwind(AssertUnwindSafe(|| {
        let mut doc = Document::open(data, &OpenOptions::default()).map_err(|e| e.to_string())?;
        let mut flattened = None;
        let mut applied = None;
        match &xfdf {
            None => flattened = Some(sealwax_annots::flatten(&mut doc).map_err(|e| e.to_string())?),
            Some(text) => {
                applied = Some(
                    sealwax_annots::apply_xfdf(&mut doc, text, true).map_err(|e| e.to_string())?,
                );
            }
        }
        let mut bytes = doc
            .save(&SaveOptions::default())
            .map_err(|e| e.to_string())?;
        // The conversion reads the pages as opened: what was drawn is seen once saved.
        let report = match pdfa {
            Some(options) => {
                let mut reopened =
                    Document::open(bytes, &OpenOptions::default()).map_err(|e| e.to_string())?;
                let report =
                    sealwax_pdfa::convert(&mut reopened, options).map_err(|e| e.to_string())?;
                bytes = reopened
                    .save(&SaveOptions::default())
                    .map_err(|e| e.to_string())?;
                Some(report)
            }
            None => None,
        };
        Ok::<_, String>((bytes, flattened, applied, report))
    }));
    row.seconds = started.elapsed().as_secs_f64();
    match done {
        Ok(Ok((bytes, flattened, applied, report))) => {
            row.flattened = flattened;
            row.applied = applied;
            row.pdfa = report;
            let target = out_dir.join(format!("{}.pdf", row.sha256));
            match std::fs::write(&target, &bytes) {
                Ok(()) => row.out_bytes = Some(bytes.len()),
                Err(error) => row.error = Some(error.to_string()),
            }
        }
        Ok(Err(error)) => row.error = Some(error),
        Err(_) => row.error = Some("panic".into()),
    }
    row
}

fn options(conformance: Option<Conformance>) -> Option<Options> {
    conformance.map(|c| {
        Options::new(match c {
            Conformance::B => Level::B,
            Conformance::U => Level::U,
            Conformance::A => Level::A,
        })
    })
}

/// Flatten every PDF under `paths` into `out_dir`, one JSON line each in `out`.
pub fn run_flatten(
    paths: &[PathBuf],
    out_dir: &Path,
    pdfa: Option<Conformance>,
    out: &Path,
    jobs: usize,
) -> std::io::Result<usize> {
    std::fs::create_dir_all(out_dir)?;
    let options = options(pdfa);
    crate::corpus::each_to_jsonl(paths, out, jobs, |file| {
        one(file, out_dir, Task::Flatten, options.as_ref())
    })
}

/// Apply to every PDF under `paths` the XFDF `<xfdf-dir>/<sha256 of the PDF>.xfdf`, flattening
/// first, into `out_dir`, one JSON line each in `out`.
pub fn run_xfdf(
    paths: &[PathBuf],
    xfdf_dir: &Path,
    out_dir: &Path,
    pdfa: Option<Conformance>,
    out: &Path,
    jobs: usize,
) -> std::io::Result<usize> {
    std::fs::create_dir_all(out_dir)?;
    let options = options(pdfa);
    crate::corpus::each_to_jsonl(paths, out, jobs, |file| {
        one(file, out_dir, Task::Xfdf(xfdf_dir), options.as_ref())
    })
}

/// Merge `inputs` into `out`, each flattened first; returns the pages merged.
pub fn merge(inputs: &[PathBuf], out: &Path) -> Result<usize, String> {
    let mut sources = Vec::with_capacity(inputs.len());
    for (index, input) in inputs.iter().enumerate() {
        let data = std::fs::read(input).map_err(|e| format!("source {}: {e}", index + 1))?;
        sources.push(
            Document::open(data, &OpenOptions::default())
                .map_err(|e| format!("source {}: {e}", index + 1))?,
        );
    }
    let (merged, _) = sealwax_annots::merge(sources).map_err(|e| e.to_string())?;
    let bytes = merged
        .save(&SaveOptions::default())
        .map_err(|e| e.to_string())?;
    std::fs::write(out, bytes).map_err(|e| e.to_string())?;
    Ok(merged.page_count())
}
