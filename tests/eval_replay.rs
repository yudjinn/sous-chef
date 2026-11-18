//! T3.4: replay (`eval::evaluate`) over `tests/fixtures/ws`. Expected answers are in
//! `tests/fixtures/README.md`.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::fs;
use std::path::Path;

use common::*;
use sous_chef::error::Error;
use sous_chef::eval::{self, ConfigCache, DataStore, OpStatus, RefProblem, Target};
use sous_chef::index::Index;
use sous_chef::release::Feature;
use sous_chef::types::{AssignOp, FileKind};
use sous_chef::workspace::Workspace;

struct Fx {
    _cache: tempfile::TempDir,
    _tmp: Option<tempfile::TempDir>,
    ws: Workspace,
    idx: Index,
}

fn indexed(ws: Workspace, tmp: Option<tempfile::TempDir>) -> Fx {
    let cache = tempfile::tempdir().unwrap();
    let mut idx = Index::open_in(cache.path(), &ws).unwrap();
    idx.refresh(&ws).unwrap();
    Fx {
        _cache: cache,
        _tmp: tmp,
        ws,
        idx,
    }
}

fn fixture() -> Fx {
    indexed(fixture_ws(), None)
}

/// A copy of the fixture workspace after `edit(ws root)`, indexed.
fn modified(edit: impl FnOnce(&Path)) -> (Fx, std::path::PathBuf) {
    let (tmp, root) = temp_ws();
    edit(&root);
    let ws = Workspace::discover(&ws_config(&root)).unwrap();
    (indexed(ws, Some(tmp)), root)
}

fn target(recipe: Option<&str>, machine: Option<&str>) -> Target {
    Target {
        mc: None,
        recipe: recipe.map(str::to_string),
        version: None,
        machine: machine.map(str::to_string),
        distro: None,
    }
}

fn eval_with(fx: &Fx, cache: &ConfigCache, t: &Target) -> DataStore {
    eval::evaluate_with(cache, &fx.idx, &fx.ws, t).unwrap()
}

fn eval(fx: &Fx, t: &Target) -> DataStore {
    eval_with(fx, &ConfigCache::new(), t)
}

fn strs<T: ToString>(v: &[T]) -> Vec<String> {
    v.iter().map(|x| x.to_string()).collect()
}

const CONFIG_VISITS: &[(&str, FileKind, &[&str])] = &[
    ("build:conf/bblayers.conf:1-14", FileKind::Conf, &[]),
    (
        "meta-core:conf/layer.conf:1-13",
        FileKind::Conf,
        &["build:conf/bblayers.conf:10-14"],
    ),
    (
        "meta-vendor:conf/layer.conf:1-11",
        FileKind::Conf,
        &["build:conf/bblayers.conf:10-14"],
    ),
    (
        "meta-mine:conf/layer.conf:1-11",
        FileKind::Conf,
        &["build:conf/bblayers.conf:10-14"],
    ),
    ("meta-core:conf/bitbake.conf:1-52", FileKind::Conf, &[]),
    (
        "build:conf/local.conf:1-15",
        FileKind::Conf,
        &["meta-core:conf/bitbake.conf:50"],
    ),
    (
        "meta-core:conf/machine/qemux86.conf:1-8",
        FileKind::Conf,
        &["meta-core:conf/bitbake.conf:51"],
    ),
    (
        "meta-core:conf/distro/testdistro.conf:1-13",
        FileKind::Conf,
        &["meta-core:conf/bitbake.conf:52"],
    ),
    ("meta-core:classes/base.bbclass:1-60", FileKind::Class, &[]),
    (
        "meta-core:classes-global/sanityish.bbclass:1-10",
        FileKind::Class,
        &[
            "meta-core:conf/bitbake.conf:52",
            "meta-core:conf/distro/testdistro.conf:13",
        ],
    ),
];

fn visits(ds: &DataStore) -> Vec<(String, FileKind, Vec<String>)> {
    ds.files()
        .iter()
        .map(|v| (v.loc.to_string(), v.kind, strs(&v.via)))
        .collect()
}

fn expected(rows: &[(&str, FileKind, &[&str])]) -> Vec<(String, FileKind, Vec<String>)> {
    rows.iter()
        .map(|(l, k, v)| (l.to_string(), *k, strs(v)))
        .collect()
}

#[test]
fn visit_order_for_hello_1_1() {
    let fx = fixture();
    let ds = eval(&fx, &target(Some("hello"), None));
    let recipe_via: &[&str] = &["meta-core:recipes-core/hello/hello_1.1.bb:1"];
    let mut want: Vec<(&str, FileKind, &[&str])> = CONFIG_VISITS.to_vec();
    want.extend_from_slice(&[
        (
            "meta-core:recipes-core/hello/hello_1.1.bb:1-9",
            FileKind::Recipe,
            &[],
        ),
        (
            "meta-core:recipes-core/hello/hello.inc:1-20",
            FileKind::Include,
            recipe_via,
        ),
        (
            "meta-core:classes-recipe/autotoolsish.bbclass:1-30",
            FileKind::Class,
            &[
                "meta-core:recipes-core/hello/hello_1.1.bb:1",
                "meta-core:recipes-core/hello/hello.inc:11",
            ],
        ),
        // Lower layer priority first: vendor (6), then mine (10).
        (
            "meta-vendor:recipes-core/hello/hello_%.bbappend:1-9",
            FileKind::Append,
            recipe_via,
        ),
        (
            "meta-mine:recipes-core/hello/hello_1.1.bbappend:1-10",
            FileKind::Append,
            recipe_via,
        ),
    ]);
    assert_eq!(visits(&ds), expected(&want));
    let notes = ds.replay();
    assert_eq!(
        notes.recipe.as_ref().unwrap().to_string(),
        "meta-core:recipes-core/hello/hello_1.1.bb:1"
    );
    assert_eq!(
        strs(&notes.appends),
        [
            "meta-vendor:recipes-core/hello/hello_%.bbappend:1",
            "meta-mine:recipes-core/hello/hello_1.1.bbappend:1"
        ]
    );
    assert!(notes.references.is_empty(), "{:?}", notes.references);
    assert!(notes.anonymous.is_empty());
}

#[test]
fn hello_1_0_gets_only_the_wildcard_append() {
    let fx = fixture();
    let mut t = target(Some("hello"), None);
    t.version = Some("1.0".into());
    let ds = eval(&fx, &t);
    let appends: Vec<String> = ds
        .files()
        .iter()
        .filter(|v| v.kind == FileKind::Append)
        .map(|v| v.loc.to_string())
        .collect();
    assert_eq!(
        appends,
        ["meta-vendor:recipes-core/hello/hello_%.bbappend:1-9"]
    );
    assert_eq!(
        ds.value("SRC_URI").unwrap().text,
        "https://example.org/releases/hello-1.0.tar.gz file://vendor.patch"
    );
}

/// `(loc, op, value, status, via)` of every `SRC_URI` operation.
fn src_uri_history(ds: &DataStore) -> Vec<(String, AssignOp, String, OpStatus, Vec<String>)> {
    ds.history("SRC_URI")
        .into_iter()
        .map(|(op, st)| (op.loc.to_string(), op.op, op.value, st, strs(&op.via)))
        .collect()
}

#[test]
fn src_uri_history_for_two_machines() {
    let fx = fixture();
    let recipe = "meta-core:recipes-core/hello/hello_1.1.bb:1";
    let row = |loc: &str, op: AssignOp, value: &str, st: OpStatus, via: &[&str]| {
        (loc.to_string(), op, value.to_string(), st, strs(via))
    };
    let history = |boardx: OpStatus| {
        vec![
            row(
                "meta-core:recipes-core/hello/hello.inc:9",
                AssignOp::Set,
                "https://example.org/releases/hello-${PV}.tar.gz",
                OpStatus::Applied,
                &[recipe],
            ),
            row(
                "meta-core:recipes-core/hello/hello_1.1.bb:5",
                AssignOp::Append,
                "file://hello.patch",
                OpStatus::Applied,
                &[],
            ),
            row(
                "meta-vendor:recipes-core/hello/hello_%.bbappend:4",
                AssignOp::Set,
                " file://vendor.patch",
                OpStatus::Applied,
                &[recipe],
            ),
            row(
                "meta-vendor:recipes-core/hello/hello_%.bbappend:5",
                AssignOp::Set,
                " file://boardx.cfg",
                boardx,
                &[recipe],
            ),
            row(
                "meta-mine:recipes-core/hello/hello_1.1.bbappend:4",
                AssignOp::Set,
                " file://mine.patch",
                OpStatus::Applied,
                &[recipe],
            ),
        ]
    };

    let q = eval(&fx, &target(Some("hello"), Some("qemux86")));
    assert_eq!(src_uri_history(&q), history(OpStatus::InactiveOverride));
    assert_eq!(
        q.value("SRC_URI").unwrap().text,
        "https://example.org/releases/hello-1.1.tar.gz file://hello.patch \
         file://vendor.patch file://mine.patch"
    );
    let flag = q.flag_history("SRC_URI", "sha256sum");
    assert_eq!(flag.len(), 1);
    assert_eq!(
        flag[0].0.loc.to_string(),
        "meta-core:recipes-core/hello/hello_1.1.bb:6"
    );

    let b = eval(&fx, &target(Some("hello"), Some("boardx")));
    assert_eq!(src_uri_history(&b), history(OpStatus::Applied));
    assert_eq!(
        b.value("SRC_URI").unwrap().text,
        "https://example.org/releases/hello-1.1.tar.gz file://hello.patch \
         file://vendor.patch file://boardx.cfg file://mine.patch"
    );
    assert!(
        b.files()
            .iter()
            .any(|v| v.loc.path.ends_with("machine/boardx.conf"))
    );
    assert!(
        !b.files()
            .iter()
            .any(|v| v.loc.path.ends_with("machine/qemux86.conf"))
    );
}

#[test]
fn overrides_and_values_per_machine() {
    let fx = fixture();
    let q = eval(&fx, &target(Some("hello"), None));
    assert_eq!(
        q.value("OVERRIDES").unwrap().text,
        "linux:i686:pn-hello:layer-core:qemux86:testdistro:class-target:libc-glibc:forcevariable"
    );
    assert_eq!(q.value("FILE_LAYERNAME").unwrap().text, "core");
    assert_eq!(
        q.value("EXTRA_OECONF").unwrap().text,
        "--disable-nls --enable-local-debug --with-mine"
    );
    assert_eq!(q.value("DISTRO_FEATURES").unwrap().text, " wayland");
    let b = eval(&fx, &target(Some("hello"), Some("boardx")));
    assert_eq!(
        b.value("OVERRIDES").unwrap().text,
        "linux:arm:pn-hello:layer-core:socx:boardx:testdistro:class-target:libc-glibc:forcevariable"
    );
    assert_eq!(
        b.value("EXTRA_OECONF").unwrap().text,
        "--disable-nls --enable-local-debug --enable-boardx --with-mine"
    );
    // The target machine wins over local.conf's `MACHINE ??= "qemux86"`.
    assert_eq!(b.value("MACHINE").unwrap().text, "boardx");
    let h = b.history("MACHINE");
    assert_eq!(h.len(), 1);
    assert_eq!(h[0].0.loc.to_string(), "build:conf/local.conf:2");
    assert_eq!(h[0].1, OpStatus::Shadowed);
    // An app recipe in meta-mine gets layer-mine.
    let a = eval(&fx, &target(Some("app"), None));
    assert_eq!(a.value("FILE_LAYERNAME").unwrap().text, "mine");
    assert!(a.overrides().contains(&"layer-mine".to_string()));
}

#[test]
fn values_computed_from_file() {
    let fx = fixture();
    let ds = eval(&fx, &target(Some("hello"), None));
    let root = fx.ws.layer("meta-core").unwrap().root.clone();
    let recipe = root.join("recipes-core/hello/hello_1.1.bb");
    assert_eq!(ds.value("FILE").unwrap().text, recipe.to_string_lossy());
    assert_eq!(ds.value("PN").unwrap().text, "hello");
    assert_eq!(ds.value("PV").unwrap().text, "1.1");
    assert_eq!(ds.value("BPN").unwrap().text, "hello");
    assert_eq!(ds.value("BP").unwrap().text, "hello-1.1");
    assert_eq!(
        ds.value("THISDIR").unwrap().text,
        root.join("recipes-core/hello").to_string_lossy()
    );
    // `FILESEXTRAPATHS:prepend := "${THISDIR}/${PN}:"` in each bbappend saw that
    // bbappend's directory; the last prepend ends up first.
    let mine = fx
        .ws
        .layer("meta-mine")
        .unwrap()
        .root
        .join("recipes-core/hello/hello");
    let vendor = fx
        .ws
        .layer("meta-vendor")
        .unwrap()
        .root
        .join("recipes-core/hello/hello");
    assert_eq!(
        ds.value("FILESEXTRAPATHS").unwrap().text,
        format!("{}:{}:__default:", mine.display(), vendor.display())
    );
    assert_eq!(
        ds.value("S").unwrap().text.rsplit('/').next(),
        Some("hello-1.1")
    );
    // `FILES:${PN}` was renamed by expand_keys.
    assert_eq!(
        ds.value("FILES:hello").unwrap().text,
        "/usr/bin/* /usr/sbin/* /usr/lib/lib*.so.* /etc/hello.conf"
    );
    assert!(ds.replay().incomplete_keys.is_empty());
}

#[test]
fn layer_confs_see_their_own_layerdir() {
    let fx = fixture();
    let ds = eval(&fx, &target(None, None));
    let roots: Vec<String> = fx
        .ws
        .layers
        .iter()
        .map(|l| l.root.to_string_lossy().to_string())
        .collect();
    let build = fx
        .ws
        .build_dir
        .as_ref()
        .unwrap()
        .to_string_lossy()
        .to_string();
    assert_eq!(
        ds.value("BBPATH").unwrap().text,
        format!("{build}:{}", roots.join(":"))
    );
    assert_eq!(
        ds.value("BBFILE_PATTERN_vendor").unwrap().text,
        format!("^{}/", roots[1])
    );
    assert!(ds.value("LAYERDIR").is_none());
    assert!(ds.value("LAYERDIR_RE").is_none());
    // History keeps the text as written.
    let h = ds.history("BBFILE_PATTERN_vendor");
    assert_eq!(h[0].0.value, "^${LAYERDIR}/");
}

#[test]
fn configuration_only() {
    let fx = fixture();
    let ds = eval(&fx, &target(None, None));
    assert_eq!(visits(&ds), expected(CONFIG_VISITS));
    assert!(ds.replay().recipe.is_none());
    assert_eq!(ds.value("MACHINE").unwrap().text, "qemux86");
    assert_eq!(ds.value("DISTRO").unwrap().text, "testdistro");
    // No recipe: PN is unknown, and the static result says so.
    let ov = ds.value("OVERRIDES").unwrap();
    assert!(ov.unresolved.contains(&"PN".to_string()), "{ov:?}");
}

#[test]
fn functions_are_pointers() {
    let fx = fixture();
    let ds = eval(&fx, &target(Some("hello"), None));
    assert!(ds.is_function("do_install"));
    let pieces: Vec<(String, Option<String>)> = ds
        .function("do_install")
        .unwrap()
        .into_iter()
        .map(|p| (p.loc.to_string(), p.calls))
        .collect();
    assert_eq!(
        pieces,
        [
            (
                "meta-core:classes-recipe/autotoolsish.bbclass:30".to_string(),
                Some("autotoolsish_do_install".to_string())
            ),
            (
                "meta-core:recipes-core/hello/hello.inc:15-18".to_string(),
                None
            ),
            (
                "meta-mine:recipes-core/hello/hello_1.1.bbappend:8-10".to_string(),
                None
            ),
        ]
    );
    // base.bbclass's EXPORT_FUNCTIONS stub was replaced by autotoolsish's.
    let h = ds.history("do_install");
    assert_eq!(h[0].0.loc.to_string(), "meta-core:classes/base.bbclass:54");
    assert_eq!(h[0].1, OpStatus::Shadowed);
    assert!(ds.is_function("base_do_install"));
    assert_eq!(ds.raw_flag("base_do_fetch", "python").as_deref(), Some("1"));

    let app = eval(&fx, &target(Some("app"), None));
    let p = app.function("do_install").unwrap();
    assert_eq!(
        strs(&p.iter().map(|p| &p.loc).collect::<Vec<_>>()),
        ["meta-mine:recipes-app/app/app_0.1.bb:25-29"]
    );
    assert_eq!(app.raw_flag("do_install", "fakeroot").as_deref(), Some("1"));
    assert_eq!(app.raw_flag("do_report", "python").as_deref(), Some("1"));
    assert_eq!(app.raw_flag("do_compile", "python"), None);
    assert_eq!(
        app.raw_flag("app_helper_name", "func").as_deref(),
        Some("1")
    );
}

#[test]
fn python_values_are_flagged() {
    let fx = fixture();
    for machine in ["qemux86", "boardx"] {
        let ds = eval(&fx, &target(Some("app"), Some(machine)));
        // `${@'hi' if d.getVar('MACHINE') == 'boardx' else 'hello'}` is in the evaluated
        // subset and MACHINE is certain: evaluated statically.
        let g = ds.value("APP_GREETING").unwrap();
        assert!(!g.unevaluated_python, "{g:?}");
        assert_eq!(g.text, if machine == "boardx" { "hi" } else { "hello" });
        // Still flagged: anonymous python appends to it.
        let m = ds.value("EXTRA_OEMAKE").unwrap();
        assert!(m.unevaluated_python);
        assert!(ds.value("FILESPATH").unwrap().unevaluated_python);
        assert!(!ds.value("SRC_URI").unwrap().unevaluated_python);
        assert!(!ds.value("LICENSE").unwrap().unevaluated_python);
        assert_eq!(ds.value("RDEPENDS:app").unwrap().text, "libfoo");
        let anon = &ds.replay().anonymous;
        assert_eq!(anon.len(), 1);
        assert_eq!(
            anon[0].loc.to_string(),
            "meta-mine:recipes-app/app/app_0.1.bb:13-16"
        );
        assert_eq!(anon[0].sets, ["EXTRA_OEMAKE"]);
        assert!(!anon[0].opaque);
        assert!(anon[0].name.starts_with("__anon_16_"));
        assert!(ds.replay().python_vars.contains("EXTRA_OEMAKE"));
        assert_eq!(ds.raw_flag(&anon[0].name, "python").as_deref(), Some("1"));
    }
}

#[test]
fn anonymous_python_alone_flags_what_it_sets() {
    let (fx, _root) = modified(|root| {
        let dir = root.join("meta-mine/recipes-app/anon");
        fs::create_dir_all(&dir).unwrap();
        fs::write(
            dir.join("anon_1.0.bb"),
            "LICENSE = \"CLOSED\"\nTOUCHED = \"plain\"\nUSER = \"${TOUCHED}\"\n\
             python () {\n    d.setVar('TOUCHED', 'other')\n}\n\
             python __anonymous () {\n    oe.utils.do_things(d)\n}\n",
        )
        .unwrap();
    });
    let ds = eval(&fx, &target(Some("anon"), None));
    let t = ds.value("TOUCHED").unwrap();
    assert_eq!(t.text, "plain");
    assert!(t.unevaluated_python);
    // A value that uses it is flagged too.
    assert!(ds.value("USER").unwrap().unevaluated_python);
    assert!(!ds.value("LICENSE").unwrap().unevaluated_python);
    let anon = &ds.replay().anonymous;
    assert_eq!(anon.len(), 2);
    assert!(!anon[0].opaque);
    assert!(anon[1].opaque);
    assert!(anon[1].sets.is_empty());
}

#[test]
fn second_recipe_reuses_the_configuration_pass() {
    let fx = fixture();
    let cache = ConfigCache::new();
    let hello = eval_with(&fx, &cache, &target(Some("hello"), None));
    assert_eq!(cache.passes(), 1);
    let libfoo = eval_with(&fx, &cache, &target(Some("libfoo"), None));
    assert_eq!(cache.passes(), 1);
    let _global = eval_with(&fx, &cache, &target(None, None));
    assert_eq!(cache.passes(), 1);
    assert_eq!(hello.value("PN").unwrap().text, "hello");
    assert_eq!(libfoo.value("PN").unwrap().text, "libfoo");
    // The recipes did not leak into each other or into the cached configuration.
    assert!(
        libfoo
            .history("SRC_URI")
            .iter()
            .all(|(op, _)| !op.loc.path.starts_with("recipes-core/hello"))
    );
    let again = eval_with(&fx, &cache, &target(Some("hello"), None));
    assert_eq!(
        again.history("SRC_URI").len(),
        hello.history("SRC_URI").len()
    );
    assert_eq!(again.files().len(), hello.files().len());
    // Another machine is another configuration.
    eval_with(&fx, &cache, &target(Some("hello"), Some("boardx")));
    assert_eq!(cache.passes(), 2);
    eval_with(&fx, &cache, &target(Some("app"), Some("boardx")));
    assert_eq!(cache.passes(), 2);
}

#[test]
fn cache_is_invalidated_by_conf_and_class_changes_only() {
    let (mut fx, root) = modified(|_| {});
    let cache = ConfigCache::new();
    eval_with(&fx, &cache, &target(Some("hello"), None));
    assert_eq!(cache.passes(), 1);

    // A recipe change does not touch the configuration.
    fs::write(
        root.join("meta-core/recipes-core/hello/hello_1.1.bb"),
        "require hello.inc\nSRC_URI += \"file://other.patch\"\n",
    )
    .unwrap();
    fx.idx.refresh(&fx.ws).unwrap();
    let ds = eval_with(&fx, &cache, &target(Some("hello"), None));
    assert_eq!(cache.passes(), 1);
    assert!(
        ds.value("SRC_URI")
            .unwrap()
            .text
            .contains("file://other.patch")
    );

    // A conf change does.
    let local = root.join("build/conf/local.conf");
    let text = fs::read_to_string(&local).unwrap();
    fs::write(&local, format!("{text}EXTRA_OECONF:append = \" --late\"\n")).unwrap();
    fx.idx.refresh(&fx.ws).unwrap();
    let ds = eval_with(&fx, &cache, &target(Some("hello"), None));
    assert_eq!(cache.passes(), 2);
    assert!(ds.value("EXTRA_OECONF").unwrap().text.contains(" --late "));

    // So does a global class.
    let class = root.join("meta-core/classes-global/sanityish.bbclass");
    let text = fs::read_to_string(&class).unwrap();
    fs::write(&class, format!("{text}SANITY_EXTRA = \"1\"\n")).unwrap();
    fx.idx.refresh(&fx.ws).unwrap();
    let ds = eval_with(&fx, &cache, &target(Some("hello"), None));
    assert_eq!(cache.passes(), 3);
    assert_eq!(ds.value("SANITY_EXTRA").unwrap().text, "1");
}

#[test]
fn evaluate_uses_the_process_cache() {
    // The only test in this binary that uses the global cache.
    let fx = fixture();
    let before = eval::global_cache().passes();
    let a = eval::evaluate(&fx.idx, &fx.ws, &target(Some("hello"), None)).unwrap();
    let b = eval::evaluate(&fx.idx, &fx.ws, &target(Some("libfoo"), None)).unwrap();
    assert_eq!(eval::global_cache().passes(), before + 1);
    assert_eq!(a.value("PN").unwrap().text, "hello");
    assert_eq!(b.value("PN").unwrap().text, "libfoo");
}

#[test]
fn recipe_by_path_and_unknown_recipe() {
    let fx = fixture();
    let ds = eval(
        &fx,
        &target(Some("meta-core:recipes-core/hello/hello_1.0.bb"), None),
    );
    assert_eq!(ds.value("PV").unwrap().text, "1.0");
    let abs = fx
        .ws
        .layer("meta-core")
        .unwrap()
        .root
        .join("recipes-core/libfoo/libfoo_2.0.bb");
    let ds = eval(&fx, &target(Some(&abs.to_string_lossy()), None));
    assert_eq!(ds.value("PN").unwrap().text, "libfoo");
    let err = eval::evaluate_with(
        &ConfigCache::new(),
        &fx.idx,
        &fx.ws,
        &target(Some("nosuchrecipe"), None),
    )
    .unwrap_err();
    assert!(matches!(err, Error::NotFound { .. }), "{err}");
}

#[test]
fn unfollowed_references_are_recorded() {
    let (fx, _root) = modified(|root| {
        let dir = root.join("meta-mine/recipes-app/refs");
        fs::create_dir_all(&dir).unwrap();
        fs::write(
            dir.join("refs_1.0.bb"),
            "require missing.inc\ninclude also-missing.inc\ninclude ${@os.path.basename('x')}.inc\n\
             require ${NOT_SET}.inc\ninherit nosuchclass\ninherit autotoolsish autotoolsish\n\
             require refs_1.0.bb\nrequire self.inc\n",
        )
        .unwrap();
        fs::write(
            dir.join("self.inc"),
            "require self.inc\nX = \"1\"\nrequire refs.inc\n",
        )
        .unwrap();
        fs::write(dir.join("refs.inc"), "require self.inc\n").unwrap();
    });
    let ds = eval(&fx, &target(Some("refs"), None));
    let got: Vec<(String, String, RefProblem)> = ds
        .replay()
        .references
        .iter()
        .map(|r| {
            (
                r.loc.as_ref().map(|l| l.to_string()).unwrap_or_default(),
                r.kind.clone(),
                r.problem.clone(),
            )
        })
        .collect();
    let f = "meta-mine:recipes-app/refs/refs_1.0.bb";
    assert_eq!(
        got,
        [
            (
                format!("{f}:1"),
                "require".into(),
                RefProblem::NotFound("missing.inc".into())
            ),
            (format!("{f}:3"), "include".into(), RefProblem::Python),
            (
                format!("{f}:4"),
                "require".into(),
                RefProblem::Undefined(vec!["NOT_SET".into()])
            ),
            (
                format!("{f}:5"),
                "inherit".into(),
                RefProblem::NotFound("nosuchclass".into())
            ),
            (
                format!("{f}:7"),
                "require".into(),
                RefProblem::Cycle(format!("{f}:1").parse().unwrap())
            ),
            // BitBake skips an include only when the written name equals the including
            // file's path, so `require self.inc` inside self.inc recurses.
            (
                "meta-mine:recipes-app/refs/self.inc:1".into(),
                "require".into(),
                RefProblem::Cycle("meta-mine:recipes-app/refs/self.inc:1".parse().unwrap())
            ),
            (
                "meta-mine:recipes-app/refs/refs.inc:1".into(),
                "require".into(),
                RefProblem::Cycle("meta-mine:recipes-app/refs/self.inc:1".parse().unwrap())
            ),
        ]
    );
    // A class is parsed once.
    let classes = ds
        .files()
        .iter()
        .filter(|v| v.loc.path.ends_with("autotoolsish.bbclass"))
        .count();
    assert_eq!(classes, 1);
    assert_eq!(ds.value("X").unwrap().text, "1");
}

// ---------------------------------------------------------------------------------------
// The two recipe walkers: `tools::recipe::walk_recipe` (static, from the index edges) and
// the replay's visit list. They are kept apart on purpose (see the module docs of
// `mcp/tools/recipe.rs`); on the fixture recipes they must agree.

/// The files the static walk parses, in order (one entry per parse, repeats included).
fn static_walk(fx: &Fx, layer: &str, path: &str) -> Vec<(String, FileKind)> {
    let file = sous_chef::index::query::file_by_path(&fx.idx, layer, Path::new(path))
        .unwrap()
        .unwrap();
    let recipe = sous_chef::index::query::RecipeRow {
        pn: file.pn.clone().unwrap(),
        pv: file.pv.clone().unwrap(),
        file,
    };
    let walk = sous_chef::mcp::tools::recipe::walk_recipe(&fx.idx, &recipe).unwrap();
    walk.nodes
        .iter()
        .filter(|n| n.state == sous_chef::mcp::tools::recipe::NodeState::Parsed)
        .map(|n| {
            let f = n.file.as_ref().unwrap();
            (format!("{}:{}", f.layer, f.path.display()), f.kind)
        })
        .collect()
}

/// The files the replay's recipe pass visits, in order (the configuration pass dropped).
fn replay_walk(fx: &Fx, layer: &str, path: &str) -> Vec<(String, FileKind)> {
    let ds = eval(fx, &target(Some(&format!("{layer}:{path}")), None));
    let visits = ds.files();
    let start = visits
        .iter()
        .position(|v| v.kind == FileKind::Recipe)
        .unwrap();
    visits[start..]
        .iter()
        .map(|v| (format!("{}:{}", v.loc.layer, v.loc.path.display()), v.kind))
        .collect()
}

#[test]
fn static_walk_and_replay_agree_on_the_fixture_recipes() {
    let fx = fixture();
    for (layer, path) in [
        ("meta-core", "recipes-core/hello/hello_1.0.bb"),
        ("meta-core", "recipes-core/hello/hello_1.1.bb"),
        ("meta-mine", "recipes-app/app/app_0.1.bb"),
        ("meta-core", "recipes-core/libfoo/libfoo_2.0.bb"),
    ] {
        let a = static_walk(&fx, layer, path);
        let b = replay_walk(&fx, layer, path);
        assert!(a.len() > 1 || path.contains("app_"), "{path}: {a:?}");
        assert_eq!(a, b, "{layer}:{path}");
    }
}

#[test]
fn static_walk_and_replay_differ_where_documented() {
    // A documented difference: the static walk expands only the variables that follow from
    // the file's own path (`PN`, `BPN`, `THISDIR`, ...); the replay uses the live store.
    let (fx, _root) = modified(|root| {
        let dir = root.join("meta-mine/recipes-app/varinc");
        fs::create_dir_all(&dir).unwrap();
        fs::write(
            dir.join("varinc_1.0.bb"),
            "INCNAME = \"varinc\"\nrequire ${INCNAME}.inc\n",
        )
        .unwrap();
        fs::write(dir.join("varinc.inc"), "X = \"1\"\n").unwrap();
    });
    let (layer, path) = ("meta-mine", "recipes-app/varinc/varinc_1.0.bb");
    let (fx2, _root2) = modified(|root| {
        let dir = root.join("meta-mine/recipes-app/cls");
        fs::create_dir_all(&dir).unwrap();
        fs::write(
            dir.join("cls_1.0.bb"),
            "inherit base\ninherit_defer deferone\ninherit autotoolsish\n",
        )
        .unwrap();
        fs::write(dir.join("cls_%.bbappend"), "inherit autotoolsish\n").unwrap();
        let c = root.join("meta-mine/classes-recipe");
        fs::create_dir_all(&c).unwrap();
        fs::write(c.join("deferone.bbclass"), "inherit_defer defertwo\n").unwrap();
        fs::write(c.join("defertwo.bbclass"), "Y = \"2\"\n").unwrap();
    });
    // Two more: a class the configuration already inherited (`base`) is parsed again by the
    // static walk (it does not see the configuration's `__inherit_cache`), and an
    // `inherit_defer` inside a deferred class is dropped by the static walk (the replay runs
    // it, as `ast.finalize` does). A class inherited by both the recipe and its bbappend is
    // parsed once by both.
    let cls = |p: &str| (p.to_string(), FileKind::Class);
    let (recipe, append) = (
        (
            "meta-mine:recipes-app/cls/cls_1.0.bb".to_string(),
            FileKind::Recipe,
        ),
        (
            "meta-mine:recipes-app/cls/cls_%.bbappend".to_string(),
            FileKind::Append,
        ),
    );
    assert_eq!(
        static_walk(&fx2, "meta-mine", "recipes-app/cls/cls_1.0.bb"),
        [
            recipe.clone(),
            cls("meta-core:classes/base.bbclass"),
            cls("meta-core:classes-recipe/autotoolsish.bbclass"),
            append.clone(),
            cls("meta-mine:classes-recipe/deferone.bbclass"),
        ]
    );
    assert_eq!(
        replay_walk(&fx2, "meta-mine", "recipes-app/cls/cls_1.0.bb"),
        [
            recipe,
            cls("meta-core:classes-recipe/autotoolsish.bbclass"),
            append,
            cls("meta-mine:classes-recipe/deferone.bbclass"),
            cls("meta-mine:classes-recipe/defertwo.bbclass"),
        ]
    );
    assert_eq!(
        static_walk(&fx, layer, path),
        [(format!("{layer}:{path}"), FileKind::Recipe)]
    );
    assert_eq!(
        replay_walk(&fx, layer, path),
        [
            (format!("{layer}:{path}"), FileKind::Recipe),
            (
                "meta-mine:recipes-app/varinc/varinc.inc".to_string(),
                FileKind::Include
            ),
        ]
    );
}

/// The fixture copy on `codename` with a recipe that uses `inherit_defer` on a class in
/// `classes/` (found on every release).
fn deferring(codename: &str) -> Fx {
    let (tmp, _root, _cfg, ws) = temp_ws_release(codename, |root| {
        let dir = root.join("meta-mine/recipes-app/later");
        fs::create_dir_all(&dir).unwrap();
        fs::write(
            dir.join("later_1.0.bb"),
            "inherit_defer deferme\nX = \"1\"\n",
        )
        .unwrap();
        let c = root.join("meta-mine/classes");
        fs::create_dir_all(&c).unwrap();
        fs::write(c.join("deferme.bbclass"), "DEFERRED = \"yes\"\n").unwrap();
    });
    indexed(ws, Some(tmp))
}

#[test]
fn inherit_defer_before_scarthgap_is_a_problem_and_inherits_nothing() {
    let fx = deferring("kirkstone");
    let ds = eval(&fx, &target(Some("later"), None));
    let issues: Vec<(String, String, String, RefProblem)> = ds
        .replay()
        .references
        .iter()
        .filter(|r| r.kind == "inherit_defer")
        .map(|r| {
            (
                r.loc.as_ref().map(|l| l.to_string()).unwrap_or_default(),
                r.kind.clone(),
                r.text.clone(),
                r.problem.clone(),
            )
        })
        .collect();
    assert_eq!(
        issues,
        [(
            "meta-mine:recipes-app/later/later_1.0.bb:1".to_string(),
            "inherit_defer".to_string(),
            "deferme".to_string(),
            RefProblem::NotInRelease(Feature::InheritDefer),
        )]
    );
    assert!(ds.value("DEFERRED").is_none());
    assert!(
        !ds.files()
            .iter()
            .any(|v| v.loc.path.ends_with("deferme.bbclass"))
    );
    // The rest of the recipe is still replayed.
    assert_eq!(ds.value("X").unwrap().text, "1");
}

#[test]
fn inherit_defer_from_scarthgap_inherits_at_the_end() {
    let fx = deferring("scarthgap");
    let ds = eval(&fx, &target(Some("later"), None));
    assert!(
        ds.replay()
            .references
            .iter()
            .all(|r| r.kind != "inherit_defer"),
        "{:?}",
        ds.replay().references
    );
    assert_eq!(ds.value("DEFERRED").unwrap().text, "yes");
    let last = ds.files().last().unwrap();
    assert_eq!(last.loc.to_string(), "meta-mine:classes/deferme.bbclass:1");
}
