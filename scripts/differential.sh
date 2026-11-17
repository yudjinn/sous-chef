#!/usr/bin/env bash
# T8.1: the BitBake side of the differential test.
#
# Runs parse-only BitBake commands in the bitbake-setup build of the live environment
# (docs/live-environment.md) and stores the raw output under
# $SOUS_CHEF_LIVE/runs/differential/, where tests/differential.rs reads it.
#
#   SOUS_CHEF_LIVE=~/Development/sous-chef-live scripts/differential.sh [recipe...]
#
# With no recipe arguments it runs the workspace-level commands (show-layers, show-appends)
# and `bitbake -e` for every recipe in RECIPES below. A dump that already exists is kept;
# delete it (or pass FORCE=1) to run it again.
#
# Only parse-only commands are run: `bitbake -e <recipe>` and `bitbake-layers show-*`.
# Nothing is fetched, compiled or built (the binding rule in docs/milestones/M9-kas-live.md,
# "Limits on live runs"). The bitbake-setup build is used, not the kas build: BitBake allows
# one client per build directory and the kas build is used by other work.

set -euo pipefail

: "${SOUS_CHEF_LIVE:?set SOUS_CHEF_LIVE to the live environment directory}"
BUILD="${SOUS_CHEF_BUILD:-$SOUS_CHEF_LIVE/bitbake-builds/kiss-wrynose-dogbonedark/build}"
OUT="$SOUS_CHEF_LIVE/runs/differential"
mkdir -p "$OUT/env"

# Keep this list in sync with RECIPES in tests/differential.rs.
RECIPES=(
    curl sl linux-kiss u-boot-kiss kiss-image packagegroup-core-boot
    zlib openssl busybox dropbear ncurses dbus systemd-boot util-linux
    gmp-native nativesdk-zlib quilt-native expat glib-2.0 base-files
    python3 bash opencsd virtual/kernel virtual/bootloader
)
if [ "$#" -gt 0 ]; then
    RECIPES=("$@")
    WORKSPACE=0
else
    WORKSPACE=1
fi

# One BitBake command in the build environment, with a time limit (a parse should take
# seconds; ten minutes means something is wrong).
bb() {
    local out="$1"
    shift
    local start end rc
    start=$(date +%s.%N)
    set +e
    # init-build-env overwrites the positional parameters (`set --`), so keep them first.
    env -u NETRC_FILE timeout 600 bash -c \
        'b="$1"; shift; args=("$@"); . "$b/init-build-env" >/dev/null && exec "${args[@]}"' \
        bash "$BUILD" "$@" >"$out" 2>"$out.err"
    rc=$?
    set -e
    end=$(date +%s.%N)
    printf '%s\t%s\t%.1f s\texit %d\n' "$(date -Iseconds)" "$*" "$(echo "$end - $start" | bc)" "$rc" \
        | tee -a "$OUT/commands.log"
}

if [ "$WORKSPACE" = 1 ]; then
    for cmd in show-layers show-appends; do
        if [ -n "${FORCE:-}" ] || [ ! -s "$OUT/$cmd.out" ]; then
            bb "$OUT/$cmd.out" bitbake-layers "$cmd"
        fi
    done
fi

for r in "${RECIPES[@]}"; do
    f="$OUT/env/${r//\//_}.env"
    if [ -n "${FORCE:-}" ] || [ ! -s "$f" ]; then
        bb "$f" bitbake -e "$r"
    fi
done
