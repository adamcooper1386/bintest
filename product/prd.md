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
- ORM-style assertions (use raw SQL)
- Schema diffing or database migrations
- Non-SQL databases (NoSQL, key-value stores)

This is **not** a test framework.
It is a **binary execution spec runner** that verifies all observable effects of running a binary, including external state like databases.

## Core Concept

Tests are defined as **declarative execution specs**.

Each spec:
- describes setup steps (filesystem, database state)
- defines one or more executable invocations
- asserts on outputs, exit codes, filesystem effects, and database state
- defines teardown explicitly
- runs sequentially and deterministically

No Rust code is required to write tests.

### Multi-Step Tests

Many real-world workflows require running multiple commands with state verification between each step. bintest supports multi-step tests where each step can have its own assertions:

```yaml
tests:
  - name: migration_rollback_workflow
    steps:
      - name: apply_migrations
        run:
          cmd: sqler
          args: ["migrate"]
        expect:
          exit: 0
          sql:
            - table_exists: users
      - name: rollback
        run:
          cmd: sqler
          args: ["migrate", "down"]
        expect:
          exit: 0
          sql:
            - table_not_exists: users
```

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
- Define environment variables (with interpolation support)
- Define working directory
- Define temp directories
- Define file fixtures
- Define command arguments
- Define stdin
- Define expected:
  - exit code
  - stdout / stderr (exact, contains, regex)
  - filesystem state
  - database state (SQL query results)

### Database Connections

Define database connections at suite, file, or test level:

```yaml
databases:
  default:
    driver: postgres
    url: "${DATABASE_URL}"
  root:
    driver: postgres
    url: "postgres://${ROOT_USER}:${ROOT_PASSWORD}@localhost:5432/postgres"
```

**Supported drivers:**
- `postgres` - PostgreSQL
- `sqlite` - SQLite (including `:memory:`)

**Connection lifecycle:**
- Lazy connections (opened on first use)
- Pooled per-file (shared across tests in a file)
- Closed after file teardown completes
- Connection errors show clear messages with masked passwords

### SQL Assertions

Assert database state after command execution:

```yaml
expect:
  sql:
    # Raw query with exact match
    - query: "SELECT COUNT(*) FROM users"
      returns: "3"

    # Query with contains/regex matching
    - query: "SELECT name FROM users"
      returns:
        contains: "alice"

    # Shorthand existence checks
    - table_exists: users
    - table_not_exists: temp_data

    # Row count assertions
    - row_count:
        table: users
        equals: 3

    # Empty/null checks
    - query: "SELECT * FROM deleted_users"
      returns_empty: true
```

### SQL Setup/Teardown

Execute SQL during setup and teardown:

```yaml
setup:
  - sql:
      database: root
      statements:
        - "DROP DATABASE IF EXISTS testdb"
        - "CREATE DATABASE testdb"
  - sql_file:
      database: default
      path: fixtures/schema.sql

teardown:
  - sql:
      database: root
      statements:
        - "DROP DATABASE IF EXISTS testdb"
      on_error: continue
```

### Binary Resolution

The binary under test is specified via the `binary` field at suite or file level:

```yaml
version: 1
binary: ../target/release/myapp  # relative to this file

tests:
  - name: help_works
    run:
      cmd: "${BINARY}"  # automatically set from binary field
      args: ["--help"]
    expect:
      exit: 0
```

**Resolution rules:**
- Paths are resolved relative to the file containing the `binary` field
- File-level `binary` overrides suite-level `binary`
- The resolved absolute path is available as `${BINARY}` in all commands
- Supports `${VAR}` syntax for environment variable interpolation in paths

**Hierarchy:**
- Suite-level (`bintest.yaml`): Default binary for all spec files
- File-level (spec file): Overrides suite-level for that file

**Path validation:**
- Binary path is resolved and validated at load time
- Missing or inaccessible binaries produce clear error messages

**Additional resolution:**
- Commands can still use PATH lookup or absolute paths directly
- Command paths support `${VAR}` environment variable expansion

### Setup / Teardown
- Explicit setup steps
- Explicit teardown steps
- Guaranteed teardown execution (even on failure)

### State Isolation
- Each test file runs in its own sandbox
- Tests within a file share the sandbox (can affect each other)
- No shared global state by default
- No implicit filesystem access
- Database state is NOT automatically isolated (use setup/teardown)

### Multi-Step Tests

Tests can contain multiple steps executed sequentially:

```yaml
tests:
  - name: workflow_test
    steps:
      - name: step_one
        run:
          cmd: my_cli
          args: ["init"]
        expect:
          exit: 0
      - name: step_two
        setup:
          - write_file:
              path: config.json
              contents: "{}"
        run:
          cmd: my_cli
          args: ["run"]
        expect:
          exit: 0
          sql:
            - row_count:
                table: results
                greater_than: 0
```

**Step execution semantics:**
- Steps execute sequentially within a test
- If any step fails, subsequent steps are skipped
- Each step can have its own setup and teardown
- Step-level setup runs after test-level setup
- Failure reporting shows which step failed

**Backward compatibility:** Single `run`/`expect` tests work as before (implicit single step).

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

### Basic CLI Test

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
```

### Database CLI Test (Multi-Step)

```yaml
version: 1

databases:
  default:
    driver: postgres
    url: "${DATABASE_URL}"
  root:
    driver: postgres
    url: "${ROOT_DATABASE_URL}"

setup:
  - sql:
      database: root
      statements:
        - "DROP DATABASE IF EXISTS test_db"
        - "CREATE DATABASE test_db"
      on_error: continue
  - copy_dir:
      from: fixtures/migrations
      to: sql/migrations

teardown:
  - sql:
      database: root
      statements:
        - "DROP DATABASE IF EXISTS test_db"
      on_error: continue

tests:
  - name: migration_rollback_workflow
    steps:
      - name: apply_all_migrations
        run:
          cmd: sqler
          args: ["migrate"]
        expect:
          exit: 0
          sql:
            - table_exists: users
            - table_exists: posts
            - row_count:
                table: _migrations
                equals: 2

      - name: rollback_one
        run:
          cmd: sqler
          args: ["migrate", "down"]
        expect:
          exit: 0
          sql:
            - table_exists: users
            - table_not_exists: posts
            - row_count:
                table: _migrations
                equals: 1
```

## Success Metrics

- AI agent can generate a valid test without human correction
- No Rust knowledge required to author tests
- Zero test harness code in user repos
- Tests are readable without context
- Failures are self-explanatory
- Database state assertions replace shell script helper functions


