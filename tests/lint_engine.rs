//! T4.1: the lint engine (targets, rule selection, releases, severity overrides,
//! suppression, sorting, exit codes), the sample rule `hygiene/parse-error`, and the
//! scaffold post-write lint, on a temporary copy of the fixture workspace.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::fs;
use std::ops::RangeInclusive;
use std::path::{Path, PathBuf};
use std::process::Command;

use common::*;
use sous_chef::config::Config;
use sous_chef::error::{Error, Result};
use sous_chef::eval::{self, DataStore};
use sous_chef::index::Index;
use sous_chef::lint::rules::hygiene_parse_error::HygieneParseError;
use sous_chef::lint::{
    self, Evaluate, Finding, LintCliArgs, LintContext, Options, Replay, Report, Rule, Scope,
    Severity, Target,
};
use sous_chef::release::Release;
use sous_chef::scaffold::{self, FileAction, GenContext, Generator, Kind, Plan, PlannedFile};
use sous_chef::types::{FileKind, LineSpan, Loc};
use sous_chef::workspace::Workspace;

// ---------------------------------------------------------------------------------------
// Harness

struct Env {
    _tmp: tempfile::TempDir,
    _cache: tempfile::TempDir,
    root: PathBuf,
    config: Config,
    ws: Workspace,
    index: Index,
}

impl Env {
    fn new() -> Env {
        Env::with(|_| {})
    }

    fn with(edit: impl FnOnce(&mut Config)) -> Env {
        let (tmp, root) = temp_ws();
        let root = fs::canonicalize(root).unwrap();
        let cache = tempfile::tempdir().unwrap();
        let mut config = ws_config(&root);
        edit(&mut config);
        let ws = Workspace::discover(&config).unwrap();
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

    fn add(&mut self, rel: &str, content: &str) -> PathBuf {
        let p = self.root.join(rel);
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        fs::write(&p, content).unwrap();
        self.index.refresh(&self.ws).unwrap();
        p
    }

    fn run(&self, target: &Target, options: &Options, rules: &[&dyn Rule]) -> Report {
        lint::run_with(
            &self.ws,
            &self.index,
            &self.config,
            target,
            options,
            rules,
            &NoEval,
        )
        .unwrap()
    }

    fn run_all(&self, target: &Target) -> Report {
        lint::run(
            &self.ws,
            &self.index,
            &self.config,
            target,
            &Options::default(),
        )
        .unwrap()
    }

    fn resolve(&self, text: &str) -> Result<Target> {
        lint::resolve_target(&self.ws, &self.index, Some(text), &self.root)
    }
}

/// No evaluator.
struct NoEval;

impl Evaluate for NoEval {
    fn evaluate(&self, _: &Index, _: &Workspace, _: &eval::Target) -> Result<Option<DataStore>> {
        Ok(None)
    }
}

/// An evaluator that returns an empty datastore for every recipe except `app`, which fails.
/// The engine names each recipe by its `layer:path`.
struct FakeEval;

impl Evaluate for FakeEval {
    fn evaluate(&self, _: &Index, _: &Workspace, t: &eval::Target) -> Result<Option<DataStore>> {
        assert!(t.version.is_none(), "{t:?}");
        if t.recipe.as_deref() == Some("meta-mine:recipes-app/app/app_0.1.bb") {
            return Err(Error::Invalid {
                msg: "replay exploded".into(),
            });
        }
        Ok(Some(DataStore::new()))
    }
}

type CheckFn = fn(&TestRule, &LintContext<'_>, &mut Vec<Finding>);

struct TestRule {
    id: &'static str,
    scope: Scope,
    severity: Severity,
    range: RangeInclusive<Release>,
    enabled: bool,
    check: CheckFn,
}

impl TestRule {
    const fn file(id: &'static str, check: CheckFn) -> TestRule {
        TestRule {
            id,
            scope: Scope::File,
            severity: Severity::Warning,
            range: Release::FLOOR..=Release::NEWEST,
            enabled: true,
            check,
        }
    }
}

impl Rule for TestRule {
    fn id(&self) -> &'static str {
        self.id
    }
    fn scope(&self) -> Scope {
        self.scope
    }
    fn default_severity(&self) -> Severity {
        self.severity
    }
    fn check(&self, cx: &LintContext<'_>, out: &mut Vec<Finding>) {
        (self.check)(self, cx, out)
    }
    fn releases(&self) -> RangeInclusive<Release> {
        self.range.clone()
    }
    fn default_enabled(&self) -> bool {
        self.enabled
    }
}

/// One finding on line 1 of every unit, naming the releases it was checked against.
fn line_one(r: &TestRule, cx: &LintContext<'_>, out: &mut Vec<Finding>) {
    let names: Vec<&str> = cx.releases.iter().map(|r| r.codename()).collect();
    out.push(Finding {
        rule: r.id,
        severity: r.severity,
        loc: Loc {
            lines: LineSpan::line(1),
            ..cx.loc.clone()
        },
        message: names.join(" "),
    });
}

/// A finding on every line containing `LINTME`.
fn flagged_lines(r: &TestRule, cx: &LintContext<'_>, out: &mut Vec<Finding>) {
    for (i, l) in cx.source.unwrap_or("").lines().enumerate() {
        if l.contains("LINTME") {
            out.push(Finding {
                rule: r.id,
                severity: r.severity,
                loc: Loc {
                    lines: LineSpan::line(i as u32 + 1),
                    ..cx.loc.clone()
                },
                message: "flagged".into(),
            });
        }
    }
}

fn rows(report: &Report) -> Vec<String> {
    report
        .findings
        .iter()
        .map(|f| format!("{} {} {}", f.severity.as_str(), f.rule, f.loc))
        .collect()
}

/// [`rows`] of the findings of one rule only. Tests of the engine through the real rule set
/// look at `hygiene/parse-error` alone, so rules that also fire on the fixture workspace
/// (`app_0.1.bb` has findings of its own) do not change them.
fn rows_of(report: &Report, rule: &str) -> Vec<String> {
    report
        .findings
        .iter()
        .filter(|f| f.rule == rule)
        .map(|f| format!("{} {} {}", f.severity.as_str(), f.rule, f.loc))
        .collect()
}

const PE: &str = "hygiene/parse-error";

fn opts(rules: &[&str]) -> Options {
    Options {
        rules: rules.iter().map(|s| s.to_string()).collect(),
        ..Options::default()
    }
}

const GARBAGE: &str = "SUMMARY = \"x\"\nthis is not bitbake\nLICENSE = \"MIT\"\n";

// ---------------------------------------------------------------------------------------
// hygiene/parse-error fixtures

fn fixture_findings(name: &str) -> Vec<(u32, Severity, String)> {
    let env = Env::new();
    let path = fixture_path(&format!("lint/hygiene/parse-error/{name}"));
    let text = fs::read_to_string(&path).unwrap();
    let loc = Loc {
        layer: "fixture".into(),
        path: PathBuf::from(name),
        lines: LineSpan::line(1),
    };
    lint::check_text(
        &HygieneParseError,
        &env.ws,
        &env.index,
        loc,
        &text,
        FileKind::from_path(Path::new(name)),
        &[],
        None,
    )
    .into_iter()
    .map(|f| {
        assert_eq!(f.rule, "hygiene/parse-error");
        assert!(!f.message.contains('\n'), "{}", f.message);
        (f.loc.lines.start, f.severity, f.message)
    })
    .collect()
}

#[test]
fn parse_error_bad_recipe() {
    let got = fixture_findings("bad.bb");
    let lines: Vec<(u32, Severity)> = got.iter().map(|(l, s, _)| (*l, *s)).collect();
    assert_eq!(
        lines,
        [
            (2, Severity::Error),   // indented comment
            (3, Severity::Error),   // unparsed line
            (4, Severity::Warning), // addtask groups glued
            (5, Severity::Error),   // addpylib outside a .conf
            (7, Severity::Error),   // function never closed
        ],
        "{got:#?}"
    );
    assert!(
        got[0].2.contains("start the comment in column 0"),
        "{got:#?}"
    );
    assert!(got[2].2.contains("write all `after` tasks in one group"));
    assert!(got[4].2.contains("close the function with `}`"));
}

#[test]
fn parse_error_bad_conf() {
    let got = fixture_findings("bad.conf");
    let lines: Vec<(u32, Severity)> = got.iter().map(|(l, s, _)| (*l, *s)).collect();
    assert_eq!(
        lines,
        [(2, Severity::Error), (3, Severity::Error)],
        "{got:#?}"
    );
    assert!(
        got[0].2.starts_with("`inherit` in a .conf file"),
        "{got:#?}"
    );
    assert!(
        got[1].2.starts_with("a function in a .conf file"),
        "{got:#?}"
    );
}

#[test]
fn parse_error_good_files_have_no_findings() {
    assert_eq!(fixture_findings("good.bb"), []);
    assert_eq!(fixture_findings("good.conf"), []);
    // `addpylib` in a layer's conf/layer.conf: base configuration, on a release with it.
    assert_eq!(fixture_findings("conf/layer.conf"), []);
}

/// `hygiene/parse-error` on `text` as the file `path` of a layer, checked against
/// `releases`.
fn parse_error_at(path: &str, text: &str, releases: &[Release]) -> Vec<(u32, Severity, String)> {
    let env = Env::new();
    let loc = Loc {
        layer: "fixture".into(),
        path: PathBuf::from(path),
        lines: LineSpan::line(1),
    };
    lint::check_text(
        &HygieneParseError,
        &env.ws,
        &env.index,
        loc,
        text,
        FileKind::from_path(Path::new(path)),
        releases,
        None,
    )
    .into_iter()
    .map(|f| (f.loc.lines.start, f.severity, f.message))
    .collect()
}

#[test]
fn parse_error_judges_addpylib_by_place_and_release() {
    let line = "A = \"1\"\naddpylib ${LAYERDIR}/lib oe\n";
    // Base configuration (cookerdata `parse_config_file`): accepted from BitBake 2.4 on.
    for base in ["conf/layer.conf", "conf/bitbake.conf", "conf/bblayers.conf"] {
        assert_eq!(
            parse_error_at(base, line, &[Release::Wrynose]),
            [],
            "{base}"
        );
        assert_eq!(
            parse_error_at(base, line, &[Release::Mickledore]),
            [],
            "{base}"
        );
    }
    // Kirkstone and Langdale (BitBake 2.0, 2.2) have no such pattern: "unparsed line".
    let old = parse_error_at(
        "conf/layer.conf",
        line,
        &[Release::Kirkstone, Release::Scarthgap],
    );
    assert_eq!(old.len(), 1, "{old:#?}");
    assert_eq!((old[0].0, old[0].1), (2, Severity::Error));
    assert!(old[0].2.contains("not available in kirkstone,"), "{old:#?}");
    // Anywhere else BitBake rejects it on every release: a file the base configuration
    // includes, a machine conf, a recipe, a class, an `.inc`.
    for other in [
        "conf/distro/mine.conf",
        "conf/machine/m.conf",
        "conf/local.conf",
        "conf/layer-extra.conf",
        "recipes-a/a/a_1.0.bb",
        "classes/c.bbclass",
        "recipes-a/a/a.inc",
    ] {
        let got = parse_error_at(other, line, &[Release::Wrynose]);
        assert_eq!(got.len(), 1, "{other}: {got:#?}");
        assert_eq!((got[0].0, got[0].1), (2, Severity::Error), "{other}");
        assert!(
            got[0].2.contains("only in the base configuration"),
            "{other}: {got:#?}"
        );
    }
    // One argument matches no release's pattern.
    let got = parse_error_at("conf/layer.conf", "addpylib ${LAYERDIR}/lib\n", &[]);
    assert_eq!(got.len(), 1, "{got:#?}");
    assert!(got[0].2.starts_with("unparsed line"), "{got:#?}");
}

#[test]
fn parse_error_skips_whitespace_after_backslash_but_the_parser_still_reports_it() {
    // The rule relies on the parser's message text; this pins it.
    let text = fs::read_to_string(fixture_path("lint/hygiene/parse-error/good.bb")).unwrap();
    let parsed = sous_chef::parse::parse(&text);
    assert!(
        parsed
            .diagnostics
            .iter()
            .any(|d| d.message.starts_with("whitespace after `\\`") && d.span.start == 4),
        "{:?}",
        parsed.diagnostics
    );
    let conf = fs::read_to_string(fixture_path("lint/hygiene/parse-error/good.conf")).unwrap();
    let parsed = sous_chef::parse::parse(&conf);
    let msgs: Vec<&str> = parsed
        .diagnostics
        .iter()
        .map(|d| d.message.as_str())
        .collect();
    // `addfragments` is a statement kind of its own: no diagnostic, nothing to skip.
    assert!(!msgs.iter().any(|m| m.contains("addfragments")), "{msgs:?}");
    assert!(
        parsed
            .statements
            .iter()
            .any(|s| matches!(s.kind, sous_chef::parse::StatementKind::AddFragments { .. }))
    );
}

// ---------------------------------------------------------------------------------------
// Targets

#[test]
fn target_resolution() {
    let env = Env::new();
    let root = &env.root;
    assert_eq!(
        lint::resolve_target(&env.ws, &env.index, None, root).unwrap(),
        Target::Workspace
    );
    assert_eq!(
        env.resolve("meta-mine").unwrap(),
        Target::Layer("meta-mine".into())
    );
    assert_eq!(env.resolve("build").unwrap(), Target::Layer("build".into()));
    let app = root.join("meta-mine/recipes-app/app/app_0.1.bb");
    assert_eq!(
        env.resolve("meta-mine:recipes-app/app/app_0.1.bb").unwrap(),
        Target::Files {
            files: vec![app.clone()],
            lines: None
        }
    );
    assert_eq!(
        env.resolve("meta-mine:recipes-app/app/app_0.1.bb:2-3")
            .unwrap(),
        Target::Files {
            files: vec![app.clone()],
            lines: Some(LineSpan::new(2, 3))
        }
    );
    assert_eq!(
        env.resolve("meta-mine/recipes-app/app/app_0.1.bb").unwrap(),
        Target::Files {
            files: vec![app.clone()],
            lines: None
        }
    );
    assert_eq!(
        env.resolve(&root.join("meta-core").to_string_lossy())
            .unwrap(),
        Target::Layer("meta-core".into())
    );
    match env.resolve("meta-mine/recipes-app").unwrap() {
        Target::Files { files, lines: None } => {
            assert!(files.contains(&app), "{files:?}");
            assert!(
                files
                    .iter()
                    .all(|f| f.starts_with(root.join("meta-mine/recipes-app")))
            );
        }
        other => panic!("{other:?}"),
    }
    assert_eq!(
        env.resolve("hello").unwrap(),
        Target::Recipe {
            name: "hello".into(),
            version: None
        }
    );
    assert_eq!(
        env.resolve("hello_1.0").unwrap(),
        Target::Recipe {
            name: "hello".into(),
            version: Some("1.0".into())
        }
    );
    assert!(matches!(
        env.resolve("no-such-thing"),
        Err(Error::NotFound { .. })
    ));
    assert!(matches!(
        env.resolve("meta-mine:recipes-app/none.bb"),
        Err(Error::NotFound { .. })
    ));
    assert!(
        env.resolve("meta-mine:../meta-core/conf/layer.conf")
            .is_err()
    );
    assert!(matches!(
        env.resolve("/etc/passwd"),
        Err(Error::Confined { .. })
    ));
}

#[test]
fn recipe_target_covers_bb_includes_and_appends() {
    let env = Env::new();
    let rule = TestRule::file("hygiene/test-each", line_one);
    let target = env.resolve("hello_1.1").unwrap();
    let report = env.run(&target, &Options::default(), &[&rule]);
    let files: Vec<String> = report
        .findings
        .iter()
        .map(|f| format!("{}:{}", f.loc.layer, f.loc.path.display()))
        .collect();
    assert!(
        files.contains(&"meta-core:recipes-core/hello/hello_1.1.bb".to_string()),
        "{files:?}"
    );
    assert!(
        files.contains(&"meta-core:recipes-core/hello/hello.inc".to_string()),
        "{files:?}"
    );
    assert!(
        files.contains(&"meta-mine:recipes-core/hello/hello_1.1.bbappend".to_string()),
        "{files:?}"
    );
    assert!(
        files.contains(&"meta-vendor:recipes-core/hello/hello_%.bbappend".to_string()),
        "{files:?}"
    );
    assert!(!files.iter().any(|f| f.ends_with(".bbclass")), "{files:?}");
}

#[test]
fn whole_workspace_skips_upstream_layers_unless_all() {
    let env = Env::new();
    let rule = TestRule::file("hygiene/test-each", line_one);
    let report = env.run(&Target::Workspace, &Options::default(), &[&rule]);
    assert_eq!(report.layers, ["meta-mine", "build"]);
    assert!(
        report
            .findings
            .iter()
            .all(|f| f.loc.layer == "meta-mine" || f.loc.layer == "build")
    );
    assert!(
        report
            .notes
            .iter()
            .any(|n| n.contains("2 layers that are not writable (meta-core, meta-vendor)")),
        "{:?}",
        report.notes
    );
    let all = Options {
        all: true,
        ..Options::default()
    };
    let report = env.run(&Target::Workspace, &all, &[&rule]);
    assert_eq!(
        report.layers,
        ["meta-core", "meta-vendor", "meta-mine", "build"]
    );
    assert!(report.findings.iter().any(|f| f.loc.layer == "meta-core"));
    assert!(report.notes.is_empty(), "{:?}", report.notes);
}

/// (w11) The per-file hook: `(0, n)` before the first file, one call per file, `(n, n)`
/// after the last, `n` the files checked.
#[test]
fn progress_hook_counts_every_file() {
    let env = Env::new();
    let rule = TestRule::file("hygiene/test-each", line_one);
    let calls = std::cell::RefCell::new(Vec::new());
    let hook = |done: usize, total: usize| calls.borrow_mut().push((done, total));
    let all = Options {
        all: true,
        ..Options::default()
    };
    let report = lint::run_with_progress(
        &env.ws,
        &env.index,
        &env.config,
        &Target::Workspace,
        &all,
        &[&rule],
        &NoEval,
        Some(&hook),
    )
    .unwrap();
    let n = report.files;
    assert!(n > 2, "{n}");
    let want: Vec<(usize, usize)> = (0..=n).map(|d| (d, n)).collect();
    assert_eq!(calls.into_inner(), want);
    // The same report as without a hook.
    assert_eq!(env.run(&Target::Workspace, &all, &[&rule]), report);
}

#[test]
fn no_writable_layer_means_every_layer() {
    let env = Env::with(|c| c.writable_layers.clear());
    let rule = TestRule::file("hygiene/test-each", line_one);
    let report = env.run(&Target::Workspace, &Options::default(), &[&rule]);
    assert_eq!(
        report.layers,
        ["meta-core", "meta-vendor", "meta-mine", "build"]
    );
}

#[test]
fn a_pointer_with_lines_reports_only_those_lines() {
    let mut env = Env::new();
    env.add(
        "meta-mine/recipes-app/x/x_1.0.bb",
        "A = \"LINTME\"\nB = \"LINTME\"\nC = \"LINTME\"\n",
    );
    let rule = TestRule::file("hygiene/test-flag", flagged_lines);
    let target = env.resolve("meta-mine:recipes-app/x/x_1.0.bb:2").unwrap();
    let report = env.run(&target, &Options::default(), &[&rule]);
    assert_eq!(
        rows(&report),
        ["warning hygiene/test-flag meta-mine:recipes-app/x/x_1.0.bb:2"]
    );
}

// ---------------------------------------------------------------------------------------
// Rule selection and configuration

fn garbage_env(edit: impl FnOnce(&mut Config)) -> Env {
    // The org rules (T4.3) report the fixture workspace's deliberate problems (the orphan
    // bbappend, the `pn-` override in local.conf); these tests count hygiene/parse-error.
    let mut env = Env::with(|c| {
        edit(c);
        c.lint.disable.push("org".into());
    });
    env.add("meta-mine/recipes-app/bad/bad_1.0.bb", GARBAGE);
    env
}

#[test]
fn parse_error_through_the_engine() {
    let env = garbage_env(|_| {});
    let report = env.run_all(&Target::Layer("meta-mine".into()));
    assert_eq!(
        rows_of(&report, PE),
        ["error hygiene/parse-error meta-mine:recipes-app/bad/bad_1.0.bb:2"]
    );
}

#[test]
fn disable_by_id_by_group_and_by_alias() {
    for disable in ["hygiene/parse-error", "hygiene", "hygiene/"] {
        let env = garbage_env(|c| c.lint.disable = vec![disable.into()]);
        let report = env.run_all(&Target::Layer("meta-mine".into()));
        assert!(
            !report
                .findings
                .iter()
                .any(|f| lint::entry_matches(disable, f.rule)),
            "{disable}: {:?}",
            report.findings
        );
        assert!(report.notes.iter().all(|n| !n.contains("sous-chef.toml")));
    }
    let env = garbage_env(|c| c.lint.disable = vec!["org".into()]);
    assert_eq!(
        rows_of(&env.run_all(&Target::Layer("meta-mine".into())), PE).len(),
        1
    );

    // The alias and the alias group name hygiene/variable-order only.
    let vo = TestRule::file("hygiene/variable-order", line_one);
    let tabs = TestRule::file("hygiene/tabs", line_one);
    for disable in ["style/variable-order", "style"] {
        let env = Env::with(|c| c.lint.disable = vec![disable.into()]);
        let report = env.run(
            &env.resolve("meta-mine:conf/layer.conf").unwrap(),
            &Options::default(),
            &[&vo, &tabs],
        );
        let ids: Vec<&str> = report.findings.iter().map(|f| f.rule).collect();
        assert_eq!(ids, ["hygiene/tabs"], "{disable}");
    }
}

#[test]
fn unknown_ids_in_config_are_notes_and_in_a_selection_are_errors() {
    let env = garbage_env(|c| {
        c.lint.disable = vec!["hygiene/no-such-rule".into()];
        c.lint.severity.insert("nosuchgroup".into(), Severity::Info);
    });
    let report = env.run_all(&Target::Layer("meta-mine".into()));
    assert_eq!(rows_of(&report, PE).len(), 1);
    assert!(
        report
            .notes
            .iter()
            .any(|n| n.contains("[lint] disable names \"hygiene/no-such-rule\"")),
        "{:?}",
        report.notes
    );
    assert!(
        report
            .notes
            .iter()
            .any(|n| n.contains("[lint.severity] names \"nosuchgroup\"")),
        "{:?}",
        report.notes
    );
    let err = lint::run(
        &env.ws,
        &env.index,
        &env.config,
        &Target::Workspace,
        &opts(&["hygiene/parse-eror"]),
    )
    .unwrap_err();
    assert!(err.to_string().contains("\"hygiene/parse-eror\""), "{err}");
}

#[test]
fn selection_by_id_and_group() {
    let env = Env::new();
    let a = TestRule::file("hygiene/test-a", line_one);
    let b = TestRule::file("org/test-b", line_one);
    let off = TestRule {
        enabled: false,
        ..TestRule::file("hygiene/test-off", line_one)
    };
    let t = env.resolve("meta-mine:conf/layer.conf").unwrap();
    let ids = |o: &Options| -> Vec<&'static str> {
        env.run(&t, o, &[&a, &b, &off])
            .findings
            .iter()
            .map(|f| f.rule)
            .collect()
    };
    assert_eq!(ids(&Options::default()), ["hygiene/test-a", "org/test-b"]);
    assert_eq!(ids(&opts(&["org"])), ["org/test-b"]);
    assert_eq!(ids(&opts(&["hygiene"])), ["hygiene/test-a"]);
    assert_eq!(ids(&opts(&["hygiene/test-off"])), ["hygiene/test-off"]);

    // An exact id runs even when disabled in the config; its group does not.
    let env = Env::with(|c| c.lint.disable = vec!["org".into()]);
    let t = env.resolve("meta-mine:conf/layer.conf").unwrap();
    let ids = |o: &Options| -> Vec<&'static str> {
        env.run(&t, o, &[&a, &b])
            .findings
            .iter()
            .map(|f| f.rule)
            .collect()
    };
    assert_eq!(ids(&opts(&["org"])), Vec::<&str>::new());
    assert_eq!(ids(&opts(&["org/test-b"])), ["org/test-b"]);
}

#[test]
fn severity_overrides_and_minimum() {
    let env = garbage_env(|c| {
        c.lint
            .severity
            .insert("hygiene/parse-error".into(), Severity::Warning);
    });
    let report = env.run_all(&Target::Layer("meta-mine".into()));
    assert_eq!(
        rows_of(&report, PE),
        ["warning hygiene/parse-error meta-mine:recipes-app/bad/bad_1.0.bb:2"]
    );
    assert!(report.notes.iter().all(|n| !n.contains("sous-chef.toml")));
    let only_errors = Options {
        min_severity: Some(Severity::Error),
        ..Options::default()
    };
    let report = lint::run(
        &env.ws,
        &env.index,
        &env.config,
        &Target::Layer("meta-mine".into()),
        &only_errors,
    )
    .unwrap();
    assert!(rows_of(&report, PE).is_empty());
    assert!(
        report
            .findings
            .iter()
            .all(|f| f.severity == Severity::Error)
    );

    // An id wins over its group, whatever the order in the table.
    let env = garbage_env(|c| {
        c.lint.severity.insert("hygiene".into(), Severity::Info);
        c.lint
            .severity
            .insert("hygiene/parse-error".into(), Severity::Warning);
    });
    let report = env.run_all(&Target::Layer("meta-mine".into()));
    let pe = |r: &Report| r.findings.iter().find(|f| f.rule == PE).unwrap().severity;
    assert_eq!(pe(&report), Severity::Warning);
    let env = garbage_env(|c| {
        c.lint.severity.insert("hygiene".into(), Severity::Info);
    });
    let report = env.run_all(&Target::Layer("meta-mine".into()));
    assert_eq!(pe(&report), Severity::Info);
}

// ---------------------------------------------------------------------------------------
// Releases

#[test]
fn release_range_runs_for_a_layer_that_claims_a_release_in_it() {
    let env = Env::new();
    // Older than the workspace's release (wrynose); meta-mine claims kirkstone.
    let kirkstone_only = TestRule {
        range: Release::Kirkstone..=Release::Kirkstone,
        ..TestRule::file("hygiene/test-kirkstone", line_one)
    };
    // No layer claims anything between langdale and mickledore.
    let middle = TestRule {
        range: Release::Langdale..=Release::Mickledore,
        ..TestRule::file("hygiene/test-middle", line_one)
    };
    let newest = TestRule {
        range: Release::Wrynose..=Release::Wrynose,
        ..TestRule::file("hygiene/test-wrynose", line_one)
    };
    let rules: [&dyn Rule; 3] = [&kirkstone_only, &middle, &newest];

    let mine = env.run(
        &Target::Layer("meta-mine".into()),
        &Options::default(),
        &rules,
    );
    let ids: std::collections::BTreeSet<&str> = mine.findings.iter().map(|f| f.rule).collect();
    assert_eq!(
        ids.into_iter().collect::<Vec<_>>(),
        ["hygiene/test-kirkstone", "hygiene/test-wrynose"]
    );
    assert!(
        mine.findings
            .iter()
            .all(|f| f.message == "kirkstone scarthgap wrynose"),
        "{:?}",
        mine.findings
    );

    // meta-core is not writable and named as the target: checked against what it claims
    // (wrynose only) plus the workspace release.
    let core = env.run(
        &Target::Layer("meta-core".into()),
        &Options::default(),
        &rules,
    );
    assert!(!core.findings.is_empty());
    assert!(
        core.findings
            .iter()
            .all(|f| f.rule == "hygiene/test-wrynose")
    );
    assert!(core.findings.iter().all(|f| f.message == "wrynose"));

    // The build directory's files: the workspace release alone.
    let build = env.run(&Target::Layer("build".into()), &Options::default(), &rules);
    assert!(!build.findings.is_empty());
    assert!(
        build
            .findings
            .iter()
            .all(|f| f.rule == "hygiene/test-wrynose")
    );
    assert!(build.findings.iter().all(|f| f.message == "wrynose"));
}

#[test]
fn release_range_gates_layer_scope_rules_too() {
    let env = Env::new();
    let layer_rule = TestRule {
        scope: Scope::Layer,
        range: Release::Kirkstone..=Release::Kirkstone,
        ..TestRule::file("org/test-layer", line_one)
    };
    let mine = env.run(
        &Target::Layer("meta-mine".into()),
        &Options::default(),
        &[&layer_rule],
    );
    assert_eq!(
        rows(&mine),
        ["warning org/test-layer meta-mine:conf/layer.conf:1"]
    );
    let core = env.run(
        &Target::Layer("meta-core".into()),
        &Options::default(),
        &[&layer_rule],
    );
    assert!(core.findings.is_empty());
}

/// The distinct release lists (`line_one` messages) reported in `layer`.
fn releases_reported(report: &Report, layer: &str) -> Vec<String> {
    let mut v: Vec<String> = report
        .findings
        .iter()
        .filter(|f| f.loc.layer == layer)
        .map(|f| f.message.clone())
        .collect();
    v.sort();
    v.dedup();
    v
}

// meta-vendor is not writable and claims `scarthgap wrynose`; the workspace is wrynose.

#[test]
fn an_upstream_layer_named_as_the_target_is_checked_against_every_release_it_claims() {
    let env = Env::new();
    let file_rule = TestRule::file("hygiene/test-each", line_one);
    let layer_rule = TestRule {
        scope: Scope::Layer,
        ..TestRule::file("org/test-layer", line_one)
    };
    let report = env.run(
        &Target::Layer("meta-vendor".into()),
        &Options::default(),
        &[&file_rule, &layer_rule],
    );
    assert!(report.findings.iter().any(|f| f.rule == "org/test-layer"));
    assert_eq!(
        releases_reported(&report, "meta-vendor"),
        ["scarthgap wrynose"]
    );
}

#[test]
fn a_file_or_recipe_target_in_an_upstream_layer_checks_that_layer_against_its_claims() {
    let mut env = Env::new();
    let abs = env.add(
        "meta-vendor/recipes-x/vx/vx_1.0.bb",
        "SUMMARY = \"x\"\nLICENSE = \"MIT\"\n",
    );
    let rule = TestRule::file("hygiene/test-each", line_one);
    let files = Target::Files {
        files: vec![abs],
        lines: None,
    };
    let report = env.run(&files, &Options::default(), &[&rule]);
    assert_eq!(
        rows(&report),
        ["warning hygiene/test-each meta-vendor:recipes-x/vx/vx_1.0.bb:1"]
    );
    assert_eq!(
        releases_reported(&report, "meta-vendor"),
        ["scarthgap wrynose"]
    );

    let recipe = Target::Recipe {
        name: "vx".into(),
        version: None,
    };
    let report = env.run(&recipe, &Options::default(), &[&rule]);
    assert_eq!(
        releases_reported(&report, "meta-vendor"),
        ["scarthgap wrynose"]
    );
}

#[test]
fn every_layer_gets_its_claims_when_no_layer_is_writable() {
    let env = Env::with(|c| c.writable_layers.clear());
    let rule = TestRule::file("hygiene/test-each", line_one);
    let report = env.run(&Target::Workspace, &Options::default(), &[&rule]);
    assert_eq!(
        releases_reported(&report, "meta-vendor"),
        ["scarthgap wrynose"]
    );
    assert_eq!(
        releases_reported(&report, "meta-mine"),
        ["kirkstone scarthgap wrynose"]
    );
    assert_eq!(releases_reported(&report, "build"), ["wrynose"]);
}

#[test]
fn an_upstream_layer_swept_in_by_all_gets_the_workspace_release_alone() {
    let env = Env::new();
    let rule = TestRule::file("hygiene/test-each", line_one);
    let all = Options {
        all: true,
        ..Options::default()
    };
    let report = env.run(&Target::Workspace, &all, &[&rule]);
    assert_eq!(releases_reported(&report, "meta-vendor"), ["wrynose"]);
    // The writable layer keeps every release it claims in the same run.
    assert_eq!(
        releases_reported(&report, "meta-mine"),
        ["kirkstone scarthgap wrynose"]
    );
}

// ---------------------------------------------------------------------------------------
// Scopes

#[test]
fn recipe_scope_uses_the_evaluator_seam() {
    let env = Env::new();
    fn needs_data(r: &TestRule, cx: &LintContext<'_>, out: &mut Vec<Finding>) {
        assert!(cx.data.is_some() && cx.file.is_some());
        assert_eq!(cx.kind, Some(FileKind::Recipe));
        line_one(r, cx, out);
    }
    let rule = TestRule {
        scope: Scope::Recipe,
        ..TestRule::file("hygiene/test-recipe", needs_data)
    };
    let all = Options {
        all: true,
        ..Options::default()
    };
    let report = lint::run_with(
        &env.ws,
        &env.index,
        &env.config,
        &Target::Workspace,
        &all,
        &[&rule],
        &FakeEval,
    )
    .unwrap();
    let files: Vec<String> = report
        .findings
        .iter()
        .map(|f| f.loc.path.display().to_string())
        .collect();
    assert!(
        files.contains(&"recipes-core/hello/hello_1.0.bb".to_string()),
        "{files:?}"
    );
    assert!(files.iter().all(|f| f.ends_with(".bb")), "{files:?}");
    assert!(!files.iter().any(|f| f.contains("app_0.1")), "{files:?}");
    assert!(
        report
            .notes
            .iter()
            .any(|n| n.starts_with("recipe app (") && n.contains("replay exploded")),
        "{:?}",
        report.notes
    );

    // Without an evaluator: skipped, with one note.
    let report = env.run(&Target::Workspace, &all, &[&rule]);
    assert!(report.findings.is_empty());
    assert_eq!(
        report.notes,
        [
            "recipe-scope rules skipped, the evaluator is not available in this build: hygiene/test-recipe"
        ]
    );
}

#[test]
fn the_real_evaluator_seam_runs_recipe_rules_with_data() {
    let env = Env::new();
    let target = eval::Target {
        recipe: Some("hello".into()),
        ..eval::Target::default()
    };
    assert!(
        Replay
            .evaluate(&env.index, &env.ws, &target)
            .unwrap()
            .is_some()
    );

    fn with_data(r: &TestRule, cx: &LintContext<'_>, out: &mut Vec<Finding>) {
        let data = cx.data.expect("recipe scope gets a datastore");
        assert!(data.value("PN").is_some(), "{}", cx.loc);
        line_one(r, cx, out);
    }
    let rule = TestRule {
        scope: Scope::Recipe,
        ..TestRule::file("hygiene/test-recipe", with_data)
    };
    let all = Options {
        all: true,
        ..Options::default()
    };
    let report = lint::run_with(
        &env.ws,
        &env.index,
        &env.config,
        &Target::Workspace,
        &all,
        &[&rule],
        &Replay,
    )
    .unwrap();
    assert!(report.notes.is_empty(), "{:?}", report.notes);
    let mut files: Vec<String> = report.findings.iter().map(|f| f.loc.to_string()).collect();
    files.sort();
    assert_eq!(
        files,
        [
            "meta-core:recipes-core/hello/hello_1.0.bb:1",
            "meta-core:recipes-core/hello/hello_1.1.bb:1",
            "meta-core:recipes-core/libfoo/libfoo_2.0.bb:1",
            "meta-mine:recipes-app/app/app_0.1.bb:1",
            "meta-vendor:recipes-vendor/libfoo-alt/libfoo-alt_1.0.bb:1",
        ]
    );
}

#[test]
fn recipe_scope_evaluates_the_linted_file_not_the_preferred_one() {
    let mut env = Env::new();
    // The same PN_PV in a higher-priority layer (meta-vendor 6 > meta-core 5).
    env.add(
        "meta-vendor/recipes-core/libfoo/libfoo_2.0.bb",
        "SUMMARY = \"vendor copy\"\nLICENSE = \"MIT\"\n",
    );
    fn summary(r: &TestRule, cx: &LintContext<'_>, out: &mut Vec<Finding>) {
        let data = cx.data.expect("recipe scope gets a datastore");
        let recipe = data
            .files()
            .iter()
            .find(|v| v.kind == FileKind::Recipe)
            .expect("the recipe is visited");
        assert_eq!(recipe.loc.layer, cx.loc.layer);
        assert_eq!(recipe.loc.path, cx.loc.path);
        out.push(Finding {
            rule: r.id,
            severity: r.severity,
            loc: cx.loc.clone(),
            message: data.value("SUMMARY").map(|v| v.text).unwrap_or_default(),
        });
    }
    let rule = TestRule {
        scope: Scope::Recipe,
        ..TestRule::file("hygiene/test-recipe", summary)
    };
    let lint_one = |target: &str| -> Vec<(String, String)> {
        let report = lint::run_with(
            &env.ws,
            &env.index,
            &env.config,
            &env.resolve(target).unwrap(),
            &Options::default(),
            &[&rule],
            &Replay,
        )
        .unwrap();
        assert!(report.notes.is_empty(), "{:?}", report.notes);
        report
            .findings
            .iter()
            .map(|f| (f.loc.layer.clone(), f.message.clone()))
            .collect()
    };
    // The lower-priority copy is evaluated as itself.
    assert_eq!(
        lint_one("meta-core:recipes-core/libfoo/libfoo_2.0.bb"),
        [("meta-core".to_string(), "The foo library".to_string())]
    );
    assert_eq!(
        lint_one("meta-vendor:recipes-core/libfoo/libfoo_2.0.bb"),
        [("meta-vendor".to_string(), "vendor copy".to_string())]
    );
}

#[test]
fn layer_scope_findings_are_kept_only_in_the_target_files() {
    let env = Env::new();
    fn two_files(r: &TestRule, cx: &LintContext<'_>, out: &mut Vec<Finding>) {
        assert!(cx.file.is_none() && cx.layer.is_some());
        for p in ["conf/layer.conf", "recipes-app/app/app_0.1.bb"] {
            out.push(Finding {
                rule: r.id,
                severity: r.severity,
                loc: Loc {
                    layer: cx.layer.unwrap().to_string(),
                    path: PathBuf::from(p),
                    lines: LineSpan::line(1),
                },
                message: "layer".into(),
            });
        }
    }
    let rule = TestRule {
        scope: Scope::Layer,
        ..TestRule::file("org/test-layer", two_files)
    };
    let whole = env.run(
        &Target::Layer("meta-mine".into()),
        &Options::default(),
        &[&rule],
    );
    assert_eq!(whole.findings.len(), 2);
    let one = env.run(
        &env.resolve("meta-mine:recipes-app/app/app_0.1.bb").unwrap(),
        &Options::default(),
        &[&rule],
    );
    assert_eq!(
        rows(&one),
        ["warning org/test-layer meta-mine:recipes-app/app/app_0.1.bb:1"]
    );
}

// ---------------------------------------------------------------------------------------
// Suppression, sorting

#[test]
fn inline_suppression() {
    let mut env = Env::new();
    env.add(
        "meta-mine/recipes-app/s/s_1.0.bb",
        "\
# sous-chef: allow hygiene/test-flag
A = \"LINTME\"
B = \"LINTME\"
# sous-chef: allow hygiene
# another comment
C = \"LINTME\"
# sous-chef: allow org/test-other, hygiene/test-flag
do_x() {
    echo LINTME
}
# sous-chef: allow org
D = \"LINTME\"
E = \"LINTME \\
LINTME\"
",
    );
    let rule = TestRule::file("hygiene/test-flag", flagged_lines);
    let report = env.run(
        &env.resolve("meta-mine:recipes-app/s/s_1.0.bb").unwrap(),
        &Options::default(),
        &[&rule],
    );
    let lines: Vec<u32> = report.findings.iter().map(|f| f.loc.lines.start).collect();
    // 2 suppressed (line before); 3 not; 6 suppressed (stacked comments); 9 suppressed
    // (before the statement); 12 not (other group); 13 and 14 not.
    assert_eq!(lines, [3, 12, 13, 14]);

    // The parse-error rule through the real engine.
    let mut env = Env::new();
    env.add(
        "meta-mine/recipes-app/g/g_1.0.bb",
        "# sous-chef: allow hygiene/parse-error\nthis is not bitbake\nnor is this\n",
    );
    let report = env.run_all(&env.resolve("meta-mine:recipes-app/g/g_1.0.bb").unwrap());
    assert_eq!(
        rows_of(&report, PE),
        ["error hygiene/parse-error meta-mine:recipes-app/g/g_1.0.bb:3"]
    );
}

#[test]
fn findings_are_sorted_by_file_then_line_and_deduplicated() {
    let mut env = Env::new();
    env.add(
        "meta-mine/recipes-app/b/b_1.0.bb",
        "X = \"LINTME\"\nY = \"LINTME\"\n",
    );
    env.add("meta-mine/recipes-app/a/a_1.0.bb", "\nY = \"LINTME\"\n");
    fn backwards(r: &TestRule, cx: &LintContext<'_>, out: &mut Vec<Finding>) {
        let mut mine = Vec::new();
        flagged_lines(r, cx, &mut mine);
        mine.reverse();
        out.extend(mine.iter().cloned());
        out.extend(mine);
    }
    let rule = TestRule::file("hygiene/test-flag", backwards);
    let other = TestRule::file("hygiene/test-and", flagged_lines);
    let report = env.run(
        &Target::Layer("meta-mine".into()),
        &Options::default(),
        &[&rule, &other],
    );
    assert_eq!(
        rows(&report),
        [
            "warning hygiene/test-and meta-mine:recipes-app/a/a_1.0.bb:2",
            "warning hygiene/test-flag meta-mine:recipes-app/a/a_1.0.bb:2",
            "warning hygiene/test-and meta-mine:recipes-app/b/b_1.0.bb:1",
            "warning hygiene/test-flag meta-mine:recipes-app/b/b_1.0.bb:1",
            "warning hygiene/test-and meta-mine:recipes-app/b/b_1.0.bb:2",
            "warning hygiene/test-flag meta-mine:recipes-app/b/b_1.0.bb:2",
        ]
    );
}

// ---------------------------------------------------------------------------------------
// CLI and exit codes

fn cli(env: &Env, args: LintCliArgs) -> (i32, String) {
    let mut out = Vec::new();
    let code = lint::cli_with(&env.ws, &env.index, &env.config, &args, &mut out).unwrap();
    (code, String::from_utf8(out).unwrap())
}

#[test]
fn cli_exit_codes_and_output() {
    let env = garbage_env(|_| {});
    let (code, out) = cli(&env, LintCliArgs::default());
    assert_eq!(code, 1, "{out}");
    // Other rules also report on the fixture workspace (for example line 13 of
    // build/conf/local.conf, override/plus-equals-on-default); the summary counts
    // parse-error once.
    assert!(
        out.lines()
            .next()
            .unwrap()
            .contains("hygiene/parse-error 1"),
        "{out}"
    );
    assert!(out.contains("error\thygiene/parse-error\tmeta-mine:recipes-app/bad/bad_1.0.bb:2\t"));
    assert!(out.contains("warning\toverride/plus-equals-on-default\tbuild:conf/local.conf:13\t"));

    let (code, _) = cli(
        &env,
        LintCliArgs {
            rules: vec!["org".into()],
            ..LintCliArgs::default()
        },
    );
    assert_eq!(code, 0);

    let warn = garbage_env(|c| {
        c.lint
            .severity
            .insert("hygiene/parse-error".into(), Severity::Warning);
    });
    assert_eq!(cli(&warn, LintCliArgs::default()).0, 0);
    let (code, _) = cli(
        &warn,
        LintCliArgs {
            fail_on: Some(Severity::Warning),
            ..LintCliArgs::default()
        },
    );
    assert_eq!(code, 1);
    // A severity filter that hides the warning also keeps it from failing.
    let (code, out) = cli(
        &warn,
        LintCliArgs {
            fail_on: Some(Severity::Warning),
            severity: Some(Severity::Error),
            ..LintCliArgs::default()
        },
    );
    assert_eq!(code, 0, "{out}");

    let (code, out) = cli(
        &env,
        LintCliArgs {
            format: Some(sous_chef::render::Format::Json),
            ..LintCliArgs::default()
        },
    );
    assert_eq!(code, 1);
    let v: serde_json::Value = serde_json::from_str(&out).unwrap();
    assert_eq!(v["errors"], 1);
    assert_eq!(v["by_rule"]["hygiene/parse-error"], 1);

    let mut out = Vec::new();
    let err = lint::cli_with(
        &env.ws,
        &env.index,
        &env.config,
        &LintCliArgs {
            target: Some("no-such-target".into()),
            ..LintCliArgs::default()
        },
        &mut out,
    );
    assert!(matches!(err, Err(Error::NotFound { .. })));
}

#[test]
fn binary_exit_codes() {
    let env = garbage_env(|_| {});
    let bin = env!("CARGO_BIN_EXE_sous-chef");
    let run = |args: &[&str]| {
        let out = Command::new(bin)
            .args(args)
            .current_dir(&env.root)
            .env("XDG_CACHE_HOME", env._cache.path())
            .output()
            .unwrap();
        (
            out.status.code(),
            String::from_utf8_lossy(&out.stdout).into_owned(),
            String::from_utf8_lossy(&out.stderr).into_owned(),
        )
    };
    let (code, out, err) = run(&["lint"]);
    assert_eq!(code, Some(1), "{out}{err}");
    assert!(
        out.contains("hygiene/parse-error\tmeta-mine:recipes-app/bad/bad_1.0.bb:2"),
        "{out}"
    );
    let (code, out, err) = run(&["lint", "meta-mine", "--rule", "org"]);
    assert_eq!(code, Some(0), "{out}{err}");
    let (code, out, err) = run(&[
        "lint",
        "--fail-on",
        "info",
        "--rule",
        "hygiene/parse-error",
        "meta-core",
    ]);
    assert_eq!(code, Some(0), "{out}{err}");
    let (code, _, err) = run(&["lint", "no-such-target"]);
    assert_eq!(code, Some(2), "{err}");
    assert!(err.contains("no-such-target"), "{err}");
    let (code, _, err) = run(&["lint", "--rule", "nope"]);
    assert_eq!(code, Some(2), "{err}");
}

// ---------------------------------------------------------------------------------------
// Scaffold post-write lint

struct WriteBad(PathBuf);

impl Generator for WriteBad {
    fn kind(&self) -> Kind {
        Kind::Recipe
    }
    fn options(&self) -> &'static [&'static str] {
        &[]
    }
    fn plan(&self, _: &GenContext<'_>) -> Result<Plan> {
        Ok(Plan {
            files: vec![PlannedFile {
                path: self.0.clone(),
                content: GARBAGE.into(),
                action: FileAction::Create,
            }],
            notes: vec![],
            new_layer: None,
        })
    }
}

#[test]
fn scaffold_rule_lint_reports_findings_in_written_files() {
    let mut env = Env::new();
    // A pre-existing bad file in the same layer must not show up: only written files count.
    env.add("meta-mine/recipes-app/old/old_1.0.bb", GARBAGE);
    let target = env.root.join("meta-mine/recipes-app/new/new_1.0.bb");
    let request = scaffold::Request {
        kind: Kind::Recipe,
        layer: Some("meta-mine".into()),
        name: Some("new".into()),
        version: Some("1.0".into()),
        options: serde_json::Map::new(),
    };
    let outcome = scaffold::execute(
        &env.ws,
        &env.config,
        &mut env.index,
        &request,
        true,
        Some(&WriteBad(target.clone())),
        &scaffold::RuleLint,
    )
    .unwrap();
    assert!(outcome.written);
    let findings = outcome.lint.unwrap().unwrap();
    let got: Vec<String> = findings
        .iter()
        .filter(|f| f.rule == PE)
        .map(|f| format!("{} {}", f.rule, f.loc))
        .collect();
    // Other rules may report on the written file too, but only on that file.
    assert!(
        findings
            .iter()
            .all(|f| f.loc.path.ends_with("new/new_1.0.bb")),
        "{findings:?}"
    );
    assert_eq!(
        got,
        ["hygiene/parse-error meta-mine:recipes-app/new/new_1.0.bb:2"]
    );
}
