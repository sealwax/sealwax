//! `sealwax pdfa`: convert every PDF of a corpus to PDF/A-3, for veraPDF and the other judges of
//! `benchmark/judges/`.
//!
//! Outputs are written to `<out-dir>/<sha256 of the input>.pdf`, outside the repository, with one
//! JSON line per document: what the conversion changed and what it left.

use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::{Path, PathBuf};
use std::time::Instant;

use clap::ValueEnum;
use sealwax::{Document, Error, OpenOptions, SaveOptions};
use sealwax_pdfa::{Level, Options, Report, convert};
use serde::Serialize;

/// The conformance level to declare.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum Conformance {
    /// PDF/A-3b.
    B,
    /// PDF/A-3u.
    U,
    /// PDF/A-3a.
    A,
}

#[derive(Debug, Default, Serialize)]
struct Row {
    sha256: String,
    bytes: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    out_bytes: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    report: Option<Report>,
    seconds: f64,
}

fn one(file: &Path, out_dir: &Path, options: &Options) -> Row {
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
    let converted = catch_unwind(AssertUnwindSafe(|| {
        let mut doc = Document::open(data, &OpenOptions::default())?;
        let report = convert(&mut doc, options)?;
        Ok::<_, Error>((doc.save(&SaveOptions::default())?, report))
    }));
    row.seconds = started.elapsed().as_secs_f64();
    match converted {
        Ok(Ok((bytes, report))) => {
            row.report = Some(report);
            let target = out_dir.join(format!("{}.pdf", row.sha256));
            match std::fs::write(&target, &bytes) {
                Ok(()) => row.out_bytes = Some(bytes.len()),
                Err(error) => row.error = Some(error.to_string()),
            }
        }
        Ok(Err(Error::PasswordRequired)) => row.error = Some("password".into()),
        Ok(Err(Error::Malformed)) => row.error = Some("malformed".into()),
        Ok(Err(error)) => row.error = Some(error.to_string()),
        Err(_) => row.error = Some("panic".into()),
    }
    row
}

/// Convert every PDF under `paths` into `out_dir`, one JSON line each in `out`.
pub fn run(
    paths: &[PathBuf],
    out_dir: &Path,
    conformance: Conformance,
    language: Option<&str>,
    out: &Path,
    jobs: usize,
) -> std::io::Result<usize> {
    std::fs::create_dir_all(out_dir)?;
    let mut options = Options::new(match conformance {
        Conformance::B => Level::B,
        Conformance::U => Level::U,
        Conformance::A => Level::A,
    });
    if let Some(tag) = language {
        options = options.language(tag);
    }
    crate::corpus::each_to_jsonl(paths, out, jobs, |file| one(file, out_dir, &options))
}
