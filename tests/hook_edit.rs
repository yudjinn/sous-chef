//! T7.4: after Edit / Write / MultiEdit the hook reindexes the file, lints it with the
//! file-scope rules and adds findings at warning level or above; an edit in a layer that is
//! not writable gets one line suggesting a bbappend.
//!
//! Events are built from the recorded `edit.json` / `write.json` (hand-written in the
//! documented shape, see `plugin/README.md`), pointed at a temporary copy of the fixture
//! workspace, whose file is changed on disk first as the tool would have.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;
mod hook_support;

use std::fs;
use std::path::{Path, PathBuf};
use std::time::Instant;

use hook_support::*;
use serde_json::Value;
use sous_chef::index::{Index, query};

struct Fx {
    _tmp: tempfile::TempDir,
    root: PathBuf,
    cache: tempfile::TempDir,
}

fn fx() -> Fx {
    let (tmp, root) = common::temp_ws();
    let root = fs::canonicalize(root).unwrap();
    let cache = indexed_cache(&root);
    Fx {
        _tmp: tmp,
        root,
        cache,
    }
}

impl Fx {
    /// Writes `content` to `rel` (as the tool did) and runs the hook with a `tool` event for
    /// it (`Edit`, `Write` or `MultiEdit`).
    fn edit(&self, tool: &str, rel: &str, content: &str) -> std::process::Output {
        let abs = self.root.join(rel);
        fs::create_dir_all(abs.parent().unwrap()).unwrap();
        fs::write(&abs, content).unwrap();
        let template = if tool == "Write" { "write" } else { "edit" };
        let mut ev: Value = serde_json::from_str(&event_in(template, &self.root)).unwrap();
        let path = Value::String(abs.to_str().unwrap().to_string());
        ev["tool_name"] = Value::String(tool.into());
        ev["tool_input"]["file_path"] = path.clone();
        ev["tool_response"]["filePath"] = path;
        if tool == "Write" {
            ev["tool_input"]["content"] = Value::String(content.into());
            ev["tool_response"]["content"] = Value::String(content.into());
        }
        run_hook(ev.to_string().as_bytes(), self.cache.path())
    }

    fn index(&self) -> Index {
        let ws = discover(&self.root);
        Index::open_in(self.cache.path(), &ws).unwrap()
    }
}

const NEW: &str = "meta-mine/recipes-app/tool/tool_1.0.bb";

/// A recipe with `n` host paths in do_install (each a hygiene/host-path warning).
fn host_paths(n: usize) -> String {
    let mut s = String::from(
        "SUMMARY = \"tool\"\nLICENSE = \"MIT\"\n\ndo_install() {\n\tinstall -d ${D}${bindir}\n",
    );
    for i in 0..n {
        s.push_str(&format!("\tinstall -m 0644 f{i} /etc/f{i}\n"));
    }
    s.push_str("}\n");
    s
}

#[test]
fn a_write_that_introduces_a_finding_yields_it_and_reindexes_the_file() {
    let f = fx();
    let ctx = context_of(&f.edit("Write", NEW, &host_paths(1)));
    assert_eq!(
        ctx,
        "sous-chef lint of meta-mine:recipes-app/tool/tool_1.0.bb after this change (1 at \
         warning or above):\n\
         warning hygiene/host-path meta-mine:recipes-app/tool/tool_1.0.bb:6 absolute host \
         path `/etc` in do_install: it names the build host, not the target; write \
         `${D}${sysconfdir}` when installing (or `${sysconfdir}` / \
         `${STAGING_DIR_HOST}${sysconfdir}` when reading)"
    );
    // The new file is in the index now, with its statements.
    let index = f.index();
    let row = query::file_by_path(
        &index,
        "meta-mine",
        Path::new("recipes-app/tool/tool_1.0.bb"),
    )
    .unwrap()
    .unwrap();
    let names: Vec<String> = query::statements_in(&index, row.id, &Default::default())
        .unwrap()
        .into_iter()
        .filter_map(|s| s.name)
        .collect();
    assert_eq!(names, ["SUMMARY", "LICENSE", "do_install"]);
}

#[test]
fn an_edit_of_an_indexed_file_is_reindexed_and_linted() {
    let f = fx();
    let rel = "meta-mine/recipes-core/hello/hello_1.1.bbappend";
    let mut text = fs::read_to_string(f.root.join(rel)).unwrap();
    text.push_str("SRCREV = \"${AUTOREV}\"\n");
    let ctx = context_of(&f.edit("Edit", rel, &text));
    let lines: Vec<&str> = ctx.lines().collect();
    assert_eq!(lines.len(), 2, "{ctx}");
    assert!(
        lines[1].starts_with(
            "warning hygiene/autorev meta-mine:recipes-core/hello/hello_1.1.bbappend:11 \
             `SRCREV` follows the branch head"
        ),
        "{ctx}"
    );
    let index = f.index();
    let row = query::file_by_path(
        &index,
        "meta-mine",
        Path::new("recipes-core/hello/hello_1.1.bbappend"),
    )
    .unwrap()
    .unwrap();
    assert!(
        query::statement_by_name(&index, row.id, "SRCREV")
            .unwrap()
            .iter()
            .any(|s| s.loc.lines.start == 11)
    );
}

#[test]
fn a_clean_edit_yields_nothing() {
    let f = fx();
    let rel = "meta-mine/recipes-core/hello/hello_1.1.bbappend";
    let text = fs::read_to_string(f.root.join(rel))
        .unwrap()
        .replace("--with-mine", "--with-mine --with-more");
    assert_noop(&f.edit("Edit", rel, &text));
    // Info findings are not reported: a recipe without HOMEPAGE is only info.
    assert_noop(&f.edit("Write", NEW, &host_paths(0)));
    // Nor are files that are not BitBake metadata.
    assert_noop(&f.edit("Write", "meta-mine/recipes-app/app/files/app.c", "int x;\n"));
}

#[test]
fn more_than_five_findings_are_cut_with_a_count() {
    let f = fx();
    let ctx = context_of(&f.edit("MultiEdit", NEW, &host_paths(7)));
    let lines: Vec<&str> = ctx.lines().collect();
    assert_eq!(lines.len(), 7, "{ctx}");
    assert!(lines[0].ends_with("(7 at warning or above):"), "{ctx}");
    assert!(
        lines[1..6]
            .iter()
            .all(|l| l.starts_with("warning hygiene/host-path "))
    );
    assert_eq!(
        lines[6],
        "+2 more (`lint meta-mine:recipes-app/tool/tool_1.0.bb` lists them)"
    );
}

#[test]
fn an_edit_in_an_upstream_layer_yields_the_warning() {
    let f = fx();
    let rel = "meta-core/recipes-core/hello/hello.inc";
    let text = fs::read_to_string(f.root.join(rel))
        .unwrap()
        .replace("--disable-nls", "--disable-nls --enable-x");
    let ctx = context_of(&f.edit("Edit", rel, &text));
    assert_eq!(
        ctx,
        "meta-core is not a writable layer: this edits upstream metadata, which an update of \
         the layer overwrites; put the change in a bbappend in a writable layer \
         (`scaffold kind=where` says where)."
    );
}

#[test]
fn disabled_rules_and_suppressions_are_honoured() {
    let f = fx();
    // A finding inside a statement is suppressed by a comment above the statement.
    let suppressed = host_paths(1).replace(
        "do_install() {",
        "# sous-chef: allow hygiene/host-path\ndo_install() {",
    );
    assert_noop(&f.edit("Write", NEW, &suppressed));

    let toml = f.root.join("sous-chef.toml");
    let mut text = fs::read_to_string(&toml).unwrap();
    text.push_str("\n[lint]\ndisable = [\"hygiene/host-path\"]\n");
    fs::write(&toml, text).unwrap();
    assert_noop(&f.edit("Write", NEW, &host_paths(2)));
}

#[test]
fn a_locked_index_fails_open() {
    let f = fx();
    let db = sous_chef::index::cache_dir_in(f.cache.path(), &discover(&f.root))
        .join(sous_chef::index::INDEX_FILE);
    let lock = rusqlite::Connection::open(&db).unwrap();
    lock.execute_batch("BEGIN IMMEDIATE").unwrap();
    let out = f.edit("Write", NEW, &host_paths(1));
    lock.execute_batch("ROLLBACK").unwrap();
    assert_noop(&out);
    assert!(log_text(f.cache.path(), Some(&f.root)).contains("edit"));
}

#[test]
fn an_edit_introducing_legacy_syntax_yields_the_finding() {
    let f = fx();
    let rel = "meta-mine/recipes-core/hello/hello_1.1.bbappend";
    let mut text = fs::read_to_string(f.root.join(rel)).unwrap();
    text.push_str("EXTRA_OECONF_append = \" --legacy\"\n");
    let ctx = context_of(&f.edit("Edit", rel, &text));
    assert!(
        ctx.contains(
            "error override/legacy-syntax meta-mine:recipes-core/hello/hello_1.1.bbappend:11 "
        ),
        "{ctx}"
    );
    assert!(ctx.contains("write `EXTRA_OECONF:append`"), "{ctx}");
}

#[test]
#[ignore = "timing; run with --ignored --nocapture"]
fn timing() {
    let f = fx();
    let n = 20;
    let start = Instant::now();
    for i in 0..n {
        // A different file content each time, so every call reindexes.
        let out = f.edit("Write", NEW, &host_paths(1 + i % 3));
        context_of(&out);
    }
    println!(
        "edit hook, reindex + lint one file: {:.1} ms per call",
        start.elapsed().as_secs_f64() * 1000.0 / f64::from(n as u32)
    );
}
