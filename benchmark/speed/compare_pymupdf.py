"""How fast sealwax reads pages next to PyMuPDF: milliseconds per page, and pages per second, for
each operation, in one thread, over the first pages of each document.

    python -I compare_pymupdf.py FILE_OR_DIR... [--pages 40] [--out RESULTS.json]

| Operation | sealwax | PyMuPDF |
|---|---|---|
| open | `Document.from_bytes` | `pymupdf.open` (per document) |
| text | `glyphs(i, annotations=True)` | `get_text()` |
| drawings | `drawings(i, annotations=True)` | `get_drawings()` |
| render | `render_png(i, 150 / 72)`, first three pages | `get_pixmap(dpi=150).tobytes("png")` |
| fields | `fields(i)` | `list(widgets())` |

Each page goes through every operation of one library, then of the other. Counts and times only,
never a document's text. PyMuPDF (AGPL) runs here as an outside tool, in an environment of its
own; sealwax neither links nor depends on it.
"""

import argparse
import json
import sys
import time
from collections.abc import Callable
from functools import partial
from pathlib import Path

import pymupdf
import sealwax

RENDERED_PAGES = 3


def png(page: pymupdf.Page) -> bytes:
    return page.get_pixmap(dpi=150).tobytes("png")


def timed(totals: dict, key: str, call: Callable[[], object]) -> None:
    start = time.perf_counter()
    try:
        call()
    except Exception:  # noqa: BLE001 - a failure is counted, not raised
        totals["failures"][key] = totals["failures"].get(key, 0) + 1
    totals["seconds"][key] = totals["seconds"].get(key, 0.0) + time.perf_counter() - start
    totals["calls"][key] = totals["calls"].get(key, 0) + 1


def measure(path: Path, pages: int, totals: dict) -> None:
    data = path.read_bytes()
    try:
        ours = sealwax.Document.from_bytes(data)
        theirs = pymupdf.open(stream=data, filetype="pdf")
    except Exception:  # noqa: BLE001 - a document either library refuses is skipped
        totals["skipped"] += 1
        return
    if theirs.needs_pass:
        totals["skipped"] += 1
        return
    timed(totals, "open sealwax", lambda: sealwax.Document.from_bytes(data))
    timed(totals, "open pymupdf", lambda: pymupdf.open(stream=data, filetype="pdf"))
    for index in range(min(ours.page_count, theirs.page_count, pages)):
        page = theirs[index]
        timed(totals, "text sealwax", partial(ours.glyphs, index, annotations=True))
        timed(totals, "drawings sealwax", partial(ours.drawings, index, annotations=True))
        timed(totals, "fields sealwax", partial(ours.fields, index))
        if index < RENDERED_PAGES:
            timed(totals, "render sealwax", partial(ours.render_png, index, 150 / 72))
        timed(totals, "text pymupdf", page.get_text)
        timed(totals, "drawings pymupdf", page.get_drawings)
        timed(totals, "fields pymupdf", partial(list, page.widgets()))
        if index < RENDERED_PAGES:
            timed(totals, "render pymupdf", partial(png, page))
    totals["documents"] += 1


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("inputs", nargs="+", type=Path)
    parser.add_argument("--pages", type=int, default=40, help="pages read per document")
    parser.add_argument("--out", type=Path, help="write the totals as JSON")
    args = parser.parse_args()
    pymupdf.TOOLS.mupdf_display_errors(False)
    totals: dict = {"documents": 0, "skipped": 0, "seconds": {}, "calls": {}, "failures": {}}
    for item in args.inputs:
        paths = sorted(item.rglob("*.pdf")) if item.is_dir() else [item]
        for path in paths:
            measure(path.resolve(), args.pages, totals)
    print(f"{totals['documents']} documents ({totals['skipped']} skipped)")
    print(f"{'operation':10} {'sealwax ms':>11} {'pymupdf ms':>11} {'ratio':>6} {'sealwax/s':>10}")
    for operation in ("open", "text", "drawings", "render", "fields"):
        ours, theirs = (
            totals["seconds"].get(f"{operation} {who}", 0.0) * 1000 / max(totals["calls"].get(f"{operation} {who}", 0), 1)
            for who in ("sealwax", "pymupdf")
        )
        ratio = ours / theirs if theirs else float("nan")
        print(f"{operation:10} {ours:11.2f} {theirs:11.2f} {ratio:6.2f} {1000 / ours if ours else 0:10.0f}")
    if totals["failures"]:
        print("failures:", totals["failures"])
    if args.out:
        args.out.write_text(json.dumps(totals, indent=1))
    return 0


if __name__ == "__main__":
    sys.exit(main())
