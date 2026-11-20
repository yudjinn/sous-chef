//! The Bash and Edit hooks on events captured from a live Claude Code session (real-checks,
//! 2026-11-20; Claude Code 2.1.328, `--model haiku`, the plugin loaded with `--plugin-dir`;
//! docs/live-verification.md, "The Bash and Edit hooks in a live session"). The recorded
//! files are in `tests/fixtures/outputs/live/hooks/live_*`: the events as the session gave
//! them to a capture hook, what `sous-chef hook` answered and what the model was shown.
//!
//! The Bash event is the one that mattered: Claude Code passed the hook only the first
//! 29 999 of the output's 33 946 bytes, with `persistedOutputPath` (the whole output, in a
//! file it later removes) and `persistedOutputSize`, fields the hooks reference does not
//! list. These tests replay it on a copy of the fixture workspace with that file put back.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;
mod hook_support;

use std::path::{Path, PathBuf};

use hook_support::*;
use serde_json::Value;

fn recorded(name: &str) -> String {
    std::fs::read_to_string(
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/outputs/live/hooks")
            .join(name),
    )
    .unwrap()
}

fn json(name: &str) -> Value {
    serde_json::from_str(&recorded(name)).unwrap()
}

/// The whole console the session's `./bitbake curl` printed (a script that `cat`s the real
/// y1-build console): the file Claude Code persisted held exactly these bytes (compared
/// with `cmp` during the run).
fn console() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/outputs/live/y1-build/log/console-configure-fail.out")
}

/// The captured Bash event, moved to `root`, with `persistedOutputPath` set to `persisted`.
fn bash_event(root: &Path, persisted: &Path) -> String {
    let mut ev = json("live_bash_cut_curl_configure.event.json");
    ev["cwd"] = Value::String(root.display().to_string());
    ev["tool_response"]["persistedOutputPath"] = Value::String(persisted.display().to_string());
    ev.to_string()
}

fn updated(out: &std::process::Output) -> serde_json::Map<String, Value> {
    assert_eq!(out.status.code(), Some(0), "{out:?}");
    let v: Value = serde_json::from_slice(&out.stdout).unwrap();
    v["hookSpecificOutput"]["updatedToolOutput"]
        .as_object()
        .unwrap()
        .clone()
}

/// The event as captured: the cut stdout, the two extra fields.
#[test]
fn the_captured_bash_event_is_cut_and_names_the_whole_output() {
    let ev = json("live_bash_cut_curl_configure.event.json");
    assert_eq!(ev["tool_input"]["command"], "./bitbake curl");
    let r = ev["tool_response"].as_object().unwrap();
    let stdout = r["stdout"].as_str().unwrap();
    assert_eq!(stdout.len(), 29_999);
    assert_eq!(r["persistedOutputSize"], 33_946);
    assert_eq!(r["noOutputExpected"], false);
    let whole = std::fs::read_to_string(console()).unwrap();
    assert_eq!(whole.len(), 33_946);
    assert!(whole.starts_with(stdout));
    // The first error of the build is after the cut.
    assert!(!stdout.contains("configure: error:"));
    assert!(whole.contains("configure: error: libpsl"));
}

/// With the saved file there (as during the session), the digest is made from the whole
/// output, and the reply drops the two fields: Claude Code then showed the digest itself
/// (525 bytes), not an "Output too large" preview. The reply equals the one recorded live.
#[test]
fn a_cut_bash_output_is_digested_from_the_persisted_file() {
    let (tmp, root) = common::temp_ws();
    let root = std::fs::canonicalize(root).unwrap();
    let persisted = tmp.path().join("b6mb7zedj.txt");
    std::fs::copy(console(), &persisted).unwrap();
    let cache = indexed_cache(&root);
    let out = run_hook(bash_event(&root, &persisted).as_bytes(), cache.path());
    let u = updated(&out);
    assert!(!u.contains_key("persistedOutputPath"), "{u:?}");
    assert!(!u.contains_key("persistedOutputSize"), "{u:?}");
    assert_eq!(u["noOutputExpected"], false);
    let stdout = u["stdout"].as_str().unwrap();
    assert!(
        stdout.starts_with("sous-chef: bitbake output (401 lines, 33946 bytes) stored as "),
        "{stdout}"
    );
    assert!(
        stdout.contains("configure: error: libpsl libs and/or directories were not found"),
        "{stdout}"
    );
    let live = json("live_bash_cut_curl_configure.response.json");
    assert_eq!(
        stdout,
        live["hookSpecificOutput"]["updatedToolOutput"]["stdout"]
            .as_str()
            .unwrap()
    );
    // What the model was shown in the session: that text, nothing around it.
    assert_eq!(
        recorded("live_bash_cut_curl_configure.tool_result.txt").trim_end(),
        stdout.trim_end()
    );
}

/// Without the file (gone, or a client that does not write one) the cut stdout is all
/// there is: the digest of the first 29 999 bytes, as the session before the fix got it
/// ("configure failed", line 32), with the same handle.
#[test]
fn without_the_persisted_file_the_cut_output_is_digested() {
    let (tmp, root) = common::temp_ws();
    let root = std::fs::canonicalize(root).unwrap();
    let cache = indexed_cache(&root);
    let out = run_hook(
        bash_event(&root, &tmp.path().join("gone.txt")).as_bytes(),
        cache.path(),
    );
    let u = updated(&out);
    let stdout = u["stdout"].as_str().unwrap();
    let before = json("live_bash_cut_curl_configure.response_before_fix.json");
    assert_eq!(
        stdout,
        before["hookSpecificOutput"]["updatedToolOutput"]["stdout"]
            .as_str()
            .unwrap()
    );
    assert!(stdout.contains("(345 lines, 29999 bytes)"), "{stdout}");
    // Before the fix the reply kept the fields, and the model saw the digest as the
    // preview of a persisted output.
    assert!(
        before["hookSpecificOutput"]["updatedToolOutput"]
            .get("persistedOutputPath")
            .is_some()
    );
    assert!(
        recorded("live_bash_cut_curl_configure.tool_result_before_fix.txt")
            .starts_with("<persisted-output>\nOutput too large (33.2KB).")
    );
}

/// The captured Edit of `meta-core/recipes-core/hello/hello_1.1.bb` (a layer the
/// workspace does not list as writable): the follow-up says so, as in the session, where
/// the model relayed it.
#[test]
fn the_captured_edit_gets_the_live_follow_up() {
    let (_tmp, root) = common::temp_ws();
    let root = std::fs::canonicalize(root).unwrap();
    let mut ev = json("live_edit_meta_core.event.json");
    let file = root.join("meta-core/recipes-core/hello/hello_1.1.bb");
    let old = ev["tool_input"]["old_string"].as_str().unwrap().to_string();
    let new = ev["tool_input"]["new_string"].as_str().unwrap().to_string();
    let text = std::fs::read_to_string(&file).unwrap();
    assert_eq!(text, ev["tool_response"]["originalFile"].as_str().unwrap());
    let cache = indexed_cache(&root);
    std::fs::write(&file, text.replacen(&old, &new, 1)).unwrap();
    ev["cwd"] = Value::String(root.display().to_string());
    ev["tool_input"]["file_path"] = Value::String(file.display().to_string());
    ev["tool_response"]["filePath"] = Value::String(file.display().to_string());
    let ctx = context_of(&run_hook(ev.to_string().as_bytes(), cache.path()));
    let live = json("live_edit_meta_core.response.json");
    assert_eq!(
        ctx,
        live["hookSpecificOutput"]["additionalContext"]
            .as_str()
            .unwrap()
    );
    assert!(
        ctx.starts_with("meta-core is not a writable layer"),
        "{ctx}"
    );
}
