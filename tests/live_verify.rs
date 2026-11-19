//! T9.2: checks against the live yocto-kiss environment (`docs/live-verification.md`).
//!
//! Every test that reads the live environment is `#[ignore]` and also returns early,
//! passing, unless `SOUS_CHEF_LIVE` names the directory that `scripts/live-env.sh` created,
//! so the normal gate never needs the environment. Two need nothing live and run in the
//! normal gate: `live_bug_wrapper_with_bitbake_setup_init_build_env` (a temporary script)
//! and `live_failed_fetch_log_detected` (a recorded log under `tests/fixtures/`). Run the
//! others with
//!
//! ```sh
//! SOUS_CHEF_LIVE=~/Development/sous-chef-live cargo test --test live_verify -- --ignored
//! ```
//!
//! They read the live workspace and the BitBake output recorded under
//! `$SOUS_CHEF_LIVE/runs/` (`scripts/live-env.sh --verify`, and T9.2's
//! `runs/verify/`); none of them starts BitBake. The index is built in a temporary
//! directory. A part whose recorded output is missing is skipped with a message.
//!
//! Tests named `live_bug_*` assert the behaviour BitBake shows and **fail** on the code at
//! the T9.2 commit: each reproduces one defect listed in `docs/live-verification.md`.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::collections::BTreeMap;
use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use serde_json::{Value, json};
use sous_chef::config::Config;
use sous_chef::eval::{self, Target};
use sous_chef::index::{Index, query};
use sous_chef::parse::parse;
use sous_chef::types::FileKind;
use sous_chef::workspace::Workspace;

// ---------------------------------------------------------------------------------------
// Helpers

fn live() -> Option<PathBuf> {
    let p = PathBuf::from(std::env::var_os("SOUS_CHEF_LIVE")?);
    if p.join("yocto-kiss/build/conf/bblayers.conf").is_file() {
        Some(p)
    } else {
        eprintln!("SOUS_CHEF_LIVE is set but has no yocto-kiss/build: skipping");
        None
    }
}

fn build_dir(root: &Path) -> PathBuf {
    root.join("yocto-kiss/build")
}

struct Live {
    ws: Workspace,
    idx: Index,
    _cache: tempfile::TempDir,
}

fn open(root: &Path) -> Live {
    let mut cfg = Config {
        build_dir: Some(build_dir(root)),
        writable_layers: vec!["meta-kiss".to_string(), "meta-kiss-ti".to_string()],
        work_dir: root.join("yocto-kiss"),
        ..Config::default()
    };
    cfg.bitbake.enabled = false;
    let ws = Workspace::discover(&cfg).unwrap();
    let cache = tempfile::tempdir().unwrap();
    let mut idx = Index::open_in(cache.path(), &ws).unwrap();
    idx.refresh(&ws).unwrap();
    Live {
        ws,
        idx,
        _cache: cache,
    }
}

/// A recorded output under `$SOUS_CHEF_LIVE/runs/`, or `None` (the caller skips).
fn run_output(root: &Path, name: &str) -> Option<String> {
    let p = root.join("runs").join(name);
    match fs::read_to_string(&p) {
        Ok(s) => Some(s),
        Err(_) => {
            eprintln!("{} not recorded: skipping that check", p.display());
            None
        }
    }
}

/// `VAR="value"` lines of a `bitbake -e` dump.
fn bitbake_e_values(dump: &str) -> BTreeMap<String, String> {
    let mut out = BTreeMap::new();
    for line in dump.lines() {
        let line = line.strip_prefix("export ").unwrap_or(line);
        if line.starts_with('#') {
            continue;
        }
        if let Some((name, rest)) = line.split_once("=\"")
            && let Some(v) = rest.strip_suffix('"')
            && !name.is_empty()
            && !name.contains(char::is_whitespace)
        {
            out.insert(name.to_string(), v.to_string());
        }
    }
    out
}

fn curl() -> Target {
    Target {
        recipe: Some("curl".to_string()),
        version: None,
        machine: None,
        distro: None,
        mc: None,
    }
}

fn canon(p: &Path) -> PathBuf {
    fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf())
}

/// Runs `sous-chef serve` on the kas build and returns the text of each tool call.
fn mcp(root: &Path, calls: &[(&str, Value)]) -> Vec<String> {
    let tmp = tempfile::tempdir().unwrap();
    let cfg = tmp.path().join("sous-chef.toml");
    fs::write(
        &cfg,
        format!(
            "build_dir = {:?}\nwritable_layers = [\"meta-kiss\", \"meta-kiss-ti\"]\n\
             [bitbake]\nenabled = false\n[output]\nbudget_tokens = 200000\n",
            build_dir(root).display().to_string()
        ),
    )
    .unwrap();
    let mut child = Command::new(env!("CARGO_BIN_EXE_sous-chef"))
        .arg("--config")
        .arg(&cfg)
        .arg("serve")
        .current_dir(tmp.path())
        .env("XDG_CACHE_HOME", tmp.path().join("cache"))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let mut stdin = child.stdin.take().unwrap();
    let mut lines = BufReader::new(child.stdout.take().unwrap()).lines();
    let mut id = 0u64;
    let mut request = |stdin: &mut std::process::ChildStdin, method: &str, params: Value| {
        id += 1;
        let msg = json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params});
        writeln!(stdin, "{msg}").unwrap();
        stdin.flush().unwrap();
        loop {
            let line = lines.next().unwrap().unwrap();
            let v: Value = serde_json::from_str(&line).unwrap();
            if v["id"] == json!(id) {
                return v;
            }
        }
    };
    request(
        &mut stdin,
        "initialize",
        json!({"protocolVersion": "2025-06-18", "capabilities": {},
               "clientInfo": {"name": "live_verify", "version": "0"}}),
    );
    writeln!(
        stdin,
        "{}",
        json!({"jsonrpc": "2.0", "method": "notifications/initialized"})
    )
    .unwrap();
    let mut out = Vec::new();
    for (tool, args) in calls {
        let r = request(
            &mut stdin,
            "tools/call",
            json!({"name": tool, "arguments": args}),
        );
        let text = r["result"]["content"][0]["text"]
            .as_str()
            .unwrap_or("")
            .to_string();
        out.push(text);
    }
    drop(stdin);
    let _ = child.wait();
    out
}

/// Rows of a tab-separated tool table, header and notes left out.
fn rows(text: &str) -> Vec<Vec<String>> {
    text.lines()
        .skip(1)
        .filter(|l| l.contains('\t'))
        .map(|l| l.split('\t').map(str::to_string).collect())
        .collect()
}

// ---------------------------------------------------------------------------------------
// 1. Discovery and release

/// `bitbake-layers show-layers`: the same layers in the same order, with the same
/// priorities; BitBake names a layer by its collection, sous-chef by its directory.
#[test]
#[ignore = "needs SOUS_CHEF_LIVE"]
fn live_discovery_matches_show_layers() {
    let Some(root) = live() else { return };
    let l = open(&root);
    assert_eq!(l.ws.release.release.to_string(), "wrynose");
    let Some(out) = run_output(&root, "show-layers.out") else {
        return;
    };
    let mut bb: Vec<(String, PathBuf, i32)> = Vec::new();
    let mut table = false;
    for line in out.lines() {
        if line.starts_with("=====") {
            table = true;
            continue;
        }
        let w: Vec<&str> = line.split_whitespace().collect();
        if table && w.len() == 3 {
            bb.push((
                w[0].to_string(),
                canon(Path::new(w[1])),
                w[2].parse().unwrap(),
            ));
        }
    }
    assert_eq!(bb.len(), 5, "{out}");
    let ours: Vec<&sous_chef::workspace::Layer> = l.ws.layers.iter().collect();
    assert_eq!(ours.len(), bb.len());
    for (layer, (coll, path, prio)) in ours.iter().zip(&bb) {
        assert_eq!(canon(&layer.root), *path, "layer order");
        assert_eq!(layer.priority, *prio, "priority of {}", layer.name);
        assert!(
            layer.collections.contains(coll),
            "{} collections {:?} lack {coll}",
            layer.name,
            layer.collections
        );
    }
    let writable: Vec<&str> = ours
        .iter()
        .filter(|x| x.writable)
        .map(|x| x.name.as_str())
        .collect();
    assert_eq!(writable, ["meta-kiss", "meta-kiss-ti"]);
}

// ---------------------------------------------------------------------------------------
// 2. Index and parser

/// Every BitBake metadata file of the layers parses with no diagnostic except the six
/// "whitespace after `\`" warnings, which are real (checked with grep).
#[test]
#[ignore = "needs SOUS_CHEF_LIVE"]
fn live_parser_has_no_diagnostics_on_real_metadata() {
    let Some(root) = live() else { return };
    let l = open(&root);
    let mut files = 0;
    let mut diags: Vec<String> = Vec::new();
    for f in query::files_by_kind(
        &l.idx,
        &[
            FileKind::Recipe,
            FileKind::Append,
            FileKind::Class,
            FileKind::Include,
            FileKind::Conf,
        ],
    )
    .unwrap()
    {
        let p = f.path.to_string_lossy().to_string();
        // Not BitBake metadata: data files a recipe installs, wic includes, ccache config.
        let metadata = match f.kind {
            FileKind::Conf => p.starts_with("conf/") && p != "conf/ccache.conf",
            FileKind::Include => !p.contains("/files/") && !p.contains(".wks"),
            _ => true,
        };
        if !metadata || f.layer == "build" {
            continue;
        }
        let abs = l.ws.layer(&f.layer).unwrap().root.join(&f.path);
        let src = fs::read_to_string(&abs).unwrap();
        files += 1;
        for d in parse(&src).diagnostics {
            diags.push(format!("{}:{}:{} {}", f.layer, p, d.span.start, d.message));
        }
    }
    assert!(files > 1600, "{files}");
    // `addfragments` (since w5-fix-eval2) and `addpylib` (since w7-fix, in
    // `meta-arm:conf/layer.conf:21` and `meta:conf/layer.conf:147`) are statement kinds of
    // their own and have no diagnostic.
    let odd: Vec<&String> = diags
        .iter()
        .filter(|d| !d.contains("whitespace after"))
        .collect();
    assert!(odd.is_empty(), "{odd:#?}");
    assert_eq!(diags.len(), 6, "{diags:#?}");
}

/// The scanner indexes `.conf` / `.inc` files that are not BitBake metadata (a recipe's
/// `files/wpa_supplicant.conf`, `files/wic/common.wks.inc`, `conf/ccache.conf`) and
/// `hygiene/parse-error` then reports 422 errors in 29 such files of OE-core.
#[test]
#[ignore = "needs SOUS_CHEF_LIVE"]
fn live_bug_data_files_are_not_metadata() {
    let Some(root) = live() else { return };
    let l = open(&root);
    let indexed: Vec<String> = query::files_by_kind(&l.idx, &[FileKind::Conf, FileKind::Include])
        .unwrap()
        .into_iter()
        .map(|f| format!("{}:{}", f.layer, f.path.display()))
        .collect();
    for data in [
        "meta:recipes-connectivity/wpa-supplicant/wpa-supplicant/wpa_supplicant.conf",
        "meta:recipes-extended/lighttpd/lighttpd/lighttpd.conf",
        "meta:files/wic/common.wks.inc",
    ] {
        assert!(
            !indexed.iter().any(|f| f == data),
            "{data} is indexed as BitBake metadata"
        );
    }
}

// ---------------------------------------------------------------------------------------
// 3. bbappends and shadowed recipes

/// `bitbake-layers show-appends`: every (recipe, bbappends in order) pair BitBake lists
/// should be the same in the index. It is for 19 of 20 recipes; for `optee-os` the two
/// same-priority appends `optee-ftpm/optee-os_%.bbappend` and `optee/optee-os_%.bbappend`
/// come in the other order, because `resolve::recipe_files` sorts the glob matches as
/// paths (component by component: `optee` < `optee-ftpm`) where BitBake sorts strings
/// (`-` < `/`: `optee-ftpm/` < `optee/`).
#[test]
#[ignore = "needs SOUS_CHEF_LIVE"]
fn live_bug_appends_order_vs_show_appends() {
    let Some(root) = live() else { return };
    let l = open(&root);
    let Some(out) = run_output(&root, "show-appends.out") else {
        return;
    };
    let mut bb: BTreeMap<String, Vec<PathBuf>> = BTreeMap::new();
    let mut cur: Option<String> = None;
    for line in out.lines() {
        // `<recipe>.bb:` or `<recipe>.bb (skipped):`; a recipe may be listed twice.
        if let Some(r) = line.strip_suffix(':')
            && !line.starts_with(' ')
            && r.trim_end_matches(" (skipped)").ends_with(".bb")
        {
            cur = Some(r.trim_end_matches(" (skipped)").to_string());
            bb.entry(cur.clone().unwrap()).or_default().clear();
        } else if let (Some(c), Some(p)) = (&cur, line.strip_prefix("  ")) {
            bb.get_mut(c).unwrap().push(canon(Path::new(p.trim())));
        }
    }
    assert!(bb.len() >= 19, "{bb:#?}");
    let recipes = query::files_by_kind(&l.idx, &[FileKind::Recipe]).unwrap();
    for (name, appends) in &bb {
        let r = recipes
            .iter()
            .find(|r| r.path.file_name().unwrap().to_string_lossy() == *name)
            .unwrap_or_else(|| panic!("{name} not indexed"));
        let ours: Vec<PathBuf> = query::appends_for(&l.idx, r.id)
            .unwrap()
            .iter()
            .map(|a| canon(&l.ws.layer(&a.layer).unwrap().root.join(&a.path)))
            .collect();
        assert_eq!(&ours, appends, "appends of {name}");
    }
}

/// `show-overlayed` says "No overlayed files found"; `layers show=shadowed` agrees.
#[test]
#[ignore = "needs SOUS_CHEF_LIVE"]
fn live_no_shadowed_recipes() {
    let Some(root) = live() else { return };
    if let Some(out) = run_output(&root, "show-overlayed.out") {
        assert!(out.contains("No overlayed files found."), "{out}");
    }
    let t = mcp(&root, &[("layers", json!({"show": "shadowed"}))]);
    assert!(
        t[0].contains("no recipe or class file is provided by more than one layer"),
        "{}",
        t[0]
    );
}

/// meta-arm's `dynamic-layers/meta-tpm2/.../tpm2-tools_5.7.bbappend` is collected only
/// through `BBFILES_DYNAMIC` when meta-tpm2 is present, so BitBake never sees it; `layers
/// show=appends` reports it as `no-match` and says BitBake stops with a fatal error.
#[test]
#[ignore = "needs SOUS_CHEF_LIVE"]
fn live_bug_dynamic_layer_append_reported_as_no_match() {
    let Some(root) = live() else { return };
    let t = mcp(&root, &[("layers", json!({"show": "appends"}))]);
    let no_match: Vec<Vec<String>> = rows(&t[0])
        .into_iter()
        .filter(|r| r.get(3).is_some_and(|s| s == "no-match"))
        .collect();
    assert!(no_match.is_empty(), "{no_match:?}");
}

// ---------------------------------------------------------------------------------------
// 4. Evaluation spot checks on curl

/// The values the static evaluator gets right on curl, equal to `bitbake -e curl`.
#[test]
#[ignore = "needs SOUS_CHEF_LIVE"]
fn live_curl_static_values_equal_bitbake() {
    let Some(root) = live() else { return };
    let Some(dump) = run_output(&root, "bitbake-e-curl.out") else {
        return;
    };
    let bb = bitbake_e_values(&dump);
    let l = open(&root);
    let ds = eval::evaluate(&l.idx, &l.ws, &curl()).unwrap();
    for name in [
        "SRC_URI",
        "FILE_LAYERNAME",
        "TMPDIR",
        "PREFERRED_PROVIDER_virtual/kernel",
        "PREFERRED_PROVIDER_virtual/bootloader",
        "LAYERSERIES_CORENAMES",
    ] {
        let ours = ds.value(name).unwrap_or_else(|| panic!("{name} not set"));
        assert_eq!(Some(&ours.text), bb.get(name), "{name}");
    }
    // SRC_URI has override variants; until x1 it was flagged because OVERRIDES held python.
    // With the pure helpers evaluated, OVERRIDES is known and SRC_URI is certain.
    assert!(!ds.value("SRC_URI").unwrap().unevaluated_python);
    assert!(!ds.value("FILE_LAYERNAME").unwrap().unevaluated_python);
    // Values built from the tune's python: equal since x1 (OVERRIDES and MACHINEOVERRIDES
    // also certain; the work directories may still carry a flag from elsewhere).
    for name in ["OVERRIDES", "MACHINEOVERRIDES", "S", "WORKDIR", "T"] {
        let ours = ds.value(name).unwrap();
        assert_eq!(Some(&ours.text), bb.get(name), "{name}");
        if name.ends_with("OVERRIDES") {
            assert!(
                !ours.unevaluated_python && ours.unresolved.is_empty(),
                "{name} equal but flagged: {ours:?}"
            );
        }
    }
    // A value with python outside the subset (`get_base_dep(d)`): different, and flagged.
    let ours = ds.value("DEPENDS").unwrap();
    assert_ne!(Some(&ours.text), bb.get("DEPENDS"));
    assert!(
        ours.unevaluated_python,
        "DEPENDS differs but is not flagged"
    );
}

/// `overrides show=chain` splits `OVERRIDES` on `:` inside `${@...}`, so python fragments
/// such as `', '', d)}${@bb.utils.contains('TUNE_FEATURES', 'armv4', 'armv4` are listed as
/// entries (and later used as "known" names by `show=dead`).
#[test]
#[ignore = "needs SOUS_CHEF_LIVE"]
fn live_bug_overrides_chain_splits_python() {
    let Some(root) = live() else { return };
    let t = mcp(
        &root,
        &[("overrides", json!({"recipe": "curl", "show": "chain"}))],
    );
    let broken: Vec<String> = rows(&t[0])
        .into_iter()
        .filter_map(|r| r.get(1).cloned())
        .filter(|e| e.matches("${").count() != e.matches('}').count())
        .collect();
    assert!(
        broken.is_empty(),
        "python fragments as entries: {broken:#?}"
    );
}

/// `inherit_defer toolchain/${TOOLCHAIN}` (base.bbclass) is not followed: `TOOLCHAIN` is
/// `gcc` but flagged as python because of the override chain, so `toolchain/gcc.bbclass`
/// is never parsed and `TCOVERRIDE` (`toolchain-gcc` in BitBake's `OVERRIDES`) is unset.
#[test]
#[ignore = "needs SOUS_CHEF_LIVE"]
fn live_bug_toolchain_class_not_inherited() {
    let Some(root) = live() else { return };
    let l = open(&root);
    let ds = eval::evaluate(&l.idx, &l.ws, &curl()).unwrap();
    assert_eq!(
        ds.value("TOOLCHAIN").map(|v| v.text),
        Some("gcc".to_string())
    );
    let refs: Vec<String> = ds
        .replay()
        .references
        .iter()
        .map(|r| format!("{} {}", r.kind, r.text))
        .collect();
    assert!(
        !refs.iter().any(|r| r.contains("toolchain/${TOOLCHAIN}")),
        "{refs:#?}"
    );
    assert_eq!(
        ds.value("TCOVERRIDE").map(|v| v.text),
        Some("toolchain-gcc".to_string())
    );
}

// ---------------------------------------------------------------------------------------
// 5. Providers

/// `deps kind=providers` picks the providers BitBake reports (`bitbake -e virtual/kernel`
/// gives `FILE` = meta-kiss's linux-kiss_7.0.bb).
#[test]
#[ignore = "needs SOUS_CHEF_LIVE"]
fn live_providers_match_bitbake() {
    let Some(root) = live() else { return };
    let t = mcp(
        &root,
        &[
            (
                "deps",
                json!({"recipe": "virtual/kernel", "kind": "providers"}),
            ),
            (
                "deps",
                json!({"recipe": "virtual/bootloader", "kind": "providers"}),
            ),
            (
                "deps",
                json!({"recipe": "curl-native", "kind": "providers"}),
            ),
        ],
    );
    let first = |s: &str| rows(s).into_iter().next().unwrap_or_default();
    let k = first(&t[0]);
    assert_eq!(k[0], "linux-kiss");
    assert!(k[2].starts_with("selected by PREFERRED_PROVIDER_virtual/kernel"));
    let b = first(&t[1]);
    assert_eq!(b[0], "u-boot-kiss");
    // Since w10 a BBCLASSEXTEND variant is a provider of its name (BitBake: curl_8.19.0.bb).
    let c = first(&t[2]);
    assert_eq!(c[0], "curl-native", "{}", t[2]);
    assert!(t[2].contains("the `native` variant of curl"), "{}", t[2]);
}

/// `deps kind=build` splits `DEPENDS` on whitespace inside `${@...}` and lists fragments
/// such as `'1',` and `'',` as dependencies (the latter "provided" by libglvnd).
#[test]
#[ignore = "needs SOUS_CHEF_LIVE"]
fn live_bug_deps_build_splits_python() {
    let Some(root) = live() else { return };
    let t = mcp(
        &root,
        &[("deps", json!({"recipe": "curl", "kind": "build"}))],
    );
    let broken: Vec<String> = rows(&t[0])
        .into_iter()
        .filter_map(|r| r.get(1).cloned())
        .filter(|d| d.contains('\'') || d.contains(','))
        .collect();
    assert!(
        broken.is_empty(),
        "python fragments as dependencies: {broken:?}"
    );
}

// ---------------------------------------------------------------------------------------
// 6. The environment runner

/// bitbake-setup's `init-build-env` ends with `set --`, which clears the wrapper's
/// positional parameters: `shift 2` fails and the program never runs (exit 1, no output).
/// Reproduced with a script of the same shape; BitBake is not started. Needs nothing live
/// (a temporary directory and `bash`), so it runs in the normal gate.
#[test]
fn live_bug_wrapper_with_bitbake_setup_init_build_env() {
    let tmp = tempfile::tempdir().unwrap();
    let oe = tmp.path().join("oe-init-build-env");
    fs::write(&oe, "BUILDDIR=\"$1\"; export BUILDDIR\n").unwrap();
    let init = tmp.path().join("init-build-env");
    fs::write(
        &init,
        format!(
            "# init-build-env wrapper created by bitbake-setup\ncd {}\nset {}\n. ./oe-init-build-env\nset --\n",
            tmp.path().display(),
            tmp.path().join("build").display()
        ),
    )
    .unwrap();
    let out = Command::new("bash")
        .arg("-c")
        .arg(sous_chef::bitbake::env::WRAPPER)
        .arg("--")
        .arg(&init)
        .arg(tmp.path().join("build"))
        .arg("echo")
        .arg("program ran")
        .output()
        .unwrap();
    assert_eq!(
        String::from_utf8_lossy(&out.stdout).trim(),
        "program ran",
        "exit {:?}",
        out.status.code()
    );
}

// ---------------------------------------------------------------------------------------
// 7. Logs

fn live_sample(name: &str) -> String {
    let p = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/outputs/live/broken-fetch")
        .join(name);
    fs::read_to_string(p).unwrap()
}

/// The real failed `do_fetch` log: the first error is the `FetchError`, with the URL.
/// Reads only the recorded copy under `tests/fixtures/outputs/live/broken-fetch/`, so it
/// runs in the normal gate.
#[test]
fn live_failed_fetch_log_detected() {
    let log = live_sample("log.do_fetch.1484343");
    let f = sous_chef::bitbake::logs::detect(&log);
    let first = sous_chef::bitbake::logs::first_error(&f).unwrap();
    assert_eq!(
        first.message,
        "FetchError: Unable to fetch URL from any source."
    );
    assert_eq!(
        first.detail.as_deref(),
        Some("https://sous-chef-verify.invalid/broken-fetch-1.0.tar.gz")
    );
    let console = live_sample("console.out");
    let logs = sous_chef::bitbake::logs::failure_logfiles(&console);
    assert_eq!(logs.len(), 1, "{logs:?}");
    assert!(logs[0].ends_with("broken-fetch/1.0/temp/log.do_fetch.1484343"));
    let tasks = sous_chef::bitbake::logs::failed_tasks(&console);
    assert_eq!(tasks.len(), 1, "{tasks:?}");
}

/// The Bash hook stores the real console output (5.6 KB) and replaces it with a digest
/// under 1 KB that names the recipe, the task and the first error.
#[test]
#[ignore = "needs SOUS_CHEF_LIVE"]
fn live_bash_hook_digests_real_fetch_failure() {
    let Some(root) = live() else { return };
    let cache = tempfile::tempdir().unwrap();
    // The hook needs a workspace (it stores the output under the workspace's cache).
    fs::write(
        cache.path().join("sous-chef.toml"),
        format!("build_dir = {:?}\n", build_dir(&root).display().to_string()),
    )
    .unwrap();
    let console = live_sample("console.out");
    // The hook opens an existing index read-only; build it first.
    let indexed = Command::new(env!("CARGO_BIN_EXE_sous-chef"))
        .arg("index")
        .current_dir(cache.path())
        .env("XDG_CACHE_HOME", cache.path())
        .output()
        .unwrap();
    assert!(indexed.status.success(), "{indexed:?}");
    let event = json!({
        "session_id": "0", "transcript_path": "/dev/null", "cwd": cache.path(),
        "permission_mode": "default", "hook_event_name": "PostToolUse", "tool_name": "Bash",
        "tool_input": {"command": "cd build && bitbake -c fetch broken-fetch 2>&1"},
        "tool_response": {"stdout": console, "stderr": "", "interrupted": false, "isImage": false},
        "tool_use_id": "t", "duration_ms": 1
    });
    let mut child = Command::new(env!("CARGO_BIN_EXE_sous-chef"))
        .arg("hook")
        .current_dir(cache.path())
        .env("XDG_CACHE_HOME", cache.path())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(event.to_string().as_bytes())
        .unwrap();
    let out = child.wait_with_output().unwrap();
    let v: Value = serde_json::from_slice(&out.stdout).expect("a hook response");
    let text = v["hookSpecificOutput"]["updatedToolOutput"]["stdout"]
        .as_str()
        .unwrap();
    assert!(text.len() < 1024, "{} bytes", text.len());
    for want in [
        "recipe\tbroken-fetch",
        "task\tdo_fetch",
        "FetchError: Unable to fetch URL from any source.",
    ] {
        assert!(text.contains(want), "{want} missing in {text}");
    }
    // The same command under `kas shell -c '...'` is recognised too (T9.3).
    assert!(sous_chef::hook::bash::runs_bitbake(
        "kas shell kas/kiss.yaml -c 'bitbake -c fetch broken-fetch'"
    ));
}
