//! `sous-chef lint --format sarif`: SARIF 2.1.0, the subset GitHub code scanning reads.
//!
//! One run. `tool.driver`: `name` `sous-chef`, `version` and `semanticVersion` (the crate
//! version), and `rules`: every rule of the catalogue, in registry order,
//! with `id`, `name`, `shortDescription` (the first sentence of its row in
//! docs/lint-rules.md), `fullDescription` (the row, cut to GitHub's 1024 characters), `help`
//! and `defaultConfiguration.level` (its effective severity). `results`: one per finding,
//! `ruleId`, `ruleIndex`, `level` (`error`, `warning`, `note` for info), `message.text`, one
//! location whose `artifactLocation.uri` is the file relative to the repository root
//! ([`RepoPaths`]; `uriBaseId` `%SRCROOT%`) and `region` `startLine` / `endLine`, and
//! `partialFingerprints` (`primaryLocationLineHash`, the key code scanning uses, and
//! `sousChefFinding/v1`, both [`super::baseline::fingerprints`]). A file outside the
//! repository root gets an absolute `file://` URI and no `uriBaseId`. Each rule also has
//! `properties.tags` (`yocto` and its group) and `properties.problem.severity`. More than
//! [`MAX_RESULTS`] findings (GitHub rejects such a file) are cut to the most severe by the
//! caller ([`keep_most_severe`], `lint::ci`), with a note on stderr.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use serde::Serialize;

use super::{Finding, Severity, all_rules, effective_severity};
use crate::config::Config;
use crate::workspace::Workspace;

/// The SARIF version written.
pub const VERSION: &str = "2.1.0";
/// The schema URI written in `$schema`.
pub const SCHEMA_URI: &str = "https://json.schemastore.org/sarif-2.1.0.json";
/// GitHub's limit for `shortDescription` / `fullDescription`.
pub const MAX_DESCRIPTION: usize = 1024;
/// GitHub code scanning's limit of results per run: a SARIF file with more is rejected
/// (docs.github.com, "SARIF support for code scanning", "Upload limits").
pub const MAX_RESULTS: usize = 25_000;

/// The indices of the findings to write when there are more than `limit`: the most severe
/// (errors, then warnings, then info; report order within a severity), returned in report
/// order. All of them when there are at most `limit`.
pub fn keep_most_severe(severities: &[Severity], limit: usize) -> Vec<usize> {
    let mut idx: Vec<usize> = (0..severities.len()).collect();
    if idx.len() <= limit {
        return idx;
    }
    idx.sort_by_key(|&i| std::cmp::Reverse(severities[i]));
    idx.truncate(limit);
    idx.sort_unstable();
    idx
}

/// Paths relative to the repository root: the git top level of the working directory
/// (`git rev-parse --show-toplevel` through [`crate::gitcmd`]), else the working directory.
#[derive(Debug, Clone)]
pub struct RepoPaths {
    pub root: PathBuf,
}

impl RepoPaths {
    pub fn new(work_dir: &Path) -> RepoPaths {
        let root = crate::gitcmd::git_text(work_dir, &["rev-parse", "--show-toplevel"])
            .map(|t| PathBuf::from(t.trim()))
            .filter(|p| !p.as_os_str().is_empty() && p.is_dir())
            .unwrap_or_else(|| work_dir.to_path_buf());
        let root = std::fs::canonicalize(&root).unwrap_or(root);
        RepoPaths { root }
    }

    /// The finding's file relative to the root (`/`-separated), or `None` when it is outside
    /// the root (then [`RepoPaths::absolute`]).
    pub fn relative(&self, ws: &Workspace, f: &Finding) -> Option<String> {
        let abs = self.absolute(ws, f)?;
        let rel = abs.strip_prefix(&self.root).ok()?;
        Some(
            rel.components()
                .map(|c| c.as_os_str().to_string_lossy().into_owned())
                .collect::<Vec<_>>()
                .join("/"),
        )
    }

    /// The finding's file as an absolute path (symlinks resolved when it exists).
    pub fn absolute(&self, ws: &Workspace, f: &Finding) -> Option<PathBuf> {
        let abs = ws.abs_path(&f.loc)?;
        Some(std::fs::canonicalize(&abs).unwrap_or(abs))
    }
}

/// Percent-encodes a path for a URI: everything but unreserved characters and `/`.
pub fn uri_path(p: &str) -> String {
    let mut out = String::new();
    for b in p.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'.' | b'_' | b'~' | b'/') {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

#[derive(Serialize)]
struct Log {
    #[serde(rename = "$schema")]
    schema: &'static str,
    version: &'static str,
    runs: Vec<Run>,
}

#[derive(Serialize)]
struct Run {
    tool: Tool,
    results: Vec<SarifResult>,
}

#[derive(Serialize)]
struct Tool {
    driver: Driver,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct Driver {
    name: &'static str,
    version: &'static str,
    semantic_version: &'static str,
    rules: Vec<RuleDescriptor>,
}

#[derive(Serialize)]
struct Text {
    text: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct RuleDescriptor {
    id: &'static str,
    name: String,
    short_description: Text,
    full_description: Text,
    help: Text,
    default_configuration: Level,
    properties: RuleProperties,
}

/// The rule properties GitHub code scanning reads: `tags` (filters in the Security tab)
/// and `problem.severity` (`error`, `warning` or `recommendation`) for a rule that is not a
/// security rule. sous-chef has no security rule, so no `security-severity`.
#[derive(Serialize)]
struct RuleProperties {
    tags: Vec<String>,
    #[serde(rename = "problem.severity")]
    problem_severity: &'static str,
}

#[derive(Serialize)]
struct Level {
    level: &'static str,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct SarifResult {
    rule_id: &'static str,
    rule_index: usize,
    level: &'static str,
    message: Text,
    locations: Vec<Location>,
    partial_fingerprints: Fingerprints,
}

#[derive(Serialize)]
struct Fingerprints {
    #[serde(rename = "primaryLocationLineHash")]
    primary: String,
    #[serde(rename = "sousChefFinding/v1")]
    own: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct Location {
    physical_location: Physical,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct Physical {
    artifact_location: Artifact,
    region: Region,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct Artifact {
    uri: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    uri_base_id: Option<&'static str>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct Region {
    start_line: u32,
    end_line: u32,
}

/// `error`, `warning` or `note`.
pub fn level(s: Severity) -> &'static str {
    match s {
        Severity::Error => "error",
        Severity::Warning => "warning",
        Severity::Info => "note",
    }
}

/// The description of every rule from the catalogue (docs/lint-rules.md), by id.
pub fn rule_descriptions() -> HashMap<String, String> {
    const DOC: &str = include_str!("../../docs/lint-rules.md");
    let mut out = HashMap::new();
    for line in DOC.lines() {
        let Some(rest) = line.strip_prefix("| `") else {
            continue;
        };
        let cells: Vec<&str> = rest.trim_end_matches('|').split(" | ").collect();
        if let [id, _sev, _scope, flags] = cells.as_slice() {
            out.insert(
                id.trim_end_matches('`').to_string(),
                flags.trim().to_string(),
            );
        }
    }
    out
}

/// At most `n` characters, `...` at the end when cut.
fn cut(s: &str, n: usize) -> String {
    if s.chars().count() <= n {
        return s.to_string();
    }
    let mut t: String = s.chars().take(n.saturating_sub(3)).collect();
    t.push_str("...");
    t
}

/// The first sentence of a catalogue description (up to the first `. ` outside a code
/// span), at most [`MAX_DESCRIPTION`] characters.
fn first_sentence(s: &str) -> String {
    let mut in_code = false;
    let chars: Vec<char> = s.chars().collect();
    for (i, c) in chars.iter().enumerate() {
        if *c == '`' {
            in_code = !in_code;
        }
        if !in_code && *c == '.' && chars.get(i + 1).is_none_or(|n| n.is_whitespace()) {
            return cut(&chars[..=i].iter().collect::<String>(), MAX_DESCRIPTION);
        }
    }
    cut(s, MAX_DESCRIPTION)
}

/// The SARIF log of `findings` (in report order), with `fingerprints` in the same order.
pub fn render(
    ws: &Workspace,
    config: &Config,
    paths: &RepoPaths,
    findings: &[Finding],
    fingerprints: &[String],
) -> String {
    let docs = rule_descriptions();
    let rules = all_rules();
    let descriptors: Vec<RuleDescriptor> = rules
        .iter()
        .map(|r| {
            let d = docs
                .get(r.id())
                .cloned()
                .unwrap_or_else(|| r.id().to_string());
            RuleDescriptor {
                id: r.id(),
                name: r.id().replace(['/', '-'], "_"),
                short_description: Text {
                    text: first_sentence(&d),
                },
                full_description: Text {
                    text: cut(&d, MAX_DESCRIPTION),
                },
                help: Text {
                    text: format!(
                        "Rule {} of sous-chef's catalogue (docs/lint-rules.md), which says what it \
                         checks, what it leaves alone and why. Disable it with `[lint] disable` in \
                         sous-chef.toml, or on one line with `# sous-chef: allow {}` above it.",
                        r.id(),
                        r.id()
                    ),
                },
                default_configuration: Level {
                    level: level(effective_severity(config, *r)),
                },
                properties: RuleProperties {
                    tags: vec![
                        "yocto".to_string(),
                        r.id().split('/').next().unwrap_or_default().to_string(),
                    ],
                    problem_severity: match effective_severity(config, *r) {
                        Severity::Error => "error",
                        Severity::Warning => "warning",
                        Severity::Info => "recommendation",
                    },
                },
            }
        })
        .collect();
    let results = findings
        .iter()
        .zip(fingerprints)
        .map(|(f, fp)| {
            let artifact = match paths.relative(ws, f) {
                Some(rel) => Artifact {
                    uri: uri_path(&rel),
                    uri_base_id: Some("%SRCROOT%"),
                },
                None => Artifact {
                    uri: paths.absolute(ws, f).map_or_else(
                        || uri_path(&f.loc.path.to_string_lossy()),
                        |p| format!("file://{}", uri_path(&p.to_string_lossy())),
                    ),
                    uri_base_id: None,
                },
            };
            SarifResult {
                rule_id: f.rule,
                rule_index: rules.iter().position(|r| r.id() == f.rule).unwrap_or(0),
                level: level(f.severity),
                message: Text {
                    text: f.message.clone(),
                },
                locations: vec![Location {
                    physical_location: Physical {
                        artifact_location: artifact,
                        region: Region {
                            start_line: f.loc.lines.start.max(1),
                            end_line: f.loc.lines.end.max(f.loc.lines.start).max(1),
                        },
                    },
                }],
                partial_fingerprints: Fingerprints {
                    primary: fp.clone(),
                    own: fp.clone(),
                },
            }
        })
        .collect();
    let log = Log {
        schema: SCHEMA_URI,
        version: VERSION,
        runs: vec![Run {
            tool: Tool {
                driver: Driver {
                    name: "sous-chef",
                    version: env!("CARGO_PKG_VERSION"),
                    semantic_version: env!("CARGO_PKG_VERSION"),
                    rules: descriptors,
                },
            },
            results,
        }],
    };
    serde_json::to_string_pretty(&log).unwrap_or_else(|_| "{}".into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sentences_and_uris() {
        assert_eq!(
            first_sentence("Tabs in `a. b` bodies. More here."),
            "Tabs in `a. b` bodies."
        );
        assert_eq!(uri_path("meta x/a%b.bb"), "meta%20x/a%25b.bb");
        assert_eq!(level(Severity::Info), "note");
        let d = rule_descriptions();
        assert!(d.contains_key("hygiene/release-removed"));
        assert_eq!(d.len(), all_rules().len());
    }

    #[test]
    fn the_most_severe_results_are_kept() {
        use Severity::*;
        let s = [Info, Warning, Error, Info, Error, Warning];
        assert_eq!(keep_most_severe(&s, 10), vec![0, 1, 2, 3, 4, 5]);
        assert_eq!(keep_most_severe(&s, 3), vec![1, 2, 4]);
        assert_eq!(keep_most_severe(&s, 2), vec![2, 4]);
        assert!(keep_most_severe(&s, 0).is_empty());
    }
}
