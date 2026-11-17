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
    pub layers: Vec<LayerStatus>,
    /// Files on disk by kind (`bb`, `bbappend`, `inc`, `bbclass`, `conf`), across the layers
    /// and `build/conf`.
    pub files: BTreeMap<String, u32>,
    pub index: IndexStatus,
    pub bitbake: BitbakeStatus,
    /// The running background job, if any.
    pub job: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct LayerStatus {
    pub name: String,
    pub root: String,
    pub priority: i32,
    pub writable: bool,
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
    // The job table has no query API yet (T5.4); until then no job is ever reported.
    report.job = None;

    output(&report, freshness, &args, ctx.config.output.budget_tokens)
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
        for key in ["layers", "files", "index", "bitbake", "job", "work_dir"] {
            assert!(v.get(key).is_some(), "{key}");
        }
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
        ] {
            assert!(text.contains(field), "{field}");
        }
    }
}
