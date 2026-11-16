"""Publish the workspace's crates on crates.io, each after those it depends on, skipping a version
already published, so that a release stopped halfway is finished by running it again.

    python -I .github/scripts/publish_crates.py [--dry-run]

When crates.io limits the rate (a new crate, or many versions at once), it says until when; the
script waits that long, within MAX_WAIT, then gives up: running it again later publishes the rest.
CARGO_REGISTRY_TOKEN is read by cargo, never by this script.
"""

import argparse
import json
import re
import subprocess
import sys
import time
import urllib.error
import urllib.request
from datetime import UTC, datetime
from email.utils import parsedate_to_datetime

INDEX = "https://index.crates.io"
USER_AGENT = "sealwax-release (https://github.com/sealwax/sealwax)"
# A token from trusted publishing lasts 30 minutes.
MAX_WAIT = 20 * 60


def log(message: str) -> None:
    print(f"{datetime.now(UTC):%H:%M:%S} {message}", flush=True)


def publishable() -> list[tuple[str, str]]:
    """The workspace members crates.io may receive, as (name, version), each after the members it
    depends on (development dependencies aside: cargo leaves them out of what it publishes)."""
    metadata = json.loads(
        subprocess.run(
            ["cargo", "metadata", "--no-deps", "--format-version", "1"],
            capture_output=True, text=True, check=True,
        ).stdout
    )
    members = {
        package["name"]: package
        for package in metadata["packages"]
        if package["id"] in metadata["workspace_members"] and package.get("publish") != []
    }
    ordered: list[tuple[str, str]] = []
    done: set[str] = set()

    def visit(name: str, path: tuple[str, ...]) -> None:
        if name in done:
            return
        if name in path:
            raise SystemExit(f"dependency cycle: {' -> '.join((*path, name))}")
        for dependency in members[name]["dependencies"]:
            if dependency["kind"] != "dev" and dependency["name"] in members:
                visit(dependency["name"], (*path, name))
        done.add(name)
        ordered.append((name, members[name]["version"]))

    for name in sorted(members):
        visit(name, ())
    return ordered


def index_path(name: str) -> str:
    name = name.lower()
    if len(name) <= 2:
        return f"{len(name)}/{name}"
    if len(name) == 3:
        return f"3/{name[0]}/{name}"
    return f"{name[:2]}/{name[2:4]}/{name}"


def published(name: str, version: str) -> bool:
    request = urllib.request.Request(f"{INDEX}/{index_path(name)}", headers={"User-Agent": USER_AGENT})
    try:
        with urllib.request.urlopen(request, timeout=30) as response:
            lines = response.read().decode().splitlines()
    except urllib.error.HTTPError as error:
        if error.code == 404:
            return False
        raise
    return any(json.loads(line)["vers"] == version for line in lines if line.strip())


def publish(name: str) -> None:
    waited = 0.0
    while True:
        result = subprocess.run(["cargo", "publish", "-p", name], capture_output=True, text=True, check=False)
        sys.stderr.write(result.stderr)
        if result.returncode == 0:
            log(f"published {name}")
            return
        limited = re.search(r"try again after ([^.]+ GMT)", result.stderr)
        if "429" not in result.stderr or not limited:
            raise SystemExit(f"cargo publish -p {name} failed")
        wait = (parsedate_to_datetime(limited.group(1)) - datetime.now(UTC)).total_seconds() + 10
        if waited + wait > MAX_WAIT:
            raise SystemExit(f"{name}: crates.io asks to wait until {limited.group(1)}; run this again then")
        log(f"{name}: crates.io limits the rate, waiting {int(wait)} s")
        time.sleep(max(wait, 0))
        waited += wait


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--dry-run", action="store_true", help="say what would be published")
    args = parser.parse_args()
    for name, version in publishable():
        if published(name, version):
            log(f"{name} {version} is already on crates.io")
        elif args.dry_run:
            log(f"{name} {version} would be published")
        else:
            publish(name)
    return 0


if __name__ == "__main__":
    sys.exit(main())
