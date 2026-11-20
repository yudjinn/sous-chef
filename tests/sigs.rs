//! The `sigs` tool on the fixture workspace, with a fake `bitbake-diffsigs`
//! (`tests/fixtures/fakebb/bin/bitbake-diffsigs`, a link to the fake bitbake) that prints
//! recorded output. The recorded outputs (`tests/fixtures/outputs/sigs/`) are hand-written
//! from the format of BitBake's `compare_sigfiles` (`lib/bb/siggen.py`, BitBake 2.18; the
//! messages are the same on 2.0 and 2.8) and `bin/bitbake-diffsigs` (recursion indented
//! by four spaces), not recorded from a build. The real outputs of the live builds are in
//! `tests/fixtures/outputs/live/` (y1-build: a change of curl's own inputs, checked in
//! `tests/live_build.rs`; real-checks: a change reaching curl through zlib, also replayed
//! through the tool here).
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::future::Future;
use std::path::PathBuf;
use std::pin::Pin;

use sous_chef::bitbake::JobTable;
use sous_chef::bitbake::sigs::{DiffSigs, RunOutput};
use sous_chef::handles::HandleStore;
use sous_chef::index::Index;
use sous_chef::mcp::Ctx;
use sous_chef::mcp::tools::sigs::{self, Args};
use sous_chef::workspace::Workspace;

/// Runs the fake script with the recorded output, and drops the lines the fake prints about
/// its own arguments (after checking them).
struct Fake {
    stdout: Option<&'static str>,
    stderr: Option<&'static str>,
    exit: i32,
    expect_args: Vec<String>,
}

fn fixture(rel: &str) -> PathBuf {
    common::fixture_path(rel)
}

impl DiffSigs for Fake {
    fn diffsigs<'a>(
        &'a self,
        args: &'a [String],
    ) -> Pin<Box<dyn Future<Output = sous_chef::error::Result<RunOutput>> + Send + 'a>> {
        Box::pin(async move {
            let mut cmd = tokio::process::Command::new(fixture("fakebb/bin/bitbake-diffsigs"));
            cmd.args(args).env("FAKEBB_EXIT", self.exit.to_string());
            if let Some(o) = self.stdout {
                // A recorded live output is named by its path under `outputs/`.
                let rel = if o.starts_with("live/") {
                    format!("outputs/{o}")
                } else {
                    format!("outputs/sigs/{o}")
                };
                cmd.env("FAKEBB_OUTPUT", fixture(&rel));
            }
            if let Some(e) = self.stderr {
                let text = std::fs::read_to_string(fixture(&format!("outputs/sigs/{e}"))).unwrap();
                cmd.env("FAKEBB_STDERR", text.trim_end());
            }
            let out = cmd.output().await?;
            let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
            let mut seen = Vec::new();
            let mut kept = String::new();
            for l in stdout.lines() {
                if let Some(a) = l.strip_prefix("arg: ") {
                    seen.push(a.to_string());
                } else if l.starts_with("fakebb: ")
                    || l.starts_with("cwd: ")
                    || l.starts_with("BUILDDIR: ")
                    || l.starts_with("argc: ")
                {
                } else {
                    kept.push_str(l);
                    kept.push('\n');
                }
            }
            assert_eq!(seen, self.expect_args);
            Ok(RunOutput {
                status: out.status.code(),
                stdout: kept,
                stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
                timed_out: false,
            })
        })
    }
}

struct Fx {
    _cache: tempfile::TempDir,
    ctx: Ctx,
}

fn ctx() -> Fx {
    ctx_with(common::fixture_config())
}

fn ctx_with(config: sous_chef::config::Config) -> Fx {
    let cache = tempfile::tempdir().unwrap();
    let workspace = Workspace::discover(&config).unwrap();
    let mut index = Index::open_in(cache.path(), &workspace).unwrap();
    index.refresh(&workspace).unwrap();
    let handles = HandleStore::open(&index.cache_dir().join("handles")).unwrap();
    Fx {
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

fn task_args(recipe: &str, task: &str) -> Args {
    Args {
        recipe: Some(recipe.into()),
        task: Some(task.into()),
        sigfile1: None,
        sigfile2: None,
        budget_tokens: None,
        format: None,
    }
}

fn t_args(pn: &str, task: &str) -> Vec<String> {
    ["-c", "never", "-t", pn, task]
        .iter()
        .map(|s| s.to_string())
        .collect()
}

/// The handle id varies with nothing but the output; mask it so snapshots read well.
fn mask(out: &str) -> String {
    let re = regex::Regex::new(r"h:[0-9a-f]{10}").unwrap();
    re.replace_all(out, "h:HANDLE").into_owned()
}

#[tokio::test]
async fn variable_change_through_a_dependency() {
    let f = ctx();
    let fake = Fake {
        stdout: Some("diffsigs-hello-compile.out"),
        stderr: None,
        exit: 0,
        expect_args: t_args("hello", "do_compile"),
    };
    let out = sigs::run_with(&f.ctx, task_args("hello", "compile"), Some(&fake), None)
        .await
        .unwrap();
    assert!(out.len() < 1100, "{} bytes:\n{out}", out.len());
    insta::assert_snapshot!(mask(&out));
    // The variable maps to the assignment that added the new word.
    assert!(
        out.contains("cause\tEXTRA_OECONF (hello)\tvalue changed: \"--disable-nls {+--with-mine+}\"\tmeta-mine:recipes-core/hello/hello_1.1.bbappend:6"),
        "{out}"
    );
    assert!(out.contains("by way of hello:do_configure"), "{out}");
    // The full output is under the handle.
    let h = regex::Regex::new(r"h:[0-9a-f]{10}")
        .unwrap()
        .find(&out)
        .unwrap()
        .as_str()
        .to_string();
    let whole = f.ctx.handles.get(&sous_chef::handles::Handle(h)).unwrap();
    assert!(String::from_utf8_lossy(&whole).contains("Variable do_configure value changed:"));
}

#[tokio::test]
async fn kirkstone_names_and_deep_recursion() {
    let f = ctx();
    let fake = Fake {
        stdout: Some("diffsigs-kirkstone-patch.out"),
        stderr: None,
        exit: 0,
        expect_args: t_args("hello", "do_configure"),
    };
    let mut a = task_args("hello", "do_configure");
    a.budget_tokens = Some(2000);
    let out = sigs::run_with(&f.ctx, a, Some(&fake), None).await.unwrap();
    insta::assert_snapshot!(mask(&out));
    assert!(
        out.contains("via\thello/hello_1.1.bb:do_fetch\thash eeeeeeeeee -> ffffffffff"),
        "{out}"
    );
    assert!(
        out.contains("cause\tSRC_URI (hello)\tvalue changed: \"https://example.org/releases/hello-${PV}.tar.gz file://hello.patch {+file://mine.patch+}\"\tmeta-mine:recipes-core/hello/hello_1.1.bbappend:4"),
        "{out}"
    );
}

#[tokio::test]
async fn two_files_own_inputs() {
    let f = ctx();
    let root = common::fixture_ws_root();
    let a = root.join("meta-core/recipes-core/hello/hello_1.0.bb");
    let b = root.join("meta-core/recipes-core/hello/hello_1.1.bb");
    let fake = Fake {
        stdout: Some("diffsigs-files.out"),
        stderr: None,
        exit: 0,
        expect_args: vec![
            "-c".into(),
            "never".into(),
            a.canonicalize().unwrap().display().to_string(),
            b.canonicalize().unwrap().display().to_string(),
        ],
    };
    let args = Args {
        recipe: None,
        task: None,
        sigfile1: Some(a.display().to_string()),
        sigfile2: Some(b.display().to_string()),
        budget_tokens: Some(2000),
        format: None,
    };
    let out = sigs::run_with(&f.ctx, args, Some(&fake), None)
        .await
        .unwrap();
    insta::assert_snapshot!(mask(&out));
    assert!(
        out.contains("differs: its own inputs changed (basehash)"),
        "{out}"
    );
    assert!(
        out.contains(
            "variables\tthe task's variable dependencies changed: added EXTRA_OEMAKE; removed none"
        ),
        "{out}"
    );
    assert!(out.contains("cause\ttaint\t"), "{out}");
}

#[tokio::test]
async fn only_one_signature() {
    let f = ctx();
    let fake = Fake {
        stdout: None,
        stderr: Some("diffsigs-only-one.err"),
        exit: 1,
        expect_args: t_args("hello", "do_compile"),
    };
    let out = sigs::run_with(&f.ctx, task_args("hello", "do_compile"), Some(&fake), None)
        .await
        .unwrap();
    assert!(
        out.contains("task\thello:do_compile\tonly one signature: nothing to compare"),
        "{out}"
    );
    assert!(
        out.contains("note: the stamps directory holds 0 signature files for this task"),
        "{out}"
    );
}

#[tokio::test]
async fn no_runner_says_so_and_runs_nothing() {
    let f = ctx();
    let out = sigs::run(&f.ctx, task_args("hello", "compile"))
        .await
        .unwrap();
    assert!(
        out.contains("task\thello:do_compile\tnot compared\t-"),
        "{out}"
    );
    assert!(
        out.contains("bitbake is disabled in sous-chef.toml ([bitbake] enabled = false)"),
        "{out}"
    );
    assert!(
        out.contains("bitbake-diffsigs -t hello do_compile"),
        "{out}"
    );
}

#[tokio::test]
async fn bad_arguments() {
    let f = ctx();
    let mut a = task_args("hello", "compile");
    a.task = None;
    assert!(sigs::run(&f.ctx, a).await.is_err());
    let a = Args {
        recipe: None,
        task: None,
        sigfile1: Some("/etc/passwd".into()),
        sigfile2: Some("/etc/hosts".into()),
        budget_tokens: None,
        format: None,
    };
    let e = sigs::run(&f.ctx, a).await.unwrap_err().to_string();
    assert!(e.contains("outside the workspace"), "{e}");
    let a = task_args("../hello", "compile");
    assert!(sigs::run(&f.ctx, a).await.is_err());
}

#[test]
fn stamp_signature_files_are_counted() {
    let (_tmp, root) = common::temp_ws();
    let dir = root.join("build/tmp/stamps/armv7at2hf-neon-oe-linux-gnueabi/curl");
    std::fs::create_dir_all(&dir).unwrap();
    for n in [
        "8.19.0.do_fetch.sigdata.fd00",
        "8.19.0.do_fetch.fd00",
        "8.19.0.do_fetch.sigdata.ab12",
        "8.19.0.do_compile.sigdata.0001",
    ] {
        std::fs::write(dir.join(n), "x").unwrap();
    }
    let found =
        sous_chef::bitbake::sigs::stamp_sigfiles(&root.join("build"), "curl", "do_fetch", |_| true);
    assert_eq!(found.len(), 2, "{found:?}");
}

/// The real `bitbake-diffsigs -t curl do_configure` of real-checks (2026-11-20): curl
/// re-ran because zlib's `CFLAGS` changed (`CFLAGS:append:class-target = " -O1"` in a
/// scratch `zlib_%.bbappend`), five dependencies down. Replayed on a copy of the fixture
/// workspace with a zlib recipe and that bbappend: the chain as BitBake printed it, the
/// cause attributed to zlib and pointed at the bbappend's line, and no claim that curl's
/// own inputs did not change (BitBake's output cannot say: zlib's do_compile, whose own
/// basehash changed, printed only its dependency).
#[tokio::test]
async fn real_change_through_a_dependency_points_at_the_dependencys_bbappend() {
    let (_tmp, root) = common::temp_ws();
    let zlib = root.join("meta-core/recipes-core/zlib");
    std::fs::create_dir_all(&zlib).unwrap();
    std::fs::write(
        zlib.join("zlib_1.3.2.bb"),
        concat!(
            "SUMMARY = \"Zlib Compression Library\"\n",
            "LICENSE = \"Zlib\"\n",
            "CFLAGS += \"-D_REENTRANT -fPIE\"\n",
            "BBCLASSEXTEND = \"native nativesdk\"\n"
        ),
    )
    .unwrap();
    let app = root.join("meta-mine/recipes-core/zlib");
    std::fs::create_dir_all(&app).unwrap();
    std::fs::copy(
        fixture("outputs/live/real-checks/sigs/zlib_%.bbappend.txt"),
        app.join("zlib_%.bbappend"),
    )
    .unwrap();
    let f = ctx_with(common::ws_config(&root));
    let fake = Fake {
        stdout: Some("live/real-checks/sigs/diffsigs-curl-do_configure.out"),
        stderr: None,
        exit: 0,
        expect_args: t_args("curl", "do_configure"),
    };
    let mut a = task_args("curl", "configure");
    a.budget_tokens = Some(2000);
    let out = sigs::run_with(&f.ctx, a, Some(&fake), None).await.unwrap();
    assert!(
        out.contains(concat!(
            "task\tcurl:do_configure\tre-ran: a dependency's hash changed, by way of ",
            "curl:do_prepare_recipe_sysroot -> zlib:do_populate_sysroot -> zlib:do_install ",
            "-> zlib:do_compile -> zlib:do_configure\t-"
        )),
        "{out}"
    );
    assert!(!out.contains("its own inputs did not change"), "{out}");
    assert!(
        out.contains("via\tzlib:do_configure\thash 5df8f861e8 -> c54a2de5c5\t-"),
        "{out}"
    );
    assert_eq!(out.matches("\nvia\t").count(), 5, "{out}");
    assert!(
        out.contains(concat!(
            "cause\tCFLAGS (zlib)\tvalue changed: \"${TARGET_CFLAGS} -D_REENTRANT -fPIE {+-O1+}\"",
            "\tmeta-mine:recipes-core/zlib/zlib_%.bbappend:3"
        )),
        "{out}"
    );
    assert!(
        out.contains("note: bitbake-diffsigs shows a changed dependency's comparison instead of the task's own"),
        "{out}"
    );
    assert!(!out.contains("not recognised"), "{out}");

    // curl's do_install, seven dependencies deep, at the default budget: the cause comes
    // before the path, so a budget cut drops `via` rows, never the cause (live, with the
    // path first, the cause was the row cut).
    let fake = Fake {
        stdout: Some("live/real-checks/sigs/diffsigs-curl-do_install.out"),
        stderr: None,
        exit: 0,
        expect_args: t_args("curl", "do_install"),
    };
    let out = sigs::run_with(&f.ctx, task_args("curl", "install"), Some(&fake), None)
        .await
        .unwrap();
    assert!(
        out.contains("by way of curl:do_compile -> curl:do_configure -> "),
        "{out}"
    );
    let cause = out.find("\ncause\tCFLAGS (zlib)\t").expect(&out);
    let via = out.find("\nvia\t").expect(&out);
    assert!(cause < via, "{out}");
    assert!(out.contains("zlib_%.bbappend:3"), "{out}");
}
