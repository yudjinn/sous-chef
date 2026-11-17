//! Evaluator gaps closed in round w8: virtual providers in task `depends` flags and
//! `DEPENDS` normalisation, kas defaults at `local.conf`'s place, honest skip notes
//! (`COMPATIBLE_MACHINE`, `COMPATIBLE_HOST`, `INCOMPATIBLE_LICENSE`), `BBMASK` per machine,
//! and the class context of an `inherit` in an `.inc` included from a `.conf`. Each test
//! runs on a temporary copy of the fixture workspace; the fixture itself is not changed.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;
mod kas_support;

use std::fs;
use std::path::{Path, PathBuf};

use common::*;
use sous_chef::eval::{self, ConfigCache, DataStore, OpKind, RefProblem, Target};
use sous_chef::index::Index;
use sous_chef::workspace::Workspace;

struct Fx {
    _cache: tempfile::TempDir,
    _tmp: tempfile::TempDir,
    #[allow(dead_code)]
    root: PathBuf,
    ws: Workspace,
    idx: Index,
}

/// A copy of the fixture workspace after `edit(ws root)`, discovered and indexed.
fn modified(edit: impl FnOnce(&Path)) -> Fx {
    let (tmp, root) = temp_ws();
    let root = fs::canonicalize(root).unwrap();
    edit(&root);
    let ws = Workspace::discover(&ws_config(&root)).unwrap();
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

fn target(recipe: &str) -> Target {
    Target {
        recipe: Some(recipe.to_string()),
        ..Target::default()
    }
}

fn eval(fx: &Fx, t: &Target) -> DataStore {
    eval::evaluate_with(&ConfigCache::new(), &fx.idx, &fx.ws, t).unwrap()
}

// ---------------------------------------------------------------------------------------
// Item 2: handleVirtRecipeProviders on task `depends` flags; DEPENDS normalised / ""
// ---------------------------------------------------------------------------------------

fn virtual_providers(root: &Path) {
    append_to(
        root,
        "meta-core/conf/distro/testdistro.conf",
        "\nBB_RECIPE_VIRTUAL_PROVIDERS = \"virtual/cross-cc virtual/cross-binutils\"\n\
         PREFERRED_PROVIDER_virtual/cross-cc = \"gcc-cross-${TARGET_ARCH}\"\n",
    );
}

#[test]
fn task_depends_flags_are_rewritten_for_tasks_in_bbtasks() {
    let fx = modified(|r| {
        virtual_providers(r);
        append_to(
            r,
            "meta-mine/recipes-app/app/app_0.1.bb",
            "\ndo_report[depends] = \"virtual/cross-cc:do_populate_sysroot  libfoo:do_build\"\n\
             do_notask[depends] = \"virtual/cross-cc:do_populate_sysroot\"\n\
             addtask gone\n\
             do_gone[depends] = \"virtual/cross-cc:do_populate_sysroot\"\n\
             deltask gone\n\
             addtask nobin\n\
             do_nobin[depends] = \"virtual/cross-binutils:do_populate_sysroot\"\n",
        );
    });
    let ds = eval(&fx, &target("app"));
    assert!(
        ds.tasks().iter().any(|t| t == "do_report"),
        "{:?}",
        ds.tasks()
    );
    assert!(
        !ds.tasks().iter().any(|t| t == "do_gone"),
        "{:?}",
        ds.tasks()
    );
    // In __BBTASKS: rewritten, entries joined with single spaces.
    assert_eq!(
        ds.flag("do_report", "depends").unwrap().text,
        "gcc-cross-i686:do_populate_sysroot libfoo:do_build"
    );
    let (op, _) = ds.flag_history("do_report", "depends").pop().unwrap();
    assert_eq!(op.kind, OpKind::VirtualProviderRewrite);
    assert_eq!(op.loc.path, Path::new("conf/distro/testdistro.conf"));
    let notes = &ds.replay().var_notes["do_report"];
    assert!(
        notes[0].contains("do_report[depends] rewritten")
            && notes[0].contains("virtual/cross-cc -> gcc-cross-i686"),
        "{notes:?}"
    );
    // Not a task (no addtask, or removed by deltask): BitBake does not look at it.
    assert_eq!(
        ds.flag("do_notask", "depends").unwrap().text,
        "virtual/cross-cc:do_populate_sysroot"
    );
    assert_eq!(
        ds.flag("do_gone", "depends").unwrap().text,
        "virtual/cross-cc:do_populate_sysroot"
    );
    // No PREFERRED_PROVIDER: BitBake writes the text None (no error for task flags).
    assert_eq!(
        ds.flag("do_nobin", "depends").unwrap().text,
        "None:do_populate_sysroot"
    );
}

#[test]
fn a_task_depends_entry_without_one_colon_is_a_parse_error() {
    let fx = modified(|r| {
        virtual_providers(r);
        append_to(
            r,
            "meta-mine/recipes-app/app/app_0.1.bb",
            "\ndo_report[depends] = \"virtual/cross-cc:do_populate_sysroot bad\"\n",
        );
    });
    let ds = eval(&fx, &target("app"));
    assert!(
        ds.replay().references.iter().any(|r| matches!(
            &r.problem,
            RefProblem::Fatal(m) if m.contains("ValueError") && m.contains("\"bad\"")
        )),
        "{:?}",
        ds.replay().references
    );
    assert_eq!(
        ds.flag("do_report", "depends").unwrap().text,
        "virtual/cross-cc:do_populate_sysroot bad"
    );
}

#[test]
fn depends_is_normalised_and_unset_depends_becomes_empty() {
    let fx = modified(|r| {
        virtual_providers(r);
        append_to(
            r,
            "meta-mine/recipes-app/app/app_0.1.bb",
            "\nDEPENDS += \"  zlib   \"\n",
        );
        write(
            r,
            "meta-mine/recipes-app/nodeps/nodeps_1.0.bb",
            "LICENSE = \"CLOSED\"\n",
        );
    });
    let ds = eval(&fx, &target("app"));
    assert_eq!(ds.value("DEPENDS").unwrap().text, "libfoo zlib");
    let (op, _) = ds.history("DEPENDS").pop().unwrap();
    assert_eq!(op.kind, OpKind::VirtualProviderRewrite);
    assert!(
        ds.replay().var_notes["DEPENDS"][0].contains("joined by single spaces"),
        "{:?}",
        ds.replay().var_notes
    );

    let ds = eval(&fx, &target("nodeps"));
    assert_eq!(ds.value("DEPENDS").unwrap().text, "");
    assert!(
        ds.replay().var_notes["DEPENDS"][0].contains("was unset"),
        "{:?}",
        ds.replay().var_notes
    );

    // Already normal: nothing recorded.
    let fx = modified(virtual_providers);
    let ds = eval(&fx, &target("app"));
    assert_eq!(ds.value("DEPENDS").unwrap().text, "libfoo");
    assert!(!ds.replay().var_notes.contains_key("DEPENDS"));
}

// ---------------------------------------------------------------------------------------
// Item 3: kas defaults where bitbake.conf includes conf/local.conf
// ---------------------------------------------------------------------------------------

/// The kas project before `kas checkout` (no build directory), with a `bitbake.conf` in
/// its own layer holding `text`.
fn kas_eval(bitbake_conf: &str) -> DataStore {
    let p = kas_support::Project::before();
    write(&p.root, "meta-proj/conf/bitbake.conf", bitbake_conf);
    let ws = p.ws();
    assert_eq!(ws.build_dir, None);
    let cache = tempfile::tempdir().unwrap();
    let mut idx = Index::open_in(cache.path(), &ws).unwrap();
    idx.refresh(&ws).unwrap();
    eval::evaluate_with(&ConfigCache::new(), &idx, &ws, &Target::default()).unwrap()
}

#[test]
fn kas_defaults_apply_where_bitbake_conf_includes_local_conf() {
    // kas writes `MACHINE ??= "board1"` into local.conf: a `??=` before the include loses
    // to it, a `??=` after it wins, and nothing before the include sees the kas value.
    let ds = kas_eval(
        "MACHINE ??= \"fromconf\"\n\
         EARLY := \"m-${MACHINE}\"\n\
         DISTRO_EARLY := \"d-${DISTRO}\"\n\
         include conf/local.conf\n\
         LATE := \"m-${MACHINE}\"\n\
         DISTRO ??= \"laterdistro\"\n",
    );
    let text = |n: &str| ds.value(n).map(|v| v.text);
    assert_eq!(text("MACHINE").as_deref(), Some("board1"));
    assert_eq!(text("EARLY").as_deref(), Some("m-fromconf"));
    // `:=` before local.conf: DISTRO was unset there, the reference stays as written.
    assert_eq!(ds.raw("DISTRO_EARLY").as_deref(), Some("d-${DISTRO}"));
    assert_eq!(text("LATE").as_deref(), Some("m-board1"));
    assert_eq!(text("DISTRO").as_deref(), Some("laterdistro"));
    let notes = &ds.replay().target_notes;
    assert!(
        notes
            .iter()
            .any(|n| n.contains("MACHINE = \"board1\" from the kas file kas/board.yaml:5")),
        "{notes:?}"
    );
    // DISTRO's final value is not kas's: no note claims it is.
    assert!(
        !notes.iter().any(|n| n.starts_with("note: DISTRO =")),
        "{notes:?}"
    );

    // A `?=` in the configuration before local.conf beats kas's `??=`.
    let ds = kas_eval("MACHINE ?= \"strong\"\ninclude conf/local.conf\n");
    assert_eq!(ds.value("MACHINE").unwrap().text, "strong");

    // A bitbake.conf that never includes local.conf: kas's local.conf is not read.
    let ds = kas_eval("X = \"1\"\n");
    assert_eq!(ds.value("MACHINE"), None);
    assert!(
        ds.replay()
            .target_notes
            .iter()
            .any(|n| n.contains("no MACHINE is set")),
        "{:?}",
        ds.replay().target_notes
    );
}
