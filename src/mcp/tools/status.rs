//! Tool `status`: workspace, index and bitbake environment status. See docs/mcp-tools.md.
//!
//! `status` never fails because the workspace is broken: problems (configuration, index
//! refresh) are reported as fields. When the server could not build its context at all,
//! `mcp::Server` calls [`degraded`] instead of [`run`]; the output has the same shape.

use std::collections::BTreeMap;
use std::path::Path;
use std::time::SystemTime;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::bitbake::environment_state;
use crate::config::Config;
use crate::error::{Error, Result};
use crate::mcp::{Ctx, index_path};
use crate::render::{self, Format, Table};
use crate::types::FileKind;
use crate::workspace::Workspace;

#[derive(Debug, Clone, Deserialize, JsonSchema)]
pub struct Args {
    /// Output budget in tokens (default 600).
    pub budget_tokens: Option<usize>,
    /// `text` (default) or `json`.
    pub format: Option<Format>,
}

/// The status report. `format: "json"` returns it serialised; text renders it as a
/// two-column `field value` table.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct Report {
    /// sous-chef version.
    pub version: String,
    /// `ready`, or `degraded` when the workspace could not be loaded at startup.
    pub state: String,
    /// Why the server is degraded.
    pub startup_error: Option<String>,
    /// Where discovery started.
    pub work_dir: String,
    pub build_dir: Option<String>,
    /// `Config::validate` failure, if any.
    pub config_problem: Option<String>,
    /// The workspace release and where it came from (`ReleaseInfo::describe`), for example
    /// `wrynose (LAYERSERIES_CORENAMES)`. `None` when the workspace could not be loaded.
    pub release: Option<String>,
    pub layers: Vec<LayerStatus>,
    /// The kas project, when the workspace is one (left out of the JSON otherwise).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub kas: Option<KasStatus>,
    /// Files on disk by kind (`bb`, `bbappend`, `inc`, `bbclass`, `conf`), across the layers
    /// and `build/conf`.
    pub files: BTreeMap<String, u32>,
    pub index: IndexStatus,
    pub bitbake: BitbakeStatus,
    /// The running background job, if any, as `<id> <op> [<target>] running <elapsed>s`
    /// ([`job_line`]).
    pub job: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct LayerStatus {
    pub name: String,
    pub root: String,
    pub priority: i32,
    pub writable: bool,
}

/// What `status` shows about a kas project (`workspace::kas::KasProject`).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct KasStatus {
    /// The kas files as kas takes them (`kas/a.yaml:kas/b.yaml`, relative to the kas work
    /// directory); empty when they are not known.
    pub files: String,
    /// How they were found (`KasSource::describe`).
    pub source: String,
    pub work_dir: String,
    pub build_dir: String,
    /// `machine`, `distro` and targets, kas's defaults when the files set none.
    pub machine: String,
    pub distro: String,
    pub target: Vec<String>,
    /// Layers the kas files name, and those not on disk yet (paths).
    pub layers: usize,
    pub missing: Vec<String>,
    /// In the kas files but not in `bblayers.conf` (paths), and the reverse (layer names).
    pub not_in_bblayers: Vec<String>,
    pub not_in_kas: Vec<String>,
    pub lock_files: Vec<String>,
    pub problems: Vec<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct IndexStatus {
    pub path: Option<String>,
    /// Seconds since the database file was last written; `None` if it does not exist.
    pub age_secs: Option<u64>,
    /// Files seen by the refresh this call made.
    pub scanned: Option<u32>,
    pub reindexed: Option<usize>,
    pub removed: Option<u32>,
    pub refresh_millis: Option<u64>,
    /// Refresh failure, if any.
    pub error: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct BitbakeStatus {
    /// `[bitbake] enabled` in the configuration.
    pub enabled: bool,
    /// Whether a usable build environment was found.
    pub environment: bool,
    pub env_build_dir: Option<String>,
    pub init_script: Option<String>,
    /// `init-script` or `initialised` (already set up when the server started).
    pub env_mode: Option<String>,
    /// Why no environment was found.
    pub env_problem: Option<String>,
}

pub async fn run(ctx: &Ctx, args: Args) -> Result<String> {
    let mut report = base(&ctx.config);
    report.state = "ready".into();
    report.config_problem = ctx
        .config
        .validate(&ctx.workspace)
        .err()
        .map(|e| e.to_string());
    fill_workspace(&mut report, &ctx.workspace);

    let path = index_path(&ctx.workspace);
    let mut freshness = None;
    {
        let mut index = ctx.index.lock().await;
        match index.refresh(&ctx.workspace) {
            Ok(r) => {
                report.index.scanned = Some(r.scanned);
                report.index.reindexed = Some(r.reindexed.len());
                report.index.removed = Some(r.removed);
                report.index.refresh_millis = Some(r.millis);
                freshness = render::freshness_line(&r);
                // The index is fresh: its counts replace the disk walk done above, which
                // stays as the answer when the refresh fails.
                if let Ok(stats) = crate::index::query::layer_stats(&index) {
                    report.files = indexed_counts(&stats);
                }
            }
            Err(e) => report.index.error = Some(e.to_string()),
        }
    }
    report.index.age_secs = age_secs(&path);
    report.index.path = Some(path.display().to_string());

    let state = environment_state(&ctx.config, &ctx.workspace);
    report.bitbake.env_problem = state.problem;
    if let Some(env) = state.env {
        report.bitbake.environment = true;
        report.bitbake.env_mode = Some(env.mode().to_string());
        report.bitbake.env_build_dir = Some(env.build_dir.display().to_string());
        report.bitbake.init_script = env.init_script.map(|p| p.display().to_string());
    }
    report.job = ctx.jobs.running().map(|j| job_line(&j));

    output(&report, freshness, &args, ctx.config.output.budget_tokens)
}

/// The running job as one line: `job:3 build core-image-minimal running 42s`.
pub fn job_line(j: &crate::bitbake::JobInfo) -> String {
    let mut parts = vec![j.id.clone(), j.op.clone()];
    parts.extend(j.target.clone());
    parts.push(j.state.as_str().to_string());
    parts.push(format!("{}s", j.elapsed_secs));
    parts.join(" ")
}

/// `status` when the server has no context: same shape, with `startup_error` set.
pub fn degraded(config: &Config, startup_error: &str, args: Args) -> Result<String> {
    let mut report = base(config);
    report.state = "degraded".into();
    report.startup_error = Some(startup_error.to_string());
    output(&report, None, &args, config.output.budget_tokens)
}

fn base(config: &Config) -> Report {
    Report {
        version: env!("CARGO_PKG_VERSION").into(),
        work_dir: config.work_dir.display().to_string(),
        build_dir: config.build_dir.as_ref().map(|p| p.display().to_string()),
        bitbake: BitbakeStatus {
            enabled: config.bitbake.enabled,
            init_script: config.init_script.as_ref().map(|p| p.display().to_string()),
            ..BitbakeStatus::default()
        },
        ..Report::default()
    }
}

fn fill_workspace(report: &mut Report, ws: &Workspace) {
    if let Some(b) = &ws.build_dir {
        report.build_dir = Some(b.display().to_string());
    }
    report.release = Some(ws.release.describe());
    report.layers = ws
        .layers
        .iter()
        .map(|l| LayerStatus {
            name: l.name.clone(),
            root: l.root.display().to_string(),
            priority: l.priority,
            writable: l.writable,
        })
        .collect();
    report.files = count_files(ws);
    report.kas = ws.kas.as_ref().map(kas_status);
}

fn kas_status(k: &crate::workspace::kas::KasProject) -> KasStatus {
    let show = |p: &Path| {
        p.strip_prefix(&k.work_dir)
            .unwrap_or(p)
            .display()
            .to_string()
    };
    KasStatus {
        files: k.files_arg(&k.work_dir),
        source: k.source.describe(),
        work_dir: k.work_dir.display().to_string(),
        build_dir: k.build_dir.display().to_string(),
        machine: k.machine_or_default().to_string(),
        distro: k.distro_or_default().to_string(),
        target: k
            .targets_or_default()
            .into_iter()
            .map(str::to_string)
            .collect(),
        layers: k.layers.len(),
        missing: k
            .layers
            .iter()
            .filter(|l| !l.present)
            .map(|l| show(&l.path))
            .collect(),
        not_in_bblayers: k.not_in_bblayers.iter().map(|p| show(p)).collect(),
        not_in_kas: k.not_in_kas.clone(),
        lock_files: k.lock_files.iter().map(|p| show(p)).collect(),
        problems: k.problems.clone(),
    }
}

fn output(
    report: &Report,
    freshness: Option<String>,
    args: &Args,
    default_budget: usize,
) -> Result<String> {
    if args.format == Some(Format::Json) {
        return serde_json::to_string(report).map_err(|e| Error::Invalid {
            msg: format!("serialising the status report: {e}"),
        });
    }
    let budget = args.budget_tokens.unwrap_or(default_budget);
    let mut out = table(report).render(budget);
    if let Some(line) = freshness {
        if !out.is_empty() && !out.ends_with('\n') {
            out.push('\n');
        }
        out.push_str(&line);
    }
    Ok(out)
}

fn table(r: &Report) -> Table {
    let none = || "none".to_string();
    let mut rows: Vec<(&str, String)> =
        vec![("version", r.version.clone()), ("state", r.state.clone())];
    if let Some(e) = &r.startup_error {
        rows.push(("startup_error", e.clone()));
    }
    rows.push(("work_dir", r.work_dir.clone()));
    rows.push(("build_dir", r.build_dir.clone().unwrap_or_else(none)));
    rows.push((
        "config",
        r.config_problem.clone().unwrap_or_else(|| "ok".into()),
    ));
    rows.push(("release", r.release.clone().unwrap_or_else(none)));
    rows.push(("layers", r.layers.len().to_string()));
    for l in &r.layers {
        rows.push((
            "layer",
            format!(
                "{} priority={} writable={} {}",
                l.name, l.priority, l.writable, l.root
            ),
        ));
    }
    if let Some(k) = &r.kas {
        let files = if k.files.is_empty() {
            "files not known".to_string()
        } else {
            k.files.clone()
        };
        rows.push(("kas", format!("{files} ({})", k.source)));
        if !k.files.is_empty() {
            rows.push((
                "kas_defaults",
                format!(
                    "machine {}, distro {}, target {}",
                    k.machine,
                    k.distro,
                    k.target.join(" ")
                ),
            ));
            let mut layers = format!("{} in the kas files", k.layers);
            if !k.missing.is_empty() {
                layers.push_str(&format!("; not checked out yet: {}", k.missing.join(", ")));
            }
            rows.push(("kas_layers", layers));
        }
        if !k.lock_files.is_empty() {
            rows.push(("kas_lock", k.lock_files.join(", ")));
        }
        if !k.not_in_bblayers.is_empty() || !k.not_in_kas.is_empty() {
            let mut parts = Vec::new();
            if !k.not_in_bblayers.is_empty() {
                parts.push(format!(
                    "in the kas files but not in bblayers.conf: {}",
                    k.not_in_bblayers.join(", ")
                ));
            }
            if !k.not_in_kas.is_empty() {
                parts.push(format!(
                    "in bblayers.conf but not in the kas files: {}",
                    k.not_in_kas.join(", ")
                ));
            }
            rows.push((
                "kas_mismatch",
                format!(
                    "{}; bblayers.conf is used until kas checkout regenerates it",
                    parts.join("; ")
                ),
            ));
        }
        for p in &k.problems {
            rows.push(("kas_problem", p.clone()));
        }
    }
    let files = if r.files.is_empty() {
        none()
    } else {
        r.files
            .iter()
            .map(|(k, n)| format!("{k}={n}"))
            .collect::<Vec<_>>()
            .join(" ")
    };
    rows.push(("files", files));
    rows.push(("index", r.index.path.clone().unwrap_or_else(none)));
    rows.push((
        "index_age",
        r.index
            .age_secs
            .map(|s| format!("{s}s"))
            .unwrap_or_else(|| "missing".into()),
    ));
    let refresh = match (&r.index.error, r.index.scanned) {
        (Some(e), _) => format!("failed: {e}"),
        (None, Some(scanned)) => format!(
            "scanned {scanned}, reindexed {}, removed {} in {} ms",
            r.index.reindexed.unwrap_or(0),
            r.index.removed.unwrap_or(0),
            r.index.refresh_millis.unwrap_or(0)
        ),
        (None, None) => "not run".into(),
    };
    rows.push(("refresh", refresh));
    let bitbake = match (r.bitbake.enabled, &r.bitbake.env_build_dir) {
        (false, _) => "disabled in sous-chef.toml".to_string(),
        (true, Some(dir)) => match (&r.bitbake.env_mode, &r.bitbake.init_script) {
            (Some(mode), Some(script)) => {
                format!("build environment found ({mode}): {dir}, init {script}")
            }
            (Some(mode), None) => format!("build environment found ({mode}): {dir}"),
            _ => format!("build environment found: {dir}"),
        },
        (true, None) => match &r.bitbake.env_problem {
            Some(p) => format!("no build environment found: {p}"),
            None => "no build environment found".to_string(),
        },
    };
    rows.push(("bitbake", bitbake));
    rows.push(("job", r.job.clone().unwrap_or_else(none)));

    let mut t = Table::new(&["field", "value"]);
    for (k, v) in &rows {
        t.row(&[k, v]);
    }
    t
}

/// Seconds since `path` was modified, or `None` if it cannot be read.
fn age_secs(path: &Path) -> Option<u64> {
    let modified = std::fs::metadata(path).ok()?.modified().ok()?;
    Some(
        SystemTime::now()
            .duration_since(modified)
            .map(|d| d.as_secs())
            .unwrap_or(0),
    )
}

/// File counts by kind over all indexed layers (the build pseudo-layer included); kinds
/// with no files are left out, as in [`count_files`].
fn indexed_counts(stats: &[crate::index::query::LayerStats]) -> BTreeMap<String, u32> {
    let mut counts = BTreeMap::new();
    for s in stats {
        for (kind, n) in [
            (FileKind::Recipe, s.recipes),
            (FileKind::Append, s.appends),
            (FileKind::Include, s.includes),
            (FileKind::Class, s.classes),
            (FileKind::Conf, s.confs),
        ] {
            if n > 0 {
                *counts.entry(kind.as_str().to_string()).or_insert(0) += n;
            }
        }
    }
    counts
}

/// Counts BitBake files by kind under every layer root (not descending into another layer
/// nested inside it, or into hidden and git-ignored directories) and in `build/conf`.
fn count_files(ws: &Workspace) -> BTreeMap<String, u32> {
    let mut counts = BTreeMap::new();
    let mut bump = |p: &Path| {
        if let Some(kind) = FileKind::from_path(p) {
            *counts.entry(kind.as_str().to_string()).or_insert(0) += 1;
        }
    };
    let roots: Vec<&Path> = ws.layers.iter().map(|l| l.root.as_path()).collect();
    for layer in &ws.layers {
        let own = layer.root.clone();
        let others: Vec<std::path::PathBuf> = roots
            .iter()
            .filter(|r| **r != own.as_path())
            .map(|r| r.to_path_buf())
            .chain(ws.build_dir.clone())
            .collect();
        let walk = ignore::WalkBuilder::new(&layer.root)
            .filter_entry(move |e| !others.iter().any(|o| o.as_path() == e.path()))
            .build();
        for entry in walk.flatten() {
            if entry.file_type().is_some_and(|t| t.is_file()) {
                bump(entry.path());
            }
        }
    }
    if let Some(build) = &ws.build_dir
        && let Ok(rd) = std::fs::read_dir(build.join("conf"))
    {
        for entry in rd.flatten() {
            let p = entry.path();
            if p.is_file() && p.extension().is_some_and(|e| e == "conf") {
                bump(&p);
            }
        }
    }
    counts
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::workspace::Layer;
    use std::path::PathBuf;

    fn fixture_ws() -> Workspace {
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/ws");
        let layer = |name: &str, priority| Layer {
            name: name.into(),
            root: root.join(name),
            collections: vec![],
            priority,
            compat: vec![],
            depends: vec![],
            bbfiles: vec![],
            writable: name == "meta-mine",
        };
        Workspace {
            build_dir: Some(root.join("build")),
            layers: vec![
                layer("meta-core", 5),
                layer("meta-vendor", 6),
                layer("meta-mine", 10),
            ],
            ..Workspace::default()
        }
    }

    #[test]
    fn counts_fixture_files_by_kind() {
        let counts = count_files(&fixture_ws());
        assert!(counts.get("bb").copied().unwrap_or(0) > 0, "{counts:?}");
        assert!(counts.get("conf").copied().unwrap_or(0) >= 5, "{counts:?}");
        assert!(counts.keys().all(|k| k.parse::<FileKind>().is_ok()));
    }

    #[test]
    fn degraded_json_has_the_documented_shape() {
        let args = Args {
            budget_tokens: None,
            format: Some(Format::Json),
        };
        let out = degraded(&Config::default(), "no layers", args).unwrap();
        let v: serde_json::Value = serde_json::from_str(&out).unwrap();
        assert_eq!(v["state"], "degraded");
        assert_eq!(v["startup_error"], "no layers");
        for key in [
            "layers", "files", "index", "bitbake", "job", "work_dir", "release",
        ] {
            assert!(v.get(key).is_some(), "{key}");
        }
        assert!(v["release"].is_null());
    }

    #[test]
    fn table_has_one_row_per_field() {
        let mut r = base(&Config::default());
        r.state = "ready".into();
        fill_workspace(&mut r, &fixture_ws());
        // Table internals are private to render.rs (a stub until T2.4); check via Debug.
        let text = format!("{:?}", table(&r));
        for field in [
            "version",
            "layers",
            "meta-mine",
            "files",
            "refresh",
            "bitbake",
            "job",
            "release",
            "wrynose (assumed)",
        ] {
            assert!(text.contains(field), "{field}");
        }
    }

    fn copy_dir(from: &Path, to: &Path) {
        std::fs::create_dir_all(to).unwrap();
        for e in std::fs::read_dir(from).unwrap() {
            let e = e.unwrap();
            let dest = to.join(e.file_name());
            if e.file_type().unwrap().is_dir() {
                copy_dir(&e.path(), &dest);
            } else {
                std::fs::copy(e.path(), dest).unwrap();
            }
        }
    }

    #[test]
    fn kas_rows_and_json_field() {
        let fixtures = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/kas");
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("project");
        copy_dir(&fixtures.join("project"), &root);
        std::fs::create_dir_all(root.join(".git")).unwrap();
        let root = root.canonicalize().unwrap();
        let cfg = Config {
            kas: vec![root.join("kas/base.yaml"), root.join("kas/board.yaml")],
            work_dir: root.clone(),
            ..Config::default()
        };
        let ws = Workspace::discover_in(&cfg, &crate::workspace::kas::KasEnv::default()).unwrap();
        let mut r = base(&cfg);
        fill_workspace(&mut r, &ws);
        let args = Args {
            budget_tokens: Some(5000),
            format: None,
        };
        let text = output(&r, None, &args, 600).unwrap();
        let row = |name: &str| {
            text.lines()
                .find(|l| l.split_whitespace().next() == Some(name))
                .unwrap_or_else(|| panic!("no {name} row in {text}"))
                .to_string()
        };
        assert!(
            row("kas").contains("kas/base.yaml:kas/board.yaml (from kas in sous-chef.toml)"),
            "{text}"
        );
        assert!(
            row("kas_defaults").contains("machine board1, distro projdistro, target proj-image"),
            "{text}"
        );
        assert!(
            row("kas_layers").contains(
                "4 in the kas files; not checked out yet: meta-vendor, openembedded-core/meta"
            ),
            "{text}"
        );
        assert!(row("kas_lock").contains("kas/base.lock.yaml"), "{text}");
        let json = Args {
            budget_tokens: None,
            format: Some(Format::Json),
        };
        let v: serde_json::Value =
            serde_json::from_str(&output(&r, None, &json, 600).unwrap()).unwrap();
        assert_eq!(v["kas"]["machine"], "board1");
        assert_eq!(v["kas"]["files"], "kas/base.yaml:kas/board.yaml");

        // Without kas: no rows, no JSON key.
        let mut r = base(&Config::default());
        fill_workspace(&mut r, &fixture_ws());
        let text = output(&r, None, &args, 600).unwrap();
        assert!(!text.lines().any(|l| l.starts_with("kas")), "{text}");
        let v: serde_json::Value =
            serde_json::from_str(&output(&r, None, &json, 600).unwrap()).unwrap();
        assert!(v.get("kas").is_none());
    }

    #[test]
    fn release_row_and_json_field() {
        let mut ws = fixture_ws();
        ws.release = crate::release::ReleaseInfo {
            release: crate::release::Release::Scarthgap,
            source: crate::release::ReleaseSource::CoreNames,
            note: None,
        };
        let mut r = base(&Config::default());
        fill_workspace(&mut r, &ws);
        assert_eq!(
            r.release.as_deref(),
            Some("scarthgap (LAYERSERIES_CORENAMES)")
        );
        let args = Args {
            budget_tokens: None,
            format: Some(Format::Json),
        };
        let v: serde_json::Value =
            serde_json::from_str(&output(&r, None, &args, 600).unwrap()).unwrap();
        assert_eq!(v["release"], "scarthgap (LAYERSERIES_CORENAMES)");
        let args = Args {
            budget_tokens: None,
            format: None,
        };
        let text = output(&r, None, &args, 600).unwrap();
        let line = text.lines().find(|l| l.starts_with("release")).unwrap();
        assert!(line.contains("scarthgap (LAYERSERIES_CORENAMES)"), "{text}");
    }
}
