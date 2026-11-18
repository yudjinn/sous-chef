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
//! - Release-aware (`Feature::KeptUntilStyhead`): for a recipe checked against Walnascar or
//!   later, a git URL without `branch=` / `nobranch=1` (a `tag=` included) or with several
//!   comma-separated branches is an error naming those releases; BitBake 2.12 stops on both
//!   ([`strict_git_problem`]).
//!
//! The finding is placed on the assignment that contributed the entry (the last applied
//! `SRC_URI` operation whose expanded value contains it), which may be in an `.inc` or a
//! bbappend; otherwise on line 1 of the recipe.

use crate::eval::{DataStore, OpStatus};
use crate::lint::helpers;
use crate::lint::{Finding, LintContext, Rule, Scope, Severity};
use crate::parse::words;
use crate::release::Feature;
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

/// The entry as shown in a message: the URL without its parameters, at most 80 characters.
fn short_entry(entry: &str) -> String {
    entry
        .split(';')
        .next()
        .unwrap_or(entry)
        .chars()
        .take(80)
        .collect()
}

/// A git URL problem that stops BitBake in one of the context's releases: from Walnascar
/// (BitBake 2.12, `2515fbd10` "fetch: Drop multiple branch/revision support for single git
/// urls") `fetch2/git.py` raises "The url does not set any branch parameter or set
/// nobranch=1." for a URL without `branch=` or `nobranch=1` (a `tag=` does not help), and a
/// `branch=` with several comma-separated names is no longer split (5.2 migration guide,
/// "Git fetcher: support for multiple revisions per URL removed"). Before, the first is a
/// warning and the second is accepted. `None` when no release of the context is affected.
fn strict_git_problem(
    cx: &LintContext<'_>,
    params: &[(&str, &str)],
    rev: bool,
    name: Option<&str>,
) -> Option<String> {
    let strict = helpers::releases_lacking(cx, Feature::KeptUntilStyhead);
    if strict.is_empty() {
        return None;
    }
    let from = Feature::KeptUntilStyhead
        .removed_in()
        .map_or("", |r| r.codename());
    let branch = param(params, "branch");
    // `usehead=1` implies `nobranch` (`git.py`, `urldata_init`).
    if branch.is_none()
        && param(params, "nobranch") != Some("1")
        && param(params, "usehead") != Some("1")
    {
        let rev_hint = if rev {
            String::new()
        } else {
            format!(
                " and set {} = \"<commit hash>\"",
                name.map_or("SRCREV".to_string(), |n| format!("SRCREV_{n}"))
            )
        };
        return Some(format!(
            "git URL without `branch=`: from {from} on BitBake stops on it (\"The url does not \
             set any branch parameter or set nobranch=1.\", a `tag=` is not enough), and this \
             layer is checked against {}; add `;branch=<branch>` (or `;nobranch=1` for a \
             commit on no branch){rev_hint}",
            helpers::release_list(&strict)
        ));
    }
    if branch.is_some_and(|b| b.contains(',')) {
        return Some(format!(
            "git URL with several branches in `branch=`: from {from} on one git URL fetches \
             one branch, and this layer is checked against {}; write one SRC_URI entry per \
             branch, each with its own `name=` and SRCREV_<name>",
            helpers::release_list(&strict)
        ));
    }
    None
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
            words::words(&data.expand(&op.value).text)
                .into_iter()
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
        for entry in words::words(&src_uri.text) {
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
                    if let Some(message) = strict_git_problem(cx, &params, rev, name) {
                        let loc = origin(data, entry)
                            .unwrap_or_else(|| helpers::loc_at(cx, LineSpan::line(1)));
                        out.push(Finding {
                            rule: self.id(),
                            severity: Severity::Error,
                            loc,
                            message: format!("`{}`: {message}", short_entry(entry)),
                        });
                        continue;
                    }
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
