//! The unfollow loop: plan it, then walk it slowly, checkpointing as it goes.
//!
//! Transport and sleeping are behind traits so the loop's real behaviour
//! (classification, retries, backoff, abort conditions) is unit-tested without
//! touching the network or the clock.

use std::time::Duration;

use anyhow::Result;
use regex::Regex;

use crate::curl::Request;
use crate::list::Company;
use crate::pace::{Backoff, PaceConfig, Pacer};
use crate::state::{Failure, State};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Response {
    pub status: u16,
    /// First few hundred bytes, for diagnostics only.
    pub snippet: String,
}

pub trait Sender {
    fn send(&self, req: &Request) -> impl std::future::Future<Output = Result<Response>> + Send;
}

pub trait Sleeper {
    fn sleep(&self, d: Duration) -> impl std::future::Future<Output = ()> + Send;
}

pub struct TokioSleeper;

impl Sleeper for TokioSleeper {
    async fn sleep(&self, d: Duration) {
        if !d.is_zero() {
            tokio::time::sleep(d).await;
        }
    }
}

// ----------------------------------------------------------------- planning --

#[derive(Debug, Default)]
pub struct Plan {
    pub targets: Vec<Company>,
    pub already_done: usize,
    pub filtered_out: usize,
    /// Entries the template cannot be filled for, with the missing variables.
    pub incomplete: Vec<(String, Vec<String>)>,
    /// Trimmed off by `--max`.
    pub over_limit: usize,
}

pub struct PlanFilters<'a> {
    pub only: Option<&'a Regex>,
    pub except: Option<&'a Regex>,
    pub max: Option<usize>,
    pub retry_failed: bool,
}

/// Decide exactly who gets a request, and why everyone else does not.
pub fn plan(
    companies: &[Company],
    state: &State,
    required_vars: &[String],
    filters: &PlanFilters<'_>,
) -> Plan {
    let mut out = Plan::default();

    for company in companies {
        let label = company.label();

        if let Some(re) = filters.only {
            if !matches_company(re, company) {
                out.filtered_out += 1;
                continue;
            }
        }
        if let Some(re) = filters.except {
            if matches_company(re, company) {
                out.filtered_out += 1;
                continue;
            }
        }
        if state.is_done(&company.key()) {
            out.already_done += 1;
            continue;
        }
        if !filters.retry_failed
            && state
                .failed
                .iter()
                .any(|f| f.key == company.key() && is_permanent_status(f.status))
        {
            out.filtered_out += 1;
            continue;
        }

        let missing = company.missing_for(required_vars);
        if !missing.is_empty() {
            out.incomplete.push((label, missing));
            continue;
        }

        out.targets.push(company.clone());
    }

    if let Some(max) = filters.max {
        if out.targets.len() > max {
            out.over_limit = out.targets.len() - max;
            out.targets.truncate(max);
        }
    }

    out
}

fn matches_company(re: &Regex, c: &Company) -> bool {
    [c.name.as_deref(), c.slug.as_deref(), c.urn.as_deref()]
        .into_iter()
        .flatten()
        .any(|field| re.is_match(field))
}

fn is_permanent_status(status: Option<u16>) -> bool {
    matches!(classify(status.unwrap_or(0)), Outcome::Permanent(_))
}

// ----------------------------------------------------------- classification --

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    Ok,
    /// Server is pushing back; wait and try the same company again.
    Retry(String),
    /// This company will never succeed; move on.
    Permanent(String),
    /// Stop the whole run.
    Fatal(String),
}

/// Map an HTTP status onto what the loop should do next.
pub fn classify(status: u16) -> Outcome {
    match status {
        200..=204 | 206 => Outcome::Ok,
        // LinkedIn's own "you look like a bot" status is 999.
        429 | 999 => Outcome::Retry(format!("rate limited (HTTP {status})")),
        500 | 502 | 503 | 504 => Outcome::Retry(format!("server error (HTTP {status})")),
        401 | 407 => Outcome::Fatal(format!(
            "not authenticated (HTTP {status}) — your captured cookie has expired, capture again"
        )),
        403 => Outcome::Fatal(format!(
            "forbidden (HTTP {status}) — expired CSRF token or the request was rejected; capture again"
        )),
        300..=399 => Outcome::Fatal(format!(
            "redirected (HTTP {status}) — almost always a bounce to the login page"
        )),
        400 | 404 | 409 | 410 | 422 => {
            Outcome::Permanent(format!("request rejected for this company (HTTP {status})"))
        }
        other => Outcome::Permanent(format!("unexpected HTTP {other}")),
    }
}

// --------------------------------------------------------------- the runner --

#[derive(Debug, Clone)]
pub struct RunOptions {
    pub pace: PaceConfig,
    /// Retries per company when the server asks us to wait.
    pub max_retries: u32,
    /// Stop the run after this many companies fail back to back.
    pub max_consecutive_failures: u32,
    pub backoff_base: Duration,
    pub backoff_max: Duration,
}

impl Default for RunOptions {
    fn default() -> Self {
        Self {
            pace: PaceConfig::default(),
            max_retries: 3,
            max_consecutive_failures: 3,
            backoff_base: Duration::from_secs(60),
            backoff_max: Duration::from_secs(900),
        }
    }
}

#[derive(Debug)]
pub enum Event<'a> {
    Waiting(Duration),
    Attempt {
        index: usize,
        total: usize,
        label: &'a str,
    },
    Succeeded {
        status: u16,
    },
    Failed {
        status: Option<u16>,
        reason: String,
    },
    BackingOff {
        wait: Duration,
        reason: String,
    },
    Aborted(String),
}

#[derive(Debug, Default, PartialEq, Eq)]
pub struct Report {
    pub attempted: usize,
    pub succeeded: usize,
    pub failed: usize,
    pub requests_sent: u32,
    pub aborted: Option<String>,
}

/// Walk the plan. Saves the checkpoint after every company.
pub async fn run<S: Sender, Z: Sleeper>(
    template: &Request,
    targets: &[Company],
    state: &mut State,
    sender: &S,
    sleeper: &Z,
    opts: &RunOptions,
    mut on_event: impl FnMut(Event<'_>),
) -> Result<Report> {
    let mut report = Report::default();
    let mut pacer = Pacer::new(opts.pace);
    let mut consecutive_failures = 0u32;
    let total = targets.len();

    for (index, company) in targets.iter().enumerate() {
        let label = company.label();
        let key = company.key();
        let rendered = template.render(&company.vars());

        let mut backoff = Backoff::new(opts.backoff_base, opts.backoff_max);
        let mut last: (Option<u16>, String) = (None, "not attempted".to_string());
        let mut settled = false;

        for _ in 0..=opts.max_retries {
            let delay = pacer.next_delay();
            if !delay.is_zero() {
                on_event(Event::Waiting(delay));
                sleeper.sleep(delay).await;
            }

            on_event(Event::Attempt {
                index: index + 1,
                total,
                label: &label,
            });

            let response = match sender.send(&rendered).await {
                Ok(r) => r,
                Err(e) => {
                    // A transport error (DNS, TLS, timeout) is worth one wait.
                    let wait = backoff.next();
                    last = (None, format!("transport error: {e}"));
                    if backoff.attempts() > opts.max_retries {
                        break;
                    }
                    on_event(Event::BackingOff {
                        wait,
                        reason: last.1.clone(),
                    });
                    sleeper.sleep(wait).await;
                    continue;
                }
            };
            report.requests_sent += 1;

            match classify(response.status) {
                Outcome::Ok => {
                    state.mark_done(&key);
                    state.save()?;
                    report.succeeded += 1;
                    consecutive_failures = 0;
                    on_event(Event::Succeeded {
                        status: response.status,
                    });
                    settled = true;
                    break;
                }
                Outcome::Retry(reason) => {
                    last = (Some(response.status), reason.clone());
                    let wait = backoff.next();
                    if backoff.attempts() > opts.max_retries {
                        break;
                    }
                    on_event(Event::BackingOff { wait, reason });
                    sleeper.sleep(wait).await;
                }
                Outcome::Permanent(reason) => {
                    last = (Some(response.status), reason);
                    break;
                }
                Outcome::Fatal(reason) => {
                    state.mark_failed(Failure {
                        key: key.clone(),
                        label: label.clone(),
                        status: Some(response.status),
                        reason: reason.clone(),
                    });
                    state.save()?;
                    report.attempted += 1;
                    report.failed += 1;
                    report.aborted = Some(reason.clone());
                    on_event(Event::Failed {
                        status: Some(response.status),
                        reason,
                    });
                    on_event(Event::Aborted(report.aborted.clone().unwrap_or_default()));
                    return Ok(report);
                }
            }
        }

        report.attempted += 1;

        if !settled {
            state.mark_failed(Failure {
                key,
                label: label.clone(),
                status: last.0,
                reason: last.1.clone(),
            });
            state.save()?;
            report.failed += 1;
            consecutive_failures += 1;
            on_event(Event::Failed {
                status: last.0,
                reason: last.1.clone(),
            });

            if consecutive_failures >= opts.max_consecutive_failures {
                let reason = format!(
                    "{consecutive_failures} companies failed in a row — stopping instead of hammering LinkedIn"
                );
                report.aborted = Some(reason.clone());
                on_event(Event::Aborted(reason));
                return Ok(report);
            }
        }
    }

    Ok(report)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    use crate::curl;

    struct ScriptedSender {
        statuses: Mutex<Vec<u16>>,
        seen: Mutex<Vec<String>>,
    }

    impl ScriptedSender {
        fn new(statuses: &[u16]) -> Self {
            Self {
                statuses: Mutex::new(statuses.iter().rev().copied().collect()),
                seen: Mutex::new(Vec::new()),
            }
        }

        fn seen(&self) -> Vec<String> {
            self.seen.lock().expect("lock").clone()
        }
    }

    impl Sender for ScriptedSender {
        async fn send(&self, req: &Request) -> Result<Response> {
            self.seen
                .lock()
                .expect("lock")
                .push(req.body.clone().unwrap_or_else(|| req.url.clone()));
            let status = self.statuses.lock().expect("lock").pop().unwrap_or(200);
            Ok(Response {
                status,
                snippet: String::new(),
            })
        }
    }

    struct NoSleep;

    impl Sleeper for NoSleep {
        async fn sleep(&self, _d: Duration) {}
    }

    fn template() -> Request {
        curl::parse("curl 'https://x.test/u' --data-raw '{\"urn\":\"{urn}\"}'").unwrap()
    }

    fn companies(n: usize) -> Vec<Company> {
        (1..=n)
            .map(|i| Company {
                name: Some(format!("Co {i}")),
                slug: Some(format!("co-{i}")),
                urn: Some(format!("urn:li:fsd_company:{i}")),
                ..Default::default()
            })
            .collect()
    }

    fn fast_opts() -> RunOptions {
        RunOptions {
            pace: PaceConfig {
                interval: Duration::from_millis(500),
                jitter: 0.0,
                batch_size: 0,
                batch_pause: Duration::ZERO,
            },
            ..RunOptions::default()
        }
    }

    async fn drive(statuses: &[u16], n: usize, opts: RunOptions) -> (Report, State, Vec<String>) {
        let sender = ScriptedSender::new(statuses);
        let mut state = State::default();
        let targets = companies(n);
        let report = run(
            &template(),
            &targets,
            &mut state,
            &sender,
            &NoSleep,
            &opts,
            |_| {},
        )
        .await
        .unwrap();
        let seen = sender.seen();
        (report, state, seen)
    }

    #[tokio::test]
    async fn happy_path_unfollows_everyone_once() {
        let (report, state, seen) = drive(&[200, 200, 200], 3, fast_opts()).await;
        assert_eq!(report.succeeded, 3);
        assert_eq!(report.failed, 0);
        assert_eq!(report.requests_sent, 3);
        assert!(report.aborted.is_none());
        assert_eq!(state.done.len(), 3);
        // Each request carried that company's own URN.
        assert_eq!(seen[0], r#"{"urn":"urn:li:fsd_company:1"}"#);
        assert_eq!(seen[2], r#"{"urn":"urn:li:fsd_company:3"}"#);
    }

    #[tokio::test]
    async fn a_429_is_retried_and_can_still_succeed() {
        let (report, state, _) = drive(&[429, 429, 200], 1, fast_opts()).await;
        assert_eq!(report.succeeded, 1);
        assert_eq!(report.requests_sent, 3);
        assert!(state.is_done("urn:li:fsd_company:1"));
        assert!(state.failed.is_empty());
    }

    #[tokio::test]
    async fn retries_are_capped() {
        let opts = RunOptions {
            max_retries: 2,
            ..fast_opts()
        };
        let (report, state, _) = drive(&[429, 429, 429, 429, 429], 1, opts).await;
        assert_eq!(report.succeeded, 0);
        assert_eq!(report.failed, 1);
        assert_eq!(report.requests_sent, 3, "1 attempt + 2 retries");
        assert_eq!(state.failed[0].status, Some(429));
    }

    #[tokio::test]
    async fn a_403_aborts_the_entire_run_immediately() {
        let (report, state, _) = drive(&[403, 200, 200], 3, fast_opts()).await;
        assert_eq!(report.requests_sent, 1, "must not keep going after 403");
        assert_eq!(report.succeeded, 0);
        let aborted = report.aborted.expect("run should abort");
        assert!(aborted.contains("capture again"), "{aborted}");
        assert_eq!(state.done.len(), 0);
    }

    #[tokio::test]
    async fn a_login_redirect_aborts_too() {
        let (report, _, _) = drive(&[302], 2, fast_opts()).await;
        assert!(report.aborted.unwrap().contains("login page"));
    }

    #[tokio::test]
    async fn permanent_failures_move_on_without_retrying() {
        let (report, state, _) = drive(&[404, 200, 200], 3, fast_opts()).await;
        assert_eq!(report.requests_sent, 3, "404 is not retried");
        assert_eq!(report.succeeded, 2);
        assert_eq!(report.failed, 1);
        assert_eq!(state.failed[0].status, Some(404));
    }

    #[tokio::test]
    async fn three_failures_in_a_row_stop_the_run() {
        let opts = RunOptions {
            max_retries: 0,
            max_consecutive_failures: 3,
            ..fast_opts()
        };
        let (report, _, _) = drive(&[404, 404, 404, 200, 200], 5, opts).await;
        assert_eq!(report.attempted, 3);
        assert_eq!(report.requests_sent, 3);
        assert!(report.aborted.unwrap().contains("in a row"));
    }

    #[tokio::test]
    async fn a_success_resets_the_consecutive_failure_counter() {
        let opts = RunOptions {
            max_retries: 0,
            max_consecutive_failures: 3,
            ..fast_opts()
        };
        let (report, _, _) = drive(&[404, 404, 200, 404, 404, 200], 6, opts).await;
        assert!(report.aborted.is_none(), "{report:?}");
        assert_eq!(report.succeeded, 2);
        assert_eq!(report.failed, 4);
    }

    // ------------------------------------------------------------- planning --

    fn needs_urn() -> Vec<String> {
        vec!["urn".to_string()]
    }

    fn no_filters() -> PlanFilters<'static> {
        PlanFilters {
            only: None,
            except: None,
            max: None,
            retry_failed: false,
        }
    }

    #[test]
    fn plan_skips_companies_already_done() {
        let mut state = State::default();
        state.mark_done("urn:li:fsd_company:2");
        let p = plan(&companies(3), &state, &needs_urn(), &no_filters());
        assert_eq!(p.targets.len(), 2);
        assert_eq!(p.already_done, 1);
    }

    #[test]
    fn plan_applies_only_and_except() {
        let state = State::default();
        let only = Regex::new("co-1$").unwrap();
        let p = plan(
            &companies(3),
            &state,
            &needs_urn(),
            &PlanFilters {
                only: Some(&only),
                ..no_filters()
            },
        );
        assert_eq!(p.targets.len(), 1);
        assert_eq!(p.filtered_out, 2);

        let except = Regex::new("Co 1|Co 2").unwrap();
        let p = plan(
            &companies(3),
            &state,
            &needs_urn(),
            &PlanFilters {
                except: Some(&except),
                ..no_filters()
            },
        );
        assert_eq!(p.targets.len(), 1);
        assert_eq!(p.targets[0].label(), "Co 3");
    }

    #[test]
    fn plan_honours_max_and_reports_the_remainder() {
        let p = plan(
            &companies(10),
            &State::default(),
            &needs_urn(),
            &PlanFilters {
                max: Some(4),
                ..no_filters()
            },
        );
        assert_eq!(p.targets.len(), 4);
        assert_eq!(p.over_limit, 6);
    }

    #[test]
    fn plan_sets_aside_entries_the_template_cannot_fill() {
        let mut list = companies(2);
        list.push(Company {
            name: Some("No URN".into()),
            slug: Some("no-urn".into()),
            ..Default::default()
        });
        let p = plan(&list, &State::default(), &needs_urn(), &no_filters());
        assert_eq!(p.targets.len(), 2);
        assert_eq!(
            p.incomplete,
            vec![("No URN".to_string(), vec!["urn".to_string()])]
        );
    }

    #[test]
    fn plan_does_not_re_attempt_permanent_failures_unless_asked() {
        let mut state = State::default();
        state.mark_failed(Failure {
            key: "urn:li:fsd_company:1".into(),
            label: "Co 1".into(),
            status: Some(404),
            reason: "gone".into(),
        });
        state.mark_failed(Failure {
            key: "urn:li:fsd_company:2".into(),
            label: "Co 2".into(),
            status: Some(429),
            reason: "rate limited".into(),
        });

        let p = plan(&companies(3), &state, &needs_urn(), &no_filters());
        let labels: Vec<String> = p.targets.iter().map(|c| c.label()).collect();
        assert_eq!(
            labels,
            vec!["Co 2", "Co 3"],
            "a 429 is worth retrying, a 404 is not"
        );

        let p = plan(
            &companies(3),
            &state,
            &needs_urn(),
            &PlanFilters {
                retry_failed: true,
                ..no_filters()
            },
        );
        assert_eq!(p.targets.len(), 3);
    }

    #[test]
    fn classification_covers_the_statuses_that_matter() {
        assert_eq!(classify(200), Outcome::Ok);
        assert_eq!(classify(204), Outcome::Ok);
        assert!(matches!(classify(429), Outcome::Retry(_)));
        assert!(matches!(classify(999), Outcome::Retry(_)));
        assert!(matches!(classify(503), Outcome::Retry(_)));
        assert!(matches!(classify(401), Outcome::Fatal(_)));
        assert!(matches!(classify(403), Outcome::Fatal(_)));
        assert!(matches!(classify(302), Outcome::Fatal(_)));
        assert!(matches!(classify(404), Outcome::Permanent(_)));
        assert!(matches!(classify(422), Outcome::Permanent(_)));
        assert!(matches!(classify(418), Outcome::Permanent(_)));
    }
}
