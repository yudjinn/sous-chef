//! Fixes for the unflagged wrong answers the differential test against BitBake found
//! (docs/differential.md, "Bugs"). Each test runs on a temporary copy of the fixture
//! workspace; the fixture itself is not changed.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;
mod kas_support;

use std::fs;
use std::path::{Path, PathBuf};

use common::*;
use sous_chef::eval::{self, ConfigCache, DataStore, RefProblem, Target};
use sous_chef::index::Index;
use sous_chef::index::query;
use sous_chef::release::Feature;
use sous_chef::workspace::Workspace;
use sous_chef::workspace::resolve;

struct Fx {
    _cache: tempfile::TempDir,
    _tmp: tempfile::TempDir,
    root: PathBuf,
    ws: Workspace,
    idx: Index,
}

fn index(ws: Workspace, tmp: tempfile::TempDir, root: PathBuf) -> Fx {
    let cache = tempfile::tempdir().unwrap();
    let mut idx = Index::open_in(cache.path(), &ws).unwrap();
    idx.refresh(&ws).unwrap();
    Fx {
        _cache: cache,
        _tmp: tmp,
        root,
        ws,
        idx,
    }
}

/// A copy of the fixture workspace after `edit(ws root)`, discovered and indexed.
fn modified(edit: impl FnOnce(&Path)) -> Fx {
    let (tmp, root) = temp_ws();
    let root = fs::canonicalize(root).unwrap();
    edit(&root);
    let ws = Workspace::discover(&ws_config(&root)).unwrap();
    index(ws, tmp, root)
}

/// As [`modified`], with `release = "<codename>"` in `sous-chef.toml`.
fn modified_release(codename: &str, edit: impl FnOnce(&Path)) -> Fx {
    let (tmp, root, _cfg, ws) = temp_ws_release(codename, edit);
    index(ws, tmp, root)
}

fn write(root: &Path, rel: &str, text: &str) {
    let p = root.join(rel);
    fs::create_dir_all(p.parent().unwrap()).unwrap();
    fs::write(p, text).unwrap();
}

fn append_to(root: &Path, rel: &str, text: &str) {
    let p = root.join(rel);
    let mut s = fs::read_to_string(&p).unwrap();
    s.push_str(text);
    fs::write(p, s).unwrap();
}

fn target(recipe: Option<&str>) -> Target {
    Target {
        recipe: recipe.map(str::to_string),
        ..Target::default()
    }
}

fn eval(fx: &Fx, t: &Target) -> DataStore {
    eval::evaluate_with(&ConfigCache::new(), &fx.idx, &fx.ws, t).unwrap()
}

fn text(ds: &DataStore, name: &str) -> Option<String> {
    ds.value(name).map(|v| v.text)
}

fn replace_in(root: &Path, rel: &str, from: &str, to: &str) {
    let p = root.join(rel);
    let s = fs::read_to_string(&p).unwrap();
    assert!(s.contains(from), "{rel} has no {from:?}");
    fs::write(p, s.replace(from, to)).unwrap();
}

// ---------------------------------------------------------------------------------------
// Bug 1: configuration fragments (addfragments)
// ---------------------------------------------------------------------------------------

/// OE-core's Wrynose `bitbake.conf` lines 813-822, in the fixture's stand-in.
const FRAGMENTS_4: &str = "include conf/toolcfg.conf\ninclude conf/local.conf\n\n\
OE_FRAGMENTS_PREFIX ?= \"conf/fragments\"\n\
OE_FRAGMENTS_METADATA_VARS ?= \"BB_CONF_FRAGMENT_SUMMARY BB_CONF_FRAGMENT_DESCRIPTION\"\n\
OE_FRAGMENTS_BUILTIN ?= \"machine:MACHINE distro:DISTRO\"\n\
addfragments ${OE_FRAGMENTS_PREFIX} OE_FRAGMENTS OE_FRAGMENTS_METADATA_VARS OE_FRAGMENTS_BUILTIN\n";

/// Walnascar's form (BitBake 2.12): no built-in fragments.
const FRAGMENTS_3: &str = "include conf/toolcfg.conf\ninclude conf/local.conf\n\n\
OE_FRAGMENTS_PREFIX ?= \"conf/fragments\"\n\
OE_FRAGMENTS_METADATA_VARS ?= \"BB_CONF_FRAGMENT_SUMMARY BB_CONF_FRAGMENT_DESCRIPTION\"\n\
addfragments ${OE_FRAGMENTS_PREFIX} OE_FRAGMENTS OE_FRAGMENTS_METADATA_VARS\n";

/// A build configured the way `bitbake-setup` configures it: no `MACHINE` / `DISTRO` in
/// `local.conf`, fragments in `conf/toolcfg.conf`, one named fragment in meta-core.
fn fragment_build(root: &Path, directive: &str, toolcfg: &str) {
    replace_in(
        root,
        "meta-core/conf/bitbake.conf",
        "include conf/local.conf\n",
        directive,
    );
    replace_in(
        root,
        "build/conf/local.conf",
        "MACHINE ??= \"qemux86\"\nDISTRO ?= \"testdistro\"\n",
        "",
    );
    write(root, "build/conf/toolcfg.conf", toolcfg);
    write(
        root,
        "meta-core/conf/fragments/test/extra.conf",
        "BB_CONF_FRAGMENT_SUMMARY = \"An extra setting\"\nEXTRA_FROM_FRAGMENT = \"yes\"\n",
    );
}

const TOOLCFG: &str = "# Automated config file controlled by tools\n\
OE_FRAGMENTS += \"distro/testdistro machine/boardx core/test/extra\"\n";

#[test]
fn machine_and_distro_come_from_builtin_fragments() {
    let fx = modified(|r| fragment_build(r, FRAGMENTS_4, TOOLCFG));
    let ds = eval(&fx, &target(None));
    let machine = ds.value("MACHINE").unwrap();
    assert_eq!(machine.text, "boardx");
    assert!(!machine.unevaluated_python && machine.unresolved.is_empty());
    assert_eq!(text(&ds, "DISTRO").as_deref(), Some("testdistro"));
    // The machine conf is read: MACHINEOVERRIDES =. "socx:" and TARGET_ARCH = "arm".
    let ovr = ds.value("OVERRIDES").unwrap().text;
    assert!(ovr.starts_with("linux:arm:"), "{ovr}");
    assert!(ovr.contains(":socx:boardx:testdistro:"), "{ovr}");
    // The history names the toolcfg.conf line, reached through the addfragments line.
    let hist = ds.history("MACHINE");
    let (op, _) = hist.last().unwrap();
    assert_eq!(op.loc.to_string(), "build:conf/toolcfg.conf:2");
    assert_eq!(
        op.via.last().unwrap().to_string(),
        "meta-core:conf/bitbake.conf:56"
    );
    // The named fragment is included, and its metadata variables become flags.
    assert_eq!(text(&ds, "EXTRA_FROM_FRAGMENT").as_deref(), Some("yes"));
    assert_eq!(ds.value("BB_CONF_FRAGMENT_SUMMARY"), None);
    assert_eq!(
        ds.raw_flag("BB_CONF_FRAGMENT_SUMMARY", "core/test/extra")
            .as_deref(),
        Some("An extra setting")
    );
    assert!(
        ds.files()
            .iter()
            .any(|v| v.loc.path == Path::new("conf/fragments/test/extra.conf")),
    );
    assert!(
        ds.replay().references.is_empty(),
        "{:?}",
        ds.replay().references
    );
    // An explicit target still wins.
    let t = Target {
        machine: Some("qemux86".into()),
        ..Target::default()
    };
    let ds = eval(&fx, &t);
    assert_eq!(text(&ds, "MACHINE").as_deref(), Some("qemux86"));
    assert!(
        ds.replay().references.is_empty(),
        "{:?}",
        ds.replay().references
    );
}

/// A bitbake-setup `bblayers.conf` sets no `BBPATH`: the build directory is on it because
/// the build environment exports `BBPATH=$BUILDDIR`, so `conf/toolcfg.conf` and
/// `conf/local.conf` are found there.
#[test]
fn the_build_directory_is_on_bbpath_without_bblayers_setting_it() {
    let fx = modified(|r| {
        fragment_build(r, FRAGMENTS_4, TOOLCFG);
        replace_in(
            r,
            "build/conf/bblayers.conf",
            "BBPATH = \"${TOPDIR}\"\n",
            "",
        );
    });
    let ds = eval(&fx, &target(None));
    assert_eq!(text(&ds, "MACHINE").as_deref(), Some("boardx"));
    let bbpath = text(&ds, "BBPATH").unwrap();
    assert!(
        bbpath.starts_with(&fx.ws.build_dir.as_ref().unwrap().display().to_string()),
        "{bbpath}"
    );
    assert!(
        ds.files()
            .iter()
            .any(|v| v.loc.layer == "build" && v.loc.path == Path::new("conf/local.conf"))
    );
    // The index resolves the include the same way.
    let conf = query::file_by_path(&fx.idx, "meta-core", Path::new("conf/bitbake.conf"))
        .unwrap()
        .unwrap();
    let edges = query::edges_from(&fx.idx, conf.id).unwrap();
    let e = edges
        .iter()
        .find(|e| e.target_text == "conf/toolcfg.conf")
        .unwrap();
    assert_eq!(e.target.as_ref().unwrap().layer, "build");
}

#[test]
fn a_recipe_is_evaluated_for_the_fragment_machine() {
    let fx = modified(|r| fragment_build(r, FRAGMENTS_4, TOOLCFG));
    let ds = eval(&fx, &target(Some("hello")));
    assert_eq!(text(&ds, "MACHINE").as_deref(), Some("boardx"));
    let ovr = ds.value("OVERRIDES").unwrap().text;
    assert!(ovr.contains(":pn-hello:"), "{ovr}");
    assert!(ovr.contains(":socx:boardx:testdistro:"), "{ovr}");
}

#[test]
fn an_unknown_fragment_is_reported() {
    let fx = modified(|r| {
        fragment_build(
            r,
            FRAGMENTS_4,
            "OE_FRAGMENTS += \"machine/boardx core/test/nosuch\"\n",
        )
    });
    let ds = eval(&fx, &target(None));
    // (No distro fragment here: `include conf/distro/${DISTRO}.conf` is reported too.)
    let refs: Vec<_> = ds
        .replay()
        .references
        .iter()
        .filter(|r| r.kind == "addfragments")
        .collect();
    assert_eq!(refs.len(), 1, "{refs:?}");
    assert_eq!(refs[0].text, "core/test/nosuch");
    assert!(
        matches!(&refs[0].problem, RefProblem::NotFound(w) if w.contains("conf/fragments/test/nosuch.conf")),
        "{refs:?}"
    );
    assert_eq!(
        refs[0].loc.as_ref().unwrap().to_string(),
        "meta-core:conf/bitbake.conf:56"
    );
    assert_eq!(text(&ds, "MACHINE").as_deref(), Some("boardx"));
}

#[test]
fn a_fragment_list_that_needs_python_flags_the_builtin_variables() {
    let fx = modified(|r| {
        fragment_build(r, FRAGMENTS_4, "OE_FRAGMENTS = \"${@'machine/boardx'}\"\n");
        // A weak default is allowed next to a built-in fragment.
        append_to(r, "build/conf/local.conf", "MACHINE ??= \"qemux86\"\n");
    });
    let ds = eval(&fx, &target(None));
    let refs = &ds.replay().references;
    assert!(
        refs.iter()
            .any(|r| r.kind == "addfragments" && r.problem == RefProblem::Python),
        "{refs:?}"
    );
    let m = ds.value("MACHINE").unwrap();
    assert_eq!(m.text, "qemux86");
    assert_eq!(m.unresolved, ["OE_FRAGMENTS"]);
    assert!(ds.value("DISTRO").is_none_or(|d| !d.unresolved.is_empty()));
}

#[test]
fn a_builtin_fragment_next_to_an_assignment_is_fatal() {
    let fx = modified(|r| {
        fragment_build(r, FRAGMENTS_4, TOOLCFG);
        append_to(r, "build/conf/local.conf", "MACHINE = \"qemux86\"\n");
    });
    let ds = eval(&fx, &target(None));
    let refs = &ds.replay().references;
    assert!(
        refs.iter().any(|r| r.text == "machine/boardx"
            && matches!(&r.problem, RefProblem::Fatal(m) if m.contains("already got an assignment"))),
        "{refs:?}"
    );
}

#[test]
fn fragments_on_older_releases() {
    // Scarthgap (BitBake 2.8): no such directive; BitBake fails with "unparsed line".
    let fx = modified_release("scarthgap", |r| fragment_build(r, FRAGMENTS_4, TOOLCFG));
    let ds = eval(&fx, &target(None));
    let refs = &ds.replay().references;
    assert!(
        refs.iter().any(|r| r.kind == "addfragments"
            && r.problem == RefProblem::NotInRelease(Feature::AddFragments)),
        "{refs:?}"
    );
    assert_eq!(ds.value("MACHINE"), None);
    assert_eq!(text(&ds, "EXTRA_FROM_FRAGMENT"), None);

    // Walnascar (BitBake 2.12): three arguments, no built-in fragments; `machine/boardx`
    // is a named fragment of a layer `machine`, which does not exist.
    let fx = modified_release("walnascar", |r| fragment_build(r, FRAGMENTS_3, TOOLCFG));
    let ds = eval(&fx, &target(None));
    assert_eq!(text(&ds, "EXTRA_FROM_FRAGMENT").as_deref(), Some("yes"));
    assert_eq!(ds.value("MACHINE"), None);
    let refs = &ds.replay().references;
    let words: Vec<&str> = refs
        .iter()
        .filter(|r| r.kind == "addfragments")
        .map(|r| r.text.as_str())
        .collect();
    assert_eq!(words, ["distro/testdistro", "machine/boardx"], "{refs:?}");

    // The four-argument form on Walnascar: the first group takes two words (greedy), so
    // the fragments variable is OE_FRAGMENTS_METADATA_VARS, whose words have no `/`.
    let fx = modified_release("walnascar", |r| fragment_build(r, FRAGMENTS_4, TOOLCFG));
    let ds = eval(&fx, &target(None));
    assert!(
        ds.replay()
            .references
            .iter()
            .any(|r| matches!(&r.problem, RefProblem::Fatal(m) if m.contains("no `/`"))),
        "{:?}",
        ds.replay().references
    );

    // The three-argument form on Wrynose is an unparsed line.
    let fx = modified(|r| fragment_build(r, FRAGMENTS_3, TOOLCFG));
    let ds = eval(&fx, &target(None));
    assert!(
        ds.replay()
            .references
            .iter()
            .any(|r| matches!(&r.problem, RefProblem::Fatal(m) if m.contains("unparsed line"))),
        "{:?}",
        ds.replay().references
    );
}

#[test]
fn the_addfragments_line_is_a_statement_with_an_edge() {
    let src = "addfragments ${OE_FRAGMENTS_PREFIX} OE_FRAGMENTS OE_FRAGMENTS_METADATA_VARS OE_FRAGMENTS_BUILTIN\n";
    let parsed = sous_chef::parse::parse(src);
    let st = &parsed.statements[0];
    assert_eq!(st.span.start, 1);
    let af = st.add_fragments().unwrap();
    assert_eq!(
        af.groups(4).unwrap(),
        [
            "${OE_FRAGMENTS_PREFIX}",
            "OE_FRAGMENTS",
            "OE_FRAGMENTS_METADATA_VARS",
            "OE_FRAGMENTS_BUILTIN"
        ]
    );
    assert_eq!(
        af.groups(3).unwrap(),
        [
            "${OE_FRAGMENTS_PREFIX} OE_FRAGMENTS",
            "OE_FRAGMENTS_METADATA_VARS",
            "OE_FRAGMENTS_BUILTIN"
        ]
    );
    assert_eq!(af.groups(5), None);
    // Fewer than three arguments never parse.
    let bad = sous_chef::parse::parse("addfragments a b\n");
    assert!(bad.diagnostics[0].message.starts_with("unparsed line"));

    let fx = modified(|r| fragment_build(r, FRAGMENTS_4, TOOLCFG));
    let conf = query::file_by_path(&fx.idx, "meta-core", Path::new("conf/bitbake.conf"))
        .unwrap()
        .unwrap();
    let edges = query::edges_from(&fx.idx, conf.id).unwrap();
    let e = edges.iter().find(|e| e.kind == "addfragments").unwrap();
    assert_eq!(e.line, Some(56));
    assert!(e.target.is_none());
    assert_eq!(
        e.target_text,
        "${OE_FRAGMENTS_PREFIX} OE_FRAGMENTS OE_FRAGMENTS_METADATA_VARS OE_FRAGMENTS_BUILTIN"
    );
    let stmts = query::statements_in(&fx.idx, conf.id, &Default::default()).unwrap();
    assert!(stmts.iter().any(|s| s.kind == "addfragments"));
}

// ---------------------------------------------------------------------------------------
// Item 5: the machine and distro an evaluation uses by default
// ---------------------------------------------------------------------------------------

fn ctx_for(fx: &Fx) -> sous_chef::mcp::Ctx {
    let config = ws_config(&fx.root);
    let workspace = Workspace::discover(&config).unwrap();
    let mut index = Index::open_in(fx._cache.path(), &workspace).unwrap();
    index.refresh(&workspace).unwrap();
    let handles =
        sous_chef::handles::HandleStore::open(&index.cache_dir().join("handles")).unwrap();
    sous_chef::mcp::Ctx {
        config,
        workspace,
        index: tokio::sync::Mutex::new(index),
        handles,
        jobs: sous_chef::bitbake::JobTable::new(),
    }
}

fn boardx_dep(root: &Path) {
    fragment_build(root, FRAGMENTS_4, TOOLCFG);
    append_to(
        root,
        "meta-mine/recipes-app/app/app_0.1.bb",
        "\nDEPENDS:append:boardx = \" boardx-only-dep\"\n",
    );
}

#[tokio::test]
async fn deps_without_a_machine_uses_the_fragment_machine() {
    use sous_chef::mcp::tools::deps;
    let fx = modified(boardx_dep);
    let ctx = ctx_for(&fx);
    let out = deps::run(
        &ctx,
        deps::Args {
            mc: None,
            recipe: "app".into(),
            kind: deps::Kind::Build,
            depth: None,
            budget_tokens: Some(4000),
            format: None,
        },
    )
    .await
    .unwrap();
    assert!(out.contains("boardx-only-dep"), "{out}");
    assert!(!out.contains("no MACHINE is set"), "{out}");
}

#[tokio::test]
async fn var_and_overrides_without_a_machine_use_the_fragment_machine() {
    use sous_chef::mcp::tools::{overrides, var};
    let fx = modified(boardx_dep);
    let ctx = ctx_for(&fx);
    let out = var::run(
        &ctx,
        var::Args {
            mc: None,
            name: "MACHINE".into(),
            recipe: Some("app".into()),
            machine: None,
            distro: None,
            authoritative: false,
            flags: false,
            budget_tokens: Some(4000),
            format: None,
        },
    )
    .await
    .unwrap();
    assert!(out.contains("final: \"boardx\""), "{out}");
    assert!(out.contains("build:conf/toolcfg.conf:2"), "{out}");
    let out = overrides::run(
        &ctx,
        overrides::Args {
            mc: None,
            recipe: Some("app".into()),
            machine: None,
            distro: None,
            show: overrides::Show::Chain,
            budget_tokens: Some(4000),
            format: None,
        },
    )
    .await
    .unwrap();
    assert!(out.contains("boardx"), "{out}");
}

/// Records `MACHINE` as seen by a recipe-scope rule.
struct MachineRule;

impl sous_chef::lint::Rule for MachineRule {
    fn id(&self) -> &'static str {
        "test/machine"
    }
    fn scope(&self) -> sous_chef::lint::Scope {
        sous_chef::lint::Scope::Recipe
    }
    fn default_severity(&self) -> sous_chef::lint::Severity {
        sous_chef::lint::Severity::Info
    }
    fn check(
        &self,
        cx: &sous_chef::lint::LintContext<'_>,
        out: &mut Vec<sous_chef::lint::Finding>,
    ) {
        let m = cx
            .data
            .and_then(|d| d.value("MACHINE"))
            .map(|v| v.text)
            .unwrap_or_default();
        out.push(sous_chef::lint::Finding {
            rule: self.id(),
            severity: sous_chef::lint::Severity::Info,
            loc: cx.loc.clone(),
            message: format!("MACHINE={m}"),
        });
    }
    fn releases(&self) -> std::ops::RangeInclusive<sous_chef::release::Release> {
        sous_chef::release::Release::FLOOR..=sous_chef::release::Release::NEWEST
    }
}

#[test]
fn lint_recipe_evaluation_uses_the_fragment_machine() {
    use sous_chef::lint;
    let fx = modified(boardx_dep);
    let config = ws_config(&fx.root);
    let report = lint::run_with(
        &fx.ws,
        &fx.idx,
        &config,
        &lint::Target::Recipe {
            name: "app".into(),
            version: None,
        },
        &lint::Options {
            rules: vec!["test/machine".into()],
            ..lint::Options::default()
        },
        &[&MachineRule],
        &lint::Replay,
    )
    .unwrap();
    let msgs: Vec<&str> = report.findings.iter().map(|f| f.message.as_str()).collect();
    assert_eq!(msgs, ["MACHINE=boardx"], "{report:?}");
}

#[test]
fn a_kas_project_without_a_build_directory_takes_the_machine_from_the_kas_files() {
    let p = kas_support::Project::before();
    let ws = p.ws();
    assert_eq!(ws.build_dir, None);
    let cache = tempfile::tempdir().unwrap();
    let mut idx = Index::open_in(cache.path(), &ws).unwrap();
    idx.refresh(&ws).unwrap();
    let ds = eval::evaluate_with(&ConfigCache::new(), &idx, &ws, &target(None)).unwrap();
    assert_eq!(text(&ds, "MACHINE").as_deref(), Some("board1"));
    assert_eq!(text(&ds, "DISTRO").as_deref(), Some("projdistro"));
    let notes = &ds.replay().target_notes;
    assert!(
        notes
            .iter()
            .any(|n| n.contains("MACHINE = \"board1\" from the kas file kas/board.yaml:5")),
        "{notes:?}"
    );
    assert!(
        notes
            .iter()
            .any(|n| n.contains("DISTRO = \"projdistro\" from the kas file kas/base.yaml:7")),
        "{notes:?}"
    );
    // An explicit machine wins and needs no note.
    let t = Target {
        machine: Some("other".into()),
        ..Target::default()
    };
    let ds = eval::evaluate_with(&ConfigCache::new(), &idx, &ws, &t).unwrap();
    assert_eq!(text(&ds, "MACHINE").as_deref(), Some("other"));
    assert!(
        !ds.replay()
            .target_notes
            .iter()
            .any(|n| n.starts_with("note: MACHINE")),
    );
    // After the checkout kas has written local.conf: no defaults from the kas files.
    let p = kas_support::Project::after();
    let ws = p.ws();
    let cache = tempfile::tempdir().unwrap();
    let mut idx = Index::open_in(cache.path(), &ws).unwrap();
    idx.refresh(&ws).unwrap();
    let ds = eval::evaluate_with(&ConfigCache::new(), &idx, &ws, &target(None)).unwrap();
    assert!(
        !ds.replay()
            .target_notes
            .iter()
            .any(|n| n.contains("kas project without a build directory")),
        "{:?}",
        ds.replay().target_notes
    );
}

#[tokio::test]
async fn no_machine_at_all_is_said() {
    use sous_chef::mcp::tools::var;
    let fx = modified(|r| {
        replace_in(r, "build/conf/local.conf", "MACHINE ??= \"qemux86\"\n", "");
    });
    let ds = eval(&fx, &target(Some("hello")));
    assert_eq!(ds.value("MACHINE"), None);
    assert!(
        ds.replay()
            .target_notes
            .iter()
            .any(|n| n.starts_with("note: no MACHINE is set")),
        "{:?}",
        ds.replay().target_notes
    );
    let ctx = ctx_for(&fx);
    let out = var::run(
        &ctx,
        var::Args {
            mc: None,
            name: "WORKDIR".into(),
            recipe: Some("hello".into()),
            machine: None,
            distro: None,
            authoritative: false,
            flags: false,
            budget_tokens: Some(4000),
            format: None,
        },
    )
    .await
    .unwrap();
    assert!(out.contains("note: no MACHINE is set"), "{out}");
    // The fixture workspace sets MACHINE in local.conf: no note there.
    let fx = modified(|_| {});
    let ds = eval(&fx, &target(Some("hello")));
    assert!(ds.replay().target_notes.is_empty());
    // Nor when the machine is given.
    let fx = modified(|r| {
        replace_in(r, "build/conf/local.conf", "MACHINE ??= \"qemux86\"\n", "");
    });
    let t = Target {
        recipe: Some("hello".into()),
        machine: Some("qemux86".into()),
        ..Target::default()
    };
    assert!(eval(&fx, &t).replay().target_notes.is_empty());
}

// ---------------------------------------------------------------------------------------
// Bug 3: BB_RECIPE_VIRTUAL_PROVIDERS rewrites DEPENDS
// ---------------------------------------------------------------------------------------

fn virtual_providers(root: &Path) {
    append_to(
        root,
        "meta-core/conf/distro/testdistro.conf",
        "\nBB_RECIPE_VIRTUAL_PROVIDERS = \"virtual/cross-cc virtual/cross-binutils virtual/nothing\"\n\
         PREFERRED_PROVIDER_virtual/cross-cc = \"gcc-cross-${TARGET_ARCH}\"\n\
         PREFERRED_PROVIDER_virtual/cross-binutils = \"binutils-cross-${TARGET_ARCH}\"\n",
    );
    append_to(
        root,
        "meta-mine/recipes-app/app/app_0.1.bb",
        "\nDEPENDS:append = \" virtual/cross-cc  virtual/cross-binutils\"\n",
    );
}

#[test]
fn virtual_providers_in_depends_are_rewritten() {
    let fx = modified(virtual_providers);
    let ds = eval(&fx, &target(Some("app")));
    let deps = ds.value("DEPENDS").unwrap();
    assert_eq!(deps.text, "libfoo gcc-cross-i686 binutils-cross-i686");
    assert!(
        !deps.unevaluated_python && deps.unresolved.is_empty(),
        "{deps:?}"
    );
    // The history ends with the rewrite, at the provider setting it used first.
    let (op, status) = ds.history("DEPENDS").pop().unwrap();
    assert_eq!(status, eval::OpStatus::Applied);
    assert_eq!(op.loc.layer, "meta-core");
    assert_eq!(op.loc.path, Path::new("conf/distro/testdistro.conf"));
    let notes = &ds.replay().var_notes["DEPENDS"];
    assert!(
        notes[0].contains("virtual/cross-cc -> gcc-cross-i686")
            && notes[0].contains("virtual/cross-binutils -> binutils-cross-i686"),
        "{notes:?}"
    );
    // Another machine, another provider: the rewrite follows the configuration.
    let t = Target {
        recipe: Some("app".into()),
        machine: Some("boardx".into()),
        ..Target::default()
    };
    let ds = eval(&fx, &t);
    assert_eq!(
        text(&ds, "DEPENDS").as_deref(),
        Some("libfoo gcc-cross-arm binutils-cross-arm")
    );
}

#[test]
fn virtual_providers_keep_flags_and_report_missing_providers() {
    let fx = modified(|r| {
        virtual_providers(r);
        append_to(
            r,
            "meta-mine/recipes-app/app/app_0.1.bb",
            "DEPENDS += \"virtual/nothing ${@'x'}\"\n",
        );
    });
    let ds = eval(&fx, &target(Some("app")));
    let deps = ds.value("DEPENDS").unwrap();
    assert!(deps.text.contains("gcc-cross-i686"), "{deps:?}");
    // Python in DEPENDS keeps the value flagged after the rewrite.
    assert!(deps.unevaluated_python, "{deps:?}");
    // No PREFERRED_PROVIDER_virtual/nothing: BitBake's fatal error, the word stays.
    assert!(deps.text.contains("virtual/nothing"), "{deps:?}");
    assert!(
        ds.replay()
            .references
            .iter()
            .any(|r| r.text == "PREFERRED_PROVIDER_virtual/nothing"
                && matches!(r.problem, RefProblem::Fatal(_))),
        "{:?}",
        ds.replay().references
    );
}

#[test]
fn virtual_providers_are_not_rewritten_before_walnascar() {
    let fx = modified_release("scarthgap", virtual_providers);
    let ds = eval(&fx, &target(Some("app")));
    let deps = ds.value("DEPENDS").unwrap();
    assert!(
        deps.text.contains("virtual/cross-cc") && !deps.text.contains("gcc-cross"),
        "{deps:?}"
    );
    assert!(!ds.replay().var_notes.contains_key("DEPENDS"));
}

// ---------------------------------------------------------------------------------------
// Bug 2: files matched by one BBFILES glob are sorted as strings
// ---------------------------------------------------------------------------------------

fn sibling_appends(root: &Path) {
    write(
        root,
        "meta-mine/recipes-x/a/a_1.0.bb",
        "LICENSE = \"CLOSED\"\nX = \"recipe\"\n",
    );
    write(
        root,
        "meta-mine/recipes-x/foo/a_%.bbappend",
        "X = \"from foo\"\nORDER .= \" foo\"\n",
    );
    write(
        root,
        "meta-mine/recipes-x/foo-bar/a_%.bbappend",
        "X = \"from foo-bar\"\nORDER .= \" foo-bar\"\n",
    );
}

#[test]
fn appends_from_one_glob_apply_in_python_string_order() {
    let fx = modified(sibling_appends);
    // BitBake: `for g in sorted(globbed)` (cooker.py collect_bbfiles), a string sort in
    // which `-` (0x2D) comes before `/` (0x2F): foo-bar/ first, foo/ last.
    let recipe = query::find_recipes(&fx.idx, "a").unwrap();
    assert_eq!(recipe.len(), 1);
    let appends: Vec<String> = query::appends_for(&fx.idx, recipe[0].file.id)
        .unwrap()
        .iter()
        .map(|f| f.path.to_string_lossy().into_owned())
        .collect();
    assert_eq!(
        appends,
        [
            "recipes-x/foo-bar/a_%.bbappend",
            "recipes-x/foo/a_%.bbappend"
        ]
    );
    let ds = eval(&fx, &target(Some("a")));
    assert_eq!(text(&ds, "X").as_deref(), Some("from foo"));
    assert_eq!(text(&ds, "ORDER").as_deref(), Some(" foo-bar foo"));
    let collected = resolve::recipe_files(&fx.ws);
    let pos = |s: &str| collected.iter().position(|p| p.ends_with(s)).unwrap();
    assert!(pos("recipes-x/foo-bar/a_%.bbappend") < pos("recipes-x/foo/a_%.bbappend"));
    assert!(fx.root.is_dir());
}

#[test]
fn python_string_order_of_paths() {
    use std::cmp::Ordering::*;
    let c = |a: &str, b: &str| resolve::py_str_cmp(Path::new(a), Path::new(b));
    assert_eq!(c("/r/foo-bar/a", "/r/foo/a"), Less);
    assert_eq!(c("/r/foo/a", "/r/foo.d/a"), Greater);
    assert_eq!(c("/r/a", "/r/a"), Equal);
    // Code point order for non-ASCII text (UTF-8 byte order agrees with it).
    assert_eq!(c("/r/\u{e9}", "/r/z"), Greater);
    assert_eq!(c("/r/\u{10000}", "/r/\u{ffff}"), Greater);
    #[cfg(unix)]
    {
        use std::ffi::OsStr;
        use std::os::unix::ffi::OsStrExt;
        // An undecodable byte is U+DC00 + byte (surrogateescape): above U+D7FF, below
        // U+E000.
        let bad = Path::new(OsStr::from_bytes(b"/r/\xff"));
        assert_eq!(resolve::py_str_cmp(bad, Path::new("/r/\u{d7ff}")), Greater);
        assert_eq!(resolve::py_str_cmp(bad, Path::new("/r/\u{e000}")), Less);
    }
}

// ---------------------------------------------------------------------------------------
// Bug 6: is_function for override-qualified function names
// ---------------------------------------------------------------------------------------

#[test]
fn package_functions_with_expanded_names_are_functions() {
    let fx = modified(|root| {
        append_to(
            root,
            "meta-mine/recipes-app/app/app_0.1.bb",
            "\npkg_postinst:${PN}:prepend () {\n\techo pre\n}\n\
             pkg_postinst:${PN}:append () {\n\techo post\n}\n\
             pkg_postrm:${PN} () {\n\techo rm\n}\n",
        );
    });
    let ds = eval(&fx, &target(Some("app")));
    for name in ["pkg_postinst:app", "pkg_postrm:app"] {
        assert!(ds.is_function(name), "{name}");
        // BitBake's own `func` flag is gone (renameVar copies only the keyword lists).
        assert_eq!(ds.raw_flag(name, "func"), None, "{name}");
    }
    let pieces = ds.function("pkg_postinst:app").unwrap();
    assert_eq!(pieces.len(), 2, "{pieces:?}");
    assert_eq!(ds.function("pkg_postrm:app").unwrap().len(), 1);
    assert!(ds.is_function("do_compile"));
    assert!(!ds.is_function("EXTRA_OEMAKE"));
    assert!(!ds.is_function("pkg_preinst:app"));
}

// ---------------------------------------------------------------------------------------
// Bug 4: a `.bb` no BBFILES glob matches is not a recipe
// ---------------------------------------------------------------------------------------

fn template_recipe(root: &Path) {
    // meta-mine's BBFILES is `${LAYERDIR}/recipes-*/*/*.bb`: `templates/` is not matched.
    write(
        root,
        "meta-mine/templates/example/example_1.0.bb",
        "LICENSE = \"CLOSED\"\nPROVIDES = \"virtual/foo\"\nX = \"template\"\n",
    );
    write(
        root,
        "meta-mine/templates/example/hello_%.bbappend",
        "X = \"uncollected append\"\n",
    );
}

#[test]
fn a_recipe_outside_bbfiles_is_indexed_but_not_a_recipe() {
    let fx = modified(template_recipe);
    let file = query::file_by_path(
        &fx.idx,
        "meta-mine",
        Path::new("templates/example/example_1.0.bb"),
    )
    .unwrap()
    .expect("still indexed");
    assert_eq!(
        query::not_collected(&fx.idx, file.id).unwrap().as_deref(),
        Some("not matched by any layer's BBFILES")
    );
    let collected = query::file_by_path(
        &fx.idx,
        "meta-mine",
        Path::new("recipes-app/app/app_0.1.bb"),
    )
    .unwrap()
    .unwrap();
    assert_eq!(query::not_collected(&fx.idx, collected.id).unwrap(), None);

    // Not found by name.
    assert!(query::find_recipes(&fx.idx, "example").unwrap().is_empty());
    assert!(
        !query::recipe_names_like(&fx.idx, "exam", 10)
            .unwrap()
            .contains(&"example".to_string())
    );
    let e = eval::evaluate_with(
        &ConfigCache::new(),
        &fx.idx,
        &fx.ws,
        &target(Some("example")),
    );
    assert!(
        matches!(e, Err(sous_chef::error::Error::NotFound { .. })),
        "{:?}",
        e.map(|_| ())
    );

    // Found by path, with the note.
    let ds = eval(
        &fx,
        &target(Some("meta-mine:templates/example/example_1.0.bb")),
    );
    assert_eq!(text(&ds, "X").as_deref(), Some("template"));
    let notes = &ds.replay().target_notes;
    assert!(
        notes
            .iter()
            .any(|n| n.contains("not matched by any layer's BBFILES")),
        "{notes:?}"
    );
    assert!(ds.replay().appends.is_empty());

    // Not a provider.
    let p = eval::providers::Providers::new(&fx.idx, &fx.ws, None, None)
        .unwrap()
        .providers("virtual/foo")
        .unwrap();
    let pns: Vec<&str> = p.providers.iter().map(|p| p.pn.as_str()).collect();
    assert!(!pns.contains(&"example"), "{pns:?}");
    assert!(pns.contains(&"libfoo"), "{pns:?}");

    // An append outside BBFILES applies to nothing and is not an orphan either (the
    // org/layer-conf lint rule reports it).
    let hello = eval(
        &fx,
        &target(Some("meta-core:recipes-core/hello/hello_1.1.bb")),
    );
    assert_ne!(text(&hello, "X").as_deref(), Some("uncollected append"));
    assert!(
        !query::orphan_appends(&fx.idx)
            .unwrap()
            .iter()
            .any(|f| f.path.starts_with("templates"))
    );
    // A collected recipe evaluates without the note.
    assert!(hello.replay().target_notes.is_empty());
}
