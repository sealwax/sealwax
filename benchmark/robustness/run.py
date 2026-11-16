"""Run every sealwax operation on the hostile inputs of `hostile.py`, one process each, killed past a
time or memory limit, and report what became of each: done, refused (an explicit error), or
killed (time, memory), crashed (a signal) or panicked.

    uv run --with psutil python -I run.py HOSTILE_DIR --sealwax PATH --out RESULTS.jsonl
    uv run --with psutil python -I run.py HOSTILE_DIR --sealwax PATH --out RESULTS.jsonl --corpus DIR
    uv run --with psutil python -I run.py report RESULTS.jsonl

Signing uses the judges' signer (`benchmark/judges/sign.py rsa-sign`), with the judges' Python.
"""

import argparse
import hashlib
import json
import os
import shutil
import signal
import subprocess
import sys
import tempfile
import threading
import time
from concurrent.futures import ThreadPoolExecutor
from pathlib import Path

import psutil

JUDGES = Path(__file__).resolve().parent.parent / "judges"
POLL = 0.02


def operations(sealwax: str, judge_python: str, pki: Path, benign_xfdf: Path) -> dict[str, callable]:
    def batch(command: list[str]):
        def run(pdf: Path, work: Path) -> list[str]:
            return [sealwax, *command, str(pdf), "--out", str(work / "rows.jsonl"), "--jobs", "1",
                    *(["--out-dir", str(work / "out")] if command[0] not in ("census", "text-census") else [])]
        return run

    def sign(pdf: Path, work: Path) -> list[str]:
        return [sealwax, "sign", str(pdf), "--out", str(work / "signed.pdf"), "--cert", str(pki / "cert.pem"),
                "--field", "Robustness", "--certify", "--", judge_python, "-I", str(JUDGES / "sign.py"),
                "rsa-sign", str(pki / "key.pem")]

    return {
        "info": lambda pdf, work: [sealwax, "info", str(pdf)],
        "census": batch(["census"]),
        "save": batch(["save", "--mode", "stream"]),
        "incremental": batch(["save", "--mode", "incremental"]),
        "optimize": batch(["save", "--mode", "optimize"]),
        "grayscale": batch(["save", "--mode", "grayscale"]),
        "text": batch(["text-census"]),
        "pdfa": batch(["pdfa", "--level", "a"]),
        "flatten": batch(["flatten"]),
        "merge": lambda pdf, work: [sealwax, "merge", str(pdf), str(pdf), "--out", str(work / "merged.pdf")],
        "xfdf": _xfdf_op(sealwax, benign_xfdf),
        "sign": sign,
        # Exit status 2 when a signature is not intact or the file does not open: "refused".
        "verify": lambda pdf, work: [sealwax, "verify", str(pdf)],
        "glyphs": lambda pdf, work: [sealwax, "glyphs", str(pdf), "--annotations", "--out", str(work / "glyphs.jsonl")],
        "render": lambda pdf, work: [sealwax, "render", str(pdf), "--scale", "2", "--out", str(work / "page.png")],
    }


def watch(command: list[str], work: Path, seconds: float, memory: int) -> dict:
    started = time.monotonic()
    process = subprocess.Popen(command, cwd=work, stdout=subprocess.PIPE, stderr=subprocess.PIPE,
                               stdin=subprocess.DEVNULL, start_new_session=True)
    killed = None
    peak = 0
    output: dict[str, bytes] = {}

    def drain(name, pipe):
        output[name] = pipe.read()

    readers = [threading.Thread(target=drain, args=(n, p)) for n, p in (("out", process.stdout), ("err", process.stderr))]
    for reader in readers:
        reader.start()
    reaped = None
    try:
        handle = psutil.Process(process.pid)
    except psutil.Error:
        handle = None
    while reaped is None:
        pid, status, usage = os.wait4(process.pid, os.WNOHANG)
        if pid:
            reaped = (status, usage)
            break
        try:
            rss = sum(p.memory_info().rss for p in [handle, *handle.children(recursive=True)]) if handle else 0
        except psutil.Error:
            rss = 0
        peak = max(peak, rss)
        if rss > memory:
            killed = "memory"
        elif time.monotonic() - started > seconds:
            killed = "time"
        if killed:
            os.killpg(process.pid, signal.SIGKILL)
            _, status, usage = os.wait4(process.pid, 0)
            reaped = (status, usage)
            break
        time.sleep(POLL)
    status, usage = reaped
    process.returncode = os.waitstatus_to_exitcode(status)
    for reader in readers:
        reader.join()
    elapsed = time.monotonic() - started
    code = process.returncode
    return {"code": code, "killed": killed, "seconds": round(elapsed, 2),
            "peak_mib": round(max(peak, usage.ru_maxrss) / (1 << 20)),
            "stderr": output.get("err", b"")[-2000:].decode("utf-8", "replace")}


def verdict(result: dict, work: Path) -> tuple[str, str]:
    if result["killed"]:
        return result["killed"], ""
    stderr = result["stderr"]
    rows = work / "rows.jsonl"
    row_error = ""
    if result["code"] == 0 and rows.exists():
        for line in rows.read_text().splitlines():
            row = json.loads(line)
            row_error = row_error or str(row.get("error") or row.get("count_error") or row.get("pages_error") or "")
    if "panicked" in stderr:
        line = next((l for l in stderr.splitlines() if "panicked" in l), "")
        # A dependency's panic sealwax caught and turned into an error of its own.
        if result["code"] == 0 and row_error and row_error != "panic":
            return "caught", f"{row_error[:80]} ({line[:100]})"
        return "panic", line[:200]
    if "overflowed its stack" in stderr:
        return "stack", ""
    if result["code"] < 0:
        return "signal", signal.Signals(-result["code"]).name
    if "memory allocation" in stderr:
        return "alloc", stderr.strip().splitlines()[-1][:200]
    if result["code"] != 0:
        return "refused", (stderr.strip().splitlines() or [""])[-1][:200]
    if row_error:
        return "refused", row_error[:200]
    return "done", ""


def run(args: argparse.Namespace) -> None:
    hostile = Path(args.hostile).resolve()
    judge_python = str(JUDGES / ".venv" / "bin" / "python")
    with tempfile.TemporaryDirectory() as scratch:
        scratch = Path(scratch)
        pki = scratch / "pki"
        pki.mkdir()
        subprocess.run([judge_python, "-I", "-c",
                        "import sys; sys.path.insert(0, sys.argv[1]); import sign; "
                        "from pathlib import Path; sign.make_certificate(Path(sys.argv[2]))",
                        str(JUDGES), str(pki)], check=True)
        shutil.copyfile(hostile / "xfdf" / "benign.xfdf", scratch / "benign.xfdf")
        sealwax = str(Path(args.sealwax).resolve())
        ops = operations(sealwax, judge_python, pki, scratch / "benign.xfdf")
        jobs = []
        selected = set(args.ops.split(",")) if args.ops else set(ops)
        if args.corpus:
            # Real documents, named by their SHA-256 only.
            for pdf in sorted(p for p in Path(args.corpus).resolve().rglob("*") if p.suffix.lower() == ".pdf" and p.is_file()):
                case = "corpus/" + hashlib.sha256(pdf.read_bytes()).hexdigest()[:16]
                for name, op in ops.items():
                    if name in selected:
                        jobs.append((case, name, pdf, op))
        for pdf in [] if args.corpus else sorted((hostile / "pdf").glob("*.pdf")):
            if args.only and pdf.stem not in args.only.split(","):
                continue
            for name, op in ops.items():
                if name in selected:
                    jobs.append((f"pdf/{pdf.stem}", name, pdf, op))
        if not args.corpus and (not args.only or "xfdf" in args.only.split(",")):
            for xfdf_file in sorted((hostile / "xfdf").glob("*.xfdf")):
                jobs.append((f"xfdf/{xfdf_file.stem}", "xfdf", hostile / "benign.pdf",
                             _xfdf_op(sealwax, xfdf_file)))
        sink = open(args.out, "w")
        lock = threading.Lock()

        def one(job):
            case, name, pdf, op = job
            work = Path(tempfile.mkdtemp(dir=scratch))
            try:
                (work / "out").mkdir()
                result = watch(op(pdf, work), work, args.seconds, args.memory_mib << 20)
                outcome, detail = verdict(result, work)
            finally:
                shutil.rmtree(work, ignore_errors=True)
            row = {"case": case, "op": name, "outcome": outcome, "detail": detail,
                   "seconds": result["seconds"], "peak_mib": result["peak_mib"]}
            with lock:
                sink.write(json.dumps(row) + "\n")
                sink.flush()
                print(f"{case:32} {name:12} {outcome:8} {result['seconds']:7.2f}s {result['peak_mib']:6} MiB {detail[:80]}",
                      flush=True)

        with ThreadPoolExecutor(args.jobs) as pool:
            list(pool.map(one, jobs))
        sink.close()


def _xfdf_op(sealwax: str, xfdf_file: Path):
    def run(pdf: Path, work: Path) -> list[str]:
        directory = work / "xfdf"
        directory.mkdir(exist_ok=True)
        digest = hashlib.sha256(pdf.read_bytes()).hexdigest()
        shutil.copyfile(xfdf_file, directory / f"{digest}.xfdf")
        return [sealwax, "xfdf", str(pdf), "--xfdf-dir", str(directory), "--out-dir", str(work / "out"),
                "--out", str(work / "rows.jsonl"), "--jobs", "1"]
    return run


SYMBOLS = {"done": "✓", "refused": "×", "caught": "c", "time": "⏱", "memory": "M", "panic": "P", "stack": "S", "signal": "!",
           "alloc": "A"}


def report(path: str) -> None:
    rows = [json.loads(l) for l in Path(path).read_text().splitlines()]
    ops = list(dict.fromkeys(r["op"] for r in rows))
    cases = sorted(dict.fromkeys(r["case"] for r in rows))
    table = {(r["case"], r["op"]): r for r in rows}
    print("| Case | " + " | ".join(ops) + " |")
    print("|---|" + "---|" * len(ops))
    for case in cases:
        cells = []
        for op in ops:
            row = table.get((case, op))
            if not row:
                cells.append("")
                continue
            mark = SYMBOLS.get(row["outcome"], "?")
            slow = row["seconds"] >= 5 or row["peak_mib"] >= 512
            cells.append(f"{mark} {row['seconds']:.0f}s {row['peak_mib']}M" if slow or mark not in "✓×" else mark)
        print(f"| {case} | " + " | ".join(cells) + " |")
    counts: dict[str, int] = {}
    for row in rows:
        counts[row["outcome"]] = counts.get(row["outcome"], 0) + 1
    print()
    print(", ".join(f"{k}: {v}" for k, v in sorted(counts.items())))


def main() -> None:
    if sys.argv[1:2] == ["report"]:
        report(sys.argv[2])
        return
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("hostile")
    parser.add_argument("--sealwax", required=True)
    parser.add_argument("--out", required=True)
    parser.add_argument("--seconds", type=float, default=30)
    parser.add_argument("--memory-mib", type=int, default=4096)
    parser.add_argument("--jobs", type=int, default=8)
    parser.add_argument("--ops", default="")
    parser.add_argument("--only", default="")
    parser.add_argument("--corpus", default="", help="run on every PDF under this directory instead")
    run(parser.parse_args())


if __name__ == "__main__":
    main()
