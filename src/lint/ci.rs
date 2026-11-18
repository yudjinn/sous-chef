//! The `sous-chef lint` options that change what the command does rather than what it
//! checks: the migration report (`--migrate-to`, [`super::migrate`]).
//!
//! [`dispatch`] is called by [`super::cli_with`] once the target and the options are known;
//! it returns `None` when none of these options is set, and the plain report is printed.

use std::io::Write;

use super::{LintCliArgs, Options, Severity, Target, migrate};
use crate::config::Config;
use crate::error::Result;
use crate::index::Index;
use crate::release::Release;
use crate::workspace::Workspace;

/// Handles `--migrate-to`; `None` when it is not set.
pub fn dispatch(
    ws: &Workspace,
    index: &Index,
    cfg: &Config,
    args: &LintCliArgs,
    target: &Target,
    options: &Options,
    out: &mut dyn Write,
) -> Result<Option<i32>> {
    let Some(to) = args.migrate_to.as_deref() else {
        return Ok(None);
    };
    let to = Release::parse_arg(to, "--migrate-to")?;
    let m = migrate::report(ws, index, cfg, target, options, to)?;
    let text = migrate::render(&m, args.format.unwrap_or_default());
    out.write_all(text.as_bytes())?;
    out.write_all(b"\n")?;
    Ok(Some(migrate::exit_code(
        &m,
        args.fail_on.unwrap_or(Severity::Error),
    )))
}
