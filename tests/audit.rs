//! T8.6: the workspace audit report. The fixture workspace in both formats (snapshots), a
//! copy with a layer on disk that `bblayers.conf` does not enable, a `.gitmodules` file and
//! git facts from a repository created here, the kas project fixture (pins and lock file),
//! the MCP tool's digest and its handle through `slice`, determinism, the `root` argument,
//! and the three index queries the report added.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;
mod kas_support;

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use sous_chef::audit::{self, Audit, Options};
use sous_chef::bitbake::JobTable;
use sous_chef::config::Config;
use sous_chef::handles::{Handle, HandleStore};
use sous_chef::index::{Index, query};
use sous_chef::mcp::Ctx;
use sous_chef::mcp::tools::{audit as audit_tool, slice};
use sous_chef::render::{self, Format, JsonTable};
use sous_chef::workspace::Workspace;
use sous_chef::workspace::kas::KasEnv;

struct Fixture {
    _cache: tempfile::TempDir,
    ctx: Ctx,
}

fn ctx_for(config: Config, workspace: Workspace) -> Fixture {
    let cache = tempfile::tempdir().unwrap();
    let mut index = Index::open_in(cache.path(), &workspace).unwrap();
    index.refresh(&workspace).unwrap();
    let handles = HandleStore::open(&index.cache_dir().join("handles")).unwrap();
    let ctx = Ctx {
        config,
        workspace,
        index: tokio::sync::Mutex::new(index),
        handles,
        jobs: JobTable::new(),
    };
    Fixture { _cache: cache, ctx }
}

fn fixture_with(config: Config) -> Fixture {
    let workspace = Workspace::discover_in(&config, &KasEnv::default()).unwrap();
    ctx_for(config, workspace)
}

async fn build(f: &Fixture, root: &Path) -> Audit {
    let opts = Options {
        generated: Some("TIMESTAMP".into()),
        ..Options::new(root)
    };
    audit::build(&f.ctx, &opts).await.unwrap()
}

/// The report with the `Generated:` line (time and version) masked.
fn masked_md(a: &Audit) -> String {
    mask_md(&audit::render_markdown(a))
}

fn mask_md(md: &str) -> String {
    let mut lines: Vec<&str> = md.lines().collect();
    assert!(
        lines[0].starts_with(audit::GENERATED_PREFIX),
        "{}",
        lines[0]
    );
    lines[0] = "Generated: [masked]";
    lines.join("\n")
}

fn masked_json(a: &Audit) -> String {
    let mut v: serde_json::Value = serde_json::from_str(&audit::render_json(a)).unwrap();
    v["generated"] = "[masked]".into();
    v["sous_chef_version"] = "[masked]".into();
    serde_json::to_string_pretty(&v).unwrap()
}

/// Runs git for a test: argv, no user configuration, no signing, no hooks.
fn git(dir: &Path, args: &[&str]) -> bool {
    Command::new("git")
        .args([
            "-c",
            "user.name=test",
            "-c",
            "user.email=test@example.com",
            "-c",
            "commit.gpgsign=false",
            "-c",
            "core.hooksPath=/dev/null",
            "-c",
            "init.defaultBranch=main",
        ])
        .args(args)
        .current_dir(dir)
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_INDEX_FILE")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .output()
        .is_ok_and(|o| o.status.success())
}

fn write(path: &Path, text: &str) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, text).unwrap();
}

fn layer_conf(collection: &str, compat: &str) -> String {
    format!(
        "BBPATH .= \":${{LAYERDIR}}\"\n\
         BBFILES += \"${{LAYERDIR}}/recipes-*/*/*.bb\"\n\
         BBFILE_COLLECTIONS += \"{collection}\"\n\
         BBFILE_PATTERN_{collection} = \"^${{LAYERDIR}}/\"\n\
         BBFILE_PRIORITY_{collection} = \"8\"\n\
         LAYERDEPENDS_{collection} = \"core\"\n\
         LAYERSERIES_COMPAT_{collection} = \"{compat}\"\n"
    )
}

// ---------------------------------------------------------------------------------------
// The fixture workspace

#[tokio::test]
async fn fixture_workspace_markdown_and_json() {
    let f = fixture_with(common::fixture_config());
    let root = common::fixture_ws_root();
    let a = build(&f, &root).await;
    let md = masked_md(&a);
    insta::assert_snapshot!("audit_fixture_md", md);
    insta::assert_snapshot!("audit_fixture_json", masked_json(&a));

    // The enabled layers, and meta-legacy present but not enabled (from its layer.conf).
    let names: Vec<(&str, bool)> = a
        .inventory
        .layers
        .iter()
        .map(|l| (l.name.as_str(), l.enabled))
        .collect();
    assert_eq!(
        names,
        [
            ("meta-core", true),
            ("meta-vendor", true),
            ("meta-mine", true),
            ("meta-legacy", false)
        ]
    );
    let legacy = &a.inventory.layers[3];
    assert_eq!(legacy.collections, ["legacy"]);
    assert_eq!(legacy.compat, ["dunfell"]);
    assert!(legacy.files.is_none());
    assert_eq!(a.releases.layers[3].older_than_floor, ["dunfell"]);
    // The fixture is no repository of its own: git facts are unknown, and the enclosing
    // sous-chef checkout is not reported.
    assert!(a.inventory.repositories.is_empty());
    // lint --all on the fixture: 7 findings (docs/lint-rules.md).
    assert_eq!(a.lint.own_total.total() + a.lint.upstream_total.total(), 7);
    // No absolute path in either format.
    let root_text = fs::canonicalize(&root).unwrap().display().to_string();
    assert!(!md.contains(&root_text));
    assert!(!audit::render_json(&a).contains(&root_text));
    // Every section starts with its summary line.
    for h in [
        "## Summary",
        "## 1. Inventory",
        "## 2. Releases",
        "## 3. Layer structure",
        "## 4. Lint findings",
        "## 5. Parse and resolution problems",
        "## 6. What was not checked",
    ] {
        let at = md.find(h).unwrap_or_else(|| panic!("{h} missing"));
        let after: Vec<&str> = md[at..].lines().skip(1).filter(|l| !l.is_empty()).collect();
        assert!(
            !after[0].starts_with('#') && !after[0].starts_with('|'),
            "{h} does not start with a summary line: {}",
            after[0]
        );
    }
}

#[tokio::test]
async fn two_runs_differ_only_in_the_generated_line() {
    let f = fixture_with(common::fixture_config());
    let root = common::fixture_ws_root();
    let a = audit::build(&f.ctx, &Options::new(&root)).await.unwrap();
    let b = audit::build(&f.ctx, &Options::new(&root)).await.unwrap();
    let (ma, mb) = (audit::render_markdown(&a), audit::render_markdown(&b));
    let la: Vec<&str> = ma.lines().collect();
    let lb: Vec<&str> = mb.lines().collect();
    assert_eq!(la.len(), lb.len());
    for (i, (x, y)) in la.iter().zip(&lb).enumerate() {
        if x != y {
            assert_eq!(i, 0, "line {} differs: {x:?} vs {y:?}", i + 1);
        }
    }
    assert_eq!(masked_json(&a), masked_json(&b));
}

// ---------------------------------------------------------------------------------------
// A repository: a layer on disk that bblayers.conf does not enable, .gitmodules, git facts

#[tokio::test]
async fn layer_not_enabled_gitmodules_and_git_facts() {
    let (_tmp, root) = common::temp_ws();
    let root = fs::canonicalize(root).unwrap();
    // A layer on disk that the build does not enable.
    write(
        &root.join("meta-extra/conf/layer.conf"),
        &layer_conf("extra", "scarthgap"),
    );
    write(
        &root.join("meta-extra/recipes-x/x/x_1.0.bb"),
        "SUMMARY = \"x\"\n",
    );
    // Build output is never searched.
    write(
        &root.join("build/tmp/work/meta-junk/conf/layer.conf"),
        &layer_conf("junk", "wrynose"),
    );
    if !git(&root, &["init", "-q"]) {
        eprintln!("git is not available; skipping the git part of this test");
        return;
    }
    // A submodule-like layer: its own repository, declared in .gitmodules.
    let sub = root.join("layers/meta-sub");
    write(&sub.join("conf/layer.conf"), &layer_conf("sub", "wrynose"));
    assert!(git(&sub, &["init", "-q"]));
    assert!(git(&sub, &["add", "."]));
    assert!(git(&sub, &["commit", "-q", "-m", "sub"]));
    assert!(git(
        &sub,
        &[
            "remote",
            "add",
            "origin",
            "https://bob:s3cr3t-token@example.com/meta-sub.git"
        ]
    ));
    write(
        &root.join(".gitmodules"),
        "[submodule \"meta-sub\"]\n\tpath = layers/meta-sub\n\turl = https://bob:s3cr3t-token@example.com/meta-sub.git\n\
         [submodule \"missing\"]\n\tpath = layers/missing\n\turl = https://example.com/missing.git\n",
    );
    fs::create_dir_all(root.join("layers/missing")).unwrap();
    write(&root.join(".gitignore"), "build/tmp/\n");
    assert!(git(&root, &["add", "-A", "--", ".", ":!layers/meta-sub"]));
    assert!(git(&root, &["commit", "-q", "-m", "root"]));
    assert!(git(
        &root,
        &[
            "remote",
            "add",
            "origin",
            "https://alice:ghp_TOKEN123@example.com/org/build.git"
        ]
    ));
    // A dirty file in the root repository.
    write(&root.join("meta-mine/README"), "uncommitted\n");

    let f = fixture_with(common::ws_config(&root));
    let a = build(&f, &root).await;
    let md = audit::render_markdown(&a);
    let json = audit::render_json(&a);

    let layer = |name: &str| a.inventory.layers.iter().find(|l| l.name == name).unwrap();
    let extra = layer("meta-extra");
    assert!(!extra.enabled);
    assert_eq!(extra.role, "not enabled");
    assert_eq!(extra.collections, ["extra"]);
    assert_eq!(extra.priority, Some(8));
    assert_eq!(extra.compat, ["scarthgap"]);
    assert_eq!(extra.depends, ["core"]);
    assert_eq!(extra.repository.as_deref(), Some("."));
    assert!(extra.files.is_none());
    let sub_layer = layer("meta-sub");
    assert_eq!(sub_layer.path, "layers/meta-sub");
    assert_eq!(sub_layer.repository.as_deref(), Some("layers/meta-sub"));
    assert!(
        a.inventory.layers.iter().all(|l| l.name != "meta-junk"),
        "build output was searched"
    );
    assert!(md.contains("present but not enabled"), "{md}");
    assert!(
        a.releases
            .layers
            .iter()
            .any(|r| r.layer == "meta-extra" && !r.claims_workspace)
    );

    let repo = |p: &str| {
        a.inventory
            .repositories
            .iter()
            .find(|r| r.path == p)
            .unwrap()
    };
    let top = repo(".");
    assert_eq!(
        top.remote.as_deref(),
        Some("https://example.com/org/build.git")
    );
    assert_eq!(top.revision.as_ref().map(String::len), Some(40));
    assert_eq!(top.branch.as_deref(), Some("main"));
    assert!(top.dirty.unwrap() >= 1, "{top:?}");
    assert!(top.layers.contains(&"meta-mine".to_string()));
    assert!(top.layers.contains(&"meta-extra".to_string()));
    let s = repo("layers/meta-sub");
    assert_eq!(s.submodule.as_deref(), Some(".: meta-sub"));
    assert_eq!(
        s.remote.as_deref(),
        Some("https://example.com/meta-sub.git")
    );
    assert_eq!(s.dirty, Some(0));
    assert!(
        a.inventory
            .notes
            .iter()
            .any(|n| n.contains("submodule missing") && n.contains("not checked out")),
        "{:?}",
        a.inventory.notes
    );
    // Credentials never reach the report.
    for text in [&md, &json] {
        assert!(!text.contains("s3cr3t"), "credentials in the report");
        assert!(!text.contains("ghp_TOKEN123"), "credentials in the report");
        assert!(!text.contains("alice"), "user name in the report");
        assert!(!text.contains(&root.display().to_string()));
    }
}

// ---------------------------------------------------------------------------------------
// kas

#[tokio::test]
async fn kas_project_pins_and_lock_file() {
    let p = kas_support::Project::after();
    let f = fixture_with(p.config());
    let a = build(&f, &p.root).await;
    let md = masked_md(&a);
    let kas = a.inventory.kas.as_ref().unwrap();
    assert_eq!(kas.files, ["kas/base.yaml", "kas/board.yaml"]);
    assert_eq!(kas.lock_files, ["kas/base.lock.yaml"]);
    assert_eq!(kas.without_lock, ["kas/board.yaml"]);
    let repo = |id: &str| {
        a.inventory
            .repositories
            .iter()
            .find(|r| r.kas_repo.as_deref().is_some_and(|k| k.contains(id)))
            .unwrap_or_else(|| panic!("no repository for kas repo {id}: {md}"))
    };
    let oe = repo("openembedded-core");
    assert_eq!(oe.path, "openembedded-core");
    assert_eq!(
        oe.pin.as_deref(),
        Some("89e6c98d92887913cadf06b2adb97f26cde4849b")
    );
    assert_eq!(oe.pin_from.as_deref(), Some("kas/base.lock.yaml"));
    assert_eq!(
        oe.remote.as_deref(),
        Some("https://git.openembedded.org/openembedded-core")
    );
    // The fixture copy is no git repository: the checkout's revision is unknown.
    assert_eq!(oe.at_pin, None);
    assert_eq!(oe.layers, ["meta"]);
    let vendor = repo("meta-vendor");
    assert_eq!(
        vendor.pin.as_deref(),
        Some("0123456789abcdef0123456789abcdef01234567")
    );
    assert_eq!(vendor.pin_from.as_deref(), Some("kas/base.yaml"));
    let bitbake = repo("bitbake");
    assert!(!bitbake.checked_out);
    assert_eq!(bitbake.pin, None);
    let own = repo("proj");
    assert_eq!(own.path, ".");
    assert_eq!(own.layers, ["meta-proj", "meta-proj-bsp"]);
    assert!(md.contains("| kas repo | kas pin |"), "{md}");
    assert!(
        md.contains("89e6c98d9288 (kas/base.lock.yaml): unknown"),
        "{md}"
    );
    assert!(md.contains("- lock files: kas/base.lock.yaml"), "{md}");
    assert_eq!(a.summary.kas_files, ["kas/base.yaml", "kas/board.yaml"]);
    insta::assert_snapshot!("audit_kas_inventory", section(&md, "## 1. Inventory"));
}

/// One `## ` section of the Markdown report.
fn section(md: &str, heading: &str) -> String {
    let at = md.find(heading).unwrap();
    let rest = &md[at + heading.len()..];
    let end = rest.find("\n## ").unwrap_or(rest.len());
    format!("{heading}{}", &rest[..end])
}

#[test]
fn kas_checkout_off_its_pin_is_flagged() {
    assert!(audit::at_pin(
        "89e6c98d92887913cadf06b2adb97f26cde4849b",
        "89e6c98d92887913cadf06b2adb97f26cde4849b"
    ));
    assert!(!audit::at_pin(
        "33a6bd6c00000000000000000000000000000000",
        "89e6c98d92887913cadf06b2adb97f26cde4849b"
    ));
}

#[test]
fn credentials_are_stripped_from_remotes() {
    assert_eq!(
        audit::strip_credentials("https://user:token@host.example/org/x.git"),
        "https://host.example/org/x.git"
    );
    assert_eq!(
        audit::strip_credentials("git@host.example:org/x.git"),
        "git@host.example:org/x.git"
    );
}

// ---------------------------------------------------------------------------------------
// The MCP tool

fn slice_args(h: &str, page: u32) -> slice::Args {
    slice::Args {
        handle: h.to_string(),
        lines: None,
        grep: None,
        context: None,
        var: None,
        failures: None,
        page: Some(page),
        budget_tokens: None,
        format: Some(Format::Json),
    }
}

#[tokio::test]
async fn tool_digest_fits_the_budget_and_slice_returns_the_report() {
    let f = fixture_with(common::fixture_config());
    let out = audit_tool::run(
        &f.ctx,
        audit_tool::Args {
            root: None,
            budget_tokens: Some(600),
            format: None,
        },
    )
    .await
    .unwrap();
    assert!(render::estimate_tokens(&out) <= 600, "{out}");
    assert!(out.starts_with("field\tvalue\nsummary\t"), "{out}");
    assert!(out.contains("\n4 lint\t"), "{out}");
    assert!(out.contains("\nattention 1\t"), "{out}");
    let hint = out
        .lines()
        .find(|l| l.starts_with("slice handle=h:"))
        .unwrap();
    let h = hint
        .trim_start_matches("slice handle=")
        .split_whitespace()
        .next()
        .unwrap()
        .to_string();

    // Page through the handle and rebuild the report.
    let stored = String::from_utf8(f.ctx.handles.get(&Handle(h.clone())).unwrap()).unwrap();
    let mut lines: Vec<String> = Vec::new();
    for page in 1.. {
        let json = slice::run(&f.ctx, slice_args(&h, page)).await.unwrap();
        let t: JsonTable = serde_json::from_str(&json).unwrap();
        for row in &t.rows {
            lines.push(render::unescape_cell(&row[1]));
        }
        if !t.notes.iter().any(|n| n.contains("; next: slice")) {
            break;
        }
    }
    assert_eq!(lines.join("\n"), stored.trim_end_matches('\n'));
    // It is the report: the same as a fresh build apart from the Generated line.
    let fresh = audit::render_markdown(
        &audit::build(&f.ctx, &Options::new(&common::fixture_ws_root()))
            .await
            .unwrap(),
    );
    assert_eq!(mask_md(&stored), mask_md(&fresh));

    // JSON digest: the same rows.
    let json = audit_tool::run(
        &f.ctx,
        audit_tool::Args {
            root: None,
            budget_tokens: Some(600),
            format: Some(Format::Json),
        },
    )
    .await
    .unwrap();
    let t: JsonTable = serde_json::from_str(&json).unwrap();
    assert_eq!(t.header, ["field", "value"]);
    assert!(t.notes.iter().any(|n| n.starts_with("slice handle=h:")));
}

#[tokio::test]
async fn tool_digest_keeps_the_hint_on_a_tiny_budget() {
    let f = fixture_with(common::fixture_config());
    let out = audit_tool::run(
        &f.ctx,
        audit_tool::Args {
            root: None,
            budget_tokens: Some(150),
            format: None,
        },
    )
    .await
    .unwrap();
    assert!(out.contains("slice handle=h:"), "{out}");
    assert!(out.contains("more over budget_tokens"), "{out}");
}

#[tokio::test]
async fn tool_root_stays_inside_the_workspace() {
    let f = fixture_with(common::fixture_config());
    let ws_root = fs::canonicalize(common::fixture_ws_root()).unwrap();
    assert_eq!(audit_tool::resolve_root(&f.ctx, None).unwrap(), ws_root);
    assert_eq!(
        audit_tool::resolve_root(&f.ctx, Some("meta-mine")).unwrap(),
        ws_root.join("meta-mine")
    );
    for bad in ["..", "/", "/etc", "../../..", "no-such-dir"] {
        let e = audit_tool::resolve_root(&f.ctx, Some(bad));
        assert!(e.is_err(), "{bad} accepted: {e:?}");
    }
    assert_eq!(
        audit_tool::workspace_span(&f.ctx.workspace),
        Some(ws_root.clone())
    );
}

// ---------------------------------------------------------------------------------------
// The index queries the report added

#[test]
fn queries_unknown_statements_unresolved_inherits_and_anonymous_python() {
    let (_tmp, root) = common::temp_ws();
    write(
        &root.join("meta-mine/recipes-app/odd/odd_1.0.bb"),
        "SUMMARY = \"odd\"\nthis is not bitbake\ninherit nosuchclass\ninherit ${DYN_CLASS}\npython () {\n    pass\n}\n",
    );
    let cfg = common::ws_config(&root);
    let ws = Workspace::discover(&cfg).unwrap();
    let cache = tempfile::tempdir().unwrap();
    let mut index = Index::open_in(cache.path(), &ws).unwrap();
    index.refresh(&ws).unwrap();

    let unknown: Vec<String> = query::unknown_statements(&index)
        .unwrap()
        .iter()
        .map(|s| format!("{} {}", s.loc, s.snippet))
        .collect();
    assert_eq!(
        unknown,
        ["meta-mine:recipes-app/odd/odd_1.0.bb:2 this is not bitbake"]
    );

    let inherits: Vec<String> = query::unresolved_inherit_edges(&index)
        .unwrap()
        .iter()
        .map(|e| format!("{} {}", e.via(), e.target_text))
        .collect();
    assert_eq!(
        inherits,
        [
            "meta-mine:recipes-app/odd/odd_1.0.bb:3 nosuchclass",
            "meta-mine:recipes-app/odd/odd_1.0.bb:4 ${DYN_CLASS}"
        ]
    );

    let anon: Vec<String> = query::anonymous_python_files(&index)
        .unwrap()
        .iter()
        .map(|f| format!("{}:{}", f.layer, f.path.display()))
        .collect();
    assert!(
        anon.contains(&"meta-mine:recipes-app/odd/odd_1.0.bb".to_string()),
        "{anon:?}"
    );
    // The fixture's own anonymous python is still there, and nothing else is listed.
    let fixture_anon = {
        let ws = common::fixture_ws();
        let cache = tempfile::tempdir().unwrap();
        let mut index = Index::open_in(cache.path(), &ws).unwrap();
        index.refresh(&ws).unwrap();
        query::anonymous_python_files(&index).unwrap().len()
    };
    assert_eq!(anon.len(), fixture_anon + 1);
}

// ---------------------------------------------------------------------------------------
// The CLI

#[test]
fn cli_writes_the_report_and_exits_0() {
    let tmp = tempfile::tempdir().unwrap();
    let out = tmp.path().join("report.json");
    let cache = tmp.path().join("cache");
    let status = Command::new(env!("CARGO_BIN_EXE_sous-chef"))
        .args(["audit", "--format", "json", "--output"])
        .arg(&out)
        .arg(common::fixture_ws_root())
        .env("XDG_CACHE_HOME", &cache)
        .current_dir(tmp.path())
        .output()
        .unwrap();
    assert_eq!(status.status.code(), Some(0), "{status:?}");
    // Progress goes to stderr, the report to the file; stdout stays empty.
    assert!(status.stdout.is_empty());
    assert!(String::from_utf8_lossy(&status.stderr).contains("sous-chef audit: linting"));
    let v: serde_json::Value = serde_json::from_str(&fs::read_to_string(&out).unwrap()).unwrap();
    assert_eq!(v["schema"], audit::SCHEMA);
    assert_eq!(v["summary"]["release"], "wrynose");

    // A root that does not exist: no report, exit 2.
    let bad = Command::new(env!("CARGO_BIN_EXE_sous-chef"))
        .args(["audit", "no-such-root"])
        .env("XDG_CACHE_HOME", &cache)
        .current_dir(tmp.path())
        .output()
        .unwrap();
    assert_eq!(bad.status.code(), Some(2), "{bad:?}");
}

// ---------------------------------------------------------------------------------------
// The live workspace (docs/live-environment.md); skipped unless SOUS_CHEF_LIVE is set.

#[tokio::test]
#[ignore = "needs SOUS_CHEF_LIVE (docs/live-environment.md)"]
async fn live_workspace_audit() {
    let Some(live) = std::env::var_os("SOUS_CHEF_LIVE").map(PathBuf::from) else {
        eprintln!("SOUS_CHEF_LIVE not set; skipped");
        return;
    };
    let root = live.join("yocto-kiss");
    let cfg = Config::load(&sous_chef::config::CliOverrides {
        config: Some(live.join("scratch/sous-chef.toml")),
        work_dir: root.clone(),
        ..Default::default()
    })
    .unwrap();
    let f = fixture_with(cfg);
    let a = build(&f, &root).await;
    let md = audit::render_markdown(&a);
    let enabled: Vec<&str> = a
        .inventory
        .layers
        .iter()
        .filter(|l| l.enabled)
        .map(|l| l.name.as_str())
        .collect();
    // In BBLAYERS order, as kas writes it (prio, then repository name).
    assert_eq!(
        enabled,
        [
            "meta-arm",
            "meta-arm-toolchain",
            "meta-kiss",
            "meta-kiss-ti",
            "meta"
        ],
        "{md}"
    );
    let not_enabled: Vec<&str> = a
        .inventory
        .layers
        .iter()
        .filter(|l| !l.enabled)
        .map(|l| l.path.as_str())
        .collect();
    for p in [
        "meta-arm/meta-arm-bsp",
        "meta-kiss-amd",
        "meta-kiss-nxp",
        "meta-kiss-st",
        "openembedded-core/meta-selftest",
        "openembedded-core/meta-skeleton",
    ] {
        assert!(not_enabled.contains(&p), "{p} not listed: {not_enabled:?}");
    }
    assert!(
        not_enabled.iter().all(|p| !p.starts_with("bitbake/")),
        "{not_enabled:?}"
    );
    assert_eq!(a.summary.release, "wrynose");
    assert!(a.lint.files > 1000, "{}", a.lint.files);
    assert!(!md.contains(&live.display().to_string()));
}
