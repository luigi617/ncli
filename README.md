# ncli

Define workflows that chain other CLIs: one named command runs an ordered sequence of external CLI invocations.

## Development

Requires a stable [Rust toolchain](https://rustup.rs/).

```sh
cargo run -- --version                      # run the CLI
cargo test                                  # run tests
cargo clippy --all-targets -- -D warnings   # lint
cargo fmt                                   # format
cargo install --path .                      # install ncli
```
