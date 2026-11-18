//! Tool `pkgdata`: which package ships a file, and which recipe made a package, from the
//! package data an existing build left (`PKGDATA_DIR`). See docs/mcp-tools.md.
//!
//! Reads `bitbake::pkgdata`; never builds. Every answer names the recipe with a pointer to
//! its `.bb` in the layers (and, for `find_path`, the `FILES:<pkg>` assignment), so "this
//! file on the target" leads to the recipe in one step.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use schemars::JsonSchema;
use serde::Deserialize;

use crate::bitbake::pkgdata::{self, Found, Package, PkgData};
use crate::bitbake::{BbEnv, logs};
use crate::error::{Error, Result};
use crate::eval::{self, DataStore, OpStatus, Target};
use crate::handles::HandleKind;
use crate::index::Index;
use crate::index::query;
use crate::mcp::Ctx;
use crate::mcp::tools::recipe;
use crate::render::{self, Format, Table};

/// Rows shown before the rest goes under a handle.
const LIMIT: usize = 40;

/// Recipes evaluated for `FILES` pointers in one `find_path` call.
const MAX_EVALUATED: usize = 8;

/// What to look up.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Mode {
    /// Which package ships a file (`path`, a glob as `oe-pkgdata-util find-path` takes).
    #[default]
    #[serde(alias = "find-path")]
    FindPath,
    /// A package's files with sizes (`package`).
    #[serde(alias = "list-pkg-files")]
    ListPkgFiles,
    /// The recipe that produced a package (`package`).
    #[serde(alias = "lookup-recipe")]
    LookupRecipe,
    /// A recipe's packages (`recipe`).
    #[serde(alias = "list-pkgs")]
    ListPkgs,
    /// Version, size, runtime dependencies and provides, runtime name (`package`).
    #[serde(alias = "package-info")]
    PackageInfo,
}

#[derive(Debug, Clone, Deserialize, JsonSchema)]
pub struct Args {
    /// Default `find_path`.
    #[serde(default)]
    pub mode: Mode,
    /// For `find_path`: a target path or glob, e.g. `*/libssl.so*`.
    pub path: Option<String>,
    /// For `list_pkg_files`, `lookup_recipe`, `package_info`: a package name.
    pub package: Option<String>,
    /// For `list_pkgs`: a recipe (`pn`).
    pub recipe: Option<String>,
    /// Output budget in tokens (default 600).
    pub budget_tokens: Option<usize>,
    /// `text` (default) or `json`.
    pub format: Option<Format>,
}

pub async fn run(ctx: &Ctx, args: Args) -> Result<String> {
    let budget = match args
        .budget_tokens
        .unwrap_or(ctx.config.output.budget_tokens)
    {
        0 => render::DEFAULT_BUDGET_TOKENS,
        n => n,
    };
    let format = args.format.unwrap_or_default();
    let mut index = ctx.index.lock().await;
    let report = index.refresh(&ctx.workspace)?;
    let mut notes: Vec<String> = Vec::new();
    // The configuration's values, read before anything is awaited (the index is not Sync).
    let (machine, value) = {
        let ds = eval::evaluate(&index, &ctx.workspace, &Target::default()).ok();
        (
            ds.as_ref()
                .and_then(|d| d.value("MACHINE"))
                .map(|v| v.text)
                .filter(|m| !m.is_empty() && !m.contains("${")),
            ds.as_ref().and_then(|d| d.value("PKGDATA_DIR")),
        )
    };
    let located = locate(ctx, machine, value, &mut notes).await?;
    let confine = |p: &Path| ctx.workspace.confine(p);
    let mut t = match located {
        None => {
            let mut t = Table::new(&["kind", "value"]);
            t.row(&[
                "pkgdata",
                "none: nothing has been packaged in this build directory",
            ]);
            t.note("note: package data is written by do_packagedata (after do_package) of each recipe built, e.g. bitbake <recipe> or an image; sous-chef does not run it");
            t
        }
        Some(dir) => {
            let pd = PkgData::new(dir, &confine);
            match args.mode {
                Mode::FindPath => find_path(ctx, &index, &pd, &args, &mut notes)?,
                Mode::ListPkgFiles => list_pkg_files(ctx, &index, &pd, &args, &mut notes)?,
                Mode::LookupRecipe => lookup_recipe(ctx, &index, &pd, &args, &mut notes)?,
                Mode::ListPkgs => list_pkgs(ctx, &index, &pd, &args, &mut notes)?,
                Mode::PackageInfo => package_info(ctx, &index, &pd, &args, &mut notes)?,
            }
        }
    };
    for n in &notes {
        t.note(n);
    }
    if let Some(line) = render::freshness_line(&report) {
        t.note(&line);
    }
    Ok(t.render_as(format, budget))
}

/// `PKGDATA_DIR`: from the evaluated configuration when it is static and exists, else
/// `bitbake-getvar --value PKGDATA_DIR` when a build environment is available, else the
/// first `<build>/tmp*/pkgdata/<MACHINE>` (or the only machine directory). A note says
/// which. `None` when there is no package data.
async fn locate(
    ctx: &Ctx,
    machine: Option<String>,
    value: Option<crate::eval::Value>,
    notes: &mut Vec<String>,
) -> Result<Option<PathBuf>> {
    let static_value = value
        .as_ref()
        .filter(|v| !v.unevaluated_python && v.unresolved.is_empty() && v.text.starts_with('/'))
        .map(|v| v.text.trim().to_string());
    if let Some(v) = &static_value {
        let p = PathBuf::from(v);
        if p.join("runtime").is_dir() {
            let c = confined(ctx, &p)?;
            notes.push(format!(
                "note: PKGDATA_DIR = {v} (from the evaluated configuration)"
            ));
            return Ok(Some(c));
        }
    } else if ctx.config.bitbake.enabled
        && let Some(env) = BbEnv::detect(&ctx.config, &ctx.workspace)
    {
        match getvar(&env).await {
            Ok(p) if p.join("runtime").is_dir() => {
                let c = confined(ctx, &p)?;
                notes.push(format!(
                    "note: PKGDATA_DIR = {} (from bitbake-getvar; the configuration's value is not static)",
                    p.display()
                ));
                return Ok(Some(c));
            }
            Ok(p) => notes.push(format!(
                "note: bitbake-getvar gives PKGDATA_DIR = {}, which has no runtime/ directory",
                p.display()
            )),
            Err(e) => notes.push(format!("note: {e}; searched the build directory instead")),
        }
    }
    let Some(build) = &ctx.workspace.build_dir else {
        return Err(Error::NotFound {
            what: "a build directory (package data lives under <build>/tmp/pkgdata/<machine>); set build_dir in sous-chef.toml or pass --build-dir".into(),
        });
    };
    let dirs = pkgdata::find_dirs(build);
    let pick = machine
        .as_ref()
        .and_then(|m| dirs.iter().find(|(n, _)| n == m))
        .or(if dirs.len() == 1 { dirs.first() } else { None });
    match pick {
        Some((name, p)) => {
            let c = confined(ctx, p)?;
            let why = match &static_value {
                Some(v) => format!("the configuration's PKGDATA_DIR {v} has no package data"),
                None => "PKGDATA_DIR is not set, or not static, in the configuration".into(),
            };
            notes.push(format!(
                "note: package data found by searching the build directory: {} ({why}; machine {name})",
                shown(ctx, &c)
            ));
            Ok(Some(c))
        }
        None => {
            if !dirs.is_empty() {
                notes.push(format!(
                    "note: package data exists for other machines only: {}",
                    dirs.iter()
                        .map(|(n, _)| n.as_str())
                        .collect::<Vec<_>>()
                        .join(", ")
                ));
            }
            if let Some(v) = static_value {
                notes.push(format!("note: PKGDATA_DIR = {v} does not exist yet"));
            }
            Ok(None)
        }
    }
}

/// `PKGDATA_DIR` through `bitbake-getvar --value PKGDATA_DIR` (the configuration only).
async fn getvar(env: &BbEnv) -> Result<PathBuf> {
    let mut cmd = env.command("bitbake-getvar", &["--value", "PKGDATA_DIR"])?;
    cmd.kill_on_drop(true);
    let out = tokio::time::timeout(logs::GETVAR_TIMEOUT, cmd.output())
        .await
        .map_err(|_| Error::Invalid {
            msg: "bitbake-getvar PKGDATA_DIR timed out".into(),
        })??;
    if !out.status.success() {
        return Err(Error::Invalid {
            msg: "bitbake-getvar PKGDATA_DIR failed".into(),
        });
    }
    match logs::parse_getvar_value(&String::from_utf8_lossy(&out.stdout)) {
        Some(v) if v.starts_with('/') => Ok(PathBuf::from(v)),
        _ => Err(Error::Invalid {
            msg: "bitbake-getvar PKGDATA_DIR printed no absolute path".into(),
        }),
    }
}

/// The directory through `Workspace::confine`, with an error that says why when it is
/// outside (a `TMPDIR` outside the build directory: the `log` tool has the same limit).
fn confined(ctx: &Ctx, p: &Path) -> Result<PathBuf> {
    ctx.workspace.confine(p).map_err(|_| Error::Invalid {
        msg: format!(
            "PKGDATA_DIR {} is outside the workspace layers and build directory; sous-chef reads only inside them (as the log tool does), so it is not read",
            p.display()
        ),
    })
}

/// `layer:path` for a path in the workspace (`build:` for the build directory).
fn shown(ctx: &Ctx, p: &Path) -> String {
    match ctx.workspace.loc_for(p, crate::types::LineSpan::line(1)) {
        Some(l) => format!("{}:{}", l.layer, l.path.display()),
        None => p.display().to_string(),
    }
}

/// The recipe `pn` (version `pv` when the layers have it) as a pointer to its `.bb`;
/// `None` when no layer has it.
fn recipe_file(index: &Index, ctx: &Ctx, pn: &str, pv: Option<&str>) -> Option<String> {
    let rows = query::find_recipes(index, pn).ok()?;
    if let Some(v) = pv
        && let Some(r) = rows.iter().find(|r| r.pv == v)
    {
        return Some(recipe::file_label(&r.file));
    }
    recipe::resolve_selected(index, &ctx.workspace, pn, None)
        .ok()
        .map(|(r, _)| recipe::file_label(&r.file))
}

fn recipe_cell(index: &Index, ctx: &Ctx, p: &Package, missing: &mut usize) -> (String, String) {
    let pn = p.get("PN").unwrap_or("?").to_string();
    let loc = recipe_file(index, ctx, &pn, p.get("PV")).unwrap_or_else(|| {
        *missing += 1;
        "-".into()
    });
    (pn, loc)
}

fn missing_note(missing: usize, notes: &mut Vec<String>) {
    if missing > 0 {
        notes.push(format!(
            "note: {missing} {} in no layer of the workspace (no .bb pointer)",
            if missing == 1 {
                "recipe is"
            } else {
                "recipes are"
            }
        ));
    }
}

fn required<'a>(v: &'a Option<String>, what: &str, mode: &str) -> Result<&'a str> {
    v.as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| Error::Invalid {
            msg: format!("mode={mode} needs {what}"),
        })
}

/// A package name argument: letters, digits and `_ . + -` (no `/`).
fn package_name(v: &Option<String>, mode: &str) -> Result<String> {
    let s = required(v, "package", mode)?;
    logs::check_name("package", s)?;
    Ok(s.to_string())
}

/// Rows past [`LIMIT`] are stored under a handle; a note says how to read them.
fn spill(
    ctx: &Ctx,
    t: &mut Table,
    all: &[String],
    what: &str,
    notes: &mut Vec<String>,
) -> Result<()> {
    if all.len() > LIMIT {
        let text = all.join("\n");
        let h = ctx.handles.put(HandleKind::Text, what, text.as_bytes())?;
        t.limited(all.len() - LIMIT, LIMIT);
        notes.push(format!(
            "note: all {} rows are stored under {h}: slice handle={h}",
            all.len()
        ));
    }
    Ok(())
}

/// The pointers of the applied `FILES:<pkg>` operations, from the recipe's evaluated
/// datastore.
fn files_locs(ds: &DataStore, pkg: &str) -> Option<String> {
    let locs: Vec<String> = ds
        .history(&format!("FILES:{pkg}"))
        .into_iter()
        .filter(|(op, st)| *st == OpStatus::Applied && op.var.flag.is_none())
        .map(|(op, _)| op.loc.to_string())
        .collect();
    (!locs.is_empty()).then(|| locs.join(", "))
}

fn find_path(
    ctx: &Ctx,
    index: &Index,
    pd: &PkgData,
    args: &Args,
    notes: &mut Vec<String>,
) -> Result<Table> {
    let pattern = required(
        &args.path,
        "path (a file or glob on the target)",
        "find_path",
    )?;
    let re = pkgdata::glob_regex(pattern)?;
    let hits = pd.find_path(&re);
    let mut t = Table::new(&["path", "package", "recipe", "loc"]);
    if hits.is_empty() {
        notes.push(format!(
            "note: no package ships a path matching {pattern} (FILES_INFO of every runtime/ file; a file installed but in no package is not there)"
        ));
        return Ok(t);
    }
    let mut stores: HashMap<String, Option<DataStore>> = HashMap::new();
    let mut missing = 0usize;
    let mut all: Vec<String> = Vec::new();
    for (i, (p, path)) in hits.iter().enumerate() {
        let (pn, file) = recipe_cell(index, ctx, p, &mut missing);
        let mut loc = file.clone();
        if i < LIMIT && file != "-" {
            let evaluated = stores.len();
            let ds = stores.entry(pn.clone()).or_insert_with(|| {
                if evaluated >= MAX_EVALUATED {
                    return None;
                }
                recipe::resolve_selected_variant(index, &ctx.workspace, &pn, None)
                    .ok()
                    .and_then(|(r, _, v)| {
                        let target = Target {
                            recipe: Some(recipe::eval_label(&r, v.as_ref())),
                            ..Target::default()
                        };
                        eval::evaluate(index, &ctx.workspace, &target).ok()
                    })
            });
            if let Some(l) = ds.as_ref().and_then(|d| files_locs(d, &p.name)) {
                loc = format!("FILES:{} {l}", p.name);
            }
        }
        let pkg = if p.runtime_name() != p.name {
            format!("{} (runtime name {})", p.name, p.runtime_name())
        } else {
            p.name.clone()
        };
        all.push(format!("{path}\t{pkg}\t{pn}\t{loc}"));
        if i < LIMIT {
            t.row(&[path, &pkg, &pn, &loc]);
        }
    }
    spill(
        ctx,
        &mut t,
        &all,
        &format!("pkgdata find_path {pattern}"),
        notes,
    )?;
    missing_note(missing, notes);
    notes.push("note: loc is the recipe's FILES:<package> assignment (evaluated for the default machine and distro), else its .bb; paths are FILES_INFO as do_package recorded them".into());
    Ok(t)
}

/// The package a name means, with a note on how it was found; an error when none.
fn one_package(pd: &PkgData, name: &str, notes: &mut Vec<String>) -> Result<Vec<Package>> {
    let found = pd.lookup(name);
    if found.is_empty() {
        return Err(Error::NotFound {
            what: format!(
                "a built package named {name} (runtime-rprovides/, runtime-reverse/ and runtime/ of {})",
                pd.dir.display()
            ),
        });
    }
    for (p, how) in &found {
        match how {
            Found::RProvides(f) if f != name => {
                notes.push(format!("note: {name} is in the RPROVIDES of {f}"))
            }
            Found::Reverse if p.name != name => notes.push(format!(
                "note: {name} is the runtime name of the recipe-space package {} (PKG:{} = {name})",
                p.name, p.name
            )),
            Found::Runtime if p.runtime_name() != name => notes.push(format!(
                "note: {name} is a recipe-space name; the package is called {} at runtime",
                p.runtime_name()
            )),
            _ => {}
        }
    }
    Ok(found.into_iter().map(|(p, _)| p).collect())
}

fn list_pkg_files(
    ctx: &Ctx,
    index: &Index,
    pd: &PkgData,
    args: &Args,
    notes: &mut Vec<String>,
) -> Result<Table> {
    let name = package_name(&args.package, "list_pkg_files")?;
    let mut t = Table::new(&["path", "size", "package"]);
    let mut all: Vec<String> = Vec::new();
    let mut missing = 0usize;
    for p in one_package(pd, &name, notes)? {
        let (pn, file) = recipe_cell(index, ctx, &p, &mut missing);
        notes.push(format!(
            "note: package {} of recipe {pn} ({file}); {} bytes in all (PKGSIZE)",
            p.name,
            p.get("PKGSIZE").unwrap_or("?")
        ));
        match p.files() {
            Some(files) => {
                if files.is_empty() {
                    notes.push(format!("note: {} ships no files", p.name));
                }
                for (path, size) in files {
                    all.push(format!("{path}\t{size}\t{}", p.name));
                }
            }
            None => notes.push(format!("note: {} has no FILES_INFO", p.name)),
        }
    }
    for line in all.iter().take(LIMIT) {
        let cells: Vec<&str> = line.split('\t').collect();
        t.row(&cells);
    }
    spill(
        ctx,
        &mut t,
        &all,
        &format!("pkgdata list_pkg_files {name}"),
        notes,
    )?;
    missing_note(missing, notes);
    Ok(t)
}

fn lookup_recipe(
    ctx: &Ctx,
    index: &Index,
    pd: &PkgData,
    args: &Args,
    notes: &mut Vec<String>,
) -> Result<Table> {
    let name = package_name(&args.package, "lookup_recipe")?;
    let mut t = Table::new(&["package", "recipe", "version", "loc"]);
    let mut missing = 0usize;
    for p in one_package(pd, &name, notes)? {
        let (pn, file) = recipe_cell(index, ctx, &p, &mut missing);
        t.row(&[&p.name, &pn, &recipe_version(&p), &file]);
    }
    missing_note(missing, notes);
    Ok(t)
}

/// `[PE:]PV[-PR]`, as `oe-pkgdata-util package-info` prints the recipe version.
fn recipe_version(p: &Package) -> String {
    let mut v = p.get("PV").unwrap_or("").to_string();
    if let Some(e) = p.get("PE") {
        v = format!("{e}:{v}");
    }
    if let Some(r) = p.get("PR") {
        v = format!("{v}-{r}");
    }
    v
}

/// `[PKGE:]PKGV[-PKGR]`, the package version.
fn package_version(p: &Package) -> String {
    let mut v = p.get("PKGV").unwrap_or("").to_string();
    if let Some(e) = p.get("PKGE") {
        v = format!("{e}:{v}");
    }
    if let Some(r) = p.get("PKGR") {
        v = format!("{v}-{r}");
    }
    v
}

fn list_pkgs(
    ctx: &Ctx,
    index: &Index,
    pd: &PkgData,
    args: &Args,
    notes: &mut Vec<String>,
) -> Result<Table> {
    let pn = required(&args.recipe, "recipe", "list_pkgs")?;
    logs::check_name("recipe", pn)?;
    let Some(packages) = pd.recipe_packages(pn) else {
        return Err(Error::NotFound {
            what: format!(
                "package data for recipe {pn} ({}/{pn}): it has not been packaged in this build",
                pd.dir.display()
            ),
        });
    };
    let mut t = Table::new(&["package", "runtime name", "packaged", "size"]);
    let mut empty = 0usize;
    for name in &packages {
        match pd.package(name) {
            Some(p) => {
                if !p.packaged {
                    empty += 1;
                }
                t.row(&[
                    name,
                    p.runtime_name(),
                    if p.packaged { "yes" } else { "no (empty)" },
                    p.get("PKGSIZE").unwrap_or("-"),
                ]);
            }
            None => t.row(&[name, "-", "no runtime data", "-"]),
        }
    }
    let file =
        recipe_file(index, ctx, pn, None).unwrap_or_else(|| "in no layer of the workspace".into());
    notes.push(format!(
        "note: packages of recipe {pn} ({file}), from {pn}'s PACKAGES in the package data"
    ));
    if empty > 0 {
        notes.push(format!(
            "note: {empty} {} no .packaged marker (empty and not ALLOW_EMPTY): no package file is made for {}",
            if empty == 1 { "package has" } else { "packages have" },
            if empty == 1 { "it" } else { "them" }
        ));
    }
    Ok(t)
}

fn package_info(
    ctx: &Ctx,
    index: &Index,
    pd: &PkgData,
    args: &Args,
    notes: &mut Vec<String>,
) -> Result<Table> {
    let name = package_name(&args.package, "package_info")?;
    let mut t = Table::new(&["package", "field", "value", "loc"]);
    let mut missing = 0usize;
    for p in one_package(pd, &name, notes)? {
        let (pn, file) = recipe_cell(index, ctx, &p, &mut missing);
        let n = p.name.as_str();
        t.row(&[n, "recipe", &format!("{pn} {}", recipe_version(&p)), &file]);
        t.row(&[n, "version", &package_version(&p), "-"]);
        t.row(&[n, "runtime name", p.runtime_name(), "-"]);
        t.row(&[n, "size", p.get("PKGSIZE").unwrap_or("-"), "-"]);
        t.row(&[
            n,
            "packaged",
            if p.packaged { "yes" } else { "no (empty)" },
            "-",
        ]);
        for var in [
            "RDEPENDS",
            "RRECOMMENDS",
            "RSUGGESTS",
            "RPROVIDES",
            "RREPLACES",
            "RCONFLICTS",
            "LICENSE",
            "SECTION",
            "SUMMARY",
        ] {
            if let Some(v) = p.get(var)
                && !v.trim().is_empty()
            {
                t.row(&[n, var, &render::snippet(v.trim()), "-"]);
            }
        }
        if let Some(files) = p.files() {
            t.row(&[n, "files", &files.len().to_string(), "-"]);
        }
    }
    missing_note(missing, notes);
    notes.push("note: values as do_packagedata recorded them (after do_package: RDEPENDS includes the shared-library and pkg-config dependencies it added)".into());
    Ok(t)
}
