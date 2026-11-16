"""Tagging (PDF/A level a, N1) judged against the structure producers gave their documents: how
sealwax gathers text into paragraphs, and the order it reads them in.

    uv run python -I reading_order.py strip --originals DIR... --out-dir STRIPPED --map MAP.jsonl [--exclude verapdf]
    sealwax pdfa STRIPPED --level a --out-dir TAGGED --out TAGGED.jsonl
    uv run python -I reading_order.py compare --map MAP.jsonl --tagged TAGGED --out JUDGE.jsonl
    uv run python -I reading_order.py report JUDGE.jsonl       # Markdown on stdout

Validators' test suites (veraPDF's) tag their files wrong on purpose: `--exclude` leaves them
out. `strip` copies each tagged document without its structure tree (its content keeps the marked
content, which sealwax then treats as left behind), named by the SHA-256 of the copy; the map
pairs each copy with its original. `compare` reads, page by page, the text objects PDFium finds
(one per text-showing operator, in content order, the same in both files) and the structure
element each belongs to: in the original, the nearest paragraph-like ancestor of its element
(`P`, a heading, `LI` or `LBody`, a table cell, a caption...: `Span`, `Link` or `NonStruct` climb
to it); in sealwax's output, its `P`. Per document:
- `pairs`: of the pairs of text objects in one block of the original, how many sealwax puts in
  one paragraph (recall), and of those sealwax puts together, how many the original does
  (precision). A producer that tags each line as a paragraph lowers the precision while sealwax
  is right;
- `order`: of the pairs of text objects in different blocks on both sides, how many come in the
  same order (the original's order is that of its tree);
- text the original ties and sealwax makes an artifact (running heads, page numbers), and the
  other way round.

Each document is judged in its own interpreter. No text of the documents is kept.
"""

from __future__ import annotations

import argparse
import hashlib
import json
import subprocess
import sys
from collections import Counter, defaultdict
from concurrent.futures import ThreadPoolExecutor
from pathlib import Path

TIMEOUT_SECONDS = 300
MAX_PAGES = 30
MAX_NODES = 200_000
BLOCKS = {
    "P", "H", "H1", "H2", "H3", "H4", "H5", "H6", "LI", "LBody", "TD", "TH", "Caption", "Figure",
    "BlockQuote", "TOCI", "Formula",
}


def strip(args: argparse.Namespace) -> None:
    import pikepdf

    out_dir = Path(args.out_dir)
    out_dir.mkdir(parents=True, exist_ok=True)
    with open(args.map, "w") as sink:
        for root in args.originals:
            for path in sorted(Path(root).rglob("*.pdf")):
                if any(part in str(path) for part in args.exclude):
                    continue
                try:
                    with pikepdf.open(path) as pdf:
                        tree = pdf.Root.get("/StructTreeRoot")
                        if tree is None or "/ParentTree" not in tree or "/K" not in tree:
                            continue
                        if len(pdf.pages) > MAX_PAGES:
                            continue
                        del pdf.Root.StructTreeRoot
                        target = out_dir / "copy.pdf"
                        pdf.save(target)
                except Exception:  # noqa: BLE001 - a document pikepdf cannot handle is skipped
                    continue
                sha = hashlib.sha256(target.read_bytes()).hexdigest()
                target.rename(out_dir / f"{sha}.pdf")
                sink.write(json.dumps({"stripped": sha, "original": str(path)}) + "\n")


def roles(tree) -> dict[str, str]:
    found = {}
    role_map = tree.get("/RoleMap")
    if role_map is not None:
        for key, value in role_map.items():
            try:
                found[str(key)[1:]] = str(value)[1:]
            except Exception:  # noqa: BLE001
                continue
    return found


def standard(kind: str, role_map: dict[str, str]) -> str:
    seen = set()
    while kind in role_map and kind not in seen:
        seen.add(kind)
        kind = role_map[kind]
    return kind


def elements(pdf):
    """Each structure element's block (by object id) and the order of blocks in the tree."""
    import pikepdf

    tree = pdf.Root.StructTreeRoot
    role_map = roles(tree)
    order: dict[tuple, int] = {}
    parent: dict[tuple, tuple | None] = {}
    kinds: dict[tuple, str] = {}
    stack = [(tree.get("/K"), None)]
    nodes = 0
    while stack and nodes < MAX_NODES:
        node, up = stack.pop()
        nodes += 1
        if isinstance(node, pikepdf.Array):
            for item in reversed(list(node)):
                stack.append((item, up))
            continue
        if not isinstance(node, pikepdf.Dictionary) or "/S" not in node:
            continue
        key = node.objgen if node.is_indirect else (id(node), 0)
        if key in order:
            continue
        order[key] = len(order)
        parent[key] = up
        kinds[key] = standard(str(node.S)[1:], role_map)
        kids = node.get("/K")
        if kids is not None:
            stack.append((kids, key))

    def block(key):
        """The nearest paragraph-like ancestor (or the element itself, without one)."""
        current = key
        for _ in range(64):
            if current is None:
                return key
            if kinds.get(current) in BLOCKS:
                return current
            current = parent.get(current)
        return key

    return order, block


def page_elements(pdf, page) -> dict[int, tuple]:
    """MCID -> element key, from the page's entry in the parent tree."""
    import pikepdf

    key = page.obj.get("/StructParents")
    tree = pdf.Root.get("/StructTreeRoot")
    if key is None or tree is None or "/ParentTree" not in tree:
        return {}
    key = int(key)

    def lookup(node, depth=0):
        if depth > 32:
            return None
        nums = node.get("/Nums")
        if nums is not None:
            for i in range(0, len(nums) - 1, 2):
                if int(nums[i]) == key:
                    return nums[i + 1]
        kids = node.get("/Kids")
        if kids is not None:
            for kid in kids:
                limits = kid.get("/Limits")
                if limits is not None and not (int(limits[0]) <= key <= int(limits[1])):
                    continue
                found = lookup(kid, depth + 1)
                if found is not None:
                    return found
        return None

    entry = lookup(tree.ParentTree)
    found = {}
    if isinstance(entry, pikepdf.Array):
        for mcid, element in enumerate(entry):
            if isinstance(element, pikepdf.Dictionary) and element.is_indirect:
                found[mcid] = element.objgen
    return found


def text_mcids(page) -> list[int]:
    import pypdfium2.raw as raw

    found = []
    for index in range(raw.FPDFPage_CountObjects(page.raw)):
        obj = raw.FPDFPage_GetObject(page.raw, index)
        if raw.FPDFPageObj_GetType(obj) == raw.FPDF_PAGEOBJ_TEXT:
            found.append(raw.FPDFPageObj_GetMarkedContentID(obj))
    return found


def judge(original: str, tagged: str) -> dict:
    import pikepdf
    import pypdfium2 as pdfium

    sums = Counter()
    with pikepdf.open(original) as truth_pdf, pikepdf.open(tagged) as ours_pdf:
        truth_order, truth_block = elements(truth_pdf)
        ours_order, ours_block = elements(ours_pdf)
        truth_doc, ours_doc = pdfium.PdfDocument(original), pdfium.PdfDocument(tagged)
        pages = min(len(truth_doc), len(ours_doc), MAX_PAGES)
        for index in range(pages):
            truth_ids = text_mcids(truth_doc[index])
            ours_ids = text_mcids(ours_doc[index])
            if len(truth_ids) != len(ours_ids):
                sums["pages_misaligned"] += 1
                continue
            sums["pages"] += 1
            truth_map = page_elements(truth_pdf, truth_pdf.pages[index])
            ours_map = page_elements(ours_pdf, ours_pdf.pages[index])
            cells: Counter = Counter()
            for t_mcid, o_mcid in zip(truth_ids, ours_ids):
                t = truth_block(truth_map.get(t_mcid)) if t_mcid >= 0 else None
                o = ours_block(ours_map.get(o_mcid)) if o_mcid >= 0 else None
                sums["text_objects"] += 1
                if t is None and o is None:
                    sums["artifact_both"] += 1
                elif t is None:
                    sums["tied_here_only"] += 1
                elif o is None:
                    sums["artifact_here_only"] += 1
                else:
                    sums["tied_both"] += 1
                    cells[(t, o)] += 1
            # Pairs in one block, one paragraph, both.
            by_truth, by_ours = Counter(), Counter()
            for (t, o), n in cells.items():
                by_truth[t] += n
                by_ours[o] += n
                sums["pairs_both"] += n * (n - 1) // 2
            sums["pairs_truth"] += sum(n * (n - 1) // 2 for n in by_truth.values())
            sums["pairs_ours"] += sum(n * (n - 1) // 2 for n in by_ours.values())
            # Order of pairs in different blocks on both sides.
            items = list(cells.items())
            for i in range(len(items)):
                (t1, o1), n1 = items[i]
                for j in range(i + 1, len(items)):
                    (t2, o2), n2 = items[j]
                    if t1 == t2 or o1 == o2:
                        continue
                    a = truth_order.get(t1, 0) - truth_order.get(t2, 0)
                    b = ours_order.get(o1, 0) - ours_order.get(o2, 0)
                    if (a > 0) == (b > 0):
                        sums["order_same"] += n1 * n2
                    else:
                        sums["order_other"] += n1 * n2
    return {"status": "judged", **sums}


def compare(args: argparse.Namespace) -> None:
    pairs = [json.loads(line) for line in Path(args.map).read_text().splitlines()]
    tagged_dir = Path(args.tagged)

    def one(pair: dict) -> str:
        tagged = tagged_dir / f"{pair['stripped']}.pdf"
        if not tagged.exists():
            return json.dumps({"sha256": pair["stripped"], "status": "not_converted"})
        try:
            done = subprocess.run(
                [sys.executable, "-I", __file__, "judge", pair["original"], str(tagged)],
                capture_output=True, text=True, timeout=TIMEOUT_SECONDS,
            )
        except subprocess.TimeoutExpired:
            return json.dumps({"sha256": pair["stripped"], "status": "timeout"})
        lines = done.stdout.strip().splitlines()
        if done.returncode == 0 and lines:
            return json.dumps({"sha256": pair["stripped"]} | json.loads(lines[-1]))
        return json.dumps({"sha256": pair["stripped"], "status": "crash", "code": done.returncode})

    with ThreadPoolExecutor(args.jobs) as pool, open(args.out, "w") as sink:
        for line in pool.map(one, pairs):
            sink.write(line + "\n")


def ratio(a: int, b: int) -> str:
    return f"{a / b:.3f}" if b else "—"


def report(path: str) -> None:
    rows = [json.loads(line) for line in Path(path).read_text().splitlines()]
    statuses = Counter(row["status"] for row in rows)
    total = Counter()
    per_doc = defaultdict(list)
    for row in rows:
        if row["status"] != "judged":
            continue
        for key, value in row.items():
            if isinstance(value, int):
                total[key] += value
        if row.get("pairs_truth"):
            per_doc["recall"].append(row.get("pairs_both", 0) / row["pairs_truth"])
        same, other = row.get("order_same", 0), row.get("order_other", 0)
        if same + other:
            per_doc["order"].append(same / (same + other))
    print("| Status | Documents |\n|---|---|")
    for status, count in statuses.most_common():
        print(f"| {status} | {count} |")
    print()
    print("| Measure | All text objects | Median document |\n|---|---|---|")

    def median(values):
        values = sorted(values)
        return f"{values[len(values) // 2]:.3f}" if values else "—"

    print(f"| Pages compared (misaligned) | {total['pages']} ({total['pages_misaligned']}) | |")
    print(f"| Text objects | {total['text_objects']} | |")
    print(f"| Tied on both sides | {total['tied_both']} | |")
    print(f"| Artifact here only (tied in the original) | {total['artifact_here_only']} | |")
    print(f"| Tied here only (artifact in the original) | {total['tied_here_only']} | |")
    print(f"| Pairs of one block kept together (recall) | {ratio(total['pairs_both'], total['pairs_truth'])} | {median(per_doc['recall'])} |")
    print(f"| Pairs put together that share a block (precision) | {ratio(total['pairs_both'], total['pairs_ours'])} | |")
    print(f"| Pairs of blocks in the same order | {ratio(total['order_same'], total['order_same'] + total['order_other'])} | {median(per_doc['order'])} |")


def main() -> None:
    if sys.argv[1:2] == ["judge"]:
        print(json.dumps(judge(sys.argv[2], sys.argv[3])))
        return
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    commands = parser.add_subparsers(dest="command", required=True)
    strip_parser = commands.add_parser("strip")
    strip_parser.add_argument("--originals", nargs="+", required=True)
    strip_parser.add_argument("--out-dir", required=True)
    strip_parser.add_argument("--map", required=True)
    strip_parser.add_argument(
        "--exclude", nargs="*", default=[], help="skip paths containing these (validator test suites)"
    )
    compare_parser = commands.add_parser("compare")
    compare_parser.add_argument("--map", required=True)
    compare_parser.add_argument("--tagged", required=True)
    compare_parser.add_argument("--out", required=True)
    compare_parser.add_argument("--jobs", type=int, default=8)
    report_parser = commands.add_parser("report")
    report_parser.add_argument("results")
    args = parser.parse_args()
    {"strip": strip, "compare": compare, "report": lambda a: report(a.results)}[args.command](args)


if __name__ == "__main__":
    main()
