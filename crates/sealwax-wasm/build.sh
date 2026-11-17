#!/usr/bin/env bash
# Build the npm package `sealwax` (WebAssembly) into OUT_DIR (default: crates/sealwax-wasm/pkg).
#
#   crates/sealwax-wasm/build.sh [OUT_DIR]
#
# Needs the wasm32-unknown-unknown target (`rustup target add wasm32-unknown-unknown`) and the
# wasm-bindgen CLI at the version Cargo.toml pins (`cargo install wasm-bindgen-cli --version ...`).
# The module is a file of its own, sealwax_bg.wasm, which the caller loads from where it serves it
# (a bundler's `?url` import): nothing is fetched from elsewhere.
set -euo pipefail

here="$(cd "$(dirname "$0")" && pwd)"
root="$(cd "$here/../.." && pwd)"
out="${1:-$here/pkg}"
version="$(sed -n '/^\[workspace.package\]/,/^\[/s/^version = "\(.*\)"/\1/p' "$root/Cargo.toml")"
pinned="$(sed -n 's/^wasm-bindgen = "=\(.*\)"/\1/p' "$here/Cargo.toml")"
installed="$(wasm-bindgen --version | awk '{print $2}')"
if [ "$installed" != "$pinned" ]; then
    echo "wasm-bindgen $installed is installed, Cargo.toml pins $pinned" >&2
    exit 1
fi

cargo build --manifest-path "$root/Cargo.toml" -p sealwax-wasm \
    --target wasm32-unknown-unknown --profile wasm
rm -rf "${out:?}"
wasm-bindgen --target web --out-dir "$out" --out-name sealwax \
    "$root/target/wasm32-unknown-unknown/wasm/sealwax_wasm.wasm"

# The error classes the module builds: re-exported from the module's own copy, so that
# `instanceof` holds, with their declarations beside them.
errors="$(cd "$out" && find snippets -name errors.js | head -n 1)"
cp "$here/js/errors.d.ts" "$out/$(dirname "$errors")/errors.d.ts"
cat > "$out/index.js" <<JS
export { default, initSync } from "./sealwax.js";
export * from "./sealwax.js";
export * from "./$errors";
JS
cat > "$out/index.d.ts" <<TS
export { default, initSync } from "./sealwax.js";
export * from "./sealwax.js";
export * from "./$errors";
TS

# Licences: sealwax's, the third-party code and data copied into it, and the crates compiled into
# the module (cargo-about; the release build installs it).
cp "$root/LICENSE-MIT" "$root/LICENSE-APACHE" "$root/THIRD_PARTY_NOTICES.md" "$out/"
mkdir -p "$out/licenses"
cp "$root/crates/sealwax-annots/src/LICENSE-PDFIUM" "$root/crates/sealwax-pdfa/assets/fonts/LICENSE-FOXIT" \
    "$out/licenses/"
if cargo about --version >/dev/null 2>&1; then
    (cd "$here" && cargo about generate --locked -c about.toml -o "$out/licenses/DEPENDENCIES.md" about.hbs)
else
    echo "cargo-about is not installed: licenses/DEPENDENCIES.md left out (needed to publish)" >&2
fi
cp "$here/README.md" "$out/README.md"
cat > "$out/package.json" <<JSON
{
  "name": "sealwax",
  "version": "$version",
  "description": "A PDF library in Rust, compiled to WebAssembly for browsers and web views: forms, flattening, rendering, merging.",
  "license": "MIT OR Apache-2.0",
  "repository": { "type": "git", "url": "git+https://github.com/sealwax/sealwax.git" },
  "type": "module",
  "main": "./index.js",
  "types": "./index.d.ts",
  "exports": {
    ".": { "types": "./index.d.ts", "default": "./index.js" },
    "./sealwax_bg.wasm": "./sealwax_bg.wasm"
  },
  "files": ["*.js", "*.d.ts", "*.wasm", "snippets", "licenses", "README.md", "LICENSE-MIT", "LICENSE-APACHE", "THIRD_PARTY_NOTICES.md"],
  "sideEffects": false,
  "keywords": ["pdf", "wasm", "webassembly", "forms", "flatten"]
}
JSON
size="$(wc -c < "$out/sealwax_bg.wasm" | tr -d ' ')"
echo "sealwax $version: $out ($size bytes of WebAssembly)"
