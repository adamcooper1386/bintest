//! Test execution engine.
//!
//! Runs test specs in isolated sandboxes and captures results.

use crate::schema::{
    Expect, FileExpect, OutputMatch, OutputMatchStructured, Run, RunStep, Sandbox, SetupStep,
    SuiteConfig, TeardownStep, Test, TestSpec, TreeExpect, WorkDir,
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
    #[serde(serialize_with = "serialize_duration")]
    pub duration: Duration,
    pub failures: Vec<String>,
    /// Filesystem changes during test execution (if capture_fs_diff enabled).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fs_diff: Option<FilesystemDiff>,
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
    /// Whether to capture filesystem diffs (suite-level default).
    pub capture_fs_diff: bool,
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

    let ctx = match ExecutionContext::new(&merged_sandbox) {
        Ok(ctx) => ctx,
        Err(e) => {
            return SpecResult {
                tests: vec![TestResult {
                    name: "<setup>".to_string(),
                    passed: false,
                    duration: Duration::ZERO,
                    failures: vec![format!("Failed to create sandbox: {e}")],
                    fs_diff: None,
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
                fs_diff: None,
            }],
        };
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
        let result = run_test(test, &ctx, file_timeout, file_capture_fs_diff);
        indexed_results.push((idx, result));
    }

    // Run parallel tests concurrently
    if !parallel_tests.is_empty() {
        let ctx_ref = &ctx;
        thread::scope(|s| {
            let handles: Vec<_> = parallel_tests
                .iter()
                .map(|(idx, test)| {
                    let idx = *idx;
                    s.spawn(move || {
                        (
                            idx,
                            run_test(test, ctx_ref, file_timeout, file_capture_fs_diff),
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
    if let Err(e) = run_teardown_steps(&spec.teardown, &ctx) {
        results.push(TestResult {
            name: "<teardown>".to_string(),
            passed: false,
            duration: Duration::ZERO,
            failures: vec![format!("Teardown failed: {e}")],
            fs_diff: None,
        });
    }

    SpecResult { tests: results }
}

fn run_test(
    test: &Test,
    ctx: &ExecutionContext,
    file_timeout: Option<u64>,
    file_capture_fs_diff: bool,
) -> TestResult {
    let start = Instant::now();
    let mut failures = Vec::new();

    // Determine if we should capture fs diff (test overrides file)
    let capture_fs_diff = test.capture_fs_diff.unwrap_or(file_capture_fs_diff);

    // Test-level setup
    if let Err(e) = run_setup_steps(&test.setup, ctx) {
        return TestResult {
            name: test.name.clone(),
            passed: false,
            duration: start.elapsed(),
            failures: vec![format!("Test setup failed: {e}")],
            fs_diff: None,
        };
    }

    // Capture filesystem state before command (if enabled)
    let snapshot_before = if capture_fs_diff {
        Some(snapshot_filesystem(&ctx.sandbox_dir))
    } else {
        None
    };

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

    // Compute filesystem diff (if enabled)
    let fs_diff = snapshot_before.map(|before| {
        let after = snapshot_filesystem(&ctx.sandbox_dir);
        compute_fs_diff(&before, &after)
    });

    // Test-level teardown (always runs)
    if let Err(e) = run_teardown_steps(&test.teardown, ctx) {
        failures.push(format!("Test teardown failed: {e}"));
    }

    TestResult {
        name: test.name.clone(),
        passed: failures.is_empty(),
        duration: start.elapsed(),
        failures,
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
            capture_fs_diff: None,
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
            capture_fs_diff: None,
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
        test.expect.signal = Some(9); // SIGKILL
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
        test.expect.signal = Some(15); // SIGTERM
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
        test.expect.signal = Some(9);
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
        test.expect.exit = Some(0);
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
        test1.expect.exit = Some(0);
        test1.serial = true;
        let mut test2 = make_test("read_file", "cat", vec!["shared.txt"]);
        test2.expect.stdout = Some(OutputMatch::Exact("shared\n".to_string()));
        test2.serial = true;
        let spec = TestSpec {
            version: 1,
            sandbox: Sandbox::default(),
            timeout: None,
            capture_fs_diff: None,
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
            serial: false,
            capture_fs_diff: false,
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
            serial: false,
            capture_fs_diff: false,
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
            serial: false,
            capture_fs_diff: false,
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
        parallel_test.expect.stdout = Some(OutputMatch::Structured(OutputMatchStructured {
            equals: None,
            contains: Some("created".to_string()),
            regex: None,
        }));

        let spec = TestSpec {
            version: 1,
            sandbox: Sandbox::default(),
            timeout: None,
            capture_fs_diff: None,
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
            run: None,
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
            run: None,
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
        test.expect.tree = Some(TreeExpect {
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
        test.expect.tree = Some(TreeExpect {
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
        test.expect.tree = Some(TreeExpect {
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
        test.expect.tree = Some(TreeExpect {
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
        test.expect.tree = Some(TreeExpect {
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
}
