"""sealwax's drawings against PyMuPDF's, page by page: the straight lines and the rectangles
`drawings(i, annotations=True)` finds, against the `l` and `re` items of `get_drawings()`, which
includes annotations too. Quads are counted apart: PyMuPDF gives a turned rectangle as four `l`.

    python -I compare_drawings_pymupdf.py FILE_OR_DIR... [--pages 40] [--out RESULTS.jsonl]

Totals, then one JSON line per page whose counts differ, named by the SHA-256 of its document:
counts only, never a document's text. PyMuPDF (AGPL) runs here as an outside tool, in an
environment of its own; sealwax neither links nor depends on it.
"""

import argparse
import collections
import hashlib
import json
import sys
from pathlib import Path

import pymupdf
import sealwax


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("inputs", nargs="+", type=Path)
    parser.add_argument("--pages", type=int, default=40, help="pages read per document")
    parser.add_argument("--out", type=Path, help="write the pages that differ as JSON lines")
    args = parser.parse_args()
    pymupdf.TOOLS.mupdf_display_errors(False)
    totals: collections.Counter = collections.Counter()
    differing = []
    for item in args.inputs:
        for path in sorted(item.rglob("*.pdf")) if item.is_dir() else [item]:
            data = path.resolve().read_bytes()
            try:
                theirs = pymupdf.open(stream=data, filetype="pdf")
                ours = sealwax.Document.from_bytes(data)
            except Exception:  # noqa: BLE001 - a document either library refuses is skipped
                totals["documents skipped"] += 1
                continue
            if theirs.needs_pass or theirs.page_count != ours.page_count:
                totals["documents skipped"] += 1
                continue
            digest = hashlib.sha256(data).hexdigest()
            for index in range(min(ours.page_count, args.pages)):
                try:
                    items = [item[0] for drawing in theirs[index].get_drawings() for item in drawing["items"]]
                    found = ours.drawings(index, annotations=True)
                except Exception:  # noqa: BLE001 - counted
                    totals["pages failed"] += 1
                    continue
                counted = {"lines": items.count("l"), "rectangles": items.count("re"), "quads": items.count("qu")}
                mine = {key: len(found[key]) for key in ("lines", "rectangles", "quads")}
                totals["pages"] += 1
                for key, count in mine.items():
                    totals[f"{key} pymupdf"] += counted[key]
                    totals[f"{key} sealwax"] += count
                if (counted["lines"], counted["rectangles"]) != (mine["lines"], mine["rectangles"]):
                    totals["pages differing"] += 1
                    differing.append({"sha256": digest, "page": index, "pymupdf": counted, "sealwax": mine})
    for key, value in sorted(totals.items()):
        print(f"{key:22} {value}")
    for key in ("lines", "rectangles"):
        theirs_count, mine_count = totals[f"{key} pymupdf"], totals[f"{key} sealwax"]
        print(f"{key}: sealwax {mine_count - theirs_count:+} ({(mine_count - theirs_count) / max(theirs_count, 1):+.2%})")
    if args.out:
        args.out.write_text("".join(json.dumps(row) + "\n" for row in differing))
    return 0


if __name__ == "__main__":
    sys.exit(main())
