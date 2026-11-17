//! Fixes for the bugs the live verification found in the evaluator and its tools
//! (docs/live-verification.md, B4, B5, B6, B11, B15, B18). Each test runs on a temporary
//! copy of the fixture workspace; the fixture itself is not changed.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::fs;
use std::path::{Path, PathBuf};

use common::*;
use sous_chef::eval::{self, ConfigCache, DataStore, RefProblem, Target};
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

/// A copy of the fixture workspace on `release` after `edit(ws root)`, indexed.
fn modified(release: &str, edit: impl FnOnce(&Path)) -> Fx {
    let (tmp, root, _cfg, ws) = temp_ws_release(release, edit);
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

fn replace_in(root: &Path, rel: &str, from: &str, to: &str) {
    let p = root.join(rel);
    let s = fs::read_to_string(&p).unwrap();
    assert!(s.contains(from), "{rel} has no {from:?}");
    fs::write(p, s.replace(from, to)).unwrap();
}

fn eval(fx: &Fx, recipe: Option<&str>) -> DataStore {
    let t = Target {
        recipe: recipe.map(str::to_string),
        ..Target::default()
    };
    eval::evaluate_with(&ConfigCache::new(), &fx.idx, &fx.ws, &t).unwrap()
}

// ---------------------------------------------------------------------------------------
// B5: `inherit_defer toolchain/${TOOLCHAIN}` with python in OVERRIDES
// ---------------------------------------------------------------------------------------

/// OE-core Wrynose's shape: `OVERRIDES` holds python (the tune's `MACHINEOVERRIDES`) and
/// `${TCOVERRIDE}`, which only `toolchain/<toolchain>.bbclass` sets; `base.bbclass` picks
/// the toolchain with `TOOLCHAIN ??= "${PREFERRED_TOOLCHAIN}"` (which has `class-*`
/// variants) and inherits it deferred.
fn toolchain_ws(root: &Path) {
    replace_in(
        root,
        "meta-core/conf/bitbake.conf",
        "MACHINEOVERRIDES ?= \"${MACHINE}\"",
        "MACHINEOVERRIDES ?= \"${@bb.utils.contains('TUNE_FEATURES', 'armv7a', 'armv7a:', '', d)}${MACHINE}\"",
    );
    replace_in(
        root,
        "meta-core/conf/bitbake.conf",
        "${CLASSOVERRIDE}${LIBCOVERRIDE}",
        "${CLASSOVERRIDE}:${TCOVERRIDE}${LIBCOVERRIDE}",
    );
    append_to(
        root,
        "meta-core/classes/base.bbclass",
        "\nPREFERRED_TOOLCHAIN_TARGET ??= \"gcc\"\n\
         PREFERRED_TOOLCHAIN_NATIVE ??= \"gcc\"\n\
         PREFERRED_TOOLCHAIN = \"${PREFERRED_TOOLCHAIN_TARGET}\"\n\
         PREFERRED_TOOLCHAIN:class-native = \"${PREFERRED_TOOLCHAIN_NATIVE}\"\n\
         TOOLCHAIN ??= \"${PREFERRED_TOOLCHAIN}\"\n\
         inherit_defer toolchain/${TOOLCHAIN}\n",
    );
    write(
        root,
        "meta-core/classes/toolchain/gcc.bbclass",
        "TCOVERRIDE = \"toolchain-gcc\"\nCC = \"gcc\"\n",
    );
    write(
        root,
        "meta-core/classes/toolchain/clang.bbclass",
        "TCOVERRIDE = \"toolchain-clang\"\nCC = \"clang\"\n",
    );
}

#[test]
fn deferred_toolchain_inherit_is_followed_when_overrides_hold_python() {
    let fx = modified("wrynose", toolchain_ws);
    let ds = eval(&fx, Some("hello"));
    let notes = ds.replay();
    // Not refused as python: the argument expands to complete text (`toolchain/gcc`).
    assert!(
        !notes
            .references
            .iter()
            .any(|r| r.kind == "inherit_defer" && r.problem == RefProblem::Python),
        "{:?}",
        notes.references
    );
    // The subdirectory class resolves along the class search path.
    assert!(
        ds.files()
            .iter()
            .any(|f| f.loc.path.ends_with("classes/toolchain/gcc.bbclass")),
        "toolchain/gcc.bbclass not read"
    );
    let tc = ds.value("TCOVERRIDE").unwrap();
    assert_eq!(tc.text, "toolchain-gcc");
    // ... but which class was chosen depended on unknown override entries: flagged.
    assert!(tc.unevaluated_python);
    assert!(ds.value("CC").unwrap().unevaluated_python);
    assert_eq!(notes.uncertain_references.len(), 1, "{notes:?}");
    let u = &notes.uncertain_references[0];
    assert_eq!(u.kind, "inherit_defer");
    assert_eq!(u.text, "toolchain/${TOOLCHAIN}");
    assert_eq!(u.expanded, "toolchain/gcc");
    assert!(u.python);
    // TCOVERRIDE is no longer an unresolved name of OVERRIDES, and its entry is there.
    let ov = ds.overrides_value().unwrap();
    assert!(!ov.unresolved.contains(&"TCOVERRIDE".to_string()), "{ov:?}");
    assert!(ds.overrides().contains(&"toolchain-gcc".to_string()));
    // A value that consults no override is not touched.
    let s = ds.value("TMPDIR").unwrap();
    assert!(!s.unevaluated_python && s.unresolved.is_empty(), "{s:?}");
}

#[test]
fn deferred_inherit_with_python_text_is_still_refused() {
    let fx = modified("wrynose", |root| {
        toolchain_ws(root);
        append_to(
            root,
            "meta-core/classes/base.bbclass",
            "OTHER_CLASSES = \"${@'toolchain/clang'}\"\ninherit_defer ${OTHER_CLASSES}\n",
        );
    });
    let ds = eval(&fx, Some("hello"));
    assert!(
        ds.replay()
            .references
            .iter()
            .any(|r| r.kind == "inherit_defer"
                && r.text == "${OTHER_CLASSES}"
                && r.problem == RefProblem::Python),
        "{:?}",
        ds.replay().references
    );
    assert_eq!(ds.value("CC").unwrap().text, "gcc");
}

#[test]
fn deferred_inherit_without_uncertainty_is_exact() {
    // No python in OVERRIDES: the class is followed and nothing is flagged.
    let fx = modified("wrynose", |root| {
        toolchain_ws(root);
        replace_in(
            root,
            "meta-core/conf/bitbake.conf",
            "${@bb.utils.contains('TUNE_FEATURES', 'armv7a', 'armv7a:', '', d)}",
            "",
        );
    });
    let ds = eval(&fx, Some("hello"));
    let tc = ds.value("TCOVERRIDE").unwrap();
    assert_eq!(tc.text, "toolchain-gcc");
    assert!(!tc.unevaluated_python, "{tc:?}");
    assert!(ds.replay().uncertain_references.is_empty());
}
