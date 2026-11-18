//! Optional BitBake backend: environment runner, background jobs, `bitbake -e`, logs.
//!
//! `env` (T5.3) finds the build environment and runs allowlisted programs in it;
//! `jobs` (T5.4) runs them in the background; `envdump` (T5.5) parses `bitbake -e`;
//! `logs` (T5.6) locates and digests task logs. Read-only views of what a build left
//! behind, none of which builds anything: `cve` (OE-core's CVE metadata rules and its JSON
//! reports), `sigs` (digest of `bitbake-diffsigs`), `pkgdata` (the pkgdata directory).

pub mod cve;
pub mod env;
pub mod envdump;
pub mod jobs;
pub mod logs;
pub mod sigs;

pub use env::{
    BbEnv, CommandOutput, EnvState, Finish, HostEnv, KasRun, ProcessGroup, Supervised,
    environment_state, validate_arg, validate_name,
};
pub use jobs::{JobInfo, JobSpec, JobState, JobTable, Started};
