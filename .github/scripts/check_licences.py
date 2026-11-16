"""Check that a wheel or a source distribution of sealwax carries its licence files.

    python -I .github/scripts/check_licences.py DIST

DIST is a wheel (.whl), whose licence files are under `*.dist-info/licenses/` and named by
`License-File` in its metadata, or a source distribution (.tar.gz), which has them at its root.
"""

import sys
import tarfile
import zipfile
from pathlib import Path

EXPECTED = [
    "LICENSE-MIT",
    "LICENSE-APACHE",
    "THIRD_PARTY_NOTICES.md",
    "licenses/LICENSE-FOXIT",
    "licenses/LICENSE-CC0",
    "licenses/LICENSE-dingbat-to-unicode",
    "licenses/LICENSE-PDFIUM",
    "licenses/DEPENDENCIES.md",
]


def wheel_missing(path: Path) -> list[str]:
    with zipfile.ZipFile(path) as wheel:
        names = wheel.namelist()
        metadata = next(n for n in names if n.endswith(".dist-info/METADATA"))
        declared = {
            line.removeprefix("License-File:").strip()
            for line in wheel.read(metadata).decode().splitlines()
            if line.startswith("License-File:")
        }
        sizes = {info.filename: info.file_size for info in wheel.infolist()}
    missing = []
    for expected in EXPECTED:
        found = [n for n in names if n.endswith(".dist-info/licenses/" + expected)]
        if not found or not sizes[found[0]] or expected not in declared:
            missing.append(expected)
    return missing


def sdist_missing(path: Path) -> list[str]:
    with tarfile.open(path) as sdist:
        files = {
            member.name.split("/", 1)[1]: member.size
            for member in sdist.getmembers()
            if member.isfile() and "/" in member.name
        }
    # maturin puts the licence files at the root of the source distribution.
    return [expected for expected in EXPECTED if not files.get(expected)]


def main() -> int:
    if len(sys.argv) != 2:
        print(__doc__, file=sys.stderr)
        return 2
    path = Path(sys.argv[1])
    missing = wheel_missing(path) if path.suffix == ".whl" else sdist_missing(path)
    if missing:
        print(f"{path.name}: licence files missing: {', '.join(missing)}", file=sys.stderr)
        return 1
    print(f"{path.name}: {len(EXPECTED)} licence files")
    return 0


if __name__ == "__main__":
    sys.exit(main())
