# Product Requirements Document

## Product Name (working)
bintest  
(or: execspec, runbook, bincheck — name is secondary to clarity)

## Problem Statement

Writing integration tests for binaries in Rust is unnecessarily complex.

Common issues:
- Test logic is embedded in Rust code (`#[test]`), not data
- Setup/teardown is implicit and inconsistent
- State leaks across tests
- Output assertions are ad-hoc
- AI agents must understand Rust test frameworks to contribute

This leads to:
- fragile tests
- duplicated harness logic
- low adoption of integration testing
- high friction for AI-driven development

## Target Users

### Primary
- Developers building CLI tools, services, or agents
- AI agents generating tests autonomously

### Secondary
- CI systems
- Tooling authors
- Platform teams standardizing test execution

## Non-Goals

- Replacing unit tests
- Deep mocking or stubbing
- Language-specific assertions
- Property-based testing
- Performance benchmarking

This is **not** a test framework.  
It is a **binary execution spec runner**.

## Core Concept

Tests are defined as **declarative execution specs**.

Each spec:
- describes setup steps
- defines one or more executable invocations
- asserts on outputs, exit codes, and filesystem effects
- defines teardown explicitly
- runs sequentially and deterministically

No Rust code is required to write tests.

## User Experience

### Authoring
- Tests are written in YAML or TOML
- One file can define multiple scenarios
- Files are readable, diffable, and AI-friendly
- Schema can be retrieved via `bintest schema` for AI consumers

### Execution
- Installed as a local CLI tool
- Commands:
  - `bintest run ./tests` - execute test specs
  - `bintest validate ./tests` - check specs without running
  - `bintest init` - scaffold a new spec file
  - `bintest schema` - output the spec schema (for AI consumers)
- Works outside of Cargo
- Produces structured, machine-readable output (JSON)

### CI
- Non-zero exit code on failure
- Optional JUnit / JSON output
- Deterministic ordering

## Schema Versioning

The YAML spec `version:` field matches the crate's major version. A spec with `version: 1` works with bintest 1.x.

## Hierarchy

Three levels of configuration:

1. **Suite level** (`bintest.yaml` in root)
   - Setup/teardown for entire run
   - Default timeout and other settings
   - Applies to all test files

2. **File level** (individual spec files)
   - Setup/teardown per file
   - Each file gets its own sandbox
   - Files can run in parallel

3. **Test level** (tests within a file)
   - Setup/teardown per test
   - Tests share the file's sandbox
   - Tests can be serial or parallel
   - Serial tests run first, in order, before parallel tests

## Functional Requirements

### Test Definition
- Define environment variables
- Define working directory
- Define temp directories
- Define file fixtures
- Define command arguments
- Define stdin
- Define expected:
  - exit code
  - stdout / stderr (exact, contains, regex)
  - filesystem state

### Binary Resolution
- Binaries are resolved via PATH or absolute path
- No implicit resolution from sandbox

### Setup / Teardown
- Explicit setup steps
- Explicit teardown steps
- Guaranteed teardown execution (even on failure)

### State Isolation
- Each test runs in its own sandbox
- No shared global state by default
- No implicit filesystem access

### Observability
- Capture stdout, stderr
- Capture execution time
- Capture filesystem diffs
- Optional verbose tracing

### Timeouts
- Default timeout: 3 seconds per test
- Configurable at suite, file, and test level
- Prevents infinite loops and hung processes

## Example Test (Illustrative)

```yaml
version: 1

sandbox:
workdir: temp
env:
  RUST_LOG: debug

setup:
- write_file:
    path: config.toml
    contents: |
      mode = "test"

tests:
- name: init_creates_state
  run:
    cmd: my_binary
    args: ["init"]
  expect:
    exit: 0
    stdout:
      contains: "initialized"
    files:
      - path: state.json
        exists: true

teardown:
- remove_dir: sandbox

Success Metrics

AI agent can generate a valid test without human correction

No Rust knowledge required to author tests

Zero test harness code in user repos

Tests are readable without context

Failures are self-explanatory


