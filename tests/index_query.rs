//! T2.3: query layer (`index::query`) over `tests/fixtures/ws`.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::fs;
use std::path::{Path, PathBuf};

use common::*;
use sous_chef::error::Error;
use sous_chef::index::Index;
use sous_chef::index::query::{self, FileRow, StatementFilter};
use sous_chef::types::FileKind;
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
fn modified(edit: impl FnOnce(&Path)) -> Fx {
    let (tmp, root) = temp_ws();
    edit(&root);
    let ws = Workspace::discover(&ws_config(&root)).unwrap();
    indexed(ws, Some(tmp))
}

fn write(p: &Path, text: &str) {
    fs::create_dir_all(p.parent().unwrap()).unwrap();
    fs::write(p, text).unwrap();
}

fn lp(f: &FileRow) -> String {
    format!("{}:{}", f.layer, f.path.display())
}

fn id(fx: &Fx, layer: &str, path: &str) -> i64 {
    query::file_by_path(&fx.idx, layer, Path::new(path))
        .unwrap()
        .unwrap_or_else(|| panic!("{layer}:{path} not indexed"))
        .id
}

#[test]
fn files_by_id_path_and_abs() {
    let fx = fixture();
    let i = id(&fx, "meta-core", "recipes-core/hello/hello.inc");
    let f = query::file(&fx.idx, i).unwrap().unwrap();
    assert_eq!(lp(&f), "meta-core:recipes-core/hello/hello.inc");
    assert_eq!(f.kind, FileKind::Include);
    assert_eq!(f.priority, 5);
    assert_eq!(f.sha256.len(), 64);
    let abs = fx
        .ws
        .layer("meta-core")
        .unwrap()
        .root
        .join("recipes-core/hello/hello.inc");
    assert_eq!(query::file_by_abs(&fx.idx, &fx.ws, &abs).unwrap(), Some(f));
    assert_eq!(query::file(&fx.idx, 999_999).unwrap(), None);
    assert_eq!(
        query::file_by_path(&fx.idx, "meta-core", Path::new("nope.bb")).unwrap(),
        None
    );
}

#[test]
fn find_recipes_all_versions() {
    let fx = fixture();
    let rows = query::find_recipes(&fx.idx, "hello").unwrap();
    let got: Vec<(String, String, String)> = rows
        .iter()
        .map(|r| (r.pn.clone(), r.pv.clone(), lp(&r.file)))
        .collect();
    assert_eq!(
        got,
        [
            (
                "hello".into(),
                "1.1".into(),
                "meta-core:recipes-core/hello/hello_1.1.bb".into()
            ),
            (
                "hello".into(),
                "1.0".into(),
                "meta-core:recipes-core/hello/hello_1.0.bb".into()
            ),
        ]
    );
    assert!(query::find_recipes(&fx.idx, "orphan").unwrap().is_empty());
    assert_eq!(query::find_recipes(&fx.idx, "libfoo-alt").unwrap().len(), 1);
}

#[test]
fn preferred_recipe_by_priority_and_version() {
    let fx = fixture();
    let p = |name: &str, v: Option<&str>| {
        query::preferred_recipe(&fx.idx, name, v)
            .unwrap()
            .map(|r| r.pv)
    };
    assert_eq!(p("hello", None).as_deref(), Some("1.1"));
    assert_eq!(p("hello", Some("1.0")).as_deref(), Some("1.0"));
    // The first match in collection order, not the highest (findPreferredProvider).
    assert_eq!(p("hello", Some("1.%")).as_deref(), Some("1.0"));
    assert_eq!(p("hello", Some("2.%")), None);
    assert_eq!(p("nope", None), None);
}

#[test]
fn preferred_recipe_version_ordering() {
    let fx = modified(|root| {
        let d = root.join("meta-core/recipes-core/hello");
        for v in ["1.9", "1.10", "1.10+git"] {
            fs::copy(d.join("hello_1.0.bb"), d.join(format!("hello_{v}.bb"))).unwrap();
        }
        let d = root.join("meta-core/recipes-core/ver");
        for v in ["1.9", "1.10", "git"] {
            write(&d.join(format!("ver_{v}.bb")), "LICENSE = \"MIT\"\n");
        }
    });
    let pvs = |name: &str| -> Vec<String> {
        query::find_recipes(&fx.idx, name)
            .unwrap()
            .into_iter()
            .map(|r| r.pv)
            .collect()
    };
    assert_eq!(pvs("hello"), ["1.10+git", "1.10", "1.9", "1.1", "1.0"]);
    assert_eq!(
        query::preferred_recipe(&fx.idx, "hello", None)
            .unwrap()
            .unwrap()
            .pv,
        "1.10+git"
    );
    // `git` sorts above any number.
    assert_eq!(pvs("ver"), ["git", "1.10", "1.9"]);
}

#[test]
fn preferred_recipe_highest_priority_layer_wins() {
    let fx = modified(|root| {
        // meta-mine (priority 10) has an older hello than meta-core (priority 5).
        write(
            &root.join("meta-mine/recipes-core/hello/hello_0.5.bb"),
            "LICENSE = \"MIT\"\n",
        );
    });
    let best = query::preferred_recipe(&fx.idx, "hello", None)
        .unwrap()
        .unwrap();
    assert_eq!(lp(&best.file), "meta-mine:recipes-core/hello/hello_0.5.bb");
    // An explicit version is searched in every layer, highest priority first.
    let v = query::preferred_recipe(&fx.idx, "hello", Some("1.1"))
        .unwrap()
        .unwrap();
    assert_eq!(lp(&v.file), "meta-core:recipes-core/hello/hello_1.1.bb");
}

#[test]
fn appends_in_application_order() {
    let fx = fixture();
    let names = |path: &str| -> Vec<String> {
        query::appends_for(&fx.idx, id(&fx, "meta-core", path))
            .unwrap()
            .iter()
            .map(lp)
            .collect()
    };
    assert_eq!(
        names("recipes-core/hello/hello_1.1.bb"),
        [
            "meta-vendor:recipes-core/hello/hello_%.bbappend",
            "meta-mine:recipes-core/hello/hello_1.1.bbappend",
        ]
    );
    assert_eq!(
        names("recipes-core/hello/hello_1.0.bb"),
        ["meta-vendor:recipes-core/hello/hello_%.bbappend"]
    );
    assert!(names("recipes-core/libfoo/libfoo_2.0.bb").is_empty());
}

#[test]
fn orphan_appends_match_nothing() {
    let fx = fixture();
    let got: Vec<String> = query::orphan_appends(&fx.idx)
        .unwrap()
        .iter()
        .map(lp)
        .collect();
    assert_eq!(got, ["meta-mine:recipes-app/orphan/orphan_9.%.bbappend"]);
}

#[test]
fn statements_in_a_file() {
    let fx = fixture();
    let inc = id(&fx, "meta-core", "recipes-core/hello/hello.inc");
    let all = query::statements_in(&fx.idx, inc, &StatementFilter::default()).unwrap();
    assert_eq!(all.len(), 11);
    assert!(all.windows(2).all(|w| w[0].ord < w[1].ord));
    let funcs = query::statements_in(
        &fx.idx,
        inc,
        &StatementFilter {
            kinds: vec!["function".into()],
            ..StatementFilter::default()
        },
    )
    .unwrap();
    assert_eq!(funcs.len(), 1);
    assert_eq!(
        funcs[0].loc.to_string(),
        "meta-core:recipes-core/hello/hello.inc:15-18"
    );
    assert_eq!(funcs[0].overrides, ["append"]);
    assert_eq!(funcs[0].op.as_deref(), Some("shell"));
}

#[test]
fn statement_by_name_exact() {
    let fx = fixture();
    let inc = id(&fx, "meta-core", "recipes-core/hello/hello.inc");
    let got = query::statement_by_name(&fx.idx, inc, "do_install:append").unwrap();
    assert_eq!(got.len(), 1);
    assert_eq!(got[0].loc.lines.to_string(), "15-18");
    assert!(
        query::statement_by_name(&fx.idx, inc, "do_install")
            .unwrap()
            .is_empty()
    );
    let sv = query::statement_by_name(&fx.idx, inc, "FILES:${PN}").unwrap();
    assert_eq!(sv[0].op.as_deref(), Some("+="));
    assert_eq!(sv[0].value.as_deref(), Some("${sysconfdir}/hello.conf"));
}

#[test]
fn statements_by_base_in_scope() {
    let fx = fixture();
    // hello 1.1's files: its tree, then its appends in application order.
    let recipe = id(&fx, "meta-core", "recipes-core/hello/hello_1.1.bb");
    let mut files: Vec<i64> = query::tree(&fx.idx, recipe)
        .unwrap()
        .iter()
        .filter_map(|n| n.file.as_ref().map(|f| f.id))
        .collect();
    files.extend(
        query::appends_for(&fx.idx, recipe)
            .unwrap()
            .iter()
            .map(|f| f.id),
    );
    let scope = StatementFilter {
        files: Some(files),
        ..StatementFilter::default()
    };
    let got: Vec<String> = query::statements_by_base(&fx.idx, "SRC_URI", &scope)
        .unwrap()
        .iter()
        .map(|s| s.loc.to_string())
        .collect();
    assert_eq!(
        got,
        [
            "meta-core:recipes-core/hello/hello_1.1.bb:5",
            "meta-core:recipes-core/hello/hello_1.1.bb:6",
            "meta-core:recipes-core/hello/hello.inc:9",
            "meta-vendor:recipes-core/hello/hello_%.bbappend:4",
            "meta-vendor:recipes-core/hello/hello_%.bbappend:5",
            "meta-mine:recipes-core/hello/hello_1.1.bbappend:4",
        ]
    );
    // Unscoped: every file, by layer and path.
    let all =
        query::statements_by_base(&fx.idx, "EXTRA_OECONF", &StatementFilter::default()).unwrap();
    assert_eq!(all.len(), 8, "{all:#?}");
    let layer_only = StatementFilter {
        layer: Some("meta-vendor".into()),
        ..StatementFilter::default()
    };
    assert_eq!(
        query::statements_by_base(&fx.idx, "EXTRA_OECONF", &layer_only)
            .unwrap()
            .iter()
            .map(|s| s.overrides.join(":"))
            .collect::<Vec<_>>(),
        // The vendor append, then libfoo-alt's plain assignment (path order).
        ["append:boardx", "append:boradx", ""]
    );
}

#[test]
fn edges_from_and_to() {
    let fx = fixture();
    let bb = id(&fx, "meta-core", "recipes-core/hello/hello_1.1.bb");
    let inc = id(&fx, "meta-core", "recipes-core/hello/hello.inc");
    let from = query::edges_from(&fx.idx, bb).unwrap();
    assert_eq!(from.len(), 1);
    assert_eq!(from[0].kind, "require");
    assert_eq!(from[0].target.as_ref().map(|t| t.id), Some(inc));
    assert_eq!(
        from[0].via().to_string(),
        "meta-core:recipes-core/hello/hello_1.1.bb:1"
    );
    let to: Vec<(String, String)> = query::edges_to(&fx.idx, inc)
        .unwrap()
        .iter()
        .map(|e| (e.kind.clone(), lp(&e.src)))
        .collect();
    assert_eq!(
        to,
        [
            (
                "require".into(),
                "meta-core:recipes-core/hello/hello_1.0.bb".into()
            ),
            (
                "require".into(),
                "meta-core:recipes-core/hello/hello_1.1.bb".into()
            ),
        ]
    );
    let to_bb: Vec<String> = query::edges_to(&fx.idx, bb)
        .unwrap()
        .iter()
        .map(|e| format!("{} {}", e.kind, lp(&e.src)))
        .collect();
    assert_eq!(
        to_bb,
        [
            "bbappend meta-vendor:recipes-core/hello/hello_%.bbappend",
            "bbappend meta-mine:recipes-core/hello/hello_1.1.bbappend",
        ]
    );
    let conf = id(&fx, "meta-core", "conf/bitbake.conf");
    let unresolved: Vec<String> = query::edges_from(&fx.idx, conf)
        .unwrap()
        .iter()
        .filter(|e| e.target.is_none())
        .map(|e| e.target_text.clone())
        .collect();
    assert_eq!(
        unresolved,
        [
            "conf/site.conf",
            "conf/auto.conf",
            "conf/machine/${MACHINE}.conf",
            "conf/distro/${DISTRO}.conf"
        ]
    );
}

#[test]
fn tree_in_parse_order() {
    let fx = fixture();
    let bb = id(&fx, "meta-core", "recipes-core/hello/hello_1.1.bb");
    let got: Vec<(u32, String, Option<String>, Option<String>)> = query::tree(&fx.idx, bb)
        .unwrap()
        .iter()
        .map(|n| {
            (
                n.depth,
                n.kind.clone(),
                n.via.as_ref().map(|v| v.to_string()),
                n.file.as_ref().map(lp),
            )
        })
        .collect();
    assert_eq!(
        got,
        [
            (
                0,
                "root".into(),
                None,
                Some("meta-core:recipes-core/hello/hello_1.1.bb".into())
            ),
            (
                1,
                "require".into(),
                Some("meta-core:recipes-core/hello/hello_1.1.bb:1".into()),
                Some("meta-core:recipes-core/hello/hello.inc".into())
            ),
            (
                2,
                "inherit".into(),
                Some("meta-core:recipes-core/hello/hello.inc:11".into()),
                Some("meta-core:classes-recipe/autotoolsish.bbclass".into())
            ),
        ]
    );
    let conf = id(&fx, "meta-core", "conf/bitbake.conf");
    let t = query::tree(&fx.idx, conf).unwrap();
    assert_eq!(t.len(), 6);
    assert_eq!(
        t[3].file.as_ref().map(lp).as_deref(),
        Some("build:conf/local.conf")
    );
    assert!(t[1].file.is_none() && t[2].file.is_none());
    assert!(matches!(
        query::tree(&fx.idx, 999_999),
        Err(Error::NotFound { .. })
    ));
}

#[test]
fn tree_survives_a_cyclic_include() {
    let fx = modified(|root| {
        let d = root.join("meta-mine/recipes-app/cyc");
        write(
            &d.join("cyc_1.0.bb"),
            "require a.inc\ninherit autotoolsish\n",
        );
        write(&d.join("a.inc"), "require b.inc\nA = \"1\"\n");
        write(&d.join("b.inc"), "require a.inc\ninherit autotoolsish\n");
    });
    let bb = id(&fx, "meta-mine", "recipes-app/cyc/cyc_1.0.bb");
    let got: Vec<(u32, String, bool, bool)> = query::tree(&fx.idx, bb)
        .unwrap()
        .iter()
        .map(|n| {
            (
                n.depth,
                n.file.as_ref().map(lp).unwrap_or_default(),
                n.cycle,
                n.repeated,
            )
        })
        .collect();
    assert_eq!(
        got,
        [
            (
                0,
                "meta-mine:recipes-app/cyc/cyc_1.0.bb".into(),
                false,
                false
            ),
            (1, "meta-mine:recipes-app/cyc/a.inc".into(), false, false),
            (2, "meta-mine:recipes-app/cyc/b.inc".into(), false, false),
            (3, "meta-mine:recipes-app/cyc/a.inc".into(), true, false),
            (
                3,
                "meta-core:classes-recipe/autotoolsish.bbclass".into(),
                false,
                false
            ),
            // Inherited again from the recipe: BitBake skips it (__inherit_cache).
            (
                1,
                "meta-core:classes-recipe/autotoolsish.bbclass".into(),
                false,
                true
            ),
        ]
    );
}

#[test]
fn class_files_with_precedence() {
    let fx = fixture();
    let c = |name: &str, global: bool| -> Vec<(String, Option<usize>)> {
        query::class_files(&fx.idx, &fx.ws, name, global)
            .unwrap()
            .iter()
            .map(|c| (lp(&c.file), c.rank))
            .collect()
    };
    assert_eq!(
        c("autotoolsish", false),
        [(
            "meta-core:classes-recipe/autotoolsish.bbclass".into(),
            Some(0)
        )]
    );
    assert_eq!(
        c("sanityish", false),
        [("meta-core:classes-global/sanityish.bbclass".into(), None)]
    );
    assert_eq!(
        c("sanityish", true),
        [("meta-core:classes-global/sanityish.bbclass".into(), Some(0))]
    );
    assert!(c("nosuch", false).is_empty());
    // `_` in a name is not a LIKE wildcard.
    assert!(c("bas_", false).is_empty());
}

#[test]
fn class_files_shadowed_across_layers() {
    let fx = modified(|root| {
        write(
            &root.join("meta-mine/classes-recipe/autotoolsish.bbclass"),
            "X = \"1\"\n",
        );
        write(
            &root.join("meta-vendor/classes/autotoolsish.bbclass"),
            "X = \"2\"\n",
        );
    });
    let got: Vec<(String, Option<usize>)> =
        query::class_files(&fx.idx, &fx.ws, "autotoolsish", false)
            .unwrap()
            .iter()
            .map(|c| (lp(&c.file), c.rank))
            .collect();
    assert_eq!(
        got,
        [
            (
                "meta-core:classes-recipe/autotoolsish.bbclass".into(),
                Some(0)
            ),
            (
                "meta-mine:classes-recipe/autotoolsish.bbclass".into(),
                Some(1)
            ),
            ("meta-vendor:classes/autotoolsish.bbclass".into(), Some(2)),
        ]
    );
}

#[test]
fn inheritors_direct_and_transitive() {
    let fx = fixture();
    let users = |transitive: bool| -> Vec<(String, u32, String)> {
        query::inheritors(&fx.idx, "autotoolsish", transitive)
            .unwrap()
            .iter()
            .map(|u| (lp(&u.file), u.depth, u.via.to_string()))
            .collect()
    };
    let direct = vec![
        (
            "meta-core:recipes-core/hello/hello.inc".to_string(),
            1,
            "meta-core:recipes-core/hello/hello.inc:11".to_string(),
        ),
        (
            "meta-core:recipes-core/libfoo/libfoo_2.0.bb".into(),
            1,
            "meta-core:recipes-core/libfoo/libfoo_2.0.bb:12".into(),
        ),
        (
            "meta-vendor:recipes-vendor/libfoo-alt/libfoo-alt_1.0.bb".into(),
            1,
            "meta-vendor:recipes-vendor/libfoo-alt/libfoo-alt_1.0.bb:11".into(),
        ),
    ];
    assert_eq!(users(false), direct);
    let mut all = direct.clone();
    all.push((
        "meta-core:recipes-core/hello/hello_1.0.bb".into(),
        2,
        "meta-core:recipes-core/hello/hello_1.0.bb:1".into(),
    ));
    all.push((
        "meta-core:recipes-core/hello/hello_1.1.bb".into(),
        2,
        "meta-core:recipes-core/hello/hello_1.1.bb:1".into(),
    ));
    assert_eq!(users(true), all);
    assert!(
        query::inheritors(&fx.idx, "sanityish", true)
            .unwrap()
            .is_empty()
    );
}

#[test]
fn inheritors_through_a_bbappend_and_a_class() {
    let fx = modified(|root| {
        write(
            &root.join("meta-mine/classes-recipe/wrapper.bbclass"),
            "inherit autotoolsish\n",
        );
        let a = root.join("meta-mine/recipes-app/app/app_%.bbappend");
        write(&a, "inherit wrapper\n");
    });
    let got: Vec<(String, u32)> = query::inheritors(&fx.idx, "wrapper", true)
        .unwrap()
        .iter()
        .map(|u| (lp(&u.file), u.depth))
        .collect();
    assert_eq!(
        got,
        [
            ("meta-mine:recipes-app/app/app_%.bbappend".into(), 1),
            ("meta-mine:recipes-app/app/app_0.1.bb".into(), 2),
        ]
    );
    let via_class: Vec<String> = query::inheritors(&fx.idx, "autotoolsish", true)
        .unwrap()
        .iter()
        .map(|u| lp(&u.file))
        .collect();
    assert!(via_class.contains(&"meta-mine:classes-recipe/wrapper.bbclass".to_string()));
    assert!(via_class.contains(&"meta-mine:recipes-app/app/app_0.1.bb".to_string()));
}

#[test]
fn search_by_name() {
    let fx = fixture();
    let filter = StatementFilter {
        kinds: vec!["function".into()],
        layer: Some("meta-mine".into()),
        ..StatementFilter::default()
    };
    let got: Vec<String> = query::search_names(&fx.idx, "do_install", &filter, 30)
        .unwrap()
        .iter()
        .map(|s| s.loc.to_string())
        .collect();
    assert_eq!(
        got,
        [
            "meta-mine:recipes-app/app/app_0.1.bb:25-29",
            "meta-mine:recipes-core/hello/hello_1.1.bbappend:8-10",
        ]
    );
    // Case-insensitive substring, exact matches first, limit respected.
    let any = query::search_names(&fx.idx, "SRC_uri", &StatementFilter::default(), 3).unwrap();
    assert_eq!(any.len(), 3);
    assert!(
        any.iter()
            .all(|s| s.name.as_deref().unwrap().contains("SRC_URI"))
    );
    assert_eq!(any[0].name.as_deref(), Some("SRC_URI"));
    // `_` is literal, not a wildcard.
    assert!(
        query::search_names(&fx.idx, "SRC_URIX", &StatementFilter::default(), 30)
            .unwrap()
            .is_empty()
    );
    let classes_only = StatementFilter {
        file_kind: Some(FileKind::Class),
        ..StatementFilter::default()
    };
    let in_classes = query::search_names(&fx.idx, "do_install", &classes_only, 30).unwrap();
    assert!(!in_classes.is_empty());
    assert!(
        in_classes
            .iter()
            .all(|s| s.loc.path.starts_with("classes") || s.loc.path.starts_with("classes-recipe"))
    );
}

#[test]
fn search_full_text() {
    let fx = fixture();
    let got = query::search_fts(&fx.idx, "boradx", &StatementFilter::default(), 10).unwrap();
    assert_eq!(got.len(), 1);
    assert_eq!(
        got[0].loc.to_string(),
        "meta-vendor:recipes-core/hello/hello_%.bbappend:9"
    );
    let vendor_patch =
        query::search_fts(&fx.idx, "\"vendor.patch\"", &StatementFilter::default(), 10).unwrap();
    assert_eq!(vendor_patch.len(), 1);
    let scoped = StatementFilter {
        files: Some(vec![id(&fx, "meta-mine", "recipes-app/app/app_0.1.bb")]),
        ..StatementFilter::default()
    };
    assert!(
        query::search_fts(&fx.idx, "boradx", &scoped, 10)
            .unwrap()
            .is_empty()
    );
    let err =
        query::search_fts(&fx.idx, "\"unterminated", &StatementFilter::default(), 10).unwrap_err();
    assert!(matches!(err, Error::Invalid { .. }), "{err}");
}

#[test]
fn enclosing_item_covers_bodies() {
    let fx = fixture();
    let inc = id(&fx, "meta-core", "recipes-core/hello/hello.inc");
    let at = |line: u32| {
        query::enclosing_item(&fx.idx, inc, line)
            .unwrap()
            .map(|s| (s.name.unwrap_or_default(), s.loc.lines.to_string()))
    };
    assert_eq!(at(16), Some(("do_install:append".into(), "15-18".into())));
    assert_eq!(at(15), Some(("do_install:append".into(), "15-18".into())));
    assert_eq!(at(9), Some(("SRC_URI".into(), "9".into())));
    assert_eq!(at(19), None);
    assert_eq!(at(500), None);
}

#[test]
fn layer_stats_counts() {
    let fx = fixture();
    let got: Vec<(String, i32, [u32; 5], bool)> = query::layer_stats(&fx.idx)
        .unwrap()
        .iter()
        .map(|l| {
            (
                l.name.clone(),
                l.priority,
                [l.recipes, l.appends, l.includes, l.classes, l.confs],
                l.writable,
            )
        })
        .collect();
    assert_eq!(
        got,
        [
            ("meta-core".into(), 5, [3, 0, 1, 3, 5], false),
            ("meta-vendor".into(), 6, [1, 1, 0, 0, 1], false),
            ("meta-mine".into(), 10, [1, 2, 0, 0, 1], true),
            ("build".into(), 0, [0, 0, 0, 0, 2], false),
        ]
    );
    let core = &query::layer_stats(&fx.idx).unwrap()[0];
    assert_eq!(core.collections, ["core"]);
    assert_eq!(core.compat, ["wrynose"]);
    assert_eq!(core.root, fx.ws.layer("meta-core").unwrap().root);
    assert!(core.statements > 0);
}

#[test]
fn shadowed_recipes_and_classes() {
    assert!(
        query::shadowed(&fixture().idx, &fixture_ws())
            .unwrap()
            .is_empty()
    );
    let fx = modified(|root| {
        fs::copy(
            root.join("meta-core/recipes-core/hello/hello_1.0.bb"),
            root.join("meta-mine/recipes-core/hello/hello_1.0.bb"),
        )
        .unwrap();
        write(
            &root.join("meta-vendor/classes-recipe/autotoolsish.bbclass"),
            "X = \"1\"\n",
        );
    });
    let got: Vec<(String, String, Vec<String>)> = query::shadowed(&fx.idx, &fx.ws)
        .unwrap()
        .iter()
        .map(|s| {
            (
                s.kind.clone(),
                s.name.clone(),
                s.files.iter().map(lp).collect(),
            )
        })
        .collect();
    assert_eq!(
        got,
        [
            (
                "class".into(),
                "autotoolsish.bbclass".into(),
                vec![
                    "meta-core:classes-recipe/autotoolsish.bbclass".to_string(),
                    "meta-vendor:classes-recipe/autotoolsish.bbclass".to_string(),
                ]
            ),
            (
                "recipe".into(),
                "hello_1.0.bb".into(),
                vec![
                    "meta-mine:recipes-core/hello/hello_1.0.bb".to_string(),
                    "meta-core:recipes-core/hello/hello_1.0.bb".to_string(),
                ]
            ),
        ]
    );
}

#[test]
fn vercmp_orders_versions() {
    use std::cmp::Ordering::*;
    assert_eq!(query::vercmp("1.10", "1.9"), Greater);
    assert_eq!(query::vercmp("1.0+git999", "1.0"), Greater);
    assert_eq!(query::vercmp("1.0+gitAUTOINC+1234", "1.1"), Less);
    assert_eq!(query::vercmp("git", "9999"), Greater);
    let mut v = vec!["1.10", "1.9", "1.0+git", "1.0", "1.0~rc1", "git", "1.2.3"];
    v.sort_by(|a, b| query::vercmp(a, b));
    assert_eq!(
        v,
        ["1.0~rc1", "1.0", "1.0+git", "1.2.3", "1.9", "1.10", "git"]
    );
}

#[test]
fn rows_serialize() {
    let fx = fixture();
    let rows = query::find_recipes(&fx.idx, "hello").unwrap();
    let json = serde_json::to_value(&rows).unwrap();
    assert_eq!(json[0]["pv"], "1.1");
    let _: PathBuf = rows[0].file.path.clone();
}

#[test]
fn all_files_by_layer_then_path() {
    let fx = fixture();
    let rows = query::all_files(&fx.idx).unwrap();
    assert!(rows.iter().all(|f| f.sha256.len() == 64));
    let all: Vec<String> = rows.iter().map(lp).collect();
    assert_eq!(all.len(), 21);
    // Layer row order (BBLAYERS order, build directory last), then path.
    assert_eq!(all[0], "meta-core:classes-global/sanityish.bbclass");
    assert_eq!(all.last().unwrap(), "build:conf/local.conf");
    let mut layers: Vec<&str> = all.iter().map(|s| s.split(':').next().unwrap()).collect();
    layers.dedup();
    assert_eq!(layers, ["meta-core", "meta-vendor", "meta-mine", "build"]);
}

#[test]
fn files_by_kind_and_in_layer() {
    let fx = fixture();
    let appends: Vec<String> = query::files_by_kind(&fx.idx, &[FileKind::Append])
        .unwrap()
        .iter()
        .map(lp)
        .collect();
    assert_eq!(
        appends,
        [
            "meta-vendor:recipes-core/hello/hello_%.bbappend",
            "meta-mine:recipes-app/orphan/orphan_9.%.bbappend",
            "meta-mine:recipes-core/hello/hello_1.1.bbappend",
        ]
    );
    let some = query::files_by_kind(&fx.idx, &[FileKind::Class, FileKind::Conf]).unwrap();
    assert_eq!(some.len(), 3 + 9);
    assert!(
        some.iter()
            .all(|f| matches!(f.kind, FileKind::Class | FileKind::Conf))
    );
    assert!(query::files_by_kind(&fx.idx, &[]).unwrap().is_empty());

    let mine: Vec<String> = query::files_in_layer(&fx.idx, "meta-mine")
        .unwrap()
        .iter()
        .map(lp)
        .collect();
    assert_eq!(
        mine,
        [
            "meta-mine:conf/layer.conf",
            "meta-mine:recipes-app/app/app_0.1.bb",
            "meta-mine:recipes-app/orphan/orphan_9.%.bbappend",
            "meta-mine:recipes-core/hello/hello_1.1.bbappend",
        ]
    );
    assert_eq!(query::files_in_layer(&fx.idx, "build").unwrap().len(), 2);
    assert!(query::files_in_layer(&fx.idx, "nope").unwrap().is_empty());
}

#[test]
fn recipe_and_class_names_like() {
    let fx = fixture();
    assert_eq!(
        query::recipe_names_like(&fx.idx, "LIB", 20).unwrap(),
        ["libfoo", "libfoo-alt"]
    );
    assert_eq!(
        query::recipe_names_like(&fx.idx, "l", 1).unwrap(),
        ["hello"]
    );
    // `_` and `%` are literal, not wildcards.
    assert!(
        query::recipe_names_like(&fx.idx, "_", 20)
            .unwrap()
            .is_empty()
    );
    assert!(
        query::recipe_names_like(&fx.idx, "%", 20)
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        query::class_names_like(&fx.idx, "ISH", 20).unwrap(),
        ["autotoolsish", "sanityish"]
    );
    assert_eq!(
        query::class_names_like(&fx.idx, "", 2).unwrap(),
        ["autotoolsish", "base"]
    );
    assert!(
        query::class_names_like(&fx.idx, "zzz", 20)
            .unwrap()
            .is_empty()
    );
}

/// The fixture copy configured as Kirkstone (no `classes-recipe/` / `classes-global/`
/// search), with a second `autotoolsish` and `base` in meta-vendor.
fn kirkstone_with_duplicates() -> Fx {
    let (tmp, _root, _cfg, ws) = temp_ws_release("kirkstone", |root| {
        write(
            &root.join("meta-vendor/classes-recipe/autotoolsish.bbclass"),
            "X = \"1\"\n",
        );
        write(
            &root.join("meta-vendor/classes/base.bbclass"),
            "Y = \"1\"\n",
        );
    });
    indexed(ws, Some(tmp))
}

#[test]
fn kirkstone_split_class_dirs_are_indexed_but_never_candidates() {
    let fx = kirkstone_with_duplicates();
    // Still indexed.
    let auto = id(&fx, "meta-core", "classes-recipe/autotoolsish.bbclass");
    id(&fx, "meta-core", "classes-global/sanityish.bbclass");
    // Listed by class_files, but never ranked, for inherit or INHERIT.
    for global in [false, true] {
        for name in ["autotoolsish", "sanityish"] {
            let files = query::class_files(&fx.idx, &fx.ws, name, global).unwrap();
            assert!(!files.is_empty(), "{name}");
            assert!(files.iter().all(|c| c.rank.is_none()), "{name}: {files:?}");
        }
        let base = query::class_files(&fx.idx, &fx.ws, "base", global).unwrap();
        assert_eq!(base.iter().filter(|c| c.rank.is_some()).count(), 2);
    }
    // `inherit autotoolsish` in hello.inc does not resolve; nothing points at the class.
    let inc = id(&fx, "meta-core", "recipes-core/hello/hello.inc");
    let edge = query::edges_from(&fx.idx, inc)
        .unwrap()
        .into_iter()
        .find(|e| e.kind == "inherit")
        .unwrap();
    assert_eq!(edge.target_text, "autotoolsish");
    assert_eq!(edge.target, None);
    assert!(query::edges_to(&fx.idx, auto).unwrap().is_empty());
    assert!(
        query::inheritors(&fx.idx, "autotoolsish", true)
            .unwrap()
            .is_empty()
    );
    // Shadowing: base (classes/) is reported, autotoolsish (classes-recipe/) is not.
    let got: Vec<(String, Vec<String>)> = query::shadowed(&fx.idx, &fx.ws)
        .unwrap()
        .iter()
        .map(|s| (s.name.clone(), s.files.iter().map(lp).collect()))
        .collect();
    assert_eq!(
        got,
        [(
            "base.bbclass".to_string(),
            vec![
                "meta-core:classes/base.bbclass".to_string(),
                "meta-vendor:classes/base.bbclass".to_string(),
            ]
        )]
    );
}

#[test]
fn split_class_dirs_are_candidates_from_langdale() {
    // The same files on the default release: both duplicates are shadowing groups and the
    // inherit resolves.
    let (tmp, _root, _cfg, ws) = temp_ws_release("scarthgap", |root| {
        write(
            &root.join("meta-vendor/classes-recipe/autotoolsish.bbclass"),
            "X = \"1\"\n",
        );
    });
    let fx = indexed(ws, Some(tmp));
    let names: Vec<String> = query::shadowed(&fx.idx, &fx.ws)
        .unwrap()
        .iter()
        .map(|s| s.name.clone())
        .collect();
    assert_eq!(names, ["autotoolsish.bbclass"]);
    let inc = id(&fx, "meta-core", "recipes-core/hello/hello.inc");
    let edge = query::edges_from(&fx.idx, inc)
        .unwrap()
        .into_iter()
        .find(|e| e.kind == "inherit")
        .unwrap();
    assert_eq!(
        edge.target.map(|t| lp(&t)).as_deref(),
        Some("meta-core:classes-recipe/autotoolsish.bbclass")
    );
}
