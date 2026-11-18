//! Finding fingerprints and the lint baseline (`--write-baseline FILE`, `--baseline FILE`).
//!
//! **Fingerprint** ([`fingerprints`]): SHA-256 of the rule id, the file (`layer:path`) and the
//! text of the statement the finding's first line is in (whitespace collapsed; the line's own
//! text when no statement covers it, such as a comment or line 1 of an empty file), first 16
//! bytes in hex, then `:<n>`, the finding's position among the findings of the same hash in
//! report order (1 for the first). The line number is not part of it, so a finding whose
//! statement moves (a line inserted above) keeps its fingerprint; editing the statement
//! itself gives a new one. The same value is SARIF's `partialFingerprints`.
//!
//! **Baseline file**: pretty-printed JSON, `{"schema": "sous-chef-lint-baseline/1",
//! "findings": [{"fingerprint", "rule", "file"}]}`, sorted by file, rule and fingerprint, so
//! two runs on the same tree write the same bytes. [`compare`] matches the current findings
//! against it by fingerprint (each entry matches one finding): the findings left are new, the
//! entries left are stale.

use std::collections::{BTreeMap, HashMap};
use std::fs;
use std::path::Path;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use super::{Finding, SourceCache};
use crate::error::{Error, Result};
use crate::workspace::Workspace;

/// The `schema` field of a baseline file.
pub const SCHEMA: &str = "sous-chef-lint-baseline/1";

/// One baselined finding.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct Entry {
    /// `layer:path`.
    pub file: String,
    pub rule: String,
    pub fingerprint: String,
}

/// A baseline file.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Baseline {
    pub schema: String,
    pub findings: Vec<Entry>,
}

/// The text a finding's fingerprint is computed from: its statement, whitespace collapsed.
fn anchor_text(ws: &Workspace, sources: &mut SourceCache, f: &Finding) -> String {
    let key = (f.loc.layer.clone(), f.loc.path.clone());
    let Ok(src) = sources.load(ws, &key, None) else {
        return String::new();
    };
    let line = f.loc.lines.start;
    let text = src
        .parsed
        .statements
        .iter()
        .find(|s| s.span.start <= line && line <= s.span.end)
        .and_then(|s| src.text.get(s.bytes.clone()))
        .map(str::to_string)
        .or_else(|| {
            usize::try_from(line)
                .ok()
                .and_then(|n| n.checked_sub(1))
                .and_then(|i| src.text.lines().nth(i))
                .map(str::to_string)
        })
        .unwrap_or_default();
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn hash(rule: &str, file: &str, text: &str) -> String {
    let mut h = Sha256::new();
    h.update(rule.as_bytes());
    h.update([0]);
    h.update(file.as_bytes());
    h.update([0]);
    h.update(text.as_bytes());
    h.finalize()
        .iter()
        .take(16)
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// The fingerprint of each finding, in the same order (module docs).
pub fn fingerprints(ws: &Workspace, findings: &[Finding]) -> Vec<String> {
    let mut sources = SourceCache::default();
    let mut seen: HashMap<String, usize> = HashMap::new();
    findings
        .iter()
        .map(|f| {
            let file = format!("{}:{}", f.loc.layer, f.loc.path.display());
            let h = hash(f.rule, &file, &anchor_text(ws, &mut sources, f));
            let n = seen.entry(h.clone()).or_default();
            *n += 1;
            format!("{h}:{n}")
        })
        .collect()
}

/// The baseline of `findings` (sorted, deterministic).
pub fn build(ws: &Workspace, findings: &[Finding]) -> Baseline {
    let mut entries: Vec<Entry> = findings
        .iter()
        .zip(fingerprints(ws, findings))
        .map(|(f, fingerprint)| Entry {
            file: format!("{}:{}", f.loc.layer, f.loc.path.display()),
            rule: f.rule.to_string(),
            fingerprint,
        })
        .collect();
    entries.sort();
    Baseline {
        schema: SCHEMA.into(),
        findings: entries,
    }
}

/// The baseline as written to a file: pretty JSON and a final newline.
pub fn to_text(b: &Baseline) -> String {
    let mut s = serde_json::to_string_pretty(b).unwrap_or_else(|_| "{}".into());
    s.push('\n');
    s
}

/// Reads a baseline file. A missing, unreadable or malformed file, or another schema, is an
/// error naming the file.
pub fn read(path: &Path) -> Result<Baseline> {
    let text = fs::read_to_string(path).map_err(|e| Error::Invalid {
        msg: format!("baseline {}: {e}", path.display()),
    })?;
    let b: Baseline = serde_json::from_str(&text).map_err(|e| Error::Invalid {
        msg: format!(
            "baseline {}: not a sous-chef lint baseline ({e}); write one with --write-baseline",
            path.display()
        ),
    })?;
    if b.schema != SCHEMA {
        return Err(Error::Invalid {
            msg: format!(
                "baseline {}: schema {:?}, expected {SCHEMA:?}; write it again with --write-baseline",
                path.display(),
                b.schema
            ),
        });
    }
    Ok(b)
}

/// The result of [`compare`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Comparison {
    /// Findings not in the baseline, in report order.
    pub new: Vec<Finding>,
    /// Findings the baseline covers.
    pub matched: usize,
    /// Baseline entries no current finding matches.
    pub stale: Vec<Entry>,
}

impl Comparison {
    /// One line for the report.
    pub fn summary(&self, path: &Path) -> String {
        format!(
            "baseline {}: {} new {}, {} matched, {} stale {} (no longer found; refresh the file with --write-baseline)",
            path.display(),
            self.new.len(),
            if self.new.len() == 1 {
                "finding"
            } else {
                "findings"
            },
            self.matched,
            self.stale.len(),
            if self.stale.len() == 1 {
                "entry"
            } else {
                "entries"
            },
        )
    }
}

/// Matches `findings` against `baseline` by fingerprint.
pub fn compare(ws: &Workspace, findings: &[Finding], baseline: &Baseline) -> Comparison {
    let mut left: BTreeMap<&str, Vec<&Entry>> = BTreeMap::new();
    for e in &baseline.findings {
        left.entry(e.fingerprint.as_str()).or_default().push(e);
    }
    let mut new = Vec::new();
    let mut matched = 0;
    for (f, fp) in findings.iter().zip(fingerprints(ws, findings)) {
        match left.get_mut(fp.as_str()).and_then(|v| v.pop()) {
            Some(_) => matched += 1,
            None => new.push(f.clone()),
        }
    }
    let mut stale: Vec<Entry> = left.into_values().flatten().cloned().collect();
    stale.sort();
    Comparison {
        new,
        matched,
        stale,
    }
}
