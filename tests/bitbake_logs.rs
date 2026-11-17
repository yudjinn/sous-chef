//! T5.6: failure detection, log location, digests and the `log` tool, on recorded logs.
//!
//! The logs under `tests/fixtures/outputs/` were not captured from a real build (none is
//! available to the agents). They were written to the formats in the BitBake and OE-core
//! sources (bitbake `lib/bb/build.py`, `lib/bb/ui/knotty.py`, `lib/bb/runqueue.py`,
//! `lib/bb/utils.py`, `lib/bb/fetch/__init__.py`; OE-core `meta/lib/oe/patch.py`,
//! `meta/classes-global/patch.bbclass`, `base.bbclass`, `logging.bbclass`,
//! `meta/lib/oe/qa.py`, `classes-recipe/autotools.bbclass`), checked at bitbake 5495a54 and
//! openembedded-core 9518ce9 (2026-11-16). The compiler, configure, make, wget and quilt
//! output inside them follows those tools' usual formats and was not checked against a
//! source. They use the prefix `/home/builder/poky` for the workspace root; the tool test
//! rewrites it to a temporary copy of the fixture workspace.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::fs;
use std::path::{Path, PathBuf};

use sous_chef::bitbake::JobTable;
use sous_chef::bitbake::logs::{
    self, DigestInput, FailureKind, Rank, detect, digest, failed_tasks, failure_logfiles,
    find_task_logs, first_error, logs_in_t_dir, meta_from_path, newest_cooker_log,
};
use sous_chef::handles::{Handle, HandleStore};
use sous_chef::index::Index;
use sous_chef::mcp::Ctx;
use sous_chef::mcp::tools::log;
use sous_chef::render::Format;
use sous_chef::workspace::{Layer, Workspace};

const PREFIX: &str = "/home/builder/poky";
const WORK: &str = "/home/builder/poky/build/tmp/work/core2-64-poky-linux";

fn output(name: &str) -> String {
    fs::read_to_string(common::fixture_path(&format!("outputs/{name}"))).unwrap()
}

/// A workspace whose layers sit at the recorded logs' paths (nothing needs to exist:
/// `loc_for` is lexical first).
fn recorded_ws() -> Workspace {
    let layer = |name: &str| Layer {
        name: name.to_string(),
        root: PathBuf::from(format!("{PREFIX}/{name}")),
        collections: vec![name.to_string()],
        priority: 5,
        compat: Vec::new(),
        depends: Vec::new(),
        bbfiles: Vec::new(),
        writable: false,
    };
    Workspace {
        build_dir: Some(PathBuf::from(format!("{PREFIX}/build"))),
        layers: vec![layer("meta-core"), layer("meta-mine")],
    }
}

fn handle() -> Handle {
    Handle("h:0123456789".into())
}

/// Digest of a recorded task log, rendered as text; checks the size limit on the way.
fn digest_of(name: &str, log_path: &str, run_script: Option<&str>) -> (logs::Digest, String) {
    let text = output(name);
    let ws = recorded_ws();
    let h = handle();
    let path = PathBuf::from(log_path);
    let d = digest(&DigestInput {
        text: &text,
        log_path: Some(&path),
        run_script,
        ws: Some(&ws),
        handle: Some(&h),
        definition: None,
    });
    let out = d.render(Format::Text, 600);
    println!("--- {name} ({} bytes)\n{out}", out.len());
    assert!(
        out.len() < 1024,
        "digest of {name} is {} bytes:\n{out}",
        out.len()
    );
    assert!(
        out.contains("slice handle=h:0123456789 failures=true | grep=RE | lines="),
        "{out}"
    );
    (d, out)
}

#[test]
fn compile_error() {
    let run = output("run.do_compile.48213");
    let (d, out) = digest_of(
        "log.do_compile.48213",
        &format!("{WORK}/hello/1.0/temp/log.do_compile.48213"),
        Some(&run),
    );
    assert_eq!(d.meta.recipe.as_deref(), Some("hello"));
    assert_eq!(d.meta.version.as_deref(), Some("1.0"));
    assert_eq!(d.meta.task.as_deref(), Some("do_compile"));
    assert_eq!(d.meta.exit_code, Some(1));
    let e = d.first.as_ref().unwrap();
    assert_eq!(e.kind, FailureKind::Compile);
    assert_eq!(e.line, 8);
    assert!(
        e.message
            .contains("main.c:12:5: error: implicit declaration of function 'greet'")
    );
    // The "In function" line before and the caret lines after.
    assert!(
        e.context[0].contains("In function 'main'"),
        "{:?}",
        e.context
    );
    assert!(e.context.iter().any(|c| c.contains("greet(argv[1]);")));
    // The second compiler error and the generic `oe_runmake failed` are not consequences
    // of make; make's `***` lines and the exit trap are.
    assert_eq!(d.more, 1);
    let f = d.function.as_ref().unwrap();
    assert_eq!(f.function, "autotoolsish_do_compile");
    assert_eq!(
        f.chain,
        [
            "bbfatal_log",
            "die",
            "oe_runmake",
            "autotoolsish_do_compile",
            "do_compile"
        ]
    );
    assert_eq!(
        d.function_at.as_deref(),
        Some("meta-core:classes-recipe/autotoolsish.bbclass:23")
    );
    assert!(out.contains("task\tdo_compile"), "{out}");
    assert!(out.contains("error (compile, line 8)"), "{out}");
}

#[test]
fn compile_error_without_run_file_names_the_function() {
    let (d, out) = digest_of(
        "log.do_compile.48213",
        &format!("{WORK}/hello/1.0/temp/log.do_compile.48213"),
        None,
    );
    let f = d.function.as_ref().unwrap();
    assert_eq!(f.function, "autotoolsish_do_compile");
    assert_eq!(f.run_line, 78);
    assert!(d.function_at.is_none());
    assert!(out.contains("run file line 78"), "{out}");
}

#[test]
fn configure_error() {
    let (d, out) = digest_of(
        "log.do_configure.51077",
        &format!("{WORK}/libfoo/2.0/temp/log.do_configure.51077"),
        None,
    );
    assert_eq!(d.meta.recipe.as_deref(), Some("libfoo"));
    assert_eq!(d.meta.task.as_deref(), Some("do_configure"));
    // dash's exit trap: `WARNING: exit code 1 from a shell command.`
    assert_eq!(d.meta.exit_code, Some(1));
    let e = d.first.as_ref().unwrap();
    assert_eq!(e.kind, FailureKind::Configure);
    // Not AC_MSG_FAILURE's `in `<dir>':` line before it.
    assert_eq!(
        e.message,
        "configure: error: C compiler cannot create executables"
    );
    assert!(e.context.iter().any(|c| c.contains("config.log")));
    assert!(d.function.is_none());
    assert!(out.contains("task\tdo_configure"));
}

#[test]
fn fetch_failure() {
    let (d, out) = digest_of(
        "log.do_fetch.60311",
        &format!("{WORK}/app/0.1/temp/log.do_fetch.60311"),
        None,
    );
    assert_eq!(d.meta.task.as_deref(), Some("do_fetch"));
    assert_eq!(d.meta.recipe.as_deref(), Some("app"));
    let e = d.first.as_ref().unwrap();
    assert_eq!(e.kind, FailureKind::Fetch);
    assert_eq!(
        e.message,
        "FetchError: Unable to fetch URL from any source."
    );
    assert_eq!(
        e.detail.as_deref(),
        Some("https://downloads.example.org/app/app-0.1.tar.gz")
    );
    assert!(out.contains("url\thttps://downloads.example.org/app/app-0.1.tar.gz"));
    // The wget output inside the DEBUG message is not an error of its own.
    assert_eq!(d.more, 0);
}

#[test]
fn patch_failure() {
    let (d, out) = digest_of(
        "log.do_patch.61544",
        &format!("{WORK}/hello/1.1/temp/log.do_patch.61544"),
        None,
    );
    assert_eq!(d.meta.task.as_deref(), Some("do_patch"));
    assert_eq!(d.meta.version.as_deref(), Some("1.1"));
    let e = d.first.as_ref().unwrap();
    assert_eq!(e.kind, FailureKind::Patch);
    assert_eq!(
        e.detail.as_deref(),
        Some("0002-greet-add-name-argument.patch")
    );
    assert!(
        e.context.iter().any(|c| c == "Hunk #1 FAILED at 10."),
        "{:?}",
        e.context
    );
    assert!(e.context.iter().any(|c| c.contains("does not apply")));
    assert_eq!(
        e.message,
        "Applying patch '0002-greet-add-name-argument.patch'"
    );
    assert!(
        out.contains("\tApplying patch '0002-greet-add-name-argument.patch'\n"),
        "{out}"
    );
    assert_eq!(d.more, 0);
}

#[test]
fn qa_issue() {
    let (d, out) = digest_of(
        "log.do_package_qa.70210",
        &format!("{WORK}/app/0.1/temp/log.do_package_qa.70210"),
        None,
    );
    assert_eq!(d.meta.task.as_deref(), Some("do_package_qa"));
    let e = d.first.as_ref().unwrap();
    assert_eq!(e.kind, FailureKind::Qa);
    assert_eq!(
        e.message,
        "QA Issue: File /usr/bin/app in package app doesn't have GNU_HASH (didn't pass LDFLAGS?)"
    );
    assert_eq!(e.detail.as_deref(), Some("ldflags"));
    // The second file; `Fatal QA errors were found` is a consequence.
    assert_eq!(d.more, 1);
    assert!(out.contains("+1 more errors"));
}

#[test]
fn python_task_traceback() {
    let (d, out) = digest_of(
        "log.do_gen_config.73302",
        &format!("{WORK}/app/0.1/temp/log.do_gen_config.73302"),
        None,
    );
    assert_eq!(d.meta.task.as_deref(), Some("do_gen_config"));
    let e = d.first.as_ref().unwrap();
    assert_eq!(e.kind, FailureKind::Python);
    assert_eq!(
        e.message,
        "TypeError: Object of type set is not JSON serializable"
    );
    assert_eq!(e.detail.as_deref(), Some("TypeError"));
    // Reduced to the exception and the last frame in a layer file (not json/encoder.py).
    assert_eq!(
        e.context,
        ["in write_app_config at meta-mine:classes/appconfig.bbclass:9"]
    );
    assert!(!out.contains("encoder.py"), "{out}");
}

#[test]
fn python_traceback_without_workspace_uses_last_absolute_frame() {
    let text = output("log.do_gen_config.73302");
    let d = digest(&DigestInput {
        text: &text,
        ..DigestInput::default()
    });
    assert_eq!(
        d.first.unwrap().context,
        ["in default at /usr/lib/python3.12/json/encoder.py:180"]
    );
}

#[test]
fn every_recorded_log_names_task_and_first_error() {
    // (file, task, kind, start of the first error message)
    let cases = [
        (
            "log.do_compile.48213",
            "do_compile",
            FailureKind::Compile,
            "../../sources/hello-1.0/src/main.c:12:5",
        ),
        (
            "log.do_configure.51077",
            "do_configure",
            FailureKind::Configure,
            "configure: error: C compiler",
        ),
        (
            "log.do_fetch.60311",
            "do_fetch",
            FailureKind::Fetch,
            "FetchError: Unable to fetch",
        ),
        (
            "log.do_patch.61544",
            "do_patch",
            FailureKind::Patch,
            "Applying patch '0002-greet",
        ),
        (
            "log.do_package_qa.70210",
            "do_package_qa",
            FailureKind::Qa,
            "QA Issue: File /usr/bin/app ",
        ),
        (
            "log.do_gen_config.73302",
            "do_gen_config",
            FailureKind::Python,
            "TypeError:",
        ),
    ];
    for (name, task, kind, start) in cases {
        let (d, out) = digest_of(name, &format!("{WORK}/x/1/temp/{name}"), None);
        assert_eq!(d.meta.task.as_deref(), Some(task), "{name}");
        let e = d.first.as_ref().unwrap();
        assert_eq!(e.kind, kind, "{name}");
        assert!(e.message.starts_with(start), "{name}: {}", e.message);
        assert!(out.contains(&format!("task\t{task}")), "{name}: {out}");
        // The task name also comes from the log text alone.
        let text = output(name);
        assert_eq!(
            logs::meta_from_text(&text).task.as_deref(),
            Some(task),
            "{name}"
        );
    }
}

#[test]
fn detector_is_ordered_and_ranked() {
    let text = output("log.do_compile.48213");
    let f = detect(&text);
    assert!(f.windows(2).all(|w| w[0].line < w[1].line));
    let kinds: Vec<(FailureKind, Rank)> = f.iter().map(|f| (f.kind, f.rank)).collect();
    assert_eq!(
        kinds,
        [
            (FailureKind::Compile, Rank::Specific),
            (FailureKind::Compile, Rank::Specific),
            (FailureKind::Make, Rank::Consequence),
            (FailureKind::Make, Rank::Consequence),
            (FailureKind::Error, Rank::Consequence),
            (FailureKind::Shell, Rank::Consequence),
        ]
    );
    assert_eq!(first_error(&f).unwrap().line, 8);
    // The exit trap block covers the warning and its backtrace.
    let trap = &f[5];
    assert_eq!((trap.span.start, trap.span.end), (19, 26));
    assert!(detect("").is_empty());
    assert!(detect("NOTE: all fine\nDEBUG: ERROR: not really\n").is_empty());
}

#[test]
fn console_output_locates_logs_and_tasks() {
    let out = output("bitbake-hello-compile.out");
    let log = PathBuf::from(format!("{WORK}/hello/1.0/temp/log.do_compile.48213"));
    assert_eq!(failure_logfiles(&out), std::slice::from_ref(&log));
    let tasks = failed_tasks(&out);
    assert_eq!(tasks.len(), 1);
    assert_eq!(
        tasks[0].recipe_file,
        PathBuf::from(format!(
            "{PREFIX}/meta-core/recipes-core/hello/hello_1.0.bb"
        ))
    );
    assert_eq!(tasks[0].task, "do_compile");
    assert_eq!(tasks[0].exit_code, Some(1));
    assert_eq!(tasks[0].log.as_ref(), Some(&log));

    let meta = logs::meta_from_text(&out);
    assert_eq!(meta.recipe.as_deref(), Some("hello"));
    assert_eq!(meta.version.as_deref(), Some("1.0"));
    assert_eq!(meta.pf.as_deref(), Some("hello-1.0-r0"));
    assert_eq!(meta.exit_code, Some(1));
}

#[test]
fn console_output_with_log_data_digests() {
    let out = output("bitbake-app-install.out");
    let d = digest(&DigestInput {
        text: &out,
        ws: Some(&recorded_ws()),
        ..DigestInput::default()
    });
    assert_eq!(d.meta.recipe.as_deref(), Some("app"));
    assert_eq!(d.meta.version.as_deref(), Some("0.1"));
    assert_eq!(d.meta.task.as_deref(), Some("do_install"));
    assert_eq!(d.meta.exit_code, Some(1));
    let e = d.first.as_ref().unwrap();
    assert_eq!(e.kind, FailureKind::Shell);
    assert_eq!(
        e.message,
        "install: cannot stat 'app-helper': No such file or directory"
    );
    assert_eq!(d.function.as_ref().unwrap().function, "do_install");
    assert!(d.render(Format::Text, 600).len() < 1024);
    assert_eq!(
        failure_logfiles(&out),
        [PathBuf::from(format!(
            "{WORK}/app/0.1/temp/log.do_install.80117"
        ))]
    );
}

#[test]
fn meta_from_path_handles_both_workdir_layouts() {
    let m = meta_from_path(Path::new(
        "/b/tmp/work/core2-64-poky-linux/hello/1.0-r0/temp/log.do_compile.1",
    ));
    assert_eq!(m.recipe.as_deref(), Some("hello"));
    assert_eq!(m.version.as_deref(), Some("1.0-r0"));
    assert_eq!(m.task.as_deref(), Some("do_compile"));
    let m = meta_from_path(Path::new("/elsewhere/log.do_install"));
    assert_eq!(m.task.as_deref(), Some("do_install"));
    assert!(m.recipe.is_none());
}

#[test]
fn json_digest() {
    let text = output("log.do_patch.61544");
    let d = digest(&DigestInput {
        text: &text,
        handle: Some(&handle()),
        ..DigestInput::default()
    });
    let v: serde_json::Value = serde_json::from_str(&d.render(Format::Json, 600)).unwrap();
    assert_eq!(v["header"][0], "field");
    assert!(
        v["rows"]
            .as_array()
            .unwrap()
            .iter()
            .any(|r| r[0] == "error (patch, line 10)")
    );
    assert!(
        v["notes"][0]
            .as_str()
            .unwrap()
            .starts_with("slice handle=h:0123456789")
    );
}

#[test]
fn getvar_value_is_last_line() {
    assert_eq!(
        logs::parse_getvar_value("setup noise\n/b/tmp/work/x/hello/1.0/temp\n\n").as_deref(),
        Some("/b/tmp/work/x/hello/1.0/temp")
    );
    assert_eq!(logs::parse_getvar_value(""), None);
}

// --- locating logs on disk and the tool --------------------------------------------------

fn set_mtime(p: &Path, secs: u64) {
    let t = std::time::UNIX_EPOCH + std::time::Duration::from_secs(secs);
    fs::File::options()
        .write(true)
        .open(p)
        .unwrap()
        .set_modified(t)
        .unwrap();
}

/// Copies the fixture workspace and lays out a build directory with recorded logs.
fn build_tree() -> (tempfile::TempDir, PathBuf) {
    let (tmp, root) = common::temp_ws();
    let root = fs::canonicalize(&root).unwrap();
    let prefix = root.display().to_string();
    let put = |rel: &str, fixture: &str, secs: u64| {
        let p = root.join(rel);
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        fs::write(&p, output(fixture).replace(PREFIX, &prefix)).unwrap();
        set_mtime(&p, secs);
        p
    };
    let work = "build/tmp/work/core2-64-poky-linux";
    let t = format!("{work}/hello/1.0/temp");
    put(
        &format!("{t}/log.do_configure.48100"),
        "log.do_configure.51077",
        1_000,
    );
    let compile = put(
        &format!("{t}/log.do_compile.48213"),
        "log.do_compile.48213",
        2_000,
    );
    put(
        &format!("{t}/run.do_compile.48213"),
        "run.do_compile.48213",
        2_000,
    );
    std::os::unix::fs::symlink("log.do_compile.48213", root.join(&t).join("log.do_compile"))
        .unwrap();
    fs::write(root.join(&t).join("log.task_order"), "x\n").unwrap();
    put(
        &format!("{work}/app/0.1/temp/log.do_package_qa.70210"),
        "log.do_package_qa.70210",
        3_000,
    );
    put(
        "build/tmp/log/cooker/qemux86-64/20261116091400.log",
        "bitbake-hello-compile.out",
        2_500,
    );
    assert!(compile.exists());
    (tmp, root)
}

fn ctx_for(root: &Path, cache: &Path) -> Ctx {
    let config = common::ws_config(root);
    let workspace = Workspace::discover(&config).unwrap();
    let index = Index::open_in(cache, &workspace).unwrap();
    let handles = HandleStore::open(&index.cache_dir().join("handles")).unwrap();
    Ctx {
        config,
        workspace,
        index: tokio::sync::Mutex::new(index),
        handles,
        jobs: JobTable::new(),
    }
}

fn args(recipe: Option<&str>, task: Option<&str>) -> log::Args {
    log::Args {
        recipe: recipe.map(str::to_string),
        task: task.map(str::to_string),
        job: None,
        budget_tokens: None,
        format: None,
    }
}

#[test]
fn finds_newest_task_logs() {
    let (_tmp, root) = build_tree();
    let build = root.join("build");
    let all = find_task_logs(&build, None, None);
    let names: Vec<String> = all
        .iter()
        .map(|p| p.file_name().unwrap().to_string_lossy().into_owned())
        .collect();
    // The symlink duplicates log.do_compile.48213 and is dropped; log.task_order is no log.
    assert_eq!(
        names,
        [
            "log.do_package_qa.70210",
            "log.do_compile.48213",
            "log.do_configure.48100"
        ]
    );
    let hello = find_task_logs(&build, Some("hello"), Some("do_configure"));
    assert_eq!(hello.len(), 1);
    assert!(find_task_logs(&build, Some("nope"), None).is_empty());
    let t = build.join("tmp/work/core2-64-poky-linux/hello/1.0/temp");
    assert_eq!(logs_in_t_dir(&t, Some("compile")).len(), 1);
    assert_eq!(
        newest_cooker_log(&build).unwrap().file_name().unwrap(),
        "20261116091400.log"
    );
}

#[tokio::test]
async fn log_tool_digests_a_recipe_task() {
    let (tmp, root) = build_tree();
    let ctx = ctx_for(&root, &tmp.path().join("cache"));
    let out = log::run(&ctx, args(Some("hello"), Some("compile")))
        .await
        .unwrap();
    assert!(out.len() < 1024, "{out}");
    assert!(out.contains("recipe\thello"), "{out}");
    assert!(out.contains("task\tdo_compile"), "{out}");
    assert!(
        out.contains("implicit declaration of function 'greet'"),
        "{out}"
    );
    // The run file next to the log maps the failing line to the layer.
    assert!(
        out.contains("autotoolsish_do_compile at meta-core:classes-recipe/autotoolsish.bbclass:23"),
        "{out}"
    );
    assert!(
        out.contains("log\tbuild:tmp/work/core2-64-poky-linux/hello/1.0/temp/log.do_compile.48213"),
        "{out}"
    );

    // The full log is stored under the handle the digest names.
    let id = out
        .lines()
        .find_map(|l| l.strip_prefix("handle\t"))
        .unwrap();
    let stored = ctx.handles.get(&id.parse().unwrap()).unwrap();
    assert!(String::from_utf8_lossy(&stored).contains("make: *** [Makefile:4: all] Error 2"));
    assert!(
        out.contains(&format!(
            "slice handle={id} failures=true | grep=RE | lines=7-10"
        )),
        "{out}"
    );
}

#[tokio::test]
async fn log_tool_defaults_to_the_most_recent_failure() {
    let (tmp, root) = build_tree();
    let ctx = ctx_for(&root, &tmp.path().join("cache"));
    // The newest console log names the do_compile log, though a newer QA log exists.
    let out = log::run(&ctx, args(None, None)).await.unwrap();
    assert!(out.contains("task\tdo_compile"), "{out}");

    // Without a console log: the newest failing task log.
    fs::remove_dir_all(root.join("build/tmp/log")).unwrap();
    let out = log::run(&ctx, args(None, None)).await.unwrap();
    assert!(out.contains("task\tdo_package_qa"), "{out}");

    // Recipe only: its newest failing log.
    let out = log::run(&ctx, args(Some("hello"), None)).await.unwrap();
    assert!(out.contains("task\tdo_compile"), "{out}");

    // JSON.
    let mut a = args(Some("hello"), Some("do_configure"));
    a.format = Some(Format::Json);
    let v: serde_json::Value = serde_json::from_str(&log::run(&ctx, a).await.unwrap()).unwrap();
    assert!(
        v["rows"]
            .as_array()
            .unwrap()
            .iter()
            .any(|r| r[1] == "do_configure")
    );
}

#[tokio::test]
async fn log_tool_errors() {
    let (tmp, root) = build_tree();
    let ctx = ctx_for(&root, &tmp.path().join("cache"));
    let e = log::run(&ctx, args(Some("nope"), Some("compile")))
        .await
        .unwrap_err();
    assert!(
        e.to_string().contains("do_compile log for recipe nope"),
        "{e}"
    );
    let e = log::run(&ctx, args(Some("hello"), Some("../../etc")))
        .await
        .unwrap_err();
    assert!(e.to_string().contains("invalid task name"), "{e}");
    let e = log::run(&ctx, args(Some("a/b"), None)).await.unwrap_err();
    assert!(e.to_string().contains("invalid recipe name"), "{e}");
    let mut a = args(None, None);
    a.job = Some("job:1".into());
    assert!(log::run(&ctx, a).await.is_err());
}

#[tokio::test]
async fn log_tool_confines_logs_named_by_console_output() {
    let (tmp, root) = build_tree();
    // A console log pointing outside the workspace is not followed.
    let outside = tmp.path().join("outside/log.do_compile.1");
    fs::create_dir_all(outside.parent().unwrap()).unwrap();
    fs::write(&outside, "ERROR: secret\n").unwrap();
    let cooker = root.join("build/tmp/log/cooker/qemux86-64/20261116091400.log");
    fs::write(
        &cooker,
        format!(
            "ERROR: Logfile of failure stored in: {}\n",
            outside.display()
        ),
    )
    .unwrap();
    let ctx = ctx_for(&root, &tmp.path().join("cache"));
    let e = log::run(&ctx, args(None, None)).await.unwrap_err();
    assert!(e.to_string().contains("outside the workspace"), "{e}");
}
