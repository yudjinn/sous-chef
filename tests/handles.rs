//! T5.1: handle store. Stable ids, round-trip, retention (age, total size, rows lost to an
//! index rebuild, never a handle stored by this process), deny-list.
//!
//! The `handles` table is created here from the schema DDL (T2.1), not by the index code.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use rusqlite::{Connection, params};
use sha2::{Digest, Sha256};
use sous_chef::Error;
use sous_chef::handles::{
    Handle, HandleKind, HandleStore, Retention, contains_private_key, is_denied_path,
};

/// The index schema's DDL, verbatim.
const SCHEMA_DDL: &str = "
CREATE TABLE handles(
    id      TEXT PRIMARY KEY,   -- \"h:\" + 10 hex
    sha256  TEXT,               -- full hex; blob at <cache dir>/handles/<2 hex>/<sha256>
    kind    TEXT,               -- log | env | build | value | text
    command TEXT,
    bytes   INT,
    created INT                 -- seconds since the Unix epoch
);
CREATE INDEX handles_created ON handles(created);
";

const DAY: u64 = 24 * 60 * 60;

struct Cache {
    _tmp: tempfile::TempDir,
    root: PathBuf,
}

impl Cache {
    fn new() -> Cache {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().to_path_buf();
        let conn = Connection::open(root.join("index.sqlite")).unwrap();
        conn.execute_batch(SCHEMA_DDL).unwrap();
        Cache { _tmp: tmp, root }
    }
    fn db(&self) -> PathBuf {
        self.root.join("index.sqlite")
    }
    fn handles_dir(&self) -> PathBuf {
        self.root.join("handles")
    }
    fn conn(&self) -> Connection {
        Connection::open(self.db()).unwrap()
    }
    fn open(&self, retention: Retention) -> HandleStore {
        HandleStore::from_connection(&self.handles_dir(), self.conn(), retention).unwrap()
    }
    /// A handle left by an earlier process: a row plus its blob, written directly.
    fn foreign(&self, tag: &str, size: usize, created: u64) -> (Handle, PathBuf) {
        let sha: String = Sha256::digest(format!("foreign {tag} {}", self.root.display()))
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect();
        let id = format!("h:{}", &sha[..10]);
        let blob = self.handles_dir().join(&sha[..2]).join(&sha);
        fs::create_dir_all(blob.parent().unwrap()).unwrap();
        fs::write(&blob, vec![b'x'; size]).unwrap();
        self.conn()
            .execute(
                "INSERT INTO handles(id, sha256, kind, command, bytes, created)
                 VALUES (?1, ?2, 'log', ?3, ?4, ?5)",
                params![id, sha, format!("old {tag}"), size as i64, created as i64],
            )
            .unwrap();
        (Handle(id), blob)
    }
    fn row_count(&self) -> i64 {
        self.conn()
            .query_row("SELECT COUNT(*) FROM handles", [], |r| r.get(0))
            .unwrap()
    }
    fn blob_files(&self) -> Vec<PathBuf> {
        let mut out = Vec::new();
        if let Ok(rd) = fs::read_dir(self.handles_dir()) {
            for sub in rd.flatten() {
                for f in fs::read_dir(sub.path()).unwrap().flatten() {
                    out.push(f.path());
                }
            }
        }
        out
    }
}

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs()
}

fn expected_sha(kind: &str, command: &str, bytes: &[u8]) -> String {
    let mut h = Sha256::new();
    h.update(kind.as_bytes());
    h.update([0u8]);
    h.update((command.len() as u64).to_le_bytes());
    h.update(command.as_bytes());
    h.update(bytes);
    h.finalize().iter().map(|b| format!("{b:02x}")).collect()
}

fn set_created(cache: &Cache, h: &Handle, created: u64) {
    cache
        .conn()
        .execute(
            "UPDATE handles SET created = ?1 WHERE id = ?2",
            params![created as i64, h.0],
        )
        .unwrap();
}

// ---- stable ids ---------------------------------------------------------------------

#[test]
fn ids_are_stable_and_content_addressed() {
    let cache = Cache::new();
    let store = cache.open(Retention::default());
    let out = b"NOTE: Tasks Summary: all succeeded\n";
    let h1 = store.put(HandleKind::Build, "bitbake hello", out).unwrap();
    let h2 = store.put(HandleKind::Build, "bitbake hello", out).unwrap();
    assert_eq!(h1, h2);
    assert!(h1.is_valid());
    assert_eq!(h1.0.len(), 12);

    let sha = expected_sha("build", "bitbake hello", out);
    assert_eq!(h1.0, format!("h:{}", &sha[..10]));
    let blob = cache.handles_dir().join(&sha[..2]).join(&sha);
    assert_eq!(fs::read(&blob).unwrap(), out);
    assert_eq!(cache.row_count(), 1);

    // Kind, command and bytes each change the id.
    let other_kind = store.put(HandleKind::Log, "bitbake hello", out).unwrap();
    let other_cmd = store.put(HandleKind::Build, "bitbake world", out).unwrap();
    let other_bytes = store.put(HandleKind::Build, "bitbake hello", b"x").unwrap();
    let all = [&h1, &other_kind, &other_cmd, &other_bytes];
    for (i, a) in all.iter().enumerate() {
        for b in &all[i + 1..] {
            assert_ne!(a, b);
        }
    }
    // Framing: moving bytes between command and output changes the id.
    let a = store.put(HandleKind::Text, "ab", b"c").unwrap();
    let b = store.put(HandleKind::Text, "a", b"bc").unwrap();
    assert_ne!(a, b);

    // Same id from a fresh store on the same cache.
    drop(store);
    let store = cache.open(Retention::default());
    assert_eq!(
        store.put(HandleKind::Build, "bitbake hello", out).unwrap(),
        h1
    );
}

#[test]
fn handle_parsing() {
    assert!("h:0123456789".parse::<Handle>().is_ok());
    assert!("h:abcdefabcd".parse::<Handle>().is_ok());
    for bad in [
        "",
        "h:",
        "h:012345678",
        "h:01234567890",
        "h:ABCDEF0123",
        "x:0123456789",
    ] {
        assert!(
            matches!(bad.parse::<Handle>(), Err(Error::Invalid { .. })),
            "{bad}"
        );
    }
    for k in HandleKind::ALL {
        assert_eq!(k.as_str().parse::<HandleKind>().unwrap(), k);
    }
    assert!("blob".parse::<HandleKind>().is_err());
}

// ---- round-trip ---------------------------------------------------------------------

#[test]
fn round_trip_and_meta() {
    let cache = Cache::new();
    let store = cache.open(Retention::default());
    let binary: Vec<u8> = (0..=255u8).cycle().take(70_000).collect();
    let cases: Vec<(HandleKind, &str, Vec<u8>)> = vec![
        (HandleKind::Log, "log.do_compile", b"ERROR: oops\n".to_vec()),
        (
            HandleKind::Env,
            "bitbake -e hello",
            b"SRC_URI=\"file://a\"\n".to_vec(),
        ),
        (HandleKind::Build, "bitbake core-image", binary),
        (HandleKind::Value, "var SRC_URI recipe=hello", Vec::new()),
        (
            HandleKind::Text,
            "anything",
            "ünïcode\n".as_bytes().to_vec(),
        ),
    ];
    let before = now();
    let mut handles = Vec::new();
    for (kind, cmd, bytes) in &cases {
        let h = store.put(*kind, cmd, bytes).unwrap();
        assert_eq!(&store.get(&h).unwrap(), bytes);
        let m = store.meta(&h).unwrap();
        assert_eq!(m.handle, h);
        assert_eq!(m.kind, *kind);
        assert_eq!(m.command, *cmd);
        assert_eq!(m.bytes, bytes.len() as u64);
        assert!(m.created >= before && m.created <= now());
        handles.push(h);
    }
    assert_eq!(store.list().unwrap().len(), cases.len());
    let total: u64 = cases.iter().map(|c| c.2.len() as u64).sum();
    assert_eq!(store.total_bytes().unwrap(), total);

    // The row is in the index table with the schema's columns.
    let (kind, command, bytes): (String, String, i64) = cache
        .conn()
        .query_row(
            "SELECT kind, command, bytes FROM handles WHERE id = ?1",
            params![handles[0].0],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .unwrap();
    assert_eq!(
        (kind.as_str(), command.as_str(), bytes),
        ("log", "log.do_compile", 12)
    );

    // Survives reopening.
    drop(store);
    let store = cache.open(Retention::default());
    for (h, (_, _, bytes)) in handles.iter().zip(&cases) {
        assert_eq!(&store.get(h).unwrap(), bytes);
    }
}

#[test]
fn put_file_round_trip() {
    let cache = Cache::new();
    let store = cache.open(Retention::default());
    let log = cache.root.join("log.do_fetch.1234");
    fs::write(&log, "ERROR: Fetcher failure\n").unwrap();
    let h = store.put_file(HandleKind::Log, &log).unwrap();
    assert_eq!(store.get(&h).unwrap(), b"ERROR: Fetcher failure\n");
    assert_eq!(store.meta(&h).unwrap().command, log.display().to_string());
}

#[test]
fn unknown_invalid_and_damaged_handles() {
    let cache = Cache::new();
    let store = cache.open(Retention::default());
    let unknown = Handle("h:0000000000".into());
    assert!(matches!(store.get(&unknown), Err(Error::NotFound { .. })));
    assert!(matches!(store.meta(&unknown), Err(Error::NotFound { .. })));
    let bad = Handle("h:zz".into());
    assert!(matches!(store.get(&bad), Err(Error::Invalid { .. })));

    let h = store.put(HandleKind::Text, "t", b"hello").unwrap();
    let sha = expected_sha("text", "t", b"hello");
    let blob = store.blob_path(&sha);
    fs::write(&blob, b"hel").unwrap();
    assert!(matches!(store.get(&h), Err(Error::Invalid { .. })));
    // Re-putting repairs it.
    assert_eq!(store.put(HandleKind::Text, "t", b"hello").unwrap(), h);
    assert_eq!(store.get(&h).unwrap(), b"hello");
    fs::remove_file(&blob).unwrap();
    assert!(matches!(store.get(&h), Err(Error::NotFound { .. })));
}

#[test]
fn null_columns_do_not_panic() {
    let cache = Cache::new();
    cache
        .conn()
        .execute(
            "INSERT INTO handles(id, sha256, kind, command, bytes, created)
             VALUES ('h:abababab00', NULL, 'text', NULL, NULL, NULL)",
            [],
        )
        .unwrap();
    let store = cache.open(Retention {
        max_age_secs: u64::MAX,
        max_total_bytes: u64::MAX,
    });
    // No blob for an empty sha, so retention removed the row; get is NotFound either way.
    assert!(store.get(&Handle("h:abababab00".into())).is_err());
}

// ---- retention ----------------------------------------------------------------------

#[test]
fn retention_by_age_on_open() {
    let cache = Cache::new();
    let t = now();
    let (old, old_blob) = cache.foreign("old", 10, t - 15 * DAY);
    let (recent, recent_blob) = cache.foreign("recent", 10, t - 13 * DAY);

    let store = cache.open(Retention::default());
    assert!(matches!(store.meta(&old), Err(Error::NotFound { .. })));
    assert!(!old_blob.exists());
    assert!(recent_blob.exists());
    assert_eq!(store.get(&recent).unwrap(), vec![b'x'; 10]);
}

#[test]
fn retention_by_total_size_deletes_oldest_first() {
    let cache = Cache::new();
    let t = now();
    let (a, a_blob) = cache.foreign("a", 60, t - 3 * DAY);
    let (b, b_blob) = cache.foreign("b", 60, t - 2 * DAY);
    let (c, c_blob) = cache.foreign("c", 60, t - DAY);

    let store = cache.open(Retention {
        max_age_secs: 14 * DAY,
        max_total_bytes: 130,
    });
    // 180 > 130: dropping the oldest (a) leaves 120.
    assert!(store.meta(&a).is_err() && !a_blob.exists());
    assert!(store.meta(&b).is_ok() && b_blob.exists());
    assert!(store.meta(&c).is_ok() && c_blob.exists());
    assert_eq!(store.total_bytes().unwrap(), 120);

    let report = store
        .apply_retention(
            Retention {
                max_age_secs: 14 * DAY,
                max_total_bytes: 60,
            },
            now(),
        )
        .unwrap();
    assert_eq!(report.deleted, 1);
    assert_eq!(report.freed_bytes, 60);
    assert!(store.meta(&b).is_err());
    assert!(store.meta(&c).is_ok());
}

#[test]
fn retention_never_deletes_handles_stored_by_this_process() {
    let cache = Cache::new();
    let store = cache.open(Retention::default());
    let mine = store
        .put(HandleKind::Log, "mine", &vec![b'm'; 1000])
        .unwrap();
    // Make it look ancient, and the size limit tiny.
    set_created(&cache, &mine, 0);
    let (foreign, _) = cache.foreign("f", 10, 0);
    let strict = Retention {
        max_age_secs: DAY,
        max_total_bytes: 1,
    };
    let report = store.apply_retention(strict, now()).unwrap();
    assert_eq!(report.deleted, 1);
    assert!(store.meta(&foreign).is_err());
    assert_eq!(store.get(&mine).unwrap(), vec![b'm'; 1000]);

    // Also across a reopen in the same process.
    drop(store);
    let store = cache.open(strict);
    assert_eq!(store.get(&mine).unwrap().len(), 1000);
}

#[test]
fn retention_sweeps_blobs_without_rows() {
    let cache = Cache::new();
    let store = cache.open(Retention::default());
    let mine = store.put(HandleKind::Env, "bitbake -e", b"A=1\n").unwrap();
    let (old, old_blob) = cache.foreign("orphan", 50, now());
    let (_, stray_blob) = cache.foreign("stray", 5, now());
    // An index rebuild drops every row; the blobs stay on disk.
    cache.conn().execute("DELETE FROM handles", []).unwrap();
    assert_eq!(cache.blob_files().len(), 3);

    let report = store.apply_retention(Retention::default(), now()).unwrap();
    assert_eq!(report.deleted, 0);
    assert_eq!(report.freed_bytes, 55);
    assert!(!old_blob.exists() && !stray_blob.exists());
    // The blob stored by this process survives, and re-putting restores its row.
    let mine_blob = store.blob_path(&expected_sha("env", "bitbake -e", b"A=1\n"));
    assert!(mine_blob.exists());
    assert_eq!(cache.blob_files(), vec![mine_blob]);
    assert!(store.meta(&old).is_err());
    assert_eq!(
        store.put(HandleKind::Env, "bitbake -e", b"A=1\n").unwrap(),
        mine
    );
    assert_eq!(store.get(&mine).unwrap(), b"A=1\n");

    // A row whose blob vanished is removed too.
    let (gone, gone_blob) = cache.foreign("gone", 5, now());
    fs::remove_file(gone_blob).unwrap();
    let report = store.apply_retention(Retention::default(), now()).unwrap();
    assert_eq!(report.deleted, 1);
    assert!(store.meta(&gone).is_err());
}

// ---- deny-list ----------------------------------------------------------------------

#[test]
fn deny_list_paths() {
    for p in [
        ".env",
        "/work/app/.env",
        ".env.local",
        ".env.production",
        ".envrc",
        "/home/u/.ssh/id_rsa",
        "id_ed25519",
        "/home/u/.ssh/config",
        "server.key",
        "signing_key.pem",
        "KEY.PEM",
        "cert.p12",
        "store.pfx",
        "release.keystore",
        "app.jks",
        "trust.bks",
        "putty.ppk",
        "/home/u/.gnupg/private-keys-v1.d/ABC.key",
        "secring.gpg",
        ".netrc",
    ] {
        assert!(is_denied_path(Path::new(p)), "{p} should be denied");
    }
    for p in [
        "hello_1.0.bb",
        "log.do_compile.1234",
        "environment",
        "conf/local.conf",
        "keystore.txt",
        "id_rsa.pub",
        "recipes-env/foo.bb",
        "",
    ] {
        assert!(!is_denied_path(Path::new(p)), "{p} should be allowed");
    }
}

#[test]
fn deny_list_private_key_content() {
    assert!(contains_private_key(
        b"x\n-----BEGIN RSA PRIVATE KEY-----\nMII"
    ));
    assert!(contains_private_key(
        b"-----BEGIN OPENSSH PRIVATE KEY-----\n"
    ));
    assert!(contains_private_key(b"-----BEGIN PRIVATE KEY-----"));
    assert!(contains_private_key(
        b"-----BEGIN ENCRYPTED PRIVATE KEY-----"
    ));
    assert!(contains_private_key(
        b"-----BEGIN PGP PRIVATE KEY BLOCK-----"
    ));
    assert!(!contains_private_key(
        b"-----BEGIN CERTIFICATE-----\nPRIVATE KEY later"
    ));
    assert!(!contains_private_key(b"-----BEGIN PUBLIC KEY-----"));
    assert!(!contains_private_key(b""));
}

#[test]
fn deny_list_refuses_to_store() {
    let cache = Cache::new();
    let store = cache.open(Retention::default());
    for cmd in [
        "cat .env",
        "cat '/srv/app/.env.local'",
        "cp ~/.ssh/id_rsa /tmp",
        "openssl rsa -in=signing.key",
        "keytool -list -keystore release.jks",
    ] {
        let r = store.put(HandleKind::Text, cmd, b"harmless");
        assert!(matches!(r, Err(Error::Invalid { .. })), "{cmd}");
    }
    let r = store.put(
        HandleKind::Log,
        "bitbake hello",
        b"NOTE: ok\n-----BEGIN EC PRIVATE KEY-----\nabc\n",
    );
    let msg = r.unwrap_err().to_string();
    assert!(msg.contains("deny-list"), "{msg}");

    let env = cache.root.join(".env");
    fs::write(&env, "TOKEN=secret").unwrap();
    assert!(matches!(
        store.put_file(HandleKind::Text, &env),
        Err(Error::Invalid { .. })
    ));
    // Nothing reached the disk or the table.
    assert!(cache.blob_files().is_empty());
    assert_eq!(cache.row_count(), 0);

    // Ordinary output is fine.
    assert!(
        store
            .put(HandleKind::Build, "bitbake -c compile hello", b"ok")
            .is_ok()
    );
}

#[test]
fn deny_list_refuses_to_return() {
    let cache = Cache::new();
    let t = now();
    // Rows written by something other than this store (an older version, a manual edit).
    let (by_name, _) = cache.foreign("name", 4, t);
    cache
        .conn()
        .execute(
            "UPDATE handles SET command = 'cat /app/.env' WHERE id = ?1",
            params![by_name.0],
        )
        .unwrap();
    let (by_content, blob) = cache.foreign("content", 0, t);
    let secret = b"-----BEGIN OPENSSH PRIVATE KEY-----\n";
    fs::write(&blob, secret).unwrap();
    cache
        .conn()
        .execute(
            "UPDATE handles SET bytes = ?1 WHERE id = ?2",
            params![secret.len() as i64, by_content.0],
        )
        .unwrap();

    let store = cache.open(Retention::default());
    for h in [&by_name, &by_content] {
        let r = store.get(h);
        assert!(matches!(r, Err(Error::Invalid { .. })), "{h}: {r:?}");
    }
}

// ---- configured retention -------------------------------------------------------------

#[test]
fn open_with_retention_applies_and_reports_the_given_limits() {
    let cache = Cache::new();
    let t = now();
    let (three_days, blob) = cache.foreign("three days", 10, t - 3 * DAY);
    // The defaults (14 days) keep it.
    let store = HandleStore::open(&cache.handles_dir()).unwrap();
    assert_eq!(store.retention(), Retention::default());
    assert!(store.meta(&three_days).is_ok());
    assert_eq!(store.count().unwrap(), 1);
    assert_eq!(store.total_bytes().unwrap(), 10);
    drop(store);
    // Two days, as `[handles] max_age_days = 2` gives: deleted on open.
    let two_days = Retention {
        max_age_secs: 2 * DAY,
        max_total_bytes: 1_000_000,
    };
    let store = HandleStore::open_with_retention(&cache.handles_dir(), two_days).unwrap();
    assert_eq!(store.retention(), two_days);
    assert!(matches!(
        store.meta(&three_days),
        Err(Error::NotFound { .. })
    ));
    assert!(!blob.exists());
    assert_eq!(store.count().unwrap(), 0);
}
