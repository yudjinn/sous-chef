#!/usr/bin/env bash
# Create (or re-check) the per-release workspaces the multi-release differential runs on
# (docs/live-environment.md, "Release workspaces"; docs/differential.md, "Other releases").
#
#   scripts/release-env.sh [scarthgap|kirkstone ...]     (default: both)
#
# For each release, under $SOUS_CHEF_LIVE/releases/<release>/:
#   openembedded-core, bitbake, meta-yocto, meta-openembedded   clones at the pinned commits
#   build/                                                      conf/ written by this script
#
# The clones are made from the yocto-kiss workspace's full clones when they have the commit
# (no network); otherwise from upstream. The build directory gets a bblayers.conf and a
# local.conf (MACHINE qemux86-64, DISTRO poky) and the parse-only knobs listed below. Nothing
# is fetched or built: the only BitBake commands meant for these build directories are
# `bitbake -e`, `bitbake-getvar`, `bitbake-layers show-*` and `bitbake -p`
# (scripts/differential.sh runs them). Safe to re-run: existing clones are only checked.
#
# Environment:
#   SOUS_CHEF_LIVE   the live directory (default ~/Development/sous-chef-live)

set -euo pipefail

LIVE="${SOUS_CHEF_LIVE:-$HOME/Development/sous-chef-live}"
LIVE="$(cd "$LIVE" && pwd)"
KISS="$LIVE/yocto-kiss"
BBSETUP_LAYERS="$LIVE/bitbake-builds/kiss-wrynose-dogbonedark/layers"

# --- pins (branch heads in the local clones on 2026-11-19) ---------------------------
# BitBake branch per release: OE-core's meta/conf/sanity.conf BB_MIN_VERSION
# (kirkstone 1.53.1 -> bitbake 2.0; scarthgap 2.8.1 -> bitbake 2.8).
declare -A PIN
PIN[scarthgap/openembedded-core]="fc4316861becc9f0d41857b49f6438929ab4c291"
PIN[scarthgap/bitbake]="6b443060ee96a62efb94723c97f22b51c9b20507"
PIN[scarthgap/meta-yocto]="34e15a437a792db6778fe1bf62617b307c7f9945"
PIN[scarthgap/meta-openembedded]="2db6aece32c7f8ebf6015b468e1b4e21de072e3b"
PIN[kirkstone/openembedded-core]="9687b9409d3a1e121f0f04de856055764d0a392e"
PIN[kirkstone/bitbake]="a9f09b9e2f187f07a6f6dfd1caa3fd2f3d564a8a"
PIN[kirkstone/meta-yocto]="34e3c9a19b8b955116109a2e9528966db3fced37"
PIN[kirkstone/meta-openembedded]="ce8539c941f6fcbecaca4d16640ac105c0595589"
declare -A BRANCH
BRANCH[scarthgap/bitbake]="2.8"
BRANCH[kirkstone/bitbake]="2.0"
declare -A UPSTREAM
UPSTREAM[openembedded-core]="https://git.openembedded.org/openembedded-core"
UPSTREAM[bitbake]="https://git.openembedded.org/bitbake"
UPSTREAM[meta-yocto]="https://git.yoctoproject.org/meta-yocto"
UPSTREAM[meta-openembedded]="https://git.openembedded.org/meta-openembedded"
declare -A LOCAL
LOCAL[openembedded-core]="$KISS/openembedded-core"
LOCAL[bitbake]="$KISS/bitbake"
LOCAL[meta-yocto]="$BBSETUP_LAYERS/meta-yocto"
LOCAL[meta-openembedded]="$BBSETUP_LAYERS/meta-openembedded"

log() { printf '==> %s\n' "$*" >&2; }

RELEASES=("$@")
[[ ${#RELEASES[@]} -gt 0 ]] || RELEASES=(scarthgap kirkstone)

clone() {  # <release> <repo>
    local rel="$1" repo="$2" dst="$LIVE/releases/$1/$2"
    local pin="${PIN[$rel/$repo]}" branch="${BRANCH[$rel/$repo]:-$rel}"
    if [[ ! -d "$dst/.git" ]]; then
        local src="${LOCAL[$repo]}"
        if git -C "$src" cat-file -e "$pin^{commit}" 2>/dev/null; then
            log "$rel: cloning $repo ($branch) from the local clone $src"
            # The source's remote-tracking branch is not a local branch there: clone, then
            # fetch the one ref into the new repository.
            git init --quiet "$dst"
            git -C "$dst" fetch --quiet --no-tags "$src" "refs/remotes/origin/$branch:refs/remotes/origin/$branch"
        else
            log "$rel: cloning $repo ($branch) from ${UPSTREAM[$repo]}"
            git init --quiet "$dst"
            git -C "$dst" fetch --quiet --no-tags "${UPSTREAM[$repo]}" "refs/heads/$branch:refs/remotes/origin/$branch"
        fi
        git -C "$dst" remote add origin "${UPSTREAM[$repo]}"
    fi
    if [[ "$(git -C "$dst" rev-parse HEAD 2>/dev/null)" != "$pin" ]]; then
        git -C "$dst" -c advice.detachedHead=false checkout --quiet "$pin"
    fi
}

write_if_changed() {  # <path> <content>
    if [[ ! -f "$1" || "$(cat "$1")" != "$2" ]]; then
        log "writing $1"
        printf '%s\n' "$2" > "$1"
    fi
}

for rel in "${RELEASES[@]}"; do
    case "$rel" in scarthgap|kirkstone) ;; *) log "unknown release $rel"; exit 2 ;; esac
    R="$LIVE/releases/$rel"
    mkdir -p "$R/build/conf"
    for repo in openembedded-core bitbake meta-yocto meta-openembedded; do
        clone "$rel" "$repo"
    done
    # oe-init-build-env finds BitBake at ../bitbake (relative to openembedded-core) or in
    # $BITBAKEDIR; the clone sits next to it, so the default works.
    write_if_changed "$R/build/conf/bblayers.conf" "# Written by sous-chef scripts/release-env.sh
POKY_BBLAYERS_CONF_VERSION = \"2\"
BBPATH = \"\${TOPDIR}\"
BBFILES ?= \"\"
BBLAYERS ?= \" \\
  $R/openembedded-core/meta \\
  $R/meta-yocto/meta-poky \\
  $R/meta-yocto/meta-yocto-bsp \\
  $R/meta-openembedded/meta-oe \\
  \""
    write_if_changed "$R/build/conf/local.conf" "# Written by sous-chef scripts/release-env.sh: parse-only use (bitbake -e, bitbake-getvar).
MACHINE = \"qemux86-64\"
DISTRO = \"poky\"
CONF_VERSION = \"2\"
# Parse-only knobs (docs/differential.md, \"Other releases\"): no network, and no host
# distribution check (this host is not a tested distribution of either release).
BB_NO_NETWORK = \"1\"
SANITY_TESTED_DISTROS = \"\""
    printf '%s\n' "$rel:" \
        "  openembedded-core $(git -C "$R/openembedded-core" rev-parse HEAD)" \
        "  bitbake           $(git -C "$R/bitbake" rev-parse HEAD) ($(sed -n 's/^__version__ = "\(.*\)"/\1/p' "$R/bitbake/lib/bb/__init__.py"))" \
        "  meta-yocto        $(git -C "$R/meta-yocto" rev-parse HEAD)" \
        "  meta-openembedded $(git -C "$R/meta-openembedded" rev-parse HEAD)" \
        "  build             $R/build"
done
echo "disk: $(du -sh "$LIVE/releases" | cut -f1)"
