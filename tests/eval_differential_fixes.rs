//! Fixes for the unflagged wrong answers the differential test against BitBake found
//! (docs/differential.md, "Bugs"). Each test runs on a temporary copy of the fixture
//! workspace; the fixture itself is not changed.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::fs;
use std::path::{Path, PathBuf};

use common::*;
use sous_chef::eval::{self, ConfigCache, DataStore, Target};
use sous_chef::index::Index;
use sous_chef::index::query;
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
