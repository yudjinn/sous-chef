//! `hygiene/git-pv`: a `<pn>_git.bb` recipe that sets no `PV`, so `PV` is `git`.
//!
//! See docs/lint-rules.md. OE-core's `bitbake.conf` sets
//! `PV = "${@bb.parse.vars_from_file(d.getVar('FILE', False),d)[1] or '1.0'}"`, and BitBake's
//! `vars_from_file` (`lib/bb/parse/__init__.py`) splits the file name without its extension
//! at `_`: `u-boot-kiss_git.bb` gives `PV = "git"` (BitBake's `bitbake -e` of yocto-kiss's
//! `u-boot-kiss` says `PV="git"`). The version then holds no version number, and since it
//! has no `+`, `package.bbclass` (`package_convert_pr_autoinc`: `if srcpv and "+" in pkgv`)
//! appends no source revision to `PKGV`, so packages built from another `SRCREV` keep the
//! same version `git` and a package manager does not see them as an upgrade.
//!
//! Recipe scope: reported when the recipe file's version part is exactly `git` and no
//! recipe, `.inc`, bbappend or class operation on `PV` contributes (only the `bitbake.conf`
//! default does), the evaluated value is `git`, and the evaluated `SRC_URI` has a `git://` or
//! `gitsm://` entry (OE-core's `clang-cross_git.bb` fetches nothing). Not reported when
//! python sets `PV`. Info, not a warning: OE-core does this itself (`sato-icon-theme_git.bb`,
//! `vulkan-samples_git.bb`, `rpm-sequoia-crypto-policy_git.bb` on wrynose set no `PV`).

use crate::eval::OpStatus;
use crate::lint::helpers;
use crate::lint::{Finding, LintContext, Rule, Scope, Severity};
use crate::release::Feature;
use crate::types::{FileKind, LineSpan};

pub struct HygieneGitPv;

impl Rule for HygieneGitPv {
    fn id(&self) -> &'static str {
        "hygiene/git-pv"
    }

    fn scope(&self) -> Scope {
        Scope::Recipe
    }

    fn default_severity(&self) -> Severity {
        Severity::Info
    }

    fn check(&self, cx: &LintContext<'_>, out: &mut Vec<Finding>) {
        let Some(data) = cx.data else { return };
        let Some(stem) = cx.loc.path.file_stem().and_then(|s| s.to_str()) else {
            return;
        };
        if FileKind::from_path(&cx.loc.path) != Some(FileKind::Recipe) {
            return;
        }
        let parts: Vec<&str> = stem.split('_').collect();
        if parts.len() != 2 || parts[1] != "git" {
            return;
        }
        if data.replay().python_vars.contains("PV") {
            return;
        }
        let set_by_metadata = data.history("PV").iter().any(|(op, status)| {
            *status != OpStatus::InactiveOverride
                && FileKind::from_path(&op.loc.path) != Some(FileKind::Conf)
        });
        if set_by_metadata {
            return;
        }
        if data.value("PV").is_none_or(|v| v.text != "git") {
            return;
        }
        // Only a recipe that fetches from git has a revision to lose (OE-core's
        // `clang-cross_git.bb` fetches nothing).
        let fetches_git = data.value("SRC_URI").is_some_and(|v| {
            crate::parse::words::words(&v.text)
                .iter()
                .any(|w| w.starts_with("git://") || w.starts_with("gitsm://"))
        });
        if !fetches_git {
            return;
        }
        // Up to Mickledore `package.bbclass` adds no source revision to PKGV at all
        // (Kirkstone's `package_convert_pr_autoinc` only replaces AUTOINC): `${SRCPV}` in PV
        // is what carries it ([`Feature::KeptUntilMickledore`]).
        let srcpv = helpers::releases_having(cx, Feature::KeptUntilMickledore);
        let message = if srcpv.is_empty() {
            format!(
                "`{stem}.bb` sets no PV, so BitBake takes it from the file name: PV is `git`, \
                 with no version number and no `+`, so no source revision is added to PKGV and \
                 a new SRCREV gives packages of the same version; set `PV = \"<version>+git\"`"
            )
        } else {
            format!(
                "`{stem}.bb` sets no PV, so BitBake takes it from the file name: PV is `git`, \
                 with no version number and no source revision (on {} only `${{SRCPV}}` in PV \
                 puts the revision in the package version), so a new SRCREV gives packages of \
                 the same version; set `PV = \"<version>+git${{SRCPV}}\"`",
                helpers::release_list(&srcpv)
            )
        };
        out.push(helpers::finding(self, cx, LineSpan::line(1), message));
    }
}
