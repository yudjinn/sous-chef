//! Class functions evaluated statically (acc-helpers, `src/eval/classfn.rs`): a function a
//! class defines with `def` is run only when the last definition of its name the parse
//! executed is the verified text (`tests/fixtures/classfn/realdefs.bbclass`, copied from
//! OE-core); a different definition (the fixture workspace's own stand-in
//! `base_set_filespath`, a recipe's `gnome_verdir`) or none leaves it unevaluated, and
//! `bb.data.inherits_class` is answered only when the inherits are certain.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::fs;
use std::path::{Path, PathBuf};

use common::*;
use sous_chef::eval::{self, ConfigCache, DataStore, Target};
use sous_chef::index::Index;
use sous_chef::workspace::Workspace;

struct Fx {
    _cache: tempfile::TempDir,
    _tmp: tempfile::TempDir,
    root: PathBuf,
    ws: Workspace,
    idx: Index,
}

fn modified(edit: impl FnOnce(&Path)) -> Fx {
    let (tmp, root) = temp_ws();
    let root = fs::canonicalize(root).unwrap();
    let real = fs::read_to_string(fixture_path("classfn/realdefs.bbclass")).unwrap();
    write(&root, "meta-mine/classes/realdefs.bbclass", &real);
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

fn recipe(root: &Path, pn: &str, body: &str) {
    write(
        root,
        &format!("meta-mine/recipes-app/{pn}/{pn}_1.0.bb"),
        &format!("LICENSE = \"CLOSED\"\n{body}"),
    );
}

fn eval(fx: &Fx, recipe: &str) -> DataStore {
    let t = Target {
        recipe: Some(recipe.to_string()),
        ..Target::default()
    };
    eval::evaluate_with(&ConfigCache::new(), &fx.idx, &fx.ws, &t).unwrap()
}

fn certain(ds: &DataStore, name: &str) -> String {
    let v = ds.value(name).unwrap_or_else(|| panic!("{name} unset"));
    assert!(
        !v.unevaluated_python && v.unresolved.is_empty(),
        "{name} not certain: {v:?}"
    );
    v.text
}

fn flagged(ds: &DataStore, name: &str) -> String {
    let v = ds.value(name).unwrap_or_else(|| panic!("{name} unset"));
    assert!(v.unevaluated_python, "{name} not flagged: {v:?}");
    v.text
}

const USES: &str = "BASE_DEFAULT_DEPS = \"virtual/libc\"\n\
    AUTO = \"${@get_autotools_dep(d)}\"\n\
    BASEDEP = \"${@get_base_dep(d)}\"\n\
    GD = \"${@gettext_dependencies(d)}\"\n\
    GO = \"${@gettext_oeconf(d)}\"\n\
    GV = \"${@gnome_verdir(\"${PV}.5\")}\"\n\
    FILESOVERRIDES = \"arm:kiss\"\n\
    FILESEXTRAPATHS:prepend = \"/extra:\"\n\
    FP = \"${@base_set_filespath([\"/r/${BPN}\", \"/r/files\"], d)}\"\n\
    valid_archs = \"x86 arm\"\n\
    KARCH = \"${@map_kernel_arch(d.getVar('TARGET_ARCH'), d)}\"\n\
    UARCH = \"${@map_uboot_arch(d.getVar('KARCH'), d)}\"\n";

#[test]
fn verified_definitions_are_evaluated() {
    let fx = modified(|r| recipe(r, "cf", &format!("inherit realdefs\n{USES}")));
    let ds = eval(&fx, "cf");
    assert_eq!(
        certain(&ds, "AUTO"),
        "autoconf-native automake-native libtool-native libtool-cross "
    );
    // The returned text is expanded again, as BitBake's expansion loop does.
    assert_eq!(certain(&ds, "BASEDEP"), "virtual/libc");
    assert_eq!(certain(&ds, "GD"), "gettext-native");
    assert_eq!(certain(&ds, "GO"), "--enable-nls");
    assert_eq!(certain(&ds, "GV"), "1.0");
    // The fixture's TARGET_ARCH is i686.
    assert_eq!(certain(&ds, "KARCH"), "x86");
    assert_eq!(certain(&ds, "UARCH"), "x86");
    // FILESEXTRAPATHS is `__default:` in the fixture's bitbake.conf, or unset.
    let fp = certain(&ds, "FP");
    assert!(
        fp.starts_with("/extra/kiss:/r/cf/kiss:/r/files/kiss:/extra/arm:")
            && fp.ends_with(":/r/cf/:/r/files/"),
        "{fp}"
    );
    let log = ds.python_log("AUTO");
    assert!(
        log.own
            .iter()
            .any(|e| e.evaluated && e.code == "get_autotools_dep(d)"),
        "{log:?}"
    );
}

#[test]
fn other_definitions_are_not_evaluated() {
    let fx = modified(|r| {
        // A recipe's own gnome_verdir (OE-core's gcr does this) after the class's.
        recipe(
            r,
            "redef",
            &format!(
                "inherit realdefs\n{USES}def gnome_verdir(v):\n    return \".\".join(v.split(\".\")[:2]) or v\n"
            ),
        );
        // No realdefs: base_set_filespath is the fixture's stand-in (meta-core
        // base.bbclass), the others are not defined at all.
        recipe(r, "nodefs", USES);
    });
    let ds = eval(&fx, "redef");
    assert!(flagged(&ds, "GV").contains("${@gnome_verdir("));
    assert_eq!(certain(&ds, "BASEDEP"), "virtual/libc");
    let log = ds.python_log("GV");
    assert!(
        log.own.iter().any(|e| !e.evaluated
            && e.why
                .as_deref()
                .is_some_and(|w| w.contains("other than the verified one"))),
        "{log:?}"
    );
    let ds = eval(&fx, "nodefs");
    for v in ["AUTO", "BASEDEP", "GD", "GO", "GV", "FP"] {
        flagged(&ds, v);
    }
    let log = ds.python_log("FP");
    assert!(
        log.own.iter().any(|e| e
            .why
            .as_deref()
            .is_some_and(|w| w.contains("base_set_filespath is defined at"))),
        "{log:?}"
    );
}

#[test]
fn inherits_class_is_read_from_the_inherit_cache() {
    let fx = modified(|r| {
        write(r, "meta-mine/classes/native.bbclass", "# a stand-in\n");
        recipe(r, "nat", &format!("inherit realdefs native\n{USES}"));
        // An inherit whose argument names an undefined variable is not followed: whether
        // the recipe inherits native is then not known.
        recipe(
            r,
            "unsure",
            &format!("inherit realdefs\ninherit ${{MAYBE_NATIVE}}\n{USES}"),
        );
        // INHIBIT_AUTOTOOLS_DEPS decides first: no inherits_class call.
        recipe(
            r,
            "inhibit",
            &format!(
                "inherit realdefs\ninherit ${{MAYBE_NATIVE}}\nINHIBIT_AUTOTOOLS_DEPS = \"1\"\n{USES}"
            ),
        );
    });
    let ds = eval(&fx, "nat");
    assert_eq!(
        certain(&ds, "AUTO"),
        "autoconf-native automake-native libtool-native "
    );
    let ds = eval(&fx, "unsure");
    flagged(&ds, "AUTO");
    // gettext_dependencies asks only when INHIBIT_DEFAULT_DEPS is set.
    assert_eq!(certain(&ds, "GD"), "gettext-native");
    let ds = eval(&fx, "inhibit");
    assert_eq!(certain(&ds, "AUTO"), "");
    let _ = &fx.root;
}
