//! T9.3: the kas and bitbake-setup modes of the environment runner, with the fake kas
//! (`tests/fixtures/fakebb/kasbin/kas`) and the fake BitBake. No real kas or BitBake.
#![cfg(unix)]
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;
mod kas_support;

use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use kas_support::Project;
use sous_chef::bitbake::env::{
    INIT_SCRIPT_NAME, KAS_SHELL_ARGS, SETUP_SCRIPT_NAME, kas_command_string,
};
use sous_chef::bitbake::{BbEnv, Finish, HostEnv};
use sous_chef::workspace::Workspace;

fn kas_path() -> OsString {
    let dir = common::fixture_path("fakebb/kasbin");
    std::env::join_paths([dir, PathBuf::from("/usr/bin"), PathBuf::from("/bin")]).unwrap()
}

fn kas_env(p: &Project) -> BbEnv {
    let host = HostEnv {
        path: Some(kas_path()),
        ..HostEnv::default()
    };
    BbEnv::probe(&p.config(), &p.ws(), &host).unwrap()
}

fn std_cmd(c: &tokio::process::Command) -> (PathBuf, Vec<String>, Option<PathBuf>) {
    let s = c.as_std();
    (
        PathBuf::from(s.get_program()),
        s.get_args()
            .map(|a| a.to_string_lossy().into_owned())
            .collect(),
        s.get_current_dir().map(Path::to_path_buf),
    )
}

#[test]
fn a_kas_project_with_kas_on_path_runs_through_kas_shell() {
    let p = Project::after();
    let env = kas_env(&p);
    assert_eq!(env.mode(), "kas");
    assert_eq!(env.build_dir, p.path("build"));
    assert_eq!(env.init_script, None);
    let cmd = env.command("bitbake", &["-e", "app"]).unwrap();
    let (program, args, dir) = std_cmd(&cmd);
    assert_eq!(program, common::fixture_path("fakebb/kasbin/kas"));
    let files = format!(
        "{}:{}",
        p.path("kas/base.yaml").display(),
        p.path("kas/board.yaml").display()
    );
    assert_eq!(
        args,
        [
            KAS_SHELL_ARGS[0],
            KAS_SHELL_ARGS[1],
            files.as_str(),
            "-c",
            "'bitbake' '-e' 'app'"
        ]
    );
    assert_eq!(dir, Some(p.root.clone()));
}

#[test]
fn before_checkout_kas_mode_says_to_run_kas_checkout() {
    let p = Project::before();
    let host = HostEnv {
        path: Some(kas_path()),
        ..HostEnv::default()
    };
    let e = BbEnv::probe(&p.config(), &p.ws(), &host).unwrap_err();
    assert!(
        e.contains("run kas checkout kas/base.yaml:kas/board.yaml"),
        "{e}"
    );
    // Without kas on PATH the reason says so.
    let e = BbEnv::probe(&p.config(), &p.ws(), &HostEnv::default()).unwrap_err();
    assert!(e.contains("kas is not on PATH"), "{e}");
}

#[test]
fn a_repo_include_keeps_kas_mode_off() {
    // kas shell -k still runs repo_setup_loop, which fetches and checks out the
    // repositories `{repo, file}` includes name: sous-chef does not run kas then.
    let p = Project::after();
    fs::write(
        p.path("kas/vendor.yaml"),
        "header:\n  version: 11\n  includes:\n    - repo: meta-vendor\n      file: kas/vendor-extra.yaml\n",
    )
    .unwrap();
    let cfg = sous_chef::config::Config {
        kas: vec![p.path("kas/base.yaml"), p.path("kas/vendor.yaml")],
        ..p.config()
    };
    let ws = Workspace::discover_in(&cfg, &sous_chef::workspace::kas::KasEnv::default()).unwrap();
    assert_eq!(ws.kas.as_ref().unwrap().repo_includes, ["meta-vendor"]);
    let host = HostEnv {
        path: Some(kas_path()),
        ..HostEnv::default()
    };
    match BbEnv::probe(&cfg, &ws, &host) {
        Ok(env) => assert_ne!(env.mode(), "kas", "{env:?}"),
        Err(e) => assert!(
            e.contains("include files from repo meta-vendor")
                && e.contains("still fetches and checks out"),
            "{e}"
        ),
    }
    // Without the include, the same checkout is kas mode.
    assert_eq!(kas_env(&p).mode(), "kas");
}

#[tokio::test]
async fn the_command_reaches_bitbake_with_its_argv() {
    let p = Project::after();
    let env = kas_env(&p);
    let out = env
        .output("bitbake", &["-c", "compile", "app"], None)
        .await
        .unwrap();
    assert!(out.finish.success(), "{out:?}");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("kasarg: shell\nkasarg: --keep-config-unchanged\n"),
        "{stdout}"
    );
    let bb = &stdout[stdout.find("fakebb: bitbake\n").expect("bitbake ran")..];
    assert!(
        bb.contains(&format!("cwd: {}\n", p.path("build").display())),
        "{bb}"
    );
    assert!(
        bb.contains("argc: 3\narg: -c\narg: compile\narg: app\n"),
        "{bb}"
    );
    assert!(p.path("fakekas-called").is_file());
}

/// The command string kas runs in a shell is built from validated names only: anything a
/// shell would interpret is refused before kas is started.
#[tokio::test]
async fn shell_characters_are_refused_before_kas_starts() {
    let p = Project::after();
    let env = kas_env(&p);
    let marker = p.path("pwned");
    let bad = [
        format!("app;touch {}", marker.display()),
        format!("$(touch {})", marker.display()),
        format!("`touch {}`", marker.display()),
        "app'".to_string(),
        "a\"pp".to_string(),
        "app other".to_string(),
        "app|id".to_string(),
        "-c;id".to_string(),
        "app\nid".to_string(),
    ];
    for b in &bad {
        assert!(env.command("bitbake", &["-e", b]).is_err(), "{b:?}");
        assert!(
            env.output("bitbake", &["-e", b], None).await.is_err(),
            "{b:?}"
        );
        assert!(kas_command_string("bitbake", &[b]).is_err(), "{b:?}");
    }
    for prog in ["sh", "bitbake;id", "kas"] {
        assert!(env.command(prog, &[]).is_err(), "{prog}");
    }
    assert!(!marker.exists());
    assert!(
        !p.path("fakekas-called").exists(),
        "kas was started for a refused command"
    );
}

#[test]
fn a_kas_file_path_with_a_colon_is_refused() {
    let p = Project::after();
    let mut env = kas_env(&p);
    if let Some(k) = env.kas.as_mut() {
        k.files.push(PathBuf::from("/x/a:b.yaml"));
    }
    assert!(env.command("bitbake", &["-p"]).is_err());
}

/// True once `pid` no longer runs (gone, or a zombie waiting to be reaped), polling up to a
/// deadline. The runner signals the whole group and reaps only its own child; the fake
/// BitBake and its `sleep` are not its children, so they may still be exiting (a signal is
/// delivered, the process not yet scheduled to die) when it returns. Checking once, at
/// that instant, failed 2 runs in 30 under a parallel `cargo test`.
fn dead(pid: &str) -> bool {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        if !common::process_alive(pid.trim().parse().unwrap()) {
            return true;
        }
        if Instant::now() > deadline {
            return false;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

#[tokio::test]
async fn a_timeout_kills_the_whole_kas_process_group() {
    let p = Project::after();
    let env = kas_env(&p);
    let pidfile = p.path("pids");
    // Far longer than any run below may take: a run that ends sooner was killed.
    const SLEEP: Duration = Duration::from_secs(120);
    fs::write(
        p.path("fakekas.env"),
        format!(
            "export FAKEBB_SLEEP={}\nexport FAKEBB_PIDFILE={}\n",
            SLEEP.as_secs(),
            pidfile.display()
        ),
    )
    .unwrap();
    // The timeout must fire after the fake BitBake has written its pids (kas, sh, the fake
    // and its sleep child start first). If a loaded machine took longer than the timeout,
    // the run is repeated with the timeout doubled, until the pids were written before the
    // kill, within an overall deadline.
    let deadline = Instant::now() + Duration::from_secs(60);
    let mut timeout = Duration::from_millis(1500);
    let pids = loop {
        let _ = fs::remove_file(&pidfile);
        let start = Instant::now();
        let out = env.output("bitbake", &["-p"], Some(timeout)).await.unwrap();
        assert_eq!(out.finish, Finish::TimedOut);
        // The timeout plus the SIGTERM grace and the output drain, not the fake's sleep.
        let took = start.elapsed();
        assert!(took < timeout + Duration::from_secs(20), "{took:?}");
        if let Ok(pids) = fs::read_to_string(&pidfile) {
            break pids;
        }
        assert!(
            Instant::now() < deadline,
            "the fake BitBake never wrote its pids before the timeout"
        );
        timeout *= 2;
    };
    let pids: Vec<&str> = pids.split_whitespace().collect();
    assert_eq!(pids.len(), 2, "{pids:?}");
    for pid in pids {
        // The fake bitbake and its sleep child are gone.
        assert!(dead(pid), "process {pid} survived");
    }
}

#[test]
fn an_initialised_environment_still_wins_over_kas() {
    let p = Project::after();
    let mut path = OsString::from(common::fixture_path("fakebb/bin"));
    path.push(":");
    path.push(kas_path());
    let host = HostEnv {
        builddir: Some(p.path("build")),
        bbpath: None,
        path: Some(path),
    };
    let env = BbEnv::probe(&p.config(), &p.ws(), &host).unwrap();
    assert_eq!(env.mode(), "initialised");
}

// --- bitbake-setup --------------------------------------------------------------------

/// A bitbake-setup build directory: `build/init-build-env` written as bitbake-setup
/// writes it (`cd <dir of oe-init-build-env>; set <builddir>; . ./oe-init-build-env;
/// set --`), pointing at the fake `oe-init-build-env`.
fn setup_build(base: &Path) -> (PathBuf, Workspace) {
    let build = base.join("bitbake-builds/proj-board1/build");
    fs::create_dir_all(build.join("conf")).unwrap();
    let build = build.canonicalize().unwrap();
    let oe = base.join("bitbake-builds/proj-board1/layers/openembedded-core");
    fs::create_dir_all(oe.join("meta/conf")).unwrap();
    fs::copy(
        common::fixture_path("fakebb").join(INIT_SCRIPT_NAME),
        oe.join(INIT_SCRIPT_NAME),
    )
    .unwrap();
    common::copy_dir(&common::fixture_path("fakebb/bin"), &oe.join("bin"));
    fs::write(
        build.join(SETUP_SCRIPT_NAME),
        format!(
            "# init-build-env wrapper created by bitbake-setup\ncd {}\nset {}\n. ./oe-init-build-env\nset --\n\n",
            oe.display(),
            build.display()
        ),
    )
    .unwrap();
    let ws = Workspace {
        build_dir: Some(build.clone()),
        ..Workspace::default()
    };
    (build, ws)
}

#[tokio::test]
async fn a_bitbake_setup_build_is_detected_and_runs() {
    let tmp = tempfile::tempdir().unwrap();
    let (build, ws) = setup_build(tmp.path());
    let cfg = sous_chef::config::Config::default();
    let env = BbEnv::probe(&cfg, &ws, &HostEnv::default()).unwrap();
    assert_eq!(env.mode(), "bitbake-setup");
    assert_eq!(env.init_script, Some(build.join(SETUP_SCRIPT_NAME)));
    let out = env
        .output("bitbake-layers", &["show-layers"], None)
        .await
        .unwrap();
    assert!(out.finish.success(), "{out:?}");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.starts_with("fakebb: bitbake-layers\n"), "{stdout}");
    assert!(
        stdout.contains(&format!("cwd: {}\n", build.display())),
        "{stdout}"
    );
    assert!(
        stdout.contains(&format!("BUILDDIR: {}\n", build.display())),
        "{stdout}"
    );
    assert!(stdout.contains("argc: 1\narg: show-layers\n"), "{stdout}");
}

#[tokio::test]
async fn a_configured_init_build_env_uses_the_setup_wrapper() {
    let tmp = tempfile::tempdir().unwrap();
    let (build, ws) = setup_build(tmp.path());
    let cfg = sous_chef::config::Config {
        init_script: Some(build.join(SETUP_SCRIPT_NAME)),
        ..sous_chef::config::Config::default()
    };
    let env = BbEnv::probe(&cfg, &ws, &HostEnv::default()).unwrap();
    assert_eq!(env.mode(), "bitbake-setup");
    let out = env.output("bitbake", &["-e", "app"], None).await.unwrap();
    assert!(out.finish.success(), "{out:?}");
    assert!(
        String::from_utf8_lossy(&out.stdout).contains("arg: -e\narg: app\n"),
        "{out:?}"
    );
}

// ---------------------------------------------------------------------------------------
// y3-leftovers, item 7: cancel through `kas shell`, and the server's TMPDIR.

/// A cancel of a kas-mode job interrupts BitBake through `kas shell` as Ctrl-C twice does.
/// The fake kas stays the parent and defers SIGINT and SIGTERM as kas 5.5 does (its asyncio
/// handlers do not run while `subprocess.call` waits), so the interrupts reach BitBake
/// only through the job's process group; the fake BitBake behaves as knotty (stops after
/// the second SIGINT, ignores SIGTERM). Live (y3-leftovers) a kas-mode `bitbake curl`
/// stopped this way in 3.4 s.
#[tokio::test]
async fn a_cancel_reaches_bitbake_through_kas_shell() {
    use sous_chef::bitbake::{JobSpec, JobState, JobTable, Started};
    use sous_chef::handles::HandleKind;
    let p = Project::after();
    let env = kas_env(&p);
    let pidfile = p.path("pids");
    fs::write(
        p.path("fakekas.env"),
        format!(
            "export FAKEKAS_AS_KAS=1\nexport FAKEBB_ON_INT=1\nexport FAKEBB_PIDFILE={}\n",
            pidfile.display()
        ),
    )
    .unwrap();
    let jobs = JobTable::new();
    let out_dir = p.path("jobs");
    let spec = JobSpec {
        op: "build".into(),
        target: Some("app".into()),
        program: "bitbake".into(),
        args: vec!["app".into()],
        kind: HandleKind::Build,
    };
    let job = match jobs.start(&env, spec, &out_dir).unwrap() {
        Started::Started(j) => j,
        other => panic!("{other:?}"),
    };
    let deadline = Instant::now() + Duration::from_secs(10);
    while !pidfile.exists() {
        assert!(Instant::now() < deadline, "the fake BitBake never started");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    let bitbake_pid = fs::read_to_string(&pidfile).unwrap();
    let started = Instant::now();
    jobs.cancel(&job.id).unwrap();
    let done = jobs.wait(&job.id, Duration::from_secs(30)).await.unwrap();
    assert_eq!(done.state, JobState::Cancelled, "{done:?}");
    assert!(started.elapsed() < Duration::from_secs(10));
    assert!(dead(bitbake_pid.trim()), "the fake BitBake survived");
    let output = fs::read_to_string(&done.output).unwrap();
    for want in [
        "Keyboard Interrupt, closing down...",
        "Second Keyboard Interrupt, stopping...",
        "fakebb: build stopped",
        // kas waited for its shell, and its own handling came only after.
        "fakekas: shell returned 1",
    ] {
        assert!(output.contains(want), "{want:?} not in:\n{output}");
    }
    assert!(
        output.find("fakebb: build stopped").unwrap()
            < output.find("fakekas: shell returned").unwrap(),
        "{output}"
    );
}

/// kas copies `TMPDIR` from its environment into BitBake's and adds it to
/// `BB_ENV_PASSTHROUGH_ADDITIONS` (kas 5.5 `libkas.py` `get_build_environ`), so the
/// server's own temporary directory would become BitBake's `TMPDIR` (seen live in
/// y3-leftovers). Every mode removes it from the first process's environment (z1-env:
/// the initialised mode too; tests/bitbake_pass_env.rs runs each mode end to end).
#[test]
fn kas_mode_does_not_hand_the_servers_tmpdir_to_kas() {
    use sous_chef::bitbake::env::STRIPPED_ENV;
    assert_eq!(STRIPPED_ENV, ["TMPDIR"]);
    let removed = |cmd: &tokio::process::Command| -> Vec<String> {
        cmd.as_std()
            .get_envs()
            .filter(|(_, v)| v.is_none())
            .map(|(k, _)| k.to_string_lossy().into_owned())
            .collect()
    };
    let p = Project::after();
    let cmd = kas_env(&p).command("bitbake", &["curl"]).unwrap();
    assert_eq!(removed(&cmd), ["TMPDIR"]);
    let plain = BbEnv {
        build_dir: p.path("build"),
        init_script: None,
        kas: None,
        pass_env: Vec::new(),
    };
    let cmd = plain.command("bitbake", &["curl"]).unwrap();
    assert_eq!(removed(&cmd), ["TMPDIR"]);
}
