# Releasing

How to cut a sous-chef release: what the maintainer runs, what
[`.github/workflows/release.yml`](../.github/workflows/release.yml) does, and how to check
the result. Written for v0.1.0, the first release; the same steps hold for later ones with
the version changed.

## What a release is

- A tag `v<version>` on `main`, where `<version>` is the `version` in `Cargo.toml`.
- A GitHub release of that tag with four tarballs, `sous-chef-v<version>-<target>.tar.gz`
  (the binary, `LICENSE`, `README.md`) for `x86_64-unknown-linux-gnu`,
  `aarch64-unknown-linux-gnu`, `aarch64-apple-darwin` and `x86_64-apple-darwin`, a
  `SHA256SUMS` file, and the version's `CHANGELOG.md` section as its notes.
- The plugin at that tag: users add the marketplace as `yudjinn/sous-chef#v<version>`
  (README, "Install"), so the plugin a user installs is the one in the tagged tree. Nothing
  is published anywhere else (no crates.io, no plugin directory).

The version is written in these places, and the tests fail when they disagree:

| Where | Checked by |
|---|---|
| `Cargo.toml` `version` (and `Cargo.lock`, which `cargo` updates) | the source of truth |
| `sous-chef --version`, the MCP server info, SARIF `tool.driver.version`, the audit report | `env!("CARGO_PKG_VERSION")`; `tests/release_metadata.rs` checks `--version` |
| `plugin/.claude-plugin/plugin.json` `version` | `tests/plugin_packaging.rs` |
| `CHANGELOG.md`: a `## [<version>] - <date>` section with a body, and its link reference | `tests/release_metadata.rs` |
| The tag | `release.yml`, step "Tag matches Cargo.toml" |
| README "Install" examples (`tag=v0.1.0`, `#v0.1.0`, `sous-chef 0.1.0`) | not checked: update by hand |

## Before tagging

1. Everything to be released is merged into `dev` and CI is green on it (Linux and macOS
   jobs of `ci.yml`).
2. `CHANGELOG.md`: the `## [<version>] - <date>` section says what the release contains,
   for users, with today's date; `[Unreleased]` above it is empty or holds only what is
   not in this release. The link references at the bottom name the new tag.
3. Optional dry run of the release builds, from `dev`:

   ```sh
   gh workflow run release.yml --ref dev
   gh run watch "$(gh run list --workflow release.yml --branch dev --limit 1 --json databaseId --jq '.[0].databaseId')"
   ```

   It builds and smoke-tests the four tarballs (named after the branch, e.g.
   `sous-chef-dev-x86_64-unknown-linux-gnu.tar.gz`), checks the checksums and extracts the
   notes into the run artifact `release-dry-run` (`SHA256SUMS`, `NOTES.md`). It creates no
   release.

## Cut the release

`main` and `dev` point at the same commit today, and `main` has no branch protection. Bring
`main` up to `dev` (a fast-forward; it refuses if `main` has commits `dev` lacks), tag that
commit and push the tag:

```sh
git fetch origin
git switch main
git merge --ff-only origin/dev
git push origin main

git tag -a v0.1.0 -m "sous-chef 0.1.0"
git push origin v0.1.0
```

(Instead of the merge, a pull request from `dev` into `main` merged with a merge commit
works as well; then tag the merge commit after `git pull`.) The tag must be pushed after
the commit it names is on GitHub, and it must be annotated (`-a`) so it carries a date and
a tagger.

## What the workflow does

The tag push starts `release.yml` (the workflow file as it is at the tagged commit):

1. **build**, one job per target (Ubuntu 22.04 x86_64 and arm64 runners, one macOS 15
   arm64 runner for both macOS targets):
   - checks that the tag is `v` + the `Cargo.toml` version, and stops otherwise;
   - `cargo build --release --locked --target <target>` with the pinned toolchain;
   - packs `sous-chef`, `LICENSE` and `README.md` into
     `sous-chef-v<version>-<target>.tar.gz` (one top-level directory of the same name)
     and writes its `.sha256`;
   - unpacks the tarball and runs `sous-chef --version`, which must print
     `sous-chef <version>`. The x86_64 macOS binary is run under Rosetta 2 when the runner
     has it; otherwise only its architecture is checked (`file`);
   - uploads the tarball and its checksum as a run artifact.
2. **release**, once all four builds passed:
   - joins the checksums into `SHA256SUMS` and verifies them (`sha256sum -c`), and that
     there are four;
   - takes the body of the version's `CHANGELOG.md` section as the notes;
   - `gh release create v<version> --verify-tag --draft --title "sous-chef v<version>"
     --notes-file NOTES.md <tarballs> SHA256SUMS`. `--verify-tag` aborts if the tag is not
     on GitHub (instead of creating one from the default branch); `--draft` leaves the
     release unpublished, so nobody sees it, and no "latest release" link points at it,
     until it is checked.

If a build fails, nothing is released: fix the cause on `dev`, delete the tag
(`git push origin :refs/tags/v0.1.0` and `git tag -d v0.1.0`; only while the release is
still a draft or absent) and tag again. To rerun a job that failed for an unrelated reason
(a runner problem), use "Re-run failed jobs" on the run.

## Publish the draft

```sh
gh release view v0.1.0                 # notes, assets (4 tarballs + SHA256SUMS), "draft"
gh release download v0.1.0 --dir /tmp/sous-chef-v0.1.0
(cd /tmp/sous-chef-v0.1.0 && sha256sum -c SHA256SUMS)
gh release edit v0.1.0 --draft=false   # publish; or "Publish release" on the release page
```

Publishing makes it the latest release (GitHub picks the newest by date and version).

## Verify the install

On a machine (or a fresh container) without a source checkout, follow README "Install"
literally:

```sh
tag=v0.1.0 target=x86_64-unknown-linux-gnu
base=https://github.com/yudjinn/sous-chef/releases/download/$tag
curl -fLO "$base/sous-chef-$tag-$target.tar.gz" -fLO "$base/SHA256SUMS"
sha256sum --ignore-missing -c SHA256SUMS
tar -xzf "sous-chef-$tag-$target.tar.gz"
install "sous-chef-$tag-$target/sous-chef" ~/.local/bin/
sous-chef --version                                   # sous-chef 0.1.0

claude plugin marketplace add yudjinn/sous-chef#v0.1.0
claude plugin install sous-chef@sous-chef
claude plugin list                                    # sous-chef@sous-chef, Version: 0.1.0
```

Then, in a Yocto project directory: `claude mcp list` shows `plugin:sous-chef:sous-chef`
connected, and in a session `/mcp` lists its tools. The source install as well, once:

```sh
rev=$(git ls-remote https://github.com/yudjinn/sous-chef 'refs/tags/v0.1.0^{}' | cut -f1)
cargo install --locked --git https://github.com/yudjinn/sous-chef --rev "$rev" sous-chef
```

## The next version

On `dev`, in one commit:

1. `Cargo.toml` `version` (patch for fixes, minor for anything else while the version is
   0.x), then `cargo check` so `Cargo.lock` follows.
2. `plugin/.claude-plugin/plugin.json` `version`, the same string. Claude Code keeps users
   on the plugin version they installed until this string changes.
3. `CHANGELOG.md`: move what is under `[Unreleased]` into a new
   `## [<version>] - <date>` section (Added / Changed / Fixed / Removed), and add its link
   reference (`[<version>]: https://github.com/yudjinn/sous-chef/releases/tag/v<version>`)
   and update `[Unreleased]` to compare from the new tag.
4. README "Install": the `tag=`, `#v...` and `sous-chef <version>` examples.

`cargo test` checks 1 to 3. Then follow "Cut the release" with the new tag. Between
releases, add user-facing changes under `[Unreleased]` as they are merged.
