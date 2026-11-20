//! `var` on a task function lists what BitBake runs with it: `[prefuncs]`, the task,
//! `[postfuncs]` (each with pointers), the flags that change how it runs, and the functions
//! not run that may change those lists; `recipe show=tasks` has the lists in its `funcs`
//! column. The fixture workspace has no `[prefuncs]` / `[postfuncs]`: the cases are on a
//! temporary copy with a class added. The live check (`#[ignore]`, `SOUS_CHEF_LIVE`) is
//! busybox's `do_install` on yocto-kiss, compared with the recorded `bitbake -e busybox`.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::fs;
use std::path::{Path, PathBuf};

use sous_chef::bitbake::JobTable;
use sous_chef::config::Config;
use sous_chef::handles::HandleStore;
use sous_chef::index::Index;
use sous_chef::mcp::Ctx;
use sous_chef::mcp::tools::recipe::{self, Show};
use sous_chef::mcp::tools::var;
use sous_chef::render::Format;
use sous_chef::workspace::Workspace;

fn ctx_for(config: Config) -> (tempfile::TempDir, Ctx) {
    let cache = tempfile::tempdir().unwrap();
    let workspace = Workspace::discover(&config).unwrap();
    let mut index = Index::open_in(cache.path(), &workspace).unwrap();
    index.refresh(&workspace).unwrap();
    let handles = HandleStore::open(&index.cache_dir().join("handles")).unwrap();
    (
        cache,
        Ctx {
            config,
            workspace,
            index: tokio::sync::Mutex::new(index),
            handles,
            jobs: JobTable::new(),
        },
    )
}

fn args(name: &str, recipe: &str, format: Option<Format>) -> var::Args {
    var::Args {
        mc: None,
        name: name.into(),
        recipe: Some(recipe.into()),
        machine: None,
        distro: None,
        authoritative: false,
        flags: false,
        budget_tokens: None,
        format,
    }
}

/// The class the fixture copy gets, inherited by hello's bbappend: a postfunc by name, a
/// postfunc through a variable with an evaluated python helper, a prefunc that is not
/// defined, a helper with flags that is not a task, and anonymous python that writes
/// prefuncs through a computed task name (once under a false guard).
const CLASS: &str = r#"do_install[postfuncs] += "tidy_up"
do_install[postfuncs] += "${TIDY_MORE}"
TIDY_MORE = "${@bb.utils.contains('DISTRO_FEATURES', 'nosuchfeature', 'never_this', 'tidy_more', d)}"
do_install[prefuncs] = "missing_prefunc"
do_install[lockfiles] = "${TMPDIR}/install.lock"

tidy_up() {
	rm -f ${D}/junk
}

tidy_more() {
	:
}

helper_fn() {
	:
}
helper_fn[postfuncs] = "tidy_up"

python () {
    for task in (d.getVar('MY_TASKS') or '').split():
        d.prependVarFlag(task, 'prefuncs', 'my_prefunc ')
    if d.getVar('NOSUCHVAR_SET') == 'yes':
        for t in d.getVar('__BBTASKS', False):
            d.appendVarFlag(t, 'postfuncs', ' never_added')
    for x in bb.build.tasksbetween('do_unpack', 'do_install', d):
        d.appendVarFlag(x, 'postfuncs', ' maybe_added')
}
MY_TASKS = "do_compile do_configure"
"#;

fn edited_fixture() -> (tempfile::TempDir, PathBuf) {
    let (tmp, root) = common::temp_ws();
    let root = fs::canonicalize(root).unwrap();
    let cls = root.join("meta-mine/classes/tidy.bbclass");
    fs::create_dir_all(cls.parent().unwrap()).unwrap();
    fs::write(&cls, CLASS).unwrap();
    let app = root.join("meta-mine/recipes-core/hello/hello_1.1.bbappend");
    let mut text = fs::read_to_string(&app).unwrap();
    text.push_str("\ninherit tidy\n");
    fs::write(&app, text).unwrap();
    (tmp, root)
}

fn strip(out: &str, root: &Path) -> String {
    out.replace(root.display().to_string().as_str(), "<ws>")
}

#[tokio::test]
async fn var_on_a_task_function_lists_prefuncs_and_postfuncs() {
    let (_tmp, root) = edited_fixture();
    let (_c, ctx) = ctx_for(common::ws_config(&root));
    let out = strip(
        &var::run(&ctx, args("do_install", "hello", None))
            .await
            .unwrap(),
        &root,
    );
    let lines: Vec<&str> = out.lines().collect();
    let at = |prefix: &str| {
        lines
            .iter()
            .position(|l| l.starts_with(prefix))
            .unwrap_or_else(|| panic!("no line {prefix:?} in\n{out}"))
    };
    // Run order: the task line, the prefunc, the postfuncs in flag order.
    let task = at(
        "task: do_install (addtask at meta-core:classes/base.bbclass:45) runs its [prefuncs], then do_install, then its [postfuncs]",
    );
    let pre = at("prefunc: missing_prefunc (not defined");
    let post1 = at(
        "postfunc: tidy_up meta-mine:classes/tidy.bbclass:7-9 (by [postfuncs] += \"tidy_up\" at meta-mine:classes/tidy.bbclass:1)",
    );
    let post2 = at(
        "postfunc: tidy_more meta-mine:classes/tidy.bbclass:11-13 (by [postfuncs] += \"${TIDY_MORE}\" at meta-mine:classes/tidy.bbclass:2)",
    );
    assert!(at("final: function do_install") < task, "{out}");
    assert!(task < pre && pre < post1 && post1 < post2, "{out}");
    assert!(
        out.contains("; [lockfiles] \"${TMPDIR}/install.lock\" at meta-mine:classes/tidy.bbclass:5 (held while it runs)"),
        "{out}"
    );
    // The anonymous python: the computed write over MY_TASKS (no do_install) and the one
    // under a false guard are left out; the one over tasksbetween may reach it.
    assert!(!out.contains("my_prefunc"), "{out}");
    assert!(!out.contains("never_added"), "{out}");
    assert!(
        out.contains("note: not run: anonymous python at meta-mine:classes/tidy.bbclass:")
            && out.contains("may append maybe_added to [postfuncs] of the tasks it computes"),
        "{out}"
    );
    // The flag value itself is certain (the python helper is evaluated).
    assert!(!out.contains("postfuncs: uncertain"), "{out}");

    // JSON: the same lines, as notes.
    let json = var::run(&ctx, args("do_install", "hello", Some(Format::Json)))
        .await
        .unwrap();
    let v: serde_json::Value = serde_json::from_str(&json).unwrap();
    let notes: Vec<String> = v["notes"]
        .as_array()
        .unwrap()
        .iter()
        .map(|n| strip(n.as_str().unwrap(), &root))
        .collect();
    assert!(
        notes
            .iter()
            .any(|n| n.starts_with("postfunc: tidy_up meta-mine:classes/tidy.bbclass:7-9")),
        "{notes:?}"
    );

    // A task without the flags says so; a function that is not a task says its lists only
    // run for a task.
    let out = var::run(&ctx, args("do_compile", "hello", None))
        .await
        .unwrap();
    assert!(out.contains("prefuncs: none set"), "{out}");
    assert!(out.contains("postfuncs: none set"), "{out}");
    // MY_TASKS holds do_compile: the computed write may reach it.
    assert!(
        out.contains(
            "may prepend my_prefunc to [prefuncs], append maybe_added to [postfuncs] of the tasks it computes"
        ),
        "{out}"
    );
    let out = var::run(&ctx, args("helper_fn", "hello", None))
        .await
        .unwrap();
    assert!(
        out.contains("note: helper_fn is not a task (no addtask)"),
        "{out}"
    );
    assert!(out.contains("postfunc: tidy_up meta-mine:"), "{out}");
    // A plain variable gets none of it.
    let out = var::run(&ctx, args("EXTRA_OECONF", "hello", None))
        .await
        .unwrap();
    assert!(
        !out.contains("task:") && !out.contains("postfuncs"),
        "{out}"
    );
}

#[tokio::test]
async fn recipe_tasks_has_the_run_lists() {
    let (_tmp, root) = edited_fixture();
    let (_c, ctx) = ctx_for(common::ws_config(&root));
    let out = recipe::run(
        &ctx,
        recipe::Args {
            mc: None,
            name: "hello".into(),
            version: None,
            show: Show::Tasks,
            budget_tokens: Some(4000),
            format: None,
        },
    )
    .await
    .unwrap();
    let row = out
        .lines()
        .find(|l| l.starts_with("do_install\t"))
        .unwrap_or_else(|| panic!("{out}"));
    assert!(
        row.contains("\tpre: missing_prefunc; post: tidy_up tidy_more\t"),
        "{row}"
    );
    let row = out
        .lines()
        .find(|l| l.starts_with("do_compile\t"))
        .unwrap_or_else(|| panic!("{out}"));
    assert!(row.split('\t').nth(4) == Some("-"), "{row}");
}

/// The unmodified fixture: `do_install` of hello has neither list, and says so.
#[tokio::test]
async fn fixture_hello_do_install_says_none_set() {
    let (_c, ctx) = ctx_for(common::fixture_config());
    let out = var::run(&ctx, args("do_install", "hello", None))
        .await
        .unwrap();
    println!("{out}");
    for line in [
        "task: do_install (addtask at meta-core:classes/base.bbclass:45) runs its [prefuncs], then do_install, then its [postfuncs]",
        "prefuncs: none set",
        "postfuncs: none set",
        "flags: [dirs] \"${B}\" at meta-core:classes/base.bbclass:46 (created; it runs in the last)",
    ] {
        assert!(out.lines().any(|l| l == line), "{line}\n{out}");
    }
    assert!(!out.contains("not run:"), "{out}");
}

// ---- live ----

fn live() -> Option<PathBuf> {
    let p = PathBuf::from(std::env::var_os("SOUS_CHEF_LIVE")?);
    p.join("yocto-kiss/build/conf/bblayers.conf")
        .is_file()
        .then_some(p)
}

/// busybox's `do_install` on yocto-kiss: both postfuncs, each defined where the recorded
/// `bitbake -e busybox` puts it (`# line: N, file: ...` above the function).
#[tokio::test]
#[ignore]
async fn live_busybox_do_install_lists_its_postfuncs() {
    let Some(live) = live() else {
        eprintln!("SOUS_CHEF_LIVE not set: skipping");
        return;
    };
    let mut cfg = Config {
        build_dir: Some(live.join("yocto-kiss/build")),
        work_dir: live.join("yocto-kiss"),
        ..Config::default()
    };
    cfg.bitbake.enabled = false;
    let (_c, ctx) = ctx_for(cfg);
    let out = var::run(&ctx, args("do_install", "busybox", None))
        .await
        .unwrap();
    println!("{out}");
    let out_dir = live.join("runs/var-taskfuncs");
    if fs::create_dir_all(&out_dir).is_ok() {
        let _ = fs::write(out_dir.join("var-do_install-busybox.txt"), &out);
    }
    let post: Vec<&str> = out
        .lines()
        .filter(|l| l.starts_with("postfunc: "))
        .collect();
    assert_eq!(post.len(), 2, "{out}");
    assert!(
        post[0].starts_with("postfunc: remove_libtool_la meta:"),
        "{out}"
    );
    assert!(
        post[1].starts_with("postfunc: rm_systemd_unitdir meta:"),
        "{out}"
    );
    // The definitions agree with bitbake -e's `# line:` comments.
    let Ok(dump) = fs::read_to_string(live.join("runs/differential/env/busybox.env")) else {
        eprintln!("busybox.env not recorded: skipping the comparison");
        return;
    };
    for (line, f) in post.iter().zip(["remove_libtool_la", "rm_systemd_unitdir"]) {
        let lines: Vec<&str> = dump.lines().collect();
        let i = lines
            .iter()
            .position(|l| *l == format!("{f}() {{"))
            .unwrap_or_else(|| panic!("{f} not in the dump"));
        let comment = lines[i - 1];
        let (n, file) = comment
            .strip_prefix("# line: ")
            .and_then(|r| r.split_once(", file: "))
            .unwrap();
        let rel = file.split("/layers/openembedded-core/").nth(1).unwrap();
        let rel = rel.strip_prefix("meta/").unwrap_or(rel);
        let want = format!("{rel}:{n}-");
        assert!(line.contains(&want), "{line} vs {comment}");
    }
}
