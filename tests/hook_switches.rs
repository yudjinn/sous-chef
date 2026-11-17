//! `[hooks]` in `sous-chef.toml`: `grep_read`, `bash` and `edit` switch the handlers off.
//! A switched-off hook exits 0 with no output. The grep-through-Bash annotation follows
//! `grep_read`, the BitBake digest `bash`.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;
mod hook_support;

use std::path::{Path, PathBuf};

use hook_support::*;
use serde_json::Value;

/// A copy of the fixture workspace whose `sous-chef.toml` ends with `hooks`, indexed.
fn ws_with(hooks: &str) -> (tempfile::TempDir, PathBuf, tempfile::TempDir) {
    let (tmp, root) = common::temp_ws();
    let root = root.canonicalize().unwrap();
    let toml = root.join("sous-chef.toml");
    let text = std::fs::read_to_string(&toml).unwrap();
    std::fs::write(&toml, format!("{text}\n[hooks]\n{hooks}\n")).unwrap();
    let cache = indexed_cache(&root);
    (tmp, root, cache)
}

fn run(name: &str, root: &Path, cache: &Path) -> std::process::Output {
    run_hook(event_in(name, root).as_bytes(), cache)
}

/// The edit event on a line that `override/legacy-syntax` flags.
fn legacy_edit(root: &Path) -> String {
    let abs = root.join("meta-mine/recipes-core/hello/hello_1.1.bbappend");
    let text = std::fs::read_to_string(&abs).unwrap();
    std::fs::write(&abs, format!("{text}\nEXTRA_OECONF_append = \" --x\"\n")).unwrap();
    let mut ev: Value = serde_json::from_str(&event_in("edit", root)).unwrap();
    ev["tool_input"]["file_path"] = Value::from(abs.to_str().unwrap());
    ev["tool_response"]["filePath"] = Value::from(abs.to_str().unwrap());
    ev.to_string()
}

fn replaced(out: &std::process::Output) -> bool {
    String::from_utf8_lossy(&out.stdout).contains("updatedToolOutput")
}

#[test]
fn everything_is_on_by_default() {
    let (_t, root, cache) = ws_with("");
    context_of(&run("grep_content", &root, cache.path()));
    context_of(&run("read_cut", &root, cache.path()));
    context_of(&run("bash_grep_rn", &root, cache.path()));
    assert!(replaced(&run("bash_build_failure", &root, cache.path())));
    let ctx = context_of(&run_hook(legacy_edit(&root).as_bytes(), cache.path()));
    assert!(ctx.contains("override/legacy-syntax"), "{ctx}");
}

#[test]
fn grep_read_off_silences_grep_read_and_grep_through_bash() {
    let (_t, root, cache) = ws_with("grep_read = false");
    assert_noop(&run("grep_content", &root, cache.path()));
    assert_noop(&run("read_cut", &root, cache.path()));
    assert_noop(&run("bash_grep_rn", &root, cache.path()));
    assert_noop(&run("bash_rg", &root, cache.path()));
    // The others still run.
    assert!(replaced(&run("bash_build_failure", &root, cache.path())));
    context_of(&run_hook(legacy_edit(&root).as_bytes(), cache.path()));
    // No workspace was discovered for the switched-off calls: their log line is in the
    // cache root's log.
    let log = log_text(cache.path(), None);
    assert!(log.contains("switched off by [hooks] grep_read"), "{log}");
}

#[test]
fn bash_off_silences_the_digest_only() {
    let (_t, root, cache) = ws_with("bash = false");
    assert_noop(&run("bash_build_failure", &root, cache.path()));
    context_of(&run("bash_grep_rn", &root, cache.path()));
    context_of(&run("grep_content", &root, cache.path()));
}

#[test]
fn edit_off_silences_the_edit_hook() {
    let (_t, root, cache) = ws_with("edit = false");
    assert_noop(&run_hook(legacy_edit(&root).as_bytes(), cache.path()));
    context_of(&run("read_cut", &root, cache.path()));
}

#[test]
fn an_unknown_key_switches_nothing_off_and_fails_open() {
    // The configuration does not load; hooks that need the workspace fail open, and
    // nothing is printed.
    let (_t, root, cache) = ws_with("");
    let toml = root.join("sous-chef.toml");
    let text = std::fs::read_to_string(&toml).unwrap();
    std::fs::write(&toml, format!("{text}grep = false\n")).unwrap();
    assert_noop(&run("grep_content", &root, cache.path()));
    assert_noop(&run("bash_grep_rn", &root, cache.path()));
    // No workspace could be discovered: the log is the cache root's.
    let log = log_text(cache.path(), None);
    assert!(log.contains("failed open"), "{log}");
    assert!(!log.contains("switched off"), "{log}");
}
