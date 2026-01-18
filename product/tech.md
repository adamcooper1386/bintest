
---

# `tech.md`

```md
# Technical Design

## Overview

The tool is a standalone Rust CLI that:
- loads test specifications from disk
- validates them against a strict schema
- executes them in a controlled sandbox
- reports results deterministically

It is **not** coupled to Cargo or Rust projects.

## Architecture

### Components

1. CLI Interface (`run`, `validate`, `init`, `schema`)
2. Spec Loader & Validator
3. Sandbox Manager
4. Execution Engine
5. Assertion Engine
6. Reporter

### Data Flow

spec file → validated spec → execution plan
→ sandbox creation
→ setup steps
→ test steps
→ assertions
→ teardown
→ report


## Spec Format

- YAML (primary)
- TOML (optional)
- JSON (machine use)

All specs are validated against a versioned schema.

Invalid specs fail before execution.

### Schema Versioning

The `version:` field in spec files matches the crate's major version:
- `version: 1` → compatible with bintest 1.x
- `version: 2` → compatible with bintest 2.x

The schema can be retrieved via `bintest schema` for AI consumers to programmatically generate valid specs.

## Configuration Hierarchy

Three levels, each with optional setup/teardown:

1. **Suite level** - `bintest.yaml` in the run root
   - Defines defaults (timeout, env, etc.)
   - Suite-wide setup/teardown

2. **File level** - individual spec files
   - Each file gets its own sandbox
   - Files can run in parallel
   - File-level setup/teardown

3. **Test level** - tests within a file
   - Tests share the file's sandbox (can affect each other)
   - Can be marked serial or parallel
   - Serial tests run first, in declaration order
   - Parallel tests run after all serial tests complete
   - Test-level setup/teardown

## Sandbox Model

- Each test suite gets an isolated root directory
- Optionally backed by:
  - temp directory
  - user-specified path
- Environment variables are explicitly defined
- No implicit inheritance from host unless allowed

## Execution Model

- Commands are executed via `std::process::Command`
- Binary resolution: PATH lookup or absolute path
- No shell by default (shell optional, explicit)
- Signals captured

### Timeouts

- Default: 3 seconds per test
- Configurable at suite, file, and test level (most specific wins)
- Prevents infinite loops and hung processes

## Assertion Engine

Assertions are declarative and ordered.

Supported assertions:
- exit code equals
- stdout:
  - equals
  - contains
  - regex
- stderr:
  - equals
  - contains
  - regex
- file exists / not exists
- file contents match
- directory tree snapshot

Assertions are evaluated strictly and reported with diffs.

## Error Handling

- First failure halts the current test
- Teardown always runs
- All errors are structured
- Exit codes are deterministic

## Output Formats

- Human-readable (default)
- JSON (stable schema)
- Optional JUnit XML

## Determinism Guarantees

- Ordered execution
- No parallelism by default
- Explicit randomness (opt-in)
- Stable output ordering

## Installation

- Distributed as a single static binary
- Installed via:
  - `brew`
  - `cargo install`
  - direct download

## Why Rust

- Single static binary
- Strong typing for schema enforcement
- Excellent process control
- Predictable performance
- Easy cross-compilation

## AI-Friendliness Considerations

- Strict schema
- Minimal configuration surface
- Explicit defaults
- No hidden behavior
- Clear error messages
- Machine-readable output

An AI agent should be able to:
- generate a spec from scratch
- modify an existing spec safely
- reason about failures without inspecting code
