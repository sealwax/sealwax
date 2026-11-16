"""PDFium as the judge of a save or a conversion: every page of the output must render, pixel for
pixel, and extract the same text, each character at the same place, as the same page of the input.

    uv run python -I render.py run --originals DIR... --saved OUT_DIR --rows SAVE.jsonl --out JUDGE.jsonl
    uv run python -I render.py report JUDGE.jsonl        # Markdown on stdout

`SAVE.jsonl` and `OUT_DIR` come from `sealwax save` (outputs named by the SHA-256 of their input).
Each document is judged in its own interpreter, so a PDFium crash or hang only loses that one.
Statuses, the first that applies:
- `pages`: the page count or a page size differs;
- `text`: the text PDFium extracts differs (pages concerned);
- `moved`: same text, but characters moved by more than 0.05 pt (pages, and the largest move);
- `pixels`: same text at the same places, other pixels (pages, pixels that differ, largest
  difference): a font drawn with other outlines, a colour space;
- `same`: same page count, sizes, pixels and text on every page compared;
- `output_unreadable`: PDFium reads the input but not the output (a regression);
- `input_unreadable`: PDFium cannot read the input (the output is not judged);
- `timeout`, `crash`.
"""

from __future__ import annotations

import argparse
import hashlib
import json
import subprocess
import sys
from collections import Counter
from concurrent.futures import ThreadPoolExecutor
from pathlib import Path

TIMEOUT_SECONDS = 120
MAX_PAGES = 100
SCALE = 1.0  # 72 dpi
MOVE_TOLERANCE = 0.05  # points


def characters(page) -> list[tuple[int, float, float]]:
    """Each character PDFium extracts from the page, with its origin."""
    import ctypes

    import pypdfium2.raw as raw

    textpage = page.get_textpage()
    x, y = ctypes.c_double(), ctypes.c_double()
    found = []
    for index in range(textpage.count_chars()):
        code = raw.FPDFText_GetUnicode(textpage.raw, index)
        raw.FPDFText_GetCharOrigin(textpage.raw, index, ctypes.byref(x), ctypes.byref(y))
        found.append((code, x.value, y.value))
    return found


def judge(original: str, saved: str) -> dict:
    import numpy as np
    import pypdfium2 as pdfium

    def load(path: str):
        doc = pdfium.PdfDocument(path)
        doc.init_forms()
        return doc

    try:
        before = load(original)
    except pdfium.PdfiumError as error:
        return {"status": "input_unreadable", "detail": str(error)[:80]}
    try:
        after = load(saved)
    except pdfium.PdfiumError as error:
        return {"status": "output_unreadable", "detail": str(error)[:80]}
    if len(before) != len(after):
        return {"status": "pages", "detail": f"{len(before)} -> {len(after)} pages"}
    row: dict = {"status": "same", "pages": len(before), "compared": min(len(before), MAX_PAGES)}
    pixels, text, moved, sizes = [], [], [], []
    for index in range(row["compared"]):
        a, b = before[index], after[index]
        if [round(v, 2) for v in a.get_size()] != [round(v, 2) for v in b.get_size()]:
            sizes.append(index + 1)
            continue
        image_a = a.render(scale=SCALE, may_draw_forms=True).to_numpy()
        image_b = b.render(scale=SCALE, may_draw_forms=True).to_numpy()
        if image_a.shape != image_b.shape:
            pixels.append([index + 1, -1, -1])
        elif not np.array_equal(image_a, image_b):
            delta = np.abs(image_a.astype(np.int16) - image_b.astype(np.int16))
            pixels.append([index + 1, int(np.count_nonzero(delta.max(axis=-1))), int(delta.max())])
        chars_a, chars_b = characters(a), characters(b)
        if [c[0] for c in chars_a] != [c[0] for c in chars_b]:
            text.append(index + 1)
        else:
            largest = max(
                (max(abs(p[1] - q[1]), abs(p[2] - q[2])) for p, q in zip(chars_a, chars_b)),
                default=0.0,
            )
            if largest > MOVE_TOLERANCE:
                moved.append([index + 1, round(largest, 3)])
    if sizes:
        row.update(status="pages", sizes=sizes)
    elif text:
        row.update(status="text", text=text)
    elif moved:
        row.update(status="moved", moved=moved)
    elif pixels:
        row.update(status="pixels", pixels=pixels)
    return row


def sha256(path: Path) -> str:
    return hashlib.sha256(path.read_bytes()).hexdigest()


def run(args: argparse.Namespace) -> None:
    originals = {}
    for root in args.originals:
        for path in sorted(Path(root).rglob("*")):
            if path.is_file() and path.suffix.lower() == ".pdf":
                originals.setdefault(sha256(path), path)
    jobs = []
    for line in Path(args.rows).read_text().splitlines():
        row = json.loads(line)
        if "out_bytes" in row and row["sha256"] in originals:
            jobs.append((row["sha256"], str(originals[row["sha256"]]), str(Path(args.saved) / f"{row['sha256']}.pdf")))
    jobs = sorted(set(jobs))

    def one(job: tuple[str, str, str]) -> str:
        sha, original, saved = job
        try:
            done = subprocess.run(
                [sys.executable, "-I", __file__, "judge", original, saved],
                capture_output=True,
                text=True,
                timeout=TIMEOUT_SECONDS,
            )
        except subprocess.TimeoutExpired:
            return json.dumps({"sha256": sha, "status": "timeout"})
        lines = done.stdout.strip().splitlines()
        if done.returncode == 0 and lines:
            return json.dumps({"sha256": sha} | json.loads(lines[-1]))
        return json.dumps({"sha256": sha, "status": "crash", "code": done.returncode})

    with ThreadPoolExecutor(args.jobs) as pool, open(args.out, "w") as sink:
        for line in pool.map(one, jobs):
            sink.write(line + "\n")


def report(path: str) -> None:
    rows = [json.loads(line) for line in Path(path).read_text().splitlines()]
    counts = Counter(row["status"] for row in rows)
    pages = sum(row.get("compared", 0) for row in rows if row["status"] == "same")
    print(f"| Status | Documents |\n|---|---|")
    for status, count in counts.most_common():
        print(f"| {status} | {count} |")
    print(f"\nPages identical in `same` documents: {pages}.\n")
    for row in rows:
        if row["status"] not in ("same", "input_unreadable"):
            detail = (
                row.get("detail")
                or row.get("text")
                or row.get("moved")
                or row.get("pixels")
                or row.get("sizes")
                or row.get("code")
            )
            print(f"- `{row['sha256'][:16]}` {row['status']}: {detail}")


def main() -> None:
    if sys.argv[1:2] == ["judge"]:
        print(json.dumps(judge(sys.argv[2], sys.argv[3])))
        return
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    commands = parser.add_subparsers(dest="command", required=True)
    run_parser = commands.add_parser("run")
    run_parser.add_argument("--originals", nargs="+", required=True)
    run_parser.add_argument("--saved", required=True)
    run_parser.add_argument("--rows", required=True)
    run_parser.add_argument("--out", required=True)
    run_parser.add_argument("--jobs", type=int, default=8)
    report_parser = commands.add_parser("report")
    report_parser.add_argument("results")
    args = parser.parse_args()
    if args.command == "run":
        run(args)
    else:
        report(args.results)


if __name__ == "__main__":
    main()
