//! Shared helpers for integration tests. Include with `mod common;`.
//!
//! Not every test binary uses every helper, hence the dead-code allowance.
#![allow(dead_code)]
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::fs;
use std::path::{Path, PathBuf};

use sous_chef::config::Config;
use sous_chef::workspace::Workspace;

/// Absolute path of `tests/fixtures`.
pub fn fixtures_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures")
}

/// Absolute path of a file or directory under `tests/fixtures`, e.g.
/// `fixture_path("ws/meta-core/conf/layer.conf")` or `fixture_path("syntax/crlf.bb")`.
pub fn fixture_path(rel: &str) -> PathBuf {
    fixtures_dir().join(rel)
}

/// Absolute path of the fixture workspace, `tests/fixtures/ws`.
pub fn fixture_ws_root() -> PathBuf {
    fixture_path("ws")
}

/// The configuration `tests/fixtures/ws/sous-chef.toml` describes, built directly (no file
/// loading): build dir `ws/build`, `meta-mine` writable, bitbake disabled, discovery from
/// the workspace root.
pub fn ws_config(root: &Path) -> Config {
    let mut cfg = Config {
        build_dir: Some(root.join("build")),
        writable_layers: vec!["meta-mine".to_string()],
        work_dir: root.to_path_buf(),
        ..Config::default()
    };
    cfg.bitbake.enabled = false;
    cfg
}

/// Configuration for the fixture workspace in the source tree.
pub fn fixture_config() -> Config {
    ws_config(&fixture_ws_root())
}

/// The discovered fixture workspace (three layers, through `build/conf/bblayers.conf`).
/// Panics if discovery fails.
pub fn fixture_ws() -> Workspace {
    Workspace::discover(&fixture_config()).expect("discovering the fixture workspace")
}

/// A writable copy of the fixture workspace in a temporary directory. Keep the `TempDir`
/// alive for as long as the copy is used; the `PathBuf` is the copy's `ws` root.
pub fn temp_ws() -> (tempfile::TempDir, PathBuf) {
    let tmp = tempfile::tempdir().expect("creating a temp dir");
    let root = tmp.path().join("ws");
    copy_dir(&fixture_ws_root(), &root);
    (tmp, root)
}

/// A writable copy of the fixture workspace whose `sous-chef.toml` sets
/// `release = "<codename>"`, with the configuration loaded from that file. `edit(ws root)`
/// runs before discovery. Keep the `TempDir` alive while the workspace is used.
pub fn temp_ws_release(
    codename: &str,
    edit: impl FnOnce(&Path),
) -> (tempfile::TempDir, PathBuf, Config, Workspace) {
    let (tmp, root) = temp_ws();
    let root = fs::canonicalize(root).unwrap();
    let toml = root.join("sous-chef.toml");
    let mut text = fs::read_to_string(&toml).unwrap();
    // Top-level keys must come before the first table.
    text = format!("release = \"{codename}\"\n{text}");
    fs::write(&toml, text).unwrap();
    edit(&root);
    let cfg = Config::load(&sous_chef::config::CliOverrides {
        config: Some(toml),
        work_dir: root.clone(),
        ..Default::default()
    })
    .unwrap();
    let ws = Workspace::discover(&cfg).unwrap();
    assert_eq!(ws.release.release.codename(), codename);
    (tmp, root, cfg, ws)
}

/// Recursively copies a directory (regular files and directories only).
pub fn copy_dir(from: &Path, to: &Path) {
    fs::create_dir_all(to).unwrap();
    for entry in fs::read_dir(from).unwrap() {
        let entry = entry.unwrap();
        let src = entry.path();
        let dst = to.join(entry.file_name());
        if entry.file_type().unwrap().is_dir() {
            copy_dir(&src, &dst);
        } else {
            fs::copy(&src, &dst).unwrap();
        }
    }
}

/// Every regular file under `dir`, sorted.
pub fn files_under(dir: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        for entry in fs::read_dir(&d).unwrap() {
            let entry = entry.unwrap();
            if entry.file_type().unwrap().is_dir() {
                stack.push(entry.path());
            } else {
                out.push(entry.path());
            }
        }
    }
    out.sort();
    out
}

/// True while `pid` runs: it exists and is not a zombie. Asks `ps -o stat= -p PID`, which
/// procps (Linux) and BSD `ps` (macOS) both understand, instead of reading `/proc`, which
/// macOS does not have. `ps` prints nothing, and exits 1, for a pid that is gone; a zombie's
/// state starts with `Z` (`X`, dead, is Linux only).
pub fn process_alive(pid: u32) -> bool {
    let out = std::process::Command::new("ps")
        .args(["-o", "stat=", "-p", &pid.to_string()])
        .output()
        .expect("running ps");
    let stat = String::from_utf8_lossy(&out.stdout);
    let stat = stat.trim();
    !stat.is_empty() && !stat.starts_with('Z') && !stat.starts_with('X')
}
