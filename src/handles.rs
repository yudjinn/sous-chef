//! Content-addressed store for large output: `h:<10 hex>` handles.
//!
//! - A handle id is `h:` + the first 10 hex digits of sha256 over (kind, command, bytes),
//!   so identical output stored for the same command gets the same handle.
//! - The bytes live at `<dir>/<first 2 hex>/<full sha256 hex>`, where `dir` is normally
//!   `<cache dir>/handles` (the cache dir being the one that holds `index.sqlite`).
//! - Metadata lives in the index's `handles` table ([`HANDLES_DDL`]).
//! - Retention runs when the store is opened: handles older than [`Retention::max_age_secs`]
//!   are deleted, then the oldest until the total is at most
//!   [`Retention::max_total_bytes`]. A handle stored (or re-stored) by the current process is
//!   never deleted.
//! - A deny-list ([`is_denied_path`], [`contains_private_key`]) keeps secrets out: such
//!   output is neither stored nor returned.

use std::collections::HashSet;
use std::fmt;
use std::fs;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::str::FromStr;
use std::sync::{Mutex, MutexGuard, OnceLock};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use rusqlite::{Connection, OptionalExtension, params};
use serde::Serialize;
use sha2::{Digest, Sha256};

use crate::error::{Error, Result};

/// DDL of the index `handles` table, as the index schema (T2.1) defines it, with
/// `IF NOT EXISTS` added. [`HandleStore`] runs it on open so the store also works on a
/// database the index schema has not set up (tests, or a store opened before the index).
/// Columns are nullable in the schema; a NULL is read as empty text or zero.
pub const HANDLES_DDL: &str = "\
CREATE TABLE IF NOT EXISTS handles(
    id      TEXT PRIMARY KEY,
    sha256  TEXT,
    kind    TEXT,
    command TEXT,
    bytes   INT,
    created INT
);
CREATE INDEX IF NOT EXISTS handles_created ON handles(created);";

/// Number of hex digits after `h:`.
pub const HANDLE_HEX_LEN: usize = 10;

/// `h:` followed by 10 hex digits.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize)]
pub struct Handle(pub String);

impl fmt::Display for Handle {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl Handle {
    /// True for `h:` followed by exactly 10 lowercase hex digits.
    pub fn is_valid(&self) -> bool {
        self.0.strip_prefix("h:").is_some_and(|hex| {
            hex.len() == HANDLE_HEX_LEN
                && hex
                    .bytes()
                    .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        })
    }
}

impl FromStr for Handle {
    type Err = Error;
    fn from_str(s: &str) -> Result<Handle> {
        let h = Handle(s.trim().to_string());
        if h.is_valid() {
            Ok(h)
        } else {
            Err(invalid_handle(s))
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum HandleKind {
    Log,
    Env,
    Build,
    Value,
    Text,
}

impl HandleKind {
    pub const ALL: [HandleKind; 5] = [
        HandleKind::Log,
        HandleKind::Env,
        HandleKind::Build,
        HandleKind::Value,
        HandleKind::Text,
    ];

    /// The text stored in the `kind` column.
    pub fn as_str(self) -> &'static str {
        match self {
            HandleKind::Log => "log",
            HandleKind::Env => "env",
            HandleKind::Build => "build",
            HandleKind::Value => "value",
            HandleKind::Text => "text",
        }
    }
}

impl fmt::Display for HandleKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for HandleKind {
    type Err = Error;
    fn from_str(s: &str) -> Result<HandleKind> {
        HandleKind::ALL
            .into_iter()
            .find(|k| k.as_str() == s)
            .ok_or_else(|| Error::Invalid {
                msg: format!("unknown handle kind {s:?}; expected log, env, build, value or text"),
            })
    }
}

/// Metadata kept for each handle.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct HandleMeta {
    pub handle: Handle,
    pub kind: HandleKind,
    pub command: String,
    pub bytes: u64,
    /// Seconds since the Unix epoch.
    pub created: u64,
}

/// Retention limits applied when the store is opened.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Retention {
    /// Handles last stored longer ago than this are deleted. Default 14 days.
    pub max_age_secs: u64,
    /// After the age pass, the oldest handles are deleted until the total size is at most
    /// this. Default 500 MB.
    pub max_total_bytes: u64,
}

impl Default for Retention {
    fn default() -> Retention {
        Retention {
            max_age_secs: 14 * 24 * 60 * 60,
            max_total_bytes: 500 * 1000 * 1000,
        }
    }
}

/// What a retention pass removed.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct RetentionReport {
    /// Handles (rows) deleted.
    pub deleted: u32,
    /// Bytes of output freed, including blob files without a row.
    pub freed_bytes: u64,
}

/// The handle store. Methods take `&self`; the store is shared through `mcp::Ctx`.
#[derive(Debug)]
pub struct HandleStore {
    dir: PathBuf,
    conn: Mutex<Connection>,
}

/// sha256 (hex) of every handle stored by this process; retention never deletes them, with
/// or without a row.
fn created_here() -> MutexGuard<'static, HashSet<String>> {
    static SET: OnceLock<Mutex<HashSet<String>>> = OnceLock::new();
    SET.get_or_init(|| Mutex::new(HashSet::new()))
        .lock()
        .unwrap_or_else(|e| e.into_inner())
}

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

impl HandleStore {
    /// Opens the store rooted at `dir` (normally `<cache dir>/handles`), with its metadata in
    /// `<dir>/../index.sqlite`, and applies the default retention.
    ///
    /// Open the [`crate::index::Index`] first: a schema rebuild replaces the database file.
    pub fn open(dir: &Path) -> Result<HandleStore> {
        let db = dir
            .parent()
            .map(|p| p.join("index.sqlite"))
            .ok_or_else(|| Error::Invalid {
                msg: format!(
                    "handle directory {} has no parent to hold index.sqlite",
                    dir.display()
                ),
            })?;
        HandleStore::open_with(dir, &db, Retention::default())
    }

    /// Opens the store at `dir` with metadata in the SQLite database `db`.
    pub fn open_with(dir: &Path, db: &Path, retention: Retention) -> Result<HandleStore> {
        if let Some(parent) = db.parent() {
            fs::create_dir_all(parent)?;
        }
        let conn = Connection::open(db)?;
        conn.busy_timeout(Duration::from_secs(30))?;
        HandleStore::from_connection(dir, conn, retention)
    }

    /// Opens the store at `dir` on an existing connection (to the index database, or any
    /// database for tests). Creates the `handles` table if missing, then applies retention.
    pub fn from_connection(
        dir: &Path,
        conn: Connection,
        retention: Retention,
    ) -> Result<HandleStore> {
        fs::create_dir_all(dir)?;
        conn.execute_batch(HANDLES_DDL)?;
        let store = HandleStore {
            dir: dir.to_path_buf(),
            conn: Mutex::new(conn),
        };
        store.apply_retention(retention, now_secs())?;
        Ok(store)
    }

    /// Opens the store at `dir` on an existing connection without a retention pass, for a
    /// short-lived process that only adds a handle (the Bash hook, T7.3). Retention also
    /// sweeps blob files that have no row, which would race with a `put` of the MCP server
    /// between writing the blob and inserting its row; the server's own open applies it.
    pub fn without_retention(dir: &Path, conn: Connection) -> Result<HandleStore> {
        fs::create_dir_all(dir)?;
        conn.execute_batch(HANDLES_DDL)?;
        Ok(HandleStore {
            dir: dir.to_path_buf(),
            conn: Mutex::new(conn),
        })
    }

    /// The directory blobs are stored under.
    pub fn dir(&self) -> &Path {
        &self.dir
    }

    fn conn(&self) -> MutexGuard<'_, Connection> {
        self.conn.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Where the output with this sha256 (hex) is stored.
    pub fn blob_path(&self, sha_hex: &str) -> PathBuf {
        let prefix = sha_hex.get(..2).unwrap_or("00");
        self.dir.join(prefix).join(sha_hex)
    }

    /// Stores output and returns its handle; identical input gives the same handle.
    /// Refuses output whose command names a denied file or whose bytes hold a private key.
    pub fn put(&self, kind: HandleKind, command: &str, bytes: &[u8]) -> Result<Handle> {
        self.put_at(kind, command, bytes, now_secs())
    }

    /// Reads `path` and stores it as `kind`, with the path as the command. Refuses denied
    /// files by name before reading them.
    pub fn put_file(&self, kind: HandleKind, path: &Path) -> Result<Handle> {
        if is_denied_path(path) {
            return Err(denied(&path.display().to_string()));
        }
        let bytes = fs::read(path)?;
        self.put(kind, &path.display().to_string(), &bytes)
    }

    fn put_at(&self, kind: HandleKind, command: &str, bytes: &[u8], now: u64) -> Result<Handle> {
        if command_is_denied(command) || contains_private_key(bytes) {
            return Err(denied(command));
        }
        let sha = handle_sha(kind, command, bytes);
        let handle = Handle(format!("h:{}", &sha[..HANDLE_HEX_LEN]));

        let conn = self.conn();
        let existing: Option<String> = conn
            .query_row(
                "SELECT sha256 FROM handles WHERE id = ?1",
                params![handle.0],
                |r| r.get(0),
            )
            .optional()?;
        if let Some(other) = &existing
            && other != &sha
        {
            return Err(Error::Invalid {
                msg: format!(
                    "handle id collision for {handle}; delete {} to reset the handle store",
                    self.dir.display()
                ),
            });
        }

        let path = self.blob_path(&sha);
        let intact = fs::metadata(&path).is_ok_and(|m| m.len() == bytes.len() as u64);
        if !intact {
            write_atomic(&path, bytes)?;
        }
        conn.execute(
            "INSERT INTO handles(id, kind, command, sha256, bytes, created)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)
             ON CONFLICT(id) DO UPDATE SET created = excluded.created",
            params![
                handle.0,
                kind.as_str(),
                command,
                sha,
                bytes.len() as i64,
                now as i64
            ],
        )?;
        created_here().insert(sha);
        Ok(handle)
    }

    /// The stored output. Errors on an unknown or expired handle, on missing or damaged
    /// storage, and on output that matches the deny-list.
    pub fn get(&self, handle: &Handle) -> Result<Vec<u8>> {
        let (meta, sha) = self.row(handle)?;
        if command_is_denied(&meta.command) {
            return Err(denied(&meta.command));
        }
        let bytes = match fs::read(self.blob_path(&sha)) {
            Ok(b) => b,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                return Err(not_found(handle));
            }
            Err(e) => return Err(e.into()),
        };
        if bytes.len() as u64 != meta.bytes {
            return Err(Error::Invalid {
                msg: format!(
                    "stored output for {handle} is damaged; rerun the command to recreate it"
                ),
            });
        }
        if contains_private_key(&bytes) {
            return Err(denied(&meta.command));
        }
        Ok(bytes)
    }

    pub fn meta(&self, handle: &Handle) -> Result<HandleMeta> {
        self.row(handle).map(|(m, _)| m)
    }

    fn row(&self, handle: &Handle) -> Result<(HandleMeta, String)> {
        if !handle.is_valid() {
            return Err(invalid_handle(&handle.0));
        }
        let conn = self.conn();
        let row = conn
            .query_row(
                "SELECT kind, command, bytes, created, sha256 FROM handles WHERE id = ?1",
                params![handle.0],
                |r| {
                    Ok((
                        text(r, 0)?,
                        text(r, 1)?,
                        int(r, 2)?,
                        int(r, 3)?,
                        text(r, 4)?,
                    ))
                },
            )
            .optional()?;
        let (kind, command, bytes, created, sha) = row.ok_or_else(|| not_found(handle))?;
        Ok((
            HandleMeta {
                handle: handle.clone(),
                kind: kind.parse()?,
                command,
                bytes: u64::try_from(bytes).unwrap_or(0),
                created: u64::try_from(created).unwrap_or(0),
            },
            sha,
        ))
    }

    /// All handles, newest first.
    pub fn list(&self) -> Result<Vec<HandleMeta>> {
        let conn = self.conn();
        let mut stmt = conn.prepare(
            "SELECT id, kind, command, bytes, created FROM handles ORDER BY created DESC, id",
        )?;
        let rows = stmt.query_map([], |r| {
            Ok((
                text(r, 0)?,
                text(r, 1)?,
                text(r, 2)?,
                int(r, 3)?,
                int(r, 4)?,
            ))
        })?;
        let mut out = Vec::new();
        for row in rows {
            let (id, kind, command, bytes, created) = row?;
            out.push(HandleMeta {
                handle: Handle(id),
                kind: kind.parse()?,
                command,
                bytes: u64::try_from(bytes).unwrap_or(0),
                created: u64::try_from(created).unwrap_or(0),
            });
        }
        Ok(out)
    }

    /// Total bytes of output recorded in the table.
    pub fn total_bytes(&self) -> Result<u64> {
        let total: i64 =
            self.conn()
                .query_row("SELECT COALESCE(SUM(bytes), 0) FROM handles", [], |r| {
                    r.get(0)
                })?;
        Ok(u64::try_from(total).unwrap_or(0))
    }

    /// Deletes handles older than `retention.max_age_secs` (relative to `now`, unix
    /// seconds), then the oldest until the total is within `retention.max_total_bytes`, then
    /// rows whose blob is gone and blob files without a row. Handles stored by this process
    /// are never deleted. Runs automatically on open.
    pub fn apply_retention(&self, retention: Retention, now: u64) -> Result<RetentionReport> {
        let protected = created_here().clone();
        let mut report = RetentionReport::default();
        let conn = self.conn();

        let rows: Vec<(String, String, u64, u64)> = {
            let mut stmt = conn
                .prepare("SELECT id, sha256, bytes, created FROM handles ORDER BY created, id")?;
            let mapped = stmt.query_map([], |r| {
                Ok((
                    text(r, 0)?,
                    text(r, 1)?,
                    u64::try_from(int(r, 2)?).unwrap_or(0),
                    u64::try_from(int(r, 3)?).unwrap_or(0),
                ))
            })?;
            mapped.collect::<rusqlite::Result<_>>()?
        };

        let cutoff = now.saturating_sub(retention.max_age_secs);
        let mut total: u64 = rows.iter().map(|r| r.2).sum();
        let mut doomed: Vec<&(String, String, u64, u64)> = Vec::new();
        // Oldest first: the age pass and the size pass both take from the front.
        for row in &rows {
            let (_, sha, bytes, created) = row;
            if protected.contains(sha) {
                continue;
            }
            let too_old = *created < cutoff;
            let too_big = total > retention.max_total_bytes;
            let blob_gone = !self.blob_path(sha).is_file();
            if too_old || too_big || blob_gone {
                total = total.saturating_sub(*bytes);
                doomed.push(row);
            }
        }

        let tx = conn.unchecked_transaction()?;
        for (id, sha, bytes, _) in &doomed {
            tx.execute("DELETE FROM handles WHERE id = ?1", params![id])?;
            // Several rows never share a blob (the sha names one row), but keep the file if
            // another surviving row points at it all the same.
            let shared: i64 = tx.query_row(
                "SELECT COUNT(*) FROM handles WHERE sha256 = ?1",
                params![sha],
                |r| r.get(0),
            )?;
            if shared == 0 && remove_if_exists(&self.blob_path(sha))? {
                report.freed_bytes += bytes;
            }
            report.deleted += 1;
        }
        tx.commit()?;

        // Blob files (and interrupted temp files) with no row.
        let live: HashSet<String> = {
            let mut stmt = conn.prepare("SELECT sha256 FROM handles")?;
            let mapped = stmt.query_map([], |r| text(r, 0))?;
            mapped.collect::<rusqlite::Result<_>>()?
        };
        drop(conn);
        for sub in read_dir_or_empty(&self.dir)? {
            if !sub.is_dir() {
                continue;
            }
            for file in read_dir_or_empty(&sub)? {
                let name = file
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_default();
                if live.contains(&name) || protected.contains(&name) {
                    continue;
                }
                // A temp file may belong to a put still running in another process.
                let fresh_tmp = name.starts_with(".tmp-")
                    && fs::metadata(&file)
                        .and_then(|m| m.modified())
                        .ok()
                        .and_then(|t| t.elapsed().ok())
                        .is_none_or(|age| age < Duration::from_secs(3600));
                if fresh_tmp {
                    continue;
                }
                let len = fs::metadata(&file).map_or(0, |m| m.len());
                if remove_if_exists(&file)? {
                    report.freed_bytes += len;
                }
            }
        }
        Ok(report)
    }
}

/// A text column; NULL reads as "".
fn text(r: &rusqlite::Row<'_>, i: usize) -> rusqlite::Result<String> {
    Ok(r.get::<_, Option<String>>(i)?.unwrap_or_default())
}

/// An integer column; NULL reads as 0.
fn int(r: &rusqlite::Row<'_>, i: usize) -> rusqlite::Result<i64> {
    Ok(r.get::<_, Option<i64>>(i)?.unwrap_or(0))
}

/// sha256 hex over a framing of (kind, command, bytes) that no two distinct inputs share.
fn handle_sha(kind: HandleKind, command: &str, bytes: &[u8]) -> String {
    let mut h = Sha256::new();
    h.update(kind.as_str().as_bytes());
    h.update([0u8]);
    h.update((command.len() as u64).to_le_bytes());
    h.update(command.as_bytes());
    h.update(bytes);
    h.finalize().iter().map(|b| format!("{b:02x}")).collect()
}

fn write_atomic(path: &Path, bytes: &[u8]) -> Result<()> {
    let parent = path.parent().ok_or_else(|| Error::Invalid {
        msg: format!("bad handle path {}", path.display()),
    })?;
    fs::create_dir_all(parent)?;
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let tmp = parent.join(format!(".tmp-{}-{name}", std::process::id()));
    {
        let mut f = fs::File::create(&tmp)?;
        f.write_all(bytes)?;
        f.sync_all()?;
    }
    fs::rename(&tmp, path)?;
    Ok(())
}

fn remove_if_exists(path: &Path) -> Result<bool> {
    match fs::remove_file(path) {
        Ok(()) => Ok(true),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(e) => Err(e.into()),
    }
}

fn read_dir_or_empty(dir: &Path) -> Result<Vec<PathBuf>> {
    match fs::read_dir(dir) {
        Ok(rd) => Ok(rd.filter_map(|e| e.ok().map(|e| e.path())).collect()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Vec::new()),
        Err(e) => Err(e.into()),
    }
}

fn not_found(handle: &Handle) -> Error {
    Error::NotFound {
        what: format!(
            "handle {handle} (expired or never stored; rerun the command to get a new one)"
        ),
    }
}

fn invalid_handle(s: &str) -> Error {
    Error::Invalid {
        msg: format!("invalid handle {s:?}; expected h: followed by 10 hex digits"),
    }
}

fn denied(what: &str) -> Error {
    Error::Invalid {
        msg: format!(
            "refusing to store or return output of {what:?}: it matches the secrets deny-list (.env files, private keys, keystores)"
        ),
    }
}

/// Exact file names of private keys.
const DENIED_NAMES: &[&str] = &[
    "id_rsa",
    "id_dsa",
    "id_ecdsa",
    "id_ed25519",
    "id_ecdsa_sk",
    "id_ed25519_sk",
    "secring.gpg",
    ".netrc",
];

/// Extensions (lowercase) of private keys and keystores.
const DENIED_EXTENSIONS: &[&str] = &[
    "key", "pem", "p8", "p12", "pfx", "ppk", "jks", "keystore", "bks", "jceks",
];

/// Directory names whose contents are all secret.
const DENIED_DIRS: &[&str] = &[".ssh", "private-keys-v1.d"];

/// True for files that must never be stored or returned: `.env*`, private keys (ssh
/// identities, `*.key`, `*.pem`, PKCS#8/#12, PuTTY, GnuPG secret keyrings), keystores
/// (`*.jks`, `*.keystore`, `*.bks`, `*.jceks`), anything under `.ssh/` or
/// `private-keys-v1.d/`, and `.netrc`.
pub fn is_denied_path(path: &Path) -> bool {
    let Some(name) = path.file_name().map(|n| n.to_string_lossy().to_lowercase()) else {
        return false;
    };
    if name.starts_with(".env") || DENIED_NAMES.contains(&name.as_str()) {
        return true;
    }
    if let Some(ext) = Path::new(&name).extension().map(|e| e.to_string_lossy())
        && DENIED_EXTENSIONS.contains(&ext.as_ref())
    {
        return true;
    }
    path.parent().is_some_and(|p| {
        p.components().any(|c| {
            let c = c.as_os_str().to_string_lossy();
            DENIED_DIRS.contains(&c.as_ref())
        })
    })
}

/// True when any word of `command` (quotes stripped, `KEY=` prefixes removed) names a
/// denied file.
fn command_is_denied(command: &str) -> bool {
    command.split_whitespace().any(|word| {
        let word = word.trim_matches(|c| c == '"' || c == '\'');
        let word = word.rsplit_once('=').map_or(word, |(_, v)| v);
        !word.is_empty() && !word.starts_with('-') && is_denied_path(Path::new(word))
    })
}

/// True when `bytes` contain a PEM / OpenSSH / PGP private key block header.
pub fn contains_private_key(bytes: &[u8]) -> bool {
    const BEGIN: &[u8] = b"-----BEGIN ";
    const KEY: &[u8] = b"PRIVATE KEY";
    let mut i = 0;
    while let Some(off) = find(&bytes[i..], BEGIN) {
        let start = i + off + BEGIN.len();
        let line_end = bytes[start..]
            .iter()
            .take(64)
            .position(|&b| b == b'\n')
            .map_or((start + 64).min(bytes.len()), |p| start + p);
        if find(&bytes[start..line_end], KEY).is_some() {
            return true;
        }
        i = start;
    }
    false
}

fn find(hay: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.is_empty() || hay.len() < needle.len() {
        return None;
    }
    hay.windows(needle.len()).position(|w| w == needle)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn put_at_sets_created() {
        let dir = tempfile::tempdir().unwrap();
        let conn = Connection::open_in_memory().unwrap();
        let s =
            HandleStore::from_connection(&dir.path().join("handles"), conn, Retention::default())
                .unwrap();
        let h = s.put_at(HandleKind::Text, "unit", b"x", 42).unwrap();
        assert_eq!(s.meta(&h).unwrap().created, 42);
    }
}
