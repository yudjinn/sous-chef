//! An `inherit` inside a class: which class directories the index searches for it.
//!
//! BitBake searches `classes-<__bbclasstype>/` then `classes/`, where `__bbclasstype` is
//! `global` while the configuration is parsed (`INHERIT`) and `recipe` afterwards
//! (bitbake-semantics.md, "Finding files"). A class in `classes/` can be parsed in either,
//! so its `inherit`s are resolved in both and the edge records which search found the
//! target; a class in `classes-recipe/` is recipe-only and one in `classes-global/`
//! global-only.

mod common;

use std::fs;
use std::path::Path;

use common::*;
use sous_chef::index::{Index, query};

/// `(target_text, target path, context)` of the `inherit` edges out of a `meta-core` file.
fn inherit_edges(index: &Index, path: &str) -> Vec<(String, Option<String>, Option<String>)> {
    let f = query::file_by_path(index, "meta-core", Path::new(path))
        .unwrap()
        .unwrap_or_else(|| panic!("{path} is indexed"));
    query::edges_from(index, f.id)
        .unwrap()
        .into_iter()
        .filter(|e| e.kind == "inherit")
        .map(|e| {
            (
                e.target_text,
                e.target.map(|t| t.path.to_string_lossy().into_owned()),
                e.context,
            )
        })
        .collect()
}

fn add_classes(root: &Path) {
    let core = root.join("meta-core");
    let w = |rel: &str, text: &str| {
        let p = core.join(rel);
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        fs::write(p, text).unwrap();
    };
    // `sanityish` is only in `classes-global/`, `autotoolsish` only in `classes-recipe/`,
    // `base` only in `classes/` (the fixture workspace).
    w("classes/signish.bbclass", "inherit sanityish\n");
    w("classes/shared.bbclass", "inherit base\n");
    w(
        "classes/mixed.bbclass",
        "inherit autotoolsish sanityish nosuchclass\n",
    );
    w("classes/variable.bbclass", "inherit ${SOMECLASS}\n");
    w("classes-recipe/recipeonly.bbclass", "inherit sanityish\n");
    w(
        "classes-global/globalonly.bbclass",
        "inherit autotoolsish\n",
    );
    // The same name in both context directories: each context finds its own.
    w("classes-recipe/dual.bbclass", "A = \"recipe\"\n");
    w("classes-global/dual.bbclass", "A = \"global\"\n");
    w("classes/usesdual.bbclass", "inherit dual\n");
}

fn s(x: &str) -> String {
    x.to_string()
}

#[test]
fn an_inherit_in_a_shared_class_is_resolved_in_both_contexts() {
    let (_tmp, _root, _cfg, ws) = temp_ws_release("wrynose", add_classes);
    let cache = tempfile::tempdir().unwrap();
    let mut index = Index::open_in(cache.path(), &ws).unwrap();
    index.refresh(&ws).unwrap();

    // Found only by the global search: resolved (OE-core's `sign_rpm` `inherit sanity`).
    assert_eq!(
        inherit_edges(&index, "classes/signish.bbclass"),
        [(
            s("sanityish"),
            Some(s("classes-global/sanityish.bbclass")),
            Some(s("global"))
        )]
    );
    // Both searches end in `classes/`.
    assert_eq!(
        inherit_edges(&index, "classes/shared.bbclass"),
        [(s("base"), Some(s("classes/base.bbclass")), Some(s("both")))]
    );
    assert_eq!(
        inherit_edges(&index, "classes/mixed.bbclass"),
        [
            (
                s("autotoolsish"),
                Some(s("classes-recipe/autotoolsish.bbclass")),
                Some(s("recipe"))
            ),
            (
                s("sanityish"),
                Some(s("classes-global/sanityish.bbclass")),
                Some(s("global"))
            ),
            (s("nosuchclass"), None, None),
        ]
    );
    // Needs evaluation: one unresolved edge, as for any other file.
    assert_eq!(
        inherit_edges(&index, "classes/variable.bbclass"),
        [(s("${SOMECLASS}"), None, None)]
    );
    // Different files in the two contexts: one edge each.
    assert_eq!(
        inherit_edges(&index, "classes/usesdual.bbclass"),
        [
            (
                s("dual"),
                Some(s("classes-recipe/dual.bbclass")),
                Some(s("recipe"))
            ),
            (
                s("dual"),
                Some(s("classes-global/dual.bbclass")),
                Some(s("global"))
            ),
        ]
    );
    // A context class searches its own context only, and records none.
    assert_eq!(
        inherit_edges(&index, "classes-recipe/recipeonly.bbclass"),
        [(s("sanityish"), None, None)]
    );
    assert_eq!(
        inherit_edges(&index, "classes-global/globalonly.bbclass"),
        [(s("autotoolsish"), None, None)]
    );
    // A recipe is still recipe-only, with no context recorded.
    let libfoo = inherit_edges(&index, "recipes-core/libfoo/libfoo_2.0.bb");
    assert!(libfoo.iter().all(|e| e.2.is_none()), "{libfoo:?}");

    let unresolved: Vec<(String, String)> = query::unresolved_inherit_edges(&index)
        .unwrap()
        .into_iter()
        .filter(|e| e.src.layer == "meta-core")
        .map(|e| (e.src.path.to_string_lossy().into_owned(), e.target_text))
        .collect();
    for (path, text) in [
        ("classes/mixed.bbclass", "nosuchclass"),
        ("classes-recipe/recipeonly.bbclass", "sanityish"),
        ("classes-global/globalonly.bbclass", "autotoolsish"),
    ] {
        assert!(
            unresolved.contains(&(s(path), s(text))),
            "{path} {text}: {unresolved:?}"
        );
    }
    assert!(
        !unresolved
            .iter()
            .any(|(p, _)| p == "classes/signish.bbclass" || p == "classes/usesdual.bbclass"),
        "{unresolved:?}"
    );
}

#[test]
fn before_split_class_directories_only_classes_is_searched() {
    // Kirkstone (BitBake 2.0) searches `classes/` alone and has no class contexts.
    let (_tmp, _root, _cfg, ws) = temp_ws_release("kirkstone", add_classes);
    let cache = tempfile::tempdir().unwrap();
    let mut index = Index::open_in(cache.path(), &ws).unwrap();
    index.refresh(&ws).unwrap();
    assert_eq!(
        inherit_edges(&index, "classes/signish.bbclass"),
        [(s("sanityish"), None, None)]
    );
    assert_eq!(
        inherit_edges(&index, "classes/shared.bbclass"),
        [(s("base"), Some(s("classes/base.bbclass")), None)]
    );
}

/// `.inc` files reached from global-context files, recipe-context files, both and neither.
fn add_incs(root: &Path) {
    let core = root.join("meta-core");
    let w = |rel: &str, text: &str| {
        let p = core.join(rel);
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        fs::write(p, text).unwrap();
    };
    // Global: a `.conf`, through a chain of two `.inc` files.
    w(
        "conf/gl.conf",
        "require conf/gl.inc\nrequire conf/both.inc\n",
    );
    w("conf/gl.inc", "require conf/gl2.inc\n");
    w("conf/gl2.inc", "inherit sanityish autotoolsish\n");
    // Global: a class in `classes-global/`.
    w("classes-global/gclass.bbclass", "require conf/gc.inc\n");
    w("conf/gc.inc", "inherit sanityish\n");
    // Recipe: a recipe.
    w(
        "recipes-core/incy/incy_1.0.bb",
        "require incy.inc\nrequire conf/both.inc\n",
    );
    w(
        "recipes-core/incy/incy.inc",
        "inherit sanityish autotoolsish\n",
    );
    // Both: the `.conf` and the recipe above.
    w("conf/both.inc", "inherit sanityish autotoolsish\n");
    // Through a class in `classes/`: both.
    w("classes/sharedinc.bbclass", "require conf/sh.inc\n");
    w("conf/sh.inc", "inherit base\n");
    // Nothing reaches it: recipe.
    w(
        "recipes-core/incy/lonely.inc",
        "inherit autotoolsish sanityish\n",
    );
    // `inherit` in a `.conf`: BitBake's configuration parser rejects the line.
    w("conf/bad.conf", "inherit sanityish\n");
}

#[test]
fn an_inherit_in_an_inc_follows_what_reaches_it() {
    let (_tmp, root, _cfg, ws) = temp_ws_release("wrynose", add_incs);
    let cache = tempfile::tempdir().unwrap();
    let mut index = Index::open_in(cache.path(), &ws).unwrap();
    index.refresh(&ws).unwrap();

    let global = [
        (
            s("sanityish"),
            Some(s("classes-global/sanityish.bbclass")),
            None,
        ),
        (s("autotoolsish"), None, None),
    ];
    assert_eq!(inherit_edges(&index, "conf/gl2.inc"), global);
    assert_eq!(
        inherit_edges(&index, "conf/gc.inc"),
        [(
            s("sanityish"),
            Some(s("classes-global/sanityish.bbclass")),
            None
        )]
    );
    assert_eq!(
        inherit_edges(&index, "recipes-core/incy/incy.inc"),
        [
            (s("sanityish"), None, None),
            (
                s("autotoolsish"),
                Some(s("classes-recipe/autotoolsish.bbclass")),
                None
            ),
        ]
    );
    assert_eq!(
        inherit_edges(&index, "conf/both.inc"),
        [
            (
                s("sanityish"),
                Some(s("classes-global/sanityish.bbclass")),
                Some(s("global"))
            ),
            (
                s("autotoolsish"),
                Some(s("classes-recipe/autotoolsish.bbclass")),
                Some(s("recipe"))
            ),
        ]
    );
    assert_eq!(
        inherit_edges(&index, "conf/sh.inc"),
        [(s("base"), Some(s("classes/base.bbclass")), Some(s("both")))]
    );
    assert_eq!(
        inherit_edges(&index, "recipes-core/incy/lonely.inc"),
        [
            (
                s("autotoolsish"),
                Some(s("classes-recipe/autotoolsish.bbclass")),
                None
            ),
            (s("sanityish"), None, None),
        ]
    );
    // No edge for an `inherit` in a `.conf`, resolved or not.
    assert_eq!(inherit_edges(&index, "conf/bad.conf"), []);

    // Reaching an `.inc` from a `.conf` later changes its edges with one file's refresh.
    let gl = root.join("meta-core/conf/gl.conf");
    fs::write(
        &gl,
        "require conf/gl.inc\nrequire conf/both.inc\nrequire recipes-core/incy/lonely.inc\n",
    )
    .unwrap();
    index.refresh_file(&ws, &gl).unwrap();
    assert_eq!(
        inherit_edges(&index, "recipes-core/incy/lonely.inc"),
        [
            (s("autotoolsish"), None, None),
            (
                s("sanityish"),
                Some(s("classes-global/sanityish.bbclass")),
                None
            ),
        ]
    );
    // Dropping the recipe's reach makes `both.inc` global only.
    let incy = root.join("meta-core/recipes-core/incy/incy_1.0.bb");
    fs::write(&incy, "require incy.inc\n").unwrap();
    index.refresh_file(&ws, &incy).unwrap();
    assert_eq!(inherit_edges(&index, "conf/both.inc"), global);
}

#[test]
fn an_inherit_in_a_conf_is_one_parse_error_and_no_unresolved_edge() {
    let (_tmp, root, cfg, ws) = temp_ws_release("wrynose", add_incs);
    let cache = tempfile::tempdir().unwrap();
    let mut index = Index::open_in(cache.path(), &ws).unwrap();
    index.refresh(&ws).unwrap();
    let report = sous_chef::lint::run(
        &ws,
        &index,
        &cfg,
        &sous_chef::lint::Target::Files {
            files: vec![root.join("meta-core/conf/bad.conf")],
            lines: None,
        },
        &sous_chef::lint::Options::default(),
    )
    .unwrap();
    let parse: Vec<_> = report
        .findings
        .iter()
        .filter(|f| f.rule == "hygiene/parse-error")
        .collect();
    assert_eq!(parse.len(), 1, "{:?}", report.findings);
    assert!(
        parse[0].message.starts_with("`inherit` in a .conf file"),
        "{}",
        parse[0].message
    );
    // The audit's unresolved-inherit listing reads the edges: nothing for this line.
    assert!(
        !query::unresolved_inherit_edges(&index)
            .unwrap()
            .iter()
            .any(|e| e.src.path == Path::new("conf/bad.conf")),
    );
}
