//! T7.2: Grep and Read annotation. Recorded events on the fixture workspace; the hook adds
//! `additionalContext` and never replaces or alters the tool output.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;
mod hook_support;

use std::path::Path;

use hook_support::*;
use serde_json::Value;

/// sha256 of every file under the workspace, to show the hook touched nothing.
fn snapshot(root: &Path) -> Vec<(std::path::PathBuf, Vec<u8>)> {
    common::files_under(root)
        .into_iter()
        .map(|p| {
            let b = std::fs::read(&p).unwrap();
            (p, b)
        })
        .collect()
}

#[test]
fn grep_content_hits_get_their_items_and_recipes() {
    let cache = indexed_cache(&ws_root());
    let before = snapshot(&ws_root());
    let ctx = context_of(&run_hook(event("grep_content").as_bytes(), cache.path()));
    assert_eq!(
        ctx,
        "sous-chef index context for the Yocto hits above:\n\
         meta-core:recipes-core/hello/hello.inc:16 in [do_install:append 15-18]\n\
         meta-mine:recipes-app/app/app_0.1.bb:26,27 in [do_install 25-29]\n\
         meta-mine:recipes-core/hello/hello_1.1.bbappend:9 in [do_install:append 8-10]\n\
         recipe app_0.1.bb (meta-mine): 0 bbappends, 0 included files; \
         `recipe app` / `var <NAME> recipe=app` give the merged view.\n\
         recipe hello_1.1.bb (meta-core): 2 bbappends, 1 included file; \
         `recipe hello` / `var <NAME> recipe=hello` give the merged view."
    );
    // meta-legacy is not in BBLAYERS: its hits are outside the workspace.
    assert!(!ctx.contains("oldapp"));
    assert_eq!(snapshot(&ws_root()), before);
}

/// Captured from Claude Code 2.1.326: paths relative to `cwd`, `numFiles` 0, `filenames`
/// empty, an extra `totalLines`; hits in ripgrep's order, one in meta-legacy (outside the
/// workspace).
#[test]
fn captured_grep_content_event() {
    let cache = indexed_cache(&ws_root());
    let ev = event("captured/grep_content");
    let v: Value = serde_json::from_str(&ev).unwrap();
    assert_eq!(v["tool_response"]["numFiles"], 0);
    let ctx = context_of(&run_hook(ev.as_bytes(), cache.path()));
    assert_eq!(
        ctx,
        "sous-chef index context for the Yocto hits above:\n\
         meta-mine:recipes-app/app/app_0.1.bb:26 in [do_install 25-29]\n\
         meta-core:recipes-core/hello/hello.inc:16 in [do_install:append 15-18]\n\
         recipe app_0.1.bb (meta-mine): 0 bbappends, 0 included files; \
         `recipe app` / `var <NAME> recipe=app` give the merged view."
    );
    // `numFiles` and `filenames` play no part in content mode.
    let mut v2 = v.clone();
    v2["tool_response"]["numFiles"] = Value::from(7);
    v2["tool_response"]["filenames"] =
        serde_json::json!(["meta-core/recipes-core/hello/hello_1.0.bb"]);
    assert_eq!(
        context_of(&run_hook(v2.to_string().as_bytes(), cache.path())),
        ctx
    );
}

/// Context lines (`-C 1`), as ripgrep 15.2.0 printed them for this search on the fixture
/// (`rg -n -C1 'install -d' meta-core`): `<path>-<line>-<text>` lines and `--` are skipped,
/// only the match line is annotated. Claude Code's Grep with `-C` was not captured; the
/// format is ripgrep's documented `--field-context-separator`.
#[test]
fn grep_context_lines_are_not_hits() {
    let cache = indexed_cache(&ws_root());
    let mut v: Value = serde_json::from_str(&event("captured/grep_content")).unwrap();
    v["tool_input"]["-C"] = Value::from(1);
    v["tool_response"]["content"] = Value::String(
        "meta-core/recipes-core/hello/hello.inc-15-do_install:append() {\n\
         meta-core/recipes-core/hello/hello.inc:16:\tinstall -d ${D}${sysconfdir}\n\
         meta-core/recipes-core/hello/hello.inc-17-\techo \"greeting=hello\" > ${D}${sysconfdir}/hello.conf\n\
         --\n\
         meta-mine/recipes-app/app/app_0.1.bb-29-}\n\
         meta-mine/recipes-app/app/app_0.1.bb-30-FOO = \"meta-core/recipes-core/hello/hello.inc:2:\""
            .into(),
    );
    let ctx = context_of(&run_hook(v.to_string().as_bytes(), cache.path()));
    assert_eq!(
        ctx,
        "sous-chef index context for the Yocto hits above:\n\
         meta-core:recipes-core/hello/hello.inc:16 in [do_install:append 15-18]"
    );
}

/// A relative search directory: the captured event (`path: "."`) printed paths relative to
/// `cwd`; whether they are relative to `cwd` or to the search directory for another
/// relative `path` was not captured, so both are tried, and a line is used only when one
/// of them names a file.
#[test]
fn grep_in_a_relative_directory_resolves_against_cwd_or_the_directory() {
    let cache = indexed_cache(&ws_root());
    let mut v: Value = serde_json::from_str(&event("captured/grep_content")).unwrap();
    v["tool_input"]["path"] = Value::from("meta-core");
    v["tool_response"]["content"] =
        Value::String("meta-core/recipes-core/hello/hello.inc:16:\tinstall -d".into());
    let want = "sous-chef index context for the Yocto hits above:\n\
                meta-core:recipes-core/hello/hello.inc:16 in [do_install:append 15-18]";
    assert_eq!(
        context_of(&run_hook(v.to_string().as_bytes(), cache.path())),
        want
    );
    v["tool_response"]["content"] =
        Value::String("recipes-core/hello/hello.inc:16:\tinstall -d".into());
    assert_eq!(
        context_of(&run_hook(v.to_string().as_bytes(), cache.path())),
        want
    );
}

#[test]
fn grep_files_mode_names_the_recipes() {
    let cache = indexed_cache(&ws_root());
    let ctx = context_of(&run_hook(event("grep_files").as_bytes(), cache.path()));
    let lines: Vec<&str> = ctx.lines().collect();
    assert_eq!(lines.len(), 4, "{ctx}");
    // The bbappends lead to the recipes they apply to: hello_%.bbappend applies to both
    // hello versions, hello_1.1.bbappend to 1.1 only.
    assert!(lines[1].starts_with("recipe hello_1.1.bb (meta-core): 2 bbappends, 1 included file;"));
    assert!(lines[2].starts_with("recipe hello_1.0.bb (meta-core): 1 bbappend, 1 included file;"));
    assert!(
        lines[3].starts_with("recipe libfoo-alt_1.0.bb (meta-vendor): 0 bbappends, 0 included")
    );
    // No line numbers, so no item annotations.
    assert!(!ctx.contains(" in ["));
}

#[test]
fn grep_relative_paths_and_single_file_searches() {
    let cache = indexed_cache(&ws_root());
    let ws = ws_root();
    let mut ev: Value = serde_json::from_str(&event("grep_content")).unwrap();
    let content = ev["tool_response"]["content"].as_str().unwrap().to_string();
    let prefix = format!("{}/", ws.display());
    ev["tool_response"]["content"] = Value::String(content.replace(&prefix, ""));
    let ctx = context_of(&run_hook(ev.to_string().as_bytes(), cache.path()));
    assert!(ctx.contains("meta-core:recipes-core/hello/hello.inc:16 in [do_install:append 15-18]"));

    // A search of one file: ripgrep prints no file name.
    let inc = ws.join("meta-core/recipes-core/hello/hello.inc");
    ev["tool_input"]["path"] = Value::String(inc.display().to_string());
    ev["tool_response"]["content"] =
        Value::String("15:do_install:append() {\n16:\tinstall -d ${D}${sysconfdir}".into());
    ev["tool_response"]["numFiles"] = Value::from(1);
    let ctx = context_of(&run_hook(ev.to_string().as_bytes(), cache.path()));
    assert_eq!(
        ctx,
        "sous-chef index context for the Yocto hits above:\n\
         meta-core:recipes-core/hello/hello.inc:15,16 in [do_install:append 15-18]"
    );
}

#[test]
fn grep_without_yocto_hits_is_silent() {
    let cache = indexed_cache(&ws_root());
    let mut ev: Value = serde_json::from_str(&event("grep_content")).unwrap();
    let app_c = ws_root().join("meta-mine/recipes-app/app/files/app.c");
    ev["tool_response"]["content"] = Value::String(format!("{}:3:int main(void)", app_c.display()));
    assert_noop(&run_hook(ev.to_string().as_bytes(), cache.path()));
    // Hits on single-line items say nothing about items.
    let bb = ws_root().join("meta-core/recipes-core/hello/hello_1.1.bb");
    ev["tool_response"]["content"] =
        Value::String(format!("{}:3:LIC_FILES_CHKSUM = \"...\"", bb.display()));
    let ctx = context_of(&run_hook(ev.to_string().as_bytes(), cache.path()));
    assert!(!ctx.contains(" in ["), "{ctx}");
    assert!(ctx.contains("recipe hello_1.1.bb"));
}

#[test]
fn read_that_cuts_an_item_says_where_it_ends() {
    let cache = indexed_cache(&ws_root());
    let ev = event("read_cut");
    let ctx = context_of(&run_hook(ev.as_bytes(), cache.path()));
    assert_eq!(
        ctx,
        "sous-chef index context for this read of meta-core:recipes-core/hello/hello.inc:1-16:\n\
         The read ends at line 16 inside [do_install:append 15-18], which ends at line 18 \
         (meta-core:recipes-core/hello/hello.inc:15-18)."
    );
    // The recorded output is the file's own lines, and the hook leaves it alone.
    let v: Value = serde_json::from_str(&ev).unwrap();
    let content = v["tool_response"]["file"]["content"].as_str().unwrap();
    let disk =
        std::fs::read_to_string(ws_root().join("meta-core/recipes-core/hello/hello.inc")).unwrap();
    let first16: Vec<&str> = disk.lines().take(16).collect();
    assert_eq!(content.as_bytes(), first16.join("\n").as_bytes());
    assert!(!ctx.contains(content.lines().next().unwrap()));
}

#[test]
fn read_cut_at_both_ends() {
    let cache = indexed_cache(&ws_root());
    let ctx = context_of(&run_hook(event("read_cut_both").as_bytes(), cache.path()));
    assert_eq!(
        ctx,
        "sous-chef index context for this read of meta-mine:recipes-app/app/app_0.1.bb:23-26:\n\
         The read starts at line 23 inside [do_compile 21-23], which begins at line 21 \
         (meta-mine:recipes-app/app/app_0.1.bb:21-23).\n\
         The read ends at line 26 inside [do_install 25-29], which ends at line 29 \
         (meta-mine:recipes-app/app/app_0.1.bb:25-29)."
    );
}

#[test]
fn whole_recipe_read_lists_the_other_contributors() {
    let cache = indexed_cache(&ws_root());
    let ctx = context_of(&run_hook(
        event("read_whole_recipe").as_bytes(),
        cache.path(),
    ));
    assert_eq!(
        ctx,
        "sous-chef index context for this read of meta-core:recipes-core/hello/hello_1.1.bb:1-9:\n\
         hello_1.1.bb is not the whole recipe; these files also contribute: \
         meta-core:recipes-core/hello/hello.inc (require), \
         meta-vendor:recipes-core/hello/hello_%.bbappend (bbappend), \
         meta-mine:recipes-core/hello/hello_1.1.bbappend (bbappend)."
    );
}

/// Captured from Claude Code 2.1.326: the content keeps the file's final newline and
/// `numLines` / `totalLines` say 10 for the 9-line recipe. The range is the file's 9 lines.
#[test]
fn captured_whole_recipe_read_names_the_real_lines() {
    let cache = indexed_cache(&ws_root());
    let ev = event("captured/read_whole_recipe");
    let v: Value = serde_json::from_str(&ev).unwrap();
    assert_eq!(v["tool_response"]["file"]["numLines"], 10);
    let ctx = context_of(&run_hook(ev.as_bytes(), cache.path()));
    assert_eq!(
        ctx,
        "sous-chef index context for this read of meta-core:recipes-core/hello/hello_1.1.bb:1-9:\n\
         hello_1.1.bb is not the whole recipe; these files also contribute: \
         meta-core:recipes-core/hello/hello.inc (require), \
         meta-vendor:recipes-core/hello/hello_%.bbappend (bbappend), \
         meta-mine:recipes-core/hello/hello_1.1.bbappend (bbappend)."
    );
}

/// Captured: `offset: 10, limit: 6`, `startLine` 10 (1-based): lines 10 to 15, the last
/// one opening `do_install:append`.
#[test]
fn captured_read_with_an_offset() {
    let cache = indexed_cache(&ws_root());
    let ctx = context_of(&run_hook(
        event("captured/read_offset").as_bytes(),
        cache.path(),
    ));
    assert_eq!(
        ctx,
        "sous-chef index context for this read of meta-core:recipes-core/hello/hello.inc:10-15:\n\
         The read ends at line 15 inside [do_install:append 15-18], which ends at line 18 \
         (meta-core:recipes-core/hello/hello.inc:15-18)."
    );
}

/// The captured offset read moved to the end of the file, in the captured shape (final
/// newline kept, one phantom line counted): the range stops at the file's last line, 20.
#[test]
fn a_read_to_the_end_of_the_file_stops_at_its_last_line() {
    let cache = indexed_cache(&ws_root());
    let mut v: Value = serde_json::from_str(&event("captured/read_offset")).unwrap();
    let inc =
        std::fs::read_to_string(ws_root().join("meta-core/recipes-core/hello/hello.inc")).unwrap();
    let tail: Vec<&str> = inc.lines().skip(15).collect();
    v["tool_input"]["offset"] = Value::from(16);
    v["tool_input"]["limit"] = Value::from(100);
    v["tool_response"]["file"]["content"] = Value::String(format!("{}\n", tail.join("\n")));
    v["tool_response"]["file"]["startLine"] = Value::from(16);
    v["tool_response"]["file"]["numLines"] = Value::from(6);
    let ctx = context_of(&run_hook(v.to_string().as_bytes(), cache.path()));
    assert_eq!(
        ctx,
        "sous-chef index context for this read of meta-core:recipes-core/hello/hello.inc:16-20:\n\
         The read starts at line 16 inside [do_install:append 15-18], which begins at line 15 \
         (meta-core:recipes-core/hello/hello.inc:15-18)."
    );
}

#[test]
fn reads_with_nothing_to_add_are_silent() {
    let cache = indexed_cache(&ws_root());
    // A whole include file: not a recipe, nothing cut.
    assert_noop(&run_hook(event("read_whole_inc").as_bytes(), cache.path()));
    // A non-Yocto file.
    let app_c = ws_root().join("meta-mine/recipes-app/app/files/app.c");
    let ev = event("read_cut").replace(
        &format!(
            "{}/meta-core/recipes-core/hello/hello.inc",
            ws_root().display()
        ),
        &app_c.display().to_string(),
    );
    assert_noop(&run_hook(ev.as_bytes(), cache.path()));
    // A read whose last line ends an item (the line count comes from `content`).
    let mut v: Value = serde_json::from_str(&event("read_cut")).unwrap();
    let inc =
        std::fs::read_to_string(ws_root().join("meta-core/recipes-core/hello/hello.inc")).unwrap();
    let first18: Vec<&str> = inc.lines().take(18).collect();
    v["tool_response"]["file"]["content"] = Value::String(first18.join("\n"));
    v["tool_response"]["file"]["numLines"] = Value::from(18);
    assert_noop(&run_hook(v.to_string().as_bytes(), cache.path()));
    // A non-text Read result.
    v["tool_response"] = serde_json::json!({"type": "image", "file": {"base64": "", "type": "image/png", "originalSize": 0}});
    assert_noop(&run_hook(v.to_string().as_bytes(), cache.path()));
}

#[test]
fn a_file_changed_since_indexing_is_skipped() {
    let (_tmp, root) = common::temp_ws();
    let cache = indexed_cache(&root);
    // Indexed and unchanged: annotated.
    let ev = event_in("read_cut", &root);
    context_of(&run_hook(ev.as_bytes(), cache.path()));
    // Edited after indexing: the hook does not refresh, so it says nothing.
    let inc = root.join("meta-core/recipes-core/hello/hello.inc");
    let text = std::fs::read_to_string(&inc).unwrap();
    std::fs::write(&inc, format!("# new first line\n{text}")).unwrap();
    assert_noop(&run_hook(ev.as_bytes(), cache.path()));
    let grep = event_in("grep_content", &root);
    let ctx = context_of(&run_hook(grep.as_bytes(), cache.path()));
    assert!(!ctx.contains("hello.inc"), "{ctx}");
    assert!(ctx.contains("app_0.1.bb:26,27 in [do_install 25-29]"));
}

#[test]
fn missing_index_is_logged() {
    let cache = tempfile::tempdir().unwrap();
    assert_noop(&run_hook(event("read_cut").as_bytes(), cache.path()));
    let log = log_text(cache.path(), Some(&ws_root()));
    assert!(log.contains("grep_read"), "{log}");
    assert!(log.contains("run `sous-chef index`"), "{log}");
}

#[test]
fn hook_call_is_fast() {
    let cache = indexed_cache(&ws_root());
    // Warm the page cache, then time a call (debug build, whole process).
    run_hook(event("grep_content").as_bytes(), cache.path());
    let t = std::time::Instant::now();
    let out = run_hook_with(event("grep_content").as_bytes(), cache.path(), "150");
    let took = t.elapsed();
    context_of(&out);
    assert!(took.as_millis() < 1000, "{took:?}");
}
