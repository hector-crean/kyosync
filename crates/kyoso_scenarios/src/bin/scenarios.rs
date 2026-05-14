//! `kyoso_scenarios` binary entry point.
//!
//! ```text
//! kyoso_scenarios --scenario late_join
//! kyoso_scenarios --all
//! kyoso_scenarios --list
//! ```
//!
//! Each scenario writes its report to
//! `target/harness-reports/scenario-<name>.json` and prints a one-line
//! summary to stdout. Exit code is non-zero iff any scenario diverged
//! or aborted — usable as a CI gate.

use clap::Parser;
use kyoso_scenarios::{run_scenario, scenario_names, write_report, ScenarioStatus};

#[derive(Parser, Debug)]
#[command(
    version,
    about = "Scripted multi-client scenario runner for the bench harness"
)]
struct Args {
    /// Run a specific scenario by name. Use `--list` to see options.
    #[arg(long, conflicts_with_all = ["all", "list"])]
    scenario: Option<String>,
    /// Run every known scenario in sequence.
    #[arg(long, conflicts_with_all = ["scenario", "list"])]
    all: bool,
    /// Print the catalog and exit.
    #[arg(long, conflicts_with_all = ["scenario", "all"])]
    list: bool,
}

#[tokio::main(flavor = "multi_thread", worker_threads = 4)]
async fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("warn,kyoso_scenarios=info")),
        )
        .init();

    let args = Args::parse();
    if args.list {
        for name in scenario_names() {
            println!("{name}");
        }
        return;
    }

    let to_run: Vec<&str> = if args.all {
        scenario_names().iter().copied().collect()
    } else if let Some(name) = &args.scenario {
        vec![name.as_str()]
    } else {
        eprintln!("specify --scenario <name> or --all (or --list)");
        std::process::exit(2);
    };

    let mut any_failed = false;
    for name in to_run {
        let report = match run_scenario(name).await {
            Ok(r) => r,
            Err(e) => {
                eprintln!("error: {e}");
                any_failed = true;
                continue;
            }
        };
        let path = match write_report(&report) {
            Ok(p) => p,
            Err(e) => {
                eprintln!("error writing report for {name}: {e}");
                any_failed = true;
                continue;
            }
        };
        let status_glyph = match report.status {
            ScenarioStatus::Converged => "✔",
            ScenarioStatus::Diverged => "✘",
            ScenarioStatus::Aborted => "⚠",
        };
        println!("{status_glyph} {} → {}", report.summary, path.display());
        if !matches!(report.status, ScenarioStatus::Converged) {
            any_failed = true;
        }
    }

    if any_failed {
        std::process::exit(1);
    }
}
