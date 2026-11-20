//! The labelled lint sample (docs/lint-rules.md, "Measured precision") held against a fresh
//! `sous-chef lint --all` of the three live workspaces.
//!
//! `tests/fixtures/lint-precision/labels.tsv` labels findings TP (true positive), TBU (true
//! but unhelpful) or FP (false positive). The test re-lints Wrynose (`yocto-kiss/build`),
//! Scarthgap and Kirkstone (`releases/<r>/build`) with the CLI, as
//! `runs/lint-precision/run.sh` does, and fails when a TP or TBU finding is gone (a lost true
//! positive; a TBU's message may change, its rule and pointer may not) or a FP marked
//! `fixed = yes` is back. A FP marked `fixed = no` (a known false positive) is only reported
//! on stderr. Rows are keyed by (release, rule, pointer), never by message.
//!
//! `#[ignore]`, and passes without checking unless `SOUS_CHEF_LIVE` names the live
//! environment (docs/live-environment.md); a workspace whose build directory is missing is
//! skipped. Reads the workspaces only (no BitBake). The binary is the test build of
//! sous-chef, or `SOUS_CHEF_PRECISION_BIN` (a release build is about ten times faster):
//!
//! SOUS_CHEF_LIVE=~/Development/sous-chef-live cargo test --test lint_precision -- --ignored
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use common::fixture_path;
use serde_json::Value;

/// One labelled row.
struct Label {
    rule: String,
    release: String,
    pointer: String,
    label: String,
    fixed: String,
    reason: String,
}

fn labels() -> Vec<Label> {
    let text = fs::read_to_string(fixture_path("lint-precision/labels.tsv")).unwrap();
    let mut out = Vec::new();
    for line in text.lines() {
        if line.starts_with('#') || line.starts_with("rule\t") || line.trim().is_empty() {
            continue;
        }
        let c: Vec<&str> = line.split('\t').collect();
        assert!(c.len() >= 7, "labels.tsv: short row {line:?}");
        assert!(
            matches!(c[4], "TP" | "TBU" | "FP"),
            "labels.tsv: unknown label in {line:?}"
        );
        assert!(
            c[4] != "FP" || matches!(c[5], "yes" | "no"),
            "labels.tsv: a FP row needs fixed = yes or no: {line:?}"
        );
        out.push(Label {
            rule: c[0].to_string(),
            release: c[1].to_string(),
            pointer: c[2].to_string(),
            label: c[4].to_string(),
            fixed: c[5].to_string(),
            reason: c[6].to_string(),
        });
    }
    out
}

/// Build directories of the three workspaces, by release.
fn workspaces(live: &Path) -> BTreeMap<&'static str, PathBuf> {
    BTreeMap::from([
        ("wrynose", live.join("yocto-kiss/build")),
        ("scarthgap", live.join("releases/scarthgap/build")),
        ("kirkstone", live.join("releases/kirkstone/build")),
    ])
}

/// `(rule, pointer)` of every finding of `lint --all` on `build`.
fn lint_all(bin: &Path, build: &Path) -> BTreeSet<(String, String)> {
    let tmp = tempfile::tempdir().unwrap();
    let out = Command::new(bin)
        .args(["lint", "--all", "--format", "json", "--build-dir"])
        .arg(build)
        .current_dir(tmp.path())
        .env("XDG_CACHE_HOME", tmp.path().join("cache"))
        .output()
        .unwrap();
    // Exit status 1 is "a finding at or above --fail-on"; 2 is "lint could not run".
    assert!(
        out.status.code().is_some_and(|c| c < 2),
        "lint --all on {}: {:?}\n{}",
        build.display(),
        out.status,
        String::from_utf8_lossy(&out.stderr)
    );
    let v: Value = serde_json::from_slice(&out.stdout).unwrap();
    v["findings"]["rows"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| {
            (
                r[1].as_str().unwrap().to_string(),
                r[2].as_str().unwrap().to_string(),
            )
        })
        .collect()
}

#[test]
#[ignore = "needs SOUS_CHEF_LIVE"]
fn labelled_findings_hold_on_the_live_workspaces() {
    let labels = labels();
    assert!(labels.len() >= 150, "{} labels", labels.len());
    let Some(live) = std::env::var_os("SOUS_CHEF_LIVE").map(PathBuf::from) else {
        eprintln!("SOUS_CHEF_LIVE not set: skipping");
        return;
    };
    let bin = std::env::var_os("SOUS_CHEF_PRECISION_BIN")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(env!("CARGO_BIN_EXE_sous-chef")));
    let mut problems = Vec::new();
    let mut checked = 0;
    for (release, build) in workspaces(&live) {
        if !build.join("conf/bblayers.conf").is_file() {
            eprintln!("{release}: no {}: skipping", build.display());
            continue;
        }
        let found = lint_all(&bin, &build);
        for l in labels.iter().filter(|l| l.release == release) {
            checked += 1;
            let present = found.contains(&(l.rule.clone(), l.pointer.clone()));
            match (l.label.as_str(), l.fixed.as_str(), present) {
                ("TP" | "TBU", _, false) => problems.push(format!(
                    "{release} {} {}: labelled {} but no longer reported ({})",
                    l.rule, l.pointer, l.label, l.reason
                )),
                ("FP", "yes", true) => problems.push(format!(
                    "{release} {} {}: a fixed false positive is back ({})",
                    l.rule, l.pointer, l.reason
                )),
                ("FP", "no", false) => eprintln!(
                    "{release} {} {}: known false positive no longer reported: mark it fixed",
                    l.rule, l.pointer
                ),
                ("FP", "no", true) => eprintln!(
                    "{release} {} {}: known false positive still reported ({})",
                    l.rule, l.pointer, l.reason
                ),
                _ => {}
            }
        }
    }
    eprintln!("checked {checked} labelled findings");
    assert!(problems.is_empty(), "{}", problems.join("\n"));
}
