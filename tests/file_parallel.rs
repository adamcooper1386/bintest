//! Integration tests for file-level parallel execution.

use std::fs;
use std::process::Command;
use std::time::Instant;
use tempfile::TempDir;

fn bintest_cmd() -> Command {
    Command::new(env!("CARGO_BIN_EXE_bintest"))
}

/// Create a spec file that sleeps for a given duration.
fn sleep_spec(name: &str, duration_secs: f64) -> String {
    format!(
        r#"version: 1
tests:
  - name: {name}
    run:
      cmd: sleep
      args: ["{duration_secs}"]
    expect:
      exit: 0
"#
    )
}

#[test]
fn test_files_run_in_parallel_by_default() {
    let temp_dir = TempDir::new().unwrap();

    // Create two spec files that each sleep for 0.3 seconds
    let spec1_path = temp_dir.path().join("spec1.yaml");
    let spec2_path = temp_dir.path().join("spec2.yaml");

    fs::write(&spec1_path, sleep_spec("sleep1", 0.3)).unwrap();
    fs::write(&spec2_path, sleep_spec("sleep2", 0.3)).unwrap();

    let start = Instant::now();
    let output = bintest_cmd()
        .arg("run")
        .arg(temp_dir.path())
        .output()
        .unwrap();
    let elapsed = start.elapsed();

    assert!(
        output.status.success(),
        "stdout: {}\nstderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );

    // If parallel, should complete in ~0.3s (+ overhead)
    // If serial, would take ~0.6s
    // Use generous threshold to avoid flaky tests under system load
    assert!(
        elapsed.as_secs_f64() < 0.8,
        "Files took {:.2}s to run (expected < 0.8s for parallel execution)",
        elapsed.as_secs_f64()
    );
}

#[test]
fn test_files_run_serially_when_configured() {
    let temp_dir = TempDir::new().unwrap();

    // Create suite config with serial: true
    let suite_config = r#"version: 1
serial: true
"#;
    fs::write(temp_dir.path().join("bintest.yaml"), suite_config).unwrap();

    // Create two spec files that each sleep for 0.2 seconds
    let spec1_path = temp_dir.path().join("spec1.yaml");
    let spec2_path = temp_dir.path().join("spec2.yaml");

    fs::write(&spec1_path, sleep_spec("sleep1", 0.2)).unwrap();
    fs::write(&spec2_path, sleep_spec("sleep2", 0.2)).unwrap();

    let start = Instant::now();
    let output = bintest_cmd()
        .arg("run")
        .arg(temp_dir.path())
        .output()
        .unwrap();
    let elapsed = start.elapsed();

    assert!(
        output.status.success(),
        "stdout: {}\nstderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );

    // If serial, should take at least 0.4s (0.2 + 0.2)
    assert!(
        elapsed.as_secs_f64() >= 0.35,
        "Files took {:.2}s to run (expected >= 0.35s for serial execution)",
        elapsed.as_secs_f64()
    );
}

#[test]
fn test_parallel_files_results_maintain_order() {
    let temp_dir = TempDir::new().unwrap();

    // Create spec files with predictable names for ordering
    let spec_a = temp_dir.path().join("a_first.yaml");
    let spec_b = temp_dir.path().join("b_second.yaml");
    let spec_c = temp_dir.path().join("c_third.yaml");

    fs::write(
        &spec_a,
        r#"version: 1
tests:
  - name: test_a
    run:
      cmd: echo
      args: ["a"]
    expect:
      exit: 0
"#,
    )
    .unwrap();

    fs::write(
        &spec_b,
        r#"version: 1
tests:
  - name: test_b
    run:
      cmd: echo
      args: ["b"]
    expect:
      exit: 0
"#,
    )
    .unwrap();

    fs::write(
        &spec_c,
        r#"version: 1
tests:
  - name: test_c
    run:
      cmd: echo
      args: ["c"]
    expect:
      exit: 0
"#,
    )
    .unwrap();

    let output = bintest_cmd()
        .arg("run")
        .arg(temp_dir.path())
        .output()
        .unwrap();

    assert!(output.status.success());

    let stdout = String::from_utf8_lossy(&output.stdout);

    // Results should be in alphabetical order (how find_specs returns them)
    let pos_a = stdout.find("test_a").expect("test_a not found");
    let pos_b = stdout.find("test_b").expect("test_b not found");
    let pos_c = stdout.find("test_c").expect("test_c not found");

    assert!(
        pos_a < pos_b && pos_b < pos_c,
        "Results not in expected order:\n{}",
        stdout
    );
}

#[test]
fn test_many_files_parallel_speedup() {
    let temp_dir = TempDir::new().unwrap();

    // Create 5 spec files that each sleep for 0.2 seconds
    for i in 0..5 {
        let spec_path = temp_dir.path().join(format!("spec{i}.yaml"));
        fs::write(&spec_path, sleep_spec(&format!("sleep{i}"), 0.2)).unwrap();
    }

    let start = Instant::now();
    let output = bintest_cmd()
        .arg("run")
        .arg(temp_dir.path())
        .output()
        .unwrap();
    let elapsed = start.elapsed();

    assert!(
        output.status.success(),
        "stdout: {}\nstderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );

    // If parallel, should complete in ~0.2s (+ overhead)
    // If serial, would take ~1.0s (5 * 0.2)
    assert!(
        elapsed.as_secs_f64() < 0.6,
        "5 files took {:.2}s to run (expected < 0.6s for parallel execution)",
        elapsed.as_secs_f64()
    );
}

#[test]
fn test_serial_files_preserve_side_effects() {
    let temp_dir = TempDir::new().unwrap();

    // Create suite config with serial: true
    let suite_config = r#"version: 1
serial: true
"#;
    fs::write(temp_dir.path().join("bintest.yaml"), suite_config).unwrap();

    // First spec creates a file
    let spec1 = temp_dir.path().join("a_create.yaml");
    fs::write(
        &spec1,
        format!(
            r#"version: 1
tests:
  - name: create_file
    run:
      cmd: sh
      args: ["-c", "echo created > {}/marker.txt"]
    expect:
      exit: 0
"#,
            temp_dir.path().display()
        ),
    )
    .unwrap();

    // Second spec reads the file (depends on first)
    let spec2 = temp_dir.path().join("b_read.yaml");
    fs::write(
        &spec2,
        format!(
            r#"version: 1
tests:
  - name: read_file
    run:
      cmd: cat
      args: ["{}/marker.txt"]
    expect:
      exit: 0
      stdout:
        contains: "created"
"#,
            temp_dir.path().display()
        ),
    )
    .unwrap();

    let output = bintest_cmd()
        .arg("run")
        .arg(temp_dir.path())
        .output()
        .unwrap();

    assert!(
        output.status.success(),
        "stdout: {}\nstderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}
