//! Scaffold generator: `layer` (T6.3).
//!
//! Creates `<path>/<name>/` with `conf/layer.conf` and `README.md` (plus `COPYING.MIT` and
//! an empty `recipes-example/` when asked), and adds the layer to `BBLAYERS` in the build
//! directory's `conf/bblayers.conf` as a second, separate change of the same plan. The
//! In a kas project (`ws.kas` with files, generating the build directory if there is one),
//! the layer is added to a kas file instead (`workspace::kas::plan_add_layer`: one line
//! inserted under `layers` of the own repository, comments and order kept), or, when no
//! file can be edited safely, the lines to add are given in the notes. The
//! tool's `name` is the layer directory (`meta-...`), which is also the collection name, as
//! `bitbake-layers create-layer` makes it. Options: [`OPTIONS`].
//!
//! - `path`: the directory to create the layer in (absolute, or relative to the working
//!   directory). Default: the directory holding the first writable layer, else the first
//!   layer. The engine allows a new layer only in a directory that already holds a
//!   workspace layer, outside every existing layer and the build directory.
//! - `priority`: `BBFILE_PRIORITY`, default 6 (the `create-layer` default).
//! - `depends`: `LAYERDEPENDS` collections, default `core` when a layer provides it.
//! - `compat`: `LAYERSERIES_COMPAT`, default the core layer's own `LAYERSERIES_COMPAT`
//!   (else the workspace release).
//! - `copying`: also write `COPYING.MIT`; `example`: also create `recipes-example/` (as
//!   `recipes-example/.gitkeep`, since a plan holds files only).

use std::path::{Path, PathBuf};

use super::recipe::{check_parses, invalid, opt_text, opt_words, quote};
use super::{FileAction, GenContext, Generator, Kind, Plan, PlannedFile, file_sha256};
use crate::error::Result;
use crate::parse::{StatementKind, parse};
use crate::release::Release;
use crate::types::AssignOp;
use crate::workspace::Workspace;
use crate::workspace::kas::{self, LayerEdit};

/// Every option `layer` accepts.
pub const OPTIONS: &[&str] = &[
    "path", "priority", "depends", "compat", "copying", "example",
];

pub struct LayerGen;

impl Generator for LayerGen {
    fn kind(&self) -> Kind {
        Kind::Layer
    }

    fn options(&self) -> &'static [&'static str] {
        OPTIONS
    }

    fn plan(&self, cx: &GenContext<'_>) -> Result<Plan> {
        plan(cx)
    }
}

/// The text OE-core ships as `COPYING.MIT` (the MIT license without a copyright line).
const MIT: &str = "Permission is hereby granted, free of charge, to any person obtaining a copy
of this software and associated documentation files (the \"Software\"), to deal
in the Software without restriction, including without limitation the rights
to use, copy, modify, merge, publish, distribute, sublicense, and/or sell
copies of the Software, and to permit persons to whom the Software is
furnished to do so, subject to the following conditions:

The above copyright notice and this permission notice shall be included in
all copies or substantial portions of the Software.

THE SOFTWARE IS PROVIDED \"AS IS\", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM,
OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN
THE SOFTWARE.
";

/// A boolean option (`true` / `false`, or those words as strings), default false.
fn opt_bool(cx: &GenContext<'_>, key: &str) -> Result<bool> {
    match cx.request.options.get(key) {
        None | Some(serde_json::Value::Null) => Ok(false),
        Some(serde_json::Value::Bool(b)) => Ok(*b),
        Some(serde_json::Value::String(s)) if s == "true" || s == "false" => Ok(s == "true"),
        Some(_) => Err(invalid(format!("option `{key}` must be true or false"))),
    }
}

fn priority(cx: &GenContext<'_>) -> Result<i64> {
    let bad = || invalid("option `priority` must be a whole number (BBFILE_PRIORITY)");
    match cx.request.options.get("priority") {
        None | Some(serde_json::Value::Null) => Ok(6),
        Some(serde_json::Value::Number(n)) => n.as_i64().ok_or_else(bad),
        Some(serde_json::Value::String(s)) => s.trim().parse().map_err(|_| bad()),
        Some(_) => Err(bad()),
    }
}

fn check_name(name: &str) -> Result<()> {
    let rest = name.strip_prefix("meta-").unwrap_or("");
    let ok = !rest.is_empty()
        && rest
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-');
    if ok {
        Ok(())
    } else {
        Err(invalid(format!(
            "layer name {name:?} must be meta-<name> in lower-case letters, digits and `-`"
        )))
    }
}

/// Where the new layer goes by default: beside the first writable layer, else the first.
fn default_parent(ws: &Workspace) -> Result<PathBuf> {
    ws.layers
        .iter()
        .find(|l| l.writable)
        .or_else(|| ws.layers.first())
        .and_then(|l| l.root.parent())
        .map(Path::to_path_buf)
        .ok_or_else(|| {
            invalid("the workspace has no layer to create a new one beside; pass `path`")
        })
}

fn plan(cx: &GenContext<'_>) -> Result<Plan> {
    let ws = cx.ws;
    let name = cx
        .request
        .name
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| invalid("layer needs `name`, e.g. meta-mycompany"))?;
    check_name(name)?;
    if let Some(l) = ws
        .layers
        .iter()
        .find(|l| l.name == name || l.collections.iter().any(|c| c == name))
    {
        return Err(invalid(format!(
            "layer {} already uses the name or collection {name}",
            l.name
        )));
    }
    let parent = match opt_text(cx, "path")? {
        Some(p) => {
            let p = cx.config.work_dir.join(p);
            std::fs::canonicalize(&p).map_err(|_| {
                invalid(format!(
                    "`path` {} is not an existing directory",
                    p.display()
                ))
            })?
        }
        None => default_parent(ws)?,
    };
    let root = parent.join(name);
    let mut notes = vec![format!(
        "the layer is created in {}: a new layer must go in a directory that already holds a workspace layer (or the one `path` names), outside every existing layer",
        parent.display()
    )];

    let priority = priority(cx)?;
    let core = ws
        .layers
        .iter()
        .find(|l| l.collections.iter().any(|c| c == "core"));
    let depends = {
        let given = opt_words(cx, "depends")?;
        if given.is_empty() && !cx.request.options.contains_key("depends") {
            core.map(|_| vec!["core".to_string()]).unwrap_or_default()
        } else {
            given
        }
    };
    for d in &depends {
        let dep = d.split_whitespace().next().unwrap_or(d);
        if !ws
            .layers
            .iter()
            .any(|l| l.collections.iter().any(|c| c == dep))
        {
            notes.push(format!(
                "LAYERDEPENDS names {dep}, which no layer of the workspace provides"
            ));
        }
    }
    let compat = {
        let given = opt_words(cx, "compat")?;
        if !given.is_empty() {
            given
        } else {
            match core.filter(|c| !c.compat.is_empty()) {
                Some(c) => c.compat.clone(),
                None => {
                    notes.push(format!(
                        "no core layer with LAYERSERIES_COMPAT: compat is the workspace release ({})",
                        ws.release.release.codename()
                    ));
                    vec![ws.release.release.codename().to_string()]
                }
            }
        }
    };
    for c in &compat {
        if Release::from_codename(c).is_none() {
            notes.push(if Release::is_older_than_floor(c) {
                format!("LAYERSERIES_COMPAT names {c}, which is older than sous-chef supports")
            } else {
                format!("LAYERSERIES_COMPAT names {c}, which sous-chef does not know")
            });
        }
    }

    let layer_conf = layer_conf(name, priority, &depends, &compat)?;
    check_parses("conf/layer.conf", &layer_conf)?;
    let mut files = vec![
        PlannedFile {
            path: root.join("conf").join("layer.conf"),
            content: layer_conf,
            action: FileAction::Create,
        },
        PlannedFile {
            path: root.join("README.md"),
            content: readme(name, &depends, &compat),
            action: FileAction::Create,
        },
    ];
    if opt_bool(cx, "copying")? {
        files.push(PlannedFile {
            path: root.join("COPYING.MIT"),
            content: MIT.to_string(),
            action: FileAction::Create,
        });
    }
    if opt_bool(cx, "example")? {
        files.push(PlannedFile {
            path: root.join("recipes-example").join(".gitkeep"),
            content: String::new(),
            action: FileAction::Create,
        });
    }

    let kas = ws
        .kas
        .as_ref()
        .filter(|k| !k.files.is_empty() && ws.build_dir.as_ref().is_none_or(|b| k.generates(b)));
    match (kas, &ws.build_dir) {
        (Some(k), _) => {
            let shown = |f: &Path| f.strip_prefix(&k.root).unwrap_or(f).display().to_string();
            match kas::plan_add_layer(k, &root) {
                LayerEdit::Modify { file, content } => {
                    notes.push(format!(
                        "this is a kas project: the layer is added to {} (a separate change in this plan), not to bblayers.conf, which kas generates; run kas checkout {} to regenerate bblayers.conf",
                        shown(&file),
                        k.files_arg(&k.work_dir)
                    ));
                    files.push(PlannedFile {
                        action: FileAction::Modify {
                            before_sha: file_sha256(&file)?,
                        },
                        path: file,
                        content,
                    });
                }
                LayerEdit::Manual { lines, why } => {
                    let target = k
                        .files
                        .first()
                        .map(|f| shown(f))
                        .unwrap_or_else(|| "your kas file".into());
                    notes.push(format!(
                        "this is a kas project, so bblayers.conf (which kas generates) is left alone, but no kas file was changed: {why}; add these lines to {target} (merging with its repos), then run kas checkout:\n{lines}"
                    ));
                }
            }
        }
        (None, Some(build)) => {
            let conf = build.join("conf").join("bblayers.conf");
            let old = std::fs::read_to_string(ws.confine(&conf)?)?;
            let entry = match build.parent().and_then(|b| root.strip_prefix(b).ok()) {
                Some(rel) => format!("${{TOPDIR}}/../{}", rel.display()),
                None => root.display().to_string(),
            };
            let content = add_to_bblayers(&old, &entry)?;
            check_parses("conf/bblayers.conf", &content)?;
            files.push(PlannedFile {
                path: conf.clone(),
                content,
                action: FileAction::Modify {
                    before_sha: file_sha256(&conf)?,
                },
            });
            notes.push(
                "BBLAYERS in build:conf/bblayers.conf gets the new layer (a separate change in this plan)".into(),
            );
        }
        (None, None) => notes.push(format!(
            "no build directory: add {} to BBLAYERS of your build, or to `layers` in sous-chef.toml",
            root.display()
        )),
    }
    notes.push(format!(
        "to scaffold into {name} later, list it in writable_layers in sous-chef.toml; sous-chef sees the layer once the workspace is discovered again (restart the server)"
    ));
    notes.push(
        "the new layer's files are not linted after the write: the layer is not part of the loaded workspace yet".into(),
    );
    Ok(Plan {
        files,
        notes,
        new_layer: Some(root),
    })
}

/// `bblayers.conf` with `entry` added to `BBLAYERS`:
/// inside the last plain assignment of `BBLAYERS` (a new continuation line before a closing
/// quote that stands on its own line, else a word before the closing quote). Without such
/// an assignment, `BBLAYERS += "<entry>"` is appended.
fn add_to_bblayers(old: &str, entry: &str) -> Result<String> {
    let last = parse(old)
        .statements
        .into_iter()
        .rev()
        .find(|s| {
            matches!(&s.kind, StatementKind::Assign { var, op, .. }
                if var.base == "BBLAYERS" && var.overrides.is_empty() && var.flag.is_none()
                    && matches!(op, AssignOp::Set | AssignOp::Default | AssignOp::WeakDefault | AssignOp::Append))
        });
    if let Some(stmt) = last
        && let Some(text) = old.get(stmt.bytes.clone())
        && let Some(q) = text.trim_end().rfind('"')
    {
        let at = stmt.bytes.start + q;
        let line_start = old[..at].rfind('\n').map(|i| i + 1).unwrap_or(0);
        let mut out = String::with_capacity(old.len() + entry.len() + 8);
        if old[line_start..at].trim().is_empty() && line_start > stmt.bytes.start {
            // `  "` on its own line: add `  <entry> \` above it.
            out.push_str(&old[..line_start]);
            out.push_str(&format!("  {entry} \\\n"));
            out.push_str(&old[line_start..]);
        } else {
            out.push_str(old[..at].trim_end_matches(' '));
            out.push_str(&format!(" {entry}"));
            out.push_str(&old[at..]);
        }
        return Ok(out);
    }
    let mut out = old.to_string();
    if !out.is_empty() && !out.ends_with('\n') {
        out.push('\n');
    }
    out.push_str(&format!("BBLAYERS += {}\n", quote(entry)?));
    Ok(out)
}

/// `conf/layer.conf`, as the `bitbake-layers create-layer` template lays it out.
fn layer_conf(c: &str, priority: i64, depends: &[String], compat: &[String]) -> Result<String> {
    let mut out = String::new();
    out.push_str("# We have a conf and classes directory, add to BBPATH\n");
    out.push_str("BBPATH .= \":${LAYERDIR}\"\n\n");
    out.push_str("# We have recipes-* directories, add to BBFILES\n");
    out.push_str("BBFILES += \"${LAYERDIR}/recipes-*/*/*.bb \\\n");
    out.push_str("            ${LAYERDIR}/recipes-*/*/*.bbappend\"\n\n");
    out.push_str(&format!("BBFILE_COLLECTIONS += \"{c}\"\n"));
    out.push_str(&format!("BBFILE_PATTERN_{c} = \"^${{LAYERDIR}}/\"\n"));
    out.push_str(&format!("BBFILE_PRIORITY_{c} = \"{priority}\"\n\n"));
    out.push_str(&format!(
        "LAYERDEPENDS_{c} = {}\n",
        quote(&depends.join(" "))?
    ));
    out.push_str(&format!(
        "LAYERSERIES_COMPAT_{c} = {}\n",
        quote(&compat.join(" "))?
    ));
    Ok(out)
}

fn readme(name: &str, depends: &[String], compat: &[String]) -> String {
    let mut out = format!("# {name}\n\n");
    out.push_str(&format!(
        "Compatible with the Yocto Project series: {}.\n\n",
        compat.join(", ")
    ));
    out.push_str("## Dependencies\n\n");
    if depends.is_empty() {
        out.push_str("None.\n\n");
    } else {
        for d in depends {
            out.push_str(&format!("- `{d}`\n"));
        }
        out.push('\n');
    }
    out.push_str("## Adding the layer to a build\n\n");
    out.push_str(&format!(
        "Run `bitbake-layers add-layer <path to {name}>`, or add its path to `BBLAYERS` in\n`conf/bblayers.conf`.\n"
    ));
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn add(old: &str) -> Option<String> {
        add_to_bblayers(old, "/b").ok()
    }

    #[test]
    fn bblayers_entry_goes_inside_the_assignment() {
        assert_eq!(
            add("BBLAYERS ?= \" \\\n  /a \\\n  \"\n").as_deref(),
            Some("BBLAYERS ?= \" \\\n  /a \\\n  /b \\\n  \"\n")
        );
        assert_eq!(
            add("BBPATH = \"${TOPDIR}\"\nBBLAYERS = \"/a /c\"\n").as_deref(),
            Some("BBPATH = \"${TOPDIR}\"\nBBLAYERS = \"/a /c /b\"\n")
        );
        assert_eq!(
            add("BBLAYERS ?= \"/a \\\n  /c\"\n").as_deref(),
            Some("BBLAYERS ?= \"/a \\\n  /c /b\"\n")
        );
        assert_eq!(
            add("BBPATH = \"x\"").as_deref(),
            Some("BBPATH = \"x\"\nBBLAYERS += \"/b\"\n")
        );
        // Only a plain assignment is edited, the last one.
        assert_eq!(
            add("BBLAYERS = \"/a\"\nBBLAYERS:append = \" /z\"\n").as_deref(),
            Some("BBLAYERS = \"/a /b\"\nBBLAYERS:append = \" /z\"\n")
        );
    }
}
