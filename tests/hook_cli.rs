//! T7.1: `sous-chef hook` reads a recorded event on stdin, dispatches, and always exits 0.
//! Malformed input, unknown tools, a missing index and a blown budget change nothing.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod hook_support;

use hook_support::*;
use serde_json::Value;

/// Every recorded event, by tool.
const EVENTS: &[&str] = &[
    "grep_content",
    "grep_files",
    "read_cut",
    "read_cut_both",
    "read_whole_recipe",
    "read_whole_inc",
    "bash",
    "edit",
    "write",
    "unknown_tool",
];

#[test]
fn recorded_events_have_the_documented_shape() {
    for name in EVENTS {
        let v: Value = serde_json::from_str(&event(name)).unwrap();
        for key in [
            "session_id",
            "transcript_path",
            "cwd",
            "hook_event_name",
            "tool_name",
            "tool_input",
            "tool_response",
            "tool_use_id",
        ] {
            assert!(v.get(key).is_some(), "{name}: missing {key}");
        }
        assert_eq!(v["hook_event_name"], "PostToolUse", "{name}");
        assert!(v["cwd"].as_str().unwrap().starts_with('/'), "{name}");
    }
}

#[test]
fn every_recorded_event_gives_a_valid_response() {
    let cache = indexed_cache(&ws_root());
    for name in EVENTS {
        let out = run_hook(event(name).as_bytes(), cache.path());
        assert_eq!(out.status.code(), Some(0), "{name}: {out:?}");
        if out.stdout.is_empty() {
            continue;
        }
        // Not a no-op: it must be one context object and nothing else.
        context_of(&out);
        assert!(
            name.starts_with("grep") || name.starts_with("read"),
            "{name} should be a no-op until its handler exists"
        );
    }
}

#[test]
fn stub_handlers_are_no_ops() {
    let cache = indexed_cache(&ws_root());
    for name in ["bash", "edit", "write"] {
        assert_noop(&run_hook(event(name).as_bytes(), cache.path()));
    }
}

#[test]
fn malformed_input_is_a_no_op() {
    let cache = tempfile::tempdir().unwrap();
    let inputs: &[&[u8]] = &[
        b"",
        b"   \n",
        b"{not json",
        b"[]",
        b"\"PostToolUse\"",
        b"{}",
        b"{\"hook_event_name\": 7}",
        b"{\"hook_event_name\": \"PostToolUse\", \"tool_name\": \"Read\"}",
        b"{\"hook_event_name\": \"PostToolUse\", \"tool_name\": \"Read\", \"cwd\": \"/\", \
          \"tool_input\": 3, \"tool_response\": [1, 2]}",
        b"\xff\xfe\x00garbage",
    ];
    for input in inputs {
        assert_noop(&run_hook(input, cache.path()));
    }
    // A truncated recorded event.
    let ev = event("read_cut");
    assert_noop(&run_hook(&ev.as_bytes()[..ev.len() / 2], cache.path()));
    // The reason is logged to the cache directory, not to stdout.
    assert!(log_text(cache.path(), None).contains("not JSON"));
}

#[test]
fn unknown_tool_and_other_events_are_no_ops() {
    let cache = indexed_cache(&ws_root());
    assert_noop(&run_hook(event("unknown_tool").as_bytes(), cache.path()));
    let pre = event("read_cut").replace("\"PostToolUse\"", "\"PreToolUse\"");
    assert_noop(&run_hook(pre.as_bytes(), cache.path()));
    let mcp = event("read_cut").replace("\"Read\"", "\"mcp__sous-chef__item\"");
    assert_noop(&run_hook(mcp.as_bytes(), cache.path()));
}

#[test]
fn missing_index_is_a_no_op_and_never_creates_one() {
    let cache = tempfile::tempdir().unwrap();
    for name in EVENTS {
        assert_noop(&run_hook(event(name).as_bytes(), cache.path()));
    }
    let idx = sous_chef::index::cache_dir_in(cache.path(), &discover(&ws_root()))
        .join(sous_chef::index::INDEX_FILE);
    assert!(!idx.exists(), "the hook must not build an index");
}

#[test]
fn cwd_outside_any_workspace_is_a_no_op() {
    let cache = indexed_cache(&ws_root());
    let elsewhere = tempfile::tempdir().unwrap();
    let ev: Value = serde_json::from_str(&event("read_cut")).unwrap();
    let mut ev = ev;
    ev["cwd"] = Value::String(elsewhere.path().to_str().unwrap().to_string());
    assert_noop(&run_hook(ev.to_string().as_bytes(), cache.path()));
}

#[test]
fn over_budget_is_a_no_op() {
    let cache = indexed_cache(&ws_root());
    for name in EVENTS {
        assert_noop(&run_hook_with(event(name).as_bytes(), cache.path(), "0"));
    }
}

#[test]
fn library_entry_point_never_errors() {
    let cache = tempfile::tempdir().unwrap();
    let opts = sous_chef::hook::Options {
        cache_root: Some(cache.path().to_path_buf()),
        ..Default::default()
    };
    assert_eq!(sous_chef::hook::run("{", &opts), None);
    assert_eq!(sous_chef::hook::run("", &opts), None);
    assert_eq!(sous_chef::hook::run(&event("unknown_tool"), &opts), None);
}
