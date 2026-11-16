"""sealwax's colour rendering against PDFium's, page by page.

    python -I compare_pdfium.py FILE_OR_DIR... [--dpi 100] [--pages 10] [--out RESULTS.jsonl]

Each page is rendered by `Document.render_png` and by PDFium (forms and annotations drawn), at
the same resolution; the two images are averaged over blocks of 4 x 4 pixels, and the blocks
compared channel by channel, in levels (0-255). One JSON line per document, numbers only: for
each page, its size in pixels on both sides, the median and the 99th percentile of the block
differences, and the share of blocks differing by more than 32 levels (a glyph or a shape missing
or misplaced, rather than drawn a little differently). Needs numpy, Pillow and pypdfium2
(Apache-2.0 / BSD), as outside tools.
"""

import argparse
import hashlib
import io
import json
import sys
from pathlib import Path

import numpy as np
import pypdfium2 as pdfium
from PIL import Image

import sealwax

BLOCK = 4


def blocks(image: np.ndarray) -> np.ndarray:
    height, width = (image.shape[0] // BLOCK) * BLOCK, (image.shape[1] // BLOCK) * BLOCK
    cropped = image[:height, :width].astype(np.float32)
    return cropped.reshape(height // BLOCK, BLOCK, width // BLOCK, BLOCK, 3).mean(axis=(1, 3))


def compare(path: Path, dpi: float, pages: int) -> dict:
    data = path.read_bytes()
    row: dict = {"sha256": hashlib.sha256(data).hexdigest()}
    try:
        ours = sealwax.Document.from_bytes(data)
        theirs = pdfium.PdfDocument(data)
        theirs.init_forms()
    except (sealwax.PdfError, pdfium.PdfiumError) as error:
        return row | {"error": type(error).__name__}
    scale = dpi / 72
    row["pages"] = []
    for index in range(min(ours.page_count, len(theirs), pages)):
        try:
            mine = np.asarray(Image.open(io.BytesIO(ours.render_png(index, scale))).convert("RGB"))
        except sealwax.PdfError as error:
            row["pages"].append({"error": type(error).__name__})
            continue
        bitmap = theirs[index].render(scale=scale, may_draw_forms=True, draw_annots=True, rev_byteorder=True)
        other = bitmap.to_numpy()[:, :, :3]
        height, width = min(mine.shape[0], other.shape[0]), min(mine.shape[1], other.shape[1])
        delta = np.abs(blocks(mine[:height, :width]) - blocks(other[:height, :width]))
        row["pages"].append({
            "sealwax": [mine.shape[1], mine.shape[0]],
            "pdfium": [other.shape[1], other.shape[0]],
            "median": round(float(np.median(delta)), 2),
            "p99": round(float(np.percentile(delta, 99)), 2),
            "over_32": round(float((delta.max(axis=-1) > 32).mean()), 5),
        })
    return row


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("paths", nargs="+")
    parser.add_argument("--dpi", type=float, default=100)
    parser.add_argument("--pages", type=int, default=10)
    parser.add_argument("--out")
    args = parser.parse_args()
    files = sorted({p for root in map(Path, args.paths) for p in ([root] if root.is_file() else root.rglob("*.pdf"))})
    sink = open(args.out, "w") if args.out else sys.stdout
    for path in files:
        print(json.dumps(compare(path, args.dpi, args.pages)), file=sink, flush=True)
    return 0


if __name__ == "__main__":
    sys.exit(main())
