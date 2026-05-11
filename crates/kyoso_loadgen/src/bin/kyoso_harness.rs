//! `kyoso_harness` — bench-harness orchestrator + findings emitter.
//!
//! Two subcommands:
//!
//! - `summarize` (default): read every existing report under
//!   `target/harness-reports/` + `target/criterion/`, aggregate into
//!   `findings.json` + `findings.md`. Fast, no recompile, no test
//!   runs. Use after `just bench-all` or in CI.
//! - `run`: invoke `just bench-all` first, then summarize. Slower but
//!   self-contained.
//!
//! AI feedback loop:
//!
//! ```bash
//! just findings           # summarize whatever's in target/harness-reports/
//! cat target/harness-reports/findings.json    # AI reads this
//! # … AI picks a finding, opens suspected_files, makes a fix, runs:
//! just bench-all          # re-run the relevant layer(s)
//! just findings           # check the finding cleared
//! ```

use std::path::PathBuf;
use std::process::Command;

use clap::{Parser, Subcommand};
use kyoso_loadgen::findings::{Findings, summarize};

#[derive(Parser, Debug)]
#[command(version, about = "Bench-harness orchestrator + findings emitter")]
struct Args {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand, Debug)]
enum Cmd {
    /// Read every existing report under --reports-dir + sibling
    /// `target/criterion/` and emit findings.{json,md}.
    Summarize {
        #[arg(long, default_value = "target/harness-reports")]
        reports_dir: PathBuf,
        #[arg(long, default_value = "target/harness-reports/findings.json")]
        output_json: PathBuf,
        #[arg(long, default_value = "target/harness-reports/findings.md")]
        output_md: PathBuf,
    },
    /// Run the full harness via `just bench-all` first, then
    /// summarize the resulting reports. Slower than `summarize` but
    /// self-contained.
    Run {
        #[arg(long, default_value = "target/harness-reports")]
        reports_dir: PathBuf,
        #[arg(long, default_value = "target/harness-reports/findings.json")]
        output_json: PathBuf,
        #[arg(long, default_value = "target/harness-reports/findings.md")]
        output_md: PathBuf,
        /// Skip `just bench-micro` (still runs loadgen + chaos).
        #[arg(long)]
        skip_bench: bool,
        /// Skip `just bench-load`.
        #[arg(long)]
        skip_load: bool,
        /// Skip `just chaos`.
        #[arg(long)]
        skip_chaos: bool,
    },
}

fn main() -> std::io::Result<()> {
    let args = Args::parse();
    match args.cmd {
        Cmd::Summarize {
            reports_dir,
            output_json,
            output_md,
        } => emit(&reports_dir, &output_json, &output_md),
        Cmd::Run {
            reports_dir,
            output_json,
            output_md,
            skip_bench,
            skip_load,
            skip_chaos,
        } => {
            if !skip_bench {
                eprintln!("==> just bench-micro");
                run_just("bench-micro")?;
            }
            if !skip_load {
                eprintln!("==> just bench-load");
                run_just("bench-load")?;
            }
            if !skip_chaos {
                eprintln!("==> just chaos");
                run_just("chaos")?;
            }
            emit(&reports_dir, &output_json, &output_md)
        }
    }
}

fn run_just(recipe: &str) -> std::io::Result<()> {
    let status = Command::new("just").arg(recipe).status()?;
    if !status.success() {
        eprintln!(
            "warning: `just {recipe}` exited with status {:?} — continuing to gather findings",
            status.code()
        );
    }
    Ok(())
}

fn emit(reports_dir: &PathBuf, output_json: &PathBuf, output_md: &PathBuf) -> std::io::Result<()> {
    if let Some(parent) = output_json.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let f: Findings = summarize(reports_dir);
    let json = serde_json::to_string_pretty(&f)
        .map_err(|e| std::io::Error::other(format!("serialize findings: {e}")))?;
    std::fs::write(output_json, json)?;
    std::fs::write(output_md, f.to_markdown())?;
    eprintln!(
        "wrote {} findings → {} + {}",
        f.findings.len(),
        output_json.display(),
        output_md.display(),
    );
    if f.next_actions.is_empty() {
        eprintln!("next_actions: (none — clean run)");
    } else {
        eprintln!("next_actions:");
        for a in &f.next_actions {
            eprintln!("  - {a}");
        }
    }
    // Exit non-zero if anything Critical surfaced — useful in CI.
    let critical = f
        .findings
        .iter()
        .any(|x| matches!(x.severity, kyoso_loadgen::findings::Severity::Critical));
    if critical {
        std::process::exit(2);
    }
    Ok(())
}
