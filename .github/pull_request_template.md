## What changes

<!-- What this pull request changes for users, and why. Link the issue it addresses. -->

## How it was checked

<!-- Tests added or run, and anything checked by hand (veraPDF, a PDF reader, pyHanko...). -->

- [ ] `cargo fmt --all --check`, `cargo clippy --workspace --all-targets` and `cargo test --workspace --exclude sealwax-py` pass
- [ ] New entry points come with a fuzz target; errors quote no document content
- [ ] No third-party document or code added without its licence in `THIRD_PARTY_NOTICES.md`
