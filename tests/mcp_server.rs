//! T2.5: spawn `sous-chef serve` on the fixture workspace and talk MCP over stdio.
//!
//! Asserts protocol-level facts only (handshake, the 16 tools, result shapes), so it passes
//! both while discovery and the index are stubs (the server runs degraded) and once they
//! are implemented.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::io::{BufRead, BufReader, Read, Write};
use std::path::Path;
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc::{Receiver, channel};
use std::thread;
use std::time::{Duration, Instant};

use serde_json::{Value, json};

const TIMEOUT: Duration = Duration::from_secs(60);

const TOOLS: [&str; 16] = [
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
        let mut child = Command::new(env!("CARGO_BIN_EXE_sous-chef"))
            .arg("serve")
            .arg("--build-dir")
            .arg(ws.join("build"))
            .current_dir(&ws)
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
        let id = self.next_id;
        self.next_id += 1;
        self.send(&json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params}));
        let deadline = Instant::now() + TIMEOUT;
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
                return msg;
            }
        }
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

    // tools/list: the 16 tools in registry order, six always loaded.
    let list = s.result("tools/list", json!({}));
    let tools = list["tools"].as_array().unwrap();
    let names: Vec<&str> = tools.iter().map(|t| t["name"].as_str().unwrap()).collect();
    assert_eq!(names, TOOLS);
    for (i, t) in tools.iter().enumerate() {
        assert_eq!(t["inputSchema"]["type"], "object", "{t}");
        let desc = t["description"].as_str().unwrap();
        assert!(!desc.is_empty());
        let loading = if i < 6 { "always" } else { "deferred" };
        assert_eq!(t["_meta"]["sous-chef/loading"], loading, "{t}");
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
