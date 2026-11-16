//! `sealwax save`: save every PDF of a corpus, in full or incrementally, so that independent
//! tools can judge the outputs (`benchmark/judges/`): same rendering, valid structure, PDF/A
//! conformance kept.
//!
//! Outputs are written to `<out-dir>/<sha256 of the input>.pdf`, outside the repository. The
//! incremental mode rewrites the catalog and the first page unchanged, the objects a signature
//! revision touches, then appends them as an update.

use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::{Path, PathBuf};
use std::time::Instant;

use clap::ValueEnum;
use sealwax::{Document, Error, OpenOptions, SaveOptions, XrefFormat};
use serde::Serialize;

/// How each document is saved.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum Mode {
    /// Full save, cross-reference stream and object streams.
    Stream,
    /// Full save, cross-reference table.
    Table,
    /// Incremental update rewriting the catalog and the first page.
    Incremental,
    /// Optimized (`sealwax::optimize`): streams compressed, rewritten when smaller; the input
    /// itself when it is encrypted, signed, or not made smaller.
    Optimize,
    /// Rendered to a grayscale PDF at 200 dpi, as a fax (`sealwax_raster::grayscale`).
    Grayscale,
}

#[derive(Debug, Default, Serialize)]
struct Row {
    sha256: String,
    bytes: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    out_bytes: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<String>,
    seconds: f64,
}

fn save(doc: &mut Document, mode: Mode) -> Result<Vec<u8>, Error> {
    match mode {
        Mode::Stream => doc.save(&SaveOptions::default().xref(XrefFormat::Stream)),
        Mode::Table => doc.save(&SaveOptions::default().xref(XrefFormat::Table)),
        Mode::Incremental => {
            let first_page = doc.page_ids().first().copied().flatten();
            for id in [Some(doc.catalog_id()), first_page].into_iter().flatten() {
                if let Some(object) = doc.object(id)? {
                    doc.set_object(id, object);
                }
            }
            doc.save_incremental()
        }
        // Not reached: `one` optimizes or renders the bytes before opening them.
        Mode::Optimize | Mode::Grayscale => doc.save(&SaveOptions::default()),
    }
}

fn one(file: &Path, out_dir: &Path, mode: Mode) -> Row {
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
    let saved = catch_unwind(AssertUnwindSafe(|| {
        if mode == Mode::Optimize {
            return sealwax::optimize(&data, &OpenOptions::default()).map(|o| o.data);
        }
        if mode == Mode::Grayscale {
            return sealwax_raster::grayscale(
                &data,
                sealwax_raster::DEFAULT_DPI,
                &OpenOptions::default(),
            )
            .map_err(|error| match error {
                sealwax_raster::Error::Pdf(error) => error,
                _ => Error::Unsupported {
                    what: "rendering this document",
                },
            });
        }
        let mut doc = Document::open(data, &OpenOptions::default())?;
        save(&mut doc, mode)
    }));
    row.seconds = started.elapsed().as_secs_f64();
    match saved {
        Ok(Ok(bytes)) => {
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

/// Save every PDF under `paths` into `out_dir`, one JSON line each in `out`.
pub fn run(
    paths: &[PathBuf],
    out_dir: &Path,
    mode: Mode,
    out: &Path,
    jobs: usize,
) -> std::io::Result<usize> {
    std::fs::create_dir_all(out_dir)?;
    crate::corpus::each_to_jsonl(paths, out, jobs, |file| one(file, out_dir, mode))
}
