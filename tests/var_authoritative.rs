//! T5.5: `var` with `authoritative: true`, against the fake `bitbake` in
//! `tests/fixtures/fakebb/` printing the recorded `bitbake -e curl` output
//! (`tests/fixtures/outputs/envdump/`) in a workspace laid out like the environment it was
//! recorded in. Never a real BitBake.
#![cfg(unix)]
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod envdump_support;

use std::fs;
use std::path::Path;
use std::time::Duration;

use sous_chef::bitbake::{JobSpec, JobTable};
use sous_chef::error::Error;
use sous_chef::handles::{Handle, HandleKind, HandleStore};
use sous_chef::index::Index;
use sous_chef::mcp::Ctx;
use sous_chef::mcp::tools::{slice, var, var_authoritative};
use sous_chef::workspace::Workspace;

use envdump_support::LiveWs;

struct Fixture {
    _tmp: tempfile::TempDir,
    cache: tempfile::TempDir,
    live: LiveWs,
    ctx: Ctx,
}

fn ctx_for(live: &LiveWs, cache: &Path) -> Ctx {
    let workspace = Workspace::discover(&live.config).unwrap();
    let index = Index::open_in(cache, &workspace).unwrap();
    let handles = HandleStore::open(&index.cache_dir().join("handles")).unwrap();
    Ctx {
        config: live.config.clone(),
        workspace,
        index: tokio::sync::Mutex::new(index),
        handles,
        jobs: JobTable::new(),
    }
}

fn fixture() -> Fixture {
    let tmp = tempfile::tempdir().unwrap();
    let live = LiveWs::new(tmp.path());
    let dump = live.write_dump();
    live.fake(&[("FAKEBB_OUTPUT", &dump.display().to_string())]);
    let cache = tempfile::tempdir().unwrap();
    let ctx = ctx_for(&live, cache.path());
    Fixture {
        _tmp: tmp,
        cache,
        live,
        ctx,
    }
}

fn args(name: &str, recipe: Option<&str>) -> var::Args {
    var::Args {
        name: name.into(),
        recipe: recipe.map(str::to_string),
        machine: None,
        distro: None,
        authoritative: true,
        flags: false,
        budget_tokens: Some(8000),
        format: None,
    }
}

async fn ok(f: &Fixture, a: var::Args) -> String {
    var::run(&f.ctx, a).await.unwrap()
}

async fn err(f: &Fixture, a: var::Args) -> String {
    let e = var::run(&f.ctx, a).await.unwrap_err();
    e.to_string()
}

fn rows(out: &str) -> Vec<Vec<String>> {
    out.lines()
        .skip(1)
        .take_while(|l| l.contains('\t'))
        .map(|l| l.split('\t').map(str::to_string).collect())
        .collect()
}

fn handle_in(out: &str, prefix: &str) -> Handle {
    let at = out.find(prefix).unwrap() + prefix.len();
    let id: String = out[at..]
        .chars()
        .take_while(|c| c.is_ascii_alphanumeric() || *c == ':')
        .collect();
    id.parse().unwrap()
}

#[tokio::test]
async fn src_uri_from_bitbake_in_the_static_table_format() {
    let f = fixture();
    let out = ok(&f, args("SRC_URI", Some("curl"))).await;
    let mut lines = out.lines();
    assert_eq!(lines.next(), Some("status\top\toverride\tloc\tsnippet"));
    let r = rows(&out);
    assert_eq!(r.len(), 3, "{out}");
    assert_eq!(
        r[0][..4],
        ["active", "=", "-", "meta:conf/bitbake.conf:745"]
    );
    assert_eq!(r[0][4], "SRC_URI = \"\"");
    assert_eq!(
        r[1][..4],
        [
            "active",
            "=",
            "-",
            "meta:recipes-support/curl/curl_8.19.0.bb:42"
        ]
    );
    assert!(r[1][4].starts_with("SRC_URI = \"https://curl.se/download/${BP}.tar.xz"));
    // `:append:class-nativesdk` is inactive: class-nativesdk is not in BitBake's OVERRIDES.
    assert_eq!(
        r[2][..4],
        [
            "inactive",
            "=",
            "append:class-nativesdk",
            "meta:recipes-support/curl/curl_8.19.0.bb:46"
        ]
    );
    assert!(r[2][4].starts_with("SRC_URI:append:class-nativesdk = "));
    // The value is longer than 600 characters: cut, with the full text in a value handle.
    let fin = out.lines().find(|l| l.starts_with("final: ")).unwrap();
    assert!(fin.starts_with("final: \"https://curl.se/download/curl-8.19.0.tar.xz "));
    let vh = handle_in(fin, "chars; slice handle=");
    assert_eq!(f.ctx.handles.meta(&vh).unwrap().kind, HandleKind::Value);
    let full = String::from_utf8(f.ctx.handles.get(&vh).unwrap()).unwrap();
    assert!(full.starts_with("https://curl.se/download/curl-8.19.0.tar.xz"));
    assert!(full.chars().count() > 600);

    // The source line names BitBake and the env handle; the handle holds the run's output.
    let src = out.lines().find(|l| l.starts_with("source: ")).unwrap();
    assert!(
        src.starts_with("source: BitBake (bitbake -e curl), handle h:"),
        "{src}"
    );
    assert!(!src.contains("reused"));
    let h = handle_in(src, "handle ");
    let meta = f.ctx.handles.meta(&h).unwrap();
    assert_eq!(meta.kind, HandleKind::Env);
    assert_eq!(meta.command, "bitbake -e curl");
    let text = String::from_utf8(f.ctx.handles.get(&h).unwrap()).unwrap();
    assert!(
        text.contains("argc: 2\narg: -e\narg: curl\n"),
        "{text:.200}"
    );
    // The fixture's cut markers are the only unrecognised lines.
    assert!(
        out.contains("note: 45 line(s) of the dump were not recognised"),
        "{out}"
    );
    assert!(!out.contains("global configuration only"));

    // `slice var=` reads the same block from the same handle.
    let sl = slice::run(
        &f.ctx,
        slice::Args {
            handle: h.0.clone(),
            lines: None,
            grep: None,
            context: None,
            var: Some("SRC_URI".into()),
            failures: None,
            page: None,
            budget_tokens: None,
            format: None,
        },
    )
    .await
    .unwrap();
    assert!(sl.contains("# $SRC_URI [6 operations]"), "{sl}");
    assert!(sl.contains("SRC_URI=\"https://curl.se/download/curl-8.19.0.tar.xz"));
    assert!(!sl.contains("SRC_URI_OVERRIDES_PACKAGE_ARCH"));
}

#[tokio::test]
async fn flags_functions_exports_and_renames() {
    let f = fixture();
    let mut a = args("SRC_URI", Some("curl"));
    a.flags = true;
    let out = ok(&f, a).await;
    assert!(out.contains("\t[sha256sum]\t"), "{out}");
    assert!(out.contains(
        "final[sha256sum]: \"4eb41489790d19e190d7ac7e18e82857cdd68af8f4e66b292ced562d333f11df\""
    ));
    assert!(out.contains("final[vardepsexclude]: not printed by bitbake -e and not derivable"));
    assert!(out.contains("final[doc]: \"The list of source files"));

    let out = ok(&f, args("PACKAGECONFIG[openssl]", Some("curl"))).await;
    let r = rows(&out);
    assert_eq!(r.len(), 1, "{out}");
    assert_eq!(r[0][2], "[openssl]");
    assert!(out.contains("final[openssl]: \"--with-openssl,--without-openssl,openssl\""));

    // `??=` shows as such.
    let out = ok(&f, args("FILE_LAYERNAME", Some("curl"))).await;
    let r = rows(&out);
    assert_eq!(r[0][1], "??=");
    assert_eq!(r[0][3], "meta:conf/bitbake.conf:801");
    // An operation made by python code: BitBake's own name and file.
    assert_eq!(r[1][1], "set");
    assert_eq!(r[1][3], "cookerdata.py:513");
    assert!(r[1][4].starts_with("_parse_recipe: FILE_LAYERNAME set \"core\""));
    assert!(out.contains("final: \"core\""));

    let out = ok(&f, args("do_compile", Some("curl"))).await;
    let r = rows(&out);
    assert_eq!(r[0][..4], ["active", "shell", "-", "autogenerated:1"]);
    assert!(out.contains("final: shell function do_compile, 2 lines"));
    let out = ok(&f, args("autotools_do_compile", Some("curl"))).await;
    assert_eq!(
        rows(&out)[0][3],
        "meta:classes-recipe/autotools.bbclass:227"
    );
    let out = ok(&f, args("base_do_fetch", Some("curl"))).await;
    assert_eq!(rows(&out)[0][..4], ["active", "python", "-", "-"]);

    let out = ok(&f, args("AR", Some("curl"))).await;
    assert!(out.contains("note: exported to task environments"));
    let out = ok(&f, args("SHELL", Some("curl"))).await;
    assert!(out.contains("final: not printed (unexported); pre-expansion value: \"/usr/bin/zsh\""));
    let out = ok(&f, args("ALLOW_EMPTY", Some("curl"))).await;
    assert!(out.contains("final: ALLOW_EMPTY is not set"));
    // Package overrides are not in OVERRIDES.
    assert!(rows(&out).iter().all(|r| r[0] == "inactive"), "{out}");

    // `${PN}` in the name is expanded with BitBake's values; the name never reaches a shell.
    let out = ok(&f, args("FILES:${PN}", Some("curl"))).await;
    assert!(
        out.contains("note: the name FILES:${PN} expands to FILES:curl"),
        "{out}"
    );
    let r = rows(&out);
    assert_eq!(r.len(), 4, "{out}");
    assert_eq!(r[3][1], "rename");
    assert_eq!(r[3][3], "data.py:97");
    assert!(r[3][4].starts_with("FILES:${PN} renamed to FILES:curl"));
    assert!(out.contains("final: \"/usr/bin/* "));

    let out = ok(&f, args("NO_SUCH_VAR", Some("curl"))).await;
    assert!(
        out.contains("note: BitBake has no variable or function NO_SUCH_VAR in bitbake -e curl")
    );

    // JSON keeps rows and notes.
    let mut a = args("PN", Some("curl"));
    a.format = Some(sous_chef::render::Format::Json);
    let out = ok(&f, a).await;
    let v: serde_json::Value = serde_json::from_str(&out).unwrap();
    assert!(v.to_string().contains("final: \\\"curl\\\""), "{out}");
}

#[tokio::test]
async fn names_and_arguments_are_checked_before_anything_runs() {
    let f = fixture();
    let e = err(&f, args("PN", Some("curl;rm -rf /"))).await;
    assert!(e.contains("needs a recipe name BitBake accepts"), "{e}");
    let e = err(
        &f,
        args("PN", Some("meta:recipes-support/curl/curl_8.19.0.bb")),
    )
    .await;
    assert!(e.contains("needs a recipe name"), "{e}");
    let mut a = args("PN", Some("curl"));
    a.machine = Some("qemux86".into());
    let e = err(&f, a).await;
    assert!(
        e.contains("machine cannot be set with authoritative=true"),
        "{e}"
    );
    let e = err(&f, args("PN[", Some("curl"))).await;
    assert!(e.contains("bad name"), "{e}");
    assert!(f.ctx.jobs.list().is_empty());
}

#[tokio::test]
async fn global_configuration_runs_bitbake_e_without_a_target() {
    let f = fixture();
    let out = ok(&f, args("MACHINE", None)).await;
    assert!(
        out.contains("source: BitBake (bitbake -e), handle"),
        "{out}"
    );
    assert!(out.contains("note: global configuration only (no recipe)"));
    let job = f.ctx.jobs.last().unwrap();
    assert_eq!(job.command, "bitbake -e");
    assert_eq!(job.op, "env");
}

#[tokio::test]
async fn an_unchanged_tree_reuses_the_dump() {
    let f = fixture();
    let first = ok(&f, args("PN", Some("curl"))).await;
    assert!(!first.contains("reused"));
    assert_eq!(f.ctx.jobs.list().len(), 1);

    // Same tree: no second run, the same handle.
    let second = ok(&f, args("PV", Some("curl"))).await;
    assert!(
        second.contains(", reused: the tree is unchanged since it ran"),
        "{second}"
    );
    assert_eq!(f.ctx.jobs.list().len(), 1);
    let h1 = handle_in(&first, "source: BitBake (bitbake -e curl), handle ");
    let h2 = handle_in(&second, "source: BitBake (bitbake -e curl), handle ");
    assert_eq!(h1, h2);

    // A file outside the recipe's tree (another recipe) does not count.
    envdump_support::write(
        &f.live
            .root
            .join("yocto-kiss/openembedded-core/meta/recipes-x/zz/zz_1.0.bb"),
        "SUMMARY = \"zz\"\n",
    );
    let out = ok(&f, args("PN", Some("curl"))).await;
    assert!(out.contains("reused"), "{out}");
    assert_eq!(f.ctx.jobs.list().len(), 1);

    // A server restart keeps the cache (the handle store and the cache file persist).
    let restarted = ctx_for(&f.live, f.cache.path());
    let out = var::run(&restarted, args("PN", Some("curl")))
        .await
        .unwrap();
    assert!(out.contains("reused"), "{out}");
    assert!(restarted.jobs.list().is_empty());

    // An edit to the recipe's bbappend runs BitBake again.
    let append = f
        .live
        .root
        .join("yocto-kiss/meta-kiss/recipes-support/curl/curl_%.bbappend");
    fs::write(&append, "EXTRA_OECONF:append = \" --enable-more\"\n").unwrap();
    let out = ok(&f, args("PN", Some("curl"))).await;
    assert!(!out.contains("reused"), "{out}");
    assert_eq!(f.ctx.jobs.list().len(), 2);

    // So does a configuration file.
    let local = f.live.build().join("conf/local.conf");
    fs::write(
        &local,
        "MACHINE ?= \"dogbonedark\"\nDISTRO ?= \"kiss\"\nX = \"1\"\n",
    )
    .unwrap();
    let out = ok(&f, args("PN", Some("curl"))).await;
    assert!(!out.contains("reused"), "{out}");
    assert_eq!(f.ctx.jobs.list().len(), 3);
    // And the cache is per recipe: the global dump is separate.
    let out = ok(&f, args("PN", None)).await;
    assert!(!out.contains("reused"), "{out}");
    assert_eq!(f.ctx.jobs.list().len(), 4);
}

#[tokio::test]
async fn a_running_job_blocks_a_new_run_but_not_a_cached_dump() {
    let f = fixture();
    ok(&f, args("PN", Some("curl"))).await;
    let dump = f.live.root.join("bitbake-e-curl.out");
    f.live.fake(&[
        ("FAKEBB_OUTPUT", &dump.display().to_string()),
        ("FAKEBB_SLEEP", "30"),
    ]);
    let env = sous_chef::bitbake::BbEnv::probe(
        &f.ctx.config,
        &f.ctx.workspace,
        &sous_chef::bitbake::HostEnv::from_process(),
    )
    .unwrap();
    let started = f
        .ctx
        .jobs
        .start(
            &env,
            JobSpec {
                op: "build".into(),
                target: Some("kiss-image".into()),
                program: "bitbake".into(),
                args: vec!["kiss-image".into()],
                kind: HandleKind::Build,
            },
            &f.cache.path().join("jobs"),
        )
        .unwrap();
    let sous_chef::bitbake::Started::Started(job) = started else {
        panic!("not started");
    };
    // Cached: answered while the build runs.
    let out = ok(&f, args("PV", Some("curl"))).await;
    assert!(out.contains("reused"), "{out}");
    // Not cached: refused, naming the running job.
    let e = err(&f, args("PV", None)).await;
    assert!(
        e.contains(&format!("{} (build kiss-image) is running", job.id))
            && e.contains("one client per build directory")
            && e.contains("authoritative=false"),
        "{e}"
    );
    f.ctx.jobs.cancel(&job.id).unwrap();
    f.ctx.jobs.wait(&job.id, Duration::from_secs(10)).await;
}

#[tokio::test]
async fn a_running_read_only_job_is_waited_for_then_bitbake_e_runs() {
    let f = fixture();
    let dump = f.live.root.join("bitbake-e-curl.out");
    let env = sous_chef::bitbake::BbEnv::probe(
        &f.ctx.config,
        &f.ctx.workspace,
        &sous_chef::bitbake::HostEnv::from_process(),
    )
    .unwrap();
    // A `bitbake-layers` of a second, then the dump without sleeping for the next run.
    let pidfile = f.live.root.join("layers.pids");
    f.live.fake(&[
        ("FAKEBB_SLEEP", "1"),
        ("FAKEBB_PIDFILE", &pidfile.display().to_string()),
    ]);
    let started = f
        .ctx
        .jobs
        .start(
            &env,
            JobSpec {
                op: "layers".into(),
                target: None,
                program: "bitbake-layers".into(),
                args: vec!["show-layers".into()],
                kind: HandleKind::Text,
            },
            &f.cache.path().join("jobs"),
        )
        .unwrap();
    let sous_chef::bitbake::Started::Started(layers) = started else {
        panic!("not started");
    };
    // Once it sleeps, the next run (the `bitbake -e` after it) reads the new settings.
    while !pidfile.exists() {
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    f.live
        .fake(&[("FAKEBB_OUTPUT", &dump.display().to_string())]);
    let out = ok(&f, args("PN", Some("curl"))).await;
    assert!(out.contains("source: BitBake (bitbake -e curl)"), "{out}");
    let done = f.ctx.jobs.get(&layers.id).unwrap();
    assert_ne!(done.state, sous_chef::bitbake::JobState::Running);
}

#[tokio::test]
async fn a_slow_run_returns_the_job_and_the_next_call_waits_for_it() {
    let f = fixture();
    let dump = f.live.root.join("bitbake-e-curl.out");
    f.live.fake(&[
        ("FAKEBB_OUTPUT", &dump.display().to_string()),
        ("FAKEBB_SLEEP", "2"),
    ]);
    let a = args("PN", Some("curl"));
    let out = var_authoritative::run_with(&f.ctx, &a, Duration::from_millis(100))
        .await
        .unwrap();
    assert!(
        out.starts_with("bitbake -e curl is running as job:1"),
        "{out}"
    );
    assert!(out.contains("bitbake op=status job=job:1"));
    let out = var_authoritative::run_with(&f.ctx, &a, Duration::from_secs(20))
        .await
        .unwrap();
    assert!(out.contains("final: \"curl\""), "{out}");
    assert_eq!(f.ctx.jobs.list().len(), 1);
    // And it is cached now.
    let out = var_authoritative::run_with(&f.ctx, &a, Duration::from_secs(20))
        .await
        .unwrap();
    assert!(out.contains("reused"), "{out}");
}

#[tokio::test]
async fn a_failed_run_is_an_error_with_the_first_errors() {
    let f = fixture();
    let bad = f.live.root.join("bad.out");
    fs::write(
        &bad,
        "Loading cache...done.\nERROR: Nothing PROVIDES 'curl'\nSummary: There was 1 ERROR message, returning a non-zero exit code.\n",
    )
    .unwrap();
    f.live.fake(&[
        ("FAKEBB_OUTPUT", &bad.display().to_string()),
        ("FAKEBB_EXIT", "1"),
    ]);
    let e = err(&f, args("PN", Some("curl"))).await;
    assert!(
        e.contains("bitbake -e curl failed (exit 1): ERROR: Nothing PROVIDES 'curl'")
            && e.contains("slice handle=h:"),
        "{e}"
    );
    // Not cached: the next call runs again.
    let dump = f.live.root.join("bitbake-e-curl.out");
    f.live
        .fake(&[("FAKEBB_OUTPUT", &dump.display().to_string())]);
    let out = ok(&f, args("PN", Some("curl"))).await;
    assert!(
        out.contains("final: \"curl\"") && !out.contains("reused"),
        "{out}"
    );

    // Exit 0 without a dump.
    let g = fixture();
    let junk = g.live.root.join("junk.out");
    fs::write(&junk, "nothing useful\n").unwrap();
    g.live
        .fake(&[("FAKEBB_OUTPUT", &junk.display().to_string())]);
    let e = err(&g, args("PN", Some("curl"))).await;
    assert!(e.contains("holds no environment dump"), "{e}");
    // Not cached either.
    let dump = g.live.write_dump();
    g.live
        .fake(&[("FAKEBB_OUTPUT", &dump.display().to_string())]);
    let out = ok(&g, args("PN", Some("curl"))).await;
    assert!(!out.contains("reused"), "{out}");
}

#[tokio::test]
async fn without_an_environment_it_is_unavailable() {
    let f = fixture();
    let mut config = f.live.config.clone();
    config.bitbake.enabled = false;
    let cache = tempfile::tempdir().unwrap();
    let ctx = Ctx {
        config,
        ..ctx_for(&f.live, cache.path())
    };
    let e = var::run(&ctx, args("PN", Some("curl"))).await.unwrap_err();
    assert!(matches!(e, Error::BitbakeUnavailable), "{e}");
}
