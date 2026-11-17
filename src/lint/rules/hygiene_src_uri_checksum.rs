//! `hygiene/src-uri-checksum`: a remote download in `SRC_URI` without a checksum; a git URL
//! without `SRCREV` or without `branch=`.
//!
//! See docs/lint-rules.md. Recipe scope: `SRC_URI`, its flags and `SRCREV` are read from the
//! evaluated recipe, so entries from an `.inc`, a bbappend or a class count. Nothing is
//! reported when `SRC_URI` depends on python the evaluator cannot run, or for an entry that
//! still contains `${` after expansion.
//!
//! - `http://`, `https://`, `ftp://` entries: a checksum is a `<name>.sha256sum` flag on
//!   `SRC_URI` (`name=` parameter; `sha256sum` without one), a `sha256sum=` parameter, or one
//!   of the other checksum kinds the fetcher accepts (`md5sum`, `sha1sum`, `sha384sum`,
//!   `sha512sum`), as flag or parameter.
//! - `git://` and `gitsm://` entries: a revision is a `rev=` or `tag=` parameter, or a value
//!   for `SRCREV_<name>` (with `name=`) or `SRCREV` that is not empty and not OE-core's
//!   `INVALID` default (`SRCREV ??= "INVALID"` in `bitbake.conf`); a value from python
//!   (`${AUTOREV}`) counts as set. A branch is `branch=`, or `nobranch=1`; not checked when a
//!   `tag=` is given.
//!
//! The finding is placed on the assignment that contributed the entry (the last applied
//! `SRC_URI` operation whose expanded value contains it), which may be in an `.inc` or a
//! bbappend; otherwise on line 1 of the recipe.

use crate::eval::{DataStore, OpStatus};
use crate::lint::helpers;
use crate::lint::{Finding, LintContext, Rule, Scope, Severity};
use crate::types::{LineSpan, Loc};

pub struct HygieneSrcUriChecksum;

const CHECKSUMS: &[&str] = &["sha256sum", "md5sum", "sha1sum", "sha384sum", "sha512sum"];

/// `scheme://rest;k=v;k=v` split into the scheme and its parameters.
fn split_url(entry: &str) -> Option<(&str, Vec<(&str, &str)>)> {
    let (scheme, _) = entry.split_once("://")?;
    let params = entry
        .split(';')
        .skip(1)
        .filter_map(|p| p.split_once('='))
        .collect();
    Some((scheme, params))
}

fn param<'a>(params: &[(&'a str, &'a str)], key: &str) -> Option<&'a str> {
    params.iter().find(|(k, _)| *k == key).map(|(_, v)| *v)
}

/// Whether a revision variable has a usable value (or one from python).
fn has_rev(data: &DataStore, name: &str) -> bool {
    data.value(name).is_some_and(|v| {
        let t = v.text.trim();
        v.unevaluated_python || (!t.is_empty() && t != "INVALID")
    })
}

/// Where an entry came from: the last applied `SRC_URI` operation whose expanded value
/// contains it.
fn origin(data: &DataStore, entry: &str) -> Option<Loc> {
    data.history("SRC_URI")
        .into_iter()
        .rev()
        .filter(|(_, st)| *st == OpStatus::Applied)
        .find(|(op, _)| {
            data.expand(&op.value)
                .text
                .split_whitespace()
                .any(|w| w == entry)
        })
        .map(|(op, _)| op.loc)
}

impl Rule for HygieneSrcUriChecksum {
    fn id(&self) -> &'static str {
        "hygiene/src-uri-checksum"
    }

    fn scope(&self) -> Scope {
        Scope::Recipe
    }

    fn default_severity(&self) -> Severity {
        Severity::Warning
    }

    fn check(&self, cx: &LintContext<'_>, out: &mut Vec<Finding>) {
        let Some(data) = cx.data else { return };
        if data.replay().python_vars.contains("SRC_URI") {
            return;
        }
        let Some(src_uri) = data.value("SRC_URI") else {
            return;
        };
        if src_uri.unevaluated_python {
            return;
        }
        for entry in src_uri.text.split_whitespace() {
            if entry.contains("${") {
                continue;
            }
            let Some((scheme, params)) = split_url(entry) else {
                continue;
            };
            let name = param(&params, "name");
            let problem = match scheme {
                "http" | "https" | "ftp" => {
                    let has = CHECKSUMS.iter().any(|c| {
                        param(&params, c).is_some()
                            || data
                                .raw_flag(
                                    "SRC_URI",
                                    &name.map_or(c.to_string(), |n| format!("{n}.{c}")),
                                )
                                .is_some()
                    });
                    (!has).then(|| {
                        let flag = name.map_or("sha256sum".to_string(), |n| format!("{n}.sha256sum"));
                        format!(
                            "remote download without a checksum; add SRC_URI[{flag}] = \"<sha256 of the file>\""
                        )
                    })
                }
                "git" | "gitsm" => {
                    let rev = param(&params, "rev").is_some()
                        || param(&params, "tag").is_some()
                        || name.is_some_and(|n| has_rev(data, &format!("SRCREV_{n}")))
                        || has_rev(data, "SRCREV");
                    let branch = param(&params, "branch").is_some()
                        || param(&params, "nobranch") == Some("1")
                        || param(&params, "tag").is_some();
                    match (rev, branch) {
                        (true, true) => None,
                        (false, true) => Some(format!(
                            "git URL without a revision; set {} = \"<commit hash>\"",
                            name.map_or("SRCREV".to_string(), |n| format!("SRCREV_{n}"))
                        )),
                        (true, false) => Some(
                            "git URL without `branch=`; add `;branch=<branch>` (or `;nobranch=1` \
                             for a commit on no branch)"
                                .to_string(),
                        ),
                        (false, false) => Some(format!(
                            "git URL without a revision or `branch=`; add `;branch=<branch>` and \
                             set {} = \"<commit hash>\"",
                            name.map_or("SRCREV".to_string(), |n| format!("SRCREV_{n}"))
                        )),
                    }
                }
                _ => None,
            };
            let Some(message) = problem else { continue };
            let loc = origin(data, entry).unwrap_or_else(|| helpers::loc_at(cx, LineSpan::line(1)));
            let short: String = entry
                .split(';')
                .next()
                .unwrap_or(entry)
                .chars()
                .take(80)
                .collect();
            out.push(Finding {
                rule: self.id(),
                severity: self.default_severity(),
                loc,
                message: format!("`{short}`: {message}"),
            });
        }
    }
}
