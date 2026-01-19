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
- **Database assertions** - Verify SQL query results, table existence, row counts
- **Multi-step workflows** - Run sequences of commands with state verification between steps
- **Conditional execution** - Skip or require tests based on environment or command availability
- **Environment control** - Set variables, inherit or isolate from host
- **Timeouts** - Per-test, per-file, or suite-wide limits
- **Setup/teardown** - File creation, directory setup, SQL execution, cleanup
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
      from: fixtures/input.txt
      to: input.txt
  - copy_dir:
      from: fixtures/migrations
      to: sql/migrations
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

### Multi-Step Tests

Run multiple commands in sequence with assertions after each step:

```yaml
tests:
  - name: build_workflow
    steps:
      - name: init
        run:
          cmd: my-cli
          args: ["init"]
        expect:
          exit: 0
          files:
            - path: .config
              exists: true

      - name: build
        run:
          cmd: my-cli
          args: ["build"]
        expect:
          exit: 0
          stdout:
            contains: "Build complete"

      - name: verify
        run:
          cmd: my-cli
          args: ["status"]
        expect:
          exit: 0
```

Steps execute sequentially. If any step fails, remaining steps are skipped. Each step can have its own setup and teardown.

### Conditional Execution

Skip tests or require conditions to be met:

```yaml
tests:
  # Skip when environment variable is set
  - name: skip_in_ci
    skip_if:
      - env: CI
    run: ...

  # Require environment variable to be set
  - name: needs_database
    require:
      - env: DATABASE_URL
    run: ...

  # Require a command to be available
  - name: needs_git
    require:
      - cmd: git --version
    run: ...

  # Multiple conditions (all must be met for require, any triggers skip_if)
  - name: complex_conditions
    require:
      - env: API_KEY
      - cmd: docker --version
    skip_if:
      - env: SKIP_SLOW_TESTS
    run: ...
```

## Database Testing

### Database Configuration

Define database connections at the file or suite level:

```yaml
databases:
  default:
    driver: sqlite
    url: "sqlite::memory:"

  postgres:
    driver: postgres
    url: "${DATABASE_URL}"  # Environment variable interpolation
```

Supported drivers: `sqlite`, `postgres`

### SQL Assertions

Verify database state after command execution:

```yaml
expect:
  sql:
    # Query with exact match
    - query: "SELECT COUNT(*) FROM users"
      returns: "3"

    # Query with pattern matching
    - query: "SELECT name FROM users"
      returns:
        contains: "alice"

    # Table existence checks
    - table_exists: users
    - table_not_exists: temp_data

    # Row count assertions
    - row_count:
        table: users
        equals: 3
    - row_count:
        table: logs
        greater_than: 0

    # Empty/null checks
    - query: "SELECT * FROM deleted"
      returns_empty: true
    - query: "SELECT optional_field FROM config"
      returns_null: true
    - query: "SELECT * FROM users WHERE id = 1"
      returns_one_row: true
```

### SQL Setup and Teardown

Execute SQL during setup and teardown:

```yaml
setup:
  - sql:
      database: default
      statements:
        - "CREATE TABLE users (id INTEGER PRIMARY KEY, name TEXT)"
        - "INSERT INTO users (name) VALUES ('alice')"

  # Execute SQL from a file
  - sql_file:
      database: default
      path: fixtures/schema.sql

teardown:
  - sql:
      database: default
      statements:
        - "DROP TABLE IF EXISTS users"
      on_error: continue  # Don't fail on cleanup errors
```

### Database Snapshots

Save and restore database state (SQLite only):

```yaml
setup:
  - sql:
      statements:
        - "CREATE TABLE users (id INTEGER, name TEXT)"
        - "INSERT INTO users VALUES (1, 'alice')"

  # Save initial state
  - db_snapshot:
      database: default
      name: baseline

tests:
  - name: modify_and_restore
    setup:
      - sql:
          statements:
            - "DELETE FROM users"
    run: ...
    teardown:
      # Restore to saved state
      - db_restore:
          database: default
          name: baseline
```

### Per-File Database Isolation

Automatically reset database state before each test:

```yaml
databases:
  default:
    driver: sqlite
    url: "sqlite::memory:"
    isolation: per_file  # Each test gets fresh state from file setup

setup:
  - sql:
      statements:
        - "CREATE TABLE counter (value INTEGER)"
        - "INSERT INTO counter VALUES (0)"

tests:
  - name: increment
    # Modifies counter, but next test still sees 0
    ...

  - name: still_zero
    # Database was reset to post-setup state
    ...
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
- `fs-diff.yaml` - Filesystem change tracking
- `signals.yaml` - Signal assertions (Unix)
- `parallel.yaml` - Parallel and serial test execution
- `steps.yaml` - Multi-step test workflows
- `copy-dir.yaml` - Directory copying in setup
- `sql.yaml` - Database assertions and SQL setup/teardown
- `workflow.yaml` - Multi-step database workflow
- `conditional.yaml` - Conditional test execution (skip_if, require)
- `db-snapshot.yaml` - Database snapshot and restore
- `db-isolation.yaml` - Per-file database isolation
- `sandbox-dir/` - Persistent sandbox directories
- `suite-config/` - Suite-level configuration

## License

MIT License - see [LICENSE](LICENSE)
