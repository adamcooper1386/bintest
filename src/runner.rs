//! Test execution engine.
//!
//! Runs test specs in isolated sandboxes and captures results.

use crate::database::ConnectionManager;
use crate::env;
use crate::schema::{
    Condition, DatabaseConfig, DbDriver, Expect, FileExpect, OutputMatch, OutputMatchStructured,
    RowCountExpect, Run, RunStep, Sandbox, SandboxDir, SetupStep, SqlExpect, SqlOnError,
    SqlReturns, SqlReturnsStructured, SuiteConfig, TeardownStep, Test, TestSpec, TreeExpect,
    WorkDir,
};
use std::collections::HashMap;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::thread;
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
    /// Whether the test was skipped due to skip_if or require conditions.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub skipped: bool,
    /// Reason for skipping the test (if skipped).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub skip_reason: Option<String>,
    #[serde(serialize_with = "serialize_duration")]
    pub duration: Duration,
    pub failures: Vec<String>,
    /// Which step failed (None if test-level setup/teardown failed, or for single-step tests).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub failed_step: Option<StepFailure>,
    /// Filesystem changes during test execution (if capture_fs_diff enabled).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fs_diff: Option<FilesystemDiff>,
}

/// Information about which step failed in a multi-step test.
#[derive(Debug, Clone, serde::Serialize)]
pub struct StepFailure {
    /// Step name.
    pub name: String,
    /// Step index (0-based).
    pub index: usize,
}

/// Filesystem changes captured during test execution.
#[derive(Debug, Clone, serde::Serialize)]
pub struct FilesystemDiff {
    /// Files that were created during test execution.
    pub added: Vec<PathBuf>,
    /// Files that were deleted during test execution.
    pub removed: Vec<PathBuf>,
    /// Files that were modified during test execution.
    pub modified: Vec<PathBuf>,
}

fn serialize_duration<S>(duration: &Duration, serializer: S) -> Result<S::Ok, S::Error>
where
    S: serde::Serializer,
{
    serializer.serialize_f64(duration.as_secs_f64())
}

// ============================================================================
// Conditional Execution
// ============================================================================

/// Result of evaluating skip/require conditions for a test.
#[derive(Debug)]
enum ConditionResult {
    /// Test should run.
    Run,
    /// Test should be skipped with the given reason.
    Skip(String),
}

/// Check if a condition is satisfied.
///
/// For `env` conditions: checks if the environment variable is set (non-empty).
/// For `cmd` conditions: checks if the command exits with code 0.
fn check_condition(condition: &Condition) -> bool {
    if let Some(env_var) = &condition.env {
        // Check if environment variable is set and non-empty
        return std::env::var(env_var)
            .map(|v| !v.is_empty())
            .unwrap_or(false);
    }

    if let Some(cmd) = &condition.cmd {
        // Parse command string and execute
        let parts: Vec<&str> = cmd.split_whitespace().collect();
        if parts.is_empty() {
            return false;
        }

        // Interpolate environment variables in command path
        let cmd_path = match env::interpolate_env(parts[0]) {
            Ok(path) => path,
            Err(_) => return false, // Treat interpolation failure as condition not met
        };

        let result = Command::new(&cmd_path)
            .args(&parts[1..])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();

        return result.is_ok_and(|status| status.success());
    }

    // If no condition type is specified, treat as satisfied
    true
}

/// Evaluate skip_if and require conditions for a test.
///
/// Returns `ConditionResult::Run` if the test should run, or
/// `ConditionResult::Skip(reason)` if it should be skipped.
fn evaluate_conditions(test: &Test) -> ConditionResult {
    // Check skip_if conditions - skip if ANY condition is true
    for condition in &test.skip_if {
        if check_condition(condition) {
            let reason = if let Some(env_var) = &condition.env {
                format!("skip_if: environment variable '{}' is set", env_var)
            } else if let Some(cmd) = &condition.cmd {
                format!("skip_if: command '{}' succeeded", cmd)
            } else {
                "skip_if: condition met".to_string()
            };
            return ConditionResult::Skip(reason);
        }
    }

    // Check require conditions - skip if ANY condition is NOT met
    for condition in &test.require {
        if !check_condition(condition) {
            let reason = if let Some(env_var) = &condition.env {
                format!("require: environment variable '{}' is not set", env_var)
            } else if let Some(cmd) = &condition.cmd {
                format!("require: command '{}' failed or not found", cmd)
            } else {
                "require: condition not met".to_string()
            };
            return ConditionResult::Skip(reason);
        }
    }

    ConditionResult::Run
}

/// Run suite-level setup steps.
///
/// Creates a temporary context for suite setup (uses temp directory).
pub fn run_suite_setup(config: &SuiteConfig) -> Result<(), String> {
    if config.setup.is_empty() {
        return Ok(());
    }

    let ctx = ExecutionContext::new(&Sandbox::default(), config.sandbox_dir.as_ref())
        .map_err(|e| format!("Failed to create suite context: {e}"))?;
    let db_manager = ConnectionManager::new(config.databases.clone());
    let result = run_setup_steps(&config.setup, &ctx, &db_manager);
    db_manager.close_all();
    result
}

/// Run suite-level teardown steps.
///
/// Creates a temporary context for suite teardown (uses temp directory).
pub fn run_suite_teardown(config: &SuiteConfig) -> Result<(), String> {
    if config.teardown.is_empty() {
        return Ok(());
    }

    let ctx = ExecutionContext::new(&Sandbox::default(), config.sandbox_dir.as_ref())
        .map_err(|e| format!("Failed to create suite context: {e}"))?;
    let db_manager = ConnectionManager::new(config.databases.clone());
    let result = run_teardown_steps(&config.teardown, &ctx, &db_manager);
    db_manager.close_all();
    result
}

/// Context for test execution within a sandbox.
struct ExecutionContext {
    sandbox_dir: PathBuf,
    env: HashMap<String, String>,
    inherit_env: bool,
    _temp_dir: Option<tempfile::TempDir>,
}

impl ExecutionContext {
    fn new(sandbox: &Sandbox, suite_sandbox_dir: Option<&SandboxDir>) -> std::io::Result<Self> {
        let (sandbox_dir, temp_dir) = match (&sandbox.workdir, suite_sandbox_dir) {
            // If suite specifies a sandbox_dir and workdir is temp, use the suite's dir
            (WorkDir::Temp, Some(SandboxDir::Local)) => {
                let timestamp = chrono::Local::now().format("%Y%m%d_%H%M%S_%3f");
                let dir = PathBuf::from(".bintest").join(timestamp.to_string());
                std::fs::create_dir_all(&dir)?;
                (dir, None)
            }
            (WorkDir::Temp, Some(SandboxDir::Path(p))) => {
                let timestamp = chrono::Local::now().format("%Y%m%d_%H%M%S_%3f");
                let dir = p.join(timestamp.to_string());
                std::fs::create_dir_all(&dir)?;
                (dir, None)
            }
            // Default temp behavior
            (WorkDir::Temp, None) => {
                let temp = tempfile::tempdir()?;
                let path = temp.path().to_path_buf();
                (path, Some(temp))
            }
            // Explicit path always wins
            (WorkDir::Path(p), _) => {
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
    /// Whether to capture filesystem diffs (suite-level default).
    pub capture_fs_diff: bool,
    /// Directory for test sandboxes (from suite config or CLI).
    pub sandbox_dir: Option<SandboxDir>,
    /// Suite-level database configurations.
    pub databases: HashMap<String, DatabaseConfig>,
}

impl EffectiveConfig {
    /// Create from optional suite config.
    pub fn from_suite(suite: Option<&SuiteConfig>) -> Self {
        match suite {
            Some(cfg) => Self {
                default_timeout: cfg.timeout,
                suite_env: cfg.env.clone(),
                inherit_env: cfg.inherit_env,
                capture_fs_diff: cfg.capture_fs_diff,
                sandbox_dir: cfg.sandbox_dir.clone(),
                databases: cfg.databases.clone(),
            },
            None => Self::default(),
        }
    }
}

/// Run a test specification file with optional suite configuration.
#[cfg_attr(not(test), allow(dead_code))]
pub fn run_spec(spec: &TestSpec, suite_config: Option<&SuiteConfig>) -> SpecResult {
    run_spec_filtered(spec, suite_config, None)
}

/// Run a test specification file with optional suite configuration and filter.
pub fn run_spec_filtered(
    spec: &TestSpec,
    suite_config: Option<&SuiteConfig>,
    filter: Option<&str>,
) -> SpecResult {
    let effective = EffectiveConfig::from_suite(suite_config);
    run_spec_with_config(spec, &effective, filter)
}

/// Run a test specification file with effective configuration.
fn run_spec_with_config(
    spec: &TestSpec,
    effective: &EffectiveConfig,
    filter: Option<&str>,
) -> SpecResult {
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

    // Determine file-level capture_fs_diff (file overrides suite)
    let file_capture_fs_diff = spec.capture_fs_diff.unwrap_or(effective.capture_fs_diff);

    let ctx = match ExecutionContext::new(&merged_sandbox, effective.sandbox_dir.as_ref()) {
        Ok(ctx) => ctx,
        Err(e) => {
            return SpecResult {
                tests: vec![TestResult {
                    name: "<setup>".to_string(),
                    passed: false,
                    skipped: false,
                    skip_reason: None,
                    duration: Duration::ZERO,
                    failures: vec![format!("Failed to create sandbox: {e}")],
                    failed_step: None,
                    fs_diff: None,
                }],
            };
        }
    };

    // Merge database configurations (file-level overrides suite-level)
    let mut merged_databases = effective.databases.clone();
    for (name, config) in &spec.databases {
        merged_databases.insert(name.clone(), config.clone());
    }

    // Create connection manager (connections are lazy, opened on first use)
    let db_manager = ConnectionManager::new(merged_databases);

    // Run file-level setup
    if let Err(e) = run_setup_steps(&spec.setup, &ctx, &db_manager) {
        return SpecResult {
            tests: vec![TestResult {
                name: "<setup>".to_string(),
                passed: false,
                skipped: false,
                skip_reason: None,
                duration: Duration::ZERO,
                failures: vec![format!("Setup failed: {e}")],
                failed_step: None,
                fs_diff: None,
            }],
        };
    }

    // Initialize isolation for databases with per_file isolation
    // This captures the post-setup state that will be restored before each test
    let isolated_databases = db_manager.get_isolated_databases();
    for db_name in &isolated_databases {
        if let Err(e) = db_manager.init_isolation(db_name) {
            return SpecResult {
                tests: vec![TestResult {
                    name: "<setup>".to_string(),
                    passed: false,
                    skipped: false,
                    skip_reason: None,
                    duration: Duration::ZERO,
                    failures: vec![format!(
                        "Failed to initialize database isolation for '{}': {}",
                        db_name, e
                    )],
                    failed_step: None,
                    fs_diff: None,
                }],
            };
        }
    }

    // Filter tests by name if a filter is provided
    let filtered_tests: Vec<(usize, &Test)> = spec
        .tests
        .iter()
        .enumerate()
        .filter(|(_, test)| filter.map(|f| test.name.contains(f)).unwrap_or(true))
        .collect();

    // If no tests match the filter, return empty results
    if filtered_tests.is_empty() {
        return SpecResult { tests: vec![] };
    }

    // Partition tests into serial and parallel groups, preserving indices
    let (serial_tests, parallel_tests): (Vec<_>, Vec<_>) = filtered_tests
        .into_iter()
        .partition(|(_, test)| test.serial);

    // Collect results with their indices
    let mut indexed_results: Vec<(usize, TestResult)> = Vec::with_capacity(spec.tests.len());

    // Run serial tests first, in order
    for (idx, test) in serial_tests {
        let result = run_test(test, &ctx, &db_manager, file_timeout, file_capture_fs_diff);
        indexed_results.push((idx, result));
    }

    // Run parallel tests concurrently
    if !parallel_tests.is_empty() {
        let ctx_ref = &ctx;
        let db_ref = &db_manager;
        thread::scope(|s| {
            let handles: Vec<_> = parallel_tests
                .iter()
                .map(|(idx, test)| {
                    let idx = *idx;
                    s.spawn(move || {
                        (
                            idx,
                            run_test(test, ctx_ref, db_ref, file_timeout, file_capture_fs_diff),
                        )
                    })
                })
                .collect();

            for handle in handles {
                let result = handle.join().expect("Test thread panicked");
                indexed_results.push(result);
            }
        });
    }

    // Sort by original index to maintain declaration order
    indexed_results.sort_by_key(|(idx, _)| *idx);
    let mut results: Vec<TestResult> = indexed_results.into_iter().map(|(_, r)| r).collect();

    // Run file-level teardown (always runs)
    if let Err(e) = run_teardown_steps(&spec.teardown, &ctx, &db_manager) {
        results.push(TestResult {
            name: "<teardown>".to_string(),
            passed: false,
            skipped: false,
            skip_reason: None,
            duration: Duration::ZERO,
            failures: vec![format!("Teardown failed: {e}")],
            failed_step: None,
            fs_diff: None,
        });
    }

    // Close database connections
    db_manager.close_all();

    SpecResult { tests: results }
}

fn run_test(
    test: &Test,
    ctx: &ExecutionContext,
    db_manager: &ConnectionManager,
    file_timeout: Option<u64>,
    file_capture_fs_diff: bool,
) -> TestResult {
    let start = Instant::now();
    let mut failures = Vec::new();
    let mut failed_step: Option<StepFailure> = None;

    // Check skip_if and require conditions
    match evaluate_conditions(test) {
        ConditionResult::Skip(reason) => {
            return TestResult {
                name: test.name.clone(),
                passed: true, // Skipped tests count as passed
                skipped: true,
                skip_reason: Some(reason),
                duration: start.elapsed(),
                failures: vec![],
                failed_step: None,
                fs_diff: None,
            };
        }
        ConditionResult::Run => {}
    }

    // Reset database isolation for databases with per_file isolation
    // This restores the post-file-setup state before each test
    for db_name in db_manager.get_isolated_databases() {
        if let Err(e) = db_manager.reset_isolation(&db_name) {
            return TestResult {
                name: test.name.clone(),
                passed: false,
                skipped: false,
                skip_reason: None,
                duration: start.elapsed(),
                failures: vec![format!(
                    "Failed to reset database isolation for '{}': {}",
                    db_name, e
                )],
                failed_step: None,
                fs_diff: None,
            };
        }
    }

    // Determine if we should capture fs diff (test overrides file)
    let capture_fs_diff = test.capture_fs_diff.unwrap_or(file_capture_fs_diff);

    // Test-level setup
    if let Err(e) = run_setup_steps(&test.setup, ctx, db_manager) {
        return TestResult {
            name: test.name.clone(),
            passed: false,
            skipped: false,
            skip_reason: None,
            duration: start.elapsed(),
            failures: vec![format!("Test setup failed: {e}")],
            failed_step: None,
            fs_diff: None,
        };
    }

    // Capture filesystem state before steps (if enabled)
    let snapshot_before = if capture_fs_diff {
        Some(snapshot_filesystem(&ctx.sandbox_dir))
    } else {
        None
    };

    // Determine timeout - test timeout overrides file timeout overrides default
    let timeout_secs = test
        .timeout
        .or(file_timeout)
        .unwrap_or(DEFAULT_TIMEOUT_SECS);
    let timeout = Duration::from_secs(timeout_secs);

    // Execute steps sequentially
    let is_multi_step = test.steps.len() > 1 || test.steps.first().is_some_and(|s| s.name != "run");

    for (step_index, step) in test.steps.iter().enumerate() {
        // Step-level setup
        if let Err(e) = run_setup_steps(&step.setup, ctx, db_manager) {
            let msg = if is_multi_step {
                format!("Step '{}' [{}] setup failed: {e}", step.name, step_index)
            } else {
                format!("Setup failed: {e}")
            };
            failures.push(msg);
            failed_step = Some(StepFailure {
                name: step.name.clone(),
                index: step_index,
            });
            break; // Skip remaining steps
        }

        // Run the step command
        let step_failed = match run_command(&step.run, ctx, timeout) {
            Ok(output) => {
                // Check step assertions
                let mut step_failures = Vec::new();
                check_expectations(&step.expect, &output, ctx, db_manager, &mut step_failures);

                if !step_failures.is_empty() {
                    // Prefix failures with step info for multi-step tests
                    for f in step_failures {
                        let msg = if is_multi_step {
                            format!("Step '{}' [{}]: {f}", step.name, step_index)
                        } else {
                            f
                        };
                        failures.push(msg);
                    }
                    true
                } else {
                    false
                }
            }
            Err(e) => {
                let msg = if is_multi_step {
                    format!(
                        "Step '{}' [{}] execution failed: {e}",
                        step.name, step_index
                    )
                } else {
                    format!("Command execution failed: {e}")
                };
                failures.push(msg);
                true
            }
        };

        // Step-level teardown (always runs for this step, even if assertions failed)
        if let Err(e) = run_teardown_steps(&step.teardown, ctx, db_manager) {
            let msg = if is_multi_step {
                format!("Step '{}' [{}] teardown failed: {e}", step.name, step_index)
            } else {
                format!("Teardown failed: {e}")
            };
            failures.push(msg);
        }

        // If step failed, record it and skip remaining steps
        if step_failed {
            failed_step = Some(StepFailure {
                name: step.name.clone(),
                index: step_index,
            });
            break;
        }
    }

    // Compute filesystem diff (if enabled)
    let fs_diff = snapshot_before.map(|before| {
        let after = snapshot_filesystem(&ctx.sandbox_dir);
        compute_fs_diff(&before, &after)
    });

    // Test-level teardown (always runs)
    if let Err(e) = run_teardown_steps(&test.teardown, ctx, db_manager) {
        failures.push(format!("Test teardown failed: {e}"));
    }

    // For single-step tests (implicit "run" step), don't report failed_step
    let failed_step = if is_multi_step { failed_step } else { None };

    TestResult {
        name: test.name.clone(),
        passed: failures.is_empty(),
        skipped: false,
        skip_reason: None,
        duration: start.elapsed(),
        failures,
        failed_step,
        fs_diff,
    }
}

struct CommandOutput {
    /// Exit code if process exited normally.
    exit_code: Option<i32>,
    /// Signal number if process was terminated by a signal (Unix only).
    signal: Option<i32>,
    stdout: String,
    stderr: String,
}

fn run_command(
    run: &Run,
    ctx: &ExecutionContext,
    timeout: Duration,
) -> Result<CommandOutput, String> {
    // Interpolate environment variables in cmd
    let cmd_path = env::interpolate_env(&run.cmd)?;

    let mut cmd = if run.shell {
        let mut c = Command::new("sh");
        c.arg("-c");
        c.arg(format!("{} {}", cmd_path, run.args.join(" ")));
        c
    } else {
        let mut c = Command::new(&cmd_path);
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

                // Get exit code and signal
                let exit_code = status.code();
                #[cfg(unix)]
                let signal = {
                    use std::os::unix::process::ExitStatusExt;
                    status.signal()
                };
                #[cfg(not(unix))]
                let signal = None;

                return Ok(CommandOutput {
                    exit_code,
                    signal,
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
    db_manager: &ConnectionManager,
    failures: &mut Vec<String>,
) {
    // Check signal or exit code (signal takes precedence if specified)
    if let Some(expected_signal) = expect.signal {
        // Expecting a signal termination
        match output.signal {
            Some(actual_signal) => {
                if actual_signal != expected_signal {
                    failures.push(format!(
                        "Signal: expected {expected_signal}, got {actual_signal}"
                    ));
                }
            }
            None => {
                let exit_info = output
                    .exit_code
                    .map(|c| format!("exit code {c}"))
                    .unwrap_or_else(|| "unknown".to_string());
                failures.push(format!(
                    "Signal: expected {expected_signal}, but process exited with {exit_info}"
                ));
            }
        }
    } else {
        // Expecting normal exit (default behavior)
        let expected_exit = expect.exit.unwrap_or(0);
        match output.exit_code {
            Some(actual_exit) => {
                if actual_exit != expected_exit {
                    failures.push(format!(
                        "Exit code: expected {expected_exit}, got {actual_exit}"
                    ));
                }
            }
            None => {
                // Process was killed by a signal when we expected an exit code
                let signal_info = output
                    .signal
                    .map(|s| format!("signal {s}"))
                    .unwrap_or_else(|| "unknown cause".to_string());
                failures.push(format!(
                    "Exit code: expected {expected_exit}, but process was terminated by {signal_info}"
                ));
            }
        }
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

    // Check tree structure
    if let Some(tree) = &expect.tree {
        check_tree_expect(tree, ctx, failures);
    }

    // Check SQL assertions
    for (i, sql_expect) in expect.sql.iter().enumerate() {
        check_sql_expect(sql_expect, i, db_manager, failures);
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

fn check_tree_expect(tree_expect: &TreeExpect, ctx: &ExecutionContext, failures: &mut Vec<String>) {
    // Determine root directory to check
    let root = tree_expect
        .root
        .as_ref()
        .map(|p| ctx.resolve_path(p))
        .unwrap_or_else(|| ctx.sandbox_dir.clone());

    // Collect all files in the tree
    let actual_files = collect_files_recursive(&root);

    // Check that required paths exist
    for entry in &tree_expect.contains {
        let full_path = root.join(&entry.path);
        if !full_path.exists() {
            failures.push(format!(
                "Tree: expected path to exist: {}",
                entry.path.display()
            ));
            continue;
        }

        // Check contents if specified
        if let Some(matcher) = &entry.contents {
            if full_path.is_file() {
                match std::fs::read_to_string(&full_path) {
                    Ok(contents) => {
                        let name = format!("tree:{}", entry.path.display());
                        if let Err(e) = check_output_match(&name, &contents, matcher) {
                            failures.push(e);
                        }
                    }
                    Err(e) => {
                        failures.push(format!(
                            "Tree: failed to read {}: {e}",
                            entry.path.display()
                        ));
                    }
                }
            } else {
                failures.push(format!(
                    "Tree: cannot check contents of directory: {}",
                    entry.path.display()
                ));
            }
        }
    }

    // Check that excluded paths don't exist
    for excluded in &tree_expect.excludes {
        let full_path = root.join(excluded);
        if full_path.exists() {
            failures.push(format!(
                "Tree: expected path to not exist: {}",
                excluded.display()
            ));
        }
    }

    // If exact mode, verify no unexpected files exist
    if tree_expect.exact {
        let expected_paths: std::collections::HashSet<_> = tree_expect
            .contains
            .iter()
            .map(|e| e.path.clone())
            .collect();

        for actual in &actual_files {
            // Get relative path from root
            if let Ok(relative) = actual.strip_prefix(&root) {
                let relative_path = relative.to_path_buf();
                // Check if this path or any parent is in expected_paths
                let mut is_expected = false;
                let mut check_path = relative_path.clone();
                loop {
                    if expected_paths.contains(&check_path) {
                        is_expected = true;
                        break;
                    }
                    match check_path.parent() {
                        Some(parent) if !parent.as_os_str().is_empty() => {
                            check_path = parent.to_path_buf();
                        }
                        _ => break,
                    }
                }
                if !is_expected {
                    failures.push(format!(
                        "Tree: unexpected file in exact mode: {}",
                        relative_path.display()
                    ));
                }
            }
        }
    }
}

/// Check a SQL assertion.
fn check_sql_expect(
    sql_expect: &SqlExpect,
    index: usize,
    db_manager: &ConnectionManager,
    failures: &mut Vec<String>,
) {
    let db_name = &sql_expect.database;
    let prefix = format!("sql[{index}]");

    // Handle table_exists shorthand
    if let Some(table) = &sql_expect.table_exists {
        let query = table_exists_query(db_manager, db_name, table);
        match db_manager.execute(db_name, &query) {
            Ok(result) => {
                // Result should be non-empty or truthy for table existing
                let exists = !result.is_empty()
                    && result != "0"
                    && result.to_lowercase() != "false"
                    && result.to_lowercase() != "f";
                if !exists {
                    failures.push(format!("{prefix}: table '{}' does not exist", table));
                }
            }
            Err(e) => {
                failures.push(format!("{prefix}: failed to check table existence: {e}"));
            }
        }
        return;
    }

    // Handle table_not_exists shorthand
    if let Some(table) = &sql_expect.table_not_exists {
        let query = table_exists_query(db_manager, db_name, table);
        match db_manager.execute(db_name, &query) {
            Ok(result) => {
                let exists = !result.is_empty()
                    && result != "0"
                    && result.to_lowercase() != "false"
                    && result.to_lowercase() != "f";
                if exists {
                    failures.push(format!("{prefix}: table '{}' exists but should not", table));
                }
            }
            Err(_) => {
                // Query failure likely means table doesn't exist, which is what we want
            }
        }
        return;
    }

    // Handle row_count shorthand
    if let Some(row_count) = &sql_expect.row_count {
        check_row_count(row_count, db_name, &prefix, db_manager, failures);
        return;
    }

    // Handle raw query assertions
    if let Some(query) = &sql_expect.query {
        match db_manager.execute(db_name, query) {
            Ok(result) => {
                // Check returns_empty
                if let Some(true) = sql_expect.returns_empty {
                    if !result.is_empty() {
                        failures.push(format!(
                            "{prefix}: expected empty result\n  Query: {query}\n  Got: {result:?}"
                        ));
                    }
                    return;
                }

                // Check returns_null
                if let Some(true) = sql_expect.returns_null {
                    let is_null = result.trim().eq_ignore_ascii_case("null");
                    if !is_null {
                        failures.push(format!(
                            "{prefix}: expected NULL result\n  Query: {query}\n  Got: {result:?}"
                        ));
                    }
                    return;
                }

                // Check returns_one_row
                if let Some(true) = sql_expect.returns_one_row {
                    let row_count = if result.is_empty() {
                        0
                    } else {
                        result.lines().count()
                    };
                    if row_count != 1 {
                        failures.push(format!(
                            "{prefix}: expected exactly one row\n  Query: {query}\n  Got: {} row(s)",
                            row_count
                        ));
                    }
                    return;
                }

                // Check returns
                if let Some(returns) = &sql_expect.returns
                    && let Err(e) = check_sql_returns(&prefix, query, &result, returns)
                {
                    failures.push(e);
                }
            }
            Err(e) => {
                failures.push(format!(
                    "{prefix}: query failed\n  Query: {query}\n  Error: {e}"
                ));
            }
        }
    }
}

/// Generate a table existence check query appropriate for the database driver.
fn table_exists_query(db_manager: &ConnectionManager, db_name: &str, table: &str) -> String {
    // Get driver from config if available
    let driver = db_manager.get_driver(db_name).unwrap_or(DbDriver::Postgres);

    match driver {
        DbDriver::Postgres => {
            format!(
                "SELECT EXISTS (SELECT FROM information_schema.tables WHERE table_name = '{table}')"
            )
        }
        DbDriver::Sqlite => {
            format!("SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name='{table}'")
        }
    }
}

/// Check row count assertion.
fn check_row_count(
    row_count: &RowCountExpect,
    db_name: &str,
    prefix: &str,
    db_manager: &ConnectionManager,
    failures: &mut Vec<String>,
) {
    let table = &row_count.table;
    let query = format!("SELECT COUNT(*) FROM {table}");

    match db_manager.execute(db_name, &query) {
        Ok(result) => {
            let count: u64 = match result.trim().parse() {
                Ok(n) => n,
                Err(_) => {
                    failures.push(format!(
                        "{prefix}: failed to parse row count as number: {result:?}"
                    ));
                    return;
                }
            };

            if let Some(expected) = row_count.equals
                && count != expected
            {
                failures.push(format!(
                    "{prefix}: row_count for '{}': expected {}, got {}",
                    table, expected, count
                ));
            }

            if let Some(min) = row_count.greater_than
                && count <= min
            {
                failures.push(format!(
                    "{prefix}: row_count for '{}': expected > {}, got {}",
                    table, min, count
                ));
            }

            if let Some(max) = row_count.less_than
                && count >= max
            {
                failures.push(format!(
                    "{prefix}: row_count for '{}': expected < {}, got {}",
                    table, max, count
                ));
            }
        }
        Err(e) => {
            failures.push(format!(
                "{prefix}: failed to count rows in '{}': {}",
                table, e
            ));
        }
    }
}

/// Check SQL query result against expected returns.
fn check_sql_returns(
    prefix: &str,
    query: &str,
    actual: &str,
    returns: &SqlReturns,
) -> Result<(), String> {
    match returns {
        SqlReturns::Exact(expected) => {
            if actual != expected {
                Err(format!(
                    "{prefix}: expected exact match\n  Query: {query}\n  Expected: {expected:?}\n  Got: {actual:?}"
                ))
            } else {
                Ok(())
            }
        }
        SqlReturns::Structured(s) => check_sql_returns_structured(prefix, query, actual, s),
    }
}

/// Check SQL query result against structured match.
fn check_sql_returns_structured(
    prefix: &str,
    query: &str,
    actual: &str,
    matcher: &SqlReturnsStructured,
) -> Result<(), String> {
    if let Some(expected) = &matcher.equals
        && actual != expected
    {
        return Err(format!(
            "{prefix}: expected exact match\n  Query: {query}\n  Expected: {expected:?}\n  Got: {actual:?}"
        ));
    }

    if let Some(substring) = &matcher.contains
        && !actual.contains(substring)
    {
        return Err(format!(
            "{prefix}: expected to contain {substring:?}\n  Query: {query}\n  Got: {actual:?}"
        ));
    }

    if let Some(pattern) = &matcher.regex {
        let re = regex::Regex::new(pattern)
            .map_err(|e| format!("{prefix}: invalid regex {pattern:?}: {e}"))?;
        if !re.is_match(actual) {
            return Err(format!(
                "{prefix}: expected to match regex {pattern:?}\n  Query: {query}\n  Got: {actual:?}"
            ));
        }
    }

    Ok(())
}

/// Recursively copy a directory and all its contents.
fn copy_dir_recursive(from: &Path, to: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(to)?;
    for entry in std::fs::read_dir(from)? {
        let entry = entry?;
        let file_type = entry.file_type()?;
        let src = entry.path();
        let dst = to.join(entry.file_name());
        if file_type.is_dir() {
            copy_dir_recursive(&src, &dst)?;
        } else {
            std::fs::copy(&src, &dst)?;
        }
    }
    Ok(())
}

/// Recursively collect all files in a directory.
fn collect_files_recursive(dir: &Path) -> Vec<PathBuf> {
    let mut files = Vec::new();
    if let Ok(entries) = std::fs::read_dir(dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                files.extend(collect_files_recursive(&path));
            } else {
                files.push(path);
            }
        }
    }
    files
}

/// State of a file for diff comparison.
#[derive(Debug, Clone, PartialEq, Eq)]
struct FileState {
    /// Size in bytes.
    size: u64,
    /// Modification time (as duration since UNIX_EPOCH).
    modified: Option<Duration>,
}

/// Snapshot the filesystem state of a directory.
fn snapshot_filesystem(root: &Path) -> HashMap<PathBuf, FileState> {
    let mut snapshot = HashMap::new();
    snapshot_dir_recursive(root, root, &mut snapshot);
    snapshot
}

fn snapshot_dir_recursive(root: &Path, dir: &Path, snapshot: &mut HashMap<PathBuf, FileState>) {
    if let Ok(entries) = std::fs::read_dir(dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if let Ok(relative) = path.strip_prefix(root)
                && let Ok(metadata) = entry.metadata()
            {
                let modified = metadata
                    .modified()
                    .ok()
                    .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok());
                snapshot.insert(
                    relative.to_path_buf(),
                    FileState {
                        size: metadata.len(),
                        modified,
                    },
                );
            }
            if path.is_dir() {
                snapshot_dir_recursive(root, &path, snapshot);
            }
        }
    }
}

/// Compute the difference between two filesystem snapshots.
fn compute_fs_diff(
    before: &HashMap<PathBuf, FileState>,
    after: &HashMap<PathBuf, FileState>,
) -> FilesystemDiff {
    let mut added = Vec::new();
    let mut removed = Vec::new();
    let mut modified = Vec::new();

    // Find added and modified files
    for (path, after_state) in after {
        match before.get(path) {
            None => added.push(path.clone()),
            Some(before_state) => {
                if before_state != after_state {
                    modified.push(path.clone());
                }
            }
        }
    }

    // Find removed files
    for path in before.keys() {
        if !after.contains_key(path) {
            removed.push(path.clone());
        }
    }

    // Sort for deterministic output
    added.sort();
    removed.sort();
    modified.sort();

    FilesystemDiff {
        added,
        removed,
        modified,
    }
}

fn run_setup_steps(
    steps: &[SetupStep],
    ctx: &ExecutionContext,
    db_manager: &ConnectionManager,
) -> Result<(), String> {
    for step in steps {
        run_setup_step(step, ctx, db_manager)?;
    }
    Ok(())
}

fn run_setup_step(
    step: &SetupStep,
    ctx: &ExecutionContext,
    db_manager: &ConnectionManager,
) -> Result<(), String> {
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

    if let Some(copy) = &step.copy_dir {
        copy_dir_recursive(&ctx.resolve_path(&copy.from), &ctx.resolve_path(&copy.to)).map_err(
            |e| {
                format!(
                    "Failed to copy directory {} to {}: {e}",
                    copy.from.display(),
                    copy.to.display()
                )
            },
        )?;
    }

    if let Some(run) = &step.run {
        run_simple_command(run, ctx)?;
    }

    if let Some(sql) = &step.sql {
        run_sql_statements(sql, db_manager)?;
    }

    if let Some(sql_file) = &step.sql_file {
        run_sql_file(sql_file, ctx, db_manager)?;
    }

    if let Some(snapshot) = &step.db_snapshot {
        db_manager
            .create_snapshot(&snapshot.database, &snapshot.name)
            .map_err(|e| format!("Failed to create snapshot '{}': {e}", snapshot.name))?;
    }

    if let Some(restore) = &step.db_restore {
        db_manager
            .restore_snapshot(&restore.database, &restore.name)
            .map_err(|e| format!("Failed to restore snapshot '{}': {e}", restore.name))?;
    }

    Ok(())
}

/// Execute SQL statements from a SqlStatements config.
fn run_sql_statements(
    sql: &crate::schema::SqlStatements,
    db_manager: &ConnectionManager,
) -> Result<(), String> {
    let mut errors = Vec::new();

    for statement in &sql.statements {
        if let Err(e) = db_manager.execute(&sql.database, statement) {
            let err_msg = format!("SQL error: {e}");
            if sql.on_error == SqlOnError::Fail {
                return Err(err_msg);
            }
            errors.push(err_msg);
        }
    }

    if !errors.is_empty() && sql.on_error == SqlOnError::Fail {
        return Err(errors.join("; "));
    }

    Ok(())
}

/// Execute SQL from a file.
fn run_sql_file(
    sql_file: &crate::schema::SqlFile,
    ctx: &ExecutionContext,
    db_manager: &ConnectionManager,
) -> Result<(), String> {
    let path = ctx.resolve_path(&sql_file.path);
    let contents = std::fs::read_to_string(&path)
        .map_err(|e| format!("Failed to read SQL file {}: {e}", sql_file.path.display()))?;

    // Execute the file contents as a single statement
    // Note: For multiple statements, users should use sql.statements instead
    if let Err(e) = db_manager.execute(&sql_file.database, &contents) {
        let err_msg = format!("SQL file {} failed: {e}", sql_file.path.display());
        if sql_file.on_error == SqlOnError::Fail {
            return Err(err_msg);
        }
    }

    Ok(())
}

fn run_teardown_steps(
    steps: &[TeardownStep],
    ctx: &ExecutionContext,
    db_manager: &ConnectionManager,
) -> Result<(), String> {
    let mut errors = Vec::new();
    for step in steps {
        if let Err(e) = run_teardown_step(step, ctx, db_manager) {
            errors.push(e);
        }
    }
    if errors.is_empty() {
        Ok(())
    } else {
        Err(errors.join("; "))
    }
}

fn run_teardown_step(
    step: &TeardownStep,
    ctx: &ExecutionContext,
    db_manager: &ConnectionManager,
) -> Result<(), String> {
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

    if let Some(sql) = &step.sql {
        // For teardown, we use the same function but always continue on error
        // since we want to try all statements
        let sql_with_continue = crate::schema::SqlStatements {
            database: sql.database.clone(),
            statements: sql.statements.clone(),
            on_error: if sql.on_error == SqlOnError::Fail {
                SqlOnError::Fail
            } else {
                SqlOnError::Continue
            },
        };
        run_sql_statements(&sql_with_continue, db_manager)?;
    }

    if let Some(restore) = &step.db_restore {
        db_manager
            .restore_snapshot(&restore.database, &restore.name)
            .map_err(|e| format!("Failed to restore snapshot '{}': {e}", restore.name))?;
    }

    Ok(())
}

fn run_simple_command(run: &RunStep, ctx: &ExecutionContext) -> Result<(), String> {
    // Interpolate environment variables in cmd
    let cmd_path = env::interpolate_env(&run.cmd)?;

    let mut cmd = Command::new(&cmd_path);
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
        .map_err(|e| format!("Failed to run {}: {e}", cmd_path))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(format!(
            "Command {} failed with exit code {:?}: {}",
            cmd_path,
            output.status.code(),
            stderr.trim()
        ));
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::schema::{CopyDir, CopyFile, Step, WriteFile};

    /// Helper to create a minimal test spec with one test.
    fn make_spec(test: Test) -> TestSpec {
        TestSpec {
            version: 1,
            sandbox: Sandbox::default(),
            timeout: None,
            capture_fs_diff: None,
            databases: HashMap::new(),
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
            skip_if: vec![],
            require: vec![],
            setup: vec![],
            steps: vec![Step {
                name: "run".to_string(),
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
            }],
            teardown: vec![],
            timeout: None,
            serial: false,
            capture_fs_diff: None,
        }
    }

    /// Helper trait to access the first step's run/expect for test compatibility.
    trait TestExt {
        fn run_mut(&mut self) -> &mut Run;
        fn expect_mut(&mut self) -> &mut Expect;
    }

    impl TestExt for Test {
        fn run_mut(&mut self) -> &mut Run {
            &mut self.steps[0].run
        }
        fn expect_mut(&mut self) -> &mut Expect {
            &mut self.steps[0].expect
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
        test.expect_mut().exit = Some(0);
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
        test.expect_mut().exit = Some(1);
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
        test.expect_mut().exit = Some(1); // Expecting 1 but will get 0
        let spec = make_spec(test);
        let result = run_spec_standalone(&spec);

        assert!(!result.tests[0].passed);
        assert!(result.tests[0].failures[0].contains("Exit code"));
    }

    // ==================== Stdout Assertion Tests ====================

    #[test]
    fn test_stdout_exact_match() {
        let mut test = make_test("stdout_exact", "echo", vec!["hello"]);
        test.expect_mut().stdout = Some(OutputMatch::Exact("hello\n".to_string()));
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
        test.expect_mut().stdout = Some(OutputMatch::Exact("world\n".to_string()));
        let spec = make_spec(test);
        let result = run_spec_standalone(&spec);

        assert!(!result.tests[0].passed);
        assert!(result.tests[0].failures[0].contains("expected exact match"));
    }

    #[test]
    fn test_stdout_contains() {
        let mut test = make_test("stdout_contains", "echo", vec!["hello world"]);
        test.expect_mut().stdout = Some(OutputMatch::Structured(OutputMatchStructured {
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
        test.expect_mut().stdout = Some(OutputMatch::Structured(OutputMatchStructured {
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
        test.expect_mut().stdout = Some(OutputMatch::Structured(OutputMatchStructured {
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
        test.expect_mut().stdout = Some(OutputMatch::Structured(OutputMatchStructured {
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
        test.expect_mut().stdout = Some(OutputMatch::Structured(OutputMatchStructured {
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
        test.expect_mut().stderr = Some(OutputMatch::Structured(OutputMatchStructured {
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
        test.expect_mut().files = vec![FileExpect {
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
        test.expect_mut().files = vec![FileExpect {
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
        test.expect_mut().files = vec![FileExpect {
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
        test.expect_mut().files = vec![FileExpect {
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
        test.expect_mut().files = vec![FileExpect {
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
        test.expect_mut().stdout = Some(OutputMatch::Exact("test config\n".to_string()));
        let mut spec = make_spec(test);
        spec.setup = vec![SetupStep {
            write_file: Some(WriteFile {
                path: PathBuf::from("config.txt"),
                contents: "test config\n".to_string(),
            }),
            create_dir: None,
            copy_file: None,
            copy_dir: None,
            run: None,
            ..Default::default()
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
        test.expect_mut().exit = Some(0);
        let mut spec = make_spec(test);
        spec.setup = vec![SetupStep {
            write_file: None,
            create_dir: Some(PathBuf::from("subdir")),
            copy_file: None,
            copy_dir: None,
            run: None,
            ..Default::default()
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
        test.expect_mut().stdout = Some(OutputMatch::Exact("per-test config\n".to_string()));
        test.setup = vec![SetupStep {
            write_file: Some(WriteFile {
                path: PathBuf::from("test_config.txt"),
                contents: "per-test config\n".to_string(),
            }),
            create_dir: None,
            copy_file: None,
            copy_dir: None,
            run: None,
            ..Default::default()
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
        test.expect_mut().stdout = Some(OutputMatch::Exact("original content\n".to_string()));
        let mut spec = make_spec(test);
        spec.setup = vec![
            SetupStep {
                write_file: Some(WriteFile {
                    path: PathBuf::from("source.txt"),
                    contents: "original content\n".to_string(),
                }),
                create_dir: None,
                copy_file: None,
                copy_dir: None,
                run: None,
                ..Default::default()
            },
            SetupStep {
                write_file: None,
                create_dir: None,
                copy_file: Some(CopyFile {
                    from: PathBuf::from("source.txt"),
                    to: PathBuf::from("dest.txt"),
                }),
                copy_dir: None,
                run: None,
                ..Default::default()
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
        test.expect_mut().stdout = Some(OutputMatch::Structured(OutputMatchStructured {
            equals: None,
            contains: Some("setup ran".to_string()),
            regex: None,
        }));
        let mut spec = make_spec(test);
        spec.setup = vec![SetupStep {
            write_file: None,
            create_dir: None,
            copy_file: None,
            copy_dir: None,
            run: Some(RunStep {
                cmd: "sh".to_string(),
                args: vec![
                    "-c".to_string(),
                    "echo 'setup ran' > created_by_setup.txt".to_string(),
                ],
            }),
            ..Default::default()
        }];
        let result = run_spec_standalone(&spec);

        assert!(
            result.tests[0].passed,
            "failures: {:?}",
            result.tests[0].failures
        );
    }

    #[test]
    fn test_setup_copy_dir() {
        // Test that copy_dir recursively copies a directory
        let mut test = make_test(
            "read_copied_dir",
            "sh",
            vec!["-c", "cat dest/sub/nested.txt && cat dest/root.txt"],
        );
        test.expect_mut().stdout = Some(OutputMatch::Structured(OutputMatchStructured {
            equals: None,
            contains: Some("nested content".to_string()),
            regex: None,
        }));
        let mut spec = make_spec(test);
        // First create source directory structure, then copy it
        spec.setup = vec![
            SetupStep {
                write_file: None,
                create_dir: Some(PathBuf::from("source/sub")),
                copy_file: None,
                copy_dir: None,
                run: None,
                ..Default::default()
            },
            SetupStep {
                write_file: Some(WriteFile {
                    path: PathBuf::from("source/root.txt"),
                    contents: "root content\n".to_string(),
                }),
                create_dir: None,
                copy_file: None,
                copy_dir: None,
                run: None,
                ..Default::default()
            },
            SetupStep {
                write_file: Some(WriteFile {
                    path: PathBuf::from("source/sub/nested.txt"),
                    contents: "nested content\n".to_string(),
                }),
                create_dir: None,
                copy_file: None,
                copy_dir: None,
                run: None,
                ..Default::default()
            },
            SetupStep {
                write_file: None,
                create_dir: None,
                copy_file: None,
                copy_dir: Some(CopyDir {
                    from: PathBuf::from("source"),
                    to: PathBuf::from("dest"),
                }),
                run: None,
                ..Default::default()
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
    fn test_teardown_removes_file() {
        let mut test = make_test("create_file", "touch", vec!["to_remove.txt"]);
        test.expect_mut().files = vec![FileExpect {
            path: PathBuf::from("to_remove.txt"),
            exists: Some(true),
            contents: None,
        }];
        let mut spec = make_spec(test);
        spec.teardown = vec![TeardownStep {
            remove_dir: None,
            remove_file: Some(PathBuf::from("to_remove.txt")),
            run: None,
            ..Default::default()
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
        test.expect_mut().stdout = Some(OutputMatch::Exact("test_value\n".to_string()));
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
        test.expect_mut().stdout = Some(OutputMatch::Exact("overridden\n".to_string()));
        test.run_mut()
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
        test.expect_mut().stdout = Some(OutputMatch::Exact("empty\n".to_string()));
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
        test.run_mut().stdin = Some("input data".to_string());
        test.expect_mut().stdout = Some(OutputMatch::Exact("input data".to_string()));
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
        test.run_mut().shell = true;
        test.expect_mut().stdout = Some(OutputMatch::Structured(OutputMatchStructured {
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

    // ==================== Signal Tests ====================

    #[test]
    #[cfg(unix)]
    fn test_signal_assertion_passes() {
        // Test that expects SIGKILL (9) - use sh to send kill signal to itself
        let mut test = make_test(
            "signal_test",
            "sh",
            vec!["-c", "kill -9 $$"], // $$ is the shell's PID
        );
        test.expect_mut().signal = Some(9); // SIGKILL
        let spec = make_spec(test);
        let result = run_spec_standalone(&spec);

        assert!(
            result.tests[0].passed,
            "failures: {:?}",
            result.tests[0].failures
        );
    }

    #[test]
    #[cfg(unix)]
    fn test_signal_assertion_wrong_signal() {
        // Test expects SIGTERM (15) but gets SIGKILL (9)
        let mut test = make_test("signal_mismatch", "sh", vec!["-c", "kill -9 $$"]);
        test.expect_mut().signal = Some(15); // SIGTERM
        let spec = make_spec(test);
        let result = run_spec_standalone(&spec);

        assert!(!result.tests[0].passed);
        assert!(result.tests[0].failures[0].contains("Signal: expected 15, got 9"));
    }

    #[test]
    #[cfg(unix)]
    fn test_signal_expected_but_normal_exit() {
        // Test expects a signal but process exits normally
        let mut test = make_test("signal_expected_normal_exit", "true", vec![]);
        test.expect_mut().signal = Some(9);
        let spec = make_spec(test);
        let result = run_spec_standalone(&spec);

        assert!(!result.tests[0].passed);
        assert!(result.tests[0].failures[0].contains("but process exited with exit code 0"));
    }

    #[test]
    #[cfg(unix)]
    fn test_exit_expected_but_signal_received() {
        // Test expects exit code 0 but gets killed by signal
        let mut test = make_test("exit_expected_signal", "sh", vec!["-c", "kill -9 $$"]);
        test.expect_mut().exit = Some(0);
        let spec = make_spec(test);
        let result = run_spec_standalone(&spec);

        assert!(!result.tests[0].passed);
        assert!(result.tests[0].failures[0].contains("terminated by signal 9"));
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
            capture_fs_diff: None,
            databases: HashMap::new(),
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
        // Both must be serial since test2 depends on test1's output
        let mut test1 = make_test("create_file", "sh", vec!["-c", "echo shared > shared.txt"]);
        test1.expect_mut().exit = Some(0);
        test1.serial = true;
        let mut test2 = make_test("read_file", "cat", vec!["shared.txt"]);
        test2.expect_mut().stdout = Some(OutputMatch::Exact("shared\n".to_string()));
        test2.serial = true;
        let spec = TestSpec {
            version: 1,
            sandbox: Sandbox::default(),
            timeout: None,
            capture_fs_diff: None,
            databases: HashMap::new(),
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
        test.expect_mut().stdout = Some(OutputMatch::Structured(OutputMatchStructured {
            equals: None,
            contains: Some("subdir".to_string()),
            regex: None,
        }));
        test.run_mut().cwd = Some(PathBuf::from("subdir"));
        let mut spec = make_spec(test);
        spec.setup = vec![SetupStep {
            write_file: None,
            create_dir: Some(PathBuf::from("subdir")),
            copy_file: None,
            copy_dir: None,
            run: None,
            ..Default::default()
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
            serial: false,
            capture_fs_diff: false,
            sandbox_dir: None,
            databases: HashMap::new(),
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
            serial: false,
            capture_fs_diff: false,
            sandbox_dir: None,
            databases: HashMap::new(),
            setup: vec![],
            teardown: vec![],
        };

        let mut test = make_test("env_test", "sh", vec!["-c", "echo $SUITE_VAR"]);
        test.expect_mut().stdout = Some(OutputMatch::Exact("from_suite\n".to_string()));
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
            serial: false,
            capture_fs_diff: false,
            sandbox_dir: None,
            databases: HashMap::new(),
            setup: vec![],
            teardown: vec![],
        };

        let mut test = make_test("env_override", "sh", vec!["-c", "echo $MY_VAR"]);
        test.expect_mut().stdout = Some(OutputMatch::Exact("from_file\n".to_string()));
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
            serial: false,
            capture_fs_diff: false,
            sandbox_dir: None,
            databases: HashMap::new(),
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

    // ==================== Parallel Execution Tests ====================

    #[test]
    fn test_parallel_tests_run_concurrently() {
        // Two parallel tests that each sleep 0.3s should complete in ~0.3s, not ~0.6s
        let mut test1 = make_test("parallel_1", "sleep", vec!["0.3"]);
        test1.serial = false; // default, but explicit
        let mut test2 = make_test("parallel_2", "sleep", vec!["0.3"]);
        test2.serial = false;

        let spec = TestSpec {
            version: 1,
            sandbox: Sandbox::default(),
            timeout: None,
            capture_fs_diff: None,
            databases: HashMap::new(),
            setup: vec![],
            tests: vec![test1, test2],
            teardown: vec![],
        };

        let start = std::time::Instant::now();
        let result = run_spec_standalone(&spec);
        let elapsed = start.elapsed();

        assert_eq!(result.tests.len(), 2);
        assert!(result.tests[0].passed);
        assert!(result.tests[1].passed);
        // Should complete in less than 0.5s if parallel (0.3s + overhead)
        // Would be 0.6s+ if sequential
        assert!(
            elapsed.as_secs_f64() < 0.5,
            "Parallel tests took too long: {:.2}s (expected < 0.5s)",
            elapsed.as_secs_f64()
        );
    }

    #[test]
    fn test_serial_tests_run_sequentially() {
        // Two serial tests that each sleep 0.2s should complete in ~0.4s
        let mut test1 = make_test("serial_1", "sleep", vec!["0.2"]);
        test1.serial = true;
        let mut test2 = make_test("serial_2", "sleep", vec!["0.2"]);
        test2.serial = true;

        let spec = TestSpec {
            version: 1,
            sandbox: Sandbox::default(),
            timeout: None,
            capture_fs_diff: None,
            databases: HashMap::new(),
            setup: vec![],
            tests: vec![test1, test2],
            teardown: vec![],
        };

        let start = std::time::Instant::now();
        let result = run_spec_standalone(&spec);
        let elapsed = start.elapsed();

        assert_eq!(result.tests.len(), 2);
        assert!(result.tests[0].passed);
        assert!(result.tests[1].passed);
        // Should take at least 0.4s if sequential
        assert!(
            elapsed.as_secs_f64() >= 0.35,
            "Serial tests completed too fast: {:.2}s (expected >= 0.35s)",
            elapsed.as_secs_f64()
        );
    }

    #[test]
    fn test_serial_tests_run_before_parallel() {
        // Serial test creates a file, parallel test reads it
        // This verifies serial tests complete before parallel tests start
        let mut serial_test = make_test(
            "serial_create",
            "sh",
            vec!["-c", "echo created > marker.txt"],
        );
        serial_test.serial = true;

        let mut parallel_test = make_test("parallel_read", "cat", vec!["marker.txt"]);
        parallel_test.serial = false;
        parallel_test.expect_mut().stdout = Some(OutputMatch::Structured(OutputMatchStructured {
            equals: None,
            contains: Some("created".to_string()),
            regex: None,
        }));

        let spec = TestSpec {
            version: 1,
            sandbox: Sandbox::default(),
            timeout: None,
            capture_fs_diff: None,
            databases: HashMap::new(),
            setup: vec![],
            tests: vec![serial_test, parallel_test],
            teardown: vec![],
        };

        let result = run_spec_standalone(&spec);

        assert!(
            result.tests[0].passed,
            "serial test failed: {:?}",
            result.tests[0].failures
        );
        assert!(
            result.tests[1].passed,
            "parallel test failed: {:?}",
            result.tests[1].failures
        );
    }

    #[test]
    fn test_results_maintain_declaration_order() {
        // Tests should be reported in declaration order regardless of execution order
        let mut test1 = make_test("first", "echo", vec!["1"]);
        test1.serial = false;
        let mut test2 = make_test("second", "echo", vec!["2"]);
        test2.serial = true; // Runs first due to serial
        let mut test3 = make_test("third", "echo", vec!["3"]);
        test3.serial = false;

        let spec = TestSpec {
            version: 1,
            sandbox: Sandbox::default(),
            timeout: None,
            capture_fs_diff: None,
            databases: HashMap::new(),
            setup: vec![],
            tests: vec![test1, test2, test3],
            teardown: vec![],
        };

        let result = run_spec_standalone(&spec);

        assert_eq!(result.tests.len(), 3);
        assert_eq!(result.tests[0].name, "first");
        assert_eq!(result.tests[1].name, "second");
        assert_eq!(result.tests[2].name, "third");
    }

    #[test]
    fn test_mixed_serial_parallel_execution() {
        // Mix of serial and parallel tests
        let mut s1 = make_test("serial_1", "echo", vec!["s1"]);
        s1.serial = true;
        let mut p1 = make_test("parallel_1", "echo", vec!["p1"]);
        p1.serial = false;
        let mut s2 = make_test("serial_2", "echo", vec!["s2"]);
        s2.serial = true;
        let mut p2 = make_test("parallel_2", "echo", vec!["p2"]);
        p2.serial = false;

        let spec = TestSpec {
            version: 1,
            sandbox: Sandbox::default(),
            timeout: None,
            capture_fs_diff: None,
            databases: HashMap::new(),
            setup: vec![],
            tests: vec![s1, p1, s2, p2],
            teardown: vec![],
        };

        let result = run_spec_standalone(&spec);

        assert_eq!(result.tests.len(), 4);
        assert!(result.tests.iter().all(|t| t.passed));
        // Results maintain declaration order
        assert_eq!(result.tests[0].name, "serial_1");
        assert_eq!(result.tests[1].name, "parallel_1");
        assert_eq!(result.tests[2].name, "serial_2");
        assert_eq!(result.tests[3].name, "parallel_2");
    }

    // ==================== Filesystem Diff Tests ====================

    #[test]
    fn test_fs_diff_captures_added_files() {
        let mut test = make_test("create_files", "sh", vec!["-c", "touch a.txt b.txt"]);
        test.capture_fs_diff = Some(true);
        let spec = make_spec(test);
        let result = run_spec_standalone(&spec);

        assert!(result.tests[0].passed);
        let diff = result.tests[0]
            .fs_diff
            .as_ref()
            .expect("fs_diff should be captured");
        assert!(
            diff.added
                .iter()
                .any(|p| p.to_string_lossy().contains("a.txt"))
        );
        assert!(
            diff.added
                .iter()
                .any(|p| p.to_string_lossy().contains("b.txt"))
        );
    }

    #[test]
    fn test_fs_diff_captures_modified_files() {
        let mut test = make_test(
            "modify_file",
            "sh",
            vec!["-c", "echo modified >> existing.txt"],
        );
        test.capture_fs_diff = Some(true);
        test.setup = vec![SetupStep {
            write_file: Some(WriteFile {
                path: PathBuf::from("existing.txt"),
                contents: "initial\n".to_string(),
            }),
            create_dir: None,
            copy_file: None,
            copy_dir: None,
            run: None,
            ..Default::default()
        }];
        let spec = make_spec(test);
        let result = run_spec_standalone(&spec);

        assert!(result.tests[0].passed);
        let diff = result.tests[0]
            .fs_diff
            .as_ref()
            .expect("fs_diff should be captured");
        assert!(
            diff.modified
                .iter()
                .any(|p| p.to_string_lossy().contains("existing.txt"))
        );
    }

    #[test]
    fn test_fs_diff_captures_removed_files() {
        let mut test = make_test("remove_file", "rm", vec!["to_delete.txt"]);
        test.capture_fs_diff = Some(true);
        test.setup = vec![SetupStep {
            write_file: Some(WriteFile {
                path: PathBuf::from("to_delete.txt"),
                contents: "delete me\n".to_string(),
            }),
            create_dir: None,
            copy_file: None,
            copy_dir: None,
            run: None,
            ..Default::default()
        }];
        let spec = make_spec(test);
        let result = run_spec_standalone(&spec);

        assert!(result.tests[0].passed);
        let diff = result.tests[0]
            .fs_diff
            .as_ref()
            .expect("fs_diff should be captured");
        assert!(
            diff.removed
                .iter()
                .any(|p| p.to_string_lossy().contains("to_delete.txt"))
        );
    }

    #[test]
    fn test_fs_diff_disabled_by_default() {
        let test = make_test("no_diff", "touch", vec!["file.txt"]);
        let spec = make_spec(test);
        let result = run_spec_standalone(&spec);

        assert!(result.tests[0].passed);
        assert!(result.tests[0].fs_diff.is_none());
    }

    #[test]
    fn test_fs_diff_enabled_at_file_level() {
        let test = make_test("file_level_diff", "touch", vec!["file.txt"]);
        let mut spec = make_spec(test);
        spec.capture_fs_diff = Some(true);
        let result = run_spec_standalone(&spec);

        assert!(result.tests[0].passed);
        assert!(result.tests[0].fs_diff.is_some());
    }

    // ==================== Tree Expectation Tests ====================

    #[test]
    fn test_tree_contains_passes() {
        use crate::schema::{TreeEntry, TreeExpect};

        let mut test = make_test(
            "create_tree",
            "sh",
            vec!["-c", "mkdir -p src && touch src/main.rs Cargo.toml"],
        );
        test.expect_mut().tree = Some(TreeExpect {
            root: None,
            contains: vec![
                TreeEntry {
                    path: PathBuf::from("src/main.rs"),
                    contents: None,
                },
                TreeEntry {
                    path: PathBuf::from("Cargo.toml"),
                    contents: None,
                },
            ],
            excludes: vec![],
            exact: false,
        });
        let spec = make_spec(test);
        let result = run_spec_standalone(&spec);

        assert!(
            result.tests[0].passed,
            "failures: {:?}",
            result.tests[0].failures
        );
    }

    #[test]
    fn test_tree_contains_fails_on_missing() {
        use crate::schema::{TreeEntry, TreeExpect};

        let mut test = make_test("empty_tree", "true", vec![]);
        test.expect_mut().tree = Some(TreeExpect {
            root: None,
            contains: vec![TreeEntry {
                path: PathBuf::from("missing.txt"),
                contents: None,
            }],
            excludes: vec![],
            exact: false,
        });
        let spec = make_spec(test);
        let result = run_spec_standalone(&spec);

        assert!(!result.tests[0].passed);
        assert!(
            result.tests[0]
                .failures
                .iter()
                .any(|f| f.contains("expected path to exist"))
        );
    }

    #[test]
    fn test_tree_excludes_passes() {
        use crate::schema::TreeExpect;

        let mut test = make_test("no_target", "true", vec![]);
        test.expect_mut().tree = Some(TreeExpect {
            root: None,
            contains: vec![],
            excludes: vec![PathBuf::from("target/")],
            exact: false,
        });
        let spec = make_spec(test);
        let result = run_spec_standalone(&spec);

        assert!(
            result.tests[0].passed,
            "failures: {:?}",
            result.tests[0].failures
        );
    }

    #[test]
    fn test_tree_excludes_fails_on_present() {
        use crate::schema::TreeExpect;

        let mut test = make_test("creates_forbidden", "mkdir", vec!["forbidden"]);
        test.expect_mut().tree = Some(TreeExpect {
            root: None,
            contains: vec![],
            excludes: vec![PathBuf::from("forbidden")],
            exact: false,
        });
        let spec = make_spec(test);
        let result = run_spec_standalone(&spec);

        assert!(!result.tests[0].passed);
        assert!(
            result.tests[0]
                .failures
                .iter()
                .any(|f| f.contains("expected path to not exist"))
        );
    }

    #[test]
    fn test_tree_with_contents_check() {
        use crate::schema::{TreeEntry, TreeExpect};

        let mut test = make_test(
            "create_with_content",
            "sh",
            vec!["-c", "echo 'hello world' > greeting.txt"],
        );
        test.expect_mut().tree = Some(TreeExpect {
            root: None,
            contains: vec![TreeEntry {
                path: PathBuf::from("greeting.txt"),
                contents: Some(OutputMatch::Structured(OutputMatchStructured {
                    equals: None,
                    contains: Some("hello".to_string()),
                    regex: None,
                })),
            }],
            excludes: vec![],
            exact: false,
        });
        let spec = make_spec(test);
        let result = run_spec_standalone(&spec);

        assert!(
            result.tests[0].passed,
            "failures: {:?}",
            result.tests[0].failures
        );
    }

    #[test]
    fn test_sandbox_dir_local_creates_bintest_directory() {
        // Create a temp directory to use as working directory
        let temp_dir = tempfile::tempdir().unwrap();
        let original_dir = std::env::current_dir().unwrap();
        std::env::set_current_dir(temp_dir.path()).unwrap();

        // Create a suite config with sandbox_dir: local
        let suite_config = SuiteConfig {
            version: 1,
            timeout: None,
            env: HashMap::new(),
            inherit_env: None,
            serial: false,
            capture_fs_diff: false,
            sandbox_dir: Some(SandboxDir::Local),
            databases: HashMap::new(),
            setup: vec![],
            teardown: vec![],
        };

        // Run a simple test
        let mut test = make_test("sandbox_test", "sh", vec!["-c", "pwd"]);
        test.expect_mut().exit = Some(0);
        let spec = make_spec(test);

        let result = run_spec(&spec, Some(&suite_config));

        // Verify test passed
        assert!(
            result.tests[0].passed,
            "failures: {:?}",
            result.tests[0].failures
        );

        // Verify .bintest directory was created
        let bintest_dir = temp_dir.path().join(".bintest");
        assert!(
            bintest_dir.exists(),
            ".bintest directory should exist at {:?}",
            bintest_dir
        );

        // Verify there's a timestamp subdirectory
        let entries: Vec<_> = std::fs::read_dir(&bintest_dir)
            .unwrap()
            .filter_map(|e| e.ok())
            .collect();
        assert!(
            !entries.is_empty(),
            ".bintest directory should contain timestamp subdirectory"
        );

        // Restore original directory
        std::env::set_current_dir(original_dir).unwrap();
    }
}
