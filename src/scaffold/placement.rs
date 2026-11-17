//! Scaffold generator: `where` (T6.5). Placement advice; plans no files.
//!
//! Input: a recipe (`recipe` option or the tool's `name`, `version` picks a version), and a
//! `variable`, a `task` or an `intent` ([`INTENTS`]); with only a recipe the advice is for
//! changing that recipe in general. Options: [`OPTIONS`]. The answer is in the plan's
//! notes, one line each:
//!
//! - `where:` the file (an existing one as a pointer, or the path to create);
//! - `why:` the convention, as the `org` rules encode it (`org/upstream-edit`,
//!   `org/policy-in-recipe`, `org/recipe-change-in-local-conf`, `org/duplicate-versions`,
//!   `org/repeated-logic`);
//! - `scaffold:` the call that makes the change, or `by hand:` when no generator writes
//!   that file;
//! - then short facts with pointers: where the value is set now, the bbappends that
//!   already apply, how the layer was chosen.
//!
//! Rules (M6, T6.5): a change to a recipe in a layer that is not writable goes in a bbappend
//! in a writable layer, the existing one when there is one; a recipe in a writable layer is
//! edited directly (its `.inc` when the value is set there). Hardware-specific values go in
//! the machine conf, or a `:<machine>` override in the bbappend when they belong to one
//! recipe. Policy (`DISTRO_FEATURES`, `PREFERRED_PROVIDER_*`, `PREFERRED_VERSION_*`, the
//! init manager, libc) goes in the distro conf. Image content goes in the image recipe or a
//! bbappend to it. `local.conf` is only ever named as a developer-local trial. Logic shared by
//! several recipes goes in a bbclass; data shared by the versions of one recipe in a `.inc`.
//!
//! The writable layer: the `layer` argument when it is writable (when it is not, a note
//! says so and names the writable layers); else the only writable layer; else the one
//! holding related content (a bbappend of the recipe, the recipe itself, the machine or
//! distro conf, the image), else the highest priority one, and a note says how it was
//! chosen. With no writable layer the advice is still given and a note says how to make
//! one writable.

use std::path::{Path, PathBuf};

use super::recipe::{check_pn, invalid, opt_text};
use super::{GenContext, Generator, Kind, Plan};
use crate::error::{Error, Result};
use crate::eval::providers::{self, Providers, Selection};
use crate::eval::{self, DataStore, OpStatus, Target};
use crate::index::query::{self, FileRow, RecipeRow};
use crate::lint::helpers;
use crate::types::{FileKind, Loc};
use crate::workspace::Layer;

/// Every option `where` accepts.
pub const OPTIONS: &[&str] = &[
    "recipe", "variable", "task", "intent", "machine", "image", "package", "item", "value",
];

/// The intents `intent` accepts.
pub const INTENTS: &[&str] = &[
    "add-patch",
    "change-config",
    "add-package-to-image",
    "set-machine-feature",
    "pin-version",
    "change-provider",
];

pub struct PlacementGen;

impl Generator for PlacementGen {
    fn kind(&self) -> Kind {
        Kind::Where
    }

    fn options(&self) -> &'static [&'static str] {
        OPTIONS
    }

    fn plan(&self, cx: &GenContext<'_>) -> Result<Plan> {
        let notes = advise(cx)?;
        Ok(Plan {
            files: Vec::new(),
            notes,
            new_layer: None,
        })
    }
}

/// What the change is about.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Intent {
    AddPatch,
    ChangeConfig,
    AddToImage,
    MachineFeature,
    PinVersion,
    ChangeProvider,
    /// Distro policy other than versions and providers.
    DistroPolicy,
    /// A task of a recipe.
    Task,
}

/// Variables that are distro policy (`org/policy-in-recipe` and T6.5).
fn is_distro_policy(base: &str) -> bool {
    base.starts_with("DISTRO_FEATURES")
        || base.starts_with("VIRTUAL-RUNTIME")
        || matches!(
            base,
            "INIT_MANAGER" | "TCLIBC" | "TCMODE" | "DISTROOVERRIDES" | "LICENSE_FLAGS_ACCEPTED"
        )
}

fn is_machine_policy(base: &str) -> bool {
    base.starts_with("MACHINE_FEATURES")
        || matches!(
            base,
            "MACHINEOVERRIDES" | "SERIAL_CONSOLES" | "KERNEL_IMAGETYPE" | "DEFAULTTUNE"
        )
}

fn is_image_content(base: &str) -> bool {
    matches!(
        base,
        "IMAGE_INSTALL" | "IMAGE_FEATURES" | "CORE_IMAGE_EXTRA_INSTALL" | "EXTRA_IMAGE_FEATURES"
    )
}

/// `EXTRA_OECONF:append:foo[flag]` -> `EXTRA_OECONF`.
fn base_of(var: &str) -> &str {
    let end = var.find([':', '[']).unwrap_or(var.len());
    &var[..end]
}

/// Providers whose choice is usually machine configuration in OE-Core layers.
fn machine_provider(item: &str) -> bool {
    matches!(item, "virtual/kernel" | "virtual/bootloader")
}

struct Advice {
    notes: Vec<String>,
}

impl Advice {
    fn push(&mut self, s: impl Into<String>) {
        self.notes.push(s.into());
    }
}

fn advise(cx: &GenContext<'_>) -> Result<Vec<String>> {
    let variable = opt_text(cx, "variable")?;
    let task = opt_text(cx, "task")?;
    let machine = opt_text(cx, "machine")?;
    let intent = match opt_text(cx, "intent")? {
        Some(i) => Some(parse_intent(i)?),
        None => None,
    };
    let recipe_name = match opt_text(cx, "recipe")? {
        Some(r) => Some(r),
        None => cx
            .request
            .name
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty()),
    };
    if variable.is_some() && task.is_some() {
        return Err(invalid("pass `variable` or `task`, not both"));
    }
    let intent = match (intent, variable, task) {
        (Some(i), _, _) => i,
        (None, Some(v), _) => {
            let b = base_of(v);
            if b.starts_with("PREFERRED_VERSION") {
                Intent::PinVersion
            } else if b.starts_with("PREFERRED_PROVIDER") {
                Intent::ChangeProvider
            } else if is_distro_policy(b) {
                Intent::DistroPolicy
            } else if is_machine_policy(b) {
                Intent::MachineFeature
            } else if is_image_content(b) {
                Intent::AddToImage
            } else {
                Intent::ChangeConfig
            }
        }
        (None, None, Some(_)) => Intent::Task,
        (None, None, None) if recipe_name.is_some() => Intent::ChangeConfig,
        (None, None, None) => {
            return Err(invalid(format!(
                "where needs a recipe (`name`), a `variable`, a `task` or an `intent` ({})",
                INTENTS.join(", ")
            )));
        }
    };

    let mut a = Advice { notes: Vec::new() };
    let layers = Layers::new(cx, &mut a);
    match intent {
        Intent::AddPatch | Intent::ChangeConfig | Intent::Task => {
            let Some(name) = recipe_name else {
                return Err(invalid(
                    "this change belongs to a recipe: pass the recipe as `name` (or the `recipe` option)",
                ));
            };
            let sel = find_recipe(cx, name, machine)?;
            recipe_change(cx, &layers, &mut a, &sel, intent, variable, task, machine)?;
        }
        Intent::AddToImage => image_change(cx, &layers, &mut a, recipe_name, machine)?,
        Intent::MachineFeature => machine_change(cx, &layers, &mut a, variable, machine)?,
        Intent::PinVersion | Intent::ChangeProvider | Intent::DistroPolicy => {
            distro_change(cx, &layers, &mut a, intent, variable, recipe_name, machine)?
        }
    }
    Ok(a.notes)
}

fn parse_intent(s: &str) -> Result<Intent> {
    Ok(match s {
        "add-patch" => Intent::AddPatch,
        "change-config" => Intent::ChangeConfig,
        "add-package-to-image" => Intent::AddToImage,
        "set-machine-feature" => Intent::MachineFeature,
        "pin-version" => Intent::PinVersion,
        "change-provider" => Intent::ChangeProvider,
        other => {
            return Err(invalid(format!(
                "unknown intent {other:?}; intents: {}",
                INTENTS.join(", ")
            )));
        }
    })
}

fn find_recipe(cx: &GenContext<'_>, name: &str, machine: Option<&str>) -> Result<Selection> {
    check_pn(name)?;
    let version = cx
        .request
        .version
        .as_deref()
        .map(str::trim)
        .filter(|v| !v.is_empty());
    match providers::preferred_recipe(cx.index, cx.ws, name, version, machine, None)? {
        Some(s) => Ok(s),
        None => {
            let like = query::recipe_names_like(cx.index, name, 10)?;
            Err(Error::NotFound {
                what: match (version, like.is_empty()) {
                    (Some(v), _) => format!("version {v} of recipe {name}"),
                    (None, true) => format!("recipe {name} in the workspace"),
                    (None, false) => format!("recipe {name}; similar: {}", like.join(", ")),
                },
            })
        }
    }
}

// ---------------------------------------------------------------------------------------
// The writable layer

struct Layers<'a> {
    writable: Vec<&'a Layer>,
    /// The `layer` argument, when it names a writable layer.
    asked: Option<&'a Layer>,
}

impl<'a> Layers<'a> {
    fn new(cx: &GenContext<'a>, a: &mut Advice) -> Layers<'a> {
        let mut writable: Vec<&Layer> = cx.ws.layers.iter().filter(|l| l.writable).collect();
        writable.sort_by_key(|l| std::cmp::Reverse(l.priority));
        let mut asked = None;
        if let Some(name) = cx.request.layer.as_deref()
            && let Some(l) = cx.ws.layer(name)
        {
            if l.writable {
                asked = Some(l);
            } else if writable.is_empty() {
                a.push(format!(
                    "note: {} is not writable, and no layer is; list your own layer in writable_layers in sous-chef.toml (or create one: scaffold kind=layer)",
                    l.name
                ));
            } else {
                a.push(format!(
                    "note: {} is not writable (upstream: an update of the layer overwrites edits there); use {} instead",
                    l.name,
                    names(&writable)
                ));
            }
        }
        if asked.is_none() && writable.is_empty() && cx.request.layer.is_none() {
            a.push(
                "note: no layer is writable: list your own layer in writable_layers in sous-chef.toml (or create one: scaffold kind=layer); until then the advice names no layer",
            );
        }
        Layers { writable, asked }
    }

    /// The layer to use: asked, the only one, the first that `holds` content, else the
    /// highest priority one. Pushes a note on how a choice among several was made.
    fn pick(
        &self,
        a: &mut Advice,
        holds: &dyn Fn(&Layer) -> bool,
        what: &str,
    ) -> Option<&'a Layer> {
        if let Some(l) = self.asked {
            return Some(l);
        }
        match self.writable.as_slice() {
            [] => None,
            [one] => Some(*one),
            many => {
                if let Some(l) = many.iter().find(|l| holds(l)) {
                    a.push(format!(
                        "layer: {} of the writable layers {}, because it already holds {what}; pass `layer` to choose another",
                        l.name,
                        names(many)
                    ));
                    Some(*l)
                } else {
                    let l = many[0];
                    a.push(format!(
                        "layer: {} of the writable layers {} (highest priority; none holds {what}); pass `layer` to choose another",
                        l.name,
                        names(many)
                    ));
                    Some(l)
                }
            }
        }
    }
}

fn names(layers: &[&Layer]) -> String {
    layers
        .iter()
        .map(|l| l.name.as_str())
        .collect::<Vec<_>>()
        .join(", ")
}

/// `<layer>` or a placeholder when there is no writable layer.
fn layer_name(l: Option<&Layer>) -> String {
    l.map(|l| l.name.clone())
        .unwrap_or_else(|| "<your writable layer>".into())
}

fn layer_arg(l: Option<&Layer>) -> String {
    match l {
        Some(l) => format!(" layer={}", l.name),
        None => String::new(),
    }
}

fn file_label(f: &FileRow) -> String {
    format!("{}:{}", f.layer, f.path.display())
}

fn recipe_label(r: &RecipeRow) -> String {
    file_label(&r.file)
}

/// A conf file `conf/<sub>/<name>.conf` in the indexed layers, highest priority first.
fn find_conf(cx: &GenContext<'_>, sub: &str, name: &str) -> Result<Option<FileRow>> {
    let rel = PathBuf::from("conf").join(sub).join(format!("{name}.conf"));
    let mut hits: Vec<FileRow> = query::files_by_kind(cx.index, &[FileKind::Conf])?
        .into_iter()
        .filter(|f| f.path == rel)
        .collect();
    hits.sort_by_key(|f| std::cmp::Reverse(f.priority));
    Ok(hits.into_iter().next())
}

fn writable(cx: &GenContext<'_>, layer: &str) -> bool {
    cx.ws.layer(layer).is_some_and(|l| l.writable)
}

/// The last applied assignment to `var` (any override form), as `loc (lhs op)`.
fn set_at(ds: &DataStore, var: &str) -> Option<(Loc, String)> {
    ds.history(base_of(var))
        .into_iter()
        .rev()
        .find(|(_, st)| *st == OpStatus::Applied)
        .map(|(op, _)| {
            let lhs = helpers::colon_name(&op.var);
            (op.loc, format!("{lhs} {}", op.op.as_str()))
        })
}

// ---------------------------------------------------------------------------------------
// Recipe changes

#[allow(clippy::too_many_arguments)]
fn recipe_change(
    cx: &GenContext<'_>,
    layers: &Layers<'_>,
    a: &mut Advice,
    sel: &Selection,
    intent: Intent,
    variable: Option<&str>,
    task: Option<&str>,
    machine: Option<&str>,
) -> Result<()> {
    let r = &sel.recipe;
    let pn = &r.pn;
    let value = cx.opt_str("value")?;
    let ds = eval::evaluate(
        cx.index,
        cx.ws,
        &Target {
            recipe: Some(recipe_label(r)),
            machine: machine.map(str::to_string),
            ..Target::default()
        },
    )
    .ok();
    let task = task.map(|t| {
        if t.starts_with("do_") {
            t.to_string()
        } else {
            format!("do_{t}")
        }
    });
    let what = match (intent, variable, &task) {
        (Intent::AddPatch, _, _) => "a patch".to_string(),
        (_, Some(v), _) => v.to_string(),
        (_, _, Some(t)) => t.clone(),
        _ => format!("a change to {pn}"),
    };
    let appends = query::appends_for(cx.index, r.file.id)?;
    let in_writable = writable(cx, &r.file.layer);

    if in_writable && layers.asked.is_none_or(|l| l.name == r.file.layer) {
        // Edit the recipe (or its .inc) in place.
        let mut target = recipe_label(r);
        let mut why = format!(
            "{} is in {}, which is writable: change the recipe itself (a bbappend to your own recipe only scatters it)",
            recipe_label(r),
            r.file.layer
        );
        if let (Some(v), Some(d)) = (variable, ds.as_ref())
            && let Some((loc, lhs)) = set_at(d, v)
            && loc.layer == r.file.layer
            && loc.path != r.file.path
            && loc.path.extension().is_some_and(|e| e == "inc")
        {
            target = format!("{}:{}", loc.layer, loc.path.display());
            why = format!(
                "{v} is set in {} (`{lhs}`), shared by the versions of {pn}: change it there for all of them, in the .bb for one",
                loc
            );
        }
        a.push(format!("where: {target}"));
        a.push(format!("why: {why}"));
        match intent {
            Intent::AddPatch => a.push(format!(
                "by hand: put the patch in {}/files/ and add `file://<name>.patch` to SRC_URI in the recipe",
                parent_label(&r.file)
            )),
            _ => a.push(format!(
                "by hand: edit the file{}",
                machine
                    .map(|m| format!(" (machine-specific: `{}:{m}` or `:append:{m}`)", base_of(&what)))
                    .unwrap_or_default()
            )),
        }
        let versions: Vec<RecipeRow> = query::find_recipes(cx.index, pn)?
            .into_iter()
            .filter(|x| x.file.layer == r.file.layer)
            .collect();
        let has_inc = ds.as_ref().is_some_and(|d| {
            d.files()
                .iter()
                .any(|v| v.kind == FileKind::Include && v.loc.layer == r.file.layer)
        });
        if versions.len() > 1 && !has_inc {
            a.push(format!(
                "note: {} versions of {pn} in {} share no .inc; `scaffold kind=inc_split name={pn}` moves what they share into {pn}.inc, so a shared change is made once",
                versions.len(),
                r.file.layer
            ));
        }
    } else {
        let mine: Vec<&FileRow> = appends.iter().filter(|f| writable(cx, &f.layer)).collect();
        let layer = layers.pick(
            a,
            &|l: &Layer| mine.iter().any(|f| f.layer == l.name),
            &format!("a bbappend of {pn}"),
        );
        let existing = layer.and_then(|l| mine.iter().rev().find(|f| f.layer == l.name));
        match existing {
            Some(f) => a.push(format!("where: {} (the existing bbappend)", file_label(f))),
            None => a.push(format!(
                "where: a new bbappend in {}: {}",
                layer_name(layer),
                new_append_path(r, intent == Intent::AddPatch).display()
            )),
        }
        let upstream = if in_writable {
            format!("{} is in {}", recipe_label(r), r.file.layer)
        } else {
            format!(
                "{} is in {}, which is not writable (an update of the layer overwrites edits there, org/upstream-edit)",
                recipe_label(r),
                r.file.layer
            )
        };
        a.push(format!(
            "why: {upstream}; a bbappend in a writable layer changes it from outside{}",
            if existing.is_some() {
                ", and one exists already: add to it rather than a second one"
            } else {
                ""
            }
        ));
        a.push(append_call(
            pn,
            layer,
            intent,
            variable,
            task.as_deref(),
            machine,
            value,
        ));
        for f in appends
            .iter()
            .filter(|f| Some(f.layer.as_str()) != layer.map(|l| l.name.as_str()))
        {
            a.push(format!(
                "note: {} also applies to {}",
                file_label(f),
                recipe_label(r)
            ));
        }
    }

    // Facts.
    if let (Some(v), Some(d)) = (variable, ds.as_ref()) {
        match set_at(d, v) {
            Some((loc, lhs)) => a.push(format!(
                "now: {} is set last at {loc} (`{lhs}`)",
                base_of(v)
            )),
            None => a.push(format!("now: {} is not set for {pn}", base_of(v))),
        }
    }
    if let (Some(t), Some(d)) = (&task, ds.as_ref()) {
        match d.function(t) {
            Some(pieces) if !pieces.is_empty() => {
                let at: Vec<String> = pieces.iter().map(|p| p.loc.to_string()).collect();
                a.push(format!("now: {t} is made of {}", at.join(", ")));
                if pieces.iter().any(|p| p.calls.is_some()) {
                    a.push(format!(
                        "note: {t} comes from a class; `{t}:append()` / `:prepend()` changes it for {pn} only; the same change in several recipes belongs in a bbclass of your own (scaffold kind=class)"
                    ));
                }
            }
            _ => a.push(format!("now: {pn} has no {t}; defining it adds the function, and a new task needs `addtask`")),
        }
        if !pieces_from_class(d, t) {
            a.push(format!(
                "note: if several recipes need the same {t} change, put it in a bbclass (scaffold kind=class) and inherit it (org/repeated-logic)"
            ));
        }
    }
    if machine.is_some() {
        a.push(format!(
            "note: a value for every recipe on that machine belongs in its machine conf (conf/machine/{}.conf); a value for {pn} alone is the `:{}` override above",
            machine.unwrap_or(""),
            machine.unwrap_or("")
        ));
    }
    Ok(())
}

fn pieces_from_class(d: &DataStore, t: &str) -> bool {
    d.function(t)
        .is_some_and(|p| p.iter().any(|x| x.calls.is_some()))
}

fn parent_label(f: &FileRow) -> String {
    format!(
        "{}:{}",
        f.layer,
        f.path.parent().unwrap_or(Path::new("")).display()
    )
}

/// Where scaffold would create the bbappend (relative to the layer).
fn new_append_path(r: &RecipeRow, exact: bool) -> PathBuf {
    let parent = r.file.path.parent().unwrap_or(Path::new(""));
    let dir = if parent
        .components()
        .next()
        .is_some_and(|c| c.as_os_str().to_string_lossy().starts_with("recipes-"))
    {
        parent.to_path_buf()
    } else {
        PathBuf::from("recipes-misc").join(&r.pn)
    };
    let name = if exact {
        format!("{}_{}.bbappend", r.pn, r.pv)
    } else {
        format!("{}_%.bbappend", r.pn)
    };
    dir.join(name)
}

/// The `scaffold:` line for a bbappend change, or `by hand:` for what the bbappend
/// generator does not write (functions).
fn append_call(
    pn: &str,
    layer: Option<&Layer>,
    intent: Intent,
    variable: Option<&str>,
    task: Option<&str>,
    machine: Option<&str>,
    value: Option<&str>,
) -> String {
    let mut opts = serde_json::Map::new();
    match (intent, variable, task) {
        (Intent::AddPatch, _, _) => {
            opts.insert("files".into(), serde_json::json!(["<name>.patch"]));
        }
        (_, _, Some(t)) => {
            return format!(
                "by hand: add `{t}:append() {{ ... }}` (or `:prepend`) to the bbappend{}; scaffold kind=bbappend writes variables only (`scaffold kind=bbappend name={pn}{} options={{\"set\": ...}}` creates the file)",
                machine
                    .map(|m| format!(", `{t}:append:{m}()` for one machine"))
                    .unwrap_or_default(),
                layer_arg(layer)
            );
        }
        (_, Some(v), _) => {
            let lhs = if v.contains(':') || v.contains('[') {
                v.to_string()
            } else {
                format!("{v}:append")
            };
            let val = value.unwrap_or(" ...");
            opts.insert("set".into(), serde_json::json!({ lhs: val }));
        }
        _ => {
            opts.insert(
                "set".into(),
                serde_json::json!({ "<VARIABLE>:append": " ..." }),
            );
        }
    }
    if let Some(m) = machine {
        opts.insert("machine".into(), serde_json::json!(m));
    }
    format!(
        "scaffold: scaffold kind=bbappend name={pn}{} options={}",
        layer_arg(layer),
        serde_json::Value::Object(opts)
    )
}

// ---------------------------------------------------------------------------------------
// Images

fn is_image(cx: &GenContext<'_>, r: &RecipeRow) -> Result<bool> {
    Ok(query::tree(cx.index, r.file.id)?.iter().any(|n| {
        n.kind == "inherit"
            && n.file.as_ref().is_some_and(|f| {
                f.path
                    .file_stem()
                    .is_some_and(|s| helpers::image_class(&s.to_string_lossy()))
            })
    }))
}

fn image_recipes(cx: &GenContext<'_>) -> Result<Vec<RecipeRow>> {
    let mut out: Vec<RecipeRow> = Vec::new();
    for class in ["image", "core-image"] {
        for u in query::inheritors(cx.index, class, true)? {
            if u.file.kind == FileKind::Recipe
                && let (Some(pn), Some(pv)) = (u.file.pn.clone(), u.file.pv.clone())
                && !out.iter().any(|r| r.file.id == u.file.id)
            {
                out.push(RecipeRow {
                    file: u.file,
                    pn,
                    pv,
                });
            }
        }
    }
    Ok(out)
}

fn image_change(
    cx: &GenContext<'_>,
    layers: &Layers<'_>,
    a: &mut Advice,
    recipe_name: Option<&str>,
    machine: Option<&str>,
) -> Result<()> {
    let image_opt = opt_text(cx, "image")?;
    let mut package = opt_text(cx, "package")?.map(str::to_string);
    let mut image: Option<Selection> = None;
    if let Some(i) = image_opt {
        image = Some(find_recipe(cx, i, machine)?);
    } else if let Some(n) = recipe_name {
        let s = find_recipe(cx, n, machine)?;
        if is_image(cx, &s.recipe)? {
            image = Some(s);
        } else if package.is_none() {
            package = Some(s.recipe.pn.clone());
        }
    }
    let pkg = package.unwrap_or_else(|| "<package>".into());
    let local = format!(
        "note: `IMAGE_INSTALL:append = \" {pkg}\"` in local.conf only for a developer-local trial: it is not part of any layer and changes every image (org/recipe-change-in-local-conf)"
    );
    let Some(img) = image else {
        let all = image_recipes(cx)?;
        a.push(format!(
            "where: the image recipe that should contain {pkg} (or a bbappend to it); pass it as `image`"
        ));
        a.push("why: image content belongs to the image (org/policy-in-recipe), not to the package's recipe or local.conf".to_string());
        if all.is_empty() {
            a.push("note: no image recipe (one inheriting image or core-image) is in the indexed layers; an image of your own is a recipe that inherits core-image".to_string());
        } else {
            let shown: Vec<String> = all.iter().take(5).map(recipe_label).collect();
            a.push(format!(
                "note: image recipes: {}{}",
                shown.join(", "),
                if all.len() > 5 {
                    format!(" +{} more", all.len() - 5)
                } else {
                    String::new()
                }
            ));
        }
        a.push(local);
        return Ok(());
    };
    let r = &img.recipe;
    if writable(cx, &r.file.layer) {
        a.push(format!("where: {}", recipe_label(r)));
        a.push(format!(
            "why: image content belongs in the image recipe, and {} is in a writable layer",
            r.pn
        ));
        a.push(format!(
            "by hand: add `IMAGE_INSTALL += \"{pkg}\"` to the recipe"
        ));
    } else {
        let appends = query::appends_for(cx.index, r.file.id)?;
        let mine: Vec<&FileRow> = appends.iter().filter(|f| writable(cx, &f.layer)).collect();
        let layer = layers.pick(
            a,
            &|l: &Layer| mine.iter().any(|f| f.layer == l.name),
            &format!("a bbappend of {}", r.pn),
        );
        match layer.and_then(|l| mine.iter().rev().find(|f| f.layer == l.name)) {
            Some(f) => a.push(format!("where: {} (the existing bbappend)", file_label(f))),
            None => a.push(format!(
                "where: a bbappend in {}: {} (or an image recipe of your own that requires {})",
                layer_name(layer),
                new_append_path(r, false).display(),
                recipe_label(r)
            )),
        }
        a.push(format!(
            "why: image content belongs to the image; {} is in {}, which is not writable",
            recipe_label(r),
            r.file.layer
        ));
        a.push(format!(
            "by hand: `IMAGE_INSTALL:append = \" {pkg}\"` in the bbappend; scaffold kind=bbappend refuses IMAGE_INSTALL in `set` (`scaffold kind=bbappend name={}{} options={{\"set\": ...}}` creates the file for other variables)",
            r.pn,
            layer_arg(layer)
        ));
    }
    a.push(local);
    Ok(())
}

// ---------------------------------------------------------------------------------------
// Machine and distro configuration

fn config_value(cx: &GenContext<'_>, machine: Option<&str>, name: &str) -> Result<Option<String>> {
    let p = Providers::new(cx.index, cx.ws, machine, None)?;
    Ok(p.config()
        .value(name)
        .map(|v| v.text.trim().to_string())
        .filter(|v| !v.is_empty()))
}

fn machine_change(
    cx: &GenContext<'_>,
    layers: &Layers<'_>,
    a: &mut Advice,
    variable: Option<&str>,
    machine: Option<&str>,
) -> Result<()> {
    let var = variable.unwrap_or("MACHINE_FEATURES");
    let m = match machine {
        Some(m) => Some(m.to_string()),
        None => config_value(cx, None, "MACHINE")?,
    };
    let Some(m) = m else {
        return Err(invalid(
            "no MACHINE is configured; pass the machine as the `machine` option",
        ));
    };
    let conf = find_conf(cx, "machine", &m)?;
    match &conf {
        Some(f) if writable(cx, &f.layer) => {
            a.push(format!("where: {}", file_label(f)));
            a.push(format!(
                "why: {} is hardware description; it belongs in the machine conf, which is in a writable layer",
                base_of(var)
            ));
        }
        Some(f) => {
            let layer = layers.pick(
                a,
                &|l: &Layer| l.root.join("conf/machine").is_dir(),
                "machine confs",
            );
            a.push(format!(
                "where: a machine of your own in {}: conf/machine/<new-machine>.conf with `require conf/machine/{m}.conf`, then MACHINE = \"<new-machine>\" in local.conf",
                layer_name(layer)
            ));
            a.push(format!(
                "why: {} is hardware description (machine conf, org/policy-in-recipe); {} is in {}, which is not writable",
                base_of(var),
                file_label(f),
                f.layer
            ));
        }
        None => {
            let layer = layers.pick(a, &|_| false, "machine confs");
            a.push(format!(
                "where: conf/machine/{m}.conf in {}",
                layer_name(layer)
            ));
            a.push(format!(
                "why: {} is hardware description; machine {m} has no conf in the indexed layers",
                base_of(var)
            ));
        }
    }
    a.push(format!(
        "by hand: no scaffold kind writes machine confs; e.g. `{}:append = \" <feature>\"`",
        base_of(var)
    ));
    a.push(format!(
        "note: a value for one recipe on {m} is a `:{m}` override in that recipe's bbappend (scaffold kind=bbappend with the `machine` option); local.conf only for a developer-local trial"
    ));
    Ok(())
}

fn distro_change(
    cx: &GenContext<'_>,
    layers: &Layers<'_>,
    a: &mut Advice,
    intent: Intent,
    variable: Option<&str>,
    recipe_name: Option<&str>,
    machine: Option<&str>,
) -> Result<()> {
    let p = Providers::new(cx.index, cx.ws, machine, None)?;
    // The variable to set, and facts about the current choice.
    let mut facts: Vec<String> = Vec::new();
    let mut to_machine = false;
    let var = match intent {
        Intent::PinVersion => {
            let pn = match (variable, recipe_name) {
                (Some(v), _) => v
                    .strip_prefix("PREFERRED_VERSION_")
                    .map(str::to_string)
                    .ok_or_else(|| invalid(format!("{v} is not PREFERRED_VERSION_<recipe>")))?,
                (None, Some(n)) => n.to_string(),
                (None, None) => {
                    return Err(invalid("pin-version needs the recipe (`name`)"));
                }
            };
            let versions: Vec<String> = query::find_recipes(cx.index, &pn)?
                .iter()
                .map(|r| format!("{} ({})", r.pv, r.file.layer))
                .collect();
            if versions.is_empty() {
                return Err(Error::NotFound {
                    what: format!("recipe {pn} in the workspace"),
                });
            }
            if let Some(s) = p.best_version(&pn, None)? {
                facts.push(format!(
                    "now: {} {} is used ({}{})",
                    pn,
                    s.recipe.pv,
                    s.describe(),
                    s.setting
                        .as_ref()
                        .and_then(|x| x.loc.as_ref())
                        .map(|l| format!(", {l}"))
                        .unwrap_or_default()
                ));
            }
            facts.push(format!("note: versions of {pn}: {}", versions.join(", ")));
            format!("PREFERRED_VERSION_{pn}")
        }
        Intent::ChangeProvider => {
            let item = match (variable, opt_text(cx, "item")?) {
                (_, Some(i)) => i.to_string(),
                (Some(v), None) => v
                    .strip_prefix("PREFERRED_PROVIDER_")
                    .map(str::to_string)
                    .ok_or_else(|| invalid(format!("{v} is not PREFERRED_PROVIDER_<name>")))?,
                (None, None) => {
                    return Err(invalid(
                        "change-provider needs the provided name: the `item` option (virtual/kernel, ...)",
                    ));
                }
            };
            to_machine = machine_provider(&item);
            let set = p.providers(&item)?;
            let all: Vec<&str> = set.providers.iter().map(|x| x.pn.as_str()).collect();
            if all.is_empty() {
                facts.push(format!(
                    "note: no recipe in the indexed layers provides {item}"
                ));
            } else {
                facts.push(format!(
                    "now: {item} comes from {}{} (providers: {})",
                    all[0],
                    set.preferred_provider
                        .as_ref()
                        .and_then(|s| s.loc.as_ref())
                        .map(|l| format!(", set at {l}"))
                        .unwrap_or_default(),
                    all.join(", ")
                ));
            }
            if let Some(r) = recipe_name
                && !all.contains(&r)
            {
                facts.push(format!("note: {r} does not provide {item}"));
            }
            format!("PREFERRED_PROVIDER_{item}")
        }
        _ => {
            let v = variable.unwrap_or("DISTRO_FEATURES").to_string();
            if let Some((loc, lhs)) = set_at(p.config(), &v) {
                facts.push(format!(
                    "now: {} is set last at {loc} (`{lhs}`)",
                    base_of(&v)
                ));
            }
            v
        }
    };
    let (sub, name, kind) = if to_machine {
        let m = match machine {
            Some(m) => Some(m.to_string()),
            None => config_value(cx, None, "MACHINE")?,
        };
        ("machine", m, "machine")
    } else {
        ("distro", config_value(cx, machine, "DISTRO")?, "distro")
    };
    let Some(name) = name else {
        let layer = layers.pick(a, &|_| false, &format!("{kind} confs"));
        a.push(format!(
            "where: the {kind} conf (conf/{sub}/<{kind}>.conf) in {}",
            layer_name(layer)
        ));
        a.push(format!(
            "why: {var} is {kind} policy; no {} is configured",
            kind.to_uppercase()
        ));
        a.push(format!("by hand: no scaffold kind writes {kind} confs"));
        a.extend_facts(facts);
        return Ok(());
    };
    let conf = find_conf(cx, sub, &name)?;
    let policy = if to_machine {
        format!("{var} picks hardware support: by OE convention it is set in the machine conf")
    } else {
        format!(
            "{var} is distro policy (org/policy-in-recipe): it belongs in the distro conf, not in a recipe or bbappend"
        )
    };
    match &conf {
        Some(f) if writable(cx, &f.layer) => {
            a.push(format!("where: {}", file_label(f)));
            a.push(format!("why: {policy}"));
        }
        Some(f) => {
            let layer = layers.pick(
                a,
                &|l: &Layer| l.root.join("conf").join(sub).is_dir(),
                &format!("{kind} confs"),
            );
            a.push(format!(
                "where: a {kind} of your own in {}: conf/{sub}/<new-{kind}>.conf with `require conf/{sub}/{name}.conf`, then {} = \"<new-{kind}>\" in local.conf",
                layer_name(layer),
                kind.to_uppercase()
            ));
            a.push(format!(
                "why: {policy}; {} is in {}, which is not writable",
                file_label(f),
                f.layer
            ));
        }
        None => {
            let layer = layers.pick(a, &|_| false, &format!("{kind} confs"));
            a.push(format!(
                "where: conf/{sub}/{name}.conf in {}",
                layer_name(layer)
            ));
            a.push(format!(
                "why: {policy}; {kind} {name} has no conf in the indexed layers"
            ));
        }
    }
    let line = if var.ends_with("_FEATURES") {
        format!("{var}:append = \" <feature>\"")
    } else {
        format!("{var} = \"...\"")
    };
    a.push(format!(
        "by hand: no scaffold kind writes {kind} confs; add `{line}` there"
    ));
    a.extend_facts(facts);
    a.push(format!(
        "note: `{var}` in local.conf only for a developer-local trial: local.conf is not part of any layer"
    ));
    Ok(())
}

impl Advice {
    fn extend_facts(&mut self, facts: Vec<String>) {
        self.notes.extend(facts);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base_strips_overrides_and_flags() {
        assert_eq!(base_of("EXTRA_OECONF:append:foo"), "EXTRA_OECONF");
        assert_eq!(base_of("SRC_URI[sha256sum]"), "SRC_URI");
        assert_eq!(
            base_of("PREFERRED_PROVIDER_virtual/kernel"),
            "PREFERRED_PROVIDER_virtual/kernel"
        );
    }

    #[test]
    fn new_append_mirrors_the_recipe_directory() {
        let r = RecipeRow {
            file: FileRow {
                id: 1,
                layer: "meta-core".into(),
                layer_id: 1,
                path: "recipes-core/hello/hello_1.1.bb".into(),
                kind: FileKind::Recipe,
                sha256: String::new(),
                pn: Some("hello".into()),
                pv: Some("1.1".into()),
                priority: 5,
            },
            pn: "hello".into(),
            pv: "1.1".into(),
        };
        assert_eq!(
            new_append_path(&r, true),
            PathBuf::from("recipes-core/hello/hello_1.1.bbappend")
        );
        assert_eq!(
            new_append_path(&r, false),
            PathBuf::from("recipes-core/hello/hello_%.bbappend")
        );
    }
}
