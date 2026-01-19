# CLAUDE.md

## Project: bintest

A declarative integration test runner for executables.

## Session Start

Before working on this project, read these files in order:

1. `product/mission.md` - Core principles and goals
2. `product/prd.md` - What to build and why
3. `product/tech.md` - Technical architecture and structure

## Development Workflow

1. **Read** - Study the product docs (see above)
2. **History** - Review recent git history
3. **Check** - Does the task align with prd.md?
4. **Implement** - Write code that satisfies the PRD
5. **Verify** - Run `cargo check` and `cargo test` after changes
6. **Examples** - Add examples of implemented feature to examples directory
7. **Verify Examples** - Verify that all examples still pass
8. **Commit** - Only after all checks pass with no warnings IMPORTANT! CHANGES MUST BE COMMITED AT THE END OF THE WORKFLOW. THE HUMAN REVIEWS COMMITS.

If requirements are unclear or missing, update prd.md first.

## Build & Test

```bash
cargo fmt                         # Format first
cargo check                       # Fast compile check
cargo clippy -- -D warnings       # Lint (warnings are errors)
cargo test                        # Run unit tests
cargo build                       # Full build
cargo run                         # Run the binary
```

Run after every change:
```bash
cargo fmt && cargo check && cargo clippy -- -D warnings && cargo test
```

## Before Committing

All of these must pass with **zero warnings**:
- `cargo fmt --check`
- `cargo clippy -- -D warnings`
- `cargo test`

## Code Style

- Standard Rust idioms
- Explicit over implicit
- Small, focused functions
- Document public APIs
- Treat warnings as errors
