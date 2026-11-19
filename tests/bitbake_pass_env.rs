//! z1-env: what reaches BitBake from the environment sous-chef was started in, in each
//! runner mode, end to end: `sous-chef serve` is started with `TMPDIR` (every name of
//! `STRIPPED_ENV`), `MACHINE` and `DL_DIR` in **its** environment (set on that child process
//! only; the test process's own environment is never changed), the `bitbake` tool runs the
//! fake BitBake (`tests/fixtures/fakebb/`), and the fake writes the environment it got to
//! the file `FAKEBB_ENV_OUT` names. Also `[bitbake] pass_env` and the `status` row.
#![cfg(unix)]
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;
mod kas_support;

use std::collections::BTreeMap;
use std::ffi::OsString;
use std::io::{BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc::{Receiver, channel};
use std::thread;
use std::time::{Duration, Instant};

use serde_json::{Value, json};
use sous_chef::bitbake::env::STRIPPED_ENV;

const TIMEOUT: Duration = Duration::from_secs(60);

/// A `sous-chef serve` over stdio.
struct Server {
    child: Child,
    stdin: Option<ChildStdin>,
    lines: Receiver<String>,
    next_id: u64,
    _cache: tempfile::TempDir,
}

impl Server {
    /// `sous-chef serve` in `dir` with exactly `env` added to the inherited environment, after
    /// removing the variables that would change which mode is detected or what the test
    /// checks, so a test run from a build shell or with a `TMPDIR` of its own gives the same
    /// result.
    fn start(dir: &Path, env: &[(&str, OsString)]) -> Server {
        let cache = tempfile::tempdir().unwrap();
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_sous-chef"));
        cmd.arg("serve").current_dir(dir);
        for v in [
            "BUILDDIR",
            "BBPATH",
            "MACHINE",
            "DL_DIR",
            "KAS_WORK_DIR",
            "KAS_BUILD_DIR",
            "KAS_MACHINE",
            "KAS_DISTRO",
            "KAS_TARGET",
            "KAS_TASK",
        ]
        .into_iter()
        .chain(STRIPPED_ENV)
        {
            cmd.env_remove(v);
        }
        cmd.env("XDG_CACHE_HOME", cache.path());
        for (k, v) in env {
            cmd.env(k, v);
        }
        let mut child = cmd
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("spawning sous-chef serve");
        let stdout = child.stdout.take().unwrap();
        let (tx, rx) = channel();
        thread::spawn(move || {
            for line in BufReader::new(stdout).lines() {
                let Ok(line) = line else { break };
                if tx.send(line).is_err() {
                    break;
                }
            }
        });
        let mut stderr = child.stderr.take().unwrap();
        thread::spawn(move || {
            let mut sink = String::new();
            let _ = stderr.read_to_string(&mut sink);
        });
        let stdin = child.stdin.take();
        let mut s = Server {
            child,
            stdin,
            lines: rx,
            next_id: 1,
            _cache: cache,
        };
        s.request(
            "initialize",
            json!({
                "protocolVersion": "2025-06-18",
                "capabilities": {},
                "clientInfo": {"name": "sous-chef-test", "version": "0"}
            }),
        );
        s.send(&json!({"jsonrpc": "2.0", "method": "notifications/initialized"}));
        s
    }

    fn send(&mut self, msg: &Value) {
        let stdin = self.stdin.as_mut().unwrap();
        writeln!(stdin, "{msg}").unwrap();
        stdin.flush().unwrap();
    }

    fn request(&mut self, method: &str, params: Value) -> Value {
        let id = self.next_id;
        self.next_id += 1;
        self.send(&json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params}));
        let deadline = Instant::now() + TIMEOUT;
        loop {
            let left = deadline.saturating_duration_since(Instant::now());
            let line = self
                .lines
                .recv_timeout(left)
                .unwrap_or_else(|e| panic!("no response to {method}: {e}"));
            let msg: Value = serde_json::from_str(&line).unwrap();
            if msg["id"] == json!(id) {
                assert!(msg.get("error").is_none(), "{method} failed: {msg}");
                return msg["result"].clone();
            }
        }
    }

    /// The text of a tool call's result.
    fn call(&mut self, tool: &str, args: Value) -> String {
        let r = self.request("tools/call", json!({"name": tool, "arguments": args}));
        r["content"][0]["text"].as_str().unwrap().to_string()
    }

    /// `status`'s `bitbake` row (its mode).
    fn mode(&mut self) -> String {
        let text = self.call("status", json!({}));
        text.lines()
            .find(|l| l.starts_with("bitbake\t"))
            .unwrap_or_else(|| panic!("no bitbake row: {text}"))
            .to_string()
    }

    /// `bitbake op=task target=hello task=configure args=["force"]` (`bitbake -c configure
    /// -f hello`, as in the live check), waited for; the environment the fake BitBake got.
    fn bitbake_env(&mut self, out: &Path) -> BTreeMap<String, String> {
        let _ = std::fs::remove_file(out);
        let text = self.call(
            "bitbake",
            json!({"op": "task", "target": "hello", "task": "configure", "args": ["force"], "wait": 30}),
        );
        assert!(text.contains("state\tsucceeded"), "{text}");
        let env = std::fs::read_to_string(out)
            .unwrap_or_else(|e| panic!("the fake BitBake wrote no environment ({e}): {text}"));
        env.lines()
            .filter_map(|l| l.split_once('='))
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }

    fn finish(mut self) {
        drop(self.stdin.take());
        let deadline = Instant::now() + TIMEOUT;
        while self.child.try_wait().unwrap().is_none() {
            if Instant::now() > deadline {
                let _ = self.child.kill();
                panic!("server did not exit after stdin closed");
            }
            thread::sleep(Duration::from_millis(20));
        }
    }
}

fn fakebb(rel: &str) -> PathBuf {
    common::fixture_path("fakebb").join(rel)
}

/// `<dir>:/usr/bin:/bin`.
fn path_with(dir: &Path) -> OsString {
    std::env::join_paths([dir.to_path_buf(), PathBuf::from("/usr/bin"), "/bin".into()]).unwrap()
}

/// What the server is started with: every stripped name, two of BitBake's own workflow
/// variables, and where the fake writes its environment.
fn server_env(scratch: &Path, out: &Path) -> Vec<(&'static str, OsString)> {
    let mut env: Vec<(&'static str, OsString)> = STRIPPED_ENV
        .iter()
        .map(|n| {
            (
                *n,
                scratch.join(format!("{n}-of-the-server")).into_os_string(),
            )
        })
        .collect();
    env.push(("MACHINE", "board-from-env".into()));
    env.push(("DL_DIR", scratch.join("downloads").into_os_string()));
    env.push(("FAKEBB_ENV_OUT", out.as_os_str().to_owned()));
    env
}

/// Every stripped name is missing (unless `passed`), `MACHINE` and `DL_DIR` arrived.
fn check(mode: &str, got: &BTreeMap<String, String>, scratch: &Path, passed: &[&str]) {
    for n in STRIPPED_ENV {
        if passed.contains(&n) {
            assert_eq!(
                got.get(n).map(String::as_str),
                Some(scratch.join(format!("{n}-of-the-server")).to_str().unwrap()),
                "{mode}: {n} listed in pass_env did not reach BitBake: {got:?}"
            );
        } else {
            assert!(!got.contains_key(n), "{mode}: {n} reached BitBake: {got:?}");
        }
    }
    assert_eq!(
        got.get("MACHINE").map(String::as_str),
        Some("board-from-env"),
        "{mode}: {got:?}"
    );
    assert_eq!(
        got.get("DL_DIR").map(String::as_str),
        Some(scratch.join("downloads").to_str().unwrap()),
        "{mode}: {got:?}"
    );
}

/// A copy of the fixture workspace with `sous-chef.toml` = `build_dir = "build"` + `extra`.
fn workspace(extra: &str) -> (tempfile::TempDir, PathBuf) {
    let (tmp, root) = common::temp_ws();
    let root = root.canonicalize().unwrap();
    std::fs::write(
        root.join("sous-chef.toml"),
        format!("build_dir = \"build\"\n{extra}\n"),
    )
    .unwrap();
    (tmp, root)
}

fn init_script(script: &Path, pass_env: &str) -> String {
    format!(
        "init_script = \"{}\"\n[bitbake]\nenabled = true\n{pass_env}",
        script.display()
    )
}

/// Runs the init-script mode (`init_script` = the fake `oe-init-build-env`) with
/// `pass_env` (TOML line or empty) and checks the result.
fn init_script_mode(pass_env: &str, passed: &[&str]) {
    let (tmp, root) = workspace(&init_script(&fakebb("oe-init-build-env"), pass_env));
    let out = tmp.path().join("env.out");
    let mut s = Server::start(&root, &server_env(tmp.path(), &out));
    assert!(s.mode().contains("(init-script)"), "{}", s.mode());
    let got = s.bitbake_env(&out);
    check("init-script", &got, tmp.path(), passed);
    s.finish();
}

#[test]
fn init_script_mode_strips_tmpdir_and_keeps_bitbakes_own_variables() {
    init_script_mode("", &[]);
}

#[test]
fn a_pass_env_name_reaches_bitbake() {
    init_script_mode("pass_env = [\"TMPDIR\"]\n", &["TMPDIR"]);

    // kas mode too: kas would then hand it to BitBake (kas 5.5 `get_build_environ`).
    let p = kas_support::Project::after();
    std::fs::write(
        p.path("sous-chef.toml"),
        "kas = [\"kas/base.yaml\", \"kas/board.yaml\"]\n[bitbake]\npass_env = [\"TMPDIR\"]\n",
    )
    .unwrap();
    let scratch = tempfile::tempdir().unwrap();
    let out = scratch.path().join("env.out");
    let mut env = server_env(scratch.path(), &out);
    env.push(("PATH", path_with(&fakebb("kasbin"))));
    let mut s = Server::start(&p.root, &env);
    assert!(s.mode().contains("(kas)"), "{}", s.mode());
    let got = s.bitbake_env(&out);
    check("kas", &got, scratch.path(), &["TMPDIR"]);
    s.finish();
}
