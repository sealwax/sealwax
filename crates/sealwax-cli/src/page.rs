//! `sealwax glyphs` and `sealwax render`: one document's glyphs, and one of its pages as an image.

use std::fs::File;
use std::io::{BufWriter, Write as _};
use std::path::Path;

use sealwax::{Document, OpenOptions};
use sealwax_text::TextGlyph;
use serde::Serialize;

#[derive(Serialize)]
struct Line<'a> {
    /// The page, from 1.
    page: usize,
    #[serde(flatten)]
    glyph: &'a TextGlyph,
}

/// Write the glyphs of every page of `file` to `out` (standard output without one), one JSON
/// object per glyph and line.
///
/// # Errors
/// A message for what failed: opening, reading a page, writing.
pub(crate) fn glyphs(
    file: &Path,
    password: &str,
    annotations: bool,
    out: Option<&Path>,
) -> Result<(), String> {
    let doc = Document::open_path(file, &OpenOptions::default().password(password))
        .map_err(|error| format!("{}: {error}", file.display()))?;
    let sink: Box<dyn std::io::Write> = match out {
        Some(path) => {
            Box::new(File::create(path).map_err(|error| format!("{}: {error}", path.display()))?)
        }
        None => Box::new(std::io::stdout().lock()),
    };
    let mut sink = BufWriter::new(sink);
    for page in 0..doc.page_count() {
        let found = sealwax_text::glyphs(&doc, page, annotations)
            .map_err(|error| format!("page {}: {error}", page + 1))?;
        for glyph in &found {
            let line = Line {
                page: page + 1,
                glyph,
            };
            serde_json::to_writer(&mut sink, &line).map_err(|error| error.to_string())?;
            sink.write_all(b"\n").map_err(|error| error.to_string())?;
        }
    }
    sink.flush().map_err(|error| error.to_string())
}

/// Render page `page` (from 1) of `file` to a PNG image at `out`.
///
/// # Errors
/// A message for what failed: opening, rendering, writing.
pub(crate) fn render(
    file: &Path,
    password: &str,
    page: usize,
    scale: f64,
    out: &Path,
) -> Result<(), String> {
    let doc = Document::open_path(file, &OpenOptions::default().password(password))
        .map_err(|error| format!("{}: {error}", file.display()))?;
    let index = page.checked_sub(1).ok_or("pages are numbered from 1")?;
    let png = sealwax::raster::render_png(&doc, index, scale).map_err(|error| error.to_string())?;
    std::fs::write(out, png).map_err(|error| format!("{}: {error}", out.display()))
}
