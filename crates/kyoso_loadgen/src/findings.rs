//! Findings schema + parsers.
//!
//! Reads everything the bench harness produces (`target/harness-reports/*.json`,
//! `target/criterion/<group>/<bench>/new/estimates.json`, `cargo test`
//! output) and aggregates into a single [`Findings`] document. Two
//! consumers:
//!
//! - **AI agents** (Claude Code, etc.) read `findings.json` to pick
//!   the next action. Each [`Finding`] carries `severity`, `repro`
//!   (a single shell command), and `suspected_files` (a list of
//!   source paths most likely to need a fix). The agent picks the
//!   highest-severity finding, opens the suspected files, makes a
//!   change, re-runs the harness, and re-reads `findings.json` to
//!   confirm the finding cleared.
//! - **Humans** read `findings.md`, the same data prettified. Useful
//!   for PR descriptions and standup updates.
//!
//! See `HARNESS.md` "Feedback loop" for the workflow.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "lowercase")]
pub enum Severity {
    /// CRDT correctness violation, divergence, build failure.
    Critical,
    /// Throughput / latency regression beyond the configured threshold.
    High,
    /// Performance change worth noting; not necessarily bad.
    Medium,
    /// Informational; not actionable on its own.
    Low,
    Info,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct Findings {
    /// ISO-8601 timestamp.
    pub generated_at: String,
    pub summary: Summary,
    /// Sorted by `severity` descending, then by `layer`.
    pub findings: Vec<Finding>,
    /// One-line action items the AI should consider next, ordered by
    /// priority. Drawn from the highest-severity findings.
    pub next_actions: Vec<String>,
}

#[derive(Debug, Default, Serialize, Deserialize)]
pub struct Summary {
    pub chaos: ChaosSummary,
    pub loadgen: LoadgenSummary,
    pub criterion: CriterionSummary,
    pub reconnect: ReconnectSummary,
}

#[derive(Debug, Default, Serialize, Deserialize)]
pub struct ChaosSummary {
    pub sweeps_run: usize,
    pub total_seeds: usize,
    pub diverged_seeds: usize,
    pub all_converged: bool,
}

#[derive(Debug, Default, Serialize, Deserialize)]
pub struct LoadgenSummary {
    /// Keyed by profile name (`graph` / `comments` / `mixed`).
    pub profiles: BTreeMap<String, LoadgenProfile>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct LoadgenProfile {
    pub clients: usize,
    pub rate_per_client: u32,
    pub duration_s: u64,
    pub ops_submitted: u64,
    pub ops_echoed: u64,
    pub errors: u64,
    pub throughput_ops_per_sec: f64,
    pub latency_p50_us: u64,
    pub latency_p99_us: u64,
    pub latency_max_us: u64,
}

#[derive(Debug, Default, Serialize, Deserialize)]
pub struct CriterionSummary {
    pub bench_count: usize,
    /// All benches read; useful for the AI to know what's covered.
    pub bench_names: Vec<String>,
}

#[derive(Debug, Default, Serialize, Deserialize)]
pub struct ReconnectSummary {
    /// Whether `cargo test -p kyoso_server --test reconnect` passed.
    /// `None` if the test output isn't available (didn't run yet).
    pub passed: Option<bool>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct Finding {
    pub severity: Severity,
    /// Which harness layer surfaced this — `chaos`, `loadgen`,
    /// `criterion`, `test`, etc.
    pub layer: String,
    pub title: String,
    /// Free-form context the AI can quote in commit messages or
    /// surface to a user. May span multiple lines.
    pub details: String,
    /// One-line shell command that reproduces the finding. Optional
    /// for findings that are just observations (load-test summary).
    pub repro: Option<String>,
    /// Source files most likely related. AI uses these as starting
    /// points for the fix. Heuristic — may include false positives;
    /// the agent is expected to investigate.
    pub suspected_files: Vec<String>,
}

// ---------------------------------------------------------------------------
// Parsers
// ---------------------------------------------------------------------------

/// Read every report file under `reports_dir` and aggregate into a
/// fresh [`Findings`] document.
pub fn summarize(reports_dir: &Path) -> Findings {
    let mut findings = Vec::new();
    let mut summary = Summary::default();

    if let Ok(entries) = fs::read_dir(reports_dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            let Some(name) = path.file_name().and_then(|s| s.to_str()) else {
                continue;
            };
            if name.starts_with("chaos-") && name.ends_with(".json") {
                if let Some((s, mut fs)) = parse_chaos(&path) {
                    summary.chaos.sweeps_run += 1;
                    summary.chaos.total_seeds += s.total_seeds;
                    summary.chaos.diverged_seeds += s.diverged_seeds;
                    findings.append(&mut fs);
                }
            } else if name.starts_with("loadgen-") && name.ends_with(".json") {
                if let Some((profile_name, profile, mut fs)) = parse_loadgen(&path) {
                    summary.loadgen.profiles.insert(profile_name, profile);
                    findings.append(&mut fs);
                }
            }
        }
    }
    summary.chaos.all_converged = summary.chaos.diverged_seeds == 0;

    // Criterion lives under `target/criterion/`, sibling of
    // reports_dir. Walk both levels of the criterion layout.
    if let Some(target_dir) = reports_dir.parent() {
        let crit_dir = target_dir.join("criterion");
        if crit_dir.is_dir() {
            let (cs, mut cfs) = parse_criterion(&crit_dir);
            summary.criterion = cs;
            findings.append(&mut cfs);
        }
    }

    // Sort findings by severity (Critical first) then by layer.
    findings.sort_by(|a, b| a.severity.cmp(&b.severity).then(a.layer.cmp(&b.layer)));

    let next_actions: Vec<String> = findings
        .iter()
        .filter(|f| matches!(f.severity, Severity::Critical | Severity::High))
        .take(5)
        .map(|f| format!("[{}] {}", layer_short(&f.layer), f.title))
        .collect();

    Findings {
        generated_at: chrono::Utc::now().to_rfc3339(),
        summary,
        findings,
        next_actions,
    }
}

fn layer_short(layer: &str) -> &str {
    match layer {
        "chaos" => "chaos",
        "loadgen" => "load",
        "criterion" => "bench",
        "test" => "test",
        other => other,
    }
}

fn parse_chaos(path: &Path) -> Option<(ChaosCount, Vec<Finding>)> {
    let bytes = fs::read(path).ok()?;
    let report: ChaosFileShape = serde_json::from_slice(&bytes).ok()?;
    let mut findings = Vec::new();
    let total_seeds = report.runs.len();
    let mut diverged_seeds = 0usize;
    for run in &report.runs {
        if !run.converged {
            diverged_seeds += 1;
            let model = path
                .file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or("unknown")
                .strip_prefix("chaos-")
                .unwrap_or("unknown")
                .to_string();
            findings.push(divergence_finding(&model, run));
        }
    }
    Some((
        ChaosCount {
            total_seeds,
            diverged_seeds,
        },
        findings,
    ))
}

struct ChaosCount {
    total_seeds: usize,
    diverged_seeds: usize,
}

#[derive(Deserialize)]
struct ChaosFileShape {
    #[serde(default)]
    all_converged: bool,
    runs: Vec<ChaosRunShape>,
}

#[derive(Deserialize)]
struct ChaosRunShape {
    config: ChaosRunConfig,
    converged: bool,
    ops_issued: u64,
    re_delivered_after_drop: u64,
    peer_applied_seqs: Vec<u64>,
}

#[derive(Deserialize)]
struct ChaosRunConfig {
    peers: usize,
    op_rounds: usize,
    drop_probability: f64,
    max_delay_rounds: usize,
    seed: u64,
}

fn divergence_finding(model: &str, run: &ChaosRunShape) -> Finding {
    let suspected_files = match model {
        "graph" => vec![
            "crates/kyoso_graph_crdt/src/backend.rs".to_string(),
            "crates/kyoso_graph_crdt/src/op.rs".to_string(),
        ],
        "comments" => vec![
            "crates/kyoso_comments_crdt/src/backend.rs".to_string(),
            "crates/kyoso_comments_crdt/src/op.rs".to_string(),
        ],
        _ => Vec::new(),
    };
    let repro = format!(
        "just chaos-{model} {peers} {rounds} {drop} {delay} 1 \
         && # then re-run with --first-seed 0x{seed:X}",
        peers = run.config.peers,
        rounds = run.config.op_rounds,
        drop = run.config.drop_probability,
        delay = run.config.max_delay_rounds,
        seed = run.config.seed,
    );
    Finding {
        severity: Severity::Critical,
        layer: "chaos".to_string(),
        title: format!(
            "{} chaos sim diverged at seed 0x{:X}",
            model, run.config.seed
        ),
        details: format!(
            "Peers (count {}) reached the same applied_seq ({:?}) but at least one \
             peer's snapshot did not match the canonical replica. \
             ops_issued={} drops_recovered={} drop_prob={} max_delay_rounds={}",
            run.config.peers,
            run.peer_applied_seqs,
            run.ops_issued,
            run.re_delivered_after_drop,
            run.config.drop_probability,
            run.config.max_delay_rounds,
        ),
        repro: Some(repro),
        suspected_files,
    }
}

fn parse_loadgen(path: &Path) -> Option<(String, LoadgenProfile, Vec<Finding>)> {
    let bytes = fs::read(path).ok()?;
    let report: LoadgenFileShape = serde_json::from_slice(&bytes).ok()?;
    let profile_name = path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("unknown")
        .strip_prefix("loadgen-")
        .unwrap_or("unknown")
        .to_string();
    let profile = LoadgenProfile {
        clients: report.config.clients,
        rate_per_client: report.config.rate_per_client,
        duration_s: report.config.duration_s,
        ops_submitted: report.ops_submitted,
        ops_echoed: report.ops_echoed,
        errors: report.errors,
        throughput_ops_per_sec: report.throughput_ops_per_sec,
        latency_p50_us: report.latency_us.p50,
        latency_p99_us: report.latency_us.p99,
        latency_max_us: report.latency_us.max,
    };
    let mut findings = Vec::new();
    if report.errors > 0 {
        findings.push(Finding {
            severity: Severity::Critical,
            layer: "loadgen".to_string(),
            title: format!(
                "loadgen profile '{}' reported {} errors",
                profile_name, report.errors
            ),
            details: format!("ops_submitted={} ops_echoed={} errors={}",
                report.ops_submitted, report.ops_echoed, report.errors),
            repro: Some(format!("just bench-load-{profile_name}")),
            suspected_files: vec![
                "crates/kyoso_sync/src/client.rs".to_string(),
                "crates/kyoso_sync/src/transport.rs".to_string(),
                "apps/kyoso_server/src/handlers/room_ws.rs".to_string(),
            ],
        });
    }
    if report.ops_submitted != report.ops_echoed {
        findings.push(Finding {
            severity: Severity::High,
            layer: "loadgen".to_string(),
            title: format!(
                "loadgen profile '{}' lost {} ops in flight",
                profile_name,
                report.ops_submitted.saturating_sub(report.ops_echoed)
            ),
            details: format!(
                "ops_submitted={} but only {} were echoed back. Could indicate \
                 server lag, broadcast-channel saturation, or a peer dropping \
                 frames at the WS layer.",
                report.ops_submitted, report.ops_echoed
            ),
            repro: Some(format!("just bench-load-{profile_name}")),
            suspected_files: vec![
                "apps/kyoso_server/src/handlers/room_ws.rs".to_string(),
                "apps/kyoso_server/src/services/room.rs".to_string(),
            ],
        });
    }
    // Latency observation as Info — not necessarily a problem, but
    // useful for the AI's situational awareness.
    findings.push(Finding {
        severity: Severity::Info,
        layer: "loadgen".to_string(),
        title: format!(
            "loadgen profile '{}': {:.0} ops/s, p99 {} \u{00B5}s",
            profile_name, report.throughput_ops_per_sec, report.latency_us.p99,
        ),
        details: format!(
            "{} clients \u{00D7} {} ops/s \u{00D7} {}s. p50={}\u{00B5}s p95={}\u{00B5}s \
             p99={}\u{00B5}s p999={}\u{00B5}s max={}\u{00B5}s mean={:.1}\u{00B5}s",
            report.config.clients,
            report.config.rate_per_client,
            report.config.duration_s,
            report.latency_us.p50,
            report.latency_us.p95,
            report.latency_us.p99,
            report.latency_us.p999,
            report.latency_us.max,
            report.latency_us.mean,
        ),
        repro: None,
        suspected_files: Vec::new(),
    });
    Some((profile_name, profile, findings))
}

#[derive(Deserialize)]
struct LoadgenFileShape {
    config: LoadgenFileConfig,
    ops_submitted: u64,
    ops_echoed: u64,
    errors: u64,
    throughput_ops_per_sec: f64,
    latency_us: LoadgenLatencyShape,
}

#[derive(Deserialize)]
struct LoadgenFileConfig {
    clients: usize,
    rate_per_client: u32,
    duration_s: u64,
}

#[derive(Deserialize)]
struct LoadgenLatencyShape {
    p50: u64,
    p95: u64,
    p99: u64,
    p999: u64,
    max: u64,
    mean: f64,
}

/// Walk `target/criterion/` looking for per-bench `estimates.json`
/// files. Records the bench name (group/name) and the mean point
/// estimate. No regression detection without a saved baseline — that
/// requires comparing `<bench>/base/estimates.json` against `new/`,
/// which criterion handles internally; we just surface the existence
/// of the bench so the AI knows what's covered.
fn parse_criterion(crit_dir: &Path) -> (CriterionSummary, Vec<Finding>) {
    let mut bench_names = Vec::new();
    walk_criterion(crit_dir, &mut bench_names, &PathBuf::new());
    bench_names.sort();
    bench_names.dedup();
    let summary = CriterionSummary {
        bench_count: bench_names.len(),
        bench_names: bench_names.clone(),
    };
    // No findings — criterion regressions need a baseline diff that
    // happens via `cargo bench --baseline harness`. We just record
    // the bench inventory here.
    (summary, Vec::new())
}

fn walk_criterion(dir: &Path, out: &mut Vec<String>, prefix: &Path) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            let name = entry.file_name();
            // Skip the criterion-meta directory.
            if name == "report" {
                continue;
            }
            // Each bench's `new/estimates.json` is the per-iteration
            // record. Presence => bench exists.
            let est = path.join("new").join("estimates.json");
            if est.is_file() {
                let group_name = prefix.join(&name).to_string_lossy().to_string();
                out.push(group_name);
            } else {
                walk_criterion(&path, out, &prefix.join(name));
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Markdown renderer
// ---------------------------------------------------------------------------

impl Findings {
    pub fn to_markdown(&self) -> String {
        let mut out = String::new();
        out.push_str("# Bench harness findings\n\n");
        out.push_str(&format!("_Generated at {}_\n\n", self.generated_at));

        // Headline summary
        out.push_str("## Summary\n\n");
        out.push_str(&format!(
            "- **Chaos** ({} sweep{}, {} seed{}): {} {}\n",
            self.summary.chaos.sweeps_run,
            if self.summary.chaos.sweeps_run == 1 { "" } else { "s" },
            self.summary.chaos.total_seeds,
            if self.summary.chaos.total_seeds == 1 { "" } else { "s" },
            if self.summary.chaos.all_converged { "✓ all converged" } else { "✗ divergences detected" },
            if self.summary.chaos.diverged_seeds > 0 {
                format!("({} diverged)", self.summary.chaos.diverged_seeds)
            } else {
                String::new()
            },
        ));
        out.push_str(&format!(
            "- **Loadgen**: {} profile{}\n",
            self.summary.loadgen.profiles.len(),
            if self.summary.loadgen.profiles.len() == 1 { "" } else { "s" }
        ));
        for (name, p) in &self.summary.loadgen.profiles {
            out.push_str(&format!(
                "  - `{}`: {} clients × {} ops/s × {}s → {:.0} ops/s, p99 {}µs, errors={}\n",
                name,
                p.clients,
                p.rate_per_client,
                p.duration_s,
                p.throughput_ops_per_sec,
                p.latency_p99_us,
                p.errors,
            ));
        }
        out.push_str(&format!(
            "- **Criterion**: {} benches recorded\n",
            self.summary.criterion.bench_count
        ));
        if let Some(passed) = self.summary.reconnect.passed {
            out.push_str(&format!(
                "- **Reconnect tests**: {}\n",
                if passed { "✓ passed" } else { "✗ failed" }
            ));
        }
        out.push('\n');

        // Next actions
        if !self.next_actions.is_empty() {
            out.push_str("## Next actions\n\n");
            for action in &self.next_actions {
                out.push_str(&format!("- {}\n", action));
            }
            out.push('\n');
        }

        // Findings detail
        out.push_str("## Findings\n\n");
        if self.findings.is_empty() {
            out.push_str("_No findings._\n");
        } else {
            for f in &self.findings {
                let badge = match f.severity {
                    Severity::Critical => "🔴 CRITICAL",
                    Severity::High => "🟠 HIGH",
                    Severity::Medium => "🟡 MEDIUM",
                    Severity::Low => "🔵 LOW",
                    Severity::Info => "⚪ INFO",
                };
                out.push_str(&format!("### {} `{}` {}\n\n", badge, f.layer, f.title));
                out.push_str(&format!("{}\n\n", f.details));
                if let Some(repro) = &f.repro {
                    out.push_str("**Repro:**\n\n");
                    out.push_str(&format!("```bash\n{}\n```\n\n", repro));
                }
                if !f.suspected_files.is_empty() {
                    out.push_str("**Suspected files:**\n\n");
                    for file in &f.suspected_files {
                        out.push_str(&format!("- `{}`\n", file));
                    }
                    out.push('\n');
                }
            }
        }
        out
    }
}
