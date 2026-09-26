## Description
*What changed, why, and linked issue.*

## Verification Gate
I confirm the following commands were run and exited 0. **(Paste the terminal output below)**:

- [ ] `cargo fmt --all -- --check`
- [ ] `cargo clippy --all-targets --all-features --locked -- -D warnings`
- [ ] `RUSTFLAGS=-Dwarnings cargo test --all --locked`
- [ ] `cargo build --release --all --locked`
- [ ] `docker build -t so4-oracle .` *(only if Dockerfile or dependencies changed)*

## Verification Output
```text
(paste your successful command outputs here)