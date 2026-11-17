//! T2.1: index schema, pragmas, cache path, reopen and version-mismatch rebuild.

use std::path::{Path, PathBuf};
use std::time::Duration;

use rusqlite::Connection;
use sous_chef::index::{self, Index, schema};
use sous_chef::workspace::{Layer, Workspace};

fn layer(name: &str, root: &Path) -> Layer {
    Layer {
        name: name.into(),
        root: root.to_path_buf(),
        collections: vec![name.into()],
        priority: 5,
        compat: vec![],
        depends: vec![],
        bbfiles: vec![],
        writable: false,
    }
}

fn ws(roots: &[&str], build: Option<&str>) -> Workspace {
    Workspace {
        build_dir: build.map(PathBuf::from),
        layers: roots
            .iter()
            .map(|r| {
                layer(
                    Path::new(r).file_name().unwrap().to_str().unwrap(),
                    Path::new(r),
                )
            })
            .collect(),
        ..Workspace::default()
    }
}

fn objects(conn: &Connection, ty: &str) -> Vec<String> {
    let mut st = conn
        .prepare("SELECT name FROM sqlite_master WHERE type = ?1 ORDER BY name")
        .unwrap();
    st.query_map([ty], |r| r.get(0))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap()
}

fn columns(conn: &Connection, table: &str) -> Vec<String> {
    let mut st = conn
        .prepare(&format!(
            "SELECT name FROM pragma_table_info('{table}') ORDER BY cid"
        ))
        .unwrap();
    st.query_map([], |r| r.get(0))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap()
}

fn index_columns(conn: &Connection, table: &str) -> Vec<Vec<String>> {
    let mut out = vec![];
    for idx in objects(conn, "index") {
        let tbl: String = conn
            .query_row(
                "SELECT tbl_name FROM sqlite_master WHERE name = ?1",
                [&idx],
                |r| r.get(0),
            )
            .unwrap();
        if tbl != table {
            continue;
        }
        let mut st = conn
            .prepare(&format!(
                "SELECT name FROM pragma_index_info('{idx}') ORDER BY seqno"
            ))
            .unwrap();
        let cols: Vec<String> = st
            .query_map([], |r| r.get(0))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap();
        out.push(cols);
    }
    out
}

fn fresh(dir: &Path) -> Index {
    Index::open_in(dir, &ws(&["/w/poky/meta", "/w/meta-foo"], Some("/w/build"))).unwrap()
}

#[test]
fn fresh_index_has_every_table_and_column() {
    let tmp = tempfile::tempdir().unwrap();
    let idx = fresh(tmp.path());
    let c = idx.conn();

    let tables = objects(c, "table");
    for t in schema::TABLES {
        assert!(
            tables.contains(&t.to_string()),
            "missing table {t}: {tables:?}"
        );
    }

    let expect: &[(&str, &[&str])] = &[
        (
            "layers",
            &[
                "id",
                "name",
                "root",
                "priority",
                "collections",
                "compat",
                "depends",
                "writable",
            ],
        ),
        (
            "files",
            &[
                "id", "layer_id", "path", "kind", "mtime", "size", "sha256", "pn", "pv",
            ],
        ),
        (
            "statements",
            &[
                "id",
                "file_id",
                "ord",
                "kind",
                "name",
                "base",
                "overrides",
                "flag",
                "op",
                "legacy",
                "value",
                "start_line",
                "end_line",
                "snippet",
            ],
        ),
        (
            "edges",
            &[
                "id",
                "src_file",
                "kind",
                "target_text",
                "target_file",
                "line",
            ],
        ),
        ("tasks", &["file_id", "name", "before", "after", "line"]),
        ("provides", &["file_id", "name"]),
        ("depends", &["file_id", "kind", "pkg", "target", "line"]),
        ("statements_fts", &["name", "snippet"]),
        (
            "handles",
            &["id", "sha256", "kind", "command", "bytes", "created"],
        ),
    ];
    for (t, cols) in expect {
        assert_eq!(columns(c, t), *cols, "columns of {t}");
    }
}

#[test]
fn contract_indexes_exist() {
    let tmp = tempfile::tempdir().unwrap();
    let idx = fresh(tmp.path());
    let c = idx.conn();
    let want: &[(&str, &[&str])] = &[
        ("statements", &["base"]),
        ("statements", &["file_id", "ord"]),
        ("statements", &["kind", "name"]),
        ("edges", &["src_file"]),
        ("edges", &["target_file"]),
        ("files", &["layer_id", "path"]),
    ];
    for (t, cols) in want {
        let have = index_columns(c, t);
        let cols: Vec<String> = cols.iter().map(|s| s.to_string()).collect();
        assert!(
            have.contains(&cols),
            "{t}: no index on {cols:?}; have {have:?}"
        );
    }
}

#[test]
fn pragmas_are_set() {
    let tmp = tempfile::tempdir().unwrap();
    let idx = fresh(tmp.path());
    let c = idx.conn();
    let mode: String = c
        .pragma_query_value(None, "journal_mode", |r| r.get(0))
        .unwrap();
    assert_eq!(mode, "wal");
    let fk: i64 = c
        .pragma_query_value(None, "foreign_keys", |r| r.get(0))
        .unwrap();
    assert_eq!(fk, 1);
    let busy: i64 = c
        .pragma_query_value(None, "busy_timeout", |r| r.get(0))
        .unwrap();
    assert_eq!(busy, 30_000);
    assert_eq!(schema::user_version(c).unwrap(), schema::SCHEMA_VERSION);
    assert_eq!(schema::BUSY_TIMEOUT, Duration::from_secs(30));
}

#[test]
fn cache_path_rule() {
    let tmp = tempfile::tempdir().unwrap();
    let w = ws(&["/w/poky/meta", "/w/meta-foo"], Some("/w/build"));
    let idx = Index::open_in(tmp.path(), &w).unwrap();
    let key = index::workspace_key(&w);
    assert_eq!(key.len(), 16);
    assert!(
        key.chars()
            .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase())
    );
    let expected = tmp.path().join("sous-chef").join(&key).join("index.sqlite");
    assert_eq!(idx.path(), expected);
    assert_eq!(idx.cache_dir(), expected.parent().unwrap());
    assert!(expected.is_file());

    // Key: sha256 of the sorted roots, each followed by "\n".
    use sha2::{Digest, Sha256};
    let d = Sha256::digest(b"/w/build\n/w/meta-foo\n/w/poky/meta\n");
    let hex: String = d.iter().take(8).map(|b| format!("{b:02x}")).collect();
    assert_eq!(key, hex);

    // Order-independent, but different roots give a different key.
    let reordered = ws(&["/w/meta-foo", "/w/poky/meta"], Some("/w/build"));
    assert_eq!(index::workspace_key(&reordered), key);
    let other = ws(&["/w/meta-foo"], Some("/w/build"));
    assert_ne!(index::workspace_key(&other), key);
}

#[test]
fn cache_root_from_env() {
    assert_eq!(
        index::cache_root_from(Some("/xdg".into()), Some("/home/u".into())).unwrap(),
        PathBuf::from("/xdg")
    );
    // Empty or relative XDG_CACHE_HOME is ignored.
    assert_eq!(
        index::cache_root_from(Some("".into()), Some("/home/u".into())).unwrap(),
        PathBuf::from("/home/u/.cache")
    );
    assert_eq!(
        index::cache_root_from(Some("rel".into()), Some("/home/u".into())).unwrap(),
        PathBuf::from("/home/u/.cache")
    );
    assert!(index::cache_root_from(None, None).is_err());
}

fn insert_layer(idx: &mut Index, name: &str) {
    idx.write(|tx| {
        tx.execute(
            "INSERT INTO layers(name, root, priority) VALUES (?1, '/x', 5)",
            [name],
        )?;
        Ok(())
    })
    .unwrap();
}

#[test]
fn reopen_keeps_data() {
    let tmp = tempfile::tempdir().unwrap();
    let path = {
        let mut idx = fresh(tmp.path());
        insert_layer(&mut idx, "meta-foo");
        idx.path().to_path_buf()
    };
    let idx = fresh(tmp.path());
    assert_eq!(idx.path(), path);
    let n: i64 = idx
        .conn()
        .query_row(
            "SELECT count(*) FROM layers WHERE name = 'meta-foo'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(n, 1);
}

#[test]
fn version_mismatch_rebuilds() {
    let tmp = tempfile::tempdir().unwrap();
    let path = {
        let mut idx = fresh(tmp.path());
        insert_layer(&mut idx, "meta-foo");
        idx.conn()
            .pragma_update(None, "user_version", schema::SCHEMA_VERSION + 1)
            .unwrap();
        // A table the current schema does not have: it must be gone after the rebuild.
        idx.conn().execute_batch("CREATE TABLE stale(x)").unwrap();
        idx.path().to_path_buf()
    };
    let idx = Index::open_at(&path).unwrap();
    let c = idx.conn();
    assert_eq!(schema::user_version(c).unwrap(), schema::SCHEMA_VERSION);
    let n: i64 = c
        .query_row("SELECT count(*) FROM layers", [], |r| r.get(0))
        .unwrap();
    assert_eq!(n, 0, "old rows survived the rebuild");
    assert!(!objects(c, "table").contains(&"stale".to_string()));
    for t in schema::TABLES {
        assert!(objects(c, "table").contains(&t.to_string()), "missing {t}");
    }
}

#[test]
fn foreign_database_and_garbage_file_are_rebuilt() {
    let tmp = tempfile::tempdir().unwrap();
    // user_version 0 but already has objects: not ours.
    let p1 = tmp.path().join("a/index.sqlite");
    std::fs::create_dir_all(p1.parent().unwrap()).unwrap();
    Connection::open(&p1)
        .unwrap()
        .execute_batch("CREATE TABLE layers(only_column)")
        .unwrap();
    let idx = Index::open_at(&p1).unwrap();
    assert!(columns(idx.conn(), "layers").contains(&"priority".to_string()));

    // Not a database at all.
    let p2 = tmp.path().join("b/index.sqlite");
    std::fs::create_dir_all(p2.parent().unwrap()).unwrap();
    std::fs::write(&p2, vec![0xAB; 8192]).unwrap();
    let idx = Index::open_at(&p2).unwrap();
    assert_eq!(
        schema::user_version(idx.conn()).unwrap(),
        schema::SCHEMA_VERSION
    );
}

#[test]
fn write_commits_on_ok_and_rolls_back_on_err() {
    let tmp = tempfile::tempdir().unwrap();
    let mut idx = fresh(tmp.path());
    insert_layer(&mut idx, "kept");
    let r: Result<(), sous_chef::error::Error> = idx.write(|tx| {
        tx.execute("INSERT INTO layers(name) VALUES ('dropped')", [])?;
        Err(sous_chef::error::Error::Invalid { msg: "boom".into() })
    });
    assert!(r.is_err());
    let names: Vec<String> = {
        let mut st = idx
            .conn()
            .prepare("SELECT name FROM layers ORDER BY name")
            .unwrap();
        st.query_map([], |r| r.get(0))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap()
    };
    assert_eq!(names, ["kept"]);
}

#[test]
fn write_takes_the_lock_immediately() {
    // While one connection holds a write transaction, BEGIN IMMEDIATE on another fails with
    // SQLITE_BUSY at BEGIN (short timeout), not later at the first write.
    let tmp = tempfile::tempdir().unwrap();
    let mut idx = fresh(tmp.path());
    let path = idx.path().to_path_buf();
    let mut other = schema::connect(&path).unwrap();
    other.busy_timeout(Duration::from_millis(50)).unwrap();
    idx.write(|_tx| {
        let begin = other.transaction();
        let err = begin.expect_err("second writer got the lock");
        assert!(
            matches!(
                err.sqlite_error_code(),
                Some(rusqlite::ErrorCode::DatabaseBusy)
            ),
            "{err}"
        );
        Ok(())
    })
    .unwrap();
    // `schema::connect` makes plain `transaction()` immediate too; it now succeeds.
    let tx = other.transaction().unwrap();
    tx.commit().unwrap();
}

#[test]
fn cascades_and_fts_triggers() {
    let tmp = tempfile::tempdir().unwrap();
    let mut idx = fresh(tmp.path());
    idx.write(|tx| {
        tx.execute_batch(
            "INSERT INTO layers(id, name) VALUES (1, 'meta-foo');
             INSERT INTO files(id, layer_id, path, kind) VALUES (10, 1, 'a.bb', 'bb'),
                                                                (11, 1, 'a.inc', 'inc');
             INSERT INTO statements(file_id, ord, kind, name, base, snippet)
                 VALUES (10, 0, 'function', 'do_install:append', 'do_install', 'do_install:append() {');
             INSERT INTO edges(src_file, kind, target_text, target_file, line)
                 VALUES (10, 'require', 'a.inc', 11, 1);
             INSERT INTO tasks(file_id, name) VALUES (10, 'do_x');
             INSERT INTO provides(file_id, name) VALUES (10, 'a');
             INSERT INTO depends(file_id, kind, target, line) VALUES (10, 'DEPENDS', 'zlib', 2);",
        )?;
        Ok(())
    })
    .unwrap();
    let count =
        |idx: &Index, sql: &str| -> i64 { idx.conn().query_row(sql, [], |r| r.get(0)).unwrap() };
    assert_eq!(
        count(
            &idx,
            "SELECT count(*) FROM statements_fts WHERE statements_fts MATCH 'do_install'"
        ),
        1
    );

    // Deleting the target sets edges.target_file to NULL.
    idx.write(|tx| {
        tx.execute("DELETE FROM files WHERE id = 11", [])?;
        Ok(())
    })
    .unwrap();
    assert_eq!(
        count(&idx, "SELECT count(*) FROM edges WHERE target_file IS NULL"),
        1
    );

    // Deleting the layer cascades to its files and everything below them, FTS included.
    idx.write(|tx| {
        tx.execute("DELETE FROM layers WHERE id = 1", [])?;
        Ok(())
    })
    .unwrap();
    for t in [
        "files",
        "statements",
        "edges",
        "tasks",
        "provides",
        "depends",
    ] {
        assert_eq!(count(&idx, &format!("SELECT count(*) FROM {t}")), 0, "{t}");
    }
    assert_eq!(
        count(
            &idx,
            "SELECT count(*) FROM statements_fts WHERE statements_fts MATCH 'do_install'"
        ),
        0
    );

    // Foreign keys are enforced.
    let bad = idx.write(|tx| {
        tx.execute("INSERT INTO files(layer_id, path) VALUES (999, 'x.bb')", [])?;
        Ok(())
    });
    assert!(bad.is_err());
}
