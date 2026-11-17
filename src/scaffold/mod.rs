//! Scaffold engine: plans, dry runs, confined writes. Generators live in their own files.
//!
//! A generator ([`Generator`]) turns a [`Request`] into a [`Plan`] and never touches the
//! disk. The engine:
//!
//! 1. finds the generator for the request's [`Kind`] ([`generator`]) and checks the
//!    request's `options` against the names the generator accepts ([`check_options`]);
//! 2. checks the plan ([`check_plan`]): absolute paths, no duplicates, every path passes
//!    `Workspace::confine` and lies in a writable layer, `Create` targets do not exist,
//!    `Modify` targets still hash to `before_sha`. A dry run runs the same checks, so a plan
//!    that could not be applied is refused in both modes;
//! 3. on `write`, applies it ([`apply_plan`]): each file is written to a temporary file in
//!    the target directory and renamed over the target; the `Create` / `Modify` condition is
//!    checked again just before the rename;
//! 4. after writing, the caller refreshes the index and runs [`PostWriteLint`] on exactly
//!    the written files ([`execute`] does all of this).
//!
//! New layers (T6.3) are created outside every existing layer root. A plan that does so sets
//! [`Plan::new_layer`]; files under that root are then allowed when the new root sits in a
//! directory that already holds a workspace layer, and `conf/bblayers.conf` of the build
//! directory may be modified by the same plan. Nothing else outside a writable layer is.

pub mod append;
pub mod class;
pub mod inc_split;
pub mod layer;
pub mod placement;
pub mod recipe;

use std::fs;
use std::io::Write;
use std::path::{Component, Path, PathBuf};

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::config::Config;
use crate::error::{Error, Result};
use crate::index::{Index, RefreshReport};
use crate::lint::Finding;
use crate::types::LineSpan;
use crate::workspace::{Layer, Workspace};

/// What a generator returns. Nothing is written until the engine applies it.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct Plan {
    pub files: Vec<PlannedFile>,
    /// Things the user must supply or know (missing checksums, confinement notes...).
    pub notes: Vec<String>,
    /// Root of a layer this plan creates (T6.3), absolute. Files under it are allowed even
    /// though no workspace layer contains them; see the module documentation.
    pub new_layer: Option<PathBuf>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PlannedFile {
    /// Absolute path of the file to create or modify.
    pub path: PathBuf,
    /// Full new content of the file.
    pub content: String,
    pub action: FileAction,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub enum FileAction {
    /// Never overwrites an existing file.
    Create,
    /// Applied only if the file's current sha256 (hex) equals `before_sha`.
    Modify { before_sha: String },
}

impl FileAction {
    pub fn as_str(&self) -> &'static str {
        match self {
            FileAction::Create => "create",
            FileAction::Modify { .. } => "modify",
        }
    }
}

/// What to create.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Kind {
    /// A new recipe.
    Recipe,
    /// A bbappend (or a change to an existing one).
    Bbappend,
    /// A new layer.
    Layer,
    /// A new bbclass.
    Class,
    /// Move statements shared by several versions into `<pn>.inc`.
    IncSplit,
    /// Write nothing: recommend where a change belongs.
    Where,
}

impl Kind {
    pub const ALL: [Kind; 6] = [
        Kind::Recipe,
        Kind::Bbappend,
        Kind::Layer,
        Kind::Class,
        Kind::IncSplit,
        Kind::Where,
    ];

    /// The argument text (`inc_split`, ...).
    pub fn as_str(self) -> &'static str {
        match self {
            Kind::Recipe => "recipe",
            Kind::Bbappend => "bbappend",
            Kind::Layer => "layer",
            Kind::Class => "class",
            Kind::IncSplit => "inc_split",
            Kind::Where => "where",
        }
    }

    /// The task that implements this kind's generator.
    pub fn task(self) -> &'static str {
        match self {
            Kind::Recipe | Kind::Bbappend => "T6.2",
            Kind::Layer | Kind::Class => "T6.3",
            Kind::IncSplit => "T6.4",
            Kind::Where => "T6.5",
        }
    }

    /// `where` writes nothing, so it needs no writable target layer.
    pub fn writes(self) -> bool {
        self != Kind::Where
    }
}

/// A scaffold request, as the `scaffold` tool received it.
#[derive(Debug, Clone, PartialEq)]
pub struct Request {
    pub kind: Kind,
    /// Target layer name, if given.
    pub layer: Option<String>,
    pub name: Option<String>,
    pub version: Option<String>,
    /// Kind-specific options; already checked against [`Generator::options`] when a
    /// generator sees them.
    pub options: serde_json::Map<String, serde_json::Value>,
}

/// What a generator may look at. Read-only: generators never write.
pub struct GenContext<'a> {
    pub ws: &'a Workspace,
    pub config: &'a Config,
    /// Refreshed by [`execute`] just before [`Generator::plan`] is called.
    pub index: &'a Index,
    pub request: &'a Request,
}

impl<'a> GenContext<'a> {
    /// The target layer: the one named by `layer`, or the only writable layer when none is
    /// named. Errors name the writable layers.
    pub fn target_layer(&self) -> Result<&'a Layer> {
        target_layer(self.ws, self.request.layer.as_deref())
    }

    /// A string option, `None` if absent; an error if present with another JSON type.
    pub fn opt_str(&self, key: &str) -> Result<Option<&'a str>> {
        match self.request.options.get(key) {
            None | Some(serde_json::Value::Null) => Ok(None),
            Some(serde_json::Value::String(s)) => Ok(Some(s.as_str())),
            Some(_) => Err(invalid(format!("option `{key}` must be a string"))),
        }
    }
}

/// The interface T6.2 to T6.5 implement, one unit struct per kind.
pub trait Generator: Sync {
    fn kind(&self) -> Kind;
    /// Every option name this kind accepts. Any other key in `options` is refused by the
    /// engine before [`Generator::plan`] is called.
    fn options(&self) -> &'static [&'static str];
    /// Computes the plan. Must not write anything.
    fn plan(&self, cx: &GenContext<'_>) -> Result<Plan>;
}

/// The generator for `kind`, or `None` while its task has not landed.
///
/// Generator tasks: replace `None` in your arm with `Some(&<file>::<Struct>)` (the only
/// change to this file a generator task needs).
pub fn generator(kind: Kind) -> Option<&'static dyn Generator> {
    match kind {
        Kind::Recipe => Some(&recipe::RecipeGen), // T6.2: recipe.rs
        Kind::Bbappend => Some(&append::AppendGen), // T6.2: append.rs
        Kind::Layer => Some(&layer::LayerGen),    // T6.3: layer.rs
        Kind::Class => Some(&class::ClassGen),    // T6.3: class.rs
        Kind::IncSplit => Some(&inc_split::IncSplitGen), // T6.4: inc_split.rs
        Kind::Where => Some(&placement::PlacementGen), // T6.5: placement.rs
    }
}

/// [`generator`], or the one-line error for a kind whose generator does not exist yet.
pub fn require_generator(kind: Kind) -> Result<&'static dyn Generator> {
    generator(kind).ok_or_else(|| {
        invalid(format!(
            "scaffold kind `{}` is not implemented yet ({}); nothing was written",
            kind.as_str(),
            kind.task()
        ))
    })
}

/// Refuses option keys the generator does not accept, listing the valid ones.
pub fn check_options(
    generator: &dyn Generator,
    options: &serde_json::Map<String, serde_json::Value>,
) -> Result<()> {
    let valid = generator.options();
    let mut unknown: Vec<&str> = options
        .keys()
        .map(String::as_str)
        .filter(|k| !valid.contains(k))
        .collect();
    if unknown.is_empty() {
        return Ok(());
    }
    unknown.sort_unstable();
    let listed = if valid.is_empty() {
        "(none)".to_string()
    } else {
        valid.join(", ")
    };
    Err(invalid(format!(
        "unknown option{} {} for kind {}; valid options: {listed}",
        if unknown.len() == 1 { "" } else { "s" },
        unknown
            .iter()
            .map(|k| format!("`{k}`"))
            .collect::<Vec<_>>()
            .join(", "),
        generator.kind().as_str()
    )))
}

/// Resolves the target layer (see [`GenContext::target_layer`]). Does not check that it is
/// writable; [`check_target_layer`] does.
pub fn target_layer<'a>(ws: &'a Workspace, name: Option<&str>) -> Result<&'a Layer> {
    match name {
        Some(n) => ws.layer(n).ok_or_else(|| Error::NotFound {
            what: format!(
                "layer {n}; layers: {}",
                ws.layers
                    .iter()
                    .map(|l| l.name.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
        }),
        None => {
            let writable: Vec<&Layer> = ws.layers.iter().filter(|l| l.writable).collect();
            match writable.as_slice() {
                [one] => Ok(one),
                [] => Err(not_writable_hint("no layer is writable")),
                many => Err(Error::Ambiguous {
                    what: "target layer; pass `layer`".into(),
                    candidates: many.iter().map(|l| l.name.clone()).collect(),
                }),
            }
        }
    }
}

/// When `layer` is given, it must exist and, for kinds that write, be writable.
pub fn check_target_layer(ws: &Workspace, kind: Kind, layer: Option<&str>) -> Result<()> {
    let Some(name) = layer else { return Ok(()) };
    let l = target_layer(ws, Some(name))?;
    if kind.writes() && !l.writable {
        return Err(not_writable(ws, &l.name));
    }
    Ok(())
}

fn not_writable(ws: &Workspace, name: &str) -> Error {
    let writable: Vec<&str> = ws
        .layers
        .iter()
        .filter(|l| l.writable)
        .map(|l| l.name.as_str())
        .collect();
    if writable.is_empty() {
        not_writable_hint(&format!("layer {name} is not writable and no layer is"))
    } else {
        invalid(format!(
            "layer {name} is not writable; writable layers: {} (writable_layers in sous-chef.toml)",
            writable.join(", ")
        ))
    }
}

fn not_writable_hint(what: &str) -> Error {
    invalid(format!(
        "{what}; list the layer in writable_layers in sous-chef.toml"
    ))
}

fn invalid(msg: String) -> Error {
    Error::Invalid { msg }
}

/// Lower-case hex sha256 of `bytes` (the form `FileAction::Modify::before_sha` uses).
pub fn sha256_hex(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// [`sha256_hex`] of a file's current content.
pub fn file_sha256(path: &Path) -> Result<String> {
    Ok(sha256_hex(&fs::read(path)?))
}

/// A planned file after [`check_plan`]: where it will be written and how to show it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CheckedFile {
    /// Resolved target (symlinks followed): the file actually written.
    pub target: PathBuf,
    /// `layer:path` when the target is inside a workspace layer (or the build directory),
    /// else `<new layer name>:path` for a new layer, else the absolute path.
    pub display: String,
    /// Index into `Plan::files`.
    pub index: usize,
}

/// Checks every planned file without writing anything. See the module documentation.
pub fn check_plan(ws: &Workspace, plan: &Plan) -> Result<Vec<CheckedFile>> {
    let new_layer = match &plan.new_layer {
        Some(root) => Some(check_new_layer(ws, root)?),
        None => None,
    };
    let mut out: Vec<CheckedFile> = Vec::with_capacity(plan.files.len());
    for (i, f) in plan.files.iter().enumerate() {
        if !f.path.is_absolute() {
            return Err(invalid(format!(
                "generator produced a relative path {}; this is a bug in the generator",
                f.path.display()
            )));
        }
        let (target, display) = match new_layer
            .as_ref()
            .and_then(|root| under_new_layer(root, &f.path))
        {
            Some(found) => found?,
            None => confine_writable(ws, &f.path, new_layer.is_some())?,
        };
        if out.iter().any(|c| c.target == target) {
            return Err(invalid(format!(
                "plan names {} twice; this is a bug in the generator",
                display
            )));
        }
        check_condition(&target, &display, &f.action)?;
        out.push(CheckedFile {
            target,
            display,
            index: i,
        });
    }
    Ok(out)
}

/// The confined target of a file that must be inside a writable layer (or be the build
/// directory's `conf/bblayers.conf` when the plan creates a layer).
fn confine_writable(ws: &Workspace, path: &Path, new_layer: bool) -> Result<(PathBuf, String)> {
    let target = ws.confine(path)?;
    if let Some(layer) = containing_layer(ws, &target) {
        let display = display_in(&layer.name, &layer.root, &target);
        if !layer.writable {
            return Err(not_writable(ws, &layer.name));
        }
        return Ok((target, display));
    }
    if let Some(build) = &ws.build_dir
        && target.starts_with(build)
    {
        let display = display_in(crate::workspace::BUILD_LAYER, build, &target);
        if new_layer && target == build.join("conf").join("bblayers.conf") {
            return Ok((target, display));
        }
        return Err(invalid(format!(
            "{display} is in the build directory, which scaffold does not write (only conf/bblayers.conf, for a new layer)"
        )));
    }
    // `confine` accepted it, so it is in a layer or the build directory; unreachable in
    // practice, refused anyway.
    Err(Error::Confined {
        path: path.to_path_buf(),
    })
}

/// The innermost layer whose root contains `target`.
fn containing_layer<'a>(ws: &'a Workspace, target: &Path) -> Option<&'a Layer> {
    ws.layers
        .iter()
        .filter(|l| target.starts_with(&l.root))
        .max_by_key(|l| l.root.components().count())
}

fn display_in(name: &str, root: &Path, target: &Path) -> String {
    match target.strip_prefix(root) {
        Ok(rel) => format!("{name}:{}", rel.display()),
        Err(_) => target.display().to_string(),
    }
}

/// Resolved root of a new layer: its parent must exist and already hold a workspace layer;
/// the root itself must not be (or be inside) an existing layer or the build directory, and
/// must not already contain `conf/layer.conf`.
fn check_new_layer(ws: &Workspace, root: &Path) -> Result<PathBuf> {
    let refuse = |why: &str| invalid(format!("cannot create layer at {}: {why}", root.display()));
    if !root.is_absolute() {
        return Err(refuse("the path is not absolute"));
    }
    let resolved = resolve_path(root).map_err(|_| refuse("the path cannot be resolved"))?;
    let parent = resolved
        .parent()
        .ok_or_else(|| refuse("it has no parent directory"))?;
    let inside_existing = ws
        .layers
        .iter()
        .map(|l| &l.root)
        .chain(ws.build_dir.as_ref())
        .any(|r| resolved.starts_with(r) || r.starts_with(&resolved));
    if inside_existing {
        return Err(refuse(
            "it overlaps an existing layer or the build directory",
        ));
    }
    let holds_layer = ws.layers.iter().any(|l| l.root.parent() == Some(parent));
    if !holds_layer {
        return Err(refuse(
            "a new layer must be created in a directory that already holds a workspace layer",
        ));
    }
    if resolved.join("conf").join("layer.conf").exists() {
        return Err(refuse("it already contains conf/layer.conf"));
    }
    Ok(resolved)
}

/// `Some` when `path` is lexically under the new layer root; then the resolved target must
/// stay under it (symlinks inside the new tree cannot point elsewhere).
fn under_new_layer(root: &Path, path: &Path) -> Option<Result<(PathBuf, String)>> {
    let resolved = resolve_path(path).ok()?;
    if !lexical(path).starts_with(root) && !resolved.starts_with(root) {
        return None;
    }
    if !resolved.starts_with(root) {
        return Some(Err(Error::Confined {
            path: path.to_path_buf(),
        }));
    }
    let name = root
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    Some(Ok((resolved.clone(), display_in(&name, root, &resolved))))
}

/// Lexical normalisation (`.` dropped, `..` pops), no filesystem access.
fn lexical(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for c in path.components() {
        match c {
            Component::ParentDir => {
                out.pop();
            }
            Component::CurDir => {}
            c => out.push(c),
        }
    }
    out
}

/// As `Workspace::confine` resolves: the longest existing prefix through the filesystem,
/// the rest lexically. A dangling symlink is an error.
fn resolve_path(path: &Path) -> std::io::Result<PathBuf> {
    let mut cur = PathBuf::new();
    let mut exists = true;
    for c in path.components() {
        match c {
            Component::Prefix(_) | Component::RootDir => cur.push(c),
            Component::CurDir => {}
            Component::ParentDir => {
                cur.pop();
            }
            Component::Normal(name) => {
                cur.push(name);
                if exists {
                    match fs::canonicalize(&cur) {
                        Ok(real) => cur = real,
                        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                            if fs::symlink_metadata(&cur).is_ok() {
                                return Err(e);
                            }
                            exists = false;
                        }
                        Err(e) => return Err(e),
                    }
                }
            }
        }
    }
    Ok(cur)
}

/// `Create`: the target must not exist. `Modify`: it must exist and hash to `before_sha`.
fn check_condition(target: &Path, display: &str, action: &FileAction) -> Result<()> {
    match action {
        FileAction::Create => {
            if fs::symlink_metadata(target).is_ok() {
                return Err(invalid(format!(
                    "{display} already exists; scaffold never overwrites a file it creates (remove it or scaffold a change to it)"
                )));
            }
        }
        FileAction::Modify { before_sha } => {
            let current = match fs::read(target) {
                Ok(bytes) => sha256_hex(&bytes),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                    return Err(invalid(format!(
                        "{display} no longer exists; re-run scaffold"
                    )));
                }
                Err(e) => return Err(e.into()),
            };
            if !current.eq_ignore_ascii_case(before_sha) {
                return Err(invalid(format!(
                    "{display} changed since the plan was made (sha256 {} expected, {} found); re-run scaffold",
                    short(before_sha),
                    short(&current)
                )));
            }
        }
    }
    Ok(())
}

fn short(sha: &str) -> &str {
    sha.get(..12).unwrap_or(sha)
}

/// Writes a checked plan. Re-checks each file's condition immediately before its rename.
/// Returns the written targets in plan order. If a write fails, the error names the files
/// already written (they are not rolled back).
pub fn apply_plan(plan: &Plan, checked: &[CheckedFile]) -> Result<Vec<PathBuf>> {
    let mut written: Vec<PathBuf> = Vec::with_capacity(checked.len());
    let mut shown: Vec<&str> = Vec::new();
    for c in checked {
        let Some(f) = plan.files.get(c.index) else {
            continue;
        };
        if let Err(e) = write_one(c, f) {
            if shown.is_empty() {
                return Err(e);
            }
            return Err(invalid(format!(
                "writing {} failed ({e}); already written: {}",
                c.display,
                shown.join(", ")
            )));
        }
        written.push(c.target.clone());
        shown.push(&c.display);
    }
    Ok(written)
}

fn write_one(c: &CheckedFile, f: &PlannedFile) -> Result<()> {
    let dir = c.target.parent().ok_or_else(|| Error::Confined {
        path: c.target.clone(),
    })?;
    fs::create_dir_all(dir)?;
    let file_name = c
        .target
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let tmp = temp_path(dir, &file_name);
    let result = (|| -> Result<()> {
        let mut out = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&tmp)?;
        out.write_all(f.content.as_bytes())?;
        out.sync_all()?;
        drop(out);
        if let FileAction::Modify { .. } = f.action
            && let Ok(meta) = fs::metadata(&c.target)
        {
            fs::set_permissions(&tmp, meta.permissions())?;
        }
        check_condition(&c.target, &c.display, &f.action)?;
        fs::rename(&tmp, &c.target)?;
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(&tmp);
    }
    result
}

fn temp_path(dir: &Path, file_name: &str) -> PathBuf {
    use std::sync::atomic::{AtomicU64, Ordering};
    static N: AtomicU64 = AtomicU64::new(0);
    let n = N.fetch_add(1, Ordering::Relaxed);
    dir.join(format!(
        ".{file_name}.sous-chef-{}-{n}.tmp",
        std::process::id()
    ))
}

/// What the post-write lint sees.
pub struct LintTarget<'a> {
    pub ws: &'a Workspace,
    pub config: &'a Config,
    pub index: &'a Index,
    /// Exactly the files the scaffold wrote, resolved.
    pub files: &'a [PathBuf],
}

/// The post-write lint step, a seam so tests can observe it. [`RuleLint`] is the real one.
pub trait PostWriteLint: Sync {
    fn lint(&self, target: &LintTarget<'_>) -> Result<Vec<Finding>>;
}

/// Runs the lint engine ([`crate::lint::run`]) on exactly the written files: file-scope
/// rules on each, recipe-scope rules on each `.bb`, and layer-scope rules for their layers,
/// keeping only findings located in the written files. `[lint] disable`, `[lint.severity]`
/// and inline suppression apply. Files of a new layer are not in the workspace until
/// `bblayers.conf` lists it, so they are not linted; neither are files that are not BitBake
/// metadata (patches and other files a bbappend adds).
pub struct RuleLint;

impl PostWriteLint for RuleLint {
    fn lint(&self, t: &LintTarget<'_>) -> Result<Vec<Finding>> {
        let files: Vec<PathBuf> = t
            .files
            .iter()
            .filter(|p| t.ws.loc_for(p, LineSpan::line(1)).is_some())
            // Patches and other files a scaffold adds are not BitBake metadata.
            .filter(|p| crate::types::FileKind::from_path(p).is_some())
            .cloned()
            .collect();
        if files.is_empty() {
            return Ok(Vec::new());
        }
        let target = crate::lint::Target::Files { files, lines: None };
        let report = crate::lint::run(
            t.ws,
            t.index,
            t.config,
            &target,
            &crate::lint::Options::default(),
        )?;
        Ok(report.findings)
    }
}

/// Result of [`execute`].
#[derive(Debug, Clone, PartialEq)]
pub struct Outcome {
    pub kind: Kind,
    pub plan: Plan,
    /// One per planned file, in plan order.
    pub checked: Vec<CheckedFile>,
    /// True when the files were written.
    pub written: bool,
    /// After a write: the index refresh (an error message when it failed).
    pub refresh: Option<std::result::Result<RefreshReport, String>>,
    /// After a write: lint findings on the written files (an error message when lint
    /// failed).
    pub lint: Option<std::result::Result<Vec<Finding>, String>>,
}

/// The whole scaffold: plan, check, and on `write` apply, refresh, lint.
///
/// `generator` overrides the dispatch on `request.kind` (tests); `None` uses [`generator`].
/// A failing refresh or lint is reported in the outcome, never as an error: the files are
/// already written by then.
pub fn execute(
    ws: &Workspace,
    config: &Config,
    index: &mut Index,
    request: &Request,
    write: bool,
    generator_override: Option<&dyn Generator>,
    lint: &dyn PostWriteLint,
) -> Result<Outcome> {
    let generator = match generator_override {
        Some(g) => g,
        None => require_generator(request.kind)?,
    };
    check_options(generator, &request.options)?;
    check_target_layer(ws, request.kind, request.layer.as_deref())?;
    // Generators read the index (the recipe a bbappend extends, existing versions and
    // `.inc` files) and cannot refresh it themselves (`GenContext.index` is shared).
    index.refresh(ws)?;
    let plan = generator.plan(&GenContext {
        ws,
        config,
        index,
        request,
    })?;
    if !request.kind.writes() && !plan.files.is_empty() {
        return Err(invalid(format!(
            "kind {} must not plan files; this is a bug in the generator",
            request.kind.as_str()
        )));
    }
    let checked = check_plan(ws, &plan)?;
    let mut outcome = Outcome {
        kind: request.kind,
        plan,
        checked,
        written: false,
        refresh: None,
        lint: None,
    };
    if !write || outcome.plan.files.is_empty() {
        return Ok(outcome);
    }
    let files = apply_plan(&outcome.plan, &outcome.checked)?;
    outcome.written = true;
    outcome.refresh = Some(index.refresh(ws).map_err(|e| e.to_string()));
    outcome.lint = Some(
        lint.lint(&LintTarget {
            ws,
            config,
            index,
            files: &files,
        })
        .map_err(|e| e.to_string()),
    );
    Ok(outcome)
}

/// Number of lines in a planned file's content.
pub fn line_count(content: &str) -> usize {
    content.lines().count()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sha_is_lower_hex() {
        assert_eq!(
            sha256_hex(b""),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
    }

    #[test]
    fn every_kind_has_a_one_line_not_implemented_error() {
        for k in Kind::ALL {
            if generator(k).is_none() {
                let e = require_generator(k).err().map(|e| e.to_string());
                let msg = e.unwrap_or_default();
                assert!(msg.contains(k.as_str()) && !msg.contains('\n'), "{msg}");
            }
        }
    }

    #[test]
    fn lexical_pops_parent() {
        assert_eq!(lexical(Path::new("/a/b/../c/./d")), PathBuf::from("/a/c/d"));
    }
}
