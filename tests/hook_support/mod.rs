//! Helpers for the hook tests: recorded events from `tests/fixtures/hooks/`, an index of
//! the fixture workspace in a temporary cache root, and running `sous-chef hook`.
//!
//! Recorded events use `{{WS}}` for the absolute path of `tests/fixtures/ws`; [`event`]
//! substitutes it.
#![allow(dead_code)]
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

use serde_json::Value;
use sous_chef::config::{CliOverrides, Config};
use sous_chef::index::Index;
use sous_chef::workspace::Workspace;

pub fn ws_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/ws")
}

/// The recorded event `name` (without `.json`), with `{{WS}}` replaced by `ws`.
pub fn event_in(name: &str, ws: &Path) -> String {
    let p = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/hooks")
        .join(format!("{name}.json"));
    let text = std::fs::read_to_string(&p).unwrap_or_else(|e| panic!("{}: {e}", p.display()));
    let ws = ws.to_str().unwrap();
    assert!(
        !ws.contains('"') && !ws.contains('\\'),
        "path needs JSON escaping"
    );
    text.replace("{{WS}}", ws)
}

/// The recorded event `name` on the fixture workspace in the source tree.
pub fn event(name: &str) -> String {
    event_in(name, &ws_root())
}

/// The workspace as the hook discovers it: `sous-chef.toml` in `root`.
pub fn discover(root: &Path) -> Workspace {
    let cfg = Config::load(&CliOverrides {
        build_dir: None,
        config: None,
        layers: Vec::new(),
        work_dir: root.to_path_buf(),
    })
    .unwrap();
    Workspace::discover(&cfg).unwrap()
}

/// A temporary cache root holding a fresh index of the workspace at `root`.
pub fn indexed_cache(root: &Path) -> tempfile::TempDir {
    let cache = tempfile::tempdir().unwrap();
    let ws = discover(root);
    let mut idx = Index::open_in(cache.path(), &ws).unwrap();
    idx.refresh(&ws).unwrap();
    cache
}

/// Runs `sous-chef hook` with `stdin`, the cache root `cache` and a generous budget (debug
/// builds under a loaded test runner are slow; the budget itself is tested separately).
pub fn run_hook(stdin: &[u8], cache: &Path) -> Output {
    run_hook_with(stdin, cache, "5000")
}

pub fn run_hook_with(stdin: &[u8], cache: &Path, budget_ms: &str) -> Output {
    let mut child = Command::new(env!("CARGO_BIN_EXE_sous-chef"))
        .arg("hook")
        .current_dir(cache)
        .env("XDG_CACHE_HOME", cache)
        .env("SOUS_CHEF_HOOK_BUDGET_MS", budget_ms)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child.stdin.take().unwrap().write_all(stdin).unwrap();
    child.wait_with_output().unwrap()
}

/// Asserts the run exited 0 and printed nothing on stdout (a valid no-op).
pub fn assert_noop(out: &Output) {
    assert_eq!(out.status.code(), Some(0), "{out:?}");
    assert!(
        out.stdout.is_empty(),
        "expected no output, got {:?}",
        String::from_utf8_lossy(&out.stdout)
    );
}

/// Asserts the run exited 0 and printed one PostToolUse context object; returns the text.
pub fn context_of(out: &Output) -> String {
    assert_eq!(out.status.code(), Some(0), "{out:?}");
    let text = String::from_utf8(out.stdout.clone()).unwrap();
    let v: Value = serde_json::from_str(&text)
        .unwrap_or_else(|e| panic!("stdout is not one JSON object ({e}): {text:?}"));
    let top = v.as_object().unwrap();
    assert_eq!(top.len(), 1, "{text}");
    let spec = top["hookSpecificOutput"].as_object().unwrap();
    assert_eq!(spec["hookEventName"], "PostToolUse");
    assert!(
        !spec.contains_key("updatedToolOutput") && !spec.contains_key("updatedMCPToolOutput"),
        "the hook must not replace tool output: {text}"
    );
    assert_eq!(spec.len(), 2, "{text}");
    let ctx = spec["additionalContext"].as_str().unwrap().to_string();
    assert!(ctx.chars().count() <= 10_000);
    ctx
}

/// The hook's debug log for the workspace at `root` under `cache`.
pub fn log_text(cache: &Path, root: Option<&Path>) -> String {
    let dir = match root {
        Some(r) => sous_chef::index::cache_dir_in(cache, &discover(r)),
        None => cache.join("sous-chef"),
    };
    std::fs::read_to_string(dir.join("hook.log")).unwrap_or_default()
}
