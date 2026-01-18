mod loader;
mod runner;
mod schema;

use clap::{Parser, Subcommand, ValueEnum};
use std::fs;
use std::path::PathBuf;

#[derive(Clone, Copy, Default, ValueEnum)]
enum OutputFormat {
    /// Human-readable output with checkmarks
    #[default]
    Human,
    /// Machine-readable JSON output
    Json,
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
        Command::Run { path, output } => {
            // Determine the test root directory for suite config
            let test_root = if path.is_file() {
                path.parent().unwrap_or(&path)
            } else {
                &path
            };

            // Load suite config if present
            let suite_config = match loader::load_suite_config(test_root) {
                Ok(config) => config,
                Err(e) => {
                    eprintln!("Error loading suite config: {e}");
                    std::process::exit(1);
                }
            };

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

            // Run suite-level setup if configured
            if let Some(ref config) = suite_config
                && let Err(e) = runner::run_suite_setup(config)
            {
                eprintln!("Suite setup failed: {e}");
                std::process::exit(1);
            }

            let mut all_results = Vec::new();
            let mut total_passed = 0;
            let mut total_failed = 0;

            for spec_path in &specs {
                let spec = match loader::load_spec(spec_path) {
                    Ok(s) => s,
                    Err(e) => {
                        if matches!(output, OutputFormat::Human) {
                            eprintln!("✗ Failed to load {}: {e}", spec_path.display());
                        }
                        total_failed += 1;
                        continue;
                    }
                };

                let result = runner::run_spec(&spec, suite_config.as_ref());

                match output {
                    OutputFormat::Human => {
                        println!("\n{}", spec_path.display());
                        for test in &result.tests {
                            if test.passed {
                                println!("  ✓ {} ({:.2?})", test.name, test.duration);
                                total_passed += 1;
                            } else {
                                println!("  ✗ {} ({:.2?})", test.name, test.duration);
                                for failure in &test.failures {
                                    println!("    {failure}");
                                }
                                total_failed += 1;
                            }
                        }
                    }
                    OutputFormat::Json => {
                        for test in &result.tests {
                            if test.passed {
                                total_passed += 1;
                            } else {
                                total_failed += 1;
                            }
                        }
                        all_results.push(serde_json::json!({
                            "file": spec_path.display().to_string(),
                            "tests": result.tests,
                        }));
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

            match output {
                OutputFormat::Human => {
                    println!("\n{total_passed} passed, {total_failed} failed");
                }
                OutputFormat::Json => {
                    let output = serde_json::json!({
                        "passed": total_passed,
                        "failed": total_failed,
                        "results": all_results,
                    });
                    println!(
                        "{}",
                        serde_json::to_string_pretty(&output).expect("Failed to serialize")
                    );
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
