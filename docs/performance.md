# Performance (T8.2)

Speed and size of sous-chef on a real workspace, against the targets the milestone (M8,
T8.2) and the hooks (`hook::BUDGET`) set.

## Setup

- **Workspace:** the bitbake-setup build of the live environment
  ([live-environment.md](live-environment.md)), yocto-kiss for `dogbonedark` on Wrynose:
  7 layers (OE-core, meta-yocto-bsp, meta-poky, meta-arm-toolchain, meta-arm, meta-kiss,
  meta-kiss-ti), 1784 indexed files (1035 `.bb`, 20 `.bbappend`, 383 `.inc`, 242 `.bbclass`,
  104 `.conf`). This is OE-core plus a BSP layer and two small product layers; it does not
  contain meta-openembedded (the milestone asks for poky + meta-openembedded, which is
  several times larger; see "Not measured").
- **Machine:** AMD Ryzen 9 5900HS laptop CPU (8 cores, 16 threads), 15 GiB RAM, NVMe SSD,
  btrfs, Linux 7.2 (Arch-based). The source files were in the page cache (they had just been
  parsed by BitBake), so "cold" below means a cold sous-chef index or evaluation cache, not
  a cold disk.
- **Build:** `cargo build --release` (rustc 1.102.0-nightly); the measurements run in the
  release test binary.
- **Date:** 2026-11-17, sous-chef at `67909a3` (code unchanged by this task).

## How to reproduce

```sh
SOUS_CHEF_LIVE=~/Development/sous-chef-live \
    cargo test --release --test performance -- --ignored --nocapture
```

`tests/performance.rs` is ignored by default and returns at once without `SOUS_CHEF_LIVE`.
It runs no BitBake command. Index caches go to `$SOUS_CHEF_LIVE/runs/perf/`, the report to
`$SOUS_CHEF_LIVE/runs/perf/report.txt`. For the "changed one file" refresh it appends a
comment line to the build's `conf/local.conf` and restores the exact bytes afterwards (the
test asserts this). `SOUS_CHEF_PERF_RUNS` (default 5) sets the repetitions.
`SOUS_CHEF_PERF_DIR=<dir>` puts the caches and the report there instead of `runs/perf/`;
`SOUS_CHEF_PERF_NO_EDIT=1` skips the touch / change / `refresh_file` rows, so the test
writes nothing to the build directory. Timing is plain
`std::time::Instant`: the crate has no benchmark dependency and none was added. The whole
test takes about 32 s, of which 15 s is `lint --all`.

## Results

Median and worst of the runs (each run is a separate call; "first call" is the first of a
series in a fresh process state).

| Measurement | Median | Worst | Runs | Target | Met |
|---|---|---|---|---|---|
| First index build (`Index::open_in` + `refresh`, empty cache) | 1268 ms | 1272 ms | 3 | under 30 s | yes |
| No-change refresh | 104 ms | 105 ms | 10 | under 150 ms | yes |
| Refresh after touching one file (mtime only, same bytes) | 104 ms | 110 ms | 5 | | |
| Refresh after changing one file (`conf/local.conf`, reparse + edge rebuild) | 204 ms | 204 ms | 5 | | |
| `Index::refresh_file` of that file (the edit hook's path) | 4.9 ms | 5.0 ms | 5 | | |
| Configuration pass (`evaluate`, no recipe, cold cache) | 19.7 ms | 23.4 ms | 5 | | |
| `evaluate` curl, cold (configuration pass included) | 22.4 ms | 23.9 ms | 5 | | |
| `evaluate` curl, warm (configuration cached) | 6.5 ms | 6.6 ms | 10 | | |
| `evaluate` linux-kiss / python3 / kiss-image, warm | 7.7 / 6.8 / 6.7 ms | | 1 each | | |
| `lint` layer meta-kiss (8 files, 6 findings) | 58 ms | 75 ms | 5 | | |
| `lint` layer meta-kiss-ti (3 files, 0 findings) | 84 ms | 85 ms | 5 | | |
| `lint` workspace, own layers writable (14 files incl. build conf, 6 findings) | 144 ms | 166 ms | 5 | | |
| `lint --all` (1784 files, 1505 findings) | 14.8 s | | 1 | | |
| tool `status` (first call 133 ms) | 132 ms | 133 ms | 5 | | |
| tool `layers` (first 104 ms) | 105 ms | 106 ms | 5 | | |
| tool `recipe curl` (first 129 ms) | 115 ms | 116 ms | 5 | | |
| tool `item recipe=curl name=SRC_URI` (first 105 ms) | 105 ms | 108 ms | 5 | | |
| tool `search do_install` (first 110 ms) | 109 ms | 112 ms | 5 | | |
| tool `class autotools` (first 105 ms) | 105 ms | 105 ms | 5 | | |
| tool `var SRC_URI recipe=curl` (first 135 ms) | 118 ms | 120 ms | 5 | warm under 100 ms | **no** |
| tool `overrides recipe=curl` (first 112 ms) | 111 ms | 112 ms | 5 | | |
| tool `deps curl` (first 151 ms) | 150 ms | 153 ms | 5 | | |
| tool `lint target=meta-kiss` (first 166 ms) | 163 ms | 165 ms | 5 | | |
| `sous-chef hook`, Grep (19 result lines in curl and sl), process wall time | 4.6 ms | 11.1 ms | 6 | 150 ms | yes |
| `sous-chef hook`, Read (whole `curl_8.19.0.bb`), process wall time | 3.2 ms | 3.3 ms | 6 | 150 ms | yes |
| `sous-chef hook`, Edit (`sl_git.bb`: refresh_file + file-scope lint), process wall time | 8.6 ms | 8.8 ms | 6 | 150 ms | yes |

Other numbers:

- **Index size:** 1784 files, 36 556 statements, 3203 edges; the database is 11.6 MiB
  (`index.sqlite` plus WAL). Workspace discovery takes 0.8 ms.
- **Memory:** peak RSS of the whole test process (index build, all evaluations, lint
  `--all`, the tool calls) was 79 MiB.
- **CLI:** `sous-chef index` from an empty cache took 1.39 s wall time end to end (process
  start, discovery, build, printing 1784 lines); a second run reported "reindexed 0 in
  104 ms".
- Every hook call produced output (6 of 6 for each tool); `hook.log` reports 1 to 6 ms
  inside the handler (`grep_read 2 ms`, `grep_read 1 ms`, `edit 6 ms`). The hooks never
  refresh the whole index, which is why they are far below the tools.

## After the w5-fix-eval2 refresh fix (item 10)

Measured 2026-11-17 on the same machine and workspace, the baseline (`b19a20e`, built from
`git archive` into `target/`) and branch `w5-fix-eval2` run back to back with
`SOUS_CHEF_PERF_NO_EDIT=1` (the rows that write to the build's `local.conf` skipped) and
`SOUS_CHEF_PERF_DIR` under `runs/fix-eval2/` (both switches are new in
`tests/performance.rs`). Another agent was using the machine, so absolute times of the
cold build are higher than in the table above for both builds.

| Measurement | Before (median / worst) | After (median / worst) |
|---|---|---|
| No-change refresh | 104.2 / 104.9 ms | 18.9 / 21.2 ms |
| tool `var SRC_URI recipe=curl` | 118.5 / 119.5 ms (first 137.5) | 28.6 / 29.0 ms (first 53.2) |
| First index build | 2021 / 2037 ms | 1892 / 1920 ms |
| `lint --all` | 14.9 s, 1784 files, 1523 findings | 14.8 s, 1745 files (data files left out), 1092 findings |

Every tool call drops by about 85 ms (`layers` 105 -> 21 ms, `item` 109 -> 24 ms, `search`
110 -> 25 ms); **`var` warm under 100 ms is now met**. The cause, as found by the previous
round: `Workspace::confine` (`resolve_existing`) canonicalised every prefix of the path, each
`realpath` resolving the whole prefix again (about depth² lookups per file, for 1784
files). Now `resolve_existing` tries one `realpath` of the whole path first (the component
walk only runs when part of the path does not exist, as for a file scaffold is about to
create), and the walk, which never follows symlinks, confines each canonical layer root once
and takes a regular file under it as its own resolved path; a symlink is confined on its own
as before (`tests/index_scan.rs` `symlinked_directory_leading_outside_is_not_indexed`, and
the confinement tests in `tests/workspace_layers.rs`). A refresh still stats every file, so
it still notices every changed, added and removed file. The per-directory
`conf/bblayers.conf` check in the walk was measured by limiting it to `conf` directories:
no difference (19 ms either way), so it was kept as it is.

## Status after the fix rounds (2026-11-17)

The numbers measured by the fix agents, the table above being the first measurement: a
no-change refresh **104 ms -> 19 ms**; `var SRC_URI recipe=curl` warm **118 ms -> 29 ms**
(the target, under 100 ms, is met); every lookup tool about 85 ms faster (`layers` 105 ->
21 ms, `item` 109 -> 24 ms, `search` 110 -> 25 ms); `lint --all` on the bitbake-setup build
unchanged in time (14.8 s), with 1745 files instead of 1784 (data files are no longer
linted). The kas build of the live environment, re-run for this document at `8f66f94`
without BitBake: `sous-chef lint` on the writable layers 0.14 s wall time (13 files),
`lint --all` 14.3 s (1719 files, 647 findings).

Not measured yet: meta-openembedded scale, a disk-cold run, authoritative `var`, the
`bitbake`, `log`, `slice` and `scaffold` tools, the edit hook on a new file.

## After the later rounds (w9-final, 2026-11-18)

Re-run on the same machine and workspace at the end of the code part of branch
`w9-final` (after w6 to w8 and this branch), release build, with `SOUS_CHEF_PERF_NO_EDIT=1`
(no touch / change / `refresh_file` rows) and `SOUS_CHEF_PERF_DIR` under
`runs/w9-final/`; the whole test took 24 s. Medians (worst):

| Measurement | Earlier (first table, or "Status after the fix rounds") | w9-final |
|---|---|---|
| First index build | 1268 ms | 1161 ms (1173) |
| No-change refresh | 19 ms | 19.4 ms (20.8) |
| tool `var SRC_URI recipe=curl`, warm | 29 ms | 29.1 ms (29.4; first call 56.9) |
| `evaluate` curl, cold / warm | 22.4 / 6.5 ms | 25.2 / 7.0 ms |
| tool `layers` / `search do_install` | 21 / 25 ms | 20.7 / 25.6 ms |
| `lint --all` (bitbake-setup build) | 14.8 s, 1745 files | 15.3 s, 1744 files, 625 findings |
| hook Grep / Read / Edit, process wall time | 4.6 / 3.2 / 8.6 ms | 4.6 / 3.5 / 9.2 ms |

The index holds 1784 files, 36 556 statements and 3204 edges (11.6 MiB); peak RSS 79 MiB.
Nothing moved beyond run-to-run noise. On the kas build, `sous-chef audit` with the kas
files configured took 15.4 s (lint 15.0 s of it).

## Against the targets

- **Full index under 30 s: met** (1.3 s; 1.2 s at w9-final). Not checked at meta-openembedded scale.
- **No-change refresh under 150 ms: met** (104 ms median, 110 ms worst at `67909a3`, with
  about 45 ms of headroom; 19 ms since w5-fix-eval2, 19.4 ms at w9-final). A refresh walks and stats every file (it was not profiled further), so its time
  grows with the number of files, and a workspace with meta-openembedded (several thousand more files) would likely exceed
  150 ms. The fallbacks the milestone lists (directory mtimes, at most one refresh per N
  seconds, a watcher) were **not** implemented: the target is met here, and this task may
  not change `src/`.
- **`var` warm under 100 ms: not met** at `67909a3` (118 ms). The evaluation itself is 6.5 ms warm; the
  rest is the no-change refresh every tool call does first (about 104 ms, see every tool
  row: no tool is faster than the refresh). **Met since w5-fix-eval2** (29 ms): the refresh
  itself was made cheaper (see "After the w5-fix-eval2 refresh fix"); no refresh-rate limit
  or watcher was added.
- **Hook budget 150 ms: met** for Grep, Read and Edit by a factor of 15 or more, including
  process start.
- Changing a `.conf` costs a full edge rebuild (204 ms here); changing one recipe through
  the edit hook's `refresh_file` costs 5 ms.

## Not measured

- poky + meta-openembedded (the milestone's reference workspace): not present in the live
  environment's bitbake-setup build (meta-openembedded is fetched but not in `BBLAYERS`).
  Adding it would need a change to the build's `bblayers.conf`, which was not made.
- A disk-cold run (dropping the page cache needs root).
- `var` with `authoritative: true` (runs BitBake) and the `bitbake`, `log`, `slice` and
  `scaffold` tools.
- The edit hook on a new file (which recomputes every edge, like the 204 ms refresh above).
