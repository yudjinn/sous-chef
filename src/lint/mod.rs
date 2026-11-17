//! Lint engine, `Rule` trait and registry. Rules live in `rules/`, one file each.
//!
//! Stub (M0): T4.1 implements `LintContext` construction, `run`, configuration, suppression
//! and the CLI.

pub mod helpers;
pub mod rules;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::config::Config;
use crate::error::{Error, Result};
use crate::eval::DataStore;
use crate::index::Index;
use crate::parse::ParsedFile;
use crate::render::Format;
use crate::types::{FileKind, Loc};
use crate::workspace::Workspace;

/// What the engine hands a rule.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
pub enum Scope {
    /// One parsed file.
    File,
    /// The evaluated recipe tree.
    Recipe,
    /// Index queries over a layer.
    Layer,
}

/// Ordered: `Info < Warning < Error`.
#[derive(
    Debug,
    Clone,
    Copy,
    PartialEq,
    Eq,
    PartialOrd,
    Ord,
    Hash,
    Serialize,
    Deserialize,
    JsonSchema,
    clap::ValueEnum,
)]
#[serde(rename_all = "lowercase")]
pub enum Severity {
    Info,
    Warning,
    Error,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Finding {
    pub rule: &'static str,
    pub severity: Severity,
    pub loc: Loc,
    /// One line: what is wrong, then what to write instead.
    pub message: String,
}

/// Input to `Rule::check`. Which fields are set depends on the rule's scope:
///
/// - `File`: `file`, `source`, `kind`, and `loc` covering the whole file.
/// - `Recipe`: as `File` for the recipe's `.bb`, plus `data`.
/// - `Layer`: `layer`; `loc` points at the layer's `conf/layer.conf`.
///
/// `index` and `ws` are always set.
pub struct LintContext<'a> {
    pub file: Option<&'a ParsedFile>,
    /// The file's text, for rules that look at raw lines (tabs, trailing whitespace).
    pub source: Option<&'a str>,
    pub kind: Option<FileKind>,
    pub data: Option<&'a DataStore>,
    /// Layer name, for layer scope.
    pub layer: Option<&'a str>,
    pub index: &'a Index,
    pub ws: &'a Workspace,
    pub loc: Loc,
}

pub trait Rule: Sync {
    /// The rule id, e.g. `"override/legacy-syntax"`.
    fn id(&self) -> &'static str;
    fn scope(&self) -> Scope;
    fn default_severity(&self) -> Severity;
    fn check(&self, cx: &LintContext<'_>, out: &mut Vec<Finding>);
}

/// Every rule in the catalogue.
pub fn all_rules() -> &'static [&'static dyn Rule] {
    rules::ALL
}

/// `sous-chef lint` arguments. Flattened into the CLI by `main.rs`; T4.1 may add flags here
/// without touching `main.rs`.
#[derive(Debug, Clone, Default, PartialEq, Eq, clap::Args)]
pub struct LintCliArgs {
    /// A recipe, a file pointer, a layer name or a path; default: the whole workspace.
    pub target: Option<String>,
    /// Rule ids or groups to run (repeatable).
    #[arg(long = "rule", value_name = "ID")]
    pub rules: Vec<String>,
    /// Minimum severity to report.
    #[arg(long, value_enum)]
    pub severity: Option<Severity>,
    /// Output format.
    #[arg(long, value_enum)]
    pub format: Option<Format>,
    /// Exit with status 1 if any finding is at or above this severity (default: error).
    #[arg(long, value_enum)]
    pub fail_on: Option<Severity>,
    /// Also lint layers that are not writable.
    #[arg(long)]
    pub all: bool,
}

/// Runs `sous-chef lint`, printing findings to stdout. Returns the process exit code.
// Stub (M0): T4.1 implements it.
pub fn cli(_cfg: &Config, _args: &LintCliArgs) -> Result<i32> {
    Err(Error::not_implemented())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rule_ids_are_unique_and_grouped() {
        let mut ids: Vec<_> = all_rules().iter().map(|r| r.id()).collect();
        let n = ids.len();
        ids.sort_unstable();
        ids.dedup();
        assert_eq!(ids.len(), n, "duplicate rule id");
        for id in ids {
            let group = id.split('/').next().unwrap_or("");
            assert!(
                ["override", "org", "hygiene"].contains(&group),
                "{id} has unknown group"
            );
        }
    }
}
