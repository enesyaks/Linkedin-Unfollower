//! `liunfollow` — replay one captured LinkedIn unfollow request across a list
//! of companies, slowly, resumably, and without hardcoding a single LinkedIn
//! endpoint.
//!
//! See the repository README for the capture-once workflow.

mod curl;
mod list;
mod pace;
mod rng;
mod runner;
mod state;

use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{bail, Context, Result};
use clap::{Args, Parser, Subcommand};
use regex::Regex;

use crate::pace::{human, PaceConfig};
use crate::runner::{Event, PlanFilters, Report, RunOptions, Sender};
use crate::state::State;

#[derive(Parser)]
#[command(
    name = "liunfollow",
    version,
    about = "Bulk-unfollow LinkedIn companies at a deliberately polite pace.",
    long_about = "Replays ONE request you captured yourself from the browser's Network tab \
across a list of companies exported by web/unfollow.js.\n\n\
Nothing about LinkedIn's API is hardcoded, so this keeps working when LinkedIn \
changes its internals. Every run is a dry run unless you pass --yes."
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Check a captured request file: what it would send, and what it is missing.
    Doctor(DoctorArgs),
    /// Show exactly who would be unfollowed, and how long it would take.
    Plan(WorkArgs),
    /// Do it. Dry run unless you pass --yes.
    Run(WorkArgs),
    /// Print progress recorded in the state file.
    Status(StatusArgs),
}

#[derive(Args)]
struct DoctorArgs {
    /// File containing the captured `curl ...` command.
    #[arg(short, long)]
    request: PathBuf,
}

#[derive(Args)]
struct StatusArgs {
    #[arg(short, long, default_value = "liunfollow-state.json")]
    state: PathBuf,
}

#[derive(Args)]
struct WorkArgs {
    /// JSON exported by `LIU.export()` in the browser console.
    #[arg(short, long)]
    list: PathBuf,

    /// File containing the captured `curl ...` command.
    #[arg(short, long)]
    request: PathBuf,

    /// Resume checkpoint. Rewritten after every company.
    #[arg(short, long, default_value = "liunfollow-state.json")]
    state: PathBuf,

    /// Only companies whose name, slug or urn matches this regex.
    #[arg(long, value_name = "REGEX")]
    only: Option<String>,

    /// Skip companies whose name, slug or urn matches this regex.
    #[arg(long, value_name = "REGEX")]
    except: Option<String>,

    /// Stop after this many companies in one run.
    #[arg(long, default_value_t = 50)]
    max: usize,

    /// Seconds between requests, before jitter.
    #[arg(long, default_value_t = 4.0, value_name = "SECONDS")]
    interval: f64,

    /// Randomise each gap by this fraction, 0 to 1.
    #[arg(long, default_value_t = 0.4)]
    jitter: f64,

    /// Take a long pause after this many requests. 0 disables.
    #[arg(long, default_value_t = 15)]
    batch_size: u32,

    /// Length of that pause, in seconds.
    #[arg(long, default_value_t = 30.0, value_name = "SECONDS")]
    batch_pause: f64,

    /// Retries per company when LinkedIn asks us to wait.
    #[arg(long, default_value_t = 3)]
    retries: u32,

    /// Give up the whole run after this many back-to-back failures.
    #[arg(long, default_value_t = 3)]
    abort_after: u32,

    /// Per-request HTTP timeout, in seconds.
    #[arg(long, default_value_t = 30)]
    timeout: u64,

    /// First wait after LinkedIn pushes back, in seconds. Doubles each retry.
    #[arg(long, default_value_t = 60.0, value_name = "SECONDS")]
    backoff: f64,

    /// Ceiling for that doubling, in seconds.
    #[arg(long, default_value_t = 900.0, value_name = "SECONDS")]
    backoff_max: f64,

    /// Also re-attempt companies that previously failed permanently.
    #[arg(long)]
    retry_failed: bool,

    /// Actually send the requests. Without this, nothing leaves your machine.
    #[arg(long)]
    yes: bool,
}

impl WorkArgs {
    fn pace(&self) -> Result<PaceConfig> {
        let cfg = PaceConfig {
            interval: Duration::from_secs_f64(self.interval.max(0.0)),
            jitter: self.jitter,
            batch_size: self.batch_size,
            batch_pause: Duration::from_secs_f64(self.batch_pause.max(0.0)),
        };
        if let Err(msg) = cfg.validate() {
            bail!(msg);
        }
        Ok(cfg)
    }

    fn run_options(&self) -> Result<RunOptions> {
        if self.backoff < 1.0 {
            bail!("--backoff below 1s defeats the purpose of backing off");
        }
        if self.backoff_max < self.backoff {
            bail!("--backoff-max must not be smaller than --backoff");
        }
        Ok(RunOptions {
            pace: self.pace()?,
            max_retries: self.retries,
            max_consecutive_failures: self.abort_after.max(1),
            backoff_base: Duration::from_secs_f64(self.backoff),
            backoff_max: Duration::from_secs_f64(self.backoff_max),
        })
    }
}

// --------------------------------------------------------------- transport --

struct HttpSender {
    client: reqwest::Client,
}

impl HttpSender {
    fn new(timeout: Duration) -> Result<Self> {
        let client = reqwest::Client::builder()
            // A redirect here means "you are logged out" — never follow it.
            .redirect(reqwest::redirect::Policy::none())
            .timeout(timeout)
            .build()
            .context("cannot build the HTTP client")?;
        Ok(Self { client })
    }
}

impl Sender for HttpSender {
    async fn send(&self, req: &curl::Request) -> Result<runner::Response> {
        let method = reqwest::Method::from_bytes(req.method.as_bytes())
            .with_context(|| format!("invalid HTTP method: {}", req.method))?;
        let mut builder = self.client.request(method, &req.url);
        for (name, value) in &req.headers {
            builder = builder.header(name, value);
        }
        if let Some(body) = &req.body {
            builder = builder.body(body.clone());
        }
        let response = builder.send().await.context("request failed")?;
        let status = response.status().as_u16();
        let snippet = response
            .text()
            .await
            .unwrap_or_default()
            .chars()
            .take(300)
            .collect();
        Ok(runner::Response { status, snippet })
    }
}

// --------------------------------------------------------------- commands ---

fn load_request(path: &Path) -> Result<curl::Request> {
    warn_if_world_readable(path);
    let raw = std::fs::read_to_string(path)
        .with_context(|| format!("cannot read captured request: {}", path.display()))?;
    curl::parse(&raw)
}

/// The capture file holds your session cookie. Say so if the filesystem agrees.
fn warn_if_world_readable(path: &Path) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if let Ok(meta) = std::fs::metadata(path) {
            let mode = meta.permissions().mode() & 0o777;
            if mode & 0o077 != 0 {
                eprintln!(
                    "warning: {} is readable by other users (mode {:o}). It contains your \
LinkedIn session cookie.\n         fix with: chmod 600 {}",
                    path.display(),
                    mode,
                    path.display()
                );
            }
        }
    }
    #[cfg(not(unix))]
    let _ = path;
}

fn doctor(args: &DoctorArgs) -> Result<()> {
    let req = load_request(&args.request)?;
    println!("Captured request (credentials redacted):\n{req}");

    let vars = req.required_vars();
    if vars.is_empty() {
        println!(
            "PROBLEM: no placeholder found.\n\
             Every request would be byte-identical, so it would unfollow the same company \
             over and over.\n\
             Edit {} and replace the company identifier with one of: {}",
            args.request.display(),
            curl::PLACEHOLDERS.join(", ")
        );
        bail!("the captured request has no per-company placeholder");
    }

    println!("Placeholders found:");
    for site in req.placeholder_sites() {
        println!("  - {site}");
    }
    println!("Your company list must provide: {}", vars.join(", "));

    let has_cookie = req
        .headers
        .iter()
        .any(|(n, _)| n.eq_ignore_ascii_case("cookie"));
    if !has_cookie {
        println!(
            "warning: no cookie header. LinkedIn will reject this with 401/403 unless the \
capture included one."
        );
    }
    let has_csrf = req
        .headers
        .iter()
        .any(|(n, _)| n.to_ascii_lowercase().contains("csrf"));
    if !has_csrf && req.method != "GET" {
        println!("warning: no csrf-token header on a {} request.", req.method);
    }
    println!(
        "\nLooks usable. Next: liunfollow plan --list companies.json --request {}",
        args.request.display()
    );
    Ok(())
}

fn status(args: &StatusArgs) -> Result<()> {
    let state = State::load(&args.state)?;
    println!("state file: {}", args.state.display());
    println!("unfollowed: {}", state.done.len());
    println!("failed:     {}", state.failed.len());
    for f in &state.failed {
        let code = f
            .status
            .map(|s| s.to_string())
            .unwrap_or_else(|| "-".into());
        println!("  [{code}] {} — {}", f.label, f.reason);
    }
    Ok(())
}

fn build_plan<'a>(
    args: &WorkArgs,
    only: &'a Option<Regex>,
    except: &'a Option<Regex>,
    companies: &[list::Company],
    state: &State,
    required: &[String],
) -> runner::Plan {
    runner::plan(
        companies,
        state,
        required,
        &PlanFilters {
            only: only.as_ref(),
            except: except.as_ref(),
            max: Some(args.max),
            retry_failed: args.retry_failed,
        },
    )
}

fn print_plan(plan: &runner::Plan, pace: &PaceConfig) {
    println!("Targets: {}", plan.targets.len());
    for (i, c) in plan.targets.iter().enumerate() {
        println!("  {:>3}. {}  [{}]", i + 1, c.label(), c.key());
    }
    if plan.already_done > 0 {
        println!("Skipped, already unfollowed: {}", plan.already_done);
    }
    if plan.filtered_out > 0 {
        println!("Skipped by filters / past failures: {}", plan.filtered_out);
    }
    if plan.over_limit > 0 {
        println!("Left for the next run (--max): {}", plan.over_limit);
    }
    if !plan.incomplete.is_empty() {
        println!(
            "Cannot be used, the list lacks fields the template needs ({}):",
            plan.incomplete.len()
        );
        for (label, missing) in &plan.incomplete {
            println!("  - {label}: missing {}", missing.join(", "));
        }
    }
    let n = u32::try_from(plan.targets.len()).unwrap_or(u32::MAX);
    println!(
        "Estimated wall time: {} at {} between requests",
        human(pace.estimate(n)),
        human(pace.interval)
    );
}

async fn work(args: &WorkArgs, execute: bool) -> Result<()> {
    let template = load_request(&args.request)?;
    let required = template.required_vars();
    if required.is_empty() {
        bail!(
            "the captured request has no per-company placeholder — run `liunfollow doctor \
--request {}` for the fix",
            args.request.display()
        );
    }

    let companies = list::load(&args.list)?;
    let mut state = State::load(&args.state)?;
    let pace = args.pace()?;

    let only = args
        .only
        .as_deref()
        .map(Regex::new)
        .transpose()
        .context("--only is not a valid regex")?;
    let except = args
        .except
        .as_deref()
        .map(Regex::new)
        .transpose()
        .context("--except is not a valid regex")?;

    println!(
        "Loaded {} companies from {}",
        companies.len(),
        args.list.display()
    );
    let plan = build_plan(args, &only, &except, &companies, &state, &required);
    print_plan(&plan, &pace);

    if plan.targets.is_empty() {
        println!("\nNothing to do.");
        return Ok(());
    }

    if !execute || !args.yes {
        println!(
            "\nDRY RUN — no request was sent.\nRun it for real by adding --yes to the same command."
        );
        return Ok(());
    }

    println!("\nSending. Ctrl-C is safe: progress is saved after every company.\n");

    let sender = HttpSender::new(Duration::from_secs(args.timeout))?;
    let opts = args.run_options()?;
    let report = runner::run(
        &template,
        &plan.targets,
        &mut state,
        &sender,
        &runner::TokioSleeper,
        &opts,
        print_event,
    )
    .await?;

    print_report(&report, &args.state);
    if report.aborted.is_some() {
        std::process::exit(2);
    }
    Ok(())
}

fn print_event(event: Event<'_>) {
    match event {
        Event::Waiting(d) => {
            if d >= Duration::from_secs(10) {
                println!("   … waiting {}", human(d));
            }
        }
        Event::Attempt {
            index,
            total,
            label,
        } => {
            print!("[{index}/{total}] {label} … ");
            flush();
        }
        Event::Succeeded { status } => println!("ok ({status})"),
        Event::Failed { status, reason } => {
            let code = status.map(|s| s.to_string()).unwrap_or_else(|| "-".into());
            println!("FAILED [{code}] {reason}");
        }
        Event::BackingOff { wait, reason } => {
            println!("backing off {} — {reason}, retrying", human(wait));
        }
        Event::Aborted(reason) => println!("\nABORTED: {reason}"),
    }
}

fn flush() {
    use std::io::Write;
    let _ = std::io::stdout().flush();
}

fn print_report(report: &Report, state_path: &Path) {
    println!(
        "\nUnfollowed {} of {} attempted ({} requests sent, {} failed).",
        report.succeeded, report.attempted, report.requests_sent, report.failed
    );
    println!("Progress saved in {}", state_path.display());
    if report.aborted.is_none() && report.succeeded > 0 {
        println!("Re-run the same command to continue with the next batch.");
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    match &cli.command {
        Command::Doctor(args) => doctor(args),
        Command::Status(args) => status(args),
        Command::Plan(args) => work(args, false).await,
        Command::Run(args) => work(args, true).await,
    }
}
