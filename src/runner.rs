//! Test execution engine.
//!
//! Runs test specs in isolated sandboxes and captures results.

use crate::schema::{
    Expect, FileExpect, OutputMatch, OutputMatchStructured, Run, RunStep, Sandbox, SetupStep,
    TeardownStep, Test, TestSpec, WorkDir,
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

/// Run a test specification file.
pub fn run_spec(spec: &TestSpec) -> SpecResult {
    let ctx = match ExecutionContext::new(&spec.sandbox) {
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
        let result = run_test(test, &ctx);
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

fn run_test(test: &Test, ctx: &ExecutionContext) -> TestResult {
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

    // Run the command
    let timeout = Duration::from_secs(test.timeout.unwrap_or(DEFAULT_TIMEOUT_SECS));
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
