//! T6.1: the scaffold engine and the `scaffold` tool, on a temporary copy of the fixture
//! workspace, driven by test-only generators and a recording post-write lint.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use common::*;
use sous_chef::bitbake::JobTable;
use sous_chef::error::{Error, Result};
use sous_chef::handles::{Handle, HandleStore};
use sous_chef::index::Index;
use sous_chef::lint::{Finding, Severity};
use sous_chef::mcp::Ctx;
use sous_chef::mcp::tools::scaffold::{self as tool, Args, JsonOutput, Kind};
use sous_chef::render::Format;
use sous_chef::scaffold::{
    self, FileAction, GenContext, Generator, LintTarget, Plan, PlannedFile, PostWriteLint,
};
use sous_chef::types::{LineSpan, Loc};
use sous_chef::workspace::Workspace;

// ---------------------------------------------------------------------------------------
// Harness

struct Env {
    _tmp: tempfile::TempDir,
    _cache: tempfile::TempDir,
    root: PathBuf,
    ctx: Ctx,
}

impl Env {
    fn new() -> Env {
        let (tmp, root) = temp_ws();
        let cache = tempfile::tempdir().unwrap();
        let config = ws_config(&root);
        let workspace = Workspace::discover(&config).unwrap();
        let index = Index::open_in(cache.path(), &workspace).unwrap();
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

    /// Canonical path of `rel` under the workspace copy (the copy's parent may be a
    /// symlinked temp dir; layer roots are canonical).
    fn path(&self, rel: &str) -> PathBuf {
        fs::canonicalize(&self.root).unwrap().join(rel)
    }

    fn mine(&self, rel: &str) -> PathBuf {
        self.path(&format!("meta-mine/{rel}"))
    }
}

type PlanFn = dyn Fn(&GenContext<'_>) -> Result<Plan> + Sync;

/// A generator that returns whatever plan its closure builds.
struct TestGen {
    kind: Kind,
    options: &'static [&'static str],
    plan: Box<PlanFn>,
}

impl TestGen {
    fn new(plan: impl Fn(&GenContext<'_>) -> Result<Plan> + Sync + 'static) -> TestGen {
        TestGen {
            kind: Kind::Recipe,
            options: &["category", "source"],
            plan: Box::new(plan),
        }
    }

    fn fixed(plan: Plan) -> TestGen {
        TestGen::new(move |_| Ok(plan.clone()))
    }
}

impl Generator for TestGen {
    fn kind(&self) -> Kind {
        self.kind
    }
    fn options(&self) -> &'static [&'static str] {
        self.options
    }
    fn plan(&self, cx: &GenContext<'_>) -> Result<Plan> {
        (self.plan)(cx)
    }
}

/// Records which files it was asked to lint and returns canned findings for them.
#[derive(Default)]
struct RecLint {
    calls: Mutex<Vec<Vec<PathBuf>>>,
    fail: bool,
}

impl PostWriteLint for RecLint {
    fn lint(&self, t: &LintTarget<'_>) -> Result<Vec<Finding>> {
        self.calls.lock().unwrap().push(t.files.to_vec());
        if self.fail {
            return Err(Error::Invalid {
                msg: "lint exploded".into(),
            });
        }
        Ok(t.files
            .iter()
            .filter_map(|p| t.ws.loc_for(p, LineSpan::line(1)))
            .map(|loc| Finding {
                rule: "org/recipe-path",
                severity: Severity::Warning,
                loc,
                message: "test finding".into(),
            })
            .collect())
    }
}

fn args(write: bool) -> Args {
    Args {
        kind: Kind::Recipe,
        layer: None,
        name: Some("foo".into()),
        version: None,
        options: serde_json::Map::new(),
        write,
        budget_tokens: None,
        format: None,
    }
}

fn create(path: PathBuf, content: &str) -> PlannedFile {
    PlannedFile {
        path,
        content: content.into(),
        action: FileAction::Create,
    }
}

fn modify(path: PathBuf, content: &str, before_sha: String) -> PlannedFile {
    PlannedFile {
        path,
        content: content.into(),
        action: FileAction::Modify { before_sha },
    }
}

fn plan(files: Vec<PlannedFile>) -> Plan {
    Plan {
        files,
        notes: vec!["LIC_FILES_CHKSUM must be supplied".into()],
        new_layer: None,
    }
}

async fn run(env: &Env, a: Args, g: &TestGen, lint: &RecLint) -> Result<String> {
    tool::run_with(&env.ctx, a, Some(g), lint).await
}

const FOO: &str = "SUMMARY = \"foo\"\nLICENSE = \"MIT\"\nSRC_URI = \"file://foo.c\"\n";

fn tmp_leftovers(dir: &Path) -> Vec<String> {
    fs::read_dir(dir)
        .unwrap()
        .filter_map(|e| e.ok())
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| n.contains(".tmp"))
        .collect()
}

// ---------------------------------------------------------------------------------------
// Dry run vs write

#[tokio::test]
async fn dry_run_shows_table_and_content_and_writes_nothing() {
    let env = Env::new();
    let target = env.mine("recipes-foo/foo/foo_1.0.bb");
    let g = TestGen::fixed(plan(vec![create(target.clone(), FOO)]));
    let lint = RecLint::default();

    let out = run(&env, args(false), &g, &lint).await.unwrap();

    let mut lines = out.lines();
    assert_eq!(lines.next(), Some("action\tpath\tlines"));
    assert_eq!(
        lines.next(),
        Some("create\tmeta-mine:recipes-foo/foo/foo_1.0.bb\t3")
    );
    assert!(out.contains("dry run: nothing written"), "{out}");
    assert!(
        out.contains("note: LIC_FILES_CHKSUM must be supplied"),
        "{out}"
    );
    assert!(
        out.contains(&format!(
            "--- create meta-mine:recipes-foo/foo/foo_1.0.bb\n{FOO}"
        )),
        "{out}"
    );
    assert!(!target.exists());
    assert!(!env.mine("recipes-foo").exists(), "no directory created");
    assert!(
        lint.calls.lock().unwrap().is_empty(),
        "no lint on a dry run"
    );
}

#[tokio::test]
async fn write_creates_the_file_via_rename_and_reports_it() {
    let env = Env::new();
    let target = env.mine("recipes-foo/foo/foo_1.0.bb");
    let g = TestGen::fixed(plan(vec![create(target.clone(), FOO)]));
    let lint = RecLint::default();

    let out = run(&env, args(true), &g, &lint).await.unwrap();

    assert_eq!(fs::read_to_string(&target).unwrap(), FOO);
    assert!(tmp_leftovers(target.parent().unwrap()).is_empty());
    assert!(
        out.starts_with(
            "action\tpath\tlines\tresult\ncreate\tmeta-mine:recipes-foo/foo/foo_1.0.bb\t3\twritten\n"
        ),
        "{out}"
    );
    assert!(out.contains("wrote 1 file"), "{out}");
    assert!(
        !out.contains("--- create"),
        "no content echo after a write: {out}"
    );
}

#[tokio::test]
async fn failing_index_refresh_is_a_note_not_an_error() {
    // `Index::refresh` is the M0 stub ("not implemented") until T2.2 lands; once it is real,
    // the refresh succeeds and the note is absent. Either way the scaffold succeeds.
    let env = Env::new();
    let target = env.mine("recipes-foo/foo/foo_1.0.bb");
    let g = TestGen::fixed(plan(vec![create(target.clone(), FOO)]));
    let refresh_fails = env
        .ctx
        .index
        .lock()
        .await
        .refresh(&env.ctx.workspace)
        .is_err();

    let out = run(&env, args(true), &g, &RecLint::default())
        .await
        .unwrap();

    assert!(target.exists());
    assert_eq!(
        out.contains("note: index refresh failed"),
        refresh_fails,
        "{out}"
    );
}

#[tokio::test]
async fn json_output_carries_the_same_data() {
    let env = Env::new();
    let target = env.mine("recipes-foo/foo/foo_1.0.bb");
    let g = TestGen::fixed(plan(vec![create(target, FOO)]));
    let mut a = args(false);
    a.format = Some(Format::Json);

    let out = run(&env, a, &g, &RecLint::default()).await.unwrap();
    let j: JsonOutput = serde_json::from_str(&out).unwrap();

    assert_eq!(j.kind, "recipe");
    assert!(!j.written);
    assert_eq!(j.files.header, ["action", "path", "lines"]);
    assert_eq!(
        j.files.rows,
        [["create", "meta-mine:recipes-foo/foo/foo_1.0.bb", "3"]]
    );
    assert_eq!(j.contents.len(), 1);
    assert_eq!(j.contents[0].content, FOO);
    assert!(j.content_handle.is_none());
    assert!(j.lint.is_none());
}

#[tokio::test]
async fn content_over_the_cap_goes_to_a_handle() {
    let env = Env::new();
    let big: String = (0..800)
        .map(|i| format!("# filler line {i:04}\n"))
        .collect();
    assert!(big.len() > tool::CONTENT_CAP_BYTES);
    let g = TestGen::fixed(plan(vec![create(
        env.mine("recipes-foo/foo/foo_1.0.bb"),
        &big,
    )]));

    let out = run(&env, args(false), &g, &RecLint::default())
        .await
        .unwrap();

    assert!(out.len() <= tool::CONTENT_CAP_BYTES, "{} bytes", out.len());
    assert!(out.contains("over the 8 KB cap: stored as h:"), "{out}");
    assert!(!out.contains("--- create"), "{out}");
    let h = out
        .split_whitespace()
        .find(|w| w.starts_with("h:") && w.trim_end_matches(';').len() == 12)
        .unwrap()
        .trim_end_matches(';');
    let stored = env.ctx.handles.get(&Handle(h.to_string())).unwrap();
    let stored = String::from_utf8(stored).unwrap();
    assert!(stored.contains(&format!(
        "--- create meta-mine:recipes-foo/foo/foo_1.0.bb\n{big}"
    )));
}

// ---------------------------------------------------------------------------------------
// Refusals

#[tokio::test]
async fn refuses_a_non_writable_target_layer() {
    let env = Env::new();
    let target = env.path("meta-core/recipes-foo/foo/foo_1.0.bb");
    let g = TestGen::fixed(plan(vec![create(target.clone(), FOO)]));

    // Named as the target layer.
    let mut a = args(true);
    a.layer = Some("meta-core".into());
    let e = run(&env, a, &g, &RecLint::default()).await.unwrap_err();
    let msg = e.to_string();
    assert!(msg.contains("layer meta-core is not writable"), "{msg}");
    assert!(msg.contains("writable layers: meta-mine"), "{msg}");

    // A plan file inside it, with no `layer` argument, in both modes.
    for write in [false, true] {
        let e = run(&env, args(write), &g, &RecLint::default())
            .await
            .unwrap_err();
        assert!(e.to_string().contains("meta-core is not writable"), "{e}");
    }
    // `..` from a writable layer into a read-only one.
    let sneaky = env.mine("../meta-core/recipes-foo/foo/foo_1.0.bb");
    let g2 = TestGen::fixed(plan(vec![create(sneaky, FOO)]));
    let e = run(&env, args(true), &g2, &RecLint::default())
        .await
        .unwrap_err();
    assert!(e.to_string().contains("meta-core is not writable"), "{e}");
    assert!(!target.exists());
    assert!(!env.path("meta-core/recipes-foo").exists());
}

#[tokio::test]
async fn refuses_unknown_layer_names_listing_the_layers() {
    let env = Env::new();
    let g = TestGen::fixed(plan(vec![]));
    let mut a = args(false);
    a.layer = Some("meta-nope".into());
    let e = run(&env, a, &g, &RecLint::default()).await.unwrap_err();
    assert!(matches!(e, Error::NotFound { .. }), "{e}");
    assert!(
        e.to_string().contains("meta-core, meta-vendor, meta-mine"),
        "{e}"
    );
}

#[tokio::test]
async fn refuses_paths_that_escape_the_workspace() {
    let env = Env::new();
    let outside = env.path("../escaped.bb");
    let g = TestGen::fixed(plan(vec![create(env.mine("../../escaped.bb"), FOO)]));
    for write in [false, true] {
        let e = run(&env, args(write), &g, &RecLint::default())
            .await
            .unwrap_err();
        assert!(matches!(e, Error::Confined { .. }), "{e}");
    }
    assert!(!outside.exists());

    // Relative paths are a generator bug.
    let g = TestGen::fixed(plan(vec![create("recipes-foo/foo/foo_1.0.bb".into(), FOO)]));
    let e = run(&env, args(true), &g, &RecLint::default())
        .await
        .unwrap_err();
    assert!(e.to_string().contains("relative path"), "{e}");
}

#[cfg(unix)]
#[tokio::test]
async fn refuses_a_symlink_that_leads_out_of_the_layer() {
    let env = Env::new();
    let away = tempfile::tempdir().unwrap();
    std::os::unix::fs::symlink(away.path(), env.mine("recipes-link")).unwrap();
    let g = TestGen::fixed(plan(vec![create(
        env.mine("recipes-link/foo/foo_1.0.bb"),
        FOO,
    )]));
    let e = run(&env, args(true), &g, &RecLint::default())
        .await
        .unwrap_err();
    assert!(matches!(e, Error::Confined { .. }), "{e}");
    assert!(!away.path().join("foo").exists());
}

#[tokio::test]
async fn refuses_to_overwrite_on_create() {
    let env = Env::new();
    let existing = env.mine("recipes-app/app/app_0.1.bb");
    let before = fs::read(&existing).unwrap();
    let g = TestGen::fixed(plan(vec![create(existing.clone(), FOO)]));
    for write in [false, true] {
        let e = run(&env, args(write), &g, &RecLint::default())
            .await
            .unwrap_err();
        let msg = e.to_string();
        assert!(
            msg.contains("meta-mine:recipes-app/app/app_0.1.bb already exists"),
            "{msg}"
        );
    }
    assert_eq!(fs::read(&existing).unwrap(), before);
}

#[tokio::test]
async fn modify_applies_when_the_hash_matches() {
    let env = Env::new();
    let file = env.mine("recipes-core/hello/hello_1.1.bbappend");
    let old = fs::read_to_string(&file).unwrap();
    let new = format!("{old}EXTRA = \"1\"\n");
    let sha = scaffold::file_sha256(&file).unwrap();
    let g = TestGen::fixed(plan(vec![modify(file.clone(), &new, sha)]));

    let out = run(&env, args(true), &g, &RecLint::default())
        .await
        .unwrap();

    assert!(
        out.contains("modify\tmeta-mine:recipes-core/hello/hello_1.1.bbappend\t"),
        "{out}"
    );
    assert_eq!(fs::read_to_string(&file).unwrap(), new);
    assert!(tmp_leftovers(file.parent().unwrap()).is_empty());
}

#[tokio::test]
async fn refuses_modify_with_a_stale_before_sha() {
    let env = Env::new();
    let file = env.mine("recipes-core/hello/hello_1.1.bbappend");
    let old = fs::read(&file).unwrap();
    let stale = scaffold::sha256_hex(b"what the file used to be");
    let g = TestGen::fixed(plan(vec![modify(file.clone(), "X = \"1\"\n", stale)]));
    for write in [false, true] {
        let e = run(&env, args(write), &g, &RecLint::default())
            .await
            .unwrap_err();
        let msg = e.to_string();
        assert!(msg.contains("changed since the plan was made"), "{msg}");
        assert!(msg.contains("re-run scaffold"), "{msg}");
    }
    assert_eq!(fs::read(&file).unwrap(), old);

    // A Modify of a file that does not exist.
    let gone = env.mine("recipes-core/hello/gone.bbappend");
    let g = TestGen::fixed(plan(vec![modify(gone, "", scaffold::sha256_hex(b""))]));
    let e = run(&env, args(true), &g, &RecLint::default())
        .await
        .unwrap_err();
    assert!(e.to_string().contains("no longer exists"), "{e}");
}

#[tokio::test]
async fn a_bad_file_anywhere_in_the_plan_writes_nothing() {
    let env = Env::new();
    let first = env.mine("recipes-foo/foo/foo_1.0.bb");
    let g = TestGen::fixed(plan(vec![
        create(first.clone(), FOO),
        create(env.mine("recipes-app/app/app_0.1.bb"), FOO),
    ]));
    let e = run(&env, args(true), &g, &RecLint::default())
        .await
        .unwrap_err();
    assert!(e.to_string().contains("already exists"), "{e}");
    assert!(!first.exists());

    let g = TestGen::fixed(plan(vec![
        create(first.clone(), FOO),
        create(first.clone(), FOO),
    ]));
    let e = run(&env, args(true), &g, &RecLint::default())
        .await
        .unwrap_err();
    assert!(e.to_string().contains("twice"), "{e}");
    assert!(!first.exists());
}

// ---------------------------------------------------------------------------------------
// Options and dispatch

#[tokio::test]
async fn unknown_options_are_an_error_listing_the_valid_ones() {
    let env = Env::new();
    let g = TestGen::fixed(plan(vec![]));
    let mut a = args(false);
    a.options.insert("colour".into(), "blue".into());
    a.options.insert("category".into(), "core".into());
    let e = run(&env, a, &g, &RecLint::default()).await.unwrap_err();
    assert_eq!(
        e.to_string(),
        "unknown option `colour` for kind recipe; valid options: category, source"
    );
}

#[tokio::test]
async fn generators_see_the_request() {
    let env = Env::new();
    let g = TestGen::new(|cx| {
        let layer = cx.target_layer()?;
        let category = cx.opt_str("category")?.unwrap_or("misc");
        let name = cx.request.name.clone().unwrap_or_default();
        Ok(plan(vec![create(
            layer
                .root
                .join(format!("recipes-{category}/{name}/{name}_1.0.bb")),
            FOO,
        )]))
    });
    let mut a = args(false);
    a.options.insert("category".into(), "net".into());
    let out = run(&env, a, &g, &RecLint::default()).await.unwrap();
    assert!(
        out.contains("create\tmeta-mine:recipes-net/foo/foo_1.0.bb\t3"),
        "{out}"
    );

    let mut a = args(false);
    a.options.insert("category".into(), 3.into());
    let e = run(&env, a, &g, &RecLint::default()).await.unwrap_err();
    assert!(
        e.to_string().contains("option `category` must be a string"),
        "{e}"
    );
}

#[tokio::test]
async fn kinds_without_a_generator_return_a_one_line_error() {
    let env = Env::new();
    for kind in Kind::ALL {
        if scaffold::generator(kind).is_some() {
            continue;
        }
        let mut a = args(true);
        a.kind = kind;
        let e = tool::run(&env.ctx, a).await.unwrap_err();
        let msg = e.to_string();
        assert!(msg.contains("is not implemented yet"), "{msg}");
        assert!(msg.contains(kind.as_str()), "{msg}");
        assert!(!msg.contains('\n'));
    }
}

#[tokio::test]
async fn the_registered_tool_parses_arguments() {
    let env = Env::new();
    let entry = sous_chef::mcp::find_tool("scaffold").unwrap();
    let args = serde_json::json!({"kind": "inc_split", "name": "hello"});
    let e = (entry.call)(&env.ctx, args.as_object().unwrap().clone())
        .await
        .unwrap_err();
    assert!(e.to_string().contains("inc_split"), "{e}");
    let bad = serde_json::json!({"kind": "potato"});
    let e = (entry.call)(&env.ctx, bad.as_object().unwrap().clone())
        .await
        .unwrap_err();
    assert!(e.to_string().contains("invalid arguments"), "{e}");
}

#[tokio::test]
async fn where_writes_nothing_and_needs_no_writable_layer() {
    let env = Env::new();
    let mut g = TestGen::fixed(Plan {
        files: vec![],
        notes: vec!["put it in a bbappend in meta-mine".into()],
        new_layer: None,
    });
    g.kind = Kind::Where;
    let mut a = args(true);
    a.kind = Kind::Where;
    a.layer = Some("meta-core".into());
    let lint = RecLint::default();
    let out = run(&env, a.clone(), &g, &lint).await.unwrap();
    assert_eq!(
        out,
        "action\tpath\tlines\nnote: put it in a bbappend in meta-mine\n"
    );
    assert!(lint.calls.lock().unwrap().is_empty());

    let mut g = TestGen::fixed(plan(vec![create(env.mine("x.bb"), FOO)]));
    g.kind = Kind::Where;
    let e = run(&env, a, &g, &lint).await.unwrap_err();
    assert!(e.to_string().contains("must not plan files"), "{e}");
    assert!(!env.mine("x.bb").exists());
}

// ---------------------------------------------------------------------------------------
// Post-write lint

#[tokio::test]
async fn post_write_lint_runs_on_exactly_the_written_files() {
    let env = Env::new();
    let created = env.mine("recipes-foo/foo/foo_1.0.bb");
    let modified = env.mine("recipes-core/hello/hello_1.1.bbappend");
    let sha = scaffold::file_sha256(&modified).unwrap();
    let g = TestGen::fixed(plan(vec![
        create(created.clone(), FOO),
        modify(modified.clone(), "X = \"1\"\n", sha),
    ]));
    let lint = RecLint::default();

    let out = run(&env, args(true), &g, &lint).await.unwrap();

    assert_eq!(*lint.calls.lock().unwrap(), [vec![created, modified]]);
    assert!(
        out.contains("lint: 2 findings on the 2 written files"),
        "{out}"
    );
    let findings = out.split("\n\n").nth(1).unwrap();
    assert_eq!(
        findings,
        "severity\trule\tloc\tmessage\n\
         warning\torg/recipe-path\tmeta-mine:recipes-foo/foo/foo_1.0.bb:1\ttest finding\n\
         warning\torg/recipe-path\tmeta-mine:recipes-core/hello/hello_1.1.bbappend:1\ttest finding\n"
    );

    // JSON carries the findings too.
    let created2 = env.mine("recipes-foo/bar/bar_1.0.bb");
    let g = TestGen::fixed(plan(vec![create(created2, FOO)]));
    let mut a = args(true);
    a.format = Some(Format::Json);
    let j: JsonOutput = serde_json::from_str(&run(&env, a, &g, &lint).await.unwrap()).unwrap();
    assert!(j.written);
    let l = j.lint.unwrap();
    assert_eq!(l.rows.len(), 1);
    assert_eq!(l.rows[0][2], "meta-mine:recipes-foo/bar/bar_1.0.bb:1");
}

#[tokio::test]
async fn failing_lint_is_a_note_and_the_files_stay_written() {
    let env = Env::new();
    let created = env.mine("recipes-foo/foo/foo_1.0.bb");
    let g = TestGen::fixed(plan(vec![create(created.clone(), FOO)]));
    let lint = RecLint {
        fail: true,
        ..RecLint::default()
    };
    let out = run(&env, args(true), &g, &lint).await.unwrap();
    assert!(created.exists());
    assert!(
        out.contains("note: lint failed on the written files (lint exploded)"),
        "{out}"
    );
}

#[tokio::test]
async fn the_real_rule_lint_runs_on_written_files() {
    // With the M0 rule stubs this finds nothing; once T4.x rules land, a generated file that
    // trips a file-scope rule shows up here. Run through `run`, not `run_with`.
    let env = Env::new();
    let created = env.mine("recipes-foo/foo/foo_1.0.bb");
    let g = TestGen::fixed(plan(vec![create(created.clone(), FOO)]));
    let out = tool::run_with(&env.ctx, args(true), Some(&g), &scaffold::RuleLint)
        .await
        .unwrap();
    assert!(created.exists());
    assert!(out.contains("lint: "), "{out}");
    assert!(!out.contains("lint failed"), "{out}");
}

// ---------------------------------------------------------------------------------------
// New layers (the T6.3 confinement rule)

#[tokio::test]
async fn a_new_layer_beside_the_existing_ones_may_be_created() {
    let env = Env::new();
    let root = env.path("meta-new");
    let bblayers = env.path("build/conf/bblayers.conf");
    let old = fs::read_to_string(&bblayers).unwrap();
    let sha = scaffold::file_sha256(&bblayers).unwrap();
    let mut g = TestGen::fixed(Plan {
        files: vec![
            create(root.join("conf/layer.conf"), "BBPATH .= \":${LAYERDIR}\"\n"),
            modify(bblayers.clone(), &format!("{old}# new\n"), sha),
        ],
        notes: vec![],
        new_layer: Some(root.clone()),
    });
    g.kind = Kind::Layer;
    let mut a = args(true);
    a.kind = Kind::Layer;
    let lint = RecLint::default();
    let out = run(&env, a.clone(), &g, &lint).await.unwrap();
    assert!(
        out.contains("create\tmeta-new:conf/layer.conf\t1\twritten"),
        "{out}"
    );
    assert!(out.contains("modify\tbuild:conf/bblayers.conf\t"), "{out}");
    assert!(root.join("conf/layer.conf").exists());
    assert!(fs::read_to_string(&bblayers).unwrap().ends_with("# new\n"));

    // A second run: the layer now exists.
    let e = run(&env, a, &g, &lint).await.unwrap_err();
    assert!(e.to_string().contains("cannot create layer"), "{e}");
}

#[tokio::test]
async fn new_layer_rules_refuse_other_places() {
    let env = Env::new();
    let away = tempfile::tempdir().unwrap();
    let mut a = args(false);
    a.kind = Kind::Layer;

    // Not in a directory holding a workspace layer.
    let mut g = TestGen::fixed(Plan {
        files: vec![create(away.path().join("meta-x/conf/layer.conf"), "")],
        notes: vec![],
        new_layer: Some(away.path().join("meta-x")),
    });
    g.kind = Kind::Layer;
    let e = run(&env, a.clone(), &g, &RecLint::default())
        .await
        .unwrap_err();
    assert!(
        e.to_string().contains("already holds a workspace layer"),
        "{e}"
    );

    // Inside an existing layer.
    let mut g = TestGen::fixed(Plan {
        files: vec![],
        notes: vec![],
        new_layer: Some(env.mine("meta-sub")),
    });
    g.kind = Kind::Layer;
    let e = run(&env, a.clone(), &g, &RecLint::default())
        .await
        .unwrap_err();
    assert!(e.to_string().contains("overlaps an existing layer"), "{e}");

    // A file of the plan outside the new layer and outside every writable layer.
    let root = env.path("meta-new");
    let mut g = TestGen::fixed(Plan {
        files: vec![create(env.path("build/conf/other.conf"), "")],
        notes: vec![],
        new_layer: Some(root),
    });
    g.kind = Kind::Layer;
    let e = run(&env, a.clone(), &g, &RecLint::default())
        .await
        .unwrap_err();
    assert!(e.to_string().contains("build directory"), "{e}");

    // bblayers.conf without a new layer.
    let bblayers = env.path("build/conf/bblayers.conf");
    let sha = scaffold::file_sha256(&bblayers).unwrap();
    let g = TestGen::fixed(plan(vec![modify(bblayers, "", sha)]));
    let e = run(&env, args(false), &g, &RecLint::default())
        .await
        .unwrap_err();
    assert!(e.to_string().contains("build directory"), "{e}");
}

#[test]
fn loc_display_matches_pointer_rows() {
    // The findings table uses `Loc` display; keep the format pinned here.
    let l = Loc {
        layer: "meta-mine".into(),
        path: "a/b.bb".into(),
        lines: LineSpan::line(1),
    };
    assert_eq!(l.to_string(), "meta-mine:a/b.bb:1");
}
