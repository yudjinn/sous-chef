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
