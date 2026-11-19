//! CI output of `sous-chef lint`: SARIF 2.1.0, GitHub annotations, fingerprints and the
//! baseline, on temporary copies of the fixture workspace (no git repository: paths are
//! relative to the workspace root, the working directory).

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::fs;
use std::path::{Path, PathBuf};

use common::*;
use serde_json::Value;
use sous_chef::config::Config;
use sous_chef::index::Index;
use sous_chef::lint::ci::{OutputFormat, escape_data, escape_property};
use sous_chef::lint::{self, LintCliArgs, Severity};
use sous_chef::workspace::Workspace;

struct Env {
    _tmp: tempfile::TempDir,
    _cache: tempfile::TempDir,
    root: PathBuf,
    config: Config,
    ws: Workspace,
    index: Index,
}

fn env() -> Env {
    let (tmp, root) = temp_ws();
    let root = fs::canonicalize(root).unwrap();
    let config = ws_config(&root);
    let ws = Workspace::discover(&config).unwrap();
    let cache = tempfile::tempdir().unwrap();
    let mut index = Index::open_in(cache.path(), &ws).unwrap();
    index.refresh(&ws).unwrap();
    Env {
        _tmp: tmp,
        _cache: cache,
        root,
        config,
        ws,
        index,
    }
}

impl Env {
    fn cli(&self, args: LintCliArgs) -> (i32, String) {
        let mut out = Vec::new();
        let code = lint::cli_with(&self.ws, &self.index, &self.config, &args, &mut out).unwrap();
        (code, String::from_utf8(out).unwrap())
    }

    fn try_cli(&self, args: LintCliArgs) -> Result<i32, String> {
        let mut out = Vec::new();
        lint::cli_with(&self.ws, &self.index, &self.config, &args, &mut out)
            .map_err(|e| e.to_string())
    }

    fn edit(&mut self, rel: &str, f: impl FnOnce(String) -> String) {
        let p = self.root.join(rel);
        let text = fs::read_to_string(&p).unwrap_or_default();
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        fs::write(&p, f(text)).unwrap();
        self.index.refresh(&self.ws).unwrap();
    }

    fn remove(&mut self, rel: &str) {
        fs::remove_file(self.root.join(rel)).unwrap();
        self.index.refresh(&self.ws).unwrap();
    }
}

fn sarif(e: &Env) -> Value {
    let (_, out) = e.cli(LintCliArgs {
        output: Some(OutputFormat::Sarif),
        ..LintCliArgs::default()
    });
    serde_json::from_str(&out).unwrap()
}

/// A structural check of the SARIF 2.1.0 log against the parts of the schema
/// (`sarif-schema-2.1.0.json`) and of GitHub's requirements that the output uses: the
/// required properties, their types, the `level` enumeration and the 1024-character
/// description limit. No JSON Schema validator is available without a new dependency.
fn check_sarif(v: &Value) {
    assert_eq!(v["version"], "2.1.0");
    assert_eq!(
        v["$schema"],
        "https://json.schemastore.org/sarif-2.1.0.json"
    );
    let runs = v["runs"].as_array().unwrap();
    assert_eq!(runs.len(), 1);
    let driver = &runs[0]["tool"]["driver"];
    assert_eq!(driver["name"], "sous-chef");
    assert!(driver["version"].is_string());
    assert!(driver["semanticVersion"].is_string());
    let rules = driver["rules"].as_array().unwrap();
    assert_eq!(rules.len(), lint::all_rules().len());
    for r in rules {
        assert!(r["id"].is_string());
        for k in ["shortDescription", "fullDescription", "help"] {
            let t = r[k]["text"].as_str().unwrap();
            assert!(
                !t.is_empty() && t.chars().count() <= 1024,
                "{k} of {}",
                r["id"]
            );
        }
        let level = r["defaultConfiguration"]["level"].as_str().unwrap();
        assert!(["error", "warning", "note"].contains(&level));
    }
    for res in runs[0]["results"].as_array().unwrap() {
        let id = res["ruleId"].as_str().unwrap();
        let idx = res["ruleIndex"].as_u64().unwrap() as usize;
        assert_eq!(rules[idx]["id"], id);
        assert!(["error", "warning", "note"].contains(&res["level"].as_str().unwrap()));
        assert!(!res["message"]["text"].as_str().unwrap().is_empty());
        let locs = res["locations"].as_array().unwrap();
        assert_eq!(locs.len(), 1);
        let phys = &locs[0]["physicalLocation"];
        let uri = phys["artifactLocation"]["uri"].as_str().unwrap();
        assert!(!uri.starts_with('/'), "{uri}");
        assert_eq!(phys["artifactLocation"]["uriBaseId"], "%SRCROOT%");
        let start = phys["region"]["startLine"].as_u64().unwrap();
        assert!(start >= 1);
        assert!(phys["region"]["endLine"].as_u64().unwrap() >= start);
        assert!(res["partialFingerprints"]["primaryLocationLineHash"].is_string());
    }
}

#[test]
fn sarif_on_the_fixture_workspace() {
    let e = env();
    let v = sarif(&e);
    check_sarif(&v);
    // The results (rules come from docs/lint-rules.md and are checked structurally).
    let results = serde_json::to_string_pretty(&v["runs"][0]["results"]).unwrap();
    insta::assert_snapshot!("ci_fixture_sarif_results", results);
    let ids: Vec<&str> = v["runs"][0]["tool"]["driver"]["rules"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r["id"].as_str().unwrap())
        .collect();
    let registry: Vec<&str> = lint::all_rules().iter().map(|r| r.id()).collect();
    assert_eq!(ids, registry);
    // No absolute path anywhere.
    assert!(
        !serde_json::to_string(&v)
            .unwrap()
            .contains(&e.root.display().to_string())
    );
}

#[test]
fn github_annotations_on_the_fixture_workspace() {
    let e = env();
    let (code, out) = e.cli(LintCliArgs {
        output: Some(OutputFormat::Github),
        ..LintCliArgs::default()
    });
    assert_eq!(code, 0);
    insta::assert_snapshot!("ci_fixture_github", out);
    for line in out.lines() {
        assert!(
            line.starts_with("::warning file=") || line.starts_with("::notice file="),
            "{line}"
        );
    }
}

#[test]
fn workflow_command_escaping() {
    assert_eq!(escape_data("a%b\r\nc:d,e"), "a%25b%0D%0Ac:d,e");
    assert_eq!(escape_property("a%b\r\nc:d,e"), "a%25b%0D%0Ac%3Ad%2Ce");
}

/// GitHub shows 10 annotations of each type per step: beyond that, one notice first says
/// how many findings of which type are only in the log.
#[test]
fn annotations_over_the_per_step_limit_are_announced() {
    use sous_chef::lint::Finding;
    use sous_chef::lint::ci::{ANNOTATIONS_PER_TYPE, annotation_overflow};
    use sous_chef::types::{LineSpan, Loc};
    let f = |severity, n: usize| Finding {
        rule: "hygiene/release-syntax",
        severity,
        loc: Loc {
            layer: "meta-mine".into(),
            path: PathBuf::from(format!("recipes/a/a_{n}.bb")),
            lines: LineSpan::line(1),
        },
        message: "m".into(),
    };
    let at_limit: Vec<Finding> = (0..ANNOTATIONS_PER_TYPE)
        .flat_map(|n| [f(Severity::Error, n), f(Severity::Warning, n)])
        .collect();
    assert_eq!(annotation_overflow(&at_limit), None);
    let mut over = at_limit;
    over.push(f(Severity::Warning, 99));
    let notice = annotation_overflow(&over).unwrap();
    assert!(
        notice.starts_with("::notice title=sous-chef lint::21 findings: 11 warnings; "),
        "{notice}"
    );
    assert!(
        notice.ends_with('\n') && notice.lines().count() == 1,
        "{notice}"
    );
    assert!(!notice.contains("errors"), "{notice}");
}

/// Fingerprint and start line of each SARIF result, by rule.
fn prints(v: &Value) -> Vec<(String, String, u64)> {
    v["runs"][0]["results"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| {
            (
                r["ruleId"].as_str().unwrap().to_string(),
                r["partialFingerprints"]["primaryLocationLineHash"]
                    .as_str()
                    .unwrap()
                    .to_string(),
                r["locations"][0]["physicalLocation"]["region"]["startLine"]
                    .as_u64()
                    .unwrap(),
            )
        })
        .collect()
}

#[test]
fn a_moved_line_keeps_its_fingerprint() {
    let mut e = env();
    let before = prints(&sarif(&e));
    let app = "meta-mine/recipes-app/app/app_0.1.bb";
    e.edit(app, |t| format!("\n{t}"));
    let after = prints(&sarif(&e));
    let find =
        |v: &[(String, String, u64)], rule: &str| v.iter().find(|x| x.0 == rule).cloned().unwrap();
    let (_, fp0, line0) = find(&before, "hygiene/release-syntax");
    let (_, fp1, line1) = find(&after, "hygiene/release-syntax");
    assert_eq!(line1, line0 + 1);
    assert_eq!(fp0, fp1);
    // Editing the statement itself gives a new fingerprint.
    e.edit(app, |t| {
        t.replace("-o app ${WORKDIR}/app.c", "-o app2 ${WORKDIR}/app.c")
    });
    let (_, fp2, _) = find(&prints(&sarif(&e)), "hygiene/release-syntax");
    assert_ne!(fp0, fp2);
}

#[test]
fn baseline_new_matched_stale_and_fail_on() {
    let mut e = env();
    let base = e.root.join("lint-baseline.json");
    let (code, out) = e.cli(LintCliArgs {
        write_baseline: Some(base.clone()),
        ..LintCliArgs::default()
    });
    assert_eq!(code, 0);
    assert!(out.starts_with("baseline "), "{out}");
    assert!(out.trim_end().ends_with("wrote 4 findings"), "{out}");
    let text = fs::read_to_string(&base).unwrap();
    // Deterministic and sorted.
    let (_, _) = e.cli(LintCliArgs {
        write_baseline: Some(e.root.join("again.json")),
        ..LintCliArgs::default()
    });
    assert_eq!(fs::read_to_string(e.root.join("again.json")).unwrap(), text);
    let v: Value = serde_json::from_str(&text).unwrap();
    assert_eq!(v["schema"], "sous-chef-lint-baseline/1");
    let files: Vec<&str> = v["findings"]
        .as_array()
        .unwrap()
        .iter()
        .map(|f| f["file"].as_str().unwrap())
        .collect();
    let mut sorted = files.clone();
    sorted.sort();
    assert_eq!(files, sorted);

    // Nothing new: 0 even with --fail-on warning (the fixture has warnings).
    let with = |fail_on: Option<Severity>| LintCliArgs {
        baseline: Some(base.clone()),
        fail_on,
        ..LintCliArgs::default()
    };
    let (code, out) = e.cli(with(Some(Severity::Warning)));
    assert_eq!(code, 0, "{out}");
    assert!(out.starts_with("no findings"), "{out}");
    assert!(out.contains("4 matched, 0 stale entries"), "{out}");
    let (code, _) = e.cli(LintCliArgs {
        fail_on: Some(Severity::Warning),
        ..LintCliArgs::default()
    });
    assert_eq!(code, 1, "without the baseline the warnings fail");

    // A new error fails; the baselined warnings still do not count.
    e.edit("meta-mine/recipes-app/old/old_1.0.bb", |_| {
        "SUMMARY = \"x\"\nLICENSE = \"MIT\"\nPNBLACKLIST[old] = \"broken\"\n".into()
    });
    let (code, out) = e.cli(with(None));
    assert_eq!(code, 1, "{out}");
    assert!(out.starts_with("1 finding in"), "{out}");
    assert!(out.contains("hygiene/release-removed"), "{out}");
    assert!(
        out.contains("1 new finding, 4 matched, 0 stale entries"),
        "{out}"
    );

    // A baselined finding that no longer occurs is stale.
    e.remove("meta-mine/recipes-app/orphan/orphan_9.%.bbappend");
    let (_, out) = e.cli(with(None));
    assert!(
        out.contains("1 new finding, 3 matched, 1 stale entry"),
        "{out}"
    );

    // With --fail-on error a new warning does not fail.
    e.edit("meta-mine/recipes-app/old/old_1.0.bb", |_| {
        "SUMMARY = \"x\"\nLICENSE = \"MIT\"\nSERIAL_CONSOLE = \"115200 ttyS0\"\n".into()
    });
    let (code, out) = e.cli(with(None));
    assert_eq!(code, 0, "{out}");
    let (code, _) = e.cli(with(Some(Severity::Warning)));
    assert_eq!(code, 1);

    // SARIF with a baseline lists only the new finding.
    let (_, out) = e.cli(LintCliArgs {
        baseline: Some(base.clone()),
        output: Some(OutputFormat::Sarif),
        ..LintCliArgs::default()
    });
    let v: Value = serde_json::from_str(&out).unwrap();
    assert_eq!(v["runs"][0]["results"].as_array().unwrap().len(), 1);
}

#[test]
fn a_missing_or_malformed_baseline_is_an_error() {
    let e = env();
    let missing = e.root.join("nope.json");
    let err = e
        .try_cli(LintCliArgs {
            baseline: Some(missing),
            ..LintCliArgs::default()
        })
        .unwrap_err();
    assert!(
        err.contains("baseline") && err.contains("nope.json"),
        "{err}"
    );
    let bad = e.root.join("bad.json");
    fs::write(&bad, "{\"findings\": 3}").unwrap();
    let err = e
        .try_cli(LintCliArgs {
            baseline: Some(bad),
            ..LintCliArgs::default()
        })
        .unwrap_err();
    assert!(err.contains("not a sous-chef lint baseline"), "{err}");
    let other = e.root.join("other.json");
    fs::write(&other, "{\"schema\": \"x/9\", \"findings\": []}").unwrap();
    let err = e
        .try_cli(LintCliArgs {
            baseline: Some(other),
            ..LintCliArgs::default()
        })
        .unwrap_err();
    assert!(err.contains("schema"), "{err}");
}

#[test]
fn the_binary_takes_the_new_formats() {
    let e = env();
    let bin = env!("CARGO_BIN_EXE_sous-chef");
    let cache = tempfile::tempdir().unwrap();
    let run = |args: &[&str]| {
        std::process::Command::new(bin)
            .args(args)
            .current_dir(&e.root)
            .env("XDG_CACHE_HOME", cache.path())
            .output()
            .unwrap()
    };
    let out = run(&["lint", "--format", "sarif"]);
    assert_eq!(out.status.code(), Some(0));
    let v: Value = serde_json::from_slice(&out.stdout).unwrap();
    check_sarif(&v);
    assert!(String::from_utf8_lossy(&out.stderr).contains("4 findings in 6 files"));
    let out = run(&["lint", "--format", "json"]);
    let v: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(v["warnings"], 3);
    let out = run(&["lint", "--baseline", "missing.json"]);
    assert_eq!(out.status.code(), Some(2));
    let out = run(&["lint", "--migrate-to", "wrynose", "--format", "sarif"]);
    assert_eq!(out.status.code(), Some(2));
    let _ = Path::new("");
}
