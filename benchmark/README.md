# Benchmarks

Tools that check sealwax against independent judges and hostile inputs. They work on corpora you
fetch yourself: no document is committed to this repository.

## Corpora

Public corpora that suit the judges, cloned under `corpus/public/` (ignored by git):

```sh
git clone --depth 1 --filter=blob:none --sparse https://github.com/mozilla/pdf.js corpus/public/pdfjs
git -C corpus/public/pdfjs sparse-checkout set test/pdfs
git clone --depth 1 https://github.com/pdf-association/safedocs corpus/public/safedocs
git clone --depth 1 https://github.com/veraPDF/veraPDF-corpus corpus/public/verapdf
```

Outputs are named by the SHA-256 of their input, and reports name documents the same way.

## Hostile inputs (`robustness/`)

`hostile.py` builds, from scratch, PDFs and XFDF meant to break a PDF library:
- decompression bombs in every kind of stream;
- operator, path, text and mesh floods;
- sizes read from the file;
- cycles and exponential fan-outs in every tree and graph;
- deep nesting, and malicious XML.

`run.py` runs each `sealwax` command on each of them in a process of its own, kills it past a time
or memory limit, and reports what happened: done, refused with an error, or killed.

```sh
cargo build --release -p sealwax-cli
python3 -I benchmark/robustness/hostile.py /tmp/hostile
cd benchmark/robustness
uv run --no-project --with psutil python -I run.py /tmp/hostile --sealwax ../../target/release/sealwax --out results.jsonl
uv run --no-project --with psutil python -I run.py report results.jsonl
```

`--corpus DIR` runs the same commands on real documents instead, to check that the limits refuse
none of them. The signing operation uses the judges' signer: set up `judges/` first
(`uv sync` there).

## Judges (`judges/`)

Each judge reads what a `sealwax` command wrote and checks it with another implementation (`uv
sync` installs them):

| Judge | Checks |
|---|---|
| `render.py` | PDFium renders every page of the output as the input's, and extracts the same text at the same places |
| `structure.py` | qpdf opens the output without recovery and finds no new problem |
| `sign.py` | pyHanko validates signatures appended to the output, and sealwax's own signatures, timestamps and validation data |
| `reading_order.py` | Tagging compared with the structure the producers of tagged documents gave them |

Each script's docstring shows how to run it.

## Comparisons (`text/`, `raster/`, `speed/`)

Measures of sealwax against other libraries on a corpus, numbers only (no text from the
documents), run as outside tools in an environment of their own:

| Script | Compares |
|---|---|
| `text/compare_pymupdf.py` | The text of each page's glyphs, read as a reader reads it, with PyMuPDF's `get_text()` |
| `text/compare_drawings_pymupdf.py` | The lines and rectangles of each page with the `l` and `re` items of PyMuPDF's `get_drawings()` |
| `raster/compare_pdfium.py` | Each page rendered to PNG with PDFium's rendering, in blocks of 4 x 4 pixels |
| `speed/compare_pymupdf.py` | Milliseconds per page of opening, text, drawings, rendering and fields, with PyMuPDF's |

Build the bindings in release mode for `speed/` (`maturin build --release`, or `maturin develop
--release`): a debug build is twenty to sixty times slower.

## PDF/A (`verapdf/`)

`run.sh` validates a directory with [veraPDF](https://verapdf.org), in its official Docker image
and without network; `summarize.py` turns its JSON report into a Markdown summary.

```sh
benchmark/verapdf/run.sh OUT_DIR report.json 3a
python3 -I benchmark/verapdf/summarize.py report.json
```
