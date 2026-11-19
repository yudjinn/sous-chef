//! T7.3: the Bash output digest. Large bitbake output is stored under a handle the MCP
//! server's `slice` can read, and the tool output is replaced by the digest
//! (`updatedToolOutput`); small output and other commands pass through untouched.
//!
//! The recorded events `bash_build_failure.json` and `bash_other_command.json` were written
//! by hand in the documented `PostToolUse` shape (see `plugin/README.md`), like the other
//! recorded events; they were not captured from a live session.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;
mod hook_support;

use std::path::Path;
use std::time::Instant;

use hook_support::*;
use serde_json::Value;
use sous_chef::bitbake::JobTable;
use sous_chef::handles::{Handle, HandleStore};
use sous_chef::index::Index;
use sous_chef::mcp::Ctx;
use sous_chef::mcp::tools::slice::{self, Args};

/// The `updatedToolOutput` of a hook run (asserting the response shape).
fn replaced(out: &std::process::Output) -> Value {
    assert_eq!(out.status.code(), Some(0), "{out:?}");
    let text = String::from_utf8(out.stdout.clone()).unwrap();
    let v: Value = serde_json::from_str(&text)
        .unwrap_or_else(|e| panic!("stdout is not one JSON object ({e}): {text:?}"));
    let spec = v["hookSpecificOutput"].as_object().unwrap();
    assert_eq!(spec["hookEventName"], "PostToolUse");
    assert!(!spec.contains_key("additionalContext"), "{text}");
    assert_eq!(v.as_object().unwrap().len(), 1);
    spec["updatedToolOutput"].clone()
}

/// The handle named in a replacement.
fn handle_of(stdout: &str) -> Handle {
    let at = stdout.find("stored as h:").unwrap() + "stored as ".len();
    stdout[at..at + 12].parse().unwrap()
}

/// The MCP server's view of the workspace at `root` with the cache root `cache`, as
/// `mcp::Ctx::open` builds it.
fn server(root: &Path, cache: &Path) -> Ctx {
    let config = sous_chef::config::Config::load(&sous_chef::config::CliOverrides {
        build_dir: None,
        config: None,
        layers: Vec::new(),
        work_dir: root.to_path_buf(),
    })
    .unwrap();
    let workspace = discover(root);
    let index = Index::open_in(cache, &workspace).unwrap();
    let handles = HandleStore::open(&index.cache_dir().join("handles")).unwrap();
    Ctx {
        config,
        workspace,
        index: tokio::sync::Mutex::new(index),
        handles,
        jobs: JobTable::new(),
    }
}

fn slice_args(h: &Handle) -> Args {
    Args {
        handle: h.0.clone(),
        lines: None,
        grep: None,
        context: None,
        var: None,
        failures: None,
        page: None,
        budget_tokens: None,
        format: None,
    }
}

#[tokio::test]
async fn a_failing_build_is_replaced_by_its_digest_and_the_handle_slices() {
    let cache = indexed_cache(&ws_root());
    let ev = event("bash_build_failure");
    let original: Value = serde_json::from_str(&ev).unwrap();
    let original_stdout = original["tool_response"]["stdout"].as_str().unwrap();
    assert!(original_stdout.len() > 3 * 1024);

    let out = run_hook(ev.as_bytes(), cache.path());
    let r = replaced(&out);
    // The Bash output shape, other fields as they were.
    assert_eq!(r["stderr"], "");
    assert_eq!(r["interrupted"], false);
    assert_eq!(r["isImage"], false);
    assert_eq!(r.as_object().unwrap().len(), 4, "{r}");
    let stdout = r["stdout"].as_str().unwrap();
    assert!(stdout.len() < 2000, "{stdout}");
    assert!(
        stdout.starts_with(&format!(
            "sous-chef: bitbake output (115 lines, {} bytes) stored as h:",
            original_stdout.len()
        )),
        "{stdout}"
    );
    for want in [
        "recipe\thello\n",
        "task\tdo_compile\n",
        "exit\t1\n",
        "oe_runmake failed",
        // The task's definition, from the index: the class default hello inherits.
        "definition\tmeta-core:classes-recipe/autotoolsish.bbclass:22-24\n",
    ] {
        assert!(stdout.contains(want), "{want:?} not in:\n{stdout}");
    }
    let h = handle_of(stdout);
    assert!(
        stdout.trim_end().ends_with(&format!(
            "slice handle={h} failures=true | grep=RE | lines=105-107"
        )),
        "{stdout}"
    );

    // The server's slice reads the handle: the full output, byte for byte.
    let ctx = server(&ws_root(), cache.path());
    assert_eq!(ctx.handles.get(&h).unwrap(), original_stdout.as_bytes());
    let failures = slice::run(
        &ctx,
        Args {
            failures: Some(true),
            ..slice_args(&h)
        },
    )
    .await
    .unwrap();
    assert!(failures.contains("oe_runmake failed"), "{failures}");
    let first = slice::run(
        &ctx,
        Args {
            lines: Some("1-1".into()),
            ..slice_args(&h)
        },
    )
    .await
    .unwrap();
    assert!(first.contains("Loading cache...done."), "{first}");
}

#[test]
fn large_output_without_a_failure_gets_its_tail_and_a_handle() {
    let cache = indexed_cache(&ws_root());
    let mut ev: Value = serde_json::from_str(&event("bash")).unwrap();
    let listing: String = (0..120)
        .map(|i| format!("recipe-{i:03}                                   1.0-r0\n"))
        .collect();
    ev["tool_input"]["command"] = Value::String("bitbake-layers show-recipes".into());
    ev["tool_response"]["stdout"] = Value::String(listing);
    ev["tool_response"]["stderr"] = Value::String("WARNING: something on stderr\n".into());
    let r = replaced(&run_hook(ev.to_string().as_bytes(), cache.path()));
    let stdout = r["stdout"].as_str().unwrap();
    assert!(
        stdout.contains("bytes, stdout then stderr) stored as h:"),
        "{stdout}"
    );
    assert!(
        stdout.contains("no failure recognised; last 10 lines:\n"),
        "{stdout}"
    );
    assert!(stdout.contains("recipe-119"), "{stdout}");
    assert!(stdout.contains("WARNING: something on stderr"), "{stdout}");
    assert!(!stdout.contains("recipe-100 "), "{stdout}");
    let h = handle_of(stdout);
    assert!(
        stdout
            .trim_end()
            .ends_with(&format!("slice handle={h} grep=RE | lines=72-121")),
        "{stdout}"
    );
    let ctx = server(&ws_root(), cache.path());
    let full = String::from_utf8(ctx.handles.get(&h).unwrap()).unwrap();
    assert!(full.starts_with("recipe-000 "));
    assert!(full.ends_with(
        "recipe-119                                   1.0-r0\nWARNING: something on stderr\n"
    ));
}

#[test]
fn small_output_and_other_commands_are_untouched() {
    let cache = indexed_cache(&ws_root());
    // bitbake-layers, a few hundred bytes.
    assert_noop(&run_hook(event("bash").as_bytes(), cache.path()));
    // The same large failing output, from `cat`.
    assert_noop(&run_hook(
        event("bash_other_command").as_bytes(),
        cache.path(),
    ));
    // Nothing was stored.
    let ctx = server(&ws_root(), cache.path());
    assert!(ctx.handles.list().unwrap().is_empty());
}

#[test]
fn no_index_or_a_locked_database_fails_open() {
    let empty = tempfile::tempdir().unwrap();
    assert_noop(&run_hook(
        event("bash_build_failure").as_bytes(),
        empty.path(),
    ));
    let cache = indexed_cache(&ws_root());
    let db = sous_chef::index::cache_dir_in(cache.path(), &discover(&ws_root()))
        .join(sous_chef::index::INDEX_FILE);
    let lock = rusqlite::Connection::open(&db).unwrap();
    lock.execute_batch("BEGIN IMMEDIATE").unwrap();
    let out = run_hook(event("bash_build_failure").as_bytes(), cache.path());
    lock.execute_batch("ROLLBACK").unwrap();
    assert_noop(&out);
    assert!(log_text(cache.path(), Some(&ws_root())).contains("bash"));
}

#[test]
#[ignore = "timing; run with --ignored --nocapture"]
fn timing() {
    let cache = indexed_cache(&ws_root());
    let ev = event("bash_build_failure");
    let n = 20;
    let start = Instant::now();
    for _ in 0..n {
        replaced(&run_hook(ev.as_bytes(), cache.path()));
    }
    println!(
        "bash hook, failing build: {:.1} ms per call",
        start.elapsed().as_secs_f64() * 1000.0 / f64::from(n)
    );
}

// ---------------------------------------------------------------------------------------
// y3-leftovers: the hook on recorded real consoles of curl (docs/live-verification.md).

/// A recorded real output (`tests/fixtures/outputs/live/<name>`) with `{{LIVE}}/yocto-kiss`
/// replaced by `root` (so its build directory is the workspace's `build/`) and the given
/// further replacements.
fn live_output(name: &str, root: &Path, more: &[(&str, &str)]) -> String {
    let p = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/outputs/live")
        .join(name);
    let mut text = std::fs::read_to_string(&p)
        .unwrap()
        .replace("{{LIVE}}/yocto-kiss", root.to_str().unwrap());
    for (from, to) in more {
        text = text.replace(from, to);
    }
    text
}

/// A Bash event in the workspace at `root`: `command` with `stdout`.
fn bash_event(root: &Path, command: &str, stdout: &str) -> String {
    let mut ev: Value = serde_json::from_str(&event_in("bash", root)).unwrap();
    ev["tool_input"]["command"] = Value::String(command.into());
    ev["tool_response"]["stdout"] = Value::String(stdout.into());
    ev.to_string()
}

const KAS_COMMAND: &str =
    "env -u NETRC_FILE ../venv/bin/kas shell ../scratch/kas-scratch.yaml -c 'bitbake curl'";

/// The real `configure: error` console: the hook reads the run file the trap names (next
/// to the task log the console names) and maps the `oe_runconf` frame through its `# line:`
/// comment, as the `log` tool does: run-file line 255 is `autotools.bbclass:96`.
#[test]
fn the_hook_maps_a_real_failing_frame_to_its_metadata_line() {
    let (_tmp, root) = common::temp_ws();
    let root = std::fs::canonicalize(root).unwrap();
    let t = root.join("build/tmp/work/armv7at2hf-neon-oe-linux-gnueabi/curl/8.19.0/temp");
    std::fs::create_dir_all(&t).unwrap();
    std::fs::write(
        t.join("run.do_configure.1749535"),
        live_output("y1-build/log/run.do_configure.1749535", &root, &[]),
    )
    .unwrap();
    let console = live_output("y1-build/log/console-configure-fail.out", &root, &[]);
    let cache = indexed_cache(&root);
    let r = replaced(&run_hook(
        bash_event(&root, KAS_COMMAND, &console).as_bytes(),
        cache.path(),
    ));
    let stdout = r["stdout"].as_str().unwrap();
    let function = stdout
        .lines()
        .find_map(|l| l.strip_prefix("function\t"))
        .unwrap_or_else(|| panic!("{stdout}"));
    // The metadata file is outside this workspace, so the row gives its path (cut to the
    // snippet width); on the live workspace it is `meta:classes-recipe/autotools.bbclass:96`.
    let expected = format!(
        "oe_runconf at {}/build/../openembedded-core/meta/classes-recipe/autotools.bbclass:96; called from",
        root.display()
    );
    let keep = sous_chef::render::SNIPPET_MAX_CHARS - 3;
    if expected.chars().count() <= keep {
        assert!(function.starts_with(&expected), "{stdout}");
    } else {
        // A long temporary directory (TMPDIR) pushes `:96` past the cut: the row is then
        // exactly the cut prefix, and the line number cannot be seen in this output.
        let cut: String = expected.chars().take(keep).collect();
        assert_eq!(function, format!("{cut}..."), "{stdout}");
    }
    assert!(!stdout.contains("run file line"), "{stdout}");
}

/// The real failing `do_compile:prepend` console, laid out as hello 1.1's with the prepend
/// in hello's bbappend: the frame maps to `autogenerated` (the EXPORT_FUNCTIONS stub), and
/// the hook points at the bbappend line holding the failing command, as the `log` tool does
/// (`tests/bitbake_logs.rs`, `log_tool_points_a_failing_prepend_at_its_operation`).
#[test]
fn the_hook_points_a_real_failing_prepend_at_its_operation() {
    let (_tmp, root) = common::temp_ws();
    let root = std::fs::canonicalize(root).unwrap();
    let as_hello: &[(&str, &str)] = &[
        ("/curl/8.19.0/temp/", "/hello/1.1/temp/"),
        (
            "recipes-support/curl/curl_8.19.0.bb",
            "recipes-core/hello/hello_1.1.bb",
        ),
        ("curl-8.19.0-r0", "hello-1.1-r0"),
    ];
    let t = root.join("build/tmp/work/armv7at2hf-neon-oe-linux-gnueabi/hello/1.1/temp");
    std::fs::create_dir_all(&t).unwrap();
    std::fs::write(
        t.join("run.do_compile.1757933"),
        live_output("y1-build/log/run.do_compile.1757933", &root, as_hello),
    )
    .unwrap();
    let append = root.join("meta-mine/recipes-core/hello/hello_1.1.bbappend");
    let mut text = std::fs::read_to_string(&append).unwrap();
    text.push_str("\ndo_compile:prepend() {\n\techo \"lib/sous_chef_probe.c:12:5: error: 'probe' undeclared\"\n\tfalse\n}\n");
    let line = text.lines().position(|l| l == "\tfalse").unwrap() + 1;
    std::fs::write(&append, text).unwrap();
    let console = live_output("y1-build/log/console-compile-fail.out", &root, as_hello);
    let cache = indexed_cache(&root);
    let r = replaced(&run_hook(
        bash_event(&root, KAS_COMMAND, &console).as_bytes(),
        cache.path(),
    ));
    let stdout = r["stdout"].as_str().unwrap();
    assert!(
        stdout.contains(&format!(
            "function\tdo_compile at meta-mine:recipes-core/hello/hello_1.1.bbappend:{line} (run inside the EXPORT_FUNCTIONS stub)\n"
        )),
        "{stdout}"
    );
    // Without the run file the row gives the run-file line, as before.
    std::fs::remove_file(t.join("run.do_compile.1757933")).unwrap();
    let r = replaced(&run_hook(
        bash_event(&root, KAS_COMMAND, &console).as_bytes(),
        cache.path(),
    ));
    let stdout = r["stdout"].as_str().unwrap();
    assert!(
        stdout.contains("function\tdo_compile (run file line 148)\n"),
        "{stdout}"
    );
}

/// The real first build's console (trimmed): no failure, three warnings. The replacement
/// lists the warnings, and its tail leaves out knotty's count of them.
#[test]
fn the_hook_lists_the_warnings_of_a_real_successful_build() {
    let (_tmp, root) = common::temp_ws();
    let root = std::fs::canonicalize(root).unwrap();
    let console = live_output("y3-leftovers/build1-first-build-trimmed.out", &root, &[]);
    assert!(console.len() > 3 * 1024);
    let cache = indexed_cache(&root);
    let r = replaced(&run_hook(
        bash_event(
            &root,
            "env -u NETRC_FILE ../venv/bin/kas shell kas/kiss.yaml:kas/dogbonedark.yaml -c 'bitbake curl'",
            &console,
        )
        .as_bytes(),
        cache.path(),
    ));
    let stdout = r["stdout"].as_str().unwrap();
    let at = stdout
        .find("3 warnings:\n")
        .unwrap_or_else(|| panic!("{stdout}"));
    let listed: Vec<&str> = stdout[at..].lines().skip(1).take(3).collect();
    assert!(
        listed[0].starts_with("WARNING: Your host glibc version (2.44)"),
        "{stdout}"
    );
    assert!(
        listed[1].starts_with("WARNING: ca-certificates-20260816-r0 do_fetch"),
        "{stdout}"
    );
    assert!(
        listed[2].starts_with("WARNING: gnu-config-native-20250709+git-r0 do_fetch"),
        "{stdout}"
    );
    assert!(
        !stdout.contains("Summary: There were 3 WARNING messages."),
        "{stdout}"
    );
    assert!(
        stdout.contains("no failure recognised; last 10 lines:\n"),
        "{stdout}"
    );
    assert!(stdout.len() < 2048, "{}", stdout.len());
}
