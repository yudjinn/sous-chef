//! T8.4: the release diff, on a small git repository built here (two commits, a tag, fixed
//! dates so commit hashes are stable) holding an upstream layer, and a writable layer
//! beside it with bbappends. Snapshots of the text and JSON forms, the default selection,
//! `recipe` and `all`, the working tree as `to`, revision validation, the repository left
//! untouched, and the handle for a diff over the budget.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use sous_chef::bitbake::JobTable;
use sous_chef::config::Config;
use sous_chef::diff::{self, Options, Severity};
use sous_chef::handles::{Handle, HandleStore};
use sous_chef::index::Index;
use sous_chef::mcp::Ctx;
use sous_chef::mcp::tools::{diff as diff_tool, slice};
use sous_chef::render::{self, Format, JsonTable};
use sous_chef::workspace::Workspace;
use sous_chef::workspace::kas::KasEnv;

/// Runs git for the test: argv, no user configuration, no hooks, fixed dates.
fn git(dir: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .args([
            // Read-only commands (status on the live checkout) never refresh the index.
            "--no-optional-locks",
            "-c",
            "user.name=test",
            "-c",
            "user.email=test@example.com",
            "-c",
            "commit.gpgsign=false",
            "-c",
            "tag.gpgsign=false",
            "-c",
            "core.hooksPath=/dev/null",
            "-c",
            "init.defaultBranch=main",
        ])
        .args(args)
        .current_dir(dir)
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_INDEX_FILE")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_AUTHOR_DATE", "2026-01-01T00:00:00Z")
        .env("GIT_COMMITTER_DATE", "2026-01-01T00:00:00Z")
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stdout).unwrap()
}

fn write(path: &Path, text: &str) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, text).unwrap();
}

fn layer_conf(collection: &str, priority: u32) -> String {
    format!(
        "BBPATH .= \":${{LAYERDIR}}\"\n\
         BBFILES += \"${{LAYERDIR}}/recipes-*/*/*.bb ${{LAYERDIR}}/recipes-*/*/*.bbappend\"\n\
         BBFILE_COLLECTIONS += \"{collection}\"\n\
         BBFILE_PATTERN_{collection} = \"^${{LAYERDIR}}/\"\n\
         BBFILE_PRIORITY_{collection} = \"{priority}\"\n\
         LAYERSERIES_COMPAT_{collection} = \"wrynose\"\n"
    )
}

const HELLO_V1: &str = r#"SUMMARY = "Hello world"
LICENSE = "MIT"

require hello.inc
require ${BPN}-common.inc
require recipes-core/shared/shared.inc
include conf/distro/elsewhere.inc

SRC_URI = "https://example.org/hello-${PV}.tar.gz \
           file://keep.patch \
           "
SRC_URI[sha256sum] = "aaaa"
EXTRA_OECONF = "--enable-a --enable-b"

inherit autotools

do_install() {
    install -d ${D}${bindir}
    install -m 0755 hello ${D}${bindir}
}

addtask do_check after do_compile before do_install
"#;

const HELLO_V2: &str = r#"SUMMARY = "Hello, world"
LICENSE = "MIT"

require hello.inc
require ${BPN}-common.inc
require recipes-core/shared/shared.inc
include conf/distro/elsewhere.inc

SRC_URI = "https://example.org/hello-${PV}.tar.gz \
           file://keep.patch \
           file://fix.patch \
           file://${@bb.utils.contains('DISTRO_FEATURES', 'x11', 'x.patch', '', d)} \
           "
SRC_URI[sha256sum] = "bbbb"
EXTRA_OECONF = "--enable-a   --enable-b"

inherit autotools pkgconfig

do_install() {
    # install the binary
    install -d ${D}${bindir}
    install -m 0755 hello ${D}${bindir}/hello
}

addtask do_check after do_compile before do_install
"#;

/// The workspace: `upstream/` is a git repository holding `meta-up`; `meta-mine` (writable)
/// is beside it, outside any repository. Returns the temp dir and the workspace root.
fn build_repo() -> (tempfile::TempDir, PathBuf) {
    let tmp = tempfile::tempdir().unwrap();
    let root = fs::canonicalize(tmp.path()).unwrap().join("ws");
    let repo = root.join("upstream");
    let up = repo.join("meta-up");
    write(
        &root.join("build/conf/bblayers.conf"),
        "BBPATH = \"${TOPDIR}\"\nBBFILES ?= \"\"\nBBLAYERS ?= \" \\\n  ${TOPDIR}/../upstream/meta-up \\\n  ${TOPDIR}/../meta-mine \\\n  \"\n",
    );
    write(
        &root.join("build/conf/local.conf"),
        "MACHINE ?= \"qemux86\"\n",
    );

    // ---- commit 1, tag v1
    write(&up.join("conf/layer.conf"), &layer_conf("up", 5));
    write(&repo.join("README"), "upstream\n");
    write(&up.join("recipes-core/hello/hello_1.0.bb"), HELLO_V1);
    write(
        &up.join("recipes-core/hello/hello.inc"),
        "HOMEPAGE = \"https://example.org/hello\"\nDEPENDS = \"zlib\"\n",
    );
    write(
        &up.join("recipes-core/hello/hello-common.inc"),
        "# common settings\nBBCLASSEXTEND = \"native\"\n",
    );
    write(
        &up.join("recipes-core/hello/files/keep.patch"),
        "--- a\n+++ b\n",
    );
    write(
        &up.join("recipes-core/shared/shared.inc"),
        "SHARED_FLAGS = \"-O2\"\n",
    );
    write(
        &up.join("recipes-core/widget/widget_git.bb"),
        "SUMMARY = \"Widget\"\nLICENSE = \"MIT\"\nPV = \"1.0+git\"\n\
         SRC_URI = \"git://example.org/widget.git;branch=main;protocol=https\"\n\
         SRCREV = \"1111111111111111111111111111111111111111\"\n",
    );
    write(
        &up.join("recipes-core/stable/stable_1.0.bb"),
        "SUMMARY = \"Stable\"\nLICENSE = \"MIT\"\n",
    );
    write(
        &up.join("recipes-core/gone/gone_1.0.bb"),
        "SUMMARY = \"Removed later\"\nLICENSE = \"MIT\"\n",
    );
    write(
        &up.join("recipes-core/libfoo/libfoo_2.0.bb"),
        "SUMMARY = \"libfoo\"\nLICENSE = \"BSD-3-Clause\"\n",
    );
    write(
        &up.join("recipes-core/moved/moved_1.0.bb"),
        "SUMMARY = \"Moves to another directory\"\nLICENSE = \"MIT\"\n",
    );
    write(
        &up.join("classes/autotools.bbclass"),
        "autotools_do_configure() {\n    ./configure\n}\nEXPORT_FUNCTIONS do_configure\n",
    );
    write(
        &up.join("classes/pkgconfig.bbclass"),
        "DEPENDS:append = \" pkgconfig-native\"\n",
    );
    git(&repo, &["init", "-q"]);
    git(&repo, &["add", "-A"]);
    git(&repo, &["commit", "-q", "-m", "release 1"]);
    git(&repo, &["tag", "v1"]);

    // ---- commit 2
    git(
        &repo,
        &[
            "mv",
            "meta-up/recipes-core/hello/hello_1.0.bb",
            "meta-up/recipes-core/hello/hello_1.1.bb",
        ],
    );
    write(&up.join("recipes-core/hello/hello_1.1.bb"), HELLO_V2);
    write(
        &up.join("recipes-core/hello/hello.inc"),
        "HOMEPAGE = \"https://example.org/hello\"\nDEPENDS = \"zlib openssl\"\n",
    );
    write(
        &up.join("recipes-core/hello/hello-common.inc"),
        "# common settings, reworded\n\nBBCLASSEXTEND = \"native\"\n",
    );
    write(
        &up.join("recipes-core/hello/files/fix.patch"),
        "--- a\n+++ b\n",
    );
    write(
        &up.join("recipes-core/shared/shared.inc"),
        "SHARED_FLAGS = \"-O2 -g\"\n",
    );
    write(
        &up.join("recipes-core/widget/widget_git.bb"),
        "SUMMARY = \"Widget\"\nLICENSE = \"MIT\"\nPV = \"1.0+git\"\n\
         SRC_URI = \"git://example.org/widget.git;branch=main;protocol=https\"\n\
         SRCREV = \"2222222222222222222222222222222222222222\"\n",
    );
    git(
        &repo,
        &["rm", "-q", "meta-up/recipes-core/gone/gone_1.0.bb"],
    );
    write(
        &up.join("recipes-core/newone/newone_1.0.bb"),
        "SUMMARY = \"Added later\"\nLICENSE = \"MIT\"\n",
    );
    write(
        &up.join("recipes-core/libfoo/libfoo_2.0.bb"),
        "SUMMARY = \"libfoo\"\nLICENSE = \"BSD-3-Clause\"\nPACKAGECONFIG ??= \"ssl\"\n",
    );
    fs::create_dir_all(up.join("recipes-core/moved-elsewhere")).unwrap();
    git(
        &repo,
        &[
            "mv",
            "meta-up/recipes-core/moved/moved_1.0.bb",
            "meta-up/recipes-core/moved-elsewhere/moved_1.0.bb",
        ],
    );
    write(
        &up.join("classes/autotools.bbclass"),
        "autotools_do_configure() {\n    ./configure --disable-static\n}\nEXPORT_FUNCTIONS do_configure\n",
    );
    git(&repo, &["add", "-A"]);
    git(&repo, &["commit", "-q", "-m", "release 2"]);

    // ---- the writable layer (not in a repository)
    let mine = root.join("meta-mine");
    write(&mine.join("conf/layer.conf"), &layer_conf("mine", 10));
    write(
        &mine.join("recipes-core/hello/hello_1.0.bbappend"),
        "EXTRA_OECONF += \"--enable-mine\"\n",
    );
    write(
        &mine.join("recipes-core/hello/hello_%.bbappend"),
        "FILESEXTRAPATHS:prepend := \"${THISDIR}/${PN}:\"\n\
         SRC_URI += \"file://mine.patch file://fix.patch\"\n\
         do_install:append() {\n    rm -f ${D}${bindir}/extra\n}\n",
    );
    write(
        &mine.join("recipes-core/hello/hello/mine.patch"),
        "--- a\n+++ b\n",
    );
    write(
        &mine.join("recipes-core/hello/hello/fix.patch"),
        "--- a\n+++ b\n",
    );
    write(
        &mine.join("recipes-core/widget/widget_git.bbappend"),
        "FILESEXTRAPATHS:prepend := \"${THISDIR}/files:\"\nSRC_URI += \"file://widget-fix.patch\"\n",
    );
    write(
        &mine.join("recipes-core/widget/files/widget-fix.patch"),
        "--- a\n+++ b\n",
    );
    write(
        &mine.join("recipes-core/stable/stable_%.bbappend"),
        "LICENSE:append = \" & Apache-2.0\"\n",
    );
    write(
        &mine.join("recipes-app/app/app_1.0.bb"),
        "SUMMARY = \"Own app\"\nLICENSE = \"MIT\"\nDEPENDS = \"libfoo\"\n\
         require recipes-core/shared/shared.inc\ninherit autotools\n",
    );
    (tmp, root)
}

struct Fixture {
    _cache: tempfile::TempDir,
    ctx: Ctx,
}

fn fixture(root: &Path) -> Fixture {
    let mut config = Config {
        build_dir: Some(root.join("build")),
        writable_layers: vec!["meta-mine".to_string()],
        work_dir: root.to_path_buf(),
        ..Config::default()
    };
    config.bitbake.enabled = false;
    fixture_from(config)
}

fn fixture_from(config: Config) -> Fixture {
    let workspace = Workspace::discover_in(&config, &KasEnv::default()).unwrap();
    let cache = tempfile::tempdir().unwrap();
    let mut index = Index::open_in(cache.path(), &workspace).unwrap();
    index.refresh(&workspace).unwrap();
    let handles = HandleStore::open(&index.cache_dir().join("handles")).unwrap();
    Fixture {
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

fn opts(from: &str, to: Option<&str>) -> Options {
    Options {
        layer: "meta-up".into(),
        from: from.into(),
        to: to.map(str::to_string),
        recipe: None,
        all: false,
    }
}

fn args(from: &str, to: Option<&str>, budget: usize, format: Option<Format>) -> diff_tool::Args {
    diff_tool::Args {
        layer: "meta-up".into(),
        from: from.into(),
        to: to.map(str::to_string),
        recipe: None,
        all: None,
        budget_tokens: Some(budget),
        format,
    }
}

/// `git status --porcelain` and `HEAD` of the repository.
fn repo_state(repo: &Path) -> (String, String) {
    (
        git(repo, &["status", "--porcelain", "--untracked-files=all"]),
        git(repo, &["rev-parse", "HEAD"]),
    )
}

#[tokio::test]
async fn release_diff_text_and_json() {
    let (_tmp, root) = build_repo();
    let repo = root.join("upstream");
    let before = repo_state(&repo);
    let f = fixture(&root);

    let text = diff_tool::run(&f.ctx, args("v1", Some("HEAD"), 100_000, None))
        .await
        .unwrap();
    insta::assert_snapshot!("release_diff_text", text);
    let d = diff::build(&f.ctx, &opts("v1", Some("HEAD")))
        .await
        .unwrap();
    insta::assert_snapshot!("release_diff_json", diff::render_json(&d));

    // The summary comes first, then the own bbappends by severity.
    assert!(text.starts_with("recipe\tkind\tname\tchange\tvalue\tfrom\tto\n-\tsummary\t"));
    let sev: Vec<(String, Severity)> = d
        .appends
        .iter()
        .map(|a| (a.append.clone(), a.severity))
        .collect();
    assert_eq!(
        sev,
        [
            (
                "meta-mine:recipes-core/hello/hello_1.0.bbappend:1".to_string(),
                Severity::NoLongerApplies
            ),
            (
                "meta-mine:recipes-core/hello/hello_%.bbappend:1".to_string(),
                Severity::DifferentVersion
            ),
            (
                "meta-mine:recipes-core/widget/widget_git.bbappend:1".to_string(),
                Severity::SourceChanged
            ),
            (
                "meta-mine:recipes-core/stable/stable_%.bbappend:1".to_string(),
                Severity::Unchanged
            ),
        ]
    );
    let c = &d.summary.appends;
    assert_eq!(
        (c.no_longer_applies, c.different_version, c.source_changed),
        (1, 1, 1)
    );
    // The pinned append names the version it applied to.
    assert!(d.appends[0].detail.contains("hello_1.0.bb (version 1.0)"));
    // The % append: its patch of the same name as the upstream's new one is flagged.
    let fix = d.appends[1]
        .patches
        .iter()
        .find(|p| p.entry == "file://fix.patch")
        .unwrap();
    assert!(
        fix.upstream
            .as_deref()
            .unwrap()
            .contains("added to the recipe upstream")
    );
    assert_eq!(
        fix.file.as_deref(),
        Some("meta-mine:recipes-core/hello/hello/fix.patch:1")
    );
    assert!(
        d.appends[1]
            .touches
            .iter()
            .any(|i: &String| i == "do_install")
    );
    // The widget append's patch sits on a changed SRCREV.
    assert!(
        d.appends[2]
            .source_changes
            .iter()
            .any(|s| s.starts_with("SRCREV ="))
    );

    // Default selection: appended (hello, widget, stable) or used (libfoo by DEPENDS),
    // plus the file meta-mine requires.
    let names: Vec<(&str, &str)> = d
        .recipes
        .iter()
        .map(|r| (r.name.as_str(), r.status))
        .collect();
    assert_eq!(
        names,
        [
            ("hello", "changed"),
            ("stable", "unchanged"),
            ("widget", "changed"),
            ("libfoo", "changed"),
            ("meta-mine:recipes-app/app/app_1.0.bb", "changed"),
        ]
    );
    let hello = &d.recipes[0];
    assert_eq!(
        (hello.from_version.as_deref(), hello.to_version.as_deref()),
        (Some("1.0"), Some("1.1"))
    );
    let has = |kind: &str, name: &str, change: &str| {
        hello
            .changes
            .iter()
            .any(|c| c.kind == kind && c.name == name && c.change == change)
    };
    assert!(has("var", "SUMMARY =", "changed"));
    assert!(has("var", "DEPENDS =", "changed"), "the .inc change");
    assert!(has("function", "do_install", "changed"));
    assert!(has("inherit", "inherit pkgconfig", "added"));
    assert!(has("SRC_URI entry", "file://fix.patch", "added"));
    // A `${@...}` expression is one entry.
    assert!(
        hello.changes.iter().any(
            |c| c.kind == "SRC_URI entry" && c.name.starts_with("file://${@bb.utils.contains(")
        )
    );
    assert!(has("file", "recipes-core/hello/hello_1.1.bb", "renamed"));
    // shared.inc, required by a layer-root path, is part of the tree and compared.
    assert!(has("var", "SHARED_FLAGS =", "changed"));
    assert!(
        hello
            .changes
            .iter()
            .any(|c| c.change == "not compared" && c.name == "conf/distro/elsewhere.inc")
    );
    // Whitespace-only and comment-only changes are counted, not listed.
    assert_eq!(hello.whitespace_only, 1);
    assert_eq!(hello.cosmetic_files, 1);
    assert!(!has("var", "EXTRA_OECONF =", "changed"));
    assert!(!has("var", "BBCLASSEXTEND =", "changed"));

    // Classes: autotools changed, inherited by hello and by meta-mine's app.
    assert_eq!(d.classes.len(), 1);
    assert_eq!(d.classes[0].path, "classes/autotools.bbclass");
    assert!(
        d.classes[0]
            .inherited_by
            .iter()
            .any(|i: &String| i == "hello")
    );
    assert_eq!(
        d.classes[0].own_users,
        ["meta-mine:recipes-app/app/app_1.0.bb:5"]
    );

    // Deterministic.
    let again = diff_tool::run(&f.ctx, args("v1", Some("HEAD"), 100_000, None))
        .await
        .unwrap();
    assert_eq!(text, again);

    // JSON through the tool: the same rows as the table.
    let json = diff_tool::run(
        &f.ctx,
        args("v1", Some("HEAD"), 100_000, Some(Format::Json)),
    )
    .await
    .unwrap();
    let t: JsonTable = serde_json::from_str(&json).unwrap();
    assert_eq!(
        t.header,
        ["recipe", "kind", "name", "change", "value", "from", "to"]
    );
    assert_eq!(t.rows.len() + 1 + t.notes.len(), text.lines().count());

    // Nothing in the repository changed.
    assert_eq!(repo_state(&repo), before);
}

#[tokio::test]
async fn all_recipe_and_working_tree() {
    let (_tmp, root) = build_repo();
    let repo = root.join("upstream");
    // An uncommitted change and an untracked recipe.
    write(
        &repo.join("meta-up/recipes-core/stable/stable_1.0.bb"),
        "SUMMARY = \"Stable, edited\"\nLICENSE = \"MIT\"\n",
    );
    write(
        &repo.join("meta-up/recipes-core/fresh/fresh_0.1.bb"),
        "SUMMARY = \"Untracked\"\n",
    );
    let before = repo_state(&repo);
    let f = fixture(&root);

    // `all`: added, removed, moved; the working tree is the default `to`.
    let d = diff::build(
        &f.ctx,
        &Options {
            all: true,
            ..opts("v1", None)
        },
    )
    .await
    .unwrap();
    assert_eq!(d.to.given, "working tree");
    assert_eq!(d.to.uncommitted, Some(2));
    let status = |n: &str| d.recipes.iter().find(|r| r.name == n).map(|r| r.status);
    assert_eq!(status("gone"), Some("removed"));
    assert_eq!(status("newone"), Some("added"));
    assert_eq!(status("fresh"), Some("added"));
    assert_eq!(status("stable"), Some("changed"));
    assert_eq!(status("moved"), Some("changed"));
    assert!(d.recipes.iter().find(|r| r.name == "moved").unwrap().moved);
    let s = &d.summary;
    assert_eq!(
        (s.compared, s.added, s.removed, s.renamed),
        (8, 2, 1, 2),
        "{}",
        s.line
    );
    // The stable append now reports a changed recipe.
    let stable = d.appends.iter().find(|a| a.recipe == "stable").unwrap();
    assert_eq!(stable.severity, Severity::RecipeChanged);

    // One recipe.
    let d = diff::build(
        &f.ctx,
        &Options {
            recipe: Some("widget".into()),
            ..opts("v1", Some("HEAD"))
        },
    )
    .await
    .unwrap();
    assert_eq!(d.recipes.len(), 1);
    assert_eq!(d.appends.len(), 1);
    let err = diff::build(
        &f.ctx,
        &Options {
            recipe: Some("hell".into()),
            ..opts("v1", Some("HEAD"))
        },
    )
    .await
    .unwrap_err();
    assert!(err.to_string().contains("similar: hello"), "{err}");

    assert_eq!(repo_state(&repo), before);
}

#[tokio::test]
async fn revisions_are_refused_before_git() {
    let (tmp, root) = build_repo();
    let repo = root.join("upstream");
    let before = repo_state(&repo);
    let f = fixture(&root);
    let target = tmp.path().join("x");
    let target = target.to_string_lossy().into_owned();
    let as_option = format!("--output={target}");
    for bad in [
        as_option.as_str(),
        "-p",
        "v1 HEAD",
        "HEAD:meta-up",
        "v1..HEAD",
        "",
    ] {
        let err = diff_tool::run(&f.ctx, args(bad, None, 600, None))
            .await
            .unwrap_err();
        assert!(err.to_string().contains("refused"), "{bad:?}: {err}");
        let err = diff_tool::run(&f.ctx, args("v1", Some(bad), 600, None))
            .await
            .unwrap_err();
        assert!(err.to_string().contains("refused"), "{bad:?}: {err}");
    }
    assert!(!Path::new(&target).exists());
    // A revision that does not exist: an error, nothing fetched.
    let err = diff_tool::run(&f.ctx, args("v9", None, 600, None))
        .await
        .unwrap_err();
    assert!(err.to_string().contains("not a commit"), "{err}");
    // An unknown layer, and a layer that is not in a repository.
    let mut a = args("v1", None, 600, None);
    a.layer = "meta-nope".into();
    assert!(diff_tool::run(&f.ctx, a).await.is_err());
    let mut a = args("v1", None, 600, None);
    a.layer = "meta-mine".into();
    let err = diff_tool::run(&f.ctx, a).await.unwrap_err();
    assert!(
        err.to_string().contains("not inside a git repository"),
        "{err}"
    );
    assert_eq!(repo_state(&repo), before);
}

#[tokio::test]
async fn large_diff_goes_to_a_handle() {
    let (_tmp, root) = build_repo();
    let f = fixture(&root);
    let mut a = args("v1", Some("HEAD"), 600, None);
    a.all = Some(true);
    let out = diff_tool::run(&f.ctx, a).await.unwrap();
    assert!(out.contains("more over budget_tokens"), "{out}");
    assert!(out.starts_with("recipe\tkind\tname\tchange\tvalue\tfrom\tto\n-\tsummary\t"));
    let hint = out
        .lines()
        .find(|l| l.starts_with("slice handle=h:"))
        .unwrap();
    let h = hint
        .trim_start_matches("slice handle=")
        .split_whitespace()
        .next()
        .unwrap()
        .to_string();
    let stored = String::from_utf8(f.ctx.handles.get(&Handle(h.clone())).unwrap()).unwrap();
    let full = diff::render_text(
        &diff::build(
            &f.ctx,
            &Options {
                all: true,
                ..opts("v1", Some("HEAD"))
            },
        )
        .await
        .unwrap(),
    );
    assert_eq!(stored, full);
    // Page through it with slice.
    let mut lines: Vec<String> = Vec::new();
    for page in 1.. {
        let json = slice::run(
            &f.ctx,
            slice::Args {
                handle: h.clone(),
                lines: None,
                grep: None,
                context: None,
                var: None,
                failures: None,
                page: Some(page),
                budget_tokens: None,
                format: Some(Format::Json),
            },
        )
        .await
        .unwrap();
        let t: JsonTable = serde_json::from_str(&json).unwrap();
        for row in &t.rows {
            lines.push(render::unescape_cell(&row[1]));
        }
        if !t.notes.iter().any(|n| n.contains("; next: slice")) {
            break;
        }
    }
    assert_eq!(lines.join("\n"), stored.trim_end_matches('\n'));

    // Asked for JSON: the handle holds JSON (the diff document), and paging through it
    // with slice gives back a document that parses.
    let mut a = args("v1", Some("HEAD"), 600, Some(Format::Json));
    a.all = Some(true);
    let out = diff_tool::run(&f.ctx, a).await.unwrap();
    let t: JsonTable = serde_json::from_str(&out).unwrap();
    assert!(t.more_over_budget > 0);
    let hint = t
        .notes
        .iter()
        .find(|n| n.starts_with("slice handle=h:"))
        .unwrap();
    let h = hint
        .trim_start_matches("slice handle=")
        .split_whitespace()
        .next()
        .unwrap()
        .to_string();
    assert!(
        t.notes
            .iter()
            .any(|n| n.contains("JSON, schema sous-chef-diff/1")),
        "{:?}",
        t.notes
    );
    let stored = String::from_utf8(f.ctx.handles.get(&Handle(h.clone())).unwrap()).unwrap();
    let v: serde_json::Value = serde_json::from_str(&stored).unwrap();
    assert_eq!(v["schema"], "sous-chef-diff/1");
    let mut lines: Vec<String> = Vec::new();
    for page in 1.. {
        let json = slice::run(
            &f.ctx,
            slice::Args {
                handle: h.clone(),
                lines: None,
                grep: None,
                context: None,
                var: None,
                failures: None,
                page: Some(page),
                budget_tokens: None,
                format: Some(Format::Json),
            },
        )
        .await
        .unwrap();
        let t: JsonTable = serde_json::from_str(&json).unwrap();
        for row in &t.rows {
            lines.push(render::unescape_cell(&row[1]));
        }
        if !t.notes.iter().any(|n| n.contains("; next: slice")) {
            break;
        }
    }
    let back: serde_json::Value = serde_json::from_str(&lines.join("\n")).unwrap();
    assert_eq!(back, v);
}

/// A workspace whose upstream layer `meta-up` has `old_conf` as its `conf/layer.conf` at
/// tag `v1`, and the standard one (`recipes-*/*/*.bb`) at `HEAD` and on disk. At `v1`
/// `foo` and `bar` are under `old-recipes/`; at `HEAD` `foo` has moved to
/// `recipes-core/` and `bar` stays where it was.
fn bbfiles_repo(old_conf: &str) -> (tempfile::TempDir, PathBuf) {
    let tmp = tempfile::tempdir().unwrap();
    let root = fs::canonicalize(tmp.path()).unwrap().join("ws");
    let repo = root.join("upstream");
    let up = repo.join("meta-up");
    write(
        &root.join("build/conf/bblayers.conf"),
        "BBPATH = \"${TOPDIR}\"\nBBFILES ?= \"\"\nBBLAYERS ?= \"${TOPDIR}/../upstream/meta-up ${TOPDIR}/../meta-mine\"\n",
    );
    write(
        &root.join("build/conf/local.conf"),
        "MACHINE ?= \"qemux86\"\n",
    );
    write(&up.join("conf/layer.conf"), old_conf);
    write(&up.join("conf/extra.inc"), "# nothing\n");
    write(
        &up.join("old-recipes/foo/foo_1.0.bb"),
        "SUMMARY = \"Foo\"\nLICENSE = \"MIT\"\n",
    );
    write(
        &up.join("old-recipes/bar/bar_1.0.bb"),
        "SUMMARY = \"Bar\"\nLICENSE = \"MIT\"\n",
    );
    git(&repo, &["init", "-q"]);
    git(&repo, &["add", "-A"]);
    git(&repo, &["commit", "-q", "-m", "release 1"]);
    git(&repo, &["tag", "v1"]);
    write(&up.join("conf/layer.conf"), &layer_conf("up", 5));
    fs::create_dir_all(up.join("recipes-core/foo")).unwrap();
    git(
        &repo,
        &[
            "mv",
            "meta-up/old-recipes/foo/foo_1.0.bb",
            "meta-up/recipes-core/foo/foo_1.0.bb",
        ],
    );
    git(&repo, &["add", "-A"]);
    git(&repo, &["commit", "-q", "-m", "release 2"]);
    write(
        &root.join("meta-mine/conf/layer.conf"),
        &layer_conf("mine", 10),
    );
    (tmp, root)
}

#[tokio::test]
async fn old_recipes_are_collected_with_the_old_bbfiles() {
    let old_conf = "BBPATH .= \":${LAYERDIR}\"\n\
         BBFILES += \"${LAYERDIR}/old-recipes/*/*.bb\"\n\
         BBFILE_COLLECTIONS += \"up\"\n\
         BBFILE_PATTERN_up = \"^${LAYERDIR}/\"\n\
         BBFILE_PRIORITY_up = \"5\"\n\
         LAYERSERIES_COMPAT_up = \"wrynose\"\n";
    let (_tmp, root) = bbfiles_repo(old_conf);
    let f = fixture(&root);
    let d = diff::build(
        &f.ctx,
        &Options {
            all: true,
            ..opts("v1", Some("HEAD"))
        },
    )
    .await
    .unwrap();
    let status = |n: &str| d.recipes.iter().find(|r| r.name == n).map(|r| r.status);
    // With the current BBFILES on both sides, foo would be "added" (old-recipes/ is not
    // collected) and bar not listed at all.
    assert_eq!(status("foo"), Some("changed"), "{:?}", d.recipes);
    assert!(d.recipes.iter().find(|r| r.name == "foo").unwrap().moved);
    assert_eq!(status("bar"), Some("removed"), "{:?}", d.recipes);
    assert!(
        !d.notes.iter().any(|n| n.contains("cannot be decided")),
        "{:?}",
        d.notes
    );

    // The working tree as `to` uses the current BBFILES there; v1 still its own.
    let d = diff::build(
        &f.ctx,
        &Options {
            all: true,
            ..opts("v1", None)
        },
    )
    .await
    .unwrap();
    let status = |n: &str| d.recipes.iter().find(|r| r.name == n).map(|r| r.status);
    assert_eq!(status("foo"), Some("changed"));
    assert_eq!(status("bar"), Some("removed"));
}

#[tokio::test]
async fn undecidable_old_bbfiles_fall_back_with_a_note() {
    let old_conf = "require conf/extra.inc\n\
         BBFILES += \"${LAYERDIR}/old-recipes/*/*.bb\"\n\
         BBFILE_COLLECTIONS += \"up\"\n\
         BBFILE_PATTERN_up = \"^${LAYERDIR}/\"\n\
         BBFILE_PRIORITY_up = \"5\"\n";
    let (_tmp, root) = bbfiles_repo(old_conf);
    let f = fixture(&root);
    let d = diff::build(
        &f.ctx,
        &Options {
            all: true,
            ..opts("v1", Some("HEAD"))
        },
    )
    .await
    .unwrap();
    let status = |n: &str| d.recipes.iter().find(|r| r.name == n).map(|r| r.status);
    assert_eq!(status("foo"), Some("added"));
    assert_eq!(status("bar"), None);
    assert!(
        d.notes.iter().any(|n| n.starts_with(
            "BBFILES at v1 cannot be decided statically (conf/layer.conf includes conf/extra.inc)"
        )),
        "{:?}",
        d.notes
    );
    // `?=` depends on what came before.
    let (_tmp, root) = bbfiles_repo("BBFILES ?= \"${LAYERDIR}/old-recipes/*/*.bb\"\n");
    let f = fixture(&root);
    let d = diff::build(
        &f.ctx,
        &Options {
            all: true,
            ..opts("v1", Some("HEAD"))
        },
    )
    .await
    .unwrap();
    assert!(
        d.notes
            .iter()
            .any(|n| n.contains("BBFILES ?= depends on what was set before")),
        "{:?}",
        d.notes
    );
}

#[test]
fn cli_writes_json_and_refuses_bad_revisions() {
    let (tmp, root) = build_repo();
    write(
        &root.join("sous-chef.toml"),
        "build_dir = \"build\"\nwritable_layers = [\"meta-mine\"]\n",
    );
    let cache = tmp.path().join("cache");
    let run = |extra: &[&str]| {
        Command::new(env!("CARGO_BIN_EXE_sous-chef"))
            .arg("diff")
            .args(extra)
            .current_dir(&root)
            .env("XDG_CACHE_HOME", &cache)
            .output()
            .unwrap()
    };
    let out = run(&["meta-up", "v1", "HEAD", "--format", "json"]);
    assert_eq!(out.status.code(), Some(0), "{out:?}");
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(v["schema"], "sous-chef-diff/1");
    assert_eq!(v["summary"]["appends"]["no_longer_applies"], 1);
    let text = run(&["meta-up", "v1", "HEAD"]);
    assert_eq!(text.status.code(), Some(0));
    assert!(String::from_utf8_lossy(&text.stdout).starts_with("recipe\tkind\t"));
    let target = tmp.path().join("x");
    let bad = run(&["meta-up", "--", &format!("--output={}", target.display())]);
    assert_eq!(bad.status.code(), Some(2), "{bad:?}");
    assert!(String::from_utf8_lossy(&bad.stderr).contains("refused"));
    assert!(!target.exists());
}

// ---------------------------------------------------------------------------------------
// The live workspace (docs/live-environment.md); skipped unless SOUS_CHEF_LIVE is set.

#[tokio::test]
#[ignore = "needs SOUS_CHEF_LIVE (docs/live-environment.md)"]
async fn live_openembedded_core_release_diff() {
    let Some(live) = std::env::var_os("SOUS_CHEF_LIVE").map(PathBuf::from) else {
        eprintln!("SOUS_CHEF_LIVE not set; skipped");
        return;
    };
    let root = live.join("yocto-kiss");
    let repo = root.join("openembedded-core");
    let cfg = Config::load(&sous_chef::config::CliOverrides {
        config: Some(live.join("scratch/sous-chef.toml")),
        work_dir: root.clone(),
        ..Default::default()
    })
    .unwrap();
    let before = repo_state(&repo);
    let f = fixture_from(cfg);
    let started = std::time::Instant::now();
    let d = diff::build(
        &f.ctx,
        &Options {
            layer: "meta".into(),
            from: "yocto-6.0".into(),
            to: Some("HEAD".into()),
            recipe: None,
            all: true,
        },
    )
    .await
    .unwrap();
    let secs = started.elapsed().as_secs_f64();
    assert!(secs < 60.0, "{secs} s");
    let text = diff::render_text(&d);
    // Invariants: every recipe is on some side, pointers are into meta, seven cells a row,
    // no absolute path, the counts add up, and the checkout is untouched.
    let s = &d.summary;
    assert!(s.compared > 500, "{}", s.line);
    assert_eq!(s.compared, s.changed + s.added + s.removed + s.unchanged);
    for r in d.recipes.iter().filter(|r| r.kind == "recipe") {
        assert!(r.from_file.is_some() || r.to_file.is_some(), "{}", r.name);
        for p in r.from_file.iter().chain(&r.to_file) {
            assert!(p.starts_with("meta:"), "{p}");
        }
    }
    for line in text.lines().take_while(|l| !l.starts_with("note: ")) {
        assert_eq!(line.split('\t').count(), 7, "{line}");
    }
    assert!(!text.contains(&root.display().to_string()));
    assert_eq!(repo_state(&repo), before);
}
