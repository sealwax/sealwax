//! `sealwax pages`: page count and geometry of every PDF of a corpus, one JSON line per document,
//! to compare them with what other readers report.

use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::{Path, PathBuf};
use std::time::Instant;

use sealwax::{Document, Error, OpenOptions, PageGeometry};
use serde::Serialize;

#[derive(Debug, Default, Serialize)]
struct Row {
    sha256: String,
    bytes: usize,
    engine: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    count: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    count_error: Option<String>,
    /// `/Count` of the page tree's root, when it disagrees with `count`.
    #[serde(skip_serializing_if = "Option::is_none")]
    declared_count: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pages: Option<Vec<Page>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pages_error: Option<String>,
    seconds: f64,
}

#[derive(Debug, Serialize)]
struct Page {
    media: [f64; 4],
    crop: [f64; 4],
    rotation: u16,
    user_unit: f64,
}

impl From<PageGeometry> for Page {
    fn from(page: PageGeometry) -> Self {
        let corners = |r: sealwax::Rect| [r.x1, r.y1, r.x2, r.y2];
        Self {
            media: corners(page.media_box),
            crop: corners(page.crop_box),
            rotation: page.rotation.degrees(),
            user_unit: page.user_unit,
        }
    }
}

fn measure(file: &Path) -> Row {
    let mut row = Row {
        engine: format!("sealwax {}", env!("CARGO_PKG_VERSION")),
        ..Row::default()
    };
    let data = match std::fs::read(file) {
        Ok(data) => data,
        Err(error) => {
            row.count_error = Some(error.to_string());
            row.pages_error = row.count_error.clone();
            return row;
        }
    };
    row.sha256 = crate::corpus::sha256(&data);
    row.bytes = data.len();
    let started = Instant::now();
    let opened = catch_unwind(AssertUnwindSafe(|| {
        Document::open(data, &OpenOptions::default())
            .map(|doc| (doc.page_count(), doc.declared_page_count(), doc.geometry()))
    }));
    row.seconds = started.elapsed().as_secs_f64();
    let error = match opened {
        Ok(Ok((count, declared, geometry))) => {
            row.count = Some(count);
            row.declared_count = declared.filter(|d| usize::try_from(*d).ok() != Some(count));
            if count == 0 {
                row.pages_error = Some("no_pages".into());
            } else {
                row.pages = Some(geometry.into_iter().map(Page::from).collect());
            }
            return row;
        }
        Ok(Err(Error::PasswordRequired)) => "password".to_owned(),
        Ok(Err(Error::Malformed)) => "malformed".to_owned(),
        Ok(Err(error)) => error.to_string(),
        Err(_) => "panic".to_owned(),
    };
    row.count_error = Some(error.clone());
    row.pages_error = Some(error);
    row
}

/// Measure every PDF under `paths` with `jobs` threads, one JSON line each in `out`.
pub fn run(paths: &[PathBuf], out: &Path, jobs: usize) -> std::io::Result<usize> {
    crate::corpus::each_to_jsonl(paths, out, jobs, measure)
}
