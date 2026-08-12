Before committing changes, run all of the following from the repository root and ensure they pass:

```sh
cargo fmt --all
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo nextest run --no-fail-fast
```
