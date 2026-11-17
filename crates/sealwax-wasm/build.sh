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

# WebAssembly SIMD (Chrome and Edge 91, Firefox 89, Safari 16.4, Node 16.4): the renderer's
# SIMD layer is a quarter of the module without it, in portable code.
CARGO_TARGET_WASM32_UNKNOWN_UNKNOWN_RUSTFLAGS="-C target-feature=+simd128" \
    cargo build --manifest-path "$root/Cargo.toml" -p sealwax-wasm \
    --target wasm32-unknown-unknown --profile wasm
rm -rf "${out:?}"
wasm-bindgen --target web --out-dir "$out" --out-name sealwax \
    "$root/target/wasm32-unknown-unknown/wasm/sealwax_wasm.wasm"

# Binaryen's optimizer, size first: with SIMD, the module goes from 7.7 MB to 4.4 MB (2.32 MB to
# 1.98 MB gzipped). The one on PATH, else Binaryen's npm package at a pinned version.
if command -v wasm-opt >/dev/null 2>&1; then
    optimize=(wasm-opt)
else
    optimize=(npx --yes -p binaryen@132.0.0 wasm-opt)
fi
"${optimize[@]}" -Oz --enable-bulk-memory --enable-nontrapping-float-to-int --enable-sign-ext \
    --enable-mutable-globals --enable-reference-types --enable-multivalue --enable-simd \
    "$out/sealwax_bg.wasm" -o "$out/sealwax_bg.wasm"

# The error classes the module builds: re-exported from the module's own copy, so that
# `instanceof` holds, with their declarations beside them.
errors="$(cd "$out" && find snippets -name errors.js | head -n 1)"
cp "$here/js/errors.d.ts" "$out/$(dirname "$errors")/errors.d.ts"
# A panic stops the module: its functions guarded, `reset()` to make another (see js/patch.mjs).
node "$here/js/patch.mjs" "$out/sealwax.js" "./$errors"
cat > "$out/index.js" <<JS
import init, { __sealwax_forget, __sealwax_is_crashed } from "./sealwax.js";

export { default, initSync } from "./sealwax.js";
export * from "./sealwax.js";
export * from "./$errors";

/** Whether the module has stopped, a panic aborting it: every call throws until \`reset()\`. */
export function isCrashed() {
  return __sealwax_is_crashed();
}

/** Make the module anew, as \`init\` does: the documents opened before are lost, open them again. */
export async function reset(module_or_path) {
  __sealwax_forget();
  return init(module_or_path);
}
JS
cat > "$out/index.d.ts" <<TS
import type { InitInput, InitOutput } from "./sealwax.js";

export { default, initSync } from "./sealwax.js";
export * from "./sealwax.js";
export * from "./$errors";

/** Whether the module has stopped, a panic aborting it: every call throws until \`reset()\`. */
export function isCrashed(): boolean;

/** Make the module anew, as \`init\` does: the documents opened before are lost, open them again. */
export function reset(
  module_or_path?: { module_or_path: InitInput | Promise<InitInput> } | InitInput | Promise<InitInput>,
): Promise<InitOutput>;
TS
# Node's fetch does not read file: URLs: under Node the package reads its module itself, and is
# ready once imported.
cat > "$out/node.js" <<JS
import { readFile } from "node:fs/promises";

import init, { reset as resetWith } from "./index.js";

export * from "./index.js";

const own = async () => ({ module_or_path: await readFile(new URL("./sealwax_bg.wasm", import.meta.url)) });

await init(await own());

/** As in a browser; the module read from the package when none is given. */
export default async function initialize(module_or_path) {
  return init(module_or_path ?? (await own()));
}

/** Make the module anew, read from the package when none is given. */
export async function reset(module_or_path) {
  return resetWith(module_or_path ?? (await own()));
}
JS

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
    ".": { "types": "./index.d.ts", "node": "./node.js", "default": "./index.js" },
    "./sealwax_bg.wasm": "./sealwax_bg.wasm"
  },
  "files": ["*.js", "*.d.ts", "*.wasm", "snippets", "licenses", "README.md", "LICENSE-MIT", "LICENSE-APACHE", "THIRD_PARTY_NOTICES.md"],
  "sideEffects": false,
  "keywords": ["pdf", "wasm", "webassembly", "forms", "flatten"]
}
JSON
size="$(wc -c < "$out/sealwax_bg.wasm" | tr -d ' ')"
echo "sealwax $version: $out ($size bytes of WebAssembly)"
