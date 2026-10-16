//! `aibrowse eval detectors`: the detector harness (design.md §5.9).
//!
//! Two halves, reported together:
//! - public bot-detector pages (sannysoft, CreepJS, BrowserScan, ...), each visited once
//!   and read into a structured verdict, for fingerprint and automation tells;
//! - a local raw event logger, driven through the real humanized input paths, for
//!   behavioral tells a detector could compute from the events themselves.
//!
//! Run it once headless and once `--headful` and compare: the two must not differ.

mod analysis;
mod clicks;
mod event_log;
mod keyboard;
mod persona_match;
mod pointer;
mod sites;
mod wheel;

use std::fmt::Write as _;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use aibrowse_browser::{Browser, LaunchOptions};
use anyhow::{Context, bail};
use serde::Serialize;

use analysis::Severity;
use event_log::EventLogReport;
use sites::SiteReport;

const USAGE: &str =
    "usage: aibrowse eval detectors [--headful] [--profile NAME] [--local] [--sites a,b]";

struct Options {
    profile: String,
    headless: bool,
    /// Only the local event logger: no internet needed.
    local: bool,
    sites: Option<Vec<String>>,
}

#[derive(Serialize)]
struct Report {
    mode: &'static str,
    profile: String,
    user_agent: String,
    sites: Vec<SiteReport>,
    #[serde(skip_serializing_if = "Option::is_none")]
    event_log: Option<EventLogReport>,
    #[serde(skip_serializing_if = "Option::is_none")]
    event_log_error: Option<String>,
}

pub async fn run(args: &[String]) -> anyhow::Result<()> {
    let options = parse(args)?;
    let browser = Browser::launch(LaunchOptions {
        profile: options.profile.clone(),
        headless: options.headless,
        mute: true,
        ..LaunchOptions::default()
    })
    .await?;
    let user_agent = browser.user_agent().await.unwrap_or_default();

    // The local logger goes first: it is the part that can't be disturbed by a slow or
    // changed third-party page.
    let (event_log, event_log_error) = match event_log::run(&browser).await {
        Ok(report) => (Some(report), None),
        Err(err) => (None, Some(format!("{err:#}"))),
    };
    let mut reports = Vec::new();
    if !options.local {
        let mut tab = browser.open_tab().await?;
        for site in sites::all() {
            if options
                .sites
                .as_ref()
                .is_some_and(|names| !names.iter().any(|n| n == site.name))
            {
                continue;
            }
            let report = sites::visit(&mut tab, &site).await;
            eprintln!(
                "{:<22} {:<9} {}",
                report.name, report.verdict, report.summary
            );
            reports.push(report);
        }
    }
    browser.close().await?;

    let report = Report {
        mode: if options.headless {
            "headless"
        } else {
            "headful"
        },
        profile: options.profile,
        user_agent,
        sites: reports,
        event_log,
        event_log_error,
    };
    println!("{}", summarize(&report));
    let path = report_path()?;
    std::fs::write(&path, serde_json::to_vec_pretty(&report)?)?;
    println!("Report: {}", path.display());
    Ok(())
}

fn parse(args: &[String]) -> anyhow::Result<Options> {
    let mut options = Options {
        profile: "detectors".into(),
        headless: true,
        local: false,
        sites: None,
    };
    let mut iter = args.iter();
    while let Some(arg) = iter.next() {
        let mut value = || {
            iter.next()
                .cloned()
                .with_context(|| format!("{arg} needs a value"))
        };
        match arg.as_str() {
            "--profile" => options.profile = value()?,
            "--headful" => options.headless = false,
            "--local" => options.local = true,
            "--sites" => options.sites = Some(value()?.split(',').map(str::to_owned).collect()),
            other => bail!("unknown option {other:?}\n{USAGE}"),
        }
    }
    Ok(options)
}

/// The human-readable table: one line per detector, then the event logger's numbers and
/// every finding.
fn summarize(report: &Report) -> String {
    let mut out = format!(
        "Detector eval ({}, profile {}, {})\n",
        report.mode, report.profile, report.user_agent
    );
    for site in &report.sites {
        let _ = writeln!(
            out,
            "  {:<22} {:<9} {}",
            site.name, site.verdict, site.summary
        );
    }
    match (&report.event_log, &report.event_log_error) {
        (Some(log), _) => {
            let a = &log.analysis;
            let (p, k, w) = (&a.pointer, &a.keyboard, &a.wheel);
            let _ = writeln!(
                out,
                "Event logger: {} events — {} tells, {} outliers",
                log.events.len(),
                a.tells(),
                a.outliers()
            );
            let _ = writeln!(
                out,
                "  pointer  {} strokes, straightness {:.2}, max deviation {:.1}px, speed CV {:.2}, peak at {:.0}%, {:.0}ms per stroke, moves every {:.1}ms (CV {:.2}), screen offset {:?}",
                p.strokes,
                p.straightness_median,
                p.deviation_px_max,
                p.speed_cv_median,
                p.peak_at_median * 100.0,
                p.duration_ms_median,
                p.move_interval_ms_median,
                p.move_interval_cv,
                p.screen_offset
            );
            let _ = writeln!(
                out,
                "  clicks   {} clicks, dwell {:.0}ms, press {:.0}ms (min {:.0}ms)",
                p.clicks, p.dwell_ms_median, p.press_ms_median, p.press_ms_min
            );
            let _ = writeln!(
                out,
                "  keys     {} keystrokes, hold {:.0}ms (CV {:.2}), inter-key {:.0}ms (CV {:.2}), {:.0} WPM",
                k.keystrokes, k.hold_ms_median, k.hold_cv, k.iki_ms_median, k.iki_cv, k.wpm
            );
            let _ = writeln!(
                out,
                "  wheel    {} ticks in {} gestures, looks like a {} (persona: {}), {} distinct deltas, median {:.0}px, every {:.1}ms (CV {:.2})",
                w.ticks,
                w.gestures,
                w.device,
                log.persona_device,
                w.distinct_deltas,
                w.delta_px_median,
                w.interval_ms_median,
                w.interval_cv
            );
            let _ = writeln!(
                out,
                "  persona  measured (persona): {}",
                persona_match::compare(&log.persona, &log.kinematics)
            );
            for finding in &a.findings {
                let tag = match finding.severity {
                    Severity::Tell => "TELL",
                    Severity::Outlier => "outlier",
                };
                let _ = writeln!(out, "  {tag:<8} {}: {}", finding.check, finding.detail);
            }
        }
        (None, Some(err)) => {
            let _ = writeln!(out, "Event logger failed: {err}");
        }
        (None, None) => {}
    }
    out.trim_end().to_owned()
}

fn report_path() -> anyhow::Result<PathBuf> {
    let dir = dirs::home_dir()
        .context("no home directory")?
        .join(".aibrowse/evals");
    std::fs::create_dir_all(&dir)?;
    let stamp = SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs();
    Ok(dir.join(format!("detectors-{stamp}.json")))
}
