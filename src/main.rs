mod loader;
mod runner;
mod schema;

use clap::{Parser, Subcommand, ValueEnum};
use std::fmt::Write as _;
use std::fs;
use std::path::PathBuf;
use std::thread;
use std::time::Duration;

#[derive(Clone, Copy, Default, ValueEnum)]
enum OutputFormat {
    /// Human-readable output with checkmarks
    #[default]
    Human,
    /// Machine-readable JSON output
    Json,
    /// JUnit XML output for CI systems
    Junit,
}

#[derive(Parser)]
#[command(name = "bintest")]
#[command(about = "A declarative integration test runner for executables")]
#[command(version)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Execute test specs
    Run {
        /// Path to test specs (file or directory)
        path: PathBuf,
        /// Output format
        #[arg(short, long, default_value = "human")]
        output: OutputFormat,
        /// Filter tests by name pattern (substring match)
        #[arg(short, long)]
        filter: Option<String>,
        /// Show verbose output (command details, full diffs)
        #[arg(short, long)]
        verbose: bool,
        /// Directory for test sandboxes (overrides suite config).
        /// Use "local" for .bintest/<timestamp>/, or specify a path.
        #[arg(long)]
        sandbox_dir: Option<String>,
    },
    /// Validate test specs without running them
    Validate {
        /// Path to test specs (file or directory)
        path: PathBuf,
    },
    /// Scaffold a new spec file
    Init {
        /// Output path for the new spec file
        #[arg(default_value = "tests/example.yaml")]
        path: PathBuf,
    },
    /// Output the spec schema (for AI consumers)
    Schema,
}

fn main() {
    let cli = Cli::parse();

    match cli.command {
        Command::Run {
            path,
            output,
            filter,
            verbose,
            sandbox_dir,
        } => {
            // Show filter info in verbose mode
            if verbose && let Some(ref f) = filter {
                eprintln!("Filtering tests by: {f:?}");
            }

            // Determine the test root directory for suite config
            let test_root = if path.is_file() {
                path.parent().unwrap_or(&path)
            } else {
                &path
            };

            // Load suite config if present
            let mut suite_config = match loader::load_suite_config(test_root) {
                Ok(config) => config,
                Err(e) => {
                    eprintln!("Error loading suite config: {e}");
                    std::process::exit(1);
                }
            };

            // CLI sandbox_dir overrides suite config
            if let Some(ref dir) = sandbox_dir {
                suite_config = Some(suite_config.unwrap_or_default());
                if let Some(ref mut config) = suite_config {
                    config.sandbox_dir = Some(if dir == "local" {
                        schema::SandboxDir::Local
                    } else {
                        schema::SandboxDir::Path(PathBuf::from(dir))
                    });
                }
            }

            let spec_paths = match loader::find_specs(&path) {
                Ok(s) => s,
                Err(e) => {
                    eprintln!("Error finding specs: {e}");
                    std::process::exit(1);
                }
            };

            if spec_paths.is_empty() {
                eprintln!("No spec files found at: {}", path.display());
                std::process::exit(1);
            }

            // Run suite-level setup if configured
            if let Some(ref config) = suite_config
                && let Err(e) = runner::run_suite_setup(config)
            {
                eprintln!("Suite setup failed: {e}");
                std::process::exit(1);
            }

            // Determine if we should run files serially
            let run_serial = suite_config.as_ref().is_some_and(|c| c.serial);

            // Load all specs first, tracking any load failures
            let specs_with_paths: Vec<_> = spec_paths
                .iter()
                .map(|p| (p.clone(), loader::load_spec(p)))
                .collect();

            // Track total execution time
            let run_start = std::time::Instant::now();

            // Run specs (parallel by default, serial if configured)
            let filter_ref = filter.as_deref();
            let file_results: Vec<(PathBuf, Result<runner::SpecResult, String>)> = if run_serial {
                // Serial execution
                specs_with_paths
                    .into_iter()
                    .map(|(path, spec_result)| {
                        let result = match spec_result {
                            Ok(spec) => Ok(runner::run_spec_filtered(
                                &spec,
                                suite_config.as_ref(),
                                filter_ref,
                            )),
                            Err(e) => Err(e.to_string()),
                        };
                        (path, result)
                    })
                    .collect()
            } else {
                // Parallel execution (default)
                thread::scope(|s| {
                    let handles: Vec<_> = specs_with_paths
                        .into_iter()
                        .map(|(path, spec_result)| {
                            let suite_config_ref = suite_config.as_ref();
                            s.spawn(move || {
                                let result = match spec_result {
                                    Ok(spec) => Ok(runner::run_spec_filtered(
                                        &spec,
                                        suite_config_ref,
                                        filter_ref,
                                    )),
                                    Err(e) => Err(e.to_string()),
                                };
                                (path, result)
                            })
                        })
                        .collect();

                    handles
                        .into_iter()
                        .map(|h| h.join().expect("Spec thread panicked"))
                        .collect()
                })
            };

            // Sort results by original path order for deterministic output
            let mut sorted_results: Vec<_> = file_results;
            sorted_results.sort_by(|a, b| {
                spec_paths
                    .iter()
                    .position(|p| p == &a.0)
                    .cmp(&spec_paths.iter().position(|p| p == &b.0))
            });

            let mut json_results = Vec::new();
            let mut junit_results = Vec::new();
            let mut total_passed = 0;
            let mut total_failed = 0;

            for (spec_path, result) in sorted_results {
                match result {
                    Err(e) => {
                        if matches!(output, OutputFormat::Human) {
                            eprintln!("✗ Failed to load {}: {e}", spec_path.display());
                        }
                        // For JUnit, create a synthetic failed test for load errors
                        if matches!(output, OutputFormat::Junit) {
                            junit_results.push(JunitFileResult {
                                file: spec_path.display().to_string(),
                                tests: vec![runner::TestResult {
                                    name: "<load>".to_string(),
                                    passed: false,
                                    duration: Duration::ZERO,
                                    failures: vec![format!("Failed to load spec: {e}")],
                                    fs_diff: None,
                                }],
                                total_time: Duration::ZERO,
                            });
                        }
                        total_failed += 1;
                    }
                    Ok(spec_result) => {
                        let file_time: Duration =
                            spec_result.tests.iter().map(|t| t.duration).sum();

                        for test in &spec_result.tests {
                            if test.passed {
                                total_passed += 1;
                            } else {
                                total_failed += 1;
                            }
                        }

                        match output {
                            OutputFormat::Human => {
                                println!("\n{}", spec_path.display());
                                for test in &spec_result.tests {
                                    if test.passed {
                                        println!("  ✓ {} ({:.2?})", test.name, test.duration);
                                    } else {
                                        println!("  ✗ {} ({:.2?})", test.name, test.duration);
                                        for failure in &test.failures {
                                            println!("    {failure}");
                                        }
                                    }
                                    // Show filesystem diff if captured
                                    if let Some(ref diff) = test.fs_diff {
                                        if verbose {
                                            // Verbose mode: show full file paths
                                            if !diff.added.is_empty() {
                                                println!("    fs added:");
                                                for path in &diff.added {
                                                    println!("      + {}", path.display());
                                                }
                                            }
                                            if !diff.removed.is_empty() {
                                                println!("    fs removed:");
                                                for path in &diff.removed {
                                                    println!("      - {}", path.display());
                                                }
                                            }
                                            if !diff.modified.is_empty() {
                                                println!("    fs modified:");
                                                for path in &diff.modified {
                                                    println!("      ~ {}", path.display());
                                                }
                                            }
                                        } else {
                                            // Normal mode: show summary
                                            let mut diff_parts = Vec::new();
                                            if !diff.added.is_empty() {
                                                diff_parts
                                                    .push(format!("+{} added", diff.added.len()));
                                            }
                                            if !diff.removed.is_empty() {
                                                diff_parts.push(format!(
                                                    "-{} removed",
                                                    diff.removed.len()
                                                ));
                                            }
                                            if !diff.modified.is_empty() {
                                                diff_parts.push(format!(
                                                    "~{} modified",
                                                    diff.modified.len()
                                                ));
                                            }
                                            if !diff_parts.is_empty() {
                                                println!("    fs: {}", diff_parts.join(", "));
                                            }
                                        }
                                    }
                                }
                            }
                            OutputFormat::Json => {
                                json_results.push(serde_json::json!({
                                    "file": spec_path.display().to_string(),
                                    "tests": spec_result.tests,
                                }));
                            }
                            OutputFormat::Junit => {
                                junit_results.push(JunitFileResult {
                                    file: spec_path.display().to_string(),
                                    tests: spec_result.tests,
                                    total_time: file_time,
                                });
                            }
                        }
                    }
                }
            }

            // Run suite-level teardown if configured (always runs)
            if let Some(ref config) = suite_config
                && let Err(e) = runner::run_suite_teardown(config)
            {
                if matches!(output, OutputFormat::Human) {
                    eprintln!("Suite teardown failed: {e}");
                }
                total_failed += 1;
            }

            let total_time = run_start.elapsed();

            match output {
                OutputFormat::Human => {
                    println!("\n{total_passed} passed, {total_failed} failed");
                }
                OutputFormat::Json => {
                    let output = serde_json::json!({
                        "passed": total_passed,
                        "failed": total_failed,
                        "results": json_results,
                    });
                    println!(
                        "{}",
                        serde_json::to_string_pretty(&output).expect("Failed to serialize")
                    );
                }
                OutputFormat::Junit => {
                    print!("{}", format_junit_xml(&junit_results, total_time));
                }
            }

            if total_failed > 0 {
                std::process::exit(1);
            }
        }
        Command::Validate { path } => {
            let specs = match loader::find_specs(&path) {
                Ok(s) => s,
                Err(e) => {
                    eprintln!("Error finding specs: {e}");
                    std::process::exit(1);
                }
            };

            if specs.is_empty() {
                eprintln!("No spec files found at: {}", path.display());
                std::process::exit(1);
            }

            let mut errors = 0;
            for spec_path in &specs {
                match loader::load_spec(spec_path) {
                    Ok(spec) => {
                        println!("✓ {} ({} tests)", spec_path.display(), spec.tests.len());
                    }
                    Err(e) => {
                        eprintln!("✗ {}: {e}", spec_path.display());
                        errors += 1;
                    }
                }
            }

            if errors > 0 {
                eprintln!("\n{errors} spec(s) failed validation");
                std::process::exit(1);
            }
            println!("\nAll {} spec(s) valid", specs.len());
        }
        Command::Init { path } => {
            let template = r#"version: 1

sandbox:
  workdir: temp
  env:
    # Add environment variables here
    # RUST_LOG: debug

# setup:
#   - write_file:
#       path: config.toml
#       contents: |
#         key = "value"

tests:
  - name: example_test
    run:
      cmd: echo
      args: ["hello", "world"]
    expect:
      exit: 0
      stdout:
        contains: "hello"

# teardown:
#   - remove_dir: sandbox
"#;
            if path.exists() {
                eprintln!("Error: file already exists: {}", path.display());
                std::process::exit(1);
            }
            if let Some(parent) = path.parent()
                && !parent.as_os_str().is_empty()
                && !parent.exists()
                && let Err(e) = fs::create_dir_all(parent)
            {
                eprintln!("Error creating directory: {e}");
                std::process::exit(1);
            }
            if let Err(e) = fs::write(&path, template) {
                eprintln!("Error writing file: {e}");
                std::process::exit(1);
            }
            println!("Created: {}", path.display());
        }
        Command::Schema => {
            let schema = schema::generate_schema();
            let json = serde_json::to_string_pretty(&schema).expect("Failed to serialize schema");
            println!("{json}");
        }
    }
}

/// A file result for JUnit output.
struct JunitFileResult {
    file: String,
    tests: Vec<runner::TestResult>,
    total_time: Duration,
}

/// Format test results as JUnit XML.
fn format_junit_xml(results: &[JunitFileResult], total_time: Duration) -> String {
    let mut xml = String::new();
    xml.push_str("<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n");

    let total_tests: usize = results.iter().map(|r| r.tests.len()).sum();
    let total_failures: usize = results
        .iter()
        .flat_map(|r| &r.tests)
        .filter(|t| !t.passed)
        .count();

    let _ = writeln!(
        xml,
        "<testsuites tests=\"{total_tests}\" failures=\"{total_failures}\" time=\"{:.3}\">",
        total_time.as_secs_f64()
    );

    for file_result in results {
        let tests = file_result.tests.len();
        let failures = file_result.tests.iter().filter(|t| !t.passed).count();

        let _ = writeln!(
            xml,
            "  <testsuite name=\"{}\" tests=\"{tests}\" failures=\"{failures}\" time=\"{:.3}\">",
            escape_xml(&file_result.file),
            file_result.total_time.as_secs_f64()
        );

        for test in &file_result.tests {
            let _ = writeln!(
                xml,
                "    <testcase name=\"{}\" time=\"{:.3}\">",
                escape_xml(&test.name),
                test.duration.as_secs_f64()
            );

            if !test.passed {
                let message = test
                    .failures
                    .first()
                    .map(|s| s.as_str())
                    .unwrap_or("Test failed");
                let _ = writeln!(xml, "      <failure message=\"{}\">", escape_xml(message));
                for failure in &test.failures {
                    let _ = writeln!(xml, "{}", escape_xml(failure));
                }
                xml.push_str("      </failure>\n");
            }

            // Include filesystem diff as system-out if present
            if let Some(ref diff) = test.fs_diff {
                let mut diff_output = String::new();
                if !diff.added.is_empty() {
                    diff_output.push_str("Added:\n");
                    for path in &diff.added {
                        let _ = writeln!(diff_output, "  + {}", path.display());
                    }
                }
                if !diff.removed.is_empty() {
                    diff_output.push_str("Removed:\n");
                    for path in &diff.removed {
                        let _ = writeln!(diff_output, "  - {}", path.display());
                    }
                }
                if !diff.modified.is_empty() {
                    diff_output.push_str("Modified:\n");
                    for path in &diff.modified {
                        let _ = writeln!(diff_output, "  ~ {}", path.display());
                    }
                }
                if !diff_output.is_empty() {
                    let _ = writeln!(
                        xml,
                        "      <system-out>{}</system-out>",
                        escape_xml(&diff_output)
                    );
                }
            }

            xml.push_str("    </testcase>\n");
        }

        xml.push_str("  </testsuite>\n");
    }

    xml.push_str("</testsuites>\n");
    xml
}

/// Escape special XML characters.
fn escape_xml(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}
