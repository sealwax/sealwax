# Contributing to sealwax

Thank you for your interest in sealwax. Bug reports, fixes, tests, documentation and new
capabilities are all welcome.

## Before you start

- **Bugs:** open an issue with the smallest PDF or XFDF that shows the problem, if you can share
  one. Never attach a document that holds personal or confidential data: build a synthetic one
  instead, or describe its structure.
- **Larger changes:** open an issue first, so that we can agree on the approach before you write
  the code.
- **Security issues:** do not open a public issue; follow [SECURITY.md](SECURITY.md).

## Setting up

You need:

- Rust 1.97 or later (`rustup` installs it), with `clippy` and `rustfmt`. The published crates
  build with 1.92; `sealwax-convert` needs 1.97, for docxide-pdf, and so do the Python bindings,
  which convert DOCX with it;
- for the fuzz targets, a nightly toolchain and `cargo-fuzz`;
- for the Python bindings, Python 3.12 or later and [uv](https://docs.astral.sh/uv/) (or pip and
  maturin).

```sh
cargo build --workspace
cargo test --workspace --exclude sealwax-py
```

The Python bindings are built with maturin:

```sh
cd crates/sealwax-py
uv venv --python 3.12 .venv
uv pip install --python .venv/bin/python maturin pytest cryptography mypy
VIRTUAL_ENV=$PWD/.venv .venv/bin/maturin develop
.venv/bin/python -m mypy --strict python/sealwax
.venv/bin/python -m pytest -q tests
```

## What every change goes through

Continuous integration runs these checks on every pull request; run them locally first:

```sh
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace --exclude sealwax-py
```

It also runs the Python tests, the fuzz targets for a minute each, and `cargo-deny` for the
licences and advisories of dependencies.

## Rules of the code

- **No `unsafe`.** It is forbidden for the whole workspace.
- **No panic on any input.** No `unwrap`, `expect`, `panic!` or unchecked indexing on data that
  comes from a document. Clippy warns about the first three.
- **Untrusted input.** Anything read from a document can be hostile. Bound what you allocate or
  loop over, and check the document's `Limits` (`check_time`, `check_pages`) in long operations.
- **No network.** The library never reaches the network itself. What needs it takes a client from
  its caller.
- **Errors without content.** An error message may name a kind, a bound, an object number; never
  text, names or values from the document.
- **Tests with synthetic documents.** Tests build the PDFs they need. Do not add third-party
  documents to the repository unless their licence allows it, and record them in
  `THIRD_PARTY_NOTICES.md`.
- **Dependencies.** Only permissively licensed ones, compatible with MIT OR Apache-2.0. Copied or
  adapted code goes into `THIRD_PARTY_NOTICES.md`.

A new entry point should come with a fuzz target in `fuzz/`.

## Commits and pull requests

- Keep each pull request to one topic, with tests.
- Write commit messages in the imperative or descriptive present, prefixed by a type and, when
  useful, a scope: `feat(pdfa): ...`, `fix(sign): ...`, `docs: ...`, `chore: ...`.
- Explain in the pull request what changes for users, and how you checked it.

## License of contributions

Unless you explicitly state otherwise, any contribution intentionally submitted for inclusion in
sealwax by you, as defined in the Apache-2.0 license, shall be dual licensed under the MIT and
Apache-2.0 licenses, without any additional terms or conditions.
