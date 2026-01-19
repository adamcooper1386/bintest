//! Schema definitions for bintest spec files.
//!
//! This module defines the structure of test specification files.
//! Specs are written in YAML and validated against these types.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;

/// Suite-level configuration loaded from `bintest.yaml` in the test root.
///
/// Provides defaults that apply to all spec files in the suite.
#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema)]
pub struct SuiteConfig {
    /// Schema version (must match crate major version).
    #[serde(default = "default_version")]
    pub version: u32,

    /// Default timeout in seconds for all tests (can be overridden at file/test level).
    #[serde(default)]
    pub timeout: Option<u64>,

    /// Default environment variables for all tests.
    #[serde(default)]
    pub env: HashMap<String, String>,

    /// Whether to inherit environment from host by default.
    #[serde(default)]
    pub inherit_env: Option<bool>,

    /// Run spec files serially instead of in parallel (default: false).
    /// When false (default), spec files run in parallel for faster execution.
    #[serde(default)]
    pub serial: bool,

    /// Capture filesystem diffs for all tests (default: false).
    /// Shows what files were added, removed, or modified during test execution.
    #[serde(default)]
    pub capture_fs_diff: bool,

    /// Directory for test sandboxes. If set, sandboxes are created here instead of system temp.
    /// Use "local" for `.bintest/<timestamp>/`, or specify a custom path.
    /// When not set, uses system temp directory (auto-deleted after tests).
    #[serde(default)]
    pub sandbox_dir: Option<SandboxDir>,

    /// Database connections available for SQL assertions and setup/teardown.
    /// Keys are connection names (e.g., "default", "root").
    #[serde(default)]
    pub databases: HashMap<String, DatabaseConfig>,

    /// Setup steps run before the entire suite.
    #[serde(default)]
    pub setup: Vec<SetupStep>,

    /// Teardown steps run after the entire suite.
    #[serde(default)]
    pub teardown: Vec<TeardownStep>,
}

fn default_version() -> u32 {
    1
}

/// Directory configuration for test sandboxes.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(from = "String", into = "String")]
pub enum SandboxDir {
    /// Use `.bintest/<timestamp>/` in the test root directory.
    Local,
    /// Use a specific path for sandboxes.
    Path(PathBuf),
}

impl From<String> for SandboxDir {
    fn from(s: String) -> Self {
        if s == "local" {
            SandboxDir::Local
        } else {
            SandboxDir::Path(PathBuf::from(s))
        }
    }
}

impl From<SandboxDir> for String {
    fn from(dir: SandboxDir) -> String {
        match dir {
            SandboxDir::Local => "local".to_string(),
            SandboxDir::Path(p) => p.display().to_string(),
        }
    }
}

/// Root document for a test specification file.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct TestSpec {
    /// Schema version (must match crate major version).
    pub version: u32,

    /// Sandbox configuration for this spec file.
    #[serde(default)]
    pub sandbox: Sandbox,

    /// Default timeout in seconds for tests in this file.
    #[serde(default)]
    pub timeout: Option<u64>,

    /// Capture filesystem diffs for tests in this file (overrides suite setting).
    #[serde(default)]
    pub capture_fs_diff: Option<bool>,

    /// Database connections for this spec file.
    /// Merges with suite-level databases (file-level overrides suite-level).
    #[serde(default)]
    pub databases: HashMap<String, DatabaseConfig>,

    /// Setup steps run before all tests in this file.
    #[serde(default)]
    pub setup: Vec<SetupStep>,

    /// The tests defined in this file.
    pub tests: Vec<Test>,

    /// Teardown steps run after all tests in this file.
    #[serde(default)]
    pub teardown: Vec<TeardownStep>,
}

/// Sandbox configuration controlling the test execution environment.
#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema)]
pub struct Sandbox {
    /// Working directory mode: "temp" creates a temp dir, or a path for explicit location.
    #[serde(default)]
    pub workdir: WorkDir,

    /// Environment variables to set.
    #[serde(default)]
    pub env: HashMap<String, String>,

    /// Whether to inherit environment from host (default: false).
    #[serde(default)]
    pub inherit_env: bool,
}

/// Working directory configuration.
#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema)]
#[serde(from = "Option<String>", into = "Option<String>")]
pub enum WorkDir {
    /// Use a temporary directory (deleted after tests).
    #[default]
    Temp,
    /// Use a specific path.
    Path(PathBuf),
}

impl From<Option<String>> for WorkDir {
    fn from(s: Option<String>) -> Self {
        match s {
            None => WorkDir::Temp,
            Some(s) if s == "temp" => WorkDir::Temp,
            Some(path) => WorkDir::Path(PathBuf::from(path)),
        }
    }
}

impl From<WorkDir> for Option<String> {
    fn from(w: WorkDir) -> Option<String> {
        match w {
            WorkDir::Temp => Some("temp".to_string()),
            WorkDir::Path(p) => Some(p.display().to_string()),
        }
    }
}

/// A setup step executed before tests.
///
/// Each step is a single-key map where the key determines the action.
#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub struct SetupStep {
    /// Write a file with the given contents.
    #[serde(default)]
    pub write_file: Option<WriteFile>,

    /// Create a directory.
    #[serde(default)]
    pub create_dir: Option<PathBuf>,

    /// Copy a file from source to destination.
    #[serde(default)]
    pub copy_file: Option<CopyFile>,

    /// Copy a directory recursively from source to destination.
    #[serde(default)]
    pub copy_dir: Option<CopyDir>,

    /// Run an arbitrary command.
    #[serde(default)]
    pub run: Option<RunStep>,

    /// Execute SQL statements.
    #[serde(default)]
    pub sql: Option<SqlStatements>,

    /// Execute SQL from a file.
    #[serde(default)]
    pub sql_file: Option<SqlFile>,

    /// Save database state to a named snapshot.
    #[serde(default)]
    pub db_snapshot: Option<DbSnapshot>,

    /// Restore database state from a named snapshot.
    #[serde(default)]
    pub db_restore: Option<DbRestore>,
}

/// A teardown step executed after tests.
///
/// Each step is a single-key map where the key determines the action.
#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub struct TeardownStep {
    /// Remove a directory.
    #[serde(default)]
    pub remove_dir: Option<PathBuf>,

    /// Remove a file.
    #[serde(default)]
    pub remove_file: Option<PathBuf>,

    /// Run an arbitrary command.
    #[serde(default)]
    pub run: Option<RunStep>,

    /// Execute SQL statements.
    #[serde(default)]
    pub sql: Option<SqlStatements>,

    /// Restore database state from a named snapshot.
    #[serde(default)]
    pub db_restore: Option<DbRestore>,
}

/// Write a file with specific contents.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct WriteFile {
    /// Path to the file (relative to sandbox).
    pub path: PathBuf,

    /// File contents.
    pub contents: String,
}

/// Copy a file from one location to another.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct CopyFile {
    /// Source path.
    pub from: PathBuf,

    /// Destination path.
    pub to: PathBuf,
}

/// Copy a directory recursively from one location to another.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct CopyDir {
    /// Source directory path.
    pub from: PathBuf,

    /// Destination directory path.
    pub to: PathBuf,
}

/// SQL statements to execute.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct SqlStatements {
    /// Database connection name (defaults to "default").
    #[serde(default = "default_database_name")]
    pub database: String,

    /// SQL statements to execute in order.
    pub statements: Vec<String>,

    /// Error handling: "fail" (default) or "continue".
    #[serde(default)]
    pub on_error: SqlOnError,
}

/// SQL file to execute.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct SqlFile {
    /// Database connection name (defaults to "default").
    #[serde(default = "default_database_name")]
    pub database: String,

    /// Path to the SQL file (relative to sandbox).
    pub path: PathBuf,

    /// Error handling: "fail" (default) or "continue".
    #[serde(default)]
    pub on_error: SqlOnError,
}

/// Error handling strategy for SQL execution.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum SqlOnError {
    /// Stop execution on first error (default).
    #[default]
    Fail,
    /// Continue executing remaining statements on error.
    Continue,
}

/// Save database state to a named snapshot.
///
/// Snapshots are stored in memory and can be restored later within the same file.
/// Currently only supported for SQLite databases.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct DbSnapshot {
    /// Database connection name (defaults to "default").
    #[serde(default = "default_database_name")]
    pub database: String,

    /// Name for this snapshot (used to restore later).
    pub name: String,
}

/// Restore database state from a named snapshot.
///
/// The snapshot must have been created earlier in the same file using `db_snapshot`.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct DbRestore {
    /// Database connection name (defaults to "default").
    #[serde(default = "default_database_name")]
    pub database: String,

    /// Name of the snapshot to restore.
    pub name: String,
}

/// A command to run (used in setup/teardown).
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct RunStep {
    /// The command/binary to execute.
    pub cmd: String,

    /// Command arguments.
    #[serde(default)]
    pub args: Vec<String>,
}

/// A single step within a multi-step test.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct Step {
    /// Step name (used in failure reporting).
    pub name: String,

    /// Step-level setup steps.
    #[serde(default)]
    pub setup: Vec<SetupStep>,

    /// The command to execute.
    pub run: Run,

    /// Expected outcomes.
    #[serde(default)]
    pub expect: Expect,

    /// Step-level teardown steps.
    #[serde(default)]
    pub teardown: Vec<TeardownStep>,
}

// ============================================================================
// Conditional Execution Types
// ============================================================================

/// A condition for conditional test execution.
///
/// Conditions can check environment variables or command availability.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub struct Condition {
    /// Environment variable that must be set (for require) or must be set (for skip_if).
    #[serde(default)]
    pub env: Option<String>,

    /// Command that must succeed (exit 0) for require, or must succeed for skip_if.
    #[serde(default)]
    pub cmd: Option<String>,
}

/// Helper enum for deserializing both test formats.
/// Only used during deserialization, not stored, so the size difference is acceptable.
#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
#[allow(clippy::large_enum_variant)]
enum TestFormat {
    /// New format with explicit steps.
    MultiStep {
        name: String,
        #[serde(default)]
        description: Option<String>,
        #[serde(default)]
        skip_if: Vec<Condition>,
        #[serde(default)]
        require: Vec<Condition>,
        #[serde(default)]
        setup: Vec<SetupStep>,
        steps: Vec<Step>,
        #[serde(default)]
        teardown: Vec<TeardownStep>,
        #[serde(default)]
        timeout: Option<u64>,
        #[serde(default)]
        serial: bool,
        #[serde(default)]
        capture_fs_diff: Option<bool>,
    },
    /// Old format with single run/expect (implicit single step).
    SingleStep {
        name: String,
        #[serde(default)]
        description: Option<String>,
        #[serde(default)]
        skip_if: Vec<Condition>,
        #[serde(default)]
        require: Vec<Condition>,
        #[serde(default)]
        setup: Vec<SetupStep>,
        run: Run,
        #[serde(default)]
        expect: Expect,
        #[serde(default)]
        teardown: Vec<TeardownStep>,
        #[serde(default)]
        timeout: Option<u64>,
        #[serde(default)]
        serial: bool,
        #[serde(default)]
        capture_fs_diff: Option<bool>,
    },
}

/// A single test case.
///
/// Tests can be defined in two formats:
/// 1. Single-step (backward compatible): `run` + `expect` fields
/// 2. Multi-step: `steps` array with named steps
///
/// Internally, single-step tests are converted to a single step named "run".
#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct Test {
    /// Unique name for this test.
    pub name: String,

    /// Optional description.
    #[serde(default)]
    pub description: Option<String>,

    /// Conditions that cause the test to be skipped if ANY are true.
    /// Use for skipping tests in certain environments (e.g., skip_if env: CI).
    #[serde(default)]
    pub skip_if: Vec<Condition>,

    /// Conditions that must ALL be met for the test to run.
    /// Use for requiring dependencies (e.g., require env: DATABASE_URL).
    #[serde(default)]
    pub require: Vec<Condition>,

    /// Test-level setup steps (run once before all steps).
    #[serde(default)]
    pub setup: Vec<SetupStep>,

    /// The steps to execute. For single-step tests, this contains one step named "run".
    pub steps: Vec<Step>,

    /// Test-level teardown steps (run once after all steps).
    #[serde(default)]
    pub teardown: Vec<TeardownStep>,

    /// Timeout in seconds (overrides file/suite default).
    #[serde(default)]
    pub timeout: Option<u64>,

    /// Whether this test must run serially (not in parallel).
    #[serde(default)]
    pub serial: bool,

    /// Capture filesystem diff for this test (overrides file/suite setting).
    #[serde(default)]
    pub capture_fs_diff: Option<bool>,
}

impl<'de> Deserialize<'de> for Test {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let format = TestFormat::deserialize(deserializer)?;
        Ok(match format {
            TestFormat::MultiStep {
                name,
                description,
                skip_if,
                require,
                setup,
                steps,
                teardown,
                timeout,
                serial,
                capture_fs_diff,
            } => Test {
                name,
                description,
                skip_if,
                require,
                setup,
                steps,
                teardown,
                timeout,
                serial,
                capture_fs_diff,
            },
            TestFormat::SingleStep {
                name,
                description,
                skip_if,
                require,
                setup,
                run,
                expect,
                teardown,
                timeout,
                serial,
                capture_fs_diff,
            } => {
                // Convert single run/expect to a single step named "run"
                Test {
                    name,
                    description,
                    skip_if,
                    require,
                    setup,
                    steps: vec![Step {
                        name: "run".to_string(),
                        setup: vec![],
                        run,
                        expect,
                        teardown: vec![],
                    }],
                    teardown,
                    timeout,
                    serial,
                    capture_fs_diff,
                }
            }
        })
    }
}

/// Command execution configuration.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct Run {
    /// The command/binary to execute.
    pub cmd: String,

    /// Command arguments.
    #[serde(default)]
    pub args: Vec<String>,

    /// Standard input to provide.
    #[serde(default)]
    pub stdin: Option<String>,

    /// Additional environment variables for this command.
    #[serde(default)]
    pub env: HashMap<String, String>,

    /// Working directory (relative to sandbox, defaults to sandbox root).
    #[serde(default)]
    pub cwd: Option<PathBuf>,

    /// Run through shell (default: false).
    #[serde(default)]
    pub shell: bool,
}

/// Expected outcomes from a test execution.
#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema)]
pub struct Expect {
    /// Expected exit code (default: 0 if no signal expected).
    #[serde(default)]
    pub exit: Option<i32>,

    /// Expected signal that terminated the process (e.g., 9 for SIGKILL, 15 for SIGTERM).
    /// If set, exit code is ignored and the process must have been killed by this signal.
    #[serde(default)]
    pub signal: Option<i32>,

    /// Expected stdout content.
    #[serde(default)]
    pub stdout: Option<OutputMatch>,

    /// Expected stderr content.
    #[serde(default)]
    pub stderr: Option<OutputMatch>,

    /// Expected filesystem state.
    #[serde(default)]
    pub files: Vec<FileExpect>,

    /// Expected directory tree structure.
    #[serde(default)]
    pub tree: Option<TreeExpect>,

    /// Expected database state (SQL assertions).
    #[serde(default)]
    pub sql: Vec<SqlExpect>,
}

/// Matching rules for stdout/stderr.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(untagged)]
pub enum OutputMatch {
    /// Exact string match.
    Exact(String),

    /// Structured match with multiple options.
    Structured(OutputMatchStructured),
}

/// Structured output matching with multiple match types.
#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema)]
pub struct OutputMatchStructured {
    /// Exact string match.
    #[serde(default)]
    pub equals: Option<String>,

    /// Substring match.
    #[serde(default)]
    pub contains: Option<String>,

    /// Regular expression match.
    #[serde(default)]
    pub regex: Option<String>,
}

/// Expected state of a file after test execution.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct FileExpect {
    /// Path to the file (relative to sandbox).
    pub path: PathBuf,

    /// Whether the file should exist.
    #[serde(default)]
    pub exists: Option<bool>,

    /// Expected file contents.
    #[serde(default)]
    pub contents: Option<OutputMatch>,
}

/// Expected directory tree structure after test execution.
#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema)]
pub struct TreeExpect {
    /// Root directory to check (relative to sandbox, defaults to sandbox root).
    #[serde(default)]
    pub root: Option<PathBuf>,

    /// Paths that must exist in the tree.
    #[serde(default)]
    pub contains: Vec<TreeEntry>,

    /// Paths that must not exist in the tree.
    #[serde(default)]
    pub excludes: Vec<PathBuf>,

    /// If true, only paths in `contains` should exist (no extra files).
    #[serde(default)]
    pub exact: bool,
}

/// An entry in a tree expectation.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct TreeEntry {
    /// Path to the file or directory (relative to tree root).
    pub path: PathBuf,

    /// Expected file contents (only for files, not directories).
    #[serde(default)]
    pub contents: Option<OutputMatch>,
}

// ============================================================================
// SQL Assertion Types
// ============================================================================

/// A SQL assertion to verify database state.
///
/// Supports multiple assertion types through mutually exclusive fields.
/// Only one type of assertion should be specified per `SqlExpect`.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct SqlExpect {
    /// Database connection name (defaults to "default").
    #[serde(default = "default_database_name")]
    pub database: String,

    /// Raw SQL query to execute.
    #[serde(default)]
    pub query: Option<String>,

    /// Expected result from the query.
    #[serde(default)]
    pub returns: Option<SqlReturns>,

    /// Assert that the query returns no rows.
    #[serde(default)]
    pub returns_empty: Option<bool>,

    /// Assert that the query returns a NULL value.
    #[serde(default)]
    pub returns_null: Option<bool>,

    /// Assert that the query returns exactly one row.
    #[serde(default)]
    pub returns_one_row: Option<bool>,

    /// Shorthand: check if a table exists.
    #[serde(default)]
    pub table_exists: Option<String>,

    /// Shorthand: check if a table does not exist.
    #[serde(default)]
    pub table_not_exists: Option<String>,

    /// Shorthand: assert row count for a table.
    #[serde(default)]
    pub row_count: Option<RowCountExpect>,
}

fn default_database_name() -> String {
    "default".to_string()
}

/// Expected return value from a SQL query.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(untagged)]
pub enum SqlReturns {
    /// Exact string match (single value or newline-separated for multiple rows).
    Exact(String),

    /// Structured match with multiple options.
    Structured(SqlReturnsStructured),
}

/// Structured SQL result matching.
#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema)]
pub struct SqlReturnsStructured {
    /// Exact string match.
    #[serde(default)]
    pub equals: Option<String>,

    /// Substring match.
    #[serde(default)]
    pub contains: Option<String>,

    /// Regular expression match.
    #[serde(default)]
    pub regex: Option<String>,
}

/// Row count assertion for a table.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct RowCountExpect {
    /// Table name to count rows from.
    pub table: String,

    /// Exact row count expected.
    #[serde(default)]
    pub equals: Option<u64>,

    /// Minimum row count (exclusive).
    #[serde(default)]
    pub greater_than: Option<u64>,

    /// Maximum row count (exclusive).
    #[serde(default)]
    pub less_than: Option<u64>,
}

// ============================================================================
// Database Configuration Types
// ============================================================================

/// Database driver type.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum DbDriver {
    /// PostgreSQL database.
    Postgres,
    /// SQLite database (including `:memory:`).
    Sqlite,
}

/// Database isolation mode for per-file state management.
///
/// Controls how database state is handled between test files.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum DbIsolation {
    /// No automatic isolation. Database state persists across tests (default).
    /// Use explicit setup/teardown for state management.
    #[default]
    None,
    /// Fresh database for each test file.
    ///
    /// For SQLite: Creates a fresh in-memory database for each file.
    /// For PostgreSQL: Creates an automatic snapshot at file start and
    /// restores it between tests (requires setup to create initial state).
    PerFile,
}

/// Database connection configuration.
///
/// Defines how to connect to a database. URLs support environment variable
/// interpolation using `${VAR}` syntax.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct DatabaseConfig {
    /// The database driver to use.
    pub driver: DbDriver,

    /// Connection URL. Supports `${VAR}` interpolation.
    ///
    /// Examples:
    /// - `postgres://user:pass@localhost:5432/dbname`
    /// - `sqlite:///path/to/file.db`
    /// - `sqlite::memory:`
    /// - `${DATABASE_URL}`
    pub url: String,

    /// Isolation mode for this database connection.
    ///
    /// Controls how database state is managed between test files.
    /// Default is `none` (no automatic isolation).
    #[serde(default)]
    pub isolation: DbIsolation,
}

/// Generate the JSON Schema for test specification files.
pub fn generate_schema() -> schemars::schema::RootSchema {
    schemars::schema_for!(TestSpec)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_minimal_spec() {
        let yaml = r#"
version: 1
tests:
  - name: simple_test
    run:
      cmd: echo
      args: ["hello"]
    expect:
      exit: 0
"#;
        let spec: TestSpec = serde_yaml::from_str(yaml).unwrap();
        assert_eq!(spec.version, 1);
        assert_eq!(spec.tests.len(), 1);
        assert_eq!(spec.tests[0].name, "simple_test");
        // Single-step format is converted to a step named "run"
        assert_eq!(spec.tests[0].steps.len(), 1);
        assert_eq!(spec.tests[0].steps[0].name, "run");
    }

    #[test]
    fn parse_full_spec() {
        let yaml = r#"
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
"#;
        let spec: TestSpec = serde_yaml::from_str(yaml).unwrap();
        assert_eq!(spec.version, 1);
        assert_eq!(spec.sandbox.env.get("RUST_LOG"), Some(&"debug".to_string()));
        assert_eq!(spec.setup.len(), 1);
        assert_eq!(spec.tests.len(), 1);
        assert_eq!(spec.teardown.len(), 1);
    }

    #[test]
    fn parse_stdout_exact_match() {
        let yaml = r#"
version: 1
tests:
  - name: exact_output
    run:
      cmd: echo
      args: ["hello"]
    expect:
      stdout: "hello\n"
"#;
        let spec: TestSpec = serde_yaml::from_str(yaml).unwrap();
        // Access expect through the step
        match &spec.tests[0].steps[0].expect.stdout {
            Some(OutputMatch::Exact(s)) => assert_eq!(s, "hello\n"),
            _ => panic!("Expected exact match"),
        }
    }

    #[test]
    fn parse_stdout_structured_match() {
        let yaml = r#"
version: 1
tests:
  - name: contains_output
    run:
      cmd: echo
      args: ["hello world"]
    expect:
      stdout:
        contains: "world"
"#;
        let spec: TestSpec = serde_yaml::from_str(yaml).unwrap();
        // Access expect through the step
        match &spec.tests[0].steps[0].expect.stdout {
            Some(OutputMatch::Structured(s)) => {
                assert_eq!(s.contains, Some("world".to_string()));
            }
            _ => panic!("Expected structured match"),
        }
    }

    #[test]
    fn parse_multi_step_test() {
        let yaml = r#"
version: 1
tests:
  - name: workflow_test
    setup:
      - write_file:
          path: initial.txt
          contents: "start"
    steps:
      - name: initialize
        run:
          cmd: my_cli
          args: ["init"]
        expect:
          exit: 0
      - name: execute
        setup:
          - write_file:
              path: config.json
              contents: "{}"
        run:
          cmd: my_cli
          args: ["run"]
        expect:
          exit: 0
          stdout:
            contains: "success"
    teardown:
      - remove_file: initial.txt
"#;
        let spec: TestSpec = serde_yaml::from_str(yaml).unwrap();
        assert_eq!(spec.tests.len(), 1);
        let test = &spec.tests[0];
        assert_eq!(test.name, "workflow_test");
        assert_eq!(test.setup.len(), 1);
        assert_eq!(test.teardown.len(), 1);
        assert_eq!(test.steps.len(), 2);

        // Check first step
        assert_eq!(test.steps[0].name, "initialize");
        assert_eq!(test.steps[0].run.cmd, "my_cli");
        assert_eq!(test.steps[0].run.args, vec!["init"]);
        assert!(test.steps[0].setup.is_empty());

        // Check second step
        assert_eq!(test.steps[1].name, "execute");
        assert_eq!(test.steps[1].run.cmd, "my_cli");
        assert_eq!(test.steps[1].run.args, vec!["run"]);
        assert_eq!(test.steps[1].setup.len(), 1);
    }

    #[test]
    fn parse_mixed_single_and_multi_step() {
        let yaml = r#"
version: 1
tests:
  - name: single_step_test
    run:
      cmd: echo
      args: ["hello"]
    expect:
      exit: 0
  - name: multi_step_test
    steps:
      - name: step_one
        run:
          cmd: echo
          args: ["one"]
        expect:
          exit: 0
      - name: step_two
        run:
          cmd: echo
          args: ["two"]
        expect:
          exit: 0
"#;
        let spec: TestSpec = serde_yaml::from_str(yaml).unwrap();
        assert_eq!(spec.tests.len(), 2);

        // First test: single-step format converted to steps
        assert_eq!(spec.tests[0].name, "single_step_test");
        assert_eq!(spec.tests[0].steps.len(), 1);
        assert_eq!(spec.tests[0].steps[0].name, "run");

        // Second test: explicit multi-step format
        assert_eq!(spec.tests[1].name, "multi_step_test");
        assert_eq!(spec.tests[1].steps.len(), 2);
        assert_eq!(spec.tests[1].steps[0].name, "step_one");
        assert_eq!(spec.tests[1].steps[1].name, "step_two");
    }

    #[test]
    fn parse_database_config() {
        let yaml = r#"
version: 1

databases:
  default:
    driver: postgres
    url: "${DATABASE_URL}"
  root:
    driver: postgres
    url: "postgres://admin:secret@localhost:5432/postgres"
  sqlite_test:
    driver: sqlite
    url: "sqlite::memory:"

tests:
  - name: db_test
    run:
      cmd: echo
      args: ["hello"]
    expect:
      exit: 0
"#;
        let spec: TestSpec = serde_yaml::from_str(yaml).unwrap();
        assert_eq!(spec.databases.len(), 3);

        let default_db = spec.databases.get("default").unwrap();
        assert_eq!(default_db.driver, DbDriver::Postgres);
        assert_eq!(default_db.url, "${DATABASE_URL}");
        assert_eq!(default_db.isolation, DbIsolation::None); // default

        let root_db = spec.databases.get("root").unwrap();
        assert_eq!(root_db.driver, DbDriver::Postgres);
        assert_eq!(
            root_db.url,
            "postgres://admin:secret@localhost:5432/postgres"
        );

        let sqlite_db = spec.databases.get("sqlite_test").unwrap();
        assert_eq!(sqlite_db.driver, DbDriver::Sqlite);
        assert_eq!(sqlite_db.url, "sqlite::memory:");
    }

    #[test]
    fn parse_database_isolation() {
        let yaml = r#"
version: 1

databases:
  isolated:
    driver: sqlite
    url: "sqlite::memory:"
    isolation: per_file
  shared:
    driver: sqlite
    url: "sqlite::memory:"
    isolation: none

tests:
  - name: test
    run:
      cmd: echo
      args: ["hello"]
    expect:
      exit: 0
"#;
        let spec: TestSpec = serde_yaml::from_str(yaml).unwrap();
        assert_eq!(spec.databases.len(), 2);

        let isolated_db = spec.databases.get("isolated").unwrap();
        assert_eq!(isolated_db.isolation, DbIsolation::PerFile);

        let shared_db = spec.databases.get("shared").unwrap();
        assert_eq!(shared_db.isolation, DbIsolation::None);
    }

    #[test]
    fn parse_conditional_execution() {
        let yaml = r#"
version: 1
tests:
  - name: requires_database
    require:
      - env: DATABASE_URL
      - cmd: psql --version
    run:
      cmd: echo
      args: ["database test"]
    expect:
      exit: 0

  - name: skip_in_ci
    skip_if:
      - env: CI
    run:
      cmd: echo
      args: ["local only test"]
    expect:
      exit: 0

  - name: multi_step_with_conditions
    require:
      - env: API_KEY
    skip_if:
      - env: SKIP_SLOW_TESTS
    steps:
      - name: step_one
        run:
          cmd: echo
          args: ["one"]
        expect:
          exit: 0
"#;
        let spec: TestSpec = serde_yaml::from_str(yaml).unwrap();
        assert_eq!(spec.tests.len(), 3);

        // First test: require conditions
        let test1 = &spec.tests[0];
        assert_eq!(test1.name, "requires_database");
        assert_eq!(test1.require.len(), 2);
        assert_eq!(test1.require[0].env, Some("DATABASE_URL".to_string()));
        assert_eq!(test1.require[1].cmd, Some("psql --version".to_string()));
        assert!(test1.skip_if.is_empty());

        // Second test: skip_if conditions
        let test2 = &spec.tests[1];
        assert_eq!(test2.name, "skip_in_ci");
        assert_eq!(test2.skip_if.len(), 1);
        assert_eq!(test2.skip_if[0].env, Some("CI".to_string()));
        assert!(test2.require.is_empty());

        // Third test: both conditions on multi-step
        let test3 = &spec.tests[2];
        assert_eq!(test3.name, "multi_step_with_conditions");
        assert_eq!(test3.require.len(), 1);
        assert_eq!(test3.require[0].env, Some("API_KEY".to_string()));
        assert_eq!(test3.skip_if.len(), 1);
        assert_eq!(test3.skip_if[0].env, Some("SKIP_SLOW_TESTS".to_string()));
    }
}
