# Database CLI Testing Requirements

## Overview

This document specifies features required for bintest to support comprehensive integration testing of database CLI tools like sqler, pgloader, flyway, and similar utilities. These tools share common testing patterns that go beyond standard CLI testing.

## Background

Database CLI tools require integration tests that verify:
1. CLI behavior (flags, output, exit codes) - **currently supported**
2. Database state changes (tables created, rows inserted, migrations tracked) - **not supported**
3. Multi-step workflows where database state must be verified between commands - **not supported**

### Reference Implementation

This requirements document is based on analysis of [sqler](https://github.com/adamcooper1386/sqler)'s integration test suite, which uses shell scripts with helper functions for database assertions. The goal is to enable a clean declarative replacement.

---

## Feature 1: Database Connections

### Problem

Database CLI tests need to execute SQL queries against a database to verify state. Currently, users must shell out to `psql` or similar tools, which is verbose and error-prone.

### Requirements

#### 1.1 Connection Configuration

Define database connections at suite, file, or test level:

```yaml
version: 1

databases:
  default:
    driver: postgres
    url: "postgres://user:pass@localhost:5432/testdb"
  root:
    driver: postgres
    url: "postgres://postgres:postgres@localhost:5432/postgres"
```

**Supported drivers (initial):**
- `postgres` - PostgreSQL via native driver
- `sqlite` - SQLite via native driver

**Connection string formats:**
- PostgreSQL: `postgres://user:pass@host:port/dbname`
- SQLite: `sqlite:///path/to/file.db` or `sqlite::memory:`

#### 1.2 Environment Variable Interpolation

Connection URLs should support environment variable interpolation:

```yaml
databases:
  default:
    driver: postgres
    url: "${DATABASE_URL}"
  root:
    driver: postgres
    url: "postgres://${TEST_ROOT_USER}:${TEST_ROOT_PASSWORD}@${TEST_DB_HOST}:${TEST_DB_PORT}/postgres"
```

#### 1.3 Connection Lifecycle

- Connections are lazy (opened on first use)
- Connections are pooled per-file (shared across tests in a file)
- Connections are closed after file teardown completes
- Connection errors produce clear error messages with masked passwords

---

## Feature 2: SQL Assertions

### Problem

Database tests need to verify state via SQL queries. Common patterns include checking if tables exist, counting rows, and verifying specific values.

### Requirements

#### 2.1 Basic SQL Assertions

Add `expect.sql` for database state verification:

```yaml
tests:
  - name: migration_creates_tables
    run:
      cmd: sqler
      args: ["migrate"]
    expect:
      exit: 0
      sql:
        - query: "SELECT EXISTS (SELECT FROM information_schema.tables WHERE table_name = 'users')"
          returns: "t"
        - query: "SELECT COUNT(*) FROM _sqler_migrations"
          returns: "3"
```

#### 2.2 Named Database Connections

Specify which connection to use (defaults to `default`):

```yaml
expect:
  sql:
    - database: root
      query: "SELECT 1 FROM pg_database WHERE datname = 'myapp'"
      returns: "1"
    - database: default
      query: "SELECT COUNT(*) FROM users"
      returns: "5"
```

#### 2.3 Result Matching Options

Support the same matching patterns as stdout/stderr:

```yaml
expect:
  sql:
    # Exact match (single value)
    - query: "SELECT COUNT(*) FROM users"
      returns: "3"

    # Contains match
    - query: "SELECT name FROM users ORDER BY name"
      returns:
        contains: "alice"

    # Regex match
    - query: "SELECT created_at FROM users LIMIT 1"
      returns:
        regex: "\\d{4}-\\d{2}-\\d{2}"

    # Multi-row results (newline-separated)
    - query: "SELECT name FROM users ORDER BY name"
      returns: |
        alice
        bob
        charlie
```

#### 2.4 Existence Assertions

Shorthand for common existence checks:

```yaml
expect:
  sql:
    # Table exists
    - table_exists: users
    - table_exists: posts
      database: default

    # Table does not exist
    - table_not_exists: temp_data

    # Row count
    - row_count:
        table: users
        equals: 3

    # Row count comparison
    - row_count:
        table: audit_log
        greater_than: 0
```

These expand internally to the appropriate SQL for the database driver.

#### 2.5 Null and Empty Handling

```yaml
expect:
  sql:
    # Query returns no rows
    - query: "SELECT * FROM users WHERE id = 999"
      returns_empty: true

    # Query returns NULL
    - query: "SELECT deleted_at FROM users WHERE id = 1"
      returns_null: true

    # Query returns exactly one row
    - query: "SELECT id FROM users WHERE email = 'admin@test.com'"
      returns_one_row: true
```

---

## Feature 3: SQL Setup and Teardown

### Problem

Database tests often need to set up database state before tests and clean up after. Currently this requires shell commands or the CLI being tested.

### Requirements

#### 3.1 SQL in Setup/Teardown

Execute SQL statements during setup and teardown:

```yaml
setup:
  - sql:
      database: root
      statements:
        - "DROP DATABASE IF EXISTS testdb"
        - "DROP USER IF EXISTS testuser"
        - "CREATE USER testuser WITH PASSWORD 'testpass'"
        - "CREATE DATABASE testdb OWNER testuser"

teardown:
  - sql:
      database: root
      statements:
        - "DROP DATABASE IF EXISTS testdb"
        - "DROP USER IF EXISTS testuser"
```

#### 3.2 SQL File Execution

Execute SQL from files:

```yaml
setup:
  - sql_file:
      database: default
      path: fixtures/schema.sql
  - sql_file:
      database: default
      path: fixtures/seed-data.sql
```

#### 3.3 Execution Options

```yaml
setup:
  - sql:
      database: default
      statements:
        - "DELETE FROM users"
      on_error: continue  # or "fail" (default)
```

---

## Feature 4: Multi-Step Tests

### Problem

Database workflows often require running multiple commands with state verification between each step. Example: run migrations, verify tables exist, run rollback, verify tables removed.

### Requirements

#### 4.1 Steps Within a Test

Allow multiple run/expect pairs within a single test:

```yaml
tests:
  - name: rollback_workflow
    steps:
      - name: apply_migrations
        run:
          cmd: sqler
          args: ["migrate"]
        expect:
          exit: 0
          sql:
            - row_count:
                table: _sqler_migrations
                equals: 3
            - table_exists: users
            - table_exists: posts

      - name: rollback_one
        run:
          cmd: sqler
          args: ["migrate", "down"]
        expect:
          exit: 0
          sql:
            - row_count:
                table: _sqler_migrations
                equals: 2
            - table_exists: users
            - table_not_exists: posts

      - name: rollback_all
        run:
          cmd: sqler
          args: ["migrate", "down", "2"]
        expect:
          exit: 0
          sql:
            - row_count:
                table: _sqler_migrations
                equals: 0
            - table_not_exists: users
```

#### 4.2 Step Execution Semantics

- Steps execute sequentially within a test
- If any step fails, subsequent steps are skipped
- Each step can have its own `setup` and `teardown`
- Step-level setup runs after test-level setup
- Failure reporting shows which step failed

#### 4.3 Step-Level Setup

```yaml
tests:
  - name: modified_migration_detection
    steps:
      - name: apply_migrations
        run:
          cmd: sqler
          args: ["migrate"]
        expect:
          exit: 0

      - name: modify_file_and_check_status
        setup:
          - write_file:
              path: sql/migrations/01-create-users.sql
              contents: |
                -- Modified for test
                CREATE TABLE users (id SERIAL PRIMARY KEY);
        run:
          cmd: sqler
          args: ["migrate", "--status"]
        expect:
          stdout:
            regex: "modified|changed|mismatch"
        teardown:
          - copy_file:
              from: fixtures/original/01-create-users.sql
              to: sql/migrations/01-create-users.sql
```

#### 4.4 Backward Compatibility

Single `run`/`expect` tests continue to work as before (implicit single step).

---

## Feature 5: Test Fixtures with Snapshots

### Problem

Database tests often need to reset to a known state. Copying fixture files or restoring database dumps is common.

### Requirements

#### 5.1 Directory Copying in Setup

```yaml
setup:
  - copy_dir:
      from: fixtures/sql
      to: sql
```

#### 5.2 Database Snapshots (Future Enhancement)

```yaml
setup:
  - db_snapshot:
      database: default
      name: clean_state

# ... tests run ...

teardown:
  - db_restore:
      database: default
      name: clean_state
```

This uses database-specific mechanisms (pg_dump/pg_restore, SQLite backup API).

---

## Feature 6: Enhanced Error Reporting

### Problem

When database assertions fail, users need clear information about what was expected vs. what was found.

### Requirements

#### 6.1 SQL Assertion Failure Messages

```
FAILED: test_migration_creates_tables
  Step: verify_tables
  Assertion: sql[0]
    Query: SELECT COUNT(*) FROM _sqler_migrations
    Expected: "3"
    Actual: "2"
    Database: default
```

#### 6.2 Connection Error Messages

```
FAILED: test_migration_creates_tables
  Setup error: Failed to connect to database 'default'
    URL: postgres://user:****@localhost:5432/testdb
    Error: connection refused
```

#### 6.3 SQL Execution Error Messages

```
FAILED: test_migration_creates_tables
  Step: verify_tables
  Assertion: sql[1]
    Query: SELECT COUNT(*) FROM nonexistent_table
    Error: relation "nonexistent_table" does not exist
    Database: default
```

---

## Feature 7: Parallel Execution Safety

### Problem

Database tests often cannot run in parallel because they share database state.

### Requirements

#### 7.1 File-Level Serial Execution

```yaml
# bintest.yaml (suite config)
version: 1
serial: true  # All test files run sequentially
```

#### 7.2 Test-Level Serial Marking

```yaml
tests:
  - name: database_setup
    serial: true  # This test runs alone
    run: ...

  - name: read_only_test_1
    run: ...  # Can run in parallel with other non-serial tests

  - name: read_only_test_2
    run: ...
```

#### 7.3 Database-Level Isolation (Future Enhancement)

Automatically create isolated databases per test file:

```yaml
databases:
  default:
    driver: postgres
    template: "postgres://root@localhost/template_db"
    isolation: per_file  # Creates unique DB per test file
```

---

## Feature 8: Conditional Test Execution

### Problem

Some tests should only run under certain conditions (e.g., PostgreSQL version, environment).

### Requirements

#### 8.1 Skip Conditions

```yaml
tests:
  - name: test_advisory_locks
    skip_if:
      - env_missing: "TEST_ROOT_PASSWORD"
    run: ...

  - name: test_pg15_features
    skip_if:
      - sql:
          database: default
          query: "SHOW server_version_num"
          less_than: "150000"
    run: ...
```

#### 8.2 Require Conditions

```yaml
tests:
  - name: test_requires_root
    require:
      - sql:
          database: root
          query: "SELECT 1"
          succeeds: true
    run: ...
```

---

## Implementation Priority

### Phase 1: Core Database Support
1. Database connection configuration (Feature 1)
2. Basic SQL assertions (Feature 2.1-2.3)
3. SQL in setup/teardown (Feature 3.1)

### Phase 2: Enhanced Assertions
4. Existence assertion shorthands (Feature 2.4-2.5)
5. SQL file execution (Feature 3.2)
6. Enhanced error reporting (Feature 6)

### Phase 3: Multi-Step Tests
7. Steps within tests (Feature 4)
8. Directory copying (Feature 5.1)

### Phase 4: Advanced Features
9. Conditional execution (Feature 8)
10. Database snapshots (Feature 5.2)
11. Database isolation (Feature 7.3)

---

## Example: Complete sqler Test Conversion

### Before (Shell Script)

```bash
#!/bin/bash
# Test: Migration rollback functionality

test_name "migrate down rolls back one migration"
setup_fresh_db
sqler_cmd migrate >/dev/null 2>&1

assert_table_exists "users"
assert_table_exists "posts"
assert_table_exists "comments"
assert_migration_count 3

sqler_cmd migrate down >/dev/null 2>&1

assert_table_not_exists "comments"
assert_table_exists "posts"
assert_table_exists "users"
assert_migration_count 2
```

### After (bintest YAML)

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
        - "DROP DATABASE IF EXISTS sqler_test_db"
        - "DROP USER IF EXISTS sqler_test"
        - "CREATE USER sqler_test WITH PASSWORD 'sqler_test'"
        - "CREATE DATABASE sqler_test_db OWNER sqler_test"
      on_error: continue
  - copy_dir:
      from: fixtures/sql
      to: sql

teardown:
  - sql:
      database: root
      statements:
        - "DROP DATABASE IF EXISTS sqler_test_db"
        - "DROP USER IF EXISTS sqler_test"
      on_error: continue

tests:
  - name: migrate_down_rolls_back_one_migration
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
            - table_exists: comments
            - row_count:
                table: _sqler_migrations
                equals: 3

      - name: rollback_one
        run:
          cmd: sqler
          args: ["migrate", "down"]
        expect:
          exit: 0
          sql:
            - table_exists: users
            - table_exists: posts
            - table_not_exists: comments
            - row_count:
                table: _sqler_migrations
                equals: 2
```

---

## Non-Goals

The following are explicitly out of scope:

1. **ORM-style assertions** - No `expect.records` with object matching. Use raw SQL.
2. **Schema diffing** - No automatic schema comparison. Use explicit queries.
3. **Database migrations** - bintest doesn't manage schemas, only asserts against them.
4. **Non-SQL databases** - Focus on SQL databases (PostgreSQL, SQLite, MySQL).
5. **GUI/visual testing** - CLI-only scope.

---

## Open Questions

1. **Connection pooling strategy** - Should connections be shared across tests in a file, or isolated per-test?

2. **Transaction wrapping** - Should tests automatically run in a transaction that rolls back? (Speeds up tests but may hide transaction-related bugs)

3. **Async query execution** - Should multiple SQL assertions run in parallel, or always sequential?

4. **Result format normalization** - How to handle driver-specific result formatting (e.g., boolean as `t`/`f` vs `true`/`false`)?

5. **Large result sets** - How to handle queries that return many rows? Truncation? Streaming comparison?
