//! `hygiene/insane-skip`: `INSANE_SKIP` of a QA check whose failure usually means the
//! recipe builds wrongly, not that the check does not apply (info).
//!
//! See docs/lint-rules.md. OE-core's `insane.bbclass` (Wrynose) has these checks in
//! `ERROR_QA`; skipping one hides the problem it reports instead of fixing it:
//!
//! - `ldflags`: "File ... doesn't have GNU_HASH (didn't pass LDFLAGS?)"
//!   (`package_qa_hash_style`): the binaries were linked without the distro's `LDFLAGS`, so
//!   also without the hardening and `--hash-style` flags they carry. Usually a Makefile that
//!   ignores `LDFLAGS`; pass them (`EXTRA_OEMAKE`, a patch).
//! - `textrel`: text relocations in a shared object (code not built position-independent).
//! - `buildpaths`: build host paths (`TMPDIR`) in the output; the build is not reproducible.
//!
//! Every other check is not reported: skipping `dev-so`, `arch`, `libdir`, `dev-deps`,
//! `already-stripped`, `staticdev`, `file-rdeps`, `32bit-time` and the rest is routine in
//! OE-core itself (prebuilt or firmware files, plugins, odd layouts), where the check does
//! not apply. Recipes only (`.bb`, `.bbappend`, `.inc`); a class (`go.bbclass` skips
//! `ldflags` for the Go linker) sets it for every user on purpose. Not reported in a file
//! that inherits `bin_package` (prebuilt binaries cannot be relinked), and not for a
//! `:remove`.

use crate::lint::helpers;
use crate::lint::{Finding, LintContext, Rule, Scope, Severity};
use crate::parse::{StatementKind, words};
use crate::types::{FileKind, LineSpan};

pub struct HygieneInsaneSkip;

/// The checks reported, with what skipping each hides and what to do instead.
const HIDING: &[(&str, &str)] = &[
    (
        "ldflags",
        "the binaries are linked without the distro's LDFLAGS (the check found no GNU_HASH); \
         pass LDFLAGS to the build (EXTRA_OEMAKE, or a patch to the Makefile) and drop the skip",
    ),
    (
        "textrel",
        "a shared object has text relocations (code not built position-independent); build \
         it with -fPIC and drop the skip",
    ),
    (
        "buildpaths",
        "the output holds build host paths, so the build is not reproducible; remove them \
         at their source and drop the skip",
    ),
];

impl Rule for HygieneInsaneSkip {
    fn id(&self) -> &'static str {
        "hygiene/insane-skip"
    }

    fn scope(&self) -> Scope {
        Scope::File
    }

    fn default_severity(&self) -> Severity {
        Severity::Info
    }

    fn check(&self, cx: &LintContext<'_>, out: &mut Vec<Finding>) {
        let Some(file) = cx.file else { return };
        if !matches!(
            cx.kind,
            Some(FileKind::Recipe | FileKind::Append | FileKind::Include)
        ) {
            return;
        }
        let prebuilt = file.statements.iter().any(|s| {
            matches!(&s.kind, StatementKind::Inherit { classes, .. }
                if words::words(classes).contains(&"bin_package"))
        });
        if prebuilt {
            return;
        }
        for s in &file.statements {
            let StatementKind::Assign { var, value, .. } = &s.kind else {
                continue;
            };
            if var.base != "INSANE_SKIP"
                || var.flag.is_some()
                || var.overrides.iter().any(|o| o == "remove")
            {
                continue;
            }
            for word in words::words(value) {
                let Some((check, hides)) = HIDING.iter().find(|(c, _)| *c == word) else {
                    continue;
                };
                out.push(helpers::finding(
                    self,
                    cx,
                    LineSpan::line(s.span.start),
                    format!(
                        "`{}` skips the `{check}` QA check, which usually hides a build problem: {hides} (keep it for prebuilt or firmware files)",
                        helpers::colon_name(var)
                    ),
                ));
            }
        }
    }
}
