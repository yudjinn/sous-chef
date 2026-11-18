//! T8.2: scale and performance on the live workspace (docs/performance.md).
//!
//! Ignored by default. Run it with a release build and the live environment
//! (docs/live-environment.md):
//!
//! ```sh
//! SOUS_CHEF_LIVE=~/Development/sous-chef-live cargo test --release --test performance -- --ignored --nocapture
//! ```
//!
//! It measures, on the bitbake-setup build: the first index build, a no-change refresh, a
//! refresh after touching one file and after changing one, `eval::evaluate` cold and warm,
//! `lint`, every lookup tool on a typical query, and one `sous-chef hook` call each for
//! Grep, Read and Edit. Each is run several times (`SOUS_CHEF_PERF_RUNS`, default 5) and
//! reported as median and worst. No BitBake command is run.
//!
//! Everything it writes is under `$SOUS_CHEF_LIVE/runs/perf/` (index caches) except the
//! one-line comment it appends to the build's `conf/local.conf` for the "changed file"
//! refresh, which it removes again (the file's bytes are restored exactly). The report is
//! printed and written to `$SOUS_CHEF_LIVE/runs/perf/report.txt`.
//!
//! Plain `std::time::Instant` timing: the crate has no benchmark dependency (`criterion` is
//! not in Cargo.toml and is not added for this).
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::fmt::Write as _;
use std::fs;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant, SystemTime};

use serde_json::json;
use sous_chef::bitbake::JobTable;
use sous_chef::config::Config;
use sous_chef::eval::{self, ConfigCache, Target};
use sous_chef::handles::HandleStore;
use sous_chef::index::Index;
use sous_chef::lint;
use sous_chef::mcp::Ctx;
use sous_chef::mcp::tools;
use sous_chef::workspace::Workspace;

fn runs() -> usize {
    std::env::var("SOUS_CHEF_PERF_RUNS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(5)
}

struct Stats {
    median: Duration,
    worst: Duration,
    n: usize,
}

fn stats(mut v: Vec<Duration>) -> Stats {
    v.sort();
    Stats {
        median: v[v.len() / 2],
        worst: *v.last().unwrap(),
        n: v.len(),
    }
}

fn ms(d: Duration) -> String {
    format!("{:.1}", d.as_secs_f64() * 1000.0)
}

struct Report {
    text: String,
}

impl Report {
    fn row(&mut self, what: &str, s: &Stats, target: &str) {
        let line = format!(
            "{what}\t{}\t{}\t{}\t{target}",
            ms(s.median),
            ms(s.worst),
            s.n
        );
        println!("{line}");
        let _ = writeln!(self.text, "{line}");
    }
    fn note(&mut self, s: &str) {
        println!("{s}");
        let _ = writeln!(self.text, "{s}");
    }
}

fn time<T>(f: impl FnOnce() -> T) -> (Duration, T) {
    let t = Instant::now();
    let r = f();
    (t.elapsed(), r)
}

fn config(build: &Path) -> Config {
    let mut cfg = Config {
        build_dir: Some(build.to_path_buf()),
        work_dir: build.to_path_buf(),
        writable_layers: vec!["meta-kiss".into(), "meta-kiss-ti".into()],
        ..Config::default()
    };
    cfg.bitbake.enabled = false;
    cfg
}

fn peak_rss_kib() -> Option<u64> {
    let s = fs::read_to_string("/proc/self/status").ok()?;
    s.lines()
        .find(|l| l.starts_with("VmHWM:"))
        .and_then(|l| l.split_whitespace().nth(1))
        .and_then(|v| v.parse().ok())
}

fn db_size(idx_path: &Path) -> u64 {
    ["", "-wal", "-shm"]
        .iter()
        .filter_map(|suf| fs::metadata(format!("{}{suf}", idx_path.display())).ok())
        .map(|m| m.len())
        .sum()
}

/// One `sous-chef hook` process; returns the wall time, whether it printed anything, and
/// the hook's own line from `hook.log` (its internal time and outcome).
fn hook_call(
    bin: &Path,
    cache_root: &Path,
    cwd: &Path,
    event: &str,
    log: &Path,
) -> (Duration, bool, String) {
    let before = fs::read_to_string(log).unwrap_or_default().len();
    let t = Instant::now();
    let mut child = Command::new(bin)
        .arg("hook")
        .current_dir(cwd)
        .env("XDG_CACHE_HOME", cache_root)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(event.as_bytes())
        .unwrap();
    let out = child.wait_with_output().unwrap();
    let wall = t.elapsed();
    let after = fs::read_to_string(log).unwrap_or_default();
    let line = after
        .get(before..)
        .unwrap_or("")
        .lines()
        .last()
        .unwrap_or("")
        .to_string();
    (wall, !out.stdout.is_empty(), line)
}

#[test]
#[ignore = "needs the live environment: set SOUS_CHEF_LIVE and run with --release --ignored"]
fn live_performance() {
    let Some(live) = std::env::var_os("SOUS_CHEF_LIVE").map(PathBuf::from) else {
        eprintln!("SOUS_CHEF_LIVE is not set; skipping");
        return;
    };
    let build = live.join("bitbake-builds/kiss-wrynose-dogbonedark/build");
    // `SOUS_CHEF_PERF_DIR` puts the caches and the report elsewhere (a run that must write
    // nothing under `runs/perf`).
    let perf = std::env::var_os("SOUS_CHEF_PERF_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| live.join("runs/perf"));
    fs::create_dir_all(&perf).unwrap();
    // `SOUS_CHEF_PERF_NO_EDIT=1` skips the measurements that write to the build's
    // `conf/local.conf` (touch, change, restore).
    let no_edit = std::env::var_os("SOUS_CHEF_PERF_NO_EDIT").is_some();
    let n = runs();
    let mut rep = Report {
        text: String::new(),
    };
    rep.note(&format!(
        "# sous-chef performance on the live workspace ({} build, debug assertions {})",
        if cfg!(debug_assertions) {
            "debug"
        } else {
            "release"
        },
        cfg!(debug_assertions)
    ));
    rep.note("measurement\tmedian ms\tworst ms\truns\ttarget");

    let cfg = config(&build);
    let (d, ws) = time(|| Workspace::discover(&cfg).unwrap());
    rep.note(&format!(
        "workspace discovery: {} ms, {} layers",
        ms(d),
        ws.layers.len()
    ));

    // 1. First index build: a new cache root each run.
    let mut first = Vec::new();
    let mut scanned = 0;
    let mut idx_path = PathBuf::new();
    for i in 0..n.min(3) {
        let root = perf.join(format!("cold-{i}"));
        let _ = fs::remove_dir_all(&root);
        let (d, r) = time(|| {
            let mut idx = Index::open_in(&root, &ws).unwrap();
            let r = idx.refresh(&ws).unwrap();
            (idx.path().to_path_buf(), r)
        });
        first.push(d);
        scanned = r.1.scanned;
        idx_path = r.0;
    }
    rep.row(
        "first index build (open + refresh)",
        &stats(first),
        "< 30 s (M8)",
    );
    let idx = Index::open_in(&perf.join("cold-0"), &ws).unwrap();
    let count = |sql: &str| -> i64 { idx.conn().query_row(sql, [], |r| r.get(0)).unwrap() };
    rep.note(&format!(
        "index: {} files scanned, {} files, {} statements, {} edges; database {:.1} MiB",
        scanned,
        count("SELECT count(*) FROM files"),
        count("SELECT count(*) FROM statements"),
        count("SELECT count(*) FROM edges"),
        db_size(&idx_path) as f64 / 1048576.0
    ));
    drop(idx);

    // The working index for everything below.
    let cache_root = perf.join("warm");
    let mut idx = Index::open_in(&cache_root, &ws).unwrap();
    idx.refresh(&ws).unwrap();

    // 2. No-change refresh.
    let mut v = Vec::new();
    for _ in 0..(n * 2) {
        let (d, r) = time(|| idx.refresh(&ws).unwrap());
        assert!(r.reindexed.is_empty());
        v.push(d);
    }
    rep.row("no-change refresh", &stats(v), "< 150 ms (M8)");

    // 3. Touch one file (mtime only), then change one (a comment line), then restore it.
    if no_edit {
        rep.note("SOUS_CHEF_PERF_NO_EDIT: the touch / change / refresh_file rows are skipped");
    }
    if !no_edit {
        let local = build.join("conf/local.conf");
        let original = fs::read(&local).unwrap();
        let mut touched = Vec::new();
        let mut changed = Vec::new();
        let mut single = Vec::new();
        for i in 0..n {
            let f = fs::File::options().write(true).open(&local).unwrap();
            f.set_modified(SystemTime::now()).unwrap();
            drop(f);
            let (d, _) = time(|| idx.refresh(&ws).unwrap());
            touched.push(d);
            let mut text = original.clone();
            text.extend_from_slice(format!("# sous-chef perf {i}\n").as_bytes());
            fs::write(&local, &text).unwrap();
            let (d, r) = time(|| idx.refresh(&ws).unwrap());
            assert_eq!(r.reindexed.len(), 1);
            changed.push(d);
            fs::write(&local, &original).unwrap();
            let (d, _) = time(|| idx.refresh_file(&ws, &local).unwrap());
            single.push(d);
        }
        assert_eq!(fs::read(&local).unwrap(), original);
        idx.refresh(&ws).unwrap();
        rep.row(
            "refresh after touching one file (mtime only)",
            &stats(touched),
            "",
        );
        rep.row(
            "refresh after changing one file (reparse + edges)",
            &stats(changed),
            "",
        );
        rep.row(
            "refresh_file of that file (the edit hook's path)",
            &stats(single),
            "",
        );
    }

    // 4. Evaluation: configuration pass and a recipe, cold (new cache) and warm.
    let t = |recipe: Option<&str>| Target {
        recipe: recipe.map(str::to_string),
        version: None,
        machine: Some("dogbonedark".into()),
        distro: Some("kiss".into()),
        mc: None,
    };
    let mut cold_cfg = Vec::new();
    let mut cold = Vec::new();
    for _ in 0..n {
        let (d, _) =
            time(|| eval::evaluate_with(&ConfigCache::new(), &idx, &ws, &t(None)).unwrap());
        cold_cfg.push(d);
        let (d, _) =
            time(|| eval::evaluate_with(&ConfigCache::new(), &idx, &ws, &t(Some("curl"))).unwrap());
        cold.push(d);
    }
    rep.row(
        "configuration pass (evaluate, recipe None, cold)",
        &stats(cold_cfg),
        "",
    );
    rep.row(
        "evaluate curl, cold (configuration pass included)",
        &stats(cold),
        "",
    );
    let cache = ConfigCache::new();
    eval::evaluate_with(&cache, &idx, &ws, &t(Some("curl"))).unwrap();
    let mut warm = Vec::new();
    for _ in 0..(n * 2) {
        let (d, _) = time(|| eval::evaluate_with(&cache, &idx, &ws, &t(Some("curl"))).unwrap());
        warm.push(d);
    }
    rep.row(
        "evaluate curl, warm (configuration cached)",
        &stats(warm),
        "",
    );
    let mut warm_big = Vec::new();
    for r in ["linux-kiss", "python3", "kiss-image"] {
        let (d, _) = time(|| eval::evaluate_with(&cache, &idx, &ws, &t(Some(r))).unwrap());
        warm_big.push(d);
        rep.note(&format!("evaluate {r}, warm: {} ms", ms(d)));
    }

    // 5. Lint.
    let opts = |all| lint::Options {
        rules: Vec::new(),
        min_severity: None,
        all,
        ..lint::Options::default()
    };
    for (label, target, all, reps) in [
        (
            "lint meta-kiss (layer)",
            lint::Target::Layer("meta-kiss".into()),
            false,
            n,
        ),
        (
            "lint meta-kiss-ti (layer)",
            lint::Target::Layer("meta-kiss-ti".into()),
            false,
            n,
        ),
        (
            "lint workspace (writable layers: meta-kiss, meta-kiss-ti)",
            lint::Target::Workspace,
            false,
            n,
        ),
        ("lint workspace --all", lint::Target::Workspace, true, 1),
    ] {
        let mut v = Vec::new();
        let mut last = None;
        for _ in 0..reps {
            let (d, r) = time(|| lint::run(&ws, &idx, &cfg, &target, &opts(all)).unwrap());
            v.push(d);
            last = Some(r);
        }
        rep.row(label, &stats(v), "");
        let r = last.unwrap();
        rep.note(&format!(
            "  {} files, {} findings, {} layers, {} notes",
            r.files,
            r.findings.len(),
            r.layers.len(),
            r.notes.len()
        ));
    }

    // 6. The lookup tools through their MCP entry points (each call refreshes the index
    // first, as a tool call does). The first call is cold; the rest are warm.
    drop(idx);
    let rt = tokio::runtime::Runtime::new().unwrap();
    let index = Index::open_in(&cache_root, &ws).unwrap();
    let handles = HandleStore::open(&index.cache_dir().join("handles")).unwrap();
    let ctx = Ctx {
        config: cfg.clone(),
        workspace: ws.clone(),
        index: tokio::sync::Mutex::new(index),
        handles,
        jobs: JobTable::new(),
    };
    eval::global_cache().clear();
    macro_rules! tool {
        ($label:expr, $m:ident, $args:expr) => {{
            let mut v = Vec::new();
            let mut first = Duration::ZERO;
            let mut bytes = 0;
            for i in 0..=n {
                let a: tools::$m::Args = serde_json::from_value($args).unwrap();
                let (d, out) = time(|| rt.block_on(tools::$m::run(&ctx, a)));
                let out = out.unwrap();
                bytes = out.len();
                if i == 0 {
                    first = d;
                } else {
                    v.push(d);
                }
            }
            rep.row(
                &format!(
                    "tool {} (first call {} ms, {} bytes out)",
                    $label,
                    ms(first),
                    bytes
                ),
                &stats(v),
                if stringify!($m) == "var" {
                    "warm < 100 ms (M8)"
                } else {
                    ""
                },
            );
        }};
    }
    tool!("status", status, json!({}));
    tool!("layers", layers, json!({}));
    tool!("recipe curl", recipe, json!({"name": "curl"}));
    tool!(
        "item recipe=curl name=SRC_URI",
        item,
        json!({"recipe": "curl", "name": "SRC_URI"})
    );
    tool!("search do_install", search, json!({"query": "do_install"}));
    tool!("class autotools", class, json!({"name": "autotools"}));
    tool!(
        "var SRC_URI recipe=curl",
        var,
        json!({"name": "SRC_URI", "recipe": "curl", "machine": "dogbonedark", "distro": "kiss"})
    );
    tool!(
        "overrides recipe=curl",
        overrides,
        json!({"recipe": "curl", "machine": "dogbonedark", "distro": "kiss"})
    );
    tool!("deps curl", deps, json!({"recipe": "curl"}));
    tool!(
        "lint target=meta-kiss",
        lint,
        json!({"target": "meta-kiss"})
    );
    drop(ctx);

    // 7. Hooks: one `sous-chef hook` process per call, as Claude Code runs it.
    let bin = PathBuf::from(env!("CARGO_BIN_EXE_sous-chef"));
    let log = sous_chef::index::cache_dir_in(&cache_root, &ws).join("hook.log");
    let layer_root = |name: &str| {
        ws.layers
            .iter()
            .find(|l| l.name == name)
            .unwrap()
            .root
            .clone()
    };
    let sl = layer_root("meta-kiss").join("recipes-games/sl/sl_git.bb");
    let curl = layer_root("meta").join("recipes-support/curl/curl_8.19.0.bb");
    let curl_text = fs::read_to_string(&curl).unwrap();
    let grep_lines: Vec<String> = [&curl, &sl]
        .iter()
        .flat_map(|p| {
            let t = fs::read_to_string(p).unwrap();
            t.lines()
                .enumerate()
                .filter(|(_, l)| l.contains("install") || l.contains("SRC_URI"))
                .map(|(i, l)| format!("{}:{}:{l}", p.display(), i + 1))
                .collect::<Vec<_>>()
        })
        .collect();
    let base = |tool: &str, input: serde_json::Value, resp: serde_json::Value| {
        json!({
            "session_id": "perf",
            "transcript_path": "/dev/null",
            "cwd": build,
            "permission_mode": "default",
            "hook_event_name": "PostToolUse",
            "tool_name": tool,
            "tool_input": input,
            "tool_response": resp,
            "tool_use_id": "toolu_perf",
            "duration_ms": 5
        })
        .to_string()
    };
    let grep = base(
        "Grep",
        json!({"pattern": "install|SRC_URI", "path": ws.layers[0].root, "output_mode": "content", "-n": true}),
        json!({"mode": "content", "numFiles": 2, "filenames": [], "content": grep_lines.join("\n"), "numLines": grep_lines.len()}),
    );
    let read = base(
        "Read",
        json!({"file_path": curl}),
        json!({"type": "text", "file": {"filePath": curl, "content": curl_text, "numLines": curl_text.lines().count(), "startLine": 1, "totalLines": curl_text.lines().count()}}),
    );
    // The Edit event names a real edit; the file itself is not changed (the hook only reads
    // it, so refresh_file finds it unchanged and lint runs on it).
    let edit = base(
        "Edit",
        json!({"file_path": sl, "old_string": "EXTRA_OEMAKE = \"-e\"", "new_string": "EXTRA_OEMAKE = \"-e\"", "replace_all": false}),
        json!({"filePath": sl, "oldString": "EXTRA_OEMAKE = \"-e\"", "newString": "EXTRA_OEMAKE = \"-e\"", "originalFile": null, "structuredPatch": [], "userModified": false, "replaceAll": false}),
    );
    for (label, ev) in [
        ("hook Grep", &grep),
        ("hook Read", &read),
        ("hook Edit", &edit),
    ] {
        let mut wall = Vec::new();
        let mut last = String::new();
        let mut outputs = 0;
        for _ in 0..=n {
            let (d, printed, line) = hook_call(&bin, &cache_root, &build, ev, &log);
            wall.push(d);
            outputs += usize::from(printed);
            last = line;
        }
        rep.row(
            &format!("{label} (process wall time)"),
            &stats(wall),
            "150 ms budget",
        );
        rep.note(&format!(
            "  output on {outputs} of {} calls; last log line: {}",
            n + 1,
            last.split_once(' ').map(|x| x.1).unwrap_or("")
        ));
    }

    rep.note(&format!(
        "peak RSS of the test process: {} MiB",
        peak_rss_kib().unwrap_or(0) / 1024
    ));
    let live_s = live.display().to_string();
    fs::write(
        perf.join("report.txt"),
        rep.text.replace(&live_s, "{{LIVE}}"),
    )
    .unwrap();
}
