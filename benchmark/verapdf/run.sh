#!/usr/bin/env bash
# Validate PDFs with veraPDF, the reference PDF/A validator, in its official image without network.
#
#   benchmark/verapdf/run.sh DIR REPORT.json [FLAVOUR]
#
# DIR is a directory on the host (under the home directory, which Colima shares), mounted
# read-only. FLAVOUR is the profile to apply whatever the files declare: 3a by default, the level
# PDF/A-3a asks for. The JSON report then goes through summarize.py.
#
# One process: with --processes, the official image's veraPDF 1.28.2 waits forever (13/11/2026).
#
# veraPDF is GPL-3.0-or-later / MPL-2.0. It runs here as a separate tool and is never linked into
# sealwax.
set -euo pipefail

dir="$(cd "$1" && pwd)"
report="$2"
flavour="${3:-3a}"
image=verapdf/cli:v1.28.2

docker run --rm --network none --platform linux/amd64 -v "$dir:/data:ro" "$image" \
    --flavour "$flavour" --format json --recurse --maxfailuresdisplayed 3 /data \
    >"$report"
