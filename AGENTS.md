Before committing changes that modify Rust source code or Rust build metadata
(such as `Cargo.toml`, `Cargo.lock`, or `rust-toolchain`), run all of the
following from the repository root and ensure they pass:

```sh
cargo fmt --all
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo nextest run --no-fail-fast
```

These commands are not required when the change set contains no Rust source or
Rust build metadata changes.

## Agent skills

### Issue tracker

Issues and specs are tracked as local Markdown files under `.scratch/`. See `docs/agents/issue-tracker.md`.

### Triage labels

Use the five canonical triage label strings unchanged. See `docs/agents/triage-labels.md`.

### Domain docs

Use the single-context domain-doc layout. See `docs/agents/domain.md`.
