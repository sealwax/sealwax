"""Markdown summary of a veraPDF JSON report: how many documents comply, and which rules fail.

    python3 -I benchmark/verapdf/summarize.py REPORT.json [--title ...]
    python3 -I benchmark/verapdf/summarize.py AFTER.json --baseline BEFORE.json --baseline-files DIR

Files are expected to be named `<sha256>.pdf`, as the bench writes them.

With `--baseline`, the report compares each document with its verdict before a save: `sealwax
save` names its outputs after the SHA-256 of their input, so the baseline's files (in
`--baseline-files`) are hashed to pair them. Standard library only.
"""

from __future__ import annotations

import argparse
import hashlib
import json
from collections import Counter, defaultdict
from pathlib import Path

EXAMPLES = 4


def sha_of(name: str) -> str:
    return Path(name).stem


def verdicts(report: dict) -> dict[str, dict]:
    """Per document: `compliant` (bool or None when veraPDF could not validate) and failed rules."""
    found: dict[str, dict] = {}
    for job in report["report"]["jobs"]:
        sha = sha_of(job["itemDetails"]["name"])
        results = job.get("validationResult") or []
        if not results:
            reason = job.get("taskException", {}).get("exceptionMessage") or "no validation result"
            found[sha] = {"compliant": None, "rules": {}, "reason": str(reason)[:120]}
            continue
        result = results[0]
        rules = {}
        for rule in result.get("details", {}).get("ruleSummaries", []):
            if rule.get("status", "").lower() != "failed":
                continue
            key = f"{rule['specification']} {rule['clause']}-{rule['testNumber']}"
            rules[key] = {"checks": rule.get("failedChecks", 0), "description": rule.get("description", "")}
        found[sha] = {"compliant": bool(result.get("compliant")), "rules": rules, "profile": result.get("profileName")}
    return found


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("report", type=Path)
    parser.add_argument("--title", default="veraPDF")
    parser.add_argument("--baseline", type=Path, help="veraPDF report of the inputs of the save")
    parser.add_argument("--baseline-files", type=Path, help="the inputs of the save, named as in --baseline")
    args = parser.parse_args()
    docs = verdicts(json.loads(args.report.read_text()))
    profiles = {d.get("profile") for d in docs.values() if d.get("profile")}
    compliant = sum(1 for d in docs.values() if d["compliant"] is True)
    failing = sum(1 for d in docs.values() if d["compliant"] is False)
    unvalidated = [d for d in docs.values() if d["compliant"] is None]
    print(f"## {args.title}\n")
    print(f"- Profile: {', '.join(sorted(profiles)) or '—'}.")
    print(f"- Documents: {len(docs)}. **Compliant: {compliant}**. Not compliant: {failing}.", end="")
    print(f" Not validated: {len(unvalidated)}." if unvalidated else "")
    for d in unvalidated:
        print(f"  - {d['reason']}")

    by_rule: dict[str, list[str]] = defaultdict(list)
    checks: Counter[str] = Counter()
    descriptions: dict[str, str] = {}
    for sha, d in sorted(docs.items()):
        for key, rule in d["rules"].items():
            by_rule[key].append(sha)
            checks[key] += rule["checks"]
            descriptions[key] = rule["description"]
    if by_rule:
        print("\n### Failed rules\n")
        print("| Rule (clause-test) | What it requires | Documents | Failed checks | Examples (SHA-256) |")
        print("|---|---|---|---|---|")
        for key, shas in sorted(by_rule.items(), key=lambda item: (-len(item[1]), item[0])):
            description = " ".join(descriptions[key].split())[:160].replace("|", "\\|")
            examples = ", ".join(f"`{s[:16]}`" for s in shas[:EXAMPLES])
            print(f"| {key} | {description} | {len(shas)} | {checks[key]} | {examples} |")

    if args.baseline:
        compare(docs, verdicts(json.loads(args.baseline.read_text())), args.baseline_files)

    return 0


def compare(after: dict[str, dict], before: dict[str, dict], files: Path) -> None:
    """Per saved document: same verdict and same failed rules as its input, or what changed."""
    by_content = {}
    for path in sorted(files.glob("*.pdf")):
        by_content.setdefault(hashlib.sha256(path.read_bytes()).hexdigest(), path.stem)
    outcome: Counter[str] = Counter()
    changes = []
    for sha, verdict in sorted(after.items()):
        name = by_content.get(sha)
        if name is None or name not in before:
            outcome["no baseline"] += 1
            continue
        old = before[name]
        if old["compliant"] == verdict["compliant"] and old["rules"].keys() == verdict["rules"].keys():
            outcome["same verdict, same failed rules"] += 1
            continue
        if old["compliant"] and not verdict["compliant"]:
            outcome["compliance lost"] += 1
        elif not old["compliant"] and verdict["compliant"]:
            outcome["compliance gained"] += 1
        else:
            outcome["failed rules changed"] += 1
        gained = sorted(verdict["rules"].keys() - old["rules"].keys())
        lost = sorted(old["rules"].keys() - verdict["rules"].keys())
        changes.append(f"- `{sha[:16]}`: now failing {gained or '—'}, no longer failing {lost or '—'}")
    print("\n### Against the input of the save\n")
    print("| Outcome | Documents |\n|---|---|")
    for key, count in outcome.most_common():
        print(f"| {key} | {count} |")
    if changes:
        print()
        print("\n".join(changes))


if __name__ == "__main__":
    raise SystemExit(main())
