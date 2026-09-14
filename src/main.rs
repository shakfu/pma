//! pma: portfolio maintenance across many repositories.
//!
//! Design: `docs/dev/design.md`.

mod todo;

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(name = "pma", version, about = "Maintain many projects from one place")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Check TODO.md files against the format.
    ///
    /// Each path is a TODO.md file or a directory containing one. Exits 1 when
    /// any file has errors or cannot be read; warnings alone exit 0.
    Lint {
        /// Files or directories; defaults to the current directory.
        paths: Vec<PathBuf>,
    },
}

fn main() -> ExitCode {
    match Cli::parse().command {
        Command::Lint { paths } => lint(&paths),
    }
}

fn lint(paths: &[PathBuf]) -> ExitCode {
    let default = [PathBuf::from(".")];
    let paths = if paths.is_empty() {
        &default[..]
    } else {
        paths
    };

    let (mut errors, mut warnings, mut failed) = (0, 0, false);
    for path in paths {
        let file = todo_path(path);
        let text = match std::fs::read_to_string(&file) {
            Ok(text) => text,
            Err(err) => {
                println!("{}: error: {err}", file.display());
                errors += 1;
                failed = true;
                continue;
            }
        };
        let mut parsed = todo::parse(&text);
        parsed.diagnostics.sort_by_key(|d| d.line);
        for d in &parsed.diagnostics {
            println!(
                "{}:{}: {}: {}",
                file.display(),
                d.line,
                d.severity,
                d.message
            );
            match d.severity {
                todo::Severity::Error => errors += 1,
                todo::Severity::Warning => warnings += 1,
            }
        }
        failed |= parsed.has_errors();
    }

    if errors + warnings > 0 {
        eprintln!("{errors} errors, {warnings} warnings");
    }
    if failed {
        ExitCode::FAILURE
    } else {
        ExitCode::SUCCESS
    }
}

fn todo_path(path: &Path) -> PathBuf {
    if path.is_dir() {
        path.join("TODO.md")
    } else {
        path.to_path_buf()
    }
}
