//! T4.5: the `lint` MCP tool and the `sous-chef://rules` resource.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::fs;
use std::path::PathBuf;

use common::*;
use sous_chef::bitbake::JobTable;
use sous_chef::config::Config;
use sous_chef::handles::HandleStore;
use sous_chef::index::Index;
use sous_chef::lint::{JsonReport, Severity};
use sous_chef::mcp::tools::lint::{self as tool, Args};
use sous_chef::mcp::{self, Ctx};
use sous_chef::render::Format;
use sous_chef::workspace::Workspace;

struct Env {
    _tmp: tempfile::TempDir,
    _cache: tempfile::TempDir,
    root: PathBuf,
    ctx: Ctx,
}

impl Env {
    /// A copy of the fixture workspace with extra files, indexed once so tool output has no
    /// freshness line.
    async fn new(files: &[(&str, &str)], edit: impl FnOnce(&mut Config)) -> Env {
        let (tmp, root) = temp_ws();
        let root = fs::canonicalize(root).unwrap();
        for (rel, content) in files {
            let p = root.join(rel);
            fs::create_dir_all(p.parent().unwrap()).unwrap();
            fs::write(p, content).unwrap();
        }
        let cache = tempfile::tempdir().unwrap();
        let mut config = ws_config(&root);
        edit(&mut config);
        let workspace = Workspace::discover(&config).unwrap();
        let mut index = Index::open_in(cache.path(), &workspace).unwrap();
        index.refresh(&workspace).unwrap();
        let handles = HandleStore::open(&index.cache_dir().join("handles")).unwrap();
        let ctx = Ctx {
            config,
            workspace,
            index: tokio::sync::Mutex::new(index),
            handles,
            jobs: JobTable::new(),
        };
        Env {
            _tmp: tmp,
            _cache: cache,
            root,
            ctx,
        }
    }
}

fn args(target: Option<&str>) -> Args {
    Args {
        target: target.map(str::to_string),
        rules: None,
        severity: None,
        budget_tokens: None,
        format: None,
        fix: None,
        fix_suggested: None,
        write: None,
        ..Args::default()
    }
}

#[tokio::test]
async fn fix_through_the_tool_is_a_dry_run_unless_write() {
    let rel = "meta-mine/recipes-app/old/old_1.0.bb";
    let old = "SUMMARY = \"old\"\nLICENSE = \"MIT\"\nDEPENDS_append = \" zlib\"\n";
    let env = Env::new(&[(rel, old)], |_| {}).await;
    let target = Some("meta-mine:recipes-app/old/old_1.0.bb");
    let dry = tool::run(
        &env.ctx,
        Args {
            fix: Some(true),
            ..args(target)
        },
    )
    .await
    .unwrap();
    assert!(dry.starts_with("lint --fix (dry run"), "{dry}");
    assert!(dry.contains("+DEPENDS:append = \" zlib\""), "{dry}");
    assert_eq!(fs::read_to_string(env.root.join(rel)).unwrap(), old);
    // A small budget cuts the text on a line boundary, with a note.
    let cut = tool::run(
        &env.ctx,
        Args {
            fix: Some(true),
            budget_tokens: Some(60),
            ..args(target)
        },
    )
    .await
    .unwrap();
    assert!(cut.len() < dry.len(), "{cut}");
    assert!(
        cut.ends_with("run `sous-chef lint --fix`\n") && cut.contains("note: output cut"),
        "{cut}"
    );
    // `write` alone is refused.
    let refused = tool::run(
        &env.ctx,
        Args {
            write: Some(true),
            ..args(target)
        },
    )
    .await;
    assert!(refused.is_err());
    let wrote = tool::run(
        &env.ctx,
        Args {
            fix: Some(true),
            write: Some(true),
            ..args(target)
        },
    )
    .await
    .unwrap();
    assert!(
        wrote.contains("after: 1 file written; 1 finding fixed"),
        "{wrote}"
    );
    assert_eq!(
        fs::read_to_string(env.root.join(rel)).unwrap(),
        old.replace("DEPENDS_append", "DEPENDS:append")
    );
    // JSON carries the whole report (after the write nothing is left to fix).
    let json = tool::run(
        &env.ctx,
        Args {
            fix: Some(true),
            format: Some(Format::Json),
            ..args(target)
        },
    )
    .await
    .unwrap();
    let v: serde_json::Value = serde_json::from_str(&json).unwrap();
    assert_eq!(v["write"], false);
    assert!(v["available"].as_array().unwrap().is_empty(), "{json}");
}

const BAD: &str = "\
SUMMARY = \"bad\"
LICENSE = \"MIT\"
this is not bitbake
addtask do_x after do_a before do_b after do_c
do_broken() {
    echo
";

#[tokio::test]
async fn snapshot_on_a_fixture_layer() {
    let env = Env::new(
        &[
            ("meta-mine/recipes-app/bad/bad_1.0.bb", BAD),
            ("meta-mine/conf/distro/mine.conf", "inherit foo\n"),
        ],
        |_| {},
    )
    .await;
    let out = tool::run(
        &env.ctx,
        Args {
            rules: Some(vec!["hygiene/parse-error".into()]),
            ..args(Some("meta-mine"))
        },
    )
    .await
    .unwrap();
    insta::assert_snapshot!("lint_meta_mine", out);

    // The default rule set on the same layer: the same parse-error findings, plus those of
    // the other rules (the hygiene rules report on app_0.1.bb and bad_1.0.bb too).
    let all = tool::run(&env.ctx, args(Some("meta-mine"))).await.unwrap();
    // org/append-no-match also reports the fixture workspace's orphan bbappend.
    assert!(all.contains("hygiene/parse-error 4"), "{all}");
    assert!(all.contains("org/append-no-match 1"), "{all}");

    // A clean layer.
    let core = tool::run(
        &env.ctx,
        Args {
            rules: Some(vec!["hygiene/parse-error".into()]),
            ..args(Some("meta-vendor"))
        },
    )
    .await
    .unwrap();
    assert_eq!(
        core, "no findings in 3 files\nseverity\trule\tpointer\tmessage\n",
        "{core}"
    );
}

#[tokio::test]
async fn budget_truncation_keeps_the_summary_line() {
    let many: String = (0..60).map(|i| format!("not bitbake line {i}\n")).collect();
    let env = Env::new(&[("meta-mine/recipes-app/many/many_1.0.bb", &many)], |_| {}).await;
    let out = tool::run(
        &env.ctx,
        Args {
            budget_tokens: Some(150),
            rules: Some(vec!["hygiene/parse-error".into()]),
            ..args(Some("meta-mine:recipes-app/many/many_1.0.bb"))
        },
    )
    .await
    .unwrap();
    let mut lines = out.lines();
    assert_eq!(
        lines.next(),
        Some("60 findings in 1 file: 60 error, 0 warning, 0 info; by rule: hygiene/parse-error 60"),
        "{out}"
    );
    assert_eq!(lines.next(), Some("severity\trule\tpointer\tmessage"));
    assert!(out.contains(" more over budget_tokens\n"), "{out}");
    assert!(sous_chef::render::estimate_tokens(&out) <= 150, "{out}");

    // JSON keeps the counts of every finding while the rows are cut.
    let json = tool::run(
        &env.ctx,
        Args {
            budget_tokens: Some(150),
            rules: Some(vec!["hygiene/parse-error".into()]),
            format: Some(Format::Json),
            ..args(Some("meta-mine:recipes-app/many/many_1.0.bb"))
        },
    )
    .await
    .unwrap();
    let r: JsonReport = serde_json::from_str(&json).unwrap();
    assert_eq!(r.errors, 60);
    assert_eq!(r.by_rule["hygiene/parse-error"], 60);
    assert!(r.findings.more_over_budget > 0);
    assert_eq!(r.findings.rows.len() + r.findings.more_over_budget, 60);
}

#[tokio::test]
async fn tool_arguments_and_errors() {
    let env = Env::new(&[("meta-mine/recipes-app/bad/bad_1.0.bb", BAD)], |_| {}).await;
    let errors_only = tool::run(
        &env.ctx,
        Args {
            severity: Some(Severity::Error),
            ..args(Some("meta-mine"))
        },
    )
    .await
    .unwrap();
    assert!(errors_only.starts_with("2 findings"), "{errors_only}");
    assert!(!errors_only.contains("\twarning\t") && !errors_only.contains("warning\thygiene"));

    let e = tool::run(
        &env.ctx,
        Args {
            rules: Some(vec!["nope".into()]),
            ..args(None)
        },
    )
    .await
    .unwrap_err();
    assert!(e.to_string().contains("\"nope\""), "{e}");
    let msg = mcp::tool_error_message("lint", &e);
    assert!(!msg.contains('\n'), "{msg}");

    let e = tool::run(&env.ctx, args(Some("no-such-recipe")))
        .await
        .unwrap_err();
    assert!(e.to_string().contains("no-such-recipe"), "{e}");

    // Whole workspace: upstream layers are skipped with a note.
    let ws = tool::run(&env.ctx, args(None)).await.unwrap();
    assert!(
        ws.contains("note: not checked: 2 layers that are not writable"),
        "{ws}"
    );

    // A change on disk shows up, with the freshness line.
    fs::write(
        env.root.join("meta-mine/recipes-app/bad/bad_1.0.bb"),
        "SUMMARY = \"fixed\"\n",
    )
    .unwrap();
    let fixed = tool::run(&env.ctx, args(Some("meta-mine"))).await.unwrap();
    // The parse errors are gone; the fixture workspace's orphan bbappend
    // (org/append-no-match) and the other rules' findings remain.
    assert!(!fixed.contains("hygiene/parse-error"), "{fixed}");
    assert!(fixed.contains("org/append-no-match 1"), "{fixed}");
    assert!(
        fixed.contains("reindexed 1 file: meta-mine:recipes-app/bad/bad_1.0.bb"),
        "{fixed}"
    );
}

#[tokio::test]
async fn the_rules_resource_is_the_catalogue_with_effective_severities() {
    let plain = tool::rules_resource(None);
    assert_eq!(plain, mcp::rules_text(None));
    let rows: Vec<&str> = plain.lines().skip(1).collect();
    assert_eq!(rows.len(), sous_chef::lint::all_rules().len());
    assert!(plain.contains("hygiene/parse-error\terror\tfile\tAny `Unknown` statement"));

    let env = Env::new(&[], |c| {
        c.lint
            .severity
            .insert("hygiene/parse-error".into(), Severity::Warning);
        c.lint.severity.insert("org".into(), Severity::Info);
        c.lint.disable = vec!["style".into()];
    })
    .await;
    let text = mcp::rules_text(Some(&env.ctx.config));
    assert!(
        text.contains("hygiene/parse-error\twarning\tfile\t"),
        "{text}"
    );
    assert!(text.contains("org/layer-conf\tinfo\tlayer\t"), "{text}");
    assert!(
        text.contains("override/legacy-syntax\terror\tfile\t"),
        "{text}"
    );
    assert!(
        text.ends_with("disabled in sous-chef.toml: style\n"),
        "{text}"
    );
}
