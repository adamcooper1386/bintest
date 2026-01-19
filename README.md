# bintest

A declarative integration test runner for executables. Define tests in YAML or TOML, run them against any CLI tool.

## Installation

```bash
cargo install --path .
```

## Quick Start

Create a test file `tests/example.yaml`:

```yaml
version: 1

tests:
  - name: hello_world
    run:
      cmd: echo
      args: ["hello", "world"]
    expect:
      exit: 0
      stdout:
        contains: "hello"
```

Run it:

```bash
bintest run tests/example.yaml
```

## Features

- **Declarative tests** - Define expected behavior in YAML/TOML
- **Output matching** - Exact, contains, or regex patterns for stdout/stderr
- **Exit code & signal assertions** - Verify process termination
- **File assertions** - Check files exist and contain expected content
- **Directory tree snapshots** - Assert filesystem structure after execution
- **Filesystem diffs** - Track what files changed during tests
- **Environment control** - Set variables, inherit or isolate from host
- **Timeouts** - Per-test, per-file, or suite-wide limits
- **Setup/teardown** - File creation, directory setup, cleanup commands
- **Parallel execution** - Tests and files run concurrently by default
- **Multiple output formats** - Human-readable, JSON, or JUnit XML

## Test Specification

### Basic Structure

```yaml
version: 1

sandbox:
  workdir: temp          # "temp" for auto-cleanup, or a path
  env:
    MY_VAR: "value"
  inherit_env: false     # Don't inherit host environment

tests:
  - name: test_name
    run:
      cmd: my-cli
      args: ["--flag", "value"]
      stdin: "input data"
    expect:
      exit: 0
      stdout: "exact match"
      stderr:
        contains: "partial match"
```

### Output Matching

```yaml
# Exact match
stdout: "exact output\n"

# Structured matching
stdout:
  contains: "substring"

stdout:
  regex: "pattern \\d+"
```

### File Assertions

```yaml
expect:
  files:
    - path: output.txt
      exists: true
      contents:
        contains: "expected content"
    - path: should-not-exist.txt
      exists: false
```

### Directory Tree Assertions

```yaml
expect:
  tree:
    root: "."
    contains:
      - path: src/main.rs
      - path: Cargo.toml
        contents:
          contains: "[package]"
    excludes:
      - path: target/
```

### Signal Assertions (Unix)

```yaml
expect:
  signal: 9  # SIGKILL
```

### Setup and Teardown

```yaml
setup:
  - write_file:
      path: config.toml
      contents: |
        key = "value"
  - create_dir: data/
  - copy_file:
      src: fixtures/input.txt
      dest: input.txt
  - run:
      cmd: ./init.sh

teardown:
  - remove_file: temp.txt
  - remove_dir: cache/
```

### Test Ordering

```yaml
tests:
  - name: setup_first
    serial: true    # Runs before parallel tests
    run: ...

  - name: parallel_test_1
    run: ...        # Runs in parallel

  - name: parallel_test_2
    run: ...        # Runs in parallel
```

## Suite Configuration

Create `bintest.yaml` in your test directory:

```yaml
version: 1

# Default timeout for all tests (seconds)
timeout: 30

# Environment variables for all tests
env:
  RUST_LOG: debug

# Inherit host environment
inherit_env: true

# Run files serially instead of in parallel
serial: false

# Capture filesystem changes
capture_fs_diff: true

# Persist sandbox directories for debugging
sandbox_dir: local  # Creates .bintest/<timestamp>/
```

## CLI Usage

```bash
# Run tests
bintest run tests/
bintest run tests/specific.yaml

# Filter tests by name
bintest run tests/ --filter "test_name"

# Verbose output
bintest run tests/ --verbose

# Output formats
bintest run tests/ --output human   # Default
bintest run tests/ --output json
bintest run tests/ --output junit

# Persist sandbox for debugging
bintest run tests/ --sandbox-dir local
bintest run tests/ --sandbox-dir /tmp/debug

# Validate specs without running
bintest validate tests/

# Generate new spec file
bintest init tests/new.yaml

# Output JSON schema
bintest schema
```

## Examples

See the [examples/](examples/) directory for comprehensive examples:

- `basic.yaml` - Simple output matching
- `regex.yaml` - Pattern matching with regex
- `stdin.yaml` - Providing input to commands
- `files.yaml` - File existence and content assertions
- `tree.yaml` - Directory structure assertions
- `env.yaml` - Environment variable handling
- `timeout.yaml` - Timeout configuration
- `setup-teardown.yaml` - Test fixtures
- `fs-diff.yaml` - Filesystem change tracking
- `signals.yaml` - Signal assertion (Unix)
- `serial.yaml` - Test ordering control
- `sandbox-dir/` - Persistent sandbox directories

## License

MIT License - see [LICENSE](LICENSE)
