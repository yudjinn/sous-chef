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

#[tokio::test]
async fn a_timeout_kills_the_whole_kas_process_group() {
    let p = Project::after();
    let env = kas_env(&p);
    let pidfile = p.path("pids");
    fs::write(
        p.path("fakekas.env"),
        format!(
            "export FAKEBB_SLEEP=30\nexport FAKEBB_PIDFILE={}\n",
            pidfile.display()
        ),
    )
    .unwrap();
    let start = Instant::now();
    let out = env
        .output("bitbake", &["-p"], Some(Duration::from_millis(1500)))
        .await
        .unwrap();
    assert_eq!(out.finish, Finish::TimedOut);
    assert!(start.elapsed() < Duration::from_secs(10));
    let pids = fs::read_to_string(&pidfile).unwrap();
    for pid in pids.split_whitespace() {
        // The fake bitbake and its sleep child are gone.
        assert!(
            !Path::new(&format!("/proc/{pid}")).exists()
                || fs::read_to_string(format!("/proc/{pid}/stat"))
                    .map(|s| s.contains(") Z "))
                    .unwrap_or(true),
            "process {pid} survived"
        );
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
