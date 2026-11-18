//! The `pkgdata` tool on copies of the fixture workspace holding a recorded package data
//! tree, `tests/fixtures/outputs/pkgdata/qemux86/`. The tree is hand-written in the format
//! OE-core's `emit_pkgdata` writes (`meta/lib/oe/packagedata.py`, Wrynose; the same in
//! Kirkstone's `package.bbclass`): values through Python's `unicode_escape` encoder,
//! `FILES_INFO` as `json.dumps(..., sort_keys=True)`, `runtime-reverse/` and
//! `runtime-rprovides/` symlinks. It has an escaped file name (`café "notes".txt` in
//! `hello-doc`), a renamed package (`libfoo` is `libfoo2` at runtime) with an `RPROVIDES`,
//! empty packages without `.packaged`, and a package with more files than one answer shows.
//! Not from a build: the live build directory has no package data.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::fs;
use std::path::Path;

use sous_chef::bitbake::JobTable;
use sous_chef::handles::HandleStore;
use sous_chef::index::Index;
use sous_chef::mcp::Ctx;
use sous_chef::mcp::tools::pkgdata::{self, Args, Mode};
use sous_chef::workspace::Workspace;

/// Copies a directory, symlinks as symlinks.
fn copy_tree(from: &Path, to: &Path) {
    fs::create_dir_all(to).unwrap();
    for e in fs::read_dir(from).unwrap() {
        let e = e.unwrap();
        let ty = e.file_type().unwrap();
        let dst = to.join(e.file_name());
        if ty.is_symlink() {
            std::os::unix::fs::symlink(fs::read_link(e.path()).unwrap(), &dst).unwrap();
        } else if ty.is_dir() {
            copy_tree(&e.path(), &dst);
        } else {
            fs::copy(e.path(), &dst).unwrap();
        }
    }
}

struct Fx {
    _cache: tempfile::TempDir,
    _tmp: tempfile::TempDir,
    root: String,
    ctx: Ctx,
}

/// A fixture copy; `pkgdata` copies the recorded tree into `build/tmp/pkgdata/qemux86`;
/// `conf` appends `PKGDATA_DIR` to the copy's `bitbake.conf`.
fn fx(pkgdata: bool, conf: bool) -> Fx {
    let (tmp, root) = common::temp_ws();
    let root = fs::canonicalize(root).unwrap();
    if pkgdata {
        copy_tree(
            &common::fixture_path("outputs/pkgdata/qemux86"),
            &root.join("build/tmp/pkgdata/qemux86"),
        );
    }
    if conf {
        let p = root.join("meta-core/conf/bitbake.conf");
        let mut s = fs::read_to_string(&p).unwrap();
        s.push_str("PKGDATA_DIR = \"${TMPDIR}/pkgdata/${MACHINE}\"\n");
        fs::write(&p, s).unwrap();
    }
    let config = common::ws_config(&root);
    let cache = tempfile::tempdir().unwrap();
    let workspace = Workspace::discover(&config).unwrap();
    let mut index = Index::open_in(cache.path(), &workspace).unwrap();
    index.refresh(&workspace).unwrap();
    let handles = HandleStore::open(&index.cache_dir().join("handles")).unwrap();
    Fx {
        _cache: cache,
        _tmp: tmp,
        root: root.display().to_string(),
        ctx: Ctx {
            config,
            workspace,
            index: tokio::sync::Mutex::new(index),
            handles,
            jobs: JobTable::new(),
        },
    }
}

fn args(mode: Mode) -> Args {
    Args {
        mode,
        path: None,
        package: None,
        recipe: None,
        budget_tokens: Some(3000),
        format: None,
    }
}

async fn call(f: &Fx, a: Args) -> String {
    let out = pkgdata::run(&f.ctx, a).await.unwrap();
    let re = regex::Regex::new(r"h:[0-9a-f]{10}").unwrap();
    re.replace_all(&out.replace(&f.root, "<ws>"), "h:HANDLE")
        .into_owned()
}

#[tokio::test]
async fn find_path_names_the_recipe_and_its_files_assignment() {
    let f = fx(true, true);
    let mut a = args(Mode::FindPath);
    a.path = Some("/usr/bin/hello".into());
    let out = call(&f, a).await;
    insta::assert_snapshot!(out);
    assert!(
        out.contains("/usr/bin/hello\thello\thello\tFILES:hello meta-core:conf/bitbake.conf:27, meta-core:recipes-core/hello/hello.inc:20"),
        "{out}"
    );
    assert!(
        out.contains(
            "note: PKGDATA_DIR = <ws>/build/tmp/pkgdata/qemux86 (from the evaluated configuration)"
        ),
        "{out}"
    );
}

#[tokio::test]
async fn find_path_glob_escaped_name_and_renamed_package() {
    let f = fx(true, false);
    let mut a = args(Mode::FindPath);
    a.path = Some("*caf*".into());
    let out = call(&f, a).await;
    assert!(
        out.contains("/usr/share/doc/hello/café \"notes\".txt\thello-doc\thello\t"),
        "{out}"
    );
    // Without PKGDATA_DIR in the configuration the build directory is searched.
    assert!(
        out.contains("note: package data found by searching the build directory: build:tmp/pkgdata/qemux86 (PKGDATA_DIR is not set, or not static, in the configuration; machine qemux86)"),
        "{out}"
    );
    let mut a = args(Mode::FindPath);
    a.path = Some("/usr/lib/libfoo.so*".into());
    let out = call(&f, a).await;
    assert!(
        out.contains("/usr/lib/libfoo.so.2\tlibfoo (runtime name libfoo2)\tlibfoo\t"),
        "{out}"
    );
    assert!(
        out.contains("/usr/lib/libfoo.so\tlibfoo-dev\tlibfoo\t"),
        "{out}"
    );
}

#[tokio::test]
async fn list_pkg_files_caps_and_stores_the_rest() {
    let f = fx(true, false);
    let mut a = args(Mode::ListPkgFiles);
    a.package = Some("libfoo2".into());
    let out = call(&f, a).await;
    insta::assert_snapshot!(out);
    assert!(out.contains("+7 more (limit=40)"), "{out}");
    assert!(
        out.contains("note: all 47 rows are stored under h:HANDLE"),
        "{out}"
    );
    assert!(
        out.contains("note: libfoo2 is the runtime name of the recipe-space package libfoo (PKG:libfoo = libfoo2)"),
        "{out}"
    );
}

#[tokio::test]
async fn lookup_recipe_through_rprovides_and_runtime_names() {
    let f = fx(true, false);
    let mut a = args(Mode::LookupRecipe);
    a.package = Some("libfoo-compat".into());
    let out = call(&f, a).await;
    assert!(
        out.contains("libfoo\tlibfoo\t2.0-r0\tmeta-core:recipes-core/libfoo/libfoo_2.0.bb"),
        "{out}"
    );
    assert!(
        out.contains("note: libfoo-compat is in the RPROVIDES of libfoo"),
        "{out}"
    );
    let mut a = args(Mode::LookupRecipe);
    a.package = Some("hello".into());
    let out = call(&f, a).await;
    assert!(
        out.contains("hello\thello\t1.1-r0\tmeta-core:recipes-core/hello/hello_1.1.bb"),
        "{out}"
    );
    let mut a = args(Mode::LookupRecipe);
    a.package = Some("nosuch".into());
    let e = pkgdata::run(&f.ctx, a).await.unwrap_err().to_string();
    assert!(e.contains("a built package named nosuch"), "{e}");
}

#[tokio::test]
async fn list_pkgs_of_a_recipe() {
    let f = fx(true, false);
    let mut a = args(Mode::ListPkgs);
    a.recipe = Some("hello".into());
    let out = call(&f, a).await;
    insta::assert_snapshot!(out);
    assert!(out.contains("hello-dev\thello-dev\tno (empty)\t0"), "{out}");
}

#[tokio::test]
async fn package_info_with_renamed_package() {
    let f = fx(true, false);
    let mut a = args(Mode::PackageInfo);
    a.package = Some("libfoo2".into());
    let out = call(&f, a).await;
    insta::assert_snapshot!(out);
    assert!(out.contains("libfoo\truntime name\tlibfoo2\t-"), "{out}");
    assert!(out.contains("libfoo\tRPROVIDES\tlibfoo-compat\t-"), "{out}");
    // The escaped newline in SUMMARY is decoded (and the cell escaped again for the table).
    assert!(out.contains("libfoo\tSUMMARY\tFoo library"), "{out}");
}

#[tokio::test]
async fn no_package_data() {
    let f = fx(false, true);
    let mut a = args(Mode::FindPath);
    a.path = Some("/usr/bin/hello".into());
    let out = call(&f, a).await;
    assert!(
        out.contains("pkgdata\tnone: nothing has been packaged in this build directory"),
        "{out}"
    );
    assert!(out.contains("written by do_packagedata"), "{out}");
    assert!(
        out.contains("note: PKGDATA_DIR = <ws>/build/tmp/pkgdata/qemux86 does not exist yet"),
        "{out}"
    );
}

#[tokio::test]
async fn arguments_are_checked() {
    let f = fx(true, false);
    let a = args(Mode::FindPath);
    let e = pkgdata::run(&f.ctx, a).await.unwrap_err().to_string();
    assert!(e.contains("mode=find_path needs path"), "{e}");
    let mut a = args(Mode::PackageInfo);
    a.package = Some("../../etc/passwd".into());
    assert!(pkgdata::run(&f.ctx, a).await.is_err());
    // Kebab-case modes as oe-pkgdata-util spells them are accepted.
    let v: Args =
        serde_json::from_value(serde_json::json!({"mode": "find-path", "path": "/x"})).unwrap();
    assert_eq!(v.mode, Mode::FindPath);
}
