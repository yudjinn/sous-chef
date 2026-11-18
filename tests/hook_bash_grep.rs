//! A `grep -n` / `rg -n` run through Bash gets the Grep annotation as `additionalContext`;
//! the output is never replaced, and anything not confidently recognised is left alone.
//!
//! The recorded events `bash_grep_rn.json`, `bash_rg.json`, `bash_grep_pipeline.json` and
//! `bash_grep_other_files.json` were written by hand in the documented `PostToolUse` Bash
//! shape (see `plugin/README.md`); they were not captured from a live session. Their
//! `stdout` is the real output of GNU grep 3.12 and ripgrep 15.2.0 run in
//! `tests/fixtures/ws` (relative paths, as the commands name relative directories).
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;
mod hook_support;

use std::time::{Duration, Instant};

use hook_support::*;
use serde_json::Value;
use sous_chef::hook::grep_read::MAX_HITS;

#[test]
fn grep_rn_over_recipes_is_annotated() {
    let cache = indexed_cache(&ws_root());
    let ctx = context_of(&run_hook(event("bash_grep_rn").as_bytes(), cache.path()));
    // meta-legacy is not in BBLAYERS: its hits are outside the workspace.
    assert_eq!(
        ctx,
        "sous-chef index context for the Yocto hits above:\n\
         meta-core:recipes-core/hello/hello.inc:16 in [do_install:append 15-18]\n\
         meta-mine:recipes-app/app/app_0.1.bb:26 in [do_install 25-29]\n\
         recipe app_0.1.bb (meta-mine): 0 bbappends, 0 included files; \
         `recipe app` / `var <NAME> recipe=app` give the merged view."
    );
}

#[test]
fn rg_n_with_a_pattern_is_annotated() {
    let cache = indexed_cache(&ws_root());
    let ctx = context_of(&run_hook(event("bash_rg").as_bytes(), cache.path()));
    assert_eq!(
        ctx,
        "sous-chef index context for the Yocto hits above:\n\
         meta-mine:recipes-core/hello/hello_1.1.bbappend:8 in [do_install:append 8-10]\n\
         meta-core:classes-recipe/autotoolsish.bbclass:26 in [autotoolsish_do_install 26-28]\n\
         meta-core:recipes-core/hello/hello.inc:15 in [do_install:append 15-18]\n\
         meta-mine:recipes-app/app/app_0.1.bb:25 in [do_install 25-29]\n\
         meta-core:classes/base.bbclass:47 in [base_do_install 47-49]\n\
         recipe hello_1.1.bb (meta-core): 2 bbappends, 1 included file; \
         `recipe hello` / `var <NAME> recipe=hello` give the merged view.\n\
         recipe app_0.1.bb (meta-mine): 0 bbappends, 0 included files; \
         `recipe app` / `var <NAME> recipe=app` give the merged view."
    );
}

#[test]
fn a_pipeline_is_untouched() {
    let cache = indexed_cache(&ws_root());
    let ev = event("bash_grep_pipeline");
    let v: Value = serde_json::from_str(&ev).unwrap();
    // The same output as the annotated event; only the command differs.
    let plain: Value = serde_json::from_str(&event("bash_grep_rn")).unwrap();
    assert_eq!(v["tool_response"], plain["tool_response"]);
    assert_noop(&run_hook(ev.as_bytes(), cache.path()));
}

#[test]
fn grep_on_other_files_is_untouched() {
    let cache = indexed_cache(&ws_root());
    assert_noop(&run_hook(
        event("bash_grep_other_files").as_bytes(),
        cache.path(),
    ));
}

#[test]
fn commands_it_does_not_recognise_are_untouched() {
    let cache = indexed_cache(&ws_root());
    let mut v: Value = serde_json::from_str(&event("bash_grep_rn")).unwrap();
    for command in [
        // No line numbers asked for: the output would not have them.
        "grep -r 'install -d' meta-core meta-mine meta-legacy",
        "rg 'install -d'",
        "cd meta-core && grep -rn 'install -d' .",
        "grep -rnc 'install -d' meta-core",
        "cat meta-core/recipes-core/hello/hello.inc",
    ] {
        v["tool_input"]["command"] = Value::from(command);
        assert_noop(&run_hook(v.to_string().as_bytes(), cache.path()));
    }
    // Recognised, but output that does not parse.
    v["tool_input"]["command"] = Value::from("grep -rn x meta-core");
    v["tool_response"]["stdout"] = Value::from("Binary file x matches\ngrep: y: No such file");
    assert_noop(&run_hook(v.to_string().as_bytes(), cache.path()));
}

#[test]
fn context_lines_from_grep_are_skipped() {
    let cache = indexed_cache(&ws_root());
    let mut v: Value = serde_json::from_str(&event("bash_grep_rn")).unwrap();
    v["tool_input"]["command"] = Value::from("grep -rn -C1 'install -d' meta-core");
    // GNU grep 3.12 output of that command in the fixture workspace.
    v["tool_response"]["stdout"] = Value::from(
        "meta-core/recipes-core/hello/hello.inc-15-do_install:append() {\n\
         meta-core/recipes-core/hello/hello.inc:16:\tinstall -d ${D}${sysconfdir}\n\
         meta-core/recipes-core/hello/hello.inc-17-\techo \"greeting=hello\" > ${D}${sysconfdir}/hello.conf",
    );
    assert_eq!(
        context_of(&run_hook(v.to_string().as_bytes(), cache.path())),
        "sous-chef index context for the Yocto hits above:\n\
         meta-core:recipes-core/hello/hello.inc:16 in [do_install:append 15-18]"
    );
}

/// The event of a Bash `grep -rn` over the fixture with 400 hits.
fn four_hundred_hits() -> String {
    let ws = ws_root();
    let files = [
        "meta-core/recipes-core/hello/hello.inc",
        "meta-mine/recipes-app/app/app_0.1.bb",
        "meta-core/classes/base.bbclass",
        "meta-core/classes-recipe/autotoolsish.bbclass",
        "meta-mine/recipes-core/hello/hello_1.1.bbappend",
    ];
    let mut stdout = Vec::new();
    for f in files {
        let text = std::fs::read_to_string(ws.join(f)).unwrap();
        for (i, l) in text.lines().enumerate() {
            stdout.push(format!("{f}:{}:{l}", i + 1));
        }
    }
    while stdout.len() < 400 {
        let again = stdout.clone();
        stdout.extend(again);
    }
    stdout.truncate(400);
    let mut v: Value = serde_json::from_str(&event("bash_grep_rn")).unwrap();
    v["tool_input"]["command"] = Value::from("grep -rn '' meta-core meta-mine");
    v["tool_response"]["stdout"] = Value::from(stdout.join("\n"));
    v.to_string()
}

/// A grep over the fixture with a few hundred hits: the first [`MAX_HITS`] are looked at
/// and the context says the rest were not. Run with the generous budget of [`run_hook`]
/// (5 s): under a loaded test runner a debug build can miss the real 150 ms budget, and the
/// hook then drops its output on purpose, which is not what this test checks. Speed is
/// measured by the ignored [`timing`] test.
#[test]
fn hundreds_of_hits_are_capped() {
    let cache = indexed_cache(&ws_root());
    let ev = four_hundred_hits();
    let ctx = context_of(&run_hook(ev.as_bytes(), cache.path()));
    assert!(
        ctx.ends_with(&format!(
            "(only the first {MAX_HITS} hits were looked at; {} more output lines were not)",
            400 - MAX_HITS
        )),
        "{ctx}"
    );
    assert!(ctx.contains("in [do_install:append 15-18]"), "{ctx}");
    let log = log_text(cache.path(), Some(&ws_root()));
    assert!(log.contains("bash_grep"), "{log}");
}

/// The 400-hit grep within the real 150 ms budget, whole process (best of 3 after a warm-up).
#[test]
#[ignore = "timing; run with --ignored --nocapture"]
fn timing() {
    let cache = indexed_cache(&ws_root());
    let ev = four_hundred_hits();
    run_hook(ev.as_bytes(), cache.path());
    let mut best = Duration::MAX;
    let mut ctx = String::new();
    for _ in 0..3 {
        let t = Instant::now();
        let out = run_hook_with(ev.as_bytes(), cache.path(), "150");
        best = best.min(t.elapsed());
        ctx = context_of(&out);
    }
    assert!(ctx.contains("in [do_install:append 15-18]"), "{ctx}");
    assert!(best < Duration::from_millis(1000), "{best:?}");
    eprintln!("400-hit Bash grep, whole process (best of 3): {best:?}");
}
