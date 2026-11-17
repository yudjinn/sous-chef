//! T8.3 `layers show=compat`: the fixture workspace, the `org/layer-conf` fixture layers
//! (the view and the rule agree), and `BBLAYERS` order on a temporary copy.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::fs;
use std::path::{Path, PathBuf};

use sous_chef::audit;
use sous_chef::bitbake::JobTable;
use sous_chef::config::Config;
use sous_chef::handles::HandleStore;
use sous_chef::index::Index;
use sous_chef::lint::Severity;
use sous_chef::lint::rules::org_layer_conf::{OrgLayerConf, layer_problems};
use sous_chef::lint::{self, Options, Target};
use sous_chef::mcp::Ctx;
use sous_chef::mcp::tools::layers::{self, Show};
use sous_chef::render::{self, Format, JsonTable};
use sous_chef::workspace::Workspace;

struct Fixture {
    _cache: tempfile::TempDir,
    ctx: Ctx,
}

fn fixture_with(config: Config) -> Fixture {
    let cache = tempfile::tempdir().unwrap();
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
    Fixture { _cache: cache, ctx }
}

async fn compat(f: &Fixture, format: Option<Format>) -> String {
    layers::run(
        &f.ctx,
        layers::Args {
            show: Show::Compat,
            budget_tokens: Some(4000),
            format,
        },
    )
    .await
    .unwrap()
}

/// The `org/layer-conf` fixture layers, without a build directory.
fn lint_layers(tmp: &Path, names: &[&str]) -> Config {
    let root = tmp.join("ws");
    for l in names {
        common::copy_dir(
            &common::fixture_path(&format!("lint/org/layer-conf/{l}")),
            &root.join(l),
        );
    }
    let root = fs::canonicalize(root).unwrap();
    let mut config = Config {
        layers: names.iter().map(PathBuf::from).collect(),
        writable_layers: names.iter().map(|l| l.to_string()).collect(),
        work_dir: root,
        ..Config::default()
    };
    config.bitbake.enabled = false;
    config
}

#[tokio::test]
async fn fixture_workspace() {
    let f = fixture_with(common::fixture_config());
    let out = compat(&f, None).await;
    insta::assert_snapshot!("layers_compat_fixture", out);
    assert!(!out.contains("\torder\t"), "{out}");
    let json = compat(&f, Some(Format::Json)).await;
    let v: serde_json::Value = serde_json::from_str(&json).unwrap();
    assert_eq!(v["rows"].as_array().unwrap().len(), 3, "{json}");
}

#[tokio::test]
async fn lint_fixture_layers_agree_with_the_rule() {
    let tmp = tempfile::tempdir().unwrap();
    let names = [
        "core-layer",
        "bad-layer",
        "incompat-layer",
        "copy-layer",
        "good-layer",
    ];
    let config = lint_layers(tmp.path(), &names);
    let f = fixture_with(config);
    let out = compat(&f, None).await;
    insta::assert_snapshot!("layers_compat_lint_fixture", out);
    // A copied LAYERSERIES_COMPAT makes BitBake 2.4+ stop: the view says error, as the rule.
    let copy_rows: Vec<&str> = out
        .lines()
        .filter(|l| l.starts_with("copy-layer\tcompat\t"))
        .collect();
    assert_eq!(copy_rows.len(), 2, "{out}");
    assert!(
        copy_rows.iter().all(|l| l.contains("\tcompat\terror\t")),
        "{out}"
    );

    // Every compat / depends finding of org/layer-conf is a row, with the same message.
    let index = f.ctx.index.lock().await;
    let ws = &f.ctx.workspace;
    for name in names {
        let report = lint::run_with(
            ws,
            &index,
            &f.ctx.config,
            &Target::Layer(name.into()),
            &Options {
                rules: vec!["org/layer-conf".into()],
                ..Options::default()
            },
            &[&OrgLayerConf],
            &lint::Replay,
        )
        .unwrap();
        let problems = layer_problems(ws, &index, name);
        assert_eq!(report.findings.len(), problems.len(), "{name}");
        for p in problems
            .iter()
            .filter(|p| matches!(p.check.group(), "compat" | "depends"))
        {
            let row = format!(
                "{name}\t{}\t{}\t{}\t{}",
                p.check.group(),
                p.severity.as_str(),
                p.message,
                p.loc
            );
            assert!(out.contains(&row), "missing row {row}\n{out}");
            assert!(
                report
                    .findings
                    .iter()
                    .any(|x| x.message == p.message && x.loc == p.loc),
                "{name}: {}",
                p.message
            );
        }
    }
}

/// The fixture workspace with meta-mine (which depends on core, and has a class at the same
/// path as meta-core) and meta-vendor (compatible with scarthgap only) listed before
/// meta-core in `BBLAYERS`.
fn misordered_ws() -> (tempfile::TempDir, PathBuf) {
    let (tmp, root) = common::temp_ws();
    let root = fs::canonicalize(root).unwrap();
    // meta-mine (depends on core) first; it also has a class at the same path as meta-core.
    let conf = root.join("build/conf/bblayers.conf");
    let text = fs::read_to_string(&conf).unwrap().replace(
        "  ${LAYERS_ROOT}/meta-core \\\n  ${TOPDIR}/../meta-vendor \\\n  ${TOPDIR}/../meta-mine \\\n",
        "  ${TOPDIR}/../meta-mine \\\n  ${TOPDIR}/../meta-vendor \\\n  ${LAYERS_ROOT}/meta-core \\\n",
    );
    assert!(
        text.contains("meta-mine \\\n  ${TOPDIR}/../meta-vendor"),
        "{text}"
    );
    fs::write(&conf, text).unwrap();
    let class = root.join("meta-mine/classes-recipe/autotoolsish.bbclass");
    fs::create_dir_all(class.parent().unwrap()).unwrap();
    fs::write(&class, "# A copy that shadows meta-core's.\n").unwrap();
    // meta-vendor claims only scarthgap: not compatible with the wrynose core.
    let vconf = root.join("meta-vendor/conf/layer.conf");
    let text = fs::read_to_string(&vconf)
        .unwrap()
        .replace("\"scarthgap wrynose\"", "\"scarthgap\"");
    fs::write(&vconf, text).unwrap();
    (tmp, root)
}

#[tokio::test]
async fn bblayers_order() {
    let (_tmp, root) = misordered_ws();
    let f = fixture_with(common::ws_config(&root));
    let out = compat(&f, None).await;
    insta::assert_snapshot!("layers_compat_order", out);
    assert!(
        out.contains("meta-mine\torder\twarning\tmeta-mine comes before meta-core in BBLAYERS"),
        "{out}"
    );
    assert!(
        out.contains("so its classes-recipe/autotoolsish.bbclass is found instead of meta-core's"),
        "{out}"
    );
    assert!(
        out.contains("meta-vendor\torder\tinfo\tmeta-vendor comes before meta-core in BBLAYERS"),
        "{out}"
    );
    assert!(out.contains("meta-vendor\tcompat\terror\t"), "{out}");
}

/// `layers::order_problems` is the one implementation: its typed rows are the `order` rows
/// of `layers show=compat` and of the audit report's section 3, field for field.
#[tokio::test]
async fn order_rows_are_typed_and_shared_with_the_audit() {
    let (_tmp, root) = misordered_ws();
    let f = fixture_with(common::ws_config(&root));
    let rows = {
        let index = f.ctx.index.lock().await;
        layers::order_problems(&index, &f.ctx.workspace).unwrap()
    };
    let got: Vec<(String, Severity)> = rows.iter().map(|o| (o.layer.clone(), o.severity)).collect();
    assert_eq!(
        got,
        [
            ("meta-mine".to_string(), Severity::Warning),
            ("meta-vendor".to_string(), Severity::Info)
        ]
    );
    assert!(
        rows.iter()
            .all(|o| o.loc.starts_with("build:conf/bblayers.conf:")),
        "{rows:#?}"
    );

    // The view's rows, read back from its JSON.
    let json: JsonTable = serde_json::from_str(&compat(&f, Some(Format::Json)).await).unwrap();
    let view: Vec<Vec<String>> = json
        .rows
        .iter()
        .filter(|r| r.get(1).map(String::as_str) == Some("order"))
        .map(|r| r.iter().map(|c| render::unescape_cell(c)).collect())
        .collect();
    let typed: Vec<Vec<String>> = rows
        .iter()
        .map(|o| {
            vec![
                o.layer.clone(),
                "order".into(),
                o.severity.as_str().into(),
                o.message.clone(),
                o.loc.clone(),
            ]
        })
        .collect();
    assert_eq!(view, typed);

    // The audit's, built from the typed rows.
    let a = audit::build(&f.ctx, &audit::Options::new(&root))
        .await
        .unwrap();
    let from_audit: Vec<Vec<String>> = a
        .structure
        .order
        .iter()
        .map(|p| {
            vec![
                p.layer.clone(),
                p.check.clone(),
                p.severity.clone(),
                p.message.clone(),
                p.pointer.clone(),
            ]
        })
        .collect();
    assert_eq!(from_audit, typed);
}
