//! T9.3: the Bash hook digests `kas build` (and `kas shell -c 'bitbake ...'`) output like
//! BitBake's own, and leaves `kas checkout` / `kas dump` alone.
//!
//! The recorded event `bash_kas_build.json` was written by hand in the documented
//! `PostToolUse` shape: `bash_build_failure.json`'s BitBake console output after five kas
//! log lines, with the command `kas build kas/kiss.yaml:kas/dogbonedark.yaml 2>&1`. It was
//! not captured from a live session.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;
mod hook_support;

use hook_support::*;
use serde_json::Value;

fn replaced(out: &std::process::Output) -> Value {
    assert_eq!(out.status.code(), Some(0), "{out:?}");
    let v: Value = serde_json::from_slice(&out.stdout)
        .unwrap_or_else(|e| panic!("not one JSON object ({e}): {out:?}"));
    v["hookSpecificOutput"]["updatedToolOutput"].clone()
}

#[test]
fn a_failing_kas_build_is_digested() {
    let cache = indexed_cache(&ws_root());
    let ev = event("bash_kas_build");
    let original: Value = serde_json::from_str(&ev).unwrap();
    assert!(
        original["tool_input"]["command"]
            .as_str()
            .unwrap()
            .starts_with("kas build ")
    );
    let len = original["tool_response"]["stdout"].as_str().unwrap().len();
    let r = replaced(&run_hook(ev.as_bytes(), cache.path()));
    let stdout = r["stdout"].as_str().unwrap();
    assert!(
        stdout.starts_with(&format!(
            "sous-chef: bitbake output (120 lines, {len} bytes) stored as h:"
        )),
        "{stdout}"
    );
    for want in ["recipe\thello\n", "task\tdo_compile\n", "oe_runmake failed"] {
        assert!(stdout.contains(want), "{want:?} not in:\n{stdout}");
    }
    assert_eq!(r["stderr"], "");
}

#[test]
fn kas_shell_running_bitbake_is_digested_and_checkout_is_not() {
    let cache = indexed_cache(&ws_root());
    let mut ev: Value = serde_json::from_str(&event("bash_kas_build")).unwrap();
    ev["tool_input"]["command"] =
        Value::String("kas shell kas/kiss.yaml:kas/dogbonedark.yaml -c 'bitbake hello'".into());
    let r = replaced(&run_hook(ev.to_string().as_bytes(), cache.path()));
    assert!(
        r["stdout"]
            .as_str()
            .unwrap()
            .starts_with("sous-chef: bitbake output")
    );

    for c in [
        "kas checkout kas/kiss.yaml:kas/dogbonedark.yaml",
        "kas dump kas/kiss.yaml:kas/dogbonedark.yaml",
        "kas shell kas/kiss.yaml -c 'runqemu nographic'",
    ] {
        ev["tool_input"]["command"] = Value::String(c.into());
        assert_noop(&run_hook(ev.to_string().as_bytes(), cache.path()));
    }
}
