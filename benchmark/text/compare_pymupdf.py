"""sealwax's glyphs against PyMuPDF's text, page by page: the text sealwax's glyphs carry, in the
order of the content then of the annotations' appearances, read as a reader reads them
(`/ActualText` replacements, without the glyphs marked overdrawn), compared with PyMuPDF's
`get_text()`, which includes annotations too, white space left out on both sides.

    python -I compare_pymupdf.py FILE_OR_DIR... [--out RESULTS.jsonl]

One JSON line per document, counts only, never its text: pages, pages whose text is the same,
and the similarity of the whole (difflib's ratio, 1 for the same text). PyMuPDF (AGPL) runs here
as an outside tool, in an environment of its own; sealwax neither links nor depends on it.
"""

import argparse
import difflib
import hashlib
import json
import sys
from pathlib import Path

import fitz

import sealwax


def read(glyph: dict) -> str:
    """What a glyph reads as: its replacement text when the document gives one, else its own text
    unless it repeats the glyph before it."""
    if glyph["actual_text"] is not None:
        return glyph["actual_text"]
    if glyph["overdrawn"]:
        return ""
    return glyph["text"] or ""


def squeezed(text: str) -> str:
    return "".join(text.split())


def compare(path: Path) -> dict:
    data = path.read_bytes()
    row: dict = {"sha256": hashlib.sha256(data).hexdigest()}
    try:
        ours = sealwax.Document.from_bytes(data)
    except sealwax.PdfError as error:
        return row | {"error": type(error).__name__}
    with fitz.open(stream=data, filetype="pdf") as theirs:
        pages = min(ours.page_count, len(theirs))
        same = 0
        mine_all, theirs_all = [], []
        for index in range(pages):
            mine = squeezed("".join(read(glyph) for glyph in ours.glyphs(index, annotations=True)))
            other = squeezed(theirs[index].get_text())
            same += mine == other
            mine_all.append(mine)
            theirs_all.append(other)
    a, b = "".join(mine_all), "".join(theirs_all)
    ratio = difflib.SequenceMatcher(None, a, b, autojunk=False).ratio() if (a or b) else 1.0
    return row | {"pages": pages, "same_pages": same, "similarity": round(ratio, 4),
                  "characters": len(b)}


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("paths", nargs="+")
    parser.add_argument("--out")
    args = parser.parse_args()
    files = sorted({p for root in map(Path, args.paths) for p in ([root] if root.is_file() else root.rglob("*.pdf"))})
    sink = open(args.out, "w") if args.out else sys.stdout
    for path in files:
        print(json.dumps(compare(path)), file=sink, flush=True)
    return 0


if __name__ == "__main__":
    sys.exit(main())
