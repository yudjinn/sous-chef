//! T2.2: scanner and freshness (`Index::refresh`) over `tests/fixtures/ws`.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime};

use common::*;
use rusqlite::Connection;
use sous_chef::index::Index;
use sous_chef::workspace::Workspace;

fn count(conn: &Connection, sql: &str) -> i64 {
    conn.query_row(sql, [], |r| r.get(0)).unwrap()
}

/// The 21 metadata files of the fixture workspace, as `layer:path`.
const FIXTURE_FILES: &[&str] = &[
    "build:conf/bblayers.conf",
    "build:conf/local.conf",
    "meta-core:classes-global/sanityish.bbclass",
    "meta-core:classes-recipe/autotoolsish.bbclass",
    "meta-core:classes/base.bbclass",
    "meta-core:conf/bitbake.conf",
    "meta-core:conf/distro/testdistro.conf",
    "meta-core:conf/layer.conf",
    "meta-core:conf/machine/boardx.conf",
    "meta-core:conf/machine/qemux86.conf",
    "meta-core:recipes-core/hello/hello.inc",
    "meta-core:recipes-core/hello/hello_1.0.bb",
    "meta-core:recipes-core/hello/hello_1.1.bb",
    "meta-core:recipes-core/libfoo/libfoo_2.0.bb",
    "meta-mine:conf/layer.conf",
    "meta-mine:recipes-app/app/app_0.1.bb",
    "meta-mine:recipes-app/orphan/orphan_9.%.bbappend",
    "meta-mine:recipes-core/hello/hello_1.1.bbappend",
    "meta-vendor:conf/layer.conf",
    "meta-vendor:recipes-core/hello/hello_%.bbappend",
    "meta-vendor:recipes-vendor/libfoo-alt/libfoo-alt_1.0.bb",
];

fn indexed_files(conn: &Connection) -> Vec<String> {
    let mut st = conn
        .prepare(
            "SELECT l.name || ':' || f.path FROM files f JOIN layers l ON l.id = f.layer_id \
             ORDER BY 1",
        )
        .unwrap();
    st.query_map([], |r| r.get(0))
        .unwrap()
        .collect::<rusqlite::Result<_>>()
        .unwrap()
}

fn abs(ws: &Workspace, lp: &str) -> PathBuf {
    let (layer, path) = lp.split_once(':').unwrap();
    let root = if layer == "build" {
        ws.build_dir.clone().unwrap()
    } else {
        ws.layer(layer).unwrap().root.clone()
    };
    root.join(path)
}

fn statements_expected(ws: &Workspace) -> i64 {
    FIXTURE_FILES
        .iter()
        .map(|f| {
            sous_chef::parse::parse(&fs::read_to_string(abs(ws, f)).unwrap())
                .statements
                .len() as i64
        })
        .sum()
}

/// `(src, kind, target_text, target)` with files as `layer:path`.
fn edges(conn: &Connection) -> Vec<(String, String, String, Option<String>)> {
    let mut st = conn
        .prepare(
            "SELECT ls.name || ':' || fs.path, e.kind, e.target_text, lt.name || ':' || ft.path \
             FROM edges e JOIN files fs ON fs.id = e.src_file JOIN layers ls ON ls.id = fs.layer_id \
             LEFT JOIN files ft ON ft.id = e.target_file LEFT JOIN layers lt ON lt.id = ft.layer_id \
             ORDER BY 1, e.line, 3, 4",
        )
        .unwrap();
    st.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))
        .unwrap()
        .collect::<rusqlite::Result<_>>()
        .unwrap()
}

fn fresh_index(ws: &Workspace) -> (tempfile::TempDir, Index) {
    let cache = tempfile::tempdir().unwrap();
    let idx = Index::open_in(cache.path(), ws).unwrap();
    (cache, idx)
}

#[test]
fn first_scan_row_counts() {
    let ws = fixture_ws();
    let (_c, mut idx) = fresh_index(&ws);
    let r = idx.refresh(&ws).unwrap();
    assert_eq!(r.scanned, 21);
    assert_eq!(r.reindexed.len(), 21);
    assert_eq!(r.removed, 0);
    let conn = idx.conn();
    assert_eq!(indexed_files(conn), FIXTURE_FILES);
    assert_eq!(count(conn, "SELECT count(*) FROM layers"), 4);
    let kinds = |k: &str| {
        count(
            conn,
            &format!("SELECT count(*) FROM files WHERE kind = '{k}'"),
        )
    };
    assert_eq!(
        [
            kinds("bb"),
            kinds("bbappend"),
            kinds("inc"),
            kinds("bbclass"),
            kinds("conf")
        ],
        [5, 3, 1, 3, 9]
    );
    assert_eq!(
        count(conn, "SELECT count(*) FROM statements"),
        statements_expected(&ws)
    );
    assert_eq!(
        count(conn, "SELECT count(*) FROM statements_fts"),
        statements_expected(&ws)
    );
    assert_eq!(count(conn, "SELECT count(*) FROM tasks"), 9);
    assert_eq!(count(conn, "SELECT count(*) FROM provides"), 7);
    assert_eq!(count(conn, "SELECT count(*) FROM depends"), 7);
    assert_eq!(count(conn, "SELECT count(*) FROM edges"), 14);
    assert_eq!(
        count(
            conn,
            "SELECT count(*) FROM edges WHERE target_file IS NOT NULL"
        ),
        9
    );
}

#[test]
fn first_scan_rows_are_right() {
    let ws = fixture_ws();
    let (_c, mut idx) = fresh_index(&ws);
    idx.refresh(&ws).unwrap();
    let conn = idx.conn();

    // pn / pv from file names.
    let pnpv: Vec<(String, Option<String>, Option<String>)> = conn
        .prepare("SELECT path, pn, pv FROM files WHERE kind IN ('bb', 'bbappend') ORDER BY path")
        .unwrap()
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
        .unwrap()
        .collect::<rusqlite::Result<_>>()
        .unwrap();
    let get = |p: &str| pnpv.iter().find(|x| x.0 == p).cloned().unwrap();
    assert_eq!(
        get("recipes-core/hello/hello_1.1.bb"),
        (
            "recipes-core/hello/hello_1.1.bb".into(),
            Some("hello".into()),
            Some("1.1".into())
        )
    );
    assert_eq!(
        get("recipes-app/orphan/orphan_9.%.bbappend").2.as_deref(),
        Some("9.%")
    );

    // A statement row, full value, overrides and FTS.
    let row: (String, String, String, Option<String>, String, String, i64, i64, String) = conn
        .query_row(
            "SELECT s.kind, s.name, s.base, s.overrides, s.op, s.value, s.start_line, s.end_line, s.snippet \
             FROM statements s JOIN files f ON f.id = s.file_id \
             WHERE f.path = 'recipes-core/hello/hello_%.bbappend' AND s.start_line = 9",
            [],
            |r| {
                Ok((
                    r.get(0)?,
                    r.get(1)?,
                    r.get(2)?,
                    r.get(3)?,
                    r.get(4)?,
                    r.get(5)?,
                    r.get(6)?,
                    r.get(7)?,
                    r.get(8)?,
                ))
            },
        )
        .unwrap();
    assert_eq!(
        row,
        (
            "assign".into(),
            "EXTRA_OECONF:append:boradx".into(),
            "EXTRA_OECONF".into(),
            Some("append:boradx".into()),
            "=".into(),
            " --enable-typo".into(),
            9,
            9,
            "EXTRA_OECONF:append:boradx = \" --enable-typo\"".into()
        )
    );
    let fn_row: (String, String, i64, i64) = conn
        .query_row(
            "SELECT s.name, s.op, s.start_line, s.end_line FROM statements s \
             JOIN files f ON f.id = s.file_id \
             WHERE f.path = 'recipes-app/app/app_0.1.bb' AND s.kind = 'function' AND s.name = 'do_install'",
            [],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
        )
        .unwrap();
    assert_eq!(
        fn_row,
        ("do_install".into(), "fakeroot shell".into(), 25, 29)
    );
    let anon: i64 = count(
        conn,
        "SELECT start_line FROM statements WHERE kind = 'function' AND name = '__anonymous'",
    );
    assert_eq!(anon, 13);
    let fts = count(
        conn,
        "SELECT count(*) FROM statements_fts WHERE statements_fts MATCH '\"boradx\"'",
    );
    assert_eq!(fts, 1);

    // Tasks, provides, depends.
    let task: (String, String, String, i64) = conn
        .query_row(
            "SELECT name, before, after, line FROM tasks WHERE name = 'report'",
            [],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
        )
        .unwrap();
    assert_eq!(
        task,
        ("report".into(), "do_build".into(), "do_compile".into(), 34)
    );
    let providers: Vec<String> = conn
        .prepare(
            "SELECT f.pn FROM provides p JOIN files f ON f.id = p.file_id \
             WHERE p.name = 'virtual/foo' ORDER BY 1",
        )
        .unwrap()
        .query_map([], |r| r.get(0))
        .unwrap()
        .collect::<rusqlite::Result<_>>()
        .unwrap();
    assert_eq!(providers, ["libfoo", "libfoo-alt"]);
    let deps: Vec<(String, String, Option<String>, String, i64)> = conn
        .prepare(
            "SELECT f.path, d.kind, d.pkg, d.target, d.line FROM depends d \
             JOIN files f ON f.id = d.file_id ORDER BY 1, 5, 4",
        )
        .unwrap()
        .query_map([], |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?))
        })
        .unwrap()
        .collect::<rusqlite::Result<_>>()
        .unwrap();
    let d = |p: &str, k: &str, pkg: Option<&str>, t: &str, l: i64| {
        (
            p.to_string(),
            k.to_string(),
            pkg.map(str::to_string),
            t.to_string(),
            l,
        )
    };
    assert_eq!(
        deps,
        [
            d(
                "classes-recipe/autotoolsish.bbclass",
                "DEPENDS",
                None,
                "autoconf-native",
                11
            ),
            d(
                "classes-recipe/autotoolsish.bbclass",
                "DEPENDS",
                None,
                "automake-native",
                11
            ),
            d("recipes-app/app/app_0.1.bb", "DEPENDS", None, "libfoo", 4),
            d(
                "recipes-app/app/app_0.1.bb",
                "RDEPENDS",
                Some("${PN}"),
                "libfoo",
                11
            ),
            d(
                "recipes-core/hello/hello.inc",
                "DEPENDS",
                None,
                "virtual/foo",
                7
            ),
            d(
                "recipes-core/libfoo/libfoo_2.0.bb",
                "PACKAGECONFIG",
                Some("zlib"),
                "zlib",
                15
            ),
            d(
                "recipes-core/libfoo/libfoo_2.0.bb",
                "PACKAGECONFIG",
                Some("ssl"),
                "openssl",
                16
            ),
        ]
    );
}

#[test]
fn first_scan_edges() {
    let ws = fixture_ws();
    let (_c, mut idx) = fresh_index(&ws);
    idx.refresh(&ws).unwrap();
    let e = |s: &str, k: &str, t: &str, f: Option<&str>| {
        (
            s.to_string(),
            k.to_string(),
            t.to_string(),
            f.map(str::to_string),
        )
    };
    assert_eq!(
        edges(idx.conn()),
        [
            e(
                "meta-core:conf/bitbake.conf",
                "include",
                "conf/site.conf",
                None
            ),
            e(
                "meta-core:conf/bitbake.conf",
                "include",
                "conf/auto.conf",
                None
            ),
            e(
                "meta-core:conf/bitbake.conf",
                "include",
                "conf/local.conf",
                Some("build:conf/local.conf")
            ),
            e(
                "meta-core:conf/bitbake.conf",
                "include",
                "conf/machine/${MACHINE}.conf",
                None
            ),
            e(
                "meta-core:conf/bitbake.conf",
                "include",
                "conf/distro/${DISTRO}.conf",
                None
            ),
            e(
                "meta-core:recipes-core/hello/hello.inc",
                "inherit",
                "autotoolsish",
                Some("meta-core:classes-recipe/autotoolsish.bbclass")
            ),
            e(
                "meta-core:recipes-core/hello/hello_1.0.bb",
                "require",
                "hello.inc",
                Some("meta-core:recipes-core/hello/hello.inc")
            ),
            e(
                "meta-core:recipes-core/hello/hello_1.1.bb",
                "require",
                "hello.inc",
                Some("meta-core:recipes-core/hello/hello.inc")
            ),
            e(
                "meta-core:recipes-core/libfoo/libfoo_2.0.bb",
                "inherit",
                "autotoolsish",
                Some("meta-core:classes-recipe/autotoolsish.bbclass")
            ),
            e(
                "meta-mine:recipes-app/orphan/orphan_9.%.bbappend",
                "bbappend",
                "orphan_9.%.bb",
                None
            ),
            e(
                "meta-mine:recipes-core/hello/hello_1.1.bbappend",
                "bbappend",
                "hello_1.1.bb",
                Some("meta-core:recipes-core/hello/hello_1.1.bb")
            ),
            e(
                "meta-vendor:recipes-core/hello/hello_%.bbappend",
                "bbappend",
                "hello_%.bb",
                Some("meta-core:recipes-core/hello/hello_1.0.bb")
            ),
            e(
                "meta-vendor:recipes-core/hello/hello_%.bbappend",
                "bbappend",
                "hello_%.bb",
                Some("meta-core:recipes-core/hello/hello_1.1.bb")
            ),
            e(
                "meta-vendor:recipes-vendor/libfoo-alt/libfoo-alt_1.0.bb",
                "inherit",
                "autotoolsish",
                Some("meta-core:classes-recipe/autotoolsish.bbclass")
            ),
        ]
    );
}

/// Snapshot of every statement row except those of `skip` (by `layer:path`).
fn statement_rows(conn: &Connection, skip: &str) -> Vec<(i64, i64, String)> {
    let mut st = conn
        .prepare(
            "SELECT s.id, s.file_id, coalesce(s.name, '') FROM statements s \
             JOIN files f ON f.id = s.file_id JOIN layers l ON l.id = f.layer_id \
             WHERE l.name || ':' || f.path <> ?1 ORDER BY s.id",
        )
        .unwrap();
    st.query_map([skip], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
        .unwrap()
        .collect::<rusqlite::Result<_>>()
        .unwrap()
}

fn rows_of(conn: &Connection, lp: &str) -> Vec<String> {
    let mut st = conn
        .prepare(
            "SELECT s.kind || ' ' || coalesce(s.name, '') || ' ' || s.start_line FROM statements s \
             JOIN files f ON f.id = s.file_id JOIN layers l ON l.id = f.layer_id \
             WHERE l.name || ':' || f.path = ?1 ORDER BY s.ord",
        )
        .unwrap();
    st.query_map([lp], |r| r.get(0))
        .unwrap()
        .collect::<rusqlite::Result<_>>()
        .unwrap()
}

fn reindexed(r: &sous_chef::index::RefreshReport) -> Vec<String> {
    r.reindexed
        .iter()
        .map(|l| format!("{}:{}", l.layer, l.path.display()))
        .collect()
}

fn temp_index() -> (
    tempfile::TempDir,
    PathBuf,
    Workspace,
    tempfile::TempDir,
    Index,
) {
    let (tmp, root) = temp_ws();
    let ws = Workspace::discover(&ws_config(&root)).unwrap();
    let (cache, mut idx) = fresh_index(&ws);
    idx.refresh(&ws).unwrap();
    (tmp, root, ws, cache, idx)
}

#[test]
fn second_scan_reindexes_nothing() {
    let (_t, _root, ws, _c, mut idx) = temp_index();
    let before = statement_rows(idx.conn(), "");
    let r = idx.refresh(&ws).unwrap();
    assert_eq!(r.scanned, 21);
    assert!(r.reindexed.is_empty(), "{:?}", r.reindexed);
    assert_eq!(r.removed, 0);
    assert_eq!(statement_rows(idx.conn(), ""), before);
}

#[test]
fn touching_without_change_reindexes_nothing() {
    let (_t, _root, ws, _c, mut idx) = temp_index();
    let file = abs(&ws, "meta-core:recipes-core/hello/hello.inc");
    let before = statement_rows(idx.conn(), "");
    let later = SystemTime::now() + Duration::from_secs(3600);
    fs::File::options()
        .write(true)
        .open(&file)
        .unwrap()
        .set_modified(later)
        .unwrap();
    let r = idx.refresh(&ws).unwrap();
    assert!(r.reindexed.is_empty(), "{:?}", r.reindexed);
    assert_eq!(statement_rows(idx.conn(), ""), before);
    // The new mtime was recorded, so the next refresh does not even hash it.
    let mtime: i64 = idx
        .conn()
        .query_row(
            "SELECT mtime FROM files WHERE path = 'recipes-core/hello/hello.inc'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    let want = later
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap()
        .as_nanos() as i64;
    assert_eq!(mtime, want);
}

#[test]
fn editing_a_file_updates_exactly_its_rows() {
    let (_t, _root, ws, _c, mut idx) = temp_index();
    let lp = "meta-mine:recipes-app/app/app_0.1.bb";
    let file = abs(&ws, lp);
    let others = statement_rows(idx.conn(), lp);
    let mut text = fs::read_to_string(&file).unwrap();
    text.push_str("EXTRA_NEW = \"1\"\n");
    fs::write(&file, &text).unwrap();
    let r = idx.refresh(&ws).unwrap();
    assert_eq!(reindexed(&r), [lp]);
    assert_eq!(r.removed, 0);
    assert_eq!(statement_rows(idx.conn(), lp), others);
    let rows = rows_of(idx.conn(), lp);
    assert_eq!(rows.last().unwrap(), "assign EXTRA_NEW 35");
    assert_eq!(rows.len(), sous_chef::parse::parse(&text).statements.len());
    // FTS follows.
    assert_eq!(
        count(
            idx.conn(),
            "SELECT count(*) FROM statements_fts WHERE statements_fts MATCH 'EXTRA_NEW'"
        ),
        1
    );
}

#[test]
fn adding_a_file_adds_exactly_its_rows() {
    let (_t, _root, ws, _c, mut idx) = temp_index();
    let lp = "meta-mine:recipes-app/newapp/newapp_1.0.bb";
    let file = abs(&ws, lp);
    let others = statement_rows(idx.conn(), lp);
    fs::create_dir_all(file.parent().unwrap()).unwrap();
    fs::write(
        &file,
        "SUMMARY = \"new\"\nLICENSE = \"MIT\"\ninherit autotoolsish\n",
    )
    .unwrap();
    let r = idx.refresh(&ws).unwrap();
    assert_eq!(reindexed(&r), [lp]);
    assert_eq!(r.scanned, 22);
    assert_eq!(statement_rows(idx.conn(), lp), others);
    assert_eq!(
        rows_of(idx.conn(), lp),
        [
            "assign SUMMARY 1",
            "assign LICENSE 2",
            "inherit autotoolsish 3"
        ]
    );
    assert!(edges(idx.conn()).contains(&(
        lp.to_string(),
        "inherit".to_string(),
        "autotoolsish".to_string(),
        Some("meta-core:classes-recipe/autotoolsish.bbclass".to_string())
    )));
    let provides = count(
        idx.conn(),
        "SELECT count(*) FROM provides WHERE name = 'newapp'",
    );
    assert_eq!(provides, 1);
}

#[test]
fn deleting_a_file_removes_exactly_its_rows() {
    let (_t, _root, ws, _c, mut idx) = temp_index();
    let lp = "meta-core:recipes-core/hello/hello.inc";
    let others = statement_rows(idx.conn(), lp);
    fs::remove_file(abs(&ws, lp)).unwrap();
    let r = idx.refresh(&ws).unwrap();
    assert!(r.reindexed.is_empty());
    assert_eq!(r.removed, 1);
    assert_eq!(r.scanned, 20);
    assert!(rows_of(idx.conn(), lp).is_empty());
    assert_eq!(statement_rows(idx.conn(), lp), others);
    // The require edges that pointed at it are now unresolved.
    let req: Vec<_> = edges(idx.conn())
        .into_iter()
        .filter(|e| e.1 == "require")
        .collect();
    assert_eq!(req.len(), 2);
    assert!(req.iter().all(|e| e.3.is_none()), "{req:?}");
    assert_eq!(
        count(
            idx.conn(),
            "SELECT count(*) FROM depends WHERE target = 'virtual/foo'"
        ),
        0
    );
}

#[test]
fn new_inc_resolves_an_unresolved_edge() {
    let (_t, _root, ws, _c, mut idx) = temp_index();
    let site = |conn: &Connection| {
        edges(conn)
            .into_iter()
            .find(|e| e.2 == "conf/site.conf")
            .unwrap()
            .3
    };
    assert_eq!(site(idx.conn()), None);
    let lp = "meta-mine:recipes-app/app/app_0.1.bb";
    let app = abs(&ws, lp);
    let mut text = fs::read_to_string(&app).unwrap();
    text.push_str("require app-extra.inc\n");
    fs::write(&app, &text).unwrap();
    idx.refresh(&ws).unwrap();
    let extra = |conn: &Connection| {
        edges(conn)
            .into_iter()
            .find(|e| e.2 == "app-extra.inc")
            .unwrap()
            .3
    };
    assert_eq!(extra(idx.conn()), None);

    fs::write(app.with_file_name("app-extra.inc"), "X = \"1\"\n").unwrap();
    fs::write(
        ws.build_dir.clone().unwrap().join("conf/site.conf"),
        "Y = \"1\"\n",
    )
    .unwrap();
    let r = idx.refresh(&ws).unwrap();
    // Layers are walked in BBLAYERS order, the build directory last.
    assert_eq!(
        reindexed(&r),
        [
            "meta-mine:recipes-app/app/app-extra.inc",
            "build:conf/site.conf"
        ]
    );
    assert_eq!(
        extra(idx.conn()).as_deref(),
        Some("meta-mine:recipes-app/app/app-extra.inc")
    );
    assert_eq!(site(idx.conn()).as_deref(), Some("build:conf/site.conf"));
}

#[test]
fn removed_layer_drops_its_files() {
    let (_t, root, _ws, _c, mut idx) = temp_index();
    let mut cfg = ws_config(&root);
    // Without the build directory: discovery finds every layer.conf (four layers).
    cfg.build_dir = None;
    fs::remove_file(root.join("build/conf/bblayers.conf")).unwrap();
    let ws2 = Workspace::discover(&cfg).unwrap();
    assert_eq!(ws2.layers.len(), 4);
    let r = idx.refresh(&ws2).unwrap();
    // The build pseudo-layer and its remaining file (local.conf) are gone; meta-legacy's
    // files are new.
    assert_eq!(r.removed, 2);
    assert!(
        reindexed(&r).iter().all(|f| f.starts_with("meta-legacy:")),
        "{:?}",
        reindexed(&r)
    );
    assert_eq!(
        count(
            idx.conn(),
            "SELECT count(*) FROM layers WHERE name = 'build'"
        ),
        0
    );
}

#[test]
fn skipped_directories_are_not_walked() {
    let (_t, _root, ws, _c, mut idx) = temp_index();
    let core = ws.layer("meta-core").unwrap().root.clone();
    for d in [".git", "tmp", "sstate-cache", "nested-build"] {
        let p = core.join(d).join("conf");
        fs::create_dir_all(&p).unwrap();
        fs::write(p.join("x.conf"), "A = \"1\"\n").unwrap();
    }
    fs::write(
        core.join("nested-build/conf/bblayers.conf"),
        "BBLAYERS = \"\"\n",
    )
    .unwrap();
    fs::write(
        core.join("recipes-core/hello/files/notes.txt"),
        "not metadata\n",
    )
    .unwrap();
    let r = idx.refresh(&ws).unwrap();
    assert!(r.reindexed.is_empty(), "{:?}", r.reindexed);
    assert_eq!(r.scanned, 21);
}

#[cfg(unix)]
#[test]
fn symlink_outside_the_workspace_is_not_indexed() {
    let (t, _root, ws, _c, mut idx) = temp_index();
    let outside = t.path().join("outside.bb");
    fs::write(&outside, "A = \"1\"\n").unwrap();
    let link = ws
        .layer("meta-mine")
        .unwrap()
        .root
        .join("recipes-app/app/evil_1.0.bb");
    std::os::unix::fs::symlink(&outside, &link).unwrap();
    let r = idx.refresh(&ws).unwrap();
    assert!(r.reindexed.is_empty(), "{:?}", r.reindexed);
}

#[test]
fn unreadable_bytes_do_not_break_the_scan() {
    let (_t, _root, ws, _c, mut idx) = temp_index();
    let lp = "meta-mine:recipes-app/app/garbage_1.0.bb";
    let mut bytes = b"A = \"\xff\xfe\"\n\x00\x01 }\ndo_x() {\n".to_vec();
    bytes.extend_from_slice(&[0xc3, 0x28, b'\n']);
    fs::write(abs(&ws, lp), bytes).unwrap();
    let r = idx.refresh(&ws).unwrap();
    assert_eq!(reindexed(&r), [lp]);
    assert!(!rows_of(idx.conn(), lp).is_empty());
}

#[test]
fn no_change_refresh_is_fast() {
    let ws = fixture_ws();
    let (_c, mut idx) = fresh_index(&ws);
    idx.refresh(&ws).unwrap();
    let mut best = Duration::MAX;
    for _ in 0..5 {
        let t = Instant::now();
        let r = idx.refresh(&ws).unwrap();
        best = best.min(t.elapsed());
        assert!(r.reindexed.is_empty());
    }
    // The target is 20 ms; asserted generously for slow CI machines and debug builds.
    assert!(
        best < Duration::from_millis(100),
        "no-change refresh took {best:?}"
    );
    eprintln!("no-change refresh: {best:?}");
}

#[test]
fn refresh_through_open() {
    // Index::open path is exercised by the MCP tests; here the explicit-root variant is
    // checked to land in the expected directory.
    let ws = fixture_ws();
    let cache = tempfile::tempdir().unwrap();
    let mut idx = Index::open_in(cache.path(), &ws).unwrap();
    idx.refresh(&ws).unwrap();
    assert!(idx.path().starts_with(cache.path()));
    assert!(Path::new(idx.path()).exists());
}
