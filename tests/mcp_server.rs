//! T2.5: spawn `sous-chef serve` on the fixture workspace and talk MCP over stdio.
//!
//! Asserts protocol-level facts only (handshake, the 18 tools, result shapes), so it passes
//! both while discovery and the index are stubs (the server runs degraded) and once they
//! are implemented.
//!
//! Also (w10) the server against a copy of the fixture workspace with a `sous-chef.toml`
//! of the test's own ([`configured`]): `[handles]` reaching the store, and progress
//! notifications, with the fake BitBake in `tests/fixtures/fakebb/`.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::io::{BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc::{Receiver, channel};
use std::thread;
use std::time::{Duration, Instant};

use serde_json::{Value, json};

const TIMEOUT: Duration = Duration::from_secs(60);

const TOOLS: [&str; 18] = [
    "recipe",
    "var",
    "overrides",
    "item",
    "search",
    "class",
    "layers",
    "deps",
    "lint",
    "scaffold",
    "bitbake",
    "log",
    "slice",
    "status",
    "audit",
    "diff",
    "sigs",
    "pkgdata",
];

struct Session {
    child: Child,
    stdin: Option<ChildStdin>,
    lines: Receiver<String>,
    /// Every line the server wrote to stdout, for the "only protocol on stdout" check.
    seen: Vec<String>,
    next_id: u64,
    _cache: tempfile::TempDir,
}

impl Session {
    fn start() -> Session {
        Session::start_in(tempfile::tempdir().unwrap())
    }

    /// As [`Session::start`], with `XDG_CACHE_HOME` = `cache`.
    fn start_in(cache: tempfile::TempDir) -> Session {
        let ws = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/ws");
        Session::spawn(
            cache,
            &ws,
            &["--build-dir", &ws.join("build").to_string_lossy()],
        )
    }

    /// `sous-chef serve <flags>` started in `ws` (whose `sous-chef.toml`, if any, is read),
    /// with `XDG_CACHE_HOME` = `cache`.
    fn spawn(cache: tempfile::TempDir, ws: &Path, flags: &[&str]) -> Session {
        let mut child = Command::new(env!("CARGO_BIN_EXE_sous-chef"))
            .arg("serve")
            .args(flags)
            .current_dir(ws)
            .env("XDG_CACHE_HOME", cache.path())
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
        // Drain stderr so the server never blocks on a full pipe.
        let mut stderr = child.stderr.take().unwrap();
        thread::spawn(move || {
            let mut sink = String::new();
            let _ = stderr.read_to_string(&mut sink);
        });
        let stdin = child.stdin.take();
        Session {
            child,
            stdin,
            lines: rx,
            seen: Vec::new(),
            next_id: 1,
            _cache: cache,
        }
    }

    fn send(&mut self, msg: &Value) {
        let stdin = self.stdin.as_mut().unwrap();
        writeln!(stdin, "{msg}").unwrap();
        stdin.flush().unwrap();
    }

    fn notify(&mut self, method: &str) {
        self.send(&json!({"jsonrpc": "2.0", "method": method}));
    }

    /// Sends a request and returns the whole response message with the same id.
    fn request(&mut self, method: &str, params: Value) -> Value {
        self.request_noting(method, params).0
    }

    /// As [`Session::request`], also returning every message without an id (the
    /// notifications) that arrived before the response, in order.
    fn request_noting(&mut self, method: &str, params: Value) -> (Value, Vec<Value>) {
        let id = self.next_id;
        self.next_id += 1;
        self.send(&json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params}));
        let deadline = Instant::now() + TIMEOUT;
        let mut notes = Vec::new();
        loop {
            let left = deadline.saturating_duration_since(Instant::now());
            let line = self
                .lines
                .recv_timeout(left)
                .unwrap_or_else(|e| panic!("no response to {method}: {e}"));
            self.seen.push(line.clone());
            let msg: Value = serde_json::from_str(&line)
                .unwrap_or_else(|e| panic!("non-JSON line on stdout ({e}): {line}"));
            if msg["id"] == json!(id) {
                return (msg, notes);
            }
            if msg.get("id").is_none() {
                notes.push(msg);
            }
        }
    }

    /// Calls a tool with `_meta` = `meta` (none: no `_meta`); returns the result and the
    /// `notifications/progress` messages that came before it.
    fn call_noting(&mut self, tool: &str, args: Value, meta: Option<Value>) -> (Value, Vec<Value>) {
        let mut params = json!({"name": tool, "arguments": args});
        if let Some(m) = meta {
            params["_meta"] = m;
        }
        let (msg, notes) = self.request_noting("tools/call", params);
        assert!(msg.get("error").is_none(), "{tool} failed: {msg}");
        let progress = notes
            .into_iter()
            .filter(|n| n["method"] == "notifications/progress")
            .collect();
        (msg["result"].clone(), progress)
    }

    /// The lines that arrive within `d` (nothing is sent).
    fn idle(&mut self, d: Duration) -> Vec<Value> {
        let deadline = Instant::now() + d;
        let mut out = Vec::new();
        while let Ok(line) = self
            .lines
            .recv_timeout(deadline.saturating_duration_since(Instant::now()))
        {
            self.seen.push(line.clone());
            out.push(serde_json::from_str(&line).unwrap());
        }
        out
    }

    fn result(&mut self, method: &str, params: Value) -> Value {
        let msg = self.request(method, params);
        assert!(msg.get("error").is_none(), "{method} failed: {msg}");
        msg["result"].clone()
    }

    fn call(&mut self, tool: &str, args: Value) -> Value {
        self.result("tools/call", json!({"name": tool, "arguments": args}))
    }

    /// Closes stdin, waits for the server to exit and returns the cache directory.
    fn finish_keep_cache(mut self) -> tempfile::TempDir {
        drop(self.stdin.take());
        let deadline = Instant::now() + TIMEOUT;
        while self.child.try_wait().unwrap().is_none() {
            if Instant::now() > deadline {
                let _ = self.child.kill();
                panic!("server did not exit after stdin closed");
            }
            thread::sleep(Duration::from_millis(20));
        }
        std::mem::replace(&mut self._cache, tempfile::tempdir().unwrap())
    }

    /// Closes stdin and waits for a clean exit.
    fn finish(mut self) {
        drop(self.stdin.take());
        let deadline = Instant::now() + TIMEOUT;
        let status = loop {
            if let Some(status) = self.child.try_wait().unwrap() {
                break status;
            }
            if Instant::now() > deadline {
                let _ = self.child.kill();
                panic!("server did not exit after stdin closed");
            }
            thread::sleep(Duration::from_millis(20));
        };
        assert!(status.success(), "server exited with {status}");
        while let Ok(line) = self.lines.recv_timeout(Duration::from_secs(5)) {
            self.seen.push(line);
        }
        for line in &self.seen {
            let msg: Value = serde_json::from_str(line).unwrap();
            assert_eq!(msg["jsonrpc"], "2.0", "not JSON-RPC: {line}");
        }
    }
}

fn text_of(result: &Value) -> &str {
    let content = result["content"].as_array().expect("content array");
    assert_eq!(content.len(), 1, "{result}");
    assert_eq!(content[0]["type"], "text", "{result}");
    content[0]["text"].as_str().unwrap()
}

#[test]
fn serve_initialize_list_and_status() {
    let mut s = Session::start();

    let init = s.result(
        "initialize",
        json!({
            "protocolVersion": "2025-06-18",
            "capabilities": {},
            "clientInfo": {"name": "sous-chef-test", "version": "0"}
        }),
    );
    assert_eq!(init["protocolVersion"], "2025-06-18");
    assert_eq!(init["serverInfo"]["name"], "sous-chef");
    assert!(init["capabilities"]["tools"].is_object(), "{init}");
    assert!(init["capabilities"]["resources"].is_object(), "{init}");
    let instructions = init["instructions"].as_str().unwrap();
    for word in ["recipe", "var", "item", "search", "Grep", "Read"] {
        assert!(instructions.contains(word), "instructions lack {word}");
    }
    s.notify("notifications/initialized");

    // tools/list: the 18 tools in registry order, six always loaded.
    let list = s.result("tools/list", json!({}));
    let tools = list["tools"].as_array().unwrap();
    let names: Vec<&str> = tools.iter().map(|t| t["name"].as_str().unwrap()).collect();
    assert_eq!(names, TOOLS);
    for (i, t) in tools.iter().enumerate() {
        assert_eq!(t["inputSchema"]["type"], "object", "{t}");
        let desc = t["description"].as_str().unwrap();
        assert!(!desc.is_empty());
        // Claude Code loads these six upfront under tool search; the rest stay deferred.
        if i < 6 {
            assert_eq!(t["_meta"]["anthropic/alwaysLoad"], json!(true), "{t}");
        } else {
            assert!(t.get("_meta").is_none(), "{t}");
        }
    }

    // status, text: a successful tool result with one text block.
    let status = s.call("status", json!({}));
    assert_eq!(status["isError"], json!(false), "{status}");
    text_of(&status);

    // status, json: the documented fields.
    let status = s.call("status", json!({"format": "json"}));
    assert_eq!(status["isError"], json!(false), "{status}");
    let report: Value = serde_json::from_str(text_of(&status)).unwrap();
    assert_eq!(report["version"], env!("CARGO_PKG_VERSION"));
    let state = report["state"].as_str().unwrap();
    assert!(state == "ready" || state == "degraded", "{report}");
    assert!(report["layers"].is_array(), "{report}");
    assert!(report["files"].is_object(), "{report}");
    assert!(report["index"].is_object(), "{report}");
    assert!(report["bitbake"].is_object(), "{report}");
    assert!(report.get("job").is_some(), "{report}");
    assert!(report.get("build_dir").is_some(), "{report}");
    if state == "ready" {
        assert_eq!(
            report["release"], "wrynose (LAYERSERIES_CORENAMES)",
            "{report}"
        );
    }

    // A failing tool is a tool error with a one-line message, not a protocol error.
    let bad = s.call("recipe", json!({}));
    assert_eq!(bad["isError"], json!(true), "{bad}");
    let msg = text_of(&bad);
    assert!(msg.starts_with("recipe: "), "{msg}");
    assert!(!msg.contains('\n'), "{msg}");

    // An unknown tool is a protocol error.
    let unknown = s.request("tools/call", json!({"name": "nope", "arguments": {}}));
    assert!(unknown["error"].is_object(), "{unknown}");

    // Resources.
    let resources = s.result("resources/list", json!({}));
    let uris: Vec<&str> = resources["resources"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r["uri"].as_str().unwrap())
        .collect();
    assert_eq!(uris, ["sous-chef://help", "sous-chef://rules"]);
    let help = s.result("resources/read", json!({"uri": "sous-chef://help"}));
    let help = help["contents"][0]["text"].as_str().unwrap();
    for t in TOOLS {
        assert!(help.contains(&format!("## {t} (")), "help lacks {t}");
    }
    let rules = s.result("resources/read", json!({"uri": "sous-chef://rules"}));
    let rules = rules["contents"][0]["text"].as_str().unwrap();
    assert!(rules.starts_with("id\tseverity\tscope\tflags\n"));
    assert!(rules.contains("override/legacy-syntax\terror\tfile\t"));
    let missing = s.request("resources/read", json!({"uri": "sous-chef://nope"}));
    assert!(missing["error"].is_object(), "{missing}");

    s.finish();
}

fn initialize(s: &mut Session) {
    s.result(
        "initialize",
        json!({
            "protocolVersion": "2025-06-18",
            "capabilities": {},
            "clientInfo": {"name": "sous-chef-test", "version": "0"}
        }),
    );
}

#[test]
fn server_start_removes_output_files_of_a_dead_server() {
    // The first server creates the workspace's cache directory.
    let mut s = Session::start();
    initialize(&mut s);
    let cache = s.finish_keep_cache();
    let dirs: Vec<_> = std::fs::read_dir(cache.path().join("sous-chef"))
        .unwrap()
        .map(|e| e.unwrap().path())
        .collect();
    assert_eq!(dirs.len(), 1, "{dirs:?}");
    let jobs = dirs[0].join("jobs");
    std::fs::create_dir_all(&jobs).unwrap();
    // A job output file of a pid that is not alive, last written two days ago.
    let mut dead = 4_000_000u32;
    while Path::new("/proc").join(dead.to_string()).exists() {
        dead += 1;
    }
    let stale = jobs.join(format!("{dead}-1.out"));
    std::fs::File::create(&stale)
        .unwrap()
        .set_modified(std::time::SystemTime::now() - Duration::from_secs(2 * 24 * 60 * 60))
        .unwrap();

    // The next server removes it without any job being started.
    let mut s = Session::start_in(cache);
    initialize(&mut s);
    let deadline = Instant::now() + Duration::from_secs(20);
    while stale.exists() && Instant::now() < deadline {
        thread::sleep(Duration::from_millis(20));
    }
    assert!(!stale.exists(), "{} is still there", stale.display());
    s.finish();
}

/// A server on a writable copy of the fixture workspace whose `sous-chef.toml` is
/// `build_dir = "build"`, `writable_layers = ["meta-mine"]` and then `extra`, started
/// without flags so the file is read. Keep the returned `TempDir` alive while the session
/// runs; the `PathBuf` is the copy's root.
fn configured(extra: &str) -> (Session, tempfile::TempDir, PathBuf) {
    let (tmp, root) = common::temp_ws();
    let root = std::fs::canonicalize(root).unwrap();
    std::fs::write(
        root.join("sous-chef.toml"),
        format!("build_dir = \"build\"\nwritable_layers = [\"meta-mine\"]\n{extra}\n"),
    )
    .unwrap();
    let mut s = Session::spawn(tempfile::tempdir().unwrap(), &root, &[]);
    initialize(&mut s);
    s.notify("notifications/initialized");
    (s, tmp, root)
}

#[test]
fn handles_table_reaches_the_store_and_status_shows_it() {
    let (mut s, _tmp, _root) = configured("[handles]\nmax_total_mb = 7\nmax_age_days = 2\n");
    let status = s.call("status", json!({"format": "json"}));
    let report: Value = serde_json::from_str(text_of(&status)).unwrap();
    assert_eq!(report["state"], "ready", "{report}");
    let h = &report["handles"];
    assert_eq!(h["max_total_bytes"], 7_000_000, "{report}");
    assert_eq!(h["max_age_days"], 2, "{report}");
    assert!(h["count"].is_u64() && h["bytes"].is_u64(), "{report}");
    let text = s.call("status", json!({}));
    let row = text_of(&text)
        .lines()
        .find(|l| l.starts_with("handles\t"))
        .unwrap_or_else(|| panic!("no handles row: {}", text_of(&text)))
        .to_string();
    assert!(row.contains("limits 7.0 MB, 2 days"), "{row}");
    s.finish();
}

#[test]
fn a_bad_handles_value_serves_degraded_with_the_reason() {
    let (mut s, _tmp, _root) = configured("[handles]\nmax_age_days = 0\n");
    let status = s.call("status", json!({"format": "json"}));
    let report: Value = serde_json::from_str(text_of(&status)).unwrap();
    assert_eq!(report["state"], "degraded", "{report}");
    let err = report["startup_error"].as_str().unwrap();
    assert!(
        err.contains("handles.max_age_days = 0 is out of range"),
        "{err}"
    );
    s.finish();
}

// ---- progress notifications (w10) -------------------------------------------------------

/// A server whose bitbake commands run the fake BitBake (`tests/fixtures/fakebb/`), which
/// reads its settings from `<build>/fakebb.env`.
fn with_fake_bitbake() -> (Session, tempfile::TempDir, PathBuf) {
    let init = common::fixture_path("fakebb/init-with-env");
    configured(&format!(
        "init_script = \"{}\"\n[bitbake]\nenabled = true\n",
        init.display()
    ))
}

fn fake(root: &Path, vars: &[(&str, &str)]) {
    let text: String = vars
        .iter()
        .map(|(k, v)| format!("export {k}='{v}'\n"))
        .collect();
    std::fs::write(root.join("build/fakebb.env"), text).unwrap();
}

/// The progress values of `notes`, after checking that every one carries `token`, has a
/// message, and that the values strictly increase.
fn progress_values(notes: &[Value], token: &Value) -> Vec<f64> {
    let mut values = Vec::new();
    for n in notes {
        let p = &n["params"];
        assert_eq!(&p["progressToken"], token, "{n}");
        assert!(p["message"].as_str().is_some_and(|m| !m.is_empty()), "{n}");
        values.push(p["progress"].as_f64().unwrap());
    }
    assert!(
        values.windows(2).all(|w| w[1] > w[0]),
        "progress does not increase: {values:?}"
    );
    values
}

#[test]
fn a_waiting_bitbake_call_with_a_token_gets_progress_before_its_result() {
    let (mut s, _tmp, root) = with_fake_bitbake();
    fake(&root, &[("FAKEBB_SLEEP", "2")]);
    let token = json!("bb-1");
    let asked = Instant::now();
    let (result, notes) = s.call_noting(
        "bitbake",
        json!({"op": "layers", "wait": 30}),
        Some(json!({"progressToken": token})),
    );
    let took = asked.elapsed();
    let text = text_of(&result);
    assert!(text.contains("state\tsucceeded"), "{text}");
    let values = progress_values(&notes, &token);
    assert!(values.len() >= 2, "{notes:?}");
    // Rate-limited: at most four a second, plus the last one.
    assert!(
        (values.len() as f64) <= took.as_secs_f64() * 4.0 + 2.0,
        "{} notifications in {took:?}",
        values.len()
    );
    let first = notes[0]["params"]["message"].as_str().unwrap();
    assert!(first.starts_with("job:1 layers running"), "{first}");
    // The fake prints its argv first: the tail line is in the message.
    assert!(
        notes.iter().any(|n| n["params"]["message"]
            .as_str()
            .unwrap()
            .contains("arg: show-layers")),
        "{notes:?}"
    );
    // Nothing after the result.
    let late = s.idle(Duration::from_millis(600));
    assert!(late.is_empty(), "after the result: {late:?}");

    // An integer token works the same.
    fake(&root, &[("FAKEBB_SLEEP", "1")]);
    let (_, notes) = s.call_noting(
        "bitbake",
        json!({"op": "env", "wait": 30}),
        Some(json!({"progressToken": 7})),
    );
    assert!(!progress_values(&notes, &json!(7)).is_empty());
    s.finish();
}

#[test]
fn without_a_token_no_progress_is_sent() {
    let (mut s, _tmp, root) = with_fake_bitbake();
    fake(&root, &[("FAKEBB_SLEEP", "1")]);
    let (result, notes) = s.call_noting("bitbake", json!({"op": "layers", "wait": 30}), None);
    assert!(text_of(&result).contains("state\tsucceeded"), "{result}");
    assert!(notes.is_empty(), "{notes:?}");
    // `_meta` without a progress token: none either.
    fake(&root, &[("FAKEBB_SLEEP", "1")]);
    let (_, notes) = s.call_noting(
        "bitbake",
        json!({"op": "layers", "wait": 30}),
        Some(json!({"other": 1})),
    );
    assert!(notes.is_empty(), "{notes:?}");
    let (_, notes) = s.call_noting("audit", json!({}), None);
    assert!(notes.is_empty(), "{notes:?}");
    assert!(s.idle(Duration::from_millis(300)).is_empty());
    s.finish();
}

/// (w11) `lint` reports its files as progress `done` of `total` while it runs.
#[test]
fn lint_with_a_token_reports_files_with_a_total() {
    let mut s = Session::start();
    initialize(&mut s);
    let token = json!("lint-1");
    let (result, notes) = s.call_noting(
        "lint",
        json!({"target": "meta-core"}),
        Some(json!({"progressToken": token})),
    );
    assert_eq!(result["isError"], json!(false), "{result}");
    let values = progress_values(&notes, &token);
    // The first file at once, the last one flushed before the result.
    assert!(values.len() >= 2, "{notes:?}");
    let totals: Vec<f64> = notes
        .iter()
        .map(|n| n["params"]["total"].as_f64().expect("a total"))
        .collect();
    let total = totals[0];
    assert!(total >= 2.0, "{notes:?}");
    assert!(totals.iter().all(|t| *t == total), "{notes:?}");
    assert_eq!(*values.last().unwrap(), total, "{notes:?}");
    for n in &notes {
        let m = n["params"]["message"].as_str().unwrap();
        assert!(m.starts_with("lint: ") && m.ends_with(" files"), "{m}");
    }
    assert!(s.idle(Duration::from_millis(400)).is_empty());

    // Without a token: none.
    let (result, notes) = s.call_noting("lint", json!({"target": "meta-core"}), None);
    assert_eq!(result["isError"], json!(false), "{result}");
    assert!(notes.is_empty(), "{notes:?}");
    s.finish();
}

#[test]
fn authoritative_var_and_audit_send_progress_with_a_token() {
    let (mut s, _tmp, root) = with_fake_bitbake();
    // The fake prints no dump, so the answer is an error; the wait still reports progress.
    fake(&root, &[("FAKEBB_SLEEP", "1")]);
    let token = json!("var-1");
    let (result, notes) = s.call_noting(
        "var",
        json!({"name": "PN", "recipe": "hello", "authoritative": true}),
        Some(json!({"progressToken": token})),
    );
    assert_eq!(result["isError"], json!(true), "{result}");
    assert!(text_of(&result).contains("no environment dump"), "{result}");
    assert!(!progress_values(&notes, &token).is_empty(), "{notes:?}");
    assert!(
        notes[0]["params"]["message"]
            .as_str()
            .unwrap()
            .starts_with("job:1 env hello running"),
        "{notes:?}"
    );

    let token = json!("audit-1");
    let (result, notes) = s.call_noting("audit", json!({}), Some(json!({"progressToken": token})));
    assert_eq!(result["isError"], json!(false), "{result}");
    let values = progress_values(&notes, &token);
    // The first stage at once; the last stage flushed before the result.
    assert!(values.len() >= 2, "{notes:?}");
    for n in &notes {
        let m = n["params"]["message"].as_str().unwrap();
        assert!(m.starts_with("audit: "), "{m}");
    }
    assert_eq!(
        notes[0]["params"]["message"], "audit: refreshing the index",
        "{notes:?}"
    );
    let last = notes.last().unwrap()["params"]["message"].as_str().unwrap();
    assert!(last.contains("parse and resolution problems"), "{last}");
    s.finish();
}
