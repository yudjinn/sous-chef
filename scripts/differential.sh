#!/usr/bin/env bash
# T8.1: the BitBake side of the differential test.
#
# Runs parse-only BitBake commands in a build of the live environment
# (docs/live-environment.md) and stores the raw output where tests/differential.rs reads it.
#
#   SOUS_CHEF_LIVE=~/Development/sous-chef-live [SOUS_CHEF_DIFF_WS=<name|file>] scripts/differential.sh [recipe...]
#
# The workspace is described by tests/fixtures/differential/<name>.ws (default `wrynose`,
# the bitbake-setup build of yocto-kiss; `scarthgap` and `kirkstone` are the release
# workspaces of scripts/release-env.sh): the build directory, how its environment is set
# up, where the dumps go, the recipe list. The same file is read by the test.
#
# With no recipe arguments it runs the workspace-level commands (show-layers, show-appends)
# and `bitbake -e` for every recipe in the description. A dump that already exists is kept;
# delete it (or pass FORCE=1) to run it again.
#
# Only parse-only commands are run: `bitbake -e <recipe>` and `bitbake-layers show-*`.
# Nothing is fetched, compiled or built (the binding rule in docs/milestones/M9-kas-live.md,
# "Limits on live runs"). For Wrynose the bitbake-setup build is used, not the kas build:
# BitBake allows one client per build directory and the kas build is used by other work.

set -euo pipefail

: "${SOUS_CHEF_LIVE:?set SOUS_CHEF_LIVE to the live environment directory}"
HERE="$(cd "$(dirname "$0")/.." && pwd)"
WS="${SOUS_CHEF_DIFF_WS:-wrynose}"
[[ -f "$WS" ]] || WS="$HERE/tests/fixtures/differential/$WS.ws"
[[ -f "$WS" ]] || { echo "no workspace description $WS" >&2; exit 2; }

# key = value lines; `recipes` accumulates.
WS_RECIPES=()
WS_PATH=""
while IFS= read -r line; do
    [[ "$line" =~ ^[[:space:]]*(#|$) ]] && continue
    key="${line%%=*}"; key="${key//[[:space:]]/}"
    val="${line#*=}"; val="${val#"${val%%[![:space:]]*}"}"; val="${val%"${val##*[![:space:]]}"}"
    case "$key" in
        build) WS_BUILD="$SOUS_CHEF_LIVE/$val" ;;
        init) WS_INIT="$val" ;;
        dumps) WS_DUMPS="$SOUS_CHEF_LIVE/$val" ;;
        path) [[ -n "$val" ]] && WS_PATH="$SOUS_CHEF_LIVE/$val" ;;
        recipes) read -r -a words <<<"$val"; WS_RECIPES+=("${words[@]}") ;;
    esac
done <"$WS"

BUILD="${SOUS_CHEF_BUILD:-$WS_BUILD}"
OUT="$WS_DUMPS"
mkdir -p "$OUT/env"

RECIPES=("${WS_RECIPES[@]}")
if [ "$#" -gt 0 ]; then
    RECIPES=("$@")
    WORKSPACE=0
else
    WORKSPACE=1
fi

case "$WS_INIT" in
    setup) INIT_SRC="$BUILD/init-build-env"; INIT_DIR="" ;;
    oe:*) INIT_DIR="$SOUS_CHEF_LIVE/${WS_INIT#oe:}"; INIT_SRC="" ;;
    *) echo "unknown init $WS_INIT in $WS" >&2; exit 2 ;;
esac

# One BitBake command in the build environment, with a time limit (a parse should take
# seconds; ten minutes means something is wrong). TMPDIR and NETRC_FILE are dropped:
# OE-core passes TMPDIR through to BitBake in some releases' BB_ENV_PASSTHROUGH lists.
bb() {
    local out="$1"
    shift
    local start end rc
    start=$(date +%s.%N)
    set +e
    # init-build-env overwrites the positional parameters (`set --`), so keep them first.
    env -u NETRC_FILE -u TMPDIR PATH="${WS_PATH:+$WS_PATH:}$PATH" timeout 600 bash -c '
        src="$1"; dir="$2"; build="$3"; shift 3; args=("$@")
        if [ -n "$src" ]; then . "$src" >/dev/null || exit 1
        else cd "$dir" && . ./oe-init-build-env "$build" >/dev/null || exit 1; fi
        exec "${args[@]}"' \
        bash "$INIT_SRC" "$INIT_DIR" "$BUILD" "$@" >"$out" 2>"$out.err"
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
