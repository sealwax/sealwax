//! What the corpus commands share: finding the PDFs, and one JSON line per document written from
//! several threads.

use std::fmt::Write as _;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use serde::Serialize;
use sha2::{Digest, Sha256};

/// Every `*.pdf` under the given files and directories, sorted.
pub fn pdfs(paths: &[PathBuf]) -> Vec<PathBuf> {
    fn walk(path: &Path, found: &mut Vec<PathBuf>) {
        if path.is_dir() {
            if let Ok(entries) = std::fs::read_dir(path) {
                for entry in entries.flatten() {
                    walk(&entry.path(), found);
                }
            }
        } else if path
            .extension()
            .is_some_and(|e| e.eq_ignore_ascii_case("pdf"))
        {
            found.push(path.to_path_buf());
        }
    }
    let mut found = Vec::new();
    for path in paths {
        walk(path, &mut found);
    }
    found.sort();
    found
}

/// Lowercase hexadecimal SHA-256: how the bench names documents.
pub fn sha256(data: &[u8]) -> String {
    let mut hex = String::with_capacity(64);
    for byte in Sha256::digest(data) {
        let _ = write!(hex, "{byte:02x}");
    }
    hex
}

/// Compute `row` for every PDF under `paths` with `jobs` threads, one JSON line each in `out`.
/// Returns how many documents were processed.
pub fn each_to_jsonl<T: Serialize>(
    paths: &[PathBuf],
    out: &Path,
    jobs: usize,
    row: impl Fn(&Path) -> T + Sync,
) -> std::io::Result<usize> {
    let files = pdfs(paths);
    let queue = Mutex::new(files.iter());
    let sink = Mutex::new(std::io::BufWriter::new(std::fs::File::create(out)?));
    std::thread::scope(|scope| {
        for _ in 0..jobs.max(1) {
            scope.spawn(|| {
                while let Some(file) = queue.lock().ok().and_then(|mut q| q.next()) {
                    let row = row(file);
                    if let (Ok(line), Ok(mut sink)) = (serde_json::to_string(&row), sink.lock()) {
                        let _ = writeln!(sink, "{line}");
                    }
                }
            });
        }
    });
    sink.lock()
        .map_err(|_| std::io::Error::other("poisoned"))?
        .flush()?;
    Ok(files.len())
}
