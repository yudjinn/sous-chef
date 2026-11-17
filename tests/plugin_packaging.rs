//! T7.5: the Claude Code plugin in `plugin/` stays consistent with the binary.
//!
//! - The manifest, the marketplace file, `hooks/hooks.json` and `.mcp.json` parse, and name
//!   the same plugin and version as `Cargo.toml`.
//! - Every command they run is a real subcommand of the built binary.
//! - Every hook matcher names only tools that have a handler in `hook::HANDLERS`, and every
//!   handled tool has a matcher.
//! - The hook commands fail open when `sous-chef` is not on `PATH`, and reach the handlers
//!   when it is; the MCP command starts the server in `$CLAUDE_PROJECT_DIR`.
//! - Events captured from a real Claude Code session give the expected context.
//! - Every tool name the skill mentions is in the tool registry.
//!
//! Nothing here needs Claude Code or the network: the commands are run with `sh -c`, the way
//! Claude Code runs a shell-form hook.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;
mod hook_support;

use std::collections::BTreeSet;
use std::ffi::OsString;
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::sync::mpsc::channel;
use std::time::Duration;

use hook_support::*;
use serde_json::{Value, json};
use sous_chef::hook::{HANDLERS, POST_TOOL_USE, handler_for};

const PLUGIN_NAME: &str = "sous-chef";
const SKILL: &str = "skills/yocto-layers/SKILL.md";

fn plugin_path(rel: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("plugin")
        .join(rel)
}

fn plugin_text(rel: &str) -> String {
    std::fs::read_to_string(plugin_path(rel)).unwrap_or_else(|e| panic!("plugin/{rel}: {e}"))
}

fn plugin_json(rel: &str) -> Value {
    serde_json::from_str(&plugin_text(rel))
        .unwrap_or_else(|e| panic!("plugin/{rel} is not JSON: {e}"))
}

fn bin() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_sous-chef"))
}

/// `PATH` holding the built binary (and the system directories for `sh`).
fn path_with_binary() -> OsString {
    let dir = bin().parent().unwrap().to_path_buf();
    std::env::join_paths([dir, "/usr/bin".into(), "/bin".into()]).unwrap()
}

/// The subcommand a plugin command runs: the word after `exec sous-chef `.
fn subcommand_of(command: &str) -> String {
    let rest = command
        .split("exec sous-chef ")
        .nth(1)
        .unwrap_or_else(|| panic!("command does not exec sous-chef: {command}"));
    rest.split_whitespace().next().unwrap().to_string()
}

/// `sous-chef <sub> --help` succeeds: clap rejects an unknown subcommand with status 2.
fn assert_real_subcommand(sub: &str) {
    let out = Command::new(bin()).args([sub, "--help"]).output().unwrap();
    assert!(
        out.status.success() && !out.stdout.is_empty(),
        "`sous-chef {sub}` is not a subcommand: {out:?}"
    );
}

/// The `PostToolUse` entries of `hooks/hooks.json`: (matcher, hook object).
fn hook_entries() -> Vec<(String, Value)> {
    let v = plugin_json("hooks/hooks.json");
    let events = v["hooks"]
        .as_object()
        .expect("a top-level \"hooks\" object");
    assert_eq!(
        events.keys().collect::<Vec<_>>(),
        ["PostToolUse"],
        "only PostToolUse hooks are handled"
    );
    let mut out = Vec::new();
    for group in events["PostToolUse"].as_array().unwrap() {
        let matcher = group["matcher"].as_str().unwrap().to_string();
        for h in group["hooks"].as_array().unwrap() {
            out.push((matcher.clone(), h.clone()));
        }
    }
    out
}

/// Runs `command` with `sh -c`, `stdin` piped in, the given `PATH` and cache root.
fn run_shell(command: &str, path: &OsString, stdin: &[u8], cache: &Path) -> Output {
    let mut child = Command::new("/bin/sh")
        .arg("-c")
        .arg(command)
        .current_dir(cache)
        .env("PATH", path)
        .env("XDG_CACHE_HOME", cache)
        .env("SOUS_CHEF_HOOK_BUDGET_MS", "5000")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child.stdin.take().unwrap().write_all(stdin).unwrap();
    child.wait_with_output().unwrap()
}

fn hook_command_for(tool: &str) -> String {
    let (_, h) = hook_entries()
        .into_iter()
        .find(|(m, _)| m.split('|').any(|t| t == tool))
        .unwrap_or_else(|| panic!("no hook matches {tool}"));
    h["command"].as_str().unwrap().to_string()
}

#[test]
fn manifest_and_marketplace_parse_and_agree() {
    let m = plugin_json(".claude-plugin/plugin.json");
    assert_eq!(m["name"], PLUGIN_NAME);
    assert_eq!(
        m["version"],
        env!("CARGO_PKG_VERSION"),
        "keep in step with Cargo.toml"
    );
    assert!(!m["description"].as_str().unwrap().is_empty());
    assert!(m["author"]["name"].is_string());
    // The default layout is used: no component paths in the manifest.
    for key in ["hooks", "mcpServers", "skills", "commands", "agents"] {
        assert!(
            m.get(key).is_none(),
            "{key} would change the default layout"
        );
    }
    for rel in ["hooks/hooks.json", ".mcp.json", SKILL] {
        assert!(plugin_path(rel).is_file(), "plugin/{rel} missing");
    }

    let mk = plugin_json(".claude-plugin/marketplace.json");
    assert_eq!(mk["name"], PLUGIN_NAME);
    assert!(mk["owner"]["name"].is_string());
    let plugins = mk["plugins"].as_array().unwrap();
    assert_eq!(plugins.len(), 1);
    assert_eq!(plugins[0]["name"], PLUGIN_NAME);
    assert_eq!(
        plugins[0]["source"], "./",
        "the marketplace root is the plugin"
    );
}

#[test]
fn hook_matchers_match_the_handler_registry() {
    let entries = hook_entries();
    let matchers: Vec<&str> = entries.iter().map(|(m, _)| m.as_str()).collect();
    assert_eq!(matchers, ["Grep|Read", "Bash", "Edit|Write|MultiEdit"]);

    let mut matched = BTreeSet::new();
    for m in &matchers {
        // Letters and `|` only: Claude Code then compares exact tool names, not a regex.
        assert!(
            m.chars().all(|c| c.is_ascii_alphanumeric() || c == '|'),
            "{m} would be read as a regular expression"
        );
        for tool in m.split('|') {
            assert!(
                handler_for(POST_TOOL_USE, tool).is_some(),
                "matcher {m}: no handler for {tool}"
            );
            matched.insert(tool.to_string());
        }
    }
    for h in HANDLERS {
        assert!(h.events().contains(&POST_TOOL_USE));
        for tool in h.tools() {
            assert!(
                matched.contains(*tool),
                "handler {} tool {tool} has no matcher",
                h.name()
            );
        }
    }
}

#[test]
fn hook_and_mcp_commands_name_real_subcommands() {
    for (m, h) in hook_entries() {
        assert_eq!(h["type"], "command", "{m}");
        assert!(
            h.get("args").is_none(),
            "{m}: shell form is what makes it fail open"
        );
        assert!(h["timeout"].as_u64().is_some_and(|t| t > 0), "{m}");
        let cmd = h["command"].as_str().unwrap();
        assert_eq!(subcommand_of(cmd), "hook", "{m}");
        assert_real_subcommand(&subcommand_of(cmd));
    }

    let mcp = plugin_json(".mcp.json");
    let servers = mcp["mcpServers"].as_object().unwrap();
    assert_eq!(servers.keys().collect::<Vec<_>>(), [PLUGIN_NAME]);
    let s = &servers[PLUGIN_NAME];
    assert_eq!(s["command"], "sh");
    let args = s["args"].as_array().unwrap();
    assert_eq!(args.len(), 2);
    assert_eq!(args[0], "-c");
    let script = args[1].as_str().unwrap();
    assert_eq!(subcommand_of(script), "serve");
    assert_real_subcommand("serve");
}

#[test]
fn hook_commands_fail_open_without_the_binary() {
    let empty = tempfile::tempdir().unwrap();
    let path = OsString::from(empty.path());
    for (m, h) in hook_entries() {
        let cmd = h["command"].as_str().unwrap();
        for name in ["grep_content", "bash_build_failure", "edit"] {
            let out = run_shell(cmd, &path, event(name).as_bytes(), empty.path());
            assert_eq!(out.status.code(), Some(0), "{m} {name}: {out:?}");
            assert!(out.stdout.is_empty(), "{m} {name}: {out:?}");
            assert!(out.stderr.is_empty(), "{m} {name}: {out:?}");
        }
    }
}

#[test]
fn hook_commands_reach_the_handlers() {
    let path = path_with_binary();
    let cache = indexed_cache(&ws_root());

    // Grep | Read: context added, output untouched.
    let cmd = hook_command_for("Grep");
    let out = run_shell(&cmd, &path, event("grep_content").as_bytes(), cache.path());
    assert!(context_of(&out).contains("[do_install:append 15-18]"));
    let cmd = hook_command_for("Read");
    let out = run_shell(&cmd, &path, event("read_cut").as_bytes(), cache.path());
    assert!(context_of(&out).contains("do_install:append"));

    // Bash: a failing bitbake build is replaced by its digest.
    let cmd = hook_command_for("Bash");
    let out = run_shell(
        &cmd,
        &path,
        event("bash_build_failure").as_bytes(),
        cache.path(),
    );
    assert_eq!(out.status.code(), Some(0), "{out:?}");
    let v: Value = serde_json::from_slice(&out.stdout).unwrap();
    let stdout = v["hookSpecificOutput"]["updatedToolOutput"]["stdout"]
        .as_str()
        .unwrap();
    assert!(stdout.contains("stored as h:"), "{stdout}");

    // Edit | Write | MultiEdit: the edited file is linted (on a copy of the workspace).
    let (_tmp, root) = common::temp_ws();
    let root = std::fs::canonicalize(root).unwrap();
    let cache = indexed_cache(&root);
    let rel = "meta-mine/recipes-core/hello/hello_1.1.bbappend";
    let abs = root.join(rel);
    let mut text = std::fs::read_to_string(&abs).unwrap();
    text.push_str("EXTRA_OECONF_append = \" --legacy\"\n");
    std::fs::write(&abs, &text).unwrap();
    for tool in ["Edit", "Write", "MultiEdit"] {
        let mut ev: Value = serde_json::from_str(&event_in("edit", &root)).unwrap();
        ev["tool_name"] = json!(tool);
        ev["tool_input"]["file_path"] = json!(abs.to_str().unwrap());
        ev["tool_response"]["filePath"] = json!(abs.to_str().unwrap());
        let cmd = hook_command_for(tool);
        let out = run_shell(&cmd, &path, ev.to_string().as_bytes(), cache.path());
        let ctx = context_of(&out);
        assert!(ctx.contains("override/legacy-syntax"), "{tool}: {ctx}");
    }
}

#[test]
fn captured_events_give_the_expected_context() {
    // Real events from Claude Code 2.1.326 (tests/fixtures/hooks/captured/README.md).
    let cache = indexed_cache(&ws_root());
    let cases = [
        ("captured/grep_content", "in [do_install:append 15-18]"),
        (
            "captured/read_offset",
            "inside [do_install:append 15-18], which ends at line 18",
        ),
        ("captured/read_whole_recipe", "these files also contribute"),
    ];
    for (name, want) in cases {
        let out = run_hook(event(name).as_bytes(), cache.path());
        let ctx = context_of(&out);
        assert!(ctx.contains(want), "{name}: {ctx}");
    }
}

#[test]
fn mcp_command_serves_the_project_dir() {
    let mcp = plugin_json(".mcp.json");
    let script = mcp["mcpServers"][PLUGIN_NAME]["args"][1]
        .as_str()
        .unwrap()
        .to_string();
    let cache = tempfile::tempdir().unwrap();
    let elsewhere = tempfile::tempdir().unwrap();
    // Started outside the project: only the `cd "$CLAUDE_PROJECT_DIR"` finds the workspace.
    let mut child = Command::new("/bin/sh")
        .arg("-c")
        .arg(&script)
        .current_dir(elsewhere.path())
        .env("PATH", path_with_binary())
        .env("CLAUDE_PROJECT_DIR", ws_root())
        .env("XDG_CACHE_HOME", cache.path())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let mut stdin = child.stdin.take().unwrap();
    let stdout = child.stdout.take().unwrap();
    let (tx, rx) = channel();
    std::thread::spawn(move || {
        for line in BufReader::new(stdout).lines() {
            let Ok(line) = line else { break };
            if tx.send(line).is_err() {
                break;
            }
        }
    });
    let mut request = |id: u64, method: &str, params: Value| -> Value {
        let msg = json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params});
        writeln!(stdin, "{msg}").unwrap();
        stdin.flush().unwrap();
        loop {
            let line = rx
                .recv_timeout(Duration::from_secs(60))
                .unwrap_or_else(|e| panic!("no response to {method}: {e}"));
            let v: Value = serde_json::from_str(&line).unwrap();
            if v["id"] == json!(id) {
                assert!(v.get("error").is_none(), "{method}: {v}");
                return v["result"].clone();
            }
        }
    };
    request(
        1,
        "initialize",
        json!({
            "protocolVersion": "2025-06-18",
            "capabilities": {},
            "clientInfo": {"name": "plugin-packaging-test", "version": "0"}
        }),
    );
    let list = request(2, "tools/list", json!({}));
    let names: Vec<&str> = list["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["name"].as_str().unwrap())
        .collect();
    let registry: Vec<&str> = sous_chef::mcp::TOOLS.iter().map(|t| t.name).collect();
    assert_eq!(names, registry);
    let status = request(3, "tools/call", json!({"name": "status", "arguments": {}}));
    let text = status["content"][0]["text"].as_str().unwrap();
    let root = ws_root();
    assert!(text.contains("state\tready"), "{text}");
    assert!(text.contains("config\tok"), "{text}");
    assert!(
        text.contains(&format!("work_dir\t{}\n", root.display())),
        "{text}"
    );
    drop(request);
    let _ = child.kill();
    let _ = child.wait();
}

/// Levenshtein distance, for spotting a misspelt tool name.
fn distance(a: &str, b: &str) -> usize {
    let b: Vec<char> = b.chars().collect();
    let mut prev: Vec<usize> = (0..=b.len()).collect();
    for (i, ca) in a.chars().enumerate() {
        let mut cur = vec![i + 1];
        for (j, cb) in b.iter().enumerate() {
            let sub = prev[j] + usize::from(ca != *cb);
            cur.push(sub.min(prev[j + 1] + 1).min(cur[j] + 1));
        }
        prev = cur;
    }
    prev[b.len()]
}

/// Whether `word` is probably a misspelling of `tool`: one edit away for short names (`var`,
/// `log`), two for longer ones.
fn near_miss(word: &str, tool: &str) -> bool {
    let limit = if tool.len() < 5 { 1 } else { 2 };
    word != tool && distance(word, tool) <= limit
}

/// The text of every `code span` in `text`.
fn code_spans(text: &str) -> Vec<String> {
    text.split('`')
        .skip(1)
        .step_by(2)
        .map(str::to_string)
        .collect()
}

#[test]
fn skill_frontmatter_is_valid() {
    let text = plugin_text(SKILL);
    let rest = text
        .strip_prefix("---\n")
        .expect("frontmatter on the first line");
    let (front, body) = rest.split_once("\n---\n").expect("closing ---");
    let mut name = None;
    let mut description = None;
    for line in front.lines() {
        if let Some(v) = line.strip_prefix("name: ") {
            name = Some(v.trim().to_string());
        } else if let Some(v) = line.strip_prefix("description: ") {
            description = Some(v.trim().to_string());
        } else {
            panic!("unexpected frontmatter line: {line}");
        }
    }
    assert_eq!(name.as_deref(), Some("yocto-layers"), "the directory name");
    let description = description.expect("description");
    // The listing truncates description + when_to_use at 1,536 characters.
    assert!(!description.is_empty() && description.chars().count() <= 1536);
    assert!(
        !description.contains(['<', '>']),
        "angle brackets are escaped in listings"
    );
    assert!(!body.trim().is_empty());
}

#[test]
fn skill_names_only_registered_tools() {
    let tools: BTreeSet<&str> = sous_chef::mcp::TOOLS.iter().map(|t| t.name).collect();
    let text = plugin_text(SKILL);

    // The "Which tool" table: the last column starts with the tool, and every tool has a row.
    let mut in_table = BTreeSet::new();
    for line in text
        .lines()
        .filter(|l| l.starts_with("| ") && l.contains('`'))
    {
        let last = line.trim_end_matches('|').rsplit('|').next().unwrap();
        let tool = code_spans(last).into_iter().next().unwrap();
        assert!(
            tools.contains(tool.as_str()),
            "table row names `{tool}`: {line}"
        );
        in_table.insert(tool);
    }
    let in_table: BTreeSet<&str> = in_table.iter().map(String::as_str).collect();
    assert_eq!(in_table, tools, "every tool has a row in the table");

    // Any other code span that looks like a tool name must be one, or a word the tool
    // reference uses (an argument or value such as `layer` or `loc`).
    let docs =
        std::fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("docs/mcp-tools.md"))
            .unwrap();
    let doc_words: BTreeSet<String> = code_spans(&docs)
        .iter()
        .flat_map(|s| {
            s.split(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))
                .map(str::to_string)
                .collect::<Vec<_>>()
        })
        .collect();
    for span in code_spans(&text) {
        let word = span.split([' ', ':', '=']).next().unwrap();
        if word.is_empty() || !word.chars().all(|c| c.is_ascii_lowercase() || c == '_') {
            continue;
        }
        if tools.contains(word) || doc_words.contains(word) {
            continue;
        }
        for tool in &tools {
            assert!(
                !near_miss(word, tool),
                "`{span}` looks like a misspelling of the tool `{tool}`"
            );
        }
    }
}

#[test]
fn near_miss_check_catches_misspellings() {
    assert!(near_miss("recipes", "recipe"));
    assert!(near_miss("serach", "search"));
    assert!(near_miss("overides", "overrides"));
    assert!(near_miss("vars", "var"));
    assert!(!near_miss("kas", "var"));
    assert!(!near_miss("var", "var"));
}
