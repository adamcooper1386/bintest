use clap::{Parser, Subcommand};
use std::path::PathBuf;

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
        Command::Run { path } => {
            println!("Running tests from: {}", path.display());
        }
        Command::Validate { path } => {
            println!("Validating specs at: {}", path.display());
        }
        Command::Init { path } => {
            println!("Creating new spec file at: {}", path.display());
        }
        Command::Schema => {
            println!("Outputting schema...");
        }
    }
}
