//! Test execution engine.
//!
//! Runs test specs in isolated sandboxes and captures results.

use crate::schema::{
    Expect, FileExpect, OutputMatch, OutputMatchStructured, Run, RunStep, Sandbox, SetupStep,
    SuiteConfig, TeardownStep, Test, TestSpec, WorkDir,
};
use std::collections::HashMap;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

/// Default timeout per test in seconds.
const DEFAULT_TIMEOUT_SECS: u64 = 3;

/// Result of running a test spec file.
#[derive(Debug, serde::Serialize)]
pub struct SpecResult {
    pub tests: Vec<TestResult>,
}

/// Result of running a single test.
#[derive(Debug, serde::Serialize)]
pub struct TestResult {
    pub name: String,
    pub passed: bool,
    #[serde(serialize_with = "serialize_duration")]
    pub duration: Duration,
    pub failures: Vec<String>,
}

fn serialize_duration<S>(duration: &Duration, serializer: S) -> Result<S::Ok, S::Error>
where
    S: serde::Serializer,
{
    serializer.serialize_f64(duration.as_secs_f64())
}

/// Run suite-level setup steps.
///
/// Creates a temporary context for suite setup (uses temp directory).
pub fn run_suite_setup(config: &SuiteConfig) -> Result<(), String> {
    if config.setup.is_empty() {
        return Ok(());
    }

    let ctx = ExecutionContext::new(&Sandbox::default())
        .map_err(|e| format!("Failed to create suite context: {e}"))?;
    run_setup_steps(&config.setup, &ctx)
}

/// Run suite-level teardown steps.
///
/// Creates a temporary context for suite teardown (uses temp directory).
pub fn run_suite_teardown(config: &SuiteConfig) -> Result<(), String> {
    if config.teardown.is_empty() {
        return Ok(());
    }

    let ctx = ExecutionContext::new(&Sandbox::default())
        .map_err(|e| format!("Failed to create suite context: {e}"))?;
    run_teardown_steps(&config.teardown, &ctx)
}

/// Context for test execution within a sandbox.
struct ExecutionContext {
    sandbox_dir: PathBuf,
    env: HashMap<String, String>,
    inherit_env: bool,
    _temp_dir: Option<tempfile::TempDir>,
}

impl ExecutionContext {
    fn new(sandbox: &Sandbox) -> std::io::Result<Self> {
        let (sandbox_dir, temp_dir) = match &sandbox.workdir {
            WorkDir::Temp => {
                let temp = tempfile::tempdir()?;
                let path = temp.path().to_path_buf();
                (path, Some(temp))
            }
            WorkDir::Path(p) => {
                std::fs::create_dir_all(p)?;
                (p.clone(), None)
            }
        };

        Ok(Self {
            sandbox_dir,
            env: sandbox.env.clone(),
            inherit_env: sandbox.inherit_env,
            _temp_dir: temp_dir,
        })
    }

    fn resolve_path(&self, path: &Path) -> PathBuf {
        if path.is_absolute() {
            path.to_path_buf()
        } else {
            self.sandbox_dir.join(path)
        }
    }
}

/// Effective configuration for running a spec, combining suite and file settings.
#[derive(Debug, Clone, Default)]
pub struct EffectiveConfig {
    /// Default timeout (from suite or file).
    pub default_timeout: Option<u64>,
    /// Additional environment variables from suite config.
    pub suite_env: HashMap<String, String>,
    /// Whether to inherit env from host (suite-level default).
    pub inherit_env: Option<bool>,
}

impl EffectiveConfig {
    /// Create from optional suite config.
    pub fn from_suite(suite: Option<&SuiteConfig>) -> Self {
        match suite {
            Some(cfg) => Self {
                default_timeout: cfg.timeout,
                suite_env: cfg.env.clone(),
                inherit_env: cfg.inherit_env,
            },
            None => Self::default(),
        }
    }
}

/// Run a test specification file with optional suite configuration.
pub fn run_spec(spec: &TestSpec, suite_config: Option<&SuiteConfig>) -> SpecResult {
    let effective = EffectiveConfig::from_suite(suite_config);
    run_spec_with_config(spec, &effective)
}

/// Run a test specification file with effective configuration.
fn run_spec_with_config(spec: &TestSpec, effective: &EffectiveConfig) -> SpecResult {
    // Merge suite env with sandbox env (sandbox takes precedence)
    let mut merged_sandbox = spec.sandbox.clone();
    for (k, v) in &effective.suite_env {
        merged_sandbox.env.entry(k.clone()).or_insert(v.clone());
    }
    // Apply suite-level inherit_env if file doesn't specify differently
    if let Some(inherit) = effective.inherit_env {
        // Only override if sandbox has default (false)
        if !merged_sandbox.inherit_env {
            merged_sandbox.inherit_env = inherit;
        }
    }

    // Determine file-level default timeout
    let file_timeout = spec.timeout.or(effective.default_timeout);

    let ctx = match ExecutionContext::new(&merged_sandbox) {
        Ok(ctx) => ctx,
        Err(e) => {
            return SpecResult {
                tests: vec![TestResult {
                    name: "<setup>".to_string(),
                    passed: false,
                    duration: Duration::ZERO,
                    failures: vec![format!("Failed to create sandbox: {e}")],
                }],
            };
        }
    };

    // Run file-level setup
    if let Err(e) = run_setup_steps(&spec.setup, &ctx) {
        return SpecResult {
            tests: vec![TestResult {
                name: "<setup>".to_string(),
                passed: false,
                duration: Duration::ZERO,
                failures: vec![format!("Setup failed: {e}")],
            }],
        };
    }

    // Run tests
    let mut results = Vec::new();
    for test in &spec.tests {
        let result = run_test(test, &ctx, file_timeout);
        results.push(result);
    }

    // Run file-level teardown (always runs)
    if let Err(e) = run_teardown_steps(&spec.teardown, &ctx) {
        results.push(TestResult {
            name: "<teardown>".to_string(),
            passed: false,
            duration: Duration::ZERO,
            failures: vec![format!("Teardown failed: {e}")],
        });
    }

    SpecResult { tests: results }
}

fn run_test(test: &Test, ctx: &ExecutionContext, file_timeout: Option<u64>) -> TestResult {
    let start = Instant::now();
    let mut failures = Vec::new();

    // Test-level setup
    if let Err(e) = run_setup_steps(&test.setup, ctx) {
        return TestResult {
            name: test.name.clone(),
            passed: false,
            duration: start.elapsed(),
            failures: vec![format!("Test setup failed: {e}")],
        };
    }

    // Run the command - test timeout overrides file timeout overrides default
    let timeout_secs = test
        .timeout
        .or(file_timeout)
        .unwrap_or(DEFAULT_TIMEOUT_SECS);
    let timeout = Duration::from_secs(timeout_secs);
    match run_command(&test.run, ctx, timeout) {
        Ok(output) => {
            // Check assertions
            check_expectations(&test.expect, &output, ctx, &mut failures);
        }
        Err(e) => {
            failures.push(format!("Command execution failed: {e}"));
        }
    }

    // Test-level teardown (always runs)
    if let Err(e) = run_teardown_steps(&test.teardown, ctx) {
        failures.push(format!("Test teardown failed: {e}"));
    }

    TestResult {
        name: test.name.clone(),
        passed: failures.is_empty(),
        duration: start.elapsed(),
        failures,
    }
}

struct CommandOutput {
    exit_code: i32,
    stdout: String,
    stderr: String,
}

fn run_command(
    run: &Run,
    ctx: &ExecutionContext,
    timeout: Duration,
) -> Result<CommandOutput, String> {
    let mut cmd = if run.shell {
        let mut c = Command::new("sh");
        c.arg("-c");
        c.arg(format!("{} {}", run.cmd, run.args.join(" ")));
        c
    } else {
        let mut c = Command::new(&run.cmd);
        c.args(&run.args);
        c
    };

    // Set working directory
    let cwd = run
        .cwd
        .as_ref()
        .map(|p| ctx.resolve_path(p))
        .unwrap_or_else(|| ctx.sandbox_dir.clone());
    cmd.current_dir(&cwd);

    // Set environment
    if !ctx.inherit_env {
        cmd.env_clear();
    }
    for (k, v) in &ctx.env {
        cmd.env(k, v);
    }
    for (k, v) in &run.env {
        cmd.env(k, v);
    }

    // Setup stdin
    if run.stdin.is_some() {
        cmd.stdin(Stdio::piped());
    } else {
        cmd.stdin(Stdio::null());
    }
    cmd.stdout(Stdio::piped());
    cmd.stderr(Stdio::piped());

    let mut child = cmd.spawn().map_err(|e| format!("Failed to spawn: {e}"))?;

    // Write stdin if provided
    if let Some(stdin_data) = &run.stdin
        && let Some(mut stdin) = child.stdin.take()
    {
        stdin
            .write_all(stdin_data.as_bytes())
            .map_err(|e| format!("Failed to write stdin: {e}"))?;
    }

    // Wait with timeout
    let start = Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                let output = child
                    .wait_with_output()
                    .map_err(|e| format!("Failed to read output: {e}"))?;
                return Ok(CommandOutput {
                    exit_code: status.code().unwrap_or(-1),
                    stdout: String::from_utf8_lossy(&output.stdout).to_string(),
                    stderr: String::from_utf8_lossy(&output.stderr).to_string(),
                });
            }
            Ok(None) => {
                if start.elapsed() > timeout {
                    let _ = child.kill();
                    return Err(format!("Command timed out after {}s", timeout.as_secs()));
                }
                std::thread::sleep(Duration::from_millis(10));
            }
            Err(e) => return Err(format!("Failed to wait: {e}")),
        }
    }
}

fn check_expectations(
    expect: &Expect,
    output: &CommandOutput,
    ctx: &ExecutionContext,
    failures: &mut Vec<String>,
) {
    // Check exit code
    let expected_exit = expect.exit.unwrap_or(0);
    if output.exit_code != expected_exit {
        failures.push(format!(
            "Exit code: expected {expected_exit}, got {}",
            output.exit_code
        ));
    }

    // Check stdout
    if let Some(matcher) = &expect.stdout
        && let Err(e) = check_output_match("stdout", &output.stdout, matcher)
    {
        failures.push(e);
    }

    // Check stderr
    if let Some(matcher) = &expect.stderr
        && let Err(e) = check_output_match("stderr", &output.stderr, matcher)
    {
        failures.push(e);
    }

    // Check files
    for file_expect in &expect.files {
        check_file_expect(file_expect, ctx, failures);
    }
}

fn check_output_match(name: &str, actual: &str, matcher: &OutputMatch) -> Result<(), String> {
    match matcher {
        OutputMatch::Exact(expected) => {
            if actual != expected {
                Err(format!(
                    "{name}: expected exact match\n  expected: {expected:?}\n  got: {actual:?}"
                ))
            } else {
                Ok(())
            }
        }
        OutputMatch::Structured(s) => check_structured_match(name, actual, s),
    }
}

fn check_structured_match(
    name: &str,
    actual: &str,
    matcher: &OutputMatchStructured,
) -> Result<(), String> {
    if let Some(expected) = &matcher.equals
        && actual != expected
    {
        return Err(format!(
            "{name}: expected exact match\n  expected: {expected:?}\n  got: {actual:?}"
        ));
    }

    if let Some(substring) = &matcher.contains
        && !actual.contains(substring)
    {
        return Err(format!(
            "{name}: expected to contain {substring:?}\n  got: {actual:?}"
        ));
    }

    if let Some(pattern) = &matcher.regex {
        let re = regex::Regex::new(pattern)
            .map_err(|e| format!("{name}: invalid regex {pattern:?}: {e}"))?;
        if !re.is_match(actual) {
            return Err(format!(
                "{name}: expected to match regex {pattern:?}\n  got: {actual:?}"
            ));
        }
    }

    Ok(())
}

fn check_file_expect(file_expect: &FileExpect, ctx: &ExecutionContext, failures: &mut Vec<String>) {
    let path = ctx.resolve_path(&file_expect.path);

    if let Some(should_exist) = file_expect.exists {
        let exists = path.exists();
        if should_exist && !exists {
            failures.push(format!("File should exist: {}", file_expect.path.display()));
            return;
        }
        if !should_exist && exists {
            failures.push(format!(
                "File should not exist: {}",
                file_expect.path.display()
            ));
            return;
        }
    }

    if let Some(matcher) = &file_expect.contents {
        match std::fs::read_to_string(&path) {
            Ok(contents) => {
                let name = format!("file:{}", file_expect.path.display());
                if let Err(e) = check_output_match(&name, &contents, matcher) {
                    failures.push(e);
                }
            }
            Err(e) => {
                failures.push(format!(
                    "Failed to read {}: {e}",
                    file_expect.path.display()
                ));
            }
        }
    }
}

fn run_setup_steps(steps: &[SetupStep], ctx: &ExecutionContext) -> Result<(), String> {
    for step in steps {
        run_setup_step(step, ctx)?;
    }
    Ok(())
}

fn run_setup_step(step: &SetupStep, ctx: &ExecutionContext) -> Result<(), String> {
    if let Some(write_file) = &step.write_file {
        let path = ctx.resolve_path(&write_file.path);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| format!("Failed to create directory: {e}"))?;
        }
        std::fs::write(&path, &write_file.contents)
            .map_err(|e| format!("Failed to write {}: {e}", write_file.path.display()))?;
    }

    if let Some(dir_path) = &step.create_dir {
        let path = ctx.resolve_path(dir_path);
        std::fs::create_dir_all(&path)
            .map_err(|e| format!("Failed to create directory {}: {e}", dir_path.display()))?;
    }

    if let Some(copy) = &step.copy_file {
        let from = ctx.resolve_path(&copy.from);
        let to = ctx.resolve_path(&copy.to);
        if let Some(parent) = to.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| format!("Failed to create directory: {e}"))?;
        }
        std::fs::copy(&from, &to).map_err(|e| {
            format!(
                "Failed to copy {} to {}: {e}",
                copy.from.display(),
                copy.to.display()
            )
        })?;
    }

    if let Some(run) = &step.run {
        run_simple_command(run, ctx)?;
    }

    Ok(())
}

fn run_teardown_steps(steps: &[TeardownStep], ctx: &ExecutionContext) -> Result<(), String> {
    let mut errors = Vec::new();
    for step in steps {
        if let Err(e) = run_teardown_step(step, ctx) {
            errors.push(e);
        }
    }
    if errors.is_empty() {
        Ok(())
    } else {
        Err(errors.join("; "))
    }
}

fn run_teardown_step(step: &TeardownStep, ctx: &ExecutionContext) -> Result<(), String> {
    if let Some(dir_path) = &step.remove_dir {
        let path = ctx.resolve_path(dir_path);
        if path.exists() {
            std::fs::remove_dir_all(&path)
                .map_err(|e| format!("Failed to remove {}: {e}", dir_path.display()))?;
        }
    }

    if let Some(file_path) = &step.remove_file {
        let path = ctx.resolve_path(file_path);
        if path.exists() {
            std::fs::remove_file(&path)
                .map_err(|e| format!("Failed to remove {}: {e}", file_path.display()))?;
        }
    }

    if let Some(run) = &step.run {
        run_simple_command(run, ctx)?;
    }

    Ok(())
}

fn run_simple_command(run: &RunStep, ctx: &ExecutionContext) -> Result<(), String> {
    let mut cmd = Command::new(&run.cmd);
    cmd.args(&run.args);
    cmd.current_dir(&ctx.sandbox_dir);

    if !ctx.inherit_env {
        cmd.env_clear();
    }
    for (k, v) in &ctx.env {
        cmd.env(k, v);
    }

    let output = cmd
        .output()
        .map_err(|e| format!("Failed to run {}: {e}", run.cmd))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(format!(
            "Command {} failed with exit code {:?}: {}",
            run.cmd,
            output.status.code(),
            stderr.trim()
        ));
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::schema::{CopyFile, WriteFile};

    /// Helper to create a minimal test spec with one test.
    fn make_spec(test: Test) -> TestSpec {
        TestSpec {
            version: 1,
            sandbox: Sandbox::default(),
            timeout: None,
            setup: vec![],
            tests: vec![test],
            teardown: vec![],
        }
    }

    /// Helper to run a spec without suite config (for backwards compat in tests).
    fn run_spec_standalone(spec: &TestSpec) -> SpecResult {
        run_spec(spec, None)
    }

    /// Helper to create a minimal test with a command.
    fn make_test(name: &str, cmd: &str, args: Vec<&str>) -> Test {
        Test {
            name: name.to_string(),
            description: None,
            setup: vec![],
            run: Run {
                cmd: cmd.to_string(),
                args: args.into_iter().map(String::from).collect(),
                stdin: None,
                env: HashMap::new(),
                cwd: None,
                shell: false,
            },
            expect: Expect::default(),
            teardown: vec![],
            timeout: None,
            serial: false,
        }
    }

    // ==================== Basic Execution Tests ====================

    #[test]
    fn test_simple_echo() {
        let test = make_test("echo_test", "echo", vec!["hello"]);
        let spec = make_spec(test);
        let result = run_spec_standalone(&spec);

        assert_eq!(result.tests.len(), 1);
        assert!(
            result.tests[0].passed,
            "failures: {:?}",
            result.tests[0].failures
        );
        assert_eq!(result.tests[0].name, "echo_test");
    }

    #[test]
    fn test_exit_code_zero() {
        let mut test = make_test("exit_zero", "true", vec![]);
        test.expect.exit = Some(0);
        let spec = make_spec(test);
        let result = run_spec_standalone(&spec);

        assert!(
            result.tests[0].passed,
            "failures: {:?}",
            result.tests[0].failures
        );
    }

    #[test]
    fn test_exit_code_nonzero() {
        let mut test = make_test("exit_one", "false", vec![]);
        test.expect.exit = Some(1);
        let spec = make_spec(test);
        let result = run_spec_standalone(&spec);

        assert!(
            result.tests[0].passed,
            "failures: {:?}",
            result.tests[0].failures
        );
    }

    #[test]
    fn test_exit_code_mismatch() {
        let mut test = make_test("exit_mismatch", "true", vec![]);
        test.expect.exit = Some(1); // Expecting 1 but will get 0
        let spec = make_spec(test);
        let result = run_spec_standalone(&spec);

        assert!(!result.tests[0].passed);
        assert!(result.tests[0].failures[0].contains("Exit code"));
    }

    // ==================== Stdout Assertion Tests ====================

    #[test]
    fn test_stdout_exact_match() {
        let mut test = make_test("stdout_exact", "echo", vec!["hello"]);
        test.expect.stdout = Some(OutputMatch::Exact("hello\n".to_string()));
        let spec = make_spec(test);
        let result = run_spec_standalone(&spec);

        assert!(
            result.tests[0].passed,
            "failures: {:?}",
            result.tests[0].failures
        );
    }

    #[test]
    fn test_stdout_exact_mismatch() {
        let mut test = make_test("stdout_exact_fail", "echo", vec!["hello"]);
        test.expect.stdout = Some(OutputMatch::Exact("world\n".to_string()));
        let spec = make_spec(test);
        let result = run_spec_standalone(&spec);

        assert!(!result.tests[0].passed);
        assert!(result.tests[0].failures[0].contains("expected exact match"));
    }

    #[test]
    fn test_stdout_contains() {
        let mut test = make_test("stdout_contains", "echo", vec!["hello world"]);
        test.expect.stdout = Some(OutputMatch::Structured(OutputMatchStructured {
            equals: None,
            contains: Some("world".to_string()),
            regex: None,
        }));
        let spec = make_spec(test);
        let result = run_spec_standalone(&spec);

        assert!(
            result.tests[0].passed,
            "failures: {:?}",
            result.tests[0].failures
        );
    }

    #[test]
    fn test_stdout_contains_mismatch() {
        let mut test = make_test("stdout_contains_fail", "echo", vec!["hello"]);
        test.expect.stdout = Some(OutputMatch::Structured(OutputMatchStructured {
            equals: None,
            contains: Some("world".to_string()),
            regex: None,
        }));
        let spec = make_spec(test);
        let result = run_spec_standalone(&spec);

        assert!(!result.tests[0].passed);
        assert!(result.tests[0].failures[0].contains("expected to contain"));
    }

    #[test]
    fn test_stdout_regex() {
        let mut test = make_test("stdout_regex", "echo", vec!["hello123world"]);
        test.expect.stdout = Some(OutputMatch::Structured(OutputMatchStructured {
            equals: None,
            contains: None,
            regex: Some(r"hello\d+world".to_string()),
        }));
        let spec = make_spec(test);
        let result = run_spec_standalone(&spec);

        assert!(
            result.tests[0].passed,
            "failures: {:?}",
            result.tests[0].failures
        );
    }

    #[test]
    fn test_stdout_regex_mismatch() {
        let mut test = make_test("stdout_regex_fail", "echo", vec!["hello"]);
        test.expect.stdout = Some(OutputMatch::Structured(OutputMatchStructured {
            equals: None,
            contains: None,
            regex: Some(r"\d+".to_string()),
        }));
        let spec = make_spec(test);
        let result = run_spec_standalone(&spec);

        assert!(!result.tests[0].passed);
        assert!(result.tests[0].failures[0].contains("expected to match regex"));
    }

    #[test]
    fn test_stdout_invalid_regex() {
        let mut test = make_test("stdout_invalid_regex", "echo", vec!["hello"]);
        test.expect.stdout = Some(OutputMatch::Structured(OutputMatchStructured {
            equals: None,
            contains: None,
            regex: Some(r"[invalid".to_string()),
        }));
        let spec = make_spec(test);
        let result = run_spec_standalone(&spec);

        assert!(!result.tests[0].passed);
        assert!(result.tests[0].failures[0].contains("invalid regex"));
    }

    // ==================== Stderr Assertion Tests ====================

    #[test]
    fn test_stderr_contains() {
        let mut test = make_test("stderr_test", "sh", vec!["-c", "echo error >&2"]);
        test.expect.stderr = Some(OutputMatch::Structured(OutputMatchStructured {
            equals: None,
            contains: Some("error".to_string()),
            regex: None,
        }));
        let spec = make_spec(test);
        let result = run_spec_standalone(&spec);

        assert!(
            result.tests[0].passed,
            "failures: {:?}",
            result.tests[0].failures
        );
    }

    // ==================== File Expectation Tests ====================

    #[test]
    fn test_file_exists() {
        let mut test = make_test("file_exists", "touch", vec!["output.txt"]);
        test.expect.files = vec![FileExpect {
            path: PathBuf::from("output.txt"),
            exists: Some(true),
            contents: None,
        }];
        let spec = make_spec(test);
        let result = run_spec_standalone(&spec);

        assert!(
            result.tests[0].passed,
            "failures: {:?}",
            result.tests[0].failures
        );
    }

    #[test]
    fn test_file_not_exists() {
        let mut test = make_test("file_not_exists", "true", vec![]);
        test.expect.files = vec![FileExpect {
            path: PathBuf::from("nonexistent.txt"),
            exists: Some(false),
            contents: None,
        }];
        let spec = make_spec(test);
        let result = run_spec_standalone(&spec);

        assert!(
            result.tests[0].passed,
            "failures: {:?}",
            result.tests[0].failures
        );
    }

    #[test]
    fn test_file_exists_failure() {
        let mut test = make_test("file_exists_fail", "true", vec![]);
        test.expect.files = vec![FileExpect {
            path: PathBuf::from("missing.txt"),
            exists: Some(true),
            contents: None,
        }];
        let spec = make_spec(test);
        let result = run_spec_standalone(&spec);

        assert!(!result.tests[0].passed);
        assert!(result.tests[0].failures[0].contains("should exist"));
    }

    #[test]
    fn test_file_contents() {
        let mut test = make_test("file_contents", "sh", vec!["-c", "echo hello > output.txt"]);
        test.expect.files = vec![FileExpect {
            path: PathBuf::from("output.txt"),
            exists: None,
            contents: Some(OutputMatch::Exact("hello\n".to_string())),
        }];
        let spec = make_spec(test);
        let result = run_spec_standalone(&spec);

        assert!(
            result.tests[0].passed,
            "failures: {:?}",
            result.tests[0].failures
        );
    }

    #[test]
    fn test_file_contents_contains() {
        let mut test = make_test(
            "file_contents_contains",
            "sh",
            vec!["-c", "echo 'hello world' > output.txt"],
        );
        test.expect.files = vec![FileExpect {
            path: PathBuf::from("output.txt"),
            exists: None,
            contents: Some(OutputMatch::Structured(OutputMatchStructured {
                equals: None,
                contains: Some("world".to_string()),
                regex: None,
            })),
        }];
        let spec = make_spec(test);
        let result = run_spec_standalone(&spec);

        assert!(
            result.tests[0].passed,
            "failures: {:?}",
            result.tests[0].failures
        );
    }

    // ==================== Setup/Teardown Tests ====================

    #[test]
    fn test_file_level_setup_write_file() {
        let mut test = make_test("read_setup_file", "cat", vec!["config.txt"]);
        test.expect.stdout = Some(OutputMatch::Exact("test config\n".to_string()));
        let mut spec = make_spec(test);
        spec.setup = vec![SetupStep {
            write_file: Some(WriteFile {
                path: PathBuf::from("config.txt"),
                contents: "test config\n".to_string(),
            }),
            create_dir: None,
            copy_file: None,
            run: None,
        }];
        let result = run_spec_standalone(&spec);

        assert!(
            result.tests[0].passed,
            "failures: {:?}",
            result.tests[0].failures
        );
    }

    #[test]
    fn test_file_level_setup_create_dir() {
        let mut test = make_test("check_dir", "test", vec!["-d", "subdir"]);
        test.expect.exit = Some(0);
        let mut spec = make_spec(test);
        spec.setup = vec![SetupStep {
            write_file: None,
            create_dir: Some(PathBuf::from("subdir")),
            copy_file: None,
            run: None,
        }];
        let result = run_spec_standalone(&spec);

        assert!(
            result.tests[0].passed,
            "failures: {:?}",
            result.tests[0].failures
        );
    }

    #[test]
    fn test_test_level_setup() {
        let mut test = make_test("read_test_setup_file", "cat", vec!["test_config.txt"]);
        test.expect.stdout = Some(OutputMatch::Exact("per-test config\n".to_string()));
        test.setup = vec![SetupStep {
            write_file: Some(WriteFile {
                path: PathBuf::from("test_config.txt"),
                contents: "per-test config\n".to_string(),
            }),
            create_dir: None,
            copy_file: None,
            run: None,
        }];
        let spec = make_spec(test);
        let result = run_spec_standalone(&spec);

        assert!(
            result.tests[0].passed,
            "failures: {:?}",
            result.tests[0].failures
        );
    }

    #[test]
    fn test_setup_copy_file() {
        let mut test = make_test("read_copied_file", "cat", vec!["dest.txt"]);
        test.expect.stdout = Some(OutputMatch::Exact("original content\n".to_string()));
        let mut spec = make_spec(test);
        spec.setup = vec![
            SetupStep {
                write_file: Some(WriteFile {
                    path: PathBuf::from("source.txt"),
                    contents: "original content\n".to_string(),
                }),
                create_dir: None,
                copy_file: None,
                run: None,
            },
            SetupStep {
                write_file: None,
                create_dir: None,
                copy_file: Some(CopyFile {
                    from: PathBuf::from("source.txt"),
                    to: PathBuf::from("dest.txt"),
                }),
                run: None,
            },
        ];
        let result = run_spec_standalone(&spec);

        assert!(
            result.tests[0].passed,
            "failures: {:?}",
            result.tests[0].failures
        );
    }

    #[test]
    fn test_setup_run_command() {
        let mut test = make_test("check_setup_command", "cat", vec!["created_by_setup.txt"]);
        test.expect.stdout = Some(OutputMatch::Structured(OutputMatchStructured {
            equals: None,
            contains: Some("setup ran".to_string()),
            regex: None,
        }));
        let mut spec = make_spec(test);
        spec.setup = vec![SetupStep {
            write_file: None,
            create_dir: None,
            copy_file: None,
            run: Some(RunStep {
                cmd: "sh".to_string(),
                args: vec![
                    "-c".to_string(),
                    "echo 'setup ran' > created_by_setup.txt".to_string(),
                ],
            }),
        }];
        let result = run_spec_standalone(&spec);

        assert!(
            result.tests[0].passed,
            "failures: {:?}",
            result.tests[0].failures
        );
    }

    #[test]
    fn test_teardown_removes_file() {
        let mut test = make_test("create_file", "touch", vec!["to_remove.txt"]);
        test.expect.files = vec![FileExpect {
            path: PathBuf::from("to_remove.txt"),
            exists: Some(true),
            contents: None,
        }];
        let mut spec = make_spec(test);
        spec.teardown = vec![TeardownStep {
            remove_dir: None,
            remove_file: Some(PathBuf::from("to_remove.txt")),
            run: None,
        }];
        let result = run_spec_standalone(&spec);

        // Test should pass (file existed when checked)
        assert!(
            result.tests[0].passed,
            "failures: {:?}",
            result.tests[0].failures
        );
        // No teardown errors
        assert!(
            result
                .tests
                .iter()
                .all(|t| t.name != "<teardown>" || t.passed)
        );
    }

    // ==================== Environment Variable Tests ====================

    #[test]
    fn test_sandbox_env() {
        let mut test = make_test("env_test", "sh", vec!["-c", "echo $MY_VAR"]);
        test.expect.stdout = Some(OutputMatch::Exact("test_value\n".to_string()));
        let mut spec = make_spec(test);
        spec.sandbox
            .env
            .insert("MY_VAR".to_string(), "test_value".to_string());
        let result = run_spec_standalone(&spec);

        assert!(
            result.tests[0].passed,
            "failures: {:?}",
            result.tests[0].failures
        );
    }

    #[test]
    fn test_command_env_override() {
        let mut test = make_test("env_override", "sh", vec!["-c", "echo $MY_VAR"]);
        test.expect.stdout = Some(OutputMatch::Exact("overridden\n".to_string()));
        test.run
            .env
            .insert("MY_VAR".to_string(), "overridden".to_string());
        let mut spec = make_spec(test);
        spec.sandbox
            .env
            .insert("MY_VAR".to_string(), "original".to_string());
        let result = run_spec_standalone(&spec);

        assert!(
            result.tests[0].passed,
            "failures: {:?}",
            result.tests[0].failures
        );
    }

    #[test]
    fn test_env_cleared_without_inherit() {
        // When inherit_env is false (default), custom host env vars should not be visible
        // Note: We use a custom var because sh may set default PATH even with cleared env
        let mut test = make_test(
            "env_cleared",
            "sh",
            vec!["-c", "echo ${BINTEST_CUSTOM_VAR:-empty}"],
        );
        test.expect.stdout = Some(OutputMatch::Exact("empty\n".to_string()));
        let spec = make_spec(test);
        // Even if BINTEST_CUSTOM_VAR is set in the host, it shouldn't be inherited
        let result = run_spec_standalone(&spec);

        assert!(
            result.tests[0].passed,
            "failures: {:?}",
            result.tests[0].failures
        );
    }

    // ==================== Stdin Tests ====================

    #[test]
    fn test_stdin() {
        let mut test = make_test("stdin_test", "cat", vec![]);
        test.run.stdin = Some("input data".to_string());
        test.expect.stdout = Some(OutputMatch::Exact("input data".to_string()));
        let spec = make_spec(test);
        let result = run_spec_standalone(&spec);

        assert!(
            result.tests[0].passed,
            "failures: {:?}",
            result.tests[0].failures
        );
    }

    // ==================== Shell Mode Tests ====================

    #[test]
    fn test_shell_mode() {
        let mut test = make_test("shell_test", "echo", vec!["hello", "&&", "echo", "world"]);
        test.run.shell = true;
        test.expect.stdout = Some(OutputMatch::Structured(OutputMatchStructured {
            equals: None,
            contains: Some("hello".to_string()),
            regex: None,
        }));
        let spec = make_spec(test);
        let result = run_spec_standalone(&spec);

        assert!(
            result.tests[0].passed,
            "failures: {:?}",
            result.tests[0].failures
        );
    }

    // ==================== Timeout Tests ====================

    #[test]
    fn test_timeout() {
        let mut test = make_test("timeout_test", "sleep", vec!["10"]);
        test.timeout = Some(1); // 1 second timeout
        let spec = make_spec(test);
        let result = run_spec_standalone(&spec);

        assert!(!result.tests[0].passed);
        assert!(result.tests[0].failures[0].contains("timed out"));
    }

    // ==================== Multiple Tests ====================

    #[test]
    fn test_multiple_tests_in_spec() {
        let test1 = make_test("test_one", "echo", vec!["one"]);
        let test2 = make_test("test_two", "echo", vec!["two"]);
        let spec = TestSpec {
            version: 1,
            sandbox: Sandbox::default(),
            timeout: None,
            setup: vec![],
            tests: vec![test1, test2],
            teardown: vec![],
        };
        let result = run_spec_standalone(&spec);

        assert_eq!(result.tests.len(), 2);
        assert!(result.tests[0].passed);
        assert!(result.tests[1].passed);
        assert_eq!(result.tests[0].name, "test_one");
        assert_eq!(result.tests[1].name, "test_two");
    }

    #[test]
    fn test_shared_sandbox_between_tests() {
        // First test creates a file, second test reads it
        let mut test1 = make_test("create_file", "sh", vec!["-c", "echo shared > shared.txt"]);
        test1.expect.exit = Some(0);
        let mut test2 = make_test("read_file", "cat", vec!["shared.txt"]);
        test2.expect.stdout = Some(OutputMatch::Exact("shared\n".to_string()));
        let spec = TestSpec {
            version: 1,
            sandbox: Sandbox::default(),
            timeout: None,
            setup: vec![],
            tests: vec![test1, test2],
            teardown: vec![],
        };
        let result = run_spec_standalone(&spec);

        assert!(
            result.tests[0].passed,
            "test1 failures: {:?}",
            result.tests[0].failures
        );
        assert!(
            result.tests[1].passed,
            "test2 failures: {:?}",
            result.tests[1].failures
        );
    }

    // ==================== Working Directory Tests ====================

    #[test]
    fn test_custom_cwd() {
        let mut test = make_test("cwd_test", "pwd", vec![]);
        test.expect.stdout = Some(OutputMatch::Structured(OutputMatchStructured {
            equals: None,
            contains: Some("subdir".to_string()),
            regex: None,
        }));
        test.run.cwd = Some(PathBuf::from("subdir"));
        let mut spec = make_spec(test);
        spec.setup = vec![SetupStep {
            write_file: None,
            create_dir: Some(PathBuf::from("subdir")),
            copy_file: None,
            run: None,
        }];
        let result = run_spec_standalone(&spec);

        assert!(
            result.tests[0].passed,
            "failures: {:?}",
            result.tests[0].failures
        );
    }

    // ==================== Command Not Found ====================

    #[test]
    fn test_command_not_found() {
        let test = make_test("not_found", "nonexistent_command_12345", vec![]);
        let spec = make_spec(test);
        let result = run_spec_standalone(&spec);

        assert!(!result.tests[0].passed);
        assert!(result.tests[0].failures[0].contains("Failed to spawn"));
    }

    // ==================== Suite Config Tests ====================

    #[test]
    fn test_suite_config_timeout() {
        // Suite config sets 1 second timeout, test should timeout
        let suite_config = SuiteConfig {
            version: 1,
            timeout: Some(1),
            env: HashMap::new(),
            inherit_env: None,
            setup: vec![],
            teardown: vec![],
        };

        let test = make_test("slow_test", "sleep", vec!["10"]);
        let spec = make_spec(test);
        let result = run_spec(&spec, Some(&suite_config));

        assert!(!result.tests[0].passed);
        assert!(result.tests[0].failures[0].contains("timed out"));
    }

    #[test]
    fn test_suite_config_env() {
        // Suite config provides environment variable
        let mut suite_env = HashMap::new();
        suite_env.insert("SUITE_VAR".to_string(), "from_suite".to_string());

        let suite_config = SuiteConfig {
            version: 1,
            timeout: None,
            env: suite_env,
            inherit_env: None,
            setup: vec![],
            teardown: vec![],
        };

        let mut test = make_test("env_test", "sh", vec!["-c", "echo $SUITE_VAR"]);
        test.expect.stdout = Some(OutputMatch::Exact("from_suite\n".to_string()));
        let spec = make_spec(test);
        let result = run_spec(&spec, Some(&suite_config));

        assert!(
            result.tests[0].passed,
            "failures: {:?}",
            result.tests[0].failures
        );
    }

    #[test]
    fn test_file_env_overrides_suite_env() {
        // File-level env should override suite-level env
        let mut suite_env = HashMap::new();
        suite_env.insert("MY_VAR".to_string(), "from_suite".to_string());

        let suite_config = SuiteConfig {
            version: 1,
            timeout: None,
            env: suite_env,
            inherit_env: None,
            setup: vec![],
            teardown: vec![],
        };

        let mut test = make_test("env_override", "sh", vec!["-c", "echo $MY_VAR"]);
        test.expect.stdout = Some(OutputMatch::Exact("from_file\n".to_string()));
        let mut spec = make_spec(test);
        spec.sandbox
            .env
            .insert("MY_VAR".to_string(), "from_file".to_string());
        let result = run_spec(&spec, Some(&suite_config));

        assert!(
            result.tests[0].passed,
            "failures: {:?}",
            result.tests[0].failures
        );
    }

    #[test]
    fn test_file_timeout_overrides_suite_timeout() {
        // File-level timeout should override suite-level timeout
        let suite_config = SuiteConfig {
            version: 1,
            timeout: Some(10), // Suite says 10 seconds
            env: HashMap::new(),
            inherit_env: None,
            setup: vec![],
            teardown: vec![],
        };

        let test = make_test("timeout_test", "sleep", vec!["5"]);
        let mut spec = make_spec(test);
        spec.timeout = Some(1); // File says 1 second
        let result = run_spec(&spec, Some(&suite_config));

        assert!(!result.tests[0].passed);
        assert!(result.tests[0].failures[0].contains("timed out"));
    }

    #[test]
    fn test_test_timeout_overrides_file_timeout() {
        // Test-level timeout should override file-level timeout
        let mut test = make_test("timeout_test", "sleep", vec!["5"]);
        test.timeout = Some(1); // Test says 1 second
        let mut spec = make_spec(test);
        spec.timeout = Some(10); // File says 10 seconds
        let result = run_spec_standalone(&spec);

        assert!(!result.tests[0].passed);
        assert!(result.tests[0].failures[0].contains("timed out"));
    }
}
