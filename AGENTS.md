# Autonomous Agent Directives

Read and execute these instructions strictly before committing code or opening a pull request.

## 1. Project Orientation
- **Binary**: This is an Axum service running the price loop, the keeper loop, and the HTTP API.
- **Workspace Layout**: Core logic lives in `oracle/`, and shared configuration is in `shared/config/`.
- **Integration Tests**: Located exclusively in `oracle/tests/`. **Never** create or use a root `tests/` directory—the root manifest is virtual and will silently ignore it.

## 2. The Verification Gate (Non-Negotiable)
These commands MUST all exit 0 before any git commit is made, and again before a PR is opened or updated:

`cargo fmt --all -- --check`
`cargo clippy --all-targets --all-features --locked -- -D warnings`
`RUSTFLAGS=-Dwarnings cargo test --all --locked`
`cargo build --release --all --locked`

If the `Dockerfile` or dependencies changed, you must also run:
`docker build -t so4-oracle .`

**Explicit Rules:**
- A local `cargo build` passing locally is not sufficient. CI sets `RUSTFLAGS="-Dwarnings"`, so a warning is a build failure there. Always reproduce with the flag set.
- **Never** silence a lint with `#[allow(...)]` to get through the gate. Resolve the underlying cause or state in the PR why the allow is correct.
- **Never** commit with failing or ignored tests. `#[ignore]` requires a linked issue.
- If CI fails after a push, fix forward in the same PR. Do not merge on a red pipeline.

## 3. PR Standards
- Use Conventional Commits matching the existing history (`feat:`, `fix:`, `test(scope):`).
- One logical change per PR.
- Use imperative, present-tense titles.
- The PR body must cover: what changed, why, how it was verified (pasted command output), and the linked issue.
- Every PR states which of the gate commands were run and their results. No PR is opened for work that has not been run.

## 4. Repository-Specific Traps
- **Single-Instance Deployment**: Must run as exactly one instance (no horizontal scaling). In-flight submission locks (`in_flight_keys`), failure counts, and execution blacklists live in in-process memory (`AppState`). Running multiple replicas causes competing keeper transactions and duplicate execution races (#1019).
- **Environment Variables**: Read verbatim by `Config::from_env()`. A near-miss like `ORDER_HANDLER_CONTRACT_ID` is silently ignored and the process exits.
- **Tokens Config**: `config/tokens.json` is embedded via `include_str!` at compile time and must exist in the Docker builder stage before `cargo build`.
- **Panics**: `[profile.release]` sets `panic = "abort"`. The keeper loop must never panic—errors are recorded and the loop continues.
- **MSRV**: The Dockerfile base image encodes the real MSRV. Do not bump dependencies without checking it.
- **Admin Routes**: Admin endpoints authenticate via the `AdminAuth` extractor (`oracle/src/api/mod.rs`). New admin routes must take it.

## 5. Secrets and Safety
- **Never** commit `.dev.vars`, keeper secret keys, or admin tokens.
- `SecretString` exists so keys do not land in logs—never `Debug`-print a config value that holds one.
- **Never** point tests or examples at mainnet RPC.