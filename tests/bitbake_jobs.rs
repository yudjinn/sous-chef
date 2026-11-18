//! T5.4: background jobs and the `bitbake` tool, against the fake `bitbake` in
//! `tests/fixtures/fakebb/` (never a real BitBake). Also the connections that waited for
//! the job table: `log job=...`, `status.job`, and the release-dependent work directory
//! layout in `bitbake::logs`.
//!
//! Each test configures `init_script = tests/fixtures/fakebb/init-with-env`, which sources
//! the fake `oe-init-build-env` and then `<build>/fakebb.env`; the test writes `FAKEBB_*`
//! settings there instead of changing the test process environment.
#![cfg(unix)]
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use sous_chef::bitbake::logs::{meta_from_path, meta_from_path_in};
use sous_chef::bitbake::{BbEnv, HostEnv, JobSpec, JobState, JobTable, Started};
use sous_chef::config::Config;
use sous_chef::error::Error;
use sous_chef::handles::{HandleKind, HandleStore};
use sous_chef::index::Index;
use sous_chef::mcp::tools::bitbake::{self, Args, Op, check_destructive, plan};
use sous_chef::mcp::tools::{log, status};
use sous_chef::mcp::{Ctx, tool_error_message};
use sous_chef::release::{Release, ReleaseInfo, ReleaseSource};
use sous_chef::render::Format;
use sous_chef::workspace::Workspace;

struct Fixture {
    _tmp: tempfile::TempDir,
    _cache: tempfile::TempDir,
    root: PathBuf,
    ctx: Ctx,
}

fn fixture_with(edit: impl FnOnce(&mut Config)) -> Fixture {
    let (tmp, root) = common::temp_ws();
    let mut config = common::ws_config(&root);
    config.bitbake.enabled = true;
    config.init_script = Some(common::fixture_path("fakebb/init-with-env"));
    edit(&mut config);
    let workspace = Workspace::discover(&config).unwrap();
    let cache = tempfile::tempdir().unwrap();
    let index = Index::open_in(cache.path(), &workspace).unwrap();
    let handles = HandleStore::open(&index.cache_dir().join("handles")).unwrap();
    let ctx = Ctx {
        config,
        workspace,
        index: tokio::sync::Mutex::new(index),
        handles,
        jobs: JobTable::new(),
    };
    Fixture {
        _tmp: tmp,
        _cache: cache,
        root,
        ctx,
    }
}

fn fixture() -> Fixture {
    fixture_with(|_| {})
}

impl Fixture {
    fn build(&self) -> PathBuf {
        self.ctx.workspace.build_dir.clone().unwrap()
    }

    /// Settings for the next fake bitbake run.
    fn fake(&self, vars: &[(&str, &str)]) {
        let text: String = vars
            .iter()
            .map(|(k, v)| format!("export {k}='{v}'\n"))
            .collect();
        fs::write(self.build().join("fakebb.env"), text).unwrap();
    }

    async fn call(&self, args: Args) -> sous_chef::error::Result<String> {
        bitbake::run(&self.ctx, args).await
    }

    async fn wait_done(&self, id: &str) {
        let job = self
            .ctx
            .jobs
            .wait(id, Duration::from_secs(20))
            .await
            .unwrap();
        assert_ne!(job.state, JobState::Running, "{job:?}");
    }
}

fn args(op: Op) -> Args {
    Args {
        op,
        target: None,
        task: None,
        args: Vec::new(),
        confirm: false,
        job: None,
        wait: None,
        budget_tokens: None,
        format: None,
    }
}

fn target(op: Op, t: &str) -> Args {
    Args {
        target: Some(t.into()),
        ..args(op)
    }
}

fn with_args(mut a: Args, extra: &[&str]) -> Args {
    a.args = extra.iter().map(|s| s.to_string()).collect();
    a
}

fn status_of(id: Option<&str>) -> Args {
    Args {
        job: id.map(str::to_string),
        ..args(Op::Status)
    }
}

fn json(text: &str) -> serde_json::Value {
    serde_json::from_str(text).unwrap_or_else(|e| panic!("{e}: {text}"))
}

/// Waits until `path` exists.
async fn wait_for_file(path: &Path) {
    let deadline = Instant::now() + Duration::from_secs(10);
    while !path.exists() {
        assert!(
            Instant::now() < deadline,
            "{} never appeared",
            path.display()
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

fn alive(pid: &str) -> bool {
    // A zombie still has a /proc entry; check its state.
    fs::read_to_string(format!("/proc/{pid}/stat"))
        .map(|s| {
            s.rsplit_once(')')
                .is_some_and(|(_, rest)| !rest.trim_start().starts_with('Z'))
        })
        .unwrap_or(false)
}

// --- argv ------------------------------------------------------------------------------

#[test]
fn argv_for_each_operation() {
    let f = fixture();
    let ws = &f.ctx.workspace;
    let cfg = &f.ctx.config;
    let argv = |a: Args| {
        let p = plan(cfg, ws, &a).unwrap();
        (p.spec.command_line(), p.destructive, p.spec.kind)
    };
    assert_eq!(
        argv(target(Op::Build, "core-image-minimal")),
        (
            "bitbake core-image-minimal".into(),
            false,
            HandleKind::Build
        )
    );
    assert_eq!(
        argv(with_args(target(Op::Build, "hello"), &["continue"])).0,
        "bitbake -k hello"
    );
    let mut t = with_args(target(Op::Task, "hello"), &["force"]);
    t.task = Some("do_compile".into());
    assert_eq!(argv(t).0, "bitbake -c compile -f hello");
    let mut t = target(Op::Task, "hello");
    t.task = Some("install".into());
    assert_eq!(argv(t).0, "bitbake -c install hello");
    assert_eq!(
        argv(args(Op::Env)),
        ("bitbake -e".into(), false, HandleKind::Env)
    );
    assert_eq!(argv(target(Op::Env, "hello")).0, "bitbake -e hello");
    assert_eq!(
        argv(target(Op::Clean, "hello")),
        ("bitbake -c clean hello".into(), false, HandleKind::Build)
    );
    assert_eq!(
        argv(with_args(target(Op::Clean, "hello"), &["cleansstate"])),
        (
            "bitbake -c cleansstate hello".into(),
            true,
            HandleKind::Build
        )
    );
    assert!(argv(with_args(target(Op::Clean, "hello"), &["cleanall"])).1);
    assert_eq!(
        argv(with_args(args(Op::Devtool), &["status"])).0,
        "devtool status"
    );
    for (sub, destructive) in [("modify", false), ("build", false), ("reset", true)] {
        assert_eq!(
            argv(with_args(target(Op::Devtool, "hello"), &[sub])),
            (
                format!("devtool {sub} hello"),
                destructive,
                HandleKind::Build
            )
        );
    }
    assert_eq!(
        argv(with_args(
            target(Op::Devtool, "hello"),
            &["finish", "meta-mine"]
        )),
        (
            "devtool finish hello meta-mine".into(),
            true,
            HandleKind::Build
        )
    );
    assert_eq!(
        argv(args(Op::Layers)),
        ("bitbake-layers show-layers".into(), false, HandleKind::Text)
    );
    assert_eq!(
        argv(with_args(target(Op::Layers, "hello"), &["show-recipes"])).0,
        "bitbake-layers show-recipes hello"
    );
    assert_eq!(
        argv(with_args(args(Op::Layers), &["show-appends"])).0,
        "bitbake-layers show-appends"
    );
    assert_eq!(
        argv(with_args(args(Op::Layers), &["show-overlayed"])).0,
        "bitbake-layers show-overlayed"
    );

    // Refused: unknown arguments, missing targets, wrong shapes.
    for bad in [
        args(Op::Build),
        with_args(target(Op::Build, "hello"), &["-f"]),
        with_args(target(Op::Build, "hello"), &["force"]),
        target(Op::Task, "hello"),
        with_args(target(Op::Env, "hello"), &["continue"]),
        with_args(target(Op::Clean, "hello"), &["clean", "cleanall"]),
        with_args(target(Op::Clean, "hello"), &["distclean"]),
        target(Op::Devtool, "hello"),
        with_args(target(Op::Devtool, "hello"), &["add"]),
        with_args(target(Op::Devtool, "hello"), &["status"]),
        with_args(target(Op::Devtool, "hello"), &["modify", "/src/tree"]),
        with_args(args(Op::Devtool), &["reset"]),
        with_args(target(Op::Devtool, "hello"), &["finish"]),
        with_args(target(Op::Layers, "hello"), &["show-layers"]),
        with_args(args(Op::Layers), &["add-layer"]),
    ] {
        assert!(
            matches!(plan(cfg, ws, &bad), Err(Error::Invalid { .. })),
            "{bad:?}"
        );
    }
    // devtool finish: the destination must be a known, writable layer.
    let e = plan(
        cfg,
        ws,
        &with_args(target(Op::Devtool, "hello"), &["finish", "meta-core"]),
    )
    .unwrap_err();
    assert!(e.to_string().contains("not writable"), "{e}");
    let e = plan(
        cfg,
        ws,
        &with_args(target(Op::Devtool, "hello"), &["finish", "meta-nope"]),
    )
    .unwrap_err();
    assert!(matches!(e, Error::NotFound { .. }), "{e}");
}

/// `mc:<multiconfig>:<target>` is a BitBake target (multiconfig); nothing else with a `:`
/// is, and the earlier injection cases stay rejected with the prefix in front of them.
#[tokio::test]
async fn multiconfig_targets_are_accepted_in_exactly_one_form() {
    let f = fixture();
    let ws = &f.ctx.workspace;
    let cfg = &f.ctx.config;
    let line = |a: Args| plan(cfg, ws, &a).map(|p| p.spec.command_line());
    assert_eq!(
        line(target(Op::Build, "mc:musl:core-image-minimal")).unwrap(),
        "bitbake mc:musl:core-image-minimal"
    );
    let mut t = target(Op::Task, "mc:tiny:hello");
    t.task = Some("compile".into());
    assert_eq!(line(t).unwrap(), "bitbake -c compile mc:tiny:hello");
    assert_eq!(
        line(target(Op::Env, "mc:musl:hello")).unwrap(),
        "bitbake -e mc:musl:hello"
    );
    assert_eq!(
        line(target(Op::Clean, "mc:musl:hello")).unwrap(),
        "bitbake -c clean mc:musl:hello"
    );
    // devtool and bitbake-layers take plain names.
    assert!(line(with_args(target(Op::Devtool, "mc:musl:hello"), &["modify"])).is_err());
    assert!(
        line(with_args(
            target(Op::Layers, "mc:musl:hello"),
            &["show-recipes"]
        ))
        .is_err()
    );
    let marker = f.root.join("pwned");
    for t in [
        "mc:musl:hello;touch pwned",
        "mc:musl:$(touch pwned)",
        "mc:musl:`touch pwned`",
        "mc:musl:-k",
        "mc:-k:hello",
        "mc:musl:--help",
        "mc:musl:a b",
        "mc:musl:a:b",
        "mc::hello",
        "mc:musl:",
        "multiconfig:musl:hello",
        "hello:mc:musl:x",
        "a:b",
    ] {
        for op in [Op::Build, Op::Task, Op::Env, Op::Clean] {
            let mut a = target(op, t);
            a.task = Some("compile".into());
            let e = f.call(a).await.unwrap_err();
            assert!(matches!(e, Error::Invalid { .. }), "{op:?} {t}: {e}");
        }
    }
    assert!(f.ctx.jobs.list().is_empty(), "a process was started");
    assert!(!marker.exists());
}

#[tokio::test]
async fn unsafe_names_are_rejected_before_anything_runs() {
    let f = fixture();
    let marker = f.root.join("pwned");
    let bad_targets = [
        "hello;touch pwned".to_string(),
        "$(touch pwned)".to_string(),
        "`touch pwned`".to_string(),
        "-k".to_string(),
        "--help".to_string(),
        "a b".to_string(),
        "a:b".to_string(),
    ];
    for t in &bad_targets {
        for op in [Op::Build, Op::Task, Op::Env, Op::Clean] {
            let mut a = target(op, t);
            a.task = Some("compile".into());
            let e = f.call(a).await.unwrap_err();
            assert!(matches!(e, Error::Invalid { .. }), "{op:?} {t}: {e}");
        }
        let e = f
            .call(with_args(target(Op::Devtool, t), &["modify"]))
            .await
            .unwrap_err();
        assert!(matches!(e, Error::Invalid { .. }), "devtool {t}: {e}");
        let e = f
            .call(with_args(target(Op::Layers, t), &["show-recipes"]))
            .await
            .unwrap_err();
        assert!(matches!(e, Error::Invalid { .. }), "layers {t}: {e}");
    }
    for task in ["-f", "compile;id", "$(id)", "do_compile x"] {
        let mut a = target(Op::Task, "hello");
        a.task = Some(task.into());
        assert!(f.call(a).await.is_err(), "{task}");
    }
    let e = f
        .call(with_args(
            target(Op::Devtool, "hello"),
            &["finish", "meta-mine;id"],
        ))
        .await
        .unwrap_err();
    assert!(matches!(e, Error::Invalid { .. }), "{e}");
    assert!(f.ctx.jobs.list().is_empty(), "a process was started");
    assert!(!marker.exists());
}

// --- builds ----------------------------------------------------------------------------

#[tokio::test]
async fn successful_build() {
    let f = fixture();
    f.fake(&[("FAKEBB_STDERR", "NOTE: Tasks Summary: all succeeded")]);
    let out = f.call(target(Op::Build, "hello")).await.unwrap();
    assert!(out.contains("job\tjob:1\n"), "{out}");
    assert!(out.contains("state\trunning\n"), "{out}");
    assert!(out.contains("command\tbitbake hello\n"), "{out}");
    assert!(out.contains("op=status job=job:1"), "{out}");
    f.wait_done("job:1").await;

    let out = f.call(status_of(None)).await.unwrap();
    assert!(out.contains("state\tsucceeded\n"), "{out}");
    assert!(out.contains("exit\t0\n"), "{out}");
    assert!(
        out.contains("tail\tNOTE: Tasks Summary: all succeeded"),
        "{out}"
    );
    assert!(out.contains("slice handle=h:"), "{out}");

    let v = json(
        &f.call(Args {
            format: Some(Format::Json),
            ..status_of(Some("1"))
        })
        .await
        .unwrap(),
    );
    assert_eq!(v["job"]["state"], "succeeded");
    assert_eq!(v["job"]["op"], "build");
    assert_eq!(v["job"]["target"], "hello");
    assert_eq!(v["job"]["kind"], "build");
    let handle: sous_chef::handles::Handle = v["job"]["handle"].as_str().unwrap().parse().unwrap();
    let stored = String::from_utf8(f.ctx.handles.get(&handle).unwrap()).unwrap();
    // stdout and stderr both land in the output; the build ran in the build directory.
    assert!(stored.contains("fakebb: bitbake\n"), "{stored}");
    assert!(stored.contains("arg: hello\n"), "{stored}");
    assert!(stored.contains("argc: 1\n"), "{stored}");
    assert!(
        stored.contains(&format!("cwd: {}", f.build().display())),
        "{stored}"
    );
    assert!(stored.contains("all succeeded"), "{stored}");
    // The output file is gone once the handle holds it.
    let job = f.ctx.jobs.get("job:1").unwrap();
    assert!(!job.output.exists());
    assert!(v["digest"].is_null());
}

/// The recorded console output of a failing `do_compile`, with its paths moved into the
/// temporary workspace, and the task log it names in place.
fn recorded_failure(f: &Fixture, with_task_log: bool) -> PathBuf {
    let text = fs::read_to_string(common::fixture_path("outputs/bitbake-hello-compile.out"))
        .unwrap()
        .replace("/home/builder/poky", &f.root.display().to_string());
    let out = f.root.join("console.out");
    fs::write(&out, text).unwrap();
    if with_task_log {
        let temp = f
            .build()
            .join("tmp/work/core2-64-poky-linux/hello/1.0/temp");
        fs::create_dir_all(&temp).unwrap();
        for name in ["log.do_compile.48213", "run.do_compile.48213"] {
            let text = fs::read_to_string(common::fixture_path(&format!("outputs/{name}")))
                .unwrap()
                .replace("/home/builder/poky", &f.root.display().to_string());
            fs::write(temp.join(name), text).unwrap();
        }
    }
    out
}

#[tokio::test]
async fn failing_build_digest_and_log_job() {
    let f = fixture();
    let console = recorded_failure(&f, true);
    f.fake(&[
        ("FAKEBB_OUTPUT", &console.display().to_string()),
        ("FAKEBB_EXIT", "1"),
    ]);
    f.call(target(Op::Build, "hello")).await.unwrap();
    f.wait_done("job:1").await;

    let out = f.call(status_of(Some("job:1"))).await.unwrap();
    assert!(out.contains("state\tfailed\n"), "{out}");
    assert!(out.contains("exit\t1\n"), "{out}");
    // The digest of the console output names the failing task and the first error.
    assert!(out.contains("recipe\thello"), "{out}");
    assert!(out.contains("task\tdo_compile"), "{out}");
    assert!(out.contains("oe_runmake failed"), "{out}");
    assert!(out.contains("handle\th:"), "{out}");
    assert!(out.contains("log job=job:1"), "{out}");
    assert!(out.len() < 2048, "{}", out.len());

    let v = json(
        &f.call(Args {
            format: Some(Format::Json),
            ..status_of(None)
        })
        .await
        .unwrap(),
    );
    assert_eq!(v["job"]["state"], "failed");
    assert_eq!(v["job"]["exit_code"], 1);
    assert_eq!(v["digest"]["meta"]["task"], "do_compile");
    let job_handle = v["job"]["handle"].as_str().unwrap().to_string();

    // log job=...: the task log named in the output, digested as usual.
    let la = |task: Option<&str>| log::Args {
        recipe: None,
        task: task.map(str::to_string),
        job: Some("job:1".into()),
        budget_tokens: None,
        format: None,
    };
    let out = log::run(&f.ctx, la(None)).await.unwrap();
    assert!(out.contains("task\tdo_compile"), "{out}");
    assert!(out.contains("log.do_compile.48213"), "{out}");
    assert!(
        !out.contains(&job_handle),
        "the task log has its own handle: {out}"
    );
    assert!(out.contains("slice handle=h:"), "{out}");
    // A task filter that matches no named log falls back to the console output.
    let out = log::run(&f.ctx, la(Some("install"))).await.unwrap();
    assert!(out.contains(&job_handle), "{out}");
    assert!(out.contains("names no task log"), "{out}");
    // Unknown job.
    let mut a = la(None);
    a.job = Some("job:9".into());
    let e = log::run(&f.ctx, a).await.unwrap_err();
    assert!(matches!(e, Error::NotFound { .. }), "{e}");
}

#[tokio::test]
async fn log_job_without_a_readable_task_log_digests_the_output() {
    let f = fixture();
    let console = recorded_failure(&f, false);
    f.fake(&[
        ("FAKEBB_OUTPUT", &console.display().to_string()),
        ("FAKEBB_EXIT", "1"),
    ]);
    f.call(target(Op::Build, "hello")).await.unwrap();
    f.wait_done("job:1").await;
    let out = log::run(
        &f.ctx,
        log::Args {
            recipe: None,
            task: None,
            job: Some("1".into()),
            budget_tokens: None,
            format: None,
        },
    )
    .await
    .unwrap();
    assert!(out.contains("is not readable"), "{out}");
    assert!(out.contains("task\tdo_compile"), "{out}");
    assert!(out.contains("oe_runmake failed"), "{out}");
}

// --- warnings of a finished job (y3-leftovers, item 5) ----------------------------------

/// A recorded real console (`tests/fixtures/outputs/live/y3-leftovers/<name>`), with
/// `{{LIVE}}` replaced by the fixture's root, written into the fixture.
fn live_y3(f: &Fixture, name: &str) -> PathBuf {
    let text = fs::read_to_string(common::fixture_path(&format!(
        "outputs/live/y3-leftovers/{name}"
    )))
    .unwrap()
    .replace("{{LIVE}}", &f.root.display().to_string());
    let p = f.root.join(name);
    fs::write(&p, text).unwrap();
    p
}

/// The first real build of curl had three warnings (uninative disabled, two fetches that
/// fell back to MIRRORS) and ended with knotty's count of them: the finished job lists the
/// warnings, not the count.
#[tokio::test]
async fn a_finished_job_lists_its_warnings_not_their_count() {
    let f = fixture();
    let console = live_y3(&f, "build1-first-build-trimmed.out");
    f.fake(&[("FAKEBB_OUTPUT", &console.display().to_string())]);
    f.call(target(Op::Build, "curl")).await.unwrap();
    f.wait_done("job:1").await;
    let out = f.call(status_of(Some("job:1"))).await.unwrap();
    assert!(out.contains("state\tsucceeded\n"), "{out}");
    let warnings: Vec<&str> = out
        .lines()
        .filter_map(|l| l.strip_prefix("warning\t"))
        .collect();
    assert_eq!(warnings.len(), 3, "{out}");
    assert!(warnings[0].starts_with("WARNING: Your host glibc version (2.44) is newer"));
    assert!(
        warnings[1]
            .starts_with("WARNING: ca-certificates-20260816-r0 do_fetch: Failed to fetch URL")
    );
    assert!(warnings[2].starts_with("WARNING: gnu-config-native-20250709+git-r0 do_fetch:"));
    assert!(!out.contains("Summary: There were"), "{out}");
    assert!(
        out.contains("tail\tNOTE: Tasks Summary: Attempted 919 tasks of which 2 didn't need to be rerun and all succeeded.\n"),
        "{out}"
    );
    let v = json(
        &f.call(Args {
            format: Some(Format::Json),
            ..status_of(Some("job:1"))
        })
        .await
        .unwrap(),
    );
    assert_eq!(v["warnings"].as_array().unwrap().len(), 3, "{v}");
}

/// Repeated warnings are listed once and counted; more than five distinct ones are counted
/// with a slice hint.
#[test]
fn warning_rows_dedupe_and_cap() {
    let mut text = String::new();
    for i in 0..7 {
        text.push_str(&format!(
            "WARNING: w{i}\nWARNING: w{i}\n| WARNING: from a log\n"
        ));
    }
    text.push_str("Summary: There were 14 WARNING messages.\n");
    let (shown, total) = sous_chef::bitbake::logs::console_warnings(&text);
    assert_eq!((shown.len(), total), (7, 14));
    let h: sous_chef::handles::Handle = "h:0123456789".parse().unwrap();
    let rows = sous_chef::bitbake::logs::warning_rows(&shown, total, Some(&h));
    assert_eq!(rows.len(), 6, "{rows:?}");
    assert_eq!(rows[0], "WARNING: w0");
    assert_eq!(
        rows[5],
        "+2 more distinct warnings; 7 repeated lines left out (14 WARNING lines in all: slice handle=h:0123456789 grep=^WARNING:)"
    );
    assert!(sous_chef::bitbake::logs::is_warning_summary(
        "Summary: There was 1 WARNING message."
    ));
    assert!(!sous_chef::bitbake::logs::is_warning_summary(
        "Summary: There was 1 ERROR message, returning a non-zero exit code."
    ));
}

// --- running, status, cancel -----------------------------------------------------------

#[tokio::test]
async fn status_while_running_then_cancel() {
    let f = fixture();
    let pidfile = f.root.join("fake.pids");
    f.fake(&[
        ("FAKEBB_SLEEP", "60"),
        ("FAKEBB_PIDFILE", &pidfile.display().to_string()),
        ("FAKEBB_STDERR", "NOTE: Executing Tasks"),
    ]);
    let mut a = target(Op::Task, "hello");
    a.task = Some("compile".into());
    f.call(a).await.unwrap();
    wait_for_file(&pidfile).await;
    tokio::time::sleep(Duration::from_millis(1100)).await;

    let out = f.call(status_of(None)).await.unwrap();
    assert!(out.contains("state\trunning\n"), "{out}");
    assert!(out.contains("op\ttask\n"), "{out}");
    assert!(out.contains("command\tbitbake -c compile hello\n"), "{out}");
    assert!(out.contains("tail\tNOTE: Executing Tasks"), "{out}");
    assert!(!out.contains("elapsed\t0s"), "{out}");

    // The `status` tool reports the running job.
    let v = json(
        &status::run(
            &f.ctx,
            status::Args {
                budget_tokens: None,
                format: Some(Format::Json),
            },
        )
        .await
        .unwrap(),
    );
    let job = v["job"].as_str().unwrap();
    assert!(job.starts_with("job:1 task hello running "), "{job}");
    assert!(job.ends_with('s'), "{job}");
    // log job= refuses a running job.
    let e = log::run(
        &f.ctx,
        log::Args {
            recipe: None,
            task: None,
            job: Some("job:1".into()),
            budget_tokens: None,
            format: None,
        },
    )
    .await
    .unwrap_err();
    assert!(e.to_string().contains("still running"), "{e}");

    let pids = fs::read_to_string(&pidfile).unwrap();
    let pids: Vec<&str> = pids.split_whitespace().collect();
    assert!(pids.iter().all(|p| alive(p)), "{pids:?}");

    let started = Instant::now();
    let out = f.call(args(Op::Cancel)).await.unwrap();
    assert!(out.contains("state\tcancelled\n"), "{out}");
    assert!(started.elapsed() < Duration::from_secs(5));
    // The whole group is gone: the fake and its sleep child.
    for p in &pids {
        assert!(!alive(p), "pid {p} survived the cancel");
    }
    let job = f.ctx.jobs.get("job:1").unwrap();
    assert_eq!(job.state, JobState::Cancelled);
    assert!(job.ended.is_some() && job.handle.is_some());

    // After the cancel nothing is running.
    let v = json(
        &status::run(
            &f.ctx,
            status::Args {
                budget_tokens: None,
                format: Some(Format::Json),
            },
        )
        .await
        .unwrap(),
    );
    assert!(v["job"].is_null());
    let e = f.call(args(Op::Cancel)).await.unwrap_err();
    assert!(e.to_string().contains("no job is running"), "{e}");
    let e = f.call(status_of(Some("job:1"))).await.unwrap();
    assert!(e.contains("state\tcancelled"), "{e}");
    let e = f
        .call(Args {
            job: Some("job:1".into()),
            ..args(Op::Cancel)
        })
        .await
        .unwrap_err();
    assert!(e.to_string().contains("not running"), "{e}");
}

#[tokio::test]
async fn cancel_kills_a_job_that_ignores_sigterm() {
    let f = fixture();
    let pidfile = f.root.join("fake.pids");
    f.fake(&[
        ("FAKEBB_SLEEP", "60"),
        ("FAKEBB_PIDFILE", &pidfile.display().to_string()),
        ("FAKEBB_IGNORE_TERM", "1"),
    ]);
    f.call(target(Op::Build, "hello")).await.unwrap();
    wait_for_file(&pidfile).await;
    let pids = fs::read_to_string(&pidfile).unwrap();
    let out = f.call(args(Op::Cancel)).await.unwrap();
    assert!(out.contains("state\tcancelled\n"), "{out}");
    for p in pids.split_whitespace() {
        assert!(!alive(p), "pid {p} survived");
    }
}

/// y1-build: a cancel interrupts a BitBake client twice (SIGINT, as Ctrl-C twice) and lets
/// it exit by itself, because terminating it leaves the BitBake server, outside the job's
/// group, building on (seen live). The fake behaves as knotty and ignores SIGTERM, so it
/// stops in time only if the interrupts reach it.
#[tokio::test]
async fn cancel_interrupts_the_client_twice_and_lets_it_stop() {
    let f = fixture();
    let pidfile = f.root.join("fake.pids");
    f.fake(&[
        ("FAKEBB_ON_INT", "1"),
        ("FAKEBB_PIDFILE", &pidfile.display().to_string()),
    ]);
    f.call(target(Op::Build, "hello")).await.unwrap();
    wait_for_file(&pidfile).await;
    let pid = fs::read_to_string(&pidfile).unwrap();
    let started = Instant::now();
    let out = f.call(args(Op::Cancel)).await.unwrap();
    assert!(out.contains("state\tcancelled\n"), "{out}");
    assert!(started.elapsed() < Duration::from_secs(10));
    assert!(!alive(pid.trim()), "the fake client survived");
    let job = f.ctx.jobs.get("job:1").unwrap();
    let stored = f.ctx.handles.get(&job.handle.unwrap()).unwrap();
    let stored = String::from_utf8_lossy(&stored);
    assert!(
        stored.contains("Keyboard Interrupt, closing down..."),
        "{stored}"
    );
    assert!(
        stored.contains("Second Keyboard Interrupt, stopping..."),
        "{stored}"
    );
    assert!(stored.contains("fakebb: build stopped"), "{stored}");
}

/// The pid of a child that has exited and been reaped.
fn dead_pid() -> u32 {
    let mut c = std::process::Command::new("true").spawn().unwrap();
    let pid = c.id();
    c.wait().unwrap();
    pid
}

#[tokio::test]
async fn the_first_job_removes_output_files_of_dead_servers() {
    let f = fixture();
    let jobs = f.ctx.handles.dir().parent().unwrap().join("jobs");
    fs::create_dir_all(&jobs).unwrap();
    let two_days_ago = std::time::SystemTime::now() - Duration::from_secs(2 * 24 * 3600);
    let put = |name: String| {
        let p = jobs.join(name);
        fs::File::create(&p)
            .unwrap()
            .set_modified(two_days_ago)
            .unwrap();
        p
    };
    // A crashed server's file, and one of a live process (this test's parent).
    let stale = put(format!("{}-7.out", dead_pid()));
    let live = put(format!("{}-7.out", std::os::unix::process::parent_id()));
    f.fake(&[]);
    f.call(target(Op::Build, "hello")).await.unwrap();
    f.wait_done("job:1").await;
    assert!(!stale.exists(), "{}", stale.display());
    assert!(live.exists(), "{}", live.display());
}

#[test]
fn a_running_job_is_killed_when_the_server_runtime_is_dropped() {
    // What happens when the server exits: `serve` returns and `main` drops the runtime,
    // which drops the task supervising the job.
    let f = fixture();
    let pidfile = f.root.join("fake.pids");
    f.fake(&[
        ("FAKEBB_SLEEP", "60"),
        ("FAKEBB_PIDFILE", &pidfile.display().to_string()),
    ]);
    let rt = tokio::runtime::Runtime::new().unwrap();
    rt.block_on(async {
        f.call(target(Op::Build, "hello")).await.unwrap();
        wait_for_file(&pidfile).await;
    });
    let pids = fs::read_to_string(&pidfile).unwrap();
    let pids: Vec<&str> = pids.split_whitespace().collect();
    assert!(pids.iter().all(|p| alive(p)), "{pids:?}");
    drop(rt);
    // SIGKILL was sent to the group; nothing waited, so poll.
    let deadline = Instant::now() + Duration::from_secs(10);
    while pids.iter().any(|p| alive(p)) {
        assert!(Instant::now() < deadline, "{pids:?} survived the runtime");
        std::thread::sleep(Duration::from_millis(20));
    }
}

#[tokio::test]
async fn one_job_at_a_time() {
    let f = fixture();
    let pidfile = f.root.join("fake.pids");
    f.fake(&[
        ("FAKEBB_SLEEP", "60"),
        ("FAKEBB_PIDFILE", &pidfile.display().to_string()),
    ]);
    f.call(target(Op::Build, "hello")).await.unwrap();
    wait_for_file(&pidfile).await;

    // A second build, a task, env and layers all get the running job back.
    let mut task = target(Op::Task, "libfoo");
    task.task = Some("compile".into());
    for a in [
        target(Op::Build, "core-image-minimal"),
        task,
        target(Op::Clean, "hello"),
        args(Op::Env),
        args(Op::Layers),
    ] {
        let out = f.call(a).await.unwrap();
        assert!(out.contains("job\tjob:1\n"), "{out}");
        assert!(out.contains("already running"), "{out}");
        assert!(out.contains("target\thello\n"), "{out}");
    }
    assert_eq!(f.ctx.jobs.list().len(), 1);

    f.call(args(Op::Cancel)).await.unwrap();
    // Once it has ended, env and layers run (one after the other).
    f.fake(&[]);
    let out = f.call(target(Op::Env, "hello")).await.unwrap();
    assert!(out.contains("job\tjob:2\n"), "{out}");
    f.wait_done("job:2").await;
    let v = json(
        &f.call(Args {
            format: Some(Format::Json),
            ..status_of(None)
        })
        .await
        .unwrap(),
    );
    assert_eq!(v["job"]["kind"], "env");
    assert_eq!(v["job"]["command"], "bitbake -e hello");
    assert!(v["notes"][0].as_str().unwrap().contains("var=NAME"), "{v}");
    let out = f
        .call(with_args(args(Op::Layers), &["show-layers"]))
        .await
        .unwrap();
    assert!(out.contains("job\tjob:3\n"), "{out}");
    f.wait_done("job:3").await;
    let job = f.call(status_of(None)).await.unwrap();
    assert!(
        job.contains("command\tbitbake-layers show-layers\n"),
        "{job}"
    );
    assert!(job.contains("state\tsucceeded\n"), "{job}");
    assert_eq!(f.ctx.jobs.get("job:3").unwrap().kind, HandleKind::Text);
}

// --- destructive gates -----------------------------------------------------------------

#[tokio::test]
async fn destructive_operations_need_confirm_and_config() {
    let cases: [(&str, Args); 4] = [
        (
            "bitbake -c cleansstate hello",
            with_args(target(Op::Clean, "hello"), &["cleansstate"]),
        ),
        (
            "bitbake -c cleanall hello",
            with_args(target(Op::Clean, "hello"), &["cleanall"]),
        ),
        (
            "devtool reset hello",
            with_args(target(Op::Devtool, "hello"), &["reset"]),
        ),
        (
            "devtool finish hello meta-mine",
            with_args(target(Op::Devtool, "hello"), &["finish", "meta-mine"]),
        ),
    ];

    // Not allowed in the configuration (the default).
    let f = fixture();
    for (cmd, a) in &cases {
        let e = f.call(a.clone()).await.unwrap_err();
        let msg = e.to_string();
        assert!(msg.contains("confirm: true"), "{msg}");
        assert!(msg.contains("allow_destructive"), "{msg}");
        let e = f
            .call(Args {
                confirm: true,
                ..a.clone()
            })
            .await
            .unwrap_err();
        let msg = tool_error_message("bitbake", &e);
        assert!(msg.contains(cmd), "{msg}");
        assert!(msg.contains("allow_destructive = true"), "{msg}");
        assert!(msg.contains("[bitbake]"), "{msg}");
    }
    // `clean` itself is not destructive.
    assert!(
        check_destructive(
            &f.ctx.config,
            &plan(&f.ctx.config, &f.ctx.workspace, &target(Op::Clean, "hello")).unwrap(),
            false
        )
        .is_ok()
    );
    assert!(f.ctx.jobs.list().is_empty());

    // Allowed in the configuration: confirm is still required, then the job runs.
    let f = fixture_with(|c| c.bitbake.allow_destructive = true);
    for (n, (cmd, a)) in cases.iter().enumerate() {
        let e = f.call(a.clone()).await.unwrap_err();
        assert!(e.to_string().contains("confirm: true"), "{e}");
        let out = f
            .call(Args {
                confirm: true,
                ..a.clone()
            })
            .await
            .unwrap();
        let id = format!("job:{}", n + 1);
        assert!(out.contains(&format!("job\t{id}\n")), "{out}");
        assert!(out.contains(&format!("command\t{cmd}\n")), "{out}");
        f.wait_done(&id).await;
    }
    assert_eq!(f.ctx.jobs.list().len(), 4);
}

#[tokio::test]
async fn no_environment_is_an_error() {
    let f = fixture_with(|c| c.bitbake.enabled = false);
    let e = f.call(target(Op::Build, "hello")).await.unwrap_err();
    assert!(e.to_string().contains("enabled = false"), "{e}");
    let e = f.call(status_of(None)).await.unwrap_err();
    assert!(matches!(e, Error::NotFound { .. }), "{e}");
}

// --- release-dependent work directory layout --------------------------------------------

#[test]
fn version_directory_follows_the_release() {
    let ws = |release| Workspace {
        release: ReleaseInfo {
            release,
            source: ReleaseSource::Config,
            note: None,
        },
        ..Workspace::default()
    };
    let kirkstone = ws(Release::Kirkstone);
    let mickledore = ws(Release::Mickledore);
    let nanbield = ws(Release::Nanbield);
    let wrynose = ws(Release::Wrynose);

    let old = Path::new("/b/tmp/work/core2-64-poky-linux/hello/1.0-r0/temp/log.do_compile.1");
    let m = meta_from_path_in(old, Some(&kirkstone));
    assert_eq!(m.recipe.as_deref(), Some("hello"));
    assert_eq!(m.version.as_deref(), Some("1.0"));
    assert_eq!(m.task.as_deref(), Some("do_compile"));
    assert_eq!(
        meta_from_path_in(old, Some(&mickledore)).version.as_deref(),
        Some("1.0")
    );
    // With EXTENDPE (`${PE}_`).
    let pe = Path::new("/b/tmp/work/x86_64-linux/foo/2_1.2.3-r5/temp/log.do_install");
    assert_eq!(
        meta_from_path_in(pe, Some(&kirkstone)).version.as_deref(),
        Some("1.2.3")
    );
    // From nanbield the directory is PV, taken as it is.
    let new = Path::new("/b/tmp/work/core2-64-poky-linux/hello/1.0/temp/log.do_compile.1");
    assert_eq!(
        meta_from_path_in(new, Some(&nanbield)).version.as_deref(),
        Some("1.0")
    );
    assert_eq!(
        meta_from_path_in(new, Some(&wrynose)).version.as_deref(),
        Some("1.0")
    );
    let git = Path::new("/b/tmp/work/core2-64-poky-linux/foo/1.0+git-r0/temp/log.do_fetch");
    assert_eq!(
        meta_from_path_in(git, Some(&wrynose)).version.as_deref(),
        Some("1.0+git-r0")
    );
    assert_eq!(
        meta_from_path_in(git, Some(&kirkstone)).version.as_deref(),
        Some("1.0+git")
    );
    // No workspace: lexical, as before.
    assert_eq!(meta_from_path(old).version.as_deref(), Some("1.0-r0"));
    assert_eq!(
        meta_from_path_in(old, None).version.as_deref(),
        Some("1.0-r0")
    );
    // The digest uses the workspace release.
    let d = sous_chef::bitbake::logs::digest(&sous_chef::bitbake::logs::DigestInput {
        text: "ERROR: oe_runmake failed\n",
        log_path: Some(old),
        ws: Some(&kirkstone),
        ..Default::default()
    });
    assert_eq!(d.meta.version.as_deref(), Some("1.0"));
}

// --- one job at a time: queueing behind a read-only job (w10) ----------------------------

fn fake_env(f: &Fixture) -> BbEnv {
    BbEnv::probe(&f.ctx.config, &f.ctx.workspace, &HostEnv::from_process()).unwrap()
}

fn spec(op: &str, program: &str, args: &[&str]) -> JobSpec {
    JobSpec {
        op: op.into(),
        target: None,
        program: program.into(),
        args: args.iter().map(|s| s.to_string()).collect(),
        kind: HandleKind::Text,
    }
}

#[tokio::test]
async fn an_operation_behind_a_read_only_job_waits_for_it_then_starts() {
    let f = fixture();
    let pidfile = f.root.join("fake.pids");
    f.fake(&[
        ("FAKEBB_SLEEP", "2"),
        ("FAKEBB_PIDFILE", &pidfile.display().to_string()),
    ]);
    let out = f.call(args(Op::Layers)).await.unwrap();
    assert!(out.contains("job\tjob:1\n"), "{out}");
    wait_for_file(&pidfile).await;
    f.fake(&[]);
    // `bitbake -e` while `bitbake-layers` runs: waits for it, then starts.
    let asked = Instant::now();
    let out = f.call(target(Op::Env, "hello")).await.unwrap();
    assert!(out.contains("job\tjob:2\n"), "{out}");
    assert!(out.contains("started;"), "{out}");
    assert!(
        asked.elapsed() >= Duration::from_millis(500),
        "did not wait"
    );
    let first = f.ctx.jobs.get("job:1").unwrap();
    assert_eq!(first.state, JobState::Succeeded, "{first:?}");
    f.wait_done("job:2").await;
}

#[tokio::test]
async fn an_operation_behind_a_build_is_refused_at_once_saying_why() {
    let f = fixture();
    let pidfile = f.root.join("fake.pids");
    f.fake(&[
        ("FAKEBB_SLEEP", "60"),
        ("FAKEBB_PIDFILE", &pidfile.display().to_string()),
    ]);
    f.call(target(Op::Build, "hello")).await.unwrap();
    wait_for_file(&pidfile).await;
    for a in [args(Op::Env), args(Op::Layers), target(Op::Build, "libfoo")] {
        let asked = Instant::now();
        let out = f.call(a).await.unwrap();
        assert!(asked.elapsed() < Duration::from_secs(5), "it waited");
        assert!(out.contains("job\tjob:1\n"), "{out}");
        assert!(
            out.contains("job:1 (build hello) is already running (one job at a time)")
                && out.contains("gives up after about 20 s"),
            "{out}"
        );
    }
    assert_eq!(f.ctx.jobs.list().len(), 1);
    f.call(args(Op::Cancel)).await.unwrap();
}

#[tokio::test]
async fn queueing_gives_up_after_its_wait_and_never_runs_two_jobs() {
    let f = fixture();
    let env = fake_env(&f);
    let dir = f.ctx.jobs_dir();
    let pidfile = f.root.join("fake.pids");
    f.fake(&[
        ("FAKEBB_SLEEP", "60"),
        ("FAKEBB_PIDFILE", &pidfile.display().to_string()),
    ]);
    let first = match f
        .ctx
        .jobs
        .start(
            &env,
            spec("layers", "bitbake-layers", &["show-layers"]),
            &dir,
        )
        .unwrap()
    {
        Started::Started(j) => j,
        other => panic!("{other:?}"),
    };
    assert!(first.is_read_only());
    wait_for_file(&pidfile).await;
    // Still running after the wait: the running job comes back, nothing started.
    let mut ticks = 0;
    let r = f
        .ctx
        .jobs
        .start_or_queue(
            &env,
            spec("env", "bitbake", &["-e"]),
            &dir,
            Duration::from_millis(300),
            |j| {
                assert_eq!(j.id, first.id);
                ticks += 1;
            },
        )
        .await
        .unwrap();
    assert!(
        matches!(&r, Started::AlreadyRunning(j) if j.id == first.id),
        "{r:?}"
    );
    assert!(ticks > 0);
    assert_eq!(f.ctx.jobs.list().len(), 1);
    f.ctx.jobs.cancel(&first.id).unwrap();
    f.wait_done(&first.id).await;

    // Two callers queued behind a short read-only job: each starts in turn, never both.
    f.fake(&[("FAKEBB_SLEEP", "1")]);
    let short = f
        .ctx
        .jobs
        .start(&env, spec("env", "bitbake", &["-e"]), &dir)
        .unwrap();
    assert!(matches!(short, Started::Started(_)));
    let queue = |op: &'static str| {
        let jobs = f.ctx.jobs.clone();
        let env = env.clone();
        let dir = dir.clone();
        let running_seen = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        async move {
            let r = jobs
                .start_or_queue(
                    &env,
                    spec(op, "bitbake", &["-e"]),
                    &dir,
                    Duration::from_secs(20),
                    |_| {
                        let n = jobs
                            .list()
                            .iter()
                            .filter(|j| j.state == JobState::Running)
                            .count();
                        running_seen.fetch_max(n, std::sync::atomic::Ordering::Relaxed);
                    },
                )
                .await
                .unwrap();
            (r, running_seen.load(std::sync::atomic::Ordering::Relaxed))
        }
    };
    let ((a, seen_a), (b, seen_b)) = tokio::join!(queue("env"), queue("layers"));
    assert!(matches!(a, Started::Started(_)), "{a:?}");
    assert!(matches!(b, Started::Started(_)), "{b:?}");
    assert!(seen_a <= 1 && seen_b <= 1, "two jobs ran at once");
    let ids: Vec<String> = f.ctx.jobs.list().into_iter().map(|j| j.id).collect();
    assert_eq!(ids.len(), 4, "{ids:?}");
    for id in &ids {
        f.wait_done(id).await;
    }
}

#[tokio::test]
async fn wait_answers_once_the_job_has_ended() {
    let f = fixture();
    f.fake(&[("FAKEBB_SLEEP", "1")]);
    // A start with wait: the answer is the finished job, its output stored.
    let out = f
        .call(Args {
            wait: Some(30),
            ..args(Op::Layers)
        })
        .await
        .unwrap();
    assert!(out.contains("state\tsucceeded\n"), "{out}");
    assert!(out.contains("handle\th:"), "{out}");
    assert!(!out.contains("started; poll"), "{out}");
    // A wait shorter than the job: still running, with the poll hint.
    let out = f
        .call(Args {
            wait: Some(0),
            ..args(Op::Layers)
        })
        .await
        .unwrap();
    assert!(out.contains("state\trunning\n"), "{out}");
    assert!(out.contains("started; poll"), "{out}");
    // status with wait: the same job, finished.
    let out = f
        .call(Args {
            wait: Some(30),
            ..status_of(Some("job:2"))
        })
        .await
        .unwrap();
    assert!(
        out.contains("job\tjob:2\n") && out.contains("state\tsucceeded\n"),
        "{out}"
    );
    // Too long a wait is refused before anything runs.
    let e = f
        .call(Args {
            wait: Some(301),
            ..args(Op::Layers)
        })
        .await
        .unwrap_err();
    assert!(e.to_string().contains("wait=301 is too long"), "{e}");
    assert_eq!(f.ctx.jobs.list().len(), 2);
}
