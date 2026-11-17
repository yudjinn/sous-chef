//! T5.3: the environment runner, against the fake `bitbake` in `tests/fixtures/fakebb/`.
//! Detection in each mode, argv safety, exit status, timeout and cancellation killing the
//! whole process group. No real BitBake is needed.
#![cfg(unix)]
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use sous_chef::bitbake::env::{ALLOWED_PROGRAMS, INIT_SCRIPT_NAME};
use sous_chef::bitbake::{BbEnv, Finish, HostEnv, Supervised, validate_name};
use sous_chef::config::Config;
use sous_chef::workspace::{Layer, Workspace};

fn fakebb() -> PathBuf {
    common::fixture_path("fakebb")
}

fn layer(root: &Path) -> Layer {
    Layer {
        name: root.file_name().unwrap().to_string_lossy().into_owned(),
        root: root.canonicalize().unwrap(),
        collections: vec![],
        priority: 5,
        compat: vec![],
        depends: vec![],
        bbfiles: vec![],
        writable: false,
    }
}

/// A checkout under `base`: `<poky>/oe-init-build-env` and `<poky>/bin/*` (copies of the
/// fake), layer `<poky>/meta`, and `<base>/build/conf`.
struct Tree {
    poky: PathBuf,
    build: PathBuf,
    ws: Workspace,
}

fn tree(base: &Path, poky_name: &str, with_script: bool) -> Tree {
    let poky = base.join(poky_name);
    fs::create_dir_all(poky.join("meta/conf")).unwrap();
    if with_script {
        fs::copy(fakebb().join(INIT_SCRIPT_NAME), poky.join(INIT_SCRIPT_NAME)).unwrap();
        common::copy_dir(&fakebb().join("bin"), &poky.join("bin"));
    }
    let build = base.join("build");
    fs::create_dir_all(build.join("conf")).unwrap();
    let build = build.canonicalize().unwrap();
    let ws = Workspace {
        build_dir: Some(build.clone()),
        layers: vec![layer(&poky.join("meta"))],
    };
    Tree { poky, build, ws }
}

fn enabled() -> Config {
    Config::default()
}

fn fake_path() -> OsString {
    let mut p = OsString::from(fakebb().join("bin"));
    p.push(":");
    p.push(std::env::var_os("PATH").unwrap_or_default());
    p
}

async fn stdout_of(env: &BbEnv, program: &str, args: &[&str]) -> (Finish, String) {
    let mut cmd = env.command(program, args).unwrap();
    // Initialised mode relies on PATH; the test process has no fake bitbake on its own.
    cmd.env("PATH", fake_path());
    cmd.stdout(std::process::Stdio::piped());
    let out = cmd.output().await.unwrap();
    let finish = Finish::Exited(out.status);
    (finish, String::from_utf8_lossy(&out.stdout).into_owned())
}

// --- detection -------------------------------------------------------------------------

#[test]
fn detects_configured_init_script() {
    let tmp = tempfile::tempdir().unwrap();
    let t = tree(tmp.path(), "poky", false);
    let cfg = Config {
        init_script: Some(fakebb().join(INIT_SCRIPT_NAME)),
        ..enabled()
    };
    let env = BbEnv::detect_in(&cfg, &t.ws, &HostEnv::default()).unwrap();
    assert_eq!(env.build_dir, t.build);
    assert_eq!(
        env.init_script.as_deref(),
        Some(
            fakebb()
                .join(INIT_SCRIPT_NAME)
                .canonicalize()
                .unwrap()
                .as_path()
        )
    );
    assert_eq!(env.mode(), "init-script");

    // The configured script wins over an initialised process environment.
    let host = HostEnv {
        builddir: Some(t.build.clone()),
        bbpath: None,
        path: Some(fake_path()),
    };
    let env = BbEnv::detect_in(&cfg, &t.ws, &host).unwrap();
    assert_eq!(env.mode(), "init-script");

    // A configured script that does not exist is reported, not silently replaced.
    let cfg = Config {
        init_script: Some(tmp.path().join("missing-init")),
        ..enabled()
    };
    let e = BbEnv::probe(&cfg, &t.ws, &host).unwrap_err();
    assert!(
        e.contains("missing-init") && e.contains("init_script"),
        "{e}"
    );

    // A script but no build directory.
    let cfg = Config {
        init_script: Some(fakebb().join(INIT_SCRIPT_NAME)),
        ..enabled()
    };
    let no_build = Workspace {
        build_dir: None,
        layers: t.ws.layers.clone(),
    };
    let e = BbEnv::probe(&cfg, &no_build, &HostEnv::default()).unwrap_err();
    assert!(e.contains("no build directory"), "{e}");
}

#[test]
fn finds_init_script_next_to_the_layers() {
    let tmp = tempfile::tempdir().unwrap();
    let t = tree(tmp.path(), "poky", true);
    let env = BbEnv::detect_in(&enabled(), &t.ws, &HostEnv::default()).unwrap();
    assert_eq!(env.build_dir, t.build);
    assert_eq!(
        env.init_script,
        Some(t.ws.layers[0].root.parent().unwrap().join(INIT_SCRIPT_NAME))
    );
    assert!(t.poky.join(INIT_SCRIPT_NAME).is_file());
}

#[test]
fn finds_init_script_next_to_the_build_dir() {
    let tmp = tempfile::tempdir().unwrap();
    let t = tree(tmp.path(), "layers/poky", false);
    fs::copy(
        fakebb().join(INIT_SCRIPT_NAME),
        tmp.path().join(INIT_SCRIPT_NAME),
    )
    .unwrap();
    let env = BbEnv::detect_in(&enabled(), &t.ws, &HostEnv::default()).unwrap();
    assert_eq!(
        env.init_script,
        Some(t.build.parent().unwrap().join(INIT_SCRIPT_NAME))
    );
}

#[test]
fn detects_an_initialised_environment() {
    let tmp = tempfile::tempdir().unwrap();
    let t = tree(tmp.path(), "poky", false);
    let other_build = tmp.path().join("other-build");
    fs::create_dir_all(&other_build).unwrap();

    // BUILDDIR names the build directory.
    let host = HostEnv {
        builddir: Some(other_build.clone()),
        bbpath: None,
        path: Some(fake_path()),
    };
    let env = BbEnv::detect_in(&enabled(), &t.ws, &host).unwrap();
    assert_eq!(env.init_script, None);
    assert_eq!(env.mode(), "initialised");
    assert_eq!(env.build_dir, other_build.canonicalize().unwrap());

    // BBPATH alone: the workspace's build directory.
    let host = HostEnv {
        builddir: None,
        bbpath: Some(OsString::from("/x")),
        path: Some(fake_path()),
    };
    let env = BbEnv::detect_in(&enabled(), &t.ws, &host).unwrap();
    assert_eq!(env.build_dir, t.build);
    assert_eq!(env.init_script, None);

    // BUILDDIR set, but no bitbake on PATH: not initialised, and no script to fall back on.
    let host = HostEnv {
        builddir: Some(other_build),
        bbpath: None,
        path: Some(OsString::from(tmp.path())),
    };
    let e = BbEnv::probe(&enabled(), &t.ws, &host).unwrap_err();
    assert!(e.contains(INIT_SCRIPT_NAME), "{e}");

    // bitbake on PATH but nothing says the environment is initialised.
    let host = HostEnv {
        builddir: None,
        bbpath: None,
        path: Some(fake_path()),
    };
    assert_eq!(BbEnv::detect_in(&enabled(), &t.ws, &host), None);
}

#[test]
fn detects_nothing_without_a_build_environment() {
    let tmp = tempfile::tempdir().unwrap();
    let t = tree(tmp.path(), "poky", true);

    // Disabled in the configuration, even with everything present.
    let mut cfg = enabled();
    cfg.bitbake.enabled = false;
    let e = BbEnv::probe(&cfg, &t.ws, &HostEnv::default()).unwrap_err();
    assert!(e.contains("disabled"), "{e}");

    // No build directory and no initialised environment.
    let ws = Workspace {
        build_dir: None,
        layers: t.ws.layers.clone(),
    };
    let e = BbEnv::probe(&enabled(), &ws, &HostEnv::default()).unwrap_err();
    assert!(e.contains("no build directory"), "{e}");

    // A build directory but no init script anywhere.
    let tmp2 = tempfile::tempdir().unwrap();
    let t2 = tree(tmp2.path(), "poky", false);
    let e = BbEnv::probe(&enabled(), &t2.ws, &HostEnv::default()).unwrap_err();
    assert!(e.contains("set init_script"), "{e}");

    // The fixture workspace (bitbake disabled in its sous-chef.toml) has none.
    let cfg = common::fixture_config();
    assert_eq!(BbEnv::detect(&cfg, &common::fixture_ws()), None);
}

// --- running ---------------------------------------------------------------------------

#[tokio::test]
async fn runs_through_the_init_script() {
    let tmp = tempfile::tempdir().unwrap();
    let t = tree(tmp.path(), "poky", true);
    let env = BbEnv::detect_in(&enabled(), &t.ws, &HostEnv::default()).unwrap();
    let out = env
        .output("bitbake", &["-c", "compile", "hello"], None)
        .await
        .unwrap();
    assert!(out.finish.success(), "{out:?}");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.starts_with("fakebb: bitbake\n"), "{stdout}");
    // The banner of the sourced script was discarded.
    assert!(!stdout.contains("Shell environment"), "{stdout}");
    assert!(
        stdout.contains(&format!("cwd: {}\n", t.build.display())),
        "{stdout}"
    );
    assert!(
        stdout.contains(&format!("BUILDDIR: {}\n", t.build.display())),
        "{stdout}"
    );
    assert!(
        stdout.contains("argc: 3\narg: -c\narg: compile\narg: hello\n"),
        "{stdout}"
    );

    // Every allowlisted program resolves through the script's PATH.
    for p in ALLOWED_PROGRAMS {
        let out = env.output(p, &["--help"], None).await.unwrap();
        let stdout = String::from_utf8_lossy(&out.stdout);
        assert!(
            stdout.starts_with(&format!("fakebb: {p}\n")),
            "{p}: {stdout}"
        );
    }
}

#[tokio::test]
async fn runs_directly_when_initialised() {
    let tmp = tempfile::tempdir().unwrap();
    let t = tree(tmp.path(), "poky", false);
    let host = HostEnv {
        builddir: Some(t.build.clone()),
        bbpath: None,
        path: Some(fake_path()),
    };
    let env = BbEnv::detect_in(&enabled(), &t.ws, &host).unwrap();
    let (finish, stdout) = stdout_of(&env, "bitbake-layers", &["show-layers"]).await;
    assert!(finish.success());
    assert!(stdout.starts_with("fakebb: bitbake-layers\n"), "{stdout}");
    assert!(
        stdout.contains(&format!("cwd: {}\n", t.build.display())),
        "{stdout}"
    );
    assert!(stdout.contains("argc: 1\narg: show-layers\n"), "{stdout}");
}

#[tokio::test]
async fn reports_exit_status_and_stderr() {
    let tmp = tempfile::tempdir().unwrap();
    let t = tree(tmp.path(), "poky", true);
    let env = BbEnv::detect_in(&enabled(), &t.ws, &HostEnv::default()).unwrap();
    let mut cmd = env.command("bitbake", &["hello"]).unwrap();
    cmd.env("FAKEBB_EXIT", "3")
        .env("FAKEBB_STDERR", "ERROR: boom");
    let out = cmd.output().await.unwrap();
    assert_eq!(out.status.code(), Some(3));
    assert_eq!(String::from_utf8_lossy(&out.stderr), "ERROR: boom\n");
}

// --- argv safety -----------------------------------------------------------------------

#[test]
fn refuses_programs_outside_the_allowlist() {
    let env = BbEnv {
        build_dir: PathBuf::from("/"),
        init_script: Some(fakebb().join(INIT_SCRIPT_NAME)),
    };
    for p in [
        "bash",
        "sh",
        "rm",
        "/usr/bin/bitbake",
        "bitbake;id",
        "../bitbake",
        "",
    ] {
        let e = env.command(p, &[]).unwrap_err().to_string();
        assert!(e.contains("not allowed"), "{p}: {e}");
    }
}

#[tokio::test]
async fn hostile_target_names_are_rejected_and_never_reach_a_shell() {
    let tmp = tempfile::tempdir().unwrap();
    let t = tree(tmp.path(), "poky", true);
    let marker = tmp.path().join("PWNED");
    let m = marker.display();
    let env = BbEnv::detect_in(&enabled(), &t.ws, &HostEnv::default()).unwrap();
    let hostile = [
        format!("hello;touch {m}"),
        format!("$(touch {m})"),
        format!("`touch {m}`"),
        format!("hello && touch {m}"),
        format!("hello|touch {m}"),
        format!("hello\ntouch {m}"),
        "-e".to_string() + &format!(";touch {m}"),
        "--postfile=/etc/passwd".to_string(),
        "${IFS}".to_string(),
    ];
    for h in &hostile {
        assert!(validate_name(h).is_err(), "{h:?}");
        let e = env.command("bitbake", &["-c", "compile", h]).unwrap_err();
        assert!(e.to_string().contains("invalid argument"), "{h:?}: {e}");
    }
    // A leading dash is an option to `command` but never a valid name.
    assert!(validate_name("-c").is_err());
    // Nothing ran.
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert!(!marker.exists());
}

#[tokio::test]
async fn shell_metacharacters_in_paths_stay_data() {
    // The init script and build directory travel as argv: a checkout whose path is full
    // of shell syntax still works and runs nothing extra.
    let tmp = tempfile::tempdir().unwrap();
    let marker = tmp.path().join("PWNED");
    let weird = format!(
        "we ird;$(touch {})`touch {}`'\"",
        marker.display(),
        marker.display()
    );
    let base = tmp.path().join(&weird);
    fs::create_dir_all(&base).unwrap();
    let t = tree(&base, "poky $HOME", true);
    let env = BbEnv::detect_in(&enabled(), &t.ws, &HostEnv::default()).unwrap();
    assert!(
        env.init_script
            .as_ref()
            .unwrap()
            .to_string_lossy()
            .contains(";$(touch")
    );
    let out = env
        .output(
            "bitbake",
            &["core-image-minimal", "virtual/kernel", "gtk+3"],
            None,
        )
        .await
        .unwrap();
    assert!(out.finish.success(), "{out:?}");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains(&format!("BUILDDIR: {}\n", t.build.display())),
        "{stdout}"
    );
    assert!(
        stdout.contains("argc: 3\narg: core-image-minimal\narg: virtual/kernel\narg: gtk+3\n"),
        "{stdout}"
    );
    assert!(!marker.exists());
}

// --- timeout and cancellation ----------------------------------------------------------

/// Pids written by the fake: its own and its sleeping child's.
fn read_pids(file: &Path) -> (u32, u32) {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        if let Ok(s) = fs::read_to_string(file) {
            let mut it = s.split_whitespace().map(|p| p.parse().unwrap());
            if let (Some(a), Some(b)) = (it.next(), it.next()) {
                return (a, b);
            }
        }
        assert!(Instant::now() < deadline, "the fake never wrote its pids");
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// True once `pid` no longer runs (gone, or a zombie waiting to be reaped).
fn dead(pid: u32) -> bool {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        match fs::read_to_string(format!("/proc/{pid}/stat")) {
            Err(_) => return true,
            Ok(stat) => {
                let state = stat.rsplit(')').next().unwrap_or("").trim_start();
                if state.starts_with('Z') || state.starts_with('X') {
                    return true;
                }
            }
        }
        if Instant::now() > deadline {
            return false;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

fn sleeping_command(env: &BbEnv, pidfile: &Path, ignore_term: bool) -> tokio::process::Command {
    let mut cmd = env.command("bitbake", &["hello"]).unwrap();
    cmd.env("FAKEBB_SLEEP", "30")
        .env("FAKEBB_PIDFILE", pidfile)
        .stdout(std::process::Stdio::null());
    if ignore_term {
        cmd.env("FAKEBB_IGNORE_TERM", "1");
    }
    cmd
}

#[cfg(target_os = "linux")]
#[tokio::test]
async fn timeout_kills_the_process_group() {
    let tmp = tempfile::tempdir().unwrap();
    let t = tree(tmp.path(), "poky", true);
    let env = BbEnv::detect_in(&enabled(), &t.ws, &HostEnv::default()).unwrap();
    let pidfile = tmp.path().join("pids");
    let mut proc = Supervised::spawn(sleeping_command(&env, &pidfile, false)).unwrap();
    let leader = proc.group().id().unwrap();
    let started = Instant::now();
    let finish = proc
        .wait(Some(Duration::from_secs(1)), std::future::pending())
        .await
        .unwrap();
    assert_eq!(finish, Finish::TimedOut);
    assert!(
        started.elapsed() < Duration::from_secs(10),
        "{:?}",
        started.elapsed()
    );
    let (fake, sleeper) = read_pids(&pidfile);
    // bash exec'd the fake, so the fake is the group leader; its child is in the group too.
    assert_eq!(fake, leader);
    assert!(dead(fake), "fake bitbake {fake} survived");
    assert!(dead(sleeper), "its child {sleeper} survived");
}

#[cfg(target_os = "linux")]
#[tokio::test]
async fn timeout_kills_a_group_that_ignores_sigterm() {
    let tmp = tempfile::tempdir().unwrap();
    let t = tree(tmp.path(), "poky", true);
    let env = BbEnv::detect_in(&enabled(), &t.ws, &HostEnv::default()).unwrap();
    let pidfile = tmp.path().join("pids");
    let mut proc = Supervised::spawn(sleeping_command(&env, &pidfile, true)).unwrap();
    let finish = proc
        .wait(Some(Duration::from_secs(1)), std::future::pending())
        .await
        .unwrap();
    assert_eq!(finish, Finish::TimedOut);
    let (fake, sleeper) = read_pids(&pidfile);
    assert!(dead(fake), "fake bitbake {fake} survived");
    assert!(dead(sleeper), "its child {sleeper} survived");
}

#[cfg(target_os = "linux")]
#[tokio::test]
async fn cancellation_kills_the_process_group() {
    let tmp = tempfile::tempdir().unwrap();
    let t = tree(tmp.path(), "poky", true);
    let env = BbEnv::detect_in(&enabled(), &t.ws, &HostEnv::default()).unwrap();
    let pidfile = tmp.path().join("pids");
    let mut proc = Supervised::spawn(sleeping_command(&env, &pidfile, false)).unwrap();
    let notify = std::sync::Arc::new(tokio::sync::Notify::new());
    let trigger = std::sync::Arc::clone(&notify);
    let pf = pidfile.clone();
    tokio::spawn(async move {
        // Cancel once the fake is sleeping, as `bitbake op: cancel` would from another call.
        while !pf.exists() {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        trigger.notify_one();
    });
    let finish = proc
        .wait(Some(Duration::from_secs(60)), notify.notified())
        .await
        .unwrap();
    assert_eq!(finish, Finish::Cancelled);
    let (fake, sleeper) = read_pids(&pidfile);
    assert!(dead(fake));
    assert!(dead(sleeper));
}

#[cfg(target_os = "linux")]
#[tokio::test]
async fn dropping_a_running_command_kills_its_group() {
    let tmp = tempfile::tempdir().unwrap();
    let t = tree(tmp.path(), "poky", true);
    let env = BbEnv::detect_in(&enabled(), &t.ws, &HostEnv::default()).unwrap();
    let pidfile = tmp.path().join("pids");
    let proc = Supervised::spawn(sleeping_command(&env, &pidfile, false)).unwrap();
    let (fake, sleeper) = read_pids(&pidfile);
    drop(proc);
    assert!(dead(fake));
    assert!(dead(sleeper));
}

#[tokio::test]
async fn output_with_timeout_keeps_what_arrived() {
    let tmp = tempfile::tempdir().unwrap();
    let t = tree(tmp.path(), "poky", true);
    // The fake reads FAKEBB_SLEEP from the environment; `output` builds its own command,
    // so set it through the init script's environment instead: a copy that exports it.
    let script = t.poky.join(INIT_SCRIPT_NAME);
    let mut text = fs::read_to_string(&script).unwrap();
    text.push_str("export FAKEBB_SLEEP=30\n");
    fs::write(&script, text).unwrap();
    let env = BbEnv::detect_in(&enabled(), &t.ws, &HostEnv::default()).unwrap();
    let out = env
        .output("bitbake", &["hello"], Some(Duration::from_millis(800)))
        .await
        .unwrap();
    assert_eq!(out.finish, Finish::TimedOut);
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("arg: hello\n"), "{stdout}");
}

// --- status ----------------------------------------------------------------------------

#[tokio::test]
async fn status_reports_the_environment() {
    use sous_chef::bitbake::JobTable;
    use sous_chef::handles::HandleStore;
    use sous_chef::index::Index;
    use sous_chef::mcp::Ctx;
    use sous_chef::mcp::tools::status;
    use sous_chef::render::Format;

    async fn report(cfg: Config, ws: Workspace, cache: &Path) -> serde_json::Value {
        let index = Index::open_in(cache, &ws).unwrap();
        let handles = HandleStore::open(&index.cache_dir().join("handles")).unwrap();
        let ctx = Ctx {
            config: cfg,
            workspace: ws,
            index: tokio::sync::Mutex::new(index),
            handles,
            jobs: JobTable::new(),
        };
        let args = status::Args {
            budget_tokens: None,
            format: Some(Format::Json),
        };
        serde_json::from_str(&status::run(&ctx, args).await.unwrap()).unwrap()
    }

    let tmp = tempfile::tempdir().unwrap();
    let t = tree(tmp.path(), "poky", false);
    let cfg = Config {
        init_script: Some(fakebb().join(INIT_SCRIPT_NAME)),
        ..enabled()
    };
    let v = report(cfg, t.ws.clone(), &tmp.path().join("cache")).await;
    let bb = &v["bitbake"];
    assert_eq!(bb["environment"], true, "{v}");
    assert_eq!(bb["env_mode"], "init-script");
    assert_eq!(bb["env_build_dir"], t.build.display().to_string());
    assert!(bb["env_problem"].is_null());

    // Without a script: not found, with the reason. (Skipped if this test process itself
    // runs inside an initialised environment, which detection would then accept.)
    let host = HostEnv::from_process();
    if host.builddir.is_none() && host.bbpath.is_none() {
        let v = report(enabled(), t.ws.clone(), &tmp.path().join("cache2")).await;
        let bb = &v["bitbake"];
        assert_eq!(bb["environment"], false, "{v}");
        let problem = bb["env_problem"].as_str().unwrap();
        assert!(problem.contains("set init_script"), "{v}");
    }
}
