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
#[serde(untagged)]
pub enum WorkDir {
    /// Use a temporary directory (deleted after tests).
    #[default]
    Temp,
    /// Use a specific path.
    Path(PathBuf),
}

/// A setup step executed before tests.
///
/// Each step is a single-key map where the key determines the action.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
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

    /// Run an arbitrary command.
    #[serde(default)]
    pub run: Option<RunStep>,
}

/// A teardown step executed after tests.
///
/// Each step is a single-key map where the key determines the action.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
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

/// A command to run (used in setup/teardown).
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct RunStep {
    /// The command/binary to execute.
    pub cmd: String,

    /// Command arguments.
    #[serde(default)]
    pub args: Vec<String>,
}

/// A single test case.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct Test {
    /// Unique name for this test.
    pub name: String,

    /// Optional description.
    #[serde(default)]
    pub description: Option<String>,

    /// Test-level setup steps.
    #[serde(default)]
    pub setup: Vec<SetupStep>,

    /// The command to execute.
    pub run: Run,

    /// Expected outcomes.
    pub expect: Expect,

    /// Test-level teardown steps.
    #[serde(default)]
    pub teardown: Vec<TeardownStep>,

    /// Timeout in seconds (overrides file/suite default).
    #[serde(default)]
    pub timeout: Option<u64>,

    /// Whether this test must run serially (not in parallel).
    #[serde(default)]
    pub serial: bool,
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
    /// Expected exit code (default: 0).
    #[serde(default)]
    pub exit: Option<i32>,

    /// Expected stdout content.
    #[serde(default)]
    pub stdout: Option<OutputMatch>,

    /// Expected stderr content.
    #[serde(default)]
    pub stderr: Option<OutputMatch>,

    /// Expected filesystem state.
    #[serde(default)]
    pub files: Vec<FileExpect>,
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
        match &spec.tests[0].expect.stdout {
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
        match &spec.tests[0].expect.stdout {
            Some(OutputMatch::Structured(s)) => {
                assert_eq!(s.contains, Some("world".to_string()));
            }
            _ => panic!("Expected structured match"),
        }
    }
}
