"""qpdf (through pikepdf) as the judge of a save's structure: the output must open without
recovery and pass `qpdf --check`.

    uv run python -I structure.py run --originals DIR... --saved OUT_DIR --rows SAVE.jsonl --out JUDGE.jsonl
    uv run python -I structure.py report JUDGE.jsonl     # Markdown on stdout

Content streams are copied as they are, so a problem qpdf finds in the input's content is found
again in the output. Statuses:
- `clean`: the output opens strictly and qpdf finds nothing;
- `inherited`: qpdf finds problems in the output, and found some in the input too;
- `new`: qpdf finds problems in the output and none in the input (a regression);
- `needs_recovery`: the output only opens with qpdf's recovery (a regression);
- `input_unreadable`: qpdf cannot open the input (an encryption it refuses, for instance);
- `unjudged`: qpdf lacks a decoder (jbig2dec) to check the streams;
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


def problems(path: str, recovery: bool) -> list[str]:
    import pikepdf

    with pikepdf.open(path, attempt_recovery=recovery, suppress_warnings=True) as pdf:
        found = ["WARNING: " + w for w in pdf.get_warnings()]
        return found + pdf.check_pdf_syntax()


def judge(original: str, saved: str) -> dict:
    import pikepdf

    try:
        before = problems(original, recovery=True)
    except pikepdf.DependencyError as error:
        return {"status": "unjudged", "detail": str(error)[:120]}
    except (pikepdf.PdfError, pikepdf.PasswordError) as error:
        return {"status": "input_unreadable", "detail": str(error)[:120]}
    try:
        after = problems(saved, recovery=False)
    except pikepdf.DependencyError as error:
        return {"status": "unjudged", "detail": str(error)[:120]}
    except pikepdf.PdfError as error:
        return {"status": "needs_recovery", "detail": str(error)[:200]}
    if not after:
        return {"status": "clean"}
    status = "inherited" if before else "new"
    return {"status": status, "before": len(before), "after": len(after), "detail": after[:3]}


def sha256(path: Path) -> str:
    return hashlib.sha256(path.read_bytes()).hexdigest()


def run(args: argparse.Namespace) -> None:
    originals = {}
    for root in args.originals:
        for path in sorted(Path(root).rglob("*")):
            if path.is_file() and path.suffix.lower() == ".pdf":
                originals.setdefault(sha256(path), path)
    jobs = set()
    for line in Path(args.rows).read_text().splitlines():
        row = json.loads(line)
        if "out_bytes" in row and row["sha256"] in originals:
            jobs.add((row["sha256"], str(originals[row["sha256"]]), str(Path(args.saved) / f"{row['sha256']}.pdf")))

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
        for line in pool.map(one, sorted(jobs)):
            sink.write(line + "\n")


def report(path: str) -> None:
    rows = [json.loads(line) for line in Path(path).read_text().splitlines()]
    counts = Counter(row["status"] for row in rows)
    print("| Status | Documents |\n|---|---|")
    for status, count in counts.most_common():
        print(f"| {status} | {count} |")
    print()
    for row in rows:
        if row["status"] in ("new", "needs_recovery", "timeout", "crash"):
            print(f"- `{row['sha256'][:16]}` {row['status']}: {row.get('detail') or row.get('code')}")


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
