//! y3-leftovers, item 1: `deps` shows what made the real build of curl build what it built
//! (docs/live-verification.md, "y3-leftovers"): runtime recommendations (`RRECOMMENDS`,
//! which BitBake builds for as for `RDEPENDS`: `bb.cache` `rundeps` / `runrecs`,
//! `bb.taskdata` `add_tasks`), the `depends` flags of the recipe's tasks, and a native
//! variant's `DEPENDS` as `native.bbclass`'s `native_filter` makes BitBake read it. The
//! curl lines are those of OE-core Wrynose `curl_8.19.0.bb` (`RRECOMMENDS:lib${BPN} +=
//! "ca-certificates"`, line 200 there), on the fixture workspace.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::fs;
use std::path::Path;

use sous_chef::bitbake::JobTable;
use sous_chef::handles::HandleStore;
use sous_chef::index::Index;
use sous_chef::mcp::Ctx;
use sous_chef::mcp::tools::deps::{self, Kind};
use sous_chef::workspace::Workspace;

fn append_to(root: &Path, rel: &str, text: &str) {
    let p = root.join(rel);
    let mut s = fs::read_to_string(&p).unwrap();
    s.push_str(text);
    fs::write(p, s).unwrap();
}

fn write(root: &Path, rel: &str, text: &str) {
    let p = root.join(rel);
    fs::create_dir_all(p.parent().unwrap()).unwrap();
    fs::write(p, text).unwrap();
}

struct Fx {
    _tmp: tempfile::TempDir,
    _cache: tempfile::TempDir,
    ctx: Ctx,
}

fn fixture(edit: impl FnOnce(&Path)) -> Fx {
    let (tmp, root) = common::temp_ws();
    let root = fs::canonicalize(root).unwrap();
    edit(&root);
    let config = common::ws_config(&root);
    let workspace = Workspace::discover(&config).unwrap();
    let cache = tempfile::tempdir().unwrap();
    let mut index = Index::open_in(cache.path(), &workspace).unwrap();
    index.refresh(&workspace).unwrap();
    let handles = HandleStore::open(&index.cache_dir().join("handles")).unwrap();
    Fx {
        _tmp: tmp,
        _cache: cache,
        ctx: Ctx {
            config,
            workspace,
            index: tokio::sync::Mutex::new(index),
            handles,
            jobs: JobTable::new(),
        },
    }
}

async fn deps_of(fx: &Fx, recipe: &str, kind: Kind) -> String {
    deps::run(
        &fx.ctx,
        deps::Args {
            recipe: recipe.into(),
            kind,
            mc: None,
            depth: None,
            budget_tokens: Some(4000),
            format: None,
        },
    )
    .await
    .unwrap()
}

/// libfoo as curl: `RRECOMMENDS:lib${BPN}`-style recommendations, a recommendation from
/// `PACKAGECONFIG` field 5, a plain `RDEPENDS` (every package) and an `RSUGGESTS` (builds
/// nothing).
fn recommends(root: &Path) {
    append_to(
        root,
        "meta-core/recipes-core/libfoo/libfoo_2.0.bb",
        "PACKAGES =+ \"lib${BPN}\"\n\
         RRECOMMENDS:lib${BPN} += \"ca-certificates\"\n\
         RRECOMMENDS:${PN}-dev = \"${PN}-doc (>= 1.0)\"\n\
         RDEPENDS = \"base-files\"\n\
         RSUGGESTS:${PN} = \"never-built\"\n\
         PACKAGECONFIG[zlib] = \"--with-zlib,--without-zlib,zlib,zlib,zlib-extra\"\n",
    );
}

#[tokio::test]
async fn runtime_lists_recommendations_as_recommendations() {
    let fx = fixture(recommends);
    let out = deps_of(&fx, "libfoo", Kind::Runtime).await;
    assert!(
        out.starts_with("package\tdep\trelation\tsource\tloc\n"),
        "{out}"
    );
    let row = |pkg: &str, dep: &str, rel: &str, source: &str| {
        format!("{pkg}\t{dep}\t{rel}\t{source}\tmeta-core:recipes-core/libfoo/libfoo_2.0.bb:")
    };
    for want in [
        row(
            "liblibfoo",
            "ca-certificates",
            "recommends",
            "RRECOMMENDS:liblibfoo",
        ),
        row(
            "libfoo-dev",
            "libfoo-doc",
            "recommends",
            "RRECOMMENDS:libfoo-dev",
        ),
        row("libfoo-dev", "libfoo", "depends", "RDEPENDS:libfoo-dev"),
        row("(every package)", "base-files", "depends", "RDEPENDS"),
        row("libfoo", "zlib", "depends", "PACKAGECONFIG[zlib]"),
        row("libfoo", "zlib-extra", "recommends", "PACKAGECONFIG[zlib]"),
    ] {
        assert!(out.contains(&want), "{want:?} not in:\n{out}");
    }
    assert!(
        !out.contains("never-built"),
        "RSUGGESTS builds nothing: {out}"
    );
    assert!(
        out.contains("note: relation recommends (RRECOMMENDS): BitBake builds its provider"),
        "{out}"
    );
}

#[tokio::test]
async fn build_lists_the_recipes_named_by_task_depends_flags() {
    let fx = fixture(|root| {
        append_to(
            root,
            "meta-core/recipes-core/hello/hello.inc",
            "do_unpack[depends] += \"xz-native:do_populate_sysroot\"\n\
             do_install[depends] += \"virtual/foo:do_populate_sysroot\"\n\
             do_compile[depends] += \"virtual/foo:do_populate_sysroot ${PN}:do_configure\"\n\
             do_not_a_task[depends] = \"never-seen:do_x\"\n",
        );
    });
    let out = deps_of(&fx, "hello", Kind::Build).await;
    // virtual/foo is in DEPENDS already: one row, from DEPENDS.
    assert_eq!(out.matches("\tvirtual/foo\t").count(), 1, "{out}");
    assert!(
        out.contains("1\txz-native\t-\tdo_unpack[depends] (do_populate_sysroot)\tmeta-core:recipes-core/hello/hello.inc:"),
        "{out}"
    );
    assert!(!out.contains("never-seen"), "not a task: {out}");
    assert!(!out.contains("\thello\t"), "its own tasks: {out}");
    assert!(out.contains("note: do_<task>[depends] rows"), "{out}");
}

/// A `BBCLASSEXTEND = "native"` recipe: BitBake reads the variant's `DEPENDS` through
/// `native_filter(..., selfref=False)` (OE-core `native.bbclass`), so `zlib` is
/// `zlib-native`, a `-native` word stays, a `-cross-` word becomes `-native` and the
/// recipe's own name is dropped. (acc-helpers: the evaluator installs the filter, only for
/// OE-core's own handler text, `tests/fixtures/classfn/native-handler.bbclass`; `deps` maps
/// the words through it.)
#[tokio::test]
async fn a_native_variant_reads_depends_through_native_filter() {
    let fx = fixture(|root| {
        let handler =
            fs::read_to_string(common::fixture_path("classfn/native-handler.bbclass")).unwrap();
        write(
            root,
            "meta-core/classes-recipe/native.bbclass",
            &format!("CLASSOVERRIDE = \"class-native\"\nMACHINEOVERRIDES = \"\"\n{handler}"),
        );
        write(
            root,
            "meta-core/recipes-core/tool/tool_1.0.bb",
            "LICENSE = \"MIT\"\n\
             DEPENDS = \"zlib pkgconf-native binutils-cross-arm tool-native\"\n\
             BBCLASSEXTEND = \"native\"\n",
        );
    });
    let out = deps_of(&fx, "tool-native", Kind::Build).await;
    // DEPENDS is certain, so the evaluator's value is already filtered. handleVirtRecipeProviders
    // stores the filtered words and the next read filters them again, as in BitBake:
    // binutils-cross-arm, then binutils-native-arm, then binutils-native-arm-native.
    for (dep, source) in [
        ("zlib-native", "DEPENDS"),
        ("pkgconf-native", "DEPENDS"),
        ("binutils-native-arm-native", "DEPENDS"),
    ] {
        assert!(
            out.lines()
                .any(|l| l.starts_with(&format!("1\t{dep}\t"))
                    && l.contains(&format!("\t{source}\t"))),
            "{dep} {source} not in:\n{out}"
        );
    }
    assert!(!out.contains("\ttool-native\t"), "{out}");
    assert!(out.contains("note: native_filter: "), "{out}");
    // The target recipe reads its DEPENDS as written.
    let out = deps_of(&fx, "tool", Kind::Build).await;
    assert!(out.contains("1\tzlib\t"), "{out}");
    assert!(!out.contains("native_filter"), "{out}");
}
