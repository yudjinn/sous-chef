//! The `sous-chef lint` options for CI and for migrations, which change what the command
//! prints or which findings count rather than what it checks: `--format sarif|github`
//! ([`super::sarif`], [`annotations`]), `--baseline` / `--write-baseline`
//! ([`super::baseline`]) and `--migrate-to` ([`super::migrate`]). See docs/ci.md.
//!
//! [`dispatch`] is called by [`super::cli_with`] once the target and the options are known;
//! it returns `None` when none of these options is set (`--format` not given on the command
//! line either), and the plain report is printed as before.
//!
//! Exit status, every format: 1 when a finding that counts (with `--baseline`: a new one) is
//! at or above `--fail-on` (default `error`), 0 otherwise; `--write-baseline` exits 0; an
//! error (bad target, missing or malformed baseline) is 2 (`main.rs`).

use std::fs;
use std::io::Write;

use super::{
    LintCliArgs, Options, Report, Severity, Target, baseline, exit_code, migrate, render_report,
    run, sarif,
};
use crate::config::Config;
use crate::error::{Error, Result};
use crate::index::Index;
use crate::release::Release;
use crate::render::Format;
use crate::workspace::Workspace;

/// `--format` of `sous-chef lint`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, clap::ValueEnum)]
pub enum OutputFormat {
    /// The report as text (summary line, findings table, notes).
    #[default]
    Text,
    /// The same as JSON.
    Json,
    /// SARIF 2.1.0 for GitHub code scanning (and other SARIF consumers).
    Sarif,
    /// GitHub Actions workflow commands (`::error file=...,line=...::message`).
    Github,
}

impl OutputFormat {
    fn from_format(f: Format) -> OutputFormat {
        match f {
            Format::Text => OutputFormat::Text,
            Format::Json => OutputFormat::Json,
        }
    }
}

/// The output format: `--format` from the command line, else [`LintCliArgs::format`] (the
/// library field), else text.
pub fn output_format(args: &LintCliArgs) -> OutputFormat {
    args.output
        .or(args.format.map(OutputFormat::from_format))
        .unwrap_or_default()
}

/// Escapes the message of a workflow command (`escapeData` in `@actions/core`).
pub fn escape_data(s: &str) -> String {
    s.replace('%', "%25")
        .replace('\r', "%0D")
        .replace('\n', "%0A")
}

/// Escapes a property value of a workflow command (`escapeProperty` in `@actions/core`).
pub fn escape_property(s: &str) -> String {
    escape_data(s).replace(':', "%3A").replace(',', "%2C")
}

/// GitHub Actions shows at most this many annotations of each type (error, warning,
/// notice) per step, and 50 per job; the rest are dropped silently (actions/toolkit,
/// docs/problem-matchers.md, "Limitations"). Every command stays in the step's log.
pub const ANNOTATIONS_PER_TYPE: usize = 10;

/// The notice printed before the findings when some will not be shown as annotations:
/// `None` when every type has at most [`ANNOTATIONS_PER_TYPE`] findings.
pub fn annotation_overflow(findings: &[super::Finding]) -> Option<String> {
    let count = |s: Severity| findings.iter().filter(|f| f.severity == s).count();
    let over: Vec<String> = [
        (Severity::Error, "error"),
        (Severity::Warning, "warning"),
        (Severity::Info, "notice"),
    ]
    .iter()
    .filter_map(|&(s, name)| {
        let n = count(s);
        (n > ANNOTATIONS_PER_TYPE).then(|| format!("{n} {name}s"))
    })
    .collect();
    if over.is_empty() {
        return None;
    }
    Some(format!(
        "::notice title={}::{}\n",
        escape_property("sous-chef lint"),
        escape_data(&format!(
            "{} findings: {}; GitHub shows {ANNOTATIONS_PER_TYPE} annotations of each type per \
             step (50 per job), so some are only in this step's log. Use --format sarif for \
             code scanning, or a baseline to report only new findings.",
            findings.len(),
            over.join(", ")
        ))
    ))
}

/// One workflow command per finding: `::error` / `::warning` / `::notice` (info) with
/// `file` (relative to the repository root), `line`, `endLine` and `title` (the rule id),
/// after the [`annotation_overflow`] notice when there is one.
pub fn annotations(
    ws: &Workspace,
    paths: &sarif::RepoPaths,
    findings: &[super::Finding],
) -> String {
    let mut out = annotation_overflow(findings).unwrap_or_default();
    for f in findings {
        let cmd = match f.severity {
            Severity::Error => "error",
            Severity::Warning => "warning",
            Severity::Info => "notice",
        };
        let file = paths
            .relative(ws, f)
            .or_else(|| paths.absolute(ws, f).map(|p| p.display().to_string()))
            .unwrap_or_else(|| f.loc.path.display().to_string());
        out.push_str(&format!(
            "::{cmd} file={},line={},endLine={},title={}::{}\n",
            escape_property(&file),
            f.loc.lines.start.max(1),
            f.loc.lines.end.max(f.loc.lines.start).max(1),
            escape_property(f.rule),
            escape_data(&f.message)
        ));
    }
    out
}

/// Handles `--migrate-to`, `--format sarif|github`, `--baseline` and `--write-baseline`;
/// `None` when none is set and `--format` was not given on the command line.
pub fn dispatch(
    ws: &Workspace,
    index: &Index,
    cfg: &Config,
    args: &LintCliArgs,
    target: &Target,
    options: &Options,
    out: &mut dyn Write,
) -> Result<Option<i32>> {
    let fail_on = args.fail_on.unwrap_or(Severity::Error);
    let format = output_format(args);
    if let Some(to) = args.migrate_to.as_deref() {
        let to = Release::parse_arg(to, "--migrate-to")?;
        let as_format = match format {
            OutputFormat::Text => Format::Text,
            OutputFormat::Json => Format::Json,
            OutputFormat::Sarif | OutputFormat::Github => {
                return Err(Error::Invalid {
                    msg: "--migrate-to prints text or json; --format sarif and github are for \
                          lint findings"
                        .into(),
                });
            }
        };
        let m = migrate::report(ws, index, cfg, target, options, to)?;
        let text = migrate::render(&m, as_format);
        out.write_all(text.as_bytes())?;
        out.write_all(b"\n")?;
        return Ok(Some(migrate::exit_code(&m, fail_on)));
    }
    if args.output.is_none() && args.baseline.is_none() && args.write_baseline.is_none() {
        return Ok(None);
    }
    let mut report = run(ws, index, cfg, target, options)?;
    if let Some(path) = &args.write_baseline {
        let b = baseline::build(ws, &report.findings);
        fs::write(path, baseline::to_text(&b)).map_err(|e| Error::Invalid {
            msg: format!("writing baseline {}: {e}", path.display()),
        })?;
        out.write_all(
            format!(
                "baseline {}: wrote {} {}\n",
                path.display(),
                b.findings.len(),
                if b.findings.len() == 1 {
                    "finding"
                } else {
                    "findings"
                }
            )
            .as_bytes(),
        )?;
        return Ok(Some(0));
    }
    let mut summary_note = None;
    if let Some(path) = &args.baseline {
        let b = baseline::read(path)?;
        let cmp = baseline::compare(ws, &report.findings, &b);
        summary_note = Some(cmp.summary(path));
        report.findings = cmp.new;
    }
    let code = exit_code(&report.findings, fail_on);
    match format {
        OutputFormat::Text | OutputFormat::Json => {
            if let Some(n) = summary_note {
                report.notes.push(n);
            }
            let f = if format == OutputFormat::Json {
                Format::Json
            } else {
                Format::Text
            };
            let text = render_report(&report, f, usize::MAX);
            out.write_all(text.as_bytes())?;
            if !text.ends_with('\n') {
                out.write_all(b"\n")?;
            }
        }
        OutputFormat::Sarif | OutputFormat::Github => {
            let paths = sarif::RepoPaths::new(&cfg.work_dir);
            let text = if format == OutputFormat::Sarif {
                // Fingerprints of every finding (occurrence numbers count them all), then
                // at most GitHub's limit of results, the most severe.
                let fps = baseline::fingerprints(ws, &report.findings);
                let sev: Vec<Severity> = report.findings.iter().map(|f| f.severity).collect();
                let keep = sarif::keep_most_severe(&sev, sarif::MAX_RESULTS);
                if keep.len() < report.findings.len() {
                    report.notes.push(format!(
                        "SARIF: {} of {} findings written, the most severe (GitHub code \
                         scanning rejects more than {} results per run)",
                        keep.len(),
                        report.findings.len(),
                        sarif::MAX_RESULTS
                    ));
                }
                let findings: Vec<super::Finding> =
                    keep.iter().map(|&i| report.findings[i].clone()).collect();
                let fps: Vec<String> = keep.iter().map(|&i| fps[i].clone()).collect();
                let mut t = sarif::render(ws, cfg, &paths, &findings, &fps);
                t.push('\n');
                t
            } else {
                annotations(ws, &paths, &report.findings)
            };
            out.write_all(text.as_bytes())?;
            // The summary and notes go to stderr: stdout is the SARIF log or the commands.
            report_to_stderr(&report, summary_note.as_deref());
        }
    }
    Ok(Some(code))
}

/// The summary line, the notes and the baseline line on stderr (SARIF and annotations).
fn report_to_stderr(report: &Report, baseline_note: Option<&str>) {
    eprintln!("sous-chef lint: {}", super::summary_line(report));
    for n in &report.notes {
        eprintln!("sous-chef lint: note: {n}");
    }
    if let Some(n) = baseline_note {
        eprintln!("sous-chef lint: {n}");
    }
}
