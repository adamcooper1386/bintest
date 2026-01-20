
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
4. Database Connection Manager
5. Execution Engine
6. Assertion Engine (stdout/stderr, filesystem, SQL)
7. Reporter

### Data Flow

```
spec file → validated spec → execution plan
→ sandbox creation
→ database connections (lazy)
→ setup steps (filesystem + SQL)
→ for each test:
    → test setup
    → for each step:
        → step setup
        → command execution
        → assertions (stdout, stderr, files, SQL)
        → step teardown
    → test teardown
→ file teardown
→ close database connections
→ report
```


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
- Binary resolution: `binary` field, PATH lookup, or absolute path
- Command paths support `${VAR}` environment variable expansion
- No shell by default (shell optional, explicit)
- Signals captured

### Binary Under Test

The `binary` field specifies the executable being tested:

```yaml
version: 1
binary: ../target/release/myapp  # relative to this file

tests:
  - name: test_my_binary
    run:
      cmd: "${BINARY}"  # automatically populated from binary field
      args: ["--version"]
```

**Resolution:**
1. Path is resolved relative to the config file containing the `binary` field
2. Path is canonicalized to an absolute path at load time
3. The resolved path is injected as `BINARY` environment variable
4. File-level `binary` overrides suite-level `binary`

**Validation:**
- Binary path must exist and be accessible at load time
- Missing binaries produce clear error messages before tests run

### Command Path Interpolation

The `cmd` field supports `${VAR}` syntax for environment variable expansion:

```yaml
run:
  cmd: "${BINARY}"     # from binary field
  args: ["--version"]
```

The `${BINARY}` variable is automatically set when a `binary` field is defined.

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
- SQL query results:
  - equals (single value)
  - contains
  - regex
  - returns_empty
  - returns_null
  - returns_one_row
- SQL shorthands:
  - table_exists / table_not_exists
  - row_count (equals, greater_than, less_than)

Assertions are evaluated strictly and reported with diffs.

## Database Connection Manager

### Supported Drivers

- `postgres` - PostgreSQL via `tokio-postgres` or `sqlx`
- `sqlite` - SQLite via `rusqlite` or `sqlx`

### Connection String Formats

```
postgres://user:pass@host:port/dbname
sqlite:///path/to/file.db
sqlite::memory:
```

### Environment Variable Interpolation

Both command paths (`run.cmd`) and database connection URLs support `${VAR}` syntax:

```yaml
databases:
  default:
    driver: postgres
    url: "${DATABASE_URL}"
  root:
    driver: postgres
    url: "postgres://${ROOT_USER}:${ROOT_PASSWORD}@${DB_HOST}:5432/postgres"
```

Interpolation happens at runtime. Missing variables cause execution errors with clear messages.

### Connection Lifecycle

1. **Lazy initialization** - Connections open on first SQL operation
2. **Per-file pooling** - All tests in a file share connections
3. **Cleanup** - Connections close after file teardown completes
4. **Error handling** - Connection errors produce clear messages with masked passwords

### SQL Execution

- Statements execute synchronously
- Results are returned as newline-separated text (for multi-row)
- NULL values are handled explicitly (`returns_null: true`)
- Query errors are captured and reported with context

### SQL in Setup/Teardown

```yaml
setup:
  - sql:
      database: root
      statements:
        - "DROP DATABASE IF EXISTS testdb"
        - "CREATE DATABASE testdb"
      on_error: continue  # or "fail" (default)
  - sql_file:
      database: default
      path: fixtures/schema.sql
```

## Multi-Step Test Execution

### Step Model

Tests can contain multiple steps. Each step is a complete run/expect cycle:

```yaml
tests:
  - name: workflow
    steps:
      - name: step_one
        run: ...
        expect: ...
      - name: step_two
        setup: ...
        run: ...
        expect: ...
        teardown: ...
```

### Execution Order

1. Test-level setup runs once
2. For each step (in order):
   - Step-level setup runs
   - Command executes
   - Assertions evaluate
   - Step-level teardown runs
3. Test-level teardown runs (always, even on failure)

### Failure Handling

- If a step fails, remaining steps are skipped
- Test-level teardown still runs
- Failure reports include step name and index

### Backward Compatibility

Tests with single `run`/`expect` (no `steps`) work unchanged. They are treated as a single implicit step.

## Error Handling

- First failure halts the current test (but not other steps' teardown)
- Teardown always runs
- All errors are structured
- Exit codes are deterministic

### SQL Error Reporting

SQL assertion failures include full context:

```
FAILED: test_migration_creates_tables
  Step: verify_tables
  Assertion: sql[0]
    Query: SELECT COUNT(*) FROM _migrations
    Expected: "3"
    Actual: "2"
    Database: default
```

Connection errors mask passwords:

```
FAILED: test_migration_creates_tables
  Setup error: Failed to connect to database 'default'
    URL: postgres://user:****@localhost:5432/testdb
    Error: connection refused
```

SQL execution errors include query context:

```
FAILED: test_migration_creates_tables
  Step: verify_tables
  Assertion: sql[1]
    Query: SELECT COUNT(*) FROM nonexistent_table
    Error: relation "nonexistent_table" does not exist
    Database: default
```

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

## Implementation Phases

### Phase 1: Core Database Support
1. Database connection configuration
2. Basic SQL assertions (query + returns)
3. SQL in setup/teardown

### Phase 2: Enhanced Assertions
4. Existence assertion shorthands (table_exists, row_count)
5. SQL file execution (sql_file)
6. Enhanced error reporting with masked passwords

### Phase 3: Multi-Step Tests
7. Steps within tests
8. Directory copying (copy_dir)

### Phase 4: Advanced Features (Future)
9. Conditional test execution (skip_if, require)
10. Database snapshots (db_snapshot, db_restore)
11. Per-file database isolation

## Open Questions

1. **Connection pooling** - Should connections be per-file or per-test?
   - Decision: Per-file (shared across tests) for performance

2. **Transaction wrapping** - Auto-rollback tests?
   - Decision: No. May hide transaction bugs. Use explicit setup/teardown.

3. **Result format normalization** - Handle driver differences?
   - Decision: Document driver-specific formats. Users write portable queries.

4. **Large result sets** - Truncate or stream?
   - Decision: Truncate with warning after reasonable limit (e.g., 1000 rows)
